//! NE5: disposable browser fixture and executable facade/page proof.
//! DATABASE_URL=... APP_ORIGIN=... BIND_ADDR=... cargo run -p
//! bilibi-finance-walkthrough-fixture --bin einvoice-ui [serve]
use bilibi_database::pool;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_einvoice_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://localhost:4320".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    bilibi_finance_walkthrough_fixture::install(&db, &app.services).await?;
    // Retain the real Queue executor for interactive submit/poll actions.
    let _stop = bilibi_finance_walkthrough_fixture::einvoice_ui::run(&db, &app.services).await?;
    if std::env::args().nth(1).as_deref() == Some("serve") {
        let listener = tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?;
        println!("NE5 browser fixture ready; manager@example.test / fixture password");
        axum::serve(listener, bilibi_web::staff_router(app)).await?;
    }
    Ok(())
}
