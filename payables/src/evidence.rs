//! Bilimbi.Finance.Payables.Evidence: frozen exports of this module's own tables.
use crate::*;
use bilibi_database::Error;
use serde::Serialize;
use serde_json::Value;
use tokio_postgres::GenericClient;

const DEFAULT_LIMIT: i64 = 500;
const MAX_LIMIT: i64 = 500;
const ISO: &str = "YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"";

/// Each dataset, its table and its fields, by dataset name. Freeze a field
/// list once exported; never add a credential to one.
const DATASETS: &[(&str, &str, &[&str])] = &[
    (
        "settlement",
        "finance_payables_settlements",
        &[
            "id",
            "tenant_id",
            "company_id",
            "settlement_number",
            "kind",
            "credit_item_id",
            "credit_document_id",
            "target_document_id",
            "amount",
            "effective_date",
            "cash_account_id",
            "reference",
            "reverses_settlement_id",
            "entry_id",
            "entry_number",
            "review",
            "actor_user_id",
            "actor_impersonator_id",
            "audit_trace_id",
            "audit_action",
            "recorded_at",
        ],
    ),
    (
        "opening_stage",
        "finance_payables_opening_stages",
        &[
            "id",
            "tenant_id",
            "company_id",
            "source_reference",
            "part",
            "payload",
            "recorded_by_user_id",
            "recorded_by_impersonator_id",
            "recorded_at",
        ],
    ),
    (
        "opening_decision",
        "finance_payables_opening_decisions",
        &[
            "id",
            "tenant_id",
            "company_id",
            "batch_id",
            "replaces_batch_id",
            "reason",
            "effective_date",
            "review",
            "approval",
            "state",
            "recorded_by_user_id",
            "recorded_by_impersonator_id",
            "recorded_at",
        ],
    ),
    (
        "opening_batch",
        "finance_payables_opening_batches",
        &[
            "id",
            "tenant_id",
            "company_id",
            "source_reference",
            "source_csv",
            "currency_code",
            "conversion_date",
            "entry_id",
            "entry_number",
            "recorded_by_user_id",
            "recorded_by_impersonator_id",
            "recorded_at",
        ],
    ),
    (
        "opening_item",
        "finance_payables_opening_items",
        &[
            "id",
            "tenant_id",
            "company_id",
            "batch_id",
            "source_id",
            "document_id",
            "source_facts",
        ],
    ),
    (
        "vendor",
        "finance_payables_vendors",
        &[
            "id",
            "tenant_id",
            "company_id",
            "code",
            "name",
            "record_kind",
            "record_id",
            "payment_terms_days",
            "payable_account_id",
            "expense_account_id",
            "tax_code_id",
            "active",
            "inserted_at",
            "updated_at",
        ],
    ),
    (
        "allocation",
        "finance_payables_payment_allocations",
        &[
            "id",
            "tenant_id",
            "company_id",
            "payment_id",
            "line_number",
            "document_id",
            "amount",
        ],
    ),
    (
        "document",
        "finance_payables_documents",
        &[
            "id",
            "tenant_id",
            "company_id",
            "document_type",
            "status",
            "document_number",
            "corrects_document_id",
            "party_key",
            "party_code",
            "party_name",
            "party_tax_id",
            "party_registration_id",
            "party_address",
            "party_document_number",
            "document_date",
            "due_date",
            "currency_code",
            "description",
            "total_net",
            "total_tax",
            "total",
            "created_by_user_id",
            "created_by_impersonator_id",
            "created_at",
            "approved_by_user_id",
            "approved_by_impersonator_id",
            "approved_at",
            "posted_by_user_id",
            "posted_by_impersonator_id",
            "posted_at",
            "payable_account_id",
            "entry_id",
            "entry_number",
            "voided_by_user_id",
            "voided_by_impersonator_id",
            "voided_at",
            "void_reason",
        ],
    ),
    (
        "document_line",
        "finance_payables_document_lines",
        &[
            "id",
            "tenant_id",
            "company_id",
            "document_id",
            "line_number",
            "description",
            "account_id",
            "amount",
            "tax_code_id",
            "external_order_line_id",
            "order_line_id",
            "quantity",
        ],
    ),
    (
        "match_event",
        "finance_payables_match_events",
        &[
            "id",
            "tenant_id",
            "company_id",
            "document_id",
            "event",
            "reason",
            "held_event_id",
            "actor_user_id",
            "actor_impersonator_id",
            "occurred_at",
        ],
    ),
    (
        "match_exception",
        "finance_payables_match_exceptions",
        &[
            "id",
            "tenant_id",
            "company_id",
            "match_line_id",
            "code",
            "detail",
        ],
    ),
    (
        "match_line",
        "finance_payables_match_lines",
        &[
            "id",
            "tenant_id",
            "company_id",
            "event_id",
            "document_id",
            "line_number",
            "order_line_id",
            "order_number",
            "billed_quantity",
            "billed_unit_price",
            "previously_billed_quantity",
            "ordered_quantity",
            "ordered_unit_price",
            "received_quantity",
            "quantity_tolerance_kind",
            "quantity_tolerance",
            "price_tolerance_kind",
            "price_tolerance",
        ],
    ),
    (
        "order_line",
        "finance_payables_order_lines",
        &[
            "id",
            "tenant_id",
            "company_id",
            "order_line_id",
            "order_id",
            "order_number",
            "line_number",
            "description",
            "unit",
            "quantity",
            "unit_price",
            "currency_code",
            "order_status",
            "superseded_by_amendment_id",
            "order_event_key",
        ],
    ),
    (
        "order_receipt",
        "finance_payables_order_receipts",
        &[
            "id",
            "tenant_id",
            "company_id",
            "order_line_id",
            "event_key",
            "kind",
            "posting_id",
            "receipt_id",
            "receipt_number",
            "posted_at",
            "received_quantity",
            "rejected_quantity",
        ],
    ),
    (
        "payment",
        "finance_payables_payments",
        &[
            "id",
            "tenant_id",
            "company_id",
            "payment_number",
            "payment_date",
            "party_key",
            "party_code",
            "party_name",
            "paid_from_account_id",
            "amount",
            "currency_code",
            "reference",
            "description",
            "reverses_payment_id",
            "entry_id",
            "entry_number",
            "posted_by_user_id",
            "posted_by_impersonator_id",
            "posted_at",
        ],
    ),
    (
        "payment_request",
        "finance_payables_payment_requests",
        &[
            "id",
            "tenant_id",
            "company_id",
            "request_number",
            "payment_date",
            "party_key",
            "party_code",
            "party_name",
            "paid_from_account_id",
            "amount",
            "currency_code",
            "reference",
            "description",
            "submitted_by_user_id",
            "submitted_by_impersonator_id",
            "submitted_at",
        ],
    ),
    (
        "payment_request_allocation",
        "finance_payables_payment_request_allocations",
        &[
            "id",
            "tenant_id",
            "company_id",
            "request_id",
            "line_number",
            "document_id",
            "amount",
        ],
    ),
    (
        "payment_request_event",
        "finance_payables_payment_request_events",
        &[
            "id",
            "tenant_id",
            "company_id",
            "request_id",
            "event",
            "reason",
            "payment_id",
            "actor_user_id",
            "actor_impersonator_id",
            "occurred_at",
        ],
    ),
    (
        "settings",
        "finance_payables_settings",
        &[
            "id",
            "tenant_id",
            "company_id",
            "payable_account_id",
            "quantity_tolerance_kind",
            "quantity_tolerance",
            "price_tolerance_kind",
            "price_tolerance",
            "inserted_at",
            "updated_at",
        ],
    ),
    (
        "settings_change",
        "finance_payables_settings_changes",
        &[
            "id",
            "tenant_id",
            "company_id",
            "old_payable_account_id",
            "new_payable_account_id",
            "old_quantity_tolerance_kind",
            "new_quantity_tolerance_kind",
            "old_quantity_tolerance",
            "new_quantity_tolerance",
            "old_price_tolerance_kind",
            "new_price_tolerance_kind",
            "old_price_tolerance",
            "new_price_tolerance",
            "reason",
            "changed_by_user_id",
            "changed_by_impersonator_id",
            "changed_at",
        ],
    ),
    (
        "tax_code",
        "finance_payables_tax_codes",
        &[
            "id",
            "tenant_id",
            "company_id",
            "code",
            "name",
            "rate",
            "account_id",
            "active",
            "inserted_at",
            "updated_at",
        ],
    ),
    (
        "tax_code_change",
        "finance_payables_tax_code_changes",
        &[
            "id",
            "tenant_id",
            "company_id",
            "tax_code_id",
            "old_name",
            "new_name",
            "old_rate",
            "new_rate",
            "old_account_id",
            "new_account_id",
            "old_active",
            "new_active",
            "reason",
            "changed_by_user_id",
            "changed_by_impersonator_id",
            "changed_at",
        ],
    ),
    (
        "tax_line",
        "finance_payables_document_tax_lines",
        &[
            "id",
            "tenant_id",
            "company_id",
            "document_id",
            "line_number",
            "tax_code_id",
            "tax_code",
            "rate",
            "account_id",
            "taxable_amount",
            "tax_amount",
        ],
    ),
];

