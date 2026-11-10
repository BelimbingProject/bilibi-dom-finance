//! What each customer owes today, and how much of it is overdue, for the
//! Customers list. Read from `finance.receivables.aging.get` as of today, so
//! the figures are the aging report's own: its first bucket is not yet due,
//! the rest are overdue.
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
}

/// Owed and overdue by customer; empty when the person may not read aging
/// (it needs ledger view), and the columns then show "—".
#[derive(Default)]
pub(super) struct Owing {
    by_customer: HashMap<i64, (Decimal, Decimal)>,
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
                    (c.customer_id, (owed, overdue))
                })
                .collect(),
            known: true,
        }
    }

    /// The Owed and Overdue cells of one customer: each amount opens the
    /// invoices behind it, and nothing owed reads "—".
    pub(super) fn cells(&self, customer: i64) -> Result<(Html, Html)> {
        let (owed, overdue) = self.by_customer.get(&customer).copied().unwrap_or_default();
        let cell = |amount: Decimal, status: &str| -> Result<Html> {
            if !self.known || amount.is_zero() {
                return f::tabular("—");
            }
            f::link(
                &format!("{INVOICES}?customer={customer}&status={status}"),
                &Money::display_decimal(amount, self.units),
            )
        };
        Ok((cell(owed, "unpaid")?, cell(overdue, "overdue")?))
    }
}
