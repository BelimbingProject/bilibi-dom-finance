//! Reviewed conversion contracts shared by the page and the agent doors.
use crate::*;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock};

static OWNER: OnceLock<Arc<Reconciliation>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Reconciliation>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate reconciliation opening binding".into()))
}
fn owner() -> Result<&'static Reconciliation> {
    OWNER
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| Error::Internal.into())
}
fn company(input: &Value) -> Result<i64> {
    input["company_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or(ReconciliationError::InvalidOption)
}
fn failure(error: ReconciliationError) -> bilibi_agent_api::Error {
    use bilibi_agent_api::Error as E;
    match error {
        ReconciliationError::Unauthorized => E::Forbidden,
        ReconciliationError::CompanyNotFound | ReconciliationError::BankAccountNotFound => {
            E::NotFound
        }
        ReconciliationError::Failed(e) => e.into(),
        ReconciliationError::Invalid(fields) => {
            let mut result = BTreeMap::new();
            for field in fields {
                result
                    .entry(field.field.to_owned())
                    .or_insert_with(Vec::new)
                    .push(field.message);
            }
            E::InvalidInput(result)
        }
        other => E::InvalidInput(BTreeMap::from([("base".into(), vec![other.to_string()])])),
    }
}
fn context_schema() -> Value {
    json!({"type":"object","required":["company_id","bank_account_id"],"properties":{"company_id":{"type":"integer","minimum":1},"bank_account_id":{"type":"integer","minimum":1}}})
}
fn schema() -> Value {
    json!({"type":"object","required":["company_id","bank_account_id","source_reference","statement_source","statement_balance","currency_code","csv"],"properties":{"company_id":{"type":"integer","minimum":1},"bank_account_id":{"type":"integer","minimum":1},"source_reference":{"type":"string","maxLength":100},"statement_source":{"type":"string","maxLength":500},"statement_balance":{"type":"string"},"currency_code":{"type":"string","maxLength":3},"csv":{"type":"string","maxLength":131072},"replaces_bridge_id":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":500},"override_reason":{"type":"string","maxLength":500}}})
}
fn match_schema() -> Value {
    json!({"type":"object","required":["company_id","bank_account_id","opening_item_id","statement_line_id"],"properties":{"company_id":{"type":"integer","minimum":1},"bank_account_id":{"type":"integer","minimum":1},"opening_item_id":{"type":"integer","minimum":1},"statement_line_id":{"type":"integer","minimum":1}}})
}
fn unmatch_schema() -> Value {
    json!({"type":"object","required":["company_id","match_id","reason"],"properties":{"company_id":{"type":"integer","minimum":1},"match_id":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":500}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
async fn fence(
    c: &dyn OperationClient,
    facts: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let row=c.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'),COALESCE(NULLIF(current_setting('finance.reconciliation_reviews',true),''),'[]')::jsonb", &[&facts.to_string()]).await?.ok_or(Error::Internal)?;
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
        "SELECT set_config('finance.reconciliation_reviews',$1,true)",
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
struct Current;
impl ReadHandler for Current {
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
                .opening_current(c, s, company(input).map_err(failure)?, input)
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
            let p = owner()
                .map_err(failure)?
                .prepare_opening(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":p.review}), false).await?;
            Ok(json!({"changes":p.review}).into())
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
            let p = owner
                .prepare_opening(tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            fence(tx.sql(), &json!({"input":input,"review":p.review}), true).await?;
            let record = owner
                .import_opening_on(tx, s, company, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record,"path":format!("/finance/reconciliation/accounts/{}",input["bank_account_id"])}).into())
        })
    }
}
struct MatchOpening;
impl WriteHandler for MatchOpening {
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
            let review = owner()
                .map_err(failure)?
                .prepare_opening_match(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":review}), false).await?;
            Ok(json!({"changes":review}).into())
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
            let review = owner
                .prepare_opening_match(tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            fence(tx.sql(), &json!({"input":input,"review":review}), true).await?;
            let record = owner
                .match_opening_on(tx, s, company, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record}).into())
        })
    }
}
struct UnmatchOpening;
impl WriteHandler for UnmatchOpening {
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
            let review = owner()
                .map_err(failure)?
                .prepare_opening_unmatch(c, s, company(input).map_err(failure)?, input)
                .await
                .map_err(failure)?;
            fence(c, &json!({"input":input,"review":review}), false).await?;
            Ok(json!({"changes":review}).into())
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
            let review = owner
                .prepare_opening_unmatch(tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            fence(tx.sql(), &json!({"input":input,"review":review}), true).await?;
            let record = owner
                .unmatch_opening_on(tx, s, company, input)
                .await
                .map_err(failure)?;
            Ok(json!({"record":record}).into())
        })
    }
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/reconciliation.opening",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
    Operation{key:"finance.reconciliation.opening.current",title:"Read current opening bank bridge",summary:"Read the current opening bridge for a bank account.",keywords:&["conversion"],capability:VIEW,input:context_schema,output,handler:Handler::Read(&Current),channels:Channels::Every,step_up:&[]},
    Operation{key:"finance.reconciliation.opening.context",title:"Read opening bank bridge",summary:"Read the approved opening balances, bank account and retained opening items.",keywords:&["conversion"],capability:VIEW,input:context_schema,output,handler:Handler::Read(&Context),channels:Channels::Every,step_up:&[]},
    Operation{key:"finance.reconciliation.opening.import",title:"Record opening bank bridge",summary:"Review source statement balance and outstanding items against the approved opening entry; record without posting again.",keywords:&["conversion"],capability:MANAGE_RECONCILIATIONS,input:schema,output,handler:Handler::SafeWrite(&Import),channels:Channels::Every,step_up:&[]},
    Operation{key:"finance.reconciliation.opening.match",title:"Match an opening item",summary:"Review and match an outstanding opening item to a statement line on or after conversion without posting again.",keywords:&["conversion","matching"],capability:MANAGE_MATCHES,input:match_schema,output,handler:Handler::SafeWrite(&MatchOpening),channels:Channels::Every,step_up:&[]},
    Operation{key:"finance.reconciliation.opening.unmatch",title:"Unmatch an opening item",summary:"Unmatch an opening item with a reason unless a finalised reconciliation counted the match.",keywords:&["conversion","matching"],capability:MANAGE_MATCHES,input:unmatch_schema,output,handler:Handler::SafeWrite(&UnmatchOpening),channels:Channels::Every,step_up:&[]}
],guides:&[]}) }