/// One page: the explicit fields, the rows and the cursor of the next page
/// (the last row's id when the page was full).
#[derive(Clone, Debug, Serialize)]
pub struct EvidencePage {
    pub fields: Vec<&'static str>,
    pub rows: Vec<Value>,
    pub next: Option<i64>,
}

/// Every dataset's name, sorted.
pub(crate) fn datasets() -> Vec<&'static str> {
    DATASETS.iter().map(|(name, _, _)| *name).collect()
}

/// A field as the export writes it.
fn column(field: &str) -> String {
    if field.ends_with("_at") {
        format!("to_char(r.{field},'{ISO}') AS {field}")
    } else if field.ends_with("_date") {
        format!("to_char(r.{field},'YYYY-MM-DD') AS {field}")
    } else if matches!(
        field,
        "amount"
            | "total"
            | "total_net"
            | "total_tax"
            | "rate"
            | "old_rate"
            | "new_rate"
            | "taxable_amount"
            | "tax_amount"
            | "quantity"
            | "quantity_tolerance"
            | "price_tolerance"
            | "old_quantity_tolerance"
            | "new_quantity_tolerance"
            | "old_price_tolerance"
            | "new_price_tolerance"
            | "billed_quantity"
            | "billed_unit_price"
            | "previously_billed_quantity"
            | "ordered_quantity"
            | "ordered_unit_price"
            | "received_quantity"
            | "rejected_quantity"
            | "unit_price"
    ) {
        format!("r.{field}::text AS {field}")
    } else {
        format!("r.{field}")
    }
}

