//! Statement lines against ledger entries on the bank account's ledger
//! account (Bilimbi `Reconciliation.Matching`, `Match`, `MatchLine`,
//! `MatchEntry` and `MatchEvent`) or against opening items: what is unmatched
//! on each side, suggested ordinary matches, and the matches themselves with
//! the events that make and unmake them. The facade authorizes every call and
//! holds the company lock for writes; the ledger is read only through its
//! facade, on the connection the caller hands in.
use crate::{
    statements::stored, BankAccount, FieldError, LedgerItem, Match, MatchEvent, MatchStatus,
    OpeningItem, ReconciliationError, Result, StatementLine, StatementLineItem,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_finance_ledger::{Entry, EntryQuery, Ledger, LedgerError, Money};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio_postgres::{GenericClient, Row};

/// The ledger is read 500 entries at a time, its largest page.
const PAGE: u32 = 500;
const TEXT_LIMIT: usize = 500;
const ISO: &str = r#"'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'"#;

/// The ids of the bank account's matches standing at match-event number `$4`
/// (NULL for now): a `matched` event numbered through it and no `unmatched`
/// event that is. `$1` tenant, `$2` company, `$3` bank account.
const STANDING: &str = "SELECT m.id FROM finance_reconciliation_matches m \
     JOIN finance_reconciliation_match_events e ON e.match_id = m.id AND e.action = 'matched' \
     WHERE m.tenant_id = $1 AND m.company_id = $2 AND m.bank_account_id = $3 \
       AND ($4::bigint IS NULL OR e.event_number <= $4) \
       AND NOT EXISTS (SELECT 1 FROM finance_reconciliation_match_events u \
                       WHERE u.match_id = m.id AND u.action = 'unmatched' \
                         AND ($4::bigint IS NULL OR u.event_number <= $4))";

/// Where a reading of the two sides stops: entry dates and statement-line
/// dates through `to`, and the entry, statement, match-event and opening-bridge
/// boundaries it counts through (each `None` for now).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bounds {
    pub to: Option<Date>,
    pub through_entry_number: Option<i64>,
    pub through_statement_number: Option<i64>,
    pub through_match_event_number: Option<i64>,
    pub through_opening_id: Option<i64>,
}

/// What stands unmatched on the statement and book sides, including opening
/// items that stand in for the opening entry.
pub(crate) struct Sides {
    pub lines: Vec<StatementLineItem>,
    pub entries: Vec<LedgerItem>,
    pub opening_items: Vec<OpeningItem>,
}

/// The bridge as it stood at this pin. An older bridge remains available
/// after a correction so previously pinned reports can be reproduced.
async fn bridge(
    client: &impl GenericClient,
    tenant: i64,
    company: i64,
    account: i64,
    pin: Option<i64>,
    entry_pin: Option<i64>,
) -> Result<Option<(i64, i64, Date)>> {
    let row = client.query_opt("SELECT id,opening_entry_id,to_char(conversion_date,'YYYY-MM-DD') FROM finance_reconciliation_opening_bridges WHERE tenant_id=$1 AND company_id=$2 AND bank_account_id=$3 AND ($4::bigint IS NULL OR id<=$4) AND ($5::bigint IS NULL OR opening_entry_number<=$5) ORDER BY id DESC LIMIT 1", &[&tenant,&company,&account,&pin,&entry_pin]).await.map_err(Error::from)?;
    row.map(|r| Ok((r.get(0), r.get(1), date(&r, 2)?)))
        .transpose()
}

pub(crate) async fn last_opening_id(
    client: &impl GenericClient,
    tenant: i64,
    company: i64,
) -> Result<i64> {
    Ok(client.query_one("SELECT COALESCE(max(id),0)::bigint FROM finance_reconciliation_opening_bridges WHERE tenant_id=$1 AND company_id=$2", &[&tenant,&company]).await.map_err(Error::from)?.get(0))
}

fn opening_item(row: &Row, offset: usize) -> Result<OpeningItem> {
    Ok(OpeningItem {
        id: row.get(offset),
        bridge_id: row.get(offset + 1),
        source_id: row.get(offset + 2),
        item_date: date(row, offset + 3)?,
        description: row.get(offset + 4),
        amount: stored(row.get(offset + 5))?,
        conversion_date: date(row, offset + 6)?,
    })
}

