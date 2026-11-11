//! A bank account's reconciliations (Bilimbi `Reconciliation.Reconciliations`,
//! `ReconciliationRecord`, `ReconciliationEvent` and `ReconciliationItem`):
//! the figures as of a statement end date, and the events that open,
//! finalise and abandon them. The facade authorizes every call and holds the
//! company lock for writes; the ledger is read only through its facade, on
//! the connection the caller hands in.
use crate::{
    matching::{self, date, timestamp, Bounds},
    statements::stored,
    BankAccount, BankReconciliation, FieldError, Figures, ItemKind, Pins, ReconciliationError,
    ReconciliationEvent, ReconciliationItem, ReconciliationPreview, ReconciliationStatus, Result,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_finance_ledger::{Ledger, MinorUnits, Money, MoneyError, TotalsQuery};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_postgres::{GenericClient, Row};

const ISO: &str = r#"'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'"#;
const TEXT_LIMIT: usize = 500;

// Figures

/// The reconciliation of the bank account as of `end_date` against
/// `statement_balance`, read at `pins` (each the latest when absent). The
/// book balance is the ledger account's debits less credits through
/// `end_date`, summed by PostgreSQL through the ledger. The items that
/// explain the difference are the statement lines through `end_date` with no
/// standing match, the entries and opening items through `end_date` with none,
/// and each standing match whose two sides through `end_date` differ
/// (`timing`). `difference` is the statement balance less the book balance,
/// less the unmatched statement lines, plus the unmatched entries and opening
/// items, plus the timing items; zero means the two balances are explained.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn compute(
    ledger: &Ledger,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    end_date: Date,
    statement_balance: Money,
    pins: Pins,
) -> Result<ReconciliationPreview> {
    let tenant = scope.tenant();
    let through_statement = match pins.through_statement_number {
        Some(number) => number,
        None => matching::last_statement_number(client, tenant, company_id).await?,
    };
    let through_event = match pins.through_match_event_number {
        Some(number) => number,
        None => matching::last_event_number(client, tenant, company_id).await?,
    };
    let through_opening = match pins.through_opening_id {
        Some(id) => id,
        None => matching::last_opening_id(client, tenant, company_id).await?,
    };
    let totals = ledger
        .account_totals_on(
            client,
            scope,
            company_id,
            &TotalsQuery {
                to: Some(end_date),
                through_entry_number: pins.through_entry_number,
                ..TotalsQuery::default()
            },
        )
        .await?;
    let through_entry = totals.through_entry_number;
    let sides = matching::unmatched(
        ledger,
        client,
        scope,
        company_id,
        account,
        Bounds {
            to: Some(end_date),
            through_entry_number: Some(through_entry),
            through_statement_number: Some(through_statement),
            through_match_event_number: Some(through_event),
            through_opening_id: Some(through_opening),
        },
    )
    .await?;
    let book_balance = totals
        .rows
        .iter()
        .find(|row| row.account.id == account.ledger_account_id)
        .map_or(Money::ZERO, |row| row.debits - row.credits);
    let mut items: Vec<ReconciliationItem> = sides
        .lines
        .iter()
        .map(|item| ReconciliationItem {
            kind: ItemKind::StatementLine,
            statement_line_id: Some(item.line.id),
            entry_id: None,
            match_id: None,
            opening_item_id: None,
            item_date: item.line.line_date,
            description: item.line.description.clone(),
            reference: item.line.reference.clone(),
            amount: item.line.amount,
        })
        .collect();
    items.extend(sides.entries.iter().map(|item| ReconciliationItem {
        kind: ItemKind::Entry,
        statement_line_id: None,
        entry_id: Some(item.entry_id),
        match_id: None,
        opening_item_id: None,
        item_date: item.entry_date,
        description: cut(format!("Entry {}: {}", item.entry_number, item.description)),
        reference: item.reference.clone(),
        amount: item.amount,
    }));
    items.extend(sides.opening_items.iter().map(|item| ReconciliationItem {
        kind: ItemKind::OpeningItem,
        statement_line_id: None,
        entry_id: None,
        match_id: None,
        opening_item_id: Some(item.id),
        item_date: item.item_date,
        description: cut(item.description.clone()),
        reference: Some(item.source_id.chars().take(100).collect()),
        amount: item.amount,
    }));
    items.extend(
        timing(
            client,
            tenant,
            company_id,
            account.id,
            through_event,
            end_date,
        )
        .await?,
    );
    let sum = |kind: ItemKind| -> Money {
        items
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.amount)
            .sum()
    };
    let difference = statement_balance - book_balance - sum(ItemKind::StatementLine)
        + sum(ItemKind::Entry)
        + sum(ItemKind::OpeningItem)
        + sum(ItemKind::Timing);
    Ok(ReconciliationPreview {
        bank_account_id: account.id,
        end_date,
        statement_balance,
        figures: Figures {
            book_balance,
            difference,
            currency_code: totals.currency_code,
            minor_units: totals.minor_units,
            through_entry_number: through_entry,
            through_statement_number: through_statement,
            through_match_event_number: through_event,
            through_opening_id: through_opening,
            items,
        },
    })
}

