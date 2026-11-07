//! Retained conversion evidence. These records are source summaries, never
//! ledger entries, tax returns, submissions, payroll runs or remittances.
mod bridge;
pub(crate) use bridge::BRIDGE;

use crate::{Reports, MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_finance_ledger::Money;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};
use std::collections::HashSet;

const MANAGE: &str = bilibi_finance_ledger::MANAGE_ACCOUNTS;
fn required<'a>(input: &'a Value, key: &str, max: usize) -> bilibi_agent_api::Result<&'a str> {
    let v = input[key].as_str().unwrap_or("").trim();
    if v.is_empty() || v.chars().count() > max || v.contains('\0') {
        return Err(bilibi_agent_api::invalid(
            key,
            "Enter a value within the allowed length.",
        ));
    }
    Ok(v)
}
fn kind(input: &Value) -> bilibi_agent_api::Result<&str> {
    let value = input["kind"].as_str().unwrap_or("");
    if !["comparative", "movement", "sst", "payroll"].contains(&value) {
        return Err(bilibi_agent_api::invalid(
            "kind",
            "Choose a source-period schedule kind.",
        ));
    }
    Ok(value)
}
fn capability(kind: &str, write: bool) -> &'static str {
    match (kind, write) {
        ("sst", true) => bilibi_finance_sst::MANAGE_SETTINGS,
        ("sst", false) => bilibi_finance_sst::VIEW,
        ("payroll", true) => bilibi_finance_payroll::MANAGE_SETTINGS,
        ("payroll", false) => bilibi_finance_payroll::VIEW,
        (_, true) => MANAGE,
        _ => VIEW,
    }
}
pub(crate) async fn authorise(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    cap: &str,
) -> bilibi_agent_api::Result<()> {
    let company = OrganisationId::new(company)
        .map_err(|_| bilibi_agent_api::invalid("company_id", "Choose a company."))?;
    bilibi_company::authorize_work_target(client, scope, company.get(), cap).await?;
    Ok(())
}
pub(crate) fn company(input: &Value) -> bilibi_agent_api::Result<i64> {
    input["company_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| bilibi_agent_api::invalid("company_id", "Choose a company."))
}
fn date(input: &Value, field: &str) -> bilibi_agent_api::Result<Date> {
    required(input, field, 10)?
        .parse()
        .map_err(|_| bilibi_agent_api::invalid(field, "Enter a date as YYYY-MM-DD."))
}

async fn review(
    owner: &Reports,
    client: &dyn OperationClient,
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<Value> {
    let company = company(input)?;
    let kind = kind(input)?;
    authorise(client, scope, company, MANAGE).await?;
    authorise(client, scope, company, capability(kind, true)).await?;
    authorise(client, scope, company, VIEW).await?;
    let row = client.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT", &[&scope.tenant(), &company]).await?.ok_or(Error::NotFound)?;
    if row.get::<_, String>(0) == "archived" {
        return Err(bilibi_agent_api::invalid(
            "company_id",
            "This company is archived and read-only.",
        ));
    }
    let reference = required(input, "source_reference", 100)?;
    let system = required(input, "source_system", 100)?;
    let timezone = required(input, "accounting_timezone", 100)?;
    jiff::tz::TimeZone::get(timezone).map_err(|_| {
        bilibi_agent_api::invalid("accounting_timezone", "Choose a recognised time zone.")
    })?;
    let book = owner
        .ledger
        .get_book_on(client, scope, company)
        .await
        .map_err(|_| bilibi_agent_api::invalid("company_id", "Open the company book first."))?;
    if required(input, "currency_code", 3)? != book.currency_code {
        return Err(bilibi_agent_api::invalid(
            "currency_code",
            "Use the book currency.",
        ));
    }
    let opening = owner
        .ledger
        .opening_context_on(client, scope, company)
        .await
        .map_err(|_| bilibi_agent_api::invalid("opening", "Approve the opening balances first."))?;
    let start = date(input, "period_start")?;
    let end = date(input, "period_end")?;
    if start > end || end >= opening.entry.entry_date {
        return Err(bilibi_agent_api::invalid(
            "period_end",
            "The source period must end before conversion.",
        ));
    }
    if kind == "movement" {
        let years = owner
            .ledger
            .list_fiscal_years_on(client, scope, company)
            .await
            .map_err(|_| {
                bilibi_agent_api::invalid("opening", "Read the conversion fiscal year first.")
            })?;
        let year = years
            .iter()
            .find(|y| {
                y.start_date <= opening.entry.entry_date && opening.entry.entry_date <= y.end_date
            })
            .ok_or_else(|| {
                bilibi_agent_api::invalid("opening", "Open the conversion fiscal year first.")
            })?;
        let last_source_day =
            opening.entry.entry_date.yesterday().map_err(|_| {
                bilibi_agent_api::invalid("opening", "The conversion date is invalid.")
            })?;
        if start.day() != 1
            || start < year.start_date
            || end != start.last_of_month().min(last_source_day)
        {
            return Err(bilibi_agent_api::invalid("period_start", "Choose one source month in the conversion fiscal year, ending at cut-off for a partial month."));
        }
    }
    let rows = input["rows"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 500)
        .ok_or_else(|| bilibi_agent_api::invalid("rows", "Enter between 1 and 500 rows."))?;
    let accounts = owner
        .ledger
        .list_accounts_on(client, scope, company)
        .await
        .map_err(|_| bilibi_agent_api::invalid("company_id", "Read the chart first."))?;
    let mut seen = HashSet::new();
    let mut facts = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let path = format!("rows.{index}");
        if !row.is_object() {
            return Err(bilibi_agent_api::invalid(&path, "Enter a schedule row."));
        }
        let source_id = row["source_id"].as_str().unwrap_or("").trim();
        if source_id.is_empty()
            || source_id.len() > 100
            || source_id.contains('\0')
            || !seen.insert(source_id.to_owned())
        {
            return Err(bilibi_agent_api::invalid(
                &format!("{path}.source_id"),
                "Enter a unique source identifier.",
            ));
        }
        let amount =
            Money::parse(row["amount"].as_str().unwrap_or(""), book.minor_units).map_err(|_| {
                bilibi_agent_api::invalid(
                    &format!("{path}.amount"),
                    "Enter an amount at the book's precision.",
                )
            })?;
        if ["comparative", "movement"].contains(&kind) {
            let id = row["account_id"].as_i64().ok_or_else(|| {
                bilibi_agent_api::invalid(&format!("{path}.account_id"), "Choose an account.")
            })?;
            if !accounts.iter().any(|a| a.id == id && !a.is_group) {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.account_id"),
                    "Choose an account in the chart.",
                ));
            }
            if !["debit", "credit"].contains(&row["side"].as_str().unwrap_or("")) {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.side"),
                    "Choose debit or credit.",
                ));
            }
            if amount < Money::ZERO {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.amount"),
                    "Enter a non-negative amount.",
                ));
            }
        } else {
            if row["reference"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty() || s.len() > 100 || s.contains('\0'))
            {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.reference"),
                    "Enter the source filing or remittance reference.",
                ));
            }
            if amount < Money::ZERO {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.amount"),
                    "Enter a non-negative amount.",
                ));
            }
        }
        let mut fact = json!({"source_id":source_id,"amount":amount.shown(book.minor_units)});
        if ["comparative", "movement"].contains(&kind) {
            let account = accounts
                .iter()
                .find(|a| Some(a.id) == row["account_id"].as_i64())
                .ok_or(Error::Internal)?;
            fact["account_id"] = row["account_id"].clone();
            fact["account_name"] = json!(account.name);
            fact["account_code"] = json!(account.code);
            fact["side"] = row["side"].clone();
        } else {
            let authority = row["authority"].as_str().unwrap_or("").trim();
            if authority.is_empty() || authority.len() > 100 || authority.contains('\0') {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.authority"),
                    "Enter the authority.",
                ));
            }
            let event_date: Date =
                row["event_date"]
                    .as_str()
                    .unwrap_or("")
                    .parse()
                    .map_err(|_| {
                        bilibi_agent_api::invalid(
                            &format!("{path}.event_date"),
                            "Enter the filing or remittance date.",
                        )
                    })?;
            if event_date < start || event_date >= opening.entry.entry_date {
                return Err(bilibi_agent_api::invalid(
                    &format!("{path}.event_date"),
                    "The filing or remittance must not precede the source period and must precede conversion.",
                ));
            }
            fact["reference"] = json!(row["reference"].as_str().unwrap_or("").trim());
            fact["authority"] = json!(authority);
            fact["event_date"] = json!(event_date.to_string());
            fact["status"] = json!(if kind == "sst" { "filed" } else { "remitted" });
        }
        facts.push(fact);
    }
    if client.query_opt("SELECT id FROM finance_reports_source_periods WHERE tenant_id=$1 AND company_id=$2 AND kind=$3 AND source_reference=$4", &[&scope.tenant(), &company, &kind, &reference]).await?.is_some() { return Err(bilibi_agent_api::invalid("source_reference", "This source reference has already been recorded for this kind.")); }
    Ok(
        json!({"company_id":company,"kind":kind,"source_reference":reference,"source_system":system,"currency_code":book.currency_code,"minor_units":book.minor_units,"accounting_timezone":timezone,"conversion_date":opening.entry.entry_date.to_string(),"opening_entry_id":opening.entry.id,"period_start":start.to_string(),"period_end":end.to_string(),"rows":facts,"notice":"Retained source evidence only; this creates no historical transactions or statutory submissions."}),
    )
}

