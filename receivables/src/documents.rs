//! Bilimbi Receivables.Documents, NR1: unnumbered drafts with whole-line
//! replacement, per-line half-up tax, and a kept discard (never deletion).
use crate::{
    authorize, invalid,
    store::{self, CUSTOMERS, DOCUMENTS, LINES, SERIES, SETUPS, TAX, TAX_LINES},
    validation as v, Document, DocumentQuery, ReceivableError as R, Receivables, Result,
    MANAGE_DOCUMENTS, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{Account, AccountType, MinorUnits, Money};
use bilibi_tenancy::Scope;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct Prepared {
    pub before: Option<Value>,
    pub after: Value,
    pub replacement: Option<(Vec<Value>, Vec<Value>)>,
    pub facts: Value,
}

// A single statement gives headers and children the same MVCC snapshot.
pub(crate) const READ: &str = "SELECT to_jsonb(d) || jsonb_build_object(
 'settlements',COALESCE((SELECT jsonb_agg(to_jsonb(e)-'review' ORDER BY e.settlement_number) FROM finance_receivables_settlements e WHERE e.tenant_id=$1 AND e.company_id=$2 AND (e.credit_document_id=d.id OR e.target_document_id=d.id)),'[]'::jsonb),
 'opening',(SELECT i.source_facts || jsonb_build_object('source_reference',b.source_reference,'conversion_date',b.conversion_date::text,'batch_id',b.id,'replaced_by_batch_id',(SELECT x.batch_id FROM finance_receivables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=b.id)) FROM finance_receivables_opening_items i JOIN finance_receivables_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id),
 'write_offs',COALESCE((SELECT jsonb_agg(to_jsonb(w) || jsonb_build_object('events',COALESCE((SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM finance_receivables_write_off_events e WHERE e.write_off_id=w.id AND e.tenant_id=$1 AND e.company_id=$2),'[]'::jsonb)) ORDER BY w.id) FROM finance_receivables_write_offs w WHERE w.document_id=d.id AND w.tenant_id=$1 AND w.company_id=$2),'[]'::jsonb),
 'outstanding',CASE WHEN EXISTS(SELECT 1 FROM finance_receivables_opening_items i JOIN finance_receivables_opening_decisions x ON x.replaces_batch_id=i.batch_id AND x.tenant_id=$1 AND x.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id) THEN 0 WHEN d.kind IN ('invoice','opening_credit') AND d.status='issued' THEN finance_receivables_outstanding(d.id) ELSE NULL END,
 'lines',COALESCE((SELECT jsonb_agg(to_jsonb(l) ORDER BY line_number) FROM finance_receivables_document_lines l WHERE l.tenant_id=$1 AND l.company_id=$2 AND l.document_id=d.id),'[]'::jsonb),
 'tax_lines',COALESCE((SELECT jsonb_agg(to_jsonb(t) ORDER BY tax_code) FROM finance_receivables_document_tax_lines t WHERE t.tenant_id=$1 AND t.company_id=$2 AND t.document_id=d.id),'[]'::jsonb))
 FROM finance_receivables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2";

impl Receivables {
    pub async fn create_draft(
        &self,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Document> {
        self.save_draft(scope, company, None, attrs).await
    }
    pub async fn update_draft(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<Document> {
        self.save_draft(scope, company, Some(id), attrs).await
    }
    pub async fn discard_draft(&self, scope: &Scope, company: i64, id: i64) -> Result<Document> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                MANAGE_DOCUMENTS,
                "finance.receivables.discard_draft",
            )
            .await?;
        let document = self.discard_draft_on(&mut tx, scope, company, id).await?;
        tx.commit().await?;
        Ok(document)
    }

