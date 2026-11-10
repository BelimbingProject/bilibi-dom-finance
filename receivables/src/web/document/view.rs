//! How an invoice or credit note reads: the document itself on the left
//! (customer, dates, lines, totals) and a narrow summary on the right
//! (status, amount due, receipts and credits, its journal, files), with Back,
//! its local actions as icon buttons, one "Non-local actions" menu and the one primary verb for its state in the header
//! (`bilibi_finance_ledger::DocumentLayout`, `RecordActions`). Every action
//! still raises the same event or opens the same page as before.
use super::{
    super::{
        account_html, documents::reference, format as f, render, CUSTOMERS, INVOICES, RECEIPTS,
    },
    Document,
};
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::{
    DocumentLayout, EntryLink, Money, People, RecordAction, RecordActions,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    BackLink, Button, ButtonVariant, Card, Cell, ConfirmDialog, EmptyState, Facts, FlexRow, Header,
    Html, InlineEdit, InlineLongText, PageVariant, SectionHeading,
};
use serde_json::Value;

impl Document {
    pub(super) async fn body(&mut self, scope: &Scope) -> Result<String> {
        let back = render(BackLink {
            href: INVOICES,
            id: "receivables-invoice-back",
            title: "Back to invoices",
            ..BackLink::default()
        })?;
        let Some(d) = self.record.clone() else {
            let mut missing = EmptyState::missing(
                "Document not found",
                "There is no such invoice or credit note in this company, or you cannot read it.",
            );
            missing.id = "receivables-invoice-missing".into();
            missing.action = back;
            return f::frame(
                "receivables-invoice",
                PageVariant::Detail,
                &self.flash,
                render(missing)?,
                None,
            );
        };
        let display = self.pages.display(scope).await?;
        let mut mentioned = vec![
            d.issued_by_user_id,
            d.issued_by_impersonator_id,
            d.discarded_by_user_id,
        ];
        mentioned.extend(d.emails.iter().map(|e| Some(e.requested_by_user_id)));
        for w in &d.write_offs {
            mentioned.push(Some(w.submitted_by_user_id));
            mentioned.push(w.submitted_by_impersonator_id);
            for e in &w.events {
                mentioned.push(Some(e.actor_user_id));
                mentioned.push(e.actor_impersonator_id);
            }
        }
        let people = self.pages.people(scope, mentioned).await?;
        let kind = f::kind_label(&d.kind);
        let customer_name = d
            .party_name
            .clone()
            .or_else(|| self.customer.as_ref().map(|c| c.name.clone()))
            .unwrap_or_default();
        let name = reference(&d);

        let mut body = f::announce(d.id)?;
        body.push(&render(Header {
            title: Html::text(&name),
            title_actions: f::status_badge(&d.status)?,
            subtitle: &format!("{kind} for {customer_name}"),
            actions: self.header_actions(&d, back)?,
        })?);

        let mut document = self.document_card(&d, kind, &customer_name, &display)?;
        document.push(&self.lines_card(&d)?);
        document.push(&self.write_offs.body(
            &d,
            &self.accounts,
            &self.figures,
            &display,
            &people,
        )?);
        document.push(
            &self
                .credit_actions
                .body(
                    &self.pages,
                    d.id,
                    &d.kind,
                    &d.settlements,
                    self.figures.units(),
                )
                .await?,
        );
        if let Some(history) = self.history_card(&d, &display, &people)? {
            document.push(&history);
        }
        let mut summary = self.summary_card(&d, &display, &people)?;
        if let Some(applied) = self.applied_card(&d)? {
            summary.push(&applied);
        }
        if !self.balance_pins.is_object() {
            summary.push(&super::attachments_slot()?);
        }
        body.push(&DocumentLayout::render(
            "receivables-invoice-layout",
            document,
            summary,
        )?);

        let modal = self.modal(&display)?;
        f::frame(
            "receivables-invoice",
            PageVariant::Detail,
            &self.flash,
            body,
            modal,
        )
    }

