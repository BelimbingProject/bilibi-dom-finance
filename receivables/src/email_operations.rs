//! The one reviewed door for emailing an invoice: the invoice page's Send
//! dialog and an agent both call `finance.receivables.document.send`, and
//! both start from `finance.receivables.document.email`.
use crate::{agent_operations::failure, Receivables, ISSUE, MODULE, VIEW};
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

static OWNER: OnceLock<Arc<Receivables>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Receivables>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate receivables email binding".into()))
}
fn owner() -> bilibi_agent_api::Result<&'static Receivables> {
    OWNER.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
fn id(input: &Value, key: &str) -> bilibi_agent_api::Result<i64> {
    input[key]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(Error::Invalid.into())
}
const SUMMARY: &str = "Email this issued invoice to the customer with its PDF attached. The send is queued and recorded on the invoice; mail that has left cannot be taken back.";

struct Draft;
impl ReadHandler for Draft {
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
            Ok(owner()?
                .email_draft(
                    client,
                    scope,
                    id(input, "company_id")?,
                    id(input, "document_id")?,
                )
                .await
                .map_err(|e| failure(e, "lines"))?
                .into())
        })
    }
}

struct Send;
impl Send {
    async fn review(
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
    ) -> bilibi_agent_api::Result<(Value, crate::email::Composed)> {
        let (company, document) = (id(input, "company_id")?, id(input, "document_id")?);
        let email = owner()?
            .prepare_email(client, scope, company, document, input)
            .await
            .map_err(|e| failure(e, "lines"))?;
        Ok((
            json!({"company_id": company, "summary": SUMMARY, "changes": {"document_id": document, "email": email}}),
            email,
        ))
    }
}
impl WriteHandler for Send {
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
            let (review, _) = Self::review(client, scope, input).await?;
            crate::agent_operations::fence(
                client,
                &json!({"action":"send_invoice","input":input,"review":review}),
                false,
            )
            .await?;
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
            let (company, document) = (id(input, "company_id")?, id(input, "document_id")?);
            let (review, email) = Self::review(tx.sql(), scope, input).await?;
            crate::agent_operations::fence(
                tx.sql(),
                &json!({"action":"send_invoice","input":input,"review":review}),
                true,
            )
            .await?;
            let record = owner()?
                .send_email_on(tx, scope, company, document, email)
                .await
                .map_err(|e| failure(e, "lines"))?;
            Ok(json!({"record":record,"path":format!("{}/{document}", crate::web::INVOICES)}).into())
        })
    }
}

fn target() -> Value {
    json!({"type":"object","required":["company_id","document_id"],"properties":{"company_id":{"type":"integer","minimum":1},"document_id":{"type":"integer","minimum":1}}})
}
fn send_input() -> Value {
    json!({"type":"object","required":["company_id","document_id"],"properties":{
        "company_id":{"type":"integer","minimum":1},
        "document_id":{"type":"integer","minimum":1},
        "to":{"type":"string","maxLength":254,"description":"The address to send to. Left out, the customer's email address."},
        "subject":{"type":"string","maxLength":200,"description":"Left out, the company's subject with the invoice's details filled in."},
        "message":{"type":"string","maxLength":5000,"description":"Left out, the company's message with the invoice's details filled in."}
    }})
}
fn output() -> Value {
    json!({"type":"object","description":"The email as it would go out (to, from_name, reply_to, subject, message, attachment), or the recorded send. A send is queued: read the document's emails for its state (queued, sent, failed, cancelled)."})
}

inventory::submit! { Contribution::new(DEFINITIONS,"finance/receivables.email_operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {
    operations:&[
        Operation {key:"finance.receivables.document.email",title:"Read an invoice's email",summary:"Read whether this company emails invoices, and the email an issued invoice would go out with: recipient, sender name, subject, message and the PDF's name. A refusal or a missing address is said in words.",keywords:&["send","mail"],capability:VIEW,input:target,output,handler:Handler::Read(&Draft),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.send",title:"Email an invoice",summary:"Email an issued invoice to the customer with its PDF attached, when the company has invoice email turned on. Refused for a draft, a credit note, or a customer with no address unless one is given. Each send is recorded on the invoice.",keywords:&["send","mail","email"],capability:ISSUE,input:send_input,output,handler:Handler::SafeWrite(&Send),channels:Channels::Every,step_up:&[]},
    ],guides:&[],
}) }
