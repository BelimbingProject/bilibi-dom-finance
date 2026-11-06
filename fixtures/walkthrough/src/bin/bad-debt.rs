//! Part of #60: disposable invoice-linked bad-debt journal proof and browser host.
//! Run on a fresh database; `serve` keeps the proved records for browser/MCP use.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{
    AccountType, Ledger, LedgerError, Money, NewAccount, NewBook, NewEntry, NewFiscalYear, NewLine,
    Reversal,
};
use bilibi_finance_receivables::{AgingOptions, ReceivableError, Receivables, ReceivedTaxOptions};
use bilibi_finance_sst::{AdjustmentInput, MappingInput, Pins, RegistrationInput, Sst, TaxType};
use bilibi_finance_walkthrough_fixture::{sign_in, Installed, CHART, CURRENCY, MINOR_UNITS};
use jiff::civil::date;
use rust_decimal::Decimal;
use serde_json::json;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://localhost:3186".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let services = &app.services;
    bilibi_finance_walkthrough_fixture::without_step_up(services)?;
    if std::env::args().any(|a| a == "serve-existing") {
        if std::env::var("BILIBI_AGENT_PROOF").as_deref() != Ok("disposable")
            || db
                .get()
                .await?
                .query_opt(
                    "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=1 AND i.email='manager@example.test'",
                    &[],
                )
                .await?
                .is_none()
        {
            return Err(Error::Invalid.into());
        }
        return serve(app).await;
    }

    bilibi_finance_walkthrough_fixture::install_platform(&db).await?;
    services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    // Fixture identity only: give the browser reviewer and reader Company 001.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Alex Admin','human','admin@example.test'),(5,1,'person_5','Lee Viewer','human','viewer@example.test') ON CONFLICT DO NOTHING;
    INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(3,1,4,1,'Reviewer'),(4,1,5,1,'Reader') ON CONFLICT DO NOTHING;
    UPDATE users SET person_id=4 WHERE id=4 AND tenant_id=1 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE id=4 AND tenant_id=1 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
    UPDATE users SET person_id=5 WHERE id=2 AND tenant_id=1 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE id=2 AND tenant_id=1 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
    SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
    SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))").await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    for user in [1, 7] {
        for cap in bilibi_finance_receivables::CAPABILITIES
            .into_iter()
            .chain(bilibi_finance_ledger::CAPABILITIES)
            .chain(bilibi_finance_sst::CAPABILITIES)
            .chain(bilibi_finance_payables::CAPABILITIES)
        {
            grants.set_direct(&admin, user, cap, Some(true)).await?;
        }
    }
    let manager = sign_in(&db, services, 1, 1).await?;
    let checker = sign_in(&db, services, 1, 4).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let other = sign_in(&db, services, 2, 5).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let book = ledger
        .open_book(
            &manager,
            1,
            &NewBook {
                currency_code: CURRENCY.into(),
                minor_units: MINOR_UNITS,
            },
        )
        .await?;
    for (code, name, kind, retained) in CHART {
        let mut new = NewAccount::new(code, name, kind);
        new.retained_earnings = retained;
        ledger.create_account(&manager, 1, &new).await?;
    }
    let installed = Installed {
        book,
        accounts: ledger.list_accounts(&manager, 1).await?,
    };
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let sst = services.get::<Sst>(bilibi_finance_sst::SERVICE)?;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let control = account("1100")?;
    let bank = account("1000")?;
    let money = |s: &str| Money::parse(s, installed.book.minor_units).expect("fixture money");
    ledger
        .open_fiscal_year(
            &manager,
            1,
            &NewFiscalYear {
                start_date: Some(date(2025, 1, 1)),
                end_date: Some(date(2025, 12, 31)),
            },
        )
        .await?;
    ledger
        .open_fiscal_year(
            &manager,
            1,
            &NewFiscalYear {
                start_date: Some(date(2026, 1, 1)),
                end_date: Some(date(2026, 12, 31)),
            },
        )
        .await?;
    let expense = ledger
        .create_account(
            &manager,
            1,
            &NewAccount::new("BD-expense", "Bad-debt expense", AccountType::Expense),
        )
        .await?;
    let allowance = ledger
        .create_account(
            &manager,
            1,
            &NewAccount::new(
                "BD-allowance",
                "Allowance for receivables",
                AccountType::Asset,
            ),
        )
        .await?;
    ar.configure(
        &manager,
        1,
        &json!({"receivable_account_id":control,"income_account_id":account("4000")?}),
    )
    .await?;
    let series=ar.create_series(&manager,1,&json!({"code":"BD-I","name":"Invoices","kind":"invoice","prefix":"BD-I-","number_width":3})).await?;
    let receipts=ar.create_series(&manager,1,&json!({"code":"BD-R","name":"Receipts","kind":"payment","prefix":"BD-R-","number_width":3})).await?;
    let customer = ar
        .create_customer(
            &manager,
            1,
            &json!({"record_kind":"company","record_id":35,"payment_terms_days":30}),
        )
        .await?;
    let tax=ar.create_tax_code(&manager,1,&json!({"code":"BD-service","name":"Service tax specimen","rate":"8","account_id":account("2100")?})).await?;
    let mut invoices = vec![];
    for description in [
        "Unpaid invoice",
        "Part-paid invoice",
        "Allowance write-off",
        "Browser and MCP proposal",
    ] {
        let draft=ar.create_draft(&manager,1,&json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id,"issue_date":"2025-09-01","due_date":"2025-10-01","description":description,"lines":[{"description":description,"quantity":"1","unit_price":"100","tax_code_id":tax.id}]})).await?;
        invoices.push(ar.issue_document(&manager, 1, draft.id).await?);
    }
    if let Ok(root) = std::env::var("BAD_DEBT_STORAGE") {
        bilibi_finance_walkthrough_fixture::attachments::configure_storage(
            &db, services, &admin, &root,
        )
        .await?;
        let attachments = services
            .get::<bilibi_finance_attachments::Attachments>(bilibi_finance_attachments::SERVICE)?;
        attachments.attach(&admin,1,"finance/receivables/document",invoices[1].id,&bilibi_finance_attachments::NewFile {file_name:"collection-assessment.txt".into(),content_type:Some("text/plain".into()),content:b"Disposable collection assessment: reminders, failed arrangement and independent no-recovery assessment BD-2026-01.".to_vec()}).await?;
        println!("Collection assessment retained through invoice Source documents.");
    }
    let payment = json!({"series_id":receipts.id,"customer_id":customer.id,"payment_date":"2026-01-15","deposit_account_id":bank,"amount":"54","allocations":[{"document_id":invoices[1].id,"amount":"54"}]});
    ar.record_payment(&manager, 1, &payment).await?;
    let before = ar
        .aged_receivables(&manager, 1, date(2026, 9, 2), &AgingOptions::default())
        .await?;
    assert_eq!(before.total, Decimal::from(378));
    assert!(
        ar.tie_out_control_accounts(&manager, 1, date(2026, 9, 2), &Default::default())
            .await?
            .tied_out
    );
    let due = ar
        .received_tax_totals(
            &manager,
            1,
            &ReceivedTaxOptions::new(date(2026, 9, 2), date(2026, 9, 2)),
        )
        .await?;
    assert_eq!(
        due.iter()
            .filter(|r| r.measure == "unpaid")
            .map(|r| r.tax_amount)
            .sum::<Decimal>(),
        Decimal::from(28)
    );
    println!("Four invoices 432; collected 54; aged receivables/control 378. Day after 12 months: unpaid service tax 28 (not relief eligibility).");
    let input = json!({"document_id":invoices[1].id,"entry_date":"2026-10-01","amount":"54","offset_account_id":expense.id,"reason":"No reasonable expectation of recovery after documented collection efforts","evidence":"Collection assessment BD-2026-01; source documents on invoice"});
    assert!(matches!(
        ar.submit_write_off(&viewer, 1, &input).await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(ar.submit_write_off(&other, 1, &input).await.is_err());
    assert!(ar.submit_write_off(&manager, 2, &input).await.is_err());
    let mut wrong = input.clone();
    wrong["amount"] = json!("54.001");
    assert!(matches!(
        ar.submit_write_off(&manager, 1, &wrong).await,
        Err(ReceivableError::Invalid(_))
    ));
    wrong = input.clone();
    wrong["amount"] = json!("55");
    assert!(ar.submit_write_off(&manager, 1, &wrong).await.is_err());
    wrong = input.clone();
    wrong["offset_account_id"] = json!(control);
    assert!(ar.submit_write_off(&manager, 1, &wrong).await.is_err());
    wrong["offset_account_id"] = json!(bank);
    assert!(matches!(
        ar.submit_write_off(&manager, 1, &wrong).await,
        Err(ReceivableError::Invalid(_))
    ));
    wrong = input.clone();
    wrong["entry_date"] = json!("2025-10-01");
    assert!(ar.submit_write_off(&manager, 1, &wrong).await.is_err());
    let proposed = ar.submit_write_off(&manager, 1, &input).await?;
    assert_eq!(
        ar.get_document(&manager, 1, invoices[1].id)
            .await?
            .outstanding,
        Some(money("54"))
    );
    let decision =
        json!({"write_off_id":proposed.id,"reason":"Collection assessment and journal checked"});
    assert!(matches!(
        ar.approve_write_off(&manager, 1, &decision).await,
        Err(ReceivableError::Invalid(_))
    ));
    ledger
        .set_duty_separation(
            &manager,
            1,
            false,
            Some("Disposable strict-independence proof"),
        )
        .await?;
    assert!(ar.approve_write_off(&manager, 1, &decision).await.is_err());
    ledger.set_duty_separation(&manager, 1, true, None).await?;
    let before_count: i64 = db
        .get()
        .await?
        .query_one("SELECT count(*) FROM finance_ledger_entries", &[])
        .await?
        .get(0);
    db.get().await?.batch_execute("CREATE FUNCTION bd_capture_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.auditable_type='finance.receivables.write_off_event' THEN RAISE EXCEPTION 'bad-debt fixture late audit refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER bd_capture_fault BEFORE INSERT ON base_audit_mutations FOR EACH ROW EXECUTE FUNCTION bd_capture_fault()").await?;
    assert!(ar.approve_write_off(&checker, 1, &decision).await.is_err());
    db.get().await?.batch_execute("DROP TRIGGER bd_capture_fault ON base_audit_mutations; DROP FUNCTION bd_capture_fault()").await?;
    assert_eq!(
        db.get()
            .await?
            .query_one("SELECT count(*) FROM finance_ledger_entries", &[])
            .await?
            .get::<_, i64>(0),
        before_count
    );
    assert!(ar
        .get_document(&manager, 1, invoices[1].id)
        .await?
        .write_offs[0]
        .events
        .is_empty());
    let (first, second) = tokio::join!(
        ar.approve_write_off(&checker, 1, &decision),
        ar.approve_write_off(&checker, 1, &decision)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let approved = first.or(second)?;
    println!("Late audit refusal rolled back the journal and decision; competing approvals posted exactly once.");
    assert_eq!(
        ar.get_document(&manager, 1, invoices[1].id)
            .await?
            .outstanding,
        Some(Money::ZERO)
    );
    assert!(ar.approve_write_off(&checker, 1, &decision).await.is_err());
    assert!(ar.record_payment(&manager, 1, &payment).await.is_err());
    let entry = approved.events[0].ledger_entry_id.ok_or(Error::Internal)?;
    assert!(matches!(
        ledger
            .reverse_entry(
                &checker,
                1,
                entry,
                &Reversal {
                    entry_date: date(2026, 10, 15),
                    description: "Wrong door".into(),
                    reference: None
                }
            )
            .await,
        Err(LedgerError::OwnedBy(_))
    ));
    let net = ar
        .aged_receivables(&manager, 1, date(2026, 10, 1), &AgingOptions::default())
        .await?;
    let net_pins = net.pins();
    assert_eq!(net.total, Decimal::from(324));
    assert!(
        ar.tie_out_control_accounts(&manager, 1, date(2026, 10, 1), &Default::default())
            .await?
            .tied_out
    );
    assert_eq!(
        ar.received_tax_totals(
            &manager,
            1,
            &ReceivedTaxOptions::new(date(2026, 9, 2), date(2026, 9, 2))
        )
        .await?
        .iter()
        .filter(|r| r.measure == "unpaid")
        .map(|r| r.tax_amount)
        .sum::<Decimal>(),
        Decimal::from(28)
    );
    println!("Direct write-off 54 approved independently; aged receivables/control 324; original revenue/tax and 12-month schedule unchanged.");
    let reversal = json!({"write_off_id":proposed.id,"reversal_date":"2026-10-15","reason":"Customer resumed payment; restore balance before recovery receipt"});
    ar.reverse_write_off(&checker, 1, &reversal).await?;
    let mut recovery = payment.clone();
    recovery["payment_date"] = json!("2026-10-15");
    recovery["amount"] = json!("27");
    recovery["allocations"][0]["amount"] = json!("27");
    ar.record_payment(&manager, 1, &recovery).await?;
    assert_eq!(
        ar.get_document(&manager, 1, invoices[1].id)
            .await?
            .outstanding,
        Some(money("27"))
    );
    assert!(
        ar.tie_out_control_accounts(&manager, 1, date(2026, 10, 15), &Default::default())
            .await?
            .tied_out
    );
    assert_eq!(
        ar.aged_receivables(
            &manager,
            1,
            date(2026, 10, 15),
            &AgingOptions {
                through_entry_number: net_pins.through_entry_number,
                through_opening_batch_id: net_pins.through_opening_batch_id,
                through_settlement_number: net_pins.through_settlement_number,
                ..AgingOptions::default()
            }
        )
        .await?
        .total,
        Decimal::from(324)
    );
    assert!(ar.reverse_write_off(&checker, 1, &reversal).await.is_err());
    wrong = input.clone();
    wrong["amount"] = json!("27");
    wrong["entry_date"] = json!("2026-10-16");
    let remaining = ar.submit_write_off(&manager, 1, &wrong).await?;
    ar.approve_write_off(
        &checker,
        1,
        &json!({"write_off_id":remaining.id,"reason":"Remaining balance still irrecoverable"}),
    )
    .await?;
    println!("Reversed write-off, recovered 27 on original invoice, re-assessed/wrote off residual 27; pinned aging reproduced and control tied out.");
    wrong = input.clone();
    wrong["document_id"] = json!(invoices[2].id);
    wrong["amount"] = json!("108");
    wrong["offset_account_id"] = json!(allowance.id);
    let assessment = ledger
        .submit_journal(
            &manager,
            1,
            &NewEntry {
                entry_date: date(2026, 9, 30),
                description: "Specific allowance assessment for invoice BD-I-003".into(),
                reference: Some("BD-ALLOWANCE-ASSESSMENT".into()),
                lines: vec![
                    NewLine::debit(expense.id, money("108")),
                    NewLine::credit(allowance.id, money("108")),
                ],
            },
        )
        .await?;
    ledger.approve_journal(&checker, 1, assessment.id).await?;
    let w = ar.submit_write_off(&manager, 1, &wrong).await?;
    ar.approve_write_off(
        &checker,
        1,
        &json!({"write_off_id":w.id,"reason":"Use company's assessed allowance"}),
    )
    .await?;
    assert!(
        ar.tie_out_control_accounts(&manager, 1, date(2026, 10, 28), &Default::default())
            .await?
            .tied_out
    );
    wrong["document_id"] = json!(invoices[3].id);
    let rejected = ar.submit_write_off(&manager, 1, &wrong).await?;
    ar.reject_write_off(
        &checker,
        1,
        &json!({"write_off_id":rejected.id,"reason":"Collection evidence incomplete"}),
    )
    .await?;
    assert_eq!(
        ar.get_document(&manager, 1, invoices[3].id)
            .await?
            .outstanding,
        Some(money("108"))
    );
    println!("Allowance write-off and rejected proposal retained. Refused own approval, duplicate decisions, direct ledger reversal, over-settlement, excess/precision/control/date/reach errors.");
    sst.register(
        &manager,
        1,
        &RegistrationInput {
            service_tax_number: Some("W10-2501-00000001".into()),
            period_months: Some(1),
            period_start: Some(date(2025, 1, 1)),
            service_tax_basis: Some("payment".into()),
            ..Default::default()
        },
    )
    .await?;
    sst.map_tax_code(
        &manager,
        1,
        &MappingInput {
            source: "receivables".into(),
            tax_code_id: tax.id,
            box_key: Some("11d".into()),
            reason: None,
        },
    )
    .await?;
    let figures = sst
        .preview_return(
            &manager,
            1,
            TaxType::ServiceTax,
            date(2026, 9, 1),
            Pins::default(),
        )
        .await?;
    assert_eq!(figures.tax_payable, Decimal::from(28));
    let tax_return = sst
        .open_return(&manager, 1, TaxType::ServiceTax, date(2026, 10, 1))
        .await?;
    sst.add_adjustment(
        &manager,
        1,
        tax_return.id,
        &AdjustmentInput {
            box_key: "13d".into(),
            taxable_amount: None,
            tax_amount: Some("4".into()),
            description: Some(
                "Specimen external DG approval; no eligibility or actual approval asserted".into(),
            ),
            reference: Some("BD-EXTERNAL-APPROVAL-SPECIMEN".into()),
        },
    )
    .await?;
    sst.add_adjustment(&manager,1,tax_return.id,&AdjustmentInput {
        box_key:"11d".into(),taxable_amount:Some("25".into()),tax_amount:Some("2".into()),
        description:Some("Specimen repayment after recovery of 27 on invoice BD-I-002; externally approved refund was 4".into()),
        reference:Some("BD-I-002 / BD-EXTERNAL-APPROVAL-SPECIMEN".into()),
    }).await?;
    let return_report = sst.get_return(&manager, 1, tax_return.id).await?;
    assert_eq!(
        return_report.figures.ok_or(Error::Internal)?.tax_payable,
        Decimal::from(-2)
    );
    let trial = ledger
        .trial_balance(&manager, 1, date(2026, 10, 28), None)
        .await?;
    assert_eq!(trial.total_debit, trial.total_credit);
    assert_eq!(
        trial
            .rows
            .iter()
            .find(|r| r.account.id == account("2100").expect("tax account"))
            .ok_or(Error::Internal)?
            .credit_balance,
        money("32")
    );
    let aging = ar
        .aged_receivables(&manager, 1, date(2026, 10, 28), &AgingOptions::default())
        .await?;
    assert_eq!(aging.total, Decimal::from(216));
    println!("Reconciliation: original invoices 432 = receipts 81 + net write-offs 135 + open receivables 216. Original tax ledger 32 = received tax 4 + twelve-month unpaid tax 28; separate specimen return relief 4 less recovery repayment 2 gives a net deduction of 2, without posting a ledger refund or recording tax paid.");
    println!("SST: September unpaid tax 28; October manual 13(d) relief 4 and recovery repayment 2 use explicit specimen external approval/evidence. Bilibi neither determines eligibility nor applies/files. Income-tax treatment needs accountant confirmation.");
    for dataset in ["write_off", "write_off_event"] {
        let page = ar
            .evidence_page(&manager, 1, dataset, Some(500), None)
            .await?;
        assert!(!page.rows.is_empty());
        println!(
            "Evidence {dataset}: {} immutable audited rows",
            page.rows.len()
        );
    }
    let client = db.get().await?;
    let unaudited:i64=client.query_one("SELECT count(*) FROM (SELECT 'finance.receivables.write_off' kind,id FROM finance_receivables_write_offs UNION ALL SELECT 'finance.receivables.write_off_event',id FROM finance_receivables_write_off_events) r WHERE NOT EXISTS(SELECT 1 FROM base_audit_mutations m WHERE m.tenant_id=1 AND m.auditable_type=r.kind AND m.auditable_id=r.id::text)",&[]).await?.get(0);
    assert_eq!(unaudited, 0);
    drop(client);
    println!("Browser: {} /finance/receivables/invoices/{}; manager@example.test / admin@example.test; walkthrough password. Unpaid invoice {} remains open.",origin,invoices[3].id,invoices[0].id);
    if std::env::args().any(|a| a == "serve") {
        serve(app).await?;
    }
    Ok(())
}

async fn serve(app: bilibi_web::App) -> Result<(), Box<dyn std::error::Error>> {
    axum::serve(
        tokio::net::TcpListener::bind(
            std::env::var("BIND_ADDR").unwrap_or("127.0.0.1:3186".into()),
        )
        .await?,
        bilibi_web::router(app),
    )
    .await?;
    Ok(())
}
