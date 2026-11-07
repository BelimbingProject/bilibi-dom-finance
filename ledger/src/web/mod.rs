//! The ledger's pages. Bilimbi's ledger has no pages (`web: nil`), so these are
//! this port's own: each reads through the ledger's read contracts and writes
//! through its write contracts, the same ones agents call over `/api/v1` and
//! `/mcp` (ADR 01, `PageContracts`). A page never calls the `Ledger` facade for
//! something a contract covers.
mod account;
mod accounts;
mod book;
mod books;
mod entry;
mod group;
mod groups;
mod journal;
mod journal_period;
mod journals;
mod opening;
mod trial;
mod view;
mod years;

use crate::{MANAGE_ACCOUNTS, MANAGE_PERIODS, MODULE, VIEW};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{Clock, DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    Alert, EmptyState, FlashGroup, FlashMap, Html, StatusKind, NO_FLASH,
};
use jiff::{civil::Date, Timestamp};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const PAGES: &str = "finance/ledger.pages";

/// The menu group Reports opened for the bookkeeper's everyday pages.
const GROUP: &str = "finance.group.bookkeeping";

/// Finance page identity, most specific first: tax, invoice, receivables,
/// payables, assets, then money.
/// The glyphs are vendored in the Platform (`apps/base/ui/assets/vendor/product-icons/`).
const ICON_MONEY: &str = "gmdi-money-r";

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
}

