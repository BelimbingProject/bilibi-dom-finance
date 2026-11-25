//! Reads over a company's recorded documents, lines, submissions and status
//! events, the view the facade returns, and the one writer of status events
//! (Bilimbi `Bilimbi.Finance.Einvoice.Documents`). Every query names the
//! tenant and company; callers authorize first. Every write is a `mutate` of
//! the caller's audited transaction, returning each row as a `Change`.
use crate::authority::{Authority, Payload};
use crate::document::{Details, Document, DocumentLine, PartySnapshot};
use crate::draft::ValidDraft;
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use jiff::{SignedDuration, Timestamp};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_postgres::{GenericClient, Row};

const STAMP: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'";

/// Events that move a document's status; any other event records an answer,
/// a posting or a notice without changing it. `consolidated` and `released`
/// move a source in and out of a consolidated document.
pub(crate) const STATUS_EVENTS: [&str; 9] = [
    "issued",
    "submitted",
    "refused",
    "valid",
    "invalid",
    "cancelled",
    "rejected",
    "consolidated",
    "released",
];
/// The events that end an attempt: the authority answered, whatever it said.
const ANSWERS: [&str; 4] = ["submitted", "refused", "duplicate", "rate_limited"];
/// The events after which a consolidated document releases its sources.
pub(crate) const RELEASING: [&str; 3] = ["refused", "invalid", "cancelled"];
/// The statuses in which the authority holds a document as validated, so it
/// posts (a buyer's rejection request leaves it valid there).
pub(crate) const VALID_STATUSES: [&str; 2] = ["valid", "rejected"];
/// A status event keeps at most this much of a reason.
const REASON_LENGTH: usize = 300;

/// One status event, in recording order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StatusEvent {
    /// 1, 2, 3, ... per document, numbered by the database.
    pub sequence: i32,
    pub event: String,
    pub submission_id: Option<i64>,
    pub authority_status: Option<String>,
    pub submission_uid: Option<String>,
    pub document_uuid: Option<String>,
    pub long_id: Option<String>,
    pub reason: Option<String>,
    pub http_status: Option<i32>,
    /// The authority's answer as it came.
    pub response: Option<Value>,
    pub ledger_entry_id: Option<i64>,
    pub actor_type: String,
    pub actor_user_id: Option<i64>,
    pub actor_impersonator_id: Option<i64>,
    pub system_principal: Option<String>,
    pub recorded_at: Timestamp,
}

/// What an operator should look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Attention {
    /// Not submitted within the authority's outage allowance of the authority
    /// becoming unavailable.
    Overdue,
    /// The buyer asked the authority to reject it.
    RejectionRequested,
    /// Validated, without its ledger entry, and no posting was tried or the
    /// last try is older than the entry it waits for.
    Unposted,
    /// Validated, without its ledger entry, after a failed posting.
    PostingFailed,
    /// Cancelled while its entry stands, and no reversal was tried.
    Unreversed,
    /// Cancelled while its entry stands, after a failed reversal.
    ReversalFailed,
}

/// The transaction a document is the tax document of: the module that
/// recognised it in the ledger, its record in that module's terms, and the
/// entry that record posted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceLink {
    pub module: String,
    pub record: String,
    pub ledger_entry_id: i64,
}

/// A document without its lines or events, as lists show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DocumentSummary {
    pub id: i64,
    pub authority: String,
    pub environment: String,
    pub document_type: String,
    pub code_number: String,
    pub issued_at: Timestamp,
    pub currency_code: String,
    pub exchange_rate: Option<Decimal>,
    pub line_extension_amount: Decimal,
    pub tax_exclusive_amount: Decimal,
    pub tax_amount: Decimal,
    pub tax_inclusive_amount: Decimal,
    pub payable_amount: Decimal,
    pub original_document_id: Option<i64>,
    pub issued_by_user_id: i64,
    pub issued_by_impersonator_id: Option<i64>,
    pub recorded_at: Timestamp,
    pub buyer_name: Option<String>,
    /// The buyer, or the supplier of a self-billed document.
    pub counterparty_name: Option<String>,
    pub self_billed: bool,
    pub consolidated: bool,
    /// The latest status-moving event.
    pub status: String,
}

/// One document a consolidated document covers, with the consolidated
/// document's lines that carry it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConsolidatedSource {
    pub document: DocumentSummary,
    pub line_numbers: Vec<i32>,
}

/// The documents whose ledger side does not match their status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Reconciliation {
    /// Valid (or buyer-rejected) documents without their ledger entry.
    pub unposted: Vec<ReconciliationRow>,
    /// Cancelled documents whose entry stands.
    pub unreversed: Vec<ReconciliationRow>,
}

/// One document of the reconciliation, with its entries and the reason its
/// last posting or reversal failed, if one did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationRow {
    pub document: DocumentSummary,
    pub ledger_entry_id: Option<i64>,
    pub reversal_entry_id: Option<i64>,
    pub reason: Option<String>,
}

