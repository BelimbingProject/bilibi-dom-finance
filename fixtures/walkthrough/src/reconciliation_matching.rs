//! The matching and reconciliation walkthrough (NC2): `finance/reconciliation`
//! matches, reconciliations and statement-line postings on Company 001, in a
//! disposable database, after the import walkthrough (which adds the bank
//! accounts' chart and deactivates the foreign currency account). A fresh
//! bank account on its own ledger account keeps the figures readable. Every
//! line is the facade's actual answer or the database's own refusal; nothing
//! here is a test double.
use crate::{sign_in, COMPANY};
use bilibi_authz::grants::Grants;
use bilibi_database::Db;
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::{
    AccountType, ApprovalExceptionQuery, Entry, Ledger, Money, NewAccount, NewEntry, NewLine,
    PostOptions,
};
use bilibi_finance_reconciliation::{
    BankReconciliation, Match, NewBankAccount, NewMatch, NewReconciliation, PageOptions, Pins,
    PostLine, Reconciliation, ReconciliationError, ReconciliationPreview, ReverseLine,
    StatementLineItem, POST_STATEMENT_LINES,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::Authentication;
use jiff::civil::{date, Date};
use serde_json::json;
use std::fmt::Debug;
use tokio_postgres::Client;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn show<T: Debug>(step: &str, result: Result<T, ReconciliationError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(ReconciliationError::Invalid(errors)) => {
            let errors: Vec<String> = errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect();
            println!("{step}: refused ({})", errors.join("; "));
        }
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

fn brief_match(m: &Match) -> String {
    let entries: Vec<String> = m
        .entries
        .iter()
        .map(|e| format!("entry {} {} {}", e.entry_number, e.entry_date, e.amount))
        .collect();
    let events: Vec<String> = m
        .events
        .iter()
        .map(|e| {
            format!(
                "{} {}{} by user {}{}",
                e.event_number,
                e.action,
                e.reason
                    .as_ref()
                    .map_or(String::new(), |r| format!(" ({r})")),
                e.recorded_by_user_id,
                e.recorded_by_impersonator_id
                    .map_or(String::new(), |i| format!(" impersonated by {i}"))
            )
        })
        .collect();
    format!(
        "match {} {:?} note={:?} lines={:?} [{}] events [{}]",
        m.match_number,
        m.status,
        m.note,
        m.statement_line_ids,
        entries.join(", "),
        events.join("; ")
    )
}

fn brief_entry(e: &Entry) -> String {
    let lines: Vec<String> = e
        .lines
        .iter()
        .map(|l| {
            if l.debit.is_zero() {
                format!("{}:-{}", l.account_id, l.credit)
            } else {
                format!("{}:{}", l.account_id, l.debit)
            }
        })
        .collect();
    format!(
        "entry {} {} {:?} ref={:?} owner={} reverses={:?} [{}]",
        e.entry_number,
        e.entry_date,
        e.description,
        e.reference,
        e.owner
            .as_ref()
            .map_or("none".into(), |o| format!("{} {}", o.module, o.record)),
        e.reverses_entry_id,
        lines.join(" ")
    )
}

fn figures(preview: &ReconciliationPreview) -> String {
    let f = &preview.figures;
    format!(
        "to {} statement {} book {} difference {} {} through entry {} statement {} match event {}\n{}",
        preview.end_date,
        preview.statement_balance,
        f.book_balance,
        f.difference,
        f.currency_code,
        f.through_entry_number,
        f.through_statement_number,
        f.through_match_event_number,
        f.items
            .iter()
            .map(|i| format!(
                "  {} {} {:?} ref={:?} {}",
                i.kind.as_str(),
                i.item_date,
                i.description,
                i.reference,
                i.amount
            ))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn brief_reconciliation(r: &BankReconciliation) -> String {
    let events: Vec<String> = r
        .events
        .iter()
        .map(|e| {
            format!(
                "{} {}{}{} by user {}",
                e.event_number,
                e.action,
                e.reason
                    .as_ref()
                    .map_or(String::new(), |r| format!(" ({r})")),
                e.through_entry_number.map_or(String::new(), |n| format!(
                    " book {} difference {} at entry {n} statement {} match event {}",
                    e.book_balance.unwrap_or_default(),
                    e.difference.unwrap_or_default(),
                    e.through_statement_number.unwrap_or_default(),
                    e.through_match_event_number.unwrap_or_default()
                )),
                e.recorded_by_user_id
            )
        })
        .collect();
    format!(
        "reconciliation {} {:?} to {} balance {} opened by user {} events [{}]{}",
        r.reconciliation_number,
        r.status,
        r.end_date,
        r.statement_balance,
        r.opened_by_user_id,
        events.join("; "),
        r.figures.as_ref().map_or(String::new(), |f| format!(
            " difference {} with {} items",
            f.difference,
            f.items.len()
        ))
    )
}

/// Raw SQL from "another client": the database's guards hold without the facade.
async fn raw(client: &Client, label: &str, sql: &str) {
    match client.batch_execute(sql).await {
        Ok(()) => println!("{label}: ok (changed)"),
        Err(error) => println!(
            "{label}: refused by database — {}",
            error.as_db_error().map_or("unavailable", |d| d.message())
        ),
    }
    client.batch_execute("ROLLBACK").await.ok();
}

fn line_id(lines: &[StatementLineItem], description: &str) -> Outcome<i64> {
    Ok(lines
        .iter()
        .find(|l| l.line.description == description)
        .ok_or("fixture statement line")?
        .line
        .id)
}

/// What the statement shows on `day`: its lines through that day, from nil.
fn statement_balance(lines: &[StatementLineItem], day: Date) -> Money {
    lines
        .iter()
        .filter(|l| l.line.line_date <= day)
        .map(|l| l.line.amount)
        .sum()
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let reconciliation = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let other = sign_in(db, services, 2, 5).await?;
    // The admin acting as the manager, as an operator's impersonation session.
    let admin_as_manager = {
        let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
        let client = db.get().await?;
        authentication
            .authenticate_session(&**client, 1, 1, Some(4), None, String::new())
            .await?
    };
    let raw_client = db.get().await?;
    let units = ledger.get_book(&manager, COMPANY).await?.minor_units;
    let money = |text: &str| Money::parse(text, units).expect("fixture amount");

    println!("\n== Matching: a bank account and its postings");
    if ledger
        .list_accounts(&manager, COMPANY)
        .await?
        .iter()
        .all(|a| a.code != "1840")
    {
        ledger
            .create_account(
                &manager,
                COMPANY,
                &NewAccount::new("1840", "Collections at bank", AccountType::Asset),
            )
            .await?;
    }
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let id = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or("fixture account")
    };
    let (bank, cash, receivables, payables, sales) = (
        id("1840")?,
        id("1000")?,
        id("1100")?,
        id("2000")?,
        id("4000")?,
    );
    let collections = reconciliation
        .create_bank_account(
            &manager,
            COMPANY,
            &NewBankAccount {
                name: "Collections account".into(),
                ledger_account_id: bank,
                import_mapping: json!({
                    "date": "Date", "description": "Details", "reference": "Ref", "amount": "Amount"
                }),
                charges_account_id: Some(id("6000")?),
                interest_account_id: Some(id("7000")?),
                match_window_days: None,
                active: None,
                reason: Some("Collections".into()),
            },
        )
        .await?;
    println!(
        "manager adds bank account #{} {:?} on ledger account 1840, window {} days",
        collections.id, collections.name, collections.match_window_days
    );
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
    let mut entries = Vec::new();
    for new in [
        post(
            date(2026, 2, 2),
            "Customer receipt INV-201",
            "INV-201",
            bank,
            receivables,
            "300.00",
        ),
        post(
            date(2026, 2, 5),
            "Supplier payment",
            "PAY-7",
            payables,
            bank,
            "120.00",
        ),
        post(
            date(2026, 2, 10),
            "Counter receipt A",
            "BATCH-1",
            bank,
            receivables,
            "50.00",
        ),
        post(
            date(2026, 2, 11),
            "Counter receipt B",
            "BATCH-1",
            bank,
            receivables,
            "25.00",
        ),
        post(
            date(2026, 2, 26),
            "Cheque 2001",
            "CHQ-2001",
            payables,
            bank,
            "80.00",
        ),
        post(
            date(2026, 2, 27),
            "Deposit DEP-9",
            "DEP-9",
            bank,
            receivables,
            "40.00",
        ),
    ] {
        let entry = ledger
            .post_entry(&manager, COMPANY, &new, &PostOptions::default())
            .await?;
        println!("manager posts {}", brief_entry(&entry));
        entries.push(entry);
    }
    let (receipt, payment, counter_a, counter_b, cheque, deposit) = (
        &entries[0],
        &entries[1],
        &entries[2],
        &entries[3],
        &entries[4],
        &entries[5],
    );
    let february = "Date,Details,Ref,Amount\n\
                    2026-02-03,Receipt INV-201,INV-201,300.00\n\
                    2026-02-06,Payment PAY-7,PAY-7,-120.00\n\
                    2026-02-12,Batch deposit,BATCH-1,75.00\n\
                    2026-02-20,Account fee,,-5.00\n\
                    2026-02-28,Interest,,0.42\n\
                    2026-02-28,Unknown credit,,10.00\n";
    let march = "Date,Details,Ref,Amount\n\
                 2026-03-02,Deposit,DEP-9,40.00\n\
                 2026-03-05,March fee,,-5.00\n";
    for (name, content) in [
        ("collections-feb.csv", february),
        ("collections-mar.csv", march),
    ] {
        let done = reconciliation
            .import_statement(
                &manager,
                COMPANY,
                collections.id,
                &NewFile {
                    file_name: name.into(),
                    content: content.as_bytes().to_vec(),
                    content_type: None,
                },
            )
            .await?;
        println!(
            "manager imports {name} as statement {} with {} lines",
            done.statement.statement_number, done.statement.line_count
        );
    }

    println!("\n== Unmatched items and suggestions");
    let unmatched = reconciliation
        .unmatched_items(&viewer, COMPANY, collections.id, None)
        .await?;
    for item in &unmatched.lines {
        println!(
            "unmatched line {} (statement {} line {}) {} {:?} ref={:?} {} -> suggested {}",
            item.line.id,
            item.statement_number,
            item.line.line_number,
            item.line.line_date,
            item.line.description,
            item.line.reference,
            item.line.amount,
            unmatched
                .suggestions
                .get(&item.line.id)
                .map_or("nothing".into(), |e| format!(
                    "entry {} {:?}",
                    e.entry_number, e.description
                ))
        );
    }
    for item in &unmatched.entries {
        println!(
            "unmatched entry {} {} {:?} ref={:?} {}",
            item.entry_number, item.entry_date, item.description, item.reference, item.amount
        );
    }
    show(
        "viewer reads the unmatched items through 2026-02-10",
        reconciliation
            .unmatched_items(&viewer, COMPANY, collections.id, Some(date(2026, 2, 10)))
            .await
            .map(|u| (u.lines.len(), u.entries.len(), u.suggestions.len())),
    );
    show(
        "an unknown bank account",
        reconciliation
            .unmatched_items(&viewer, COMPANY, 999, None)
            .await
            .map(|_| ()),
    );
    show(
        "the other tenant reads Company 001's unmatched items",
        reconciliation
            .unmatched_items(&other, COMPANY, collections.id, None)
            .await
            .map(|_| ()),
    );
    let lines = reconciliation
        .list_statement_lines(&viewer, COMPANY, collections.id, None)
        .await?;
    let (inv, pay, batch, fee, interest, unknown, dep, march_fee) = (
        line_id(&lines, "Receipt INV-201")?,
        line_id(&lines, "Payment PAY-7")?,
        line_id(&lines, "Batch deposit")?,
        line_id(&lines, "Account fee")?,
        line_id(&lines, "Interest")?,
        line_id(&lines, "Unknown credit")?,
        line_id(&lines, "Deposit")?,
        line_id(&lines, "March fee")?,
    );

    println!("\n== Matching: who may, and what is refused");
    let pick = |lines: Vec<i64>, entries: Vec<i64>| NewMatch {
        statement_line_ids: lines,
        entry_ids: entries,
        note: None,
    };
    let other_line = reconciliation
        .list_bank_accounts(&manager, COMPANY)
        .await?
        .into_iter()
        .find(|a| a.name == "Operating account")
        .ok_or("operating account")?;
    let other_line = reconciliation
        .list_statement_lines(&manager, COMPANY, other_line.id, None)
        .await?
        .first()
        .ok_or("operating line")?
        .line
        .id;
    let cash_only = ledger
        .post_entry(
            &manager,
            COMPANY,
            &post(date(2026, 2, 3), "Cash sale", "CS-1", cash, sales, "300.00"),
            &PostOptions::default(),
        )
        .await?;
    for (step, scope, selection) in [
        ("viewer matches", &viewer, pick(vec![inv], vec![receipt.id])),
        (
            "nothing chosen on one side",
            &manager,
            pick(vec![inv], vec![]),
        ),
        (
            "a line of another bank account",
            &manager,
            pick(vec![other_line], vec![receipt.id]),
        ),
        ("an unknown entry", &manager, pick(vec![inv], vec![999_999])),
        (
            "an entry with no posting on the bank account's ledger account",
            &manager,
            pick(vec![inv], vec![cash_only.id]),
        ),
        (
            "sides that differ",
            &manager,
            pick(vec![inv], vec![payment.id]),
        ),
        (
            "a note of 501 characters",
            &manager,
            NewMatch {
                note: Some("n".repeat(501)),
                ..pick(vec![inv], vec![receipt.id])
            },
        ),
        (
            "the other tenant matches",
            &other,
            pick(vec![inv], vec![receipt.id]),
        ),
    ] {
        show(
            step,
            reconciliation
                .record_match(scope, COMPANY, collections.id, &selection)
                .await
                .map(|m| brief_match(&m)),
        );
    }

    println!("\n== Matching: one to one, and one line to many entries");
    let matched = reconciliation
        .record_match(
            &manager,
            COMPANY,
            collections.id,
            &NewMatch {
                note: Some("  Receipt for INV-201  ".into()),
                ..pick(vec![inv], vec![receipt.id])
            },
        )
        .await?;
    println!("manager matches {}", brief_match(&matched));
    let pay_match = reconciliation
        .record_match(
            &manager,
            COMPANY,
            collections.id,
            &pick(vec![pay], vec![payment.id]),
        )
        .await?;
    println!("manager matches {}", brief_match(&pay_match));
    show(
        "manager matches the batch deposit to both counter receipts (the same id twice counts once)",
        reconciliation
            .record_match(
                &manager,
                COMPANY,
                collections.id,
                &pick(vec![batch, batch], vec![counter_a.id, counter_b.id]),
            )
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "the matched line again",
        reconciliation
            .record_match(
                &manager,
                COMPANY,
                collections.id,
                &pick(vec![inv], vec![receipt.id]),
            )
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "the matched entry with another line",
        reconciliation
            .record_match(
                &manager,
                COMPANY,
                collections.id,
                &pick(vec![unknown], vec![receipt.id]),
            )
            .await
            .map(|m| brief_match(&m)),
    );

    println!("\n== Unmatching");
    show(
        "unmatch without a reason",
        reconciliation
            .unmatch(&manager, COMPANY, pay_match.id, Some("   "))
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "viewer unmatches",
        reconciliation
            .unmatch(&viewer, COMPANY, pay_match.id, Some("Wrong"))
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "admin, impersonating the manager, unmatches the payment",
        reconciliation
            .unmatch(
                &admin_as_manager,
                COMPANY,
                pay_match.id,
                Some("  Matched in error  "),
            )
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "unmatch it again",
        reconciliation
            .unmatch(&manager, COMPANY, pay_match.id, Some("Again"))
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "an unknown match",
        reconciliation
            .unmatch(&manager, COMPANY, 999_999, Some("Gone"))
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "manager matches the payment again, as a new match",
        reconciliation
            .record_match(
                &manager,
                COMPANY,
                collections.id,
                &pick(vec![pay], vec![payment.id]),
            )
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "viewer reads the first match",
        reconciliation
            .get_match(&viewer, COMPANY, matched.id)
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "viewer lists the bank account's matches, two per page, after the first",
        reconciliation
            .list_matches(
                &viewer,
                COMPANY,
                collections.id,
                &PageOptions {
                    limit: Some(2),
                    after: Some(matched.id),
                },
            )
            .await
            .map(|ms| ms.iter().map(brief_match).collect::<Vec<_>>()),
    );

    raw(&raw_client, "an unmatched event without a reason", &format!(
        "INSERT INTO finance_reconciliation_match_events(tenant_id,company_id,event_number,match_id,action,recorded_by_user_id,recorded_at) \
         VALUES(1,{COMPANY},NULL,{},'unmatched',1,NULL)", matched.id
    )).await;

    println!("\n== Two sessions match the same line at once");
    let racing = pick(vec![dep], vec![deposit.id]);
    let (one, two) = tokio::join!(
        reconciliation.record_match(&manager, COMPANY, collections.id, &racing),
        reconciliation.record_match(&admin, COMPANY, collections.id, &racing),
    );
    println!(
        "one matched, the other refused: {}",
        match (&one, &two) {
            (Ok(done), Err(error)) | (Err(error), Ok(done)) =>
                format!("match {} / {error}", done.match_number),
            _ => format!("{:?} / {:?}", one.as_ref().err(), two.as_ref().err()),
        }
    );

    println!("\n== Posting charges and interest from statement lines");
    let savings = reconciliation
        .list_bank_accounts(&manager, COMPANY)
        .await?
        .into_iter()
        .find(|a| a.name == "Savings account")
        .ok_or("savings account")?;
    let savings_fee = reconciliation
        .list_statement_lines(&manager, COMPANY, savings.id, None)
        .await?
        .into_iter()
        .find(|l| l.line.description == "Fee")
        .ok_or("savings fee")?
        .line
        .id;
    let as_kind = |kind: &str| PostLine {
        kind: kind.into(),
        description: None,
    };
    for (step, scope, line, kind) in [
        ("staff posts the fee", &staff, fee, "charge"),
        ("a kind that is neither", &manager, fee, "fee"),
        ("money out as interest", &manager, fee, "interest"),
        ("money in as a charge", &manager, interest, "charge"),
        (
            "a fee of a bank account with no charges account",
            &manager,
            savings_fee,
            "charge",
        ),
        ("a matched line", &manager, inv, "interest"),
        ("an unknown line", &manager, 999_999, "charge"),
    ] {
        show(
            step,
            reconciliation
                .post_statement_line(scope, COMPANY, line, &as_kind(kind))
                .await
                .map(|p| brief_entry(&p.entry)),
        );
    }
    grants
        .set_direct(&admin, 3, POST_STATEMENT_LINES, Some(true))
        .await?;
    let before: i64 = raw_client
        .query_one("SELECT count(*) FROM finance_reconciliation_matches", &[])
        .await?
        .get(0);
    show(
        "staff, granted statement-line posting but not the ledger's posting, posts the fee",
        reconciliation
            .post_statement_line(&staff, COMPANY, fee, &as_kind("charge"))
            .await
            .map(|p| brief_entry(&p.entry)),
    );
    let after: i64 = raw_client
        .query_one("SELECT count(*) FROM finance_reconciliation_matches", &[])
        .await?
        .get(0);
    println!("matches before and after the ledger's refusal: {before} / {after}");
    grants
        .set_direct(&admin, 3, POST_STATEMENT_LINES, None)
        .await?;
    let posted = reconciliation
        .post_statement_line(
            &manager,
            COMPANY,
            fee,
            &PostLine {
                kind: "charge".into(),
                description: Some("  Monthly account fee  ".into()),
            },
        )
        .await?;
    println!(
        "manager posts the fee: {}\n  and {}",
        brief_entry(&posted.entry),
        brief_match(&posted.posted_match)
    );
    show(
        "post the fee again",
        reconciliation
            .post_statement_line(&manager, COMPANY, fee, &as_kind("charge"))
            .await
            .map(|p| brief_entry(&p.entry)),
    );
    let interest_posted = reconciliation
        .post_statement_line(&manager, COMPANY, interest, &as_kind("interest"))
        .await?;
    println!(
        "manager posts the interest: {}",
        brief_entry(&interest_posted.entry)
    );
    let march_posted = reconciliation
        .post_statement_line(&manager, COMPANY, march_fee, &as_kind("charge"))
        .await?;
    println!(
        "manager posts the March fee: {}",
        brief_entry(&march_posted.entry)
    );

    println!("\n== Reversing a posted statement line");
    let reason = |reason: &str, day: Option<Date>| ReverseLine {
        reason: Some(reason.into()),
        entry_date: day,
    };
    for (step, scope, line, reverse) in [
        (
            "staff reverses the March fee",
            &staff,
            march_fee,
            reason("Refunded", None),
        ),
        ("a blank reason", &manager, march_fee, reason("  ", None)),
        (
            "a reason of 501 characters",
            &manager,
            march_fee,
            reason(&"r".repeat(501), None),
        ),
        (
            "an unmatched line",
            &manager,
            unknown,
            reason("Refunded", None),
        ),
        (
            "a line matched by hand, not posted",
            &manager,
            inv,
            reason("Refunded", None),
        ),
        (
            "a reversal dated before the entry",
            &manager,
            march_fee,
            reason("Refunded", Some(date(2026, 3, 1))),
        ),
    ] {
        show(
            step,
            reconciliation
                .reverse_statement_line(scope, COMPANY, line, &reverse)
                .await
                .map(|r| brief_entry(&r.reversal)),
        );
    }
    let reversed = reconciliation
        .reverse_statement_line(
            &manager,
            COMPANY,
            march_fee,
            &reason("  Fee refunded by the bank  ", Some(date(2026, 3, 6))),
        )
        .await?;
    println!(
        "manager reverses the March fee: {}\n  and {}",
        brief_entry(&reversed.reversal),
        brief_match(&reversed.unmatched_match)
    );
    show(
        "post the March fee again",
        reconciliation
            .post_statement_line(&manager, COMPANY, march_fee, &as_kind("charge"))
            .await
            .map(|p| brief_entry(&p.entry)),
    );
    show(
        "reverse the March fee again",
        reconciliation
            .reverse_statement_line(&manager, COMPANY, march_fee, &reason("Again", None))
            .await
            .map(|r| brief_entry(&r.reversal)),
    );

    println!("\n== Reconciliations: preview and open");
    let feb_end = date(2026, 2, 28);
    let feb_balance = statement_balance(&lines, feb_end);
    let preview = reconciliation
        .preview_reconciliation(
            &viewer,
            COMPANY,
            collections.id,
            &NewReconciliation {
                end_date: Some(feb_end),
                statement_balance: feb_balance.to_string(),
            },
            &Pins::default(),
        )
        .await?;
    println!("viewer previews February: {}", figures(&preview));
    for (step, end_date, balance, pins) in [
        ("a preview with no end date", None, "1", Pins::default()),
        (
            "a preview with a balance finer than the book",
            Some(feb_end),
            "1.234",
            Pins::default(),
        ),
        (
            "a preview with a negative pin",
            Some(feb_end),
            "1",
            Pins {
                through_entry_number: Some(-1),
                ..Pins::default()
            },
        ),
    ] {
        show(
            step,
            reconciliation
                .preview_reconciliation(
                    &viewer,
                    COMPANY,
                    collections.id,
                    &NewReconciliation {
                        end_date,
                        statement_balance: balance.into(),
                    },
                    &pins,
                )
                .await
                .map(|p| p.figures.difference),
        );
    }
    let foreign = reconciliation
        .list_bank_accounts(&manager, COMPANY)
        .await?
        .into_iter()
        .find(|a| !a.active)
        .ok_or("inactive bank account")?;
    let opening = |day: Option<Date>, balance: &str| NewReconciliation {
        end_date: day,
        statement_balance: balance.into(),
    };
    for (step, scope, account, new) in [
        (
            "viewer opens",
            &viewer,
            collections.id,
            opening(Some(feb_end), "1"),
        ),
        (
            "no end date and a balance finer than the book",
            &manager,
            collections.id,
            opening(None, "1.234"),
        ),
        (
            "a blank balance",
            &manager,
            collections.id,
            opening(Some(feb_end), " "),
        ),
        (
            "an inactive bank account",
            &manager,
            foreign.id,
            opening(Some(feb_end), "1"),
        ),
    ] {
        show(
            step,
            reconciliation
                .open_reconciliation(scope, COMPANY, account, &new)
                .await
                .map(|r| brief_reconciliation(&r)),
        );
    }
    let mistyped = reconciliation
        .open_reconciliation(
            &manager,
            COMPANY,
            collections.id,
            &opening(Some(feb_end), &(feb_balance + money("5")).to_string()),
        )
        .await?;
    println!(
        "manager opens with a mistyped balance: {}",
        brief_reconciliation(&mistyped)
    );
    show(
        "a second open reconciliation",
        reconciliation
            .open_reconciliation(
                &manager,
                COMPANY,
                collections.id,
                &opening(Some(date(2026, 3, 31)), "1"),
            )
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "viewer reads it, with its figures read now",
        reconciliation
            .get_reconciliation(&viewer, COMPANY, mistyped.id)
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "admin finalises it",
        reconciliation
            .finalise_reconciliation(&admin, COMPANY, mistyped.id, None)
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "abandon it without a reason",
        reconciliation
            .abandon_reconciliation(&manager, COMPANY, mistyped.id, None)
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "manager abandons it",
        reconciliation
            .abandon_reconciliation(
                &manager,
                COMPANY,
                mistyped.id,
                Some("Statement balance mistyped"),
            )
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "abandon it again",
        reconciliation
            .abandon_reconciliation(&manager, COMPANY, mistyped.id, Some("Again"))
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "finalise the abandoned one",
        reconciliation
            .finalise_reconciliation(&admin, COMPANY, mistyped.id, None)
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "read the abandoned one: no figures",
        reconciliation
            .get_reconciliation(&viewer, COMPANY, mistyped.id)
            .await
            .map(|r| brief_reconciliation(&r)),
    );

    println!("\n== Reconciliations: finalising by someone other than whoever opened it");
    let february_open = reconciliation
        .open_reconciliation(
            &manager,
            COMPANY,
            collections.id,
            &opening(Some(feb_end), &feb_balance.to_string()),
        )
        .await?;
    println!("manager opens: {}", brief_reconciliation(&february_open));
    for (step, scope) in [
        ("viewer finalises", &viewer),
        ("the manager who opened it finalises", &manager),
        (
            "admin, impersonating the manager who opened it, finalises",
            &admin_as_manager,
        ),
    ] {
        show(
            step,
            reconciliation
                .finalise_reconciliation(scope, COMPANY, february_open.id, None)
                .await
                .map(|r| brief_reconciliation(&r)),
        );
    }
    // Outstanding items never block: a reason is kept when lines wait.
    let finalised = reconciliation
        .finalise_reconciliation(
            &admin,
            COMPANY,
            february_open.id,
            Some("Outstanding items checked against the March statement"),
        )
        .await?;
    println!("admin finalises: {}", brief_reconciliation(&finalised));
    let snapshot = finalised.figures.clone().ok_or("finalised figures")?;
    for item in &snapshot.items {
        println!(
            "  item {} {} {:?} {}",
            item.kind.as_str(),
            item.item_date,
            item.description,
            item.amount
        );
    }
    show(
        "finalise it again",
        reconciliation
            .finalise_reconciliation(&admin, COMPANY, february_open.id, None)
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "abandon the finalised one",
        reconciliation
            .abandon_reconciliation(&manager, COMPANY, february_open.id, Some("Late"))
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    show(
        "unmatch a match the finalised reconciliation counted",
        reconciliation
            .unmatch(&manager, COMPANY, matched.id, Some("Late"))
            .await
            .map(|m| brief_match(&m)),
    );
    show(
        "reverse the February fee, which the finalised reconciliation counted",
        reconciliation
            .reverse_statement_line(&manager, COMPANY, fee, &reason("Late", None))
            .await
            .map(|r| brief_entry(&r.reversal)),
    );
    show(
        "a reconciliation ending on or before the last finalised one",
        reconciliation
            .open_reconciliation(
                &manager,
                COMPANY,
                collections.id,
                &opening(Some(date(2026, 2, 27)), "1"),
            )
            .await
            .map(|r| brief_reconciliation(&r)),
    );

    println!("\n== A finalised reconciliation reproduces at its numbers");
    let late = ledger
        .post_entry(
            &manager,
            COMPANY,
            &post(
                date(2026, 2, 15),
                "Late-recorded receipt",
                "LATE-1",
                bank,
                receivables,
                "7.00",
            ),
            &PostOptions::default(),
        )
        .await?;
    println!("manager posts, after finalising, {}", brief_entry(&late));
    let stored = reconciliation
        .get_reconciliation(&viewer, COMPANY, february_open.id)
        .await?
        .figures
        .ok_or("stored figures")?;
    let pinned = reconciliation
        .preview_reconciliation(
            &viewer,
            COMPANY,
            collections.id,
            &NewReconciliation {
                end_date: Some(feb_end),
                statement_balance: feb_balance.to_string(),
            },
            &Pins {
                through_entry_number: Some(stored.through_entry_number),
                through_statement_number: Some(stored.through_statement_number),
                through_match_event_number: Some(stored.through_match_event_number),
                through_opening_id: Some(stored.through_opening_id),
            },
        )
        .await?;
    let now = reconciliation
        .preview_reconciliation(
            &viewer,
            COMPANY,
            collections.id,
            &NewReconciliation {
                end_date: Some(feb_end),
                statement_balance: feb_balance.to_string(),
            },
            &Pins::default(),
        )
        .await?;
    println!(
        "stored snapshot equals the stored figures read back: {}",
        stored == snapshot
    );
    println!(
        "preview at the stored numbers equals the snapshot: {}",
        pinned.figures == stored
    );
    println!(
        "preview now, for the same date: difference {} with {} items, differing from the snapshot: {} (the late entry is new)",
        now.figures.difference,
        now.figures.items.len(),
        now.figures != stored
    );

    println!("\n== Finalising as an approval exception while duties are not separated");
    let march_end = date(2026, 3, 31);
    let march_balance = statement_balance(&lines, march_end);
    let march_open = reconciliation
        .open_reconciliation(
            &manager,
            COMPANY,
            collections.id,
            &opening(Some(march_end), &march_balance.to_string()),
        )
        .await?;
    println!("manager opens: {}", brief_reconciliation(&march_open));
    show(
        "the manager who opened it finalises while duties are separated",
        reconciliation
            .finalise_reconciliation(
                &manager,
                COMPANY,
                march_open.id,
                Some("Outstanding items checked"),
            )
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    let exceptions_before = ledger
        .list_approval_exceptions(&manager, COMPANY, &ApprovalExceptionQuery::default())
        .await?
        .len();
    ledger
        .set_duty_separation(&manager, COMPANY, false, Some("Reconciliation proof"))
        .await?;
    show(
        "the manager who opened it finalises while duties are not separated",
        reconciliation
            .finalise_reconciliation(
                &manager,
                COMPANY,
                march_open.id,
                Some("Outstanding items checked"),
            )
            .await
            .map(|r| brief_reconciliation(&r)),
    );
    ledger
        .set_duty_separation(&manager, COMPANY, true, None)
        .await?;
    let exceptions = ledger
        .list_approval_exceptions(&manager, COMPANY, &ApprovalExceptionQuery::default())
        .await?;
    for exception in exceptions.iter().skip(exceptions_before) {
        println!(
            "approval exception {}: {} {} {} #{} {:?} same_person={} prepared by {:?} approved by {} reason {:?}",
            exception.exception_number,
            exception.source,
            exception.action,
            exception.subject_type,
            exception.subject_id,
            exception.subject,
            exception.same_person,
            exception.prepared_by_user_id,
            exception.approved_by_user_id,
            exception.reason
        );
    }
    show(
        "viewer lists the bank account's reconciliations",
        reconciliation
            .list_reconciliations(&viewer, COMPANY, collections.id)
            .await
            .map(|rs| rs.iter().map(brief_reconciliation).collect::<Vec<_>>()),
    );

    println!("\n== The database's own refusals");
    raw(&raw_client, "a second standing match of a matched statement line", &format!(
        "BEGIN; WITH m AS (INSERT INTO finance_reconciliation_matches(tenant_id,company_id,bank_account_id,match_number,matched_by_user_id,matched_at) \
         VALUES(1,{COMPANY},{},NULL,1,NULL) RETURNING id) \
         INSERT INTO finance_reconciliation_match_lines(tenant_id,company_id,match_id,statement_line_id) SELECT 1,{COMPANY},id,{inv} FROM m; COMMIT",
        collections.id
    )).await;
    raw(&raw_client, "a second standing match of a matched entry", &format!(
        "BEGIN; WITH m AS (INSERT INTO finance_reconciliation_matches(tenant_id,company_id,bank_account_id,match_number,matched_by_user_id,matched_at) \
         VALUES(1,{COMPANY},{},NULL,1,NULL) RETURNING id) \
         INSERT INTO finance_reconciliation_match_entries(tenant_id,company_id,match_id,bank_account_id,entry_id,entry_number,entry_date,description,amount) \
         SELECT 1,{COMPANY},id,{},{},{},date '2026-02-02','x',300 FROM m; COMMIT",
        collections.id, collections.id, receipt.id, receipt.entry_number
    )).await;
    raw(&raw_client, "a match whose sides differ, at commit", &format!(
        "BEGIN; WITH m AS (INSERT INTO finance_reconciliation_matches(tenant_id,company_id,bank_account_id,match_number,matched_by_user_id,matched_at) \
         VALUES(1,{COMPANY},{},NULL,1,NULL) RETURNING id), \
         l AS (INSERT INTO finance_reconciliation_match_lines(tenant_id,company_id,match_id,statement_line_id) SELECT 1,{COMPANY},id,{unknown} FROM m RETURNING match_id), \
         e AS (INSERT INTO finance_reconciliation_match_entries(tenant_id,company_id,match_id,bank_account_id,entry_id,entry_number,entry_date,description,amount) \
               SELECT 1,{COMPANY},id,{},{},{},date '2026-02-26','x',-80 FROM m RETURNING match_id) \
         INSERT INTO finance_reconciliation_match_events(tenant_id,company_id,event_number,match_id,action,recorded_by_user_id,recorded_at) \
         SELECT 1,{COMPANY},NULL,id,'matched',1,NULL FROM m; COMMIT",
        collections.id, collections.id, cheque.id, cheque.entry_number
    )).await;
    raw(&raw_client, "an unmatched event for a match a finalised reconciliation counted", &format!(
        "INSERT INTO finance_reconciliation_match_events(tenant_id,company_id,event_number,match_id,action,reason,recorded_by_user_id,recorded_at) \
         VALUES(1,{COMPANY},NULL,{},'unmatched','Late',1,NULL)", matched.id
    )).await;
    raw(&raw_client, "a second matched event for a recorded match", &format!(
        "INSERT INTO finance_reconciliation_match_events(tenant_id,company_id,event_number,match_id,action,recorded_by_user_id,recorded_at) \
         VALUES(1,{COMPANY},NULL,{},'matched',1,NULL)", reversed.unmatched_match.id
    )).await;
    raw(&raw_client, "a line added to a recorded match", &format!(
        "INSERT INTO finance_reconciliation_match_lines(tenant_id,company_id,match_id,statement_line_id) VALUES(1,{COMPANY},{},{unknown})",
        matched.id
    )).await;
    raw(
        &raw_client,
        "UPDATE of a match event",
        "UPDATE finance_reconciliation_match_events SET reason = 'x'",
    )
    .await;
    raw(
        &raw_client,
        "DELETE of a match",
        "DELETE FROM finance_reconciliation_matches",
    )
    .await;
    raw(&raw_client, "a second open reconciliation", &format!(
        "BEGIN; INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) \
         VALUES(1,{COMPANY},{},NULL,date '2026-04-30',0,1,NULL); \
         INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) \
         VALUES(1,{COMPANY},{},NULL,date '2026-05-31',0,1,NULL); COMMIT",
        collections.id, collections.id
    )).await;
    raw(&raw_client, "a reconciliation ending before the last finalised one", &format!(
        "INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) \
         VALUES(1,{COMPANY},{},NULL,date '2026-03-01',0,1,NULL)", collections.id
    )).await;
    raw(&raw_client, "a finalised event with a difference", &format!(
        "BEGIN; WITH r AS (INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) \
         VALUES(1,{COMPANY},{},NULL,date '2026-04-30',0,1,NULL) RETURNING id), \
         o AS (INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,recorded_by_user_id,recorded_at) SELECT 1,{COMPANY},NULL,id,'opened',1,NULL FROM r) \
         INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,book_balance,difference,through_entry_number,through_statement_number,through_match_event_number,recorded_by_user_id,recorded_at) \
         SELECT 1,{COMPANY},NULL,id,'finalised',0,5,1,1,1,1,NULL FROM r; COMMIT",
        collections.id
    )).await;
    raw(&raw_client, "an item added to a finalised reconciliation", &format!(
        "INSERT INTO finance_reconciliation_reconciliation_items(tenant_id,company_id,reconciliation_id,item_number,kind,statement_line_id,item_date,description,amount) \
         VALUES(1,{COMPANY},{},99,'statement_line',{unknown},date '2026-02-28','late',10)", february_open.id
    )).await;
    raw(&raw_client, "an abandoned event for a finalised reconciliation", &format!(
        "INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,reason,recorded_by_user_id,recorded_at) \
         VALUES(1,{COMPANY},NULL,{},'abandoned','Late',1,NULL)", february_open.id
    )).await;
    raw(&raw_client, "an abandoned event without a reason", &format!(
        "BEGIN; WITH r AS (INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) \
         VALUES(1,{COMPANY},{},NULL,date '2026-04-30',0,1,NULL) RETURNING id), \
         o AS (INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,recorded_by_user_id,recorded_at) SELECT 1,{COMPANY},NULL,id,'opened',1,NULL FROM r) \
         INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,recorded_by_user_id,recorded_at) \
         SELECT 1,{COMPANY},NULL,id,'abandoned',1,NULL FROM r; COMMIT",
        collections.id
    )).await;
    raw(
        &raw_client,
        "UPDATE of a reconciliation item",
        "UPDATE finance_reconciliation_reconciliation_items SET amount = 0",
    )
    .await;
    let numbers: Vec<i64> = raw_client
        .query(
            "SELECT event_number FROM finance_reconciliation_match_events WHERE company_id = $1 ORDER BY event_number",
            &[&COMPANY],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    println!(
        "the company's match-event numbers, with the refused attempts above leaving no gap: {numbers:?}"
    );

    println!("\n== Audit and evidence");
    for kind in [
        "match",
        "match_line",
        "match_entry",
        "match_event",
        "reconciliation_record",
        "reconciliation_event",
        "reconciliation_item",
    ] {
        let count: i64 = raw_client
            .query_one(
                "SELECT count(*) FROM base_audit_mutations WHERE auditable_type = $1",
                &[&format!("finance.reconciliation.{kind}")],
            )
            .await?
            .get(0);
        let page = reconciliation
            .evidence_page(&viewer, COMPANY, kind, Some(500), None)
            .await?;
        println!(
            "finance.reconciliation.{kind}: {count} audited mutations, {} evidence rows",
            page.rows.len()
        );
    }
    show(
        "viewer reads the first reconciliation-item evidence row",
        reconciliation
            .evidence_page(&viewer, COMPANY, "reconciliation_item", Some(1), None)
            .await
            .map(|page| (page.next, page.rows.first().cloned())),
    );
    Ok(())
}
