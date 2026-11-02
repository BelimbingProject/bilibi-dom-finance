//! Reviewed opening action on the record list. Bilimbi has no opening-import UI; shared Base
//! components render it and PageContracts supplies the same write as MCP.
use crate::*;
use askama::Template;
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_ui::{
    page::{LiveInput, LiveRequest},
    params, Alert, BuiltinColumn, Button, ButtonVariant, Cell, ErrorSummary, Field, FlashGroup,
    FlexRow, FlexTable, Form, Html, InputKind, ListColumns, Modal, ModalWidth, StatusKind,
};

use super::{Pages, PAGES};
use serde_json::{json, Value};
use std::collections::BTreeMap;
const CONTRACT: &str = "finance.fixed_assets.opening.import";
const STAGE: &str = "finance.fixed_assets.opening.stage";
const CONTEXT: &str = "finance.fixed_assets.opening.context";
const TITLE: &str = "Opening asset register";
fn field_label(key: &str) -> &str {
    match key {
        "part" => "Batch number",
        "part_count" => "Batch count",
        "replaces_batch_id" => "Register to replace",
        "reason" => "Correction reason",
        "csv" => "Source register CSV",
        "source_evidence" => "Source history and tax schedules",
        "series_id" => "Opening invoice series",
        "source_reference" => "Source reference",
        "currency_code" => "Book currency",
        "override_reason" => "Approval exception reason",
        _ => key,
    }
}

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
struct Review {
    contract: &'static str,
    input: Value,
    facts: Value,
}
pub(super) struct Opening {
    pub(super) completed: bool,
    pages: Pages,
    values: Value,
    fields: BTreeMap<String, Vec<String>>,
    message: Option<(StatusKind, String)>,
    review: Option<Review>,
    columns: ListColumns,
    balance_columns: ListColumns,
    stage_columns: ListColumns,
    class_columns: ListColumns,
    evidence: super::source_evidence::EvidenceView,
}
impl Opening {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            completed: false,
            evidence: Default::default(),
            pages,
            values: json!({}),
            fields: BTreeMap::new(),
            message: None,
            review: None,
            class_columns: ListColumns::mount(
                "conversion-classes",
                vec![
                    BuiltinColumn::new("class", "Category"),
                    BuiltinColumn::new("cost_account", "Cost account"),
                    BuiltinColumn::new("accumulated_account", "Accumulation account"),
                    BuiltinColumn::new("cost", "Source cost").right(),
                    BuiltinColumn::new("accumulated", "Source accumulation").right(),
                    BuiltinColumn::new("carrying", "Carrying value").right(),
                ],
            )
            .map_err(|_| Error::Internal)?,
            stage_columns: ListColumns::mount(
                "conversion-stages",
                vec![
                    BuiltinColumn::new("source", "Source reference"),
                    BuiltinColumn::new("batch", "Batch"),
                    BuiltinColumn::new("state", "State"),
                ],
            )
            .map_err(|_| Error::Internal)?,
            balance_columns: ListColumns::mount(
                "conversion-balances",
                vec![
                    BuiltinColumn::new("account", "Account"),
                    BuiltinColumn::new("register", "Register balance").right(),
                    BuiltinColumn::new("opening", "Opening balance").right(),
                    BuiltinColumn::new("difference", "Difference").right(),
                ],
            )
            .map_err(|_| Error::Internal)?,
            columns: ListColumns::mount(
                "conversion-rows",
                vec![
                    BuiltinColumn::new("source", "Source ID"),
                    BuiltinColumn::new("record", "Record"),
                    BuiltinColumn::new("date", "Original date"),
                    BuiltinColumn::new("original", "Source cost").right(),
                    BuiltinColumn::new("balance", "Carrying value").right(),
                    BuiltinColumn::new("accumulated", "Source accumulation").right(),
                    BuiltinColumn::new("remaining", "Remaining months").right(),
                    BuiltinColumn::new("residual", "Residual value").right(),
                    BuiltinColumn::new("service", "In-service date"),
                    BuiltinColumn::new("cost_account", "Cost account"),
                    BuiltinColumn::new("accumulated_account", "Accumulation account"),
                ],
            )
            .map_err(|_| Error::Internal)?,
        })
    }
    fn errors(&self, key: &str) -> Vec<String> {
        self.fields
            .iter()
            .filter(|(path, _)| path.as_str() == key || path.starts_with(&format!("{key}.")))
            .flat_map(|(path, messages)| {
                messages.iter().map(move |message| {
                    let detail = if path == key {
                        message.clone()
                    } else {
                        format!("{path}: {message}")
                    };
                    bilibi_ui::error_sentence(field_label(key), &detail)
                })
            })
            .collect()
    }
    fn fail(&mut self, error: ContractError) {
        match error {
            ContractError::InvalidInput(fields) => self.fields = fields,
            ContractError::Forbidden => {
                self.message = Some((
                    StatusKind::Error,
                    "You do not have permission to import opening records in the current company."
                        .into(),
                ))
            }
            ContractError::Backend(Error::Conflict) => {
                self.message = Some((
                    StatusKind::Error,
                    "The reviewed figures changed. Preview the source again.".into(),
                ))
            }
            e => {
                self.message = Some((StatusKind::Error, {
                    let _ = e;
                    "The import could not be completed. Preview the source again or contact an administrator.".into()
                }))
            }
        }
    }
    pub(super) async fn body(&mut self, scope: &Scope) -> Result<Html> {
        let mut page = Html::EMPTY;
        let company = scope.current_company().ok_or(Error::Invalid)?.get();
        let context = match self
            .pages
            .contracts
            .read(scope, CONTEXT, &json!({"company_id":company}))
            .await
        {
            Ok(value) => value["result"].clone(),
            Err(e) => {
                self.fields.clear();
                self.fail(e);
                if !self.fields.is_empty() {
                    self.message = Some((
                        StatusKind::Error,
                        self.fields
                            .values()
                            .flatten()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" "),
                    ));
                }
                Value::Null
            }
        };
        let mut flash = bilibi_ui::NO_FLASH;
        if let Some((kind, message)) = &self.message {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        page.push(&render(FlashGroup {
            id: "conversion-flash",
            flash,
        })?);
        if !context.is_null() {
            let c = self.pages.db.get().await?;
            let allowed = bilibi_authz::decide(&**c, scope, ACQUIRE).await?.allowed;
            drop(c);
            let currency = context["book"]["currency_code"].as_str().unwrap_or("");
            let conversion = context["opening"]["entry"]["entry_date"]
                .as_str()
                .unwrap_or("");
            page.push(&render(Alert {
                id: "conversion-context",
                body: Html::text(&format!(
                    "Conversion date {conversion}. Currency {currency}. Opening entry {}.",
                    context["opening"]["entry"]["entry_number"]
                )),
                ..Alert::default()
            })?);
            if !context["batch"].is_null() {
                page.push(&render(Alert{id:"conversion-recorded",kind:StatusKind::Info,body:Html::text(&format!("Reconciled register {}. The source evidence is retained. To correct it, stage a new source export naming this register ID and request independent approval.",context["batch"]["id"])),..Alert::default()})?);
            }
            let stage_rows = context["stages"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|stage| {
                    let reference = stage["source_reference"].as_str().unwrap_or("");
                    let reconciled = context["decisions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|d| d["review"]["source_reference"] == reference);
                    FlexRow::new(stage["id"].to_string())
                        .cell("source", Cell::text(reference))
                        .cell(
                            "batch",
                            Cell::text(format!("{} / {}", stage["part"], stage["part_count"])),
                        )
                        .cell(
                            "state",
                            Cell::text(if reconciled {
                                "Reconciled"
                            } else {
                                "Committed source batch"
                            }),
                        )
                })
                .collect::<Vec<_>>();
            if !stage_rows.is_empty() {
                let mut table = FlexTable::new(
                    "conversion-stages",
                    self.stage_columns.flex_columns(),
                    stage_rows,
                );
                table.event = "stage_grid";
                table.caption = "Retained source batches";
                table.zoom = self.stage_columns.zoom;
                table.suggestions = self.stage_columns.flex_suggestions();
                table.add_query = &self.stage_columns.add_query;
                page.push(&render(table)?);
            }
            if allowed {
                if self.values["currency_code"].is_null() {
                    self.values["currency_code"] = json!(currency);
                }
                let mut fields = render(Alert {
                    id: "conversion-boundary",
                    kind: StatusKind::Info,
                    body: Html::text(crate::opening::CONVENTION),
                    ..Alert::default()
                })?;
                let mut summary = ErrorSummary::new("conversion-errors");
                for (key, messages) in &self.fields {
                    for message in messages {
                        summary.push(
                            format!("conversion-{}", key.split('.').next().unwrap_or(key)),
                            bilibi_ui::error_sentence(
                                field_label(key.split('.').next().unwrap_or(key)),
                                message,
                            ),
                        );
                    }
                }
                fields.push(&render(summary)?);
                fields.push(&render(
                    Field::in_form("conversion", "mode")
                        .id("conversion-mode")
                        .label("Action")
                        .kind(InputKind::Select)
                        .options(vec![
                            bilibi_ui::Choice::new("Import complete register", "direct"),
                            bilibi_ui::Choice::new("Stage source batch", "stage"),
                            bilibi_ui::Choice::new("Reconcile staged register", "reconcile"),
                        ])
                        .value(self.values["mode"].as_str().unwrap_or("direct")),
                )?);
                let staged = context["stages"].as_array().map_or(0, Vec::len);
                fields.push(&render(Alert{id:"conversion-staged",kind:StatusKind::Info,body:Html::text(&format!("{staged} source batches retained. Stage each numbered batch under the same source reference and batch count. Reconcile with CSV empty after every batch is present. Staging commits source evidence; reconciliation creates the opening records.")),..Alert::default()})?);
                for (key, label, kind, required, hint) in [
                    (
                        "source_reference",
                        "Source reference",
                        InputKind::Text,
                        true,
                        "A permanent identifier for this complete source export; preserve it when retrying.",
                    ),
                    (
                        "currency_code",
                        "Book currency",
                        InputKind::Text,
                        true,
                        "Amounts must use the book's currency and decimal places.",
                    ),
                    (
                        "csv",
                        "Source register CSV",
                        InputKind::Textarea,
                        false,
                        "Paste the complete register: 1 to 500 rows, at most 128 KiB.",
                    ),
                    (
                        "source_evidence",
                        "Source history and tax schedules (JSON)",
                        InputKind::Textarea,
                        false,
                        "Optional evidence array; each record names source_id: history, source_period_expense and tax_capital_allowances. See the opening guide. These facts create no postings and do not change the prospective estimate.",
                    ),
                    (
                        "override_reason",
                        "Approval exception reason",
                        InputKind::Text,
                        false,
                        "Required only when overriding separation of duties with the appropriate authority.",
                    ),
                    (
                        "part",
                        "Batch number",
                        InputKind::Number,
                        false,
                        "For staging: sequential batch number, starting at 1.",
                    ),
                    (
                        "part_count",
                        "Batch count",
                        InputKind::Number,
                        false,
                        "For staging: total batches in this export, at most 200.",
                    ),
                    (
                        "replaces_batch_id",
                        "Register to replace",
                        InputKind::Number,
                        false,
                        "For a correction: the current reconciled register ID. Keep the same ID for all source batches.",
                    ),
                    (
                        "reason",
                        "Correction reason",
                        InputKind::Text,
                        false,
                        "Required for a correction. Preserve the prior export and explain the replacement.",
                    ),
                ] {
                    let mut field = Field::in_form("conversion", key)
                        .id(format!("conversion-{key}"))
                        .label(label)
                        .kind(kind)
                        .value(self.values[key].as_str().unwrap_or(""))
                        .errors(self.errors(key))
                        .hint(hint);
                    if required {
                        field = field.required();
                    }
                    if key == "csv" {
                        field = field.attr("rows", "12");
                    }
                    if key == "currency_code" {
                        field = field.readonly();
                    }
                    fields.push(&render(field)?);
                }

                fields.push(&render(Alert {
                    id: "conversion-header",
                    body: Html::text(&format!(
                        "CSV header: {}",
                        context["header"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    )),
                    ..Alert::default()
                })?);
                let mut buttons = render(Button {
                    button_type: "button",
                    event: "close_opening",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?;
                buttons.push(&render(Button {
                    id: "conversion-preview",
                    button_type: "submit",
                    body: Html::text("Preview action"),
                    variant: ButtonVariant::Primary,
                    working: "Checking…",
                    ..Button::default()
                })?);
                page.push(&render(Form {
                    id: "conversion-form",
                    event: "preview_opening",
                    class: "mt-4",
                    body: fields,
                    actions: render(Actions { buttons })?,
                    ..Form::default()
                })?);
            }
        }
        if let Some(review) = &self.review {
            let facts = &review.facts;
            let units = facts["minor_units"]
                .as_i64()
                .or_else(|| context["book"]["minor_units"].as_i64())
                .and_then(bilibi_finance_ledger::MinorUnits::new)
                .ok_or(Error::Internal)?;
            let money = |v: &Value| {
                v.as_str()
                    .and_then(|s| bilibi_finance_ledger::Money::parse(s, units).ok())
                    .map(|m| m.display(units))
                    .unwrap_or_default()
            };
            let mut rows = vec![];
            for r in facts["rows"].as_array().into_iter().flatten() {
                let text = |key: &str| r[key].as_str().unwrap_or("");
                rows.push(
                    FlexRow::new(text("source_id"))
                        .cell("source", Cell::text(text("source_id")))
                        .cell("record", Cell::text(text("description")))
                        .cell("date", Cell::text(text("acquisition_date")))
                        .cell("original", Cell::text(money(&r["cost"])))
                        .cell("balance", Cell::text(money(&r["carrying_value"])))
                        .cell(
                            "accumulated",
                            Cell::text(money(&r["accumulated_depreciation"])),
                        )
                        .cell(
                            "remaining",
                            Cell::text(r["remaining_life_months"].to_string()),
                        )
                        .cell("residual", Cell::text(money(&r["residual_value"])))
                        .cell("service", Cell::text(text("in_service_date")))
                        .cell("cost_account", Cell::text(text("cost_account_id_code")))
                        .cell(
                            "accumulated_account",
                            Cell::text(text("accumulated_account_id_code")),
                        ),
                );
            }
            let mut table = FlexTable::new("conversion-rows", self.columns.flex_columns(), rows);
            table.event = "opening_grid";
            table.zoom = self.columns.zoom;
            table.suggestions = self.columns.flex_suggestions();
            table.add_query = &self.columns.add_query;
            let mut body = self
                .pages
                .plans
                .instance()
                .await?
                .banner("conversion-instance", "")
                .map_err(|_| Error::Internal)?;
            body.push(&render(Alert{id:"conversion-review-context",body:Html::text(&format!("Conversion date {}. Source {}. {} records. Review every source identifier, date, amount and account mapping before approval.",facts["conversion_date"].as_str().or_else(||context["opening"]["entry"]["entry_date"].as_str()).unwrap_or(""),facts["source_reference"].as_str().unwrap_or(""),facts["rows"].as_array().map_or_else(||facts["rows"].as_u64().unwrap_or(0) as usize,Vec::len))),..Alert::default()})?);
            if !facts["replaces_batch_id"].is_null() {
                body.push(&render(Alert{id:"conversion-correction-review",kind:StatusKind::Warning,body:Html::text(&format!("Replace register {} as of the conversion date {}. Reason: {}. Prior records and approvals remain available at their original report boundaries.",facts["replaces_batch_id"],facts["conversion_date"].as_str().unwrap_or(""),facts["reason"].as_str().unwrap_or(""))),..Alert::default()})?);
            }
            if let Some(classes) = facts["class_totals"].as_array() {
                let rows = classes
                    .iter()
                    .map(|row| {
                        FlexRow::new(format!(
                            "{}-{}-{}",
                            row["category_id"],
                            row["cost_account_id"],
                            row["accumulated_account_id"]
                        ))
                        .cell(
                            "class",
                            Cell::text(row["category_name"].as_str().unwrap_or("")),
                        )
                        .cell(
                            "cost_account",
                            Cell::text(row["cost_account"].as_str().unwrap_or("")),
                        )
                        .cell(
                            "accumulated_account",
                            Cell::text(row["accumulated_account"].as_str().unwrap_or("")),
                        )
                        .cell("cost", Cell::text(money(&row["cost"])))
                        .cell(
                            "accumulated",
                            Cell::text(money(&row["accumulated_depreciation"])),
                        )
                        .cell("carrying", Cell::text(money(&row["carrying_value"])))
                    })
                    .collect();
                let mut table = FlexTable::new(
                    "conversion-classes",
                    self.class_columns.flex_columns(),
                    rows,
                );
                table.caption = "Source totals by class and account pair";
                table.event = "class_grid";
                table.zoom = self.class_columns.zoom;
                table.suggestions = self.class_columns.flex_suggestions();
                table.add_query = &self.class_columns.add_query;
                body.push(&render(table)?);
            }
            if let Some(reconciliation) = facts["reconciliation"].as_array() {
                let rows = reconciliation
                    .iter()
                    .map(|row| {
                        FlexRow::new(row["account_id"].to_string())
                            .cell(
                                "account",
                                Cell::text(row["account_code"].as_str().unwrap_or("")),
                            )
                            .cell("register", Cell::text(money(&row["register_balance"])))
                            .cell("opening", Cell::text(money(&row["opening_balance"])))
                            .cell("difference", Cell::text(money(&row["difference"])))
                    })
                    .collect();
                let mut balances = FlexTable::new(
                    "conversion-balances",
                    self.balance_columns.flex_columns(),
                    rows,
                );
                balances.event = "balance_grid";
                balances.caption = "Opening reconciliation; balances are debit-positive";
                balances.zoom = self.balance_columns.zoom;
                balances.suggestions = self.balance_columns.flex_suggestions();
                balances.add_query = &self.balance_columns.add_query;
                body.push(&render(balances)?);
            } else if !facts["total"].is_null() {
                body.push(&render(bilibi_ui::Facts {
                    id: "conversion-totals",
                    items: vec![
                        bilibi_ui::Fact {
                            title: "Register outstanding",
                            value: Html::text(&money(&facts["total"])),
                            ..Default::default()
                        },
                        bilibi_ui::Fact {
                            title: "Opening control balance",
                            value: Html::text(&money(&facts["opening_balance"])),
                            ..Default::default()
                        },
                        bilibi_ui::Fact {
                            title: "Difference",
                            value: Html::text(&money(&facts["difference"])),
                            ..Default::default()
                        },
                    ],
                })?);
            }
            if review.contract != STAGE {
                body.push(&render(table)?);
            }
            if let Some(evidence) = facts["source_evidence"].as_object() {
                for (source, facts) in evidence {
                    body.push(&self.evidence.render(source, facts, units)?);
                }
            }
            if review.contract != STAGE {
                body.push(&render(Alert{id:"conversion-review-reconciliation",kind:StatusKind::Info,body:Html::text("The complete register reconciles exactly to the approved opening account balances. This import records opening state and retains source evidence; it creates no new ledger entry."),..Alert::default()})?);
            } else {
                body.push(&render(Alert{id:"conversion-stage-review",body:Html::text(&format!("Previewed source batch {} of {}, containing {} rows. Committing this batch retains source evidence. It does not create opening records or approve reconciliation.",facts["part"],facts["part_count"],facts["rows"])),..Alert::default()})?);
            }
            if let Some(convention) = facts["month_convention"].as_str() {
                body.push(&render(Alert {
                    id: "conversion-month-convention",
                    body: Html::text(convention),
                    ..Alert::default()
                })?);
            }
            if let Some(policy) = facts["depreciation"].as_str() {
                body.push(&render(Alert {
                    id: "conversion-depreciation",
                    body: Html::text(policy),
                    ..Alert::default()
                })?);
            }
            body.push(&render(Alert{id:"conversion-final",kind:StatusKind::Warning,body:Html::text("Source batches and reconciled registers are permanent evidence. A correction retains the prior register and requires dependent activity to be reversed first. The opening journal remains protected against reversal."),..Alert::default()})?);
            let mut buttons = render(Button {
                button_type: "button",
                event: "cancel_opening",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                id: "conversion-import",
                button_type: "button",
                event: "import_opening",
                body: Html::text(if review.contract == STAGE {
                    "Commit source batch"
                } else {
                    "Approve reconciliation"
                }),
                variant: ButtonVariant::Primary,
                working: "Importing…",
                ..Button::default()
            })?);
            body.push(&render(Actions { buttons })?);
            page = render(Modal {
                id: "conversion-review-modal",
                title: if review.contract == STAGE {
                    "Review source batch"
                } else {
                    "Review opening import"
                },
                description: if review.contract == STAGE {
                    "Commit this source batch for later reconciliation."
                } else {
                    "Approve the reconciled opening register for the current company."
                },
                cancel_event: "cancel_opening",
                width: ModalWidth::Wide,
                body,
                ..Modal::default()
            })?;
        } else {
            page = render(Modal {
                id: "conversion-modal", title: TITLE,
                description: "Reconcile the source register with the current company's approved opening balances.",
                cancel_event: "close_opening", width: ModalWidth::Wide, body: page, ..Modal::default()
            })?;
        }
        Ok(page)
    }
}
impl Opening {
    pub(super) async fn update(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        if let LiveInput::Event(value) = r.input {
            match value["type"].as_str().unwrap_or("") {
                "preview_opening" => {
                    self.review = None;
                    self.fields.clear();
                    let fields = params::form_values_checked(
                        value,
                        "conversion",
                        crate::opening_csv::MAX_BYTES,
                    )
                    .map_err(Error::refused)?;
                    self.values = json!(fields);
                    let mut input = self.values.clone();
                    input["company_id"] = r
                        .scope
                        .current_company()
                        .map(|id| json!(id.get()))
                        .unwrap_or(Value::Null);

                    if input["override_reason"] == "" {
                        input
                            .as_object_mut()
                            .expect("form object")
                            .remove("override_reason");
                    }
                    let source = input["source_evidence"].as_str().unwrap_or("").trim();
                    input["source_evidence"] = if source.is_empty() {
                        json!([])
                    } else {
                        match serde_json::from_str::<Value>(source) {
                            Ok(value) => value,
                            Err(_) => {
                                self.fields.insert(
                                    "source_evidence".into(),
                                    vec!["must be a valid JSON array; each record names source_id"
                                        .into()],
                                );
                                return Ok(());
                            }
                        }
                    };
                    let contract = if self.values["mode"] == "stage" {
                        STAGE
                    } else {
                        CONTRACT
                    };
                    let staged = self.values["mode"] == "reconcile";
                    if let Some(object) = input.as_object_mut() {
                        object.remove("mode");
                        for key in ["part", "part_count", "replaces_batch_id"] {
                            let value = object
                                .get(key)
                                .and_then(Value::as_str)
                                .and_then(|text| text.parse::<i64>().ok());
                            object.remove(key);
                            if let Some(value) = value {
                                object.insert(key.into(), json!(value));
                            }
                        }
                        if object.get("reason").is_some_and(|v| v == "") {
                            object.remove("reason");
                        }
                        if staged {
                            object.insert("staged".into(), json!(true));
                        }
                    }
                    let call = Call {
                        scope: r.scope,
                        credential: r.credential,
                        service: PAGES,
                        event: "preview_opening",
                        contract,
                    };
                    match self.pages.contracts.preview(&call, input.clone()).await {
                        Ok(answer) => {
                            let result = &answer["results"][0];
                            if result["valid"] == true {
                                let facts = result["preview"]["result"]["changes"].clone();
                                if let Some(rows) = facts["rows"].as_array() {
                                    self.columns.load(rows, |r| {
                                        r["source_id"].as_str().unwrap_or("").to_owned()
                                    });
                                }
                                self.review = Some(Review {
                                    contract,
                                    input,
                                    facts,
                                });
                            } else {
                                self.fields=serde_json::from_value(result["error"]["fields"].clone()).unwrap_or_else(|_|BTreeMap::from([("base".into(),vec!["The opening source could not be reviewed. Check permissions and source data.".into()])]));
                            }
                        }
                        Err(e) => self.fail(e),
                    }
                }
                "import_opening" => {
                    if let Some(review) = self.review.take() {
                        let call = Call {
                            scope: r.scope,
                            credential: r.credential,
                            service: PAGES,
                            event: "import_opening",
                            contract: review.contract,
                        };
                        match self.pages.contracts.commit(&call, review.input).await {
                            Ok(_) => {
                                self.completed = review.contract != STAGE;
                                self.message = Some((
                                    StatusKind::Success,
                                    if review.contract == STAGE {
                                        "The source batch was committed. Continue staging or reconcile the complete register.".into()
                                    } else {
                                        "The opening register was reconciled. Prior source evidence is retained.".into()
                                    },
                                ))
                            }
                            Err(e) => self.fail(e),
                        }
                    }
                }
                "cancel_opening" => self.review = None,
                event if event.starts_with("source_evidence_grid:") => {
                    self.evidence.handle(event, &params::string_values(value));
                }
                "class_grid" => {
                    self.class_columns.handle(&params::string_values(value));
                }
                "stage_grid" => {
                    self.stage_columns.handle(&params::string_values(value));
                }
                "balance_grid" => {
                    self.balance_columns.handle(&params::string_values(value));
                }
                "opening_grid" => {
                    self.columns.handle(&params::string_values(value));
                }
                "clear-flash" => {
                    if self
                        .message
                        .as_ref()
                        .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
                    {
                        self.message = None;
                    }
                }
                _ => return Err(Error::Invalid),
            }
        }
        Ok(())
    }
}
