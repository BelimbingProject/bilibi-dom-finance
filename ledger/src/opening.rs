//! Accounting system conversion's first slice, using Bilimbi's journal approval
//! controls. Source rows remain evidence; they never issue invoices or bills.
use crate::{
    accounts, books, entries, journals, FieldError, Journal, Ledger, LedgerError, Money, NewEntry,
    NewLine, Result, SUBMIT_JOURNALS,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, MutationTransaction};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// The most recent opening submission and its current posting state. This
/// measures opening balances only, never completion of an accounting conversion.
#[derive(Clone, Debug, serde::Serialize)]
pub struct OpeningProgress {
    pub journal_id: i64,
    pub journal_number: i64,
    #[serde(serialize_with = "crate::as_text")]
    pub conversion_date: Date,
    pub status: crate::JournalStatus,
    pub entry_id: Option<i64>,
    pub reversed_by_entry_id: Option<i64>,
    /// The modules that imported an opening register against the posted
    /// entry (customers, suppliers, assets, bank items), with how many of its
    /// accounts each recognised; empty until one does.
    pub imported: Vec<OpeningImport>,
}

/// One module's opening register recognised against the posted opening entry.
#[derive(Clone, Debug, serde::Serialize)]
pub struct OpeningImport {
    /// The importing module's descriptor id (`finance/receivables`).
    pub owner_module: String,
    pub accounts: i64,
}

