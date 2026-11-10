//! Emailing an issued invoice to its customer, with the PDF attached. Mail
//! leaves Bilibi, so a send is never fire-and-forget:
//!
//! 1. **Request** (the caller's own transaction, under `ISSUE`): the company
//!    must have invoice email turned on, the document must be an issued
//!    invoice and the address must be one mail can go to. One row in
//!    `finance_receivables_document_emails` records who asked, to whom and
//!    with which words, as `queued`, and a durable job is queued with it.
//! 2. **Delivery** (the job, as the person who asked): the setting, the
//!    person's permission and the invoice are read again; the PDF is made
//!    (`print_document`) and handed to Base mail (`base/mail.mailer`).
//! 3. **Outcome** (an audited write): `sent` with the PDF's size and SHA-256
//!    and the message id, `failed`, or `cancelled` with a reason code. A
//!    crash between sending and settling sends once more.
//!
//! Statements and reminders are not built here.
use crate::{
    authorize, documents, invalid,
    posting::PreparedIssue,
    store::{self, CUSTOMERS, SETUPS},
    Document, DocumentEmail, InvoiceEmail, ReceivableError as R, Receivables, Result, Setup, ISSUE,
    VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_finance_ledger::dates;
use bilibi_mail::{Address, Attachment, Message};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

/// The audit kind of one send.
const RECORD: &str = "finance.receivables.document_email";
const WORKER: &str = "finance/receivables-invoice-email";
pub(crate) const JOB_SERVICE: &str = "finance/receivables.invoice-email";
/// The most times one invoice is emailed in a day.
const DAILY_LIMIT: i64 = 5;

/// The words Bilibi fills in, in a subject or a message.
pub(crate) const WORDS: [&str; 5] = ["number", "company", "customer", "total", "due_date"];
/// The wording used while the company has not written its own.
pub(crate) const DEFAULT_SUBJECT: &str = "Invoice {number} from {company}";
pub(crate) const DEFAULT_MESSAGE: &str = "Hello {customer},\n\nInvoice {number} for {total} is attached. It is due on {due_date}.\n\nThank you,\n{company}";
pub(crate) const TURNED_OFF: &str =
    "Invoice email is turned off for this company. Turn it on in Receivables settings.";

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

/// The first `{word}` in `text` that Bilibi cannot fill in.
fn unknown_word(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else {
            return None;
        };
        let word = &rest[..end];
        if !word.is_empty()
            && word.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            && !WORDS.contains(&word)
        {
            return Some(word);
        }
    }
    None
}

fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = template.to_owned();
    for (word, value) in values {
        text = text.replace(&format!("{{{word}}}"), value);
    }
    text
}

fn one_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn address(field: &str, email: &str) -> Result<String> {
    Address::new(None, email)
        .map(|a| a.email)
        .map_err(|_| invalid(field, "is not an email address"))
}

/// The company's invoice email choice after `patch`, checked. An omitted key
/// keeps its value.
pub(crate) fn settings(old: Option<&Value>, patch: Option<&Value>) -> Result<Value> {
    let mut value = old
        .filter(|old| old.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(patch) = patch else {
        return Ok(value);
    };
    for (key, field) in patch
        .as_object()
        .ok_or_else(|| invalid("invoice_email", "is invalid"))?
    {
        value[key] = field.clone();
    }
    let mut email: InvoiceEmail =
        serde_json::from_value(value).map_err(|_| invalid("invoice_email", "is invalid"))?;
    email.from_name = email.from_name.trim().to_owned();
    email.reply_to = email.reply_to.trim().to_owned();
    email.subject = email.subject.trim().to_owned();
    email.message = email.message.replace("\r\n", "\n").trim().to_owned();
    if email.from_name.len() > 200 || !one_line(&email.from_name) {
        return Err(invalid(
            "invoice_email.from_name",
            "must be one line of at most 200 characters",
        ));
    }
    if !email.reply_to.is_empty() {
        email.reply_to = address("invoice_email.reply_to", &email.reply_to)?;
    }
    if email.subject.chars().count() > 150 || !one_line(&email.subject) {
        return Err(invalid(
            "invoice_email.subject",
            "must be one line of at most 150 characters",
        ));
    }
    if email.message.chars().count() > 4000
        || email
            .message
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(invalid(
            "invoice_email.message",
            "must be text of at most 4000 characters",
        ));
    }
    for (field, text) in [("subject", &email.subject), ("message", &email.message)] {
        if let Some(word) = unknown_word(text) {
            return Err(invalid(
                &format!("invoice_email.{field}"),
                format!(
                    "has {{{word}}}, which Bilibi cannot fill in. Use {{number}}, {{company}}, {{customer}}, {{total}} or {{due_date}}"
                ),
            ));
        }
    }
    Ok(serde_json::to_value(email).map_err(|_| Error::Internal)?)
}

