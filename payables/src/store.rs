//! Private SQL for Bilimbi.Finance.Payables records. Never reads sibling tables.
use crate::*;
use bilibi_database::{Change, Result as DbResult};
use serde_json::{json, Value};
use tokio_postgres::{GenericClient, Row};

#[derive(Clone, Copy)]
pub(crate) enum Table {
    OpeningBatch,
    OpeningItem,
    Vendors,
    Settings,
    TaxCodes,
    SettingsChanges,
    TaxCodeChanges,
    Documents,
    Lines,
    TaxLines,
    Allocations,
    Requests,
    RequestAllocations,
    RequestEvents,
    MatchEvents,
    MatchLines,
    MatchExceptions,
    OrderLines,
    OrderReceipts,
}
impl Table {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::OpeningBatch => "finance_payables_opening_batches",
            Self::OpeningItem => "finance_payables_opening_items",
            Self::Vendors => "finance_payables_vendors",
            Self::MatchEvents => "finance_payables_match_events",
            Self::MatchLines => "finance_payables_match_lines",
            Self::MatchExceptions => "finance_payables_match_exceptions",
            Self::OrderLines => "finance_payables_order_lines",
            Self::OrderReceipts => "finance_payables_order_receipts",
            Self::Settings => "finance_payables_settings",
            Self::TaxCodes => "finance_payables_tax_codes",
            Self::SettingsChanges => "finance_payables_settings_changes",
            Self::TaxCodeChanges => "finance_payables_tax_code_changes",
            Self::Documents => "finance_payables_documents",
            Self::Lines => "finance_payables_document_lines",
            Self::TaxLines => "finance_payables_document_tax_lines",
            Self::Allocations => "finance_payables_payment_allocations",
            Self::Requests => "finance_payables_payment_requests",
            Self::RequestAllocations => "finance_payables_payment_request_allocations",
            Self::RequestEvents => "finance_payables_payment_request_events",
        }
    }
    fn kind(self) -> &'static str {
        match self {
            Self::OpeningBatch => "finance.payables.opening_batch",
            Self::OpeningItem => "finance.payables.opening_item",
            Self::Vendors => "finance.payables.vendor",
            Self::Settings => "finance.payables.settings",
            Self::TaxCodes => "finance.payables.tax_code",
            Self::SettingsChanges => "finance.payables.settings_change",
            Self::TaxCodeChanges => "finance.payables.tax_code_change",
            Self::Documents => "finance.payables.document",
            Self::Lines => "finance.payables.document_line",
            Self::TaxLines => "finance.payables.document_tax_line",
            Self::Allocations => "finance.payables.allocation",
            Self::Requests => "finance.payables.payment_request",
            Self::RequestAllocations => "finance.payables.payment_request_allocation",
            Self::RequestEvents => "finance.payables.payment_request_event",
            Self::MatchEvents => "finance.payables.match_event",
            Self::MatchLines => "finance.payables.match_line",
            Self::MatchExceptions => "finance.payables.match_exception",
            Self::OrderLines => "finance.payables.order_line",
            Self::OrderReceipts => "finance.payables.order_receipt",
        }
    }
}
/// All identifiers come from the private enum. Populate only this module's
/// rows; every inserted row is returned verbatim for mutation capture.
pub(crate) async fn insert(
    c: &(impl GenericClient + Sync),
    table: Table,
    value: Value,
) -> DbResult<(Value, Change)> {
    let name = table.name();
    let row=c.query_one(&format!("INSERT INTO {name} SELECT (jsonb_populate_record(NULL::{name}, $1::jsonb || jsonb_build_object('id',nextval(pg_get_serial_sequence('{name}','id')), 'inserted_at',timezone('UTC',clock_timestamp()), 'updated_at',timezone('UTC',clock_timestamp())))).* RETURNING to_jsonb({name})"),&[&value]).await?;
    let value: Value = row.get(0);
    let id = value["id"].as_i64().ok_or(Error::Invalid)?;
    Ok((
        value.clone(),
        Change {
            kind: table.kind().into(),
            id,
            old: Value::Null,
            new: value,
        },
    ))
}
pub(crate) fn update_change(table: Table, id: i64, before: Value, after: Value) -> Change {
    Change {
        kind: table.kind().into(),
        id,
        old: before,
        new: after,
    }
}
pub(crate) async fn raw(
    c: &(impl GenericClient + Sync),
    t: Table,
    tenant: i64,
    company: i64,
    id: i64,
) -> DbResult<Value> {
    let n = t.name();
    Ok(c.query_one(
        &format!("SELECT to_jsonb(t) FROM {n} t WHERE tenant_id=$1 AND company_id=$2 AND id=$3"),
        &[&tenant, &company, &id],
    )
    .await?
    .get(0))
}
pub(crate) async fn settings(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
) -> DbResult<Option<(i64, Settings)>> {
    let r=c.query_opt("SELECT id,payable_account_id,quantity_tolerance_kind,quantity_tolerance,price_tolerance_kind,price_tolerance FROM finance_payables_settings WHERE tenant_id=$1 AND company_id=$2",&[&tenant,&company]).await?;
    Ok(r.map(|r| {
        (
            r.get(0),
            Settings {
                payable_account_id: r.get(1),
                quantity_tolerance: tolerance(&r, 2, 3),
                price_tolerance: tolerance(&r, 4, 5),
            },
        )
    }))
}
fn tolerance(r: &Row, k: usize, v: usize) -> Option<Tolerance> {
    r.get::<_, Option<String>>(k).map(|kind| Tolerance {
        kind: if kind == "percent" {
            ToleranceKind::Percent
        } else {
            ToleranceKind::Amount
        },
        value: r.get(v),
    })
}
pub(crate) fn settings_json(s: &Settings) -> Value {
    json!({"payable_account_id":s.payable_account_id,
        "quantity_tolerance_kind":s.quantity_tolerance.as_ref().map(|t|t.kind),"quantity_tolerance":s.quantity_tolerance.as_ref().map(|t|t.value),
        "price_tolerance_kind":s.price_tolerance.as_ref().map(|t|t.kind),"price_tolerance":s.price_tolerance.as_ref().map(|t|t.value)})
}
pub(crate) async fn taxes(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
) -> DbResult<Vec<TaxCode>> {
    Ok(c.query("SELECT id,code,name,rate,account_id,active FROM finance_payables_tax_codes WHERE tenant_id=$1 AND company_id=$2 ORDER BY code",&[&tenant,&company]).await?.iter().map(|r| TaxCode {id:r.get(0),code:r.get(1),name:r.get(2),rate:r.get(3),account_id:r.get(4),active:r.get(5)}).collect())
}
pub(crate) fn money(d: Decimal) -> DbResult<Money> {
    Money::new(
        d,
        bilibi_finance_ledger::MinorUnits::new(4).ok_or(Error::Invalid)?,
    )
    .map_err(|_| Error::Invalid)
}
fn text(r: &Value, k: &str) -> DbResult<String> {
    r[k].as_str().map(str::to_owned).ok_or(Error::Invalid)
}
fn opt_text(r: &Value, k: &str) -> Option<String> {
    r[k].as_str().map(str::to_owned)
}
fn int(r: &Value, k: &str) -> DbResult<i64> {
    r[k].as_i64().ok_or(Error::Invalid)
}
fn opt_int(r: &Value, k: &str) -> Option<i64> {
    r[k].as_i64()
}
fn amount(r: &Value, k: &str) -> DbResult<Money> {
    // Keep PostgreSQL numeric exact even with serde_json/arbitrary_precision;
    // Decimal's default visitor does not accept its private number map.
    let d = Decimal::from_str_exact(&r[k].to_string()).map_err(|_| Error::Invalid)?;
    money(d)
}
pub(crate) async fn document(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> DbResult<Option<Document>> {
    let Some(mut document) = document_record(c, tenant, company, id).await? else {
        return Ok(None);
    };
    document.debit_note_ids = c.query("SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND corrects_document_id=$3 ORDER BY id",&[&tenant,&company,&id]).await?.iter().map(|r|r.get(0)).collect();
    document.match_events = match_events(c, tenant, company, id, false).await?;
    Ok(Some(document))
}
// The immutable record has at most 500 lines and 500 tax groups. Corrections
// are a separate collection, paged by document.list in the agent contract.
pub(crate) async fn document_record(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> DbResult<Option<Document>> {
    let Some(row)=c.query_opt("SELECT to_jsonb(d) FROM finance_payables_documents d WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id]).await? else {return Ok(None)};
    let v: Value = row.get(0);
    let mut lines = Vec::new();
    for r in c.query("SELECT id,line_number,description,account_id,amount,tax_code_id,external_order_line_id,order_line_id,quantity FROM finance_payables_document_lines WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 ORDER BY line_number",&[&tenant,&company,&id]).await? {
        lines.push(DocumentLine {id:r.get(0),line_number:r.get(1),description:r.get(2),account_id:r.get(3),amount:money(r.get(4))?,tax_code_id:r.get(5),external_order_line_id:r.get(6),order_line_id:r.get(7),quantity:r.get(8)});
    }
    let mut tax_lines = Vec::new();
    for r in c.query("SELECT id,line_number,tax_code_id,tax_code,rate,account_id,taxable_amount,tax_amount FROM finance_payables_document_tax_lines WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 ORDER BY line_number",&[&tenant,&company,&id]).await? {
        tax_lines.push(TaxLine {id:r.get(0),line_number:r.get(1),tax_code_id:r.get(2),tax_code:r.get(3),rate:r.get(4),account_id:r.get(5),taxable_amount:money(r.get(6))?,tax_amount:money(r.get(7))?});
    }
    let status = text(&v, "status")?;
    let kind = text(&v, "document_type")?;
    let outstanding = if ["bill", "opening_credit"].contains(&kind.as_str()) && status == "posted" {
        Some(outstanding(c, tenant, company, id).await?)
    } else {
        None
    };
    Ok(Some(Document {
        settlements:c.query("SELECT to_jsonb(e)-'review' FROM finance_payables_settlements e WHERE e.tenant_id=$1 AND e.company_id=$2 AND (e.credit_document_id=$3 OR e.target_document_id=$3) ORDER BY e.settlement_number",&[&tenant,&company,&id]).await?.iter().map(|r|r.get(0)).collect(),
        opening: c.query_opt("SELECT i.source_facts || jsonb_build_object('source_reference',b.source_reference,'conversion_date',b.conversion_date::text,'batch_id',b.id,'replaced_by_batch_id',(SELECT x.batch_id FROM finance_payables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=b.id)) FROM finance_payables_opening_items i JOIN finance_payables_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3",&[&tenant,&company,&id]).await?.map(|r|r.get(0)),
        id,
        vendor_id: crate::vendors::of_key(c, tenant, company, &text(&v, "party_key")?).await?,
        document_type: kind,
        status,
        document_number: opt_int(&v, "document_number"),
        corrects_document_id: opt_int(&v, "corrects_document_id"),
        party: Party {
            code: text(&v, "party_code")?,
            key: text(&v, "party_key")?,
            name: text(&v, "party_name")?,
            tax_id: opt_text(&v, "party_tax_id"),
            registration_id: opt_text(&v, "party_registration_id"),
            address: opt_text(&v, "party_address"),
        },
        party_document_number: opt_text(&v, "party_document_number"),
        document_date: text(&v, "document_date")?
            .parse()
            .map_err(|_| Error::Invalid)?,
        due_date: text(&v, "due_date")?.parse().map_err(|_| Error::Invalid)?,
        currency_code: text(&v, "currency_code")?,
        description: opt_text(&v, "description"),
        total_net: amount(&v, "total_net")?,
        total_tax: amount(&v, "total_tax")?,
        total: amount(&v, "total")?,
        created_by_user_id: int(&v, "created_by_user_id")?,
        created_by_impersonator_id: opt_int(&v, "created_by_impersonator_id"),
        created_at: text(&v, "created_at")?,
        approved_by_user_id: opt_int(&v, "approved_by_user_id"),
        approved_by_impersonator_id: opt_int(&v, "approved_by_impersonator_id"),
        approved_at: opt_text(&v, "approved_at"),
        posted_by_user_id: opt_int(&v, "posted_by_user_id"),
        posted_by_impersonator_id: opt_int(&v, "posted_by_impersonator_id"),
        posted_at: opt_text(&v, "posted_at"),
        voided_by_user_id: opt_int(&v, "voided_by_user_id"),
        voided_by_impersonator_id: opt_int(&v, "voided_by_impersonator_id"),
        voided_at: opt_text(&v, "voided_at"),
        void_reason: opt_text(&v, "void_reason"),
        payable_account_id: opt_int(&v, "payable_account_id"),
        entry_id: opt_int(&v, "entry_id"),
        entry_number: opt_int(&v, "entry_number"),
        lines,
        tax_lines,
        debit_note_ids: Vec::new(),
        outstanding,
        match_status: None,
        match_events: match_events(c, tenant, company, id, true).await?,
    })
    .map(|mut d| {
        d.match_status = matching::status(&d);
        d
    }))
}
/// Current balance uses the same temporal calculation as reports and writes.
/// A document's match events oldest first, with their lines in line order
/// and each line's exceptions in recording order.
async fn match_events(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    document: i64,
    current_only: bool,
) -> DbResult<Vec<MatchEvent>> {
    let mut events = Vec::new();
    // A review only needs the latest event and the hold it approves, never
    // the growing history. Both rows are bounded by the document's 500 lines.
    for r in c.query("WITH latest AS (SELECT id,held_event_id FROM finance_payables_match_events WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 ORDER BY id DESC LIMIT 1) SELECT id,event,reason,held_event_id,actor_user_id,actor_impersonator_id,to_char(occurred_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US') FROM finance_payables_match_events WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 AND (NOT $4 OR id IN (SELECT id FROM latest UNION SELECT held_event_id FROM latest)) ORDER BY id",&[&tenant,&company,&document,&current_only]).await? {
        events.push(MatchEvent {id:r.get(0),event:r.get(1),reason:r.get(2),held_event_id:r.get(3),actor_user_id:r.get(4),actor_impersonator_id:r.get(5),occurred_at:r.get(6),lines:Vec::new()});
    }
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    if ids.is_empty() {
        return Ok(events);
    }
    let mut lines: Vec<(i64, i64, MatchLine)> = Vec::new();
    for r in c.query("SELECT event_id,id,line_number,order_line_id,order_number,billed_quantity,billed_unit_price,previously_billed_quantity,ordered_quantity,ordered_unit_price,received_quantity,quantity_tolerance_kind,quantity_tolerance,price_tolerance_kind,price_tolerance FROM finance_payables_match_lines WHERE tenant_id=$1 AND company_id=$2 AND event_id=ANY($3) ORDER BY event_id,line_number",&[&tenant,&company,&ids]).await? {
        lines.push((r.get(0), r.get(1), MatchLine {line_number:r.get(2),order_line_id:r.get(3),order_number:r.get(4),billed_quantity:r.get(5),billed_unit_price:r.get(6),previously_billed_quantity:r.get(7),ordered_quantity:r.get(8),ordered_unit_price:r.get(9),received_quantity:r.get(10),quantity_tolerance:tolerance(&r,11,12),price_tolerance:tolerance(&r,13,14),exceptions:Vec::new()}));
    }
    let line_ids: Vec<i64> = lines.iter().map(|l| l.1).collect();
    for r in c.query("SELECT match_line_id,code,detail FROM finance_payables_match_exceptions WHERE tenant_id=$1 AND company_id=$2 AND match_line_id=ANY($3) ORDER BY id",&[&tenant,&company,&line_ids]).await? {
        let id: i64 = r.get(0);
        if let Some(line) = lines.iter_mut().find(|l| l.1 == id) {
            line.2.exceptions.push(MatchException {code:r.get(1),detail:r.get(2)});
        }
    }
    for (event, _, line) in lines {
        if let Some(e) = events.iter_mut().find(|e| e.id == event) {
            e.lines.push(line);
        }
    }
    Ok(events)
}
/// Records one match event with its lines and their exceptions, in the
/// transaction that decides it; the database stamps the time and checks the
/// document's status. Returns every inserted row as a change.
pub(crate) async fn record_match(
    sql: &(impl GenericClient + Sync),
    document: &Document,
    (tenant, company): (i64, i64),
    actor: (i64, Option<i64>),
    event: &str,
    approval: Option<(&str, i64)>,
    lines: &[MatchLine],
) -> DbResult<Vec<Change>> {
    let (row, change) = insert(sql, Table::MatchEvents, json!({"tenant_id":tenant,"company_id":company,"document_id":document.id,"event":event,"reason":approval.map(|a|a.0),"held_event_id":approval.map(|a|a.1),"actor_user_id":actor.0,"actor_impersonator_id":actor.1})).await?;
    let event_id = int(&row, "id")?;
    let mut changes = vec![change];
    for l in lines {
        let kind = |t: &Option<Tolerance>| t.as_ref().map(|t| t.kind);
        let value = |t: &Option<Tolerance>| t.as_ref().map(|t| t.value);
        let (row, change) = insert(sql, Table::MatchLines, json!({"tenant_id":tenant,"company_id":company,"event_id":event_id,"document_id":document.id,"line_number":l.line_number,"order_line_id":l.order_line_id,"order_number":l.order_number,"billed_quantity":l.billed_quantity,"billed_unit_price":l.billed_unit_price,"previously_billed_quantity":l.previously_billed_quantity,"ordered_quantity":l.ordered_quantity,"ordered_unit_price":l.ordered_unit_price,"received_quantity":l.received_quantity,"quantity_tolerance_kind":kind(&l.quantity_tolerance),"quantity_tolerance":value(&l.quantity_tolerance),"price_tolerance_kind":kind(&l.price_tolerance),"price_tolerance":value(&l.price_tolerance)})).await?;
        let line_id = int(&row, "id")?;
        changes.push(change);
        for x in &l.exceptions {
            changes.push(insert(sql, Table::MatchExceptions, json!({"tenant_id":tenant,"company_id":company,"match_line_id":line_id,"code":x.code,"detail":x.detail})).await?.1);
        }
    }
    Ok(changes)
}
/// With only documents in NP1, debit notes are the reductions. NP3 extends
/// this date-window minimum with allocations and reversal events.
pub(crate) async fn outstanding(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    bill: i64,
) -> DbResult<Money> {
    if c.query_opt("SELECT i.id FROM finance_payables_opening_items i JOIN finance_payables_opening_decisions x ON x.replaces_batch_id=i.batch_id AND x.tenant_id=$1 AND x.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3",&[&tenant,&company,&bill]).await?.is_some() {return Ok(Money::ZERO);}
    crate::balances::owed(
        c,
        tenant,
        company,
        bill,
        jiff::civil::date(9999, 12, 31),
        i64::MAX,
    )
    .await
}
