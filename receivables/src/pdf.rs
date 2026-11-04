//! Regenerable A4 views of issued invoices and credit notes. Base Artifacts
//! owns rendering, private storage and delivery authorization.
use crate::{authorize, Document, InvoiceLayout, ReceivableError, Receivables, Result, VIEW};
use askama::Template;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bilibi_artifacts::{HtmlFuture, HtmlOwner, Operation, Owner, Reference};
use bilibi_database::Error;
use bilibi_finance_ledger::Money;
use bilibi_module_registry::{
    contributions::{Access, Download, DOWNLOADS},
    Contribution, Provenance,
};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use bilibi_ui::download::{Download as DownloadHandler, DownloadFuture, DownloadRequest, FileBody};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const FILES: &str = "finance/receivables.pdf-downloads";
inventory::submit! { Contribution::new(DOWNLOADS, "finance/receivables.pdf", Provenance { module: crate::MODULE, source: file!(), line: line!() }, &Download {
    path: "/finance/receivables/invoices/{id}/pdf", service: FILES, operation: "pdf", access: Access::Capability(VIEW),
}) }

/// Printable bytes regenerated from an issued record, never an accounting record.
pub struct PdfFile {
    pub bytes: Vec<u8>,
    pub filename: String,
}

struct Printable {
    html: String,
}
impl Owner for Printable {
    fn owner_id(&self) -> &'static str {
        "finance/receivables"
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
            if !matches!(operation, Operation::Create | Operation::Read)
                || !reference.is_some_and(|r| {
                    r.kind == "invoice-pdf"
                        && r.subject
                            .strip_prefix("document-")
                            .is_some_and(|id| id.parse::<i64>().is_ok_and(|n| n > 0))
                })
            {
                return Err(Error::Forbidden);
            }
            bilibi_company::authorize_work_target(client, scope, company.get(), VIEW)
                .await
                .map(|_| ())
        })
    }
}
impl HtmlOwner for Printable {
    fn render_html<'a>(
        &'a self,
        _scope: &'a Scope,
        _company: OrganisationId,
        _reference: &'a Reference,
        _data: &'a Value,
    ) -> HtmlFuture<'a> {
        Box::pin(async move { Ok(self.html.clone()) })
    }
}

impl Receivables {
    /// Print an issued invoice or credit note using current company letterhead
    /// and the document's frozen customer facts and amounts (`VIEW`).
    pub async fn print_document(&self, scope: &Scope, company: i64, id: i64) -> Result<PdfFile> {
        let document = self.get_document(scope, company, id).await?;
        if document.status != "issued"
            || !matches!(document.kind.as_str(), "invoice" | "credit_note")
            || document.opening.is_some()
        {
            return Err(ReceivableError::NotIssued);
        }
        let layout = self.get_setup(scope, company).await?.invoice_layout;
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let company_id =
            OrganisationId::new(company).map_err(|_| ReceivableError::CompanyNotFound)?;
        let name = self
            .parties
            .organisation_name(&**client, scope, company_id)
            .await?
            .map(|o| o.name)
            .unwrap_or_default();
        let units = self
            .ledger
            .book_figures_on(&**client, scope, company, VIEW)
            .await?
            .minor_units;
        let mut logo = String::new();
        if let Some(id) = self
            .settings
            .get(
                &**client,
                scope,
                bilibi_settings::Target::Company(company_id),
                bilibi_company::IDENTITY_IMAGE_KEY,
            )
            .await?
            .value
            .as_i64()
        {
            let url = self
                .media
                .stream_url(&**client, scope, id)
                .await
                .map_err(|_| ReceivableError::Pdf("company_image_unavailable"))?;
            let token = url
                .split_once("?token=")
                .ok_or(ReceivableError::Pdf("company_image_unavailable"))?
                .1;
            let image = self
                .media
                .read_signed_asset(&**client, id, token)
                .await
                .map_err(|_| ReceivableError::Pdf("company_image_unavailable"))?;
            if !matches!(
                image.mime_type.as_deref().unwrap_or(""),
                "image/png" | "image/jpeg" | "image/webp"
            ) {
                return Err(ReceivableError::Pdf("company_image_invalid"));
            }
            logo = format!(
                "data:{};base64,{}",
                image.mime_type.as_deref().unwrap_or(""),
                STANDARD.encode(image.bytes)
            );
        }
        let title = if document.kind == "credit_note" {
            "Credit note"
        } else {
            "Invoice"
        };
        let reference = document.document_reference.clone().unwrap_or_default();
        let rows = document
            .lines
            .iter()
            .map(|l| Row {
                description: l.description.clone(),
                quantity: l.quantity.normalize().to_string(),
                price: Money::display_decimal(l.unit_price, units),
                net: l.amount.display(units),
                tax: l.tax_amount.display(units),
            })
            .collect();
        let template = Invoice {
            title,
            reference: &reference,
            company_name: if layout.company_name.is_empty() {
                &name
            } else {
                &layout.company_name
            },
            logo: &logo,
            layout: &layout,
            document: &document,
            rows,
            subtotal: document.subtotal.display(units),
            tax: document.tax_total.display(units),
            total: document.total.display(units),
        };
        let owner = Printable {
            html: template
                .render()
                .map_err(|_| ReceivableError::Pdf("template"))?,
        };
        let owned = self.artifacts.for_owner(scope, company_id, &owner);
        let artifact = owned
            .render_inline(
                &mut client,
                Reference {
                    subject: format!("document-{id}"),
                    kind: "invoice-pdf".into(),
                },
                &json!({}),
            )
            .await
            .map_err(|e| ReceivableError::Pdf(e.code()))?;
        let file = owned
            .read(&mut client, &artifact.id)
            .await
            .map_err(|e| ReceivableError::Pdf(e.code()))?;
        Ok(PdfFile {
            bytes: file.bytes,
            filename: format!("{}-{reference}.pdf", document.kind),
        })
    }
}

pub(crate) struct Downloads {
    pub receivables: Arc<Receivables>,
}
impl DownloadHandler<Scope, Error> for Downloads {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let company = request
                .scope
                .current_company()
                .ok_or(Error::NotFound)?
                .get();
            let id = request
                .path
                .get("id")
                .and_then(|id| id.parse().ok())
                .ok_or(Error::NotFound)?;
            let file = self
                .receivables
                .print_document(request.scope, company, id)
                .await
                .map_err(|error| match error {
                    ReceivableError::Unauthorized => Error::Forbidden,
                    ReceivableError::DocumentNotFound | ReceivableError::NotIssued => {
                        Error::NotFound
                    }
                    _ => Error::Internal,
                })?;
            Ok(FileBody::inline(
                file.bytes,
                "application/pdf",
                &file.filename,
            ))
        })
    }
}

struct Row {
    description: String,
    quantity: String,
    price: String,
    net: String,
    tax: String,
}
#[derive(Template)]
#[template(path = "receivables_invoice.html")]
struct Invoice<'a> {
    title: &'a str,
    reference: &'a str,
    company_name: &'a str,
    logo: &'a str,
    layout: &'a InvoiceLayout,
    document: &'a Document,
    rows: Vec<Row>,
    subtotal: String,
    tax: String,
    total: String,
}
