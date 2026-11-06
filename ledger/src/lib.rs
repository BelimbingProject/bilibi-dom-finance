//! The general ledger, module `finance/ledger` (Bilimbi
//! `Bilimbi.Finance.Ledger`): a company's book, chart of accounts, journal
//! entries, fiscal years and their closing, controlled bookkeeping and late
//! recordings, reversals, the separation of duties with its approval
//! exceptions, and manual journals. Other Finance modules post, reverse and
//! ask for approvals through this facade; none reads its tables.
//!
//! Every function takes the sealed [`Scope`] it was handed and an explicit
//! company id. Reads authorize `finance.ledger.view` and writes their own
//! capability for the scope's actor and the target company at the moment
//! they run, through Core Company's `authorize_work_target`: a missing grant, or a
//! sibling company without tenant-wide reach, is
//! [`LedgerError::Unauthorized`], and a company the scope cannot see is
//! [`LedgerError::CompanyNotFound`]. A write then holds the company's row
//! lock for its mutation transaction, so an archived company is
//! [`LedgerError::CompanyArchived`] and the company's ledger writes run one
//! at a time.
//!
//! A book is opened once per company with its currency code and that
//! currency's number of decimal places; both are fixed for good, because
//! every posted amount is denominated in them ([`Money`]). A new company's
//! book opens controlled (see `bookkeeping.rs`). Accounts are never deleted
//! and keep their code and type; the one equity account designated for
//! retained earnings receives the year-end close.
//!
//! A company divides its time into fiscal years of monthly periods. Periods
//! close in date order and reopen in reverse; a reopen needs a reason and the
//! reopen capability, and a locked period never reopens.
//! [`Ledger::close_fiscal_year`] posts the entry that moves the year's income
//! and expense balances into retained earnings and closes the year with its
//! periods. Every close, reopen and lock is an append-only closing event, and
//! a status is the latest event's. A controlled company posts only into a
//! period it has opened and left open; an entry is never refused for being
//! old, but one recorded 45 or more days after its date is listed for an
//! independent review. A reopen, and turning controlled bookkeeping off,
//! override a control and are recorded as approval exceptions.
//!
//! A posted entry is never edited or deleted: [`Ledger::reverse_entry`] posts
//! its mirror image, once. An entry another module owns is corrected only
//! through that module, which calls [`Ledger::reverse_owned_entry`] inside the
//! transaction that records its own reversal. The owner is a seam between
//! modules, not an authorization boundary: the ledger cannot tell which module
//! is calling, so a caller holding `finance.ledger.entries.post` that passes an
//! entry's owner from anywhere else reverses it without the owner's record, and
//! the owner's control-account tie-out then shows the difference.
//!
//! A company requires separation of duties unless it turns it off: the person
//! who approves a record, or whoever impersonated them, is not the person who
//! prepared it, nor anyone they impersonated. [`Ledger::check_approval`] is how
//! Finance approvals outside manual journals ask, inside the approving transaction, and it
//! records what an independent reviewer reads: a same-person approval while
//! separation is off, and an override of a control. A manual journal is
//! submitted to Workflow and posts only once its shared approval round completes
//! ([`Ledger::submit_journal`], [`Ledger::approve_journal`]); its submitter may
//! withdraw or correct it while it waits ([`Ledger::withdraw_journal`],
//! [`Ledger::replace_journal`]).
//!
//! Figures are summed by PostgreSQL ([`Ledger::trial_balance`],
//! [`Ledger::account_totals`], [`Ledger::account_totals_by_owner`]) and name
//! the last entry number they count, which reproduces them exactly: entries
//! are numbered in posting order and never change. [`Ledger::evidence_page`]
//! exports each table for the auditor's pack.
//!
//! Results are plain records; the schema, SQL and validation stay private.
mod accounts;
mod agent_operations;
mod bookkeeping;
mod books;
mod configuration;
mod contributions;
mod conversion;
mod currency;
mod duties;
mod entries;
mod evidence;
mod groups;
mod journal_approvals;
mod journal_list;
mod journals;
mod late_recordings;
mod money;
mod opening;
mod opening_csv;
mod periods;
mod postings;
mod saved_templates;
mod templates;
mod totals;
mod web;

pub use contributions::{
    APPROVE_JOURNALS, CAPABILITIES, LOCK_PERIODS, MANAGE_ACCOUNTS, MANAGE_CONTROLLED_BOOKKEEPING,
    MANAGE_DUTY_SEPARATION, MANAGE_PERIODS, MANAGE_TEMPLATES, POST_ENTRIES, REOPEN_PERIODS,
    REVIEW_EXCEPTIONS, REVIEW_LATE_RECORDINGS, SUBMIT_JOURNALS, VIEW,
};
pub use conversion::OpeningContext;
pub use evidence::EvidencePage;
pub use money::{MinorUnits, Money, MoneyError};
pub use opening::{OpeningBalances, OpeningProgress};
pub use saved_templates::NewChartTemplate;

use bilibi_company::WriteError;
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::GenericClient;

/// The descriptor id, which later versions record as the owner of entries.
pub const MODULE: &str = "finance/ledger";
/// The registry service id of the [`Ledger`] facade.
pub const SERVICE: &str = "finance/ledger.ledger";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/agent_api", "base/authz", "base/database", "base/datetime", "base/locale", "base/menu", "base/party_ids", "base/records", "base/settings", "base/tenancy", "base/ui", "base/workflow", "core/company", "core/person", "core/party", "core/currency"],
        migrations: &[
            ("finance-ledger-001-books-and-accounts", include_str!("../migrations/001-books-and-accounts.sql")),
            ("finance-ledger-002-entries", include_str!("../migrations/002-entries.sql")),
            ("finance-ledger-003-posting-notices", include_str!("../migrations/003-posting-notices.sql")),
            ("finance-ledger-004-periods", include_str!("../migrations/004-periods.sql")),
            ("finance-ledger-005-journals", include_str!("../migrations/005-journals.sql")),
            ("finance-ledger-006-journal-withdrawal", include_str!("../migrations/006-journal-withdrawal.sql")),
            ("finance-ledger-007-opening-balances", include_str!("../migrations/007-opening-balances.sql")),
            ("finance-ledger-008-account-groups", include_str!("../migrations/008-account-groups.sql")),
            ("finance-ledger-009-chart-templates", include_str!("../migrations/009-chart-templates.sql")),
            ("finance-ledger-010-opening-recognitions", include_str!("../migrations/010-opening-recognitions.sql")),
            ("finance-ledger-011-opening-replacements", include_str!("../migrations/011-opening-replacements.sql")),
            ("finance-ledger-012-workflow-approvals", include_str!("../migrations/012-workflow-approvals.sql")),
            ("finance-ledger-013-journal-list", include_str!("../migrations/013-journal-list.sql")),
            ("finance-ledger-014-zero-opening-recognition", include_str!("../migrations/014-zero-opening-recognition.sql")),
        ],
        services: &[Service { id: SERVICE, factory: |services| {
            journal_approvals::bind(services.get(bilibi_workflow::SERVICE)?)?;
            saved_templates::bind(services.get("base/settings.store")?)?;
            currency::bind(
                services.get("core/currency.currencies")?,
                services.get("base/settings.store")?,
            )?;
            Ok(Arc::new(Ledger {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            mutations: services.get("base/database.mutations")?,
            postings: postings::Relay::new(),
        })) } }, Service { id: web::PAGES, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                parties: services.get("base/tenancy.parties")?,
            }))))
        } }],
    }
}

pub type Result<T> = std::result::Result<T, LedgerError>;

/// A book's currency facts alone: what a page needs to format an amount.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BookFigures {
    pub company_id: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
}

/// Why a call did not happen (Bilimbi's refusal atoms and changeset errors).
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
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
    #[error("The company's book is already open")]
    BookAlreadyOpen,
    #[error("Not found")]
    AccountNotFound,
    #[error("Not found")]
    EntryNotFound,
    #[error("Not found")]
    FiscalYearNotFound,
    #[error("Not found")]
    PeriodNotFound,
    /// Closing a period that is closed or locked.
    #[error("The period is not open")]
    PeriodNotOpen,
    /// Reopening or locking a period that is open.
    #[error("The period is not closed")]
    PeriodNotClosed,
    /// Reopening a locked period, or a fiscal year whose last period is.
    #[error("The period is locked and never reopens")]
    PeriodLocked,
    /// Periods close in date order; so does a fiscal year after the previous one.
    #[error("An earlier period is still open")]
    EarlierPeriodOpen,
    /// A year's last period closes only with the year.
    #[error("The period closes with its fiscal year")]
    ClosesWithFiscalYear,
    /// Periods reopen in reverse date order.
    #[error("A later period is closed")]
    LaterPeriodClosed,
    /// Periods lock in date order.
    #[error("An earlier period is not locked")]
    EarlierPeriodUnlocked,
    /// A closed fiscal year's periods reopen only after the year.
    #[error("The fiscal year is closed")]
    FiscalYearClosed,
    #[error("The fiscal year is not closed")]
    FiscalYearNotClosed,
    /// The year-end close needs an active equity account designated for
    /// retained earnings.
    #[error("No retained-earnings account is designated")]
    NoRetainedEarningsAccount,
    #[error("That entry was not recorded late")]
    NotLateRecorded,
    /// A late entry or an approval exception already has its one review.
    #[error("It has already been reviewed")]
    AlreadyReviewed,
    /// The reviewer posted the late entry, or approved or prepared what an
    /// approval exception records, or impersonated (or was impersonated by)
    /// whoever did.
    #[error("The reviewer is not independent of what is reviewed")]
    ReviewerNotIndependent,
    /// The entry already has its one reversal.
    #[error("The entry is already reversed")]
    AlreadyReversed,
    /// A reversal is not reversed in turn: post a new entry instead.
    #[error("A reversal is not reversed")]
    ReversalNotReversible,
    /// A year-end closing entry is reversed only by reopening its year.
    #[error("A year-end closing entry is reversed by reopening its fiscal year")]
    ClosingEntry,
    /// The entry belongs to another module's record: correct it through that
    /// module.
    #[error("The entry is owned by {} ({})", .0.module, .0.record)]
    OwnedBy(EntryOwner),
    /// [`Ledger::reverse_owned_entry`] for an entry another module or record owns.
    #[error("The entry is owned by another record")]
    NotTheOwner,
    /// [`Ledger::reverse_owned_entry`] for a manual entry.
    #[error("The entry has no owner")]
    NotOwned,
    /// The approver prepared the record, impersonated whoever did, or was
    /// impersonated by them, while the company requires separation of duties.
    #[error(
        "Someone else must approve this because the company requires separation of duties, including when impersonating. For a one-person company, change Separation of duties on Approval exceptions: /finance/reports/approval-exceptions"
    )]
    SamePerson,
    #[error("Set up manual journal approval rules on Approval rules before submitting a journal.")]
    ApprovalNotConfigured,
    #[error("{0}")]
    ApprovalRefused(String),
    #[error("Not found")]
    ExceptionNotFound,
    #[error("Not found")]
    JournalNotFound,
    /// Anything but a submitted journal: approved, rejected or withdrawn.
    #[error("The journal is no longer waiting for approval")]
    JournalDecided,
    /// A journal is withdrawn only by the person who submitted it.
    #[error("Only the person who submitted this journal can withdraw it")]
    NotSubmitter,
    /// An option the call does not accept: an `owner` or settlement account
    /// of another shape, or a limit, cursor or bound out of range.
    #[error("Invalid option")]
    InvalidOption,
    /// The relay's database connection could not be made to listen.
    #[error("Posting notices are unavailable")]
    NoticesUnavailable,
    /// Refused values, in the words a form shows beside each field.
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    /// A database or capture failure; never a refusal.
    #[error(transparent)]
    Failed(#[from] Error),
}

/// One refusal of a submitted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
    /// The 1-based line of an entry the refusal is about, when it is about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}
impl FieldError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
            line: None,
        }
    }
    pub(crate) fn on_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

/// The company's book: its currency and that currency's decimal places.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Book {
    pub company_id: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
}
/// What opens a book: three letters (trimmed and upcased) and 0 to 4.
#[derive(Clone, Debug)]
pub struct NewBook {
    pub currency_code: String,
    pub minor_units: i64,
}

/// The five types every chart of accounts classifies by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}
impl AccountType {
    pub const ALL: [AccountType; 5] = [
        Self::Asset,
        Self::Liability,
        Self::Equity,
        Self::Income,
        Self::Expense,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "asset",
            Self::Liability => "liability",
            Self::Equity => "equity",
            Self::Income => "income",
            Self::Expense => "expense",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value.trim())
    }
}

/// One row of the chart, as the facade shows it: an account that takes
/// postings, or an account group (`is_group`) that takes none and stands for
/// the total of everything under it. `parent_id` is the enclosing group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Account {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub account_type: AccountType,
    pub parent_id: Option<i64>,
    pub is_group: bool,
    pub active: bool,
    pub retained_earnings: bool,
}
impl Account {
    /// Whether an entry may post to it now: an active account, never a group.
    pub fn postable(&self) -> bool {
        self.active && !self.is_group
    }
}
/// What creates an account: `code` (unique in the company), `name` and
/// `account_type`, optionally under an account group of the same type
/// (`parent_id`), `active` (true by default) and `retained_earnings` (false
/// by default).
#[derive(Clone, Debug)]
pub struct NewAccount {
    pub code: String,
    pub name: String,
    pub account_type: AccountType,
    pub parent_id: Option<i64>,
    pub active: bool,
    pub retained_earnings: bool,
}
impl NewAccount {
    pub fn new(
        code: impl Into<String>,
        name: impl Into<String>,
        account_type: AccountType,
    ) -> Self {
        Self {
            code: code.into(),
            name: name.into(),
            account_type,
            parent_id: None,
            active: true,
            retained_earnings: false,
        }
    }
}
/// What changes on an account: each `Some` replaces the field; `parent_id:
/// Some(None)` takes it out of its group. Code and type never change.
#[derive(Clone, Debug, Default)]
pub struct AccountChange {
    pub name: Option<String>,
    pub parent_id: Option<Option<i64>>,
    pub active: Option<bool>,
    pub retained_earnings: Option<bool>,
}
/// What creates an account group: `code` (unique among the company's accounts
/// and groups), `name` and `account_type`, optionally inside another group of
/// the same type (`parent_id`). A group takes no postings.
#[derive(Clone, Debug)]
pub struct NewAccountGroup {
    pub code: String,
    pub name: String,
    pub account_type: AccountType,
    pub parent_id: Option<i64>,
}
impl NewAccountGroup {
    pub fn new(
        code: impl Into<String>,
        name: impl Into<String>,
        account_type: AccountType,
    ) -> Self {
        Self {
            code: code.into(),
            name: name.into(),
            account_type,
            parent_id: None,
        }
    }
}
/// What changes on an account group: each `Some` replaces the field;
/// `parent_id: Some(None)` takes it out of its group. Code and type never
/// change.
#[derive(Clone, Debug, Default)]
pub struct AccountGroupChange {
    pub name: Option<String>,
    pub parent_id: Option<Option<i64>>,
}

