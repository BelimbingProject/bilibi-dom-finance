//! Bilimbi.Finance.Sst.Mappings: which box each tax code of receivables and
//! payables reports in. A mapping is never changed: the next one for the same
//! code replaces it, and a return reads them through the mapping number it
//! records. Tax codes are read only through their owners' facades.
use crate::store::{self, Table};
use crate::*;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap;

/// One recorded mapping: `box_key` is `None` when the code is deliberately
/// not reported (a code for a tax other than SST).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mapping {
    pub id: i64,
    pub mapping_number: i64,
    pub source: TaxSource,
    pub tax_code_id: i64,
    pub tax_code: String,
    #[serde(rename = "box")]
    pub box_key: Option<String>,
    pub reason: Option<String>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}

/// A receivables or payables tax code with its current mapping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaxCodeMapping {
    pub source: TaxSource,
    pub id: i64,
    pub code: String,
    pub name: String,
    pub rate: Decimal,
    pub account_id: i64,
    pub active: bool,
    pub mapping: Option<Mapping>,
}

/// What maps a tax code: `source` (`receivables` or `payables`), its
/// `tax_code_id`, the `box_key` it reports in (one of
/// [`Sst::mappable_boxes`]; blank or `None` for not reported) and an
/// optional `reason`.
#[derive(Clone, Debug, Default)]
pub struct MappingInput {
    pub source: String,
    pub tax_code_id: i64,
    pub box_key: Option<String>,
    pub reason: Option<String>,
}

const COLUMNS: &str = "id,mapping_number,source,tax_code_id,tax_code,box,reason,recorded_by_user_id,recorded_by_impersonator_id";

fn row(r: &tokio_postgres::Row) -> bilibi_database::Result<Mapping> {
    Ok(Mapping {
        id: r.get(0),
        mapping_number: r.get(1),
        source: r.get::<_, &str>(2).parse().map_err(|_| Error::Invalid)?,
        tax_code_id: r.get(3),
        tax_code: r.get(4),
        box_key: r.get(5),
        reason: r.get(6),
        recorded_by_user_id: r.get(7),
        recorded_by_impersonator_id: r.get(8),
        recorded_at: r.get(9),
    })
}

/// The last mapping number the company recorded, 0 for none.
pub(crate) async fn last_number(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
) -> bilibi_database::Result<i64> {
    Ok(c.query_one(
        "SELECT COALESCE(max(mapping_number),0) FROM finance_sst_mappings WHERE tenant_id=$1 AND company_id=$2",
        &[&tenant, &company],
    )
    .await?
    .get(0))
}

/// Each code's latest mapping numbered through `through`.
pub(crate) async fn at(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    through: i64,
) -> bilibi_database::Result<HashMap<(TaxSource, i64), Mapping>> {
    let sql = format!(
        "SELECT DISTINCT ON (source,tax_code_id) {COLUMNS},{} FROM finance_sst_mappings WHERE tenant_id=$1 AND company_id=$2 AND mapping_number<=$3 ORDER BY source,tax_code_id,mapping_number DESC",
        store::stamp("recorded_at")
    );
    c.query(&sql, &[&tenant, &company, &through])
        .await?
        .iter()
        .map(|r| row(r).map(|m| ((m.source, m.tax_code_id), m)))
        .collect()
}

impl Sst {
    /// Every tax code of receivables and then payables, each with its current
    /// mapping or `None`. Also needs `finance.receivables.view` and
    /// `finance.payables.view`.
    pub async fn list_tax_codes(&self, s: &Scope, company: i64) -> Result<Vec<TaxCodeMapping>> {
        {
            let c = self.db.get().await.map_err(Error::from)?;
            authorize(&**c, s, company, VIEW).await?;
        }
        let mut codes = Vec::new();
        for source in TaxSource::ALL {
            codes.extend(
                self.source_codes(s, company, source)
                    .await?
                    .into_iter()
                    .map(|c| (source, c)),
            );
        }
        let c = self.db.get().await.map_err(Error::from)?;
        let mut current = at(&**c, s.tenant(), company, i64::MAX).await?;
        Ok(codes
            .into_iter()
            .map(|(source, code)| TaxCodeMapping {
                mapping: current.remove(&(source, code.id)),
                source,
                id: code.id,
                code: code.code,
                name: code.name,
                rate: code.rate,
                account_id: code.account_id,
                active: code.active,
            })
            .collect())
    }

