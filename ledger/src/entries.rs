//! The `finance_ledger_entries` and `finance_ledger_entry_lines` rows (Bilimbi
//! `Ledger.Entry` and `Ledger.Line` with the facade's `entry_changeset`,
//! account and owner rules). A posted entry never changes, so there is only
//! validation, insertion and reading here; the facade authorizes and holds
//! the company lock, and these run inside it.
use crate::{
    periods, Entry, EntryLine, EntryOwner, EntryQuery, FieldError, LedgerError, Money, NewEntry,
    NewLine,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::Result;
use jiff::{civil::Date, Timestamp};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::{GenericClient, Row};

const DESCRIPTION_LIMIT: usize = 500;
const REFERENCE_LIMIT: usize = 100;
const OWNER_LIMIT: usize = 100;
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;

const ENTRY_COLUMNS: &str = "id,entry_number,entry_date::text AS entry_date,description,reference,\
     currency_code,reverses_entry_id,owner_module,owner_record,posted_by_user_id,\
     posted_by_impersonator_id,to_char(posted_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS posted_at";
const LINE_COLUMNS: &str = "entry_id,line_number,account_id,debit,credit,description";

/// Latest dated posting, including backdated entries but not year-end closing
/// entries and their reversals, for the report's opening month.
pub(crate) async fn latest_date(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
) -> Result<Option<String>> {
    Ok(client
        .query_opt(
            &format!(
                "SELECT max(e.entry_date)::text FROM finance_ledger_entries e \
                 WHERE e.tenant_id=$1 AND e.company_id=$2 AND ({not_closing})",
                not_closing = periods::NOT_CLOSING
            ),
            &[&tenant, &company],
        )
        .await?
        .and_then(|row| row.get(0)))
}

pub(crate) async fn latest_account_entry_number(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    account: i64,
    through: Date,
) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT COALESCE(max(e.entry_number),0)::bigint FROM finance_ledger_entries e \
             WHERE e.tenant_id=$1 AND e.company_id=$2 AND e.entry_date <= $4::text::date \
               AND EXISTS (SELECT 1 FROM finance_ledger_entry_lines l \
                 WHERE l.tenant_id=$1 AND l.company_id=$2 AND l.entry_id=e.id AND l.account_id=$3)",
            &[&tenant, &company, &account, &through.to_string()],
        )
        .await?
        .get(0))
}

/// A new entry's values after trimming and checking, ready to insert.
#[derive(serde::Serialize)]
pub(crate) struct ValidEntry {
    #[serde(serialize_with = "crate::as_text")]
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
    owner: Option<EntryOwner>,
    reverses_entry_id: Option<i64>,
    pub lines: Vec<ValidLine>,
}

/// Which inactive accounts a new entry may name.
#[derive(Clone, Copy)]
pub(crate) enum Inactive<'a> {
    /// Only these (the settlement exception of [`crate::PostOptions`]).
    Listed(&'a [i64]),
    /// Any: a reversal or a year-end closing entry.
    Any,
}

impl ValidEntry {
    /// Marks the entry as the reversal of `original`, carrying its owner.
    pub(crate) fn reversing(mut self, original: &Entry) -> Self {
        self.reverses_entry_id = Some(original.id);
        self.owner = original.owner.clone();
        self
    }
}

/// The mirror image of `original`: each line's debit becomes a credit and
/// the other way round, with its description.
pub(crate) fn mirror(original: &Entry, entry_date: Date, description: String) -> NewEntry {
    NewEntry {
        entry_date,
        description,
        reference: None,
        lines: original
            .lines
            .iter()
            .map(|line| NewLine {
                account_id: line.account_id,
                debit: line.credit,
                credit: line.debit,
                description: line.description.clone(),
            })
            .collect(),
    }
}
#[derive(serde::Serialize)]
pub(crate) struct ValidLine {
    pub account_id: i64,
    pub debit: Money,
    pub credit: Money,
    pub description: Option<String>,
}

