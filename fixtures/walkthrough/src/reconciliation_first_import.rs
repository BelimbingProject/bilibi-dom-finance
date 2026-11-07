//! A bank account added with its name and ledger account only, whose bank's
//! columns are mapped on its first statement import (design 5, "add the
//! account, then map columns on the first import"): the import refuses until
//! the columns are mapped, the preview guesses them from the file's header
//! row, the mapped import keeps them with the account in one transaction, the
//! next file imports through them, and replacing them is a bank account
//! change. Run inside the reconciliation walkthrough, on Company 001; every
//! line is the facade's own answer.
use crate::COMPANY;
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::{AccountType, Ledger, NewAccount};
use bilibi_finance_reconciliation::{NewBankAccount, Reconciliation, ReconciliationError};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn file(name: &str, content: &str) -> NewFile {
    NewFile {
        file_name: name.into(),
        content: content.as_bytes().to_vec(),
        content_type: None,
    }
}

fn said<T>(result: &Result<T, ReconciliationError>) -> String {
    match result {
        Ok(_) => "accepted".into(),
        Err(ReconciliationError::Invalid(errors)) => format!(
            "refused ({})",
            errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Err(error) => format!("refused ({error:?})"),
    }
}

pub async fn run(
    reconciliation: &Reconciliation,
    ledger: &Ledger,
    manager: &Scope,
    staff: &Scope,
) -> Outcome<()> {
    println!("\n== A bank account mapped on its first import");
    let ledger_account = match ledger
        .list_accounts(manager, COMPANY)
        .await?
        .into_iter()
        .find(|a| a.code == "1895")
    {
        Some(account) => account.id,
        None => {
            ledger
                .create_account(
                    manager,
                    COMPANY,
                    &NewAccount::new("1895", "Second current account", AccountType::Asset),
                )
                .await?
                .id
        }
    };
    let account = reconciliation
        .create_bank_account(
            manager,
            COMPANY,
            &NewBankAccount {
                name: "Second current account".into(),
                ledger_account_id: ledger_account,
                import_mapping: Value::Null,
                charges_account_id: None,
                interest_account_id: None,
                match_window_days: None,
                active: None,
                reason: None,
            },
        )
        .await?;
    println!(
        "manager adds it with a name and ledger account only: mapping {:?}",
        account.import_mapping
    );
    assert!(account.import_mapping.is_none());

    // A bank's export with a title row above its header, money out and in
    // in separate columns, day-first dates and a running balance.
    let first = "Account statement 1234-5678\n\
                 Txn Date,Transaction Description,Ref No,Withdrawal,Deposit,Balance\n\
                 01/03/2026,Opening transfer,TRF-1,,1000.00,1000.00\n\
                 02/03/2026,Service charge,FEE-1,12.50,,987.50\n\
                 04/03/2026,\"Receipt, INV-9\",INV-9,,250.00,1237.50\n";
    let refused = reconciliation
        .import_statement(manager, COMPANY, account.id, &file("march.csv", first))
        .await;
    println!(
        "an import before the columns are mapped: {}",
        said(&refused)
    );
    assert!(matches!(refused, Err(ReconciliationError::MappingNotSet)));

    let preview = reconciliation
        .preview_statement(manager, COMPANY, account.id, first.as_bytes(), None)
        .await?;
    println!(
        "the preview guesses (guessed={}): {}",
        preview.guessed, preview.mapping
    );
    println!(
        "its columns {:?}; first row {:?}; {} lines would import, the first {:?}; refusal {:?}",
        preview.columns,
        preview.rows.first(),
        preview.line_count,
        preview
            .lines
            .first()
            .map(|l| format!("{} {} {}", l.date, l.description, l.amount)),
        preview.refusal
    );
    assert!(preview.guessed && preview.refusal.is_none() && preview.line_count == 3);
    assert_eq!(preview.mapping["header_rows"], json!(2));
    assert_eq!(preview.mapping["date"], json!("Txn Date"));
    assert_eq!(preview.mapping["date_format"], json!("DD/MM/YYYY"));
    assert_eq!(preview.mapping["debit"], json!("Withdrawal"));
    assert_eq!(preview.mapping["credit"], json!("Deposit"));
    assert_eq!(preview.mapping["balance"], json!("Balance"));

    let mut wrong = preview.mapping.clone();
    wrong["description"] = Value::Null;
    let refused = reconciliation
        .preview_statement(manager, COMPANY, account.id, first.as_bytes(), Some(&wrong))
        .await?;
    println!(
        "the preview with no description column: refusal {:?}",
        refused.refusal.as_ref().map(|r| r.to_string())
    );
    let refused = reconciliation
        .import_statement_mapped(
            manager,
            COMPANY,
            account.id,
            &file("march.csv", first),
            &wrong,
        )
        .await;
    println!("importing through it: {}", said(&refused));
    let after = reconciliation
        .get_bank_account(manager, COMPANY, account.id)
        .await?;
    println!(
        "the account's mapping after the refusal: {:?}",
        after.import_mapping
    );
    assert!(after.import_mapping.is_none());

    // The first mapping is set by whoever imports: staff imports here.
    let done = reconciliation
        .import_statement_mapped(
            staff,
            COMPANY,
            account.id,
            &file("march.csv", first),
            &preview.mapping,
        )
        .await?;
    println!(
        "staff imports through the guess: statement {}, {} lines {} to {}",
        done.statement.statement_number,
        done.statement.line_count,
        done.statement.start_date,
        done.statement.end_date
    );
    let kept = reconciliation
        .get_bank_account(manager, COMPANY, account.id)
        .await?;
    println!(
        "the account keeps the columns: {}",
        serde_json::to_string(&kept.import_mapping)?
    );
    assert!(kept.import_mapping.is_some());
    let log = reconciliation
        .list_bank_account_changes(manager, COMPANY, account.id, &Default::default())
        .await?;
    println!(
        "its change log: {:?}",
        log.iter()
            .map(|c| format!(
                "{} by user {} ({:?})",
                c.field, c.changed_by_user_id, c.reason
            ))
            .collect::<Vec<_>>()
    );

    let second = "Account statement 1234-5678\n\
                  Txn Date,Transaction Description,Ref No,Withdrawal,Deposit,Balance\n\
                  05/03/2026,Supplier payment,CHQ-2,100.00,,1137.50\n";
    let next = reconciliation
        .import_statement(manager, COMPANY, account.id, &file("march-2.csv", second))
        .await?;
    println!(
        "the next file imports in one step: statement {}, {} line",
        next.statement.statement_number, next.statement.line_count
    );

    // Replacing a different mapping is a bank account change.
    let mut changed = preview.mapping.clone();
    changed["reference"] = Value::Null;
    let third = "Account statement 1234-5678\n\
                 Txn Date,Transaction Description,Ref No,Withdrawal,Deposit,Balance\n\
                 06/03/2026,Card refund,,,20.00,1157.50\n";
    let by_staff = reconciliation
        .import_statement_mapped(
            staff,
            COMPANY,
            account.id,
            &file("march-3.csv", third),
            &changed,
        )
        .await;
    println!("staff imports through other columns: {}", said(&by_staff));
    assert!(matches!(by_staff, Err(ReconciliationError::Unauthorized)));
    let by_manager = reconciliation
        .import_statement_mapped(
            manager,
            COMPANY,
            account.id,
            &file("march-3.csv", third),
            &changed,
        )
        .await;
    println!(
        "manager imports through other columns: {}",
        said(&by_manager)
    );
    assert!(by_manager.is_ok());
    let kept = reconciliation
        .get_bank_account(manager, COMPANY, account.id)
        .await?;
    println!(
        "the account now reads the reference from {:?}",
        kept.import_mapping.and_then(|m| m.reference)
    );
    Ok(())
}
