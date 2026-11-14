//! The MyInvois clearance authority (Bilimbi `Bilimbi.Finance.Einvoice.MyInvois`):
//! what the module knows of it without talking to it. Rules follow the
//! authority's e-Invoice Guideline v4.8 (30 August 2026) and its SDK as read on
//! 12 October 2026. Format version 1.1 turns on signature validation; version
//! 1.0 is submitted unsigned, which is how a company tests the sandbox before
//! its certificate arrives. Invoices (`01`) and the credit, debit and refund
//! notes that reference a validated original (`02` to `04`), their
//! self-billed counterparts (`11` to `14`), which the company issues as the
//! buyer, and the monthly consolidated invoice, in ringgit, are issued in this
//! release; foreign currencies come later. The builder (`ubl.rs`) already
//! writes every element the SDK's samples use, and the signer (`signer.rs`)
//! the JSON XAdES signature a version 1.1 document carries.
//!
//! The consolidated invoice follows the Specific Guideline v4.9 (§3.6, §3.7):
//! it reports a calendar month (Malaysian time) of sales no buyer asked an
//! individual e-invoice for, issued within seven days after the month ends. A
//! self-billed document (§8.3) names the supplier as the counterparty.
mod api;
mod client;
pub(crate) use api::sha256_hex;
pub mod json;
pub mod signer;
pub mod ubl;

use crate::authority::{
    Authority, CancelOutcome, CertificateIdentity, CertificateSubject, Connection,
    ConsolidationWindow, DocumentVersion, FormatKind, LoginOutcome, Lookup, Payload, PayloadError,
    PayloadOptions, PostingAccount, PostingRule, Progress, Search, Side, StatusRead, SubmitOutcome,
    Supplier, SupplierField, ValidationContext,
};
use crate::code_lists::Codes;
use crate::document::{Document, DocumentLine, PartySnapshot};
use crate::draft::{Draft, DraftLine, DraftParty};
use crate::FieldError;
use jiff::{civil::Date, tz::TimeZone, SignedDuration, Timestamp};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// The authority's stable ID.
pub(crate) const ID: &str = "myinvois";
/// The format version documents are built in when a company chose none.
pub(crate) const VERSION: &str = "1.1";
const VERSIONS: [DocumentVersion; 2] = [
    DocumentVersion {
        version: "1.1",
        signed: true,
    },
    DocumentVersion {
        version: "1.0",
        signed: false,
    },
];

const INVOICE: &str = "01";
const NOTES: [&str; 3] = ["02", "03", "04"];
const SELF_BILLED_INVOICE: &str = "11";
const SELF_BILLED_NOTES: [&str; 3] = ["12", "13", "14"];
// The invoice type each note type corrects.
fn invoice_of(note_type: &str) -> Option<&'static str> {
    match note_type {
        "02" | "03" | "04" => Some(INVOICE),
        "12" | "13" | "14" => Some(SELF_BILLED_INVOICE),
        _ => None,
    }
}
// The authority's general TINs (Specific Guideline §3.7 and §8.3): the
// general public buyer of a consolidated invoice, and a foreign supplier
// without a Malaysian TIN on a self-billed document.
const GENERAL_PUBLIC_TIN: &str = "EI00000000010";
const FOREIGN_SUPPLIER_TIN: &str = "EI00000000030";
const HOME_COUNTRY: &str = "MYS";
const CONSOLIDATED_CLASSIFICATION: &str = "004";
// A note references an original the authority holds as validated (a buyer's
// rejection request leaves it valid there).
const REFERABLE: [&str; 2] = ["valid", "rejected"];
// The authority reports tax in ringgit, whatever the document currency.
const TAX_CURRENCY: &str = "MYR";
const REGISTRATION_TYPES: [&str; 4] = ["BRN", "NRIC", "PASSPORT", "ARMY"];
const EXEMPT: &str = "E";
// Tax types that charge tax, so a rate above zero; and those that take none.
const TAXED: [&str; 5] = ["01", "02", "03", "04", "05"];
const UNTAXED: [&str; 2] = ["06", EXEMPT];
// The supplier profile fields every document carries, as a refusal names them.
const SUPPLIER_REQUIRED: [(SupplierField, &str); 9] = [
    (SupplierField::TaxId, "supplier TIN"),
    (
        SupplierField::RegistrationNumber,
        "supplier registration number",
    ),
    (SupplierField::IndustryCode, "industry classification code"),
    (SupplierField::BusinessActivity, "business activity"),
    (SupplierField::Phone, "supplier phone"),
    (SupplierField::AddressLine1, "address line 1"),
    (SupplierField::City, "city"),
    (SupplierField::StateCode, "state code"),
    (SupplierField::CountryCode, "country code"),
];