/// The shape `post_entry`'s `owner` option must have: a module's descriptor
/// id (`finance/receivables`) and a record of its own as a kind and a
/// positive integer (`payment:17`), each at most 100 characters.
pub(crate) fn owner_valid(owner: &EntryOwner) -> bool {
    let name = |part: &str| {
        part.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    let module = owner.module.split_once('/');
    let record = owner.record.split_once(':');
    owner.module.chars().count() <= OWNER_LIMIT
        && owner.record.chars().count() <= OWNER_LIMIT
        && module.is_some_and(|(container, module)| name(container) && name(module))
        && record.is_some_and(|(kind, id)| {
            name(kind)
                && !id.starts_with('0')
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
        })
}

/// `Entry.changeset/2`, `Line.changeset/3` and the facade's balance,
/// account and open-period rules, in one pass: every refusal comes back, not
/// only the first. The owner's record must also have no original entry yet.
pub(crate) async fn validate(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    units: crate::MinorUnits,
    new: &NewEntry,
    owner: Option<&EntryOwner>,
    inactive: Inactive<'_>,
) -> Result<std::result::Result<ValidEntry, Vec<FieldError>>> {
    let refusal = periods::date_refusal(client, tenant, company_id, new.entry_date).await?;
    validate_values(
        client, tenant, company_id, units, new, owner, inactive, refusal,
    )
    .await
}

/// A read-only projection of reopening this year's final period. No other
/// date or state may use this path; actual posting still checks the database.
pub(crate) async fn validate_reopening(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    units: crate::MinorUnits,
    new: &NewEntry,
    year: &crate::FiscalYear,
) -> Result<std::result::Result<ValidEntry, Vec<FieldError>>> {
    if year.status != crate::YearStatus::Closed
        || year.end_date != new.entry_date
        || !year
            .periods
            .last()
            .is_some_and(|p| p.status == crate::PeriodStatus::Closed)
    {
        return Err(bilibi_database::Error::Conflict);
    }
    validate_values(
        client,
        tenant,
        company_id,
        units,
        new,
        None,
        Inactive::Any,
        None,
    )
    .await
}

async fn validate_values(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    units: crate::MinorUnits,
    new: &NewEntry,
    owner: Option<&EntryOwner>,
    inactive: Inactive<'_>,
    date_refusal: Option<String>,
) -> Result<std::result::Result<ValidEntry, Vec<FieldError>>> {
    let mut errors = Vec::new();
    if let Some(message) = date_refusal {
        errors.push(FieldError::new("entry_date", message));
    }
    let description = new.description.trim().to_owned();
    if description.is_empty() {
        errors.push(FieldError::new("description", "can't be blank"));
    } else if description.chars().count() > DESCRIPTION_LIMIT {
        errors.push(FieldError::new(
            "description",
            format!("should be at most {DESCRIPTION_LIMIT} character(s)"),
        ));
    }
    let reference = new
        .reference
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
        .map(str::to_owned);
    if reference
        .as_ref()
        .is_some_and(|reference| reference.chars().count() > REFERENCE_LIMIT)
    {
        errors.push(FieldError::new(
            "reference",
            format!("should be at most {REFERENCE_LIMIT} character(s)"),
        ));
    }

    let mut lines = Vec::new();
    let mut line_errors = false;
    for (index, line) in new.lines.iter().enumerate() {
        let number = index + 1;
        let before = errors.len();
        let mut add = |field: &'static str, message: String| {
            errors.push(FieldError::new(field, message).on_line(number))
        };
        let (debit, credit) = (line.debit, line.credit);
        for (field, amount) in [("debit", debit), ("credit", credit)] {
            if amount.amount().is_sign_negative() && !amount.is_zero() {
                add(field, "must be greater than or equal to 0".into());
            } else if Money::new(amount.amount(), units).is_err() {
                add(field, format!("has more than {units} decimal places"));
            }
        }
        if debit.is_positive() == credit.is_positive() {
            add("debit", "a line has either a debit or a credit".into());
        }
        let line_description = line
            .description
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        if line_description
            .as_ref()
            .is_some_and(|text| text.chars().count() > DESCRIPTION_LIMIT)
        {
            add(
                "description",
                format!("should be at most {DESCRIPTION_LIMIT} character(s)"),
            );
        }
        line_errors |= errors.len() > before;
        lines.push(ValidLine {
            account_id: line.account_id,
            debit,
            credit,
            description: line_description,
        });
    }
    if lines.len() < 2 {
        errors.push(FieldError::new("lines", "needs at least two lines"));
    } else if !line_errors {
        let debits: Money = lines.iter().map(|line| line.debit).sum();
        let credits: Money = lines.iter().map(|line| line.credit).sum();
        if debits != credits {
            errors.push(FieldError::new(
                "lines",
                "debits and credits do not balance",
            ));
        }
    }

    // Every line names an account of this company, never an account group; a
    // new posting also needs it active, except the accounts the settlement
    // exception lists. The company lock serializes this with deactivation.
    let ids: Vec<i64> = lines
        .iter()
        .map(|line| line.account_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows = client
        .query(
            "SELECT id,active,is_group FROM finance_ledger_accounts WHERE tenant_id=$1 AND company_id=$2 AND id = ANY($3)",
            &[&tenant, &company_id, &ids],
        )
        .await?;
    let accounts: BTreeMap<i64, (bool, bool)> = rows
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect();
    for (index, line) in lines.iter().enumerate() {
        match accounts.get(&line.account_id) {
            None => errors.push(
                FieldError::new("account_id", "is not an account of this company")
                    .on_line(index + 1),
            ),
            Some((_, true)) => errors.push(
                FieldError::new("account_id", "is an account group and takes no postings")
                    .on_line(index + 1),
            ),
            Some((false, _)) if matches!(inactive, Inactive::Listed(ids) if !ids.contains(&line.account_id)) => {
                errors.push(FieldError::new("account_id", "is inactive").on_line(index + 1))
            }
            Some(_) => {}
        }
    }

    if let Some(owner) = owner {
        let taken = client
            .query_opt(
                "SELECT 1 FROM finance_ledger_entries WHERE company_id=$1 AND owner_module=$2 \
                 AND owner_record=$3 AND reverses_entry_id IS NULL",
                &[&company_id, &owner.module, &owner.record],
            )
            .await?;
        if taken.is_some() {
            errors.push(FieldError::new("owner_record", "already has an entry"));
        }
    }

    Ok(if errors.is_empty() {
        Ok(ValidEntry {
            entry_date: new.entry_date,
            description,
            reference,
            owner: owner.cloned(),
            reverses_entry_id: None,
            lines,
        })
    } else {
        Err(errors)
    })
}

/// A row written by [`insert`], for the audit capture.
pub(crate) struct Inserted {
    pub entry: Entry,
    pub entry_json: Value,
    pub lines: Vec<(i64, Value)>,
}

/// Inserts the entry and its lines; the database numbers and stamps the entry,
/// so what comes back is what PostgreSQL stored.
pub(crate) async fn insert(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    currency_code: &str,
    actor: i64,
    impersonator: Option<i64>,
    entry: &ValidEntry,
) -> Result<Inserted> {
    let (owner_module, owner_record) = match &entry.owner {
        Some(owner) => (Some(owner.module.as_str()), Some(owner.record.as_str())),
        None => (None, None),
    };
    let date = entry.entry_date.to_string();
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_entries(tenant_id,company_id,entry_number,entry_date,description,\
             reference,currency_code,owner_module,owner_record,posted_by_user_id,posted_by_impersonator_id,posted_at,\
             reverses_entry_id) \
             VALUES($1,$2,0,$3::text::date,$4,$5,$6,$7,$8,$9,$10,now() AT TIME ZONE 'UTC',$11) RETURNING id",
            &[
                &tenant,
                &company_id,
                &date,
                &entry.description,
                &entry.reference,
                &currency_code,
                &owner_module,
                &owner_record,
                &actor,
                &impersonator,
                &entry.reverses_entry_id,
            ],
        )
        .await?;
    let entry_id: i64 = row.get(0);
    let mut lines = Vec::new();
    for (index, line) in entry.lines.iter().enumerate() {
        let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let row = client
            .query_one(
                "INSERT INTO finance_ledger_entry_lines(tenant_id,company_id,entry_id,line_number,account_id,\
                 debit,credit,description) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
                &[
                    &tenant,
                    &company_id,
                    &entry_id,
                    &number,
                    &line.account_id,
                    &line.debit.amount(),
                    &line.credit.amount(),
                    &line.description,
                ],
            )
            .await?;
        lines.push((
            row.get::<_, i64>(0),
            json!({
                "entry_id": entry_id, "line_number": number, "account_id": line.account_id,
                "debit": line.debit.amount().to_string(), "credit": line.credit.amount().to_string(),
                "description": line.description,
            }),
        ));
    }
    let stored = find(client, tenant, company_id, entry_id)
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    let entry_json = to_json(&stored);
    Ok(Inserted {
        entry: stored,
        entry_json,
        lines,
    })
}

