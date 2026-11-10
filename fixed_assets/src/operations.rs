//! Bilimbi FixedAssets lifecycle, shared by PageContracts and the agent doors.
//! Preparation is read-only on the handed connection. Execution joins the plan's
//! mutation boundary; the ordered local fence catches earlier dependent steps.
use crate::opening_operations::{company, failure, fence};
use crate::*;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationFuture, ReadHandler, WriteHandler,
    DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use serde_json::{json, Value};
use std::sync::OnceLock;
static OWNER: OnceLock<Arc<FixedAssets>> = OnceLock::new();
pub(super) fn bind(owner: Arc<FixedAssets>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate fixed assets lifecycle binding".into()))
}
fn owner() -> Result<&'static FixedAssets> {
    OWNER
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| Error::Internal.into())
}
fn id(input: &Value, key: &'static str) -> Result<i64> {
    input[key]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(key, "must be greater than 0"))
}
fn decode<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T> {
    serde_json::from_value(input.clone())
        .map_err(|_| invalid("base", "contains an invalid field value"))
}
fn date(input: &Value, key: &'static str) -> Result<Date> {
    input[key]
        .as_str()
        .and_then(|d| d.parse().ok())
        .ok_or_else(|| invalid(key, "is invalid"))
}
fn pins(input: &Value) -> Result<ReportPins> {
    decode(input)
}
fn target() -> Value {
    schema(&[], json!({}))
}
fn schema(required: &[&str], properties: Value) -> Value {
    let mut props = json!({"company_id":{"type":"integer","minimum":1}});
    props
        .as_object_mut()
        .expect("object")
        .extend(properties.as_object().expect("object").clone());
    let mut fields = vec!["company_id"];
    fields.extend(required);
    json!({"type":"object","required":fields,"properties":props})
}
fn category_schema() -> Value {
    schema(
        &[
            "cost_account_id",
            "accumulated_account_id",
            "expense_account_id",
            "gain_account_id",
            "loss_account_id",
            "method",
            "life_months",
            "reason",
        ],
        json!({
            "name":{"type":"string","maxLength":200,"description":"Required for a new category; omitted when revising."},
            "category_id":{"type":"integer","minimum":1,"description":"Omit to create; provide to append a revision. Existing asset assumptions remain frozen."},
            "cost_account_id":{"type":"integer","minimum":1},"accumulated_account_id":{"type":"integer","minimum":1},"expense_account_id":{"type":"integer","minimum":1},"gain_account_id":{"type":"integer","minimum":1},"loss_account_id":{"type":"integer","minimum":1},
            "method":{"type":"string","enum":["straight_line","reducing_balance"]},"life_months":{"type":"integer","minimum":1,"maximum":1200},"annual_rate":{"type":"string","description":"Annual percentage for reducing balance; cleared for straight line."},"reason":{"type":"string","maxLength":500}
        }),
    )
}
fn register_schema() -> Value {
    schema(
        &["as_of"],
        json!({"source":{"type":"string","enum":["ordinary","opening"]},"status":{"type":"string","enum":["in_service","disposed","voided"]},"as_of":{"type":"string","format":"date","description":"Dated register and projected schedule, not a range filter."},"through_entry_number":{"type":"integer","minimum":0},"through_asset_number":{"type":"integer","minimum":0},"through_event_number":{"type":"integer","minimum":0},"through_category_revision_id":{"type":"integer","minimum":0}}),
    )
}
fn schedule_schema() -> Value {
    schema(
        &["as_of"],
        json!({"as_of":{"type":"string","format":"date","description":"Dated register and projected schedule, not a range filter."},"through_entry_number":{"type":"integer","minimum":0},"through_asset_number":{"type":"integer","minimum":0},"through_event_number":{"type":"integer","minimum":0},"through_category_revision_id":{"type":"integer","minimum":0}}),
    )
}
fn sources_schema() -> Value {
    schema(
        &["category_revision_id"],
        json!({"category_revision_id":{"type":"integer","minimum":1,"description":"The category revision whose cost account the debit must be on."}}),
    )
}
fn asset_schema() -> Value {
    schema(
        &["asset_id"],
        json!({"asset_id":{"type":"integer","minimum":1}}),
    )
}
fn run_schema() -> Value {
    schema(
        &["period"],
        json!({"period":{"type":"string","format":"date","description":"First day of the month. Charges are recognized at month-end; missed scheduled charges are caught up. A consumed month is never rerun, including after reversal."}}),
    )
}
fn event_schema() -> Value {
    schema(
        &["asset_id", "effective_date", "reason"],
        json!({"asset_id":{"type":"integer","minimum":1},"effective_date":{"type":"string","description":"ISO date, YYYY-MM-DD."},"reason":{"type":"string","maxLength":500}}),
    )
}
fn reverse_run_schema() -> Value {
    let mut s = event_schema();
    let p = s["properties"].as_object_mut().expect("object");
    p.remove("asset_id");
    p.insert("run_id".into(), json!({"type":"integer","minimum":1}));
    s["required"] = json!(["company_id", "run_id", "effective_date", "reason"]);
    s
}
fn disposal_schema() -> Value {
    let mut s = event_schema();
    s["properties"]["proceeds"] = json!({"type":"string","description":"Nonnegative amount in the book's minor units; zero for a write-off."});
    s["properties"]["settlement_account_id"] = json!({"type":"integer","minimum":1,"description":"Active postable asset account, required for positive proceeds."});
    s["required"]
        .as_array_mut()
        .expect("array")
        .push(json!("proceeds"));
    s
}
fn acquisition_schema() -> Value {
    schema(
        &[
            "source",
            "source_id",
            "line_number",
            "category_revision_id",
            "description",
            "in_service_date",
        ],
        json!({"source":{"type":"string","enum":["entry","bill"]},"source_id":{"type":"integer","minimum":1},"line_number":{"type":"integer","minimum":1,"maximum":2147483647},"category_revision_id":{"type":"integer","minimum":1},"description":{"type":"string","maxLength":500},"in_service_date":{"type":"string","format":"date"},"residual_value":{"type":"string"},"method":{"type":"string","enum":["straight_line","reducing_balance"]},"life_months":{"type":"integer","minimum":1,"maximum":1200},"annual_rate":{"type":"string"}}),
    )
}
fn output() -> Value {
    json!({"type":"object","properties":{
        "record":{"type":"object","description":"The saved revision, asset, run or event. Amounts are exact decimal strings; dates are ISO dates."},
        "records":{"type":"array","items":{"type":"object"},"description":"Categories with revision history, runs newest month first with items, exceptions and reversal state, or acquisition sources newest first."},
        "report":{"type":"object","required":["company_id","as_of","minor_units","through_entry_number","through_asset_number","through_event_number","through_category_revision_id","rows","reconciliation"],"properties":{
            "company_id":{"type":"integer"},"as_of":{"type":"string","format":"date"},"currency_code":{"type":"string"},"minor_units":{"type":"integer"},
            "through_entry_number":{"type":"integer"},"through_asset_number":{"type":"integer"},"through_event_number":{"type":"integer"},"through_category_revision_id":{"type":"integer"},
            "rows":{"type":"array","items":{"type":"object"},"description":"Frozen asset and category revision, posted accumulation, carrying value, disposal state and full projected schedule."},
            "reconciliation":{"type":"array","items":{"type":"object","properties":{"account_id":{"type":"integer"},"kind":{"type":"string","enum":["Balance","Activity"]},"ledger_balance":{"type":"string"},"register_balance":{"type":"string"},"difference":{"type":"string"},"other_postings":{"type":"string","description":"Activity rows only: postings on this depreciation, gain or loss account that Fixed assets did not make (sales on a shared income account, a manual journal). Not a difference."}}}}
        }}
    }})
}

