//! Manual journals (Bilimbi `Ledger.Journal`, `Ledger.JournalLine` and
//! `Ledger.JournalEvent`): an entry submitted for approval, kept with its
//! lines until one event decides it. Its lines obey the rules of the entry it
//! becomes, so submission validates it as `entries::validate` does. The
//! database numbers and stamps a journal and its event, refuses a line added
//! after submission, an unbalanced journal at commit, and a second decision.
//! Workflow proves approval eligibility; its adapter writes the one final
//! journal decision and entry (see `012-workflow-approvals.sql`). The facade authorizes and holds the company lock;
//! these run inside it.
use crate::{
    entries::ValidEntry, EntryLine, FieldError, Journal, JournalDecision, JournalQuery,
    JournalStatus, LedgerError, Money, NewEntry, NewLine,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Result};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_postgres::{GenericClient, Row};

const REASON_LIMIT: usize = 500;
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;
const STAMP: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'";

fn columns() -> String {
    format!(
        "j.id,j.journal_number,j.entry_date::text,j.description,j.reference,j.currency_code,\
         j.submitted_by_user_id,j.submitted_by_impersonator_id,to_char(j.submitted_at,{STAMP}),\
         e.event,e.reason,e.entry_id,e.actor_user_id,e.actor_impersonator_id,to_char(e.occurred_at,{STAMP}),\
         r.replaced_journal_id,b.journal_id,rj.journal_number,bj.journal_number"
    )
}
const FROM: &str = "FROM finance_ledger_journals j \
     LEFT JOIN finance_ledger_journal_events e ON e.journal_id = j.id AND e.company_id = j.company_id \
     LEFT JOIN finance_ledger_journal_replacements r ON r.journal_id = j.id AND r.company_id = j.company_id \
     LEFT JOIN finance_ledger_journal_replacements b ON b.replaced_journal_id = j.id AND b.company_id = j.company_id \
     LEFT JOIN finance_ledger_journals rj ON rj.id = r.replaced_journal_id AND rj.company_id = j.company_id \
     LEFT JOIN finance_ledger_journals bj ON bj.id = b.journal_id AND bj.company_id = j.company_id";

/// Inserts the journal and its lines; the database numbers and stamps it.
pub(crate) async fn insert(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    currency_code: &str,
    submitter: (i64, Option<i64>),
    valid: &ValidEntry,
) -> Result<(Journal, Vec<Change>)> {
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_journals(tenant_id,company_id,journal_number,entry_date,description,\
             reference,currency_code,submitted_by_user_id,submitted_by_impersonator_id,submitted_at) \
             VALUES($1,$2,NULL,$3::text::date,$4,$5,$6,$7,$8,NULL) RETURNING id",
            &[
                &tenant,
                &company_id,
                &valid.entry_date.to_string(),
                &valid.description,
                &valid.reference,
                &currency_code,
                &submitter.0,
                &submitter.1,
            ],
        )
        .await?;
    let journal_id: i64 = row.get(0);
    let mut changes = Vec::new();
    for (index, line) in valid.lines.iter().enumerate() {
        let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let row = client
            .query_one(
                "INSERT INTO finance_ledger_journal_lines(tenant_id,company_id,journal_id,line_number,account_id,\
                 debit,credit,description) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
                &[
                    &tenant,
                    &company_id,
                    &journal_id,
                    &number,
                    &line.account_id,
                    &line.debit.amount(),
                    &line.credit.amount(),
                    &line.description,
                ],
            )
            .await?;
        changes.push(Change {
            kind: "finance.ledger.journal_line".into(),
            id: row.get(0),
            old: Value::Null,
            new: json!({
                "journal_id": journal_id, "line_number": number, "account_id": line.account_id,
                "debit": line.debit.amount().to_string(), "credit": line.credit.amount().to_string(),
                "description": line.description,
            }),
        });
    }
    let journal = find(client, tenant, company_id, journal_id)
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    changes.insert(
        0,
        Change {
            kind: "finance.ledger.journal".into(),
            id: journal_id,
            old: Value::Null,
            new: to_json(&journal),
        },
    );
    Ok((journal, changes))
}

/// Appends the journal's one decision; the database stamps it.
pub(crate) async fn decide(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    journal_id: i64,
    actor: (i64, Option<i64>),
    status: JournalStatus,
    reason: Option<&str>,
    entry_id: Option<i64>,
) -> Result<Change> {
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_journal_events(tenant_id,company_id,journal_id,event,reason,entry_id,\
             actor_user_id,actor_impersonator_id,occurred_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,NULL) RETURNING id",
            &[
                &tenant,
                &company_id,
                &journal_id,
                &status.as_str(),
                &reason,
                &entry_id,
                &actor.0,
                &actor.1,
            ],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.journal_event".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "journal_id": journal_id, "event": status.as_str(), "reason": reason, "entry_id": entry_id,
            "actor_user_id": actor.0, "actor_impersonator_id": actor.1,
        }),
    })
}

/// `JournalEvent.changeset/2`'s `reason` for a rejection: trimmed, required,
/// at most 500 characters.
pub(crate) fn validate_reason(reason: &str) -> std::result::Result<String, Vec<FieldError>> {
    let reason = reason.trim();
    if reason.is_empty() {
        Err(vec![FieldError::new(
            "reason",
            "is required to reject a journal",
        )])
    } else if reason.chars().count() > REASON_LIMIT {
        Err(vec![FieldError::new(
            "reason",
            format!("should be at most {REASON_LIMIT} character(s)"),
        )])
    } else {
        Ok(reason.to_owned())
    }
}

