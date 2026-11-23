//! Reviewed customer statements and reminders, delivered through the shared mail worker.
use crate::{
    agent_operations, invalid, AgingOptions, PdfFile, ReceivableError, Receivables, Result,
    MANAGE_DOCUMENTS, MODULE, VIEW,
};
use askama::Template;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_artifacts::{HtmlFuture, HtmlOwner, Operation as FileOperation, Owner, Reference};
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_templates::{
    Audience, Context, DefaultText, Output, Slot, Template as Wording, Value as Fact, Variable,
    CATALOGUE,
};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use tokio_postgres::Transaction;

const KEY: &str = "finance.receivables.statement_cover";
const SUBJECT_LIMIT: usize = 200;
pub(super) const BODY_LIMIT: usize = 16_000;
const REMINDER: &str = " We would appreciate payment of any overdue amounts.";

inventory::submit! { Contribution::new(CATALOGUE, KEY, Provenance { module:MODULE, source:file!(), line:line!() }, &Wording {
    key: KEY,
    label: "Customer statement email",
    audience: Audience::Customer,
    outputs: &[Output::PlainText],
    base_language: "en",
    slots: &[
        Slot { id:"subject", label:"Subject", context:Context::Subject, required:true, max_bytes:SUBJECT_LIMIT },
        Slot { id:"body", label:"Message", context:Context::PlainText, required:true, max_bytes:BODY_LIMIT },
    ],
    variables: &[
        Variable { id:"customer_name", label:"Customer name", optional:false, sample:Fact::Text("Acme Trading"), slots:&["subject","body"] },
        Variable { id:"statement_date", label:"Statement date", optional:false, sample:Fact::Date(jiff::civil::date(2026, 11, 20)), slots:&["subject","body"] },
        Variable { id:"overdue_reminder", label:"Overdue payment reminder", optional:true, sample:Fact::Text(REMINDER), slots:&["body"] },
    ],
    defaults: &[DefaultText {
        language:"en", version:"1", slots:&[
            ("subject","Your statement"),
            ("body","Please find your statement attached.{{overdue_reminder}}"),
        ],
    }],
}) }

pub(super) const DRAFT: &str = "finance.receivables.statement.draft";
struct DraftStatement;

fn draft_input() -> Value {
    json!({"type":"object","required":["company_id","customer_id","as_of"],"properties":{
        "company_id":{"type":"integer","minimum":1},"customer_id":{"type":"integer","minimum":1},
        "as_of":{"type":"string","format":"date"},"language":{"type":"string","maxLength":80}
    }})
}

impl ReadHandler for DraftStatement {
    fn owner(&self) -> &'static str {
        MODULE
    }

    fn requires_transaction(&self) -> bool {
        true
    }

    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = agent_operations::owner()?;
            let tx = client.transaction().ok_or(Error::Internal)?;
            let draft = draft(service, tx, scope, input)
                .await
                .map_err(|error| agent_operations::failure(error, "lines"))?;
            Ok(draft.into())
        })
    }
}

fn statement_request(input: &Value) -> Result<(i64, i64, jiff::civil::Date)> {
    let id = |name: &str| {
        input[name]
            .as_i64()
            .filter(|n| *n > 0)
            .ok_or(ReceivableError::InvalidOption)
    };
    let date = input["as_of"]
        .as_str()
        .ok_or(ReceivableError::InvalidOption)?
        .parse()
        .map_err(|_| invalid("as_of", "Choose a valid date."))?;
    Ok((id("company_id")?, id("customer_id")?, date))
}

async fn draft(
    service: &Receivables,
    tx: &Transaction<'_>,
    scope: &Scope,
    input: &Value,
) -> Result<Value> {
    let (company, customer_id, as_of) = statement_request(input)?;
    service
        .authorize_on(tx, scope, company, MANAGE_DOCUMENTS)
        .await?;
    let report = service
        .aged_receivables_on(tx, scope, company, as_of, &AgingOptions::default())
        .await?;
    let party = statement_customer(&report, customer_id)?;
    let row = crate::store::find(
        tx,
        crate::store::CUSTOMERS,
        scope.tenant(),
        company,
        Some(customer_id),
    )
    .await?
    .ok_or(ReceivableError::CustomerNotFound)?;
    let customer = service.customer(tx, scope, row).await?;
    let company = OrganisationId::new(company).map_err(|_| ReceivableError::InvalidOption)?;
    let template = service.templates.get(KEY)?;
    let resolved = service
        .templates
        .resolve(
            tx,
            scope,
            KEY,
            Some(company),
            input["language"].as_str().unwrap_or(template.base_language),
            None,
        )
        .await?;
    let mut facts = vec![
        ("customer_name", Fact::Text(&customer.name)),
        ("statement_date", Fact::Date(as_of)),
    ];
    if party
        .documents
        .iter()
        .any(|document| document.days_overdue > 0)
    {
        facts.push(("overdue_reminder", Fact::Text(REMINDER)));
    }
    let slots = template
        .render(&resolved.slots, &facts)
        .map_err(|problem| Error::refused(problem.to_string()))?;
    Ok(json!({
        "name":customer.name,"recipient":customer.party.email.unwrap_or_default(),"slots":slots,
        "template":{"language":resolved.language,"origin":resolved.origin}
    }))
}

