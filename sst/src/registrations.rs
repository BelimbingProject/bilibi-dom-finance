//! Bilimbi.Finance.Sst.Registrations: a company's SST registration as
//! numbered versions, never changed. A call records the next version from the
//! latest one and what it gives, carrying over whatever it leaves out; a call
//! that changes nothing records nothing.
use crate::periods::Cycle;
use crate::store::{self, Table};
use crate::*;
use serde_json::json;

/// One version of the registration: its registration numbers (at least one,
/// each shaped `A00-0000-00000000`), the cycle its taxable periods follow and
/// when service tax is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub id: i64,
    pub version: i64,
    pub sales_tax_number: Option<String>,
    pub service_tax_number: Option<String>,
    pub period_months: i32,
    pub period_start: Date,
    pub service_tax_basis: String,
    pub reason: Option<String>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}
impl Registration {
    pub(crate) fn cycle(&self) -> Cycle {
        Cycle {
            first: self.period_start,
            months: self.period_months,
        }
    }
    /// The registration number for `tax_type`, if the company holds one.
    pub fn number(&self, tax_type: TaxType) -> Option<&str> {
        match tax_type {
            TaxType::SalesTax => self.sales_tax_number.as_deref(),
            TaxType::ServiceTax => self.service_tax_number.as_deref(),
        }
    }
}

/// What records the next version. `None` carries the latest version's value
/// over; a blank registration number removes it. `period_months` is 1 to 12
/// (2 before any version), `service_tax_basis` is `payment` (the default) or
/// `invoice`, and the cycle and the basis change only from a `period_start`
/// after the last finalised return's period.
#[derive(Clone, Debug, Default)]
pub struct RegistrationInput {
    pub sales_tax_number: Option<String>,
    pub service_tax_number: Option<String>,
    pub period_months: Option<i64>,
    pub period_start: Option<Date>,
    pub service_tax_basis: Option<String>,
    pub reason: Option<String>,
}

const NUMBER_SHAPE: &str = "is not shaped like A00-0000-00000000";

/// A number as the department issues it: a station letter and two digits,
/// the year and month of registration, and an eight-digit serial.
fn shaped(number: &str) -> bool {
    let b = number.as_bytes();
    b.len() == 17
        && b[0].is_ascii_uppercase()
        && b[3] == b'-'
        && b[8] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 0 | 3 | 8) || c.is_ascii_digit())
}

const COLUMNS: &str = "id,version,sales_tax_number,service_tax_number,period_months::integer,period_start::text,service_tax_basis,reason,recorded_by_user_id,recorded_by_impersonator_id";

fn row(r: &tokio_postgres::Row) -> bilibi_database::Result<Registration> {
    Ok(Registration {
        id: r.get(0),
        version: r.get(1),
        sales_tax_number: r.get(2),
        service_tax_number: r.get(3),
        period_months: r.get(4),
        period_start: store::date(r.get(5))?,
        service_tax_basis: r.get(6),
        reason: r.get(7),
        recorded_by_user_id: r.get(8),
        recorded_by_impersonator_id: r.get(9),
        recorded_at: r.get(10),
    })
}

