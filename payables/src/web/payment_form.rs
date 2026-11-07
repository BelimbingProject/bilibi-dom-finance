//! Requesting a supplier payment: the supplier, the account it is paid from
//! and how much goes to each bill still owed. Bilimbi's payables had no page
//! for it; this saves the request through `finance.payables.payment.submit`,
//! the contract the agent doors call. Someone else approves it afterwards.
use super::{company, format as f, record_id, render, Pages, Written, PAYMENTS};
use crate::{AgedParty, AgingQuery, SUBMIT_PAYMENTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountType, MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    error_sentence,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BackLink, Button, ButtonVariant, Choice, Combobox, EmptyState, ErrorSummary,
    Facts, Field, Html, InputKind, StatusKind,
};
use jiff::civil::Date;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const PREFIX: &str = "payment";

#[derive(Default)]
struct Draft {
    date: String,
    party: String,
    account: String,
    reference: String,
    description: String,
    amounts: BTreeMap<i64, String>,
}

pub(super) struct PaymentForm {
    pages: Pages,
    for_bill: Option<i64>,
    draft: Draft,
    started: bool,
    errors: Vec<(String, String)>,
    asked: Vec<i64>,
    submitted: bool,
    notice: Option<String>,
    owed: Vec<AgedParty>,
    accounts: Vec<Account>,
    units: Option<MinorUnits>,
    currency: String,
    unavailable: Option<&'static str>,
}

