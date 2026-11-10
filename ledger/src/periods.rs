//! The `finance_ledger_fiscal_years`, `finance_ledger_periods` and
//! `finance_ledger_closing_events` rows (Bilimbi `Ledger.FiscalYear`,
//! `Ledger.Period`, `Ledger.ClosingEvent` and the facade's closing rules).
//! A status is never stored: it is the latest closing event's, and a period
//! or year with none is open. The database numbers and stamps every event and
//! refuses one that breaks the closing rules (see `004-periods.sql`); the
//! facade decides each refusal first, in these words, from [`State`]. The
//! facade authorizes and holds the company lock; these run inside it.
use crate::{
    ClosingAction, ClosingEvent, FieldError, FiscalYear, NewFiscalYear, Period, PeriodStatus,
    YearStatus,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Result};
use jiff::{civil::Date, Timestamp, ToSpan};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;

/// A year runs whole calendar months, from one month to two years.
const MAX_MONTHS: i32 = 24;
const REASON_LIMIT: usize = 500;
const STAMP: &str = "to_char({},'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')";

fn stamp(column: &str) -> String {
    STAMP.replace("{}", column)
}

/// Every fiscal year of the company in date order, each with its periods and
/// the statuses its closing events leave them in.
pub(crate) struct State {
    pub years: Vec<FiscalYear>,
}

impl State {
    pub fn year(&self, id: i64) -> Option<&FiscalYear> {
        self.years.iter().find(|year| year.id == id)
    }
    /// The company's periods in date order: consecutive years' periods are
    /// contiguous, so they follow each other across years.
    pub fn periods(&self) -> impl Iterator<Item = &Period> {
        self.years.iter().flat_map(|year| year.periods.iter())
    }
    pub fn period(&self, id: i64) -> Option<&Period> {
        self.periods().find(|period| period.id == id)
    }
    /// The periods just before and just after the given one.
    pub fn neighbours(&self, id: i64) -> (Option<&Period>, Option<&Period>) {
        let periods: Vec<&Period> = self.periods().collect();
        match periods.iter().position(|period| period.id == id) {
            Some(index) => (
                index.checked_sub(1).map(|before| periods[before]),
                periods.get(index + 1).copied(),
            ),
            None => (None, None),
        }
    }
}

/// The periods a close of `period_id` closes, in date order: the period
/// alone, or with `with_earlier` every open period before it as well, so
/// several months close in one write. The closing rules are the same either
/// way: only an open period closes, in date order, and a year's last period
/// closes only with its year.
pub(crate) fn closing(
    state: &State,
    period_id: i64,
    with_earlier: bool,
) -> std::result::Result<Vec<&Period>, crate::LedgerError> {
    use crate::LedgerError;
    let periods: Vec<&Period> = state.periods().collect();
    let index = periods
        .iter()
        .position(|period| period.id == period_id)
        .ok_or(LedgerError::PeriodNotFound)?;
    if periods[index].status != PeriodStatus::Open {
        return Err(LedgerError::PeriodNotOpen);
    }
    let mut closes: Vec<&Period> = periods[..index]
        .iter()
        .copied()
        .filter(|period| period.status == PeriodStatus::Open)
        .collect();
    if !with_earlier && !closes.is_empty() {
        return Err(LedgerError::EarlierPeriodOpen);
    }
    closes.push(periods[index]);
    for period in &closes {
        if state.year(period.fiscal_year_id).is_some_and(|year| {
            year.end_date == period.end_date && year.status != YearStatus::Closed
        }) {
            return Err(LedgerError::ClosesWithFiscalYear);
        }
    }
    Ok(closes)
}

