//! Match a source opening item on the ordinary bank matching page.
use super::{company, render, Pages, PAGES};
use crate::{OpeningItem, StatementLineItem};
use askama::Template;
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::MinorUnits;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    params, Alert, Button, ButtonVariant, Card, Choice, Field, Form, Html, InputKind, Modal,
    ModalWidth, SectionHeading, StatusKind,
};
use serde_json::{json, Value};

const CONTRACT: &str = "finance.reconciliation.opening.match";
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    buttons: Html,
}
struct Review {
    input: Value,
    facts: Value,
}
pub(super) struct OpeningMatch {
    account: i64,
    item: Option<i64>,
    line: Option<i64>,
    message: Option<String>,
    review: Option<Review>,
}
impl OpeningMatch {
    pub(super) fn new(account: i64) -> Self {
        Self {
            account,
            item: None,
            line: None,
            message: None,
            review: None,
        }
    }
    pub(super) fn body(
        &self,
        items: &[OpeningItem],
        lines: &[StatementLineItem],
        units: u8,
        can_match: bool,
    ) -> Result<Html> {
        if !can_match || items.is_empty() {
            return Ok(Html::EMPTY);
        }
        let units = MinorUnits::new(i64::from(units)).ok_or(Error::Internal)?;
        let mut choices = vec![Choice::new("Choose an opening item", "")];
        choices.extend(items.iter().map(|item| {
            Choice::new(
                format!(
                    "{} · {} · {}",
                    item.description,
                    item.source_id,
                    item.amount.display(units)
                ),
                item.id.to_string(),
            )
        }));
        let selected = items.iter().find(|item| Some(item.id) == self.item);
        let mut line_choices = vec![Choice::new("Choose a statement line", "")];
        line_choices.extend(
            lines
                .iter()
                .filter(|line| selected.is_some_and(|item| item.amount == line.line.amount))
                .map(|line| {
                    Choice::new(
                        format!(
                            "{} · {} · {}",
                            line.line.line_date,
                            line.line.description,
                            line.line.amount.display(units)
                        ),
                        line.line.id.to_string(),
                    )
                }),
        );
        let mut body = render(SectionHeading {
            title: "Match an opening item",
            description: "Choose an outstanding item and the matching statement line.",
            ..SectionHeading::default()
        })?;
        if let Some(message) = &self.message {
            body.push(&render(Alert {
                kind: StatusKind::Error,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        let mut fields = render(
            Field::in_form("opening_match", "item")
                .id("opening-match-item")
                .label("Opening item")
                .kind(InputKind::Select)
                .options(choices)
                .value(self.item.map(|v| v.to_string()).unwrap_or_default()),
        )?;
        fields.push(&render(
            Field::in_form("opening_match", "line")
                .id("opening-match-line")
                .label("Statement line")
                .kind(InputKind::Select)
                .options(line_choices)
                .value(self.line.map(|v| v.to_string()).unwrap_or_default()),
        )?);
        let actions = render(Button {
            button_type: "button",
            event: "preview_opening_match",
            body: Html::text("Review match"),
            variant: ButtonVariant::Primary,
            ..Button::default()
        })?;
        body.push(&render(Form {
            id: "opening-match-form",
            event: "opening_match_change",
            change: true,
            class: "mt-4",
            body: fields,
            actions,
        })?);
        render(Card {
            id: "opening-match",
            body,
            ..Card::default()
        })
    }
    pub(super) async fn modal(&self, pages: &Pages) -> Result<Html> {
        let Some(review) = &self.review else {
            return Ok(Html::EMPTY);
        };
        let f = &review.facts;
        let mut body = pages
            .plans
            .instance()
            .await?
            .banner("opening-match-instance", "")
            .map_err(|_| Error::Internal)?;
        body.push(&render(Alert {body:Html::text(&format!("Match opening item {} ({}) to the statement line {} on {}. No new entry will be posted.",f["source_id"].as_str().unwrap_or(""),f["item_description"].as_str().unwrap_or(""),f["statement_description"].as_str().unwrap_or(""),f["statement_date"].as_str().unwrap_or(""))),..Alert::default()})?);
        let mut buttons = render(Button {
            button_type: "button",
            event: "cancel_opening_match",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            button_type: "button",
            event: "commit_opening_match",
            body: Html::text("Match"),
            variant: ButtonVariant::Primary,
            working: "Matching…",
            ..Button::default()
        })?);
        body.push(&render(Actions { buttons })?);
        render(Modal {
            id: "opening-match-review-modal",
            title: "Review opening match",
            description: "Match these items for the current company.",
            cancel_event: "cancel_opening_match",
            width: ModalWidth::Wide,
            body,
            ..Modal::default()
        })
    }
    fn failure(&mut self, e: ContractError) {
        self.message = Some(match e {
            ContractError::InvalidInput(fields) => fields
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            ContractError::Forbidden => "You do not have permission to match these items.".into(),
            _ => "The match could not be completed. Review the items again.".into(),
        });
    }
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        event: &str,
        value: &Value,
        available: (&[OpeningItem], &[StatementLineItem]),
    ) -> Result<bool> {
        let (items, lines) = available;
        match event {
            "opening_match_change" => {
                let values = params::form_values(value, "opening_match");
                self.item = values.get("item").and_then(|s| s.parse().ok());
                self.line = values.get("line").and_then(|s| s.parse().ok());
                if !lines.iter().any(|line| {
                    Some(line.line.id) == self.line
                        && items
                            .iter()
                            .any(|i| Some(i.id) == self.item && i.amount == line.line.amount)
                }) {
                    self.line = None;
                }
            }
            "cancel_opening_match" => self.review = None,
            "preview_opening_match" => {
                let Some(item) = self.item else {
                    self.message = Some("Choose an opening item.".into());
                    return Ok(false);
                };
                let Some(line) = self.line else {
                    self.message = Some("Choose a statement line.".into());
                    return Ok(false);
                };
                let input = json!({"company_id":company(scope)?,"bank_account_id":self.account,"opening_item_id":item,"statement_line_id":line});
                let call = Call {
                    scope,
                    credential,
                    service: PAGES,
                    event: "preview_opening_match",
                    contract: CONTRACT,
                };
                match pages.contracts.preview(&call, input.clone()).await {
                    Ok(answer) => {
                        let result = &answer["results"][0];
                        if result["valid"] == true {
                            self.review = Some(Review {
                                input,
                                facts: result["preview"]["result"]["changes"].clone(),
                            });
                            self.message = None;
                        } else {
                            self.message = Some(result["error"]["fields"].to_string());
                        }
                    }
                    Err(e) => self.failure(e),
                }
            }
            "commit_opening_match" => {
                if let Some(review) = self.review.take() {
                    let call = Call {
                        scope,
                        credential,
                        service: PAGES,
                        event: "commit_opening_match",
                        contract: CONTRACT,
                    };
                    match pages.contracts.commit(&call, review.input).await {
                        Ok(_) => {
                            self.item = None;
                            self.line = None;
                            self.message = None;
                            return Ok(true);
                        }
                        Err(e) => self.failure(e),
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(false)
    }
}
