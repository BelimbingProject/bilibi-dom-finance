//! Management reports, module `finance/reports` (Bilimbi
//! `Bilimbi.Finance.Reports`): a company's figures read live from its general
//! ledger: profit and loss for date ranges, the balance sheet and the cash
//! position as of dates, headline figures for a period against the one before
//! it, and the entries behind any account's figure. They are management
//! reports for the company's own use, not statutory financial statements. The
//! module also retains source-period conversion schedules as evidence without
//! turning them into postings, tax returns, payroll runs or remittances.
//!
//! Every report figure comes from the [`Ledger`] facade; this module stores
//! issued snapshots and source-period evidence and reads none of the ledger's
//! tables. Every function takes the sealed [`Scope`] it was handed and an
//! explicit company id, and authorizes
//! the ledger's view capability (`finance.ledger.view`) for the scope's actor
//! and that company when it runs, through Core Company's `authorize_work_target`: a
//! missing grant, or a sibling company without tenant-wide reach, is
//! [`ReportsError::Unauthorized`], and a company the scope cannot see is
//! [`ReportsError::CompanyNotFound`]. Issuing a report asks for
//! [`ISSUE`] as well.
//!
//! A report names `through_entry_number`, the last ledger entry it counts, and
//! every column of one report is read at that same number, so passing it back
//! reproduces the postings counted. It does not freeze the chart, account
//! names or the layout: cash membership and section totals can still change.
//! [`Reports::issue_report`] stores a durable issued copy, and
//! [`Reports::get_issued_report`] reads it back without recomputing anything.
//!
//! Accounts are grouped by their type and, within a type, by the sections the
//! company configures in the [`SECTIONS`] setting (see `layout.rs`). Section
//! headings and account choices are the company's data; a report reads them as
//! they are when it runs.
mod agent_operations;
mod auditor_pack;
mod configuration;
mod contributions;
mod controls;
mod conversion;
pub use conversion::ConversionPins;
mod conversion_operations;
mod conversion_page;
mod issued;
mod layout;
mod pack_owner;
mod setup;
mod setup_page;
mod source_period;
mod source_period_web;
mod statement;
mod web;

pub use auditor_pack::AuditorPack;
pub use bilibi_finance_ledger::{PostingEvent, PostingSubscription};
pub use configuration::SECTIONS;
pub use contributions::{CAPABILITIES, EXPORT, ISSUE, SIGN_OFF};
pub use issued::{EvidencePage, IssuedReport};
pub use setup::{SetupArea, SetupOverview, SetupState};

use bilibi_company::WriteError;
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction};
use bilibi_finance_ledger::{
    Account, AccountType, Entry, EntryQuery, Ledger, LedgerError, MinorUnits, Money, TotalsQuery,
};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, Span};
use serde::Serialize;
use serde_json::{json, Value};
use statement::Column as Totals;
use std::{collections::BTreeMap, sync::Arc};

/// The descriptor id.
pub const MODULE: &str = "finance/reports";
/// The registry service id of the [`Reports`] facade.
pub const SERVICE: &str = "finance/reports.reports";
/// The capability every report asks for: the ledger's view capability.
pub const VIEW: &str = "finance.ledger.view";