pub(crate) async fn state(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
) -> Result<State> {
    let events = client
        .query(
            "SELECT fiscal_year_id,period_id,action,entry_id FROM finance_ledger_closing_events \
             WHERE tenant_id=$1 AND company_id=$2 ORDER BY event_number",
            &[&tenant, &company_id],
        )
        .await?;
    let mut period_status: BTreeMap<i64, PeriodStatus> = BTreeMap::new();
    let mut year_event: BTreeMap<i64, (ClosingAction, Option<i64>)> = BTreeMap::new();
    for row in &events {
        let action = ClosingAction::parse(row.get(2)).ok_or(bilibi_database::Error::Internal)?;
        match row.get::<_, Option<i64>>(1) {
            Some(period) => {
                period_status.insert(period, action.status_after());
            }
            None => {
                year_event.insert(row.get(0), (action, row.get(3)));
            }
        }
    }
    let mut periods: BTreeMap<i64, Vec<Period>> = BTreeMap::new();
    for row in client
        .query(
            "SELECT id,fiscal_year_id,period_number,start_date::text,end_date::text \
             FROM finance_ledger_periods WHERE tenant_id=$1 AND company_id=$2 ORDER BY start_date",
            &[&tenant, &company_id],
        )
        .await?
    {
        let id: i64 = row.get(0);
        let fiscal_year_id: i64 = row.get(1);
        periods.entry(fiscal_year_id).or_default().push(Period {
            id,
            fiscal_year_id,
            period_number: row.get(2),
            start_date: date(row.get(3))?,
            end_date: date(row.get(4))?,
            status: period_status
                .get(&id)
                .copied()
                .unwrap_or(PeriodStatus::Open),
        });
    }
    let mut years = Vec::new();
    for row in client
        .query(
            &format!(
                "SELECT id,start_date::text,end_date::text,opened_by_user_id,opened_by_impersonator_id,{} \
                 FROM finance_ledger_fiscal_years WHERE tenant_id=$1 AND company_id=$2 ORDER BY start_date",
                stamp("opened_at")
            ),
            &[&tenant, &company_id],
        )
        .await?
    {
        let id: i64 = row.get(0);
        let (status, closing_entry_id) = match year_event.get(&id) {
            Some((ClosingAction::Close, entry)) => (YearStatus::Closed, *entry),
            _ => (YearStatus::Open, None),
        };
        years.push(FiscalYear {
            id,
            start_date: date(row.get(1))?,
            end_date: date(row.get(2))?,
            opened_by_user_id: row.get(3),
            opened_by_impersonator_id: row.get(4),
            opened_at: instant(row.get(5))?,
            status,
            closing_entry_id,
            periods: periods.remove(&id).unwrap_or_default(),
        });
    }
    Ok(State { years })
}

/// `FiscalYear.changeset/3` after `previous`, the company's latest year: the
/// start defaults to the day after it and must be that day; the end defaults
/// to twelve months on.
pub(crate) fn validate_year(
    previous: Option<&FiscalYear>,
    new: &NewFiscalYear,
) -> std::result::Result<(Date, Date), Vec<FieldError>> {
    let after_previous = previous.and_then(|year| year.end_date.tomorrow().ok());
    let start = new.start_date.or(after_previous);
    let end = new.end_date.or_else(|| {
        start.and_then(|start| {
            start
                .checked_add(12.months())
                .ok()
                .and_then(|end| end.yesterday().ok())
        })
    });
    let mut errors = Vec::new();
    if start.is_none() {
        errors.push(FieldError::new("start_date", "can't be blank"));
    }
    if end.is_none() {
        errors.push(FieldError::new("end_date", "can't be blank"));
    }
    let (Some(start), Some(end)) = (start, end) else {
        return Err(errors);
    };
    if let Some(expected) = after_previous {
        if start != expected {
            errors.push(FieldError::new(
                "start_date",
                format!("must be {expected}, the day after the previous fiscal year ends"),
            ));
        }
    }
    let latest = start.checked_add(MAX_MONTHS.months()).ok();
    if start.day() != 1 {
        errors.push(FieldError::new(
            "start_date",
            "must be the first day of a month",
        ));
    } else if !end.tomorrow().is_ok_and(|next| next.day() == 1) {
        errors.push(FieldError::new(
            "end_date",
            "must be the last day of a month",
        ));
    } else if end <= start || latest.is_none_or(|latest| end >= latest) {
        errors.push(FieldError::new(
            "end_date",
            format!("must end one to {MAX_MONTHS} months after the start"),
        ));
    }
    if errors.is_empty() {
        Ok((start, end))
    } else {
        Err(errors)
    }
}

/// The year's calendar months as `(period_number, start_date, end_date)`.
fn months(start: Date, end: Date) -> Vec<(i16, Date, Date)> {
    let mut months = Vec::new();
    let mut first = start;
    let mut number = 1i16;
    while first <= end {
        months.push((number, first, first.last_of_month()));
        match first.checked_add(1.month()) {
            Ok(next) => first = next,
            Err(_) => break,
        }
        number += 1;
    }
    months
}

