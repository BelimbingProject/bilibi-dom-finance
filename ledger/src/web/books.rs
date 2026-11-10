//! Company books: every company the person may work in, with the state of its
//! book and fiscal calendar, side by side. It is an overview of Finance's own
//! facts, not a consolidation: each row stands alone, in its own currency. It
//! reads each company through the same contracts an agent calls (`book.get`,
//! `fiscal_year.list`), which prove the reach again, and it never switches the
//! session's current company.
use super::{
    event_type, filter_combobox, no_company, render, shell, view, Company, Flash, Pages, Read,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, AddToItem, AddToMenu, Badge, BadgeKind, BuiltinColumn, Card, Cell, Choice, Filter,
    FilterToolbar, FlexRow, FlexTable, Header, Html, IconButton, ListColumns, ListSpec, ListState,
    PageVariant, RecordLink, TableEmpty, ToolbarControl,
};
use jiff::civil::Date;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, LazyLock},
};
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        filters: BTreeMap::from([
            (
                "book".into(),
                Filter::one_of(
                    &["all", "open", "not_open", "no_access", "unavailable"],
                    "all",
                ),
            ),
            ("currency".into(), Filter::string("")),
            (
                "year".into(),
                Filter::one_of(
                    &[
                        "all",
                        "open",
                        "closed",
                        "locked",
                        "none",
                        "no_access",
                        "unavailable",
                    ],
                    "all",
                ),
            ),
        ]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("company books list spec")
});

pub(super) struct Books {
    pages: Pages,
    flash: Flash,
    columns: Option<ListColumns>,
    list: ListState,
    framed: bool,
}

/// What a company's book answered.
enum Book {
    Open(view::BookRow),
    NotOpen,
    /// The person may work in the company but the ledger refused the read.
    NoAccess,
    Unavailable,
}

/// The state of a company's fiscal calendar, as the books page words it.
struct Calendar {
    latest: Option<(String, String)>,
    earliest_open: Option<(String, String)>,
    past_due: bool,
}

impl Calendar {
    fn of(years: &[view::Year], today: Date) -> Self {
        let latest = years.last().map(|y| {
            (
                crate::dates::months_text(&y.start_date, &y.end_date),
                if y.periods.iter().all(|p| p.status == "locked") {
                    "locked".into()
                } else {
                    y.status.clone()
                },
            )
        });
        let earliest_open = years
            .iter()
            .flat_map(|y| y.periods.iter())
            .find(|p| p.status == "open")
            .map(|p| (p.start_date.clone(), p.end_date.clone()));
        let past_due = earliest_open
            .as_ref()
            .and_then(|(_, end)| end.parse::<Date>().ok())
            .is_some_and(|end| end < today);
        Self {
            latest,
            earliest_open,
            past_due,
        }
    }
}

/// The company's name in a row, marked when it is the session's current one.
#[derive(askama::Template)]
#[template(
    source = r#"<span class="font-medium text-ink-strong">{{ name }}</span>{% if current %} <span class="text-ink-muted">current company</span>{% endif %}"#,
    ext = "html"
)]
struct CompanyCell<'a> {
    name: &'a str,
    current: bool,
}

/// Just the id of a row, for the column manager.
struct Line(String);

