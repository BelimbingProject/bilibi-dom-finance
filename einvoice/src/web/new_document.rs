//! The New e-invoice dialog of the E-invoices list: the one door through
//! which a person starts the e-invoice of an issued invoice or credit note.
//! "+ E-invoice" opens it with a choice of documents; an invoice's own page
//! and list open it for that invoice (`FiscalStart::href`). It reads the
//! draft through `finance.einvoice.document.prefill`, asks only for what the
//! invoice does not hold (the buyer's registration, state and country codes,
//! each line's classification and tax type), then records it with
//! `finance.einvoice.document.issue` and sends it with
//! `finance.einvoice.document.submit`, the contracts an agent calls.
use super::{company, render, Pages, ADDRESS, PAGES};
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{dates, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    Alert, Attrs, Button, ButtonVariant, Choice, Combobox, Field, Form, Html, InputKind, Modal,
    ModalWidth, StatusKind,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const FORM: &str = "einvoice";
pub(super) const ISSUE: &str = "finance.einvoice.document.issue";
pub(super) const SUBMIT: &str = "finance.einvoice.document.submit";
/// The live event that records and sends; `PAGE_WRITES` names its contracts.
pub(super) const EVENT: &str = "create_einvoice";

/// The buyer fields a person may correct: key in the form, path in the
/// draft's `buyer`, label.
const BUYER: [(&str, &[&str], &str); 10] = [
    ("tax_id", &["tax_id"], "Tax identification number (TIN)"),
    (
        "registration_type",
        &["registration_type"],
        "Registration type",
    ),
    (
        "registration_number",
        &["registration_number"],
        "Registration number",
    ),
    ("phone", &["phone"], "Phone"),
    ("email", &["email"], "Email"),
    ("address", &["address", "lines"], "Address"),
    ("city", &["address", "city"], "City"),
    ("postcode", &["address", "postcode"], "Postcode"),
    ("state_code", &["address", "state_code"], "State"),
    ("country_code", &["address", "country_code"], "Country"),
];

#[derive(Default)]
struct Line {
    description: String,
    detail: String,
    classification: String,
    tax_type: String,
    taxed: bool,
}

/// What the dialog holds between events.
#[derive(Default)]
pub(super) struct NewDocument {
    /// The documents "+ E-invoice" offers; empty when an invoice opened it.
    choices: Vec<Choice>,
    /// `module|record` of the chosen document.
    chosen: String,
    /// The prefill's `draft`, which the edits below are laid over.
    draft: Value,
    /// The prefill's `source`: what the invoice is, for the dialog's words.
    source: Value,
    number: String,
    buyer: BTreeMap<&'static str, String>,
    lines: Vec<Line>,
    codes: Value,
    errors: BTreeMap<String, Vec<String>>,
    /// What stops this dialog as a whole, said in its own alert.
    notice: Option<String>,
    /// The read was refused: the dialog offers only Close.
    refused: bool,
}

/// What an event of the dialog came to.
pub(super) enum Outcome {
    Stay,
    Close,
    /// The e-invoice was recorded: open it, with what happened to its sending.
    Opened(String, (StatusKind, String)),
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn key(module: &str, record: &str) -> String {
    format!("{module}|{record}")
}

impl NewDocument {
    /// The dialog with a choice of documents ("+ E-invoice").
    pub(super) async fn choose(pages: &Pages, scope: &Scope) -> Result<Self> {
        let mut dialog = Self::default();
        match pages
            .read(scope, "finance.einvoice.source.list", json!({}))
            .await
        {
            Ok(answer) => {
                dialog.choices = answer["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        Choice::new(
                            format!(
                                "{} · {}",
                                text(&r["number"]),
                                text(&r["party_name"]),
                            ),
                            key(&text(&r["module"]), &text(&r["record"])),
                        )
                        .detail(dates::display_text(&text(&r["issue_date"])))
                    })
                    .collect();
                if dialog.choices.is_empty() {
                    dialog.refused = true;
                    dialog.notice = Some(
                        "Every issued invoice and credit note already has an e-invoice.".into(),
                    );
                }
            }
            Err(error) => dialog.refuse(error)?,
        }
        Ok(dialog)
    }

    /// The dialog for one document, as an invoice's page asks for it.
    pub(super) async fn start(
        pages: &Pages,
        scope: &Scope,
        module: &str,
        record: &str,
    ) -> Result<Self> {
        let mut dialog = Self::default();
        dialog.load(pages, scope, module, record).await?;
        Ok(dialog)
    }

    fn refuse(&mut self, error: Error) -> Result<()> {
        self.refused = true;
        self.notice = Some(match error {
            Error::Forbidden | Error::Unauthorized => {
                "You do not have permission to do that.".into()
            }
            Error::Invalid | Error::NotFound => {
                "This is not an issued invoice or credit note that you can read.".into()
            }
            other => return Err(other),
        });
        Ok(())
    }

    async fn load(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        module: &str,
        record: &str,
    ) -> Result<()> {
        self.chosen = key(module, record);
        self.errors.clear();
        self.notice = None;
        let answer = match pages
            .read(
                scope,
                "finance.einvoice.document.prefill",
                json!({"source": {"module": module, "record": record}}),
            )
            .await
        {
            Ok(answer) => answer,
            Err(error) => return self.refuse(error),
        };
        if let Some(existing) = answer["document"]
            .as_object()
            .filter(|d| d["replaceable"] != json!(true))
        {
            self.refused = true;
            self.notice = Some(format!(
                "{} already has e-invoice {}.",
                text(&answer["source"]["number"]),
                existing["number"].as_str().unwrap_or_default()
            ));
            return Ok(());
        }
        self.codes = pages
            .read(scope, "finance.einvoice.code.list", json!({}))
            .await
            .unwrap_or(Value::Null);
        self.draft = answer["draft"].clone();
        self.source = answer["source"].clone();
        self.number = text(&self.draft["code_number"]);
        let buyer = &self.draft["buyer"];
        self.buyer = BUYER
            .iter()
            .map(|(key, path, _)| {
                let value = path.iter().fold(buyer, |at, step| &at[*step]);
                let value = match value.as_array() {
                    Some(lines) => lines
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    None => text(value),
                };
                (*key, value)
            })
            .collect();
        self.lines = self.draft["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|line| {
                let rate = text(&line["tax_rate"]);
                let taxed = rate.parse::<rust_decimal::Decimal>().is_ok_and(|r| !r.is_zero());
                let amount = |key: &str| {
                    text(&line[key])
                        .parse()
                        .map_or_else(|_| text(&line[key]), |d| Money::display_decimal(d, None))
                };
                Line {
                    description: text(&line["description"]),
                    detail: format!(
                        "{} × {}{}",
                        text(&line["quantity"]),
                        amount("unit_price"),
                        if taxed {
                            format!(" · tax {rate}%")
                        } else {
                            String::new()
                        }
                    ),
                    classification: text(&line["classification_code"]),
                    tax_type: text(&line["tax_type"]),
                    taxed,
                }
            })
            .collect();
        if self.source["kind"] == "credit_note" && self.draft["original_document_id"].is_null() {
            self.notice = Some(
                "Send the e-invoice of the invoice this credit note corrects first, and wait until it is valid. A credit note's e-invoice refers to it."
                    .into(),
            );
        }
        Ok(())
    }

    /// One event of the dialog.
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        v: &Value,
    ) -> Result<Outcome> {
        match v["type"].as_str() {
            Some("cancel_einvoice") => return Ok(Outcome::Close),
            Some("einvoice_change") => {
                let before = self.chosen.clone();
                self.read(v);
                if self.chosen != before {
                    if let Some((module, record)) = self.chosen.clone().split_once('|') {
                        let choices = std::mem::take(&mut self.choices);
                        *self = Self {
                            choices,
                            ..Self::default()
                        };
                        self.load(pages, scope, module, record).await?;
                    }
                }
            }
            Some(EVENT) => {
                self.read(v);
                return self.save(pages, scope, credential).await;
            }
            _ => return Err(Error::Invalid),
        }
        Ok(Outcome::Stay)
    }

    fn read(&mut self, v: &Value) {
        let field = |key: &str| {
            v[format!("{FORM}[{key}]")]
                .as_str()
                .map(|text| text.chars().take(1024).collect::<String>())
        };
        if !self.choices.is_empty() {
            if let Some(chosen) = field("source") {
                self.chosen = chosen;
            }
        }
        if let Some(number) = field("code_number") {
            self.number = number;
        }
        for (key, _, _) in BUYER {
            if let Some(value) = field(key) {
                self.buyer.insert(key, value);
            }
        }
        for (n, line) in self.lines.iter_mut().enumerate() {
            if let Some(code) = field(&format!("classification_{n}")) {
                line.classification = code;
            }
            if let Some(code) = field(&format!("tax_type_{n}")) {
                line.tax_type = code;
            }
        }
    }

    /// The `document.issue` input: the prefilled draft with the person's
    /// edits laid over it, issued now.
    fn input(&self) -> Value {
        let mut input = self.draft.clone();
        input["code_number"] = json!(self.number.trim());
        let now = jiff::Timestamp::now();
        input["issued_at"] =
            json!(jiff::Timestamp::from_second(now.as_second()).unwrap_or(now).to_string());
        for (key, path, _) in BUYER {
            let value = self.buyer.get(key).map_or("", |v| v.trim());
            let value = if key == "address" {
                json!(value
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>())
            } else if value.is_empty() {
                Value::Null
            } else {
                json!(value)
            };
            match path {
                [one] => input["buyer"][*one] = value,
                [one, two] => input["buyer"][*one][*two] = value,
                _ => {}
            }
        }
        for (n, line) in self.lines.iter().enumerate() {
            input["lines"][n]["classification_code"] = json!(line.classification.trim());
            input["lines"][n]["tax_type"] = json!(line.tax_type.trim());
        }
        input
    }

    async fn save(&mut self, pages: &Pages, scope: &Scope, credential: &str) -> Result<Outcome> {
        if self.refused || self.draft.is_null() {
            return Ok(Outcome::Stay);
        }
        self.errors.clear();
        self.notice = None;
        let mut input = self.input();
        input["company_id"] = json!(company(scope)?);
        let call = |contract| Call {
            scope,
            credential,
            service: PAGES,
            event: EVENT,
            contract,
        };
        let recorded = match pages.contracts.submit(&call(ISSUE), input).await {
            Ok(answer) => answer["result"]["record"].clone(),
            Err(error) => {
                self.refusal(error, "The e-invoice could not be recorded. Please try again.")?;
                return Ok(Outcome::Stay);
            }
        };
        let Some(id) = recorded["document"]["id"].as_i64() else {
            return Ok(Outcome::Close);
        };
        let number = text(&recorded["document"]["code_number"]);
        let sent = pages
            .contracts
            .submit(
                &call(SUBMIT),
                json!({"company_id": company(scope)?, "document_id": id}),
            )
            .await;
        let flash = match sent {
            Ok(_) => (
                StatusKind::Success,
                format!(
                    "E-invoice {number} was sent. Its status changes when the tax authority answers."
                ),
            ),
            Err(error) => {
                let why = match error {
                    ContractError::InvalidInput(fields) => fields
                        .into_values()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" "),
                    ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => {
                        "You do not have permission to submit documents.".into()
                    }
                    ContractError::Backend(
                        error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                    ) => return Err(error),
                    _ => "Open it and press Submit to authority to try again.".into(),
                };
                (
                    StatusKind::Error,
                    format!("E-invoice {number} was recorded, but it was not sent. {why}"),
                )
            }
        };
        Ok(Outcome::Opened(format!("{ADDRESS}/{id}"), flash))
    }

    /// A refused issue: each field's words beside the field, the rest in the
    /// dialog's alert.
    fn refusal(&mut self, error: ContractError, fallback: &str) -> Result<()> {
        let fields = match error {
            ContractError::InvalidInput(fields) => fields,
            ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => {
                self.notice = Some("You do not have permission to do that.".into());
                return Ok(());
            }
            ContractError::Backend(Error::Conflict) => {
                self.notice =
                    Some("The details changed. Check them and submit again.".into());
                return Ok(());
            }
            ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            ) => return Err(error),
            other => {
                eprintln!("e-invoice page: issue failed: {other:?}");
                self.notice = Some(fallback.into());
                return Ok(());
            }
        };
        let mut notices = Vec::new();
        for (path, messages) in fields {
            let message = messages.join(", ");
            let own = match path.as_str() {
                "code_number" => Some("code_number".to_owned()),
                p => p
                    .strip_prefix("buyer.")
                    .map(|rest| rest.rsplit('.').next().unwrap_or(rest))
                    .map(|leaf| if leaf == "lines" { "address" } else { leaf })
                    .filter(|leaf| BUYER.iter().any(|(key, _, _)| key == leaf))
                    .map(str::to_owned)
                    .or_else(|| {
                        let mut parts = p.strip_prefix("lines.")?.splitn(2, '.');
                        let n = parts.next()?.parse::<usize>().ok()?;
                        match parts.next()? {
                            "classification_code" => Some(format!("classification_{n}")),
                            "tax_type" | "tax_rate" => Some(format!("tax_type_{n}")),
                            _ => None,
                        }
                    }),
            };
            match own {
                Some(field) => self.errors.entry(field).or_default().push(message),
                None => notices.push(match path.as_str() {
                    "base" if message.contains("already issued that number") => {
                        self.errors
                            .entry("code_number".into())
                            .or_default()
                            .push("is already used by another e-invoice".into());
                        continue;
                    }
                    "base" => message,
                    "supplier" => format!("Your company's own e-invoice details {message}."),
                    "source" if message.contains("already the source") => {
                        "This document already has an e-invoice.".into()
                    }
                    "source" if message.contains("recognises") => {
                        "The e-invoice total would differ from the invoice total. Check the tax type of each line.".into()
                    }
                    "source" => {
                        "This document cannot get an e-invoice: its journal entry was reversed or you cannot read it.".into()
                    }
                    "original_document_id" => {
                        "Send the e-invoice of the invoice this credit note corrects first, and wait until it is valid.".into()
                    }
                    "currency_code" => format!("The currency {message}."),
                    "buyer.name" => format!("The customer name {message}."),
                    _ => format!("Line details: {message}."),
                }),
            }
        }
        if !notices.is_empty() {
            self.notice = Some(notices.join(" "));
        }
        Ok(())
    }

    fn errors(&self, key: &str, label: &str) -> Vec<String> {
        self.errors
            .get(key)
            .into_iter()
            .flatten()
            .map(|message| format!("{label} {message}"))
            .collect()
    }

    fn codes(&self, list: &str) -> Vec<Choice> {
        self.codes[list]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(code, name)| Choice::new(name.as_str().unwrap_or(code), code).detail(code))
            .collect()
    }

    pub(super) fn dialog(&self) -> Result<Html> {
        let at = |key: &str| format!("einvoice-new-{key}");
        let mut body = Html::EMPTY;
        if let Some(notice) = &self.notice {
            body.push(&render(Alert {
                kind: if self.refused {
                    StatusKind::Info
                } else {
                    StatusKind::Error
                },
                id: "einvoice-new-notice",
                class: "mb-3",
                body: Html::text(notice),
            })?);
        }
        if !self.choices.is_empty() {
            body.push(&render(
                Combobox::new(at("source"), format!("{FORM}[source]"))
                    .label("Invoice or credit note *")
                    .aria_label("Invoice or credit note")
                    .placeholder("Choose an issued document")
                    .options(self.choices.clone())
                    .value(&self.chosen)
                    .attrs(Attrs {
                        required: true,
                        ..Attrs::default()
                    }),
            )?);
        }
        let ready = !self.refused && !self.draft.is_null();
        if ready {
            body.push(&render(SourceLine {
                text: format!(
                    "{} · {} · {} · {} {}",
                    text(&self.source["number"]),
                    text(&self.source["party_name"]),
                    dates::display_text(&text(&self.source["issue_date"])),
                    text(&self.source["total"])
                        .parse()
                        .map_or_else(|_| text(&self.source["total"]), |d| {
                            Money::display_decimal(d, None)
                        }),
                    text(&self.source["currency_code"]),
                ),
            })?);
            body.push(&render(
                Field::in_form(FORM, "code_number")
                    .id(at("code_number"))
                    .label("E-invoice number")
                    .value(&self.number)
                    .required()
                    .attr("maxlength", "50")
                    .errors(self.errors("code_number", "E-invoice number")),
            )?);
            let mut buyer = Html::EMPTY;
            for (key, _, label) in BUYER {
                let value = self.buyer.get(key).map_or("", String::as_str);
                let errors = self.errors(key, label);
                let required = !matches!(key, "email" | "postcode");
                let combobox = |list: &str, placeholder: &str| {
                    let mut c = Combobox::new(at(key), format!("{FORM}[{key}]"))
                        .aria_label(label)
                        .placeholder(placeholder)
                        .options(self.codes(list))
                        .value(value)
                        .errors(errors.clone())
                        .attrs(Attrs {
                            required,
                            ..Attrs::default()
                        });
                    c = c.label(format!("{label} *"));
                    render(c)
                };
                buyer.push(&match key {
                    "state_code" => combobox("state", "Choose a state")?,
                    "country_code" => combobox("country", "Choose a country")?,
                    _ => {
                        let mut field = Field::in_form(FORM, key)
                            .id(at(key))
                            .label(label)
                            .value(value)
                            .errors(errors);
                        if required {
                            field = field.required();
                        }
                        field = match key {
                            "registration_type" => field
                                .kind(InputKind::Select)
                                .prompt("Choose a type")
                                .options(self.codes("registration_type")),
                            "address" => field
                                .kind(InputKind::Textarea)
                                .attr("rows", "2")
                                .hint("Up to three lines."),
                            "phone" => field.hint("With the country code, such as +60123456789."),
                            _ => field,
                        };
                        render(field)?
                    }
                });
            }
            body.push(&render(Section {
                id: "einvoice-new-buyer",
                heading: &format!("Buyer: {}", text(&self.source["party_name"])),
                columns: true,
                body: buyer,
            })?);
            let mut lines = Html::EMPTY;
            for (n, line) in self.lines.iter().enumerate() {
                let mut fields = render(
                    Combobox::new(
                        at(&format!("classification-{n}")),
                        format!("{FORM}[classification_{n}]"),
                    )
                    .label("Classification *")
                    .aria_label(format!("Classification of line {}", n + 1))
                    .placeholder("Choose what was sold")
                    .options(self.codes("classification"))
                    .value(&line.classification)
                    .errors(self.errors(&format!("classification_{n}"), "Classification"))
                    .attrs(Attrs {
                        required: true,
                        ..Attrs::default()
                    }),
                )?;
                fields.push(&render(
                    Field::in_form(FORM, &format!("tax_type_{n}"))
                        .id(at(&format!("tax-type-{n}")))
                        .label("Tax type")
                        .aria_label(format!("Tax type of line {}", n + 1))
                        .kind(InputKind::Select)
                        .prompt("Choose a tax type")
                        .options(self.codes("tax_type"))
                        .value(&line.tax_type)
                        .required()
                        .hint(if line.taxed {
                            "Which tax this line charges."
                        } else {
                            ""
                        })
                        .errors(self.errors(&format!("tax_type_{n}"), "Tax type")),
                )?);
                lines.push(&render(LineRow {
                    n,
                    description: &line.description,
                    detail: &line.detail,
                    fields,
                })?);
            }
            body.push(&render(Section {
                id: "einvoice-new-lines",
                heading: "Lines",
                columns: false,
                body: lines,
            })?);
        }
        let mut actions = render(Button {
            id: "einvoice-new-cancel",
            button_type: "button",
            event: "cancel_einvoice",
            body: Html::text(if ready { "Cancel" } else { "Close" }),
            ..Button::default()
        })?;
        if ready {
            actions.push(&render(Button {
                id: "einvoice-new-submit",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Sending…",
                body: Html::text("Submit e-invoice"),
                ..Button::default()
            })?);
        }
        let form = Form {
            id: "einvoice-new-form",
            event: "einvoice_change",
            change: true,
            class: "mt-4",
            body,
            actions: render(ActionRow { buttons: actions })?,
        }
        .submit_event(EVENT)
        .map_err(|_| Error::Internal)?;
        render(Modal {
            id: "einvoice-new-modal",
            title: "New e-invoice",
            cancel_event: "cancel_einvoice",
            width: ModalWidth::Wide,
            description: "The e-invoice is recorded and sent to the tax authority. It cannot be edited afterwards.",
            role: "",
            flash: None,
            body: form,
        })
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<p id="einvoice-new-source" class="mb-3 text-sm text-ink">{{ text }}</p>"#,
    ext = "html"
)]
struct SourceLine {
    text: String,
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::section(labelledby=id, heading=heading) %}<div class="{% if columns %}grid items-start gap-x-3 sm:grid-cols-2{% else %}space-y-3{% endif %}">{{ body|safe }}</div>{% endcall %}"#,
    ext = "html"
)]
struct Section<'a> {
    id: &'a str,
    heading: &'a str,
    columns: bool,
    body: Html,
}

#[derive(askama::Template)]
#[template(
    source = r#"<div id="einvoice-new-line-{{ n }}" class="border-t border-line pt-3 first:border-t-0 first:pt-0"><p class="text-sm font-medium text-ink-strong">{{ description }}</p><p class="text-sm text-ink-muted">{{ detail }}</p><div class="mt-2 grid items-start gap-x-3 sm:grid-cols-2">{{ fields|safe }}</div></div>"#,
    ext = "html"
)]
struct LineRow<'a> {
    n: usize,
    description: &'a str,
    detail: &'a str,
    fields: Html,
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct ActionRow {
    buttons: Html,
}