    /// Back, the local actions, the "Non-local actions" menu, then the one primary verb for this state:
    /// Issue a draft; Record receipt while an invoice is owed.
    fn header_actions(&self, d: &crate::Document, back: Html) -> Result<Html> {
        let here = format!("{INVOICES}/{}", d.id);
        let historical = self.balance_pins.is_object();
        let issued = d.status == "issued";
        let owed = d.outstanding.is_some_and(|m| m.is_positive());
        let mut items = Vec::new();
        let mut primary = None;
        if self.draft() {
            if self.can_issue {
                primary = Some(render(Button {
                    id: "receivables-invoice-issue",
                    button_type: "button",
                    event: "ask_issue",
                    variant: ButtonVariant::Primary,
                    body: Html::text(&format!("Issue {}", f::kind_label(&d.kind).to_lowercase())),
                    ..Button::default()
                })?);
            }
            if self.can_manage {
                items.push(RecordAction::here(
                    "trash",
                    "Discard draft",
                    &here,
                    "discard",
                ));
            }
        }
        if issued && !historical && d.kind == "invoice" {
            if owed && self.can_receive {
                primary = Some(render(Button {
                    id: "receivables-invoice-receive",
                    href: &format!("{RECEIPTS}?receive={}", d.id),
                    variant: ButtonVariant::Primary,
                    body: Html::text("Record receipt"),
                    ..Button::default()
                })?);
            }
            if self.can_manage {
                items.push(RecordAction::link(
                    "New credit note",
                    format!("{INVOICES}?credit={}", d.id),
                ));
            }
            if owed && self.write_offs.can_propose() {
                items.push(RecordAction::here(
                    "document-minus",
                    "Write off",
                    &here,
                    "write_off",
                ));
            }
        }
        if issued && d.kind == "invoice" && self.can_send() {
            items.push(RecordAction::here(
                "envelope",
                "Send by email",
                &here,
                "send",
            ));
        }
        if issued && d.opening.is_none() && matches!(d.kind.as_str(), "invoice" | "credit_note") {
            items.push(RecordAction::local(
                "printer",
                "Print / download PDF",
                format!("{INVOICES}/{}/pdf", d.id),
            ));
        }
        RecordActions::render(
            back,
            "receivables-invoice-actions",
            &reference(d),
            items,
            primary,
        )
    }

