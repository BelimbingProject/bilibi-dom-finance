//! One operation per action for the page, HTTP API and MCP.
use crate::{agent_operations::OWNER, Reports, ReportsError, ISSUE, MODULE, SIGN_OFF, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};
const SIGN_ATTESTATION: &str = "I have reviewed this exact issued snapshot, its differences, excluded history and schedule limits. My note explains my conclusion and any qualifications. This attestation does not change the figures or resolve differences.";
fn owner() -> bilibi_agent_api::Result<&'static Reports> {
    Ok(OWNER.get().ok_or(Error::Internal)?)
}
fn refusal(e: ReportsError) -> bilibi_agent_api::Error {
    match e {
        ReportsError::Unauthorized => bilibi_agent_api::Error::Forbidden,
        ReportsError::ReportNotFound | ReportsError::CompanyNotFound => {
            bilibi_agent_api::Error::NotFound
        }
        ReportsError::Failed(e) => e.into(),
        e => bilibi_agent_api::invalid("base", e.to_string()),
    }
}
fn id(input: &Value, key: &str) -> bilibi_agent_api::Result<i64> {
    input[key]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| bilibi_agent_api::invalid(key, "Choose a record."))
}
fn pins(input: &Value) -> bilibi_agent_api::Result<crate::ConversionPins> {
    if input["pins"].is_null() {
        Ok(Default::default())
    } else {
        serde_json::from_value(input["pins"].clone()).map_err(|_| {
            bilibi_agent_api::invalid(
                "pins",
                "Use the complete boundaries returned by the report.",
            )
        })
    }
}
fn day(input: &Value) -> bilibi_agent_api::Result<Date> {
    input["as_of"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bilibi_agent_api::invalid("as_of", "Choose the reporting date."))
}
fn text(input: &Value, key: &str, max: usize) -> bilibi_agent_api::Result<String> {
    let value = input[key].as_str().unwrap_or("").trim();
    if value.is_empty() || value.chars().count() > max || value.contains('\0') {
        Err(bilibi_agent_api::invalid(
            key,
            format!("Enter 1 to {max} characters."),
        ))
    } else {
        Ok(value.into())
    }
}
async fn writable(
    c: &tokio_postgres::Transaction<'_>,
    s: &Scope,
    company: i64,
    cap: &str,
) -> bilibi_agent_api::Result<()> {
    owner()?
        .conversion_access(c, s, company, cap)
        .await
        .map_err(refusal)?;
    bilibi_company::require_writable(c, s, company, true)
        .await
        .map_err(|e| bilibi_agent_api::invalid("base", e.to_string()))?;
    Ok(())
}
async fn fence(c: &dyn OperationClient, v: &Value, consume: bool) -> bilibi_agent_api::Result<()> {
    let encoded = v.to_string();
    let row=c.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'),COALESCE(NULLIF(current_setting('finance.conversion_reviews',true),''),'[]')::jsonb",&[&encoded]).await?.ok_or(Error::Internal)?;
    let hash: String = row.get(0);
    let mut pending: Vec<String> =
        serde_json::from_value(row.get(1)).map_err(|_| Error::Internal)?;
    if consume {
        if pending.first() != Some(&hash) {
            return Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict,
            ));
        }
        pending.remove(0);
    } else {
        pending.push(hash);
    }
    c.query_opt(
        "SELECT set_config('finance.conversion_reviews',$1,true)",
        &[&json!(pending).to_string()],
    )
    .await?;
    Ok(())
}
struct Preview;
impl ReadHandler for Preview {
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
            Ok(owner()?
                .conversion_report(s, id(input, "company_id")?, day(input)?, &pins(input)?)
                .await
                .map_err(refusal)?
                .into())
        })
    }
}
struct Issued;
impl ReadHandler for Issued {
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
            let o = owner()?;
            let c = o.db.get().await.map_err(Error::from)?;
            Ok(
                o.conversion_issued_on(&**c, s, id(input, "company_id")?, id(input, "report_id")?)
                    .await
                    .map_err(refusal)?
                    .into(),
            )
        })
    }
}
struct List;
impl ReadHandler for List {
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
            let o = owner()?;
            let c = o.db.get().await.map_err(Error::from)?;
            let company = id(input, "company_id")?;
            o.conversion_access(&**c, s, company, VIEW)
                .await
                .map_err(refusal)?;
            let after = input["after"].as_i64().unwrap_or(0);
            if after < 0 {
                return Err(bilibi_agent_api::invalid(
                    "after",
                    "Choose a nonnegative cursor.",
                ));
            }
            let rows=c.query("SELECT id,issued_at::text,through_entry_number,content->>'as_of',EXISTS(SELECT 1 FROM finance_reports_conversion_signoffs s WHERE s.tenant_id=$1 AND s.company_id=$2 AND s.report_id=r.id) FROM finance_reports_issued r WHERE tenant_id=$1 AND company_id=$2 AND kind='conversion' AND id>$3 ORDER BY id LIMIT 100",&[&s.tenant(),&company,&after]).await.map_err(Error::from)?.iter().map(|r|json!({"id":r.get::<_,i64>(0),"issued_at":r.get::<_,String>(1),"through_entry_number":r.get::<_,i64>(2),"as_of":r.get::<_,String>(3),"signed":r.get::<_,bool>(4)})).collect::<Vec<_>>();
            let next = if rows.len() == 100 {
                rows.last().map(|r| r["id"].clone())
            } else {
                None
            };
            Ok(json!({"rows":rows,"next":next}).into())
        })
    }
}
struct Issue;
async fn issue_review(
    c: &tokio_postgres::Transaction<'_>,
    s: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let company = id(input, "company_id")?;
    writable(c, s, company, ISSUE).await?;
    let mut report = owner()?
        .conversion_on(c, s, company, day(input)?, &pins(input)?)
        .await
        .map_err(refusal)?;
    report["reviewer_reference"] = json!(text(input, "reviewer_reference", 2000)?);
    report["review_note"] = json!(text(input, "note", 8000)?);
    report["unresolved_differences"]
        .as_array_mut()
        .ok_or(Error::Internal)?
        .retain(|r| r["kind"] != "schedule_assessment");
    let mut schedules = serde_json::Map::new();
    for area in ["bank", "tax", "payroll"] {
        let status_key = format!("{area}_status");
        let reference_key = format!("{area}_reference");
        let status = input[&status_key].as_str().unwrap_or("not_assessed");
        if !["not_assessed", "applicable", "not_applicable"].contains(&status) {
            return Err(bilibi_agent_api::invalid(
                &status_key,
                "Choose whether this schedule applies.",
            ));
        }
        let reference = if status == "not_assessed" {
            String::new()
        } else {
            text(input, &reference_key, 2000)?
        };
        schedules.insert(
            area.into(),
            json!({"status":status,"reference_or_reason":reference}),
        );
        if status == "not_assessed" {
            report["unresolved_differences"].as_array_mut().ok_or(Error::Internal)?.push(json!({"area":area,"kind":"schedule_assessment","reason":"The accountant has not assessed this schedule's applicability or completeness."}));
        }
    }
    report["schedule_reviews"] = Value::Object(schedules);
    Ok(report)
}
impl WriteHandler for Issue {
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
            let v = issue_review(c.transaction().ok_or(Error::Internal)?, s, input).await?;
            fence(c, &v, false).await?;
            Ok(v.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let v = issue_review(tx.sql(), s, input).await?;
            fence(tx.sql(), &v, true).await?;
            Ok(owner()?
                .issue_conversion_on(tx, s, id(input, "company_id")?, v)
                .await
                .map_err(refusal)?
                .into())
        })
    }
}
struct Sign;
async fn sign_review(
    c: &tokio_postgres::Transaction<'_>,
    s: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let company = id(input, "company_id")?;
    writable(c, s, company, SIGN_OFF).await?;
    let record = owner()?
        .conversion_issued_on(c, s, company, id(input, "report_id")?)
        .await
        .map_err(refusal)?;
    if record["signoffs"].as_array().is_none_or(|a| !a.is_empty()) {
        return Err(bilibi_agent_api::invalid(
            "report_id",
            "This report is already signed.",
        ));
    }
    let approval = Reports::conversion_approval(&record, None).map_err(refusal)?;
    let decision = owner()?
        .ledger
        .preview_approval_on(c, s, company, &approval)
        .await
        .map_err(|e| bilibi_agent_api::invalid("base", e.to_string()))?;
    Ok(
        json!({"record":record,"reviewer_reference":text(input,"reviewer_reference",2000)?,"note":text(input,"note",8000)?,"approval":decision,"attestation":SIGN_ATTESTATION}),
    )
}
impl WriteHandler for Sign {
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
            let v = sign_review(c.transaction().ok_or(Error::Internal)?, s, input).await?;
            fence(c, &v, false).await?;
            Ok(v.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let v = sign_review(tx.sql(), s, input).await?;
            fence(tx.sql(), &v, true).await?;
            let company = id(input, "company_id")?;
            let reference = text(input, "reviewer_reference", 2000)?;
            let note = text(input, "note", 8000)?;
            let attestation = v["attestation"].as_str().ok_or(Error::Internal)?.to_owned();
            let row = owner()?
                .sign_conversion_on(
                    tx,
                    s,
                    company,
                    &v["record"],
                    crate::conversion::SignOff {
                        reviewer_reference: reference,
                        note,
                        attestation,
                    },
                )
                .await
                .map_err(refusal)?;
            Ok(row.into())
        })
    }
}
fn schema() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1},"after":{"type":"integer","minimum":0}}})
}
fn dated() -> Value {
    let mut s = schema();
    s["required"] = json!(["company_id", "as_of"]);
    s["properties"]["as_of"] = json!({"type":"string","format":"date"});
    s["properties"]["pins"] = json!({"type":"object","required":["through_entry_number","receivables_opening_batch_id","receivables_settlement_number","payables_opening_batch_id","payables_settlement_number"],"properties":{"through_entry_number":{"type":"integer","minimum":0},"receivables_opening_batch_id":{"type":"integer","minimum":0},"receivables_settlement_number":{"type":"integer","minimum":0},"payables_opening_batch_id":{"type":"integer","minimum":0},"payables_settlement_number":{"type":"integer","minimum":0}}});
    s
}
fn record() -> Value {
    let mut s = schema();
    s["required"] = json!(["company_id", "report_id"]);
    s["properties"]["report_id"] = json!({"type":"integer","minimum":1});
    s
}
fn write_schema(mut s: Value) -> Value {
    s["required"]
        .as_array_mut()
        .unwrap()
        .extend([json!("reviewer_reference"), json!("note")]);
    s["properties"]["reviewer_reference"] = json!({"type":"string","maxLength":2000});
    s["properties"]["note"] = json!({"type":"string","maxLength":8000});
    s
}
fn issue_schema() -> Value {
    let mut schema = write_schema(dated());
    for area in ["bank", "tax", "payroll"] {
        schema["properties"][format!("{area}_status")] =
            json!({"type":"string","enum":["not_assessed","applicable","not_applicable"]});
        schema["properties"][format!("{area}_reference")] =
            json!({"type":"string","maxLength":2000});
    }
    schema
}
fn sign_schema() -> Value {
    write_schema(record())
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/reports.conversion-operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {operations:&[
 Operation {key:"finance.reports.conversion.preview",title:"Read conversion reconciliation",summary:"Collect opening sources, target balances, module reconciliations and explicit limits on one snapshot.",keywords:&["conversion"],capability:VIEW,input:dated,output,handler:Handler::Read(&Preview),channels:Channels::Every,step_up:&[]},
 Operation {key:"finance.reports.conversion.list",title:"List conversion reports",summary:"Read issued conversion reports and their sign-off status, 100 at a time.",keywords:&[],capability:VIEW,input:schema,output,handler:Handler::Read(&List),channels:Channels::Every,step_up:&[]},
 Operation {key:"finance.reports.conversion.get",title:"Read an issued conversion report",summary:"Read the immutable snapshot and accountant sign-off with all source permissions rechecked.",keywords:&[],capability:VIEW,input:record,output,handler:Handler::Read(&Issued),channels:Channels::Every,step_up:&[]},
 Operation {key:"finance.reports.conversion.issue",title:"Issue conversion reconciliation",summary:"Freeze the reviewed report and source evidence. Issuance does not sign it or certify completeness.",keywords:&[],capability:ISSUE,input:issue_schema,output,handler:Handler::SafeWrite(&Issue),channels:Channels::Every,step_up:&[]},
 Operation {key:"finance.reports.conversion.sign",title:"Sign conversion reconciliation",summary:"Record an accountant's attestation, references and qualifications against one issued snapshot; duty separation includes source preparers.",keywords:&[],capability:SIGN_OFF,input:sign_schema,output,handler:Handler::SafeWrite(&Sign),channels:Channels::Every,step_up:&[]},
],guides:&[]})}
