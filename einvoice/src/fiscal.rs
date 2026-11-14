//! The e-invoice of a record another Finance module posted (an invoice, a
//! credit note), through the ledger's seam
//! (`bilibi_finance_ledger::FISCAL_SOURCES`, `FISCAL_DOCUMENTS`): this module
//! tells the record's own list and page where its e-invoice stands, and
//! starts one from what the record says. It never names or depends on the
//! module that posted the record, and reads it only through that module's
//! own reader, under the person's own scope.
use crate::{
    code_lists, documents, Authority, DraftAddress, DraftParty, Einvoice,
    EinvoiceError, FieldError, PartySnapshot, Result, ISSUE, MODULE, SUBMIT, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_finance_ledger::{
    FiscalDocuments, FiscalDocumentsDefinition, FiscalFuture, FiscalSource, FiscalSourceKind,
    FiscalStanding, FiscalStandings, FiscalTone, FISCAL_DOCUMENTS,
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

/// The page that starts an e-invoice (`FiscalStart::href`) and lists them.
pub(crate) const START: &str = "/finance/einvoice/documents";

/// A status as a person reads it, on every page that shows one.
pub(crate) fn status_label(status: &str) -> &'static str {
    match status {
        "issued" | "released" => "Not sent",
        "submitted" => "Sent",
        "valid" => "Valid",
        "refused" => "Refused",
        "invalid" => "Invalid",
        "cancelled" => "Cancelled",
        "rejected" => "Rejection requested",
        "consolidated" => "Consolidated",
        _ => "Unknown",
    }
}

fn tone(status: &str) -> FiscalTone {
    match status {
        "valid" => FiscalTone::Good,
        "refused" | "invalid" | "rejected" => FiscalTone::Problem,
        "cancelled" => FiscalTone::Ended,
        _ => FiscalTone::Waiting,
    }
}

/// A document the authority turned back or that was cancelled no longer
/// stands, so another may be issued for the same record.
fn replaceable(status: &str) -> bool {
    matches!(status, "refused" | "invalid" | "cancelled")
}

async fn may(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> std::result::Result<bool, bilibi_database::Error> {
    match bilibi_company::authorize_work_target(client, scope, company_id, capability).await {
        Ok(_) => Ok(true),
        Err(
            bilibi_database::Error::Forbidden
            | bilibi_database::Error::Unauthorized
            | bilibi_database::Error::NotFound,
        ) => Ok(false),
        Err(other) => Err(other),
    }
}

/// The newest e-invoice of each record: `(record, document id, number, status)`.
async fn newest(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    module: &str,
    records: &[String],
) -> std::result::Result<Vec<(String, i64, String, String)>, bilibi_database::Error> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            "SELECT DISTINCT ON (l.source_record) l.source_record, d.id, d.code_number, \
               COALESCE((SELECT e.event FROM finance_einvoice_status_events e \
                 WHERE e.document_id = d.id AND e.event = ANY($5) \
                 ORDER BY e.sequence DESC LIMIT 1), 'issued') \
             FROM finance_einvoice_source_links l \
             JOIN finance_einvoice_documents d ON d.id = l.document_id AND d.company_id = l.company_id \
             WHERE l.tenant_id = $1 AND l.company_id = $2 AND l.source_module = $3 \
               AND l.source_record = ANY($4) \
             ORDER BY l.source_record, d.id DESC",
            &[
                &tenant,
                &company_id,
                &module,
                &records,
                &&documents::STATUS_EVENTS[..],
            ],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect())
}

fn standing(record: String, id: i64, number: String, status: String) -> FiscalStanding {
    FiscalStanding {
        record,
        label: status_label(&status).to_owned(),
        tone: tone(&status),
        replaceable: replaceable(&status),
        status,
        number,
        href: format!("{START}/{id}"),
    }
}

struct Issued;

inventory::submit! {
    Contribution::new(FISCAL_DOCUMENTS, "finance/einvoice/fiscal_documents",
        Provenance { module: MODULE, source: file!(), line: line!() },
        &FiscalDocumentsDefinition { documents: &Issued })
}

impl FiscalDocuments for Issued {
    fn standing<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        company_id: i64,
        module: &'a str,
        records: &'a [String],
    ) -> FiscalFuture<'a, FiscalStandings> {
        Box::pin(async move {
            if !may(client, scope, company_id, VIEW).await? {
                return Ok(FiscalStandings::default());
            }
            let documents = newest(client, scope.tenant(), company_id, module, records)
                .await?
                .into_iter()
                .map(|(record, id, number, status)| standing(record, id, number, status))
                .collect();
            // Starting one records it and sends it, so it takes both grants.
            let start = may(client, scope, company_id, ISSUE).await?
                && may(client, scope, company_id, SUBMIT).await?;
            Ok(FiscalStandings {
                readable: true,
                documents,
                start: start.then(|| START.to_owned()),
            })
        })
    }
}

