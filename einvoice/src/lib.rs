//! E-invoicing, module `finance/einvoice` (Bilimbi
//! `Bilimbi.Finance.Einvoice`): the fiscal documents a company issues through
//! a clearance authority. This version holds the schema, the company's setup
//! (settings, credentials, signing certificate with its facts, and supplier
//! profile, read here without any secret), the [`Authority`] contract with
//! the one authority this release clears documents with (MyInvois) and its
//! rules: the [`Draft`] a caller asks to issue and its validation, the UBL
//! 2.1 JSON builder with its byte-exact writer, the JSON XAdES signer, and
//! the authority's code lists with each company's overrides; clearance
//! (issuing, submitting, refreshing and cancelling a document); and the
//! ledger bridge: a validated document's one owned entry and a cancelled
//! one's reversal, retried from the reconciliation read, source links to the
//! entry another Finance module already posted, the monthly consolidated
//! document, the printable PDF on Base Artifacts with the validation link's
//! QR code, and the auditor's evidence datasets. Other modules never read its
//! tables.
//!
//! Every function takes the sealed [`Scope`] it was handed and an explicit
//! company id. Reads authorize their capability and writes theirs for the
//! scope's actor and the target company at the moment they run, through Core
//! Company's `authorize_work_target`: a missing grant, or a sibling company without
//! tenant-wide reach, is [`EinvoiceError::Unauthorized`], and a company the
//! scope cannot see is [`EinvoiceError::CompanyNotFound`]. A write then holds
//! the company's row lock inside an audited mutation transaction, so an
//! archived company is [`EinvoiceError::CompanyArchived`].
mod agent_operations;
mod authority;
mod certificate;
mod clearance;
mod code_lists;
mod configuration;
mod consolidation;
mod contributions;
mod document;
mod documents;
mod draft;
mod evidence;
mod fiscal;
mod list;
pub mod myinvois;
mod pdf;
mod posting;
mod web;
mod workers;

pub use authority::{
    Answer, Authority, AuthorityStatus, CancelOutcome, CancelRefusal, CertificateIdentity,
    CertificateSubject, Connection, ConsolidationWindow, Credentials, DocumentStatus,
    DocumentVersion, FormatKind, LoginOutcome, Lookup, Original, Payload, PayloadError,
    PayloadOptions, PostingAccount, PostingRule, Progress, Search, Side, StatusRead, SubmitOutcome,
    Supplier, SupplierField, ValidationContext,
};
pub use certificate::{Certificate, CertificateError, PrivateKey};
pub use clearance::{POLL_WORKER, SUBMIT_WORKER};
pub use code_lists::Codes;
pub use consolidation::{
    Candidates, ConsolidatedLine, ConsolidatedRequest, ConsolidationRequest, RefusedCandidate,
};
pub use contributions::{CANCEL, CAPABILITIES, ISSUE, SETTINGS, SUBMIT, VIEW};
pub use document::{AddressSnapshot, Details, Document, DocumentLine, PartySnapshot};
pub use documents::{
    Attention, ConsolidatedSource, DocumentSummary, DocumentView, Reconciliation,
    ReconciliationRow, SourceLink, StatusEvent,
};
pub use draft::{
    Draft, DraftAddress, DraftLine, DraftParty, DraftSource, ValidAddress, ValidDraft, ValidLine,
    ValidParty,
};
pub use evidence::EvidencePage;
pub use list::{DocumentPage, DocumentQuery};
pub use myinvois::signer::SigningKey;
pub use pdf::{qr_code_svg, PdfArtifact, PdfFile, ARTIFACT_OWNER};

use bilibi_artifacts::Artifacts;
use bilibi_company::WriteError;
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction};
use bilibi_finance_ledger::Ledger;
use bilibi_integration::Integration;
use bilibi_module_registry::{Layer, Module, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_queue::Queue;
use bilibi_settings::{Resolved, Settings, Target};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, Timestamp};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_postgres::GenericClient;

