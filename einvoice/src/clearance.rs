//! Issuing a document and clearing it with the authority (Bilimbi
//! `Einvoice.issue_document/3`, `submit_document/3`, `refresh_status/3`,
//! `cancel_document/4`, `test_connection/2` and `Bilimbi.Finance.Einvoice.Clearance`):
//! submitting it, polling until it is validated or refused, refreshing its
//! status, and cancelling it. Every outcome is a new status event with the
//! authority's answer beside it, and every outcome the authority decides is
//! also an audit action; nothing here edits a document.
//!
//! The facade authorizes each call, and the submit and poll jobs run as the
//! person who asked (`Queue::enqueue_for`), so a recorded event and an audit
//! action name that person. No transaction is held across a call to the
//! authority: a submission's bytes commit before it is sent, and its answer is
//! recorded in a second transaction under the company lock, only while the
//! document is still submittable.
//!
//! A status that leaves ledger work (a validated document's entry, a
//! cancelled one's reversal) runs it through the bridge (`posting.rs`) once the
//! status is committed and outside any transaction of ours.
use crate::authority::{
    Answer, Authority, AuthorityStatus, CancelOutcome, CancelRefusal, Connection, Credentials,
    DocumentStatus, LoginOutcome, Lookup, Original, PayloadError, PayloadOptions, Progress, Search,
    StatusRead, SubmitOutcome, ValidationContext,
};
use crate::code_lists::{self, Codes};
use crate::document::{Details, PartySnapshot};
use crate::documents::{self, Attempt, DocumentView, NewEvent, Recorded, SourceLink, RELEASING};
use crate::draft::Draft;
use crate::myinvois::signer::SigningKey;
use crate::posting::{utc_date, Bridge};
use crate::{
    authorize, configuration, refusal, supplier_profile, text, Certificate, DraftSource, Einvoice,
    EinvoiceError, FieldError, PrivateKey, Result, CANCEL, ISSUE, SUBMIT,
};
use bilibi_agent_api::OperationClient;
use bilibi_audit::Attribution;
use bilibi_company::WriteError;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::LedgerError;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{Execution, Outcome};
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use jiff::{SignedDuration, Timestamp};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio_postgres::GenericClient;

/// The submit job's worker ID.
pub const SUBMIT_WORKER: &str = "finance/einvoice-submit";
/// The poll job's worker ID.
pub const POLL_WORKER: &str = "finance/einvoice-poll";

// A document a consolidated document holds (`consolidated`) is reported by it
// and never submitted on its own; one it released may be.
const SUBMITTABLE: [&str; 4] = ["issued", "released", "refused", "invalid"];
const CANCELLABLE: [&str; 2] = ["valid", "rejected"];
// Notes in these statuses still reference their original at the authority.
const ACTIVE_NOTE_STATUSES: [&str; 3] = ["submitted", "valid", "rejected"];
const MAX_READS_PER_JOB: u32 = 10;
// The SDK asks for 3 to 5 seconds between reads.
const POLL_INTERVAL: Duration = Duration::from_secs(4);
const LOOKUP_MARGIN_SECONDS: i64 = 60;
const REASON_LENGTH: usize = 300;

/// Whether a document in this status may be submitted.
fn submittable(status: &str) -> bool {
    SUBMITTABLE.contains(&status)
}