impl Pages {
    /// The names of the people a page mentions (`crate::People`).
    pub(crate) async fn people(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Result<crate::People> {
        let client = self.db.get().await?;
        Ok(crate::People::read(&self.directory, &**client, scope, ids).await)
    }
}

macro_rules! route {
    ($key:literal, $path:literal, $operation:literal) => { route!($key, $path, $operation, bilibi_module_registry::contributions::CompanyScope::Company); };
    ($key:literal, $path:literal, $operation:literal, $coverage:expr) => {
        inventory::submit! { Contribution::new(ROUTES, $key,
        Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: $coverage,
            method: Method::Get, path: $path, service: PAGES,
            operation: $operation, access: Access::Capability(VIEW), framed: true,
        }) }
    };
}
route!(
    "finance/ledger.book",
    "/finance/ledger/books/{id}",
    "book",
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
route!(
    "finance/ledger.books",
    "/finance/ledger/books",
    "books",
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
route!(
    "finance/ledger.accounts",
    "/finance/ledger/accounts",
    "accounts",
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
route!(
    "finance/ledger.account",
    "/finance/ledger/accounts/{id}",
    "account"
);
route!(
    "finance/ledger.groups",
    "/finance/ledger/account-groups",
    "groups",
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
route!(
    "finance/ledger.group",
    "/finance/ledger/account-groups/{id}",
    "group"
);
route!(
    "finance/ledger.journals",
    "/finance/ledger/journals",
    "journals"
);
route!(
    "finance/ledger.journal",
    "/finance/ledger/journals/{id}",
    "journal"
);
route!(
    "finance/ledger.years",
    "/finance/ledger/fiscal-years",
    "years",
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
route!(
    "finance/ledger.opening-balances",
    "/finance/ledger/opening-balances",
    "opening"
);
route!(
    "finance/ledger.trial-balance",
    "/finance/ledger/trial-balance",
    "trial"
);

macro_rules! menu {
    ($id:literal, $label:literal, $path:literal, $order:literal) => {
        menu!(
            $id,
            $label,
            $path,
            $order,
            bilibi_module_registry::contributions::CompanyScope::Company
        );
    };
    ($id:literal, $label:literal, $path:literal, $order:literal, $coverage:expr) => {
        bilibi_menu::contribute!(
            "finance/ledger",
            bilibi_menu::MenuItem {
                company_scope: $coverage,
                ..bilibi_menu::MenuItem::page($id, $label, $path)
            }
            .under(GROUP)
            .icon(ICON_MONEY)
            .requires(bilibi_menu::Requirement::Capability(VIEW))
            .order($order)
        );
    };
}
menu!(
    "finance.ledger.accounts",
    "Chart of accounts",
    "/finance/ledger/accounts",
    5,
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
menu!(
    "finance.ledger.groups",
    "Account groups",
    "/finance/ledger/account-groups",
    6,
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
menu!(
    "finance.ledger.journals",
    "Journals",
    "/finance/ledger/journals",
    8
);
menu!(
    "finance.ledger.years",
    "Fiscal years",
    "/finance/ledger/fiscal-years",
    9,
    bilibi_module_registry::contributions::CompanyScope::Tenant
);
menu!(
    "finance.ledger.trial-balance",
    "Trial balance",
    "/finance/ledger/trial-balance",
    10
);
menu!(
    "finance.ledger.books",
    "Company books",
    "/finance/ledger/books",
    13,
    bilibi_module_registry::contributions::CompanyScope::Tenant
);

// Every write these pages make is a ledger contract (ADR 01).
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, Provenance { module: MODULE, source: file!(), line: line!() }, &PageWrites {
    service: PAGES,
    contracts: &[
        ("review_publication", "finance.ledger.chart_template.save"),
        ("commit_publication", "finance.ledger.chart_template.save"),
        ("review_publication", "finance.ledger.chart_template.propose"),
        ("commit_publication", "finance.ledger.chart_template.propose"),
        ("review_open", "finance.ledger.book.open"),
        ("open_book", "finance.ledger.book.open"),
        ("create", "finance.ledger.account.create"),
        ("save_field", "finance.ledger.account.update"),
        ("ask_deactivate", "finance.ledger.account.update"),
        ("ask_designate", "finance.ledger.account.update"),
        ("deactivate", "finance.ledger.account.update"),
        ("designate", "finance.ledger.account.update"),
        ("activate", "finance.ledger.account.update"),
        ("ask_template", "finance.ledger.chart_template.apply"),
        ("choose_sections", "finance.ledger.chart_template.apply"),
        ("apply_template", "finance.ledger.chart_template.apply"),
        ("create_group", "finance.ledger.account_group.create"),
        ("save_group_field", "finance.ledger.account_group.update"),
        ("reverse", "finance.ledger.entry.reverse"),
        ("record_late_review", "finance.ledger.entry.review_late"),
        ("preview_opening", "finance.ledger.opening_balances.submit"),
        ("submit_opening", "finance.ledger.opening_balances.submit"),
        ("submit_journal", "finance.ledger.journal.submit"),
        ("ask_approve", "finance.ledger.journal.approve"),
        ("approve", "finance.ledger.journal.approve"),
        ("reject", "finance.ledger.journal.reject"),
        ("withdraw", "finance.ledger.journal.withdraw"),
        ("replace_journal", "finance.ledger.journal.replace"),
        ("open_year", "finance.ledger.fiscal_year.open"),
        ("confirm_open_year", "finance.ledger.fiscal_year.open"),
        ("choose", "finance.ledger.period.close"),
        ("choose", "finance.ledger.period.lock"),
        ("choose", "finance.ledger.fiscal_year.close"),
        ("choose", "finance.ledger.fiscal_year.lock"),
        ("do_confirm", "finance.ledger.period.close"),
        ("do_confirm", "finance.ledger.period.lock"),
        ("do_confirm", "finance.ledger.fiscal_year.close"),
        ("do_confirm", "finance.ledger.fiscal_year.lock"),
        ("do_reopen", "finance.ledger.period.reopen"),
        ("do_reopen", "finance.ledger.fiscal_year.reopen"),
    ],
    pending: Pending::None,
}) }

/// The write capability each event needs, so a control the reader may not use
/// is not drawn (Bilimbi hides it; it does not explain).
pub(crate) fn capability_for(event: &str) -> &'static str {
    match event {
        "create_account" => MANAGE_ACCOUNTS,
        "reopen_period" | "reopen_year" => crate::REOPEN_PERIODS,
        "lock_period" | "lock_year" => crate::LOCK_PERIODS,
        _ => MANAGE_PERIODS,
    }
}

impl Pages {
    /// The company the signed-in person works in now: the session's current
    /// company, as every Finance page reads it (Bilibi ADR 09). It chooses
    /// where a page looks and grants nothing; the contract proves the reach.
    pub(crate) fn company(scope: &Scope) -> Option<i64> {
        scope.current_company().map(|id| id.get())
    }

    pub(crate) async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.db.get().await?;
        Ok(bilibi_authz::effective_capabilities(&**client, scope)
            .await?
            .contains(capability))
    }

    pub(crate) async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        match self
            .display
            .display(&**client, scope, scope.current_company(), locale)
            .await
        {
            // An archived company still shows its history; fall back to the
            // person's own clock, as the other Finance pages do.
            Err(Error::Forbidden) => self.display.display(&**client, scope, None, locale).await,
            result => result,
        }
    }

    pub(crate) async fn today(&self, scope: &Scope) -> Result<Date> {
        let display = self.display(scope).await?;
        Ok(Clock::resolve(&display, None, Timestamp::now()).today)
    }

    pub(crate) fn call<'a>(
        &self,
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

    /// A read contract about the current company, answered as a page reads it.
    pub(crate) async fn read(&self, scope: &Scope, key: &str, input: Value) -> Result<Read> {
        let Some(company) = Self::company(scope) else {
            return Ok(Read::NoCompany);
        };
        self.read_in(scope, company, key, input).await
    }

    /// A read contract about a named company. The contract proves the reach
    /// and the capability again, so a company the person cannot reach is
    /// `Read::Denied`, never data.
    pub(crate) async fn read_in(
        &self,
        scope: &Scope,
        company: i64,
        key: &str,
        mut input: Value,
    ) -> Result<Read> {
        input["company_id"] = json!(company);
        match self.contracts.read(scope, key, &input).await {
            Ok(value) => Ok(Read::Found(value["result"].clone())),
            Err(error) => Ok(match failure(error)? {
                Failure::Denied => Read::Denied,
                Failure::Refused(refusal) => Read::Refused(refusal.message()),
                Failure::Changed | Failure::Failed => Read::Failed,
            }),
        }
    }

    /// A reviewed form: the contract's preview and commit in one transaction.
    pub(crate) async fn submit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<std::result::Result<Value, Failure>> {
        let Some(company) = Self::company(scope) else {
            return Ok(Err(Failure::Denied));
        };
        self.submit_in(scope, company, credential, event, contract, input)
            .await
    }

    pub(crate) async fn submit_in(
        &self,
        scope: &Scope,
        company: i64,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: Value,
    ) -> Result<std::result::Result<Value, Failure>> {
        input["company_id"] = json!(company);
        let call = self.call(scope, credential, event, contract);
        match self.contracts.submit(&call, input).await {
            Ok(value) => Ok(Ok(value)),
            Err(error) => Ok(Err(failure(error)?)),
        }
    }

    /// The review half of a confirmation: the contract's own summary of what
    /// will change.
    pub(crate) async fn review(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<std::result::Result<Review, Failure>> {
        let Some(company) = Self::company(scope) else {
            return Ok(Err(Failure::Denied));
        };
        self.review_in(scope, company, credential, event, contract, input)
            .await
    }

    pub(crate) async fn review_in(
        &self,
        scope: &Scope,
        company: i64,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: Value,
    ) -> Result<std::result::Result<Review, Failure>> {
        input["company_id"] = json!(company);
        let call = self.call(scope, credential, event, contract);
        match self.contracts.preview(&call, input.clone()).await {
            Ok(value) => {
                let result = &value["results"][0];
                if result["valid"].as_bool() == Some(true) {
                    return Ok(Ok(Review {
                        input,
                        facts: result["preview"]["result"]["changes"].clone(),
                    }));
                }
                Ok(Err(Failure::Refused(Refusal::from_review(result))))
            }
            Err(error) => Ok(Err(failure(error)?)),
        }
    }

    /// The current company's name, so a page can say whose book it shows.
    /// Display only: a company the directory cannot name is "Company
    /// unavailable", never a refusal.
    pub(crate) async fn company_name(&self, scope: &Scope) -> Result<String> {
        let Some(company) = Self::company(scope) else {
            return Ok(String::new());
        };
        let id = bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::NotFound)?;
        let client = self.db.get().await?;
        Ok(self
            .parties
            .organisation_name(&**client, scope, id)
            .await?
            .map(|company| company.name)
            .unwrap_or_else(|| "Company unavailable".into()))
    }

    /// Name an explicit chart target only from the shared reachable list.
    /// Core's organisation-name read also names directory-only companies;
    /// that is broader than the companies an accountant can work in.
    pub(crate) async fn company_name_in(&self, scope: &Scope, company: i64) -> Result<String> {
        if Some(company) == Self::company(scope) {
            return self.company_name(scope).await;
        }
        Ok(self
            .companies(scope)
            .await?
            .into_iter()
            .find(|c| c.id == company)
            .map(|c| c.name)
            .unwrap_or_else(|| "Company unavailable".into()))
    }

    /// The companies the person may work in, by name: what the top bar's
    /// switcher offers. Listing a company grants nothing; every read and
    /// write below still proves its own target. Archived companies are left
    /// out, as everywhere else.
    pub(crate) async fn companies(&self, scope: &Scope) -> Result<Vec<Company>> {
        let client = self.db.get().await?;
        let mut list: Vec<Company> = self
            .parties
            .reachable_organisations(&**client, scope)
            .await?
            .into_iter()
            .filter(|company| !company.archived)
            .map(|company| Company {
                id: company.id.get(),
                name: company.name,
            })
            .collect();
        list.sort_by_key(|company| company.name.to_lowercase());
        Ok(list)
    }

    pub(crate) async fn commit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        review: Review,
    ) -> Result<std::result::Result<Value, Failure>> {
        let call = self.call(scope, credential, event, contract);
        match self.contracts.commit(&call, review.input).await {
            Ok(value) => Ok(Ok(value)),
            Err(error) => Ok(Err(failure(error)?)),
        }
    }
}

impl Pages {
    /// The whole chart, by code: the contract pages 100 accounts at a time
    /// and the pages follow its cursor (up to 5,000 accounts).
    pub(crate) async fn chart(
        &self,
        scope: &Scope,
    ) -> Result<std::result::Result<Vec<view::Account>, Read>> {
        let Some(company) = Self::company(scope) else {
            return Ok(Err(Read::NoCompany));
        };
        self.chart_in(scope, company).await
    }

    pub(crate) async fn chart_in(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<std::result::Result<Vec<view::Account>, Read>> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..50 {
            let mut input = json!({"page_size": 100});
            if let Some(code) = &after {
                input["after_code"] = json!(code);
            }
            match self
                .read_in(scope, company, "finance.ledger.account.list", input)
                .await?
            {
                Read::Found(value) => {
                    all.extend(
                        view::parse::<Vec<view::Account>>(value["records"].clone())
                            .unwrap_or_default(),
                    );
                    after = value["next_after_code"].as_str().map(str::to_owned);
                    if after.is_none() {
                        break;
                    }
                }
                other => return Ok(Err(other)),
            }
        }
        Ok(Ok(all))
    }

    /// One company's chart balances and group totals as of today, or `None`
    /// when its book or figures cannot be read through the contract.
    pub(crate) async fn chart_figures_in(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<Option<view::ChartBalances>> {
        let today = self.today(scope).await?;
        Ok(
            match self
                .read_in(
                    scope,
                    company,
                    "finance.ledger.chart_balance.get",
                    json!({"as_of": today.to_string()}),
                )
                .await?
            {
                Read::Found(value) => view::parse(value),
                _ => None,
            },
        )
    }

    /// The company's book, its controls, and what refused when it cannot be read.
    pub(crate) async fn book(
        &self,
        scope: &Scope,
    ) -> Result<std::result::Result<view::BookState, Read>> {
        let Some(company) = Self::company(scope) else {
            return Ok(Err(Read::NoCompany));
        };
        self.book_in(scope, company).await
    }

    /// Every fiscal year of a named company, oldest first: the contract pages
    /// 100 at a time and the pages follow its cursor.
    pub(crate) async fn years_in(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<std::result::Result<Vec<view::Year>, Read>> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..20 {
            let mut input = json!({"page_size": 100});
            if let Some(date) = &after {
                input["after_start_date"] = json!(date);
            }
            match self
                .read_in(scope, company, "finance.ledger.fiscal_year.list", input)
                .await?
            {
                Read::Found(value) => {
                    all.extend(
                        view::parse::<Vec<view::Year>>(value["records"].clone())
                            .unwrap_or_default(),
                    );
                    after = value["next_after_start_date"].as_str().map(str::to_owned);
                    if after.is_none() {
                        break;
                    }
                }
                other => return Ok(Err(other)),
            }
        }
        Ok(Ok(all))
    }

    /// The book of a named company; see [`Pages::read_in`].
    pub(crate) async fn book_in(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<std::result::Result<view::BookState, Read>> {
        Ok(
            match self
                .read_in(scope, company, "finance.ledger.book.get", json!({}))
                .await?
            {
                Read::Found(value) => match view::parse(value) {
                    Some(state) => Ok(state),
                    None => Err(Read::Failed),
                },
                other => Err(other),
            },
        )
    }
}

/// A company the person may work in, named for a choice.
#[derive(Clone, Debug)]
pub(crate) struct Company {
    pub id: i64,
    pub name: String,
}

/// What a read contract answered.
pub(crate) enum Read {
    Found(Value),
    NoCompany,
    Denied,
    Refused(String),
    Failed,
}

/// A reviewed confirmation waiting for the person's answer.
#[derive(Clone)]
pub(crate) struct Review {
    pub input: Value,
    pub facts: Value,
}

/// Why a write did not happen, in the terms a page needs.
pub(crate) enum Failure {
    /// The person may not do this.
    Denied,
    /// The ledger refused the values or the state, with its own words.
    Refused(Refusal),
    /// The record changed after it was shown, or the confirmation lapsed.
    Changed,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Refusal {
    pub fields: BTreeMap<String, Vec<String>>,
}
impl Refusal {
    fn from_review(result: &Value) -> Self {
        let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(map) = result["error"]["fields"].as_object() {
            for (field, messages) in map {
                fields.insert(
                    field.clone(),
                    messages
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m.as_str().map(str::to_owned))
                        .collect(),
                );
            }
        } else if result["error"]["code"].as_str() == Some("missing_capability") {
            fields.insert("base".into(), vec![DENIED.into()]);
        }
        Self { fields }
    }
    /// The messages that belong to no field of the form, or all of them when
    /// the form has no field to show them beside.
    pub(crate) fn message(&self) -> String {
        let mut messages: Vec<String> = Vec::new();
        for (field, list) in &self.fields {
            for message in list {
                if field == "base" || field.is_empty() {
                    messages.push(message.clone());
                } else {
                    messages.push(error_text(field, message));
                }
            }
        }
        if messages.is_empty() {
            "That could not be done. Check the details and try again.".into()
        } else {
            messages
                .into_iter()
                .map(|m| {
                    if m.ends_with(['.', '!', '?']) {
                        m
                    } else {
                        format!("{m}.")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
    /// The messages of one field (a `lines.0.debit` path counts for `lines`).
    pub(crate) fn field(&self, name: &str) -> Vec<String> {
        self.fields
            .iter()
            .filter(|(path, _)| *path == name || path.ends_with(&format!(".{name}")))
            .flat_map(|(_, messages)| messages.iter().cloned())
            .collect()
    }
    /// What the form should say in its own alert: the messages that belong to
    /// no field, and those of a field the form shows no place for (a total
    /// that does not balance is a refusal of `lines`, not of one input).
    pub(crate) fn alert(&self, placed: &[&str]) -> Vec<String> {
        let mut messages = self.fields.get("base").cloned().unwrap_or_default();
        for (path, list) in &self.fields {
            let last = path.rsplit('.').next().unwrap_or(path);
            if path != "base" && !placed.contains(&last) {
                messages.extend(list.iter().map(|m| error_text(path, m)));
            }
        }
        messages
    }
}

pub(crate) const DENIED: &str = "You do not have permission to do that.";

/// One field's refusal as a sentence: "Code has already been taken."
pub(crate) fn error_text(field: &str, message: &str) -> String {
    // A refusal of the lines as a whole, or of one line ("a line has either a
    // debit or a credit"), already reads as a sentence.
    let label = if field.ends_with("lines") || message.starts_with("a line ") {
        String::new()
    } else {
        field_label(field)
    };
    bilibi_ui::error_sentence(&label, message)
}

fn field_label(path: &str) -> String {
    let last = path.rsplit('.').next().unwrap_or(path);
    match last {
        "entry_date" => "Date".into(),
        "csv" => "Opening balances CSV".into(),
        "account_id" => "Account".into(),
        "currency_code" => "Currency".into(),
        "minor_units" => "Decimal places".into(),
        "start_date" => "Start date".into(),
        "end_date" => "End date".into(),
        other => {
            let text = other.replace('_', " ");
            let mut chars = text.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

/// Sorts a contract error into what a page does about it. Only a signed-out
/// browser or a missing sign-in code ends the page; everything else is a
/// message on it.
fn failure(error: ContractError) -> Result<Failure> {
    Ok(match error {
        ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. })) => {
            return Err(error);
        }
        ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => Failure::Denied,
        ContractError::InvalidInput(fields) => Failure::Refused(Refusal {
            fields: fields.into_iter().collect(),
        }),
        ContractError::Backend(Error::Conflict) => Failure::Changed,
        ContractError::PlanRejected(results) => Failure::Refused(Refusal::from_review(&results[0])),
        ContractError::NotFound | ContractError::Backend(Error::NotFound) => {
            Failure::Refused(Refusal {
                fields: BTreeMap::from([(
                    "base".into(),
                    vec!["That record no longer exists.".into()],
                )]),
            })
        }
        _ => Failure::Failed,
    })
}

impl Failure {
    /// The one sentence a page shows for it.
    pub(crate) fn text(&self, what: &str) -> String {
        match self {
            Self::Denied => DENIED.into(),
            Self::Refused(refusal) => refusal.message(),
            Self::Changed => format!(
                "{what} changed while you were looking at it. Reload the page and try again."
            ),
            Self::Failed => format!("{what} was not saved. Try again."),
        }
    }
}

/// The page-wide message (one at a time) that every page here shows in the
/// corner, dismissed by the shared `clear-flash` event.
#[derive(Default)]
pub(crate) struct Flash(Option<(StatusKind, String)>);
impl Flash {
    pub(crate) fn success(&mut self, text: impl Into<String>) {
        self.0 = Some((StatusKind::Success, text.into()));
    }
    pub(crate) fn error(&mut self, text: impl Into<String>) {
        self.0 = Some((StatusKind::Error, text.into()));
    }
    /// The browser dismissed one kind; an older success must not clear a
    /// newer refusal.
    pub(crate) fn dismiss(&mut self, payload: &Value) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| payload["key"].as_str() == Some(kind.name()))
        {
            self.0 = None;
        }
    }
    pub(crate) fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = text,
                StatusKind::Error => flash.error = text,
                StatusKind::Warning => flash.warning = text,
                StatusKind::Info => flash.info = text,
            }
        }
        flash
    }
}

/// A page of the ledger around its content: the corner message, then the page.
pub(crate) fn shell(
    id: &str,
    variant: bilibi_ui::PageVariant,
    flash: &Flash,
    mut body: Html,
) -> Result<String> {
    if flash
        .map()
        .error
        .contains("/finance/reports/approval-exceptions")
    {
        body.push(&render(bilibi_ui::Button {
            id: "ledger-approval-settings",
            href: "/finance/reports/approval-exceptions",
            body: Html::text("Approval exceptions"),
            ..bilibi_ui::Button::default()
        })?);
    }
    let mut html = render(FlashGroup {
        id: "flash-group",
        flash: flash.map(),
    })?;
    html.push(&render(bilibi_ui::Page {
        id,
        variant,
        body,
        ..bilibi_ui::Page::default()
    })?);
    Ok(html.to_string())
}

/// What a page shows when the person has no company to read.
pub(crate) fn no_company(id: &str) -> Result<Html> {
    render(
        EmptyState::missing(
            "No current company",
            "Choose a current company in the top bar, or ask an administrator for access.",
        )
        .id(id),
    )
}

/// What a page shows when a read was refused.
pub(crate) fn unavailable(id: &str, what: &str, refused: &Read) -> Result<Html> {
    match refused {
        Read::Denied => render(EmptyState::forbidden(format!("view {what}")).id(id)),
        Read::Refused(text) => render(Alert {
            id,
            kind: StatusKind::Error,
            class: "mb-3",
            body: Html::text(text),
        }),
        _ => render(
            EmptyState::missing(
                "This could not be shown",
                "Reload the page. If it keeps failing, ask your administrator.",
            )
            .id(id),
        ),
    }
}

/// Dialog actions sit at the end of the row. The shared form starts its
/// action line at the left, so this row fills that line.
pub(crate) fn dialog_actions(buttons: Html) -> Result<Html> {
    render(DialogActions { buttons })
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct DialogActions {
    buttons: Html,
}

pub(crate) fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

/// A timestamp from a contract answer, in the person's own display mode.
pub(crate) fn instant(display: &DisplayContext, iso: &str, id: &str) -> Result<Html> {
    let Ok(at) = iso.parse::<Timestamp>() else {
        return Ok(Html::text(iso));
    };
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    render(TimeView {
        id,
        instant: bilibi_ui::flex_table::Instant::new(
            &rendered.iso,
            &rendered.text,
            &rendered.company,
            &rendered.utc,
            rendered.mode.as_str(),
        ),
    })
}

#[derive(Template)]
#[template(
    source = "{% import \"flex_table.html\" as table %}{% call table::instant(id=id, i=instant) %}{% endcall %}",
    ext = "html"
)]
struct TimeView<'a> {
    id: &'a str,
    instant: bilibi_ui::flex_table::Instant,
}

/// Tells the workspace which record this page shows, so another tile can follow it.
#[derive(Template)]
#[template(
    source = r#"{% import "workspace.html" as workspace %}{% call workspace::announce(kind=kind, record_id=record_id) %}{% endcall %}"#,
    ext = "html"
)]
pub(crate) struct Announce<'a> {
    pub kind: &'a str,
    pub record_id: &'a str,
}

