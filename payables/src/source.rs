//! Bilimbi.Finance.Payables.Attachments.Bill: read on the caller's connection.
use crate::*;
use bilibi_finance_attachments::{
    on_source_client, Source, SourceClient, SourceDefinition, SourceDocument, SourceFuture,
    SourceRefusal, SOURCES,
};
use bilibi_module_registry::{Contribution, Provenance};
struct Documents;
impl Source for Documents {
    fn label(&self) -> &'static str {
        "Payables"
    }
    fn view_capabilities(&self) -> &'static [&'static str] {
        &[VIEW]
    }
    fn document<'a>(
        &'a self,
        c: SourceClient<'a>,
        s: &'a Scope,
        company: i64,
        id: i64,
    ) -> SourceFuture<'a> {
        Box::pin(async move {
            on_source_client!(c, |sql| {
                authorize(sql, s, company, VIEW)
                    .await
                    .map_err(|e| match e {
                        PayablesError::Unauthorized => SourceRefusal::Unauthorized,
                        PayablesError::CompanyNotFound => SourceRefusal::NotFound,
                        PayablesError::Failed(e) => SourceRefusal::Failed(e),
                        _ => SourceRefusal::Failed(Error::Invalid),
                    })?;
                let d = store::document(sql, s.tenant(), company, id)
                    .await?
                    .ok_or(SourceRefusal::NotFound)?;
                Ok(SourceDocument {
                    label: d.label(),
                    date: d.document_date,
                    is_final: d.status != "draft",
                })
            })
        })
    }
    fn href(&self, id: i64) -> Option<String> {
        Some(format!("/finance/payables/bills/{id}"))
    }
}
inventory::submit! {Contribution::new(SOURCES,DOCUMENT_SOURCE,Provenance {module:MODULE,source:file!(),line:line!()},&SourceDefinition {source:&Documents})}

// Bilimbi.Finance.Payables.Attachments.Payment: evidence follows the request through its decision.
struct Payments;
impl Source for Payments {
    fn label(&self) -> &'static str {
        "Payment"
    }
    fn view_capabilities(&self) -> &'static [&'static str] {
        &[VIEW]
    }
    fn document<'a>(
        &'a self,
        c: SourceClient<'a>,
        s: &'a Scope,
        company: i64,
        id: i64,
    ) -> SourceFuture<'a> {
        Box::pin(async move {
            on_source_client!(c, |sql| {
                authorize(sql, s, company, VIEW)
                    .await
                    .map_err(|e| match e {
                        PayablesError::Unauthorized => SourceRefusal::Unauthorized,
                        PayablesError::CompanyNotFound => SourceRefusal::NotFound,
                        PayablesError::Failed(e) => SourceRefusal::Failed(e),
                        _ => SourceRefusal::Failed(Error::Invalid),
                    })?;
                let r=sql.query_opt("SELECT r.request_number,r.payment_date::text,EXISTS(SELECT 1 FROM finance_payables_payment_request_events e WHERE e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.request_id=r.id) FROM finance_payables_payment_requests r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id=$3",&[&s.tenant(),&company,&id]).await.map_err(Error::from)?.ok_or(SourceRefusal::NotFound)?;
                Ok(SourceDocument {
                    label: format!("Payment {}", r.get::<_, i64>(0)),
                    date: r
                        .get::<_, String>(1)
                        .parse()
                        .map_err(|_| SourceRefusal::Failed(Error::Invalid))?,
                    is_final: r.get(2),
                })
            })
        })
    }
    fn href(&self, id: i64) -> Option<String> {
        Some(format!("/finance/payables/payments/{id}"))
    }
}
inventory::submit! {Contribution::new(SOURCES,PAYMENT_SOURCE,Provenance {module:MODULE,source:file!(),line:line!()},&SourceDefinition {source:&Payments})}