/// A starter chart: groups and accounts a company can start from, as data
/// (bundled JSON or a saved company snapshot). `review` states its review status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartTemplate {
    pub key: String,
    pub name: String,
    pub review: String,
    pub description: String,
    /// What a person should know before applying it, such as what needs
    /// advice specific to the business.
    pub notes: Vec<String>,
    /// Optional parts of the chart a person ticks, such as the accounts only a
    /// business registered for a tax or with employees needs.
    pub sections: Vec<ChartTemplateSection>,
    /// Groups and accounts, each after the group it sits in. In a preview,
    /// exactly the rows the chosen sections create.
    pub rows: Vec<ChartTemplateRow>,
}
/// An optional part of a starter chart. Its rows are created only when a
/// request chooses its `key`; none is chosen by default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartTemplateSection {
    pub key: String,
    pub name: String,
    pub description: String,
}
/// One group or account a starter chart creates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartTemplateRow {
    pub code: String,
    pub name: String,
    pub account_type: AccountType,
    pub is_group: bool,
    pub parent_code: Option<String>,
    pub retained_earnings: bool,
    /// The optional section this row belongs to; none for a core row.
    pub section: Option<String>,
}
/// What applying a starter chart to a company would do now: the exact rows
/// the chosen sections and the core create, how many rows the company's chart already has, and which of
/// the template's codes those use. A chart that has any row is never changed:
/// `applicable` is true only for an empty one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartTemplatePreview {
    pub template: ChartTemplate,
    pub chosen_sections: Vec<String>,
    pub existing_rows: usize,
    pub conflicting_codes: Vec<String>,
    pub applicable: bool,
}
/// What applying a starter chart created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartTemplateApplied {
    pub key: String,
    pub sections: Vec<String>,
    pub groups: usize,
    pub accounts: usize,
}

/// One row of the chart with its figure at a date. An account reports its own
/// `balance`; a group reports the `total` of every account under it, and never
/// a balance of its own. Both are on the account type's natural side (debits
/// less credits for asset and expense, credits less debits for the others) and
/// negative when the figure sits on the other side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChartBalance {
    #[serde(flatten)]
    pub account: Account,
    pub balance: Option<Money>,
    pub total: Option<Money>,
}
/// The chart's figures as of a date, counted through `through_entry_number`
/// (see [`TrialBalance`]), every account and group by code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChartBalances {
    pub company_id: i64,
    #[serde(serialize_with = "as_text")]
    pub as_of: Date,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub rows: Vec<ChartBalance>,
}

/// The module and record an entry belongs to: `module` is the owner's
/// descriptor id (`finance/receivables`) and `record` a kind and a positive
/// integer in the owner's own terms (`payment:17`). A manual entry has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntryOwner {
    pub module: String,
    pub record: String,
}

/// One line of a new entry: an account and exactly one positive side.
#[derive(Clone, Debug)]
pub struct NewLine {
    pub account_id: i64,
    pub debit: Money,
    pub credit: Money,
    pub description: Option<String>,
}
impl NewLine {
    pub fn debit(account_id: i64, amount: Money) -> Self {
        Self {
            account_id,
            debit: amount,
            credit: Money::ZERO,
            description: None,
        }
    }
    pub fn credit(account_id: i64, amount: Money) -> Self {
        Self {
            account_id,
            debit: Money::ZERO,
            credit: amount,
            description: None,
        }
    }
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}
/// What posts an entry: `entry_date`, `description` (1 to 500 characters),
/// an optional `reference` (at most 100) and at least two `lines`.
#[derive(Clone, Debug)]
pub struct NewEntry {
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub lines: Vec<NewLine>,
}
/// Options of [`Ledger::post_entry`] (Bilimbi's keyword options).
#[derive(Clone, Debug, Default)]
pub struct PostOptions {
    /// Accounts that may be inactive in this entry only, for a module that
    /// settles a document it posted earlier. Each must still be an account of
    /// the company; every other line still needs an active account.
    pub settlement_account_ids: Vec<i64>,
    /// The module and record this entry recognises. The record has at most
    /// one original entry in the company, and an owned entry is corrected
    /// only through its owner.
    pub owner: Option<EntryOwner>,
}

/// One line of a posted entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntryLine {
    pub line_number: i32,
    pub account_id: i64,
    pub debit: Money,
    pub credit: Money,
    pub description: Option<String>,
}
/// A posted entry with its lines, as the database stored it. The number and
/// time are the database's; `reversed_by_entry_id` names the later entry that
/// reversed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: i64,
    pub entry_number: i64,
    #[serde(serialize_with = "as_text")]
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub currency_code: String,
    pub reverses_entry_id: Option<i64>,
    pub owner: Option<EntryOwner>,
    pub posted_by_user_id: i64,
    pub posted_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub posted_at: Timestamp,
    pub reversed_by_entry_id: Option<i64>,
    pub lines: Vec<EntryLine>,
}
/// Dates and instants are written as their ISO 8601 text, so the Domain
/// does not switch on a serde feature of the shared `jiff` crate.
fn as_text<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
/// Bounds of [`Ledger::list_entries`]; `Default` lists the first 100.
#[derive(Clone, Debug, Default)]
pub struct EntryQuery {
    /// Entry dates, both inclusive.
    pub from: Option<Date>,
    pub to: Option<Date>,
    /// 1 to 500, 100 when absent.
    pub limit: Option<u32>,
    /// The `(entry_date, id)` of the last entry already read: only entries
    /// after it come back.
    pub after: Option<(Date, i64)>,
    /// Entries with a line on this account.
    pub account_id: Option<i64>,
    /// Entries numbered up to this one, to reproduce an earlier read.
    pub through_entry_number: Option<i64>,
    /// Leaves out year-end closing entries and their reversals.
    pub exclude_closing: bool,
}

/// A fiscal year's status, its latest closing event's (open with none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum YearStatus {
    Open,
    Closed,
}
impl YearStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}
/// A period's status, its latest closing event's (open with none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PeriodStatus {
    Open,
    Closed,
    Locked,
}
impl PeriodStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Locked => "locked",
        }
    }
}
/// What a closing event does to its period, or (with no period) its year.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosingAction {
    Close,
    Reopen,
    Lock,
}
impl ClosingAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Reopen => "reopen",
            Self::Lock => "lock",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        [Self::Close, Self::Reopen, Self::Lock]
            .into_iter()
            .find(|action| action.as_str() == value)
    }
    /// The status the action leaves a period in.
    pub fn status_after(self) -> PeriodStatus {
        match self {
            Self::Close => PeriodStatus::Closed,
            Self::Reopen => PeriodStatus::Open,
            Self::Lock => PeriodStatus::Locked,
        }
    }
}

/// What opens a fiscal year: the first day of a month and the last day of a
/// month, one to 24 months on. The start defaults to the day after the
/// previous year ends (the first year needs one), the end to twelve months
/// on.
#[derive(Clone, Debug, Default)]
pub struct NewFiscalYear {
    pub start_date: Option<Date>,
    pub end_date: Option<Date>,
}
/// One calendar month of a fiscal year.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Period {
    pub id: i64,
    pub fiscal_year_id: i64,
    pub period_number: i16,
    #[serde(serialize_with = "as_text")]
    pub start_date: Date,
    #[serde(serialize_with = "as_text")]
    pub end_date: Date,
    pub status: PeriodStatus,
}
/// A fiscal year with its periods in date order; `closing_entry_id` names the
/// year-end closing entry while the year is closed by one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FiscalYear {
    pub id: i64,
    #[serde(serialize_with = "as_text")]
    pub start_date: Date,
    #[serde(serialize_with = "as_text")]
    pub end_date: Date,
    pub opened_by_user_id: i64,
    pub opened_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub opened_at: Timestamp,
    pub status: YearStatus,
    pub closing_entry_id: Option<i64>,
    pub periods: Vec<Period>,
}
/// One event of a fiscal year's closing log; `period_id` is `None` for an
/// event on the year itself, whose close names the closing entry and the
/// retained-earnings account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClosingEvent {
    pub event_number: i64,
    pub fiscal_year_id: i64,
    pub period_id: Option<i64>,
    pub action: ClosingAction,
    pub reason: Option<String>,
    pub entry_id: Option<i64>,
    pub retained_earnings_account_id: Option<i64>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub recorded_at: Timestamp,
}
/// What the year-end close may say about its entry: the description
/// defaults to `Close fiscal year <start> to <end>`.
#[derive(Clone, Debug, Default)]
pub struct YearClose {
    pub description: Option<String>,
    pub reference: Option<String>,
}

/// One change of the company's controlled bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BookkeepingChange {
    pub id: i64,
    pub change_number: i64,
    pub controlled: bool,
    pub reason: Option<String>,
    pub changed_by_user_id: i64,
    pub changed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub changed_at: Timestamp,
}
/// The company's controlled bookkeeping: the setting, its latest change and
/// how many fiscal years the company has opened. A controlled company with
/// no fiscal year has no period to post into until one is opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ControlledBookkeeping {
    pub controlled: bool,
    pub change: Option<BookkeepingChange>,
    pub fiscal_years: usize,
}

/// The late-recording report's thresholds, in days from an entry's date to
/// its recording: listed from `approaching_days`, late past `late_days`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LateRecordingThresholds {
    pub approaching_days: i32,
    pub late_days: i32,
}
/// Where a listed entry stands against the thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LateStatus {
    Approaching,
    Late,
}
/// An independent reviewer's reason for a late entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LateRecordingReview {
    pub id: i64,
    pub entry_id: i64,
    pub reason: String,
    pub reviewed_by_user_id: i64,
    pub reviewed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub reviewed_at: Timestamp,
}
/// A posted entry recorded late: `days` are whole UTC days from its
/// `entry_date` (its transaction date) to `posted_at` (its recording).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LateRecording {
    pub entry_id: i64,
    pub entry_number: i64,
    #[serde(serialize_with = "as_text")]
    pub entry_date: Date,
    #[serde(serialize_with = "as_text")]
    pub posted_at: Timestamp,
    pub days: i32,
    pub status: LateStatus,
    pub description: String,
    pub reference: Option<String>,
    pub posted_by_user_id: i64,
    pub posted_by_impersonator_id: Option<i64>,
    pub review: Option<LateRecordingReview>,
}
/// Bounds of [`Ledger::list_late_recordings`]; `Default` lists the first 100
/// of both statuses, reviewed or not.
#[derive(Clone, Debug, Default)]
pub struct LateRecordingQuery {
    pub status: Option<LateStatus>,
    pub reviewed: Option<bool>,
    /// 1 to 500, 100 when absent.
    pub limit: Option<u32>,
    /// The last `entry_number` of the previous page.
    pub after: Option<i64>,
}

/// What a reversal says about itself: its `entry_date` (not before the
/// original's), `description` (1 to 500 characters) and an optional
/// `reference` (at most 100). Its lines are the original's, sides swapped.
#[derive(Clone, Debug)]
pub struct Reversal {
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
}

/// One change of the company's separation of duties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DutySeparationChange {
    pub id: i64,
    pub change_number: i64,
    pub required: bool,
    pub reason: Option<String>,
    pub changed_by_user_id: i64,
    pub changed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub changed_at: Timestamp,
}
/// The company's separation of duties: the setting and its latest change. A
/// company that never changed it requires separation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DutySeparation {
    pub required: bool,
    pub change: Option<DutySeparationChange>,
}

/// Someone whose work an approval approves: an account and, when they acted
/// as it through impersonation, the operator who did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Preparer {
    pub user_id: i64,
    pub impersonator_id: Option<i64>,
}
/// What [`Ledger::check_approval`] decides: `source` (the approving module's
/// descriptor id, such as `finance/payables`), `action` and `subject_type`
/// (lower-case codes), `subject_id`, `subject` (a label a reviewer reads, kept
/// to 200 characters), `prepared_by` (everyone whose work is approved) and,
/// for an approval that overrides a control, `override_reason`: the
/// approver's reason, which records the override whoever approves.
#[derive(Clone, Debug)]
pub struct Approval {
    pub source: String,
    pub action: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub subject: String,
    pub prepared_by: Vec<Preparer>,
    pub override_reason: Option<String>,
}

/// An independent reviewer's reading of an approval exception.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApprovalExceptionReview {
    pub note: Option<String>,
    pub reviewed_by_user_id: i64,
    pub reviewed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub reviewed_at: Timestamp,
}
/// An approval an independent reviewer should read: a same-person approval
/// while separation is off (naming the change that turned it off) or an
/// override of a control. `overrides` is Bilimbi's `override`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApprovalException {
    pub id: i64,
    pub exception_number: i64,
    pub source: String,
    pub action: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub subject: String,
    pub same_person: bool,
    #[serde(rename = "override")]
    pub overrides: bool,
    pub prepared_by_user_id: Option<i64>,
    pub prepared_by_impersonator_id: Option<i64>,
    pub approved_by_user_id: i64,
    pub approved_by_impersonator_id: Option<i64>,
    pub reason: String,
    pub separation_change_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub recorded_at: Timestamp,
    pub review: Option<ApprovalExceptionReview>,
}
/// Bounds of [`Ledger::list_approval_exceptions`]; `Default` lists the first
/// 100, reviewed or not.
#[derive(Clone, Debug, Default)]
pub struct ApprovalExceptionQuery {
    /// Recorded at or after this instant.
    pub recorded_from: Option<Timestamp>,
    /// Recorded before this instant, so a period's bounds in any time zone
    /// select it exactly.
    pub recorded_before: Option<Timestamp>,
    pub reviewed: Option<bool>,
    /// 1 to 500, 100 when absent.
    pub limit: Option<u32>,
    /// The last `exception_number` of the previous page.
    pub after: Option<i64>,
}

