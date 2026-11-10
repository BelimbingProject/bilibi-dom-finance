//! Receivables pages. Bilimbi has none here (`web: nil`); these exist because
//! a person without an agent must do the same daily work (ADR 01). Every read
//! and write goes through a `finance.receivables.*` operation contract with
//! `PageContracts`, the same contract the agent doors call; no page touches
//! the facade or a table. Business meaning follows Belimbing's receivables.
mod actions;
mod aging;
mod company_pick;
mod credits;
mod customer;
mod customers;
mod document;
mod documents;
mod format;
mod issue;
mod lines;
mod opening;
mod owing;
mod payment;
mod payments;
mod settings;
mod write_offs;

use crate::{MODULE, VIEW};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{MountFuture, Page, PageBody, PageFuture, PageRequest},
    Choice, FlashMap, Html, StatusKind, NO_FLASH,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const PAGES: &str = "finance/receivables.pages";
pub(crate) const CUSTOMERS: &str = "/finance/receivables/customers";
/// The drop target of the Add customers dialog (`bilibi_records::drop_target!`).
pub(crate) const CUSTOMER_TARGET: &str = "receivables.customer";

// Companies and contacts dropped on Customers, or carried by "Add to…", open
// the Add customers dialog with them chosen; the dialog commits
// `customer.create` as it does for "+ Customer". The chooser lists many
// parties at once, so the page takes several records per drop.
bilibi_records::drop_target!(
    "finance/receivables",
    bilibi_records::DropTarget {
        id: CUSTOMER_TARGET,
        service: PAGES,
        contract: "finance.receivables.customer.create",
        capability: crate::MANAGE_CUSTOMERS,
        path: CUSTOMERS,
        label: "Add as customer",
        accepts: &["company", "person"],
        max: 20,
    }
);
pub(crate) const INVOICES: &str = "/finance/receivables/invoices";
/// The drop target of the New invoice dialog (`bilibi_records::drop_target!`).
pub(crate) const INVOICE_TARGET: &str = "receivables.invoice";

// A customer's Core company or contact, carried by a list row's actions menu
// (or dropped on Invoices), opens the New invoice dialog with that customer
// chosen; the dialog commits `draft.create` as "+ Invoice" does. A party that
// is not a customer yet gets the page's own notice, not a dialog.
bilibi_records::drop_target!(
    "finance/receivables",
    bilibi_records::DropTarget {
        id: INVOICE_TARGET,
        service: PAGES,
        contract: "finance.receivables.draft.create",
        capability: crate::MANAGE_DOCUMENTS,
        path: INVOICES,
        label: "New invoice",
        accepts: &["company", "person"],
        max: 1,
    }
);
pub(crate) const RECEIPTS: &str = "/finance/receivables/receipts";
const AGING: &str = "/finance/receivables/aging";
pub(crate) const SETTINGS: &str = "/finance/receivables/settings";

/// The most records one page reads before it says the list is cut short. Lists
/// are read through the contracts' cursors and filtered, sorted and paged
/// here, so a page never asks the database for more than this.
pub(super) const READ_LIMIT: usize = 2000;

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<PageContracts>,
    pub plans: Arc<bilibi_agent_api::plans::Plans>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub records: Arc<bilibi_records::Records>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
}

bilibi_menu::contribute!(
    "finance/receivables",
    bilibi_menu::MenuItem::container("finance.group.receivables", "Receivables")
        .under("finance")
        .icon("iconoir-receive-dollars")
        .order(15)
);
macro_rules! menu {
    ($id:literal, $label:literal, $path:expr, $icon:literal, $order:literal) => {
        bilibi_menu::contribute!(
            "finance/receivables",
            bilibi_menu::MenuItem::page($id, $label, $path)
                .company_scoped()
                .icon($icon)
                .under("finance.group.receivables")
                .requires(bilibi_menu::Requirement::Capability(VIEW))
                .order($order)
        );
    };
}
menu!(
    "finance.receivables.customers",
    "Customers",
    CUSTOMERS,
    "iconoir-receive-dollars",
    10
);
menu!(
    "finance.receivables.invoices",
    "Invoices",
    INVOICES,
    "vaadin-invoice",
    20
);
menu!(
    "finance.receivables.receipts",
    "Receipts",
    RECEIPTS,
    "iconoir-receive-dollars",
    30
);
menu!(
    "finance.receivables.aging",
    "Aging",
    AGING,
    "iconoir-receive-dollars",
    40
);
menu!(
    "finance.receivables.settings",
    "Settings",
    SETTINGS,
    "iconoir-receive-dollars",
    50
);

