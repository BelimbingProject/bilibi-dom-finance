//! Commerce's documented v1 events, kept as Finance-owned facts. Delivery
//! only copies parts; the scheduled worker uses the normal authorized draft
//! and customer paths. No publisher dependency and no borrowed user authority.
use crate::{
    documents, invalid, store, Document, ReceivableError as R, Receivables, Result,
    MANAGE_DOCUMENTS, MODULE, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_authz::SystemScope;
use bilibi_database::{Error, MutationTransaction};
use bilibi_events::{Event, On, Subscribe, Subscriber, SubscriberFuture, SUBSCRIBERS};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, CAPABILITY_POLICIES,
    HANDLERS, WORKERS,
};
use bilibi_schedule::{Definition, Misfire, Overlap, Reach, Schedule, DEFINITIONS, EXECUTE};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Transaction;

const CONFIRMED: &str = "commerce/sales.order_confirmed";
const STATUS: &str = "commerce/sales.order_status_changed";
const WORKER: &str = "finance/receivables-order-invoices";
pub(crate) const JOB_SERVICE: &str = "finance/receivables.order_invoices";
macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

pub(super) const ORDERS: store::Table = store::Table {
    name: "finance_receivables_sales_orders",
    kind: "finance.receivables.sales_order",
    columns: &[
        "order_id",
        "order_number",
        "revision",
        "status",
        "part_count",
        "facts",
        "issue_date",
        "document_id",
        "waiting_reason",
        "attempted_at",
    ],
    timestamps: false,
};
const PARTS: store::Table = store::Table {
    name: "finance_receivables_sales_order_parts",
    kind: "finance.receivables.sales_order_part",
    columns: &["sales_order_id", "part_index", "event_key", "payload"],
    timestamps: false,
};