/// One entry of the company with its lines.
pub(crate) async fn find(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    entry_id: i64,
) -> Result<Option<Entry>> {
    let rows = client
        .query(
            &format!("SELECT {ENTRY_COLUMNS} FROM finance_ledger_entries WHERE tenant_id=$1 AND company_id=$2 AND id=$3"),
            &[&tenant, &company_id, &entry_id],
        )
        .await?;
    Ok(hydrate(client, tenant, company_id, rows).await?.pop())
}

/// Entries by date then id, as [`EntryQuery`] bounds them.
pub(crate) async fn list(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    query: &EntryQuery,
) -> std::result::Result<Vec<Entry>, LedgerError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    let invalid = |id: i64| id < 1;
    if !(1..=MAX_LIMIT).contains(&limit)
        || query.account_id.is_some_and(invalid)
        || query.after.is_some_and(|(_, id)| invalid(id))
        || query.through_entry_number.is_some_and(|number| number < 0)
    {
        return Err(LedgerError::InvalidOption);
    }
    let (from, to) = (
        query.from.map(|d| d.to_string()),
        query.to.map(|d| d.to_string()),
    );
    let (after_date, after_id) = match query.after {
        Some((date, id)) => (Some(date.to_string()), Some(id)),
        None => (None, None),
    };
    // The lines' (company_id, account_id) index finds the account's entries.
    let rows = client
        .query(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM finance_ledger_entries e \
                 WHERE e.tenant_id=$1 AND e.company_id=$2 \
                 AND ($3::text IS NULL OR e.entry_date >= $3::text::date) \
                 AND ($4::text IS NULL OR e.entry_date <= $4::text::date) \
                 AND ($5::bigint IS NULL OR e.entry_number <= $5) \
                 AND ($6::bigint IS NULL OR e.id IN (SELECT entry_id FROM finance_ledger_entry_lines \
                      WHERE company_id=$2 AND account_id=$6)) \
                 AND ($7::text IS NULL OR e.entry_date > $7::text::date \
                      OR (e.entry_date = $7::text::date AND e.id > $8)) \
                 AND (NOT $10 OR ({not_closing})) \
                 ORDER BY e.entry_date, e.id LIMIT $9",
                not_closing = periods::NOT_CLOSING,
            ),
            &[
                &tenant,
                &company_id,
                &from,
                &to,
                &query.through_entry_number,
                &query.account_id,
                &after_date,
                &after_id,
                &i64::from(limit),
                &query.exclude_closing,
            ],
        )
        .await?;
    Ok(hydrate(client, tenant, company_id, rows).await?)
}

