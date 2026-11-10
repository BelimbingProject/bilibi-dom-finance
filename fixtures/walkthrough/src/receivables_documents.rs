//! Real NR2 proof, mirroring Bilimbi Receivables Documents, Payments and reports.
//! Runs last on the disposable walkthrough database; no mocks or product seeds.
use crate::sign_in;
use bilibi_database::Db;
use bilibi_finance_attachments::{AttachmentError, Attachments, NewFile};
use bilibi_finance_ledger::{
    AccountChange, AccountType, Ledger, LedgerError, Money, NewAccount, NewEntry, NewLine,
    PostOptions, Reversal,
};
use bilibi_finance_receivables::{
    AgingOptions, Page, PaymentQuery, ReceivableError as R, Receivables, ReceivedTaxOptions,
    TaxOptions,
};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::{json, Value};
use std::fmt::Debug;

fn field<T: Debug>(result: Result<T, R>, name: &str, message: &str) {
    // A refusal from the party facts frozen at issue arrives through the ledger.
    let found = match &result {
        Err(R::Invalid(errors)) => errors
            .iter()
            .any(|e| e.field == name && e.message == message),
        Err(R::Ledger(LedgerError::Invalid(errors))) => errors
            .iter()
            .any(|e| e.field == name && e.message == message),
        _ => false,
    };
    assert!(found, "expected {name}: {message}; got {result:?}");
}

