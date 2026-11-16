//! `DATABASE_URL=… cargo run -p bilibi-finance-walkthrough-fixture`, from the
//! Platform root with this container mounted, against a disposable database:
//! migrates it, installs the Platform and Finance walkthrough fixtures, and
//! then shows the ledger facade refusing and accepting what Bilimbi's does,
//! with the database's own refusals last. Every line is the facade's actual
//! answer; nothing here is a test double.
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{
    AccountChange, AccountGroupChange, AccountType, Entry, EntryOwner, EntryQuery, Ledger,
    LedgerError, MinorUnits, Money, NewAccount, NewAccountGroup, NewBook, NewEntry, NewLine,
    PostOptions, PostingEvent, PostingSubscription, SERVICE,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};

mod einvoice;
use jiff::civil::date;
use std::{fmt::Debug, time::Duration};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_einvoice_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payroll_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();

/// An entry on one line: number, date, description, owner and `account:amount`
/// per line (debits positive, credits negative).
fn brief(entry: &Entry) -> String {
    let lines: Vec<String> = entry
        .lines
        .iter()
        .map(|line| {
            let amount = if line.debit.is_zero() {
                format!("-{}", line.credit)
            } else {
                line.debit.to_string()
            };
            format!("{}:{amount}", line.account_id)
        })
        .collect();
    format!(
        "#{} {} {:?} owner={} reverses={:?} reversed_by={:?} [{}]",
        entry.entry_number,
        entry.entry_date,
        entry.description,
        entry
            .owner
            .as_ref()
            .map_or("none".into(), |o| format!("{} {}", o.module, o.record)),
        entry.reverses_entry_id,
        entry.reversed_by_entry_id,
        lines.join(" ")
    )
}

/// What the subscription hears within `wait`, or nothing.
async fn heard(subscription: &mut PostingSubscription, wait: Duration) -> String {
    match tokio::time::timeout(wait, subscription.next()).await {
        Ok(Some(PostingEvent::Posted {
            company_id,
            entry_number,
        })) => format!("posted: company {company_id} entry {entry_number}"),
        Ok(Some(PostingEvent::Refresh)) => "refresh".into(),
        Ok(None) => "relay gone".into(),
        Err(_) => "nothing".into(),
    }
}

