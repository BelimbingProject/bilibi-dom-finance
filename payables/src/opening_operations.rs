//! Conversion contracts share the owner path with the UI. The transaction-local
//! review fence prevents earlier plan steps from changing reviewed opening facts.
use crate::*;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use serde_json::{json, Value};
use std::sync::OnceLock;
static OWNER: OnceLock<Arc<Payables>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Payables>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate opening import binding".into()))
}
fn owner() -> Result<&'static Payables> {
    OWNER
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| Error::Internal.into())
}
fn failure(e: PayablesError) -> bilibi_agent_api::Error {
    crate::agent_operations::error(e, "csv")
}
fn company(input: &Value) -> Result<i64> {
    input["company_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid("company_id", "must identify the current company"))
}
fn target() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1}}})
}
fn context_schema() -> Value {
    let mut value = target();
    let properties = value["properties"]
        .as_object_mut()
        .expect("schema properties");
    properties.insert("items_after".into(), json!({"type":"integer","minimum":0,"description":"Return imported items after this item id; pass the previous items_next."}));
    properties.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":500,"description":"Items per page, default 200."}));
    value
}
fn base_schema() -> Value {
    json!({"type":"object","required":["company_id","source_reference","currency_code","csv","opening_account_classifications"],"properties":{"company_id":{"type":"integer","minimum":1},"source_reference":{"type":"string","maxLength":100},"currency_code":{"type":"string","maxLength":3},"csv":{"type":"string","maxLength":131072,"description":"UTF-8 CSV; obtain the exact required header from opening.context. Import the complete opening register, at most 500 rows."},"opening_account_classifications":{"type":"array","items":{"type":"object","required":["account_id","classification"],"properties":{"account_id":{"type":"integer","minimum":1},"classification":{"type":"string","enum":["receivables_control","payables_control","not_a_control"]}}},"description":"Classify every non-zero asset and liability returned by opening.context."},"override_reason":{"type":"string","maxLength":500}}})
}
fn schema() -> Value {
    let mut value = base_schema();
    let properties = value["properties"]
        .as_object_mut()
        .expect("schema properties");
    properties.insert(
        "part".into(),
        json!({"type":"integer","minimum":1,"maximum":200}),
    );
    properties.insert(
        "part_count".into(),
        json!({"type":"integer","minimum":1,"maximum":200}),
    );
    properties.insert("staged".into(),json!({"type":"boolean","description":"Reconcile retained source batches; pass an empty csv."}));
    properties.insert(
        "replaces_batch_id".into(),
        json!({"type":"integer","minimum":1}),
    );
    properties.insert("reason".into(), json!({"type":"string","maxLength":500}));
    value
}