/// A recorded document as the facade returns it: the document with its lines,
/// its status events, its current `status` (the latest status-moving event)
/// and what the events say about it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocumentView {
    pub document: Document,
    /// The authority it is cleared with.
    pub authority: String,
    pub status: String,
    pub issued_by_user_id: i64,
    pub issued_by_impersonator_id: Option<i64>,
    pub recorded_at: Timestamp,
    /// The counterparty's name: the buyer, or the supplier of a self-billed
    /// document.
    pub counterparty_name: Option<String>,
    pub self_billed: bool,
    pub consolidated: bool,
    /// The latest accepted submission's UID.
    pub submission_uid: Option<String>,
    pub document_uuid: Option<String>,
    pub long_id: Option<String>,
    pub validated_at: Option<Timestamp>,
    /// The authority's public link for a validated document.
    pub validation_url: Option<String>,
    /// When the supplier's cancellation window ends.
    pub cancellable_until: Option<Timestamp>,
    pub attention: Vec<Attention>,
    /// Whether a submit job for the document is waiting or running.
    pub submission_queued: bool,
    /// The entry its validation posted, and the one that reversed it.
    pub ledger_entry_id: Option<i64>,
    pub reversal_entry_id: Option<i64>,
    /// The transactions it is the tax document of. With any, it never posts
    /// or reverses an entry itself (`recognised_by_source`).
    pub source_links: Vec<SourceLink>,
    pub recognised_by_source: bool,
    /// The latest PDF for this state and the reader’s source access.
    pub pdf_artifact_id: Option<String>,
    /// For a consolidated document, the documents it covers.
    pub sources: Vec<ConsolidatedSource>,
    /// For a covered document, the consolidated document holding it now.
    pub consolidated_by: Option<DocumentSummary>,
    pub events: Vec<StatusEvent>,
    /// Set only in `cancel_document`'s answer: the authority already held the
    /// document as cancelled when the request was due, so none was sent.
    pub already_cancelled: bool,
}

/// A recorded document and its events, before the view adds what needs
/// settings or the queue.
pub(crate) struct Recorded {
    pub document: Document,
    pub authority: String,
    pub issued_by_user_id: i64,
    pub issued_by_impersonator_id: Option<i64>,
    pub recorded_at: Timestamp,
    pub events: Vec<StatusEvent>,
    pub source_links: Vec<SourceLink>,
}

impl Recorded {
    /// The current status: the latest status-moving event.
    pub fn status(&self) -> &str {
        self.events
            .iter()
            .rev()
            .find(|event| STATUS_EVENTS.contains(&event.event.as_str()))
            .map_or("issued", |event| event.event.as_str())
    }

    fn latest(&self, name: &str) -> Option<&StatusEvent> {
        self.events.iter().rev().find(|event| event.event == name)
    }

    /// The events since the latest status-moving one, in order.
    pub fn since_status(&self) -> &[StatusEvent] {
        let start = self
            .events
            .iter()
            .rposition(|event| STATUS_EVENTS.contains(&event.event.as_str()))
            .map_or(0, |index| index + 1);
        &self.events[start..]
    }

    pub fn submission_uid(&self) -> Option<&str> {
        self.latest("submitted")?.submission_uid.as_deref()
    }

    pub fn document_uuid(&self) -> Option<&str> {
        self.latest("submitted")?.document_uuid.as_deref()
    }

    pub(crate) fn submission_sequence(&self) -> Option<i32> {
        self.latest("submitted").map(|event| event.sequence)
    }

    fn submission_events(&self) -> impl Iterator<Item = &StatusEvent> {
        let submission = self.submission_sequence().zip(self.document_uuid());
        self.events.iter().rev().filter(move |event| {
            submission.is_some_and(|(sequence, uuid)| {
                event.sequence >= sequence && event.document_uuid.as_deref() == Some(uuid)
            })
        })
    }

    fn long_id(&self) -> Option<&str> {
        self.submission_events()
            .find_map(|event| event.long_id.as_deref())
    }

    /// When the authority validated it: its own instant, or when the `valid`
    /// event was recorded.
    pub fn validated_at(&self) -> Option<Timestamp> {
        let valid = self
            .submission_events()
            .find(|event| event.event == "valid")?;
        Some(
            valid
                .response
                .as_ref()
                .and_then(|response| response.get("dateTimeValidated"))
                .and_then(Value::as_str)
                .and_then(|text| text.parse().ok())
                .unwrap_or(valid.recorded_at),
        )
    }

    pub fn cancellable_until(&self, authority: &impl Authority) -> Option<Timestamp> {
        let hours = i64::from(authority.cancellation_window_hours());
        self.validated_at()
            .map(|at| at + SignedDuration::from_hours(hours))
    }

    /// Every document UUID the document's events recorded.
    pub fn known_uuids(&self) -> Vec<String> {
        let mut uuids: Vec<String> = Vec::new();
        for uuid in self.events.iter().filter_map(|e| e.document_uuid.clone()) {
            if !uuids.contains(&uuid) {
                uuids.push(uuid);
            }
        }
        uuids
    }

    pub fn consolidated(&self) -> bool {
        self.document.details.consolidation_month.is_some()
    }

    /// The entry the document's validation posted.
    pub fn ledger_entry_id(&self) -> Option<i64> {
        self.latest("posted")?.ledger_entry_id
    }

