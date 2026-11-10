//! NS3 running-app proof on a disposable database, never a product seed.
//! Run `cargo run -p bilibi-finance-walkthrough-fixture --bin sst` first: it
//! records the registrations, mappings, documents and returns through the
//! real facades. `--bin sst-ui` then proves the mounted SST page states and
//! downloads against those records and serves the composed app.
//! `deny <capability>` / `restore <capability>` withdraw or restore one of the
//! manager's grants, so a revoked grant can be shown mid-page, and
//! `post <date>` posts a manual sales tax entry that an open page follows.
use bilibi_authz::{grants::Grants, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{Ledger, Money, NewEntry, NewLine, PostOptions};
use bilibi_finance_sst::{ReturnStatus, Sst};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::{DownloadRequest, DownloadService, FileBody},
    page::{LiveInput, LiveRequest, LiveState, PageRequest, PageService},
};
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
// The manager holds every SST capability, as in the NS1/NS2 proof.
inventory::submit! { Contribution::new(VOCABULARY,"walkthrough.fixture.finance.sst",Provenance {module:"finance/sst",source:file!(),line:line!()},&VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
    roles:&[
        RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_sst::CAPABILITIES},
    ],
}) }

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// What a reader needs to see every SST figure, and nothing more.
const VIEWER_GRANTS: [&str; 4] = [
    bilibi_finance_sst::VIEW,
    "finance.ledger.view",
    "finance.receivables.view",
    "finance.payables.view",
];

async fn event(
    state: &mut Box<dyn LiveState<Scope, Error>>,
    scope: &Scope,
    value: serde_json::Value,
) -> Outcome<bilibi_ui::page::LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&value),
        })
        .await?)
}

