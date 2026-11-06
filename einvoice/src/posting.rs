//! The bridge from a cleared document to the company's ledger (Bilimbi
//! `Bilimbi.Finance.Einvoice.Posting` and `Clearance.retry_posting/3`,
//! `retry_reversal/4`).
//!
//! A document the authority validated posts one balanced entry through the
//! ledger's `post_entry_in`, as the person the work runs for, dated the
//! document's issue date (a consolidated document's, the last day of the month
//! it reports, so the month's revenue stays in that month), with the
//! document's code number and authority UUID in the entry's `reference`. The
//! accounts come from the company's `finance.einvoice.posting.*` settings, by
//! account code in its own chart; nothing here names an account, and the
//! authority's `posting_rule` says which settings each document type posts
//! to. The counter-account (receivable, payable or settlement) takes the
//! rule's side for the gross amount; the net (revenue or expense) and tax
//! accounts take the other side. A tax of zero posts no tax line.
//!
//! The entry names the document as its owner (`finance/einvoice`,
//! `document:<id>`), so the ledger refuses a second entry for it and a
//! reversal from anywhere but here. Its ID is recorded on a `posted` status
//! event, and a cancelled document's entry is reversed with
//! `reverse_owned_entry` and recorded on a `posting_reversed` event, so an
//! auditor walks document to UUID to entry. A document issued with a source
//! (another module already recognised its transaction) never posts and never
//! reverses: its accounting, and its correction, are that module's.
//!
//! An entry and the event naming it are one transaction under the company's
//! writable lock, so a document never has an entry without its `posted`
//! event, and the database refuses a second `posted` or `posting_reversed`
//! event. A posting that cannot happen (accounts unset, book not open,
//! currency mismatch, missing grant, a ledger refusal) is recorded as
//! `posting_failed` or `reversal_failed` with its reason, in a transaction of
//! its own once the first has rolled back, and the reconciliation read lists
//! the document until a retry succeeds. Only a database failure that may pass
//! at once (a lost connection, lock contention, a serialization failure) is
//! tried a second time, as a fresh transaction, and recorded as one failure
//! that says it was tried twice; every ledger refusal is final, and a closed
//! period is never reopened to post.
use crate::clearance::Writer;
use crate::documents::{self, NewEvent, Recorded, VALID_STATUSES};
use crate::{
    authorize, Authority, DocumentView, Einvoice, EinvoiceError, PostingAccount, Reconciliation,
    Result, Side, SUBMIT, VIEW,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{
    Entry, EntryOwner, LedgerError, Money, NewEntry, NewLine, PostOptions, Reversal, POST_ENTRIES,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde_json::{json, Map, Value};
use tokio_postgres::error::SqlState;

/// A status event keeps at most this much of a reason.
const REASON_LENGTH: usize = 300;

/// What a posting or reversal attempt came to.
pub(crate) enum Settled {
    /// Nothing to do: no such document, one linked to its source, or one that
    /// already has its entry (or its reversal).
    Unchanged,
    /// The entry (or its reversal) was posted and its event recorded.
    Posted,
    /// Recorded as the failure event, with its reason.
    Failed(String),
}

/// Why one attempt did not post.
enum Refused {
    /// A failure that may pass at once: tried once more.
    Transient(String),
    Final(String),
}

/// Which follow-up an attempt makes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bridge {
    Post,
    Reverse,
}

impl Bridge {
    fn failure_event(self) -> &'static str {
        match self {
            Bridge::Post => "posting_failed",
            Bridge::Reverse => "reversal_failed",
        }
    }
}

