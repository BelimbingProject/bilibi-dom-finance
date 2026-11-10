//! Conversion reports have one home. Every read and reviewed write uses the
//! same operation as MCP; this page owns no accounting or approval logic.
use askama::Template;
use bilibi_agent_api::page::{Call, PageContracts};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::{PartyProofs, Scope};
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, BackLink, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice,
    CreateButton, EmptyState, Field, FlexRow, FlexTable, Form, Header, Html, InputKind,
    ListColumns, Modal, PageVariant, StatusKind, TableEmpty,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
pub(crate) const SERVICE: &str = "finance/reports.conversion-pages";
const PATH: &str = "/finance/reports/conversions";
const PREVIEW: &str = "finance.reports.conversion.preview";
const ISSUE: &str = "finance.reports.conversion.issue";
const SIGN: &str = "finance.reports.conversion.sign";
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<PageContracts>,
    pub parties: Arc<PartyProofs>,
}
struct Review {
    input: Value,
    contract: &'static str,
    instance: Value,
    attestation: Option<String>,
}
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as dialog %}{% call dialog::confirm_dialog(id="conversion-confirm-modal",consequence=consequence,detail=detail,confirm=confirm,working="Saving…",confirm_event=event,cancel_event="cancel_review",body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct Confirmation<'a> {
    consequence: &'a str,
    detail: &'a str,
    confirm: &'a str,
    event: &'a str,
    body: Html,
}
/// "+ Report": opens the reporting-date dialog.
fn new_report_button(id: &str) -> Result<Html> {
    render(CreateButton {
        noun: "Report",
        id,
        event: "new_report",
        button_type: "button",
        ..CreateButton::default()
    })
}
#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="font-medium text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
struct ReportLink<'a> {
    href: &'a str,
    label: &'a str,
}
/// A dialog's Cancel then its one primary verb, at the end of the row.
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons }}</div>"#,
    ext = "html"
)]
struct ActionRow {
    buttons: Html,
}
struct State {
    pages: Pages,
    id: Option<i64>,
    company: Option<i64>,
    values: BTreeMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
    message: String,
    document: Value,
    review: Option<Review>,
    confirm: bool,
    path: Vec<String>,
    offset: usize,
    columns: ListColumns,
    list_after: i64,
    list_columns: ListColumns,
    account_columns: ListColumns,
    /// The "+ Report" dialog (the reporting date to preview) is open.
    choosing: bool,
}
fn render(t: impl Template) -> Result<Html> {
    Html::render(&t).map_err(|_| Error::Internal)
}
fn label(s: &str) -> String {
    let text = s.replace('_', " ");
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
impl State {
    fn new(pages: Pages, r: &PageRequest<'_, Scope>) -> Self {
        Self {
            pages,
            id: r.path.get("report_id").and_then(|v| v.parse().ok()),
            company: None,
            values: BTreeMap::new(),
            errors: BTreeMap::new(),
            message: String::new(),
            document: Value::Null,
            review: None,
            confirm: false,
            path: Vec::new(),
            offset: 0,
            columns: ListColumns::mount(
                "conversion-facts",
                vec![
                    BuiltinColumn::new("name", "Item"),
                    BuiltinColumn::new("value", "Value"),
                    BuiltinColumn::new("open", "Details"),
                ],
            )
            .expect("conversion columns"),
            list_after: 0,
            choosing: false,
            list_columns: ListColumns::mount(
                "conversion-list",
                vec![
                    BuiltinColumn::new("report", "Report"),
                    BuiltinColumn::new("date", "Reporting date"),
                    BuiltinColumn::new("pin", "Through entry"),
                    BuiltinColumn::new("status", "Status"),
                ],
            )
            .expect("conversion list columns"),
            account_columns: ListColumns::mount(
                "conversion-accounts",
                vec![
                    BuiltinColumn::new("account", "Account"),
                    BuiltinColumn::new("source", "Source balance").right(),
                    BuiltinColumn::new("target", "Target balance").right(),
                    BuiltinColumn::new("movement", "Movement").right(),
                ],
            )
            .expect("conversion account columns"),
        }
    }
    fn reset_company(&mut self, company: Option<i64>) {
        self.company = company;
        self.values.clear();
        self.errors.clear();
        self.message.clear();
        self.document = Value::Null;
        self.review = None;
        self.confirm = false;
        self.path.clear();
        self.offset = 0;
        self.list_after = 0;
    }
    async fn read(&self, s: &Scope, key: &str, input: Value) -> std::result::Result<Value, String> {
        self.pages
            .contracts
            .read(s, key, &input)
            .await
            .map(|v| v["result"].clone())
            .map_err(|e| match e {
                // The owner's own words for what is wrong, never the code.
                bilibi_agent_api::Error::InvalidInput(fields) => {
                    fields.into_values().flatten().collect::<Vec<_>>().join(" ")
                }
                bilibi_agent_api::Error::Forbidden => {
                    "You do not have permission to do that.".into()
                }
                bilibi_agent_api::Error::NotFound => {
                    "That conversion report is not in the current company.".into()
                }
                _ => {
                    "The conversion report could not be read. Reload the page and try again.".into()
                }
            })
    }
    fn current(&self) -> &Value {
        self.path.iter().fold(&self.document, |v, k| match v {
            Value::Array(rows) => k
                .parse::<usize>()
                .ok()
                .and_then(|i| rows.get(i))
                .unwrap_or(&Value::Null),
            _ => &v[k],
        })
    }
    fn entries(&self) -> Vec<(String, Value)> {
        match self.current() {
            Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Value::Array(a) => a
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect(),
            v => vec![("value".into(), v.clone())],
        }
    }
    fn scalar(&self, key: &str, v: &Value) -> String {
        let text = match v {
            Value::Null => "None".into(),
            Value::Bool(true) => "Yes".into(),
            Value::Bool(false) => "No".into(),
            Value::String(s) => s.clone(),
            _ => v.to_string(),
        };
        if [
            "balance",
            "debit",
            "credit",
            "amount",
            "total",
            "difference",
            "cost",
            "accumulated",
            "carrying_value",
            "movement",
            "depreciation",
            "residual_value",
            "outstanding",
        ]
        .iter()
        .any(|k| key.contains(k))
        {
            let units = self
                .document
                .pointer("/report/content/minor_units")
                .or_else(|| self.document.get("minor_units"))
                .and_then(Value::as_u64)
                .and_then(|n| i64::try_from(n).ok())
                .and_then(bilibi_finance_ledger::MinorUnits::new);
            if let Some(units) = units {
                if let Ok(m) = bilibi_finance_ledger::Money::parse(&text, units) {
                    return m.display(units);
                }
            }
        }
        text
    }
    fn issued_list(&mut self) -> Result<Html> {
        let data = self.document["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        self.list_columns.load(&data, |r| r["id"].to_string());
        let mut rows = Vec::new();
        for r in &data {
            let id = r["id"].as_i64().ok_or(Error::Internal)?;
            rows.push(
                FlexRow::new(id.to_string())
                    .slot(
                        "report",
                        render(ReportLink {
                            href: &format!("{PATH}/{id}"),
                            label: &format!("Report {id}"),
                        })?,
                    )
                    .cell(
                        "date",
                        Cell::text(bilibi_finance_ledger::dates::display_text(
                            r["as_of"].as_str().unwrap_or(""),
                        )),
                    )
                    .cell("pin", Cell::text(r["through_entry_number"].to_string()))
                    .cell(
                        "status",
                        Cell::text(if r["signed"] == true {
                            "Signed"
                        } else {
                            "Issued; unsigned"
                        }),
                    ),
            );
        }
        let mut table = FlexTable::new("conversion-list", self.list_columns.flex_columns(), rows);
        table.caption = "Issued conversion reports";
        table.empty = Some(TableEmpty::State(
            EmptyState::missing("No conversion reports yet", "")
                .id("conversion-list-empty")
                .action(new_report_button("conversion-new-empty")?),
        ));
        table.zoom = self.list_columns.zoom;
        table.suggestions = self.list_columns.flex_suggestions();
        render(table)
    }
    fn account_table(&mut self) -> Result<Html> {
        let data = self.current().as_array().cloned().unwrap_or_default();
        self.offset = self.offset.min(data.len().saturating_sub(1) / 100 * 100);
        let selected = &data[self.offset..(self.offset + 100).min(data.len())];
        self.account_columns
            .load(selected, |r| r["account_id"].to_string());
        let rows = selected
            .iter()
            .map(|r| {
                FlexRow::new(r["account_id"].to_string())
                    .cell(
                        "account",
                        Cell::text(format!(
                            "{} · {}",
                            r["name"].as_str().unwrap_or(""),
                            r["code"].as_str().unwrap_or("")
                        )),
                    )
                    .cell(
                        "source",
                        Cell::text(self.scalar("balance", &r["source_balance"])),
                    )
                    .cell(
                        "target",
                        Cell::text(self.scalar("balance", &r["target_balance"])),
                    )
                    .cell(
                        "movement",
                        Cell::text(self.scalar("movement", &r["movement"])),
                    )
            })
            .collect();
        let mut table = FlexTable::new(
            "conversion-accounts",
            self.account_columns.flex_columns(),
            rows,
        );
        table.caption = "Source and target trial balance by account (debit less credit)";
        table.zoom = self.account_columns.zoom;
        table.suggestions = self.account_columns.flex_suggestions();
        let mut body = render(table)?;
        body.push(&render(Alert {id:"conversion-account-boundary",kind:StatusKind::Info,body:Html::text(&format!("{} accounts; showing {}–{}. Movement includes legitimate target activity and is not itself an unresolved difference.",data.len(),if data.is_empty(){0}else{self.offset+1},(self.offset+100).min(data.len()))),..Alert::default()})?);
        for (event, title, show) in [
            ("up", "Back to report", true),
            ("previous", "Previous accounts", self.offset > 0),
            ("next", "Next accounts", self.offset + 100 < data.len()),
        ] {
            if show {
                body.push(&render(Button {
                    event,
                    body: Html::text(title),
                    ..Button::default()
                })?);
            }
        }
        Ok(body)
    }
    fn table(&mut self) -> Result<Html> {
        let entries = self.entries();
        let end = (self.offset + 100).min(entries.len());
        self.offset = self.offset.min(entries.len().saturating_sub(1) / 100 * 100);
        let selected = &entries[self.offset..end.max(self.offset)];
        self.columns.load(selected, |r| r.0.clone());
        let mut rows = Vec::new();
        for (index, (key, v)) in selected.iter().enumerate() {
            let mut row = FlexRow::new(key.clone()).cell("name", Cell::text(label(key)));
            let value = match v {
                Value::Object(o) => format!("{} fields", o.len()),
                Value::Array(a) => format!("{} records", a.len()),
                _ => self.scalar(key, v),
            };
            row = row.cell("value", Cell::text(value));
            if v.is_object() || v.is_array() {
                row = row.slot(
                    "open",
                    render(Button {
                        event: "inspect",
                        name: "value",
                        value: &(self.offset + index).to_string(),
                        body: Html::text("Open"),
                        ..Button::default()
                    })?,
                );
            }
            rows.push(row);
        }
        let mut table = FlexTable::new("conversion-facts", self.columns.flex_columns(), rows);
        table.caption = "Conversion report facts";
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        let mut body = render(table)?;
        body.push(&render(Alert {
            id: "conversion-count",
            kind: StatusKind::Info,
            body: Html::text(&format!(
                "Records {}–{} of {}. {}",
                if entries.is_empty() {
                    0
                } else {
                    self.offset + 1
                },
                end,
                entries.len(),
                self.path
                    .iter()
                    .map(|k| label(k))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )),
            ..Alert::default()
        })?);
        for (event, title, show) in [
            ("up", "Back to previous section", !self.path.is_empty()),
            ("previous", "Previous records", self.offset > 0),
            ("next", "Next records", end < entries.len()),
        ] {
            if show {
                body.push(&render(Button {
                    event,
                    body: Html::text(title),
                    ..Button::default()
                })?);
            }
        }
        Ok(body)
    }
    async fn view(&mut self, s: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let company = s.current_company().map(|c| c.get());
        if self.company != company {
            self.reset_company(company);
        }
        let name = if let Some(company) = s.current_company() {
            let c = self.pages.db.get().await?;
            self.pages
                .parties
                .organisation_name(&**c, s, company)
                .await?
                .map(|r| r.name)
                .unwrap_or_else(|| "Company unavailable".into())
        } else {
            "No current company".into()
        };
        // The list starts a report with "+ Report"; a report or preview
        // leads back to the list from the header.
        let listing = self.id.is_none() && self.document.get("rows").is_some();
        let previewing = self.id.is_none() && !self.document.is_null() && !listing;
        body.push(&render(Header {
            title: Html::text("Conversion reports"),
            subtitle: &format!("Current company: {name}"),
            actions: if company.is_none() {
                Html::EMPTY
            } else if self.id.is_some() {
                render(BackLink {
                    href: PATH,
                    id: "conversion-back",
                    title: "Back to conversion reports",
                    ..BackLink::default()
                })?
            } else if previewing {
                render(Button {
                    id: "conversion-issued-list",
                    event: "issued_list",
                    button_type: "button",
                    body: Html::text("Issued reports"),
                    ..Button::default()
                })?
            } else {
                new_report_button("conversion-new")?
            },
            ..Header::default()
        })?);
        if let Some(company) = company {
            if let Some(id) = self.id {
                match self
                    .read(
                        s,
                        "finance.reports.conversion.get",
                        json!({"company_id":company,"report_id":id}),
                    )
                    .await
                {
                    Ok(v) => {
                        if self.review.is_none() {
                            self.document = v;
                        }
                    }
                    Err(e) => {
                        self.document = Value::Null;
                        self.review = None;
                        self.message = e;
                    }
                }
            } else if self.document.is_null() {
                match self
                    .read(
                        s,
                        "finance.reports.conversion.list",
                        json!({"company_id":company,"after":self.list_after}),
                    )
                    .await
                {
                    Ok(v) => self.document = v,
                    Err(e) => self.message = e,
                }
            }
            if self.id.is_none() && !self.document.is_null() {
                if let Err(e) = self
                    .read(
                        s,
                        "finance.reports.conversion.list",
                        json!({"company_id":company}),
                    )
                    .await
                {
                    self.document = Value::Null;
                    self.review = None;
                    self.message = e;
                }
            }
            // A refusal of the reporting date is said in its dialog.
            if !self.message.is_empty() && !self.choosing {
                body.push(&render(Alert {
                    id: "conversion-message",
                    kind: StatusKind::Warning,
                    body: Html::text(&self.message),
                    ..Alert::default()
                })?);
            }
            if !self.document.is_null() {
                if self.id.is_none() && self.document.get("rows").is_some() {
                    body.push(&self.issued_list()?);
                } else {
                    let content = if self.id.is_some() {
                        &self.document["report"]["content"]
                    } else {
                        &self.document
                    };
                    body.push(&render(Alert {id:"conversion-summary",kind:StatusKind::Info,body:Html::text(&format!("Conversion {} · reporting date {} · {} unresolved items. Review every difference, source and schedule limit.",bilibi_finance_ledger::dates::display_text(content["conversion_date"].as_str().unwrap_or("")),bilibi_finance_ledger::dates::display_text(content["as_of"].as_str().unwrap_or("")),content["unresolved_differences"].as_array().map_or(0,Vec::len))),..Alert::default()})?);
                    let stale_bridges = content["bank_bridges"].as_array().map_or(0, |rows| {
                        rows.iter()
                            .filter(|row| row["source_activity_after_finalisation"] == true)
                            .count()
                    });
                    if stale_bridges > 0 {
                        body.push(&render(Alert {
                            id: "conversion-bank-bridge-stale",
                            kind: StatusKind::Warning,
                            body: Html::text(&format!(
                                "{stale_bridges} finalised bank bridge(s) have later statement or match activity through this report date. Finalise a new reconciliation that includes it."
                            )),
                            ..Alert::default()
                        })?);
                    }
                    body.push(&render(Button {
                        event: "accounts",
                        body: Html::text("Source and target balances"),
                        ..Button::default()
                    })?);
                    if self.id.is_some() {
                        body.push(&render(Button {
                            event: "signoffs",
                            body: Html::text("Accountant sign-off"),
                            ..Button::default()
                        })?);
                    }
                    if matches!(self.path.as_slice(), [.., section, rows] if section == "source_trial_balance" && rows == "rows")
                    {
                        body.push(&self.account_table()?);
                    } else {
                        body.push(&self.table()?);
                    }
                }
            }
            if self.id.is_none() {
                if let Some(next) = self.document["next"].as_i64() {
                    body.push(&render(Button {
                        event: "more_reports",
                        name: "value",
                        value: &next.to_string(),
                        body: Html::text("More reports"),
                        ..Button::default()
                    })?);
                }
            }
            let signed = self.document["signoffs"]
                .as_array()
                .is_some_and(|s| !s.is_empty());
            if self.id.is_none() && self.choosing {
                let preview = render(Form {
                    id: "conversion-preview-form",
                    event: "preview",
                    class: "mt-4",
                    body: render(
                        Field::in_form("conversion", "as_of")
                            .id("conversion-preview-as-of")
                            .label("Reporting date")
                            .kind(InputKind::Date)
                            .required()
                            .value(self.values.get("as_of").map(String::as_str).unwrap_or(""))
                            .errors(self.errors.get("as_of").cloned().unwrap_or_default()),
                    )?,
                    actions: {
                        let mut buttons = render(Button {
                            button_type: "button",
                            event: "close_new",
                            body: Html::text("Cancel"),
                            ..Button::default()
                        })?;
                        buttons.push(&render(Button {
                            button_type: "submit",
                            variant: ButtonVariant::Primary,
                            body: Html::text("Preview report"),
                            ..Button::default()
                        })?);
                        render(ActionRow { buttons })?
                    },
                    ..Form::default()
                })?;
                let mut preview = preview;
                if !self.message.is_empty() {
                    let mut alert = render(Alert {
                        id: "conversion-new-refusal",
                        kind: StatusKind::Error,
                        body: Html::text(&self.message),
                        ..Alert::default()
                    })?;
                    alert.push(&preview);
                    preview = alert;
                }
                body.push(&render(Modal {
                    id: "conversion-new-modal",
                    title: "New conversion report",
                    description: &format!(
                        "Preview {name}'s conversion as of a reporting date, then review and issue it."
                    ),
                    cancel_event: "close_new",
                    body: preview,
                    ..Modal::default()
                })?);
            }
            // The issue form belongs to a previewed report, the sign-off form
            // to an issued one: never to the list.
            if !signed && (self.id.is_some() || previewing) {
                let mut fields = Html::EMPTY;
                for (key, title, kind) in [
                    (
                        "reviewer_reference",
                        "Reviewer and source references",
                        InputKind::Text,
                    ),
                    (
                        "note",
                        "Review conclusion and qualifications",
                        InputKind::Textarea,
                    ),
                ] {
                    fields.push(&render(
                        Field::in_form("conversion", key)
                            .id(format!("conversion-{key}"))
                            .label(title)
                            .kind(kind)
                            .required()
                            .value(self.values.get(key).map(String::as_str).unwrap_or(""))
                            .errors(self.errors.get(key).cloned().unwrap_or_default()),
                    )?);
                }
                if self.id.is_none() {
                    for area in ["bank", "tax", "payroll"] {
                        let status = format!("{area}_status");
                        let reference = format!("{area}_reference");
                        fields.push(&render(
                            Field::in_form("conversion", &status)
                                .label(format!("{} schedule", label(area)))
                                .kind(InputKind::Select)
                                .value(
                                    self.values
                                        .get(&status)
                                        .map(String::as_str)
                                        .unwrap_or("not_assessed"),
                                )
                                .options(vec![
                                    Choice::new("Not assessed", "not_assessed"),
                                    Choice::new("Applicable", "applicable"),
                                    Choice::new("Not applicable", "not_applicable"),
                                ])
                                .errors(self.errors.get(&status).cloned().unwrap_or_default()),
                        )?);
                        fields.push(&render(
                            Field::in_form("conversion", &reference)
                                .label(format!("{} schedule reference or reason", label(area)))
                                .value(
                                    self.values
                                        .get(&reference)
                                        .map(String::as_str)
                                        .unwrap_or(""),
                                )
                                .errors(self.errors.get(&reference).cloned().unwrap_or_default()),
                        )?);
                    }
                }
                let mut actions = Html::EMPTY;
                actions.push(&render(Button {
                    button_type: "submit",
                    body: Html::text(if self.id.is_some() {
                        "Review sign-off"
                    } else {
                        "Review and issue"
                    }),
                    variant: ButtonVariant::Primary,
                    ..Button::default()
                })?);
                fields.push(&render(Alert {id:"conversion-significance",kind:StatusKind::Info,body:Html::text(if self.id.is_some(){"Sign the exact issued report after reviewing its sources, differences and excluded history. Explain any qualifications in your conclusion."}else{"Once issued, this report cannot be changed. Issuing does not sign it."}),..Alert::default()})?);
                body.push(&render(Card {
                    id: "conversion-form-card",
                    body: render(Form {
                        id: "conversion-form",
                        event: if self.id.is_some() {
                            "review_sign"
                        } else {
                            "review_issue"
                        },
                        body: fields,
                        actions,
                        ..Form::default()
                    })?,
                    ..Card::default()
                })?);
            }
            if self.review.is_some() {
                body.push(&render(Button {
                    event: "open_review",
                    body: Html::text("Continue to confirmation"),
                    variant: ButtonVariant::Primary,
                    ..Button::default()
                })?);
            }
            if let Some(review) = self.review.as_ref().filter(|_| self.confirm) {
                let instance = &review.instance;
                let mut confirmation = render(bilibi_ui::InstanceBanner {
                    id: "conversion-instance",
                    tone: StatusKind::Info,
                    role: instance["environment"].as_str().unwrap_or(""),
                    label: instance["label"].as_str().unwrap_or(""),
                    message: instance["notice"].as_str().unwrap_or(""),
                    ..bilibi_ui::InstanceBanner::default()
                })?;
                if let Some(attestation) = &review.attestation {
                    confirmation.push(&render(Alert {
                        id: "conversion-sign-attestation",
                        kind: StatusKind::Info,
                        body: Html::text(attestation),
                        ..Alert::default()
                    })?);
                }
                body.push(&render(Confirmation {consequence:if review.contract==ISSUE {"Issue this conversion report?"}else{"Sign this issued conversion report?"},detail:&format!("Current company: {name}. Review the report's sources, differences and limits before confirming. This record cannot be changed."),confirm:if review.contract==ISSUE {"Issue"}else{"Sign"},event:if review.contract==ISSUE {"issue"}else{"sign"},body:confirmation})?);
            }
        } else {
            body.push(&render(Alert {
                id: "conversion-no-company",
                kind: StatusKind::Info,
                body: Html::text("Choose a current company in the top bar."),
                ..Alert::default()
            })?);
        }
        Ok(LiveBody {
            html: render(bilibi_ui::Page {
                id: "conversion-page",
                variant: PageVariant::Detail,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let company = r.scope.current_company().map(|c| c.get());
            if self.company != company {
                self.reset_company(company);
                return self.view(r.scope).await;
            }
            if let LiveInput::Event(v) = r.input {
                let event = v["type"].as_str().ok_or(Error::Invalid)?;
                match event {
                    "inspect" => {
                        if let Some((key, _)) = v["value"]
                            .as_str()
                            .and_then(|s| s.parse::<usize>().ok())
                            .and_then(|i| self.entries().get(i).cloned())
                        {
                            self.path.push(key);
                            self.offset = 0;
                        }
                    }
                    "up" => {
                        self.path.pop();
                        self.offset = 0;
                    }
                    "previous" => self.offset = self.offset.saturating_sub(100),
                    "next" => self.offset = self.offset.saturating_add(100),
                    "grid" => {
                        let values = params::string_values(v);
                        self.columns.handle(&values);
                        self.list_columns.handle(&values);
                        self.account_columns.handle(&values);
                    }
                    "accounts" => {
                        self.path = if self.id.is_some() {
                            vec![
                                "report".into(),
                                "content".into(),
                                "source_trial_balance".into(),
                                "rows".into(),
                            ]
                        } else {
                            vec!["source_trial_balance".into(), "rows".into()]
                        };
                        self.offset = 0;
                    }
                    "signoffs" => {
                        self.path = vec!["signoffs".into()];
                        self.offset = 0;
                    }
                    "cancel_review" => self.confirm = false,
                    "open_review" => self.confirm = true,
                    "new_report" => {
                        self.choosing = true;
                        self.message.clear();
                    }
                    "close_new" => {
                        self.choosing = false;
                        self.message.clear();
                    }
                    "issued_list" => {
                        self.document = Value::Null;
                        self.path.clear();
                        self.offset = 0;
                        self.list_after = 0;
                    }
                    "more_reports" => {
                        self.list_after = v["value"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        self.document = Value::Null;
                        self.path.clear();
                        self.offset = 0;
                    }
                    "preview" | "review_issue" | "review_sign" => {
                        self.errors.clear();
                        self.message.clear();
                        self.review = None;
                        self.confirm = false;
                        for key in [
                            "as_of",
                            "reviewer_reference",
                            "note",
                            "bank_status",
                            "bank_reference",
                            "tax_status",
                            "tax_reference",
                            "payroll_status",
                            "payroll_reference",
                        ] {
                            if let Some(text) = v[format!("conversion[{key}]")].as_str() {
                                self.values.insert(key.into(), text.into());
                            }
                        }
                        if event == "preview" {
                            let input = json!({
                                "company_id": r.scope.current_company().map(|c| c.get()),
                                "as_of": self.values.get("as_of").cloned().unwrap_or_default(),
                            });
                            match self.read(r.scope, PREVIEW, input).await {
                                Ok(document) => {
                                    self.choosing = false;
                                    self.document = document;
                                    self.path.clear();
                                    self.offset = 0;
                                }
                                Err(error) => self.message = error,
                            }
                        } else {
                            let mut input = json!(self.values);
                            input["company_id"] = json!(r.scope.current_company().map(|c| c.get()));
                            if let Some(id) = self.id {
                                input["report_id"] = json!(id);
                            }
                            let contract = if event == "review_issue" { ISSUE } else { SIGN };
                            let call = Call {
                                scope: r.scope,
                                credential: r.credential,
                                service: SERVICE,
                                event,
                                contract,
                            };
                            match self.pages.contracts.preview(&call, input.clone()).await {
                                Ok(review) => {
                                    if review["valid"] == true {
                                        let result = &review["results"][0]["preview"]["result"];
                                        let attestation = if contract == SIGN {
                                            Some(
                                                result["attestation"]
                                                    .as_str()
                                                    .ok_or(Error::Internal)?
                                                    .to_owned(),
                                            )
                                        } else {
                                            None
                                        };
                                        self.document = if contract == ISSUE {
                                            result.clone()
                                        } else {
                                            result["record"].clone()
                                        };
                                        self.path.clear();
                                        self.offset = 0;
                                        self.review = Some(Review {
                                            input,
                                            contract,
                                            instance: review["instance"].clone(),
                                            attestation,
                                        });
                                    } else {
                                        let fields = &review["results"][0]["error"]["fields"];
                                        self.errors = serde_json::from_value(fields.clone())
                                            .unwrap_or_default();
                                        self.message=self.errors.get("base").map(|v|v.join(" ")).unwrap_or_else(||"The report could not be reviewed. Check the highlighted fields and your permissions.".into());
                                    }
                                }
                                Err(e) => self.message = e.to_string(),
                            }
                        }
                    }
                    "issue" | "sign" => {
                        if let Some(review) = self.review.take() {
                            let call = Call {
                                scope: r.scope,
                                credential: r.credential,
                                service: SERVICE,
                                event,
                                contract: review.contract,
                            };
                            match self.pages.contracts.commit(&call, review.input).await {
                                Ok(done) => {
                                    let result = &done["results"][0]["result"]["result"];
                                    if review.contract == ISSUE {
                                        if let Some(id) = result["id"].as_i64() {
                                            return Ok(LiveBody {
                                                redirect: Some(format!("{PATH}/{id}")),
                                                ..LiveBody::default()
                                            });
                                        }
                                    }
                                    self.message = "Accountant sign-off recorded.".into();
                                    self.document = Value::Null;
                                    self.path.clear();
                                }
                                Err(e) => self.message = e.to_string(),
                            }
                        }
                    }
                    "clear-flash" => self.message.clear(),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _: &str) -> Option<&'static str> {
        Some("Conversion reports")
    }
    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = State::new(self.clone(), &r);
            let body = state.view(r.scope.ok_or(Error::Unauthorized)?).await?;
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
                Box::new(State::new(self.clone(), &r)) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}
