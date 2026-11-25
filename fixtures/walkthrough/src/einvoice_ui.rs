//! NE5's running proof uses the public facade, page and download contracts.
//! Only the clearance authority is a stand-in. No product seeds or unit tests.
mod linked_print;

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
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Output, Stdio},
    sync::Arc,
    time::Duration,
};

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

fn pdf_command(program: &str, args: &[&str], input: &[u8]) -> Outcome<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("missing command input")?
        .write_all(input)?;
    Ok(child.wait_with_output()?)
}

fn pdf_text(bytes: &[u8]) -> Outcome<String> {
    let output = pdf_command("pdftotext", &["-", "-"], bytes)?;
    assert!(
        output.status.success(),
        "pdftotext: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn check_pdf(bytes: &[u8], status: &str, validation: Option<&str>) -> Outcome<()> {
    assert!(bytes.starts_with(b"%PDF-"));
    let text = pdf_text(bytes)?;
    assert!(
        text.contains(status),
        "missing fiscal status {status}: {text}"
    );
    let mut urls = Vec::new();
    for page in 1..=text.split_terminator('\x0c').count() {
        let page = page.to_string();
        let raster = pdf_command(
            "pdftoppm",
            &[
                "-f",
                &page,
                "-l",
                &page,
                "-r",
                "150",
                "-png",
                "-singlefile",
                "-",
            ],
            bytes,
        )?;
        assert!(
            raster.status.success(),
            "pdftoppm: {}",
            String::from_utf8_lossy(&raster.stderr)
        );
        let scan = pdf_command(
            "zbarimg",
            &[
                "--quiet",
                "--raw",
                "--nodbus",
                "-Sdisable",
                "-Sqrcode.enable",
                "-",
            ],
            &raster.stdout,
        )?;
        match scan.status.code() {
            Some(0) => urls.extend(String::from_utf8(scan.stdout)?.lines().map(str::to_owned)),
            Some(4) => assert!(scan.stdout.is_empty()),
            _ => return Err(format!("zbarimg: {}", String::from_utf8_lossy(&scan.stderr)).into()),
        }
    }
    assert_eq!(urls, validation.into_iter().collect::<Vec<_>>());
    Ok(())
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
    save(
        db,
        services,
        &admin,
        Target::Global,
        "pdf.chromium.binary",
        json!(std::env::var("WALKTHROUGH_CHROMIUM").unwrap_or_else(|_| "chromium".into())),
    )
    .await?;
    save(
        db,
        services,
        &admin,
        Target::Global,
        "finance.einvoice.sandbox.portal_url",
        json!("https://example.test"),
    )
    .await?;
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
        if n == 0 {
            let example = d.lines.first().ok_or("missing draft line")?.clone();
            d.lines = (1..=85).map(|number| {
                let mut line = example.clone();
                line.description = Some(format!("Fiscal line {number:03} with a longer recorded description for pagination."));
                line
            }).collect();
        }
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
    clearance::jobs_settle(&**db.get().await?).await?;
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
    let validation_url = "https://example.test/DOCUMENT-0001/share/LONGID-0001-WALKTHROUGH";
    assert_eq!(d.validation_url.as_deref(), Some(validation_url));
    let artifact = d.pdf_artifact_id.ok_or("no rendered e-invoice artifact")?;
    assert_eq!(
        facade.generate_pdf(&manager, 1, ids[0]).await?.artifact_id,
        artifact
    );
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
    check_pdf(&file.bytes, "Validated", Some(validation_url))?;
    publish_malay_letter(db, services, &admin).await?;
    let malay = facade.generate_pdf(&manager, 1, ids[0]).await?;
    assert_ne!(malay.artifact_id, path["artifact_id"]);
    let translated = facade.read_pdf(&manager, 1, &malay.artifact_id).await?;
    check_pdf(&translated.bytes, "Disahkan", Some(validation_url))?;
    assert!(downloads
        .0
        .serve(DownloadRequest {
            operation: "pdf",
            scope: &other,
            path: &path,
            query: &HashMap::new(),
        })
        .await
        .is_err());
    assert_eq!(
        save(
            db,
            services,
            &admin,
            Target::Global,
            "finance.einvoice.sandbox.portal_url",
            json!("https://changed.example.test"),
        )
        .await?,
        "stored"
    );
    let updated = facade.generate_pdf(&manager, 1, ids[0]).await?;
    assert_ne!(updated.artifact_id, malay.artifact_id);
    let file = facade.read_pdf(&manager, 1, &updated.artifact_id).await?;
    check_pdf(
        &file.bytes,
        "Disahkan",
        Some("https://changed.example.test/DOCUMENT-0001/share/LONGID-0001-WALKTHROUGH"),
    )?;
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
    grants
        .set_direct(&admin, 1, CANCEL, Some(bilibi_authz::grants::Direct::Deny))
        .await?;
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
    grants
        .set_direct(&admin, 1, VIEW, Some(bilibi_authz::grants::Direct::Deny))
        .await?;
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
    let cancelled = facade.generate_pdf(&manager, 1, ids[0]).await?;
    assert_ne!(cancelled.artifact_id, updated.artifact_id);
    let file = facade.read_pdf(&manager, 1, &cancelled.artifact_id).await?;
    check_pdf(&file.bytes, "Dibatalkan", None)?;
    let invalid = facade
        .issue_document(&manager, 1, &draft("NE5-INVALID", None))
        .await?
        .document
        .id;
    facade.submit_document(&manager, 1, invalid).await?;
    clearance::jobs_settle(&**db.get().await?).await?;
    assert_eq!(
        facade.get_document(&manager, 1, invalid).await?.status,
        "invalid"
    );
    let invalid = facade.generate_pdf(&manager, 1, invalid).await?;
    let file = facade.read_pdf(&manager, 1, &invalid.artifact_id).await?;
    check_pdf(&file.bytes, "Tidak sah", None)?;
    grants
        .set_direct(&admin, 1, SUBMIT, Some(bilibi_authz::grants::Direct::Deny))
        .await?;
    has(
        &event(&mut detail, &manager, json!({"type":"submit"})).await?,
        "You do not have permission to submit documents.",
    );
    grants.set_direct(&admin, 1, SUBMIT, None).await?;
    println!(
        "NE5 actions: real queue submit/poll/refresh; PDF bytes; latest status ignores PDF; pending cancel revoked and refused, Unicode reason accepted; revoked PDF download and forged submit refused"
    );
    linked_print::prove(services, &manager, &admin).await?;
    // Leave a validated document and an issued document for the browser run.
    facade.submit_document(&manager, 1, ids[1]).await?;
    clearance::jobs_settle(&**db.get().await?).await?;
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
    let pdf = facade
        .generate_pdf(&manager, 1, consolidated.document.id)
        .await?;
    let file = facade.read_pdf(&manager, 1, &pdf.artifact_id).await?;
    let text = pdf_text(&file.bytes)?;
    assert!(
        text.contains("Status dokumen") && text.contains("September 2026"),
        "{text}"
    );
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

async fn publish_malay_letter(db: &Db, services: &Services, admin: &Scope) -> Outcome<()> {
    use bilibi_agent_api::{Handler, Registry};
    use bilibi_database::MutationBoundary;
    use bilibi_settings::Settings;
    let key = "finance.einvoice.fiscal_document";
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let current = registry
        .read(
            db,
            admin,
            "base.templates.print.get",
            &json!({"key":key,"company_id":1,"language":"ms"}),
        )
        .await?;
    let print = &current["result"]["print"];
    let mut layout: Value = serde_json::from_str(
        print["effective"]["layout"]
            .as_str()
            .ok_or("missing fiscal layout")?,
    )?;
    layout["paper"] = json!("letter");
    let input = json!({
        "key":key, "company_id":1, "language":"ms", "review":print["review"],
        "layout":layout.to_string()
    });
    let operation = "base.templates.print.publish";
    let Handler::SafeWrite(publish) = registry.operations[operation].definition.handler else {
        return Err("expected a reviewed fiscal layout write".into());
    };
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(&mut client, admin.mutation_context(operation))
        .await?;
    publish.preview(tx.sql(), admin, &input).await?;
    publish.commit(&mut tx, admin, &input).await?;
    settings
        .put(
            &mut tx,
            admin,
            Target::Company(OrganisationId::new(1)?),
            "templates.language",
            json!("ms"),
        )
        .await?;
    tx.commit().await?;
    Ok(())
}