/// The statuses a journal moves through: submitted until its one decision.
/// `Withdrawn` is the submitter taking it back before anyone decided it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JournalStatus {
    Submitted,
    Approved,
    Rejected,
    Withdrawn,
}
impl JournalStatus {
    pub const ALL: [JournalStatus; 4] = [
        Self::Submitted,
        Self::Approved,
        Self::Rejected,
        Self::Withdrawn,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}
/// A journal's decision: who approved, rejected or withdrew it and when, any `reason`,
/// and the `entry_id` an approval posted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JournalDecision {
    pub event: JournalStatus,
    pub reason: Option<String>,
    pub entry_id: Option<i64>,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub occurred_at: Timestamp,
}
/// A manual journal with its lines; the number and time are the database's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Journal {
    pub id: i64,
    pub journal_number: i64,
    #[serde(serialize_with = "as_text")]
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub currency_code: String,
    pub submitted_by_user_id: i64,
    pub submitted_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub submitted_at: Timestamp,
    pub status: JournalStatus,
    pub decision: Option<JournalDecision>,
    /// The withdrawn journal this one corrects, when it was submitted as its replacement.
    pub replaces_journal_id: Option<i64>,
    /// The journal that corrects this one once it is withdrawn and replaced.
    pub replaced_by_journal_id: Option<i64>,
    /// The number of the journal `replaces_journal_id` names.
    pub replaces_journal_number: Option<i64>,
    /// The number of the journal `replaced_by_journal_id` names.
    pub replaced_by_journal_number: Option<i64>,
    pub lines: Vec<EntryLine>,
}
/// Bounds of [`Ledger::list_journals`]; `Default` lists the first 100 of
/// every status.
#[derive(Clone, Debug, Default)]
pub struct JournalQuery {
    pub status: Option<JournalStatus>,
    /// 1 to 500, 100 when absent.
    pub limit: Option<u32>,
    /// The last `journal_number` of the previous page.
    pub after: Option<i64>,
}

/// Bounds of [`Ledger::account_totals`] and [`Ledger::account_totals_by_owner`].
#[derive(Clone, Debug, Default)]
pub struct TotalsQuery {
    /// Entry dates, both inclusive; `from` after `to` is `InvalidOption`.
    pub from: Option<Date>,
    pub to: Option<Date>,
    /// Counts entries numbered up to this one (0 or more), to reproduce an
    /// earlier read; the company's last entry when absent.
    pub through_entry_number: Option<i64>,
    /// Leaves out year-end closing entries and their reversals, so income
    /// and expense over a closed year show its activity.
    pub exclude_closing: bool,
}
/// One account's debit and credit totals over a window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AccountTotal {
    #[serde(flatten)]
    pub account: Account,
    pub debits: Money,
    pub credits: Money,
}
/// One account's totals from the entries of one owner module (its descriptor
/// id, `None` for entries with no owner).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnerTotal {
    #[serde(flatten)]
    pub account: Account,
    pub owner_module: Option<String>,
    pub debits: Money,
    pub credits: Money,
}
/// Account totals over a window, counted through `through_entry_number`:
/// [`AccountTotals`] or [`OwnerTotals`]. Account names, parents and active
/// flags are as they are now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Totals<R> {
    pub company_id: i64,
    #[serde(serialize_with = "as_optional_text")]
    pub from: Option<Date>,
    #[serde(serialize_with = "as_optional_text")]
    pub to: Option<Date>,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub rows: Vec<R>,
}
pub type AccountTotals = Totals<AccountTotal>;
pub type OwnerTotals = Totals<OwnerTotal>;
/// One account of a trial balance: its totals and its balance on one side
/// (the other is zero).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrialBalanceRow {
    #[serde(flatten)]
    pub account: Account,
    pub debits: Money,
    pub credits: Money,
    pub debit_balance: Money,
    pub credit_balance: Money,
}
/// Every account with a posting dated on or before `as_of`, by code, counted
/// through `through_entry_number`. `total_debit` equals `total_credit`,
/// because every posted entry balances.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrialBalance {
    pub company_id: i64,
    #[serde(serialize_with = "as_text")]
    pub as_of: Date,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub rows: Vec<TrialBalanceRow>,
    pub total_debit: Money,
    pub total_credit: Money,
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

/// What a [`PostingSubscription`] hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostingEvent {
    /// The entry numbered `entry_number` committed in the subscribed company.
    Posted { company_id: i64, entry_number: i64 },
    /// Notices may have been missed (the listening connection dropped, or the
    /// subscriber fell behind): read the ledger again.
    Refresh,
}
/// A company's posting notices, from [`Ledger::subscribe_postings`]; it ends
/// when dropped.
pub struct PostingSubscription {
    tenant: i64,
    company_id: i64,
    receiver: tokio::sync::broadcast::Receiver<postings::Heard>,
}

/// The trial balance on one snapshot, for a caller that has already
/// authorized the company ([`Ledger::trial_balance_on`] and the
/// `finance.ledger.trial_balance.get` operation).
pub(crate) async fn trial_balance_of(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    as_of: Date,
    through_entry_number: Option<i64>,
) -> Result<TrialBalance> {
    let window = totals::Window::new(&TotalsQuery {
        to: Some(as_of),
        through_entry_number,
        ..TotalsQuery::default()
    })?;
    let book = books::find(client, tenant, company_id)
        .await?
        .ok_or(LedgerError::BookNotOpen)?;
    let (through, totals) = totals::by_account(client, tenant, company_id, &window).await?;
    let rows: Vec<TrialBalanceRow> = totals
        .into_iter()
        .map(|total| {
            let net = total.debits - total.credits;
            TrialBalanceRow {
                debit_balance: if net.is_positive() { net } else { Money::ZERO },
                credit_balance: if (-net).is_positive() {
                    -net
                } else {
                    Money::ZERO
                },
                account: total.account,
                debits: total.debits,
                credits: total.credits,
            }
        })
        .collect();
    Ok(TrialBalance {
        company_id,
        as_of,
        through_entry_number: through,
        currency_code: book.currency_code,
        minor_units: book.minor_units,
        total_debit: rows.iter().map(|row| row.debit_balance).sum(),
        total_credit: rows.iter().map(|row| row.credit_balance).sum(),
        rows,
    })
}

/// The chart's balances and group totals on one snapshot, for a caller that
/// has already authorized the company.
pub(crate) async fn chart_balances_of(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    as_of: Date,
    through_entry_number: Option<i64>,
) -> Result<ChartBalances> {
    let trial = trial_balance_of(client, tenant, company_id, as_of, through_entry_number).await?;
    let chart = accounts::list(client, tenant, company_id).await?;
    Ok(ChartBalances {
        company_id,
        as_of,
        through_entry_number: trial.through_entry_number,
        currency_code: trial.currency_code,
        minor_units: trial.minor_units,
        rows: groups::roll_up(chart, &trial.rows),
    })
}

/// The starter chart `key` with the optional `sections` chosen, against the
/// company's chart as it is now.
pub(crate) async fn chart_template_preview(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    key: &str,
    sections: &[String],
) -> Result<ChartTemplatePreview> {
    let template = saved_templates::require(client, tenant, key).await?;
    let rows = templates::select(&template, sections)?;
    let (existing_rows, conflicting_codes) =
        templates::existing(client, tenant, company_id, &rows).await?;
    Ok(ChartTemplatePreview {
        chosen_sections: template
            .sections
            .iter()
            .filter(|s| sections.contains(&s.key))
            .map(|s| s.key.clone())
            .collect(),
        template: ChartTemplate {
            rows: rows.into_iter().cloned().collect(),
            ..template.clone()
        },
        existing_rows,
        conflicting_codes,
        applicable: existing_rows == 0,
    })
}

/// The facade, resolved from the registry as [`SERVICE`].
pub struct Ledger {
    db: Db,
    mutations: Arc<MutationBoundary>,
    postings: postings::Relay,
}