/// A match standing at `through_event` whose statement side through the end
/// date differs from its ledger side through the end date is a timing
/// difference: money the books and the bank saw on either side of the date.
async fn timing(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    through_event: i64,
    end_date: Date,
) -> Result<Vec<ReconciliationItem>> {
    let matches = matching::standing(
        client,
        tenant,
        company_id,
        bank_account_id,
        Some(through_event),
    )
    .await?;
    let line_ids: Vec<i64> = matches
        .iter()
        .flat_map(|found| found.statement_line_ids.iter().copied())
        .collect();
    let mut lines: HashMap<i64, (Date, Money)> = HashMap::new();
    for row in client
        .query(
            "SELECT id,to_char(line_date,'YYYY-MM-DD'),amount FROM finance_reconciliation_statement_lines \
             WHERE tenant_id = $1 AND company_id = $2 AND id = ANY($3)",
            &[&tenant, &company_id, &line_ids],
        )
        .await
        .map_err(Error::from)?
    {
        lines.insert(row.get(0), (date(&row, 1)?, stored(row.get(2))?));
    }
    let mut items = Vec::new();
    for found in matches {
        let line_sides: Vec<(Date, Money)> = found
            .statement_line_ids
            .iter()
            .filter_map(|id| lines.get(id).copied())
            .collect();
        let statement_side: Money = line_sides
            .iter()
            .filter(|(day, _)| *day <= end_date)
            .map(|(_, amount)| *amount)
            .sum();
        let ledger_side: Money = found
            .entries
            .iter()
            .filter(|entry| entry.entry_date <= end_date)
            .map(|entry| entry.amount)
            .sum();
        let opening_side: Money = found
            .opening_items
            .iter()
            .filter(|item| item.conversion_date <= end_date)
            .map(|item| item.amount)
            .sum();
        let amount = ledger_side + opening_side - statement_side;
        if amount.is_zero() {
            continue;
        }
        let item_date = found
            .entries
            .iter()
            .map(|entry| entry.entry_date)
            .chain(found.opening_items.iter().map(|item| item.conversion_date))
            .chain(line_sides.iter().map(|(day, _)| *day))
            .filter(|day| *day <= end_date)
            .max()
            .unwrap_or(end_date);
        items.push(ReconciliationItem {
            kind: ItemKind::Timing,
            statement_line_id: None,
            entry_id: None,
            match_id: Some(found.id),
            opening_item_id: None,
            item_date,
            description: format!(
                "Match {}: cleared after the statement date",
                found.match_number
            ),
            reference: None,
            amount,
        });
    }
    Ok(items)
}

/// An item's description fits its column (an entry's own may already be 500
/// characters long).
fn cut(text: String) -> String {
    if text.chars().count() > TEXT_LIMIT {
        text.chars().take(TEXT_LIMIT).collect()
    } else {
        text
    }
}

// Opening, finalising, abandoning

