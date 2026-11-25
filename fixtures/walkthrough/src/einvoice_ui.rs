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

fn printed_field<'a>(text: &'a str, label: &str) -> &'a str {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    assert!(lines.any(|line| line == label), "missing {label}: {text}");
    lines.next().unwrap_or_default()
}

fn check_pdf(bytes: &[u8], label: &str, status: &str, validation: Option<&str>) -> Outcome<()> {
    assert!(bytes.starts_with(b"%PDF-"));
    let text = pdf_text(bytes)?;
    assert_eq!(printed_field(&text, label), status);
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

fn check_long_pdf(bytes: &[u8], malay: bool, expected: (f64, f64)) -> Outcome<()> {
    let (headings, tax_label, total_label) = if malay {
        (
            ["Klasifikasi", "Perihal", "Kuantiti"],
            "Jumlah cukai",
            "Jumlah termasuk cukai",
        )
    } else {
        (
            ["Classification", "Description", "Quantity"],
            "Total tax",
            "Total including tax",
        )
    };
    let text = pdf_text(bytes)?;
    let pages: Vec<_> = text.split_terminator('\x0c').collect();
    assert!(pages.len() > 1, "expected a multipage fiscal PDF");
    let last_lines = pages
        .iter()
        .skip(1)
        .find(|page| page.contains("Fiscal line 085"))
        .expect("the final seeded line must be on a continuation page");
    for heading in headings {
        assert!(
            last_lines.split_whitespace().any(|word| word == heading),
            "missing repeated heading {heading}"
        );
    }
    for tax_fact in ["01", "6%"] {
        assert!(
            last_lines.split_whitespace().any(|word| word == tax_fact),
            "missing line tax fact {tax_fact}"
        );
    }
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains("2 Jalan Contoh") && words.contains("Buyer Two Sdn Bhd"),
        "{text}"
    );
    assert!(!words.contains("99 Jalan Baru"), "{text}");
    for (label, amount) in [(tax_label, 408.0), (total_label, 7208.0)] {
        assert_eq!(
            printed_field(&text, label)
                .strip_prefix("MYR ")
                .and_then(|value| value.parse::<f64>().ok()),
            Some(amount),
            "85 lines at MYR 80 plus 6% tax should print {label} as MYR {amount}"
        );
    }
    let info = pdf_command(
        "pdfinfo",
        &["-f", "1", "-l", &pages.len().to_string(), "-"],
        bytes,
    )?;
    assert!(
        info.status.success(),
        "pdfinfo: {}",
        String::from_utf8_lossy(&info.stderr)
    );
    let output = String::from_utf8(info.stdout)?;
    let sizes: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let (label, dimensions) = line.split_once("size:")?;
            if !label.starts_with("Page ") {
                return None;
            }
            let mut words = dimensions.split_whitespace();
            let width: f64 = words.next()?.parse().ok()?;
            if words.next()? != "x" {
                return None;
            }
            let height: f64 = words.next()?.parse().ok()?;
            Some((width, height))
        })
        .collect();
    assert_eq!(sizes.len(), pages.len(), "{output}");
    for (width, height) in sizes {
        assert!(
            (width - expected.0).abs() < 2.0 && (height - expected.1).abs() < 2.0,
            "expected {expected:?} pt, got {width} x {height} pt"
        );
    }
    Ok(())
}

