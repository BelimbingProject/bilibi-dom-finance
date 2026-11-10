//! Sending an issued invoice by email: the dialog the invoice page opens
//! from its Send button (`?do=send`). It starts from `document.email` (who
//! it goes to, the words, the PDF's name) and sends through `document.send`,
//! the contract an agent calls. Each send then shows in the page's History.
use super::super::{
    format::{self as f, Errors},
    render, Flash, Pages, Written, INVOICES,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{params, Button, ButtonVariant, Facts, Field, Form, Html, InputKind, ModalWidth};
use serde_json::{json, Value};
use std::collections::HashMap;

const READ: &str = "finance.receivables.document.email";
const SEND: &str = "finance.receivables.document.send";
const FIELDS: [(&str, &str); 3] = [("to", "To"), ("subject", "Subject"), ("message", "Message")];

/// The email a person is about to send, as they have it in the dialog.
pub(super) struct Sending {
    values: HashMap<String, String>,
    from: String,
    attachment: String,
    /// Why the To box starts empty.
    no_address: Option<String>,
    errors: Errors,
}

/// What a press of Send came to.
pub(super) enum Sent {
    /// On its way to this address.
    Queued(String),
    /// Refused on a field: the dialog stays open and says where.
    Invalid,
    Refused(String),
}

impl Sending {
    /// The dialog's starting values, or why this invoice cannot be sent.
    pub(super) async fn open(
        pages: &Pages,
        scope: &Scope,
        id: i64,
    ) -> Result<std::result::Result<Self, String>> {
        let answer = pages.read(scope, READ, json!({"document_id": id})).await?;
        let email = &answer["email"];
        if let Some(refusal) = answer["refusal"].as_str() {
            return Ok(Err(refusal.to_owned()));
        }
        if !email.is_object() {
            return Ok(Err(
                "This invoice cannot be sent by email right now. Reload the page and try again."
                    .into(),
            ));
        }
        let text = |key: &str| email[key].as_str().unwrap_or_default().to_owned();
        let from = match email["reply_to"].as_str().filter(|r| !r.is_empty()) {
            Some(reply) => format!("{} · replies go to {reply}", text("from_name")),
            None => text("from_name"),
        };
        Ok(Ok(Self {
            values: FIELDS
                .iter()
                .map(|(key, _)| ((*key).to_owned(), text(key)))
                .collect(),
            from,
            attachment: text("attachment"),
            no_address: answer["no_address"].as_str().map(str::to_owned),
            errors: Errors::default(),
        }))
    }

    pub(super) fn dialog(&self, id: i64, flash: &Flash) -> Result<Html> {
        let value = |key: &str| self.values.get(key).cloned().unwrap_or_default();
        let field = |key: &str, label: &str| {
            Field::in_form("email", key)
                .id(format!("receivables-send-{key}"))
                .label(label)
                .value(value(key))
                .errors(self.errors.under(key, label))
                .required()
        };
        let labels: Vec<_> = FIELDS
            .iter()
            .map(|(key, label)| {
                (
                    (*key).to_owned(),
                    format!("receivables-send-{key}"),
                    (*label).to_owned(),
                )
            })
            .collect();
        let mut body = render(self.errors.summary("receivables-send-errors", &labels))?;
        body.push(&render(Facts {
            id: "receivables-send-facts",
            items: vec![
                f::fact("From", &self.from),
                f::fact_html(
                    "Attached",
                    f::link(&format!("{INVOICES}/{id}/pdf"), &self.attachment)?,
                ),
            ],
        })?);
        let mut to = field("to", "To")
            .kind(InputKind::Email)
            .attr("maxlength", "254")
            .autocomplete("off");
        if let Some(why) = self.no_address.as_deref().filter(|_| value("to").is_empty()) {
            to = to.hint(why);
        }
        body.push(&render(to)?);
        body.push(&render(field("subject", "Subject").attr("maxlength", "200"))?);
        body.push(&render(
            field("message", "Message")
                .kind(InputKind::Textarea)
                .attr("rows", "8")
                .attr("maxlength", "5000"),
        )?);
        let mut actions = render(Button {
            id: "receivables-send-cancel",
            button_type: "button",
            event: "cancel_send",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "receivables-send-confirm",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Sending…",
            body: Html::text("Send"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "receivables-send-form",
            event: "send",
            change: false,
            class: "mt-4",
            body,
            actions: f::actions(actions)?,
        })?;
        f::modal(
            "receivables-send-modal",
            "Send invoice by email",
            "The customer gets this message with the invoice attached as a PDF. Each send is kept in this invoice's history.",
            "cancel_send",
            ModalWidth::Narrow,
            flash,
            form,
        )
    }

    /// Sends what the person has in the dialog, under the live `send` event.
    pub(super) async fn send(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        id: i64,
        v: &Value,
    ) -> Result<Sent> {
        let form = params::form_values(v, "email");
        let mut input = json!({"document_id": id});
        let mut missing = Errors::default();
        for (key, _) in FIELDS {
            let text = f::posted(&form, key);
            self.values.insert(key.to_owned(), text.to_owned());
            if text.is_empty() {
                missing
                    .0
                    .insert(key.to_owned(), vec!["can't be blank".into()]);
            }
            input[key] = json!(text);
        }
        if !missing.is_empty() {
            self.errors = missing;
            return Ok(Sent::Invalid);
        }
        Ok(
            match pages.write(scope, credential, "send", SEND, input).await? {
                Written::Done(record) => Sent::Queued(
                    record["to_email"]
                        .as_str()
                        .ok_or(Error::Internal)?
                        .to_owned(),
                ),
                Written::Invalid(fields)
                    if fields
                        .keys()
                        .all(|key| FIELDS.iter().any(|(k, _)| k == key)) =>
                {
                    self.errors = Errors(fields);
                    Sent::Invalid
                }
                Written::Invalid(fields) => Sent::Refused(f::sentences(&fields)),
                Written::Refused(message) => Sent::Refused(super::super::worded(
                    message,
                    "You do not have permission to send invoices.",
                )),
            },
        )
    }
}
