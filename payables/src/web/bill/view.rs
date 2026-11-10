//! How a bill or debit note reads, as its twin the invoice does: the document
//! on the left (supplier, dates, lines, totals, the three-way match) and a
//! narrow summary on the right (status, amount due, its journal, who recorded,
//! approved and posted it, files), with Back, its local actions as icon buttons, one "Non-local actions" menu and the one
//! primary verb for its state in the header
//! (`bilibi_finance_ledger::DocumentLayout`, `RecordActions`).
use super::{
    super::{format as f, render, INDEX, NEW_PAYMENT},
    table, Attention, Bill, Corrects, Dialog, DialogView, ExceptionView, MatchResult, Missing,
};
use crate::{
    Document, APPROVE_DOCUMENTS, APPROVE_MATCH, CREATE_DOCUMENTS, POST_DOCUMENTS, SUBMIT_PAYMENTS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::{
    AccountName, DocumentLayout, DocumentTotals, EntryLink, MinorUnits, Owed, People, RecordAction,
    RecordActions,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    BackLink, BadgeKind, Button, ButtonVariant, Card, Cell, EmptyState, Fact, Facts, FlashMap,
    FlexRow, Html, SectionHeading, NO_FLASH,
};
use serde_json::Value;

/// What this person may do to the bill now.
struct Can {
    approve: bool,
    post: bool,
    match_: bool,
    create: bool,
    pay: bool,
}

impl Bill {
    pub(in crate::web) async fn body(&self, scope: &Scope) -> Result<String> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                bilibi_ui::StatusKind::Success => flash.success = message,
                bilibi_ui::StatusKind::Warning => flash.warning = message,
                bilibi_ui::StatusKind::Error => flash.error = message,
                bilibi_ui::StatusKind::Info => flash.info = message,
            }
        }
        let back = render(BackLink {
            href: INDEX,
            id: "payables-bill-back",
            title: "Back to bills",
            ..BackLink::default()
        })?;
        let Some(d) = &self.document else {
            let mut missing = EmptyState::missing(
                "Document not found",
                "There is no such document for this company, or it cannot be read with your current access.",
            );
            missing.id = "payables-bill-missing".into();
            missing.action = back;
            return Missing {
                flash,
                missing: render(missing)?,
            }
            .render()
            .map_err(|_| Error::Internal);
        };
        let historical = self.balance_pins.is_object();
        let can = Can {
            approve: !historical && self.pages.allowed(scope, APPROVE_DOCUMENTS).await?,
            post: !historical && self.pages.allowed(scope, POST_DOCUMENTS).await?,
            match_: !historical && self.pages.allowed(scope, APPROVE_MATCH).await?,
            create: !historical && self.pages.allowed(scope, CREATE_DOCUMENTS).await?,
            pay: !historical && self.pages.allowed(scope, SUBMIT_PAYMENTS).await?,
        };
        let display = self.pages.display(scope).await?;
        let mut mentioned = vec![
            Some(d.created_by_user_id),
            d.created_by_impersonator_id,
            d.approved_by_user_id,
            d.approved_by_impersonator_id,
            d.posted_by_user_id,
            d.posted_by_impersonator_id,
            d.voided_by_user_id,
            d.voided_by_impersonator_id,
        ];
        for e in &d.match_events {
            mentioned.push(Some(e.actor_user_id));
            mentioned.push(e.actor_impersonator_id);
        }
        let people = self.pages.people(scope, mentioned).await?;
        let units = self.units;
        // Where the money is today; a balance read as of an earlier date
        // keeps the plain status, since "overdue" would be about today.
        let replaced = d
            .opening
            .as_ref()
            .is_some_and(|o| !o["replaced_by_batch_id"].is_null());
        let owed = match d.outstanding {
            Some(outstanding)
                if !historical
                    && !replaced
                    && d.document_type == "bill"
                    && d.status == "posted" =>
            {
                Some(Owed::of(
                    d.total.amount(),
                    outstanding.amount(),
                    Some(d.due_date),
                    self.pages.today(scope).await?,
                ))
            }
            _ => None,
        };
        let status = match owed {
            Some(owed) => owed.badges(),
            None if replaced && d.status == "posted" => {
                f::badge("Replaced", BadgeKind::Neutral, true)?
            }
            None => f::status(&d.status)?,
        };

        let mut document = self.document_card(d)?;
        document.push(&self.lines_card(d, units)?);
        if let Some(matches) = self.match_card(d)? {
            document.push(&matches);
        }
        if let Some(events) = self.events_card(d, &display, &people)? {
            document.push(&events);
        }
        document.push(
            &self
                .credit_actions
                .body(&self.pages, d.id, &d.document_type, &d.settlements, units)
                .await?,
        );
        let mut summary = self.summary_card(d, status.clone(), owed, units, &display, &people)?;
        summary.push(&render(AttachmentsSlot)?);

        let mut parts = Vec::new();
        if let Some(attention) = self.attention(d)? {
            parts.push(attention);
        }
        parts.push(DocumentLayout::render(
            "payables-bill-layout",
            document,
            summary,
        )?);
        let mut title_actions = status;
        if let Some(status) = d.match_status.as_deref() {
            title_actions.push(&f::match_badge(status, "Match: ")?);
        }
        let dialog = match self.dialog {
            Some(Dialog::Void) => Some(DialogView::new("void", &self.reason)),
            Some(Dialog::ApproveException { .. }) => {
                Some(DialogView::new("approve-exception", &self.reason))
            }
            None => None,
        }
        .map(render)
        .transpose()?;
        View {
            flash,
            record: d.id,
            title: self.heading(d),
            subtitle: self.subtitle(d),
            title_actions,
            actions: self.header_actions(d, &can, back)?,
            parts,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// "Bill from Acme" under a numbered title; the supplier's own number
    /// while it has none of ours.
    fn subtitle(&self, d: &Document) -> String {
        match (d.document_number, d.party_document_number.as_deref()) {
            (Some(_), _) => format!("{} from {}", f::kind(&d.document_type), d.party.name),
            (None, Some(number)) => format!("Supplier's invoice {number}"),
            (None, None) => String::new(),
        }
    }

    /// Back, the local actions, the "Non-local actions" menu, then the one primary verb for this state:
    /// Approve a draft, Post an approved bill, Request payment while it is owed.
    fn header_actions(&self, d: &Document, can: &Can, back: Html) -> Result<Html> {
        let here = format!("{INDEX}/{}", d.id);
        let bill = d.document_type == "bill";
        let mut items = Vec::new();
        let mut primary = None;
        let event = |id: &str, event: &str, label: &str| {
            render(Button {
                id,
                button_type: "button",
                event,
                variant: ButtonVariant::Primary,
                body: Html::text(label),
                ..Button::default()
            })
        };
        match d.status.as_str() {
            "draft" => {
                if can.approve {
                    primary = Some(event("payables-bill-approve", "approve", "Approve")?);
                }
                if can.create && can.approve && bill && super::super::bill_form::corrigible(d) {
                    items.push(RecordAction::local(
                        "pencil-square",
                        "Correct draft",
                        format!("{INDEX}?correct={}", d.id),
                    ));
                }
            }
            "approved" | "held" => {
                if can.post {
                    primary = Some(event("payables-bill-post", "post", "Post")?);
                }
                if can.match_ && d.match_status.as_deref() == Some("held") {
                    items.push(RecordAction::here(
                        "check-badge",
                        "Approve exceptions",
                        &here,
                        "approve_exception",
                    ));
                }
            }
            "posted" if bill => {
                if can.pay && d.outstanding.is_some_and(|m| m.is_positive()) {
                    primary = Some(render(Button {
                        id: "payables-bill-pay",
                        href: &format!("{NEW_PAYMENT}?bill={}", d.id),
                        variant: ButtonVariant::Primary,
                        body: Html::text("Request payment"),
                        ..Button::default()
                    })?);
                }
                if can.create {
                    items.push(RecordAction::link(
                        "Record debit note",
                        format!("{INDEX}?debit_note={}", d.id),
                    ));
                }
            }
            _ => {}
        }
        if can.approve && matches!(d.status.as_str(), "draft" | "approved" | "held") {
            items.push(RecordAction::here("no-symbol", "Void", &here, "void"));
        }
        RecordActions::render(
            back,
            "payables-bill-actions",
            &self.heading(d),
            items,
            primary,
        )
    }

    /// The document as the supplier sent it.
    fn document_card(&self, d: &Document) -> Result<Html> {
        let mut items = vec![
            f::fact("Supplier", &d.party.name),
            f::fact(
                "Supplier's invoice number",
                f::optional(&d.party_document_number),
            ),
            f::fact(
                "Date",
                &bilibi_finance_ledger::dates::display(d.document_date),
            ),
        ];
        if d.document_type != "opening_credit" {
            items.push(f::fact(
                "Due date",
                &bilibi_finance_ledger::dates::display(d.due_date),
            ));
        }
        if d.description.is_some() {
            items.push(f::fact("Description", f::optional(&d.description)));
        }
        if let Some(record) = d.corrects_document_id {
            items.push(Fact {
                title: "Corrects",
                value: render(Corrects { record })?,
                ..Fact::default()
            });
        }
        if let Some(opening) = &d.opening {
            items.extend(self.opening_facts(opening));
        }
        let mut b = render(SectionHeading {
            id: "payables-bill-document-heading",
            title: f::kind(&d.document_type),
            ..SectionHeading::default()
        })?;
        b.push(&render(Facts {
            id: "payables-bill-document-facts",
            items,
        })?);
        b.push(&render(SectionHeading {
            id: "payables-bill-party-heading",
            title: "Supplier details",
            class: "mt-4",
            ..SectionHeading::default()
        })?);
        b.push(&render(Facts {
            id: "payables-bill-party-facts",
            items: vec![
                f::fact("Tax ID", f::optional(&d.party.tax_id)),
                f::fact("Registration number", f::optional(&d.party.registration_id)),
                f::fact("Address", f::optional(&d.party.address)),
            ],
        })?);
        render(Card {
            id: "payables-bill-facts",
            role: "region",
            labelledby: "payables-bill-document-heading",
            body: b,
            ..Card::default()
        })
    }

    fn opening_facts(&self, opening: &Value) -> Vec<Fact<'static>> {
        let text = |key: &str| opening[key].as_str().unwrap_or("").to_owned();
        let mut items = vec![
            f::fact(
                "Opening item",
                f::opening_item_kind(opening["item_kind"].as_str().unwrap_or("")),
            ),
            f::fact("Source system", &text("source_system")),
            f::fact("Source reference", &text("source_reference")),
            f::fact(
                "Conversion date",
                &bilibi_finance_ledger::dates::display_text(&text("conversion_date")),
            ),
        ];
        if let Some(amount) = opening["original_total"]
            .as_str()
            .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
        {
            items.push(f::fact(
                "Original total",
                &bilibi_finance_ledger::Money::display_decimal(amount, self.units),
            ));
        }
        if let Some(batch) = opening["replaced_by_batch_id"].as_i64() {
            items.push(f::fact(
                "Opening register",
                &format!("Replaced by register {batch}; kept as source evidence."),
            ));
        }
        items
    }

    fn lines_card(&self, d: &Document, units: Option<MinorUnits>) -> Result<Html> {
        let mut rows = Vec::new();
        for l in &d.lines {
            let mut row = FlexRow::new(l.id.to_string());
            row.dom_id = Some(format!("payables-line-{}", l.line_number));
            rows.push(
                row.cell("number", Cell::text(l.line_number.to_string()))
                    .cell("description", Cell::text(&l.description))
                    .slot("account", AccountName::label(&self.account(l.account_id))?)
                    .cell(
                        "order_line",
                        Cell::text(l.order_line_id.map_or_else(
                            || f::optional(&l.external_order_line_id).to_owned(),
                            |id| format!("#{id}"),
                        )),
                    )
                    .cell("quantity", Cell::text(f::decimal(l.quantity)))
                    .cell("amount", Cell::text(l.amount.display(units))),
            );
        }
        let mut b = render(SectionHeading {
            id: "payables-bill-lines-heading",
            title: "Lines",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        b.push(&table(&self.lines, "lines_grid", "Lines", rows)?);
        b.push(&DocumentTotals::render(
            "payables-bill-totals",
            vec![
                ("Net", d.total_net.display(units)),
                ("Tax", d.total_tax.display(units)),
                ("Total", amount(d.total.display(units), &d.currency_code)),
            ],
        )?);
        render(Card {
            id: "payables-bill-lines",
            role: "region",
            labelledby: "payables-bill-lines-heading",
            inner_class: "p-0",
            body: b,
            ..Card::default()
        })
    }

    fn account(&self, id: i64) -> String {
        self.accounts
            .iter()
            .find(|a| a.0 == id)
            .map(|a| a.1.clone())
            .unwrap_or_else(|| "Account unavailable".into())
    }

    /// What the last three-way match compared, line by line.
    fn match_card(&self, d: &Document) -> Result<Option<Html>> {
        let Some(e) = f::last_match(d) else {
            return Ok(None);
        };
        let mut rows = Vec::new();
        for l in &e.lines {
            let mut row = FlexRow::new(l.line_number.to_string());
            row.dom_id = Some(format!("payables-match-line-{}", l.line_number));
            let result = if l.exceptions.is_empty() {
                f::badge("Matched", BadgeKind::Success, true)?
            } else {
                render(MatchResult {
                    details: l.exceptions.iter().map(|e| e.detail.as_str()).collect(),
                    compact: self.matches.mode() == bilibi_ui::RowMode::Compact,
                })?
            };
            rows.push(
                row.cell("number", Cell::text(l.line_number.to_string()))
                    .cell(
                        "order",
                        Cell::text(format!(
                            "{} line #{}",
                            f::number(l.order_number),
                            l.order_line_id
                        )),
                    )
                    .cell(
                        "billed",
                        Cell::text(format!(
                            "{} at {}",
                            l.billed_quantity.normalize(),
                            l.billed_unit_price.normalize()
                        )),
                    )
                    .cell(
                        "ordered",
                        Cell::text(format!(
                            "{} at {}",
                            f::decimal(l.ordered_quantity),
                            f::decimal(l.ordered_unit_price)
                        )),
                    )
                    .cell("received", Cell::text(f::decimal(l.received_quantity)))
                    .cell(
                        "billed_before",
                        Cell::text(l.previously_billed_quantity.normalize().to_string()),
                    )
                    .cell(
                        "tolerances",
                        Cell::text(format!(
                            "qty {} · price {}",
                            f::tolerance(&l.quantity_tolerance),
                            f::tolerance(&l.price_tolerance)
                        )),
                    )
                    .slot("result", result),
            );
        }
        let mut b = render(SectionHeading {
            id: "payables-bill-match-heading",
            title: "Three-way match",
            class: "p-5 pb-0",
            description: "Billed against ordered and received, within the tolerances in force.",
            ..SectionHeading::default()
        })?;
        b.push(&table(&self.matches, "matches_grid", "Match lines", rows)?);
        Ok(Some(render(Card {
            id: "payables-bill-match",
            role: "region",
            labelledby: "payables-bill-match-heading",
            inner_class: "p-0",
            body: b,
            ..Card::default()
        })?))
    }

    fn events_card(
        &self,
        d: &Document,
        display: &DisplayContext,
        people: &People,
    ) -> Result<Option<Html>> {
        if d.match_events.is_empty() {
            return Ok(None);
        }
        let mut rows = Vec::new();
        for e in &d.match_events {
            let mut row = FlexRow::new(e.id.to_string());
            row.dom_id = Some(format!("payables-match-event-{}", e.id));
            rows.push(
                row.slot(
                    "when",
                    f::time(
                        display,
                        &format!("payables-match-event-at-{}", e.id),
                        &e.occurred_at,
                    )?,
                )
                .slot(
                    "event",
                    f::badge(&sentence(&e.event), BadgeKind::Neutral, false)?,
                )
                .cell("exceptions", Cell::text(f::exceptions(e).to_string()))
                .cell("reason", Cell::text(f::optional(&e.reason)))
                .cell(
                    "by",
                    Cell::text(people.who(Some(e.actor_user_id), e.actor_impersonator_id)),
                ),
            );
        }
        let mut b = render(SectionHeading {
            id: "payables-bill-match-events-heading",
            title: "Match history",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        b.push(&table(&self.events, "events_grid", "Match events", rows)?);
        Ok(Some(render(Card {
            id: "payables-bill-match-events",
            role: "region",
            labelledby: "payables-bill-match-events-heading",
            inner_class: "p-0",
            body: b,
            ..Card::default()
        })?))
    }

    /// Where the bill stands: status, what is still owed, its journal and
    /// who recorded, approved and posted it.
    fn summary_card(
        &self,
        d: &Document,
        status: Html,
        owed: Option<Owed>,
        units: Option<MinorUnits>,
        display: &DisplayContext,
        people: &People,
    ) -> Result<Html> {
        let stamp = |title: &'static str,
                     user: Option<i64>,
                     impersonator: Option<i64>,
                     at: Option<&str>,
                     element: &str|
         -> Result<Fact<'static>> {
            Ok(Fact {
                title,
                value: f::actor_at(display, people, user, impersonator, at, element, " · ")?,
                ..Fact::default()
            })
        };
        let mut items = vec![Fact {
            title: "Status",
            value: status,
            ..Fact::default()
        }];
        if let Some(pins) = self.balance_pins.as_object() {
            items.push(f::fact(
                "Balance as of",
                &bilibi_finance_ledger::dates::display_text(
                    pins.get("as_of").and_then(Value::as_str).unwrap_or(""),
                ),
            ));
        }
        if let Some(outstanding) = d.outstanding {
            let mut due = amount(outstanding.display(units), &d.currency_code);
            if let Some(late) = owed.and_then(|o| o.overdue_by()) {
                due = format!("{due} · {late}");
            }
            items.push(f::fact(
                if d.document_type == "opening_credit" {
                    "Available credit"
                } else {
                    "Amount due"
                },
                &due,
            ));
        }
        items.push(f::fact(
            "Total",
            &amount(d.total.display(units), &d.currency_code),
        ));
        if d.entry_id.is_some() {
            items.push(Fact {
                title: "Journal",
                value: EntryLink::html(d.entry_id, d.entry_number)?,
                ..Fact::default()
            });
        }
        if let Some(id) = d.payable_account_id {
            items.push(Fact {
                title: "Payable account",
                value: AccountName::label(&self.account(id))?,
                ..Fact::default()
            });
        }
        items.push(stamp(
            "Recorded",
            Some(d.created_by_user_id),
            d.created_by_impersonator_id,
            Some(&d.created_at),
            "payables-bill-created-at",
        )?);
        if d.approved_by_user_id.is_some() {
            items.push(stamp(
                "Approved",
                d.approved_by_user_id,
                d.approved_by_impersonator_id,
                d.approved_at.as_deref(),
                "payables-bill-approved-at",
            )?);
        }
        if d.posted_by_user_id.is_some() {
            items.push(stamp(
                "Posted",
                d.posted_by_user_id,
                d.posted_by_impersonator_id,
                d.posted_at.as_deref(),
                "payables-bill-posted-at",
            )?);
        }
        if d.status == "voided" {
            items.push(stamp(
                "Voided",
                d.voided_by_user_id,
                d.voided_by_impersonator_id,
                d.voided_at.as_deref(),
                "payables-bill-voided-at",
            )?);
            items.push(f::fact("Void reason", f::optional(&d.void_reason)));
        }
        let mut b = render(SectionHeading {
            id: "payables-bill-summary-heading",
            title: "Summary",
            ..SectionHeading::default()
        })?;
        b.push(&render(Facts {
            id: "payables-bill-summary",
            items,
        })?);
        if self
            .flash
            .as_ref()
            .is_some_and(|(_, message)| message.contains("separation of duties"))
        {
            b.push(&render(Button {
                id: "payables-approval-settings",
                href: "/finance/reports/approval-exceptions",
                body: Html::text("Approval exceptions"),
                ..Button::default()
            })?);
        }
        render(Card {
            id: "payables-bill-summary-card",
            role: "region",
            labelledby: "payables-bill-summary-heading",
            body: b,
            ..Card::default()
        })
    }

    /// The hold the three-way match put on the bill, above everything else.
    fn attention(&self, d: &Document) -> Result<Option<Html>> {
        if !matches!(
            d.match_status.as_deref(),
            Some("held" | "exception_approved")
        ) {
            return Ok(None);
        }
        let last_match = f::last_match(d);
        Ok(Some(render(Attention {
            approved: d.match_status.as_deref() == Some("exception_approved"),
            has_match: last_match.is_some(),
            exceptions: last_match
                .into_iter()
                .flat_map(|e| &e.lines)
                .flat_map(|l| {
                    l.exceptions.iter().map(move |e| ExceptionView {
                        line: l.line_number,
                        code: e.code.clone(),
                        detail: e.detail.clone(),
                    })
                })
                .collect(),
        })?))
    }
}

fn amount(text: String, currency: &str) -> String {
    format!("{text} {currency}")
}

/// A stored word ("exception_approved") as a sentence-case label.
fn sentence(word: &str) -> String {
    let text = word.replace('_', " ");
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[derive(Template)]
#[template(path = "bill.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    record: i64,
    title: String,
    subtitle: String,
    title_actions: Html,
    actions: Html,
    parts: Vec<Html>,
    dialog: Option<Html>,
}

#[derive(Template)]
#[template(
    source = r#"<div id="payables-bill-attachments-slot" data-live-ignore><div id="payables-bill-attachments"></div></div>"#,
    ext = "html"
)]
struct AttachmentsSlot;