/// Bump whenever the interpretation of issued content or a report's
/// calculation changes.
const IMPLEMENTATION_VERSION: &str = "2";
/// A report reads one to four columns.
const MAX_COLUMNS: usize = 4;
/// Entries an account's activity lists at a time when no limit is given.
const DEFAULT_ENTRY_LIMIT: u32 = 100;

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &[
            "base/agent_api", "base/authz", "base/database", "base/datetime", "base/locale", "base/menu",
            "base/party_ids", "base/principal_directory", "base/settings", "base/tenancy", "base/ui",
            "core/company", "base/artifacts", "finance/ledger",
            "finance/attachments", "finance/receivables", "finance/payables",
            "finance/reconciliation", "finance/einvoice", "finance/payroll",
            "finance/sst", "finance/fixed_assets",
        ],
        migrations: &[
            ("finance-reports-001-issued", include_str!("../migrations/001-issued.sql")),
            ("finance-reports-002-source-period", include_str!("../migrations/002-source-period.sql")),
            ("finance-reports-002-conversion-signoffs", include_str!("../migrations/002-conversion-signoffs.sql")),
            ("finance-reports-003-conversion-signoff-attestation", include_str!("../migrations/003-conversion-signoff-attestation.sql")),
        ],
        services: &[Service { id: SERVICE, factory: |services| { let reports = Arc::new(Reports {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            mutations: services.get("base/database.mutations")?,
            settings: services.get("base/settings.store")?,
            ledger: services.get(bilibi_finance_ledger::SERVICE)?,
            artifacts: services.get("base/artifacts.store")?,
            sources: auditor_pack::Sources {
                attachments: services.get(bilibi_finance_attachments::SERVICE)?,
                receivables: services.get(bilibi_finance_receivables::SERVICE)?,
                payables: services.get(bilibi_finance_payables::SERVICE)?,
                reconciliation: services.get(bilibi_finance_reconciliation::SERVICE)?,
                einvoice: services.get(bilibi_finance_einvoice::SERVICE)?,
                payroll: services.get(bilibi_finance_payroll::SERVICE)?,
                sst: services.get(bilibi_finance_sst::SERVICE)?,
                fixed_assets: services.get(bilibi_finance_fixed_assets::SERVICE)?,
            },
        }); agent_operations::bind(reports.clone())?; Ok(reports) } }, Service { id: "finance/reports.pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                reports: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                directory: services.get(bilibi_finance_ledger::PEOPLE)?,
            }))))
        } }, Service { id: "finance/reports.section-pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                reports: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                directory: services.get(bilibi_finance_ledger::PEOPLE)?,
            }))))
        } }, Service { id: "finance/reports.control-pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(controls::Pages {
                parties: services.get("base/tenancy.parties")?,
                reports: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                directory: services.get(bilibi_finance_ledger::PEOPLE)?,
            }))))
        } }, Service { id: source_period_web::PAGES, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(source_period_web::Pages {
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            }))))
        } }, Service { id: setup_page::PAGES, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(setup_page::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                parties: services.get("base/tenancy.parties")?,
            }))))
        } }, Service { id: conversion_page::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(conversion_page::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                parties: services.get("base/tenancy.parties")?,
            }))))
        } }, Service { id: "finance/reports.downloads", factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(auditor_pack::Downloads { reports: services.get(SERVICE)? }))))
        } }],
    }
}

pub type Result<T> = std::result::Result<T, ReportsError>;

/// Why a call did not happen (Bilimbi's refusal atoms).
#[derive(Debug, thiserror::Error)]
pub enum ReportsError {
    /// A system scope, a missing grant, or a sibling company without
    /// tenant-wide reach.
    #[error("Access denied")]
    Unauthorized,
    /// A company the scope cannot see: missing, deleted or another tenant's.
    #[error("Not found")]
    CompanyNotFound,
    #[error("This company is archived and read-only")]
    CompanyArchived,
    #[error("The company's book is not open")]
    BookNotOpen,
    #[error("Not found")]
    AccountNotFound,
    #[error("Not found")]
    ReportNotFound,
    /// No periods or dates, more than four, a period that ends before it
    /// starts, a negative entry number, or a limit, cursor or dataset the
    /// ledger refuses.
    #[error("Invalid option")]
    InvalidOption,
    /// The ledger's posting relay could not listen.
    #[error("Posting notices are unavailable")]
    NoticesUnavailable,
    #[error("The cash account choices are unavailable. Try again.")]
    CashAccountsUnavailable,
    /// Any other refusal of the ledger's facade, as the ledger words it.
    #[error(transparent)]
    Ledger(LedgerError),
    #[error("The company changed while the pack was being generated")]
    SnapshotChanged,
    #[error("Archive exceeds the configured size limit")]
    InvalidSize,
    #[error("Evidence control totals do not agree")]
    ControlTotalsMismatch,
    #[error("Source integrity failure")]
    SourceIntegrityFailure,
    #[error("Artifact storage: {0}")]
    Artifact(&'static str),
    /// A database or capture failure; never a refusal.
    #[error(transparent)]
    Failed(#[from] Error),
}
impl From<LedgerError> for ReportsError {
    fn from(error: LedgerError) -> Self {
        match error {
            LedgerError::Unauthorized => Self::Unauthorized,
            LedgerError::CompanyNotFound => Self::CompanyNotFound,
            LedgerError::CompanyArchived => Self::CompanyArchived,
            LedgerError::BookNotOpen => Self::BookNotOpen,
            LedgerError::AccountNotFound => Self::AccountNotFound,
            LedgerError::InvalidOption => Self::InvalidOption,
            LedgerError::NoticesUnavailable => Self::NoticesUnavailable,
            LedgerError::Failed(error) => Self::Failed(error),
            other => Self::Ledger(other),
        }
    }
}

/// A date range, both days inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    pub from: Date,
    pub to: Date,
}

/// One column of a report: a period for flows, a date for balances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Column {
    Period {
        #[serde(serialize_with = "as_text")]
        from: Date,
        #[serde(serialize_with = "as_text")]
        to: Date,
    },
    AsOf {
        #[serde(serialize_with = "as_text")]
        as_of: Date,
    },
}