async fn opening_items(
    client: &impl GenericClient,
    tenant: i64,
    company: i64,
    account: i64,
    bounds: Bounds,
) -> Result<Vec<OpeningItem>> {
    let Some((bridge, _, conversion)) = bridge(
        client,
        tenant,
        company,
        account,
        bounds.through_opening_id,
        bounds.through_entry_number,
    )
    .await?
    else {
        return Ok(vec![]);
    };
    if bounds.to.is_some_and(|end| end < conversion) {
        return Ok(vec![]);
    }
    client.query("SELECT i.id,i.bridge_id,i.source_id,to_char(i.item_date,'YYYY-MM-DD'),i.description,i.amount,to_char(b.conversion_date,'YYYY-MM-DD') FROM finance_reconciliation_opening_items i JOIN finance_reconciliation_opening_bridges b ON b.id=i.bridge_id WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.bridge_id=$3 AND NOT EXISTS (SELECT 1 FROM finance_reconciliation_match_opening_items x JOIN finance_reconciliation_match_events e ON e.match_id=x.match_id AND e.action='matched' WHERE x.opening_item_id=i.id AND ($4::bigint IS NULL OR e.event_number<=$4) AND NOT EXISTS (SELECT 1 FROM finance_reconciliation_match_events u WHERE u.match_id=x.match_id AND u.action='unmatched' AND ($4::bigint IS NULL OR u.event_number<=$4))) ORDER BY i.item_date,i.id", &[&tenant,&company,&bridge,&bounds.through_match_event_number]).await.map_err(Error::from)?.iter().map(|r| opening_item(r,0)).collect()
}

// Ledger side

/// Every entry with a posting on the bank account's ledger account, within
/// `bounds`' date and entry number, as its side of that account; an entry
/// whose postings there cancel is left out. Read through the ledger facade
/// page by page, on `client`.
pub(crate) async fn ledger_items(
    ledger: &Ledger,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    bounds: Bounds,
) -> Result<Vec<LedgerItem>> {
    let mut query = EntryQuery {
        to: bounds.to,
        limit: Some(PAGE),
        account_id: Some(account.ledger_account_id),
        through_entry_number: bounds.through_entry_number,
        ..EntryQuery::default()
    };
    let mut items = Vec::new();
    loop {
        let entries = ledger
            .list_entries_on(client, scope, company_id, &query)
            .await?;
        items.extend(
            entries
                .iter()
                .filter_map(|entry| ledger_item(entry, account.ledger_account_id)),
        );
        match entries.last() {
            Some(last) if entries.len() == PAGE as usize => {
                query.after = Some((last.entry_date, last.id));
            }
            _ => return Ok(items),
        }
    }
}

/// One entry's side of the account: its debits less its credits there, or
/// `None` when they cancel.
pub(crate) fn ledger_item(entry: &Entry, account_id: i64) -> Option<LedgerItem> {
    let amount: Money = entry
        .lines
        .iter()
        .filter(|line| line.account_id == account_id)
        .map(|line| line.debit - line.credit)
        .sum();
    (!amount.is_zero()).then(|| LedgerItem {
        entry_id: entry.id,
        entry_number: entry.entry_number,
        entry_date: entry.entry_date,
        description: entry.description.clone(),
        reference: entry.reference.clone(),
        amount,
    })
}

// Numbers a reading is pinned to

/// The company's latest match-event number, 0 before any.
pub(crate) async fn last_event_number(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT COALESCE(max(event_number), 0)::bigint FROM finance_reconciliation_match_events \
             WHERE tenant_id = $1 AND company_id = $2",
            &[&tenant, &company_id],
        )
        .await
        .map_err(Error::from)?
        .get(0))
}

/// The company's latest statement number, 0 before any.
pub(crate) async fn last_statement_number(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT COALESCE(max(statement_number), 0)::bigint FROM finance_reconciliation_statements \
             WHERE tenant_id = $1 AND company_id = $2",
            &[&tenant, &company_id],
        )
        .await
        .map_err(Error::from)?
        .get(0))
}

