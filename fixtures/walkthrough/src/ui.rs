//! `DATABASE_URL=… APP_ORIGIN=http://127.0.0.1:4316 BIND_ADDR=127.0.0.1:4316
//! cargo run -p bilibi-finance-walkthrough-fixture --bin
//! bilibi-finance-walkthrough-ui`, from the Platform root with this container
//! mounted, against a disposable database: migrates it, installs the Finance
//! walkthrough fixture, configures Base Artifacts' storage as an operator
//! would, keeps a few source documents through the facade, and serves the
//! composed app so the Source documents page, its downloads and the
//! embeddable panel can be tried in a browser.
//!
//! `/walkthrough/records` stands in for the pages of the modules that own
//! records (a bill, a statement), which later Finance slices add: each record
//! page mounts the attachments panel as a panel of the embed
//! `finance.attachments-panel`, exactly as they will. Sign in as
//! `manager@example.test` (every attachments capability), `staff@example.test`
//! (view and attach) or `viewer@example.test` (view), password `PASSWORD`.
use bilibi_database::{pool, Error};
use bilibi_finance_attachments::{NewFile, PANEL_EMBED};
use bilibi_finance_walkthrough_fixture::{
    attachments::configure_storage,
    install,
    records::{self, OPEN_SOURCE},
    sign_in, COMPANY, PASSWORD,
};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Method, Route, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{
    LiveBody, LiveFuture, LiveRequest, LiveState, MountFuture, Page, PageBody, PageFuture,
    PageRequest, PageService, PanelSpec,
};
use jiff::civil::date;
use std::{collections::HashMap, sync::Arc};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();

const MODULE: &str = "extensions/finance_attachments_walkthrough";
const PAGES: &str = "extensions/finance_attachments_walkthrough.pages";

struct Records;

fn record_page(id: &str) -> String {
    format!(
        "<section id=\"walkthrough-record\"><h1 class=\"text-lg font-semibold\">Record {id}</h1>\
<p class=\"text-sm\"><a href=\"/walkthrough/records\">All records</a> · <a href=\"/finance/attachments\">Source documents</a></p>\
<div id=\"record-attachments-slot\" data-live-ignore><div id=\"record-attachments\"></div></div></section>"
    )
}

struct RecordState {
    id: String,
}
impl LiveState<Scope, Error> for RecordState {
    fn update<'a>(&'a mut self, _request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            Ok(LiveBody {
                html: record_page(&self.id),
                ..LiveBody::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        vec![PanelSpec {
            id: "record-attachments".into(),
            embed: PANEL_EMBED.into(),
            path: HashMap::from([
                ("panel".into(), "record-attachments".into()),
                ("source".into(), OPEN_SOURCE.into()),
                ("document".into(), self.id.clone()),
            ]),
            query: HashMap::new(),
        }]
    }
}

impl Page<Scope, Error> for Records {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let html = match request.operation {
                "index" => "<section id=\"walkthrough-records\"><h1 class=\"text-lg font-semibold\">Walkthrough records</h1>\
<ul class=\"mt-3 list-disc pl-5 text-sm\"><li><a id=\"record-1\" href=\"/walkthrough/records/1\">Bill 12 (draft)</a></li>\
<li><a id=\"record-2\" href=\"/walkthrough/records/2\">Bill 7 (final)</a></li></ul></section>"
                    .to_owned(),
                "record" => record_page(request.path.get("id").ok_or(Error::NotFound)?),
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: request.operation == "record",
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "record" {
                return Ok(None);
            }
            let id = request.path.get("id").ok_or(Error::NotFound)?.clone();
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(RecordState { id });
            Ok(Some(state))
        })
    }
}

inventory::submit! {
    Module { id: MODULE, layer: Layer::Extensions,
        dependencies: &["base/ui"], migrations: &[], services: &[
            Service { id: PAGES, factory: |_| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Records)))) },
        ],
    }
}
inventory::submit! { Contribution::new(ROUTES, "walkthrough.records.index", Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/walkthrough/records", service: PAGES, operation: "index", access: Access::SignedIn, framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "walkthrough.records.record", Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/walkthrough/records/{id}", service: PAGES, operation: "record", access: Access::SignedIn, framed: true,
}) }

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    // The app's own services serve the proof too: a second install in one
    // process would register the Platform's bindings twice.
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    // Company 001's viewer and staff accounts join the company, as the
    // attachments walkthrough does.
    db.get()
        .await?
        .batch_execute(
            "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Viewer','human','lee@example.test'),(5,1,'person_5','Sam Staff','human','sam@example.test') ON CONFLICT DO NOTHING; \
             INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','viewer@example.test'),(4,1,5,1,'Clerk','staff@example.test') ON CONFLICT DO NOTHING; \
             UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; \
             UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=3 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; \
             SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1)); \
             SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1))",
        )
        .await?;

    let root = std::env::var("BILIBI_ATTACHMENTS_DEMO_ROOT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join(format!(
                "bilibi-finance-attachments-ui-{}",
                std::process::id()
            ))
            .display()
            .to_string()
    });
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let admin = sign_in(&db, services, 1, 4).await?;
    configure_storage(&db, services, &admin, &root).await?;

    let attachments = services
        .get::<bilibi_finance_attachments::Attachments>(bilibi_finance_attachments::SERVICE)?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let draft = records::add(&db, 1, COMPANY, "open", "Bill 12", date(2026, 3, 15), false).await?;
    let posted = records::add(&db, 1, COMPANY, "open", "Bill 7", date(2025, 11, 2), true).await?;
    for (id, name, bytes) in [
        (
            draft,
            "bill-12-scan.pdf",
            &b"%PDF-1.4 supplier invoice 12"[..],
        ),
        (
            posted,
            "bill-7-scan.pdf",
            &b"%PDF-1.4 supplier invoice 7"[..],
        ),
    ] {
        attachments
            .attach(
                &manager,
                COMPANY,
                OPEN_SOURCE,
                id,
                &NewFile {
                    file_name: name.into(),
                    content: bytes.to_vec(),
                    content_type: Some("application/pdf".into()),
                },
            )
            .await?;
    }
    println!("storage root {root}; records {draft} (draft) and {posted} (final)");
    println!(
        "sign in as manager@example.test, staff@example.test or viewer@example.test, password {PASSWORD}"
    );

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Attachments UI listening at {origin}");
    axum::serve(listener, bilibi_web::staff_router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Internal)?;
    Ok(())
}