/// A figure in a table cell: right-aligned digits that line up.
#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>"#,
    ext = "html"
)]
pub(crate) struct Figure {
    pub text: String,
}

/// A plain link to an entry's page.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
pub(crate) struct EntryLink<'a> {
    pub id: i64,
    pub label: &'a str,
}

/// An account named for a person: its name, then its code in muted italics,
/// both escaped. The plain-text form for a select option is
/// `view::account_label`.
#[derive(Template)]
#[template(
    source = r#"{{ name }} <span class="font-normal italic text-ink-muted">{{ code }}</span>"#,
    ext = "html"
)]
pub(crate) struct AccountName<'a> {
    pub name: &'a str,
    pub code: &'a str,
}

pub(crate) fn account_name(account: &view::Account) -> Result<Html> {
    render(AccountName {
        name: &account.name,
        code: &account.code,
    })
}

/// A plain link to an account's page.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/accounts/{{ id }}" class="text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
pub(crate) struct AccountLink<'a> {
    pub id: i64,
    pub label: &'a str,
}

/// A plain link to the page of the record that owns an entry.
#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
pub(crate) struct RecordLink<'a> {
    pub href: &'a str,
    pub label: &'a str,
}

/// The page and name of the record that owns an entry ("invoice", "bill",
/// "receipt"), when its module has one; a link only, never a read of the
/// sibling's records.
pub(crate) fn owner_page(module: &str, record: &str) -> Option<(String, &'static str)> {
    let (kind, id) = record.split_once(':')?;
    let id: i64 = id.parse().ok().filter(|id| *id > 0)?;
    let (path, noun) = match (module, kind) {
        ("finance/receivables", "document") => ("/finance/receivables/invoices", "invoice"),
        ("finance/receivables", "payment") => ("/finance/receivables/receipts", "receipt"),
        ("finance/payables", "document") => ("/finance/payables/bills", "bill"),
        ("finance/payables", "payment") => ("/finance/payables/payments", "payment"),
        _ => return None,
    };
    Some((format!("{path}/{id}"), noun))
}

