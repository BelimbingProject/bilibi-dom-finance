//! The month-end checklist on the click-through fixture's company: the
//! contract an agent reads (`finance.reports.month_close.get`), then the
//! page through its own `LiveState`, closing one month and then several.
use bilibi_agent_api::page::PageContracts;
use bilibi_database::Error;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use serde_json::{json, Value};
use std::collections::HashMap;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

const READ: &str = "finance.reports.month_close.get";

fn has(body: &LiveBody, expected: &str) {
    assert!(body.html.contains(expected), "page missing {expected}");
}

fn lacks(body: &LiveBody, unexpected: &str) {
    assert!(!body.html.contains(unexpected), "page shows {unexpected}");
}

async fn act(
    state: &mut State,
    scope: &Scope,
    credential: &str,
    event: Value,
) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential,
            input: LiveInput::Event(&event),
        })
        .await?)
}

/// `company` has the fiscal year 2026 with every month open and October's
/// records from the fixture: bills, an asset in service, a bank account with
/// no statement yet.
pub async fn proof(services: &Services, manager: &Scope, company: i64) -> Outcome<()> {
    let contracts = services.get::<PageContracts>(bilibi_agent_api::page::SERVICE)?;
    let read = |input: Value| {
        let contracts = contracts.clone();
        async move {
            Ok::<Value, Box<dyn std::error::Error>>(
                contracts.read(manager, READ, &input).await?["result"].clone(),
            )
        }
    };

    // October by name: every area answers, and each thing left names its page.
    let october = read(json!({"company_id": company, "month": "2026-10"})).await?;
    assert_eq!(october["period"]["name"], "October 2026");
    assert_eq!(october["fiscal_year"], "January to December 2026");
    assert_eq!(october["close"]["allowed"], false);
    assert_eq!(october["close"]["blocked"], "Close January 2026 first.");
    let checks = october["checks"].as_array().expect("checks");
    println!("Month-end checklist, October 2026:");
    for check in checks {
        println!(
            "  {} [{} · {}] {} -> {} {}",
            check["name"].as_str().unwrap_or_default(),
            check["state"].as_str().unwrap_or_default(),
            check["badge"].as_str().unwrap_or_default(),
            check["found"].as_str().unwrap_or_default(),
            check["action"].as_str().unwrap_or_default(),
            check["path"].as_str().unwrap_or_default(),
        );
        if check["state"] == "open" {
            assert!(
                check["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with("/finance/")),
                "{} has nowhere to go",
                check["id"]
            );
            assert!(check["action"]
                .as_str()
                .is_some_and(|action| !action.is_empty()));
        }
    }
    let ids: Vec<&str> = checks.iter().filter_map(|c| c["id"].as_str()).collect();
    for id in [
        "draft-invoices",
        "draft-bills",
        "unposted-bills",
        "payments",
        "journals",
        "late-entries",
        "depreciation",
        "payroll",
        "sst",
        "einvoices",
        "trial-balance",
    ] {
        assert!(ids.contains(&id), "no {id} check");
    }
    assert!(
        ids.iter().any(|id| id.starts_with("bank-")),
        "no bank check"
    );
    let state = |id: &str| {
        checks
            .iter()
            .find(|c| {
                c["id"]
                    .as_str()
                    .is_some_and(|i| i == id || i.starts_with(id))
            })
            .map(|c| c["state"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    };
    // The fixture's asset went into service on 1 October with no run, and
    // its bank account has never been reconciled.
    assert_eq!(state("depreciation"), "open");
    assert_eq!(state("bank-"), "open");
    assert_eq!(state("trial-balance"), "clear");
    assert!(october["open_checks"]
        .as_u64()
        .is_some_and(|open| open >= 2));

    // No month named: the earliest open one, which may close, alone or with
    // the months after it up to November (December closes with the year).
    let first = read(json!({"company_id": company})).await?;
    assert_eq!(first["period"]["name"], "January 2026");
    assert_eq!(first["close"]["allowed"], true);
    assert_eq!(first["close"]["operation"], "finance.ledger.period.close");
    let later = first["later"].as_array().expect("later months");
    assert_eq!(later.len(), 10);
    assert_eq!(later[9]["name"], "November 2026");
    let april = later[2]["period_id"].as_i64().expect("April");
    let december = read(json!({"company_id": company, "month": "2026-12"})).await?;
    assert_eq!(december["close"]["allowed"], false);
    assert!(december["close"]["blocked"]
        .as_str()
        .is_some_and(|text| text.contains("closes with the year")));
    // A run reads as one: the dated checks cover February to April.
    let run = read(json!({"company_id": company, "month": "2026-02", "through_period_id": april}))
        .await?;
    assert_eq!(run["name"], "February to April 2026");
    assert!(contracts
        .read(
            manager,
            READ,
            &json!({"company_id": company, "month": "2031-01"})
        )
        .await
        .is_err());

    // The page: close January, land on February, then close February to
    // April together.
    let login = services
        .get::<bilibi_core::user::UserService>("core/user.accounts")?
        .login("manager@other.example.test", crate::PASSWORD)
        .await?;
    assert!(!login.pending);
    let pages = services.get::<PageService<Scope, Error>>("finance/reports.month-close-pages")?;
    let mut page: State = pages
        .0
        .mount(PageRequest {
            operation: "month-close",
            scope: Some(manager),
            path: &HashMap::new(),
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .expect("the month-end checklist mounts");
    let shown = page
        .update(LiveRequest {
            scope: manager,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    has(&shown, "Close January 2026");
    has(&shown, ">Close January<");
    has(&shown, "Close several months");
    has(&shown, "Before you close");
    has(&shown, "Back to fiscal years");
    let asked = act(&mut page, manager, &login.token, json!({"type":"close"})).await?;
    has(&asked, "Close January 2026?");
    has(
        &asked,
        "No one can add or change entries dated in January until someone reopens it.",
    );
    let cancelled = act(&mut page, manager, &login.token, json!({"type":"cancel"})).await?;
    lacks(&cancelled, "month-close-confirm");
    act(&mut page, manager, &login.token, json!({"type":"close"})).await?;
    let closed = act(&mut page, manager, &login.token, json!({"type":"do_close"})).await?;
    has(&closed, "January 2026 is closed.");
    has(&closed, "Close February 2026");
    has(&closed, ">Close February<");
    assert!(
        closed.query.contains_key("period"),
        "the address names February"
    );

    let several = act(
        &mut page,
        manager,
        &login.token,
        json!({"type":"close_several"}),
    )
    .await?;
    has(&several, "close-months-modal");
    has(&several, "Last month to close");
    let chosen = act(
        &mut page,
        manager,
        &login.token,
        json!({"type":"several_change","several[through]": april.to_string()}),
    )
    .await?;
    has(&chosen, "February to April 2026 close together.");
    has(&chosen, ">Close 3 months<");
    let done = act(
        &mut page,
        manager,
        &login.token,
        json!({"type":"do_close_several"}),
    )
    .await?;
    has(&done, "February to April 2026 are closed.");
    has(&done, "Close May 2026");
    lacks(&done, "close-months-modal");

    // A closed month still shows what it left behind, and says it is closed.
    let january = read(json!({"company_id": company, "month": "2026-01"})).await?;
    assert_eq!(
        january["close"]["blocked"],
        "January 2026 is already closed."
    );
    println!(
        "Month close proof: one contract lists every module's open items with a link each; the earliest open month may close and December waits for the year; the page closes January and lands on February, then closes February to April together and lands on May"
    );
    Ok(())
}
