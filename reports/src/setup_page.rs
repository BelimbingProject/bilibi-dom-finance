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
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Card, Cell, EmptyState, FlashGroup, FlexRow,
    FlexTable, Header, Html, ListColumns, PageVariant, SectionHeading, StatusKind, TableEmpty,
    NO_FLASH,
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
}

struct Line(String);

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

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let mut subtitle = String::new();
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
        let mut page = render(Header {
            title: Html::text("Finance setup"),
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            ..Header::default()
        })?;
        page.push(&body);
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
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Finance setup")
    }

    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = Setup {
                pages: self.clone(),
                columns: HashMap::new(),
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

    fn mount<'a>(&'a self, _r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(Box::new(Setup {
                pages: self.clone(),
                columns: HashMap::new(),
            }) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
