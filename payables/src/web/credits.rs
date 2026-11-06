//! Credit actions stay on the existing document page and use the agent doors.
use super::{render, Pages, PAGES};
use crate::POST_PAYMENTS;
use bilibi_agent_api::page::Call;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    Alert, Attrs, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice, Combobox, Fact, Facts,
    Field, FlexRow, FlexTable, Fold, Form, Html, InputKind, ListColumns, Modal, ModalWidth,
    SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct Credits {
    allowed: bool,
    context: Value,
    draft: Option<Draft>,
    columns: ListColumns,
    message: Option<(StatusKind, String)>,
}
struct Draft {
    action: String,
    values: BTreeMap<String, String>,
    fields: BTreeMap<String, Vec<String>>,
    review: Option<Review>,
}
struct Review {
    input: Value,
    facts: Value,
    banner: Html,
}
fn contract(action: &str) -> &'static str {
    match action {
        "apply" => "finance.payables.credit.apply",
        "refund" => "finance.payables.credit.refund",
        _ => "finance.payables.settlement.reverse",
    }
}
fn verb(action: &str) -> &str {
    match action {
        "apply" => "Apply credit",
        "refund" => "Receive supplier refund",
        _ => "Reverse settlement",
    }
}
fn text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            "—".into()
        } else {
            value.to_string()
        }
    })
}
fn fact<'a>(title: &'a str, value: impl AsRef<str>) -> Fact<'a> {
    Fact {
        title,
        value: Html::text(value.as_ref()),
        ..Fact::default()
    }
}
fn money(value: &Value, units: MinorUnits) -> String {
    Money::parse(&text(value), units)
        .map(|m| m.display(units))
        .unwrap_or_else(|_| "—".into())
}
impl Credits {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            allowed: false,
            context: Value::Null,
            draft: None,
            message: None,
            columns: ListColumns::mount(
                "credit-settlements",
                [
                    ("date", "Effective date"),
                    ("kind", "Action"),
                    ("amount", "Amount"),
                    ("target", "Invoice or bill"),
                    ("reference", "Reference"),
                    ("entry", "Ledger entry"),
                    ("reverse", "Action"),
                ]
                .iter()
                .map(|(id, label)| BuiltinColumn::new(id, label))
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
        })
    }
    pub(super) async fn load(
        &mut self,
        pages: &Pages,
        s: &Scope,
        record: i64,
        kind: &str,
    ) -> Result<()> {
        self.allowed = pages.allowed(s, POST_PAYMENTS).await?;
        if kind == "opening_credit" && self.allowed {
            let search = self
                .draft
                .as_ref()
                .and_then(|d| d.values.get("search"))
                .cloned()
                .unwrap_or_default();
            self.search(pages, s, record, &search).await?;
        }
        Ok(())
    }
    async fn search(&mut self, pages: &Pages, s: &Scope, record: i64, search: &str) -> Result<()> {
        let company = s.current_company().ok_or(Error::Unauthorized)?.get();
        let mut answer = pages
            .contracts
            .read(
                s,
                "finance.payables.credit.context",
                &json!({"company_id":company,"credit_document_id":record,"search":search}),
            )
            .await
            .map_err(|_| Error::Invalid)?;
        self.context = answer["result"].take();
        Ok(())
    }
    pub(super) async fn body(
        &self,
        pages: &Pages,
        record: i64,
        kind: &str,
        events: &[Value],
        units: Option<MinorUnits>,
    ) -> Result<Html> {
        let units = units.or_else(|| {
            self.context["book"]["minor_units"]
                .as_i64()
                .and_then(MinorUnits::new)
        });
        let mut actions = Html::EMPTY;
        if kind == "opening_credit" && self.allowed {
            for (action, label) in [
                ("apply", "Apply credit"),
                ("refund", "Receive supplier refund"),
            ] {
                actions.push(&render(Button {
                    id: &format!("credit-{action}"),
                    button_type: "button",
                    event: &format!("ask_credit_{action}"),
                    body: Html::text(label),
                    ..Button::default()
                })?);
            }
        }
        if events.is_empty() && actions.is_empty() {
            return Ok(Html::EMPTY);
        }
        let mut body=render(SectionHeading{title:"Credit settlements",description:"Applications and refunds retain their original controls and dates. A same-control application changes no ledger entry.",actions,..SectionHeading::default()})?;
        if let Some((kind, message)) = &self.message {
            body.push(&render(Alert {
                id: "credit-message",
                kind: *kind,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        let rows = events
            .iter()
            .map(|e| {
                let reversed = events
                    .iter()
                    .any(|r| r["reverses_settlement_id"] == e["id"]);
                let button = if self.allowed && e["kind"] != "reverse" && !reversed {
                    render(Button {
                        id: &format!("credit-reverse-{}", e["id"]),
                        event: "ask_credit_reverse",
                        name: "settlement_id",
                        value: &text(&e["id"]),
                        button_type: "button",
                        body: Html::text("Reverse"),
                        ..Button::default()
                    })?
                } else {
                    Html::text(if reversed { "Reversed" } else { "" })
                };
                Ok(FlexRow::new(text(&e["id"]))
                    .cell("date", Cell::text(text(&e["effective_date"])))
                    .cell("kind", Cell::text(text(&e["kind"])))
                    .cell(
                        "amount",
                        Cell::text(
                            text(&e["amount"])
                                .parse()
                                .map(|amount| Money::display_decimal(amount, units))
                                .unwrap_or_else(|_| "—".into()),
                        ),
                    )
                    .cell("target", Cell::text(text(&e["target_document_id"])))
                    .cell("reference", Cell::text(text(&e["reference"])))
                    .cell(
                        "entry",
                        Cell::text(if e["entry_number"].is_null() {
                            "No posting".into()
                        } else {
                            format!("#{}", e["entry_number"])
                        }),
                    )
                    .slot("reverse", button))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new("credit-settlements", self.columns.flex_columns(), rows);
        table.event = "credit_grid";
        table.caption = "Credit settlement history";
        body.push(&render(table)?);
        if let Some(d) = &self.draft {
            let mut form_body = Html::EMPTY;
            if let Some(review) = &d.review {
                form_body.push(&review.banner);
                let f = &review.facts;
                let mut items = vec![
                    fact("Action", verb(&d.action)),
                    fact(
                        "Credit source",
                        text(&f["credit"]["source"]["document_reference"]),
                    ),
                    fact("Supplier", text(&f["credit"]["party_name"])),
                    fact("Target", text(&f["target"]["party_document_number"])),
                    fact("Effective date", text(&f["effective_date"])),
                    fact(
                        "Posting",
                        if f["posts"] == true {
                            "Owned ledger entry"
                        } else {
                            "No ledger entry"
                        },
                    ),
                ];
                if let Some(units) = units {
                    items.push(fact("Amount", money(&f["amount"], units)));
                    items.push(fact(
                        "Available credit",
                        money(&f["available_credit"], units),
                    ));
                    if !f["available_target"].is_null() {
                        items.push(fact(
                            "Available invoice or bill",
                            money(&f["available_target"], units),
                        ));
                    }
                }
                for account in f["controls"].as_array().into_iter().flatten() {
                    items.push(fact(
                        "Account",
                        format!("{} · {}", text(&account["name"]), text(&account["code"])),
                    ));
                }
                form_body.push(&render(Facts {
                    id: "credit-review-facts",
                    items,
                })?);
                form_body.push(&render(Fold {
                    id: "credit-review-details",
                    title: "Review details",
                    summary: "Source, posting and approval facts",
                    body: Html::text(
                        &serde_json::to_string_pretty(f).map_err(|_| Error::Internal)?,
                    ),
                    open: false,
                })?);
            } else {
                for (key, label) in [
                    ("effective_date", "Effective date"),
                    ("amount", "Amount"),
                    ("reference", "Reference"),
                    ("override_reason", "Approval exception reason"),
                ] {
                    if d.action == "reverse" && key == "amount" {
                        continue;
                    }
                    let mut field = Field::named(format!("credit[{key}]"))
                        .label(label)
                        .id(format!("credit-{key}"))
                        .value(d.values.get(key).cloned().unwrap_or_default())
                        .errors(d.fields.get(key).cloned().unwrap_or_default());
                    if key != "override_reason" {
                        field = field.required();
                    }
                    if key == "effective_date" {
                        field = field.kind(InputKind::Date);
                    }
                    form_body.push(&render(field)?);
                }
                let key = if d.action == "apply" {
                    "target_document_id"
                } else {
                    "cash_account_id"
                };
                if d.action != "reverse" {
                    if d.action == "apply" {
                        form_body.push(&render(
                            Field::named("credit[search]")
                                .label("Find invoice or bill")
                                .id("credit-search")
                                .value(d.values.get("search").cloned().unwrap_or_default()),
                        )?);
                    }
                    let choices = if d.action == "apply" {
                        self.context["targets"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|v| {
                                Choice::new(text(&v["party_document_number"]), text(&v["id"]))
                                    .detail(text(&v["document_date"]))
                            })
                            .collect()
                    } else {
                        self.context["cash_accounts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|v| {
                                Choice::new(text(&v["name"]), text(&v["id"]))
                                    .detail(text(&v["code"]))
                            })
                            .collect()
                    };
                    form_body.push(&render(
                        Combobox::new(format!("credit-{key}"), format!("credit[{key}]"))
                            .label(if d.action == "apply" {
                                "Invoice or bill *"
                            } else {
                                "Cash or deposit account *"
                            })
                            .aria_label(if d.action == "apply" {
                                "Invoice or bill"
                            } else {
                                "Cash or deposit account"
                            })
                            .value(d.values.get(key).cloned().unwrap_or_default())
                            .options(choices)
                            .errors(d.fields.get(key).cloned().unwrap_or_default())
                            .attrs(Attrs {
                                required: true,
                                ..Attrs::default()
                            }),
                    )?);
                }
                let general = d
                    .fields
                    .iter()
                    .filter(|(key, _)| {
                        ![
                            "effective_date",
                            "amount",
                            "reference",
                            "override_reason",
                            "target_document_id",
                            "cash_account_id",
                        ]
                        .contains(&key.as_str())
                    })
                    .flat_map(|(_, v)| v.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !general.is_empty() {
                    form_body.push(&render(Alert {
                        id: "credit-errors",
                        kind: StatusKind::Error,
                        body: Html::text(&general),
                        ..Alert::default()
                    })?);
                }
            }
            let mut buttons = render(Button {
                id: "credit-cancel",
                button_type: "button",
                event: "cancel_credit",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                id: "credit-submit",
                button_type: "button",
                event: if d.review.is_some() {
                    "commit_credit"
                } else {
                    "preview_credit"
                },
                variant: ButtonVariant::Primary,
                body: Html::text(if d.review.is_some() {
                    verb(&d.action)
                } else {
                    "Review settlement"
                }),
                ..Button::default()
            })?);
            let form = render(Form {
                id: "credit-form",
                event: "change_credit",
                change: true,
                class: "mt-4",
                body: form_body,
                actions: super::format::actions(buttons)?,
            })?;
            body.push(&render(Modal{id:"credit-settlement-modal",title:verb(&d.action),description:"Settle this credit for the current company. Review the dated balances and posting before confirming.",cancel_event:"cancel_credit",width:ModalWidth::Wide,body:form,..Modal::default()})?);
        }
        let _ = (pages, record);
        render(Card {
            id: "credit-settlement-card",
            class: "mt-4",
            body,
            ..Card::default()
        })
    }
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
        v: &Value,
        record: i64,
    ) -> Result<()> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        if event == "credit_grid" {
            self.columns.handle(&bilibi_ui::params::string_values(v));
            return Ok(());
        }
        if event == "cancel_credit" {
            self.draft = None;
            return Ok(());
        }
        if !pages.allowed(s, POST_PAYMENTS).await? {
            self.message = Some((
                StatusKind::Error,
                "You do not have permission to settle this credit.".into(),
            ));
            return Ok(());
        }
        if let Some(action) = event.strip_prefix("ask_credit_") {
            let mut values = BTreeMap::new();
            values.insert("effective_date".into(), pages.today(s).await?.to_string());
            values.insert("settlement_id".into(), text(&v["settlement_id"]));
            self.draft = Some(Draft {
                action: action.into(),
                values,
                fields: BTreeMap::new(),
                review: None,
            });
            return Ok(());
        }
        if event == "change_credit" {
            let Some(d) = &mut self.draft else {
                return Ok(());
            };
            let before = d.values.get("search").cloned().unwrap_or_default();
            d.values.extend(bilibi_ui::params::form_values(v, "credit"));
            d.review = None;
            let search = d.values.get("search").cloned().unwrap_or_default();
            if before != search {
                d.values.remove("target_document_id");
                self.search(pages, s, record, &search).await?;
            }
            return Ok(());
        }
        if event == "preview_credit" {
            let Some(d) = &mut self.draft else {
                return Ok(());
            };
            d.values.extend(bilibi_ui::params::form_values(v, "credit"));
            let mut input = json!({"company_id":s.current_company().ok_or(Error::Unauthorized)?.get(),"credit_document_id":record});
            for (k, value) in &d.values {
                if ["search", "settlement_id"].contains(&k.as_str()) {
                    continue;
                }
                if !value.is_empty() {
                    input[k] = if ["target_document_id", "cash_account_id"].contains(&k.as_str()) {
                        json!(value.parse::<i64>().ok())
                    } else {
                        json!(value.trim())
                    };
                }
            }
            if d.action == "reverse" {
                input
                    .as_object_mut()
                    .ok_or(Error::Invalid)?
                    .remove("credit_document_id");
                input["settlement_id"] = json!(d
                    .values
                    .get("settlement_id")
                    .and_then(|v| v.parse::<i64>().ok()));
            }
            let call = Call {
                scope: s,
                credential,
                service: PAGES,
                event: "preview_credit",
                contract: contract(&d.action),
            };
            match pages.contracts.preview(&call, input.clone()).await {
                Ok(answer) => {
                    let result = &answer["results"][0];
                    if result["valid"] == true {
                        d.review = Some(Review {
                            input,
                            facts: result["preview"]["result"]["changes"].clone(),
                            banner: pages
                                .plans
                                .instance()
                                .await?
                                .banner("credit-instance", "")
                                .map_err(|_| Error::Internal)?,
                        });
                    } else {
                        d.fields=serde_json::from_value(result["error"]["fields"].clone()).unwrap_or_else(|_|BTreeMap::from([("base".into(),vec!["The settlement could not be reviewed. Check access and try again.".into()])]));
                    }
                }
                Err(bilibi_agent_api::Error::InvalidInput(fields)) => d.fields = fields,
                Err(_) => {
                    d.fields = BTreeMap::from([(
                        "base".into(),
                        vec![
                            "The settlement could not be reviewed. Check access and try again."
                                .into(),
                        ],
                    )]);
                }
            }
            return Ok(());
        }
        if event == "commit_credit" {
            if let Some(d) = &mut self.draft {
                if let Some(review) = d.review.take() {
                    let call = Call {
                        scope: s,
                        credential,
                        service: PAGES,
                        event: "commit_credit",
                        contract: contract(&d.action),
                    };
                    match pages.contracts.commit(&call, review.input).await {
                        Ok(_) => {
                            self.draft = None;
                            self.message =
                                Some((StatusKind::Success, "Credit settlement recorded.".into()));
                        }
                        Err(bilibi_agent_api::Error::InvalidInput(fields)) => d.fields = fields,
                        Err(_) => {
                            d.fields.insert("base".into(),vec!["The reviewed details changed or the settlement was refused. Review it again.".into()]);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// A drill-down carries the complete amount tuple in the existing record URL.
pub(super) fn balance_query(query: &std::collections::HashMap<String, String>) -> Result<Value> {
    let mut input = json!({});
    if let Some(day) = query.get("as_of") {
        input["as_of"] = json!(day);
    }
    let pin_keys = [
        "through_entry_number",
        "through_opening_batch_id",
        "through_settlement_number",
    ];
    let supplied = pin_keys
        .iter()
        .filter(|key| query.contains_key(**key))
        .count();
    if supplied != 0 && supplied != pin_keys.len() {
        return Err(Error::Invalid);
    }
    for key in pin_keys {
        if let Some(value) = query.get(key) {
            input[key] = json!(value.parse::<i64>().map_err(|_| Error::Invalid)?);
        }
    }
    if input.as_object().is_some_and(|o| !o.is_empty()) && input["as_of"].is_null() {
        return Err(Error::Invalid);
    }
    Ok(input)
}

/// Preserve the complete captured tuple when the live host updates the URL.
pub(super) fn balance_params(pins: &Value) -> std::collections::HashMap<String, String> {
    pins.as_object()
        .into_iter()
        .flat_map(|fields| fields.iter())
        .map(|(key, value)| (key.clone(), text(value)))
        .collect()
}