fn stage_schema() -> Value {
    let mut value = schema();
    value["required"]
        .as_array_mut()
        .expect("schema required")
        .extend([json!("part"), json!("part_count")]);
    value
}
fn output() -> Value {
    json!({"type":"object"})
}
async fn fence(
    c: &dyn OperationClient,
    facts: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let row=c.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'),COALESCE(NULLIF(current_setting('finance.payables_reviews',true),''),'[]')::jsonb",&[&facts.to_string()]).await?.ok_or(Error::Internal)?;
    let digest: String = row.get(0);
    let mut pending: Vec<String> =
        serde_json::from_value(row.get(1)).map_err(|_| Error::Internal)?;
    if consume {
        if pending.first() != Some(&digest) {
            return Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict,
            ));
        }
        pending.remove(0);
    } else {
        pending.push(digest);
    }
    c.query_opt(
        "SELECT set_config('finance.payables_reviews',$1,true)",
        &[&json!(pending).to_string()],
    )
    .await?;
    Ok(())
}
struct Context;
impl ReadHandler for Context {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            Ok(owner()
                .map_err(failure)?
                .opening_context(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?
                .into())
        })
    }
}
struct Import;
impl WriteHandler for Import {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let resolved = owner()
                .map_err(failure)?
                .resolve_opening(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?;
            let prepared = owner()
                .map_err(failure)?
                .prepare_opening(c, s, company(input).map_err(failure)?, &resolved)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":prepared.review}), false).await?;
            Ok(json!({"changes":prepared.review}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let owner = owner().map_err(failure)?;
            let company = company(input).map_err(failure)?;
            let resolved = owner
                .resolve_opening(tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            let prepared = owner
                .prepare_opening(tx.sql(), s, company, &resolved)
                .await
                .map_err(failure)?;
            fence(
                tx.sql(),
                &json!({"input":input,"review":prepared.review}),
                true,
            )
            .await?;
            let record = owner
                .import_opening_on(tx, s, company, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record,"path":"/finance/payables/bills?source=opening"}).into())
        })
    }
}
struct Stage;
impl WriteHandler for Stage {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let prepared = owner()
                .map_err(failure)?
                .prepare_opening_stage(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":prepared}), false).await?;
            Ok(json!({"changes":prepared}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let owner = owner().map_err(failure)?;
            let company = company(input).map_err(failure)?;
            let prepared = owner
                .prepare_opening_stage(tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            fence(tx.sql(), &json!({"input":input,"review":prepared}), true).await?;
            let record = owner
                .stage_opening_on(tx, s, company, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record,"path":"/finance/payables/bills?source=opening"}).into())
        })
    }
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/payables.opening",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:"finance.payables.opening.context",title:"Read opening import context",summary:"Read the approved conversion entry, source balances, CSV header and retained import batch for the current company.",keywords:&["conversion"],capability:VIEW,input:context_schema,output,handler:Handler::Read(&Context),channels:Channels::Every,step_up:&[]},
 Operation{key:"finance.payables.opening.stage",title:"Stage opening source batch",summary:"Retain one immutable source batch for a resumable register. Identical retries are safe. Reconcile only after all batches are committed, with approval independent of every staging preparer.",keywords:&["conversion"],capability:POST_DOCUMENTS,input:stage_schema,output,handler:Handler::SafeWrite(&Stage),channels:Channels::Every,step_up:&[]},
 Operation{key:"finance.payables.opening.import",title:"Opening bills",summary:"Preview a complete opening register and exact reconciliation, then approve once. Retain source evidence without a new acquisition, supply or tax posting. Identical retries return the retained batch. Requires approval independent of the opening source preparer.",keywords:&["conversion"],capability:POST_DOCUMENTS,input:schema,output,handler:Handler::SafeWrite(&Import),channels:Channels::Every,step_up:&[]}
],guides:&[bilibi_agent_api::Guide {key:"finance.payables.opening",title:"Opening bills",summary:"Convert source registers without duplicating opening postings.",keywords:&["conversion"],capability:VIEW,body:include_str!("../opening-guide.md")}]})}

