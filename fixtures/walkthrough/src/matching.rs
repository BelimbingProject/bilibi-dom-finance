//! Running proof of Bilimbi.Finance.Payables' three-way match (NP2) through
//! the real facades, after `payables::run` configured Payables. Supply Chain
//! is a separate container this crate never names: without it mounted, the
//! order source is unavailable and a bill line cannot name an order line;
//! with it mounted, a line names only an order line Payables' own copy of the
//! source knows. The matching itself against goods received is shown with
//! Supply Chain mounted (README, Proof). The database's own guards close.
use crate::{sign_in, COMPANY};
use bilibi_database::{Db, Error};
use bilibi_finance_ledger::{Ledger, Money};
use bilibi_finance_payables::{
    DocumentLineInput, DocumentPageQuery, DocumentQuery, MatchApproval, NewDocument, Payables,
    PayablesError, MATCH_EXCEPTION_CODES, MATCH_STATUSES,
};
use bilibi_module_registry::Services;
use jiff::civil::date;
use rust_decimal::Decimal;
use std::fmt::Debug;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn refused<T: Debug>(
    label: &str,
    r: Result<T, PayablesError>,
    expected: impl FnOnce(&PayablesError) -> bool,
) -> Outcome<()> {
    match r {
        Err(e) if expected(&e) => {
            println!("{label}: refused ({e:?})");
            Ok(())
        }
        other => Err(format!("{label}: unexpected {other:?}").into()),
    }
}
/// The field refusal Bilimbi's changeset gives, on the bill's first line.
fn field(e: &PayablesError, name: &str, message: &str) -> bool {
    matches!(e, PayablesError::Invalid(errors)
        if errors.iter().any(|f| f.field == name && f.message == message && f.line == Some(1)))
}
async fn db_refuses(c: &tokio_postgres::Client, label: &str, sql: &str) -> Outcome<()> {
    let result = c.batch_execute(sql).await;
    let _ = c.batch_execute("ROLLBACK").await;
    match result {
        Err(e) => {
            println!(
                "{label}: refused ({})",
                e.as_db_error()
                    .map_or_else(|| e.to_string(), |d| d.message().into())
            );
            Ok(())
        }
        Ok(()) => Err(format!("{label}: database accepted forbidden write").into()),
    }
}

