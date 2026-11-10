//! The Bills list's reads: one bounded page with where each bill's money is,
//! and the three figures above the list. Both read "overdue" against one day,
//! `as_of`, which a page sends as today where the company is.
use crate::*;
use bilibi_finance_ledger::{Owed, OwedSummary, Tally};

/// An opening bill a later opening register replaced: it stays as source
/// evidence and owes nothing.
const REPLACED: &str = "EXISTS(SELECT 1 FROM finance_payables_opening_items i JOIN finance_payables_opening_decisions x ON x.replaces_batch_id=i.batch_id AND x.tenant_id=$1 AND x.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id)";
/// What a posted bill still owes after every debit note, payment and
/// settlement on record (`store::outstanding` for one bill).
const OWED: &str = "finance_payables_residual($1,$2,d.id,'9999-12-31'::date,9223372036854775807,9223372036854775807)";
/// A bill's three-way match as the list names it, or NULL without order lines.
const MATCH: &str = "CASE WHEN d.document_type<>'bill' OR d.status='voided' OR NOT EXISTS(SELECT 1 FROM finance_payables_document_lines l WHERE l.tenant_id=$1 AND l.company_id=$2 AND l.document_id=d.id AND l.order_line_id IS NOT NULL) THEN NULL ELSE COALESCE((SELECT CASE e.event WHEN 'posted' THEN CASE WHEN EXISTS(SELECT 1 FROM finance_payables_match_lines ml JOIN finance_payables_match_exceptions me ON me.match_line_id=ml.id AND me.tenant_id=$1 AND me.company_id=$2 WHERE ml.tenant_id=$1 AND ml.company_id=$2 AND ml.event_id=e.id) THEN 'posted_with_exceptions' ELSE 'matched' END WHEN 'held' THEN 'held' ELSE 'exception_approved' END FROM finance_payables_match_events e WHERE e.tenant_id=$1 AND e.company_id=$2 AND e.document_id=d.id ORDER BY e.id DESC LIMIT 1),'pending') END";

/// The statuses that say where a posted bill's money is.
const MONEY: [&str; 3] = ["unpaid", "overdue", "paid"];

fn today() -> Date {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
}

