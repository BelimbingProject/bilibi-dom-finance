//! A statement line the books do not have, recorded where it belongs: money
//! in as a customer's receipt, money out as a supplier payment. Receivables
//! and Payables own those forms and this module depends on neither, so the
//! row links to them with the ledger's `MoneySeen` and the person comes back
//! here, where a saved receipt's entry is matched to its line.
use super::{company, render, told, Detail};
use crate::{BankAccount, NewMatch, ReconciliationError, ReconciliationItem};
use bilibi_database::Result;
use bilibi_finance_ledger::{MinorUnits, Money, MoneySeen, MONEY_SEEN_ENTRY};
use bilibi_tenancy::Scope;
use bilibi_ui::{params, AddToItem, AddToMenu, Html, StatusKind};
use std::collections::HashMap;

/// Where money in is recorded as a customer's receipt, and what a person
/// needs to do it. Receivables owns the form; this page only links to it.
pub(super) const RECEIPTS: (&str, &str) = (
    "/finance/receivables/receipts",
    "finance.receivables.payments.post",
);
/// Where money out is requested as a supplier payment. Payables owns the form.
pub(super) const PAYMENTS: (&str, &str) = (
    "/finance/payables/payments/new",
    "finance.payables.payments.submit",
);
/// The statement line a link out of this page was about, kept in the address
/// the person comes back to.
const LINE: &str = "line";

impl Detail {
    /// The connected page's address: a person coming back from recording a
    /// statement line names the line and the entry that was posted for it.
    pub(in crate::web) fn returned(mut self, query: &HashMap<String, String>) -> Self {
        let id = |key: &str| params::positive_id(query.get(key).map(String::as_str));
        self.returned = id(LINE).zip(id(MONEY_SEEN_ENTRY));
        self
    }

    /// Matches the statement line a person just recorded elsewhere to the
    /// entry that recording posted, so they do not match it by hand. Only an
    /// entry this person posted: an address alone never matches someone
    /// else's entry. The facade still refuses sides that differ.
    pub(super) async fn match_returned(
        &mut self,
        scope: &Scope,
        line: i64,
        entry: i64,
    ) -> Result<()> {
        if !self.can_match {
            return Ok(());
        }
        let Some(account) = self.account.as_ref().map(|a| a.id) else {
            return Ok(());
        };
        let company = company(scope)?;
        let mine = match self.pages.ledger.get_entry(scope, company, entry).await {
            Ok(posted) => posted.posted_by_user_id == scope.user(),
            Err(_) => false,
        };
        if !mine {
            return Ok(());
        }
        let new = NewMatch {
            statement_line_ids: vec![line],
            entry_ids: vec![entry],
            note: Some("Recorded from the statement line".into()),
        };
        let r = &self.pages.reconciliation;
        self.flash = match r.record_match(scope, company, account, &new).await {
            Ok(_) => Some((
                StatusKind::Success,
                "Recorded and matched to the statement line.".into(),
            )),
            // Already matched (the page was opened twice): nothing to say.
            Err(ReconciliationError::StatementLineMatched | ReconciliationError::EntryMatched) => {
                None
            }
            Err(e) => match told::<()>(Err(e))? {
                Err(why) => Some((
                    StatusKind::Warning,
                    format!("Recorded, but not matched to the statement line. {why}"),
                )),
                Ok(()) => None,
            },
        };
        Ok(())
    }

    /// The link that records a statement line in another module's form and
    /// comes back here.
    fn record_href(
        &self,
        path: &str,
        account: &BankAccount,
        item: &ReconciliationItem,
        units: u8,
    ) -> Option<String> {
        let line = item.statement_line_id?;
        let units = MinorUnits::new(i64::from(units))?;
        let amount = Money::new(item.amount.amount().abs(), units).ok()?;
        Some(
            MoneySeen {
                date: item.item_date.to_string(),
                amount: amount.shown(units),
                account_id: Some(account.ledger_account_id),
                text: item.description.clone(),
                reference: item.reference.clone().unwrap_or_default(),
                return_to: Some(format!(
                    "/finance/reconciliation/reconciliations/{}?{LINE}={line}",
                    self.id
                )),
            }
            .href(path),
        )
    }

    /// The row's Non-local actions menu: Record receipt for money in, Pay a
    /// bill for money out, for a person who may; nothing otherwise.
    pub(super) fn elsewhere(
        &self,
        account: &BankAccount,
        item: &ReconciliationItem,
        row: usize,
        units: u8,
    ) -> Result<Html> {
        let choice = if item.amount.is_positive() {
            self.can_receive.then_some(("Record receipt", RECEIPTS.0))
        } else {
            (!item.amount.is_zero() && self.can_pay).then_some(("Pay a bill", PAYMENTS.0))
        };
        let Some((label, href)) = choice
            .and_then(|(label, path)| Some((label, self.record_href(path, account, item, units)?)))
        else {
            return Ok(Html::EMPTY);
        };
        render(AddToMenu::new(
            format!("item-{row}-actions"),
            item.description.clone(),
            vec![AddToItem {
                label: label.into(),
                href,
                disabled: None,
            }],
        ))
    }
}
