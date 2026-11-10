//! Bank reconciliation, module `finance/reconciliation` (Bilimbi
//! `Bilimbi.Finance.Reconciliation`): a company's bank accounts, each mapped
//! to one ledger account and carrying the import mapping its bank's CSV export
//! is read through, and the bank statements imported for them; the opening
//! bridge between an approved opening entry, the source statement and its
//! outstanding items; the matches between statement lines and the book side;
//! the reconciliations of an account as of a statement end date; and the bank
//! charges and interest posted from statement lines.
//!
//! Every function takes the sealed [`Scope`] it was handed and an explicit
//! company id. Reads authorize `finance.reconciliation.view` and writes their
//! own capability for the scope's actor and the target company at the moment
//! they run, through Core Company's `authorize_work_target`: a missing grant, or a
//! sibling company without tenant-wide reach, is
//! [`ReconciliationError::Unauthorized`], and a company the scope cannot see
//! is [`ReconciliationError::CompanyNotFound`]. A write then holds the
//! company's row lock for its mutation transaction, so an archived company is
//! [`ReconciliationError::CompanyArchived`] and the company's reconciliation
//! writes run one at a time.
//!
//! The ledger is read only through `finance/ledger`'s facade with the same
//! scope, so a call that reads the chart (adding or changing a bank account)
//! or the book (importing a statement) also needs `finance.ledger.view`, and
//! the file an import keeps needs `finance.attachments.create`: each facade
//! authorizes its own calls.
//!
//! A bank account names an active asset account of the ledger that no other
//! bank account maps, and keeps it for good. Its name, counter accounts, match
//! window, import mapping and active flag change in place, and every change
//! is appended to its change log with the field, the values before and after,
//! the actor, the reason and the database's time. A bank account is never
//! deleted; deactivate it, and it imports and reconciles no more.
//!
//! A statement and its lines are inserted in one transaction, together with
//! the file itself kept whole as the statement's source document through
//! `finance/attachments` ([`STATEMENT_SOURCE`]), and are never changed or
//! deleted. Opening bridges expose their retained source statements through
//! [`OPENING_BRIDGE_SOURCE`]. The same statement file (by SHA-256) and the
//! same line (by fingerprint) are refused a second time; PostgreSQL refuses
//! them on its own as well.
//!
//! A match groups statement lines of a bank account with ledger entries on
//! its ledger account, or with one outstanding opening item, whose sides
//! agree. It is undone only by an appended `unmatched` event with a reason;
//! PostgreSQL refuses a line, entry or opening item in two standing matches.
//! A reconciliation reads its figures at pinned entry, statement, match-event
//! and opening-bridge boundaries, is finalised once its difference is zero by
//! someone other than whoever opened it (or as the ledger's approval exception
//! while the company does not separate duties), and keeps the matches it
//! counted for good.
//!
//! Results are plain records; the schema, SQL and validation stay private.
mod bank_accounts;
mod contributions;
mod csv;
mod evidence;
mod guess;
mod import;
mod mapping;
mod matching;
mod opening;
mod opening_operations;
mod posting;
mod reconciliations;
mod source;
mod statements;
mod web;

pub use contributions::{
    CAPABILITIES, FINALISE_RECONCILIATIONS, IMPORT_STATEMENTS, MANAGE_BANK_ACCOUNTS,
    MANAGE_MATCHES, MANAGE_RECONCILIATIONS, MANAGE_STATEMENT_LINES, POST_STATEMENT_LINES, VIEW,
};
pub use evidence::EvidencePage;
pub use import::{PreviewLine, StatementPreview, MAX_LINES};
pub use mapping::{
    Column, ImportMapping, AMOUNT_SIGNS, DATE_FORMATS, DECIMAL_SEPARATORS, DELIMITERS,
};
pub use posting::KINDS as POSTING_KINDS;
pub use source::{OPENING_BRIDGE_SOURCE, STATEMENT_SOURCE};

use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_finance_attachments::{AttachmentError, Attachments, NewFile};
use bilibi_finance_ledger::{Approval, Entry, Ledger, LedgerError, MinorUnits, Money, Preparer};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
use bilibi_ui::page::PageService;
use jiff::{civil::Date, Timestamp};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

/// The descriptor id.
pub const MODULE: &str = "finance/reconciliation";
/// The registry service id of the [`Reconciliation`] facade.
pub const SERVICE: &str = "finance/reconciliation.reconciliation";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &[
            "base/agent_api", "base/authz", "base/database", "base/datetime", "base/locale", "base/menu", "base/party_ids", "base/principal_directory", "base/tenancy", "base/ui", "core/company", "finance/attachments", "finance/ledger",
        ],
        migrations: &[("finance-reconciliation-001-reconciliation", include_str!("../migrations/001-reconciliation.sql")),
            ("finance-reconciliation-002-opening-bridge", include_str!("../migrations/002-opening-bridge.sql")),
            ("finance-reconciliation-003-mapping-on-first-import", include_str!("../migrations/003-mapping-on-first-import.sql"))],
        services: &[Service { id: SERVICE, factory: |services| {
            let reconciliation = Arc::new(Reconciliation {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                ledger: services.get(bilibi_finance_ledger::SERVICE)?,
                attachments: services.get(bilibi_finance_attachments::SERVICE)?,

            });
            opening_operations::bind(reconciliation.clone())?;
            Ok(reconciliation)
        } }, Service { id: web::PAGES, factory: |services| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(web::Pages {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            reconciliation: services.get(SERVICE)?,
            ledger: services.get(bilibi_finance_ledger::SERVICE)?,
            attachments: services.get(bilibi_finance_attachments::SERVICE)?,
            display: services.get("base/datetime.display")?,
            locales: services.get("base/locale.locales")?,
            contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            plans: services.get("base/agent_api.plans")?,
            parties: services.get("base/tenancy.parties")?,
            directory: services.get(bilibi_finance_ledger::PEOPLE)?,
        })))) }],
    }
}

pub type Result<T> = std::result::Result<T, ReconciliationError>;