impl Payables {
    pub async fn page_documents(
        &self,
        s: &Scope,
        company: i64,
        q: &DocumentPageQuery,
    ) -> Result<DocumentPage> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        self.page_documents_on(&**c, s, company, q).await
    }

    pub(crate) async fn page_documents_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        q: &DocumentPageQuery,
    ) -> Result<DocumentPage> {
        crate::agent_operations::authorize_on(c, s, company, VIEW).await?;
        if q.document_type
            .as_deref()
            .is_some_and(|k| !matches!(k, "bill" | "debit_note" | "opening_credit"))
            || q.status.as_deref().is_some_and(|k| {
                !matches!(k, "draft" | "approved" | "held" | "posted" | "voided")
                    && !MONEY.contains(&k)
            })
            || q.source
                .as_deref()
                .is_some_and(|source| !matches!(source, "ordinary" | "opening"))
            || q.page < 1
            || ![25, 50, 100].contains(&q.page_size)
            || q.search.chars().count() > 255
        {
            return Err(PayablesError::InvalidOption);
        }
        let search = q.search.trim();
        let as_of = q.as_of.unwrap_or_else(today);
        let day = as_of.to_string();
        // Bilimbi page_documents: literal substring search in the party's
        // snapshot and own number, then clamp before reading the last page.
        let filter = format!(" FROM finance_payables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2 AND ($3::text IS NULL OR d.document_type=$3) AND ($4::text IS NULL OR ($4 IN ('unpaid','overdue','paid') AND d.document_type='bill' AND d.status='posted' AND NOT {REPLACED} AND CASE $4 WHEN 'paid' THEN {OWED}<=0 WHEN 'overdue' THEN {OWED}>0 AND d.due_date<$7::text::date ELSE {OWED}>0 END) OR ($4 NOT IN ('unpaid','overdue','paid') AND d.status=$4)) AND (strpos(lower(d.party_code),lower($5))>0 OR strpos(lower(d.party_name),lower($5))>0 OR strpos(lower(COALESCE(d.party_document_number,'')),lower($5))>0) AND ($6::text IS NULL OR EXISTS (SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id)=($6='opening'))");
        let total: i64 = c
            .query_opt(
                &format!("SELECT count(*){filter}"),
                &[
                    &s.tenant(),
                    &company,
                    &q.document_type,
                    &q.status,
                    &search,
                    &q.source,
                    &day,
                ],
            )
            .await?
            .ok_or(Error::Internal)?
            .get(0);
        let page = q.page.min(((total + q.page_size - 1) / q.page_size).max(1));
        let offset = (page - 1) * q.page_size;
        let rows = c
            .query(
                &format!("SELECT d.id,d.document_number,d.document_type,d.party_code,d.party_name,d.party_document_number,d.document_date::text,d.total,d.currency_code,d.status,{MATCH},EXISTS(SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id),d.due_date::text,{REPLACED},CASE WHEN d.document_type IN ('bill','opening_credit') AND d.status='posted' THEN CASE WHEN {REPLACED} THEN 0 ELSE {OWED} END END{filter} ORDER BY d.document_date DESC,d.id DESC LIMIT $8 OFFSET $9"),
                &[&s.tenant(), &company, &q.document_type, &q.status, &search, &q.source, &day, &q.page_size, &offset],
            )
            .await?;
        let documents = rows
            .into_iter()
            .map(|row| {
                let document_type: String = row.get(2);
                let status: String = row.get(9);
                let total: Decimal = row.get(7);
                let due_date: String = row.get(12);
                let replaced: bool = row.get(13);
                let outstanding: Option<Decimal> = row.get(14);
                let owed = outstanding
                    .filter(|_| document_type == "bill" && status == "posted" && !replaced)
                    .map(|owed| Owed::of(total, owed, due_date.parse().ok(), as_of));
                DocumentPageRow {
                    id: row.get(0),
                    document_number: row.get(1),
                    document_type,
                    party_code: row.get(3),
                    party_name: row.get(4),
                    party_document_number: row.get(5),
                    document_date: row.get(6),
                    due_date,
                    total,
                    outstanding,
                    currency_code: row.get(8),
                    status,
                    match_status: row.get(10),
                    opening: row.get(11),
                    replaced,
                    payment_status: owed.map(|o| o.payment_status),
                    days_overdue: owed.and_then(|o| o.days_overdue),
                }
            })
            .collect();
        Ok(DocumentPage {
            documents,
            total,
            page,
            page_size: q.page_size,
            as_of: day,
        })
    }

    /// Bills not posted yet (draft, approved or held), posted bills still
    /// owed, and those of them past their due date, read against `as_of`
    /// (today in UTC when absent).
    pub async fn summarize_documents(
        &self,
        s: &Scope,
        company: i64,
        as_of: Option<Date>,
    ) -> Result<OwedSummary> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        self.summarize_documents_on(&**c, s, company, as_of).await
    }

    pub(crate) async fn summarize_documents_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        as_of: Option<Date>,
    ) -> Result<OwedSummary> {
        crate::agent_operations::authorize_on(c, s, company, VIEW).await?;
        let day = as_of.unwrap_or_else(today).to_string();
        let row = c
            .query_opt(
                &format!("SELECT count(*) FILTER (WHERE t.status<>'posted'),COALESCE(sum(t.total) FILTER (WHERE t.status<>'posted'),0),count(*) FILTER (WHERE t.owed>0),COALESCE(sum(t.owed) FILTER (WHERE t.owed>0),0),count(*) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date),COALESCE(sum(t.owed) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date),0),max($3::text::date-t.due_date) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date) FROM (SELECT d.status,d.total,d.due_date,CASE WHEN d.status='posted' AND NOT {REPLACED} THEN {OWED} ELSE 0 END AS owed FROM finance_payables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.document_type='bill' AND d.status IN ('draft','approved','held','posted')) t"),
                &[&s.tenant(), &company, &day],
            )
            .await?
            .ok_or(Error::Internal)?;
        let oldest: Option<i32> = row.get(6);
        Ok(OwedSummary {
            as_of: day,
            not_posted: Tally {
                count: row.get(0),
                amount: row.get(1),
            },
            awaiting_payment: Tally {
                count: row.get(2),
                amount: row.get(3),
            },
            overdue: Tally {
                count: row.get(4),
                amount: row.get(5),
            },
            oldest_days_overdue: oldest.map(i64::from),
        })
    }
}