// A consolidated invoice is issued within seven days after its month ends, in
// Malaysian time (UTC+8, no daylight saving).
const LOCAL_OFFSET_HOURS: i8 = 8;
const CONSOLIDATION_DAYS: i64 = 7;
// A sale above this needs an individual e-invoice, never a consolidated one.
const CONSOLIDATION_LIMIT: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

pub(crate) const TAX_ID_MESSAGE: &str = "must be one or two capital letters and 8 to 12 digits";
pub(crate) const PHONE_MESSAGE: &str =
    "must be a plus sign and 8 to 20 digits, 20 characters at most";
pub(crate) const INDUSTRY_CODE_MESSAGE: &str = "must be five digits";
pub(crate) const REGISTRATION_TYPE_MESSAGE: &str = "must be BRN, NRIC, PASSPORT, or ARMY";
pub(crate) const STATE_CODE_MESSAGE: &str = "must be two digits";
pub(crate) const COUNTRY_CODE_MESSAGE: &str = "must be three capital letters";

/// MyInvois, the one authority this release clears documents with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MyInvois;

fn digits(value: &[u8], count: std::ops::RangeInclusive<usize>) -> bool {
    count.contains(&value.len()) && value.iter().all(u8::is_ascii_digit)
}

pub(crate) fn valid_format(kind: FormatKind, value: &str) -> bool {
    let bytes = value.as_bytes();
    match kind {
        // One or two capital letters, then 8 to 12 digits.
        FormatKind::TaxId => {
            let letters = bytes.iter().take_while(|b| b.is_ascii_uppercase()).count();
            (1..=2).contains(&letters) && digits(&bytes[letters..], 8..=12)
        }
        FormatKind::Phone => bytes.first() == Some(&b'+') && digits(&bytes[1..], 8..=19),
        FormatKind::IndustryCode => digits(bytes, 5..=5),
        FormatKind::RegistrationType => ["BRN", "NRIC", "PASSPORT", "ARMY"].contains(&value),
        FormatKind::StateCode => digits(bytes, 2..=2),
        FormatKind::CountryCode => bytes.len() == 3 && bytes.iter().all(u8::is_ascii_uppercase),
    }
}

/// Whether the format version is one the authority accepts.
pub(crate) fn valid_document_version(value: &str) -> bool {
    VERSIONS.iter().any(|v| v.version == value)
}

/// Settings validator for the authority's base URLs: an https URL without a
/// path (a query or fragment is not read).
pub(crate) fn valid_url(value: &str) -> bool {
    let rest = value.strip_prefix("https://").unwrap_or("");
    host_only(rest)
}

/// Settings validator for the sandbox API's base URL: as [`valid_url`], or an
/// http URL on this machine's loopback (`127.0.0.1`, `[::1]` or `localhost`,
/// with a port), where a stand-in for the authority answers sandbox calls.
/// Production traffic is https only.
pub(crate) fn valid_sandbox_api_url(value: &str) -> bool {
    if valid_url(value) {
        return true;
    }
    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['?', '#', '/']).next().unwrap_or("");
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    host_only(rest) && ["127.0.0.1", "[::1]", "localhost"].contains(&host)
}

