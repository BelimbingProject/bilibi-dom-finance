//! Browser fixture for the ledger pages (books, chart of accounts, journal
//! entries, manual journals, fiscal years, trial balance). Run with
//! DATABASE_URL, APP_ORIGIN, BIND_ADDR and BILIBI_SETTINGS_KEY against a
//! disposable database. Identity fixture SQL only; every ledger record is
//! created by the ledger's facade, as the pages and agents create them.
//!
//! Sign in as `manager@other.example.test` (every ledger capability, in Company
//! 100), `reviewer@other.example.test` (the administrator, who approves the
//! manager's journals) or `viewer@other.example.test` (view only), with the
//! walkthrough password. Pass `empty` to stop before the book is opened, or
//! `bare` to open it with an empty chart (the starter charts are offered).
//! Run `bilibi-web migrate` and `seed-production` first for Core's currency
//! list. `empty` and `book-proof` also clear tenant 2's used currencies, so the
//! Book page offers every current one with none used yet; `book-proof` then
//! opens it through the Book page's own `LiveState` and stops with the `bare`
//! state.
//! Pass `opening` for the chart and open periods with no posted entries, or
//! `opening-proof` to drive the opening balances page through it as well.
//! After seeding it drives the Journals page's period control through
//! its real `LiveState` and prints one proof line.
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{
    AccountChange, AccountType, Ledger, Money, NewAccount, NewAccountGroup, NewBook, NewEntry,
    NewFiscalYear, NewLine, PostOptions,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::civil::{date, Date};
use serde_json::json;
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();

const COMPANY: i64 = 100;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Alex Admin','human','admin@example.test'),(5,1,'person_5','Lee Viewer','human','viewer@example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','admin@example.test'),(4,1,5,1,'Reader','viewer@example.test') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))"
    ).await?;
    // Tenant 2 is an ordinary tenant: a manager, an administrator who reviews,
    // and a reader. None of them is a platform operator.
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(6,2,'person_6','Other Manager','human','manager@other.example.test'),(7,2,'person_7','Other Reviewer','human','reviewer@other.example.test'),(8,2,'person_8','Other Reader','human','viewer@other.example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(5,2,6,100,'Recorder'),(6,2,7,100,'Reviewer'),(7,2,8,100,'Reader') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=6 WHERE tenant_id=2 AND id=5 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=5 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','reviewer@other.example.test',i.password_hash,2,8,2 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=5 ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 8,2,'Other Reviewer',id,7 FROM user_identities WHERE home_tenant_id=2 AND home_user_id=8 ON CONFLICT DO NOTHING; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=8 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','viewer@other.example.test',i.password_hash,2,9,2 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=5 ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 9,2,'Other Reader',id,8 FROM user_identities WHERE home_tenant_id=2 AND home_user_id=9 ON CONFLICT DO NOTHING; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=9 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT 2,8,id FROM roles WHERE tenant_id=2 AND code='admin' ON CONFLICT DO NOTHING;
         INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT 2,9,id FROM roles WHERE tenant_id=2 AND code='viewer' ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations));
         SELECT setval(pg_get_serial_sequence('users','id'),(SELECT max(id) FROM users));
         SELECT setval(pg_get_serial_sequence('user_identities','id'),(SELECT max(id) FROM user_identities))"
    ).await?;
    let manager = sign_in(&db, services, 2, 5).await?;
    let reviewer = sign_in(&db, services, 2, 8).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    bilibi_finance_walkthrough_fixture::initialize_journal_approvals(
        &db, services, &manager, COMPANY,
    )
    .await?;

    let unopened = matches!(
        ledger.get_book(&manager, COMPANY).await,
        Err(bilibi_finance_ledger::LedgerError::BookNotOpen)
    );
    let book_proof = std::env::args().any(|arg| arg == "book-proof");
    if unopened && (book_proof || std::env::args().any(|arg| arg == "empty")) {
        // Disposable identity-level fixture state: the tenant has the ISO list
        // but has used no currency yet.
        db.get()
            .await?
            .execute("DELETE FROM currency_tenant_used WHERE tenant_id=2", &[])
            .await?;
    }
    if unopened && book_proof {
        book_page_proof(&db, services, &manager).await?;
    } else if unopened && std::env::args().any(|arg| arg == "bare") {
        // An adopter who has opened the book and nothing else: the chart is
        // empty, so the starter charts are offered.
        ledger
            .open_book(
                &manager,
                COMPANY,
                &NewBook {
                    currency_code: "MYR".into(),
                    minor_units: 2,
                },
            )
            .await?;
    } else if unopened && !std::env::args().any(|arg| arg == "empty") {
        let (account, pin) = seed(&ledger, &manager, &reviewer).await?;
        if std::env::args().any(|arg| arg == "opening-proof") {
            let first = date(jiff::Zoned::now().year() - 1, 1, 1);
            bilibi_finance_walkthrough_fixture::opening_page::proof(
                services, &manager, &reviewer, COMPANY, first,
            )
            .await?;
        } else if !std::env::args().any(|arg| arg == "opening") {
            period_proof(services, &manager, account, pin).await?;
        }
    }
    println!(
        "Ledger fixture at {origin}: manager@other.example.test, reviewer@other.example.test, viewer@other.example.test; walkthrough password"
    );
    axum::serve(
        tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
        bilibi_web::router(app),
    )
    .await?;
    Ok(())
}

