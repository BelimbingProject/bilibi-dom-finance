//! One invoice or credit note: header, party, lines, tax and posting status,
//! the receipts and credits applied to it, and its source documents. A draft
//! edits in place (dates through the shared date box, and the description)
//! and its lines open in a dialog (`lines.rs`, the editor New invoice uses);
//! it is issued (`issue.rs`) or discarded after a confirmation. Reads
//! `document.get`; writes call `draft.update`, `draft.discard` and
//! `document.issue`. The page is drawn in `document/view.rs`; an address with
//! `?do=discard` or `?do=write_off` (its local icon buttons) opens that dialog.
mod view;

use super::{
    format::{self as f, Errors},
    issue::{self, Asked, Issue},
    lines::Lines,
    render, Account, Flash, Pages, Written, INVOICES,
};
use crate::{
    Customer, Document as Record, Payment, Series, TaxCode, ISSUE, MANAGE_DOCUMENTS, POST_PAYMENTS,
};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::RecordActions;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, Button, ButtonVariant, CommitStates, CommitStatus, Form, Html, ListColumns, ModalWidth,
    StatusKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// The draft's lines being edited, and what the last save was refused for.
struct Editing {
    lines: Lines,
    errors: Errors,
}

pub(super) struct Document {
    pages: Pages,
    id: i64,
    balance_input: Value,
    balance_pins: Value,
    record: Option<Record>,
    customer: Option<Customer>,
    series: Option<Series>,
    accounts: Vec<Account>,
    tax_codes: Vec<TaxCode>,
    /// Receipts that settle this invoice, and credit notes that credit it.
    receipts: Vec<Payment>,
    credits: Vec<Record>,
    figures: f::Figures,
    can_manage: bool,
    can_issue: bool,
    can_receive: bool,
    /// The reference of the invoice a credit note credits.
    credited: Option<String>,
    /// The dialog an address asked for (`?do=`), opened once the record loads.
    requested: Option<String>,
    statuses: CommitStates,
    flash: Flash,
    discarding: bool,
    /// The reviewed issue that waits for the person's confirmation.
    issuing: Option<Issue>,
    /// Where to go once this event is answered: the page itself after an
    /// issue, so the browser tab takes the document's new name.
    redirect: Option<String>,
    editing: Option<Editing>,
    lines_cols: ListColumns,
    tax_cols: ListColumns,
    applied_cols: ListColumns,
    write_offs: super::write_offs::Journals,
    credit_actions: super::credits::Credits,
}

