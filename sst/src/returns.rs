//! Bilimbi.Finance.Sst.Returns: a company's SST returns. Opening one for a
//! taxable period, adjusting it while open, finalising it into a stored
//! snapshot, discarding it, and amending a finalised one with a new return
//! for the same period. Nothing is updated or deleted: a return's status is
//! its events', and an adjustment is withdrawn by an appended row.
use crate::figures::Period;
use crate::form::{self, BoxFigure};
use crate::registrations::{self, Registration};
use crate::store::{self, Table};
use crate::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// How many times finalising reads the figures again when a mapping or one
/// of the return's adjustments was recorded between the reading and the lock.
const FINALISE_ATTEMPTS: usize = 3;

/// `open`, `finalised` or `discarded`, from the return's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnStatus {
    Open,
    Finalised,
    Discarded,
}
impl ReturnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Finalised => "finalised",
            Self::Discarded => "discarded",
        }
    }
}

/// One event of a return: `opened`, `finalised` (with the numbers its
/// figures were read at, the currency and the tax payable) or `discarded`
/// (with its reason).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReturnEvent {
    pub id: i64,
    pub event_number: i64,
    pub action: String,
    pub reason: Option<String>,
    pub version: Option<i64>,
    pub through_entry_number: Option<i64>,
    pub through_mapping_number: Option<i64>,
    pub through_adjustment_number: Option<i64>,
    pub currency_code: Option<String>,
    pub minor_units: Option<u8>,
    pub tax_payable: Option<Decimal>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}

/// A return's own fields, `status`, `version` (once finalised: 1 for the
/// period's first, then each amendment's), the tax payable and currency it
/// was finalised with, the finalised return that amends it, and its events.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaxReturn {
    pub id: i64,
    pub return_number: i64,
    pub tax_type: TaxType,
    pub registration_id: i64,
    #[serde(serialize_with = "as_text")]
    pub period_start: Date,
    #[serde(serialize_with = "as_text")]
    pub period_end: Date,
    #[serde(serialize_with = "as_text")]
    pub due_date: Date,
    pub amends_return_id: Option<i64>,
    pub reason: Option<String>,
    pub opened_by_user_id: i64,
    pub opened_by_impersonator_id: Option<i64>,
    pub opened_at: String,
    pub status: ReturnStatus,
    pub version: Option<i64>,
    pub tax_payable: Option<Decimal>,
    pub currency_code: Option<String>,
    pub minor_units: Option<u8>,
    pub superseded_by_return_id: Option<i64>,
    pub events: Vec<ReturnEvent>,
}

fn as_text<S: serde::Serializer>(date: &Date, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.collect_str(date)
}

/// One adjustment row as recorded: `added` to a box (a value and a tax, a
/// column the box does not take zero), or `withdrawn` with its reason as the
/// description. An amendment's copy names the adjustment it copies.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Adjustment {
    pub id: i64,
    pub adjustment_number: i64,
    pub return_id: i64,
    pub action: String,
    pub withdraws_adjustment_id: Option<i64>,
    pub copied_from_adjustment_id: Option<i64>,
    #[serde(rename = "box")]
    pub box_key: Option<String>,
    pub taxable_amount: Option<Decimal>,
    pub tax_amount: Option<Decimal>,
    pub description: String,
    pub reference: Option<String>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}

/// An adjustment added to a return as its page lists it: `standing` until a
/// `withdrawal` names it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReturnAdjustment {
    pub id: i64,
    pub adjustment_number: i64,
    #[serde(rename = "box")]
    pub box_key: String,
    pub taxable_amount: Decimal,
    pub tax_amount: Decimal,
    pub description: String,
    pub reference: Option<String>,
    pub copied_from_adjustment_id: Option<i64>,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
    pub standing: bool,
    pub withdrawal: Option<Withdrawal>,
}

/// The row that withdrew an adjustment, its reason as `description`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Withdrawal {
    pub id: i64,
    pub adjustment_number: i64,
    pub description: String,
    pub recorded_by_user_id: i64,
    pub recorded_by_impersonator_id: Option<i64>,
    pub recorded_at: String,
}

/// What adds an adjustment: a `box_key` the form lets a preparer adjust,
/// `taxable_amount` and `tax_amount` as written (non-negative, in the book's
/// minor units; a column the box does not take left out or zero), a
/// `description` and an optional `reference`.
#[derive(Clone, Debug, Default)]
pub struct AdjustmentInput {
    pub box_key: String,
    pub taxable_amount: Option<String>,
    pub tax_amount: Option<String>,
    pub description: Option<String>,
    pub reference: Option<String>,
}

/// One box whose figures differ between two readings: the box now and the
/// change (`None` where the box takes no such column).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoxChange {
    pub key: &'static str,
    pub item: &'static str,
    pub label: &'static str,
    pub taxable_amount: Option<Decimal>,
    pub tax_amount: Option<Decimal>,
    pub taxable_change: Option<Decimal>,
    pub tax_change: Option<Decimal>,
}

/// What the documents dated in a finalised return's period show now, under
/// the same mappings and adjustments: each box that differs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PostedSince {
    pub through_entry_number: i64,
    pub boxes: Vec<BoxChange>,
}

/// The finalised return an amendment amends, and each box that changed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Amends {
    pub return_id: i64,
    pub return_number: i64,
    pub version: Option<i64>,
    pub tax_payable: Decimal,
    pub boxes: Vec<BoxChange>,
}

/// One return with its registration version, adjustments and figures: an
/// open one's read now, a finalised one's as stored, a discarded one's none.
/// A finalised return also shows `posted_since` (`None` when the actor may
/// not read the documents) and an amendment the return it `amends`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReturnReport {
    pub tax_return: TaxReturn,
    pub registration: Option<Registration>,
    pub registration_number: Option<String>,
    pub adjustments: Vec<ReturnAdjustment>,
    pub period_ended: bool,
    pub figures: Option<Figures>,
    pub posted_since: Option<PostedSince>,
    pub amends: Option<Amends>,
}

/// A finalised return's figures read again from the live records at the
/// numbers it recorded, and whether every box, source row and account
/// figure equals its snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Reproduction {
    pub figures: Figures,
    pub reproduced: bool,
}

