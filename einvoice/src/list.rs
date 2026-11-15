//! Bilimbi `Einvoice.list_documents/3` and `Documents.list`: bounded,
//! newest-first summaries. Search treats every character literally.
use crate::{
    authorize, documents, Authority, DocumentSummary, Einvoice, EinvoiceError, Result, VIEW,
};
use bilibi_database::Error;
use bilibi_tenancy::Scope;

/// Filters for the company's fiscal documents. Empty strings mean all.
#[derive(Clone, Debug)]
pub struct DocumentQuery {
    pub search: String,
    pub status: String,
    pub document_type: String,
    pub environment: String,
    pub page: u64,
    pub page_size: u64,
}

impl Default for DocumentQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            status: String::new(),
            document_type: String::new(),
            environment: String::new(),
            page: 1,
            page_size: 25,
        }
    }
}

/// A clamped page; an empty result has page 1 and zero total pages.
#[derive(Clone, Debug)]
pub struct DocumentPage {
    pub entries: Vec<DocumentSummary>,
    pub page: u64,
    pub page_size: u64,
    pub total_entries: u64,
    pub total_pages: u64,
}

impl Einvoice {
    /// Read summaries through `finance.einvoice.view`, descending by ID.
    /// `%`, `_` and backslash in search are text, never SQL wildcards.
    pub async fn list_documents(
        &self,
        scope: &Scope,
        company: i64,
        q: &DocumentQuery,
    ) -> Result<DocumentPage> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let authority = Self::authority();
        if q.page == 0
            || q.page > 1_000_000
            || ![25, 50, 100, 300].contains(&q.page_size)
            || q.search.chars().count() > 255
            || (!q.status.is_empty() && !documents::STATUS_EVENTS.contains(&q.status.as_str()))
            || (!q.environment.is_empty()
                && !["sandbox", "production"].contains(&q.environment.as_str()))
            || (!q.document_type.is_empty()
                && !authority
                    .issuable_document_types()
                    .contains(&q.document_type.as_str()))
        {
            return Err(EinvoiceError::InvalidOption);
        }
        let tenant = scope.tenant();
        let size = q.page_size as i64;
        let page = q.page as i64;
        // One statement counts and selects IDs and statuses from the same snapshot, even
        // when a concurrent issuer adds a document or clearance moves status.
        // strpos, unlike ILIKE, gives literal substring semantics directly.
        let row = client
            .query_one(
                "WITH candidates AS (
               SELECT d.*, COALESCE((SELECT e.event FROM finance_einvoice_status_events e
                   WHERE e.tenant_id = $1 AND e.company_id = $2 AND e.document_id = d.id
                     AND e.event = ANY($8) ORDER BY e.sequence DESC LIMIT 1), 'issued') AS status
               FROM finance_einvoice_documents d WHERE d.tenant_id = $1 AND d.company_id = $2),
             matches AS MATERIALIZED (
               SELECT d.id, d.status FROM candidates d
               WHERE d.tenant_id = $1 AND d.company_id = $2
                 AND ($3 = '' OR strpos(lower(d.code_number), lower($3)) > 0
                   OR strpos(lower(CASE WHEN d.document_type = ANY($7) THEN d.supplier->>'name'
                      ELSE d.buyer->>'name' END), lower($3)) > 0)
                 AND ($4 = '' OR $4 = d.status)
                 AND ($5 = '' OR d.document_type = $5) AND ($6 = '' OR d.environment = $6)),
             counted AS (SELECT count(*) AS total FROM matches),
             paged AS MATERIALIZED (SELECT id, status FROM matches ORDER BY id DESC LIMIT $9
               OFFSET (SELECT (LEAST($10, GREATEST(1, (total + $9 - 1) / $9)) - 1) * $9 FROM counted))
             SELECT total, ARRAY(SELECT id FROM paged ORDER BY id DESC),
               ARRAY(SELECT status FROM paged ORDER BY id DESC) FROM counted",
                &[
                    &tenant,
                    &company,
                    &q.search.trim(),
                    &q.status,
                    &q.document_type,
                    &q.environment,
                    &authority.self_billed_document_types(),
                    &&documents::STATUS_EVENTS[..],
                    &size,
                    &page,
                ],
            )
            .await
            .map_err(Error::from)?;
        let total_entries = row.get::<_, i64>(0) as u64;
        let total_pages = total_entries.div_ceil(q.page_size);
        let ids: Vec<i64> = row.get(1);
        let statuses: Vec<String> = row.get(2);
        // Document snapshots are immutable; only status can move between reads.
        let mut entries =
            documents::summaries(&**client, &authority, tenant, company, &ids).await?;
        for (entry, status) in entries.iter_mut().zip(statuses) {
            entry.status = status;
        }
        Ok(DocumentPage {
            entries,
            page: q.page.min(total_pages.max(1)),
            page_size: q.page_size,
            total_entries,
            total_pages,
        })
    }
}