pub(crate) async fn activity_boundary(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    through: Date,
) -> Result<(i64, i64)> {
    let through = through.to_string();
    let row = client
        .query_one(
            "SELECT \
             COALESCE((SELECT max(s.statement_number) FROM finance_reconciliation_statements s \
               WHERE s.tenant_id=$1 AND s.company_id=$2 AND s.bank_account_id=$3 \
                 AND EXISTS (SELECT 1 FROM finance_reconciliation_statement_lines l \
                   WHERE l.statement_id=s.id AND l.line_date <= $4::text::date)),0)::bigint, \
             COALESCE((SELECT max(e.event_number) FROM finance_reconciliation_match_events e \
               JOIN finance_reconciliation_matches m ON m.id=e.match_id \
               WHERE m.tenant_id=$1 AND m.company_id=$2 AND m.bank_account_id=$3 \
                 AND (EXISTS (SELECT 1 FROM finance_reconciliation_match_lines ml \
                       JOIN finance_reconciliation_statement_lines l ON l.id=ml.statement_line_id \
                       WHERE ml.match_id=m.id AND l.line_date <= $4::text::date) \
                   OR EXISTS (SELECT 1 FROM finance_reconciliation_match_entries me \
                       WHERE me.match_id=m.id AND me.entry_date <= $4::text::date) \
                   OR EXISTS (SELECT 1 FROM finance_reconciliation_match_opening_items moi \
                       JOIN finance_reconciliation_opening_items oi ON oi.id=moi.opening_item_id \
                       JOIN finance_reconciliation_opening_bridges ob ON ob.id=oi.bridge_id \
                       WHERE moi.tenant_id=$1 AND moi.company_id=$2 AND moi.match_id=m.id \
                         AND ob.bank_account_id=$3 \
                         AND ob.conversion_date <= $4::text::date))),0)::bigint",
            &[&tenant, &company_id, &bank_account_id, &through],
        )
        .await
        .map_err(Error::from)?;
    Ok((row.get(0), row.get(1)))
}

// Statement side

/// The bank account's statement lines within `bounds`' date and statement
/// number, by date, statement and line, each with its statement number.
pub(crate) async fn statement_lines(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    bounds: Bounds,
) -> Result<Vec<StatementLineItem>> {
    let to = bounds.to.map(|date| date.to_string());
    client
        .query(
            "SELECT l.id,l.statement_id,l.bank_account_id,l.line_number,to_char(l.line_date,'YYYY-MM-DD'),\
             l.description,l.reference,l.amount,l.running_balance,s.statement_number \
             FROM finance_reconciliation_statement_lines l \
             JOIN finance_reconciliation_statements s ON s.id = l.statement_id \
             WHERE l.tenant_id = $1 AND l.company_id = $2 AND l.bank_account_id = $3 \
               AND ($4::text IS NULL OR l.line_date <= $4::text::date) \
               AND ($5::bigint IS NULL OR s.statement_number <= $5) \
             ORDER BY l.line_date, s.statement_number, l.line_number",
            &[
                &tenant,
                &company_id,
                &bank_account_id,
                &to,
                &bounds.through_statement_number,
            ],
        )
        .await
        .map_err(Error::from)?
        .iter()
        .map(|row| {
            Ok(StatementLineItem {
                line: line_from_row(row)?,
                statement_number: row.get(9),
            })
        })
        .collect()
}

/// One statement line of the company, or `None`.
pub(crate) async fn find_line(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    line_id: i64,
) -> Result<Option<StatementLine>> {
    client
        .query_opt(
            "SELECT id,statement_id,bank_account_id,line_number,to_char(line_date,'YYYY-MM-DD'),\
             description,reference,amount,running_balance \
             FROM finance_reconciliation_statement_lines WHERE tenant_id = $1 AND company_id = $2 AND id = $3",
            &[&tenant, &company_id, &line_id],
        )
        .await
        .map_err(Error::from)?
        .as_ref()
        .map(line_from_row)
        .transpose()
}