/// A copied order and the invoice, or the reason it still needs attention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SalesOrder {
    pub id: i64,
    pub order_id: i64,
    pub order_number: String,
    pub status: String,
    pub document_id: Option<i64>,
    pub waiting_reason: String,
}
#[derive(Deserialize)]
struct Snapshot {
    company_id: i64,
    order_id: i64,
    order_number: String,
    revision: i64,
    order_status: String,
    customer_kind: String,
    customer_id: i64,
    currency_code: String,
    subtotal: String,
    tax_total: String,
    total: String,
    part_index: i32,
    part_count: i32,
    lines: Vec<Line>,
}
#[derive(Deserialize)]
struct Line {
    line_number: i32,
    description: String,
    quantity: String,
    unit_price: String,
    tax_code: String,
    tax_rate: String,
    subtotal: String,
    tax_total: String,
    total: String,
}
fn snapshot(payload: &Value) -> bilibi_database::Result<Snapshot> {
    let s: Snapshot = serde_json::from_value(payload.clone()).map_err(|_| Error::Invalid)?;
    if s.company_id <= 0
        || s.order_id <= 0
        || s.customer_id <= 0
        || s.revision <= 0
        || !matches!(s.customer_kind.as_str(), "company" | "person")
        || !matches!(s.order_status.as_str(), "confirmed" | "cancelled")
        || !(1..=100).contains(&s.part_count)
        || !(1..=s.part_count).contains(&s.part_index)
        || s.lines.len() != 1
        || s.lines[0].line_number != s.part_index
        || s.order_number.is_empty()
        || s.order_number.len() > 100
    {
        return Err(Error::Invalid);
    }
    Ok(s)
}
fn header(payload: &Value) -> Value {
    let mut facts = payload.clone();
    for key in ["lines", "part_index", "revision", "order_status"] {
        facts.as_object_mut().expect("snapshot object").remove(key);
    }
    facts
}
inventory::submit! { Contribution::new(SUBSCRIBERS, "finance/receivables.sales_orders", provenance!(), &Subscriber {
    events: &[On { event: CONFIRMED, versions: &[1] }, On { event: STATUS, versions: &[1] }], handler: &Orders,
}) }
struct Orders;
impl Subscribe for Orders {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a SystemScope,
        event: &'a Event,
    ) -> SubscriberFuture<'a> {
        Box::pin(async move {
            let s = snapshot(&event.payload)?;
            if event.version != 1 || event.tenant != scope.tenant() {
                return Err(Error::Invalid);
            }
            let tenant = scope.tenant();
            // Same lock as all company ledger writes, including the worker.
            tx.query_opt(
                "SELECT id FROM companies WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                &[&tenant, &s.company_id],
            )
            .await?
            .ok_or(Error::NotFound)?;
            let old=tx.query_opt("SELECT to_jsonb(o) FROM finance_receivables_sales_orders o WHERE tenant_id=$1 AND company_id=$2 AND order_id=$3", &[&tenant,&s.company_id,&s.order_id]).await?.map(|r| r.get::<_,Value>(0));
            let facts = header(&event.payload);
            let mut changes = Vec::new();
            let mut order = if let Some(old) = old {
                if old["facts"] != facts {
                    return Err(Error::Conflict);
                }
                old
            } else {
                store::save(tx,ORDERS,tenant,s.company_id,None,json!({"order_id":s.order_id,"order_number":s.order_number,"revision":s.revision,"status":s.order_status,"part_count":s.part_count,"facts":facts,"issue_date":event.recorded_at.get(..10).ok_or(Error::Invalid)?,"document_id":null,"waiting_reason":"Waiting for all order lines.","attempted_at":null}),&mut changes).await?
            };
            if s.revision > order["revision"].as_i64().ok_or(Error::Internal)? {
                let old = order.clone();
                order["revision"] = json!(s.revision);
                order["status"] = json!(s.order_status);
                if s.order_status == "cancelled" {
                    order["waiting_reason"] =
                        json!("The sales order was cancelled. Review any invoice already created.");
                }
                order = store::save(
                    tx,
                    ORDERS,
                    tenant,
                    s.company_id,
                    Some(old),
                    order,
                    &mut changes,
                )
                .await?;
            }
            if event.name == CONFIRMED {
                if s.order_status != "confirmed" {
                    return Err(Error::Invalid);
                }
                let id = order["id"].as_i64().ok_or(Error::Internal)?;
                if let Some(existing)=tx.query_opt("SELECT payload FROM finance_receivables_sales_order_parts WHERE tenant_id=$1 AND company_id=$2 AND sales_order_id=$3 AND part_index=$4", &[&tenant,&s.company_id,&id,&s.part_index]).await? {
                    if existing.get::<_,Value>(0)!=event.payload { return Err(Error::Conflict); }
                } else {
                    store::save(tx,PARTS,tenant,s.company_id,None,json!({"sales_order_id":id,"part_index":s.part_index,"event_key":event.key,"payload":event.payload}),&mut changes).await?;
                }
            }
            if order["status"] == "confirmed"
                && order["document_id"].is_null()
                && order["waiting_reason"] == "Waiting for all order lines."
            {
                let id = order["id"].as_i64().ok_or(Error::Internal)?;
                let count: i64=tx.query_one("SELECT count(*) FROM finance_receivables_sales_order_parts WHERE tenant_id=$1 AND company_id=$2 AND sales_order_id=$3", &[&tenant,&s.company_id,&id]).await?.get(0);
                if count == i64::from(s.part_count) {
                    let mut after = order.clone();
                    after["waiting_reason"]=json!("Waiting for invoice preparation. Check scheduled work or prepare the invoice here.");
                    store::save(
                        tx,
                        ORDERS,
                        tenant,
                        s.company_id,
                        Some(order),
                        after,
                        &mut changes,
                    )
                    .await?;
                }
            }
            Ok(changes)
        })
    }
}