/// A return with its events and adjustment rows, as stored.
pub(crate) struct Loaded {
    pub tax_return: TaxReturn,
    pub adjustments: Vec<Adjustment>,
}
impl Loaded {
    fn status(&self) -> ReturnStatus {
        self.tax_return.status
    }
    fn finalised(&self) -> Option<&ReturnEvent> {
        self.tax_return
            .events
            .iter()
            .find(|e| e.action == "finalised")
    }
    fn period(&self) -> Period {
        Period {
            tax_type: self.tax_return.tax_type,
            period_start: self.tax_return.period_start,
            period_end: self.tax_return.period_end,
        }
    }
    /// Adjustments added and not withdrawn, both numbered through `through`.
    pub fn standing(&self, through: i64) -> Vec<&Adjustment> {
        let withdrawn: HashSet<i64> = self
            .adjustments
            .iter()
            .filter(|a| a.action == "withdrawn" && a.adjustment_number <= through)
            .filter_map(|a| a.withdraws_adjustment_id)
            .collect();
        self.adjustments
            .iter()
            .filter(|a| {
                a.action == "added" && a.adjustment_number <= through && !withdrawn.contains(&a.id)
            })
            .collect()
    }
    /// `Ok` while open: a finalised or discarded return is refused.
    fn open(&self) -> Result<()> {
        match self.status() {
            ReturnStatus::Open => Ok(()),
            ReturnStatus::Finalised => Err(SstError::ReturnFinalised),
            ReturnStatus::Discarded => Err(SstError::ReturnDiscarded),
        }
    }
    fn period_ended(&self) -> bool {
        today() > self.tax_return.period_end
    }
}

const RETURN_COLUMNS: &str = "r.id,r.return_number,r.tax_type,r.registration_id,r.period_start::text,r.period_end::text,r.due_date::text,r.amends_return_id,r.reason,r.opened_by_user_id,r.opened_by_impersonator_id";
const EVENT_COLUMNS: &str = "id,event_number,return_id,action,reason,version::bigint,through_entry_number,through_mapping_number,through_adjustment_number,currency_code,minor_units::integer,tax_payable::text,recorded_by_user_id,recorded_by_impersonator_id";
const ADJUSTMENT_COLUMNS: &str = "id,adjustment_number,return_id,action,withdraws_adjustment_id,copied_from_adjustment_id,box,taxable_amount::text,tax_amount::text,description,reference,recorded_by_user_id,recorded_by_impersonator_id";

fn amount(text: Option<&str>) -> bilibi_database::Result<Option<Decimal>> {
    text.map(store::decimal).transpose()
}

fn event_row(r: &tokio_postgres::Row) -> bilibi_database::Result<(i64, ReturnEvent)> {
    Ok((
        r.get(2),
        ReturnEvent {
            id: r.get(0),
            event_number: r.get(1),
            action: r.get(3),
            reason: r.get(4),
            version: r.get(5),
            through_entry_number: r.get(6),
            through_mapping_number: r.get(7),
            through_adjustment_number: r.get(8),
            currency_code: r.get(9),
            minor_units: r
                .get::<_, Option<i32>>(10)
                .map(u8::try_from)
                .transpose()
                .map_err(|_| Error::Invalid)?,
            tax_payable: amount(r.get(11))?,
            recorded_by_user_id: r.get(12),
            recorded_by_impersonator_id: r.get(13),
            recorded_at: r.get(14),
        },
    ))
}

fn adjustment_row(r: &tokio_postgres::Row) -> bilibi_database::Result<Adjustment> {
    Ok(Adjustment {
        id: r.get(0),
        adjustment_number: r.get(1),
        return_id: r.get(2),
        action: r.get(3),
        withdraws_adjustment_id: r.get(4),
        copied_from_adjustment_id: r.get(5),
        box_key: r.get(6),
        taxable_amount: amount(r.get(7))?,
        tax_amount: amount(r.get(8))?,
        description: r.get(9),
        reference: r.get(10),
        recorded_by_user_id: r.get(11),
        recorded_by_impersonator_id: r.get(12),
        recorded_at: r.get(13),
    })
}

/// The company's returns matching `filter` (on `r`), each with its events,
/// ordered by id.
async fn read_returns(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    filter: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> bilibi_database::Result<Vec<TaxReturn>> {
    let sql = format!(
        "SELECT {RETURN_COLUMNS},{} FROM finance_sst_returns r WHERE r.tenant_id=$1 AND r.company_id=$2 {filter}",
        store::stamp("r.opened_at")
    );
    let mut all: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&tenant, &company];
    all.extend_from_slice(params);
    let rows = c.query(&sql, &all).await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
    let mut events: HashMap<i64, Vec<ReturnEvent>> = HashMap::new();
    let sql = format!(
        "SELECT {EVENT_COLUMNS},{} FROM finance_sst_return_events WHERE tenant_id=$1 AND company_id=$2 AND return_id=ANY($3) ORDER BY event_number",
        store::stamp("recorded_at")
    );
    for r in c.query(&sql, &[&tenant, &company, &ids]).await? {
        let (return_id, event) = event_row(&r)?;
        events.entry(return_id).or_default().push(event);
    }
    let superseded = superseded(c, tenant, company).await?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.get(0);
            let events = events.remove(&id).unwrap_or_default();
            let finalised = events.iter().find(|e| e.action == "finalised").cloned();
            let status = if finalised.is_some() {
                ReturnStatus::Finalised
            } else if events.iter().any(|e| e.action == "discarded") {
                ReturnStatus::Discarded
            } else {
                ReturnStatus::Open
            };
            Ok(TaxReturn {
                id,
                return_number: r.get(1),
                tax_type: r.get::<_, &str>(2).parse().map_err(|_| Error::Invalid)?,
                registration_id: r.get(3),
                period_start: store::date(r.get(4))?,
                period_end: store::date(r.get(5))?,
                due_date: store::date(r.get(6))?,
                amends_return_id: r.get(7),
                reason: r.get(8),
                opened_by_user_id: r.get(9),
                opened_by_impersonator_id: r.get(10),
                opened_at: r.get(11),
                status,
                version: finalised.as_ref().and_then(|e| e.version),
                tax_payable: finalised.as_ref().and_then(|e| e.tax_payable),
                currency_code: finalised.as_ref().and_then(|e| e.currency_code.clone()),
                minor_units: finalised.as_ref().and_then(|e| e.minor_units),
                superseded_by_return_id: superseded.get(&id).copied(),
                events,
            })
        })
        .collect()
}

