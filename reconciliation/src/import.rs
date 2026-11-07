//! Statement import (Bilimbi `Reconciliation.Import`): a bank's CSV export
//! read through the bank account's mapping into an immutable statement and
//! its lines. The facade authorizes the call and holds the company lock for
//! the write; [`prepare`] runs before it, so everything that can be refused is
//! refused before the file is stored.
use crate::{
    csv::{self, CsvError},
    mapping::{self, ImportMapping},
    BankAccount, DuplicateLine, LineError, ReconciliationError,
};
use bilibi_database::{Change, MutationTransaction, Result as DbResult};
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::{MinorUnits, Money, MoneyError};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio_postgres::GenericClient;

/// The most lines one statement may hold.
pub const MAX_LINES: usize = 10_000;
const DEFAULT_CONTENT_TYPE: &str = "text/csv";
const FILE_NAME_LIMIT: usize = 255;
const DESCRIPTION_LIMIT: usize = 500;
const REFERENCE_LIMIT: usize = 100;

/// One line read from the file.
struct Line {
    /// The row of the file it came from, among its non-blank rows.
    row: usize,
    line_number: i32,
    date: Date,
    description: String,
    reference: Option<String>,
    amount: Money,
    balance: Option<Money>,
    fingerprint: String,
}

/// A statement ready to record: everything [`prepare`] checked.
pub(crate) struct Prepared {
    /// The file to keep: trimmed name, the bytes, `text/csv` unless given.
    pub file: NewFile,
    sha256: String,
    lines: Vec<Line>,
    pub skipped: usize,
    start_date: Date,
    pub end_date: Date,
    mapping: ImportMapping,
}

/// Reads `file` as a statement of the bank account, before the transaction
/// that records it: refuses, whole, a file already imported, a line already
/// imported, a file whose rows do not read through the mapping, a line before
/// an opening bridge's conversion date, and a running balance that does not
/// follow from the amounts.
pub(crate) async fn prepare(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    mapping: &ImportMapping,
    units: MinorUnits,
    file: &NewFile,
) -> Result<Prepared, ReconciliationError> {
    let name = file.file_name.trim();
    if name.is_empty() || name.chars().count() > FILE_NAME_LIMIT {
        return Err(ReconciliationError::InvalidFileName);
    }
    let sha256 = hash(&file.content);
    fresh_file(client, tenant, company_id, &sha256).await?;
    let (lines, skipped) = read(&file.content, mapping, units)?;
    let lines = fingerprinted(ordered(lines, mapping)?);
    fresh_lines(client, tenant, company_id, bank_account_id, &lines).await?;
    check_cutover(client, tenant, company_id, bank_account_id, &lines).await?;
    let dates = lines.iter().map(|line| line.date);
    Ok(Prepared {
        file: NewFile {
            file_name: name.to_owned(),
            content: file.content.clone(),
            content_type: Some(
                file.content_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|type_| !type_.is_empty())
                    .unwrap_or(DEFAULT_CONTENT_TYPE)
                    .to_owned(),
            ),
        },
        sha256,
        skipped,
        start_date: dates.clone().min().expect("a statement has lines"),
        end_date: dates.max().expect("a statement has lines"),
        mapping: mapping.clone(),
        lines,
    })
}

/// The duplicate and opening-cutoff checks again, under the company lock:
/// another import or bridge may have landed since [`prepare`].
pub(crate) async fn check_fresh(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    prepared: &Prepared,
) -> Result<(), ReconciliationError> {
    fresh_file(client, tenant, company_id, &prepared.sha256).await?;
    fresh_lines(client, tenant, company_id, bank_account_id, &prepared.lines).await?;
    check_cutover(client, tenant, company_id, bank_account_id, &prepared.lines).await
}