/// Why a call did not happen (Bilimbi's refusal atoms and changeset errors).
#[derive(Debug, thiserror::Error)]
pub enum ReconciliationError {
    /// A missing grant, or a sibling company without tenant-wide reach.
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
    BankAccountNotFound,
    /// An inactive bank account imports and reconciles no more.
    #[error("The bank account is inactive")]
    BankAccountInactive,
    #[error("Not found")]
    StatementNotFound,
    /// A limit or cursor out of range.
    #[error("Invalid option")]
    InvalidOption,
    /// Refused values, in the words a form shows beside each field.
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error("The file needs a name of at most 255 characters")]
    InvalidFileName,
    /// The same file (by SHA-256) is already statement `statement_number`.
    #[error("This file is already imported as statement {statement_number}")]
    DuplicateFile { statement_number: i64 },
    /// Lines the bank account already holds, with where.
    #[error("The file holds lines that are already imported")]
    DuplicateLines(Vec<DuplicateLine>),
    /// Rows the mapping cannot read, every one with why.
    #[error("The file holds rows the import mapping cannot read")]
    InvalidLines(Vec<LineError>),
    /// Header names the mapping needs that the file does not have.
    #[error("The file lacks the mapped columns: {}", .0.join(", "))]
    MissingHeaders(Vec<String>),
    #[error("The file holds no lines")]
    NoLines,
    /// The bank account's columns are not mapped yet: its first import maps
    /// them ([`Reconciliation::import_statement_mapped`]).
    #[error("The bank account's columns are not mapped yet")]
    MappingNotSet,
    #[error("A statement holds at most {0} lines")]
    TooManyLines(usize),
    /// The balance column does not follow from the amounts in either order.
    #[error("The running balance does not follow from the amounts at row {0}")]
    RunningBalanceMismatch(usize),
    /// The reader accepts files of at most 5 MiB.
    #[error("The file is larger than 5 MB")]
    FileTooLarge,
    #[error("The file is not UTF-8 text")]
    NotUtf8,
    #[error("A quote in the middle of a field at row {0}")]
    StrayQuote(usize),
    #[error("A quote left open at row {0}")]
    UnterminatedQuote(usize),
    /// Source document storage refused the file or the link (an operator has
    /// not configured Base Artifacts' storage root, the file is over its
    /// limit, ...).
    #[error(transparent)]
    Attachment(AttachmentError),
    /// The ledger refused the read of its chart or book.
    #[error(transparent)]
    Ledger(LedgerError),
    #[error("Choose at least one statement line and one ledger entry.")]
    InvalidSelection,
    #[error("The statement line no longer exists.")]
    StatementLineNotFound,
    #[error("That statement line is already matched.")]
    StatementLineMatched,
    #[error("That ledger entry no longer exists.")]
    EntryNotFound,
    #[error("That ledger entry does not post to this bank account.")]
    EntryNotOnAccount,
    #[error("That ledger entry is already matched on this account.")]
    EntryMatched,
    /// The two sides of a match differ.
    #[error(
        "The sides do not agree: the statement lines sum to {statement_side} and the ledger entries to {ledger_side}."
    )]
    Unbalanced {
        statement_side: Money,
        ledger_side: Money,
    },
    #[error("The match no longer exists.")]
    MatchNotFound,
    #[error("That match is already unmatched.")]
    MatchNotStanding,
    /// A finalised reconciliation counted the match, which is kept for good.
    #[error("A finalised reconciliation counted that match, so it is kept.")]
    MatchFinalised,
    #[error("The reconciliation no longer exists.")]
    ReconciliationNotFound,
    #[error("The reconciliation is finalised and cannot change.")]
    ReconciliationFinalised,
    #[error("The reconciliation was abandoned.")]
    ReconciliationAbandoned,
    /// The finaliser opened the reconciliation, impersonated whoever did, or
    /// was impersonated by them, while the company separates duties.
    #[error(
        "Someone else must finalise this because the company requires separation of duties. For a one-person company, change Separation of duties on Approval exceptions: /finance/reports/approval-exceptions"
    )]
    SamePerson,
    /// A reconciliation is finalised only once its difference is zero.
    #[error(
        "The difference is {0}, not zero; match or post what is unmatched, or check the statement balance."
    )]
    Difference(Money),
    #[error("Choose whether the line is a charge or interest.")]
    InvalidKind,
    #[error("Set the bank account's charges account first.")]
    NoChargesAccount,
    #[error("Set the bank account's interest account first.")]
    NoInterestAccount,
    #[error("Money in cannot be posted as a bank charge.")]
    NotACharge,
    #[error("Money out cannot be posted as interest.")]
    NotInterest,
    /// The line already posted an entry, which the ledger holds as the
    /// line's: match the line to it instead.
    #[error("That statement line already posted an entry; match the line to it instead.")]
    StatementLinePosted,
    /// A reversal's reason is blank or over 500 characters.
    #[error("Give a reason of at most 500 characters.")]
    InvalidReason,
    #[error("That statement line is not matched.")]
    StatementLineNotMatched,
    /// The line's match is not the one that posted it.
    #[error("That statement line did not post the entry it is matched to.")]
    StatementLineNotPosted,
    /// A database or capture failure; never a refusal.
    #[error(transparent)]
    Failed(#[from] Error),
}

impl From<LedgerError> for ReconciliationError {
    fn from(error: LedgerError) -> Self {
        match error {
            LedgerError::Unauthorized => Self::Unauthorized,
            LedgerError::CompanyNotFound => Self::CompanyNotFound,
            LedgerError::CompanyArchived => Self::CompanyArchived,
            LedgerError::BookNotOpen => Self::BookNotOpen,
            LedgerError::SamePerson => Self::SamePerson,
            LedgerError::Failed(error) => Self::Failed(error),
            other => Self::Ledger(other),
        }
    }
}
impl From<AttachmentError> for ReconciliationError {
    fn from(error: AttachmentError) -> Self {
        match error {
            AttachmentError::Unauthorized => Self::Unauthorized,
            AttachmentError::CompanyNotFound => Self::CompanyNotFound,
            AttachmentError::CompanyArchived => Self::CompanyArchived,
            AttachmentError::Failed(error) => Self::Failed(error),
            other => Self::Attachment(other),
        }
    }
}

/// One refusal of a submitted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
impl FieldError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

/// A row of the file the import mapping cannot read: its 1-based number among
/// the file's non-blank rows, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LineError {
    pub row: usize,
    pub message: String,
}

/// A line of the file the bank account already holds: the file's row, the
/// line's number in this import, and the statement and line that hold it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DuplicateLine {
    pub row: usize,
    pub line_number: i32,
    pub statement_number: i64,
    pub statement_line_number: i32,
}

/// One bank account, as the facade shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BankAccount {
    pub id: i64,
    pub name: String,
    /// The active asset account of the ledger it is mapped to, for good.
    pub ledger_account_id: i64,
    /// The expense account bank charges post to.
    pub charges_account_id: Option<i64>,
    /// The income account interest posts to.
    pub interest_account_id: Option<i64>,
    /// How many days a suggested match's dates may differ by, 0 to 90.
    pub match_window_days: i32,
    /// How its bank's export is read; `None` until the first import maps the
    /// columns ([`Reconciliation::import_statement_mapped`]).
    pub import_mapping: Option<ImportMapping>,
    pub active: bool,
}

/// What adds a bank account: a `name` of at most 200 characters, the
/// `ledger_account_id` (an active asset account of the company no other bank
/// account maps), the `import_mapping` (see [`ImportMapping::parse`]; `null`
/// leaves the columns to be mapped on the first import), and
/// optionally a `charges_account_id` (an expense account), an
/// `interest_account_id` (an income account), `match_window_days` (0 to 90,
/// 5 when absent), `active` (true when absent) and a `reason` for the change
/// log.
#[derive(Clone, Debug)]
pub struct NewBankAccount {
    pub name: String,
    pub ledger_account_id: i64,
    pub import_mapping: Value,
    pub charges_account_id: Option<i64>,
    pub interest_account_id: Option<i64>,
    pub match_window_days: Option<i32>,
    pub active: Option<bool>,
    pub reason: Option<String>,
}

/// What changes on a bank account: each `Some` replaces the field;
/// `charges_account_id: Some(None)` removes the counter account. The ledger
/// account never changes.
#[derive(Clone, Debug, Default)]
pub struct BankAccountEdit {
    pub name: Option<String>,
    pub charges_account_id: Option<Option<i64>>,
    pub interest_account_id: Option<Option<i64>>,
    pub match_window_days: Option<i32>,
    pub import_mapping: Option<Value>,
    pub active: Option<bool>,
    pub reason: Option<String>,
}

