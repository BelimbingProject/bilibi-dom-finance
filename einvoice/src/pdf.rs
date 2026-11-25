//! Private visual copies from the frozen fiscal document and its clearance evidence.
use crate::clearance::{action, Writer};
use crate::documents::{self, NewEvent, StatusEvent};
use crate::{
    authorize, print_layouts::CONTRACT, print_values, Einvoice, EinvoiceError, Result, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_artifacts::{Operation, Owner, PdfOptions, Reference};
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_templates::Origin;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use qrcode::render::svg;
use qrcode::QrCode;
use rsa::sha2::{Digest, Sha256};
use serde::Serialize;
use serde_json::{json, Value};

/// The stable Artifacts owner id.
pub const ARTIFACT_OWNER: &str = "finance/einvoice";
/// The artifact reference kind of a document's PDF; its subject is
/// `document-<id>`.
const KIND: &str = "document-pdf";

/// A stored PDF of a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PdfArtifact {
    pub artifact_id: String,
    pub byte_size: i64,
    pub sha256: String,
}

/// A stored PDF's bytes and download name, for the download route. Holds a
/// private document: no `Debug`.
pub struct PdfFile {
    pub bytes: Vec<u8>,
    pub filename: String,
}

/// An SVG of the QR code for `text`, `size` pixels wide, or `None` for a
/// blank text (Bilimbi `QrCode.svg/2`, Guideline §2.4.4): black on white, with
/// no script or external resource, for a page to embed.
pub fn qr_code_svg(text: &str, size: u32) -> Option<String> {
    if text.is_empty() || size == 0 {
        return None;
    }
    let code = QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color<'_>>()
            .min_dimensions(size, size)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}

/// The module's Artifacts adapter. It renders only the document the facade
/// handed it, built from a view the caller was authorized to read.
pub(crate) struct ArtifactOwner;

fn reference(document_id: i64) -> Reference {
    Reference {
        subject: format!("document-{document_id}"),
        kind: KIND.into(),
    }
}

fn filename(reference: &Reference) -> String {
    match reference.subject.strip_prefix("document-") {
        Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => {
            format!("e-invoice-{id}.pdf")
        }
        _ => "e-invoice.pdf".into(),
    }
}

impl Owner for ArtifactOwner {
    fn owner_id(&self) -> &'static str {
        ARTIFACT_OWNER
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let shaped =
                reference.is_some_and(|r| r.kind == KIND && r.subject.starts_with("document-"));
            let allowed = match operation {
                Operation::Purge => true,
                Operation::Create | Operation::Read | Operation::Delete => shaped,
                Operation::Retain => false,
            };
            if !allowed {
                return Err(Error::Forbidden);
            }
            bilibi_company::authorize_work_target(client, scope, company.get(), VIEW)
                .await
                .map(|_| ())
        })
    }
}

fn source_sequence(events: &[StatusEvent]) -> i32 {
    events
        .iter()
        .rev()
        .find(|event| event.event != "pdf_generated")
        .map_or(0, |event| event.sequence)
}

fn source_indices(records: &[Option<crate::SourceRecord>]) -> Value {
    json!(records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.as_ref().map(|_| index))
        .collect::<Vec<_>>())
}

fn current_pdfs(events: &[StatusEvent]) -> impl Iterator<Item = &Value> {
    let as_of = i64::from(source_sequence(events));
    events
        .iter()
        .rev()
        .filter(|event| event.event == "pdf_generated")
        .filter_map(|event| event.response.as_ref())
        .filter(move |response| response["as_of_sequence"].as_i64() == Some(as_of))
}

struct PreparedPdf {
    inline: bilibi_artifacts::InlineDocument,
    identity: PdfIdentity,
}

#[derive(PartialEq, Eq)]
struct PdfIdentity {
    template_version: String,
    data_version: String,
    source_indices: Value,
}

impl PdfIdentity {
    fn matching<'a>(&'a self, events: &'a [StatusEvent]) -> impl Iterator<Item = PdfArtifact> + 'a {
        current_pdfs(events)
            .filter(move |response| {
                response["data_version"].as_str() == Some(self.data_version.as_str())
                    && response["template_version"].as_str() == Some(self.template_version.as_str())
                    && response["source_indices"] == self.source_indices
            })
            .filter_map(|response| {
                Some(PdfArtifact {
                    artifact_id: response["artifact_id"].as_str()?.to_owned(),
                    byte_size: response["byte_size"].as_i64()?,
                    sha256: response["sha256"].as_str()?.to_owned(),
                })
            })
    }
}