macro_rules! route {
    ($key:literal, $path:expr, $operation:literal) => {
        inventory::submit! { Contribution::new(ROUTES, $key,
        Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
            method: Method::Get, path: $path, service: PAGES,
            operation: $operation, access: Access::Capability(VIEW), framed: true,
        }) }
    };
}
route!("finance/receivables.customers", CUSTOMERS, "customers");
route!(
    "finance/receivables.customer",
    "/finance/receivables/customers/{id}",
    "customer"
);
route!("finance/receivables.invoices", INVOICES, "invoices");
route!(
    "finance/receivables.invoice",
    "/finance/receivables/invoices/{id}",
    "invoice"
);
route!("finance/receivables.receipts", RECEIPTS, "receipts");
route!(
    "finance/receivables.receipt",
    "/finance/receivables/receipts/{id}",
    "receipt"
);
route!("finance/receivables.aging", AGING, "aging");
route!("finance/receivables.settings", SETTINGS, "settings");

// Each live write event (its name is the page event the person raises) names the
// contract it calls; the host refuses any other
// business write from these pages (`base/agent_api` README, page writes).
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, Provenance { module: MODULE, source: file!(), line: line!() }, &PageWrites {
    service: PAGES,
    contracts: &[
        ("preview_credit", "finance.receivables.credit.apply"),
        ("preview_credit", "finance.receivables.credit.refund"),
        ("preview_credit", "finance.receivables.settlement.reverse"),
        ("commit_credit", "finance.receivables.credit.apply"),
        ("commit_credit", "finance.receivables.credit.refund"),
        ("commit_credit", "finance.receivables.settlement.reverse"),

        ("preview_opening", "finance.receivables.opening.import"),
        ("import_opening", "finance.receivables.opening.import"),
        ("preview_opening", "finance.receivables.opening.stage"),
        ("import_opening", "finance.receivables.opening.stage"),

        ("save_write_off", "finance.receivables.write_off.submit"),
        ("save_write_off", "finance.receivables.write_off.approve"),
        ("save_write_off", "finance.receivables.write_off.reject"),
        ("save_write_off", "finance.receivables.write_off.reverse"),
        ("create_customer", "finance.receivables.customer.create"),
        ("save_customer", "finance.receivables.customer.update"),
        ("record_relationship", "core.company.relationship.assign"),
        ("create_draft", "finance.receivables.draft.create"),
        ("create_draft", "finance.receivables.document.issue"),
        ("invoice_order", "finance.receivables.sales_order.invoice"),
        ("save_draft", "finance.receivables.draft.update"),
        ("save_lines", "finance.receivables.draft.update"),
        ("discard", "finance.receivables.draft.discard"),
        ("ask_issue", "finance.receivables.document.issue"),
        ("issue", "finance.receivables.document.issue"),
        ("save_receipt", "finance.receivables.payment.record"),
        ("reverse", "finance.receivables.payment.reverse"),
        ("configure", "finance.receivables.setup.configure"),
        ("save_layout", "finance.receivables.setup.configure"),
        ("confirm_receivable", "finance.receivables.setup.configure"),
        ("save_series", "finance.receivables.series.create"),
        ("save_series", "finance.receivables.series.update"),
        ("save_tax", "finance.receivables.tax_code.create"),
        ("save_tax", "finance.receivables.tax_code.update"),
        ("toggle_series", "finance.receivables.series.update"),
        ("toggle_tax", "finance.receivables.tax_code.update"),
    ],
    pending: Pending::None,
}) }

impl Pages {
    /// The names of the people a page mentions.
    pub(super) async fn people(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Result<bilibi_finance_ledger::People> {
        let client = self.db.get().await?;
        Ok(bilibi_finance_ledger::People::read(&self.directory, &**client, scope, ids).await)
    }

    pub(super) fn party_picker(&self) -> bilibi_finance_ledger::PartyPickerContext {
        bilibi_finance_ledger::PartyPickerContext {
            records: self.records.clone(),
            contracts: self.contracts.clone(),
            service: PAGES,
            role: "customer",
        }
    }

    /// The company a person works in now: the session's current company.
    pub(super) fn company(scope: &Scope) -> Option<i64> {
        scope.current_company().map(|id| id.get())
    }

    pub(super) async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let c = self.db.get().await?;
        Ok(bilibi_authz::decide(&**c, scope, capability).await?.allowed)
    }

