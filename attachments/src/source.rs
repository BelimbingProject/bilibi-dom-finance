//! The record a source document supports, as the module that owns the record
//! describes it (Bilimbi `Bilimbi.Finance.Attachments.Source`).
//!
//! A Finance module that keeps source documents contributes one [`Source`]
//! per kind of record (a bill, a payment request, an invoice, a bank
//! statement) under the id `<its module id>/<record kind>`
//! (`finance/payables/bill`), the id stored with each attachment. The
//! attachments facade asks it again on every read and write.
//!
//! `view_capabilities` are what the owning module asks to read its records:
//! every metadata read of an attachment (`get_attachment`, the lists, the
//! source documents page) also needs it and leaves out the attachments of a
//! source whose records the caller cannot read, so the attachments grant
//! alone discloses no record's label, file name or date. `document` reads the
//! record with the caller's scope on the caller's connection, so it sees the
//! caller's uncommitted writes, and answers:
//!
//! - `label`: how the record is named to a person (`"Bill 12"`), kept with
//!   the attachment for the index and the auditor's pack;
//! - `date`: the record's own date, which decides the financial year the
//!   retention counts from. A record whose date can change while it is not
//!   final (a draft invoice) makes its module call
//!   [`crate::Attachments::refresh_retention`] once the record is final,
//!   inside the transaction that finalises it, so the retention follows the
//!   final date;
//! - `is_final`: whether the record is beyond change (posted, issued,
//!   approved, imported), after which its attachments are kept as they are:
//!   one can still be added, none replaced or removed.
//!
//! The source authorizes the read itself (a missing grant is
//! [`SourceRefusal::Unauthorized`], a missing record
//! [`SourceRefusal::NotFound`]); the attachments facade never reads the
//! owner's tables.
use crate::MODULE;
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use std::{future::Future, pin::Pin};
use tokio_postgres::{Client, Transaction};

/// The catalog key sources are contributed to.
pub const SOURCES: &str = "finance/attachments.sources";

/// The caller's connection: a pool connection or an open transaction.
/// Expand it with [`on_source_client!`] to call a `&(impl GenericClient + Sync)` read.
#[derive(Clone, Copy)]
pub enum SourceClient<'a> {
    Client(&'a Client),
    Transaction(&'a Transaction<'a>),
}

/// Runs `$body` with `$c` bound to the concrete client inside a
/// [`SourceClient`]: `on_source_client!(client, |c| read(c, scope, id).await)`.
#[macro_export]
macro_rules! on_source_client {
    ($client:expr, |$c:ident| $body:expr) => {
        match $client {
            $crate::SourceClient::Client($c) => $body,
            $crate::SourceClient::Transaction($c) => $body,
        }
    };
}

/// What a source says about one record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDocument {
    pub label: String,
    pub date: Date,
    pub is_final: bool,
}

/// Why a source did not answer; reaches the caller as the attachment
/// operation's own refusal.
#[derive(Debug, thiserror::Error)]
pub enum SourceRefusal {
    #[error("Not found")]
    NotFound,
    #[error("Access denied")]
    Unauthorized,
    #[error(transparent)]
    Failed(#[from] bilibi_database::Error),
}

pub type SourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SourceDocument, SourceRefusal>> + Send + 'a>>;

/// Implemented once per kind of record by the module that owns it.
pub trait Source: Send + Sync {
    /// The kind of record, as a person reads it (`"Bill"`).
    fn label(&self) -> &'static str;
    /// The capabilities the owning module asks to read its records.
    fn view_capabilities(&self) -> &'static [&'static str];
    /// The record `document_id` of `company_id`, read with the caller's scope.
    fn document<'a>(
        &'a self,
        client: SourceClient<'a>,
        scope: &'a Scope,
        company_id: i64,
        document_id: i64,
    ) -> SourceFuture<'a>;
    /// The record's own page, so the Source documents list can link the
    /// record each file supports; `None` for a record with no page.
    fn href(&self, _document_id: i64) -> Option<String> {
        None
    }
}

/// The contribution payload: `Contribution::new(SOURCES, "finance/<module>/<record>", provenance, &SourceDefinition { source: &BillSource })`.
pub struct SourceDefinition {
    pub source: &'static dyn Source,
}

/// A source id is `<contributing module id>/<record kind>` in snake_case and
/// at most 100 characters, so the id says who owns the record.
fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let definition = entry.get::<SourceDefinition>()?;
        let kind = entry
            .id
            .strip_prefix(entry.provenance.module)
            .and_then(|rest| rest.strip_prefix('/'));
        let valid = kind.is_some_and(|kind| {
            !kind.is_empty()
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && kind.as_bytes()[0].is_ascii_lowercase()
        }) && entry.id.len() <= 100
            && !definition.source.label().trim().is_empty()
            && !definition.source.view_capabilities().is_empty()
            && definition
                .source
                .view_capabilities()
                .iter()
                .all(|capability| !capability.is_empty());
        if !valid {
            return Err(RegistryError::Contribution(format!(
                "invalid attachment source {} at {}:{}",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<SourceDefinition>(SOURCES, MODULE, validate) }
