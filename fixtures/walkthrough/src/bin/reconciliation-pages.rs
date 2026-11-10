//! NC4's disposable proof of the matching and reconciliation pages. Run on an
//! empty database with DATABASE_URL, APP_ORIGIN, BIND_ADDR and
//! NC4_STORAGE_ROOT set. It drives the mounted pages through their own events
//! (matching a selection, accepting a suggestion, unmatching with a reason,
//! posting a charge and interest, finalising by someone other than the
//! opener, abandoning, the links that record a statement line as a receipt or a
//! payment and the match made on coming back, finalising with a reason while
//! lines are not in the books) and their posting-notice subscriptions, then serves the
//! app with a second bank account left unmatched and open for the browser.
//! The fixture owns its records and storage; no product seed or route is
//! added.
use bilibi_database::{pool, Db, Error};
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::{
    AccountType, Ledger, Money, NewAccount, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_reconciliation::{
    BankAccount, NewBankAccount, NewReconciliation, PageOptions, Reconciliation,
};
use bilibi_finance_walkthrough_fixture::{
    attachments::configure_storage, install, sign_in, COMPANY,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::civil::{date, Date};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

async fn mount(
    pages: &PageService<Scope, Error>,
    scope: &Scope,
    operation: &'static str,
    id: i64,
) -> Outcome<(State, LiveBody)> {
    mount_at(pages, scope, operation, id, HashMap::new()).await
}

/// The page as a person lands on it from an address with a query.
async fn mount_at(
    pages: &PageService<Scope, Error>,
    scope: &Scope,
    operation: &'static str,
    id: i64,
    query: HashMap<String, String>,
) -> Outcome<(State, LiveBody)> {
    let path = HashMap::from([("id".into(), id.to_string())]);
    let mut state = pages
        .0
        .mount(PageRequest {
            operation,
            scope: Some(scope),
            path: &path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .ok_or(Error::Internal)?;
    let body = refresh(&mut state, scope).await?;
    Ok((state, body))
}

async fn refresh(state: &mut State, scope: &Scope) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?)
}

async fn event(state: &mut State, scope: &Scope, value: Value) -> Outcome<String> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Event(&value),
        })
        .await?
        .html)
}

/// Fails the proof unless `html` holds every expected fragment.
fn shows(step: &str, html: &str, expected: &[&str]) -> Outcome<()> {
    for text in expected {
        if !html.contains(text) {
            return Err(format!("{step}: the page lacks {text:?}").into());
        }
    }
    println!("{step}: shows {}", expected.join(" | "));
    Ok(())
}

fn lacks(step: &str, html: &str, absent: &[&str]) -> Outcome<()> {
    for text in absent {
        if html.contains(text) {
            return Err(format!("{step}: the page still shows {text:?}").into());
        }
    }
    println!("{step}: withholds {}", absent.join(" | "));
    Ok(())
}

/// Whether the page's Finalise button is rendered disabled.
fn finalise_disabled(html: &str) -> bool {
    html.split("<button")
        .find(|tag| tag.contains("id=\"reconciliation-finalise\""))
        .and_then(|tag| tag.split('>').next())
        .is_some_and(|tag| tag.split_whitespace().any(|word| word == "disabled"))
}

/// One wake from a page's posting subscription, within five seconds.
async fn woken(
    state: &mut State,
    step: &str,
    post: impl std::future::Future<Output = Outcome<()>>,
) -> Outcome<()> {
    let mut subscriptions = state.subscriptions();
    let receiver = &mut subscriptions
        .first_mut()
        .ok_or("no posting subscription")?
        .receiver;
    post.await?;
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .map_err(|_| format!("{step}: no posting notice within 5 seconds"))??;
    println!("{step}: the page was woken by the posting notice");
    Ok(())
}

struct Setup {
    account: BankAccount,
    entries: Vec<i64>,
}