/// A small company's two years: a chart, entries through the months, closed
/// periods, a closed year, and manual journals in each state.
async fn seed(
    ledger: &Ledger,
    manager: &bilibi_tenancy::Scope,
    reviewer: &bilibi_tenancy::Scope,
) -> Outcome<(i64, i64)> {
    let book = ledger
        .open_book(
            manager,
            COMPANY,
            &NewBook {
                currency_code: "MYR".into(),
                minor_units: 2,
            },
        )
        .await?;
    let units = book.minor_units;
    let mut ids = std::collections::HashMap::new();
    let chart: [(&str, &str, AccountType, bool); 14] = [
        ("1000", "Cash at bank", AccountType::Asset, false),
        ("1100", "Trade receivables", AccountType::Asset, false),
        ("1200", "Equipment", AccountType::Asset, false),
        ("2000", "Trade payables", AccountType::Liability, false),
        ("2100", "Tax payable", AccountType::Liability, false),
        ("2200", "Accrued expenses", AccountType::Liability, false),
        ("3000", "Share capital", AccountType::Equity, false),
        ("3100", "Retained earnings", AccountType::Equity, true),
        ("4000", "Sales", AccountType::Income, false),
        ("4100", "Service income", AccountType::Income, false),
        ("5000", "Cost of sales", AccountType::Expense, false),
        ("5100", "Rent", AccountType::Expense, false),
        ("5200", "Salaries", AccountType::Expense, false),
        ("6000", "Bank charges", AccountType::Expense, false),
    ];
    for (code, name, kind, retained) in chart {
        let mut account = NewAccount::new(code, name, kind);
        account.retained_earnings = retained;
        ids.insert(
            code,
            ledger.create_account(manager, COMPANY, &account).await?.id,
        );
    }
    // A group over the bank accounts, and the operating account inside it.
    let group = ledger
        .create_account_group(
            manager,
            COMPANY,
            &NewAccountGroup::new("1900", "Cash and bank", AccountType::Asset),
        )
        .await?;
    ledger
        .update_account(
            manager,
            COMPANY,
            ids["1000"],
            &AccountChange {
                parent_id: Some(Some(group.id)),
                ..AccountChange::default()
            },
        )
        .await?;
    let mut operating = NewAccount::new("1010", "Operating account", AccountType::Asset);
    operating.parent_id = Some(group.id);
    ids.insert(
        "1010",
        ledger
            .create_account(manager, COMPANY, &operating)
            .await?
            .id,
    );

    let this_year = jiff::Zoned::now().year();
    let first = date(this_year - 1, 1, 1);
    ledger
        .open_fiscal_year(
            manager,
            COMPANY,
            &NewFiscalYear {
                start_date: Some(first),
                end_date: None,
            },
        )
        .await?;
    ledger
        .open_fiscal_year(manager, COMPANY, &NewFiscalYear::default())
        .await?;

    if std::env::args().any(|arg| arg.starts_with("opening")) {
        return Ok((ids["1000"], 0));
    }

    let money = |text: &str| Money::parse(text, units);
    let post = |day: Date,
                description: &str,
                reference: Option<&str>,
                debit: &str,
                credit: &str,
                amount: &str| {
        let entry = NewEntry {
            entry_date: day,
            description: description.into(),
            reference: reference.map(str::to_owned),
            lines: vec![
                NewLine::debit(ids[debit], money(amount).expect("fixture amount")),
                NewLine::credit(ids[credit], money(amount).expect("fixture amount")),
            ],
        };
        async move {
            ledger
                .post_entry(manager, COMPANY, &entry, &PostOptions::default())
                .await
        }
    };
    let last = this_year - 1;
    post(
        date(last, 1, 2),
        "Shareholders' capital paid in",
        Some("CAP-001"),
        "1010",
        "3000",
        "50000.00",
    )
    .await?;
    post(
        date(last, 2, 10),
        "Equipment bought",
        Some("INV-7781"),
        "1200",
        "1010",
        "12000.00",
    )
    .await?;
    for month in 3..=11 {
        post(
            date(last, month, 15),
            "Sales for the month",
            None,
            "1010",
            "4000",
            "9500.00",
        )
        .await?;
        post(
            date(last, month, 20),
            "Rent for the month",
            Some("RENT"),
            "5100",
            "1010",
            "2500.00",
        )
        .await?;
        post(
            date(last, month, 28),
            "Salaries for the month",
            None,
            "5200",
            "1010",
            "4200.00",
        )
        .await?;
    }
    post(
        date(last, 12, 5),
        "Service income, consulting",
        Some("SVC-12"),
        "1100",
        "4100",
        "3800.00",
    )
    .await?;
    post(
        date(last, 12, 31),
        "Bank charges for the year",
        None,
        "6000",
        "1010",
        "96.40",
    )
    .await?;
    for month in 1..=9 {
        post(
            date(this_year, month, 14),
            "Sales for the month",
            None,
            "1010",
            "4000",
            "11200.50",
        )
        .await?;
        post(
            date(this_year, month, 20),
            "Rent for the month",
            Some("RENT"),
            "5100",
            "1010",
            "2500.00",
        )
        .await?;
        post(
            date(this_year, month, 27),
            "Salaries for the month",
            None,
            "5200",
            "1010",
            "4350.00",
        )
        .await?;
    }
    let wrong = post(
        date(this_year, 9, 30),
        "Office supplies, wrong account",
        Some("EXP-221"),
        "6000",
        "1010",
        "318.75",
    )
    .await?;

    // Close last year, then the first eight periods of this one.
    let years = ledger.list_fiscal_years(manager, COMPANY).await?;
    ledger
        .close_fiscal_year(manager, COMPANY, years[0].id, &Default::default())
        .await?;
    for period in years[1].periods.iter().take(8) {
        ledger
            .close_period(manager, COMPANY, period.id, None)
            .await?;
    }

    // Manual journals: one waiting, one approved, one rejected.
    let adjustment =
        |day: Date, description: &str, debit: &str, credit: &str, amount: &str| NewEntry {
            entry_date: day,
            description: description.into(),
            reference: Some("ADJ".into()),
            lines: vec![
                NewLine::debit(ids[debit], money(amount).expect("fixture amount"))
                    .described("Adjustment"),
                NewLine::credit(ids[credit], money(amount).expect("fixture amount")),
            ],
        };
    let approved = ledger
        .submit_journal(
            manager,
            COMPANY,
            &adjustment(
                date(this_year, 9, 30),
                "Accrue September audit fee",
                "5100",
                "2200",
                "1800.00",
            ),
        )
        .await?;
    ledger
        .approve_journal(reviewer, COMPANY, approved.id)
        .await?;
    let rejected = ledger
        .submit_journal(
            manager,
            COMPANY,
            &adjustment(
                date(this_year, 9, 30),
                "Write off a receivable",
                "6000",
                "1100",
                "950.00",
            ),
        )
        .await?;
    ledger
        .reject_journal(
            reviewer,
            COMPANY,
            rejected.id,
            "The customer has paid. No write-off.",
        )
        .await?;
    ledger
        .submit_journal(
            manager,
            COMPANY,
            &adjustment(
                date(this_year, 10, 15),
                "Accrue October salaries",
                "5200",
                "2200",
                "4350.00",
            ),
        )
        .await?;
    println!(
        "Ledger seeded: wrong-account entry {}; journal waiting for approval is the latest",
        wrong.id
    );
    Ok((ids["1010"], wrong.entry_number))
}

