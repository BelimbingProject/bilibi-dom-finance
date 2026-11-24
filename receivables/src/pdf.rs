//! Private issued invoices and credit notes rendered from the owner's facts.
use crate::{authorize, print_layouts, print_values, ReceivableError, Receivables, Result, VIEW};
use bilibi_artifacts::{Operation, Owner, PdfOptions, Reference};
use bilibi_database::Error;
use bilibi_finance_ledger::Money;
use bilibi_module_registry::{
    contributions::{Access, Download, DOWNLOADS},
    Contribution, Provenance,
};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_templates::Origin;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use bilibi_ui::download::{Download as DownloadHandler, DownloadFuture, DownloadRequest, FileBody};
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

struct Printable;
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

impl Receivables {
    /// Print the issued commercial copy. Both the download and mail use it.
    pub async fn print_document(&self, scope: &Scope, company: i64, id: i64) -> Result<PdfFile> {
        let document = self.get_document(scope, company, id).await?;
        if document.status != "issued"
            || !matches!(document.kind.as_str(), "invoice" | "credit_note")
            || document.opening.is_some()
        {
            return Err(ReceivableError::NotIssued);
        }
        let letterhead = self.get_setup(scope, company).await?.invoice_layout;
        let mut client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let company_id =
            OrganisationId::new(company).map_err(|_| ReceivableError::CompanyNotFound)?;
        let language = self
            .settings
            .get(
                &**client,
                scope,
                Target::Company(company_id),
                bilibi_templates::LANGUAGE,
            )
            .await?;
        let contract = print_layouts::for_kind(&document.kind);
        let resolved = self
            .templates
            .resolve_print(
                &**client,
                scope,
                contract.key,
                Some(company_id),
                language.value.as_str().unwrap_or("en"),
                None,
            )
            .await?;
        let compiled = contract
            .compile(&resolved.language, &resolved.layout)
            .map_err(|_| ReceivableError::Pdf("invalid_print_layout"))?;
        let logo = if let Some(id) = self
            .settings
            .get(
                &**client,
                scope,
                Target::Company(company_id),
                crate::configuration::INVOICE_LOGO,
            )
            .await?
            .value
            .as_i64()
        {
            let url = self
                .media
                .stream_url(&**client, scope, id)
                .await
                .map_err(|_| ReceivableError::Pdf("invoice_logo_unavailable"))?;
            let token = url
                .split_once("?token=")
                .ok_or(ReceivableError::Pdf("invoice_logo_unavailable"))?
                .1;
            Some(
                self.media
                    .read_signed_asset(&**client, id, token)
                    .await
                    .map_err(|_| ReceivableError::Pdf("invoice_logo_unavailable"))?,
            )
        } else {
            None
        };
        let validation = self
            .ledger
            .fiscal_validation_url_on(
                &**client,
                &self.settings,
                scope,
                company,
                crate::MODULE,
                &format!("document:{id}"),
            )
            .await?;

        let balance = if document.outstanding.is_some() {
            let on = self.today(&**client, scope, company).await?;
            let units = self
                .ledger
                .book_figures_on(&**client, scope, company, VIEW)
                .await?
                .minor_units;
            let row = client
                .query_one(
                    "SELECT COALESCE((
                       SELECT sum(a.amount) FROM finance_receivables_allocations a
                       JOIN finance_receivables_payments p ON p.id=a.payment_id
                         AND p.tenant_id=$1 AND p.company_id=$2
                       WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.document_id=$3
                         AND p.payment_date<=$4::text::date AND NOT EXISTS (
                           SELECT 1 FROM finance_receivables_payment_reversals r
                           WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.payment_id=p.id
                             AND r.reversal_date<=$4::text::date
                         )
                     ),0)::text,
                     finance_receivables_residual(
                       $1,$2,$3,$4::text::date,9223372036854775807,9223372036854775807
                     )::text",
                    &[&scope.tenant(), &company, &id, &on.to_string()],
                )
                .await
                .map_err(Error::from)?;
            let money = |index: usize| {
                Money::parse(row.get::<_, &str>(index), units)
                    .map_err(|_| ReceivableError::Pdf("payment_amount"))
            };
            let paid = money(0)?;
            let outstanding = money(1)?;
            Some(print_values::Balance {
                on,
                paid,
                outstanding,
                adjustments: document.total - paid - outstanding,
                units,
            })
        } else {
            None
        };
        let business_name = document
            .print_facts
            .as_ref()
            .map(|facts| facts.seller_name.as_str());
        let trading = if business_name == Some(letterhead.company_name.as_str()) {
            ""
        } else {
            letterhead.company_name.as_str()
        };
        let letterhead_lines = [trading, letterhead.contact.as_str()]
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let print_letterhead = print_values::Letterhead {
            lines: &letterhead_lines,
            terms: &letterhead.payment_terms,
            bank: &letterhead.bank_details,
            footer: &letterhead.footer,
            logo: logo.as_ref().and_then(|image| {
                image
                    .mime_type
                    .as_deref()
                    .map(|mime| (mime, image.bytes.as_ref()))
            }),
        };
        let accent = self.branding.accent(&**client).await?;
        let inline = print_values::render(
            &compiled,
            &document,
            resolved.language.split('-').next() == Some("ms"),
            balance.as_ref(),
            print_letterhead,
            validation.as_deref(),
            accent,
        )?;

        let reference = Reference {
            subject: format!("document-{id}"),
            kind: "invoice-pdf".into(),
        };
        let revision = match resolved.origin {
            Origin::Packaged { version } => format!("packaged-{version}"),
            Origin::Tenant { revision } | Origin::Company { revision, .. } => {
                format!("revision-{revision}")
            }
        };
        let options = PdfOptions::default()
            .template_version(format!("{}-{revision}", resolved.language))
            .data_version(format!(
                "{}-entry-{}",
                contract.version,
                document
                    .ledger_entry_number
                    .ok_or(ReceivableError::NotIssued)?
            ));
        let owner = Printable;
        let owned = self.artifacts.for_owner(scope, company_id, &owner);
        let artifact =
            Box::pin(owned.render_inline_document(&mut client, reference, inline, &options))
                .await
                .map_err(|error| ReceivableError::Pdf(error.code()))?;
        let file = owned
            .read(&mut client, &artifact.document.id)
            .await
            .map_err(|error| ReceivableError::Pdf(error.code()))?;
        let number = document
            .document_reference
            .as_deref()
            .ok_or(ReceivableError::NotIssued)?;
        Ok(PdfFile {
            bytes: file.bytes,
            filename: format!("{}-{number}.pdf", document.kind),
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
            let file = Box::pin(self.receivables.print_document(request.scope, company, id))
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