/// Who a clearance write is attributed to and how its transaction opens: the
/// person calling the facade now, with the capability the call needs, or the
/// queue job acting for them, whose transaction also fences its lease and
/// re-proves the person's grant.
#[derive(Clone, Copy)]
pub(crate) enum Writer<'a> {
    Session(&'static str),
    Job(&'a Execution),
}

/// What a submission needs, checked before a job is queued and again when it
/// runs.
struct Prepared {
    credentials: Credentials,
    version: String,
    signing_key: Option<SigningKey>,
}

/// The exact immutable snapshots reviewed and then recorded by issuance.
#[derive(serde::Serialize)]
pub(crate) struct PreparedIssue {
    environment: String,
    valid: crate::ValidDraft,
    supplier: PartySnapshot,
    buyer: PartySnapshot,
    details: Details,
    source: Option<SourceLink>,
}

/// What a submission attempt leads to, before it is recorded.
enum Decided {
    Submit(SubmitOutcome),
    /// A search for an earlier attempt that could not answer: retried
    /// without sending.
    Deferred(&'static str, Answer),
}

/// The ledger work a status change leaves for after its commit.
enum FollowUp {
    None,
    Post,
    Reverse,
}

impl Einvoice {
    /// Records a document the company issues (`finance.einvoice.documents.issue`):
    /// the draft is validated by its shape and the authority's rules against
    /// the company's code lists and profile (`Draft::validate`); refusals are
    /// `Invalid`, at each field's path in Bilimbi's words, and a code number
    /// the company already issued in the environment is `CodeNumberTaken`. The
    /// company's own party (the supplier, or the buyer of a self-billed
    /// document) is its supplier profile, and the environment its current
    /// setting. The document, its lines and its `issued` event are recorded
    /// in one transaction with the audit action
    /// `finance.einvoice.document.issued`. Amounts are derived, never taken.
    ///
    /// A draft's `source` names the transaction the document is the tax
    /// document of, when another module already recognised it in the ledger
    /// (`finance/receivables` `document:17` and the entry it posted). The
    /// ledger must hold that entry for that owner, unreversed and not itself a
    /// reversal, in the document's currency and for the document's total, and
    /// no other document that stands may link the same source; reading the
    /// entry needs `finance.ledger.view`. The link is recorded with the
    /// document, which then never posts when validated nor reverses when
    /// cancelled: its accounting and its correction are the source module's,
    /// so a note correcting a linked original is refused without its own
    /// `source`. Each problem is a `source` field error.
    pub async fn issue_document(
        &self,
        scope: &Scope,
        company_id: i64,
        draft: &Draft,
    ) -> Result<DocumentView> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                ISSUE,
                "finance.einvoice.issue_document",
            )
            .await?;
        let name = bilibi_company::identity(tx.sql(), scope, company_id)
            .await?
            .ok_or(EinvoiceError::CompanyNotFound)?
            .display_name;
        let (profile, environment) = self
            .issue_settings(tx.sql(), scope, company_id, name)
            .await?;
        let prepared = self
            .prepare_issue(tx.sql(), scope, company_id, draft, profile, environment)
            .await?;
        let document_id = self
            .record_issue(&mut tx, scope, company_id, prepared)
            .await?;
        tx.commit().await?;
        self.view(scope, company_id, document_id).await
    }

    /// Profile and environment in one settings read; never a credential.
    pub(crate) async fn issue_settings(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        name: String,
    ) -> Result<(crate::Supplier, String)> {
        let target = company_target(company_id)?;
        let mut keys: Vec<String> = configuration::SUPPLIER_FIELDS
            .iter()
            .map(|field| configuration::supplier_key(field))
            .collect();
        keys.push("finance.einvoice.environment".into());
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let values = self
            .settings
            .resolve_many(client, scope, target, &keys)
            .await?;
        let environment = values
            .get("finance.einvoice.environment")
            .and_then(|v| text(&v.value))
            .unwrap_or_else(|| "sandbox".into());
        Ok((supplier_profile(&values, name), environment))
    }

    pub(crate) async fn prepare_issue(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        draft: &Draft,
        profile: crate::Supplier,
        environment: String,
    ) -> Result<PreparedIssue> {
        let authority = Self::authority();
        let tenant = scope.tenant();
        // Every list once per issue, so each rule reads the same snapshot.
        let mut codes: BTreeMap<String, Codes> = BTreeMap::new();
        for list in code_lists::lists(authority.id()) {
            codes.insert(
                list.to_owned(),
                code_lists::codes(client, tenant, company_id, authority.id(), list).await?,
            );
        }
        let original = match draft.original_document_id {
            Some(id) if id > 0 => documents::fetch(client, tenant, company_id, id).await?,
            _ => None,
        };
        let original_rules = original.as_ref().map(|original| Original {
            document_type: original.document.document_type.clone(),
            currency_code: original.document.currency_code.clone(),
            status: original.status().to_owned(),
            document_uuid: original.document_uuid().map(str::to_owned),
        });
        let context = ValidationContext {
            supplier: &profile,
            codes: &codes,
            original: original_rules.as_ref(),
            consolidated: false,
        };
        let valid = draft
            .validate(Timestamp::now(), &authority, &context)
            .map_err(EinvoiceError::Invalid)?;
        if documents::code_number_taken(client, company_id, &environment, &valid.code_number)
            .await?
        {
            return Err(EinvoiceError::CodeNumberTaken);
        }
        let source = match &draft.source {
            Some(source) => Some(
                self.source_link(client, scope, company_id, source, &valid)
                    .await?
                    .map_err(|message| {
                        EinvoiceError::Invalid(vec![FieldError::new("source", message)])
                    })?,
            ),
            None if original
                .as_ref()
                .is_some_and(|original| original.recognised_by_source()) =>
            {
                return Err(EinvoiceError::Invalid(vec![FieldError::new(
                    "source",
                    "is required: the original document is the tax document of a transaction \
                     another module recognised",
                )]));
            }
            None => None,
        };
        let self_billed = authority
            .self_billed_document_types()
            .contains(&valid.document_type.as_str());
        let counterparty = if self_billed {
            valid.supplier.as_ref()
        } else {
            valid.buyer.as_ref()
        }
        .ok_or(EinvoiceError::Failed(Error::Internal))?;
        let (supplier, buyer) = if self_billed {
            (
                PartySnapshot::of_party(counterparty, true),
                PartySnapshot::of_profile(&profile),
            )
        } else {
            (
                PartySnapshot::of_profile(&profile),
                PartySnapshot::of_party(counterparty, false),
            )
        };
        // A note snapshots the original's code number and UUID: what the
        // payload references, as it stood when the note was issued.
        let details = Details {
            payment_means_code: valid.payment_means_code.clone(),
            payment_terms: valid.payment_terms.clone(),
            billing_period_start: valid.billing_period_start.map(|date| date.to_string()),
            billing_period_end: valid.billing_period_end.map(|date| date.to_string()),
            billing_period_description: valid.billing_period_description.clone(),
            original_code_number: original
                .as_ref()
                .map(|original| original.document.code_number.clone()),
            original_document_uuid: original
                .as_ref()
                .and_then(|original| original.document_uuid().map(str::to_owned)),
            consolidation_month: None,
        };
        Ok(PreparedIssue {
            environment,
            valid,
            supplier,
            buyer,
            details,
            source,
        })
    }

    /// Joins the caller's audited transaction; never opens or commits one.
    pub(crate) async fn record_issue(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        prepared: PreparedIssue,
    ) -> Result<i64> {
        authorize(tx.sql(), scope, company_id, ISSUE).await?;
        bilibi_company::require_writable(tx.sql(), scope, company_id, true)
            .await
            .map_err(write_refusal)?;
        let PreparedIssue {
            environment,
            valid,
            supplier,
            buyer,
            details,
            source,
        } = prepared;
        let authority = Self::authority();
        let (document_type, code_number) = (valid.document_type.clone(), valid.code_number.clone());
        let original_document_id = valid.original_document_id;
        let document_id = documents::insert_document(
            tx,
            scope,
            company_id,
            authority.id(),
            environment.clone(),
            valid,
            supplier,
            buyer,
            details,
        )
        .await?;
        let (source_module, source_record, source_entry) = match &source {
            Some(link) => (
                Some(link.module.clone()),
                Some(link.record.clone()),
                Some(link.ledger_entry_id),
            ),
            None => (None, None, None),
        };
        if let Some(link) = source {
            documents::record_source_link(tx, scope, company_id, document_id, link).await?;
        }
        action(
            tx,
            "finance.einvoice.document.issued",
            json!({
                "document_id": document_id, "document_type": document_type,
                "code_number": code_number, "environment": environment,
                "original_document_id": original_document_id,
                "source_module": source_module, "source_record": source_record,
                "source_ledger_entry_id": source_entry,
            }),
        )
        .await?;
        Ok(document_id)
    }

    /// One document with its lines, its status events in order, its current
    /// status and what the events say about it (`finance.einvoice.view`).
    pub async fn get_document(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<DocumentView> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, crate::VIEW).await?;
        drop(client);
        self.view(scope, company_id, document_id).await
    }

    /// Queues the document's submission to the authority as the signed-in
    /// person (`finance.einvoice.documents.submit`), answering the job's ID.
    /// Refused before queuing when the document's status is not `issued`,
    /// `released`, `refused` or `invalid` (`NotSubmittable`), another document
    /// that stands already links a source of this one and no earlier attempt
    /// of this one is unresolved (`SourceTaken`), the company lacks
    /// credentials for the document's environment (`MissingCredentials`), the
    /// chosen format version is signed and the certificate and key are
    /// missing or mismatched (`SigningKeyRequired`), or a submit job for the
    /// document is already waiting or running (`AlreadyQueued`). The audit
    /// action is `finance.einvoice.submission.queued`.
    pub async fn submit_document(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<i64> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                SUBMIT,
                "finance.einvoice.submit_document",
            )
            .await?;
        let tenant = scope.tenant();
        let recorded = documents::fetch(tx.sql(), tenant, company_id, document_id)
            .await?
            .ok_or(EinvoiceError::DocumentNotFound)?;
        self.prepare(tx.sql(), scope, company_id, &recorded).await?;
        if documents::unresolved_attempts(tx.sql(), tenant, company_id, &recorded)
            .await?
            .is_empty()
            && documents::source_taken(tx.sql(), document_id).await?
        {
            return Err(EinvoiceError::SourceTaken);
        }
        if submission_queued(tx.sql(), tenant, company_id, document_id).await? {
            return Err(EinvoiceError::AlreadyQueued);
        }
        let job = self
            .queue
            .enqueue_for(
                &mut tx,
                scope,
                SUBMIT_WORKER,
                job_args(company_id, document_id),
            )
            .await
            .map_err(refusal)?;
        let document = &recorded.document;
        action(
            &tx,
            "finance.einvoice.submission.queued",
            json!({
                "document_id": document.id, "code_number": document.code_number,
                "environment": document.environment, "job_id": job.id(),
            }),
        )
        .await?;
        tx.commit().await?;
        Ok(job.id())
    }

    /// Reads the document's status at the authority and records what changed
    /// (`finance.einvoice.documents.submit`): a buyer's rejection request
    /// becomes `rejected`, a cancellation made on the portal `cancelled`, and
    /// an unchanged status is kept as `status_checked`. Answers the document
    /// afterwards; `NotSubmitted` before the authority has given it a UUID,
    /// and `Authority(code)` when the authority could not answer
    /// (`rate_limited`, `authority_unavailable`, `not_yet_available`,
    /// `status_unreadable`).
    pub async fn refresh_status(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<DocumentView> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, SUBMIT).await?;
        drop(client);
        self.ensure_writable(scope, company_id, Writer::Session(SUBMIT))
            .await?;
        let recorded = self.fetch(scope, company_id, document_id).await?;
        if recorded.document_uuid().is_none() {
            return Err(EinvoiceError::NotSubmitted);
        }
        let credentials = self
            .credentials(scope, company_id, &recorded.document.environment)
            .await?;
        match self
            .read_status(
                scope,
                company_id,
                &recorded,
                &credentials,
                Writer::Session(SUBMIT),
            )
            .await
        {
            Ok(_) => self.view(scope, company_id, document_id).await,
            Err(Outcome::Retry(code) | Outcome::Cancel(code)) => {
                Err(EinvoiceError::Authority(code))
            }
            Err(Outcome::Completed) => self.view(scope, company_id, document_id).await,
        }
    }

    /// Cancels a validated document at the authority with `reason` (1 to 300
    /// characters), inside the authority's cancellation window after
    /// validation and only while no note the authority holds references it
    /// (`finance.einvoice.documents.cancel`); then records `cancelled`.
    /// Before asking, it reads the document's status at the authority (an
    /// earlier request's answer may have been lost): when the authority
    /// already holds it as cancelled, that is recorded, the ledger reversal
    /// runs, and the updated document is answered as a success without a
    /// request; when the read cannot confirm a cancellable status, nothing is
    /// sent and the read's own refusal is answered (`RateLimited`,
    /// `AuthorityUnavailable` or `Authority(code)`).
    /// Refusals: `NotCancellable`, `CancellationPeriodOver`,
    /// `ActiveReferencingDocuments`, `InvalidReason`, `CancellationRefused`
    /// from the authority (recorded as `cancellation_refused`), `RateLimited`
    /// and `AuthorityUnavailable`. The audit actions are
    /// `finance.einvoice.cancelled` and `finance.einvoice.cancellation.refused`.
    pub async fn cancel_document(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        reason: &str,
    ) -> Result<DocumentView> {
        let authority = Self::authority();
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, CANCEL).await?;
        let tenant = scope.tenant();
        let recorded = documents::fetch(&**client, tenant, company_id, document_id)
            .await?
            .ok_or(EinvoiceError::DocumentNotFound)?;
        let Some(uuid) = recorded
            .document_uuid()
            .filter(|_| CANCELLABLE.contains(&recorded.status()))
            .map(str::to_owned)
        else {
            return Err(EinvoiceError::NotCancellable);
        };
        if recorded
            .cancellable_until(&authority)
            .is_some_and(|until| Timestamp::now() > until)
        {
            return Err(EinvoiceError::CancellationPeriodOver);
        }
        // A note the authority holds (submitted, valid, or under rejection)
        // keeps its original from being cancelled; the authority refuses that
        // too.
        let notes = documents::notes_of(&**client, tenant, company_id, document_id).await?;
        if documents::statuses(&**client, tenant, company_id, &notes)
            .await?
            .iter()
            .any(|(_, status)| ACTIVE_NOTE_STATUSES.contains(&status.as_str()))
        {
            return Err(EinvoiceError::ActiveReferencingDocuments);
        }
        if reason.trim().is_empty() || reason.chars().count() > REASON_LENGTH {
            return Err(EinvoiceError::InvalidReason);
        }
        drop(client);
        self.ensure_writable(scope, company_id, Writer::Session(CANCEL))
            .await?;
        let document = &recorded.document;
        let credentials = self
            .credentials(scope, company_id, &document.environment)
            .await?;
        // An earlier cancellation request may have reached the authority even
        // when its answer was lost. Check its status before sending another.
        let status = self
            .read_status(
                scope,
                company_id,
                &recorded,
                &credentials,
                Writer::Session(CANCEL),
            )
            .await
            .map_err(|outcome| match outcome {
                Outcome::Retry("rate_limited") => EinvoiceError::RateLimited,
                Outcome::Cancel(code) => EinvoiceError::Authority(code),
                _ => EinvoiceError::AuthorityUnavailable,
            })?;
        if status == "cancelled" {
            let mut view = self.view(scope, company_id, document_id).await?;
            view.already_cancelled = true;
            return Ok(view);
        }
        if !CANCELLABLE.contains(&status.as_str()) {
            return Err(EinvoiceError::NotCancellable);
        }
        let api_url = self.api_url(&document.environment).await?;
        let connection = self.connection(scope, company_id, &credentials, &api_url);
        let base = json!({
            "document_id": document.id, "code_number": document.code_number,
            "environment": document.environment, "document_uuid": uuid,
        });
        match authority.cancel(&connection, &uuid, reason).await {
            CancelOutcome::Cancelled(answer) => {
                let mut event = event_with("cancelled", &answer);
                event.authority_status = Some(AuthorityStatus::Cancelled.name());
                event.document_uuid = Some(uuid.clone());
                event.reason = Some(reason.to_owned());
                let recorded = self
                    .while_cancellable(
                        scope,
                        company_id,
                        document_id,
                        event,
                        ("finance.einvoice.cancelled", &base, &answer),
                    )
                    .await?;
                if recorded {
                    self.settle(
                        scope,
                        company_id,
                        document_id,
                        FollowUp::Reverse,
                        Writer::Session(CANCEL),
                    )
                    .await;
                }
                self.view(scope, company_id, document_id).await
            }
            CancelOutcome::Refused(code, answer) => {
                let mut event = event_with("cancellation_refused", &answer);
                event.document_uuid = Some(uuid.clone());
                event.reason = answer
                    .reason
                    .clone()
                    .or_else(|| Some(refusal_name(code).into()));
                self.while_cancellable(
                    scope,
                    company_id,
                    document_id,
                    event,
                    ("finance.einvoice.cancellation.refused", &base, &answer),
                )
                .await?;
                Err(EinvoiceError::CancellationRefused {
                    code,
                    reason: answer.reason,
                })
            }
            CancelOutcome::RateLimited(_) => Err(EinvoiceError::RateLimited),
            CancelOutcome::Unavailable(_) => Err(EinvoiceError::AuthorityUnavailable),
        }
    }

    /// Logs in to the authority with the company's current environment and
    /// credentials (`finance.einvoice.settings.manage`), and records the
    /// outcome as the audit action `finance.einvoice.connection.tested`.
    pub async fn test_connection(&self, scope: &Scope, company_id: i64) -> Result<LoginOutcome> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, crate::SETTINGS).await?;
        let environment = self.environment(&**client, scope, company_id).await?;
        drop(client);
        let credentials = self.credentials(scope, company_id, &environment).await?;
        let api_url = self.api_url(&environment).await?;
        let connection = self.connection(scope, company_id, &credentials, &api_url);
        let outcome = Self::authority().login(&connection).await;
        let client = self.db.get().await.map_err(Error::from)?;
        bilibi_audit::record_action(
            &**client,
            &Attribution::from_scope(scope),
            None,
            "finance.einvoice.connection.tested",
            json!({ "company_id": company_id, "environment": environment, "outcome": outcome.name() }),
            false,
        )
        .await?;
        Ok(outcome)
    }

    /// One submission attempt for the document at the instant `now`, as the
    /// submit job runs it for the person who queued it
    /// (`finance.einvoice.documents.submit`). A document whose status is
    /// already past submitting is left alone (`Completed`). Otherwise the
    /// answer is recorded: `submitted` (the poll job is queued), `refused`,
    /// `duplicate` (`Cancel("duplicate_submission")`, for an operator),
    /// `rate_limited` and `unavailable` (`Retry`), and once the authority has
    /// been unavailable for its outage allowance since the first failed
    /// attempt, `overdue` with the audit action
    /// `finance.einvoice.submission.overdue`. A document that cannot be sent
    /// records `unsendable` and cancels the job with the reason.
    pub async fn run_submission(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        now: Timestamp,
    ) -> Outcome {
        self.submission(scope, company_id, document_id, now, Writer::Session(SUBMIT))
            .await
    }

    /// Reads a submitted document's progress until the authority decides,
    /// within one job's bounds (`finance.einvoice.documents.submit`), as the
    /// poll job runs it: `Retry("in_progress")` while the authority is still
    /// processing, and once it has decided, `valid` (with the long ID that
    /// completes the validation link) or `invalid` (with its validation
    /// results) is recorded.
    pub async fn run_poll(&self, scope: &Scope, company_id: i64, document_id: i64) -> Outcome {
        self.poll(scope, company_id, document_id, Writer::Session(SUBMIT))
            .await
    }

    pub(crate) async fn submission(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        now: Timestamp,
        writer: Writer<'_>,
    ) -> Outcome {
        if let Err(outcome) = self.job_reach(scope, company_id, writer).await {
            return outcome;
        }
        let recorded = match self.fetch(scope, company_id, document_id).await {
            Ok(recorded) => recorded,
            Err(error) => return cancel_for(error),
        };
        if !submittable(recorded.status()) {
            return Outcome::Completed;
        }
        match self
            .submit_recorded(scope, company_id, recorded, now, writer)
            .await
        {
            Ok(outcome) | Err(outcome) => outcome,
        }
    }

    async fn submit_recorded(
        &self,
        scope: &Scope,
        company_id: i64,
        recorded: Recorded,
        now: Timestamp,
        writer: Writer<'_>,
    ) -> std::result::Result<Outcome, Outcome> {
        let client = self.db.get().await.map_err(|_| retry_database())?;
        let prepared = self.prepare(&**client, scope, company_id, &recorded).await;
        drop(client);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let reason = prepare_code(&error);
                self.record_alone(
                    scope,
                    company_id,
                    recorded.document.id,
                    unsendable(format!("cannot submit: {reason}")),
                    writer,
                )
                .await?;
                return Ok(Outcome::Cancel(reason));
            }
        };
        let client = self.db.get().await.map_err(|_| retry_database())?;
        let attempts =
            documents::unresolved_attempts(&**client, scope.tenant(), company_id, &recorded)
                .await
                .map_err(|_| retry_database())?;
        drop(client);
        if attempts.is_empty() {
            return self
                .send(scope, company_id, &recorded, &prepared, now, writer)
                .await;
        }
        // An earlier attempt that never got a final answer may have been
        // accepted. Ask the authority before sending the bytes again: an
        // accepted document is recorded, a search that cannot answer
        // (anything but a clean not-found) defers the retry without sending,
        // and only a clean not-found lets the document go out again.
        let api_url = self
            .api_url(&recorded.document.environment)
            .await
            .map_err(|_| retry_database())?;
        let connection = self.connection(scope, company_id, &prepared.credentials, &api_url);
        let decided = match self.find(&connection, &recorded, &attempts).await {
            Search::Found(found) => Decided::Submit(SubmitOutcome::Accepted(found)),
            Search::NotFound(_) => {
                return self
                    .send(scope, company_id, &recorded, &prepared, now, writer)
                    .await;
            }
            Search::RateLimited(answer) => Decided::Deferred("rate_limited", answer),
            Search::Unavailable(answer) => Decided::Deferred("unavailable", answer),
            Search::Refused(answer) => Decided::Deferred("refused", answer),
        };
        self.record_outcome(
            scope,
            company_id,
            &recorded,
            attempts[0].id,
            decided,
            now,
            writer,
        )
        .await
    }

    async fn send(
        &self,
        scope: &Scope,
        company_id: i64,
        recorded: &Recorded,
        prepared: &Prepared,
        now: Timestamp,
        writer: Writer<'_>,
    ) -> std::result::Result<Outcome, Outcome> {
        let authority = Self::authority();
        let document = &recorded.document;
        let options = PayloadOptions {
            version: &prepared.version,
            signing_key: prepared.signing_key.as_ref(),
        };
        let payload = match authority.payload(document, &options, now) {
            Ok(payload) => payload,
            Err(error) => {
                let code = payload_code(error);
                self.record_alone(
                    scope,
                    company_id,
                    document.id,
                    unsendable(format!("cannot build the payload: {code}")),
                    writer,
                )
                .await?;
                return Ok(Outcome::Cancel(code));
            }
        };
        // The bytes are committed before the authority sees them, unless
        // another document that stands took the source since `prepare` looked.
        let mut client = self.db.get().await.map_err(|_| retry_database())?;
        let mut tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                writer,
                "finance.einvoice.store_submission",
            )
            .await
            .map_err(cancel_for)?;
        if documents::source_taken(tx.sql(), document.id)
            .await
            .map_err(|_| retry_database())?
        {
            drop(tx);
            drop(client);
            self.record_alone(
                scope,
                company_id,
                document.id,
                unsendable("cannot submit: source_taken".into()),
                writer,
            )
            .await?;
            return Ok(Outcome::Cancel("source_taken"));
        }
        let sha256 = crate::myinvois::sha256_hex(&payload.bytes);
        let attempt =
            documents::store_submission(&mut tx, scope, company_id, document, &payload, sha256)
                .await
                .map_err(|_| retry_database())?;
        tx.commit().await.map_err(|_| retry_database())?;
        drop(client);

        let api_url = self
            .api_url(&document.environment)
            .await
            .map_err(|_| retry_database())?;
        let connection = self.connection(scope, company_id, &prepared.credentials, &api_url);
        let outcome = authority
            .submit(&connection, &payload, &document.code_number)
            .await;
        let decided = self
            .recover(scope, company_id, &connection, recorded, outcome)
            .await;
        self.record_outcome(
            scope, company_id, recorded, attempt.id, decided, now, writer,
        )
        .await
    }

    // A duplicate, or an outage that may have hidden an accepted request,
    // means the authority may hold the document already: ask for it by its
    // code number, among what was submitted since the document last became
    // submittable, instead of sending the bytes again. A lookup that finds
    // nothing leaves the outcome as it was; one that cannot answer turns a
    // duplicate into a deferred search.
    async fn recover(
        &self,
        scope: &Scope,
        company_id: i64,
        connection: &Connection<'_>,
        recorded: &Recorded,
        outcome: SubmitOutcome,
    ) -> Decided {
        let (SubmitOutcome::Duplicate(_) | SubmitOutcome::Unavailable(_)) = &outcome else {
            return Decided::Submit(outcome);
        };
        let attempts = match self.db.get().await {
            Ok(client) => {
                documents::unresolved_attempts(&**client, scope.tenant(), company_id, recorded)
                    .await
            }
            Err(error) => Err(Error::from(error)),
        };
        let Ok(attempts) = attempts else {
            return Decided::Submit(outcome);
        };
        let (status, answer) = match self.find(connection, recorded, &attempts).await {
            Search::Found(found) => return Decided::Submit(SubmitOutcome::Accepted(found)),
            Search::NotFound(_) => return Decided::Submit(outcome),
            Search::RateLimited(answer) => ("rate_limited", answer),
            Search::Unavailable(answer) => ("unavailable", answer),
            Search::Refused(answer) => ("refused", answer),
        };
        match outcome {
            SubmitOutcome::Duplicate(_) => Decided::Deferred(status, answer),
            outcome => Decided::Submit(outcome),
        }
    }

    /// The window runs from a minute before the oldest unresolved attempt to
    /// a minute past the moment of asking, so it holds the attempt that has
    /// just ended; a document the authority holds under an identifier this
    /// document already recorded belongs to an earlier cycle.
    async fn find(
        &self,
        connection: &Connection<'_>,
        recorded: &Recorded,
        attempts: &[Attempt],
    ) -> Search {
        let oldest = attempts
            .iter()
            .map(|attempt| attempt.recorded_at)
            .min()
            .unwrap_or_else(Timestamp::now);
        let exclude = recorded.known_uuids();
        let lookup = Lookup {
            code_number: &recorded.document.code_number,
            since: oldest - SignedDuration::from_secs(LOOKUP_MARGIN_SECONDS),
            until: Timestamp::now() + SignedDuration::from_secs(LOOKUP_MARGIN_SECONDS + 1),
            exclude: &exclude,
        };
        Self::authority().find_submission(connection, &lookup).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_outcome(
        &self,
        scope: &Scope,
        company_id: i64,
        recorded: &Recorded,
        submission_id: i64,
        decided: Decided,
        now: Timestamp,
        writer: Writer<'_>,
    ) -> std::result::Result<Outcome, Outcome> {
        let document = &recorded.document;
        let tenant = scope.tenant();
        let mut client = self.db.get().await.map_err(|_| retry_database())?;
        let mut tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                writer,
                "finance.einvoice.record_submission",
            )
            .await
            .map_err(cancel_for)?;
        // Another job may have recorded its answer while this one waited for
        // the lock; an outcome is recorded only while the document is still
        // submittable, so of two overlapping jobs only the first leaves an
        // event, an audit action and a poll job.
        let status = documents::status(tx.sql(), tenant, company_id, document.id)
            .await
            .map_err(|_| retry_database())?;
        if !status.as_deref().is_some_and(submittable) {
            return Ok(Outcome::Completed);
        }
        let base = json!({
            "document_id": document.id, "code_number": document.code_number,
            "environment": document.environment, "submission_id": submission_id,
        });
        let database = |_: Error| retry_database();
        let outcome = match decided {
            Decided::Submit(SubmitOutcome::Accepted(answer)) => {
                let mut event = event_with("submitted", &answer);
                event.submission_id = Some(submission_id);
                event.authority_status = Some(AuthorityStatus::Submitted.name());
                event.submission_uid = answer.submission_uid.clone();
                event.document_uuid = answer.document_uuid.clone();
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                answer_action(&tx, "finance.einvoice.submitted", &base, &answer)
                    .await
                    .map_err(database)?;
                self.queue
                    .enqueue_for(
                        &mut tx,
                        scope,
                        POLL_WORKER,
                        job_args(company_id, document.id),
                    )
                    .await
                    .map_err(|_| Outcome::Cancel("poll_not_queued"))?;
                Outcome::Completed
            }
            Decided::Submit(SubmitOutcome::Refused(answer)) => {
                let mut event = event_with("refused", &answer);
                event.submission_id = Some(submission_id);
                event.submission_uid = answer.submission_uid.clone();
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                answer_action(&tx, "finance.einvoice.refused", &base, &answer)
                    .await
                    .map_err(database)?;
                Outcome::Completed
            }
            Decided::Submit(SubmitOutcome::Duplicate(answer)) => {
                let mut event = event_with("duplicate", &answer);
                event.submission_id = Some(submission_id);
                event.reason = Some(
                    "the authority holds an identical submission; read its status before resubmitting"
                        .into(),
                );
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                answer_action(&tx, "finance.einvoice.submission.duplicate", &base, &answer)
                    .await
                    .map_err(database)?;
                Outcome::Cancel("duplicate_submission")
            }
            Decided::Deferred(status, answer) => {
                let mut event = event_with("search_deferred", &answer);
                event.submission_id = Some(submission_id);
                let why = if status == "rate_limited" {
                    retry_after_reason(&answer)
                } else {
                    answer.reason.clone().unwrap_or_else(|| "no answer".into())
                };
                event.reason = Some(format!(
                    "the search for the document could not answer: {why}"
                ));
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                self.notice_if_overdue(&mut tx, scope, company_id, document, now)
                    .await
                    .map_err(database)?;
                Outcome::Retry(if status == "rate_limited" {
                    "rate_limited"
                } else {
                    "authority_unavailable"
                })
            }
            Decided::Submit(SubmitOutcome::RateLimited(answer)) => {
                let mut event = event_with("rate_limited", &answer);
                event.submission_id = Some(submission_id);
                event.reason = Some(retry_after_reason(&answer));
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                Outcome::Retry("rate_limited")
            }
            Decided::Submit(SubmitOutcome::Unavailable(answer)) => {
                let mut event = event_with("unavailable", &answer);
                event.submission_id = Some(submission_id);
                documents::record_event(&mut tx, scope, company_id, document.id, event)
                    .await
                    .map_err(database)?;
                self.notice_if_overdue(&mut tx, scope, company_id, document, now)
                    .await
                    .map_err(database)?;
                Outcome::Retry("authority_unavailable")
            }
        };
        tx.commit().await.map_err(|_| retry_database())?;
        Ok(outcome)
    }

    /// Once the authority has been unreachable for its outage allowance since
    /// this document's first failed attempt, the operator is told once,
    /// through an `overdue` event the page shows and an audit action; the job
    /// keeps retrying.
    async fn notice_if_overdue(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        document: &crate::Document,
        now: Timestamp,
    ) -> bilibi_database::Result<()> {
        let hours = Self::authority().outage_notice_hours();
        let events = documents::events(tx.sql(), scope.tenant(), company_id, document.id).await?;
        let start = events
            .iter()
            .rposition(|event| documents::STATUS_EVENTS.contains(&event.event.as_str()))
            .map_or(0, |index| index + 1);
        let since_status = &events[start..];
        let first = since_status
            .iter()
            .find(|event| ["unavailable", "search_deferred"].contains(&event.event.as_str()));
        let noticed = since_status.iter().any(|event| event.event == "overdue");
        let Some(first) = first else { return Ok(()) };
        if noticed || now < first.recorded_at + SignedDuration::from_hours(i64::from(hours)) {
            return Ok(());
        }
        let mut overdue = NewEvent::new("overdue");
        overdue.reason = Some(format!(
            "not submitted within {hours} hours of the authority becoming unavailable"
        ));
        documents::record_event(tx, scope, company_id, document.id, overdue).await?;
        action(
            tx,
            "finance.einvoice.submission.overdue",
            json!({
                "document_id": document.id, "code_number": document.code_number,
                "environment": document.environment,
                "unavailable_since": first.recorded_at.to_string(),
            }),
        )
        .await
    }

    pub(crate) async fn poll(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        writer: Writer<'_>,
    ) -> Outcome {
        if let Err(outcome) = self.job_reach(scope, company_id, writer).await {
            return outcome;
        }
        let recorded = match self.fetch(scope, company_id, document_id).await {
            Ok(recorded) => recorded,
            Err(error) => return cancel_for(error),
        };
        let Some(uid) = recorded
            .submission_uid()
            .filter(|_| recorded.status() == "submitted")
            .map(str::to_owned)
        else {
            return Outcome::Completed;
        };
        let credentials = match self
            .credentials(scope, company_id, &recorded.document.environment)
            .await
        {
            Ok(credentials) => credentials,
            Err(error) => return cancel_for(error),
        };
        let api_url = match self.api_url(&recorded.document.environment).await {
            Ok(url) => url,
            Err(_) => return retry_database(),
        };
        let connection = self.connection(scope, company_id, &credentials, &api_url);
        let authority = Self::authority();
        let mut reads_left = MAX_READS_PER_JOB;
        loop {
            match authority.submission_progress(&connection, &uid).await {
                Progress::InProgress(_) if reads_left > 1 => {
                    reads_left -= 1;
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Progress::InProgress(_) => return Outcome::Retry("in_progress"),
                Progress::Done(_) => {
                    return match self
                        .read_status(scope, company_id, &recorded, &credentials, writer)
                        .await
                    {
                        Ok(status) if status == "submitted" => Outcome::Retry("in_progress"),
                        Ok(_) => Outcome::Completed,
                        Err(outcome) => outcome,
                    };
                }
                Progress::RateLimited(_) => return Outcome::Retry("rate_limited"),
                Progress::Unavailable(_) => return Outcome::Retry("authority_unavailable"),
                Progress::Refused(answer) => {
                    let event = event_with("status_unreadable", &answer);
                    if let Err(outcome) = self
                        .record_alone(scope, company_id, document_id, event, writer)
                        .await
                    {
                        return outcome;
                    }
                    return Outcome::Cancel("status_unreadable");
                }
            }
        }
    }

    // The authority's current view of the document, recorded as the event it
    // implies; answers the status the document now has. A 404 right after a
    // submission is the authority not having indexed the document yet (the
    // sandbox answers "not authorized to get document details" for a few
    // seconds), so it is retried rather than recorded as unreadable.
    async fn read_status(
        &self,
        scope: &Scope,
        company_id: i64,
        recorded: &Recorded,
        credentials: &Credentials,
        writer: Writer<'_>,
    ) -> std::result::Result<String, Outcome> {
        let document = &recorded.document;
        let uuid = recorded
            .document_uuid()
            .ok_or(Outcome::Cancel("not_submitted"))?;
        let api_url = self
            .api_url(&document.environment)
            .await
            .map_err(|_| retry_database())?;
        let connection = self.connection(scope, company_id, credentials, &api_url);
        match Self::authority().document_status(&connection, uuid).await {
            StatusRead::Read(answer) => {
                let mut client = self.db.get().await.map_err(|_| retry_database())?;
                let mut tx = self
                    .begin(
                        &mut client,
                        scope,
                        company_id,
                        writer,
                        "finance.einvoice.record_status",
                    )
                    .await
                    .map_err(cancel_for)?;
                let current = documents::fetch(tx.sql(), scope.tenant(), company_id, document.id)
                    .await
                    .map_err(|_| retry_database())?
                    .ok_or(Outcome::Cancel("document_not_found"))?;
                let (status, follow_up, changed) = self
                    .apply_status(&mut tx, scope, company_id, &current, &answer)
                    .await
                    .map_err(|_| retry_database())?;
                if changed {
                    tx.commit().await.map_err(|_| retry_database())?;
                    self.settle(scope, company_id, document.id, follow_up, writer)
                        .await;
                }
                Ok(status)
            }
            StatusRead::RateLimited(_) => Err(Outcome::Retry("rate_limited")),
            StatusRead::Unavailable(_) => Err(Outcome::Retry("authority_unavailable")),
            StatusRead::Refused(answer) if answer.http_status == Some(404) => {
                Err(Outcome::Retry("not_yet_available"))
            }
            StatusRead::Refused(answer) => {
                let event = event_with("status_unreadable", &answer);
                self.record_alone(scope, company_id, document.id, event, writer)
                    .await?;
                Err(Outcome::Cancel("status_unreadable"))
            }
        }
    }

    // Records the status event the answer implies and returns the status,
    // the ledger work it leaves for after the commit, and whether anything
    // was recorded.
    async fn apply_status(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        current: &Recorded,
        answer: &DocumentStatus,
    ) -> bilibi_database::Result<(String, FollowUp, bool)> {
        let document = &current.document;
        let status = current.status().to_owned();
        let uuid = current.document_uuid().map(str::to_owned);
        let base = json!({
            "document_id": document.id, "code_number": document.code_number,
            "environment": document.environment, "document_uuid": uuid,
        });
        let common = |name: &'static str, authority_status: AuthorityStatus| {
            let mut event = event_with(name, &answer.answer);
            event.authority_status = Some(authority_status.name());
            event.document_uuid = uuid.clone();
            event.long_id = answer.long_id.clone();
            event
        };
        let settled = ["valid", "rejected", "cancelled"].contains(&status.as_str());
        let rejected = answer.rejection_requested_at.is_some();
        Ok(match answer.status {
            AuthorityStatus::Valid
                if rejected && !["rejected", "cancelled"].contains(&status.as_str()) =>
            {
                if !settled {
                    let event = common("valid", AuthorityStatus::Valid);
                    documents::record_event(tx, scope, company_id, document.id, event).await?;
                    answer_action(tx, "finance.einvoice.validated", &base, &answer.answer).await?;
                }
                let mut event = common("rejected", AuthorityStatus::Valid);
                event.reason = Some(
                    answer
                        .answer
                        .reason
                        .clone()
                        .unwrap_or_else(|| "the buyer requested rejection".into()),
                );
                documents::record_event(tx, scope, company_id, document.id, event).await?;
                answer_action(tx, "finance.einvoice.rejected", &base, &answer.answer).await?;
                let follow_up = if status == "valid" {
                    FollowUp::None
                } else {
                    FollowUp::Post
                };
                ("rejected".into(), follow_up, true)
            }
            AuthorityStatus::Valid if !settled => {
                let event = common("valid", AuthorityStatus::Valid);
                documents::record_event(tx, scope, company_id, document.id, event).await?;
                answer_action(tx, "finance.einvoice.validated", &base, &answer.answer).await?;
                ("valid".into(), FollowUp::Post, true)
            }
            AuthorityStatus::Invalid if status != "invalid" => {
                let mut event = common("invalid", AuthorityStatus::Invalid);
                event.reason = invalid_reason(&answer.answer);
                documents::record_event(tx, scope, company_id, document.id, event).await?;
                answer_action(tx, "finance.einvoice.invalid", &base, &answer.answer).await?;
                ("invalid".into(), FollowUp::None, true)
            }
            AuthorityStatus::Cancelled if status != "cancelled" => {
                let event = common("cancelled", AuthorityStatus::Cancelled);
                documents::record_event(tx, scope, company_id, document.id, event).await?;
                answer_action(tx, "finance.einvoice.cancelled", &base, &answer.answer).await?;
                ("cancelled".into(), FollowUp::Reverse, true)
            }
            AuthorityStatus::Submitted => ("submitted".into(), FollowUp::None, false),
            other => {
                let event = common("status_checked", other);
                documents::record_event(tx, scope, company_id, document.id, event).await?;
                (status, FollowUp::None, true)
            }
        })
    }

    /// The ledger work a status change leaves, run once the status is
    /// committed and outside any transaction of ours: the ledger rolls back
    /// the transaction it joins when it refuses, which would take ours with
    /// it. Its outcome is recorded on the document (`posted` or
    /// `posting_failed`, `posting_reversed` or `reversal_failed`); a failure
    /// waits on the reconciliation read for a retry and never fails the
    /// clearance that led to it.
    async fn settle(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        follow_up: FollowUp,
        writer: Writer<'_>,
    ) {
        let bridge = match follow_up {
            FollowUp::None => return,
            FollowUp::Post => Bridge::Post,
            FollowUp::Reverse => Bridge::Reverse,
        };
        self.bridge(
            scope,
            company_id,
            document_id,
            bridge,
            utc_date(Timestamp::now()),
            writer,
        )
        .await;
    }

    /// The source a draft names, held to the ledger: the entry exists, that
    /// module and record own it, it stands unreversed and is not a reversal,
    /// and it recognises what the document bills, in the document's currency;
    /// and no other standing document links the source. The inner error is
    /// the `source` field's message.
    async fn source_link(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        source: &DraftSource,
        draft: &crate::ValidDraft,
    ) -> Result<std::result::Result<SourceLink, String>> {
        const SHAPE: &str = "names a module, a record, and a ledger entry";
        let module = source.module.as_deref().map(str::trim).unwrap_or_default();
        let record = source.record.as_deref().map(str::trim).unwrap_or_default();
        let Some(entry_id) = source.ledger_entry_id.filter(|id| *id > 0) else {
            return Ok(Err(SHAPE.into()));
        };
        if !owner_module(module) || !owner_record(record) {
            return Ok(Err(SHAPE.into()));
        }
        let entry = match self
            .ledger
            .get_entry_on(client, scope, company_id, entry_id)
            .await
        {
            Ok(entry) => entry,
            Err(LedgerError::EntryNotFound) => {
                return Ok(Err(format!(
                    "names ledger entry {entry_id}, which does not exist"
                )));
            }
            Err(LedgerError::Failed(error)) => return Err(error.into()),
            Err(error) => return Ok(Err(format!("cannot read ledger entry {entry_id}: {error}"))),
        };
        let recognised: rust_decimal::Decimal =
            entry.lines.iter().map(|line| line.debit.amount()).sum();
        let billed = draft.line_extension_amount + draft.tax_amount;
        let owned = entry
            .owner
            .as_ref()
            .is_some_and(|owner| owner.module == module && owner.record == record);
        let refusal = if !owned {
            Some(format!(
                "ledger entry {} is not owned by {module} {record}",
                entry.id
            ))
        } else if entry.reverses_entry_id.is_some() {
            Some(format!("ledger entry {} is a reversal", entry.id))
        } else if let Some(by) = entry.reversed_by_entry_id {
            Some(format!(
                "ledger entry {} was reversed by entry {by}",
                entry.id
            ))
        } else if entry.currency_code != draft.currency_code {
            Some(format!(
                "ledger entry {} is in {}, the document in {}",
                entry.id, entry.currency_code, draft.currency_code
            ))
        } else if recognised != billed {
            Some(format!(
                "ledger entry {} recognises {}, not the document's {}",
                entry.id,
                bilibi_locale::money(recognised, 2),
                bilibi_locale::money(billed, 2)
            ))
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return Ok(Err(refusal));
        }
        if let Some(standing) =
            documents::standing_source_document(client, scope.tenant(), company_id, module, record)
                .await?
        {
            return Ok(Err(format!(
                "is already the source of document {standing}, which stands"
            )));
        }
        Ok(Ok(SourceLink {
            module: module.to_owned(),
            record: record.to_owned(),
            ledger_entry_id: entry_id,
        }))
    }

    // Records only if the document, read again under the company lock, is
    // still one the supplier may cancel: a refresh or the portal may have
    // moved it while the authority was asked.
    async fn while_cancellable(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        event: NewEvent,
        (name, base, answer): (&str, &Value, &Answer),
    ) -> Result<bool> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                CANCEL,
                "finance.einvoice.cancel_document",
            )
            .await?;
        let status = documents::status(tx.sql(), scope.tenant(), company_id, document_id).await?;
        if !status
            .as_deref()
            .is_some_and(|status| CANCELLABLE.contains(&status))
        {
            return Ok(false);
        }
        documents::record_event(&mut tx, scope, company_id, document_id, event).await?;
        answer_action(&tx, name, base, answer).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// What a submission needs: the company's credentials for the document's
    /// environment, the format version, and the signing key a signed version
    /// needs.
    async fn prepare(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        recorded: &Recorded,
    ) -> Result<Prepared> {
        let status = recorded.status();
        // A refused or invalid consolidated document released its sources,
        // which a new one may cover; it is never sent again.
        if !submittable(status) || (RELEASING.contains(&status) && recorded.consolidated()) {
            return Err(EinvoiceError::NotSubmittable);
        }
        let environment = &recorded.document.environment;
        let credentials = self
            .read_credentials(client, scope, company_id, environment)
            .await?;
        if !credentials
            .client_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
            || !credentials
                .client_secret
                .as_deref()
                .is_some_and(|secret| !secret.is_empty())
        {
            return Err(EinvoiceError::MissingCredentials);
        }
        let target = company_target(company_id)?;
        let version = text(
            &self
                .settings
                .get(client, scope, target, "finance.einvoice.document_version")
                .await?
                .value,
        )
        .unwrap_or_else(|| crate::myinvois::VERSION.into());
        let signed = Self::authority()
            .document_versions()
            .iter()
            .any(|v| v.version == version && v.signed);
        let signing_key = if signed {
            Some(
                self.signing_key(client, scope, company_id)
                    .await?
                    .ok_or(EinvoiceError::SigningKeyRequired)?,
            )
        } else {
            None
        };
        Ok(Prepared {
            credentials,
            version,
            signing_key,
        })
    }

    /// The stored certificate and private key, when both read and belong
    /// together. They are read here, inside the facade, and only sign.
    async fn signing_key(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
    ) -> Result<Option<SigningKey>> {
        if !self.settings.encryption_available() {
            return Ok(None);
        }
        let target = company_target(company_id)?;
        let read = |key: &'static str| async move {
            self.settings
                .get(client, scope, target, key)
                .await
                .map(|resolved| text(&resolved.value))
        };
        let (Some(certificate), Some(key)) = (
            read("finance.einvoice.certificate_pem").await?,
            read("finance.einvoice.private_key_pem").await?,
        ) else {
            return Ok(None);
        };
        let (Ok(certificate), Ok(key)) =
            (Certificate::parse(&certificate), PrivateKey::parse(&key))
        else {
            return Ok(None);
        };
        Ok(certificate
            .key_matches(&key)
            .then_some(SigningKey { certificate, key }))
    }

    async fn credentials(
        &self,
        scope: &Scope,
        company_id: i64,
        environment: &str,
    ) -> Result<Credentials> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.read_credentials(&**client, scope, company_id, environment)
            .await
    }

    /// The company's client ID and secret for `environment`; a secret that
    /// cannot be read (no settings key) is missing.
    async fn read_credentials(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        environment: &str,
    ) -> Result<Credentials> {
        let target = company_target(company_id)?;
        let client_id = text(
            &self
                .settings
                .get(
                    client,
                    scope,
                    target,
                    &format!("finance.einvoice.{environment}.client_id"),
                )
                .await?
                .value,
        );
        let client_secret = if self.settings.encryption_available() {
            text(
                &self
                    .settings
                    .get(
                        client,
                        scope,
                        target,
                        &format!("finance.einvoice.{environment}.client_secret"),
                    )
                    .await?
                    .value,
            )
        } else {
            None
        };
        Ok(Credentials {
            environment: environment.to_owned(),
            client_id,
            client_secret,
        })
    }

    pub(crate) async fn environment(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
    ) -> Result<String> {
        let target = company_target(company_id)?;
        Ok(text(
            &self
                .settings
                .get(client, scope, target, "finance.einvoice.environment")
                .await?
                .value,
        )
        .unwrap_or_else(|| "sandbox".into()))
    }

    async fn api_url(&self, environment: &str) -> Result<String> {
        let client = self.db.get().await.map_err(Error::from)?;
        let url = self
            .settings
            .get_global(
                &**client,
                &format!("finance.einvoice.{environment}.api_url"),
            )
            .await?;
        Ok(url.as_str().unwrap_or_default().to_owned())
    }

    fn connection<'a>(
        &'a self,
        scope: &'a Scope,
        company_id: i64,
        credentials: &'a Credentials,
        api_url: &'a str,
    ) -> Connection<'a> {
        Connection {
            integration: &self.integration,
            scope,
            company_id,
            credentials,
            api_url,
        }
    }

    pub(crate) async fn fetch(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<Recorded> {
        let client = self.db.get().await.map_err(Error::from)?;
        documents::fetch(&**client, scope.tenant(), company_id, document_id)
            .await?
            .ok_or(EinvoiceError::DocumentNotFound)
    }

    /// The view of a document the caller was authorized to read.
    pub(crate) async fn view(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<DocumentView> {
        let recorded = self.fetch(scope, company_id, document_id).await?;
        let client = self.db.get().await.map_err(Error::from)?;
        let portal = self
            .settings
            .get_global(
                &**client,
                &format!(
                    "finance.einvoice.{}.portal_url",
                    recorded.document.environment
                ),
            )
            .await?;
        self.view_on(
            &**client,
            scope,
            company_id,
            recorded,
            portal.as_str().unwrap_or_default(),
        )
        .await
    }

    pub(crate) async fn view_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        recorded: Recorded,
        portal: &str,
    ) -> Result<DocumentView> {
        let document_id = recorded.document.id;
        let queued = submission_queued(client, scope.tenant(), company_id, document_id).await?;
        let (sources, consolidated_by) =
            crate::consolidation::view(client, scope.tenant(), company_id, document_id).await?;
        let mut view = recorded.view(&Self::authority(), portal, queued);
        view.sources = sources;
        view.consolidated_by = consolidated_by;
        Ok(view)
    }

    /// A clearance write's transaction, holding the company's row lock.
    pub(crate) async fn begin<'c>(
        &self,
        client: &'c mut tokio_postgres::Client,
        scope: &Scope,
        company_id: i64,
        writer: Writer<'_>,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        let tx = match writer {
            Writer::Session(capability) => {
                authorize(&*client, scope, company_id, capability).await?;
                self.mutations
                    .begin(client, scope.mutation_context(action))
                    .await?
            }
            Writer::Job(execution) => execution.begin(client, action).await.map_err(refusal)?,
        };
        bilibi_company::require_writable(tx.sql(), scope, company_id, true)
            .await
            .map_err(write_refusal)?;
        Ok(tx)
    }

    /// Whether the company can be written to, asked before an irreversible
    /// call to the authority so its outcome can still be recorded.
    pub(crate) async fn ensure_writable(
        &self,
        scope: &Scope,
        company_id: i64,
        writer: Writer<'_>,
    ) -> Result<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                writer,
                "finance.einvoice.check_writable",
            )
            .await?;
        tx.rollback().await?;
        Ok(())
    }

    /// A job's own reach: the person's grant on the company now, and a
    /// company that can be written to.
    async fn job_reach(
        &self,
        scope: &Scope,
        company_id: i64,
        writer: Writer<'_>,
    ) -> std::result::Result<(), Outcome> {
        let client = self.db.get().await.map_err(|_| retry_database())?;
        authorize(&**client, scope, company_id, SUBMIT)
            .await
            .map_err(cancel_for)?;
        drop(client);
        self.ensure_writable(scope, company_id, writer)
            .await
            .map_err(cancel_for)
    }

    /// Records one event in a transaction of its own.
    async fn record_alone(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        event: NewEvent,
        writer: Writer<'_>,
    ) -> std::result::Result<(), Outcome> {
        let mut client = self.db.get().await.map_err(|_| retry_database())?;
        let mut tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                writer,
                "finance.einvoice.record_status",
            )
            .await
            .map_err(cancel_for)?;
        documents::record_event(&mut tx, scope, company_id, document_id, event)
            .await
            .map_err(|_| retry_database())?;
        tx.commit().await.map_err(|_| retry_database())
    }
}

