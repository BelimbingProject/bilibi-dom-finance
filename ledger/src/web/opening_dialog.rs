//! Journals’ reviewed opening-balance dialog; the same contract serves MCP.
//! Bilimbi has no equivalent page; posting still uses its journal controls.
use super::{
    dialog_actions, error_text, event_type, no_company, render, Failure, Flash, Pages, Refusal,
    Review,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    params, Alert, BuiltinColumn, Button, ButtonVariant, Cell, ColumnsOutcome, Field, FlexRow,
    FlexTable, Form, Html, InputKind, ListColumns, Modal, ModalWidth, StatusKind,
};
use serde_json::{json, Value};

const CONTRACT: &str = "finance.ledger.opening_balances.submit";
const FIELDS: [&str; 4] = [
    "conversion_date",
    "currency_code",
    "source_reference",
    "csv",
];

pub(super) struct OpeningDialog {
    pages: Pages,
    values: Value,
    refusal: Refusal,
    flash: Flash,
    review: Option<Review>,
    columns: ListColumns,
    destination: Option<String>,
    /// The waiting opening journal the address names to correct.
    replaces: Option<i64>,
}

impl OpeningDialog {
    pub(super) fn new(pages: Pages, replaces: Option<i64>) -> Self {
        Self {
            pages,
            replaces,
            values: json!({}),
            refusal: Refusal::default(),
            flash: Flash::default(),
            review: None,
            destination: None,
            columns: ListColumns::mount(
                "opening-rows",
                vec![
                    BuiltinColumn::new("source", "Source ID"),
                    BuiltinColumn::new("account", "Mapped account"),
                    BuiltinColumn::new("debit", "Debit").right(),
                    BuiltinColumn::new("credit", "Credit").right(),
                ],
            )
            .expect("opening preview columns"),
        }
    }

    pub(super) fn replaces(&self) -> Option<i64> {
        self.replaces
    }

    pub(super) fn take_destination(&mut self) -> Option<String> {
        self.destination.take()
    }