/// The descriptor id.
pub const MODULE: &str = "finance/einvoice";
/// The registry service id of the [`Einvoice`] facade.
pub const SERVICE: &str = "finance/einvoice.einvoice";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &[
            "base/agent_api", "base/artifacts", "base/audit", "base/authz", "base/database", "base/integration",
            "base/datetime", "base/locale", "base/menu", "base/ui",
            "base/party_ids", "base/principal_directory", "base/queue", "base/settings", "base/tenancy", "core/company",
            "finance/ledger",
        ],
        migrations: &[
            ("finance-einvoice-001-documents", include_str!("../migrations/001-documents.sql")),
            ("finance-einvoice-002-consolidation", include_str!("../migrations/002-consolidation.sql")),
            ("finance-einvoice-003-source-links", include_str!("../migrations/003-source-links.sql")),
        ],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let owner = Arc::new(Einvoice {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                mutations: services.get("base/database.mutations")?,
                settings: services.get("base/settings.store")?,
                integration: services.get("base/integration.exchanges")?,
                queue: services.get("base/queue.store")?,
                ledger: services.get(bilibi_finance_ledger::SERVICE)?,
                artifacts: services.get("base/artifacts.store")?,
                parties: services.get("base/tenancy.parties")?,
                });
                agent_operations::bind(owner.clone())?;
                Ok(owner)
            } },
            Service { id: web::PAGES, factory: |services| Ok(Arc::new(
                bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                    einvoice: services.get(SERVICE)?,
                    contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                    directory: services.get(bilibi_finance_ledger::PEOPLE)?,
                }))
            )) },
            Service { id: web::FILES, factory: |services| Ok(Arc::new(
                bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(web::Downloads {
                    einvoice: services.get(SERVICE)?,
                }))
            )) },
            Service { id: workers::SUBMIT_SERVICE, factory: |services| workers::handler(services, workers::Job::Submit) },
            Service { id: workers::POLL_SERVICE, factory: |services| workers::handler(services, workers::Job::Poll) },
            Service { id: web::control::SERVICE, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::control::Pages {
                einvoice: services.get(SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
            })))) },
        ],
    }
}

pub type Result<T> = std::result::Result<T, EinvoiceError>;