/// One email, ready to queue: every word is final.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Composed {
    pub to: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub subject: String,
    pub message: String,
    pub attachment: String,
}

/// What an email says about its invoice.
struct Facts<'a> {
    reference: &'a str,
    customer: &'a str,
    total: String,
    due_date: Option<&'a str>,
    /// The address mail goes to unless the caller names another.
    to: Option<String>,
    /// Why there is no such address, said to the person.
    no_address: &'static str,
}

impl Receivables {
    /// Emails an issued invoice to its customer with the PDF attached
    /// (`ISSUE`). `attrs` may carry `to`, `subject` and `message`; what is
    /// left out comes from the customer and the company's invoice email
    /// settings. The send is queued: read the document's `emails` for how it
    /// ended.
    pub async fn send_invoice(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<DocumentEmail> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                ISSUE,
                "finance.receivables.send_invoice",
            )
            .await?;
        let composed = self
            .prepare_email(tx.sql(), scope, company, id, attrs)
            .await?;
        let email = self
            .send_email_on(&mut tx, scope, company, id, composed)
            .await?;
        tx.commit().await?;
        Ok(email)
    }

    async fn email_setup(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
    ) -> Result<Setup> {
        Ok(store::decode(
            store::find(client, SETUPS, scope.tenant(), company, None)
                .await?
                .ok_or(R::NotConfigured)?,
        )?)
    }

    /// The email for `facts`, with the caller's own `to`, `subject` and
    /// `message` where given.
    async fn compose(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        setup: &Setup,
        facts: Facts<'_>,
        input: &Value,
    ) -> Result<Composed> {
        let settings = &setup.invoice_email;
        let company_name = if setup.invoice_layout.company_name.is_empty() {
            self.parties
                .organisation_name(
                    client,
                    scope,
                    OrganisationId::new(company).map_err(|_| R::CompanyNotFound)?,
                )
                .await?
                .map(|o| o.name)
                .unwrap_or_default()
        } else {
            setup.invoice_layout.company_name.clone()
        };
        let text = |key: &str| {
            input[key]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
        };
        let to = match text("to") {
            Some(typed) => address("to", typed)?,
            None => address(
                "to",
                &facts.to.ok_or_else(|| invalid("to", facts.no_address))?,
            )?,
        };
        let due = facts.due_date.map(dates::display_text).unwrap_or_default();
        let values = [
            ("number", facts.reference),
            ("company", company_name.as_str()),
            ("customer", facts.customer),
            ("total", facts.total.as_str()),
            ("due_date", due.as_str()),
        ];
        let or_default = |own: &str, built_in: &str| {
            if own.is_empty() {
                built_in.to_owned()
            } else {
                own.to_owned()
            }
        };
        let subject = match text("subject") {
            Some(typed) => typed.to_owned(),
            None => fill(&or_default(&settings.subject, DEFAULT_SUBJECT), &values),
        };
        if subject.len() > 200 || !one_line(&subject) {
            return Err(invalid(
                "subject",
                "must be one line of at most 200 characters",
            ));
        }
        let message = match input["message"].as_str().map(str::trim) {
            Some(typed) if !typed.is_empty() => typed.replace("\r\n", "\n"),
            _ => fill(&or_default(&settings.message, DEFAULT_MESSAGE), &values),
        };
        if message.chars().count() > 5000
            || message
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(invalid(
                "message",
                "must be text of at most 5000 characters",
            ));
        }
        let from_name = or_default(&settings.from_name, &company_name);
        if from_name.is_empty() || from_name.len() > 200 || !one_line(&from_name) {
            return Err(invalid(
                "base",
                "Set a From name for invoice email in Receivables settings.",
            ));
        }
        Ok(Composed {
            to,
            from_name,
            reply_to: Some(settings.reply_to.clone()).filter(|r| !r.is_empty()),
            subject,
            message,
            attachment: format!("invoice-{}.pdf", facts.reference),
        })
    }

    /// The address a customer's invoices go to as this reader may see it:
    /// the linked party's current email, else the one frozen at issue.
    async fn recipient(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        document: &Document,
    ) -> Result<(Option<String>, &'static str)> {
        let frozen = document.party_email.clone().filter(|e| !e.is_empty());
        let row = store::find(
            client,
            CUSTOMERS,
            scope.tenant(),
            company,
            Some(document.customer_id),
        )
        .await?
        .ok_or(R::CustomerNotFound)?;
        let party = self.customer(client, scope, row).await?.party;
        let hidden = party.restricted.iter().any(|field| field == "email");
        Ok((
            party.email.filter(|e| !e.is_empty()).or(frozen),
            if hidden {
                "You cannot see this customer's email address. Type the address to send to."
            } else {
                "This customer has no email address. Add one to the customer, or type the address to send to."
            },
        ))
    }

    /// The email an issued invoice would go out with, without the lock or
    /// the limits of a send: what a dialog starts from.
    async fn compose_issued(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
        capability: &str,
        input: &Value,
    ) -> Result<Composed> {
        let setup = self.email_setup(client, scope, company).await?;
        if !setup.invoice_email.enabled {
            return Err(invalid("base", TURNED_OFF));
        }
        let document = documents::read(client, scope.tenant(), company, id).await?;
        if document.kind != "invoice" {
            return Err(invalid("base", "Only an invoice can be sent by email."));
        }
        if document.status != "issued" {
            return Err(invalid(
                "base",
                "Only an issued invoice can be sent. Issue this draft first.",
            ));
        }
        if document.opening.is_some() {
            return Err(invalid(
                "base",
                "An opening invoice has no PDF to send. Send the invoice from the system it was issued in.",
            ));
        }
        let book = self
            .ledger
            .book_figures_on(client, scope, company, capability)
            .await?;
        let (to, no_address) = self.recipient(client, scope, company, &document).await?;
        self.compose(
            client,
            scope,
            company,
            &setup,
            Facts {
                reference: document.document_reference.as_deref().unwrap_or_default(),
                customer: document.party_name.as_deref().unwrap_or_default(),
                total: format!(
                    "{} {}",
                    document.total.display(book.minor_units),
                    book.currency_code
                ),
                due_date: document.due_date.as_deref(),
                to,
                no_address,
            },
            input,
        )
        .await
    }

    /// What the send dialog and an agent start from (`VIEW`): whether invoice
    /// email is on, the email as it would go out, or why it cannot.
    pub(crate) async fn email_draft(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Value> {
        self.authorize_on(client, scope, company, VIEW).await?;
        let enabled = store::find(client, SETUPS, scope.tenant(), company, None)
            .await?
            .map(store::decode::<Setup>)
            .transpose()?
            .is_some_and(|setup| setup.invoice_email.enabled);
        // The document must exist for this reader whatever the setting says.
        documents::read(client, scope.tenant(), company, id).await?;
        Ok(
            match self
                .compose_issued(client, scope, company, id, VIEW, &json!({}))
                .await
            {
                Ok(email) => json!({"enabled": enabled, "email": email, "refusal": null}),
                // No address yet: the rest of the email is still worth showing.
                Err(R::Invalid(errors)) if errors.iter().all(|e| e.field == "to") => {
                    let rest = self
                        .compose_issued(
                            client,
                            scope,
                            company,
                            id,
                            VIEW,
                            &json!({"to": "nobody@example.invalid"}),
                        )
                        .await
                        .ok()
                        .map(|mut email| {
                            email.to.clear();
                            email
                        });
                    json!({"enabled": enabled, "email": rest, "refusal": null, "no_address": errors[0].message})
                }
                Err(R::Invalid(errors)) => {
                    json!({"enabled": enabled, "email": null, "refusal": errors[0].message})
                }
                Err(R::NotConfigured) => {
                    json!({"enabled": false, "email": null, "refusal": TURNED_OFF})
                }
                Err(error) => return Err(error),
            },
        )
    }

    /// Everything a send needs, decided before anything is written.
    pub(crate) async fn prepare_email(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
        input: &Value,
    ) -> Result<Composed> {
        self.join(client, scope, company, ISSUE).await?;
        sender(scope)?;
        let composed = self
            .compose_issued(client, scope, company, id, ISSUE, input)
            .await?;
        let counts = client
            .query_opt(
                "SELECT count(*) FILTER (WHERE state='queued' AND to_email=$4 AND requested_at > (now() AT TIME ZONE 'UTC') - interval '1 hour'),
                        count(*) FILTER (WHERE state IN ('queued','sent') AND requested_at > (now() AT TIME ZONE 'UTC') - interval '1 day')
                 FROM finance_receivables_document_emails WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3",
                &[&scope.tenant(), &company, &id, &composed.to],
            )
            .await?
            .ok_or(Error::Internal)?;
        if counts.get::<_, i64>(0) > 0 {
            return Err(invalid(
                "base",
                format!("This invoice is already on its way to {}.", composed.to),
            ));
        }
        if counts.get::<_, i64>(1) >= DAILY_LIMIT {
            return Err(invalid(
                "base",
                format!(
                    "This invoice was emailed {DAILY_LIMIT} times in the last day. Try again tomorrow."
                ),
            ));
        }
        Ok(composed)
    }

    /// The email a draft's issue would send, when the company offers Issue
    /// and send for invoices: `None` when it does not. `Err` says why this
    /// one cannot be sent (no address), for the caller to show or refuse on.
    pub(crate) async fn issue_email(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
        prepared: &PreparedIssue,
    ) -> Result<Option<Result<Composed>>> {
        let setup = self.email_setup(client, scope, company).await?;
        let after = &prepared.after;
        if !setup.invoice_email.enabled
            || !setup.invoice_email.issue_and_send
            || after["kind"] != "invoice"
        {
            return Ok(None);
        }
        if let Err(refusal) = sender(scope) {
            return Ok(Some(Err(refusal)));
        }
        let draft = documents::read(client, scope.tenant(), company, id).await?;
        let book = self
            .ledger
            .book_figures_on(client, scope, company, ISSUE)
            .await?;
        let composed = self
            .compose(
                client,
                scope,
                company,
                &setup,
                Facts {
                    reference: after["document_reference"].as_str().unwrap_or_default(),
                    customer: after["party_name"].as_str().unwrap_or_default(),
                    total: format!(
                        "{} {}",
                        draft.total.display(book.minor_units),
                        book.currency_code
                    ),
                    due_date: after["due_date"].as_str(),
                    to: after["party_email"]
                        .as_str()
                        .filter(|e| !e.is_empty())
                        .map(str::to_owned),
                    no_address: "This customer has no email address, so the invoice cannot be emailed from here. Add one to the customer first, or issue without sending.",
                },
                &json!({}),
            )
            .await;
        Ok(Some(match composed {
            Ok(composed) => Ok(composed),
            Err(R::Invalid(errors)) => Err(R::Invalid(errors)),
            Err(other) => return Err(other),
        }))
    }

    /// Records the send and queues its delivery on the caller's transaction.
    pub(crate) async fn send_email_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
        composed: Composed,
    ) -> Result<DocumentEmail> {
        self.join(tx.sql(), scope, company, ISSUE).await?;
        sender(scope)?;
        let (tenant, actor) = (scope.tenant(), scope.user());
        let row = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row: Value = sql
                        .query_one(
                            "INSERT INTO finance_receivables_document_emails AS m (tenant_id,company_id,document_id,to_email,from_name,reply_to,subject,message,attachment_name,requested_by_user_id)
                             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING to_jsonb(m)",
                            &[
                                &tenant,
                                &company,
                                &id,
                                &composed.to,
                                &composed.from_name,
                                &composed.reply_to,
                                &composed.subject,
                                &composed.message,
                                &composed.attachment,
                                &actor,
                            ],
                        )
                        .await?
                        .get(0);
                    let change = Change {
                        kind: RECORD.into(),
                        id: row["id"].as_i64().ok_or(Error::Internal)?,
                        old: Value::Null,
                        new: row.clone(),
                    };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        let email: DocumentEmail = store::decode(row)?;
        self.queue
            .enqueue_for(tx, scope, WORKER, json!({"email": email.id}))
            .await?;
        Ok(email)
    }
}

/// Queue runs a job as the person who asked, so someone acting as another
/// person cannot ask: said plainly here, before anything is written.
fn sender(scope: &Scope) -> Result<()> {
    if scope.impersonator().is_some() || scope.principal().is_some() {
        return Err(invalid(
            "base",
            "Email is sent in your own name. Stop acting as someone else, then send it.",
        ));
    }
    Ok(())
}

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
                    vec![Change {
                        kind: RECORD.into(),
                        id: request,
                        old: old.get(0),
                        new,
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }
}