fn invalid(field: &str, message: &str) -> EinvoiceError {
    EinvoiceError::Invalid(vec![FieldError::new(field, message)])
}

fn blank(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(|text| text.trim().is_empty())
}

impl Einvoice {
    /// The posted records an e-invoice can be started for
    /// (`finance.einvoice.view`), newest first: those with no e-invoice, or
    /// whose e-invoice no longer stands.
    pub(crate) async fn sources_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Value> {
        bilibi_company::authorize_work_target(client, scope, company_id, VIEW)
            .await
            .map_err(crate::refusal)?;
        let choices = self
            .ledger
            .fiscal_sources_on(client, scope, company_id, 100)
            .await
            .map_err(ledger_error)?;
        let mut open = Vec::new();
        let mut modules: Vec<&str> = choices.iter().map(|(module, _)| *module).collect();
        modules.dedup();
        for module in modules {
            let records: Vec<String> = choices
                .iter()
                .filter(|(owner, _)| *owner == module)
                .map(|(_, choice)| choice.record.clone())
                .collect();
            let taken = newest(client, scope.tenant(), company_id, module, &records).await?;
            for (_, choice) in choices.iter().filter(|(owner, _)| *owner == module) {
                let stands = taken
                    .iter()
                    .any(|(record, _, _, status)| *record == choice.record && !replaceable(status));
                if !stands {
                    open.push(json!({
                        "module": module, "record": choice.record, "number": choice.number,
                        "party_name": choice.party_name, "issue_date": choice.issue_date,
                    }));
                }
            }
        }
        Ok(json!({ "records": open }))
    }

