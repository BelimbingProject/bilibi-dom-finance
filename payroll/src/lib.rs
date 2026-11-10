//! Payroll recognition, mirroring Bilimbi `Bilimbi.Finance.Payroll`.
//! Only this facade is public. People events feed a Finance-owned projection.
mod configuration;
mod contributions;
mod evidence;
mod posting;
mod source;
mod store;
mod web;

use bilibi_database::{Db, Error};
use bilibi_finance_ledger::{FieldError, Ledger, LedgerError};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
pub use contributions::{CAPABILITIES, MANAGE_SETTINGS, POST_POSTINGS, VIEW};
pub use evidence::EvidencePage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const MODULE: &str = "finance/payroll";
pub const SERVICE: &str = "finance/payroll.payroll";
pub type Result<T> = std::result::Result<T, PayrollError>;

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/authz", "base/database", "base/datetime", "base/events", "base/locale", "base/menu", "base/principal_directory", "base/settings", "base/tenancy", "base/ui", "core/company", "finance/ledger"],
        migrations: &[("finance-payroll-001-payroll", include_str!("../migrations/001-payroll.sql")), ("finance-payroll-002-people-source", include_str!("../migrations/002-people-source.sql"))],
        services: &[Service { id: SERVICE, factory: |services| Ok(Arc::new(Payroll {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            ledger: services.get(bilibi_finance_ledger::SERVICE)?,
            settings: services.get("base/settings.store")?,
            events: services.get(bilibi_events::SERVICE)?,
        })) }, Service { id: "finance/payroll.pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                payroll: services.get(SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                directory: services.get(bilibi_finance_ledger::PEOPLE)?,
            }))))
        } }],
    }
}

#[derive(Clone)]
pub struct Payroll {
    events: Arc<bilibi_events::Events>,
    db: Db,
    ledger: Arc<Ledger>,
    settings: Arc<bilibi_settings::Settings>,
}

#[derive(Debug, thiserror::Error)]
pub enum PayrollError {
    #[error("You do not have permission to do that.")]
    Unauthorized,
    #[error("The company could not be found.")]
    CompanyNotFound,
    #[error("The company is archived; nothing can be changed.")]
    CompanyArchived,
    #[error("Set the posting accounts first.")]
    PayrollNotConfigured,
    #[error("The payroll and claims source is not installed.")]
    PeopleUnavailable,
    #[error("That run or claim could not be found.")]
    SourceNotFound,
    #[error("Only a finalised run can be posted.")]
    NotFinalised,
    #[error("It is already posted.")]
    AlreadyPosted,
    #[error("That posting is already reversed.")]
    AlreadyReversed,
    #[error("The posting no longer exists.")]
    PostingNotFound,
    #[error("Invalid option")]
    InvalidOption,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error("The action could not be completed: {0}.")]
    AttemptFailed(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Failed(#[from] Error),
}

impl From<tokio_postgres::Error> for PayrollError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Failed(error.into())
    }
}

/// The posting accounts in force. IDs, not chart codes, are company data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub company_id: i64,
    pub net_pay_account_id: i64,
    pub claims_payable_account_id: i64,
}

/// Omitted settings retain their existing values; both are required initially.
#[derive(Clone, Debug, Default)]
pub struct SettingsChange {
    pub net_pay_account_id: Option<i64>,
    pub claims_payable_account_id: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    pub id: i64,
    pub kind: String,
    pub code: String,
    pub direction: String,
    pub expense_account_id: Option<i64>,
    pub liability_account_id: Option<i64>,
}

/// Upserts by the immutable kind/code/direction key. No delete operation.
#[derive(Clone, Debug)]
pub struct MappingChange {
    pub kind: String,
    pub code: String,
    pub direction: String,
    pub expense_account_id: Option<i64>,
    pub liability_account_id: Option<i64>,
    pub reason: Option<String>,
}

/// The posted-event ID identifies a posting, never its ledger entry ID.
/// Dates/times and exact decimal totals are ISO/decimal strings, as in evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Posting {
    pub id: i64,
    pub source_kind: String,
    pub source_id: i64,
    pub source_reference: String,
    pub entry_id: i64,
    pub entry_number: i64,
    pub entry_date: String,
    pub total: String,
    pub currency_code: String,
    pub posted_by_user_id: i64,
    pub posted_by_impersonator_id: Option<i64>,
    pub posted_at: String,
    pub reversal: Option<ReversalRecord>,
    pub last_failure: Option<Attempt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReversalRecord {
    pub entry_id: i64,
    pub entry_number: i64,
    pub entry_date: String,
    pub reason: String,
    pub reversed_by_user_id: i64,
    pub reversed_by_impersonator_id: Option<i64>,
    pub reversed_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub action: String,
    pub reason: String,
    pub actor_user_id: i64,
    pub occurred_at: String,
}

/// A reversal needs a nonblank reason; omitted date uses the company's local
/// calendar (Base DateTime), not the browser or the source's calendar.
#[derive(Clone, Debug, Default)]
pub struct Reversal {
    pub reason: String,
    pub entry_date: Option<jiff::civil::Date>,
}

