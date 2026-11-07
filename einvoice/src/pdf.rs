//! The printable document (Bilimbi `ArtifactOwner`, `QrCode`,
//! `Einvoice.generate_pdf/3` and `read_pdf/3`): the module's adapter for Base
//! Artifacts, who may create, read and delete a document's PDF, and the QR
//! code of the authority's validation link.
//!
//! A PDF is a regenerable visual of the stored record, not the record: the
//! payload the authority validated lives in the submission. It is rendered by
//! Base Artifacts' pure serializer (`bilibi_artifacts::pdf`), so it needs no
//! browser or external tool and is deterministic for one document view. It
//! prints the parties, lines, totals, status, the authority's identifiers and
//! the validation link whose QR code the document page shows. Every operation
//! re-asks `finance.einvoice.view` for the company, as the facade does.
use crate::clearance::{action, Writer};
use crate::documents::{self, DocumentView, NewEvent};
use crate::{authorize, Authority, Einvoice, EinvoiceError, PartySnapshot, Result, VIEW};
use bilibi_artifacts::pdf::{self as serializer, Block, Metadata};
use bilibi_artifacts::{Operation, Owner, PdfOwner, Reference, RenderFuture};
use bilibi_database::Error;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use qrcode::render::svg;
use qrcode::QrCode;
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
pub(crate) struct ArtifactOwner {
    render: Option<serializer::Document>,
}

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