/// What every report names: the company, its book's currency, the last entry
/// counted and its columns, in the order the periods or dates were given (the
/// first is the reporting column, the rest its comparatives).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Header {
    pub company_id: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub through_entry_number: i64,
    pub columns: Vec<Column>,
}

/// One account of a report with one amount per column, on its type's natural
/// side (on the cash position, debits less credits).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    pub account_id: i64,
    pub code: String,
    pub name: String,
    pub account_type: AccountType,
    pub parent_id: Option<i64>,
    pub active: bool,
    pub amounts: Vec<Money>,
}
/// A configured section's rows and totals; `name` is `None` for the accounts
/// no section covers, listed after the named sections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Section {
    pub name: Option<String>,
    pub rows: Vec<Row>,
    pub totals: Vec<Money>,
}
/// One account type's sections and totals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Group {
    pub account_type: AccountType,
    pub sections: Vec<Section>,
    pub totals: Vec<Money>,
}

/// Income then expense, and income less expense per column. Year-end closing
/// entries are left out, so a closed year shows its activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfitAndLoss {
    #[serde(flatten)]
    pub header: Header,
    /// Codes the layout lists that the chart does not have.
    pub unmatched_codes: Vec<String>,
    pub groups: Vec<Group>,
    pub net_income: Vec<Money>,
}
/// Assets, liabilities and equity. `earnings` is income less expense not yet
/// closed into an equity account; it is counted in equity's totals, so
/// `total_assets` equals `total_liabilities_and_equity`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BalanceSheet {
    #[serde(flatten)]
    pub header: Header,
    pub unmatched_codes: Vec<String>,
    pub groups: Vec<Group>,
    pub earnings: Vec<Money>,
    pub total_assets: Vec<Money>,
    pub total_liabilities_and_equity: Vec<Money>,
}
/// Every account the cash sections cover, by section, and the total. Without
/// configured cash sections it lists nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CashPosition {
    #[serde(flatten)]
    pub header: Header,
    pub unmatched_codes: Vec<String>,
    pub sections: Vec<Section>,
    pub totals: Vec<Money>,
}
/// Headline figures for a period and the one before it, each a two-value
/// list: flows over each period, balances as of each period's last day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Overview {
    #[serde(flatten)]
    pub header: Header,
    pub income: Vec<Money>,
    pub expense: Vec<Money>,
    pub net_income: Vec<Money>,
    pub assets: Vec<Money>,
    pub liabilities: Vec<Money>,
    pub equity: Vec<Money>,
    pub cash: Vec<Money>,
}

/// Bounds of [`Reports::account_activity`].
#[derive(Clone, Debug, Default)]
pub struct ActivityQuery {
    /// Entry dates, both inclusive.
    pub from: Option<Date>,
    pub to: Option<Date>,
    /// The report's entry number, so the activity lists exactly the entries
    /// its figure counted.
    pub through_entry_number: Option<i64>,
    /// Leaves out year-end closing entries and their reversals, as a profit
    /// and loss figure does.
    pub exclude_closing: bool,
    /// 1 to 500, 100 when absent.
    pub limit: Option<u32>,
    /// The `(entry_date, id)` of the last entry already read.
    pub after: Option<(Date, i64)>,
}
/// An entry behind an account's figure, with all its lines and the account's
/// own debit and credit in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityEntry {
    #[serde(flatten)]
    pub entry: Entry,
    pub debit: Money,
    pub credit: Money,
}
/// One account's debits and credits over a window and the entries behind
/// them, in date order. `more` is true when the page came back full; the next
/// page (which may be empty) is read by passing the last entry's date and id
/// as `after`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AccountActivity {
    pub company_id: i64,
    pub account: Account,
    #[serde(serialize_with = "as_optional_text")]
    pub from: Option<Date>,
    #[serde(serialize_with = "as_optional_text")]
    pub to: Option<Date>,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub debits: Money,
    pub credits: Money,
    pub entries: Vec<ActivityEntry>,
    pub more: bool,
}

/// What [`Reports::issue_report`] freezes: a statement with the periods or
/// dates its live read takes, or the ledger's trial balance as of a date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    ProfitAndLoss(Vec<Period>),
    BalanceSheet(Vec<Date>),
    CashPosition(Vec<Date>),
    TrialBalance(Date),
    // TODO(finance/receivables, finance/payables): Bilimbi also issues the
    // aged receivables and aged payables, under each module's own view
    // capability; they arrive with those modules' aging reports.
}
impl Issue {
    /// The stored `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ProfitAndLoss(_) => "profit_and_loss",
            Self::BalanceSheet(_) => "balance_sheet",
            Self::CashPosition(_) => "cash_position",
            Self::TrialBalance(_) => "trial_balance",
        }
    }
}