fn show<T: Debug>(step: &str, result: Result<T, LedgerError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(LedgerError::Invalid(errors)) => {
            let errors: Vec<String> = errors
                .iter()
                .map(|error| match error.line {
                    Some(line) => format!("line {line} {} {}", error.field, error.message),
                    None => format!("{} {}", error.field, error.message),
                })
                .collect();
            println!("{step}: refused ({})", errors.join("; "));
        }
        Err(error) => println!("{step}: refused ({error:?})"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_web::verify_composition(&db).await?;
    let migrated = bilibi_database::migrate(&db, &modules).await?;
    println!("mounted Domain modules: {:?}", bilibi_domains::MODULES);
    println!("migrations applied: {:?}", migrated.applied);
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;

    bilibi_finance_walkthrough_fixture::adopter_setup::opening(&db, &services).await?;
    let installed = install(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::receivables::run(&db, &services).await?;
    println!("fixture book: {:?}", installed.book);
    for account in &installed.accounts {
        println!("fixture account: {account:?}");
    }
    let ledger = services.get::<Ledger>(SERVICE)?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let viewer = sign_in(&db, &services, 1, 2).await?;
    let admin = sign_in(&db, &services, 1, 4).await?;
    let other = sign_in(&db, &services, 2, 5).await?;
    let contractor = sign_in(&db, &services, 1, 7).await?;
    let find = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|account| account.code == code)
            .map(|account| account.id)
            .ok_or(Error::NotFound)
    };
    let cash = find("1000")?;
    let receivables = find("1100")?;
    let sales = find("4000")?;
    let capital = find("3000")?;
    let retained = find("3100")?;

    println!("\n== Books");
    show(
        "manager reads company 1's book",
        ledger.get_book(&manager, COMPANY).await,
    );
    show(
        "manager opens company 1's book again",
        ledger
            .open_book(
                &manager,
                COMPANY,
                &NewBook {
                    currency_code: "MYR".into(),
                    minor_units: 2,
                },
            )
            .await,
    );
    show(
        "admin reads company 2's book",
        ledger.get_book(&admin, 2).await,
    );
    show(
        "admin opens company 2's book with a bad currency and 5 minor units",
        ledger
            .open_book(
                &admin,
                2,
                &NewBook {
                    currency_code: "ringgit".into(),
                    minor_units: 5,
                },
            )
            .await,
    );
    show(
        "contractor (staff, affiliated with company 2) opens company 2's book as ' usd ' with 2 minor units",
        ledger
            .open_book(
                &contractor,
                2,
                &NewBook {
                    currency_code: " usd ".into(),
                    minor_units: 2,
                },
            )
            .await,
    );

    println!("\n== Reach: grants, affiliation, tenancy, archival");
    show(
        "viewer (no affiliation, no tenant-wide reach) lists company 1's accounts",
        ledger.list_accounts(&viewer, COMPANY).await,
    );
    show(
        "contractor (affiliated with company 2, no tenant-wide reach) reads company 1's book",
        ledger.get_book(&contractor, COMPANY).await,
    );
    show(
        "manager (tenant-wide reach) reads company 2's book",
        ledger.get_book(&manager, 2).await,
    );
    show(
        "tenant 2's manager reads company 1's book",
        ledger.get_book(&other, COMPANY).await,
    );
    show(
        "tenant 2's manager reads tenant 2's company 100 book",
        ledger.get_book(&other, 100).await,
    );
    let client = db.get().await?;
    client
        .execute(
            "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=3",
            &[],
        )
        .await?;
    show(
        "admin opens archived company 3's book",
        ledger
            .open_book(
                &admin,
                3,
                &NewBook {
                    currency_code: "MYR".into(),
                    minor_units: 2,
                },
            )
            .await,
    );

    println!("\n== Chart of accounts");
    println!(
        "account types: {:?}",
        Ledger::account_types()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
    );
    show(
        "manager adds 1000 again",
        ledger
            .create_account(
                &manager,
                COMPANY,
                &NewAccount::new("1000", "Petty cash", AccountType::Asset),
            )
            .await,
    );
    show(
        "manager adds a blank code and an over-long name",
        ledger
            .create_account(
                &manager,
                COMPANY,
                &NewAccount::new("  ", "n".repeat(201), AccountType::Asset),
            )
            .await,
    );
    show(
        "manager designates an income account for retained earnings",
        ledger
            .create_account(&manager, COMPANY, &{
                let mut new = NewAccount::new("4100", "Other income", AccountType::Income);
                new.retained_earnings = true;
                new
            })
            .await,
    );
    show(
        "manager groups an expense under the sales account (an account, not a group)",
        ledger
            .create_account(&manager, COMPANY, &{
                let mut new = NewAccount::new("5100", "Freight", AccountType::Expense);
                new.parent_id = Some(sales);
                new
            })
            .await,
    );
    show(
        "manager groups an account under another company's account id",
        ledger
            .create_account(&manager, COMPANY, &{
                let mut new = NewAccount::new("1200", "Deposits", AccountType::Asset);
                new.parent_id = Some(999_999);
                new
            })
            .await,
    );
    show(
        "manager adds a group with a code an account already has",
        ledger
            .create_account_group(
                &manager,
                COMPANY,
                &NewAccountGroup::new("1000", "Current assets", AccountType::Asset),
            )
            .await,
    );
    let current = ledger
        .create_account_group(
            &manager,
            COMPANY,
            &NewAccountGroup::new("1900", "Current assets", AccountType::Asset),
        )
        .await?;
    println!("manager adds group 1900 Current assets: {current:?}");
    let current = current.id;
    let nested = ledger
        .create_account_group(&manager, COMPANY, &{
            let mut new = NewAccountGroup::new("1950", "Cash and bank", AccountType::Asset);
            new.parent_id = Some(current);
            new
        })
        .await?;
    println!("manager adds group 1950 inside 1900: {nested:?}");
    let nested = nested.id;
    show(
        "manager adds an expense group inside the asset group 1900",
        ledger
            .create_account_group(&manager, COMPANY, &{
                let mut new = NewAccountGroup::new("5900", "Overheads", AccountType::Expense);
                new.parent_id = Some(current);
                new
            })
            .await,
    );
    show(
        "manager places group 1900 inside 1950, which sits inside 1900",
        ledger
            .update_account_group(
                &manager,
                COMPANY,
                current,
                &AccountGroupChange {
                    parent_id: Some(Some(nested)),
                    ..AccountGroupChange::default()
                },
            )
            .await,
    );
    show(
        "manager changes group 1900 as an account",
        ledger
            .update_account(
                &manager,
                COMPANY,
                current,
                &AccountChange {
                    active: Some(false),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "manager changes account 1000 as a group",
        ledger
            .update_account_group(
                &manager,
                COMPANY,
                cash,
                &AccountGroupChange {
                    name: Some("Cash".into()),
                    ..AccountGroupChange::default()
                },
            )
            .await,
    );
    let petty = ledger
        .create_account(&manager, COMPANY, &{
            let mut new = NewAccount::new("1010", "Petty cash", AccountType::Asset);
            new.parent_id = Some(nested);
            new
        })
        .await?;
    println!("manager adds 1010 Petty cash inside group 1950: {petty:?}");
    let petty = petty.id;
    show(
        "manager renames 1010 and takes it out of its group",
        ledger
            .update_account(
                &manager,
                COMPANY,
                petty,
                &AccountChange {
                    name: Some("  Petty cash float  ".into()),
                    parent_id: Some(None),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "manager puts account 1000 in group 1950 (a group of its type)",
        ledger
            .update_account(
                &manager,
                COMPANY,
                cash,
                &AccountChange {
                    parent_id: Some(Some(nested)),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "manager puts account 1100 in the asset group 1900, and income account 4000 in it",
        {
            ledger
                .update_account(
                    &manager,
                    COMPANY,
                    receivables,
                    &AccountChange {
                        parent_id: Some(Some(current)),
                        ..AccountChange::default()
                    },
                )
                .await?;
            ledger
                .update_account(
                    &manager,
                    COMPANY,
                    sales,
                    &AccountChange {
                        parent_id: Some(Some(current)),
                        ..AccountChange::default()
                    },
                )
                .await
        },
    );
    show(
        "manager renames group 1950 and moves it to the top",
        ledger
            .update_account_group(
                &manager,
                COMPANY,
                nested,
                &AccountGroupChange {
                    name: Some("  Bank and cash  ".into()),
                    parent_id: Some(None),
                },
            )
            .await,
    );
    show(
        "manager reads the chart's balances and group totals before any posting",
        ledger
            .chart_balances(&manager, COMPANY, date(2026, 12, 31), None)
            .await
            .map(|chart| {
                chart
                    .rows
                    .iter()
                    .filter(|r| r.account.parent_id.is_some() || r.account.is_group)
                    .map(|r| {
                        (
                            r.account.code.clone(),
                            r.balance.map(|m| m.to_string()),
                            r.total.map(|m| m.to_string()),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "manager deactivates 1010 (balance zero)",
        ledger
            .update_account(
                &manager,
                COMPANY,
                petty,
                &AccountChange {
                    active: Some(false),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "manager moves the retained-earnings designation from 3100 to 3000",
        ledger
            .update_account(
                &manager,
                COMPANY,
                capital,
                &AccountChange {
                    retained_earnings: Some(true),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "3100 after the move",
        ledger
            .list_accounts(&manager, COMPANY)
            .await
            .map(|accounts| accounts.into_iter().find(|a| a.id == retained)),
    );
    show(
        "manager designates the sales account",
        ledger
            .update_account(
                &manager,
                COMPANY,
                sales,
                &AccountChange {
                    retained_earnings: Some(true),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "manager updates an account id of another company",
        ledger
            .update_account(
                &manager,
                COMPANY,
                999_999,
                &AccountChange {
                    active: Some(false),
                    ..AccountChange::default()
                },
            )
            .await,
    );
    show(
        "admin (grant_all, tenant-wide) lists company 1's accounts",
        ledger
            .list_accounts(&admin, COMPANY)
            .await
            .map(|accounts| accounts.len()),
    );

    println!("\n== Journal entries");
    let units = installed.book.minor_units;
    let money = |text: &str| Money::parse(text, units).expect("fixture amount");
    let tax = find("2100")?;
    let costs = find("5000")?;
    let day = date(2026, 10, 1);
    let entry = |description: &str, lines: Vec<NewLine>| NewEntry {
        entry_date: day,
        description: description.into(),
        reference: None,
        lines,
    };
    let manual = PostOptions::default();
    ledger
        .create_account(
            &contractor,
            2,
            &NewAccount::new("9000", "Company 2 cash", AccountType::Asset),
        )
        .await?;
    let mut subscription = ledger.subscribe_postings(&manager, COMPANY).await?;
    println!("manager subscribes to company 1's postings");
    show(
        "viewer (no affiliation) subscribes to company 1's postings",
        ledger
            .subscribe_postings(&viewer, COMPANY)
            .await
            .map(|_| ()),
    );
    show(
        "manager posts to account group 1900: Dr 1900 / Cr 3000 5.00",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Posted to a group",
                    vec![
                        NewLine::debit(current, money("5.00")),
                        NewLine::credit(capital, money("5.00")),
                    ],
                ),
                &manual,
            )
            .await
            .map(|e| e.entry_number),
    );
    show(
        "manager posts capital: Dr 1000 / Cr 3000 1,000.00",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    reference: Some("  CAP-1 ".into()),
                    ..entry(
                        "  Capital injected  ",
                        vec![
                            NewLine::debit(cash, money("1000.00")).described("Bank"),
                            NewLine::credit(capital, money("1000.00")),
                        ],
                    )
                },
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    println!(
        "subscriber hears: {}",
        heard(&mut subscription, Duration::from_secs(5)).await
    );
    let invoice = EntryOwner {
        module: "finance/receivables".into(),
        record: "invoice:17".into(),
    };
    let owned = PostOptions {
        owner: Some(invoice.clone()),
        ..PostOptions::default()
    };
    let invoice_entry = ledger
        .post_entry(
            &manager,
            COMPANY,
            &entry(
                "Invoice 17",
                vec![
                    NewLine::debit(receivables, money("118.00")),
                    NewLine::credit(sales, money("100.00")),
                    NewLine::credit(tax, money("18.00")),
                ],
            ),
            &owned,
        )
        .await?;
    println!(
        "manager posts invoice 17 owned by finance/receivables: {}",
        brief(&invoice_entry)
    );
    println!(
        "subscriber hears: {}",
        heard(&mut subscription, Duration::from_secs(5)).await
    );
    show(
        "manager reads group totals after the capital entry (1900 and 1950 hold accounts, never postings)",
        ledger
            .chart_balances(&manager, COMPANY, date(2026, 12, 31), None)
            .await
            .map(|chart| {
                chart
                    .rows
                    .iter()
                    .filter(|r| r.account.is_group || r.account.parent_id.is_some())
                    .map(|r| {
                        (
                            r.account.code.clone(),
                            r.balance.map(|m| m.to_string()),
                            r.total.map(|m| m.to_string()),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "manager posts a second original entry for invoice:17",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Invoice 17 again",
                    vec![
                        NewLine::debit(receivables, money("5.00")),
                        NewLine::credit(sales, money("5.00")),
                    ],
                ),
                &owned,
            )
            .await
            .map(|e| brief(&e)),
    );
    println!(
        "subscriber hears after the refusals: {}",
        heard(&mut subscription, Duration::from_millis(500)).await
    );
    show(
        "manager posts one line, with a blank description",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry("   ", vec![NewLine::debit(cash, money("1.00"))]),
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager posts debits 10.00 against credits 9.99",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Unbalanced",
                    vec![
                        NewLine::debit(cash, money("10.00")),
                        NewLine::credit(sales, money("9.99")),
                    ],
                ),
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    let mut both = NewLine::debit(cash, money("1.00"));
    both.credit = money("1.00");
    let none = NewLine::debit(sales, Money::ZERO);
    show(
        "manager posts a line with both sides and a line with neither",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry("Two sides", vec![both, none]),
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    let fine = Money::parse("1.005", MinorUnits::new(4).expect("units")).expect("amount");
    show(
        "manager posts 1.005 into a two-place book, and a negative amount",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Too fine",
                    vec![
                        NewLine::debit(cash, fine),
                        NewLine::credit(sales, fine),
                        NewLine::debit(cash, -money("2.00")),
                    ],
                ),
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager posts to the deactivated 1010, an unknown account and another company's account",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Accounts",
                    vec![
                        NewLine::debit(petty, money("1.00")),
                        NewLine::credit(999_999, money("1.00")),
                    ],
                ),
                &manual,
            )
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager posts 1010 with it listed as a settlement account",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Settle petty cash float",
                    vec![
                        NewLine::debit(costs, money("4.00")),
                        NewLine::credit(petty, money("4.00")),
                    ],
                ),
                &PostOptions {
                    settlement_account_ids: vec![petty],
                    ..PostOptions::default()
                },
            )
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager posts with a settlement account that is not the company's",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(
                    "Settle elsewhere",
                    vec![
                        NewLine::debit(costs, money("4.00")),
                        NewLine::credit(999_999, money("4.00")),
                    ],
                ),
                &PostOptions {
                    settlement_account_ids: vec![999_999],
                    ..PostOptions::default()
                },
            )
            .await
            .map(|e| brief(&e)),
    );
    for (step, owner) in [
        (
            "an owner with a capital letter",
            ("Finance/receivables", "invoice:1"),
        ),
        (
            "an owner record with leading zero",
            ("finance/receivables", "invoice:01"),
        ),
        (
            "an owner record without a kind",
            ("finance/receivables", "17"),
        ),
        (
            "an owner module without a slash",
            ("receivables", "invoice:1"),
        ),
    ] {
        show(
            &format!("manager posts with {step}"),
            ledger
                .post_entry(
                    &manager,
                    COMPANY,
                    &entry(
                        "Owner shape",
                        vec![
                            NewLine::debit(cash, money("1.00")),
                            NewLine::credit(sales, money("1.00")),
                        ],
                    ),
                    &PostOptions {
                        owner: Some(EntryOwner {
                            module: owner.0.into(),
                            record: owner.1.into(),
                        }),
                        ..PostOptions::default()
                    },
                )
                .await
                .map(|e| brief(&e)),
        );
    }
    let simple = |note: &str| {
        entry(
            note,
            vec![
                NewLine::debit(cash, money("1.00")),
                NewLine::credit(sales, money("1.00")),
            ],
        )
    };
    show(
        "viewer (no entries.post, no affiliation) posts",
        ledger
            .post_entry(&viewer, COMPANY, &simple("Viewer"), &manual)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "contractor (staff, affiliated with company 2) posts into company 2: no entries.post",
        ledger
            .post_entry(&contractor, 2, &simple("Contractor"), &manual)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "admin posts into company 4, whose book is not open",
        ledger
            .post_entry(&admin, 4, &simple("No book"), &manual)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "admin posts into archived company 3",
        ledger
            .post_entry(&admin, 3, &simple("Archived"), &manual)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "tenant 2's manager posts into company 1",
        ledger
            .post_entry(&other, COMPANY, &simple("Other tenant"), &manual)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager deactivates 1000, which holds a balance",
        ledger
            .update_account(
                &manager,
                COMPANY,
                cash,
                &AccountChange {
                    active: Some(false),
                    ..AccountChange::default()
                },
            )
            .await,
    );

    println!("\n-- Numbering stays gapless under concurrent posts");
    let mut posts = Vec::new();
    for n in 0..8 {
        let (ledger, admin, simple) = (
            ledger.clone(),
            admin.clone(),
            simple(&format!("Concurrent {n}")),
        );
        posts.push(tokio::spawn(async move {
            ledger
                .post_entry(&admin, COMPANY, &simple, &PostOptions::default())
                .await
        }));
    }
    let mut numbers = Vec::new();
    for post in posts {
        numbers.push(post.await??.entry_number);
    }
    numbers.sort_unstable();
    println!("eight concurrent posts were numbered {numbers:?}");
    let latest = ledger
        .list_entries(&manager, COMPANY, &EntryQuery::default())
        .await?
        .iter()
        .map(|e| e.entry_number)
        .max();
    println!("highest entry number of company 1: {latest:?}");

    println!("\n-- Reading entries");
    show(
        "manager reads the invoice entry",
        ledger
            .get_entry(&manager, COMPANY, invoice_entry.id)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "manager reads an entry id that does not exist",
        ledger
            .get_entry(&manager, COMPANY, 999_999)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "viewer reads the invoice entry",
        ledger
            .get_entry(&viewer, COMPANY, invoice_entry.id)
            .await
            .map(|e| brief(&e)),
    );
    show(
        "contractor reads company 2 for company 1's entry id",
        ledger
            .get_entry(&contractor, 2, invoice_entry.id)
            .await
            .map(|e| brief(&e)),
    );
    let mut page = EntryQuery {
        limit: Some(4),
        ..EntryQuery::default()
    };
    let mut pages = Vec::new();
    loop {
        let entries = ledger.list_entries(&manager, COMPANY, &page).await?;
        pages.push(entries.iter().map(|e| e.entry_number).collect::<Vec<_>>());
        match entries.last() {
            Some(last) if entries.len() == 4 => page.after = Some((last.entry_date, last.id)),
            _ => break,
        }
    }
    println!("manager pages entries four at a time: {pages:?}");
    show(
        "manager lists entries on the sales account",
        ledger
            .list_entries(
                &manager,
                COMPANY,
                &EntryQuery {
                    account_id: Some(sales),
                    ..EntryQuery::default()
                },
            )
            .await
            .map(|entries| entries.iter().map(brief).collect::<Vec<_>>()),
    );
    show(
        "manager lists entries through number 2",
        ledger
            .list_entries(
                &manager,
                COMPANY,
                &EntryQuery {
                    through_entry_number: Some(2),
                    ..EntryQuery::default()
                },
            )
            .await
            .map(|entries| entries.iter().map(|e| e.entry_number).collect::<Vec<_>>()),
    );
    show(
        "manager lists entries dated after 1 October 2026",
        ledger
            .list_entries(
                &manager,
                COMPANY,
                &EntryQuery {
                    from: Some(date(2026, 10, 2)),
                    ..EntryQuery::default()
                },
            )
            .await
            .map(|entries| entries.len()),
    );
    for limit in [0, 501] {
        show(
            &format!("manager lists with limit {limit}"),
            ledger
                .list_entries(
                    &manager,
                    COMPANY,
                    &EntryQuery {
                        limit: Some(limit),
                        ..EntryQuery::default()
                    },
                )
                .await
                .map(|entries| entries.len()),
        );
    }

    println!("\n== Money in the book's minor units");
    let units = installed.book.minor_units;
    for text in ["12.50", "12.505", "-3", "10000000000000000", "twelve"] {
        match Money::parse(text, units) {
            Ok(money) => println!("{text:>8}: {} (shown {})", money, money.shown(units)),
            Err(error) => println!("{text:>8}: refused ({error})"),
        }
    }

    println!("\n== The database's own refusals");
    for (step, sql) in [
        (
            "UPDATE a book's currency",
            "UPDATE finance_ledger_books SET currency_code='USD' WHERE company_id=1",
        ),
        (
            "DELETE a book",
            "DELETE FROM finance_ledger_books WHERE company_id=1",
        ),
        (
            "DELETE an account",
            "DELETE FROM finance_ledger_accounts WHERE company_id=1 AND code='1000'",
        ),
        (
            "UPDATE an account's code",
            "UPDATE finance_ledger_accounts SET code='9999' WHERE company_id=1 AND code='1000'",
        ),
        (
            "UPDATE an account's type",
            "UPDATE finance_ledger_accounts SET account_type='equity' WHERE company_id=1 AND code='1000'",
        ),
        (
            "INSERT a second retained-earnings account",
            "UPDATE finance_ledger_accounts SET retained_earnings=true WHERE company_id=1 AND code='3100'",
        ),
        (
            "INSERT a posting line on an account group",
            "INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,e.id,99,g.id,1,0 FROM finance_ledger_entries e, finance_ledger_accounts g \
             WHERE e.company_id=1 AND e.entry_number=1 AND g.company_id=1 AND g.code='1900'",
        ),
        (
            "UPDATE an account into a group",
            "UPDATE finance_ledger_accounts SET is_group=true WHERE company_id=1 AND code='1000'",
        ),
        (
            "UPDATE an account's group to an account",
            "UPDATE finance_ledger_accounts a SET parent_id=p.id FROM finance_ledger_accounts p \
             WHERE a.company_id=1 AND a.code='1100' AND p.company_id=1 AND p.code='1000'",
        ),
        (
            "INSERT an account with a parent of another company",
            "INSERT INTO finance_ledger_accounts(tenant_id,company_id,code,name,account_type,parent_id,inserted_at,updated_at) \
             SELECT 1,2,'1000','Cash','asset',id,now(),now() FROM finance_ledger_accounts WHERE company_id=1 AND code='1000'",
        ),
        (
            "INSERT a bookkeeping change that changes nothing",
            "INSERT INTO finance_ledger_bookkeeping_changes(tenant_id,company_id,change_number,controlled,changed_by_user_id,changed_at) VALUES(1,1,NULL,true,1,NULL)",
        ),
        (
            "UPDATE a posted entry's description",
            "UPDATE finance_ledger_entries SET description='changed' WHERE company_id=1 AND entry_number=1",
        ),
        (
            "DELETE a posted entry",
            "DELETE FROM finance_ledger_entries WHERE company_id=1 AND entry_number=1",
        ),
        (
            "UPDATE a line's amount",
            "UPDATE finance_ledger_entry_lines SET debit=9 WHERE company_id=1 AND line_number=1",
        ),
        (
            "DELETE a line",
            "DELETE FROM finance_ledger_entry_lines WHERE company_id=1",
        ),
        (
            "TRUNCATE the entries",
            "TRUNCATE finance_ledger_entries",
        ),
        (
            "INSERT a line into a posted entry",
            "INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,e.id,9,a.id,5,0 FROM finance_ledger_entries e, finance_ledger_accounts a \
             WHERE e.company_id=1 AND e.entry_number=1 AND a.company_id=1 AND a.code='1000'",
        ),
        (
            "INSERT an entry with no lines",
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','No lines','MYR',1,now())",
        ),
        (
            "INSERT an unbalanced entry",
            "BEGIN; INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Unbalanced','MYR',1,now()); \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,(SELECT max(id) FROM finance_ledger_entries),1,id,5,0 FROM finance_ledger_accounts WHERE company_id=1 AND code='1000'; COMMIT",
        ),
        (
            "INSERT a line with both sides",
            "BEGIN; INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Both','MYR',1,now()); \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,(SELECT max(id) FROM finance_ledger_entries),1,id,5,5 FROM finance_ledger_accounts WHERE company_id=1 AND code='1000'; COMMIT",
        ),
        (
            "INSERT an entry in a currency other than its company's book",
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Wrong currency','USD',1,now())",
        ),
        (
            "INSERT a line on another company's account",
            "BEGIN; INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Foreign account','MYR',1,now()); \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,(SELECT max(id) FROM finance_ledger_entries),1,id,5,0 FROM finance_ledger_accounts WHERE company_id=2 LIMIT 1; COMMIT",
        ),
        (
            "INSERT a reversal of an owned entry without its owner",
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,reverses_entry_id,posted_by_user_id,posted_at) \
             SELECT 1,1,0,'2026-10-02','Manual reversal of an owned entry','MYR',id,1,now() FROM finance_ledger_entries WHERE company_id=1 AND owner_record='invoice:17'",
        ),
        (
            "INSERT an entry with an owner but no record",
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,owner_module,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Half owner','MYR','finance/receivables',1,now())",
        ),
        (
            "INSERT a second original entry for an owned record",
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,owner_module,owner_record,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Again','MYR','finance/receivables','invoice:17',1,now())",
        ),
        (
            "TRUNCATE the books",
            "TRUNCATE finance_ledger_books",
        ),
    ] {
        let outcome = client.batch_execute(sql).await;
        // A refusal inside an explicit BEGIN leaves the session aborted.
        let _ = client.batch_execute("ROLLBACK").await;
        match outcome {
            Ok(()) => println!("{step}: accepted (!)"),
            Err(error) => println!(
                "{step}: refused ({})",
                error
                    .as_db_error()
                    .map(|e| e.message().to_owned())
                    .unwrap_or_else(|| error.to_string())
            ),
        }
    }

    println!("\n== Posting notices announce only committed entries");
    let raw_entry = |note: &str| {
        format!(
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
             VALUES(1,1,0,'2026-10-01','Raw {note}','MYR',1,now()); \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
             SELECT 1,1,(SELECT max(id) FROM finance_ledger_entries),v.n, \
               (SELECT id FROM finance_ledger_accounts WHERE company_id=1 AND code=v.code),v.debit,v.credit \
             FROM (VALUES (1,'1000',3,0),(2,'4000',0,3)) v(n,code,debit,credit)"
        )
    };
    let mut watcher = ledger.subscribe_postings(&manager, COMPANY).await?;
    let mut neighbour = ledger.subscribe_postings(&contractor, 2).await?;
    client.batch_execute("BEGIN").await?;
    client.batch_execute(&raw_entry("rolled back")).await?;
    println!(
        "inside the open transaction: {}",
        heard(&mut watcher, Duration::from_millis(500)).await
    );
    client.batch_execute("ROLLBACK").await?;
    println!(
        "after ROLLBACK: {}",
        heard(&mut watcher, Duration::from_millis(500)).await
    );
    client.batch_execute("BEGIN").await?;
    client.batch_execute(&raw_entry("committed")).await?;
    println!(
        "inside the open transaction: {}",
        heard(&mut watcher, Duration::from_millis(500)).await
    );
    client.batch_execute("COMMIT").await?;
    println!(
        "after COMMIT: {}",
        heard(&mut watcher, Duration::from_secs(5)).await
    );
    let refused = ledger
        .post_entry(
            &manager,
            COMPANY,
            &entry(
                "Unbalanced",
                vec![
                    NewLine::debit(cash, money("1.00")),
                    NewLine::credit(sales, money("2.00")),
                ],
            ),
            &manual,
        )
        .await;
    println!(
        "a refused post: {}",
        if refused.is_err() {
            "refused"
        } else {
            "accepted (!)"
        }
    );
    println!(
        "after the refused post: {}",
        heard(&mut watcher, Duration::from_millis(500)).await
    );
    let posted = ledger
        .post_entry(&manager, COMPANY, &simple("Announced"), &manual)
        .await?;
    println!("manager posts entry {}", posted.entry_number);
    println!(
        "after the post: {}",
        heard(&mut watcher, Duration::from_secs(5)).await
    );
    println!(
        "company 2's subscriber, meanwhile: {}",
        heard(&mut neighbour, Duration::from_millis(500)).await
    );
    let changes: Vec<(i64, bool)> = client
        .query(
            "SELECT change_number,controlled FROM finance_ledger_bookkeeping_changes WHERE company_id=1 ORDER BY change_number",
            &[],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    println!("company 1's bookkeeping changes: {changes:?}");
    let audited: i64 = client
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'finance.ledger.%'",
            &[],
        )
        .await?
        .get(0);
    println!("audit mutations recorded for finance.ledger.*: {audited}");
    bilibi_finance_walkthrough_fixture::attachments::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::payables::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::payments::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::matching::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::periods::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::controls::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::totals::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::reports::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::reports::earnings_split(&db, &services).await?;

    einvoice::prove(
        &db,
        &services,
        &einvoice::Actors {
            manager: &manager,
            viewer: &viewer,
            contractor: &contractor,
            other_tenant: &other,
        },
    )
    .await?;
    bilibi_finance_walkthrough_fixture::payroll::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::receivables_documents::run(&db, &services).await?;

    bilibi_finance_walkthrough_fixture::clearance::run(&db, &services, &manager, &viewer, &admin)
        .await?;
    bilibi_finance_walkthrough_fixture::reconciliation::run(&db, &services).await?;

    // NC2: reconciliation matching, reconciliations and statement-line posting.
    bilibi_finance_walkthrough_fixture::reconciliation_matching::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::adopter_setup::reports(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::charts::run(&db, &services).await?;
    bilibi_finance_walkthrough_fixture::chart_templates::run(&db, &services).await?;
    Ok(())
}
