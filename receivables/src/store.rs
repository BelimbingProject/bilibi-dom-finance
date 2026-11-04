//! Private row persistence for Bilimbi Receivables schemas. Identifiers come
//! only from these module-owned constants; values always use bind parameters.
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Error, Result};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

#[derive(Clone, Copy)]
pub(crate) struct Table {
    pub name: &'static str,
    pub kind: &'static str,
    pub columns: &'static [&'static str],
    pub timestamps: bool,
}
pub(crate) const SETUPS: Table = Table {
    name: "finance_receivables_setups",
    kind: "finance.receivables.setup",
    columns: &[
        "receivable_account_id",
        "income_account_id",
        "invoice_layout",
    ],
    timestamps: true,
};
pub(crate) const HISTORY: Table = Table {
    name: "finance_receivables_setup_changes",
    kind: "finance.receivables.setup_change",
    columns: &[
        "field",
        "old_value",
        "new_value",
        "changed_by_user_id",
        "changed_by_impersonator_id",
    ],
    timestamps: false,
};
pub(crate) const SERIES: Table = Table {
    name: "finance_receivables_series",
    kind: "finance.receivables.series",
    columns: &["code", "name", "kind", "prefix", "number_width", "active"],
    timestamps: true,
};
pub(crate) const TAX: Table = Table {
    name: "finance_receivables_tax_codes",
    kind: "finance.receivables.tax_code",
    columns: &["code", "name", "rate", "account_id", "active"],
    timestamps: true,
};
pub(crate) const CUSTOMERS: Table = Table {
    name: "finance_receivables_customers",
    kind: "finance.receivables.customer",
    columns: &["payment_terms_days", "active", "record_kind", "record_id"],
    timestamps: true,
};
pub(crate) const DOCUMENTS: Table = Table {
    name: "finance_receivables_documents",
    kind: "finance.receivables.document",
    columns: &[
        "kind",
        "status",
        "series_id",
        "customer_id",
        "credited_document_id",
        "currency_code",
        "issue_date",
        "due_date",
        "description",
        "subtotal",
        "tax_total",
        "total",
        "discarded_by_user_id",
    ],
    timestamps: true,
};
pub(crate) const LINES: Table = Table {
    name: "finance_receivables_document_lines",
    kind: "finance.receivables.document_line",
    columns: &[
        "document_id",
        "line_number",
        "description",
        "quantity",
        "unit_price",
        "amount",
        "account_id",
        "tax_code_id",
        "tax_rate",
        "tax_amount",
    ],
    timestamps: false,
};
pub(crate) const TAX_LINES: Table = Table {
    name: "finance_receivables_document_tax_lines",
    kind: "finance.receivables.document_tax_line",
    columns: &[
        "document_id",
        "tax_code_id",
        "tax_code",
        "tax_rate",
        "taxable_amount",
        "tax_amount",
        "account_id",
    ],
    timestamps: false,
};

