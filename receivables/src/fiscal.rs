//! Issued invoices and credit notes and their fiscal documents (e-invoices),
//! through the ledger's seam (`bilibi_finance_ledger::FISCAL_SOURCES`,
//! `FISCAL_DOCUMENTS`): Receivables describes one issued document for the
//! module that issues its fiscal document, and reads back where that fiscal
//! document stands for its own list and page. It never names or depends on
//! that module, and shows nothing when none is mounted or the person may not
//! read it.
use crate::{documents, Document, ReceivableError, Receivables, Result, MODULE, VIEW};
use bilibi_agent_api::OperationClient;
use bilibi_finance_ledger::{
    FiscalChoice, FiscalFuture, FiscalLine, FiscalParty, FiscalSource, FiscalSourceKind,
    FiscalSources, FiscalSourcesDefinition, FiscalStanding, FiscalStart, FISCAL_SOURCES,
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};

/// Where an issued document's fiscal document stands, and how to start one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fiscal {
    /// Its fiscal document, when one was issued for it.
    pub document: Option<FiscalStanding>,
    /// The address that starts one: only while none stands, the document can
    /// have one, and this person may issue and submit it.
    pub start: Option<String>,
}

impl Fiscal {
    pub(crate) fn is_empty(&self) -> bool {
        self.document.is_none() && self.start.is_none()
    }
}

/// The record name the ledger entry's owner uses for a document.
fn record(id: i64) -> String {
    format!("document:{id}")
}

/// Whether a fiscal document can be made for it: an issued invoice or credit
/// note that posted its own entry (an opening balance did not).
fn eligible(kind: &str, status: &str, opening: bool) -> bool {
    status == "issued" && !opening && matches!(kind, "invoice" | "credit_note")
}

impl Receivables {
    /// Where the fiscal documents of these documents stand: `(id, kind,
    /// status, opening)` each. The second answer is whether this person may
    /// read fiscal documents at all.
    pub(crate) async fn fiscal_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        documents: &[(i64, &str, &str, bool)],
    ) -> Result<(Vec<Fiscal>, bool)> {
        let records: Vec<String> = documents
            .iter()
            .filter(|(_, kind, status, opening)| eligible(kind, status, *opening))
            .map(|(id, ..)| record(*id))
            .collect();
        let standings = self
            .ledger
            .fiscal_standing_on(client, scope, company, MODULE, &records)
            .await
            .map_err(ReceivableError::Ledger)?;
        let fiscal = documents
            .iter()
            .map(|(id, kind, status, opening)| {
                if !eligible(kind, status, *opening) {
                    return Fiscal::default();
                }
                let name = record(*id);
                let document = standings
                    .documents
                    .iter()
                    .find(|standing| standing.record == name)
                    .cloned();
                let start = standings
                    .start
                    .as_deref()
                    .filter(|_| document.as_ref().is_none_or(|d| d.replaceable))
                    .map(|path| FiscalStart::href(path, MODULE, &name));
                Fiscal { document, start }
            })
            .collect();
        Ok((fiscal, standings.readable))
    }
}

struct Issued;

inventory::submit! {
    Contribution::new(FISCAL_SOURCES, "finance/receivables/fiscal_sources",
        Provenance { module: MODULE, source: file!(), line: line!() },
        &FiscalSourcesDefinition { sources: &Issued })
}

async fn may_view(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
) -> std::result::Result<bool, bilibi_database::Error> {
    match bilibi_company::authorize_work_target(client, scope, company_id, VIEW).await {
        Ok(_) => Ok(true),
        Err(
            bilibi_database::Error::Forbidden
            | bilibi_database::Error::Unauthorized
            | bilibi_database::Error::NotFound,
        ) => Ok(false),
        Err(other) => Err(other),
    }
}

fn describe(d: Document) -> Option<FiscalSource> {
    if !eligible(&d.kind, &d.status, d.opening.is_some()) {
        return None;
    }
    Some(FiscalSource {
        record: record(d.id),
        kind: if d.kind == "credit_note" {
            FiscalSourceKind::CreditNote
        } else {
            FiscalSourceKind::Invoice
        },
        number: d.document_reference?,
        issue_date: d.issue_date?,
        currency_code: d.currency_code,
        total: d.total.amount(),
        ledger_entry_id: d.ledger_entry_id?,
        corrects: d.credited_document_id.map(record),
        party: FiscalParty {
            name: d.party_name,
            tax_id: d.party_tax_identifier,
            registration_number: d.party_registration_identifier,
            email: d.party_email,
            phone: d.party_phone,
            address_lines: [d.party_line1, d.party_line2, d.party_line3]
                .into_iter()
                .flatten()
                .collect(),
            city: d.party_locality,
            postcode: d.party_postcode,
            state: d.party_admin1_code,
            country: d.party_country_iso,
        },
        lines: d
            .lines
            .into_iter()
            .map(|line| FiscalLine {
                description: line.description,
                quantity: line.quantity,
                unit_price: line.unit_price,
                tax_rate: line.tax_rate,
            })
            .collect(),
    })
}

impl FiscalSources for Issued {
    fn read<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        record: &'a str,
    ) -> FiscalFuture<'a, Option<FiscalSource>> {
        Box::pin(async move {
            let Some(id) = record
                .strip_prefix("document:")
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
            else {
                return Ok(None);
            };
            if !may_view(client, scope, company_id).await? {
                return Ok(None);
            }
            match documents::read(client, scope.tenant(), company_id, id).await {
                Ok(document) => Ok(describe(document)),
                Err(ReceivableError::Failed(error)) => Err(error),
                Err(_) => Ok(None),
            }
        })
    }

    fn recent<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        limit: i64,
    ) -> FiscalFuture<'a, Vec<FiscalChoice>> {
        Box::pin(async move {
            if !may_view(client, scope, company_id).await? {
                return Ok(Vec::new());
            }
            let rows = client
                .query(
                    "SELECT d.id, d.document_reference, COALESCE(d.party_name,''), d.issue_date::text \
                     FROM finance_receivables_documents d \
                     WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.status='issued' \
                       AND d.kind IN ('invoice','credit_note') AND d.ledger_entry_id IS NOT NULL \
                       AND d.document_reference IS NOT NULL AND d.issue_date IS NOT NULL \
                       AND NOT EXISTS (SELECT 1 FROM finance_receivables_opening_items i \
                         WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id) \
                     ORDER BY d.id DESC LIMIT $3",
                    &[&scope.tenant(), &company_id, &limit.clamp(1, 500)],
                )
                .await?;
            Ok(rows
                .iter()
                .map(|row| FiscalChoice {
                    record: record(row.get(0)),
                    number: row.get(1),
                    party_name: row.get(2),
                    issue_date: row.get(3),
                })
                .collect())
        })
    }
}