fn line_from_row(row: &Row) -> Result<StatementLine> {
    Ok(StatementLine {
        id: row.get(0),
        statement_id: row.get(1),
        bank_account_id: row.get(2),
        line_number: row.get(3),
        line_date: date(row, 4)?,
        description: row.get(5),
        reference: row.get(6),
        amount: stored(row.get(7))?,
        running_balance: row.get::<_, Option<Decimal>>(8).map(stored).transpose()?,
    })
}

/// The statement line ids and entry ids the bank account's standing matches
/// hold at match-event number `through` (`None` for now).
pub(crate) async fn taken(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    through: Option<i64>,
) -> Result<(HashSet<i64>, HashSet<i64>)> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] =
        [&tenant, &company_id, &bank_account_id, &through];
    let lines = client
        .query(
            &format!("SELECT statement_line_id FROM finance_reconciliation_match_lines WHERE match_id IN ({STANDING})"),
            &params,
        )
        .await
        .map_err(Error::from)?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let entries = client
        .query(
            &format!("SELECT entry_id FROM finance_reconciliation_match_entries WHERE match_id IN ({STANDING})"),
            &params,
        )
        .await
        .map_err(Error::from)?
        .iter()
        .map(|row| row.get(0))
        .collect();
    Ok((lines, entries))
}

/// What stands unmatched on each side within `bounds`.
pub(crate) async fn unmatched(
    ledger: &Ledger,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    bounds: Bounds,
) -> Result<Sides> {
    let tenant = scope.tenant();
    let items = ledger_items(ledger, client, scope, company_id, account, bounds).await?;
    let (lines_taken, entries_taken) = taken(
        client,
        tenant,
        company_id,
        account.id,
        bounds.through_match_event_number,
    )
    .await?;
    let lines = statement_lines(client, tenant, company_id, account.id, bounds)
        .await?
        .into_iter()
        .filter(|item| !lines_taken.contains(&item.line.id))
        .collect();
    let opening_entry = bridge(
        client,
        tenant,
        company_id,
        account.id,
        bounds.through_opening_id,
        bounds.through_entry_number,
    )
    .await?
    .map(|(_, entry, _)| entry);
    let entries = items
        .into_iter()
        .filter(|item| {
            !entries_taken.contains(&item.entry_id) && Some(item.entry_id) != opening_entry
        })
        .collect();
    let opening_items = opening_items(client, tenant, company_id, account.id, bounds).await?;
    Ok(Sides {
        lines,
        entries,
        opening_items,
    })
}

// Suggestions

/// One-to-one suggestions: for each unmatched statement line in turn, the
/// unmatched entry of the same amount dated within `window_days` of it,
/// preferring one whose reference or description names the line's reference
/// (or the other way round), then the nearest date, then the lowest entry
/// number. Each entry is suggested once. Keyed by statement line id.
pub(crate) fn suggestions(
    lines: &[StatementLineItem],
    entries: &[LedgerItem],
    window_days: i32,
) -> BTreeMap<i64, LedgerItem> {
    let mut by_amount: HashMap<Decimal, Vec<&LedgerItem>> = HashMap::new();
    for entry in entries {
        by_amount
            .entry(entry.amount.amount().normalize())
            .or_default()
            .push(entry);
    }
    let mut used = HashSet::new();
    let mut suggested = BTreeMap::new();
    for item in lines {
        let line = &item.line;
        let best = by_amount
            .get(&line.amount.amount().normalize())
            .into_iter()
            .flatten()
            .filter(|entry| !used.contains(&entry.entry_id))
            .filter_map(|entry| {
                let days = days_apart(entry.entry_date, line.line_date)?;
                (days <= i64::from(window_days)).then_some((
                    !reference_hit(line, entry),
                    days,
                    entry.entry_number,
                    *entry,
                ))
            })
            .min_by_key(|(miss, days, number, _)| (*miss, *days, *number));
        if let Some((_, _, _, entry)) = best {
            used.insert(entry.entry_id);
            suggested.insert(line.id, entry.clone());
        }
    }
    suggested
}

fn days_apart(a: Date, b: Date) -> Option<i64> {
    let span = a.until(b).ok()?;
    Some(i64::from(span.get_days()).abs())
}

