//! Opening the New invoice and New credit note dialogs: from their buttons,
//! and New invoice for a customer another list carried: a customer row's actions
//! menu (or a Companies or Contacts row) links here with `?drop=kind:id`, and
//! a row dropped on the page raises `drop_records`. Both open the New invoice
//! dialog with that customer chosen, as if it had been picked there; nothing
//! is saved until the person saves the draft through `draft.create`.
use super::{Documents, Draft, Errors};
use crate::{web::INVOICE_TARGET, MANAGE_DOCUMENTS};
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use bilibi_ui::{Html, StatusKind};
use serde_json::json;
use std::collections::HashMap;

impl Documents {
    /// Opens the New invoice or New credit note dialog, as its button does.
    /// False when the person may not start documents.
    pub(super) async fn start_draft(&mut self, scope: &Scope, kind: &'static str) -> Result<bool> {
        if !self.pages.allowed(scope, MANAGE_DOCUMENTS).await? {
            self.flash.set(StatusKind::Error, crate::web::DENIED);
            return Ok(false);
        }
        self.opening = None;
        if kind == "credit_note" {
            self.credit_invoices = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.document.list",
                    json!({"kind":"invoice","status":"issued"}),
                    ("after", "next_after"),
                )
                .await?
                .0;
        }
        let mut values = HashMap::new();
        let mut open = self.series.iter().filter(|s| s.kind == kind && s.active);
        if let (Some(only), None) = (open.next(), open.next()) {
            values.insert("series_id".into(), only.id.to_string());
        }
        self.create = Some(Draft {
            kind,
            values,
            errors: Errors::default(),
        });
        Ok(true)
    }

    /// The live page opens on the same customer the first render did.
    pub(in crate::web) async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        if self.dropped.is_some() {
            self.load(scope).await?;
        }
        self.open_dropped(scope).await
    }

    pub(super) async fn open_dropped(&mut self, scope: &Scope) -> Result<()> {
        match self.dropped.take() {
            Some(text) => self.receive_drop(scope, &text).await,
            None => Ok(()),
        }
    }

    /// The package is a request only: `Records::receive` re-reads the record
    /// under this person's scope, it must be the party of one of this
    /// company's active customers, and `draft.create` decides on save. A
    /// company or contact that is not a customer here gets a notice instead.
    pub(super) async fn receive_drop(&mut self, scope: &Scope, text: &str) -> Result<()> {
        let dropped = match self
            .pages
            .records
            .receive(scope, INVOICE_TARGET, text)
            .await?
        {
            Ok(dropped) => dropped,
            Err(refusal) => {
                self.flash.set(StatusKind::Error, refusal.message());
                return Ok(());
            }
        };
        let [party] = dropped.as_slice() else {
            return Ok(());
        };
        let reference = format!("{}:{}", party.reference.kind, party.id);
        let name = party.label.clone().unwrap_or_default();
        let customer = self
            .customers
            .iter()
            .find(|customer| customer.party.reference() == reference)
            .map(|customer| (customer.id, customer.active));
        match customer {
            Some((id, true)) => {
                if self.start_draft(scope, "invoice").await? {
                    if let Some(draft) = self.create.as_mut() {
                        draft.values.insert("customer_id".into(), id.to_string());
                    }
                }
            }
            Some((_, false)) => self.flash.set(
                StatusKind::Error,
                format!("{name} is an inactive customer. Make it active on its customer page to invoice it."),
            ),
            None => self.flash.set(
                StatusKind::Error,
                format!("{name} is not a customer of this company yet. Add it as a customer first, then start its invoice."),
            ),
        }
        Ok(())
    }

    /// Companies and contacts dropped on the page open the same dialog as
    /// "+ Invoice", so the tile accepts them only while that button shows.
    pub(super) fn drop_zone(&self, html: String) -> String {
        match self
            .pages
            .records
            .drop_target(INVOICE_TARGET)
            .filter(|_| self.can_add)
        {
            Some(target) => target
                .zone()
                .wrap("receivables-invoices-drop", Html::raw(html))
                .to_string(),
            None => html,
        }
    }
}
