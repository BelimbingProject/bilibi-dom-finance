//! Opening balances: one page that brings a company's balances over from the
//! system it used before. Step 1 is a grid of every account with a debit and
//! a credit column, live totals and the difference, that the person types into
//! or pastes a spreadsheet range into (`paste`, with the bank statement
//! import's column step). Submitting sends the grid through
//! `finance.ledger.opening_balances.submit`, the contract agents call, so the
//! same validation and the separate approval apply. Steps 2 to 4 (unpaid
//! invoices, unpaid bills, assets) are a checklist on the same page: each says
//! what it waits for, and once the balances journal is posted links to the
//! owner's own import. The page only links there; whether one was imported is
//! the ledger's own recognition of the opening account.
mod grid;
mod paste;
mod sections;
mod steps;

use super::{
    event_type, no_company, render, shell, unavailable, view, Failure, Flash, Pages, Read, Refusal,
    Review,
};
use crate::{MinorUnits, SUBMIT_JOURNALS};
use bilibi_database::{Error, Result};
use bilibi_datetime::DisplayContext;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, BuiltinColumn, Button, ButtonVariant, ColumnsOutcome, EmptyState, Header,
    Html, ListColumns, PageVariant,
};
use grid::Draft;
use paste::Paste;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub(super) const PATH: &str = "/finance/ledger/opening-balances";
const CONTRACT: &str = "finance.ledger.opening_balances.submit";
const PASTE_FORM: &str = "opening_paste";

/// Where the opening journal stands, from `opening_balances.get`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Nothing submitted, or the last one was rejected or withdrawn.
    Open,
    /// The person's own waiting journal, being corrected.
    Correcting,
    Waiting,
    Posted,
    Reversed,
}

#[derive(serde::Deserialize)]
struct Progress {
    journal_id: i64,
    journal_number: i64,
    status: String,
    reversed_by_entry_id: Option<i64>,
    #[serde(default)]
    imported: Vec<Imported>,
}

#[derive(serde::Deserialize)]
struct Imported {
    owner_module: String,
    accounts: i64,
}

/// The page's state: the draft grid, a paste being matched, and the review
/// waiting for an answer.
pub(in crate::web) struct Opening {
    pages: Pages,
    /// The waiting journal the address asks to correct.
    correct: Option<i64>,
    /// The journal the draft was last filled from, so it is filled once.
    filled_from: Option<i64>,
    draft: Draft,
    paste: Option<Paste>,
    paste_text: String,
    refusal: Refusal,
    /// The contract's refusal of one source row, beside that account.
    row_errors: BTreeMap<i64, String>,
    flash: Flash,
    review: Option<Review>,
    grid: ListColumns,
    steps: ListColumns,
    reviewed: ListColumns,
}