impl Einvoice {
    /// Posts a validated document's entry, or reverses a cancelled one's, and
    /// records the outcome as an event. The document is read again under the
    /// company's lock, so one that already has its entry (or reversal) is
    /// `Unchanged`, whatever view of it the caller held. Runs outside any
    /// transaction of the caller's: the ledger's refusal rolls back the one it
    /// joins, and the failure is recorded afterwards.
    pub(crate) async fn bridge(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        bridge: Bridge,
        on: Date,
        writer: Writer<'_>,
    ) -> Settled {
        let mut outcome = self
            .bridge_once(scope, company_id, document_id, bridge, on, writer)
            .await;
        if let Err(Refused::Transient(_)) = outcome {
            outcome = self
                .bridge_once(scope, company_id, document_id, bridge, on, writer)
                .await;
        }
        let message = match outcome {
            Ok(Some(_)) => return Settled::Posted,
            Ok(None) => return Settled::Unchanged,
            Err(Refused::Transient(message)) => {
                format!("a database failure, tried twice: {message}")
            }
            Err(Refused::Final(message)) => message,
        };
        let message: String = message.chars().take(REASON_LENGTH).collect();
        let mut event = NewEvent::new(bridge.failure_event());
        event.reason = Some(message.clone());
        // The failure is recorded only if the database can take it; either
        // way the caller hears it.
        let _ = self
            .record_follow_up(scope, company_id, document_id, event, writer)
            .await;
        Settled::Failed(message)
    }

    async fn bridge_once(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        bridge: Bridge,
        on: Date,
        writer: Writer<'_>,
    ) -> std::result::Result<Option<Entry>, Refused> {
        let mut client = self
            .db
            .get()
            .await
            .map_err(|error| database(error.into()))?;
        let action = match bridge {
            Bridge::Post => "finance.einvoice.post_document",
            Bridge::Reverse => "finance.einvoice.reverse_document",
        };
        let mut tx = self
            .begin(&mut client, scope, company_id, writer, action)
            .await
            .map_err(refused)?;
        let current = documents::fetch(tx.sql(), scope.tenant(), company_id, document_id)
            .await
            .map_err(database)?;
        let Some(current) = current.filter(|current| !current.recognised_by_source()) else {
            return Ok(None);
        };
        let done = match bridge {
            Bridge::Post => current.ledger_entry_id().is_some(),
            Bridge::Reverse => {
                current.ledger_entry_id().is_none() || current.reversal_entry_id().is_some()
            }
        };
        if done {
            return Ok(None);
        }
        let entry = match bridge {
            Bridge::Post => {
                self.post_entry(&mut tx, scope, company_id, &current)
                    .await?
            }
            Bridge::Reverse => {
                self.reverse_entry(&mut tx, scope, company_id, &current, on)
                    .await?
            }
        };
        let mut event = NewEvent::new(match bridge {
            Bridge::Post => "posted",
            Bridge::Reverse => "posting_reversed",
        });
        event.ledger_entry_id = Some(entry.id);
        event.reason = Some(format!("entry {}", entry.entry_number));
        documents::record_event(&mut tx, scope, company_id, document_id, event)
            .await
            .map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(Some(entry))
    }

    async fn post_entry(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        current: &Recorded,
    ) -> std::result::Result<Entry, Refused> {
        let document = &current.document;
        if current.document_uuid().is_none() || !VALID_STATUSES.contains(&current.status()) {
            return Err(Refused::Final("the document is not validated".into()));
        }
        let book = self
            .ledger
            .get_book_on(tx.sql(), scope, company_id)
            .await
            .map_err(ledger_refusal)?;
        if book.currency_code != document.currency_code {
            return Err(Refused::Final(format!(
                "the book is kept in {}, the document in {}",
                book.currency_code, document.currency_code
            )));
        }
        let authority = Self::authority();
        let rule = authority
            .posting_rule(&document.document_type)
            .ok_or_else(|| {
                Refused::Final(format!(
                    "document type {} has no posting rule",
                    document.document_type
                ))
            })?;
        let accounts = self
            .posting_accounts(tx, scope, company_id, [rule.counter, rule.net, rule.tax])
            .await?;
        let opposite = match rule.side {
            Side::Debit => Side::Credit,
            Side::Credit => Side::Debit,
        };
        let mut lines = Vec::new();
        for (account, side, amount, description) in [
            (accounts[0], rule.side, document.payable_amount, None),
            (
                accounts[1],
                opposite,
                document.tax_exclusive_amount,
                Some("Tax-exclusive amount"),
            ),
            (accounts[2], opposite, document.tax_amount, Some("Tax")),
        ] {
            if amount.is_zero() {
                continue;
            }
            let money = Money::new(amount, book.minor_units)
                .map_err(|error| Refused::Final(error.to_string()))?;
            let line = match side {
                Side::Debit => NewLine::debit(account, money),
                Side::Credit => NewLine::credit(account, money),
            };
            lines.push(match description {
                Some(text) => line.described(text),
                None => line,
            });
        }
        if lines.len() < 2 {
            return Err(Refused::Final("the document's amounts are zero".into()));
        }
        let entry_date = match &document.details.billing_period_end {
            Some(end) if current.consolidated() => end
                .parse::<Date>()
                .map_err(|_| Refused::Final("the billing period's end is not a date".into()))?,
            _ => utc_date(document.issued_at),
        };
        self.ledger
            .post_entry_in(
                tx,
                scope,
                company_id,
                &NewEntry {
                    entry_date,
                    description: description(current),
                    reference: Some(reference(current)),
                    lines,
                },
                &PostOptions {
                    owner: Some(owner(document.id)),
                    settlement_account_ids: Vec::new(),
                },
            )
            .await
            .map_err(ledger_refusal)
    }

