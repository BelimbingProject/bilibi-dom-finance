//! How a customer credit's actions read: its history table, the Apply, Refund
//! and Reverse dialog, and the review a person confirms, drawn from the
//! contract's own review facts.
use super::{amount, fact, number, text, verb, Credits, Draft};
use crate::web::{account_html, format as f, render, Account, INVOICES};
use crate::Document;
use bilibi_database::Result;
use bilibi_finance_ledger::{dates, EntryLink, MinorUnits, RecordAction};
use bilibi_ui::{
    Alert, Attrs, BadgeKind, Button, ButtonVariant, Card, Cell, Choice, Combobox, Fact, Facts, Field,
    FlexRow, FlexTable, Form, Html, InputKind, Modal, ModalWidth, SectionHeading, StatusKind,
    TableEmpty,
};
use serde_json::Value;

/// What one settlement did, in words: "Applied to INV-00007", "Refunded from
/// Cash at bank". A reversal names what it took back.
fn happened(event: &Value, events: &[Value], accounts: &[Account], on_credit: bool) -> String {
    let account = |id: &Value| {
        id.as_i64()
            .and_then(|id| accounts.iter().find(|a| a.id == id))
            .map_or_else(|| "a bank or cash account".to_owned(), |a| a.name.clone())
    };
    let describe = |e: &Value| match e["kind"].as_str() {
        Some("apply") if on_credit => format!("Applied to {}", text(&e["target_reference"])),
        Some("apply") => format!("Credit {} applied", text(&e["credit_reference"])),
        _ => format!("Refunded from {}", account(&e["cash_account_id"])),
    };
    if event["kind"] == "reverse" {
        let original = events
            .iter()
            .find(|e| e["id"] == event["reverses_settlement_id"]);
        return match original {
            Some(original) => format!("Reversed: {}", describe(original).to_lowercase()),
            None => "Reversed an earlier entry".into(),
        };
    }
    describe(event)
}

/// Customer credit applied to an invoice, as rows of its "Receipts and
/// credits" table: the credit, the date and the amount. A reversed
/// application stays listed, marked as reversed.
pub(in crate::web) fn applied_rows(d: &Document, units: Option<MinorUnits>) -> Result<Vec<FlexRow>> {
    let mut rows = Vec::new();
    for e in d
        .settlements
        .iter()
        .filter(|e| e["kind"] == "apply" && e["target_document_id"].as_i64() == Some(d.id))
    {
        let reversed = d
            .settlements
            .iter()
            .any(|r| r["reverses_settlement_id"] == e["id"]);
        let mut row = FlexRow::new(format!("s{}", e["id"]));
        row.dom_id = Some(format!("receivables-invoice-applied-credit-{}", e["id"]));
        rows.push(
            row.slot(
                "document",
                f::link(
                    &format!("{INVOICES}/{}", e["credit_document_id"]),
                    &format!("Credit {}", text(&e["credit_reference"])),
                )?,
            )
            .cell(
                "date",
                Cell::text(dates::display_text(&text(&e["effective_date"]))),
            )
            .slot("amount", f::tabular(&amount(number(&e["amount"]), units))?)
            .slot(
                "status",
                if reversed {
                    bilibi_ui::status_badge(BadgeKind::Warning, "Reversed")
                } else {
                    bilibi_ui::status_badge(BadgeKind::Success, "Applied")
                },
            ),
        );
    }
    Ok(rows)
}

