//! Private visual copies from the frozen fiscal document and its clearance evidence.
use crate::clearance::{action, Writer};
use crate::documents::{self, NewEvent, StatusEvent};
use crate::{
    authorize, print_layouts::CONTRACT, print_values, Einvoice, EinvoiceError, Result, VIEW,
};
use bilibi_artifacts::{Operation, Owner, PdfOptions, Reference};
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_templates::Origin;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Serialize;
use serde_json::json;

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

impl Einvoice {
    /// Renders the document as a PDF through Base Artifacts and records it on
    /// a `pdf_generated` event (`finance.einvoice.view`). The event keeps the
    /// sequence of the last other event the PDF was rendered from; while the
    /// document has gained none since, asking again answers that artifact and
    /// records nothing. Base Artifacts needs a storage root and a retention
    /// period first (`Artifacts("policy")`). The audit action is
    /// `finance.einvoice.pdf.generated`.
    pub async fn generate_pdf(
        &self,
        scope: &Scope,
        company_id: i64,
        document_id: i64,
    ) -> Result<PdfArtifact> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company_id, VIEW).await?;
        // Recording the PDF on the document is a write, refused for an
        // archived company before an artifact is stored.
        bilibi_company::require_writable(&**client, scope, company_id, false)
            .await
            .map_err(crate::company_write)?;
        let company =
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?;
        let language = self
            .settings
            .get(
                &**client,
                scope,
                Target::Company(company),
                bilibi_templates::LANGUAGE,
            )
            .await
            .map_err(|_| EinvoiceError::Artifacts("print_language_unavailable"))?;
        let resolved = self
            .templates
            .resolve_print(
                &**client,
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
        let accent = self.branding.accent(&**client).await?;
        drop(client);
        let view = self.view(scope, company_id, document_id).await?;
        let as_of = source_sequence(&view.events);
        if let Some(stored) = view
            .events
            .iter()
            .rev()
            .find(|event| event.event == "pdf_generated")
            .and_then(|event| event.response.as_ref())
            .filter(|response| {
                response["as_of_sequence"].as_i64() == Some(i64::from(as_of))
                    && response["template_version"].as_str() == Some(template_version.as_str())
            })
            .and_then(|response| {
                Some(PdfArtifact {
                    artifact_id: response["artifact_id"].as_str()?.to_owned(),
                    byte_size: response["byte_size"].as_i64()?,
                    sha256: response["sha256"].as_str()?.to_owned(),
                })
            })
        {
            return Ok(stored);
        }
        let compiled = CONTRACT
            .compile(&resolved.language, &resolved.layout)
            .map_err(|_| EinvoiceError::Artifacts("invalid_print_layout"))?;
        let inline = print_values::document(
            &view,
            &compiled,
            resolved.language.split('-').next() == Some("ms"),
            accent,
        )?;
        let options = PdfOptions::default()
            .template_version(template_version.clone())
            .data_version(format!("{}-event-{as_of}", CONTRACT.version));
        let mut client = self.db.get().await.map_err(Error::from)?;
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
        let events = documents::events(tx.sql(), scope.tenant(), company_id, document_id).await?;
        if source_sequence(&events) != as_of {
            return Err(EinvoiceError::Artifacts("clearance_changed"));
        }
        let mut event = NewEvent::new("pdf_generated");
        event.response = Some(json!({
            "artifact_id": artifact.artifact_id, "byte_size": artifact.byte_size,
            "sha256": artifact.sha256, "as_of_sequence": as_of, "template_version": template_version,
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

    /// The bytes of a PDF [`Einvoice::generate_pdf`] stored, for the download
    /// route (`finance.einvoice.view`, asked again by Base Artifacts, which
    /// verifies the bytes and records the read).
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
        Ok(PdfFile {
            filename: filename(&document.metadata.reference),
            bytes: document.bytes,
        })
    }
}

fn artifacts(error: bilibi_artifacts::Error) -> EinvoiceError {
    match error {
        bilibi_artifacts::Error::Database(Error::Forbidden) => EinvoiceError::Unauthorized,
        other => EinvoiceError::Artifacts(other.code()),
    }
}