    /// The entry that reversed it.
    pub fn reversal_entry_id(&self) -> Option<i64> {
        self.latest("posting_reversed")?.ledger_entry_id
    }

    /// Whether another module recognised its transaction in the ledger, so
    /// the document never posts or reverses an entry itself.
    pub fn recognised_by_source(&self) -> bool {
        !self.source_links.is_empty()
    }

    /// What its ledger side needs: a validated document without its entry,
    /// or a cancelled one whose entry stands; a linked one needs nothing.
    fn ledger_attention(&self, status: &str) -> Option<Attention> {
        if self.recognised_by_source() {
            return None;
        }
        let failed = |name| self.latest(name).is_some();
        let posted = self.ledger_entry_id().is_some();
        if VALID_STATUSES.contains(&status) && !posted {
            Some(if failed("posting_failed") {
                Attention::PostingFailed
            } else {
                Attention::Unposted
            })
        } else if status == "cancelled" && posted && self.reversal_entry_id().is_none() {
            Some(if failed("reversal_failed") {
                Attention::ReversalFailed
            } else {
                Attention::Unreversed
            })
        } else {
            None
        }
    }

    /// The view, given the portal's base URL for the validation link and
    /// whether a submit job is waiting.
    pub fn view(
        self,
        authority: &impl Authority,
        portal_url: &str,
        submission_queued: bool,
    ) -> DocumentView {
        let status = self.status().to_owned();
        let since_status = self.since_status();
        let mut attention = Vec::new();
        if since_status.iter().any(|event| event.event == "overdue") {
            attention.push(Attention::Overdue);
        }
        if status == "rejected" {
            attention.push(Attention::RejectionRequested);
        }
        attention.extend(self.ledger_attention(&status));
        let self_billed = authority
            .self_billed_document_types()
            .contains(&self.document.document_type.as_str());
        let counterparty = if self_billed {
            &self.document.supplier
        } else {
            &self.document.buyer
        };
        let document_uuid = self.document_uuid().map(str::to_owned);
        let long_id = self.long_id().map(str::to_owned);
        DocumentView {
            counterparty_name: counterparty.name.clone(),
            self_billed,
            consolidated: self.consolidated(),
            submission_uid: self.submission_uid().map(str::to_owned),
            validation_url: document_uuid
                .as_deref()
                .zip(long_id.as_deref())
                .filter(|_| VALID_STATUSES.contains(&status.as_str()))
                .map(|(uuid, long_id)| authority.validation_url(portal_url, uuid, long_id)),
            document_uuid,
            long_id,
            validated_at: self.validated_at(),
            cancellable_until: self.cancellable_until(authority),
            attention,
            submission_queued,
            ledger_entry_id: self.ledger_entry_id(),
            reversal_entry_id: self.reversal_entry_id(),
            recognised_by_source: self.recognised_by_source(),
            pdf_artifact_id: None,
            source_links: self.source_links,
            sources: Vec::new(),
            consolidated_by: None,
            status,
            authority: self.authority,
            issued_by_user_id: self.issued_by_user_id,
            issued_by_impersonator_id: self.issued_by_impersonator_id,
            recorded_at: self.recorded_at,
            events: self.events,
            document: self.document,
            already_cancelled: false,
        }
    }
}

fn stamp(row: &Row, column: &str) -> Result<Timestamp> {
    row.get::<_, String>(column)
        .parse()
        .map_err(|_| Error::Internal)
}

fn json_into<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Error::Internal)
}

