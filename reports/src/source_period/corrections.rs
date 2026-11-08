//! Correcting a retained schedule without editing it. A mistaken schedule is
//! withdrawn, or replaced by a new schedule of the same kind, with a reason;
//! the original stays as evidence with its correction beside it, and only a
//! schedule nobody has corrected counts (the year-to-date bridge reads only
//! those). Both are reviewed writes through the same fence as the import.
use super::{authorise, capability, company, fence, insert, kind, review, MANAGE};
use crate::{MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, WriteHandler,
    DEFINITIONS,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

pub(crate) const REPLACE: &str = "finance.reports.source_period.replace";
pub(crate) const WITHDRAW: &str = "finance.reports.source_period.withdraw";

/// Why the schedule is corrected, as the person wrote it.
fn reason(input: &Value) -> bilibi_agent_api::Result<String> {
    let reason = input["reason"].as_str().unwrap_or("").trim();
    if reason.is_empty() || reason.chars().count() > 500 || reason.contains('\0') {
        return Err(bilibi_agent_api::invalid(
            "reason",
            "Give the reason, in up to 500 characters.",
        ));
    }
    Ok(reason.to_owned())
}

/// The schedule being corrected: one of this company and kind that no
/// correction names yet. Locked, so a racing correction waits and then sees
/// this one's.
async fn target(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    kind: &str,
    field: &str,
    id: Option<i64>,
) -> bilibi_agent_api::Result<Value> {
    let id = id
        .filter(|id| *id > 0)
        .ok_or_else(|| bilibi_agent_api::invalid(field, "Choose a retained schedule."))?;
    let row = client
        .query_opt(
            "SELECT s.id, s.source_reference, s.period_start::text, s.period_end::text, \
             EXISTS (SELECT 1 FROM finance_reports_source_period_corrections c \
                     WHERE c.tenant_id = s.tenant_id AND c.company_id = s.company_id AND c.schedule_id = s.id) \
             FROM finance_reports_source_periods s \
             WHERE s.tenant_id = $1 AND s.company_id = $2 AND s.kind = $3 AND s.id = $4 FOR UPDATE OF s",
            &[&scope.tenant(), &company, &kind, &id],
        )
        .await?
        .ok_or_else(|| {
            bilibi_agent_api::invalid(field, "Choose a retained schedule of this kind.")
        })?;
    if row.get::<_, bool>(4) {
        return Err(bilibi_agent_api::invalid(
            field,
            "This schedule has already been withdrawn or replaced.",
        ));
    }
    Ok(
        json!({"id":id,"source_reference":row.get::<_, String>(1),"period_start":row.get::<_, String>(2),"period_end":row.get::<_, String>(3)}),
    )
}

/// The replacement's review: the import's own review of the new schedule,
/// the schedule it replaces and why.
async fn review_replace(
    client: &dyn OperationClient,
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let owner = crate::agent_operations::OWNER
        .get()
        .ok_or(Error::Internal)?;
    let mut result = review(owner, client, scope, input).await?;
    let company = company(input)?;
    let kind = kind(input)?;
    let replaces = target(
        client,
        scope,
        company,
        kind,
        "replaces_id",
        input["replaces_id"].as_i64(),
    )
    .await?;
    result["replaces"] = replaces;
    result["reason"] = json!(reason(input)?);
    result["notice"] = json!("The schedule it replaces stays as evidence, marked replaced. This creates no historical transactions or statutory submissions.");
    Ok(result)
}

async fn review_withdraw(
    client: &dyn OperationClient,
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let company = company(input)?;
    let kind = kind(input)?;
    authorise(client, scope, company, MANAGE).await?;
    authorise(client, scope, company, capability(kind, true)).await?;
    authorise(client, scope, company, VIEW).await?;
    let status: String = client
        .query_opt(
            "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT",
            &[&scope.tenant(), &company],
        )
        .await?
        .ok_or(Error::NotFound)?
        .get(0);
    if status == "archived" {
        return Err(bilibi_agent_api::invalid(
            "company_id",
            "This company is archived and read-only.",
        ));
    }
    let schedule = target(client, scope, company, kind, "id", input["id"].as_i64()).await?;
    Ok(
        json!({"company_id":company,"kind":kind,"schedule":schedule,"reason":reason(input)?,"notice":"The schedule stays as evidence, marked withdrawn, and no longer counts in the year-to-date bridge."}),
    )
}

/// Records one correction row in the caller's transaction.
async fn correct(
    sql: &tokio_postgres::Transaction<'_>,
    scope_ids: (i64, i64, Option<i64>),
    company: i64,
    schedule: i64,
    replacement: Option<i64>,
    reason: &str,
) -> bilibi_database::Result<i64> {
    let (tenant, user, impersonator) = scope_ids;
    let row = sql
        .query_one(
            "INSERT INTO finance_reports_source_period_corrections(tenant_id,company_id,schedule_id,replacement_id,reason,recorded_by_user_id,recorded_by_impersonator_id) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
            &[&tenant, &company, &schedule, &replacement, &reason, &user, &impersonator],
        )
        .await?;
    Ok(row.get(0))
}

struct Replace;
impl WriteHandler for Replace {
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
            let result = review_replace(client, scope, input).await?;
            fence(client, &result, false).await?;
            Ok(json!({"changes":result}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let result = review_replace(tx.sql(), scope, input).await?;
            fence(tx.sql(), &result, true).await?;
            let ids = (
                scope.tenant(),
                scope.user(),
                scope.impersonator().map(|i| i.user()),
            );
            let record = result.clone();
            let id = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let company = record["company_id"].as_i64().ok_or(Error::Internal)?;
                        let replaced = record["replaces"]["id"].as_i64().ok_or(Error::Internal)?;
                        let id = insert(sql, ids.0, ids.1, ids.2, &record).await?;
                        let reason = record["reason"].as_str().unwrap_or("");
                        let correction =
                            correct(sql, ids, company, replaced, Some(id), reason).await?;
                        Ok((
                            id,
                            vec![
                                Change {
                                    kind: "finance.reports.source_period".into(),
                                    id,
                                    old: Value::Null,
                                    new: record.clone(),
                                },
                                Change {
                                    kind: "finance.reports.source_period_correction".into(),
                                    id: correction,
                                    old: Value::Null,
                                    new: json!({"schedule_id":replaced,"replacement_id":id,"reason":reason}),
                                },
                            ],
                        ))
                    })
                })
                .await?;
            Ok(json!({"id":id,"record":result}).into())
        })
    }
}