/// The period of the same length that ends the day before `period` starts. A
/// period of whole calendar months gives the same number of whole months
/// before it, so a quarter compares with the previous quarter; any other
/// period gives the same number of days. A period at the calendar's edge is
/// `InvalidOption`.
pub fn previous_period(period: Period) -> Result<Period> {
    let Period { from, to } = period;
    let before = from.yesterday().map_err(|_| ReportsError::InvalidOption)?;
    let start = if from.day() == 1 && to == to.last_of_month() {
        let months = (i32::from(to.year()) - i32::from(from.year())) * 12 + i32::from(to.month())
            - i32::from(from.month())
            + 1;
        from.checked_sub(Span::new().months(months))
    } else {
        let days = from
            .until(to)
            .map_err(|_| ReportsError::InvalidOption)?
            .get_days()
            + 1;
        from.checked_sub(Span::new().days(days))
    };
    Ok(Period {
        from: start.map_err(|_| ReportsError::InvalidOption)?,
        to: before,
    })
}

/// The facade, resolved from the registry as [`SERVICE`].
pub struct Reports {
    db: Db,
    mutations: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    ledger: Arc<Ledger>,
    artifacts: Arc<bilibi_artifacts::Artifacts>,
    sources: auditor_pack::Sources,
}

/// One live read: the ledger's columns at one entry number, the chart and the
/// layout they are arranged by.
struct Read {
    company_id: i64,
    currency_code: String,
    minor_units: MinorUnits,
    through_entry_number: i64,
    accounts: Vec<Account>,
    columns: Vec<Totals>,
    layout: Value,
    cash_defaults: Option<Vec<i64>>,
}
impl Read {
    fn header(&self, columns: Vec<Column>) -> Header {
        Header {
            company_id: self.company_id,
            currency_code: self.currency_code.clone(),
            minor_units: self.minor_units,
            through_entry_number: self.through_entry_number,
            columns,
        }
    }
    /// One report's sections, each account's section and the unmatched codes.
    fn arrange(&self, report: &str) -> Arranged {
        let sections = layout::sections(&self.layout, report);
        let (mut membership, unmatched) = layout::assign(&self.accounts, &sections);
        if report == "cash_position" {
            if let Some(ids) = &self.cash_defaults {
                membership.retain(|id, _| ids.contains(id));
            }
        }
        Arranged {
            sections,
            membership,
            unmatched,
        }
    }
}
struct Arranged {
    sections: Vec<layout::Section>,
    membership: BTreeMap<i64, usize>,
    unmatched: Vec<String>,
}
impl Arranged {
    /// What an issued statement stores of its layout: every covered account's
    /// section, the effective sections and the unmatched codes.
    fn to_json(&self) -> Value {
        json!({
            "account_sections": self.membership.iter().map(|(id, index)| (id.to_string(), json!(index))).collect::<serde_json::Map<_, _>>(),
            "sections": self.sections,
            "unmatched_codes": self.unmatched,
        })
    }
}

impl Reports {
    /// Profit and loss for each period (one to four; the first is the
    /// reporting period, the rest its comparatives): income and expense
    /// groups with their sections and rows, and `net_income` per column.
    pub async fn profit_and_loss(
        &self,
        scope: &Scope,
        company_id: i64,
        periods: &[Period],
        through_entry_number: Option<i64>,
    ) -> Result<ProfitAndLoss> {
        self.authorize(scope, company_id, VIEW).await?;
        Ok(self
            .profit_and_loss_read(scope, company_id, periods, through_entry_number)
            .await?
            .0)
    }

    /// The balance sheet as of each date (one to four).
    pub async fn balance_sheet(
        &self,
        scope: &Scope,
        company_id: i64,
        dates: &[Date],
        through_entry_number: Option<i64>,
    ) -> Result<BalanceSheet> {
        self.authorize(scope, company_id, VIEW).await?;
        Ok(self
            .balance_sheet_read(scope, company_id, dates, through_entry_number)
            .await?
            .0)
    }

    /// The cash position as of each date (one to four): every account the
    /// company's cash sections cover, with its balance as debits less
    /// credits, by section, and the total.
    pub async fn cash_position(
        &self,
        scope: &Scope,
        company_id: i64,
        dates: &[Date],
        through_entry_number: Option<i64>,
    ) -> Result<CashPosition> {
        self.authorize(scope, company_id, VIEW).await?;
        Ok(self
            .cash_position_read(scope, company_id, dates, through_entry_number)
            .await?
            .0)
    }