impl Credits {
    /// The credit actions of a record header. A credit with something left
    /// offers Apply credit as its primary verb (returned) and Refund credit;
    /// an invoice still owed, for a customer with credit left, offers Apply
    /// credit beside its own actions.
    pub(in crate::web) fn header_actions(
        &self,
        d: &Document,
        owed: bool,
        here: &str,
        items: &mut Vec<RecordAction>,
    ) -> Result<Option<Html>> {
        if d.kind == "invoice" {
            if owed && !self.offered_total().is_zero() {
                items.push(RecordAction::here(
                    "arrow-right-circle",
                    "Apply credit",
                    here,
                    "apply_credit",
                ));
            }
            return Ok(None);
        }
        if !crate::is_credit(&d.kind) {
            return Ok(None);
        }
        if self.can_refund() {
            items.push(RecordAction::here(
                "receipt-refund",
                "Refund credit",
                here,
                "refund_credit",
            ));
        }
        if !self.can_apply() {
            return Ok(None);
        }
        Ok(Some(render(Button {
            id: "credit-apply",
            button_type: "button",
            event: "ask_credit_apply",
            variant: ButtonVariant::Primary,
            body: Html::text("Apply credit"),
            ..Button::default()
        })?))
    }

    /// On an invoice that is still owed: what its customer has in unused
    /// credit, with the way to apply it.
    pub(in crate::web) fn offer_fact(
        &self,
        d: &Document,
        figures: &f::Figures,
    ) -> Result<Option<Fact<'static>>> {
        let offered = self.offered_total();
        if d.kind != "invoice" || offered.is_zero() {
            return Ok(None);
        }
        let mut value = Html::text(&format!(
            "{} {} · ",
            figures.decimal(offered),
            d.currency_code
        ));
        value.push(&f::link(
            &format!("{INVOICES}/{}?do=apply_credit", d.id),
            "Apply credit",
        )?);
        Ok(Some(f::fact_html("Customer's credit", value)))
    }

    /// The history of a credit: every application, refund and reversal, with
    /// Reverse on what still stands. Nothing on any other document; an
    /// invoice lists the credit applied to it with its receipts.
    pub(in crate::web) fn history(
        &self,
        d: &Document,
        accounts: &[Account],
        units: Option<MinorUnits>,
    ) -> Result<Html> {
        if !crate::is_credit(&d.kind) || d.status != "issued" {
            return Ok(Html::EMPTY);
        }
        let events = &d.settlements;
        let mut rows = Vec::new();
        for e in events {
            let reversed = events
                .iter()
                .any(|r| r["reverses_settlement_id"] == e["id"]);
            let action = if reversed {
                bilibi_ui::status_badge(BadgeKind::Neutral, "Reversed")
            } else if self.allowed && e["kind"] != "reverse" {
                render(Button {
                    id: &format!("credit-reverse-{}", e["id"]),
                    event: "ask_credit_reverse",
                    name: "settlement_id",
                    value: &text(&e["id"]),
                    button_type: "button",
                    body: Html::text("Reverse"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            };
            let what = happened(e, events, accounts, true);
            let what = match e["target_document_id"].as_i64() {
                Some(target) if e["kind"] == "apply" => {
                    let mut html = Html::text("Applied to ");
                    html.push(&f::link(
                        &format!("{INVOICES}/{target}"),
                        &text(&e["target_reference"]),
                    )?);
                    html
                }
                _ => Html::text(&what),
            };
            let mut row = FlexRow::new(text(&e["id"]));
            row.dom_id = Some(format!("credit-settlement-{}", e["id"]));
            rows.push(
                row.cell(
                    "date",
                    Cell::text(dates::display_text(&text(&e["effective_date"]))),
                )
                .slot("what", what)
                .slot(
                    "amount",
                    f::tabular(&amount(number(&e["amount"]), units))?,
                )
                .slot(
                    "entry",
                    match e["entry_id"].as_i64() {
                        Some(id) => EntryLink::html(Some(id), e["entry_number"].as_i64())?,
                        None => Html::text("None needed"),
                    },
                )
                .slot("reverse", action),
            );
        }
        let mut table = FlexTable::new("credit-settlements", self.columns.flex_columns(), rows);
        table.event = "credit_grid";
        table.caption = "How this credit was used";
        table.framed = false;
        table.empty = Some(TableEmpty::Content(Html::text(
            "Nothing has been applied or refunded yet.",
        )));
        let mut body = render(SectionHeading {
            id: "credit-settlement-heading",
            title: "How this credit was used",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        body.push(&render(table)?);
        render(Card {
            id: "credit-settlement-card",
            role: "region",
            labelledby: "credit-settlement-heading",
            inner_class: "p-0",
            body,
            ..Card::default()
        })
    }

    /// The Apply, Refund or Reverse dialog while it is open.
    pub(in crate::web) fn dialog(
        &self,
        d: &Document,
        accounts: &[Account],
        units: Option<MinorUnits>,
    ) -> Result<Option<Html>> {
        let Some(draft) = &self.draft else {
            return Ok(None);
        };
        let name = crate::web::documents::reference(d);
        let mut body = Html::EMPTY;
        let general = draft
            .fields
            .iter()
            .filter(|(key, _)| {
                ![
                    "effective_date",
                    "amount",
                    "target_document_id",
                    "credit_document_id",
                    "cash_account_id",
                ]
                .contains(&key.as_str())
            })
            .flat_map(|(_, messages)| messages.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        if !general.is_empty() {
            body.push(&render(Alert {
                id: "credit-errors",
                kind: StatusKind::Error,
                body: Html::text(&general),
                ..Alert::default()
            })?);
        }
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.fields.get(key).cloned().unwrap_or_default();
        if let Some(review) = &draft.review {
            body.push(&review.banner);
            body.push(&self.review(draft, &review.facts, accounts, units)?);
        } else if draft.action == "reverse" {
            let original = d
                .settlements
                .iter()
                .find(|e| text(&e["id"]) == value("settlement_id"));
            if let Some(original) = original {
                body.push(&render(Sentence {
                    id: "credit-reverse-what",
                    text: &format!(
                        "{} of {} on {}. Reversing gives the amount back to the credit{}.",
                        happened(original, &d.settlements, accounts, true),
                        amount(number(&original["amount"]), units),
                        dates::display_text(&text(&original["effective_date"])),
                        if original["kind"] == "apply" {
                            ", and the invoice is unpaid by that much again"
                        } else {
                            ""
                        }
                    ),
                })?);
            }
            body.push(&render(self.date_field(
                draft,
                "Reversal date",
                "On or after the date being reversed.",
            ))?);
        } else {
            if draft.on_invoice {
                body.push(&render(
                    Combobox::new("credit-credit_document_id", "credit[credit_document_id]")
                        .label("Credit *")
                        .aria_label("Credit")
                        .placeholder("Choose a credit")
                        .value(value("credit_document_id"))
                        .options(
                            self.offered
                                .iter()
                                .map(|c| {
                                    Choice::new(text(&c["document_reference"]), text(&c["id"]))
                                        .detail(format!(
                                            "{} · {} left",
                                            dates::display_text(&text(&c["issue_date"])),
                                            amount(number(&c["available"]), units)
                                        ))
                                })
                                .collect(),
                        )
                        .errors(errors("credit_document_id"))
                        .attrs(Attrs {
                            required: true,
                            ..Attrs::default()
                        }),
                )?);
            } else if draft.action == "apply" {
                if !self.context["next_after_id"].is_null() || !value("search").is_empty() {
                    body.push(&render(
                        Field::named("credit[search]")
                            .label("Find an invoice")
                            .id("credit-search")
                            .hint("This customer has many unpaid invoices. Type part of a number to narrow the list below.")
                            .value(value("search")),
                    )?);
                }
                body.push(&render(
                    Combobox::new("credit-target_document_id", "credit[target_document_id]")
                        .label("Invoice *")
                        .aria_label("Invoice")
                        .placeholder("Choose an unpaid invoice")
                        .value(value("target_document_id"))
                        .options(
                            self.context["targets"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|t| {
                                    Choice::new(text(&t["document_reference"]), text(&t["id"]))
                                        .detail(format!(
                                            "due {} · {} unpaid",
                                            dates::display_text(&text(&t["due_date"])),
                                            amount(number(&t["available"]), units)
                                        ))
                                })
                                .collect(),
                        )
                        .errors(errors("target_document_id"))
                        .attrs(Attrs {
                            required: true,
                            ..Attrs::default()
                        }),
                )?);
            } else {
                body.push(&render(
                    Combobox::new("credit-cash_account_id", "credit[cash_account_id]")
                        .label("Paid from *")
                        .aria_label("Paid from")
                        .placeholder("Choose the bank or cash account")
                        .value(value("cash_account_id"))
                        .options(
                            self.context["cash_accounts"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|a| {
                                    Choice::new(text(&a["name"]), text(&a["id"]))
                                        .detail(text(&a["code"]))
                                })
                                .collect(),
                        )
                        .errors(errors("cash_account_id"))
                        .attrs(Attrs {
                            required: true,
                            ..Attrs::default()
                        }),
                )?);
            }
            let mut pair = render(
                Field::named("credit[amount]")
                    .label("Amount")
                    .id("credit-amount")
                    .attr("inputmode", "decimal")
                    .value(value("amount"))
                    .required()
                    .hint(if draft.on_invoice {
                        format!(
                            "This invoice has {} unpaid.",
                            d.outstanding
                                .map_or_else(|| "—".into(), |m| amount(m.amount(), units))
                        )
                    } else {
                        format!("{} is left on this credit.", amount(self.left(), units))
                    })
                    .errors(errors("amount")),
            )?;
            pair.push(&render(self.date_field(
                draft,
                "Date",
                if draft.action == "refund" {
                    "The day the money left the account."
                } else {
                    "The day the credit pays the invoice."
                },
            ))?);
            body.push(&f::two(pair)?);
        }
        let mut buttons = render(Button {
            id: "credit-cancel",
            button_type: "button",
            event: "cancel_credit",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        if draft.review.is_some() {
            buttons.push(&render(Button {
                id: "credit-back",
                button_type: "button",
                event: "back_credit",
                body: Html::text("Back"),
                ..Button::default()
            })?);
        }
        buttons.push(&render(Button {
            id: "credit-submit",
            button_type: "button",
            event: if draft.review.is_some() {
                "commit_credit"
            } else {
                "preview_credit"
            },
            variant: ButtonVariant::Primary,
            working: if draft.review.is_some() {
                "Saving…"
            } else {
                "Checking…"
            },
            body: Html::text(if draft.review.is_some() {
                verb(&draft.action)
            } else {
                "Review"
            }),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "credit-form",
            event: "change_credit",
            change: true,
            class: "mt-4",
            body,
            actions: f::actions(buttons)?,
        })?;
        let description = match (draft.action.as_str(), draft.on_invoice) {
            ("apply", true) => format!(
                "Pay {name} with credit this customer already has. No new money is received."
            ),
            ("apply", false) => format!(
                "Pay an invoice of this customer with credit {name}. No new money is received."
            ),
            ("refund", _) => format!("Record money paid back to the customer from credit {name}."),
            _ => format!("Take back one use of credit {name}."),
        };
        Ok(Some(render(Modal {
            id: "credit-settlement-modal",
            title: match draft.action.as_str() {
                "apply" => "Apply credit",
                "refund" => "Refund credit",
                _ => "Reverse this entry",
            },
            description: &description,
            cancel_event: "cancel_credit",
            width: ModalWidth::Narrow,
            body: form,
            ..Modal::default()
        })?))
    }

    fn date_field(&self, draft: &Draft, label: &str, hint: &str) -> Field {
        Field::named("credit[effective_date]")
            .label(label)
            .id("credit-effective_date")
            .kind(InputKind::Date)
            .value(
                draft
                    .values
                    .get("effective_date")
                    .cloned()
                    .unwrap_or_default(),
            )
            .required()
            .hint(hint)
            .errors(
                draft
                    .fields
                    .get("effective_date")
                    .cloned()
                    .unwrap_or_default(),
            )
    }

    /// What the write will do, from the contract's own review: the sentence,
    /// what is left afterwards and the journal it posts.
    fn review(
        &self,
        draft: &Draft,
        facts: &Value,
        accounts: &[Account],
        units: Option<MinorUnits>,
    ) -> Result<Html> {
        let money = number(&facts["amount"]);
        let credit = text(&facts["credit"]["document_reference"]);
        let target = text(&facts["target"]["document_reference"]);
        let currency = text(&facts["currency_code"]);
        let named = |id: &Value| -> Result<Html> {
            match id.as_i64() {
                Some(id) => account_html(accounts, id),
                None => Ok(Html::text("—")),
            }
        };
        let credit_account = &facts["credit"]["receivable_account_id"];
        let cash_account = if facts["original"].is_null() {
            &facts["input"]["cash_account_id"]
        } else {
            &facts["original"]["cash_account_id"]
        };
        let reversing = draft.action == "reverse";
        let sentence = match (draft.action.as_str(), target.is_empty()) {
            ("apply", _) => format!(
                "Apply {} {currency} of credit {credit} to invoice {target}.",
                amount(money, units)
            ),
            ("refund", _) => format!(
                "Refund {} {currency} of credit {credit} to the customer.",
                amount(money, units)
            ),
            (_, false) => format!(
                "Take back {} {currency} of credit {credit} applied to invoice {target}.",
                amount(money, units)
            ),
            (_, true) => format!(
                "Take back the refund of {} {currency} from credit {credit}.",
                amount(money, units)
            ),
        };
        let mut html = render(Sentence {
            id: "credit-review-sentence",
            text: &sentence,
        })?;
        let sign = if reversing { -money } else { money };
        let mut items = vec![fact(
            "Date",
            dates::display_text(&text(&facts["effective_date"])),
        )];
        items.push(fact(
            "Credit left afterwards",
            amount(number(&facts["available_credit"]) - sign, units),
        ));
        if !facts["available_target"].is_null() {
            items.push(fact(
                "Invoice unpaid afterwards",
                amount(number(&facts["available_target"]) - sign, units),
            ));
        }
        if !cash_account.is_null() {
            items.push(f::fact_html(
                if reversing { "Paid back into" } else { "Paid from" },
                named(cash_account)?,
            ));
        }
        items.push(if facts["posts"] == true {
            let mut journal = Html::text(if reversing {
                "Reverses the journal between "
            } else if cash_account.is_null() {
                "Moves the amount from "
            } else {
                "Takes the amount out of "
            });
            journal.push(&named(credit_account)?);
            journal.push(&Html::text(if cash_account.is_null() {
                " to "
            } else {
                " and "
            }));
            journal.push(&named(if cash_account.is_null() {
                &facts["target"]["receivable_account_id"]
            } else {
                cash_account
            })?);
            f::fact_html("Journal", journal)
        } else {
            fact(
                "Journal",
                "None needed: the credit and the invoice are on the same account.",
            )
        });
        html.push(&render(Facts {
            id: "credit-review-facts",
            items,
        })?);
        if !facts["approval"]["approval_exception"].is_null() {
            html.push(&render(Alert {
                id: "credit-review-exception",
                kind: StatusKind::Warning,
                body: Html::text(
                    "You also prepared this credit, so this is recorded as an approval exception for an independent reviewer.",
                ),
                ..Alert::default()
            })?);
        }
        Ok(html)
    }

}

#[derive(askama::Template)]
#[template(
    source = r#"<p id="{{ id }}" class="text-sm text-ink-strong">{{ text }}</p>"#,
    ext = "html"
)]
struct Sentence<'a> {
    id: &'a str,
    text: &'a str,
}