    pub(super) async fn view(&mut self, scope: &Scope) -> Result<Html> {
        let mut page = Html::EMPTY;
        if Pages::company(scope).is_none() {
            page.push(&no_company("opening-no-company")?);
        } else if self.pages.allowed(scope, crate::SUBMIT_JOURNALS).await? {
            if self.values.get("currency_code").is_none() {
                if let Ok(state) = self.pages.book(scope).await? {
                    if let Some(book) = state.book {
                        self.values["currency_code"] = json!(book.currency_code);
                    }
                }
            }
            let mut body = render(Alert {
                id: "opening-boundary",
                kind: StatusKind::Info,
                body: Html::text(
                    "The conversion date is the first day of processing in Bilibi. Use the source closing trial balance from immediately before that date. After approval, import opening invoices, bills and the asset register from their own opening pages. Review control-account reconciliation and the YTD reporting bridge with the accountant.",
                ),
                ..Alert::default()
            })?;
            if self.replaces.is_some() {
                body.push(&render(Alert {
                    id: "opening-correcting",
                    kind: StatusKind::Warning,
                    body: Html::text(
                        "You are correcting your waiting opening journal. Submitting withdraws it, so it can never post, and this correction takes its place as the opening; it needs its own approval. Use a new source reference.",
                    ),
                    ..Alert::default()
                })?);
            }
            let messages = self.refusal.alert(&FIELDS);
            if !messages.is_empty() {
                body.push(&render(Alert {
                    id: "opening-refusal",
                    kind: StatusKind::Error,
                    body: Html::text(&messages.join(" ")),
                    ..Alert::default()
                })?);
            }
            for (name, label, kind, hint) in [
                (
                    "conversion_date",
                    "Conversion date",
                    InputKind::Date,
                    "The first day Bilibi owns transaction processing; its fiscal period must be open.",
                ),
                (
                    "currency_code",
                    "Book currency",
                    InputKind::Text,
                    "Use the book's currency. No currency conversion is performed.",
                ),
                (
                    "source_reference",
                    "Source reference",
                    InputKind::Text,
                    "A name for this batch. Reuse it if you retry; a corrected batch needs a new one.",
                ),
                (
                    "csv",
                    "Opening balances CSV",
                    InputKind::Textarea,
                    "Paste a CSV with the header source_id,account_code,debit,credit. Use the codes of active accounts and amounts without thousands separators. Up to 500 rows.",
                ),
            ] {
                let errors = self
                    .refusal
                    .field(name)
                    .iter()
                    .map(|m| error_text(name, m))
                    .collect::<Vec<_>>();
                let mut field = Field::in_form("opening", name)
                    .id(format!("opening-{name}"))
                    .label(label)
                    .kind(kind)
                    .value(self.values[name].as_str().unwrap_or(""))
                    .required()
                    .hint(hint)
                    .errors(errors);
                if name == "csv" {
                    field = field.attr("rows", "12");
                }
                body.push(&render(field)?);
            }
            let form = render(Form {
                id: "opening-form",
                event: "preview_opening",
                class: "mt-4",
                body,
                actions: {
                    let mut actions = render(Button {
                        event: "close_opening",
                        button_type: "button",
                        body: Html::text("Cancel"),
                        ..Button::default()
                    })?;
                    actions.push(&render(Button {
                        id: "opening-preview",
                        button_type: "submit",
                        variant: ButtonVariant::Primary,
                        body: Html::text("Preview opening balances"),
                        working: "Checking…",
                        ..Button::default()
                    })?);
                    dialog_actions(actions)?
                },
                ..Form::default()
            })?;
            if self.review.is_none() {
                let mut body = render(bilibi_ui::FlashGroup {
                    id: "opening-flash",
                    flash: self.flash.map(),
                })?;
                body.push(&form);
                page.push(&render(Modal {
                    id: "opening-modal",
                    title: "Opening balances",
                    description: "Submit one opening journal for approval.",
                    cancel_event: "close_opening",
                    width: ModalWidth::Wide,
                    body,
                    ..Modal::default()
                })?);
            }
        } else {
            page.push(&render(Alert {
                id: "opening-denied",
                kind: StatusKind::Info,
                body: Html::text(
                    "Submitting opening balances requires permission to submit journals.",
                ),
                ..Alert::default()
            })?);
        }
        if let Some(review) = &self.review {
            let facts = &review.facts;
            let units = facts["minor_units"]
                .as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or(Error::Internal)?;
            let records = facts["rows"].as_array().ok_or(Error::Internal)?;
            let mut rows = Vec::new();
            for (i, row) in records.iter().enumerate() {
                let text = |key: &str| row[key].as_str().unwrap_or("");
                rows.push(
                    FlexRow::new(i.to_string())
                        .cell("source", Cell::text(text("source_id")))
                        .slot(
                            "account",
                            render(super::AccountName {
                                name: text("account_name"),
                                code: text("account_code"),
                            })?,
                        )
                        .cell(
                            "debit",
                            Cell::text(super::view::amount(text("debit"), units)),
                        )
                        .cell(
                            "credit",
                            Cell::text(super::view::amount(text("credit"), units)),
                        ),
                );
            }
            let mut table = FlexTable::new("opening-rows", self.columns.flex_columns(), rows);
            table.caption = "Source balances mapped to the book";
            table.zoom = self.columns.zoom;
            table.suggestions = self.columns.flex_suggestions();
            table.add_query = &self.columns.add_query;
            let mut body = render(Alert {
                id: "opening-review-summary",
                kind: StatusKind::Info,
                body: Html::text(&format!(
                    "Conversion date {}. Currency {}. Source {}. Debits {}. Credits {}. {}",
                    facts["conversion_date"].as_str().unwrap_or(""),
                    facts["currency_code"].as_str().unwrap_or(""),
                    facts["source_reference"].as_str().unwrap_or(""),
                    super::view::amount(facts["debit"].as_str().unwrap_or(""), units),
                    super::view::amount(facts["credit"].as_str().unwrap_or(""), units),
                    if !facts["existing_journal"].is_null() {
                        "This source is already recorded. Continue to its journal without creating another.".to_owned()
                    } else if let Some(number) = facts["replaces_journal_number"].as_i64() {
                        format!(
                            "Submission withdraws journal {number} and posts nothing. This correction becomes the opening and needs separate journal approval."
                        )
                    } else {
                        "Submission posts nothing. Separate journal approval is required."
                            .to_owned()
                    }
                )),
                ..Alert::default()
            })?;
            body.push(&render(table)?);
            body.push(&render(Alert {
                id: "opening-review-excluded",
                kind: StatusKind::Warning,
                body: Html::text(facts["excluded"].as_str().unwrap_or("")),
                ..Alert::default()
            })?);
            let mut actions = render(Button {
                button_type: "button",
                event: "cancel_opening",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                id: "opening-submit",
                button_type: "button",
                event: "submit_opening",
                variant: ButtonVariant::Primary,
                body: Html::text(if facts["existing_journal"].is_null() {
                    "Submit for approval"
                } else {
                    "Open recorded journal"
                }),
                working: "Submitting…",
                ..Button::default()
            })?);
            body.push(&dialog_actions(actions)?);
            page.push(&render(Modal { id:"opening-review-modal", title:"Review opening balances",
                description:"Compare every mapped balance with the approved source trial balance before submitting.",
                cancel_event:"cancel_opening", width:ModalWidth::Wide, body, ..Modal::default() })?);
        }
        Ok(page)
    }

    fn failed(&mut self, failure: Failure) {
        match failure {
            Failure::Refused(refusal) => self.refusal = refusal,
            other => self.flash.error(other.text("Opening balances")),
        }
    }
}