    async fn reverse_entry(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        current: &Recorded,
        on: Date,
    ) -> std::result::Result<Entry, Refused> {
        let entry_id = current
            .ledger_entry_id()
            .ok_or_else(|| Refused::Final("the document has no entry".into()))?;
        self.ledger
            .reverse_owned_entry(
                tx,
                scope,
                company_id,
                entry_id,
                &owner(current.document.id),
                &Reversal {
                    entry_date: on,
                    description: format!("Reversal of {}", description(current)),
                    reference: Some(reference(current)),
                },
            )
            .await
            .map_err(ledger_refusal)
    }

    /// The ledger accounts of the rule's three settings, by the company's
    /// account codes: every setting must be set and name an account of the
    /// chart.
    async fn posting_accounts(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        needed: [PostingAccount; 3],
    ) -> std::result::Result<[i64; 3], Refused> {
        let keys: Vec<String> = needed
            .iter()
            .map(|account| format!("finance.einvoice.posting.{}", account.field()))
            .collect();
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let target = crate::clearance::company_target(company_id).map_err(refused)?;
        let values = self
            .settings
            .resolve_many(tx.sql(), scope, target, &key_refs)
            .await
            .map_err(database)?;
        let codes: Vec<Option<String>> = keys
            .iter()
            .map(|key| {
                values
                    .get(key)
                    .and_then(|resolved| crate::text(&resolved.value))
            })
            .collect();
        let missing: Vec<&str> = needed
            .iter()
            .zip(&codes)
            .filter(|(_, code)| code.is_none())
            .map(|(account, _)| match account.field() {
                "receivable_account" => "receivable",
                "revenue_account" => "income",
                "tax_account" => "tax",
                "settlement_account" => "settlement",
                "payable_account" => "payable",
                "expense_account" => "expense",
                _ => "purchase tax",
            })
            .collect();
        if !missing.is_empty() {
            let (last, rest) = missing.split_last().unwrap_or((&"", &[]));
            let names = if rest.is_empty() {
                format!("{last} account")
            } else {
                format!("{} and {last} accounts", rest.join(", "))
            };
            return Err(Refused::Final(format!(
                "Choose the {names} in the e-invoice settings."
            )));
        }
        let chart = self
            .ledger
            .list_accounts_on(tx.sql(), scope, company_id)
            .await
            .map_err(ledger_refusal)?;
        let mut ids = [0; 3];
        let mut unknown = Vec::new();
        let mut groups = Vec::new();
        for (index, (account, code)) in needed.iter().zip(&codes).enumerate() {
            let code = code.as_deref().unwrap_or_default();
            match chart.iter().find(|entry| entry.code == code) {
                Some(found) if found.is_group => groups.push(format!("{} {code}", account.field())),
                Some(found) => ids[index] = found.id,
                None => unknown.push(format!("{} {code}", account.field())),
            }
        }
        if !unknown.is_empty() {
            return Err(Refused::Final(format!(
                "accounts not in the chart: {}",
                unknown.join(", ")
            )));
        }
        if !groups.is_empty() {
            return Err(Refused::Final(format!(
                "These are account groups, not accounts that take postings: {}",
                groups.join(", ")
            )));
        }
        Ok(ids)
    }

