//! Reviewed source-period schedules, shared with the agent contract.
use crate::{MODULE, VIEW};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Cell, Choice,
    Combobox, CreateButton, EmptyState, Fact, Facts, Field, FlexRow, FlexTable, Form, Header, Html,
    InputKind, ListColumns, Modal, ModalWidth, PageVariant, StatusKind, Tab, TableEmpty, Tabs,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

mod bridge;
mod correct;
mod detail;
mod form;
mod paste;

pub(crate) const PAGES: &str = "finance/reports.source-period-pages";
const PATH: &str = "/finance/reports/source-periods";
const LIST: &str = "finance.reports.source_period.list";
const IMPORT: &str = "finance.reports.source_period.import";
const KINDS: [(&str, &str, &str); 4] = [
    (
        "comparative",
        "Comparatives",
        "/finance/reports/source-periods?kind=comparative",
    ),
    (
        "movement",
        "Monthly movements",
        "/finance/reports/source-periods?kind=movement",
    ),
    (
        "sst",
        "Filed SST",
        "/finance/reports/source-periods?kind=sst",
    ),
    (
        "payroll",
        "Payroll remittances",
        "/finance/reports/source-periods?kind=payroll",
    ),
];
inventory::submit! { Contribution::new(ROUTES,"finance/reports.source-periods",Provenance {module:MODULE,source:file!(),line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company, method:Method::Get,path:"/finance/reports/source-periods",service:PAGES,operation:"list",access:Access::Capability(VIEW),framed:true }) }
inventory::submit! { Contribution::new(ROUTES,"finance/reports.source-period",Provenance {module:MODULE,source:file!(),line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company, method:Method::Get,path:"/finance/reports/source-periods/{kind}/{id}",service:PAGES,operation:"detail",access:Access::Capability(VIEW),framed:true }) }
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page(
        "finance.reports.source-periods",
        "Source-period schedules",
        PATH
    )
    .company_scoped()
    .under("finance.group.bookkeeping")
    .icon("gmdi-money-r")
    .requires(bilibi_menu::Requirement::Capability(VIEW))
    .order(65)
);
inventory::submit! { Contribution::new(PAGE_WRITES,PAGES,Provenance {module:MODULE,source:file!(),line:line!()}, &PageWrites {service:PAGES,contracts:&[("preview",IMPORT),("do_confirm",IMPORT),("preview_correction",crate::source_period::REPLACE),("do_correct",crate::source_period::REPLACE),("preview_withdraw",crate::source_period::WITHDRAW),("do_withdraw",crate::source_period::WITHDRAW)],pending:Pending::None}) }
fn render(t: impl Template) -> Result<Html> {
    Html::render(&t).map_err(|_| Error::Internal)
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    buttons: Html,
}
/// The header's "+ Schedule", opening the dialog.
fn new_button(id: &str) -> Result<Html> {
    render(CreateButton {
        noun: "Schedule",
        id,
        event: "new_schedule",
        button_type: "button",
        ..CreateButton::default()
    })
}
/// The opening entry a schedule was retained against, as a link.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">Open the opening entry</a>"#,
    ext = "html"
)]
struct EntryLink {
    id: i64,
}
#[derive(Template)]
#[template(
    source = r#"<div id="source-period-attachments-slot" class="mt-4" data-live-ignore><div id="source-period-attachments"></div></div>"#,
    ext = "html"
)]
struct AttachmentSlot;
#[derive(Clone)]
pub(crate) struct Pages {
    pub contracts: Arc<PageContracts>,
}
struct Review {
    input: Value,
    facts: Value,
    /// The contract the confirmation commits and the live event that does.
    contract: &'static str,
    confirm: &'static str,
}
struct State {
    pages: Pages,
    kind: String,
    id: Option<i64>,
    framed: bool,
    after: i64,
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
    message: Option<String>,
    review: Option<Review>,
    records: Vec<Value>,
    columns: ListColumns,
    /// The "+ Schedule" dialog is open.
    adding: bool,
    /// The rows pasted into the dialog, and how they are read.
    paste_text: String,
    paste: Option<paste::Paste>,
    /// The book's decimal places and currency and the chart, read once per dialog.
    chart: Option<(
        bilibi_finance_ledger::MinorUnits,
        String,
        Vec<paste::Account>,
    )>,
    /// What the dialog refuses that has no field of its own.
    dialog_message: Option<String>,
    /// The "+ Schedule" dialog corrects this retained schedule (id and
    /// source reference) instead of adding a new one.
    replacing: Option<(i64, String)>,
    /// The withdraw dialog is open.
    withdrawing: bool,
}
impl State {
    fn query(&self) -> HashMap<String, String> {
        let mut query = HashMap::from([("kind".into(), self.kind.clone())]);
        if self.after > 0 {
            query.insert("after".into(), self.after.to_string());
        }
        query
    }
    fn new(pages: Pages, r: &PageRequest<'_, Scope>) -> Result<Self> {
        let selected = if r.operation == "detail" {
            r.path.get("kind")
        } else {
            r.query.get("kind")
        }
        .map(String::as_str)
        .unwrap_or("comparative");
        if !KINDS.iter().any(|(k, _, _)| *k == selected) {
            return Err(Error::NotFound);
        }
        Ok(Self {
            pages,
            kind: selected.into(),
            id: if r.operation == "detail" {
                Some(
                    params::positive_id(r.path.get("id").map(String::as_str))
                        .ok_or(Error::NotFound)?,
                )
            } else {
                None
            },
            framed: r.framed,
            after: r
                .query
                .get("after")
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(0),
            values: HashMap::new(),
            errors: BTreeMap::new(),
            message: None,
            review: None,
            records: vec![],
            adding: false,
            paste_text: String::new(),
            paste: None,
            chart: None,
            dialog_message: None,
            replacing: None,
            withdrawing: false,
            columns: ListColumns::mount(
                "source-periods",
                vec![
                    BuiltinColumn::new("source", "Source reference"),
                    BuiltinColumn::new("period", "Period"),
                    BuiltinColumn::new("system", "Source system"),
                    BuiltinColumn::new("status", "Status"),
                ],
            )
            .map_err(|_| Error::Internal)?,
        })
    }
    fn company(scope: &Scope) -> Result<i64> {
        scope
            .current_company()
            .map(|id| id.get())
            .ok_or(Error::NotFound)
    }
    async fn read(&self, scope: &Scope, key: &str, input: Value) -> Result<Value> {
        self.pages
            .contracts
            .read(scope, key, &input)
            .await
            .map(|v| v["result"].clone())
            .map_err(|error| match error {
                ContractError::Forbidden => Error::Forbidden,
                ContractError::NotFound => Error::NotFound,
                ContractError::Backend(error) => error,
                _ => Error::Internal,
            })
    }
    async fn body(&mut self, scope: &Scope) -> Result<LiveBody> {
        let company = Self::company(scope)?;
        let mut input = json!({"company_id":company,"kind":self.kind});
        if let Some(id) = self.id {
            input["id"] = json!(id);
        } else if self.after > 0 {
            input["after"] = json!(self.after);
        }
        let list = match self.read(scope, LIST, input).await {
            Ok(list) => list,
            Err(Error::Forbidden) => {
                self.records.clear();
                let mut body = render(Header {
                    title: Html::text("Source-period schedules"),
                    subtitle: "Retained evidence for the current company's conversion.",
                    ..Header::default()
                })?;
                let tabs = KINDS
                    .iter()
                    .map(|(key, label, href)| Tab::link(href, label, self.kind == *key))
                    .collect();
                body.push(&render(Tabs {
                    id: "source-period-kinds",
                    label: "Schedule kind",
                    tabs,
                    ..Tabs::default()
                })?);
                body.push(&render(Alert {
                    id: "source-period-no-access",
                    kind: StatusKind::Warning,
                    body: Html::text("No access to these source schedules in the current company."),
                    ..Alert::default()
                })?);
                return Ok(LiveBody {
                    html: render(bilibi_ui::Page {
                        id: "source-period-page",
                        variant: PageVariant::Form,
                        body,
                        ..Default::default()
                    })?
                    .to_string(),
                    query: self.query(),
                    ..LiveBody::default()
                });
            }
            Err(error) => return Err(error),
        };
        self.records = list["records"].as_array().cloned().unwrap_or_default();
        let mut body = render(Header {
            title: Html::text("Source-period schedules"),
            subtitle: "Retained evidence for the current company's conversion.",
            actions: if self.id.is_none() {
                new_button("source-period-new")?
            } else {
                self.detail_actions()?
            },
            ..Header::default()
        })?;
        if let Some(message) = self.message.as_ref().filter(|_| self.id.is_none()) {
            body.push(&render(Alert {
                id: "source-period-message",
                kind: StatusKind::Info,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        if self.id.is_some() {
            body.push(&self.detail(scope, company).await?);
        } else {
            let tabs = KINDS
                .iter()
                .map(|(key, label, href)| Tab::link(href, label, self.kind == *key))
                .collect();
            body.push(&render(Tabs {
                id: "source-period-kinds",
                label: "Schedule kind",
                tabs,
                ..Tabs::default()
            })?);
            let rows = self
                .records
                .iter()
                .map(|r| {
                    let id = r["id"].as_i64().unwrap_or_default();
                    let reference = r["source_reference"].as_str().unwrap_or("");
                    let href = format!("{PATH}/{}/{id}", self.kind);
                    let link = render(bilibi_ui::lists::RecordLink {
                        workspace: self.framed,
                        kind: MODULE,
                        record_id: &id.to_string(),
                        href: &href,
                        class: "font-medium text-ink-strong hover:underline",
                        body: Html::text(reference),
                    })
                    .unwrap_or(Html::EMPTY);
                    let row = FlexRow::new(id.to_string())
                        .slot("source", link)
                        .cell(
                            "period",
                            Cell::text(format!(
                                "{} – {}",
                                bilibi_finance_ledger::dates::display_text(
                                    r["period_start"].as_str().unwrap_or("")
                                ),
                                bilibi_finance_ledger::dates::display_text(
                                    r["period_end"].as_str().unwrap_or("")
                                )
                            )),
                        )
                        .cell(
                            "system",
                            Cell::text(r["source_system"].as_str().unwrap_or("")),
                        );
                    // A corrected schedule stays listed as evidence, marked.
                    let (kind, status) = match r["correction"]["replacement_id"].as_i64() {
                        Some(_) => (
                            BadgeKind::Warning,
                            format!(
                                "Replaced by {}",
                                r["correction"]["replacement_reference"]
                                    .as_str()
                                    .unwrap_or("")
                            ),
                        ),
                        None if !r["correction"].is_null() => {
                            (BadgeKind::Neutral, "Withdrawn".to_owned())
                        }
                        None => (BadgeKind::Success, "Current".to_owned()),
                    };
                    row.cell("status", Cell::text(&status)).slot(
                        "status",
                        render(Badge::status(kind, &status)).unwrap_or(Html::EMPTY),
                    )
                })
                .collect();
            let mut table = FlexTable::new("source-periods", self.columns.flex_columns(), rows);
            table.caption = "Source-period schedules";
            table.event = "grid";
            table.empty = Some(TableEmpty::State(
                EmptyState::missing("No schedules of this kind yet", "")
                    .id("source-periods-empty")
                    .action(new_button("source-period-new-empty")?),
            ));
            body.push(&render(table)?);
            if let Some(next) = list["next"].as_i64() {
                let href = format!("{PATH}?kind={}&after={next}", self.kind);
                body.push(&render(Button {
                    href: &href,
                    body: Html::text("Next schedules"),
                    ..Button::default()
                })?);
            }
            if self.kind == "movement" {
                body.push(&self.bridge(scope, company).await?);
            }
        }
        // The schedule is entered (or corrected) in a dialog, which the
        // review replaces.
        if self.adding && self.review.is_none() {
            let (title, description) = match &self.replacing {
                Some((_, reference)) => (
                    "Correct source schedule",
                    format!("Retain a corrected copy of {reference}. The original stays as evidence, marked replaced."),
                ),
                None => (
                    "New source schedule",
                    "Retain a source report's rows for the current company.".to_owned(),
                ),
            };
            body.push(&render(Modal {
                id: "source-period-modal",
                title,
                description: &description,
                cancel_event: "close_schedule",
                width: ModalWidth::Wide,
                body: self.form(scope, company).await?,
                ..Modal::default()
            })?);
        }
        if self.withdrawing && self.review.is_none() {
            body.push(&self.withdraw_modal()?);
        }
        if let Some(review) = &self.review {
            body.push(&if review.contract == crate::source_period::WITHDRAW {
                self.withdraw_review_modal(review)?
            } else {
                self.review_modal(review)?
            });
        }
        Ok(LiveBody {
            html: render(bilibi_ui::Page {
                id: "source-period-page",
                variant: PageVariant::Form,
                body,
                ..Default::default()
            })?
            .to_string(),
            query: if self.id.is_none() {
                self.query()
            } else {
                Default::default()
            },
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match value["type"].as_str().unwrap_or("") {
                    "schedule_change" => self.take_details(value)?,
                    "paste_headed" => {
                        if let Some(paste) = &mut self.paste {
                            paste.set_headed(value["on"].as_str() == Some("true"));
                        }
                    }
                    "paste_columns" => {
                        if let Some(paste) = &mut self.paste {
                            paste.choose(
                                &params::form_values_checked(value, paste::FORM, 64)
                                    .map_err(Error::refused)?,
                            );
                        }
                    }
                    "preview" => self.preview(&r, IMPORT, "preview", "do_confirm").await?,
                    "preview_correction" => {
                        self.preview(
                            &r,
                            crate::source_period::REPLACE,
                            "preview_correction",
                            "do_correct",
                        )
                        .await?
                    }
                    "preview_withdraw" => self.preview_withdraw(&r, value).await?,
                    "do_confirm" | "do_correct" | "do_withdraw" => {
                        let event = value["type"].as_str().unwrap_or("");
                        self.commit(&r, event).await?
                    }
                    "correct_schedule" => self.open_correction()?,
                    "withdraw_schedule" => {
                        self.withdrawing = true;
                        self.values.clear();
                        self.errors.clear();
                        self.dialog_message = None;
                    }
                    "close_withdraw" => {
                        self.withdrawing = false;
                        self.values.clear();
                        self.errors.clear();
                        self.dialog_message = None;
                    }
                    "cancel_review" => self.review = None,
                    "new_schedule" => {
                        self.adding = true;
                        self.message = None;
                    }
                    "close_schedule" => {
                        self.adding = false;
                        self.replacing = None;
                        self.values.clear();
                        self.errors.clear();
                        self.paste_text.clear();
                        self.paste = None;
                        self.dialog_message = None;
                    }
                    "grid" => {
                        self.columns.handle(&params::string_values(value));
                    }
                    _ => return Err(Error::Invalid),
                }
            }
            self.body(r.scope).await
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        let Some(id) = self.id else { return vec![] };
        if self.records.is_empty() {
            return vec![];
        }
        vec![PanelSpec {
            id: "source-period-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: HashMap::from([
                ("panel".into(), "source-period-attachments".into()),
                (
                    "source".into(),
                    format!("finance/reports/source_{}", self.kind),
                ),
                ("document".into(), id.to_string()),
            ]),
            query: HashMap::new(),
        }]
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _: &str) -> Option<&'static str> {
        Some("Source-period schedules")
    }
    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = State::new(self.clone(), &r)?;
            let body = state.body(r.scope.ok_or(Error::Unauthorized)?).await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(
                Box::new(State::new(self.clone(), &r)?) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}
