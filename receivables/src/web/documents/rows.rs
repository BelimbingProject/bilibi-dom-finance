//! What the Invoices list says about each row and above them: the row's next
//! step, icons and menu, its type and source, and the three figures (awaiting
//! payment, overdue, drafts) that open the list narrowed to them.
use super::{f, DocumentPageRow, Documents, INVOICES, RECEIPTS};
use bilibi_database::Result;
use bilibi_finance_ledger::{OwedFigure, OwedSummary, RecordAction, RecordActions, Tally};
use bilibi_ui::Html;

impl Documents {
    /// A row's next step, local icons and menu: what its record page offers
    /// for its state. Record receipt is the named next step on an unpaid
    /// invoice; Write off is rare and serious, so it waits in the menu.
    pub(super) fn row_actions(&self, d: &DocumentPageRow) -> Result<Html> {
        let here = format!("{INVOICES}/{}", d.id);
        let mut items = Vec::new();
        let issued = d.status == "issued";
        if issued && d.kind == "invoice" {
            if self.can_receive && d.outstanding.is_some_and(|m| m.is_positive()) {
                items.push(RecordAction::next(
                    "Record receipt",
                    format!("{RECEIPTS}?receive={}", d.id),
                ));
            }
            if self.can_add {
                items.push(RecordAction::link(
                    "New credit note",
                    format!("{INVOICES}?credit={}", d.id),
                ));
            }
            if self.can_write_off && d.outstanding.is_some_and(|m| m.is_positive()) {
                items.push(RecordAction::link(
                    "Write off",
                    format!("{here}?{}=write_off", bilibi_finance_ledger::RECORD_DO),
                ));
            }
        }
        if issued && !d.opening && matches!(d.kind.as_str(), "invoice" | "credit_note") {
            items.push(RecordAction::local(
                "printer",
                "Print",
                format!("{here}/pdf"),
            ));
        }
        if d.status == "draft" && self.can_add {
            items.push(RecordAction::here(
                "trash",
                "Discard draft",
                &here,
                "discard",
            ));
        }
        RecordActions::row(
            &format!("receivables-invoice-{}-actions", d.id),
            &Self::page_reference(d),
            items,
        )
    }

    /// The type, and where the document came from when that is not the
    /// ordinary way: "Invoice · Opening", "Invoice · Sales order SO-12".
    pub(super) fn kind(d: &DocumentPageRow) -> String {
        let kind = f::kind_label(&d.kind);
        match &d.sales_order {
            Some(order) => format!("{kind} · Sales order {}", order.order_number),
            None if d.opening && d.kind == "invoice" => format!("{kind} · Opening"),
            None => kind.into(),
        }
    }

    /// Awaiting payment, overdue and drafts above the list, each a link to
    /// the list narrowed to it. Nothing while there are no invoices.
    pub(super) fn strips(&self) -> Result<Html> {
        let Some(s) = self.summary.as_ref().filter(|s| {
            s.not_posted.count > 0 || s.awaiting_payment.count > 0 || s.overdue.count > 0
        }) else {
            return Ok(Html::EMPTY);
        };
        let amount = |tally: &Tally| {
            format!(
                "{} {}",
                self.figures.decimal(tally.amount),
                self.figures.currency
            )
            .trim_end()
            .to_owned()
        };
        let count = |tally: &Tally| OwedSummary::counted(tally.count, "invoice", "invoices");
        OwedSummary::strips(
            "receivables-invoices-summary",
            vec![
                OwedFigure {
                    title: "Awaiting payment",
                    detail: count(&s.awaiting_payment),
                    amount: amount(&s.awaiting_payment),
                    href: format!("{INVOICES}?status=unpaid"),
                },
                OwedFigure {
                    title: "Overdue",
                    detail: s.overdue_detail("invoice", "invoices"),
                    amount: amount(&s.overdue),
                    href: format!("{INVOICES}?status=overdue"),
                },
                OwedFigure {
                    title: "Drafts",
                    detail: format!("{} not issued yet", count(&s.not_posted)),
                    amount: amount(&s.not_posted),
                    href: format!("{INVOICES}?status=draft"),
                },
            ],
        )
    }

    pub(super) fn page_reference(d: &DocumentPageRow) -> String {
        d.document_reference
            .clone()
            .unwrap_or_else(|| format!("Draft #{}", d.id))
    }
}