pub(crate) async fn progress(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
) -> Result<Option<OpeningProgress>> {
    let Some(row) = client.query_opt(
        "SELECT journal_id FROM finance_ledger_opening_balances WHERE tenant_id=$1 AND company_id=$2 ORDER BY id DESC LIMIT 1",
        &[&scope.tenant(), &company],
    ).await? else {
        return Ok(None);
    };
    let journal = journals::find(client, scope.tenant(), company, row.get(0))
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    let entry_id = journal.decision.as_ref().and_then(|d| d.entry_id);
    let reversed_by_entry_id = match entry_id {
        Some(id) => {
            entries::find(client, scope.tenant(), company, id)
                .await?
                .ok_or(bilibi_database::Error::Internal)?
                .reversed_by_entry_id
        }
        None => None,
    };
    let imported = match entry_id {
        Some(entry) => client
            .query(
                "SELECT owner_module,count(DISTINCT account_id) FROM finance_ledger_opening_recognitions WHERE tenant_id=$1 AND company_id=$2 AND entry_id=$3 GROUP BY owner_module ORDER BY owner_module",
                &[&scope.tenant(), &company, &entry],
            )
            .await?
            .iter()
            .map(|row| OpeningImport {
                owner_module: row.get(0),
                accounts: row.get(1),
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(Some(OpeningProgress {
        journal_id: journal.id,
        journal_number: journal.journal_number,
        conversion_date: journal.entry_date,
        status: journal.status,
        entry_id,
        reversed_by_entry_id,
        imported,
    }))
}

/// Source closing balances posted at the start of the conversion date. The
/// accountant retains responsibility for the cut-off and reporting bridge.
#[derive(Clone)]
pub struct OpeningBalances {
    pub conversion_date: Date,
    pub currency_code: String,
    /// Stable export/batch identifier, permanent within this company.
    pub source_reference: String,
    /// UTF-8 CSV: source_id,account_code,debit,credit. At most 500 source rows.
    pub csv: String,
    /// The waiting opening journal this submission corrects: it is withdrawn and
    /// this one takes its place as the book's opening, in one transaction.
    pub replaces_journal_id: Option<i64>,
}

fn refused(field: &'static str, message: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(vec![FieldError::new(field, message)])
}

pub(crate) struct Prepared {
    pub entry: NewEntry,
    pub existing: Option<Journal>,
    pub review: Value,
}

/// Called only after authorization; reads on the plan's own connection.
pub(crate) async fn prepare(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    source: &OpeningBalances,
) -> Result<Prepared> {
    let tenant = scope.tenant();
    let reference = source.source_reference.trim();
    if reference.is_empty() || reference.chars().count() > 100 || reference.contains('\0') {
        return Err(refused(
            "source_reference",
            "must contain 1 to 100 characters without null characters",
        ));
    }
    if source.csv.contains('\0') {
        return Err(refused("csv", "must not contain null characters"));
    }
    let book = books::find(client, tenant, company)
        .await?
        .ok_or(LedgerError::BookNotOpen)?;
    if source.currency_code != book.currency_code {
        return Err(refused(
            "currency_code",
            "must match the book's currency; conversion does not exchange currencies",
        ));
    }
    let existing_row = client.query_opt(
        "SELECT source_csv,journal_id FROM finance_ledger_opening_balances WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3",
        &[&tenant, &company, &reference]).await?;
    let rows = crate::opening_csv::parse(source.csv.as_bytes(), ",").map_err(|error| {
        refused(
            "csv",
            match error {
                crate::opening_csv::CsvError::TooLarge => "must fit within 128 KiB".into(),
                crate::opening_csv::CsvError::NotUtf8 => "must be UTF-8".into(),
                crate::opening_csv::CsvError::StrayQuote(row) => {
                    format!("row {row} has a misplaced quote")
                }
                crate::opening_csv::CsvError::UnterminatedQuote(row) => {
                    format!("row {row} has an unclosed quote")
                }
            },
        )
    })?;
    if rows
        .first()
        .map(|r| r.iter().map(String::as_str).collect::<Vec<_>>())
        != Some(vec!["source_id", "account_code", "debit", "credit"])
    {
        return Err(refused(
            "csv",
            "requires the header source_id,account_code,debit,credit",
        ));
    }
    if !(3..=501).contains(&rows.len()) {
        return Err(refused("csv", "must contain 2 to 500 balance rows"));
    }
    let chart = accounts::list(client, tenant, company).await?;
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    let mut mapped = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(1) {
        let row_error = |message: String| refused("csv", format!("row {}: {message}", index + 1));
        if row.len() != 4 {
            return Err(row_error("must contain four columns".into()));
        }
        let source_id = row[0].trim();
        if source_id.is_empty() || source_id.chars().count() > 200 || !seen.insert(source_id) {
            return Err(row_error(
                "source_id must be unique and contain 1 to 200 characters".into(),
            ));
        }
        let account = chart
            .iter()
            .find(|a| a.code == row[1].trim() && (a.active || existing_row.is_some()))
            .ok_or_else(|| {
                row_error(format!(
                    "account code {:?} does not map to an active account",
                    row[1]
                ))
            })?;
        let amount = |column: usize| -> Result<Money> {
            if row[column].trim().is_empty() {
                return Ok(Money::ZERO);
            }
            Money::parse(row[column].trim(), book.minor_units).map_err(|e| row_error(e.to_string()))
        };
        let debit = amount(2)?;
        let credit = amount(3)?;
        lines.push(NewLine {
            account_id: account.id,
            debit,
            credit,
            description: Some(format!("Source {source_id}")),
        });
        mapped.push(json!({"source_id":source_id,"account_id":account.id,
            "account_code":account.code,"account_name":account.name,
            "debit":debit,"credit":credit}));
    }
    let entry = NewEntry {
        entry_date: source.conversion_date,
        description: "Opening balances — accounting system conversion".into(),
        reference: Some(reference.into()),
        lines,
    };
    let mut replaced_number = None;
    let existing = if let Some(row) = existing_row {
        let journal = journals::find(client, tenant, company, row.get(1))
            .await?
            .ok_or(LedgerError::JournalNotFound)?;
        if row.get::<_, String>(0) != source.csv
            || journal.entry_date != source.conversion_date
            || journal.replaces_journal_id != source.replaces_journal_id
        {
            return Err(refused(
                "source_reference",
                "already identifies a different opening journal; use the recorded journal's correction controls",
            ));
        }
        replaced_number = journal.replaces_journal_number;
        Some(journal)
    } else {
        // A new source reference must never silently double a live opening. A
        // rejected or withdrawn opening is dead, and a correction is its own
        // opening; a waiting one is corrected by naming it in `replaces_journal_id`.
        if let Some(replaced) = source.replaces_journal_id {
            let waiting = crate::withdrawable_journal(client, scope, company, replaced).await?;
            replaced_number = Some(waiting.journal_number);
            if !is_opening(client, tenant, company, replaced).await? {
                return Err(refused("replaces_journal_id", "is not an opening balances journal"));
            }
        } else if client.query_opt(
            "SELECT o.id FROM finance_ledger_opening_balances o WHERE o.tenant_id=$1 AND o.company_id=$2 AND NOT EXISTS (SELECT 1 FROM finance_ledger_journal_events e WHERE e.journal_id=o.journal_id AND e.company_id=o.company_id AND e.event IN ('rejected','withdrawn')) LIMIT 1",
            &[&tenant, &company]).await?.is_some() {
            return Err(refused("source_reference", "the book already has an opening journal; approve it, withdraw or reject it and submit again, or correct it by submitting with the journal it replaces"));
        }
        if client.query_opt("SELECT id FROM finance_ledger_entries WHERE tenant_id=$1 AND company_id=$2 LIMIT 1", &[&tenant, &company]).await?.is_some() {
            return Err(refused("conversion_date", "opening balances require a book without posted entries"));
        }
        entries::validate(
            client,
            tenant,
            company,
            book.minor_units,
            &entry,
            None,
            entries::Inactive::Listed(&[]),
        )
        .await?
        .map_err(|errors| {
            LedgerError::Invalid(
                errors
                    .into_iter()
                    .map(|error| {
                        if let Some(line) = error.line {
                            FieldError::new("csv", format!("row {}: {}", line + 1, error.message))
                        } else if error.field == "lines" {
                            FieldError::new("csv", error.message)
                        } else if error.field == "entry_date" {
                            FieldError::new("conversion_date", error.message)
                        } else {
                            error
                        }
                    })
                    .collect(),
            )
        })?;
        None
    };
    let debit: Money = entry.lines.iter().map(|l| l.debit).sum();
    let credit: Money = entry.lines.iter().map(|l| l.credit).sum();
    let review = json!({"conversion_date":source.conversion_date.to_string(),
        "currency_code":book.currency_code,"minor_units":book.minor_units,"source_reference":reference,"rows":mapped,
        "debit":debit,"credit":credit,
        "existing_journal":existing,"replaces_journal_id":source.replaces_journal_id,"replaces_journal_number":replaced_number,"posts":false,
        "excluded":"This journal creates ledger balances only. After approval, import opening invoices, bills and the asset register through their own reconciled opening operations. Bank reconciliation items, source-period tax detail and historical transactions remain outside these imports. Income and expense balances appear on the conversion date; review the YTD reporting bridge with the accountant."});
    Ok(Prepared {
        entry,
        existing,
        review,
    })
}

impl Ledger {
    /// Read opening-balance progress (`finance.ledger.view`), or `None`
    /// before the first submission. Ordinary manual journals do not count.
    pub async fn opening_progress(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<Option<OpeningProgress>> {
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        crate::authorize(&**client, scope, company, crate::VIEW).await?;
        progress(&**client, scope, company).await
    }

    /// Submit one opening journal, or return the identical recorded submission.
    /// Approval is a separate call, with the ordinary separation of duties.
    pub async fn submit_opening_balances(
        &self,
        scope: &Scope,
        company: i64,
        source: &OpeningBalances,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                &[SUBMIT_JOURNALS],
                "finance.ledger.opening_balances.submit",
            )
            .await?;
        let journal = Self::submit_opening_balances_on(&mut tx, scope, company, source).await?;
        tx.commit().await?;
        Ok(journal)
    }

    pub(super) async fn submit_opening_balances_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        source: &OpeningBalances,
    ) -> Result<Journal> {
        crate::join(tx, scope, company, SUBMIT_JOURNALS).await?;
        let prepared = prepare(tx.sql(), scope, company, source).await?;
        if let Some(journal) = prepared.existing {
            return Ok(journal);
        }
        let journal = Self::submit_journal_on(tx, scope, company, &prepared.entry).await?;
        let tenant = scope.tenant();
        let journal_id = journal.id;
        let reference = source.source_reference.trim().to_owned();
        let csv = source.csv.clone();
        tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one("INSERT INTO finance_ledger_opening_balances(tenant_id,company_id,source_reference,source_csv,journal_id) VALUES($1,$2,$3,$4,$5) RETURNING id", &[&tenant,&company,&reference,&csv,&journal_id]).await?;
            Ok(((), vec![Change {kind:"finance.ledger.opening_balances".into(),id:row.get(0),old:Value::Null,
                new:json!({"source_reference":reference,"source_csv":csv,"journal_id":journal_id})}]))
        })).await?;
        if let Some(replaced) = source.replaces_journal_id {
            let reason = format!(
                "Corrected by opening balances source {}",
                source.source_reference.trim()
            );
            Self::record_withdrawal(tx, scope, company, replaced, Some(reason)).await?;
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let change = journals::link(sql, tenant, company, journal_id, replaced).await?;
                    Ok(((), vec![change]))
                })
            })
            .await?;
        }
        Ok(journals::find(tx.sql(), tenant, company, journal_id)
            .await?
            .ok_or(bilibi_database::Error::Internal)?)
    }
}

/// Whether the journal is one a source reference and CSV were retained for.
pub(crate) async fn is_opening(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    journal_id: i64,
) -> Result<bool> {
    Ok(client.query_opt(
        "SELECT id FROM finance_ledger_opening_balances WHERE tenant_id=$1 AND company_id=$2 AND journal_id=$3",
        &[&tenant, &company, &journal_id]).await?.is_some())
}

/// An opening journal is corrected only by submitting opening balances again
/// with `replaces_journal_id`, so its replacement keeps the opening's source
/// reference and CSV; a manual replacement would leave the book with no
/// opening record and let a second opening be submitted beside it.
pub(crate) async fn refuse_manual_replacement(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    journal_id: i64,
) -> Result<()> {
    if is_opening(client, scope.tenant(), company, journal_id).await? {
        return Err(refused(
            "journal_id",
            format!(
                "is an opening balances journal; correct it from Journals (/finance/ledger/journals?correct={journal_id}) or by submitting opening balances again with replaces_journal_id"
            ),
        ));
    }
    Ok(())
}