/// One field of a bank account having been set or changed: the values before
/// (none for a new account) and after as text, who, why, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BankAccountChange {
    pub id: i64,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub reason: Option<String>,
    pub changed_by_user_id: i64,
    pub changed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub changed_at: Timestamp,
}

/// An imported statement: the file's name and SHA-256, the mapping it was
/// read through, its dates and balances (when the file has a running balance
/// column), and who imported it when. [`Reconciliation::get_statement`] also
/// returns its lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Statement {
    pub id: i64,
    pub bank_account_id: i64,
    pub statement_number: i64,
    pub file_name: String,
    pub file_sha256: String,
    pub import_mapping: ImportMapping,
    pub line_count: i32,
    #[serde(serialize_with = "as_text")]
    pub start_date: Date,
    #[serde(serialize_with = "as_text")]
    pub end_date: Date,
    pub opening_balance: Option<Money>,
    pub closing_balance: Option<Money>,
    pub imported_by_user_id: i64,
    pub imported_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub imported_at: Timestamp,
    /// In line order; empty in a list.
    pub lines: Vec<StatementLine>,
}

/// One line of a statement. `amount` is signed from the account holder's
/// side: money in positive and money out negative, which is the sign of a
/// debit less a credit on the asset account it is matched against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatementLine {
    pub id: i64,
    pub statement_id: i64,
    pub bank_account_id: i64,
    pub line_number: i32,
    #[serde(serialize_with = "as_text")]
    pub line_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub amount: Money,
    pub running_balance: Option<Money>,
}

/// What [`Reconciliation::import_statement`] recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportedStatement {
    pub statement: Statement,
    /// Rows whose amount was zero, which are not lines.
    pub skipped_count: usize,
}

/// `limit` (1 to 500, 100 when absent) and `after`, the last id already read.
#[derive(Clone, Debug, Default)]
pub struct PageOptions {
    pub limit: Option<i64>,
    pub after: Option<i64>,
}
impl PageOptions {
    /// The page this asks for, or `InvalidOption`.
    pub(crate) fn resolve(&self) -> Result<(i64, i64)> {
        let limit = self.limit.unwrap_or(100);
        let after = self.after.unwrap_or(0);
        if (1..=500).contains(&limit) && after >= 0 && (self.after.is_none() || after >= 1) {
            Ok((limit, after))
        } else {
            Err(ReconciliationError::InvalidOption)
        }
    }
}

/// A statement line with its statement's number, as the matching and
/// reconciliation reads list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatementLineItem {
    #[serde(flatten)]
    pub line: StatementLine,
    pub statement_number: i64,
}

/// One entry's side of a bank account's ledger account: its debits less its
/// credits there (the sign of a statement line's amount), with its number,
/// date, description and reference. A match keeps these as they were when
/// it was made, which never changes, because a posted entry never does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LedgerItem {
    pub entry_id: i64,
    pub entry_number: i64,
    #[serde(serialize_with = "as_text")]
    pub entry_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub amount: Money,
}

/// One outstanding source item already included in the opening ledger entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OpeningItem {
    pub id: i64,
    pub bridge_id: i64,
    pub source_id: String,
    #[serde(serialize_with = "as_text")]
    pub item_date: Date,
    #[serde(serialize_with = "as_text")]
    pub conversion_date: Date,
    pub description: String,
    pub amount: Money,
}

/// What stands unmatched on a bank account, from
/// [`Reconciliation::unmatched_items`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unmatched {
    /// Statement lines with no standing match, by date, statement and line.
    pub lines: Vec<StatementLineItem>,
    /// Entries on the ledger account with no standing match, by date and id.
    pub entries: Vec<LedgerItem>,
    /// Source items included in the opening balance but not yet cleared.
    pub opening_items: Vec<OpeningItem>,
    /// A statement line's id to the entry suggested for it: the same amount
    /// within the account's match window, by reference and then the nearest
    /// date, each entry suggested once.
    pub suggestions: BTreeMap<i64, LedgerItem>,
}

/// What [`Reconciliation::record_match`] matches: statement lines of the
/// bank account and entries on its ledger account, one or many on each side,
/// with an optional `note` (at most 500 characters).
#[derive(Clone, Debug, Default)]
pub struct NewMatch {
    pub statement_line_ids: Vec<i64>,
    pub entry_ids: Vec<i64>,
    pub note: Option<String>,
}

/// Whether a match stands: `unmatched` once its `unmatched` event is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchStatus {
    Matched,
    Unmatched,
}

/// A match's `matched` event, recorded with it, or its `unmatched` event,
/// with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MatchEvent {
    pub event_number: i64,
    pub action: String,
    pub reason: Option<String>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub recorded_at: Timestamp,
}

/// A match: its statement lines, its entries as they were matched (by entry
/// number), any opening items, and its events. The database numbers it per
/// company.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Match {
    pub id: i64,
    pub bank_account_id: i64,
    pub match_number: i64,
    pub note: Option<String>,
    pub matched_by_user_id: i64,
    pub matched_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub matched_at: Timestamp,
    pub status: MatchStatus,
    pub statement_line_ids: Vec<i64>,
    pub entries: Vec<LedgerItem>,
    pub opening_items: Vec<OpeningItem>,
    pub events: Vec<MatchEvent>,
}

/// The boundaries a reconciliation's figures are read at, each the latest when
/// `None` (0 or more): how a finalised reconciliation's figures reproduce.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pins {
    pub through_entry_number: Option<i64>,
    pub through_statement_number: Option<i64>,
    pub through_match_event_number: Option<i64>,
    pub through_opening_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityBoundary {
    pub through_entry_number: i64,
    pub through_statement_number: i64,
    pub through_match_event_number: i64,
}

/// What opens (or previews) a reconciliation: the statement's `end_date` and
/// the `statement_balance` it shows that day, in decimal notation no finer
/// than the book's currency.
#[derive(Clone, Debug, Default)]
pub struct NewReconciliation {
    pub end_date: Option<Date>,
    pub statement_balance: String,
}

/// What explains a reconciliation's difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// On the statement, not in the books.
    StatementLine,
    /// In the books, not on the statement.
    Entry,
    /// Matched, but cleared after the end date on one side.
    Timing,
    /// An outstanding source item already included in the opening entry.
    OpeningItem,
}
impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StatementLine => "statement_line",
            Self::Entry => "entry",
            Self::Timing => "timing",
            Self::OpeningItem => "opening_item",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::StatementLine,
            Self::Entry,
            Self::Timing,
            Self::OpeningItem,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
    }
}

/// One item of a reconciliation: the statement line, entry, opening item or
/// match (`timing`) it is about, dated, described and signed as the difference
/// counts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationItem {
    pub kind: ItemKind,
    pub statement_line_id: Option<i64>,
    pub entry_id: Option<i64>,
    pub match_id: Option<i64>,
    pub opening_item_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub item_date: Date,
    pub description: String,
    pub reference: Option<String>,
    pub amount: Money,
}