async fn fence(
    client: &dyn OperationClient,
    review: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let encoded = review.to_string();
    let row = client.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'), COALESCE(NULLIF(current_setting('finance.source_period_reviews',true),''),'[]')::jsonb", &[&encoded]).await?.ok_or(Error::Internal)?;
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
            "SELECT set_config('finance.source_period_reviews',$1,true)",
            &[&json!(pending).to_string()],
        )
        .await?;
    Ok(())
}
struct Import;
impl WriteHandler for Import {
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
            let owner = crate::agent_operations::OWNER
                .get()
                .ok_or(Error::Internal)?;
            let result = review(owner, client, scope, input).await?;
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
            let owner = crate::agent_operations::OWNER
                .get()
                .ok_or(Error::Internal)?;
            let result = review(owner, tx.sql(), scope, input).await?;
            fence(tx.sql(), &result, true).await?;
            let tenant = scope.tenant();
            let user = scope.user();
            let impersonator = scope.impersonator().map(|i| i.user());
            let record = result.clone();
            let id = tx.mutate(move |sql| Box::pin(async move {
                let row = sql.query_one("INSERT INTO finance_reports_source_periods(tenant_id,company_id,kind,source_reference,source_system,currency_code,accounting_timezone,conversion_date,opening_entry_id,period_start,period_end,facts,recorded_by_user_id,recorded_by_impersonator_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::date,$9,$10::text::date,$11::text::date,$12,$13,$14) RETURNING id", &[&tenant, &record["company_id"].as_i64(), &record["kind"].as_str(), &record["source_reference"].as_str(), &record["source_system"].as_str(), &record["currency_code"].as_str(), &record["accounting_timezone"].as_str(), &record["conversion_date"].as_str(), &record["opening_entry_id"].as_i64(), &record["period_start"].as_str(), &record["period_end"].as_str(), &record["rows"], &user, &impersonator]).await?;
                let id = row.get(0);
                let change = Change { kind: "finance.reports.source_period".into(), id, old: Value::Null, new: record };
                Ok((id, vec![change]))
            })).await?;
            Ok(json!({"id":id,"record":result}).into())
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
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = company(input)?;
            let kind = kind(input)?;
            authorise(client, scope, company, VIEW).await?;
            authorise(client, scope, company, capability(kind, false)).await?;
            let after = input["after"].as_i64().unwrap_or(0);
            let id = input["id"].as_i64();
            if after < 0 {
                return Err(bilibi_agent_api::invalid("after", "Choose a valid cursor."));
            }
            let rows = client.query("SELECT id,source_reference,source_system,currency_code,accounting_timezone,conversion_date::text,opening_entry_id,period_start::text,period_end::text,facts,recorded_by_user_id,recorded_by_impersonator_id,to_char(recorded_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM finance_reports_source_periods WHERE tenant_id=$1 AND company_id=$2 AND kind=$3 AND id>$4 AND ($5::bigint IS NULL OR id=$5) ORDER BY id LIMIT 100", &[&scope.tenant(), &company, &kind, &after, &id]).await?;
            let records: Vec<Value> = rows.iter().map(|r| json!({"id":r.get::<_,i64>(0),"kind":kind,"source_reference":r.get::<_,String>(1),"source_system":r.get::<_,String>(2),"currency_code":r.get::<_,String>(3),"accounting_timezone":r.get::<_,String>(4),"conversion_date":r.get::<_,String>(5),"opening_entry_id":r.get::<_,i64>(6),"period_start":r.get::<_,String>(7),"period_end":r.get::<_,String>(8),"rows":r.get::<_,Value>(9),"recorded_by_user_id":r.get::<_,i64>(10),"recorded_by_impersonator_id":r.get::<_,Option<i64>>(11),"recorded_at":r.get::<_,String>(12)})).collect();
            let next = (records.len() == 100)
                .then(|| records.last().and_then(|v| v["id"].as_i64()))
                .flatten();
            Ok(json!({"records":records,"next":next}).into())
        })
    }
}
fn read_schema() -> Value {
    json!({"type":"object","required":["company_id","kind"],"properties":{"company_id":{"type":"integer","minimum":1},"kind":{"type":"string","enum":["comparative","movement","sst","payroll"]},"after":{"type":"integer","minimum":0},"id":{"type":"integer","minimum":1}}})
}
fn write_schema() -> Value {
    let mut schema = read_schema();
    schema["properties"]
        .as_object_mut()
        .expect("schema object")
        .remove("after");
    schema["properties"]
        .as_object_mut()
        .expect("schema object")
        .remove("id");
    schema["required"] = json!([
        "company_id",
        "kind",
        "source_reference",
        "source_system",
        "currency_code",
        "accounting_timezone",
        "period_start",
        "period_end",
        "rows"
    ]);
    schema["properties"]["source_reference"] = json!({"type":"string","maxLength":100});
    schema["properties"]["source_system"] = json!({"type":"string","maxLength":100});
    schema["properties"]["currency_code"] = json!({"type":"string","maxLength":3});
    schema["properties"]["accounting_timezone"] = json!({"type":"string","maxLength":100});
    schema["properties"]["period_start"] = json!({"type":"string","format":"date"});
    schema["properties"]["period_end"] = json!({"type":"string","format":"date"});
    schema["properties"]["rows"] = json!({"type":"array","items":{"type":"object","required":["source_id","amount"],"properties":{"source_id":{"type":"string","maxLength":100},"amount":{"type":"string"},"account_id":{"type":"integer"},"side":{"type":"string","enum":["debit","credit"]},"authority":{"type":"string","maxLength":100},"reference":{"type":"string","maxLength":100},"event_date":{"type":"string","format":"date"}}}});
    schema
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/reports.source-period-operations",Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions {
    operations: &[
        Operation {key:"finance.reports.source_period.list",title:"Read source-period schedules",summary:"Read retained source schedules by kind; these are evidence, not historical transactions or submissions.",keywords:&["conversion","history"],capability:VIEW,input:read_schema,output,handler:Handler::Read(&List),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.reports.source_period.import",title:"Retain a source-period schedule",summary:"Review and retain one source schedule without creating postings, returns or remittances.",keywords:&["conversion","history"],capability:MANAGE,input:write_schema,output,handler:Handler::SafeWrite(&Import),channels:Channels::Every,step_up:&[]},
    ], guides:&[],
}) }