    /// Headline figures for `period` and [`previous_period`] of it: income,
    /// expense and net income over each period, and assets, liabilities,
    /// equity and cash as of each period's last day.
    pub async fn overview(
        &self,
        scope: &Scope,
        company_id: i64,
        period: Period,
        through_entry_number: Option<i64>,
    ) -> Result<Overview> {
        self.authorize(scope, company_id, VIEW).await?;
        let current = periods_checked(&[period])?[0];
        let previous = previous_period(current)?;
        let windows = vec![
            flow(current),
            flow(previous),
            balance(current.to),
            balance(previous.to),
        ];
        let read = self
            .read(scope, company_id, windows, through_entry_number)
            .await?;
        let (flows, stocks) = read.columns.split_at(2);
        let profit = read.arrange("profit_and_loss");
        let cash = read.arrange("cash_position");
        let (groups, net_income) =
            statement::profit_and_loss(&read.accounts, &profit.sections, &profit.membership, flows);
        let sheet = statement::balance_sheet(&read.accounts, &[], &BTreeMap::new(), stocks);
        let (_, cash) =
            statement::cash_position(&read.accounts, &cash.sections, &cash.membership, stocks);
        Ok(Overview {
            header: read.header(
                [current, previous]
                    .map(|p| Column::Period {
                        from: p.from,
                        to: p.to,
                    })
                    .to_vec(),
            ),
            income: groups[0].totals.clone(),
            expense: groups[1].totals.clone(),
            net_income,
            assets: sheet.groups[0].totals.clone(),
            liabilities: sheet.groups[1].totals.clone(),
            equity: sheet.groups[2].totals.clone(),
            cash,
        })
    }

    /// The entries behind one account's figure: the account, its debits and
    /// credits over the window, and those entries in date order with all
    /// their lines and the account's own debit and credit in each. Given a
    /// report's window and `through_entry_number` it lists exactly the
    /// entries the figure counted, and its debits and credits equal the
    /// figure. `AccountNotFound` for an account of another company.
    pub async fn account_activity(
        &self,
        scope: &Scope,
        company_id: i64,
        account_id: i64,
        query: &ActivityQuery,
    ) -> Result<AccountActivity> {
        self.authorize(scope, company_id, VIEW).await?;
        let limit = query.limit.unwrap_or(DEFAULT_ENTRY_LIMIT);
        let account = self
            .ledger
            .list_accounts(scope, company_id)
            .await?
            .into_iter()
            .find(|account| account.id == account_id && !account.is_group)
            .ok_or(ReportsError::AccountNotFound)?;
        let totals = self
            .ledger
            .account_totals(
                scope,
                company_id,
                &TotalsQuery {
                    from: query.from,
                    to: query.to,
                    through_entry_number: query.through_entry_number,
                    exclude_closing: query.exclude_closing,
                },
            )
            .await?;
        let entries = self
            .ledger
            .list_entries(
                scope,
                company_id,
                &EntryQuery {
                    from: query.from,
                    to: query.to,
                    limit: Some(limit),
                    after: query.after,
                    account_id: Some(account.id),
                    through_entry_number: Some(totals.through_entry_number),
                    exclude_closing: query.exclude_closing,
                },
            )
            .await?;
        let (debits, credits) = totals
            .rows
            .iter()
            .find(|row| row.account.id == account.id)
            .map_or((Money::ZERO, Money::ZERO), |row| (row.debits, row.credits));
        let more = entries.len() == limit as usize;
        Ok(AccountActivity {
            company_id,
            from: totals.from,
            to: totals.to,
            through_entry_number: totals.through_entry_number,
            currency_code: totals.currency_code,
            minor_units: totals.minor_units,
            debits,
            credits,
            entries: entries
                .into_iter()
                .map(|entry| {
                    let own = entry
                        .lines
                        .iter()
                        .filter(|line| line.account_id == account.id);
                    let (debit, credit) = own.fold((Money::ZERO, Money::ZERO), |(d, c), line| {
                        (d + line.debit, c + line.credit)
                    });
                    ActivityEntry {
                        entry,
                        debit,
                        credit,
                    }
                })
                .collect(),
            account,
            more,
        })
    }

    /// The company's posting notices ([`Ledger::subscribe_postings`]), so a
    /// live report reads again when the ledger changes. A notice carries no
    /// figures; on [`PostingEvent::Refresh`] read again as well.
    pub async fn subscribe(&self, scope: &Scope, company_id: i64) -> Result<PostingSubscription> {
        Ok(self.ledger.subscribe_postings(scope, company_id).await?)
    }