async fn check_cutover(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    account: i64,
    lines: &[Line],
) -> Result<(), ReconciliationError> {
    if let Some(row) = client.query_opt("SELECT to_char(conversion_date,'YYYY-MM-DD') FROM finance_reconciliation_opening_bridges WHERE tenant_id=$1 AND company_id=$2 AND bank_account_id=$3 ORDER BY id DESC LIMIT 1", &[&tenant,&company_id,&account]).await.map_err(bilibi_database::Error::from)? {
        let date: Date = row.get::<_,String>(0).parse().map_err(|_| bilibi_database::Error::Internal)?;
        if lines.iter().any(|line| line.date < date) {
            return Err(ReconciliationError::Invalid(vec![crate::FieldError::new("file", format!("statement lines must be on or after the opening conversion date {date}"))]));
        }
    }
    Ok(())
}

fn hash(content: &[u8]) -> String {
    hex(&Sha256::digest(content))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn fresh_file(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    sha256: &str,
) -> Result<(), ReconciliationError> {
    match client
        .query_opt(
            "SELECT statement_number FROM finance_reconciliation_statements \
             WHERE tenant_id=$1 AND company_id=$2 AND file_sha256=$3",
            &[&tenant, &company_id, &sha256],
        )
        .await
        .map_err(bilibi_database::Error::from)?
    {
        None => Ok(()),
        Some(row) => Err(ReconciliationError::DuplicateFile {
            statement_number: row.get(0),
        }),
    }
}

/// A line read from the file before it is numbered and fingerprinted.
struct Read {
    row: usize,
    date: Date,
    description: String,
    reference: Option<String>,
    amount: Money,
    balance: Option<Money>,
}

fn read(
    content: &[u8],
    mapping: &ImportMapping,
    units: MinorUnits,
) -> Result<(Vec<Read>, usize), ReconciliationError> {
    let rows = csv::parse(content, &mapping.delimiter).map_err(|error| match error {
        CsvError::TooLarge => ReconciliationError::FileTooLarge,
        CsvError::NotUtf8 => ReconciliationError::NotUtf8,
        CsvError::StrayQuote(row) => ReconciliationError::StrayQuote(row),
        CsvError::UnterminatedQuote(row) => ReconciliationError::UnterminatedQuote(row),
    })?;
    let header_rows = usize::from(mapping.header_rows);
    let (header, data) = rows.split_at(header_rows.min(rows.len()));
    let headers: &[String] = header.last().map_or(&[], Vec::as_slice);
    let missing = mapping.missing_headers(headers);
    if !missing.is_empty() {
        return Err(ReconciliationError::MissingHeaders(missing));
    }
    if data.len() > MAX_LINES {
        return Err(ReconciliationError::TooManyLines(MAX_LINES));
    }
    let mut lines = Vec::new();
    let mut errors = Vec::new();
    let mut skipped = 0;
    for (index, row) in data.iter().enumerate() {
        match parse_row(row, header_rows + index + 1, headers, mapping, units) {
            Ok(Some(line)) => lines.push(line),
            Ok(None) => skipped += 1,
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        Err(ReconciliationError::InvalidLines(errors))
    } else if lines.is_empty() {
        Err(ReconciliationError::NoLines)
    } else {
        Ok((lines, skipped))
    }
}

/// `Ok(None)` for a row whose amount is zero.
fn parse_row(
    row: &[String],
    number: usize,
    headers: &[String],
    mapping: &ImportMapping,
    units: MinorUnits,
) -> Result<Option<Read>, LineError> {
    let fail = |message: String| LineError {
        row: number,
        message,
    };
    let cell = |column: Option<&crate::Column>| ImportMapping::cell(row, headers, column);
    let date_text = cell(mapping.date.as_ref());
    let date =
        mapping::parse_date(date_text.unwrap_or(""), &mapping.date_format).ok_or_else(|| {
            fail(format!(
                "date {} is not {}",
                inspect(date_text),
                mapping.date_format
            ))
        })?;
    let description = cell(mapping.description.as_ref()).unwrap_or("").trim();
    if description.is_empty() {
        return Err(fail("description is blank".into()));
    }
    let description: String = description.chars().take(DESCRIPTION_LIMIT).collect();
    let reference = cell(mapping.reference.as_ref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(REFERENCE_LIMIT).collect::<String>());
    let amount = amount(row, headers, mapping).map_err(fail)?;
    let amount = Money::new(amount, units).map_err(|error| {
        fail(match error {
            MoneyError::TooLarge => "amount is too large".into(),
            _ => "amount has more decimal places than the book's currency".into(),
        })
    })?;
    let balance = match &mapping.balance {
        None => None,
        Some(column) => {
            let text = cell(Some(column));
            let balance = match mapping::parse_amount(text, &mapping.decimal_separator) {
                Ok(Some(balance)) => balance,
                Ok(None) => return Err(fail("running balance is blank".into())),
                Err(()) => return Err(fail(format!("balance {} is not a number", inspect(text)))),
            };
            Some(Money::new(balance, units).map_err(|error| {
                fail(match error {
                    MoneyError::TooLarge => "running balance is too large".into(),
                    _ => "running balance has more decimal places than the book's currency".into(),
                })
            })?)
        }
    };
    if amount.is_zero() {
        return Ok(None);
    }
    Ok(Some(Read {
        row: number,
        date,
        description,
        reference,
        amount,
        balance,
    }))
}

/// The line's signed amount, from one signed column or from debit (money out)
/// and credit (money in) columns.
fn amount(row: &[String], headers: &[String], mapping: &ImportMapping) -> Result<Decimal, String> {
    let cell = |column: Option<&crate::Column>| ImportMapping::cell(row, headers, column);
    let separator = mapping.decimal_separator.as_str();
    if mapping.amount.is_some() {
        let text = cell(mapping.amount.as_ref());
        return match mapping::parse_amount(text, separator) {
            Ok(None) => Err("amount is blank".into()),
            Ok(Some(amount)) if mapping.amount_sign == "withdrawals_positive" => Ok(-amount),
            Ok(Some(amount)) => Ok(amount),
            Err(()) => Err(format!("amount {} is not a number", inspect(text))),
        };
    }
    let side = |column: Option<&crate::Column>, name: &str| {
        let text = cell(column);
        match mapping::parse_amount(text, separator) {
            Ok(None) => Ok(Decimal::ZERO),
            Ok(Some(amount)) => Ok(amount),
            Err(()) => Err(format!("{name} {} is not a number", inspect(text))),
        }
    };
    let debit = side(mapping.debit.as_ref(), "debit")?;
    let credit = side(mapping.credit.as_ref(), "credit")?;
    // The column gives the direction; a bank that brackets or signs the figure
    // in it is saying the same thing twice.
    if !debit.is_zero() && !credit.is_zero() {
        Err("both debit and credit hold an amount".into())
    } else {
        Ok(credit.abs() - debit.abs())
    }
}

/// `inspect/1` of a cell: `nil` or the quoted text.
fn inspect(text: Option<&str>) -> String {
    text.map_or_else(|| "nil".to_owned(), |text| format!("{text:?}"))
}

/// With a running balance each line's balance is the previous line's plus its
/// amount, read in whichever direction the bank wrote the file; without one
/// the file is read oldest first.
fn ordered(
    mut lines: Vec<Read>,
    mapping: &ImportMapping,
) -> Result<Vec<Read>, ReconciliationError> {
    if mapping.balance.is_none() {
        if lines.first().map(|line| line.date) > lines.last().map(|line| line.date) {
            lines.reverse();
        }
        return Ok(lines);
    }
    let Some(row) = chain_break(&lines) else {
        return Ok(lines);
    };
    lines.reverse();
    if chain_break(&lines).is_none() {
        Ok(lines)
    } else {
        Err(ReconciliationError::RunningBalanceMismatch(row))
    }
}

/// The row of the first line whose balance is not the previous line's plus its
/// own amount.
fn chain_break(lines: &[Read]) -> Option<usize> {
    lines
        .windows(2)
        .find(|pair| pair[1].balance != pair[0].balance.map(|balance| balance + pair[1].amount))
        .map(|pair| pair[1].row)
}

/// Numbers the lines and fingerprints each: a hash of its date, description,
/// reference, amount, running balance and its occurrence among identical
/// lines of the same file. The same movement twice in one file is two lines;
/// the same file, or an overlapping export, brings the same fingerprints again
/// and is refused. Amounts are written without trailing zeros, so `12.5` and
/// `12.50` are the same line.
fn fingerprinted(lines: Vec<Read>) -> Vec<Line> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let identity = [
                line.date.to_string(),
                line.description.clone(),
                line.reference.clone().unwrap_or_default(),
                line.amount.amount().to_string(),
                line.balance
                    .map_or_else(String::new, |balance| balance.amount().to_string()),
            ]
            .join("\n");
            let occurrence = seen.entry(identity.clone()).or_insert(0);
            *occurrence += 1;
            Line {
                row: line.row,
                line_number: index as i32 + 1,
                date: line.date,
                description: line.description,
                reference: line.reference,
                amount: line.amount,
                balance: line.balance,
                fingerprint: hash(format!("{identity}\n{occurrence}").as_bytes()),
            }
        })
        .collect()
}

