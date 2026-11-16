//! Attaching, replacing, removing and reading source documents, and the
//! company's retention setting.
use crate::{
    files, owner, retain,
    retention::{self, DEFAULT_YEARS},
    source::SourceClient,
    store::{self, Filter, NewAttachment},
    Attachment, AttachmentError, AttachmentFile, AttachmentQuery, Attachments, Capture, FieldError,
    Registered, Result, Retention, RetentionChange, SourceDocument, SourceKind, Status, CREATE,
    DELETE, MANAGE_RETENTION, VIEW,
};
use bilibi_artifacts::Retention as StorageRetention;
use bilibi_company::WriteError;
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use jiff::{civil::Date, Timestamp};
use serde_json::Value;
use std::time::SystemTime;
use tokio_postgres::{Client, GenericClient};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

/// The actor's current grant and company reach decide every call.
pub(crate) async fn authorize(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company_id, capability)
        .await
        .map(|_| ())
        .map_err(crate::refusal_of)
}

fn positive(id: i64) -> bool {
    id > 0
}

impl Attachments {
    /// Every write: the capability and company reach now, then the audited
    /// transaction holding the company's row lock (Bilimbi `write/4` with
    /// `lock_writable_company/2`).
    pub(crate) async fn write<'c>(
        &self,
        client: &'c mut Client,
        scope: &Scope,
        company_id: i64,
        capability: &str,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        authorize(&*client, scope, company_id, capability).await?;
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        lock_writable(&tx, scope, company_id).await?;
        Ok(tx)
    }

    pub(crate) fn registered(&self, source_id: &str) -> Result<&Registered> {
        self.sources
            .get(source_id)
            .ok_or(AttachmentError::InvalidSource)
    }

    pub(crate) async fn source_document(
        &self,
        client: SourceClient<'_>,
        scope: &Scope,
        company_id: i64,
        source: &Registered,
        document_id: i64,
    ) -> Result<SourceDocument> {
        if !positive(document_id) {
            return Err(AttachmentError::NotFound);
        }
        crate::on_source_client!(client, |sql| self
            .authorize_source(sql, scope, company_id, source)
            .await)?;
        Ok(source
            .source
            .document(client, scope, company_id, document_id)
            .await?)
    }

    async fn authorize_source(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        source: &Registered,
    ) -> Result<()> {
        for capability in source.source.view_capabilities() {
            authorize(client, scope, company_id, capability).await?;
        }
        Ok(())
    }

    /// An attachment the caller may read: the source must be mounted and its
    /// view capabilities held.
    pub(crate) async fn readable_attachment(
        &self,
        client: &Client,
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
    ) -> Result<(Attachment, &Registered)> {
        let attachment = self
            .any_attachment(client, scope, company_id, attachment_id)
            .await?;
        let source = self.source_of(&attachment)?;
        self.authorize_source(client, scope, company_id, source)
            .await?;
        Ok((attachment, source))
    }

    pub(crate) async fn any_attachment(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
    ) -> Result<Attachment> {
        if !positive(attachment_id) {
            return Err(AttachmentError::AttachmentNotFound);
        }
        store::fetch(client, scope.tenant(), company_id, attachment_id)
            .await?
            .map(|attachment| self.present(attachment))
            .ok_or(AttachmentError::AttachmentNotFound)
    }

    pub(crate) fn source_of(&self, attachment: &Attachment) -> Result<&Registered> {
        self.sources
            .get(&attachment.source)
            .ok_or(AttachmentError::SourceUnavailable)
    }

    /// Names the kind of record, as its source does.
    pub(crate) fn present(&self, mut attachment: Attachment) -> Attachment {
        attachment.source_label = self
            .sources
            .get(&attachment.source)
            .map(|registered| registered.source.label().to_owned());
        attachment
    }

    async fn standing_attachment(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
    ) -> Result<(Attachment, &Registered)> {
        let attachment = self
            .any_attachment(client, scope, company_id, attachment_id)
            .await?;
        if attachment.status != Status::Standing {
            return Err(AttachmentError::AttachmentNotStanding);
        }
        let source = self.source_of(&attachment)?;
        Ok((attachment, source))
    }

    /// The sources of the company's attachments the caller can read.
    pub(crate) async fn readable_sources(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<String>> {
        let mut readable = Vec::new();
        for source in store::sources(client, scope.tenant(), company_id).await? {
            if let Some(registered) = self.sources.get(&source) {
                if self
                    .authorize_source(client, scope, company_id, registered)
                    .await
                    .is_ok()
                {
                    readable.push(source);
                }
            }
        }
        Ok(readable)
    }

    pub(crate) async fn retention_years(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company_id: i64,
    ) -> Result<i32> {
        Ok(store::latest_change(client, scope.tenant(), company_id)
            .await?
            .map_or(DEFAULT_YEARS, |change| change.years))
    }

    // ---- Retention setting

    /// The company's retention (`finance.attachments.view`): the years and the
    /// latest change, or none for a company that keeps the default.
    pub async fn get_retention(&self, scope: &Scope, company_id: i64) -> Result<Retention> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let change = store::latest_change(&**client, scope.tenant(), company_id).await?;
        Ok(Retention {
            years: change.as_ref().map_or(DEFAULT_YEARS, |c| c.years),
            change,
        })
    }

    /// Lengthens the company's retention to `years` with a `reason`
    /// (`finance.attachments.retention.manage`). The change applies to
    /// documents captured afterwards; an attachment already captured keeps its
    /// retain-until unless [`Attachments::extend_retention`] moves it. Fewer or
    /// the same years is `RetentionNotLengthened`.
    pub async fn set_retention(
        &self,
        scope: &Scope,
        company_id: i64,
        years: i32,
        reason: &str,
    ) -> Result<Retention> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                MANAGE_RETENTION,
                "finance.attachments.set_retention",
            )
            .await?;
        let tenant = scope.tenant();
        let reason = reason.trim().to_owned();
        // Bilimbi asks whether the years lengthen retention before it reads
        // the changeset's errors, which it lists by field (reason, years).
        let current = self.retention_years(tx.sql(), scope, company_id).await?;
        if years <= current {
            return Err(AttachmentError::RetentionNotLengthened);
        }
        let mut errors = reason_errors(&reason);
        if years > 100 {
            errors.push(FieldError::new(
                "years",
                "must be less than or equal to 100",
            ));
        }
        if !errors.is_empty() {
            return Err(AttachmentError::Invalid(errors));
        }
        let (user, impersonator) = actor_ids(scope);
        let change = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let change = store::insert_change(
                        sql,
                        tenant,
                        company_id,
                        years,
                        &reason,
                        user,
                        impersonator,
                    )
                    .await?;
                    let json = serde_json::to_value(&change).map_err(|_| Error::Internal)?;
                    let record = Change {
                        kind: "finance.attachments.retention_change".into(),
                        id: change.id,
                        old: Value::Null,
                        new: json,
                    };
                    Ok((change, vec![record]))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(Retention {
            years: change.years,
            change: Some(change),
        })
    }

    /// Every change of the company's retention, oldest first
    /// (`finance.attachments.view`).
    pub async fn list_retention_changes(
        &self,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Vec<RetentionChange>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(store::changes(&**client, scope.tenant(), company_id).await?)
    }

    /// The retain-until a document dated `date` gets now: the company's
    /// retention years after the end of the financial year the date falls in,
    /// at the end of that day in UTC. Needs `finance.attachments.view` and,
    /// because the financial year is the ledger's, `finance.ledger.view`.
    pub async fn retain_until(
        &self,
        scope: &Scope,
        company_id: i64,
        date: Date,
    ) -> Result<Timestamp> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        authorize(&**client, scope, company_id, bilibi_finance_ledger::VIEW).await?;
        let years = self.retention_years(&**client, scope, company_id).await?;
        Ok(retention::until(retention::financial_year_end(date), years))
    }

    // ---- Attaching

    /// Keeps `file` as a source document of the record `document_id` that
    /// `source` describes (`finance.attachments.create`): stores the bytes as a
    /// records-grade artifact retained as [`Attachments::retain_until`] says for
    /// the record's date, then records the attachment with who captured it and
    /// when. A record that is final takes more attachments; nothing is
    /// replaced.
    ///
    /// Refusals: the source's own (`NotFound`, `Unauthorized`, ...), the
    /// file's (`InvalidFileName`, `EmptyFile`, `FileTooLarge`,
    /// `InvalidContentType`) and Base Artifacts' storage refusals.
    pub async fn attach(
        &self,
        scope: &Scope,
        company_id: i64,
        source: &str,
        document_id: i64,
        file: &files::NewFile,
    ) -> Result<Attachment> {
        let capture = {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, CREATE).await?;
            let registered = self.registered(source)?;
            let document = self
                .source_document(
                    SourceClient::Client(&client),
                    scope,
                    company_id,
                    registered,
                    document_id,
                )
                .await?;
            drop(client);
            self.store_file(scope, company_id, registered, file, document.date)
                .await?
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                CREATE,
                "finance.attachments.attach",
            )
            .await?;
        let attachment = self
            .link(&mut tx, scope, company_id, source, document_id, &capture)
            .await?;
        tx.commit().await?;
        Ok(attachment)
    }

    /// Stores `file` as a records-grade artifact for a record of `source`
    /// dated `document_date`, before the transaction that creates the record
    /// (`finance.attachments.create`). The capture it returns is handed to
    /// [`Attachments::attach_capture`] inside that transaction.
    pub async fn capture_file(
        &self,
        scope: &Scope,
        company_id: i64,
        source: &str,
        file: &files::NewFile,
        document_date: Date,
    ) -> Result<Capture> {
        {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, CREATE).await?;
        }
        let registered = self.registered(source)?;
        self.store_file(scope, company_id, registered, file, document_date)
            .await
    }

    async fn store_file(
        &self,
        scope: &Scope,
        company_id: i64,
        source: &Registered,
        file: &files::NewFile,
        document_date: Date,
    ) -> Result<Capture> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, CREATE).await?;
        authorize(&**client, scope, company_id, bilibi_finance_ledger::VIEW).await?;
        // An archived company takes no document: refuse before any byte is stored.
        ensure_writable(&**client, scope, company_id, false).await?;
        let valid = files::validate(file)?;
        let years = self.retention_years(&**client, scope, company_id).await?;
        let year_end = retention::financial_year_end(document_date);
        let until = retention::until(year_end, years);
        let reason = format!(
            "{}: kept {years} years after the financial year ending {year_end}",
            source.id
        );
        let owner = owner::ArtifactOwner::user();
        let company =
            OrganisationId::new(company_id).map_err(|_| AttachmentError::CompanyNotFound)?;
        let stored = self
            .artifacts
            .for_owner(scope, company, &owner)
            .put_retained(
                &mut client,
                owner::reference(source.id),
                &file.content,
                &valid.content_type,
                StorageRetention {
                    retain_until: SystemTime::from(until),
                    legal_hold: false,
                    reason: Some(reason),
                },
            )
            .await?;
        Ok(Capture {
            source: source.id.to_owned(),
            document_date,
            artifact_id: stored.id,
            file_name: valid.file_name,
            content_type: stored.content_type,
            byte_size: stored.byte_size,
            sha256: stored.sha256,
            retention_years: years,
            retain_until: until,
        })
    }

    /// Records a [`Attachments::capture_file`] capture as an attachment of
    /// `document_id`, which `source` reads back on the caller's transaction (so
    /// a record inserted in it is read). Authorizes `finance.attachments.create`
    /// and holds the company's lock like any write, and joins the caller's
    /// transaction.
    pub async fn attach_capture(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        source: &str,
        document_id: i64,
        capture: &Capture,
    ) -> Result<Attachment> {
        authorize(tx.sql(), scope, company_id, CREATE).await?;
        lock_writable(tx, scope, company_id).await?;
        self.link(tx, scope, company_id, source, document_id, capture)
            .await
    }

    /// Reads the record through its source and inserts the attachment.
    async fn link(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        source: &str,
        document_id: i64,
        capture: &Capture,
    ) -> Result<Attachment> {
        let registered = self.registered(source)?;
        if capture.source != registered.id {
            return Err(AttachmentError::CaptureMismatch);
        }
        let document = self
            .source_document(
                SourceClient::Transaction(tx.sql()),
                scope,
                company_id,
                registered,
                document_id,
            )
            .await?;
        if document.date != capture.document_date {
            return Err(AttachmentError::CaptureMismatch);
        }
        let attachment = self
            .insert(
                tx,
                scope,
                company_id,
                registered,
                document_id,
                &document,
                capture,
            )
            .await?;
        Ok(self.present(attachment))
    }

    async fn insert(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        source: &Registered,
        document_id: i64,
        document: &SourceDocument,
        capture: &Capture,
    ) -> Result<Attachment> {
        let tenant = scope.tenant();
        let (user, impersonator) = actor_ids(scope);
        let (id, module) = (source.id, source.module);
        let (label, date) = (document.label.clone(), document.date);
        let capture = capture.clone();
        Ok(tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let id = store::insert(
                        sql,
                        tenant,
                        company_id,
                        &NewAttachment {
                            source: id,
                            source_module: module,
                            document_id,
                            document_label: &label,
                            document_date: date,
                            artifact_id: &capture.artifact_id,
                            file_name: &capture.file_name,
                            content_type: &capture.content_type,
                            byte_size: capture.byte_size,
                            sha256: &capture.sha256,
                            retention_years: capture.retention_years,
                            retain_until: capture.retain_until,
                            captured_by: user,
                            impersonator,
                        },
                    )
                    .await?;
                    let row = store::fetch(sql, tenant, company_id, id)
                        .await?
                        .ok_or(Error::Internal)?;
                    let change = Change {
                        kind: "finance.attachments.attachment".into(),
                        id,
                        old: Value::Null,
                        new: row.json(),
                    };
                    Ok((row, vec![change]))
                })
            })
            .await?)
    }

    /// Replaces a standing attachment of a record that is not final with
    /// `file` (`finance.attachments.create`): captures the new file as
    /// [`Attachments::attach`] does and records `replaced` on the old, naming
    /// the new. A final record's attachment is `DocumentFinal`; a replaced or
    /// removed one is `AttachmentNotStanding`.
    pub async fn replace(
        &self,
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
        file: &files::NewFile,
    ) -> Result<Attachment> {
        let capture = {
            let client = self.db.get().await.map_err(Error::from)?;
            authorize(&**client, scope, company_id, CREATE).await?;
            let (attachment, source) = self
                .standing_attachment(&**client, scope, company_id, attachment_id)
                .await?;
            let document = self
                .source_document(
                    SourceClient::Client(&client),
                    scope,
                    company_id,
                    source,
                    attachment.document_id,
                )
                .await?;
            changeable(&document)?;
            drop(client);
            self.store_file(scope, company_id, source, file, document.date)
                .await?
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                CREATE,
                "finance.attachments.replace",
            )
            .await?;
        let (old, source) = self
            .standing_attachment(tx.sql(), scope, company_id, attachment_id)
            .await?;
        let document = self
            .source_document(
                SourceClient::Transaction(tx.sql()),
                scope,
                company_id,
                source,
                old.document_id,
            )
            .await?;
        changeable(&document)?;
        if document.date != capture.document_date {
            return Err(AttachmentError::CaptureMismatch);
        }
        let new = self
            .insert(
                &mut tx,
                scope,
                company_id,
                source,
                old.document_id,
                &document,
                &capture,
            )
            .await?;
        let (user, impersonator) = actor_ids(scope);
        let tenant = scope.tenant();
        let (old_id, new_id) = (old.id, new.id);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (id, json) = store::insert_event(
                    sql,
                    tenant,
                    company_id,
                    &store::NewEvent {
                        attachment_id: old_id,
                        event: "replaced",
                        replacement_id: Some(new_id),
                        retain_until: None,
                        reason: None,
                        user,
                        impersonator,
                        principal: None,
                    },
                )
                .await?;
                Ok(((), vec![event_change(id, json)]))
            })
        })
        .await?;
        let replacement = store::fetch(tx.sql(), tenant, company_id, new.id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(self.present(replacement))
    }

    /// Removes a standing attachment of a record that is not final
    /// (`finance.attachments.delete`): an event with its `reason`; the bytes
    /// stay retained and readable from the attachment's history. A final
    /// record's attachment is `DocumentFinal`.
    pub async fn remove(
        &self,
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
        reason: &str,
    ) -> Result<Attachment> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company_id,
                DELETE,
                "finance.attachments.remove",
            )
            .await?;
        let (attachment, source) = self
            .standing_attachment(tx.sql(), scope, company_id, attachment_id)
            .await?;
        let document = self
            .source_document(
                SourceClient::Transaction(tx.sql()),
                scope,
                company_id,
                source,
                attachment.document_id,
            )
            .await?;
        changeable(&document)?;
        let reason = reason.trim().to_owned();
        let mut errors = Vec::new();
        if reason.is_empty() {
            errors.push(FieldError::new("reason", "is required"));
        } else if reason.chars().count() > 500 {
            errors.push(FieldError::new(
                "reason",
                "should be at most 500 character(s)",
            ));
        }
        if !errors.is_empty() {
            return Err(AttachmentError::Invalid(errors));
        }
        let (user, impersonator) = actor_ids(scope);
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (id, json) = store::insert_event(
                    sql,
                    tenant,
                    company_id,
                    &store::NewEvent {
                        attachment_id,
                        event: "removed",
                        replacement_id: None,
                        retain_until: None,
                        reason: Some(&reason),
                        user,
                        impersonator,
                        principal: None,
                    },
                )
                .await?;
                Ok(((), vec![event_change(id, json)]))
            })
        })
        .await?;
        let removed = store::fetch(tx.sql(), tenant, company_id, attachment_id)
            .await?
            .ok_or(Error::Internal)?;
        tx.commit().await?;
        Ok(self.present(removed))
    }

    // ---- Reading

    /// The standing attachments of the record `document_id` that `source`
    /// describes, oldest first (`finance.attachments.view`); none when the
    /// caller cannot read the source's records.
    pub async fn list_attachments(
        &self,
        scope: &Scope,
        company_id: i64,
        source: &str,
        document_id: i64,
    ) -> Result<Vec<Attachment>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let registered = self.registered(source)?;
        if self
            .authorize_source(&**client, scope, company_id, registered)
            .await
            .is_err()
            || !positive(document_id)
        {
            return Ok(Vec::new());
        }
        Ok(store::standing_for_record(
            &**client,
            scope.tenant(),
            company_id,
            registered.id,
            document_id,
        )
        .await?
        .into_iter()
        .map(|attachment| self.present(attachment))
        .collect())
    }

    /// The company's attachments by number, leaving out those of a source the
    /// caller cannot read (`finance.attachments.view`).
    pub async fn list_company_attachments(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &AttachmentQuery,
    ) -> Result<Vec<Attachment>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let status = query.status.as_deref().unwrap_or("standing");
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
        if !(status == "all" || Status::ALL.iter().any(|s| s.as_str() == status))
            || !(1..=MAX_LIMIT).contains(&limit)
            || query.after.is_some_and(|after| after < 0)
            || query
                .document_ids
                .as_ref()
                .is_some_and(|ids| ids.iter().any(|id| *id < 1))
        {
            return Err(AttachmentError::InvalidOption);
        }
        let sources = self.readable_sources(&**client, scope, company_id).await?;
        let rows = store::list(
            &**client,
            scope.tenant(),
            company_id,
            &Filter {
                sources: &sources,
                source: query.source.as_deref(),
                document_ids: query.document_ids.as_deref(),
                status,
                limit,
                after: query.after,
            },
        )
        .await?;
        Ok(rows.into_iter().map(|a| self.present(a)).collect())
    }

    /// The kinds of record the company has attachments for, whatever their
    /// standing, leaving out the sources the caller cannot read: the filter
    /// options of [`Attachments::list_company_attachments`].
    pub async fn list_sources(&self, scope: &Scope, company_id: i64) -> Result<Vec<SourceKind>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        Ok(self
            .readable_sources(&**client, scope, company_id)
            .await?
            .into_iter()
            .map(|source| SourceKind {
                source_label: self.source_label(&source).map(str::to_owned),
                source,
            })
            .collect())
    }

    /// One attachment (`finance.attachments.view`): the record it supports, the
    /// file, its retention (`retain_until` follows the latest extension, and
    /// `on_hold`), who captured it when, its `status` and its `events` oldest
    /// first. The caller must also be able to read the record's source
    /// (`Unauthorized` otherwise).
    pub async fn get_attachment(
        &self,
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
    ) -> Result<Attachment> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let (attachment, _) = self
            .readable_attachment(&client, scope, company_id, attachment_id)
            .await?;
        drop(client);
        self.catch_up(&retain::Who::Scope(scope), scope, company_id, attachment)
            .await
    }

    /// An attachment's bytes, for a download (`finance.attachments.view`).
    /// Re-reads the record through its source (the actor must still be able to
    /// read it) and the bytes through Base Artifacts, which re-authorizes,
    /// checks the SHA-256 and records the read. A replaced or removed
    /// attachment is still read: its bytes are retained.
    pub async fn read_attachment(
        &self,
        scope: &Scope,
        company_id: i64,
        attachment_id: i64,
    ) -> Result<AttachmentFile> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let (attachment, source) = self
            .readable_attachment(&client, scope, company_id, attachment_id)
            .await?;
        self.source_document(
            SourceClient::Client(&client),
            scope,
            company_id,
            source,
            attachment.document_id,
        )
        .await?;
        let owner = owner::ArtifactOwner::user();
        let company =
            OrganisationId::new(company_id).map_err(|_| AttachmentError::CompanyNotFound)?;
        let document = self
            .artifacts
            .for_owner(scope, company, &owner)
            .read(&mut client, &attachment.artifact_id)
            .await?;
        drop(client);
        self.catch_up(
            &retain::Who::Scope(scope),
            scope,
            company_id,
            attachment.clone(),
        )
        .await?;
        Ok(AttachmentFile {
            bytes: document.bytes,
            file_name: attachment.file_name,
            content_type: attachment.content_type,
            byte_size: attachment.byte_size,
            sha256: attachment.sha256,
        })
    }
}

