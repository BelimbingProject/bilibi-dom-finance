//! The periods walkthrough: Company 002's fiscal years and their closing,
//! controlled bookkeeping and late recordings, in a disposable database.
//! Company 002's book is opened by the ledger walkthrough before this runs.
//! Dates are counted back from today, so the late-recording report has
//! entries recorded late whatever day it runs. Every line is the facade's
//! actual answer or the database's refusal; nothing here is a test double.
use crate::sign_in;
use bilibi_database::Db;
use bilibi_finance_ledger::{
    AccountChange, AccountType, ClosingEvent, Entry, EntryQuery, FiscalYear, LateRecording,
    LateRecordingQuery, LateStatus, Ledger, LedgerError, Money, NewAccount, NewEntry,
    NewFiscalYear, NewLine, Period, PostOptions, YearClose, SERVICE,
};
use bilibi_module_registry::Services;
use jiff::{civil::Date, ToSpan, Zoned};
use std::fmt::Debug;
use tokio_postgres::Client;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// The company whose periods this walkthrough closes.
const COMPANY: i64 = 2;

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
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

/// A year on one line: dates, status, closing entry and each period's
/// number and status.
fn year(year: &FiscalYear) -> String {
    let periods: Vec<String> = year
        .periods
        .iter()
        .map(|p| format!("{}:{}", p.period_number, p.status.as_str()))
        .collect();
    format!(
        "year {} {}..{} {} closing_entry={:?} periods [{}]",
        year.id,
        year.start_date,
        year.end_date,
        year.status.as_str(),
        year.closing_entry_id,
        periods.join(" ")
    )
}

fn period(period: &Period) -> String {
    format!(
        "period {} #{} {}..{} {}",
        period.id,
        period.period_number,
        period.start_date,
        period.end_date,
        period.status.as_str()
    )
}

fn event(event: &ClosingEvent) -> String {
    format!(
        "#{} {} {} reason={:?} entry={:?} retained_earnings={:?} by {}",
        event.event_number,
        event
            .period_id
            .map_or("year".to_owned(), |id| format!("period {id}")),
        event.action.as_str(),
        event.reason,
        event.entry_id,
        event.retained_earnings_account_id,
        event.recorded_by_user_id
    )
}

fn entry(entry: &Entry) -> String {
    let lines: Vec<String> = entry
        .lines
        .iter()
        .map(|line| {
            if line.debit.is_zero() {
                format!("{}:-{}", line.account_id, line.credit)
            } else {
                format!("{}:{}", line.account_id, line.debit)
            }
        })
        .collect();
    format!(
        "#{} {} {:?} reverses={:?} [{}]",
        entry.entry_number,
        entry.entry_date,
        entry.description,
        entry.reverses_entry_id,
        lines.join(" ")
    )
}

fn late(late: &LateRecording) -> String {
    format!(
        "#{} dated {} days={} {:?} posted_by={} review={:?}",
        late.entry_number,
        late.entry_date,
        late.days,
        late.status,
        late.posted_by_user_id,
        late.review
            .as_ref()
            .map(|r| (&r.reason, r.reviewed_by_user_id))
    )
}

