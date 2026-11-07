//! NP4 running-app proof on a disposable database, never a product seed.
//! `cargo run -p bilibi-finance-walkthrough-fixture --bin bills-ui` creates
//! bills through Payables and serves the real app. `hold <order-line-id>`
//! adds a bill against a real event-fed Supply Chain line when mounted;
//! `post <bill-id>` records another match for the two-session stale review.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_attachments::{Attachments, NewFile};
use bilibi_finance_ledger::Money;
use bilibi_finance_payables::{
    DocumentLineInput, DocumentPageQuery, NewDocument, Payables, Settings, SettingsInput,
    DOCUMENT_SOURCE,
};
use bilibi_finance_walkthrough_fixture::{
    attachments::configure_storage, install, sign_in, COMPANY,
};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
use jiff::civil::date;
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:4316".into());
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    let installed = install(&db, services).await?;
    // Only identity fixtures use SQL; all Finance records below use facades.
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Alex Admin','human','admin@example.test'),(5,1,'person_5','Lee Viewer','human','viewer@example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','admin@example.test'),(4,1,5,1,'Reader','viewer@example.test') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))"
    ).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants
        .set_direct(&admin, 2, "finance.payables.view", Some(true))
        .await?;
    grants
        .set_system(
            &admin,
            "domain-events",
            "events.delivery.deliver",
            Some(true),
        )
        .await?;
    let p = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let units = installed.book.minor_units;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    p.put_settings(
        &manager,
        COMPANY,
        &SettingsInput {
            settings: Settings {
                payable_account_id: account("2000")?,
                quantity_tolerance: None,
                price_tolerance: None,
            },
            reason: None,
        },
    )
    .await?;
    let vendor = bilibi_finance_walkthrough_fixture::vendors::vendor(
        &db,
        services,
        &manager,
        COMPANY,
        "np4-party",
        "North Paper",
    )
    .await?;
    let draft = |number: &str, amount: &str, order: Option<i64>| -> Outcome<NewDocument> {
        Ok(NewDocument {
            vendor_id: Some(vendor),
            party_document_number: Some(number.into()),
            document_date: date(2026, 10, 17),
            due_date: Some(date(2026, 11, 16)),
            description: Some("Source documents and independent review".into()),
            lines: vec![DocumentLineInput {
                description: "Paper supplies".into(),
                account_id: account("5000")?,
                amount: Money::parse(amount, units)?,
                tax_code_id: None,
                external_order_line_id: order.is_none().then(|| "PO-REF-17".into()),
                order_line_id: order,
                quantity: order.map(|_| rust_decimal::Decimal::from(9)),
            }],
            tax_lines: None,
        })
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hold") => {
            let line = args.get(1).ok_or(Error::Invalid)?.parse()?;
            let d = p
                .create_bill(
                    &manager,
                    COMPANY,
                    &draft(&format!("NP4-MATCH-{line}"), "99", Some(line))?,
                )
                .await?;
            p.approve_document(&admin, COMPANY, d.id).await?;
            let held = p.post_document(&manager, COMPANY, d.id).await?;
            println!(
                "order-linked bill {}: {} {:?}",
                held.id, held.status, held.match_status
            );
            return Ok(());
        }
        Some("post") => {
            let id = args.get(1).ok_or(Error::Invalid)?.parse()?;
            let d = p.post_document(&manager, COMPANY, id).await?;
            println!(
                "bill {}: {} {:?}; last match {:?}",
                d.id,
                d.status,
                d.match_status,
                d.match_events.last()
            );
            return Ok(());
        }
        Some("deny") | Some("restore") => {
            let key = args.get(1).ok_or(Error::Invalid)?;
            grants
                .set_direct(&admin, 1, key, (args[0] == "deny").then_some(false))
                .await?;
            println!("{} {key}", args[0]);
            return Ok(());
        }
        None => (),
        _ => return Err(Error::Invalid.into()),
    }
    let root = std::env::temp_dir().join("bilibi-np4-source-documents");
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    configure_storage(&db, services, &admin, root.to_str().ok_or(Error::Invalid)?).await?;
    let existing = p
        .page_documents(&manager, COMPANY, &DocumentPageQuery::default())
        .await?;
    if existing.total == 0 {
        let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
        for n in 1..=30 {
            let number = if n == 1 {
                "NP4-MiXeD%_\\-17".into()
            } else {
                format!("NP4-{n:03}")
            };
            let d = p
                .create_bill(&manager, COMPANY, &draft(&number, "72.50", None)?)
                .await?;
            if n == 1 {
                attachments
                    .attach(
                        &manager,
                        COMPANY,
                        DOCUMENT_SOURCE,
                        d.id,
                        &NewFile {
                            file_name: "party-invoice.txt".into(),
                            content: b"Source invoice NP4: supplies 72.50".to_vec(),
                            content_type: Some("text/plain".into()),
                        },
                    )
                    .await?;
            }
            if n % 3 == 0 {
                p.approve_document(&admin, COMPANY, d.id).await?;
            }
            if n % 6 == 0 {
                p.post_document(&manager, COMPANY, d.id).await?;
            } else if n % 7 == 0 {
                p.void_document(&admin, COMPANY, d.id, "Duplicate party document")
                    .await?;
            }
            println!("bill {} ({number})", d.id);
        }
        let bill = p
            .page_documents(
                &manager,
                COMPANY,
                &DocumentPageQuery {
                    status: Some("posted".into()),
                    ..DocumentPageQuery::default()
                },
            )
            .await?
            .documents
            .remove(0);
        let note = p
            .create_debit_note(
                &manager,
                COMPANY,
                bill.id,
                &draft("NP4-NOTE", "12.50", None)?,
            )
            .await?;
        println!("debit note {} corrects {}", note.id, bill.id);
    }

    // Regression evidence uses the public facade, with independently chosen
    // party number / wildcard inputs that the old internal-number search misses.
    for search in ["mixed", "%_\\", "NP4-MiXeD%_\\-17"] {
        let page = p
            .page_documents(
                &manager,
                COMPANY,
                &DocumentPageQuery {
                    search: search.into(),
                    ..DocumentPageQuery::default()
                },
            )
            .await?;
        assert_eq!(
            page.total, 1,
            "literal, case-insensitive party number search"
        );
        assert_eq!(
            page.documents[0].party_document_number.as_deref(),
            Some("NP4-MiXeD%_\\-17")
        );
        println!("literal party-number search {search:?}: exactly its one document");
    }
    let page = p
        .page_documents(
            &manager,
            COMPANY,
            &DocumentPageQuery {
                search: "NP4-0".into(),
                page: i64::MAX,
                ..DocumentPageQuery::default()
            },
        )
        .await?;
    assert_eq!(page.page, 2);
    assert_eq!(page.documents.len(), 4);
    assert_eq!(
        page.documents
            .last()
            .unwrap()
            .party_document_number
            .as_deref(),
        Some("NP4-002")
    );
    let page = p
        .page_documents(
            &manager,
            COMPANY,
            &DocumentPageQuery {
                search: "z".repeat(255),
                page: 999,
                ..DocumentPageQuery::default()
            },
        )
        .await?;
    assert_eq!((page.page, page.total, page.documents.len()), (1, 0, 0));
    println!(
        "out-of-range pages: last nonempty page 2 (4 rows, ending NP4-002); empty page 1; 255-character search accepted"
    );

    // Run actual mounted page states against the real database before serving.
    let pages = services.get::<PageService<Scope, Error>>("finance/payables.pages")?;
    let path = HashMap::from([("id".into(), "1".into())]);
    let query = HashMap::new();
    let viewer = sign_in(&db, services, 1, 2).await?;
    let request = PageRequest {
        operation: "bill",
        scope: Some(&viewer),
        path: &path,
        query: &query,
        body: &[],
        framed: false,
    };
    let html = pages.0.render(request).await?.html;
    assert!(html.contains("payables-bill-lines-table"));
    assert!(!html.contains("id=\"payables-bill-approve\""));
    let request = PageRequest {
        operation: "bill",
        scope: Some(&viewer),
        path: &path,
        query: &query,
        body: &[],
        framed: false,
    };
    let mut state = pages.0.mount(request).await?.ok_or(Error::Internal)?;
    let before = p.get_document(&manager, COMPANY, 1).await?.status;
    state
        .update(LiveRequest {
            scope: &viewer,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    let event = serde_json::json!({"type":"approve"});
    let body = state
        .update(LiveRequest {
            scope: &viewer,
            credential: "",
            input: LiveInput::Event(&event),
        })
        .await?;
    assert!(body
        .html
        .contains("You do not have permission to approve documents."));
    let body = state
        .update(LiveRequest {
            scope: &viewer,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    assert!(body
        .html
        .contains("You do not have permission to approve documents."));
    let expired_success = serde_json::json!({"type":"clear-flash", "key":"success"});
    let body = state
        .update(LiveRequest {
            scope: &viewer,
            credential: "",
            input: LiveInput::Event(&expired_success),
        })
        .await?;
    assert!(body
        .html
        .contains("You do not have permission to approve documents."));
    let dismiss = serde_json::json!({"type":"clear-flash", "key":"error"});
    let body = state
        .update(LiveRequest {
            scope: &viewer,
            credential: "",
            input: LiveInput::Event(&dismiss),
        })
        .await?;
    assert!(!body
        .html
        .contains("You do not have permission to approve documents."));
    assert_eq!(p.get_document(&manager, COMPANY, 1).await?.status, before);
    println!("read-only page: controls absent; forged approval refused; bill unchanged");
    println!(
        "refusal survives refresh and an old success dismissal; matching error dismissal succeeds"
    );
    // NP5: the pages that record, pay, age and set up mount and render for the
    // people who may use them, and keep their controls from a reader. Writes
    // need a browser session, so they are tried in the browser.
    for (operation, marker) in [
        ("bills", "id=\"payables-bills-new\""),
        ("payments", "payables-payments-table"),
        ("payment_new", "id=\"payables-pay-form\""),
        ("aged", "payables-aged-parties"),
        ("setup", "id=\"payables-tax-add\""),
    ] {
        assert!(
            page_html(&pages, operation, &manager, &path, &query)
                .await?
                .contains(marker),
            "{operation} renders {marker} for the recorder"
        );
    }
    for (operation, marker) in [
        ("bills", "id=\"payables-bills-new\""),
        ("setup", "id=\"payables-tax-add\""),
        ("payments", "id=\"payables-payments-new\""),
    ] {
        assert!(
            !page_html(&pages, operation, &viewer, &path, &query)
                .await?
                .contains(marker),
            "{operation} hides {marker} from a reader"
        );
    }
    println!(
        "recording pages: the recorder gets the bill, payment and setup forms; a reader gets none of their controls"
    );
    println!(
        "Fixture accounts: manager@example.test (recorder), admin@example.test (independent reviewer), viewer@example.test (read only); the walkthrough fixture's password applies"
    );
    // The real queue executor delivers mounted Supply Chain events into
    // Payables' own copy; the fixture never writes order or receipt facts.
    let executor =
        bilibi_queue::Executor::from_services(services, bilibi_queue::Config::default())?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let listener = tokio::net::TcpListener::bind(
        std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:4316".into()),
    )
    .await?;
    println!("Bills UI listening at {origin}");
    let served = axum::serve(listener, bilibi_web::router(app)).await;
    let _ = stop.send(true);
    let _ = worker.await;
    served?;
    Ok(())
}

/// One page's first render for a signed-in person (NP5).
async fn page_html(
    pages: &PageService<Scope, Error>,
    operation: &'static str,
    scope: &Scope,
    path: &HashMap<String, String>,
    query: &HashMap<String, String>,
) -> Result<String, Error> {
    Ok(pages
        .0
        .render(PageRequest {
            operation,
            scope: Some(scope),
            path,
            query,
            body: &[],
            framed: false,
        })
        .await?
        .html)
}