/// Inserts the year and its periods, all open, as the audit records them.
/// The database stamps the year and checks, at commit, that its periods
/// reach its end.
pub(crate) async fn insert_year(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    (start, end): (Date, Date),
) -> Result<(i64, Vec<Change>)> {
    // The BEFORE INSERT trigger stamps the row before NOT NULL is checked.
    let row = client
        .query_one(
            &format!(
                "INSERT INTO finance_ledger_fiscal_years(tenant_id,company_id,start_date,end_date,\
                 opened_by_user_id,opened_by_impersonator_id,opened_at) \
                 VALUES($1,$2,$3::text::date,$4::text::date,$5,$6,NULL) RETURNING id,{}",
                stamp("opened_at")
            ),
            &[
                &tenant,
                &company_id,
                &start.to_string(),
                &end.to_string(),
                &actor,
                &impersonator,
            ],
        )
        .await?;
    let year_id: i64 = row.get(0);
    let mut changes = vec![Change {
        kind: "finance.ledger.fiscal_year".into(),
        id: year_id,
        old: Value::Null,
        new: json!({
            "company_id": company_id, "start_date": start.to_string(), "end_date": end.to_string(),
            "opened_by_user_id": actor, "opened_by_impersonator_id": impersonator,
            "opened_at": row.get::<_, String>(1),
        }),
    }];
    for (number, first, last) in months(start, end) {
        let row = client
            .query_one(
                "INSERT INTO finance_ledger_periods(tenant_id,company_id,fiscal_year_id,period_number,start_date,end_date) \
                 VALUES($1,$2,$3,$4,$5::text::date,$6::text::date) RETURNING id",
                &[&tenant, &company_id, &year_id, &number, &first.to_string(), &last.to_string()],
            )
            .await?;
        changes.push(Change {
            kind: "finance.ledger.period".into(),
            id: row.get(0),
            old: Value::Null,
            new: json!({
                "fiscal_year_id": year_id, "period_number": number,
                "start_date": first.to_string(), "end_date": last.to_string(),
            }),
        });
    }
    Ok((year_id, changes))
}

/// One closing event to append: a period's (with `period_id`) or the year's.
pub(crate) struct NewEvent {
    pub fiscal_year_id: i64,
    pub period_id: Option<i64>,
    pub action: ClosingAction,
    pub reason: Option<String>,
    pub entry_id: Option<i64>,
    pub retained_earnings_account_id: Option<i64>,
}

/// Appends a closing event; the database numbers and stamps it and refuses
/// one that breaks the closing rules.
pub(crate) async fn record_event(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    event: &NewEvent,
) -> Result<Change> {
    let row = client
        .query_one(
            &format!(
                "INSERT INTO finance_ledger_closing_events(tenant_id,company_id,event_number,fiscal_year_id,\
                 period_id,action,reason,entry_id,retained_earnings_account_id,recorded_by_user_id,\
                 recorded_by_impersonator_id,recorded_at) \
                 VALUES($1,$2,NULL,$3,$4,$5,$6,$7,$8,$9,$10,NULL) RETURNING id,event_number,{}",
                stamp("recorded_at")
            ),
            &[
                &tenant,
                &company_id,
                &event.fiscal_year_id,
                &event.period_id,
                &event.action.as_str(),
                &event.reason,
                &event.entry_id,
                &event.retained_earnings_account_id,
                &actor,
                &impersonator,
            ],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.closing_event".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "event_number": row.get::<_, i64>(1),
            "fiscal_year_id": event.fiscal_year_id,
            "period_id": event.period_id,
            "action": event.action.as_str(),
            "reason": event.reason,
            "entry_id": event.entry_id,
            "retained_earnings_account_id": event.retained_earnings_account_id,
            "recorded_by_user_id": actor,
            "recorded_by_impersonator_id": impersonator,
            "recorded_at": row.get::<_, String>(2),
        }),
    })
}

/// The year's closing log, oldest first.
pub(crate) async fn events(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    fiscal_year_id: i64,
) -> Result<Vec<ClosingEvent>> {
    let rows = client
        .query(
            &format!(
                "SELECT event_number,fiscal_year_id,period_id,action,reason,entry_id,\
                 retained_earnings_account_id,recorded_by_user_id,recorded_by_impersonator_id,{} \
                 FROM finance_ledger_closing_events WHERE tenant_id=$1 AND company_id=$2 AND fiscal_year_id=$3 \
                 ORDER BY event_number",
                stamp("recorded_at")
            ),
            &[&tenant, &company_id, &fiscal_year_id],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ClosingEvent {
                event_number: row.get(0),
                fiscal_year_id: row.get(1),
                period_id: row.get(2),
                action: ClosingAction::parse(row.get(3)).ok_or(bilibi_database::Error::Internal)?,
                reason: row.get(4),
                entry_id: row.get(5),
                retained_earnings_account_id: row.get(6),
                recorded_by_user_id: row.get(7),
                recorded_by_impersonator_id: row.get(8),
                recorded_at: instant(row.get(9))?,
            })
        })
        .collect()
}