impl Ledger {
    /// The account types a chart of accounts classifies by.
    pub fn account_types() -> &'static [AccountType] {
        &AccountType::ALL
    }

    /// The company's book (`finance.ledger.view`), or `BookNotOpen` before
    /// [`Ledger::open_book`].
    pub async fn get_book(&self, scope: &Scope, company_id: i64) -> Result<Book> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.get_book_on(&**client, scope, company_id).await
    }

    /// The same authorized book read on the caller's connection, so an owner
    /// holding the company lock never needs a second pool connection.
    pub async fn get_book_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Book> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        books::find(client, scope.tenant(), company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)
    }

    /// The book's currency and minor units for a record owner that holds its
    /// own Finance capability for the company but not `finance.ledger.view`
    /// (a sales-workflow person preparing a draft). `authority` is the
    /// caller's own capability and is decided here for that company; only the
    /// currency facts are returned, never an entry, figure or account.
    pub async fn book_figures_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        authority: &str,
    ) -> Result<BookFigures> {
        if !authority.starts_with("finance.") || authority == VIEW {
            return Err(LedgerError::InvalidOption);
        }
        agent_operations::authorize_on(client, scope, company_id, authority).await?;
        let book = books::find(client, scope.tenant(), company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        Ok(BookFigures {
            company_id: book.company_id,
            currency_code: book.currency_code,
            minor_units: book.minor_units,
        })
    }

    /// The chart on the caller's connection for a validation that needs only
    /// which accounts exist and may be posted to, decided under the caller's
    /// own Finance capability (see [`Ledger::book_figures_on`]). A person
    /// without `finance.ledger.view` is never shown the chart by this.
    pub async fn list_accounts_for_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        authority: &str,
    ) -> Result<Vec<Account>> {
        if !authority.starts_with("finance.") || authority == VIEW {
            return Err(LedgerError::InvalidOption);
        }
        agent_operations::authorize_on(client, scope, company_id, authority).await?;
        Ok(accounts::list(client, scope.tenant(), company_id).await?)
    }

    /// Opens the company's book once (`finance.ledger.accounts.manage`); a
    /// second call is `BookAlreadyOpen`. The new book is controlled from the
    /// start.
    pub async fn open_book(&self, scope: &Scope, company_id: i64, new: &NewBook) -> Result<Book> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.open_book",
            )
            .await?;
        let book = Self::open_book_on(&mut tx, scope, company_id, new).await?;
        tx.commit().await?;
        Ok(book)
    }

    async fn open_book_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewBook,
    ) -> Result<Book> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        if books::find(tx.sql(), tenant, company_id).await?.is_some() {
            return Err(LedgerError::BookAlreadyOpen);
        }
        let valid = books::validate(new).map_err(LedgerError::Invalid)?;
        if let Err(refused) = currency::choose_on(tx, scope, &valid.currency_code, false).await? {
            return Err(LedgerError::Invalid(vec![refused]));
        }
        let controlled = bookkeeping::controlled(tx.sql(), tenant, company_id).await?;
        let actor = scope.user();
        let impersonator = scope.impersonator().map(|operator| operator.user());
        let book = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (id, book) = books::insert(sql, tenant, company_id, &valid).await?;
                    let mut changes = vec![Change {
                        kind: "finance.ledger.book".into(),
                        id,
                        old: serde_json::Value::Null,
                        new: books::to_json(&book),
                    }];
                    if !controlled {
                        let (id, recorded) = bookkeeping::record(
                            sql,
                            tenant,
                            company_id,
                            actor,
                            impersonator,
                            true,
                            None,
                        )
                        .await?;
                        changes.push(Change {
                            kind: "finance.ledger.bookkeeping_change".into(),
                            id,
                            old: serde_json::Value::Null,
                            new: recorded,
                        });
                    }
                    Ok((book, changes))
                })
            })
            .await?;
        Ok(book)
    }

    /// The company's accounts, ordered by code (`finance.ledger.view`).
    pub async fn list_accounts(&self, scope: &Scope, company_id: i64) -> Result<Vec<Account>> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.list_accounts_on(&**client, scope, company_id).await
    }

    /// The same authorized chart read on the caller's connection. A record
    /// owner validates account choices while holding its company write lock.
    pub async fn list_accounts_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<Account>> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        Ok(accounts::list(client, scope.tenant(), company_id).await?)
    }

    /// Adds an account (`finance.ledger.accounts.manage`). `retained_earnings`
    /// designates an equity account as the one the year-end close moves
    /// profit into; designating another account later moves the designation.
    pub async fn create_account(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewAccount,
    ) -> Result<Account> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.create_account",
            )
            .await?;
        let account = Self::create_account_on(&mut tx, scope, company_id, new).await?;
        tx.commit().await?;
        Ok(account)
    }

    async fn create_account_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewAccount,
    ) -> Result<Account> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        let valid = accounts::validate_new(tx.sql(), tenant, company_id, new)
            .await?
            .map_err(LedgerError::Invalid)?;
        let account = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    if valid.retained_earnings {
                        if let Some((id, old, new)) =
                            accounts::release_designation(sql, tenant, company_id, None).await?
                        {
                            changes.push(account_change(id, old, new));
                        }
                    }
                    let account = accounts::insert(sql, tenant, company_id, &valid).await?;
                    changes.push(account_change(
                        account.id,
                        serde_json::Value::Null,
                        accounts::to_json(&account),
                    ));
                    Ok((account, changes))
                })
            })
            .await?;
        Ok(account)
    }

    /// Changes an account's name, parent, active flag or retained-earnings
    /// designation (`finance.ledger.accounts.manage`). A deactivated account
    /// takes no new postings and is deactivated only once its balance is
    /// zero; its history stays.
    pub async fn update_account(
        &self,
        scope: &Scope,
        company_id: i64,
        account_id: i64,
        change: &AccountChange,
    ) -> Result<Account> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.update_account",
            )
            .await?;
        let account =
            Self::update_account_on(&mut tx, scope, company_id, account_id, change).await?;
        tx.commit().await?;
        Ok(account)
    }

    async fn update_account_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        account_id: i64,
        change: &AccountChange,
    ) -> Result<Account> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        let current = accounts::find(tx.sql(), tenant, company_id, account_id)
            .await?
            .ok_or(LedgerError::AccountNotFound)?;
        let units = books::find(tx.sql(), tenant, company_id)
            .await?
            .map(|book| book.minor_units)
            .unwrap_or(MinorUnits(0));
        let next = accounts::validate_change(tx.sql(), tenant, company_id, &current, change, units)
            .await?
            .map_err(LedgerError::Invalid)?;
        let account = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    if next.retained_earnings && !current.retained_earnings {
                        if let Some((id, old, new)) =
                            accounts::release_designation(sql, tenant, company_id, Some(current.id))
                                .await?
                        {
                            changes.push(account_change(id, old, new));
                        }
                    }
                    let saved = accounts::update(sql, tenant, company_id, &next).await?;
                    changes.push(account_change(
                        saved.id,
                        accounts::to_json(&current),
                        accounts::to_json(&saved),
                    ));
                    Ok((saved, changes))
                })
            })
            .await?;
        Ok(account)
    }

    /// Adds an account group (`finance.ledger.accounts.manage`): a row of the
    /// chart that takes no postings and stands for the total of the accounts
    /// and groups under it. Its code is unique among the company's accounts
    /// and groups; its type and any enclosing group must match.
    pub async fn create_account_group(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewAccountGroup,
    ) -> Result<Account> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.create_account_group",
            )
            .await?;
        let group = Self::create_account_group_on(&mut tx, scope, company_id, new).await?;
        tx.commit().await?;
        Ok(group)
    }

    async fn create_account_group_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewAccountGroup,
    ) -> Result<Account> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        let valid = accounts::validate_new_group(tx.sql(), tenant, company_id, new)
            .await?
            .map_err(LedgerError::Invalid)?;
        let group = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let group = accounts::insert(sql, tenant, company_id, &valid).await?;
                    let change = account_change(
                        group.id,
                        serde_json::Value::Null,
                        accounts::to_json(&group),
                    );
                    Ok((group, vec![change]))
                })
            })
            .await?;
        Ok(group)
    }

    /// Changes an account group's name or the group it sits in
    /// (`finance.ledger.accounts.manage`). A group never sits under itself or
    /// anything under it.
    pub async fn update_account_group(
        &self,
        scope: &Scope,
        company_id: i64,
        group_id: i64,
        change: &AccountGroupChange,
    ) -> Result<Account> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.update_account_group",
            )
            .await?;
        let group =
            Self::update_account_group_on(&mut tx, scope, company_id, group_id, change).await?;
        tx.commit().await?;
        Ok(group)
    }

    async fn update_account_group_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        group_id: i64,
        change: &AccountGroupChange,
    ) -> Result<Account> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        let current = accounts::find(tx.sql(), tenant, company_id, group_id)
            .await?
            .ok_or(LedgerError::AccountNotFound)?;
        let next = accounts::validate_group_change(tx.sql(), tenant, company_id, &current, change)
            .await?
            .map_err(LedgerError::Invalid)?;
        let group = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let saved = accounts::update(sql, tenant, company_id, &next).await?;
                    let change = account_change(
                        saved.id,
                        accounts::to_json(&current),
                        accounts::to_json(&saved),
                    );
                    Ok((saved, vec![change]))
                })
            })
            .await?;
        Ok(group)
    }

    /// The starter charts a company may choose from, each with its rows.
    pub async fn chart_templates(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<ChartTemplate>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        saved_templates::all(&**client, scope.tenant()).await
    }

    /// What applying the starter chart `key` with the optional `sections`
    /// chosen to the company would do now (`finance.ledger.view`); `Invalid`
    /// for a key or section that names none.
    pub async fn preview_chart_template(
        &self,
        scope: &Scope,
        company_id: i64,
        key: &str,
        sections: &[String],
    ) -> Result<ChartTemplatePreview> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        chart_template_preview(&**client, scope.tenant(), company_id, key, sections).await
    }

    /// Creates the starter chart `key`'s core groups and accounts, and those
    /// of the optional `sections` chosen, in one transaction (`finance.ledger.accounts.manage`), for a company whose
    /// chart is empty. A chart with any account or group is never changed: it
    /// is refused, and so is a second application. The rows are the
    /// company's own afterwards; the template is not linked to them.
    pub async fn apply_chart_template(
        &self,
        scope: &Scope,
        company_id: i64,
        key: &str,
        sections: &[String],
    ) -> Result<ChartTemplateApplied> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_ACCOUNTS],
                "finance.ledger.apply_chart_template",
            )
            .await?;
        let applied =
            Self::apply_chart_template_on(&mut tx, scope, company_id, key, sections).await?;
        tx.commit().await?;
        Ok(applied)
    }

    async fn apply_chart_template_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        key: &str,
        sections: &[String],
    ) -> Result<ChartTemplateApplied> {
        join(tx, scope, company_id, MANAGE_ACCOUNTS).await?;
        let tenant = scope.tenant();
        let preview = chart_template_preview(tx.sql(), tenant, company_id, key, sections).await?;
        if !preview.applicable {
            return Err(LedgerError::Invalid(vec![FieldError::new(
                "base",
                format!(
                    "The chart already has {} account(s) or group(s). A starter chart applies only to an empty chart.",
                    preview.existing_rows
                ),
            )]));
        }
        let template = preview.template;
        let rows = template.rows.clone();
        let chosen = preview.chosen_sections;
        let applied = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut ids: std::collections::BTreeMap<&str, i64> = Default::default();
                    let mut changes = Vec::new();
                    let (mut groups, mut accounts_made) = (0, 0);
                    for row in &rows {
                        let valid = accounts::ValidAccount {
                            code: row.code.clone(),
                            name: row.name.clone(),
                            account_type: row.account_type,
                            parent_id: row.parent_code.as_deref().and_then(|c| ids.get(c).copied()),
                            is_group: row.is_group,
                            active: true,
                            retained_earnings: row.retained_earnings,
                        };
                        let account = accounts::insert(sql, tenant, company_id, &valid).await?;
                        ids.insert(row.code.as_str(), account.id);
                        if row.is_group {
                            groups += 1;
                        } else {
                            accounts_made += 1;
                        }
                        changes.push(account_change(
                            account.id,
                            serde_json::Value::Null,
                            accounts::to_json(&account),
                        ));
                    }
                    Ok((
                        ChartTemplateApplied {
                            key: template.key.clone(),
                            sections: chosen,
                            groups,
                            accounts: accounts_made,
                        },
                        changes,
                    ))
                })
            })
            .await?;
        Ok(applied)
    }

    /// Posts a journal entry (`finance.ledger.entries.post`). It is refused
    /// with [`LedgerError::Invalid`] unless it has at least two lines, its
    /// debits equal its credits, and every line names an active account of the
    /// company with exactly one positive amount no finer than the book's minor
    /// units; `BookNotOpen` before the book is opened. The database numbers
    /// the entry (1, 2, 3, ... per company, never skipping) and stamps it.
    /// `options.owner` marks the entry as the one a module's record
    /// recognises, and `settlement_account_ids` is the settlement exception to
    /// the active-account rule; an `owner` or id of another shape is
    /// `InvalidOption`. The entry's date must fall in an open period: never
    /// in a closed or locked one (`entry_date falls in a closed period`),
    /// never before the first fiscal year (`is before the first fiscal
    /// year`), and, for a controlled company, never where no period exists.
    pub async fn post_entry(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewEntry,
        options: &PostOptions,
    ) -> Result<Entry> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[POST_ENTRIES],
                "finance.ledger.post_entry",
            )
            .await?;
        let entry = self
            .post_entry_in(&mut tx, scope, company_id, new, options)
            .await?;
        tx.commit().await?;
        Ok(entry)
    }

    /// Validate a proposed owned posting on the caller's review connection,
    /// without allocating an identity or number. Requires posting authority,
    /// not view. The deterministic JSON contains the normalized entry, book,
    /// referenced accounts and posting controls; compare it again at execution.
    /// For a record not yet allocated, pass no owner and describe its future
    /// ownership in the caller's review. The real post still checks that owner.
    pub async fn preview_post_entry_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        new: &NewEntry,
        options: &PostOptions,
    ) -> Result<serde_json::Value> {
        preview_lock(client, scope, company_id).await?;
        let tenant = scope.tenant();
        let (book, valid) = prepare_post(client, tenant, company_id, new, options).await?;
        let calendar = periods::state(client, tenant, company_id).await?;
        let period = calendar
            .periods()
            .find(|p| p.start_date <= valid.entry_date && valid.entry_date <= p.end_date);
        Ok(serde_json::json!({
            "book":book,"entry":valid,
            "accounts":accounts::list(client,tenant,company_id).await?.into_iter().filter(|a|new.lines.iter().any(|l|l.account_id==a.id)).collect::<Vec<_>>(),
            "period":period,
            "bookkeeping":bookkeeping::latest(client,tenant,company_id).await?,
            "settlement_account_ids":options.settlement_account_ids,
        }))
    }

    /// Preview the owner's correction on the handed review connection. Uses
    /// the same owner, date, period and double-reversal checks as execution;
    /// returns the original and normalized mirror with its posting controls.
    /// Like the correction itself, this does not require ledger view.
    pub async fn preview_reverse_owned_entry_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
        owner: &EntryOwner,
        reversal: &Reversal,
    ) -> Result<serde_json::Value> {
        preview_lock(client, scope, company_id).await?;
        if !entries::owner_valid(owner) {
            return Err(LedgerError::InvalidOption);
        }
        let tenant = scope.tenant();
        let (book, original, valid) =
            prepare_reverse(client, tenant, company_id, entry_id, Some(owner), reversal).await?;
        let calendar = periods::state(client, tenant, company_id).await?;
        let period = calendar
            .periods()
            .find(|p| p.start_date <= valid.entry_date && valid.entry_date <= p.end_date);
        Ok(serde_json::json!({
            "book":book,"original":original,"entry":valid,
            "accounts":accounts::list(client,tenant,company_id).await?.into_iter().filter(|a|original.lines.iter().any(|l|l.account_id==a.id)).collect::<Vec<_>>(),
            "period":period,
            "bookkeeping":bookkeeping::latest(client,tenant,company_id).await?,
        }))
    }

    /// Posts inside the record owner's audited transaction, leaving commit
    /// to that owner so its record and the entry stand or fall together.
    /// Applies the same authorization, company lock and validation as
    /// [`Ledger::post_entry`]; never call the standalone method while holding
    /// the company lock on another connection.
    pub async fn post_entry_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewEntry,
        options: &PostOptions,
    ) -> Result<Entry> {
        join(tx, scope, company_id, POST_ENTRIES).await?;
        let tenant = scope.tenant();
        let (book, valid) = prepare_post(tx.sql(), tenant, company_id, new, options).await?;
        let actor = scope.user();
        let impersonator = scope.impersonator().map(|operator| operator.user());
        let entry = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let posted = entries::insert(
                        sql,
                        tenant,
                        company_id,
                        &book.currency_code,
                        actor,
                        impersonator,
                        &valid,
                    )
                    .await?;
                    let entry = posted.entry.clone();
                    Ok((entry, entry_changes(posted)))
                })
            })
            .await?;
        Ok(entry)
    }

    /// Decides an approval without recording its exception. The exception includes
    /// its normalized reason and separation-change basis, not just its presence.
    pub async fn preview_approval_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        approval: &Approval,
    ) -> Result<Value> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        let exception =
            duties::decide(client, scope.tenant(), company_id, people(scope), approval).await?;
        Ok(json!({
            "approval_exception": exception,
            "duty_separation": separation_view(client, scope.tenant(), company_id).await?,
        }))
    }

    /// One posted entry with its lines (`finance.ledger.view`), or
    /// `EntryNotFound` for a missing id or another company's.
    pub async fn get_entry(&self, scope: &Scope, company_id: i64, entry_id: i64) -> Result<Entry> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.get_entry_on(&**client, scope, company_id, entry_id)
            .await
    }

    /// Authorized entry read on the caller's connection, including inside a
    /// sibling write holding the company lock. Never acquire a second pool slot.
    pub async fn get_entry_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
    ) -> Result<Entry> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        entries::find(client, scope.tenant(), company_id, entry_id)
            .await?
            .ok_or(LedgerError::EntryNotFound)
    }

    /// Posted entries with their lines, ordered by entry date and then id
    /// (`finance.ledger.view`). A caller pages by passing the last returned
    /// entry's date and id as `after` until fewer than `limit` come back;
    /// `exclude_closing` leaves out year-end closing entries and their
    /// reversals. An out-of-range option is `InvalidOption`.
    pub async fn list_entries(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &EntryQuery,
    ) -> Result<Vec<Entry>> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.list_entries_on(&**client, scope, company_id, query)
            .await
    }

    /// Authorized [`Ledger::list_entries`] on the caller's connection,
    /// including inside a sibling write holding the company lock. Never
    /// acquire a second pool slot.
    pub async fn list_entries_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        query: &EntryQuery,
    ) -> Result<Vec<Entry>> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        entries::list(client, scope.tenant(), company_id, query).await
    }

    pub async fn latest_account_entry_number_on(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        account_id: i64,
        through: Date,
    ) -> Result<i64> {
        authorize(client, scope, company_id, VIEW).await?;
        Ok(entries::latest_account_entry_number(
            client,
            scope.tenant(),
            company_id,
            account_id,
            through,
        )
        .await?)
    }

    /// Subscribes to the company's postings (`finance.ledger.view`). After
    /// the transaction that posts an entry commits, the subscription hears
    /// [`PostingEvent::Posted`] for it, from any process; a rolled-back post
    /// announces nothing, and a post inside a caller's own transaction is
    /// announced when that transaction commits. A notice carries no figures:
    /// read them through this facade, which authorizes the read afresh. The
    /// subscription listens before this returns, so it hears every posting
    /// committed after.
    pub async fn subscribe_postings(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<PostingSubscription> {
        {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, VIEW).await?;
        }
        self.postings
            .subscribe(scope.tenant(), company_id)
            .await
            .ok_or(LedgerError::NoticesUnavailable)
    }

    // Totals

    /// The trial balance as of a date (`finance.ledger.view`): every account
    /// with a posting dated on or before `as_of`, by code, with its debit and
    /// credit totals and its balance on one side. An account reports its own
    /// postings only; `parent_id` lets a reader group rows without counting a
    /// parent twice. The result names `through_entry_number`, the last entry
    /// it counts: passing it back with the same `as_of` reproduces the
    /// figures exactly, even after an entry is posted later with an earlier
    /// date. A negative number is `InvalidOption`; `BookNotOpen` before the
    /// book is opened.
    pub async fn trial_balance(
        &self,
        scope: &Scope,
        company_id: i64,
        as_of: Date,
        through_entry_number: Option<i64>,
    ) -> Result<TrialBalance> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.trial_balance_on(&**client, scope, company_id, as_of, through_entry_number)
            .await
    }

    /// Authorized figures on the caller's database snapshot.
    pub async fn trial_balance_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        as_of: Date,
        through_entry_number: Option<i64>,
    ) -> Result<TrialBalance> {
        authorize(client, scope, company_id, VIEW).await?;
        trial_balance_of(
            client,
            scope.tenant(),
            company_id,
            as_of,
            through_entry_number,
        )
        .await
    }

    /// Each account's balance and each group's total as of a date
    /// (`finance.ledger.view`), every row of the chart by code, counted
    /// through one pinned entry number like [`Ledger::trial_balance`]. A
    /// group's total sums the accounts under it; it is never an account's own
    /// balance, and no posting is counted twice.
    pub async fn chart_balances(
        &self,
        scope: &Scope,
        company_id: i64,
        as_of: Date,
        through_entry_number: Option<i64>,
    ) -> Result<ChartBalances> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        chart_balances_of(
            &**client,
            scope.tenant(),
            company_id,
            as_of,
            through_entry_number,
        )
        .await
    }

    /// Each account's debit and credit totals over the entries dated `from`
    /// through `to` (`finance.ledger.view`): every account with a posting in
    /// that window, by code, summed by PostgreSQL. The result names
    /// `through_entry_number`, the last entry it counts; several windows read
    /// with the same number describe one state of the ledger, which is how a
    /// report keeps its columns consistent. `exclude_closing` leaves out
    /// year-end closing entries and their reversals (a profit and loss reads
    /// this way, a balance sheet counts them). A negative number or a `from`
    /// after `to` is `InvalidOption`; `BookNotOpen` before the book is opened.
    pub async fn account_totals(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &TotalsQuery,
    ) -> Result<AccountTotals> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.account_totals_on(&**client, scope, company_id, query)
            .await
    }

    /// Authorized aggregate read on the caller's snapshot or write transaction.
    pub async fn account_totals_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        query: &TotalsQuery,
    ) -> Result<AccountTotals> {
        let window = totals::Window::new(query)?;
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        let tenant = scope.tenant();
        let book = books::find(client, tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let (through, rows) = totals::by_account(client, tenant, company_id, &window).await?;
        Ok(Totals {
            company_id,
            from: window.from,
            to: window.to,
            through_entry_number: through,
            currency_code: book.currency_code,
            minor_units: book.minor_units,
            rows,
        })
    }

    /// [`Ledger::account_totals`] split by owner (`finance.ledger.view`): one
    /// row per account and owner module that posted to it in the window, by
    /// code and then owner, entries with no owner first. The same query and
    /// the same `through_entry_number` reproduce the same figures. A control
    /// account ties out when its owner module's rows are the whole account:
    /// any other row is a posting the owner's own records do not know, shown
    /// as a difference, never netted away.
    pub async fn account_totals_by_owner(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &TotalsQuery,
    ) -> Result<OwnerTotals> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.account_totals_by_owner_on(&**client, scope, company_id, query)
            .await
    }

    /// Owner totals on the caller's snapshot, with the same authorization and pins.
    pub async fn account_totals_by_owner_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
        query: &TotalsQuery,
    ) -> Result<OwnerTotals> {
        let window = totals::Window::new(query)?;
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        let tenant = scope.tenant();
        let book = books::find(client, tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let (through, rows) = totals::by_owner(client, tenant, company_id, &window).await?;
        Ok(Totals {
            company_id,
            from: window.from,
            to: window.to,
            through_entry_number: through,
            currency_code: book.currency_code,
            minor_units: book.minor_units,
            rows,
        })
    }

    // Evidence

    /// The datasets [`Ledger::evidence_page`] exports, one per table this
    /// module owns, sorted.
    pub fn evidence_datasets() -> Vec<&'static str> {
        evidence::datasets()
    }

    /// One ID-ordered evidence page of `dataset` with explicit fields, rows
    /// and next cursor (`finance.ledger.view`), for the auditor's pack.
    /// `limit` is 1 to 500 (500 when absent) and `after` the last id of the
    /// previous page; an unknown dataset or an option out of range is
    /// `InvalidOption`. Each page is a live read on its own connection, so
    /// pages read across a posting are not one snapshot of the ledger.
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
        evidence::page(client, scope.tenant(), company_id, dataset, limit, after).await
    }

    // Reversals

    /// Posts the reversal of a manual entry (`finance.ledger.entries.post`):
    /// the same accounts and amounts with debits and credits swapped, dated
    /// `reversal.entry_date`, not before the original's (`entry_date cannot be
    /// before the reversed entry's date`). An entry is reversed at most once
    /// (`AlreadyReversed`), and a reversal is not itself reversed
    /// (`ReversalNotReversible`): post a new entry instead. Accounts
    /// deactivated since the original still take its reversal, but its date
    /// must fall in an open period, as a new entry's does. A year-end closing
    /// entry is reversed only by [`Ledger::reopen_fiscal_year`]
    /// (`ClosingEntry`). An entry another module owns is
    /// [`LedgerError::OwnedBy`]: correct it through that module, which records
    /// its own reversal and calls [`Ledger::reverse_owned_entry`].
    pub async fn reverse_entry(
        &self,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
        reversal: &Reversal,
    ) -> Result<Entry> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[POST_ENTRIES],
                "finance.ledger.reverse_entry",
            )
            .await?;
        let entry = reverse(&mut tx, scope, company_id, entry_id, None, reversal).await?;
        tx.commit().await?;
        Ok(entry)
    }

    /// The owner's correction path (`finance.ledger.entries.post`): posts the
    /// reversal of an entry the calling module posted with `owner` (the same
    /// module and record as [`PostOptions::owner`]), as
    /// [`Ledger::reverse_entry`] does for a manual entry and with the same
    /// refusals. The owner calls it inside the transaction that records its own
    /// reversal (a payment reversal, a cancellation), so both stand or
    /// neither; it authorizes and takes the company lock on that transaction,
    /// and leaves the commit to the owner. The reversal carries the same owner.
    /// An entry owned by another module or record is `NotTheOwner`, a manual
    /// entry `NotOwned`, and an `owner` of another shape `InvalidOption`.
    ///
    /// Only the owning module calls this, from its own correction path; use
    /// [`Ledger::reverse_entry`] for a manual entry.
    pub async fn reverse_owned_entry(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
        owner: &EntryOwner,
        reversal: &Reversal,
    ) -> Result<Entry> {
        // Authorized first, as Bilimbi's `write/4` is: a caller out of reach
        // learns nothing about the owner it passed.
        join(tx, scope, company_id, POST_ENTRIES).await?;
        if !entries::owner_valid(owner) {
            return Err(LedgerError::InvalidOption);
        }
        reverse(tx, scope, company_id, entry_id, Some(owner), reversal).await
    }

    // Separation of duties

    /// The company's separation of duties (`finance.ledger.view`): the
    /// setting and its latest change. A company that never changed it
    /// requires separation.
    pub async fn get_duty_separation(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<DutySeparation> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        separation_view(&**client, scope.tenant(), company_id).await
    }

    /// Turns the company's separation of duties on or off
    /// (`finance.ledger.duty-separation.manage`); turning it off needs a
    /// `reason` (`reason is required to turn separation off`). The change is
    /// appended with the actor, any impersonator and the database's time; a
    /// call that changes nothing records nothing. While separation is off, an
    /// approval by the person who prepared the record is allowed and listed
    /// in the approval exceptions with this change's reason.
    pub async fn set_duty_separation(
        &self,
        scope: &Scope,
        company_id: i64,
        required: bool,
        reason: Option<&str>,
    ) -> Result<DutySeparation> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_DUTY_SEPARATION],
                "finance.ledger.set_duty_separation",
            )
            .await?;
        let tenant = scope.tenant();
        let reason = duties::validate_change(required, reason).map_err(LedgerError::Invalid)?;
        let latest = duties::latest_change(tx.sql(), tenant, company_id).await?;
        if duties::required(latest.as_ref()) == required {
            tx.rollback().await?;
            return Ok(DutySeparation {
                required,
                change: latest,
            });
        }
        let (actor, impersonator) = people(scope);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let change = duties::record_change(
                    sql,
                    tenant,
                    company_id,
                    actor,
                    impersonator,
                    required,
                    reason.as_deref(),
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        let view = separation_view(tx.sql(), tenant, company_id).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Every change of the company's separation of duties, oldest first
    /// (`finance.ledger.view`).
    pub async fn list_duty_separation_changes(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<DutySeparationChange>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(duties::changes(&**client, scope.tenant(), company_id).await?)
    }

    /// Whether the scope's actor may approve what `approval` describes, for
    /// the Finance modules that approve: called inside the transaction that
    /// records the approval, which it authorizes (`finance.ledger.view`, which
    /// the exceptions are read with) and locks the company on, so a refusal
    /// rolls the approval back. When the approver, or whoever impersonated
    /// them, is someone in `prepared_by` or impersonated someone there, the
    /// approval is `SamePerson` while the company requires separation; while
    /// it does not, it is allowed and recorded as a same-person approval
    /// exception with the reason separation was turned off. An override is
    /// always recorded with its `override_reason`. Anything else records
    /// nothing. A malformed `source`, code, subject or reason is
    /// [`LedgerError::Invalid`].
    pub async fn check_approval(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        approval: &Approval,
    ) -> Result<()> {
        join(tx, scope, company_id, VIEW).await?;
        let (tenant, approver) = (scope.tenant(), people(scope));
        if let Some(exception) =
            duties::decide(tx.sql(), tenant, company_id, approver, approval).await?
        {
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let change =
                        duties::record_exception(sql, tenant, company_id, approver, &exception)
                            .await?;
                    Ok(((), vec![change]))
                })
            })
            .await?;
        }
        Ok(())
    }

    /// The company's approval exceptions, oldest first, each with its review
    /// (`finance.ledger.view`). An out-of-range option is `InvalidOption`.
    pub async fn list_approval_exceptions(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &ApprovalExceptionQuery,
    ) -> Result<Vec<ApprovalException>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        duties::exceptions(&**client, scope.tenant(), company_id, query).await
    }

    /// One approval exception, as [`Ledger::list_approval_exceptions`] shows
    /// it (`finance.ledger.view`), or `ExceptionNotFound`.
    pub async fn get_approval_exception(
        &self,
        scope: &Scope,
        company_id: i64,
        exception_id: i64,
    ) -> Result<ApprovalException> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        duties::exception(&**client, scope.tenant(), company_id, exception_id)
            .await?
            .ok_or(LedgerError::ExceptionNotFound)
    }

    /// Records an independent review of an approval exception with an
    /// optional `note` (`finance.ledger.approval-exceptions.review`): the
    /// reviewer, any impersonator and the database's time. An exception is
    /// reviewed once (`AlreadyReviewed`), and never by whoever approved or
    /// prepared what it records, nor anyone impersonating them or impersonated
    /// by them (`ReviewerNotIndependent`), whatever the company's separation
    /// setting.
    pub async fn review_approval_exception(
        &self,
        scope: &Scope,
        company_id: i64,
        exception_id: i64,
        note: Option<&str>,
    ) -> Result<ApprovalException> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[REVIEW_EXCEPTIONS],
                "finance.ledger.review_approval_exception",
            )
            .await?;
        let tenant = scope.tenant();
        let exception = duties::exception(tx.sql(), tenant, company_id, exception_id)
            .await?
            .ok_or(LedgerError::ExceptionNotFound)?;
        let reviewer = people(scope);
        if exception.review.is_some() {
            return Err(LedgerError::AlreadyReviewed);
        }
        let approver = (
            exception.approved_by_user_id,
            exception.approved_by_impersonator_id,
        );
        let preparer = exception
            .prepared_by_user_id
            .map(|user| (user, exception.prepared_by_impersonator_id));
        if same_person(reviewer, approver)
            || preparer.is_some_and(|preparer| same_person(reviewer, preparer))
        {
            return Err(LedgerError::ReviewerNotIndependent);
        }
        let note = duties::validate_note(note).map_err(LedgerError::Invalid)?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let change = duties::record_review(
                    sql,
                    tenant,
                    company_id,
                    exception_id,
                    reviewer,
                    note.as_deref(),
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        let reviewed = duties::exception(tx.sql(), tenant, company_id, exception_id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(reviewed)
    }

    // Manual journals

    /// Submits a manual journal for approval (`finance.ledger.journals.submit`):
    /// `entry_date`, `description`, an optional `reference` and `lines`, as
    /// [`Ledger::post_entry`] takes them. It is checked as an entry would be
    /// (balanced, active accounts of the company, an open period) and kept,
    /// numbered per company, with the submitter and the database's time;
    /// nothing posts until [`Ledger::approve_journal`]. `BookNotOpen` before
    /// the book is opened.
    pub async fn submit_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewEntry,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[SUBMIT_JOURNALS],
                "finance.ledger.submit_journal",
            )
            .await?;
        let journal = Self::submit_journal_on(&mut tx, scope, company_id, new).await?;
        tx.commit().await?;
        Ok(journal)
    }

    async fn submit_journal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewEntry,
    ) -> Result<Journal> {
        join(tx, scope, company_id, SUBMIT_JOURNALS).await?;
        let tenant = scope.tenant();
        let book = books::find(tx.sql(), tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let valid = entries::validate(
            tx.sql(),
            tenant,
            company_id,
            book.minor_units,
            new,
            None,
            entries::Inactive::Listed(&[]),
        )
        .await?
        .map_err(LedgerError::Invalid)?;
        journal_approvals::submission_policy(tx.sql(), scope, company_id).await?;
        let submitter = people(scope);
        let journal = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    journals::insert(
                        sql,
                        tenant,
                        company_id,
                        &book.currency_code,
                        submitter,
                        &valid,
                    )
                    .await
                })
            })
            .await?;
        journal_approvals::submit(tx, scope, company_id, journal.id).await?;
        Ok(journal)
    }

    /// Approves a submitted journal (`finance.ledger.journals.approve`), which
    /// posts it as an entry on its date with its lines, by the approver, and
    /// records the `approved` decision naming the entry. The entry is checked
    /// as [`Ledger::post_entry`] checks one, now: a period closed or an
    /// account deactivated since submission is [`LedgerError::Invalid`], and
    /// the journal stays submitted. Workflow proves live eligibility and
    /// independence under the journal's saved rules and current safety floors.
    /// A permitted preparer approval records one Workflow exception. An
    /// intermediate vote leaves the journal waiting; only the final vote posts.
    /// Anything but a submitted journal is `JournalDecided`.
    pub async fn approve_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[APPROVE_JOURNALS],
                "finance.ledger.approve_journal",
            )
            .await?;
        let journal = Self::approve_journal_on(&mut tx, scope, company_id, journal_id).await?;
        tx.commit().await?;
        Ok(journal)
    }

    async fn approve_journal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
    ) -> Result<Journal> {
        journal_approvals::decide(tx, scope, company_id, journal_id, true, "").await
    }

    async fn apply_journal_approval_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
    ) -> Result<Journal> {
        join(tx, scope, company_id, APPROVE_JOURNALS).await?;
        let tenant = scope.tenant();
        let book = books::find(tx.sql(), tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let journal = submitted_journal(tx.sql(), tenant, company_id, journal_id).await?;
        let approver = people(scope);
        let valid = entries::validate(
            tx.sql(),
            tenant,
            company_id,
            book.minor_units,
            &journals::entry(&journal),
            None,
            entries::Inactive::Listed(&[]),
        )
        .await?
        .map_err(LedgerError::Invalid)?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                let posted = entries::insert(
                    sql,
                    tenant,
                    company_id,
                    &book.currency_code,
                    approver.0,
                    approver.1,
                    &valid,
                )
                .await?;
                let entry_id = posted.entry.id;
                changes.extend(entry_changes(posted));
                changes.push(
                    journals::decide(
                        sql,
                        tenant,
                        company_id,
                        journal_id,
                        approver,
                        JournalStatus::Approved,
                        None,
                        Some(entry_id),
                    )
                    .await?,
                );
                Ok(((), changes))
            })
        })
        .await?;
        let journal = journals::find(tx.sql(), tenant, company_id, journal_id)
            .await?
            .ok_or(Error::Internal)?;
        Ok(journal)
    }

    /// Rejects a submitted journal with a `reason`
    /// (`finance.ledger.journals.approve`; `reason is required to reject a
    /// journal`): it never posts. Anything but a submitted journal is
    /// `JournalDecided`.
    pub async fn reject_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: &str,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[APPROVE_JOURNALS],
                "finance.ledger.reject_journal",
            )
            .await?;
        let journal =
            Self::reject_journal_on(&mut tx, scope, company_id, journal_id, reason).await?;
        tx.commit().await?;
        Ok(journal)
    }

    async fn reject_journal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: &str,
    ) -> Result<Journal> {
        journals::validate_reason(reason).map_err(LedgerError::Invalid)?;
        journal_approvals::decide(tx, scope, company_id, journal_id, false, reason).await
    }

    async fn apply_journal_rejection_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: &str,
    ) -> Result<Journal> {
        join(tx, scope, company_id, APPROVE_JOURNALS).await?;
        let tenant = scope.tenant();
        submitted_journal(tx.sql(), tenant, company_id, journal_id).await?;
        let reason = journals::validate_reason(reason).map_err(LedgerError::Invalid)?;
        let actor = people(scope);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let change = journals::decide(
                    sql,
                    tenant,
                    company_id,
                    journal_id,
                    actor,
                    JournalStatus::Rejected,
                    Some(&reason),
                    None,
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        let journal = journals::find(tx.sql(), tenant, company_id, journal_id)
            .await?
            .ok_or(Error::Internal)?;
        Ok(journal)
    }

    /// Takes back a submitted journal its submitter no longer wants approved
    /// (`finance.ledger.journals.submit`), with an optional `reason`. Nothing
    /// posts; the journal stays on record, `withdrawn`, with who withdrew it
    /// and when, and cannot be approved. Only the person who submitted it can
    /// withdraw it (`NotSubmitter`); anything but a submitted journal is
    /// `JournalDecided`. To correct it, use [`Ledger::replace_journal`].
    pub async fn withdraw_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: Option<&str>,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[SUBMIT_JOURNALS],
                "finance.ledger.withdraw_journal",
            )
            .await?;
        let journal =
            Self::withdraw_journal_on(&mut tx, scope, company_id, journal_id, reason).await?;
        tx.commit().await?;
        Ok(journal)
    }

    async fn withdraw_journal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: Option<&str>,
    ) -> Result<Journal> {
        join(tx, scope, company_id, SUBMIT_JOURNALS).await?;
        let tenant = scope.tenant();
        withdrawable_journal(tx.sql(), scope, company_id, journal_id).await?;
        let reason = journals::validate_withdrawal_reason(reason).map_err(LedgerError::Invalid)?;
        Self::record_withdrawal(tx, scope, company_id, journal_id, reason).await?;
        let journal = journals::find(tx.sql(), tenant, company_id, journal_id)
            .await?
            .ok_or(Error::Internal)?;
        Ok(journal)
    }

    async fn record_withdrawal(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: Option<String>,
    ) -> Result<()> {
        journal_approvals::recall(tx, scope, company_id, journal_id, reason.as_deref()).await
    }

    async fn apply_journal_withdrawal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        reason: Option<String>,
    ) -> Result<()> {
        let tenant = scope.tenant();
        let actor = people(scope);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let change = journals::decide(
                    sql,
                    tenant,
                    company_id,
                    journal_id,
                    actor,
                    JournalStatus::Withdrawn,
                    reason.as_deref(),
                    None,
                )
                .await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        Ok(())
    }

    /// Corrects a submitted journal (`finance.ledger.journals.submit`): withdraws
    /// it and submits `new` in its place, in one transaction, so a refusal of
    /// either leaves the journal waiting as it was. The replacement is checked
    /// and numbered as [`Ledger::submit_journal`] does and names the journal it
    /// replaces; it needs its own approval, and the withdrawn one can never
    /// post. The same rules as [`Ledger::withdraw_journal`] apply.
    pub async fn replace_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        new: &NewEntry,
        reason: Option<&str>,
    ) -> Result<Journal> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[SUBMIT_JOURNALS],
                "finance.ledger.replace_journal",
            )
            .await?;
        let journal =
            Self::replace_journal_on(&mut tx, scope, company_id, journal_id, new, reason).await?;
        tx.commit().await?;
        Ok(journal)
    }

    async fn replace_journal_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
        new: &NewEntry,
        reason: Option<&str>,
    ) -> Result<Journal> {
        join(tx, scope, company_id, SUBMIT_JOURNALS).await?;
        let tenant = scope.tenant();
        withdrawable_journal(tx.sql(), scope, company_id, journal_id).await?;
        opening::refuse_manual_replacement(tx.sql(), scope, company_id, journal_id).await?;
        let reason = journals::validate_withdrawal_reason(reason).map_err(LedgerError::Invalid)?;
        // The replacement is checked and kept first, so a refusal of it
        // leaves nothing recorded; the withdrawal then names it.
        let replacement = Self::submit_journal_on(tx, scope, company_id, new).await?;
        Self::record_withdrawal(tx, scope, company_id, journal_id, reason).await?;
        let replacement_id = replacement.id;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let change =
                    journals::link(sql, tenant, company_id, replacement_id, journal_id).await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        let journal = journals::find(tx.sql(), tenant, company_id, replacement_id)
            .await?
            .ok_or(Error::Internal)?;
        Ok(journal)
    }

    /// One journal with its lines, status and decision
    /// (`finance.ledger.view`), or `JournalNotFound`.
    pub async fn get_journal(
        &self,
        scope: &Scope,
        company_id: i64,
        journal_id: i64,
    ) -> Result<Journal> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        journals::find(&**client, scope.tenant(), company_id, journal_id)
            .await?
            .ok_or(LedgerError::JournalNotFound)
    }

    /// The company's journals by number, as [`Ledger::get_journal`] shows
    /// them (`finance.ledger.view`). An out-of-range option is
    /// `InvalidOption`.
    pub async fn list_journals(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &JournalQuery,
    ) -> Result<Vec<Journal>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        journals::list(&**client, scope.tenant(), company_id, query).await
    }

    /// The statuses a journal moves through.
    pub fn journal_statuses() -> &'static [JournalStatus] {
        &JournalStatus::ALL
    }

    // Controlled bookkeeping

    /// The company's controlled bookkeeping (`finance.ledger.view`). A company
    /// with no change is not controlled; a new company's book opens
    /// controlled.
    pub async fn get_controlled_bookkeeping(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<ControlledBookkeeping> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        controlled_view(&**client, scope.tenant(), company_id).await
    }

    /// Turns the company's controlled bookkeeping on or off
    /// (`finance.ledger.controlled-bookkeeping.manage`); turning it off needs
    /// a `reason` (`reason is required to turn this off`). The change is
    /// appended with the actor, any impersonator and the database's time; a
    /// call that changes nothing records nothing. Turning it off overrides a
    /// control, so it is also recorded as an approval exception with its
    /// reason.
    pub async fn set_controlled_bookkeeping(
        &self,
        scope: &Scope,
        company_id: i64,
        controlled: bool,
        reason: Option<&str>,
    ) -> Result<ControlledBookkeeping> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_CONTROLLED_BOOKKEEPING],
                "finance.ledger.set_controlled_bookkeeping",
            )
            .await?;
        let tenant = scope.tenant();
        let reason = bookkeeping::validate(controlled, reason).map_err(LedgerError::Invalid)?;
        if bookkeeping::controlled(tx.sql(), tenant, company_id).await? == controlled {
            let view = controlled_view(tx.sql(), tenant, company_id).await?;
            tx.rollback().await?;
            return Ok(view);
        }
        let (actor, impersonator) = people(scope);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (id, recorded) = bookkeeping::record(
                    sql,
                    tenant,
                    company_id,
                    actor,
                    impersonator,
                    controlled,
                    reason.as_deref(),
                )
                .await?;
                let mut changes = vec![Change {
                    kind: "finance.ledger.bookkeeping_change".into(),
                    id,
                    old: serde_json::Value::Null,
                    new: recorded,
                }];
                if let (false, Some(reason)) = (controlled, reason.as_deref()) {
                    changes.push(
                        duties::record_override(
                            sql,
                            tenant,
                            company_id,
                            actor,
                            impersonator,
                            &duties::Override {
                                action: "controlled_bookkeeping_off",
                                subject_type: "bookkeeping",
                                subject_id: id,
                                subject: "Controlled bookkeeping of the company".into(),
                                reason,
                            },
                        )
                        .await?,
                    );
                }
                Ok(((), changes))
            })
        })
        .await?;
        let view = controlled_view(tx.sql(), tenant, company_id).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Every change of the company's controlled bookkeeping, oldest first
    /// (`finance.ledger.view`).
    pub async fn list_controlled_bookkeeping_changes(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<BookkeepingChange>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(bookkeeping::changes(&**client, scope.tenant(), company_id).await?)
    }

    // Late recordings

    /// The late-recording report's thresholds: listed from 45 days, late
    /// past 60.
    pub fn late_recording_thresholds() -> LateRecordingThresholds {
        LateRecordingThresholds {
            approaching_days: late_recordings::APPROACHING_DAYS,
            late_days: late_recordings::LATE_DAYS,
        }
    }

    /// Posted entries recorded 45 or more days after their date, oldest entry
    /// number first, each with its review (`finance.ledger.view`); year-end
    /// closing entries and their reversals are never listed. An out-of-range
    /// option is `InvalidOption`.
    pub async fn list_late_recordings(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &LateRecordingQuery,
    ) -> Result<Vec<LateRecording>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        late_recordings::list(&**client, scope.tenant(), company_id, query).await
    }

    /// Records an independent review of a late entry with its `reason`
    /// (`finance.ledger.late-recordings.review`). The entry must be listed
    /// (`NotLateRecorded`), is reviewed once (`AlreadyReviewed`), and never by
    /// whoever posted it or impersonated its poster (`ReviewerNotIndependent`).
    pub async fn record_late_recording_review(
        &self,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
        reason: &str,
    ) -> Result<LateRecordingReview> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[REVIEW_LATE_RECORDINGS],
                "finance.ledger.record_late_recording_review",
            )
            .await?;
        let review =
            Self::record_late_recording_review_on(&mut tx, scope, company_id, entry_id, reason)
                .await?;
        tx.commit().await?;
        Ok(review)
    }

    async fn record_late_recording_review_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        entry_id: i64,
        reason: &str,
    ) -> Result<LateRecordingReview> {
        join(tx, scope, company_id, REVIEW_LATE_RECORDINGS).await?;
        let reason =
            late_recordings::prepare(tx.sql(), scope, company_id, entry_id, reason).await?;
        let tenant = scope.tenant();
        let (actor, impersonator) = people(scope);
        let review = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (review, change) = late_recordings::insert_review(
                        sql,
                        tenant,
                        company_id,
                        entry_id,
                        actor,
                        impersonator,
                        &reason,
                    )
                    .await?;
                    Ok((review, vec![change]))
                })
            })
            .await?;
        Ok(review)
    }

    // Fiscal years and periods

    /// Opens the company's next fiscal year (`finance.ledger.periods.manage`)
    /// and divides it into calendar-month periods, all open. The first year
    /// may start on any month; each later year starts the day after the
    /// previous one ends. Once a company has a fiscal year, no entry is posted
    /// before the first. `BookNotOpen` before the book is opened.
    pub async fn open_fiscal_year(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewFiscalYear,
    ) -> Result<FiscalYear> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_PERIODS],
                "finance.ledger.open_fiscal_year",
            )
            .await?;
        let year = Self::open_fiscal_year_on(&mut tx, scope, company_id, new).await?;
        tx.commit().await?;
        Ok(year)
    }

    async fn open_fiscal_year_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        new: &NewFiscalYear,
    ) -> Result<FiscalYear> {
        join(tx, scope, company_id, MANAGE_PERIODS).await?;
        let tenant = scope.tenant();
        books::find(tx.sql(), tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let state = periods::state(tx.sql(), tenant, company_id).await?;
        let dates =
            periods::validate_year(state.years.last(), new).map_err(LedgerError::Invalid)?;
        let (actor, impersonator) = people(scope);
        let year_id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    periods::insert_year(sql, tenant, company_id, actor, impersonator, dates).await
                })
            })
            .await?;
        let year = fiscal_year(tx.sql(), tenant, company_id, year_id).await?;
        Ok(year)
    }

    /// The company's fiscal years in date order, each with its status, its
    /// closing entry while closed, and its periods with theirs
    /// (`finance.ledger.view`).
    pub async fn list_fiscal_years(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<FiscalYear>> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.list_fiscal_years_on(&**client, scope, company_id)
            .await
    }

    /// Authorized period states on the caller's connection, including its writes.
    pub async fn list_fiscal_years_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<FiscalYear>> {
        agent_operations::authorize_on(client, scope, company_id, VIEW).await?;
        Ok(periods::state(client, scope.tenant(), company_id)
            .await?
            .years)
    }

    /// One fiscal year, as [`Ledger::list_fiscal_years`] shows it
    /// (`finance.ledger.view`), or `FiscalYearNotFound`.
    pub async fn get_fiscal_year(
        &self,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
    ) -> Result<FiscalYear> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        fiscal_year(&**client, scope.tenant(), company_id, fiscal_year_id).await
    }

    /// The fiscal year's closing log, oldest first: every close, reopen and
    /// lock of the year and of its periods, with its reason, who recorded it
    /// and when (`finance.ledger.view`). The company's events are numbered
    /// 1, 2, 3, ... without gaps.
    pub async fn list_closing_events(
        &self,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
    ) -> Result<Vec<ClosingEvent>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let tenant = scope.tenant();
        fiscal_year(&**client, tenant, company_id, fiscal_year_id).await?;
        Ok(periods::events(&**client, tenant, company_id, fiscal_year_id).await?)
    }

    /// Closes an open period with an optional `reason`
    /// (`finance.ledger.periods.manage`): no entry is posted into it until it
    /// reopens. Periods close in date order (`EarlierPeriodOpen`), and a
    /// year's last period closes only with the year (`ClosesWithFiscalYear`).
    pub async fn close_period(
        &self,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: Option<&str>,
    ) -> Result<Period> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_PERIODS],
                "finance.ledger.close_period",
            )
            .await?;
        let period = Self::close_period_on(&mut tx, scope, company_id, period_id, reason).await?;
        tx.commit().await?;
        Ok(period)
    }

    async fn close_period_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: Option<&str>,
    ) -> Result<Period> {
        join(tx, scope, company_id, MANAGE_PERIODS).await?;
        let state = periods::state(tx.sql(), scope.tenant(), company_id).await?;
        let period = state.period(period_id).ok_or(LedgerError::PeriodNotFound)?;
        let (previous, _) = state.neighbours(period_id);
        if period.status != PeriodStatus::Open {
            return Err(LedgerError::PeriodNotOpen);
        }
        if previous.is_some_and(|previous| previous.status == PeriodStatus::Open) {
            return Err(LedgerError::EarlierPeriodOpen);
        }
        if state.year(period.fiscal_year_id).is_some_and(|year| {
            year.end_date == period.end_date && year.status != YearStatus::Closed
        }) {
            return Err(LedgerError::ClosesWithFiscalYear);
        }
        let reason = periods::reason(reason, None).map_err(LedgerError::Invalid)?;
        let period = period.clone();
        Self::change_period(
            scope,
            company_id,
            tx,
            period,
            ClosingAction::Close,
            reason,
            None,
        )
        .await
    }

    /// Reopens a closed period with a `reason` (`finance.ledger.periods.reopen`),
    /// so entries post into it again. Periods reopen in reverse date order
    /// (`LaterPeriodClosed`); a locked period never reopens (`PeriodLocked`),
    /// and a closed fiscal year's periods reopen only after
    /// [`Ledger::reopen_fiscal_year`] (`FiscalYearClosed`). The reopen
    /// overrides a close, so it is also recorded as an approval exception.
    pub async fn reopen_period(
        &self,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: &str,
    ) -> Result<Period> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[REOPEN_PERIODS],
                "finance.ledger.reopen_period",
            )
            .await?;
        let period = Self::reopen_period_on(&mut tx, scope, company_id, period_id, reason).await?;
        tx.commit().await?;
        Ok(period)
    }

    async fn reopen_period_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: &str,
    ) -> Result<Period> {
        join(tx, scope, company_id, REOPEN_PERIODS).await?;
        let state = periods::state(tx.sql(), scope.tenant(), company_id).await?;
        let period = state.period(period_id).ok_or(LedgerError::PeriodNotFound)?;
        let (_, next) = state.neighbours(period_id);
        match period.status {
            PeriodStatus::Locked => return Err(LedgerError::PeriodLocked),
            PeriodStatus::Open => return Err(LedgerError::PeriodNotClosed),
            PeriodStatus::Closed => {}
        }
        if state
            .year(period.fiscal_year_id)
            .is_some_and(|year| year.status == YearStatus::Closed)
        {
            return Err(LedgerError::FiscalYearClosed);
        }
        if next.is_some_and(|next| next.status != PeriodStatus::Open) {
            return Err(LedgerError::LaterPeriodClosed);
        }
        let reason = periods::reason(Some(reason), Some("is required to reopen"))
            .map_err(LedgerError::Invalid)?;
        let period = period.clone();
        let exception = duties_reopen(
            "period",
            "Period",
            period.id,
            period.start_date,
            period.end_date,
        );
        Self::change_period(
            scope,
            company_id,
            tx,
            period,
            ClosingAction::Reopen,
            reason,
            Some(exception),
        )
        .await
    }

    /// Locks a closed period for good with an optional `reason`
    /// (`finance.ledger.periods.lock`): it never reopens. Periods lock in date
    /// order (`EarlierPeriodUnlocked`).
    pub async fn lock_period(
        &self,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: Option<&str>,
    ) -> Result<Period> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[LOCK_PERIODS],
                "finance.ledger.lock_period",
            )
            .await?;
        let period = Self::lock_period_on(&mut tx, scope, company_id, period_id, reason).await?;
        tx.commit().await?;
        Ok(period)
    }

    async fn lock_period_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        period_id: i64,
        reason: Option<&str>,
    ) -> Result<Period> {
        join(tx, scope, company_id, LOCK_PERIODS).await?;
        let state = periods::state(tx.sql(), scope.tenant(), company_id).await?;
        let period = state.period(period_id).ok_or(LedgerError::PeriodNotFound)?;
        let (previous, _) = state.neighbours(period_id);
        match period.status {
            PeriodStatus::Locked => return Err(LedgerError::PeriodLocked),
            PeriodStatus::Open => return Err(LedgerError::PeriodNotClosed),
            PeriodStatus::Closed => {}
        }
        if previous.is_some_and(|previous| previous.status != PeriodStatus::Locked) {
            return Err(LedgerError::EarlierPeriodUnlocked);
        }
        let reason = periods::reason(reason, None).map_err(LedgerError::Invalid)?;
        let period = period.clone();
        Self::change_period(
            scope,
            company_id,
            tx,
            period,
            ClosingAction::Lock,
            reason,
            None,
        )
        .await
    }

    /// Closes a fiscal year (`finance.ledger.periods.manage` and
    /// `finance.ledger.entries.post`). It posts the closing entry, dated the
    /// year's last day, that brings every income and expense account's
    /// balance for the year to zero against the company's retained-earnings
    /// account (`NoRetainedEarningsAccount` until an active equity account is
    /// designated), then closes the year and every period of it still open.
    /// A year with no income or expense balance closes with no entry. The
    /// previous year must be closed first (`EarlierPeriodOpen`). Closing a
    /// closed year again returns it unchanged; only
    /// [`Ledger::reopen_fiscal_year`] undoes it.
    pub async fn close_fiscal_year(
        &self,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
        close: &YearClose,
    ) -> Result<FiscalYear> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_PERIODS, POST_ENTRIES],
                "finance.ledger.close_fiscal_year",
            )
            .await?;
        let year =
            Self::close_fiscal_year_on(&mut tx, scope, company_id, fiscal_year_id, close).await?;
        tx.commit().await?;
        Ok(year)
    }

    async fn close_fiscal_year_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
        close: &YearClose,
    ) -> Result<FiscalYear> {
        join(tx, scope, company_id, MANAGE_PERIODS).await?;
        authorize(tx.sql(), scope, company_id, POST_ENTRIES).await?;
        let tenant = scope.tenant();
        let book = books::find(tx.sql(), tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let state = periods::state(tx.sql(), tenant, company_id).await?;
        let year = state
            .year(fiscal_year_id)
            .ok_or(LedgerError::FiscalYearNotFound)?
            .clone();
        if year.status == YearStatus::Closed {
            return Ok(year);
        }
        let open: Vec<Period> = year
            .periods
            .iter()
            .filter(|period| period.status == PeriodStatus::Open)
            .cloned()
            .collect();
        let first_open = open
            .first()
            .map(|period| period.id)
            .ok_or(Error::Internal)?;
        if state
            .neighbours(first_open)
            .0
            .is_some_and(|previous| previous.status == PeriodStatus::Open)
        {
            return Err(LedgerError::EarlierPeriodOpen);
        }
        let account = accounts::list(tx.sql(), tenant, company_id)
            .await?
            .into_iter()
            .find(|account| account.retained_earnings && account.active)
            .ok_or(LedgerError::NoRetainedEarningsAccount)?;
        let balances = periods::closing_balances(tx.sql(), tenant, company_id, &year).await?;
        let closing = if balances.is_empty() {
            None
        } else {
            let profit_or_loss: rust_decimal::Decimal = balances.iter().map(|(_, net)| *net).sum();
            let mut lines: Vec<NewLine> =
                balances.iter().map(|(id, net)| side(*id, -*net)).collect();
            if !profit_or_loss.is_zero() {
                lines.push(side(account.id, profit_or_loss));
            }
            let new = NewEntry {
                entry_date: year.end_date,
                description: close.description.clone().unwrap_or_else(|| {
                    format!("Close fiscal year {} to {}", year.start_date, year.end_date)
                }),
                reference: close.reference.clone(),
                lines,
            };
            Some(
                entries::validate(
                    tx.sql(),
                    tenant,
                    company_id,
                    book.minor_units,
                    &new,
                    None,
                    entries::Inactive::Any,
                )
                .await?
                .map_err(LedgerError::Invalid)?,
            )
        };
        let (actor, impersonator) = people(scope);
        let currency = book.currency_code.clone();
        let account_id = account.id;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                let mut entry_id = None;
                if let Some(closing) = closing {
                    let posted = entries::insert(
                        sql,
                        tenant,
                        company_id,
                        &currency,
                        actor,
                        impersonator,
                        &closing,
                    )
                    .await?;
                    entry_id = Some(posted.entry.id);
                    changes.extend(entry_changes(posted));
                }
                let mut events = vec![periods::NewEvent {
                    fiscal_year_id,
                    period_id: None,
                    action: ClosingAction::Close,
                    reason: None,
                    entry_id,
                    retained_earnings_account_id: Some(account_id),
                }];
                events.extend(open.iter().map(|period| periods::NewEvent {
                    fiscal_year_id,
                    period_id: Some(period.id),
                    action: ClosingAction::Close,
                    reason: None,
                    entry_id: None,
                    retained_earnings_account_id: None,
                }));
                for event in &events {
                    changes.push(
                        periods::record_event(sql, tenant, company_id, actor, impersonator, event)
                            .await?,
                    );
                }
                Ok(((), changes))
            })
        })
        .await?;
        let year = fiscal_year(tx.sql(), tenant, company_id, fiscal_year_id).await?;
        Ok(year)
    }

    /// Reopens a closed fiscal year with a `reason` (`finance.ledger.periods.reopen`
    /// and `finance.ledger.entries.post`): it reopens the year and its last
    /// period and reverses the closing entry on the year's last day; the
    /// year's other periods stay closed until each is reopened. A year whose
    /// last period is locked never reopens (`PeriodLocked`), and a year
    /// reopens only while the next year's periods are all open
    /// (`LaterPeriodClosed`). The reopen is recorded as an approval exception.
    pub async fn reopen_fiscal_year(
        &self,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
        reason: &str,
    ) -> Result<FiscalYear> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[REOPEN_PERIODS, POST_ENTRIES],
                "finance.ledger.reopen_fiscal_year",
            )
            .await?;
        let year =
            Self::reopen_fiscal_year_on(&mut tx, scope, company_id, fiscal_year_id, reason).await?;
        tx.commit().await?;
        Ok(year)
    }

    async fn reopen_fiscal_year_on(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        fiscal_year_id: i64,
        reason: &str,
    ) -> Result<FiscalYear> {
        join(tx, scope, company_id, REOPEN_PERIODS).await?;
        authorize(tx.sql(), scope, company_id, POST_ENTRIES).await?;
        let tenant = scope.tenant();
        let book = books::find(tx.sql(), tenant, company_id)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let state = periods::state(tx.sql(), tenant, company_id).await?;
        let year = state
            .year(fiscal_year_id)
            .ok_or(LedgerError::FiscalYearNotFound)?
            .clone();
        let last = year.periods.last().cloned().ok_or(Error::Internal)?;
        let (_, next) = state.neighbours(last.id);
        if year.status == YearStatus::Open {
            return Err(LedgerError::FiscalYearNotClosed);
        }
        if last.status == PeriodStatus::Locked {
            return Err(LedgerError::PeriodLocked);
        }
        if next.is_some_and(|next| next.status != PeriodStatus::Open) {
            return Err(LedgerError::LaterPeriodClosed);
        }
        let reason = periods::reason(Some(reason), Some("is required to reopen"))
            .map_err(LedgerError::Invalid)?
            .unwrap_or_default();
        // The closing entry's reversal is decided before anything is written
        // (Bilimbi's one `write` rolls a refused reversal back with the
        // events), as the year's last period will stand once reopened.
        let reversal = match year.closing_entry_id {
            Some(entry_id) => {
                let original = entries::find(tx.sql(), tenant, company_id, entry_id)
                    .await?
                    .ok_or(Error::Internal)?;
                let new = entries::mirror(
                    &original,
                    year.end_date,
                    format!(
                        "Reopen fiscal year {} to {}",
                        year.start_date, year.end_date
                    ),
                );
                let closed = fiscal_year(tx.sql(), tenant, company_id, fiscal_year_id).await?;
                Some(
                    entries::validate_reopening(
                        tx.sql(),
                        tenant,
                        company_id,
                        book.minor_units,
                        &new,
                        &closed,
                    )
                    .await?
                    .map_err(LedgerError::Invalid)?
                    .reversing(&original),
                )
            }
            None => None,
        };
        let (actor, impersonator) = people(scope);
        let events_reason = reason.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                for period_id in [None, Some(last.id)] {
                    changes.push(
                        periods::record_event(
                            sql,
                            tenant,
                            company_id,
                            actor,
                            impersonator,
                            &periods::NewEvent {
                                fiscal_year_id,
                                period_id,
                                action: ClosingAction::Reopen,
                                reason: Some(events_reason.clone()),
                                entry_id: None,
                                retained_earnings_account_id: None,
                            },
                        )
                        .await?,
                    );
                }
                Ok(((), changes))
            })
        })
        .await?;
        let exception = duties_reopen(
            "fiscal_year",
            "Fiscal year",
            year.id,
            year.start_date,
            year.end_date,
        );
        let currency = book.currency_code.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                if let Some(reversal) = reversal {
                    let posted = entries::insert(
                        sql,
                        tenant,
                        company_id,
                        &currency,
                        actor,
                        impersonator,
                        &reversal,
                    )
                    .await?;
                    changes.extend(entry_changes(posted));
                }
                changes.push(
                    duties::record_override(
                        sql,
                        tenant,
                        company_id,
                        actor,
                        impersonator,
                        &exception.with_reason(&reason),
                    )
                    .await?,
                );
                Ok(((), changes))
            })
        })
        .await?;
        let year = fiscal_year(tx.sql(), tenant, company_id, fiscal_year_id).await?;
        Ok(year)
    }

    /// Records the period's event (and, for a reopen, its approval
    /// exception) and returns the period with the status it now has.
    async fn change_period(
        scope: &Scope,
        company_id: i64,
        tx: &mut MutationTransaction<'_>,
        period: Period,
        action: ClosingAction,
        reason: Option<String>,
        exception: Option<Reopen>,
    ) -> Result<Period> {
        let tenant = scope.tenant();
        let (actor, impersonator) = people(scope);
        let event = periods::NewEvent {
            fiscal_year_id: period.fiscal_year_id,
            period_id: Some(period.id),
            action,
            reason,
            entry_id: None,
            retained_earnings_account_id: None,
        };
        let changed = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = vec![
                        periods::record_event(sql, tenant, company_id, actor, impersonator, &event)
                            .await?,
                    ];
                    if let (Some(exception), Some(reason)) = (exception, event.reason.as_deref()) {
                        changes.push(
                            duties::record_override(
                                sql,
                                tenant,
                                company_id,
                                actor,
                                impersonator,
                                &exception.with_reason(reason),
                            )
                            .await?,
                        );
                    }
                    Ok((
                        Period {
                            status: action.status_after(),
                            ..period
                        },
                        changes,
                    ))
                })
            })
            .await?;
        Ok(changed)
    }

    /// Opens a Finance record owner's audited write, holding the company's
    /// row lock and rechecking its capabilities inside the transaction.
    /// The owner records every changed row and owns commit/rollback; ledger
    /// posting and reversal independently check their own capabilities.
    pub async fn write<'c>(
        &self,
        client: &'c mut tokio_postgres::Client,
        scope: &Scope,
        company_id: i64,
        capabilities: &[&str],
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        for capability in capabilities {
            authorize(&*client, scope, company_id, capability).await?;
        }
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        lock(&tx, scope, company_id).await?;
        for capability in capabilities {
            authorize(tx.sql(), scope, company_id, capability).await?;
        }
        Ok(tx)
    }
}

