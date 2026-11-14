//! H4 reach for Bilimbi.Finance.Einvoice's facade-only issuance and reads.
//! Consent, transport and the outer audited transaction belong to Agent API.
use crate::{clearance::PreparedIssue, documents, *};
use bilibi_agent_api::{
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use bilibi_tenancy::OrganisationDirectory;
use serde_json::json;
use std::{sync::OnceLock, time::Duration};

static OWNER: OnceLock<Arc<Einvoice>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Einvoice>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate e-invoice operation binding".into()))
}
fn owner() -> bilibi_agent_api::Result<&'static Einvoice> {
    OWNER.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
fn failure(error: EinvoiceError) -> bilibi_agent_api::Error {
    use bilibi_agent_api::Error as E;
    match error {
        EinvoiceError::Unauthorized => E::Forbidden,
        EinvoiceError::CompanyNotFound | EinvoiceError::DocumentNotFound => E::NotFound,
        EinvoiceError::Failed(error) => error.into(),
        EinvoiceError::Invalid(errors) => {
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for e in errors {
                fields
                    .entry(e.field.replace('[', ".").replace(']', ""))
                    .or_default()
                    .push(e.message);
            }
            E::InvalidInput(fields)
        }
        other => E::InvalidInput(BTreeMap::from([("base".into(), vec![other.to_string()])])),
    }
}
fn invalid(field: impl Into<String>, message: impl Into<String>) -> EinvoiceError {
    EinvoiceError::Invalid(vec![FieldError::new(field, message)])
}
fn id(input: &Value, key: &str) -> Result<i64> {
    input[key]
        .as_i64()
        .filter(|v| *v > 0)
        .ok_or(EinvoiceError::InvalidOption)
}

fn draft(input: &Value) -> Result<Draft> {
    let mut body = input.clone();
    body.as_object_mut()
        .ok_or(EinvoiceError::InvalidOption)?
        .remove("company_id");
    // Give malformed decimal strings their actual input paths, not a generic
    // deserialization failure. Totals and consolidated line tax are not inputs.
    for (field, value) in std::iter::once(("exchange_rate".to_owned(), &body["exchange_rate"]))
        .chain(
            body["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .flat_map(|(n, line)| {
                    [
                        "quantity",
                        "unit_price",
                        "discount_amount",
                        "charge_amount",
                        "tax_rate",
                    ]
                    .into_iter()
                    .map(move |key| (format!("lines.{n}.{key}"), &line[key]))
                }),
        )
    {
        if !value.is_null() {
            let text = value
                .as_str()
                .ok_or_else(|| invalid(&field, "is invalid"))?;
            rust_decimal::Decimal::from_str_exact(text)
                .map_err(|_| invalid(field, "is invalid"))?;
        }
    }
    let draft: Draft =
        serde_json::from_value(body).map_err(|_| invalid("base", "Invalid input"))?;
    let at = draft
        .issued_at
        .ok_or_else(|| invalid("issued_at", "can't be blank"))?;
    if at.subsec_nanosecond() != 0 {
        return Err(invalid("issued_at", "must use whole seconds"));
    }
    // PostgreSQL numeric(20,6) must store exactly what the reviewer saw.
    if let Some(rate) = draft.exchange_rate {
        if rate.normalize().scale() > 6
            || rate.abs() >= rust_decimal::Decimal::from(100_000_000_000_000_i64)
        {
            return Err(invalid(
                "exchange_rate",
                "must fit 14 integer and 6 decimal places",
            ));
        }
    }
    if !draft.source.as_ref().is_some_and(|s| {
        matches!(
            s.module.as_deref(),
            Some("finance/receivables" | "finance/payables")
        )
    }) {
        return Err(invalid(
            "source",
            "must name a Receivables or Payables source",
        ));
    }
    Ok(draft)
}

impl Einvoice {
    async fn operation_company(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        capability: &str,
    ) -> Result<bilibi_tenancy::NamedOrganisation> {
        bilibi_company::authorize_work_target(client, scope, company, capability)
            .await
            .map_err(refusal)?;
        bilibi_company::Directory
            .lookup(
                client,
                scope,
                OrganisationId::new(company).map_err(|_| EinvoiceError::CompanyNotFound)?,
            )
            .await
            .map_err(refusal)?
            .ok_or(EinvoiceError::CompanyNotFound)
    }

