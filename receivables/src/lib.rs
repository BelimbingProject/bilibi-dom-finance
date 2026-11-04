//! Receivables documents, settlements and figures, mirroring Bilimbi.Finance.Receivables.
//! H2 exposes reviewed agent operations. The reference has no pages (`web: nil`);
//! `web/` adds them, calling those same operations (ADR 01).
mod agent_operations;
mod companies;
mod configuration;
mod contributions;
mod documents;
mod evidence;
mod opening;
mod opening_csv;
mod opening_lifecycle;
mod opening_operations;
mod pdf;
mod posting;
mod records;
mod reports;
mod settlements;
mod sources;
mod store;
mod validation;
mod web;
mod write_offs;

pub use contributions::{
    APPROVE_WRITE_OFFS, CAPABILITIES, ISSUE, MANAGE_CUSTOMERS, MANAGE_DOCUMENTS, MANAGE_SETUP,
    POST_PAYMENTS, SUBMIT_WRITE_OFFS, VIEW,
};
pub use evidence::EvidencePage;
pub use pdf::PdfFile;
pub use records::{
    Allocation, Customer, Document, DocumentLine, DocumentQuery, InvoiceLayout, Page, Payment,
    PaymentQuery, PaymentReversal, Series, Setup, SetupChange, TaxCode, TaxLine,
};
pub use reports::{
    AgedCustomer, AgedDocument, AgedReceivables, AgingBucket, AgingOptions, BalancePins,
    ControlAccountTieOut, ControlAccountsTieOut, OtherPosting, ReceivedTaxOptions,
    ReceivedTaxTotal, TaxOptions, TaxTotal,
};
pub use write_offs::{WriteOff, WriteOffEvent};

use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_finance_attachments::Attachments;
use bilibi_finance_ledger::{Ledger, LedgerError};
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_tenancy::Scope;
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const MODULE: &str = "finance/receivables";
pub const SERVICE: &str = "finance/receivables.receivables";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/artifacts", "base/media", "base/settings", "base/settings_editor", "base/agent_api", "base/authz", "base/database", "base/datetime", "base/locale", "base/menu", "base/party_ids", "base/records", "base/tenancy", "base/ui", "core/address", "core/company", "core/party", "finance/ledger", "finance/attachments"],
        migrations: &[("finance-receivables-001-receivables", include_str!("../migrations/001-receivables.sql")), ("finance-receivables-002-write-offs", include_str!("../migrations/002-write-offs.sql")), ("finance-receivables-003-core-company", include_str!("../migrations/003-core-company.sql")), ("finance-receivables-004-opening-items", include_str!("../migrations/004-opening-items.sql")),
            ("finance-receivables-005-opening-lifecycle", include_str!("../migrations/005-opening-lifecycle.sql")), ("finance-receivables-006-party-facts", include_str!("../migrations/006-party-facts.sql")), ("finance-receivables-007-core-party-code", include_str!("../migrations/007-core-party-code.sql")),
            ("finance-receivables-008-opening-credits", include_str!("../migrations/008-opening-credits.sql")),
            ("finance-receivables-009-credit-settlements", include_str!("../migrations/009-credit-settlements.sql")),
            ("finance-receivables-010-invoice-layout", include_str!("../migrations/010-invoice-layout.sql"))],
        services: &[Service { id: SERVICE, factory: |services| {
            let receivables = Arc::new(Receivables {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                ledger: services.get(bilibi_finance_ledger::SERVICE)?,
                attachments: services.get(bilibi_finance_attachments::SERVICE)?,
                parties: services.get("base/tenancy.parties")?,
                records: services.get(bilibi_records::SERVICE)?,
                artifacts: services.get("base/artifacts.store")?,
                media: services.get(bilibi_media::SERVICE)?,
                settings: services.get("base/settings.store")?,
            });
            agent_operations::bind(receivables.clone())?;
            opening_operations::bind(receivables.clone())?;
            Ok(receivables)
        }}, Service { id: pdf::FILES, factory: |services| Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(pdf::Downloads { receivables: services.get(SERVICE)? })))) }, Service { id: web::PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            plans: services.get("base/agent_api.plans")?,
            display: services.get("base/datetime.display")?,
            locales: services.get("base/locale.locales")?,
            parties: services.get("base/tenancy.parties")?,
            records: services.get(bilibi_records::SERVICE)?,
        })))) }],
    }
}