/// A plain link to a manual journal's page.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/{{ id }}" class="text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
pub(crate) struct JournalLink<'a> {
    pub id: i64,
    pub label: &'a str,
}

/// A list filter that picks one coded record (an account, a currency): a
/// searchable combobox, name first and code muted, sized like the toolbar's
/// selects and posted on the same `filters` event. `TODO(platform)`: the
/// shared `FilterToolbar` has no combobox control yet, so it sits beside it.
pub(crate) fn filter_combobox(
    id: &str,
    key: &str,
    label: &str,
    value: &str,
    all: &str,
    options: Vec<bilibi_ui::Choice>,
) -> Result<Html> {
    let mut choices = vec![bilibi_ui::Choice::new(all, "")];
    choices.extend(options);
    render(bilibi_ui::Form {
        id: &format!("{id}-filter"),
        event: "filters",
        change: true,
        class: "ui-compact-controls mb-2 w-full sm:w-fit sm:min-w-36",
        body: render(
            bilibi_ui::Combobox::new(
                id,
                format!("{}[{key}]", bilibi_ui::list_state::FILTERS_FORM),
            )
            .aria_label(label)
            .placeholder(all)
            .wrapper_class("mb-0")
            .value(value)
            .options(choices),
        )?,
        actions: Html::EMPTY,
    })
}

