//! A bounded list of postings and manual journals awaiting or refused posting.
use bilibi_agent_api::OperationClient;
use bilibi_database::Result;
use rust_decimal::Decimal;
use serde_json::{json, Value};

/// Approved journals are represented by their entry, so each posting appears once.
pub(crate) async fn list(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    input: &Value,
    limit: usize,
) -> Result<Value> {
    let from = input["from"].as_str();
    let to = input["to"].as_str();
    let status = input["status"].as_str();
    let source = input["source"].as_str();
    let recording = input["recording"].as_str();
    let review = input["review"].as_str();
    let approaching = crate::late_recordings::APPROACHING_DAYS;
    let late = crate::late_recordings::LATE_DAYS;
    let after_time = input["after_time"].as_str();
    let after_id = input["after_id"].as_i64();
    let after_kind = input["after_kind"].as_str();
    let account = input["account_id"].as_i64();
    let through = input["through_entry_number"].as_i64();
    let search = input["search"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let rows = client.query(
        &format!("WITH items AS (\
         SELECT e.id, 'entry'::text AS kind, e.posted_at AS stamp, e.entry_number AS number, \
          e.entry_date, e.description, e.reference, CASE WHEN EXISTS (SELECT 1 FROM finance_ledger_opening_balances o JOIN finance_ledger_journal_events v ON v.journal_id=o.journal_id AND v.tenant_id=o.tenant_id AND v.company_id=o.company_id WHERE o.tenant_id=e.tenant_id AND o.company_id=e.company_id AND v.entry_id=e.id) THEN 'opening' ELSE COALESCE(e.owner_module,'manual') END AS source, \
          CASE WHEN EXISTS (SELECT 1 FROM finance_ledger_entries r WHERE r.tenant_id=e.tenant_id AND r.company_id=e.company_id AND r.reverses_entry_id=e.id) THEN 'reversed' \
               WHEN e.reverses_entry_id IS NOT NULL THEN 'reversal' ELSE 'posted' END AS status, \
          (SELECT COALESCE(sum(l.debit),0) FROM finance_ledger_entry_lines l WHERE l.tenant_id=e.tenant_id AND l.company_id=e.company_id AND l.entry_id=e.id) AS amount, \
          (SELECT a.journal_number FROM finance_ledger_journal_events v JOIN finance_ledger_journals a ON a.id=v.journal_id AND a.tenant_id=v.tenant_id AND a.company_id=v.company_id WHERE v.entry_id=e.id AND v.tenant_id=e.tenant_id AND v.company_id=e.company_id) AS request, \
          CASE WHEN {NOT_CLOSING} THEN CAST(e.posted_at AS date) - e.entry_date ELSE NULL::integer END AS recorded_days, \
          EXISTS (SELECT 1 FROM finance_ledger_late_recording_reviews r WHERE r.tenant_id=e.tenant_id AND r.company_id=e.company_id AND r.entry_id=e.id) AS reviewed \
         FROM finance_ledger_entries e WHERE e.tenant_id=$1 AND e.company_id=$2 \
          AND ($10::bigint IS NULL OR e.entry_number <= $10) \
          AND ($9::bigint IS NULL OR EXISTS (SELECT 1 FROM finance_ledger_entry_lines l WHERE l.tenant_id=e.tenant_id AND l.company_id=e.company_id AND l.entry_id=e.id AND l.account_id=$9)) \
         UNION ALL \
         SELECT j.id, 'journal', j.submitted_at, NULL::bigint, j.entry_date, j.description, j.reference, CASE WHEN EXISTS (SELECT 1 FROM finance_ledger_opening_balances o WHERE o.tenant_id=j.tenant_id AND o.company_id=j.company_id AND o.journal_id=j.id) THEN 'opening' ELSE 'manual' END, \
          COALESCE(ev.event,'submitted'), \
          (SELECT COALESCE(sum(l.debit),0) FROM finance_ledger_journal_lines l WHERE l.tenant_id=j.tenant_id AND l.company_id=j.company_id AND l.journal_id=j.id), \
          j.journal_number, NULL::integer, NULL::boolean \
         FROM finance_ledger_journals j LEFT JOIN finance_ledger_journal_events ev ON ev.journal_id=j.id AND ev.tenant_id=j.tenant_id AND ev.company_id=j.company_id \
         WHERE j.tenant_id=$1 AND j.company_id=$2 AND COALESCE(ev.event,'submitted') <> 'approved' \
          AND $10::bigint IS NULL \
          AND ($9::bigint IS NULL OR EXISTS (SELECT 1 FROM finance_ledger_journal_lines l WHERE l.tenant_id=j.tenant_id AND l.company_id=j.company_id AND l.journal_id=j.id AND l.account_id=$9)) \
         ) SELECT id,kind,to_char(stamp,'YYYY-MM-DD\"T\"HH24:MI:SS.US'),number,entry_date::text,description,source,status,amount,request,recorded_days,reviewed \
         FROM items WHERE ($3::text IS NULL OR entry_date >= $3::text::date) AND ($4::text IS NULL OR entry_date <= $4::text::date) \
          AND ($5::text IS NULL OR status=$5) AND ($6::text IS NULL OR source=$6) \
          AND ($13::text IS NULL OR number::text=$13 OR request::text=$13 OR position(lower($13) in lower(description)) > 0 OR position(lower($13) in lower(COALESCE(reference,''))) > 0) \
          AND ($7::text IS NULL OR (stamp,id,kind) < ($7::text::timestamp,$8::bigint,$11::text)) \
          AND ($14::text IS NULL OR CASE WHEN recorded_days > $17 THEN 'late' WHEN recorded_days >= $16 THEN 'approaching' WHEN recorded_days IS NOT NULL THEN 'on_time' WHEN kind='journal' THEN 'unposted' ELSE 'excluded' END = $14) \
          AND ($15::text IS NULL OR (recorded_days >= $16 AND reviewed = ($15='reviewed'))) \
         ORDER BY stamp DESC,id DESC,kind DESC LIMIT $12", NOT_CLOSING=crate::periods::NOT_CLOSING),
        &[&tenant,&company,&from,&to,&status,&source,&after_time,&after_id,&account,&through,&after_kind,&((limit + 1) as i64),&search,&recording,&review,&approaching,&late]
    ).await?;
    let more = rows.len() > limit;
    let records: Vec<_> = rows.into_iter().take(limit).map(|r| json!({
        "id":r.get::<_,i64>(0), "kind":r.get::<_,String>(1), "stamp":r.get::<_,String>(2),
        "number":r.get::<_,Option<i64>>(3), "date":r.get::<_,String>(4), "description":r.get::<_,String>(5),
        "source":r.get::<_,String>(6), "status":r.get::<_,String>(7), "amount":r.get::<_,Decimal>(8).to_string(), "request":r.get::<_,Option<i64>>(9),
        "recorded_days": r.get::<_,Option<i32>>(10),
        "recording": match r.get::<_,Option<i32>>(10) { Some(d) if d > late => "late", Some(d) if d >= approaching => "approaching", Some(_) => "on_time", None if r.get::<_,String>(1) == "journal" => "unposted", None => "excluded" },
        "review": match r.get::<_,Option<i32>>(10) { Some(d) if d >= approaching => if r.get::<_,Option<bool>>(11) == Some(true) { "reviewed" } else { "pending" }, _ => "not_required" }
    })).collect();
    let sources: Vec<String> = client.query("SELECT DISTINCT owner_module FROM finance_ledger_entries WHERE tenant_id=$1 AND company_id=$2 AND owner_module IS NOT NULL ORDER BY owner_module", &[&tenant,&company]).await?.into_iter().map(|r| r.get(0)).collect();
    Ok(json!({"records":records,"more":more,"sources":sources}))
}
