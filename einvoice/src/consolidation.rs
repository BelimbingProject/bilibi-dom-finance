//! A consolidated document: one document to the authority for a month's sales
//! that no buyer asked an individual document for (Bilimbi
//! `Bilimbi.Finance.Einvoice.Consolidation`, `consolidation_candidates/3` and
//! `issue_consolidated_document/3`).
//!
//! The sources are the company's recorded documents issued in the month (in
//! the authority's calendar, `Authority::consolidation_window`) in the current
//! environment that were never submitted on their own, are not held by another
//! consolidated document, are not waiting in a submit job, and pass the
//! authority's own limits (`Authority::consolidation_refusal`). They are
//! grouped into a line per source and tax, each line's amount and tax being the
//! exact sums of that source's lines of the tax, so the consolidated totals
//! equal the sources' totals; the authority shapes the document itself
//! (`Authority::consolidated_document`).
//!
//! Recording one writes a row per source and line it covers and a
//! `consolidated` status event on each source, which then cannot be submitted
//! on its own (the database refuses that as well). When the authority refuses
//! the consolidated document, finds it invalid, or it is cancelled, its sources
//! are released (`released`, in `documents::record_event`) and may be
//! consolidated again; the document itself, whose bytes and issue date never
//! change, is never submitted again.
use crate::authority::{Authority, ValidationContext};
use crate::code_lists::{self, Codes};
use crate::document::{Details, PartySnapshot};
use crate::documents::{
    self, ConsolidatedSource, DocumentSummary, DocumentView, NewEvent, Recorded, SourceLink,
    RELEASING,
};
use crate::{authorize, supplier_profile, Einvoice, EinvoiceError, Result, ISSUE, VIEW};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use jiff::Timestamp;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;

/// A source's status before it is consolidated: recorded, or released by a
/// consolidated document that was cancelled or found invalid.
const OPEN_STATUSES: [&str; 2] = ["issued", "released"];

/// What a caller asks to consolidate: the month (any date in it), the
/// company's code number for the document, and optionally the eligible
/// documents to cover (default every one the candidates list).
#[derive(Clone, Debug, Default)]
pub struct ConsolidationRequest {
    pub month: Date,
    pub code_number: Option<String>,
    pub document_ids: Option<Vec<i64>>,
}

/// What the authority shapes a consolidated document from.
#[derive(Clone, Debug)]
pub struct ConsolidatedRequest {
    pub code_number: Option<String>,
    pub issued_at: Timestamp,
    /// The first day of the month it reports.
    pub month: Date,
    pub lines: Vec<ConsolidatedLine>,
}

/// One line of a consolidated document: one source's lines of one tax.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConsolidatedLine {
    /// The source's code number.
    pub description: String,
    pub amount: Decimal,
    pub tax_amount: Decimal,
    pub tax_type: String,
    pub tax_rate: Option<Decimal>,
    pub tax_exemption_reason: Option<String>,
    pub source_ids: Vec<i64>,
}

/// A document the authority's limits keep out of a consolidated document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefusedCandidate {
    pub document: DocumentSummary,
    pub reason: String,
}

/// What a consolidated document for a month would cover in the company's
/// current environment: the eligible documents, oldest first, those the
/// authority's limits keep out, and the eligible documents' totals. `from`
/// and `until` bound the issue instants it covers; it may be issued from
/// `until` until `closes_at` (`open` says whether that is `now`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidates {
    /// The first day of the month.
    pub month: Date,
    pub environment: String,
    pub from: Timestamp,
    pub until: Timestamp,
    pub closes_at: Timestamp,
    pub open: bool,
    pub documents: Vec<DocumentSummary>,
    pub refused: Vec<RefusedCandidate>,
    pub line_extension_amount: Decimal,
    pub tax_amount: Decimal,
    pub payable_amount: Decimal,
}

/// Whether a consolidated document for the window may be issued at `now`.
fn open(window: &crate::ConsolidationWindow, now: Timestamp) -> Result<()> {
    if now < window.until {
        Err(EinvoiceError::PeriodNotEnded)
    } else if now >= window.closes_at {
        Err(EinvoiceError::ConsolidationWindowClosed)
    } else {
        Ok(())
    }
}