/// The end date and statement balance a reconciliation is opened with, or
/// the refusals a form shows beside each field.
pub(crate) fn opening(
    end_date: Option<Date>,
    statement_balance: &str,
    units: MinorUnits,
) -> std::result::Result<(Date, Money), Vec<FieldError>> {
    let mut errors = Vec::new();
    if end_date.is_none() {
        errors.push(FieldError::new("end_date", "can't be blank"));
    }
    let balance = if statement_balance.trim().is_empty() {
        errors.push(FieldError::new("statement_balance", "can't be blank"));
        None
    } else {
        match Money::parse(statement_balance, units) {
            Ok(balance) => Some(balance),
            Err(MoneyError::TooFine(_)) => {
                errors.push(FieldError::new(
                    "statement_balance",
                    "has more decimal places than the book's currency",
                ));
                None
            }
            Err(error) => {
                errors.push(FieldError::new("statement_balance", error.to_string()));
                None
            }
        }
    };
    match (end_date, balance) {
        (Some(end_date), Some(balance)) if errors.is_empty() => Ok((end_date, balance)),
        _ => Err(errors),
    }
}

/// One open reconciliation per bank account, each ending after the last
/// finalised one; the database refuses the same.
pub(crate) async fn check_open(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    end_date: Date,
) -> Result<std::result::Result<(), Vec<FieldError>>> {
    let others = list(client, tenant, company_id, bank_account_id).await?;
    let mut errors = Vec::new();
    if others
        .iter()
        .any(|other| other.status == ReconciliationStatus::Open)
    {
        errors.push(FieldError::new(
            "bank_account_id",
            "already has an open reconciliation",
        ));
    }
    if let Some(last) = others
        .iter()
        .filter(|other| other.status == ReconciliationStatus::Finalised)
        .map(|other| other.end_date)
        .max()
    {
        if end_date <= last {
            errors.push(FieldError::new(
                "end_date",
                format!("must be after the last finalised reconciliation, {last}"),
            ));
        }
    }
    Ok(if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    })
}

/// Records the reconciliation and its `opened` event; the database numbers
/// both. Returns its id.
pub(crate) async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    people: (i64, Option<i64>),
    bank_account_id: i64,
    end_date: Date,
    statement_balance: Money,
) -> Result<i64> {
    let (actor, impersonator) = people;
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        "INSERT INTO finance_reconciliation_reconciliations(tenant_id,company_id,bank_account_id,reconciliation_number,end_date,statement_balance,opened_by_user_id,opened_by_impersonator_id,opened_at) \
                         VALUES($1,$2,$3,NULL,$4::text::date,$5,$6,$7,NULL) RETURNING id,reconciliation_number",
                        &[&tenant, &company_id, &bank_account_id, &end_date.to_string(), &statement_balance.amount(), &actor, &impersonator],
                    )
                    .await?;
                let id: i64 = row.get(0);
                let record = Change {
                    kind: "finance.reconciliation.reconciliation_record".into(),
                    id,
                    old: Value::Null,
                    new: json!({
                        "company_id": company_id, "bank_account_id": bank_account_id,
                        "reconciliation_number": row.get::<_, i64>(1), "end_date": end_date.to_string(),
                        "statement_balance": statement_balance.to_string(),
                        "opened_by_user_id": actor, "opened_by_impersonator_id": impersonator,
                    }),
                };
                let opened = event(sql, tenant, company_id, people, id, "opened", None, None).await?;
                Ok((id, vec![record, opened]))
            })
        })
        .await?)
}

/// Refuses a change to a reconciliation that is no longer open.
pub(crate) fn still_open(reconciliation: &BankReconciliation) -> Result<()> {
    match reconciliation.status {
        ReconciliationStatus::Open => Ok(()),
        ReconciliationStatus::Finalised => Err(ReconciliationError::ReconciliationFinalised),
        ReconciliationStatus::Abandoned => Err(ReconciliationError::ReconciliationAbandoned),
    }
}

