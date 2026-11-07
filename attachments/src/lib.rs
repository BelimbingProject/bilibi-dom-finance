//! Source documents, module `finance/attachments` (Bilimbi
//! `Bilimbi.Finance.Attachments`): the supplier's invoice scan behind a bill,
//! a remittance behind a payment, a contract behind an invoice, a voucher
//! behind a manual journal, the bank's own file behind an imported statement.
//!
//! Every function takes the sealed [`Scope`] it was handed and an explicit
//! company id. Reads authorize `finance.attachments.view` and writes their
//! own capability (`finance.attachments.create` to attach or replace,
//! `finance.attachments.delete` to remove, and
//! `finance.attachments.retention.manage` to lengthen the company's retention
//! or an attachment's, or to place and release a legal hold) for the scope's
//! actor and the target company at the moment they run, through Core
//! Company's `authorize_work_target`: a missing grant, or a sibling company
//! without tenant-wide reach, is [`AttachmentError::Unauthorized`], and a
//! company the scope cannot see is [`AttachmentError::CompanyNotFound`]. A
//! write then holds the company's row lock for its mutation transaction, so
//! an archived company is [`AttachmentError::CompanyArchived`].
//!
//! The record an attachment supports is described by a [`Source`] the module
//! that owns it contributes (see `source.rs`), which reads the record with the
//! same scope: an attachment also needs whatever that source asks to read the
//! record, and answers the source's refusal when it is not readable. Reading
//! an attachment's metadata ([`Attachments::get_attachment`], the lists)
//! needs all of the source's `view_capabilities` as well as
//! `finance.attachments.view`: a list leaves out the attachments of every
//! source the caller cannot read, and a single read or a retention change of
//! one is [`AttachmentError::Unauthorized`].
//!
//! The file's bytes are a records-grade document in Base Artifacts: stored
//! once, hashed, and retained until the company's retention years
//! ([`Attachments::get_retention`]; seven until the company lengthens it) have
//! passed after the end of the financial year the record's date falls in.
//! Base refuses to delete the bytes before then, and a legal hold refuses it
//! after. A change of retention ([`Attachments::extend_retention`],
//! [`Attachments::place_hold`], [`Attachments::release_hold`]) is made in Base
//! Artifacts first, after a short transaction under the company lock has
//! checked it, and its event follows without the lock; Base is the fact, so
//! [`Attachments::get_attachment`] and [`Attachments::read_attachment`] append
//! any retention event Base holds and this module does not before answering,
//! and an event that could not be recorded after Base applied the change is
//! [`AttachmentError::RecordedInStorage`].
//!
//! An attachment row is the link from the record to that artifact with the
//! file's name, type, size, SHA-256, retain-until, and who captured it when;
//! it is never changed. Its standing is its latest event: a draft record's
//! attachment can be replaced ([`Attachments::replace`], which captures the
//! new file and records `replaced` on the old) or removed
//! ([`Attachments::remove`], an event with its reason), and once the record is
//! final (posted, issued, approved, imported) its attachments are kept as
//! they are, though one can still be added. A replaced or removed
//! attachment's bytes stay retained and readable from its history; nothing
//! here deletes them.
//!
//! Base Artifacts stores a file outside any transaction, so
//! [`Attachments::attach`] stores the bytes first and records the link in its
//! own transaction; a record inserted in the same transaction as its file (a
//! bank statement) uses [`Attachments::capture_file`] before the transaction
//! and [`Attachments::attach_capture`] inside it. A storage refusal
//! ([`AttachmentError::RetentionNotConfigured`] until an operator sets Base
//! Artifacts' storage root and retention, [`AttachmentError::InvalidSize`]
//! over its limit) stores nothing.
//!
//! Results are plain records; the schema, SQL and validation stay private.
mod contributions;
mod evidence;
mod files;
mod operations;
mod owner;
mod refresh;
mod retain;
mod retention;
mod source;
mod store;
mod web;

pub use contributions::{CAPABILITIES, CREATE, DELETE, MANAGE_RETENTION, REFRESH_PRINCIPAL, VIEW};
pub use evidence::EvidencePage;
pub use files::{NewFile, MAX_FILE_BYTES};
pub use owner::OWNER_ID as ARTIFACT_OWNER;
pub use retention::DEFAULT_YEARS;
pub use source::{
    Source, SourceClient, SourceDefinition, SourceDocument, SourceFuture, SourceRefusal, SOURCES,
};
pub use web::EMBED as PANEL_EMBED;

