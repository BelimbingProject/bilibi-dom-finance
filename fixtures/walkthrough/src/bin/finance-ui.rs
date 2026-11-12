//! NU1: one disposable browser fixture for the Finance click-through sweep.
//! Run with DATABASE_URL, APP_ORIGIN, BIND_ADDR and BILIBI_SETTINGS_KEY.
//! Identity fixture SQL only; every Finance record is created by its facade.
use bilibi_authz::{RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{pool, Error};
use bilibi_finance_attachments::{Attachments, NewFile};
use bilibi_finance_ledger::{
    AccountType, EntryOwner, Ledger, Money, NewAccount, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_payables::{
    DocumentLineInput, NewDocument, Payables, Settings, SettingsInput, DOCUMENT_SOURCE,
};
use bilibi_finance_reconciliation::{NewBankAccount, Reconciliation};
use bilibi_finance_walkthrough_fixture::{attachments::configure_storage, install, sign_in};
use bilibi_module_registry::{Contribution, Provenance};
use jiff::civil::date;
use serde_json::json;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
macro_rules! browser_roles {
    ($id:literal, $module:literal, $caps:path) => {
        inventory::submit! { Contribution::new(VOCABULARY,$id,Provenance {module:$module,source:file!(),line:line!()},&VocabularyContribution {
            domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[
                RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&$caps},
            ],
        }) }
    };
}
browser_roles!(
    "walkthrough.browser.einvoice",
    "finance/einvoice",
    bilibi_finance_einvoice::CAPABILITIES
);
browser_roles!(
    "walkthrough.browser.assets",
    "finance/fixed_assets",
    bilibi_finance_fixed_assets::CAPABILITIES
);
browser_roles!(
    "walkthrough.browser.payroll",
    "finance/payroll",
    bilibi_finance_payroll::CAPABILITIES
);
browser_roles!(
    "walkthrough.browser.receivables",
    "finance/receivables",
    bilibi_finance_receivables::CAPABILITIES
);
browser_roles!(
    "walkthrough.browser.sst",
    "finance/sst",
    bilibi_finance_sst::CAPABILITIES
);
inventory::submit! { Contribution::new(VOCABULARY,"walkthrough.fixture.finance.browser",Provenance {module:"finance/ledger",source:file!(),line:line!()},&VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[
        RoleDefinition {code:"viewer",name:"viewer",description:"Walkthrough fixture role",grant_all:false,capabilities:&[
            "finance.ledger.view", "finance.attachments.view", "finance.payables.view", "finance.receivables.view",
            "finance.einvoice.view", "finance.fixed-assets.view", "finance.payroll.view",
            "finance.reconciliation.view", "finance.sst.view",
        ]},
    ],
}) }