// Pages and agent plans use exactly these reviewed, transaction-joining doors.
struct Credit(&'static str);
impl WriteHandler for Credit {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let prepared = owner()
                .map_err(failure)?
                .prepare_credit(c, s, company(input).map_err(failure)?, self.0, input)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":prepared.review}), false).await?;
            Ok(json!({"changes":prepared.review}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let owner = owner().map_err(failure)?;
            let company = company(input).map_err(failure)?;
            let prepared = owner
                .prepare_credit(tx.sql(), s, company, self.0, input)
                .await
                .map_err(failure)?;
            fence(
                tx.sql(),
                &json!({"input":input,"review":prepared.review}),
                true,
            )
            .await?;
            let record = owner
                .settle_credit_on(tx, s, company, self.0, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record}).into())
        })
    }
}
fn credit_schema(action: &str) -> Value {
    let mut required = vec!["company_id", "effective_date", "reference"];
    if action == "reverse" {
        required.push("settlement_id");
    } else {
        required.extend(["credit_document_id", "amount"]);
    }
    if action == "apply" {
        required.push("target_document_id");
    }
    if action == "refund" {
        required.push("cash_account_id");
    }
    json!({"type":"object","required":required,"properties":{
        "company_id":{"type":"integer","minimum":1},
        "credit_document_id":{"type":"integer","minimum":1},
        "target_document_id":{"type":"integer","minimum":1},
        "settlement_id":{"type":"integer","minimum":1},
        "cash_account_id":{"type":"integer","minimum":1},
        "effective_date":{"type":"string","format":"date"},
        "amount":{"type":"string","description":"Positive amount in book minor units."},
        "reference":{"type":"string","maxLength":100},
        "override_reason":{"type":"string","maxLength":500}
    }})
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/payables.credit",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:"finance.payables.credit.apply",title:"Apply supplier credit",summary:"Apply a retained supplier credit to the same party's invoice or bill. Same-control application posts no entry; different controls transfer the amount between saved controls. Rechecks every dated residual.",keywords:&["credit","conversion"],capability:POST_PAYMENTS,input:||credit_schema("apply"),output,handler:Handler::SafeWrite(&Credit("apply")),channels:Channels::Every,step_up:&[]},
 Operation{key:"finance.payables.credit.refund",title:"Receive supplier refund",summary:"Record the refund of a retained supplier credit with its owned cash and control entry, approval and source evidence in one transaction.",keywords:&["credit","refund"],capability:POST_PAYMENTS,input:||credit_schema("refund"),output,handler:Handler::SafeWrite(&Credit("refund")),channels:Channels::Every,step_up:&[]},
 Operation{key:"finance.payables.settlement.reverse",title:"Reverse credit settlement",summary:"Append a dated reversal of an application or refund and reverse its owned entry when present. Keep the original event and restore its item balances.",keywords:&["credit","reversal"],capability:POST_PAYMENTS,input:||credit_schema("reverse"),output,handler:Handler::SafeWrite(&Credit("reverse")),channels:Channels::Every,step_up:&[]}
],guides:&[]})}

struct CreditContext;
impl ReadHandler for CreditContext {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            Ok(owner()
                .map_err(failure)?
                .credit_context(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?
                .into())
        })
    }
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/payables.credit-context",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:"finance.payables.credit.context",title:"Read credit settlement choices",summary:"Read eligible same-party targets and cash accounts. Search document references and page with next_after_id.",keywords:&["credit"],capability:VIEW,input:||{let mut schema=credit_schema("apply");schema["required"]=json!(["company_id","credit_document_id"]);schema["properties"]["search"]=json!({"type":"string","maxLength":100});schema["properties"]["after_id"]=json!({"type":"integer","minimum":0});schema},output,handler:Handler::Read(&CreditContext),channels:Channels::Every,step_up:&[]}
],guides:&[]})}

struct ControlTieOut;
impl ReadHandler for ControlTieOut {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        _c: &'a dyn OperationClient,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let date = input["as_of"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| failure(invalid("as_of", "must be YYYY-MM-DD")))?;
            let pins = crate::BalancePins {
                through_entry_number: input["through_entry_number"].as_i64(),
                through_opening_batch_id: input["through_opening_batch_id"].as_i64(),
                through_settlement_number: input["through_settlement_number"].as_i64(),
            };
            let report = owner()
                .map_err(failure)?
                .tie_out_control_accounts(s, company(input).map_err(failure)?, date, &pins)
                .await
                .map_err(failure)?;
            Ok(serde_json::to_value(report)
                .map_err(|_| Error::Internal)?
                .into())
        })
    }
}
fn tie_out_schema() -> Value {
    let mut s = target();
    s["required"] = json!(["company_id", "as_of"]);
    s["properties"]["as_of"] = json!({"type":"string","format":"date"});
    for key in [
        "through_entry_number",
        "through_opening_batch_id",
        "through_settlement_number",
    ] {
        s["properties"][key] = json!({"type":"integer","minimum":0});
    }
    // The facade enforces the all-or-none tuple because the shared operation
    // schema dialect deliberately has no cross-field combinators.
    s
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/payables.control-tie-out",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:"finance.payables.control.tie_out",title:"Read control account tie-out",summary:"Read each saved control's signed subledger balance, actual ledger balance and other owners' differences at one complete accounting tuple.",keywords:&["credit","conversion","tie-out"],capability:VIEW,input:tie_out_schema,output,handler:Handler::Read(&ControlTieOut),channels:Channels::Every,step_up:&[]}
],guides:&[]})}
