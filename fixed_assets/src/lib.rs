//! Bilimbi.Finance.FixedAssets: immutable category defaults and acquisition
//! assumptions, monthly runs and immutable corrections. Registration recognises
//! an existing debit; it never posts or takes ownership of it.
mod acquisitions;
mod activity;
mod categories;
mod contributions;
mod opening;
mod opening_csv;
mod opening_evidence;
mod opening_lifecycle;
mod opening_operations;
mod operations;
mod reports;
mod schedule;
mod store;
mod web;

pub use acquisitions::{AcquisitionSource, SourceKind};
pub use activity::{DisposalInput, Event, EventAction, EventInput, Run, RunException, RunItem};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_finance_ledger::{FieldError, Ledger, LedgerError, Money};
use bilibi_finance_payables::{Payables, PayablesError};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
pub use contributions::{
    ACQUIRE, CAPABILITIES, CONFIGURE, DEPRECIATE, DISPOSE, REVERSE, VIEW, VOID,
};
use jiff::civil::Date;
pub use reports::{Reconciliation, ReconciliationKind, RegisterReport, RegisterRow, ReportPins};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub use store::EvidencePage;
use tokio_postgres::GenericClient;

pub const MODULE: &str = "finance/fixed_assets";
pub const SERVICE: &str = "finance/fixed_assets.fixed_assets";
inventory::submit! { Module {
    id: MODULE, layer: Layer::Domains,
    dependencies: &["base/agent_api", "base/authz", "base/database", "base/tenancy", "base/datetime", "base/locale", "base/menu", "base/party_ids", "base/principal_directory", "base/ui", "core/company", "finance/ledger", "finance/payables"],
    migrations: &[
        ("finance-fixed-assets-001-acquisitions", include_str!("../migrations/001-acquisitions.sql")),
        ("finance-fixed-assets-002-runs-corrections", include_str!("../migrations/002-runs-corrections.sql")),
        ("finance-fixed-assets-003-opening-items", include_str!("../migrations/003-opening-items.sql")),
            ("finance-fixed-assets-004-opening-lifecycle", include_str!("../migrations/004-opening-lifecycle.sql")),
        ("finance-fixed-assets-005-fully-depreciated-opening", include_str!("../migrations/005-fully-depreciated-opening.sql")),
    ],
    services: &[Service { id: SERVICE, factory: |s| { let assets = Arc::new(FixedAssets {
        db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
        ledger: s.get(bilibi_finance_ledger::SERVICE)?,
        payables: s.get(bilibi_finance_payables::SERVICE)?,
        parties: s.get("base/tenancy.parties")?,

    }); opening_operations::bind(assets.clone())?; operations::bind(assets.clone())?; Ok(assets) } }, Service { id: web::PAGES, factory: |s| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
        contracts: s.get(bilibi_agent_api::page::SERVICE)?,
        plans: s.get("base/agent_api.plans")?,
        db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
        display: s.get("base/datetime.display")?,
        locales: s.get("base/locale.locales")?,
        directory: s.get("base/principal_directory.directory")?,
    })))) }],
} }

