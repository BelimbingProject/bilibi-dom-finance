//! Bilimbi Finance.Sst.Web.Format: amounts, names and refusals as the SST
//! pages show them.
use super::render;
use crate::{ReturnStatus, Sst, SstError, TaxReturn, TaxSource, TaxType};
use bilibi_database::Result;
use bilibi_ui::{Badge, BadgeKind, Html};
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