    pub(super) async fn discard_draft_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Document> {
        self.join(tx.sql(), scope, company, MANAGE_DOCUMENTS)
            .await?;
        let tenant = scope.tenant();
        let old = draft(tx.sql(), tenant, company, id).await?;
        let mut data = old.clone();
        data["status"] = json!("discarded");
        data["discarded_by_user_id"] = json!(scope.user());
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                store::save(
                    sql,
                    DOCUMENTS,
                    tenant,
                    company,
                    Some(old),
                    data,
                    &mut changes,
                )
                .await?;
                Ok(((), changes))
            })
        })
        .await?;
        let document = read(tx.sql(), tenant, company, id).await?;
        Ok(document)
    }
    pub async fn get_document(&self, scope: &Scope, company: i64, id: i64) -> Result<Document> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        read(&**client, scope.tenant(), company, id).await
    }
    pub async fn list_documents(
        &self,
        scope: &Scope,
        company: i64,
        query: &DocumentQuery,
    ) -> Result<Vec<Document>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let (limit, after) = query.page.bounds()?;
        if query
            .kind
            .as_deref()
            .is_some_and(|s| !["invoice", "credit_note", "opening_credit"].contains(&s))
            || query
                .status
                .as_deref()
                .is_some_and(|s| !["draft", "issued", "discarded"].contains(&s))
            || query.customer_id.is_some_and(|id| id <= 0)
            || query
                .source
                .as_deref()
                .is_some_and(|source| !matches!(source, "ordinary" | "opening"))
        {
            return Err(R::InvalidOption);
        }
        let sql = format!(
            "{READ} AND d.id>$3 AND ($4::text IS NULL OR d.kind=$4) AND ($5::text IS NULL OR d.status=$5) AND ($6::bigint IS NULL OR d.customer_id=$6) AND ($8::text IS NULL OR EXISTS (SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id)=($8='opening')) ORDER BY d.id LIMIT $7"
        );
        client
            .query(
                &sql,
                &[
                    &scope.tenant(),
                    &company,
                    &after,
                    &query.kind,
                    &query.status,
                    &query.customer_id,
                    &limit,
                    &query.source,
                ],
            )
            .await
            .map_err(Error::from)?
            .into_iter()
            .map(|r| store::decode(r.get(0)).map_err(R::from))
            .collect()
    }

    async fn save_draft(
        &self,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Document> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                MANAGE_DOCUMENTS,
                if id.is_some() {
                    "finance.receivables.update_draft"
                } else {
                    "finance.receivables.create_draft"
                },
            )
            .await?;
        let result = self
            .save_draft_on(&mut tx, scope, company, id, attrs)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn prepare_draft(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Prepared> {
        self.join(client, scope, company, MANAGE_DOCUMENTS).await?;
        let tenant = scope.tenant();
        // A draft is prepared under Receivables' own capability, so a person
        // without ledger view (the sales workflow) can prepare one. The ledger
        // answers the book's currency and the chart for validation only.
        let book = self
            .ledger
            .book_figures_on(client, scope, company, MANAGE_DOCUMENTS)
            .await?;
        let setup = store::find(client, SETUPS, tenant, company, None)
            .await?
            .ok_or(R::NotConfigured)?;
        let chart = self
            .ledger
            .list_accounts_for_on(client, scope, company, MANAGE_DOCUMENTS)
            .await?;
        let old = if let Some(id) = id {
            Some(draft(client, tenant, company, id).await?)
        } else {
            None
        };
        let mut fields = vec![
            "series_id",
            "customer_id",
            "issue_date",
            "due_date",
            "description",
        ];
        if id.is_none() {
            fields.push("kind");
        }
        let defaults = json!({"status":"draft","currency_code":book.currency_code,"subtotal":"0","tax_total":"0","total":"0"});
        let mut data = v::merge(Some(old.as_ref().unwrap_or(&defaults)), attrs, &fields)?;
        v::choice(&mut data, "kind", Self::document_kinds())?;
        if data["kind"] == "credit_note" {
            data = v::merge(Some(&data), attrs, &["credited_document_id"])?;
            v::integer(&mut data, "credited_document_id", true)?;
        }
        v::integer(&mut data, "series_id", true)?;
        v::integer(&mut data, "customer_id", true)?;
        v::text(&mut data, "description", false, 500)?;
        v::date(&mut data, "issue_date")?;
        v::date(&mut data, "due_date")?;
        if let (Some(issue), Some(due)) = (data["issue_date"].as_str(), data["due_date"].as_str()) {
            if due < issue {
                return Err(invalid("due_date", "cannot be before the issue date"));
            }
        }
        let series = store::find(client, SERIES, tenant, company, data["series_id"].as_i64())
            .await?
            .ok_or_else(|| invalid("series_id", "is not a series of this company"))?;
        if series["kind"] != data["kind"] {
            return Err(invalid(
                "series_id",
                format!(
                    "does not number {} documents",
                    data["kind"].as_str().expect("kind")
                ),
            ));
        }
        if series["active"] != true {
            return Err(invalid("series_id", "is an inactive series"));
        }
        let customer = store::find(
            client,
            CUSTOMERS,
            tenant,
            company,
            data["customer_id"].as_i64(),
        )
        .await?
        .ok_or_else(|| invalid("customer_id", "is not a customer of this company"))?;
        if data["kind"] == "invoice" {
            // A new invoice needs a customer linked to an active company and
            // available for Finance; a credit note may name any customer.
            self.invoiceable(
                client,
                scope,
                company,
                &self.customer(client, scope, customer.clone()).await?,
            )
            .await?;
        }
        if data["kind"] == "credit_note" {
            let credited = store::find(
                client,
                DOCUMENTS,
                tenant,
                company,
                data["credited_document_id"].as_i64(),
            )
            .await?;
            if !credited.is_some_and(|d| {
                d["kind"] == "invoice"
                    && d["status"] == "issued"
                    && d["customer_id"] == data["customer_id"]
            }) {
                return Err(invalid(
                    "credited_document_id",
                    "is not an issued invoice of this customer",
                ));
            }
        }
        // None preserves the stored lines AND their frozen tax/account snapshots.
        let replacement = if !attrs["lines"].is_null() || id.is_none() {
            let tax_codes = store::list(client, TAX, tenant, company).await?;
            let (lines, taxes, subtotal, tax_total, total) = calculate(
                &attrs["lines"],
                &setup,
                &chart,
                &tax_codes,
                book.minor_units,
            )?;
            data["subtotal"] = json!(subtotal);
            data["tax_total"] = json!(tax_total);
            data["total"] = json!(total);
            Some((lines, taxes))
        } else {
            None
        };
        let before = if let Some(id) = id {
            Some(read(client, tenant, company, id).await?)
        } else {
            None
        };
        let accounts: Vec<_> = chart
            .into_iter()
            .filter(|a| {
                replacement.as_ref().is_some_and(|(lines, taxes)| {
                    lines
                        .iter()
                        .chain(taxes)
                        .any(|line| line["account_id"].as_i64() == Some(a.id))
                })
            })
            .collect();
        Ok(Prepared {
            before: old,
            after: data,
            replacement,
            // Normalized replacement lines already bind any defaults used.
            // Header-only edits never depend on the setup's account choices.
            facts: json!({"book":book,"accounts":accounts,"series":series,"customer":customer,"document":before}),
        })
    }

    pub(super) async fn save_draft_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Document> {
        let Prepared {
            before: old,
            after: data,
            replacement,
            ..
        } = self
            .prepare_draft(tx.sql(), scope, company, id, attrs)
            .await?;
        let tenant = scope.tenant();
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    let new = store::save(sql, DOCUMENTS, tenant, company, old, data, &mut changes)
                        .await?;
                    let id = new["id"].as_i64().ok_or(Error::Internal)?;
                    if let Some((lines, taxes)) = replacement {
                        store::delete_children(sql, LINES, tenant, company, id, &mut changes)
                            .await?;
                        store::delete_children(sql, TAX_LINES, tenant, company, id, &mut changes)
                            .await?;
                        for (table, rows) in [(LINES, lines), (TAX_LINES, taxes)] {
                            for mut row in rows {
                                row["document_id"] = json!(id);
                                store::save(sql, table, tenant, company, None, row, &mut changes)
                                    .await?;
                            }
                        }
                    }
                    Ok((id, changes))
                })
            })
            .await?;
        let result = read(tx.sql(), tenant, company, id).await?;
        Ok(result)
    }
}