pub async fn run(db: &Db, services: &Services) -> Outcome<()> {
    println!("\n== Payables NP2: three-way match");
    let p = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let vendor = crate::vendors::vendor(
        db,
        services,
        &manager,
        COMPANY,
        "supplier-np2",
        "Matched supplier",
    )
    .await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let other = sign_in(db, services, 2, 5).await?;
    println!("match statuses {MATCH_STATUSES:?}; exception codes {MATCH_EXCEPTION_CODES:?}");
    let source = p.order_source_available();
    println!(
        "order source available: {source} (Supply Chain {}mounted)",
        if source { "" } else { "not " }
    );
    let posted = p
        .list_documents(
            &manager,
            COMPANY,
            &DocumentQuery {
                document_type: Some("bill".into()),
                status: Some("posted".into()),
                ..DocumentQuery::default()
            },
        )
        .await?;
    let bill = posted.first().ok_or(Error::NotFound)?;
    let units = services
        .get::<Ledger>(bilibi_finance_ledger::SERVICE)?
        .get_book(&manager, COMPANY)
        .await?
        .minor_units;
    let line = |order_line_id: Option<i64>, quantity: Option<&str>| DocumentLineInput {
        description: "Ordered goods".into(),
        account_id: bill.lines[0].account_id,
        amount: Money::parse("50.00", units).expect("fixture money"),
        tax_code_id: None,
        external_order_line_id: None,
        order_line_id,
        quantity: quantity.map(|q| q.parse::<Decimal>().expect("fixture quantity")),
    };
    let new = |number: &str, l: DocumentLineInput| NewDocument {
        vendor_id: Some(vendor),
        party_document_number: Some(number.into()),
        document_date: date(2026, 12, 20),
        due_date: None,
        description: None,
        lines: vec![l],
        tax_lines: None,
    };

    // Recording: an order line comes with the quantity billed against it, and
    // names a line the order source knows.
    let unknown = new("NP2-UNKNOWN", line(Some(900_001), Some("5")));
    if source {
        refused(
            "bill naming an order line the copy does not know",
            p.create_bill(&manager, COMPANY, &unknown).await,
            |e| {
                field(
                    e,
                    "order_line_id",
                    "is not an order line of this company in the order source",
                )
            },
        )?;
    } else {
        refused(
            "bill naming an order line with no order source mounted",
            p.create_bill(&manager, COMPANY, &unknown).await,
            |e| field(e, "order_line_id", "names no installed order source"),
        )?;
    }
    for (label, l, name, message) in [
        (
            "quantity without an order line",
            line(None, Some("5")),
            "order_line_id",
            "is needed with a quantity",
        ),
        (
            "order line without a quantity",
            line(Some(900_001), None),
            "quantity",
            "is needed with an order line",
        ),
        (
            "quantity finer than four places",
            line(Some(900_001), Some("1.00001")),
            "quantity",
            "has more than 4 decimal places",
        ),
        (
            "zero quantity",
            line(Some(900_001), Some("0")),
            "quantity",
            "must be greater than 0",
        ),
        (
            "order line id 0",
            line(Some(0), Some("1")),
            "order_line_id",
            "must be greater than 0",
        ),
    ] {
        refused(
            label,
            p.create_bill(&manager, COMPANY, &new("NP2-INVALID", l))
                .await,
            |e| field(e, name, message),
        )?;
    }
    refused(
        "debit note naming an order line its bill does not name",
        p.create_debit_note(
            &manager,
            COMPANY,
            bill.id,
            &new("NP2-NOTE", line(Some(900_001), Some("1"))),
        )
        .await,
        |e| {
            field(
                e,
                "order_line_id",
                "is not an order line of the corrected bill",
            )
        },
    )?;

    // Billed quantities are this module's own record, read with view.
    let billed = p.billed_quantities(&staff, COMPANY, &[900_001]).await?;
    println!("billed quantities for an order line nothing posted names: {billed:?}");
    assert!(billed.is_empty());
    refused(
        "billed quantities of order line 0",
        p.billed_quantities(&staff, COMPANY, &[0]).await,
        |e| matches!(e, PayablesError::InvalidOption),
    )?;
    refused(
        "billed quantities across tenants",
        p.billed_quantities(&other, COMPANY, &[900_001]).await,
        |e| {
            matches!(
                e,
                PayablesError::Unauthorized | PayablesError::CompanyNotFound
            )
        },
    )?;

    // Approving match exceptions asks its own capability and a held bill.
    let approval = MatchApproval {
        reason: "Price agreed by phone".into(),
        held_event_id: 1,
    };
    refused(
        "match approval without finance.payables.match.approve",
        p.approve_match_exception(&staff, COMPANY, bill.id, &approval)
            .await,
        |e| matches!(e, PayablesError::Unauthorized),
    )?;
    refused(
        "match approval of a posted bill",
        p.approve_match_exception(&manager, COMPANY, bill.id, &approval)
            .await,
        |e| matches!(e, PayablesError::NotHeld),
    )?;
    let held = p
        .page_documents(
            &manager,
            COMPANY,
            &DocumentPageQuery {
                status: Some("held".into()),
                ..DocumentPageQuery::default()
            },
        )
        .await?;
    println!("Held bills: {} of {}", held.documents.len(), held.total);
    assert_eq!(bill.match_status, None);
    assert!(bill.match_events.is_empty());

    // PostgreSQL's own guards, each in a transaction it rolls back.
    let account = bill.lines[0].account_id;
    let payable = bill.payable_account_id.ok_or(Error::NotFound)?;
    let raw = |number: &str| {
        format!(
            "WITH d AS (INSERT INTO finance_payables_documents(tenant_id,company_id,document_type,status,party_key,party_name,party_document_number,document_date,due_date,currency_code,total_net,total_tax,total,created_by_user_id) VALUES(1,1,'bill','draft','raw-np2','Raw party','{number}','2026-12-20','2026-12-20','MYR',50,0,50,1) RETURNING id) INSERT INTO finance_payables_document_lines(tenant_id,company_id,document_id,line_number,description,account_id,amount,order_line_id,quantity) SELECT 1,1,id,1,'Raw line',{account},50,900001,5 FROM d; UPDATE finance_payables_documents SET status='approved',approved_by_user_id=1 WHERE party_document_number='{number}'"
        )
    };
    let event = |number: &str, kind: &str| {
        format!(
            "WITH e AS (INSERT INTO finance_payables_match_events(tenant_id,company_id,document_id,event,actor_user_id) SELECT 1,1,id,'{kind}',1 FROM finance_payables_documents WHERE party_document_number='{number}' RETURNING id,document_id), l AS (INSERT INTO finance_payables_match_lines(tenant_id,company_id,event_id,document_id,line_number,order_line_id,billed_quantity,billed_unit_price,previously_billed_quantity) SELECT 1,1,id,document_id,1,900001,5,10,0 FROM e RETURNING id) INSERT INTO finance_payables_match_exceptions(tenant_id,company_id,match_line_id,code,detail) SELECT 1,1,id,'order_source_unavailable','no order source is installed to match against' FROM l"
        )
    };
    let post = |number: &str| {
        format!(
            "UPDATE finance_payables_documents SET status='posted',posted_by_user_id=1,payable_account_id={payable},entry_id=1,entry_number=1 WHERE party_document_number='{number}'"
        )
    };
    let c = db.get().await?;
    db_refuses(
        &c,
        "bill naming order lines posted without its match record",
        &format!("BEGIN; {}; {}", raw("RAW-NP2-A"), post("RAW-NP2-A")),
    )
    .await?;
    db_refuses(
        &c,
        "held bill posted over unapproved exceptions",
        &format!(
            "BEGIN; {}; {}; UPDATE finance_payables_documents SET status='held' WHERE party_document_number='RAW-NP2-B'; {}; {}",
            raw("RAW-NP2-B"),
            event("RAW-NP2-B", "held"),
            event("RAW-NP2-B", "posted"),
            post("RAW-NP2-B")
        ),
    )
    .await?;
    db_refuses(
        &c,
        "approval naming no hold",
        &format!("BEGIN; {}; INSERT INTO finance_payables_match_events(tenant_id,company_id,document_id,event,reason,held_event_id,actor_user_id) SELECT 1,1,id,'exception_approved','Because',NULL,1 FROM finance_payables_documents WHERE party_document_number='RAW-NP2-C'", raw("RAW-NP2-C")),
    )
    .await?;
    db_refuses(
        &c,
        "match event for a debit note",
        "BEGIN; INSERT INTO finance_payables_match_events(tenant_id,company_id,document_id,event,actor_user_id) SELECT 1,1,id,'held',1 FROM finance_payables_documents WHERE document_type='debit_note' LIMIT 1",
    )
    .await?;
    db_refuses(
        &c,
        "match event rewritten",
        &format!(
            "BEGIN; {}; {}; UPDATE finance_payables_match_events SET reason='Rewritten'",
            raw("RAW-NP2-D"),
            event("RAW-NP2-D", "held")
        ),
    )
    .await?;
    db_refuses(
        &c,
        "unknown exception code",
        &format!(
            "BEGIN; {}; {}",
            raw("RAW-NP2-E"),
            event("RAW-NP2-E", "held").replace("'order_source_unavailable'", "'made_up'")
        ),
    )
    .await?;
    db_refuses(
        &c,
        "order receipts truncated",
        "BEGIN; TRUNCATE finance_payables_order_receipts",
    )
    .await?;
    db_refuses(
        &c,
        "order-source copy truncated",
        "BEGIN; TRUNCATE finance_payables_order_lines CASCADE",
    )
    .await?;
    println!("Payables NP2 walkthrough complete");
    Ok(())
}