    /// The name of the current company, so a page can say whose settings it
    /// changes (the shell's account menu reads it the same way). Display
    /// only: a company the directory cannot name is "Company unavailable",
    /// never a refusal.
    pub(super) async fn company_name(&self, scope: &Scope) -> String {
        let Some(id) = scope.current_company() else {
            return "Company unavailable".into();
        };
        let Ok(c) = self.db.get().await else {
            return "Company unavailable".into();
        };
        self.parties
            .organisation_name(&**c, scope, id)
            .await
            .ok()
            .flatten()
            .map_or_else(|| "Company unavailable".into(), |o| o.name)
    }

    pub(super) async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let c = self.db.get().await?;
        let locale = self.locales.resolve(&**c, scope, None).await?.locale;
        self.display.display(&**c, scope, None, locale).await
    }

    /// One read contract for the current company; the answer is its `result`.
    pub(super) async fn read(&self, scope: &Scope, key: &str, mut input: Value) -> Result<Value> {
        let company = Self::company(scope).ok_or(Error::Unauthorized)?;
        input["company_id"] = json!(company);
        self.read_core(scope, key, input).await
    }

    /// A read contract of another module, with the input as given (Core
    /// names its own company); the answer is its `result`.
    pub(super) async fn read_core(&self, scope: &Scope, key: &str, input: Value) -> Result<Value> {
        let mut answer = self
            .contracts
            .read(scope, key, &input)
            .await
            .map_err(read_error)?;
        Ok(answer["result"].take())
    }

    /// Every record of a cursor-paged list contract, up to `READ_LIMIT`.
    /// `cursor` names the input field and the answer field that carry it:
    /// `("after_code", "next_after_code")` or `("after", "next_after")`.
    pub(super) async fn read_all<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        key: &str,
        input: Value,
        cursor: (&str, &str),
    ) -> Result<(Vec<T>, bool)> {
        let mut records = Vec::new();
        let mut input = input;
        input["page_size"] = json!(100);
        loop {
            let page = self.read(scope, key, input.clone()).await?;
            for record in page["records"].as_array().into_iter().flatten() {
                records.push(serde_json::from_value(record.clone()).map_err(|_| Error::Internal)?);
            }
            match &page[cursor.1] {
                Value::Null => return Ok((records, false)),
                next if records.len() >= READ_LIMIT => {
                    let _ = next;
                    return Ok((records, true));
                }
                next => input[cursor.0] = next.clone(),
            }
        }
    }

    /// Today's date in the company's own zone, for a date field's starting value.
    pub(super) async fn today(&self, scope: &Scope) -> String {
        match self.display(scope).await {
            Ok(display) => aging::today(&display.timezone),
            Err(_) => String::new(),
        }
    }

    /// How the company's book writes an amount.
    pub(super) async fn figures(&self, scope: &Scope) -> format::Figures {
        match self
            .read(scope, "finance.receivables.book_figures.get", json!({}))
            .await
        {
            Ok(answer) => match (
                answer["book"]["currency_code"].as_str(),
                answer["book"]["minor_units"].as_u64(),
            ) {
                (Some(currency), Some(units)) => format::Figures::book(currency, units as u32),
                _ => format::Figures::default(),
            },
            Err(_) => format::Figures::default(),
        }
    }

    /// The company's accounts, for choosing one. An account the person cannot
    /// read (no ledger view) shows by its number alone.
    pub(super) async fn accounts(&self, scope: &Scope) -> Vec<Account> {
        self.chart(scope).await.unwrap_or_default()
    }

    /// The company's accounts, or `None` when the chart could not be read
    /// (no ledger view, or a failed read), which is not the same as an empty one.
    pub(super) async fn chart(&self, scope: &Scope) -> Option<Vec<Account>> {
        self.read_all(
            scope,
            "finance.ledger.account.list",
            json!({}),
            ("after_code", "next_after_code"),
        )
        .await
        .ok()
        .map(|(accounts, _)| accounts)
    }

    pub(super) fn call<'a>(
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract,
        }
    }

    /// One reviewed write by the person's own hand (a form or a confirmation
    /// they just answered). The reply is the committed record, a refusal worded
    /// for the page, or the field errors to show beside the form.
    pub(super) async fn write(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: Value,
    ) -> Result<Written> {
        let Some(company) = Self::company(scope) else {
            return Ok(Written::Refused(DENIED.into()));
        };
        input["company_id"] = json!(company);
        self.write_core(scope, credential, event, contract, input)
            .await
    }

    /// A reviewed write of another module's contract on the person's behalf,
    /// with the input as given: Core names its own company, so none is added.
    pub(super) async fn write_core(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Written> {
        let answer = self
            .contracts
            .submit(&Self::call(scope, credential, event, contract), input)
            .await;
        Self::written(contract, event, answer)
    }

    fn written(
        contract: &str,
        event: &str,
        answer: std::result::Result<Value, ContractError>,
    ) -> Result<Written> {
        match answer {
            Ok(mut answer) => Ok(Written::Done(answer["result"]["record"].take())),
            Err(ContractError::InvalidInput(fields)) => Ok(Written::Invalid(fields)),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                Ok(Written::Refused(DENIED.into()))
            }
            Err(ContractError::Backend(Error::Conflict)) => Ok(Written::Refused(
                "The details changed. Review them again before saving.".into(),
            )),
            Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => Ok(
                Written::Refused("That record no longer exists. Reload the page.".into()),
            ),
            // The host signs the browser in again, or sends it to confirm.
            Err(ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => Err(error),
            Err(error) => {
                eprintln!("receivables page: {contract} from {event} failed: {error:?}");
                Ok(Written::Refused(
                    "That could not be saved. Please try again.".into(),
                ))
            }
        }
    }
}