    /// Records one follow-up event in a transaction of its own.
    async fn record_follow_up(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        event: NewEvent,
        writer: Writer<'_>,
    ) -> Result<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                writer,
                "finance.einvoice.record_posting",
            )
            .await?;
        documents::record_event(&mut tx, scope, company_id, document_id, event).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The documents whose ledger side does not match their status
    /// (`finance.einvoice.view`), newest first: `unposted`, valid (or
    /// buyer-rejected) documents without their entry, with the reason the last
    /// posting failed if one did, and `unreversed`, cancelled documents whose
    /// entry stands. A document linked to its source never posts, so it is
    /// never listed.
    pub async fn reconciliation(&self, scope: &Scope, company_id: i64) -> Result<Reconciliation> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(
            documents::reconciliation(&**client, &Self::authority(), scope.tenant(), company_id)
                .await?,
        )
    }

    /// Posts the ledger entry of a valid document whose posting failed or was
    /// never attempted, once its cause is fixed (`finance.einvoice.documents.submit`
    /// and the ledger's `finance.ledger.entries.post`, so a refused person
    /// leaves no failed event behind). A document that already has its entry,
    /// or is linked to its source, comes back unchanged; one that is neither
    /// valid nor rejected is `NotPostable`; a posting that fails again is
    /// recorded as `posting_failed` and is `LedgerFollowUpFailed(reason)`.
    /// Each try is the audit action `finance.einvoice.posting.retried`.
    pub async fn retry_posting(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<DocumentView> {
        self.retry(
            scope,
            company_id,
            document_id,
            Bridge::Post,
            Timestamp::now(),
        )
        .await
    }

    /// Reverses the ledger entry of a cancelled document whose reversal failed
    /// or was never attempted, dated today (UTC). The same grants, results and
    /// audit action as [`Einvoice::retry_posting`]; `NotReversible` for a
    /// document that is not cancelled.
    pub async fn retry_reversal(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<DocumentView> {
        self.retry(
            scope,
            company_id,
            document_id,
            Bridge::Reverse,
            Timestamp::now(),
        )
        .await
    }

    async fn retry(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
        bridge: Bridge,
        now: Timestamp,
    ) -> Result<DocumentView> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, SUBMIT).await?;
        authorize(&**client, scope, company_id, POST_ENTRIES).await?;
        drop(client);
        self.ensure_writable(scope, company_id, Writer::Session(SUBMIT))
            .await?;
        let current = self.fetch(scope, company_id, document_id).await?;
        let status = current.status();
        let unchanged = current.recognised_by_source()
            || match bridge {
                Bridge::Post => current.ledger_entry_id().is_some(),
                Bridge::Reverse => {
                    current.ledger_entry_id().is_none() || current.reversal_entry_id().is_some()
                }
            };
        if unchanged {
            return self.view(scope, company_id, document_id).await;
        }
        match bridge {
            Bridge::Post if !VALID_STATUSES.contains(&status) => {
                return Err(EinvoiceError::NotPostable);
            }
            Bridge::Reverse if status != "cancelled" => return Err(EinvoiceError::NotReversible),
            _ => {}
        }
        let settled = self
            .bridge(
                scope,
                company_id,
                document_id,
                bridge,
                utc_date(now),
                Writer::Session(SUBMIT),
            )
            .await;
        let (outcome, reason) = match &settled {
            Settled::Posted => ("succeeded", None),
            Settled::Unchanged => ("unchanged", None),
            Settled::Failed(message) => ("failed", Some(message.clone())),
        };
        let document = &current.document;
        let mut payload = Map::new();
        payload.insert("document_id".into(), json!(document.id));
        payload.insert("code_number".into(), json!(document.code_number));
        payload.insert("environment".into(), json!(document.environment));
        payload.insert(
            "operation".into(),
            json!(match bridge {
                Bridge::Post => "posting",
                Bridge::Reverse => "reversal",
            }),
        );
        payload.insert("outcome".into(), json!(outcome));
        if let Some(reason) = &reason {
            payload.insert("reason".into(), json!(reason));
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                Writer::Session(SUBMIT),
                "finance.einvoice.retry_posting",
            )
            .await?;
        crate::clearance::action(
            &tx,
            "finance.einvoice.posting.retried",
            Value::Object(payload),
        )
        .await?;
        tx.commit().await?;
        match reason {
            Some(reason) => Err(EinvoiceError::LedgerFollowUpFailed(reason)),
            None => self.view(scope, company_id, document_id).await,
        }
    }
}

