//! The controls walkthrough: reversals, the separation of duties with its
//! approval exceptions and reviews, and manual journals, on Company 001 in a
//! disposable database, after the other walkthroughs. Company 001's first
//! fiscal year is 2026, so every date here is in it. The manager and the
//! admin are two independent people with every ledger capability; the admin
//! also acts as the manager through impersonation, which the separation of
//! duties counts. Every line is the facade's actual answer or the database's
//! refusal; nothing here is a test double.
use crate::{records, sign_in};
use bilibi_database::{Db, MutationBoundary};
use bilibi_finance_ledger::{
    AccountChange, AccountType, Approval, ApprovalException, ApprovalExceptionQuery, Entry,
    EntryOwner, Journal, JournalQuery, JournalStatus, Ledger, LedgerError, Money, NewAccount,
    NewEntry, NewLine, PostOptions, Preparer, Reversal, SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, Scope};
use jiff::{civil::date, Timestamp, ToSpan};
use std::fmt::Debug;
use tokio_postgres::Client;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// The company whose controls this walkthrough exercises.
const COMPANY: i64 = 1;
/// What the walkthrough's approvals name as their module: a specimen record
/// of the attachments walkthrough stands in for a sibling module's record.
const SOURCE: &str = "finance/walkthrough";

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
        "#{} {} {:?} owner={} reverses={:?} reversed_by={:?} posted_by={}/{:?} [{}]",
        entry.entry_number,
        entry.entry_date,
        entry.description,
        entry
            .owner
            .as_ref()
            .map_or("none".into(), |o| format!("{} {}", o.module, o.record)),
        entry.reverses_entry_id,
        entry.reversed_by_entry_id,
        entry.posted_by_user_id,
        entry.posted_by_impersonator_id,
        lines.join(" ")
    )
}

fn journal(journal: &Journal) -> String {
    format!(
        "journal #{} {} {:?} {} submitted_by={}/{:?} decision={:?} lines={}",
        journal.journal_number,
        journal.entry_date,
        journal.description,
        journal.status.as_str(),
        journal.submitted_by_user_id,
        journal.submitted_by_impersonator_id,
        journal.decision.as_ref().map(|d| (
            d.event.as_str(),
            d.reason.as_deref(),
            d.entry_id,
            d.actor_user_id,
            d.actor_impersonator_id
        )),
        journal.lines.len()
    )
}

