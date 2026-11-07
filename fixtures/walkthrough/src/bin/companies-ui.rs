//! Browser fixture for the pages that act on the session's current company:
//! the Finance setup page, Company books, and the Fiscal years company filter.
//! Run with DATABASE_URL, APP_ORIGIN, BIND_ADDR and BILIBI_SETTINGS_KEY against
//! a disposable database. Identity fixture SQL only; every Finance record is
//! created by its facade.
//!
//! Tenant 1's companies (ids 1 to 62) hold four states: Company 001 has an
//! open book with a chart, the install's first fiscal year and payables set up; Company 002 has an open JPY book (no
//! decimal places) whose first fiscal year is a short six months; Company 003
//! has an open USD book and no fiscal year; every other company has no book.
//!
//! Sign in, with the walkthrough password, as `manager@example.test` (works
//! in every company of the tenant, affiliated with Company 001),
//! `contractor@example.test` (works only in Company 002) or
//! `viewer@example.test` (no company at all).
use bilibi_database::pool;
use bilibi_finance_ledger::{Ledger, NewAccount, NewBook, NewFiscalYear};
use bilibi_finance_payables::{Payables, Settings, SettingsInput};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use jiff::civil::date;
use serde_json::json;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
// The company-only accountant also manages periods, to show one company's
// fiscal years being opened and closed without reach to another.
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY, "walkthrough.fixture.finance.accountant", bilibi_module_registry::Provenance { module: "finance/ledger", source: file!(), line: line!() }, &bilibi_authz::VocabularyContribution {
    domains: &[], verbs: &[], capabilities: &[], system_principals: &[],
    roles: &[bilibi_authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &["admin.currency.view", "admin.currency.manage"] }, bilibi_authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &["finance.ledger.periods.manage", "finance.ledger.entries.post"] }],
}) }

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    for company in 3..=5 {
        bilibi_finance_walkthrough_fixture::give_desk(&db, 1, 1, company).await?;
    }
    let manager = sign_in(&db, services, 1, 1).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let payables = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let year = jiff::Zoned::now().year();
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::seeds::run(&db, &modules, services.catalog()?, services).await?;
    let currencies = services.get::<bilibi_currency::Currencies>(bilibi_currency::SERVICE)?;
    let boundary = services.get::<bilibi_database::MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("finance.walkthrough.company-currencies"),
        )
        .await?;
    // The currencies this walkthrough uses, so the controls list them first.
    for code in ["MYR", "JPY", "USD", "SGD"] {
        currencies
            .check(&mut tx, &manager, code)
            .await?
            .map_err(|e| format!("currency: {e:?}"))?;
    }
    currencies
        .set_base(&mut tx, &manager, Some("MYR"), None)
        .await?
        .map_err(|e| format!("base: {e:?}"))?;
    currencies
        .set_base(
            &mut tx,
            &manager,
            Some("SGD"),
            Some(bilibi_party_ids::OrganisationId::new(4)?),
        )
        .await?
        .map_err(|e| format!("base: {e:?}"))?;
    tx.commit().await?;
    drop(client);

    // `install` opened Company 001's book, chart and first fiscal year.
    if matches!(
        ledger.get_book(&manager, 2).await,
        Err(bilibi_finance_ledger::LedgerError::BookNotOpen)
    ) {
        for (company, currency, units) in [(2, "JPY", 0), (3, "USD", 2)] {
            ledger
                .open_book(
                    &manager,
                    company,
                    &NewBook {
                        currency_code: currency.into(),
                        minor_units: units,
                    },
                )
                .await?;
        }
        // Company 002 gets the walkthrough chart; Company 003 stays bare.
        for (code, name, kind, retained) in bilibi_finance_walkthrough_fixture::CHART {
            let mut account = NewAccount::new(
                code,
                if code == "1000" {
                    "Operating cash"
                } else {
                    name
                },
                kind,
            );
            account.retained_earnings = retained;
            ledger.create_account(&manager, 2, &account).await?;
        }
        // Company 002: a short first year of six months.
        ledger
            .open_fiscal_year(
                &manager,
                2,
                &NewFiscalYear {
                    start_date: Some(date(year - 1, 4, 1)),
                    end_date: Some(date(year - 1, 9, 30)),
                },
            )
            .await?;
        let period = ledger.list_fiscal_years(&manager, 2).await?[0].periods[0].id;
        ledger
            .close_period(&manager, 2, period, Some("Source period reconciled"))
            .await?;
        let payable = ledger
            .list_accounts(&manager, 1)
            .await?
            .into_iter()
            .find(|account| account.code == "2000")
            .map(|account| account.id);
        if let Some(account) = payable {
            payables
                .put_settings(
                    &manager,
                    1,
                    &SettingsInput {
                        settings: Settings {
                            payable_account_id: account,
                            quantity_tolerance: None,
                            price_tolerance: None,
                        },
                        reason: None,
                    },
                )
                .await?;
        }
    }
    prove_setup(&db, services).await?;
    if std::env::var_os("FINANCE_SETUP_PROOF_ONLY").is_some() {
        return Ok(());
    }
    println!(
        "Current-company fixture at {origin}: manager@example.test (every company), contractor@example.test (Company 002 only), viewer@example.test (no company); password bilibi-dev"
    );
    axum::serve(
        tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
        bilibi_web::router(app),
    )
    .await?;
    Ok(())
}