/// Why a call did not happen.
#[derive(Debug, thiserror::Error)]
pub enum EinvoiceError {
    /// A system scope, a missing grant, or a sibling company without
    /// tenant-wide reach.
    #[error("Access denied")]
    Unauthorized,
    /// A company the scope cannot see: missing, deleted or another tenant's.
    #[error("Not found")]
    CompanyNotFound,
    #[error("This company is archived and read-only")]
    CompanyArchived,
    /// A list the authority does not publish.
    #[error("Unknown code list")]
    UnknownList,
    /// Refused values, in the words a form shows beside each field.
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    /// No document of the company has that ID.
    #[error("Document not found")]
    DocumentNotFound,
    /// The company already issued that code number in the environment.
    #[error("The company already issued that number")]
    CodeNumberTaken,
    /// The document's status is not one a submission starts from.
    #[error("The document cannot be submitted in its status")]
    NotSubmittable,
    /// Another document that stands is already the tax document of a source
    /// of this one.
    #[error("Another document already stands for its source")]
    SourceTaken,
    /// The company has no client ID or secret for the document's environment.
    #[error("The company has no credentials for the environment")]
    MissingCredentials,
    /// The format version is signed and the certificate and key are missing,
    /// unreadable or do not belong together.
    #[error("A signing certificate and key are required")]
    SigningKeyRequired,
    /// A submit job for the document is already waiting or running.
    #[error("The document is already queued for submission")]
    AlreadyQueued,
    /// The authority has not given the document a UUID yet.
    #[error("The document has not been submitted")]
    NotSubmitted,
    /// Only a valid (or rejected) document with a UUID can be cancelled.
    #[error("The document cannot be cancelled in its status")]
    NotCancellable,
    /// The authority's cancellation window has ended.
    #[error("The cancellation period is over")]
    CancellationPeriodOver,
    /// A note the authority holds references the document.
    #[error("A note the authority holds references the document")]
    ActiveReferencingDocuments,
    /// A cancellation reason must be 1 to 300 characters.
    #[error("A reason of 1 to 300 characters is required")]
    InvalidReason,
    /// The authority refused the cancellation and said why.
    #[error("The authority refused the cancellation")]
    CancellationRefused {
        code: CancelRefusal,
        reason: Option<String>,
    },
    #[error("The authority is rate limiting requests")]
    RateLimited,
    #[error("The authority is unavailable")]
    AuthorityUnavailable,
    /// The authority could not answer a status read: `rate_limited`,
    /// `authority_unavailable`, `not_yet_available` or `status_unreadable`.
    #[error("The authority could not answer ({0})")]
    Authority(&'static str),
    /// A retry asked of a document that is neither valid nor rejected.
    #[error("The document is not validated")]
    NotPostable,
    /// A reversal retry asked of a document that is not cancelled.
    #[error("The document is not cancelled")]
    NotReversible,
    /// The ledger follow-up was tried and failed again; the reason is also
    /// recorded as the document's `posting_failed` or `reversal_failed` event.
    #[error("The ledger follow-up failed: {0}")]
    LedgerFollowUpFailed(String),
    /// The consolidated month has not ended yet.
    #[error("The month has not ended")]
    PeriodNotEnded,
    /// The authority's time for issuing the month's consolidated document is over.
    #[error("The consolidation window is closed")]
    ConsolidationWindowClosed,
    /// The month has no document a consolidated document may cover.
    #[error("Nothing to consolidate")]
    NothingToConsolidate,
    /// A named document a consolidated document may not cover.
    #[error("Document {0} cannot be consolidated")]
    NotConsolidatable(i64),
    /// Sources linked to another module's entries and sources without links.
    #[error("Linked and unlinked documents do not consolidate together")]
    MixedSourceLinks,
    /// An option of another shape: an empty or non-positive list of IDs, a
    /// limit or cursor out of range, an unknown evidence dataset.
    #[error("Invalid option")]
    InvalidOption,
    /// Base Artifacts could not render, store or read the PDF; its stable
    /// code (`policy` before storage is configured, `not_found`, ...).
    #[error("The PDF is unavailable ({0})")]
    Artifacts(&'static str),
    /// A database or capture failure; never a refusal.
    #[error(transparent)]
    Failed(#[from] Error),
}

/// One refusal of a submitted value, at the field's path (`code`,
/// `buyer.address.state_code`, `lines[2].tax_rate`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}
impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// A company's override of one code of the authority's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CodeOverride {
    pub list: String,
    pub code: String,
    pub description: Option<String>,
    pub active: bool,
}
/// What changes on a code override: `description: Some(Some(text))` renames
/// the code (or names one the shipped list lacks), `Some(None)` removes the
/// company's name for it, `active: Some(false)` retires it. A field left
/// `None` is kept; a new override is active unless said otherwise.
#[derive(Clone, Debug, Default)]
pub struct CodeOverrideChange {
    pub description: Option<Option<String>>,
    pub active: Option<bool>,
}

/// The ledger account codes a validated document posts to, by the company's
/// own chart; a blank one is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PostingAccounts {
    pub receivable_account: Option<String>,
    pub revenue_account: Option<String>,
    pub tax_account: Option<String>,
    pub settlement_account: Option<String>,
    pub payable_account: Option<String>,
    pub expense_account: Option<String>,
    pub purchase_tax_account: Option<String>,
}
impl PostingAccounts {
    /// The company's account code for `account`.
    pub fn get(&self, account: PostingAccount) -> Option<&str> {
        match account {
            PostingAccount::Receivable => &self.receivable_account,
            PostingAccount::Revenue => &self.revenue_account,
            PostingAccount::Tax => &self.tax_account,
            PostingAccount::Settlement => &self.settlement_account,
            PostingAccount::Payable => &self.payable_account,
            PostingAccount::Expense => &self.expense_account,
            PostingAccount::PurchaseTax => &self.purchase_tax_account,
        }
        .as_deref()
    }
}

/// What a company has configured, without any secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Setup {
    /// `sandbox` or `production`.
    pub environment: String,
    /// The authority's format version documents are submitted in.
    pub document_version: String,
    pub posting_accounts: PostingAccounts,
    /// Whether the current environment has a client ID.
    pub client_id_set: bool,
    /// Whether the current environment has a client secret.
    pub client_secret_set: bool,
    /// When the current environment's client secret expires, as `YYYY-MM-DD`.
    pub client_secret_expires_on: Option<String>,
    /// Whole days from today (the database's UTC date) until it expires;
    /// negative once expired.
    pub client_secret_days_left: Option<i64>,
    pub certificate_set: bool,
    pub private_key_set: bool,
    /// The stored certificate's facts, never its bytes.
    pub certificate: CertificateSetup,
    /// Warn this many days before the certificate or a client secret expires.
    pub expiry_alert_days: i64,
    /// What expires within the alert window, or already has: `certificate`
    /// and `client_secret`.
    pub expiring: Vec<&'static str>,
    /// Whether the supplier profile's tax ID is the one the certificate's
    /// subject carries.
    pub tax_id_matches_certificate: bool,
    /// The supplier profile, with the company's name for a blank `name` and
    /// `BRN` for a blank registration type.
    pub supplier: Supplier,
    /// The supplier fields every document carries that the profile leaves blank.
    pub supplier_missing: Vec<&'static str>,
}