impl OpeningDialog {
    pub(super) async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<()> {
        match event_type(value)? {
            "preview_opening" => {
                self.review = None;
                self.refusal = Refusal::default();
                // CSV exceeds the ordinary form parser's 1 KiB field limit.
                // The checked owner limit refuses the whole event instead of dropping it.
                let fields =
                    params::form_values_checked(value, "opening", crate::opening_csv::MAX_BYTES)
                        .map_err(Error::refused)?;
                self.values = json!(fields);
                let mut input = self.values.clone();
                if let Some(journal) = self.replaces {
                    input["replaces_journal_id"] = json!(journal);
                }
                match self
                    .pages
                    .review(scope, credential, "preview_opening", CONTRACT, input)
                    .await?
                {
                    Ok(review) => {
                        if let Some(rows) = review.facts["rows"].as_array() {
                            self.columns.load(rows, |row| {
                                row["source_id"].as_str().unwrap_or("").to_owned()
                            });
                        }
                        self.review = Some(review);
                    }
                    Err(failure) => self.failed(failure),
                }
            }
            "submit_opening" => {
                if let Some(review) = self.review.take() {
                    match self
                        .pages
                        .commit(scope, credential, "submit_opening", CONTRACT, review)
                        .await?
                    {
                        Ok(done) => {
                            if let Some(journal) =
                                done["results"][0]["result"]["result"]["record"]["id"].as_i64()
                            {
                                self.destination =
                                    Some(format!("/finance/ledger/journals/{journal}"));
                            } else {
                                self.flash.success(
                                    "The opening journal is recorded. Find it in Journals.",
                                );
                            }
                        }
                        Err(failure) => self.failed(failure),
                    }
                }
            }
            "cancel_opening" => self.review = None,
            "grid" => {
                if let ColumnsOutcome::Sort(_) = self.columns.handle(&params::string_values(value))
                {
                }
            }
            "clear-flash" => self.flash.dismiss(value),
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
