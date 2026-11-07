//! The opening balances card: the conversion date and source reference, the
//! paste box with its column step, the grid of every account with live
//! totals, and the review before submitting; or, once submitted, the
//! journal's balances.
use super::grid::{Amount, Draft, Side, FORM};
use super::{date, Opening, Progress, Stage, PASTE_FORM};
use crate::web::{account_name, dialog_actions, error_text, render, view, Review};
use crate::{MinorUnits, Money};
use askama::Template;
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_ui::{
    Alert, Button, ButtonVariant, Card, Cell, Field, FlexRow, FlexTable, Form, Html, InputKind,
    ListColumns, Modal, ModalWidth, SectionHeading, StatusKind,
};
use serde_json::Value;
use std::collections::BTreeMap;

impl Opening {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn balances(
        &mut self,
        stage: Stage,
        progress: Option<&Progress>,
        journal: Option<&view::Journal>,
        accounts: &[view::Account],
        units: MinorUnits,
        can_submit: bool,
        display: &DisplayContext,
    ) -> Result<Html> {
        let editable = can_submit && matches!(stage, Stage::Open | Stage::Correcting);
        let mut body = Html::EMPTY;
        let (title, description) = match stage {
            Stage::Correcting => ("Correct the account balances", "Submitting withdraws your waiting journal, so it can never post, and this correction takes its place. It needs its own approval."),
            Stage::Open if editable => ("Account balances", "Each account's balance on the day before the conversion date, from the closing trial balance of the system you used before. Type them in or paste them from a spreadsheet."),
            Stage::Open => ("Account balances", "Entering opening balances needs permission to submit journals."),
            Stage::Waiting => ("Account balances", "The balances in the journal waiting for approval."),
            Stage::Posted | Stage::Reversed => ("Account balances", "The balances the opening journal posted."),
        };
        body.push(&render(SectionHeading {
            id: "opening-balances-heading",
            title,
            description,
            ..SectionHeading::default()
        })?);
        if editable {
            body.push(&self.form()?);
            body.push(&self.paste_box(accounts, units)?);
            body.push(&grid_table(
                &mut self.grid,
                &self.draft,
                &self.row_errors,
                accounts,
                units,
                true,
            )?);
            body.push(&self.totals(accounts, units)?);
            let (debits, credits, readable) = self.draft.totals(accounts, units);
            let ready = readable && !debits.is_zero() && debits == credits;
            let mut buttons = render(Button {
                id: "opening-clear",
                button_type: "button",
                event: "clear_grid",
                disabled: self.draft.cells.is_empty(),
                body: Html::text("Clear amounts"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                id: "opening-preview",
                button_type: "button",
                event: "preview_opening",
                variant: ButtonVariant::Primary,
                disabled: !ready,
                working: "Checking…",
                body: Html::text(if stage == Stage::Correcting {
                    "Submit correction"
                } else {
                    "Submit for approval"
                }),
                ..Button::default()
            })?);
            body.push(&actions(buttons)?);
        } else if let Some(journal) = journal.filter(|_| !matches!(stage, Stage::Open)) {
            let shown = Draft::from_lines(&journal.entry_date, &journal.lines, units);
            body.push(&render(Facts {
                text: &format!(
                    "Conversion date {}. Source {}.",
                    date(display, &journal.entry_date),
                    journal.reference.as_deref().unwrap_or("")
                ),
            })?);
            body.push(&grid_table(
                &mut self.grid,
                &shown,
                &BTreeMap::new(),
                accounts,
                units,
                false,
            )?);
            let mut buttons = Html::EMPTY;
            if let Some(p) = progress {
                buttons.push(&render(Button {
                    id: "opening-journal",
                    href: &format!("/finance/ledger/journals/{}", p.journal_id),
                    variant: if stage == Stage::Waiting {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Secondary
                    },
                    body: Html::text(&format!("Go to journal {}", p.journal_number)),
                    ..Button::default()
                })?);
            }
            body.push(&actions(buttons)?);
        }
        render(Card {
            id: "opening-balances",
            class: "mt-4",
            role: "region",
            labelledby: "opening-balances-heading",
            body,
            ..Card::default()
        })
    }