fn reference_hit(line: &StatementLine, entry: &LedgerItem) -> bool {
    let needle = normalize(line.reference.as_deref());
    let entry_needle = normalize(entry.reference.as_deref());
    (!needle.is_empty()
        && [entry.reference.as_deref(), Some(entry.description.as_str())]
            .into_iter()
            .any(|haystack| normalize(haystack).contains(&needle)))
        || (!entry_needle.is_empty() && normalize(Some(&line.description)).contains(&entry_needle))
}

/// Lower case, letters and digits only (Bilimbi's `[^a-z0-9]` strip).
fn normalize(text: Option<&str>) -> String {
    text.unwrap_or_default()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

// Matching

/// The two sides of a match the facade may record: the bank account's own
/// statement lines and its ledger account's entries, both unmatched and
/// summing to the same amount. Refusals in Bilimbi's order; the ledger is
/// read on `client`, the caller's transaction.
pub(crate) async fn prepare(
    ledger: &Ledger,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    line_ids: &[i64],
    entry_ids: &[i64],
) -> Result<(Vec<StatementLine>, Vec<LedgerItem>)> {
    let (line_ids, entry_ids) = (distinct(line_ids), distinct(entry_ids));
    if line_ids.is_empty()
        || entry_ids.is_empty()
        || line_ids.iter().chain(&entry_ids).any(|id| *id < 1)
    {
        return Err(ReconciliationError::InvalidSelection);
    }
    let tenant = scope.tenant();
    let mut lines = Vec::with_capacity(line_ids.len());
    for id in &line_ids {
        match find_line(client, tenant, company_id, *id).await? {
            Some(line) if line.bank_account_id == account.id => lines.push(line),
            _ => return Err(ReconciliationError::StatementLineNotFound),
        }
    }
    let opening_entry = bridge(client, tenant, company_id, account.id, None, None)
        .await?
        .map(|(_, entry, _)| entry);
    if entry_ids.iter().any(|id| Some(*id) == opening_entry) {
        return Err(ReconciliationError::Invalid(vec![FieldError::new("entry_ids", "the opening entry is represented by the outstanding opening items, not a matchable entry")]));
    }
    let mut items = Vec::with_capacity(entry_ids.len());
    for id in &entry_ids {
        let entry = match ledger.get_entry_on(client, scope, company_id, *id).await {
            Err(LedgerError::EntryNotFound) => return Err(ReconciliationError::EntryNotFound),
            other => other?,
        };
        items.push(
            ledger_item(&entry, account.ledger_account_id)
                .ok_or(ReconciliationError::EntryNotOnAccount)?,
        );
    }
    let (lines_taken, entries_taken) = taken(client, tenant, company_id, account.id, None).await?;
    if lines.iter().any(|line| lines_taken.contains(&line.id)) {
        return Err(ReconciliationError::StatementLineMatched);
    }
    if items
        .iter()
        .any(|item| entries_taken.contains(&item.entry_id))
    {
        return Err(ReconciliationError::EntryMatched);
    }
    let statement_side: Money = lines.iter().map(|line| line.amount).sum();
    let ledger_side: Money = items.iter().map(|item| item.amount).sum();
    if statement_side != ledger_side {
        return Err(ReconciliationError::Unbalanced {
            statement_side,
            ledger_side,
        });
    }
    Ok((lines, items))
}

fn distinct(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// A match's note: trimmed, blank as none, at most 500 characters.
pub(crate) fn note(requested: Option<&str>) -> std::result::Result<Option<String>, FieldError> {
    optional_text("note", requested)
}

/// An unmatch's reason, which is needed.
pub(crate) fn unmatch_reason(requested: Option<&str>) -> std::result::Result<String, FieldError> {
    optional_text("reason", requested)?
        .ok_or_else(|| FieldError::new("reason", "is needed to unmatch"))
}

pub(crate) fn optional_text(
    field: &'static str,
    requested: Option<&str>,
) -> std::result::Result<Option<String>, FieldError> {
    match requested.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) if text.chars().count() > TEXT_LIMIT => Err(FieldError::new(
            field,
            format!("should be at most {TEXT_LIMIT} character(s)"),
        )),
        other => Ok(other.map(str::to_owned)),
    }
}