/// A reconciliation's figures: the book balance through the end date, the
/// difference (zero once explained), the book's currency, the entry,
/// statement, match-event and opening-bridge boundaries they were read at,
/// and the items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Figures {
    pub book_balance: Money,
    pub difference: Money,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub through_entry_number: i64,
    pub through_statement_number: i64,
    pub through_match_event_number: i64,
    pub through_opening_id: i64,
    pub items: Vec<ReconciliationItem>,
}

/// The figures a reconciliation of a bank account would show, from
/// [`Reconciliation::preview_reconciliation`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationPreview {
    pub bank_account_id: i64,
    #[serde(serialize_with = "as_text")]
    pub end_date: Date,
    pub statement_balance: Money,
    #[serde(flatten)]
    pub figures: Figures,
}

/// A reconciliation's status, its closing event's (open with none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReconciliationStatus {
    Open,
    Finalised,
    Abandoned,
}
impl ReconciliationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Finalised => "finalised",
            Self::Abandoned => "abandoned",
        }
    }
}

/// One event of a reconciliation: `opened`; `finalised`, with the figures
/// and the numbers they were read at; or `abandoned`, with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationEvent {
    pub event_number: i64,
    pub action: String,
    pub reason: Option<String>,
    pub book_balance: Option<Money>,
    pub difference: Option<Money>,
    pub through_entry_number: Option<i64>,
    pub through_statement_number: Option<i64>,
    pub through_match_event_number: Option<i64>,
    pub through_opening_id: Option<i64>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub recorded_at: Timestamp,
}

/// A reconciliation of a bank account as of a statement end date, with its
/// status and events. `figures` is set by
/// [`Reconciliation::get_reconciliation`] (read now while open, the stored
/// snapshot once finalised, none once abandoned) and by finalising; a list
/// leaves it out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BankReconciliation {
    pub id: i64,
    pub bank_account_id: i64,
    pub reconciliation_number: i64,
    #[serde(serialize_with = "as_text")]
    pub end_date: Date,
    pub statement_balance: Money,
    pub opened_by_user_id: i64,
    pub opened_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub opened_at: Timestamp,
    pub status: ReconciliationStatus,
    pub events: Vec<ReconciliationEvent>,
    pub figures: Option<Figures>,
}

/// What posts a statement line: `kind`, one of [`POSTING_KINDS`]
/// (`charge`, money out against the bank account's charges account, or
/// `interest`, money in against its interest account), and an optional
/// entry `description` (the line's when blank).
#[derive(Clone, Debug, Default)]
pub struct PostLine {
    pub kind: String,
    pub description: Option<String>,
}

/// What [`Reconciliation::post_statement_line`] recorded: the entry and the
/// match of the line to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PostedStatementLine {
    pub entry: Entry,
    #[serde(rename = "match")]
    pub posted_match: Match,
}

/// What reverses a statement line's posting: the needed `reason` (at most
/// 500 characters) and the reversal's `entry_date` (today, UTC, when none).
#[derive(Clone, Debug, Default)]
pub struct ReverseLine {
    pub reason: Option<String>,
    pub entry_date: Option<Date>,
}

/// What [`Reconciliation::reverse_statement_line`] recorded: the reversal
/// and the match, now unmatched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReversedStatementLine {
    pub reversal: Entry,
    #[serde(rename = "match")]
    pub unmatched_match: Match,
}

/// The choices an import mapping is built from.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MappingChoices {
    pub delimiters: &'static [&'static str],
    pub date_formats: &'static [&'static str],
    pub amount_signs: &'static [&'static str],
    pub decimal_separators: &'static [&'static str],
}

/// The facade, resolved from the registry as [`SERVICE`].
pub struct Reconciliation {
    db: Db,
    ledger: Arc<Ledger>,
    attachments: Arc<Attachments>,
}

impl Reconciliation {
    /// The choices an import mapping is built from (see [`ImportMapping`]).
    pub fn mapping_choices() -> MappingChoices {
        MappingChoices {
            delimiters: &DELIMITERS,
            date_formats: &DATE_FORMATS,
            amount_signs: &AMOUNT_SIGNS,
            decimal_separators: &DECIMAL_SEPARATORS,
        }
    }

    /// A first guess at how a bank's CSV export is read, from the file
    /// alone (separator, rows above the data, which column is which, date
    /// format, decimal mark), as [`Reconciliation::preview_statement`] makes
    /// for an account with no mapping yet. It only proposes; nothing is read
    /// from the database.
    pub fn guess_mapping(content: &[u8]) -> Value {
        guess::mapping(content)
    }

