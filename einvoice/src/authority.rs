//! The clearance authority a company's documents go to (Bilimbi
//! `Bilimbi.Finance.Einvoice.Authority`).
//!
//! The facade, tables and pages speak only of fiscal documents, parties,
//! submissions and status events. Everything one authority defines (its rules,
//! code lists, document format, signature, API and the shape of its answers)
//! sits behind this trait in that authority's own module. This release clears
//! documents with one authority, MyInvois (`myinvois.rs`).
//!
//! The callbacks that take a draft or a recorded document (draft validation
//! and the payload) and the API callbacks (login, submit, progress, find,
//! status, cancel) are here with their types. Each API callback answers in
//! meta-terms ([`Answer`] and an outcome enum), keeping the authority's own
//! answer beside it so a caller can store it as the record. The consolidated
//! document's shape and refusal are the authority's too; none of
//! them is invented before then.
use crate::code_lists::Codes;
use crate::document::Document;
use crate::draft::Draft;
use crate::myinvois::signer::SigningKey;
use crate::FieldError;
use bilibi_integration::Integration;
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;

/// A document format version a company may choose, and whether it carries a
/// signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentVersion {
    pub version: &'static str,
    pub signed: bool,
}

/// The ledger accounts a validated document posts to, by the company's own
/// account codes (the `finance.einvoice.posting.*` settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PostingAccount {
    Receivable,
    Revenue,
    Tax,
    Settlement,
    Payable,
    Expense,
    PurchaseTax,
}
impl PostingAccount {
    pub const ALL: [PostingAccount; 7] = [
        Self::Receivable,
        Self::Revenue,
        Self::Tax,
        Self::Settlement,
        Self::Payable,
        Self::Expense,
        Self::PurchaseTax,
    ];
    /// The setting field name: `finance.einvoice.posting.<field>`.
    pub fn field(self) -> &'static str {
        match self {
            Self::Receivable => "receivable_account",
            Self::Revenue => "revenue_account",
            Self::Tax => "tax_account",
            Self::Settlement => "settlement_account",
            Self::Payable => "payable_account",
            Self::Expense => "expense_account",
            Self::PurchaseTax => "purchase_tax_account",
        }
    }
}

/// The side of an entry line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Debit,
    Credit,
}

/// How a validated document of one type posts to the ledger, naming the
/// company's posting settings: `counter` (the receivable, payable or
/// settlement account) takes `side`, and `net` (revenue or expense) and `tax`
/// take the opposite side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostingRule {
    pub side: Side,
    pub counter: PostingAccount,
    pub net: PostingAccount,
    pub tax: PostingAccount,
}

/// A field whose shape the authority fixes, for settings validators and
/// document validation alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKind {
    TaxId,
    Phone,
    IndustryCode,
    RegistrationType,
    StateCode,
    CountryCode,
}

/// The supplier fields every document carries that a company's profile can
/// leave blank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplierField {
    TaxId,
    RegistrationNumber,
    IndustryCode,
    BusinessActivity,
    Phone,
    AddressLine1,
    City,
    StateCode,
    CountryCode,
}
impl SupplierField {
    /// The name of the `finance.einvoice.supplier.*` setting that fills it.
    pub fn field(self) -> &'static str {
        match self {
            Self::TaxId => "tax_id",
            Self::RegistrationNumber => "registration_number",
            Self::IndustryCode => "industry_code",
            Self::BusinessActivity => "business_activity",
            Self::Phone => "phone",
            Self::AddressLine1 => "address_line1",
            Self::City => "city",
            Self::StateCode => "state_code",
            Self::CountryCode => "country_code",
        }
    }
}

/// The attributes of a signing certificate's subject that carry the
/// company's tax and registration numbers, as the authority writes them
/// there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertificateSubject {
    pub organization_identifier: Option<String>,
    pub serial_number: Option<String>,
}

/// The tax ID and registration number a certificate states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateIdentity {
    pub tax_id: Option<String>,
    pub registration_number: Option<String>,
}

/// The company's supplier profile as the authority needs it: every
/// `finance.einvoice.supplier.*` value, blank ones as `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Supplier {
    pub name: Option<String>,
    pub tax_id: Option<String>,
    pub registration_type: Option<String>,
    pub registration_number: Option<String>,
    pub sales_tax_registration: Option<String>,
    pub tourism_tax_registration: Option<String>,
    pub industry_code: Option<String>,
    pub business_activity: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub address_line3: Option<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    pub state_code: Option<String>,
    pub country_code: Option<String>,
}