pub(crate) fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    // JSON numeric -> Decimal must never pass through serde's f64 visitor.
    // Keep the Platform's arbitrary-precision token as decimal text instead.
    fn exact(value: &mut Value) {
        match value {
            Value::Number(n) => *value = Value::String(n.to_string()),
            Value::Object(fields) => {
                for (key, v) in fields {
                    if [
                        "rate",
                        "quantity",
                        "unit_price",
                        "amount",
                        "tax_rate",
                        "tax_amount",
                        "taxable_amount",
                        "subtotal",
                        "tax_total",
                        "total",
                        "outstanding",
                        "lines",
                        "tax_lines",
                        "write_offs",
                        "events",
                        "allocations",
                    ]
                    .contains(&key.as_str())
                    {
                        exact(v);
                    }
                }
            }
            Value::Array(values) => {
                for v in values {
                    exact(v);
                }
            }
            _ => (),
        }
    }
    let mut value = value;
    exact(&mut value);
    serde_json::from_value(value).map_err(|_| Error::Internal)
}
pub(crate) async fn find(
    client: &dyn OperationClient,
    table: Table,
    tenant: i64,
    company: i64,
    id: Option<i64>,
) -> Result<Option<Value>> {
    Ok(client.query_opt(&format!("SELECT to_jsonb(r) FROM {} r WHERE tenant_id=$1 AND company_id=$2 AND ($3::bigint IS NULL OR id=$3)", table.name), &[&tenant, &company, &id]).await?.map(|r| r.get(0)))
}
pub(crate) async fn list(
    client: &dyn OperationClient,
    table: Table,
    tenant: i64,
    company: i64,
) -> Result<Vec<Value>> {
    let order = if table.name == CUSTOMERS.name {
        "id"
    } else {
        "code"
    };
    Ok(client
        .query(
            &format!(
                "SELECT to_jsonb(r) FROM {} r WHERE tenant_id=$1 AND company_id=$2 ORDER BY {order}",
                table.name
            ),
            &[&tenant, &company],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}
pub(crate) async fn taken(
    client: &dyn OperationClient,
    table: Table,
    tenant: i64,
    company: i64,
    field: &'static str,
    value: &Value,
) -> Result<bool> {
    Ok(client.query_opt(&format!("SELECT EXISTS(SELECT 1 FROM {} r WHERE tenant_id=$1 AND company_id=$2 AND to_jsonb(r)->$3=$4)", table.name), &[&tenant, &company, &field, value]).await?.ok_or(Error::Internal)?.get(0))
}
pub(crate) async fn save(
    client: &(impl GenericClient + Sync),
    table: Table,
    tenant: i64,
    company: i64,
    old: Option<Value>,
    data: Value,
    changes: &mut Vec<Change>,
) -> Result<Value> {
    let mut data = data;
    data["tenant_id"] = json!(tenant);
    data["company_id"] = json!(company);
    let row = if let Some(ref old) = old {
        let assignments = table
            .columns
            .iter()
            .map(|c| format!("{c}=v.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        let stamp = if table.timestamps {
            ",updated_at=now() AT TIME ZONE 'UTC'"
        } else {
            ""
        };
        client.query_one(&format!("UPDATE {} r SET {assignments}{stamp} FROM jsonb_populate_record(NULL::{},$1) v WHERE r.tenant_id=$2 AND r.company_id=$3 AND r.id=$4 RETURNING to_jsonb(r)", table.name, table.name), &[&data, &tenant, &company, &old["id"].as_i64().ok_or(Error::Invalid)?]).await?
    } else {
        let columns = table.columns.join(",");
        let stamps = if table.timestamps {
            ",inserted_at,updated_at"
        } else {
            ""
        };
        let now = if table.timestamps {
            ",now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC'"
        } else {
            ""
        };
        client.query_one(&format!("INSERT INTO {} AS r (tenant_id,company_id,{columns}{stamps}) SELECT tenant_id,company_id,{columns}{now} FROM jsonb_populate_record(NULL::{},$1) RETURNING to_jsonb(r)", table.name, table.name), &[&data]).await?
    };
    let new: Value = row.get(0);
    changes.push(Change {
        kind: table.kind.into(),
        id: new["id"].as_i64().ok_or(Error::Invalid)?,
        old: old.unwrap_or(Value::Null),
        new: new.clone(),
    });
    Ok(new)
}
pub(crate) async fn delete_children(
    client: &(impl GenericClient + Sync),
    table: Table,
    tenant: i64,
    company: i64,
    document: i64,
    changes: &mut Vec<Change>,
) -> Result<()> {
    for row in client.query(&format!("DELETE FROM {} r WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 RETURNING to_jsonb(r)", table.name), &[&tenant, &company, &document]).await? {
        let old: Value = row.get(0);
        changes.push(Change { kind: table.kind.into(), id: old["id"].as_i64().ok_or(Error::Invalid)?, old, new: Value::Null });
    }
    Ok(())
}