    async fn operation_issue(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
    ) -> Result<PreparedIssue> {
        let company = id(input, "company_id")?;
        let target = self
            .operation_company(client, scope, company, ISSUE)
            .await?;
        if target.archived {
            return Err(EinvoiceError::CompanyArchived);
        }
        let draft = draft(input)?;
        // TODO(platform): Settings accepts only GenericClient, not the handed
        // object-safe read client. Only non-secret settings use this fallback.
        // Bound acquisition so requests cannot retain every pool slot while
        // waiting for an extra one. No locks or business reads on this checkout.
        let (profile, environment) = {
            let settings_client = tokio::time::timeout(Duration::from_secs(2), self.db.get())
                .await
                .map_err(|_| Error::Internal)?
                .map_err(Error::from)?;
            self.issue_settings(&**settings_client, scope, company, target.name)
                .await?
        };
        self.prepare_issue(client, scope, company, &draft, profile, environment)
            .await
    }

    async fn operation_document(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        document: i64,
    ) -> Result<DocumentView> {
        self.operation_company(client, scope, company, VIEW).await?;
        let recorded = documents::fetch(client, scope.tenant(), company, document)
            .await?
            .ok_or(EinvoiceError::DocumentNotFound)?;
        let portal = {
            // Same bounded Settings-only workaround as preview. Business data
            // and consolidated/source reads remain on the handed connection.
            let settings_client = tokio::time::timeout(Duration::from_secs(2), self.db.get())
                .await
                .map_err(|_| Error::Internal)?
                .map_err(Error::from)?;
            self.settings
                .get_global(
                    &**settings_client,
                    &format!(
                        "finance.einvoice.{}.portal_url",
                        recorded.document.environment
                    ),
                )
                .await?
        };
        self.view_on(
            client,
            scope,
            company,
            recorded,
            portal.as_str().unwrap_or_default(),
        )
        .await
    }

    async fn documents_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        limit: i64,
        before: Option<i64>,
    ) -> Result<Vec<DocumentSummary>> {
        self.operation_company(client, scope, company, VIEW).await?;
        if !(1..=200).contains(&limit) || before.is_some_and(|v| v <= 0) {
            return Err(EinvoiceError::InvalidOption);
        }
        let ids: Vec<i64> = client.query("SELECT id FROM finance_einvoice_documents WHERE tenant_id=$1 AND company_id=$2 AND ($3::bigint IS NULL OR id<$3) ORDER BY id DESC LIMIT $4", &[&scope.tenant(), &company, &before, &limit]).await?.iter().map(|r|r.get(0)).collect();
        Ok(documents::summaries(client, &Self::authority(), scope.tenant(), company, &ids).await?)
    }
}

fn review(company: i64, prepared: &PreparedIssue) -> Value {
    json!({"company_id":company,"authority":Einvoice::authority().id(),
        "summary":"Issue an immutable fiscal document linked to its source. No submission or new ledger entry.",
        "changes":prepared})
}