pub type Result<T> = std::result::Result<T, ReceivableError>;

#[derive(Debug, thiserror::Error)]
pub enum ReceivableError {
    #[error("Access denied")]
    Unauthorized,
    #[error("Not found")]
    CompanyNotFound,
    #[error("This company is archived and read-only")]
    CompanyArchived,
    #[error("Receivables is not configured")]
    NotConfigured,
    #[error("Series not found")]
    SeriesNotFound,
    #[error("Tax code not found")]
    TaxCodeNotFound,
    #[error("Customer not found")]
    CustomerNotFound,
    #[error("Document not found")]
    DocumentNotFound,
    #[error("Not a draft")]
    NotADraft,
    #[error("Payment not found")]
    PaymentNotFound,
    #[error("Already reversed")]
    AlreadyReversed,
    #[error(transparent)]
    Attachment(#[from] bilibi_finance_attachments::AttachmentError),
    #[error("PDF could not be generated: {0}")]
    Pdf(&'static str),
    #[error("Only issued documents can be printed")]
    NotIssued,
    #[error("Invalid option")]
    InvalidOption,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Failed(#[from] Error),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FieldError {
    pub field: String,
    /// One-based input line number, if this error belongs to a line.
    pub line: Option<usize>,
    pub message: String,
}

pub(crate) fn invalid(field: &str, message: impl Into<String>) -> ReceivableError {
    ReceivableError::Invalid(vec![FieldError {
        field: field.into(),
        line: None,
        message: message.into(),
    }])
}

fn refusal(error: Error) -> ReceivableError {
    match error {
        Error::Forbidden | Error::Unauthorized => ReceivableError::Unauthorized,
        Error::NotFound => ReceivableError::CompanyNotFound,
        other => ReceivableError::Failed(other),
    }
}

pub(crate) async fn authorize(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company, capability)
        .await
        .map(|_| ())
        .map_err(refusal)
}

/// Each method authorizes the sealed scope and explicit target company.
/// Attributes are JSON objects with Bilimbi's field names; immutable and
/// unknown fields are ignored, and omitted fields preserve existing values.
pub struct Receivables {
    db: Db,
    artifacts: Arc<bilibi_artifacts::Artifacts>,
    media: Arc<bilibi_media::Media>,
    settings: Arc<bilibi_settings::Settings>,
    ledger: Arc<Ledger>,
    attachments: Arc<Attachments>,
    parties: Arc<bilibi_tenancy::PartyProofs>,
    records: Arc<bilibi_records::Records>,
}

impl Receivables {
    async fn authorize_on(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company: i64,
        capability: &str,
    ) -> Result<()> {
        bilibi_company::authorize_work_target(client, scope, company, capability)
            .await
            .map(|_| ())
            .map_err(refusal)
    }

    // No pooled side reads: preparation and execution share the handed client.
    async fn join(
        &self,
        client: &dyn bilibi_agent_api::OperationClient,
        scope: &Scope,
        company: i64,
        capability: &str,
    ) -> Result<()> {
        self.authorize_on(client, scope, company, capability)
            .await?;
        let row = client.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT", &[&scope.tenant(), &company]).await?.ok_or(ReceivableError::CompanyNotFound)?;
        if row.get::<_, String>(0) == "archived" {
            return Err(ReceivableError::CompanyArchived);
        }
        Ok(())
    }

    pub fn series_kinds() -> &'static [&'static str] {
        &["invoice", "credit_note", "payment"]
    }
    pub fn document_kinds() -> &'static [&'static str] {
        &["invoice", "credit_note"]
    }

    async fn write<'c>(
        &self,
        client: &'c mut tokio_postgres::Client,
        scope: &Scope,
        company: i64,
        capability: &str,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        authorize(client, scope, company, capability).await?;
        let tx = self
            .ledger
            .write(client, scope, company, &[capability], action)
            .await
            .map_err(|e| match e {
                LedgerError::CompanyNotFound => ReceivableError::CompanyNotFound,
                LedgerError::CompanyArchived => ReceivableError::CompanyArchived,
                LedgerError::Unauthorized => ReceivableError::Unauthorized,
                other => ReceivableError::Ledger(other),
            })?;
        authorize(tx.sql(), scope, company, capability).await?;
        Ok(tx)
    }
}

pub use bilibi_finance_ledger::PartyFacts;