/// The period a consolidated document covers and when it may be issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsolidationWindow {
    /// The first instant of the month, in the authority's calendar.
    pub from: Timestamp,
    /// The first instant after the month, and the moment from which the
    /// consolidated document may be issued.
    pub until: Timestamp,
    /// When the time the authority allows for issuing it ends.
    pub closes_at: Timestamp,
}

/// What the authority's draft validation reads beside the draft.
#[derive(Clone, Copy, Debug)]
pub struct ValidationContext<'a> {
    /// The company's own profile, which is the supplier, or the buyer of a
    /// self-billed document.
    pub supplier: &'a Supplier,
    /// The company's active codes, one entry per list the authority
    /// publishes (`Einvoice::list_codes`); a list missing here knows no code.
    pub codes: &'a BTreeMap<String, Codes>,
    /// The document a note's `original_document_id` names, as the company
    /// recorded it, or `None` when it names none of the company's.
    pub original: Option<&'a Original>,
    /// Whether the draft is a consolidated document built by consolidation.
    pub consolidated: bool,
}

/// What draft validation needs of the original document a note corrects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Original {
    pub document_type: String,
    pub currency_code: String,
    /// The document's current status (`valid`, `rejected`, ...).
    pub status: String,
    /// The authority's UUID for it, once validated.
    pub document_uuid: Option<String>,
}

/// How a document's payload is built.
#[derive(Clone, Copy, Debug)]
pub struct PayloadOptions<'a> {
    /// The format version, one of [`Authority::document_versions`].
    pub version: &'a str,
    /// The company's certificate and key, for a version that is signed.
    pub signing_key: Option<&'a SigningKey>,
}

/// A document's payload: the exact bytes to submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    /// `JSON` or `XML`.
    pub format: &'static str,
    pub version: String,
    pub bytes: Vec<u8>,
}

/// Why a payload was not built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// The version is signed and no signing key was given.
    #[error("signing key required")]
    SigningKeyRequired,
    /// Not a version the authority accepts.
    #[error("unsupported version")]
    UnsupportedVersion,
    /// The private key did not sign.
    #[error("the private key did not sign")]
    SigningFailed,
}

/// A company's credentials for one environment of the authority's API. The
/// secret never leaves the module: it is read inside the facade, handed to the
/// API call, and left out of `Debug`.
#[derive(Clone)]
pub struct Credentials {
    /// `sandbox` or `production`.
    pub environment: String,
    pub client_id: Option<String>,
    pub(crate) client_secret: Option<String>,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("environment", &self.environment)
            .field("client_id", &self.client_id)
            .field("client_secret_set", &self.client_secret.is_some())
            .finish()
    }
}

/// What an API callback needs: the outgoing HTTP edge, the person the call
/// acts for (every exchange is recorded under them), the company, its
/// credentials for the document's environment, and that environment's base URL.
pub struct Connection<'a> {
    pub(crate) integration: &'a Integration,
    pub(crate) scope: &'a Scope,
    pub(crate) company_id: i64,
    pub(crate) credentials: &'a Credentials,
    pub(crate) api_url: &'a str,
}

/// What the authority answered, as a status event keeps it: the HTTP status
/// and body (`None` when no answer came), a short reason, the seconds it
/// asked to wait, and the identifiers it gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub http_status: Option<u16>,
    pub response: Option<Value>,
    pub reason: Option<String>,
    pub retry_after: Option<u64>,
    pub submission_uid: Option<String>,
    pub document_uuid: Option<String>,
}

/// What became of a submitted payload.
#[derive(Clone, Debug, PartialEq)]
pub enum SubmitOutcome {
    /// Accepted for processing, with its submission UID and document UUID.
    Accepted(Answer),
    /// The authority refused it and said why.
    Refused(Answer),
    /// The same bytes were submitted inside the authority's window: an
    /// earlier attempt reached it.
    Duplicate(Answer),
    RateLimited(Answer),
    /// An outage or a transport failure: no answer, or a 5xx.
    Unavailable(Answer),
}

/// Whether a submission has finished processing.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Done(Answer),
    InProgress(Answer),
    Refused(Answer),
    RateLimited(Answer),
    Unavailable(Answer),
}

/// Which document the authority holds under a code number.
#[derive(Clone, Debug, PartialEq)]
pub enum Search {
    /// Found, with its submission UID and document UUID and the authority's
    /// own entry as the response.
    Found(Answer),
    NotFound(Answer),
    Refused(Answer),
    RateLimited(Answer),
    Unavailable(Answer),
}

