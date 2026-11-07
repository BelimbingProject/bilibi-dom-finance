//! NS1 and NS2 running proof against a fresh disposable PostgreSQL database:
//! `DATABASE_URL=… cargo run -p bilibi-finance-walkthrough-fixture --bin sst`.
//! Every business record is created through the real facades (ledger,
//! Receivables, Payables, SST), never seed SQL; the database's own refusals
//! run last, each in a transaction that is rolled back.
use bilibi_authz::{grants::Grants, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{pool, Db, Error};
use bilibi_finance_ledger::{
    AccountType, Ledger, Money, NewAccount, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_payables::{
    DocumentLineInput, NewDocument, NewTaxCode, Payables, Settings, SettingsInput, TaxCodeDetails,
};
use bilibi_finance_receivables::Receivables;
use bilibi_finance_sst::{
    Figures, MappingInput, Pins, RegistrationInput, Sst, SstError as E, TaxSource, TaxType,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_tenancy::Scope;
use jiff::civil::{date, Date};
use rust_decimal::Decimal;
use serde_json::json;
use std::fmt::Debug;

// NS2: the returns section, run last on top of NS1's records.
#[path = "../sst_returns.rs"]
mod returns;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
// The manager, affiliated with Company 001, holds every SST capability; the
// proof withdraws single grants from it directly to show each refusal.
inventory::submit! { Contribution::new(VOCABULARY,"walkthrough.fixture.finance.sst",Provenance {module:"finance/sst",source:file!(),line:line!()},&VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],
    roles:&[
        RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_sst::CAPABILITIES},
    ],
}) }

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn refused<T: Debug>(
    label: &str,
    result: Result<T, E>,
    expected: impl FnOnce(&E) -> bool,
) -> Outcome<()> {
    match result {
        Err(e) if expected(&e) => {
            println!("{label}: refused ({e})");
            Ok(())
        }
        other => Err(format!("{label}: unexpected {other:?}").into()),
    }
}
fn field<T: Debug>(label: &str, result: Result<T, E>, name: &str, message: &str) -> Outcome<()> {
    refused(
        label,
        result,
        |e| matches!(e, E::Invalid(errors) if errors.iter().any(|f| f.field == name && f.message == message)),
    )
}
fn d(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("fixture decimal")
}
/// A box's `(taxable, tax)` as the figures show it.
fn amounts(figures: &Figures, key: &str) -> (Option<Decimal>, Option<Decimal>) {
    let b = figures
        .boxes
        .iter()
        .find(|b| b.key == key)
        .expect("box on the form");
    (b.taxable_amount, b.tax_amount)
}
fn show(label: &str, f: &Figures) {
    println!(
        "{label}: {} {}..{} basis {:?}, {} ({} places), through entry {} and mapping {}",
        f.tax_type,
        f.period_start,
        f.period_end,
        f.service_tax_basis,
        f.currency_code,
        f.minor_units,
        f.through_entry_number,
        f.through_mapping_number
    );
    for b in f.boxes.iter().filter(|b| {
        b.taxable_amount.is_some_and(|a| !a.is_zero()) || b.tax_amount.is_some_and(|a| !a.is_zero())
    }) {
        println!(
            "  box {:<6} {:<55} value {:>10} tax {:>8}",
            b.item,
            b.label,
            b.taxable_amount.map_or("—".into(), |a| a.to_string()),
            b.tax_amount.map_or("—".into(), |a| a.to_string())
        );
    }
    println!("  tax payable (item 14): {}", f.tax_payable);
    for s in &f.sources {
        println!(
            "  source {} {} {} {} {} {}% x{} -> {} : {} / {}{}",
            s.line_number,
            s.source.as_str(),
            s.document_kind,
            s.measure,
            s.tax_code,
            s.rate,
            s.document_count,
            s.box_key.as_deref().unwrap_or("not reported"),
            s.taxable_amount,
            s.tax_amount,
            if s.rate_mismatch {
                " RATE MISMATCH"
            } else {
                ""
            }
        );
    }
    for u in &f.unmapped {
        println!(
            "  unmapped {} {} {} ({}%)",
            u.source.as_str(),
            u.document_kind,
            u.tax_code,
            u.rate
        );
    }
    for a in &f.accounts {
        println!(
            "  account {} {}: ledger Dr {} Cr {} | documents Dr {} Cr {} | difference {}",
            a.account_code,
            a.account_name,
            a.ledger_debits,
            a.ledger_credits,
            a.document_debits,
            a.document_credits,
            a.difference
        );
    }
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
            println!(
                "{label}: PostgreSQL refused ({})",
                e.as_db_error()
                    .map_or_else(String::new, |e| e.message().to_owned())
            );
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
    println!("NS1 and NS2: every SST setup, figures and returns proof passed");
    Ok(())
}