/// Records the match with its lines, its entries (copied as the ledger shows
/// them) and its `matched` event, in one audited write; the database numbers
/// the match and the event. Returns the match id.
pub(crate) async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    (actor, impersonator): (i64, Option<i64>),
    bank_account_id: i64,
    lines: &[StatementLine],
    items: &[LedgerItem],
    note: Option<String>,
) -> Result<i64> {
    let line_ids: Vec<i64> = lines.iter().map(|line| line.id).collect();
    let items = items.to_vec();
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        "INSERT INTO finance_reconciliation_matches(tenant_id,company_id,bank_account_id,match_number,note,matched_by_user_id,matched_by_impersonator_id,matched_at) \
                         VALUES($1,$2,$3,NULL,$4,$5,$6,NULL) RETURNING id,match_number",
                        &[&tenant, &company_id, &bank_account_id, &note, &actor, &impersonator],
                    )
                    .await?;
                let (match_id, match_number): (i64, i64) = (row.get(0), row.get(1));
                let mut changes = vec![Change {
                    kind: "finance.reconciliation.match".into(),
                    id: match_id,
                    old: Value::Null,
                    new: json!({
                        "company_id": company_id, "bank_account_id": bank_account_id,
                        "match_number": match_number, "note": note,
                        "matched_by_user_id": actor, "matched_by_impersonator_id": impersonator,
                    }),
                }];
                for line_id in &line_ids {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO finance_reconciliation_match_lines(tenant_id,company_id,match_id,statement_line_id) \
                             VALUES($1,$2,$3,$4) RETURNING id",
                            &[&tenant, &company_id, &match_id, line_id],
                        )
                        .await?
                        .get(0);
                    changes.push(Change {
                        kind: "finance.reconciliation.match_line".into(),
                        id,
                        old: Value::Null,
                        new: json!({"company_id": company_id, "match_id": match_id, "statement_line_id": line_id}),
                    });
                }
                for item in &items {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO finance_reconciliation_match_entries(tenant_id,company_id,match_id,bank_account_id,entry_id,entry_number,entry_date,description,reference,amount) \
                             VALUES($1,$2,$3,$4,$5,$6,$7::text::date,$8,$9,$10) RETURNING id",
                            &[
                                &tenant, &company_id, &match_id, &bank_account_id, &item.entry_id,
                                &item.entry_number, &item.entry_date.to_string(), &item.description,
                                &item.reference, &item.amount.amount(),
                            ],
                        )
                        .await?
                        .get(0);
                    changes.push(Change {
                        kind: "finance.reconciliation.match_entry".into(),
                        id,
                        old: Value::Null,
                        new: json!({
                            "company_id": company_id, "match_id": match_id, "bank_account_id": bank_account_id,
                            "entry_id": item.entry_id, "entry_number": item.entry_number,
                            "entry_date": item.entry_date.to_string(), "description": item.description,
                            "reference": item.reference, "amount": item.amount.to_string(),
                        }),
                    });
                }
                changes.push(event(sql, tenant, company_id, (actor, impersonator), match_id, "matched", None).await?);
                Ok((match_id, changes))
            })
        })
        .await?)
}

/// Refuses an unmatch the database would: a match that no longer stands, or
/// one a finalised reconciliation of its bank account counted.
pub(crate) async fn check_unmatch(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    found: &Match,
) -> Result<()> {
    if found.status != MatchStatus::Matched {
        return Err(ReconciliationError::MatchNotStanding);
    }
    let matched = found
        .events
        .iter()
        .find(|event| event.action == "matched")
        .ok_or(Error::Internal)?;
    let counted = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM finance_reconciliation_reconciliation_events e \
             JOIN finance_reconciliation_reconciliations r ON r.id = e.reconciliation_id \
             WHERE r.tenant_id = $1 AND r.company_id = $2 AND r.bank_account_id = $3 \
               AND e.action = 'finalised' AND e.through_match_event_number >= $4)",
            &[
                &tenant,
                &company_id,
                &found.bank_account_id,
                &matched.event_number,
            ],
        )
        .await
        .map_err(Error::from)?
        .get::<_, bool>(0);
    if counted {
        Err(ReconciliationError::MatchFinalised)
    } else {
        Ok(())
    }
}