/// `{amended return id => the finalised return that amends it}`.
async fn superseded(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
) -> bilibi_database::Result<HashMap<i64, i64>> {
    Ok(c
        .query(
            "SELECT r.amends_return_id,r.id FROM finance_sst_returns r JOIN finance_sst_return_events e ON e.return_id=r.id AND e.company_id=r.company_id AND e.action='finalised' WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.amends_return_id IS NOT NULL",
            &[&tenant, &company],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect())
}

/// One return with its events and adjustment rows, or `None`.
pub(crate) async fn fetch(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    id: i64,
) -> bilibi_database::Result<Option<Loaded>> {
    let Some(tax_return) = read_returns(c, tenant, company, "AND r.id=$3", &[&id])
        .await?
        .pop()
    else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT {ADJUSTMENT_COLUMNS},{} FROM finance_sst_return_adjustments WHERE tenant_id=$1 AND company_id=$2 AND return_id=$3 ORDER BY adjustment_number",
        store::stamp("recorded_at")
    );
    let adjustments = c
        .query(&sql, &[&tenant, &company, &id])
        .await?
        .iter()
        .map(adjustment_row)
        .collect::<bilibi_database::Result<_>>()?;
    Ok(Some(Loaded {
        tax_return,
        adjustments,
    }))
}

async fn fetch_adjustment(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    id: i64,
) -> bilibi_database::Result<Option<Adjustment>> {
    let sql = format!(
        "SELECT {ADJUSTMENT_COLUMNS},{} FROM finance_sst_return_adjustments WHERE tenant_id=$1 AND company_id=$2 AND id=$3",
        store::stamp("recorded_at")
    );
    c.query_opt(&sql, &[&tenant, &company, &id])
        .await?
        .as_ref()
        .map(adjustment_row)
        .transpose()
}

/// The return with `id`, or `ReturnNotFound`.
async fn load(c: &(impl GenericClient + Sync), s: &Scope, company: i64, id: i64) -> Result<Loaded> {
    if id <= 0 {
        return Err(SstError::ReturnNotFound);
    }
    fetch(c, s.tenant(), company, id)
        .await?
        .ok_or(SstError::ReturnNotFound)
}

/// A finalised return's stored figures, in the form's order.
async fn snapshot(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    loaded: &Loaded,
    registration: Option<&Registration>,
) -> Result<Figures> {
    let event = loaded.finalised().ok_or(SstError::ReturnNotFinalised)?;
    let id = loaded.tax_return.id;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&tenant, &company, &id];
    let stored: HashMap<String, (Option<Decimal>, Option<Decimal>)> = c
        .query("SELECT box,taxable_amount::text,tax_amount::text FROM finance_sst_return_boxes WHERE tenant_id=$1 AND company_id=$2 AND return_id=$3", &params)
        .await?
        .iter()
        .map(|r| Ok((r.get(0), (amount(r.get(1))?, amount(r.get(2))?))))
        .collect::<bilibi_database::Result<_>>()?;
    let sources = c
        .query("SELECT line_number,source,document_kind,measure,prior_period,tax_code_id,tax_code,rate::text,account_id,box,mapping_number,document_count,taxable_amount::text,tax_amount::text FROM finance_sst_return_sources WHERE tenant_id=$1 AND company_id=$2 AND return_id=$3 ORDER BY line_number", &params)
        .await?
        .iter()
        .map(|r| {
            Ok(SourceRow {
                line_number: r.get(0),
                source: r.get::<_, &str>(1).parse().map_err(|_| Error::Invalid)?,
                document_kind: r.get(2),
                measure: r.get(3),
                prior_period: r.get(4),
                tax_code_id: r.get(5),
                tax_code: r.get(6),
                rate: store::decimal(r.get(7))?,
                account_id: r.get(8),
                box_key: r.get(9),
                mapping_number: r.get(10),
                document_count: i64::from(r.get::<_, i32>(11)),
                taxable_amount: store::decimal(r.get(12))?,
                tax_amount: store::decimal(r.get(13))?,
                rate_mismatch: false,
            })
        })
        .collect::<bilibi_database::Result<_>>()?;
    let accounts = c
        .query("SELECT line_number,account_id,account_code,account_name,ledger_debits::text,ledger_credits::text,document_debits::text,document_credits::text,difference::text FROM finance_sst_return_accounts WHERE tenant_id=$1 AND company_id=$2 AND return_id=$3 ORDER BY line_number", &params)
        .await?
        .iter()
        .map(|r| {
            Ok(AccountReconciliation {
                line_number: r.get(0),
                account_id: r.get(1),
                account_code: r.get(2),
                account_name: r.get(3),
                ledger_debits: store::decimal(r.get(4))?,
                ledger_credits: store::decimal(r.get(5))?,
                document_debits: store::decimal(r.get(6))?,
                document_credits: store::decimal(r.get(7))?,
                difference: store::decimal(r.get(8))?,
            })
        })
        .collect::<bilibi_database::Result<_>>()?;
    let tax_type = loaded.tax_return.tax_type;
    Ok(Figures {
        tax_type,
        period_start: loaded.tax_return.period_start,
        period_end: loaded.tax_return.period_end,
        service_tax_basis: registration
            .filter(|_| tax_type == TaxType::ServiceTax)
            .map(|r| r.service_tax_basis.clone()),
        currency_code: event.currency_code.clone().unwrap_or_default(),
        minor_units: event.minor_units.unwrap_or_default(),
        through_entry_number: event.through_entry_number.unwrap_or_default(),
        through_mapping_number: event.through_mapping_number.unwrap_or_default(),
        through_adjustment_number: event.through_adjustment_number,
        boxes: form::boxes(tax_type)
            .iter()
            .map(|b| {
                let (taxable_amount, tax_amount) = stored.get(b.key).cloned().unwrap_or_default();
                BoxFigure {
                    key: b.key,
                    item: b.item,
                    part: b.part,
                    kind: b.kind,
                    label: b.label,
                    rate: b.rate(),
                    taxable_amount,
                    tax_amount,
                }
            })
            .collect(),
        tax_payable: event.tax_payable.unwrap_or_default(),
        sources,
        unmapped: Vec::new(),
        rate_mismatches: Vec::new(),
        accounts,
    })
}