/// A document with its ordered lines and events, or `None`.
pub(crate) async fn fetch(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<Option<Recorded>> {
    let sql = format!(
        "SELECT id, authority, environment, document_type, code_number, \
         to_char(issued_at, {STAMP}) AS issued_at, currency_code, exchange_rate, supplier, buyer, \
         details, original_document_id, line_extension_amount, tax_exclusive_amount, tax_amount, \
         tax_inclusive_amount, payable_amount, issued_by_user_id, issued_by_impersonator_id, \
         to_char(recorded_at, {STAMP}) AS recorded_at \
         FROM finance_einvoice_documents WHERE tenant_id = $1 AND company_id = $2 AND id = $3"
    );
    let Some(row) = client
        .query_opt(&sql, &[&tenant, &company_id, &document_id])
        .await?
    else {
        return Ok(None);
    };
    let lines = client
        .query(
            "SELECT line_number, description, classification_code, tariff_code, \
             origin_country_code, quantity, unit_code, unit_price, subtotal, discount_amount, \
             charge_amount, line_extension_amount, tax_type, tax_rate, tax_amount, \
             tax_exemption_reason FROM finance_einvoice_document_lines \
             WHERE tenant_id = $1 AND company_id = $2 AND document_id = $3 ORDER BY line_number",
            &[&tenant, &company_id, &document_id],
        )
        .await?
        .iter()
        .map(|line| DocumentLine {
            line_number: line.get("line_number"),
            description: line.get("description"),
            classification_code: line.get("classification_code"),
            tariff_code: line.get("tariff_code"),
            origin_country_code: line.get("origin_country_code"),
            quantity: line.get("quantity"),
            unit_code: line.get("unit_code"),
            unit_price: line.get("unit_price"),
            subtotal: line.get("subtotal"),
            discount_amount: line.get("discount_amount"),
            charge_amount: line.get("charge_amount"),
            line_extension_amount: line.get("line_extension_amount"),
            tax_type: line.get("tax_type"),
            tax_rate: line.get("tax_rate"),
            tax_amount: line.get("tax_amount"),
            tax_exemption_reason: line.get("tax_exemption_reason"),
        })
        .collect();
    let document = Document {
        id: row.get("id"),
        environment: row.get("environment"),
        document_type: row.get("document_type"),
        code_number: row.get("code_number"),
        issued_at: stamp(&row, "issued_at")?,
        currency_code: row.get("currency_code"),
        exchange_rate: row.get("exchange_rate"),
        supplier: json_into::<PartySnapshot>(row.get("supplier"))?,
        buyer: json_into::<PartySnapshot>(row.get("buyer"))?,
        details: json_into::<Details>(row.get("details"))?,
        original_document_id: row.get("original_document_id"),
        line_extension_amount: row.get("line_extension_amount"),
        tax_exclusive_amount: row.get("tax_exclusive_amount"),
        tax_amount: row.get("tax_amount"),
        tax_inclusive_amount: row.get("tax_inclusive_amount"),
        payable_amount: row.get("payable_amount"),
        lines,
    };
    Ok(Some(Recorded {
        authority: row.get("authority"),
        issued_by_user_id: row.get("issued_by_user_id"),
        issued_by_impersonator_id: row.get("issued_by_impersonator_id"),
        recorded_at: stamp(&row, "recorded_at")?,
        events: events(client, tenant, company_id, document_id).await?,
        source_links: source_links_of(client, tenant, company_id, &[document_id])
            .await?
            .into_iter()
            .map(|(_, link)| link)
            .collect(),
        document,
    }))
}

/// Each document's source links, as `(document, link)` in link order.
pub(crate) async fn source_links_of(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    document_ids: &[i64],
) -> Result<Vec<(i64, SourceLink)>> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(client
        .query(
            "SELECT document_id, source_module, source_record, ledger_entry_id \
             FROM finance_einvoice_source_links \
             WHERE tenant_id = $1 AND company_id = $2 AND document_id = ANY($3) ORDER BY id",
            &[&tenant, &company_id, &document_ids],
        )
        .await?
        .iter()
        .map(|row| {
            (
                row.get(0),
                SourceLink {
                    module: row.get(1),
                    record: row.get(2),
                    ledger_entry_id: row.get(3),
                },
            )
        })
        .collect())
}

/// The document that stands as the fiscal document of a source, or `None`:
/// its latest status is `submitted`, `valid`, `rejected` or `consolidated`,
/// or a submission was stored after its latest status event (an attempt the
/// authority may hold). The database's own rule, asked for a document not
/// recorded yet.
pub(crate) async fn standing_source_document(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    module: &str,
    record: &str,
) -> Result<Option<i64>> {
    Ok(client
        .query_opt(
            "SELECT finance_einvoice_standing_source_link(\
             ROW(0, $1, $2, 0, $3, $4, 1)::finance_einvoice_source_links, 0)",
            &[&tenant, &company_id, &module, &record],
        )
        .await?
        .ok_or(Error::Internal)?
        .get(0))
}

/// Records a source link for the document, in the transaction that records
/// the document.
pub(crate) async fn record_source_link(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document_id: i64,
    link: SourceLink,
) -> Result<()> {
    let tenant = scope.tenant();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let id: i64 = sql
                .query_one(
                    "INSERT INTO finance_einvoice_source_links (tenant_id, company_id, \
                     document_id, source_module, source_record, ledger_entry_id) \
                     VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                    &[
                        &tenant,
                        &company_id,
                        &document_id,
                        &link.module,
                        &link.record,
                        &link.ledger_entry_id,
                    ],
                )
                .await?
                .get(0);
            let change = Change {
                kind: "finance.einvoice.source_link".into(),
                id,
                old: Value::Null,
                new: json!({
                    "document_id": document_id, "source_module": link.module,
                    "source_record": link.record, "ledger_entry_id": link.ledger_entry_id,
                }),
            };
            Ok(((), vec![change]))
        })
    })
    .await
}