impl PdfOwner for ArtifactOwner {
    fn render_pdf<'a>(
        &'a self,
        _scope: &'a Scope,
        _company: OrganisationId,
        _reference: &'a Reference,
        _data: &'a Value,
    ) -> RenderFuture<'a> {
        Box::pin(async move {
            let document = self
                .render
                .as_ref()
                .ok_or(bilibi_artifacts::Error::InvalidDocument)?;
            serializer::render(document)
        })
    }
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
        drop(client);
        let view = self.view(scope, company_id, document_id).await?;
        let as_of = view
            .events
            .iter()
            .filter(|event| event.event != "pdf_generated")
            .map(|event| event.sequence)
            .max()
            .unwrap_or_default();
        if let Some(stored) = view
            .events
            .iter()
            .rev()
            .find(|event| event.event == "pdf_generated")
            .and_then(|event| event.response.as_ref())
            .filter(|response| response["as_of_sequence"].as_i64() == Some(i64::from(as_of)))
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
        let company =
            OrganisationId::new(company_id).map_err(|_| EinvoiceError::CompanyNotFound)?;
        let owner = ArtifactOwner {
            render: Some(printable(&view)),
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let stored = self
            .artifacts
            .for_owner(scope, company, &owner)
            .generate_pdf(
                &mut client,
                reference(document_id),
                &json!({ "document_id": document_id }),
            )
            .await
            .map_err(artifacts)?;
        let artifact = PdfArtifact {
            artifact_id: stored.id,
            byte_size: stored.byte_size,
            sha256: stored.sha256,
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
        let mut event = NewEvent::new("pdf_generated");
        event.response = Some(json!({
            "artifact_id": artifact.artifact_id, "byte_size": artifact.byte_size,
            "sha256": artifact.sha256, "as_of_sequence": as_of,
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
        let owner = ArtifactOwner { render: None };
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

/// The printable document of a view: what Bilimbi's `render_pdf` prints.
fn printable(view: &DocumentView) -> serializer::Document {
    let document = &view.document;
    let type_name = Einvoice::authority().document_type_name(&document.document_type);
    let title = format!("{type_name} {}", document.code_number);
    let mut heading = vec![
        format!("{} {}", type_name.to_uppercase(), document.code_number),
        format!("Issued {}", document.issued_at),
        format!("Status: {}", view.status),
    ];
    if let Some(uuid) = &view.document_uuid {
        heading.push(format!("Authority UUID: {uuid}"));
    }
    if let Some(at) = view.validated_at {
        heading.push(format!("Validated {at}"));
    }
    if let Some(url) = &view.validation_url {
        heading.push(format!("Validation link: {url}"));
    }
    if let Some(code) = &document.details.original_code_number {
        heading.push(format!(
            "Original document: {code} ({})",
            document
                .details
                .original_document_uuid
                .as_deref()
                .unwrap_or_default()
        ));
    }
    let keywords: Vec<&str> = [view.document_uuid.as_deref(), view.long_id.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    serializer::Document {
        title: latin1(&title),
        metadata: Metadata {
            author: Some(document.supplier.name.clone().unwrap_or_default()),
            subject: Some(title.clone()),
            keywords: Some(keywords.join(" ")),
        },
        blocks: vec![
            Block::Text(latin1(&heading.join("\n"))),
            Block::Table {
                headers: vec!["Supplier".into(), "Buyer".into()],
                rows: vec![vec![party(&document.supplier), party(&document.buyer)]],
            },
            Block::Table {
                headers: [
                    "#",
                    "Description",
                    "Qty",
                    "Unit price",
                    "Amount",
                    "Tax",
                    "Tax amount",
                ]
                .map(String::from)
                .to_vec(),
                rows: document
                    .lines
                    .iter()
                    .map(|line| {
                        vec![
                            line.line_number.to_string(),
                            latin1(&line.description),
                            bilibi_locale::money(line.quantity, 2),
                            bilibi_locale::money(line.unit_price, 2),
                            bilibi_locale::money(line.line_extension_amount, 2),
                            format!(
                                "{} {}%",
                                line.tax_type,
                                line.tax_rate
                                    .map(|rate| bilibi_locale::money(rate, 2))
                                    .unwrap_or_default()
                            ),
                            bilibi_locale::money(line.tax_amount, 2),
                        ]
                    })
                    .collect(),
            },
            Block::Table {
                headers: vec![
                    "Total".into(),
                    format!("Amount ({})", document.currency_code),
                ],
                rows: vec![
                    vec![
                        "Tax exclusive".into(),
                        bilibi_locale::money(document.tax_exclusive_amount, 2),
                    ],
                    vec!["Tax".into(), bilibi_locale::money(document.tax_amount, 2)],
                    vec![
                        "Tax inclusive".into(),
                        bilibi_locale::money(document.tax_inclusive_amount, 2),
                    ],
                    vec![
                        "Payable".into(),
                        bilibi_locale::money(document.payable_amount, 2),
                    ],
                ],
            },
            Block::Text(
                "Generated from the recorded document; the payload the authority validated is \
                 kept with its submission. Open the validation link, or scan its QR code on the \
                 document page, to confirm the document with the authority."
                    .into(),
            ),
        ],
    }
}

fn party(party: &PartySnapshot) -> String {
    let address = &party.address;
    let join = |parts: &[Option<&str>], separator: &str| {
        parts
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>()
            .join(separator)
    };
    [
        party.name.clone(),
        Some(format!(
            "TIN {}",
            party.tax_id.as_deref().unwrap_or_default()
        )),
        party
            .registration_number
            .as_ref()
            .map(|number| format!("Reg. {number}")),
        Some(address.lines.join(", ")),
        Some(join(
            &[address.postcode.as_deref(), address.city.as_deref()],
            " ",
        )),
        Some(join(
            &[
                address.state_code.as_deref(),
                address.country_code.as_deref(),
            ],
            " ",
        )),
        party.phone.clone(),
        party.email.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|line| !line.is_empty())
    .map(|line| latin1(&line))
    .collect::<Vec<_>>()
    .join("\n")
}

/// The serializer's font covers printable ASCII and Latin-1; anything else is
/// replaced visibly rather than refused.
fn latin1(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ' '..='~' | '\u{a0}'..='\u{ff}' | '\n' => c,
            _ => '?',
        })
        .collect()
}