/// The Book page of a company whose tenant has enabled no currency: without
/// `admin.currency.manage` the manager is told to ask an administrator; with
/// it, every currency is offered, opening the book in MYR enables MYR in the
/// same write, and the page hands the person to Chart of accounts.
async fn book_page_proof(
    db: &bilibi_database::Db,
    services: &Services,
    manager: &Scope,
) -> Outcome<()> {
    // A page write carries the person's session, as the browser's does.
    let login = services
        .get::<bilibi_core::user::UserService>("core/user.accounts")?
        .login(
            "manager@other.example.test",
            bilibi_finance_walkthrough_fixture::PASSWORD,
        )
        .await?;
    assert!(!login.pending);
    let pages = services.get::<PageService<Scope, Error>>("finance/ledger.pages")?;
    let path = HashMap::from([("id".to_owned(), COMPANY.to_string())]);
    let mount = || async {
        Ok::<State, Box<dyn std::error::Error>>(
            pages
                .0
                .mount(PageRequest {
                    operation: "book",
                    scope: Some(manager),
                    path: &path,
                    query: &HashMap::new(),
                    body: &[],
                    framed: false,
                })
                .await?
                .expect("the book page mounts"),
        )
    };
    let used = || async {
        Ok::<i64, Box<dyn std::error::Error>>(
            db.get()
                .await?
                .query_one(
                    "SELECT count(*) FROM currency_tenant_used WHERE tenant_id=2 AND code='MYR'",
                    &[],
                )
                .await?
                .get(0),
        )
    };

    // The tenant has used no currency and the manager manages no currency:
    // the contract an agent calls still offers every current one.
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let client = db.get().await?;
    let currencies = registry
        .call(
            &**client,
            manager,
            "finance.ledger.book.currencies",
            &json!({"company_id":COMPANY}),
        )
        .await?;
    drop(client);
    assert_eq!(currencies["result"]["reference"], "iso4217");
    assert!(
        currencies["result"]["currencies"]
            .as_array()
            .is_some_and(|listed| listed
                .iter()
                .any(|c| c["code"] == "MYR" && c["used"] == false)),
        "every current currency should be offered: {currencies}"
    );

    let mut state = mount().await?;
    let offered = update(&mut state, manager, LiveInput::Refresh).await?;
    has(&offered, "ledger-book-back");
    has(&offered, "ledger-book-currency-code");
    has(&offered, "MYR Malaysian");
    for gone in [
        "ledger-book-none-enabled",
        "Ask an administrator",
        "Proposed starter chart",
        "awaiting accountant review",
        "ledger-book-chart",
    ] {
        lacks(&offered, gone);
    }
    // Choosing a currency brings its standard decimal places (MYR: 2).
    let cleared = json!({"type":"validate","book[currency_code]":"","book[minor_units]":"0","book[approver]":"someone_else"});
    act(&mut state, manager, &login.token, &cleared).await?;
    let chosen = json!({"type":"validate","book[currency_code]":"MYR","book[minor_units]":"0","book[approver]":"someone_else"});
    let chosen = act(&mut state, manager, &login.token, &chosen).await?;
    lacks(&chosen, "also enables");
    let review = json!({"type":"review_open"});
    let review = act(&mut state, manager, &login.token, &review).await?;
    has(&review, "ledger-book-confirm");
    lacks(&review, "also enables");
    assert_eq!(used().await?, 0, "the review marked MYR used");
    let open = json!({"type":"open_book"});
    let opened = act(&mut state, manager, &login.token, &open).await?;
    assert_eq!(
        opened.redirect.as_deref(),
        Some(format!("/finance/ledger/accounts?company={COMPANY}").as_str()),
        "opening the book should land on Chart of accounts"
    );
    assert!(
        opened.flash.is_some(),
        "the book-open message should travel"
    );
    let book = services
        .get::<Ledger>(bilibi_finance_ledger::SERVICE)?
        .get_book(manager, COMPANY)
        .await?;
    assert_eq!(
        (book.currency_code.as_str(), book.minor_units.get()),
        ("MYR", 2)
    );
    assert_eq!(used().await?, 1, "opening the book should mark MYR used");
    let mut opened_page = mount().await?;
    let seen = update(&mut opened_page, manager, LiveInput::Refresh).await?;
    has(&seen, "ledger-book-controls-change");
    let viewer = bilibi_finance_walkthrough_fixture::sign_in(db, services, 2, 9).await?;
    let mut viewer_page = pages
        .0
        .mount(PageRequest {
            operation: "book",
            scope: Some(&viewer),
            path: &path,
            query: &HashMap::new(),
            body: &[],
            framed: false,
        })
        .await?
        .expect("the book page mounts for a reader");
    let read = update(&mut viewer_page, &viewer, LiveInput::Refresh).await?;
    has(&read, "ledger-book-controls");
    lacks(&read, "ledger-book-controls-change");
    println!(
        "Book page and book.open: with no currency used yet and no currency manage, every current currency is offered; MYR is reviewed with no enable step, opens the book, is marked used in the same write, keeps its 2 decimal places and lands on Chart of accounts"
    );
    Ok(())
}

