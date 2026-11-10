//! Delivering one queued invoice email (`email.rs` records the request): the
//! job runs as the person who asked, reads the setting, their permission and
//! the invoice again, makes the PDF and hands it to Base mail, then settles
//! the send `sent`, `failed` or `cancelled` in an audited write.
use crate::{
    authorize,
    email::{RECORD, WORKER},
    store, DocumentEmail, ReceivableError as R, Receivables, ISSUE, VIEW,
};
use bilibi_database::{Change as Audited, Error};
use bilibi_mail::{Address, Attachment, Message};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_tenancy::Scope;
use serde_json::Value;
use bilibi_ui::page::{Change, Subscription};
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

pub(crate) const JOB_SERVICE: &str = "finance/receivables.invoice-email";

/// Told once a send is settled, so an open invoice page reads again and its
/// History turns from Sending to Sent. Process-local, as the Platform's page
/// broadcasts are: a page served by another process shows it on its next read.
static SETTLED: OnceLock<broadcast::Sender<Change>> = OnceLock::new();
fn settled_sender() -> &'static broadcast::Sender<Change> {
    SETTLED.get_or_init(|| broadcast::channel(16).0)
}
/// A page's subscription to settled sends.
pub(crate) fn settled() -> Subscription {
    Subscription {
        receiver: settled_sender().subscribe(),
    }
}

fn arguments(args: Value) -> bilibi_database::Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    if object.len() != 1
        || object
            .get("email")
            .and_then(Value::as_i64)
            .is_none_or(|id| id <= 0)
    {
        return Err(Error::Invalid);
    }
    Ok(args)
}
macro_rules! provenance {
    () => {
        Provenance {
            module: crate::MODULE,
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 5, validate: arguments }) }
// Queue proves `VIEW` before each attempt; the job proves `ISSUE` itself, so
// a person who lost it gets a settled `cancelled` send, not a row left queued.
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: JOB_SERVICE, capability: VIEW }) }

/// The job that delivers one queued send.
pub(crate) struct SendInvoice {
    pub receivables: Arc<Receivables>,
}
impl Handler for SendInvoice {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            match self.run(execution, &args).await {
                Ok(true) => Outcome::Completed,
                Ok(false) => Outcome::Retry("invoice_email_not_delivered"),
                Err(error) => {
                    eprintln!("error: invoice email: {error}");
                    Outcome::Retry("invoice_email_failed")
                }
            }
        })
    }
}

/// How a delivery ended, for the row.
struct Settled {
    state: &'static str,
    reason: Option<&'static str>,
    bytes: Option<i64>,
    sha256: Option<String>,
    message_id: Option<String>,
}
impl Settled {
    fn stopped(state: &'static str, reason: &'static str) -> Self {
        Self {
            state,
            reason: Some(reason),
            bytes: None,
            sha256: None,
            message_id: None,
        }
    }
}

impl SendInvoice {
    /// `Ok(true)` once the send is settled, `Ok(false)` to try again.
    async fn run(&self, execution: &Execution, args: &Value) -> bilibi_database::Result<bool> {
        let scope = execution.scope().ok_or(Error::Unauthorized)?.clone();
        let request = args["email"].as_i64().ok_or(Error::Invalid)?;
        let owner = &self.receivables;
        let mut client = owner.db.get().await?;
        let Some(row) = client
            .query_opt(
                "SELECT to_jsonb(m), m.company_id FROM finance_receivables_document_emails m WHERE tenant_id=$1 AND id=$2 AND requested_by_user_id=$3 AND state='queued'",
                &[&scope.tenant(), &request, &scope.user()],
            )
            .await?
        else {
            // Already settled: a retry after the outcome was recorded.
            return Ok(true);
        };
        let email: DocumentEmail = store::decode(row.get(0))?;
        let company: i64 = row.get(1);
        let last = execution.attempt >= execution.max_attempts;
        let settled = match self.deliver(&scope, &client, company, &email).await {
            Ok(settled) => settled,
            Err(reason) if last => Settled::stopped("failed", reason),
            Err(_) => return Ok(false),
        };
        self.settle(execution, &mut client, request, settled)
            .await?;
        Ok(true)
    }

