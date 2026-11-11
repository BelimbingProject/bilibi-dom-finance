//! Record receipt opened for money another page saw (a bank statement line,
//! carried by the ledger's `MoneySeen`): the dialog starts filled in, so the
//! person checks and saves instead of typing the line again.
use super::{f, Payments};
use crate::Document;
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::json;

impl Payments {
    /// Open Record receipt for money another page saw: its date, amount,
    /// account and reference, and the customer it most likely came from with
    /// the amount applied to what they owe.
    pub(super) async fn start_seen(&mut self, scope: &Scope) -> Result<()> {
        let Some(seen) = self.seen.clone() else {
            return Ok(());
        };
        self.start_receipt(scope, None).await?;
        if self.draft.is_none() {
            return Ok(());
        }
        let (unpaid, _) = self
            .pages
            .read_all::<Document>(
                scope,
                "finance.receivables.document.list",
                json!({"kind": "invoice", "status": "issued"}),
                ("after", "next_after"),
            )
            .await?;
        let owed: Vec<(i64, Vec<Decimal>)> = self
            .customers
            .iter()
            .map(|c| {
                let amounts = unpaid
                    .iter()
                    .filter(|d| d.customer_id == c.id)
                    .filter_map(|d| d.outstanding.filter(|m| m.is_positive()))
                    .map(|m| m.amount())
                    .collect();
                (c.id, amounts)
            })
            .collect();
        let likely = seen
            .likely(
                self.customers
                    .iter()
                    .zip(&owed)
                    .map(|(c, (_, amounts))| (c.name.as_str(), amounts.as_slice())),
            )
            .map(|index| owed[index].0.to_string());
        let open = match &likely {
            Some(customer) => self.open_invoices(scope, customer).await?,
            None => vec![],
        };
        let deposit = seen.account_id.filter(|id| {
            Some(*id) != self.control
                && self
                    .accounts
                    .iter()
                    .any(|a| a.id == *id && a.postable() && a.account_type == "asset")
        });
        let Some(draft) = &mut self.draft else {
            return Ok(());
        };
        let values = &mut draft.values;
        if !seen.date.is_empty() {
            values.insert("payment_date".into(), seen.date.clone());
        }
        values.insert("amount".into(), seen.amount.clone());
        if let Some(account) = deposit {
            values.insert("deposit_account_id".into(), account.to_string());
        }
        if !seen.reference.is_empty() {
            values.insert(
                "reference".into(),
                seen.reference.chars().take(100).collect(),
            );
        }
        if !seen.text.is_empty() {
            values.insert("description".into(), seen.text.clone());
        }
        if let Some(customer) = likely {
            values.insert("customer_id".into(), customer);
            let amounts: Vec<Decimal> = open
                .iter()
                .map(|d| d.outstanding.map_or(Decimal::ZERO, |m| m.amount()))
                .collect();
            for (d, take) in open.iter().zip(seen.split(&amounts)) {
                if take > Decimal::ZERO {
                    values.insert(format!("alloc_{}", d.id), f::decimal(take));
                }
            }
            draft.open = open;
        }
        Ok(())
    }
}