async fn read(
    c: &(impl GenericClient + Sync),
    filter: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> bilibi_database::Result<Vec<Registration>> {
    let sql = format!(
        "SELECT {COLUMNS},{} FROM finance_sst_registrations WHERE tenant_id=$1 AND company_id=$2 {filter}",
        store::stamp("recorded_at")
    );
    c.query(&sql, params).await?.iter().map(row).collect()
}

/// The latest version.
pub(crate) async fn latest(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
) -> bilibi_database::Result<Option<Registration>> {
    Ok(
        read(c, "ORDER BY version DESC LIMIT 1", &[&tenant, &company])
            .await?
            .pop(),
    )
}

/// One version by id: the one a return was prepared under.
pub(crate) async fn get(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    id: i64,
) -> bilibi_database::Result<Option<Registration>> {
    Ok(read(c, "AND id=$3", &[&tenant, &company, &id]).await?.pop())
}

/// The latest version whose cycle starts on or before `date`.
pub(crate) async fn governing(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    date: Date,
) -> bilibi_database::Result<Option<Registration>> {
    Ok(read(
        c,
        "AND period_start<=$3::text::date ORDER BY version DESC LIMIT 1",
        &[&tenant, &company, &date.to_string()],
    )
    .await?
    .pop())
}

/// The end of the latest period any finalised return reported.
async fn reported_through(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
) -> bilibi_database::Result<Option<Date>> {
    let r = c
        .query_one(
            "SELECT max(r.period_end)::text FROM finance_sst_returns r JOIN finance_sst_return_events e ON e.return_id=r.id AND e.company_id=r.company_id AND e.action='finalised' WHERE r.tenant_id=$1 AND r.company_id=$2",
            &[&tenant, &company],
        )
        .await?;
    r.get::<_, Option<&str>>(0).map(store::date).transpose()
}

impl Sst {
    /// The current registration, the latest version, or `None` before any.
    pub async fn get_registration(&self, s: &Scope, company: i64) -> Result<Option<Registration>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        Ok(latest(&**c, s.tenant(), company).await?)
    }

    /// Every version of the registration, oldest first: its history.
    pub async fn list_registrations(&self, s: &Scope, company: i64) -> Result<Vec<Registration>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        Ok(read(&**c, "ORDER BY version", &[&s.tenant(), &company]).await?)
    }

    /// Records the next version of the registration from the current one and
    /// `input` (`finance.sst.settings.manage`); returns the current version
    /// unchanged when `input` changes nothing. Refusals are `Invalid` with
    /// Bilimbi's field messages.
    pub async fn register(
        &self,
        s: &Scope,
        company: i64,
        input: &RegistrationInput,
    ) -> Result<Registration> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(&mut c, s, company, MANAGE_SETTINGS, "finance.sst.register")
            .await?;
        let tenant = s.tenant();
        let current = latest(tx.sql(), tenant, company).await?;
        let mut errors = Vec::new();
        let number = |given: &Option<String>, carried: Option<&String>| match given {
            Some(text) => bilibi_ui::params::blank_to_none(Some(text)).map(|t| t.to_uppercase()),
            None => carried.cloned(),
        };
        let sales_tax_number = number(
            &input.sales_tax_number,
            current.as_ref().and_then(|r| r.sales_tax_number.as_ref()),
        );
        let service_tax_number = number(
            &input.service_tax_number,
            current.as_ref().and_then(|r| r.service_tax_number.as_ref()),
        );
        let period_months = input
            .period_months
            .unwrap_or_else(|| current.as_ref().map_or(2, |r| i64::from(r.period_months)));
        let period_start = input
            .period_start
            .or_else(|| current.as_ref().map(|r| r.period_start));
        let service_tax_basis = input.service_tax_basis.clone().unwrap_or_else(|| {
            current
                .as_ref()
                .map_or_else(|| "payment".into(), |r| r.service_tax_basis.clone())
        });
        let reason = reason(input.reason.as_deref(), &mut errors);
        if period_start.is_none() {
            errors.push(FieldError::new("period_start", "can't be blank"));
        }
        if service_tax_basis.is_empty() {
            errors.push(FieldError::new("service_tax_basis", "can't be blank"));
        }
        for (field, value) in [
            ("sales_tax_number", &sales_tax_number),
            ("service_tax_number", &service_tax_number),
        ] {
            if value.as_deref().is_some_and(|n| !shaped(n)) {
                errors.push(FieldError::new(field, NUMBER_SHAPE));
            }
        }
        if period_months < 1 {
            errors.push(FieldError::new(
                "period_months",
                "must be greater than or equal to 1",
            ));
        } else if period_months > 12 {
            errors.push(FieldError::new(
                "period_months",
                "must be less than or equal to 12",
            ));
        }
        if !service_tax_basis.is_empty() && !SERVICE_TAX_BASES.contains(&service_tax_basis.as_str())
        {
            errors.push(FieldError::new("service_tax_basis", "is invalid"));
        }
        if sales_tax_number.is_none() && service_tax_number.is_none() {
            errors.push(FieldError::new(
                "sales_tax_number",
                "or a service tax number is needed",
            ));
        }
        if let (Some(latest), Some(start)) = (&current, period_start) {
            let changed = i64::from(latest.period_months) != period_months
                || latest.period_start != start
                || latest.service_tax_basis != service_tax_basis;
            if let Some(through) = reported_through(tx.sql(), tenant, company).await? {
                if changed && start <= through {
                    errors.push(FieldError::new("period_start", format!("must fall after {through}, the end of the last finalised return's period, to change the cycle or basis")));
                }
            }
        }
        if !errors.is_empty() {
            return Err(SstError::Invalid(errors));
        }
        let period_start = period_start.ok_or(Error::Invalid)?;
        if let Some(latest) = current.filter(|r| {
            r.sales_tax_number == sales_tax_number
                && r.service_tax_number == service_tax_number
                && i64::from(r.period_months) == period_months
                && r.period_start == period_start
                && r.service_tax_basis == service_tax_basis
        }) {
            return Ok(latest);
        }
        let mut value = store::identity(s, company);
        value.as_object_mut().ok_or(Error::Invalid)?.extend(
            json!({
                "sales_tax_number": sales_tax_number,
                "service_tax_number": service_tax_number,
                "period_months": period_months,
                "period_start": period_start.to_string(),
                "service_tax_basis": service_tax_basis,
                "reason": reason,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (id, change) = store::insert(sql, Table::Registration, value).await?;
                    Ok((id, vec![change]))
                })
            })
            .await?;
        let registration = read(tx.sql(), "AND id=$3", &[&tenant, &company, &id])
            .await?
            .pop()
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(registration)
    }
}
