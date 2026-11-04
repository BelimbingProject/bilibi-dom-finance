//! One reviewed door for retries from the invoice list and from agents.
use crate::{agent_operations::failure, Receivables, MANAGE_DOCUMENTS, MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
static OWNER: OnceLock<Arc<Receivables>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Receivables>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate receivables order binding".into()))
}
fn owner() -> bilibi_agent_api::Result<&'static Receivables> {
    OWNER.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
fn id(input: &Value, key: &str) -> bilibi_agent_api::Result<i64> {
    input[key]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(Error::Invalid.into())
}
fn list_input() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1},"after":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn page_input() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1},"page":{"type":"integer","minimum":1}}})
}
fn draft_input() -> Value {
    json!({"type":"object","required":["company_id","sales_order_id"],"properties":{"company_id":{"type":"integer","minimum":1},"sales_order_id":{"type":"integer","minimum":1},"series_id":{"type":"integer","minimum":1}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
struct Orders;
struct OrderPage;
impl ReadHandler for OrderPage {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            Ok(owner()?
                .sales_order_page(
                    client,
                    scope,
                    id(input, "company_id")?,
                    input["page"].as_i64().unwrap_or(1),
                )
                .await
                .map_err(|e| failure(e, "lines"))?
                .into())
        })
    }
}
impl ReadHandler for Orders {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let rows = owner()?
                .sales_orders_on(
                    client,
                    scope,
                    id(input, "company_id")?,
                    input["after"].as_i64().unwrap_or(0),
                    input["page_size"].as_i64().unwrap_or(100),
                )
                .await
                .map_err(|e| failure(e, "lines"))?;
            Ok(json!({"next_after_id":rows.last().map(|r|r.id),"records":rows}).into())
        })
    }
}
struct Draft;
impl WriteHandler for Draft {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let review = owner()?
                .order_review(
                    client,
                    scope,
                    id(input, "company_id")?,
                    id(input, "sales_order_id")?,
                    input["series_id"].as_i64(),
                )
                .await
                .map_err(|e| failure(e, "lines"))?;
            crate::agent_operations::fence(
                client,
                &json!({"action":"invoice_sales_order","input":input,"review":review}),
                false,
            )
            .await?;
            Ok(review.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = id(input, "company_id")?;
            let order = id(input, "sales_order_id")?;
            let series = input["series_id"].as_i64();
            let review = owner()?
                .order_review(tx.sql(), scope, company, order, series)
                .await
                .map_err(|e| failure(e, "lines"))?;
            crate::agent_operations::fence(
                tx.sql(),
                &json!({"action":"invoice_sales_order","input":input,"review":review}),
                true,
            )
            .await?;
            let document = owner()?
                .invoice_order_on(tx, scope, company, order, series)
                .await
                .map_err(|e| failure(e, "lines"))?;
            Ok(json!({"record":document,"page":format!("/finance/receivables/invoices/{}",document.id)}).into())
        })
    }
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/receivables.order_operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {
    operations:&[
        Operation {key:"finance.receivables.sales_order.page",title:"Read orders waiting for invoicing",summary:"Read a bounded page of complete or incomplete confirmed sales orders without an invoice, with reasons for waiting.",keywords:&[],capability:VIEW,input:page_input,output,handler:Handler::Read(&OrderPage),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.sales_order.list",title:"List orders for invoicing",summary:"Read copied sales orders, their invoice links and reasons for waiting. Commerce need not be mounted.",keywords:&[],capability:VIEW,input:list_input,output,handler:Handler::Read(&Orders),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.sales_order.invoice",title:"Prepare an invoice from a sales order",summary:"Create one draft from all confirmed order lines with matching currency, tax codes and totals. Create a Finance customer through the normal rules if permitted. Repeating returns the same invoice, even if discarded.",keywords:&[],capability:MANAGE_DOCUMENTS,input:draft_input,output,handler:Handler::SafeWrite(&Draft),channels:Channels::Every,step_up:&[]},
    ],guides:&[],
}) }
