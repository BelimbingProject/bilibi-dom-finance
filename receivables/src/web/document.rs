//! One invoice or credit note: header, party, lines, tax and posting status,
//! the receipts and credits applied to it, and its source documents. A draft
//! edits in place (dates and description) and its lines open in a dialog; it
//! is issued or discarded after a confirmation. Reads `document.get`; writes
//! call `draft.update`, `draft.discard` and `document.issue`. The page is
//! drawn in `document/view.rs`; an address with `?do=discard` or
//! `?do=write_off` (its local icon buttons) opens that dialog.
mod view;

use super::{
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written,
};
use crate::{
    Customer, Document as Record, Payment, Series, TaxCode, ISSUE, MANAGE_DOCUMENTS, POST_PAYMENTS,
};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{PartyReview, RecordActions};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, Button, ButtonVariant, Choice, Combobox, CommitStates, CommitStatus, Field, Form, Html,
    InputKind, ListColumns, ModalWidth, SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

enum Dialog {
    Issue,
    Discard,
}

/// One line of the lines dialog, as typed.
#[derive(Clone, Default)]
struct Line {
    description: String,
    quantity: String,
    unit_price: String,
    account_id: String,
    tax_code_id: String,
}

struct Lines {
    rows: Vec<Line>,
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
    dialog: Option<Dialog>,
    issue_review: Option<Value>,
    issue_banner: Html,
    editing: Option<Lines>,
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
            dialog: None,
            issue_review: None,
            issue_banner: Html::default(),
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
                if self.editable() {
                    self.dialog = Some(Dialog::Discard);
                }
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
            self.credit_actions
                .load(&self.pages, scope, d.id, &d.kind)
                .await?;
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