pub(crate) fn company_target(company_id: i64) -> Result<Target> {
    Ok(Target::Company(
        OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?,
    ))
}

fn write_refusal(error: WriteError) -> EinvoiceError {
    match error {
        WriteError::NotFound => EinvoiceError::CompanyNotFound,
        WriteError::Archived => EinvoiceError::CompanyArchived,
        WriteError::Access(error) => refusal(error),
        WriteError::InUse | WriteError::Stale | WriteError::Invalid(_) => {
            EinvoiceError::Failed(Error::Internal)
        }
    }
}

/// The two identifiers a submit or poll job carries, and nothing else.
fn job_args(company_id: i64, document_id: i64) -> Value {
    json!({ "company_id": company_id, "document_id": document_id })
}

/// Whether a submit job for the document is waiting or running.
async fn submission_queued(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    document_id: i64,
) -> Result<bool> {
    // TODO(platform): Base Queue reads a job by its ID only; this asks its
    // table for a waiting job by worker and arguments, as Bilimbi asks Oban's.
    Ok(client
        .query_opt(
            "SELECT EXISTS (SELECT 1 FROM base_queue_jobs WHERE tenant_id = $1 \
             AND worker_id = $2 AND state IN ('available', 'scheduled', 'executing', 'retryable') \
             AND args = $3)",
            &[&tenant, &SUBMIT_WORKER, &job_args(company_id, document_id)],
        )
        .await?
        .ok_or(Error::Internal)?
        .get(0))
}