/// The stored signing certificate as the setup reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum CertificateSetup {
    /// No certificate is stored.
    Missing,
    /// One is stored but cannot be read now: it does not parse, or the
    /// settings key is unavailable.
    Unreadable,
    Facts(CertificateFacts),
}

/// What the setup page shows of the signing certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CertificateFacts {
    pub issuer_name: String,
    pub subject_name: String,
    pub serial_number: String,
    /// The tax ID the certificate's subject carries, as the authority writes it.
    pub tax_id: Option<String>,
    pub registration_number: Option<String>,
    pub not_before: Timestamp,
    pub not_after: Timestamp,
    /// Whole days until it expires; negative once expired.
    pub days_until_expiry: i64,
    /// Whether the stored private key belongs to it.
    pub private_key_matches: bool,
}

/// The facade, resolved from the registry as [`SERVICE`].
pub struct Einvoice {
    db: Db,
    mutations: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    integration: Arc<Integration>,
    queue: Arc<Queue>,
    ledger: Arc<Ledger>,
    artifacts: Arc<Artifacts>,
    parties: Arc<bilibi_tenancy::PartyProofs>,
}

impl Einvoice {
    /// The authority every company's documents are cleared with.
    pub fn authority() -> impl Authority {
        myinvois::MyInvois
    }

