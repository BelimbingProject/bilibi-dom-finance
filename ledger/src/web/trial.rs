//! The trial balance: every account with a posting on or before a date, with
//! its debit and credit balance. The page pins the figures to the last entry
//! it counted, so reloading shows the same numbers; "Show latest" lets them move.
use super::{event_type, figure, no_company, render, shell, unavailable, view, Flash, Pages, Read};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Button, Card, Cell, EmptyState, FilterToolbar,
    FlexRow, FlexTable, Header, Html, ListColumns, PageVariant, TableEmpty, ToolbarControl,
};
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap;

pub(super) struct Trial {
    pages: Pages,
    as_of: Option<String>,
    pin: Option<i64>,
    columns: Option<ListColumns>,
    flash: Flash,
}

impl Trial {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Self {
        Self {
            pages,
            as_of: query.get("as_of").filter(|d| d.len() == 10).cloned(),
            pin: query.get("pin").and_then(|p| p.parse().ok()),
            columns: None,
            flash: Flash::default(),
        }
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let mut shown = 0;
        let mut query = HashMap::new();
        if Pages::company(scope).is_none() {
            body.push(&no_company("ledger-trial-no-company")?);
        } else {
            match self.pages.book(scope).await? {
                Err(refused) => body.push(&unavailable("ledger-trial-refused", "the trial balance", &refused)?),
                Ok(state) if state.book.is_none() => body.push(&render(
                    EmptyState::missing(
                        "The book is not open yet",
                        "The trial balance appears once the company's book is open and entries are posted.",
                    )
                    .id("ledger-trial-no-book"),
                )?),
                Ok(_) => {
                    let as_of = match &self.as_of {
                        Some(date) => date.clone(),
                        None => self.pages.today(scope).await?.to_string(),
                    };
                    let mut input = json!({"as_of": as_of});
                    if let Some(pin) = self.pin {
                        input["through_entry_number"] = json!(pin);
                    }
                    match self
                        .pages
                        .read(scope, "finance.ledger.trial_balance.get", input)
                        .await?
                    {
                        Read::Found(value) => match view::parse::<view::TrialBalance>(value) {
                            Some(trial) => {
                                // Keep the number of the last entry counted, so
                                // a reload shows exactly these figures.
                                self.pin = Some(trial.through_entry_number);
                                self.as_of = Some(trial.as_of.clone());
                                query.insert("as_of".to_owned(), trial.as_of.clone());
                                query.insert("pin".to_owned(), trial.through_entry_number.to_string());
                                shown = trial.rows.len();
                                self.table(&trial, &mut body)?;
                            }
                            None => {
                                body.push(&Self::as_of_control(&as_of)?);
                                body.push(&unavailable("ledger-trial-refused", "the trial balance", &Read::Failed)?);
                            }
                        },
                        Read::Refused(text) => {
                            // A date the ledger will not read (a mistyped day).
                            self.pin = None;
                            body.push(&Self::as_of_control(&as_of)?);
                            body.push(&unavailable("ledger-trial-refused", "the trial balance", &Read::Refused(text))?);
                        }
                        other => {
                            body.push(&Self::as_of_control(&as_of)?);
                            body.push(&unavailable("ledger-trial-refused", "the trial balance", &other)?);
                        }
                    }
                }
            }
        }
        let mut page = render(Header {
            title: Html::text("Trial balance"),
            title_actions: bilibi_ui::Html::EMPTY,
            ..Header::default()
        })?;
        page.push(&body);
        Ok(LiveBody {
            html: shell("ledger-trial", PageVariant::List, &self.flash, page)?,
            query,
            rows: shown,
            ..LiveBody::default()
        })
    }

    fn as_of_control(as_of: &str) -> Result<Html> {
        render(FilterToolbar::new(
            "ledger-trial-filters",
            vec![ToolbarControl::date(
                "ledger-trial-as-of",
                "as_of",
                "As of",
                as_of,
            )],
        ))
    }

