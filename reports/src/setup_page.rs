//! The Finance setup page: what the current company has set up and what
//! remains, each area linking to the page that sets it up. It reads the
//! overview through `finance.reports.setup.get`, the contract an agent calls,
//! and writes nothing. Setup is per company: the page acts on the session's
//! current company and names it; the books of every company the person works
//! in are on the ledger's Company books page.
use crate::setup::SetupState;
use askama::Template;
use bilibi_agent_api::{page::PageContracts, Error as ContractError};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{Clock, DisplayPolicy};
use bilibi_finance_ledger::{MinorUnits, Money, OwedFigure, OwedSummary, Tally};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, Card, Cell, EmptyState, FlashGroup,
    FlexRow, FlexTable, Header, Html, ListColumns, PageVariant, SectionHeading, StatItem,
    StatStrip, StatusKind, TableEmpty, NO_FLASH,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub(crate) const PAGES: &str = "finance/reports.setup-pages";

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<PageContracts>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

/// An area's name, linked to where it is set up unless the person may not
/// read it.
#[derive(Template)]
#[template(
    source = r#"{% if linked %}<a id="setup-{{ id }}-link" href="{{ path }}" class="text-link hover:underline">{{ name }}</a>{% else %}<span>{{ name }}</span>{% endif %}"#,
    ext = "html"
)]
struct AreaName<'a> {
    id: &'a str,
    name: &'a str,
    path: &'a str,
    linked: bool,
}

/// One area, as the contract answers it.
#[derive(Deserialize)]
struct SetupArea {
    id: String,
    name: String,
    state: SetupState,
    optional: bool,
    summary: String,
    path: String,
}

struct Setup {
    pages: Pages,
    columns: HashMap<&'static str, ListColumns>,
    details: bool,
}

struct Line(String);

#[derive(Template)]
#[template(
    source = r#"<div class="space-y-4">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Stack {
    body: Html,
}

impl Setup {
    fn badge(area: &SetupArea) -> Result<Html> {
        render(match (area.state, area.optional) {
            (SetupState::SetUp, _) => Badge::status(BadgeKind::Success, "Set up"),
            (SetupState::NotSetUp, true) => Badge::status(BadgeKind::Neutral, "Not set up"),
            (SetupState::NotSetUp, false) => Badge::status(BadgeKind::Warning, "Not set up"),
            (SetupState::Incomplete, _) => Badge::status(BadgeKind::Warning, "Incomplete"),
            (SetupState::NoAccess, _) => Badge::status(BadgeKind::Neutral, "No access"),
            (SetupState::Unavailable, _) => Badge::status(BadgeKind::Neutral, "Could not be read"),
            (SetupState::NotTracked, _) => Badge::status(BadgeKind::Neutral, "Not tracked"),
        })
    }

