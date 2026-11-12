//! Linked-vendor proof on a disposable PostgreSQL database.
//! VENDORS_UI=1 keeps the composed app running for browser verification.
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{Ledger, Money};
use bilibi_finance_payables::{
    AgingQuery, AllocationInput, DocumentLineInput, NewDocument, NewPayment, Payables,
    PayablesError, Settings, SettingsInput, VendorInput,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, vendors::vendor};
use jiff::civil::date;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();

fn refused<T: std::fmt::Debug>(r: Result<T, PayablesError>, field: &str, text: &str) {
    match r {
        Err(
            PayablesError::Invalid(e)
            | PayablesError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(e)),
        ) => assert!(
            e.iter()
                .any(|e| e.field == field && e.message.contains(text)),
            "{e:?}"
        ),
        other => panic!("unexpected refusal: {other:?}"),
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3198".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    if std::env::args().nth(1).as_deref() == Some("serve") {
        println!("Vendor browser fixture at {origin} (existing proof database)");
        axum::serve(
            tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
            bilibi_web::router(app),
        )
        .await?;
        return Ok(());
    }
    let services = &app.services;
    let installed = install(&db, services).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let reviewer = sign_in(&db, services, 1, 4).await?;
    let other = sign_in(&db, services, 2, 5).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let p = services.get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let id = vendor(&db, services, &manager, 1, "VEN-A", "Vendor company").await?;
    let v = p.get_vendor(&manager, 1, id).await?;
    assert_eq!(v.record_kind, "company");
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .map(|a| a.id)
            .ok_or(Error::NotFound)
    };
    let payable = account("2000")?;
    let vendor_control = ledger
        .create_account(
            &manager,
            1,
            &bilibi_finance_ledger::NewAccount::new(
                "2199",
                "Vendor control",
                bilibi_finance_ledger::AccountType::Liability,
            ),
        )
        .await?;
    let ar = services
        .get::<bilibi_finance_receivables::Receivables>(bilibi_finance_receivables::SERVICE)?;
    let customer = ar
        .create_customer(
            &manager,
            1,
            &serde_json::json!({"record_kind":"company","record_id":v.party.id}),
        )
        .await?;
    assert_eq!(Some(customer.party.id), Some(v.party.id));

    let expense = account("5000")?;
    let bank = account("1000")?;
    p.put_settings(
        &manager,
        1,
        &SettingsInput {
            settings: Settings {
                payable_account_id: payable,
                quantity_tolerance: None,
                price_tolerance: None,
            },
            reason: None,
        },
    )
    .await?;
    let before_book = p
        .create_vendor(
            &manager,
            2,
            &VendorInput {
                record_kind: Some("company".into()),
                record_id: Some(v.party.id),
                new_company: None,
                new_person: None,
                payment_terms_days: 7,
                payable_account_id: None,
                expense_account_id: None,
                tax_code_id: None,
                active: true,
            },
        )
        .await?;
    assert_eq!(
        p.get_vendor(&manager, 2, before_book.id)
            .await?
            .payment_terms_days,
        7
    );
    refused(
        p.get_vendor(&manager, 1, before_book.id).await,
        "vendor_id",
        "this company",
    );
    let input = VendorInput {
        record_kind: Some("company".into()),
        record_id: Some(v.party.id),
        new_company: None,
        new_person: None,
        payment_terms_days: 30,
        payable_account_id: Some(vendor_control.id),
        expense_account_id: Some(expense),
        tax_code_id: None,
        active: true,
    };
    p.update_vendor(&manager, 1, id, &input).await?;
    let mut inactive = input.clone();
    inactive.active = false;
    p.update_vendor(&manager, 1, id, &inactive).await?;

    refused(
        p.create_vendor(&manager, 1, &input).await,
        "record_id",
        "already a supplier",
    );
    let mut invalid = input.clone();

    invalid.record_id = Some(1);
    refused(
        p.create_vendor(&manager, 1, &invalid).await,
        "record_id",
        "itself",
    );
    invalid.record_id = Some(100);
    refused(
        p.create_vendor(&manager, 1, &invalid).await,
        "record_id",
        "",
    );
    invalid.record_id = Some(999999);
    refused(
        p.create_vendor(&manager, 1, &invalid).await,
        "record_id",
        "",
    );
    invalid = input.clone();
    invalid.payable_account_id = Some(expense);
    refused(
        p.update_vendor(&manager, 1, id, &invalid).await,
        "payable_account_id",
        "",
    );
    assert!(p.create_vendor(&viewer, 1, &input).await.is_err());
    assert!(p.get_vendor(&other, 1, id).await.is_err());
    let money = |s: &str| Money::parse(s, installed.book.minor_units).expect("proof money");
    let draft = |number: &str| NewDocument {
        vendor_id: Some(id),
        party_document_number: Some(number.into()),
        document_date: date(2026, 10, 10),
        due_date: None,
        description: None,
        lines: vec![DocumentLineInput {
            description: "Supplies".into(),
            account_id: expense,
            amount: money("100"),
            tax_code_id: None,
            external_order_line_id: None,
            order_line_id: None,
            quantity: None,
        }],
        tax_lines: None,
    };
    refused(
        p.create_bill(&manager, 1, &draft("INACTIVE-VENDOR")).await,
        "vendor_id",
        "inactive vendor",
    );
    p.update_vendor(&manager, 1, id, &input).await?;
    let mut unlinked = draft("MISSING");
    unlinked.vendor_id = None;
    refused(
        p.create_bill(&manager, 1, &unlinked).await,
        "vendor_id",
        "required",
    );
    for status in ["pending", "suspended", "archived"] {
        // Identity fixture states, including a previously active company
        // becoming unavailable between the vendor being added and a bill.
        db.get()
            .await?
            .execute(
                "UPDATE companies SET status=$2 WHERE id=$1",
                &[&v.party.id, &status],
            )
            .await?;
        refused(
            p.create_bill(&manager, 1, &draft(&format!("REFUSED-{status}")))
                .await,
            "vendor_id",
            status,
        );
    }
    db.get().await?.execute("UPDATE companies SET status='active',tax_id='CORE-TAX',registration_number='CORE-REG' WHERE id=$1",&[&v.party.id]).await?;
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_direct(&reviewer, manager.user(), bilibi_company::VIEW, Some(false))
        .await?;
    refused(
        p.create_bill(&manager, 1, &draft("NO-COMPANY-VIEW")).await,
        "vendor_id",
        // Without Companies view the company's facts cannot be copied onto
        // the bill, so the refusal names the missing permission.
        "needs Companies view permission to copy the company's tax ID onto the document",
    );
    grants
        .set_direct(&reviewer, manager.user(), bilibi_company::VIEW, None)
        .await?;
    db.get().await?.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) SELECT 1,id,'companies','tax_id' FROM roles WHERE tenant_id=1 AND code='manager' ON CONFLICT DO NOTHING",&[]).await?;
    refused(
        p.create_bill(&manager, 1, &draft("MASKED-TAX")).await,
        "vendor_id",
        "restricted",
    );
    db.get().await?.execute("DELETE FROM field_restrictions WHERE tenant_id=1 AND table_id='companies' AND field_id='tax_id' AND role_id IN (SELECT id FROM roles WHERE tenant_id=1 AND code='manager')",&[]).await?;
    let bill = p.create_bill(&manager, 1, &draft("VENDOR-BILL")).await?;
    assert_eq!(bill.party.tax_id.as_deref(), Some("CORE-TAX"));
    assert_eq!(bill.party.registration_id.as_deref(), Some("CORE-REG"));
    assert!(bill.party.address.is_none());
    assert_eq!(bill.due_date, date(2026, 11, 9));
    assert_eq!(bill.vendor_id, Some(id));
    let bill = bilibi_finance_walkthrough_fixture::payables::approve_then_post(
        &p, &reviewer, &manager, 1, bill.id,
    )
    .await?;
    assert_eq!(bill.payable_account_id, Some(vendor_control.id));
    // The bill's key is the vendor's own, `vendor:{id}`; no other key is
    // matched to a vendor, so every read of the vendor's records shares it.
    assert_eq!(bill.party.key, format!("vendor:{id}"));
    refused(
        p.create_bill(&manager, 1, &draft("VENDOR-BILL")).await,
        "party_document_number",
        "already recorded",
    );
    let requested = p
        .submit_payment(
            &manager,
            1,
            &NewPayment {
                payment_date: date(2026, 10, 12),
                vendor_id: Some(id),
                supplier_key: None,
                paid_from_account_id: bank,
                reference: None,
                description: None,
                allocations: vec![AllocationInput {
                    document_id: bill.id,
                    amount: money("10"),
                }],
            },
        )
        .await?;
    assert_eq!(requested.vendor_id, Some(id));
    refused(
        p.submit_payment(
            &manager,
            1,
            &NewPayment {
                payment_date: date(2026, 10, 12),
                vendor_id: None,
                supplier_key: None,
                paid_from_account_id: bank,
                reference: None,
                description: None,
                allocations: vec![AllocationInput {
                    document_id: bill.id,
                    amount: money("1"),
                }],
            },
        )
        .await,
        "vendor_id",
        "required",
    );
    let payment = p.approve_payment(&reviewer, 1, requested.id).await?;
    assert_eq!(payment.party_key, format!("vendor:{id}"));
    assert_eq!(payment.vendor_id, Some(id));
    db.get()
        .await?
        .execute(
            "UPDATE companies SET status='suspended' WHERE id=$1",
            &[&v.party.id],
        )
        .await?;
    p.reverse_payment(&reviewer, 1, payment.id, date(2026, 10, 13), None)
        .await?;
    let mut note = draft("NOTE");
    note.vendor_id = None;
    note.document_date = date(2026, 10, 14);
    note.lines[0].amount = money("5");
    let note = p.create_debit_note(&manager, 1, bill.id, &note).await?;
    bilibi_finance_walkthrough_fixture::payables::approve_then_post(
        &p, &reviewer, &manager, 1, note.id,
    )
    .await?;
    let aged = p
        .aged_payables(&manager, 1, date(2026, 10, 31), &AgingQuery::default())
        .await?;
    assert!(aged
        .parties
        .iter()
        .any(|p| p.party_key == format!("vendor:{id}") && p.vendor_id == Some(id)));
    let contracts = services.get::<bilibi_base::agent_api::page::PageContracts>(
        bilibi_base::agent_api::page::SERVICE,
    )?;
    for key in [
        "finance.payables.document.list",
        "finance.payables.payment_request.list",
        "finance.payables.payment.list",
    ] {
        let answer = contracts
            .read(
                &manager,
                key,
                &serde_json::json!({"company_id":1,"vendor_id":id,"page_size":100}),
            )
            .await?;
        let records = answer["result"]["records"]
            .as_array()
            .ok_or(Error::Invalid)?;
        assert!(!records.is_empty());
        assert!(records.iter().all(|r| r["vendor_id"].as_i64() == Some(id)));
    }
    let c = db.get().await?;
    assert!(c.execute("UPDATE finance_payables_vendors SET record_id='1' WHERE tenant_id=1 AND company_id=1 AND id=$1",&[&id]).await.is_err());
    for dataset in ["vendor"] {
        assert!(!p
            .evidence_page(&manager, 1, dataset, Some(500), None)
            .await?
            .rows
            .is_empty());
    }
    let _ = ledger.get_book(&manager, 1).await?;
    db.get()
        .await?
        .execute(
            "UPDATE companies SET status='active' WHERE id=$1",
            &[&v.party.id],
        )
        .await?;
    println!("Vendors: Core links and statuses; defaults; frozen facts; duplicate protection; payment approval, reversal and debit notes under the vendor's own key; evidence and SQL guards proved.");
    if std::env::var("VENDORS_UI").as_deref() == Ok("1") {
        println!("Vendor browser fixture at {origin}");
        axum::serve(
            tokio::net::TcpListener::bind(std::env::var("BIND_ADDR")?).await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}