/// A refusal for a missing permission, worded for what the person was doing.
pub(super) fn worded(message: String, denied: &str) -> String {
    if message == DENIED {
        denied.into()
    } else {
        message
    }
}

pub(super) const DENIED: &str = "You do not have permission to do that.";

/// What a write came to.
pub(super) enum Written {
    Done(Value),
    /// Field errors keyed by the input's path (`lines.0.quantity`, `base`).
    Invalid(BTreeMap<String, Vec<String>>),
    Refused(String),
}

fn read_error(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        ContractError::Forbidden => Error::Forbidden,
        ContractError::NotFound => Error::NotFound,
        ContractError::InvalidInput(_) => Error::Invalid,
        _ => Error::Internal,
    }
}

/// A ledger account as the pickers need it.
#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Account {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub account_type: String,
    pub active: bool,
    #[serde(default)]
    pub is_group: bool,
}

impl Account {
    /// An active account, never an account group: what a picker may offer.
    pub(super) fn postable(&self) -> bool {
        self.active && !self.is_group
    }

    pub(super) fn label(&self) -> String {
        format!("{} {}", self.code, self.name)
    }

    /// The account name first, then its code in a sentence;
    /// `account_html` is the styled form.
    pub(super) fn choice_label(&self) -> String {
        format!("{} ({})", self.name, self.code)
    }

    /// The account as a combobox choice: the name, with its code drawn
    /// muted and italic after it.
    pub(super) fn choice(&self) -> Choice {
        Choice::new(&self.name, self.id.to_string()).detail(&self.code)
    }
}

/// An account's label from a list, or a neutral label when unreadable.
pub(super) fn account_label(accounts: &[Account], id: i64) -> String {
    accounts
        .iter()
        .find(|a| a.id == id)
        .map_or_else(|| "Account unavailable".into(), Account::label)
}

#[derive(Template)]
#[template(
    source = r#"<span>{{ name }}</span> <span class="italic text-ink-muted">{{ code }}</span>"#,
    ext = "html"
)]
struct AccountLabel<'a> {
    name: &'a str,
    code: &'a str,
}

/// An account as a read value: its name, then its code in muted italics
/// (BelimbingProject/bilibi-dom-finance#63), or a neutral label when unreadable.
pub(super) fn account_html(accounts: &[Account], id: i64) -> Result<Html> {
    match accounts.iter().find(|a| a.id == id) {
        Some(a) => render(AccountLabel {
            name: &a.name,
            code: &a.code,
        }),
        None => Ok(Html::text("Account unavailable")),
    }
}