    fn table(
        &mut self,
        group: &'static str,
        heading: &str,
        description: &str,
        areas: &[SetupArea],
    ) -> Result<Html> {
        if areas.is_empty() {
            return Ok(Html::EMPTY);
        }
        if !self.columns.contains_key(group) {
            self.columns.insert(
                group,
                ListColumns::mount(
                    group,
                    vec![
                        BuiltinColumn::new("area", "Area"),
                        BuiltinColumn::new("status", "Status"),
                        BuiltinColumn::new("detail", "What it holds"),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        let columns = self.columns.get_mut(group).ok_or(Error::Internal)?;
        let lines: Vec<Line> = areas.iter().map(|a| Line(a.id.clone())).collect();
        columns.load(&lines, |line| line.0.clone());
        let mut rows = Vec::new();
        for area in areas {
            let linked = !matches!(area.state, SetupState::NoAccess);
            let mut row = FlexRow::new(area.id.clone())
                .slot(
                    "area",
                    render(AreaName {
                        id: &area.id,
                        name: &area.name,
                        path: &area.path,
                        linked,
                    })?,
                )
                .slot("status", Self::badge(area)?)
                .cell("detail", Cell::text(&area.summary));
            row.dom_id = Some(format!("setup-{}", area.id));
            rows.push(row);
        }
        let table_id = format!("{group}-table");
        let mut table = FlexTable::new(&table_id, columns.flex_columns(), rows);
        table.caption = "Setup";
        table.framed = false;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text("Nothing to show.")));
        let heading_id = format!("{group}-heading");
        let mut card = render(SectionHeading {
            id: &heading_id,
            title: heading,
            description,
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        card.push(&render(table)?);
        render(Card {
            id: &format!("{group}-card"),
            inner_class: "p-0",
            class: "mt-4",
            role: "region",
            labelledby: &heading_id,
            body: card,
            ..Card::default()
        })
    }

    async fn home(&self, scope: &Scope, company: i64) -> Result<Html> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        let display = self
            .pages
            .display
            .display(&**client, scope, scope.current_company(), locale)
            .await?;
        let clock = Clock::resolve(&display, None, jiff::Timestamp::now());
        let today = clock.today.to_string();
        drop(client);
        let book = self
            .pages
            .contracts
            .read(
                scope,
                "finance.ledger.book.get",
                &json!({"company_id":company}),
            )
            .await;
        let (units, currency) = match book {
            Ok(value) => (
                value["result"]["book"]["minor_units"]
                    .as_i64()
                    .and_then(MinorUnits::new),
                value["result"]["book"]["currency_code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Err(_) => (None, String::new()),
        };
        let mut body = Html::EMPTY;
        for (id, heading, contract, path, singular, plural) in [
            (
                "home-receivables",
                "Money owed to you",
                "finance.receivables.document.summary",
                "/finance/receivables/invoices",
                "invoice",
                "invoices",
            ),
            (
                "home-payables",
                "Bills to pay",
                "finance.payables.document.summary",
                "/finance/payables/bills",
                "bill",
                "bills",
            ),
        ] {
            let answer = self
                .pages
                .contracts
                .read(
                    scope,
                    contract,
                    &json!({"company_id":company,"as_of":today}),
                )
                .await;
            let Ok(answer) = answer else { continue };
            let Ok(summary) = serde_json::from_value::<OwedSummary>(answer["result"].clone())
            else {
                continue;
            };
            let amount = |tally: &Tally| {
                format!(
                    "{} {}",
                    Money::display_decimal(tally.amount, units),
                    currency
                )
                .trim()
                .to_owned()
            };
            let mut section = render(SectionHeading {
                id: &format!("{id}-heading"),
                title: heading,
                ..SectionHeading::default()
            })?;
            section.push(&OwedSummary::strips(
                id,
                vec![
                    OwedFigure {
                        title: "Awaiting payment",
                        detail: OwedSummary::counted(
                            summary.awaiting_payment.count,
                            singular,
                            plural,
                        ),
                        amount: amount(&summary.awaiting_payment),
                        href: format!("{path}?status=unpaid"),
                    },
                    OwedFigure {
                        title: "Overdue",
                        detail: summary.overdue_detail(singular, plural),
                        amount: amount(&summary.overdue),
                        href: format!("{path}?status=overdue"),
                    },
                ],
            )?);
            body.push(&section);
        }
        if let Ok(answer) = self
            .pages
            .contracts
            .read(
                scope,
                "finance.reports.home.get",
                &json!({"company_id":company,"as_of":today,"timezone":clock.timezone}),
            )
            .await
        {
            let data = &answer["result"];
            let mut work = Html::EMPTY;
            if let Some(banks) = data["bank_accounts"].as_array() {
                for bank in banks {
                    let id = bank["bank_account_id"].as_i64().unwrap_or_default();
                    let count = bank["count"].as_i64().unwrap_or_default();
                    let name = bank["name"].as_str().unwrap_or_default();
                    let href = format!("/finance/reconciliation/accounts/{id}/matching");
                    let label = if count == 1 {
                        "statement line to match"
                    } else {
                        "statement lines to match"
                    };
                    work.push(&render(StatStrip {
                        id: &format!("home-bank-{id}"),
                        title: name,
                        href: &href,
                        items: vec![StatItem::number(label, &count.to_string())],
                    })?);
                }
            }
            if let Ok(page) = self.pages.contracts.read(scope, "finance.payables.document.page", &json!({"company_id":company,"status":"draft","page":1,"page_size":25,"as_of":today})).await {
                if let Some(count) = page["result"]["total"].as_u64() {
                    work.push(&render(StatStrip { id: "home-bill-drafts", title: "Draft bills", href: "/finance/payables/bills?status=draft", items: vec![StatItem::number("bills and debit notes", &count.to_string())] })?);
                }
            }
            if let Some(count) = data["journal_approvals"].as_i64() {
                work.push(&render(StatStrip {
                    id: "home-journals",
                    title: "Journals waiting for approval",
                    href: "/finance/ledger/journals?status=submitted",
                    items: vec![StatItem::number("journals", &count.to_string())],
                })?);
            }
            if let Some(count) = data["payment_approvals"].as_i64() {
                work.push(&render(StatStrip {
                    id: "home-approvals",
                    title: "Payments waiting for approval",
                    href: "/finance/payables/payments?status=submitted",
                    items: vec![StatItem::number("payment requests", &count.to_string())],
                })?);
            }
            if let Some(failed) = data["failed_einvoices"].as_array() {
                for item in failed {
                    let status = item["status"].as_str().unwrap_or_default();
                    let title = if status == "refused" {
                        "E-invoices refused"
                    } else {
                        "E-invoices invalid"
                    };
                    let href = format!("/finance/einvoice/documents?status={status}");
                    work.push(&render(StatStrip {
                        id: &format!("home-einvoice-{status}"),
                        title,
                        href: &href,
                        items: vec![StatItem::number(
                            "e-invoices",
                            &item["count"].as_u64().unwrap_or_default().to_string(),
                        )],
                    })?);
                }
            }
            if let Some(periods) = data["due_periods"].as_array() {
                for period in periods {
                    let start = period["period_start"].as_str().unwrap_or_default();
                    let due = bilibi_finance_ledger::dates::display_text(
                        period["due_date"].as_str().unwrap_or_default(),
                    );
                    let period_name = bilibi_finance_ledger::dates::months_text(
                        start,
                        period["period_end"].as_str().unwrap_or_default(),
                    );
                    let href = period["return_id"].as_i64().map_or_else(
                        || format!("/finance/sst#period-{start}"),
                        |id| format!("/finance/sst/returns/{id}"),
                    );
                    work.push(&render(StatStrip {
                        id: &format!("home-return-{start}"),
                        title: "SST return due",
                        href: &href,
                        items: vec![StatItem::number(period_name, format!("Due {due}"))],
                    })?);
                }
            }
            if !work.to_string().is_empty() {
                body.push(&render(SectionHeading {
                    id: "home-work-heading",
                    title: "Work to do",
                    ..SectionHeading::default()
                })?);
                body.push(&render(Stack { body: work })?);
            }
        }
        body.push(&render(Card {
            id: "home-assistant",
            title: "Ask your AI assistant",
            body: Html::text("Your assistant can help prepare Finance work for you to review."),
            ..Card::default()
        })?);
        Ok(body)
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let mut subtitle = String::new();
        let mut is_home = false;
        match scope.current_company() {
            None => body.push(&render(
                EmptyState::missing(
                    "No current company",
                    "Choose a current company in the top bar to see its setup.",
                )
                .id("setup-no-company"),
            )?),
            Some(company) => {
                let client = self.pages.db.get().await?;
                subtitle = self
                    .pages
                    .parties
                    .organisation_name(&**client, scope, company)
                    .await?
                    .map(|c| c.name)
                    .unwrap_or_else(|| "Company unavailable".into());
                drop(client);
                match self
                    .pages
                    .contracts
                    .read(
                        scope,
                        "finance.reports.setup.get",
                        &json!({"company_id": company.get()}),
                    )
                    .await
                {
                    Ok(value) => {
                        let areas: Vec<SetupArea> =
                            serde_json::from_value(value["result"]["areas"].clone())
                                .unwrap_or_default();
                        let result = &value["result"];
                        let complete = result["required_total"].as_u64().is_some_and(|total| {
                            total > 0 && result["required_set_up"].as_u64() == Some(total)
                        });
                        if complete && !self.details {
                            is_home = true;
                            body.push(&self.home(scope, company.get()).await?);
                        } else {
                            let summary = match (
                            result["required_set_up"].as_u64(),
                            result["required_total"].as_u64(),
                        ) {
                            (Some(done), Some(total)) => {
                                format!("{done} of {total} steps needed to start work are set up.")
                            }
                            _ => {
                                "Some steps cannot be read with your access, so no total is shown."
                                    .to_owned()
                            }
                        };
                            body.push(&render(Alert {
                                id: "setup-summary",
                                kind: StatusKind::Info,
                                class: "mb-3",
                                body: Html::text(&summary),
                            })?);
                            let (optional, required): (Vec<SetupArea>, Vec<SetupArea>) =
                                areas.into_iter().partition(|a| a.optional);
                            body.push(&self.table(
                                "setup-required",
                                "Start here",
                                "A company needs these before it can post.",
                                &required,
                            )?);
                            body.push(&self.table(
                                "setup-optional",
                                "As the company needs them",
                                "Set these up when the company uses them.",
                                &optional,
                            )?);
                        }
                    }
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                        body.push(&render(
                            EmptyState::forbidden("view the Finance setup").id("setup-forbidden"),
                        )?)
                    }
                    Err(ContractError::Backend(
                        error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                    )) => return Err(error),
                    Err(_) => body.push(&render(
                        EmptyState::missing(
                            "This could not be shown",
                            "Reload the page. If it keeps failing, ask your administrator.",
                        )
                        .id("setup-failed"),
                    )?),
                }
            }
        }
        let title = if is_home {
            "Finance home"
        } else {
            "Finance setup"
        };
        let actions = if is_home {
            render(Button {
                id: "home-setup",
                href: "/finance/setup/details",
                body: Html::text("Finance setup"),
                ..Button::default()
            })?
        } else if self.details {
            render(Button {
                id: "setup-home",
                href: "/finance/setup",
                body: Html::text("Finance home"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut page = render(Header {
            title: Html::text(title),
            actions,
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            ..Header::default()
        })?;
        page.push(&render(Stack { body })?);
        let mut html = render(FlashGroup {
            id: "flash-group",
            flash: NO_FLASH,
        })?;
        html.push(&render(bilibi_ui::Page {
            id: "setup",
            variant: PageVariant::Detail,
            body: page,
            ..bilibi_ui::Page::default()
        })?);
        Ok(LiveBody {
            html: html.to_string(),
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Setup {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match value.get("type").and_then(Value::as_str) {
                    Some("grid") => {
                        for columns in self.columns.values_mut() {
                            columns.handle(&params::string_values(value));
                        }
                    }
                    Some("clear-flash") => {}
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(if operation == "details" {
            "Finance setup"
        } else {
            "Finance home"
        })
    }

    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = Setup {
                pages: self.clone(),
                columns: HashMap::new(),
                details: r.operation == "details",
            };
            let body = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(Box::new(Setup {
                pages: self.clone(),
                columns: HashMap::new(),
                details: r.operation == "details",
            }) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
