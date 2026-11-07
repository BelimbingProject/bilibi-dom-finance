//! The year-to-date bridge from retained monthly source movements to the
//! approved opening journal, read through `finance.reports.source_period.bridge`
//! on a company whose conversion is 1 July 2026 in a calendar fiscal year (the
//! conversion fixture's `setup`): expense 5000 opens at 2,000 debit and sales
//! 4000 at 5,500 credit.
use bilibi_agent_api::{Handler, Registry};
use bilibi_database::Db;
use bilibi_finance_ledger::Ledger;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use jiff::civil::date;
use serde_json::{json, Value};
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

const BRIDGE: &str = "finance.reports.source_period.bridge";
const IMPORT: &str = "finance.reports.source_period.import";

/// The chart's account id for a code.
async fn account(ledger: &Ledger, s: &Scope, company: i64, code: &str) -> Outcome<i64> {
    Ok(ledger
        .list_accounts(s, company)
        .await?
        .into_iter()
        .find(|a| a.code == code)
        .ok_or("bridge fixture account")?
        .id)
}

/// Retains one month of source movements through the reviewed import.
async fn retain(
    db: &Db,
    services: &Services,
    s: &Scope,
    company: i64,
    reference: &str,
    (from, to): (&str, &str),
    rows: Value,
) -> Outcome<()> {
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let Handler::SafeWrite(import) = registry.operations[IMPORT].definition.handler else {
        return Err("source schedule import must be a reviewed write".into());
    };
    let input = json!({"company_id":company,"kind":"movement","source_reference":reference,"source_system":"Source system","currency_code":"MYR","accounting_timezone":"Asia/Kuala_Lumpur","period_start":from,"period_end":to,"rows":rows});
    let mut raw = db.get().await?;
    let mut tx = ledger
        .write(
            &mut raw,
            s,
            company,
            &[bilibi_finance_ledger::MANAGE_ACCOUNTS],
            "walkthrough.source-bridge",
        )
        .await?;
    import.preview(tx.sql(), s, &input).await?;
    import.commit(&mut tx, s, &input).await?;
    tx.commit().await?;
    Ok(())
}

async fn read(db: &Db, services: &Services, s: &Scope, company: i64) -> Outcome<Value> {
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    Ok(registry
        .read(db, s, BRIDGE, &json!({"company_id":company}))
        .await?["result"]
        .clone())
}

fn month(from: u8) -> (String, String) {
    let start = date(2026, from as i8, 1);
    (start.to_string(), start.last_of_month().to_string())
}

/// January to May: expense 300 debit and sales 1,000 credit a month, and
/// in January a bank movement the bridge must leave out.
pub async fn seed(db: &Db, services: &Services, s: &Scope, company: i64) -> Outcome<()> {
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let (expense, sales, bank) = (
        account(&ledger, s, company, "5000").await?,
        account(&ledger, s, company, "4000").await?,
        account(&ledger, s, company, "1000").await?,
    );
    for m in 1..=5 {
        let (from, to) = month(m);
        let mut rows = json!([
            {"source_id":"cost","account_id":expense,"side":"debit","amount":"300.00"},
            {"source_id":"sales","account_id":sales,"side":"credit","amount":"1000.00"},
        ]);
        if m == 1 {
            rows.as_array_mut().ok_or("rows")?.push(
                json!({"source_id":"bank","account_id":bank,"side":"debit","amount":"50.00"}),
            );
        }
        retain(
            db,
            services,
            s,
            company,
            &format!("MONTH-2026-{m:02}"),
            (&from, &to),
            rows,
        )
        .await?;
    }
    Ok(())
}

pub async fn prove(db: &Db, services: &Services, s: &Scope, company: i64) -> Outcome<()> {
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let trial = ledger
        .trial_balance(s, company, date(2026, 7, 1), None)
        .await?;
    let (expense, sales) = (
        account(&ledger, s, company, "5000").await?,
        account(&ledger, s, company, "4000").await?,
    );
    // Nothing retained: six source months to bridge, both accounts differ by
    // their whole opening balance.
    let empty = read(db, services, s, company).await?;
    assert_eq!(empty["state"], "ready", "{empty}");
    assert_eq!(empty["conversion_date"], "2026-07-01");
    assert_eq!(empty["months"].as_array().map(Vec::len), Some(6));
    assert_eq!(empty["missing_months"], 6);
    assert_eq!(empty["differences"], 2);
    assert_eq!(empty["agrees"], false);
    assert_eq!(empty["accounts"][0]["code"], "4000");
    assert_eq!(empty["accounts"][0]["opening"], "-5500.00");
    assert_eq!(empty["accounts"][0]["difference"], "5500.00");
    assert_eq!(empty["accounts"][1]["code"], "5000");
    assert_eq!(empty["accounts"][1]["difference"], "-2000.00");

    seed(db, services, s, company).await?;
    let (from, to) = month(6);
    retain(
        db,
        services,
        s,
        company,
        "MONTH-2026-06",
        (&from, &to),
        json!([
            {"source_id":"cost","account_id":expense,"side":"debit","amount":"500.00"},
            {"source_id":"sales","account_id":sales,"side":"credit","amount":"500.00"},
        ]),
    )
    .await?;
    let agreed = read(db, services, s, company).await?;
    assert_eq!(agreed["missing_months"], 0, "{agreed}");
    assert_eq!(agreed["repeated_months"], 0);
    assert_eq!(agreed["differences"], 0);
    assert_eq!(agreed["agrees"], true);
    assert_eq!(agreed["accounts"][1]["source"], "2000.00");
    assert_eq!(agreed["totals"]["source"], "-3500.00");
    assert_eq!(agreed["totals"]["difference"], "0.00");
    // The bank row is listed apart, never added to an income or expense line.
    assert_eq!(agreed["not_bridged"][0]["code"], "1000");
    assert_eq!(agreed["not_bridged"][0]["source"], "50.00");

    // A second June schedule is shown as a repeated month and a difference,
    // never netted against the first.
    retain(
        db,
        services,
        s,
        company,
        "MONTH-2026-06-AGAIN",
        (&from, &to),
        json!([{"source_id":"cost","account_id":expense,"side":"debit","amount":"10.00"}]),
    )
    .await?;
    let repeated = read(db, services, s, company).await?;
    assert_eq!(repeated["repeated_months"], 1, "{repeated}");
    assert_eq!(repeated["months"][5]["status"], "more_than_one");
    assert_eq!(
        repeated["months"][5]["schedules"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(repeated["accounts"][1]["difference"], "10.00");
    assert_eq!(repeated["agrees"], false);

    // Reading the bridge and retaining schedules posted nothing.
    assert_eq!(
        trial,
        ledger
            .trial_balance(s, company, date(2026, 7, 1), None)
            .await?
    );
    println!("Source-period bridge proof: missing, agreeing and repeated months, a balance sheet row left out and no postings passed.");
    Ok(())
}