/// The optional reason a submitter gives for withdrawing a journal: trimmed,
/// at most 500 characters, absent when blank.
pub(crate) fn validate_withdrawal_reason(
    reason: Option<&str>,
) -> std::result::Result<Option<String>, Vec<FieldError>> {
    match reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        Some(reason) if reason.chars().count() > REASON_LIMIT => Err(vec![FieldError::new(
            "reason",
            format!("should be at most {REASON_LIMIT} character(s)"),
        )]),
        other => Ok(other.map(str::to_owned)),
    }
}

/// Records that `journal_id` was submitted to replace the withdrawn
/// `replaced_id`; the database refuses it unless this transaction withdrew it.
pub(crate) async fn link(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    journal_id: i64,
    replaced_id: i64,
) -> Result<Change> {
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_journal_replacements(tenant_id,company_id,journal_id,replaced_journal_id) \
             VALUES($1,$2,$3,$4) RETURNING id",
            &[&tenant, &company_id, &journal_id, &replaced_id],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.journal_replacement".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({"journal_id": journal_id, "replaced_journal_id": replaced_id}),
    })
}

/// The entry an approval posts: the journal's date, description, reference
/// and lines.
pub(crate) fn entry(journal: &Journal) -> NewEntry {
    NewEntry {
        entry_date: journal.entry_date,
        description: journal.description.clone(),
        reference: journal.reference.clone(),
        lines: journal
            .lines
            .iter()
            .map(|line| NewLine {
                account_id: line.account_id,
                debit: line.debit,
                credit: line.credit,
                description: line.description.clone(),
            })
            .collect(),
    }
}

/// One journal of the company with its lines and decision.
pub(crate) async fn find(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    journal_id: i64,
) -> Result<Option<Journal>> {
    let rows = client
        .query(
            &format!(
                "SELECT {} {FROM} WHERE j.tenant_id=$1 AND j.company_id=$2 AND j.id=$3",
                columns()
            ),
            &[&tenant, &company_id, &journal_id],
        )
        .await?;
    Ok(hydrate(client, tenant, rows).await?.pop())
}

/// Journals by number, as the query bounds them.
pub(crate) async fn list(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    query: &JournalQuery,
) -> std::result::Result<Vec<Journal>, LedgerError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) || query.after.is_some_and(|after| after < 0) {
        return Err(LedgerError::InvalidOption);
    }
    // A submitted journal has no event; a decided one has exactly one.
    let status = query.status.map(JournalStatus::as_str);
    let rows = client
        .query(
            &format!(
                "SELECT {} {FROM} WHERE j.tenant_id=$1 AND j.company_id=$2 \
                 AND ($3::text IS NULL OR COALESCE(e.event, 'submitted') = $3) \
                 AND ($4::bigint IS NULL OR j.journal_number > $4) \
                 ORDER BY j.journal_number LIMIT $5",
                columns()
            ),
            &[
                &tenant,
                &company_id,
                &status,
                &query.after,
                &i64::from(limit),
            ],
        )
        .await?;
    Ok(hydrate(client, tenant, rows).await?)
}

async fn hydrate(
    client: &dyn OperationClient,
    tenant: i64,
    rows: Vec<Row>,
) -> Result<Vec<Journal>> {
    let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut lines: BTreeMap<i64, Vec<EntryLine>> = BTreeMap::new();
    for row in client
        .query(
            "SELECT journal_id,line_number,account_id,debit,credit,description FROM finance_ledger_journal_lines \
             WHERE tenant_id=$1 AND journal_id = ANY($2) ORDER BY journal_id, line_number",
            &[&tenant, &ids],
        )
        .await?
    {
        lines.entry(row.get(0)).or_default().push(EntryLine {
            line_number: row.get(1),
            account_id: row.get(2),
            debit: Money::stored(row.get::<_, Decimal>(3)),
            credit: Money::stored(row.get::<_, Decimal>(4)),
            description: row.get(5),
        });
    }
    rows.iter()
        .map(|row| {
            let id: i64 = row.get(0);
            let decision = match row.get::<_, Option<String>>(9) {
                Some(event) => Some(JournalDecision {
                    event: JournalStatus::parse(&event).ok_or(bilibi_database::Error::Internal)?,
                    reason: row.get(10),
                    entry_id: row.get(11),
                    actor_user_id: row.get(12),
                    actor_impersonator_id: row.get(13),
                    occurred_at: parse(row.get(14))?,
                }),
                None => None,
            };
            Ok(Journal {
                id,
                journal_number: row.get(1),
                entry_date: parse::<Date>(row.get(2))?,
                description: row.get(3),
                reference: row.get(4),
                currency_code: row.get(5),
                submitted_by_user_id: row.get(6),
                submitted_by_impersonator_id: row.get(7),
                submitted_at: parse(row.get(8))?,
                status: decision
                    .as_ref()
                    .map_or(JournalStatus::Submitted, |decision| decision.event),
                decision,
                replaces_journal_id: row.get(15),
                replaced_by_journal_id: row.get(16),
                replaces_journal_number: row.get(17),
                replaced_by_journal_number: row.get(18),
                lines: lines.remove(&id).unwrap_or_default(),
            })
        })
        .collect()
}

fn to_json(journal: &Journal) -> Value {
    json!({
        "journal_number": journal.journal_number,
        "entry_date": journal.entry_date.to_string(),
        "description": journal.description,
        "reference": journal.reference,
        "currency_code": journal.currency_code,
        "submitted_by_user_id": journal.submitted_by_user_id,
        "submitted_by_impersonator_id": journal.submitted_by_impersonator_id,
        "submitted_at": journal.submitted_at.to_string(),
    })
}

fn parse<T: std::str::FromStr>(text: String) -> Result<T> {
    text.parse().map_err(|_| bilibi_database::Error::Internal)
}
