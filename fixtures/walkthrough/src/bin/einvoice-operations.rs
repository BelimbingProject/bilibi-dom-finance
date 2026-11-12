//! H4 disposable source provisioning; mirrors Bilimbi Finance.Einvoice's
//! linked-document flow. Run agent-operations first; never installs seeds.
use bilibi_database::{pool, Db, Error, MutationBoundary};
use bilibi_finance_ledger::{AccountType, Ledger, Money, NewAccount, NewBook, NewFiscalYear};
use bilibi_finance_payables::{DocumentLineInput, NewDocument, Payables, SettingsInput};
use bilibi_finance_receivables::Receivables;
use bilibi_finance_walkthrough_fixture::sign_in;
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::civil::date;
use serde_json::{json, Value};
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_einvoice_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

async fn setting(
    db: &Db,
    services: &Services,
    scope: &Scope,
    company: i64,
    key: &str,
    value: Value,
) -> Outcome<()> {
    if ![1, 2].contains(&company) {
        return Err(Error::Invalid.into());
    }
    let mut c = db.get().await?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut tx = boundary
        .begin(
            &mut c,
            scope.mutation_context("walkthrough.einvoice.profile"),
        )
        .await?;
    services
        .get::<Settings>("base/settings.store")?
        .put(
            &mut tx,
            scope,
            Target::Company(OrganisationId::new(company)?),
            key,
            value,
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Outcome<()> {
    if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable") {
        return Err(Error::Invalid.into());
    }
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
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    let c = db.get().await?;
    if c.query_opt(
        "SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id=1 AND i.email='manager@example.test'",
        &[],
    )
    .await?
    .is_none()
    {
        return Err(Error::Invalid.into());
    }
    drop(c);
    let manager = sign_in(&db, &services, 1, 1).await?;
    let approver = sign_in(&db, &services, 1, 7).await?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let admin = sign_in(&db, &services, 1, 4).await?;
        match args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["saturation"] => {
                let fixture: Value = serde_json::from_slice(&std::fs::read(std::env::var(
                    "BILIBI_EINVOICE_FIXTURE",
                )?)?)?;
                let input = &fixture[0]["inputs"][7];
                let registry =
                    services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
                let bilibi_agent_api::Handler::SafeWrite(handler) = registry.operations
                    ["finance.einvoice.document.issue"]
                    .definition
                    .handler
                else {
                    return Err(Error::Invalid.into());
                };
                let mut held = Vec::new();
                for _ in 0..db.status().max_size {
                    held.push(db.get().await?);
                }
                let start = std::time::Instant::now();
                let result = handler.preview(&**held[0], &manager, input).await;
                if result.is_ok() || start.elapsed() > std::time::Duration::from_secs(4) {
                    return Err(Error::Invalid.into());
                }
                drop(held);
                let c = db.get().await?;
                handler.preview(&**c, &manager, input).await?;
                println!("H4 saturated pool: preview refused within 4 seconds; releasing checkouts restores successful preview.");
                return Ok(());
            }
            ["profile", company, activity] => {
                setting(
                    &db,
                    &services,
                    &manager,
                    company.parse()?,
                    "finance.einvoice.supplier.business_activity",
                    json!(activity),
                )
                .await?
            }
            ["environment", company, environment]
                if ["sandbox", "production"].contains(environment) =>
            {
                setting(
                    &db,
                    &services,
                    &manager,
                    company.parse()?,
                    "finance.einvoice.environment",
                    json!(environment),
                )
                .await?;
            }
            ["grant", key, enabled]
                if bilibi_finance_einvoice::CAPABILITIES.contains(key)
                    || *key == bilibi_finance_ledger::VIEW =>
            {
                services
                    .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
                    .set_direct(&admin, 1, key, Some(enabled.parse()?))
                    .await?;
            }
            ["archive"] => {
                services
                    .get::<bilibi_company::CompanyService>("core/company.companies")?
                    .archive_company(&admin, 2, Some("Disposable H4 review fence"))
                    .await?;
            }
            _ => return Err(Error::Invalid.into()),
        }
        println!("H4 guarded facade control recorded.");
        return Ok(());
    }
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let mut fixtures = Vec::new();
    for company in [1, 2] {
        // Fresh bootstrap only: opening an existing book fails instead of
        // silently changing a previous proof's data.
        let book = ledger
            .open_book(
                &manager,
                company,
                &NewBook {
                    currency_code: "MYR".into(),
                    minor_units: 2,
                },
            )
            .await?;
        ledger
            .open_fiscal_year(
                &manager,
                company,
                &NewFiscalYear {
                    start_date: Some(date(2026, 1, 1)),
                    end_date: Some(date(2026, 12, 31)),
                },
            )
            .await?;
        let mut accounts = Vec::new();
        for (code, kind) in [
            ("H4-A", AccountType::Asset),
            ("H4-L", AccountType::Liability),
            ("H4-I", AccountType::Income),
            ("H4-X", AccountType::Expense),
            ("H4-D", AccountType::Asset),
        ] {
            accounts.push(
                ledger
                    .create_account(&manager, company, &NewAccount::new(code, code, kind))
                    .await?
                    .id,
            );
        }
        ar.configure(
            &manager,
            company,
            &json!({"receivable_account_id":accounts[0],"income_account_id":accounts[2]}),
        )
        .await?;
        let series = ar
            .create_series(
                &manager,
                company,
                &json!({"code":"H4","name":"H4 invoices","kind":"invoice","prefix":"H4-"}),
            )
            .await?;
        let customer = ar
            .create_customer(
                &manager,
                company,
                &json!({"record_kind":"company","record_id": if company == 1 { 32 } else { 34 }}),
            )
            .await?;
        ap.put_settings(
            &manager,
            company,
            &SettingsInput {
                settings: bilibi_finance_payables::Settings {
                    payable_account_id: accounts[1],
                    quantity_tolerance: None,
                    price_tolerance: None,
                },
                reason: Some("H4 disposable source setup".into()),
            },
        )
        .await?;
        for (key, value) in [
            ("environment", "sandbox"),
            ("document_version", "1.0"),
            ("supplier.tax_id", "C2584563222"),
            ("supplier.registration_number", "201901234567"),
            ("supplier.industry_code", "47111"),
            ("supplier.business_activity", "Retail"),
            ("supplier.phone", "+60123456789"),
            ("supplier.address_line1", "2 Jalan Contoh"),
            ("supplier.city", "Kuala Lumpur"),
            ("supplier.state_code", "14"),
            ("supplier.country_code", "MYS"),
        ] {
            setting(
                &db,
                &services,
                &manager,
                company,
                &format!("finance.einvoice.{key}"),
                json!(value),
            )
            .await?;
        }
        let party = json!({"name":"H4 Counterparty","tax_id":"C29702635060","registration_type":"BRN","registration_number":"D12345678","phone":"+60123456789","industry_code":"47111","business_activity":"Retail","address":{"lines":["Lot 66"],"city":"Kuala Lumpur","postcode":"50480","state_code":"14","country_code":"MYS"}});
        let mut sources = Vec::new();
        // Several independent real records allow duplicate-code rollback and
        // interleaved plans without reusing an already-linked source.
        for index in 0..8 {
            let (module, id, entry) = if index % 2 == 0 {
                let doc = ar.create_draft(&manager, company, &json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id,"issue_date":"2026-01-10","lines":[{"description":"H4 source","quantity":"1","unit_price":"50.46"}]})).await?;
                let doc = ar.issue_document(&manager, company, doc.id).await?;
                (
                    "finance/receivables",
                    doc.id,
                    doc.ledger_entry_id.ok_or(Error::Internal)?,
                )
            } else {
                let doc = ap
                    .create_bill(
                        &manager,
                        company,
                        &NewDocument {
                            vendor_id: Some(
                                bilibi_finance_walkthrough_fixture::vendors::vendor(
                                    &db,
                                    &services,
                                    &manager,
                                    company,
                                    "h4",
                                    "H4 supplier",
                                )
                                .await?,
                            ),
                            party_document_number: Some(format!("H4-{index}")),
                            document_date: date(2026, 1, 10),
                            due_date: None,
                            description: None,
                            tax_lines: None,
                            lines: vec![DocumentLineInput {
                                description: "H4 source".into(),
                                account_id: accounts[3],
                                amount: Money::parse("50.46", book.minor_units)?,
                                tax_code_id: None,
                                external_order_line_id: None,
                                order_line_id: None,
                                quantity: None,
                            }],
                        },
                    )
                    .await?;
                let doc = bilibi_finance_walkthrough_fixture::payables::approve_then_post(
                    &ap, &approver, &manager, company, doc.id,
                )
                .await?;
                (
                    "finance/payables",
                    doc.id,
                    doc.entry_id.ok_or(Error::Internal)?,
                )
            };
            let source =
                json!({"module":module,"record":format!("document:{id}"),"ledger_entry_id":entry});
            let mut input = json!({"company_id":company,"document_type":if index%2==0 {"01"} else {"11"},"code_number":format!("H4-{company}-{index}"),"issued_at":"2026-01-10T04:05:06Z","currency_code":"MYR","exchange_rate":"1.123456","payment_terms":"Net 30","billing_period_start":"2026-01-01","billing_period_end":"2026-01-10","billing_period_description":"January supply","source":source,"lines":[
                {"description":"Asymmetric A","classification_code":"003","quantity":"1.700001","unit_price":"13.290001","discount_amount":"0.23","charge_amount":"0.07","tax_type":"01","tax_rate":"6","unit_code":"C62"},
                {"description":"Asymmetric B","classification_code":"003","quantity":"2.3","unit_price":"10.91","discount_amount":"0.11","charge_amount":"0.19","tax_type":"01","tax_rate":"6","unit_code":"C62"}
            ]});
            input[if index % 2 == 0 { "buyer" } else { "supplier" }] = party.clone();
            sources.push(input);
        }
        let payment_series = ar
            .create_series(
                &manager,
                company,
                &json!({"code":"H4-P","name":"H4 receipts","kind":"payment","prefix":"H4-P-"}),
            )
            .await?;
        let document_id = sources[0]["source"]["record"]
            .as_str()
            .ok_or(Error::Internal)?
            .strip_prefix("document:")
            .ok_or(Error::Internal)?
            .parse::<i64>()?;
        let payment = ar.record_payment(&manager, company, &json!({"series_id":payment_series.id,"customer_id":customer.id,"payment_date":"2026-01-10","amount":"50.46","deposit_account_id":accounts[4],"allocations":[{"document_id":document_id,"amount":"50.46"}]})).await?;
        let mut payment_input = sources[0].clone();
        payment_input["code_number"] = json!(format!("H4-{company}-8"));
        payment_input["source"] = json!({"module":"finance/receivables","record":format!("payment:{}",payment.id),"ledger_entry_id":payment.ledger_entry_id});
        sources.push(payment_input);
        fixtures.push(json!({"company_id":company,"inputs":sources,"payment_id":payment.id}));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(std::env::var("BILIBI_EINVOICE_FIXTURE")?)?;
    file.write_all(serde_json::to_string_pretty(&fixtures)?.as_bytes())?;
    println!(
        "H4 real posted sources and non-secret inputs saved; no e-invoice issued, submitted or queued."
    );
    Ok(())
}
