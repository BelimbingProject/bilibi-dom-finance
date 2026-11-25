//! Due periods for Finance home, following SST returns: a finalised return is done;
//! a period still running is not due yet, and a period before the registration
//! was kept here is not an unprepared return.
use crate::{Result, ReturnStatus, Sst, TaxType};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, tz::TimeZone, Timestamp};
use serde::Serialize;

/// A taxable period with a return still to prepare or finalise.
#[derive(Clone, Debug, Serialize)]
pub struct DuePeriod {
    pub period_start: String,
    pub period_end: String,
    pub due_date: String,
    /// An older open return has no period row on the returns page.
    pub return_id: Option<i64>,
}

impl Sst {
    /// Recent ended taxable periods still due and older open returns, in the company's calendar zone.
    pub async fn due_periods(
        &self,
        scope: &Scope,
        company: i64,
        today: Date,
        timezone: &str,
    ) -> Result<Vec<DuePeriod>> {
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        self.due_periods_on(&**client, scope, company, today, timezone)
            .await
    }

    /// Authorized due periods on the caller's connection.
    pub async fn due_periods_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
        today: Date,
        timezone: &str,
    ) -> Result<Vec<DuePeriod>> {
        let registrations = self.list_registrations_on(client, scope, company).await?;
        let Some(latest) = registrations.last() else {
            return Ok(Vec::new());
        };
        let zone = TimeZone::get(timezone).unwrap_or(TimeZone::UTC);
        let kept_from = registrations
            .first()
            .and_then(|r| r.recorded_at.parse::<Timestamp>().ok())
            .map(|at| at.to_zoned(zone).date());
        let periods = self
            .taxable_periods_on(client, scope, company, Some(today), Some(6))
            .await?;
        let Some(oldest) = periods.last() else {
            return Ok(Vec::new());
        };
        let oldest_start = oldest.period_start;
        let start = oldest_start.to_string();
        let returns = crate::returns::read_returns(
            client,
            scope.tenant(),
            company,
            "AND (r.period_start >= $3::text::date OR NOT EXISTS (SELECT 1 FROM finance_sst_return_events e WHERE e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.return_id=r.id AND e.action IN ('finalised','discarded'))) ORDER BY r.id",
            &[&start],
        )
        .await?;
        let taxes: Vec<_> = TaxType::ALL
            .into_iter()
            .filter(|tax| latest.number(*tax).is_some())
            .collect();
        let mut due: Vec<DuePeriod> = periods
            .into_iter()
            .filter(|p| p.period_end < today)
            .filter(|p| {
                let kept = kept_from.is_none_or(|from| p.due_date >= from);
                taxes.iter().any(|tax| {
                    let group: Vec<_> = returns
                        .iter()
                        .filter(|r| {
                            r.tax_type == *tax
                                && r.period_start == p.period_start
                                && r.status != ReturnStatus::Discarded
                        })
                        .collect();
                    let standing = group
                        .iter()
                        .find(|r| r.status == ReturnStatus::Open)
                        .or_else(|| group.iter().max_by_key(|r| r.version));
                    standing.map_or(kept, |r| r.status == ReturnStatus::Open)
                })
            })
            .map(|p| DuePeriod {
                period_start: p.period_start.to_string(),
                period_end: p.period_end.to_string(),
                due_date: p.due_date.to_string(),
                return_id: None,
            })
            .collect();
        for r in returns
            .iter()
            .filter(|r| r.status == ReturnStatus::Open && r.period_start < oldest_start)
        {
            let start = r.period_start.to_string();
            if !due.iter().any(|p| p.period_start == start) {
                due.push(DuePeriod {
                    period_start: start,
                    period_end: r.period_end.to_string(),
                    due_date: r.due_date.to_string(),
                    return_id: Some(r.id),
                });
            }
        }
        due.sort_by(|a, b| a.due_date.cmp(&b.due_date));
        Ok(due)
    }
}