/// A bank account on its own ledger account, five entries on it and one
/// imported statement: a receipt and a payment the statement shows a day
/// later, two counter receipts the statement shows as one batch, a cheque it
/// does not show yet, and a bank fee and interest the books lack.
async fn setup(
    services: &Services,
    manager: &Scope,
    code: &str,
    name: &str,
    file: &str,
) -> Outcome<Setup> {
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let facade = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let units = ledger.get_book(manager, COMPANY).await?.minor_units;
    let money = |text: &str| Money::parse(text, units).expect("fixture amount");
    let mut chart = ledger.list_accounts(manager, COMPANY).await?;
    for (code, name, kind) in [
        (code, name, AccountType::Asset),
        ("7000", "Interest income", AccountType::Income),
    ] {
        if chart.iter().all(|a| a.code != code) {
            ledger
                .create_account(manager, COMPANY, &NewAccount::new(code, name, kind))
                .await?;
        }
    }
    chart = ledger.list_accounts(manager, COMPANY).await?;
    let id = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or("fixture account")
    };
    let (bank, receivables, payables) = (id(code)?, id("1100")?, id("2000")?);
    let account = facade
        .create_bank_account(
            manager,
            COMPANY,
            &NewBankAccount {
                name: name.into(),
                ledger_account_id: bank,
                import_mapping: json!({"date":"Date","description":"Details","reference":"Ref","amount":"Amount"}),
                charges_account_id: Some(id("6000")?),
                interest_account_id: Some(id("7000")?),
                match_window_days: None,
                active: None,
                reason: None,
            },
        )
        .await?;
    let post =
        |day: Date, description: &str, reference: &str, debit: i64, credit: i64, amount| NewEntry {
            entry_date: day,
            description: description.into(),
            reference: Some(reference.into()),
            lines: vec![
                NewLine::debit(debit, money(amount)),
                NewLine::credit(credit, money(amount)),
            ],
        };
    let mut entries = vec![];
    for new in [
        post(
            date(2026, 10, 1),
            "Customer receipt INV-301",
            "INV-301",
            bank,
            receivables,
            "300.00",
        ),
        post(
            date(2026, 10, 5),
            "Supplier payment",
            "PAY-31",
            payables,
            bank,
            "120.00",
        ),
        post(
            date(2026, 10, 10),
            "Counter receipt A",
            "BATCH-3",
            bank,
            receivables,
            "50.00",
        ),
        post(
            date(2026, 10, 11),
            "Counter receipt B",
            "BATCH-3",
            bank,
            receivables,
            "25.00",
        ),
        post(
            date(2026, 10, 26),
            "Cheque 3001",
            "CHQ-3001",
            payables,
            bank,
            "80.00",
        ),
    ] {
        entries.push(
            ledger
                .post_entry(manager, COMPANY, &new, &PostOptions::default())
                .await?
                .id,
        );
    }
    // The fee names the ledger account: a company refuses the same file twice.
    let csv = format!(
        "Date,Details,Ref,Amount\n\
         2026-10-02,Receipt INV-301,INV-301,300.00\n\
         2026-10-06,Payment PAY-31,PAY-31,-120.00\n\
         2026-10-12,Counter batch,BATCH-3,75.00\n\
         2026-10-28,Monthly fee {code},FEE-10,-7.50\n\
         2026-10-30,Credit interest,INT-10,1.25\n"
    );
    facade
        .import_statement(
            manager,
            COMPANY,
            account.id,
            &NewFile {
                file_name: file.into(),
                content: csv.as_bytes().to_vec(),
                content_type: Some("text/csv".into()),
            },
        )
        .await?;
    Ok(Setup { account, entries })
}

/// The statement balance the October file ends at: 300 - 120 + 75 - 7.50 + 1.25.
const OCTOBER_BALANCE: &str = "248.75";

async fn line_id(
    facade: &Reconciliation,
    scope: &Scope,
    account: i64,
    reference: &str,
) -> Outcome<i64> {
    Ok(facade
        .list_statement_lines(scope, COMPANY, account, None)
        .await?
        .into_iter()
        .find(|l| l.line.reference.as_deref() == Some(reference))
        .ok_or("fixture statement line")?
        .line
        .id)
}

