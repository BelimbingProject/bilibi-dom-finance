//! The year-to-date bridge: the retained monthly source movements of each
//! income and expense account, added up from the conversion fiscal year's
//! start to cut-off, beside that account's line in the approved opening
//! journal. The opening journal posts year-to-date balances on the
//! conversion date, so the two should agree; a difference is shown, never
//! netted away, and the accountant explains it. The bridge reads retained
//! evidence and the ledger's facade only: it posts, corrects and certifies
//! nothing.
use super::{authorise, company};
use crate::{MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    DEFINITIONS,
};
use bilibi_database::Error;
use bilibi_finance_ledger::{AccountType, LedgerError, MinorUnits};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) const BRIDGE: &str = "finance.reports.source_period.bridge";

/// The amount at the book's decimal places, as a contract writes it.
fn shown(amount: Decimal, units: MinorUnits) -> String {
    let mut amount = amount;
    amount.rescale(u32::from(units.get()));
    amount.to_string()
}

/// A ledger refusal as the contract reports it.
fn refusal(error: LedgerError) -> bilibi_agent_api::Error {
    match error {
        LedgerError::Unauthorized => bilibi_agent_api::Error::Forbidden,
        LedgerError::CompanyNotFound => bilibi_agent_api::Error::NotFound,
        LedgerError::Failed(error) => error.into(),
        _ => Error::Internal.into(),
    }
}

/// One source month of the conversion fiscal year, ending at cut-off when
/// conversion falls inside it (the import's own month rule).
struct Month {
    start: Date,
    end: Date,
    schedules: Vec<Value>,
}

fn months(year_start: Date, conversion: Date) -> Result<Vec<Month>, jiff::Error> {
    let mut first = year_start.first_of_month();
    if first < year_start {
        first = year_start.last_of_month().tomorrow()?;
    }
    let last_source_day = conversion.yesterday()?;
    let mut months = Vec::new();
    while first < conversion {
        months.push(Month {
            start: first,
            end: first.last_of_month().min(last_source_day),
            schedules: Vec::new(),
        });
        first = first.last_of_month().tomorrow()?;
    }
    Ok(months)
}