pub(crate) fn figure(text: &str) -> Result<Html> {
    render(Figure {
        text: text.to_owned(),
    })
}

/// The module that owns an entry, in the words a bookkeeper uses.
pub(crate) fn source_label(module: &str) -> String {
    match module {
        "opening" => "Opening".into(),
        "finance/payables" => "Payables".into(),
        "finance/receivables" => "Receivables".into(),
        "finance/reconciliation" => "Bank reconciliation".into(),
        "finance/payroll" => "Payroll".into(),
        "finance/fixed_assets" => "Fixed assets".into(),
        "finance/einvoice" => "E-invoicing".into(),
        "finance/sst" => "Tax return".into(),
        other => other.rsplit('/').next().unwrap_or(other).replace('_', " "),
    }
}

pub(crate) fn id_param(values: &std::collections::HashMap<String, String>) -> Result<i64> {
    values
        .get("id")
        .and_then(|id| id.parse().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::NotFound)
}

pub(crate) fn event_type(event: &Value) -> Result<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "book" => "Book",
            "books" => "Company books",
            "accounts" => "Chart of accounts",
            "account" => "Account",
            "groups" => "Account groups",
            "group" => "Account group",
            "journals" => "Journals",
            "journal" => "Journal",
            "years" => "Fiscal years",
            "trial" => "Trial balance",
            "opening" => "Opening balances",
            _ => return None,
        })
    }

    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&r)?;
            let body = state
                .update(bilibi_ui::page::LiveRequest {
                    scope,
                    credential: "",
                    input: bilibi_ui::page::LiveInput::Refresh,
                })
                .await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(&r)?)) })
    }
}