    /// Sends the mail. `Ok` is an outcome to record; `Err` is worth another
    /// attempt and carries the reason recorded when none is left.
    async fn deliver(
        &self,
        scope: &Scope,
        client: &tokio_postgres::Client,
        company: i64,
        email: &DocumentEmail,
    ) -> std::result::Result<Settled, &'static str> {
        let owner = &self.receivables;
        match authorize(client, scope, company, ISSUE).await {
            Ok(()) => {}
            Err(R::Unauthorized | R::CompanyNotFound) => {
                return Ok(Settled::stopped("cancelled", "sender_access_ended"))
            }
            Err(_) => return Err("delivery_failed"),
        }
        match owner.email_setup(client, scope, company).await {
            Ok(setup) if setup.invoice_email.enabled => {}
            Ok(_) | Err(R::NotConfigured) => {
                return Ok(Settled::stopped("cancelled", "turned_off"))
            }
            Err(_) => return Err("delivery_failed"),
        }
        let pdf = match Box::pin(owner.print_document(scope, company, email.document_id)).await {
            Ok(pdf) => pdf,
            Err(R::Unauthorized) => {
                return Ok(Settled::stopped("cancelled", "sender_access_ended"))
            }
            Err(R::NotIssued | R::DocumentNotFound) => {
                return Ok(Settled::stopped("cancelled", "not_issued"))
            }
            Err(error) => {
                eprintln!("error: invoice email: the PDF could not be made: {error}");
                return Err("pdf_failed");
            }
        };
        let sha256: String = client
            .query_one("SELECT encode(sha256($1::bytea),'hex')", &[&pdf.bytes])
            .await
            .map_err(|_| "delivery_failed")?
            .get(0);
        let bytes = i64::try_from(pdf.bytes.len()).map_err(|_| "pdf_failed")?;
        let Ok(to) = Address::new(None, &email.to_email) else {
            return Ok(Settled::stopped("failed", "address_refused"));
        };
        let mut message = Message::text(to, email.subject.clone(), email.message.clone());
        message.attachments = vec![Attachment {
            filename: email.attachment_name.clone(),
            content_type: "application/pdf".into(),
            bytes: pdf.bytes,
        }];
        message.reply_to = email
            .reply_to
            .as_deref()
            .and_then(|reply| Address::new(None, reply).ok());
        // Mail goes from the installation's own address under the company's
        // name; the first rendering is only to learn that address.
        let mailer = &owner.mailer;
        let Ok(rendered) = mailer.render(&message).await else {
            return Ok(Settled::stopped("failed", "message_refused"));
        };
        message.from = Address::new(Some(&email.from_name), &rendered.envelope.from).ok();
        match mailer.deliver(&message).await {
            Ok(sent) => Ok(Settled {
                state: "sent",
                reason: None,
                bytes: Some(bytes),
                sha256: Some(sha256),
                message_id: Some(sent.message_id),
            }),
            Err(bilibi_mail::Error::Invalid) => Ok(Settled::stopped("failed", "message_refused")),
            Err(_) => Err("delivery_failed"),
        }
    }

    /// Records the outcome, audited. The queue's own fenced transaction: the
    /// row changes only while this attempt still holds the job.
    async fn settle(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        request: i64,
        settled: Settled,
    ) -> bilibi_database::Result<()> {
        let tenant = execution.tenant();
        let mut tx = execution
            .begin(client, "finance.receivables.invoice_email")
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let Some(old) = sql
                    .query_opt(
                        "SELECT to_jsonb(m) FROM finance_receivables_document_emails m WHERE tenant_id=$1 AND id=$2 AND state='queued' FOR UPDATE",
                        &[&tenant, &request],
                    )
                    .await?
                else {
                    return Ok(((), Vec::new()));
                };
                let new: Value = sql
                    .query_one(
                        "UPDATE finance_receivables_document_emails m SET state=$3, reason=$4, attachment_bytes=$5, attachment_sha256=$6, message_id=$7, settled_at=now() AT TIME ZONE 'UTC' WHERE tenant_id=$1 AND id=$2 RETURNING to_jsonb(m)",
                        &[
                            &tenant,
                            &request,
                            &settled.state,
                            &settled.reason,
                            &settled.bytes,
                            &settled.sha256,
                            &settled.message_id,
                        ],
                    )
                    .await?
                    .get(0);
                Ok((
                    (),
                    vec![Audited {
                        kind: RECORD.into(),
                        id: request,
                        old: old.get(0),
                        new,
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        let _ = settled_sender().send(Change { tenant, user: None });
        Ok(())
    }
}