/// The status event an answer leads to, with what the authority said.
fn event_with(name: &'static str, answer: &Answer) -> NewEvent {
    NewEvent {
        event: name,
        reason: answer.reason.clone(),
        http_status: answer.http_status,
        response: answer.response.clone(),
        ..NewEvent::default()
    }
}

fn unsendable(reason: String) -> NewEvent {
    let mut event = NewEvent::new("unsendable");
    event.reason = Some(reason);
    event
}

/// An audit action with its payload, in the transaction that records the
/// outcome. The payload names identifiers and outcomes, never a payload, a
/// secret or a party's details.
pub(crate) async fn action(
    tx: &MutationTransaction<'_>,
    event: &str,
    payload: Value,
) -> bilibi_database::Result<()> {
    let who = Attribution::from_context(tx.context())?;
    bilibi_audit::record_action(tx.sql(), &who, None, event, payload, false).await?;
    Ok(())
}

/// An outcome's audit action: the document's identifiers with what the
/// authority answered.
async fn answer_action(
    tx: &MutationTransaction<'_>,
    event: &str,
    base: &Value,
    answer: &Answer,
) -> bilibi_database::Result<()> {
    let mut payload: Map<String, Value> = base.as_object().cloned().unwrap_or_default();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            payload.insert(key.into(), value);
        }
    };
    put("http_status", answer.http_status.map(Value::from));
    put(
        "submission_uid",
        answer.submission_uid.clone().map(Value::from),
    );
    put(
        "document_uuid",
        answer.document_uuid.clone().map(Value::from),
    );
    put("reason", answer.reason.clone().map(Value::from));
    payload.retain(|_, value| !value.is_null());
    action(tx, event, Value::Object(payload)).await
}