/// The documents' summaries with their current status, in the order of
/// `document_ids`; an ID the company lacks is left out.
pub(crate) async fn summaries(
    client: &dyn OperationClient,
    authority: &impl Authority,
    tenant: i64,
    company_id: i64,
    document_ids: &[i64],
) -> Result<Vec<DocumentSummary>> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT d.id, d.authority, d.environment, d.document_type, d.code_number, \
         to_char(d.issued_at, {STAMP}) AS issued_at, d.currency_code, d.exchange_rate, \
         d.line_extension_amount, d.tax_exclusive_amount, d.tax_amount, d.tax_inclusive_amount, \
         d.payable_amount, d.original_document_id, d.issued_by_user_id, \
         d.issued_by_impersonator_id, to_char(d.recorded_at, {STAMP}) AS recorded_at, \
         d.buyer->>'name' AS buyer_name, d.supplier->>'name' AS supplier_name, \
         d.details ? 'consolidation_month' AS consolidated, \
         (SELECT e.event FROM finance_einvoice_status_events e WHERE e.document_id = d.id \
          AND e.event = ANY($4) ORDER BY e.sequence DESC LIMIT 1) AS status \
         FROM finance_einvoice_documents d \
         WHERE d.tenant_id = $1 AND d.company_id = $2 AND d.id = ANY($3)"
    );
    let rows = client
        .query(
            &sql,
            &[&tenant, &company_id, &document_ids, &&STATUS_EVENTS[..]],
        )
        .await?;
    let self_billed = authority.self_billed_document_types();
    let mut summaries = Vec::with_capacity(rows.len());
    for id in document_ids {
        let Some(row) = rows.iter().find(|row| row.get::<_, i64>("id") == *id) else {
            continue;
        };
        let document_type: String = row.get("document_type");
        let is_self_billed = self_billed.contains(&document_type.as_str());
        let buyer_name: Option<String> = row.get("buyer_name");
        summaries.push(DocumentSummary {
            id: *id,
            authority: row.get("authority"),
            environment: row.get("environment"),
            code_number: row.get("code_number"),
            issued_at: stamp(row, "issued_at")?,
            currency_code: row.get("currency_code"),
            exchange_rate: row.get("exchange_rate"),
            line_extension_amount: row.get("line_extension_amount"),
            tax_exclusive_amount: row.get("tax_exclusive_amount"),
            tax_amount: row.get("tax_amount"),
            tax_inclusive_amount: row.get("tax_inclusive_amount"),
            payable_amount: row.get("payable_amount"),
            original_document_id: row.get("original_document_id"),
            issued_by_user_id: row.get("issued_by_user_id"),
            issued_by_impersonator_id: row.get("issued_by_impersonator_id"),
            recorded_at: stamp(row, "recorded_at")?,
            counterparty_name: if is_self_billed {
                row.get("supplier_name")
            } else {
                buyer_name.clone()
            },
            buyer_name,
            self_billed: is_self_billed,
            consolidated: row.get("consolidated"),
            status: row
                .get::<_, Option<String>>("status")
                .unwrap_or_else(|| "issued".into()),
            document_type,
        });
    }
    Ok(summaries)
}

/// The documents whose ledger side does not match their status, newest
/// first: valid (or buyer-rejected) documents without their entry, and
/// cancelled documents whose entry stands, each with the reason its last
/// posting or reversal failed. A document linked to its source never posts,
/// so it is never listed.
pub(crate) async fn reconciliation(
    client: &dyn OperationClient,
    authority: &impl Authority,
    tenant: i64,
    company_id: i64,
) -> Result<Reconciliation> {
    let rows = client
        .query(
            "WITH status AS ( \
               SELECT DISTINCT ON (document_id) document_id, event FROM finance_einvoice_status_events \
               WHERE tenant_id = $1 AND company_id = $2 AND event = ANY($3) \
               ORDER BY document_id, sequence DESC), \
             ledger AS ( \
               SELECT document_id, \
                 max(ledger_entry_id) FILTER (WHERE event = 'posted') AS posted, \
                 max(ledger_entry_id) FILTER (WHERE event = 'posting_reversed') AS reversed, \
                 (array_agg(reason ORDER BY sequence DESC) \
                   FILTER (WHERE event = 'posting_failed'))[1] AS posting_failed, \
                 (array_agg(reason ORDER BY sequence DESC) \
                   FILTER (WHERE event = 'reversal_failed'))[1] AS reversal_failed \
               FROM finance_einvoice_status_events \
               WHERE tenant_id = $1 AND company_id = $2 \
               GROUP BY document_id) \
             SELECT s.document_id, s.event, l.posted, l.reversed, l.posting_failed, \
               l.reversal_failed \
             FROM status s JOIN ledger l USING (document_id) \
             WHERE NOT EXISTS (SELECT 1 FROM finance_einvoice_source_links k \
                               WHERE k.document_id = s.document_id) \
               AND ((s.event = ANY($4) AND l.posted IS NULL) \
                 OR (s.event = 'cancelled' AND l.posted IS NOT NULL AND l.reversed IS NULL)) \
             ORDER BY s.document_id DESC",
            &[
                &tenant,
                &company_id,
                &&STATUS_EVENTS[..],
                &&VALID_STATUSES[..],
            ],
        )
        .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
    let summaries = summaries(client, authority, tenant, company_id, &ids).await?;
    let mut reconciliation = Reconciliation::default();
    for (row, document) in rows.iter().zip(summaries) {
        let cancelled = row.get::<_, String>(1) == "cancelled";
        let entry = ReconciliationRow {
            document,
            ledger_entry_id: row.get(2),
            reversal_entry_id: row.get(3),
            reason: row.get(if cancelled { 5 } else { 4 }),
        };
        if cancelled {
            reconciliation.unreversed.push(entry);
        } else {
            reconciliation.unposted.push(entry);
        }
    }
    Ok(reconciliation)
}