    /// The conversion date and source reference; the grid's cells join this
    /// form through their `form` attribute.
    pub(super) fn form(&self) -> Result<Html> {
        let messages = self.refusal.alert(&["conversion_date", "source_reference"]);
        let mut body = Html::EMPTY;
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: "opening-refusal",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&messages.join(" ")),
            })?);
        }
        let errors = |name: &str| -> Vec<String> {
            self.refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect()
        };
        let mut fields = render(
            Field::in_form(FORM, "conversion_date")
                .id("opening-conversion-date")
                .label("Conversion date")
                .kind(InputKind::Date)
                .value(&self.draft.date)
                .required()
                .hint("The first day these books take over. Its fiscal period must be open.")
                .errors(errors("conversion_date"))
                .wrapper_class("mb-0"),
        )?;
        fields.push(&render(
            Field::in_form(FORM, "source_reference")
                .id("opening-source-reference")
                .label("Source reference")
                .value(&self.draft.reference)
                .required()
                .hint("Names this set of balances, such as the old system's report. A correction needs a new one.")
                .errors(errors("source_reference"))
                .attr("data-debounce", "400")
                .wrapper_class("mb-0"),
        )?);
        render(Form {
            id: "opening-form",
            event: "opening_change",
            change: true,
            class: "mt-4",
            body: render(TwoUp { fields })?,
            actions: Html::EMPTY,
        })
    }

    pub(super) fn paste_box(&self, accounts: &[view::Account], units: MinorUnits) -> Result<Html> {
        let mut html = render(Form {
            id: "opening-paste-form",
            event: "paste_rows",
            change: true,
            class: "mt-4",
            body: render(
                Field::in_form(PASTE_FORM, "text")
                    .id("opening-paste")
                    .label("Paste from a spreadsheet")
                    .kind(InputKind::Textarea)
                    .value(&self.paste_text)
                    .hint("Copy the rows of the old trial balance, with or without its heading row, and paste them here. You then check which column is which.")
                    .attr("rows", "3")
                    .attr("data-debounce", "500")
                    .wrapper_class("mb-0"),
            )?,
            actions: Html::EMPTY,
        })?;
        if let Some(paste) = &self.paste {
            let mut section = paste.view(accounts, units)?;
            let mut buttons = render(Button {
                id: "opening-paste-discard",
                button_type: "button",
                event: "discard_paste",
                body: Html::text("Discard"),
                ..Button::default()
            })?;
            if paste.refusal.is_none() {
                buttons.push(&render(Button {
                    id: "opening-paste-fill",
                    button_type: "button",
                    event: "fill_grid",
                    variant: ButtonVariant::Primary,
                    body: Html::text("Fill the grid"),
                    ..Button::default()
                })?);
            }
            section.push(&actions(buttons)?);
            html.push(&render(Card {
                id: "opening-paste-columns",
                class: "mt-3",
                role: "region",
                labelledby: "opening-paste-columns-heading",
                body: {
                    let mut b = render(SectionHeading {
                        id: "opening-paste-columns-heading",
                        title: "Check the pasted columns",
                        description: "Say which column holds what. The grid is filled only when you choose Fill the grid.",
                        ..SectionHeading::default()
                    })?;
                    b.push(&section);
                    b
                },
                ..Card::default()
            })?);
        }
        Ok(html)
    }

    pub(super) fn totals(&self, accounts: &[view::Account], units: MinorUnits) -> Result<Html> {
        let (debits, credits, readable) = self.draft.totals(accounts, units);
        let difference = debits - credits;
        let mut text = if !readable {
            "Some amounts are not numbers yet; see the marked cells.".to_owned()
        } else if debits.is_zero() && credits.is_zero() {
            "No balances yet. Debits and credits must be equal before you can submit.".to_owned()
        } else if difference.is_zero() {
            format!(
                "Debits and credits are both {}. They match.",
                debits.display(units)
            )
        } else {
            format!(
                "Debits {}, credits {}. The difference is {} more {}; they must be equal before you can submit.",
                debits.display(units),
                credits.display(units),
                difference.abs().display(units),
                if difference.is_positive() { "in debits" } else { "in credits" },
            )
        };
        let left_out = self.draft.left_out(accounts, units);
        if !left_out.is_empty() {
            let listed: Vec<String> = left_out
                .iter()
                .map(|(account, money)| {
                    format!(
                        "{} ({} {})",
                        view::account_label(account),
                        money.abs().display(units),
                        if money.is_positive() { "debit" } else { "credit" }
                    )
                })
                .collect();
            text.push_str(&format!(
                " Left out because it no longer takes postings: {}.",
                listed.join("; ")
            ));
        }
        render(Alert {
            id: "opening-totals",
            kind: if readable && difference.is_zero() && !debits.is_zero() {
                StatusKind::Success
            } else {
                StatusKind::Info
            },
            class: "mt-3",
            body: Html::text(&text),
        })
    }

    pub(super) fn review_modal(
        &self,
        review: &Review,
        units: MinorUnits,
        display: &DisplayContext,
    ) -> Result<Html> {
        let facts = &review.facts;
        let money = |v: &Value| {
            v.as_str()
                .and_then(|s| Money::parse(s, units).ok())
                .filter(|m| !m.is_zero())
                .map(|m| m.display(units))
                .unwrap_or_default()
        };
        let mut rows = Vec::new();
        for (i, row) in facts["rows"].as_array().into_iter().flatten().enumerate() {
            let text = |key: &str| row[key].as_str().unwrap_or("");
            rows.push(
                FlexRow::new(i.to_string())
                    .slot(
                        "account",
                        render(crate::web::AccountName {
                            name: text("account_name"),
                            code: text("account_code"),
                        })?,
                    )
                    .cell("debit", Cell::text(money(&row["debit"])))
                    .cell("credit", Cell::text(money(&row["credit"]))),
            );
        }
        let mut table = FlexTable::new("opening-review-rows", self.reviewed.flex_columns(), rows);
        table.caption = "Balances to submit";
        table.zoom = self.reviewed.zoom;
        let existing = !facts["existing_journal"].is_null();
        let mut body = render(Alert {
            id: "opening-review-summary",
            kind: StatusKind::Info,
            body: Html::text(&format!(
                "Conversion date {}. Source {}. Debits {} and credits {}. {}",
                date(display, facts["conversion_date"].as_str().unwrap_or("")),
                facts["source_reference"].as_str().unwrap_or(""),
                money(&facts["debit"]),
                money(&facts["credit"]),
                if existing {
                    "These balances are already recorded under this source reference, so nothing new is submitted.".to_owned()
                } else if let Some(number) = facts["replaces_journal_number"].as_i64() {
                    format!("Journal {number} is withdrawn and this correction waits for approval in its place. Nothing posts yet.")
                } else {
                    "The journal waits for another person to approve it. Nothing posts yet."
                        .to_owned()
                }
            )),
            ..Alert::default()
        })?;
        body.push(&render(table)?);
        let mut buttons = render(Button {
            button_type: "button",
            event: "cancel_opening",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "opening-submit",
            button_type: "button",
            event: "submit_opening",
            variant: ButtonVariant::Primary,
            working: "Submitting…",
            body: Html::text(if existing {
                "Show the recorded journal"
            } else {
                "Submit for approval"
            }),
            ..Button::default()
        })?);
        body.push(&actions(buttons)?);
        render(Modal {
            id: "opening-review-modal",
            title: "Submit the opening balances?",
            description: "Compare each balance with the old system's closing trial balance before you submit.",
            cancel_event: "cancel_opening",
            width: ModalWidth::Wide,
            body,
            ..Modal::default()
        })
    }
}

