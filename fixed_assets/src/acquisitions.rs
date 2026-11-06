//! Bilimbi.Finance.FixedAssets register: the company lock serializes acquisition
//! with source reversal and duplicate registration. Sources keep their owners.
use crate::*;
use bilibi_finance_ledger::Entry;
use jiff::ToSpan;
use serde_json::json;
use store::Table;

impl FixedAssets {
    pub async fn acquire_from_bill(
        &self,
        s: &Scope,
        company: i64,
        bill_id: i64,
        bill_line_number: i32,
        input: &AcquisitionInput,
    ) -> Result<Asset> {
        self.acquire(s, company, bill_id, bill_line_number, true, input)
            .await
    }
    /// As in Bilimbi, this path accepts any live original acquisition debit,
    /// not only ownerless entries. It does not infer optional bill provenance.
    pub async fn acquire_from_entry(
        &self,
        s: &Scope,
        company: i64,
        entry_id: i64,
        entry_line_number: i32,
        input: &AcquisitionInput,
    ) -> Result<Asset> {
        self.acquire(s, company, entry_id, entry_line_number, false, input)
            .await
    }
    async fn acquire(
        &self,
        s: &Scope,
        company: i64,
        source_id: i64,
        line_number: i32,
        bill: bool,
        input: &AcquisitionInput,
    ) -> Result<Asset> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(&mut c, s, company, ACQUIRE, "finance.fixed-assets.acquire")
            .await?;
        let asset = self
            .acquire_on(&mut tx, s, company, source_id, line_number, bill, input)
            .await?;
        tx.commit().await?;
        Ok(asset)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn prepare_acquisition(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        source_id: i64,
        line_number: i32,
        bill: bool,
        input: &AcquisitionInput,
    ) -> Result<serde_json::Value> {
        self.opening_authorize(c, s, company, ACQUIRE).await?;
        if source_id <= 0 {
            return Err(invalid(
                if bill { "bill_id" } else { "entry_id" },
                "must be greater than 0",
            ));
        }
        if line_number <= 0 {
            return Err(invalid(
                if bill {
                    "bill_line_number"
                } else {
                    "entry_line_number"
                },
                "must be greater than 0",
            ));
        }
        let entry_id = if bill {
            let doc = self
                .payables
                .get_document_on(c, s, company, source_id)
                .await?;
            if doc.document_type != "bill" || doc.status != "posted" {
                return Err(FixedAssetsError::BillNotPosted);
            }
            if !doc.lines.iter().any(|l| l.line_number == line_number) {
                return Err(FixedAssetsError::BillLineNotFound);
            }
            doc.entry_id.ok_or(Error::Invalid)?
        } else {
            source_id
        };
        let entry = self.ledger.get_entry_on(c, s, company, entry_id).await?;
        if let Ok(opening) = self.ledger.opening_context_on(c, s, company).await {
            if entry.id == opening.entry.id {
                return Err(invalid(
                    "entry_id",
                    "is an opening balance; import the opening asset register instead of registering its aggregate debit",
                ));
            }
        }
        let revision = store::revision(c, s.tenant(), company, input.category_revision_id).await?;
        let book = self.ledger.get_book_on(c, s, company).await?;
        let cost = acquisition_debit(&entry, line_number, revision.defaults.cost_account_id)?;
        if c.query_opt("SELECT id FROM finance_fixed_assets_assets WHERE tenant_id=$1 AND company_id=$2 AND entry_id=$3 AND entry_line_number=$4",&[&s.tenant(),&company,&entry_id,&line_number]).await?.is_some() {return Err(invalid("entry_id","has already been taken"));}
        let description = text(&input.description, "description", 500)?;
        if input.in_service_date < entry.entry_date {
            return Err(invalid("in_service_date", "cannot precede acquisition"));
        }
        let residual = Money::new(
            input.residual_value.unwrap_or(Money::ZERO).amount(),
            book.minor_units,
        )
        .map_err(|e| invalid("residual_value", e.to_string()))?;
        if residual < Money::ZERO {
            return Err(invalid(
                "residual_value",
                "must be greater than or equal to 0",
            ));
        }
        if residual > cost {
            return Err(invalid(
                "residual_value",
                format!("must be less than or equal to {cost}"),
            ));
        }
        let a = schedule::assumptions(Assumptions {
            method: input.method.unwrap_or(revision.defaults.assumptions.method),
            life_months: input
                .life_months
                .unwrap_or(revision.defaults.assumptions.life_months),
            annual_rate: input
                .annual_rate
                .or(revision.defaults.assumptions.annual_rate),
        })?;
        input
            .in_service_date
            .first_of_month()
            .checked_add((a.life_months - 1).months())
            .map_err(|_| invalid("in_service_date", "is invalid"))?;

        let values = json!({"category_revision_id":revision.id,"description":description,"acquisition_date":entry.entry_date.to_string(),"in_service_date":input.in_service_date.to_string(),"cost":cost,"residual_value":residual,"method":a.method,"life_months":a.life_months,"annual_rate":a.annual_rate,"entry_id":entry.id,"entry_number":entry.entry_number,"entry_line_number":line_number,"bill_id":bill.then_some(source_id),"bill_line_number":bill.then_some(line_number)});
        Ok(values)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn acquire_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        source_id: i64,
        line_number: i32,
        bill: bool,
        input: &AcquisitionInput,
    ) -> Result<Asset> {
        self.lifecycle_writable(tx, s, company, ACQUIRE).await?;
        let values = self
            .prepare_acquisition(tx.sql(), s, company, source_id, line_number, bill, input)
            .await?;
        let book = self.ledger.get_book_on(tx.sql(), s, company).await?;
        let mut row = store::identity(s, company);
        row.as_object_mut()
            .ok_or(Error::Invalid)?
            .extend(values.as_object().ok_or(Error::Invalid)?.clone());
        let value = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (v, change) = store::insert(sql, Table::Asset, row).await?;
                    Ok((v, vec![change]))
                })
            })
            .await?;
        let asset = store::asset(&value, book.minor_units)?;
        Ok(asset)
    }
    pub async fn get_asset(&self, s: &Scope, company: i64, id: i64) -> Result<AssetDetail> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let tx = c
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        let detail = self.asset_on(&*tx, s, company, id).await?;
        tx.commit().await?;
        Ok(detail)
    }
    pub(super) async fn asset_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<AssetDetail> {
        self.opening_authorize(c, s, company, VIEW).await?;
        let v = store::raw(c, Table::Asset, s.tenant(), company, id)
            .await?
            .ok_or(FixedAssetsError::NotFound)?;
        let book = self.ledger.get_book_on(c, s, company).await?;
        let asset = store::asset(&v, book.minor_units)?;
        let revision = store::revision(c, s.tenant(), company, asset.category_revision_id).await?;
        let schedule = schedule::rows(&asset, book.minor_units)?;
        let activity = activity::Activity::load(c, s, company, book.minor_units).await?;
        let voided = activity.voided(id);
        let events = activity
            .events
            .iter()
            .filter(|e| e.asset_id == Some(id))
            .cloned()
            .collect();
        let mut charges = Vec::new();
        for run in &activity.runs {
            for item in run.items.iter().filter(|i| i.asset_id == id) {
                let reversal = activity
                    .events
                    .iter()
                    .filter(|e| {
                        (e.run_id == Some(run.id) && e.action == EventAction::RunReversed)
                            || (e.asset_id == Some(id)
                                && e.action == EventAction::AcquisitionVoided)
                    })
                    .min_by_key(|e| e.event_number)
                    .cloned();
                charges.push(AssetCharge {
                    run_id: run.id,
                    period: run.period,
                    amount: item.amount,
                    entry_id: run.entry_id,
                    entry_number: run.entry_number,
                    recorded_by_user_id: run.recorded_by_user_id,
                    recorded_by_impersonator_id: run.recorded_by_impersonator_id,
                    recorded_at: run.recorded_at.clone(),
                    reversal,
                });
            }
        }
        let opening=c.query_opt("SELECT i.source_facts || jsonb_build_object('source_reference',b.source_reference) FROM finance_fixed_assets_opening_items i JOIN finance_fixed_assets_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.asset_id=$3",&[&s.tenant(),&company,&id]).await?.map(|r|r.get(0));
        Ok(AssetDetail {
            opening,
            asset,
            revision,
            schedule,
            charges,
            events,
            voided,
        })
    }
}
/// Most acquisition debits a register is offered at once, newest first.
const SOURCE_LIMIT: usize = 500;
/// Entries read from the cost account before the list stops, in pages of 500.
const SOURCE_SCAN: usize = 20_000;