/// One page of `dataset` for the company: `InvalidOption` for an unknown
/// dataset, a limit outside 1 to 500, or a negative cursor.
pub(crate) async fn page(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    dataset: &str,
    limit: Option<i64>,
    after: Option<i64>,
) -> Result<EvidencePage> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    let after = after.unwrap_or(0);
    let (_, table, fields) = DATASETS
        .iter()
        .find(|(name, _, _)| *name == dataset)
        .ok_or(PayablesError::InvalidOption)?;
    if !(1..=MAX_LIMIT).contains(&limit) || after < 0 {
        return Err(PayablesError::InvalidOption);
    }
    let select: Vec<String> = fields
        .iter()
        .map(|field| {
            if (dataset == "vendor" && matches!(*field, "code" | "name"))
                || (matches!(dataset, "payment" | "payment_request") && *field == "party_code")
            {
                format!("NULL::text AS {field}")
            } else {
                column(field)
            }
        })
        .collect();
    let row = client
        .query_one(
            &format!(
                "SELECT COALESCE(json_agg(t ORDER BY t.id),'[]'::json) FROM (SELECT {} FROM {table} r \
                 WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id > $3 ORDER BY r.id LIMIT $4) t",
                select.join(",")
            ),
            &[&tenant, &company_id, &after, &limit],
        )
        .await
        .map_err(Error::from)?;
    let rows: Vec<Value> = match row.get::<_, Value>(0) {
        Value::Array(rows) => rows,
        _ => Vec::new(),
    };
    let next = (rows.len() as i64 == limit)
        .then(|| rows.last().and_then(|row| row["id"].as_i64()))
        .flatten();
    Ok(EvidencePage {
        fields: fields.to_vec(),
        rows,
        next,
    })
}

impl Payables {
    pub fn evidence_datasets(&self) -> Vec<&'static str> {
        datasets()
    }
    pub async fn evidence_page(
        &self,
        s: &Scope,
        company: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.evidence_page_on(&**c, s, company, dataset, limit, after)
            .await
    }

    /// Authorized evidence on the caller's snapshot; never opens another connection.
    pub async fn evidence_page_on(
        &self,
        c: &(impl tokio_postgres::GenericClient + Sync),
        s: &Scope,
        company: i64,
        dataset: &str,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<EvidencePage> {
        authorize(c, s, company, VIEW).await?;
        let mut page = page(c, s.tenant(), company, dataset, limit, after).await?;
        if dataset == "vendor" {
            for row in &mut page.rows {
                let vendor = self
                    .vendor_on(c, s, company, row["id"].as_i64().ok_or(Error::Internal)?)
                    .await?;
                row["code"] = serde_json::json!(vendor.code);
                row["name"] = serde_json::json!(vendor.party.name);
            }
        }
        if matches!(dataset, "payment" | "payment_request") {
            for row in &mut page.rows {
                let id = row["id"].as_i64().ok_or(Error::Internal)?;
                let code = if dataset == "payment" {
                    crate::payments::payment(c, s.tenant(), company, id)
                        .await?
                        .party_code
                } else {
                    crate::payments::request(c, s.tenant(), company, id)
                        .await?
                        .party_code
                };
                row["party_code"] = serde_json::json!(code);
            }
        }
        Ok(page)
    }
}
