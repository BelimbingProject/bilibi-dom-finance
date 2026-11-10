//! Fiscal years and their months: open a year, then close, reopen and lock its
//! months, and close or reopen the year. A period is a whole calendar month,
//! so the page names it "January 2026", never "period 1". Each action calls
//! the same contract an agent calls; the ledger decides what is allowed and
//! says why not.
use super::{
    capability_for, dialog_actions, error_text, event_type, no_company, render, shell, unavailable,
    view, Company, Failure, Flash, Pages, Read, Refusal, Review,
};
use crate::MANAGE_PERIODS;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice,
    Combobox, ConfirmDialog, CreateButton, EmptyState, Field, Filter, FlexRow, FlexTable, Form,
    Header, Html, IconButton, IconKind, InputKind, ListColumns, ListSpec, ListState, Modal,
    PageVariant, SectionHeading, StatusKind, TableEmpty,
};
use jiff::{civil::Date, ToSpan};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        filters: BTreeMap::from([
            ("company".into(), Filter::string("")),
            (
                "status".into(),
                Filter::one_of(&["all", "open", "closed", "locked"], "all"),
            ),
        ]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("fiscal years list spec")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    ClosePeriod,
    LockPeriod,
    ReopenPeriod,
    CloseYear,
    LockYear,
    ReopenYear,
}

impl Kind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "close_period" => Self::ClosePeriod,
            "lock_period" => Self::LockPeriod,
            "reopen_period" => Self::ReopenPeriod,
            "close_year" => Self::CloseYear,
            "lock_year" => Self::LockYear,
            "reopen_year" => Self::ReopenYear,
            _ => return None,
        })
    }
    /// The page event and the contract it calls.
    fn event(self) -> &'static str {
        match self {
            Self::ClosePeriod => "close_period",
            Self::LockPeriod => "lock_period",
            Self::ReopenPeriod => "reopen_period",
            Self::CloseYear => "close_year",
            Self::LockYear => "lock_year",
            Self::ReopenYear => "reopen_year",
        }
    }
    fn contract(self) -> &'static str {
        match self {
            Self::ClosePeriod => "finance.ledger.period.close",
            Self::LockPeriod => "finance.ledger.period.lock",
            Self::ReopenPeriod => "finance.ledger.period.reopen",
            Self::CloseYear => "finance.ledger.fiscal_year.close",
            Self::LockYear => "finance.ledger.fiscal_year.lock",
            Self::ReopenYear => "finance.ledger.fiscal_year.reopen",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::ClosePeriod | Self::LockPeriod | Self::ReopenPeriod => "period_id",
            _ => "fiscal_year_id",
        }
    }
    fn needs_reason(self) -> bool {
        matches!(self, Self::ReopenPeriod | Self::ReopenYear)
    }
}

/// The one period each period action can reach now, in the ledger's own
/// order: close the earliest open one, reopen the latest closed one, lock
/// the earliest unlocked one.
#[derive(Clone, Copy)]
struct Targets {
    close: Option<i64>,
    reopen: Option<i64>,
    lock: Option<i64>,
    can_close: bool,
    can_reopen: bool,
    can_lock: bool,
    can_post: bool,
}

/// What the person chose, and the ledger's review of it once it has one.
struct Chosen {
    kind: Kind,
    company: i64,
    company_name: String,
    /// The person works in more than one company, so every sentence about
    /// this action names the company.
    several: bool,
    id: i64,
    /// "January 2026" for a month, "January to December 2026" for a year.
    label: String,
    /// The month alone ("January"), for a sentence that already has the year.
    month: String,
    review: Option<Review>,
    reason: String,
    refusal: Refusal,
    notice: String,
}

/// The company the "Open a fiscal year" dialog acts on, and the dates it
/// proposes. `calculated_end` is the ledger's twelve-month default for
/// `start`; an `end` that differs from it is the person's own choice.
#[derive(Default)]
struct Opening {
    company: i64,
    start: String,
    /// The day after the company's previous year ends; empty for its first year.
    follows: String,
    /// The last day of the company's previous year; empty for its first year.
    previous_end: String,
    end: String,
    calculated_end: String,
    review: Option<Review>,
    refusal: Refusal,
    notice: String,
}

/// Which companies the page lists. It filters this page only: it does not
/// change the session's current company and grants no reach.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// The current company until the person chooses another view.
    Auto,
    All,
    One(i64),
}

/// A line under the years of several companies.
#[derive(askama::Template)]
#[template(
    source = r#"<p id="ledger-years-note" class="mt-4 text-xs text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Note {
    text: String,
}

const FILTER_FORM: &str = "company_filter";
const ALL_COMPANIES: &str = "all";

/// What the person may do on this page, asked once per render.
#[derive(Clone, Copy)]
struct Allowed {
    open: bool,
    close: bool,
    reopen: bool,
    lock: bool,
    post: bool,
}

pub(super) struct Years {
    pages: Pages,
    list: ListState,
    flash: Flash,
    columns: Option<ListColumns>,
    pick: Pick,
    choice: Option<Chosen>,
    opening: Option<Opening>,
}