/// Records the match's `unmatched` event with its reason.
pub(crate) async fn record_unmatch(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    people: (i64, Option<i64>),
    match_id: i64,
    reason: String,
) -> Result<()> {
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let change = event(
                    sql,
                    tenant,
                    company_id,
                    people,
                    match_id,
                    "unmatched",
                    Some(&reason),
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?)
}

async fn event(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    (actor, impersonator): (i64, Option<i64>),
    match_id: i64,
    action: &str,
    reason: Option<&str>,
) -> bilibi_database::Result<Change> {
    let row = sql
        .query_one(
            "INSERT INTO finance_reconciliation_match_events(tenant_id,company_id,event_number,match_id,action,reason,recorded_by_user_id,recorded_by_impersonator_id,recorded_at) \
             VALUES($1,$2,NULL,$3,$4,$5,$6,$7,NULL) RETURNING id,event_number",
            &[&tenant, &company_id, &match_id, &action, &reason, &actor, &impersonator],
        )
        .await?;
    Ok(Change {
        kind: "finance.reconciliation.match_event".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "company_id": company_id, "match_id": match_id, "event_number": row.get::<_, i64>(1),
            "action": action, "reason": reason,
            "recorded_by_user_id": actor, "recorded_by_impersonator_id": impersonator,
        }),
    })
}

// Reading

const MATCH_COLUMNS: &str =
    "id,bank_account_id,match_number,note,matched_by_user_id,matched_by_impersonator_id";

/// One match of the company with its lines, entries, opening items and events,
/// or `None`.
pub(crate) async fn find(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    match_id: i64,
) -> Result<Option<Match>> {
    let rows = client
        .query(
            &format!("SELECT {MATCH_COLUMNS},to_char(matched_at,{ISO}) FROM finance_reconciliation_matches \
                      WHERE tenant_id = $1 AND company_id = $2 AND id = $3"),
            &[&tenant, &company_id, &match_id],
        )
        .await
        .map_err(Error::from)?;
    Ok(hydrate(client, tenant, company_id, &rows).await?.pop())
}

/// The bank account's matches after the id `after`, standing or not, oldest
/// first.
pub(crate) async fn list(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    limit: i64,
    after: i64,
) -> Result<Vec<Match>> {
    let rows = client
        .query(
            &format!("SELECT {MATCH_COLUMNS},to_char(matched_at,{ISO}) FROM finance_reconciliation_matches \
                      WHERE tenant_id = $1 AND company_id = $2 AND bank_account_id = $3 AND id > $4 ORDER BY id LIMIT $5"),
            &[&tenant, &company_id, &bank_account_id, &after, &limit],
        )
        .await
        .map_err(Error::from)?;
    hydrate(client, tenant, company_id, &rows).await
}

/// The bank account's matches standing at match-event number `through`
/// (`None` for now), by match number, with their parts.
pub(crate) async fn standing(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    through: Option<i64>,
) -> Result<Vec<Match>> {
    let rows = client
        .query(
            &format!("SELECT {MATCH_COLUMNS},to_char(matched_at,{ISO}) FROM finance_reconciliation_matches \
                      WHERE id IN ({STANDING}) ORDER BY match_number"),
            &[&tenant, &company_id, &bank_account_id, &through],
        )
        .await
        .map_err(Error::from)?;
    hydrate(client, tenant, company_id, &rows).await
}

/// The bank account's standing match holding the statement line, or `None`.
pub(crate) async fn standing_for_line(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    line_id: i64,
) -> Result<Option<Match>> {
    let none: Option<i64> = None;
    let row = client
        .query_opt(
            &format!(
                "SELECT match_id FROM finance_reconciliation_match_lines \
                      WHERE statement_line_id = $5 AND match_id IN ({STANDING})"
            ),
            &[&tenant, &company_id, &bank_account_id, &none, &line_id],
        )
        .await
        .map_err(Error::from)?;
    match row {
        Some(row) => find(client, tenant, company_id, row.get(0)).await,
        None => Ok(None),
    }
}