pub type Result<T> = std::result::Result<T, FixedAssetsError>;
#[derive(Debug, thiserror::Error)]
pub enum FixedAssetsError {
    #[error("You do not have permission to do that.")]
    Unauthorized,
    #[error("Company not found")]
    CompanyNotFound,
    #[error("This company is archived and cannot be changed.")]
    CompanyArchived,
    #[error("Not found")]
    NotFound,
    #[error("Bill not posted")]
    BillNotPosted,
    #[error("Bill line not found")]
    BillLineNotFound,
    #[error("Entry line not found")]
    EntryLineNotFound,
    #[error("Not acquisition debit")]
    NotAcquisitionDebit,
    #[error("The acquisition entry was reversed. Void the asset's acquisition from its page.")]
    AcquisitionReversed,
    #[error("Invalid period")]
    InvalidPeriod,
    #[error("Open a fiscal year covering this month before running depreciation.")]
    PeriodNotFound,
    #[error("The period is closed. Choose an open period.")]
    PeriodClosed,
    #[error("This date precedes recorded asset activity. Choose a later date.")]
    LaterActivity,
    #[error("Reverse later depreciation runs and standing disposals first.")]
    DependentPostings,
    #[error("Empty run")]
    EmptyRun,
    #[error("Already reversed")]
    AlreadyReversed,
    #[error("Before original")]
    BeforeOriginal,
    #[error("Before acquisition")]
    BeforeAcquisition,
    #[error("Already disposed")]
    AlreadyDisposed,
    #[error("Not disposed")]
    NotDisposed,
    #[error("Post depreciation through the last completed month before disposing of this asset.")]
    DepreciationRequired,
    #[error("Invalid settlement account")]
    InvalidSettlementAccount,
    #[error("This asset's acquisition was voided, so it cannot be disposed of.")]
    AssetVoided,
    #[error("This asset's acquisition is already voided.")]
    AlreadyVoided,
    #[error("Only an asset whose acquisition entry was reversed in the ledger can be voided.")]
    AcquisitionNotReversed,
    #[error("Reverse the standing disposal before voiding this asset's acquisition.")]
    DisposalStanding,
    #[error("Invalid option")]
    InvalidOption,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Payables(#[from] PayablesError),
    #[error(transparent)]
    Failed(#[from] Error),
}
impl From<tokio_postgres::Error> for FixedAssetsError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Failed(e.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    StraightLine,
    ReducingBalance,
}

/// Full assumptions, required on each category revision. The annual percentage
/// is required only for reducing balance; straight line always clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumptions {
    pub method: Method,
    pub life_months: i32,
    pub annual_rate: Option<Decimal>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevisionInput {
    pub cost_account_id: i64,
    pub accumulated_account_id: i64,
    pub expense_account_id: i64,
    pub gain_account_id: i64,
    pub loss_account_id: i64,
    #[serde(flatten)]
    pub assumptions: Assumptions,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CategoryRevision {
    pub id: i64,
    pub category_id: i64,
    pub name: String,
    pub defaults: RevisionInput,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub revision: CategoryRevision,
    pub revisions: Vec<CategoryRevision>,
}
/// Only registration-time overrides. Cost, acquisition date and source numbers
/// are never caller input. None inherits a revision's assumption (or zero residual).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionInput {
    pub category_revision_id: i64,
    pub description: String,
    #[serde(with = "date_serde")]
    pub in_service_date: Date,
    #[serde(default, deserialize_with = "money_serde::optional")]
    pub residual_value: Option<Money>,
    pub method: Option<Method>,
    pub life_months: Option<i32>,
    pub annual_rate: Option<Decimal>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: i64,
    pub asset_number: i64,
    pub category_revision_id: i64,
    pub description: String,
    #[serde(with = "date_serde")]
    pub acquisition_date: Date,
    #[serde(with = "date_serde")]
    pub in_service_date: Date,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub cost: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub residual_value: Money,
    pub assumptions: Assumptions,
    pub entry_id: i64,
    pub entry_number: i64,
    pub entry_line_number: Option<i32>,
    #[serde(with = "optional_date_serde")]
    pub opening_date: Option<Date>,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub opening_accumulated: Money,
    pub bill_id: Option<i64>,
    pub bill_line_number: Option<i32>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleRow {
    #[serde(with = "date_serde")]
    pub period: Date,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub amount: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub accumulated: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub carrying_value: Money,
}
/// Posted charge with the earliest correction that reversed it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetCharge {
    pub run_id: i64,
    #[serde(with = "date_serde")]
    pub period: Date,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub amount: Money,
    pub entry_id: Option<i64>,
    pub entry_number: Option<i64>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
    pub reversal: Option<Event>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetDetail {
    pub opening: Option<serde_json::Value>,
    pub asset: Asset,
    pub revision: CategoryRevision,
    pub schedule: Vec<ScheduleRow>,
    pub charges: Vec<AssetCharge>,
    pub events: Vec<Event>,
    pub voided: bool,
}
impl AssetDetail {
    /// Scheduled (not posted) depreciation effective at month-end. A mid-month
    /// date excludes that month's full charge.
    pub fn scheduled_depreciation(&self, as_of: Date) -> Money {
        if self.asset.opening_date.is_some_and(|date| as_of < date) {
            return Money::ZERO;
        }
        self.asset.opening_accumulated
            + self
                .schedule
                .iter()
                .filter(|r| r.period.last_of_month() <= as_of)
                .map(|r| r.amount)
                .sum()
    }
}

pub struct FixedAssets {
    db: Db,
    parties: Arc<bilibi_tenancy::PartyProofs>,
    ledger: Arc<Ledger>,
    payables: Arc<Payables>,
}
impl FixedAssets {
    async fn write<'c>(
        &self,
        c: &'c mut tokio_postgres::Client,
        s: &Scope,
        company: i64,
        cap: &str,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        self.ledger
            .write(c, s, company, &[cap], action)
            .await
            .map_err(|e| match e {
                LedgerError::Unauthorized => FixedAssetsError::Unauthorized,
                LedgerError::CompanyNotFound => FixedAssetsError::CompanyNotFound,
                LedgerError::CompanyArchived => FixedAssetsError::CompanyArchived,
                e => FixedAssetsError::Ledger(e),
            })
    }
    pub fn evidence_datasets(&self) -> Vec<&'static str> {
        store::datasets()
    }
    pub async fn evidence_page(
        &self,
        s: &Scope,
        company: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.evidence_page_on(&**c, s, company, dataset, limit, after)
            .await
    }

    /// Authorized evidence on the caller's snapshot; never opens another connection.
    pub async fn evidence_page_on(
        &self,
        c: &(impl tokio_postgres::GenericClient + Sync),
        s: &Scope,
        company: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        authorize(c, s, company, VIEW).await?;
        store::page(c, s.tenant(), company, dataset, limit, after).await
    }
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
            Error::Forbidden | Error::Unauthorized => FixedAssetsError::Unauthorized,
            Error::NotFound => FixedAssetsError::CompanyNotFound,
            e => FixedAssetsError::Failed(e),
        })
}
fn invalid(field: &'static str, message: impl Into<String>) -> FixedAssetsError {
    FixedAssetsError::Invalid(vec![FieldError::new(field, message)])
}
fn text(input: &str, field: &'static str, max: usize) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid(field, "can't be blank"));
    }
    if input.chars().count() > max {
        return Err(invalid(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
    Ok(input.into())
}

mod date_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        date: &Date,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(date)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Date, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
mod optional_date_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        date: &Option<Date>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        date.map(|d| d.to_string()).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<Date>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|d| d.parse().map_err(serde::de::Error::custom))
            .transpose()
    }
}

// Only trusted contract output is decoded at the column precision. Input money
// is parsed with the book's units before construction in the operation handler.
mod money_serde {
    use super::*;
    use bilibi_finance_ledger::MinorUnits;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Money, D::Error> {
        let amount = <Decimal as Deserialize>::deserialize(d)?;
        Money::new(amount, MinorUnits::new(4).expect("column precision"))
            .map_err(serde::de::Error::custom)
    }
    pub fn optional<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<Money>, D::Error> {
        Option::<Decimal>::deserialize(d)?
            .map(|a| {
                Money::new(a, MinorUnits::new(4).expect("column precision"))
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
    pub fn units<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<MinorUnits, D::Error> {
        MinorUnits::new(i64::deserialize(d)?)
            .ok_or_else(|| serde::de::Error::custom("invalid minor units"))
    }
}
