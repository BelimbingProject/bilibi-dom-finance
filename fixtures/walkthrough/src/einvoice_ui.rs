//! NE5's running proof uses the public facade, page and download contracts.
//! Only the clearance authority is a stand-in. No product seeds or unit tests.
use crate::clearance::{self, draft, save, StandIn, CLIENT_ID, CLIENT_SECRET};
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error};
use bilibi_finance_einvoice::{
    ConsolidationRequest, DocumentQuery, Einvoice, EinvoiceError, CANCEL, SUBMIT, VIEW,
};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{Config, Executor};
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use bilibi_ui::{download::*, page::*};
use serde_json::json;
use std::{collections::HashMap, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;
async fn event(state: &mut State, scope: &Scope, value: serde_json::Value) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&value),
        })
        .await?)
}
async fn mount(services: &Services, scope: &Scope, id: Option<i64>) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/einvoice.pages")?;
    let path = id
        .map(|id| HashMap::from([("id".into(), id.to_string())]))
        .unwrap_or_default();
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: if id.is_some() { "detail" } else { "index" },
            scope: Some(scope),
            path: &path,
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    Ok(state)
}
fn has(body: &LiveBody, text: &str) {
    assert!(body.html.contains(text), "missing {text}: {}", body.html);
}

pub async fn run(db: &Db, services: &Services) -> Outcome<tokio::sync::watch::Sender<bool>> {
    let manager = crate::sign_in(db, services, 1, 1).await?;
    let admin = crate::sign_in(db, services, 1, 4).await?;
    let other = crate::sign_in(db, services, 2, 5).await?;
    let facade = services.get::<Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let target = Target::Company(OrganisationId::new(1)?);
    let url = clearance::serve(Arc::new(StandIn::default())).await?;
    assert_eq!(
        save(
            db,
            services,
            &admin,
            Target::Global,
            "finance.einvoice.sandbox.api_url",
            json!(url)
        )
        .await?,
        "stored"
    );
    let root = format!("/tmp/ne5-pdf-{}", std::process::id());
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    crate::attachments::configure_storage(db, services, &admin, &root).await?;
    for (key, value) in [
        ("environment", "sandbox"),
        ("document_version", "1.0"),
        ("sandbox.client_id", CLIENT_ID),
        ("sandbox.client_secret", CLIENT_SECRET),
        ("supplier.tax_id", "C2584563222"),
        ("supplier.registration_number", "201901234567"),
        ("supplier.industry_code", "47111"),
        ("supplier.business_activity", "Retail"),
        ("supplier.phone", "+60123456789"),
        ("supplier.address_line1", "2 Jalan Contoh"),
        ("supplier.city", "Kuala Lumpur"),
        ("supplier.state_code", "14"),
        ("supplier.country_code", "MYS"),
    ] {
        assert_eq!(
            save(
                db,
                services,
                &manager,
                target,
                &format!("finance.einvoice.{key}"),
                json!(value)
            )
            .await?,
            "stored",
            "{key}"
        );
    }
    let mut ids = Vec::new();
    for n in 0..30 {
        let mut d = draft(&format!("NE5-{n:03}"), None);
        if n == 7 {
            d.code_number = Some("NE5-100%_\\ <invoice>".into());
            d.buyer.as_mut().unwrap().name = Some("Buyer <one> & partners".into());
        }
        ids.push(facade.issue_document(&manager, 1, &d).await?.document.id);
    }
    let mut d = draft("NE5-SELF", None);
    d.document_type = Some("11".into());
    let mut supplier = d.buyer.take().unwrap();
    supplier.name = Some("Distinct Supplier".into());
    supplier.industry_code = Some("47111".into());
    supplier.business_activity = Some("Retail".into());
    d.supplier = Some(supplier);
    let self_billed = facade.issue_document(&manager, 1, &d).await?.document.id;
    let page = facade
        .list_documents(&manager, 1, &DocumentQuery::default())
        .await?;
    assert_eq!(
        (page.total_entries, page.entries.len(), page.total_pages),
        (31, 25, 2)
    );
    assert_eq!(page.entries[0].id, self_billed);
    let last = facade
        .list_documents(
            &manager,
            1,
            &DocumentQuery {
                page: 999,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!((last.page, last.entries.len()), (2, 6));
    for search in ["%", "_", "\\", "BUYER <ONE>"] {
        let p = facade
            .list_documents(
                &manager,
                1,
                &DocumentQuery {
                    search: search.into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(p.total_entries, 1, "literal search {search}");
        assert_eq!(p.entries[0].id, ids[7]);
    }
    let p = facade
        .list_documents(
            &manager,
            1,
            &DocumentQuery {
                search: "distinct supplier".into(),
                document_type: "11".into(),
                environment: "sandbox".into(),
                status: "issued".into(),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(p.entries[0].id, self_billed);
    let empty = facade
        .list_documents(
            &manager,
            1,
            &DocumentQuery {
                environment: "production".into(),
                page: 99,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        (empty.page, empty.total_pages, empty.total_entries),
        (1, 0, 0)
    );
    assert!(matches!(
        facade
            .list_documents(&other, 1, &DocumentQuery::default())
            .await,
        Err(EinvoiceError::CompanyNotFound | EinvoiceError::Unauthorized)
    ));
    assert!(facade
        .list_documents(
            &manager,
            1,
            &DocumentQuery {
                page_size: 24,
                ..Default::default()
            }
        )
        .await
        .is_err());
    println!(
        "NE5 list: 31 documents; pages 25/6; clamp and empty; literal % _ backslash; buyer and self-billed supplier; combined filters; tenant isolation; invalid size refused"
    );
    let executor = Executor::from_services(
        services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(120),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    tokio::spawn(executor.run(shutdown));
    let mut detail = mount(services, &manager, Some(ids[0])).await?;
    has(
        &event(&mut detail, &manager, json!({"type":"submit"})).await?,
        "Submission queued.",
    );
    let client = db.get().await?;
    clearance::jobs_settle(&client).await?;
    let refreshed = event(&mut detail, &manager, json!({"type":"refresh"})).await?;
    has(&refreshed, "Status read from the authority: valid.");
    has(&refreshed, "Sandbox");
    has(&refreshed, "einvoice-document-qr");
    has(&refreshed, "Needs attention");
    let pdf = event(&mut detail, &manager, json!({"type":"generate_pdf"})).await?;
    has(&pdf, "PDF generated.");
    let d = facade.get_document(&manager, 1, ids[0]).await?;
    assert_eq!(d.status, "valid");
    assert_eq!(
        facade
            .list_documents(
                &manager,
                1,
                &DocumentQuery {
                    status: "valid".into(),
                    ..Default::default()
                }
            )
            .await?
            .total_entries,
        1,
        "PDF is not a status transition"
    );
    let artifact = d.pdf_artifact_id.unwrap();
    let downloads = services.get::<DownloadService<Scope, Error>>("finance/einvoice.files")?;
    let path = HashMap::from([
        ("id".into(), ids[0].to_string()),
        ("artifact_id".into(), artifact),
    ]);
    let file = downloads
        .0
        .serve(DownloadRequest {
            operation: "pdf",
            scope: &manager,
            path: &path,
            query: &HashMap::new(),
        })
        .await?;
    assert!(file.bytes.starts_with(b"%PDF-"));
    has(
        &event(&mut detail, &manager, json!({"type":"open_cancel"})).await?,
        "einvoice-document-cancel-modal",
    );
    let reason = "Customer changed the order – 核对";
    has(
        &event(
            &mut detail,
            &manager,
            json!({"type":"review_cancel", "cancel[reason]":reason}),
        )
        .await?,
        "einvoice-document-cancel-confirmation",
    );
    grants.set_direct(&admin, 1, CANCEL, Some(false)).await?;
    has(
        &event(&mut detail, &manager, json!({"type":"cancel"})).await?,
        "You do not have permission to cancel documents.",
    );
    assert_eq!(
        facade.get_document(&manager, 1, ids[0]).await?.status,
        "valid"
    );
    grants.set_direct(&admin, 1, CANCEL, None).await?;
    event(&mut detail, &manager, json!({"type":"open_cancel"})).await?;
    event(
        &mut detail,
        &manager,
        json!({"type":"review_cancel", "cancel[reason]":reason}),
    )
    .await?;
    has(
        &event(&mut detail, &manager, json!({"type":"cancel"})).await?,
        "The authority cancelled the document.",
    );
    assert!(facade
        .get_document(&manager, 1, ids[0])
        .await?
        .events
        .iter()
        .any(|e| e.event == "cancelled" && e.reason.as_deref() == Some(reason)));
    grants.set_direct(&admin, 1, VIEW, Some(false)).await?;
    assert!(matches!(
        downloads
            .0
            .serve(DownloadRequest {
                operation: "pdf",
                scope: &manager,
                path: &path,
                query: &HashMap::new()
            })
            .await,
        Err(Error::Forbidden)
    ));
    grants.set_direct(&admin, 1, VIEW, None).await?;
    grants.set_direct(&admin, 1, SUBMIT, Some(false)).await?;
    has(
        &event(&mut detail, &manager, json!({"type":"submit"})).await?,
        "You do not have permission to submit documents.",
    );
    grants.set_direct(&admin, 1, SUBMIT, None).await?;
    println!(
        "NE5 actions: real queue submit/poll/refresh; PDF bytes; latest status ignores PDF; pending cancel revoked and refused, Unicode reason accepted; revoked PDF download and forged submit refused"
    );
    // Leave a validated document and an issued document for the browser run.
    facade.submit_document(&manager, 1, ids[1]).await?;
    clearance::jobs_settle(&client).await?;
    // Cover two older sales with a consolidated document, without an extra UI.
    let mut covered = Vec::new();
    for n in 0..2 {
        let mut d = draft(&format!("NE5-SEPT-{n}"), None);
        d.issued_at = Some("2026-09-15T03:00:00Z".parse()?);
        covered.push(facade.issue_document(&manager, 1, &d).await?.document.id);
    }
    let consolidated = facade
        .issue_consolidated_document(
            &manager,
            1,
            &ConsolidationRequest {
                month: "2026-09-01".parse()?,
                code_number: Some("NE5-CONSOLIDATED".into()),
                document_ids: Some(covered.clone()),
            },
            "2026-10-03T03:00:00Z".parse()?,
        )
        .await?;
    assert_eq!(consolidated.sources.len(), 2);
    println!(
        "NE5 covered sources {covered:?}; consolidated document {}",
        consolidated.document.id
    );
    println!(
        "NE5 browser records: validated={}, issued={}, escaped={}, self-billed={self_billed}",
        ids[1], ids[2], ids[7]
    );
    Ok(stop)
}