/// A write inside a caller's own transaction: the capability and company
/// reach now, then the company's row lock on that transaction.
async fn preview_lock(
    client: &dyn bilibi_agent_api::OperationClient,
    scope: &Scope,
    company_id: i64,
) -> Result<()> {
    agent_operations::authorize_on(client, scope, company_id, POST_ENTRIES).await?;
    let row = client.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT", &[&scope.tenant(), &company_id]).await?.ok_or(LedgerError::CompanyNotFound)?;
    if row.get::<_, String>(0) == "archived" {
        return Err(LedgerError::CompanyArchived);
    }
    Ok(())
}

async fn join(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    authorize(tx.sql(), scope, company_id, capability).await?;
    lock(tx, scope, company_id).await
}

/// Holds the company's row lock for the transaction (Bilimbi
/// `lock_writable_company/2`): an archived company is refused.
async fn lock(tx: &MutationTransaction<'_>, scope: &Scope, company_id: i64) -> Result<()> {
    bilibi_company::require_writable(tx.sql(), scope, company_id, true)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            WriteError::NotFound => LedgerError::CompanyNotFound,
            WriteError::Archived => LedgerError::CompanyArchived,
            WriteError::Access(error) => refusal(error),
            WriteError::InUse | WriteError::Stale | WriteError::Invalid(_) => {
                LedgerError::Failed(Error::Internal)
            }
        })
}

