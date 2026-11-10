//! Plain facade answers, mirroring Bilimbi Receivables record views.
use crate::{ReceivableError, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub invoice_layout: InvoiceLayout,
    pub company_id: i64,
    pub receivable_account_id: i64,
    pub income_account_id: i64,
    /// The liability account that holds what customers have paid and not yet
    /// used; none until a company chooses one.
    #[serde(default)]
    pub customer_credit_account_id: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupChange {
    pub id: i64,
    pub field: String,
    pub old_value: Option<i64>,
    pub new_value: i64,
    pub changed_by_user_id: i64,
    pub changed_by_impersonator_id: Option<i64>,
    /// Database-stamped UTC, microsecond precision.
    pub changed_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Series {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub prefix: String,
    pub number_width: i16,
    pub active: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxCode {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub rate: Decimal,
    pub account_id: i64,
    pub active: bool,
}
/// Finance terms and availability, permanently linked to a Core Company or
/// Person. Code/name are authorized Core read facts, never stored by Finance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Customer {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub party: bilibi_finance_ledger::PartyFacts,
    pub payment_terms_days: i32,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLine {
    pub line_number: i32,
    pub description: String,
    pub quantity: Decimal,
    pub unit_price: Decimal,
    #[serde(deserialize_with = "money")]
    pub amount: Money,
    pub account_id: i64,
    pub tax_code_id: Option<i64>,
    pub tax_rate: Option<Decimal>,
    #[serde(deserialize_with = "money")]
    pub tax_amount: Money,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxLine {
    pub tax_code_id: i64,
    pub tax_code: String,
    pub tax_rate: Decimal,
    #[serde(deserialize_with = "money")]
    pub taxable_amount: Money,
    #[serde(deserialize_with = "money")]
    pub tax_amount: Money,
    pub account_id: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    #[serde(default)]
    pub sales_order: Option<crate::SalesOrder>,
    #[serde(default)]
    pub settlements: Vec<serde_json::Value>,
    #[serde(default)]
    pub opening: Option<serde_json::Value>,
    /// The receipt a customer credit came from (`id`, `payment_reference`,
    /// `reversed`); absent on every other document.
    #[serde(default)]
    pub receipt: Option<serde_json::Value>,
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub series_id: i64,
    pub customer_id: i64,
    pub credited_document_id: Option<i64>,
    pub receivable_account_id: Option<i64>,
    pub currency_code: String,
    pub issue_date: Option<String>,
    pub due_date: Option<String>,
    pub description: Option<String>,
    #[serde(deserialize_with = "money")]
    pub subtotal: Money,
    #[serde(deserialize_with = "money")]
    pub tax_total: Money,
    #[serde(deserialize_with = "money")]
    pub total: Money,
    #[serde(deserialize_with = "optional_money")]
    pub outstanding: Option<Money>,
    pub document_number: Option<i64>,
    pub document_reference: Option<String>,
    pub party_code: Option<String>,
    pub party_name: Option<String>,
    pub party_tax_identifier: Option<String>,
    pub party_registration_identifier: Option<String>,
    pub party_email: Option<String>,
    pub party_phone: Option<String>,
    pub party_line1: Option<String>,
    pub party_line2: Option<String>,
    pub party_line3: Option<String>,
    pub party_locality: Option<String>,
    pub party_postcode: Option<String>,
    pub party_admin1_code: Option<String>,
    pub party_country_iso: Option<String>,
    pub ledger_entry_id: Option<i64>,
    pub ledger_entry_number: Option<i64>,
    pub issued_by_user_id: Option<i64>,
    pub issued_by_impersonator_id: Option<i64>,
    pub issued_at: Option<String>,
    pub discarded_by_user_id: Option<i64>,
    pub discarded_at: Option<String>,
    pub lines: Vec<DocumentLine>,
    pub tax_lines: Vec<TaxLine>,
    #[serde(default)]
    pub write_offs: Vec<crate::WriteOff>,
    /// Present only on issue: `queued`, `none`, or `error: unauthorized`
    /// when the issuer cannot view the source (issuance still succeeds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_refresh: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DocumentPageRow {
    #[serde(default)]
    pub sales_order: Option<crate::SalesOrder>,
    pub opening: bool,
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub customer_id: i64,
    pub issue_date: Option<String>,
    pub due_date: Option<String>,
    pub description: Option<String>,
    #[serde(deserialize_with = "money")]
    pub total: Money,
    #[serde(deserialize_with = "optional_money")]
    pub outstanding: Option<Money>,
    pub document_reference: Option<String>,
    pub party_code: Option<String>,
    pub party_name: Option<String>,
    /// An opening invoice a later opening register replaced: kept as source
    /// evidence, it owes nothing and was never paid.
    #[serde(default)]
    pub replaced: bool,
    /// Where an issued invoice's money is; absent on anything else.
    #[serde(default)]
    pub payment_status: Option<bilibi_finance_ledger::PaymentStatus>,
    /// Days past the due date while money is still owed.
    #[serde(default)]
    pub days_overdue: Option<i64>,
}

impl DocumentPageRow {
    /// Says where the money is, read against `as_of`.
    pub(crate) fn settle(&mut self, as_of: jiff::civil::Date) {
        let owed = owed(
            &self.kind,
            &self.status,
            self.replaced,
            self.total,
            self.outstanding,
            self.due_date.as_deref(),
            as_of,
        );
        self.payment_status = owed.map(|o| o.payment_status);
        self.days_overdue = owed.and_then(|o| o.days_overdue);
    }
}

/// Where an invoice's money is on `as_of`: only an issued invoice that was
/// not replaced has an answer.
pub(crate) fn owed(
    kind: &str,
    status: &str,
    replaced: bool,
    total: Money,
    outstanding: Option<Money>,
    due_date: Option<&str>,
    as_of: jiff::civil::Date,
) -> Option<bilibi_finance_ledger::Owed> {
    if kind != "invoice" || status != "issued" || replaced {
        return None;
    }
    Some(bilibi_finance_ledger::Owed::of(
        total.amount(),
        outstanding?.amount(),
        due_date.and_then(|day| day.parse().ok()),
        as_of,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation {
    pub document_id: i64,
    #[serde(deserialize_with = "money")]
    pub amount: Money,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentReversal {
    pub reversal_date: String,
    pub reason: String,
    pub ledger_entry_id: i64,
    pub ledger_entry_number: i64,
    pub reversed_by_user_id: i64,
    pub reversed_by_impersonator_id: Option<i64>,
    pub reversed_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub id: i64,
    pub series_id: i64,
    pub customer_id: i64,
    pub payment_number: i64,
    pub payment_reference: String,
    pub payment_date: String,
    #[serde(deserialize_with = "money")]
    pub amount: Money,
    pub currency_code: String,
    pub deposit_account_id: i64,
    pub reference: Option<String>,
    pub description: Option<String>,
    pub ledger_entry_id: i64,
    pub ledger_entry_number: i64,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
    pub allocations: Vec<Allocation>,
    /// The part of the receipt applied to no invoice, kept as a customer credit.
    #[serde(default)]
    pub credit: Option<PaymentCredit>,
    pub reversal: Option<PaymentReversal>,
}
/// A receipt's unapplied amount: the customer credit document it became, and
/// how much of that credit is still unused (zero once the receipt is reversed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentCredit {
    pub document_id: i64,
    #[serde(deserialize_with = "money")]
    pub amount: Money,
    #[serde(deserialize_with = "money")]
    pub outstanding: Money,
}

// Storage has already enforced numeric(20,4); arithmetic uses the actual
// book's minor units before persistence, not this storage-scale conversion.
pub(crate) fn money<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Money, D::Error> {
    let amount = <Decimal as Deserialize>::deserialize(deserializer)?;
    Money::new(amount, MinorUnits::new(4).expect("storage scale")).map_err(serde::de::Error::custom)
}

fn optional_money<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Money>, D::Error> {
    let amount = <Option<Decimal> as Deserialize>::deserialize(deserializer)?;
    amount
        .map(|a| {
            Money::new(a, MinorUnits::new(4).expect("storage scale"))
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    /// 1–500; absent means 100.
    pub limit: Option<i64>,
    /// Exclusive positive record ID.
    pub after: Option<i64>,
}
impl Page {
    pub(crate) fn bounds(&self) -> Result<(i64, i64)> {
        let limit = self.limit.unwrap_or(100);
        if !(1..=500).contains(&limit) || self.after.is_some_and(|id| id <= 0) {
            return Err(ReceivableError::InvalidOption);
        }
        Ok((limit, self.after.unwrap_or(0)))
    }
}
#[derive(Clone, Debug, Default)]
pub struct DocumentQuery {
    pub source: Option<String>,
    pub page: Page,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub customer_id: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct PaymentQuery {
    pub page: Page,
    pub customer_id: Option<i64>,
}

/// Company letterhead and footer used when regenerating issued documents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InvoiceLayout {
    pub company_name: String,
    pub registration_number: String,
    pub address: String,
    pub contact: String,
    pub footer: String,
}