/// A page event that writes, with the person's session credential.
async fn act(
    state: &mut State,
    scope: &Scope,
    credential: &str,
    event: &serde_json::Value,
) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential,
            input: LiveInput::Event(event),
        })
        .await?)
}

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
    query: &HashMap<String, String>,
) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/ledger.pages")?;
    Ok(pages
        .0
        .mount(PageRequest {
            operation: "journals",
            scope: Some(scope),
            path: &HashMap::new(),
            query,
            body: &[],
            framed: false,
        })
        .await?
        .expect("the journal entries page mounts"))
}

fn has(body: &LiveBody, expected: &str) {
    assert!(body.html.contains(expected), "page missing {expected}");
}

fn lacks(body: &LiveBody, unexpected: &str) {
    assert!(!body.html.contains(unexpected), "page shows {unexpected}");
}

/// The address a person would reload: no sentinel year, and every date key
/// that is there parses as a date.
fn address(body: &LiveBody) {
    for (key, value) in &body.query {
        assert!(
            !value.contains("0001-01-01") && !value.contains("9999-12-31"),
            "{key}={value} is a sentinel"
        );
    }
}

fn all_dates(body: &LiveBody) {
    for key in ["period", "from", "to"] {
        assert!(!body.query.contains_key(key), "{key} is in the address");
    }
    address(body);
}