/// The window's documents a consolidated document may cover, oldest first,
/// and those the authority's limits keep out with the reason.
async fn candidates(
    client: &(impl GenericClient + Sync),
    authority: &impl Authority,
    tenant: i64,
    company_id: i64,
    environment: &str,
    window: &crate::ConsolidationWindow,
) -> Result<(Vec<Recorded>, Vec<(Recorded, String)>)> {
    let rows = client
        .query(
            "SELECT d.id FROM finance_einvoice_documents d \
             WHERE d.tenant_id = $1 AND d.company_id = $2 AND d.environment = $3 \
               AND d.issued_at >= ($4::text::timestamptz AT TIME ZONE 'UTC') \
               AND d.issued_at < ($5::text::timestamptz AT TIME ZONE 'UTC') \
               AND NOT d.details ? 'consolidation_month' \
               AND NOT EXISTS (SELECT 1 FROM finance_einvoice_submissions s \
                               WHERE s.document_id = d.id) \
               AND NOT EXISTS (SELECT 1 FROM base_queue_jobs j \
                               WHERE j.worker_id = $6 \
                                 AND j.state IN ('available', 'scheduled', 'executing', 'retryable') \
                                 AND (j.args->>'company_id')::bigint = d.company_id \
                                 AND (j.args->>'document_id')::bigint = d.id) \
             ORDER BY d.issued_at, d.id",
            &[
                &tenant,
                &company_id,
                &environment,
                &window.from.to_string(),
                &window.until.to_string(),
                &crate::SUBMIT_WORKER,
            ],
        )
        .await
        .map_err(Error::from)?;
    let (mut eligible, mut refused) = (Vec::new(), Vec::new());
    for row in rows {
        let Some(recorded) = documents::fetch(client, tenant, company_id, row.get(0)).await? else {
            continue;
        };
        if !OPEN_STATUSES.contains(&recorded.status()) {
            continue;
        }
        match authority.consolidation_refusal(&recorded.document) {
            None => eligible.push(recorded),
            Some(reason) => refused.push((recorded, reason)),
        }
    }
    Ok((eligible, refused))
}

/// The consolidated document's lines from its sources, in order: one per
/// source and tax (type, rate and exemption reason), with the exact sums.
fn group(sources: &[Recorded]) -> Vec<ConsolidatedLine> {
    let mut lines = Vec::new();
    for source in sources {
        let mut by_tax: BTreeMap<(String, Decimal, Option<String>), (Decimal, Decimal)> =
            BTreeMap::new();
        for line in &source.document.lines {
            let rate = line.tax_rate.unwrap_or_default().normalize();
            let key = (
                line.tax_type.clone(),
                rate,
                line.tax_exemption_reason.clone(),
            );
            let total = by_tax.entry(key).or_default();
            total.0 += line.line_extension_amount;
            total.1 += line.tax_amount;
        }
        for ((tax_type, tax_rate, reason), (amount, tax_amount)) in by_tax {
            lines.push(ConsolidatedLine {
                description: source.document.code_number.clone(),
                amount,
                tax_amount,
                tax_type,
                tax_rate: Some(tax_rate),
                tax_exemption_reason: reason,
                source_ids: vec![source.document.id],
            });
        }
    }
    lines
}