fn statement_customer(report: &crate::AgedReceivables, id: i64) -> Result<&crate::AgedCustomer> {
    report
        .customers
        .iter()
        .find(|customer| customer.customer_id == id)
        .ok_or_else(|| {
            invalid(
                "customer_id",
                "This customer has no open balance on that date.",
            )
        })
}

struct SendStatement;
fn input() -> Value {
    json!({"type":"object","required":["company_id","customer_id","as_of","recipient","subject","body"],"properties":{
        "company_id":{"type":"integer","minimum":1},"customer_id":{"type":"integer","minimum":1},"as_of":{"type":"string","format":"date"},
        "recipient":{"type":"string","maxLength":254},"subject":{"type":"string","maxLength":SUBJECT_LIMIT},"body":{"type":"string","maxLength":BODY_LIMIT}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/receivables.statement_operations", Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions { operations:&[Operation {
    key:DRAFT,title:"Prepare statement wording",summary:"Fill a statement email with the wording for the authorised company and requested language.",keywords:&["statement","email"],capability:VIEW,input:draft_input,output,handler:Handler::Read(&DraftStatement),channels:Channels::Every,step_up:&[]
}, Operation {
    key:"finance.receivables.statement.send",title:"Send a customer statement",summary:"Review a customer's open invoices and credits at the chosen date, then queue a statement PDF with the email.",keywords:&["reminder","chase"],capability:MANAGE_DOCUMENTS,input,output,handler:Handler::SafeWrite(&SendStatement),channels:Channels::Every,step_up:&[]
}], guides:&[] }) }

async fn review(
    service: &Receivables,
    tx: &Transaction<'_>,
    scope: &Scope,
    input: &Value,
) -> Result<Value> {
    let (company, customer, as_of) = statement_request(input)?;
    service.join(tx, scope, company, MANAGE_DOCUMENTS).await?;
    let recipient = input["recipient"].as_str().unwrap_or("").trim();
    bilibi_mail::Address::new(None, recipient)
        .map_err(|_| invalid("recipient", "Enter a valid email address."))?;
    let subject = input["subject"].as_str().unwrap_or("");
    if subject.trim().is_empty()
        || subject.len() > SUBJECT_LIMIT
        || subject
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(invalid(
            "subject",
            "Enter a subject of up to 200 characters.",
        ));
    }
    let body = input["body"].as_str().unwrap_or("").replace("\r\n", "\n");
    if body.trim().is_empty() || body.len() > BODY_LIMIT {
        return Err(invalid(
            "body",
            "Enter a message of up to 16,000 characters.",
        ));
    }
    let report = service
        .aged_receivables_on(tx, scope, company, as_of, &AgingOptions::default())
        .await?;
    let party = statement_customer(&report, customer)?;
    Ok(
        json!({"company_id":company,"customer_id":customer,"recipient":recipient,"subject":subject,"body":body,
        "statement":{"as_of":report.as_of,"currency":report.currency_code,"minor_units":report.minor_units.get(),"customer":party},
        "through_entry_number":report.through_entry_number,"through_opening_batch_id":report.through_opening_batch_id,"through_settlement_number":report.through_settlement_number}),
    )
}
impl WriteHandler for SendStatement {
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
            let tx = client.transaction().ok_or(Error::Internal)?;
            let facts = review(agent_operations::owner()?, tx, scope, input)
                .await
                .map_err(|e| agent_operations::failure(e, "lines"))?;
            agent_operations::fence(client, &facts, false).await?;
            Ok(facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = agent_operations::owner()?;
            let facts = review(service, tx.sql(), scope, input)
                .await
                .map_err(|e| agent_operations::failure(e, "lines"))?;
            agent_operations::fence(tx.sql(), &facts, true).await?;
            let company = facts["company_id"].as_i64().ok_or(Error::Invalid)?;
            let data = json!({"customer_id":facts["customer_id"],"statement":facts["statement"],"recipient":facts["recipient"],"subject":facts["subject"],"body":facts["body"],"status":"queued"});
            let row = service
                .queue_delivery_on(tx, scope, company, data)
                .await
                .map_err(|e| agent_operations::failure(e, "lines"))?;
            Ok(json!({"record":row,"path":"/finance/receivables/aging"}).into())
        })
    }
}