impl Books {
    fn filter(&self, key: &str) -> &str {
        self.list.filters.get(key).map_or("", String::as_str)
    }
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            flash: Flash::default(),
            columns: None,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            framed,
        })
    }

    /// One company's row. A company the person works in whose book the ledger
    /// will not read says so, apart from one whose book is simply not open.
    async fn row(
        &self,
        scope: &Scope,
        company: &Company,
        current: Option<i64>,
        today: Date,
        currencies: &mut BTreeSet<String>,
    ) -> Result<Option<FlexRow>> {
        let can_open = self.pages.allowed(scope, crate::MANAGE_ACCOUNTS).await?;
        let book = match self.pages.book_in(scope, company.id).await? {
            Ok(state) => match state.book {
                Some(book) => Book::Open(book),
                None => Book::NotOpen,
            },
            Err(Read::Denied) => Book::NoAccess,
            Err(_) => Book::Unavailable,
        };
        let state = match &book {
            Book::Open(_) => "open",
            Book::NotOpen => "not_open",
            Book::NoAccess => "no_access",
            Book::Unavailable => "unavailable",
        };
        let currency = match &book {
            Book::Open(b) => b.currency_code.as_str(),
            _ => "",
        };
        if !currency.is_empty() {
            currencies.insert(currency.to_owned());
        }
        if (!self.list.search.is_empty()
            && !company
                .name
                .to_lowercase()
                .contains(&self.list.search.to_lowercase()))
            || (self.filter("book") != "all" && self.filter("book") != state)
            || (!self.filter("currency").is_empty()
                && !currency.eq_ignore_ascii_case(self.filter("currency")))
        {
            return Ok(None);
        }
        let mut year_state = "none";
        let mut row = FlexRow::new(company.id.to_string()).slot(
            "company",
            render(RecordLink {
                workspace: self.framed,
                kind: "finance/ledger.book",
                record_id: &company.id.to_string(),
                href: &format!("/finance/ledger/books/{}", company.id),
                class: "text-link hover:underline",
                body: render(CompanyCell {
                    name: &company.name,
                    current: Some(company.id) == current,
                })?,
            })?,
        );
        row.dom_id = Some(format!("ledger-books-{}", company.id));
        let dash = || Cell::text("—");
        match &book {
            Book::Open(book) => {
                row = row
                    .slot("book", render(Badge::status(BadgeKind::Success, "Open"))?)
                    .cell(
                        "currency",
                        Cell::text(format!(
                            "{} · {} decimal places",
                            book.currency_code, book.minor_units
                        )),
                    );
                match self.pages.years_in(scope, company.id).await? {
                    Ok(years) if years.is_empty() => {
                        row = row
                            .slot(
                                "year",
                                render(Badge::status(BadgeKind::Warning, "No fiscal year yet"))?,
                            )
                            .cell("open", dash());
                    }
                    Ok(years) => {
                        let calendar = Calendar::of(&years, today);
                        year_state = if calendar
                            .latest
                            .as_ref()
                            .is_some_and(|(_, state)| state == "locked")
                        {
                            "locked"
                        } else if calendar
                            .latest
                            .as_ref()
                            .is_some_and(|(_, state)| state == "closed")
                        {
                            "closed"
                        } else {
                            "open"
                        };
                        row = match calendar.latest {
                            Some((months, state)) => {
                                row.cell("year", Cell::text(format!("{months} ({state})")))
                            }
                            None => row.cell("year", dash()),
                        };
                        row = match calendar.earliest_open {
                            Some((start, end)) => {
                                let mut cell = Html::text(&crate::dates::months_text(&start, &end));
                                if calendar.past_due {
                                    cell.push(&Html::text(" "));
                                    cell.push(&render(Badge::status(
                                        BadgeKind::Warning,
                                        "Not closed",
                                    ))?);
                                }
                                row.slot("open", cell)
                            }
                            None => row.cell("open", Cell::text("None open")),
                        };
                    }
                    Err(Read::Denied) => {
                        year_state = "no_access";
                        row = row
                            .cell("year", Cell::text("No access to fiscal years"))
                            .cell("open", dash());
                    }
                    Err(_) => {
                        year_state = "unavailable";
                        row = row
                            .cell("year", Cell::text("Could not be read"))
                            .cell("open", dash());
                    }
                }
            }
            Book::NotOpen => {
                row = row
                    .slot(
                        "book",
                        render(Badge::status(BadgeKind::Warning, "Not open"))?,
                    )
                    .cell("currency", dash())
                    .cell("year", dash())
                    .cell("open", dash());
            }
            Book::NoAccess => {
                year_state = "no_access";
                row = row
                    .slot(
                        "book",
                        render(Badge::status(BadgeKind::Neutral, "No access"))?,
                    )
                    .cell("currency", dash())
                    .cell("year", dash())
                    .cell("open", dash());
            }
            Book::Unavailable => {
                year_state = "unavailable";
                row = row
                    .slot(
                        "book",
                        render(Badge::status(BadgeKind::Neutral, "Could not be read"))?,
                    )
                    .cell("currency", dash())
                    .cell("year", dash())
                    .cell("open", dash());
            }
        }
        // Non-local actions holds only what leads to another kind of record (the
        // company's fiscal years and chart); opening this company's own book
        // is a plain icon button on the row. The row's name opens the book.
        let link = |label: &str, href: String| AddToItem {
            label: label.into(),
            href,
            disabled: None,
        };
        let items = match &book {
            Book::Open(_) => vec![
                link(
                    "Fiscal years",
                    format!("/finance/ledger/fiscal-years?company={}", company.id),
                ),
                link(
                    "Chart of accounts",
                    format!("/finance/ledger/accounts?company={}", company.id),
                ),
            ],
            _ => Vec::new(),
        };
        let mut actions = Html::EMPTY;
        if matches!(book, Book::NotOpen) && can_open {
            let label = format!("Open the book of {}", company.name);
            actions.push(&render(IconButton {
                id: &format!("ledger-books-open-{}", company.id),
                href: &format!("/finance/ledger/books/{}", company.id),
                ..IconButton::new("book-open", &label)
            })?);
        }
        actions.push(&render(AddToMenu::new(
            format!("ledger-books-{}-actions", company.id),
            company.name.clone(),
            items,
        ))?);
        row.actions = actions;
        Ok((self.filter("year") == "all" || self.filter("year") == year_state).then_some(row))
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let companies = self.pages.companies(scope).await?;
        let today = self.pages.today(scope).await?;
        let current = Pages::company(scope);
        let form = self.list.filters_form();
        let mut toolbar = FilterToolbar::new(
            "ledger-books-filters",
            vec![
                ToolbarControl::search(
                    "ledger-books-search",
                    "search",
                    "Company",
                    &form["search"],
                    "Company name",
                ),
                ToolbarControl::select(
                    "ledger-books-state",
                    "book",
                    "Book",
                    &form["book"],
                    vec![
                        Choice::new("All books", "all"),
                        Choice::new("Open", "open"),
                        Choice::new("Not open", "not_open"),
                        Choice::new("No access", "no_access"),
                        Choice::new("Could not be read", "unavailable"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-books-year",
                    "year",
                    "Fiscal year",
                    &form["year"],
                    vec![
                        Choice::new("All states", "all"),
                        Choice::new("Open", "open"),
                        Choice::new("Closed", "closed"),
                        Choice::new("Locked", "locked"),
                        Choice::new("None", "none"),
                        Choice::new("No access", "no_access"),
                        Choice::new("Could not be read", "unavailable"),
                    ],
                ),
            ],
        );
        // The toolbar shares its row with the currency filter.
        toolbar.class = "min-w-0 flex-1";
        let toolbar = render(toolbar)?;
        let mut body = Html::EMPTY;
        let mut currencies = BTreeSet::new();
        let mut shown = 0;
        if companies.is_empty() {
            body.push(&no_company("ledger-books-no-company")?);
        } else {
            let mut rows = Vec::new();
            for company in &companies {
                if let Some(row) = self
                    .row(scope, company, current, today, &mut currencies)
                    .await?
                {
                    rows.push(row);
                }
            }
            shown = rows.len();
            if self.columns.is_none() {
                self.columns = Some(
                    ListColumns::mount(
                        "ledger-books",
                        vec![
                            BuiltinColumn::new("company", "Company"),
                            BuiltinColumn::new("book", "Book"),
                            BuiltinColumn::new("currency", "Currency"),
                            BuiltinColumn::new("year", "Latest fiscal year"),
                            BuiltinColumn::new("open", "Earliest open month"),
                        ],
                    )
                    .map_err(|_| Error::Internal)?,
                );
            }
            let columns = self.columns.as_mut().ok_or(Error::Internal)?;
            let lines: Vec<Line> = companies.iter().map(|c| Line(c.id.to_string())).collect();
            columns.load(&lines, |line| line.0.clone());
            let actions = rows.iter().any(|row| !row.actions.is_empty());
            let mut table = FlexTable::new("ledger-books-table", columns.flex_columns(), rows);
            table.actions = actions;
            table.caption = "Company books";
            table.framed = false;
            table.zoom = columns.zoom;
            table.suggestions = columns.flex_suggestions();
            table.add_query = &columns.add_query;
            table.empty = Some(TableEmpty::Content(Html::text("No company to show.")));
            body.push(&render(Card {
                id: "ledger-books-card",
                inner_class: "p-0",
                body: render(table)?,
                ..Card::default()
            })?);
        }
        // The currencies of the books on this page, named as Core Currency
        // names them, so the filter finds "ringgit" as well as "MYR".
        let names = self.currency_names(scope).await?;
        let filters = render(FilterRegion {
            toolbar,
            currency: filter_combobox(
                "ledger-books-currency",
                "currency",
                "Currency",
                self.filter("currency"),
                "All currencies",
                currencies
                    .iter()
                    .map(|code| {
                        Choice::new(names.get(code).map_or(code.as_str(), String::as_str), code)
                            .detail(code)
                    })
                    .collect(),
            )?,
        })?;
        let mut page = render(Header {
            title: Html::text("Company books"),
            subtitle: "Company books you can access",
            title_actions: bilibi_ui::Html::EMPTY,
            ..Header::default()
        })?;
        page.push(&filters);
        page.push(&body);
        Ok(LiveBody {
            html: shell("ledger-books", PageVariant::List, &self.flash, page)?,
            rows: shown,
            query: self.list.to_params(),
            ..LiveBody::default()
        })
    }
}

impl Books {
    /// Currency names by code, from the list a book is opened from.
    async fn currency_names(&self, scope: &Scope) -> Result<BTreeMap<String, String>> {
        #[derive(Default, serde::Deserialize)]
        struct Options {
            #[serde(default)]
            currencies: Vec<Listed>,
        }
        #[derive(serde::Deserialize)]
        struct Listed {
            code: String,
            name: String,
        }
        Ok(
            match self
                .pages
                .read(
                    scope,
                    "finance.ledger.book.currencies",
                    serde_json::json!({}),
                )
                .await?
            {
                Read::Found(value) => view::parse::<Options>(value)
                    .unwrap_or_default()
                    .currencies
                    .into_iter()
                    .map(|c| (c.code, c.name))
                    .collect(),
                _ => BTreeMap::new(),
            },
        )
    }
}

/// The toolbar and the currency filter on one row.
#[derive(askama::Template)]
#[template(
    source = r#"<div id="ledger-books-filter-region" class="flex flex-wrap items-start gap-x-3">{{ toolbar }}{{ currency }}</div>"#,
    ext = "html"
)]
struct FilterRegion {
    toolbar: Html,
    currency: Html,
}

impl LiveState<Scope, Error> for Books {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Params(values) = r.input {
                self.list = ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
            }
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "filters" => {
                        self.list
                            .apply_filters(&params::form_values(value, FILTERS_FORM));
                    }
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
