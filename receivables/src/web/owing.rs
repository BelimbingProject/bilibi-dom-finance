//! What each customer owes today, and how much of it is overdue, for the
//! Customers list. Read from `finance.receivables.aging.get` as of today, so
//! the figures are the aging report's own: its first bucket is not yet due,
//! the rest are overdue. Owed is net of the credit the customer has left (a
//! deposit, an overpayment or an opening credit), as the report's total is;
//! a customer with more credit than unpaid invoices reads "300.00 credit".
use super::{format as f, Pages, INVOICES};
use bilibi_database::Result;
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::Html;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Report {
    minor_units: u32,
    customers: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    customer_id: i64,
    bucket_totals: Vec<Decimal>,
    #[serde(default)]
    credit_total: Decimal,
}

/// Owed and overdue by customer; empty when the person may not read aging
/// (it needs ledger view), and the columns then show "—".
#[derive(Default)]
pub(super) struct Owing {
    /// Unpaid invoices, the overdue part of them, and unused credit.
    by_customer: HashMap<i64, (Decimal, Decimal, Decimal)>,
    units: Option<MinorUnits>,
    known: bool,
}

impl Owing {
    pub(super) async fn read(pages: &Pages, scope: &Scope) -> Self {
        let today = pages.today(scope).await;
        let report = pages
            .read(
                scope,
                "finance.receivables.aging.get",
                json!({"as_of": today}),
            )
            .await
            .ok()
            .and_then(|value| serde_json::from_value::<Report>(value).ok());
        let Some(report) = report else {
            return Self::default();
        };
        Self {
            units: MinorUnits::new(i64::from(report.minor_units)),
            by_customer: report
                .customers
                .into_iter()
                .map(|c| {
                    let owed: Decimal = c.bucket_totals.iter().sum();
                    let overdue: Decimal = c.bucket_totals.iter().skip(1).sum();
                    (c.customer_id, (owed, overdue, c.credit_total))
                })
                .collect(),
            known: true,
        }
    }

    /// The Owed and Overdue cells of one customer: each amount opens the
    /// records behind it, and nothing owed reads "—". Owed is what the
    /// customer's unpaid invoices come to after the credit they have left.
    pub(super) fn cells(&self, customer: i64) -> Result<(Html, Html)> {
        let (unpaid, overdue, credit) =
            self.by_customer.get(&customer).copied().unwrap_or_default();
        let amount = |value: Decimal| Money::display_decimal(value, self.units);
        let invoices = |status: &str, text: &str| -> Result<Html> {
            f::link(
                &format!("{INVOICES}?customer={customer}&status={status}"),
                text,
            )
        };
        let net = unpaid - credit;
        let owed = if !self.known || (unpaid.is_zero() && credit.is_zero()) {
            f::tabular("—")?
        } else if net < Decimal::ZERO {
            f::link(
                &format!("{INVOICES}?customer={customer}&kind=customer_credit"),
                &format!("{} credit", amount(-net)),
            )?
        } else {
            invoices("unpaid", &amount(net))?
        };
        let overdue = if !self.known || overdue.is_zero() {
            f::tabular("—")?
        } else {
            invoices("overdue", &amount(overdue))?
        };
        Ok((owed, overdue))
    }
}