/// The matches of `rows` with their statement lines, entries and opening items
/// by number, and events by number, in the rows' order.
async fn hydrate(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    rows: &[Row],
) -> Result<Vec<Match>> {
    let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
    let mut lines: HashMap<i64, Vec<i64>> = HashMap::new();
    for row in client
        .query(
            "SELECT match_id,statement_line_id FROM finance_reconciliation_match_lines \
             WHERE tenant_id = $1 AND company_id = $2 AND match_id = ANY($3) ORDER BY id",
            &[&tenant, &company_id, &ids],
        )
        .await
        .map_err(Error::from)?
    {
        lines.entry(row.get(0)).or_default().push(row.get(1));
    }
    let mut entries: HashMap<i64, Vec<LedgerItem>> = HashMap::new();
    for row in client
        .query(
            "SELECT match_id,entry_id,entry_number,to_char(entry_date,'YYYY-MM-DD'),description,reference,amount \
             FROM finance_reconciliation_match_entries \
             WHERE tenant_id = $1 AND company_id = $2 AND match_id = ANY($3) ORDER BY entry_number",
            &[&tenant, &company_id, &ids],
        )
        .await
        .map_err(Error::from)?
    {
        entries.entry(row.get(0)).or_default().push(LedgerItem {
            entry_id: row.get(1),
            entry_number: row.get(2),
            entry_date: date(&row, 3)?,
            description: row.get(4),
            reference: row.get(5),
            amount: stored(row.get(6))?,
        });
    }
    let mut opening: HashMap<i64, Vec<OpeningItem>> = HashMap::new();
    for row in client.query("SELECT x.match_id,i.id,i.bridge_id,i.source_id,to_char(i.item_date,'YYYY-MM-DD'),i.description,i.amount,to_char(b.conversion_date,'YYYY-MM-DD') FROM finance_reconciliation_match_opening_items x JOIN finance_reconciliation_opening_items i ON i.id=x.opening_item_id JOIN finance_reconciliation_opening_bridges b ON b.id=i.bridge_id WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.match_id=ANY($3) ORDER BY i.id", &[&tenant,&company_id,&ids]).await.map_err(Error::from)? {
        opening.entry(row.get(0)).or_default().push(opening_item(&row, 1)?);
    }
    let mut events: HashMap<i64, Vec<MatchEvent>> = HashMap::new();
    for row in client
        .query(
            &format!("SELECT match_id,event_number,action,reason,recorded_by_user_id,recorded_by_impersonator_id,\
                      to_char(recorded_at,{ISO}) FROM finance_reconciliation_match_events \
                      WHERE tenant_id = $1 AND company_id = $2 AND match_id = ANY($3) ORDER BY event_number"),
            &[&tenant, &company_id, &ids],
        )
        .await
        .map_err(Error::from)?
    {
        events.entry(row.get(0)).or_default().push(MatchEvent {
            event_number: row.get(1),
            action: row.get(2),
            reason: row.get(3),
            recorded_by_user_id: row.get(4),
            recorded_by_impersonator_id: row.get(5),
            recorded_at: timestamp(&row, 6)?,
        });
    }
    rows.iter()
        .map(|row| {
            let id: i64 = row.get(0);
            let events = events.remove(&id).unwrap_or_default();
            let status = if events.iter().any(|event| event.action == "unmatched") {
                MatchStatus::Unmatched
            } else {
                MatchStatus::Matched
            };
            Ok(Match {
                id,
                bank_account_id: row.get(1),
                match_number: row.get(2),
                note: row.get(3),
                matched_by_user_id: row.get(4),
                matched_by_impersonator_id: row.get(5),
                matched_at: timestamp(row, 6)?,
                status,
                statement_line_ids: lines.remove(&id).unwrap_or_default(),
                entries: entries.remove(&id).unwrap_or_default(),
                opening_items: opening.remove(&id).unwrap_or_default(),
                events,
            })
        })
        .collect()
}

pub(crate) fn date(row: &Row, index: usize) -> Result<Date> {
    Ok(row
        .get::<_, String>(index)
        .parse()
        .map_err(|_| Error::Internal)?)
}

pub(crate) fn timestamp(row: &Row, index: usize) -> Result<jiff::Timestamp> {
    Ok(row
        .get::<_, String>(index)
        .parse()
        .map_err(|_| Error::Internal)?)
}