/// The status the authority holds a document in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityStatus {
    Submitted,
    Valid,
    Invalid,
    Cancelled,
}
impl AuthorityStatus {
    /// The name a status event keeps (`Valid`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::Submitted => "Submitted",
            Self::Valid => "Valid",
            Self::Invalid => "Invalid",
            Self::Cancelled => "Cancelled",
        }
    }
}

/// A document's current status at the authority.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentStatus {
    pub status: AuthorityStatus,
    /// The identifier that completes the validation link.
    pub long_id: Option<String>,
    pub validated_at: Option<Timestamp>,
    pub cancelled_at: Option<Timestamp>,
    /// When the buyer asked for the document's rejection, if they did.
    pub rejection_requested_at: Option<Timestamp>,
    pub answer: Answer,
}

/// The answer to a status read.
#[derive(Clone, Debug, PartialEq)]
pub enum StatusRead {
    Read(DocumentStatus),
    Refused(Answer),
    RateLimited(Answer),
    Unavailable(Answer),
}

/// Why the authority refused a cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelRefusal {
    /// Its cancellation window has ended.
    PeriodOver,
    /// It is not in a state that can be cancelled.
    IncorrectState,
    /// A note the authority holds references it.
    ActiveReferencingDocuments,
    Other,
}

/// What became of a cancellation request.
#[derive(Clone, Debug, PartialEq)]
pub enum CancelOutcome {
    Cancelled(Answer),
    Refused(CancelRefusal, Answer),
    RateLimited(Answer),
    Unavailable(Answer),
}

/// How a login with the company's credentials went, the "Test connection"
/// outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    Succeeded,
    MissingCredentials,
    /// The authority refused it with this HTTP status.
    Refused(u16),
    /// The authority asks the operator to wait this many seconds before login.
    RateLimited {
        status: u16,
        seconds: u64,
    },
    Unreachable,
    Failed,
}
impl LoginOutcome {
    /// The outcome an audit action names: `succeeded`, `missing_credentials`,
    /// `refused_<status>`, `unreachable` or `failed`.
    pub fn name(self) -> String {
        match self {
            Self::Succeeded => "succeeded".into(),
            Self::MissingCredentials => "missing_credentials".into(),
            Self::Refused(status) | Self::RateLimited { status, .. } => format!("refused_{status}"),
            Self::Unreachable => "unreachable".into(),
            Self::Failed => "failed".into(),
        }
    }
}

/// A search for a document the authority may already hold: the company's
/// code number, among what the submitter sent between `since` and `until`,
/// leaving out any whose document UUID is in `exclude` (identifiers this
/// document already recorded, which belong to an earlier cycle).
#[derive(Clone, Copy, Debug)]
pub struct Lookup<'a> {
    pub code_number: &'a str,
    pub since: Timestamp,
    pub until: Timestamp,
    pub exclude: &'a [String],
}

/// What one clearance authority defines for the module.
pub trait Authority {
    /// The authority's stable ID, stored on each document and code override.
    fn id(&self) -> &'static str;

    /// The document types a company may issue.
    fn issuable_document_types(&self) -> Vec<&'static str>;

    /// The document types that reference an original document (credit, debit
    /// and refund notes).
    fn note_document_types(&self) -> Vec<&'static str>;

    /// The document types the company issues as the buyer (self-billed): the
    /// counterparty is the supplier and the company's own profile the buyer.
    fn self_billed_document_types(&self) -> Vec<&'static str>;

    /// How a validated document of this type posts to the ledger, or `None`
    /// for a type with no rule.
    fn posting_rule(&self, document_type: &str) -> Option<PostingRule>;

    /// The human name of a document type, as the pages and the PDF show it.
    fn document_type_name(&self, document_type: &str) -> String;

    /// The document type a posted sale is issued as, or the note that
    /// corrects one when `credit_note`.
    fn source_document_type(&self, credit_note: bool) -> &'static str;

    /// The kinds of registration number a party carries, each with the name
    /// a person reads.
    fn registration_types(&self) -> Vec<(&'static str, &'static str)>;

    /// The authority's country code for an ISO 3166 country code, when it is
    /// sure of it; a person chooses otherwise.
    fn country_code(&self, iso: &str) -> Option<&'static str>;