/// A document's events in recording order.
pub(crate) async fn events(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<Vec<StatusEvent>> {
    let sql = format!(
        "SELECT sequence, event, submission_id, authority_status, submission_uid, document_uuid, \
         long_id, reason, http_status, response, ledger_entry_id, actor_type, actor_user_id, \
         actor_impersonator_id, system_principal, to_char(recorded_at, {STAMP}) AS recorded_at \
         FROM finance_einvoice_status_events \
         WHERE tenant_id = $1 AND company_id = $2 AND document_id = $3 ORDER BY sequence"
    );
    client
        .query(&sql, &[&tenant, &company_id, &document_id])
        .await?
        .iter()
        .map(|row| {
            Ok(StatusEvent {
                sequence: row.get("sequence"),
                event: row.get("event"),
                submission_id: row.get("submission_id"),
                authority_status: row.get("authority_status"),
                submission_uid: row.get("submission_uid"),
                document_uuid: row.get("document_uuid"),
                long_id: row.get("long_id"),
                reason: row.get("reason"),
                http_status: row.get("http_status"),
                response: row.get("response"),
                ledger_entry_id: row.get("ledger_entry_id"),
                actor_type: row.get("actor_type"),
                actor_user_id: row.get("actor_user_id"),
                actor_impersonator_id: row.get("actor_impersonator_id"),
                system_principal: row.get("system_principal"),
                recorded_at: stamp(row, "recorded_at")?,
            })
        })
        .collect()
}

/// The current status of each document, by ID.
pub(crate) async fn statuses(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    document_ids: &[i64],
) -> Result<Vec<(i64, String)>> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(client
        .query(
            "SELECT DISTINCT ON (document_id) document_id, event \
             FROM finance_einvoice_status_events \
             WHERE tenant_id = $1 AND company_id = $2 AND document_id = ANY($3) \
               AND event = ANY($4) \
             ORDER BY document_id, sequence DESC",
            &[&tenant, &company_id, &document_ids, &&STATUS_EVENTS[..]],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// The document's current status, or `None` for a document the company does
/// not have.
pub(crate) async fn status(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<Option<String>> {
    Ok(statuses(client, tenant, company_id, &[document_id])
        .await?
        .pop()
        .map(|(_, status)| status))
}

/// Whether the company already issued `code_number` in the environment.
pub(crate) async fn code_number_taken(
    client: &dyn OperationClient,
    company_id: i64,
    environment: &str,
    code_number: &str,
) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT EXISTS (SELECT 1 FROM finance_einvoice_documents \
             WHERE company_id = $1 AND environment = $2 AND code_number = $3)",
            &[&company_id, &environment, &code_number],
        )
        .await?
        .ok_or(Error::Internal)?
        .get(0))
}

/// The notes of the company that name the document as their original.
pub(crate) async fn notes_of(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<Vec<i64>> {
    Ok(client
        .query(
            "SELECT id FROM finance_einvoice_documents \
             WHERE tenant_id = $1 AND company_id = $2 AND original_document_id = $3",
            &[&tenant, &company_id, &document_id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Whether another document that stands links one of this document's
/// sources (the documents it covers do not count). The trigger on submissions
/// refuses to store one in that case, so this is asked before storing, under
/// the company lock.
pub(crate) async fn source_taken(
    client: &(impl GenericClient + Sync),
    document_id: i64,
) -> Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM finance_einvoice_source_links l \
             WHERE l.document_id = $1 \
               AND finance_einvoice_standing_source_link(l, $1) IS NOT NULL)",
            &[&document_id],
        )
        .await?
        .get(0))
}

/// One stored submission attempt.
#[derive(Clone, Debug)]
pub(crate) struct Attempt {
    pub id: i64,
    pub recorded_at: Timestamp,
}

/// The submissions stored since the document last became submittable whose
/// final answer is missing (a crash, an outage, or a search that could not
/// answer): any of them may have been accepted. Newest first.
pub(crate) async fn unresolved_attempts(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    recorded: &Recorded,
) -> Result<Vec<Attempt>> {
    let Some(last_status) = recorded
        .events
        .iter()
        .rev()
        .find(|event| STATUS_EVENTS.contains(&event.event.as_str()))
    else {
        return Ok(Vec::new());
    };
    let settled: Vec<i64> = recorded
        .since_status()
        .iter()
        .filter(|event| ANSWERS.contains(&event.event.as_str()))
        .filter_map(|event| event.submission_id)
        .collect();
    let sql = format!(
        "SELECT id, to_char(recorded_at, {STAMP}) AS recorded_at \
         FROM finance_einvoice_submissions \
         WHERE tenant_id = $1 AND company_id = $2 AND document_id = $3 \
           AND recorded_at > (SELECT recorded_at FROM finance_einvoice_status_events \
                              WHERE document_id = $3 AND sequence = $4) \
         ORDER BY id DESC"
    );
    client
        .query(
            &sql,
            &[
                &tenant,
                &company_id,
                &recorded.document.id,
                &last_status.sequence,
            ],
        )
        .await?
        .iter()
        .filter(|row| !settled.contains(&row.get::<_, i64>("id")))
        .map(|row| {
            Ok(Attempt {
                id: row.get("id"),
                recorded_at: stamp(row, "recorded_at")?,
            })
        })
        .collect()
}

/// Who a recorded row names: the scope's user and any impersonator.
fn actor(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

/// A status event to record.
#[derive(Clone, Debug, Default)]
pub(crate) struct NewEvent {
    pub event: &'static str,
    pub submission_id: Option<i64>,
    pub authority_status: Option<&'static str>,
    pub submission_uid: Option<String>,
    pub document_uuid: Option<String>,
    pub long_id: Option<String>,
    pub reason: Option<String>,
    pub http_status: Option<u16>,
    pub response: Option<Value>,
    /// The ledger entry a `posted` or `posting_reversed` event names.
    pub ledger_entry_id: Option<i64>,
}

impl NewEvent {
    pub fn new(event: &'static str) -> Self {
        Self {
            event,
            ..Self::default()
        }
    }
}

/// Records a status event for the document as the scope's user. A
/// consolidated document the authority refused, found invalid or cancelled
/// releases the documents it covered, in the same transaction.
pub(crate) async fn record_event(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document_id: i64,
    event: NewEvent,
) -> Result<()> {
    let releasing = RELEASING.contains(&event.event);
    insert_event(tx, scope, company_id, document_id, event).await?;
    if releasing {
        release_sources(tx, scope, company_id, document_id).await?;
    }
    Ok(())
}

async fn insert_event(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document_id: i64,
    event: NewEvent,
) -> Result<()> {
    let tenant = scope.tenant();
    let (user, impersonator) = actor(scope);
    let reason = event
        .reason
        .map(|reason| reason.chars().take(REASON_LENGTH).collect::<String>());
    let http_status = event.http_status.map(i32::from);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    "INSERT INTO finance_einvoice_status_events (tenant_id, company_id, \
                     document_id, submission_id, event, authority_status, submission_uid, \
                     document_uuid, long_id, reason, http_status, response, actor_type, \
                     actor_user_id, actor_impersonator_id, ledger_entry_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'user', $13, $14, \
                     $15) RETURNING id, sequence",
                    &[
                        &tenant,
                        &company_id,
                        &document_id,
                        &event.submission_id,
                        &event.event,
                        &event.authority_status,
                        &event.submission_uid,
                        &event.document_uuid,
                        &event.long_id,
                        &reason,
                        &http_status,
                        &event.response,
                        &user,
                        &impersonator,
                        &event.ledger_entry_id,
                    ],
                )
                .await?;
            let change = Change {
                kind: "finance.einvoice.status_event".into(),
                id: row.get("id"),
                old: Value::Null,
                // The authority's answer stays in the row; the audit names
                // what was recorded.
                new: json!({
                    "document_id": document_id,
                    "sequence": row.get::<_, i32>("sequence"),
                    "event": event.event,
                    "submission_id": event.submission_id,
                    "authority_status": event.authority_status,
                    "submission_uid": event.submission_uid,
                    "document_uuid": event.document_uuid,
                    "long_id": event.long_id,
                    "reason": reason,
                    "http_status": http_status,
                    "ledger_entry_id": event.ledger_entry_id,
                }),
            };
            Ok(((), vec![change]))
        })
    })
    .await
}