/// Each box of `now` whose figures differ from `before`'s.
fn box_differences(now: &[BoxFigure], before: &[BoxFigure]) -> Vec<BoxChange> {
    let difference = |now: Option<Decimal>, before: Option<Decimal>| match (now, before) {
        (None, None) => None,
        (now, before) => Some(now.unwrap_or_default() - before.unwrap_or_default()),
    };
    now.iter()
        .filter_map(|b| {
            let old = before.iter().find(|o| o.key == b.key)?;
            let taxable = difference(b.taxable_amount, old.taxable_amount);
            let tax = difference(b.tax_amount, old.tax_amount);
            let zero = |d: Option<Decimal>| d.is_none_or(|d| d.is_zero());
            (!(zero(taxable) && zero(tax))).then_some(BoxChange {
                key: b.key,
                item: b.item,
                label: b.label,
                taxable_amount: b.taxable_amount,
                tax_amount: b.tax_amount,
                taxable_change: taxable,
                tax_change: tax,
            })
        })
        .collect()
}

fn adjustment_views(adjustments: &[Adjustment]) -> Vec<ReturnAdjustment> {
    let withdrawn: HashMap<i64, &Adjustment> = adjustments
        .iter()
        .filter(|a| a.action == "withdrawn")
        .filter_map(|a| Some((a.withdraws_adjustment_id?, a)))
        .collect();
    adjustments
        .iter()
        .filter(|a| a.action == "added")
        .map(|a| {
            let withdrawal = withdrawn.get(&a.id).map(|w| Withdrawal {
                id: w.id,
                adjustment_number: w.adjustment_number,
                description: w.description.clone(),
                recorded_by_user_id: w.recorded_by_user_id,
                recorded_by_impersonator_id: w.recorded_by_impersonator_id,
                recorded_at: w.recorded_at.clone(),
            });
            ReturnAdjustment {
                id: a.id,
                adjustment_number: a.adjustment_number,
                box_key: a.box_key.clone().unwrap_or_default(),
                taxable_amount: a.taxable_amount.unwrap_or_default(),
                tax_amount: a.tax_amount.unwrap_or_default(),
                description: a.description.clone(),
                reference: a.reference.clone(),
                copied_from_adjustment_id: a.copied_from_adjustment_id,
                recorded_by_user_id: a.recorded_by_user_id,
                recorded_by_impersonator_id: a.recorded_by_impersonator_id,
                recorded_at: a.recorded_at.clone(),
                standing: withdrawal.is_none(),
                withdrawal,
            }
        })
        .collect()
}

/// Whether two readings carry the same amounts in every box, source row and
/// account figure.
fn same_amounts(a: &Figures, b: &Figures) -> bool {
    let boxes = |f: &Figures| {
        f.boxes
            .iter()
            .map(|b| (b.key, b.taxable_amount, b.tax_amount))
            .collect::<Vec<_>>()
    };
    let sources = |f: &Figures| {
        f.sources
            .iter()
            .map(|r| {
                (
                    r.source,
                    r.document_kind.clone(),
                    r.measure.clone(),
                    r.tax_code_id,
                    r.box_key.clone(),
                    r.document_count,
                    r.taxable_amount,
                    r.tax_amount,
                )
            })
            .collect::<Vec<_>>()
    };
    let accounts = |f: &Figures| {
        f.accounts
            .iter()
            .map(|r| {
                (
                    r.account_id,
                    r.ledger_debits,
                    r.ledger_credits,
                    r.document_debits,
                    r.document_credits,
                    r.difference,
                )
            })
            .collect::<Vec<_>>()
    };
    boxes(a) == boxes(b) && sources(a) == sources(b) && accounts(a) == accounts(b)
}

/// The column is numeric(20, 4): sixteen digits before the point.
const AMOUNT_LIMIT: Decimal = Decimal::from_parts(0x6FC1_0000, 0x0023_86F2, 0, false, 0);

/// One adjustment amount as written: blank or left out is zero when the box
/// does not take the column, and missing otherwise.
fn adjustment_amount(
    field: &'static str,
    text: Option<&str>,
    taken: bool,
    minor_units: u32,
    errors: &mut Vec<FieldError>,
) -> Option<Decimal> {
    let Some(text) = bilibi_ui::params::blank_to_none(text) else {
        if taken {
            errors.push(FieldError::new(field, "can't be blank"));
            return None;
        }
        return Some(Decimal::ZERO);
    };
    let Ok(amount) = Decimal::from_str_exact(&text) else {
        errors.push(FieldError::new(field, "is invalid"));
        return None;
    };
    let message = if !taken && !amount.is_zero() {
        "is not a column of this box".to_owned()
    } else if amount.is_sign_negative() && !amount.is_zero() {
        "must not be negative".to_owned()
    } else if amount >= AMOUNT_LIMIT {
        "is too large".to_owned()
    } else if amount.round_dp(minor_units) != amount {
        format!("has more than {minor_units} decimal places")
    } else {
        return Some(amount);
    };
    errors.push(FieldError::new(field, message));
    None
}

