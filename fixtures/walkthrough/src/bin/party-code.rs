//! Fixed Core codes across Company/Person customers and suppliers on a fresh DB.
//! PARTY_CODE_BROWSER=1 keeps the proved app available for a browser walkthrough.
use bilibi_database::pool;
use bilibi_finance_payables::{Payables, VendorInput};
use bilibi_finance_receivables::{AgingOptions, Receivables};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use jiff::civil::date;
use serde_json::json;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.party-code",bilibi_module_registry::Provenance {module:"finance/receivables",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution {domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[bilibi_authz::RoleDefinition{code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_receivables::CAPABILITIES}]}) }
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://127.0.0.1:45479".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let restore = std::env::args().any(|arg| arg == "--restore-identities");
    if restore {
        // Identity-fixture SQL only, guarded by this proof's issued invoice.
        // The launcher retires known demo users; no business rows are changed.
        let client = db.get().await?;
        let proof:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM finance_receivables_documents WHERE tenant_id=1 AND company_id=1 AND party_code='jamie_tv' AND status='issued')",&[]).await?.get(0);
        if !proof {
            return Err(
                "restore identities requires the completed disposable party-code proof".into(),
            );
        }
        client.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(1,1,'ada_manager','Ada Manager','human','manager@example.test') ON CONFLICT DO NOTHING;
            INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(1,1,1,1,'Manager') ON CONFLICT DO NOTHING;
            INSERT INTO user_identities(kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) SELECT 'staff','manager@example.test',i.password_hash,1,1,1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE i.email='ai@agent.my' ON CONFLICT DO NOTHING; INSERT INTO users(id,tenant_id,name,identity_id,person_id) SELECT 1,1,'Ada Manager',id,1 FROM user_identities WHERE home_tenant_id=1 AND home_user_id=1 ON CONFLICT DO NOTHING; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=1 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;").await?;
    }
    let installed = install(&db, &app.services).await?;
    if std::env::args().any(|arg| arg == "--evidence") {
        let scope = sign_in(&db, &app.services, 1, 4).await?;
        let ar = app
            .services
            .get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
        let ap = app
            .services
            .get::<Payables>(bilibi_finance_payables::SERVICE)?;
        for dataset in Receivables::evidence_datasets() {
            ar.evidence_page(&scope, 1, dataset, Some(500), None)
                .await
                .map_err(|e| format!("Receivables {dataset}: {e:?}"))?;
        }
        for dataset in ap.evidence_datasets() {
            ap.evidence_page(&scope, 1, dataset, Some(500), None)
                .await
                .map_err(|e| format!("Payables {dataset}: {e:?}"))?;
        }
        let storage = std::env::var("PARTY_CODE_STORAGE_ROOT")?;
        bilibi_finance_walkthrough_fixture::attachments::configure_storage(
            &db,
            &app.services,
            &scope,
            &storage,
        )
        .await?;
        let reports = app
            .services
            .get::<bilibi_finance_reports::Reports>(bilibi_finance_reports::SERVICE)?;
        let artifact = reports
            .export_auditor_pack(&scope, 1, date(2026, 10, 30))
            .await?;
        let pack = reports.read_auditor_pack(&scope, 1, &artifact.id).await?;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(pack.bytes))?;
        use std::io::Read;
        for name in ["receivables-customer.csv", "payables-vendor.csv"] {
            let mut csv = String::new();
            zip.by_name(name)?.read_to_string(&mut csv)?;
            assert!(csv.contains("jamie_tv"), "Core code missing from {name}");
        }
        println!(
            "PASS: every Receivables/Payables dataset and complete auditor pack carry Core codes"
        );
        return Ok(());
    }
    // The normal launcher retires demo identities at startup. Restore only the
    // walkthrough installation after that seed, without replaying Finance writes.
    if restore {
        println!("PASS: walkthrough identities restored for launcher browser proof");
        return Ok(());
    }
    let scope = sign_in(&db, &app.services, 1, 1).await?;
    let ar = app
        .services
        .get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let ap = app
        .services
        .get::<Payables>(bilibi_finance_payables::SERVICE)?;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .unwrap()
            .id
    };
    ar.configure(
        &scope,
        1,
        &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")}),
    )
    .await?;
    let series = ar.create_series(&scope,1,&json!({"code":"TV","name":"Retail invoices","kind":"invoice","prefix":"TV-","number_width":4})).await?;
    let company = ar
        .create_customer(&scope, 1, &json!({"record_kind":"company","record_id":10}))
        .await?;
    // The agent door composes Core Person creation in the same audited write.
    let registry = app
        .services
        .get::<bilibi_agent_api::Registry>("base/agent_api.registry")?;
    let key = "finance.receivables.customer.create";
    let bilibi_agent_api::Handler::SafeWrite(create) = registry.operations[key].definition.handler
    else {
        panic!("customer create must be reviewed")
    };
    let input = json!({"company_id":1,"new_person":{"name":"Jamie TV Consumer","kind":"human","email":"jamie@example.test","phone":"+60123456789"},"payment_terms_days":30});
    let boundary = app
        .services
        .get::<bilibi_database::MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            scope.mutation_context("walkthrough.party-code"),
        )
        .await?;
    let review = create
        .preview(tx.sql(), &scope, &input)
        .await?
        .envelope(key);
    assert_eq!(
        review["result"]["changes"]["target"]["creation"]["code"],
        "jamie_tv"
    );
    let answer = create.commit(&mut tx, &scope, &input).await?.envelope(key);
    let consumer_id = answer["result"]["record"]["id"]
        .as_i64()
        .expect("created customer");
    tx.commit().await?;
    drop(client);
    let consumer = ar.get_customer(&scope, 1, consumer_id).await?;
    assert_eq!(company.code, company.party.code);
    assert_eq!(consumer.code, consumer.party.code);
    assert_eq!(consumer.party.kind, "person");
    // A taken Finance reference refuses the write. Codes are never Finance input.
    assert!(ar
        .create_customer(
            &scope,
            1,
            &json!({"record_kind":"person","record_id":consumer.party.id})
        )
        .await
        .is_err());
    assert!(ar
        .update_customer(&scope, 1, consumer.id, &json!({"code":"OTHER"}))
        .await
        .is_err());
    assert!(ar
        .update_customer(
            &scope,
            1,
            consumer.id,
            &json!({"record_kind":"company","record_id":11})
        )
        .await
        .is_err());
    let vendor_input = |kind: &str, id| VendorInput {
        record_kind: Some(kind.into()),
        record_id: Some(id),
        new_company: None,
        new_person: None,
        payment_terms_days: 30,
        payable_account_id: None,
        expense_account_id: None,
        tax_code_id: None,
        active: true,
    };
    let supplier = ap
        .create_vendor(&scope, 1, &vendor_input("company", 11))
        .await?;
    // The same real person may be both a customer and supplier, with one code.
    let person_supplier = ap
        .create_vendor(&scope, 1, &vendor_input("person", consumer.party.id))
        .await?;
    assert_eq!(person_supplier.code, consumer.code);
    assert_eq!(supplier.code, supplier.party.code);
    let draft = ar.create_draft(&scope,1,&json!({"kind":"invoice","series_id":series.id,"customer_id":consumer.id,"issue_date":"2026-10-30","lines":[{"description":"Television","quantity":"1","unit_price":"1200"}]})).await?;
    let issued = ar.issue_document(&scope, 1, draft.id).await?;
    assert_eq!(issued.party_code.as_deref(), Some(consumer.code.as_str()));
    assert!(issued.ledger_entry_id.is_some());
    let aging = ar
        .aged_receivables(&scope, 1, date(2026, 10, 30), &AgingOptions::default())
        .await?;
    let aged = aging
        .customers
        .iter()
        .find(|c| c.customer_id == consumer.id)
        .expect("consumer in aging");
    assert_eq!(aged.code, consumer.code);
    let customer_evidence = ar
        .evidence_page(&scope, 1, "customer", Some(500), None)
        .await?;
    let code_at = customer_evidence
        .fields
        .iter()
        .position(|f| *f == "code")
        .unwrap();
    assert!(customer_evidence
        .rows
        .iter()
        .any(|r| r[code_at] == consumer.code));
    let supplier_evidence = ap
        .evidence_page(&scope, 1, "vendor", Some(500), None)
        .await?;
    assert!(supplier_evidence
        .rows
        .iter()
        .any(|r| r["code"] == consumer.code));
    println!(
        "PASS: company customer {} code {}; person customer {} code {}; company supplier {} code {}; person supplier shares {}; TV invoice {} posted and aging carries {}",
        company.id,
        company.code,
        consumer.id,
        consumer.code,
        supplier.id,
        supplier.code,
        person_supplier.code,
        issued.id,
        aged.code
    );
    // Privacy companion: a partial Core contact is not an authentic Finance
    // fact. Summaries withhold it, and issuing refuses to freeze it. Exercise
    // email and phone independently, then the same draft with both visible.
    let admin = sign_in(&db, &app.services, 1, 4).await?;
    let restrictions = app
        .services
        .get::<bilibi_authz::restrictions::FieldRestrictions>("base/authz.field_restrictions")?;
    for field in ["email", "phone"] {
        restrictions
            .put_mode(
                &admin,
                "persons",
                field,
                &[1],
                bilibi_authz::restrictions::Mode::Mask,
            )
            .await?;
    }
    let masked = ar.get_customer(&scope, 1, consumer.id).await?;
    assert!(masked.party.email.is_none() && masked.party.phone.is_none());
    assert!(
        masked.party.restricted.contains(&"email".into())
            && masked.party.restricted.contains(&"phone".into())
    );
    let masked_draft = ar.create_draft(&scope,1,&json!({"kind":"invoice","series_id":series.id,"customer_id":consumer.id,"issue_date":"2026-10-30","lines":[{"description":"Contact privacy control","quantity":"1","unit_price":"17"}]})).await?;
    let email_error = ar
        .issue_document(&scope, 1, masked_draft.id)
        .await
        .expect_err("masked email must not freeze");
    assert!(format!("{email_error:?}").contains("email is restricted"));
    restrictions.remove(&admin, "persons", "email").await?;
    let phone_error = ar
        .issue_document(&scope, 1, masked_draft.id)
        .await
        .expect_err("masked phone must not freeze");
    assert!(format!("{phone_error:?}").contains("phone is restricted"));
    restrictions.remove(&admin, "persons", "phone").await?;
    let visible = ar.issue_document(&scope, 1, masked_draft.id).await?;
    assert!(visible.ledger_entry_id.is_some());
    println!(
        "PASS: masked email and phone withheld in PartyLinks; each refuses document freeze; unmasked control issued"
    );
    // Optional printing proof, on this same disposable fixture installation.
    if let Ok(root) = std::env::var("INVOICE_LAYOUT_PROOF_DIR") {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        bilibi_finance_walkthrough_fixture::attachments::configure_storage(
            &db,
            &app.services,
            &admin,
            &root,
        )
        .await?;
        if let Ok(binary) = std::env::var("INVOICE_LAYOUT_CHROMIUM") {
            let settings = app
                .services
                .get::<bilibi_settings::Settings>("base/settings.store")?;
            let mut client = db.get().await?;
            let mut tx = boundary
                .begin(
                    &mut client,
                    admin.mutation_context("walkthrough.invoice-layout.configure"),
                )
                .await?;
            settings
                .put(
                    &mut tx,
                    &admin,
                    bilibi_settings::Target::Global,
                    "pdf.chromium.binary",
                    json!(binary),
                )
                .await?;
            tx.commit().await?;
        }
        let default_pdf = ar.print_document(&scope, 1, issued.id).await?;
        assert!(default_pdf.bytes.starts_with(b"%PDF-"));
        std::fs::write(format!("{root}/default.pdf"), default_pdf.bytes)?;
        assert!(ar
            .print_document(&scope, 1, masked_draft.id + 99999)
            .await
            .is_err());
        let bad = ar
            .configure(
                &scope,
                1,
                &json!({"invoice_layout":{"company_name":"x".repeat(201)}}),
            )
            .await;
        assert!(matches!(
            bad,
            Err(bilibi_finance_receivables::ReceivableError::Invalid(_))
        ));
        ar.configure(&scope, 1, &json!({"invoice_layout":{"company_name":"Example Trading & Services", "registration_number":"Example registration 001", "address":"First floor\nExample Street", "contact":"accounts@example.test", "footer":"Payment terms: 30 days.\nPlease quote the invoice number. <This is plain text.>"}})).await?;
        let configured = ar.print_document(&scope, 1, issued.id).await?;
        std::fs::write(format!("{root}/configured.pdf"), configured.bytes)?;
        let tax = ar.create_tax_code(&scope,1,&json!({"code":"PRINT-TAX","name":"Example tax","rate":"6","account_id":account("2100")})).await?;
        let lines: Vec<_> = (1..=100).map(|n| json!({"description":format!("Service line {n}: a description long enough to wrap on the printed invoice"),"quantity":"1","unit_price":"12.50","tax_code_id":tax.id})).collect();
        let long = ar.create_draft(&scope,1,&json!({"kind":"invoice","series_id":series.id,"customer_id":consumer.id,"issue_date":"2026-10-30","lines":lines})).await?;
        assert!(matches!(
            ar.print_document(&scope, 1, long.id).await,
            Err(bilibi_finance_receivables::ReceivableError::NotIssued)
        ));
        let long = ar.issue_document(&scope, 1, long.id).await?;
        assert_eq!(long.tax_total.display(None), "75.00");
        assert_eq!(long.total.display(None), "1,325.00");
        std::fs::write(
            format!("{root}/long.pdf"),
            ar.print_document(&scope, 1, long.id).await?.bytes,
        )?;
        let notes = ar.create_series(&scope,1,&json!({"code":"PRINT-CREDIT","name":"Credit notes","kind":"credit_note","prefix":"CN-"})).await?;
        let note = ar.create_draft(&scope,1,&json!({"kind":"credit_note","series_id":notes.id,"customer_id":consumer.id,"credited_document_id":issued.id,"issue_date":"2026-10-30","lines":[{"description":"Service credit","quantity":"1","unit_price":"5.00"}]})).await?;
        let note = ar.issue_document(&scope, 1, note.id).await?;
        std::fs::write(
            format!("{root}/credit-note.pdf"),
            ar.print_document(&scope, 1, note.id).await?.bytes,
        )?;
        let other = sign_in(&db, &app.services, 2, 5).await?;
        assert!(ar.print_document(&other, 1, issued.id).await.is_err());
        println!(
            "PASS: default/configured PDFs, 100-line invoice {}, credit note {}, draft/missing/cross-tenant refusals and layout validation; files in {}",
            long.id, note.id, root
        );
    }
    if std::env::var("PARTY_CODE_BROWSER").as_deref() == Ok("1") {
        let listener = tokio::net::TcpListener::bind(
            std::env::var("BIND_ADDR").unwrap_or("127.0.0.1:45479".into()),
        )
        .await?;
        println!("Browser at {origin}; manager@example.test / bilibi-dev");
        axum::serve(listener, bilibi_web::router(app)).await?;
    }
    Ok(())
}
