//! Queued delivery of a reviewed invoice email.
use crate::{
    documents, invalid, store, ReceivableError, Receivables, Result, MANAGE_DOCUMENTS, MODULE,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Ended, Ending, EndingBinding, EndingFuture, Execution, Handler, HandlerBinding, HandlerFuture,
    Outcome, Worker, ENDINGS, HANDLERS, WORKERS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

const WORKER: &str = "finance/receivables-send-invoice";
pub(crate) const PRINCIPAL: &str = "finance-receivables-send-invoice";
pub(crate) const MONITOR: &str = "finance-receivables-mail-monitor";
pub(crate) const MONITOR_CAPABILITY: &str = "finance.receivables.mail.record-failure";
pub(crate) const ENDING_SERVICE: &str = "finance/receivables.mail_ended";
pub(crate) const SERVICE: &str = "finance/receivables.send_invoice";
const MAIL: store::Table = store::Table {
    name: "finance_receivables_mail",
    kind: "finance.receivables.mail",
    columns: &["document_id", "recipient", "subject", "body", "status"],
    timestamps: true,
};
fn args(value: Value) -> bilibi_database::Result<Value> {
    if value.as_object().is_some_and(|v| v.len() == 2)
        && value["company_id"].as_i64().is_some_and(|v| v > 0)
        && value["mail_id"].as_i64().is_some_and(|v| v > 0)
    {
        Ok(value)
    } else {
        Err(Error::Invalid)
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, Provenance { module: MODULE, source: file!(), line: line!() }, &Worker { max_attempts: 5, validate: args }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, Provenance { module: MODULE, source: file!(), line: line!() }, &HandlerBinding { service: SERVICE, capability: MANAGE_DOCUMENTS }) }
inventory::submit! { Contribution::new(ENDINGS, WORKER, Provenance { module: MODULE, source: file!(), line: line!() }, &EndingBinding { service: ENDING_SERVICE, principal: MONITOR, capability: MONITOR_CAPABILITY }) }
inventory::submit! { Contribution::new(bilibi_authz::STANDING_GRANTS, MONITOR, Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_authz::StandingGrant { principal: MONITOR, capabilities: &[MONITOR_CAPABILITY] }) }

pub(crate) struct MailEnded {
    pub receivables: Arc<Receivables>,
}
impl Ending for MailEnded {
    fn ended<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        job: &'a Ended,
    ) -> EndingFuture<'a> {
        Box::pin(async move {
            let Some(company) = job.args["company_id"].as_i64() else {
                return Ok(());
            };
            let Some(id) = job.args["mail_id"].as_i64() else {
                return Ok(());
            };
            self.receivables
                .join(tx.sql(), scope, company, MONITOR_CAPABILITY)
                .await
                .map_err(|_| Error::Forbidden)?;
            let Some(old) = store::find(tx.sql(), MAIL, scope.tenant(), company, Some(id)).await?
            else {
                return Ok(());
            };
            if old["status"] != "queued" {
                return Ok(());
            }
            let tenant = scope.tenant();
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let mut data = old.clone();
                    data["status"] = json!("failed");
                    let mut changes = Vec::new();
                    store::save(sql, MAIL, tenant, company, Some(old), data, &mut changes).await?;
                    Ok(((), changes))
                })
            })
            .await
        })
    }
}

impl Receivables {
    pub(super) async fn review_mail(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        let id = input["document_id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or(ReceivableError::InvalidOption)?;
        self.authorize_on(client, scope, company, crate::VIEW)
            .await?;
        let document = documents::read(client, scope.tenant(), company, id).await?;
        if document.status != "issued" || document.kind != "invoice" || document.opening.is_some() {
            return Err(ReceivableError::NotIssued);
        }
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
        Ok(
            json!({"document_id":id,"reference":document.document_reference,"recipient":recipient,"subject":subject,"body":body,"attachment":"Invoice PDF"}),
        )
    }
    pub(super) async fn queue_mail_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        self.join(tx.sql(), scope, company, MANAGE_DOCUMENTS)
            .await?;
        let review = self.review_mail(tx.sql(), scope, company, input).await?;
        let tenant = scope.tenant();
        let system =
            bilibi_authz::SystemScope::rehydrate(tx.sql(), tenant, Some(PRINCIPAL)).await?;
        for capability in [MANAGE_DOCUMENTS, crate::VIEW] {
            bilibi_authz::authorize_system(tx.sql(), &system, capability)
                .await
                .map_err(|_| invalid("base", "Ask an administrator to enable invoice email."))?;
        }
        let data = json!({"document_id":review["document_id"],"recipient":review["recipient"],"subject":review["subject"],"body":review["body"],"status":"queued"});
        let row = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    let row =
                        store::save(sql, MAIL, tenant, company, None, data, &mut changes).await?;
                    Ok((row, changes))
                })
            })
            .await?;
        self.queue
            .enqueue_system_within(
                tx,
                &system,
                WORKER,
                json!({"company_id":company,"mail_id":row["id"]}),
            )
            .await?;
        Ok(row)
    }
}

