//! Report layout contracts, shared by pages and agents (ADR 01).
use crate::{configuration::SECTIONS, layout, Reports, MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub(crate) static OWNER: OnceLock<Arc<Reports>> = OnceLock::new();
pub(crate) fn bind(owner: Arc<Reports>) -> Result<(), RegistryError> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate report operations".into()))
}
const MANAGE: &str = bilibi_finance_ledger::MANAGE_ACCOUNTS;
async fn read(
    client: &dyn OperationClient,
    scope: &Scope,
    input: &Value,
    capability: &str,
) -> bilibi_agent_api::Result<Value> {
    let company = input["company_id"]
        .as_i64()
        .and_then(|id| OrganisationId::new(id).ok())
        .ok_or_else(|| bilibi_agent_api::invalid("company_id", "Choose a company."))?;
    bilibi_company::authorize_work_target(client, scope, company.get(), capability).await?;
    Ok(OWNER
        .get()
        .ok_or(Error::Internal)?
        .settings
        .get(client, scope, Target::Company(company), SECTIONS)
        .await?
        .value)
}
async fn review(
    client: &dyn OperationClient,
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let before = read(client, scope, input, MANAGE).await?;
    let company = input["company_id"].as_i64().ok_or(Error::Invalid)?;
    // The company lock serializes Finance writes, including a plan's layout steps.
    let row = client.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT", &[&scope.tenant(), &company]).await?.ok_or(bilibi_agent_api::Error::NotFound)?;
    if row.get::<_, String>(0) == "archived" {
        return Err(bilibi_agent_api::invalid(
            "base",
            "This company is archived and read-only.",
        ));
    }
    if let Some(sections) = input["sections"].as_array() {
        let fields: BTreeMap<_, _> = sections
            .iter()
            .enumerate()
            .filter_map(|(index, section)| {
                let name = section["name"].as_str().unwrap_or_default().trim();
                (name.is_empty() || name.chars().count() > 100).then(|| {
                    (
                        format!("sections.{index}.name"),
                        vec!["Enter a heading with 1 to 100 characters.".to_owned()],
                    )
                })
            })
            .collect();
        if !fields.is_empty() {
            return Err(bilibi_agent_api::Error::InvalidInput(fields));
        }
    }
    if input["sections"]
        .as_array()
        .is_none_or(|sections| sections.len() > 128)
        || !layout::valid(&input["sections"])
    {
        return Err(bilibi_agent_api::invalid(
            "sections",
            "Choose a report, a heading and accounts for each section.",
        ));
    }
    Ok(json!({"before":before,"after":input["sections"]}))
}
async fn fence(
    client: &dyn OperationClient,
    review: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let encoded = review.to_string();
    let row = client.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'), COALESCE(NULLIF(current_setting('finance.report_layout_reviews',true),''),'[]')::jsonb", &[&encoded]).await?.ok_or(Error::Internal)?;
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
    client
        .query_opt(
            "SELECT set_config('finance.report_layout_reviews',$1,true)",
            &[&json!(pending).to_string()],
        )
        .await?;
    Ok(())
}
/// The Finance setup of one company: which areas are set up and which remain.
struct SetupRead;
impl ReadHandler for SetupRead {
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
            let company = input["company_id"]
                .as_i64()
                .and_then(|id| OrganisationId::new(id).ok())
                .ok_or_else(|| bilibi_agent_api::invalid("company_id", "Choose a company."))?;
            bilibi_company::authorize_work_target(client, scope, company.get(), VIEW).await?;
            // A read holds no company lock, so the facades' own connections are safe here.
            let overview = OWNER
                .get()
                .ok_or(Error::Internal)?
                .setup_overview(scope, company.get())
                .await
                .map_err(|_| Error::Internal)?;
            Ok(json!(overview).into())
        })
    }
}
/// What is left before a month of one company closes.
struct MonthCloseRead;
impl ReadHandler for MonthCloseRead {
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
            let company = input["company_id"]
                .as_i64()
                .and_then(|id| OrganisationId::new(id).ok())
                .ok_or_else(|| bilibi_agent_api::invalid("company_id", "Choose a company."))?;
            bilibi_company::authorize_work_target(client, scope, company.get(), VIEW).await?;
            let query = crate::MonthQuery {
                period_id: input["period_id"].as_i64(),
                month: input["month"].as_str().map(str::to_owned),
                through_period_id: input["through_period_id"].as_i64(),
            };
            // A read holds no company lock, so the facades' own connections are safe here.
            let checklist = OWNER
                .get()
                .ok_or(Error::Internal)?
                .month_close(scope, company.get(), &query)
                .await
                .map_err(|error| match error {
                    crate::ReportsError::Unauthorized => bilibi_agent_api::Error::Forbidden,
                    crate::ReportsError::MonthNotFound => bilibi_agent_api::invalid(
                        "period_id",
                        "That month is not in this company's fiscal years.",
                    ),
                    crate::ReportsError::InvalidOption => {
                        bilibi_agent_api::invalid("month", "Enter the month as YYYY-MM.")
                    }
                    crate::ReportsError::BookNotOpen => bilibi_agent_api::invalid(
                        "base",
                        "The company's book is not open yet.",
                    ),
                    _ => Error::Internal.into(),
                })?;
            Ok(json!(checklist).into())
        })
    }
}
struct Layout;
impl ReadHandler for Layout {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move { Ok(json!({"sections":read(client,scope,input,VIEW).await?}).into()) })
    }
}
impl WriteHandler for Layout {
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
            let value = review(client, scope, input).await?;
            fence(client, &value, false).await?;
            Ok(value.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let value = review(tx.sql(), scope, input).await?;
            fence(tx.sql(), &value, true).await?;
            let company = OrganisationId::new(input["company_id"].as_i64().ok_or(Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
            OWNER
                .get()
                .ok_or(Error::Internal)?
                .settings
                .put(
                    tx,
                    scope,
                    Target::Company(company),
                    SECTIONS,
                    input["sections"].clone(),
                )
                .await?;
            Ok(json!({"sections":input["sections"]}).into())
        })
    }
}
fn get_schema() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1}}})
}
fn month_close_schema() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{
        "company_id":{"type":"integer","minimum":1},
        "period_id":{"type":"integer","minimum":1,"description":"The month to read, as a fiscal period. Without it or month, the earliest open month."},
        "month":{"type":"string","pattern":"^[0-9]{4}-[0-9]{2}$","description":"The month to read, as YYYY-MM."},
        "through_period_id":{"type":"integer","minimum":1,"description":"Read a run of months, from the first through this one."}
    }})
}
fn set_schema() -> Value {
    let mut s = get_schema();
    s["required"] = json!(["company_id", "sections"]);
    s["properties"]["sections"] = json!({"type":"array","items":{"type":"object","required":["report","name","accounts"],"properties":{"report":{"type":"string","enum":["profit_and_loss","balance_sheet","cash_position"]},"name":{"type":"string","maxLength":100},"accounts":{"type":"array","items":{"type":"string","maxLength":50}}}}});
    s
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/reports.layout-operations",Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions {
    operations: &[
        Operation {key:"finance.reports.sections.get",title:"Read report sections",summary:"Read the company's headings and account choices.",keywords:&[],capability:VIEW,input:get_schema,output,handler:Handler::Read(&Layout),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.reports.setup.get",title:"Read the Finance setup",summary:"Read which Finance areas the company has set up and which remain, each with the page that sets it up. A step has a status only where its module can tell.",keywords:&["setup","onboarding","checklist"],capability:VIEW,input:get_schema,output,handler:Handler::Read(&SetupRead),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.reports.month_close.get",title:"Read the month-end checklist",summary:"Read what is left before a month closes: unmatched bank lines, draft invoices and bills, bills not posted, payments and journals waiting for approval, late entries to review, depreciation, payroll, SST and e-invoices, each with the page that clears it, and whether the ledger's closing rules let the month close now. The checks warn; they never block. Close with finance.ledger.period.close.",keywords:&["month end","close month","closing","checklist"],capability:VIEW,input:month_close_schema,output,handler:Handler::Read(&MonthCloseRead),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.reports.sections.set",title:"Arrange report sections",summary:"Replace the company's report headings and account choices in order.",keywords:&[],capability:MANAGE,input:set_schema,output,handler:Handler::SafeWrite(&Layout),channels:Channels::Every,step_up:&[]},
    ], guides:&[],
}) }
