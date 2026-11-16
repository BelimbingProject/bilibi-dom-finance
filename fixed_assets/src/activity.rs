//! Bilimbi.Finance.FixedAssets runs/corrections. Decisions use the caller's
//! company-locked transaction; records and their owned postings commit together.
use crate::*;
use bilibi_finance_ledger::{
    AccountType, Entry, EntryOwner, MinorUnits, NewEntry, NewLine, PeriodStatus, PostOptions,
    Reversal,
};
use serde_json::{json, Value};
use store::{int, string, Table};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventInput {
    #[serde(with = "date_serde")]
    pub effective_date: Date,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisposalInput {
    pub event: EventInput,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub proceeds: Money,
    pub settlement_account_id: Option<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    RunReversed,
    Disposed,
    DisposalReversed,
    AcquisitionVoided,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub event_number: i64,
    pub asset_id: Option<i64>,
    pub run_id: Option<i64>,
    pub action: EventAction,
    #[serde(with = "date_serde")]
    pub effective_date: Date,
    pub entry_id: Option<i64>,
    pub entry_number: Option<i64>,
    pub reverses_event_id: Option<i64>,
    #[serde(default, deserialize_with = "money_serde::optional")]
    pub proceeds: Option<Money>,
    #[serde(default, deserialize_with = "money_serde::optional")]
    pub accumulated: Option<Money>,
    pub reason: String,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunItem {
    pub id: i64,
    pub run_id: i64,
    pub asset_id: i64,
    pub asset_number: i64,
    pub description: String,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub amount: Money,
}
#[derive(Clone, Debug, Serialize)]
pub struct RunException {
    pub id: i64,
    pub asset_id: i64,
    pub asset_number: i64,
    pub description: String,
    pub reason: &'static str,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    #[serde(with = "date_serde")]
    pub period: Date,
    pub entry_id: Option<i64>,
    pub entry_number: Option<i64>,
    #[serde(deserialize_with = "money_serde::deserialize")]
    pub total: Money,
    pub reversed: bool,
    pub items: Vec<RunItem>,
    pub exceptions: Vec<RunException>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}
pub(crate) fn date(v: &Value, field: &str) -> Result<Date> {
    string(v, field)?.parse().map_err(|_| Error::Invalid.into())
}
fn amount(v: &Value, field: &str, units: MinorUnits) -> Result<Money> {
    Money::new(store::decimal(v, field)?, units).map_err(|_| Error::Invalid.into())
}
fn event(v: &Value, units: MinorUnits) -> Result<Event> {
    Ok(Event {
        id: int(v, "id")?,
        event_number: int(v, "event_number")?,
        asset_id: v["asset_id"].as_i64(),
        run_id: v["run_id"].as_i64(),
        action: match v["action"].as_str() {
            Some("run_reversed") => EventAction::RunReversed,
            Some("disposed") => EventAction::Disposed,
            Some("disposal_reversed") => EventAction::DisposalReversed,
            Some("acquisition_voided") => EventAction::AcquisitionVoided,
            _ => return Err(Error::Invalid.into()),
        },
        effective_date: date(v, "effective_date")?,
        entry_id: v["entry_id"].as_i64(),
        entry_number: v["entry_number"].as_i64(),
        reverses_event_id: v["reverses_event_id"].as_i64(),
        proceeds: (!v["proceeds"].is_null())
            .then(|| amount(v, "proceeds", units))
            .transpose()?,
        accumulated: (!v["accumulated"].is_null())
            .then(|| amount(v, "accumulated", units))
            .transpose()?,
        reason: string(v, "reason")?,
        recorded_by_user_id: int(v, "recorded_by_user_id")?,
        recorded_by_impersonator_id: v["recorded_by_impersonator_id"].as_i64(),
        recorded_at: string(v, "recorded_at")?,
    })
}
pub(crate) struct Activity {
    pub events: Vec<Event>,
    pub runs: Vec<Run>,
}
impl Activity {
    pub async fn load(
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        units: MinorUnits,
    ) -> Result<Self> {
        let events = store::all(c, Table::Event, s.tenant(), company)
            .await?
            .iter()
            .map(|v| event(v, units))
            .collect::<Result<Vec<_>>>()?;
        let assets = store::all(c, Table::Asset, s.tenant(), company).await?;
        let items = store::all(c, Table::Item, s.tenant(), company).await?;
        let exceptions = store::all(c, Table::Exception, s.tenant(), company).await?;
        let mut runs = Vec::new();
        for v in store::all(c, Table::Run, s.tenant(), company).await? {
            let id = int(&v, "id")?;
            let mut run_items = Vec::new();
            let mut run_exceptions = Vec::new();
            for i in items.iter().filter(|i| i["run_id"].as_i64() == Some(id)) {
                let asset_id = int(i, "asset_id")?;
                let a = assets
                    .iter()
                    .find(|a| a["id"].as_i64() == Some(asset_id))
                    .ok_or(Error::Invalid)?;
                run_items.push(RunItem {
                    id: int(i, "id")?,
                    run_id: id,
                    asset_id,
                    asset_number: int(a, "asset_number")?,
                    description: string(a, "description")?,
                    amount: amount(i, "amount", units)?,
                });
            }
            for i in exceptions
                .iter()
                .filter(|i| i["run_id"].as_i64() == Some(id))
            {
                let asset_id = int(i, "asset_id")?;
                let a = assets
                    .iter()
                    .find(|a| a["id"].as_i64() == Some(asset_id))
                    .ok_or(Error::Invalid)?;
                run_exceptions.push(RunException {
                    id: int(i, "id")?,
                    asset_id,
                    asset_number: int(a, "asset_number")?,
                    description: string(a, "description")?,
                    reason: "acquisition_reversed",
                });
            }
            run_items.sort_by_key(|i| i.asset_number);
            run_exceptions.sort_by_key(|i| i.asset_number);
            runs.push(Run {
                id,
                period: date(&v, "period")?,
                entry_id: v["entry_id"].as_i64(),
                entry_number: v["entry_number"].as_i64(),
                total: amount(&v, "total", units)?,
                reversed: events
                    .iter()
                    .any(|e| e.run_id == Some(id) && e.action == EventAction::RunReversed),
                items: run_items,
                exceptions: run_exceptions,
                recorded_by_user_id: int(&v, "recorded_by_user_id")?,
                recorded_by_impersonator_id: v["recorded_by_impersonator_id"].as_i64(),
                recorded_at: string(&v, "recorded_at")?,
            });
        }
        runs.sort_by_key(|r| r.period);
        Ok(Self { events, runs })
    }
    pub fn voided(&self, id: i64) -> bool {
        self.events
            .iter()
            .any(|e| e.asset_id == Some(id) && e.action == EventAction::AcquisitionVoided)
    }
    pub fn disposal(&self, id: i64) -> Option<&Event> {
        self.events
            .iter()
            .filter(|e| e.asset_id == Some(id))
            .max_by_key(|e| e.event_number)
            .filter(|e| e.action == EventAction::Disposed)
    }
    fn chronological(&self, date: Date) -> Result<()> {
        if self.events.iter().any(|e| e.effective_date > date)
            || self.runs.iter().any(|r| r.period.last_of_month() > date)
        {
            Err(FixedAssetsError::LaterActivity)
        } else {
            Ok(())
        }
    }
}

impl FixedAssets {
    pub async fn list_runs(&self, s: &Scope, company: i64) -> Result<Vec<Run>> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let tx = c
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        let runs = self.runs_on(&*tx, s, company).await?;
        tx.commit().await?;
        Ok(runs)
    }
    pub(super) async fn runs_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
    ) -> Result<Vec<Run>> {
        self.opening_authorize(c, s, company, VIEW).await?;
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let mut runs = Activity::load(c, s, company, units).await?.runs;
        runs.reverse();
        Ok(runs)
    }
    pub async fn list_events(&self, s: &Scope, company: i64, asset_id: i64) -> Result<Vec<Event>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        if store::raw(&**c, Table::Asset, s.tenant(), company, asset_id)
            .await?
            .is_none()
        {
            return Err(FixedAssetsError::NotFound);
        }
        let units = self.ledger.get_book_on(&**c, s, company).await?.minor_units;
        let mut events = store::all(&**c, Table::Event, s.tenant(), company)
            .await?
            .iter()
            .map(|v| event(v, units))
            .collect::<Result<Vec<_>>>()?;
        events.retain(|e| e.asset_id == Some(asset_id));
        events.sort_by_key(|e| e.event_number);
        Ok(events)
    }
    /// A successful month is consumed forever, even after reversal.
    pub async fn depreciate(&self, s: &Scope, company: i64, period: Date) -> Result<Run> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                DEPRECIATE,
                "finance.fixed-assets.depreciate",
            )
            .await?;
        let run = self.depreciate_on(&mut tx, s, company, period).await?;
        tx.commit().await?;
        Ok(run)
    }
    pub(super) async fn prepare_run(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        period: Date,
    ) -> Result<PreparedRun> {
        self.opening_authorize(c, s, company, DEPRECIATE).await?;
        if period.day() != 1 {
            return Err(FixedAssetsError::InvalidPeriod);
        }
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let activity = Activity::load(c, s, company, units).await?;
        if let Some(run) = activity.runs.iter().find(|r| r.period == period) {
            return Ok(PreparedRun {
                units,
                existing: Some(run.clone()),
                charges: vec![],
                skipped: vec![],
                review: json!({"existing":run}),
            });
        }
        self.open_period(c, s, company, period).await?;
        let end = period.last_of_month();
        activity.chronological(end)?;
        let report = self
            .register_report_on(c, s, company, end, &ReportPins::default())
            .await?;
        let mut charges = Vec::new();
        let mut skipped = Vec::new();
        for row in &report.rows {
            if row.disposed || row.voided || row.scheduled_depreciation <= row.accumulated {
                continue;
            }
            let entry = self
                .ledger
                .get_entry_on(c, s, company, row.asset.entry_id)
                .await?;
            if entry.reversed_by_entry_id.is_some() || entry.reverses_entry_id.is_some() {
                skipped.push(row.asset.id);
            } else {
                charges.push((
                    row.asset.id,
                    row.revision.clone(),
                    row.scheduled_depreciation - row.accumulated,
                ));
            }
        }

        let lines = charge_lines(&charges, false);
        let posting = self
            .preview_post(
                c,
                s,
                company,
                end,
                format!("Asset depreciation {period}"),
                lines,
            )
            .await?;
        let review = json!({"period":period.to_string(),"register":report,"charges":charges,"skipped_asset_ids":skipped,"posting":posting});
        Ok(PreparedRun {
            units,
            existing: None,
            charges,
            skipped,
            review,
        })
    }
    pub(super) async fn depreciate_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        period: Date,
    ) -> Result<Run> {
        self.lifecycle_writable(tx, s, company, DEPRECIATE).await?;
        let p = self.prepare_run(tx.sql(), s, company, period).await?;
        if let Some(run) = p.existing {
            return Ok(run);
        }
        let PreparedRun {
            units,
            charges,
            skipped,
            ..
        } = p;
        let id = store::reserve(tx.sql(), Table::Run).await?;
        let posted = self
            .post(
                tx,
                s,
                company,
                id,
                "run",
                period.last_of_month(),
                format!("Asset depreciation {period}"),
                charge_lines(&charges, false),
            )
            .await?;
        let mut row = store::identity(s, company);
        row.as_object_mut().ok_or(Error::Invalid)?.extend(json!({"id":id,"period":period.to_string(),"total":charges.iter().map(|(_,_,m)|*m).sum::<Money>(),"entry_id":posted.as_ref().map(|e|e.id),"entry_number":posted.as_ref().map(|e|e.entry_number)}).as_object().ok_or(Error::Invalid)?.clone());
        let identity = store::identity(s, company);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (_, change) = store::insert(sql, Table::Run, row).await?;
                let mut changes = vec![change];
                for (asset, _, amount) in charges {
                    let mut row = identity.clone();
                    row["run_id"] = id.into();
                    row["asset_id"] = asset.into();
                    row["amount"] = json!(amount);
                    changes.push(store::insert(sql, Table::Item, row).await?.1);
                }
                for asset in skipped {
                    let mut row = identity.clone();
                    row["run_id"] = id.into();
                    row["asset_id"] = asset.into();
                    changes.push(store::insert(sql, Table::Exception, row).await?.1);
                }
                Ok(((), changes))
            })
        })
        .await?;
        Activity::load(tx.sql(), s, company, units)
            .await?
            .runs
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| Error::Invalid.into())
    }
    pub async fn reverse_run(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                REVERSE,
                "finance.fixed-assets.reverse-run",
            )
            .await?;
        let event = self.reverse_run_on(&mut tx, s, company, id, input).await?;
        tx.commit().await?;
        Ok(event)
    }
    pub(super) async fn reverse_run_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        self.lifecycle_writable(tx, s, company, REVERSE).await?;
        let p = self
            .prepare_reverse_run(tx.sql(), s, company, id, input)
            .await?;
        self.apply_event(tx, s, company, p).await
    }
    pub(super) async fn prepare_reverse_run(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<PreparedEvent> {
        self.opening_authorize(c, s, company, REVERSE).await?;
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let activity = Activity::load(c, s, company, units).await?;
        let run = activity
            .runs
            .iter()
            .find(|r| r.id == id)
            .ok_or(FixedAssetsError::NotFound)?;
        let entry_id = run.entry_id.ok_or(FixedAssetsError::EmptyRun)?;
        if run.reversed {
            return Err(FixedAssetsError::AlreadyReversed);
        }
        let live = run
            .items
            .iter()
            .filter(|i| !activity.voided(i.asset_id))
            .collect::<Vec<_>>();
        if live.is_empty() {
            return Err(FixedAssetsError::EmptyRun);
        }
        let reason = text(&input.reason, "reason", 500)?;
        if activity.runs.iter().any(|r| {
            r.period > run.period
                && !r.reversed
                && r.items.iter().any(|i| !activity.voided(i.asset_id))
        }) || activity
            .events
            .iter()
            .any(|e| e.asset_id.is_some_and(|id| activity.disposal(id).is_some()))
        {
            return Err(FixedAssetsError::DependentPostings);
        }
        if input.effective_date < run.period.last_of_month() {
            return Err(FixedAssetsError::BeforeOriginal);
        }
        activity.chronological(input.effective_date)?;

        let mut charges = Vec::new();
        for i in &live {
            let a = store::raw(c, Table::Asset, s.tenant(), company, i.asset_id)
                .await?
                .ok_or(Error::Invalid)?;
            charges.push((
                i.asset_id,
                store::revision(c, s.tenant(), company, int(&a, "category_revision_id")?).await?,
                i.amount,
            ));
        }
        let reversal = if live.len() == run.items.len() {
            Some((entry_id, owner("run", id)))
        } else {
            None
        };
        let lines = if reversal.is_none() {
            charge_lines(&charges, true)
        } else {
            vec![]
        };
        let values = json!({"run_id":id,"action":"run_reversed","effective_date":input.effective_date.to_string(),"reason":reason});
        self.prepare_event(
            c,
            s,
            company,
            units,
            values,
            lines,
            reversal,
            json!({"run":run,"charges":charges}),
        )
        .await
    }
    pub async fn dispose(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &DisposalInput,
    ) -> Result<Event> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(&mut c, s, company, DISPOSE, "finance.fixed-assets.dispose")
            .await?;
        let event = self.dispose_on(&mut tx, s, company, id, input).await?;
        tx.commit().await?;
        Ok(event)
    }
    pub(super) async fn dispose_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        input: &DisposalInput,
    ) -> Result<Event> {
        self.lifecycle_writable(tx, s, company, DISPOSE).await?;
        let p = self
            .prepare_dispose(tx.sql(), s, company, id, input)
            .await?;
        self.apply_event(tx, s, company, p).await
    }
    pub(super) async fn prepare_dispose(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
        input: &DisposalInput,
    ) -> Result<PreparedEvent> {
        self.opening_authorize(c, s, company, DISPOSE).await?;
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let a = store::raw(c, Table::Asset, s.tenant(), company, id)
            .await?
            .ok_or(FixedAssetsError::NotFound)?;
        let a = store::asset(&a, units)?;
        if c.query_opt("SELECT i.id FROM finance_fixed_assets_opening_items i JOIN finance_fixed_assets_opening_decisions d ON d.replaces_batch_id=i.batch_id AND d.tenant_id=$1 AND d.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.asset_id=$3",&[&s.tenant(),&company,&id]).await?.is_some() {return Err(invalid("asset_id","belongs to a replaced opening register; use the replacement asset"));}
        let activity = Activity::load(c, s, company, units).await?;
        if activity.voided(id) {
            return Err(FixedAssetsError::AssetVoided);
        }
        let reason = text(&input.event.reason, "reason", 500)?;
        let proceeds = Money::new(input.proceeds.amount(), units)
            .map_err(|e| invalid("proceeds", e.to_string()))?;
        if proceeds < Money::ZERO {
            return Err(invalid("proceeds", "must be greater than or equal to 0"));
        }
        let date = input.event.effective_date;
        if a.opening_date.is_some_and(|opening| date < opening) {
            return Err(invalid(
                "effective_date",
                "cannot precede the conversion date",
            ));
        }
        if date < a.acquisition_date {
            return Err(FixedAssetsError::BeforeAcquisition);
        }
        activity.chronological(date)?;
        if !self.acquisition_live(c, s, company, a.entry_id).await? {
            return Err(FixedAssetsError::AcquisitionReversed);
        }
        if activity.disposal(id).is_some() {
            return Err(FixedAssetsError::AlreadyDisposed);
        }
        let report = self
            .register_report_on(c, s, company, date, &ReportPins::default())
            .await?;
        let row = report
            .rows
            .iter()
            .find(|r| r.asset.id == id)
            .ok_or(Error::Invalid)?;
        if row.scheduled_depreciation != row.accumulated {
            return Err(FixedAssetsError::DepreciationRequired);
        }
        let settlement = if proceeds.is_positive() {
            let account = input
                .settlement_account_id
                .ok_or_else(|| invalid("settlement_account_id", "can't be blank"))?;
            if !self
                .ledger
                .list_accounts_on(c, s, company)
                .await?
                .iter()
                .any(|a| a.id == account && a.postable() && a.account_type == AccountType::Asset)
            {
                return Err(FixedAssetsError::InvalidSettlementAccount);
            }
            Some(account)
        } else {
            None
        };
        let revision = &row.revision.defaults;
        let gain = proceeds - (a.cost - row.accumulated);
        let mut lines = vec![
            NewLine::credit(revision.cost_account_id, a.cost),
            NewLine::debit(revision.accumulated_account_id, row.accumulated),
        ];
        if let Some(account) = settlement {
            lines.push(NewLine::debit(account, proceeds));
        }
        if gain.is_positive() {
            lines.push(NewLine::credit(revision.gain_account_id, gain));
        } else {
            lines.push(NewLine::debit(revision.loss_account_id, Money::ZERO - gain));
        }
        lines.retain(|l| l.debit.is_positive() || l.credit.is_positive());

        let values = json!({"asset_id":id,"action":"disposed","effective_date":date.to_string(),"reason":reason,"proceeds":proceeds,"accumulated":row.accumulated});
        self.prepare_event(
            c,
            s,
            company,
            units,
            values,
            lines,
            None,
            json!({"asset":row,"settlement_account_id":settlement}),
        )
        .await
    }
    pub async fn reverse_disposal(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                REVERSE,
                "finance.fixed-assets.reverse-disposal",
            )
            .await?;
        let event = self
            .reverse_disposal_on(&mut tx, s, company, id, input)
            .await?;
        tx.commit().await?;
        Ok(event)
    }
    pub(super) async fn reverse_disposal_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        self.lifecycle_writable(tx, s, company, REVERSE).await?;
        let p = self
            .prepare_reverse_disposal(tx.sql(), s, company, id, input)
            .await?;
        self.apply_event(tx, s, company, p).await
    }
    pub(super) async fn prepare_reverse_disposal(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<PreparedEvent> {
        self.opening_authorize(c, s, company, REVERSE).await?;
        if store::raw(c, Table::Asset, s.tenant(), company, id)
            .await?
            .is_none()
        {
            return Err(FixedAssetsError::NotFound);
        }
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let activity = Activity::load(c, s, company, units).await?;
        let disposed = activity.disposal(id).ok_or(FixedAssetsError::NotDisposed)?;
        let reason = text(&input.reason, "reason", 500)?;
        activity.chronological(input.effective_date)?;

        let values = json!({"asset_id":id,"action":"disposal_reversed","effective_date":input.effective_date.to_string(),"reason":reason,"reverses_event_id":disposed.id});
        self.prepare_event(
            c,
            s,
            company,
            units,
            values,
            vec![],
            Some((
                disposed.entry_id.ok_or(Error::Invalid)?,
                owner("event", disposed.id),
            )),
            json!({"disposal":disposed}),
        )
        .await
    }
    pub async fn void_acquisition(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                VOID,
                "finance.fixed-assets.void-acquisition",
            )
            .await?;
        let event = self
            .void_acquisition_on(&mut tx, s, company, id, input)
            .await?;
        tx.commit().await?;
        Ok(event)
    }
    pub(super) async fn void_acquisition_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<Event> {
        self.lifecycle_writable(tx, s, company, VOID).await?;
        let p = self
            .prepare_void_acquisition(tx.sql(), s, company, id, input)
            .await?;
        self.apply_event(tx, s, company, p).await
    }
    pub(super) async fn prepare_void_acquisition(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
        input: &EventInput,
    ) -> Result<PreparedEvent> {
        self.opening_authorize(c, s, company, VOID).await?;
        let units = self.ledger.get_book_on(c, s, company).await?.minor_units;
        let a = store::raw(c, Table::Asset, s.tenant(), company, id)
            .await?
            .ok_or(FixedAssetsError::NotFound)?;
        let a = store::asset(&a, units)?;
        let activity = Activity::load(c, s, company, units).await?;
        if activity.voided(id) {
            return Err(FixedAssetsError::AlreadyVoided);
        }
        if activity.disposal(id).is_some() {
            return Err(FixedAssetsError::DisposalStanding);
        }
        if self.acquisition_live(c, s, company, a.entry_id).await? {
            return Err(FixedAssetsError::AcquisitionNotReversed);
        }
        let reason = text(&input.reason, "reason", 500)?;
        self.open_period(c, s, company, input.effective_date.first_of_month())
            .await?;
        activity.chronological(input.effective_date)?;
        let revision = store::revision(c, s.tenant(), company, a.category_revision_id).await?;
        let charges: Vec<_> = activity
            .runs
            .iter()
            .filter(|r| !r.reversed)
            .flat_map(|r| r.items.iter())
            .filter(|i| i.asset_id == id)
            .map(|i| (id, revision.clone(), i.amount))
            .collect();
        let accumulated = charges.iter().map(|(_, _, amount)| *amount).sum::<Money>();
        let lines = charge_lines(&charges, true);

        let values = json!({"asset_id":id,"action":"acquisition_voided","effective_date":input.effective_date.to_string(),"reason":reason,"accumulated":accumulated});
        self.prepare_event(
            c,
            s,
            company,
            units,
            values,
            lines,
            None,
            json!({"asset":a,"revision":revision,"charges":charges}),
        )
        .await
    }
    async fn preview_post(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        date: Date,
        description: String,
        lines: Vec<NewLine>,
    ) -> Result<Value> {
        if lines.is_empty() {
            return Ok(Value::Null);
        }
        Ok(self
            .ledger
            .preview_post_entry_on(
                c,
                s,
                company,
                &NewEntry {
                    entry_date: date,
                    description,
                    reference: None,
                    lines,
                },
                &PostOptions::default(),
            )
            .await?)
    }
    async fn prepare_event(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        units: MinorUnits,
        values: Value,
        lines: Vec<NewLine>,
        reversal: Option<(i64, EntryOwner)>,
        basis: Value,
    ) -> Result<PreparedEvent> {
        let date = date(&values, "effective_date")?;
        let reason = string(&values, "reason")?;
        let posting = if let Some((entry, owner)) = &reversal {
            self.ledger
                .preview_reverse_owned_entry_on(
                    c,
                    s,
                    company,
                    *entry,
                    owner,
                    &Reversal {
                        entry_date: date,
                        description: reason,
                        reference: None,
                    },
                )
                .await?
        } else {
            self.preview_post(c, s, company, date, reason, lines.clone())
                .await?
        };
        let review = json!({"event":values,"basis":basis,"posting":posting});
        Ok(PreparedEvent {
            units,
            values,
            lines,
            reversal,
            review,
        })
    }
    async fn apply_event(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        mut p: PreparedEvent,
    ) -> Result<Event> {
        let id = store::reserve(tx.sql(), Table::Event).await?;
        let date = date(&p.values, "effective_date")?;
        let reason = string(&p.values, "reason")?;
        let posted = if let Some((entry, owner)) = p.reversal {
            Some(
                self.ledger
                    .reverse_owned_entry(
                        tx,
                        s,
                        company,
                        entry,
                        &owner,
                        &Reversal {
                            entry_date: date,
                            description: reason,
                            reference: None,
                        },
                    )
                    .await?,
            )
        } else {
            self.post(tx, s, company, id, "event", date, reason, p.lines)
                .await?
        };
        p.values["id"] = id.into();
        self.record_event(tx, s, company, p.units, p.values, posted.as_ref())
            .await
    }
    async fn open_period(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        month: Date,
    ) -> Result<()> {
        let years = self.ledger.list_fiscal_years_on(c, s, company).await?;
        let p = years
            .iter()
            .flat_map(|y| &y.periods)
            .find(|p| p.start_date == month)
            .ok_or(FixedAssetsError::PeriodNotFound)?;
        if p.status != PeriodStatus::Open {
            return Err(FixedAssetsError::PeriodClosed);
        }
        Ok(())
    }
    async fn acquisition_live(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<bool> {
        let entry = self.ledger.get_entry_on(c, s, company, id).await?;
        Ok(entry.reversed_by_entry_id.is_none() && entry.reverses_entry_id.is_none())
    }
    async fn post(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        kind: &str,
        date: Date,
        description: String,
        lines: Vec<NewLine>,
    ) -> Result<Option<Entry>> {
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            self.ledger
                .post_entry_in(
                    tx,
                    s,
                    company,
                    &NewEntry {
                        entry_date: date,
                        description,
                        reference: None,
                        lines,
                    },
                    &PostOptions {
                        owner: Some(owner(kind, id)),
                        ..Default::default()
                    },
                )
                .await?,
        ))
    }
    async fn record_event(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        units: MinorUnits,
        values: Value,
        entry: Option<&Entry>,
    ) -> Result<Event> {
        let mut row = store::identity(s, company);
        row.as_object_mut()
            .ok_or(Error::Invalid)?
            .extend(values.as_object().ok_or(Error::Invalid)?.clone());
        row["entry_id"] = json!(entry.map(|e| e.id));
        row["entry_number"] = json!(entry.map(|e| e.entry_number));
        let row = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (v, change) = store::insert(sql, Table::Event, row).await?;
                    Ok((v, vec![change]))
                })
            })
            .await?;
        event(&row, units)
    }
}
fn owner(kind: &str, id: i64) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("{kind}:{id}"),
    }
}
fn charge_lines(charges: &[(i64, CategoryRevision, Money)], reverse: bool) -> Vec<NewLine> {
    charges
        .iter()
        .filter(|(_, _, m)| m.is_positive())
        .flat_map(|(_, r, m)| {
            let (debit, credit) = if reverse {
                (
                    r.defaults.accumulated_account_id,
                    r.defaults.expense_account_id,
                )
            } else {
                (
                    r.defaults.expense_account_id,
                    r.defaults.accumulated_account_id,
                )
            };
            [NewLine::debit(debit, *m), NewLine::credit(credit, *m)]
        })
        .collect()
}

pub(super) struct PreparedRun {
    units: MinorUnits,
    existing: Option<Run>,
    charges: Vec<(i64, CategoryRevision, Money)>,
    skipped: Vec<i64>,
    pub review: Value,
}
pub(super) struct PreparedEvent {
    units: MinorUnits,
    values: Value,
    lines: Vec<NewLine>,
    reversal: Option<(i64, EntryOwner)>,
    pub review: Value,
}

// Decode the closed reason vocabulary without changing the facade's static
// reason type or borrowing response text.
impl<'de> Deserialize<'de> for RunException {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            id: i64,
            asset_id: i64,
            asset_number: i64,
            description: String,
            reason: String,
        }
        let value = Wire::deserialize(deserializer)?;
        let reason = match value.reason.as_str() {
            "acquisition_reversed" => "acquisition_reversed",
            _ => {
                return Err(serde::de::Error::custom(
                    "unknown depreciation exception reason",
                ));
            }
        };
        Ok(Self {
            id: value.id,
            asset_id: value.asset_id,
            asset_number: value.asset_number,
            description: value.description,
            reason,
        })
    }
}
