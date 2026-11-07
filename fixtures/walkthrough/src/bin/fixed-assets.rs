//! NX1 running proof against a fresh disposable PostgreSQL database. All
//! business records are created through the real facades, never seed SQL.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Db, Error};
use bilibi_finance_fixed_assets::{
    AcquisitionInput, Assumptions, FixedAssets, FixedAssetsError as F, Method, RevisionInput,
};
use bilibi_finance_ledger::{
    AccountChange, AccountType, Entry, Ledger, MinorUnits, Money, NewAccount, NewBook, NewEntry,
    NewFiscalYear, NewLine, PostOptions, Reversal,
};
use bilibi_finance_payables::{
    DocumentLineInput, NewDocument, NewTaxCode, Payables, Settings, SettingsInput, TaxCodeDetails,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_tenancy::Scope;
use jiff::civil::date;
use rust_decimal::Decimal;
use std::fmt::Debug;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.finance.fixed-assets",Provenance {module:"finance/fixed_assets",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
    roles:&[
        bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_fixed_assets::CAPABILITIES},
        bilibi_authz::RoleDefinition {code:"staff",name:"staff",description:"Walkthrough fixture role",grant_all:false,capabilities:&[bilibi_finance_fixed_assets::VIEW]},
    ],
}) }
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
fn refused<T: Debug>(
    label: &str,
    result: Result<T, F>,
    expected: impl FnOnce(&F) -> bool,
) -> Outcome<()> {
    match result {
        Err(e) if expected(&e) => {
            println!("{label}: refused ({e:?})");
            Ok(())
        }
        other => Err(format!("{label}: unexpected {other:?}").into()),
    }
}
fn money(s: &str) -> Money {
    Money::parse(s, MinorUnits::new(2).unwrap()).unwrap()
}
fn input(revision: i64) -> AcquisitionInput {
    AcquisitionInput {
        category_revision_id: revision,
        description: "Fixture asset".into(),
        in_service_date: date(2026, 1, 31),
        residual_value: None,
        method: None,
        life_months: None,
        annual_rate: None,
    }
}
async fn post(l: &Ledger, s: &Scope, cost: i64, credit: i64, amount: &str) -> Outcome<Entry> {
    Ok(l.post_entry(
        s,
        COMPANY,
        &NewEntry {
            entry_date: date(2026, 1, 17),
            description: "Fixture acquisition".into(),
            reference: None,
            lines: vec![
                NewLine::debit(cost, money(amount)),
                NewLine::credit(credit, money(amount)),
            ],
        },
        &PostOptions::default(),
    )
    .await?)
}
async fn db_refuses(
    c: &tokio_postgres::Client,
    label: &str,
    sql: &str,
    expected: &str,
) -> Outcome<()> {
    let result = c.batch_execute(&format!("BEGIN; {sql}; ROLLBACK")).await;
    c.batch_execute("ROLLBACK").await?;
    match result {
        Err(e)
            if e.as_db_error()
                .is_some_and(|e| e.message().contains(expected)) =>
        {
            println!("{label}: PostgreSQL refused ({expected})");
            Ok(())
        }
        other => Err(format!("{label}: unexpected {other:?}").into()),
    }
}
#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_module_registry::verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    install(&db, &services).await?;
    run(&db, &services).await?;
    println!("NX1: all acquisition, schedule, scope, concurrency, audit and SQL proofs passed");
    Ok(())
}
async fn run(db: &Db, services: &Services) -> Outcome<()> {
    let f = services.get::<FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let l = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let p = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let g = services.get::<Grants>("base/authz.grants")?;
    let manager = sign_in(db, services, 1, 1).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let chart = l.list_accounts(&manager, COMPANY).await?;
    let find = |code: &str| {
        chart
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let capital = find("3000")?;
    let cost = l
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1200", "Asset cost", AccountType::Asset),
        )
        .await?;
    let accumulated = l
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1290", "Accumulated depreciation", AccountType::Asset),
        )
        .await?;
    let defaults = RevisionInput {
        cost_account_id: cost.id,
        accumulated_account_id: accumulated.id,
        expense_account_id: find("5000")?,
        gain_account_id: find("4000")?,
        loss_account_id: find("5000")?,
        assumptions: Assumptions {
            method: Method::StraightLine,
            life_months: 2,
            annual_rate: Some(Decimal::from(24)),
        },
        reason: "Initial assumptions".into(),
    };
    refused(
        "staff cannot configure",
        f.create_category(&staff, COMPANY, "Denied", &defaults)
            .await,
        |e| matches!(e, F::Unauthorized),
    )?;
    let mut bad = defaults.clone();
    bad.reason = " ".into();
    refused(
        "reason required before SQL",
        f.create_category(&manager, COMPANY, "Invalid", &bad).await,
        |e| matches!(e,F::Invalid(v) if v[0].field=="reason"),
    )?;
    bad = defaults.clone();
    bad.accumulated_account_id = cost.id;
    refused(
        "distinct cost and accumulated",
        f.create_category(&manager, COMPANY, "Invalid", &bad).await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    bad = defaults.clone();
    bad.gain_account_id = cost.id;
    refused(
        "gain must be income",
        f.create_category(&manager, COMPANY, "Invalid", &bad).await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    let original = f
        .create_category(&manager, COMPANY, "  Equipment  ", &defaults)
        .await?;
    assert_eq!(original.name, "Equipment");
    assert_eq!(original.defaults.assumptions.annual_rate, None);
    refused(
        "duplicate trimmed name",
        f.create_category(&manager, COMPANY, "Equipment", &defaults)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    let source = post(&l, &manager, cost.id, capital, "100.01").await?;
    let mut a = input(original.id);
    refused(
        "staff cannot acquire",
        f.acquire_from_entry(&staff, COMPANY, source.id, 1, &a)
            .await,
        |e| matches!(e, F::Unauthorized),
    )?;
    a.in_service_date = date(2026, 1, 16);
    refused(
        "service cannot precede acquisition",
        f.acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    a.in_service_date = date(2026, 1, 31);
    a.residual_value = Some(Money::parse("0.001", MinorUnits::new(4).unwrap())?);
    refused(
        "residual book precision",
        f.acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    a.residual_value = Some(money("101"));
    refused(
        "residual cannot exceed cost",
        f.acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    a.residual_value = None;
    refused(
        "credit is not acquisition",
        f.acquire_from_entry(&manager, COMPANY, source.id, 2, &a)
            .await,
        |e| matches!(e, F::NotAcquisitionDebit),
    )?;
    refused(
        "missing line",
        f.acquire_from_entry(&manager, COMPANY, source.id, 7, &a)
            .await,
        |e| matches!(e, F::EntryLineNotFound),
    )?;
    for life in [0, 1201] {
        a.life_months = Some(life);
        refused(
            "useful life bounds",
            f.acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
                .await,
            |e| matches!(e, F::Invalid(v) if v[0].field == "life_months"),
        )?;
    }
    a.life_months = None;
    let acquired = f
        .acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
        .await?;
    assert_eq!(acquired.asset_number, 1);
    assert_eq!(acquired.cost, money("100.01"));
    assert_eq!(acquired.acquisition_date, date(2026, 1, 17));
    let detail = f.get_asset(&manager, COMPANY, acquired.id).await?;
    assert_eq!(detail.schedule.len(), 2);
    assert_eq!(detail.schedule[0].period, date(2026, 1, 1));
    assert_eq!(detail.schedule[0].amount, money("50.01"));
    assert_eq!(detail.schedule[1].amount, money("50.00"));
    assert_eq!(
        detail.scheduled_depreciation(date(2026, 1, 30)),
        Money::ZERO
    );
    assert_eq!(
        detail.scheduled_depreciation(date(2026, 1, 31)),
        money("50.01")
    );
    println!(
        "straight line 100.01/2, service Jan 31: 50.01,50.00; cumulative Jan 30=0, Jan 31=50.01"
    );
    refused(
        "whole source consumed once",
        f.acquire_from_entry(&manager, COMPANY, source.id, 1, &a)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    let mut revised = defaults.clone();
    revised.assumptions = Assumptions {
        method: Method::ReducingBalance,
        life_months: 3,
        annual_rate: Some(Decimal::from(24)),
    };
    revised.reason = "New defaults".into();
    for rate in [
        None,
        Some(Decimal::ZERO),
        Some(Decimal::from(101)),
        Some(Decimal::from_str_exact("24.00001")?),
    ] {
        let mut invalid_revision = revised.clone();
        invalid_revision.assumptions.annual_rate = rate;
        refused(
            "invalid reducing rate",
            f.revise_category(&manager, COMPANY, original.category_id, &invalid_revision)
                .await,
            |e| matches!(e, F::Invalid(_)),
        )?;
    }
    let revision = f
        .revise_category(&manager, COMPANY, original.category_id, &revised)
        .await?;
    let history = f.list_categories(&manager, COMPANY).await?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].revisions.len(), 2);
    assert_eq!(history[0].revision.id, revision.id);
    assert_eq!(
        f.get_asset(&manager, COMPANY, acquired.id)
            .await?
            .asset
            .assumptions
            .life_months,
        2
    );
    let rb_source = post(&l, &manager, cost.id, capital, "1200").await?;
    let mut rb = input(revision.id);
    rb.residual_value = Some(money("100"));
    let rb_asset = f
        .acquire_from_entry(&manager, COMPANY, rb_source.id, 1, &rb)
        .await?;
    let rb_detail = f.get_asset(&manager, COMPANY, rb_asset.id).await?;
    let amounts = rb_detail
        .schedule
        .iter()
        .map(|r| r.amount)
        .collect::<Vec<_>>();
    assert_eq!(amounts, vec![money("24"), money("23.52"), money("1052.48")]);
    assert_eq!(rb_detail.schedule[2].carrying_value, money("100"));
    println!(
        "reducing balance 1200, residual 100, 24%, 3 months: 24,23.52,1052.48; final carrying=100"
    );
    // Category validation happens when a revision is recorded. Historical
    // registration remains valid after the source cost account is deactivated.
    let historical_source = post(&l, &manager, cost.id, capital, "10").await?;
    // Ledger requires a zero balance before deactivation. Clear these three
    // fixture debits without reversing any of their acquisition sources.
    post(&l, &manager, capital, cost.id, "1310.01").await?;
    l.update_account(
        &manager,
        COMPANY,
        cost.id,
        &AccountChange {
            active: Some(false),
            ..Default::default()
        },
    )
    .await?;
    let mut old = input(original.id);
    old.life_months = Some(7);
    old.residual_value = Some(money("1"));
    let old_asset = f
        .acquire_from_entry(&manager, COMPANY, historical_source.id, 1, &old)
        .await?;
    let old_detail = f.get_asset(&manager, COMPANY, old_asset.id).await?;
    assert_eq!(old_detail.schedule.len(), 7);
    assert_eq!(old_detail.schedule[0].amount, money("1.29"));
    assert_eq!(old_detail.schedule[6].amount, money("1.26"));
    assert_eq!(old_detail.schedule[6].carrying_value, money("1"));
    refused(
        "new revision rejects inactive account",
        f.revise_category(&manager, COMPANY, original.category_id, &defaults)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    l.update_account(
        &manager,
        COMPANY,
        cost.id,
        &AccountChange {
            active: Some(true),
            ..Default::default()
        },
    )
    .await?;
    // Exact month boundaries and zero rows: cost fully covered by residual.
    let zero_source = post(&l, &manager, cost.id, capital, "10").await?;
    let mut zero = input(original.id);
    zero.in_service_date = date(2028, 2, 29);
    zero.residual_value = Some(money("10"));
    let zero_asset = f
        .acquire_from_entry(&manager, COMPANY, zero_source.id, 1, &zero)
        .await?;
    let zero_detail = f.get_asset(&manager, COMPANY, zero_asset.id).await?;
    assert_eq!(zero_detail.schedule.len(), 2);
    assert_eq!(zero_detail.schedule[0].period, date(2028, 2, 1));
    assert_eq!(zero_detail.schedule[1].period, date(2028, 3, 1));
    assert!(zero_detail
        .schedule
        .iter()
        .all(|r| r.amount == Money::ZERO && r.carrying_value == money("10")));
    let leap_source = post(&l, &manager, cost.id, capital, "9").await?;
    let mut leap = input(revision.id);
    leap.in_service_date = date(2028, 2, 29);
    leap.method = Some(Method::StraightLine);
    leap.life_months = Some(2);
    leap.residual_value = Some(money("1"));
    let leap_asset = f
        .acquire_from_entry(&manager, COMPANY, leap_source.id, 1, &leap)
        .await?;
    let leap_detail = f.get_asset(&manager, COMPANY, leap_asset.id).await?;
    assert_eq!(leap_detail.asset.assumptions.annual_rate, None);
    assert_eq!(
        leap_detail.scheduled_depreciation(date(2028, 2, 28)),
        Money::ZERO
    );
    assert_eq!(
        leap_detail.scheduled_depreciation(date(2028, 2, 29)),
        money("4")
    );
    // A high rate with almost-full residual caps before end of life, retaining
    // later zero rows. An override must not rewrite its category revision.
    let cap_source = post(&l, &manager, cost.id, capital, "100").await?;
    let mut cap = input(revision.id);
    cap.annual_rate = Some(Decimal::from(100));
    cap.residual_value = Some(money("99"));
    let cap_asset = f
        .acquire_from_entry(&manager, COMPANY, cap_source.id, 1, &cap)
        .await?;
    let cap_detail = f.get_asset(&manager, COMPANY, cap_asset.id).await?;
    assert_eq!(
        cap_detail
            .schedule
            .iter()
            .map(|r| r.amount)
            .collect::<Vec<_>>(),
        vec![money("1"), Money::ZERO, Money::ZERO]
    );
    assert_eq!(
        cap_detail.revision.defaults.assumptions.annual_rate,
        Some(Decimal::from(24))
    );
    println!(
        "historical revision after deactivation, residual cap, leap-month and zero rows: correct"
    );
    let reversed_source = post(&l, &manager, cost.id, capital, "20").await?;
    let reversal = l
        .reverse_entry(
            &manager,
            COMPANY,
            reversed_source.id,
            &Reversal {
                entry_date: date(2026, 1, 31),
                description: "Fixture reversed acquisition".into(),
                reference: None,
            },
        )
        .await?;
    refused(
        "reversed original",
        f.acquire_from_entry(&manager, COMPANY, reversed_source.id, 1, &a)
            .await,
        |e| matches!(e, F::AcquisitionReversed),
    )?;
    refused(
        "reversal entry",
        f.acquire_from_entry(&manager, COMPANY, reversal.id, 2, &a)
            .await,
        |e| matches!(e, F::AcquisitionReversed),
    )?;
    l.reverse_entry(
        &manager,
        COMPANY,
        source.id,
        &Reversal {
            entry_date: date(2026, 1, 31),
            description: "Correction after registration".into(),
            reference: None,
        },
    )
    .await?;
    assert!(l
        .get_entry(&manager, COMPANY, source.id)
        .await?
        .owner
        .is_none());
    assert_eq!(
        f.get_asset(&manager, COMPANY, acquired.id)
            .await?
            .asset
            .cost,
        money("100.01")
    );
    // Concurrent duplicate acquisition is serialized and returns a field refusal.
    let concurrent_source = post(&l, &manager, cost.id, capital, "17").await?;
    let (one, two) = tokio::join!(
        f.acquire_from_entry(&manager, COMPANY, concurrent_source.id, 1, &a),
        f.acquire_from_entry(&manager, COMPANY, concurrent_source.id, 1, &a)
    );
    assert!(matches!(
        (&one, &two),
        (Ok(_), Err(F::Invalid(_))) | (Err(F::Invalid(_)), Ok(_))
    ));
    println!("concurrent registration: one asset, one friendly duplicate refusal");
    // Payables provenance uses net line, not bill total. Approval is independent.
    p.put_settings(
        &manager,
        COMPANY,
        &SettingsInput {
            settings: Settings {
                payable_account_id: find("2000")?,
                quantity_tolerance: None,
                price_tolerance: None,
            },
            reason: Some("Fixture setup".into()),
        },
    )
    .await?;
    let tax = p
        .create_tax_code(
            &manager,
            COMPANY,
            &NewTaxCode {
                code: "ASSET-INPUT".into(),
                details: TaxCodeDetails {
                    name: "Fixture input tax".into(),
                    rate: "6".into(),
                    account_id: find("2100")?,
                    active: true,
                    reason: Some("Fixture tax".into()),
                },
            },
        )
        .await?;
    let draft = p
        .create_bill(
            &manager,
            COMPANY,
            &NewDocument {
                vendor_id: Some(
                    bilibi_finance_walkthrough_fixture::vendors::vendor(
                        db,
                        services,
                        &manager,
                        COMPANY,
                        "supplier",
                        "Fixture supplier",
                    )
                    .await?,
                ),
                party_document_number: Some("asset-bill".into()),
                document_date: date(2026, 1, 17),
                due_date: None,
                description: None,
                lines: vec![
                    DocumentLineInput {
                        description: "First cost".into(),
                        account_id: cost.id,
                        amount: money("333"),
                        tax_code_id: Some(tax.id),
                        external_order_line_id: None,
                        order_line_id: None,
                        quantity: None,
                    },
                    DocumentLineInput {
                        description: "Second cost".into(),
                        account_id: cost.id,
                        amount: money("27"),
                        tax_code_id: Some(tax.id),
                        external_order_line_id: None,
                        order_line_id: None,
                        quantity: None,
                    },
                ],
                tax_lines: None,
            },
        )
        .await?;
    refused(
        "draft bill",
        f.acquire_from_bill(&manager, COMPANY, draft.id, 1, &a)
            .await,
        |e| matches!(e, F::BillNotPosted),
    )?;
    p.approve_document(&admin, COMPANY, draft.id).await?;
    let bill = p.post_document(&manager, COMPANY, draft.id).await?;
    assert_eq!(bill.total_tax, money("21.60"));
    assert_eq!(bill.total, money("381.60"));
    let from_bill = f
        .acquire_from_bill(&manager, COMPANY, bill.id, 2, &a)
        .await?;
    assert_eq!(from_bill.cost, money("27"));
    assert_eq!(from_bill.bill_id, Some(bill.id));
    assert_eq!(from_bill.bill_line_number, Some(2));
    assert_eq!(from_bill.entry_line_number, Some(2));
    assert_eq!(
        l.get_entry(&manager, COMPANY, from_bill.entry_id)
            .await?
            .owner
            .unwrap()
            .module,
        "finance/payables"
    );
    refused(
        "bill-entry duplicate crossover",
        f.acquire_from_entry(&manager, COMPANY, from_bill.entry_id, 2, &a)
            .await,
        |e| matches!(e, F::Invalid(_)),
    )?;
    refused(
        "missing bill line",
        f.acquire_from_bill(&manager, COMPANY, bill.id, 3, &a).await,
        |e| matches!(e, F::BillLineNotFound),
    )?;
    // Provider capabilities stay authoritative inside the caller transaction.
    g.set_direct(&admin, 1, bilibi_finance_payables::VIEW, Some(false))
        .await?;
    refused(
        "revoked Payables view",
        f.acquire_from_bill(&manager, COMPANY, bill.id, 1, &a).await,
        |e| {
            matches!(
                e,
                F::Payables(bilibi_finance_payables::PayablesError::Unauthorized)
            )
        },
    )?;
    g.set_direct(&admin, 1, bilibi_finance_payables::VIEW, None)
        .await?;
    g.set_direct(&admin, 1, bilibi_finance_ledger::VIEW, Some(false))
        .await?;
    refused(
        "revoked Ledger view",
        f.acquire_from_entry(&manager, COMPANY, concurrent_source.id, 1, &a)
            .await,
        |e| {
            matches!(
                e,
                F::Ledger(bilibi_finance_ledger::LedgerError::Unauthorized)
            )
        },
    )?;
    g.set_direct(&admin, 1, bilibi_finance_ledger::VIEW, None)
        .await?;
    g.set_direct(&admin, 1, bilibi_finance_fixed_assets::ACQUIRE, Some(false))
        .await?;
    refused(
        "revoked acquisition capability",
        f.acquire_from_bill(&manager, COMPANY, bill.id, 1, &a).await,
        |e| matches!(e, F::Unauthorized),
    )?;
    g.set_direct(&admin, 1, bilibi_finance_fixed_assets::ACQUIRE, None)
        .await?;
    refused(
        "foreign tenant",
        f.get_asset(&other, COMPANY, acquired.id).await,
        |e| matches!(e, F::CompanyNotFound | F::Unauthorized),
    )?;
    refused(
        "source belongs to another company",
        f.acquire_from_entry(&admin, 2, source.id, 1, &a).await,
        |e| {
            matches!(
                e,
                F::Ledger(bilibi_finance_ledger::LedgerError::EntryNotFound)
            )
        },
    )?;
    services
        .get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?
        .archive_company(&admin, 3, Some("Disposable NX1 archival proof"))
        .await?;
    refused(
        "archived company",
        f.create_category(&admin, 3, "Archived", &defaults).await,
        |e| matches!(e, F::CompanyArchived),
    )?;
    // Every owned dataset is paged, scoped and exact; no sibling SQL export.
    for dataset in f.evidence_datasets() {
        let mut after = None;
        let mut count = 0;
        loop {
            let page = f
                .evidence_page(&manager, COMPANY, dataset, Some(1), after)
                .await?;
            assert!(page.rows.len() <= 1);
            for row in &page.rows {
                assert_eq!(row["tenant_id"], 1);
                assert_eq!(row["company_id"], COMPANY);
                assert_eq!(row["recorded_by_user_id"], 1);
                count += 1;
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        if matches!(dataset, "asset" | "category" | "category_revision") {
            assert!(count > 0);
        } else {
            // This acquisition-only proof has no runs or lifecycle events.
            assert_eq!(count, 0);
        }
        println!("evidence {dataset}: {count} ID-ordered rows");
    }
    refused(
        "evidence bounded",
        f.evidence_page(&manager, COMPANY, "asset", Some(501), None)
            .await,
        |e| matches!(e, F::InvalidOption),
    )?;
    four_place_proof(&f, &l, &admin, &defaults).await?;
    let c = db.get().await?;
    let audit: i64 = c
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'finance.fixed-assets.%'",
            &[],
        )
        .await?
        .get(0);
    let rows:i64=c.query_one("SELECT (SELECT count(*) FROM finance_fixed_assets_assets)+(SELECT count(*) FROM finance_fixed_assets_categories)+(SELECT count(*) FROM finance_fixed_assets_category_revisions)",&[]).await?.get(0);
    assert_eq!(audit, rows);
    for (table, kind) in [
        ("categories", "category"),
        ("category_revisions", "category_revision"),
        ("assets", "asset"),
    ] {
        let mismatches: i64 = c.query_one(&format!("SELECT count(*) FROM finance_fixed_assets_{table} r WHERE (SELECT count(*) FROM base_audit_mutations a WHERE a.auditable_type=$1 AND a.auditable_id=r.id::text AND a.tenant_id=r.tenant_id AND a.new_values->>'company_id'=r.company_id::text)<>1"), &[&format!("finance.fixed-assets.{kind}")]).await?.get(0);
        assert_eq!(mismatches, 0);
    }
    println!("audit: all {rows} affected rows captured; refusals created no rows");
    for table in ["categories", "category_revisions", "assets"] {
        for sql in [
            format!("UPDATE finance_fixed_assets_{table} SET recorded_by_user_id=1"),
            format!("DELETE FROM finance_fixed_assets_{table}"),
            format!("TRUNCATE finance_fixed_assets_{table} CASCADE"),
        ] {
            db_refuses(
                &c,
                "permanent retention",
                &sql,
                "fixed assets retain every record permanently",
            )
            .await?;
        }
    }
    db_refuses(&c,"SQL duplicate source",&format!("INSERT INTO finance_fixed_assets_assets SELECT (jsonb_populate_record(NULL::finance_fixed_assets_assets,(SELECT to_jsonb(a) || jsonb_build_object('id',99999) FROM finance_fixed_assets_assets a WHERE id={}))).*",acquired.id),"duplicate key").await?;
    db_refuses(&c,"SQL null reducing rate",&format!("INSERT INTO finance_fixed_assets_category_revisions SELECT (jsonb_populate_record(NULL::finance_fixed_assets_category_revisions,(SELECT to_jsonb(r) || jsonb_build_object('id',99999,'annual_rate',NULL) FROM finance_fixed_assets_category_revisions r WHERE id={}))).*",revision.id),"check constraint").await?;
    for (field, value) in [("bill_id", 1), ("bill_line_number", 1)] {
        db_refuses(&c,"SQL partial bill provenance",&format!("INSERT INTO finance_fixed_assets_assets SELECT (jsonb_populate_record(NULL::finance_fixed_assets_assets,(SELECT to_jsonb(a) || jsonb_build_object('id',99999,'{field}',{value}) FROM finance_fixed_assets_assets a WHERE id={}))).*",acquired.id),"check constraint").await?;
    }
    db_refuses(&c,"SQL cross-company revision",&format!("INSERT INTO finance_fixed_assets_assets SELECT (jsonb_populate_record(NULL::finance_fixed_assets_assets,(SELECT to_jsonb(a) || jsonb_build_object('id',99999,'company_id',2) FROM finance_fixed_assets_assets a WHERE id={}))).*",acquired.id),"foreign key constraint").await?;
    Ok(())
}

async fn four_place_proof(
    f: &FixedAssets,
    l: &Ledger,
    s: &Scope,
    defaults: &RevisionInput,
) -> Outcome<()> {
    let book = l
        .open_book(
            s,
            2,
            &NewBook {
                currency_code: "MYR".into(),
                minor_units: 4,
            },
        )
        .await?;
    l.open_fiscal_year(
        s,
        2,
        &NewFiscalYear {
            start_date: Some(date(2026, 1, 1)),
            end_date: None,
        },
    )
    .await?;
    let mut accounts = Vec::new();
    for (code, kind) in [
        ("COST", AccountType::Asset),
        ("ACC", AccountType::Asset),
        ("EXP", AccountType::Expense),
        ("GAIN", AccountType::Income),
        ("CAP", AccountType::Equity),
    ] {
        accounts.push(
            l.create_account(s, 2, &NewAccount::new(code, code, kind))
                .await?
                .id,
        );
    }
    let mut revision = defaults.clone();
    revision.cost_account_id = accounts[0];
    revision.accumulated_account_id = accounts[1];
    revision.expense_account_id = accounts[2];
    revision.loss_account_id = accounts[2];
    revision.gain_account_id = accounts[3];
    revision.assumptions = Assumptions {
        method: Method::ReducingBalance,
        life_months: 3,
        annual_rate: Some(Decimal::from(24)),
    };
    let revision = f.create_category(s, 2, "Equipment", &revision).await?;
    let m = |v| Money::parse(v, book.minor_units).unwrap();
    let cost = m("9999999999999999.9999");
    let source = l
        .post_entry(
            s,
            2,
            &NewEntry {
                entry_date: date(2026, 1, 17),
                description: "Four-place boundary".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(accounts[0], cost),
                    NewLine::credit(accounts[4], cost),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    let mut a = input(revision.id);
    a.residual_value = Some(m("0.0001"));
    let asset = f.acquire_from_entry(s, 2, source.id, 1, &a).await?;
    let detail = f.get_asset(s, 2, asset.id).await?;
    assert_eq!(asset.asset_number, 1);
    assert_eq!(detail.asset.cost, cost);
    assert_eq!(
        detail.schedule.iter().map(|r| r.amount).collect::<Vec<_>>(),
        vec![
            m("200000000000000"),
            m("196000000000000"),
            m("9603999999999999.9998")
        ]
    );
    assert_eq!(
        detail.schedule.iter().map(|r| r.amount).sum::<Money>(),
        cost - m("0.0001")
    );
    assert_eq!(detail.schedule[2].carrying_value, m("0.0001"));
    let exported = f.evidence_page(s, 2, "asset", Some(1), None).await?;
    assert_eq!(exported.rows[0]["cost"], "9999999999999999.9999");
    assert_eq!(exported.rows[0]["residual_value"], "0.0001");
    println!(
        "four-place numeric limit: acquisition, schedule, residual and evidence preserve exact decimals"
    );
    Ok(())
}
