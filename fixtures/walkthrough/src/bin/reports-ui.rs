//! NT2 executable page proof on a disposable PostgreSQL database. No mocks.
//! `cargo run -p bilibi-finance-walkthrough-fixture --bin reports-ui` prepares
//! the browser fixture and exercises real Page/LiveState contracts. `post`
//! later records a real entry for the two-browser live/pinned demonstration.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Db, Error, MutationBoundary};
use bilibi_finance_ledger::{
    AccountType, Ledger, MinorUnits, Money, NewAccount, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_reports::SECTIONS;
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::civil::{date, Date};
use serde_json::json;
use std::{collections::HashMap, time::Duration};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

async fn update(state: &mut State, scope: &Scope, input: LiveInput<'_>) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input,
        })
        .await?)
}
async fn mount(
    services: &Services,
    scope: &Scope,
    account: Option<i64>,
    query: &HashMap<String, String>,
) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/reports.pages")?;
    let path = account
        .map(|id| HashMap::from([("account_id".into(), id.to_string())]))
        .unwrap_or_default();
    Ok(pages
        .0
        .mount(PageRequest {
            operation: if account.is_some() {
                "activity"
            } else {
                "reports"
            },
            scope: Some(scope),
            path: &path,
            query,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap())
}
async fn post(
    ledger: &Ledger,
    scope: &Scope,
    debit: i64,
    credit: i64,
    value: &str,
    day: Date,
    description: &str,
) -> Outcome<i64> {
    let amount = Money::parse(value, MinorUnits::new(2).unwrap())?;
    Ok(ledger
        .post_entry(
            scope,
            1,
            &NewEntry {
                entry_date: day,
                description: description.into(),
                reference: Some("report <evidence> & history".into()),
                lines: vec![
                    NewLine::debit(debit, amount),
                    NewLine::credit(credit, amount),
                ],
            },
            &PostOptions::default(),
        )
        .await?
        .entry_number)
}
fn has(body: &LiveBody, expected: &str) {
    assert!(body.html.contains(expected), "page missing {expected}");
}
fn cell<'a>(html: &'a str, id: &str) -> &'a str {
    let start = html
        .find(&format!("id=\"{id}\""))
        .unwrap_or_else(|| panic!("missing {id}"));
    let rest = &html[start..];
    &rest[..rest.find("</tr>").unwrap_or(rest.len())]
}
fn href(html: &str, id: &str) -> String {
    let start = html.find(&format!("id=\"{id}\"")).unwrap();
    // Button's shared link writes href before id.
    let anchor = html[..start].rfind("<a ").unwrap();
    let link = &html[anchor..];
    let h = link.find("href=\"").unwrap() + 6;
    link[h..]
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&")
        .replace("&#38;", "&")
}
fn query(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .unwrap()
        .1
        .split('&')
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap();
            (k.into(), v.replace("%3A", ":"))
        })
        .collect()
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
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    let installed = install(&db, &services).await?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let admin = sign_in(&db, &services, 1, 4).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let find = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .unwrap()
            .id
    };
    let (cash, sales, expense, capital) = (find("1000"), find("4000"), find("5000"), find("3000"));
    if std::env::args().nth(1).as_deref() == Some("post") {
        let through = post(
            &ledger,
            &manager,
            cash,
            sales,
            "17.25",
            date(2026, 10, 17),
            "Later browser posting",
        )
        .await?;
        println!("NT2 real posting committed: through entry {through}; income increases by 17.25");
        return Ok(());
    }
    configure(&db, &services, &manager).await?;
    post(
        &ledger,
        &manager,
        cash,
        capital,
        "1000",
        date(2026, 9, 1),
        "Opening capital",
    )
    .await?;
    post(
        &ledger,
        &manager,
        cash,
        sales,
        "250",
        date(2026, 9, 17),
        "Previous income",
    )
    .await?;
    post(
        &ledger,
        &manager,
        cash,
        sales,
        "1300.50",
        date(2026, 10, 17),
        "Current income <escaped>",
    )
    .await?;
    post(
        &ledger,
        &manager,
        expense,
        cash,
        "200",
        date(2026, 10, 17),
        "Current expense",
    )
    .await?;

    let q = HashMap::from([
        ("view".into(), "profit-and-loss".into()),
        ("period".into(), "custom".into()),
        ("from".into(), "2026-10-01".into()),
        ("to".into(), "2026-10-31".into()),
    ]);
    let mut latest = mount(&services, &manager, None, &q).await?;
    let mut wakes = latest.subscriptions();
    // A commit after LISTEN but before the first mounted read must be counted.
    let through = post(
        &ledger,
        &manager,
        cash,
        sales,
        "0.50",
        date(2026, 10, 17),
        "Mount race",
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(3), wakes[0].receiver.recv()).await??;
    let first = update(&mut latest, &manager, LiveInput::Refresh).await?;
    assert!(cell(&first.html, "profit-and-loss-net-income").contains("1,101.00"));
    assert!(first
        .html
        .replace("&#38;", "&")
        .replace("&amp;", "&")
        .contains(&format!(
            "closing=excluded&from=2026-10-01&through={through}&to=2026-10-31"
        )));
    has(&first, "<select id=\"report-period-period\"");
    assert!(first.html.contains("from=2026-09-01"));
    has(&first, "name=\"through\" value=\"5\"");
    has(
        &first,
        "The report sections name account codes the chart does not have: MISSING.",
    );

    // Advance state before clicking the old control: it must pin the old value.
    let after = post(
        &ledger,
        &manager,
        cash,
        sales,
        "17.25",
        date(2026, 10, 17),
        "Pin click race",
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(3), wakes[0].receiver.recv()).await??;
    let new = update(&mut latest, &manager, LiveInput::Refresh).await?;
    has(&new, &format!("name=\"through\" value=\"{after}\""));
    let pinned = update(
        &mut latest,
        &manager,
        LiveInput::Event(&json!({"type":"pin","through":through.to_string()})),
    )
    .await?;
    assert_eq!(pinned.query["through"], through.to_string());
    assert!(cell(&pinned.html, "profit-and-loss-net-income").contains("1,101.00"));
    // A pinned Refresh and a customization event must not reread latest figures.
    let _ = post(
        &ledger,
        &manager,
        cash,
        sales,
        "2",
        date(2026, 10, 17),
        "Pinned commit",
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(3), wakes[0].receiver.recv()).await??;
    let unchanged = update(&mut latest, &manager, LiveInput::Refresh).await?;
    assert_eq!(pinned.html, unchanged.html);
    let compact = update(
        &mut latest,
        &manager,
        LiveInput::Event(
            &json!({"type":"grid:profit-and-loss","op":"zoom_preset","preset":"compact"}),
        ),
    )
    .await?;
    has(&compact, "data-mode=\"compact\"");
    update(
        &mut latest,
        &manager,
        LiveInput::Event(
            &json!({"type":"period_select:report-period","period_filter[period]":"custom"}),
        ),
    )
    .await?;
    let invalid = update(
        &mut latest,
        &manager,
        LiveInput::Event(
            &json!({"type":"period_apply:report-period","range[from]":"2026-11-01","range[to]":"2026-10-01"}),
        ),
    )
    .await?;
    has(&invalid, "Start date must be on or before end date.");
    assert_eq!(invalid.query["from"], "2026-10-01");
    let _ = post(
        &ledger,
        &manager,
        cash,
        sales,
        "1",
        date(2026, 10, 17),
        "Draft remains open",
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(3), wakes[0].receiver.recv()).await??;
    has(
        &update(&mut latest, &manager, LiveInput::Refresh).await?,
        "report-period-range-error",
    );
    update(
        &mut latest,
        &manager,
        LiveInput::Event(&json!({"type":"period_cancel:report-period"})),
    )
    .await?;
    let unpinned = update(
        &mut latest,
        &manager,
        LiveInput::Event(&json!({"type":"unpin"})),
    )
    .await?;
    assert!(!unpinned.query.contains_key("through"));
    assert!(cell(&unpinned.html, "profit-and-loss-net-income").contains("1,121.25"));
    println!("NT2: mounted read hears commit; stale pin freezes clicked cutoff; pinned notices ignored; unpin latest; range draft/error and compact survive");

    for view in ["overview", "balance-sheet", "cash-position"] {
        let mut params = q.clone();
        params.insert("view".into(), view.into());
        let page = update(&mut latest, &manager, LiveInput::Params(&params)).await?;
        has(
            &page,
            match view {
                "overview" => "overview-figure-net-income",
                "balance-sheet" => "balance-sheet-earnings",
                _ => "cash-position-cash-total",
            },
        );
        if view == "overview" {
            assert!(cell(&page.html, "overview-figure-net-income").contains("1,121.25"));
        }
    }
    update(&mut latest, &manager, LiveInput::Params(&q)).await?;
    let single = update(
        &mut latest,
        &manager,
        LiveInput::Event(&json!({"type":"compare","report[compare]":"false"})),
    )
    .await?;
    assert_eq!(single.query["compare"], "none");
    assert!(!single.html.contains("id=\"profit-and-loss-head-v1\""));
    let contractor = sign_in(&db, &services, 1, 7).await?;
    let mut forbidden = mount(&services, &contractor, None, &q).await?;
    let refusal = update(&mut forbidden, &contractor, LiveInput::Refresh).await?;
    has(&refusal, "The ledger is not open");
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants
        .set_direct(&admin, 1, bilibi_finance_reports::VIEW, Some(false))
        .await?;
    let revoked = update(&mut latest, &manager, LiveInput::Params(&q)).await?;
    has(&revoked, "You do not have permission to view the ledger.");
    assert!(!revoked.html.contains("id=\"profit-and-loss-card\""));
    grants
        .set_direct(&admin, 1, bilibi_finance_reports::VIEW, None)
        .await?;
    println!("NT2: all four views, comparisons, scoped empty/refusal and grant revocation clear old figures");

    pagination(&services, &manager, &ledger, cash, capital).await?;
    println!("NT2: all executable page proofs passed; disposable browser fixture ready (manager@example.test)");
    Ok(())
}