    /// The tax type of a line when its rate alone decides it (a line with no
    /// tax); `None` when a person must say which tax it is.
    fn tax_type_for(&self, rate: Option<rust_decimal::Decimal>) -> Option<&'static str>;

    /// The last day, in the authority's calendar, on which the consolidated
    /// document of the month holding `month` may be issued.
    fn consolidation_last_day(&self, month: jiff::civil::Date) -> jiff::civil::Date;

    /// Hours after validation in which the supplier may still cancel a document.
    fn cancellation_window_hours(&self) -> u32;

    /// Hours of unbroken outage after which an operator is told a submission
    /// is overdue.
    fn outage_notice_hours(&self) -> u32;

    /// The document format versions a company may choose, the default first.
    fn document_versions(&self) -> &'static [DocumentVersion];

    /// Whether `value` has the shape the authority requires of a field of
    /// this kind. Settings validators and document validation both ask here.
    fn valid_format(&self, kind: FormatKind, value: &str) -> bool;

    /// What `valid_format` requires of a field of this kind, for an error
    /// message.
    fn format_message(&self, kind: FormatKind) -> &'static str;

    /// The supplier profile fields every document carries that `supplier`
    /// leaves blank.
    fn supplier_missing(&self, supplier: &Supplier) -> Vec<SupplierField>;

    /// The tax ID and registration number a signing certificate's subject
    /// carries.
    fn certificate_identity(&self, subject: &CertificateSubject) -> CertificateIdentity;

    /// The period a consolidated document for the calendar month containing
    /// `month` covers, in the authority's own calendar, and when it may be
    /// issued.
    fn consolidation_window(&self, month: jiff::civil::Date) -> ConsolidationWindow;

    /// Why a recorded document may not be covered by a consolidated document,
    /// in a short phrase, or `None` when it may: the authority's own limits on
    /// what a consolidated document reports.
    fn consolidation_refusal(&self, document: &Document) -> Option<String>;

    /// The consolidated document for a month, as the draft the authority
    /// shapes it into (document type, the general buyer, the consolidated
    /// classification, the month as its billing period), whose lines carry
    /// the given amounts and their sources' tax.
    fn consolidated_document(&self, request: &crate::ConsolidatedRequest) -> Draft;

    /// The public link that shows a validated document on the authority's
    /// portal, whose base URL is `portal_url`.
    fn validation_url(&self, portal_url: &str, document_uuid: &str, long_id: &str) -> String;

    /// The authority's own refusals of a draft, beside the shape check every
    /// authority shares (`Draft::validate` runs both): codes outside the
    /// company's code lists, fields its rules require, a company profile
    /// missing what every document carries, and a note whose original is not
    /// one the authority lets it reference.
    fn validate(&self, draft: &Draft, context: &ValidationContext<'_>) -> Vec<FieldError>;

    /// The payload for a recorded document in `options.version` of the
    /// authority's format: its format, version and the exact bytes to submit,
    /// signed at `signing_time` when the version is signed.
    fn payload(
        &self,
        document: &Document,
        options: &PayloadOptions<'_>,
        signing_time: Timestamp,
    ) -> Result<Payload, PayloadError>;

    /// Logs in to the authority with the company's credentials, afresh.
    fn login(&self, connection: &Connection<'_>) -> impl Future<Output = LoginOutcome> + Send;

    /// Submits one document's payload under its code number and reports what
    /// became of it.
    fn submit(
        &self,
        connection: &Connection<'_>,
        payload: &Payload,
        code_number: &str,
    ) -> impl Future<Output = SubmitOutcome> + Send;

    /// Whether a submission has finished processing.
    fn submission_progress(
        &self,
        connection: &Connection<'_>,
        submission_uid: &str,
    ) -> impl Future<Output = Progress> + Send;

    /// Looks up a document the authority may already hold (see [`Lookup`]).
    /// Used after a duplicate or an unknown outcome, so a submission is never
    /// repeated once the authority accepted it.
    fn find_submission(
        &self,
        connection: &Connection<'_>,
        lookup: &Lookup<'_>,
    ) -> impl Future<Output = Search> + Send;

    /// A document's current status.
    fn document_status(
        &self,
        connection: &Connection<'_>,
        document_uuid: &str,
    ) -> impl Future<Output = StatusRead> + Send;

    /// Cancels a validated document with a reason.
    fn cancel(
        &self,
        connection: &Connection<'_>,
        document_uuid: &str,
        reason: &str,
    ) -> impl Future<Output = CancelOutcome> + Send;
}
