//! Reviewed customer statements and reminders, delivered through the shared mail worker.
use crate::{
    agent_operations, invalid, AgingOptions, PdfFile, ReceivableError, Receivables, Result,
    MANAGE_DOCUMENTS, MODULE, VIEW,
};
use askama::Template;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, WriteHandler,
    DEFINITIONS,
};
use bilibi_artifacts::{HtmlFuture, HtmlOwner, Operation as FileOperation, Owner, Reference};
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use tokio_postgres::Transaction;

struct SendStatement;
fn input() -> Value {
    json!({"type":"object","required":["company_id","customer_id","as_of","recipient","subject","body"],"properties":{
        "company_id":{"type":"integer","minimum":1},"customer_id":{"type":"integer","minimum":1},"as_of":{"type":"string","format":"date"},
        "recipient":{"type":"string","maxLength":254},"subject":{"type":"string","maxLength":200},"body":{"type":"string","maxLength":16000}}})
}
fn output() -> Value {
    json!({"type":"object"})
}
inventory::submit! { Contribution::new(DEFINITIONS,"finance/receivables.statement_operations", Provenance {module:MODULE,source:file!(),line:line!()}, &Definitions { operations:&[Operation {
    key:"finance.receivables.statement.send",title:"Send a customer statement",summary:"Review a customer's open invoices and credits at the chosen date, then queue a statement PDF with the email.",keywords:&["reminder","chase"],capability:MANAGE_DOCUMENTS,input,output,handler:Handler::SafeWrite(&SendStatement),channels:Channels::Every,step_up:&[]
}], guides:&[] }) }

async fn review(
    service: &Receivables,
    tx: &Transaction<'_>,
    scope: &Scope,
    input: &Value,
) -> Result<Value> {
    let company = input["company_id"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(ReceivableError::InvalidOption)?;
    let customer = input["customer_id"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(ReceivableError::InvalidOption)?;
    service.join(tx, scope, company, MANAGE_DOCUMENTS).await?;
    let as_of = input["as_of"]
        .as_str()
        .ok_or(ReceivableError::InvalidOption)?
        .parse()
        .map_err(|_| invalid("as_of", "Choose a valid date."))?;
    let recipient = input["recipient"].as_str().unwrap_or("").trim();
    bilibi_mail::Address::new(None, recipient)
        .map_err(|_| invalid("recipient", "Enter a valid email address."))?;
    let subject = input["subject"].as_str().unwrap_or("").trim();
    if subject.is_empty() || subject.len() > 200 || subject.chars().any(char::is_control) {
        return Err(invalid(
            "subject",
            "Enter a subject of up to 200 characters.",
        ));
    }
    let body = input["body"].as_str().unwrap_or("").trim();
    if body.is_empty() || body.len() > 16000 {
        return Err(invalid(
            "body",
            "Enter a message of up to 16,000 characters.",
        ));
    }
    let report = service
        .aged_receivables_on(tx, scope, company, as_of, &AgingOptions::default())
        .await?;
    let party = report
        .customers
        .iter()
        .find(|c| c.customer_id == customer)
        .ok_or_else(|| {
            invalid(
                "customer_id",
                "This customer has no open balance on that date.",
            )
        })?;
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
    source = r#"<!doctype html><html><head><meta charset="utf-8"><style>body{font:14px sans-serif;color:#222}table{border-collapse:collapse;width:100%}th,td{padding:8px;border-bottom:1px solid #ddd;text-align:left}th:last-child,td:last-child{text-align:right}</style></head><body><h1>Customer statement</h1><p>{{ customer }} · {{ date }}</p><p>Amounts in {{ currency }}</p><table><thead><tr><th>Document</th><th>Issued</th><th>Due</th><th>Outstanding</th></tr></thead><tbody>{% for row in rows %}<tr><td>{{ row.reference }}</td><td>{{ row.date }}</td><td>{{ row.due }}</td><td>{{ row.amount }}</td></tr>{% endfor %}</tbody></table><p>Total owed: {{ total }} {{ currency }}</p></body></html>"#,
    ext = "html"
)]
struct StatementView {
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
        let template = StatementView {
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
        let company_id =
            OrganisationId::new(company).map_err(|_| ReceivableError::CompanyNotFound)?;
        let mut client = self.db.get().await.map_err(Error::from)?;
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
