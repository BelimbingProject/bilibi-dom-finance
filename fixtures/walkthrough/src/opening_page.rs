//! The opening balances page, driven through its real `LiveState` on a book
//! with a chart and open periods and no posted entries (`ledger-ui
//! opening-proof`): a pasted trial balance is read, its columns guessed from
//! the header, the rows it cannot place left out by name, the grid filled and
//! totalled live, typed changes followed, then reviewed and submitted through
//! `finance.ledger.opening_balances.submit`. The submitter can reopen the
//! grid to correct it; once another person approves the journal, the later
//! steps open with their links.
use bilibi_database::Error;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::civil::Date;
use serde_json::{json, Value};
use std::collections::HashMap;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

/// A trial balance as a spreadsheet copies it: tabs, grouped thousands, one
/// account named by its name instead of its code, a group, an account the
/// chart lacks and the source's own total row.
const PASTED: &str = "Account\tName\tDebit\tCredit
1010\tOperating account\t12,500.00\t
1200\tEquipment\t8,000.00\t
Trade payables\t\t\t3,500.00
3000\tShare capital\t\t17,000.00
1900\tCash and bank\t1.00\t
9999\tSuspense\t\t5.00
Total\t\t20,501.00\t20,505.00
";

pub async fn proof(
    services: &Services,
    manager: &Scope,
    reviewer: &Scope,
    company: i64,
    conversion: Date,
) -> Outcome<()> {
    let ledger = services.get::<bilibi_finance_ledger::Ledger>(bilibi_finance_ledger::SERVICE)?;
    let login = services
        .get::<bilibi_core::user::UserService>("core/user.accounts")?
        .login("manager@other.example.test", crate::PASSWORD)
        .await?;
    assert!(!login.pending);
    let token = login.token.as_str();
    let ids: HashMap<String, i64> = ledger
        .list_accounts(manager, company)
        .await?
        .into_iter()
        .map(|a| (a.code, a.id))
        .collect();

    let mut page = mount(services, manager, &HashMap::new()).await?;
    let first = event(&mut page, manager, token, LiveInput::Refresh).await?;
    for expected in [
        "opening-steps",
        "Not started",
        "Opens once the balances journal is approved.",
        "Paste from a spreadsheet",
        "No balances yet",
    ] {
        has(&first, expected);
    }
    has(&first, &format!("opening-debit-{}", ids["5200"]));
    lacks(&first, &format!("opening-debit-{}", ids["1900"]));

    // The paste: columns guessed from the header, rows it cannot place named.
    let pasted = act(
        &mut page,
        manager,
        token,
        json!({"type":"paste_rows","opening_paste[text]":PASTED}),
    )
    .await?;
    for expected in [
        "opening-paste-fill",
        "What you pasted",
        "is an account group",
        "no account has this code or name",
        "4 rows with an amount",
        "3 rows are left out",
    ] {
        has(&pasted, expected);
    }
    // Filling the grid sums each account and leaves the rest as typed.
    let filled = act(&mut page, manager, token, json!({"type":"fill_grid"})).await?;
    has(&filled, "Filled 4 accounts from the paste");
    has(
        &filled,
        "Debits and credits are both 20,500.00. They match.",
    );
    has(&filled, &format!(r#"id="opening-debit-{}""#, ids["1010"]));
    lacks(&filled, "opening-paste-fill");

    // A typed change follows at once; the difference says which side is over.
    let cell = |code: &str, side: &str| format!("opening[{side}_{}]", ids[code]);
    let mut typed = json!({"type":"opening_change","opening[conversion_date]":conversion.to_string(),"opening[source_reference]":"Old system closing TB"});
    typed[cell("3000", "credit")] = json!("17,000.01");
    let over = act(&mut page, manager, token, typed.clone()).await?;
    has(&over, "The difference is 0.01 more in credits");
    has(&over, r#"id="opening-preview""#);
    typed[cell("3000", "credit")] = json!("1.234");
    let fine = act(&mut page, manager, token, typed.clone()).await?;
    has(&fine, "has more than 2 decimal places");
    typed[cell("3000", "credit")] = json!("17000");
    let even = act(&mut page, manager, token, typed).await?;
    has(&even, "They match.");

    // Review, then submit through the contract.
    let review = act(&mut page, manager, token, json!({"type":"preview_opening"})).await?;
    has(&review, "opening-review-modal");
    has(&review, "Share capital");
    let submitted = act(&mut page, manager, token, json!({"type":"submit_opening"})).await?;
    has(
        &submitted,
        "is submitted. It waits for another person to approve it.",
    );
    has(&submitted, "Waiting for approval");
    lacks(&submitted, "opening-debit-");
    has(&submitted, "opening-correct");
    let waiting = ledger
        .opening_progress(manager, company)
        .await?
        .expect("an opening journal");

    // The submitter reopens the grid, filled from the waiting journal.
    let mut correcting = mount(
        services,
        manager,
        &HashMap::from([("correct".to_owned(), waiting.journal_id.to_string())]),
    )
    .await?;
    let reopened = event(&mut correcting, manager, token, LiveInput::Refresh).await?;
    has(&reopened, "Correct the account balances");
    has(&reopened, "Submit correction");
    has(&reopened, r#"value="12500.00""#);
    assert_eq!(
        reopened.query.get("correct"),
        Some(&waiting.journal_id.to_string())
    );

    // Another person approves; the later steps open with their links.
    ledger
        .approve_journal(reviewer, company, waiting.journal_id)
        .await?;
    let posted = event(&mut page, manager, token, LiveInput::Refresh).await?;
    for expected in [
        "Posted",
        "Ready",
        "/finance/receivables/invoices?opening=1",
        "/finance/payables/bills?opening=1",
        "/finance/fixed-assets/register?opening=1",
        "The balances the opening journal posted.",
    ] {
        has(&posted, expected);
    }
    let progress = ledger
        .opening_progress(manager, company)
        .await?
        .expect("posted");
    assert!(progress.entry_id.is_some() && progress.imported.is_empty());
    println!(
        "opening page proof: pasted 7 rows, filled 4 accounts, left out 3 by name, followed typed totals, submitted journal {} through the contract, reopened it to correct, and opened steps 2-4 after approval",
        waiting.journal_number
    );
    Ok(())
}

async fn mount(
    services: &Services,
    scope: &Scope,
    query: &HashMap<String, String>,
) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/ledger.pages")?;
    Ok(pages
        .0
        .mount(PageRequest {
            operation: "opening",
            scope: Some(scope),
            path: &HashMap::new(),
            query,
            body: &[],
            framed: false,
        })
        .await?
        .expect("the opening balances page mounts"))
}

async fn event(
    state: &mut State,
    scope: &Scope,
    credential: &str,
    input: LiveInput<'_>,
) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential,
            input,
        })
        .await?)
}

async fn act(
    state: &mut State,
    scope: &Scope,
    credential: &str,
    value: Value,
) -> Outcome<LiveBody> {
    event(state, scope, credential, LiveInput::Event(&value)).await
}

fn has(body: &LiveBody, expected: &str) {
    if !body.html.contains(expected) {
        let path = std::env::temp_dir().join("opening-page-proof.html");
        let _ = std::fs::write(&path, &body.html);
        panic!("page missing {expected} (page in {})", path.display());
    }
}

fn lacks(body: &LiveBody, unexpected: &str) {
    assert!(!body.html.contains(unexpected), "page shows {unexpected}");
}