impl Payroll {
    pub fn mapping_kinds() -> &'static [&'static str] {
        &["component", "claim_category"]
    }
    pub fn component_directions() -> &'static [&'static str] {
        &["earning", "deduction", "employer"]
    }
    pub fn people_available(&self) -> bool {
        source::available(&self.events)
    }
    pub fn evidence_datasets() -> &'static [&'static str] {
        evidence::DATASETS
    }

    fn require_source(&self) -> Result<()> {
        if self.people_available() {
            Ok(())
        } else {
            Err(PayrollError::PeopleUnavailable)
        }
    }

    /// People-dependent reads authorize before answering unavailable.
    pub async fn list_payroll_runs(&self, scope: &Scope, company: i64) -> Result<Vec<Value>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        self.require_source()?;
        let mut rows = source::runs(&**client, scope, company).await?;
        self.source_standing(&**client, scope, company, "payroll_run", &mut rows)
            .await?;
        Ok(rows)
    }
    pub async fn list_claims(&self, scope: &Scope, company: i64) -> Result<Vec<Value>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        self.require_source()?;
        let claims = source::claims(&**client, scope, company).await?;
        let mut rows = claims.iter().map(|c| serde_json::json!({"id":c.id,"reference":c.reference,"currency_code":c.currency_code,"approved_on":c.approved_on.map(|d|d.to_string()),"amount":c.amount.to_string(),"category_code":c.category_code})).collect::<Vec<_>>();
        self.source_standing(&**client, scope, company, "claim", &mut rows)
            .await?;
        Ok(rows)
    }

    async fn source_standing(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company: i64,
        kind: &str,
        rows: &mut [Value],
    ) -> Result<()> {
        let postings = store::postings(client, scope.tenant(), company).await?;
        let events = client.query("SELECT source_id,jsonb_build_object('reason',reason) FROM finance_payroll_posting_events WHERE tenant_id=$1 AND company_id=$2 AND source_kind=$3 AND event='posting_failed' ORDER BY id DESC", &[&scope.tenant(),&company,&kind]).await?;
        for row in rows {
            let id = row["id"].as_i64();
            let posting = postings
                .iter()
                .find(|p| p.source_kind == kind && Some(p.source_id) == id);
            row["posting"] = serde_json::to_value(posting).map_err(|_| Error::Internal)?;
            row["last_failure"] = match posting {
                Some(p) => serde_json::to_value(&p.last_failure).map_err(|_| Error::Internal)?,
                None => events
                    .iter()
                    .find(|e| Some(e.get::<_, i64>(0)) == id)
                    .map_or(Value::Null, |e| e.get(1)),
            };
        }
        Ok(())
    }

    pub async fn list_postings(&self, scope: &Scope, company: i64) -> Result<Vec<Posting>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        store::postings(&**client, scope.tenant(), company).await
    }
    pub async fn get_posting(&self, scope: &Scope, company: i64, id: i64) -> Result<Posting> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        store::posting(&**client, scope.tenant(), company, id).await
    }
    pub async fn list_posting_events(&self, scope: &Scope, company: i64) -> Result<Vec<Value>> {
        self.log(scope, company, "posting_event").await
    }
    pub async fn list_settings_changes(&self, scope: &Scope, company: i64) -> Result<Vec<Value>> {
        self.log(scope, company, "settings_change").await
    }
    pub async fn list_mapping_changes(&self, scope: &Scope, company: i64) -> Result<Vec<Value>> {
        self.log(scope, company, "mapping_change").await
    }
    async fn log(&self, scope: &Scope, company: i64, dataset: &str) -> Result<Vec<Value>> {
        // Bilimbi's unpaged operational histories are complete. Evidence
        // retains its bounded contract; walk those pages inside the facade.
        let mut rows = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .evidence_page(scope, company, dataset, Some(500), after)
                .await?;
            rows.extend(page.rows);
            after = page.next;
            if after.is_none() {
                return Ok(rows);
            }
        }
    }
}

async fn authorize(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company, capability)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            Error::Forbidden | Error::Unauthorized => PayrollError::Unauthorized,
            Error::NotFound => PayrollError::CompanyNotFound,
            other => PayrollError::Failed(other),
        })
}

fn ledger_error(error: LedgerError) -> PayrollError {
    match error {
        LedgerError::Unauthorized => PayrollError::Unauthorized,
        LedgerError::CompanyNotFound => PayrollError::CompanyNotFound,
        LedgerError::CompanyArchived => PayrollError::CompanyArchived,
        other => PayrollError::Ledger(other),
    }
}

fn reason(value: Option<&str>, required: bool) -> Result<Option<String>> {
    let value = value.map(str::trim).filter(|s| !s.is_empty());
    let message = match value {
        None if required => Some("can't be blank"),
        Some(s) if s.chars().count() > 500 => Some("should be at most 500 character(s)"),
        _ => None,
    };
    if let Some(message) = message {
        return Err(PayrollError::Invalid(vec![FieldError::new(
            "reason", message,
        )]));
    }
    Ok(value.map(str::to_owned))
}