fn length(field: &'static str, text: Option<&str>, max: usize, errors: &mut Vec<FieldError>) {
    if text.is_some_and(|t| t.chars().count() > max) {
        errors.push(FieldError::new(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
}

impl Sst {
    /// The company's returns, oldest first, without figures; `tax_type`
    /// filters them and `limit` (1 to 500, default 100) and `after`, the
    /// last id already read, page them.
    pub async fn list_returns(
        &self,
        s: &Scope,
        company: i64,
        tax_type: Option<TaxType>,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<Vec<TaxReturn>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let (limit, after) = page_options(limit, after)?;
        let tax_type = tax_type.map(TaxType::as_str);
        Ok(read_returns(
            &**c,
            s.tenant(),
            company,
            "AND ($3::text IS NULL OR r.tax_type=$3) AND r.id>$4 ORDER BY r.id LIMIT $5",
            &[&tax_type, &after, &limit],
        )
        .await?)
    }

    /// One return: its fields, status, events, registration version,
    /// adjustments and figures (see [`ReturnReport`]). An open return's
    /// figures are read now and need the receivables, payables and ledger
    /// view capabilities; a finalised one's snapshot needs only
    /// `finance.sst.view`.
    pub async fn get_return(&self, s: &Scope, company: i64, id: i64) -> Result<ReturnReport> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let loaded = load(&**c, s, company, id).await?;
        self.report(&**c, s, company, loaded).await
    }

    async fn report(
        &self,
        c: &(impl GenericClient + Sync),
        s: &Scope,
        company: i64,
        loaded: Loaded,
    ) -> Result<ReturnReport> {
        let tenant = s.tenant();
        let registration =
            registrations::get(c, tenant, company, loaded.tax_return.registration_id).await?;
        let (figures, posted_since) = match loaded.status() {
            ReturnStatus::Discarded => (None, None),
            ReturnStatus::Open => {
                let registration = registration.as_ref().ok_or(Error::Invalid)?;
                let standing = loaded.standing(i64::MAX);
                let figures = self
                    .compute(
                        s,
                        company,
                        &loaded.period(),
                        registration,
                        &standing,
                        Pins::default(),
                    )
                    .await?;
                (Some(figures), None)
            }
            ReturnStatus::Finalised => {
                let stored = snapshot(c, tenant, company, &loaded, registration.as_ref()).await?;
                let posted_since = match registration.as_ref() {
                    Some(registration) => {
                        self.posted_since(s, company, &loaded, registration, &stored)
                            .await
                    }
                    None => None,
                };
                (Some(stored), posted_since)
            }
        };
        let amends = match loaded.tax_return.amends_return_id {
            Some(amended) => {
                let amended = fetch(c, tenant, company, amended)
                    .await?
                    .ok_or(Error::Invalid)?;
                let previous = snapshot(c, tenant, company, &amended, None).await?;
                Some(Amends {
                    return_id: amended.tax_return.id,
                    return_number: amended.tax_return.return_number,
                    version: amended.tax_return.version,
                    tax_payable: previous.tax_payable,
                    boxes: figures
                        .as_ref()
                        .map(|f| box_differences(&f.boxes, &previous.boxes))
                        .unwrap_or_default(),
                })
            }
            None => None,
        };
        Ok(ReturnReport {
            registration_number: registration
                .as_ref()
                .and_then(|r| r.number(loaded.tax_return.tax_type))
                .map(str::to_owned),
            registration,
            adjustments: adjustment_views(&loaded.adjustments),
            period_ended: loaded.period_ended(),
            figures,
            posted_since,
            amends,
            tax_return: loaded.tax_return,
        })
    }

    /// What the documents dated in a finalised return's period show now,
    /// under the mappings and adjustments it was finalised with; `None` when
    /// the actor may not read them.
    async fn posted_since(
        &self,
        s: &Scope,
        company: i64,
        loaded: &Loaded,
        registration: &Registration,
        stored: &Figures,
    ) -> Option<PostedSince> {
        let event = loaded.finalised()?;
        let standing = loaded.standing(event.through_adjustment_number?);
        let now = self
            .compute(
                s,
                company,
                &loaded.period(),
                registration,
                &standing,
                Pins {
                    through_entry_number: None,
                    through_mapping_number: event.through_mapping_number,
                },
            )
            .await
            .ok()?;
        Some(PostedSince {
            through_entry_number: now.through_entry_number,
            boxes: if now.through_entry_number == stored.through_entry_number {
                Vec::new()
            } else {
                box_differences(&now.boxes, &stored.boxes)
            },
        })
    }

    /// A finalised return's figures read again from the live records at the
    /// entry, mapping and adjustment numbers it recorded, and whether they
    /// equal its snapshot (`ReturnNotFinalised` otherwise). Needs the
    /// receivables, payables and ledger view capabilities.
    pub async fn reproduce_return(&self, s: &Scope, company: i64, id: i64) -> Result<Reproduction> {
        let (loaded, registration, stored) = {
            let c = self.db.get().await.map_err(Error::from)?;
            authorize(&**c, s, company, VIEW).await?;
            let loaded = load(&**c, s, company, id).await?;
            if loaded.finalised().is_none() {
                return Err(SstError::ReturnNotFinalised);
            }
            let registration =
                registrations::get(&**c, s.tenant(), company, loaded.tax_return.registration_id)
                    .await?
                    .ok_or(Error::Invalid)?;
            let stored = snapshot(&**c, s.tenant(), company, &loaded, Some(&registration)).await?;
            (loaded, registration, stored)
        };
        let standing = loaded.standing(stored.through_adjustment_number.unwrap_or_default());
        let figures = self
            .compute(
                s,
                company,
                &loaded.period(),
                &registration,
                &standing,
                Pins {
                    through_entry_number: Some(stored.through_entry_number),
                    through_mapping_number: Some(stored.through_mapping_number),
                },
            )
            .await?;
        Ok(Reproduction {
            reproduced: same_amounts(&figures, &stored),
            figures,
        })
    }

    /// Opens the return of `tax_type` for the taxable period starting on
    /// `period_start`, under the registration version governing that date
    /// (`finance.sst.returns.sst-prepare`). Refusals: `NotRegistered`,
    /// `NotRegisteredForTax`, `NotATaxablePeriod`, `ReturnOpen`,
    /// `ReturnFinalised` (amend it instead) and `PeriodOverlaps`.
    pub async fn open_return(
        &self,
        s: &Scope,
        company: i64,
        tax_type: TaxType,
        period_start: Date,
    ) -> Result<TaxReturn> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                PREPARE_RETURNS,
                "finance.sst.open-return",
            )
            .await?;
        let tenant = s.tenant();
        let registration = registrations::governing(tx.sql(), tenant, company, period_start)
            .await?
            .ok_or(SstError::NotRegistered)?;
        if registration.number(tax_type).is_none() {
            return Err(SstError::NotRegisteredForTax);
        }
        let (start, end) = registration
            .cycle()
            .period(period_start)
            .ok_or(SstError::NotATaxablePeriod)?;
        let period = Period {
            tax_type,
            period_start: start,
            period_end: end,
        };
        let returns = period_returns(tx.sql(), tenant, company, &period).await?;
        if returns.iter().any(|r| r.status == ReturnStatus::Open) {
            return Err(SstError::ReturnOpen);
        }
        if returns.iter().any(|r| r.status == ReturnStatus::Finalised) {
            return Err(SstError::ReturnFinalised);
        }
        if overlapping(tx.sql(), tenant, company, &period).await? {
            return Err(SstError::PeriodOverlaps);
        }
        let id = insert_return(
            &mut tx,
            s,
            company,
            &registration,
            &period,
            None,
            None,
            Vec::new(),
        )
        .await?;
        let opened = fetch(tx.sql(), tenant, company, id)
            .await?
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(opened.tax_return)
    }

    /// Opens an amendment of a finalised return (`finance.sst.returns.sst-amend`):
    /// a new return for its tax and period that names it, with a `reason`,
    /// carrying copies of its standing adjustments. Only the period's latest
    /// finalised return is amended, and only while no other return of the
    /// period is open (`ReturnNotFinalised`, `ReturnSuperseded`,
    /// `ReturnOpen`); a missing reason is `Invalid`.
    pub async fn amend_return(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        reason: Option<&str>,
    ) -> Result<TaxReturn> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                AMEND_RETURNS,
                "finance.sst.amend-return",
            )
            .await?;
        let tenant = s.tenant();
        let amended = load(tx.sql(), s, company, id).await?;
        if amended.status() != ReturnStatus::Finalised {
            return Err(SstError::ReturnNotFinalised);
        }
        if amended.tax_return.superseded_by_return_id.is_some() {
            return Err(SstError::ReturnSuperseded);
        }
        let period = amended.period();
        if period_returns(tx.sql(), tenant, company, &period)
            .await?
            .iter()
            .any(|r| r.status == ReturnStatus::Open)
        {
            return Err(SstError::ReturnOpen);
        }
        let registration = registrations::governing(tx.sql(), tenant, company, period.period_start)
            .await?
            .ok_or(SstError::NotRegistered)?;
        let copies = amended
            .standing(i64::MAX)
            .into_iter()
            .map(|a| {
                json!({
                    "action": "added",
                    "copied_from_adjustment_id": a.id,
                    "box": a.box_key,
                    "taxable_amount": a.taxable_amount,
                    "tax_amount": a.tax_amount,
                    "description": a.description,
                    "reference": a.reference,
                })
            })
            .collect();
        let id = insert_return(
            &mut tx,
            s,
            company,
            &registration,
            &period,
            Some(amended.tax_return.id),
            reason,
            copies,
        )
        .await?;
        let opened = fetch(tx.sql(), tenant, company, id)
            .await?
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(opened.tax_return)
    }

    /// Adds an adjustment to an open return (`finance.sst.returns.sst-prepare`):
    /// it adds to its box, and a deduction box (item 13) deducts from the
    /// tax payable. Refusals: `ReturnNotFound`, `ReturnFinalised`,
    /// `ReturnDiscarded`, and `Invalid` with Bilimbi's field messages.
    pub async fn add_adjustment(
        &self,
        s: &Scope,
        company: i64,
        return_id: i64,
        input: &AdjustmentInput,
    ) -> Result<Adjustment> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                PREPARE_RETURNS,
                "finance.sst.add-adjustment",
            )
            .await?;
        let tenant = s.tenant();
        let loaded = load(tx.sql(), s, company, return_id).await?;
        let key = input.box_key.trim();
        let found = form::boxes(loaded.tax_return.tax_type)
            .iter()
            .find(|b| b.adjustable && b.key == key);
        loaded.open()?;
        let Some(found) = found else {
            return Err(SstError::Invalid(vec![FieldError::new(
                "box",
                "is not a box of this return a preparer adjusts",
            )]));
        };
        let book = self.ledger.get_book_on(tx.sql(), s, company).await?;
        let minor_units = u32::from(book.minor_units.get());
        let mut errors = Vec::new();
        let taxable = adjustment_amount(
            "taxable_amount",
            input.taxable_amount.as_deref(),
            found.takes_taxable(),
            minor_units,
            &mut errors,
        );
        let tax = adjustment_amount(
            "tax_amount",
            input.tax_amount.as_deref(),
            found.takes_tax(),
            minor_units,
            &mut errors,
        );
        if let (Some(taxable), Some(tax)) = (taxable, tax) {
            if taxable.is_zero() && tax.is_zero() {
                errors.push(FieldError::new(
                    "tax_amount",
                    "or the value must be above zero",
                ));
            }
        }
        let description = bilibi_ui::params::blank_to_none(input.description.as_deref());
        let reference = bilibi_ui::params::blank_to_none(input.reference.as_deref());
        if description.is_none() {
            errors.push(FieldError::new("description", "can't be blank"));
        }
        length("description", description.as_deref(), 500, &mut errors);
        length("reference", reference.as_deref(), 100, &mut errors);
        if !errors.is_empty() {
            return Err(SstError::Invalid(errors));
        }
        let value = store::row(
            store::identity(s, company),
            json!({
                "return_id": return_id,
                "action": "added",
                "box": found.key,
                "taxable_amount": taxable,
                "tax_amount": tax,
                "description": description,
                "reference": reference,
            }),
        );
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (id, change) = store::insert(sql, Table::Adjustment, value).await?;
                    Ok((id, vec![change]))
                })
            })
            .await?;
        let adjustment = fetch_adjustment(tx.sql(), tenant, company, id)
            .await?
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(adjustment)
    }

    /// Withdraws a standing adjustment of an open return with a `reason`, as
    /// an appended row (`finance.sst.returns.sst-prepare`). Refusals:
    /// `AdjustmentNotFound`, `AdjustmentWithdrawn`, `ReturnFinalised`,
    /// `ReturnDiscarded`, and `Invalid` on `description` (the reason).
    pub async fn withdraw_adjustment(
        &self,
        s: &Scope,
        company: i64,
        adjustment_id: i64,
        reason: Option<&str>,
    ) -> Result<Adjustment> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                PREPARE_RETURNS,
                "finance.sst.withdraw-adjustment",
            )
            .await?;
        let tenant = s.tenant();
        let adjustment = match adjustment_id {
            id if id > 0 => fetch_adjustment(tx.sql(), tenant, company, id).await?,
            _ => None,
        }
        .filter(|a| a.action == "added")
        .ok_or(SstError::AdjustmentNotFound)?;
        let loaded = load(tx.sql(), s, company, adjustment.return_id).await?;
        if loaded
            .adjustments
            .iter()
            .any(|a| a.withdraws_adjustment_id == Some(adjustment.id))
        {
            return Err(SstError::AdjustmentWithdrawn);
        }
        loaded.open()?;
        let description = bilibi_ui::params::blank_to_none(reason);
        let mut errors = Vec::new();
        if description.is_none() {
            errors.push(FieldError::new("description", "can't be blank"));
        }
        length("description", description.as_deref(), 500, &mut errors);
        if !errors.is_empty() {
            return Err(SstError::Invalid(errors));
        }
        let value = store::row(
            store::identity(s, company),
            json!({
                "return_id": adjustment.return_id,
                "action": "withdrawn",
                "withdraws_adjustment_id": adjustment.id,
                "description": description,
            }),
        );
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (id, change) = store::insert(sql, Table::Adjustment, value).await?;
                    Ok((id, vec![change]))
                })
            })
            .await?;
        let withdrawal = fetch_adjustment(tx.sql(), tenant, company, id)
            .await?
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(withdrawal)
    }

    /// Finalises an open return (`finance.sst.returns.sst-finalise`): refuses
    /// one whose period has not ended (`PeriodNotEnded`), one with tax under
    /// a code no mapping names (`UnmappedTaxCodes`) and one with a code
    /// whose rate does not fit its box (`RateMismatch`); otherwise records
    /// the figures read now with the entry, mapping and adjustment numbers
    /// they were read at, every box, every source row and the ledger
    /// reconciliation, difference included. Needs the receivables, payables
    /// and ledger view capabilities.
    pub async fn finalise_return(&self, s: &Scope, company: i64, id: i64) -> Result<TaxReturn> {
        let tenant = s.tenant();
        for _ in 0..FINALISE_ATTEMPTS {
            // Read the figures through the sibling facades' pooled reads
            // before taking the company lock, so no second connection is
            // held while it is; then check, under the lock, that nothing the
            // reading depends on was recorded in between.
            let (loaded, figures) = {
                let c = self.db.get().await.map_err(Error::from)?;
                authorize(&**c, s, company, FINALISE_RETURNS).await?;
                let loaded = load(&**c, s, company, id).await?;
                loaded.open()?;
                if !loaded.period_ended() {
                    return Err(SstError::PeriodNotEnded);
                }
                let registration =
                    registrations::get(&**c, tenant, company, loaded.tax_return.registration_id)
                        .await?
                        .ok_or(Error::Invalid)?;
                drop(c);
                let standing = loaded.standing(i64::MAX);
                let figures = self
                    .compute(
                        s,
                        company,
                        &loaded.period(),
                        &registration,
                        &standing,
                        Pins::default(),
                    )
                    .await?;
                (loaded, figures)
            };
            if !figures.unmapped.is_empty() {
                return Err(SstError::UnmappedTaxCodes(figures.unmapped));
            }
            if !figures.rate_mismatches.is_empty() {
                return Err(SstError::RateMismatch(figures.rate_mismatches));
            }
            let mut c = self.db.get().await.map_err(Error::from)?;
            let mut tx = self
                .write(
                    &mut c,
                    s,
                    company,
                    FINALISE_RETURNS,
                    "finance.sst.finalise-return",
                )
                .await?;
            let now = load(tx.sql(), s, company, id).await?;
            now.open()?;
            let ids = |l: &Loaded| l.adjustments.iter().map(|a| a.id).collect::<Vec<_>>();
            if ids(&now) != ids(&loaded)
                || mappings::last_number(tx.sql(), tenant, company).await?
                    != figures.through_mapping_number
            {
                continue;
            }
            let through_adjustment_number: i64 = tx
                .sql()
                .query_one(
                    "SELECT COALESCE(max(adjustment_number),0) FROM finance_sst_return_adjustments WHERE tenant_id=$1 AND company_id=$2",
                    &[&tenant, &company],
                )
                .await?
                .get(0);
            let owned = || {
                store::row(
                    json!({"tenant_id": tenant, "company_id": company}),
                    json!({"return_id": id}),
                )
            };
            let event = store::row(
                store::identity(s, company),
                json!({
                    "return_id": id,
                    "action": "finalised",
                    "through_entry_number": figures.through_entry_number,
                    "through_mapping_number": figures.through_mapping_number,
                    "through_adjustment_number": through_adjustment_number,
                    "currency_code": figures.currency_code,
                    "minor_units": figures.minor_units,
                    "tax_payable": figures.tax_payable,
                }),
            );
            let mut rows: Vec<(Table, Value)> = vec![(Table::ReturnEvent, event)];
            rows.extend(figures.boxes.iter().enumerate().map(|(n, b)| {
                (
                    Table::ReturnBox,
                    store::row(
                        owned(),
                        json!({"line_number": n + 1, "box": b.key, "taxable_amount": b.taxable_amount, "tax_amount": b.tax_amount}),
                    ),
                )
            }));
            rows.extend(figures.sources.iter().map(|r| {
                (
                    Table::ReturnSource,
                    store::row(
                        owned(),
                        json!({
                            "line_number": r.line_number,
                            "source": r.source.as_str(),
                            "document_kind": r.document_kind,
                            "measure": r.measure,
                            "prior_period": r.prior_period,
                            "tax_code_id": r.tax_code_id,
                            "tax_code": r.tax_code,
                            "rate": r.rate,
                            "account_id": r.account_id,
                            "box": r.box_key,
                            "mapping_number": r.mapping_number,
                            "document_count": r.document_count,
                            "taxable_amount": r.taxable_amount,
                            "tax_amount": r.tax_amount,
                        }),
                    ),
                )
            }));
            rows.extend(figures.accounts.iter().map(|a| {
                (
                    Table::ReturnAccount,
                    store::row(
                        owned(),
                        json!({
                            "line_number": a.line_number,
                            "account_id": a.account_id,
                            "account_code": a.account_code,
                            "account_name": a.account_name,
                            "ledger_debits": a.ledger_debits,
                            "ledger_credits": a.ledger_credits,
                            "document_debits": a.document_debits,
                            "document_credits": a.document_credits,
                            "difference": a.difference,
                        }),
                    ),
                )
            }));
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::with_capacity(rows.len());
                    for (table, value) in rows {
                        changes.push(store::insert(sql, table, value).await?.1);
                    }
                    Ok(((), changes))
                })
            })
            .await?;
            let finalised = fetch(tx.sql(), tenant, company, id)
                .await?
                .ok_or(Error::Invalid)?;
            tx.commit().await?;
            return Ok(finalised.tax_return);
        }
        Err(SstError::Failed(Error::Conflict))
    }

    /// Discards an open return with a `reason` (`finance.sst.returns.sst-prepare`);
    /// it stays on record, numbered, with its adjustments, and the period can
    /// be prepared again. Refusals: `ReturnNotFound`, `ReturnFinalised`,
    /// `ReturnDiscarded`, and `Invalid` on `reason`.
    pub async fn discard_return(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        reason: Option<&str>,
    ) -> Result<TaxReturn> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                PREPARE_RETURNS,
                "finance.sst.discard-return",
            )
            .await?;
        let tenant = s.tenant();
        load(tx.sql(), s, company, id).await?.open()?;
        let mut errors = Vec::new();
        let reason = crate::reason(reason, &mut errors);
        if reason.is_none() {
            errors.push(FieldError::new("reason", "is needed to discard a return"));
        }
        if !errors.is_empty() {
            return Err(SstError::Invalid(errors));
        }
        let event = store::row(
            store::identity(s, company),
            json!({"return_id": id, "action": "discarded", "reason": reason}),
        );
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (_, change) = store::insert(sql, Table::ReturnEvent, event).await?;
                Ok(((), vec![change]))
            })
        })
        .await?;
        let discarded = fetch(tx.sql(), tenant, company, id)
            .await?
            .ok_or(Error::Invalid)?;
        tx.commit().await?;
        Ok(discarded.tax_return)
    }

    /// One return's report for an export: authorized, and refused when it
    /// was discarded.
    pub(crate) async fn export_report(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<(String, ReturnReport)> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let name = bilibi_company::identity(&**c, s, company)
            .await?
            .ok_or(SstError::CompanyNotFound)?
            .display_name;
        let loaded = load(&**c, s, company, id).await?;
        if loaded.status() == ReturnStatus::Discarded {
            return Err(SstError::ReturnDiscarded);
        }
        Ok((name, self.report(&**c, s, company, loaded).await?))
    }
}