    /// Issues an immutable report ([`ISSUE`] and the source report's view
    /// capability): its figures, the chart and layout they were arranged by,
    /// each covered account's section, the issuer and a time the database
    /// stamps. The issuing transaction holds the company's lock, which every
    /// ledger write takes, so the figures, chart and book it reads describe
    /// one state of the ledger; the stored content uses string keys, exact
    /// decimal strings and ISO dates. An archived company is
    /// `CompanyArchived`.
    pub async fn issue_report(
        &self,
        scope: &Scope,
        company_id: i64,
        issue: &Issue,
        through_entry_number: Option<i64>,
    ) -> Result<IssuedReport> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                ISSUE,
                "finance.reports.issue_report",
            )
            .await?;
        let (content, chart, layout, membership, through) = match issue {
            Issue::ProfitAndLoss(periods) => {
                let (report, read, arranged) = self
                    .profit_and_loss_read(scope, company_id, periods, through_entry_number)
                    .await?;
                (
                    json(&report)?,
                    read.accounts,
                    read.layout,
                    arranged.to_json(),
                    read.through_entry_number,
                )
            }
            Issue::BalanceSheet(dates) => {
                let (report, read, arranged) = self
                    .balance_sheet_read(scope, company_id, dates, through_entry_number)
                    .await?;
                (
                    json(&report)?,
                    read.accounts,
                    read.layout,
                    arranged.to_json(),
                    read.through_entry_number,
                )
            }
            Issue::CashPosition(dates) => {
                let (report, read, arranged) = self
                    .cash_position_read(scope, company_id, dates, through_entry_number)
                    .await?;
                (
                    json(&report)?,
                    read.accounts,
                    read.layout,
                    arranged.to_json(),
                    read.through_entry_number,
                )
            }
            Issue::TrialBalance(as_of) => {
                let report = self
                    .ledger
                    .trial_balance(scope, company_id, *as_of, through_entry_number)
                    .await?;
                let chart = self.ledger.list_accounts(scope, company_id).await?;
                let layout = self.layout(scope, company_id).await?;
                let through = report.through_entry_number;
                (json(&report)?, chart, layout, json!({}), through)
            }
        };
        let new = issued::NewIssued {
            tenant_id: scope.tenant(),
            company_id,
            kind: issue.kind(),
            implementation_version: IMPLEMENTATION_VERSION,
            through_entry_number: through,
            content,
            chart: json(&chart)?,
            layout: json!({ "configured_sections": layout }),
            membership,
            issued_by_user_id: scope.user(),
            issued_by_impersonator_id: scope.impersonator().map(|operator| operator.user()),
        };
        let stored = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let stored = issued::insert(sql, &new).await?;
                    let change = Change {
                        kind: "finance.reports.issued".into(),
                        id: stored.id,
                        old: Value::Null,
                        new: issued::summary(&stored),
                    };
                    Ok((stored, vec![change]))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// The stored issued copy (`finance.ledger.view`); it never recomputes a
    /// figure or a label. Conversion copies also require every source view.
    /// `ReportNotFound` for a missing id or another
    /// company's.
    pub async fn get_issued_report(
        &self,
        scope: &Scope,
        company_id: i64,
        id: i64,
    ) -> Result<IssuedReport> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let report = issued::find(&**client, scope.tenant(), company_id, id)
            .await?
            .ok_or(ReportsError::ReportNotFound)?;
        if report.kind == conversion::KIND {
            self.conversion_access(&**client, scope, company_id, VIEW)
                .await?;
        }
        Ok(report)
    }

    /// The datasets [`Reports::evidence_page`] exports, one per table this
    /// module owns.
    pub fn evidence_datasets() -> Vec<&'static str> {
        issued::DATASETS
            .iter()
            .copied()
            .chain(["source_period"])
            .collect()
    }

    /// One ID-ordered evidence page of `dataset` with explicit fields, rows
    /// and next cursor (`finance.ledger.view`), for the auditor's pack.
    /// `limit` is 1 to 500 (500 when absent) and `after` the last id of the
    /// previous page; an unknown dataset or an option out of range is
    /// `InvalidOption`.
    pub async fn evidence_page(
        &self,
        scope: &Scope,
        company_id: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.evidence_page_on(&**client, scope, company_id, dataset, limit, after)
            .await
    }

    /// Authorized evidence on the caller's snapshot; never opens another connection.
    pub async fn evidence_page_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        authorize(client, scope, company_id, VIEW).await?;
        if dataset == "source_period" {
            authorize(client, scope, company_id, bilibi_finance_sst::VIEW).await?;
            authorize(client, scope, company_id, bilibi_finance_payroll::VIEW).await?;
            return source_period::evidence_page(client, scope.tenant(), company_id, limit, after)
                .await;
        }
        issued::page_options(dataset, limit, after, None)?;
        if dataset == "conversion_signoff" {
            self.conversion_access(client, scope, company_id, VIEW)
                .await?;
            return issued::page(
                client,
                scope.tenant(),
                company_id,
                dataset,
                limit,
                after,
                None,
            )
            .await;
        }
        let selected =
            issued::select_issued_page(client, scope.tenant(), company_id, limit, after, None)
                .await?;
        if selected.kinds.iter().any(|kind| kind == conversion::KIND) {
            self.conversion_access(client, scope, company_id, VIEW)
                .await?;
        }
        issued::selected_issued_page(client, scope.tenant(), company_id, selected).await
    }

    // Reading

    async fn profit_and_loss_read(
        &self,
        scope: &Scope,
        company_id: i64,
        periods: &[Period],
        through: Option<i64>,
    ) -> Result<(ProfitAndLoss, Read, Arranged)> {
        let periods = periods_checked(periods)?;
        let read = self
            .read(
                scope,
                company_id,
                periods.iter().map(|p| flow(*p)).collect(),
                through,
            )
            .await?;
        let arranged = read.arrange("profit_and_loss");
        let (groups, net_income) = statement::profit_and_loss(
            &read.accounts,
            &arranged.sections,
            &arranged.membership,
            &read.columns,
        );
        let report = ProfitAndLoss {
            header: read.header(
                periods
                    .iter()
                    .map(|p| Column::Period {
                        from: p.from,
                        to: p.to,
                    })
                    .collect(),
            ),
            unmatched_codes: arranged.unmatched.clone(),
            groups,
            net_income,
        };
        Ok((report, read, arranged))
    }

    async fn balance_sheet_read(
        &self,
        scope: &Scope,
        company_id: i64,
        dates: &[Date],
        through: Option<i64>,
    ) -> Result<(BalanceSheet, Read, Arranged)> {
        let dates = dates_checked(dates)?;
        let read = self
            .read(
                scope,
                company_id,
                dates.iter().map(|d| balance(*d)).collect(),
                through,
            )
            .await?;
        let arranged = read.arrange("balance_sheet");
        let sheet = statement::balance_sheet(
            &read.accounts,
            &arranged.sections,
            &arranged.membership,
            &read.columns,
        );
        let report = BalanceSheet {
            header: read.header(as_of(dates)),
            unmatched_codes: arranged.unmatched.clone(),
            groups: sheet.groups,
            earnings: sheet.earnings,
            total_assets: sheet.total_assets,
            total_liabilities_and_equity: sheet.total_liabilities_and_equity,
        };
        Ok((report, read, arranged))
    }

    async fn cash_position_read(
        &self,
        scope: &Scope,
        company_id: i64,
        dates: &[Date],
        through: Option<i64>,
    ) -> Result<(CashPosition, Read, Arranged)> {
        let dates = dates_checked(dates)?;
        let read = self
            .read(
                scope,
                company_id,
                dates.iter().map(|d| balance(*d)).collect(),
                through,
            )
            .await?;
        let arranged = read.arrange("cash_position");
        let (sections, totals) = statement::cash_position(
            &read.accounts,
            &arranged.sections,
            &arranged.membership,
            &read.columns,
        );
        let report = CashPosition {
            header: read.header(as_of(dates)),
            unmatched_codes: arranged.unmatched.clone(),
            sections,
            totals,
        };
        Ok((report, read, arranged))
    }

    /// Reads every window at the first one's entry number, so the columns
    /// describe one state of the ledger, then the chart and the layout. The
    /// ledger validates the number.
    async fn read(
        &self,
        scope: &Scope,
        company_id: i64,
        windows: Vec<TotalsQuery>,
        through: Option<i64>,
    ) -> Result<Read> {
        let mut through = through;
        let mut book = None;
        let mut columns = Vec::with_capacity(windows.len());
        for mut window in windows {
            window.through_entry_number = through;
            let totals = self
                .ledger
                .account_totals(scope, company_id, &window)
                .await?;
            through = Some(totals.through_entry_number);
            columns.push(
                totals
                    .rows
                    .iter()
                    .map(|row| (row.account.id, (row.debits, row.credits)))
                    .collect(),
            );
            book.get_or_insert((totals.currency_code, totals.minor_units));
        }
        let (currency_code, minor_units) = book.ok_or(ReportsError::InvalidOption)?;
        let accounts = self.ledger.list_accounts(scope, company_id).await?;
        let mut layout = self.layout(scope, company_id).await?;
        // An explicit cash section wins, including an intentionally empty one.
        // The fallback covers exactly the bank accounts, never their descendants.
        let mut cash_defaults = None;
        if layout::sections(&layout, "cash_position").is_empty() {
            let ids = self
                .sources
                .reconciliation
                .cash_account_ids(scope, company_id)
                .await
                .map_err(|error| match error {
                    bilibi_finance_reconciliation::ReconciliationError::Failed(error) => {
                        ReportsError::Failed(error)
                    }
                    bilibi_finance_reconciliation::ReconciliationError::Unauthorized => {
                        ReportsError::Unauthorized
                    }
                    _ => ReportsError::CashAccountsUnavailable,
                })?;
            cash_defaults = Some(ids.clone());
            let codes: Vec<_> = accounts
                .iter()
                .filter(|a| ids.contains(&a.id))
                .map(|a| a.code.clone())
                .collect();
            if !codes.is_empty() {
                if !layout::valid(&layout) {
                    layout = json!([]);
                }
                layout
                    .as_array_mut()
                    .ok_or(ReportsError::InvalidOption)?
                    .push(
                        json!({"report":"cash_position","name":"Cash and bank","accounts":codes}),
                    );
            }
        }
        Ok(Read {
            company_id,
            currency_code,
            minor_units,
            through_entry_number: through.ok_or(ReportsError::InvalidOption)?,
            accounts,
            columns,
            layout,
            cash_defaults,
        })
    }

    /// The company's [`SECTIONS`] value as Base Settings resolves it (company,
    /// then tenant, then the empty default). A stored value that no longer
    /// validates groups nothing rather than grouping wrongly.
    async fn layout(&self, scope: &Scope, company_id: i64) -> Result<Value> {
        let target = Target::Company(
            OrganisationId::new(company_id).map_err(|_| ReportsError::CompanyNotFound)?,
        );
        let client = self.db.get().await.map_err(Error::from)?;
        match self.settings.get(&**client, scope, target, SECTIONS).await {
            Ok(resolved) => Ok(resolved.value),
            Err(Error::Invalid) => Ok(Value::Array(Vec::new())),
            Err(error) => Err(refusal(error)),
        }
    }

    async fn authorize(&self, scope: &Scope, company_id: i64, capability: &str) -> Result<()> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, capability).await
    }

    async fn write<'c>(
        &self,
        client: &'c mut tokio_postgres::Client,
        scope: &Scope,
        company_id: i64,
        capability: &str,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        authorize(&*client, scope, company_id, capability).await?;
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        bilibi_company::require_writable(tx.sql(), scope, company_id, true)
            .await
            .map_err(|error| match error {
                WriteError::NotFound => ReportsError::CompanyNotFound,
                WriteError::Archived => ReportsError::CompanyArchived,
                WriteError::Access(error) => refusal(error),
                WriteError::InUse | WriteError::Stale | WriteError::Invalid(_) => {
                    ReportsError::Failed(Error::Internal)
                }
            })?;
        Ok(tx)
    }
}