    /// The lists the authority publishes, sorted.
    pub fn code_lists() -> Vec<&'static str> {
        code_lists::lists(myinvois::ID)
    }

    /// The company's active codes in one of the authority's lists, code to
    /// description (`finance.einvoice.view`): the shipped list with the
    /// company's overrides applied. `UnknownList` for a list the authority
    /// does not publish.
    pub async fn list_codes(&self, scope: &Scope, company_id: i64, list: &str) -> Result<Codes> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let authority = Self::authority().id();
        if !code_lists::lists(authority).contains(&list) {
            return Err(EinvoiceError::UnknownList);
        }
        Ok(code_lists::codes(&**client, scope.tenant(), company_id, authority, list).await?)
    }

    /// Overrides one code for the company (`finance.einvoice.settings.manage`):
    /// `description` renames it, or names a code the shipped list lacks, and
    /// `active: Some(false)` retires it. `UnknownList` for a list the
    /// authority does not publish; refused values are `Invalid`. The audit
    /// action is `finance.einvoice.code.overridden`.
    pub async fn override_code(
        &self,
        scope: &Scope,
        company_id: i64,
        list: &str,
        code: &str,
        change: &CodeOverrideChange,
    ) -> Result<CodeOverride> {
        let authority = Self::authority().id();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                SETTINGS,
                "finance.einvoice.code.overridden",
            )
            .await?;
        if !code_lists::lists(authority).contains(&list) {
            return Err(EinvoiceError::UnknownList);
        }
        let tenant = scope.tenant();
        let valid =
            code_lists::validate(tx.sql(), tenant, company_id, authority, list, code, change)
                .await?
                .map_err(EinvoiceError::Invalid)?;
        let next = valid.next.clone();
        if valid.current.as_ref() != Some(&valid.next) {
            let current = valid.current;
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let id = code_lists::upsert(sql, tenant, company_id, authority, &next).await?;
                    let change = Change {
                        kind: "finance.einvoice.code_override".into(),
                        id,
                        old: current.as_ref().map_or(Value::Null, code_lists::to_json),
                        new: code_lists::to_json(&next),
                    };
                    Ok(((), vec![change]))
                })
            })
            .await?;
        }
        tx.commit().await?;
        Ok(valid.next)
    }

    /// What the company has configured, never a secret
    /// (`finance.einvoice.settings.manage`): the environment, which
    /// credentials are present, the client secret's expiry, the supplier
    /// profile and what it still lacks.
    pub async fn setup(&self, scope: &Scope, company_id: i64) -> Result<Setup> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, SETTINGS).await?;
        let name = bilibi_company::identity(&**client, scope, company_id)
            .await?
            .ok_or(EinvoiceError::CompanyNotFound)?
            .display_name;
        let target = Target::Company(
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?,
        );
        let environment = text(
            &self
                .settings
                .get(&**client, scope, target, "finance.einvoice.environment")
                .await?
                .value,
        )
        .unwrap_or_else(|| "sandbox".into());
        let prefix = format!("finance.einvoice.{environment}");
        let (client_id, client_secret, expires) = (
            format!("{prefix}.client_id"),
            format!("{prefix}.client_secret"),
            format!("{prefix}.secret_expires_on"),
        );
        let posting: Vec<String> = PostingAccount::ALL
            .iter()
            .map(|account| format!("finance.einvoice.posting.{}", account.field()))
            .collect();
        let supplier: Vec<String> = configuration::SUPPLIER_FIELDS
            .iter()
            .map(|field| configuration::supplier_key(field))
            .collect();
        let mut keys: Vec<&str> = vec![
            "finance.einvoice.document_version",
            "finance.einvoice.expiry_alert_days",
            &client_id,
            &expires,
        ];
        keys.extend(posting.iter().map(String::as_str));
        keys.extend(supplier.iter().map(String::as_str));
        let values = self
            .settings
            .resolve_many(&**client, scope, target, &keys)
            .await?;
        // Secrets are read masked: a stored one comes back `null` with its
        // source, which is all the setup needs to know.
        let masked = self
            .settings
            .resolve_masked(
                &**client,
                scope,
                target,
                &[
                    &client_secret,
                    "finance.einvoice.certificate_pem",
                    "finance.einvoice.private_key_pem",
                ],
            )
            .await?;
        let value = |key: &str| values.get(key).and_then(|resolved| text(&resolved.value));
        let is_set = |key: &str| {
            masked
                .get(key)
                .is_some_and(|r: &Resolved| r.source.is_some())
        };

        let expires_on = value(&expires).filter(|date| date.parse::<Date>().is_ok());
        let days_left = match &expires_on {
            Some(date) => Some(days_until(&**client, date).await?),
            None => None,
        };
        let now = Timestamp::now();
        let certificate_key = "finance.einvoice.certificate_pem";
        let private_key_key = "finance.einvoice.private_key_pem";
        // The PEMs are read here, inside the facade, and only their facts
        // leave it.
        let certificate = if !is_set(certificate_key) {
            CertificateSetup::Missing
        } else if !self.settings.encryption_available() {
            CertificateSetup::Unreadable
        } else {
            let pem = self
                .settings
                .get(&**client, scope, target, certificate_key)
                .await?;
            match text(&pem.value).as_deref().map(Certificate::parse) {
                Some(Ok(certificate)) => {
                    let key = if is_set(private_key_key) {
                        let pem = self
                            .settings
                            .get(&**client, scope, target, private_key_key)
                            .await?;
                        text(&pem.value).and_then(|pem| PrivateKey::parse(&pem).ok())
                    } else {
                        None
                    };
                    let identity = Self::authority().certificate_identity(&certificate.subject);
                    CertificateSetup::Facts(CertificateFacts {
                        issuer_name: certificate.issuer_name.clone(),
                        subject_name: certificate.subject_name.clone(),
                        serial_number: certificate.serial_number.clone(),
                        tax_id: identity.tax_id,
                        registration_number: identity.registration_number,
                        not_before: certificate.not_before,
                        not_after: certificate.not_after,
                        days_until_expiry: certificate.days_until_expiry(now),
                        private_key_matches: key.is_some_and(|key| certificate.key_matches(&key)),
                    })
                }
                _ => CertificateSetup::Unreadable,
            }
        };
        let profile = supplier_profile(&values, name);
        let posting_value = |account: PostingAccount| {
            value(&format!("finance.einvoice.posting.{}", account.field()))
        };
        let expiry_alert_days = values
            .get("finance.einvoice.expiry_alert_days")
            .and_then(|resolved| resolved.value.as_i64())
            .unwrap_or(30);
        let facts = match &certificate {
            CertificateSetup::Facts(facts) => Some(facts),
            _ => None,
        };
        let mut expiring = Vec::new();
        if facts.is_some_and(|facts| facts.days_until_expiry <= expiry_alert_days) {
            expiring.push("certificate");
        }
        if days_left.is_some_and(|days| days <= expiry_alert_days) {
            expiring.push("client_secret");
        }
        let tax_id_matches_certificate =
            profile.tax_id.is_some() && facts.is_some_and(|facts| facts.tax_id == profile.tax_id);
        Ok(Setup {
            environment,
            document_version: value("finance.einvoice.document_version")
                .unwrap_or_else(|| myinvois::VERSION.into()),
            posting_accounts: PostingAccounts {
                receivable_account: posting_value(PostingAccount::Receivable),
                revenue_account: posting_value(PostingAccount::Revenue),
                tax_account: posting_value(PostingAccount::Tax),
                settlement_account: posting_value(PostingAccount::Settlement),
                payable_account: posting_value(PostingAccount::Payable),
                expense_account: posting_value(PostingAccount::Expense),
                purchase_tax_account: posting_value(PostingAccount::PurchaseTax),
            },
            client_id_set: value(&client_id).is_some(),
            client_secret_set: is_set(&client_secret),
            client_secret_expires_on: expires_on,
            client_secret_days_left: days_left,
            certificate_set: is_set(certificate_key),
            private_key_set: is_set(private_key_key),
            certificate,
            expiry_alert_days,
            expiring,
            tax_id_matches_certificate,
            supplier_missing: Self::authority()
                .supplier_missing(&profile)
                .into_iter()
                .map(SupplierField::field)
                .collect(),
            supplier: profile,
        })
    }

    /// Every write: the capability and company reach now, then the audited
    /// transaction holding the company's row lock.
    pub(crate) async fn write<'c>(
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
            .map_err(company_write)?;
        Ok(tx)
    }
}