/// Posts the reversal of `entry_id` inside a write that already authorized
/// and locked: `owner` is `None` for a manual entry's reversal and the
/// owner's own record for [`Ledger::reverse_owned_entry`].
async fn reverse(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    entry_id: i64,
    owner: Option<&EntryOwner>,
    reversal: &Reversal,
) -> Result<Entry> {
    let tenant = scope.tenant();
    let (book, _, valid) =
        prepare_reverse(tx.sql(), tenant, company_id, entry_id, owner, reversal).await?;
    let (actor, impersonator) = people(scope);
    let entry = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let posted = entries::insert(
                    sql,
                    tenant,
                    company_id,
                    &book.currency_code,
                    actor,
                    impersonator,
                    &valid,
                )
                .await?;
                let entry = posted.entry.clone();
                Ok((entry, entry_changes(posted)))
            })
        })
        .await?;
    Ok(entry)
}

/// Shared posting preparation for execution and owner previews; never inserts.
async fn prepare_post(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    new: &NewEntry,
    options: &PostOptions,
) -> Result<(Book, entries::ValidEntry)> {
    if options
        .owner
        .as_ref()
        .is_some_and(|owner| !entries::owner_valid(owner))
        || options.settlement_account_ids.iter().any(|id| *id < 1)
    {
        return Err(LedgerError::InvalidOption);
    }
    let book = books::find(client, tenant, company_id)
        .await?
        .ok_or(LedgerError::BookNotOpen)?;
    let valid = entries::validate(
        client,
        tenant,
        company_id,
        book.minor_units,
        new,
        options.owner.as_ref(),
        entries::Inactive::Listed(&options.settlement_account_ids),
    )
    .await?
    .map_err(LedgerError::Invalid)?;
    Ok((book, valid))
}