/// Records `released` on each source a consolidated document holds, once the
/// authority refused it, found it invalid or cancelled it. A source already
/// released, or never consolidated, is left alone.
async fn release_sources(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document_id: i64,
) -> Result<()> {
    let tenant = scope.tenant();
    let sources: Vec<i64> = tx
        .sql()
        .query(
            "SELECT DISTINCT source_document_id FROM finance_einvoice_consolidated_sources \
             WHERE tenant_id = $1 AND company_id = $2 AND document_id = $3",
            &[&tenant, &company_id, &document_id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    if sources.is_empty() {
        return Ok(());
    }
    let row = tx
        .sql()
        .query_one(
            "SELECT code_number, (SELECT event FROM finance_einvoice_status_events \
             WHERE document_id = $1 ORDER BY sequence DESC LIMIT 1) \
             FROM finance_einvoice_documents WHERE id = $1",
            &[&document_id],
        )
        .await?;
    let (code_number, event): (String, String) = (row.get(0), row.get(1));
    let mut statuses = statuses(tx.sql(), tenant, company_id, &sources).await?;
    statuses.sort();
    for (source_id, status) in statuses {
        if status == "consolidated" {
            let mut released = NewEvent::new("released");
            released.reason = Some(format!("{code_number} is {event}"));
            insert_event(tx, scope, company_id, source_id, released).await?;
        }
    }
    Ok(())
}

/// Stores a submission's exact bytes before the authority sees them, with
/// their hex SHA-256; the database checks the hash against the bytes.
pub(crate) async fn store_submission(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document: &Document,
    payload: &Payload,
    sha256: String,
) -> Result<Attempt> {
    let tenant = scope.tenant();
    let (format, version, bytes) = (
        payload.format,
        payload.version.clone(),
        payload.bytes.clone(),
    );
    let (user, impersonator) = actor(scope);
    let (document_id, environment) = (document.id, document.environment.clone());
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    &format!(
                        "INSERT INTO finance_einvoice_submissions (tenant_id, company_id, \
                         document_id, environment, format, document_version, payload, \
                         payload_sha256, actor_type, actor_user_id, actor_impersonator_id) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'user', $9, $10) \
                         RETURNING id, to_char(recorded_at, {STAMP}) AS recorded_at"
                    ),
                    &[
                        &tenant,
                        &company_id,
                        &document_id,
                        &environment,
                        &format,
                        &version,
                        &bytes,
                        &sha256,
                        &user,
                        &impersonator,
                    ],
                )
                .await?;
            let attempt = Attempt {
                id: row.get("id"),
                recorded_at: stamp(&row, "recorded_at")?,
            };
            let change = Change {
                kind: "finance.einvoice.submission".into(),
                id: attempt.id,
                old: Value::Null,
                new: json!({
                    "document_id": document_id, "environment": environment, "format": format,
                    "document_version": version, "payload_sha256": sha256,
                }),
            };
            Ok((attempt, vec![change]))
        })
    })
    .await
}