/// Core Company's writable-company refusal in this module's terms.
pub(crate) fn company_write(error: WriteError) -> EinvoiceError {
    match error {
        WriteError::NotFound => EinvoiceError::CompanyNotFound,
        WriteError::Archived => EinvoiceError::CompanyArchived,
        WriteError::Access(error) => refusal(error),
        WriteError::InUse | WriteError::Stale | WriteError::Invalid(_) => {
            EinvoiceError::Failed(Error::Internal)
        }
    }
}

/// The company's supplier profile from its resolved `finance.einvoice.supplier.*`
/// values, with the company's name for a blank `name` and `BRN` for a blank
/// registration type.
pub(crate) fn supplier_profile(values: &BTreeMap<String, Resolved>, name: String) -> Supplier {
    let value = |field: &str| {
        values
            .get(&configuration::supplier_key(field))
            .and_then(|resolved| text(&resolved.value))
    };
    let mut profile = Supplier {
        name: value("name"),
        tax_id: value("tax_id"),
        registration_type: value("registration_type"),
        registration_number: value("registration_number"),
        sales_tax_registration: value("sales_tax_registration"),
        tourism_tax_registration: value("tourism_tax_registration"),
        industry_code: value("industry_code"),
        business_activity: value("business_activity"),
        phone: value("phone"),
        email: value("email"),
        address_line1: value("address_line1"),
        address_line2: value("address_line2"),
        address_line3: value("address_line3"),
        city: value("city"),
        postcode: value("postcode"),
        state_code: value("state_code"),
        country_code: value("country_code"),
    };
    profile.name.get_or_insert(name);
    profile
        .registration_type
        .get_or_insert_with(|| "BRN".into());
    profile
}

/// A trimmed, non-blank string value.
pub(crate) fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Whole days from the database's UTC date until `date` (`YYYY-MM-DD`).
async fn days_until(client: &(impl GenericClient + Sync), date: &str) -> Result<i64> {
    let row = client
        .query_one(
            "SELECT ($1::text::date - (now() AT TIME ZONE 'UTC')::date)::bigint",
            &[&date],
        )
        .await
        .map_err(Error::from)?;
    Ok(row.get(0))
}

/// The actor's current grant and company reach decide every call.
pub(crate) async fn authorize(
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

pub(crate) fn refusal(error: Error) -> EinvoiceError {
    match error {
        Error::Forbidden | Error::Unauthorized => EinvoiceError::Unauthorized,
        Error::NotFound => EinvoiceError::CompanyNotFound,
        other => EinvoiceError::Failed(other),
    }
}