async fn prepare_reverse(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    entry_id: i64,
    owner: Option<&EntryOwner>,
    reversal: &Reversal,
) -> Result<(Book, Entry, entries::ValidEntry)> {
    let book = books::find(client, tenant, company_id)
        .await?
        .ok_or(LedgerError::BookNotOpen)?;
    let original = entries::find(client, tenant, company_id, entry_id)
        .await?
        .ok_or(LedgerError::EntryNotFound)?;
    if original.reverses_entry_id.is_some() {
        return Err(LedgerError::ReversalNotReversible);
    }
    if original.reversed_by_entry_id.is_some() {
        return Err(LedgerError::AlreadyReversed);
    }
    conversion::refuse_recognized_reversal(client, tenant, company_id, original.id).await?;
    if periods::closing_entry(client, company_id, original.id).await? {
        return Err(LedgerError::ClosingEntry);
    }
    match (&original.owner, owner) {
        (Some(owned), None) => return Err(LedgerError::OwnedBy(owned.clone())),
        (None, Some(_)) => return Err(LedgerError::NotOwned),
        (Some(owned), Some(owner)) if owned != owner => return Err(LedgerError::NotTheOwner),
        _ => {}
    }
    let new = NewEntry {
        reference: reversal.reference.clone(),
        ..entries::mirror(&original, reversal.entry_date, reversal.description.clone())
    };
    let validated = entries::validate(
        client,
        tenant,
        company_id,
        book.minor_units,
        &new,
        None,
        entries::Inactive::Any,
    )
    .await?;
    let valid = if reversal.entry_date < original.entry_date {
        let early = FieldError::new("entry_date", "cannot be before the reversed entry's date");
        let mut errors = validated.err().unwrap_or_default();
        errors.insert(0, early);
        return Err(LedgerError::Invalid(errors));
    } else {
        validated
            .map_err(LedgerError::Invalid)?
            .reversing(&original)
    };
    Ok((book, original, valid))
}