async fn configure(db: &Db, services: &Services, scope: &Scope) -> Outcome<()> {
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            scope.mutation_context("walkthrough.reports.sections"),
        )
        .await?;
    settings.put(&mut tx,scope,Target::Company(OrganisationId::new(1)?),SECTIONS,json!([
        {"report":"profit_and_loss","name":"Operations","accounts":["4000","5000","MISSING"]},
        {"report":"cash_position","name":"Cash and bank","accounts":["1000"]}
    ])).await?;
    tx.commit().await?;
    Ok(())
}
async fn pagination(
    services: &Services,
    scope: &Scope,
    ledger: &Ledger,
    cash: i64,
    capital: i64,
) -> Outcome<()> {
    let account = ledger
        .create_account(
            scope,
            1,
            &NewAccount::new("1010", "Activity boundary", AccountType::Asset),
        )
        .await?;
    let q = HashMap::from([
        ("from".into(), "2026-10-01".into()),
        ("to".into(), "2026-10-31".into()),
        ("closing".into(), "excluded".into()),
    ]);
    for _ in 0..99 {
        post(
            ledger,
            scope,
            account.id,
            capital,
            "0.01",
            date(2026, 10, 17),
            "Cursor boundary",
        )
        .await?;
    }
    let mut state = mount(services, scope, Some(account.id), &q).await?;
    assert!(state.subscriptions().is_empty());
    let ninety_nine = update(&mut state, scope, LiveInput::Refresh).await?;
    assert!(!ninety_nine.html.contains("id=\"activity-next\""));
    assert_eq!(
        ninety_nine.html.matches("<tr id=\"activity-entry-").count(),
        99
    );
    post(
        ledger,
        scope,
        account.id,
        capital,
        "0.01",
        date(2026, 10, 17),
        "Hundredth",
    )
    .await?;
    // Timed Refresh does not turn activity into a moving window.
    assert_eq!(
        ninety_nine.html,
        update(&mut state, scope, LiveInput::Refresh).await?.html
    );
    let hundred = update(&mut state, scope, LiveInput::Params(&q)).await?;
    has(&hundred, "id=\"activity-next\"");
    let old_next = query(&href(&hundred.html, "activity-next"));
    post(
        ledger,
        scope,
        account.id,
        capital,
        "0.01",
        date(2026, 10, 17),
        "Hundred and first",
    )
    .await?;
    let old = update(&mut state, scope, LiveInput::Params(&old_next)).await?;
    assert_eq!(old.html.matches("<tr id=\"activity-entry-").count(), 0);
    assert!(old.html.contains("1.00"));
    let first = update(&mut state, scope, LiveInput::Params(&q)).await?;
    assert_eq!(first.html.matches("<tr id=\"activity-entry-").count(), 100);
    assert!(first.html.contains("1.01"));
    let next = query(&href(&first.html, "activity-next"));
    assert_eq!(next["closing"], "excluded");
    let second = update(&mut state, scope, LiveInput::Params(&next)).await?;
    assert_eq!(second.html.matches("<tr id=\"activity-entry-").count(), 1);
    assert!(second.html.contains("1.01"));
    assert!(!second.html.contains("id=\"activity-next\""));
    assert!(first.html.contains("by user 1"));
    // Exercise escaping through the shared activity cells as well.
    let mut own = mount(services, scope, Some(cash), &q).await?;
    let html = update(&mut own, scope, LiveInput::Refresh).await?.html;
    assert!(html.contains("Current income &lt;escaped&gt;"));
    assert!(html.contains("report &lt;evidence&gt; &amp; history"));
    println!("NT2: activity 99/100/101 boundary, empty final page, stable 1.01 whole-window total, pinned cursor, no timed reread, escaped labels");
    Ok(())
}
