//! Opening conversion controls on the bank account, using the same reviewed
//! contracts as the agent doors.
use super::{company, render, Pages, PAGES};
use askama::Template;
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    params, Alert, BuiltinColumn, Button, ButtonVariant, Cell, Fact, Facts, Field, FlexRow,
    FlexTable, Form, Html, InputKind, ListColumns, Modal, ModalWidth, StatusKind,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const IMPORT: &str = "finance.reconciliation.opening.import";
const CONTEXT: &str = "finance.reconciliation.opening.context";
const CURRENT: &str = "finance.reconciliation.opening.current";

#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    buttons: Html,
}
struct Review {
    input: Value,
    facts: Value,
}

pub(super) struct Opening {
    account: i64,
    bridge: Option<i64>,
    open: bool,
    values: Value,
    fields: BTreeMap<String, Vec<String>>,
    message: Option<String>,
    review: Option<Review>,
    columns: ListColumns,
}
impl Opening {
    pub(super) fn new(account: i64) -> Result<Self> {
        Ok(Self {
            account,
            bridge: None,
            open: false,
            values: json!({}),
            fields: BTreeMap::new(),
            message: None,
            review: None,
            columns: ListColumns::mount(
                "opening-items",
                vec![
                    BuiltinColumn::new("source", "Source ID"),
                    BuiltinColumn::new("date", "Date"),
                    BuiltinColumn::new("description", "Description"),
                    BuiltinColumn::new("amount", "Amount").right(),
                ],
            )
            .map_err(|_| Error::Internal)?,
        })
    }
    pub(super) fn bridge(&self) -> Option<i64> {
        self.bridge
    }
    pub(super) fn clear_bridge(&mut self) {
        self.bridge = None;
    }
    fn failure(&mut self, error: ContractError) {
        match error {
            ContractError::InvalidInput(fields) => self.fields = fields,
            ContractError::Forbidden => {
                self.message = Some(
                    "You do not have permission to record opening items for the current company."
                        .into(),
                )
            }
            ContractError::Backend(Error::Conflict) => {
                self.message = Some("The figures changed. Preview the source again.".into())
            }
            _ => {
                self.message = Some(
                    "The opening items could not be recorded. Check the source and try again."
                        .into(),
                )
            }
        }
    }
    pub(super) async fn body(&mut self, pages: &Pages, scope: &Scope) -> Result<Html> {
        let can_manage = pages.allowed(scope, crate::MANAGE_RECONCILIATIONS).await?;
        let mut result = if can_manage {
            render(Button {
                id: "open-bank-opening",
                button_type: "button",
                event: "open_bank_opening",
                body: Html::text("Opening bank items"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let current = pages
            .contracts
            .read(
                scope,
                CURRENT,
                &json!({"company_id":company(scope)?,"bank_account_id":self.account}),
            )
            .await;
        if let Ok(current) = &current {
            self.bridge = current["result"]["bridge_id"].as_i64();
            if self.bridge.is_some() {
                result.push(&render(AttachmentsSlot)?);
            }
        }
        if !can_manage {
            return Ok(result);
        }
        if !self.open && self.review.is_none() {
            return Ok(result);
        }
        let context = match pages
            .contracts
            .read(
                scope,
                CONTEXT,
                &json!({"company_id":company(scope)?,"bank_account_id":self.account}),
            )
            .await
        {
            Ok(value) => value["result"].clone(),
            Err(_) => {
                result.push(&render(Alert {
                    kind: StatusKind::Warning,
                    body: Html::text(
                        "Approve the opening balances journal before recording opening bank items.",
                    ),
                    ..Alert::default()
                })?);
                return Ok(result);
            }
        };
        if self.open {
            let mut fields = Html::EMPTY;
            if let Some(message) = &self.message {
                fields.push(&render(Alert {
                    kind: StatusKind::Error,
                    body: Html::text(message),
                    ..Alert::default()
                })?);
            }
            if let Some(errors) = self.fields.get("base") {
                fields.push(&render(Alert {
                    kind: StatusKind::Error,
                    body: Html::text(&errors.join(" ")),
                    ..Alert::default()
                })?);
            }
            fields.push(&render(Alert {body:Html::text("Enter the statement balance on the conversion date and the outstanding payments and deposits already included in the opening journal. Payments are negative; deposits are positive."),..Alert::default()})?);
            if let Some(id) = context["bridge"]["id"].as_i64() {
                let source = context["bridge"]["source_reference"].as_str().unwrap_or("");
                fields.push(&render(Alert {
                    body: Html::text(&format!("Current opening bridge {id}: {source}. To correct it, enter its ID and a reason below.")),
                    ..Alert::default()
                })?);
            }
            for (key, label, kind, required, hint) in [
                (
                    "source_reference",
                    "Source reference",
                    InputKind::Text,
                    true,
                    "Keep this reference for a retry.",
                ),
                (
                    "statement_source",
                    "Source statement",
                    InputKind::Text,
                    true,
                    "Identify the bank statement; attach it after recording.",
                ),
                (
                    "statement_balance",
                    "Statement balance",
                    InputKind::Text,
                    true,
                    "Use the balance on the conversion date.",
                ),
                (
                    "csv",
                    "Outstanding items CSV",
                    InputKind::Textarea,
                    true,
                    "Header: source_id,item_date,description,amount. At most 500 rows.",
                ),
                (
                    "replaces_bridge_id",
                    "Opening bridge to replace",
                    InputKind::Number,
                    false,
                    "For a correction, enter the current bridge ID.",
                ),
                (
                    "reason",
                    "Correction reason",
                    InputKind::Text,
                    false,
                    "Required when replacing a bridge.",
                ),
                (
                    "override_reason",
                    "Approval exception reason",
                    InputKind::Text,
                    false,
                    "Only needed for an approved duty-separation exception.",
                ),
            ] {
                let mut field = Field::in_form("bank_opening", key)
                    .id(format!("bank-opening-{key}"))
                    .label(label)
                    .kind(kind)
                    .value(self.values[key].as_str().unwrap_or(""))
                    .hint(hint)
                    .errors(
                        self.fields
                            .get(key)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|e| bilibi_ui::error_sentence(label, &e))
                            .collect(),
                    );
                if required {
                    field = field.required();
                }
                if key == "csv" {
                    field = field.attr("rows", "8");
                }
                fields.push(&render(field)?);
            }
            let mut buttons = render(Button {
                button_type: "button",
                event: "cancel_bank_opening",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Preview"),
                working: "Checking…",
                ..Button::default()
            })?);
            let form = render(Form {
                id: "bank-opening-form",
                event: "preview_opening",
                class: "mt-4",
                body: fields,
                actions: render(Actions { buttons })?,
                ..Form::default()
            })?;
            result.push(&render(Modal {
                id: "bank-opening-modal",
                title: "Opening bank items",
                description: "Record the opening bridge for the current company.",
                cancel_event: "cancel_bank_opening",
                width: ModalWidth::Wide,
                body: form,
                ..Modal::default()
            })?);
        }
        if let Some(review) = &self.review {
            let f = &review.facts;
            let units = context["book"]["minor_units"]
                .as_i64()
                .and_then(MinorUnits::new)
                .ok_or(Error::Internal)?;
            let money = |v: &Value| {
                v.as_str()
                    .and_then(|s| Money::parse(s, units).ok())
                    .map(|v| v.display(units))
                    .unwrap_or_else(|| "—".into())
            };
            let mut body = pages
                .plans
                .instance()
                .await?
                .banner("bank-opening-instance", "")
                .map_err(|_| Error::Internal)?;
            if f["already_imported"] == true {
                body.push(&render(Alert {body:Html::text("This exact source was already recorded. Continuing returns the existing bridge without posting again."),..Alert::default()})?);
            } else {
                body.push(&render(Alert {body:Html::text(&format!("Conversion date {}. Source {}. Review each amount against the source statement and register. No new entry will be posted.",f["conversion_date"].as_str().unwrap_or(""),f["source_reference"].as_str().unwrap_or(""))),..Alert::default()})?);
                body.push(&render(Facts {
                    id: "opening-totals",
                    items: vec![
                        Fact {
                            title: "Source trial balance",
                            value: Html::text(&money(&f["source_trial_balance"])),
                            ..Default::default()
                        },
                        Fact {
                            title: "Opening bank ledger",
                            value: Html::text(&money(&f["opening_bank_balance"])),
                            ..Default::default()
                        },
                        Fact {
                            title: "Source statement",
                            value: Html::text(&money(&f["statement_balance"])),
                            ..Default::default()
                        },
                        Fact {
                            title: "Outstanding items",
                            value: Html::text(&money(&f["items_total"])),
                            ..Default::default()
                        },
                        Fact {
                            title: "Difference",
                            value: Html::text(&money(&f["difference"])),
                            ..Default::default()
                        },
                    ],
                })?);
                let rows = f["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        FlexRow::new(item["source_id"].as_str().unwrap_or(""))
                            .cell(
                                "source",
                                Cell::text(item["source_id"].as_str().unwrap_or("")),
                            )
                            .cell("date", Cell::text(item["item_date"].as_str().unwrap_or("")))
                            .cell(
                                "description",
                                Cell::text(item["description"].as_str().unwrap_or("")),
                            )
                            .cell("amount", Cell::text(money(&item["amount"])))
                    })
                    .collect();
                let mut table = FlexTable::new("opening-items", self.columns.flex_columns(), rows);
                table.event = "opening_grid";
                table.caption = "Outstanding opening items";
                table.zoom = self.columns.zoom;
                table.suggestions = self.columns.flex_suggestions();
                table.add_query = &self.columns.add_query;
                body.push(&render(table)?);
            }
            let mut buttons = render(Button {
                button_type: "button",
                event: "cancel_bank_opening",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                button_type: "button",
                event: "import_opening",
                variant: ButtonVariant::Primary,
                body: Html::text("Record opening items"),
                working: "Recording…",
                ..Button::default()
            })?);
            body.push(&render(Actions { buttons })?);
            result.push(&render(Modal {
                id: "bank-opening-review-modal",
                title: "Review opening bank items",
                description: "Confirm the source and balances for the current company.",
                cancel_event: "cancel_bank_opening",
                width: ModalWidth::Wide,
                body,
                ..Modal::default()
            })?);
        }
        Ok(result)
    }
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        event: &str,
        value: &Value,
    ) -> Result<Option<String>> {
        match event {
            "open_bank_opening" => {
                self.open = true;
                self.message = None;
            }
            "cancel_bank_opening" => {
                self.open = false;
                self.review = None;
            }
            "opening_grid" => {
                self.columns.handle(&params::string_values(value));
            }
            "preview_opening" => {
                self.fields.clear();
                self.message = None;
                let fields = params::form_values_checked(value, "bank_opening", 131072)
                    .map_err(Error::refused)?;
                self.values = json!(fields);
                let context = match pages
                    .contracts
                    .read(
                        scope,
                        CONTEXT,
                        &json!({"company_id":company(scope)?,"bank_account_id":self.account}),
                    )
                    .await
                {
                    Ok(v) => v["result"].clone(),
                    Err(e) => {
                        self.failure(e);
                        return Ok(None);
                    }
                };
                let mut input = self.values.clone();
                input["company_id"] = json!(company(scope)?);
                input["bank_account_id"] = json!(self.account);
                input["currency_code"] = context["book"]["currency_code"].clone();
                if let Some(object) = input.as_object_mut() {
                    let replaces = object
                        .get("replaces_bridge_id")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<i64>().ok());
                    object.remove("replaces_bridge_id");
                    if let Some(id) = replaces {
                        object.insert("replaces_bridge_id".into(), json!(id));
                    }
                    for key in ["reason", "override_reason"] {
                        if object.get(key).is_some_and(|v| v == "") {
                            object.remove(key);
                        }
                    }
                }
                let call = Call {
                    scope,
                    credential,
                    service: PAGES,
                    event: "preview_opening",
                    contract: IMPORT,
                };
                match pages.contracts.preview(&call, input.clone()).await {
                    Ok(answer) => {
                        let result = &answer["results"][0];
                        if result["valid"] == true {
                            let facts = result["preview"]["result"]["changes"].clone();
                            self.review = Some(Review { input, facts });
                            self.open = false;
                        } else {
                            self.fields = serde_json::from_value(result["error"]["fields"].clone())
                                .unwrap_or_else(|_| {
                                    BTreeMap::from([(
                                        "base".into(),
                                        vec!["The opening source could not be reviewed.".into()],
                                    )])
                                });
                        }
                    }
                    Err(e) => self.failure(e),
                }
            }
            "import_opening" => {
                if let Some(review) = self.review.take() {
                    let call = Call {
                        scope,
                        credential,
                        service: PAGES,
                        event: "import_opening",
                        contract: IMPORT,
                    };
                    match pages.contracts.commit(&call, review.input).await {
                        Ok(_) => {
                            self.open = false;
                            self.values = json!({});
                            return Ok(Some(
                                "Opening items recorded without another ledger entry.".into(),
                            ));
                        }
                        Err(e) => {
                            self.open = true;
                            self.failure(e);
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}

#[derive(Template)]
#[template(
    source = r#"<div id="reconciliation-opening-attachments-slot" class="mt-4" data-live-ignore><div id="reconciliation-opening-attachments"></div></div>"#,
    ext = "html"
)]
struct AttachmentsSlot;