impl Document {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            write_offs: super::write_offs::Journals::new()?,
            credit_actions: super::credits::Credits::new()?,
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            balance_input: super::credits::balance_query(query)?,
            balance_pins: Value::Null,
            record: None,
            customer: None,
            series: None,
            accounts: vec![],
            tax_codes: vec![],
            receipts: vec![],
            credits: vec![],
            figures: f::Figures::default(),
            can_manage: false,
            can_issue: false,
            can_receive: false,
            credited: None,
            requested: RecordActions::requested(query),
            statuses: CommitStates::default(),
            flash: Flash::default(),
            discarding: false,
            issuing: None,
            redirect: None,
            editing: None,
            lines_cols: f::columns(
                "receivables-invoice-lines-table",
                &[
                    ("number", "#"),
                    ("description", "Description"),
                    ("quantity", "Quantity"),
                    ("unit_price", "Unit price"),
                    ("account", "Account"),
                    ("tax", "Tax code"),
                    ("tax_amount", "Tax"),
                    ("amount", "Amount"),
                ],
                &["quantity", "unit_price", "tax_amount", "amount"],
            )?,
            tax_cols: f::columns(
                "receivables-invoice-tax-table",
                &[
                    ("code", "Tax code"),
                    ("rate", "Rate"),
                    ("taxable", "Taxable amount"),
                    ("tax", "Tax"),
                ],
                &["rate", "taxable", "tax"],
            )?,
            applied_cols: f::columns(
                "receivables-invoice-applied-table",
                &[
                    ("document", "Document"),
                    ("date", "Date"),
                    ("amount", "Amount"),
                    ("status", "Status"),
                ],
                &["amount"],
            )?,
        })
    }

    pub(super) fn title(&self) -> Option<String> {
        self.record.as_ref().map(|d| {
            format!(
                "{} {}",
                f::kind_label(&d.kind),
                super::documents::reference(d)
            )
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.mounted(scope).await?;
        self.body(scope).await
    }

    /// Load, then open the dialog the address asked for, so the live page
    /// starts as the first render showed it.
    pub(super) async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        self.load(scope).await?;
        if let Some(action) = self.requested.take() {
            self.open(scope, &action).await?;
        }
        Ok(())
    }

    /// A local action, reached through its icon button's link.
    async fn open(&mut self, scope: &Scope, action: &str) -> Result<()> {
        match action {
            "discard" => {
                self.discarding = self.editable();
            }
            "write_off" => {
                self.write_offs
                    .event(
                        &self.pages,
                        scope,
                        "",
                        &json!({"type": "ask_submit_write_off"}),
                        self.record.as_ref(),
                        &mut self.flash,
                    )
                    .await?
            }
            "apply_credit" | "refund_credit" => {
                if let Some(d) = self.record.clone() {
                    let action = action.trim_end_matches("_credit");
                    if let Some((kind, message)) = self
                        .credit_actions
                        .ask(&self.pages, scope, action, &d, "")
                        .await
                    {
                        self.flash.set(kind, message);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.receipts.clear();
        self.credits.clear();
        let mut input = self.balance_input.clone();
        input["document_id"] = json!(self.id);
        self.record = match self
            .pages
            .read(scope, "finance.receivables.document.get", input)
            .await
        {
            Ok(mut value) => {
                self.balance_pins = value["balance_pins"].clone();
                if self.balance_pins.is_object() {
                    self.balance_input = self.balance_pins.clone();
                    self.receipts = serde_json::from_value(value["receipts"].take())
                        .map_err(|_| Error::Internal)?;
                    self.credits = serde_json::from_value(value["credits"].take())
                        .map_err(|_| Error::Internal)?;
                }
                serde_json::from_value(value).ok()
            }
            Err(Error::NotFound) => None,
            Err(error) => return Err(error),
        };
        let historical = self.balance_pins.is_object();
        if historical {
            self.write_offs = super::write_offs::Journals::new()?;
            self.credit_actions = super::credits::Credits::new()?;
            self.can_manage = false;
            self.can_issue = false;
        } else {
            self.write_offs.load(&self.pages, scope).await?;
            self.can_manage = self.pages.allowed(scope, MANAGE_DOCUMENTS).await?;
            self.can_issue = self.pages.allowed(scope, ISSUE).await?;
            self.can_receive = self.pages.allowed(scope, POST_PAYMENTS).await?;
        }
        let Some(d) = &self.record else { return Ok(()) };
        if !historical {
            self.credit_actions.load(&self.pages, scope, d).await?;
        }
        let customer_id = d.customer_id;
        let series_id = d.series_id;
        let credited = d.credited_document_id;
        let issued = d.status == "issued";
        self.figures = self.pages.figures(scope).await;
        self.customer = self
            .pages
            .read(
                scope,
                "finance.receivables.customer.get",
                json!({"customer_id": customer_id}),
            )
            .await
            .ok()
            .and_then(|v| serde_json::from_value(v).ok());
        let series: Vec<Series> = self
            .pages
            .read_all(
                scope,
                "finance.receivables.series.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        self.series = series.into_iter().find(|s| s.id == series_id);
        self.tax_codes = self
            .pages
            .read_all(
                scope,
                "finance.receivables.tax_code.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        self.accounts = self.pages.accounts(scope).await;
        self.credited = match credited {
            Some(id) => self
                .pages
                .read(
                    scope,
                    "finance.receivables.document.get",
                    json!({"document_id": id}),
                )
                .await
                .ok()
                .and_then(|v| v["document_reference"].as_str().map(str::to_owned)),
            None => None,
        };
        if issued && !historical {
            let id = self.id;
            let payments: Vec<Payment> = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.payment.list",
                    json!({"customer_id": customer_id}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.receipts = payments
                .into_iter()
                .filter(|p| p.allocations.iter().any(|a| a.document_id == id))
                .collect();
            let documents: Vec<Record> = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.document.list",
                    json!({"customer_id": customer_id, "kind": "credit_note", "status": "issued"}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.credits = documents
                .into_iter()
                .filter(|c| c.credited_document_id == Some(id))
                .collect();
        }
        Ok(())
    }

    fn draft(&self) -> bool {
        self.record.as_ref().is_some_and(|d| d.status == "draft")
    }

    fn editable(&self) -> bool {
        self.draft() && self.can_manage
    }

    fn tax_code(&self, id: Option<i64>) -> String {
        id.map_or_else(
            || "—".into(),
            |id| {
                self.tax_codes
                    .iter()
                    .find(|t| t.id == id)
                    .map_or_else(|| format!("Tax code #{id}"), |t| t.code.clone())
            },
        )
    }

    fn lines_dialog(&self, editing: &Editing) -> Result<Html> {
        let mut body = render(
            editing
                .errors
                .summary("receivables-lines-errors", &editing.lines.labels()),
        )?;
        body.push(&editing.lines.body(
            &editing.errors,
            &self.accounts,
            &self.tax_codes,
            &self.figures,
        )?);
        let mut actions = render(Button {
            id: "receivables-lines-cancel",
            button_type: "button",
            event: "cancel_lines",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "receivables-lines-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save lines"),
            ..Button::default()
        })?);
        let form = Form {
            id: "receivables-lines-form",
            event: "lines_change",
            change: true,
            class: "mt-4",
            body,
            actions: f::actions(actions)?,
        }
        .submit_event("save_lines")
        .map_err(|_| Error::Internal)?;
        f::modal(
            "receivables-lines",
            "Edit lines",
            "",
            "cancel_lines",
            ModalWidth::Wide,
            &self.flash,
            form,
        )
    }

    async fn save_field(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let key = v["field"].as_str().ok_or(Error::Invalid)?;
        let label = match key {
            "issue_date" => "Issue date",
            "due_date" => "Due date",
            "description" => "Description",
            _ => return Err(Error::Invalid),
        };
        if v["id"].as_str() != Some(&self.id.to_string()) {
            return Err(Error::Invalid);
        }
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if !self.editable() {
            self.statuses.put(
                key,
                CommitStatus::Error("This document can no longer be changed.".into()),
            );
            return Ok(());
        }
        let written = self
            .pages
            .write(
                scope,
                credential,
                "save_draft",
                "finance.receivables.draft.update",
                json!({"document_id": self.id, key: submitted}),
            )
            .await?;
        self.statuses
            .put(key, f::commit_status(&written, key, label, &submitted));
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        if self.balance_pins.is_object()
            && !matches!(
                event,
                "clear-flash"
                    | "lines_grid"
                    | "tax_grid"
                    | "applied_grid"
                    | "write_off_grid"
                    | "credit_grid"
            )
        {
            return Err(Error::Invalid);
        }
        // A message answers the action before it: it does not follow the
        // person into the next dialog or the next action.
        if !matches!(
            event,
            "clear-flash"
                | "lines_grid"
                | "tax_grid"
                | "applied_grid"
                | "write_off_grid"
                | "credit_grid"
                | "lines_change"
        ) {
            self.flash.0 = None;
        }
        match event {
            "ask_credit_apply" | "ask_credit_refund" | "ask_credit_reverse" | "cancel_credit"
            | "back_credit" | "change_credit" | "preview_credit" | "commit_credit"
            | "credit_grid" => {
                if let Some(d) = self.record.clone() {
                    if let Some((kind, message)) = self
                        .credit_actions
                        .event(&self.pages, scope, credential, v, &d)
                        .await?
                    {
                        self.flash.set(kind, message);
                    }
                }
            }

            "ask_submit_write_off"
            | "ask_approve_write_off"
            | "ask_reject_write_off"
            | "ask_reverse_write_off"
            | "save_write_off"
            | "cancel_write_off"
            | "write_off_grid" => {
                self.write_offs
                    .event(
                        &self.pages,
                        scope,
                        credential,
                        v,
                        self.record.as_ref(),
                        &mut self.flash,
                    )
                    .await?
            }
            "clear-flash" => self.flash.dismiss(v),
            "lines_grid" => {
                self.lines_cols.handle(&params::string_values(v));
            }
            "tax_grid" => {
                self.tax_cols.handle(&params::string_values(v));
            }
            "applied_grid" => {
                self.applied_cols.handle(&params::string_values(v));
            }
            "save_draft" => self.save_field(scope, credential, v).await?,
            "cancel_dialog" => {
                self.discarding = false;
                self.issuing = None;
            }
            "ask_issue" => {
                if self.can_issue && self.draft() {
                    match Issue::ask(&self.pages, scope, credential, "ask_issue", self.id).await? {
                        Asked::Ready(issuing) => self.issuing = Some(issuing),
                        Asked::Refused(message) => self.flash.set(StatusKind::Error, message),
                    }
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to issue documents.",
                    );
                }
            }
            "ask_discard" => {
                if self.editable() {
                    self.discarding = true;
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to discard drafts.",
                    );
                }
            }
            "issue" | "discard" => {
                let issue = event == "issue";
                self.discarding = false;
                let written = if issue {
                    let Some(issuing) = self.issuing.take() else {
                        self.flash
                            .set(StatusKind::Error, "Review this invoice before issuing it.");
                        return Ok(());
                    };
                    issuing
                        .commit(&self.pages, scope, credential, "issue")
                        .await?
                } else {
                    self.pages
                        .write(
                            scope,
                            credential,
                            "discard",
                            "finance.receivables.draft.discard",
                            json!({"document_id":self.id}),
                        )
                        .await?
                };
                match written {
                    Written::Done(record) => {
                        if issue {
                            // The page is opened again under its new name, so
                            // the browser tab says INV-00006, not "Draft #9".
                            self.redirect = Some(format!("{INVOICES}/{}", self.id));
                            self.flash.set(StatusKind::Success, issue::issued(&record));
                        } else {
                            self.flash.set(StatusKind::Success, "Draft discarded.");
                        }
                    }
                    Written::Invalid(fields) => {
                        let message = f::sentences(&fields);
                        self.flash.set(
                            StatusKind::Error,
                            if message.is_empty() {
                                "Nothing was changed. Reload the page and try again.".into()
                            } else {
                                message
                            },
                        );
                    }
                    Written::Refused(message) => self.flash.set(
                        StatusKind::Error,
                        super::worded(
                            message,
                            if issue {
                                "You do not have permission to issue documents."
                            } else {
                                "You do not have permission to discard drafts."
                            },
                        ),
                    ),
                }
            }
            "edit_lines" => {
                if let Some(d) = self.record.as_ref().filter(|_| self.editable()) {
                    self.editing = Some(Editing {
                        lines: Lines::of(&d.lines),
                        errors: Errors::default(),
                    });
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to change this draft.",
                    );
                }
            }
            "lines_change" => {
                if let Some(editing) = &mut self.editing {
                    editing.lines.read(v);
                }
            }
            "add_line" | "remove_line" | "paste_open" | "paste_close" | "paste_lines" => {
                if let Some(editing) = &mut self.editing {
                    editing.lines.event(v);
                }
            }
            "cancel_lines" => self.editing = None,
            "save_lines" => {
                if let Some(editing) = &mut self.editing {
                    editing.lines.read(v);
                    let input = json!({"document_id": self.id, "lines": editing.lines.input()});
                    match self
                        .pages
                        .write(
                            scope,
                            credential,
                            "save_lines",
                            "finance.receivables.draft.update",
                            input,
                        )
                        .await?
                    {
                        Written::Done(_) => {
                            self.editing = None;
                            self.flash.set(StatusKind::Success, "Lines saved.");
                        }
                        Written::Invalid(fields) => editing.errors = Errors(fields),
                        Written::Refused(message) => {
                            self.editing = None;
                            self.flash.set(
                                StatusKind::Error,
                                super::worded(
                                    message,
                                    "You do not have permission to change this draft.",
                                ),
                            );
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<div id="receivables-invoice-attachments-slot" data-live-ignore><div id="receivables-invoice-attachments"></div></div>"#,
    ext = "html"
)]
struct AttachmentsSlot;

fn attachments_slot() -> Result<Html> {
    render(AttachmentsSlot)
}

impl LiveState<Scope, Error> for Document {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Event(v) => {
                    self.event(r.scope, r.credential, v).await?;
                }
                LiveInput::Params(query) => {
                    self.balance_input = super::credits::balance_query(query)?;
                    self.balance_pins = Value::Null;
                    self.requested = RecordActions::requested(query);
                }
                LiveInput::Refresh => {}
            }
            self.load(r.scope).await?;
            if let Some(action) = self.requested.take() {
                self.open(r.scope, &action).await?;
            }
            if self.record.is_none() {
                self.flash
                    .set(StatusKind::Error, "That document is no longer available.");
            }
            let redirect = self.redirect.take();
            let flash = redirect.is_some().then(|| self.flash.0.take()).flatten();
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: super::credits::balance_params(&self.balance_pins),
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }

    fn panels(&self) -> Vec<PanelSpec> {
        if self.record.is_none() || self.balance_pins.is_object() {
            return vec![];
        }
        vec![PanelSpec {
            id: "receivables-invoice-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "receivables-invoice-attachments".into()),
                ("source".into(), "finance/receivables/document".into()),
                ("document".into(), self.id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}
