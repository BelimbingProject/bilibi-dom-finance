//! The Invoices list's reads: one bounded page with where each invoice's
//! money is, and the three figures above the list. Both read "overdue" against
//! one day, `as_of`, which a page sends as today where the company is.
use crate::{invalid, store, Error, ReceivableError, Receivables, Result};
use bilibi_agent_api::OperationClient;
use bilibi_finance_ledger::{OwedSummary, Tally};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};

/// An opening invoice a later opening register replaced: it stays as source
/// evidence and owes nothing.
const REPLACED: &str = "EXISTS(SELECT 1 FROM finance_receivables_opening_items i JOIN finance_receivables_opening_decisions x ON x.replaces_batch_id=i.batch_id AND x.tenant_id=$1 AND x.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id)";

/// The day overdue is counted against: the one asked for, else today in UTC.
fn as_of(input: &Value) -> Result<Date> {
    match input["as_of"].as_str() {
        Some(day) => day.parse().map_err(|_| invalid("as_of", "is not a date")),
        None => Ok(jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date()),
    }
}

/// `finance.receivables.document.page`.
pub(crate) async fn page(
    client: &dyn OperationClient,
    service: &Receivables,
    scope: &Scope,
    company: i64,
    input: &Value,
) -> Result<Value> {
    let tenant = scope.tenant();
    let as_of = as_of(input)?;
    let day = as_of.to_string();
    let kind = input["kind"].as_str();
    let status = input["status"].as_str();
    let customer = input["customer_id"].as_i64();
    let source = input["source"].as_str();
    let search = input["search"].as_str().unwrap_or("").trim();
    let page = input["page"].as_i64().unwrap_or(1);
    let page_size = input["page_size"].as_i64().unwrap_or(25);
    let sort = input["sort"].as_str().unwrap_or("number");
    let direction = input["direction"].as_str().unwrap_or("desc");
    if kind.is_some_and(|value| !["invoice", "credit_note", "opening_credit"].contains(&value))
        || status.is_some_and(|value| {
            !matches!(
                value,
                "draft" | "issued" | "unpaid" | "overdue" | "paid" | "discarded"
            )
        })
        || customer.is_some_and(|value| value <= 0)
        || source.is_some_and(|value| !matches!(value, "ordinary" | "opening" | "sales_order"))
        || !matches!(sort, "number" | "issue_date" | "due_date" | "total")
        || !matches!(direction, "asc" | "desc")
        || page < 1
        || ![25, 50, 100].contains(&page_size)
        || search.chars().count() > 255
    {
        return Err(ReceivableError::InvalidOption);
    }
    let filters_sql = format!(" FROM finance_receivables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2 AND ($3::text IS NULL OR d.kind=$3) AND ($4::text IS NULL OR ($4 IN ('unpaid','overdue','paid') AND d.kind='invoice' AND d.status='issued' AND NOT {REPLACED} AND CASE $4 WHEN 'paid' THEN finance_receivables_outstanding(d.id)<=0 WHEN 'overdue' THEN finance_receivables_outstanding(d.id)>0 AND d.due_date<$7::text::date ELSE finance_receivables_outstanding(d.id)>0 END) OR ($4 NOT IN ('unpaid','overdue','paid') AND d.status=$4)) AND ($5::bigint IS NULL OR d.customer_id=$5) AND ($6::text IS NULL OR finance_receivables_document_source($1,$2,d.id)=$6)");
    let filter_params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] =
        [&tenant, &company, &kind, &status, &customer, &source, &day];
    let mut matching_customers = Vec::new();
    if !search.is_empty() {
        let needle = search.to_lowercase();
        let candidates = format!("SELECT to_jsonb(c) FROM finance_receivables_customers c WHERE c.tenant_id=$1 AND c.company_id=$2 AND EXISTS (SELECT 1{filters_sql} AND d.status IN ('draft','discarded') AND d.party_name IS NULL AND d.customer_id=c.id AND strpos(lower(COALESCE(d.document_reference,'Draft #' || d.id::text)),lower($8))=0 AND strpos(lower(COALESCE(d.description,'')),lower($8))=0) ORDER BY c.id");
        let mut candidate_params = filter_params.to_vec();
        candidate_params.push(&search);
        for row in client.query(&candidates, &candidate_params).await? {
            let record = service.customer(client, scope, row.get(0)).await?;
            if record.code.to_lowercase().contains(&needle)
                || record.name.to_lowercase().contains(&needle)
            {
                matching_customers.push(record.id);
            }
        }
    }
    let where_sql = format!("{filters_sql} AND ($8='' OR strpos(lower(COALESCE(d.document_reference,'Draft #' || d.id::text)),lower($8))>0 OR strpos(lower(COALESCE(d.party_code,'')),lower($8))>0 OR strpos(lower(COALESCE(d.party_name,'')),lower($8))>0 OR strpos(lower(COALESCE(d.description,'')),lower($8))>0 OR (d.party_name IS NULL AND d.customer_id=ANY($9::bigint[])))");
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 9] = [
        &tenant,
        &company,
        &kind,
        &status,
        &customer,
        &source,
        &day,
        &search,
        &matching_customers,
    ];
    let total: i64 = client
        .query_opt(&format!("SELECT count(*){where_sql}"), &params)
        .await?
        .ok_or(Error::Internal)?
        .get(0);
    let page = page.min(((total + page_size - 1) / page_size).max(1));
    let offset = (page - 1) * page_size;
    let order = match (sort, direction) {
        ("issue_date", "asc") => "d.issue_date ASC NULLS FIRST,d.id ASC",
        ("issue_date", _) => "d.issue_date DESC NULLS LAST,d.id DESC",
        ("due_date", "asc") => "d.due_date ASC NULLS FIRST,d.id ASC",
        ("due_date", _) => "d.due_date DESC NULLS LAST,d.id DESC",
        ("total", "asc") => "d.total ASC,d.id ASC",
        ("total", _) => "d.total DESC,d.id DESC",
        ("number", "asc") => "d.id ASC",
        _ => "d.id DESC",
    };
    let rows_sql = format!("SELECT jsonb_build_object('sales_order',(SELECT to_jsonb(o) FROM finance_receivables_sales_orders o WHERE o.tenant_id=$1 AND o.company_id=$2 AND o.document_id=d.id),'opening',EXISTS (SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id),'id',d.id,'kind',d.kind,'status',d.status,'customer_id',d.customer_id,'issue_date',d.issue_date,'due_date',d.due_date,'description',d.description,'total',d.total,'outstanding',CASE WHEN {REPLACED} THEN 0 WHEN d.kind IN ('invoice','opening_credit') AND d.status='issued' THEN finance_receivables_outstanding(d.id) ELSE NULL END,'replaced',{REPLACED},'document_reference',d.document_reference,'party_code',d.party_code,'party_name',d.party_name){where_sql} ORDER BY {order} LIMIT $10 OFFSET $11");
    let mut row_params = params.to_vec();
    row_params.push(&page_size);
    row_params.push(&offset);
    let mut documents: Vec<crate::records::DocumentPageRow> = client
        .query(&rows_sql, &row_params)
        .await?
        .into_iter()
        .map(|row| store::decode(row.get(0)))
        .collect::<std::result::Result<_, _>>()?;
    for document in &mut documents {
        document.settle(as_of);
    }
    let asked: Vec<(i64, &str, &str, bool)> = documents
        .iter()
        .map(|d| (d.id, d.kind.as_str(), d.status.as_str(), d.opening))
        .collect();
    let (fiscal, fiscal_documents) = service.fiscal_on(client, scope, company, &asked).await?;
    for (document, fiscal) in documents.iter_mut().zip(fiscal) {
        document.fiscal = fiscal;
    }
    Ok(
        json!({"documents":documents,"total":total,"page":page,"page_size":page_size,"as_of":day,"fiscal_documents":fiscal_documents}),
    )
}

/// `finance.receivables.document.summary`: draft invoices, issued invoices
/// still owed, and those of them past their due date.
pub(crate) async fn summary(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    input: &Value,
) -> Result<Value> {
    let as_of = as_of(input)?;
    let day = as_of.to_string();
    let row = client
        .query_opt(
            &format!("SELECT count(*) FILTER (WHERE t.status='draft'),COALESCE(sum(t.total) FILTER (WHERE t.status='draft'),0),count(*) FILTER (WHERE t.owed>0),COALESCE(sum(t.owed) FILTER (WHERE t.owed>0),0),count(*) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date),COALESCE(sum(t.owed) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date),0),max($3::text::date-t.due_date) FILTER (WHERE t.owed>0 AND t.due_date<$3::text::date) FROM (SELECT d.status,d.total,d.due_date,CASE WHEN d.status='issued' AND NOT {REPLACED} THEN finance_receivables_outstanding(d.id) ELSE 0 END AS owed FROM finance_receivables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.kind='invoice' AND d.status IN ('draft','issued')) t"),
            &[&tenant, &company, &day],
        )
        .await?
        .ok_or(Error::Internal)?;
    let oldest: Option<i32> = row.get(6);
    Ok(json!(OwedSummary {
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
    }))
}
