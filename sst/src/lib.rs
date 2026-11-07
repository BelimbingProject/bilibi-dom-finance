//! Bilimbi.Finance.Sst: Malaysia's Sales and Service Tax return, form
//! SST-02. Each company's SST registration, kept as numbered versions; which
//! SST-02 box each tax code of receivables and payables reports in; the
//! taxable periods of the registration's cycle; and a return's figures,
//! computed from posted documents only through the Receivables and Payables
//! tax totals at one ledger entry number and reconciled against the tax
//! accounts' ledger postings; and the returns of each tax for each taxable
//! period, adjusted while open, finalised into a stored snapshot that never
//! changes, discarded, amended by a new return for the same period, read
//! again at their recorded numbers, exported, and listed as evidence; and
//! the Returns, Return and Setup pages with the CSV download and print page
//! (`web`).
//!
//! Every call authorizes its capability for the scope's signed-in actor and
//! the explicit target company when it runs; a write then holds the company
//! lock through [`Ledger::write`], so an archived company is refused and one
//! company's SST writes run one at a time. Documents and ledger figures are
//! read only through the `finance/receivables`, `finance/payables` and
//! `finance/ledger` facades with the same scope, so a call that computes
//! figures or lists tax codes also needs their view capabilities.
mod contributions;
mod evidence;
mod export;
mod figures;
mod form;
mod mappings;
mod periods;
mod registrations;
mod returns;
mod store;
mod web;

use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_finance_ledger::{FieldError, Ledger, LedgerError};
use bilibi_finance_payables::{Payables, PayablesError};
use bilibi_finance_receivables::{ReceivableError, Receivables};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
pub use contributions::{
    AMEND_RETURNS, CAPABILITIES, FINALISE_RETURNS, MANAGE_SETTINGS, PREPARE_RETURNS, VIEW,
};
pub use evidence::EvidencePage;
pub use export::{Export, ExportFormat};
pub use figures::{AccountReconciliation, Figures, SourceRow, UnmappedRow};
pub use form::{BoxFigure, BoxKind, FormBox, MappableBox};
use jiff::civil::Date;
pub use mappings::{Mapping, MappingInput, TaxCodeMapping};
pub use registrations::{Registration, RegistrationInput};
pub use returns::{
    Adjustment, AdjustmentInput, Amends, BoxChange, PostedSince, Reproduction, ReturnAdjustment,
    ReturnEvent, ReturnReport, ReturnStatus, TaxReturn, Withdrawal,
};
use serde::Serialize;
use std::{fmt, str::FromStr, sync::Arc};
use tokio_postgres::GenericClient;

pub const MODULE: &str = "finance/sst";
pub const SERVICE: &str = "finance/sst.sst";
inventory::submit! { Module {
    id: MODULE, layer: Layer::Domains,
    dependencies: &["base/authz", "base/database", "base/datetime", "base/locale", "base/menu", "base/principal_directory", "base/tenancy", "base/ui", "core/company", "finance/ledger", "finance/payables", "finance/receivables"],
    migrations: &[("finance-sst-001-sst", include_str!("../migrations/001-sst.sql"))],
    services: &[Service { id: SERVICE, factory: |s| Ok(Arc::new(Sst {
        db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
        ledger: s.get(bilibi_finance_ledger::SERVICE)?,
        receivables: s.get(bilibi_finance_receivables::SERVICE)?,
        payables: s.get(bilibi_finance_payables::SERVICE)?,
    })) }, Service { id: web::PAGES, factory: |s| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
        db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
        sst: s.get(SERVICE)?,
        ledger: s.get(bilibi_finance_ledger::SERVICE)?,
        display: s.get("base/datetime.display")?,
        locales: s.get("base/locale.locales")?,
        directory: s.get(bilibi_finance_ledger::PEOPLE)?,
    })))) }, Service { id: web::FILES, factory: |s| Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(web::Downloads {
        sst: s.get(SERVICE)?,
    })))) }],
} }