struct AttachmentSource {
    kind: &'static str,
    label: &'static str,
    capabilities: &'static [&'static str],
}
impl bilibi_finance_attachments::Source for AttachmentSource {
    fn label(&self) -> &'static str {
        self.label
    }
    fn view_capabilities(&self) -> &'static [&'static str] {
        self.capabilities
    }
    fn document<'a>(
        &'a self,
        client: bilibi_finance_attachments::SourceClient<'a>,
        scope: &'a Scope,
        company: i64,
        id: i64,
    ) -> bilibi_finance_attachments::SourceFuture<'a> {
        Box::pin(async move {
            bilibi_finance_attachments::on_source_client!(client, |sql| {
                for capability in self.capabilities {
                    crate::authorize(sql, scope, company, capability)
                        .await
                        .map_err(|e| match e {
                            crate::ReportsError::Unauthorized => {
                                bilibi_finance_attachments::SourceRefusal::Unauthorized
                            }
                            crate::ReportsError::CompanyNotFound => {
                                bilibi_finance_attachments::SourceRefusal::NotFound
                            }
                            crate::ReportsError::Failed(e) => {
                                bilibi_finance_attachments::SourceRefusal::Failed(e)
                            }
                            _ => bilibi_finance_attachments::SourceRefusal::Failed(Error::Invalid),
                        })?;
                }
                let row = sql.query_opt("SELECT source_reference,conversion_date::text FROM finance_reports_source_periods WHERE tenant_id=$1 AND company_id=$2 AND id=$3 AND kind=$4", &[&scope.tenant(), &company, &id, &self.kind]).await.map_err(Error::from)?.ok_or(bilibi_finance_attachments::SourceRefusal::NotFound)?;
                Ok(bilibi_finance_attachments::SourceDocument {
                    label: format!("{} {}", self.label, row.get::<_, String>(0)),
                    date: row.get::<_, String>(1).parse().map_err(|_| {
                        bilibi_finance_attachments::SourceRefusal::Failed(Error::Invalid)
                    })?,
                    is_final: true,
                })
            })
        })
    }
}
macro_rules! source {
    ($id:literal, $kind:literal, $label:literal, $caps:expr) => {
        inventory::submit! { Contribution::new(bilibi_finance_attachments::SOURCES,$id,Provenance {module:MODULE,source:file!(),line:line!()}, &bilibi_finance_attachments::SourceDefinition { source: &AttachmentSource { kind: $kind, label: $label, capabilities: $caps } }) }
    };
}
source!(
    "finance/reports/source_comparative",
    "comparative",
    "Source comparative",
    &[VIEW]
);
source!(
    "finance/reports/source_movement",
    "movement",
    "Source monthly movement",
    &[VIEW]
);
source!(
    "finance/reports/source_sst",
    "sst",
    "Source SST return",
    &[VIEW, bilibi_finance_sst::VIEW]
);
source!(
    "finance/reports/source_payroll",
    "payroll",
    "Source payroll remittance",
    &[VIEW, bilibi_finance_payroll::VIEW]
);