    /// The document as the customer reads it: who, when, what for, and the
    /// details it was issued with.
    fn document_card(
        &self,
        d: &crate::Document,
        kind: &str,
        customer: &str,
        display: &DisplayContext,
    ) -> Result<Html> {
        let editable = self.editable();
        // A date reads as the pages write dates ("9 Nov 2026") and edits in
        // the shared date box, which takes that form, 9/11/2026 or a calendar
        // pick. `empty` says what the document gets when the draft has none.
        let date_fact = |key: &str,
                         label: &'static str,
                         value: &str,
                         empty: &str|
         -> Result<bilibi_ui::Fact<'static>> {
            let mut field = f::inline(
                key,
                label,
                value,
                d.id,
                "save_draft",
                editable,
                &self.statuses,
            )
            .input_type("date");
            match value.parse::<jiff::civil::Date>() {
                Ok(date) => field = field.display_value(display.render_date(date)),
                Err(_) if value.is_empty() && self.draft() => field = field.display_value(empty),
                Err(_) => {}
            }
            Ok(f::fact_html(label, render(InlineEdit { field })?))
        };
        let mut items = vec![f::fact_html(
            "Customer",
            f::link(&format!("{CUSTOMERS}/{}", d.customer_id), customer)?,
        )];
        if let Some(credited) = d.credited_document_id {
            items.push(f::fact_html(
                "Credits invoice",
                f::link(
                    &format!("{INVOICES}/{credited}"),
                    &self.credited.clone().unwrap_or_else(|| "Invoice".into()),
                )?,
            ));
        }
        items.push(date_fact(
            "issue_date",
            "Issue date",
            d.issue_date.as_deref().unwrap_or(""),
            "The day you issue it",
        )?);
        items.push(date_fact(
            "due_date",
            "Due date",
            if d.kind == "opening_credit" {
                d.opening
                    .as_ref()
                    .and_then(|v| v["due_date"].as_str())
                    .unwrap_or("—")
            } else {
                d.due_date.as_deref().unwrap_or("")
            },
            "From the customer's payment terms",
        )?);
        if self.draft() {
            items.push(f::fact(
                "Series",
                self.series.as_ref().map_or("—", |s| s.name.as_str()),
            ));
        }
        if let Some(order) = &d.sales_order {
            items.push(f::fact("Sales order", &order.order_number));
            if order.status == "cancelled" {
                items.push(f::fact("Order status", "Cancelled — review this invoice."));
            }
        }
        if self.draft() {
            let mut field = f::inline(
                "description",
                "Description",
                d.description.as_deref().unwrap_or(""),
                d.id,
                "save_draft",
                editable,
                &self.statuses,
            );
            if editable {
                field.placeholder = "Add a description".into();
            }
            items.push(f::fact_html(
                "Description",
                render(InlineLongText { field, rows: 3 })?,
            ));
        } else if d.description.is_some() {
            items.push(f::fact("Description", f::optional(&d.description)));
        }
        if let Some(opening) = &d.opening {
            items.extend(self.opening_facts(opening));
        }
        let mut b = render(SectionHeading {
            id: "receivables-invoice-document-heading",
            title: kind,
            ..SectionHeading::default()
        })?;
        b.push(&render(Facts {
            id: "receivables-invoice-facts",
            items,
        })?);
        if d.status == "issued" {
            b.push(&render(SectionHeading {
                id: "receivables-invoice-party-heading",
                title: "Billed to",
                class: "mt-4",
                ..SectionHeading::default()
            })?);
            b.push(&render(Facts {
                id: "receivables-invoice-party",
                items: vec![
                    f::fact("Name", f::optional(&d.party_name)),
                    f::fact("Tax ID", f::optional(&d.party_tax_identifier)),
                    f::fact(
                        "Registration number",
                        f::optional(&d.party_registration_identifier),
                    ),
                    f::fact("Email", f::optional(&d.party_email)),
                    f::fact("Phone", f::optional(&d.party_phone)),
                    f::fact("Address", &billed_address(d)),
                ],
            })?);
        }
        render(Card {
            id: "receivables-invoice-document",
            role: "region",
            labelledby: "receivables-invoice-document-heading",
            body: b,
            ..Card::default()
        })
    }

    fn opening_facts(&self, opening: &Value) -> Vec<bilibi_ui::Fact<'static>> {
        let text = |key: &str| opening[key].as_str().unwrap_or("").to_owned();
        let mut items = vec![
            f::fact(
                "Opening item",
                f::opening_item_kind_label(opening["item_kind"].as_str().unwrap_or("")),
            ),
            f::fact("Source system", &text("source_system")),
            f::fact("Source reference", &text("source_reference")),
            f::fact("Source document", &text("document_reference")),
            f::fact(
                "Conversion date",
                &bilibi_finance_ledger::dates::display_text(&text("conversion_date")),
            ),
        ];
        if let Some(value) = opening["original_total"]
            .as_str()
            .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
        {
            items.push(f::fact("Original total", &self.figures.decimal(value)));
        }
        if let Some(batch) = opening["replaced_by_batch_id"].as_i64() {
            items.push(f::fact(
                "Opening register",
                &format!("Replaced by register {batch}; kept as source evidence."),
            ));
        }
        items
    }

    /// The lines, the tax summary and the totals, as one part of the document.
    fn lines_card(&mut self, d: &crate::Document) -> Result<Html> {
        self.lines_cols
            .load(&d.lines, |l| l.line_number.to_string());
        self.tax_cols
            .load(&d.tax_lines, |t| t.tax_code_id.to_string());
        let mut line_rows = Vec::new();
        for l in &d.lines {
            let mut row = FlexRow::new(l.line_number.to_string());
            row.dom_id = Some(format!("receivables-invoice-line-{}", l.line_number));
            line_rows.push(
                row.cell("number", Cell::text(l.line_number.to_string()))
                    .cell("description", Cell::text(&l.description))
                    .slot("quantity", f::tabular(&f::decimal(l.quantity))?)
                    .slot(
                        "unit_price",
                        f::tabular(&Money::display_decimal(l.unit_price, None))?,
                    )
                    .slot("account", account_html(&self.accounts, l.account_id)?)
                    .cell(
                        "tax",
                        Cell::text(match (l.tax_code_id, l.tax_rate) {
                            (Some(id), Some(rate)) => {
                                format!("{} ({}%)", self.tax_code(Some(id)), f::decimal(rate))
                            }
                            (Some(id), None) => self.tax_code(Some(id)),
                            _ => "—".into(),
                        }),
                    )
                    .slot("tax_amount", f::tabular(&self.figures.money(l.tax_amount))?)
                    .slot("amount", f::tabular(&self.figures.money(l.amount))?),
            );
        }
        let mut tax_rows = Vec::new();
        for t in &d.tax_lines {
            let mut row = FlexRow::new(t.tax_code_id.to_string());
            row.dom_id = Some(format!("receivables-invoice-tax-{}", t.tax_code_id));
            tax_rows.push(
                row.cell("code", Cell::text(&t.tax_code))
                    .slot("rate", f::tabular(&format!("{}%", f::decimal(t.tax_rate)))?)
                    .slot(
                        "taxable",
                        f::tabular(&self.figures.money(t.taxable_amount))?,
                    )
                    .slot("tax", f::tabular(&self.figures.money(t.tax_amount))?),
            );
        }
        let edit = if self.editable() {
            render(Button {
                id: "receivables-invoice-edit-lines",
                button_type: "button",
                event: "edit_lines",
                body: Html::text("Edit lines"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut b = render(SectionHeading {
            title: "Lines",
            id: "receivables-invoice-lines-heading",
            class: "p-5 pb-0",
            actions: edit,
            ..SectionHeading::default()
        })?;
        b.push(&f::table(
            &self.lines_cols,
            "lines_grid",
            "Lines",
            line_rows,
            if self.draft() {
                "No lines yet. Use Edit lines to add what you are invoicing."
            } else {
                "This document has no lines."
            },
        )?);
        if !d.tax_lines.is_empty() {
            b.push(&render(SectionHeading {
                title: "Tax summary",
                class: "p-5 pb-0",
                ..SectionHeading::default()
            })?);
            b.push(&f::table(
                &self.tax_cols,
                "tax_grid",
                "Tax summary",
                tax_rows,
                "",
            )?);
        }
        b.push(&f::padded(render(Facts {
            id: "receivables-invoice-totals",
            items: vec![
                f::fact(
                    "Subtotal",
                    &self.figures.with_currency(d.subtotal, &d.currency_code),
                ),
                f::fact(
                    "Tax",
                    &self.figures.with_currency(d.tax_total, &d.currency_code),
                ),
                f::fact(
                    "Total",
                    &self.figures.with_currency(d.total, &d.currency_code),
                ),
            ],
        })?)?);
        render(Card {
            id: "receivables-invoice-lines",
            role: "region",
            labelledby: "receivables-invoice-lines-heading",
            inner_class: "p-0",
            body: b,
            ..Card::default()
        })
    }

    /// Where the document stands: status, what is still owed, its journal and
    /// who issued it.
    fn summary_card(
        &self,
        d: &crate::Document,
        display: &DisplayContext,
        people: &People,
    ) -> Result<Html> {
        let mut items = vec![f::fact_html("Status", f::status_badge(&d.status)?)];
        if let Some(pins) = self.balance_pins.as_object() {
            items.push(f::fact(
                "Balance as of",
                &bilibi_finance_ledger::dates::display_text(
                    pins.get("as_of").and_then(Value::as_str).unwrap_or(""),
                ),
            ));
        }
        let issued = d.status == "issued";
        if issued && matches!(d.kind.as_str(), "invoice" | "opening_credit") {
            items.push(f::fact(
                if d.kind == "opening_credit" {
                    "Available credit"
                } else {
                    "Amount due"
                },
                &self
                    .figures
                    .with_currency_opt(d.outstanding, &d.currency_code),
            ));
        }
        items.push(f::fact(
            "Total",
            &self.figures.with_currency(d.total, &d.currency_code),
        ));
        match d.status.as_str() {
            "issued" => {
                items.push(f::fact_html(
                    "Journal",
                    EntryLink::html(d.ledger_entry_id, d.ledger_entry_number)?,
                ));
                if let Some(id) = d.receivable_account_id {
                    items.push(f::fact_html(
                        "Receivable account",
                        account_html(&self.accounts, id)?,
                    ));
                }
                items.push(f::fact_html(
                    "Issued",
                    actor_at(
                        display,
                        people,
                        d.issued_by_user_id,
                        d.issued_by_impersonator_id,
                        d.issued_at.as_deref(),
                        "receivables-invoice-issued-at",
                    )?,
                ));
            }
            "discarded" => items.push(f::fact_html(
                "Discarded",
                actor_at(
                    display,
                    people,
                    d.discarded_by_user_id,
                    None,
                    d.discarded_at.as_deref(),
                    "receivables-invoice-discarded-at",
                )?,
            )),
            _ => items.push(f::fact(
                "Posting",
                "Draft: no ref no and no posting until issued.",
            )),
        }
        let mut b = render(SectionHeading {
            id: "receivables-invoice-summary-heading",
            title: "Summary",
            ..SectionHeading::default()
        })?;
        b.push(&render(Facts {
            id: "receivables-invoice-summary",
            items,
        })?);
        render(Card {
            id: "receivables-invoice-summary-card",
            role: "region",
            labelledby: "receivables-invoice-summary-heading",
            body: b,
            ..Card::default()
        })
    }

    /// What has been received or credited against an issued invoice.
    fn applied_card(&mut self, d: &crate::Document) -> Result<Option<Html>> {
        if d.status != "issued" || d.kind != "invoice" {
            return Ok(None);
        }
        let mut applied = Vec::new();
        for p in &self.receipts {
            let allocated = p
                .allocations
                .iter()
                .filter(|a| a.document_id == d.id)
                .map(|a| a.amount.amount())
                .sum::<rust_decimal::Decimal>();
            let mut row = FlexRow::new(format!("p{}", p.id));
            row.dom_id = Some(format!("receivables-invoice-receipt-{}", p.id));
            applied.push(
                row.slot(
                    "document",
                    f::link(&format!("{RECEIPTS}/{}", p.id), &p.payment_reference)?,
                )
                .cell(
                    "date",
                    Cell::text(bilibi_finance_ledger::dates::display_text(&p.payment_date)),
                )
                .slot("amount", f::tabular(&self.figures.decimal(allocated))?)
                .slot("status", super::super::payments::status_badge(p)?),
            );
        }
        for c in &self.credits {
            let mut row = FlexRow::new(format!("c{}", c.id));
            row.dom_id = Some(format!("receivables-invoice-credit-{}", c.id));
            applied.push(
                row.slot(
                    "document",
                    f::link(&format!("{INVOICES}/{}", c.id), &reference(c))?,
                )
                .cell(
                    "date",
                    Cell::text(bilibi_finance_ledger::dates::display_text(f::optional(
                        &c.issue_date,
                    ))),
                )
                .slot("amount", f::tabular(&self.figures.money(c.total))?)
                .slot("status", f::status_badge(&c.status)?),
            );
        }
        let mut b = render(SectionHeading {
            id: "receivables-invoice-applied-heading",
            title: "Receipts and credits",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        b.push(&f::table(
            &self.applied_cols,
            "applied_grid",
            "Receipts and credits",
            applied,
            "Nothing received or credited yet.",
        )?);
        Ok(Some(render(Card {
            id: "receivables-invoice-applied",
            role: "region",
            labelledby: "receivables-invoice-applied-heading",
            inner_class: "p-0",
            body: b,
            ..Card::default()
        })?))
    }

    fn modal(&self, display: &DisplayContext) -> Result<Option<Html>> {
        if let Some(sending) = &self.sending {
            return Ok(Some(sending.dialog(self.id, &self.flash)?));
        }
        if let Some(dialog) = self
            .write_offs
            .dialog(&self.accounts, &self.flash, &self.figures)?
        {
            return Ok(Some(dialog));
        }
        if let Some(editing) = &self.editing {
            return Ok(Some(self.lines_dialog(editing)?));
        }
        if let Some(issuing) = &self.issuing {
            return Ok(Some(issuing.dialog(
                &self.figures,
                display,
                "issue",
                "issue_send",
                "cancel_dialog",
            )?));
        }
        if !self.discarding {
            return Ok(None);
        }
        Ok(Some(render(ConfirmDialog {
            id: "receivables-invoice-discard-confirm",
            consequence: "Discard this draft?",
            detail: "The draft and its lines are kept on record as discarded. It takes no number and posts nothing. This cannot be undone.",
            confirm: "Discard",
            working: "Discarding…",
            confirm_event: "discard",
            cancel_event: "cancel_dialog",
        })?))
    }
}

/// The billing address frozen on an issued document, as one line.
fn billed_address(d: &crate::Document) -> String {
    let mut lines: Vec<String> = [&d.party_line1, &d.party_line2, &d.party_line3]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let place = [
        &d.party_postcode,
        &d.party_locality,
        &d.party_admin1_code,
        &d.party_country_iso,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect::<Vec<_>>()
    .join(" ");
    if !place.is_empty() {
        lines.push(place);
    }
    if lines.is_empty() {
        "—".into()
    } else {
        lines.join(", ")
    }
}

/// Who did it and when: "Ana Lim · 6 Nov 2026, 09:13".
pub(super) fn actor_at(
    display: &DisplayContext,
    people: &People,
    user: Option<i64>,
    impersonator: Option<i64>,
    at: Option<&str>,
    element: &str,
) -> Result<Html> {
    let mut value = Html::text(&people.who(user, impersonator));
    if let Some(at) = at {
        value.push(&Html::text(" · "));
        value.push(&f::time(display, element, at)?);
    }
    Ok(value)
}
