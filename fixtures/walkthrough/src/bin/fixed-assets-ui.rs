//! NX3 running proof: real facade records and mounted Page/LiveState events.
//! Use a fresh disposable database. Add `serve` to keep the composed app open
//! for browser proof after the checks, with APP_ORIGIN and BIND_ADDR set.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_fixed_assets::{
    AcquisitionInput, Assumptions, FixedAssets, Method, RevisionInput, CAPABILITIES,
};
use bilibi_finance_ledger::{
    AccountType, Ledger, MinorUnits, Money, NewAccount, NewEntry, NewLine, PostOptions, Reversal,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::civil::date;
use serde_json::{json, Value};
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.finance.fixed-assets-ui",Provenance {module:"finance/fixed_assets",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[
        bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&CAPABILITIES},
        bilibi_authz::RoleDefinition {code:"staff",name:"staff",description:"Walkthrough fixture role",grant_all:false,capabilities:&[bilibi_finance_fixed_assets::VIEW]},
    ],
}) }
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;
static CREDENTIALS: std::sync::OnceLock<HashMap<i64, String>> = std::sync::OnceLock::new();
async fn mount(
    services: &Services,
    s: &Scope,
    op: &'static str,
    id: Option<i64>,
    query: &HashMap<String, String>,
) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/fixed_assets.pages")?;
    let path = id
        .map(|id| HashMap::from([("id".into(), id.to_string())]))
        .unwrap_or_default();
    Ok(pages
        .0
        .mount(PageRequest {
            operation: op,
            scope: Some(s),
            path: &path,
            query,
            body: &[],
            framed: true,
        })
        .await?
        .unwrap())
}
async fn event(state: &mut State, s: &Scope, v: Value) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope: s,
            credential: CREDENTIALS
                .get()
                .and_then(|tokens| tokens.get(&s.user()))
                .map(String::as_str)
                .unwrap_or(""),
            input: LiveInput::Event(&v),
        })
        .await?)
}
async fn refresh(state: &mut State, s: &Scope) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope: s,
            credential: CREDENTIALS
                .get()
                .and_then(|tokens| tokens.get(&s.user()))
                .map(String::as_str)
                .unwrap_or(""),
            input: LiveInput::Refresh,
        })
        .await?)
}
fn has(body: &LiveBody, text: &str) {
    assert!(body.html.contains(text), "missing {text}");
}
fn no(body: &LiveBody, text: &str) {
    assert!(!body.html.contains(text), "unexpected {text}");
}
fn money(s: &str) -> Money {
    Money::parse(s, MinorUnits::new(2).unwrap()).unwrap()
}
#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_module_registry::verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    bilibi_database::migrate(&db, &modules).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4333".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    // Bind the fixture contractor to this company through Core's owners,
    // then grant only read capabilities for the restricted browser session.
    let affiliations =
        services.get::<bilibi_affiliation::AffiliationService>(bilibi_affiliation::SERVICE)?;
    let users = services.get::<bilibi_user::UserService>("core/user.accounts")?;
    let affiliation = affiliations
        .create(
            &admin,
            &bilibi_affiliation::AffiliationInput {
                person_id: bilibi_affiliation::PersonId::new(2)?,
                organisation_id: bilibi_affiliation::OrganisationId::new(COMPANY)?,
                title: Some("Asset register reviewer".into()),
                work_email: None,
                work_phone: None,
                dates: bilibi_affiliation::EffectiveDates::new(None, None)?,
            },
        )
        .await?;
    users
        .link_person(&admin, 7, affiliation.affiliation.person_id)
        .await?;
    users
        .set_desk(
            &admin,
            affiliation.affiliation.organisation_id.get(),
            affiliation.affiliation.person_id.get(),
            true,
        )
        .await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    for cap in [
        bilibi_finance_fixed_assets::VIEW,
        bilibi_finance_ledger::VIEW,
    ] {
        grants.set_direct(&admin, 7, cap, Some(true)).await?;
    }
    let staff = sign_in(&db, services, 1, 7).await?;
    let users = services.get::<bilibi_user::UserService>("core/user.accounts")?;
    let mut credentials = HashMap::new();
    for user in [1, 7] {
        let email: String = db
            .get()
            .await?
            .query_one(
                "SELECT i.email FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id=$1",
                &[&user],
            )
            .await?
            .get(0);
        let login = users
            .login(&email, bilibi_finance_walkthrough_fixture::PASSWORD)
            .await?;
        assert!(!login.pending);
        credentials.insert(user, login.token);
    }
    CREDENTIALS
        .set(credentials)
        .map_err(|_| "duplicate credentials")?;

    let assets = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let find = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    let cost = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1200", "Asset cost", AccountType::Asset),
        )
        .await?;
    let accumulated = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1290", "Accumulated depreciation", AccountType::Asset),
        )
        .await?;
    let defaults = RevisionInput {
        cost_account_id: cost.id,
        accumulated_account_id: accumulated.id,
        expense_account_id: find("5000"),
        gain_account_id: find("4000"),
        loss_account_id: find("5000"),
        assumptions: Assumptions {
            method: Method::StraightLine,
            life_months: 3,
            annual_rate: None,
        },
        reason: "Initial assumptions".into(),
    };
    let empty = HashMap::new();
    let mut categories = mount(services, &manager, "categories", None, &empty).await?;
    let empty_list = refresh(&mut categories, &manager).await?;
    has(&empty_list, "No asset categories yet");
    has(&empty_list, "assets-empty-create");
    has(
        &event(&mut categories, &manager, json!({"type":"new"})).await?,
        "category-modal",
    );
    let category_form = json!({"type":"create_category","asset[name]":"Equipment <record>","asset[cost_account_id]":cost.id.to_string(),"asset[accumulated_account_id]":accumulated.id.to_string(),"asset[expense_account_id]":find("5000").to_string(),"asset[gain_account_id]":find("4000").to_string(),"asset[loss_account_id]":find("5000").to_string(),"asset[method]":"straight_line","asset[life_months]":"3"});
    let mut blank = category_form.clone();
    blank["asset[name]"] = "".into();
    let refused = event(&mut categories, &manager, blank).await?;
    has(&refused, "Name can&#39;t be blank");
    has(&refused, "category-modal");
    let added = event(&mut categories, &manager, category_form.clone()).await?;
    has(&added, "Added Equipment &#60;record&#62;.");
    no(&added, "category-modal");
    let original = assets.list_categories(&manager, COMPANY).await?.remove(0);
    // A default changed in place is a complete new revision.
    has(
        &event(
            &mut categories,
            &manager,
            json!({"type":"save_category","id":original.id.to_string(),"field":"life_months","value":"4"}),
        )
        .await?,
        "Useful life changed to 4 months",
    );
    let revised = assets.list_categories(&manager, COMPANY).await?.remove(0);
    assert_eq!(revised.revisions.len(), 2);
    assert_eq!(revised.revision.defaults.assumptions.life_months, 4);
    // Reducing balance waits on the page for its rate; nothing half-set is saved.
    has(
        &event(
            &mut categories,
            &manager,
            json!({"type":"save_category","id":original.id.to_string(),"field":"method","value":"reducing_balance"}),
        )
        .await?,
        "Enter the annual rate to switch to reducing balance.",
    );
    assert_eq!(
        assets.list_categories(&manager, COMPANY).await?[0]
            .revisions
            .len(),
        2
    );
    event(
        &mut categories,
        &manager,
        json!({"type":"save_category","id":original.id.to_string(),"field":"annual_rate","value":"20"}),
    )
    .await?;
    let reducing = assets.list_categories(&manager, COMPANY).await?.remove(0);
    assert_eq!(reducing.revisions.len(), 3);
    assert_eq!(
        reducing.revision.defaults.assumptions.method,
        Method::ReducingBalance
    );
    event(
        &mut categories,
        &manager,
        json!({"type":"save_category","id":original.id.to_string(),"field":"method","value":"straight_line"}),
    )
    .await?;
    let original = assets.list_categories(&manager, COMPANY).await?.remove(0);
    assert_eq!(
        original.revision.defaults.assumptions.method,
        Method::StraightLine
    );
    assert_eq!(original.revision.defaults.assumptions.annual_rate, None);
    for i in 0..26 {
        assets
            .create_category(&manager, COMPANY, &format!("Category {i:02}"), &defaults)
            .await?;
    }
    has(
        &refresh(&mut categories, &manager).await?,
        "Showing 1 to 25 of 27 results",
    );
    has(
        &event(&mut categories, &manager, json!({"type":"page","page":"2"})).await?,
        "Showing 26 to 27 of 27 results",
    );
    let filtered = event(
        &mut categories,
        &manager,
        json!({"type":"filter","filters[search]":"Equipment","filters[perPage]":"50"}),
    )
    .await?;
    assert_eq!(filtered.query["page_size"], "50");
    assert_eq!(filtered.query["page"], "1");
    has(&filtered, "Equipment &#60;record&#62;");
    println!("NX3: category creation/revision, escaping, search and pagination passed");
    let query = HashMap::from([
        ("period".into(), "custom".into()),
        ("from".into(), "2026-01-01".into()),
        ("to".into(), "2026-12-31".into()),
    ]);
    let mut register = mount(services, &manager, "register", None, &query).await?;
    let opened = event(
        &mut register,
        &manager,
        json!({"type":"period_select:assets-report-period","period_filter[period]":"custom"}),
    )
    .await?;
    has(&opened, "assets-report-period-range");
    let invalid = event(
        &mut register,
        &manager,
        json!({"type":"period_apply:assets-report-period","range[from]":"2026-02-01","range[to]":"2026-01-01"}),
    )
    .await?;
    has(&invalid, "Start date must be on or before end date.");
    let applied = event(
        &mut register,
        &manager,
        json!({"type":"period_apply:assets-report-period","range[from]":"2026-01-01","range[to]":"2026-12-31"}),
    )
    .await?;
    assert_eq!(applied.query["to"], "2026-12-31");
    has(&applied, "Ledger check as of 31 Dec 2026");
    let mut ids = Vec::new();
    let mut entries = Vec::new();
    for label in [
        "Equipment <one>",
        "Reversed acquisition",
        "Void after charge",
    ] {
        let entry = ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    entry_date: date(2026, 1, 17),
                    description: label.into(),
                    reference: None,
                    lines: vec![
                        NewLine::debit(cost.id, money("120")),
                        NewLine::credit(find("3000"), money("120")),
                    ],
                },
                &PostOptions::default(),
            )
            .await?;
        // The person picks the purchase; nobody types an entry number.
        let source = format!("entry:{}:1", entry.id);
        has(
            &event(&mut register, &manager, json!({"type":"new"})).await?,
            "asset-modal",
        );
        let chosen = event(
            &mut register,
            &manager,
            json!({"type":"asset_change","asset[category_revision_id]":original.revision.id.to_string(),"asset[source]":source}),
        )
        .await?;
        has(&chosen, &source);
        // The purchase's description and date fill the dialog.
        has(&chosen, "17 Jan 2026");
        let body=event(&mut register,&manager,json!({"type":"register_asset","asset[category_revision_id]":original.revision.id.to_string(),"asset[source]":source,"asset[description]":label,"asset[in_service_date]":"2026-01-31","asset[residual_value]":"0"})).await?;
        has(&body, "Registered ");
        no(&body, "asset-modal");
        let reopened = event(&mut register, &manager, json!({"type":"new"})).await?;
        no(&reopened, &source);
        event(&mut register, &manager, json!({"type":"close_asset"})).await?;
        entries.push(entry);
    }
    let report = assets
        .register_report(&manager, COMPANY, date(2026, 12, 31), &Default::default())
        .await?;
    ids.extend(report.rows.iter().map(|r| r.asset.id));
    let mut detail = mount(services, &manager, "asset", Some(ids[0]), &empty).await?;
    let first = refresh(&mut detail, &manager).await?;
    has(&first, "data-workspace-record");
    has(&first, "asset-schedule");
    has(&first, "No depreciation posted yet.");
    has(&first, "Sell or write off");
    has(&first, "Equipment &#60;one&#62;");
    println!("NX3: acquisition form and asset assumptions/schedule rendered");
    let boundary = refresh(&mut register, &manager).await?;
    has(&boundary, "assets-report-reproduce");
    // A row's own actions are icon buttons; there is no "Non-local actions"
    // menu here (its accessible name is "Actions for …" until the Platform
    // renames it, so both spellings are checked).
    has(&boundary, &format!("assets-asset-{}-dispose", ids[0]));
    no(&boundary, "Actions for");
    no(&boundary, "Non-local actions");
    event(&mut register, &manager, json!({"type":"new"})).await?;
    let bad=event(&mut register,&manager,json!({"type":"register_asset","asset[source]":"bill:9999:1","asset[category_revision_id]":original.revision.id.to_string(),"asset[description]":"Keep draft","asset[in_service_date]":"2026-02-01"})).await?;
    has(&bad, "Bought on can&#39;t be blank");
    has(&bad, "Keep draft");
    has(&bad, "asset-modal");
    no(
        &event(&mut register, &manager, json!({"type":"close_asset"})).await?,
        "asset-modal",
    );
    ledger
        .reverse_entry(
            &manager,
            COMPANY,
            entries[1].id,
            &Reversal {
                entry_date: date(2026, 1, 31),
                description: "Source correction".into(),
                reference: None,
            },
        )
        .await?;
    let mut runs = mount(services, &manager, "runs", None, &empty).await?;
    has(
        &refresh(&mut runs, &manager).await?,
        "No depreciation runs yet",
    );
    has(
        &event(&mut runs, &manager, json!({"type":"new"})).await?,
        "run-modal",
    );
    let off_month = event(
        &mut runs,
        &manager,
        json!({"type":"run_depreciation","asset[period]":"2026-01-15"}),
    )
    .await?;
    has(&off_month, "Invalid period");
    has(&off_month, "run-modal");
    no(&off_month, "base:");
    let run_body = event(
        &mut runs,
        &manager,
        json!({"type":"run_depreciation","asset[period]":"2026-01"}),
    )
    .await?;
    has(&run_body, "Posted without asset #2");
    no(&run_body, "run-modal");
    has(&run_body, "/finance/ledger/journals/entry-");
    has(&run_body, "#2 Reversed acquisition");
    has(&run_body, "Skipped assets");
    has(&refresh(&mut detail, &manager).await?, "40");
    has(
        &event(&mut detail, &manager, json!({"type":"open_dispose"})).await?,
        "asset-dispose-modal",
    );
    let disposal=event(&mut detail,&manager,json!({"type":"dispose","asset[effective_date]":"2026-02-01","asset[proceeds]":"0","asset[reason]":"Write off"})).await?;
    has(&disposal, "Disposal posted.");
    has(&disposal, "Sold or written off");
    // A disposed asset offers its reversal, and no longer a sale.
    has(&disposal, "Reverse disposal");
    no(&disposal, "asset-dispose\"");
    let restored = event(&mut detail,&manager,json!({"type":"reverse_disposal","asset[effective_date]":"2026-02-01","asset[reason]":"Restore"})).await?;
    has(&restored, "Disposal reversed");
    ledger
        .reverse_entry(
            &manager,
            COMPANY,
            entries[2].id,
            &Reversal {
                entry_date: date(2026, 2, 1),
                description: "Source correction".into(),
                reference: None,
            },
        )
        .await?;
    let mut voided = mount(services, &manager, "asset", Some(ids[2]), &empty).await?;
    // A reversed purchase offers Void, not a sale.
    let reversed = refresh(&mut voided, &manager).await?;
    has(&reversed, "Purchase reversed");
    has(&reversed, "open_void");
    no(&reversed, "open_dispose");
    let void_form = json!({"type":"void","asset[effective_date]":"2026-02-01","asset[reason]":"Retire reversed acquisition"});
    has(
        &event(&mut voided, &manager, void_form.clone()).await?,
        "asset-void-confirm",
    );
    assert!(!assets.get_asset(&manager, COMPANY, ids[2]).await?.voided);
    no(
        &event(&mut voided, &manager, json!({"type":"cancel_void"})).await?,
        "asset-void-confirm",
    );
    event(&mut voided, &manager, void_form).await?;
    let body = event(&mut voided, &manager, json!({"type":"confirm_void"})).await?;
    has(&body, "asset-voided");
    no(&body, "asset-void-modal");
    no(&body, "open_void");
    let charge = assets.get_asset(&manager, COMPANY, ids[2]).await?;
    assert_eq!(charge.charges.len(), 1);
    assert!(charge.charges[0].reversal.is_some());
    let run = assets.list_runs(&manager, COMPANY).await?.remove(0);
    has(
        &refresh(&mut runs, &manager).await?,
        &format!("assets-run-{}-reverse", run.id),
    );
    let mut reversing = mount(
        services,
        &manager,
        "runs",
        None,
        &HashMap::from([("reverse".into(), run.id.to_string())]),
    )
    .await?;
    has(
        &refresh(&mut reversing, &manager).await?,
        "run-reverse-modal",
    );
    has(&event(&mut reversing,&manager,json!({"type":"reverse_run","asset[effective_date]":"2026-02-01","asset[reason]":"Correct run"})).await?,"Depreciation reversed.");
    assert!(
        assets.get_asset(&manager, COMPANY, ids[0]).await?.charges[0]
            .reversal
            .is_some()
    );
    println!("NX3: run skips, disposal/reversal, void confirmation and charge corrections passed");
    let pinned_report = assets
        .register_report(&manager, COMPANY, date(2026, 12, 31), &Default::default())
        .await?;
    let pinned_query = HashMap::from([
        ("period".into(), "custom".into()),
        ("from".into(), "2026-01-01".into()),
        ("to".into(), "2026-12-31".into()),
        (
            "through_entry_number".into(),
            pinned_report.through_entry_number.to_string(),
        ),
        (
            "through_asset_number".into(),
            pinned_report.through_asset_number.to_string(),
        ),
        (
            "through_event_number".into(),
            pinned_report.through_event_number.to_string(),
        ),
        (
            "through_category_revision_id".into(),
            pinned_report.through_category_revision_id.to_string(),
        ),
    ]);
    let entry = ledger
        .post_entry(
            &manager,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 2, 1),
                description: "Asset after report pin".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(cost.id, money("120")),
                    NewLine::credit(find("3000"), money("120")),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    assets
        .acquire_from_entry(
            &manager,
            COMPANY,
            entry.id,
            1,
            &AcquisitionInput {
                category_revision_id: original.revision.id,
                description: "Asset after report pin".into(),
                in_service_date: date(2026, 2, 1),
                residual_value: None,
                method: None,
                life_months: None,
                annual_rate: None,
            },
        )
        .await?;
    let mut pinned = mount(services, &manager, "register", None, &pinned_query).await?;
    let body = refresh(&mut pinned, &manager).await?;
    no(&body, "Asset after report pin");
    assert!(body.query.contains_key("through_asset_number"));
    let body = event(
        &mut pinned,
        &manager,
        json!({"type":"filter","filters[source]":"ordinary","filters[status]":"","filters[perPage]":"25"}),
    )
    .await?;
    has(&body, "Asset after report pin");
    assert!(!body.query.contains_key("through_asset_number"));
    let mut pinned = mount(services, &manager, "register", None, &pinned_query).await?;
    let body = event(&mut pinned, &manager, json!({"type":"page","page":"2"})).await?;
    assert!(!body.query.contains_key("through_asset_number"));
    let mut pinned = mount(services, &manager, "register", None, &pinned_query).await?;
    let body = event(
        &mut pinned,
        &manager,
        json!({"type":"period_apply:assets-report-period","range[from]":"2026-01-01","range[to]":"2026-12-31"}),
    )
    .await?;
    assert!(!body.query.contains_key("through_asset_number"));
    let mut pinned = mount(services, &manager, "register", None, &pinned_query).await?;
    let body = event(
        &mut pinned,
        &manager,
        json!({"type":"grid","op":"zoom_preset","preset":"compact"}),
    )
    .await?;
    assert!(body.query.contains_key("through_asset_number"));
    no(&body, "Asset after report pin");
    println!("NX3: pinned register filters, pages and periods render the canonical snapshot");
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants
        .set_direct(&admin, 1, bilibi_finance_fixed_assets::ACQUIRE, Some(false))
        .await?;
    let denied = event(&mut register, &manager, json!({"type":"new"})).await?;
    has(&denied, "You do not have permission to do that.");
    no(&denied, "assets-create");
    no(&denied, "asset-modal");
    grants
        .set_direct(&admin, 1, bilibi_finance_fixed_assets::ACQUIRE, None)
        .await?;
    let mut restricted = mount(services, &staff, "runs", None, &empty).await?;
    let body = refresh(&mut restricted, &staff).await?;
    no(&body, "assets-create");
    no(&body, "?reverse=");
    let mut restricted = mount(services, &staff, "asset", Some(ids[0]), &empty).await?;
    let body = refresh(&mut restricted, &staff).await?;
    no(&body, "open_dispose");
    no(&body, "open_reverse");
    no(&body, "open_void");
    let mut columns = mount(services, &manager, "runs", None, &empty).await?;
    let body = event(
        &mut columns,
        &manager,
        json!({"type":"grid","op":"zoom_preset","preset":"compact"}),
    )
    .await?;
    has(&body, "data-mode=\"compact\"");
    event(
        &mut columns,
        &manager,
        json!({"type":"grid","op":"remove","spec":"exceptions"}),
    )
    .await?;
    let body = event(
        &mut columns,
        &manager,
        json!({"type":"grid","op":"suggest","add":"Skipped"}),
    )
    .await?;
    has(&body, "Skipped assets");
    let mut missing = mount(services, &manager, "asset", Some(i64::MAX), &empty).await?;
    has(&refresh(&mut missing, &manager).await?, "Asset unavailable");
    println!("NX3: page writes, lists, pagination, escaping, charges, corrections, confirmation, permissions and column controls passed");
    if std::env::args().any(|a| a == "serve") {
        let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4333".into());
        println!("NX3 browser fixture ready at {origin}; asset {}", ids[0]);
        axum::serve(
            tokio::net::TcpListener::bind(bind).await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}