    fn lines_dialog(&self, lines: &Lines) -> Result<Html> {
        let mut labels = vec![];
        for i in 0..lines.rows.len() {
            for (key, label) in [
                ("description", "Description"),
                ("quantity", "Quantity"),
                ("unit_price", "Unit price"),
                ("account_id", "Account"),
                ("tax_code_id", "Tax code"),
            ] {
                labels.push((
                    format!("lines.{i}.{key}"),
                    format!("receivables-line-{key}-{i}"),
                    format!("Line {} {}", i + 1, label.to_lowercase()),
                ));
            }
        }
        let mut body = render(lines.errors.summary("receivables-lines-errors", &labels))?;
        let accounts: Vec<Choice> = self
            .accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == "income")
            .map(Account::choice)
            .collect();
        let taxes: Vec<Choice> = self
            .tax_codes
            .iter()
            .filter(|t| t.active)
            .map(|t| {
                Choice::new(
                    format!("{} ({}%)", t.code, f::decimal(t.rate)),
                    t.id.to_string(),
                )
            })
            .collect();
        for (i, line) in lines.rows.iter().enumerate() {
            let err =
                |key: &str, label: &str| lines.errors.under(&format!("lines.{i}.{key}"), label);
            body.push(&render(SectionHeading {
                title: &format!("Line {}", i + 1),
                actions: render(Button {
                    id: &format!("receivables-line-remove-{i}"),
                    button_type: "button",
                    event: "remove_line",
                    name: "index",
                    value: &i.to_string(),
                    body: Html::text("Remove"),
                    ..Button::default()
                })?,
                ..SectionHeading::default()
            })?);
            body.push(&render(
                Field::in_form("line", &format!("description_{i}"))
                    .id(format!("receivables-line-description-{i}"))
                    .label("Description")
                    .attr("maxlength", "500")
                    .value(line.description.clone())
                    .errors(err("description", "Description")),
            )?);
            let mut row = Html::EMPTY;
            row.push(&render(
                Field::in_form("line", &format!("quantity_{i}"))
                    .id(format!("receivables-line-quantity-{i}"))
                    .label("Quantity")
                    .attr("inputmode", "decimal")
                    .value(line.quantity.clone())
                    .errors(err("quantity", "Quantity")),
            )?);
            row.push(&render(
                Field::in_form("line", &format!("unit_price_{i}"))
                    .id(format!("receivables-line-unit_price-{i}"))
                    .label("Unit price")
                    .attr("inputmode", "decimal")
                    .value(line.unit_price.clone())
                    .errors(err("unit_price", "Unit price")),
            )?);
            row.push(&render(
                Combobox::new(
                    format!("receivables-line-account_id-{i}"),
                    format!("line[account_id_{i}]"),
                )
                .label("Account")
                .placeholder("Company default")
                .options(accounts.clone())
                .value(line.account_id.clone())
                .errors(err("account_id", "Account")),
            )?);
            row.push(&render(
                Field::in_form("line", &format!("tax_code_id_{i}"))
                    .id(format!("receivables-line-tax_code_id-{i}"))
                    .label("Tax code")
                    .kind(InputKind::Select)
                    .prompt("No tax")
                    .options(taxes.clone())
                    .value(line.tax_code_id.clone())
                    .errors(err("tax_code_id", "Tax code")),
            )?);
            body.push(&f::four(row)?);
        }
        let form = render(Form {
            id: "receivables-lines-form",
            event: "lines_change",
            change: true,
            class: "mt-4",
            body,
            actions: Html::EMPTY,
        })?;
        let mut buttons = render(Button {
            id: "receivables-lines-add",
            button_type: "button",
            event: "add_line",
            body: Html::text("Add line"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "receivables-lines-cancel",
            button_type: "button",
            event: "cancel_lines",
            class: "ml-auto",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        buttons.push(&render(Button {
            id: "receivables-lines-save",
            button_type: "button",
            event: "save_lines",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save lines"),
            ..Button::default()
        })?);
        let mut inner = form;
        inner.push(&f::buttons(buttons)?);
        f::modal(
            "receivables-lines",
            "Edit lines",
            "Each line is quantity times unit price, rounded to the book's smallest unit. Saving replaces all the lines.",
            "cancel_lines",
            ModalWidth::Wide,
            &self.flash,
            inner,
        )
    }

    /// Fold the dialog's typed values into the draft lines.
    fn take_lines(&mut self, v: &Value) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let form = params::form_values(v, "line");
        for (i, line) in editing.rows.iter_mut().enumerate() {
            let get = |key: &str| form.get(&format!("{key}_{i}")).cloned();
            if let Some(t) = get("description") {
                line.description = t
            }
            if let Some(t) = get("quantity") {
                line.quantity = t
            }
            if let Some(t) = get("unit_price") {
                line.unit_price = t
            }
            if let Some(t) = get("account_id") {
                line.account_id = t
            }
            if let Some(t) = get("tax_code_id") {
                line.tax_code_id = t
            }
        }
    }

    fn lines_input(&self) -> Value {
        let rows = self
            .editing
            .as_ref()
            .map(|e| e.rows.as_slice())
            .unwrap_or(&[]);
        let lines: Vec<Value> = rows
            .iter()
            .map(|l| {
                let mut line = json!({
                    "description": l.description.trim(),
                    "quantity": l.quantity.trim(),
                    "unit_price": l.unit_price.trim(),
                });
                if let Ok(id) = l.account_id.parse::<i64>() {
                    line["account_id"] = json!(id)
                }
                if let Ok(id) = l.tax_code_id.parse::<i64>() {
                    line["tax_code_id"] = json!(id)
                }
                line
            })
            .collect();
        json!({"document_id": self.id, "lines": lines})
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
        match event {
            "ask_credit_apply" | "ask_credit_refund" | "ask_credit_reverse" | "cancel_credit"
            | "change_credit" | "preview_credit" | "commit_credit" | "credit_grid" => {
                self.credit_actions
                    .event(&self.pages, scope, credential, v, self.id)
                    .await?;
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
                self.dialog = None;
                self.issue_review = None;
            }
            "ask_issue" => {
                if self.can_issue && self.draft() {
                    let mut input = json!({"document_id":self.id});
                    input["company_id"] = json!(Pages::company(scope));
                    match self
                        .pages
                        .party_picker()
                        .review_core(
                            scope,
                            credential,
                            "ask_issue",
                            "finance.receivables.document.issue",
                            input,
                        )
                        .await?
                    {
                        PartyReview::Valid { review } => {
                            self.issue_banner = self
                                .pages
                                .plans
                                .instance()
                                .await?
                                .banner("receivables-issue-instance", "mb-4")
                                .map_err(|_| Error::Internal)?;
                            self.issue_review = Some(review);
                            self.dialog = Some(Dialog::Issue);
                        }
                        PartyReview::Invalid(fields) => {
                            self.flash.set(StatusKind::Error, f::sentences(&fields))
                        }
                        PartyReview::Refused(message) => self.flash.set(StatusKind::Error, message),
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
                    self.dialog = Some(Dialog::Discard);
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to discard drafts.",
                    );
                }
            }
            "issue" | "discard" => {
                let issue = v["type"] == "issue";
                self.dialog = None;
                let (event, contract) = if issue {
                    ("issue", "finance.receivables.document.issue")
                } else {
                    ("discard", "finance.receivables.draft.discard")
                };
                let written = if issue {
                    let Some(_) = self.issue_review.take() else {
                        self.flash
                            .set(StatusKind::Error, "Review this invoice before issuing it.");
                        return Ok(());
                    };
                    let input = json!({"company_id":Pages::company(scope),"document_id":self.id});
                    let answer = self
                        .pages
                        .contracts
                        .commit(&Pages::call(scope, credential, event, contract), input)
                        .await
                        .map(|mut value| value["results"][0]["result"].take());
                    Pages::written(contract, event, answer)?
                } else {
                    self.pages
                        .write(
                            scope,
                            credential,
                            event,
                            contract,
                            json!({"document_id":self.id}),
                        )
                        .await?
                };
                match written {
                    Written::Done(record) => {
                        self.flash.set(
                            StatusKind::Success,
                            if issue {
                                match record["document_reference"].as_str() {
                                    Some(r) => format!("Issued as {r} and posted to the ledger."),
                                    None => "Issued and posted to the ledger.".into(),
                                }
                            } else {
                                "Draft discarded.".to_owned()
                            },
                        );
                    }
                    Written::Invalid(fields) => {
                        let message = f::sentences(&fields);
                        self.flash.set(
                            StatusKind::Error,
                            if message.is_empty() {
                                "That could not be done.".into()
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
                    let mut rows: Vec<Line> = d
                        .lines
                        .iter()
                        .map(|l| Line {
                            description: l.description.clone(),
                            quantity: f::decimal(l.quantity),
                            unit_price: f::price(l.unit_price),
                            account_id: l.account_id.to_string(),
                            tax_code_id: l.tax_code_id.map(|i| i.to_string()).unwrap_or_default(),
                        })
                        .collect();
                    if rows.is_empty() {
                        rows.push(Line::default());
                    }
                    self.editing = Some(Lines {
                        rows,
                        errors: Errors::default(),
                    });
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to change this draft.",
                    );
                }
            }
            "lines_change" => self.take_lines(v),
            "add_line" => {
                if let Some(e) = &mut self.editing {
                    e.rows.push(Line::default());
                }
            }
            "remove_line" => {
                let index = v["index"].as_str().and_then(|i| i.parse::<usize>().ok());
                if let (Some(e), Some(i)) = (&mut self.editing, index) {
                    if i < e.rows.len() {
                        e.rows.remove(i);
                    }
                }
            }
            "cancel_lines" => self.editing = None,
            "save_lines" => {
                if self.editing.is_some() {
                    let input = self.lines_input();
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
                        Written::Invalid(fields) => {
                            if let Some(e) = &mut self.editing {
                                e.errors = Errors(fields);
                            }
                        }
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
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: super::credits::balance_params(&self.balance_pins),
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

#[derive(askama::Template)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% call ui::confirm_dialog(id="receivables-invoice-issue-confirm", consequence=consequence, detail=detail, confirm="Issue", working="Issuing…", confirm_event="issue", cancel_event="cancel_dialog", body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct IssueConfirmation {
    consequence: String,
    detail: String,
    body: Html,
}