/// Records the `finalised` event with the figures and the numbers they were
/// read at, and the items as they stood, numbered from 1.
pub(crate) async fn record_finalised(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    people: (i64, Option<i64>),
    reconciliation_id: i64,
    reason: Option<String>,
    figures: Figures,
) -> Result<()> {
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let mut changes = vec![
                    event(
                        sql,
                        tenant,
                        company_id,
                        people,
                        reconciliation_id,
                        "finalised",
                        reason.as_deref(),
                        Some(&figures),
                    )
                    .await?,
                ];
                for (number, item) in (1..).zip(&figures.items) {
                    let number: i32 = number;
                    let row = sql
                        .query_one(
                            "INSERT INTO finance_reconciliation_reconciliation_items(tenant_id,company_id,reconciliation_id,item_number,kind,statement_line_id,entry_id,match_id,item_date,description,reference,amount,opening_item_id) \
                             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::date,$10,$11,$12,$13) RETURNING id",
                            &[
                                &tenant, &company_id, &reconciliation_id, &number, &item.kind.as_str(),
                                &item.statement_line_id, &item.entry_id, &item.match_id,
                                &item.item_date.to_string(), &item.description, &item.reference,
                                &item.amount.amount(), &item.opening_item_id,
                            ],
                        )
                        .await?;
                    changes.push(Change {
                        kind: "finance.reconciliation.reconciliation_item".into(),
                        id: row.get(0),
                        old: Value::Null,
                        new: json!({
                            "company_id": company_id, "reconciliation_id": reconciliation_id,
                            "item_number": number, "kind": item.kind.as_str(),
                            "statement_line_id": item.statement_line_id, "entry_id": item.entry_id,
                            "match_id": item.match_id, "opening_item_id": item.opening_item_id, "item_date": item.item_date.to_string(),
                            "description": item.description, "reference": item.reference,
                            "amount": item.amount.to_string(),
                        }),
                    });
                }
                Ok(((), changes))
            })
        })
        .await?)
}

/// An abandonment's reason, which is needed.
pub(crate) fn abandon_reason(requested: Option<&str>) -> std::result::Result<String, FieldError> {
    matching::optional_text("reason", requested)?
        .ok_or_else(|| FieldError::new("reason", "is needed to abandon a reconciliation"))
}

/// Records the `abandoned` event with its reason.
pub(crate) async fn record_abandoned(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    people: (i64, Option<i64>),
    reconciliation_id: i64,
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
                    reconciliation_id,
                    "abandoned",
                    Some(&reason),
                    None,
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn event(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    (actor, impersonator): (i64, Option<i64>),
    reconciliation_id: i64,
    action: &str,
    reason: Option<&str>,
    figures: Option<&Figures>,
) -> bilibi_database::Result<Change> {
    let book_balance = figures.map(|f| f.book_balance.amount());
    let difference = figures.map(|f| f.difference.amount());
    let through_entry = figures.map(|f| f.through_entry_number);
    let through_statement = figures.map(|f| f.through_statement_number);
    let through_event = figures.map(|f| f.through_match_event_number);
    let through_opening = figures.map(|f| f.through_opening_id);
    let row = sql
        .query_one(
            "INSERT INTO finance_reconciliation_reconciliation_events(tenant_id,company_id,event_number,reconciliation_id,action,reason,book_balance,difference,through_entry_number,through_statement_number,through_match_event_number,recorded_by_user_id,recorded_by_impersonator_id,recorded_at,through_opening_id) \
             VALUES($1,$2,NULL,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,NULL,$13) RETURNING id,event_number",
            &[
                &tenant, &company_id, &reconciliation_id, &action, &reason, &book_balance, &difference,
                &through_entry, &through_statement, &through_event, &actor, &impersonator, &through_opening,
            ],
        )
        .await?;
    Ok(Change {
        kind: "finance.reconciliation.reconciliation_event".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "company_id": company_id, "reconciliation_id": reconciliation_id,
            "event_number": row.get::<_, i64>(1), "action": action, "reason": reason,
            "book_balance": book_balance.map(|v| v.to_string()),
            "difference": difference.map(|v| v.to_string()),
            "through_entry_number": through_entry, "through_statement_number": through_statement,
            "through_match_event_number": through_event, "through_opening_id": through_opening,
            "recorded_by_user_id": actor, "recorded_by_impersonator_id": impersonator,
        }),
    })
}

// Reading

const COLUMNS: &str =
    "id,bank_account_id,reconciliation_number,to_char(end_date,'YYYY-MM-DD'),statement_balance,\
     opened_by_user_id,opened_by_impersonator_id";

/// One reconciliation of the company with its events, or `None`.
pub(crate) async fn find(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    reconciliation_id: i64,
) -> Result<Option<BankReconciliation>> {
    let rows = client
        .query(
            &format!("SELECT {COLUMNS},to_char(opened_at,{ISO}) FROM finance_reconciliation_reconciliations \
                      WHERE tenant_id = $1 AND company_id = $2 AND id = $3"),
            &[&tenant, &company_id, &reconciliation_id],
        )
        .await
        .map_err(Error::from)?;
    Ok(hydrate(client, tenant, company_id, &rows).await?.pop())
}