fn exception(exception: &ApprovalException) -> String {
    format!(
        "exception #{} {} {} {} {} {:?} same_person={} override={} prepared_by={:?}/{:?} approved_by={}/{:?} \
         reason={:?} separation_change={:?} review={:?}",
        exception.exception_number,
        exception.source,
        exception.action,
        exception.subject_type,
        exception.subject_id,
        exception.subject,
        exception.same_person,
        exception.overrides,
        exception.prepared_by_user_id,
        exception.prepared_by_impersonator_id,
        exception.approved_by_user_id,
        exception.approved_by_impersonator_id,
        exception.reason,
        exception.separation_change_id,
        exception
            .review
            .as_ref()
            .map(|r| (r.note.as_deref(), r.reviewed_by_user_id))
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

/// What a sibling module's approval of one of its records asks
/// `check_approval`.
fn approval(record: i64, prepared_by: &[(i64, Option<i64>)], reason: Option<&str>) -> Approval {
    Approval {
        source: SOURCE.into(),
        action: if reason.is_some() {
            "record_override".into()
        } else {
            "record_approval".into()
        },
        subject_type: "specimen_record".into(),
        subject_id: record,
        subject: format!("Specimen record {record}"),
        prepared_by: prepared_by
            .iter()
            .map(|(user_id, impersonator_id)| Preparer {
                user_id: *user_id,
                impersonator_id: *impersonator_id,
            })
            .collect(),
        override_reason: reason.map(str::to_owned),
    }
}

/// A sibling's approval: `check_approval` inside the approving module's own
/// transaction, committed only when the ledger allows it.
async fn approve_record(
    db: &Db,
    mutations: &MutationBoundary,
    ledger: &Ledger,
    scope: &Scope,
    approval: &Approval,
) -> Result<(), LedgerError> {
    let mut client = db.get().await.map_err(|e| LedgerError::Failed(e.into()))?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("finance.walkthrough.approve_record"),
        )
        .await?;
    match ledger
        .check_approval(&mut tx, scope, COMPANY, approval)
        .await
    {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// An owner's correction: its own reversal record and the ledger's reversal
/// in one transaction, committed or rolled back together.
async fn owner_reverses(
    db: &Db,
    mutations: &MutationBoundary,
    ledger: &Ledger,
    scope: &Scope,
    entry_id: i64,
    owner: &EntryOwner,
    reversal: &Reversal,
    commit: bool,
) -> Result<Entry, LedgerError> {
    let mut client = db.get().await.map_err(|e| LedgerError::Failed(e.into()))?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("finance.walkthrough.reverse_payment"),
        )
        .await?;
    let reversed = ledger
        .reverse_owned_entry(&mut tx, scope, COMPANY, entry_id, owner, reversal)
        .await;
    if commit && reversed.is_ok() {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    reversed
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let ledger = services.get::<Ledger>(SERVICE)?;
    let mutations = services.get::<MutationBoundary>("base/database.mutations")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let contractor = sign_in(db, services, 1, 7).await?;
    // The admin acting as the manager: a session an operator entered through
    // impersonation, as the durable session row would name it.
    let admin_as_manager = {
        let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
        let client = db.get().await?;
        authentication
            .authenticate_session(&**client, 1, 1, Some(4), None, String::new())
            .await?
    };
    let client = db.get().await?;
    let book = ledger.get_book(&manager, COMPANY).await?;
    let money = |text: &str| Money::parse(text, book.minor_units).expect("fixture amount");
    let mut ids = Vec::new();
    for (code, name, account_type) in [
        ("7010", "Operating bank", AccountType::Asset),
        ("7400", "Service fees", AccountType::Income),
        ("7500", "Office rent", AccountType::Expense),
        ("7600", "Retired supplies", AccountType::Expense),
        ("7700", "Paused supplies", AccountType::Expense),
    ] {
        ids.push(
            ledger
                .create_account(
                    &manager,
                    COMPANY,
                    &NewAccount::new(code, name, account_type),
                )
                .await?
                .id,
        );
    }
    let [bank, fees, rent, retired, paused] = ids[..] else {
        return Err("five accounts".into());
    };
    let pair = |day, note: &str, debit: i64, credit: i64, amount: &str| NewEntry {
        entry_date: day,
        description: note.into(),
        reference: None,
        lines: vec![
            NewLine::debit(debit, money(amount)),
            NewLine::credit(credit, money(amount)),
        ],
    };
    let reversal = |day, note: &str| Reversal {
        entry_date: day,
        description: note.into(),
        reference: Some("REV".into()),
    };
    let manual = PostOptions::default();

    println!("\n== Reversals (company 1)");
    let original = ledger
        .post_entry(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 10), "June fees", bank, fees, "100.00"),
            &manual,
        )
        .await?;
    println!("manager posts {}", entry(&original));
    show(
        "viewer (view only) reverses it",
        ledger
            .reverse_entry(
                &viewer,
                COMPANY,
                original.id,
                &reversal(date(2026, 6, 20), "Wrong customer"),
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reverses it dated before the original, with a blank description",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                original.id,
                &reversal(date(2026, 6, 1), "  "),
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reverses entry 999999",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                999_999,
                &reversal(date(2026, 6, 20), "Missing"),
            )
            .await
            .map(|e| entry(&e)),
    );
    let reversed = ledger
        .reverse_entry(
            &manager,
            COMPANY,
            original.id,
            &reversal(date(2026, 6, 20), "Wrong customer"),
        )
        .await?;
    println!("manager reverses it: {}", entry(&reversed));
    show(
        "the original, read again",
        ledger
            .get_entry(&manager, COMPANY, original.id)
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reverses it again",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                original.id,
                &reversal(date(2026, 6, 21), "Again"),
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reverses the reversal",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                reversed.id,
                &reversal(date(2026, 6, 21), "Undo"),
            )
            .await
            .map(|e| entry(&e)),
    );
    let bought = ledger
        .post_entry(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 11), "Supplies bought", retired, bank, "40.00"),
            &manual,
        )
        .await?;
    let returned = ledger
        .post_entry(
            &manager,
            COMPANY,
            &pair(
                date(2026, 6, 12),
                "Supplies returned",
                bank,
                retired,
                "40.00",
            ),
            &manual,
        )
        .await?;
    ledger
        .update_account(
            &manager,
            COMPANY,
            retired,
            &AccountChange {
                active: Some(false),
                ..AccountChange::default()
            },
        )
        .await?;
    println!("manager buys and returns supplies on account 7600, then deactivates it");
    show(
        "manager posts onto deactivated account 7600",
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &pair(date(2026, 6, 13), "More supplies", retired, bank, "5.00"),
                &manual,
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reverses the purchase: a deactivated account takes its reversal",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                bought.id,
                &reversal(date(2026, 6, 14), "Purchase cancelled"),
            )
            .await
            .map(|e| entry(&e)),
    );
    if let Some(closing) = ledger
        .list_fiscal_years(&manager, 2)
        .await?
        .iter()
        .find_map(|year| year.closing_entry_id)
    {
        show(
            "manager reverses company 2's year-end closing entry",
            ledger
                .reverse_entry(
                    &manager,
                    2,
                    closing,
                    &reversal(date(2026, 6, 20), "Undo the close"),
                )
                .await
                .map(|e| entry(&e)),
        );
    }

    println!("\n== Owned entries: corrected only through their owner");
    let owner = EntryOwner {
        module: "finance/receivables".into(),
        record: "payment:301".into(),
    };
    let owned = ledger
        .post_entry(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 15), "Payment received", bank, fees, "75.00"),
            &PostOptions {
                owner: Some(owner.clone()),
                ..PostOptions::default()
            },
        )
        .await?;
    println!("manager posts {}", entry(&owned));
    show(
        "manager reverses the owned entry directly",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                owned.id,
                &reversal(date(2026, 6, 16), "Bounced"),
            )
            .await
            .map(|e| entry(&e)),
    );
    let other_record = EntryOwner {
        module: "finance/receivables".into(),
        record: "payment:302".into(),
    };
    let malformed = EntryOwner {
        module: "Receivables".into(),
        record: "payment".into(),
    };
    for (label, entry_id, claimed) in [
        (
            "the owner's correction names another record",
            owned.id,
            &other_record,
        ),
        (
            "the owner's correction names a malformed owner",
            owned.id,
            &malformed,
        ),
        (
            "the owner's correction names a manual entry",
            returned.id,
            &owner,
        ),
    ] {
        show(
            label,
            owner_reverses(
                db,
                &mutations,
                &ledger,
                &manager,
                entry_id,
                claimed,
                &reversal(date(2026, 6, 16), "Bounced"),
                true,
            )
            .await
            .map(|e| entry(&e)),
        );
    }
    show(
        "the owner reverses inside its transaction, then rolls its transaction back",
        owner_reverses(
            db,
            &mutations,
            &ledger,
            &manager,
            owned.id,
            &owner,
            &reversal(date(2026, 6, 16), "Bounced"),
            false,
        )
        .await
        .map(|e| entry(&e)),
    );
    show(
        "the owned entry after the rollback",
        ledger
            .get_entry(&manager, COMPANY, owned.id)
            .await
            .map(|e| entry(&e)),
    );
    show(
        "the owner reverses inside its transaction and commits",
        owner_reverses(
            db,
            &mutations,
            &ledger,
            &manager,
            owned.id,
            &owner,
            &reversal(date(2026, 6, 16), "Bounced"),
            true,
        )
        .await
        .map(|e| entry(&e)),
    );

    println!("\n== Separation of duties (company 1 has never changed it)");
    show(
        "manager reads company 1's separation of duties",
        ledger.get_duty_separation(&manager, COMPANY).await,
    );
    show(
        "contractor (staff: no duty-separation.manage) turns it off",
        ledger
            .set_duty_separation(&contractor, COMPANY, false, Some("Small team"))
            .await,
    );
    show(
        "manager turns it off with no reason",
        ledger
            .set_duty_separation(&manager, COMPANY, false, Some("   "))
            .await,
    );
    show(
        "manager turns it on while it is required",
        ledger
            .set_duty_separation(&manager, COMPANY, true, None)
            .await,
    );

    println!("\n== Manual journals");
    show(
        "manager submits an unbalanced journal",
        ledger
            .submit_journal(
                &manager,
                COMPANY,
                &NewEntry {
                    lines: vec![
                        NewLine::debit(rent, money("30.00")),
                        NewLine::credit(bank, money("20.00")),
                    ],
                    ..pair(date(2026, 6, 5), "June rent", rent, bank, "30.00")
                },
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "contractor (staff: no journals.submit) submits a journal",
        ledger
            .submit_journal(
                &contractor,
                COMPANY,
                &pair(date(2026, 6, 5), "June rent", rent, bank, "30.00"),
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager submits a journal onto deactivated account 7600",
        ledger
            .submit_journal(
                &manager,
                COMPANY,
                &pair(date(2026, 6, 5), "Old supplies", retired, bank, "3.00"),
            )
            .await
            .map(|j| journal(&j)),
    );
    let rent_journal = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 5), "June rent", rent, bank, "30.00"),
        )
        .await?;
    println!("manager submits {}", journal(&rent_journal));
    let impersonated = ledger
        .submit_journal(
            &admin_as_manager,
            COMPANY,
            &pair(date(2026, 6, 6), "Bank fee refund", bank, fees, "12.00"),
        )
        .await?;
    println!(
        "admin, impersonating the manager, submits {}",
        journal(&impersonated)
    );
    let rejected = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 7), "Duplicate rent", rent, bank, "30.00"),
        )
        .await?;
    let january = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 1, 15), "January accrual", rent, bank, "8.00"),
        )
        .await?;
    let paused_journal = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 8), "Paused supplies", paused, bank, "4.00"),
        )
        .await?;
    println!(
        "manager submits journals #{}, #{} and #{}",
        rejected.journal_number, january.journal_number, paused_journal.journal_number
    );
    show(
        "the submitted journals",
        ledger
            .list_journals(
                &manager,
                COMPANY,
                &JournalQuery {
                    status: Some(JournalStatus::Submitted),
                    ..JournalQuery::default()
                },
            )
            .await
            .map(|list| list.iter().map(journal).collect::<Vec<_>>()),
    );
    show(
        "manager approves the journal they submitted",
        ledger
            .approve_journal(&manager, COMPANY, rent_journal.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin, impersonating the manager, approves the manager's journal",
        ledger
            .approve_journal(&admin_as_manager, COMPANY, rent_journal.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin approves the journal admin submitted while impersonating the manager",
        ledger
            .approve_journal(&admin, COMPANY, impersonated.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "contractor (staff: no journals.approve) approves the manager's journal",
        ledger
            .approve_journal(&contractor, COMPANY, rent_journal.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin approves journal 999999",
        ledger
            .approve_journal(&admin, COMPANY, 999_999)
            .await
            .map(|j| journal(&j)),
    );
    let approved = ledger
        .approve_journal(&admin, COMPANY, rent_journal.id)
        .await?;
    println!(
        "admin approves the manager's journal: {}",
        journal(&approved)
    );
    if let Some(entry_id) = approved.decision.as_ref().and_then(|d| d.entry_id) {
        show(
            "the entry it posted",
            ledger
                .get_entry(&admin, COMPANY, entry_id)
                .await
                .map(|e| entry(&e)),
        );
    }
    show(
        "admin approves it again",
        ledger
            .approve_journal(&admin, COMPANY, rent_journal.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin rejects it",
        ledger
            .reject_journal(&admin, COMPANY, rent_journal.id, "Too late")
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin rejects the duplicate with no reason",
        ledger
            .reject_journal(&admin, COMPANY, rejected.id, " ")
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager rejects the duplicate they submitted",
        ledger
            .reject_journal(&manager, COMPANY, rejected.id, "Duplicate of journal 1")
            .await
            .map(|j| journal(&j)),
    );
    ledger
        .reject_journal(&admin, COMPANY, rejected.id, "Duplicate of journal 1")
        .await?;
    let accrual = ledger
        .post_entry(
            &manager,
            COMPANY,
            &pair(date(2026, 1, 10), "January deposit", bank, fees, "6.00"),
            &manual,
        )
        .await?;
    let periods = ledger.list_fiscal_years(&manager, COMPANY).await?;
    let first_period = periods
        .first()
        .and_then(|year| year.periods.first())
        .ok_or("company 1 has a fiscal year")?
        .clone();
    show(
        "manager closes January",
        ledger
            .close_period(&manager, COMPANY, first_period.id, Some("Month end"))
            .await
            .map(|p| p.status),
    );
    show(
        "admin approves the January journal while January is closed",
        ledger
            .approve_journal(&admin, COMPANY, january.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "the January journal, read again",
        ledger
            .get_journal(&admin, COMPANY, january.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager reverses the January deposit into closed January",
        ledger
            .reverse_entry(
                &manager,
                COMPANY,
                accrual.id,
                &reversal(date(2026, 1, 20), "Back-dated"),
            )
            .await
            .map(|e| entry(&e)),
    );
    show(
        "manager reopens January",
        ledger
            .reopen_period(&manager, COMPANY, first_period.id, "Accrual approved late")
            .await
            .map(|p| p.status),
    );
    show(
        "admin approves the January journal",
        ledger
            .approve_journal(&admin, COMPANY, january.id)
            .await
            .map(|j| journal(&j)),
    );
    ledger
        .update_account(
            &manager,
            COMPANY,
            paused,
            &AccountChange {
                active: Some(false),
                ..AccountChange::default()
            },
        )
        .await?;
    println!("manager deactivates account 7700");
    show(
        "admin approves the journal on account 7700",
        ledger
            .approve_journal(&admin, COMPANY, paused_journal.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "the journal on account 7700, read again",
        ledger
            .get_journal(&admin, COMPANY, paused_journal.id)
            .await
            .map(|j| j.status),
    );

    println!("\n== Separation of duties off");
    show(
        "manager turns separation off",
        ledger
            .set_duty_separation(
                &manager,
                COMPANY,
                false,
                Some("Two-person team during the audit"),
            )
            .await,
    );
    show(
        "manager turns it off again",
        ledger
            .set_duty_separation(&manager, COMPANY, false, Some("Again"))
            .await,
    );
    show(
        "legacy Finance separation off does not relax the journal Workflow policy",
        ledger
            .approve_journal(&manager, COMPANY, impersonated.id)
            .await
            .map(|j| journal(&j)),
    );
    let record = records::add(
        db,
        1,
        COMPANY,
        "open",
        "Approval specimen",
        date(2026, 6, 20),
        false,
    )
    .await?;
    show(
        "a sibling's approval: manager approves contractor's record",
        approve_record(
            db,
            &mutations,
            &ledger,
            &manager,
            &approval(record, &[(7, None)], None),
        )
        .await,
    );
    show(
        "a sibling's approval: manager approves their own record",
        approve_record(
            db,
            &mutations,
            &ledger,
            &manager,
            &approval(record, &[(7, None), (1, None)], None),
        )
        .await,
    );
    show(
        "a sibling's override: admin approves contractor's record past a control",
        approve_record(
            db,
            &mutations,
            &ledger,
            &admin,
            &approval(record, &[(7, None)], Some("Quantity variance accepted")),
        )
        .await,
    );
    show(
        "a sibling's override with no reason",
        approve_record(
            db,
            &mutations,
            &ledger,
            &admin,
            &approval(record, &[(7, None)], Some("  ")),
        )
        .await,
    );
    show(
        "a sibling's approval naming a malformed source",
        approve_record(
            db,
            &mutations,
            &ledger,
            &manager,
            &Approval {
                source: "Finance Walkthrough".into(),
                ..approval(record, &[(1, None)], None)
            },
        )
        .await,
    );
    show(
        "manager turns separation on again",
        ledger
            .set_duty_separation(&manager, COMPANY, true, None)
            .await,
    );
    show(
        "a sibling's approval: manager approves their own record again",
        approve_record(
            db,
            &mutations,
            &ledger,
            &manager,
            &approval(record, &[(1, None)], None),
        )
        .await,
    );
    show(
        "a sibling's approval: admin approves a record the manager prepared as admin",
        approve_record(
            db,
            &mutations,
            &ledger,
            &admin,
            &approval(record, &[(1, Some(4))], None),
        )
        .await,
    );
    show(
        "company 1's separation changes",
        ledger
            .list_duty_separation_changes(&manager, COMPANY)
            .await
            .map(|changes| {
                changes
                    .iter()
                    .map(|c| (c.change_number, c.required, c.reason.clone()))
                    .collect::<Vec<_>>()
            }),
    );

    println!("\n== Approval exceptions and their reviews");
    let exceptions = ledger
        .list_approval_exceptions(&manager, COMPANY, &ApprovalExceptionQuery::default())
        .await?;
    for listed in &exceptions {
        println!("{}", exception(listed));
    }
    let find = |action: &str, same_person: bool| {
        exceptions
            .iter()
            .rev()
            .find(|x| x.action == action && x.same_person == same_person)
            .map(|x| x.id)
            .ok_or("an exception of that kind")
    };
    let reopen = find("period_reopen", false)?;
    let own_record = find("record_approval", true)?;
    show(
        "unreviewed exceptions, one at a time after the first",
        ledger
            .list_approval_exceptions(
                &manager,
                COMPANY,
                &ApprovalExceptionQuery {
                    reviewed: Some(false),
                    limit: Some(1),
                    after: exceptions.first().map(|x| x.exception_number),
                    ..ApprovalExceptionQuery::default()
                },
            )
            .await
            .map(|list| list.iter().map(exception).collect::<Vec<_>>()),
    );
    show(
        "exceptions recorded from tomorrow",
        ledger
            .list_approval_exceptions(
                &manager,
                COMPANY,
                &ApprovalExceptionQuery {
                    recorded_from: Some(Timestamp::now().checked_add(24.hours())?),
                    ..ApprovalExceptionQuery::default()
                },
            )
            .await
            .map(|list| list.len()),
    );
    show(
        "exceptions with a limit of 0",
        ledger
            .list_approval_exceptions(
                &manager,
                COMPANY,
                &ApprovalExceptionQuery {
                    limit: Some(0),
                    ..ApprovalExceptionQuery::default()
                },
            )
            .await
            .map(|list| list.len()),
    );
    show(
        "exception 999999",
        ledger
            .get_approval_exception(&manager, COMPANY, 999_999)
            .await
            .map(|x| exception(&x)),
    );
    show(
        "manager reviews the reopen of January, which the manager made",
        ledger
            .review_approval_exception(&manager, COMPANY, reopen, Some("Fine"))
            .await
            .map(|x| exception(&x)),
    );
    show(
        "contractor (staff: no approval-exceptions.review) reviews the reopen",
        ledger
            .review_approval_exception(&contractor, COMPANY, reopen, None)
            .await
            .map(|x| exception(&x)),
    );
    show(
        "admin reviews the reopen of January with a 501-character note",
        ledger
            .review_approval_exception(&admin, COMPANY, reopen, Some(&"n".repeat(501)))
            .await
            .map(|x| exception(&x)),
    );
    show(
        "admin reviews the reopen of January",
        ledger
            .review_approval_exception(&admin, COMPANY, reopen, Some("  Accrual evidence seen  "))
            .await
            .map(|x| exception(&x)),
    );
    show(
        "admin reviews it again",
        ledger
            .review_approval_exception(&admin, COMPANY, reopen, None)
            .await
            .map(|x| exception(&x)),
    );
    show(
        "admin reviews the manager's approval of their own record",
        ledger
            .review_approval_exception(&admin, COMPANY, own_record, None)
            .await
            .map(|x| exception(&x)),
    );
    show(
        "reviewed exceptions",
        ledger
            .list_approval_exceptions(
                &viewer,
                COMPANY,
                &ApprovalExceptionQuery {
                    reviewed: Some(true),
                    ..ApprovalExceptionQuery::default()
                },
            )
            .await
            .map(|list| list.iter().map(|x| x.exception_number).collect::<Vec<_>>()),
    );

    println!("\n== Withdrawing and replacing a submitted journal");
    let typo = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 10), "Ofice rent", rent, bank, "21.00"),
        )
        .await?;
    println!("manager submits {}", journal(&typo));
    show(
        "admin withdraws the manager's journal (not its submitter)",
        ledger
            .withdraw_journal(&admin, COMPANY, typo.id, None)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "contractor (staff: no journals.submit) withdraws it",
        ledger
            .withdraw_journal(&contractor, COMPANY, typo.id, None)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "viewer replaces it (no journals.submit)",
        ledger
            .replace_journal(
                &viewer,
                COMPANY,
                typo.id,
                &pair(date(2026, 6, 10), "Office rent", rent, bank, "21.00"),
                None,
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager replaces it with an unbalanced journal: refused, the journal keeps waiting",
        ledger
            .replace_journal(
                &manager,
                COMPANY,
                typo.id,
                &NewEntry {
                    lines: vec![
                        NewLine::debit(rent, money("21.00")),
                        NewLine::credit(bank, money("20.00")),
                    ],
                    ..pair(date(2026, 6, 10), "Office rent", rent, bank, "21.00")
                },
                None,
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager replaces it onto deactivated account 7600: refused, the journal keeps waiting",
        ledger
            .replace_journal(
                &manager,
                COMPANY,
                typo.id,
                &pair(date(2026, 6, 10), "Office rent", retired, bank, "21.00"),
                None,
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager replaces it with a too-long reason: refused",
        ledger
            .replace_journal(
                &manager,
                COMPANY,
                typo.id,
                &pair(date(2026, 6, 10), "Office rent", rent, bank, "21.00"),
                Some(&"x".repeat(501)),
            )
            .await
            .map(|j| journal(&j)),
    );
    show(
        "the journal after the refusals",
        ledger
            .get_journal(&manager, COMPANY, typo.id)
            .await
            .map(|j| journal(&j)),
    );
    let corrected = ledger
        .replace_journal(
            &manager,
            COMPANY,
            typo.id,
            &pair(date(2026, 6, 10), "Office rent", rent, bank, "21.00"),
            Some("Misspelled description"),
        )
        .await?;
    println!(
        "manager replaces it with a corrected journal: {} replaces={:?}",
        journal(&corrected),
        corrected.replaces_journal_id
    );
    show(
        "the withdrawn journal, still on record with its lines and who withdrew it",
        ledger
            .get_journal(&viewer, COMPANY, typo.id)
            .await
            .map(|j| (journal(&j), j.replaced_by_journal_id, j.replaces_journal_id)),
    );
    show(
        "admin approves the withdrawn journal",
        ledger
            .approve_journal(&admin, COMPANY, typo.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager withdraws it again",
        ledger
            .withdraw_journal(&manager, COMPANY, typo.id, None)
            .await
            .map(|j| journal(&j)),
    );
    let posted: i64 = client
        .query_one(
            "SELECT count(*) FROM finance_ledger_entries WHERE company_id = $1 AND description = 'Ofice rent'",
            &[&COMPANY],
        )
        .await?
        .get(0);
    println!("entries posted from the withdrawn journal's description: {posted}");
    show(
        "the withdrawn journals",
        ledger
            .list_journals(
                &viewer,
                COMPANY,
                &JournalQuery {
                    status: Some(JournalStatus::Withdrawn),
                    ..JournalQuery::default()
                },
            )
            .await
            .map(|list| list.iter().map(journal).collect::<Vec<_>>()),
    );
    show(
        "admin approves the corrected journal: it needs its own approval and posts now",
        ledger
            .approve_journal(&admin, COMPANY, corrected.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "manager withdraws the approved journal",
        ledger
            .withdraw_journal(&manager, COMPANY, corrected.id, None)
            .await
            .map(|j| journal(&j)),
    );
    // Approval against withdrawal, and against replacement, at the same instant:
    // the company lock lets exactly one decide, and the loser sees the journal
    // is no longer waiting, so a withdrawn journal can never post stale content.
    for (label, replace, approve_first) in [
        ("withdrawal", false, true),
        ("withdrawal", false, false),
        ("replacement", true, true),
        ("replacement", true, false),
    ] {
        let name = format!("Raced {label} {}", if approve_first { "a" } else { "b" });
        let racing = ledger
            .submit_journal(
                &manager,
                COMPANY,
                &pair(date(2026, 6, 11), &name, rent, bank, "2.00"),
            )
            .await?;
        let fixed = pair(
            date(2026, 6, 11),
            &format!("{name} fixed"),
            rent,
            bank,
            "2.00",
        );
        let approve = ledger.approve_journal(&admin, COMPANY, racing.id);
        let correct = async {
            if replace {
                ledger
                    .replace_journal(&manager, COMPANY, racing.id, &fixed, None)
                    .await
            } else {
                ledger
                    .withdraw_journal(&manager, COMPANY, racing.id, None)
                    .await
            }
        };
        let (approval, correction) = if approve_first {
            tokio::join!(approve, correct)
        } else {
            let (correction, approval) = tokio::join!(correct, approve);
            (approval, correction)
        };
        let settled = ledger.get_journal(&manager, COMPANY, racing.id).await?;
        println!(
            "approval against {label} (approval asked {}): approval {}, {label} {}; the journal is {} with entry {:?}",
            if approve_first { "first" } else { "second" },
            approval
                .as_ref()
                .map_or_else(|e| format!("refused ({e})"), |_| "won".into()),
            correction
                .as_ref()
                .map_or_else(|e| format!("refused ({e})"), |_| "won".into()),
            settled.status.as_str(),
            settled.decision.as_ref().and_then(|d| d.entry_id),
        );
        assert!(
            approval.is_ok() != correction.is_ok(),
            "exactly one decides"
        );
        let stale: i64 = client
            .query_one(
                "SELECT count(*) FROM finance_ledger_entries WHERE company_id = $1 AND description = $2",
                &[&COMPANY, &name],
            )
            .await?
            .get(0);
        println!(
            "entries posted from the raced journal: {stale} ({})",
            if (settled.status == JournalStatus::Approved) == (stale == 1) {
                "consistent"
            } else {
                "INCONSISTENT"
            }
        );
    }
    let loose = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 12), "Never withdrawn", rent, bank, "3.00"),
        )
        .await?;
    let later = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 12), "Not a correction", rent, bank, "3.00"),
        )
        .await?;
    let (loose_id, later_id) = (loose.id, later.id);
    raw(
        &client,
        "a withdrawal recorded by someone other than the submitter",
        "INSERT INTO finance_ledger_journal_events(tenant_id,company_id,journal_id,event,actor_user_id,occurred_at) \
         SELECT 1,1,id,'withdrawn',4,now() FROM finance_ledger_journals \
         WHERE company_id = 1 AND submitted_by_user_id = 1 AND id NOT IN (SELECT journal_id FROM finance_ledger_journal_events) LIMIT 1",
    )
    .await;
    raw(
        &client,
        "a replacement link for a journal that was not withdrawn",
        &format!(
            "INSERT INTO finance_ledger_journal_replacements(tenant_id,company_id,journal_id,replaced_journal_id) \
             VALUES(1,1,{later_id},{loose_id})"
        ),
    )
    .await;
    raw(
        &client,
        "UPDATE a replacement link",
        "UPDATE finance_ledger_journal_replacements SET company_id = company_id",
    )
    .await;
    raw(
        &client,
        "DELETE a replacement link",
        "DELETE FROM finance_ledger_journal_replacements",
    )
    .await;

    println!("\n== Two sessions and an archived company");
    let raced = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &pair(date(2026, 6, 9), "Raced approval", rent, bank, "1.00"),
        )
        .await?;
    let (one, two) = tokio::join!(
        ledger.approve_journal(&admin, COMPANY, raced.id),
        ledger.approve_journal(&admin, COMPANY, raced.id),
    );
    show(
        "two sessions approve the same journal at once: first",
        one.map(|j| journal(&j)),
    );
    show(
        "two sessions approve the same journal at once: second",
        two.map(|j| journal(&j)),
    );
    show(
        "admin approves a journal in archived company 3",
        ledger
            .approve_journal(&admin, 3, raced.id)
            .await
            .map(|j| journal(&j)),
    );
    show(
        "admin turns off separation in archived company 3",
        ledger
            .set_duty_separation(&admin, 3, false, Some("Archived"))
            .await,
    );
    show(
        "the other tenant's manager reads company 1's journals",
        ledger
            .list_journals(
                &sign_in(db, services, 2, 5).await?,
                COMPANY,
                &JournalQuery::default(),
            )
            .await
            .map(|list| list.len()),
    );
    show(
        "all journals, two at a time after the first",
        ledger
            .list_journals(
                &viewer,
                COMPANY,
                &JournalQuery {
                    limit: Some(2),
                    after: Some(1),
                    ..JournalQuery::default()
                },
            )
            .await
            .map(|list| list.iter().map(journal).collect::<Vec<_>>()),
    );

    println!("\n== The database's own refusals (raw SQL, no facade)");
    let (approved_id, rejected_id) = (approved.id, rejected.id);
    raw(
        &client,
        "UPDATE a journal's description",
        &format!("UPDATE finance_ledger_journals SET description='Changed' WHERE id={approved_id}"),
    )
    .await;
    raw(
        &client,
        "DELETE a journal's decision",
        &format!("DELETE FROM finance_ledger_journal_events WHERE journal_id={approved_id}"),
    )
    .await;
    raw(
        &client,
        "add a line to a submitted journal",
        &format!(
            "INSERT INTO finance_ledger_journal_lines(tenant_id,company_id,journal_id,line_number,account_id,debit,credit) \
             VALUES(1,1,{approved_id},9,{rent},1,0)"
        ),
    )
    .await;
    raw(
        &client,
        "submit an unbalanced journal",
        &format!(
            "BEGIN; \
             WITH j AS (INSERT INTO finance_ledger_journals(tenant_id,company_id,journal_number,entry_date,description,currency_code,submitted_by_user_id,submitted_at) \
               VALUES(1,1,NULL,'2026-06-01','Unbalanced','{currency}',1,NULL) RETURNING id) \
             INSERT INTO finance_ledger_journal_lines(tenant_id,company_id,journal_id,line_number,account_id,debit,credit) \
               SELECT 1,1,id,1,{rent},5,0 FROM j; \
             COMMIT",
            currency = book.currency_code
        ),
    )
    .await;
    raw(
        &client,
        "write a journal decision without its Workflow approval round",
        &format!(
            "BEGIN; \
             WITH j AS (INSERT INTO finance_ledger_journals(tenant_id,company_id,journal_number,entry_date,description,currency_code,submitted_by_user_id,submitted_at) \
               VALUES(1,1,NULL,'2026-06-01','Self-approved','{currency}',1,NULL) RETURNING id), \
             l AS (INSERT INTO finance_ledger_journal_lines(tenant_id,company_id,journal_id,line_number,account_id,debit,credit) \
               SELECT 1,1,id,n,a,d,c FROM j, (VALUES (1,{rent},5,0),(2,{bank},0,5)) v(n,a,d,c)) \
             INSERT INTO finance_ledger_journal_events(tenant_id,company_id,journal_id,event,entry_id,actor_user_id,occurred_at) \
               SELECT 1,1,id,'approved',{entry},1,NULL FROM j; \
             COMMIT",
            currency = book.currency_code,
            entry = original.id
        ),
    )
    .await;
    raw(
        &client,
        "decide a rejected journal a second time",
        &format!(
            "INSERT INTO finance_ledger_journal_events(tenant_id,company_id,journal_id,event,reason,actor_user_id,occurred_at) \
             VALUES(1,1,{rejected_id},'rejected','Again',4,NULL)"
        ),
    )
    .await;
    raw(
        &client,
        "record a same-person exception while separation is required",
        "INSERT INTO finance_ledger_approval_exceptions(tenant_id,company_id,exception_number,source,action,subject_type,\
         subject_id,subject,same_person,override,prepared_by_user_id,approved_by_user_id,reason,separation_change_id,recorded_at) \
         SELECT 1,1,NULL,'finance/walkthrough','record_approval','specimen_record',1,'Specimen',true,false,1,1,'Raw',id,NULL \
         FROM finance_ledger_duty_separation_changes WHERE company_id=1 AND NOT required ORDER BY change_number DESC LIMIT 1",
    )
    .await;
    raw(
        &client,
        "turn separation on while it is on",
        "INSERT INTO finance_ledger_duty_separation_changes(tenant_id,company_id,change_number,required,changed_by_user_id,changed_at) \
         VALUES(1,1,NULL,true,1,NULL)",
    )
    .await;
    raw(
        &client,
        "the approver reviews an exception",
        &format!(
            "INSERT INTO finance_ledger_approval_exception_reviews(tenant_id,company_id,exception_id,reviewed_by_user_id,reviewed_at) \
             VALUES(1,1,{own_record},1,NULL)"
        ),
    )
    .await;
    raw(
        &client,
        "UPDATE an exception's reason",
        &format!(
            "UPDATE finance_ledger_approval_exceptions SET reason='Changed' WHERE id={reopen}"
        ),
    )
    .await;
    raw(
        &client,
        "DELETE a separation change",
        "DELETE FROM finance_ledger_duty_separation_changes WHERE company_id=1",
    )
    .await;
    raw(
        &client,
        "post a second reversal of an entry",
        &format!(
            "BEGIN; \
             WITH e AS (INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at,reverses_entry_id) \
               VALUES(1,1,0,'2026-06-25','Second reversal','{currency}',1,now(),{id}) RETURNING id) \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
               SELECT 1,1,id,n,a,d,c FROM e, (VALUES (1,{fees},100,0),(2,{bank},0,100)) v(n,a,d,c); \
             COMMIT",
            currency = book.currency_code,
            id = original.id
        ),
    )
    .await;
    raw(
        &client,
        "reverse an owned entry without its owner",
        &format!(
            "BEGIN; \
             WITH e AS (INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,currency_code,posted_by_user_id,posted_at,reverses_entry_id) \
               SELECT 1,1,0,'2026-06-25','Ownerless reversal','{currency}',1,now(),id FROM finance_ledger_entries \
               WHERE company_id=1 AND owner_module IS NOT NULL AND reverses_entry_id IS NULL \
               AND id NOT IN (SELECT reverses_entry_id FROM finance_ledger_entries WHERE reverses_entry_id IS NOT NULL) LIMIT 1 RETURNING id) \
             INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,debit,credit) \
               SELECT 1,1,id,n,a,d,c FROM e, (VALUES (1,{fees},1,0),(2,{bank},0,1)) v(n,a,d,c); \
             COMMIT",
            currency = book.currency_code
        ),
    )
    .await;
    let audited: Vec<(String, i64)> = client
        .query(
            "SELECT auditable_type, count(*) FROM base_audit_mutations WHERE auditable_type IN \
             ('finance.ledger.journal','finance.ledger.journal_line','finance.ledger.journal_event','finance.ledger.journal_replacement',\
              'finance.ledger.duty_separation_change','finance.ledger.approval_exception',\
              'finance.ledger.approval_exception_review') GROUP BY 1 ORDER BY 1",
            &[],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    println!("audit mutations recorded for the controls: {audited:?}");
    Ok(())
}