/// Every account with its debit and credit: inputs while the balances can be
/// edited, the journal's figures once submitted (then only the accounts it fills).
fn grid_table(
    columns: &mut ListColumns,
    draft: &Draft,
    row_errors: &BTreeMap<i64, String>,
    accounts: &[view::Account],
    units: MinorUnits,
    editable: bool,
) -> Result<Html> {
    let mut rows = Vec::new();
    for account in accounts.iter().filter(|a| a.postable()) {
        let label = view::account_label(account);
        let mut row = FlexRow::new(account.id.to_string())
            .cell("account", Cell::text(&label))
            .slot("account", account_name(account)?)
            .cell("type", Cell::text(view::type_label(&account.account_type)));
        let refused = match draft.amount(account.id, units) {
            Amount::Refused(side, message) => Some((side, message)),
            _ => None,
        };
        for side in [Side::Debit, Side::Credit] {
            let text = draft.cell(side, account.id);
            if editable {
                let mut errors: Vec<String> = refused
                    .iter()
                    .filter(|(s, _)| *s == side)
                    .map(|(_, m)| error_text(side.key(), m))
                    .collect();
                if side == Side::Debit {
                    errors.extend(row_errors.get(&account.id).cloned());
                }
                let field = Field::in_form(FORM, &Draft::cell_key(side, account.id))
                    .id(format!("opening-{}-{}", side.key(), account.id))
                    .aria_label(format!(
                        "{} for {label}",
                        if side == Side::Debit {
                            "Debit"
                        } else {
                            "Credit"
                        }
                    ))
                    .value(text)
                    .attr("inputmode", "decimal")
                    .attr("data-debounce", "400")
                    .attr("form", "opening-form")
                    .wrapper_class("mb-0")
                    .errors(errors);
                row = row
                    .cell(side.key(), Cell::text(text))
                    .slot(side.key(), render(field)?);
            } else {
                let shown = match draft.amount(account.id, units) {
                    Amount::Debit(m) if side == Side::Debit => m.display(units),
                    Amount::Credit(m) if side == Side::Credit => m.display(units),
                    _ => String::new(),
                };
                row = row
                    .cell(side.key(), Cell::text(&shown))
                    .slot(side.key(), crate::web::figure(&shown)?);
            }
        }
        row.dom_id = Some(format!("opening-row-{}", account.id));
        if editable
            || !draft.cell(Side::Debit, account.id).is_empty()
            || !draft.cell(Side::Credit, account.id).is_empty()
        {
            rows.push(row);
        }
    }
    let (debits, credits, _) = draft.totals(accounts, units);
    let mut total = FlexRow::new("total")
        .slot("account", render(Strong { text: "Total" })?)
        .slot(
            "debit",
            render(Strong {
                text: &debits.display(units),
            })?,
        )
        .slot(
            "credit",
            render(Strong {
                text: &credits.display(units),
            })?,
        );
    total.dom_id = Some("opening-row-total".into());
    rows.push(total);
    let keys: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
    columns.load(&keys, Clone::clone);
    let mut table = FlexTable::new("opening-grid", columns.flex_columns(), rows);
    table.caption = "Opening balance of each account";
    table.class = "mt-4";
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    render(table)
}
/// A row of buttons at the end of a section, with the standard gap above it.
fn actions(buttons: Html) -> Result<Html> {
    render(Spaced {
        body: dialog_actions(buttons)?,
    })
}

#[derive(Template)]
#[template(source = r#"<div class="mt-3">{{ body|safe }}</div>"#, ext = "html")]
struct Spaced {
    body: Html,
}

#[derive(Template)]
#[template(
    source = r#"<strong class="font-semibold tabular-nums">{{ text }}</strong>"#,
    ext = "html"
)]
struct Strong<'a> {
    text: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<p class="mt-2 text-sm text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Facts<'a> {
    text: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<div class="grid gap-3 sm:grid-cols-2">{{ fields|safe }}</div>"#,
    ext = "html"
)]
struct TwoUp {
    fields: Html,
}
