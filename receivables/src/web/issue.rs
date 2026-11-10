//! Issuing a draft: the review a person confirms and the commit, shared by
//! the invoice's own page and the New invoice dialog (one contract, two
//! keys). The confirmation says what the document becomes: its number, the
//! issue date (today when the draft has none) and the due date, all read from
//! the `document.issue` review, never worked out by the page. Where the
//! company offers Issue and send, the same review says what the email would
//! be, and the confirmation offers both: "Issue only" and "Issue and send".
use super::{format as f, render, Pages, Written};
use bilibi_database::{Error, Result};
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::PartyReview;
use bilibi_tenancy::Scope;
use bilibi_ui::{Facts, Html};
use serde_json::{json, Value};

const CONTRACT: &str = "finance.receivables.document.issue";

/// A draft whose issue was reviewed and waits for the person's confirmation.
pub(super) struct Issue {
    pub id: i64,
    review: Value,
    banner: Html,
}

pub(super) enum Asked {
    Ready(Issue),
    /// Why the draft cannot be issued as it stands, worded for the page.
    Refused(String),
}

impl Issue {
    /// Reviews the issue of `id` under the live `event` the person raised.
    pub(super) async fn ask(
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        event: &str,
        id: i64,
    ) -> Result<Asked> {
        let input = json!({"document_id": id, "company_id": Pages::company(scope)});
        Ok(
            match pages
                .party_picker()
                .review_core(scope, credential, event, CONTRACT, input)
                .await?
            {
                PartyReview::Valid { review } => Asked::Ready(Self {
                    id,
                    review,
                    banner: pages
                        .plans
                        .instance()
                        .await?
                        .banner("receivables-issue-instance", "mb-4")
                        .map_err(|_| Error::Internal)?,
                }),
                PartyReview::Invalid(fields) => Asked::Refused(f::sentences(&fields)),
                PartyReview::Refused(message) => Asked::Refused(message),
            },
        )
    }

    /// The address Issue and send would mail the invoice to, when the
    /// company offers it and the customer has one.
    pub(super) fn send_to(&self) -> Option<&str> {
        self.review["changes"]["email"]["to"].as_str()
    }

    /// The confirmation, raising `confirm_event` (issue), `send_event` (issue
    /// and email it, offered only when the review says it can be sent) or
    /// `cancel_event`.
    pub(super) fn dialog(
        &self,
        figures: &f::Figures,
        display: &DisplayContext,
        confirm_event: &'static str,
        send_event: &'static str,
        cancel_event: &'static str,
    ) -> Result<Html> {
        let after = &self.review["changes"]["after"];
        let text = |key: &str| after[key].as_str().filter(|text| !text.is_empty());
        let date = |key: &str| {
            text(key).map(|iso| match iso.parse::<jiff::civil::Date>() {
                Ok(date) => display.render_date(date),
                Err(_) => iso.to_owned(),
            })
        };
        let credit = text("kind") == Some("credit_note");
        let total = after["total"]
            .as_str()
            .and_then(|total| total.parse::<rust_decimal::Decimal>().ok())
            .map_or_else(String::new, |total| {
                format!(
                    "{} {}",
                    figures.decimal(total),
                    if figures.currency.is_empty() {
                        text("currency_code").unwrap_or("")
                    } else {
                        &figures.currency
                    }
                )
            });
        let reference = text("document_reference").unwrap_or("its number");
        let detail = match (credit, date("due_date")) {
            (false, Some(due)) => format!(
                "It becomes {reference}, due {due}. You cannot edit it afterwards; to correct it, issue a credit note."
            ),
            _ => format!("It becomes {reference}. You cannot edit it afterwards."),
        };
        // The draft's own date, or today's when it was saved without one.
        let today = self.review["changes"]["facts"]["draft"]["issue_date"].is_null();
        let mut items = vec![f::fact(
            "Issue date",
            &format!(
                "{}{}",
                date("issue_date").unwrap_or_default(),
                if today { " (today)" } else { "" }
            ),
        )];
        for (key, label) in [
            ("party_tax_identifier", "Tax ID"),
            ("party_registration_identifier", "Registration number"),
            ("party_email", "Email"),
            ("party_phone", "Phone"),
        ] {
            if let Some(value) = text(key) {
                items.push(f::fact(label, value));
            }
        }
        let address = [
            "party_line1",
            "party_line2",
            "party_line3",
            "party_postcode",
            "party_locality",
            "party_admin1_code",
            "party_country_iso",
        ]
        .into_iter()
        .filter_map(text)
        .collect::<Vec<_>>()
        .join(", ");
        if !address.is_empty() {
            items.push(f::fact("Address", &address));
        }
        // What Issue and send would email, or why this invoice cannot be
        // emailed; nothing while the company does not offer it.
        let email = &self.review["changes"]["email"];
        let offer = match (
            email["to"].as_str(),
            email["attachment"].as_str(),
            email["refusal"].as_str(),
        ) {
            (Some(to), Some(attachment), _) => {
                Some(format!("Issue and send emails {attachment} to {to}."))
            }
            (_, _, Some(refusal)) => Some(refusal.to_owned()),
            _ => None,
        };
        if let Some(offer) = &offer {
            items.push(f::fact("Email", offer));
        }
        let mut body = self.banner.clone();
        body.push(&render(Facts {
            id: "receivables-invoice-party-review",
            items,
        })?);
        let consequence = format!(
            "Issue this {} to {} for {total}?",
            if credit { "credit note" } else { "invoice" },
            text("party_name").unwrap_or("the customer"),
        );
        if self.send_to().is_some() {
            return render(SendConfirmation {
                consequence,
                detail,
                confirm_event,
                send_event,
                cancel_event,
                body,
            });
        }
        render(Confirmation {
            consequence,
            detail,
            confirm_event,
            cancel_event,
            body,
        })
    }

