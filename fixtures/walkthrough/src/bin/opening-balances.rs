//! Disposable conversion proof through the real facade, never product data.
//! Run on a fresh database: cargo run -p bilibi-finance-walkthrough-fixture
//! --bin opening-balances. UI proof: ledger-ui opening. MCP proof: the
//! agent-operations credential fixture and opening-balances-operations.py.
use bilibi_database::pool;
use bilibi_finance_ledger::{Ledger, LedgerError, Money, OpeningBalances, Reversal};
use bilibi_finance_walkthrough_fixture::{install, services, sign_in, COMPANY};
use jiff::civil::date;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = services(&db, &modules)?;
    install(&db, &services).await?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let reviewer = sign_in(&db, &services, 1, 4).await?;
    let viewer = sign_in(&db, &services, 1, 2).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let mut source = OpeningBalances {
        conversion_date: date(2026, 7, 1), currency_code: "MYR".into(),
        source_reference: "SOURCE-CLOSE-2026-06-30".into(),
        replaces_journal_id: None,
        csv: "source_id,account_code,debit,credit\nbank,1000,10000,\nreceivables,1100,3000,\ncost,5000,2000,\npayables,2000,,1500\ncapital,3000,,8000\nsales,4000,,5500\n".into(),
    };
    let original = source.csv.clone();
    let mut invalids = 0;
    for csv in [
        original.replace("5500", "5501"),
        original.replace("bank,", "bank\0,"),
        original.replace("1000,", "unknown,"),
        original.replace("receivables,", "bank,"),
        original.replace("10000,", "10000.001,"),
        original.replace("10000,", "-10000,"),
        original.replace("10000,", "10000,2"),
        "source_id,account_code,debit,credit\n\"open".into(),
    ] {
        source.csv = csv;
        assert!(matches!(
            ledger
                .submit_opening_balances(&manager, COMPANY, &source)
                .await,
            Err(LedgerError::Invalid(_))
        ));
        invalids += 1;
    }
    source.csv = original.clone();
    source.currency_code = "USD".into();
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    source.currency_code = "MYR".into();
    assert!(matches!(
        ledger
            .submit_opening_balances(&viewer, COMPANY, &source)
            .await,
        Err(LedgerError::Unauthorized)
    ));
    assert_eq!(
        ledger
            .trial_balance(&manager, COMPANY, source.conversion_date, None)
            .await?
            .through_entry_number,
        0
    );
    println!(
        "opening: {invalids} malformed, unmapped, precision or unbalanced files, wrong currency and viewer refused; no posting"
    );

    // The company lock makes two independent retries one source submission.
    let competing = {
        let ledger = ledger.clone();
        let manager = manager.clone();
        let source = source.clone();
        tokio::spawn(async move {
            ledger
                .submit_opening_balances(&manager, COMPANY, &source)
                .await
        })
    };
    let a = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await;
    let b = competing.await?;
    let journal = a?;
    assert_eq!(journal.id, b?.id);
    assert_eq!(
        ledger
            .trial_balance(&manager, COMPANY, source.conversion_date, None)
            .await?
            .through_entry_number,
        0
    );
    assert!(matches!(
        ledger.approve_journal(&manager, COMPANY, journal.id).await,
        Err(LedgerError::ApprovalRefused(message)) if message.contains("cannot decide this journal")
    ));
    let rejected = ledger
        .reject_journal(
            &reviewer,
            COMPANY,
            journal.id,
            "Source closing report superseded before approval",
        )
        .await?;
    assert_eq!(
        rejected.id,
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await?
            .id
    );
    // A withdrawn opening with no replacement no longer blocks a new submission.
    source.source_reference.push_str("-REVISED");
    let withdrawn = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    ledger
        .withdraw_journal(
            &manager,
            COMPANY,
            withdrawn.id,
            Some("Wrong export selected"),
        )
        .await?;
    assert_eq!(
        withdrawn.id,
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await?
            .id
    );
    assert!(matches!(
        ledger
            .approve_journal(&reviewer, COMPANY, withdrawn.id)
            .await,
        Err(LedgerError::JournalDecided)
    ));
    let wrong = original
        .replace("10000,", "10001,")
        .replace(",8000\n", ",8001\n");
    source.csv = wrong;
    source.source_reference.push_str("-RESUBMITTED");
    let first = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;

    // Only the end of a replacement chain decides whether an opening is live:
    // a rejected successor frees the book, as does a withdrawn one.
    let base_reference = source.source_reference.clone();
    source.source_reference = format!("{base_reference}-CHAIN-1");
    source.replaces_journal_id = Some(first.id);
    let successor = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    assert_eq!(successor.replaces_journal_id, Some(first.id));
    ledger
        .reject_journal(
            &reviewer,
            COMPANY,
            successor.id,
            "Successor export also wrong",
        )
        .await?;
    source.replaces_journal_id = None;
    source.source_reference = format!("{base_reference}-CHAIN-2");
    let second = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    source.source_reference = format!("{base_reference}-CHAIN-3");
    source.replaces_journal_id = Some(second.id);
    let successor = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    ledger
        .withdraw_journal(&manager, COMPANY, successor.id, Some("Withdrawn again"))
        .await?;
    source.replaces_journal_id = None;
    source.source_reference = format!("{base_reference}-CHAIN-4");
    let resubmitted = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    for decided in [first.id, second.id, successor.id] {
        assert!(matches!(
            ledger.approve_journal(&reviewer, COMPANY, decided).await,
            Err(LedgerError::JournalDecided)
        ));
    }

    // One live opening: another source cannot sit beside it, and the generic
    // manual correction would drop its source evidence, so both are refused.
    let other = OpeningBalances {
        conversion_date: source.conversion_date,
        currency_code: "MYR".into(),
        source_reference: "SOURCE-OTHER".into(),
        csv: original.clone(),
        replaces_journal_id: None,
    };
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &other)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    let units = ledger.get_book(&manager, COMPANY).await?.minor_units;
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).map(|a| a.id);
    let manual = bilibi_finance_ledger::NewEntry {
        entry_date: source.conversion_date,
        description: "Opening correction typed as a manual journal".into(),
        reference: None,
        lines: vec![
            bilibi_finance_ledger::NewLine {
                account_id: account("1000").ok_or("bank")?,
                debit: Money::parse("10000", units)?,
                credit: Money::ZERO,
                description: None,
            },
            bilibi_finance_ledger::NewLine {
                account_id: account("3000").ok_or("capital")?,
                debit: Money::ZERO,
                credit: Money::parse("10000", units)?,
                description: None,
            },
        ],
    };
    assert!(matches!(
        ledger
            .replace_journal(&manager, COMPANY, resubmitted.id, &manual, None)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    let unrelated = ledger.submit_journal(&manager, COMPANY, &manual).await?;
    let mut misdirected = OpeningBalances {
        source_reference: "SOURCE-MISDIRECTED".into(),
        replaces_journal_id: Some(unrelated.id),
        ..other.clone()
    };
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &misdirected)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    ledger
        .withdraw_journal(&manager, COMPANY, unrelated.id, None)
        .await?;
    misdirected.replaces_journal_id = Some(withdrawn.id);
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &misdirected)
            .await,
        Err(LedgerError::JournalDecided)
    ));

    // Correcting the waiting opening replaces it with one that carries the
    // opening record; the original can never be approved.
    source.csv = original.clone();
    source.source_reference = "SOURCE-CLOSE-2026-06-30-CORRECTED".into();
    source.replaces_journal_id = Some(resubmitted.id);
    let journal = ledger
        .submit_opening_balances(&manager, COMPANY, &source)
        .await?;
    assert_eq!(journal.replaces_journal_id, Some(resubmitted.id));
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, resubmitted.id)
            .await?
            .replaced_by_journal_id,
        Some(journal.id)
    );
    assert_eq!(
        journal.id,
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await?
            .id
    );
    assert!(matches!(
        ledger
            .approve_journal(&reviewer, COMPANY, resubmitted.id)
            .await,
        Err(LedgerError::JournalDecided)
    ));
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &other)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    source.replaces_journal_id = None;
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    source.replaces_journal_id = Some(resubmitted.id);
    let posted = ledger
        .approve_journal(&reviewer, COMPANY, journal.id)
        .await?;
    let entry_id = posted
        .decision
        .as_ref()
        .and_then(|d| d.entry_id)
        .ok_or("approved entry")?;
    let entry = ledger.get_entry(&manager, COMPANY, entry_id).await?;
    assert_eq!(entry.lines.len(), 6);
    let trial = ledger
        .trial_balance(&manager, COMPANY, source.conversion_date, None)
        .await?;
    assert_eq!(trial.total_debit.shown(trial.minor_units), "15000.00");
    assert_eq!(trial.total_credit, trial.total_debit);
    for (code, debit, credit) in [
        ("1000", "10000.00", "0.00"),
        ("1100", "3000.00", "0.00"),
        ("5000", "2000.00", "0.00"),
        ("2000", "0.00", "1500.00"),
        ("3000", "0.00", "8000.00"),
        ("4000", "0.00", "5500.00"),
    ] {
        let row = trial
            .rows
            .iter()
            .find(|r| r.account.code == code)
            .ok_or("account balance")?;
        assert_eq!(row.debit_balance.shown(trial.minor_units), debit);
        assert_eq!(row.credit_balance.shown(trial.minor_units), credit);
    }
    assert_eq!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await?
            .id,
        journal.id
    );
    source.csv = source.csv.replace("10000", "10001");
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    source.csv = source.csv.replace("10001", "10000");
    source.replaces_journal_id = None;
    source.source_reference.push_str("-DUPLICATE");
    assert!(matches!(
        ledger
            .submit_opening_balances(&manager, COMPANY, &source)
            .await,
        Err(LedgerError::Invalid(_))
    ));
    println!(
        "opening: racing retries share a journal; same-person approval refused; rejected and withdrawn openings resubmitted with new references; a waiting opening corrected by replacement that carries the opening record; a second live opening, a manual replacement and a decided original all refused; six account balances reconcile to 15000.00; posted retry creates no entry"
    );

    ledger
        .reverse_entry(
            &manager,
            COMPANY,
            entry_id,
            &Reversal {
                entry_date: source.conversion_date,
                description: "Correct conversion figures against the revised source closing report"
                    .into(),
                reference: Some("CORRECTION-1".into()),
            },
        )
        .await?;
    assert_eq!(
        ledger
            .trial_balance(&manager, COMPANY, source.conversion_date, None)
            .await?
            .total_debit
            .shown(trial.minor_units),
        "0.00"
    );
    let bank = trial
        .rows
        .iter()
        .find(|r| r.account.code == "1000")
        .ok_or("bank")?
        .account
        .id;
    let capital = trial
        .rows
        .iter()
        .find(|r| r.account.code == "3000")
        .ok_or("capital")?
        .account
        .id;
    let mut lines: Vec<_> = entry
        .lines
        .iter()
        .map(|line| bilibi_finance_ledger::NewLine {
            account_id: line.account_id,
            debit: line.debit,
            credit: line.credit,
            description: line.description.clone(),
        })
        .collect();
    for line in &mut lines {
        if line.account_id == bank {
            line.debit = Money::parse("10100", trial.minor_units)?;
        }
        if line.account_id == capital {
            line.credit = Money::parse("8100", trial.minor_units)?;
        }
    }
    let corrected = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &bilibi_finance_ledger::NewEntry {
                entry_date: source.conversion_date,
                description: "Corrected opening balances against revised source".into(),
                reference: Some("CORRECTION-2".into()),
                lines,
            },
        )
        .await?;
    ledger
        .approve_journal(&reviewer, COMPANY, corrected.id)
        .await?;
    let revised = ledger
        .trial_balance(&manager, COMPANY, source.conversion_date, None)
        .await?;
    assert_eq!(revised.total_debit.shown(trial.minor_units), "15100.00");
    assert_eq!(
        revised
            .rows
            .iter()
            .find(|r| r.account.id == bank)
            .ok_or("revised bank")?
            .debit_balance
            .shown(trial.minor_units),
        "10100.00"
    );
    assert_eq!(
        revised
            .rows
            .iter()
            .find(|r| r.account.id == capital)
            .ok_or("revised capital")?
            .credit_balance
            .shown(trial.minor_units),
        "8100.00"
    );
    assert_eq!(
        ledger
            .trial_balance(
                &manager,
                COMPANY,
                source.conversion_date,
                Some(trial.through_entry_number)
            )
            .await?,
        trial
    );
    let evidence = ledger
        .evidence_page(&manager, COMPANY, "opening_balances", Some(500), None)
        .await?;
    assert_eq!(evidence.rows.len(), 8);
    assert_eq!(
        evidence.rows[7]["source_csv"].as_str(),
        Some(source.csv.as_str())
    );
    let client = db.get().await?;
    let audited: i64 = client.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type='finance.ledger.opening_balances'", &[]).await?.get(0);
    assert_eq!(audited, 8);
    assert!(client.execute("UPDATE finance_ledger_opening_balances SET source_csv='overwritten' WHERE tenant_id=1 AND company_id=1", &[]).await.is_err());
    println!("opening: reversal and independently approved correction reconcile revised balances to 15100.00; original pin reproduces 15000.00; eight source evidence records (rejected, withdrawn, replaced in chains and approved) and their audit changes retained; database refuses evidence overwrite");
    println!(
        "EXCLUDED: settleable open invoices/bills, aging and control tie-outs, bank reconciliation items, asset registers, tax return detail, historical/comparative migration; no complete conversion claim"
    );
    Ok(())
}
