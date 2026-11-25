//! Issuance captures the issuer and original document before posting.
use crate::{invalid, Document, Receivables, Result};
use bilibi_agent_api::OperationClient;
use bilibi_database::Error;
use bilibi_finance_ledger::{sst_numbers_on, SstNumbers};
use bilibi_party::PartyId;
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_postgres::GenericClient;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Ringgit {
    pub net: Decimal,
    pub tax: Decimal,
    pub total: Decimal,
    pub rate: Decimal,
}

impl Ringgit {
    async fn from_currency(
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        currencies: &bilibi_currency::Currencies,
        document: &Document,
        on: Date,
    ) -> Result<Self> {
        let [net, tax, total] = [document.subtotal, document.tax_total, document.total];
        let subtotal = currencies
            .convert(
                client,
                scope,
                &bilibi_currency::Request::new(net.amount(), &document.currency_code, "MYR", on),
            )
            .await?
            .map_err(|refusal| invalid("issue_date", refusal.message()))?;
        let tax = currencies
            .convert(
                client,
                scope,
                &bilibi_currency::Request::new(tax.amount(), &document.currency_code, "MYR", on),
            )
            .await?
            .map_err(|refusal| invalid("issue_date", refusal.message()))?;
        let total = currencies
            .convert(
                client,
                scope,
                &bilibi_currency::Request::new(total.amount(), &document.currency_code, "MYR", on),
            )
            .await?
            .map_err(|refusal| invalid("issue_date", refusal.message()))?;
        Ok(Self {
            net: subtotal.amount,
            tax: tax.amount,
            total: total.amount,
            rate: subtotal.quote.rate,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct PrintSnapshot {
    pub seller_name: String,
    pub seller_registration: Option<String>,
    pub seller_tax: Option<String>,
    pub seller_sst: SstNumbers,
    pub ringgit: Option<Ringgit>,
    pub seller_address: Vec<String>,
    pub original_invoice: Option<String>,
    pub original_date: Option<String>,
}

pub(super) struct PrintIssue<'a> {
    pub date: Date,
    pub document: &'a Document,
    pub original: Option<&'a Document>,
    pub configured: &'a Value,
}

impl Receivables {
    pub(super) async fn prepare_print_snapshot(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        issue: PrintIssue<'_>,
    ) -> Result<PrintSnapshot> {
        let PrintIssue {
            date,
            document,
            original,
            configured,
        } = issue;
        let (issuer, fields) = bilibi_company::detail(client, scope, company)
            .await
            .map_err(|error| match error {
                Error::Forbidden => invalid(
                    "company",
                    "needs Companies view permission to record the supplier",
                ),
                other => other.into(),
            })?;
        if fields.contains("registration_number") || fields.contains("tax_id") {
            return Err(invalid(
                "company",
                "needs permission to read the supplier's registration and tax identifiers",
            ));
        }

        let organisation = PartyId::Organisation(issuer.organisation_id()?);
        let address_date = date.to_string().try_into().map_err(|_| Error::Internal)?;
        let mut seller_address: Vec<String> =
            bilibi_address::billing_address(client, scope, organisation, &address_date)
                .await
                .map_err(|error| match error {
                    Error::Forbidden => invalid(
                        "company",
                        "needs Addresses view permission to record the supplier's address",
                    ),
                    other => other.into(),
                })?
                .map(|attached| {
                    let address = attached.address.address;
                    [
                        address.line1,
                        address.line2,
                        address.line3,
                        address.postcode,
                        address.locality,
                        address.admin1_code,
                        address.country_iso,
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|line| !line.trim().is_empty())
                    .collect()
                })
                .unwrap_or_default();
        if seller_address.is_empty() {
            seller_address.extend(
                configured["address"]
                    .as_str()
                    .into_iter()
                    .flat_map(str::lines)
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned),
            );
        }
        let taxable = document
            .tax_lines
            .iter()
            .any(|line| line.tax_rate > Decimal::ZERO);
        if taxable && seller_address.is_empty() {
            return Err(invalid(
                "company",
                "Record a supplier address before issuing a taxable document.",
            ));
        }
        let seller_name = issuer.display_name();
        if seller_name.trim().is_empty() {
            return Err(invalid(
                "company",
                "Record a supplier name before issuing the document.",
            ));
        }
        let seller_sst = sst_numbers_on(client, scope, company, date).await?;
        let ringgit = if taxable
            && document.currency_code != "MYR"
            && (seller_sst.sales.is_some() || seller_sst.service.is_some())
        {
            let currencies = &self.currencies;
            let amounts = match client.transaction() {
                Some(sql) => Ringgit::from_currency(sql, scope, currencies, document, date).await?,
                None => {
                    Ringgit::from_currency(
                        client.sql_client().await?,
                        scope,
                        currencies,
                        document,
                        date,
                    )
                    .await?
                }
            };
            Some(amounts)
        } else {
            None
        };
        Ok(PrintSnapshot {
            seller_name,
            seller_registration: issuer
                .registration_number
                .filter(|number| !number.is_empty()),
            seller_tax: if issuer.tax_id.trim().is_empty() {
                configured["tax_number"]
                    .as_str()
                    .map(str::trim)
                    .filter(|number| !number.is_empty())
                    .map(str::to_owned)
            } else {
                Some(issuer.tax_id)
            },
            seller_address,
            seller_sst,
            ringgit,
            original_invoice: original.and_then(|doc| doc.document_reference.clone()),
            original_date: original.and_then(|doc| doc.issue_date.clone()),
        })
    }
}