// The pinned engine previews every step before executing any. Recheck the
// FIFO head immediately before mutation to catch earlier steps' changes.
async fn fence(
    client: &dyn OperationClient,
    input: &Value,
    review: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let facts =
        json!({"operation":"finance.einvoice.document.issue","input":input,"review":review})
            .to_string();
    let row = client.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'), COALESCE(NULLIF(current_setting('finance.einvoice_reviews',true),''),'[]')::jsonb", &[&facts]).await?.ok_or(Error::Internal)?;
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
    client
        .query_opt(
            "SELECT set_config('finance.einvoice_reviews',$1,true)",
            &[&json!(pending).to_string()],
        )
        .await?;
    Ok(())
}
struct Issue;
impl WriteHandler for Issue {
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
            let company = id(input, "company_id").map_err(failure)?;
            let prepared = owner()?
                .operation_issue(client, scope, input)
                .await
                .map_err(failure)?;
            let review = review(company, &prepared);
            fence(client, input, &review, false).await?;
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
            let service = owner()?;
            let company = id(input, "company_id").map_err(failure)?;
            authorize(tx.sql(), scope, company, ISSUE)
                .await
                .map_err(failure)?;
            bilibi_company::require_writable(tx.sql(), scope, company, true)
                .await
                .map_err(|e| {
                    failure(match e {
                        WriteError::Archived => EinvoiceError::CompanyArchived,
                        WriteError::NotFound => EinvoiceError::CompanyNotFound,
                        WriteError::Access(e) => refusal(e),
                        _ => EinvoiceError::Failed(Error::Internal),
                    })
                })?;
            let draft = draft(input).map_err(failure)?;
            let name = service
                .operation_company(tx.sql(), scope, company, ISSUE)
                .await
                .map_err(failure)?
                .name;
            let (profile, environment) = service
                .issue_settings(tx.sql(), scope, company, name)
                .await
                .map_err(failure)?;
            let prepared = service
                .prepare_issue(tx.sql(), scope, company, &draft, profile, environment)
                .await
                .map_err(failure)?;
            fence(tx.sql(), input, &review(company, &prepared), true).await?;
            let document = service
                .record_issue(tx, scope, company, prepared)
                .await
                .map_err(failure)?;
            // Read on this transaction: a pooled wrapper cannot see this issue.
            let recorded = documents::fetch(tx.sql(), scope.tenant(), company, document)
                .await?
                .ok_or(Error::Internal)?;
            let record = service
                .view_on(tx.sql(), scope, company, recorded, "")
                .await
                .map_err(failure)?;
            Ok(json!({"record":record,"path":null}).into())
        })
    }
}

/// Queues an issued document's submission to the authority, as the page's
/// Submit does. A refusal reads as the page words it.
struct Submit;
fn submit_failure(error: EinvoiceError) -> bilibi_agent_api::Error {
    match error {
        EinvoiceError::Failed(_)
        | EinvoiceError::Unauthorized
        | EinvoiceError::CompanyNotFound
        | EinvoiceError::DocumentNotFound => failure(error),
        other => bilibi_agent_api::Error::InvalidInput(BTreeMap::from([(
            "base".into(),
            vec![crate::web::submit_refusal(other)],
        )])),
    }
}
impl WriteHandler for Submit {
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
            let service = owner()?;
            let company = id(input, "company_id").map_err(failure)?;
            let document = id(input, "document_id").map_err(failure)?;
            let target = service
                .operation_company(client, scope, company, SUBMIT)
                .await
                .map_err(failure)?;
            if target.archived {
                return Err(submit_failure(EinvoiceError::CompanyArchived));
            }
            let recorded = documents::fetch(client, scope.tenant(), company, document)
                .await?
                .ok_or(bilibi_agent_api::Error::NotFound)?;
            if !crate::clearance::submittable(recorded.status()) {
                return Err(submit_failure(EinvoiceError::NotSubmittable));
            }
            let d = &recorded.document;
            Ok(json!({"company_id":company,"authority":Einvoice::authority().id(),
                "summary":"Send this e-invoice to the tax authority. Its answer arrives as a new status.",
                "changes":{"document_id":d.id,"code_number":d.code_number,"environment":d.environment,
                    "payable_amount":d.payable_amount,"currency_code":d.currency_code}})
            .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = owner()?;
            let company = id(input, "company_id").map_err(failure)?;
            let document = id(input, "document_id").map_err(failure)?;
            authorize(tx.sql(), scope, company, SUBMIT)
                .await
                .map_err(failure)?;
            bilibi_company::require_writable(tx.sql(), scope, company, true)
                .await
                .map_err(|e| {
                    submit_failure(match e {
                        WriteError::Archived => EinvoiceError::CompanyArchived,
                        WriteError::NotFound => EinvoiceError::CompanyNotFound,
                        WriteError::Access(e) => refusal(e),
                        _ => EinvoiceError::Failed(Error::Internal),
                    })
                })?;
            let job = service
                .submit_document_on(tx, scope, company, document)
                .await
                .map_err(submit_failure)?;
            Ok(json!({"record":{"document_id":document,"job_id":job},"path":null}).into())
        })
    }
}