/// Raw SQL from "another client": the database's guards hold without the facade.
async fn raw(client: &Client, label: &str, sql: &str) {
    let outcome = client.batch_execute(sql).await;
    // A refusal inside an explicit BEGIN leaves the session aborted.
    let _ = client.batch_execute("ROLLBACK").await;
    match outcome {
        Ok(()) => println!("{label}: accepted (!)"),
        Err(error) => println!(
            "{label}: refused ({})",
            error
                .as_db_error()
                .map_or_else(|| error.to_string(), |d| d.message().to_owned())
        ),
    }
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let ledger = services.get::<Ledger>(SERVICE)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let contractor = sign_in(db, services, 1, 7).await?;
    let book = ledger.get_book(&manager, COMPANY).await?;
    let money = |text: &str| Money::parse(text, book.minor_units).expect("fixture amount");

    let today = Zoned::now().date();
    let month = today.first_of_month();
    let first_start = month.checked_sub(18.months())?;
    let second_start = month.checked_sub(6.months())?;
    let account = |code: &'static str, name: &'static str, account_type| {
        NewAccount::new(code, name, account_type)
    };
    let mut ids = Vec::new();
    for new in [
        account("9001", "Bank", AccountType::Asset),
        account("9400", "Fees earned", AccountType::Income),
        account("9500", "Rent", AccountType::Expense),
        account("9300", "Accumulated profit", AccountType::Equity),
    ] {
        ids.push(ledger.create_account(&manager, COMPANY, &new).await?.id);
    }
    let [bank, fees, rent, accumulated] = ids[..] else {
        return Err("four accounts".into());
    };
    let simple = |date: Date, note: &str| NewEntry {
        entry_date: date,
        description: note.into(),
        reference: None,
        lines: vec![
            NewLine::debit(bank, money("10.00")),
            NewLine::credit(fees, money("10.00")),
        ],
    };
    let manual = PostOptions::default();

    println!("\n== Fiscal years (company 2, controlled since its book opened)");
    show(
        "manager reads company 2's controlled bookkeeping",
        ledger.get_controlled_bookkeeping(&manager, COMPANY).await,
    );
    show(
        "manager posts into company 2 before it has a fiscal year",
        ledger
            .post_entry(&manager, COMPANY, &simple(today, "No year yet"), &manual)
            .await
            .map(|e| entry(&e)),
    );
    show(
        "viewer (no affiliation with company 2) opens a fiscal year",
        ledger
            .open_fiscal_year(&viewer, COMPANY, &NewFiscalYear::default())
            .await
            .map(|y| year(&y)),
    );
    show(
        "contractor (staff: no periods.manage) opens a fiscal year",
        ledger
            .open_fiscal_year(&contractor, COMPANY, &NewFiscalYear::default())
            .await
            .map(|y| year(&y)),
    );
    show(
        "admin opens a fiscal year for company 4, whose book is not open",
        ledger
            .open_fiscal_year(&admin, 4, &NewFiscalYear::default())
            .await
            .map(|y| year(&y)),
    );
    for (step, new) in [
        ("with no dates", NewFiscalYear::default()),
        (
            "starting mid-month",
            NewFiscalYear {
                start_date: Some(first_start.checked_add(14.days())?),
                end_date: None,
            },
        ),
        (
            "ending mid-month",
            NewFiscalYear {
                start_date: Some(first_start),
                end_date: Some(first_start.checked_add(3.months())?),
            },
        ),
        (
            "running 25 months",
            NewFiscalYear {
                start_date: Some(first_start),
                end_date: Some(first_start.checked_add(25.months())?.yesterday()?),
            },
        ),
        (
            "ending before it starts",
            NewFiscalYear {
                start_date: Some(first_start),
                end_date: Some(first_start.checked_sub(1.month())?.last_of_month()),
            },
        ),
    ] {
        show(
            &format!("manager opens the first fiscal year {step}"),
            ledger
                .open_fiscal_year(&manager, COMPANY, &new)
                .await
                .map(|y| year(&y)),
        );
    }
    let first = ledger
        .open_fiscal_year(
            &manager,
            COMPANY,
            &NewFiscalYear {
                start_date: Some(first_start),
                end_date: None,
            },
        )
        .await?;
    println!(
        "manager opens the first fiscal year from {first_start} (end defaults to twelve months on): {}",
        year(&first)
    );
    show(
        "manager opens the next year starting a month late",
        ledger
            .open_fiscal_year(
                &manager,
                COMPANY,
                &NewFiscalYear {
                    start_date: Some(second_start.checked_add(1.month())?),
                    end_date: None,
                },
            )
            .await
            .map(|y| year(&y)),
    );
    let second = ledger
        .open_fiscal_year(&manager, COMPANY, &NewFiscalYear::default())
        .await?;
    println!(
        "manager opens the next year with no dates (starts the day after the first ends): {}",
        year(&second)
    );
    show(
        "manager reads controlled bookkeeping again",
        ledger
            .get_controlled_bookkeeping(&manager, COMPANY)
            .await
            .map(|c| (c.controlled, c.fiscal_years)),
    );
    show(
        "viewer lists company 2's fiscal years",
        ledger
            .list_fiscal_years(&viewer, COMPANY)
            .await
            .map(|y| y.len()),
    );
    show(
        "manager reads a fiscal year of another company",
        ledger
            .get_fiscal_year(&manager, 1, first.id)
            .await
            .map(|y| year(&y)),
    );
    show(
        "manager posts the day before the first fiscal year",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(first_start.yesterday()?, "Too early"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager posts the day after the last fiscal year",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(second.end_date.tomorrow()?, "Too late"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    let p = |n: usize| first.periods[n - 1].clone();
    for (date, note, lines) in [
        (
            p(2).start_date,
            "Fees invoiced",
            vec![
                NewLine::debit(bank, money("500.00")),
                NewLine::credit(fees, money("500.00")),
            ],
        ),
        (
            p(3).start_date,
            "Rent paid",
            vec![
                NewLine::debit(rent, money("200.00")),
                NewLine::credit(bank, money("200.00")),
            ],
        ),
    ] {
        show(
            &format!("manager posts {note:?} on {date}"),
            ledger
                .post_entry(
                    &manager,
                    COMPANY,
                    &NewEntry {
                        entry_date: date,
                        description: note.into(),
                        reference: None,
                        lines,
                    },
                    &manual,
                )
                .await
                .map(|e| entry(&e)),
        );
    }

    println!("\n-- Closing, locking and reopening periods");
    show(
        "manager closes period 2 while period 1 is open",
        ledger
            .close_period(&manager, COMPANY, p(2).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "contractor closes period 1",
        ledger
            .close_period(&contractor, COMPANY, p(1).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager closes period 1 with a 501-character reason",
        ledger
            .close_period(&manager, COMPANY, p(1).id, Some(&"x".repeat(501)))
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager closes period 1",
        ledger
            .close_period(&manager, COMPANY, p(1).id, Some("  Month end  "))
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager closes period 1 again",
        ledger
            .close_period(&manager, COMPANY, p(1).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager closes a period id of no company 2 year",
        ledger
            .close_period(&manager, COMPANY, i64::MAX, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager posts into closed period 1",
        ledger
            .post_entry(&manager, COMPANY, &simple(p(1).end_date, "Late"), &manual)
            .await
            .map(|e| entry(&e)),
    );
    for n in 2..=11 {
        ledger
            .close_period(&manager, COMPANY, p(n).id, None)
            .await?;
    }
    println!("manager closes periods 2 to 11 in order");
    show(
        "manager closes period 12, the year's last",
        ledger
            .close_period(&manager, COMPANY, p(12).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager locks period 2 before period 1",
        ledger
            .lock_period(&manager, COMPANY, p(2).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager locks open period 12",
        ledger
            .lock_period(&manager, COMPANY, p(12).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "contractor locks period 1",
        ledger
            .lock_period(&contractor, COMPANY, p(1).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager locks period 1",
        ledger
            .lock_period(&manager, COMPANY, p(1).id, Some("Audited"))
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager locks period 1 again",
        ledger
            .lock_period(&manager, COMPANY, p(1).id, None)
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager posts into locked period 1",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(p(1).start_date, "Locked"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reopens locked period 1",
        ledger
            .reopen_period(&manager, COMPANY, p(1).id, "Correction")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager reopens open period 12",
        ledger
            .reopen_period(&manager, COMPANY, p(12).id, "Correction")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager reopens period 10 while period 11 is closed",
        ledger
            .reopen_period(&manager, COMPANY, p(10).id, "Correction")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager reopens period 11 with a blank reason",
        ledger
            .reopen_period(&manager, COMPANY, p(11).id, "   ")
            .await
            .map(|x| period(&x)),
    );
    show(
        "contractor (no periods.reopen) reopens period 11",
        ledger
            .reopen_period(&contractor, COMPANY, p(11).id, "Correction")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager reopens period 11 with a reason",
        ledger
            .reopen_period(&manager, COMPANY, p(11).id, "Supplier invoice missed")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager posts into reopened period 11",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    entry_date: p(11).start_date,
                    description: "Rent missed".into(),
                    reference: None,
                    lines: vec![
                        NewLine::debit(rent, money("50.00")),
                        NewLine::credit(bank, money("50.00")),
                    ],
                },
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager closes period 11 again",
        ledger
            .close_period(&manager, COMPANY, p(11).id, None)
            .await
            .map(|x| period(&x)),
    );

    println!("\n-- Year-end close");
    show(
        "manager closes the second year while the first is open",
        ledger
            .close_fiscal_year(&manager, COMPANY, second.id, &YearClose::default())
            .await
            .map(|y| year(&y)),
    );
    show(
        "manager closes the first year with no retained-earnings account designated",
        ledger
            .close_fiscal_year(&manager, COMPANY, first.id, &YearClose::default())
            .await
            .map(|y| year(&y)),
    );
    ledger
        .update_account(
            &manager,
            COMPANY,
            accumulated,
            &AccountChange {
                retained_earnings: Some(true),
                ..AccountChange::default()
            },
        )
        .await?;
    println!("manager designates 9300 for retained earnings");
    show(
        "contractor (no periods.manage, no entries.post) closes the first year",
        ledger
            .close_fiscal_year(&contractor, COMPANY, first.id, &YearClose::default())
            .await
            .map(|y| year(&y)),
    );
    show(
        "manager closes a fiscal year id of another company",
        ledger
            .close_fiscal_year(&manager, 1, first.id, &YearClose::default())
            .await
            .map(|y| year(&y)),
    );
    show(
        "manager closes the first year with a 501-character description",
        ledger
            .close_fiscal_year(
                &manager,
                COMPANY,
                first.id,
                &YearClose {
                    description: Some("x".repeat(501)),
                    reference: None,
                },
            )
            .await
            .map(|y| year(&y)),
    );
    let closed = ledger
        .close_fiscal_year(
            &manager,
            COMPANY,
            first.id,
            &YearClose {
                description: None,
                reference: Some("YE".into()),
            },
        )
        .await?;
    println!("manager closes the first year: {}", year(&closed));
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let default_month = registry
        .call(
            &mut **db.get().await?,
            &manager,
            "finance.ledger.book.get",
            &serde_json::json!({"company_id":COMPANY}),
        )
        .await?["result"]["latest_entry_date"]
        .as_str()
        .map(str::to_owned);
    let real = ledger
        .list_entries(
            &manager,
            COMPANY,
            &EntryQuery {
                limit: Some(500),
                exclude_closing: true,
                ..EntryQuery::default()
            },
        )
        .await?;
    let latest_real = real.iter().map(|e| e.entry_date.to_string()).max();
    let closing_date = match closed.closing_entry_id {
        Some(id) => Some(ledger.get_entry(&manager, COMPANY, id).await?.entry_date),
        None => None,
    };
    if default_month != latest_real {
        return Err(format!(
            "reports would open on {default_month:?}, not the latest posting {latest_real:?}"
        )
        .into());
    }
    println!(
        "reports open on the month of {default_month:?}, the latest posting; the year-end closing entry dated {closing_date:?} does not move it"
    );
    if let Some(id) = closed.closing_entry_id {
        show(
            "the closing entry (income and expense to zero, profit to 9300)",
            ledger
                .get_entry(&manager, COMPANY, id)
                .await
                .map(|e| entry(&e)),
        );
    }
    show(
        "manager closes the first year again (unchanged)",
        ledger
            .close_fiscal_year(&manager, COMPANY, first.id, &YearClose::default())
            .await
            .map(|y| y == closed),
    );
    show(
        "manager reopens period 11 of the closed year",
        ledger
            .reopen_period(&manager, COMPANY, p(11).id, "Correction")
            .await
            .map(|x| period(&x)),
    );
    show(
        "manager reopens the open second year",
        ledger
            .reopen_fiscal_year(&manager, COMPANY, second.id, "Correction")
            .await
            .map(|y| year(&y)),
    );
    show(
        "manager reopens the first year with no reason",
        ledger
            .reopen_fiscal_year(&manager, COMPANY, first.id, "")
            .await
            .map(|y| year(&y)),
    );
    ledger
        .close_period(&manager, COMPANY, second.periods[0].id, None)
        .await?;
    println!("manager closes the second year's period 1");
    show(
        "manager reopens the first year while the next period is closed",
        ledger
            .reopen_fiscal_year(&manager, COMPANY, first.id, "Correction")
            .await
            .map(|y| year(&y)),
    );
    ledger
        .reopen_period(&manager, COMPANY, second.periods[0].id, "Wrong month")
        .await?;
    println!("manager reopens the second year's period 1");
    let reopened = ledger
        .reopen_fiscal_year(&manager, COMPANY, first.id, "Audit adjustment")
        .await?;
    println!("manager reopens the first year: {}", year(&reopened));
    let all = ledger
        .list_entries(
            &manager,
            COMPANY,
            &EntryQuery {
                from: Some(first.end_date),
                to: Some(first.end_date),
                ..EntryQuery::default()
            },
        )
        .await?;
    for posted in &all {
        println!("  entry on the year's last day: {}", entry(posted));
    }
    show(
        "manager posts an audit adjustment into the reopened period 12",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    entry_date: p(12).start_date,
                    description: "Audit adjustment".into(),
                    reference: None,
                    lines: vec![
                        NewLine::debit(bank, money("25.00")),
                        NewLine::credit(fees, money("25.00")),
                    ],
                },
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    let again = ledger
        .close_fiscal_year(&manager, COMPANY, first.id, &YearClose::default())
        .await?;
    println!("manager closes the first year again: {}", year(&again));
    if let Some(id) = again.closing_entry_id {
        show(
            "the new closing entry",
            ledger
                .get_entry(&manager, COMPANY, id)
                .await
                .map(|e| entry(&e)),
        );
    }
    for n in 2..=12 {
        ledger.lock_period(&manager, COMPANY, p(n).id, None).await?;
    }
    println!("manager locks periods 2 to 12 in order");
    show(
        "manager reopens the first year, whose last period is locked",
        ledger
            .reopen_fiscal_year(&manager, COMPANY, first.id, "Correction")
            .await
            .map(|y| year(&y)),
    );
    show(
        "viewer reads the first year's closing log",
        ledger
            .list_closing_events(&viewer, COMPANY, first.id)
            .await
            .map(|events| events.len()),
    );
    for logged in ledger
        .list_closing_events(&manager, COMPANY, first.id)
        .await?
    {
        println!("  closing log: {}", event(&logged));
    }
    show(
        "manager reads the closing log of a fiscal year id of another company",
        ledger
            .list_closing_events(&manager, 1, first.id)
            .await
            .map(|events| events.len()),
    );
    let everything = ledger
        .list_entries(&manager, COMPANY, &EntryQuery::default())
        .await?;
    let without = ledger
        .list_entries(
            &manager,
            COMPANY,
            &EntryQuery {
                exclude_closing: true,
                ..EntryQuery::default()
            },
        )
        .await?;
    println!(
        "company 2 has {} entries, {} without closing entries and their reversals",
        everything.len(),
        without.len()
    );

    println!("\n== Controlled bookkeeping");
    let beyond = second.end_date.tomorrow()?;
    show(
        "contractor turns controlled bookkeeping off",
        ledger
            .set_controlled_bookkeeping(&contractor, COMPANY, false, Some("Catch-up"))
            .await
            .map(|c| c.controlled),
    );
    show(
        "manager turns it off with no reason",
        ledger
            .set_controlled_bookkeeping(&manager, COMPANY, false, Some("  "))
            .await
            .map(|c| c.controlled),
    );
    show(
        "manager turns it on while it is on (records nothing)",
        ledger
            .set_controlled_bookkeeping(&manager, COMPANY, true, None)
            .await
            .map(|c| c.change.map(|c| c.change_number)),
    );
    show(
        "manager turns it off with a reason",
        ledger
            .set_controlled_bookkeeping(&manager, COMPANY, false, Some("  Migrating history  "))
            .await
            .map(|c| (c.controlled, c.change.map(|c| (c.change_number, c.reason)))),
    );
    show(
        "manager posts the day after the last fiscal year, uncontrolled",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(beyond, "Beyond the years"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager posts before the first fiscal year, uncontrolled",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(first_start.yesterday()?, "Before the years"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager turns it on again",
        ledger
            .set_controlled_bookkeeping(&manager, COMPANY, true, None)
            .await
            .map(|c| (c.controlled, c.change.map(|c| c.change_number))),
    );
    show(
        "manager posts the day after the last fiscal year, controlled",
        ledger
            .post_entry(&manager, COMPANY, &simple(beyond, "Beyond again"), &manual)
            .await
            .map(|e| entry(&e)),
    );
    show(
        "viewer (no affiliation) lists company 2's bookkeeping changes",
        ledger
            .list_controlled_bookkeeping_changes(&viewer, COMPANY)
            .await
            .map(|c| c.len()),
    );
    for change in ledger
        .list_controlled_bookkeeping_changes(&manager, COMPANY)
        .await?
    {
        println!(
            "  change #{} controlled={} reason={:?} by {}",
            change.change_number, change.controlled, change.reason, change.changed_by_user_id
        );
    }
    let exceptions = db
        .get()
        .await?
        .query(
            "SELECT exception_number,action,subject_type,subject,reason,approved_by_user_id \
             FROM finance_ledger_approval_exceptions WHERE company_id=$1 ORDER BY exception_number",
            &[&COMPANY],
        )
        .await?;
    for row in &exceptions {
        println!(
            "  approval exception #{} {} {} {:?} reason={:?} by {}",
            row.get::<_, i64>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3),
            row.get::<_, String>(4),
            row.get::<_, i64>(5)
        );
    }

    println!("\n== Late recordings");
    println!("thresholds: {:?}", Ledger::late_recording_thresholds());
    let mut late_ids = Vec::new();
    for (days, note) in [(50, "Approaching"), (70, "Late")] {
        let posted = ledger
            .post_entry(
                &manager,
                COMPANY,
                &simple(today.checked_sub(days.days())?, note),
                &manual,
            )
            .await?;
        println!(
            "manager posts {note:?} dated {days} days ago: {}",
            entry(&posted)
        );
        late_ids.push(posted.id);
    }
    let recent = ledger
        .post_entry(&admin, COMPANY, &simple(today, "On time"), &manual)
        .await?;
    println!("admin posts an entry dated today: {}", entry(&recent));
    for (step, query) in [
        ("all", LateRecordingQuery::default()),
        (
            "late",
            LateRecordingQuery {
                status: Some(LateStatus::Late),
                ..LateRecordingQuery::default()
            },
        ),
        (
            "approaching",
            LateRecordingQuery {
                status: Some(LateStatus::Approaching),
                ..LateRecordingQuery::default()
            },
        ),
    ] {
        match ledger.list_late_recordings(&manager, COMPANY, &query).await {
            Ok(listed) => {
                println!("late recordings ({step}): {}", listed.len());
                for listed in listed {
                    println!("  {}", late(&listed));
                }
            }
            Err(error) => println!("late recordings ({step}): refused ({error:?})"),
        }
    }
    show(
        "manager lists 501 late recordings",
        ledger
            .list_late_recordings(
                &manager,
                COMPANY,
                &LateRecordingQuery {
                    limit: Some(501),
                    ..LateRecordingQuery::default()
                },
            )
            .await
            .map(|l| l.len()),
    );
    let [approaching, overdue] = late_ids[..] else {
        return Err("two late entries".into());
    };
    show(
        "manager reviews the late entry they posted",
        ledger
            .record_late_recording_review(&manager, COMPANY, overdue, "Explained")
            .await,
    );
    show(
        "admin reviews the entry recorded today",
        ledger
            .record_late_recording_review(&admin, COMPANY, recent.id, "Explained")
            .await,
    );
    if let Some(id) = again.closing_entry_id {
        show(
            "admin reviews the year's closing entry",
            ledger
                .record_late_recording_review(&admin, COMPANY, id, "Explained")
                .await,
        );
    }
    show(
        "contractor (no late-recordings.review) reviews the late entry",
        ledger
            .record_late_recording_review(&contractor, COMPANY, overdue, "Explained")
            .await,
    );
    show(
        "admin reviews the late entry with a blank reason",
        ledger
            .record_late_recording_review(&admin, COMPANY, overdue, "  ")
            .await,
    );
    show(
        "admin reviews the late entry",
        ledger
            .record_late_recording_review(
                &admin,
                COMPANY,
                overdue,
                "  Invoice arrived after the month end  ",
            )
            .await
            .map(|r| (r.entry_id, r.reason, r.reviewed_by_user_id)),
    );
    show(
        "admin reviews it again",
        ledger
            .record_late_recording_review(&admin, COMPANY, overdue, "Again")
            .await,
    );
    show(
        "admin reviews the approaching entry",
        ledger
            .record_late_recording_review(&admin, COMPANY, approaching, "Bank statement delay")
            .await
            .map(|r| r.entry_id),
    );
    show(
        "manager lists reviewed late recordings",
        ledger
            .list_late_recordings(
                &manager,
                COMPANY,
                &LateRecordingQuery {
                    reviewed: Some(true),
                    ..LateRecordingQuery::default()
                },
            )
            .await
            .map(|l| l.iter().map(late).collect::<Vec<_>>()),
    );
    show(
        "manager lists unreviewed late recordings after the first",
        ledger
            .list_late_recordings(
                &manager,
                COMPANY,
                &LateRecordingQuery {
                    reviewed: Some(false),
                    after: Some(1),
                    ..LateRecordingQuery::default()
                },
            )
            .await
            .map(|l| l.iter().map(|x| x.entry_number).collect::<Vec<_>>()),
    );

    println!("\n-- A close racing posts into the same period");
    let racing = second.periods[0].clone();
    let mut tasks = Vec::new();
    for n in 0..6 {
        let (ledger, manager) = (ledger.clone(), manager.clone());
        let new = simple(racing.start_date, &format!("Racing {n}"));
        tasks.push(tokio::spawn(async move {
            ledger
                .post_entry(&manager, COMPANY, &new, &PostOptions::default())
                .await
                .map(|_| ())
        }));
    }
    let closing = {
        let (ledger, manager) = (ledger.clone(), manager.clone());
        tokio::spawn(async move {
            ledger
                .close_period(&manager, COMPANY, racing.id, Some("Racing close"))
                .await
                .map(|_| ())
        })
    };
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(match task.await? {
            Ok(()) => "posted".to_owned(),
            Err(LedgerError::Invalid(errors)) => errors.first().map_or("refused".into(), |e| {
                format!("refused: {} {}", e.field, e.message)
            }),
            Err(error) => format!("refused: {error:?}"),
        });
    }
    println!("six concurrent posts: {outcomes:?}");
    println!("the concurrent close: {:?}", closing.await?);
    let after = db.get().await?
        .query_one(
            "SELECT count(*) FROM finance_ledger_entries e WHERE e.company_id=$1 AND e.entry_date=$2::text::date \
             AND e.posted_at > (SELECT recorded_at FROM finance_ledger_closing_events WHERE period_id=$3 \
             ORDER BY event_number DESC LIMIT 1)",
            &[&COMPANY, &second.periods[0].start_date.to_string(), &second.periods[0].id],
        )
        .await?;
    println!(
        "entries in the period posted after its close was recorded: {}",
        after.get::<_, i64>(0)
    );

    println!("\n== The database's own closing refusals");
    let year_id = first.id;
    let period_one = p(1).id;
    let late_entry = overdue;
    for (step, sql) in [
        (
            "UPDATE a fiscal year's end",
            format!(
                "UPDATE finance_ledger_fiscal_years SET end_date=end_date+1 WHERE id={year_id}"
            ),
        ),
        (
            "DELETE a period",
            format!("DELETE FROM finance_ledger_periods WHERE id={period_one}"),
        ),
        (
            "DELETE the closing log",
            format!("DELETE FROM finance_ledger_closing_events WHERE company_id={COMPANY}"),
        ),
        (
            "TRUNCATE the closing log",
            "TRUNCATE finance_ledger_closing_events".into(),
        ),
        (
            "INSERT a period into a fiscal year opened earlier",
            format!(
                "INSERT INTO finance_ledger_periods(tenant_id,company_id,fiscal_year_id,period_number,start_date,end_date) \
                 VALUES(1,{COMPANY},{year_id},13,'{}','{}')",
                first.end_date.tomorrow()?,
                first.end_date.tomorrow()?.last_of_month()
            ),
        ),
        (
            "INSERT a fiscal year that leaves a gap",
            format!(
                "INSERT INTO finance_ledger_fiscal_years(tenant_id,company_id,start_date,end_date,opened_by_user_id,opened_at) \
                 VALUES(1,{COMPANY},'{}','{}',1,now())",
                second
                    .end_date
                    .checked_add(1.month())?
                    .first_of_month()
                    .checked_add(1.month())?,
                second.end_date.checked_add(2.months())?.last_of_month()
            ),
        ),
        (
            "INSERT a fiscal year with no periods",
            format!(
                "INSERT INTO finance_ledger_fiscal_years(tenant_id,company_id,start_date,end_date,opened_by_user_id,opened_at) \
                 VALUES(1,{COMPANY},'{}','{}',1,now())",
                second.end_date.tomorrow()?,
                second.end_date.tomorrow()?.last_of_month()
            ),
        ),
        (
            "INSERT a reopen of a locked period",
            format!(
                "INSERT INTO finance_ledger_closing_events(tenant_id,company_id,event_number,fiscal_year_id,period_id,action,reason,recorded_by_user_id,recorded_at) \
                 VALUES(1,{COMPANY},0,{year_id},{period_one},'reopen','Raw',1,now())"
            ),
        ),
        (
            "INSERT a reopen with a blank reason",
            format!(
                "BEGIN; INSERT INTO finance_ledger_closing_events(tenant_id,company_id,event_number,fiscal_year_id,period_id,action,recorded_by_user_id,recorded_at) \
                 VALUES(1,{COMPANY},0,{},{},'close',1,now()); \
                 INSERT INTO finance_ledger_closing_events(tenant_id,company_id,event_number,fiscal_year_id,period_id,action,reason,recorded_by_user_id,recorded_at) \
                 VALUES(1,{COMPANY},0,{},{},'reopen','  ',1,now()); COMMIT",
                second.id, second.periods[1].id, second.id, second.periods[1].id
            ),
        ),
        (
            "INSERT an entry reversing another company's entry",
            format!(
                "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,reverses_entry_id,posted_by_user_id,posted_at) \
                 SELECT 1,1,0,'2026-10-01','Cross reversal',b.currency_code,e.id,1,now() FROM finance_ledger_entries e, finance_ledger_books b \
                 WHERE e.company_id={COMPANY} AND b.company_id=1 LIMIT 1"
            ),
        ),
        (
            "INSERT a close of a closed year",
            format!(
                "INSERT INTO finance_ledger_closing_events(tenant_id,company_id,event_number,fiscal_year_id,action,recorded_by_user_id,recorded_at) \
                 VALUES(1,{COMPANY},0,{year_id},'close',1,now())"
            ),
        ),
        (
            "INSERT an entry into a locked period",
            format!(
                "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
                 VALUES(1,{COMPANY},0,'{}','Raw','{}',1,now())",
                first.start_date, book.currency_code
            ),
        ),
        (
            "INSERT an entry after the last fiscal year of a controlled company",
            format!(
                "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at) \
                 VALUES(1,{COMPANY},0,'{beyond}','Raw','{}',1,now())",
                book.currency_code
            ),
        ),
        (
            "INSERT a second review of a late entry",
            format!(
                "INSERT INTO finance_ledger_late_recording_reviews(tenant_id,company_id,entry_id,reason,reviewed_by_user_id,reviewed_at) \
                 VALUES(1,{COMPANY},{late_entry},'Again',4,now())"
            ),
        ),
        (
            "INSERT a review by the entry's poster",
            format!(
                "INSERT INTO finance_ledger_late_recording_reviews(tenant_id,company_id,entry_id,reason,reviewed_by_user_id,reviewed_at) \
                 SELECT 1,{COMPANY},id,'Mine',1,now() FROM finance_ledger_entries WHERE company_id={COMPANY} AND description='Fees invoiced'"
            ),
        ),
        (
            "INSERT a review of an entry recorded on time",
            format!(
                "INSERT INTO finance_ledger_late_recording_reviews(tenant_id,company_id,entry_id,reason,reviewed_by_user_id,reviewed_at) \
                 VALUES(1,{COMPANY},{},'On time',1,now())",
                recent.id
            ),
        ),
        (
            "UPDATE a late-recording review",
            format!(
                "UPDATE finance_ledger_late_recording_reviews SET reason='changed' WHERE entry_id={late_entry}"
            ),
        ),
        (
            "DELETE an approval exception",
            format!("DELETE FROM finance_ledger_approval_exceptions WHERE company_id={COMPANY}"),
        ),
        (
            "INSERT a bookkeeping change turning off with no reason",
            format!(
                "INSERT INTO finance_ledger_bookkeeping_changes(tenant_id,company_id,change_number,controlled,changed_by_user_id,changed_at) \
                 VALUES(1,{COMPANY},NULL,false,1,NULL)"
            ),
        ),
    ] {
        raw(&**db.get().await?, step, &sql).await;
    }
    Ok(())
}