/// Refused and invalid documents in the company's current environment that
/// still need someone: not a consolidated document (it released its sources
/// and is never sent again) and not followed by a newer document for the same
/// source record that stands or is itself counted as failed.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FailedDocuments {
    pub environment: String,
    pub refused: u64,
    pub invalid: u64,
}

impl Einvoice {
    /// Count failed documents still to act on, through `finance.einvoice.view`.
    pub async fn failed_documents(&self, scope: &Scope, company: i64) -> Result<FailedDocuments> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let environment = self.environment(&**client, scope, company).await?;
        let row = client
            .query_one(
                "WITH failed AS (
               SELECT d.id, COALESCE((SELECT e.event FROM finance_einvoice_status_events e
                   WHERE e.tenant_id = $1 AND e.company_id = $2 AND e.document_id = d.id
                     AND e.event = ANY($4) ORDER BY e.sequence DESC LIMIT 1), 'issued') AS status
               FROM finance_einvoice_documents d
               WHERE d.tenant_id = $1 AND d.company_id = $2 AND d.environment = $3
                 AND NOT EXISTS (SELECT 1 FROM finance_einvoice_consolidated_sources c
                   WHERE c.tenant_id = $1 AND c.company_id = $2 AND c.document_id = d.id)
                 AND NOT EXISTS (SELECT 1 FROM finance_einvoice_source_links l
                   JOIN finance_einvoice_source_links n ON n.tenant_id = l.tenant_id
                     AND n.company_id = l.company_id AND n.source_module = l.source_module
                     AND n.source_record = l.source_record AND n.document_id > l.document_id
                   CROSS JOIN LATERAL (SELECT e.event, e.recorded_at
                     FROM finance_einvoice_status_events e
                     WHERE e.tenant_id = $1 AND e.company_id = $2 AND e.document_id = n.document_id
                       AND e.event = ANY($4) ORDER BY e.sequence DESC LIMIT 1) latest
                   WHERE l.tenant_id = $1 AND l.company_id = $2 AND l.document_id = d.id
                     AND (latest.event IN ('submitted', 'valid', 'rejected', 'consolidated',
                                           'refused', 'invalid')
                       OR EXISTS (SELECT 1 FROM finance_einvoice_submissions s
                         WHERE s.document_id = n.document_id
                           AND s.recorded_at > latest.recorded_at))))
             SELECT count(*) FILTER (WHERE status = 'refused'),
               count(*) FILTER (WHERE status = 'invalid') FROM failed",
                &[
                    &scope.tenant(),
                    &company,
                    &environment,
                    &&documents::STATUS_EVENTS[..],
                ],
            )
            .await
            .map_err(Error::from)?;
        Ok(FailedDocuments {
            environment,
            refused: row.get::<_, i64>(0) as u64,
            invalid: row.get::<_, i64>(1) as u64,
        })
    }
}