/// A final record keeps its attachments as they are.
fn changeable(document: &SourceDocument) -> Result<()> {
    if document.is_final {
        Err(AttachmentError::DocumentFinal)
    } else {
        Ok(())
    }
}

pub(crate) fn actor_ids(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

pub(crate) fn event_change(id: i64, json: Value) -> Change {
    Change {
        kind: "finance.attachments.attachment_event".into(),
        id,
        old: Value::Null,
        new: json,
    }
}

/// Holds the company's row lock for the transaction: an archived company is
/// refused and the company's attachment writes run one at a time.
pub(crate) async fn lock_writable(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
) -> Result<()> {
    ensure_writable(tx.sql(), scope, company_id, true).await
}

/// Refuses an archived company, optionally holding its row lock.
pub(crate) async fn ensure_writable(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    lock: bool,
) -> Result<()> {
    bilibi_company::require_writable(client, scope, company_id, lock)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            WriteError::NotFound => AttachmentError::CompanyNotFound,
            WriteError::Archived => AttachmentError::CompanyArchived,
            WriteError::Access(error) => crate::refusal_of(error),
            WriteError::InUse | WriteError::Stale | WriteError::Invalid(_) => {
                AttachmentError::Failed(Error::Internal)
            }
        })
}

/// Bilimbi `RetentionChange.changeset`'s reason errors, trimmed.
pub(crate) fn reason_errors(reason: &str) -> Vec<FieldError> {
    let reason = reason.trim();
    if reason.is_empty() {
        vec![FieldError::new("reason", "can't be blank")]
    } else if reason.chars().count() > 500 {
        vec![FieldError::new(
            "reason",
            "should be at most 500 character(s)",
        )]
    } else {
        vec![]
    }
}