use bilibi_artifacts::{Artifacts, Error as StorageError};
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_module_registry::{Catalog, Layer, Module, Service};
use bilibi_queue::{HandlerService, Queue};
use bilibi_tenancy::{Authentication, Scope};
use bilibi_ui::{download::DownloadService, page::PageService};
use jiff::{civil::Date, Timestamp};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// The descriptor id.
pub const MODULE: &str = "finance/attachments";
/// The registry service id of the [`Attachments`] facade.
pub const SERVICE: &str = "finance/attachments.attachments";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &[
            "base/artifacts", "base/authz", "base/database", "base/datetime", "base/locale",
            "base/menu", "base/queue", "base/tenancy", "base/ui", "core/company", "finance/ledger",
        ],
        migrations: &[("finance-attachments-001-attachments", include_str!("../migrations/001-attachments.sql"))],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
                Ok(Arc::new(Attachments {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    mutations: services.get("base/database.mutations")?,
                    artifacts: services.get("base/artifacts.store")?,
                    queue: services.get("base/queue.store")?,
                    authentication: services.get("base/tenancy.authentication")?,
                    sources: Sources::from_catalog(&catalog)?,
                }))
            } },
            Service { id: web::PAGES, factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(web::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    attachments: services.get(SERVICE)?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: web::FILES, factory: |services| {
                Ok(Arc::new(DownloadService::<Scope, Error>(Arc::new(web::Downloads {
                    attachments: services.get(SERVICE)?,
                }))))
            } },
            Service { id: web::PANELS, factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(web::Panels {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    attachments: services.get(SERVICE)?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: refresh::REFRESH_SERVICE, factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(refresh::RefreshJob {
                    attachments: services.get(SERVICE)?,
                }))))
            } },
        ],
    }
}

pub type Result<T> = std::result::Result<T, AttachmentError>;

/// Why a call did not happen (Bilimbi's refusal atoms and changeset errors).
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    /// A missing grant, or a sibling company without tenant-wide reach.
    #[error("Access denied")]
    Unauthorized,
    /// A company the scope cannot see: missing, deleted or another tenant's.
    #[error("Not found")]
    CompanyNotFound,
    #[error("This company is archived and read-only")]
    CompanyArchived,
    #[error("Not found")]
    AttachmentNotFound,
    /// The record the source describes does not exist (or is not readable as
    /// that source's own refusal says).
    #[error("Not found")]
    NotFound,
    /// No mounted module contributes the source an attachment names.
    #[error("The source of this document is not available")]
    SourceUnavailable,
    /// A source id no mounted module contributes.
    #[error("Invalid source")]
    InvalidSource,
    #[error("The file needs a name of at most 255 characters")]
    InvalidFileName,
    #[error("The file is empty")]
    EmptyFile,
    #[error("The file is larger than 25 MiB")]
    FileTooLarge,
    #[error("The content type is not valid")]
    InvalidContentType,
    /// A capture made for another source or another record date.
    #[error("The stored file does not match this record")]
    CaptureMismatch,
    /// A final record keeps its attachments as they are.
    #[error("This record is final: its attachments are kept as they are")]
    DocumentFinal,
    #[error("This record is not final yet")]
    DocumentNotFinal,
    #[error("This attachment has been replaced or removed")]
    AttachmentNotStanding,
    #[error("Retention can only be lengthened")]
    RetentionNotLengthened,
    #[error("The retention date must be later than the current one")]
    RetentionNotExtended,
    /// Base Artifacts refuses a retain-until in the past or over 200 years away.
    #[error("The retention date is out of range")]
    RetentionOutOfRange,
    #[error("The attachment is already on legal hold")]
    AlreadyOnHold,
    #[error("The attachment is not on legal hold")]
    NotOnHold,
    #[error("A reason is required")]
    ReasonRequired,
    /// An option the call does not accept: a limit, cursor, status or id out
    /// of range, or a dataset the module does not export.
    #[error("Invalid option")]
    InvalidOption,
    /// An operator has not set Base Artifacts' storage root and retention.
    #[error(
        "Ask a server administrator to set Storage folder (artifacts.storage_root) and Document retention days (artifacts.retention_days) in Global settings: /system/settings"
    )]
    RetentionNotConfigured,
    /// The folder an administrator chose for Base Artifacts does not exist;
    /// the text is Base's own, naming the folder.
    #[error("{0} Global settings: /system/settings")]
    StorageFolderMissing(String),
    /// The Storage folder is empty or not an absolute, private folder.
    #[error(
        "Storage folder (artifacts.storage_root) must be the absolute path of an existing private folder (permission 0700). Ask a server administrator to set it in Global settings: /system/settings"
    )]
    StorageFolderUnsafe,
    /// The file is larger than Base Artifacts' `artifacts.max_bytes`.
    #[error("The file is larger than the storage limit")]
    InvalidSize,
    /// Base Artifacts refused with this stable code.
    #[error("Storage refused the request ({0})")]
    Storage(&'static str),
    /// Base applied the change and this module could not record its event; the
    /// next read of the attachment records it.
    #[error("The change was applied in storage but not recorded: {0}")]
    RecordedInStorage(String),
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
}
impl FieldError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl From<SourceRefusal> for AttachmentError {
    fn from(refusal: SourceRefusal) -> Self {
        match refusal {
            SourceRefusal::NotFound => Self::NotFound,
            SourceRefusal::Unauthorized => Self::Unauthorized,
            SourceRefusal::Failed(error) => refusal_of(error),
        }
    }
}
impl From<StorageError> for AttachmentError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Policy => Self::RetentionNotConfigured,
            StorageError::Invalid => Self::InvalidSize,
            StorageError::InvalidRetention => Self::RetentionOutOfRange,
            StorageError::RetentionNotExtended => Self::RetentionNotExtended,
            StorageError::AlreadyOnHold => Self::AlreadyOnHold,
            StorageError::NotOnHold => Self::NotOnHold,
            StorageError::UnsafeRoot => Self::StorageFolderUnsafe,
            StorageError::MissingRoot(_) => Self::StorageFolderMissing(error.to_string()),
            StorageError::Database(error) => refusal_of(error),
            other => Self::Storage(other.code()),
        }
    }
}

