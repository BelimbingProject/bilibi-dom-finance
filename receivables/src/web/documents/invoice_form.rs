//! The New invoice and New credit note dialog of the Invoices list: customer,
//! dates, lines and live totals together, as New bill is in Payables. Save
//! draft calls `draft.create`. Issue saves the same draft, then asks for the
//! `document.issue` review and shows its confirmation here, so an invoice is
//! entered and issued without leaving the list; a refused or cancelled issue
//! opens the saved draft's page, where nothing typed is lost.
use super::{Documents, Errors};
use crate::web::{
    format as f,
    issue::{self, Asked, Issue},
    lines::Lines,
    render, Written, INVOICES, SETTINGS,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    Alert, Attrs, Button, ButtonVariant, Choice, Combobox, Field, Form, Html, InputKind,
    ModalWidth, SectionHeading, StatusKind,
};
use serde_json::{json, Value};

const FORM: &str = "draft";

/// What the person has typed into the dialog.
pub(in crate::web) struct InvoiceForm {
    pub kind: &'static str,
    pub customer: String,
    pub credited: String,
    series: String,
    issue_date: String,
    due_date: String,
    description: String,
    lines: Lines,
    errors: Errors,
    /// A refusal of the whole save, shown in the dialog's own alert.
    notice: Option<String>,
}

impl InvoiceForm {
    /// A new document dated today; `series` is the one series that numbers
    /// this kind, when there is exactly one.
    pub(super) fn new(kind: &'static str, today: String, series: Option<i64>) -> Self {
        Self {
            kind,
            customer: String::new(),
            credited: String::new(),
            series: series.map(|id| id.to_string()).unwrap_or_default(),
            issue_date: today,
            due_date: String::new(),
            description: String::new(),
            lines: Lines::new(),
            errors: Errors::default(),
            notice: None,
        }
    }

    fn credit(&self) -> bool {
        self.kind == "credit_note"
    }

    fn read(&mut self, v: &Value) {
        for (key, into) in [
            ("customer_id", &mut self.customer),
            ("credited_document_id", &mut self.credited),
            ("series_id", &mut self.series),
            ("issue_date", &mut self.issue_date),
            ("due_date", &mut self.due_date),
            ("description", &mut self.description),
        ] {
            if let Some(text) = v[format!("{FORM}[{key}]")].as_str() {
                *into = text.chars().take(1024).collect();
            }
        }
        self.lines.read(v);
    }

    fn refuse(&mut self, path: &str, message: &str) {
        self.errors
            .0
            .entry(path.into())
            .or_default()
            .push(message.into());
    }

    /// The `draft.create` input, or nothing while a required choice is
    /// missing (each is then marked on its own field).
    fn input(&mut self, issue: bool) -> Option<Value> {
        self.errors = Errors::default();
        self.notice = None;
        let mut input = json!({"kind": self.kind});
        let mut ids = vec![("customer_id", self.customer.clone(), true)];
        ids.push(("series_id", self.series.clone(), true));
        if self.credit() {
            ids.push(("credited_document_id", self.credited.clone(), true));
        }
        for (key, text, required) in ids {
            match text.trim().parse::<i64>() {
                Ok(id) if id > 0 => input[key] = json!(id),
                _ if required => self.refuse(key, "can't be blank"),
                _ => {}
            }
        }
        for (key, text) in [
            ("issue_date", &self.issue_date),
            ("due_date", &self.due_date),
            ("description", &self.description),
        ] {
            f::put(&mut input, key, text.trim());
        }
        input["lines"] = self.lines.input();
        if issue && !self.lines.any() {
            self.refuse("lines", "needs at least one line before it is issued");
        }
        self.errors.is_empty().then_some(input)
    }
}