pub type Result<T> = std::result::Result<T, SstError>;
/// A refusal in the words Bilimbi's SST pages show it.
#[derive(Debug, thiserror::Error)]
pub enum SstError {
    #[error("You do not have permission for that action.")]
    Unauthorized,
    #[error("The company could not be found.")]
    CompanyNotFound,
    #[error("The company is archived; nothing can be changed.")]
    CompanyArchived,
    #[error("The company's ledger book is not open yet.")]
    BookNotOpen,
    #[error("Record the company's SST registration first.")]
    NotRegistered,
    #[error("The registration holds no number for that tax.")]
    NotRegisteredForTax,
    #[error("That date does not start a taxable period of the registration's cycle.")]
    NotATaxablePeriod,
    #[error("Choose sales tax or service tax.")]
    InvalidTaxType,
    #[error("That tax code no longer exists.")]
    TaxCodeNotFound,
    #[error("Choose a receivables or payables tax code.")]
    InvalidSource,
    #[error("The request was malformed.")]
    InvalidOption,
    #[error("That period already has an open return.")]
    ReturnOpen,
    #[error("That return is finalised; its figures are fixed. Amend it to change them.")]
    ReturnFinalised,
    #[error("That return was discarded.")]
    ReturnDiscarded,
    #[error("The return no longer exists.")]
    ReturnNotFound,
    #[error("Only a finalised return is amended.")]
    ReturnNotFinalised,
    #[error("A later finalised version amends that return; amend the latest one.")]
    ReturnSuperseded,
    #[error("That period overlaps another period's return.")]
    PeriodOverlaps,
    #[error("The period has not ended yet.")]
    PeriodNotEnded,
    #[error("The adjustment no longer exists.")]
    AdjustmentNotFound,
    #[error("That adjustment is already withdrawn.")]
    AdjustmentWithdrawn,
    /// Each tax code with tax in the period and no mapping.
    #[error("Map every tax code with tax in the period first: {}.", unmapped(.0))]
    UnmappedTaxCodes(Vec<UnmappedRow>),
    /// Each source row whose code's rate does not fit its box.
    #[error("A tax code's rate does not fit its box: {}.", mismatched(.0))]
    RateMismatch(Vec<SourceRow>),
    #[error("Refused: {}.", refused(.0))]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Ledger(LedgerError),
    #[error(transparent)]
    Receivables(ReceivableError),
    #[error(transparent)]
    Payables(PayablesError),
    #[error(transparent)]
    Failed(#[from] Error),
}
fn refused(errors: &[FieldError]) -> String {
    errors
        .iter()
        .map(|e| format!("{} {}", e.field.replace('_', " "), e.message))
        .collect::<Vec<_>>()
        .join("; ")
}
fn source_name(source: TaxSource) -> &'static str {
    match source {
        TaxSource::Receivables => "Receivables",
        TaxSource::Payables => "Payables",
    }
}
fn unmapped(rows: &[UnmappedRow]) -> String {
    rows.iter()
        .map(|r| format!("{} {}", source_name(r.source), r.tax_code))
        .collect::<Vec<_>>()
        .join(", ")
}
fn mismatched(rows: &[SourceRow]) -> String {
    rows.iter()
        .map(|r| format!("{} ({}%)", r.tax_code, r.rate.normalize()))
        .collect::<Vec<_>>()
        .join(", ")
}
impl From<tokio_postgres::Error> for SstError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Failed(e.into())
    }
}
impl From<LedgerError> for SstError {
    fn from(e: LedgerError) -> Self {
        match e {
            LedgerError::Unauthorized => Self::Unauthorized,
            LedgerError::CompanyNotFound => Self::CompanyNotFound,
            LedgerError::CompanyArchived => Self::CompanyArchived,
            LedgerError::BookNotOpen => Self::BookNotOpen,
            e => Self::Ledger(e),
        }
    }
}
impl From<ReceivableError> for SstError {
    fn from(e: ReceivableError) -> Self {
        match e {
            ReceivableError::Unauthorized => Self::Unauthorized,
            ReceivableError::CompanyNotFound => Self::CompanyNotFound,
            e => Self::Receivables(e),
        }
    }
}
impl From<PayablesError> for SstError {
    fn from(e: PayablesError) -> Self {
        match e {
            PayablesError::Unauthorized => Self::Unauthorized,
            PayablesError::CompanyNotFound => Self::CompanyNotFound,
            e => Self::Payables(e),
        }
    }
}

/// The two taxes a return is filed for, each on its own SST-02.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxType {
    SalesTax,
    ServiceTax,
}
impl TaxType {
    pub const ALL: [TaxType; 2] = [Self::SalesTax, Self::ServiceTax];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SalesTax => "sales_tax",
            Self::ServiceTax => "service_tax",
        }
    }
}
impl FromStr for TaxType {
    type Err = SstError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or(SstError::InvalidTaxType)
    }
}
impl fmt::Display for TaxType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The module whose tax codes a mapping names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxSource {
    Receivables,
    Payables,
}
impl TaxSource {
    pub const ALL: [TaxSource; 2] = [Self::Receivables, Self::Payables];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Receivables => "receivables",
            Self::Payables => "payables",
        }
    }
}
impl FromStr for TaxSource {
    type Err = SstError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or(SstError::InvalidSource)
    }
}

/// When service tax is due: `payment` (when payment is received, section
/// 11(1) of the Service Tax Act) or `invoice` (when invoiced, with the
/// Director General's approval under section 11(1A)).
pub const SERVICE_TAX_BASES: [&str; 2] = ["payment", "invoice"];

/// A taxable period of the registration's cycle with its statutory due date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaxablePeriod {
    pub period_start: Date,
    pub period_end: Date,
    pub due_date: Date,
}