/// `ClosingEvent.changeset/2`'s `reason`: trimmed, blank as none, at most
/// 500 characters, and required to reopen.
pub(crate) fn reason(
    text: Option<&str>,
    required: Option<&'static str>,
) -> std::result::Result<Option<String>, Vec<FieldError>> {
    let reason = text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    match (&reason, required) {
        (None, Some(message)) => Err(vec![FieldError::new("reason", message)]),
        (Some(text), _) if text.chars().count() > REASON_LIMIT => Err(vec![FieldError::new(
            "reason",
            format!("should be at most {REASON_LIMIT} character(s)"),
        )]),
        _ => Ok(reason),
    }
}

/// Why an entry may not be dated `entry_date` (Bilimbi
/// `validate_open_period/3`): into a closed or locked period, before the
/// first fiscal year, or, for a controlled company, where no period exists.
pub(crate) async fn date_refusal(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    entry_date: Date,
) -> Result<Option<String>> {
    let day = entry_date.to_string();
    let row = client
        .query_opt(
            "SELECT (SELECT finance_ledger_period_status(id) FROM finance_ledger_periods \
                     WHERE tenant_id=$1 AND company_id=$2 AND $3::text::date BETWEEN start_date AND end_date), \
                    EXISTS (SELECT 1 FROM finance_ledger_periods WHERE tenant_id=$1 AND company_id=$2 \
                     AND start_date > $3::text::date), \
                    finance_ledger_company_controlled($2)",
            &[&tenant, &company_id, &day],
        )
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    let status: Option<String> = row.get(0);
    let earlier: bool = row.get(1);
    let controlled: bool = row.get(2);
    Ok(match status {
        Some(status) if status == "open" => None,
        Some(status) => Some(format!("falls in a {status} period")),
        None if earlier => Some("is before the first fiscal year".into()),
        None if controlled => {
            Some("falls in no fiscal period: open the fiscal year that covers it first".into())
        }
        None => None,
    })
}

/// Each income and expense account's net (debits less credits) over the
/// year's dates, by code, leaving out the accounts at zero. Earlier closing
/// entries and their reversals cancel out.
pub(crate) async fn closing_balances(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    year: &FiscalYear,
) -> Result<Vec<(i64, Decimal)>> {
    let rows = client
        .query(
            "SELECT a.id, sum(l.debit) - sum(l.credit) FROM finance_ledger_entry_lines l \
             JOIN finance_ledger_entries e ON e.id = l.entry_id \
             JOIN finance_ledger_accounts a ON a.id = l.account_id \
             WHERE l.tenant_id=$1 AND l.company_id=$2 AND a.account_type IN ('income','expense') \
             AND e.entry_date BETWEEN $3::text::date AND $4::text::date \
             GROUP BY a.id, a.code ORDER BY a.code",
            &[
                &tenant,
                &company_id,
                &year.start_date.to_string(),
                &year.end_date.to_string(),
            ],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, Decimal>(1)))
        .filter(|(_, net)| !net.is_zero())
        .collect())
}

/// The SQL condition that leaves out year-end closing entries and their
/// reversals, for an entries query aliased `e` whose company is `$2`. A
/// year's close names its closing entry in the transaction that posts it, so
/// the set is the same at any `through_entry_number`.
pub(crate) const NOT_CLOSING: &str = "e.id NOT IN (SELECT entry_id FROM finance_ledger_closing_events \
     WHERE company_id=$2 AND entry_id IS NOT NULL) \
     AND (e.reverses_entry_id IS NULL OR e.reverses_entry_id NOT IN \
     (SELECT entry_id FROM finance_ledger_closing_events WHERE company_id=$2 AND entry_id IS NOT NULL))";

/// Whether the entry is a year-end closing entry, which only reopening its
/// fiscal year reverses.
pub(crate) async fn closing_entry(
    client: &dyn OperationClient,
    company_id: i64,
    entry_id: i64,
) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM finance_ledger_closing_events WHERE company_id=$1 AND entry_id=$2",
            &[&company_id, &entry_id],
        )
        .await?
        .is_some())
}

fn date(text: String) -> Result<Date> {
    text.parse().map_err(|_| bilibi_database::Error::Internal)
}

fn instant(text: String) -> Result<Timestamp> {
    text.parse().map_err(|_| bilibi_database::Error::Internal)
}