async fn bridge(
    owner: &crate::Reports,
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
) -> bilibi_agent_api::Result<Value> {
    let ledger = &owner.ledger;
    let book = match ledger.get_book_on(client, scope, company).await {
        Ok(book) => book,
        Err(LedgerError::BookNotOpen) => {
            return Ok(json!({"company_id":company,"state":"no_book"}))
        }
        Err(error) => return Err(refusal(error)),
    };
    let opening = match ledger.opening_context_on(client, scope, company).await {
        Ok(opening) => opening,
        Err(LedgerError::Invalid(_)) => {
            return Ok(json!({"company_id":company,"state":"no_opening"}))
        }
        Err(error) => return Err(refusal(error)),
    };
    let conversion = opening.entry.entry_date;
    let years = ledger
        .list_fiscal_years_on(client, scope, company)
        .await
        .map_err(refusal)?;
    let Some(year) = years
        .iter()
        .find(|y| y.start_date <= conversion && conversion <= y.end_date)
    else {
        return Ok(json!({"company_id":company,"state":"no_fiscal_year"}));
    };
    let accounts = ledger
        .list_accounts_on(client, scope, company)
        .await
        .map_err(refusal)?;
    let mut months = months(year.start_date, conversion).map_err(|_| Error::Internal)?;
    // One row per retained schedule and account: the debit-positive net of
    // its rows, summed by PostgreSQL. Every movement schedule of the
    // conversion fiscal year before cut-off is read, except one withdrawn or
    // replaced (its replacement counts instead); one whose period is not
    // a source month (an earlier cut-off) is listed apart and not added.
    let rows = client
        .query(
            "SELECT s.id, s.source_reference, s.period_start::text, s.period_end::text, \
             (r->>'account_id')::bigint, \
             sum(CASE WHEN r->>'side' = 'debit' THEN (r->>'amount')::numeric ELSE -(r->>'amount')::numeric END) \
             FROM finance_reports_source_periods s CROSS JOIN LATERAL jsonb_array_elements(s.facts) r \
             WHERE s.tenant_id = $1 AND s.company_id = $2 AND s.kind = 'movement' \
             AND s.period_start >= $3::text::date AND s.period_end < $4::text::date \
             AND NOT EXISTS (SELECT 1 FROM finance_reports_source_period_corrections c \
                 WHERE c.tenant_id = s.tenant_id AND c.company_id = s.company_id AND c.schedule_id = s.id) \
             GROUP BY s.id, s.source_reference, s.period_start, s.period_end, 5 \
             ORDER BY s.period_start, s.id, 5",
            &[
                &scope.tenant(),
                &company,
                &year.start_date.to_string(),
                &conversion.to_string(),
            ],
        )
        .await?;
    let mut source: BTreeMap<i64, Decimal> = BTreeMap::new();
    let mut other_cut_off: Vec<Value> = Vec::new();
    for row in &rows {
        let id: i64 = row.get(0);
        let date = |i: usize| row.get::<_, String>(i).parse::<Date>();
        let (start, end) = (
            date(2).map_err(|_| Error::Internal)?,
            date(3).map_err(|_| Error::Internal)?,
        );
        let schedule = json!({"id":id,"source_reference":row.get::<_, String>(1),"period_start":start.to_string(),"period_end":end.to_string()});
        let Some(month) = months.iter_mut().find(|m| m.start == start && m.end == end) else {
            if !other_cut_off.iter().any(|s| s["id"] == id) {
                other_cut_off.push(schedule);
            }
            continue;
        };
        if !month.schedules.iter().any(|s| s["id"] == id) {
            month.schedules.push(schedule);
        }
        let account: Option<i64> = row.get(4);
        let amount: Decimal = row.get(5);
        *source.entry(account.unwrap_or_default()).or_default() += amount;
    }
    let units = book.minor_units;
    let mut bridged = Vec::new();
    let mut not_bridged = Vec::new();
    let (mut source_total, mut opening_total) = (Decimal::ZERO, Decimal::ZERO);
    let mut differences = 0;
    let mut chart: Vec<_> = accounts.iter().filter(|a| !a.is_group).collect();
    chart.sort_by(|a, b| a.code.cmp(&b.code));
    for account in &chart {
        let from_source = source.remove(&account.id).unwrap_or_default();
        let flow = matches!(
            account.account_type,
            AccountType::Income | AccountType::Expense
        );
        if !flow {
            if !from_source.is_zero() {
                not_bridged.push(json!({"account_id":account.id,"code":account.code,"name":account.name,"account_type":account.account_type.as_str(),"source":shown(from_source,units)}));
            }
            continue;
        }
        let opened = opening
            .balances
            .get(&account.id)
            .map(|m| m.amount())
            .unwrap_or_default();
        if from_source.is_zero() && opened.is_zero() {
            continue;
        }
        let difference = from_source - opened;
        if !difference.is_zero() {
            differences += 1;
        }
        source_total += from_source;
        opening_total += opened;
        bridged.push(json!({"account_id":account.id,"code":account.code,"name":account.name,"account_type":account.account_type.as_str(),"source":shown(from_source,units),"opening":shown(opened,units),"difference":shown(difference,units)}));
    }
    // A row naming an account the chart no longer lists stays visible.
    for (account, amount) in source {
        not_bridged.push(json!({"account_id":account,"code":"","name":"","account_type":"","source":shown(amount,units)}));
    }
    let missing = months.iter().filter(|m| m.schedules.is_empty()).count();
    let repeated = months.iter().filter(|m| m.schedules.len() > 1).count();
    let months: Vec<Value> = months
        .into_iter()
        .map(|m| {
            let status = match m.schedules.len() {
                0 => "missing",
                1 => "retained",
                _ => "more_than_one",
            };
            json!({"month":m.start.strftime("%Y-%m").to_string(),"period_start":m.start.to_string(),"period_end":m.end.to_string(),"status":status,"schedules":m.schedules})
        })
        .collect();
    Ok(json!({
        "company_id": company,
        "state": "ready",
        "currency_code": book.currency_code,
        "minor_units": units.get(),
        "conversion_date": conversion.to_string(),
        "opening_entry_id": opening.entry.id,
        "fiscal_year": {"start_date": year.start_date.to_string(), "end_date": year.end_date.to_string()},
        "months": months,
        "accounts": bridged,
        "not_bridged": not_bridged,
        "other_cut_off": other_cut_off,
        "totals": {"source": shown(source_total, units), "opening": shown(opening_total, units), "difference": shown(source_total - opening_total, units)},
        "missing_months": missing,
        "repeated_months": repeated,
        "differences": differences,
        "agrees": missing == 0 && repeated == 0 && differences == 0 && other_cut_off.is_empty(),
        "notice": "Amounts are debit-positive. Agreement is arithmetic, not a review: the accountant explains every difference and signs off the conversion.",
    }))
}

struct Bridge;
impl ReadHandler for Bridge {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = company(input)?;
            authorise(client, scope, company, VIEW).await?;
            let owner = crate::agent_operations::OWNER
                .get()
                .ok_or(Error::Internal)?;
            Ok(bridge(owner, client, scope, company).await?.into())
        })
    }
}
fn input() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/reports.source-period-bridge",Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions {
    operations: &[
        Operation {key:BRIDGE,title:"Bridge monthly movements to the opening balances",summary:"Add up each income and expense account's retained monthly source movements from the conversion fiscal year's start to cut-off and compare them with its opening journal line. Shows missing and repeated months and every difference; it posts and certifies nothing.",keywords:&["conversion","history","year to date","reconciliation"],capability:VIEW,input,output,handler:Handler::Read(&Bridge),channels:Channels::Every,step_up:&[]},
    ], guides:&[],
}) }