async fn fresh_lines(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    lines: &[Line],
) -> Result<(), ReconciliationError> {
    let fingerprints: Vec<&str> = lines.iter().map(|line| line.fingerprint.as_str()).collect();
    let held: HashMap<String, (i64, i32)> = client
        .query(
            "SELECT l.fingerprint,s.statement_number,l.line_number \
             FROM finance_reconciliation_statement_lines l \
             JOIN finance_reconciliation_statements s ON s.id=l.statement_id AND s.company_id=l.company_id \
             WHERE l.tenant_id=$1 AND l.company_id=$2 AND l.bank_account_id=$3 AND l.fingerprint = ANY($4::text[])",
            &[&tenant, &company_id, &bank_account_id, &fingerprints],
        )
        .await
        .map_err(bilibi_database::Error::from)?
        .iter()
        .map(|row| (row.get(0), (row.get(1), row.get(2))))
        .collect();
    let duplicates: Vec<DuplicateLine> = lines
        .iter()
        .filter_map(|line| {
            held.get(&line.fingerprint)
                .map(|&(statement_number, statement_line_number)| DuplicateLine {
                    row: line.row,
                    line_number: line.line_number,
                    statement_number,
                    statement_line_number,
                })
        })
        .collect();
    if duplicates.is_empty() {
        Ok(())
    } else {
        Err(ReconciliationError::DuplicateLines(duplicates))
    }
}