struct Withdraw;
impl WriteHandler for Withdraw {
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
            let result = review_withdraw(client, scope, input).await?;
            fence(client, &result, false).await?;
            Ok(json!({"changes":result}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let result = review_withdraw(tx.sql(), scope, input).await?;
            fence(tx.sql(), &result, true).await?;
            let ids = (
                scope.tenant(),
                scope.user(),
                scope.impersonator().map(|i| i.user()),
            );
            let record = result.clone();
            let id = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let company = record["company_id"].as_i64().ok_or(Error::Internal)?;
                        let schedule = record["schedule"]["id"].as_i64().ok_or(Error::Internal)?;
                        let reason = record["reason"].as_str().unwrap_or("");
                        let id = correct(sql, ids, company, schedule, None, reason).await?;
                        Ok((
                            id,
                            vec![Change {
                                kind: "finance.reports.source_period_correction".into(),
                                id,
                                old: Value::Null,
                                new: json!({"schedule_id":schedule,"replacement_id":Value::Null,"reason":reason}),
                            }],
                        ))
                    })
                })
                .await?;
            Ok(json!({"id":id,"record":result}).into())
        })
    }
}

fn replace_schema() -> Value {
    let mut schema = super::write_schema();
    schema["required"]
        .as_array_mut()
        .expect("schema required")
        .extend([json!("replaces_id"), json!("reason")]);
    schema["properties"]["replaces_id"] = json!({"type":"integer","minimum":1});
    schema["properties"]["reason"] = json!({"type":"string","maxLength":500});
    schema
}
fn withdraw_schema() -> Value {
    json!({"type":"object","required":["company_id","kind","id","reason"],"properties":{"company_id":{"type":"integer","minimum":1},"kind":{"type":"string","enum":["comparative","movement","sst","payroll"]},"id":{"type":"integer","minimum":1},"reason":{"type":"string","maxLength":500}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/reports.source-period-corrections",Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions {
    operations: &[
        Operation {key:REPLACE,title:"Replace a retained source-period schedule",summary:"Review and retain a corrected schedule of the same kind, with a new source reference and a reason, and mark the one it replaces as replaced. The replaced schedule stays as evidence and stops counting; nothing posts.",keywords:&["conversion","history","correction"],capability:MANAGE,input:replace_schema,output,handler:Handler::SafeWrite(&Replace),channels:Channels::Every,step_up:&[]},
        Operation {key:WITHDRAW,title:"Withdraw a retained source-period schedule",summary:"Review and mark a retained schedule withdrawn, with a reason. It stays as evidence and stops counting; nothing posts or is deleted.",keywords:&["conversion","history","correction"],capability:MANAGE,input:withdraw_schema,output,handler:Handler::SafeWrite(&Withdraw),channels:Channels::Every,step_up:&[]},
    ], guides:&[],
}) }