    fn table(&mut self, trial: &view::TrialBalance, body: &mut Html) -> Result<()> {
        let units = trial.minor_units;
        body.push(&Self::as_of_control(&trial.as_of)?);
        let balanced = view::decimal(&trial.total_debit) == view::decimal(&trial.total_credit);
        let mut status = render(Badge::status(
            if balanced {
                BadgeKind::Success
            } else {
                BadgeKind::Danger
            },
            if balanced {
                "Debits equal credits"
            } else {
                "Debits and credits differ"
            },
        ))?;
        status.push(&render(Pinned {
            currency: &trial.currency_code,
        })?);
        status.push(&render(Button {
            id: "ledger-trial-latest",
            button_type: "button",
            event: "latest",
            class: "ml-3",
            body: Html::text("Show latest"),
            ..Button::default()
        })?);
        body.push(&render(StatusRow { body: status })?);

        if self.columns.is_none() {
            self.columns = Some(
                ListColumns::mount(
                    "ledger-trial-table",
                    vec![
                        BuiltinColumn::new("code", "Code"),
                        BuiltinColumn::new("name", "Name"),
                        BuiltinColumn::new("type", "Type"),
                        BuiltinColumn::new("debit_balance", "Debit balance").right(),
                        BuiltinColumn::new("credit_balance", "Credit balance").right(),
                        BuiltinColumn::new("debits", "Total debits").right(),
                        BuiltinColumn::new("credits", "Total credits").right(),
                        BuiltinColumn::new("entries", "Entries"),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        let columns = self.columns.as_mut().ok_or(Error::Internal)?;
        columns.load(&trial.rows, |r| r.id.to_string());
        let mut rows = Vec::new();
        for row in &trial.rows {
            rows.push(
                FlexRow::new(row.id.to_string())
                    .slot(
                        "code",
                        render(super::AccountLink {
                            id: row.id,
                            label: &row.code,
                        })?,
                    )
                    .cell("name", Cell::text(&row.name))
                    .cell("type", Cell::text(view::type_label(&row.account_type)))
                    .slot(
                        "debit_balance",
                        figure(&view::amount_or_dash(&row.debit_balance, units))?,
                    )
                    .slot(
                        "credit_balance",
                        figure(&view::amount_or_dash(&row.credit_balance, units))?,
                    )
                    .slot("debits", figure(&view::amount(&row.debits, units))?)
                    .slot("credits", figure(&view::amount(&row.credits, units))?)
                    .slot(
                        "entries",
                        render(EntriesLink {
                            href: format!(
                                "/finance/ledger/journals?account={}&to={}&through={}",
                                row.id, trial.as_of, trial.through_entry_number
                            ),
                        })?,
                    ),
            );
        }
        let totals: (Decimal, Decimal) = trial
            .rows
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(d, c), r| {
                (d + view::decimal(&r.debits), c + view::decimal(&r.credits))
            });
        rows.push(
            FlexRow::new("total")
                .cell("code", Cell::text("Total"))
                .slot(
                    "debit_balance",
                    figure(&view::amount(&trial.total_debit, units))?,
                )
                .slot(
                    "credit_balance",
                    figure(&view::amount(&trial.total_credit, units))?,
                )
                .slot(
                    "debits",
                    figure(&view::amount(&totals.0.to_string(), units))?,
                )
                .slot(
                    "credits",
                    figure(&view::amount(&totals.1.to_string(), units))?,
                ),
        );
        let mut table = FlexTable::new("ledger-trial-table", columns.flex_columns(), rows);
        table.caption = "Trial balance";
        table.framed = false;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            "Nothing is posted on or before this date.",
        )));
        body.push(&render(Card {
            id: "ledger-trial-card",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?);
        Ok(())
    }
}

#[derive(Template)]
#[template(
    source = r#"<span id="ledger-trial-pin" class="ml-3 text-sm text-ink-muted">Amounts in {{ currency }}.</span>"#,
    ext = "html"
)]
struct Pinned<'a> {
    currency: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<div id="ledger-trial-status" class="mb-3 flex flex-wrap items-center">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct StatusRow {
    body: Html,
}

#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="text-link hover:underline">Entries</a>"#,
    ext = "html"
)]
struct EntriesLink {
    href: String,
}

impl LiveState<Scope, Error> for Trial {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.as_of = values.get("as_of").filter(|d| d.len() == 10).cloned();
                    self.pin = values.get("pin").and_then(|p| p.parse().ok());
                }
                LiveInput::Event(value) => match event_type(value)? {
                    "filters" => {
                        let posted = params::form_values(value, FILTERS_FORM);
                        self.as_of = posted.get("as_of").filter(|d| d.len() == 10).cloned();
                        // A new date reads the ledger as it is now.
                        self.pin = None;
                    }
                    "latest" => self.pin = None,
                    "grid" => {
                        if let Some(columns) = &mut self.columns {
                            columns.handle(&params::string_values(value));
                        }
                    }
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(r.scope).await
        })
    }
}