fn retry_after_reason(answer: &Answer) -> String {
    match answer.retry_after {
        Some(seconds) => format!("rate limited; retry after {seconds} seconds"),
        None => "rate limited".into(),
    }
}

/// The authority's validation steps that failed, each with its error's code
/// and text and the specific rules broken (`innerError`), in the first 300
/// characters.
fn invalid_reason(answer: &Answer) -> Option<String> {
    let steps = answer
        .response
        .as_ref()
        .and_then(|response| response.get("validationResults"))
        .and_then(|results| results.get("validationSteps"))
        .and_then(Value::as_array);
    let Some(steps) = steps else {
        return answer.reason.clone();
    };
    let error_text = |error: &Value| -> Option<String> {
        let code = error
            .get("errorCode")
            .or_else(|| error.get("code"))
            .and_then(Value::as_str);
        let text = error
            .get("error")
            .or_else(|| error.get("message"))
            .and_then(Value::as_str);
        let joined = [code, text]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let cleaned = without_references(&joined);
        (!cleaned.is_empty()).then_some(cleaned)
    };
    let summary = steps
        .iter()
        .filter(|step| {
            !matches!(
                step.get("status").and_then(Value::as_str),
                Some("Valid" | "valid")
            )
        })
        .filter_map(|step| {
            let error = step.get("error").cloned().unwrap_or(Value::Null);
            let mut parts: Vec<String> = step
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect();
            parts.extend(error_text(&error));
            if let Some(inner) = error.get("innerError") {
                let inner = match inner {
                    Value::Array(items) => items.clone(),
                    other => vec![other.clone()],
                };
                parts.extend(inner.iter().filter_map(error_text));
            }
            let line = parts.join(": ");
            (!line.is_empty()).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let summary: String = summary.chars().take(REASON_LENGTH).collect();
    (!summary.is_empty()).then_some(summary)
}

/// The text without the authority's "Please refer to <link>" pointers.
fn without_references(text: &str) -> String {
    const POINTER: &str = "Please refer to ";
    let mut text = text.to_owned();
    while let Some(at) = text.find(POINTER) {
        let after = at + POINTER.len();
        let Some(length) = text[after..]
            .find(char::is_whitespace)
            .or_else(|| (after < text.len()).then_some(text.len() - after))
            .filter(|length| *length > 0)
        else {
            break;
        };
        let start = text[..at].trim_end().len();
        text.replace_range(start..after + length, "");
    }
    text
}

fn refusal_name(code: CancelRefusal) -> &'static str {
    match code {
        CancelRefusal::PeriodOver => "period_over",
        CancelRefusal::IncorrectState => "incorrect_state",
        CancelRefusal::ActiveReferencingDocuments => "active_referencing_documents",
        CancelRefusal::Other => "other",
    }
}

fn prepare_code(error: &EinvoiceError) -> &'static str {
    match error {
        EinvoiceError::NotSubmittable => "not_submittable",
        EinvoiceError::MissingCredentials => "missing_credentials",
        EinvoiceError::SigningKeyRequired => "signing_key_required",
        _ => "settings_unreadable",
    }
}