#[derive(Clone, Copy)]
enum Read {
    Categories,
    Register,
    Schedule,
    Asset,
    Runs,
    Sources,
}
impl ReadHandler for Read {
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
            let owner = owner().map_err(failure)?;
            let company = company(input).map_err(failure)?;
            let result = match self {
                Self::Categories => {
                    json!({"records":owner.categories_on(c,s,company).await.map_err(failure)?})
                }
                Self::Register => {
                    let mut report = owner
                        .register_report_on(
                            c,
                            s,
                            company,
                            date(input, "as_of").map_err(failure)?,
                            &pins(input).map_err(failure)?,
                        )
                        .await
                        .map_err(failure)?;
                    // Filters select records, while the complete register still
                    // ties out to the ledger at the same four report pins.
                    report.rows.retain(|row| {
                        let opening = row.asset.opening_date.is_some();
                        let source = match input["source"].as_str() {
                            Some("opening") => opening,
                            Some("ordinary") => !opening,
                            _ => true,
                        };
                        let status = if row.voided {
                            "voided"
                        } else if row.disposed {
                            "disposed"
                        } else {
                            "in_service"
                        };
                        source
                            && input["status"]
                                .as_str()
                                .is_none_or(|wanted| wanted == status)
                    });
                    json!({"report":report})
                }
                Self::Schedule => {
                    json!({"report":owner.register_report_on(c,s,company,date(input,"as_of").map_err(failure)?,&pins(input).map_err(failure)?).await.map_err(failure)?})
                }
                Self::Asset => {
                    json!({"record":owner.asset_on(c,s,company,id(input,"asset_id").map_err(failure)?).await.map_err(failure)?})
                }
                Self::Runs => json!({"records":owner.runs_on(c,s,company).await.map_err(failure)?}),
                Self::Sources => {
                    json!({"records":owner.acquisition_sources_on(c,s,company,id(input,"category_revision_id").map_err(failure)?).await.map_err(failure)?})
                }
            };
            Ok(result.into())
        })
    }
}
#[derive(Clone, Copy)]
enum Write {
    Category,
    Acquire,
    Depreciate,
    ReverseRun,
    Dispose,
    ReverseDisposal,
    Void,
}
impl Write {
    fn cap(self) -> &'static str {
        match self {
            Self::Category => CONFIGURE,
            Self::Acquire => ACQUIRE,
            Self::Depreciate => DEPRECIATE,
            Self::ReverseRun | Self::ReverseDisposal => REVERSE,
            Self::Dispose => DISPOSE,
            Self::Void => VOID,
        }
    }
    async fn review(
        self,
        owner: &FixedAssets,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        // Prove reach before the nonblocking lock, then reauthorize through Core.
        owner
            .lifecycle_preview_lock(c, s, company, self.cap())
            .await?;
        owner.opening_authorize(c, s, company, self.cap()).await?;
        match self {
            Self::Category => {
                let revision: RevisionInput = decode(input)?;
                let (name, defaults) = owner
                    .prepare_category(
                        c,
                        s,
                        company,
                        input["category_id"].as_i64(),
                        input["name"].as_str(),
                        &revision,
                    )
                    .await?;
                let accounts = owner.ledger.list_accounts_on(c, s, company).await?;
                Ok(
                    json!({"name":name,"defaults":defaults,"category_revisions":store::all(c,store::Table::Revision,s.tenant(),company).await?.into_iter().filter(|r|input["category_id"].as_i64().is_some_and(|id|r["category_id"].as_i64()==Some(id))).collect::<Vec<_>>(),"accounts":accounts.into_iter().filter(|a|[defaults.cost_account_id,defaults.accumulated_account_id,defaults.expense_account_id,defaults.gain_account_id,defaults.loss_account_id].contains(&a.id)).collect::<Vec<_>>()}),
                )
            }
            Self::Acquire => {
                let a = acquisition(owner, c, s, company, input).await?;
                let row = owner
                    .prepare_acquisition(
                        c,
                        s,
                        company,
                        id(input, "source_id")?,
                        line(input)?,
                        input["source"] == "bill",
                        &a,
                    )
                    .await?;
                Ok(
                    json!({"asset":row,"revision":store::revision(c,s.tenant(),company,a.category_revision_id).await?,"entry":owner.ledger.get_entry_on(c,s,company,row["entry_id"].as_i64().ok_or(Error::Invalid)?).await?}),
                )
            }
            Self::Depreciate => Ok(owner
                .prepare_run(c, s, company, date(input, "period")?)
                .await?
                .review),
            Self::ReverseRun => Ok(owner
                .prepare_reverse_run(c, s, company, id(input, "run_id")?, &event(input)?)
                .await?
                .review),
            Self::Dispose => Ok(owner
                .prepare_dispose(
                    c,
                    s,
                    company,
                    id(input, "asset_id")?,
                    &disposal(owner, c, s, company, input).await?,
                )
                .await?
                .review),
            Self::ReverseDisposal => Ok(owner
                .prepare_reverse_disposal(c, s, company, id(input, "asset_id")?, &event(input)?)
                .await?
                .review),
            Self::Void => Ok(owner
                .prepare_void_acquisition(c, s, company, id(input, "asset_id")?, &event(input)?)
                .await?
                .review),
        }
    }
}
fn line(input: &Value) -> Result<i32> {
    i32::try_from(id(input, "line_number")?).map_err(|_| invalid("line_number", "is invalid"))
}
async fn acquisition(
    owner: &FixedAssets,
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    input: &Value,
) -> Result<AcquisitionInput> {
    let units = owner.ledger.get_book_on(c, s, company).await?.minor_units;
    let mut v = input.clone();
    if let Some(text) = v["residual_value"].as_str() {
        v["residual_value"] =
            json!(Money::parse(text, units).map_err(|e| invalid("residual_value", e.to_string()))?);
    }
    decode(&v)
}
fn event_errors(input: &Value, errors: &mut Vec<FieldError>) -> Option<EventInput> {
    let reason = input["reason"].as_str().unwrap_or("");
    if reason.trim().is_empty() {
        errors.push(FieldError::new("reason", "can't be blank"));
    }
    let date = input["effective_date"].as_str().unwrap_or("");
    let effective_date = date.parse::<Date>().ok();
    if effective_date.is_none() {
        errors.push(FieldError::new(
            "effective_date",
            if date.is_empty() {
                "can't be blank"
            } else {
                "is invalid"
            },
        ));
    }
    Some(EventInput {
        effective_date: effective_date?,
        reason: reason.into(),
    })
}
fn finish(mut errors: Vec<FieldError>) -> Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    errors.sort_by_key(|e| e.field);
    Err(FixedAssetsError::Invalid(errors))
}
fn event(input: &Value) -> Result<EventInput> {
    let mut errors = Vec::new();
    let event = event_errors(input, &mut errors);
    finish(errors)?;
    event.ok_or_else(|| invalid("base", "contains an invalid field value"))
}
async fn disposal(
    owner: &FixedAssets,
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    input: &Value,
) -> Result<DisposalInput> {
    let units = owner.ledger.get_book_on(c, s, company).await?.minor_units;
    let mut errors = Vec::new();
    let event = event_errors(input, &mut errors);
    let text = input["proceeds"].as_str().unwrap_or("");
    let proceeds = if text.trim().is_empty() {
        errors.push(FieldError::new("proceeds", "can't be blank"));
        None
    } else {
        match Money::parse(text, units) {
            Ok(m) => Some(m),
            Err(e) => {
                errors.push(FieldError::new("proceeds", e.to_string()));
                None
            }
        }
    };
    finish(errors)?;
    match (event, proceeds) {
        (Some(event), Some(proceeds)) => Ok(DisposalInput {
            event,
            proceeds,
            settlement_account_id: input["settlement_account_id"].as_i64(),
        }),
        _ => Err(invalid("base", "contains an invalid field value")),
    }
}
impl WriteHandler for Write {
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
            let review = self
                .review(
                    owner().map_err(failure)?,
                    c,
                    s,
                    company(input).map_err(failure)?,
                    input,
                )
                .await
                .map_err(failure)?;
            fence(
                c,
                &json!({"lifecycle":input,"action":self.cap(),"review":review}),
                false,
            )
            .await?;
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
            let o = owner().map_err(failure)?;
            let company = company(input).map_err(failure)?;
            o.lifecycle_writable(tx, s, company, self.cap())
                .await
                .map_err(failure)?;
            let review = self
                .review(o, tx.sql(), s, company, input)
                .await
                .map_err(failure)?;
            fence(
                tx.sql(),
                &json!({"lifecycle":input,"action":self.cap(),"review":review}),
                true,
            )
            .await?;
            let result: Result<Value> = async {
                Ok(match self {
                    Self::Category => json!(
                        o.save_category_on(
                            tx,
                            s,
                            company,
                            input["category_id"].as_i64(),
                            input["name"].as_str(),
                            &decode(input)?
                        )
                        .await?
                    ),
                    Self::Acquire => {
                        let a = acquisition(o, tx.sql(), s, company, input).await?;
                        json!(
                            o.acquire_on(
                                tx,
                                s,
                                company,
                                id(input, "source_id")?,
                                line(input)?,
                                input["source"] == "bill",
                                &a
                            )
                            .await?
                        )
                    }
                    Self::Depreciate => json!(
                        o.depreciate_on(tx, s, company, date(input, "period")?)
                            .await?
                    ),
                    Self::ReverseRun => json!(
                        o.reverse_run_on(tx, s, company, id(input, "run_id")?, &event(input)?)
                            .await?
                    ),
                    Self::Dispose => {
                        let d = disposal(o, tx.sql(), s, company, input).await?;
                        json!(
                            o.dispose_on(tx, s, company, id(input, "asset_id")?, &d)
                                .await?
                        )
                    }
                    Self::ReverseDisposal => json!(
                        o.reverse_disposal_on(
                            tx,
                            s,
                            company,
                            id(input, "asset_id")?,
                            &event(input)?
                        )
                        .await?
                    ),
                    Self::Void => json!(
                        o.void_acquisition_on(
                            tx,
                            s,
                            company,
                            id(input, "asset_id")?,
                            &event(input)?
                        )
                        .await?
                    ),
                })
            }
            .await;
            Ok(json!({"record":result.map_err(failure)?}).into())
        })
    }
}
impl FixedAssets {
    pub(super) async fn lifecycle_preview_lock(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        capability: &str,
    ) -> Result<()> {
        // Prove capability and the sealed desk reach first, without a company
        // lock, so a competing writer can yield Conflict instead of blocking
        // the preview. The caller rechecks through Core after this try-lock.
        // The try-lock is the company write lock itself (NO KEY UPDATE, as
        // `require_writable` takes): a no-op inside a write that holds it, and
        // compatible with the KEY SHARE every authorizer takes. FOR UPDATE
        // would conflict with those readers and refuse at random.
        bilibi_authz::authorize_proof(c, s, capability)
            .await
            .map_err(|e| match e {
                Error::Forbidden | Error::Unauthorized => FixedAssetsError::Unauthorized,
                e => FixedAssetsError::Failed(e),
            })?;
        if s.principal().is_none()
            && !self
                .parties
                .reachable_organisations(c, s)
                .await?
                .iter()
                .any(|target| target.id.get() == company)
        {
            return Err(FixedAssetsError::Unauthorized);
        }
        let Some(row) = c
            .query_opt(
                "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR NO KEY UPDATE SKIP LOCKED",
                &[&s.tenant(), &company],
            )
            .await?
        else {
            return Err(
                if c.query_opt(
                    "SELECT 1 FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                    &[&s.tenant(), &company],
                )
                .await?
                .is_some()
                {
                    FixedAssetsError::Failed(Error::Conflict)
                } else {
                    FixedAssetsError::CompanyNotFound
                },
            );
        };
        if row.get::<_, String>(0) == "archived" {
            return Err(FixedAssetsError::CompanyArchived);
        }
        Ok(())
    }
    pub(super) async fn lifecycle_writable(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        cap: &str,
    ) -> Result<()> {
        authorize(tx.sql(), s, company, cap).await?;
        bilibi_company::require_writable(tx.sql(), s, company, true)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                bilibi_company::WriteError::Archived => FixedAssetsError::CompanyArchived,
                bilibi_company::WriteError::NotFound => FixedAssetsError::CompanyNotFound,
                bilibi_company::WriteError::Access(e) => FixedAssetsError::Failed(e),
                _ => FixedAssetsError::Failed(Error::Internal),
            })
    }
}
macro_rules! op {
    ($key:literal,$title:literal,$summary:literal,$cap:expr,$input:ident,$handler:expr) => {
        Operation {
            key: concat!("finance.fixed_assets.", $key),
            title: $title,
            summary: $summary,
            keywords: &["assets", "depreciation", "lifecycle"],
            capability: $cap,
            input: $input,
            output,
            handler: $handler,
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
inventory::submit! {Contribution::new(DEFINITIONS,"finance/fixed_assets.lifecycle",Provenance{module:MODULE,source:file!(),line:line!()},&Definitions{operations:&[
    op!("category.list","Asset categories","Read categories and their immutable revision history.",VIEW,target,Handler::Read(&Read::Categories)),
    op!("category.save","Prepare asset category","Create a category or append a complete revision with eligible posting accounts and depreciation defaults. Existing assets retain their original revision.",CONFIGURE,category_schema,Handler::SafeWrite(&Write::Category)),
    op!("register.get","Fixed asset register","Read dated posted accumulation, carrying values, projected schedules and account reconciliation. Filter rows by opening or ordinary source and status. Reconciliation always covers the complete register. Reproduce with the filters, date and all four returned pins.",VIEW,register_schema,Handler::Read(&Read::Register)),
    op!("schedule.get","Depreciation schedules","Read projected schedules in the dated pinned register. Scheduled amounts are not posted charges.",VIEW,schedule_schema,Handler::Read(&Read::Schedule)),
    op!("asset.get","Read asset","Read frozen source assumptions, prospective schedule, posted charges and corrections.",VIEW,asset_schema,Handler::Read(&Read::Asset)),
    op!("acquisition.sources","Acquisition sources","List the posted bill and journal lines a category's cost account still has to register, newest first, with the source, source_id and line_number asset.acquire takes.",VIEW,sources_schema,Handler::Read(&Read::Sources)),
    op!("asset.acquire","Register acquisition","Recognise one complete posted acquisition debit without reposting or taking ownership. Opening aggregates must use opening.import instead.",ACQUIRE,acquisition_schema,Handler::SafeWrite(&Write::Acquire)),
    op!("run.list","Depreciation runs","Read runs newest month first, including items, exceptions and reversals.",VIEW,target,Handler::Read(&Read::Runs)),
    op!("run.post","Post monthly depreciation","Review exact catch-up charges and skipped reversed acquisitions. A month is consumed forever; identical retries return its run.",DEPRECIATE,run_schema,Handler::SafeWrite(&Write::Depreciate)),
    op!("run.reverse","Reverse depreciation run","Reverse the latest standing run after reversing standing disposals. A reversed month cannot be posted again; the next month catches up.",REVERSE,reverse_run_schema,Handler::SafeWrite(&Write::ReverseRun)),
    op!("asset.dispose","Dispose asset","Review cost removal, accumulated depreciation, proceeds and gain or loss. Depreciation must be current through the last completed month.",DISPOSE,disposal_schema,Handler::SafeWrite(&Write::Dispose)),
    op!("disposal.reverse","Reverse asset disposal","Reverse the owned disposal posting and restore the asset through a new event.",REVERSE,event_schema,Handler::SafeWrite(&Write::ReverseDisposal)),
    op!("acquisition.void","Void acquisition","After the source entry is reversed, reverse this asset's standing depreciation and mark its acquisition void. Source ownership remains unchanged.",VOID,event_schema,Handler::SafeWrite(&Write::Void)),
],guides:&[bilibi_agent_api::Guide{key:"finance.fixed_assets.lifecycle",title:"Fixed Assets lifecycle",summary:"Prepare categories, continue opening assets, post depreciation and correct disposals with reproduced reconciliation.",keywords:&["assets","depreciation","disposal"],capability:VIEW,body:include_str!("../lifecycle-guide.md")}]})}