/// The numbers a reading is pinned at, each the latest when absent: the last
/// ledger entry counted and the last tax code mapping applied.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pins {
    pub through_entry_number: Option<i64>,
    pub through_mapping_number: Option<i64>,
}

/// Each method authorizes the sealed scope and explicit target company.
pub struct Sst {
    db: Db,
    ledger: Arc<Ledger>,
    receivables: Arc<Receivables>,
    payables: Arc<Payables>,
}
impl Sst {
    /// The boxes of a tax's SST-02 form in the form's order.
    pub fn boxes(tax_type: TaxType) -> &'static [FormBox] {
        form::boxes(tax_type)
    }
    /// The boxes a tax code of `source` may be mapped to, across both forms.
    pub fn mappable_boxes(source: TaxSource) -> Vec<MappableBox> {
        form::mappable_boxes(source)
    }

    /// Up to `count` (1 to 24, default 6) taxable periods of the current
    /// registration's cycle, newest first, ending with the one that contains
    /// `date` (default today, UTC). Empty before any registration.
    pub async fn taxable_periods(
        &self,
        s: &Scope,
        company: i64,
        date: Option<Date>,
        count: Option<usize>,
    ) -> Result<Vec<TaxablePeriod>> {
        let count = count.unwrap_or(6);
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        if !(1..=24).contains(&count) {
            return Err(SstError::InvalidOption);
        }
        let date = date.unwrap_or_else(today);
        let Some(registration) = registrations::latest(&**c, s.tenant(), company).await? else {
            return Ok(Vec::new());
        };
        let cycle = registration.cycle();
        cycle
            .recent(date, count)
            .into_iter()
            .map(|(period_start, period_end)| {
                Ok(TaxablePeriod {
                    period_start,
                    period_end,
                    due_date: cycle.due_date(period_end).ok_or(SstError::InvalidOption)?,
                })
            })
            .collect()
    }

    /// The figures a `tax_type` return for the taxable period starting
    /// `period_start` would show now, without opening one or counting any
    /// adjustment, under the registration version governing that date
    /// (`NotRegistered`, `NotRegisteredForTax`, `NotATaxablePeriod`).
    /// `pins` reproduce an earlier reading; a negative number is
    /// `InvalidOption`. Also needs the ledger, receivables and payables view
    /// capabilities.
    pub async fn preview_return(
        &self,
        s: &Scope,
        company: i64,
        tax_type: TaxType,
        period_start: Date,
        pins: Pins,
    ) -> Result<Figures> {
        let registration = {
            let c = self.db.get().await.map_err(Error::from)?;
            authorize(&**c, s, company, VIEW).await?;
            if [pins.through_entry_number, pins.through_mapping_number]
                .into_iter()
                .flatten()
                .any(|n| n < 0)
            {
                return Err(SstError::InvalidOption);
            }
            registrations::governing(&**c, s.tenant(), company, period_start)
                .await?
                .ok_or(SstError::NotRegistered)?
        };
        if registration.number(tax_type).is_none() {
            return Err(SstError::NotRegisteredForTax);
        }
        let (start, end) = registration
            .cycle()
            .period(period_start)
            .ok_or(SstError::NotATaxablePeriod)?;
        let period = figures::Period {
            tax_type,
            period_start: start,
            period_end: end,
        };
        self.compute(s, company, &period, &registration, &[], pins)
            .await
    }

    async fn write<'c>(
        &self,
        c: &'c mut tokio_postgres::Client,
        s: &Scope,
        company: i64,
        cap: &str,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        Ok(self.ledger.write(c, s, company, &[cap], action).await?)
    }
}

/// Today in UTC, as Bilimbi's `Date.utc_today/0`.
fn today() -> Date {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
}

async fn authorize(
    c: &(impl GenericClient + Sync),
    s: &Scope,
    company: i64,
    cap: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(c, s, company, cap)
        .await
        .map(|_| ())
        .map_err(|e| match e {
            Error::Forbidden | Error::Unauthorized => SstError::Unauthorized,
            Error::NotFound => SstError::CompanyNotFound,
            e => SstError::Failed(e),
        })
}

fn reason(text: Option<&str>, errors: &mut Vec<FieldError>) -> Option<String> {
    let reason = bilibi_ui::params::blank_to_none(text);
    if reason.as_ref().is_some_and(|r| r.chars().count() > 500) {
        errors.push(FieldError::new(
            "reason",
            "should be at most 500 character(s)",
        ));
    }
    reason
}

/// `limit` (1 to 500, default 100) and `after`, the last id already read.
fn page_options(limit: Option<i64>, after: Option<i64>) -> Result<(i64, i64)> {
    let limit = limit.unwrap_or(100);
    if !(1..=500).contains(&limit) || after.is_some_and(|a| a < 1) {
        return Err(SstError::InvalidOption);
    }
    Ok((limit, after.unwrap_or(0)))
}
