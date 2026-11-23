//! Focused disposable proof and optional browser server for starter templates.
use bilibi_database::{pool, Error};
use bilibi_finance_walkthrough_fixture::{chart_templates, charts, install, without_step_up};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4373".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    without_step_up(&app.services)?;
    let serve_existing = std::env::args().any(|arg| arg == "--serve-existing");
    if !serve_existing {
        install(&db, &app.services).await?;
        charts::run(&db, &app.services).await?;
        chart_templates::run(&db, &app.services).await?;
    }
    if serve_existing || std::env::args().any(|arg| arg == "--serve") {
        let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        println!("Browser proof at {origin}; PID {}", std::process::id());
        axum::serve(listener, bilibi_web::staff_router(app)).await?;
    }
    Ok(())
}