enum Read {
    Get,
    List,
    PrefillParty,
    /// The posted records an e-invoice can be started for.
    Sources,
    /// The draft an e-invoice of one posted record starts from.
    SourceDraft,
    /// The code lists a draft's choices come from.
    Codes,
}
impl ReadHandler for Read {
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
            let service = owner()?;
            let company = id(input, "company_id").map_err(failure)?;
            let result = match self {
                Self::PrefillParty => {
                    service
                        .operation_company(client, scope, company, VIEW)
                        .await
                        .map_err(failure)?;
                    json!(service
                        .prefill_party(
                            scope,
                            company,
                            id(input, "counterparty_id").map_err(failure)?
                        )
                        .await
                        .map_err(failure)?)
                }
                Self::Sources => service
                    .sources_on(client, scope, company)
                    .await
                    .map_err(failure)?,
                Self::SourceDraft => {
                    let text = |key: &str| {
                        input["source"][key]
                            .as_str()
                            .map(str::trim)
                            .filter(|text| !text.is_empty() && text.len() <= 100)
                            .ok_or_else(|| failure(invalid("source", "names a module and a record")))
                    };
                    service
                        .source_draft_on(client, scope, company, text("module")?, text("record")?)
                        .await
                        .map_err(failure)?
                }
                Self::Codes => service
                    .choice_codes_on(client, scope, company)
                    .await
                    .map_err(failure)?,
                Self::Get => json!(service
                    .operation_document(
                        client,
                        scope,
                        company,
                        id(input, "document_id").map_err(failure)?
                    )
                    .await
                    .map_err(failure)?),
                Self::List => {
                    let limit = input["page_size"].as_i64().unwrap_or(50).min(100);
                    let before = input
                        .get("before")
                        .map(|_| id(input, "before"))
                        .transpose()
                        .map_err(failure)?;
                    let mut rows = service
                        .documents_on(client, scope, company, limit + 1, before)
                        .await
                        .map_err(failure)?;
                    let more = rows.len() > limit as usize;
                    rows.truncate(limit as usize);
                    json!({"next_before":if more {rows.last().map(|r|r.id)} else {None},"records":rows})
                }
            };
            Ok(result.into())
        })
    }
}