    /// The draft an e-invoice of one posted record starts from
    /// (`finance.einvoice.view`, and the record's own view capability): what
    /// the record says, then what this company last sent for the same buyer
    /// and the same line descriptions, since the record does not hold the
    /// authority's codes. Nothing is recorded; the answer's `draft` is the
    /// input of `finance.einvoice.document.issue` once its gaps are filled.
    pub(crate) async fn source_draft_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
        module: &str,
        record: &str,
    ) -> Result<Value> {
        bilibi_company::authorize_work_target(client, scope, company_id, VIEW)
            .await
            .map_err(crate::refusal)?;
        let Some(source) = self
            .ledger
            .fiscal_source_on(client, scope, company_id, module, record)
            .await
            .map_err(ledger_error)?
        else {
            return Err(invalid(
                "source",
                "is not an issued invoice or credit note you can read",
            ));
        };
        let authority = Self::authority();
        let tenant = scope.tenant();
        let credit_note = source.kind == FiscalSourceKind::CreditNote;
        let existing = newest(client, tenant, company_id, module, &[record.to_owned()])
            .await?
            .pop()
            .map(|(record, id, number, status)| standing(record, id, number, status));
        // A credit note's e-invoice refers to the e-invoice of its invoice.
        let original = match source.corrects.as_ref().filter(|_| credit_note) {
            Some(corrected) => {
                newest(client, tenant, company_id, module, &[corrected.clone()])
                    .await?
                    .pop()
            }
            None => None,
        };
        let buyer = self
            .buyer(client, tenant, company_id, &source, &authority)
            .await?;
        let descriptions: Vec<String> = source
            .lines
            .iter()
            .map(|line| line.description.clone())
            .collect();
        let remembered = client
            .query(
                "SELECT DISTINCT ON (l.description) l.description, l.classification_code, l.tax_type \
                 FROM finance_einvoice_document_lines l \
                 WHERE l.tenant_id = $1 AND l.company_id = $2 AND l.description = ANY($3) \
                 ORDER BY l.description, l.document_id DESC",
                &[&tenant, &company_id, &descriptions],
            )
            .await?;
        let lines: Vec<Value> = source
            .lines
            .iter()
            .map(|line| {
                let memory = remembered
                    .iter()
                    .find(|row| row.get::<_, String>(0) == line.description);
                let taxed = line.tax_rate.is_some_and(|rate| !rate.is_zero());
                // A remembered tax type only when it suits the rate.
                let tax_type = authority
                    .tax_type_for(line.tax_rate)
                    .map(str::to_owned)
                    .or_else(|| memory.map(|row| row.get::<_, String>(2)))
                    .filter(|_| taxed || authority.tax_type_for(line.tax_rate).is_some());
                json!({
                    "description": line.description,
                    "quantity": line.quantity.normalize().to_string(),
                    "unit_price": line.unit_price.normalize().to_string(),
                    "tax_rate": line.tax_rate.unwrap_or_default().normalize().to_string(),
                    "tax_type": tax_type,
                    "classification_code": memory.map(|row| row.get::<_, String>(1)),
                })
            })
            .collect();
        // A number this company already used, in either environment, cannot
        // be issued again: propose the next free one.
        let mut code_number = source.number.clone();
        for attempt in 2..10 {
            let taken: bool = client
                .query_opt(
                    "SELECT EXISTS (SELECT 1 FROM finance_einvoice_documents \
                     WHERE tenant_id = $1 AND company_id = $2 AND code_number = $3)",
                    &[&tenant, &company_id, &code_number],
                )
                .await?
                .is_some_and(|row| row.get(0));
            if !taken {
                break;
            }
            code_number = format!("{}-{attempt}", source.number);
        }
        let mut draft = json!({
            "document_type": authority.source_document_type(credit_note),
            "code_number": code_number,
            "currency_code": source.currency_code,
            "buyer": buyer,
            "lines": lines,
            "source": {"module": module, "record": record, "ledger_entry_id": source.ledger_entry_id},
        });
        if let Some((_, id, _, _)) = &original {
            draft["original_document_id"] = json!(id);
        }
        Ok(json!({
            "draft": draft,
            "source": {
                "module": module, "record": record, "number": source.number,
                "kind": if credit_note { "credit_note" } else { "invoice" },
                "party_name": source.party.name, "issue_date": source.issue_date,
                "total": source.total.to_string(), "currency_code": source.currency_code,
                "corrects": source.corrects,
            },
            "document": existing,
            "original": original.map(|(_, id, number, status)| json!({
                "id": id, "code_number": number, "status": status,
            })),
        }))
    }

    /// The buyer as the record froze it, with the gaps filled from the last
    /// e-invoice this company issued to the same tax ID (or, without one, the
    /// same name).
    async fn buyer(
        &self,
        client: &dyn OperationClient,
        tenant: i64,
        company_id: i64,
        source: &FiscalSource,
        authority: &impl Authority,
    ) -> Result<DraftParty> {
        let party = &source.party;
        let self_billed: Vec<&str> = authority.self_billed_document_types();
        let last = client
            .query_opt(
                "SELECT d.buyer FROM finance_einvoice_documents d \
                 WHERE d.tenant_id = $1 AND d.company_id = $2 AND NOT (d.document_type = ANY($5)) \
                   AND CASE WHEN $3::text IS NULL THEN d.buyer->>'name' = $4 \
                            ELSE d.buyer->>'tax_id' = $3 END \
                 ORDER BY d.id DESC LIMIT 1",
                &[
                    &tenant,
                    &company_id,
                    &party.tax_id,
                    &party.name,
                    &self_billed,
                ],
            )
            .await?
            .and_then(|row| serde_json::from_value::<PartySnapshot>(row.get(0)).ok())
            .unwrap_or_default();
        let or = |own: &Option<String>, last: Option<String>| {
            if blank(own) {
                last
            } else {
                own.clone()
            }
        };
        let own_address = !party.address_lines.is_empty();
        Ok(DraftParty {
            name: party.name.clone(),
            tax_id: or(&party.tax_id, last.tax_id),
            registration_type: last.registration_type,
            registration_number: or(&party.registration_number, last.registration_number),
            sales_tax_registration: last.sales_tax_registration,
            email: or(&party.email, last.email),
            phone: or(&party.phone, last.phone),
            industry_code: None,
            business_activity: None,
            address: Some(DraftAddress {
                lines: if own_address {
                    party.address_lines.clone()
                } else {
                    last.address.lines
                },
                city: or(&party.city, last.address.city),
                postcode: or(&party.postcode, last.address.postcode),
                // The record holds no authority state code; the last one
                // sent for this buyer is the best guess.
                state_code: last.address.state_code,
                country_code: party
                    .country
                    .as_deref()
                    .and_then(|iso| authority.country_code(iso))
                    .map(str::to_owned)
                    .or(last.address.country_code),
            }),
        })
    }

    /// The codes of the lists a person chooses from when starting an
    /// e-invoice (`finance.einvoice.view`): list to code to description.
    pub(crate) async fn choice_codes_on(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company_id: i64,
    ) -> Result<Value> {
        bilibi_company::authorize_work_target(client, scope, company_id, VIEW)
            .await
            .map_err(crate::refusal)?;
        let authority = Self::authority();
        let mut lists = serde_json::Map::new();
        for list in ["classification", "state", "country", "tax_type"] {
            let codes =
                code_lists::codes(client, scope.tenant(), company_id, authority.id(), list).await?;
            lists.insert(list.to_owned(), json!(codes));
        }
        lists.insert(
            "registration_type".into(),
            Value::Object(
                authority
                    .registration_types()
                    .into_iter()
                    .map(|(code, name)| (code.to_owned(), json!(name)))
                    .collect(),
            ),
        );
        Ok(Value::Object(lists))
    }
}

fn ledger_error(error: bilibi_finance_ledger::LedgerError) -> EinvoiceError {
    match error {
        bilibi_finance_ledger::LedgerError::Failed(error) => EinvoiceError::Failed(error),
        _ => EinvoiceError::Failed(bilibi_database::Error::Internal),
    }
}