// Each assertion is an answer from the real contracts/facades, including
// refusals and the posted entry. Nothing reads Finance tables from the fixture.
async fn prove_setup(
    db: &bilibi_database::Db,
    services: &bilibi_module_registry::Services,
) -> Outcome<()> {
    use bilibi_finance_ledger::{JournalStatus, OpeningBalances, Reversal};
    use bilibi_finance_reports::{Reports, SetupState};
    let manager = sign_in(db, services, 1, 1).await?;
    let contractor = sign_in(db, services, 1, 7).await?;
    let other_tenant = sign_in(db, services, 2, 5).await?;
    let reviewer = sign_in(db, services, 1, 4).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let client = db.get().await?;
    for (company, code, origin) in [(3, "MYR", "tenant"), (4, "SGD", "company")] {
        let read = registry
            .call(
                &**client,
                &manager,
                "finance.ledger.book.currencies",
                &json!({"company_id":company}),
            )
            .await?;
        assert_eq!(read["result"]["proposed"]["code"], code);
        assert_eq!(read["result"]["proposed"]["source"], origin);
        println!("Company {company} currency proposal: {code} ({origin})");
    }
    // Changing the tenant default affects proposals only, never existing books.
    let currencies = services.get::<bilibi_currency::Currencies>(bilibi_currency::SERVICE)?;
    let boundary = services.get::<bilibi_database::MutationBoundary>("base/database.mutations")?;
    let mut changing = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut changing,
            manager.mutation_context("finance.walkthrough.default-change"),
        )
        .await?;
    currencies
        .set_base(&mut tx, &manager, Some("USD"), None)
        .await?
        .map_err(|e| format!("base: {e:?}"))?;
    tx.commit().await?;
    assert_eq!(ledger.get_book(&manager, 1).await?.currency_code, "MYR");
    assert_eq!(ledger.get_book(&manager, 2).await?.minor_units.get(), 0);
    assert_eq!(
        registry
            .call(
                &**client,
                &manager,
                "finance.ledger.book.currencies",
                &json!({"company_id":5})
            )
            .await?["result"]["proposed"]["code"],
        "USD"
    );
    assert_eq!(
        registry
            .call(
                &**client,
                &manager,
                "finance.ledger.book.currencies",
                &json!({"company_id":4})
            )
            .await?["result"]["proposed"]["code"],
        "SGD"
    );
    for scope in [&contractor, &other_tenant] {
        for key in [
            "finance.ledger.account.list",
            "finance.ledger.opening_balances.get",
            "finance.reports.setup.get",
        ] {
            assert!(registry
                .call(&**client, scope, key, &json!({"company_id":1}))
                .await
                .is_err());
        }
    }
    assert!(ledger.opening_progress(&contractor, 1).await.is_err());
    assert!(
        registry
            .call(
                &**client,
                &contractor,
                "finance.ledger.account.list",
                &json!({"company_id":2})
            )
            .await?["result"]["records"]
            .as_array()
            .unwrap()
            .len()
            >= 9
    );
    assert_eq!(
        ledger.list_accounts(&manager, 1).await?[0].code,
        ledger.list_accounts(&manager, 2).await?[0].code
    );
    let c1_account = ledger.list_accounts(&manager, 1).await?[0].id;
    assert!(ledger
        .update_account(
            &contractor,
            1,
            c1_account,
            &bilibi_finance_ledger::AccountChange {
                name: Some("Refused edit".into()),
                ..Default::default()
            }
        )
        .await
        .is_err());
    assert!(matches!(
        ledger
            .update_account(
                &contractor,
                2,
                c1_account,
                &bilibi_finance_ledger::AccountChange {
                    name: Some("Wrong company".into()),
                    ..Default::default()
                }
            )
            .await,
        Err(bilibi_finance_ledger::LedgerError::AccountNotFound)
    ));
    // Matching account codes identify separate records, without a mapping.
    assert_ne!(
        ledger.list_accounts(&manager, 1).await?[0].id,
        ledger.list_accounts(&manager, 2).await?[0].id
    );
    println!(
        "Company-only accountant and another tenant refused company 1 chart, conversion progress and setup; same chart codes remain separate records"
    );
    drop(client);
    if ledger.opening_progress(&manager, 1).await?.is_none() {
        let opening = |overview: bilibi_finance_reports::SetupOverview| {
            overview
                .areas
                .into_iter()
                .find(|a| a.id == "opening-balances")
                .unwrap()
        };
        assert_eq!(
            opening(reports.setup_overview(&manager, 1).await?).state,
            SetupState::NotSetUp
        );
        let mut source = OpeningBalances {
            conversion_date: date(2026, 7, 1),
            currency_code: "MYR".into(),
            source_reference: "SETUP-CLOSING".into(),
            csv: "source_id,account_code,debit,credit\ncash,1000,12000,\ncapital,3000,,12000\n"
                .into(),
            replaces_journal_id: None,
        };
        let original = ledger.submit_opening_balances(&manager, 1, &source).await?;
        assert_eq!(
            ledger.opening_progress(&manager, 1).await?.unwrap().status,
            JournalStatus::Submitted
        );
        assert!(opening(reports.setup_overview(&manager, 1).await?)
            .summary
            .contains("awaiting approval"));
        source.source_reference = "SETUP-CORRECTED".into();
        source.replaces_journal_id = Some(original.id);
        let corrected = ledger.submit_opening_balances(&manager, 1, &source).await?;
        assert_eq!(
            ledger
                .opening_progress(&manager, 1)
                .await?
                .unwrap()
                .journal_id,
            corrected.id
        );
        ledger
            .reject_journal(&reviewer, 1, corrected.id, "Reconcile source totals first")
            .await?;
        assert!(opening(reports.setup_overview(&manager, 1).await?)
            .summary
            .contains("rejected"));
        source.source_reference = "SETUP-RESUBMITTED".into();
        source.replaces_journal_id = None;
        let submitted = ledger.submit_opening_balances(&manager, 1, &source).await?;
        ledger
            .withdraw_journal(&manager, 1, submitted.id, None)
            .await?;
        assert!(opening(reports.setup_overview(&manager, 1).await?)
            .summary
            .contains("withdrawn"));
        source.source_reference = "SETUP-APPROVED".into();
        let submitted = ledger.submit_opening_balances(&manager, 1, &source).await?;
        ledger.approve_journal(&reviewer, 1, submitted.id).await?;
        let progress = ledger.opening_progress(&manager, 1).await?.unwrap();
        let client = db.get().await?;
        let read = registry
            .call(
                &**client,
                &manager,
                "finance.ledger.opening_balances.get",
                &json!({"company_id":1}),
            )
            .await?;
        assert_eq!(read["result"]["opening"]["journal_id"], progress.journal_id);
        assert_eq!(read["result"]["opening"]["status"], "approved");
        drop(client);
        assert_eq!(progress.status, JournalStatus::Approved);
        assert_eq!(
            opening(reports.setup_overview(&manager, 1).await?).state,
            SetupState::SetUp
        );
        ledger
            .reverse_entry(
                &manager,
                1,
                progress.entry_id.unwrap(),
                &Reversal {
                    entry_date: date(2026, 7, 1),
                    description: "Review revised source closing report".into(),
                    reference: None,
                },
            )
            .await?;
        assert!(ledger
            .opening_progress(&manager, 1)
            .await?
            .unwrap()
            .reversed_by_entry_id
            .is_some());
        assert_eq!(
            opening(reports.setup_overview(&manager, 1).await?).state,
            SetupState::Incomplete
        );
        assert!(opening(reports.setup_overview(&manager, 1).await?)
            .summary
            .contains("reversed"));
        println!(
            "Opening progress: none, waiting, replacement, rejection, withdrawal, posted and reversed all proved by the owning facade; conversion is optional and excluded from required totals"
        );
    }
    Ok(())
}