fn host_only(rest: &str) -> bool {
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let host = rest.strip_suffix('/').unwrap_or(rest);
    !host.is_empty()
        && !host.contains('/')
        && !host.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Authority for MyInvois {
    fn id(&self) -> &'static str {
        ID
    }

    fn issuable_document_types(&self) -> Vec<&'static str> {
        let mut types = vec![INVOICE];
        types.extend(NOTES);
        types.push(SELF_BILLED_INVOICE);
        types.extend(SELF_BILLED_NOTES);
        types
    }

    fn note_document_types(&self) -> Vec<&'static str> {
        NOTES.into_iter().chain(SELF_BILLED_NOTES).collect()
    }

    fn self_billed_document_types(&self) -> Vec<&'static str> {
        let mut types = vec![SELF_BILLED_INVOICE];
        types.extend(SELF_BILLED_NOTES);
        types
    }

    // A sale debits the receivable and credits revenue and tax; a self-billed
    // purchase credits the payable and debits expense and purchase tax. A
    // credit note posts the mirror image, and a refund note settles in cash.
    fn posting_rule(&self, document_type: &str) -> Option<PostingRule> {
        use PostingAccount::{Expense, Payable, PurchaseTax, Receivable, Revenue, Settlement, Tax};
        let sale = |side, counter| PostingRule {
            side,
            counter,
            net: Revenue,
            tax: Tax,
        };
        let purchase = |side, counter| PostingRule {
            side,
            counter,
            net: Expense,
            tax: PurchaseTax,
        };
        match document_type {
            "01" => Some(sale(Side::Debit, Receivable)),
            "02" => Some(sale(Side::Credit, Receivable)),
            "03" => Some(sale(Side::Debit, Receivable)),
            "04" => Some(sale(Side::Credit, Settlement)),
            "11" => Some(purchase(Side::Credit, Payable)),
            "12" => Some(purchase(Side::Debit, Payable)),
            "13" => Some(purchase(Side::Credit, Payable)),
            "14" => Some(purchase(Side::Debit, Settlement)),
            _ => None,
        }
    }

    fn document_type_name(&self, document_type: &str) -> String {
        match document_type {
            "01" => "Invoice".into(),
            "02" => "Credit note".into(),
            "03" => "Debit note".into(),
            "04" => "Refund note".into(),
            "11" => "Self-billed invoice".into(),
            "12" => "Self-billed credit note".into(),
            "13" => "Self-billed debit note".into(),
            "14" => "Self-billed refund note".into(),
            other => format!("Document type {other}"),
        }
    }

    fn source_document_type(&self, credit_note: bool) -> &'static str {
        if credit_note {
            NOTES[0]
        } else {
            INVOICE
        }
    }

    fn registration_types(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("BRN", "Business registration number"),
            ("NRIC", "MyKad or MyTentera number"),
            ("PASSPORT", "Passport number"),
            ("ARMY", "Army number"),
        ]
    }

    // Only the home country: every other country is the person's choice from
    // the authority's list.
    fn country_code(&self, iso: &str) -> Option<&'static str> {
        matches!(iso.trim().to_ascii_uppercase().as_str(), "MY" | "MYS").then_some(HOME_COUNTRY)
    }

    fn tax_type_for(&self, rate: Option<Decimal>) -> Option<&'static str> {
        rate.is_none_or(|rate| rate.is_zero())
            .then_some(UNTAXED[0])
    }

    fn consolidation_last_day(&self, month: Date) -> Date {
        last_of_month_plus_one(month)
            .checked_add(jiff::Span::new().days(CONSOLIDATION_DAYS - 1))
            .unwrap_or(month)
    }

    fn cancellation_window_hours(&self) -> u32 {
        72
    }

    fn outage_notice_hours(&self) -> u32 {
        72
    }

    fn document_versions(&self) -> &'static [DocumentVersion] {
        &VERSIONS
    }

    fn valid_format(&self, kind: FormatKind, value: &str) -> bool {
        valid_format(kind, value)
    }

    fn format_message(&self, kind: FormatKind) -> &'static str {
        match kind {
            FormatKind::TaxId => TAX_ID_MESSAGE,
            FormatKind::Phone => PHONE_MESSAGE,
            FormatKind::IndustryCode => INDUSTRY_CODE_MESSAGE,
            FormatKind::RegistrationType => REGISTRATION_TYPE_MESSAGE,
            FormatKind::StateCode => STATE_CODE_MESSAGE,
            FormatKind::CountryCode => COUNTRY_CODE_MESSAGE,
        }
    }

    fn supplier_missing(&self, supplier: &Supplier) -> Vec<SupplierField> {
        [
            (SupplierField::TaxId, &supplier.tax_id),
            (
                SupplierField::RegistrationNumber,
                &supplier.registration_number,
            ),
            (SupplierField::IndustryCode, &supplier.industry_code),
            (SupplierField::BusinessActivity, &supplier.business_activity),
            (SupplierField::Phone, &supplier.phone),
            (SupplierField::AddressLine1, &supplier.address_line1),
            (SupplierField::City, &supplier.city),
            (SupplierField::StateCode, &supplier.state_code),
            (SupplierField::CountryCode, &supplier.country_code),
        ]
        .into_iter()
        .filter(|(_, value)| value.is_none())
        .map(|(field, _)| field)
        .collect()
    }

    fn certificate_identity(&self, subject: &CertificateSubject) -> CertificateIdentity {
        CertificateIdentity {
            tax_id: subject.organization_identifier.clone(),
            registration_number: subject.serial_number.clone(),
        }
    }

    fn consolidation_window(&self, month: Date) -> ConsolidationWindow {
        let first = month.first_of_month();
        let until = local_midnight(last_of_month_plus_one(month));
        ConsolidationWindow {
            from: local_midnight(first),
            until,
            closes_at: until
                .checked_add(SignedDuration::from_hours(CONSOLIDATION_DAYS * 24))
                .unwrap_or(until),
        }
    }

    // A consolidated invoice reports sales in ringgit no buyer asked an
    // individual invoice for, none above RM10,000 (Specific Guideline §3.6).
    fn consolidation_refusal(&self, document: &Document) -> Option<String> {
        if document.document_type != INVOICE {
            Some("is not an invoice".into())
        } else if document.currency_code != TAX_CURRENCY {
            Some(format!("is not in {TAX_CURRENCY}"))
        } else if document.payable_amount > CONSOLIDATION_LIMIT {
            Some(format!(
                "is above RM{CONSOLIDATION_LIMIT}, which needs an individual e-invoice"
            ))
        } else {
            None
        }
    }

    // The general public buyer, whose contact the authority writes as "NA",
    // the consolidated classification, and one unit per line (§3.7).
    fn consolidated_document(&self, request: &crate::ConsolidatedRequest) -> Draft {
        let na = || Some("NA".to_owned());
        Draft {
            document_type: Some(INVOICE.into()),
            code_number: request.code_number.clone(),
            issued_at: Some(request.issued_at),
            currency_code: Some(TAX_CURRENCY.into()),
            billing_period_start: Some(request.month.first_of_month()),
            billing_period_end: Some(request.month.last_of_month()),
            billing_period_description: Some("Monthly".into()),
            buyer: Some(DraftParty {
                name: Some("General Public".into()),
                tax_id: Some(GENERAL_PUBLIC_TIN.into()),
                registration_type: Some("BRN".into()),
                registration_number: na(),
                phone: na(),
                address: Some(crate::DraftAddress {
                    lines: vec!["NA".into()],
                    city: na(),
                    state_code: Some("17".into()),
                    country_code: Some(HOME_COUNTRY.into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            lines: request
                .lines
                .iter()
                .map(|line| DraftLine {
                    description: Some(line.description.clone()),
                    classification_code: Some(CONSOLIDATED_CLASSIFICATION.into()),
                    quantity: Some(Decimal::ONE),
                    unit_code: Some("C62".into()),
                    unit_price: Some(line.amount),
                    tax_type: Some(line.tax_type.clone()),
                    tax_rate: line.tax_rate,
                    tax_exemption_reason: line.tax_exemption_reason.clone(),
                    tax_amount: Some(line.tax_amount),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn validation_url(&self, portal_url: &str, document_uuid: &str, long_id: &str) -> String {
        format!(
            "{}/{document_uuid}/share/{long_id}",
            portal_url.trim_end_matches('/')
        )
    }

    fn validate(&self, draft: &Draft, context: &ValidationContext<'_>) -> Vec<FieldError> {
        Rules {
            draft,
            context,
            errors: Vec::new(),
        }
        .run()
    }

    fn payload(
        &self,
        document: &Document,
        options: &PayloadOptions<'_>,
        signing_time: Timestamp,
    ) -> Result<Payload, PayloadError> {
        let version = VERSIONS
            .iter()
            .find(|v| v.version == options.version)
            .ok_or(PayloadError::UnsupportedVersion)?;
        let built = ubl::build(&ubl(document, version.version));
        let bytes = if version.signed {
            let key = options
                .signing_key
                .ok_or(PayloadError::SigningKeyRequired)?;
            signer::sign(&built, key, signing_time).map_err(|_| PayloadError::SigningFailed)?
        } else {
            signer::document_bytes(&built)
        };
        Ok(Payload {
            format: "JSON",
            version: version.version.to_owned(),
            bytes,
        })
    }

    async fn login(&self, connection: &Connection<'_>) -> LoginOutcome {
        api::login(connection).await
    }

    async fn submit(
        &self,
        connection: &Connection<'_>,
        payload: &Payload,
        code_number: &str,
    ) -> SubmitOutcome {
        api::submit(connection, payload, code_number).await
    }

    async fn submission_progress(
        &self,
        connection: &Connection<'_>,
        submission_uid: &str,
    ) -> Progress {
        api::submission_progress(connection, submission_uid).await
    }

    async fn find_submission(&self, connection: &Connection<'_>, lookup: &Lookup<'_>) -> Search {
        api::find_submission(connection, lookup).await
    }

    async fn document_status(
        &self,
        connection: &Connection<'_>,
        document_uuid: &str,
    ) -> StatusRead {
        api::document_status(connection, document_uuid).await
    }

    async fn cancel(
        &self,
        connection: &Connection<'_>,
        document_uuid: &str,
        reason: &str,
    ) -> CancelOutcome {
        api::cancel(connection, document_uuid, reason).await
    }
}

/// One run of the authority's rules over a draft.
struct Rules<'a> {
    draft: &'a Draft,
    context: &'a ValidationContext<'a>,
    errors: Vec<FieldError>,
}

impl Rules<'_> {
    fn run(mut self) -> Vec<FieldError> {
        let draft = self.draft;
        let document_type = text(&draft.document_type);
        // The company's profile is the supplier, or the buyer on a self-billed
        // document; either way it must carry what every document needs.
        let self_billed =
            document_type.is_some_and(|t| MyInvois.self_billed_document_types().contains(&t));
        let profile_field = if self_billed { "buyer" } else { "supplier" };

        if document_type.is_some_and(|t| !MyInvois.issuable_document_types().contains(&t)) {
            self.add(
                "document_type",
                "is not a document type this release issues",
            );
        }
        self.code("document_type", document_type, "document_type");
        self.code("currency_code", text(&draft.currency_code), "currency");
        self.code(
            "payment_means_code",
            text(&draft.payment_means_code),
            "payment_means",
        );
        self.tax_currency();
        self.original();
        self.profile(profile_field);
        if let Some(buyer) = &draft.buyer {
            self.party("buyer", buyer);
        }
        if let Some(supplier) = &draft.supplier {
            self.supplier_party("supplier", supplier);
        }
        for (index, line) in draft.lines.iter().enumerate() {
            self.line(&format!("lines[{index}]"), line);
        }
        self.consolidated();
        self.errors
    }

    fn add(&mut self, field: &str, message: &str) {
        self.errors.push(FieldError::new(field, message));
    }

    fn codes(&self, list: &str) -> Option<&Codes> {
        self.context.codes.get(list)
    }

    fn known(&self, list: &str, code: &str) -> bool {
        self.codes(list)
            .is_some_and(|codes| codes.contains_key(code))
    }

    fn code(&mut self, field: &str, value: Option<&str>, list: &str) {
        if let Some(code) = value {
            if !self.known(list, code) {
                self.add(field, "is not a known code");
            }
        }
    }

    fn shape(&mut self, field: &str, kind: FormatKind, value: Option<&str>) {
        if let Some(value) = value {
            if !valid_format(kind, value) {
                self.add(field, MyInvois.format_message(kind));
            }
        }
    }

    // The SDK's foreign-currency sample writes tax amounts in ringgit beside
    // document-currency totals without saying how they convert; until the
    // sandbox confirms the rule, only ringgit documents are issued.
    fn tax_currency(&mut self) {
        if text(&self.draft.currency_code).is_some_and(|c| c != TAX_CURRENCY) {
            self.add(
                "currency_code",
                &format!("must be {TAX_CURRENCY} in this release"),
            );
        }
    }

    // A note references the validated original it corrects (Guideline
    // §2.4.5): the original must be valid at the authority (a buyer's
    // rejection request leaves it valid there), carry its UUID, be an invoice
    // rather than another note, and share the currency. An invoice references
    // nothing.
    fn original(&mut self) {
        let Some(document_type) = text(&self.draft.document_type) else {
            return;
        };
        let notes = MyInvois.note_document_types();
        let is_note = notes.contains(&document_type);
        let original_id = self.draft.original_document_id;
        let original = self.context.original;
        let field = "original_document_id";
        if is_note {
            let Some(original) = original else {
                if original_id.is_none() {
                    self.add(field, "is required for a note");
                } else {
                    self.add(field, "is not a document of this company");
                }
                return;
            };
            if original_id.is_none() {
                self.add(field, "is required for a note");
            } else if !REFERABLE.contains(&original.status.as_str()) {
                self.add(field, "is not a validated document");
            } else if original.document_uuid.is_none() {
                self.add(field, "has no document UUID from the authority");
            } else if notes.contains(&original.document_type.as_str()) {
                self.add(field, "is itself a note; reference the invoice");
            } else if Some(original.document_type.as_str()) != invoice_of(document_type) {
                // A self-billed note corrects a self-billed invoice, and a note a sale.
                let name =
                    MyInvois.document_type_name(invoice_of(document_type).unwrap_or(INVOICE));
                self.add(
                    field,
                    &format!("must be the {} it corrects", name.to_lowercase()),
                );
            } else if text(&self.draft.currency_code) != Some(original.currency_code.as_str()) {
                self.add(
                    "currency_code",
                    "must match the original document's currency",
                );
            }
        } else if original_id.is_some() {
            self.add(field, "is only given for a note");
        }
    }

    fn profile(&mut self, field: &str) {
        let profile = self.context.supplier;
        let missing: Vec<&str> = MyInvois
            .supplier_missing(profile)
            .into_iter()
            .filter_map(|missing| SUPPLIER_REQUIRED.iter().find(|(f, _)| *f == missing))
            .map(|(_, label)| *label)
            .collect();
        if !missing.is_empty() {
            self.add(
                field,
                &format!(
                    "is incomplete: set {} in the e-invoice settings",
                    missing.join(", ")
                ),
            );
        } else if !self.known(
            "industry",
            profile.industry_code.as_deref().unwrap_or_default(),
        ) {
            self.add(field, "industry classification code is not a known code");
        } else if !self.known("state", profile.state_code.as_deref().unwrap_or_default()) {
            self.add(field, "state code is not a known code");
        } else if !self.known(
            "country",
            profile.country_code.as_deref().unwrap_or_default(),
        ) {
            self.add(field, "country code is not a known code");
        }
    }

    // The general public buyer's contact is "NA" (Specific Guideline §3.7).
    fn party(&mut self, prefix: &str, party: &DraftParty) {
        let general_public = text(&party.tax_id) == Some(GENERAL_PUBLIC_TIN);
        self.shape(
            &format!("{prefix}.tax_id"),
            FormatKind::TaxId,
            text(&party.tax_id),
        );
        if text(&party.registration_type).is_some_and(|t| !REGISTRATION_TYPES.contains(&t)) {
            self.add(&format!("{prefix}.registration_type"), "is invalid");
        }
        match text(&party.phone) {
            None => self.add(&format!("{prefix}.phone"), "can't be blank"),
            Some("NA") if general_public => {}
            phone => self.shape(&format!("{prefix}.phone"), FormatKind::Phone, phone),
        }
        if let Some(address) = &party.address {
            self.code(
                &format!("{prefix}.address.state_code"),
                text(&address.state_code),
                "state",
            );
            self.code(
                &format!("{prefix}.address.country_code"),
                text(&address.country_code),
                "country",
            );
        }
    }

    // A self-billed document's supplier carries what a supplier profile does:
    // its industry code and activity (`00000`, "NOT APPLICABLE", for one
    // abroad). The foreign-supplier TIN is for a supplier outside Malaysia.
    fn supplier_party(&mut self, prefix: &str, party: &DraftParty) {
        self.party(prefix, party);
        for (field, value) in [
            ("industry_code", &party.industry_code),
            ("business_activity", &party.business_activity),
        ] {
            if text(value).is_none() {
                self.add(&format!("{prefix}.{field}"), "can't be blank");
            }
        }
        self.code(
            &format!("{prefix}.industry_code"),
            text(&party.industry_code),
            "industry",
        );
        let country = party.address.as_ref().and_then(|a| text(&a.country_code));
        if text(&party.tax_id) == Some(FOREIGN_SUPPLIER_TIN) && country == Some(HOME_COUNTRY) {
            self.add(
                &format!("{prefix}.tax_id"),
                "is the general TIN for a supplier outside Malaysia",
            );
        }
    }

    fn line(&mut self, prefix: &str, line: &DraftLine) {
        let consolidated = self.context.consolidated;
        self.rate(prefix, line);
        let classification = text(&line.classification_code);
        self.code(
            &format!("{prefix}.classification_code"),
            classification,
            "classification",
        );
        if let Some(code) = classification {
            if consolidated && code != CONSOLIDATED_CLASSIFICATION {
                self.add(
                    &format!("{prefix}.classification_code"),
                    &format!("must be {CONSOLIDATED_CLASSIFICATION} on a consolidated document"),
                );
            } else if !consolidated && code == CONSOLIDATED_CLASSIFICATION {
                self.add(
                    &format!("{prefix}.classification_code"),
                    "is for a consolidated document; consolidate the month instead",
                );
            }
        }
        self.code(
            &format!("{prefix}.tax_type"),
            text(&line.tax_type),
            "tax_type",
        );
        self.code(
            &format!("{prefix}.unit_code"),
            text(&line.unit_code),
            "unit",
        );
        self.code(
            &format!("{prefix}.origin_country_code"),
            text(&line.origin_country_code),
            "country",
        );
        if text(&line.tax_type) == Some(EXEMPT) && text(&line.tax_exemption_reason).is_none() {
            self.add(
                &format!("{prefix}.tax_exemption_reason"),
                "is required for an exempt line",
            );
        }
    }

    fn rate(&mut self, prefix: &str, line: &DraftLine) {
        let (Some(tax_type), Some(rate)) = (text(&line.tax_type), line.tax_rate) else {
            return;
        };
        if TAXED.contains(&tax_type) && rate <= Decimal::ZERO {
            self.add(
                &format!("{prefix}.tax_rate"),
                "must be above 0 for this tax type",
            );
        } else if UNTAXED.contains(&tax_type) && !rate.is_zero() {
            self.add(&format!("{prefix}.tax_rate"), "must be 0 for this tax type");
        }
    }

    // A consolidated invoice goes to the general public buyer, whose contact
    // the authority writes as "NA"; only consolidation builds one.
    fn consolidated(&mut self) {
        if !self.context.consolidated {
            return;
        }
        if text(&self.draft.document_type) != Some(INVOICE) {
            self.add(
                "document_type",
                "must be an invoice for a consolidated document",
            );
        } else if self
            .draft
            .buyer
            .as_ref()
            .and_then(|buyer| text(&buyer.tax_id))
            != Some(GENERAL_PUBLIC_TIN)
        {
            self.add(
                "buyer",
                "must be the general public for a consolidated document",
            );
        }
    }
}

/// A given, non-blank value.
fn text(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|text| !text.trim().is_empty())
}

/// The builder's input for a recorded document: parties from their
/// snapshots, per-tax-type subtotals from the lines, and for a note the
/// original's code number and UUID as the issue snapshotted them.
pub(crate) fn ubl(document: &Document, version: &str) -> ubl::Document {
    let issued_at =
        Timestamp::from_second(document.issued_at.as_second()).unwrap_or(document.issued_at);
    let details = &document.details;
    let foreign = document.currency_code != TAX_CURRENCY;
    ubl::Document {
        id: document.code_number.clone(),
        issue_date: issued_at.strftime("%Y-%m-%d").to_string(),
        issue_time: issued_at.strftime("%H:%M:%SZ").to_string(),
        type_code: document.document_type.clone(),
        version: version.to_owned(),
        currency_code: document.currency_code.clone(),
        tax_currency_code: Some(TAX_CURRENCY.to_owned()),
        tax_exchange_rate: foreign.then(|| ubl::ExchangeRate {
            source: document.currency_code.clone(),
            target: TAX_CURRENCY.to_owned(),
            rate: document.exchange_rate,
        }),
        invoice_period: details
            .billing_period_start
            .as_ref()
            .map(|start| ubl::Period {
                start_date: Some(start.clone()),
                end_date: details.billing_period_end.clone(),
                description: details.billing_period_description.clone(),
            }),
        billing_references: match (
            &details.original_code_number,
            &details.original_document_uuid,
        ) {
            (Some(code), Some(uuid)) => vec![ubl::BillingReference {
                id: code.clone(),
                uuid: Some(uuid.clone()),
            }],
            _ => Vec::new(),
        },
        additional_document_references: Vec::new(),
        supplier: supplier_party(&document.supplier),
        buyer: buyer_party(&document.buyer),
        delivery: None,
        payment_means: details
            .payment_means_code
            .as_ref()
            .map(|code| ubl::PaymentMeans {
                code: code.clone(),
                payee_account: None,
            }),
        payment_terms: details.payment_terms.clone(),
        prepaid_payment: None,
        allowance_charges: Vec::new(),
        tax_total: ubl::TaxTotal {
            tax_amount: document.tax_amount,
            subtotals: tax_subtotals(&document.lines),
        },
        totals: ubl::Totals {
            line_extension_amount: document.line_extension_amount,
            tax_exclusive_amount: document.tax_exclusive_amount,
            tax_inclusive_amount: document.tax_inclusive_amount,
            allowance_total_amount: None,
            charge_total_amount: None,
            payable_rounding_amount: None,
            payable_amount: document.payable_amount,
        },
        lines: document.lines.iter().map(line).collect(),
    }
}

fn or_na(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "NA".to_owned())
}

fn supplier_party(supplier: &PartySnapshot) -> ubl::Party {
    ubl::Party {
        name: supplier.name.clone().unwrap_or_default(),
        identifications: vec![
            (
                "TIN".to_owned(),
                supplier.tax_id.clone().unwrap_or_default(),
            ),
            (
                supplier
                    .registration_type
                    .clone()
                    .unwrap_or_else(|| "BRN".to_owned()),
                supplier.registration_number.clone().unwrap_or_default(),
            ),
            ("SST".to_owned(), or_na(&supplier.sales_tax_registration)),
            ("TTX".to_owned(), or_na(&supplier.tourism_tax_registration)),
        ],
        address: address(supplier),
        phone: supplier.phone.clone(),
        email: supplier.email.clone(),
        industry_code: Some((
            supplier.industry_code.clone().unwrap_or_default(),
            supplier.business_activity.clone().unwrap_or_default(),
        )),
        additional_account: None,
    }
}

// On a self-billed document the buyer is the company's own profile, whose
// registration type may be blank (a business, BRN).
fn buyer_party(buyer: &PartySnapshot) -> ubl::Party {
    ubl::Party {
        name: buyer.name.clone().unwrap_or_default(),
        identifications: vec![
            ("TIN".to_owned(), buyer.tax_id.clone().unwrap_or_default()),
            (
                buyer
                    .registration_type
                    .clone()
                    .unwrap_or_else(|| "BRN".to_owned()),
                buyer.registration_number.clone().unwrap_or_default(),
            ),
            ("SST".to_owned(), or_na(&buyer.sales_tax_registration)),
            ("TTX".to_owned(), "NA".to_owned()),
        ],
        address: address(buyer),
        phone: buyer.phone.clone(),
        email: buyer.email.clone(),
        industry_code: None,
        additional_account: None,
    }
}

fn address(party: &PartySnapshot) -> ubl::Address {
    ubl::Address {
        city: party.address.city.clone().unwrap_or_default(),
        postcode: party.address.postcode.clone(),
        state_code: party.address.state_code.clone().unwrap_or_default(),
        lines: party.address.lines.clone(),
        country_code: party.address.country_code.clone().unwrap_or_default(),
    }
}

fn tax_subtotals(lines: &[DocumentLine]) -> Vec<ubl::TaxSubtotal> {
    let mut by_type: BTreeMap<&str, Vec<&DocumentLine>> = BTreeMap::new();
    for line in lines {
        by_type
            .entry(line.tax_type.as_str())
            .or_default()
            .push(line);
    }
    by_type
        .into_iter()
        .map(|(tax_type, lines)| ubl::TaxSubtotal {
            taxable_amount: sum(&lines, |l| l.line_extension_amount),
            tax_amount: sum(&lines, |l| l.tax_amount),
            percent: None,
            per_unit: None,
            tax_type: tax_type.to_owned(),
            exemption_reason: (tax_type == EXEMPT)
                .then(|| lines.iter().find_map(|l| l.tax_exemption_reason.clone()))
                .flatten(),
        })
        .collect()
}

fn line(line: &DocumentLine) -> ubl::Line {
    let allowance = |charge: bool, reason: &str, amount: Decimal| {
        (amount > Decimal::ZERO).then(|| ubl::AllowanceCharge {
            charge,
            reason: Some(reason.to_owned()),
            multiplier: None,
            amount,
        })
    };
    ubl::Line {
        id: line.line_number.to_string(),
        quantity: line.quantity,
        unit_code: line.unit_code.clone(),
        line_extension_amount: line.line_extension_amount,
        allowance_charges: [
            allowance(false, "Discount", line.discount_amount),
            allowance(true, "Charge", line.charge_amount),
        ]
        .into_iter()
        .flatten()
        .collect(),
        tax_total: ubl::TaxTotal {
            tax_amount: line.tax_amount,
            subtotals: vec![ubl::TaxSubtotal {
                taxable_amount: line.line_extension_amount,
                tax_amount: line.tax_amount,
                percent: line.tax_rate,
                per_unit: None,
                tax_type: line.tax_type.clone(),
                exemption_reason: line.tax_exemption_reason.clone(),
            }],
        },
        classifications: [
            line.tariff_code
                .clone()
                .map(|code| ("PTC".to_owned(), code)),
            Some(("CLASS".to_owned(), line.classification_code.clone())),
        ]
        .into_iter()
        .flatten()
        .collect(),
        description: Some(line.description.clone()),
        origin_country: line.origin_country_code.clone(),
        unit_price: line.unit_price,
        subtotal: line.subtotal,
    }
}

fn sum(lines: &[&DocumentLine], f: impl Fn(&DocumentLine) -> Decimal) -> Decimal {
    lines
        .iter()
        .fold(Decimal::new(0, 2), |total, line| total + f(line))
}

fn last_of_month_plus_one(month: Date) -> Date {
    let last = month.last_of_month();
    last.tomorrow().unwrap_or(last)
}

// Midnight of `date` in Malaysian time, as the UTC instant it is.
fn local_midnight(date: Date) -> Timestamp {
    date.at(0, 0, 0, 0)
        .to_zoned(TimeZone::fixed(jiff::tz::offset(LOCAL_OFFSET_HOURS)))
        .map(|zoned| zoned.timestamp())
        .unwrap_or(Timestamp::UNIX_EPOCH)
}
