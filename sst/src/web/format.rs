//! Bilimbi Finance.Sst.Web.Format: amounts, names and refusals as the SST
//! pages show them.
use super::render;
use crate::{ReturnStatus, Sst, SstError, TaxReturn, TaxSource, TaxType};
use askama::Template;
use bilibi_database::Result;
use bilibi_finance_ledger::dates;
use bilibi_ui::{Badge, BadgeKind, Html};
use jiff::civil::Date;
use rust_decimal::Decimal;

/// A rate as `Decimal.to_string(:normal)` writes it.
pub(super) fn rate(rate: Decimal) -> String {
    rate.normalize().to_string()
}

pub(super) fn tax_name(tax_type: TaxType) -> &'static str {
    match tax_type {
        TaxType::SalesTax => "Sales tax",
        TaxType::ServiceTax => "Service tax",
    }
}

pub(super) fn basis_name(basis: Option<&str>) -> &'static str {
    match basis {
        Some("payment") => "when payment is received",
        Some("invoice") => "when invoiced (approved accrual basis)",
        _ => "—",
    }
}

pub(super) fn source_name(source: TaxSource) -> &'static str {
    match source {
        TaxSource::Receivables => "Receivables",
        TaxSource::Payables => "Payables",
    }
}

pub(super) fn kind_name(kind: &str) -> &str {
    match kind {
        "invoice" => "Invoices",
        "credit_note" => "Credit notes",
        "bill" => "Bills",
        "debit_note" => "Debit notes",
        other => other,
    }
}

pub(super) fn measure_name(measure: &str) -> &str {
    match measure {
        "documents" => "by document date",
        "received" => "payments received",
        "unpaid" => "unpaid twelve months",
        "credited_after_due" => "credited after falling due",
        other => other,
    }
}

/// A box as `item label` from its tax's form, or the key when no form holds it.
pub(super) fn box_name(tax_type: TaxType, key: Option<&str>) -> String {
    let Some(key) = key else {
        return "Not reported".into();
    };
    Sst::boxes(tax_type)
        .iter()
        .find(|b| b.key == key)
        .map_or_else(|| key.to_owned(), |b| format!("{} {}", b.item, b.label))
}

/// A mapped box named with the tax its form is for.
pub(super) fn mapped_box_name(key: Option<&str>) -> String {
    let Some(key) = key else {
        return "Not reported".into();
    };
    TaxType::ALL
        .into_iter()
        .find_map(|tax_type| {
            Sst::boxes(tax_type)
                .iter()
                .find(|b| b.key == key)
                .map(|b| format!("{} {} {}", tax_name(tax_type), b.item, b.label))
        })
        .unwrap_or_else(|| key.to_owned())
}

pub(super) fn status_kind(status: ReturnStatus) -> BadgeKind {
    match status {
        ReturnStatus::Open => BadgeKind::Warning,
        ReturnStatus::Finalised => BadgeKind::Success,
        ReturnStatus::Discarded => BadgeKind::Neutral,
    }
}

/// ReturnsLive's status label: `finalised v2`, `finalised v1, amended`.
pub(super) fn status_label(r: &TaxReturn) -> String {
    match (r.status, r.version) {
        (ReturnStatus::Finalised, Some(v)) if r.superseded_by_return_id.is_none() => {
            format!("finalised v{v}")
        }
        (ReturnStatus::Finalised, Some(v)) => format!("finalised v{v}, amended"),
        (status, _) => status.as_str().into(),
    }
}

pub(super) fn badge(text: &str, kind: BadgeKind, class: &str) -> Result<Html> {
    render(Badge {
        kind,
        dot: true,
        class,
        body: Html::text(text),
    })
}

/// The sentence a refusal from the facade reads as. A database failure may
/// carry SQL or internals, so it is never shown.
pub(super) fn refusal(error: &SstError) -> String {
    match error {
        SstError::Failed(_) => "Nothing was changed. Reload the page and try again.".into(),
        other => other.to_string(),
    }
}

/// A taxable period as pages name it: "September to October 2026" when it
/// is whole months, its two dates otherwise (a first period that starts on
/// the day of registration).
pub(super) fn period(start: Date, end: Date) -> String {
    if start.day() == 1 && end == end.last_of_month() {
        dates::months(start, end)
    } else {
        format!("{} to {}", dates::display(start), dates::display(end))
    }
}

/// The first day a period's return can be finalised: the day after it ends.
pub(super) fn finalise_from(period_end: Date) -> String {
    dates::display(period_end.tomorrow().unwrap_or(period_end))
}

/// The day of an SST stamp (`YYYY-MM-DDTHH:MM:SS.ffffffZ`) in the zone due
/// dates are counted in.
pub(super) fn day(at: &str, timezone: &str) -> Option<Date> {
    let at: jiff::Timestamp = at.parse().ok()?;
    let zone = jiff::tz::TimeZone::get(timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    Some(at.to_zoned(zone).date())
}

/// A return counts as due soon from this many days before its due date.
const DUE_SOON_DAYS: i64 = 14;

fn days(n: i64) -> String {
    if n == 1 {
        "1 day".into()
    } else {
        format!("{n} days")
    }
}

/// A due date as a row shows it. While the return is still `owed` (not yet
/// finalised) it says how far off the date is: "30 Nov 2026 · in 20 days",
/// a warning from two weeks before, and "Overdue by 3 days" after it.
pub(super) fn due(due_date: Date, today: Date, owed: bool) -> Result<Html> {
    let date = dates::display(due_date);
    if !owed {
        return Ok(Html::text(&date));
    }
    let left = i64::from((due_date - today).get_days());
    let (text, flag) = match left {
        n if n < 0 => (
            String::new(),
            Some((format!("Overdue by {}", days(-n)), BadgeKind::Danger)),
        ),
        0 => (
            String::new(),
            Some(("Due today".into(), BadgeKind::Warning)),
        ),
        n if n <= DUE_SOON_DAYS => (
            String::new(),
            Some((format!("Due in {}", days(n)), BadgeKind::Warning)),
        ),
        n => (format!("in {}", days(n)), None),
    };
    let flag = flag
        .map(|(text, kind)| badge(&text, kind, "whitespace-nowrap"))
        .transpose()?;
    render(Due { date, text, flag })
}

#[derive(Template)]
#[template(
    source = r#"<span class="whitespace-nowrap">{{ date }}</span>{% if !text.is_empty() %}<span class="whitespace-nowrap text-ink-muted"> · {{ text }}</span>{% endif %}{% if let Some(flag) = flag %} {{ flag|safe }}{% endif %}"#,
    ext = "html"
)]
struct Due {
    date: String,
    text: String,
    flag: Option<Html>,
}