pub(super) async fn draft(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<Value> {
    let document = store::find(client, DOCUMENTS, tenant, company, Some(id))
        .await?
        .ok_or(R::DocumentNotFound)?;
    if document["status"] != "draft" {
        return Err(R::NotADraft);
    }
    Ok(document)
}
pub(crate) async fn read(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<Document> {
    let row = client
        .query_opt(&format!("{READ} AND d.id=$3"), &[&tenant, &company, &id])
        .await?
        .ok_or(R::DocumentNotFound)?;
    Ok(store::decode(row.get(0))?)
}

type Calculation = (Vec<Value>, Vec<Value>, Money, Money, Money);
fn calculate(
    input: &Value,
    setup: &Value,
    chart: &[Account],
    codes: &[Value],
    units: MinorUnits,
) -> Result<Calculation> {
    let inputs = match input {
        Value::Null => Vec::new(),
        Value::Array(rows) => rows.clone(),
        other => vec![other.clone()],
    };
    let mut lines = Vec::new();
    let mut taxes: BTreeMap<String, Value> = BTreeMap::new();
    let mut subtotal = Money::ZERO;
    let mut tax_total = Money::ZERO;
    for (index, input) in inputs.iter().enumerate() {
        let line =
            calculate_line(input, setup, chart, codes, units, index + 1).map_err(|e| match e {
                R::Invalid(mut errors) => {
                    for error in &mut errors {
                        error.line = Some(index + 1);
                    }
                    R::Invalid(errors)
                }
                other => other,
            })?;
        let amount = stored_money(&line["amount"], units)?;
        let tax = stored_money(&line["tax_amount"], units)?;
        subtotal = add(subtotal, amount, units)?;
        tax_total = add(tax_total, tax, units)?;
        if let Some(id) = line["tax_code_id"].as_i64() {
            let code = codes
                .iter()
                .find(|c| c["id"] == id)
                .expect("validated tax code");
            let group = taxes.entry(code["code"].as_str().expect("stored code").into()).or_insert_with(|| json!({"tax_code_id": id, "tax_code": code["code"], "tax_rate": code["rate"], "account_id": code["account_id"], "taxable_amount":"0", "tax_amount":"0"}));
            group["taxable_amount"] = json!(add(
                stored_money(&group["taxable_amount"], units)?,
                amount,
                units
            )?);
            group["tax_amount"] =
                json!(add(stored_money(&group["tax_amount"], units)?, tax, units)?);
        }
        lines.push(line);
    }
    let total = add(subtotal, tax_total, units)?;
    Ok((
        lines,
        taxes.into_values().collect(),
        subtotal,
        tax_total,
        total,
    ))
}
fn calculate_line(
    input: &Value,
    setup: &Value,
    chart: &[Account],
    codes: &[Value],
    units: MinorUnits,
    number: usize,
) -> Result<Value> {
    let empty = json!({});
    let mut line = v::merge(
        None,
        if input.is_object() { input } else { &empty },
        &[
            "description",
            "quantity",
            "unit_price",
            "account_id",
            "tax_code_id",
        ],
    )?;
    v::text(&mut line, "description", true, 500)?;
    let limit = Decimal::from(10_000_000_000_000_000_i64);
    let quantity = v::decimal(&mut line, "quantity", true, limit, 4)?;
    let price = v::decimal(&mut line, "unit_price", false, limit, 4)?;
    v::integer(&mut line, "account_id", false)?;
    if line["account_id"].is_null() {
        line["account_id"] = setup["income_account_id"].clone();
    }
    v::account(&mut line, "account_id", chart, AccountType::Income)?;
    let tax_id = v::integer(&mut line, "tax_code_id", false)?;
    let rate = if let Some(id) = tax_id {
        let code = codes
            .iter()
            .find(|c| c["id"] == id)
            .ok_or_else(|| invalid("tax_code_id", "is not a tax code of this company"))?;
        if code["active"] != true {
            return Err(invalid("tax_code_id", "is an inactive tax code"));
        }
        Some(store::decode::<Decimal>(code["rate"].clone())?)
    } else {
        None
    };
    let product = quantity
        .checked_mul(price)
        .ok_or_else(|| invalid("amount", "must be less than 1E+16"))?;
    let amount = rounded(product, units, "amount")?;
    let tax = if let Some(rate) = rate {
        rounded(
            amount
                .amount()
                .checked_mul(rate)
                .and_then(|v| v.checked_div(Decimal::from(100)))
                .ok_or_else(|| invalid("total", "must be less than 1E+16"))?,
            units,
            "total",
        )?
    } else {
        Money::ZERO
    };
    line["line_number"] = json!(number);
    line["amount"] = json!(amount);
    line["tax_rate"] = json!(rate);
    line["tax_amount"] = json!(tax);
    Ok(line)
}
fn rounded(value: Decimal, units: MinorUnits, field: &str) -> Result<Money> {
    Money::new(
        value.round_dp_with_strategy(
            u32::from(units.get()),
            RoundingStrategy::MidpointAwayFromZero,
        ),
        units,
    )
    .map_err(|e| invalid(field, e.to_string()))
}
fn stored_money(value: &Value, units: MinorUnits) -> Result<Money> {
    Money::new(store::decode(value.clone())?, units).map_err(|_| R::Failed(Error::Internal))
}
fn add(left: Money, right: Money, units: MinorUnits) -> Result<Money> {
    Money::new(
        left.amount()
            .checked_add(right.amount())
            .ok_or_else(|| invalid("total", "must be less than 1E+16"))?,
        units,
    )
    .map_err(|e| invalid("total", e.to_string()))
}