pub(crate) struct SendInvoice {
    pub receivables: Arc<Receivables>,
    pub mailer: Arc<bilibi_mail::Mailer>,
}
impl Handler for SendInvoice {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            match self.run(execution, args).await {
                Ok(()) => Outcome::Completed,
                Err(ReceivableError::Unauthorized) => Outcome::Cancel("actor_refused"),
                Err(error) => {
                    eprintln!("Invoice email delivery failed: {error}");
                    Outcome::Retry("invoice_mail_failed")
                }
            }
        })
    }
}
impl SendInvoice {
    async fn run(&self, execution: &Execution, args: Value) -> Result<()> {
        let scope = execution.scope().ok_or(ReceivableError::Unauthorized)?;
        if scope.principal() != Some(PRINCIPAL) {
            return Err(ReceivableError::Unauthorized);
        }
        let company = args["company_id"]
            .as_i64()
            .ok_or(ReceivableError::InvalidOption)?;
        let id = args["mail_id"]
            .as_i64()
            .ok_or(ReceivableError::InvalidOption)?;
        let mut client = self.receivables.db.get().await.map_err(Error::from)?;
        let tx = execution
            .begin(&mut client, "finance.receivables.send_invoice_check")
            .await?;
        self.receivables
            .join(tx.sql(), scope, company, MANAGE_DOCUMENTS)
            .await?;
        let row = store::find(tx.sql(), MAIL, scope.tenant(), company, Some(id))
            .await?
            .ok_or(Error::NotFound)?;
        if row["status"] != "queued" {
            tx.commit().await?;
            return Ok(());
        }
        tx.commit().await?;
        let document_id = row["document_id"].as_i64().ok_or(Error::Invalid)?;
        let pdf = self
            .receivables
            .print_document(scope, company, document_id)
            .await?;
        let address =
            bilibi_mail::Address::new(None, row["recipient"].as_str().ok_or(Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
        let mut message = bilibi_mail::Message::text(
            address,
            row["subject"].as_str().ok_or(Error::Invalid)?,
            row["body"].as_str().ok_or(Error::Invalid)?,
        );
        message.attachments.push(bilibi_mail::Attachment {
            filename: pdf.filename,
            content_type: "application/pdf".into(),
            bytes: pdf.bytes,
        });
        // Claim the external effect before SMTP: a crash after delivery must not
        // cause Queue to send the same message again. A stranded attempt is
        // shown as uncertain so a person can check before sending again.
        let mut client = self.receivables.db.get().await.map_err(Error::from)?;
        let mut tx = execution
            .begin(&mut client, "finance.receivables.invoice_sending")
            .await?;
        self.receivables
            .join(tx.sql(), scope, company, MANAGE_DOCUMENTS)
            .await?;
        let old = store::find(tx.sql(), MAIL, scope.tenant(), company, Some(id))
            .await?
            .ok_or(Error::NotFound)?;
        if old["status"] != "queued" {
            tx.commit().await?;
            return Ok(());
        }
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut data = old.clone();
                data["status"] = json!("sending");
                let mut changes = Vec::new();
                store::save(sql, MAIL, tenant, company, Some(old), data, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        tx.commit().await?;
        if self.mailer.deliver(&message).await.is_err() {
            return Ok(());
        }
        let mut client = self.receivables.db.get().await.map_err(Error::from)?;
        let mut tx = execution
            .begin(&mut client, "finance.receivables.invoice_sent")
            .await?;
        self.receivables
            .join(tx.sql(), scope, company, MANAGE_DOCUMENTS)
            .await?;
        let old = store::find(tx.sql(), MAIL, scope.tenant(), company, Some(id))
            .await?
            .ok_or(Error::NotFound)?;
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut data = old.clone();
                data["status"] = json!("sent");
                let mut changes = Vec::new();
                store::save(sql, MAIL, tenant, company, Some(old), data, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
