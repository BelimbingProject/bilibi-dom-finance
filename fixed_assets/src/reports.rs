//! Bilimbi.Finance.FixedAssets register_report/depreciation_schedule_report.
//! Four independent immutable-stream pins reproduce backdated activity too.
use crate::*;
use activity::Activity;
use bilibi_finance_ledger::{AccountTotal, MinorUnits, OwnerTotal, TotalsQuery};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use store::{int, Table};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportPins {
    pub through_entry_number: Option<i64>,
    pub through_asset_number: Option<i64>,
    pub through_event_number: Option<i64>,
    pub through_category_revision_id: Option<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconciliationKind {
    Balance,
    Activity,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    pub account_id: i64,
    pub kind: ReconciliationKind,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub ledger_balance: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub register_balance: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub difference: Money,
    /// Activity on a depreciation, gain or loss account that this module did
    /// not post (an invoice to a shared income account, a manual journal):
    /// shown beside the row, never counted as a register difference. Zero on
    /// a balance row, where every posting counts.
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub other_postings: Money,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterRow {
    pub asset: Asset,
    pub revision: CategoryRevision,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub accumulated: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub scheduled_depreciation: Money,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub carrying_value: Money,
    pub disposed: bool,
    pub voided: bool,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub disposal_gain: Money,
    pub schedule: Vec<ScheduleRow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterReport {
    pub company_id: i64,
    #[serde(with = "date_serde")]
    pub as_of: Date,
    pub currency_code: String,
    #[serde(deserialize_with = "money_serde::units")]
    pub minor_units: MinorUnits,
    pub through_entry_number: i64,
    pub through_asset_number: i64,
    pub through_event_number: i64,
    pub through_category_revision_id: i64,
    pub rows: Vec<RegisterRow>,
    pub reconciliation: Vec<Reconciliation>,
}
impl RegisterReport {
    pub fn pins(&self) -> ReportPins {
        ReportPins {
            through_entry_number: Some(self.through_entry_number),
            through_asset_number: Some(self.through_asset_number),
            through_event_number: Some(self.through_event_number),
            through_category_revision_id: Some(self.through_category_revision_id),
        }
    }
}
impl FixedAssets {
    /// Reproduce with the same date and all four returned boundaries.
    pub async fn register_report(
        &self,
        s: &Scope,
        company: i64,
        date: Date,
        pins: &ReportPins,
    ) -> Result<RegisterReport> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let tx = c
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        let report = self
            .register_report_on(&*tx, s, company, date, pins)
            .await?;
        tx.commit().await?;
        Ok(report)
    }
    pub async fn depreciation_schedule_report(
        &self,
        s: &Scope,
        company: i64,
        date: Date,
        pins: &ReportPins,
    ) -> Result<RegisterReport> {
        self.register_report(s, company, date, pins).await
    }
    /// Register and reconciliation on the caller's snapshot.
    pub async fn register_report_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        date: Date,
        pins: &ReportPins,
    ) -> Result<RegisterReport> {
        self.opening_authorize(c, s, company, VIEW).await?;
        if [
            pins.through_entry_number,
            pins.through_asset_number,
            pins.through_event_number,
            pins.through_category_revision_id,
        ]
        .into_iter()
        .flatten()
        .any(|n| n < 0)
        {
            return Err(FixedAssetsError::InvalidOption);
        }
        let query = TotalsQuery {
            to: Some(date),
            through_entry_number: pins.through_entry_number,
            ..Default::default()
        };
        let totals = self.ledger.account_totals_on(c, s, company, &query).await?;
        let through = totals.through_entry_number;
        let flows = self
            .ledger
            .account_totals_by_owner_on(
                c,
                s,
                company,
                &TotalsQuery {
                    through_entry_number: Some(through),
                    exclude_closing: true,
                    ..query
                },
            )
            .await?;
        let assets = store::all(c, Table::Asset, s.tenant(), company).await?;
        let revisions = store::all(c, Table::Revision, s.tenant(), company).await?;
        let mut activity = Activity::load(c, s, company, totals.minor_units).await?;
        let asset_pin = pins.through_asset_number.unwrap_or(
            assets
                .iter()
                .filter_map(|a| a["asset_number"].as_i64())
                .max()
                .unwrap_or(0),
        );
        let event_pin = pins.through_event_number.unwrap_or(
            activity
                .events
                .iter()
                .map(|e| e.event_number)
                .max()
                .unwrap_or(0),
        );
        let revision_pin = pins.through_category_revision_id.unwrap_or(
            revisions
                .iter()
                .filter_map(|r| r["id"].as_i64())
                .max()
                .unwrap_or(0),
        );
        activity.events.retain(|e| {
            e.event_number <= event_pin
                && e.effective_date <= date
                && e.entry_number.is_none_or(|n| n <= through)
        });
        activity.runs.retain(|r| {
            r.period.last_of_month() <= date && r.entry_number.is_none_or(|n| n <= through)
        });
        // Lifecycle state comes from the bounded events, not today's ledger reversal link.
        for run in &mut activity.runs {
            run.reversed = activity
                .events
                .iter()
                .any(|e| e.run_id == Some(run.id) && e.action == EventAction::RunReversed);
        }
        let mut revision_map = BTreeMap::new();
        for v in revisions
            .iter()
            .filter(|r| r["id"].as_i64().is_some_and(|n| n <= revision_pin))
        {
            let id = int(v, "id")?;
            revision_map.insert(id, store::revision(c, s.tenant(), company, id).await?);
        }
        let superseded:HashSet<i64>=c.query("SELECT i.asset_id FROM finance_fixed_assets_opening_items i JOIN finance_fixed_assets_opening_decisions d ON d.replaces_batch_id=i.batch_id AND d.tenant_id=$1 AND d.company_id=$2 JOIN (SELECT n.batch_id,max(a.asset_number) AS last_asset FROM finance_fixed_assets_opening_items n JOIN finance_fixed_assets_assets a ON a.id=n.asset_id AND a.tenant_id=$1 AND a.company_id=$2 WHERE n.tenant_id=$1 AND n.company_id=$2 GROUP BY n.batch_id) pinned ON pinned.batch_id=d.batch_id WHERE i.tenant_id=$1 AND i.company_id=$2 AND d.effective_date<=$4::text::date AND pinned.last_asset<=$3",&[&s.tenant(),&company,&asset_pin,&date.to_string()]).await?.iter().map(|r|r.get(0)).collect();
        let future:HashSet<i64>=c.query("SELECT i.asset_id FROM finance_fixed_assets_opening_items i JOIN finance_fixed_assets_opening_decisions d ON d.batch_id=i.batch_id AND d.tenant_id=$1 AND d.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND d.effective_date>$3::text::date",&[&s.tenant(),&company,&date.to_string()]).await?.iter().map(|r|r.get(0)).collect();
        let mut run_totals: HashMap<i64, Money> = HashMap::new();
        for item in activity
            .runs
            .iter()
            .filter(|r| !r.reversed)
            .flat_map(|r| &r.items)
        {
            let total = run_totals.entry(item.asset_id).or_insert(Money::ZERO);
            *total = *total + item.amount;
        }
        let mut rows = Vec::new();
        for v in assets {
            let a = store::asset(&v, totals.minor_units)?;
            if superseded.contains(&a.id)
                || future.contains(&a.id)
                || a.asset_number > asset_pin
                || a.entry_number > through
                || a.acquisition_date > date
                || a.opening_date.is_some_and(|opening| opening > date)
            {
                continue;
            }
            let Some(revision) = revision_map.get(&a.category_revision_id) else {
                continue;
            };
            let voided = activity.voided(a.id);
            let disposal = activity.disposal(a.id);
            let accumulated = if voided {
                Money::ZERO
            } else {
                a.opening_accumulated + run_totals.get(&a.id).copied().unwrap_or(Money::ZERO)
            };
            let schedule = schedule::rows(&a, totals.minor_units)?;
            let scheduled_depreciation = if voided {
                Money::ZERO
            } else {
                schedule
                    .iter()
                    .filter(|r| r.period.last_of_month() <= date)
                    .map(|r| r.amount)
                    .sum::<Money>()
                    + a.opening_accumulated
            };
            let carrying_value = if voided || disposal.is_some() {
                Money::ZERO
            } else {
                a.cost - accumulated
            };
            let disposal_gain = if let Some(e) = disposal {
                e.proceeds.ok_or(Error::Invalid)?
                    - (a.cost - e.accumulated.ok_or(Error::Invalid)?)
            } else {
                Money::ZERO
            };
            rows.push(RegisterRow {
                asset: a,
                revision: revision.clone(),
                accumulated,
                scheduled_depreciation,
                carrying_value,
                disposed: disposal.is_some(),
                voided,
                disposal_gain,
                schedule,
            });
        }
        rows.sort_by_key(|r| r.asset.asset_number);
        let reconciliation = reconcile(&rows, &totals.rows, &flows.rows, revision_map.values());
        Ok(RegisterReport {
            company_id: company,
            as_of: date,
            currency_code: totals.currency_code,
            minor_units: totals.minor_units,
            through_entry_number: through,
            through_asset_number: asset_pin,
            through_event_number: event_pin,
            through_category_revision_id: revision_pin,
            rows,
            reconciliation,
        })
    }
}
fn reconcile<'a>(
    rows: &[RegisterRow],
    totals: &[AccountTotal],
    flows: &[OwnerTotal],
    revisions: impl Iterator<Item = &'a CategoryRevision>,
) -> Vec<Reconciliation> {
    let mut expected = BTreeMap::<i64, Money>::new();
    let mut add = |id, amount| {
        let total = expected.entry(id).or_insert(Money::ZERO);
        *total = *total + amount;
    };
    for row in rows.iter().filter(|r| !r.voided) {
        let r = &row.revision.defaults;
        add(
            r.expense_account_id,
            row.accumulated - row.asset.opening_accumulated,
        );
        if row.disposed {
            if row.disposal_gain.is_positive() {
                add(r.gain_account_id, Money::ZERO - row.disposal_gain);
            } else {
                add(r.loss_account_id, Money::ZERO - row.disposal_gain);
            }
        } else {
            add(r.cost_account_id, row.asset.cost);
            add(r.accumulated_account_id, Money::ZERO - row.accumulated);
        }
    }
    let mut stocks = BTreeSet::new();
    let mut activities = BTreeSet::new();
    for r in revisions {
        let r = &r.defaults;
        stocks.extend([r.cost_account_id, r.accumulated_account_id]);
        activities.extend([r.expense_account_id, r.gain_account_id, r.loss_account_id]);
    }
    stocks
        .iter()
        .map(|id| (*id, ReconciliationKind::Balance))
        .chain(
            activities
                .iter()
                .map(|id| (*id, ReconciliationKind::Activity)),
        )
        .map(|(id, kind)| {
            // A balance counts every posting: an acquisition debit comes from
            // a bill or a journal, never from this module. Activity counts only
            // this module's postings, so a gain or loss account shared with
            // sales or cost of sales does not show their activity as a difference.
            let (actual, other) = if kind == ReconciliationKind::Balance {
                let actual = totals
                    .iter()
                    .find(|r| r.account.id == id)
                    .map_or(Money::ZERO, |r| r.debits - r.credits);
                (actual, Money::ZERO)
            } else {
                flows.iter().filter(|r| r.account.id == id).fold(
                    (Money::ZERO, Money::ZERO),
                    |(own, other), r| {
                        if r.owner_module.as_deref() == Some(MODULE) {
                            (own + (r.debits - r.credits), other)
                        } else {
                            (own, other + (r.debits - r.credits))
                        }
                    },
                )
            };
            let registered = expected.get(&id).copied().unwrap_or(Money::ZERO);
            Reconciliation {
                account_id: id,
                kind,
                ledger_balance: actual,
                register_balance: registered,
                difference: actual - registered,
                other_postings: other,
            }
        })
        .collect()
}