pub async fn run(db: &Db, services: &Services) -> Outcome<tokio::sync::watch::Sender<bool>> {
    let manager = crate::sign_in(db, services, 1, 1).await?;
    let admin = crate::sign_in(db, services, 1, 4).await?;
    let other = crate::sign_in(db, services, 2, 5).await?;
    let facade = services.get::<Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let target = Target::Company(OrganisationId::new(1)?);
    let authority = Arc::new(StandIn::default());
    let url = clearance::serve(authority.clone()).await?;
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
    assert_eq!(
        save(
            db,
            services,
            &manager,
            target,
            "finance.einvoice.supplier.address_line1",
            json!("99 Jalan Baru"),
        )
        .await?,
        "stored"
    );
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
    check_pdf(
        &file.bytes,
        "Document status",
        "Validated",
        Some(validation_url),
    )?;
    check_long_pdf(&file.bytes, false, (595.28, 841.89))?;
    publish_malay_letter(db, services, &admin).await?;
    let malay = facade.generate_pdf(&manager, 1, ids[0]).await?;
    assert_ne!(malay.artifact_id, path["artifact_id"]);
    let translated = facade.read_pdf(&manager, 1, &malay.artifact_id).await?;
    check_pdf(
        &translated.bytes,
        "Status dokumen",
        "Disahkan",
        Some(validation_url),
    )?;
    check_long_pdf(&translated.bytes, true, (612.0, 792.0))?;
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
        "Status dokumen",
        "Disahkan",
        Some("https://changed.example.test/DOCUMENT-0001/share/LONGID-0001-WALKTHROUGH"),
    )?;
    assert_eq!(
        save(
            db,
            services,
            &admin,
            Target::Global,
            "finance.einvoice.sandbox.portal_url",
            json!("https://example.test"),
        )
        .await?,
        "stored"
    );
    let restored = event(&mut detail, &manager, json!({"type":"generate_pdf"})).await?;
    has(&restored, "PDF generated.");
    has(&restored, &format!("/pdf/{}", malay.artifact_id));
    assert!(!restored
        .html
        .contains(&format!("/pdf/{}", updated.artifact_id)));
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
    let historical = downloads
        .0
        .serve(DownloadRequest {
            operation: "pdf",
            scope: &manager,
            path: &path,
            query: &HashMap::new(),
        })
        .await?;
    check_pdf(&historical.bytes, "Status dokumen", "Dibatalkan", None)?;
    let (first, second) = tokio::try_join!(
        facade.generate_pdf(&manager, 1, ids[0]),
        facade.generate_pdf(&manager, 1, ids[0]),
    )?;
    assert_eq!(first, second);
    let cancelled = first;
    let current = facade.get_document(&manager, 1, ids[0]).await?;
    assert_eq!(
        current
            .events
            .iter()
            .rev()
            .take_while(|event| event.event == "pdf_generated")
            .count(),
        1
    );
    assert_ne!(cancelled.artifact_id, updated.artifact_id);
    let file = facade.read_pdf(&manager, 1, &cancelled.artifact_id).await?;
    check_pdf(&file.bytes, "Status dokumen", "Dibatalkan", None)?;
    let saved_cancelled = downloads
        .0
        .serve(DownloadRequest {
            operation: "pdf",
            scope: &manager,
            path: &path,
            query: &HashMap::new(),
        })
        .await?;
    assert_eq!(saved_cancelled.bytes, file.bytes);
    let code = "NE5-RECHECK";
    let invalid = facade
        .issue_document(&manager, 1, &draft(code, None))
        .await?
        .document
        .id;
    facade.submit_document(&manager, 1, invalid).await?;
    clearance::jobs_settle(&**db.get().await?).await?;
    let valid = facade.get_document(&manager, 1, invalid).await?;
    assert_eq!(valid.status, "valid");
    let valid_pdf = facade.generate_pdf(&manager, 1, invalid).await?;
    let file = facade.read_pdf(&manager, 1, &valid_pdf.artifact_id).await?;
    check_pdf(
        &file.bytes,
        "Status dokumen",
        "Disahkan",
        Some("https://example.test/DOCUMENT-0002/share/LONGID-0002-WALKTHROUGH"),
    )?;
    authority.invalidate(code);
    facade.refresh_status(&manager, 1, invalid).await?;
    let saved_path = HashMap::from([
        ("id".into(), invalid.to_string()),
        ("artifact_id".into(), valid_pdf.artifact_id),
    ]);
    let saved = downloads
        .0
        .serve(DownloadRequest {
            operation: "pdf",
            scope: &manager,
            path: &saved_path,
            query: &HashMap::new(),
        })
        .await?;
    check_pdf(&saved.bytes, "Status dokumen", "Tidak sah", None)?;
    assert_eq!(
        facade.get_document(&manager, 1, invalid).await?.status,
        "invalid"
    );
    let invalid_pdf = facade.generate_pdf(&manager, 1, invalid).await?;
    let file = facade
        .read_pdf(&manager, 1, &invalid_pdf.artifact_id)
        .await?;
    check_pdf(&file.bytes, "Status dokumen", "Tidak sah", None)?;

    facade.submit_document(&manager, 1, invalid).await?;
    let pending = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let view = facade.get_document(&manager, 1, invalid).await?;
            if view.status == "submitted" {
                break Ok::<_, EinvoiceError>(view);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let retry_uuid = "DOCUMENT-0003";
    assert_eq!(pending.document_uuid.as_deref(), Some(retry_uuid));
    assert!(
        pending.validated_at.is_none()
            && pending.long_id.is_none()
            && pending.validation_url.is_none()
    );
    let file = facade
        .read_pdf(&manager, 1, &invalid_pdf.artifact_id)
        .await?;
    check_pdf(&file.bytes, "Status dokumen", "Dihantar", None)?;
    let text = pdf_text(&file.bytes)?;
    assert_eq!(printed_field(&text, "UUID pihak berkuasa"), retry_uuid);
    assert!(!text.contains("Masa disahkan"), "{text}");

    authority.finish_retry(retry_uuid);
    clearance::jobs_settle(&**db.get().await?).await?;
    let file = facade
        .read_pdf(&manager, 1, &invalid_pdf.artifact_id)
        .await?;
    check_pdf(
        &file.bytes,
        "Status dokumen",
        "Disahkan",
        Some("https://example.test/DOCUMENT-0003/share/LONGID-0003-WALKTHROUGH"),
    )?;
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
        printed_field(&text, "Status dokumen") == "Dikeluarkan, menunggu penghantaran"
            && text.contains("1 Sep 2026")
            && text.contains("30 Sep 2026")
            && !text.contains("September 2026"),
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
