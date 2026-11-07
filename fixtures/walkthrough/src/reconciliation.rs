//! The reconciliation walkthrough: `finance/reconciliation` bank accounts and
//! statement import on Company 001, in a disposable database, after the
//! attachments walkthrough (which affiliates the fixture's staff and viewer
//! with the company and configures Base Artifacts' storage). Every line is
//! the facade's actual answer, Base Artifacts' own or the database's refusal;
//! nothing here is a test double.
use crate::{sign_in, COMPANY};
use bilibi_database::Db;
use bilibi_finance_attachments::{AttachmentQuery, Attachments, NewFile};
use bilibi_finance_ledger::{AccountType, Ledger, NewAccount};
use bilibi_finance_reconciliation::{
    BankAccount, BankAccountEdit, ImportedStatement, NewBankAccount, PageOptions, Reconciliation,
    ReconciliationError, Statement, STATEMENT_SOURCE,
};
use bilibi_module_registry::Services;
use serde_json::{json, Value};
use std::fmt::Debug;
use tokio_postgres::Client;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn file(name: &str, content: impl Into<Vec<u8>>) -> NewFile {
    NewFile {
        file_name: name.into(),
        content: content.into(),
        content_type: None,
    }
}

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
        Err(ReconciliationError::InvalidLines(errors)) => {
            let errors: Vec<String> = errors
                .iter()
                .map(|e| format!("row {}: {}", e.row, e.message))
                .collect();
            println!("{step}: refused, unreadable rows ({})", errors.join("; "));
        }
        Err(ReconciliationError::DuplicateLines(lines)) => {
            let lines: Vec<String> = lines
                .iter()
                .map(|d| {
                    format!(
                        "row {} (line {}) is statement {} line {}",
                        d.row, d.line_number, d.statement_number, d.statement_line_number
                    )
                })
                .collect();
            println!(
                "{step}: refused, lines already imported ({})",
                lines.join("; ")
            );
        }
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

fn account(a: &BankAccount) -> String {
    format!(
        "#{} {:?} ledger={} charges={:?} interest={:?} window={} active={} mapping={}",
        a.id,
        a.name,
        a.ledger_account_id,
        a.charges_account_id,
        a.interest_account_id,
        a.match_window_days,
        a.active,
        serde_json::to_string(&a.import_mapping).unwrap_or_default()
    )
}

fn statement(s: &Statement) -> String {
    format!(
        "statement {} {:?} sha256={}.. {} to {} lines={} opening={:?} closing={:?} by user {}",
        s.statement_number,
        s.file_name,
        &s.file_sha256[..8],
        s.start_date,
        s.end_date,
        s.line_count,
        s.opening_balance.map(|m| m.to_string()),
        s.closing_balance.map(|m| m.to_string()),
        s.imported_by_user_id
    )
}

