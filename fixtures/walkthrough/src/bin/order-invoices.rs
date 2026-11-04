//! Real Commerce contracts -> durable events -> Receivables, on a fresh DB.
//! Mount Commerce for the full proof; `absent` proves Finance alone boots.
use bilibi_agent_api::{Handler, Registry};
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Db, MutationBoundary};
use bilibi_finance_receivables::{DocumentQuery, Page, Receivables};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::Services;
use bilibi_queue::{Config, Executor};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::time::Duration;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn write(
    db: &Db,
    services: &Services,
    scope: &Scope,
    key: &str,
    input: Value,
) -> Result<Value> {
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let Handler::SafeWrite(handler) = registry
        .operations
        .get(key)
        .ok_or("Commerce must be mounted for this proof")?
        .definition
        .handler
    else {
        return Err("expected write".into());
    };
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            scope.mutation_context("walkthrough.order-invoices"),
        )
        .await?;
    handler.preview(tx.sql(), scope, &input).await?;
    let value = handler.commit(&mut tx, scope, &input).await?.envelope(key);
    tx.commit().await?;
    Ok(value["result"].clone())
}
async fn deliver(db: &Db, services: &Services, admin: &Scope) -> Result<()> {
    let events = services.get::<bilibi_base::events::Events>(bilibi_base::events::SERVICE)?;
    let executor = Executor::from_services(
        services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let c = db.get().await?;
            let pending = events.deliveries(&**c, admin, 1, true, 500).await?;
            if !pending
                .iter()
                .any(|d| d.subscriber == "finance/receivables.sales_orders")
            {
                return Ok::<_, bilibi_database::Error>(());
            }
            drop(c);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    result??;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    let installed = install(&db, &services).await?;
    let admin = sign_in(&db, &services, 1, 4).await?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let viewer = sign_in(&db, &services, 1, 2).await?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    if std::env::args().any(|a| a == "absent") {
        assert!(ar.sales_orders(&manager, 1, 0, 100).await?.is_empty());
        println!("PASS Finance boots and reads its empty order copy without Commerce");
        return Ok(());
    }
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .unwrap()
            .id
    };
    ar.configure(
        &manager,
        1,
        &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")}),
    )
    .await?;
    ar.create_series(&manager,1,&json!({"code":"ORDER","name":"Sales invoices","kind":"invoice","prefix":"SI-","number_width":4})).await?;
    ar.create_tax_code(
        &manager,
        1,
        &json!({"code":"ZERO","name":"Zero rate","rate":"0","account_id":account("2100")}),
    )
    .await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    for key in [
        "commerce.sales.view",
        "commerce.sales.manage",
        "commerce.sales.confirm",
    ] {
        grants
            .set_direct(&admin, manager.user(), key, Some(true))
            .await?;
    }
    grants
        .set_system(
            &admin,
            "domain-events",
            "events.delivery.deliver",
            Some(true),
        )
        .await?;
    // A new customer, two parts and a tax snapshot, all from real Commerce.
    let input = |customer: &str, tax: &str| json!({"company_id":1,"customer_kind":"company","customer_code":customer,"currency_code":"MYR","lines":[{"description":"First service","unit":"hour","quantity":"2","unit_price":"10.25","tax_code":tax,"tax_rate":"0"},{"description":"Second service","unit":"hour","quantity":"1","unit_price":"4.50","tax_code":tax,"tax_rate":"0"}]});
    let first = write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.create",
        input("company_010", "ZERO"),
    )
    .await?;
    let order = first["record"]["id"].as_i64().unwrap();
    write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.transition",
        json!({"company_id":1,"id":order,"revision":1,"status":"confirmed"}),
    )
    .await?;
    deliver(&db, &services, &admin).await?;
    let copied = ar.sales_orders(&manager, 1, 0, 100).await?;
    assert_eq!(copied.len(), 1);
    let source = copied[0].id;
    assert!(ar
        .invoice_sales_order(&viewer, 1, source, None)
        .await
        .is_err());
    assert!(ar
        .invoice_sales_order(&manager, 2, source, None)
        .await
        .is_err());
    let invoice = ar.invoice_sales_order(&manager, 1, source, None).await?;
    assert_eq!(invoice.lines.len(), 2);
    assert_eq!(invoice.total.shown(), "25.00");
    assert_eq!(invoice.status, "draft");
    let customer = ar.get_customer(&manager, 1, invoice.customer_id).await?;
    assert_eq!(customer.code, "company_010");
    let read = ar.get_document(&manager, 1, invoice.id).await?;
    assert_eq!(read.sales_order.as_ref().unwrap().order_id, order);
    assert_eq!(
        ar.invoice_sales_order(&manager, 1, source, None).await?.id,
        invoice.id
    );
    let (a, b) = tokio::join!(
        ar.invoice_sales_order(&manager, 1, source, None),
        ar.invoice_sales_order(&manager, 1, source, None)
    );
    assert_eq!(a?.id, b?.id);
    let events = services.get::<bilibi_base::events::Events>(bilibi_base::events::SERVICE)?;
    events
        .replay(
            &admin,
            "finance/receivables.sales_orders",
            bilibi_base::events::ReplayFrom::Event(1),
            1000,
        )
        .await?;
    deliver(&db, &services, &admin).await?;
    assert_eq!(
        ar.invoice_sales_order(&manager, 1, source, None).await?.id,
        invoice.id
    );
    ar.discard_draft(&manager, 1, invoice.id).await?;
    assert_eq!(
        ar.invoice_sales_order(&manager, 1, source, None).await?.id,
        invoice.id
    );
    println!("PASS two event parts make one invoice, new customer, exact lines/tax/totals, reach, race, replay and discard idempotency");
    // A missing tax code refuses the whole write: no half-created customer.
    let waiting = write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.create",
        input("company_011", "MISSING"),
    )
    .await?;
    let waiting_id = waiting["record"]["id"].as_i64().unwrap();
    write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.transition",
        json!({"company_id":1,"id":waiting_id,"revision":1,"status":"confirmed"}),
    )
    .await?;
    deliver(&db, &services, &admin).await?;
    let waiting = ar.sales_orders(&manager, 1, source, 100).await?.remove(0);
    assert!(ar
        .invoice_sales_order(&manager, 1, waiting.id, None)
        .await
        .is_err());
    assert!(!ar
        .list_customers(&manager, 1)
        .await?
        .iter()
        .any(|c| c.code == "company_011"));
    // Customer creation permission is checked independently of drafting.
    grants
        .set_direct(
            &admin,
            manager.user(),
            bilibi_finance_receivables::MANAGE_CUSTOMERS,
            Some(false),
        )
        .await?;
    ar.create_tax_code(
        &manager,
        1,
        &json!({"code":"MISSING","name":"Order zero rate","rate":"0","account_id":account("2100")}),
    )
    .await?;
    assert!(ar
        .invoice_sales_order(&manager, 1, waiting.id, None)
        .await
        .is_err());
    grants
        .set_direct(
            &admin,
            manager.user(),
            bilibi_finance_receivables::MANAGE_CUSTOMERS,
            None,
        )
        .await?;
    let prepared = write(
        &db,
        &services,
        &manager,
        "finance.receivables.sales_order.invoice",
        json!({"company_id":1,"order_id":waiting.id}),
    )
    .await?;
    let second = prepared["record"]["id"].as_i64().unwrap();
    // Cancellation is retained and blocks issuance without touching the draft.
    write(&db,&services,&manager,"commerce.sales.order.transition",json!({"company_id":1,"id":waiting_id,"revision":2,"status":"cancelled","reason":"Customer cancelled"})).await?;
    deliver(&db, &services, &admin).await?;
    assert!(ar.issue_document(&manager, 1, second).await.is_err());
    assert_eq!(ar.get_document(&manager, 1, second).await?.status, "draft");
    let docs = ar
        .list_documents(
            &manager,
            1,
            &DocumentQuery {
                page: Page {
                    limit: Some(100),
                    after: None,
                },
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(docs.len(), 2);
    for dataset in ["sales_order", "sales_order_part"] {
        assert!(!ar
            .evidence_page(&manager, 1, dataset, Some(100), None)
            .await?
            .rows
            .is_empty());
    }
    println!("PASS missing tax and customer permission leave orders waiting; shared contract retries atomically; cancellation blocks issue; source evidence exported");
    // A refusal after customer composition still rolls back the whole write.
    let mut too_large = input("company_016", "ZERO");
    too_large["lines"][0]["quantity"] = json!("999999999");
    too_large["lines"][0]["unit_price"] = json!("999999999");
    let large = write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.create",
        too_large,
    )
    .await?;
    write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.transition",
        json!({"company_id":1,"id":large["record"]["id"],"revision":1,"status":"confirmed"}),
    )
    .await?;
    deliver(&db, &services, &admin).await?;
    let large_source = ar
        .sales_orders(&manager, 1, 0, 100)
        .await?
        .into_iter()
        .find(|o| Some(o.order_id) == large["record"]["id"].as_i64())
        .unwrap();
    assert!(ar
        .invoice_sales_order(&manager, 1, large_source.id, None)
        .await
        .is_err());
    assert!(!ar
        .list_customers(&manager, 1)
        .await?
        .iter()
        .any(|c| c.code == "company_016"));
    println!(
        "PASS amount refusal rolls back a newly composed customer and relationship with the draft"
    );
    // The unattended path uses the Finance worker's own grants, not the seller.
    ar.create_tax_code(
        &manager,
        1,
        &json!({"code":"STANDARD","name":"Six percent","rate":"6","account_id":account("2100")}),
    )
    .await?;
    let mut automatic_input = input("company_014", "STANDARD");
    for line in automatic_input["lines"].as_array_mut().unwrap() {
        line["tax_rate"] = json!("6");
    }
    let automatic = write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.create",
        automatic_input,
    )
    .await?;
    write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.transition",
        json!({"company_id":1,"id":automatic["record"]["id"],"revision":1,"status":"confirmed"}),
    )
    .await?;
    deliver(&db, &services, &admin).await?;
    let automatic_id = automatic["record"]["id"].as_i64().unwrap();
    for key in ["admin.system.schedule.execute", "admin.tenancy.tenant.list"] {
        grants
            .set_system(&admin, "scheduler", key, Some(true))
            .await?;
    }
    for key in [
        "finance.receivables.documents.manage",
        "finance.receivables.customers.manage",
        "admin.company.view",
        "admin.company.update",
        "admin.person.view",
    ] {
        grants
            .set_system(
                &admin,
                "finance-receivables-order-invoices",
                key,
                Some(true),
            )
            .await?;
    }
    let schedule = services.get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    let mut client = db.get().await?;
    schedule
        .review(
            &mut client,
            &admin,
            "finance/receivables-order-invoices",
            true,
        )
        .await?;
    schedule
        .run_now(
            &mut client,
            &admin,
            "finance/receivables-order-invoices",
            jiff::Timestamp::now(),
        )
        .await?
        .map_err(|e| format!("schedule refused: {e:?}"))?;
    drop(client);
    let executor = Executor::from_services(
        &services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let prepared = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let rows = ar.sales_orders(&manager, 1, 0, 100).await?;
            if let Some(document) = rows
                .iter()
                .find(|o| o.order_id == automatic_id)
                .and_then(|o| o.document_id)
            {
                return Ok::<_, bilibi_finance_receivables::ReceivableError>(document);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    let automatic_invoice = ar.get_document(&manager, 1, prepared??).await?;
    assert_eq!(automatic_invoice.total.shown(), "26.50");
    assert_eq!(automatic_invoice.tax_total.shown(), "1.50");
    println!("PASS enabled each-tenant scheduled work creates a taxed draft under the Finance worker's own grants");
    // Leave another unready order for browser proof.
    let browser = write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.create",
        input("company_013", "UNCONFIGURED"),
    )
    .await?;
    write(
        &db,
        &services,
        &manager,
        "commerce.sales.order.transition",
        json!({"company_id":1,"id":browser["record"]["id"],"revision":1,"status":"confirmed"}),
    )
    .await?;
    deliver(&db, &services, &admin).await?;
    println!(
        "Browser proof: an order remains waiting for tax code UNCONFIGURED on the Invoices page"
    );
    Ok(())
}