impl Years {
    /// `query` may name a company (`?company=ID`), as the company books page
    /// does; it only filters this page and grants nothing.
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            pages,
            flash: Flash::default(),
            columns: None,
            pick: match query.get("company").map(String::as_str) {
                Some("all") => Pick::All,
                Some(id) => id.parse().map_or(Pick::Auto, Pick::One),
                None => Pick::Auto,
            },
            choice: None,
            opening: None,
        })
    }

    /// The companies the page lists now: the picked one, else every company
    /// the person may work in. A picked company the person no longer reaches
    /// is dropped, never read.
    fn listed<'a>(&self, scope: &Scope, companies: &'a [Company]) -> Vec<&'a Company> {
        let current = Pages::company(scope);
        match self.pick {
            Pick::All => companies.iter().collect(),
            Pick::One(id) => companies.iter().filter(|c| c.id == id).collect(),
            Pick::Auto => {
                let own: Vec<&Company> =
                    companies.iter().filter(|c| Some(c.id) == current).collect();
                if own.is_empty() {
                    companies.iter().collect()
                } else {
                    own
                }
            }
        }
    }

    /// The company filter: every company the person may work in, the
    /// session's current one marked, and all of them together.
    fn filter(&self, scope: &Scope, companies: &[Company], listed: &[&Company]) -> Result<Html> {
        let current = Pages::company(scope);
        let mut options = vec![Choice::new("All companies", ALL_COMPANIES)];
        options.extend(companies.iter().map(|c| {
            let choice = Choice::new(&c.name, c.id.to_string());
            if Some(c.id) == current {
                choice.detail("current company")
            } else {
                choice
            }
        }));
        let value = match (self.pick, listed) {
            (Pick::All, _) => ALL_COMPANIES.to_owned(),
            (_, [one]) => one.id.to_string(),
            _ => ALL_COMPANIES.to_owned(),
        };
        // The shared filter toolbar has no combobox, so the filter is its own
        // `filters` form around one (Platform follow-up), laid out and sized
        // as the toolbar is: one row of compact controls.
        render(Form {
            id: "ledger-years-filter",
            event: "company_filter",
            change: true,
            class: "ui-compact-controls mb-3 flex flex-wrap items-start gap-x-3 gap-y-2",
            body: {
                let mut body = render(
                    Combobox::new("ledger-years-company", format!("{FILTER_FORM}[company]"))
                        .aria_label("Company")
                        .placeholder("All companies")
                        .wrapper_class("mb-0 w-full sm:w-96")
                        .value(value)
                        .options(options),
                )?;
                body.push(&render(
                    Field::in_form(FILTER_FORM, "status")
                        .id("ledger-years-status")
                        .label("Status")
                        .label_class("sr-only")
                        .wrapper_class("mb-0 w-full sm:w-auto sm:min-w-36")
                        .kind(InputKind::Select)
                        .value(&self.list.filters["status"])
                        .options(vec![
                            Choice::new("All states", "all"),
                            Choice::new("Open", "open"),
                            Choice::new("Closed", "closed"),
                            Choice::new("Locked", "locked"),
                        ]),
                )?);
                body
            },
            actions: Html::EMPTY,
        })
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let companies = self.pages.companies(scope).await?;
        let listed = self.listed(scope, &companies);
        let allowed = Allowed {
            open: self.pages.allowed(scope, MANAGE_PERIODS).await?,
            close: self
                .pages
                .allowed(scope, capability_for("close_period"))
                .await?,
            reopen: self
                .pages
                .allowed(scope, capability_for("reopen_period"))
                .await?,
            lock: self
                .pages
                .allowed(scope, capability_for("lock_period"))
                .await?,
            post: self.pages.allowed(scope, crate::POST_ENTRIES).await?,
        };
        let mut body = Html::EMPTY;
        let mut actions = Html::EMPTY;
        let mut shown = 0;
        let subtitle = match listed.as_slice() {
            [one] => one.name.clone(),
            [] => String::new(),
            _ => "All companies".to_owned(),
        };
        if companies.is_empty() {
            body.push(&no_company("ledger-years-no-company")?);
        } else {
            body.push(&self.filter(scope, &companies, &listed)?);
            if allowed.open && !listed.is_empty() {
                actions.push(&render(CreateButton {
                    noun: "Fiscal year",
                    id: "ledger-year-new",
                    class: "text-xs",
                    event: "open_dialog",
                    button_type: "button",
                    ..CreateButton::default()
                })?);
            }
            if listed.is_empty() {
                body.push(&render(
                    EmptyState::missing(
                        "That company is not available",
                        "Choose another company in the filter.",
                    )
                    .id("ledger-years-no-listed"),
                )?);
            }
            let many = listed.len() > 1;
            let mut unopened = 0;
            for company in &listed {
                let (html, rows, open) =
                    self.company_section(scope, company, allowed, many).await?;
                if open {
                    body.push(&html);
                    shown += rows;
                } else {
                    unopened += 1;
                }
            }
            if unopened > 0 {
                body.push(&render(Note {
                    text: format!(
                        "{unopened} {} no open book, so no fiscal years. See Company books.",
                        if unopened == 1 {
                            "company has"
                        } else {
                            "companies have"
                        }
                    ),
                })?);
            }
        }
        let mut page = render(Header {
            title: Html::text("Fiscal years"),
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&body);
        if let Some(opening) = &self.opening {
            if allowed.open {
                let name = companies
                    .iter()
                    .find(|c| c.id == opening.company)
                    .map_or("Company unavailable", |c| c.name.as_str());
                page.push(&render(Modal {
                    id: "ledger-year-modal",
                    title: &format!("Open a fiscal year for {name}"),
                    cancel_event: "close_dialog",
                    description:
                        "Years run in whole calendar months and follow one another without gaps.",
                    body: self.open_form(scope, &companies, opening)?,
                    ..Modal::default()
                })?);
                if let Some(review) = &opening.review {
                    page.push(&render(ConfirmDialog {
                        id: "ledger-year-confirm",
                        consequence: &format!("Open fiscal year for {name}?"),
                        detail: &review_detail(review),
                        confirm: "Open fiscal year",
                        working: "Opening…",
                        confirm_event: "confirm_open_year",
                        cancel_event: "cancel_open_review",
                    })?);
                }
            }
        }
        if let Some(choice) = &self.choice {
            page.push(&self.dialog(choice)?);
        }
        Ok(LiveBody {
            html: shell("ledger-years", PageVariant::List, &self.flash, page)?,
            rows: shown,
            query: self.list.to_params(),
            ..LiveBody::default()
        })
    }

    /// One company's years: its book must be open, and a refusal of the read
    /// is said as such, never shown as "no fiscal year".
    async fn company_section(
        &mut self,
        scope: &Scope,
        company: &Company,
        allowed: Allowed,
        heading: bool,
    ) -> Result<(Html, usize, bool)> {
        let mut out = Html::EMPTY;
        if heading {
            out.push(&render(SectionHeading {
                id: &format!("ledger-years-company-{}", company.id),
                title: &company.name,
                class: "mt-6",
                ..SectionHeading::default()
            })?);
        }
        let id = |what: &str| format!("ledger-years-{what}-{}", company.id);
        match self.pages.book_in(scope, company.id).await? {
            Err(refused) => {
                out.push(&unavailable(&id("refused"), "fiscal years", &refused)?);
                return Ok((out, 0, true));
            }
            Ok(state) if state.book.is_none() => {
                out.push(&render(
                    EmptyState::missing(
                        "The book is not open yet",
                        "Open the company's book first. Then open its first fiscal year.",
                    )
                    .id(id("no-book")),
                )?);
                // Among many companies a book that is not open is one line at
                // the end, not a section of its own.
                return Ok((out, 0, !heading));
            }
            Ok(_) => {}
        }
        let value = match self
            .pages
            .read_in(
                scope,
                company.id,
                "finance.ledger.fiscal_year.list",
                json!({"page_size": 100}),
            )
            .await?
        {
            Read::Found(value) => value,
            other => {
                out.push(&unavailable(&id("refused"), "fiscal years", &other)?);
                return Ok((out, 0, true));
            }
        };
        let years: Vec<view::Year> = view::parse(value["records"].clone()).unwrap_or_default();
        if years.is_empty() {
            out.push(&render(
                EmptyState::missing(
                    "No fiscal year yet",
                    "A company posts only into a period of an open fiscal year. Open the first one.",
                )
                .id(id("empty")),
            )?);
        }
        // The ledger reopens the latest closed period and locks the earliest
        // unlocked one; offer only those.
        let all: Vec<&view::Period> = years.iter().flat_map(|y| y.periods.iter()).collect();
        let reopen = all
            .iter()
            .rposition(|p| p.status == "closed")
            .filter(|i| all.get(i + 1).is_none_or(|p| p.status == "open"))
            .map(|i| all[i].id);
        let lock = all
            .iter()
            .position(|p| p.status != "locked")
            .filter(|i| all[*i].status == "closed")
            .map(|i| all[i].id);
        let close = all.iter().find(|p| p.status == "open").map(|p| p.id);
        let targets = Targets {
            close,
            reopen,
            lock,
            can_close: allowed.close,
            can_reopen: allowed.reopen,
            can_lock: allowed.lock,
            can_post: allowed.post,
        };
        let mut shown = 0;
        for year in years.iter().rev() {
            let state = if year.periods.iter().all(|p| p.status == "locked") {
                "locked"
            } else {
                year.status.as_str()
            };
            if self.list.filters["status"] == "all" || self.list.filters["status"] == state {
                out.push(&self.year(year, company, targets)?);
                shown += 1;
            }
        }
        if shown == 0 && !years.is_empty() {
            out.push(&render(EmptyState::missing(
                "No matching fiscal years",
                "Choose another status in the filter.",
            ))?);
        }
        Ok((out, shown, true))
    }

    fn year(&mut self, year: &view::Year, company: &Company, targets: Targets) -> Result<Html> {
        let company_name = company.name.as_str();
        let Targets {
            close: close_id,
            reopen: reopen_id,
            lock: lock_id,
            can_close: manage,
            can_reopen: reopen,
            can_lock: lock,
            can_post: post,
        } = targets;
        let closed = year.status == "closed";
        let all_locked = year.periods.iter().all(|p| p.status == "locked");
        if self.columns.is_none() {
            self.columns = Some(
                ListColumns::mount(
                    "ledger-year-periods",
                    vec![
                        BuiltinColumn::new("month", "Month"),
                        BuiltinColumn::new("status", "Status"),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        let columns = self.columns.as_mut().ok_or(Error::Internal)?;
        columns.load(&year.periods, |p| p.id.to_string());
        let mut rows = Vec::new();
        let last = year.periods.last().map(|p| p.id);
        for period in &year.periods {
            let label = crate::dates::months_text(&period.start_date, &period.end_date);
            // The period's next step is local to it, so it is a plain icon
            // button with a tooltip, raising the page's own confirmation.
            let mut actions = Html::EMPTY;
            let mut button = |kind: &str, icon: &str, text: &str, danger: bool| -> Result<()> {
                let value = format!("{kind}:{}:{}", company.id, period.id);
                let label = format!("{text} {label}");
                actions.push(&render(IconButton {
                    kind: if danger {
                        IconKind::Danger
                    } else {
                        IconKind::Neutral
                    },
                    id: &format!("ledger-period-{kind}-{}", period.id),
                    button_type: "button",
                    event: "choose",
                    name: "action",
                    value: &value,
                    ..IconButton::new(icon, &label)
                })?);
                Ok(())
            };
            match period.status.as_str() {
                // The last period of a year closes with the year.
                "open" if manage && close_id == Some(period.id) && Some(period.id) != last => {
                    button("close_period", "check-circle", "Close", false)?
                }
                "closed" => {
                    if reopen && !closed && reopen_id == Some(period.id) {
                        button("reopen_period", "arrow-uturn-left", "Reopen", false)?;
                    }
                    if lock && lock_id == Some(period.id) {
                        button("lock_period", "lock-closed", "Lock for good", true)?;
                    }
                }
                _ => {}
            }
            let mut row = FlexRow::new(period.id.to_string())
                .cell("month", Cell::text(&label))
                .slot("status", period_badge(&period.status)?);
            row.actions = actions;
            row.dom_id = Some(format!("ledger-period-{}", period.id));
            rows.push(row);
        }
        let table_id = format!("ledger-year-{}-periods", year.id);
        let mut table = FlexTable::new(&table_id, columns.flex_columns(), rows);
        table.caption = "Months";
        table.framed = false;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.actions = true;
        table.empty = Some(TableEmpty::Content(Html::text("This year has no months.")));

        let mut year_actions = Html::EMPTY;
        let mine = |id: Option<i64>| id.is_some_and(|id| year.periods.iter().any(|p| p.id == id));
        // The year's one main button is its next step: close the earliest
        // open month by name, and the year itself only once every month
        // before its last is closed (the last month closes with the year).
        let next = year
            .periods
            .iter()
            .find(|p| close_id == Some(p.id))
            .filter(|_| !closed && manage);
        match next {
            Some(period) if Some(period.id) != last => {
                let month = period
                    .start_date
                    .parse::<Date>()
                    .map_or("month", crate::dates::month_name);
                year_actions.push(&render(Button {
                    id: &format!("ledger-year-close-month-{}", year.id),
                    button_type: "button",
                    event: "choose",
                    name: "action",
                    value: &format!("close_period:{}:{}", company.id, period.id),
                    variant: ButtonVariant::Primary,
                    body: Html::text(&format!("Close {month}")),
                    ..Button::default()
                })?);
            }
            Some(_) if post => {
                year_actions.push(&render(Button {
                    id: &format!("ledger-year-close-{}", year.id),
                    button_type: "button",
                    event: "choose",
                    name: "action",
                    value: &format!("close_year:{}:{}", company.id, year.id),
                    variant: ButtonVariant::Primary,
                    body: Html::text("Close year"),
                    ..Button::default()
                })?);
            }
            _ => {}
        }
        if closed && reopen && post && !all_locked && reopen_id == year.periods.last().map(|p| p.id)
        {
            year_actions.push(&render(Button {
                id: &format!("ledger-year-reopen-{}", year.id),
                button_type: "button",
                event: "choose",
                name: "action",
                value: &format!("reopen_year:{}:{}", company.id, year.id),
                body: Html::text("Reopen year"),
                ..Button::default()
            })?);
        }
        if closed && lock && !all_locked && mine(lock_id) {
            year_actions.push(&render(Button {
                id: &format!("ledger-year-lock-{}", year.id),
                button_type: "button",
                event: "choose",
                name: "action",
                value: &format!("lock_year:{}:{}", company.id, year.id),
                variant: ButtonVariant::Danger,
                body: Html::text("Lock year"),
                ..Button::default()
            })?);
        }
        let heading_id = format!("ledger-year-{}-heading", year.id);
        let mut heading = render(SectionHeading {
            id: &heading_id,
            title: &format!(
                "{} · {}",
                company_name,
                crate::dates::months_text(&year.start_date, &year.end_date)
            ),
            title_actions: render(Badge::status(
                if closed {
                    BadgeKind::Warning
                } else {
                    BadgeKind::Success
                },
                if closed { "Closed" } else { "Open" },
            ))?,
            actions: year_actions,
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        heading.push(&render(table)?);
        render(Card {
            id: &format!("ledger-year-{}", year.id),
            inner_class: "p-0",
            class: "mt-4",
            role: "region",
            labelledby: &heading_id,
            body: heading,
            ..Card::default()
        })
    }

    fn open_form(&self, scope: &Scope, companies: &[Company], opening: &Opening) -> Result<Html> {
        let errors = |name: &str| -> Vec<String> {
            opening
                .refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect()
        };
        let mut body = Html::EMPTY;
        let mut messages = opening
            .refusal
            .alert(&["start_date", "end_date", "company_id"]);
        messages.extend(errors("company_id"));
        if !opening.notice.is_empty() {
            messages.push(opening.notice.clone());
        }
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: "ledger-year-notice",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&messages.join(" ")),
            })?);
        }
        let current = Pages::company(scope);
        let options: Vec<Choice> = companies
            .iter()
            .map(|c| {
                let choice = Choice::new(&c.name, c.id.to_string());
                if Some(c.id) == current {
                    choice.detail("current company")
                } else {
                    choice
                }
            })
            .collect();
        // A required combobox draws no mark of its own (Platform follow-up),
        // so the label carries it and the plain name goes to assistive text.
        body.push(&render(
            Combobox::new("ledger-year-company", "year[company]")
                .label("Company *")
                .aria_label("Company")
                .value(opening.company.to_string())
                .options(options),
        )?);
        body.push(&render(
            Field::in_form("year", "start_date")
                .id("ledger-year-start")
                .label("First day")
                .kind(InputKind::Date)
                .value(&opening.start)
                .hint(start_hint(opening))
                .errors(errors("start_date")),
        )?);
        body.push(&render(
            Field::in_form("year", "end_date")
                .id("ledger-year-end")
                .label("Last day")
                .kind(InputKind::Date)
                .value(&opening.end)
                .hint(end_hint(opening))
                .errors(errors("end_date")),
        )?);
        if opening.overridden() {
            body.push(&render(Button {
                id: "ledger-year-use-calculated",
                button_type: "button",
                event: "use_calculated_end",
                body: Html::text("Use calculated last day"),
                ..Button::default()
            })?);
        }
        let mut actions = render(Button {
            id: "ledger-year-cancel",
            button_type: "button",
            event: "close_dialog",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "ledger-year-open",
            button_type: "button",
            event: "open_year",
            variant: ButtonVariant::Primary,
            working: "Opening…",
            body: Html::text("Review fiscal year"),
            ..Button::default()
        })?);
        render(Form {
            id: "ledger-year-form",
            event: "validate",
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }

    fn dialog(&self, choice: &Chosen) -> Result<Html> {
        let company_name = choice.company_name.as_str();
        if choice.kind.needs_reason() {
            let errors: Vec<String> = choice
                .refusal
                .field("reason")
                .iter()
                .map(|m| error_text("reason", m))
                .collect();
            let mut body = Html::EMPTY;
            let mut messages = choice.refusal.alert(&["reason"]);
            if !choice.notice.is_empty() {
                messages.push(choice.notice.clone());
            }
            if !messages.is_empty() {
                body.push(&render(Alert {
                    id: "ledger-choice-notice",
                    kind: StatusKind::Error,
                    class: "mb-3",
                    body: Html::text(&messages.join(" ")),
                })?);
            }
            body.push(&render(
                Field::in_form("reopen", "reason")
                    .id("ledger-reopen-reason")
                    .label("Reason")
                    .kind(InputKind::Textarea)
                    .value(&choice.reason)
                    .required()
                    .hint("Up to 500 characters. A reviewer will read it.")
                    .errors(errors)
                    .attr("data-debounce", "300"),
            )?);
            let mut actions = render(Button {
                id: "ledger-reopen-cancel",
                button_type: "button",
                event: "cancel_choice",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                id: "ledger-reopen-submit",
                button_type: "button",
                event: "do_reopen",
                variant: ButtonVariant::Primary,
                working: "Reopening…",
                body: Html::text("Reopen"),
                ..Button::default()
            })?);
            return render(Modal {
                id: "ledger-choice-modal",
                title: &if choice.kind == Kind::ReopenYear {
                    format!("Reopen the fiscal year {}", choice.label)
                } else {
                    format!("Reopen {}", choice.label)
                },
                cancel_event: "cancel_choice",
                description: &if choice.kind == Kind::ReopenYear {
                    format!("{company_name}. Only the last month of the year opens again.")
                } else {
                    format!(
                        "{company_name}. Entries dated in {} can be added and changed again.",
                        choice.month
                    )
                },
                body: render(Form {
                    id: "ledger-reopen-form",
                    event: "validate",
                    change: true,
                    class: "mt-4",
                    body,
                    actions: dialog_actions(actions)?,
                })?,
                ..Modal::default()
            });
        }
        let (consequence, detail, confirm, working) = match choice.kind {
            Kind::ClosePeriod => (
                format!("Close {}?", choice.label),
                format!(
                    "No one can add or change entries dated in {} until someone reopens it.",
                    choice.month
                ),
                format!("Close {}", choice.month),
                "Closing…",
            ),
            Kind::LockPeriod => (
                format!("Lock {} for good?", choice.label),
                format!(
                    "No one can ever reopen {} after this. Months lock in date order.",
                    choice.month
                ),
                format!("Lock {}", choice.month),
                "Locking…",
            ),
            Kind::CloseYear => (
                format!("Close the fiscal year {}?", choice.label),
                close_year_detail(choice.review.as_ref()),
                "Close year".to_owned(),
                "Closing…",
            ),
            _ => (
                format!("Lock the fiscal year {} for good?", choice.label),
                "Every closed month of the year is locked, and no one can ever reopen it."
                    .to_owned(),
                "Lock year".to_owned(),
                "Locking…",
            ),
        };
        // One company needs no name in the question; among several it does.
        let consequence = if choice.several {
            format!("{consequence} ({company_name})")
        } else {
            consequence
        };
        render(ConfirmDialog {
            id: "ledger-choice-confirm",
            consequence: &consequence,
            detail: &detail,
            confirm: &confirm,
            working,
            confirm_event: "do_confirm",
            cancel_event: "cancel_choice",
        })
    }

    /// The company a period or year button names, still among those the
    /// person may work in. The contract proves the reach again when it reads.
    async fn named(&self, scope: &Scope, company: i64) -> Result<Option<Company>> {
        Ok(self
            .pages
            .companies(scope)
            .await?
            .into_iter()
            .find(|c| c.id == company))
    }

    async fn choose(&mut self, r: &LiveRequest<'_, Scope>, action: &str) -> Result<()> {
        let mut parts = action.splitn(3, ':');
        let kind = parts.next().and_then(Kind::parse).ok_or(Error::Invalid)?;
        let company_id: i64 = parts
            .next()
            .and_then(|i| i.parse().ok())
            .ok_or(Error::Invalid)?;
        let id: i64 = parts
            .next()
            .and_then(|i| i.parse().ok())
            .ok_or(Error::Invalid)?;
        if !self
            .pages
            .allowed(r.scope, capability_for(kind.event()))
            .await?
        {
            self.flash.error(super::DENIED);
            return Ok(());
        }
        let companies = self.pages.companies(r.scope).await?;
        let Some(company) = companies.iter().find(|c| c.id == company_id).cloned() else {
            self.flash
                .error("That company is not available to you. Reload the page.");
            return Ok(());
        };
        // The name comes from the ledger's own dates, never from the button.
        let years = self.pages.years_in(r.scope, company.id).await?;
        let named = years.ok().and_then(|years| {
            if kind.key() == "period_id" {
                let period = years.iter().flat_map(|y| &y.periods).find(|p| p.id == id)?;
                let start = period.start_date.parse::<Date>().ok()?;
                Some((
                    crate::dates::months_text(&period.start_date, &period.end_date),
                    crate::dates::month_name(start).to_owned(),
                ))
            } else {
                let year = years.iter().find(|y| y.id == id)?;
                Some((
                    crate::dates::months_text(&year.start_date, &year.end_date),
                    String::new(),
                ))
            }
        });
        let Some((label, month)) = named else {
            self.flash.error(GONE);
            return Ok(());
        };
        let mut choice = Chosen {
            kind,
            company: company.id,
            company_name: company.name,
            several: companies.len() > 1,
            id,
            label,
            month,
            review: None,
            reason: String::new(),
            refusal: Refusal::default(),
            notice: String::new(),
        };
        if !kind.needs_reason() {
            match self
                .pages
                .review_in(
                    r.scope,
                    company.id,
                    r.credential,
                    "choose",
                    kind.contract(),
                    json!({ kind.key(): id }),
                )
                .await?
            {
                Ok(review) => choice.review = Some(review),
                Err(failure) => {
                    self.flash.error(failure.text(&choice.label));
                    return Ok(());
                }
            }
        }
        self.choice = Some(choice);
        Ok(())
    }

    async fn confirm(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(mut choice) = self.choice.take() else {
            return Ok(());
        };
        let Some(review) = choice.review.take() else {
            return Ok(());
        };
        // The contract proves the company's reach and the capability again;
        // a refusal is said with the company named.
        match self
            .pages
            .commit(
                r.scope,
                r.credential,
                "do_confirm",
                choice.kind.contract(),
                review,
            )
            .await?
        {
            Ok(_) => {
                let label = &choice.label;
                self.flash.success(choice.say(match choice.kind {
                    Kind::ClosePeriod => format!("{label} is closed."),
                    Kind::LockPeriod => format!("{label} is locked for good."),
                    Kind::CloseYear => format!("The fiscal year {label} is closed."),
                    _ => format!("The fiscal year {label} is locked for good."),
                }));
            }
            Err(failure) => self.flash.error(choice.say(failure.text(&choice.label))),
        }
        Ok(())
    }

    async fn reopen(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(choice) = &mut self.choice else {
            return Ok(());
        };
        let input = json!({ choice.kind.key(): choice.id, "reason": choice.reason.trim() });
        match self
            .pages
            .submit_in(
                r.scope,
                choice.company,
                r.credential,
                "do_reopen",
                choice.kind.contract(),
                input,
            )
            .await?
        {
            Ok(_) => {
                let label = &choice.label;
                let text = choice.say(if choice.kind == Kind::ReopenYear {
                    format!("The fiscal year {label} is reopened. Its last month is open again.")
                } else {
                    format!("{label} is open again.")
                });
                self.choice = None;
                self.flash.success(text);
            }
            Err(Failure::Refused(refusal)) => {
                choice.refusal = refusal;
                choice.notice.clear();
            }
            Err(failure) => choice.notice = failure.text("The reopening"),
        }
        Ok(())
    }

    /// The day after `company`'s latest fiscal year ends, with that year's
    /// last day; `None` when the company has no year yet or its years cannot
    /// be read (the person then types the dates).
    async fn follow_on(&self, scope: &Scope, company: i64) -> Result<Option<(Date, Date)>> {
        let Ok(years) = self.pages.years_in(scope, company).await? else {
            return Ok(None);
        };
        Ok(years
            .last()
            .and_then(|year| year.end_date.parse::<Date>().ok())
            .and_then(|end| end.tomorrow().ok().map(|start| (start, end))))
    }

    /// Point the dialog at `company`, proposing the dates that follow its
    /// previous year. A first day or last day the person already chose for
    /// the old proposal is kept as typed.
    async fn aim(&self, scope: &Scope, opening: &mut Opening, company: i64) -> Result<()> {
        let start_was_proposed = opening.start == opening.follows;
        let end_was_calculated = opening.end == opening.calculated_end;
        opening.company = company;
        let follow = self.follow_on(scope, company).await?;
        opening.follows = follow
            .map(|(start, _)| start.to_string())
            .unwrap_or_default();
        opening.previous_end = follow.map(|(_, end)| end.to_string()).unwrap_or_default();
        if start_was_proposed {
            opening.start = opening.follows.clone();
        }
        opening.calculated_end = default_end(&opening.start);
        if start_was_proposed && end_was_calculated {
            opening.end = opening.calculated_end.clone();
        }
        opening.review = None;
        opening.refusal = Refusal::default();
        opening.notice.clear();
        Ok(())
    }

    async fn open_year(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(mut opening) = self.opening.take() else {
            return Ok(());
        };
        let mut input = json!({});
        if !opening.start.trim().is_empty() {
            input["start_date"] = json!(opening.start.trim());
        }
        if !opening.end.trim().is_empty() {
            input["end_date"] = json!(opening.end.trim());
        }
        match self
            .pages
            .review_in(
                r.scope,
                opening.company,
                r.credential,
                "open_year",
                "finance.ledger.fiscal_year.open",
                input,
            )
            .await?
        {
            Ok(review) => opening.review = Some(review),
            Err(Failure::Refused(refusal)) => {
                opening.refusal = refusal;
                opening.notice.clear();
            }
            Err(failure) => opening.notice = failure.text("The fiscal year"),
        }
        self.opening = Some(opening);
        Ok(())
    }

    async fn confirm_open_year(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(opening) = &mut self.opening else {
            return Ok(());
        };
        let Some(review) = opening.review.take() else {
            return Ok(());
        };
        let company = opening.company;
        // The contract proves the company's reach and the capability again.
        match self
            .pages
            .commit(
                r.scope,
                r.credential,
                "confirm_open_year",
                "finance.ledger.fiscal_year.open",
                review,
            )
            .await?
        {
            Ok(_) => {
                self.opening = None;
                let name = self.named(r.scope, company).await?.map(|c| c.name);
                self.flash.success(match name {
                    Some(name) => format!("The fiscal year of {name} is open."),
                    None => "The fiscal year is open.".to_owned(),
                });
            }
            Err(Failure::Refused(refusal)) => {
                opening.refusal = refusal;
                opening.notice.clear();
            }
            Err(failure) => opening.notice = failure.text("The fiscal year"),
        }
        Ok(())
    }

    /// Start the dialog on the company the page lists, else the session's
    /// current company, else the first the person may work in.
    async fn open_dialog(&mut self, scope: &Scope) -> Result<()> {
        let companies = self.pages.companies(scope).await?;
        let company = match self.pick {
            Pick::One(id) if companies.iter().any(|c| c.id == id) => Some(id),
            _ => Pages::company(scope)
                .filter(|id| companies.iter().any(|c| c.id == *id))
                .or_else(|| companies.first().map(|c| c.id)),
        };
        let Some(company) = company else {
            return Ok(());
        };
        let mut opening = Opening::default();
        self.aim(scope, &mut opening, company).await?;
        self.opening = Some(opening);
        Ok(())
    }

    /// The form changed: a company, a first day or a last day.
    async fn validate(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<()> {
        if let Some(mut opening) = self.opening.take() {
            let fields = params::form_values(value, "year");
            let start = fields.get("start_date").cloned().unwrap_or_default();
            let end = fields.get("end_date").cloned().unwrap_or_default();
            let company = fields
                .get("company")
                .and_then(|c| c.parse::<i64>().ok())
                .filter(|c| *c != opening.company);
            let follows_default = end == opening.calculated_end;
            opening.start = start;
            opening.end = end;
            match company {
                // A new company has its own previous year: `aim` proposes its
                // dates for whatever was still the old proposal and keeps
                // what the person typed.
                Some(company) if self.named(scope, company).await?.is_some() => {
                    self.aim(scope, &mut opening, company).await?;
                }
                _ => {
                    opening.calculated_end = default_end(&opening.start);
                    if follows_default {
                        opening.end = opening.calculated_end.clone();
                    }
                }
            }
            opening.review = None;
            opening.refusal = Refusal::default();
            opening.notice.clear();
            self.opening = Some(opening);
        } else if let Some(choice) = &mut self.choice {
            choice.reason = params::form_values(value, "reopen")
                .get("reason")
                .cloned()
                .unwrap_or_default();
            choice.refusal = Refusal::default();
            choice.notice.clear();
        }
        Ok(())
    }
}

/// The month or year a button named is no longer there to act on.
const GONE: &str = "Nothing was changed. Reload the page and try again.";

impl Chosen {
    /// A message about this action: among several companies it starts with
    /// the company's name, and with one company it needs none.
    fn say(&self, text: String) -> String {
        if self.several {
            format!("{}: {text}", self.company_name)
        } else {
            text
        }
    }
}

impl Opening {
    /// A last day the person chose, not the calculated twelve months.
    fn overridden(&self) -> bool {
        !self.end.trim().is_empty()
            && !self.calculated_end.is_empty()
            && self.end != self.calculated_end
    }
}

fn start_hint(opening: &Opening) -> String {
    if opening.previous_end.is_empty() {
        "Required for the company's first fiscal year. Start on the first day of a month."
            .to_owned()
    } else if opening.start == opening.follows {
        format!(
            "Follows the previous year, which ends {}.",
            crate::dates::display_text(&opening.previous_end)
        )
    } else {
        format!(
            "The previous year ends {}; the next year must start on {}.",
            crate::dates::display_text(&opening.previous_end),
            crate::dates::display_text(&opening.follows)
        )
    }
}

fn end_hint(opening: &Opening) -> String {
    if opening.calculated_end.is_empty() {
        "Set from the first day: twelve months, ending on a month-end.".to_owned()
    } else if opening.end.trim().is_empty() {
        format!(
            "Left empty, the year ends twelve months after the first day: {}.",
            crate::dates::display_text(&opening.calculated_end)
        )
    } else if opening.end == opening.calculated_end {
        "Calculated: twelve months from the first day. Choose another month-end for a short or extended year."
            .to_owned()
    } else {
        let months = months_between(&opening.start, &opening.end);
        match months {
            Some(months) => format!(
                "Chosen by you: {months} {}, a {} year. The calculated last day is {}. The ledger accepts 1 to 24 months; confirm your company may use this length.",
                if months == 1 { "month" } else { "months" },
                if months < 12 { "short" } else { "extended" },
                crate::dates::display_text(&opening.calculated_end)
            ),
            None => format!(
                "Chosen by you. The calculated last day is {}.",
                crate::dates::display_text(&opening.calculated_end)
            ),
        }
    }
}

/// Whole calendar months from a first day to a last day, when the dates run
/// from the first of a month to the end of a month.
fn months_between(start: &str, end: &str) -> Option<i32> {
    let start: Date = start.parse().ok()?;
    let end: Date = end.parse().ok()?;
    if start.day() != 1 || end != end.last_of_month() || end < start {
        return None;
    }
    Some(
        (i32::from(end.year()) - i32::from(start.year())) * 12 + i32::from(end.month())
            - i32::from(start.month())
            + 1,
    )
}

fn default_end(start: &str) -> String {
    start
        .parse::<Date>()
        .ok()
        .and_then(|date| date.checked_add(12.months()).ok())
        .and_then(|date| date.yesterday().ok())
        .map(|date| date.to_string())
        .unwrap_or_default()
}

/// What the confirmation shows of the ledger's review: the dates it will
/// open, and the number of monthly periods that makes.
fn review_detail(review: &Review) -> String {
    let start = review.facts["start_date"].as_str().unwrap_or("");
    let end = review.facts["end_date"].as_str().unwrap_or("");
    let months = months_between(start, end);
    match months {
        Some(months) => format!(
            "{}: {months} {}, all open.",
            crate::dates::months_text(start, end),
            if months == 1 { "month" } else { "months" }
        ),
        None => format!(
            "{} to {}",
            crate::dates::display_text(start),
            crate::dates::display_text(end)
        ),
    }
}

fn close_year_detail(review: Option<&Review>) -> String {
    let base = "Any month still open closes with it, and no one can add or change entries dated in the year until someone reopens it.";
    let entry = review.map(|r| &r.facts["entry"]).filter(|e| !e.is_null());
    match entry.and_then(|e| e["lines"].as_array()).map(Vec::len) {
        Some(lines) => format!(
            "{base} Closing also posts one closing entry of {lines} lines that moves income and expenses into retained earnings."
        ),
        None => format!("{base} Nothing is posted: the year has no income or expense to move."),
    }
}

fn period_badge(status: &str) -> Result<Html> {
    render(match status {
        "open" => Badge::status(BadgeKind::Success, "Open"),
        "closed" => Badge::status(BadgeKind::Warning, "Closed"),
        _ => Badge::status(BadgeKind::Neutral, "Locked"),
    })
}

impl LiveState<Scope, Error> for Years {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Params(query) = r.input {
                self.list = ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
                self.pick = match self.list.filters["company"].as_str() {
                    "" => Pick::Auto,
                    "all" => Pick::All,
                    id => id.parse().map_or(Pick::Auto, Pick::One),
                };
            }
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "open_dialog" => {
                        if self.pages.allowed(r.scope, MANAGE_PERIODS).await? {
                            self.open_dialog(r.scope).await?;
                        }
                    }
                    "close_dialog" => self.opening = None,
                    "company_filter" => {
                        let fields = params::form_values(value, FILTER_FORM);
                        self.list.apply_filters(&fields);
                        let picked = fields.get("company").cloned().unwrap_or_default();
                        self.pick = match picked.parse::<i64>() {
                            Ok(id) => Pick::One(id),
                            Err(_) => Pick::All,
                        };
                    }
                    "validate" => self.validate(r.scope, value).await?,
                    "use_calculated_end" => {
                        if let Some(opening) = &mut self.opening {
                            opening.end = opening.calculated_end.clone();
                            opening.review = None;
                            opening.refusal = Refusal::default();
                        }
                    }
                    "open_year" => self.open_year(&r).await?,
                    "confirm_open_year" => self.confirm_open_year(&r).await?,
                    "cancel_open_review" => {
                        if let Some(opening) = &mut self.opening {
                            opening.review = None;
                        }
                    }
                    "choose" => {
                        let action = value["action"].as_str().ok_or(Error::Invalid)?.to_owned();
                        self.choose(&r, &action).await?
                    }
                    "cancel_choice" => self.choice = None,
                    "do_confirm" => self.confirm(&r).await?,
                    "do_reopen" => self.reopen(&r).await?,
                    "grid" => {
                        if let Some(columns) = &mut self.columns {
                            columns.handle(&params::string_values(value));
                        }
                    }
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}