impl Documents {
    /// One event of the dialog or of the issue confirmation it leads to.
    /// `Ok(None)` when the event is not theirs.
    pub(super) async fn form_event(
        &mut self,
        scope: &Scope,
        credential: &str,
        v: &Value,
    ) -> Result<Option<Option<String>>> {
        let kind = v["type"].as_str().ok_or(Error::Invalid)?;
        let mut redirect = None;
        match kind {
            "cancel_create" => self.create = None,
            "draft_change" => {
                if let Some(form) = &mut self.create {
                    form.read(v);
                }
            }
            "add_line" | "remove_line" | "paste_open" | "paste_close" | "paste_lines" => {
                if let Some(form) = &mut self.create {
                    form.lines.event(v);
                }
            }
            "create_draft" => redirect = self.save_form(scope, credential, v).await?,
            "issue" => {
                let Some(issuing) = self.issuing.take() else {
                    return Ok(Some(None));
                };
                let id = issuing.id;
                let written = issuing
                    .commit(&self.pages, scope, credential, "issue")
                    .await?;
                match written {
                    Written::Done(record) => {
                        self.flash.set(StatusKind::Success, issue::issued(&record))
                    }
                    Written::Invalid(fields) => self.flash.set(
                        StatusKind::Error,
                        format!(
                            "The draft is saved, but it was not issued. {}",
                            f::sentences(&fields)
                        ),
                    ),
                    Written::Refused(message) => self.flash.set(
                        StatusKind::Error,
                        format!("The draft is saved, but it was not issued. {message}"),
                    ),
                }
                redirect = Some(format!("{INVOICES}/{id}"));
            }
            "cancel_issue" => {
                if let Some(issuing) = self.issuing.take() {
                    self.flash
                        .set(StatusKind::Info, "Draft saved. It has not been issued.");
                    redirect = Some(format!("{INVOICES}/{}", issuing.id));
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(redirect))
    }

    /// Save draft, or Issue: the same draft, then the issue review.
    async fn save_form(
        &mut self,
        scope: &Scope,
        credential: &str,
        v: &Value,
    ) -> Result<Option<String>> {
        let Some(form) = &mut self.create else {
            return Ok(None);
        };
        form.read(v);
        let issue = v["then"].as_str() == Some("issue") && self.can_issue;
        let Some(input) = form.input(issue) else {
            return Ok(None);
        };
        let written = self
            .pages
            .write(
                scope,
                credential,
                "create_draft",
                "finance.receivables.draft.create",
                input,
            )
            .await?;
        let record = match written {
            Written::Done(record) => record,
            Written::Invalid(fields) => {
                form.errors = Errors(fields);
                return Ok(None);
            }
            Written::Refused(message) => {
                form.notice = Some(crate::web::worded(
                    message,
                    "You do not have permission to start documents.",
                ));
                return Ok(None);
            }
        };
        self.create = None;
        let Some(id) = record["id"].as_i64() else {
            self.flash.set(StatusKind::Success, "Draft saved.");
            self.load_page(scope).await?;
            return Ok(None);
        };
        if issue {
            match Issue::ask(&self.pages, scope, credential, "create_draft", id).await? {
                Asked::Ready(issuing) => {
                    self.issuing = Some(issuing);
                    return Ok(None);
                }
                Asked::Refused(message) => self.flash.set(
                    StatusKind::Error,
                    format!("The draft is saved, but it cannot be issued yet. {message}"),
                ),
            }
        } else {
            self.flash.set(StatusKind::Success, "Draft saved.");
        }
        Ok(Some(format!("{INVOICES}/{id}")))
    }

    pub(super) fn invoice_dialog(&self, form: &InvoiceForm) -> Result<Html> {
        let credit = form.credit();
        let noun = if credit { "credit note" } else { "invoice" };
        let at = |key: &str| format!("receivables-draft-{key}");
        let mut labels: Vec<(String, String, String)> = [
            ("customer_id", "Customer"),
            ("credited_document_id", "Credited invoice"),
            ("series_id", "Numbering series"),
            ("issue_date", "Issue date"),
            ("due_date", "Due date"),
            ("description", "Description"),
        ]
        .into_iter()
        .map(|(key, label)| (key.to_owned(), at(key), label.to_owned()))
        .collect();
        labels.extend(form.lines.labels());
        let mut body = Html::EMPTY;
        if let Some(notice) = &form.notice {
            body.push(&render(Alert {
                kind: StatusKind::Error,
                id: "receivables-draft-notice",
                class: "mb-3",
                body: Html::text(notice),
            })?);
        }
        body.push(&render(
            form.errors.summary("receivables-draft-errors", &labels),
        )?);
        let series: Vec<_> = self
            .series
            .iter()
            .filter(|s| s.kind == form.kind && s.active)
            .collect();
        if series.is_empty() {
            let mut text = Html::text(&format!(
                "There is no numbering series for {noun}s yet. Add one in "
            ));
            text.push(&f::link(SETTINGS, "Receivables settings")?);
            text.push(&Html::text(", then come back."));
            body.push(&render(Alert {
                kind: StatusKind::Warning,
                id: "receivables-draft-no-series",
                class: "mb-3",
                body: text,
            })?);
        }
        let customer = self
            .customers
            .iter()
            .find(|c| c.id.to_string() == form.customer);
        let mut who = render(
            Combobox::new(at("customer_id"), format!("{FORM}[customer_id]"))
                .label("Customer *")
                .aria_label("Customer")
                .placeholder("Choose a customer")
                .options(
                    self.customers
                        .iter()
                        .filter(|c| c.active || credit)
                        .map(|c| Choice::new(&c.name, c.id.to_string()).detail(&c.code))
                        .collect(),
                )
                .value(&form.customer)
                .errors(form.errors.under("customer_id", "Customer"))
                .attrs(Attrs {
                    required: true,
                    ..Attrs::default()
                }),
        )?;
        if credit {
            let invoices: Vec<Choice> = self
                .credit_invoices
                .iter()
                .filter(|d| d.kind == "invoice" && d.status == "issued")
                .filter(|d| customer.is_none_or(|c| c.id == d.customer_id))
                .map(|d| {
                    Choice::new(super::reference(d), d.id.to_string()).detail(self.customer_name(d))
                })
                .collect();
            who.push(&render(
                Combobox::new(
                    at("credited_document_id"),
                    format!("{FORM}[credited_document_id]"),
                )
                .label("Credited invoice *")
                .aria_label("Credited invoice")
                .placeholder("Choose the invoice this credits")
                .options(invoices)
                .value(&form.credited)
                .hint("An issued invoice to the same customer.")
                .errors(
                    form.errors
                        .under("credited_document_id", "Credited invoice"),
                )
                .attrs(Attrs {
                    required: true,
                    ..Attrs::default()
                }),
            )?);
        }
        body.push(&if credit { f::two(who)? } else { who });
        let mut head = Html::EMPTY;
        let issue_date = form.issue_date.trim().parse::<jiff::civil::Date>().ok();
        let mut issue_field = Field::in_form(FORM, "issue_date")
            .id(at("issue_date"))
            .label("Issue date")
            .kind(InputKind::Date)
            .value(&form.issue_date)
            .errors(form.errors.under("issue_date", "Issue date"));
        if form.issue_date.trim().is_empty() {
            issue_field = issue_field.hint("Leave empty to use the day you issue it.");
        }
        head.push(&render(issue_field)?);
        if !credit {
            // What an empty due date becomes, when the page can already tell.
            let terms = customer
                .filter(|_| form.due_date.trim().is_empty())
                .zip(issue_date)
                .and_then(|(c, date)| {
                    let due = date
                        .checked_add(jiff::Span::new().days(i64::from(c.payment_terms_days)))
                        .ok()?;
                    Some(format!(
                        "Leave empty for {}: this customer pays in {} days.",
                        due.strftime("%-d %b %Y"),
                        c.payment_terms_days
                    ))
                });
            head.push(&render(
                Field::in_form(FORM, "due_date")
                    .id(at("due_date"))
                    .label("Due date")
                    .kind(InputKind::Date)
                    .value(&form.due_date)
                    .hint(terms.unwrap_or_else(|| {
                        "Leave empty to use the payment terms set for the customer.".into()
                    }))
                    .errors(form.errors.under("due_date", "Due date")),
            )?);
        }
        body.push(&f::two(head)?);
        body.push(&render(SectionHeading {
            id: "receivables-draft-lines-heading",
            title: "Lines",
            ..SectionHeading::default()
        })?);
        body.push(&form.lines.body(
            &form.errors,
            &self.accounts,
            &self.tax_codes,
            &self.figures,
        )?);
        let mut foot = render(
            Field::in_form(FORM, "description")
                .id(at("description"))
                .label(format!("Description of the {noun}"))
                .kind(InputKind::Textarea)
                .attr("maxlength", "500")
                .attr("rows", "2")
                .value(&form.description)
                .errors(form.errors.under("description", "Description")),
        )?;
        match series.as_slice() {
            // The one series is chosen already; it needs no field.
            [only] => foot.push(&render(
                Field::in_form(FORM, "series_id")
                    .id(at("series_id"))
                    .kind(InputKind::Hidden)
                    .value(only.id.to_string()),
            )?),
            many => foot.push(&render(
                Field::in_form(FORM, "series_id")
                    .id(at("series_id"))
                    .label("Numbering series")
                    .kind(InputKind::Select)
                    .prompt("Choose a series")
                    .options(
                        many.iter()
                            .map(|s| {
                                Choice::new(format!("{} ({})", s.name, s.code), s.id.to_string())
                            })
                            .collect(),
                    )
                    .value(&form.series)
                    .required()
                    .errors(form.errors.under("series_id", "Numbering series")),
            )?),
        }
        body.push(&render(Foot { body: foot })?);
        let mut actions = render(Button {
            id: "receivables-draft-cancel",
            button_type: "button",
            event: "cancel_create",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        // Enter saves the draft: the first submit button is the form's default.
        actions.push(&render(Button {
            id: "receivables-draft-save",
            button_type: "submit",
            name: "then",
            value: "save",
            variant: if self.can_issue {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Primary
            },
            working: "Saving…",
            body: Html::text("Save draft"),
            ..Button::default()
        })?);
        if self.can_issue {
            actions.push(&render(Button {
                id: "receivables-draft-issue",
                button_type: "submit",
                name: "then",
                value: "issue",
                variant: ButtonVariant::Primary,
                working: "Checking…",
                body: Html::text("Issue"),
                ..Button::default()
            })?);
        }
        let form_html = Form {
            id: "receivables-draft-form",
            event: "draft_change",
            change: true,
            class: "mt-4",
            body,
            actions: f::actions(actions)?,
        }
        .submit_event("create_draft")
        .map_err(|_| Error::Internal)?;
        f::modal(
            "receivables-draft-modal",
            if credit {
                "New credit note"
            } else {
                "New invoice"
            },
            "No number and no posting until you issue it.",
            "cancel_create",
            ModalWidth::Wide,
            &self.flash,
            form_html,
        )
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<div id="receivables-draft-foot" class="mt-3 border-t border-line pt-3">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Foot {
    body: Html,
}
