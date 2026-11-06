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
    has(
        &refresh(&mut categories, &manager).await?,
        "No categories yet.",
    );
    let category_form = json!({"type":"record_asset","asset[name]":"Equipment <record>","asset[cost_account_id]":cost.id.to_string(),"asset[accumulated_account_id]":accumulated.id.to_string(),"asset[expense_account_id]":find("5000").to_string(),"asset[gain_account_id]":find("4000").to_string(),"asset[loss_account_id]":find("5000").to_string(),"asset[method]":"straight_line","asset[life_months]":"3","asset[reason]":"Initial assumptions"});
    has(
        &event(&mut categories, &manager, category_form.clone()).await?,
        "Recorded.",
    );
    let original = assets.list_categories(&manager, COMPANY).await?.remove(0);
    let mut revised = category_form.clone();
    revised["asset[category_id]"] = original.id.to_string().into();
    revised["asset[life_months]"] = "4".into();
    has(
        &event(&mut categories, &manager, revised).await?,
        "Recorded.",
    );
    assert_eq!(
        assets.list_categories(&manager, COMPANY).await?[0]
            .revisions
            .len(),
        2
    );
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
    has(&applied, "Whole register reconciliation as of 2026-12-31");
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
        let body=event(&mut register,&manager,json!({"type":"record_asset","asset[source]":"entry","asset[category_revision_id]":original.revision.id.to_string(),"asset[description]":label,"asset[entry_id]":entry.id.to_string(),"asset[entry_line_number]":"1","asset[in_service_date]":"2026-01-31","asset[residual_value]":"0"})).await?;
        has(&body, "Recorded.");
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
    has(&first, "No depreciation charges posted.");
    has(&first, "Equipment &#60;one&#62;");
    println!("NX3: acquisition form and asset assumptions/schedule rendered");
    let boundary = refresh(&mut register, &manager).await?;
    has(&boundary, "assets-report-reproduce");
    let bad=event(&mut register,&manager,json!({"type":"record_asset","asset[source]":"bill","asset[bill_id]":"nope","asset[category_revision_id]":original.revision.id.to_string(),"asset[description]":"Keep draft","asset[in_service_date]":"2026-02-01"})).await?;
    has(&bad, "bill_id: is invalid");
    has(&bad, "Keep draft");
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
    let off_month = event(
        &mut runs,
        &manager,
        json!({"type":"record_asset","asset[period]":"2026-01-15"}),
    )
    .await?;
    has(&off_month, "Invalid period");
    no(&off_month, "base:");
    let run_body = event(
        &mut runs,
        &manager,
        json!({"type":"record_asset","asset[period]":"2026-01-01"}),
    )
    .await?;
    has(&run_body, "Recorded without asset #2");
    has(&run_body, "#2 Reversed acquisition");
    has(&run_body, "Skipped assets");
    has(&refresh(&mut detail, &manager).await?, "40");
    let disposal=event(&mut detail,&manager,json!({"type":"dispose","asset[effective_date]":"2026-02-01","asset[proceeds]":"0","asset[reason]":"Write off"})).await?;
    has(&disposal, "Recorded in the ledger.");
    has(&disposal, "disposed");
    has(&event(&mut detail,&manager,json!({"type":"reverse","asset[effective_date]":"2026-02-01","asset[reason]":"Restore"})).await?,"disposal_reversed");
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
    no(&body, "asset-void-form");
    let charge = assets.get_asset(&manager, COMPANY, ids[2]).await?;
    assert_eq!(charge.charges.len(), 1);
    assert!(charge.charges[0].reversal.is_some());
    let run = assets.list_runs(&manager, COMPANY).await?.remove(0);
    has(&event(&mut runs,&manager,json!({"type":"reverse","asset[run_id]":run.id.to_string(),"asset[effective_date]":"2026-02-01","asset[reason]":"Correct run"})).await?,"Depreciation reversed.");
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
    let denied = event(&mut register, &manager, json!({"type":"record_asset"})).await?;
    has(&denied, "You do not have permission for that action.");
    has(&denied, "assets-write-withheld");
    grants
        .set_direct(&admin, 1, bilibi_finance_fixed_assets::ACQUIRE, None)
        .await?;
    let mut restricted = mount(services, &staff, "runs", None, &empty).await?;
    let body = refresh(&mut restricted, &staff).await?;
    has(&body, "assets-write-withheld");
    has(&body, "assets-reverse-withheld");
    let mut restricted = mount(services, &staff, "asset", Some(ids[0]), &empty).await?;
    let body = refresh(&mut restricted, &staff).await?;
    has(&body, "asset-dispose-withheld");
    has(&body, "asset-reverse-withheld");
    has(&body, "asset-void-withheld");
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