pub async fn run(db: &Db, services: &Services) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n== Receivables NR2: issue, settle, reproduce and export");
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
    let einvoice =
        services.get::<bilibi_finance_einvoice::Einvoice>(bilibi_finance_einvoice::SERVICE)?;
    let grants = services.get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    let client = db.get().await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let manager = sign_in(db, services, 1, 1).await?;
    let viewer = sign_in(db, services, 1, 2).await?;
    let other = sign_in(db, services, 2, 5).await?;
    let units = ledger.get_book(&admin, 1).await?.minor_units;
    let money = |s: &str| Money::parse(s, units).expect("exact fixture money");
    let mut accounts = Vec::new();
    for (code, kind) in [
        ("R2-control1", AccountType::Asset),
        ("R2-control2", AccountType::Asset),
        ("R2-deposit", AccountType::Asset),
        ("R2-income", AccountType::Income),
        ("R2-tax", AccountType::Liability),
    ] {
        accounts.push(
            ledger
                .create_account(&admin, 1, &NewAccount::new(code, code, kind))
                .await?
                .id,
        );
    }
    let (control1, control2, bank, income, tax_account) = (
        accounts[0],
        accounts[1],
        accounts[2],
        accounts[3],
        accounts[4],
    );
    ar.configure(
        &admin,
        1,
        &json!({"receivable_account_id":control1,"income_account_id":income}),
    )
    .await?;
    let mut series = Vec::new();
    for (code, kind) in [
        ("INV", "invoice"),
        ("CREDIT", "credit_note"),
        ("PAY", "payment"),
    ] {
        series.push(ar.create_series(&admin, 1, &json!({"code":format!("R2-{code}"),"name":format!("R2 {code}"),"kind":kind,"prefix":format!("R2-{code}-"),"number_width":3})).await?);
    }
    let customer = ar
        .create_customer(
            &admin,
            1,
            &json!({"record_kind":"company","record_id":20,"payment_terms_days":30}),
        )
        .await?;
    let party = customer.party.clone();
    let prefill = einvoice.prefill_party(&admin, 1, party.id).await?;
    let registry = services.get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let key = "finance.einvoice.party.prefill";
    let bilibi_agent_api::Handler::Read(prefill_handler) =
        registry.operations[key].definition.handler
    else {
        panic!("prefill must be a read operation");
    };
    let prefill_input = json!({"company_id":1,"counterparty_id":party.id});
    let agent_prefill = prefill_handler
        .call(&**client, &admin, &prefill_input)
        .await?
        .envelope(key);
    assert_eq!(agent_prefill["result"], serde_json::to_value(&prefill)?);
    assert_eq!(prefill.name.as_deref(), Some(party.name.as_str()));
    assert_eq!(prefill.tax_id, party.tax_id);
    assert_eq!(prefill.registration_number, party.registration_number);
    assert_eq!(prefill.email, party.email);
    assert_eq!(prefill.phone, party.phone);
    assert!(prefill.address.is_none() && prefill.registration_type.is_none());
    let tax = ar
        .create_tax_code(
            &admin,
            1,
            &json!({"code":"R2-T10","name":"R2 ten percent","rate":"10","account_id":tax_account}),
        )
        .await?;
    let invoice = |day: Option<&str>, price: &str| json!({"kind":"invoice","series_id":series[0].id,"customer_id":customer.id,"issue_date":day,"lines":[{"description":"R2 saved line","quantity":"1","unit_price":price,"tax_code_id":tax.id}]});
    let payment = |day: &str, amount: &str, allocations: Value| json!({"series_id":series[2].id,"customer_id":customer.id,"payment_date":day,"amount":amount,"deposit_account_id":bank,"allocations":allocations});
    let draft = ar.create_draft(&admin, 1, &invoice(None, "100")).await?;
    // A draft saved without an issue date is issued with today's date in the
    // company's time, and the review says so before anyone confirms. Only the
    // review is taken here; this draft is dated and issued further down. When
    // today lies outside the open fiscal year the ledger refuses the entry
    // date, never the missing issue date.
    let issue_key = "finance.receivables.document.issue";
    let bilibi_agent_api::Handler::SafeWrite(issue_handler) =
        registry.operations[issue_key].definition.handler
    else {
        panic!("issue must be a reviewed write");
    };
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    match issue_handler
        .preview(
            &**client,
            &admin,
            &json!({"company_id":1,"document_id":draft.id}),
        )
        .await
    {
        Ok(review) => {
            let review = review.envelope(issue_key);
            let dated: jiff::civil::Date = review["result"]["changes"]["after"]["issue_date"]
                .as_str()
                .expect("the review dates the undated draft")
                .parse()?;
            assert!(
                (dated - today).get_days().abs() <= 1,
                "undated draft reviewed for {dated}, today is {today}"
            );
            assert!(review["result"]["changes"]["facts"]["draft"]["issue_date"].is_null());
        }
        Err(bilibi_agent_api::Error::InvalidInput(fields)) => assert!(
            !fields.contains_key("issue_date") && fields.contains_key("entry_date"),
            "undated draft refused for {fields:?}"
        ),
        Err(other) => panic!("undated draft review: {other:?}"),
    }
    assert!(ar
        .get_document(&admin, 1, draft.id)
        .await?
        .issue_date
        .is_none());
    // A typed amount may carry thousands commas, as the pages show amounts; a
    // comma that is not a thousands separator is refused with what to type.
    let grouped = ar
        .create_draft(&admin, 1, &invoice(None, "1,250.00"))
        .await?;
    assert_eq!(grouped.subtotal, money("1250"));
    assert_eq!(money("1,250.00"), money("1250"));
    assert!(Money::parse("1,25", units).is_err() && Money::parse("12,3456", units).is_err());
    field(
        ar.update_draft(
            &admin,
            1,
            grouped.id,
            &json!({"lines":[{"description":"R2 saved line","quantity":"1","unit_price":"1,25"}]}),
        )
        .await,
        "unit_price",
        "Enter the unit price as a number, like 1,250.00.",
    );
    ar.update_draft(&admin, 1, grouped.id, &json!({"issue_date":"9 Nov 2026"}))
        .await?;
    assert_eq!(
        ar.get_document(&admin, 1, grouped.id)
            .await?
            .issue_date
            .as_deref(),
        Some("2026-11-09")
    );
    field(
        ar.update_draft(&admin, 1, grouped.id, &json!({"issue_date":"next week"}))
            .await,
        "issue_date",
        "Enter the issue date like 9 Nov 2026.",
    );
    ar.discard_draft(&admin, 1, grouped.id).await?;
    let empty = ar.create_draft(&admin, 1, &json!({"kind":"invoice","series_id":series[0].id,"customer_id":customer.id,"issue_date":"2026-08-31"})).await?;
    field(
        ar.issue_document(&admin, 1, empty.id).await,
        "lines",
        "needs at least one line",
    );
    assert!(matches!(
        ar.issue_document(&manager, 1, draft.id).await,
        Err(R::Unauthorized)
    ));
    assert!(matches!(
        ar.record_payment(&manager, 1, &json!({})).await,
        Err(R::Unauthorized)
    ));
    assert!(matches!(
        ar.issue_document(&viewer, 1, draft.id).await,
        Err(R::Unauthorized)
    ));
    assert!(matches!(
        ar.get_document(&other, 1, draft.id).await,
        Err(R::Unauthorized) | Err(R::CompanyNotFound)
    ));

    let file = NewFile {
        file_name: "R2-source.pdf".into(),
        content: b"%PDF-1.4 R2 source".to_vec(),
        content_type: Some("application/pdf".into()),
    };
    let source = "finance/receivables/document";
    let scan = attachments
        .attach(&admin, 1, source, draft.id, &file)
        .await?;
    assert_eq!(scan.document_label, format!("Invoice (draft {})", draft.id));
    let undated = ar.create_draft(&admin, 1, &invoice(None, "1")).await?;
    let undated_scan = attachments
        .attach(&admin, 1, source, undated.id, &file)
        .await?;
    ar.discard_draft(&admin, 1, undated.id).await?;
    let retained = attachments
        .get_attachment(&admin, 1, undated_scan.id)
        .await?;
    assert_eq!(retained.retain_until, undated_scan.retain_until);
    assert_eq!(retained.document_date, undated_scan.document_date);
    let discarded_capture = attachments
        .attach(&admin, 1, source, undated.id, &file)
        .await?;
    assert_eq!(discarded_capture.document_date, undated_scan.document_date);
    assert!(matches!(
        attachments
            .remove(&admin, 1, undated_scan.id, "Discard is final")
            .await,
        Err(AttachmentError::DocumentFinal)
    ));
    ar.update_draft(&admin, 1, draft.id, &json!({"issue_date":"2026-08-31"}))
        .await?;
    ar.update_customer(&admin, 1, customer.id, &json!({"payment_terms_days":15}))
        .await?;
    // Issuing asks only the customer's status, not Companies, but the issuer
    // copies the company's facts onto the invoice under their own Companies
    // view: without it, or with a fact restricted for them, the issue is
    // refused with what to do.
    grants
        .set_direct(&admin, admin.user(), bilibi_company::VIEW, Some(false))
        .await?;
    assert!(matches!(
        einvoice.prefill_party(&admin, 1, party.id).await,
        Err(bilibi_finance_einvoice::EinvoiceError::Invalid(fields))
            if fields.iter().any(|f| f.field == "buyer" && (f.message.contains("Companies view permission") || f.message.contains("is restricted for you")))
    ));
    assert!(
        matches!(prefill_handler.call(&**client, &admin, &prefill_input).await,
        Err(bilibi_agent_api::Error::InvalidInput(fields)) if fields.contains_key("buyer"))
    );
    field(
        ar.issue_document(&admin, 1, draft.id).await,
        "customer_id",
        "needs Companies view permission to copy the company's tax ID onto the document",
    );
    grants
        .set_direct(&admin, admin.user(), bilibi_company::VIEW, None)
        .await?;
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,4,'companies','tax_id') ON CONFLICT DO NOTHING", &[]).await?;
    assert!(matches!(einvoice.prefill_party(&admin, 1, party.id).await,
        Err(bilibi_finance_einvoice::EinvoiceError::Invalid(fields)) if fields.iter().any(|f| f.field == "buyer" && f.message.contains("tax ID is restricted"))));
    assert!(
        matches!(prefill_handler.call(&**client, &admin, &prefill_input).await,
        Err(bilibi_agent_api::Error::InvalidInput(fields)) if fields.contains_key("buyer"))
    );
    field(
        ar.issue_document(&admin, 1, draft.id).await,
        "customer_id",
        "the company's tax ID is restricted for you, so it cannot be copied onto the document; ask an administrator to lift the restriction",
    );
    assert_eq!(
        ar.get_customer(&admin, 1, customer.id)
            .await?
            .party
            .restricted,
        vec!["tax_id".to_owned()]
    );
    client.execute("DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=4 AND table_id='companies' AND field_id='tax_id'", &[]).await?;
    ar.update_tax_code(&admin, 1, tax.id, &json!({"name":"R2 renamed tax"}))
        .await?;
    // Direct denial must be checked again even on an already sealed owner scope.
    grants
        .set_direct(
            &admin,
            admin.user(),
            bilibi_finance_receivables::ISSUE,
            Some(false),
        )
        .await?;
    let denied = ar.issue_document(&admin, 1, draft.id).await;
    grants
        .set_direct(
            &admin,
            admin.user(),
            bilibi_finance_receivables::ISSUE,
            None,
        )
        .await?;
    assert!(matches!(denied, Err(R::Unauthorized)));
    let (a, b) = tokio::join!(
        ar.issue_document(&admin, 1, draft.id),
        ar.issue_document(&admin, 1, draft.id)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let first = match (a, b) {
        (Ok(d), Err(R::NotADraft)) | (Err(R::NotADraft), Ok(d)) => d,
        other => panic!("concurrent issue: {other:?}"),
    };
    assert_eq!(first.document_number, Some(1));
    assert_eq!(first.document_reference.as_deref(), Some("R2-INV-001"));
    // The party is frozen as Core held it at issue. This company has no
    // address attached; the receivables-companies proof covers dated addresses.
    assert_eq!(first.party_name.as_deref(), Some(party.name.as_str()));
    assert_eq!(first.party_tax_identifier, party.tax_id);
    assert_eq!(first.party_email, party.email);
    assert_eq!(first.party_phone, party.phone);
    assert!(first.party_line1.is_none());
    assert_eq!(first.due_date.as_deref(), Some("2026-09-15"));
    assert_eq!(first.lines, draft.lines);
    assert_eq!(first.tax_lines, draft.tax_lines);
    assert_eq!(first.total, money("110"));
    let before_rename = ar
        .aged_receivables(&admin, 1, date(2026, 9, 1), &AgingOptions::default())
        .await?;
    let before_rename_pins = before_rename.pins();
    client
        .execute(
            "UPDATE companies SET legal_name='R2 later party' WHERE tenant_id=1 AND id=20",
            &[],
        )
        .await?;
    assert_eq!(
        ar.get_customer(&admin, 1, customer.id).await?.name,
        "R2 later party"
    );
    let frozen = ar.get_document(&admin, 1, first.id).await?;
    assert_eq!(frozen.party_name, first.party_name);
    assert_eq!(frozen.party_tax_identifier, first.party_tax_identifier);
    let after_rename = ar
        .aged_receivables(
            &admin,
            1,
            date(2026, 9, 1),
            &AgingOptions {
                through_entry_number: before_rename_pins.through_entry_number,
                through_opening_batch_id: before_rename_pins.through_opening_batch_id,
                through_settlement_number: before_rename_pins.through_settlement_number,
                ..AgingOptions::default()
            },
        )
        .await?;
    let find = |r: &bilibi_finance_receivables::AgedReceivables| {
        r.customers
            .iter()
            .find(|c| c.customer_id == customer.id)
            .cloned()
            .unwrap()
    };
    let (aged_before, aged) = (find(&before_rename), find(&after_rename));
    assert_eq!(aged_before.name, first.party_name.as_deref().unwrap());
    assert_eq!(aged.name, "R2 later party");
    assert_eq!(aged.code, aged_before.code);
    assert_eq!(aged.total, aged_before.total);
    assert_eq!(aged.bucket_totals, aged_before.bucket_totals);
    assert_eq!(after_rename.total, before_rename.total);
    assert_eq!(
        after_rename.through_entry_number,
        before_rename.through_entry_number
    );
    assert_eq!(aged.documents.len(), aged_before.documents.len());
    assert!(aged
        .documents
        .iter()
        .all(|d| d.party_name == first.party_name.as_deref().unwrap()));
    // Attachment labels are capture snapshots; a new capture reads the final source label.
    let final_scan = attachments
        .attach(&admin, 1, source, first.id, &file)
        .await?;
    assert_eq!(
        attachments
            .get_attachment(&admin, 1, final_scan.id)
            .await?
            .document_label,
        "Invoice 1"
    );
    assert!(matches!(
        attachments.remove(&admin, 1, scan.id, "Final").await,
        Err(AttachmentError::DocumentFinal)
    ));
    grants
        .set_direct(
            &admin,
            admin.user(),
            bilibi_finance_receivables::VIEW,
            Some(false),
        )
        .await?;
    let hidden = attachments.get_attachment(&admin, 1, scan.id).await;
    grants
        .set_direct(&admin, admin.user(), bilibi_finance_receivables::VIEW, None)
        .await?;
    assert!(matches!(hidden, Err(AttachmentError::Unauthorized)));
    assert!(matches!(
        ar.update_draft(&admin, 1, first.id, &json!({"description":"not editable"}))
            .await,
        Err(R::NotADraft)
    ));
    println!(
        "issue: party/terms frozen at issue, saved lines/tax preserved; concurrent issue exactly once; live grants and attachment finality"
    );

    ar.configure(&admin, 1, &json!({"receivable_account_id":control2}))
        .await?;
    for deposit in [control1, control2] {
        let mut refused = payment(
            "2026-09-10",
            "1",
            json!([{"document_id":first.id,"amount":"1"}]),
        );
        refused["deposit_account_id"] = json!(deposit);
        field(
            ar.record_payment(&admin, 1, &refused).await,
            "deposit_account_id",
            "is an account that records amounts customers owe; choose the bank or cash account receipts are paid into",
        );
    }
    // Accounts can be deactivated only at zero. Temporarily offset the invoice
    // through the ledger, deactivate, then reverse the offset. This leaves the
    // real outstanding invoice on its inactive control without bypassing guards.
    let offset = ledger
        .post_entry(
            &admin,
            1,
            &NewEntry {
                entry_date: date(2026, 8, 31),
                description: "R2 retire original control".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(bank, money("110")),
                    NewLine::credit(control1, money("110")),
                ],
            },
            &PostOptions::default(),
        )
        .await?;
    ledger
        .update_account(
            &admin,
            1,
            control1,
            &AccountChange {
                active: Some(false),
                ..Default::default()
            },
        )
        .await?;
    ledger
        .reverse_entry(
            &admin,
            1,
            offset.id,
            &Reversal {
                entry_date: date(2026, 8, 31),
                description: "R2 restore outstanding on retired control".into(),
                reference: None,
            },
        )
        .await?;
    let second = ar
        .create_draft(&admin, 1, &invoice(Some("2026-09-01"), "200"))
        .await?;
    let second = ar.issue_document(&admin, 1, second.id).await?;
    assert_eq!(second.receivable_account_id, Some(control2));
    assert_eq!(second.document_number, Some(2));
    let credit = |day: &str, price: &str| json!({"kind":"credit_note","series_id":series[1].id,"customer_id":customer.id,"credited_document_id":first.id,"issue_date":day,"lines":[{"description":"R2 credit","quantity":"1","unit_price":price,"tax_code_id":tax.id}]});
    let excessive = ar
        .create_draft(&admin, 1, &credit("2026-10-02", "101"))
        .await?;
    field(
        ar.issue_document(&admin, 1, excessive.id).await,
        "total",
        "exceeds what the credited invoice still owes",
    );
    let allocation = |id, amount: &str| json!({"document_id":id,"amount":amount});
    let valid = payment(
        "2026-09-10",
        "165",
        json!([allocation(first.id, "55"), allocation(second.id, "110")]),
    );
    for (attrs, name, message) in [
        (
            payment("2026-08-30", "1", json!([allocation(first.id, "1")])),
            "document_id",
            "was issued after the payment date",
        ),
        (
            payment(
                "2026-09-10",
                "2",
                json!([allocation(first.id, "1"), allocation(first.id, "1")]),
            ),
            "document_id",
            "is allocated twice",
        ),
        // No customer credit account is chosen here, so nothing may be left over.
        (
            payment("2026-09-10", "2", json!([allocation(first.id, "1")])),
            "allocations",
            "first choose the customer credit account",
        ),
        (
            payment("2026-09-10", "1", json!([allocation(first.id, "2")])),
            "allocations",
            "add up to more than the payment amount",
        ),
        (
            payment("2026-09-10", "111", json!([allocation(first.id, "111")])),
            "document_id",
            "owes less than the allocated amount",
        ),
    ] {
        field(ar.record_payment(&admin, 1, &attrs).await, name, message);
    }
    assert!(matches!(
        ar.record_payment(
            &admin,
            1,
            &payment(
                "2026-09-10",
                "1.001",
                json!([allocation(first.id, "1.001")])
            )
        )
        .await,
        Err(R::Invalid(_))
    ));
    let mut malformed = valid.clone();
    malformed["payment_date"] = json!(false);
    assert!(matches!(
        ar.record_payment(&admin, 1, &malformed).await,
        Err(R::Invalid(_))
    ));
    ledger
        .update_account(
            &admin,
            1,
            bank,
            &AccountChange {
                active: Some(false),
                ..Default::default()
            },
        )
        .await?;
    let inactive = ar.record_payment(&admin, 1, &valid).await;
    ledger
        .update_account(
            &admin,
            1,
            bank,
            &AccountChange {
                active: Some(true),
                ..Default::default()
            },
        )
        .await?;
    assert!(matches!(inactive, Err(R::Invalid(_))));

    // Close a real fiscal period: numbering, rows and audits must all roll back.
    let year = ledger
        .list_fiscal_years(&admin, 1)
        .await?
        .into_iter()
        .find(|y| y.start_date == date(2026, 1, 1))
        .expect("2026 year");
    let january = year.periods[0].id;
    ledger
        .close_period(&admin, 1, january, Some("R2 closed-period proof"))
        .await?;
    let closed = ar
        .create_draft(&admin, 1, &invoice(Some("2026-01-15"), "1"))
        .await?;
    let raw = db.get().await?;
    let before: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    let issue_refusal = ar.issue_document(&admin, 1, closed.id).await;
    let after: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    assert_eq!(before, after);
    // A payment in a closed month needs a previously issued invoice in that month.
    // The invoice is backdated only after reopening, then the same period is closed.
    ledger
        .reopen_period(&admin, 1, january, "R2 restore period")
        .await?;
    assert!(matches!(issue_refusal, Err(R::Ledger(_))));
    assert!(ar
        .get_document(&admin, 1, closed.id)
        .await?
        .document_number
        .is_none());
    // Issuance needs ISSUE + ledger view/post, not Receivables VIEW. The
    // source authorization refusal is refresh metadata, not lost issuance.
    grants
        .set_direct(
            &admin,
            admin.user(),
            bilibi_finance_receivables::VIEW,
            Some(false),
        )
        .await?;
    let narrow_issue = ar.issue_document(&admin, 1, closed.id).await;
    grants
        .set_direct(&admin, admin.user(), bilibi_finance_receivables::VIEW, None)
        .await?;
    let closed = narrow_issue?;
    assert_eq!(closed.document_number, Some(3));
    assert_eq!(
        closed.retention_refresh.as_deref(),
        Some("error: unauthorized")
    );
    ledger
        .close_period(&admin, 1, january, Some("R2 payment refusal"))
        .await?;
    // Bilimbi List.wrap accepts a single allocation object as well as arrays.
    let closed_payment = payment("2026-01-16", "1.10", allocation(closed.id, "1.10"));
    let before: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    let refused = ar.record_payment(&admin, 1, &closed_payment).await;
    let after: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    ledger
        .reopen_period(&admin, 1, january, "R2 restore period")
        .await?;
    assert!(matches!(refused, Err(R::Ledger(_))), "{refused:?}");
    assert_eq!(before, after);
    // Fail after the ledger, payment AND allocation inserts: even audit
    // capture must roll everything back, without consuming a series number.
    let before_capture: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    raw.batch_execute("CREATE FUNCTION nr2_refuse_capture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.auditable_type='finance.receivables.allocation' THEN RAISE EXCEPTION 'NR2 capture fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER nr2_capture_fault BEFORE INSERT ON base_audit_mutations FOR EACH ROW EXECUTE FUNCTION nr2_refuse_capture()").await?;
    let failed_capture = ar.record_payment(&admin, 1, &closed_payment).await;
    raw.batch_execute("DROP TRIGGER nr2_capture_fault ON base_audit_mutations; DROP FUNCTION nr2_refuse_capture()").await?;
    let after_capture: Value = raw.query_one("SELECT jsonb_build_array((SELECT count(*) FROM finance_receivables_payments),(SELECT count(*) FROM finance_ledger_entries),(SELECT count(*) FROM base_audit_mutations))", &[]).await?.get(0);
    assert!(matches!(failed_capture, Err(R::Failed(_))));
    assert_eq!(after_capture, before_capture);
    // Settle the auxiliary January invoice, keeping its distinct tax out of later windows.
    let jan_payment = ar.record_payment(&admin, 1, &closed_payment).await?;
    assert_eq!(jan_payment.payment_number, 1);
    field(
        ar.configure(&admin, 1, &json!({"receivable_account_id":bank}))
            .await,
        "receivable_account_id",
        "is an account receipts are paid into (a bank or cash account); choose the account that records amounts customers owe",
    );
    let baseline = ar
        .aged_receivables(&admin, 1, date(2026, 10, 6), &AgingOptions::default())
        .await?;
    let baseline_customer = baseline
        .customers
        .iter()
        .find(|c| c.customer_id == customer.id)
        .expect("R2 customer");
    assert_eq!(baseline_customer.total, money("330").amount());
    let baseline_pins = baseline.pins();
    let pin = baseline.through_entry_number;
    let companies = services.get::<bilibi_company::CompanyService>(bilibi_company::SERVICE)?;
    companies
        .suspend_company(&admin, party.id, Some("Walkthrough: customer suspended"))
        .await?;
    field(
        ar.create_draft(&admin, 1, &invoice(Some("2026-09-10"), "1"))
            .await,
        "customer_id",
        "This company is suspended, so it cannot be invoiced",
    );
    let paid = ar.record_payment(&admin, 1, &valid).await?;
    assert_eq!(paid.payment_number, 2);
    let entry = ledger.get_entry(&admin, 1, paid.ledger_entry_id).await?;
    assert_eq!(entry.lines.len(), 3);
    assert!(entry
        .lines
        .iter()
        .any(|l| l.account_id == control1 && l.credit == money("55")));
    assert!(entry
        .lines
        .iter()
        .any(|l| l.account_id == control2 && l.credit == money("110")));
    let payment_scan = attachments
        .attach(&admin, 1, "finance/receivables/payment", paid.id, &file)
        .await?;
    assert_eq!(payment_scan.document_label, "Payment 2");
    assert!(matches!(
        attachments
            .remove(&admin, 1, payment_scan.id, "Final payment")
            .await,
        Err(AttachmentError::DocumentFinal)
    ));
    for id in [first.ledger_entry_id.expect("issued"), paid.ledger_entry_id] {
        assert!(matches!(
            ledger
                .reverse_entry(
                    &admin,
                    1,
                    id,
                    &Reversal {
                        entry_date: date(2026, 9, 20),
                        description: "Generic reversal refused".into(),
                        reference: None
                    }
                )
                .await,
            Err(LedgerError::OwnedBy(_))
        ));
    }
    field(
        ar.reverse_payment(
            &admin,
            1,
            paid.id,
            &json!({"reversal_date":"2026-09-09","reason":"Too early"}),
        )
        .await,
        "reversal_date",
        "cannot be before the payment date",
    );
    let reversed = ar
        .reverse_payment(
            &admin,
            1,
            paid.id,
            &json!({"reversal_date":"2026-09-20","reason":"R2 bounced"}),
        )
        .await?;
    assert_eq!(reversed.allocations, paid.allocations);
    assert_eq!(reversed.amount, paid.amount);
    assert!(matches!(
        ar.reverse_payment(
            &admin,
            1,
            paid.id,
            &json!({"reversal_date":"2026-09-21","reason":"Again"})
        )
        .await,
        Err(R::AlreadyReversed)
    ));
    let partial = ar
        .record_payment(
            &admin,
            1,
            &payment("2026-09-25", "55", json!([allocation(first.id, "55")])),
        )
        .await?;
    ar.record_payment(
        &admin,
        1,
        &payment("2026-10-01", "11", json!([allocation(first.id, "11")])),
    )
    .await?;
    let note = ar
        .create_draft(&admin, 1, &credit("2026-10-02", "20"))
        .await?;
    let note = ar.issue_document(&admin, 1, note.id).await?;
    assert_eq!(note.document_number, Some(1));
    assert_eq!(note.receivable_account_id, Some(control1));
    assert_eq!(note.total, money("22"));
    ar.reverse_payment(
        &admin,
        1,
        partial.id,
        &json!({"reversal_date":"2026-10-05","reason":"R2 outside receipt window"}),
    )
    .await?;
    companies
        .reactivate_company(&admin, party.id, Some("Walkthrough: customer reactivated"))
        .await?;
    let reproduced = ar
        .aged_receivables(
            &admin,
            1,
            date(2026, 10, 6),
            &AgingOptions {
                through_entry_number: baseline_pins.through_entry_number,
                through_opening_batch_id: baseline_pins.through_opening_batch_id,
                through_settlement_number: baseline_pins.through_settlement_number,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&baseline)?,
        serde_json::to_value(&reproduced)?
    );
    for (day, expected) in [
        (date(2026, 9, 9), "330"),
        (date(2026, 9, 10), "165"),
        (date(2026, 9, 20), "330"),
        (date(2026, 9, 25), "275"),
        (date(2026, 10, 1), "264"),
        (date(2026, 10, 2), "242"),
        (date(2026, 10, 5), "297"),
    ] {
        let aged = ar
            .aged_receivables(&admin, 1, day, &AgingOptions::default())
            .await?;
        assert_eq!(
            aged.customers
                .iter()
                .find(|c| c.customer_id == customer.id)
                .expect("R2 aged")
                .total,
            money(expected).amount(),
            "aging {day}"
        );
    }
    let taxes = ar
        .tax_totals(
            &admin,
            1,
            &TaxOptions {
                from: Some(date(2026, 10, 1)),
                to: Some(date(2026, 10, 31)),
                through_entry_number: None,
            },
        )
        .await?;
    let tax_credit = taxes
        .iter()
        .find(|t| t.tax_code_id == tax.id)
        .expect("R2 tax credit");
    assert!(tax_credit.credits_prior_period && tax_credit.kind == "credit_note");
    assert_eq!(tax_credit.taxable_amount, money("20").amount());
    assert_eq!(tax_credit.tax_amount, money("2").amount());
    // Aug 31 + one calendar month = Sep 30; tax becomes due Oct 1.
    // On-due payments are not receipts; unpaid is the balance at due minus one.
    for (from, to, measure, base, tax_amount) in [
        (
            date(2026, 9, 10),
            date(2026, 9, 10),
            "received",
            "150",
            "15",
        ),
        (
            date(2026, 9, 20),
            date(2026, 9, 20),
            "received",
            "-150",
            "-15",
        ),
        (date(2026, 9, 1), date(2026, 9, 30), "received", "50", "5"),
        (date(2026, 10, 1), date(2026, 10, 1), "unpaid", "50", "5"),
        (date(2026, 10, 2), date(2026, 10, 2), "unpaid", "200", "20"),
        (
            date(2026, 10, 2),
            date(2026, 10, 2),
            "credited_after_due",
            "20",
            "2",
        ),
    ] {
        let rows = ar
            .received_tax_totals(
                &admin,
                1,
                &ReceivedTaxOptions {
                    from,
                    to,
                    unpaid_months: 1,
                    through_entry_number: None,
                },
            )
            .await?;
        let row = rows
            .iter()
            .find(|r| r.tax_code_id == tax.id && r.measure == measure)
            .expect("R2 received tax measure");
        assert_eq!(row.taxable_amount, money(base).amount(), "{from} {measure}");
        assert_eq!(
            row.tax_amount,
            money(tax_amount).amount(),
            "{from} {measure}"
        );
        if from == date(2026, 10, 1) {
            assert!(!rows
                .iter()
                .any(|r| r.tax_code_id == tax.id && r.measure == "received"));
        }
    }
    let outside = ar
        .received_tax_totals(
            &admin,
            1,
            &ReceivedTaxOptions {
                from: date(2026, 10, 5),
                to: date(2026, 10, 5),
                unpaid_months: 1,
                through_entry_number: None,
            },
        )
        .await?;
    assert!(!outside.iter().any(|r| r.tax_code_id == tax.id));
    println!(
        "reports: dated aging 330→165→330→275→264→242→297; old pin reproduces 330; calendar-month tax received 50/5, unpaid 50/5 + 200/20, prior-period credit 20/2"
    );
    let settled = ar
        .record_payment(
            &admin,
            1,
            &payment(
                "2026-10-06",
                "297",
                json!([allocation(first.id, "77"), allocation(second.id, "220")]),
            ),
        )
        .await?;
    assert_eq!(settled.payment_number, 5);
    assert_eq!(
        ar.get_document(&admin, 1, first.id).await?.outstanding,
        Some(Money::ZERO)
    );
    assert_eq!(
        ar.get_document(&admin, 1, second.id).await?.outstanding,
        Some(Money::ZERO)
    );

    // Equal and opposite differences must not net away across control accounts.
    let manual = ledger
        .post_entry(
            &admin,
            1,
            &NewEntry {
                entry_date: date(2026, 10, 7),
                description: "R2 cancelling control differences".into(),
                reference: None,
                lines: vec![
                    NewLine::debit(control1, money("7")),
                    NewLine::credit(control2, money("7")),
                ],
            },
            &PostOptions {
                settlement_account_ids: vec![control1],
                ..Default::default()
            },
        )
        .await?;
    let tied = ar
        .tie_out_control_accounts(&admin, 1, date(2026, 10, 7), &Default::default())
        .await?;
    let one = tied
        .accounts
        .iter()
        .find(|a| a.account_id == control1)
        .expect("old control retained");
    let two = tied
        .accounts
        .iter()
        .find(|a| a.account_id == control2)
        .expect("new control");
    assert_eq!(one.subledger_balance, money("0").amount());
    assert_eq!(one.difference, money("-7").amount());
    assert_eq!(two.difference, money("7").amount());
    assert!(one
        .other_postings
        .iter()
        .any(|p| p.owner_module.is_none() && p.balance == money("7").amount()));
    assert!(!tied.tied_out);
    assert_eq!(tied.total_difference, money("0").amount());
    ledger
        .reverse_entry(
            &admin,
            1,
            manual.id,
            &Reversal {
                entry_date: date(2026, 10, 8),
                description: "R2 restore controls".into(),
                reference: None,
            },
        )
        .await?;
    let restored = ar
        .tie_out_control_accounts(&admin, 1, date(2026, 10, 8), &Default::default())
        .await?;
    assert!(restored
        .accounts
        .iter()
        .filter(|a| [control1, control2].contains(&a.account_id))
        .all(|a| a.difference.is_zero() && a.subledger_balance.is_zero()));
    println!(
        "settlement: inactive original control retained at zero; multi-control receipts; per-account ±7 differences exposed despite zero net"
    );

    let mut cursor = None;
    let mut issued = Vec::new();
    loop {
        let page = ar.list_issued_documents(&admin, 1, Some(1), cursor).await?;
        if page.is_empty() {
            break;
        }
        let d = &page[0];
        let number = d.ledger_entry_number.expect("issued pin");
        assert!(cursor.is_none_or(|c| number > c));
        cursor = Some(number);
        issued.push(d.id);
    }
    for id in [first.id, second.id, closed.id, note.id] {
        assert!(issued.contains(&id));
    }
    let mut cursor = None;
    let mut payments = Vec::new();
    loop {
        let page = ar
            .list_payments(
                &admin,
                1,
                &PaymentQuery {
                    customer_id: Some(customer.id),
                    page: Page {
                        limit: Some(1),
                        after: cursor,
                    },
                },
            )
            .await?;
        if page.is_empty() {
            break;
        }
        assert!(cursor.is_none_or(|c| page[0].id > c));
        cursor = Some(page[0].id);
        payments.push(page[0].payment_number);
    }
    assert_eq!(payments, vec![1, 2, 3, 4, 5]);
    assert!(matches!(
        ar.get_payment(&admin, 1, i64::MAX).await,
        Err(R::PaymentNotFound)
    ));
    assert!(matches!(
        ar.get_document(&admin, 1, i64::MAX).await,
        Err(R::DocumentNotFound)
    ));
    assert!(ar.get_payment(&other, 1, paid.id).await.is_err());
    for limit in [0, 501] {
        assert!(matches!(
            ar.list_issued_documents(&admin, 1, Some(limit), None).await,
            Err(R::InvalidOption)
        ));
        assert!(matches!(
            ar.list_payments(
                &admin,
                1,
                &PaymentQuery {
                    page: Page {
                        limit: Some(limit),
                        after: None
                    },
                    customer_id: None
                }
            )
            .await,
            Err(R::InvalidOption)
        ));
    }
    assert!(matches!(
        ar.list_issued_documents(&admin, 1, None, Some(-1)).await,
        Err(R::InvalidOption)
    ));
    for buckets in [vec![], vec![0], vec![30, 30], vec![60, 30], vec![36501]] {
        assert!(matches!(
            ar.aged_receivables(
                &admin,
                1,
                date(2026, 10, 1),
                &AgingOptions {
                    bucket_days: buckets,
                    through_opening_batch_id: None,
                    through_settlement_number: None,
                    through_entry_number: None
                }
            )
            .await,
            Err(R::InvalidOption)
        ));
    }
    assert!(matches!(
        ar.tax_totals(
            &admin,
            1,
            &TaxOptions {
                from: Some(date(2026, 10, 2)),
                to: Some(date(2026, 10, 1)),
                through_entry_number: None
            }
        )
        .await,
        Err(R::InvalidOption)
    ));
    assert!(matches!(
        ar.tax_totals(
            &admin,
            1,
            &TaxOptions {
                through_entry_number: Some(-1),
                ..Default::default()
            }
        )
        .await,
        Err(R::InvalidOption)
    ));
    for months in [0, 121] {
        assert!(matches!(
            ar.received_tax_totals(
                &admin,
                1,
                &ReceivedTaxOptions {
                    from: date(2026, 10, 1),
                    to: date(2026, 10, 2),
                    unpaid_months: months,
                    through_entry_number: None
                }
            )
            .await,
            Err(R::InvalidOption)
        ));
    }
    let pinned_tax = ar
        .tax_totals(
            &admin,
            1,
            &TaxOptions {
                from: Some(date(2026, 10, 1)),
                to: Some(date(2026, 10, 31)),
                through_entry_number: Some(pin),
            },
        )
        .await?;
    assert!(pinned_tax.iter().all(|r| r.tax_code_id != tax.id));
    let mut pinned_receipts = ReceivedTaxOptions::new(date(2026, 9, 10), date(2026, 9, 30));
    pinned_receipts.through_entry_number = Some(pin);
    assert!(ar
        .received_tax_totals(&admin, 1, &pinned_receipts)
        .await?
        .iter()
        .all(|r| r.tax_code_id != tax.id));
    for (day, bucket) in [(date(2026, 9, 15), 0), (date(2026, 9, 16), 1)] {
        let aging = ar
            .aged_receivables(&admin, 1, day, &AgingOptions::default())
            .await?;
        let doc = aging
            .customers
            .iter()
            .flat_map(|c| &c.documents)
            .find(|d| d.document_id == first.id)
            .expect("due boundary");
        assert_eq!(doc.bucket, bucket);
        assert_eq!(doc.days_overdue, bucket as i64);
    }

    // Competing receipts cannot both consume the same outstanding balance;
    // competing reversals cannot restore it twice.
    let race = ar
        .create_draft(&admin, 1, &invoice(Some("2026-11-01"), "10"))
        .await?;
    let race = ar.issue_document(&admin, 1, race.id).await?;
    let receipt = payment("2026-11-02", "7", allocation(race.id, "7"));
    let (a, b) = tokio::join!(
        ar.record_payment(&admin, 1, &receipt),
        ar.record_payment(&admin, 1, &receipt)
    );
    let winner = match (a, b) {
        (Ok(p), Err(R::Invalid(_))) | (Err(R::Invalid(_)), Ok(p)) => p,
        other => panic!("competing settlements: {other:?}"),
    };
    assert_eq!(
        ar.get_document(&admin, 1, race.id).await?.outstanding,
        Some(money("4"))
    );
    let reversal = json!({"reversal_date":"2026-11-03", "reason":"R2 concurrent reversal"});
    let (a, b) = tokio::join!(
        ar.reverse_payment(&admin, 1, winner.id, &reversal),
        ar.reverse_payment(&admin, 1, winner.id, &reversal)
    );
    assert!(matches!(
        (a, b),
        (Ok(_), Err(R::AlreadyReversed)) | (Err(R::AlreadyReversed), Ok(_))
    ));
    assert_eq!(
        ar.get_document(&admin, 1, race.id).await?.outstanding,
        Some(money("11"))
    );

    let fractional = ar
        .create_draft(&admin, 1, &invoice(Some("2026-11-04"), "1"))
        .await?;
    let fractional = ar.issue_document(&admin, 1, fractional.id).await?;
    ar.record_payment(
        &admin,
        1,
        &payment("2026-11-05", "0.01", allocation(fractional.id, "0.01")),
    )
    .await?;
    let fractions = ar
        .received_tax_totals(
            &admin,
            1,
            &ReceivedTaxOptions::new(date(2026, 11, 5), date(2026, 11, 5)),
        )
        .await?;
    let fraction = fractions
        .iter()
        .find(|r| r.tax_code_id == tax.id && r.measure == "received")
        .expect("unrounded fraction");
    assert!(fraction
        .taxable_amount
        .to_string()
        .starts_with("0.00909090909090909"));
    assert!(fraction
        .tax_amount
        .to_string()
        .starts_with("0.000909090909090909"));
    println!(
        "boundaries: due-today bucket zero; tax entry pins; competing settlements/reversals serialize; received-tax fractions stay unrounded"
    );
    let datasets = Receivables::evidence_datasets();
    assert_eq!(datasets.len(), 21);
    for dataset in datasets {
        let mut after = None;
        let mut count = 0;
        loop {
            let page = ar.evidence_page(&admin, 1, dataset, Some(1), after).await?;
            for row in &page.rows {
                assert_eq!(row.len(), page.fields.len());
                let id = row[0].as_i64().expect("evidence id");
                assert!(after.is_none_or(|last| id > last));
                let tenant = page
                    .fields
                    .iter()
                    .position(|f| *f == "tenant_id")
                    .expect("tenant");
                let company = page
                    .fields
                    .iter()
                    .position(|f| *f == "company_id")
                    .expect("company");
                assert_eq!(row[tenant], json!(1));
                assert_eq!(row[company], json!(1));
                count += 1;
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert!(
            count > 0
                || matches!(
                    dataset,
                    "write_off"
                        | "write_off_event"
                        | "opening_stage"
                        | "opening_decision"
                        | "opening_batch"
                        | "opening_item"
                        | "sales_order"
                        | "sales_order_part"
                        | "settlement"
                        | "receipt_credit"
                ),
            "empty evidence {dataset}"
        );
        println!("evidence {dataset}: {count} rows, exclusive one-row pages");
    }
    for (dataset, limit, after) in [
        ("unknown", 1, 0),
        ("payment", 0, 0),
        ("payment", 501, 0),
        ("payment", 1, -1),
    ] {
        assert!(matches!(
            ar.evidence_page(&admin, 1, dataset, Some(limit), Some(after))
                .await,
            Err(R::InvalidOption)
        ));
    }

    // Every hostile statement lives in a transaction rolled back even if a
    // guard regresses: never let a proof corrupt the subsequent app fixture.
    let mut raw = db.get().await?;
    for (table, predicate, column) in [
        (
            "finance_receivables_payments",
            format!("id={}", paid.id),
            "description",
        ),
        (
            "finance_receivables_allocations",
            format!("payment_id={}", paid.id),
            "amount",
        ),
        (
            "finance_receivables_payment_reversals",
            format!("payment_id={}", paid.id),
            "reason",
        ),
        (
            "finance_receivables_documents",
            format!("id={}", first.id),
            "description",
        ),
    ] {
        for statement in [
            format!(
                "UPDATE {table} SET {column}={column} WHERE tenant_id=1 AND company_id=1 AND {predicate}"
            ),
            format!("DELETE FROM {table} WHERE tenant_id=1 AND company_id=1 AND {predicate}"),
        ] {
            let tx = raw.transaction().await?;
            let result = tx.batch_execute(&statement).await;
            tx.rollback().await?;
            assert!(result.is_err(), "SQL guard missing: {statement}");
        }
    }
    // Row-for-row audit coverage, not merely a nonzero aggregate count.
    for (table, kind) in [
        ("finance_receivables_payments", "payment"),
        ("finance_receivables_allocations", "allocation"),
        ("finance_receivables_payment_reversals", "payment_reversal"),
        ("finance_receivables_documents", "document"),
        ("finance_receivables_document_lines", "document_line"),
        (
            "finance_receivables_document_tax_lines",
            "document_tax_line",
        ),
        ("finance_receivables_customers", "customer"),
        ("finance_receivables_tax_codes", "tax_code"),
        ("finance_receivables_setups", "setup"),
        ("finance_receivables_setup_changes", "setup_change"),
        ("finance_receivables_series", "series"),
    ] {
        let missing:i64=raw.query_one(&format!("SELECT count(*) FROM {table} r WHERE r.tenant_id=1 AND r.company_id=1 AND NOT EXISTS (SELECT 1 FROM base_audit_mutations a WHERE a.tenant_id=r.tenant_id AND a.auditable_type=$1 AND a.auditable_id=r.id::text)"),&[&format!("finance.receivables.{kind}")]).await?.get(0);
        assert_eq!(missing, 0, "unaudited {table}");
    }
    let waiting = ar
        .create_draft(&admin, 1, &invoice(Some("2026-10-08"), "1"))
        .await?;
    // An archived invoicing company keeps its records readable and refuses
    // every write, issuing included.
    raw.batch_execute("UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1")
        .await?;
    let refused = ar.record_payment(&admin, 1, &valid).await;
    let not_issued = ar.issue_document(&admin, 1, waiting.id).await;
    let readable = ar.get_payment(&admin, 1, paid.id).await;
    raw.batch_execute("UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1")
        .await?;
    assert!(matches!(refused, Err(R::CompanyArchived)));
    assert!(
        matches!(not_issued, Err(R::CompanyArchived)),
        "{not_issued:?}"
    );
    assert!(readable.is_ok());
    companies
        .archive_company(&admin, party.id, Some("Walkthrough: customer archived"))
        .await?;
    field(
        ar.issue_document(&admin, 1, waiting.id).await,
        "customer_id",
        "This company is archived, so it cannot be invoiced",
    );
    ar.reverse_payment(
        &admin,
        1,
        settled.id,
        &json!({"reversal_date":"2026-10-08","reason":"Archived customer proof"}),
    )
    .await?;
    let archived_payment = ar
        .record_payment(
            &admin,
            1,
            &payment("2026-10-09", "1", json!([allocation(first.id, "1")])),
        )
        .await?;
    let archived_credit = ar
        .create_draft(&admin, 1, &credit("2026-10-10", "1"))
        .await?;
    ar.issue_document(&admin, 1, archived_credit.id).await?;
    ar.reverse_payment(
        &admin,
        1,
        archived_payment.id,
        &json!({"reversal_date":"2026-10-11","reason":"Archived customer proof"}),
    )
    .await?;
    println!(
        "NR2 proof passed: numbering and rollback, immutable owned postings, dated/pinned reports, all 13 exports, SQL guards, row audits, tenant isolation, archive read-only (company restored active)"
    );
    Ok(())
}