impl FixedAssets {
    /// The posted debits a category's cost account still has to register,
    /// newest first: each line that `acquire_from_bill` or
    /// `acquire_from_entry` would accept for this revision, so a person picks
    /// the bill or journal line instead of typing its numbers. A line of a
    /// posted bill is offered as that bill's line when this person may read
    /// the bill, otherwise as its journal line. The register still decides
    /// every refusal when it is saved.
    pub async fn acquisition_sources(
        &self,
        s: &Scope,
        company: i64,
        category_revision_id: i64,
    ) -> Result<Vec<AcquisitionSource>> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.acquisition_sources_on(&**c, s, company, category_revision_id)
            .await
    }
    pub(super) async fn acquisition_sources_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        category_revision_id: i64,
    ) -> Result<Vec<AcquisitionSource>> {
        self.opening_authorize(c, s, company, VIEW).await?;
        let revision = store::revision(c, s.tenant(), company, category_revision_id).await?;
        let cost_account = revision.defaults.cost_account_id;
        let opening = self
            .ledger
            .opening_context_on(c, s, company)
            .await
            .ok()
            .map(|o| o.entry.id);
        let taken: std::collections::HashSet<(i64, i32)> = c
            .query(
                "SELECT entry_id, entry_line_number FROM finance_fixed_assets_assets WHERE tenant_id=$1 AND company_id=$2 AND entry_line_number IS NOT NULL",
                &[&s.tenant(), &company],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let mut bills = std::collections::HashMap::new();
        let mut found = Vec::new();
        let mut query = bilibi_finance_ledger::EntryQuery {
            limit: Some(500),
            account_id: Some(cost_account),
            ..Default::default()
        };
        let mut scanned = 0;
        loop {
            let entries = self.ledger.list_entries_on(c, s, company, &query).await?;
            scanned += entries.len();
            for entry in &entries {
                if Some(entry.id) == opening
                    || entry.reversed_by_entry_id.is_some()
                    || entry.reverses_entry_id.is_some()
                {
                    continue;
                }
                for line in &entry.lines {
                    if line.account_id != cost_account
                        || !line.debit.is_positive()
                        || !line.credit.is_zero()
                        || taken.contains(&(entry.id, line.line_number))
                    {
                        continue;
                    }
                    let bill_id = entry
                        .owner
                        .as_ref()
                        .filter(|o| o.module == bilibi_finance_payables::MODULE)
                        .and_then(|o| o.record.strip_prefix("document:"))
                        .and_then(|id| id.parse::<i64>().ok());
                    let bill = match bill_id {
                        Some(id) => {
                            if let std::collections::hash_map::Entry::Vacant(slot) = bills.entry(id)
                            {
                                // A person who may not read bills still sees the journal line.
                                let doc = self
                                    .payables
                                    .get_document_on(c, s, company, id)
                                    .await
                                    .ok()
                                    .filter(|d| d.document_type == "bill" && d.status == "posted");
                                slot.insert(doc);
                            }
                            bills[&id].as_ref()
                        }
                        None => None,
                    };
                    let line_text = line.description.clone().filter(|d| !d.trim().is_empty());
                    found.push(match bill {
                        Some(doc) => AcquisitionSource {
                            source: SourceKind::Bill,
                            source_id: doc.id,
                            line_number: line.line_number,
                            entry_id: entry.id,
                            entry_number: entry.entry_number,
                            acquisition_date: entry.entry_date,
                            amount: line.debit,
                            description: doc
                                .lines
                                .iter()
                                .find(|l| l.line_number == line.line_number)
                                .map(|l| l.description.clone())
                                .or(line_text)
                                .unwrap_or_else(|| entry.description.clone()),
                            bill_number: doc
                                .party_document_number
                                .clone()
                                .or_else(|| doc.document_number.map(|n| n.to_string())),
                            supplier: Some(doc.party.name.clone()),
                        },
                        None => AcquisitionSource {
                            source: SourceKind::Entry,
                            source_id: entry.id,
                            line_number: line.line_number,
                            entry_id: entry.id,
                            entry_number: entry.entry_number,
                            acquisition_date: entry.entry_date,
                            amount: line.debit,
                            description: line_text.unwrap_or_else(|| entry.description.clone()),
                            bill_number: None,
                            supplier: None,
                        },
                    });
                }
            }
            match entries.last() {
                Some(last) if entries.len() == 500 && scanned < SOURCE_SCAN => {
                    query.after = Some((last.entry_date, last.id));
                }
                _ => break,
            }
        }
        found.sort_by(|a, b| {
            (b.acquisition_date, b.entry_number, a.line_number).cmp(&(
                a.acquisition_date,
                a.entry_number,
                b.line_number,
            ))
        });
        found.truncate(SOURCE_LIMIT);
        Ok(found)
    }
}

/// Where an acquisition debit was posted from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Bill,
    Entry,
}
/// One debit line `acquisition_sources` offers; `source`, `source_id` and
/// `line_number` are what `asset.acquire` takes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionSource {
    pub source: SourceKind,
    pub source_id: i64,
    pub line_number: i32,
    pub entry_id: i64,
    pub entry_number: i64,
    #[serde(with = "date_serde")]
    pub acquisition_date: Date,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub amount: Money,
    pub description: String,
    /// The supplier's invoice number, or the bill's own number.
    pub bill_number: Option<String>,
    pub supplier: Option<String>,
}

fn acquisition_debit(entry: &Entry, number: i32, cost_account: i64) -> Result<Money> {
    if entry.reversed_by_entry_id.is_some() || entry.reverses_entry_id.is_some() {
        return Err(FixedAssetsError::AcquisitionReversed);
    }
    let line = entry
        .lines
        .iter()
        .find(|l| l.line_number == number)
        .ok_or(FixedAssetsError::EntryLineNotFound)?;
    if line.account_id != cost_account || !line.debit.is_positive() || !line.credit.is_zero() {
        return Err(FixedAssetsError::NotAcquisitionDebit);
    }
    Ok(line.debit)
}