pub(crate) fn refusal_of(error: Error) -> AttachmentError {
    match error {
        Error::Forbidden | Error::Unauthorized => AttachmentError::Unauthorized,
        Error::NotFound => AttachmentError::CompanyNotFound,
        other => AttachmentError::Failed(other),
    }
}

/// The statuses an attachment moves through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Standing,
    Replaced,
    Removed,
}
impl Status {
    pub const ALL: [Status; 3] = [Self::Standing, Self::Replaced, Self::Removed];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::Replaced => "replaced",
            Self::Removed => "removed",
        }
    }
}

/// What happened to an attachment after its capture: `replaced` (naming the
/// attachment that took its place), `removed`, `retention_extended` (with the
/// new retain-until), `hold_placed` or `hold_released`, each with the actor,
/// any impersonator or system principal, and the database's time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttachmentEvent {
    pub id: i64,
    pub event: String,
    pub replacement_id: Option<i64>,
    #[serde(serialize_with = "as_text_option")]
    pub retain_until: Option<Timestamp>,
    pub reason: Option<String>,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub actor_system_principal: Option<String>,
    #[serde(serialize_with = "as_text")]
    pub occurred_at: Timestamp,
}

/// One attachment: the record it supports, the file, its retention
/// (`retain_until` follows the latest extension), who captured it when, its
/// status and its events oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Attachment {
    pub id: i64,
    pub attachment_number: i64,
    pub source: String,
    /// The descriptor id of the module that contributed the source.
    pub source_module: String,
    /// The kind of record, as the source names it; `None` once unmounted.
    pub source_label: Option<String>,
    pub document_id: i64,
    pub document_label: String,
    #[serde(serialize_with = "as_text")]
    pub document_date: Date,
    pub artifact_id: String,
    pub file_name: String,
    pub content_type: String,
    pub byte_size: i64,
    pub sha256: String,
    pub retention_years: i32,
    #[serde(serialize_with = "as_text")]
    pub retain_until: Timestamp,
    pub captured_by_user_id: i64,
    pub captured_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub captured_at: Timestamp,
    pub status: Status,
    pub on_hold: bool,
    pub events: Vec<AttachmentEvent>,
}
impl Attachment {
    /// Derives what the events decide: the standing, the retain-until in
    /// force and whether a hold stands.
    fn settle(&mut self) {
        self.status = self
            .events
            .iter()
            .find_map(|e| match e.event.as_str() {
                "replaced" => Some(Status::Replaced),
                "removed" => Some(Status::Removed),
                _ => None,
            })
            .unwrap_or(Status::Standing);
        for until in self
            .events
            .iter()
            .filter(|e| e.event == "retention_extended")
            .filter_map(|e| e.retain_until)
        {
            if until > self.retain_until {
                self.retain_until = until;
            }
        }
        self.on_hold = self
            .events
            .iter()
            .rfind(|e| e.event == "hold_placed" || e.event == "hold_released")
            .is_some_and(|e| e.event == "hold_placed");
    }
    pub(crate) fn json(&self) -> serde_json::Value {
        json!({
            "attachment_number": self.attachment_number, "source": self.source, "source_module": self.source_module,
            "document_id": self.document_id, "document_label": self.document_label,
            "document_date": self.document_date.to_string(), "artifact_id": self.artifact_id,
            "file_name": self.file_name, "content_type": self.content_type, "byte_size": self.byte_size,
            "sha256": self.sha256, "retention_years": self.retention_years,
            "retain_until": self.retain_until.to_string(),
            "captured_by_user_id": self.captured_by_user_id,
            "captured_by_impersonator_id": self.captured_by_impersonator_id,
        })
    }
}