fn payload_code(error: PayloadError) -> &'static str {
    match error {
        PayloadError::SigningKeyRequired => "signing_key_required",
        PayloadError::UnsupportedVersion => "unsupported_version",
        PayloadError::SigningFailed => "signing_failed",
    }
}

fn retry_database() -> Outcome {
    Outcome::Retry("database_unavailable")
}

/// A refusal that ends a job: the person, the company or the document is no
/// longer what it was when the job was queued.
fn cancel_for(error: EinvoiceError) -> Outcome {
    match error {
        EinvoiceError::Unauthorized => Outcome::Cancel("unauthorized"),
        EinvoiceError::CompanyNotFound => Outcome::Cancel("company_not_found"),
        EinvoiceError::CompanyArchived => Outcome::Cancel("company_archived"),
        EinvoiceError::DocumentNotFound => Outcome::Cancel("document_not_found"),
        _ => retry_database(),
    }
}

/// A ledger owner's module: a descriptor ID (`finance/receivables`), at most
/// 100 characters.
fn owner_module(text: &str) -> bool {
    let mut parts = text.split('/');
    let segment = |part: Option<&str>| {
        part.is_some_and(|part| {
            part.starts_with(|c: char| c.is_ascii_lowercase())
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    };
    text.len() <= 100 && segment(parts.next()) && segment(parts.next()) && parts.next().is_none()
}

/// A ledger owner's record: a kind and a positive integer (`document:17`), at
/// most 100 characters.
fn owner_record(text: &str) -> bool {
    let Some((kind, number)) = text.split_once(':') else {
        return false;
    };
    text.len() <= 100
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && number.starts_with(|c: char| ('1'..='9').contains(&c))
        && number.chars().all(|c| c.is_ascii_digit())
}
