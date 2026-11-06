//! Browser proof on an empty disposable database. Set DATABASE_URL,
//! APP_ORIGIN and BIND_ADDR. ADOPTER_EMPTY_BOOK=1 leaves the book unopened;
//! otherwise it prepares a draft bill and a bank account, using install defaults.
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{Ledger, Money, NewEntry, NewLine, PostOptions};
use bilibi_finance_payables::{DocumentLineInput, NewDocument, Payables, Settings, SettingsInput};
use bilibi_finance_reconciliation::{NewBankAccount, Reconciliation};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance};
use jiff::civil::date;
use serde_json::json;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.adopter.assets",Provenance {module:"finance/fixed_assets",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
    roles:&[bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_fixed_assets::CAPABILITIES}],
}) }
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    let bind = std::env::var("BIND_ADDR")?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_database::seeds::run(
        &db,
        &bilibi_module_registry::discover()?,
        app.services.catalog()?,
        &app.services,
    )
    .await?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    if std::env::var("ADOPTER_EMPTY_BOOK").as_deref() == Ok("1") {
        bilibi_finance_walkthrough_fixture::install_platform(&db).await?;
        app.services
            .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
            .reconcile()
            .await?;
    } else {
        let installed = install(&db, &app.services).await?;
        let manager = sign_in(&db, &app.services, 1, 1).await?;
        let account = |code: &str| {
            installed
                .accounts
                .iter()
                .find(|a| a.code == code)
                .map(|a| a.id)
                .ok_or(Error::NotFound)
        };
        let cash = account("1000")?;
        let capital = account("3000")?;
        let payable = account("2000")?;
        let expense = account("5000")?;
        let ledger = app.services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
        let money = |text: &str| Money::parse(text, installed.book.minor_units);
        ledger
            .post_entry(
                &manager,
                COMPANY,
                &NewEntry {
                    entry_date: date(2026, 9, 1),
                    description: "Owner's contribution".into(),
                    reference: None,
                    lines: vec![
                        NewLine::debit(cash, money("5000.00")?),
                        NewLine::credit(capital, money("5000.00")?),
                    ],
                },
                &PostOptions::default(),
            )
            .await?;
        let payables = app
            .services
            .get::<Payables>(bilibi_finance_payables::SERVICE)?;
        payables
            .put_settings(
                &manager,
                COMPANY,
                &SettingsInput {
                    settings: Settings {
                        payable_account_id: payable,
                        quantity_tolerance: None,
                        price_tolerance: None,
                    },
                    reason: Some("First-adopter walkthrough".into()),
                },
            )
            .await?;
        let bill = payables
            .create_bill(
                &manager,
                COMPANY,
                &NewDocument {
                    vendor_id: Some(
                        bilibi_finance_walkthrough_fixture::vendors::vendor(
                            &db,
                            &app.services,
                            &manager,
                            1,
                            "supplier-001",
                            "Example supplier",
                        )
                        .await?,
                    ),
                    party_document_number: Some("INV-2026-0042".into()),
                    document_date: date(2026, 9, 5),
                    due_date: Some(date(2026, 10, 5)),
                    description: Some("Paper and toner".into()),
                    lines: vec![DocumentLineInput {
                        description: "Paper and toner".into(),
                        account_id: expense,
                        amount: money("1000.00")?,
                        tax_code_id: None,
                        external_order_line_id: None,
                        order_line_id: None,
                        quantity: None,
                    }],
                    tax_lines: None,
                },
            )
            .await?;
        let reconciliation = app
            .services
            .get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
        let mapping = json!({"delimiter":",","header_rows":1,"date":"Date","date_format":"YYYY-MM-DD","description":"Description","amount":"Amount","amount_sign":"deposits_positive","decimal_separator":".","balance":"Balance"});
        let bank = reconciliation
            .create_bank_account(
                &manager,
                COMPANY,
                &NewBankAccount {
                    name: "Main bank account".into(),
                    ledger_account_id: cash,
                    charges_account_id: Some(account("6000")?),
                    interest_account_id: None,
                    match_window_days: Some(3),
                    active: Some(true),
                    reason: Some("First-adopter walkthrough".into()),
                    import_mapping: mapping,
                },
            )
            .await?;
        println!("Draft bill: {origin}/finance/payables/bills/{}; bank account: {origin}/finance/reconciliation/accounts/{}",bill.id,bank.id);
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Browser proof ready at {origin}");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