/// A consolidated document's sources (each with the lines carrying it), and
/// the consolidated document that holds a covered document now.
pub(crate) async fn view(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<(Vec<ConsolidatedSource>, Option<DocumentSummary>)> {
    let covered = client
        .query(
            "SELECT source_document_id, array_agg(line_number ORDER BY line_number) \
             FROM finance_einvoice_consolidated_sources \
             WHERE tenant_id = $1 AND company_id = $2 AND document_id = $3 \
             GROUP BY source_document_id",
            &[&tenant, &company_id, &document_id],
        )
        .await?;
    let holders: Vec<i64> = client
        .query(
            "SELECT DISTINCT document_id FROM finance_einvoice_consolidated_sources \
             WHERE tenant_id = $1 AND company_id = $2 AND source_document_id = $3 \
             ORDER BY document_id",
            &[&tenant, &company_id, &document_id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let authority = Einvoice::authority();
    let ids: Vec<i64> = covered.iter().map(|row| row.get(0)).collect();
    let mut sources: Vec<ConsolidatedSource> =
        documents::summaries(client, &authority, tenant, company_id, &ids)
            .await?
            .into_iter()
            .map(|document| {
                let line_numbers = covered
                    .iter()
                    .find(|row| row.get::<_, i64>(0) == document.id)
                    .map(|row| row.get::<_, Vec<i32>>(1))
                    .unwrap_or_default();
                ConsolidatedSource {
                    document,
                    line_numbers,
                }
            })
            .collect();
    sources.sort_by_key(|source| (source.document.issued_at, source.document.id));
    let holder = documents::summaries(client, &authority, tenant, company_id, &holders)
        .await?
        .into_iter()
        .find(|holder| !RELEASING.contains(&holder.status.as_str()));
    Ok((sources, holder))
}

/// Starts at 0.00, as Bilimbi's reduce does, so an empty month reads "0.00".
fn sum(documents: &[Recorded], field: impl Fn(&Recorded) -> Decimal) -> Decimal {
    documents
        .iter()
        .map(field)
        .fold(Decimal::new(0, 2), |total, amount| total + amount)
}

impl Einvoice {
    /// What a consolidated document for the month containing `month` would
    /// cover in the company's current environment (`finance.einvoice.view`),
    /// judged at `now` (pass the current instant).
    pub async fn consolidation_candidates(
        &self,
        scope: &Scope,
        company_id: i64,
        month: Date,
        now: Timestamp,
    ) -> Result<Candidates> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let authority = Self::authority();
        let window = authority.consolidation_window(month);
        let environment = self.environment(&**client, scope, company_id).await?;
        let tenant = scope.tenant();
        let (eligible, refused) = candidates(
            &**client,
            &authority,
            tenant,
            company_id,
            &environment,
            &window,
        )
        .await?;
        let ids: Vec<i64> = eligible.iter().map(|d| d.document.id).collect();
        let refused_ids: Vec<i64> = refused.iter().map(|(d, _)| d.document.id).collect();
        let refused_summaries =
            documents::summaries(&**client, &authority, tenant, company_id, &refused_ids).await?;
        Ok(Candidates {
            month: month.first_of_month(),
            environment,
            from: window.from,
            until: window.until,
            closes_at: window.closes_at,
            open: open(&window, now).is_ok(),
            documents: documents::summaries(&**client, &authority, tenant, company_id, &ids)
                .await?,
            refused: refused_summaries
                .into_iter()
                .zip(refused.iter())
                .map(|(document, (_, reason))| RefusedCandidate {
                    document,
                    reason: reason.clone(),
                })
                .collect(),
            line_extension_amount: sum(&eligible, |d| d.document.line_extension_amount),
            tax_amount: sum(&eligible, |d| d.document.tax_amount),
            payable_amount: sum(&eligible, |d| d.document.payable_amount),
        })
    }

    /// Issues the consolidated document for a month's sales no buyer asked an
    /// individual document for (`finance.einvoice.documents.issue`), at `now`
    /// (pass the current instant; the database stamps when it was recorded):
    /// the month must have ended and the authority's window not yet closed
    /// (`PeriodNotEnded`, `ConsolidationWindowClosed`). Its lines are one per
    /// covered document and tax; it records which documents each line covers
    /// and a `consolidated` event on each, so none is consolidated twice or
    /// submitted on its own while it holds them. It is submitted, polled,
    /// posted (dated the month's last day) and cancelled like any document,
    /// and is the audit action `finance.einvoice.document.consolidated`.
    /// Sources issued with a source link pass their links on, and the
    /// consolidated document then never posts either; linked and unlinked
    /// sources do not consolidate together (`MixedSourceLinks`). Other
    /// refusals: `NothingToConsolidate`, `NotConsolidatable(id)` for a named
    /// document that is not eligible, `InvalidOption` for an empty or
    /// non-positive list, `CodeNumberTaken`, and `Invalid`.
    pub async fn issue_consolidated_document(
        &self,
        scope: &Scope,
        company_id: i64,
        request: &ConsolidationRequest,
        now: Timestamp,
    ) -> Result<DocumentView> {
        if request
            .document_ids
            .as_ref()
            .is_some_and(|ids| ids.is_empty() || ids.iter().any(|id| *id < 1))
        {
            return Err(EinvoiceError::InvalidOption);
        }
        let authority = Self::authority();
        let now = Timestamp::from_second(now.as_second()).map_err(|_| Error::Internal)?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                ISSUE,
                "finance.einvoice.issue_consolidated_document",
            )
            .await?;
        let tenant = scope.tenant();
        let month = request.month.first_of_month();
        let window = authority.consolidation_window(month);
        open(&window, now)?;
        let environment = self.environment(tx.sql(), scope, company_id).await?;
        let (eligible, _refused) = candidates(
            tx.sql(),
            &authority,
            tenant,
            company_id,
            &environment,
            &window,
        )
        .await?;
        let sources: Vec<Recorded> = match &request.document_ids {
            None if eligible.is_empty() => return Err(EinvoiceError::NothingToConsolidate),
            None => eligible,
            Some(ids) => {
                if let Some(id) = ids
                    .iter()
                    .find(|id| !eligible.iter().any(|d| d.document.id == **id))
                {
                    return Err(EinvoiceError::NotConsolidatable(*id));
                }
                eligible
                    .into_iter()
                    .filter(|d| ids.contains(&d.document.id))
                    .collect()
            }
        };
        let linked = sources
            .iter()
            .filter(|source| source.recognised_by_source())
            .count();
        if linked != 0 && linked != sources.len() {
            return Err(EinvoiceError::MixedSourceLinks);
        }
        let lines = group(&sources);
        let name = bilibi_company::identity(tx.sql(), scope, company_id)
            .await?
            .ok_or(EinvoiceError::CompanyNotFound)?
            .display_name;
        let keys: Vec<String> = crate::configuration::SUPPLIER_FIELDS
            .iter()
            .map(|field| crate::configuration::supplier_key(field))
            .collect();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let target = crate::clearance::company_target(company_id)?;
        let values = self
            .settings
            .resolve_many(tx.sql(), scope, target, &keys)
            .await?;
        let profile = supplier_profile(&values, name);
        let mut codes: BTreeMap<String, Codes> = BTreeMap::new();
        for list in code_lists::lists(authority.id()) {
            codes.insert(
                list.to_owned(),
                code_lists::codes(tx.sql(), tenant, company_id, authority.id(), list).await?,
            );
        }
        let draft = authority.consolidated_document(&ConsolidatedRequest {
            code_number: request.code_number.clone(),
            issued_at: now,
            month,
            lines: lines.clone(),
        });
        let context = ValidationContext {
            supplier: &profile,
            codes: &codes,
            original: None,
            consolidated: true,
        };
        let valid = draft
            .validate(now, &authority, &context)
            .map_err(EinvoiceError::Invalid)?;
        if documents::code_number_taken(tx.sql(), company_id, &environment, &valid.code_number)
            .await?
        {
            return Err(EinvoiceError::CodeNumberTaken);
        }
        let buyer = valid
            .buyer
            .as_ref()
            .ok_or(EinvoiceError::Failed(Error::Internal))?;
        let (supplier, buyer) = (
            PartySnapshot::of_profile(&profile),
            PartySnapshot::of_party(buyer, false),
        );
        let month_text = month.strftime("%Y-%m").to_string();
        let details = Details {
            payment_means_code: valid.payment_means_code.clone(),
            payment_terms: valid.payment_terms.clone(),
            billing_period_start: valid.billing_period_start.map(|date| date.to_string()),
            billing_period_end: valid.billing_period_end.map(|date| date.to_string()),
            billing_period_description: valid.billing_period_description.clone(),
            original_code_number: None,
            original_document_uuid: None,
            consolidation_month: Some(month_text.clone()),
        };
        let code_number = valid.code_number.clone();
        let document_id = documents::insert_document(
            &mut tx,
            scope,
            company_id,
            authority.id(),
            environment.clone(),
            valid,
            supplier,
            buyer,
            details,
        )
        .await?;
        record_sources(
            &mut tx,
            scope,
            company_id,
            document_id,
            &code_number,
            &lines,
        )
        .await?;
        let links: Vec<SourceLink> = sources
            .iter()
            .flat_map(|source| source.source_links.iter().cloned())
            .collect();
        for link in links {
            documents::record_source_link(&mut tx, scope, company_id, document_id, link).await?;
        }
        crate::clearance::action(
            &tx,
            "finance.einvoice.document.consolidated",
            json!({
                "document_id": document_id, "code_number": code_number,
                "environment": environment, "month": month_text,
                "document_count": sources.len(),
            }),
        )
        .await?;
        tx.commit().await?;
        drop(client);
        self.view(scope, company_id, document_id).await
    }
}

/// Records which sources each line of the consolidated document carries, and
/// a `consolidated` status event on each source, inside the transaction that
/// recorded the document.
async fn record_sources(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    document_id: i64,
    code_number: &str,
    lines: &[ConsolidatedLine],
) -> Result<()> {
    let tenant = scope.tenant();
    let rows: Vec<(i64, i32)> = (1_i32..)
        .zip(lines)
        .flat_map(|(number, line)| line.source_ids.iter().map(move |id| (*id, number)))
        .collect();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            for (source_id, line_number) in rows {
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO finance_einvoice_consolidated_sources (tenant_id, \
                         company_id, document_id, source_document_id, line_number) \
                         VALUES ($1, $2, $3, $4, $5) RETURNING id",
                        &[&tenant, &company_id, &document_id, &source_id, &line_number],
                    )
                    .await?
                    .get(0);
                changes.push(Change {
                    kind: "finance.einvoice.consolidated_source".into(),
                    id,
                    old: Value::Null,
                    new: json!({
                        "document_id": document_id, "source_document_id": source_id,
                        "line_number": line_number,
                    }),
                });
            }
            Ok(((), changes))
        })
    })
    .await?;
    let mut source_ids: Vec<i64> = lines
        .iter()
        .flat_map(|line| line.source_ids.iter().copied())
        .collect();
    source_ids.dedup();
    for source_id in source_ids {
        let mut event = NewEvent::new("consolidated");
        event.reason = Some(format!("consolidated in {code_number}"));
        documents::record_event(tx, scope, company_id, source_id, event).await?;
    }
    Ok(())
}
