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
    page::*, params, Alert, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice, Combobox,
    Fact, Facts, Field, FlexRow, FlexTable, Form, Header, Html, InputKind, ListColumns, Modal,
    ModalWidth, PageVariant, StatusKind, Tab, Tabs,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

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
inventory::submit! { Contribution::new(PAGE_WRITES,PAGES,Provenance {module:MODULE,source:file!(),line:line!()}, &PageWrites {service:PAGES,contracts:&[("preview",IMPORT),("do_confirm",IMPORT)],pending:Pending::None}) }
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
            columns: ListColumns::mount(
                "source-periods",
                vec![
                    BuiltinColumn::new("source", "Source reference"),
                    BuiltinColumn::new("period", "Period"),
                    BuiltinColumn::new("system", "Source system"),
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
            ..Header::default()
        })?;
        if let Some(message) = &self.message {
            body.push(&render(Alert {
                id: "source-period-message",
                kind: StatusKind::Info,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        if self.id.is_some() {
            let record = self.records.first().ok_or(Error::NotFound)?;
            let label = record["source_reference"].as_str().unwrap_or("");
            body.push(&render(Alert {id:"source-period-detail",body:Html::text(&format!("Source {label}; {} to {}; {}. These are retained source facts, not postings or submissions.",record["period_start"].as_str().unwrap_or(""),record["period_end"].as_str().unwrap_or(""),record["currency_code"].as_str().unwrap_or(""))),..Alert::default()})?);
            body.push(&render(Facts {
                id: "source-period-facts",
                items: vec![
                    Fact {
                        title: "Source system",
                        value: Html::text(record["source_system"].as_str().unwrap_or("")),
                        ..Fact::default()
                    },
                    Fact {
                        title: "Accounting time zone",
                        value: Html::text(record["accounting_timezone"].as_str().unwrap_or("")),
                        ..Fact::default()
                    },
                    Fact {
                        title: "Conversion date",
                        value: Html::text(record["conversion_date"].as_str().unwrap_or("")),
                        ..Fact::default()
                    },
                    Fact {
                        title: "Opening entry ID",
                        value: Html::text(&record["opening_entry_id"].to_string()),
                        ..Fact::default()
                    },
                ],
            })?);
            let book = self
                .read(
                    scope,
                    "finance.ledger.book.get",
                    json!({"company_id":company}),
                )
                .await?;
            let units = book["book"]["minor_units"]
                .as_i64()
                .and_then(bilibi_finance_ledger::MinorUnits::new)
                .ok_or(Error::Internal)?;
            let statutory = self.kind == "sst" || self.kind == "payroll";
            let mut detail_columns = vec![
                BuiltinColumn::new("source", "Source ID"),
                BuiltinColumn::new("item", "Account or reference"),
                BuiltinColumn::new("side", "Side or authority"),
            ];
            if statutory {
                detail_columns.extend([
                    BuiltinColumn::new("event_date", "Event date"),
                    BuiltinColumn::new("status", "Status"),
                ]);
            }
            detail_columns.push(BuiltinColumn::new("amount", "Amount").right());
            let columns = ListColumns::mount("source-period-detail-rows", detail_columns)
                .map_err(|_| Error::Internal)?;
            let rows = record["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let amount = r["amount"]
                        .as_str()
                        .and_then(|v| bilibi_finance_ledger::Money::parse(v, units).ok())
                        .map(|v| v.display(units))
                        .unwrap_or_default();
                    let item = r["account_name"]
                        .as_str()
                        .map(|name| {
                            format!("{} · {}", name, r["account_code"].as_str().unwrap_or(""))
                        })
                        .unwrap_or_else(|| r["reference"].as_str().unwrap_or("").to_owned());
                    let side = r["side"]
                        .as_str()
                        .or_else(|| r["authority"].as_str())
                        .unwrap_or("");
                    let mut row = FlexRow::new(r["source_id"].as_str().unwrap_or(""))
                        .cell("source", Cell::text(r["source_id"].as_str().unwrap_or("")))
                        .cell("item", Cell::text(item))
                        .cell("side", Cell::text(side));
                    if statutory {
                        row = row
                            .cell(
                                "event_date",
                                Cell::text(r["event_date"].as_str().unwrap_or("")),
                            )
                            .cell("status", Cell::text(r["status"].as_str().unwrap_or("")));
                    }
                    row.cell("amount", Cell::text(amount))
                })
                .collect();
            let mut table =
                FlexTable::new("source-period-detail-rows", columns.flex_columns(), rows);
            table.caption = "Retained source rows";
            body.push(&render(table)?);
            body.push(&render(AttachmentSlot)?);
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
                    FlexRow::new(id.to_string())
                        .slot("source", link)
                        .cell(
                            "period",
                            Cell::text(format!(
                                "{} – {}",
                                r["period_start"].as_str().unwrap_or(""),
                                r["period_end"].as_str().unwrap_or("")
                            )),
                        )
                        .cell(
                            "system",
                            Cell::text(r["source_system"].as_str().unwrap_or("")),
                        )
                })
                .collect();
            let mut table = FlexTable::new("source-periods", self.columns.flex_columns(), rows);
            table.caption = "Source-period schedules";
            table.event = "grid";
            body.push(&render(table)?);
            if let Some(next) = list["next"].as_i64() {
                let href = format!("{PATH}?kind={}&after={next}", self.kind);
                body.push(&render(Button {
                    href: &href,
                    body: Html::text("Next schedules"),
                    ..Button::default()
                })?);
            }
            body.push(&self.form(scope, company).await?);
        }
        if let Some(review) = &self.review {
            let mut content=render(Alert {body:Html::text(&format!("Review source {} for {} to {}. This retains evidence only and creates no postings or submissions.",review.facts["source_reference"].as_str().unwrap_or(""),review.facts["period_start"].as_str().unwrap_or(""),review.facts["period_end"].as_str().unwrap_or(""))),..Alert::default()})?;
            let fact = |title: &'static str, value: String| Fact {
                title,
                value: Html::text(&value),
                ..Fact::default()
            };
            let row = &review.facts["rows"][0];
            let units = review.facts["minor_units"]
                .as_i64()
                .and_then(bilibi_finance_ledger::MinorUnits::new)
                .ok_or(Error::Internal)?;
            let amount = row["amount"]
                .as_str()
                .and_then(|v| bilibi_finance_ledger::Money::parse(v, units).ok())
                .map(|v| v.display(units))
                .unwrap_or_default();
            let item = row["account_name"]
                .as_str()
                .map(|name| format!("{} · {}", name, row["account_code"].as_str().unwrap_or("")))
                .unwrap_or_else(|| row["reference"].as_str().unwrap_or("").to_owned());
            content.push(&render(Facts {
                id: "source-period-review-facts",
                items: vec![
                    fact(
                        "Source system",
                        review.facts["source_system"].as_str().unwrap_or("").into(),
                    ),
                    fact(
                        "Accounting time zone",
                        review.facts["accounting_timezone"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                    ),
                    fact(
                        "Conversion date",
                        review.facts["conversion_date"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                    ),
                    fact("Source row", row["source_id"].as_str().unwrap_or("").into()),
                    fact("Account or reference", item),
                    fact(
                        "Side or authority",
                        row["side"]
                            .as_str()
                            .or_else(|| row["authority"].as_str())
                            .unwrap_or("")
                            .into(),
                    ),
                    fact(
                        "Filing or remittance date",
                        row["event_date"].as_str().unwrap_or("").into(),
                    ),
                    fact(
                        "Amount",
                        format!(
                            "{} {}",
                            review.facts["currency_code"].as_str().unwrap_or(""),
                            amount
                        ),
                    ),
                ],
            })?);
            let mut buttons = render(Button {
                button_type: "button",
                event: "cancel_review",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                button_type: "button",
                event: "do_confirm",
                variant: ButtonVariant::Primary,
                body: Html::text("Retain schedule"),
                ..Button::default()
            })?);
            content.push(&render(Actions { buttons })?);
            body.push(&render(Modal {
                id: "source-period-review-modal",
                title: "Review source schedule",
                description: "Retain this source schedule for the current company.",
                cancel_event: "cancel_review",
                width: ModalWidth::Wide,
                body: content,
                ..Modal::default()
            })?);
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
    async fn form(&self, scope: &Scope, company: i64) -> Result<Html> {
        let book = self
            .read(
                scope,
                "finance.ledger.book.get",
                json!({"company_id":company}),
            )
            .await?;
        let currency = book["book"]["currency_code"].as_str().unwrap_or("");
        let mut fields = Html::EMPTY;
        for (key, label, kind, hint) in [
            (
                "source_reference",
                "Source reference",
                InputKind::Text,
                "A permanent reference for this schedule.",
            ),
            (
                "source_system",
                "Source system",
                InputKind::Text,
                "Name the system or archive.",
            ),
            (
                "accounting_timezone",
                "Accounting time zone",
                InputKind::Text,
                "For example, Asia/Kuala_Lumpur.",
            ),
            ("period_start", "Period starts", InputKind::Date, ""),
            ("period_end", "Period ends", InputKind::Date, ""),
            (
                "source_id",
                "Source row ID",
                InputKind::Text,
                "Identify the row in the source report.",
            ),
            (
                "amount",
                "Amount",
                InputKind::Text,
                "Use the book currency and decimal places.",
            ),
        ] {
            let field_path = if ["source_id", "amount"].contains(&key) {
                format!("rows.0.{key}")
            } else {
                key.into()
            };
            fields.push(&render(
                Field::in_form("schedule", key)
                    .label(label)
                    .kind(kind)
                    .value(self.values.get(key).cloned().unwrap_or_default())
                    .hint(hint)
                    .errors(self.errors.get(&field_path).cloned().unwrap_or_default())
                    .required(),
            )?);
        }
        if self.kind == "comparative" || self.kind == "movement" {
            let mut options = Vec::new();
            let mut input = json!({"company_id":company,"page_size":100});
            loop {
                let page = self
                    .read(scope, "finance.ledger.account.list", input.clone())
                    .await?;
                for account in page["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|a| a["is_group"] == false)
                {
                    options.push(
                        Choice::new(
                            account["name"].as_str().unwrap_or(""),
                            account["id"].to_string(),
                        )
                        .detail(account["code"].as_str().unwrap_or("")),
                    );
                }
                if page["next_after_code"].is_null() {
                    break;
                }
                input["after_code"] = page["next_after_code"].clone();
            }
            fields.push(&render(
                Combobox::new("schedule-account_id", "schedule[account_id]")
                    .label("Account *")
                    .aria_label("Account")
                    .options(options)
                    .value(self.values.get("account_id").cloned().unwrap_or_default())
                    .errors(
                        self.errors
                            .get("rows.0.account_id")
                            .cloned()
                            .unwrap_or_default(),
                    ),
            )?);
            fields.push(&render(
                Field::in_form("schedule", "side")
                    .label("Side")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("Debit", "debit"),
                        Choice::new("Credit", "credit"),
                    ])
                    .value(self.values.get("side").cloned().unwrap_or_default())
                    .errors(self.errors.get("rows.0.side").cloned().unwrap_or_default())
                    .required(),
            )?);
        } else {
            for (key, label, kind) in [
                ("authority", "Authority", InputKind::Text),
                (
                    "reference",
                    "Filing or remittance reference",
                    InputKind::Text,
                ),
                ("event_date", "Filing or remittance date", InputKind::Date),
            ] {
                fields.push(&render(
                    Field::in_form("schedule", key)
                        .label(label)
                        .kind(kind)
                        .value(self.values.get(key).cloned().unwrap_or_default())
                        .errors(
                            self.errors
                                .get(&format!("rows.0.{key}"))
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .required(),
                )?);
            }
        }
        fields.push(&render(Alert {
            id: "source-period-currency",
            body: Html::text(&format!(
                "Book currency: {currency}. This form retains one source row per schedule."
            )),
            ..Alert::default()
        })?);
        let actions = render(Button {
            button_type: "submit",
            event: "preview",
            variant: ButtonVariant::Primary,
            body: Html::text("Preview schedule"),
            ..Button::default()
        })?;
        render(Card {
            id: "source-period-create",
            body: render(Form {
                id: "source-period-form",
                event: "preview",
                class: "mt-4",
                body: fields,
                actions,
                ..Form::default()
            })?,
            ..Card::default()
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match value["type"].as_str().unwrap_or("") {
                    "preview" => {
                        self.review = None;
                        self.errors.clear();
                        self.message = None;
                        self.values = params::form_values_checked(value, "schedule", 1024)
                            .map_err(Error::refused)?;
                        let get = |key: &str| self.values.get(key).cloned().unwrap_or_default();
                        let company = Self::company(r.scope)?;
                        let book = self
                            .read(
                                r.scope,
                                "finance.ledger.book.get",
                                json!({"company_id":company}),
                            )
                            .await?;
                        let mut row = json!({"source_id":get("source_id"),"amount":get("amount")});
                        if self.kind == "comparative" || self.kind == "movement" {
                            row["account_id"] = get("account_id")
                                .parse::<i64>()
                                .ok()
                                .map(Value::from)
                                .unwrap_or(Value::Null);
                            row["side"] = json!(get("side"));
                        } else {
                            for key in ["authority", "reference", "event_date"] {
                                row[key] = json!(get(key));
                            }
                        }
                        let input = json!({"company_id":company,"kind":self.kind,"source_reference":get("source_reference"),"source_system":get("source_system"),"currency_code":book["book"]["currency_code"],"accounting_timezone":get("accounting_timezone"),"period_start":get("period_start"),"period_end":get("period_end"),"rows":[row]});
                        let call = Call {
                            scope: r.scope,
                            credential: r.credential,
                            service: PAGES,
                            event: "preview",
                            contract: IMPORT,
                        };
                        match self.pages.contracts.preview(&call,input.clone()).await {
                            Ok(answer) => { let result=&answer["results"][0]; if result["valid"]==true {self.review=Some(Review {input,facts:result["preview"]["result"]["changes"].clone()});} else {self.errors=serde_json::from_value(result["error"]["fields"].clone()).unwrap_or_default();self.message=Some("Check the fields and preview again.".into());} },
                            Err(_) => self.message=Some("The schedule could not be reviewed. Check your access and try again.".into()),
                        }
                    }
                    "do_confirm" => {
                        if let Some(review) = self.review.take() {
                            let call = Call {
                                scope: r.scope,
                                credential: r.credential,
                                service: PAGES,
                                event: "do_confirm",
                                contract: IMPORT,
                            };
                            match self.pages.contracts.commit(&call, review.input).await {
                                Ok(_) => {
                                    self.values.clear();
                                    self.message=Some("The source schedule was retained. Open it to attach its supporting report.".into());
                                }
                                Err(_) => {
                                    self.message =
                                        Some("The schedule changed. Preview it again.".into())
                                }
                            }
                        }
                    }
                    "cancel_review" => self.review = None,
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