impl Opening {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let columns = |id: &str, list: Vec<BuiltinColumn>| {
            ListColumns::mount(id, list).map_err(|_| Error::Internal)
        };
        Ok(Self {
            pages,
            correct: query.get("correct").and_then(|v| v.parse().ok()),
            filled_from: None,
            draft: Draft::default(),
            paste: None,
            paste_text: String::new(),
            refusal: Refusal::default(),
            row_errors: BTreeMap::new(),
            flash: Flash::default(),
            review: None,
            grid: columns(
                "opening-grid",
                vec![
                    BuiltinColumn::new("account", "Account"),
                    BuiltinColumn::new("type", "Type"),
                    BuiltinColumn::new("debit", "Debit").right(),
                    BuiltinColumn::new("credit", "Credit").right(),
                ],
            )?,
            steps: columns(
                "opening-steps",
                vec![
                    BuiltinColumn::new("step", "Step"),
                    BuiltinColumn::new("status", "Status"),
                    BuiltinColumn::new("detail", "What it needs"),
                ],
            )?,
            reviewed: columns(
                "opening-review-rows",
                vec![
                    BuiltinColumn::new("account", "Account"),
                    BuiltinColumn::new("debit", "Debit").right(),
                    BuiltinColumn::new("credit", "Credit").right(),
                ],
            )?,
        })
    }

    async fn progress(&self, scope: &Scope) -> Result<std::result::Result<Option<Progress>, Read>> {
        Ok(
            match self
                .pages
                .read(scope, "finance.ledger.opening_balances.get", json!({}))
                .await?
            {
                Read::Found(value) => Ok(view::parse(value["opening"].clone())),
                other => Err(other),
            },
        )
    }

    async fn journal(&self, scope: &Scope, id: i64) -> Result<Option<view::Journal>> {
        Ok(
            match self
                .pages
                .read(
                    scope,
                    "finance.ledger.journal.get",
                    json!({"journal_id": id}),
                )
                .await?
            {
                Read::Found(value) => view::parse(value),
                _ => None,
            },
        )
    }

    fn stage(
        &self,
        progress: Option<&Progress>,
        journal: Option<&view::Journal>,
        user: i64,
    ) -> Stage {
        let Some(p) = progress else {
            return Stage::Open;
        };
        match p.status.as_str() {
            "submitted"
                if self.correct == Some(p.journal_id)
                    && journal.is_some_and(|j| j.submitted_by_user_id == user) =>
            {
                Stage::Correcting
            }
            "submitted" => Stage::Waiting,
            "approved" if p.reversed_by_entry_id.is_some() => Stage::Reversed,
            "approved" => Stage::Posted,
            _ => Stage::Open,
        }
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let mut actions = Html::EMPTY;
        let mut correcting = None;
        let subtitle = self.pages.company_name(scope).await?;
        if let Some(company) = Pages::company(scope) {
            match self.ready(scope, company).await? {
                Err(html) => body.push(&html),
                Ok((book, accounts)) => {
                    let units =
                        MinorUnits::new(i64::from(book.minor_units)).ok_or(Error::Internal)?;
                    match self.progress(scope).await? {
                        Err(refused) => body.push(&unavailable(
                            "opening-refused",
                            "opening balances",
                            &refused,
                        )?),
                        Ok(progress) => {
                            let journal = match &progress {
                                Some(p) => self.journal(scope, p.journal_id).await?,
                                None => None,
                            };
                            let stage =
                                self.stage(progress.as_ref(), journal.as_ref(), scope.user());
                            if self.correct.is_some() && stage != Stage::Correcting {
                                self.correct = None;
                                self.flash.error("Only the person who submitted the opening journal can correct it, and only while it waits for approval.");
                            }
                            if stage == Stage::Correcting {
                                correcting = progress.as_ref().map(|p| p.journal_id);
                            }
                            self.prefill(stage, journal.as_ref(), units);
                            let can_submit = self.pages.allowed(scope, SUBMIT_JOURNALS).await?;
                            if let (Stage::Waiting, Some(p), Some(j)) = (stage, &progress, &journal)
                            {
                                if can_submit && j.submitted_by_user_id == scope.user() {
                                    actions.push(&render(Button {
                                        id: "opening-correct",
                                        href: &format!("{PATH}?correct={}", p.journal_id),
                                        body: Html::text("Correct"),
                                        ..Button::default()
                                    })?);
                                }
                            }
                            body.push(&self.checklist(stage, progress.as_ref())?);
                            let display = self.pages.display(scope).await?;
                            body.push(&self.balances(
                                stage,
                                progress.as_ref(),
                                journal.as_ref(),
                                &accounts,
                                units,
                                can_submit,
                                &display,
                            )?);
                            if let Some(review) = &self.review {
                                body.push(&self.review_modal(review, units, &display)?);
                            }
                        }
                    }
                }
            }
        } else {
            body.push(&no_company("opening-no-company")?);
        }
        actions.push(&render(BackLink {
            href: "/finance/setup",
            id: "opening-back",
            title: "Back to Finance setup",
            ..BackLink::default()
        })?);
        let mut page = render(Header {
            title: Html::text("Opening balances"),
            subtitle: &subtitle,
            title_actions: Html::EMPTY,
            actions: std::mem::take(&mut actions),
        })?;
        page.push(&body);
        let mut query = HashMap::new();
        if let Some(id) = correcting {
            query.insert("correct".into(), id.to_string());
        }
        Ok(LiveBody {
            html: shell("ledger-opening", PageVariant::Detail, &self.flash, page)?,
            query,
            ..LiveBody::default()
        })
    }

    /// The book and its chart, or what the page shows instead.
    async fn ready(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<std::result::Result<(view::BookRow, Vec<view::Account>), Html>> {
        let book = match self.pages.book(scope).await? {
            Err(refused) => {
                return Ok(Err(unavailable(
                    "opening-book-refused",
                    "the book",
                    &refused,
                )?))
            }
            Ok(state) => state.book,
        };
        let Some(book) = book else {
            let mut html = render(
                EmptyState::missing(
                    "The book is not open yet",
                    "Open the company's book with its currency first; its balances come next.",
                )
                .id("opening-no-book"),
            )?;
            html.push(&render(Button {
                id: "opening-open-book",
                href: &format!("/finance/ledger/books/{company}"),
                variant: ButtonVariant::Primary,
                body: Html::text("Open the book"),
                ..Button::default()
            })?);
            return Ok(Err(html));
        };
        let accounts = match self.pages.chart(scope).await? {
            Ok(accounts) => accounts,
            Err(refused) => {
                return Ok(Err(unavailable(
                    "opening-chart-refused",
                    "the chart of accounts",
                    &refused,
                )?))
            }
        };
        // The whole chart: the grid lists the accounts that take postings, and
        // a paste that names a group or an inactive account says so.
        let mut accounts = accounts;
        if !accounts.iter().any(view::Account::postable) {
            let mut html = render(
                EmptyState::missing(
                    "The chart has no accounts yet",
                    "Each balance goes on an account. Add the chart of accounts, then come back to fill in the balances.",
                )
                .id("opening-no-accounts"),
            )?;
            html.push(&render(Button {
                id: "opening-chart",
                href: "/finance/ledger/accounts",
                variant: ButtonVariant::Primary,
                body: Html::text("Go to Chart of accounts"),
                ..Button::default()
            })?);
            return Ok(Err(html));
        }
        accounts.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(Ok((book, accounts)))
    }

    /// Fills the grid once from the journal it continues: the person's own
    /// waiting journal being corrected, or a rejected or withdrawn one.
    fn prefill(&mut self, stage: Stage, journal: Option<&view::Journal>, units: MinorUnits) {
        let Some(journal) = journal else {
            return;
        };
        let continues = match stage {
            Stage::Correcting => true,
            Stage::Open => matches!(journal.status.as_str(), "rejected" | "withdrawn"),
            _ => false,
        };
        if continues && self.filled_from != Some(journal.id) {
            self.filled_from = Some(journal.id);
            self.draft = Draft::from_lines(&journal.entry_date, &journal.lines, units);
        }
    }

    /// The contract's refusal: a source row's message goes beside its account
    /// (row 2 is the first account the grid fills); the rest beside its field.
    fn refused(&mut self, refusal: Refusal, accounts: &[view::Account], units: MinorUnits) {
        let rows = self.draft.rows(accounts, units);
        let mut kept = Refusal::default();
        for (field, messages) in refusal.fields {
            for message in messages {
                let row = (field == "csv")
                    .then(|| message.strip_prefix("row "))
                    .flatten()
                    .and_then(|rest| rest.split_once(": "))
                    .and_then(|(n, text)| Some((n.parse::<usize>().ok()?, text)));
                match row.and_then(|(n, text)| Some((rows.get(n.checked_sub(2)?)?.0.id, text))) {
                    Some((account, text)) => {
                        self.row_errors
                            .insert(account, bilibi_ui::error_sentence("", text));
                    }
                    None => kept
                        .fields
                        .entry(if field == "csv" {
                            "base".into()
                        } else {
                            field.clone()
                        })
                        .or_default()
                        .push(message),
                }
            }
        }
        self.refusal = kept;
    }

    async fn units_and_accounts(
        &self,
        scope: &Scope,
    ) -> Result<Option<(MinorUnits, String, Vec<view::Account>)>> {
        let Some(company) = Pages::company(scope) else {
            return Ok(None);
        };
        Ok(match self.ready(scope, company).await? {
            Ok((book, accounts)) => MinorUnits::new(i64::from(book.minor_units))
                .map(|u| (u, book.currency_code, accounts)),
            Err(_) => None,
        })
    }

    async fn event(&mut self, r: &LiveRequest<'_, Scope>, value: &Value) -> Result<()> {
        let kind = event_type(value)?;
        match kind {
            "opening_change" => {
                self.draft.read(value);
                self.row_errors.clear();
            }
            "paste_rows" => {
                let text = value
                    .get(format!("{PASTE_FORM}[text]"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text.len() > crate::columns::MAX_BYTES {
                    self.paste_text.clear();
                    self.paste = None;
                    self.flash.error(
                        "The pasted text is larger than 128 KiB. Paste the trial balance in parts.",
                    );
                } else if text.trim().is_empty() {
                    self.paste_text.clear();
                    self.paste = None;
                } else if text != self.paste_text {
                    let codes = self
                        .units_and_accounts(r.scope)
                        .await?
                        .map(|(_, _, accounts)| accounts.into_iter().map(|a| a.code).collect())
                        .unwrap_or_default();
                    self.paste_text = text.to_owned();
                    self.paste = Some(Paste::new(text, codes));
                }
            }
            "paste_headed" => {
                if let Some(paste) = &mut self.paste {
                    paste.set_headed(value["on"].as_str() == Some("true"));
                }
            }
            "paste_columns" => {
                if let Some(paste) = &mut self.paste {
                    paste.choose(&params::form_values(value, paste::FORM));
                }
            }
            "discard_paste" => {
                self.paste = None;
                self.paste_text.clear();
            }
            "fill_grid" => {
                if let (Some(paste), Some((units, _, accounts))) =
                    (self.paste.take(), self.units_and_accounts(r.scope).await?)
                {
                    let (filled, left) = paste.apply(&mut self.draft, &accounts, units);
                    self.paste_text.clear();
                    self.row_errors.clear();
                    let left = match left.len() {
                        0 => String::new(),
                        1 => " 1 row was left out; type its amount in the grid if it belongs on an account.".into(),
                        n => format!(" {n} rows were left out; type their amounts in the grid if they belong on an account."),
                    };
                    self.flash.success(format!(
                        "Filled {filled} {} from the paste. Check the totals below.{left}",
                        if filled == 1 { "account" } else { "accounts" }
                    ));
                }
            }
            "clear_grid" => {
                self.draft.cells.clear();
                self.row_errors.clear();
            }
            "preview_opening" => {
                self.review = None;
                self.refusal = Refusal::default();
                self.row_errors.clear();
                let Some((units, currency, accounts)) = self.units_and_accounts(r.scope).await?
                else {
                    return Ok(());
                };
                let input = self.draft.input(&accounts, units, &currency, self.correct);
                match self
                    .pages
                    .review(r.scope, r.credential, "preview_opening", CONTRACT, input)
                    .await?
                {
                    Ok(review) => {
                        if let Some(rows) = review.facts["rows"].as_array() {
                            self.reviewed.load(rows, |row| {
                                row["source_id"].as_str().unwrap_or("").to_owned()
                            });
                        }
                        self.review = Some(review);
                    }
                    Err(Failure::Refused(refusal)) => self.refused(refusal, &accounts, units),
                    Err(other) => self.flash.error(other.text("The opening balances")),
                }
            }
            "submit_opening" => {
                if let Some(review) = self.review.take() {
                    let recorded = !review.facts["existing_journal"].is_null();
                    match self
                        .pages
                        .commit(r.scope, r.credential, "submit_opening", CONTRACT, review)
                        .await?
                    {
                        Ok(done) => {
                            let number = done["results"][0]["result"]["result"]["record"]
                                ["journal_number"]
                                .as_i64();
                            let journal = number.map_or("The opening journal".to_owned(), |n| {
                                format!("Journal {n}")
                            });
                            self.flash.success(if recorded {
                                format!("{journal} already holds these balances; nothing new was submitted.")
                            } else {
                                format!("{journal} is submitted. It waits for another person to approve it.")
                            });
                            self.correct = None;
                            self.draft = Draft::default();
                        }
                        Err(Failure::Refused(refusal)) => {
                            if let Some((units, _, accounts)) =
                                self.units_and_accounts(r.scope).await?
                            {
                                self.refused(refusal, &accounts, units);
                            }
                        }
                        Err(other) => self.flash.error(other.text("The opening balances")),
                    }
                }
            }
            "cancel_opening" => self.review = None,
            "grid" => {
                let posted = params::string_values(value);
                for columns in [&mut self.grid, &mut self.steps, &mut self.reviewed] {
                    if let ColumnsOutcome::Sort(_) = columns.handle(&posted) {}
                }
            }
            "clear-flash" => self.flash.dismiss(value),
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Opening {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    let correct = values.get("correct").and_then(|v| v.parse().ok());
                    if correct != self.correct {
                        self.correct = correct;
                        self.filled_from = None;
                    }
                }
                LiveInput::Event(value) => self.event(&r, value).await?,
            }
            self.view(r.scope).await
        })
    }
}

/// A contract's `YYYY-MM-DD` date in the one date format pages show.
fn date(display: &DisplayContext, text: &str) -> String {
    text.parse::<jiff::civil::Date>()
        .map_or_else(|_| text.to_owned(), |d| display.render_date(d))
}
