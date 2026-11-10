//! Customer credit actions on the existing document page: Apply and Refund on
//! a credit, Apply on an invoice whose customer has credit left, and Reverse
//! in the credit's history. A credit is an opening balance or the amount a
//! receipt did not apply to an invoice; both are settled the same way. Each
//! action previews, then commits `credit.apply`, `credit.refund` or
//! `settlement.reverse`, the contracts the agent doors call, so the review a
//! person confirms says what an agent's review says.
mod view;
pub(super) use view::applied_rows;

use super::{Pages, PAGES};
use crate::{Document, POST_PAYMENTS};
use bilibi_agent_api::page::Call;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{BuiltinColumn, Fact, Html, ListColumns, StatusKind};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The refusal a person sees when a write names no field of the dialog.
const TRY_AGAIN: &str = "Nothing was changed. Reload the page and try again.";

pub(super) struct Credits {
    allowed: bool,
    /// On a credit: the invoices it can pay, the accounts a refund can be
    /// paid from and what is left of it (`credit.context`).
    context: Value,
    /// On an invoice: its customer's credits that still have something left
    /// (`credit.list`).
    offered: Vec<Value>,
    draft: Option<Draft>,
    columns: ListColumns,
}

struct Draft {
    action: String,
    /// Opened from an invoice: the person chooses the credit, not the invoice.
    on_invoice: bool,
    values: BTreeMap<String, String>,
    fields: BTreeMap<String, Vec<String>>,
    review: Option<Review>,
    /// The amount the dialog last filled in itself, so a choice the person
    /// changes refills it and an amount they typed stays.
    filled: String,
}

struct Review {
    input: Value,
    facts: Value,
    banner: Html,
}

fn contract(action: &str) -> &'static str {
    match action {
        "apply" => "finance.receivables.credit.apply",
        "refund" => "finance.receivables.credit.refund",
        _ => "finance.receivables.settlement.reverse",
    }
}

fn verb(action: &str) -> &'static str {
    match action {
        "apply" => "Apply credit",
        "refund" => "Refund credit",
        _ => "Reverse",
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn number(value: &Value) -> Decimal {
    text(value).parse().unwrap_or(Decimal::ZERO)
}

fn amount(value: Decimal, units: Option<MinorUnits>) -> String {
    Money::display_decimal(value, units)
}

/// An amount as an input shows it: the book's places, no thousands commas.
fn typed(value: Decimal, units: Option<MinorUnits>) -> String {
    match units.and_then(|u| Money::new(value, u).ok().map(|m| m.shown(u))) {
        Some(shown) => shown,
        None => value.normalize().to_string(),
    }
}

fn fact<'a>(title: &'a str, value: impl AsRef<str>) -> Fact<'a> {
    Fact {
        title,
        value: Html::text(value.as_ref()),
        ..Fact::default()
    }
}