impl Einvoice {
    async fn available_pdf_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        view: &crate::DocumentView,
        identity: &PdfIdentity,
    ) -> Result<Option<PdfArtifact>> {
        let mut matching = identity.matching(&view.events).peekable();
        if matching.peek().is_none() {
            return Ok(None);
        }
        let reference = reference(view.document.id);
        let documents = self
            .artifacts
            .subject_documents(
                client.sql_client().await?,
                scope,
                &ArtifactOwner,
                &reference.subject,
            )
            .await
            .map_err(artifacts)?;
        let now = jiff::Timestamp::now();
        Ok(matching.find(|pdf| {
            documents.iter().any(|(company, document)| {
                company.get() == company_id
                    && document.reference.kind == reference.kind
                    && document.id == pdf.artifact_id
                    && document.expires_at.as_deref().is_none_or(|expiry| {
                        expiry
                            .parse::<jiff::Timestamp>()
                            .is_ok_and(|expiry| expiry > now)
                    })
            })
        }))
    }

    pub(crate) async fn pdf_for_reader_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        view: &crate::DocumentView,
    ) -> Result<Option<String>> {
        if current_pdfs(&view.events).next().is_none() {
            return Ok(None);
        }
        match self.prepare_pdf_on(client, scope, company_id, view).await {
            Ok(prepared) => Ok(self
                .available_pdf_on(client, scope, company_id, view, &prepared.identity)
                .await?
                .map(|pdf| pdf.artifact_id)),
            Err(EinvoiceError::Artifacts(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Renders the document as a PDF through Base Artifacts and records it on
    /// a `pdf_generated` event (`finance.einvoice.view`). The event keeps the
    /// source event and print versions. An unchanged request returns the
    /// recorded artifact. Base Artifacts needs a storage root and a retention
    /// period first (`Artifacts("policy")`). The audit action is
    /// `finance.einvoice.pdf.generated`.
    pub async fn generate_pdf(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<PdfArtifact> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        // Recording the PDF on the document is a write, refused for an
        // archived company before an artifact is stored.
        bilibi_company::require_writable(&**client, scope, company_id, false)
            .await
            .map_err(crate::company_write)?;
        let company =
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?;
        let (view, prepared) = self
            .prepare_document_pdf_on(&**client, scope, company_id, document_id)
            .await?;
        let as_of = source_sequence(&view.events);
        let PreparedPdf { inline, identity } = prepared;
        if let Some(stored) = self
            .available_pdf_on(&**client, scope, company_id, &view, &identity)
            .await?
        {
            return Ok(stored);
        }
        let options = PdfOptions::default()
            .template_version(identity.template_version.clone())
            .data_version(identity.data_version.clone());
        let owned = self.artifacts.for_owner(scope, company, &ArtifactOwner);
        let stored = Box::pin(owned.render_inline_document(
            &mut client,
            reference(document_id),
            inline,
            &options,
        ))
        .await
        .map_err(artifacts)?;
        let artifact = PdfArtifact {
            artifact_id: stored.document.id,
            byte_size: stored.document.byte_size,
            sha256: stored.document.sha256,
        };
        let mut tx = self
            .begin(
                &mut client,
                scope,
                company_id,
                Writer::Session(VIEW),
                "finance.einvoice.generate_pdf",
            )
            .await?;
        let (current_view, current) = self
            .prepare_document_pdf_on(tx.sql(), scope, company_id, document_id)
            .await?;
        if current.identity != identity {
            return Err(EinvoiceError::Artifacts("print_inputs_changed"));
        }
        if let Some(stored) = self
            .available_pdf_on(tx.sql(), scope, company_id, &current_view, &identity)
            .await?
        {
            tx.rollback().await?;
            return Ok(stored);
        }
        let PdfIdentity {
            template_version,
            data_version,
            source_indices,
        } = identity;
        let mut event = NewEvent::new("pdf_generated");
        event.response = Some(json!({
            "artifact_id": artifact.artifact_id, "byte_size": artifact.byte_size,
            "sha256": artifact.sha256, "as_of_sequence": as_of, "template_version": template_version,
            "data_version": data_version, "source_indices": source_indices,
        }));
        documents::record_event(&mut tx, scope, company_id, document_id, event).await?;
        action(
            &tx,
            "finance.einvoice.pdf.generated",
            json!({
                "document_id": document_id, "code_number": view.document.code_number,
                "artifact_id": artifact.artifact_id,
            }),
        )
        .await?;
        tx.commit().await?;
        Ok(artifact)
    }

    async fn prepare_document_pdf_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<(crate::DocumentView, PreparedPdf)> {
        let recorded = documents::fetch(client, scope.tenant(), company_id, document_id)
            .await?
            .ok_or(EinvoiceError::DocumentNotFound)?;
        let portal = self
            .portal_on(client, &recorded.document.environment)
            .await?;
        let view = recorded.view(&Self::authority(), &portal, false);
        let prepared = self
            .prepare_pdf_on(client, scope, company_id, &view)
            .await?;
        Ok((view, prepared))
    }

    async fn prepare_pdf_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        view: &crate::DocumentView,
    ) -> Result<PreparedPdf> {
        let company =
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?;
        let language = self
            .settings
            .get(
                client,
                scope,
                Target::Company(company),
                bilibi_templates::LANGUAGE,
            )
            .await
            .map_err(|_| EinvoiceError::Artifacts("print_language_unavailable"))?;
        let resolved = self
            .templates
            .resolve_print(
                client,
                scope,
                CONTRACT.key,
                Some(company),
                language.value.as_str().unwrap_or("en"),
                None,
            )
            .await
            .map_err(|_| EinvoiceError::Artifacts("print_layout_unavailable"))?;
        let revision = match resolved.origin {
            Origin::Packaged { version } => format!("packaged-{version}"),
            Origin::Tenant { revision } | Origin::Company { revision, .. } => {
                format!("revision-{revision}")
            }
        };
        let template_version = format!("{}-{revision}", resolved.language);
        let accent = match client.transaction() {
            Some(tx) => self.branding.accent(tx).await?,
            None => self.branding.accent(client.sql_client().await?).await?,
        };
        let as_of = source_sequence(&view.events);
        let records = self
            .source_records_on(client, scope, company_id, &view.source_links)
            .await?;
        let source_indices = source_indices(&records);
        let source = records
            .iter()
            .flatten()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let compiled = CONTRACT
            .compile(&resolved.language, &resolved.layout)
            .map_err(|_| EinvoiceError::Artifacts("invalid_print_layout"))?;
        let inline = print_values::document(
            view,
            &source,
            &compiled,
            resolved.language.split('-').next() == Some("ms"),
            accent,
        )?;
        let mut digest = Sha256::new();
        digest.update(format!("{:?}:", inline.paper()));
        digest.update(inline.html().as_bytes());
        let data_version = format!("{}-{as_of}-{:x}", CONTRACT.version, digest.finalize());
        Ok(PreparedPdf {
            inline,
            identity: PdfIdentity {
                template_version,
                data_version,
                source_indices,
            },
        })
    }

    /// The PDF download (`finance.einvoice.view`) for the current document state.
    pub async fn read_pdf(
        &self,
        scope: &Scope,
        company_id: i64,
        artifact_id: &str,
    ) -> Result<PdfFile> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        let company =
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?;
        let owner = ArtifactOwner;
        let document = self
            .artifacts
            .for_owner(scope, company, &owner)
            .read(&mut client, artifact_id)
            .await
            .map_err(artifacts)?;
        let document_id = document
            .metadata
            .reference
            .subject
            .strip_prefix("document-")
            .and_then(|id| id.parse().ok())
            .ok_or(EinvoiceError::Unauthorized)?;
        let events = documents::events(&**client, scope.tenant(), company_id, document_id).await?;
        let response = events
            .iter()
            .rev()
            .filter(|event| event.event == "pdf_generated")
            .filter_map(|event| event.response.as_ref())
            .find(|response| response["artifact_id"].as_str() == Some(artifact_id))
            .ok_or(EinvoiceError::Unauthorized)?;
        let source_indices: Vec<usize> = serde_json::from_value(response["source_indices"].clone())
            .map_err(|_| EinvoiceError::Unauthorized)?;
        if !source_indices.is_empty() {
            let links =
                documents::source_links_of(&**client, scope.tenant(), company_id, &[document_id])
                    .await?;
            let named = source_indices
                .into_iter()
                .map(|index| links.get(index).map(|(_, link)| link.clone()))
                .collect::<Option<Vec<_>>>()
                .ok_or(EinvoiceError::Unauthorized)?;
            let records = self
                .source_records_on(&**client, scope, company_id, &named)
                .await?;
            if records.iter().any(Option::is_none) {
                return Err(EinvoiceError::Unauthorized);
            }
        }
        let (view, current) = self
            .prepare_document_pdf_on(&**client, scope, company_id, document_id)
            .await?;
        let selected = self
            .available_pdf_on(&**client, scope, company_id, &view, &current.identity)
            .await?;
        let stored_bytes = match selected {
            Some(pdf) if pdf.artifact_id == artifact_id => Some(document.bytes),
            Some(pdf) => match self
                .artifacts
                .for_owner(scope, company, &owner)
                .read(&mut client, &pdf.artifact_id)
                .await
            {
                Ok(copy) => Some(copy.bytes),
                Err(bilibi_artifacts::Error::NotFound) => None,
                Err(error) => return Err(artifacts(error)),
            },
            None => None,
        };
        let bytes = match stored_bytes {
            Some(bytes) => bytes,
            None => self
                .artifacts
                .print_document(&client, current.inline)
                .await
                .map_err(artifacts)?,
        };
        let latest = documents::events(&**client, scope.tenant(), company_id, document_id).await?;
        if source_sequence(&latest) != source_sequence(&view.events) {
            return Err(EinvoiceError::Artifacts("print_inputs_changed"));
        }
        Ok(PdfFile {
            filename: filename(&document.metadata.reference),
            bytes,
        })
    }
}

fn artifacts(error: bilibi_artifacts::Error) -> EinvoiceError {
    match error {
        bilibi_artifacts::Error::Database(Error::Forbidden) => EinvoiceError::Unauthorized,
        other => EinvoiceError::Artifacts(other.code()),
    }
}