/// How this module names itself to the ledger as the owner of a document's
/// entry.
pub(crate) fn owner(document_id: i64) -> EntryOwner {
    EntryOwner {
        module: crate::MODULE.into(),
        record: format!("document:{document_id}"),
    }
}

/// The calendar date of an instant in UTC.
pub(crate) fn utc_date(at: Timestamp) -> Date {
    at.to_zoned(TimeZone::UTC).date()
}

fn description(current: &Recorded) -> String {
    let document = &current.document;
    format!(
        "E-invoice {} ({})",
        document.code_number,
        Einvoice::authority()
            .document_type_name(&document.document_type)
            .to_lowercase()
    )
}

// Code number (at most 50 characters) and UUID (26) fit the reference's 100.
fn reference(current: &Recorded) -> String {
    format!(
        "{} {}",
        current.document.code_number,
        current.document_uuid().unwrap_or_default()
    )
}

/// A ledger refusal is final; a database failure that may pass is not.
fn ledger_refusal(error: LedgerError) -> Refused {
    match error {
        LedgerError::Failed(error) => database(error),
        LedgerError::Invalid(errors) => Refused::Final(
            errors
                .iter()
                .map(|error| match error.line {
                    Some(line) => format!("line {line} {} {}", error.field, error.message),
                    None => format!("{} {}", error.field, error.message),
                })
                .collect::<Vec<_>>()
                .join("; "),
        ),
        LedgerError::Unauthorized => Refused::Final("the actor may not post to the ledger".into()),
        LedgerError::BookNotOpen => Refused::Final("the company's book is not open".into()),
        LedgerError::CompanyArchived => Refused::Final("the company is archived".into()),
        other => Refused::Final(other.to_string()),
    }
}

fn refused(error: EinvoiceError) -> Refused {
    match error {
        EinvoiceError::Failed(error) => database(error),
        EinvoiceError::Unauthorized => {
            Refused::Final("the actor may not post to the ledger".into())
        }
        EinvoiceError::CompanyArchived => Refused::Final("the company is archived".into()),
        other => Refused::Final(other.to_string()),
    }
}

/// Whether a database failure may pass on its own: a lost connection or pool,
/// lock contention, a deadlock, a serialization failure or a cancelled query.
fn database(error: Error) -> Refused {
    let message = error.to_string();
    let transient = match &error {
        Error::Pool(_) => true,
        Error::Database(_) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            let mut found = false;
            while let Some(current) = source {
                if let Some(postgres) = current.downcast_ref::<tokio_postgres::Error>() {
                    found = postgres.is_closed()
                        || postgres.code().is_some_and(|code| {
                            [
                                SqlState::LOCK_NOT_AVAILABLE,
                                SqlState::T_R_SERIALIZATION_FAILURE,
                                SqlState::T_R_DEADLOCK_DETECTED,
                                SqlState::QUERY_CANCELED,
                            ]
                            .contains(code)
                        });
                    break;
                }
                source = current.source();
            }
            found
        }
        _ => false,
    };
    let detail = match &error {
        Error::Database(_) | Error::Pool(_) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            let mut text = message.clone();
            while let Some(current) = source {
                if let Some(postgres) = current.downcast_ref::<tokio_postgres::Error>() {
                    text = postgres
                        .as_db_error()
                        .map_or_else(|| postgres.to_string(), |db| db.message().to_owned());
                    break;
                }
                source = current.source();
            }
            text
        }
        _ => message,
    };
    if transient {
        Refused::Transient(detail)
    } else {
        Refused::Final(detail)
    }
}