/// The bank account's reconciliations by number, with their events.
pub(crate) async fn list(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
) -> Result<Vec<BankReconciliation>> {
    let rows = client
        .query(
            &format!("SELECT {COLUMNS},to_char(opened_at,{ISO}) FROM finance_reconciliation_reconciliations \
                      WHERE tenant_id = $1 AND company_id = $2 AND bank_account_id = $3 ORDER BY reconciliation_number"),
            &[&tenant, &company_id, &bank_account_id],
        )
        .await
        .map_err(Error::from)?;
    hydrate(client, tenant, company_id, &rows).await
}

/// A finalised reconciliation's stored items, by number.
pub(crate) async fn items(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    reconciliation_id: i64,
) -> Result<Vec<ReconciliationItem>> {
    client
        .query(
            "SELECT kind,statement_line_id,entry_id,match_id,to_char(item_date,'YYYY-MM-DD'),description,reference,amount,opening_item_id \
             FROM finance_reconciliation_reconciliation_items \
             WHERE tenant_id = $1 AND company_id = $2 AND reconciliation_id = $3 ORDER BY item_number",
            &[&tenant, &company_id, &reconciliation_id],
        )
        .await
        .map_err(Error::from)?
        .iter()
        .map(|row| {
            Ok(ReconciliationItem {
                kind: ItemKind::parse(row.get(0)).ok_or(Error::Internal)?,
                statement_line_id: row.get(1),
                entry_id: row.get(2),
                match_id: row.get(3),
                opening_item_id: row.get(8),
                item_date: date(row, 4)?,
                description: row.get(5),
                reference: row.get(6),
                amount: stored(row.get(7))?,
            })
        })
        .collect()
}

async fn hydrate(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    rows: &[Row],
) -> Result<Vec<BankReconciliation>> {
    let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
    let mut events: HashMap<i64, Vec<ReconciliationEvent>> = HashMap::new();
    for row in client
        .query(
            &format!("SELECT reconciliation_id,event_number,action,reason,book_balance,difference,\
                      through_entry_number,through_statement_number,through_match_event_number,\
                      recorded_by_user_id,recorded_by_impersonator_id,to_char(recorded_at,{ISO}),through_opening_id \
                      FROM finance_reconciliation_reconciliation_events \
                      WHERE tenant_id = $1 AND company_id = $2 AND reconciliation_id = ANY($3) ORDER BY event_number"),
            &[&tenant, &company_id, &ids],
        )
        .await
        .map_err(Error::from)?
    {
        events.entry(row.get(0)).or_default().push(ReconciliationEvent {
            event_number: row.get(1),
            action: row.get(2),
            reason: row.get(3),
            book_balance: row.get::<_, Option<Decimal>>(4).map(stored).transpose()?,
            difference: row.get::<_, Option<Decimal>>(5).map(stored).transpose()?,
            through_entry_number: row.get(6),
            through_statement_number: row.get(7),
            through_match_event_number: row.get(8),
            through_opening_id: row.get(12),
            recorded_by_user_id: row.get(9),
            recorded_by_impersonator_id: row.get(10),
            recorded_at: timestamp(&row, 11)?,
        });
    }
    rows.iter()
        .map(|row| {
            let id: i64 = row.get(0);
            let events = events.remove(&id).unwrap_or_default();
            let status = if events.iter().any(|e| e.action == "finalised") {
                ReconciliationStatus::Finalised
            } else if events.iter().any(|e| e.action == "abandoned") {
                ReconciliationStatus::Abandoned
            } else {
                ReconciliationStatus::Open
            };
            Ok(BankReconciliation {
                id,
                bank_account_id: row.get(1),
                reconciliation_number: row.get(2),
                end_date: date(row, 3)?,
                statement_balance: stored(row.get(4))?,
                opened_by_user_id: row.get(5),
                opened_by_impersonator_id: row.get(6),
                opened_at: timestamp(row, 7)?,
                status,
                events,
                figures: None,
            })
        })
        .collect()
}