/// Records the statement and its lines in the caller's transaction with the
/// mapping it was read through, and returns the statement's id.
pub(crate) async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    account: &BankAccount,
    prepared: &Prepared,
) -> DbResult<i64> {
    let first = &prepared.lines[0];
    let last = &prepared.lines[prepared.lines.len() - 1];
    // With a running balance the statement opens at the first line's balance
    // before its amount and closes at the last line's.
    let (opening, closing) = match (first.balance, last.balance) {
        (Some(first_balance), Some(last_balance)) => (
            Some((first_balance - first.amount).amount()),
            Some(last_balance.amount()),
        ),
        _ => (None, None),
    };
    let statement = Statement {
        bank_account_id: account.id,
        file_name: prepared.file.file_name.clone(),
        file_sha256: prepared.sha256.clone(),
        import_mapping: prepared.mapping.to_json(),
        line_count: prepared.lines.len() as i32,
        start_date: prepared.start_date.to_string(),
        end_date: prepared.end_date.to_string(),
        opening_balance: opening,
        closing_balance: closing,
    };
    let columns = Columns {
        numbers: prepared.lines.iter().map(|line| line.line_number).collect(),
        dates: prepared
            .lines
            .iter()
            .map(|line| line.date.to_string())
            .collect(),
        descriptions: prepared
            .lines
            .iter()
            .map(|line| line.description.clone())
            .collect(),
        references: prepared
            .lines
            .iter()
            .map(|line| line.reference.clone())
            .collect(),
        amounts: prepared
            .lines
            .iter()
            .map(|line| line.amount.amount())
            .collect(),
        balances: prepared
            .lines
            .iter()
            .map(|line| line.balance.map(Money::amount))
            .collect(),
        fingerprints: prepared
            .lines
            .iter()
            .map(|line| line.fingerprint.clone())
            .collect(),
    };
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    "INSERT INTO finance_reconciliation_statements(tenant_id,company_id,bank_account_id,statement_number,file_name,file_sha256,import_mapping,line_count,start_date,end_date,opening_balance,closing_balance,imported_by_user_id,imported_by_impersonator_id,imported_at) \
                     VALUES($1,$2,$3,NULL,$4,$5,$6,$7,$8::text::date,$9::text::date,$10,$11,$12,$13,NULL) RETURNING id,statement_number",
                    &[
                        &tenant,
                        &company_id,
                        &statement.bank_account_id,
                        &statement.file_name,
                        &statement.file_sha256,
                        &statement.import_mapping,
                        &statement.line_count,
                        &statement.start_date,
                        &statement.end_date,
                        &statement.opening_balance,
                        &statement.closing_balance,
                        &actor,
                        &impersonator,
                    ],
                )
                .await?;
            let statement_id: i64 = row.get(0);
            let statement_number: i64 = row.get(1);
            let mut changes = vec![Change {
                kind: "finance.reconciliation.statement".into(),
                id: statement_id,
                old: Value::Null,
                new: json!({
                    "company_id": company_id,
                    "bank_account_id": statement.bank_account_id,
                    "statement_number": statement_number,
                    "file_name": statement.file_name,
                    "file_sha256": statement.file_sha256,
                    "import_mapping": statement.import_mapping,
                    "line_count": statement.line_count,
                    "start_date": statement.start_date,
                    "end_date": statement.end_date,
                    "opening_balance": statement.opening_balance.map(|amount| amount.to_string()),
                    "closing_balance": statement.closing_balance.map(|amount| amount.to_string()),
                    "imported_by_user_id": actor,
                    "imported_by_impersonator_id": impersonator,
                }),
            }];
            let rows = sql
                .query(
                    "INSERT INTO finance_reconciliation_statement_lines(tenant_id,company_id,statement_id,bank_account_id,line_number,line_date,description,reference,amount,running_balance,fingerprint) \
                     SELECT $1,$2,$3,$4,t.number,t.line_date::date,t.description,t.reference,t.amount,t.balance,t.fingerprint \
                     FROM UNNEST($5::int4[],$6::text[],$7::text[],$8::text[],$9::numeric[],$10::numeric[],$11::text[]) \
                       AS t(number,line_date,description,reference,amount,balance,fingerprint) \
                     ORDER BY t.number RETURNING id,line_number",
                    &[
                        &tenant,
                        &company_id,
                        &statement_id,
                        &statement.bank_account_id,
                        &columns.numbers,
                        &columns.dates,
                        &columns.descriptions,
                        &columns.references,
                        &columns.amounts,
                        &columns.balances,
                        &columns.fingerprints,
                    ],
                )
                .await?;
            for row in &rows {
                let id: i64 = row.get(0);
                let number: i32 = row.get(1);
                let at = usize::try_from(number - 1).map_err(|_| bilibi_database::Error::Internal)?;
                changes.push(Change {
                    kind: "finance.reconciliation.statement_line".into(),
                    id,
                    old: Value::Null,
                    new: json!({
                        "company_id": company_id,
                        "statement_id": statement_id,
                        "bank_account_id": statement.bank_account_id,
                        "line_number": number,
                        "line_date": columns.dates[at],
                        "description": columns.descriptions[at],
                        "reference": columns.references[at],
                        "amount": columns.amounts[at].to_string(),
                        "running_balance": columns.balances[at].map(|amount| amount.to_string()),
                        "fingerprint": columns.fingerprints[at],
                    }),
                });
            }
            Ok((statement_id, changes))
        })
    })
    .await
}