impl Pages {
    fn state(&self, r: &PageRequest<'_, Scope>) -> Result<Box<dyn LiveState<Scope, Error>>> {
        let pages = self.clone();
        let company = if matches!(r.operation, "account" | "group" | "groups") {
            target_company(r.query, r.scope)?
        } else {
            None
        };
        Ok(match r.operation {
            "book" => Box::new(book::Book::new(pages, id_param(r.path)?)),
            "books" => Box::new(books::Books::new(pages, r.query, r.framed)?),
            "accounts" => Box::new(accounts::Accounts::new(pages, r.query, r.framed)?),
            "account" => Box::new(account::Account::new(pages, id_param(r.path)?, company)),
            "groups" => Box::new(groups::Groups::new(pages, r.query, r.framed, company)?),
            "group" => Box::new(group::Group::new(pages, id_param(r.path)?, company)),
            "journals" => Box::new(journals::Journals::new(pages, r.query, r.framed)?),
            "journal" => {
                let record = r.path.get("id").ok_or(Error::NotFound)?;
                if let Some(entry_id) = record.strip_prefix("entry-") {
                    let id = entry_id
                        .parse::<i64>()
                        .ok()
                        .filter(|id| *id > 0)
                        .ok_or(Error::NotFound)?;
                    let reverse = r.query.get("reverse").map(String::as_str) == Some("1");
                    Box::new(entry::Entry::new(pages, id, reverse))
                } else {
                    Box::new(journal::Journal::new(pages, id_param(r.path)?))
                }
            }
            "years" => Box::new(years::Years::new(pages, r.query)?),
            "trial" => Box::new(trial::Trial::new(pages, r.query)),
            "opening" => Box::new(opening::Opening::new(pages, r.query)?),
            _ => return Err(Error::NotFound),
        })
    }
}

/// An explicit chart drill-through target, or the current company on a direct
/// visit. This selects an address only; each contract still proves its target.
fn target_company(
    query: &std::collections::HashMap<String, String>,
    scope: Option<&Scope>,
) -> Result<Option<i64>> {
    match query.get("company") {
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or(Error::NotFound),
        None => Ok(scope.and_then(Pages::company)),
    }
}