/// The period's returns of its tax, each with its status.
async fn period_returns(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    period: &Period,
) -> bilibi_database::Result<Vec<TaxReturn>> {
    read_returns(
        c,
        tenant,
        company,
        "AND r.tax_type=$3 AND r.period_start=$4::text::date ORDER BY r.id",
        &[&period.tax_type.as_str(), &period.period_start.to_string()],
    )
    .await
}

/// Whether a return of the tax not discarded covers a different period that
/// overlaps this one.
async fn overlapping(
    c: &(impl GenericClient + Sync),
    tenant: i64,
    company: i64,
    period: &Period,
) -> bilibi_database::Result<bool> {
    Ok(c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM finance_sst_returns r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.tax_type=$3 AND (r.period_start<>$4::text::date OR r.period_end<>$5::text::date) AND r.period_start<=$5::text::date AND r.period_end>=$4::text::date AND NOT EXISTS (SELECT 1 FROM finance_sst_return_events e WHERE e.return_id=r.id AND e.company_id=r.company_id AND e.action='discarded'))",
            &[
                &tenant,
                &company,
                &period.tax_type.as_str(),
                &period.period_start.to_string(),
                &period.period_end.to_string(),
            ],
        )
        .await?
        .get(0))
}

/// Records a return with its `opened` event and any adjustment `copies`,
/// in one audited write; returns its id.
#[allow(clippy::too_many_arguments)]
async fn insert_return(
    tx: &mut MutationTransaction<'_>,
    s: &Scope,
    company: i64,
    registration: &Registration,
    period: &Period,
    amends: Option<i64>,
    reason: Option<&str>,
    copies: Vec<Value>,
) -> Result<i64> {
    let mut errors = Vec::new();
    let reason = crate::reason(reason, &mut errors);
    if amends.is_some() && reason.is_none() {
        errors.push(FieldError::new(
            "reason",
            "is needed to amend a finalised return",
        ));
    }
    if !errors.is_empty() {
        return Err(SstError::Invalid(errors));
    }
    let due_date = registration
        .cycle()
        .due_date(period.period_end)
        .ok_or(SstError::InvalidOption)?;
    let identity = json!({
        "tenant_id": s.tenant(),
        "company_id": company,
        "opened_by_user_id": s.user(),
        "opened_by_impersonator_id": s.impersonator().map(|p| p.user()),
    });
    let value = store::row(
        identity,
        json!({
            "tax_type": period.tax_type.as_str(),
            "registration_id": registration.id,
            "period_start": period.period_start.to_string(),
            "period_end": period.period_end.to_string(),
            "due_date": due_date.to_string(),
            "amends_return_id": amends,
            "reason": reason,
        }),
    );
    let event = store::row(
        store::identity(s, company),
        json!({"action": "opened", "reason": reason}),
    );
    let copies: Vec<Value> = copies
        .into_iter()
        .map(|copy| store::row(store::identity(s, company), copy))
        .collect();
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let (id, change) = store::insert(sql, Table::Return, value).await?;
                let owned = |row| store::row(row, json!({"return_id": id}));
                let mut changes = vec![
                    change,
                    store::insert(sql, Table::ReturnEvent, owned(event))
                        .await?
                        .1,
                ];
                for copy in copies {
                    changes.push(store::insert(sql, Table::Adjustment, owned(copy)).await?.1);
                }
                Ok((id, changes))
            })
        })
        .await?)
}