fn shows_all_dates(body: &LiveBody) {
    all_dates(body);
    has(body, "ledger-journals-period-all");
    lacks(body, "ledger-journals-period-clear");
}

/// Journals' period control, through the page's own live state on the
/// seeded entries: All dates by default, a preset, the custom range dialog
/// (refused, cancelled, applied with both ends inclusive), the return to All
/// dates, a `to`-only address, and the first-entry reset on every new period.
async fn period_proof(services: &Services, scope: &Scope, account: i64, pin: i64) -> Outcome<()> {
    const FILTER: &str = "ledger-journals-period";
    let last = jiff::Zoned::now().year() - 1;
    let none = HashMap::new();
    let select = |period: &str| json!({"type": format!("period_select:{FILTER}"), "period_filter[period]": period});
    let apply = |from: Date, to: Date| {
        json!({
            "type": format!("period_apply:{FILTER}"),
            "range[from]": from.to_string(),
            "range[to]": to.to_string(),
        })
    };
    let cancel = json!({"type": format!("period_cancel:{FILTER}")});
    let clear = json!({"type": format!("period_clear:{FILTER}")});
    let (march_15, march_20, march_28) = (date(last, 3, 15), date(last, 3, 20), date(last, 3, 28));

    let mut state = mount(services, scope, &none).await?;
    let every = update(&mut state, scope, LiveInput::Refresh).await?;
    assert!(every.rows > 3 && every.rows < 100, "{} entries", every.rows);
    shows_all_dates(&every);
    has(&every, "All dates");
    has(&every, "Accrue September audit fee");
    has(&every, "Accrue October salaries");
    has(&every, "Write off a receivable");
    assert_eq!(
        every.html.matches("Accrue September audit fee").count(),
        1,
        "approved journal must appear only as its entry"
    );
    let waiting_filter = json!({"type":"filters", "filters[status]":"submitted"});
    let waiting = update(&mut state, scope, LiveInput::Event(&waiting_filter)).await?;
    has(&waiting, "Accrue October salaries");
    lacks(&waiting, "Accrue September audit fee");
    let rejected_filter = json!({"type":"filters", "filters[status]":"rejected"});
    let rejected = update(&mut state, scope, LiveInput::Event(&rejected_filter)).await?;
    has(&rejected, "Write off a receivable");
    lacks(&rejected, "Accrue October salaries");
    update(&mut state, scope, LiveInput::Params(&none)).await?;
    let first_id = every
        .html
        .split("id=\"ledger-entry-")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("an entry row");

    // A later page (the address carries a cursor) starts again from the
    // first matching entry once a period is applied.
    let after = HashMap::from([(
        "after".to_owned(),
        format!("2000-01-02T00:00:00.000000|{first_id}|entry"),
    )]);
    let mut paged = mount(services, scope, &after).await?;
    let later = update(&mut paged, scope, LiveInput::Refresh).await?;
    assert!(later.query.contains_key("after"));
    let today = update(&mut paged, scope, LiveInput::Event(&select("today"))).await?;
    assert!(!today.query.contains_key("after"));
    assert_eq!(today.query["period"], "today");
    assert_eq!(today.query["from"], today.query["to"]);
    assert!(today.rows <= every.rows);
    address(&today);
    has(&today, "ledger-journals-period-clear");
    lacks(&today, "ledger-journals-period-all");

    // A refused range and Cancel leave the applied preset as it was.
    let open = update(&mut paged, scope, LiveInput::Event(&select("custom"))).await?;
    has(&open, "ledger-journals-period-range");
    address(&open);
    let refused = update(
        &mut paged,
        scope,
        LiveInput::Event(&apply(march_28, march_15)),
    )
    .await?;
    has(&refused, "Start date must be on or before end date.");
    assert_eq!(refused.query["period"], "today");
    assert_eq!(refused.query["from"], today.query["from"]);
    let cancelled = update(&mut paged, scope, LiveInput::Event(&cancel)).await?;
    lacks(&cancelled, "ledger-journals-period-range");
    assert_eq!(cancelled.query["period"], "today");
    assert_eq!(cancelled.query["from"], today.query["from"]);

    // From All dates the dialog opens on a recent range, not on the date
    // field's limits, and Cancel stays on All dates.
    let open = update(&mut state, scope, LiveInput::Event(&select("custom"))).await?;
    has(&open, "ledger-journals-period-range");
    lacks(&open, "value=\"0001-01-01\"");
    lacks(&open, "value=\"9999-12-31\"");
    address(&open);
    let refused = update(
        &mut state,
        scope,
        LiveInput::Event(&apply(march_28, march_15)),
    )
    .await?;
    has(&refused, "Start date must be on or before end date.");
    all_dates(&refused);
    let cancelled = update(&mut state, scope, LiveInput::Event(&cancel)).await?;
    lacks(&cancelled, "ledger-journals-period-range");
    shows_all_dates(&cancelled);

    // Both ends of a custom range are inclusive.
    let span = update(
        &mut state,
        scope,
        LiveInput::Event(&apply(march_15, march_28)),
    )
    .await?;
    assert_eq!(span.query["period"], "custom");
    assert_eq!(span.query["from"], march_15.to_string());
    assert_eq!(span.query["to"], march_28.to_string());
    assert_eq!(span.rows, 3);
    address(&span);
    let one = update(
        &mut state,
        scope,
        LiveInput::Event(&apply(march_28, march_28)),
    )
    .await?;
    assert_eq!(one.rows, 1);

    // Reload keeps the range; Back to the bare address is All dates.
    let reloaded = update(&mut state, scope, LiveInput::Params(&span.query)).await?;
    assert_eq!(reloaded.rows, 3);
    assert_eq!(reloaded.query, span.query);
    let back = update(&mut state, scope, LiveInput::Params(&none)).await?;
    assert_eq!(back.rows, every.rows);
    shows_all_dates(&back);

    // The date field's own limits are no range: applying them is All dates.
    update(
        &mut state,
        scope,
        LiveInput::Event(&apply(march_15, march_28)),
    )
    .await?;
    let widest = update(
        &mut state,
        scope,
        LiveInput::Event(&apply(date(1, 1, 1), date(9999, 12, 31))),
    )
    .await?;
    assert_eq!(widest.rows, every.rows);
    shows_all_dates(&widest);

    // The account filter and the trial balance pin survive every change.
    let keep = HashMap::from([
        ("account".to_owned(), account.to_string()),
        ("through".to_owned(), pin.to_string()),
    ]);
    let mut kept = mount(services, scope, &keep).await?;
    let base = update(&mut kept, scope, LiveInput::Refresh).await?;
    assert!(base.rows > 0 && base.rows <= every.rows);
    shows_all_dates(&base);
    let ranged = update(
        &mut kept,
        scope,
        LiveInput::Event(&apply(march_15, march_28)),
    )
    .await?;
    assert_eq!(ranged.rows, 3);
    assert_eq!(ranged.query["account"], account.to_string());
    assert_eq!(ranged.query["through"], pin.to_string());
    let cleared = update(&mut kept, scope, LiveInput::Event(&clear)).await?;
    assert_eq!(cleared.rows, base.rows);
    assert_eq!(cleared.query["account"], account.to_string());
    assert_eq!(cleared.query["through"], pin.to_string());
    shows_all_dates(&cleared);

    // The trial balance opens the list with a `to` and no `from`: the start
    // stays open, in the address and in the list.
    let to_only = HashMap::from([("to".to_owned(), march_20.to_string())]);
    let mut opened = mount(services, scope, &to_only).await?;
    let until = update(&mut opened, scope, LiveInput::Refresh).await?;
    assert_eq!(until.query["to"], march_20.to_string());
    for key in ["period", "from"] {
        assert!(!until.query.contains_key(key), "{key} is in the address");
    }
    address(&until);
    has(&until, "ledger-journals-period-clear");
    let mut bounded = mount(services, scope, &none).await?;
    let january = update(
        &mut bounded,
        scope,
        LiveInput::Event(&apply(date(last, 1, 1), march_20)),
    )
    .await?;
    assert!(until.rows > 0 && until.rows < every.rows);
    assert_eq!(until.rows, january.rows);
    let everything = update(&mut opened, scope, LiveInput::Event(&clear)).await?;
    assert_eq!(everything.rows, every.rows);
    shows_all_dates(&everything);

    println!(
        "Journals period proof: All dates by default and after Clear; preset restarts the list; refused range and Cancel keep the period; inclusive custom range; reload and Back; account and pin kept; to-only start open; no sentinel year in the address"
    );
    Ok(())
}