impl Receivables {
    /// Bounded source rows, authorized for the explicit company, even without Commerce.
    pub async fn sales_orders(
        &self,
        scope: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<SalesOrder>> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.sales_orders_on(&**client, scope, company, after, limit)
            .await
    }
    pub(super) async fn sales_orders_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<SalesOrder>> {
        self.authorize_on(client, scope, company, VIEW).await?;
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(R::InvalidOption);
        }
        client.query("SELECT to_jsonb(o) FROM finance_receivables_sales_orders o WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4", &[&scope.tenant(),&company,&after,&limit]).await?.iter().map(|r|store::decode(r.get(0)).map_err(R::from)).collect()
    }
    pub(super) async fn sales_order_page(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        page: i64,
    ) -> Result<Value> {
        self.authorize_on(client, scope, company, VIEW).await?;
        if page < 1 {
            return Err(R::InvalidOption);
        }
        let tenant = scope.tenant();
        let total: i64=client.query_opt("SELECT count(*) FROM finance_receivables_sales_orders WHERE tenant_id=$1 AND company_id=$2 AND document_id IS NULL AND status='confirmed'", &[&tenant,&company]).await?.ok_or(Error::Internal)?.get(0);
        let page = page.min(((total + 24) / 25).max(1));
        let offset = (page - 1) * 25;
        let rows: Vec<SalesOrder>=client.query("SELECT to_jsonb(o) FROM finance_receivables_sales_orders o WHERE tenant_id=$1 AND company_id=$2 AND document_id IS NULL AND status='confirmed' ORDER BY id LIMIT 25 OFFSET $3", &[&tenant,&company,&offset]).await?.iter().map(|r|store::decode(r.get(0))).collect::<bilibi_database::Result<_>>()?;
        Ok(json!({"records":rows,"total":total,"page":page}))
    }
    pub(super) async fn order_review(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
        series: Option<i64>,
    ) -> Result<Value> {
        self.join(client, scope, company, MANAGE_DOCUMENTS).await?;
        let tenant = scope.tenant();
        let row = store::find(client, ORDERS, tenant, company, Some(id))
            .await?
            .ok_or_else(|| invalid("order_id", "is not a copied sales order of this company"))?;
        if row["document_id"].is_number() {
            return Ok(json!({"order":row}));
        }
        if row["status"] != "confirmed" {
            return Err(invalid("order_id", "the sales order was cancelled"));
        }
        let parts=client.query("SELECT payload FROM finance_receivables_sales_order_parts WHERE tenant_id=$1 AND company_id=$2 AND sales_order_id=$3 ORDER BY part_index", &[&tenant,&company,&id]).await?;
        if parts.len() != row["part_count"].as_u64().ok_or(Error::Internal)? as usize {
            return Err(invalid("order_id", "wait for all order lines to arrive"));
        }
        let first: Value = parts[0].get(0);
        let s = snapshot(&first)?;
        let book = self
            .ledger
            .book_figures_on(client, scope, company, MANAGE_DOCUMENTS)
            .await?;
        if book.currency_code != s.currency_code {
            return Err(invalid(
                "order_id",
                "the order currency differs from the book currency; no conversion was made",
            ));
        }
        let setup = store::find(client, store::SETUPS, tenant, company, None)
            .await?
            .ok_or(R::NotConfigured)?;
        let chart = self
            .ledger
            .list_accounts_for_on(client, scope, company, MANAGE_DOCUMENTS)
            .await?;
        let codes = store::list(client, store::TAX, tenant, company).await?;
        let series_rows = store::list(client, store::SERIES, tenant, company).await?;
        let eligible: Vec<_> = series_rows
            .iter()
            .filter(|r| {
                r["kind"] == "invoice"
                    && r["active"] == true
                    && series.is_none_or(|id| r["id"] == id)
            })
            .collect();
        if eligible.len() != 1 {
            return Err(invalid("series_id","choose one active invoice series; automatic drafts need exactly one active invoice series"));
        }
        let mut lines = Vec::new();
        let mut source_lines = Vec::new();
        for (index, p) in parts.iter().enumerate() {
            let payload: Value = p.get(0);
            let part = snapshot(&payload)?;
            if header(&payload) != row["facts"] || part.part_index as usize != index + 1 {
                return Err(invalid(
                    "order_id",
                    "the order lines do not match the confirmed order",
                ));
            }
            for l in part.lines {
                let code = codes
                    .iter()
                    .find(|c| c["code"] == l.tax_code && c["active"] == true)
                    .ok_or_else(|| {
                        invalid(
                            "tax_code",
                            "add or activate each order tax code in Receivables settings",
                        )
                    })?;
                let rate: Decimal = store::decode(code["rate"].clone())?;
                if l.tax_rate.parse::<Decimal>().ok() != Some(rate) {
                    return Err(invalid(
                        "tax_code",
                        "the order tax rate differs from the Receivables tax code",
                    ));
                }
                source_lines
                    .push(json!({"subtotal":l.subtotal,"tax_total":l.tax_total,"total":l.total}));
                lines.push(json!({"description":l.description,"quantity":l.quantity,"unit_price":l.unit_price,"tax_code_id":code["id"]}));
            }
        }
        let customer=client.query_opt("SELECT to_jsonb(c) FROM finance_receivables_customers c WHERE tenant_id=$1 AND company_id=$2 AND record_kind=$3 AND record_id=$4", &[&tenant,&company,&s.customer_kind,&s.customer_id.to_string()]).await?.map(|r|r.get::<_,Value>(0));
        let target = json!({"record_kind":s.customer_kind,"record_id":s.customer_id});
        let creation = if customer.is_none() {
            let p = self
                .prepare_code(client, scope, company, store::CUSTOMERS, None, &target)
                .await?;
            Some(json!({"customer":p.after,"target":p.target}))
        } else {
            None
        };
        let issue = row["issue_date"].as_str().ok_or(Error::Internal)?;
        let attrs = json!({"kind":"invoice","series_id":eligible[0]["id"],"customer_id":customer.as_ref().map(|c|c["id"].clone()),"issue_date":issue,"description":format!("Sales order {}",s.order_number),"lines":lines});
        // Bind every default and source fact in a reviewed plan. Commit repeats
        // this read on its own transaction, then uses the normal draft rules.
        Ok(
            json!({"order":row,"book":book,"setup":setup,"chart":chart,"codes":codes,"series":eligible[0],"customer":customer,"creation":creation,"target":target,"attrs":attrs,"source_lines":source_lines,"totals":[s.subtotal,s.tax_total,s.total]}),
        )
    }
    pub(super) async fn invoice_order_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
        series: Option<i64>,
    ) -> Result<Document> {
        let review = self
            .order_review(tx.sql(), scope, company, id, series)
            .await?;
        let old = review["order"].clone();
        if let Some(document) = old["document_id"].as_i64() {
            return documents::read(tx.sql(), scope.tenant(), company, document).await;
        }
        let mut attrs = review["attrs"].clone();
        if attrs["customer_id"].is_null() {
            let c = self
                .save_customer_on(tx, scope, company, None, &review["target"])
                .await?;
            attrs["customer_id"] = json!(c.id);
        }
        let prepared = self
            .prepare_draft(tx.sql(), scope, company, None, &attrs)
            .await?;
        let amount = |v: &Value| -> Result<Decimal> { store::decode(v.clone()).map_err(R::from) };
        for (field, index) in [("subtotal", 0), ("tax_total", 1), ("total", 2)] {
            if amount(&prepared.after[field])? != amount(&review["totals"][index])? {
                return Err(invalid("order_id","the order totals differ from the invoice totals; review currency precision and tax codes"));
            }
        }
        for (invoice, source) in prepared
            .replacement
            .as_ref()
            .ok_or(Error::Internal)?
            .0
            .iter()
            .zip(review["source_lines"].as_array().ok_or(Error::Internal)?)
        {
            if amount(&invoice["amount"])? != amount(&source["subtotal"])?
                || amount(&invoice["tax_amount"])? != amount(&source["tax_total"])?
                || amount(&invoice["amount"])? + amount(&invoice["tax_amount"])?
                    != amount(&source["total"])?
            {
                return Err(invalid(
                    "order_id",
                    "an order line total differs from the invoice line total",
                ));
            }
        }
        let document = self.save_draft_on(tx, scope, company, None, &attrs).await?;
        let tenant = scope.tenant();
        let document_id = document.id;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut after = old.clone();
                after["document_id"] = json!(document_id);
                after["waiting_reason"] = json!("");
                let mut changes = Vec::new();
                store::save(sql, ORDERS, tenant, company, Some(old), after, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        Ok(document)
    }
    /// Creates at most one invoice per copied order. A refusal rolls customer,
    /// relationship and draft back together, leaving the source available.
    pub async fn invoice_sales_order(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        series: Option<i64>,
    ) -> Result<Document> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                MANAGE_DOCUMENTS,
                "finance.receivables.invoice_sales_order",
            )
            .await?;
        let document = self
            .invoice_order_on(&mut tx, scope, company, id, series)
            .await?;
        tx.commit().await?;
        Ok(document)
    }
}