fn lines(s: &Statement) -> String {
    s.lines
        .iter()
        .map(|l| {
            format!(
                "  {} {} {:?} ref={:?} {}{}",
                l.line_number,
                l.line_date,
                l.description,
                l.reference,
                l.amount,
                l.running_balance
                    .map_or(String::new(), |b| format!(" balance {b}"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn imported(step: &str, result: Result<ImportedStatement, ReconciliationError>) {
    match result {
        Ok(done) => {
            println!(
                "{step}: {}, skipped {} zero rows\n{}",
                statement(&done.statement),
                done.skipped_count,
                lines(&done.statement)
            );
        }
        Err(error) => show::<()>(step, Err(error)),
    }
}

fn new_account(name: &str, ledger_account_id: i64, mapping: Value) -> NewBankAccount {
    NewBankAccount {
        name: name.into(),
        ledger_account_id,
        import_mapping: mapping,
        charges_account_id: None,
        interest_account_id: None,
        match_window_days: None,
        active: None,
        reason: None,
    }
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
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let reconciliation = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let raw_client = db.get().await?;

    println!("\n== Reconciliation: the chart a bank account maps");
    for (code, name, account_type) in [
        ("1810", "Savings at bank", AccountType::Asset),
        ("1820", "Petty cash", AccountType::Asset),
        ("1830", "Foreign currency account", AccountType::Asset),
        ("7000", "Interest income", AccountType::Income),
    ] {
        if ledger
            .list_accounts(&manager, COMPANY)
            .await?
            .iter()
            .all(|a| a.code != code)
        {
            ledger
                .create_account(
                    &manager,
                    COMPANY,
                    &NewAccount::new(code, name, account_type),
                )
                .await?;
        }
    }
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    println!(
        "asset accounts a bank account may map: {:?}",
        chart
            .iter()
            .filter(|a| a.active && a.account_type == AccountType::Asset)
            .map(|a| format!("{} {}", a.code, a.name))
            .collect::<Vec<_>>()
    );
    let id = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or("fixture account")
    };
    let (cash, savings, petty, foreign) = (id("1000")?, id("1810")?, id("1820")?, id("1830")?);
    let (payables, charges, interest) = (id("2000")?, id("6000")?, id("7000")?);
    let operating_mapping = json!({
        "date": "Date", "description": "Details", "reference": "Ref",
        "amount": "Amount", "balance": "Balance"
    });

    println!("\n== Bank accounts: who may add one, and what is refused");
    show(
        "viewer adds a bank account",
        reconciliation
            .create_bank_account(
                &viewer,
                COMPANY,
                &new_account("Operating account", cash, operating_mapping.clone()),
            )
            .await,
    );
    show(
        "the other tenant's manager adds one to Company 001",
        reconciliation
            .create_bank_account(
                &other,
                COMPANY,
                &new_account("Operating account", cash, operating_mapping.clone()),
            )
            .await,
    );
    show(
        "blank name",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account("   ", cash, operating_mapping.clone()),
            )
            .await,
    );
    show(
        "name over 200 characters",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account(&"n".repeat(201), cash, operating_mapping.clone()),
            )
            .await,
    );
    show(
        "mapped to a liability account",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account("Payables", payables, operating_mapping.clone()),
            )
            .await,
    );
    show(
        "mapped to an account that is not in the chart",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account("Nowhere", 999_999, operating_mapping.clone()),
            )
            .await,
    );
    let mut wrong_counters = new_account("Counters", cash, operating_mapping.clone());
    wrong_counters.charges_account_id = Some(interest);
    wrong_counters.interest_account_id = Some(charges);
    wrong_counters.match_window_days = Some(91);
    show(
        "charges on an income account, interest on an expense account, window 91",
        reconciliation
            .create_bank_account(&manager, COMPANY, &wrong_counters)
            .await,
    );
    for (step, mapping) in [
        ("mapping that is not a map", json!(["Date"])),
        (
            "mapping with an unknown setting",
            json!({"date": "Date", "description": "Details", "amount": "Amount", "colour": "red", "size": 3}),
        ),
        (
            "mapping without a date column",
            json!({"description": "Details", "amount": "Amount"}),
        ),
        (
            "mapping without an amount",
            json!({"date": "Date", "description": "Details"}),
        ),
        (
            "mapping with an amount and a debit",
            json!({"date": "Date", "description": "Details", "amount": "Amount", "debit": "Out"}),
        ),
        (
            "mapping with only a debit",
            json!({"date": "Date", "description": "Details", "debit": "Out"}),
        ),
        (
            "mapping with a delimiter nobody uses",
            json!({"date": "Date", "description": "Details", "amount": "Amount", "delimiter": "#"}),
        ),
        (
            "mapping with 21 header rows",
            json!({"date": "Date", "description": "Details", "amount": "Amount", "header_rows": 21}),
        ),
        (
            "headerless mapping that names a header",
            json!({"header_rows": 0, "date": "Date", "description": 2, "amount": 3}),
        ),
        (
            "mapping with a column position of zero",
            json!({"date": 0, "description": "Details", "amount": "Amount"}),
        ),
        (
            "mapping with an unknown date format",
            json!({"date": "Date", "description": "Details", "amount": "Amount", "date_format": "DD.MM.YY"}),
        ),
    ] {
        show(
            step,
            reconciliation
                .create_bank_account(&manager, COMPANY, &new_account("Mapped", cash, mapping))
                .await,
        );
    }

    println!("\n== Bank accounts: adding, changing and the change log");
    let mut operating = new_account("Operating account", cash, operating_mapping);
    operating.charges_account_id = Some(charges);
    operating.interest_account_id = Some(interest);
    operating.reason = Some("  Main current account  ".into());
    let operating = reconciliation
        .create_bank_account(&manager, COMPANY, &operating)
        .await?;
    println!("manager adds: {}", account(&operating));
    show(
        "the same ledger account again",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account(
                    "Another name",
                    cash,
                    json!({"date": "D", "description": "T", "amount": "A"}),
                ),
            )
            .await,
    );
    show(
        "the same name again",
        reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &new_account(
                    "Operating account",
                    savings,
                    json!({"date": "D", "description": "T", "amount": "A"}),
                ),
            )
            .await,
    );
    let savings_account = reconciliation
        .create_bank_account(
            &manager,
            COMPANY,
            &new_account(
                "Savings account",
                savings,
                json!({
                    "delimiter": ";", "header_rows": 2, "date": "Datum", "date_format": "DD/MM/YYYY",
                    "description": "Text", "debit": "Soll", "credit": "Haben", "decimal_separator": ","
                }),
            ),
        )
        .await?;
    println!("manager adds: {}", account(&savings_account));
    let petty_account = reconciliation
        .create_bank_account(
            &manager,
            COMPANY,
            &new_account(
                "Petty cash book",
                petty,
                json!({
                    "header_rows": 0, "delimiter": "|", "date": 1, "date_format": "YYYYMMDD",
                    "description": 2, "reference": 4, "amount": 3, "amount_sign": "withdrawals_positive"
                }),
            ),
        )
        .await?;
    println!("manager adds: {}", account(&petty_account));
    let foreign_account = reconciliation
        .create_bank_account(
            &manager,
            COMPANY,
            &new_account(
                "Foreign currency",
                foreign,
                json!({"header_rows": 0, "date": 1, "date_format": "DD-MMM-YY", "description": 2, "amount": 3}),
            ),
        )
        .await?;
    println!("manager adds: {}", account(&foreign_account));
    show(
        "viewer changes the name",
        reconciliation
            .update_bank_account(
                &viewer,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    name: Some("x".into()),
                    ..Default::default()
                },
            )
            .await,
    );
    show(
        "manager changes nothing",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    name: Some("Operating account".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|a| account(&a)),
    );
    show(
        "manager renames it to another account's name",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    name: Some("Savings account".into()),
                    ..Default::default()
                },
            )
            .await,
    );
    show(
        "manager sets the window to 91 and a liability as charges account",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    match_window_days: Some(91),
                    charges_account_id: Some(Some(payables)),
                    ..Default::default()
                },
            )
            .await,
    );
    show(
        "manager changes the window and drops the interest account",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    match_window_days: Some(7),
                    interest_account_id: Some(None),
                    reason: Some("Interest is booked by hand".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|a| account(&a)),
    );
    show(
        "manager puts the interest account back, with a reason",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                operating.id,
                &BankAccountEdit {
                    interest_account_id: Some(Some(interest)),
                    reason: Some("Booked automatically again".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|a| account(&a)),
    );
    show(
        "the other tenant reads bank account 1",
        reconciliation
            .get_bank_account(&other, COMPANY, operating.id)
            .await,
    );
    show(
        "viewer reads the accounts",
        reconciliation
            .list_bank_accounts(&viewer, COMPANY)
            .await
            .map(|accounts| accounts.iter().map(account).collect::<Vec<_>>()),
    );
    show(
        "the change log",
        reconciliation
            .list_bank_account_changes(&viewer, COMPANY, operating.id, &PageOptions::default())
            .await
            .map(|log| {
                log.iter()
                    .map(|c| {
                        format!(
                            "{} {:?} -> {:?} ({:?}) by {}",
                            c.field, c.old_value, c.new_value, c.reason, c.changed_by_user_id
                        )
                    })
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "the change log, a page after the third entry",
        reconciliation
            .list_bank_account_changes(
                &viewer,
                COMPANY,
                operating.id,
                &PageOptions {
                    limit: Some(2),
                    after: Some(3),
                },
            )
            .await
            .map(|log| log.iter().map(|c| c.field.clone()).collect::<Vec<_>>()),
    );
    show(
        "the change log with a limit of 0",
        reconciliation
            .list_bank_account_changes(
                &viewer,
                COMPANY,
                operating.id,
                &PageOptions {
                    limit: Some(0),
                    after: None,
                },
            )
            .await
            .map(|log| log.len()),
    );

    println!("\n== Statement import: who may, and what the file must be");
    let first = "Date,Details,Ref,Amount,Balance\n\
                 2026-01-09,Cheque 1007,CHQ1007,-250.00,1450.50\n\
                 2026-01-08,Statement note,,0.00,1700.50\n\
                 2026-01-07,\"Customer receipt, INV-1\",INV-1,\"1,200.50\",1700.50\n\
                 2026-01-05,Opening deposit,,500.00,500.00\n";
    show(
        "viewer imports",
        reconciliation
            .import_statement(&viewer, COMPANY, operating.id, &file("jan.csv", first))
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "the other tenant imports into Company 001",
        reconciliation
            .import_statement(&other, COMPANY, operating.id, &file("jan.csv", first))
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "an unknown bank account",
        reconciliation
            .import_statement(&manager, COMPANY, 999, &file("jan.csv", first))
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "a blank file name",
        reconciliation
            .import_statement(&manager, COMPANY, operating.id, &file("  ", first))
            .await
            .map(|s| s.statement.statement_number),
    );
    println!(
        "statements before any import: {:?}",
        reconciliation
            .list_statements(&manager, COMPANY, operating.id, &PageOptions::default())
            .await?
            .len()
    );
    imported(
        "staff imports a file written newest first, with a zero row and a quoted comma",
        reconciliation
            .import_statement(&staff, COMPANY, operating.id, &file("jan.csv", first))
            .await,
    );
    show(
        "the same file again",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                operating.id,
                &file("jan-copy.csv", first),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "the same file, but for another bank account",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                savings_account.id,
                &file("jan-copy.csv", first),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    let overlapping = "Date,Details,Ref,Amount,Balance\n\
                       2026-01-07,\"Customer receipt, INV-1\",INV-1,1200.5,1700.50\n\
                       2026-01-09,Cheque 1007,CHQ1007,-250,1450.50\n\
                       2026-01-12,Bank interest,,1.25,1451.75\n";
    show(
        "an overlapping export (12.5 and 12.50 are the same line)",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                operating.id,
                &file("jan-overlap.csv", overlapping),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    let next = "Date,Details,Ref,Amount,Balance\n\
                2026-01-12,Bank interest,,1.25,1451.75\n\
                2026-01-15,Transfer to savings,TRF-1,-400.00,1051.75\n";
    imported(
        "manager imports the part after the first statement",
        reconciliation
            .import_statement(&manager, COMPANY, operating.id, &file("jan-2.csv", next))
            .await,
    );
    for (step, content) in [
        (
            "a file with unreadable rows",
            "Date,Details,Ref,Amount,Balance\n31/02/2026,Bad date,,5.00,5.00\n2026-02-01,   ,,5.00,10.00\n2026-02-02,Words,,twelve,10.00\n2026-02-03,Fine print,,10.123,20.00\n2026-02-04,Late,,5.00,\n",
        ),
        (
            "a file lacking mapped columns",
            "Date,Details,Amount\n2026-02-01,Rent,5.00\n",
        ),
        (
            "a file with only a header",
            "Date,Details,Ref,Amount,Balance\n",
        ),
        (
            "a file with only zero rows",
            "Date,Details,Ref,Amount,Balance\n2026-02-01,Note,,0.00,10.00\n",
        ),
        (
            "a balance column that follows from neither direction",
            "Date,Details,Ref,Amount,Balance\n2026-02-01,One,,5.00,5.00\n2026-02-02,Two,,5.00,99.00\n2026-02-03,Three,,5.00,15.00\n",
        ),
        (
            "a quote in the middle of a field",
            "Date,Details,Ref,Amount,Balance\n2026-02-01,Bad \"quote\",,5.00,5.00\n",
        ),
        (
            "text after a closing quote",
            "Date,Details,Ref,Amount,Balance\n2026-02-01,\"Quoted\"junk,,5.00,5.00\n",
        ),
        (
            "a quote left open",
            "Date,Details,Ref,Amount,Balance\n2026-02-01,\"Never closed,,5.00,5.00\n",
        ),
    ] {
        show(
            step,
            reconciliation
                .import_statement(&manager, COMPANY, operating.id, &file("bad.csv", content))
                .await
                .map(|s| s.statement.statement_number),
        );
    }
    show(
        "a file that is not UTF-8 text",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                operating.id,
                &file("bad.csv", vec![0xff, 0xfe, 0x00]),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "an empty file",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                operating.id,
                &file("empty.csv", Vec::new()),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    show(
        "a file over 5 MiB",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                operating.id,
                &file("big.csv", vec![b'a'; 5 * 1024 * 1024 + 1]),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    let too_many: String = (0..=10_000)
        .map(|n| format!("20260101|Line {n}|1.00|\n"))
        .collect();
    show(
        "a file of 10,001 rows",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                petty_account.id,
                &file("many.csv", too_many),
            )
            .await
            .map(|s| s.statement.statement_number),
    );

    println!("\n== Statement import: other banks' layouts");
    let savings_csv = "Savings export 2026;;;\nDatum;Text;Soll;Haben\n\
                       20/01/2026;Fee;2,00;\n20/01/2026;Fee;2,00;\n21/01/2026;Interest;;(0,50)\n22/01/2026;Salary;;1.500,00\n";
    imported(
        "debit and credit columns, semicolons, decimal commas, two header rows, two identical fees on one day",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                savings_account.id,
                &file("savings.csv", savings_csv),
            )
            .await,
    );
    show(
        "the same savings file again",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                savings_account.id,
                &file("savings-again.csv", savings_csv),
            )
            .await
            .map(|s| s.statement.statement_number),
    );
    let petty_csv =
        "20260203|Rent|1500.00|PC-1\n20260204|Refund|(75.00)|\n20260204|Stamps|12.5|PC-2\n";
    imported(
        "no header row, columns by position, withdrawals positive",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                petty_account.id,
                &file("petty.psv", petty_csv),
            )
            .await,
    );
    imported(
        "month names and a two-digit year, with CR and DR markers",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                foreign_account.id,
                &NewFile {
                    file_name: "fx.csv".into(),
                    content: b"05-Mar-26,Wire in,100.00 CR\n06-Mar-26,Wire out,40.00 DR\n".to_vec(),
                    content_type: Some("application/vnd.ms-excel".into()),
                },
            )
            .await,
    );
    let big: String = (1..=10_000)
        .map(|n| format!("20260301|Batch item {n}|{n}.00|\n"))
        .collect();
    let started = std::time::Instant::now();
    let big_done = reconciliation
        .import_statement(&manager, COMPANY, petty_account.id, &file("batch.psv", big))
        .await;
    println!(
        "a 10,000 line file into the petty cash book: {} in {:?}",
        big_done.as_ref().map_or_else(
            |e| format!("refused ({e})"),
            |s| format!(
                "statement {} with {} lines",
                s.statement.statement_number, s.statement.line_count
            )
        ),
        started.elapsed()
    );

    println!("\n== Reads and the file kept as a source document");
    show(
        "list statements of the operating account",
        reconciliation
            .list_statements(&viewer, COMPANY, operating.id, &PageOptions::default())
            .await
            .map(|list| {
                list.iter()
                    .map(|s| {
                        format!(
                            "{} ({} lines, {} lines loaded)",
                            s.statement_number,
                            s.line_count,
                            s.lines.len()
                        )
                    })
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "list statements after statement 1",
        reconciliation
            .list_statements(
                &viewer,
                COMPANY,
                operating.id,
                &PageOptions {
                    limit: None,
                    after: Some(1),
                },
            )
            .await
            .map(|list| list.iter().map(|s| s.statement_number).collect::<Vec<_>>()),
    );
    let one = reconciliation.get_statement(&viewer, COMPANY, 1).await?;
    println!("viewer reads {}\n{}", statement(&one), lines(&one));
    println!(
        "  mapping it was read through: {}",
        serde_json::to_string(&one.import_mapping)?
    );
    show(
        "viewer reads statement 999",
        reconciliation
            .get_statement(&viewer, COMPANY, 999)
            .await
            .map(|s| s.statement_number),
    );
    show(
        "the other tenant reads statement 1",
        reconciliation
            .get_statement(&other, COMPANY, 1)
            .await
            .map(|s| s.statement_number),
    );
    let kept = attachments
        .list_company_attachments(
            &manager,
            COMPANY,
            &AttachmentQuery {
                source: Some(STATEMENT_SOURCE.into()),
                ..Default::default()
            },
        )
        .await?;
    for a in &kept {
        println!(
            "kept as attachment #{} {} {} {:?} {} {}B sha256={}.. retained until {}",
            a.attachment_number,
            a.source_label.as_deref().unwrap_or("-"),
            a.document_label,
            a.file_name,
            a.content_type,
            a.byte_size,
            &a.sha256[..8],
            a.retain_until
        );
    }
    if let Some(a) = kept.first() {
        let bytes = attachments.read_attachment(&manager, COMPANY, a.id).await?;
        println!(
            "the first attachment's bytes equal the uploaded file: {}; its sha256 is statement 1's: {}",
            bytes.bytes == first.as_bytes(),
            bytes.sha256 == one.file_sha256
        );
    }
    println!(
        "statements with a kept file / statements: {} / {}",
        kept.len(),
        {
            raw_client
                .query_one(
                    "SELECT count(*) FROM finance_reconciliation_statements",
                    &[],
                )
                .await?
                .get::<_, i64>(0)
        }
    );

    println!("\n== Two sessions import the same new file at once");
    let racing = "Date,Details,Ref,Amount,Balance\n2026-01-20,Race,,10.00,1061.75\n";
    let (race_a, race_b) = (file("race-a.csv", racing), file("race-b.csv", racing));
    let (one, two) = tokio::join!(
        reconciliation.import_statement(&manager, COMPANY, operating.id, &race_a),
        reconciliation.import_statement(&staff, COMPANY, operating.id, &race_b),
    );
    println!(
        "one imported, the other refused: {}",
        match (&one, &two) {
            (Ok(done), Err(error)) | (Err(error), Ok(done)) =>
                format!("statement {} / {error}", done.statement.statement_number),
            _ => format!("{:?} / {:?}", one.as_ref().err(), two.as_ref().err()),
        }
    );

    crate::reconciliation_first_import::run(&reconciliation, &ledger, &manager, &staff).await?;

    println!("\n== Deactivated bank accounts import no more");
    show(
        "manager deactivates the foreign currency account",
        reconciliation
            .update_bank_account(
                &manager,
                COMPANY,
                foreign_account.id,
                &BankAccountEdit {
                    active: Some(false),
                    reason: Some("Closed".into()),
                    ..Default::default()
                },
            )
            .await
            .map(|a| account(&a)),
    );
    show(
        "import into it",
        reconciliation
            .import_statement(
                &manager,
                COMPANY,
                foreign_account.id,
                &file("fx2.csv", "07-Mar-26,Wire in,5.00\n"),
            )
            .await
            .map(|s| s.statement.statement_number),
    );

    println!("\n== The database's own refusals");
    raw(
        &raw_client,
        "UPDATE of a statement",
        "UPDATE finance_reconciliation_statements SET file_name = 'x'",
    )
    .await;
    raw(
        &raw_client,
        "DELETE of a statement line",
        "DELETE FROM finance_reconciliation_statement_lines",
    )
    .await;
    raw(
        &raw_client,
        "TRUNCATE of the statements",
        "TRUNCATE finance_reconciliation_statements CASCADE",
    )
    .await;
    raw(
        &raw_client,
        "UPDATE of a change-log row",
        "UPDATE finance_reconciliation_bank_account_changes SET new_value = 'x'",
    )
    .await;
    raw(
        &raw_client,
        "DELETE of a bank account",
        "DELETE FROM finance_reconciliation_bank_accounts",
    )
    .await;
    raw(
        &raw_client,
        "moving a bank account to another ledger account",
        "UPDATE finance_reconciliation_bank_accounts SET ledger_account_id = 1 + ledger_account_id",
    )
    .await;
    raw(&raw_client, "a statement line added to a recorded statement", "INSERT INTO finance_reconciliation_statement_lines(tenant_id,company_id,statement_id,bank_account_id,line_number,line_date,description,amount,fingerprint) SELECT tenant_id,company_id,id,bank_account_id,99,start_date,'late',1,repeat('a',64) FROM finance_reconciliation_statements WHERE statement_number = 1").await;
    raw(&raw_client, "a statement that counts lines it does not hold", "BEGIN; INSERT INTO finance_reconciliation_statements(tenant_id,company_id,bank_account_id,statement_number,file_name,file_sha256,import_mapping,line_count,start_date,end_date,imported_by_user_id,imported_at) SELECT tenant_id,company_id,bank_account_id,NULL,'x.csv',repeat('b',64),import_mapping,3,start_date,end_date,1,NULL FROM finance_reconciliation_statements WHERE statement_number = 1; COMMIT").await;
    raw_client.batch_execute("ROLLBACK").await.ok();
    raw(&raw_client, "a second statement for a file already imported", "INSERT INTO finance_reconciliation_statements(tenant_id,company_id,bank_account_id,statement_number,file_name,file_sha256,import_mapping,line_count,start_date,end_date,imported_by_user_id,imported_at) SELECT tenant_id,company_id,bank_account_id,NULL,'copy.csv',file_sha256,import_mapping,1,start_date,end_date,1,NULL FROM finance_reconciliation_statements WHERE statement_number = 1").await;
    raw(&raw_client, "a match with no lines or entries, at commit", "BEGIN; INSERT INTO finance_reconciliation_matches(tenant_id,company_id,bank_account_id,match_number,matched_by_user_id,matched_at) SELECT tenant_id,company_id,id,NULL,1,NULL FROM finance_reconciliation_bank_accounts WHERE name = 'Operating account'; COMMIT").await;
    raw_client.batch_execute("ROLLBACK").await.ok();
    raw(&raw_client, "a reconciliation with no opened event, at commit", "BEGIN; INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_at) SELECT tenant_id,company_id,id,NULL,date '2026-01-31',0,1,NULL FROM finance_reconciliation_bank_accounts WHERE name = 'Operating account'; COMMIT").await;
    raw_client.batch_execute("ROLLBACK").await.ok();
    let numbers: Vec<i64> = raw_client
        .query("SELECT statement_number FROM finance_reconciliation_statements WHERE company_id = 1 ORDER BY statement_number", &[])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    println!(
        "the company's statement numbers, with the refused attempts above leaving no gap: {numbers:?}"
    );

    println!("\n== Audit");
    for kind in [
        "bank_account",
        "bank_account_change",
        "statement",
        "statement_line",
    ] {
        let count: i64 = raw_client
            .query_one(
                "SELECT count(*) FROM base_audit_mutations WHERE auditable_type = $1",
                &[&format!("finance.reconciliation.{kind}")],
            )
            .await?
            .get(0);
        println!("audit mutations recorded for finance.reconciliation.{kind}: {count}");
    }
    show(
        "viewer reads the statement evidence page",
        reconciliation
            .evidence_page(&viewer, COMPANY, "statement", Some(2), None)
            .await
            .map(|page| {
                (
                    page.fields.len(),
                    page.rows.len(),
                    page.next,
                    page.rows.first().cloned(),
                )
            }),
    );
    show(
        "viewer reads the statement-line evidence page after line 5",
        reconciliation
            .evidence_page(&viewer, COMPANY, "statement_line", Some(2), Some(5))
            .await
            .map(|page| (page.rows.len(), page.next, page.rows.first().cloned())),
    );
    show(
        "an unknown dataset",
        reconciliation
            .evidence_page(&viewer, COMPANY, "secret", None, None)
            .await
            .map(|_| ()),
    );
    show(
        "a limit of 501",
        reconciliation
            .evidence_page(&viewer, COMPANY, "match", Some(501), None)
            .await
            .map(|_| ()),
    );
    show(
        "the other tenant reads the evidence",
        reconciliation
            .evidence_page(&other, COMPANY, "statement", None, None)
            .await
            .map(|_| ()),
    );
    println!("datasets: {:?}", Reconciliation::evidence_datasets());
    let lines_held: i64 = raw_client
        .query_one(
            "SELECT count(*) FROM finance_reconciliation_statement_lines",
            &[],
        )
        .await?
        .get(0);
    println!("statement lines held: {lines_held}");
    Ok(())
}