pub(crate) async fn evidence_page(
    client: &(impl tokio_postgres::GenericClient + Sync),
    tenant: i64,
    company: i64,
    limit: Option<i64>,
    after: Option<i64>,
) -> crate::Result<crate::EvidencePage> {
    let limit = limit.unwrap_or(500);
    let after = after.unwrap_or(0);
    if !(1..=500).contains(&limit) || after < 0 {
        return Err(crate::ReportsError::InvalidOption);
    }
    let row = client.query_one("SELECT COALESCE(json_agg(t ORDER BY t.id),'[]'::json) FROM (SELECT id,tenant_id,company_id,kind,source_reference,source_system,currency_code,accounting_timezone,conversion_date::text AS conversion_date,opening_entry_id,period_start::text AS period_start,period_end::text AS period_end,facts,recorded_by_user_id,recorded_by_impersonator_id,to_char(recorded_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS recorded_at FROM finance_reports_source_periods WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4) t", &[&tenant, &company, &after, &limit]).await.map_err(Error::from)?;
    let rows = row
        .get::<_, Value>(0)
        .as_array()
        .cloned()
        .ok_or(Error::Internal)?;
    let next = (rows.len() as i64 == limit)
        .then(|| rows.last().and_then(|r| r["id"].as_i64()))
        .flatten();
    Ok(crate::EvidencePage {
        fields: vec![
            "id",
            "tenant_id",
            "company_id",
            "kind",
            "source_reference",
            "source_system",
            "currency_code",
            "accounting_timezone",
            "conversion_date",
            "opening_entry_id",
            "period_start",
            "period_end",
            "facts",
            "recorded_by_user_id",
            "recorded_by_impersonator_id",
            "recorded_at",
        ],
        rows,
        next,
    })
}