const COMPANY: i64 = 100;

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
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Alex Admin','human','admin@example.test'),(5,1,'person_5','Lee Viewer','human','viewer@example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','admin@example.test'),(4,1,5,1,'Reader','viewer@example.test') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))"
    ).await?;
    // Tenant 2 is an ordinary tenant. Neither browser account is an operator.
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(6,2,'person_6','Other Manager','human','manager@other.example.test'),(7,2,'person_7','Other Reviewer','human','reviewer@other.example.test'),(8,2,'person_8','Other Reader','human','viewer@other.example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(5,2,6,100,'Recorder'),(6,2,7,100,'Reviewer'),(7,2,8,100,'Reader') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=6 WHERE tenant_id=2 AND id=5 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=5 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','reviewer@other.example.test',i.password_hash,2,8,2 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=5 ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 8,2,'Other Reviewer',id,7 FROM user_identities WHERE home_tenant_id=2 AND home_user_id=8 ON CONFLICT DO NOTHING; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=8 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','viewer@other.example.test',i.password_hash,2,9,2 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=5 ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 9,2,'Other Reader',id,8 FROM user_identities WHERE home_tenant_id=2 AND home_user_id=9 ON CONFLICT DO NOTHING; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=9 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT 2,8,id FROM roles WHERE tenant_id=2 AND code='admin' ON CONFLICT DO NOTHING;
         INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT 2,9,id FROM roles WHERE tenant_id=2 AND code='viewer' ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations));
         SELECT setval(pg_get_serial_sequence('users','id'),(SELECT max(id) FROM users))"
    ).await?;
    let manager = sign_in(&db, services, 2, 5).await?;
    let admin = sign_in(&db, services, 2, 8).await?;
    let operator = sign_in(&db, services, 1, 4).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let book = match ledger.get_book(&manager, COMPANY).await {
        Err(bilibi_finance_ledger::LedgerError::BookNotOpen) => {
            ledger
                .open_book(
                    &manager,
                    COMPANY,
                    &bilibi_finance_ledger::NewBook {
                        currency_code: "MYR".into(),
                        minor_units: 2,
                    },
                )
                .await?
        }
        other => other?,
    };
    let existing = ledger.list_accounts(&manager, COMPANY).await?;
    for (code, name, kind, retained) in bilibi_finance_walkthrough_fixture::CHART {
        if !existing.iter().any(|a| a.code == code) {
            let mut account = NewAccount::new(code, name, kind);
            account.retained_earnings = retained;
            ledger.create_account(&manager, COMPANY, &account).await?;
        }
    }
    if ledger
        .list_fiscal_years(&manager, COMPANY)
        .await?
        .is_empty()
    {
        ledger
            .open_fiscal_year(
                &manager,
                COMPANY,
                &bilibi_finance_ledger::NewFiscalYear {
                    start_date: Some(date(2026, 1, 1)),
                    end_date: None,
                },
            )
            .await?;
    }
    let installed = bilibi_finance_walkthrough_fixture::Installed {
        book,
        accounts: ledger.list_accounts(&manager, COMPANY).await?,
    };
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let root = std::env::var("FINANCE_UI_STORAGE")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    configure_storage(&db, services, &operator, &root).await?;
    let payables = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    if payables.list_tax_codes(&manager, COMPANY).await?.is_empty() {
        payables
            .create_tax_code(
                &manager,
                COMPANY,
                &bilibi_finance_payables::NewTaxCode {
                    code: "INPUT".into(),
                    details: bilibi_finance_payables::TaxCodeDetails {
                        name: "Input tax".into(),
                        rate: "10".into(),
                        account_id: account("2100")?,
                        active: true,
                        reason: None,
                    },
                },
            )
            .await?;
    }
    // Receivables sample data for the receivables pages: a chart default,
    // numbering, a tax code, four customers, invoices (drafts and issued, one
    // credited), receipts (one reversed). Every record is made by its facade.
    let ar = services
        .get::<bilibi_finance_receivables::Receivables>(bilibi_finance_receivables::SERVICE)?;
    if ar.list_customers(&manager, COMPANY).await?.is_empty() {
        ar.configure(
            &manager,
            COMPANY,
            &json!({"receivable_account_id": account("1100")?, "income_account_id": account("4000")?}),
        )
        .await?;
        let invoices = ar.create_series(&manager, COMPANY, &json!({"code":"INV","name":"Invoices","kind":"invoice","prefix":"INV-","number_width":5})).await?;
        let credits = ar.create_series(&manager, COMPANY, &json!({"code":"CN","name":"Credit notes","kind":"credit_note","prefix":"CN-","number_width":5})).await?;
        let receipts = ar.create_series(&manager, COMPANY, &json!({"code":"RCP","name":"Receipts","kind":"payment","prefix":"RCP-","number_width":5})).await?;
        let tax = ar.create_tax_code(&manager, COMPANY, &json!({"code":"SVC","name":"Service tax","rate":"6","account_id":account("2100")?})).await?;
        // Each customer is a Core company of this tenant, with a billing
        // address in Core Address; the first two are recorded as customers
        // of Company 100 in Companies; Finance records the remaining links
        // atomically. The fifth is pending (new invoices refused).
        let companies = services.get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?;
        let addresses = services.get::<bilibi_address::AddressService>(bilibi_address::SERVICE)?;
        let customer_role = companies
            .relationship_roles(&manager, false)
            .await?
            .into_iter()
            .find(|role| role.code == "customer")
            .map(|role| role.id)
            .ok_or(Error::NotFound)?;
        let mut customer = Vec::new();
        for (n, (_code, name, terms, email, city, status, related)) in [
            (
                "ACME",
                "Acme Trading Sdn Bhd",
                30,
                "accounts@acme.example.test",
                "Kuala Lumpur",
                "active",
                true,
            ),
            (
                "BORNEO",
                "Borneo Supplies",
                14,
                "ap@borneo.example.test",
                "Kuching",
                "active",
                true,
            ),
            (
                "CAHAYA",
                "Cahaya Retail",
                0,
                "hello@cahaya.example.test",
                "Penang",
                "active",
                false,
            ),
            (
                "DELTA",
                "Delta Works",
                45,
                "finance@delta.example.test",
                "Johor Bahru",
                "active",
                true,
            ),
            (
                "EMAS",
                "Emas Ventures",
                30,
                "admin@emas.example.test",
                "Ipoh",
                "pending",
                false,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let created = match companies
                .create_company(
                    &manager,
                    &bilibi_company::NewCompany {
                        name: name.into(),
                        status: Some(status.into()),
                        email: Some(email.into()),
                        phone: Some(format!("+60 3 5555 01{n:02}")),
                        tax_id: Some(format!("C{:08}", 25_000_000 + n)),
                        registration_number: Some(format!("20240100{:04}", 100 + n)),
                        ..bilibi_company::NewCompany::default()
                    },
                )
                .await?
            {
                bilibi_company::CreateOutcome::Created(company) => company,
                bilibi_company::CreateOutcome::Refused(errors) => {
                    return Err(format!("company {name} refused: {errors:?}").into());
                }
            };
            let target = bilibi_address::PartyId::Organisation(
                bilibi_address::OrganisationId::new(created.id)?,
            );
            addresses
                .create_and_attach(
                    &manager,
                    target,
                    &bilibi_address::AddressInput {
                        line1: Some(format!("{} Jalan Contoh", 12 + n)),
                        locality: Some(city.into()),
                        postcode: Some("50000".into()),
                        ..bilibi_address::AddressInput::default()
                    },
                    &bilibi_address::AttachmentInput {
                        kinds: vec![bilibi_address::AddressKind::Billing],
                        is_primary: true,
                        ..bilibi_address::AttachmentInput::default()
                    },
                )
                .await
                .map_err(|e| format!("billing address for {name}: {e:?}"))?;
            if related {
                companies
                    .assign_relationship(
                        &manager,
                        &bilibi_party::RelationshipInput {
                            source_organisation_id: bilibi_party::OrganisationId::new(COMPANY)?,
                            target: bilibi_party::PartyId::Organisation(
                                bilibi_party::OrganisationId::new(created.id)?,
                            ),
                            role_id: customer_role,
                            dates: bilibi_party::EffectiveDates::new(None, None)?,
                        },
                    )
                    .await
                    .map_err(|e| format!("customer relationship for {name}: {e:?}"))?;
            }
            customer.push(
                ar.create_customer(
                    &manager,
                    COMPANY,
                    &json!({"record_kind":"company","record_id": created.id, "payment_terms_days": terms}),
                )
                .await?,
            );
        }
        let draft = |customer: i64, date: &str, lines: serde_json::Value| {
            let attrs = json!({"kind":"invoice","series_id":invoices.id,"customer_id":customer,"issue_date":date,"lines":lines});
            let (ar, manager) = (&ar, &manager);
            async move { ar.create_draft(manager, COMPANY, &attrs).await }
        };
        let lines = |n: &str, price: &str, taxed: bool| {
            let mut line = json!({"description":"Consulting","quantity":n,"unit_price":price});
            if taxed {
                line["tax_code_id"] = json!(tax.id)
            }
            json!([line, {"description":"Travel","quantity":"1","unit_price":"80.00"}])
        };
        let mut issued = Vec::new();
        for (who, date, n, price, taxed) in [
            (0, "2026-07-01", "10", "150.00", true),
            (0, "2026-09-15", "5", "200.00", true),
            (1, "2026-08-10", "4", "320.00", false),
            (2, "2026-10-05", "2", "99.90", true),
            (3, "2026-05-20", "8", "175.00", true),
        ] {
            let d = draft(customer[who].id, date, lines(n, price, taxed)).await?;
            issued.push(ar.issue_document(&manager, COMPANY, d.id).await?);
        }
        let credit = ar.create_draft(&manager, COMPANY, &json!({
            "kind":"credit_note","series_id":credits.id,"customer_id":customer[0].id,
            "credited_document_id":issued[1].id,"issue_date":"2026-09-25",
            "lines":[{"description":"Travel not charged","quantity":"1","unit_price":"80.00"}]
        })).await?;
        ar.issue_document(&manager, COMPANY, credit.id).await?;
        draft(customer[2].id, "2026-10-18", lines("3", "120.00", true)).await?;
        ar.create_draft(
            &manager,
            COMPANY,
            &json!({"kind":"invoice","series_id":invoices.id,"customer_id":customer[3].id}),
        )
        .await?;
        let first = ar.record_payment(&manager, COMPANY, &json!({
            "series_id":receipts.id,"customer_id":customer[0].id,"payment_date":"2026-08-05",
            "amount":issued[0].total.to_string(),"deposit_account_id":account("1000")?,
            "reference":"TRF 88121","allocations":[{"document_id":issued[0].id,"amount":issued[0].total.to_string()}]
        })).await?;
        ar.record_payment(&manager, COMPANY, &json!({
            "series_id":receipts.id,"customer_id":customer[1].id,"payment_date":"2026-09-01",
            "amount":"500.00","deposit_account_id":account("1000")?,
            "reference":"Cheque 4471","allocations":[{"document_id":issued[2].id,"amount":"500.00"}]
        })).await?;
        let wrong = ar.record_payment(&manager, COMPANY, &json!({
            "series_id":receipts.id,"customer_id":customer[3].id,"payment_date":"2026-10-02",
            "amount":"100.00","deposit_account_id":account("1000")?,
            "reference":"Posted twice","allocations":[{"document_id":issued[4].id,"amount":"100.00"}]
        })).await?;
        ar.reverse_payment(
            &manager,
            COMPANY,
            wrong.id,
            &json!({"reversal_date":"2026-10-03","reason":"Bank reversed the transfer"}),
        )
        .await?;
        println!(
            "Receivables: /finance/receivables/invoices (first receipt {})",
            first.id
        );
    }
    if std::env::args().any(|arg| arg == "journals") {
        bilibi_finance_walkthrough_fixture::initialize_journal_approvals(
            &db, services, &manager, COMPANY,
        )
        .await?;
        let payroll =
            services.get::<bilibi_finance_payroll::Payroll>(bilibi_finance_payroll::SERVICE)?;
        if !payroll
            .list_postings(&manager, COMPANY)
            .await?
            .iter()
            .any(|posting| posting.source_kind == "payroll_run" && posting.source_id == 9004)
        {
            historical_payroll(
                &db,
                &ledger,
                &manager,
                account("5000")?,
                account("2000")?,
                installed.book.minor_units,
            )
            .await?;
        }
        println!("Journals browser fixture: {origin}; manager@other.example.test and reviewer@other.example.test; walkthrough password");
        axum::serve(
            tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
            bilibi_web::router(app),
        )
        .await?;
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "serve") {
        let _authority = bilibi_finance_walkthrough_fixture::browser::prepare_einvoice(
            &db, services, &manager, &operator, COMPANY,
        )
        .await?;
        axum::serve(
            tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
            bilibi_web::router(app),
        )
        .await?;
        return Ok(());
    }
    payables
        .put_settings(
            &manager,
            COMPANY,
            &SettingsInput {
                settings: Settings {
                    payable_account_id: account("2000")?,
                    quantity_tolerance: None,
                    price_tolerance: None,
                },
                reason: None,
            },
        )
        .await?;
    for n in 1..=3 {
        let bill = payables
            .create_bill(
                &manager,
                COMPANY,
                &NewDocument {
                    vendor_id: Some(
                        bilibi_finance_walkthrough_fixture::vendors::vendor(
                            &db,
                            services,
                            &manager,
                            COMPANY,
                            "browser-supplier",
                            "Paper supplier",
                        )
                        .await?,
                    ),
                    party_document_number: Some(format!("BROWSER-{n}")),
                    document_date: date(2026, 10, 1),
                    due_date: Some(date(2026, 10, 31)),
                    description: Some("Browser walkthrough".into()),
                    lines: vec![DocumentLineInput {
                        description: "Office supplies".into(),
                        account_id: account("5000")?,
                        amount: Money::parse("72.50", installed.book.minor_units)?,
                        tax_code_id: None,
                        external_order_line_id: None,
                        order_line_id: None,
                        quantity: None,
                    }],
                    tax_lines: None,
                },
            )
            .await?;
        // Approving posts in the same press, so bills 2 and 3 are in the books.
        if n > 1 {
            bilibi_finance_walkthrough_fixture::payables::approve_then_post(
                &payables, &admin, &manager, COMPANY, bill.id,
            )
            .await?;
        }
        let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
        let attachment = attachments
            .attach(
                &manager,
                COMPANY,
                DOCUMENT_SOURCE,
                bill.id,
                &NewFile {
                    file_name: format!("supplier-{n}.txt"),
                    content: b"Source document for browser proof".to_vec(),
                    content_type: Some("text/plain".into()),
                },
            )
            .await?;
        if n == 1 {
            attachments
                .place_hold(&manager, COMPANY, attachment.id, "Browser review")
                .await?;
        }
        println!("Bill {n}: /finance/payables/bills/{}", bill.id);
    }
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let cost = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1200", "Equipment cost", AccountType::Asset),
        )
        .await?;
    let accumulated = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1290", "Accumulated depreciation", AccountType::Asset),
        )
        .await?;
    // A posted bill that bought equipment and is not yet registered: "+ Asset"
    // offers its line under "Bought on".
    let laptop = payables
        .create_bill(
            &manager,
            COMPANY,
            &NewDocument {
                vendor_id: Some(
                    bilibi_finance_walkthrough_fixture::vendors::vendor(
                        &db,
                        services,
                        &manager,
                        COMPANY,
                        "browser-supplier",
                        "Paper supplier",
                    )
                    .await?,
                ),
                party_document_number: Some("BROWSER-LAPTOP".into()),
                document_date: date(2026, 10, 15),
                due_date: Some(date(2026, 11, 14)),
                description: Some("Office laptop".into()),
                lines: vec![DocumentLineInput {
                    description: "Laptop for the accounts team".into(),
                    account_id: cost.id,
                    amount: Money::parse("4200", installed.book.minor_units)?,
                    tax_code_id: None,
                    external_order_line_id: None,
                    order_line_id: None,
                    quantity: None,
                }],
                tax_lines: None,
            },
        )
        .await?;
    bilibi_finance_walkthrough_fixture::payables::approve_then_post(
        &payables, &admin, &manager, COMPANY, laptop.id,
    )
    .await?;
    let entry = ledger
        .post_entry(
            &manager,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 10, 1),
                description: "Equipment acquisition".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(cost.id, Money::parse("120", installed.book.minor_units)?),
                    NewLine::credit(
                        account("3000")?,
                        Money::parse("120", installed.book.minor_units)?,
                    ),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    println!(
        "Acquisition entry {}, line 1; cost {}; accumulated {}",
        entry.id, cost.id, accumulated.id
    );
    let assets = services
        .get::<bilibi_finance_fixed_assets::FixedAssets>(bilibi_finance_fixed_assets::SERVICE)?;
    let revision = assets
        .create_category(
            &manager,
            COMPANY,
            "Walkthrough equipment",
            &bilibi_finance_fixed_assets::RevisionInput {
                cost_account_id: cost.id,
                accumulated_account_id: accumulated.id,
                expense_account_id: account("5000")?,
                gain_account_id: account("4000")?,
                loss_account_id: account("5000")?,
                assumptions: bilibi_finance_fixed_assets::Assumptions {
                    method: bilibi_finance_fixed_assets::Method::StraightLine,
                    life_months: 3,
                    annual_rate: None,
                },
                reason: "Initial browser assumptions".into(),
            },
        )
        .await?;
    let asset = assets
        .acquire_from_entry(
            &manager,
            COMPANY,
            entry.id,
            1,
            &bilibi_finance_fixed_assets::AcquisitionInput {
                category_revision_id: revision.id,
                description: "Walkthrough equipment".into(),
                in_service_date: date(2026, 10, 1),
                residual_value: None,
                method: None,
                life_months: None,
                annual_rate: None,
            },
        )
        .await?;
    bilibi_finance_walkthrough_fixture::browser::prove_messages(services, &manager, asset.id)
        .await?;
    println!("Asset: /finance/fixed-assets/register/{}", asset.id);
    let bank = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?.create_bank_account(&manager,COMPANY,&NewBankAccount {
        name:"Operating account".into(),ledger_account_id:account("1000")?,charges_account_id:Some(account("5000")?),interest_account_id:Some(account("4000")?),match_window_days:None,active:None,reason:None,
        import_mapping:json!({"date":"Date","description":"Details","reference":"Ref","amount":"Amount","balance":"Balance"}),
    }).await?;
    println!("Bank account: /finance/reconciliation/accounts/{}", bank.id);
    historical_payroll(
        &db,
        &ledger,
        &manager,
        account("5000")?,
        account("2000")?,
        installed.book.minor_units,
    )
    .await?;
    // Existing authority stand-in creates documents and serves submit/poll locally.
    let _authority = bilibi_finance_walkthrough_fixture::browser::prepare_einvoice(
        &db, services, &manager, &operator, COMPANY,
    )
    .await?;
    println!(
        "Finance browser fixture: {origin}; manager@other.example.test, reviewer@other.example.test, viewer@other.example.test; walkthrough password"
    );
    axum::serve(
        tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
        bilibi_web::router(app),
    )
    .await?;
    Ok(())
}

