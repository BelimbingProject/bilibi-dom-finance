//! Serve the walkthrough database for Reports browser proof.
use bilibi_database::pool;
use bilibi_finance_walkthrough_fixture::install;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    install(&db, &app.services).await?;
    let root = std::env::var("FINANCE_UI_STORAGE")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let admin = bilibi_finance_walkthrough_fixture::sign_in(&db, &app.services, 1, 4).await?;
    bilibi_finance_walkthrough_fixture::attachments::configure_storage(
        &db,
        &app.services,
        &admin,
        &root,
    )
    .await?;
    if std::env::args().nth(1).as_deref() != Some("serve") {
        bilibi_finance_walkthrough_fixture::reports::earnings_split(&db, &app.services).await?;
    }
    let listener = tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?;
    println!("Reports UI listening at {origin}");
    axum::serve(listener, bilibi_web::staff_router(app)).await?;
    Ok(())
}