impl Credits {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            allowed: false,
            context: Value::Null,
            offered: vec![],
            draft: None,
            columns: ListColumns::mount(
                "credit-settlements",
                vec![
                    BuiltinColumn::new("date", "Date"),
                    BuiltinColumn::new("what", "What happened"),
                    BuiltinColumn::new("amount", "Amount").right(),
                    BuiltinColumn::new("entry", "Journal"),
                    BuiltinColumn::new("reverse", ""),
                ],
            )
            .map_err(|_| Error::Internal)?,
        })
    }

    /// Read what the page's record can do with credit: a credit's invoices
    /// and refund accounts, or an owed invoice's customer credits. Nothing
    /// is read for a person who may not post payments.
    pub(super) async fn load(&mut self, pages: &Pages, s: &Scope, d: &Document) -> Result<()> {
        self.allowed = pages.allowed(s, POST_PAYMENTS).await?;
        self.context = Value::Null;
        self.offered.clear();
        if !self.allowed || d.status != "issued" {
            return Ok(());
        }
        if crate::is_credit(&d.kind) {
            let search = self
                .draft
                .as_ref()
                .and_then(|d| d.values.get("search"))
                .cloned()
                .unwrap_or_default();
            self.search(pages, s, d.id, &search).await;
        } else if d.kind == "invoice" && d.outstanding.is_some_and(|m| m.is_positive()) {
            if let Ok(answer) = pages
                .read(
                    s,
                    "finance.receivables.credit.list",
                    json!({"customer_id": d.customer_id}),
                )
                .await
            {
                self.offered = answer["credits"].as_array().cloned().unwrap_or_default();
            }
        }
        Ok(())
    }

    async fn search(&mut self, pages: &Pages, s: &Scope, record: i64, search: &str) {
        self.context = pages
            .read(
                s,
                "finance.receivables.credit.context",
                json!({"credit_document_id": record, "search": search}),
            )
            .await
            .unwrap_or(Value::Null);
    }

    /// What is left of the credit this page shows.
    fn left(&self) -> Decimal {
        number(&self.context["credit"]["available"])
    }

    /// The credit this page shows can still be refunded.
    pub(super) fn can_refund(&self) -> bool {
        self.allowed && self.left() > Decimal::ZERO
    }

    /// The credit this page shows can pay an invoice of its customer now.
    pub(super) fn can_apply(&self) -> bool {
        self.can_refund()
            && self.context["targets"]
                .as_array()
                .is_some_and(|targets| !targets.is_empty())
    }

    /// What the customer of the invoice this page shows has in unused credit.
    pub(super) fn offered_total(&self) -> Decimal {
        self.offered.iter().map(|c| number(&c["available"])).sum()
    }

    /// Open a dialog an address or a button asked for.
    pub(super) async fn ask(
        &mut self,
        pages: &Pages,
        s: &Scope,
        action: &str,
        d: &Document,
        settlement: &str,
    ) -> Option<(StatusKind, String)> {
        let on_invoice = d.kind == "invoice";
        let refused = |message: &str| Some((StatusKind::Error, message.to_owned()));
        if !self.allowed {
            return refused("You do not have permission to apply or refund credit.");
        }
        match (action, on_invoice) {
            ("apply", true) if self.offered.is_empty() => {
                return refused("This customer has no credit left to apply.");
            }
            ("apply", false) if !self.can_apply() => {
                return refused(if self.can_refund() {
                    "This customer has no unpaid invoice to apply the credit to."
                } else {
                    "Nothing is left on this credit."
                });
            }
            ("refund", _) if !self.can_refund() => {
                return refused("Nothing is left on this credit.");
            }
            _ => {}
        }
        let mut values = BTreeMap::from([("effective_date".to_owned(), pages.today(s).await)]);
        if action == "reverse" {
            values.insert("settlement_id".into(), settlement.to_owned());
        }
        let mut draft = Draft {
            action: action.into(),
            on_invoice,
            values,
            fields: BTreeMap::new(),
            review: None,
            filled: String::new(),
        };
        // One credit or one unpaid invoice needs no choosing.
        if action == "apply" {
            let (key, choices) = if on_invoice {
                ("credit_document_id", self.offered.as_slice())
            } else {
                (
                    "target_document_id",
                    self.context["targets"]
                        .as_array()
                        .map_or(&[][..], Vec::as_slice),
                )
            };
            if let [only] = choices {
                draft.values.insert(key.into(), text(&only["id"]));
            }
        }
        self.fill(&mut draft, d, pages.figures(s).await.units());
        self.draft = Some(draft);
        None
    }

    /// The most the chosen credit can pay of the chosen invoice (or refund),
    /// offered as the amount until the person types their own.
    fn fill(&self, draft: &mut Draft, d: &Document, units: Option<MinorUnits>) {
        if draft.action == "reverse" {
            return;
        }
        let current = draft.values.get("amount").cloned().unwrap_or_default();
        if !current.trim().is_empty() && current != draft.filled {
            return;
        }
        let chosen = |rows: &[Value], key: &str| {
            let id = draft.values.get(key).cloned().unwrap_or_default();
            rows.iter()
                .find(|r| text(&r["id"]) == id)
                .map(|r| number(&r["available"]))
        };
        let offer = match (draft.action.as_str(), draft.on_invoice) {
            ("apply", true) => chosen(&self.offered, "credit_document_id").map(|credit| {
                credit.min(d.outstanding.map_or(Decimal::ZERO, |m| m.amount()))
            }),
            ("apply", false) => chosen(
                self.context["targets"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice),
                "target_document_id",
            )
            .map(|owed| owed.min(self.left())),
            _ => Some(self.left()),
        };
        draft.filled = offer
            .filter(|amount| *amount > Decimal::ZERO)
            .map(|amount| typed(amount, units))
            .unwrap_or_default();
        draft.values.insert("amount".into(), draft.filled.clone());
    }

    /// One event of the dialog or the history table. The answer is a message
    /// for the page to flash once the write is done or refused outright.
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
        v: &Value,
        d: &Document,
    ) -> Result<Option<(StatusKind, String)>> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        match event {
            "credit_grid" => {
                self.columns.handle(&bilibi_ui::params::string_values(v));
                return Ok(None);
            }
            "cancel_credit" => {
                self.draft = None;
                return Ok(None);
            }
            "back_credit" => {
                if let Some(draft) = &mut self.draft {
                    draft.review = None;
                }
                return Ok(None);
            }
            _ => {}
        }
        if let Some(action) = event.strip_prefix("ask_credit_") {
            return Ok(self
                .ask(pages, s, action, d, &text(&v["settlement_id"]))
                .await);
        }
        if !self.allowed {
            self.draft = None;
            return Ok(Some((
                StatusKind::Error,
                "You do not have permission to apply or refund credit.".into(),
            )));
        }
        let units = pages.figures(s).await.units();
        let Some(mut draft) = self.draft.take() else {
            return Ok(None);
        };
        let message = match event {
            "change_credit" => {
                let before = draft.values.get("search").cloned().unwrap_or_default();
                draft
                    .values
                    .extend(bilibi_ui::params::form_values(v, "credit"));
                draft.review = None;
                let search = draft.values.get("search").cloned().unwrap_or_default();
                if before != search {
                    draft.values.remove("target_document_id");
                    self.search(pages, s, d.id, &search).await;
                }
                self.fill(&mut draft, d, units);
                None
            }
            "preview_credit" => {
                draft
                    .values
                    .extend(bilibi_ui::params::form_values(v, "credit"));
                draft.fields.clear();
                let input = Self::input(&draft, d);
                let call = Call {
                    scope: s,
                    credential,
                    service: PAGES,
                    event: "preview_credit",
                    contract: contract(&draft.action),
                };
                match pages.contracts.preview(&call, input.clone()).await {
                    Ok(answer) => {
                        let result = &answer["results"][0];
                        if result["valid"] == true {
                            draft.review = Some(Review {
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
                            draft.fields =
                                serde_json::from_value(result["error"]["fields"].clone())
                                    .unwrap_or_else(|_| Self::base(TRY_AGAIN));
                        }
                    }
                    Err(bilibi_agent_api::Error::InvalidInput(fields)) => draft.fields = fields,
                    Err(_) => draft.fields = Self::base(TRY_AGAIN),
                }
                Self::word(&mut draft);
                None
            }
            "commit_credit" => match draft.review.take() {
                Some(review) => {
                    let call = Call {
                        scope: s,
                        credential,
                        service: PAGES,
                        event: "commit_credit",
                        contract: contract(&draft.action),
                    };
                    match pages.contracts.commit(&call, review.input).await {
                        Ok(_) => {
                            return Ok(Some((
                                StatusKind::Success,
                                match draft.action.as_str() {
                                    "apply" => "Credit applied.",
                                    "refund" => "Refund recorded.",
                                    _ => "Reversed. The credit has the amount back.",
                                }
                                .into(),
                            )));
                        }
                        Err(bilibi_agent_api::Error::InvalidInput(fields)) => {
                            draft.fields = fields;
                        }
                        Err(_) => {
                            draft.fields = Self::base(
                                "The details changed while you were reviewing. Review them again.",
                            );
                        }
                    }
                    Self::word(&mut draft);
                    None
                }
                None => None,
            },
            _ => return Err(Error::Invalid),
        };
        self.draft = Some(draft);
        Ok(message)
    }

    fn base(message: &str) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([("base".into(), vec![message.to_owned()])])
    }

    /// Refusals as the dialog words them: each begins with what it is about,
    /// and the ledger's separation refusal says who must act instead.
    fn word(draft: &mut Draft) {
        let action = draft.action.clone();
        for (key, messages) in &mut draft.fields {
            for message in messages.iter_mut() {
                if message.contains("separation of duties") {
                    *message = format!(
                        "Someone else must {} because you prepared this credit and the company requires separation of duties. For a one-person company, change Separation of duties on Approval exceptions.",
                        match action.as_str() {
                            "apply" => "apply this credit",
                            "refund" => "record this refund",
                            _ => "reverse this",
                        }
                    );
                } else if matches!(
                    key.as_str(),
                    "amount"
                        | "effective_date"
                        | "target_document_id"
                        | "credit_document_id"
                        | "cash_account_id"
                ) {
                    let label = match key.as_str() {
                        "amount" => "Amount",
                        "effective_date" => "Date",
                        "target_document_id" => "Invoice",
                        "credit_document_id" => "Credit",
                        _ => "Paid from",
                    };
                    *message = format!("{label} {message}.");
                }
            }
        }
    }

    /// The contract input the dialog stands for. A choice not made yet is
    /// left out, so the contract names the missing field itself.
    fn input(draft: &Draft, d: &Document) -> Value {
        let value = |key: &str| draft.values.get(key).map_or("", |v| v.trim());
        let mut input = json!({ "effective_date": value("effective_date") });
        let mut choose = |key: &str| {
            if let Ok(id) = value(key).parse::<i64>() {
                input[key] = json!(id);
            }
        };
        match (draft.action.as_str(), draft.on_invoice) {
            ("reverse", _) => choose("settlement_id"),
            ("apply", true) => {
                choose("credit_document_id");
                input["target_document_id"] = json!(d.id);
            }
            ("apply", false) => {
                choose("target_document_id");
                input["credit_document_id"] = json!(d.id);
            }
            _ => {
                choose("cash_account_id");
                input["credit_document_id"] = json!(d.id);
            }
        }
        if draft.action != "reverse" {
            input["amount"] = json!(value("amount"));
        }
        input
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