async fn historical_payroll(
    db: &bilibi_database::Db,
    ledger: &Ledger,
    manager: &bilibi_tenancy::Scope,
    expense: i64,
    liability: i64,
    units: bilibi_finance_ledger::MinorUnits,
) -> Outcome<()> {
    // A historical posting gives the browser a reversal to inspect while
    // People is unmounted, as in the payroll walkthrough's historical proof.
    let mut client = db.get().await?;
    let mut tx = ledger
        .write(
            &mut client,
            manager,
            COMPANY,
            &[bilibi_finance_payroll::POST_POSTINGS],
            "fixture.payroll.browser",
        )
        .await?;
    let posting = ledger
        .post_entry_in(
            &mut tx,
            manager,
            COMPANY,
            &NewEntry {
                entry_date: date(2026, 10, 15),
                description: "Historical payroll fixture".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(expense, Money::parse("137.29", units)?),
                    NewLine::credit(liability, Money::parse("137.29", units)?),
                ],
            },
            &PostOptions {
                owner: Some(EntryOwner {
                    module: "finance/payroll".into(),
                    record: "payroll_run:9004".into(),
                }),
                ..Default::default()
            },
        )
        .await?;
    tx.mutate(move |sql| Box::pin(async move {
        let value: serde_json::Value = sql.query_one("INSERT INTO finance_payroll_posting_events AS e(tenant_id,company_id,source_kind,source_id,source_reference,event,entry_id,entry_number,entry_date,total,currency_code,actor_user_id,occurred_at) VALUES(2,100,'payroll_run',9004,'Historical fixture','posted',$1,$2,'2026-10-15',137.29,'MYR',5,now()) RETURNING to_jsonb(e)", &[&posting.id, &posting.entry_number]).await?.get(0);
        let id = value["id"].as_i64().ok_or(Error::Internal)?;
        Ok(((), vec![bilibi_database::Change { kind: "finance.payroll.posting_event".into(), id, old: serde_json::Value::Null, new: value }]))
    })).await?;
    tx.commit().await?;
    Ok(())
}