    /// Maps a tax code to a box, or to none (deliberately not reported), as
    /// the code's next mapping (`finance.sst.settings.manage`); returns the
    /// code's current mapping alone when nothing changes. Refusals:
    /// `InvalidSource`, `TaxCodeNotFound`, and `Invalid` on `box` or
    /// `reason`. Also needs the source module's view capability.
    pub async fn map_tax_code(
        &self,
        s: &Scope,
        company: i64,
        input: &MappingInput,
    ) -> Result<Mapping> {
        {
            let c = self.db.get().await.map_err(Error::from)?;
            authorize(&**c, s, company, MANAGE_SETTINGS).await?;
        }
        let source: TaxSource = input.source.parse()?;
        // Read through the owner's pooled facade before holding the company
        // lock: tax codes are never deleted, and a code deactivated since
        // still reports what it carried.
        let code = self
            .source_codes(s, company, source)
            .await?
            .into_iter()
            .find(|c| c.id == input.tax_code_id)
            .ok_or(SstError::TaxCodeNotFound)?;
        let box_key = bilibi_ui::params::blank_to_none(input.box_key.as_deref());
        let mut errors = Vec::new();
        if box_key.as_deref().is_some_and(|key| {
            !form::mappable_boxes(source)
                .iter()
                .any(|b| b.form_box.key == key)
        }) {
            errors.push(FieldError::new(
                "box",
                format!("is not a box a {} tax code reports in", source.as_str()),
            ));
            return Err(SstError::Invalid(errors));
        }
        let reason = reason(input.reason.as_deref(), &mut errors);
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                MANAGE_SETTINGS,
                "finance.sst.map-tax-code",
            )
            .await?;
        let tenant = s.tenant();
        if let Some(current) = at(tx.sql(), tenant, company, i64::MAX)
            .await?
            .remove(&(source, code.id))
            .filter(|m| m.box_key == box_key)
        {
            return Ok(current);
        }
        if !errors.is_empty() {
            return Err(SstError::Invalid(errors));
        }
        let mut value = store::identity(s, company);
        value.as_object_mut().ok_or(Error::Invalid)?.extend(
            json!({
                "source": source.as_str(),
                "tax_code_id": code.id,
                "tax_code": code.code,
                "box": box_key,
                "reason": reason,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (id, change) = store::insert(sql, Table::Mapping, value).await?;
                    Ok((id, vec![change]))
                })
            })
            .await?;
        let sql = format!(
            "SELECT {COLUMNS},{} FROM finance_sst_mappings WHERE tenant_id=$1 AND company_id=$2 AND id=$3",
            store::stamp("recorded_at")
        );
        let mapping = row(&tx.sql().query_one(&sql, &[&tenant, &company, &id]).await?)?;
        tx.commit().await?;
        Ok(mapping)
    }

    /// Every mapping recorded, oldest first: its history. `limit` (1 to 500,
    /// default 100) and `after`, the last id already read, page it.
    pub async fn list_mappings(
        &self,
        s: &Scope,
        company: i64,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<Vec<Mapping>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let (limit, after) = page_options(limit, after)?;
        let sql = format!(
            "SELECT {COLUMNS},{} FROM finance_sst_mappings WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4",
            store::stamp("recorded_at")
        );
        let rows = c
            .query(&sql, &[&s.tenant(), &company, &after, &limit])
            .await?;
        Ok(rows
            .iter()
            .map(row)
            .collect::<bilibi_database::Result<_>>()?)
    }

    /// One source's tax codes through its owner's facade, in its order.
    async fn source_codes(
        &self,
        s: &Scope,
        company: i64,
        source: TaxSource,
    ) -> Result<Vec<SourceCode>> {
        Ok(match source {
            TaxSource::Receivables => self
                .receivables
                .list_tax_codes(s, company)
                .await?
                .into_iter()
                .map(|c| SourceCode {
                    id: c.id,
                    code: c.code,
                    name: c.name,
                    rate: c.rate,
                    account_id: c.account_id,
                    active: c.active,
                })
                .collect(),
            TaxSource::Payables => self
                .payables
                .list_tax_codes(s, company)
                .await?
                .into_iter()
                .map(|c| SourceCode {
                    id: c.id,
                    code: c.code,
                    name: c.name,
                    rate: c.rate,
                    account_id: c.account_id,
                    active: c.active,
                })
                .collect(),
        })
    }
}

/// A tax code as either owner's facade lists it.
struct SourceCode {
    id: i64,
    code: String,
    name: String,
    rate: Decimal,
    account_id: i64,
    active: bool,
}