/// Records an issued document, its lines and its `issued` event: the
/// company's profile is the supplier, or the buyer of a self-billed document.
pub(crate) async fn insert_document(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    authority: &'static str,
    environment: String,
    draft: ValidDraft,
    supplier: PartySnapshot,
    buyer: PartySnapshot,
    details: Details,
) -> Result<i64> {
    let tenant = scope.tenant();
    let (user, impersonator) = actor(scope);
    let document_id = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let total = draft.line_extension_amount + draft.tax_amount;
                let issued_at = draft.issued_at.to_string();
                let supplier = serde_json::to_value(&supplier).map_err(|_| Error::Internal)?;
                let buyer = serde_json::to_value(&buyer).map_err(|_| Error::Internal)?;
                let details = serde_json::to_value(&details).map_err(|_| Error::Internal)?;
                let row = sql
                    .query_one(
                        "INSERT INTO finance_einvoice_documents (tenant_id, company_id, \
                         authority, environment, document_type, code_number, issued_at, \
                         currency_code, exchange_rate, supplier, buyer, details, \
                         original_document_id, line_extension_amount, tax_exclusive_amount, \
                         tax_amount, tax_inclusive_amount, payable_amount, issued_by_user_id, \
                         issued_by_impersonator_id) \
                         VALUES ($1, $2, $3, $4, $5, $6, \
                         ($7::text::timestamptz AT TIME ZONE 'UTC')::timestamp(0), \
                         $8, $9, $10, $11, $12, $13, $14, $14, $15, $16, $16, $17, $18) \
                         RETURNING id",
                        &[
                            &tenant,
                            &company_id,
                            &authority,
                            &environment,
                            &draft.document_type,
                            &draft.code_number,
                            &issued_at,
                            &draft.currency_code,
                            &draft.exchange_rate,
                            &supplier,
                            &buyer,
                            &details,
                            &draft.original_document_id,
                            &draft.line_extension_amount,
                            &draft.tax_amount,
                            &total,
                            &user,
                            &impersonator,
                        ],
                    )
                    .await?;
                let id: i64 = row.get(0);
                let mut changes = vec![Change {
                    kind: "finance.einvoice.document".into(),
                    id,
                    old: Value::Null,
                    new: json!({
                        "authority": authority, "environment": environment,
                        "document_type": draft.document_type, "code_number": draft.code_number,
                        "issued_at": issued_at, "currency_code": draft.currency_code,
                        "original_document_id": draft.original_document_id,
                        "line_extension_amount": draft.line_extension_amount.to_string(),
                        "tax_amount": draft.tax_amount.to_string(),
                        "payable_amount": total.to_string(),
                    }),
                }];
                for (number, line) in (1_i32..).zip(&draft.lines) {
                    let row = sql
                        .query_one(
                            "INSERT INTO finance_einvoice_document_lines (tenant_id, company_id, \
                             document_id, line_number, description, classification_code, \
                             tariff_code, origin_country_code, quantity, unit_code, unit_price, \
                             subtotal, discount_amount, charge_amount, line_extension_amount, \
                             tax_type, tax_rate, tax_amount, tax_exemption_reason) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
                             $14, $15, $16, $17, $18, $19) RETURNING id",
                            &[
                                &tenant,
                                &company_id,
                                &id,
                                &number,
                                &line.description,
                                &line.classification_code,
                                &line.tariff_code,
                                &line.origin_country_code,
                                &line.quantity,
                                &line.unit_code,
                                &line.unit_price,
                                &line.subtotal,
                                &line.discount_amount,
                                &line.charge_amount,
                                &line.line_extension_amount,
                                &line.tax_type,
                                &line.tax_rate,
                                &line.tax_amount,
                                &line.tax_exemption_reason,
                            ],
                        )
                        .await?;
                    changes.push(Change {
                        kind: "finance.einvoice.document_line".into(),
                        id: row.get(0),
                        old: Value::Null,
                        new: json!({
                            "document_id": id, "line_number": number,
                            "line_extension_amount": line.line_extension_amount.to_string(),
                            "tax_amount": line.tax_amount.to_string(),
                        }),
                    });
                }
                Ok((id, changes))
            })
        })
        .await?;
    insert_event(tx, scope, company_id, document_id, NewEvent::new("issued")).await?;
    Ok(document_id)
}