async fn issue(
    ar: &Receivables,
    admin: &Scope,
    attributes: serde_json::Value,
) -> Result<bilibi_finance_receivables::Document, bilibi_finance_receivables::ReceivableError> {
    let draft = ar.create_draft(admin, COMPANY, &attributes).await?;
    ar.issue_document(admin, COMPANY, draft.id).await
}

struct Chart {
    bank: i64,
    sales_tax: i64,
    service_tax: i64,
    acquired_tax: i64,
    other_tax: i64,
}

async fn chart(ledger: &Ledger, admin: &Scope) -> Outcome<Chart> {
    let accounts = ledger.list_accounts(admin, COMPANY).await?;
    let find = |code: &str| {
        accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let mut add = Vec::new();
    for (code, name) in [
        ("2110", "Sales tax payable"),
        ("2120", "Service tax payable"),
        ("2130", "Service tax on acquired services"),
        ("2140", "Other tax payable"),
    ] {
        add.push(
            ledger
                .create_account(
                    admin,
                    COMPANY,
                    &NewAccount::new(code, name, AccountType::Liability),
                )
                .await?
                .id,
        );
    }
    Ok(Chart {
        bank: find("1000")?,
        sales_tax: add[0],
        service_tax: add[1],
        acquired_tax: add[2],
        other_tax: add[3],
    })
}

async fn run(db: &Db, services: &Services) -> Outcome<()> {
    println!("\n== SST NS1: registration, tax-code mappings, taxable periods and figures");
    let sst = services.get::<Sst>(bilibi_finance_sst::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let manager = sign_in(db, services, 1, 1).await?;
    let staff = sign_in(db, services, 1, 3).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let admin = sign_in(db, services, 1, 4).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let raw = db.get().await?;
    let units = ledger.get_book(&admin, COMPANY).await?.minor_units;
    let money = |s: &str| Money::parse(s, units).expect("fixture money");
    let accounts = chart(&ledger, &admin).await?;

    // Registration.
    assert_eq!(sst.get_registration(&manager, COMPANY).await?, None);
    assert!(sst
        .taxable_periods(&manager, COMPANY, None, None)
        .await?
        .is_empty());
    refused(
        "preview before any registration",
        sst.preview_return(
            &manager,
            COMPANY,
            TaxType::SalesTax,
            date(2026, 1, 1),
            Pins::default(),
        )
        .await,
        |e| matches!(e, E::NotRegistered),
    )?;
    refused(
        "an account not affiliated with the company",
        sst.get_registration(&staff, COMPANY).await,
        |e| matches!(e, E::Unauthorized),
    )?;
    refused(
        "another tenant reads no registration",
        sst.get_registration(&other, COMPANY).await,
        |e| matches!(e, E::Unauthorized | E::CompanyNotFound),
    )?;
    let first = RegistrationInput {
        sales_tax_number: Some(" x01-2601-00000001 ".into()),
        period_start: Some(date(2026, 1, 1)),
        reason: Some("Registered for sales tax".into()),
        ..Default::default()
    };
    grants
        .set_direct(
            &admin,
            manager.user(),
            bilibi_finance_sst::MANAGE_SETTINGS,
            Some(false),
        )
        .await?;
    let denied = sst.register(&manager, COMPANY, &first).await;
    grants
        .set_direct(
            &admin,
            manager.user(),
            bilibi_finance_sst::MANAGE_SETTINGS,
            None,
        )
        .await?;
    refused("without the settings grant, no registration", denied, |e| {
        matches!(e, E::Unauthorized)
    })?;
    field(
        "no registration number",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_start: Some(date(2026, 1, 1)),
                ..Default::default()
            },
        )
        .await,
        "sales_tax_number",
        "or a service tax number is needed",
    )?;
    field(
        "a number of another shape",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                service_tax_number: Some("X1-2601-1".into()),
                ..first.clone()
            },
        )
        .await,
        "service_tax_number",
        "is not shaped like A00-0000-00000000",
    )?;
    field(
        "a thirteen-month cycle",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_months: Some(13),
                ..first.clone()
            },
        )
        .await,
        "period_months",
        "must be less than or equal to 12",
    )?;
    field(
        "an unknown service tax basis",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                service_tax_basis: Some("accrual".into()),
                ..first.clone()
            },
        )
        .await,
        "service_tax_basis",
        "is invalid",
    )?;
    field(
        "no first period",
        sst.register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_start: None,
                ..first.clone()
            },
        )
        .await,
        "period_start",
        "can't be blank",
    )?;
    let v1 = sst.register(&manager, COMPANY, &first).await?;
    assert_eq!(
        (v1.version, v1.sales_tax_number.as_deref()),
        (1, Some("X01-2601-00000001"))
    );
    assert_eq!(
        (
            v1.period_months,
            v1.service_tax_basis.as_str(),
            v1.service_tax_number.as_deref()
        ),
        (2, "payment", None)
    );
    assert_eq!(v1.recorded_by_user_id, manager.user());
    println!(
        "version 1: {:?} from {} every {} months, service tax due on {}",
        v1.sales_tax_number, v1.period_start, v1.period_months, v1.service_tax_basis
    );
    refused(
        "service tax before a service tax number",
        sst.preview_return(
            &manager,
            COMPANY,
            TaxType::ServiceTax,
            date(2026, 1, 1),
            Pins::default(),
        )
        .await,
        |e| matches!(e, E::NotRegisteredForTax),
    )?;
    let v2 = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                service_tax_number: Some("X01-2601-00000002".into()),
                reason: Some("Registered for service tax".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        (v2.version, v2.period_start, v2.sales_tax_number.clone()),
        (2, v1.period_start, v1.sales_tax_number.clone())
    );
    let same = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                sales_tax_number: Some("X01-2601-00000001".into()),
                reason: Some("Nothing changes".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(same, v2);
    println!("version 2 carries the cycle over and adds {:?}; a call that changes nothing records nothing", v2.service_tax_number);
    let periods = sst
        .taxable_periods(&manager, COMPANY, Some(date(2026, 5, 15)), Some(6))
        .await?;
    let shown: Vec<_> = periods
        .iter()
        .map(|p| (p.period_start, p.period_end, p.due_date))
        .collect();
    assert_eq!(
        shown,
        [
            (date(2026, 5, 1), date(2026, 6, 30), date(2026, 7, 31)),
            (date(2026, 3, 1), date(2026, 4, 30), date(2026, 5, 31)),
            (date(2026, 1, 1), date(2026, 2, 28), date(2026, 3, 31)),
        ]
    );
    println!("two-month periods to 15 May 2026, newest first, due the month after: {shown:?}");
    let before = sst
        .taxable_periods(&manager, COMPANY, Some(date(2025, 11, 30)), Some(2))
        .await?;
    assert_eq!(
        (before.len(), before[0].period_start),
        (1, date(2026, 1, 1))
    );
    refused(
        "25 periods",
        sst.taxable_periods(&manager, COMPANY, None, Some(25)).await,
        |e| matches!(e, E::InvalidOption),
    )?;
    for start in [date(2026, 1, 15), date(2026, 2, 1)] {
        refused(
            &format!("a period starting {start}"),
            sst.preview_return(&manager, COMPANY, TaxType::SalesTax, start, Pins::default())
                .await,
            |e| matches!(e, E::NotATaxablePeriod),
        )?;
    }
    refused(
        "a period before the registration",
        sst.preview_return(
            &manager,
            COMPANY,
            TaxType::SalesTax,
            date(2025, 11, 1),
            Pins::default(),
        )
        .await,
        |e| matches!(e, E::NotRegistered),
    )?;

    // Receivables and payables tax codes and documents through their facades.
    ar.configure(&admin, COMPANY, &json!({"receivable_account_id":ledger.list_accounts(&admin, COMPANY).await?.iter().find(|a| a.code == "1100").map(|a| a.id),"income_account_id":ledger.list_accounts(&admin, COMPANY).await?.iter().find(|a| a.code == "4000").map(|a| a.id)}))
        .await?;
    let mut series = Vec::new();
    for (code, kind) in [
        ("SINV", "invoice"),
        ("SCN", "credit_note"),
        ("SPAY", "payment"),
    ] {
        series.push(ar.create_series(&admin, COMPANY, &json!({"code":code,"name":code,"kind":kind,"prefix":format!("{code}-"),"number_width":3})).await?.id);
    }
    let customer = ar
        .create_customer(
            &admin,
            COMPANY,
            &json!({"record_kind":"company","record_id":22,"payment_terms_days":30}),
        )
        .await?
        .id;
    let mut codes = std::collections::HashMap::new();
    for (code, rate, account) in [
        ("ST10", "10", accounts.sales_tax),
        ("ST5", "5", accounts.sales_tax),
        ("EX", "0", accounts.sales_tax),
        ("SV8", "8", accounts.service_tax),
        ("SV6", "6", accounts.service_tax),
        ("OT3", "3", accounts.other_tax),
    ] {
        codes.insert(
            code,
            ar.create_tax_code(
                &admin,
                COMPANY,
                &json!({"code":code,"name":format!("{code} tax"),"rate":rate,"account_id":account}),
            )
            .await?
            .id,
        );
    }
    let invoice = |day: &str, lines: &[(&str, &str)]| {
        json!({"kind":"invoice","series_id":series[0],"customer_id":customer,"issue_date":day,
            "lines":lines.iter().map(|(code, price)| json!({"description":format!("{code} line"),"quantity":"1","unit_price":price,"tax_code_id":codes[code]})).collect::<Vec<_>>()})
    };
    let issue = |attributes: serde_json::Value| issue(&ar, &admin, attributes);
    let i1 = issue(invoice(
        "2026-01-10",
        &[
            ("ST10", "1000"),
            ("ST5", "200"),
            ("SV8", "500"),
            ("EX", "300"),
        ],
    ))
    .await?;
    assert_eq!(i1.total, money("2150"));
    issue(invoice("2026-02-05", &[("SV6", "100"), ("OT3", "100")])).await?;
    let credit = |day: &str, price: &str| json!({"kind":"credit_note","series_id":series[1],"customer_id":customer,"credited_document_id":i1.id,"issue_date":day,"lines":[{"description":"Returned goods","quantity":"1","unit_price":price,"tax_code_id":codes["ST10"]}]});
    issue(credit("2026-02-20", "100")).await?;
    ar.record_payment(&admin, COMPANY, &json!({"series_id":series[2],"customer_id":customer,"payment_date":"2026-02-15","amount":"1075","deposit_account_id":accounts.bank,"allocations":[{"document_id":i1.id,"amount":"1075"}]}))
        .await?;
    let payables = ledger
        .list_accounts(&admin, COMPANY)
        .await?
        .iter()
        .find(|a| a.code == "2000")
        .map(|a| a.id)
        .ok_or(Error::NotFound)?;
    let expense = ledger
        .list_accounts(&admin, COMPANY)
        .await?
        .iter()
        .find(|a| a.code == "5000")
        .map(|a| a.id)
        .ok_or(Error::NotFound)?;
    ap.put_settings(
        &manager,
        COMPANY,
        &SettingsInput {
            settings: Settings {
                payable_account_id: payables,
                quantity_tolerance: None,
                price_tolerance: None,
            },
            reason: Some("SST walkthrough".into()),
        },
    )
    .await?;
    let mut bill_codes = std::collections::HashMap::new();
    for (code, rate) in [("IS8", "8"), ("PX", "0")] {
        let created = ap
            .create_tax_code(
                &manager,
                COMPANY,
                &NewTaxCode {
                    code: code.into(),
                    details: TaxCodeDetails {
                        name: format!("{code} tax"),
                        rate: rate.into(),
                        account_id: accounts.acquired_tax,
                        active: true,
                        reason: None,
                    },
                },
            )
            .await?;
        bill_codes.insert(code, created.id);
    }
    let vendor = bilibi_finance_walkthrough_fixture::vendors::vendor(
        db,
        services,
        &manager,
        COMPANY,
        "supplier-sst",
        "SST supplier",
    )
    .await?;
    let document = |day: Date, lines: &[(&str, &str)]| NewDocument {
        vendor_id: Some(vendor),
        party_document_number: Some(format!("S-{day}")),
        document_date: day,
        due_date: None,
        description: None,
        lines: lines
            .iter()
            .map(|(code, amount)| DocumentLineInput {
                description: format!("{code} purchase"),
                account_id: expense,
                amount: money(amount),
                tax_code_id: Some(bill_codes[code]),
                external_order_line_id: None,
                order_line_id: None,
                quantity: None,
            })
            .collect(),
        tax_lines: None,
    };
    let bill = ap
        .create_bill(
            &manager,
            COMPANY,
            &document(date(2026, 1, 20), &[("IS8", "250"), ("PX", "400")]),
        )
        .await?;
    ap.approve_document(&admin, COMPANY, bill.id).await?;
    ap.post_document(&manager, COMPANY, bill.id).await?;
    let note = ap
        .create_debit_note(
            &manager,
            COMPANY,
            bill.id,
            &document(date(2026, 2, 10), &[("IS8", "50")]),
        )
        .await?;
    ap.approve_document(&admin, COMPANY, note.id).await?;
    ap.post_document(&manager, COMPANY, note.id).await?;
    ledger
        .post_entry(
            &manager,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 1, 31),
                description: "Sales tax correction without a document".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(accounts.sales_tax, money("5")),
                    NewLine::credit(accounts.bank, money("5")),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    println!("issued 2 invoices, 1 same-period credit note and a payment; posted a bill, a debit note and a manual sales tax entry");

    // Mappings.
    let listed = sst.list_tax_codes(&manager, COMPANY).await?;
    assert_eq!(listed.len(), 8);
    assert!(listed.iter().all(|c| c.mapping.is_none()));
    assert_eq!(
        listed
            .iter()
            .map(|c| (c.source, c.code.as_str()))
            .take(1)
            .collect::<Vec<_>>(),
        [(TaxSource::Receivables, "EX")]
    );
    grants
        .set_direct(
            &admin,
            manager.user(),
            "finance.receivables.view",
            Some(false),
        )
        .await?;
    let codes_denied = sst.list_tax_codes(&manager, COMPANY).await;
    let figures_denied = sst
        .preview_return(
            &manager,
            COMPANY,
            TaxType::SalesTax,
            date(2026, 1, 1),
            Pins::default(),
        )
        .await;
    let registration_read = sst.get_registration(&manager, COMPANY).await;
    grants
        .set_direct(&admin, manager.user(), "finance.receivables.view", None)
        .await?;
    refused(
        "without the receivables view, no tax codes",
        codes_denied,
        |e| matches!(e, E::Unauthorized),
    )?;
    refused(
        "without the receivables view, no figures",
        figures_denied,
        |e| matches!(e, E::Unauthorized),
    )?;
    assert_eq!(registration_read?.map(|r| r.version), Some(2));
    println!("without the receivables view the registration still reads");
    let map = |source: &str, code: i64, box_key: Option<&str>| MappingInput {
        source: source.into(),
        tax_code_id: code,
        box_key: box_key.map(str::to_owned),
        reason: Some("Mapped from the form".into()),
    };
    refused(
        "staff cannot map",
        sst.map_tax_code(
            &staff,
            COMPANY,
            &map("receivables", codes["ST10"], Some("11b")),
        )
        .await,
        |e| matches!(e, E::Unauthorized),
    )?;
    refused(
        "a ledger tax code",
        sst.map_tax_code(
            &manager,
            COMPANY,
            &map("ledger", codes["ST10"], Some("11b")),
        )
        .await,
        |e| matches!(e, E::InvalidSource),
    )?;
    refused(
        "an unknown tax code",
        sst.map_tax_code(&manager, COMPANY, &map("payables", 999_999, Some("19")))
            .await,
        |e| matches!(e, E::TaxCodeNotFound),
    )?;
    field(
        "a payables code to a sales box",
        sst.map_tax_code(
            &manager,
            COMPANY,
            &map("payables", bill_codes["IS8"], Some("11a")),
        )
        .await,
        "box",
        "is not a box a payables tax code reports in",
    )?;
    field(
        "a code to a total",
        sst.map_tax_code(
            &manager,
            COMPANY,
            &map("receivables", codes["ST10"], Some("12")),
        )
        .await,
        "box",
        "is not a box a receivables tax code reports in",
    )?;
    let mut mapped = Vec::new();
    for (source, code, key) in [
        ("receivables", codes["ST10"], Some("11b")),
        ("receivables", codes["ST5"], Some("11a")),
        ("receivables", codes["EX"], Some("18a")),
        ("receivables", codes["SV8"], Some("11d")),
        ("receivables", codes["OT3"], None),
        ("payables", bill_codes["IS8"], Some("11d")),
        ("payables", bill_codes["PX"], Some("19")),
    ] {
        mapped.push(
            sst.map_tax_code(&manager, COMPANY, &map(source, code, key))
                .await?,
        );
    }
    assert_eq!(
        mapped.iter().map(|m| m.mapping_number).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7]
    );
    let again = sst
        .map_tax_code(
            &manager,
            COMPANY,
            &map("receivables", codes["ST10"], Some(" 11b ")),
        )
        .await?;
    assert_eq!(again, mapped[0]);
    let page = sst
        .list_mappings(&manager, COMPANY, Some(3), Some(mapped[1].id))
        .await?;
    assert_eq!(
        page.iter().map(|m| m.mapping_number).collect::<Vec<_>>(),
        [3, 4, 5]
    );
    refused(
        "a page of 501",
        sst.list_mappings(&manager, COMPANY, Some(501), None).await,
        |e| matches!(e, E::InvalidOption),
    )?;
    println!("seven mappings numbered 1-7 (OT3 deliberately not reported); mapping ST10 to 11(b) again records nothing");

    // Figures for January and February.
    let jan = date(2026, 1, 1);
    let sales = sst
        .preview_return(&manager, COMPANY, TaxType::SalesTax, jan, Pins::default())
        .await?;
    show("sales tax, SV6 unmapped", &sales);
    assert_eq!(amounts(&sales, "11a"), (Some(d("200")), Some(d("10"))));
    assert_eq!(amounts(&sales, "11b"), (Some(d("900")), Some(d("90"))));
    assert_eq!(amounts(&sales, "18a"), (Some(d("300")), None));
    assert_eq!(amounts(&sales, "19"), (Some(d("400")), None));
    assert_eq!(
        (amounts(&sales, "12").1, sales.tax_payable),
        (Some(d("100")), d("100"))
    );
    assert_eq!(
        sales
            .unmapped
            .iter()
            .map(|u| u.tax_code.as_str())
            .collect::<Vec<_>>(),
        ["SV6"]
    );
    assert!(sales
        .sources
        .iter()
        .any(|s| s.tax_code == "OT3" && s.box_key.is_none()));
    let sales_tax_account = sales
        .accounts
        .iter()
        .find(|a| a.account_id == accounts.sales_tax)
        .expect("reconciled");
    assert_eq!(
        (
            sales_tax_account.ledger_debits,
            sales_tax_account.ledger_credits,
            sales_tax_account.document_debits,
            sales_tax_account.document_credits,
            sales_tax_account.difference
        ),
        (d("15"), d("110"), d("10"), d("110"), d("-5"))
    );
    assert!(!sales
        .accounts
        .iter()
        .any(|a| a.account_id == accounts.other_tax));
    assert_eq!(
        (sales.minor_units, sales.currency_code.as_str()),
        (2, "MYR")
    );
    let service = sst
        .preview_return(&manager, COMPANY, TaxType::ServiceTax, jan, Pins::default())
        .await?;
    show("service tax on the payment basis", &service);
    assert_eq!(service.service_tax_basis.as_deref(), Some("payment"));
    assert_eq!(amounts(&service, "11d"), (Some(d("450")), Some(d("36"))));
    assert!(service
        .sources
        .iter()
        .any(|s| s.measure == "received" && s.tax_code == "SV8" && s.tax_amount == d("20")));
    assert_eq!(service.tax_payable, d("36"));
    refused(
        "another tenant reads no figures",
        sst.preview_return(&other, COMPANY, TaxType::SalesTax, jan, Pins::default())
            .await,
        |e| matches!(e, E::Unauthorized | E::CompanyNotFound),
    )?;
    refused(
        "a negative pin",
        sst.preview_return(
            &manager,
            COMPANY,
            TaxType::SalesTax,
            jan,
            Pins {
                through_entry_number: Some(-1),
                through_mapping_number: None,
            },
        )
        .await,
        |e| matches!(e, E::InvalidOption),
    )?;

    // A code mapped to a box of another rate, then remapped.
    let wrong = sst
        .map_tax_code(
            &manager,
            COMPANY,
            &map("receivables", codes["SV6"], Some("11b")),
        )
        .await?;
    let mismatched = sst
        .preview_return(&manager, COMPANY, TaxType::SalesTax, jan, Pins::default())
        .await?;
    assert_eq!(
        mismatched
            .rate_mismatches
            .iter()
            .map(|r| r.tax_code.as_str())
            .collect::<Vec<_>>(),
        ["SV6"]
    );
    assert_eq!(
        amounts(&mismatched, "11b"),
        (Some(d("1000")), Some(d("96")))
    );
    println!(
        "SV6 (6%) mapped to 11(b) (10%) is a rate mismatch: {:?}",
        mismatched
            .rate_mismatches
            .iter()
            .map(|r| (&r.tax_code, r.rate))
            .collect::<Vec<_>>()
    );
    sst.map_tax_code(
        &manager,
        COMPANY,
        &map("receivables", codes["SV6"], Some("11c")),
    )
    .await?;
    let sales = sst
        .preview_return(&manager, COMPANY, TaxType::SalesTax, jan, Pins::default())
        .await?;
    assert!(sales.unmapped.is_empty() && sales.rate_mismatches.is_empty());
    assert_eq!(amounts(&sales, "11b"), (Some(d("900")), Some(d("90"))));
    let pinned = Pins {
        through_entry_number: Some(mismatched.through_entry_number),
        through_mapping_number: Some(wrong.mapping_number),
    };
    assert_eq!(
        sst.preview_return(&manager, COMPANY, TaxType::SalesTax, jan, pinned)
            .await?,
        mismatched
    );
    println!(
        "remapped to 11(c); pinned at mapping {} the mismatch reads again exactly",
        wrong.mapping_number
    );

    // A document dated in the period and posted later changes the live
    // figures, never a pinned reading.
    let pin = Pins {
        through_entry_number: Some(sales.through_entry_number),
        through_mapping_number: Some(sales.through_mapping_number),
    };
    let companies = services.get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?;
    companies
        .suspend_company(&admin, 22, Some("Walkthrough: customer suspended"))
        .await?;
    raw.execute(
        "UPDATE companies SET legal_name='SST renamed customer' WHERE tenant_id=1 AND id=22",
        &[],
    )
    .await?;
    assert_eq!(
        sst.preview_return(&manager, COMPANY, TaxType::SalesTax, jan, pin)
            .await?,
        sales
    );
    companies
        .reactivate_company(&admin, 22, Some("Walkthrough: customer reactivated"))
        .await?;
    issue(invoice("2026-02-25", &[("ST10", "10")])).await?;
    let live = sst
        .preview_return(&manager, COMPANY, TaxType::SalesTax, jan, Pins::default())
        .await?;
    assert_eq!(amounts(&live, "11b"), (Some(d("910")), Some(d("91"))));
    assert!(live.through_entry_number > sales.through_entry_number);
    assert_eq!(
        sst.preview_return(&manager, COMPANY, TaxType::SalesTax, jan, pin)
            .await?,
        sales
    );
    println!("a February invoice posted later: live 11(b) 910/91 through entry {}, pinned at entry {} still 900/90", live.through_entry_number, sales.through_entry_number);

    // A later cycle and basis: March and April under the invoice basis.
    let v3 = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_start: Some(date(2026, 3, 1)),
                service_tax_basis: Some("invoice".into()),
                reason: Some("Invoice basis approved".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!((v3.version, v3.service_tax_basis.as_str()), (3, "invoice"));
    issue(credit("2026-03-10", "50")).await?;
    issue(invoice("2026-03-15", &[("SV8", "1000")])).await?;
    let march = date(2026, 3, 1);
    let sales_march = sst
        .preview_return(&manager, COMPANY, TaxType::SalesTax, march, Pins::default())
        .await?;
    show("sales tax, March and April", &sales_march);
    assert_eq!(amounts(&sales_march, "13a").1, Some(d("5")));
    assert_eq!(amounts(&sales_march, "11b"), (Some(d("0")), Some(d("0"))));
    assert_eq!(sales_march.tax_payable, d("-5"));
    let service_march = sst
        .preview_return(
            &manager,
            COMPANY,
            TaxType::ServiceTax,
            march,
            Pins::default(),
        )
        .await?;
    show(
        "service tax on the invoice basis, March and April",
        &service_march,
    );
    assert_eq!(service_march.service_tax_basis.as_deref(), Some("invoice"));
    assert_eq!(
        amounts(&service_march, "11d"),
        (Some(d("1000")), Some(d("80")))
    );
    assert_eq!(
        sst.preview_return(&manager, COMPANY, TaxType::ServiceTax, jan, Pins::default())
            .await?
            .service_tax_basis
            .as_deref(),
        Some("payment")
    );
    println!("January's period stays under version 2 (payment basis); March's reads version 3 (invoice basis)");
    let v4 = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                period_months: Some(1),
                period_start: Some(date(2026, 7, 1)),
                reason: Some("Monthly cycle approved".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(v4.version, 4);
    let monthly = sst
        .taxable_periods(&manager, COMPANY, Some(date(2026, 8, 10)), None)
        .await?;
    let shown: Vec<_> = monthly
        .iter()
        .map(|p| (p.period_start, p.period_end, p.due_date))
        .collect();
    assert_eq!(
        shown,
        [
            (date(2026, 8, 1), date(2026, 8, 31), date(2026, 9, 30)),
            (date(2026, 7, 1), date(2026, 7, 31), date(2026, 8, 30))
        ]
    );
    println!("a varied monthly cycle is due 30 days after each period: {shown:?}");
    let history = sst.list_registrations(&manager, COMPANY).await?;
    println!(
        "registration history: versions {:?}",
        history.iter().map(|r| r.version).collect::<Vec<_>>()
    );
    assert_eq!(
        history.iter().map(|r| r.version).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );

    // Scope: an archived company records nothing and still reads.
    raw.batch_execute("UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1")
        .await?;
    let archived = sst
        .register(
            &manager,
            COMPANY,
            &RegistrationInput {
                reason: Some("Archived".into()),
                sales_tax_number: Some("X01-2601-00000009".into()),
                ..Default::default()
            },
        )
        .await;
    let readable = sst.get_registration(&manager, COMPANY).await;
    raw.batch_execute("UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1")
        .await?;
    refused("an archived company", archived, |e| {
        matches!(e, E::CompanyArchived)
    })?;
    assert_eq!(readable?.map(|r| r.version), Some(4));

    // Every row is audited.
    for (table, kind) in [
        ("finance_sst_registrations", "registration"),
        ("finance_sst_mappings", "mapping"),
    ] {
        let missing: i64 = raw
            .query_one(
                &format!("SELECT count(*) FROM {table} r WHERE NOT EXISTS (SELECT 1 FROM base_audit_mutations a WHERE a.tenant_id=r.tenant_id AND a.auditable_type=$1 AND a.auditable_id=r.id::text)"),
                &[&format!("finance.sst.{kind}")],
            )
            .await?
            .get(0);
        assert_eq!(missing, 0, "unaudited {table}");
    }
    println!("every registration version and mapping has its audit mutation");

    // The database's own rules, each rolled back.
    let reg = "(SELECT id FROM finance_sst_registrations WHERE company_id=1 ORDER BY version DESC LIMIT 1)";
    let ret = "currval(pg_get_serial_sequence('finance_sst_returns','id'))";
    let open = |start: &str, end: &str| {
        format!("INSERT INTO finance_sst_returns (tenant_id,company_id,return_number,tax_type,registration_id,period_start,period_end,due_date,opened_by_user_id,opened_at) VALUES (1,1,0,'sales_tax',{reg},'{start}','{end}','2026-03-31',1,now()); INSERT INTO finance_sst_return_events (tenant_id,company_id,event_number,return_id,action,recorded_by_user_id,recorded_at) VALUES (1,1,0,{ret},'opened',1,now())")
    };
    let finalise = format!("INSERT INTO finance_sst_return_events (tenant_id,company_id,event_number,return_id,action,version,through_entry_number,through_mapping_number,through_adjustment_number,currency_code,minor_units,tax_payable,recorded_by_user_id,recorded_at) VALUES (1,1,0,{ret},'finalised',0,1,1,0,'MYR',2,0,1,now())");
    let boxed = format!("INSERT INTO finance_sst_return_boxes (tenant_id,company_id,return_id,line_number,box,taxable_amount,tax_amount) VALUES (1,1,{ret},1,'14',NULL,0)");
    db_refuses(
        &raw,
        "update a registration",
        "UPDATE finance_sst_registrations SET reason='edited'",
        "UPDATE is refused",
    )
    .await?;
    db_refuses(
        &raw,
        "delete a mapping",
        "DELETE FROM finance_sst_mappings",
        "DELETE is refused",
    )
    .await?;
    db_refuses(
        &raw,
        "truncate the mappings",
        "TRUNCATE finance_sst_mappings",
        "TRUNCATE is refused",
    )
    .await?;
    db_refuses(&raw, "a return without its opened event", &format!("INSERT INTO finance_sst_returns (tenant_id,company_id,return_number,tax_type,registration_id,period_start,period_end,due_date,opened_by_user_id,opened_at) VALUES (1,1,0,'sales_tax',{reg},'2026-01-01','2026-02-28','2026-03-31',1,now()); COMMIT"), "has no opened event").await?;
    db_refuses(
        &raw,
        "a second open return of the period",
        &format!(
            "{}; {}",
            open("2026-01-01", "2026-02-28"),
            open("2026-01-01", "2026-02-28")
        ),
        "already has an open return",
    )
    .await?;
    db_refuses(
        &raw,
        "an overlapping period",
        &format!(
            "{}; {}",
            open("2026-01-01", "2026-02-28"),
            open("2026-02-01", "2026-03-31")
        ),
        "overlaps another period's return",
    )
    .await?;
    db_refuses(
        &raw,
        "a box outside finalising",
        &format!("{}; {boxed}", open("2026-01-01", "2026-02-28")),
        "is not being finalised",
    )
    .await?;
    db_refuses(
        &raw,
        "a finalised event without its boxes",
        &format!("{}; {finalise}; COMMIT", open("2026-01-01", "2026-02-28")),
        "finalised without its box figures",
    )
    .await?;
    db_refuses(&raw, "an adjustment on a finalised return", &format!("{}; {finalise}; {boxed}; INSERT INTO finance_sst_return_adjustments (tenant_id,company_id,adjustment_number,return_id,action,box,taxable_amount,tax_amount,description,recorded_by_user_id,recorded_at) VALUES (1,1,0,{ret},'added','13d',0,1,'Bad debt relief',1,now())", open("2026-01-01", "2026-02-28")), "its adjustments are kept as they are").await?;
    db_refuses(&raw, "a new return of a finalised period that amends nothing", &format!("{}; {finalise}; {boxed}; INSERT INTO finance_sst_returns (tenant_id,company_id,return_number,tax_type,registration_id,period_start,period_end,due_date,opened_by_user_id,opened_at) VALUES (1,1,0,'sales_tax',{reg},'2026-01-01','2026-02-28','2026-03-31',1,now())", open("2026-01-01", "2026-02-28")), "a new return amends return").await?;
    db_refuses(&raw, "a cycle change into a finalised period", &format!("{}; {finalise}; {boxed}; INSERT INTO finance_sst_registrations (tenant_id,company_id,version,sales_tax_number,period_months,period_start,service_tax_basis,recorded_by_user_id,recorded_at) VALUES (1,1,0,'X01-2601-00000001',2,'2026-01-01','payment',1,now())", open("2026-01-01", "2026-02-28")), "taxable periods are reported through 2026-02-28").await?;
    raw.batch_execute("BEGIN").await?;
    let numbered: i64 = raw
        .query_one(&format!("INSERT INTO finance_sst_returns (tenant_id,company_id,return_number,tax_type,registration_id,period_start,period_end,due_date,opened_by_user_id,opened_at) VALUES (1,1,0,'sales_tax',{reg},'2026-09-01','2026-09-30','2026-10-30',1,now()) RETURNING return_number"), &[])
        .await?
        .get(0);
    let version: i64 = raw
        .query_one("INSERT INTO finance_sst_registrations (tenant_id,company_id,version,sales_tax_number,period_months,period_start,service_tax_basis,recorded_by_user_id,recorded_at) VALUES (1,1,99,'X01-2601-00000001',1,'2026-07-01','payment',1,'2000-01-01') RETURNING version", &[])
        .await?
        .get(0);
    raw.batch_execute("ROLLBACK").await?;
    assert_eq!((numbered, version), (1, 5));
    println!("the database numbers what the writer sent: return number 0 stored as {numbered}, registration version 99 as {version}");
    returns::run(returns::Fixture {
        db,
        services,
        invoice_series: series[0],
        customer,
        codes: &codes,
        sales_tax_account: accounts.sales_tax,
    })
    .await
}