/// A journal of the company that is still submitted.
async fn submitted_journal(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    journal_id: i64,
) -> Result<Journal> {
    let journal = journals::find(client, tenant, company_id, journal_id)
        .await?
        .ok_or(LedgerError::JournalNotFound)?;
    if journal.status != JournalStatus::Submitted {
        return Err(LedgerError::JournalDecided);
    }
    Ok(journal)
}

/// A submitted journal the signed-in person submitted, so may take back.
async fn withdrawable_journal(
    client: &dyn bilibi_agent_api::OperationClient,
    scope: &Scope,
    company_id: i64,
    journal_id: i64,
) -> Result<Journal> {
    let journal = submitted_journal(client, scope.tenant(), company_id, journal_id).await?;
    if journal.submitted_by_user_id != scope.user() {
        return Err(LedgerError::NotSubmitter);
    }
    Ok(journal)
}

async fn separation_view(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
) -> Result<DutySeparation> {
    let change = duties::latest_change(client, tenant, company_id).await?;
    Ok(DutySeparation {
        required: duties::required(change.as_ref()),
        change,
    })
}

/// The actor's current grant and company reach decide every call.
async fn authorize(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company_id, capability)
        .await
        .map(|_| ())
        .map_err(refusal)
}

fn refusal(error: Error) -> LedgerError {
    match error {
        Error::Forbidden | Error::Unauthorized => LedgerError::Unauthorized,
        Error::NotFound => LedgerError::CompanyNotFound,
        other => LedgerError::Failed(other),
    }
}

fn account_change(id: i64, old: serde_json::Value, new: serde_json::Value) -> Change {
    Change {
        kind: "finance.ledger.account".into(),
        id,
        old,
        new,
    }
}

/// The entry and its lines as the audit records them.
fn entry_changes(posted: entries::Inserted) -> Vec<Change> {
    let mut changes = vec![Change {
        kind: "finance.ledger.entry".into(),
        id: posted.entry.id,
        old: serde_json::Value::Null,
        new: posted.entry_json,
    }];
    changes.extend(posted.lines.into_iter().map(|(id, json)| Change {
        kind: "finance.ledger.entry_line".into(),
        id,
        old: serde_json::Value::Null,
        new: json,
    }));
    changes
}

/// The scope's actor and any impersonator, as every record names them.
fn people(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

/// Whether two `(user, impersonator)` people overlap: either one's user or
/// impersonator is the other's user or impersonator.
pub(crate) fn same_person(one: (i64, Option<i64>), other: (i64, Option<i64>)) -> bool {
    let mine = [Some(one.0), one.1];
    let theirs = [Some(other.0), other.1];
    mine.iter()
        .flatten()
        .any(|person| theirs.iter().flatten().any(|other| other == person))
}

/// A positive net is a debit, a negative one a credit.
fn side(account_id: i64, net: rust_decimal::Decimal) -> NewLine {
    if net.is_sign_positive() {
        NewLine::debit(account_id, Money::stored(net))
    } else {
        NewLine::credit(account_id, Money::stored(-net))
    }
}

async fn fiscal_year(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    fiscal_year_id: i64,
) -> Result<FiscalYear> {
    periods::state(client, tenant, company_id)
        .await?
        .years
        .into_iter()
        .find(|year| year.id == fiscal_year_id)
        .ok_or(LedgerError::FiscalYearNotFound)
}

async fn controlled_view(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
) -> Result<ControlledBookkeeping> {
    let change = bookkeeping::latest(client, tenant, company_id).await?;
    Ok(ControlledBookkeeping {
        controlled: change.as_ref().is_some_and(|change| change.controlled),
        change,
        fiscal_years: periods::state(client, tenant, company_id)
            .await?
            .years
            .len(),
    })
}

/// The approval exception a reopen records: it overrides a close.
struct Reopen {
    action: &'static str,
    subject_type: &'static str,
    subject_id: i64,
    subject: String,
}
impl Reopen {
    fn with_reason<'a>(&self, reason: &'a str) -> duties::Override<'a> {
        duties::Override {
            action: self.action,
            subject_type: self.subject_type,
            subject_id: self.subject_id,
            subject: self.subject.clone(),
            reason,
        }
    }
}
fn duties_reopen(
    subject_type: &'static str,
    label: &str,
    subject_id: i64,
    start: Date,
    end: Date,
) -> Reopen {
    Reopen {
        action: if subject_type == "period" {
            "period_reopen"
        } else {
            "fiscal_year_reopen"
        },
        subject_type,
        subject_id,
        subject: format!("{label} {start} to {end}"),
    }
}

mod party_links;
pub use bilibi_tenancy::PartyIdentity;
pub use party_links::{PartyFacts, PartyLinks, PartyTarget};

mod party_picker;
pub use party_picker::{
    party_billing_lines, record_relationship as record_party_relationship,
    relation as party_relation, relationship_flash as party_relationship_flash, MultiPartyChooser,
    PartyChooser, PartyErrors, PartyPickerContext, PartySubmission, Relation as PartyRelation,
    Reviewed as PartyReview,
};
