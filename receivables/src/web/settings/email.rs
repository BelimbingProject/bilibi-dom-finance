//! The Invoice email section of Receivables settings: whether this company
//! emails its invoices from Bilibi, under which name, with which words, and
//! whether Issue and send is offered. Each value is edited in place and saved
//! alone through `setup.configure` (`invoice_email`), as the layout is.
use super::{f, render, Settings, DENIED};
use crate::email::{DEFAULT_MESSAGE, DEFAULT_SUBJECT};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    Badge, BadgeKind, Card, CommitStatus, Facts, Html, InlineEdit, InlineField, InlineLongText,
    SectionHeading, StatusKind, Toggle,
};
use serde_json::{json, Value};

const WORDS: &str = "Bilibi fills in {number}, {company}, {customer}, {total} and {due_date}.";

#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex flex-col gap-1">{{ toggle|safe }}<p id="{{ id }}" class="text-xs text-ink-subtle">{{ hint }}</p></div>"#,
    ext = "html"
)]
struct Switch<'a> {
    toggle: Html,
    id: &'a str,
    hint: &'a str,
}

impl Settings {
    fn email_toggle(&self, key: &str, label: &str, on: bool, usable: bool, hint: &str) -> Result<Html> {
        render(Switch {
            toggle: render(Toggle {
                on,
                label,
                id: &format!("receivables-email-{key}"),
                event: "toggle_email",
                name: "field",
                value: &format!("{key}:{}", !on),
                disabled: !usable,
            })?,
            id: &format!("receivables-email-{key}-hint"),
            hint,
        })
    }

    pub(super) fn email_section(&self) -> Result<Html> {
        let email = self
            .setup
            .as_ref()
            .map(|s| s.invoice_email.clone())
            .unwrap_or_default();
        let editable = self.can_manage && self.setup.is_some();
        let mut body = render(SectionHeading {
            title: "Invoice email",
            id: "receivables-email-heading",
            class: "mt-6",
            title_actions: render(if email.enabled {
                Badge::status(BadgeKind::Success, "On")
            } else {
                Badge::status(BadgeKind::Neutral, "Off")
            })?,
            description: "Send an issued invoice to the customer from Bilibi, with its PDF attached. Nothing is sent while this is off.",
            ..SectionHeading::default()
        })?;
        let text = |key: &'static str, label: &'static str, value: &str, hint: String| {
            let mut field =
                InlineField::new(format!("receivables-email-{key}"), key, label, value)
                    .version(0)
                    .status(self.statuses.get(key))
                    .allow_empty()
                    .hint(hint);
            field.save_event = "save_email".into();
            field.id_value = "email".into();
            if !editable {
                field = field.readonly();
            }
            field
        };
        let or_default = |own: &str, built_in: &str| {
            if own.is_empty() {
                built_in.to_owned()
            } else {
                own.to_owned()
            }
        };
        let mut from = text(
            "from_name",
            "From name",
            &email.from_name,
            format!(
                "The sender's name your customer sees. Left empty, it is {}.",
                self.company
            ),
        );
        from.placeholder = self.company.clone();
        let mut reply = text(
            "reply_to",
            "Replies go to",
            &email.reply_to,
            "Bilibi sends from its own address. Put a mailbox you read here, so a customer's reply reaches you.".into(),
        );
        reply.placeholder = "accounts@yourcompany.com".into();
        let items = vec![
            f::fact_html(
                "Send invoices by email",
                self.email_toggle(
                    "enabled",
                    "Send invoices by email",
                    email.enabled,
                    editable,
                    "Adds Send by email to each issued invoice.",
                )?,
            ),
            f::fact_html("From name", render(InlineEdit { field: from })?),
            f::fact_html("Replies go to", render(InlineEdit { field: reply })?),
            f::fact_html(
                "Subject",
                render(InlineEdit {
                    field: text(
                        "subject",
                        "Subject",
                        &or_default(&email.subject, DEFAULT_SUBJECT),
                        WORDS.into(),
                    ),
                })?,
            ),
            f::fact_html(
                "Message",
                render(InlineLongText {
                    field: text(
                        "message",
                        "Message",
                        &or_default(&email.message, DEFAULT_MESSAGE),
                        format!("{WORDS} You can change the words for one invoice when you send it."),
                    ),
                    rows: 7,
                })?,
            ),
            f::fact_html(
                "Issue and send",
                self.email_toggle(
                    "issue_and_send",
                    "Offer Issue and send",
                    email.issue_and_send,
                    editable && email.enabled,
                    if email.enabled {
                        "Adds Issue and send beside Issue, so one press issues an invoice and emails it."
                    } else {
                        "Turn on Send invoices by email first."
                    },
                )?,
            ),
        ];
        body.push(&render(Card {
            id: "receivables-email-card",
            role: "region",
            labelledby: "receivables-email-heading",
            body: render(Facts {
                id: "receivables-email",
                items,
            })?,
            ..Card::default()
        })?);
        Ok(body)
    }

    /// `save_email` (one text saved in place) and `toggle_email` (an on/off).
    pub(super) async fn email_event(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        v: &Value,
    ) -> Result<()> {
        let (key, value, shown): (&'static str, Value, String) = if event == "toggle_email" {
            let (key, to) = v["field"]
                .as_str()
                .and_then(|field| field.split_once(':'))
                .ok_or(Error::Invalid)?;
            let key = match key {
                "enabled" => "enabled",
                "issue_and_send" => "issue_and_send",
                _ => return Err(Error::Invalid),
            };
            (key, json!(to == "true"), to.to_owned())
        } else {
            let key = match v["field"].as_str() {
                Some("from_name") => "from_name",
                Some("reply_to") => "reply_to",
                Some("subject") => "subject",
                Some("message") => "message",
                _ => return Err(Error::Invalid),
            };
            let text = v["value"].as_str().ok_or(Error::Invalid)?;
            (key, json!(text), text.to_owned())
        };
        let label = match key {
            "from_name" => "From name",
            "reply_to" => "Replies go to",
            "subject" => "Subject",
            "message" => "Message",
            _ => "Invoice email",
        };
        let status = if self.can_manage {
            let written = self
                .pages
                .write(
                    scope,
                    credential,
                    event,
                    "finance.receivables.setup.configure",
                    json!({"invoice_email": {key: value}}),
                )
                .await?;
            f::commit_status(&written, &format!("invoice_email.{key}"), label, &shown)
        } else {
            CommitStatus::Error(DENIED.into())
        };
        if event == "toggle_email" {
            // A switch has no line of its own for a refusal: say it above.
            if let CommitStatus::Error(message) = status {
                self.flash.set(StatusKind::Error, message);
            }
        } else {
            self.statuses.put(key.to_owned(), status);
        }
        Ok(())
    }
}