/// Adds each entry's lines and the entry that reversed it, if any.
async fn hydrate(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    rows: Vec<Row>,
) -> Result<Vec<Entry>> {
    let ids: Vec<i64> = rows.iter().map(|row| row.get("id")).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut lines: BTreeMap<i64, Vec<EntryLine>> = BTreeMap::new();
    for row in client
        .query(
            &format!("SELECT {LINE_COLUMNS} FROM finance_ledger_entry_lines WHERE tenant_id=$1 AND entry_id = ANY($2) ORDER BY entry_id, line_number"),
            &[&tenant, &ids],
        )
        .await?
    {
        lines.entry(row.get("entry_id")).or_default().push(EntryLine {
            line_number: row.get("line_number"),
            account_id: row.get("account_id"),
            debit: Money::stored(row.get::<_, Decimal>("debit")),
            credit: Money::stored(row.get::<_, Decimal>("credit")),
            description: row.get("description"),
        });
    }
    let reversals: BTreeMap<i64, i64> = client
        .query(
            "SELECT reverses_entry_id,id FROM finance_ledger_entries WHERE tenant_id=$1 AND company_id=$2 AND reverses_entry_id = ANY($3)",
            &[&tenant, &company_id, &ids],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    Ok(rows
        .iter()
        .map(|row| {
            let id: i64 = row.get("id");
            let owner = match (
                row.get::<_, Option<String>>("owner_module"),
                row.get::<_, Option<String>>("owner_record"),
            ) {
                (Some(module), Some(record)) => Some(EntryOwner { module, record }),
                _ => None,
            };
            Entry {
                id,
                entry_number: row.get("entry_number"),
                entry_date: row
                    .get::<_, String>("entry_date")
                    .parse()
                    .unwrap_or(Date::ZERO),
                description: row.get("description"),
                reference: row.get("reference"),
                currency_code: row.get("currency_code"),
                reverses_entry_id: row.get("reverses_entry_id"),
                owner,
                posted_by_user_id: row.get("posted_by_user_id"),
                posted_by_impersonator_id: row.get("posted_by_impersonator_id"),
                posted_at: row
                    .get::<_, String>("posted_at")
                    .parse()
                    .unwrap_or(Timestamp::UNIX_EPOCH),
                reversed_by_entry_id: reversals.get(&id).copied(),
                lines: lines.remove(&id).unwrap_or_default(),
            }
        })
        .collect())
}

pub(crate) fn to_json(entry: &Entry) -> Value {
    json!({
        "entry_number": entry.entry_number,
        "entry_date": entry.entry_date.to_string(),
        "description": entry.description,
        "reference": entry.reference,
        "currency_code": entry.currency_code,
        "reverses_entry_id": entry.reverses_entry_id,
        "owner_module": entry.owner.as_ref().map(|o| &o.module),
        "owner_record": entry.owner.as_ref().map(|o| &o.record),
        "posted_by_user_id": entry.posted_by_user_id,
        "posted_by_impersonator_id": entry.posted_by_impersonator_id,
        "posted_at": entry.posted_at.to_string(),
    })
}