/// One change of a company's retention setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetentionChange {
    pub id: i64,
    pub change_number: i64,
    pub years: i32,
    pub reason: String,
    pub changed_by_user_id: i64,
    pub changed_by_impersonator_id: Option<i64>,
    #[serde(serialize_with = "as_text")]
    pub changed_at: Timestamp,
}
/// The company's retention: the years in force and the latest change
/// ([`Attachments::list_retention_changes`] shows them all), or none for a
/// company that keeps the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Retention {
    pub years: i32,
    pub change: Option<RetentionChange>,
}

/// A file stored by [`Attachments::capture_file`], waiting for
/// [`Attachments::attach_capture`]. A capture whose record is never created
/// stays a retained, unlinked document, so check what can be checked first.
#[derive(Clone, Debug)]
pub struct Capture {
    pub(crate) source: String,
    pub(crate) document_date: Date,
    pub(crate) artifact_id: String,
    pub(crate) file_name: String,
    pub(crate) content_type: String,
    pub(crate) byte_size: i64,
    pub(crate) sha256: String,
    pub(crate) retention_years: i32,
    pub(crate) retain_until: Timestamp,
}

/// An attachment's bytes, for a download. Intentionally has no `Debug`.
pub struct AttachmentFile {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub content_type: String,
    pub byte_size: i64,
    pub sha256: String,
}

/// Options of [`Attachments::list_company_attachments`].
#[derive(Clone, Debug, Default)]
pub struct AttachmentQuery {
    /// A source id (`finance/payables/bill`).
    pub source: Option<String>,
    /// Only the attachments of these records of `source`.
    pub document_ids: Option<Vec<i64>>,
    /// One of [`Status::ALL`] or `all`; `standing` by default.
    pub status: Option<String>,
    /// 1 to 500, 100 when absent.
    pub limit: Option<i64>,
    /// The last `attachment_number` of the previous page.
    pub after: Option<i64>,
}

/// A kind of record the company has attachments for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceKind {
    pub source: String,
    pub source_label: Option<String>,
}

/// What [`Attachments::refresh_retention`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// A retention refresh job was queued with the caller's transaction.
    Queued,
    /// No standing attachment is kept for less than the record's date says.
    None,
}

fn as_text<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
fn as_text_option<T: std::fmt::Display, S: serde::Serializer>(
    value: &Option<T>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.collect_str(value),
        None => serializer.serialize_none(),
    }
}

/// One mounted source: its definition and the module that contributed it.
pub(crate) struct Registered {
    pub(crate) id: &'static str,
    pub(crate) module: &'static str,
    pub(crate) source: &'static dyn Source,
}
pub(crate) struct Sources(BTreeMap<&'static str, Registered>);
impl Sources {
    fn from_catalog(catalog: &Catalog) -> bilibi_module_registry::Result<Self> {
        let mut sources = BTreeMap::new();
        for entry in catalog.entries::<SourceDefinition>(SOURCES)? {
            sources.insert(
                entry.id,
                Registered {
                    id: entry.id,
                    module: entry.provenance.module,
                    source: entry.get::<SourceDefinition>()?.source,
                },
            );
        }
        Ok(Self(sources))
    }
    pub(crate) fn get(&self, id: &str) -> Option<&Registered> {
        self.0.get(id)
    }
}

/// The facade, resolved from the registry as [`SERVICE`].
pub struct Attachments {
    pub(crate) db: Db,
    pub(crate) mutations: Arc<MutationBoundary>,
    pub(crate) artifacts: Arc<Artifacts>,
    pub(crate) queue: Arc<Queue>,
    pub(crate) authentication: Arc<Authentication>,
    pub(crate) sources: Sources,
}

impl Attachments {
    /// The statuses an attachment moves through.
    pub fn statuses() -> &'static [Status] {
        &Status::ALL
    }

    /// The years a company keeps source documents until it sets its own.
    pub fn default_retention_years() -> i32 {
        DEFAULT_YEARS
    }

    /// The datasets [`Attachments::evidence_page`] exports, one per table this
    /// module owns.
    pub fn evidence_datasets() -> &'static [&'static str] {
        evidence::DATASETS
    }

    /// Whether a mounted module contributes `source`.
    pub fn source_mounted(&self, source: &str) -> bool {
        self.sources.get(source).is_some()
    }

    /// The kind of record `source` describes (`"Bill"`), if mounted.
    pub fn source_label(&self, source: &str) -> Option<&'static str> {
        self.sources.get(source).map(|r| r.source.label())
    }

    /// The page of record `document_id` of `source`, if its owner has one.
    /// A link only: the page authorizes its reader again.
    pub fn record_href(&self, source: &str, document_id: i64) -> Option<String> {
        self.sources
            .get(source)
            .and_then(|r| r.source.href(document_id))
    }
}
