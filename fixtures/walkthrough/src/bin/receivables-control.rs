//! Fixture-only administrative changes for the live H2 two-session proof.
//! Never an operation or production seed; only the disposable fixture is valid.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_walkthrough_fixture::sign_in;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err(Error::Invalid.into());
    }
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let client = db.get().await?;
    if client
        .query_opt(
            "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=1 AND i.email='manager@example.test'",
            &[],
        )
        .await?
        .is_none()
    {
        return Err(Error::Invalid.into());
    }
    drop(client);
    let modules = bilibi_module_registry::discover()?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    let admin = sign_in(&db, &services, 1, 4).await?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [action, cap]
            if matches!(action.as_str(), "allow" | "deny")
                && [
                    bilibi_finance_receivables::VIEW,
                    bilibi_finance_receivables::ISSUE,
                    bilibi_finance_ledger::VIEW,
                ]
                .contains(&cap.as_str()) =>
        {
            services
                .get::<Grants>("base/authz.grants")?
                .set_direct(&admin, 1, cap, Some(action == "allow"))
                .await?;
        }
        [action, company] if action == "archive" && matches!(company.as_str(), "1" | "2") => {
            let service =
                services.get::<bilibi_company::CompanyService>("core/company.companies")?;
            service
                .archive_company(
                    &admin,
                    company.parse()?,
                    Some("Disposable H2 archival proof"),
                )
                .await?;
        }
        _ => return Err(Error::Invalid.into()),
    }
    println!("Disposable administrative change applied through its facade.");
    Ok(())
}