/// The statement's own columns, owned so the audited write can move them.
struct Statement {
    bank_account_id: i64,
    file_name: String,
    file_sha256: String,
    import_mapping: Value,
    line_count: i32,
    start_date: String,
    end_date: String,
    opening_balance: Option<Decimal>,
    closing_balance: Option<Decimal>,
}

/// The lines as one array per column, for a single insert.
struct Columns {
    numbers: Vec<i32>,
    dates: Vec<String>,
    descriptions: Vec<String>,
    references: Vec<Option<String>>,
    amounts: Vec<Decimal>,
    balances: Vec<Option<Decimal>>,
    fingerprints: Vec<String>,
}

/// How a statement file would import ([`crate::Reconciliation::preview_statement`]).
#[derive(Debug)]
pub struct StatementPreview {
    /// The mapping the preview read the file through, every key present.
    pub mapping: Value,
    /// Whether that mapping is a guess from the file, not one a person chose.
    pub guessed: bool,
    /// The file's columns by the header row's text, or "Column 1", "Column
    /// 2", ... when the mapping says the file has no header row.
    pub columns: Vec<String>,
    /// The first rows below the header, as the file has them.
    pub rows: Vec<Vec<String>>,
    /// The first lines the import would record, oldest first.
    pub lines: Vec<PreviewLine>,
    /// How many lines the import would record, and how many zero-amount rows
    /// it would skip; both 0 with a refusal.
    pub line_count: usize,
    pub skipped_count: usize,
    /// Why the mapping does not read the file, as the import would refuse it.
    pub refusal: Option<ReconciliationError>,
}