    /// Issues the reviewed draft under the live `event` (the confirmation);
    /// `send` also emails it, as the review offered.
    pub(super) async fn commit(
        self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        event: &str,
        send: bool,
    ) -> Result<Written> {
        let mut input = json!({"company_id": Pages::company(scope), "document_id": self.id});
        if send {
            input["send_email"] = json!(true);
        }
        let answer = pages
            .contracts
            .commit(&Pages::call(scope, credential, event, CONTRACT), input)
            .await
            .map(|mut value| value["results"][0]["result"].take());
        Pages::written(CONTRACT, event, answer)
    }
}

/// What the page says once a document is issued, and where its email is
/// going when it was sent in the same step.
pub(super) fn issued(record: &Value, sent_to: Option<&str>) -> String {
    let issued = match record["document_reference"].as_str() {
        Some(reference) => format!("Issued as {reference} and posted to the ledger."),
        None => "Issued and posted to the ledger.".into(),
    };
    match sent_to {
        Some(to) => format!("{issued} It is on its way to {to}."),
        None => issued,
    }
}

/// The confirmation where the company offers Issue and send: Cancel, the
/// issue alone, then the one press that also emails it.
#[derive(askama::Template)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% import "components.html" as c %}{% call ui::modal(id="receivables-invoice-issue-confirm", title=consequence, cancel_event=cancel_event, width=bilibi_ui::ModalWidth::Compact, description=detail, role="alertdialog") %}
{{ body|safe }}
<div class="mt-3 flex flex-wrap justify-end gap-2">
{% call c::button(id="receivables-invoice-issue-confirm-cancel", button_type="button", event=cancel_event) %}Cancel{% endcall %}
{% call c::button(id="receivables-invoice-issue-confirm-confirm", button_type="button", event=confirm_event, working="Issuing…") %}Issue only{% endcall %}
{% call c::button(id="receivables-invoice-issue-confirm-send", button_type="button", variant=bilibi_ui::ButtonVariant::Danger, event=send_event, working="Issuing…") %}Issue and send{% endcall %}
</div>
{% endcall %}"#,
    ext = "html"
)]
struct SendConfirmation {
    consequence: String,
    detail: String,
    confirm_event: &'static str,
    send_event: &'static str,
    cancel_event: &'static str,
    body: Html,
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% call ui::confirm_dialog(id="receivables-invoice-issue-confirm", consequence=consequence, detail=detail, confirm="Issue", working="Issuing…", confirm_event=confirm_event, cancel_event=cancel_event, body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct Confirmation {
    consequence: String,
    detail: String,
    confirm_event: &'static str,
    cancel_event: &'static str,
    body: Html,
}
