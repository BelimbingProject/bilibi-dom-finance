use crate::{document_label, MODULE, VIEW};
use bilibi_agent_api::OperationClient;
use bilibi_database::Error;
use bilibi_finance_ledger::{
    FiscalChoice, FiscalFuture, FiscalReference, FiscalSource, FiscalSources,
    FiscalSourcesDefinition, FISCAL_SOURCES,
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;

struct Posted;

inventory::submit! {
    Contribution::new(FISCAL_SOURCES, "finance/payables/fiscal_sources",
        Provenance { module: MODULE, source: file!(), line: line!() },
        &FiscalSourcesDefinition { sources: &Posted })
}

impl FiscalSources for Posted {
    fn read<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        _scope: &'a Scope,
        _company_id: i64,
        _record: &'a str,
    ) -> FiscalFuture<'a, Option<FiscalSource>> {
        Box::pin(async { Ok(None) })
    }

    fn references<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        records: &'a [String],
    ) -> FiscalFuture<'a, Vec<FiscalReference>> {
        Box::pin(async move {
            match bilibi_company::authorize_work_target(client, scope, company_id, VIEW).await {
                Ok(_) => {}
                Err(Error::Forbidden | Error::Unauthorized | Error::NotFound) => {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            }
            let ids: Vec<i64> = records
                .iter()
                .filter_map(|record| record.strip_prefix("document:")?.parse().ok())
                .collect();
            let rows = client
                .query(
                    "SELECT id, document_type, document_number FROM finance_payables_documents
                     WHERE tenant_id=$1 AND company_id=$2 AND id=ANY($3) AND status='posted'
                       AND document_type IN ('bill','debit_note') AND opening_batch_id IS NULL
                       AND entry_id IS NOT NULL AND document_number IS NOT NULL",
                    &[&scope.tenant(), &company_id, &ids],
                )
                .await?;
            Ok(rows
                .iter()
                .map(|row| {
                    let id: i64 = row.get(0);
                    FiscalReference {
                        record: format!("document:{id}"),
                        label: document_label(row.get(1), Some(row.get(2))),
                        href: format!("/finance/payables/bills/{id}"),
                    }
                })
                .collect())
        })
    }

    fn recent<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        _scope: &'a Scope,
        _company_id: i64,
        _limit: i64,
    ) -> FiscalFuture<'a, Vec<FiscalChoice>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