/// One line as the import would record it: money in positive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewLine {
    pub date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub amount: Money,
}

/// The rows and lines shown before an import.
const PREVIEW_ROWS: usize = 5;

/// Reads `content` through `mapping` without storing or checking anything
/// against the bank account's statements. The rows show even when the
/// mapping is refused, read with its separator and header rows where those
/// are valid.
pub(crate) fn preview(
    content: &[u8],
    mapping: Value,
    guessed: bool,
    units: MinorUnits,
) -> StatementPreview {
    let parsed = ImportMapping::parse(&mapping);
    let (delimiter, header_rows) = match &parsed {
        Ok(parsed) => (parsed.delimiter.clone(), usize::from(parsed.header_rows)),
        Err(_) => (
            mapping["delimiter"]
                .as_str()
                .filter(|d| mapping::DELIMITERS.contains(d))
                .unwrap_or(",")
                .to_owned(),
            mapping["header_rows"]
                .as_u64()
                .filter(|rows| *rows <= 20)
                .map_or(1, |rows| rows as usize),
        ),
    };
    let rows = csv::parse(content, &delimiter).unwrap_or_default();
    let (header, data) = rows.split_at(header_rows.min(rows.len()));
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let columns = (0..width)
        .map(|index| match header.last().and_then(|h| h.get(index)) {
            Some(text) if header_rows > 0 => text.trim().to_owned(),
            _ => format!("Column {}", index + 1),
        })
        .collect();
    let mut preview = StatementPreview {
        mapping,
        guessed,
        columns,
        rows: data.iter().take(PREVIEW_ROWS).cloned().collect(),
        lines: Vec::new(),
        line_count: 0,
        skipped_count: 0,
        refusal: None,
    };
    let read = parsed
        .map_err(|message| {
            ReconciliationError::Invalid(vec![crate::FieldError::new("import_mapping", message)])
        })
        .and_then(|parsed| {
            let (lines, skipped) = read(content, &parsed, units)?;
            Ok((ordered(lines, &parsed)?, skipped))
        });
    match read {
        Ok((lines, skipped)) => {
            preview.line_count = lines.len();
            preview.skipped_count = skipped;
            preview.lines = lines
                .into_iter()
                .take(PREVIEW_ROWS)
                .map(|line| PreviewLine {
                    date: line.date,
                    description: line.description,
                    reference: line.reference,
                    amount: line.amount,
                })
                .collect();
        }
        Err(refusal) => preview.refusal = Some(refusal),
    }
    preview
}