impl PaymentForm {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            for_bill: params::positive_id(query.get("bill").map(String::as_str)),
            draft: Draft::default(),
            started: false,
            errors: vec![],
            asked: vec![],
            submitted: false,
            notice: None,
            owed: vec![],
            accounts: vec![],
            units: None,
            currency: String::new(),
            unavailable: None,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.unavailable = None;
        let Some(company) = company(scope) else {
            self.unavailable = Some("Choose a company to request payments for.");
            return Ok(());
        };
        let Ok(book) = self.pages.ledger.get_book(scope, company).await else {
            self.unavailable =
                Some("This company's books are not open yet, or you cannot read them.");
            return Ok(());
        };
        self.units = Some(book.minor_units);
        self.currency = book.currency_code;
        self.accounts = self
            .pages
            .ledger
            .list_accounts(scope, company)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|a| {
                a.postable()
                    && matches!(a.account_type, AccountType::Asset | AccountType::Liability)
            })
            .collect();
        if !self.started {
            self.draft.date = self.pages.today(scope).await?.to_string();
            self.started = true;
            if let Some(bill) = self.for_bill {
                self.prefill(scope, company, bill).await;
            }
        }
        let as_of = self
            .draft
            .date
            .parse::<Date>()
            .unwrap_or(self.pages.today(scope).await?);
        self.owed = match self
            .pages
            .payables
            .aged_payables(scope, company, as_of, &AgingQuery::default())
            .await
        {
            Ok(aged) => aged.parties,
            Err(_) => {
                self.unavailable =
                    Some("What is owed to suppliers cannot be read with your current access.");
                vec![]
            }
        };
        // Only a bill dated on or before the payment can be paid by it.
        for party in &mut self.owed {
            party.documents.retain(|d| d.document_date <= as_of);
        }
        self.owed.retain(|p| !p.documents.is_empty());
        Ok(())
    }

    /// Starts from a bill that is still owed: its supplier and the whole amount.
    async fn prefill(&mut self, scope: &Scope, company: i64, bill: i64) {
        let Ok(d) = self.pages.payables.get_document(scope, company, bill).await else {
            return;
        };
        if d.document_type != "bill" || d.status != "posted" {
            return;
        }
        if let (Some(owed), Some(units)) = (d.outstanding, self.units) {
            if owed.is_positive() {
                self.draft.party = d.party.key.clone();
                self.draft.amounts.insert(d.id, owed.shown(units));
            }
        }
    }

    fn read(&mut self, v: &Value) {
        let mut fields: HashMap<&str, &str> = HashMap::new();
        let mut amounts: BTreeMap<i64, String> = BTreeMap::new();
        for (name, value) in v.as_object().into_iter().flatten() {
            let Some(key) = name
                .strip_prefix(PREFIX)
                .and_then(|n| n.strip_prefix('['))
                .and_then(|n| n.strip_suffix(']'))
            else {
                continue;
            };
            let Some(text) = value.as_str() else { continue };
            match key
                .strip_prefix("amount_")
                .and_then(|id| id.parse::<i64>().ok())
            {
                Some(id) => {
                    amounts.insert(id, text.into());
                }
                None => {
                    fields.insert(key, text);
                }
            }
        }
        let d = &mut self.draft;
        let take = |fields: &HashMap<&str, &str>, key: &str, into: &mut String| {
            if let Some(text) = fields.get(key) {
                *into = (*text).into();
            }
        };
        take(&fields, "date", &mut d.date);
        take(&fields, "account", &mut d.account);
        take(&fields, "reference", &mut d.reference);
        take(&fields, "description", &mut d.description);
        let before = d.party.clone();
        take(&fields, "party", &mut d.party);
        if d.party != before {
            // Amounts belong to one supplier's bills.
            d.amounts.clear();
        } else if !amounts.is_empty() || fields.contains_key("party") {
            d.amounts = amounts;
        }
    }

    fn party(&self) -> Option<&AgedParty> {
        self.owed.iter().find(|p| p.party_key == self.draft.party)
    }

    fn total(&self) -> Option<(Money, bool)> {
        let units = self.units?;
        let mut total = Money::ZERO;
        let mut complete = true;
        for amount in self.draft.amounts.values().filter(|a| !a.trim().is_empty()) {
            match Money::parse(amount, units) {
                Ok(m) => total = total + m,
                Err(_) => complete = false,
            }
        }
        Some((total, complete))
    }

    fn locate(&self, path: &str) -> Option<(String, String)> {
        let top = |id: &str, label: &str| Some((format!("payables-pay-{id}"), label.to_owned()));
        match path {
            "payment_date" => top("date", "Payment date"),
            "supplier_key" | "vendor_id" => top("party", "Supplier"),
            "paid_from_account_id" => top("account", "Paid from"),
            "reference" => top("reference", "Reference"),
            "description" => top("description", "Description"),
            _ => {
                let rest = path.strip_prefix("allocations.")?;
                let (index, field) = rest.split_once('.')?;
                let doc = self.asked.get(index.parse::<usize>().ok()?)?;
                let bill = self
                    .party()
                    .and_then(|p| p.documents.iter().find(|d| d.id == *doc))
                    .map_or_else(
                        || format!("bill {doc}"),
                        |d| format!("bill #{}", d.document_number),
                    );
                match field {
                    "amount" | "document_id" => Some((
                        format!("payables-pay-amount-{doc}"),
                        format!("Amount for {bill}"),
                    )),
                    _ => None,
                }
            }
        }
    }

    fn errors_for(&self, element: &str) -> Vec<String> {
        self.errors
            .iter()
            .filter_map(|(path, message)| {
                let (id, label) = self.locate(path)?;
                (id == element).then(|| error_sentence(&label, message))
            })
            .collect()
    }

    async fn save(&mut self, scope: &Scope, credential: &str) -> Result<Option<String>> {
        self.submitted = true;
        self.errors.clear();
        self.notice = None;
        let Some(company) = company(scope) else {
            self.notice = Some("Choose a company first.".into());
            return Ok(None);
        };
        self.asked = self
            .draft
            .amounts
            .iter()
            .filter(|(_, a)| !a.trim().is_empty())
            .map(|(id, _)| *id)
            .collect();
        if self.asked.is_empty() {
            self.notice = Some("Enter how much to pay against at least one bill.".into());
            return Ok(None);
        }
        let d = &self.draft;
        let mut input = json!({
            "company_id": company,
            "allocations": self.asked.iter().map(|id| json!({"document_id": id, "amount": d.amounts[id].trim()})).collect::<Vec<_>>(),
        });
        for (key, text) in [
            ("payment_date", &d.date),
            ("supplier_key", &d.party),
            ("reference", &d.reference),
            ("description", &d.description),
        ] {
            if !text.trim().is_empty() {
                input[key] = json!(text.trim());
            }
        }
        if let Some(vendor) = self.party().and_then(|p| p.vendor_id) {
            input["vendor_id"] = json!(vendor);
        }
        if let Ok(id) = d.account.trim().parse::<i64>() {
            input["paid_from_account_id"] = json!(id);
        }
        match self
            .pages
            .write(
                scope,
                credential,
                "save_payment",
                "finance.payables.payment.submit",
                input,
            )
            .await?
        {
            Written::Done(result) => {
                let id = record_id(&result);
                Ok(id.map(|id| format!("{PAYMENTS}/{id}")))
            }
            Written::Forbidden => {
                self.notice = Some("You do not have permission to request payments.".into());
                Ok(None)
            }
            Written::Changed => {
                self.notice = Some(
                    "Nothing was saved because something changed. Check the form and try again."
                        .into(),
                );
                Ok(None)
            }
            Written::Fields(fields) => {
                self.errors = fields
                    .into_iter()
                    .flat_map(|(path, messages)| {
                        messages.into_iter().map(move |m| (path.clone(), m))
                    })
                    .collect();
                let loose: Vec<_> = self
                    .errors
                    .iter()
                    .filter(|(p, _)| self.locate(p).is_none())
                    .map(|(_, m)| m.as_str())
                    .collect();
                if !loose.is_empty() || self.errors.is_empty() {
                    self.notice = Some(if loose.is_empty() {
                        "The request was not saved. Check the form and try again.".into()
                    } else {
                        loose.join(" ")
                    });
                }
                Ok(None)
            }
        }
    }

    pub(super) fn body(&self) -> Result<String> {
        let back = render(BackLink {
            href: PAYMENTS,
            id: "payables-pay-back",
            title: "Back to payments",
            class: "",
        })?;
        if let Some(why) = self.unavailable {
            let mut empty = EmptyState::missing("Nothing to request here", why);
            empty.id = "payables-pay-unavailable".into();
            return Unavailable {
                back,
                empty: render(empty)?,
            }
            .render()
            .map_err(|_| Error::Internal);
        }
        let units = self.units.ok_or(Error::Internal)?;
        let d = &self.draft;
        let field = |key: &str, element: &str, label: &str, value: &str| {
            Field::in_form(PREFIX, key)
                .id(format!("payables-pay-{element}"))
                .label(label)
                .value(value)
                .errors(self.errors_for(&format!("payables-pay-{element}")))
        };
        let details = vec![
            field("date", "date", "Payment date", &d.date)
                .kind(InputKind::Date)
                .required(),
            field("party", "party", "Supplier", &d.party)
                .kind(InputKind::Select)
                .prompt("Choose a supplier")
                .options(
                    self.owed
                        .iter()
                        .map(|p| {
                            Choice::new(
                                format!(
                                    "{} · {} ({} {})",
                                    p.party_key,
                                    p.party_name,
                                    p.total.display(units),
                                    self.currency
                                ),
                                p.party_key.clone(),
                            )
                        })
                        .collect(),
                )
                .required(),
            field("reference", "reference", "Reference", &d.reference)
                .hint("Your bank reference or cheque number, if you have one."),
            field("description", "description", "Description", &d.description),
        ];
        let account = Combobox::new("payables-pay-account", format!("{PREFIX}[account]"))
            .label("Paid from")
            .value(d.account.clone())
            .placeholder("Search accounts")
            .options(
                self.accounts
                    .iter()
                    .map(|a| Choice::new(&a.name, a.id.to_string()).detail(&a.code))
                    .collect(),
            )
            .errors(self.errors_for("payables-pay-account"))
            .hint("The bank, cash or card account the money leaves.");
        let mut rows = Vec::new();
        if let Some(party) = self.party() {
            for doc in &party.documents {
                let element = format!("payables-pay-amount-{}", doc.id);
                rows.push(Row {
                    dom_id: format!("payables-pay-bill-{}", doc.id),
                    bill_id: doc.id,
                    bill: format!("Bill #{}", doc.document_number),
                    detail: format!(
                        "{} · dated {}",
                        f::optional(&doc.party_document_number),
                        doc.document_date
                    ),
                    due: format!("Due {}", doc.due_date),
                    overdue: (doc.days_overdue > 0)
                        .then(|| format!("{} days overdue", doc.days_overdue)),
                    owed: format!(
                        "Owed {} {}",
                        doc.outstanding.display(units),
                        doc.currency_code
                    ),
                    amount: Field::in_form(PREFIX, &format!("amount_{}", doc.id))
                        .id(element.clone())
                        .label("Amount to pay")
                        .value(d.amounts.get(&doc.id).cloned().unwrap_or_default())
                        .attr("inputmode", "decimal")
                        .attr("data-debounce", "400")
                        .errors(self.errors_for(&element)),
                });
            }
        }
        let total = match self.total() {
            Some((total, complete)) if !d.amounts.values().all(|a| a.trim().is_empty()) => {
                Some(render(Facts {
                    id: "payables-pay-total",
                    items: vec![f::fact(
                        if complete {
                            "Total payment"
                        } else {
                            "Total so far"
                        },
                        &format!("{} {}", total.display(units), self.currency),
                    )],
                })?)
            }
            _ => None,
        };
        let summary = if self.submitted && !self.errors.is_empty() {
            let mut summary = ErrorSummary::new("payables-pay-errors");
            for (path, message) in &self.errors {
                if let Some((element, label)) = self.locate(path) {
                    summary.push(element, error_sentence(&label, message));
                }
            }
            Some(render(summary)?)
        } else {
            None
        };
        let notice = self
            .notice
            .as_ref()
            .map(|message| {
                render(Alert {
                    kind: StatusKind::Error,
                    id: "payables-pay-notice",
                    body: Html::text(message),
                    ..Alert::default()
                })
            })
            .transpose()?;
        let button = |id: &str, event: &str, label: &str| {
            render(Button {
                id,
                button_type: "button",
                event,
                body: Html::text(label),
                ..Button::default()
            })
        };
        let none_owed = self.owed.is_empty();
        let chosen = self.party().is_some();
        let fill = if chosen {
            Some((
                button("payables-pay-fill", "fill", "Pay everything owed")?,
                button("payables-pay-clear", "clear", "Clear amounts")?,
            ))
        } else {
            None
        };
        View {
            back,
            summary,
            notice,
            details,
            account,
            rows,
            fill,
            none_owed,
            chosen,
            total,
            save: render(Button {
                id: "payables-pay-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                body: Html::text("Request payment"),
                ..Button::default()
            })?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<LiveBody> {
        let kind = v["type"].as_str().ok_or(Error::Invalid)?;
        let mut redirect = None;
        match kind {
            "edit" => {
                self.read(v);
                self.errors.clear();
                self.notice = None;
                self.reload(scope).await?;
            }
            "fill" => {
                self.errors.clear();
                if let (Some(party), Some(units)) = (self.party(), self.units) {
                    let all: BTreeMap<i64, String> = party
                        .documents
                        .iter()
                        .map(|d| (d.id, d.outstanding.shown(units)))
                        .collect();
                    self.draft.amounts = all;
                }
            }
            "clear" => {
                self.errors.clear();
                self.draft.amounts.clear();
            }
            "save_payment" => {
                self.read(v);
                if self.pages.allowed(scope, SUBMIT_PAYMENTS).await? {
                    redirect = self.save(scope, credential).await?;
                } else {
                    self.notice = Some("You do not have permission to request payments.".into());
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(match redirect {
            Some(to) => LiveBody {
                redirect: Some(to),
                flash: Some((
                    StatusKind::Success,
                    "Payment requested. It will post when approved.".into(),
                )),
                ..LiveBody::default()
            },
            None => LiveBody {
                html: self.body()?,
                ..LiveBody::default()
            },
        })
    }
}

impl LiveState<Scope, Error> for PaymentForm {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => {
                    self.reload(r.scope).await?;
                    Ok(LiveBody {
                        html: self.body()?,
                        ..LiveBody::default()
                    })
                }
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await,
            }
        })
    }
}

struct Row {
    dom_id: String,
    bill_id: i64,
    bill: String,
    detail: String,
    due: String,
    overdue: Option<String>,
    owed: String,
    amount: Field,
}

#[derive(Template)]
#[template(path = "payment_form.html")]
struct View {
    back: Html,
    summary: Option<Html>,
    notice: Option<Html>,
    details: Vec<Field>,
    account: Combobox,
    rows: Vec<Row>,
    fill: Option<(Html, Html)>,
    none_owed: bool,
    chosen: bool,
    total: Option<Html>,
    save: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::page(id="payables-pay", variant=bilibi_ui::PageVariant::Form) %}{% call ui::header(actions=back) %}Request a payment{% endcall %}{{ empty|safe }}{% endcall %}"#,
    ext = "html"
)]
struct Unavailable {
    back: Html,
    empty: Html,
}