fn no_args(args: Value) -> bilibi_database::Result<Value> {
    if args.as_object().is_some_and(|o| o.is_empty()) {
        Ok(args)
    } else {
        Err(Error::Invalid)
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 5, validate: |args| bilibi_schedule::worker::validate(args,no_args) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: JOB_SERVICE, capability: EXECUTE }) }
inventory::submit! { Contribution::new(CAPABILITY_POLICIES, WORKER, provenance!(), &bilibi_schedule::TENANT_POLICY) }
inventory::submit! { Contribution::new(DEFINITIONS, WORKER, provenance!(), &Definition {
    name: "Draft invoices from sales orders", expression: "* * * * *", timezone: "Etc/UTC", task_name: "Receivables order invoices", worker: WORKER, args: || json!({}), overlap: Overlap::Forbid, misfire: Misfire::Coalesce, reach: Reach::EachTenant, owner_route: Some("/finance/receivables/invoices"),
}) }
pub(crate) struct InvoiceOrders {
    pub receivables: Arc<Receivables>,
    pub schedule: Arc<Schedule>,
}
impl Handler for InvoiceOrders {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            match self.run(execution).await {
                Ok(()) => Outcome::Completed,
                Err(_) => Outcome::Retry("order_invoices_failed"),
            }
        }))
    }
}
impl InvoiceOrders {
    async fn run(&self, execution: &Execution) -> Result<()> {
        let scope = execution.scope().ok_or(R::Unauthorized)?;
        let owner = &self.receivables;
        let mut client = owner.db.get().await.map_err(Error::from)?;
        let fence = execution
            .begin(&mut client, "finance.receivables.order_invoice_batch")
            .await?;
        // Oldest attempt first prevents one unready order starving later ones.
        let rows=fence.sql().query("SELECT id,company_id FROM finance_receivables_sales_orders WHERE tenant_id=$1 AND status='confirmed' AND document_id IS NULL ORDER BY attempted_at NULLS FIRST,id LIMIT 100", &[&scope.tenant()]).await.map_err(Error::from)?;
        for row in rows {
            let id: i64 = row.get(0);
            let company: i64 = row.get(1);
            match owner.invoice_sales_order(scope, company, id, None).await {
                Ok(_) => {}
                Err(R::Failed(e)) => return Err(R::Failed(e)),
                Err(e) => {
                    // Normal business refusals are visible work, not failed
                    // event delivery. This write has its own company proof.
                    let reason=match e { R::Invalid(fields)=>fields.iter().map(|f|f.message.as_str()).collect::<Vec<_>>().join("; "), R::Unauthorized=>"An administrator must grant the scheduler permission to prepare invoices and customers.".into(), other=>other.to_string() };
                    let mut c = owner.db.get().await.map_err(Error::from)?;
                    // A missing grant must not be bypassed to write a reason.
                    let mut tx = match owner
                        .write(
                            &mut c,
                            scope,
                            company,
                            MANAGE_DOCUMENTS,
                            "finance.receivables.order_invoice_waiting",
                        )
                        .await
                    {
                        Ok(tx) => tx,
                        Err(R::Unauthorized | R::CompanyArchived | R::CompanyNotFound) => continue,
                        Err(e) => return Err(e),
                    };
                    {
                        let old = store::find(tx.sql(), ORDERS, scope.tenant(), company, Some(id))
                            .await?
                            .ok_or(Error::NotFound)?;
                        if old["document_id"].is_null() {
                            let tenant = scope.tenant();
                            let stamp: String=tx.sql().query_one("SELECT to_char(timezone('UTC',clock_timestamp()),'YYYY-MM-DD HH24:MI:SS.US')", &[]).await.map_err(Error::from)?.get(0);
                            tx.mutate(move |sql| {
                                Box::pin(async move {
                                    let mut after = old.clone();
                                    after["waiting_reason"] = json!(reason);
                                    after["attempted_at"] = json!(stamp);
                                    let mut changes = Vec::new();
                                    store::save(
                                        sql,
                                        ORDERS,
                                        tenant,
                                        company,
                                        Some(old),
                                        after,
                                        &mut changes,
                                    )
                                    .await?;
                                    Ok(((), changes))
                                })
                            })
                            .await?;
                        }
                        tx.commit().await?;
                    }
                }
            }
        }
        fence.commit().await?;
        Ok(())
    }
}