async fn mount(
    pages: &PageService<Scope, Error>,
    scope: &Scope,
    operation: &'static str,
    path: &HashMap<String, String>,
) -> Outcome<Box<dyn LiveState<Scope, Error>>> {
    let query = HashMap::new();
    let mut state = pages
        .0
        .mount(PageRequest {
            operation,
            scope: Some(scope),
            path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .ok_or(Error::Internal)?;
    state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    Ok(state)
}

async fn download(
    files: &DownloadService<Scope, Error>,
    scope: &Scope,
    operation: &'static str,
    id: i64,
) -> Result<FileBody, Error> {
    let path = HashMap::from([("id".into(), id.to_string())]);
    let query = HashMap::new();
    files
        .0
        .serve(DownloadRequest {
            operation,
            scope,
            path: &path,
            query: &query,
        })
        .await
}

async fn render(
    pages: &PageService<Scope, Error>,
    scope: &Scope,
    operation: &'static str,
    path: &HashMap<String, String>,
) -> Outcome<String> {
    let query = HashMap::new();
    Ok(pages
        .0
        .render(PageRequest {
            operation,
            scope: Some(scope),
            path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .html)
}

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
    install(&db, services).await?;
    // Only identity fixtures use SQL; every Finance record comes from facades.
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
    for key in VIEWER_GRANTS {
        grants.set_direct(&admin, 2, key, Some(true)).await?;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("deny") | Some("restore") => {
            let key = args.get(1).ok_or(Error::Invalid)?;
            grants
                .set_direct(&admin, 1, key, (args[0] == "deny").then_some(false))
                .await?;
            println!("{} {key}", args[0]);
            return Ok(());
        }
        // A manual sales tax entry from another session: an open return's
        // page reads its figures again on the posting notice.
        Some("post") => {
            let entry_date = args.get(1).ok_or(Error::Invalid)?.parse()?;
            let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
            let units = ledger.get_book(&manager, COMPANY).await?.minor_units;
            let accounts = ledger.list_accounts(&manager, COMPANY).await?;
            let account = |code: &str| {
                accounts
                    .iter()
                    .find(|a| a.code == code)
                    .map(|a| a.id)
                    .ok_or(Error::NotFound)
            };
            let entry = ledger
                .post_entry(
                    &manager,
                    COMPANY,
                    &NewEntry {
                        entry_date,
                        description: "Sales tax correction without a document".into(),
                        reference: None,
                        lines: vec![
                            NewLine::debit(account("2110")?, Money::parse("5", units)?),
                            NewLine::credit(account("1000")?, Money::parse("5", units)?),
                        ],
                    },
                    &PostOptions::default(),
                )
                .await?;
            println!("posted entry {}", entry.entry_number);
            return Ok(());
        }
        None => (),
        _ => return Err(Error::Invalid.into()),
    }
    let sst = services.get::<Sst>(bilibi_finance_sst::SERVICE)?;
    let returns = sst
        .list_returns(&manager, COMPANY, None, Some(500), None)
        .await?;
    let finalised = returns
        .iter()
        .rev()
        .find(|r| {
            // The latest version, with no amendment of its period open yet.
            r.status == ReturnStatus::Finalised
                && r.superseded_by_return_id.is_none()
                && !returns.iter().any(|o| {
                    o.status == ReturnStatus::Open
                        && o.tax_type == r.tax_type
                        && o.period_start == r.period_start
                })
        })
        .ok_or("run `--bin sst` on this database first")?;
    let discarded = returns
        .iter()
        .find(|r| r.status == ReturnStatus::Discarded)
        .ok_or("run `--bin sst` on this database first")?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let staff = sign_in(&db, services, 1, 3).await?;
    let pages = services.get::<PageService<Scope, Error>>("finance/sst.pages")?;
    let none = HashMap::new();

    // Returns page: the manager prepares; a reader sees the withheld sentence,
    // and a forged prepare from the reader's page is refused and opens nothing.
    let html = render(&pages, &manager, "returns", &none).await?;
    assert!(html.contains("id=\"sst-periods-table\"") && html.contains("id=\"sst-return-new\""));
    assert!(html.contains(&format!("id=\"return-{}\"", finalised.return_number)));
    let html = render(&pages, &viewer, "returns", &none).await?;
    assert!(html.contains("sst-prepare-forbidden") && !html.contains("sst-return-new"));
    let mut state = mount(&pages, &viewer, "returns", &none).await?;
    let start = sst
        .taxable_periods(&viewer, COMPANY, None, Some(1))
        .await?
        .remove(0)
        .period_start;
    let body = event(
        &mut state,
        &viewer,
        serde_json::json!({"type": "prepare", "period": format!("sales_tax|{start}")}),
    )
    .await?;
    assert!(body.html.contains("You do not have permission to do that."));
    let after = sst
        .list_returns(&manager, COMPANY, None, Some(500), None)
        .await?;
    assert_eq!(after.len(), returns.len());
    println!("returns page: reader sees no prepare control; a forged prepare is refused and opens nothing");

    // Return page: the finalised version shows its snapshot and Amend; the
    // reader sees it without Amend; a reason dialog refuses a blank reason.
    let path = HashMap::from([("id".into(), finalised.id.to_string())]);
    let html = render(&pages, &manager, "return", &path).await?;
    assert!(html.contains("sst-return-items-table") && html.contains("id=\"sst-return-amend\""));
    let html = render(&pages, &viewer, "return", &path).await?;
    assert!(html.contains("sst-return-items-table") && !html.contains("id=\"sst-return-amend\""));
    let mut state = mount(&pages, &manager, "return", &path).await?;
    event(
        &mut state,
        &manager,
        serde_json::json!({"type": "open_modal", "modal": "amend"}),
    )
    .await?;
    let body = event(
        &mut state,
        &manager,
        serde_json::json!({"type": "submit_reason", "reason[reason]": ""}),
    )
    .await?;
    assert!(body.html.contains("Refused: reason") && body.html.contains("sst-reason-modal"));
    let still = sst.get_return(&manager, COMPANY, finalised.id).await?;
    assert_eq!(still.tax_return.superseded_by_return_id, None);
    println!(
        "return page: snapshot shown; amend refused without a reason, dialog kept, nothing opened"
    );

    // Downloads: every request asks the facade again.
    let files = services.get::<DownloadService<Scope, Error>>("finance/sst.files")?;
    let csv = download(&files, &manager, "csv", finalised.id).await?;
    let headers = csv.headers();
    println!("csv: {} bytes, {:?}", csv.bytes.len(), headers);
    assert!(String::from_utf8(csv.bytes)?.contains("Finalised, version"));
    let print = download(&files, &viewer, "print", finalised.id).await?;
    assert!(print
        .headers()
        .iter()
        .any(|(k, v)| *k == "content-disposition" && v.starts_with("inline;")));
    assert!(matches!(
        download(&files, &staff, "csv", finalised.id).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        download(&files, &manager, "csv", discarded.id).await,
        Err(Error::NotFound)
    ));
    println!("downloads: CSV attachment and inline print page; unaffiliated account refused; discarded return not exported");

    println!("Fixture accounts: manager@example.test (every SST capability), viewer@example.test (view only); the walkthrough fixture's password applies");
    println!(
        "Finalised return: {origin}/finance/sst/returns/{}",
        finalised.id
    );
    let listener = tokio::net::TcpListener::bind(
        std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:4316".into()),
    )
    .await?;
    println!("SST UI listening at {origin}");
    axum::serve(listener, bilibi_web::router(app)).await?;
    Ok(())
}