struct StatementOwner {
    html: String,
}
impl Owner for StatementOwner {
    fn owner_id(&self) -> &'static str {
        MODULE
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: FileOperation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let id = reference
                .filter(|r| r.kind == "customer-statement")
                .and_then(|r| r.subject.strip_prefix("mail-"))
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
                .ok_or(Error::Forbidden)?;
            if !matches!(operation, FileOperation::Create | FileOperation::Read) {
                return Err(Error::Forbidden);
            }
            bilibi_company::authorize_work_target(client, scope, company.get(), VIEW).await?;
            if client.query_opt("SELECT id FROM finance_receivables_mail WHERE tenant_id=$1 AND company_id=$2 AND id=$3 AND statement IS NOT NULL", &[&scope.tenant(), &company.get(), &id]).await?.is_none() { return Err(Error::NotFound); }
            Ok(())
        })
    }
}
impl HtmlOwner for StatementOwner {
    fn render_html<'a>(
        &'a self,
        _: &'a Scope,
        _: OrganisationId,
        _: &'a Reference,
        _: &'a Value,
    ) -> HtmlFuture<'a> {
        Box::pin(async move { Ok(self.html.clone()) })
    }
}
struct StatementRow {
    reference: String,
    date: String,
    due: String,
    amount: String,
}
#[derive(Template)]
#[template(
    source = r#"<!doctype html><html><head><meta charset="utf-8"><style>body{font:14px sans-serif;color:#222}table{border-collapse:collapse;width:100%}th,td{padding:8px;border-bottom:1px solid #ddd;text-align:left}th:last-child,td:last-child{text-align:right}.block{white-space:pre-line}</style></head><body><header><h2>{{ company_name }}</h2>{% if !layout.registration_number.is_empty() %}<div class="block">Registration number: {{ layout.registration_number }}</div>{% endif %}{% if !layout.tax_number.is_empty() %}<div class="block">Tax registration number: {{ layout.tax_number }}</div>{% endif %}<div class="block">{{ layout.address }}</div><div class="block">{{ layout.contact }}</div></header><h1>Customer statement</h1><p>{{ customer }} · {{ date }}</p><p>Amounts in {{ currency }}</p><table><thead><tr><th>Document</th><th>Issued</th><th>Due</th><th>Outstanding</th></tr></thead><tbody>{% for row in rows %}<tr><td>{{ row.reference }}</td><td>{{ row.date }}</td><td>{{ row.due }}</td><td>{{ row.amount }}</td></tr>{% endfor %}</tbody></table><p>Total owed: {{ total }} {{ currency }}</p></body></html>"#,
    ext = "html"
)]
struct StatementView {
    company_name: String,
    layout: crate::InvoiceLayout,
    customer: String,
    date: String,
    currency: String,
    rows: Vec<StatementRow>,
    total: String,
}
impl Receivables {
    pub(super) async fn print_statement(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        data: &Value,
    ) -> Result<PdfFile> {
        let units = MinorUnits::new(data["minor_units"].as_i64().ok_or(Error::Invalid)?)
            .ok_or(Error::Invalid)?;
        let amount = |value: &Value| -> Result<String> {
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let value = text
                .parse::<rust_decimal::Decimal>()
                .map_err(|_| Error::Invalid)?;
            Ok(Money::display_decimal(value, units))
        };
        let customer = &data["customer"];
        let mut rows = Vec::new();
        for key in ["documents", "credits"] {
            for item in customer[key].as_array().ok_or(Error::Invalid)? {
                rows.push(StatementRow {
                    reference: format!(
                        "{}{}",
                        if key == "credits" { "Credit · " } else { "" },
                        item["document_reference"].as_str().unwrap_or("")
                    ),
                    date: bilibi_finance_ledger::dates::display_text(
                        item["issue_date"].as_str().unwrap_or(""),
                    ),
                    due: bilibi_finance_ledger::dates::display_text(
                        item["due_date"].as_str().unwrap_or(""),
                    ),
                    amount: format!(
                        "{}{}",
                        if key == "credits" { "−" } else { "" },
                        amount(&item["outstanding"])?
                    ),
                });
            }
        }
        let layout = self.get_setup(scope, company).await?.invoice_layout;
        let company_id =
            OrganisationId::new(company).map_err(|_| ReceivableError::CompanyNotFound)?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let company_name = if layout.company_name.is_empty() {
            self.parties
                .organisation_name(&**client, scope, company_id)
                .await?
                .map(|o| o.name)
                .unwrap_or_default()
        } else {
            layout.company_name.clone()
        };
        let template = StatementView {
            company_name,
            layout,
            customer: customer["name"].as_str().ok_or(Error::Invalid)?.into(),
            date: bilibi_finance_ledger::dates::display_text(
                data["as_of"].as_str().ok_or(Error::Invalid)?,
            ),
            currency: data["currency"].as_str().ok_or(Error::Invalid)?.into(),
            rows,
            total: amount(&customer["total"])?,
        };
        let owner = StatementOwner {
            html: template
                .render()
                .map_err(|_| ReceivableError::Pdf("template"))?,
        };
        let owned = self.artifacts.for_owner(scope, company_id, &owner);
        let artifact = Box::pin(owned.render_inline(
            &mut client,
            Reference {
                subject: format!("mail-{id}"),
                kind: "customer-statement".into(),
            },
            &json!({}),
        ))
        .await
        .map_err(|e| ReceivableError::Pdf(e.code()))?;
        let file = Box::pin(owned.read(&mut client, &artifact.id))
            .await
            .map_err(|e| ReceivableError::Pdf(e.code()))?;
        Ok(PdfFile {
            bytes: file.bytes,
            filename: "statement.pdf".into(),
        })
    }
}