async fn affiliate(db: &Db) -> Outcome<()> {
    // Same disposable affiliation facts as the NC3 walkthrough.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Viewer','human','lee@example.test'),(5,1,'person_5','Sam Staff','human','sam@example.test') ON CONFLICT DO NOTHING;
        INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','viewer@example.test'),(4,1,5,1,'Clerk','staff@example.test') ON CONFLICT DO NOTHING;
        UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
        UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=3 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
        SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1));
        SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1))").await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    let bind = std::env::var("BIND_ADDR")?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    affiliate(&db).await?;
    let root = std::env::var("NC4_STORAGE_ROOT")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let admin = sign_in(&db, services, 1, 4).await?;
    configure_storage(&db, services, &admin, &root).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    // A clerk affiliated with the company who may finalise, so a reconciliation
    // the manager opened has someone else to finalise it.
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    for capability in [
        "finance.ledger.view",
        bilibi_finance_reconciliation::FINALISE_RECONCILIATIONS,
    ] {
        grants.set_direct(&admin, 3, capability, Some(true)).await?;
    }
    let staff = sign_in(&db, services, 1, 3).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let facade = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let pages = services.get::<PageService<Scope, Error>>("finance/reconciliation.pages")?;
    let units = ledger.get_book(&manager, COMPANY).await?.minor_units;

    println!("\n== Matching page");
    let Setup { account, entries } = setup(
        services,
        &manager,
        "1850",
        "Clearing bank",
        "october-clearing.csv",
    )
    .await?;
    let (mut matching, body) = mount(&pages, &manager, "matching", account.id).await?;
    shows(
        "manager opens matching",
        &body.html,
        &[
            "Clearing bank: matching",
            "Unmatched statement lines",
            "Unmatched ledger entries",
            "id=\"unmatched-lines-table\"",
            "id=\"unmatched-entries-table\"",
            "id=\"standing-matches-table\"",
            "Customer receipt INV-301</a>",
            "1 Oct 2026",
            "Match each bank line to what is in the books.",
            "-120.00",
            "Selected: <span id=\"selected-lines-count\">0</span>",
            "No matches yet",
        ],
    )?;
    let receipt = line_id(&facade, &manager, account.id, "INV-301").await?;
    let batch = line_id(&facade, &manager, account.id, "BATCH-3").await?;
    let html = event(
        &mut matching,
        &manager,
        json!({"type":"accept","line":receipt.to_string()}),
    )
    .await?;
    shows(
        "manager accepts the receipt's suggestion",
        &html,
        &[
            "Match 1 made: 1 statement line with 1 entry.",
            "id=\"match-",
        ],
    )?;
    lacks(
        "the accepted line",
        &html,
        &[&format!("id=\"line-{receipt}-select\"")],
    )?;
    let html = event(
        &mut matching,
        &manager,
        json!({"type":"accept","line":receipt.to_string()}),
    )
    .await?;
    shows(
        "accepting it again",
        &html,
        &["That suggestion is no longer available."],
    )?;
    let selection = json!({"type":"select","lines[]":[batch.to_string()],"entries[]":[entries[2].to_string(),entries[3].to_string()],"note":"Counter batch"});
    let html = event(&mut matching, &manager, selection.clone()).await?;
    shows(
        "manager ticks the batch line and both counter receipts",
        &html,
        &[
            "<span id=\"selected-lines-count\">1</span>",
            "<span id=\"selected-lines-sum\" class=\"tabular-nums\">75.00</span>",
            "<span id=\"selected-entries-count\">2</span>",
            "<span id=\"selected-entries-sum\" class=\"tabular-nums\">75.00</span>",
            &format!("id=\"line-{batch}-select\" checked"),
            "value=\"Counter batch\"",
        ],
    )?;
    let unbalanced = json!({"type":"match","lines[]":[batch.to_string()],"entries[]":[entries[2].to_string()],"note":""});
    let html = event(&mut matching, &manager, unbalanced).await?;
    shows(
        "matching 75.00 with 50.00",
        &html,
        &["The amounts differ: 75 on the statement, 50 in the books."],
    )?;
    let mut matched = selection.clone();
    matched["type"] = json!("match");
    let html = event(&mut matching, &manager, matched).await?;
    shows(
        "manager matches the selection",
        &html,
        &[
            "Match 2 made: 1 statement line with 2 entries.",
            "<span id=\"selected-lines-count\">0</span>",
        ],
    )?;
    let standing = facade
        .list_matches(&manager, COMPANY, account.id, &PageOptions::default())
        .await?;
    let batch_match = standing
        .iter()
        .find(|m| m.match_number == 2)
        .ok_or("match 2")?
        .id;
    let html = event(
        &mut matching,
        &manager,
        json!({"type":"unmatching","id":batch_match.to_string()}),
    )
    .await?;
    shows(
        "manager asks to unmatch match 2",
        &html,
        &[
            "id=\"unmatch-modal\"",
            "Unmatch match 2",
            "the match and this reason stay on record",
        ],
    )?;
    let html = event(
        &mut matching,
        &manager,
        json!({"type":"unmatch","unmatch[reason]":""}),
    )
    .await?;
    shows(
        "unmatching without a reason",
        &html,
        &["Refused: reason", "id=\"unmatch-modal\""],
    )?;
    let html = event(
        &mut matching,
        &manager,
        json!({"type":"unmatch","unmatch[reason]":"Wrong batch"}),
    )
    .await?;
    shows(
        "manager unmatches with a reason",
        &html,
        &["Match 2 unmatched; its lines and entries are free again."],
    )?;
    lacks("after unmatching", &html, &["id=\"unmatch-modal\""])?;
    let mut matched = selection;
    matched["type"] = json!("match");
    let html = event(&mut matching, &manager, matched).await?;
    shows(
        "manager matches the batch again",
        &html,
        &["Match 3 made: 1 statement line with 2 entries."],
    )?;

    let (mut read_only, body) = mount(&pages, &viewer, "matching", account.id).await?;
    shows(
        "viewer opens matching",
        &body.html,
        &["id=\"matching-forbidden\""],
    )?;
    lacks(
        "viewer's matching page",
        &body.html,
        &[
            "type=\"checkbox\"",
            "id=\"match-selected\"",
            "-accept\"",
            "-unmatch\"",
        ],
    )?;
    let forged = json!({"type":"match","lines[]":[line_id(&facade, &manager, account.id, "PAY-31").await?.to_string()],"entries[]":[entries[1].to_string()]});
    let html = event(&mut read_only, &viewer, forged).await?;
    shows(
        "viewer forges a match",
        &html,
        &["You do not have permission to do that."],
    )?;
    assert_eq!(
        facade
            .list_matches(&manager, COMPANY, account.id, &PageOptions::default())
            .await?
            .len(),
        3,
        "the forged match recorded nothing"
    );

    let manual = NewEntry {
        entry_date: date(2026, 10, 29),
        description: "Transfer in".into(),
        reference: Some("TRF-1".into()),
        lines: vec![
            NewLine::debit(account.ledger_account_id, Money::parse("10.00", units)?),
            NewLine::credit(
                ledger
                    .list_accounts(&manager, COMPANY)
                    .await?
                    .iter()
                    .find(|a| a.code == "3000")
                    .ok_or("share capital")?
                    .id,
                Money::parse("10.00", units)?,
            ),
        ],
    };
    let mut transfer = None;
    woken(
        &mut matching,
        "a manual entry posts to the bank account",
        async {
            transfer = Some(
                ledger
                    .post_entry(&manager, COMPANY, &manual, &PostOptions::default())
                    .await?,
            );
            Ok(())
        },
    )
    .await?;
    let transfer = transfer.ok_or("transfer entry")?;
    let body = refresh(&mut matching, &manager).await?;
    shows(
        "the matching page reads again",
        &body.html,
        &["Transfer in", "TRF-1"],
    )?;

    println!("\n== Reconciliation page");
    let opened = facade
        .open_reconciliation(
            &manager,
            COMPANY,
            account.id,
            &NewReconciliation {
                end_date: Some(date(2026, 10, 31)),
                statement_balance: OCTOBER_BALANCE.into(),
            },
        )
        .await?;
    let (mut page, body) = mount(&pages, &manager, "reconciliation", opened.id).await?;
    shows(
        "manager opens reconciliation 1",
        &body.html,
        &[
            "Reconciliation 1 as of 31 Oct 2026",
            "Balance per statement",
            "248.75 MYR",
            "Balance per books",
            "Difference",
            "3 statement lines not in the books",
            "Not matched yet",
            "On the statement, not in the books",
            "In the books, not on the statement",
            "Post as bank charge",
            "Post as interest",
            "id=\"reconciliation-finalise\"",
            "Match statement lines",
        ],
    )?;
    if finalise_disabled(&body.html) {
        return Err("the finalise button is disabled with no difference".into());
    }
    let fee = line_id(&facade, &manager, account.id, "FEE-10").await?;
    let interest = line_id(&facade, &manager, account.id, "INT-10").await?;
    let html = event(
        &mut page,
        &manager,
        json!({"type":"post_interest","line":fee.to_string()}),
    )
    .await?;
    shows(
        "posting the fee as interest",
        &html,
        &["Money out cannot be posted as interest."],
    )?;
    let html = event(
        &mut page,
        &manager,
        json!({"type":"post_charge","line":fee.to_string()}),
    )
    .await?;
    shows(
        "manager posts the fee as a bank charge",
        &html,
        &["and matched."],
    )?;
    lacks(
        "after the charge",
        &html,
        &[&format!("id=\"item-{fee}-post-charge\"")],
    )?;

    // Another session watching the same reconciliation follows the posting.
    let (mut watcher, body) = mount(&pages, &staff, "reconciliation", opened.id).await?;
    shows("a clerk watches it", &body.html, &["Credit interest"])?;
    lacks(
        "the clerk's page",
        &body.html,
        &["Post as interest", "id=\"reconciliation-abandon\""],
    )?;
    woken(
        &mut watcher,
        "the manager posts the interest from another session",
        async {
            event(
                &mut page,
                &manager,
                json!({"type":"post_interest","line":interest.to_string()}),
            )
            .await?;
            Ok(())
        },
    )
    .await?;
    let body = refresh(&mut watcher, &staff).await?;
    shows(
        "the watching page reads again",
        &body.html,
        &["Transfer in"],
    )?;
    lacks(
        "the watching page",
        &body.html,
        &[
            "Credit interest",
            &format!("id=\"item-{interest}-post-interest\""),
        ],
    )?;

    // The transfer is not on the statement; take it out of the difference by
    // reversing it, as a bookkeeper would correct a mistaken journal.
    ledger
        .reverse_entry(
            &manager,
            COMPANY,
            transfer.id,
            &bilibi_finance_ledger::Reversal {
                entry_date: date(2026, 10, 29),
                description: "Not a bank movement".into(),
                reference: None,
            },
        )
        .await?;
    let body = refresh(&mut page, &manager).await?;
    shows(
        "with the books explained",
        &body.html,
        &["1 statement line not in the books", "Cheque 3001"],
    )?;
    let html = event(&mut page, &manager, json!({"type":"finalise"})).await?;
    shows(
        "the manager who opened it finalises",
        &html,
        &["Someone else must finalise this because the company requires separation of duties"],
    )?;
    // The payment line was never matched: the clerk is asked why first.
    let html = event(&mut watcher, &staff, json!({"type":"finalising"})).await?;
    shows(
        "the clerk presses Finalise with a line left",
        &html,
        &["Finalise with 1 statement line not in the books?"],
    )?;
    let html = event(
        &mut watcher,
        &staff,
        json!({"type":"finalise","finalise[reason]":"The payment clears in November"}),
    )
    .await?;
    shows(
        "the clerk finalises",
        &html,
        &[
            "Reconciliation 1 finalised; its figures are now fixed.",
            "finalised",
            "Finalised",
        ],
    )?;
    lacks(
        "the finalised reconciliation",
        &html,
        &[
            "id=\"reconciliation-finalise\"",
            "id=\"reconciliation-abandon\"",
            "Post as bank charge",
        ],
    )?;
    let html = event(
        &mut page,
        &manager,
        json!({"type":"abandon","abandon[reason]":"Too late"}),
    )
    .await?;
    shows(
        "abandoning the finalised one",
        &html,
        &["The reconciliation is finalised and cannot change."],
    )?;

    let (_, body) = mount(&pages, &viewer, "reconciliation", opened.id).await?;
    shows(
        "viewer opens it",
        &body.html,
        &["Reconciliation 1 as of 31 Oct 2026"],
    )?;

    println!("\n== Abandoning");
    let Setup {
        account: reserve, ..
    } = setup(
        services,
        &manager,
        "1860",
        "Reserve bank",
        "october-reserve.csv",
    )
    .await?;
    let reserve_open = facade
        .open_reconciliation(
            &manager,
            COMPANY,
            reserve.id,
            &NewReconciliation {
                end_date: Some(date(2026, 10, 31)),
                statement_balance: "1.00".into(),
            },
        )
        .await?;
    let (mut reserve_page, body) =
        mount(&pages, &viewer, "reconciliation", reserve_open.id).await?;
    shows(
        "viewer opens an open reconciliation",
        &body.html,
        &["id=\"reconciliation-finalise-forbidden\""],
    )?;
    lacks(
        "viewer's reconciliation page",
        &body.html,
        &["id=\"reconciliation-abandon\"", "Post as bank charge"],
    )?;
    let html = event(&mut reserve_page, &viewer, json!({"type":"finalise"})).await?;
    shows(
        "viewer forges finalise",
        &html,
        &["You do not have permission to do that."],
    )?;
    let (mut reserve_page, body) =
        mount(&pages, &manager, "reconciliation", reserve_open.id).await?;
    shows(
        "manager opens it, the statement balance mistyped",
        &body.html,
        &["Not explained yet", "id=\"reconciliation-abandon\""],
    )?;
    if !finalise_disabled(&body.html) {
        return Err("the finalise button is enabled with a difference".into());
    }
    println!("manager opens it: Finalise is disabled while the difference is not zero");
    let html = event(&mut reserve_page, &manager, json!({"type":"abandoning"})).await?;
    shows(
        "manager asks to abandon",
        &html,
        &["id=\"abandon-modal\"", "Abandon this reconciliation"],
    )?;
    let html = event(
        &mut reserve_page,
        &manager,
        json!({"type":"abandon","abandon[reason]":"Mistyped balance"}),
    )
    .await?;
    shows(
        "manager abandons with a reason",
        &html,
        &[
            "The reconciliation was abandoned.",
            "abandoned",
            "Mistyped balance",
        ],
    )?;
    lacks(
        "the abandoned reconciliation",
        &html,
        &["Balance per books", "id=\"reconciliation-items\""],
    )?;
    let html = event(
        &mut reserve_page,
        &manager,
        json!({"type":"post_charge","line":"999999"}),
    )
    .await?;
    shows(
        "posting a missing line",
        &html,
        &["The statement line no longer exists."],
    )?;

    println!("\n== Recording a statement line elsewhere, and finalising with lines left");
    let Setup {
        account: savings, ..
    } = setup(
        services,
        &manager,
        "1890",
        "Savings bank",
        "october-savings.csv",
    )
    .await?;
    let savings_open = facade
        .open_reconciliation(
            &manager,
            COMPANY,
            savings.id,
            &NewReconciliation {
                end_date: Some(date(2026, 10, 31)),
                statement_balance: OCTOBER_BALANCE.into(),
            },
        )
        .await?;
    let (_, body) = mount(&pages, &manager, "reconciliation", savings_open.id).await?;
    lacks(
        "a manager who may not record receipts or request payments",
        &body.html,
        &["Record receipt", "Pay a bill"],
    )?;
    for capability in [
        "finance.receivables.payments.post",
        "finance.payables.payments.submit",
    ] {
        grants.set_direct(&admin, 1, capability, Some(true)).await?;
    }
    let (_, body) = mount(&pages, &manager, "reconciliation", savings_open.id).await?;
    let back = format!(
        "return_to=%2Ffinance%2Freconciliation%2Freconciliations%2F{}%3Fline%3D",
        savings_open.id
    );
    shows(
        "nothing matched: the difference is zero, and the page says what is missing",
        &body.html,
        &[
            "5 statement lines not in the books",
            "Record receipt",
            "/finance/receivables/receipts?",
            "seen_amount=1.25",
            "seen_date=2026-10-30",
            "seen_text=Credit%20interest",
            "Pay a bill",
            "/finance/payables/payments/new?",
            "seen_amount=7.50",
            &back,
        ],
    )?;
    lacks("an amount out as a receipt", &body.html, &["seen_amount=-"])?;
    let (_, body) = mount(&pages, &staff, "reconciliation", savings_open.id).await?;
    lacks(
        "the clerk's page",
        &body.html,
        &["Record receipt", "Pay a bill"],
    )?;

    // Coming back from recording a line names the line and the entry posted.
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let id = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or("fixture account")
    };
    let recorded = |day: Date, text: &str, amount: &str| NewEntry {
        entry_date: day,
        description: text.into(),
        reference: None,
        lines: vec![
            NewLine::debit(
                id("1890").expect("bank"),
                Money::parse(amount, units).expect("amount"),
            ),
            NewLine::credit(
                id("1100").expect("receivables"),
                Money::parse(amount, units).expect("amount"),
            ),
        ],
    };
    let interest = line_id(&facade, &manager, savings.id, "INT-10").await?;
    let receipt = line_id(&facade, &manager, savings.id, "INV-301").await?;
    let batch = line_id(&facade, &manager, savings.id, "BATCH-3").await?;
    let returned = |line: i64, entry: i64| {
        HashMap::from([
            ("line".to_owned(), line.to_string()),
            ("entry".to_owned(), entry.to_string()),
        ])
    };
    let mine = ledger
        .post_entry(
            &manager,
            COMPANY,
            &recorded(date(2026, 10, 30), "Receipt for the interest line", "1.25"),
            &PostOptions::default(),
        )
        .await?;
    let (_, body) = mount_at(
        &pages,
        &manager,
        "reconciliation",
        savings_open.id,
        returned(interest, mine.id),
    )
    .await?;
    shows(
        "the manager comes back from recording the interest line",
        &body.html,
        &[
            "Recorded and matched to the statement line.",
            "4 statement lines not in the books",
        ],
    )?;
    let (_, body) = mount_at(
        &pages,
        &manager,
        "reconciliation",
        savings_open.id,
        returned(interest, mine.id),
    )
    .await?;
    lacks(
        "opening that address again",
        &body.html,
        &["Recorded and matched", "Recorded, but not matched"],
    )?;
    let differs = ledger
        .post_entry(
            &manager,
            COMPANY,
            &recorded(date(2026, 10, 12), "Receipt of another amount", "70.00"),
            &PostOptions::default(),
        )
        .await?;
    let (_, body) = mount_at(
        &pages,
        &manager,
        "reconciliation",
        savings_open.id,
        returned(batch, differs.id),
    )
    .await?;
    shows(
        "coming back with an entry of another amount",
        &body.html,
        &[
            "Recorded, but not matched to the statement line. The amounts differ: 75 on the statement, 70 in the books.",
            "4 statement lines not in the books",
        ],
    )?;
    // An address alone never matches an entry someone else posted.
    let theirs = ledger
        .post_entry(
            &admin,
            COMPANY,
            &recorded(date(2026, 10, 2), "Receipt someone else posted", "300.00"),
            &PostOptions::default(),
        )
        .await?;
    let (_, body) = mount_at(
        &pages,
        &manager,
        "reconciliation",
        savings_open.id,
        returned(receipt, theirs.id),
    )
    .await?;
    shows(
        "the manager opens an address naming an entry the administrator posted",
        &body.html,
        &["4 statement lines not in the books"],
    )?;
    lacks("that address", &body.html, &["Recorded and matched"])?;

    // Finalising while lines are not in the books: warned, asked why, never blocked.
    let refused = facade
        .finalise_reconciliation(&staff, COMPANY, savings_open.id, None)
        .await;
    match refused {
        Err(bilibi_finance_reconciliation::ReconciliationError::LinesNotInBooks(4)) => {
            println!("finalising without a reason: refused, 4 lines are not in the books")
        }
        other => return Err(format!("finalising without a reason: {other:?}").into()),
    }
    let (mut clerk, body) = mount(&pages, &staff, "reconciliation", savings_open.id).await?;
    if finalise_disabled(&body.html) {
        return Err("lines not in the books disabled Finalise".into());
    }
    let html = event(&mut clerk, &staff, json!({"type":"finalising"})).await?;
    shows(
        "the clerk presses Finalise",
        &html,
        &[
            "id=\"finalise-modal\"",
            "Finalise with 4 statement lines not in the books?",
            "4 lines are on the bank statement but not in the books. You can still finalise.",
            "id=\"finalise-reason\"",
            "id=\"finalise-cancel\"",
        ],
    )?;
    let html = event(
        &mut clerk,
        &staff,
        json!({"type":"finalise","finalise[reason]":"  "}),
    )
    .await?;
    shows(
        "finalising with a blank reason",
        &html,
        &[
            "id=\"finalise-refusal\"",
            "4 statement lines are not in the books. Say why to finalise without them.",
        ],
    )?;
    let html = event(&mut clerk, &staff, json!({"type":"cancel_finalise"})).await?;
    lacks("after Cancel", &html, &["id=\"finalise-modal\""])?;
    let html = event(&mut clerk, &staff, json!({"type":"finalise"})).await?;
    shows(
        "a finalise sent without the dialog",
        &html,
        &["id=\"finalise-modal\""],
    )?;
    let html = event(
        &mut clerk,
        &staff,
        json!({"type":"finalise","finalise[reason]":"Receipts banked on 2 November"}),
    )
    .await?;
    shows(
        "the clerk finalises with a reason",
        &html,
        &[
            "Reconciliation 3 finalised; its figures are now fixed.",
            "Receipts banked on 2 November",
            "4 statement lines not in the books",
        ],
    )?;
    lacks(
        "the finalised reconciliation",
        &html,
        &["id=\"finalise-modal\"", "Record receipt", "Pay a bill"],
    )?;

    println!("\n== Left for the browser");
    let Setup {
        account: browse, ..
    } = setup(
        services,
        &manager,
        "1870",
        "Operating bank",
        "october-operating.csv",
    )
    .await?;
    let browse_open = facade
        .open_reconciliation(
            &manager,
            COMPANY,
            browse.id,
            &NewReconciliation {
                end_date: Some(date(2026, 10, 31)),
                statement_balance: OCTOBER_BALANCE.into(),
            },
        )
        .await?;
    println!("NC4 proof passed: matching, suggestions, unmatching, refusals, withheld controls, posting notices, charges and interest, finalising by a non-opener, abandoning, recording a line elsewhere and finalising with lines left.");
    println!(
        "Matching page: {origin}/finance/reconciliation/accounts/{}/matching",
        browse.id
    );
    println!(
        "Reconciliation page: {origin}/finance/reconciliation/reconciliations/{}",
        browse_open.id
    );
    println!(
        "Finalised reconciliation: {origin}/finance/reconciliation/reconciliations/{}",
        opened.id
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