    /// Ledger account identifiers used by bank reconciliation, for cash reports.
    /// Only ledger view is required; no bank names, mappings or statements escape.
    pub async fn cash_account_ids(&self, scope: &Scope, company_id: i64) -> Result<Vec<i64>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, bilibi_finance_ledger::VIEW).await?;
        Ok(bank_accounts::list(&**client, scope.tenant(), company_id)
            .await?
            .into_iter()
            .map(|account| account.ledger_account_id)
            .collect())
    }

    /// The company's bank accounts by name, active or not
    /// (`finance.reconciliation.view`).
    pub async fn list_bank_accounts(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<BankAccount>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(bank_accounts::list(&**client, scope.tenant(), company_id).await?)
    }

    /// One bank account (`finance.reconciliation.view`).
    pub async fn get_bank_account(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
    ) -> Result<BankAccount> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)
    }

    /// Adds a bank account (`finance.reconciliation.bank-accounts.manage`).
    /// Reads the chart through the ledger, so it also needs
    /// `finance.ledger.view`. Every field set is logged with no old value.
    pub async fn create_bank_account(
        &self,
        scope: &Scope,
        company_id: i64,
        new: &NewBankAccount,
    ) -> Result<BankAccount> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_BANK_ACCOUNTS],
                "finance.reconciliation.create_bank_account",
            )
            .await?;
        let chart = self
            .ledger
            .list_accounts_on(tx.sql(), scope, company_id)
            .await?
            .into_iter()
            .map(|account| (account.id, account))
            .collect();
        let valid = bank_accounts::validate_new(tx.sql(), scope.tenant(), company_id, &chart, new)
            .await?
            .map_err(ReconciliationError::Invalid)?;
        let tenant = scope.tenant();
        let (actor, impersonator) = people(scope);
        let account = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    bank_accounts::insert(sql, tenant, company_id, actor, impersonator, &valid)
                        .await
                })
            })
            .await?;
        tx.commit().await?;
        Ok(account)
    }

    /// Changes a bank account's name, counter accounts, match window, import
    /// mapping or active flag, with an optional reason
    /// (`finance.reconciliation.bank-accounts.manage`); its ledger account
    /// never changes. Every field that changes is logged
    /// ([`Reconciliation::list_bank_account_changes`]); a change that changes
    /// nothing records nothing.
    pub async fn update_bank_account(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        change: &BankAccountEdit,
    ) -> Result<BankAccount> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_BANK_ACCOUNTS],
                "finance.reconciliation.update_bank_account",
            )
            .await?;
        let tenant = scope.tenant();
        let current = bank_accounts::find(tx.sql(), tenant, company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let chart = self
            .ledger
            .list_accounts_on(tx.sql(), scope, company_id)
            .await?
            .into_iter()
            .map(|account| (account.id, account))
            .collect();
        let Some(next) =
            bank_accounts::validate_change(tx.sql(), tenant, company_id, &chart, &current, change)
                .await?
                .map_err(ReconciliationError::Invalid)?
        else {
            return Ok(current);
        };
        let (actor, impersonator) = people(scope);
        let account = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    bank_accounts::update(
                        sql,
                        tenant,
                        company_id,
                        actor,
                        impersonator,
                        &current,
                        &next,
                    )
                    .await
                })
            })
            .await?;
        tx.commit().await?;
        Ok(account)
    }

    /// The bank account's change log, oldest first
    /// (`finance.reconciliation.view`).
    pub async fn list_bank_account_changes(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        page: &PageOptions,
    ) -> Result<Vec<BankAccountChange>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let (limit, after) = page.resolve()?;
        Ok(bank_accounts::log(
            &**client,
            scope.tenant(),
            company_id,
            bank_account_id,
            limit,
            after,
        )
        .await?)
    }

    /// Imports a bank statement (`finance.reconciliation.statements.import`):
    /// `file`, the CSV file's name and bytes (and its content type,
    /// `text/csv` unless given), is read through the bank account's import
    /// mapping into a numbered, immutable statement with its lines, which
    /// keeps that mapping as its `import_mapping`. The file itself is kept
    /// whole as a source document of the statement through
    /// `finance/attachments` ([`STATEMENT_SOURCE`]), retained as the company's
    /// retention says from the statement's end date, so the lines reproduce
    /// from it. The import also needs `finance.reconciliation.view` (the
    /// statement is read back when the file is linked to it),
    /// `finance.attachments.create` for the file and `finance.ledger.view` for
    /// the book's currency precision, all checked before anything is stored.
    ///
    /// The file is stored only once the statement passes every check below,
    /// before the transaction that records both (Base Artifacts stores a file
    /// outside any transaction), and a storage refusal imports nothing.
    /// Refused whole with `DuplicateFile` for a file already imported,
    /// `DuplicateLines` naming each line already imported (by this file's row
    /// and the statement and line that hold it), `InvalidLines` for rows the
    /// mapping cannot read, `MissingHeaders`, `NoLines`, `TooManyLines`,
    /// `RunningBalanceMismatch` when the balance column does not follow from
    /// the amounts in either direction, and the reader's `FileTooLarge`,
    /// `NotUtf8`, `StrayQuote` and `UnterminatedQuote`, and `MappingNotSet`
    /// while the account's columns are not mapped yet. A row whose amount is
    /// zero is skipped and counted in `skipped_count`.
    pub async fn import_statement(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        file: &NewFile,
    ) -> Result<ImportedStatement> {
        self.import(scope, company_id, bank_account_id, file, None)
            .await
    }

    /// Imports a statement as [`Reconciliation::import_statement`] does, read
    /// through `mapping` (see [`ImportMapping::parse`]) rather than the bank
    /// account's, and keeps that mapping with the account in the same
    /// transaction, so the next import reads the same way: the first import
    /// of an account added without one maps its columns. Setting the first
    /// mapping needs only `finance.reconciliation.statements.import`;
    /// replacing a different one is a change to the bank account and also
    /// needs `finance.reconciliation.bank-accounts.manage`. The account's
    /// change log records it with the file's name as the reason. A refused
    /// import keeps the account's mapping as it was.
    pub async fn import_statement_mapped(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        file: &NewFile,
        mapping: &Value,
    ) -> Result<ImportedStatement> {
        let mapping = ImportMapping::parse(mapping).map_err(|message| {
            ReconciliationError::Invalid(vec![FieldError::new("import_mapping", message)])
        })?;
        self.import(scope, company_id, bank_account_id, file, Some(mapping))
            .await
    }

    /// How a statement file would import, before anything is stored
    /// (`finance.reconciliation.statements.import` and
    /// `finance.reconciliation.view`, and `finance.ledger.view` for the
    /// book's precision): its columns and first rows, and the first lines the
    /// import would read through `mapping`, the bank account's own mapping
    /// when `mapping` is `None`, or, for an account whose columns are not
    /// mapped yet, a mapping guessed from the file's header row and values
    /// (`guessed`). Why that mapping does not read the file is in `refusal`;
    /// a file already imported is refused only by the import itself.
    pub async fn preview_statement(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        content: &[u8],
        mapping: Option<&Value>,
    ) -> Result<StatementPreview> {
        {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, IMPORT_STATEMENTS).await?;
            authorize(&**client, scope, company_id, VIEW).await?;
        }
        let account = self
            .active_bank_account(scope, company_id, bank_account_id)
            .await?;
        let book = self.ledger.get_book(scope, company_id).await?;
        let (mapping, guessed) = match (mapping, &account.import_mapping) {
            (Some(mapping), _) => (mapping.clone(), false),
            (None, Some(mapping)) => (mapping.to_json(), false),
            (None, None) => (guess::mapping(content), true),
        };
        Ok(import::preview(content, mapping, guessed, book.minor_units))
    }

    /// [`Reconciliation::import_statement`], through `mapping` when given
    /// (kept with the account) or the account's own.
    async fn import(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        file: &NewFile,
        mapping: Option<ImportMapping>,
    ) -> Result<ImportedStatement> {
        let tenant = scope.tenant();
        {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, IMPORT_STATEMENTS).await?;
            authorize(&**client, scope, company_id, VIEW).await?;
        }
        let account = self
            .active_bank_account(scope, company_id, bank_account_id)
            .await?;
        let replaces = |current: &Option<ImportMapping>, given: &ImportMapping| {
            current.as_ref().is_some_and(|current| current != given)
        };
        let keep = mapping.is_some();
        let mapping = match mapping.or_else(|| account.import_mapping.clone()) {
            Some(mapping) => mapping,
            None => return Err(ReconciliationError::MappingNotSet),
        };
        let mut capabilities = vec![IMPORT_STATEMENTS];
        if keep && replaces(&account.import_mapping, &mapping) {
            capabilities.push(MANAGE_BANK_ACCOUNTS);
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, MANAGE_BANK_ACCOUNTS).await?;
        }
        let book = self.ledger.get_book(scope, company_id).await?;
        let prepared = {
            let client = self.db.get().await.map_err(Error::from)?;
            import::prepare(
                &**client,
                tenant,
                company_id,
                account.id,
                &mapping,
                book.minor_units,
                file,
            )
            .await?
        };
        let capture = self
            .attachments
            .capture_file(
                scope,
                company_id,
                STATEMENT_SOURCE,
                &prepared.file,
                prepared.end_date,
            )
            .await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &capabilities,
                "finance.reconciliation.import_statement",
            )
            .await?;
        // The bank account may have been deactivated, and another import may
        // have landed, since the checks above: ask again under the lock.
        let locked = bank_accounts::find(tx.sql(), tenant, company_id, account.id)
            .await?
            .filter(|account| account.active)
            .ok_or(ReconciliationError::BankAccountInactive)?;
        // Someone may have mapped the columns meanwhile: replacing theirs is
        // a bank account change.
        if keep
            && replaces(&locked.import_mapping, &mapping)
            && !capabilities.contains(&MANAGE_BANK_ACCOUNTS)
        {
            authorize(tx.sql(), scope, company_id, MANAGE_BANK_ACCOUNTS).await?;
        }
        import::check_fresh(tx.sql(), tenant, company_id, account.id, &prepared).await?;
        let (actor, impersonator) = people(scope);
        let statement_id = import::insert(
            &mut tx,
            tenant,
            company_id,
            actor,
            impersonator,
            &locked,
            &prepared,
        )
        .await?;
        if keep {
            let reason = format!("Mapped when importing {}", prepared.file.file_name);
            if let Some(next) = bank_accounts::remap(&locked, &mapping, &reason) {
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        bank_accounts::update(
                            sql,
                            tenant,
                            company_id,
                            actor,
                            impersonator,
                            &locked,
                            &next,
                        )
                        .await
                    })
                })
                .await?;
            }
        }
        self.attachments
            .attach_capture(
                &mut tx,
                scope,
                company_id,
                STATEMENT_SOURCE,
                statement_id,
                &capture,
            )
            .await?;
        let statement = statements::find(tx.sql(), tenant, company_id, statement_id, true)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(ImportedStatement {
            statement,
            skipped_count: prepared.skipped,
        })
    }

    /// The bank account's statements, oldest first, without their lines
    /// (`finance.reconciliation.view`).
    pub async fn list_statements(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        page: &PageOptions,
    ) -> Result<Vec<Statement>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let (limit, after) = page.resolve()?;
        Ok(statements::list(
            &**client,
            scope.tenant(),
            company_id,
            bank_account_id,
            limit,
            after,
        )
        .await?)
    }

    /// One statement with its lines in line order
    /// (`finance.reconciliation.view`).
    pub async fn get_statement(
        &self,
        scope: &Scope,
        company_id: i64,
        statement_id: i64,
    ) -> Result<Statement> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        statements::find(&**client, scope.tenant(), company_id, statement_id, true)
            .await?
            .ok_or(ReconciliationError::StatementNotFound)
    }

    // Matching

    /// What stands unmatched on the bank account (`finance.reconciliation.view`):
    /// its statement lines with no standing match, the entries on its ledger
    /// account with none, and the suggested matches between them (see
    /// [`Unmatched`]). `to` keeps both sides through a date. Reads the ledger,
    /// so it also needs `finance.ledger.view`.
    pub async fn unmatched_items(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        to: Option<Date>,
    ) -> Result<Unmatched> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let account = bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let sides = matching::unmatched(
            &self.ledger,
            &**client,
            scope,
            company_id,
            &account,
            matching::Bounds {
                to,
                ..Default::default()
            },
        )
        .await?;
        let suggestions =
            matching::suggestions(&sides.lines, &sides.entries, account.match_window_days);
        Ok(Unmatched {
            lines: sides.lines,
            entries: sides.entries,
            opening_items: sides.opening_items,
            suggestions,
        })
    }

    /// Matches statement lines of the bank account with entries on its
    /// ledger account, one or many on each side, with an optional note
    /// (`finance.reconciliation.matches.manage`). Both sides must be
    /// unmatched and sum to the same amount (`Unbalanced`); other refusals
    /// are `InvalidSelection`, `StatementLineNotFound`,
    /// `StatementLineMatched`, `EntryNotFound`, `EntryNotOnAccount` and
    /// `EntryMatched`, and PostgreSQL refuses a line or an entry already in a
    /// standing match on its own. Reads the entries through the ledger, so it
    /// also needs `finance.ledger.view`.
    pub async fn record_match(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        new: &NewMatch,
    ) -> Result<Match> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_MATCHES],
                "finance.reconciliation.match",
            )
            .await?;
        let tenant = scope.tenant();
        let account = bank_accounts::find(tx.sql(), tenant, company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let note = matching::note(new.note.as_deref())
            .map_err(|error| ReconciliationError::Invalid(vec![error]))?;
        let (lines, items) = matching::prepare(
            &self.ledger,
            tx.sql(),
            scope,
            company_id,
            &account,
            &new.statement_line_ids,
            &new.entry_ids,
        )
        .await?;
        let match_id = matching::insert(
            &mut tx,
            tenant,
            company_id,
            people(scope),
            account.id,
            &lines,
            &items,
            note,
        )
        .await?;
        let recorded = matching::find(tx.sql(), tenant, company_id, match_id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(recorded)
    }

    /// Unmatches a standing match with a needed `reason`, as an appended
    /// event: its lines and entries are unmatched again
    /// (`finance.reconciliation.matches.manage`). A match that is not
    /// standing is `MatchNotStanding`, and one a finalised reconciliation
    /// counted is kept (`MatchFinalised`).
    pub async fn unmatch(
        &self,
        scope: &Scope,
        company_id: i64,
        match_id: i64,
        reason: Option<&str>,
    ) -> Result<Match> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_MATCHES],
                "finance.reconciliation.unmatch",
            )
            .await?;
        let tenant = scope.tenant();
        let found = matching::find(tx.sql(), tenant, company_id, match_id)
            .await?
            .ok_or(ReconciliationError::MatchNotFound)?;
        matching::check_unmatch(tx.sql(), tenant, company_id, &found).await?;
        let reason = matching::unmatch_reason(reason)
            .map_err(|error| ReconciliationError::Invalid(vec![error]))?;
        matching::record_unmatch(&mut tx, tenant, company_id, people(scope), found.id, reason)
            .await?;
        let unmatched = matching::find(tx.sql(), tenant, company_id, found.id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(unmatched)
    }

    /// One match with its status, statement lines, entries and events
    /// (`finance.reconciliation.view`).
    pub async fn get_match(&self, scope: &Scope, company_id: i64, match_id: i64) -> Result<Match> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        matching::find(&**client, scope.tenant(), company_id, match_id)
            .await?
            .ok_or(ReconciliationError::MatchNotFound)
    }

    /// The bank account's matches, standing or not, oldest first
    /// (`finance.reconciliation.view`).
    pub async fn list_matches(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        page: &PageOptions,
    ) -> Result<Vec<Match>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let (limit, after) = page.resolve()?;
        matching::list(
            &**client,
            scope.tenant(),
            company_id,
            bank_account_id,
            limit,
            after,
        )
        .await
    }

    /// The bank account's statement lines by date, statement and line, each
    /// with its statement number; `to` bounds the date
    /// (`finance.reconciliation.view`).
    pub async fn list_statement_lines(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        to: Option<Date>,
    ) -> Result<Vec<StatementLineItem>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        matching::statement_lines(
            &**client,
            scope.tenant(),
            company_id,
            bank_account_id,
            matching::Bounds {
                to,
                ..Default::default()
            },
        )
        .await
    }

    // Reconciliations

    /// The figures a reconciliation of the bank account would show as of the
    /// end date against the statement balance, without opening one
    /// (`finance.reconciliation.view`). `pins` fix the reading to a
    /// `through_entry_number`, `through_statement_number` and
    /// `through_match_event_number` (each 0 or more), which is how a
    /// finalised reconciliation's figures are reproduced. A missing end
    /// date, a balance that is not money of the book, or a negative pin is
    /// `InvalidOption`. Reads the ledger, so it also needs
    /// `finance.ledger.view`.
    pub async fn preview_reconciliation(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        figures: &NewReconciliation,
        pins: &Pins,
    ) -> Result<ReconciliationPreview> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let account = bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        if [
            pins.through_entry_number,
            pins.through_statement_number,
            pins.through_match_event_number,
            pins.through_opening_id,
        ]
        .into_iter()
        .flatten()
        .any(|pin| pin < 0)
        {
            return Err(ReconciliationError::InvalidOption);
        }
        let book = self
            .ledger
            .get_book_on(&**client, scope, company_id)
            .await?;
        let (end_date, balance) = reconciliations::opening(
            figures.end_date,
            &figures.statement_balance,
            book.minor_units,
        )
        .map_err(|_| ReconciliationError::InvalidOption)?;
        reconciliations::compute(
            &self.ledger,
            &**client,
            scope,
            company_id,
            &account,
            end_date,
            balance,
            *pins,
        )
        .await
    }

    /// Opens a reconciliation of the bank account as of the end date
    /// against the statement balance, the balance the bank's statement
    /// shows on that date (`finance.reconciliation.reconciliations.manage`).
    /// One reconciliation of an account is open at a time, and each ends
    /// after the last finalised one; otherwise, and for a missing date or a
    /// balance finer than the book's currency, `Invalid` says so. The
    /// account must be active. Reads the book through the ledger, so it also
    /// needs `finance.ledger.view`.
    pub async fn open_reconciliation(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        new: &NewReconciliation,
    ) -> Result<BankReconciliation> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_RECONCILIATIONS],
                "finance.reconciliation.open_reconciliation",
            )
            .await?;
        let tenant = scope.tenant();
        let account = bank_accounts::find(tx.sql(), tenant, company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        if !account.active {
            return Err(ReconciliationError::BankAccountInactive);
        }
        let book = self.ledger.get_book_on(tx.sql(), scope, company_id).await?;
        let opening =
            reconciliations::opening(new.end_date, &new.statement_balance, book.minor_units);
        let mut errors = opening.as_ref().err().cloned().unwrap_or_default();
        if let Some(end_date) = new.end_date {
            if let Err(more) =
                reconciliations::check_open(tx.sql(), tenant, company_id, account.id, end_date)
                    .await?
            {
                errors.extend(more);
            }
        }
        let (end_date, balance) = match opening {
            Ok(opening) if errors.is_empty() => opening,
            _ => return Err(ReconciliationError::Invalid(errors)),
        };
        let id = reconciliations::insert(
            &mut tx,
            tenant,
            company_id,
            people(scope),
            account.id,
            end_date,
            balance,
        )
        .await?;
        let opened = reconciliations::find(tx.sql(), tenant, company_id, id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(opened)
    }

    /// Finalises an open reconciliation once its difference is zero
    /// (`Difference` otherwise), with `finance.reconciliation.reconciliations.finalise`:
    /// records the figures, the numbers they were read at and the items that
    /// explain them, after which the reconciliation is immutable and the
    /// matches it counted are kept. Needs `finance.ledger.view`.
    ///
    /// The finaliser differs from whoever opened it (`SamePerson`),
    /// impersonation included, unless the company has turned separation of
    /// duties off, when the finalisation is listed in the ledger's approval
    /// exceptions ([`Ledger::check_approval`], asked on this write).
    pub async fn finalise_reconciliation(
        &self,
        scope: &Scope,
        company_id: i64,
        reconciliation_id: i64,
    ) -> Result<BankReconciliation> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[FINALISE_RECONCILIATIONS],
                "finance.reconciliation.finalise_reconciliation",
            )
            .await?;
        let tenant = scope.tenant();
        let open = reconciliations::find(tx.sql(), tenant, company_id, reconciliation_id)
            .await?
            .ok_or(ReconciliationError::ReconciliationNotFound)?;
        reconciliations::still_open(&open)?;
        let account = bank_accounts::find(tx.sql(), tenant, company_id, open.bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        self.ledger
            .check_approval(
                &mut tx,
                scope,
                company_id,
                &Approval {
                    source: MODULE.into(),
                    action: "reconciliation_finalisation".into(),
                    subject_type: "reconciliation".into(),
                    subject_id: open.id,
                    subject: format!(
                        "Reconciliation {} of {} to {}",
                        open.reconciliation_number, account.name, open.end_date
                    ),
                    prepared_by: vec![Preparer {
                        user_id: open.opened_by_user_id,
                        impersonator_id: open.opened_by_impersonator_id,
                    }],
                    override_reason: None,
                },
            )
            .await?;
        let read = reconciliations::compute(
            &self.ledger,
            tx.sql(),
            scope,
            company_id,
            &account,
            open.end_date,
            open.statement_balance,
            Pins::default(),
        )
        .await?;
        if !read.figures.difference.is_zero() {
            return Err(ReconciliationError::Difference(read.figures.difference));
        }
        reconciliations::record_finalised(
            &mut tx,
            tenant,
            company_id,
            people(scope),
            open.id,
            read.figures.clone(),
        )
        .await?;
        let mut finalised = reconciliations::find(tx.sql(), tenant, company_id, open.id)
            .await?
            .ok_or(Error::Internal)?;
        finalised.figures = Some(read.figures);
        tx.commit().await?;
        Ok(finalised)
    }

    /// Abandons an open reconciliation with a needed `reason`
    /// (`finance.reconciliation.reconciliations.manage`); a finalised one is
    /// kept (`ReconciliationFinalised`).
    pub async fn abandon_reconciliation(
        &self,
        scope: &Scope,
        company_id: i64,
        reconciliation_id: i64,
        reason: Option<&str>,
    ) -> Result<BankReconciliation> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_RECONCILIATIONS],
                "finance.reconciliation.abandon_reconciliation",
            )
            .await?;
        let tenant = scope.tenant();
        let open = reconciliations::find(tx.sql(), tenant, company_id, reconciliation_id)
            .await?
            .ok_or(ReconciliationError::ReconciliationNotFound)?;
        reconciliations::still_open(&open)?;
        let reason = reconciliations::abandon_reason(reason)
            .map_err(|error| ReconciliationError::Invalid(vec![error]))?;
        reconciliations::record_abandoned(
            &mut tx,
            tenant,
            company_id,
            people(scope),
            open.id,
            reason,
        )
        .await?;
        let abandoned = reconciliations::find(tx.sql(), tenant, company_id, open.id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(abandoned)
    }

    /// One reconciliation with its status, events and, unless abandoned, its
    /// figures (`finance.reconciliation.view`): an open reconciliation's are
    /// read now, a finalised one's are its stored snapshot, which
    /// [`Reconciliation::preview_reconciliation`] at its pinned numbers
    /// reproduces. Needs `finance.ledger.view`.
    pub async fn get_reconciliation(
        &self,
        scope: &Scope,
        company_id: i64,
        reconciliation_id: i64,
    ) -> Result<BankReconciliation> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.get_reconciliation_on(&**client, scope, company_id, reconciliation_id)
            .await
    }

    /// Authorized bank bridge, including its frozen finalisation, on the caller's snapshot.
    pub async fn get_reconciliation_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        reconciliation_id: i64,
    ) -> Result<BankReconciliation> {
        authorize(client, scope, company_id, VIEW).await?;
        let tenant = scope.tenant();
        let mut found = reconciliations::find(client, tenant, company_id, reconciliation_id)
            .await?
            .ok_or(ReconciliationError::ReconciliationNotFound)?;
        let account = bank_accounts::find(client, tenant, company_id, found.bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        found.figures = match found.status {
            ReconciliationStatus::Open => Some(
                reconciliations::compute(
                    &self.ledger,
                    client,
                    scope,
                    company_id,
                    &account,
                    found.end_date,
                    found.statement_balance,
                    Pins::default(),
                )
                .await?
                .figures,
            ),
            ReconciliationStatus::Finalised => {
                let event = found
                    .events
                    .iter()
                    .find(|event| event.action == "finalised")
                    .ok_or(Error::Internal)?;
                let book = self.ledger.get_book_on(client, scope, company_id).await?;
                Some(Figures {
                    book_balance: event.book_balance.ok_or(Error::Internal)?,
                    difference: event.difference.ok_or(Error::Internal)?,
                    currency_code: book.currency_code,
                    minor_units: book.minor_units,
                    through_entry_number: event.through_entry_number.ok_or(Error::Internal)?,
                    through_statement_number: event
                        .through_statement_number
                        .ok_or(Error::Internal)?,
                    through_match_event_number: event
                        .through_match_event_number
                        .ok_or(Error::Internal)?,
                    through_opening_id: event.through_opening_id.ok_or(Error::Internal)?,
                    items: reconciliations::items(client, tenant, company_id, found.id).await?,
                })
            }
            ReconciliationStatus::Abandoned => None,
        };
        Ok(found)
    }

    pub async fn activity_boundary_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
        through: Date,
    ) -> Result<ActivityBoundary> {
        authorize(client, scope, company_id, VIEW).await?;
        let account = bank_accounts::find(client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let through_entry_number = self
            .ledger
            .latest_account_entry_number_on(
                client,
                scope,
                company_id,
                account.ledger_account_id,
                through,
            )
            .await?;
        let (through_statement_number, through_match_event_number) = matching::activity_boundary(
            client,
            scope.tenant(),
            company_id,
            bank_account_id,
            through,
        )
        .await?;
        Ok(ActivityBoundary {
            through_entry_number,
            through_statement_number,
            through_match_event_number,
        })
    }

    /// The bank account's reconciliations, oldest first, with their status
    /// and events (`finance.reconciliation.view`).
    pub async fn list_reconciliations(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
    ) -> Result<Vec<BankReconciliation>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        reconciliations::list(&**client, scope.tenant(), company_id, bank_account_id).await
    }

    // Posting

    /// Posts an unmatched statement line the books do not have as a bank
    /// charge or interest (`finance.reconciliation.statement-lines.post`),
    /// dated the line's date with the line's description unless one is
    /// given, as an entry the line owns, and matches the line to it in the
    /// same write. Posting goes through the ledger facade on this write, so
    /// the actor also needs `finance.ledger.entries.post` and
    /// `finance.ledger.view`, and the ledger's refusal (a closed period, an
    /// inactive account) is returned with nothing recorded. Other refusals:
    /// `InvalidKind`, `NoChargesAccount`, `NoInterestAccount`, `NotACharge`
    /// (the line is money in), `NotInterest` (money out),
    /// `StatementLineMatched`, and `StatementLinePosted` for a line that
    /// already posted an entry (the ledger holds that entry as this line's:
    /// match the line to it instead).
    pub async fn post_statement_line(
        &self,
        scope: &Scope,
        company_id: i64,
        statement_line_id: i64,
        post: &PostLine,
    ) -> Result<PostedStatementLine> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[POST_STATEMENT_LINES],
                "finance.reconciliation.post_statement_line",
            )
            .await?;
        let tenant = scope.tenant();
        let line = matching::find_line(tx.sql(), tenant, company_id, statement_line_id)
            .await?
            .ok_or(ReconciliationError::StatementLineNotFound)?;
        let account = bank_accounts::find(tx.sql(), tenant, company_id, line.bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        if !account.active {
            return Err(ReconciliationError::BankAccountInactive);
        }
        let posted = posting::post(
            &mut tx,
            &self.ledger,
            scope,
            company_id,
            &account,
            &line,
            &post.kind,
            post.description.as_deref(),
        )
        .await?;
        tx.commit().await?;
        Ok(posted)
    }

    /// Reverses the charge or interest entry a statement line posted
    /// (`finance.reconciliation.statement-lines.manage`), with a needed
    /// reason (`InvalidReason` when blank or over 500 characters) and
    /// optionally the reversal's date (today, UTC, when none). The line must
    /// stand matched, alone, to the entry it posted
    /// (`StatementLineNotMatched`, or `StatementLineNotPosted` for any other
    /// match). In one write the match is unmatched, its `unmatched` event
    /// carrying the reason, and the entry is reversed through
    /// [`Ledger::reverse_owned_entry`] as the line's owner; the ledger's
    /// refusal (a closed period) rolls both back and is returned. A match a
    /// finalised reconciliation counted is kept (`MatchFinalised`). The line
    /// is not posted again: the ledger holds an entry for it
    /// (`StatementLinePosted`), so the right figure is booked by a manual
    /// journal and matched. Also needs `finance.ledger.entries.post` and
    /// `finance.ledger.view`.
    pub async fn reverse_statement_line(
        &self,
        scope: &Scope,
        company_id: i64,
        statement_line_id: i64,
        reverse: &ReverseLine,
    ) -> Result<ReversedStatementLine> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                &[MANAGE_STATEMENT_LINES],
                "finance.reconciliation.reverse_statement_line",
            )
            .await?;
        let tenant = scope.tenant();
        let line = matching::find_line(tx.sql(), tenant, company_id, statement_line_id)
            .await?
            .ok_or(ReconciliationError::StatementLineNotFound)?;
        let account = bank_accounts::find(tx.sql(), tenant, company_id, line.bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        let reversed = posting::reverse(
            &mut tx,
            &self.ledger,
            scope,
            company_id,
            &account,
            &line,
            reverse.reason.as_deref(),
            reverse.entry_date,
        )
        .await?;
        tx.commit().await?;
        Ok(reversed)
    }

    /// The bank account, which must be active.
    async fn active_bank_account(
        &self,
        scope: &Scope,
        company_id: i64,
        bank_account_id: i64,
    ) -> Result<BankAccount> {
        let client = self.db.get().await.map_err(Error::from)?;
        let account = bank_accounts::find(&**client, scope.tenant(), company_id, bank_account_id)
            .await?
            .ok_or(ReconciliationError::BankAccountNotFound)?;
        if account.active {
            Ok(account)
        } else {
            Err(ReconciliationError::BankAccountInactive)
        }
    }

    /// Every write: each capability and company reach now, then the audited
    /// transaction holding the company's row lock (Bilimbi `write/4` with
    /// `lock_writable_company/2`).
    async fn write<'c>(
        &self,
        client: &'c mut tokio_postgres::Client,
        scope: &Scope,
        company_id: i64,
        capabilities: &[&str],
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        Ok(self
            .ledger
            .write(client, scope, company_id, capabilities, action)
            .await?)
    }
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

fn refusal(error: Error) -> ReconciliationError {
    match error {
        Error::Forbidden | Error::Unauthorized => ReconciliationError::Unauthorized,
        Error::NotFound => ReconciliationError::CompanyNotFound,
        other => ReconciliationError::Failed(other),
    }
}

/// The scope's actor and any impersonator, as every record names them.
pub(crate) fn people(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

fn as_text<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