pub(super) fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// A page's one flash message, shown by kind.
#[derive(Default)]
pub(super) struct Flash(pub Option<(StatusKind, String)>);

impl Flash {
    pub(super) fn set(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.0 = Some((kind, message.into()));
    }
    pub(super) fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
    /// The shared dismiss event; only the kind it names is cleared.
    pub(super) fn dismiss(&mut self, event: &Value) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| event["key"].as_str() == Some(kind.name()))
        {
            self.0 = None;
        }
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "customers" => Some("Customers"),
            "customer" => Some("Customer"),
            "invoices" => Some("Invoices"),
            "invoice" => Some("Invoice"),
            "receipts" => Some("Receipts"),
            "receipt" => Some("Receipt"),
            "aging" => Some("Receivables aging"),
            "settings" => Some("Receivables settings"),
            _ => None,
        }
    }

    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&r)?;
            let html = state.first(scope).await?;
            Ok(PageBody {
                html,
                live: true,
                title: state.title(),
            })
        })
    }

    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&r)?;
            state.mounted(scope).await?;
            Ok(Some(state.into_live()))
        })
    }
}

/// Every page state behind one name, so `render` and `mount` build the same.
enum State {
    Customers(Box<customers::Customers>),
    Customer(Box<customer::Customer>),
    Invoices(Box<documents::Documents>),
    Invoice(Box<document::Document>),
    Receipts(Box<payments::Payments>),
    Receipt(Box<payment::Payment>),
    Aging(Box<aging::Aging>),
    Settings(Box<settings::Settings>),
}

impl Pages {
    fn state(&self, r: &PageRequest<'_, Scope>) -> Result<State> {
        let pages = self.clone();
        Ok(match r.operation {
            "customers" => State::Customers(Box::new(customers::Customers::new(
                pages, r.query, r.framed,
            )?)),
            "customer" => {
                State::Customer(Box::new(customer::Customer::new(pages, r.path, r.framed)?))
            }
            "invoices" => State::Invoices(Box::new(documents::Documents::new(
                pages, r.query, r.framed,
            )?)),
            "invoice" => State::Invoice(Box::new(document::Document::new(pages, r.path, r.query)?)),
            "receipts" => {
                State::Receipts(Box::new(payments::Payments::new(pages, r.query, r.framed)?))
            }
            "receipt" => State::Receipt(Box::new(payment::Payment::new(pages, r.path, r.query)?)),
            "aging" => State::Aging(Box::new(aging::Aging::new(pages, r.query)?)),
            "settings" => State::Settings(Box::new(settings::Settings::new(pages)?)),
            _ => return Err(Error::NotFound),
        })
    }
}

impl State {
    /// The first render: load, then draw.
    async fn first(&mut self, scope: &Scope) -> Result<String> {
        match self {
            State::Customers(s) => s.first(scope).await,
            State::Customer(s) => s.first(scope).await,
            State::Invoices(s) => s.first(scope).await,
            State::Invoice(s) => s.first(scope).await,
            State::Receipts(s) => s.first(scope).await,
            State::Receipt(s) => s.first(scope).await,
            State::Aging(s) => s.first(scope).await,
            State::Settings(s) => s.first(scope).await,
        }
    }

    /// A page that opens on records an address carried does so again when it
    /// goes live, so its first update keeps what the first render showed.
    async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        match self {
            State::Customers(s) => s.mounted(scope).await,
            State::Invoices(s) => s.mounted(scope).await,
            State::Invoice(s) => s.mounted(scope).await,
            State::Receipt(s) => s.mounted(scope).await,
            State::Receipts(s) => s.mounted(scope).await,
            _ => Ok(()),
        }
    }

    fn title(&self) -> Option<String> {
        match self {
            State::Customer(s) => s.title(),
            State::Invoice(s) => s.title(),
            State::Receipt(s) => s.title(),
            _ => None,
        }
    }

    fn into_live(self) -> Box<dyn bilibi_ui::page::LiveState<Scope, Error>> {
        match self {
            State::Customers(s) => s,
            State::Customer(s) => s,
            State::Invoices(s) => s,
            State::Invoice(s) => s,
            State::Receipts(s) => s,
            State::Receipt(s) => s,
            State::Aging(s) => s,
            State::Settings(s) => s,
        }
    }
}