// Income and expense over a period leave out year-end closing entries, or a
// period ending a closed year would show the year's result as zero.
fn flow(period: Period) -> TotalsQuery {
    TotalsQuery {
        from: Some(period.from),
        to: Some(period.to),
        through_entry_number: None,
        exclude_closing: true,
    }
}

fn balance(as_of: Date) -> TotalsQuery {
    TotalsQuery {
        to: Some(as_of),
        ..TotalsQuery::default()
    }
}

fn as_of(dates: &[Date]) -> Vec<Column> {
    dates.iter().map(|&as_of| Column::AsOf { as_of }).collect()
}

fn periods_checked(periods: &[Period]) -> Result<&[Period]> {
    if (1..=MAX_COLUMNS).contains(&periods.len()) && periods.iter().all(|p| p.from <= p.to) {
        Ok(periods)
    } else {
        Err(ReportsError::InvalidOption)
    }
}

fn dates_checked(dates: &[Date]) -> Result<&[Date]> {
    if (1..=MAX_COLUMNS).contains(&dates.len()) {
        Ok(dates)
    } else {
        Err(ReportsError::InvalidOption)
    }
}

/// Issued content is a durable JSON contract: string keys, exact decimal
/// strings, ISO dates and UTC timestamps.
fn json(value: &impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| ReportsError::Failed(Error::Internal))
}

/// The actor's current grant and company reach decide every call.
async fn authorize(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company_id, capability)
        .await
        .map(|_| ())
        .map_err(refusal)
}

fn refusal(error: Error) -> ReportsError {
    match error {
        Error::Forbidden | Error::Unauthorized => ReportsError::Unauthorized,
        Error::NotFound => ReportsError::CompanyNotFound,
        other => ReportsError::Failed(other),
    }
}

/// Dates are written as their ISO 8601 text, so the Domain does not switch on
/// a serde feature of the shared `jiff` crate.
fn as_text<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

fn as_optional_text<T: std::fmt::Display, S: serde::Serializer>(
    value: &Option<T>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.collect_str(value),
        None => serializer.serialize_none(),
    }
}