fn identifier() -> Value {
    json!({"type":"integer","minimum":1,"maximum":i64::MAX})
}
fn string(max: usize) -> Value {
    json!({"type":"string","maxLength":max})
}
fn object(required: &[&str], properties: Value) -> Value {
    json!({"type":"object","required":required,"properties":properties})
}
fn party() -> Value {
    object(
        &[
            "name",
            "tax_id",
            "registration_type",
            "registration_number",
            "address",
        ],
        json!({
            "name":string(300),"tax_id":string(50),"registration_type":string(20),"registration_number":string(50),
            "sales_tax_registration":string(50),"email":string(255),"phone":string(50),"industry_code":string(16),"business_activity":string(300),
            "address":object(&["lines","city","state_code","country_code"],json!({"lines":{"type":"array","items":string(300)},"city":string(100),"postcode":string(20),"state_code":string(16),"country_code":string(16)}))
        }),
    )
}
fn issue() -> Value {
    object(
        &[
            "company_id",
            "document_type",
            "code_number",
            "issued_at",
            "currency_code",
            "lines",
            "source",
        ],
        json!({
            "company_id":identifier(),"document_type":string(2),"code_number":string(50),"issued_at":{"type":"string","format":"date-time"},"currency_code":string(3),"exchange_rate":string(40),
            "payment_means_code":string(16),"payment_terms":string(300),"billing_period_start":{"type":"string","format":"date"},"billing_period_end":{"type":"string","format":"date"},"billing_period_description":string(300),"original_document_id":identifier(),
            "buyer":party(),"supplier":party(),
            "lines":{"type":"array","items":object(&["description","classification_code","quantity","unit_price","tax_type","tax_rate"],json!({"description":string(300),"classification_code":string(16),"tariff_code":string(32),"origin_country_code":string(3),"quantity":string(40),"unit_code":string(16),"unit_price":string(40),"discount_amount":string(40),"charge_amount":string(40),"tax_type":string(8),"tax_rate":string(40),"tax_exemption_reason":string(300)}))},
            "source":object(&["module","record","ledger_entry_id"],json!({"module":{"type":"string","enum":["finance/receivables","finance/payables"]},"record":string(100),"ledger_entry_id":identifier()}))
        }),
    )
}
fn get() -> Value {
    object(
        &["company_id", "document_id"],
        json!({"company_id":identifier(),"document_id":identifier()}),
    )
}
fn list() -> Value {
    object(
        &["company_id"],
        json!({"company_id":identifier(),"before":identifier(),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn prefill_party() -> Value {
    object(
        &["company_id", "counterparty_id"],
        json!({"company_id":identifier(),"counterparty_id":identifier()}),
    )
}
fn company_only() -> Value {
    object(&["company_id"], json!({"company_id":identifier()}))
}
fn source_draft() -> Value {
    object(
        &["company_id", "source"],
        json!({"company_id":identifier(),"source":object(&["module","record"],json!({"module":string(100),"record":string(100)}))}),
    )
}
fn output() -> Value {
    json!({"type":"object","description":"Immutable fiscal document with exact decimal strings and ISO instants, a newest-first summary cursor page, or reviewed immutable issue snapshots. No credentials. Issuance returns a null navigation path."})
}

inventory::submit! { Contribution::new(DEFINITIONS,"finance/einvoice.operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {
    operations:&[
        Operation {key:"finance.einvoice.document.issue",title:"Issue an e-invoice document",summary:"Record an immutable fiscal document linked to a Receivables or Payables entry. Requires ledger view too. Does not submit or post a second entry.",keywords:&["invoice","self-billed","tax document"],capability:ISSUE,input:issue,output,handler:Handler::SafeWrite(&Issue),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.document.submit",title:"Send an e-invoice to the tax authority",summary:"Queue an issued, released, refused or invalid document for submission. The authority's answer arrives later as a new status; read the document to see it.",keywords:&["submit","send","MyInvois"],capability:SUBMIT,input:get,output,handler:Handler::SafeWrite(&Submit),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.document.get",title:"Read an e-invoice document",summary:"Read immutable parties, lines and totals, status events, authority identifiers, source links and ledger follow-up flags.",keywords:&[],capability:VIEW,input:get,output,handler:Handler::Read(&Read::Get),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.document.list",title:"List e-invoice documents",summary:"Read document summaries newest first with an exclusive before cursor; at most 100 per page, default 50.",keywords:&[],capability:VIEW,input:list,output,handler:Handler::Read(&Read::List),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.party.prefill",title:"Prefill an e-invoice party",summary:"Read visible Core company facts for a fiscal draft; restricted facts refuse the prefill.",keywords:&[],capability:VIEW,input:prefill_party,output,handler:Handler::Read(&Read::PrefillParty),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.source.list",title:"List records that can get an e-invoice",summary:"Read the newest issued invoices and credit notes with no e-invoice that stands, each as the source to start one from.",keywords:&["invoice","credit note"],capability:VIEW,input:company_only,output,handler:Handler::Read(&Read::Sources),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.document.prefill",title:"Prefill an e-invoice from its source",summary:"Read the draft an e-invoice of one issued invoice or credit note starts from: its buyer and lines, with the codes this company last sent for them. Records nothing.",keywords:&["draft","invoice"],capability:VIEW,input:source_draft,output,handler:Handler::Read(&Read::SourceDraft),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.einvoice.code.list",title:"List e-invoice codes",summary:"Read the company's classification, state, country, tax type and registration type codes with their names, for a draft's choices.",keywords:&["classification","tax type","state"],capability:VIEW,input:company_only,output,handler:Handler::Read(&Read::Codes),channels:Channels::Every,step_up:&[]},
    ],
    guides:&[Guide {key:"finance.einvoice",title:"Source-linked fiscal documents",summary:"Review and issue fiscal documents without duplicating source accounting; inspect documents and their clearance status.",keywords:&[],capability:VIEW,body:include_str!("../guide.md")}],
}) }
