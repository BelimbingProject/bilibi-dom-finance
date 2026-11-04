//! Attachment sources mirroring Bilimbi `Receivables.Attachments.Document`
//! and `Receivables.Attachments.Payment`, on the caller's connection only.
use crate::{MODULE, VIEW};
use bilibi_database::Error;
use bilibi_finance_attachments::{
    on_source_client, Source, SourceClient, SourceDefinition, SourceDocument, SourceFuture,
    SourceRefusal, SOURCES,
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use tokio_postgres::GenericClient;

struct DocumentSource;
struct PaymentSource;

inventory::submit! {
    Contribution::new(SOURCES, "finance/receivables/document",
        Provenance { module: MODULE, source: file!(), line: line!() },
        &SourceDefinition { source: &DocumentSource })
}
inventory::submit! {
    Contribution::new(SOURCES, "finance/receivables/payment",
        Provenance { module: MODULE, source: file!(), line: line!() },
        &SourceDefinition { source: &PaymentSource })
}

fn refusal(error: Error) -> SourceRefusal {
    match error {
        Error::Forbidden | Error::Unauthorized => SourceRefusal::Unauthorized,
        Error::NotFound => SourceRefusal::NotFound,
        other => SourceRefusal::Failed(other),
    }
}

async fn read(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    id: i64,
    payment: bool,
) -> Result<SourceDocument, SourceRefusal> {
    bilibi_company::authorize_work_target(client, scope, company_id, VIEW)
        .await
        .map_err(refusal)?;
    // SourceDocument requires a Date unlike Bilimbi's nullable date. For an
    // undated draft (or discarded undated document), use the stable inserted_at
    // UTC date, never today's date. Issuing refreshes retention to issue_date.
    let sql = if payment {
        "SELECT 'Payment ' || payment_number::text, to_char(payment_date,'YYYY-MM-DD'), true FROM finance_receivables_payments WHERE tenant_id=$1 AND company_id=$2 AND id=$3"
    } else {
        "SELECT CASE kind WHEN 'invoice' THEN 'Invoice' WHEN 'opening_credit' THEN 'Customer credit' ELSE 'Credit note' END || ' ' || COALESCE(document_number::text,'(draft ' || id::text || ')'), to_char(COALESCE(issue_date,inserted_at::date),'YYYY-MM-DD'), status <> 'draft' FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND id=$3"
    };
    let row = client
        .query_opt(sql, &[&scope.tenant(), &company_id, &id])
        .await
        .map_err(Error::from)
        .map_err(SourceRefusal::Failed)?
        .ok_or(SourceRefusal::NotFound)?;
    let date: String = row.get(1);
    Ok(SourceDocument {
        label: row.get(0),
        date: date
            .parse()
            .map_err(|_| SourceRefusal::Failed(Error::Internal))?,
        is_final: row.get(2),
    })
}

impl Source for DocumentSource {
    fn label(&self) -> &'static str {
        "Receivables document"
    }
    fn view_capabilities(&self) -> &'static [&'static str] {
        &[VIEW]
    }
    fn document<'a>(
        &'a self,
        client: SourceClient<'a>,
        scope: &'a Scope,
        company_id: i64,
        document_id: i64,
    ) -> SourceFuture<'a> {
        Box::pin(async move {
            on_source_client!(client, |c| read(c, scope, company_id, document_id, false)
                .await)
        })
    }
}

impl Source for PaymentSource {
    fn label(&self) -> &'static str {
        "Payment received"
    }
    fn view_capabilities(&self) -> &'static [&'static str] {
        &[VIEW]
    }
    fn document<'a>(
        &'a self,
        client: SourceClient<'a>,
        scope: &'a Scope,
        company_id: i64,
        document_id: i64,
    ) -> SourceFuture<'a> {
        Box::pin(async move {
            on_source_client!(client, |c| read(c, scope, company_id, document_id, true)
                .await)
        })
    }
}
