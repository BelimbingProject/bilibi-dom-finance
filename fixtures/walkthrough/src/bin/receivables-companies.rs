//! Receivables #99 proof with real Core/Finance facades and operation handlers.
//! Use a fresh disposable DATABASE_URL. CUSTOMERS_BROWSER=1 keeps the proved
//! app running at APP_ORIGIN/BIND_ADDR for the browser walkthrough afterwards.
use bilibi_address::{AddressInput, AddressKind, AddressService, AttachmentInput};
use bilibi_agent_api::{Handler, Registry, WriteHandler};
use bilibi_company::CompanyService;
use bilibi_database::{pool, MutationBoundary};
use bilibi_finance_ledger::{Ledger, LedgerError};
use bilibi_finance_receivables::{ReceivableError, Receivables};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_party::{CalendarDate, EffectiveDates, OrganisationId, PartyId};
use serde_json::json;

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"walkthrough.fixture.customer-composition",bilibi_module_registry::Provenance { module:"finance/receivables", source:file!(), line:line!() }, &bilibi_authz::VocabularyContribution {
    domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[
        bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough fixture role",grant_all:false,capabilities:&bilibi_finance_receivables::CAPABILITIES},
    ],
}) }
fn handler<'a>(registry: &'a Registry, key: &str) -> &'a dyn WriteHandler {
    let Handler::SafeWrite(handler) = registry.operations[key].definition.handler else {
        panic!("not a write")
    };
    handler
}
fn day(s: &str) -> CalendarDate {
    s.to_owned().try_into().expect("fixture date")
}
fn refused<T: std::fmt::Debug>(answer: Result<T, ReceivableError>, field: &str, words: &str) {
    // A refusal Receivables decides, or one Core's composed write raises through the ledger.
    let refusals: Vec<(String, String)> = match &answer {
        Err(ReceivableError::Invalid(errors)) => errors
            .iter()
            .map(|e| (e.field.to_string(), e.message.to_string()))
            .collect(),
        Err(ReceivableError::Ledger(LedgerError::Invalid(errors))) => errors
            .iter()
            .map(|e| (e.field.to_string(), e.message.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    assert!(
        refusals
            .iter()
            .any(|(f, m)| f == field && m.contains(words)),
        "{answer:?}"
    );
    println!("PASS: refusal at {field}: {words}");
}
#[tokio::main]
async fn main() -> Outcome<()> {
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or("http://127.0.0.1:43999".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let installed = install(&db, &app.services).await?;
    let manager = sign_in(&db, &app.services, 1, 1).await?;
    let admin = sign_in(&db, &app.services, 1, 4).await?;
    let ar = app
        .services
        .get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let companies = app
        .services
        .get::<CompanyService>(bilibi_company::SERVICE)?;
    let addresses = app
        .services
        .get::<AddressService>(bilibi_address::SERVICE)?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let mutations = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    let ledger = app.services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .unwrap()
            .id
    };
    ar.configure(
        &manager,
        1,
        &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")}),
    )
    .await?;
    let series = ar.create_series(&manager,1,&json!({"code":"CUSTOMER-INV","name":"Customer invoices","kind":"invoice","prefix":"CU-","number_width":4})).await?;
    let create_key = "finance.receivables.customer.create";
    let create = handler(&registry, create_key);
    let input = json!({"company_id":1,"new_company":{"name":"Orchid Atelier","tax_id":"C909090","registration_number":"20269999","email":"finance@orchid.example.test","phone":"+6031234567"},"payment_terms_days":30});
    let mut raw = db.get().await?;
    let mut tx = mutations
        .begin(&mut raw, manager.mutation_context("walkthrough.customer"))
        .await?;
    let preview = create
        .preview(tx.sql(), &manager, &input)
        .await?
        .envelope(create_key);
    assert_eq!(
        preview["result"]["changes"]["target"]["creation"]["name"],
        "Orchid Atelier"
    );
    // Count on the open transaction itself: the preview holds the company lock, so
    // a second pooled connection (`ar.list_customers`) would wait on it forever,
    // and the transaction sees any row the preview wrote, which that one would not.
    let previewed: i64 = tx
        .sql()
        .query_one(
            "SELECT count(*) FROM finance_receivables_customers WHERE company_id=1",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(previewed, 0);
    let answer = create
        .commit(&mut tx, &manager, &input)
        .await?
        .envelope(create_key);
    let customer_id = answer["result"]["record"]["id"].as_i64().unwrap();
    tx.commit().await?;
    let customer = ar.get_customer(&manager, 1, customer_id).await?;
    let company = customer.party.id;
    let owner = PartyId::Organisation(OrganisationId::new(company)?);
    let source = OrganisationId::new(1)?;
    let role = bilibi_company::relationship_role_by_code(&**raw, &manager, "customer")
        .await?
        .unwrap();
    let current = bilibi_company::current_relationship(&**raw, &manager, source, owner, role.id)
        .await?
        .unwrap();
    println!(
        "PASS: operation preview writes nothing; one commit creates company, Finance link and current relationship"
    );

    let settings = app
        .services
        .get::<bilibi_settings::Settings>("base/settings.store")?;
    let mut policy = mutations
        .begin(
            &mut raw,
            manager.mutation_context("walkthrough.duplicate-policy"),
        )
        .await?;
    settings
        .put(
            &mut policy,
            &manager,
            bilibi_settings::Target::Tenant,
            "company.duplicates.on_create",
            json!("require_reason"),
        )
        .await?;
    policy.commit().await?;

    // Duplicate suggestions are Core's own masked review, and its policy
    // refuses a second company unless the matching ids and reason are reviewed.
    let duplicate =
        json!({"company_id":1,"new_company":{"name":"Orchid Atelier","code":"orchid-second"}});
    let tx = mutations
        .begin(&mut raw, manager.mutation_context("walkthrough.duplicate"))
        .await?;
    let review = create
        .preview(tx.sql(), &manager, &duplicate)
        .await?
        .envelope(create_key);
    let candidates = review["result"]["changes"]["target"]["creation"]["duplicate_candidates"]
        .as_array()
        .unwrap();
    assert!(candidates.iter().any(|c| c["id"] == company));
    tx.rollback().await?;
    refused(
        ar.create_customer(&manager, 1, &duplicate).await,
        "new_company.create_anyway",
        "Matching companies exist",
    );
    let mut accepted = duplicate.clone();
    accepted["new_company"]["create_anyway"] = json!({"candidate_ids":candidates.iter().map(|c|c["id"].clone()).collect::<Vec<_>>(),"reason":"Separate legal entity confirmed"});
    ar.create_customer(&manager, 1, &accepted).await?;
    println!("PASS: Core duplicate review and reason survive the composed Finance write");

    // Late audit failure occurs after Core wrote its company and relationship.
    // Every business row and every capture must disappear with the failed write.
    let before: (i64, i64, i64, i64) = {
        let r=raw.query_one("SELECT (SELECT count(*) FROM companies),(SELECT count(*) FROM company_relationships),(SELECT count(*) FROM finance_receivables_customers),(SELECT count(*) FROM base_audit_mutations)",&[]).await?;
        (r.get(0), r.get(1), r.get(2), r.get(3))
    };
    raw.batch_execute("CREATE FUNCTION customer_capture_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.auditable_type='finance.receivables.customer' THEN RAISE EXCEPTION 'customer composition audit fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER customer_capture_fault BEFORE INSERT ON base_audit_mutations FOR EACH ROW EXECUTE FUNCTION customer_capture_fault()").await?;
    let failed = ar
        .create_customer(
            &manager,
            1,
            &json!({"new_company":{"name":"Violet Observatory"}}),
        )
        .await;
    raw.batch_execute("DROP TRIGGER customer_capture_fault ON base_audit_mutations; DROP FUNCTION customer_capture_fault()").await?;
    assert!(failed.is_err());
    let r=raw.query_one("SELECT (SELECT count(*) FROM companies),(SELECT count(*) FROM company_relationships),(SELECT count(*) FROM finance_receivables_customers),(SELECT count(*) FROM base_audit_mutations)",&[]).await?;
    assert_eq!(before, (r.get(0), r.get(1), r.get(2), r.get(3)));
    println!(
        "PASS: late Finance audit failure rolls back company, relationship, customer and all captures"
    );

    // Existing-company linking records a missing relationship in the same write.
    let existing = ar
        .create_customer(
            &manager,
            1,
            &json!({"record_kind":"company","record_id":10}),
        )
        .await?;
    assert_eq!(existing.party.id, 10);
    // A consumer is created in Core in the same transaction as its customer.
    let consumer = ar.create_customer(&manager, 1, &json!({"new_person":{"name":"TV consumer","kind":"human","email":"consumer@example.test","phone":""}})).await?;
    assert_eq!(consumer.party.kind, "person");
    assert_eq!(consumer.code, consumer.party.code);
    assert!(!consumer.code.is_empty());

    let invoice = |date: &str| json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id,"issue_date":date,"lines":[{"description":"Professional services","quantity":"1","unit_price":"100"}]});
    let draft = ar.create_draft(&manager, 1, &invoice("2026-08-31")).await?;
    companies
        .unlink_relationship(&manager, current.id, current.version)
        .await?;
    // A reverse-direction link and out-of-date intervals are not current
    // customer relationships from the invoicing company.
    for (from, to) in [
        (Some("2030-01-01"), None),
        (Some("2025-01-01"), Some("2026-01-01")),
    ] {
        let saved = companies
            .assign_relationship(
                &manager,
                &bilibi_party::RelationshipInput {
                    source_organisation_id: source,
                    target: owner,
                    role_id: role.id,
                    dates: EffectiveDates::new(from.map(day), to.map(day))?,
                },
            )
            .await?;
        assert!(
            matches!(saved, bilibi_company::TypeOutcome::Saved(_)),
            "{saved:?}"
        );
    }
    let saved = companies
        .assign_relationship(
            &manager,
            &bilibi_party::RelationshipInput {
                source_organisation_id: OrganisationId::new(company)?,
                target: PartyId::Organisation(source),
                role_id: role.id,
                dates: EffectiveDates::new(None, None)?,
            },
        )
        .await?;
    assert!(
        matches!(saved, bilibi_company::TypeOutcome::Saved(_)),
        "{saved:?}"
    );
    refused(
        ar.create_draft(&manager, 1, &invoice("2026-08-31")).await,
        "customer_id",
        "no current customer relationship",
    );
    refused(
        ar.issue_document(&manager, 1, draft.id).await,
        "customer_id",
        "no current customer relationship",
    );
    let relation = companies
        .assign_relationship(
            &manager,
            &bilibi_party::RelationshipInput {
                source_organisation_id: source,
                target: owner,
                role_id: role.id,
                dates: EffectiveDates::new(Some(day("2026-01-02")), None)?,
            },
        )
        .await?;
    assert!(
        matches!(relation, bilibi_company::TypeOutcome::Saved(_)),
        "{relation:?}"
    );
    println!(
        "PASS: both new invoice and issue recheck the directed current relationship on the write connection"
    );

    // Billing intervals, kind priority, primary fallback, complete frozen facts.
    // Malaysia and Kuala Lumpur come from the Platform's bundled reference geography.
    let primary = addresses
        .create_and_attach(
            &manager,
            owner,
            &AddressInput {
                line1: Some("Primary office".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                is_primary: true,
                dates: EffectiveDates::new(Some(day("2000-01-01")), Some(day("2099-12-31")))?,
                ..AttachmentInput::default()
            },
        )
        .await?;
    let long_line = "Billing street ".repeat(30).trim_end().to_owned();
    let billing = addresses
        .create_and_attach(
            &manager,
            owner,
            &AddressInput {
                line1: Some(long_line.clone()),
                line2: Some("Level 2".into()),
                line3: Some("Accounts office".into()),
                locality: Some("Kuala Lumpur".into()),
                postcode: Some("50000".into()),
                country_iso: Some("MY".into()),
                admin1_code: Some("MY.14".into()),
                ..AddressInput::default()
            },
            &AttachmentInput {
                kinds: vec![AddressKind::Billing],
                dates: EffectiveDates::new(Some(day("2026-08-01")), Some(day("2026-08-31")))?,
                ..AttachmentInput::default()
            },
        )
        .await?;
    let issue_key = "finance.receivables.document.issue";
    let issue = handler(&registry, issue_key);
    let issue_input = json!({"company_id":1,"document_id":draft.id});
    let mut tx = mutations
        .begin(&mut raw, manager.mutation_context("walkthrough.issue"))
        .await?;
    let preview = issue
        .preview(tx.sql(), &manager, &issue_input)
        .await?
        .envelope(issue_key);
    let party = &preview["result"]["changes"]["after"];
    assert_eq!(party["party_tax_identifier"], "C909090");
    assert_eq!(party["party_registration_identifier"], "20269999");
    assert_eq!(party["party_email"], "finance@orchid.example.test");
    assert_eq!(party["party_phone"], "+6031234567");
    assert_eq!(party["party_line1"], long_line);
    let committed = issue
        .commit(&mut tx, &manager, &issue_input)
        .await?
        .envelope(issue_key);
    for key in [
        "name",
        "tax_identifier",
        "registration_identifier",
        "email",
        "phone",
        "line1",
        "line2",
        "line3",
        "locality",
        "postcode",
        "admin1_code",
        "country_iso",
    ] {
        let key = format!("party_{key}");
        assert_eq!(party[&key], committed["result"]["record"][&key]);
    }
    tx.commit().await?;
    let frozen = ar.get_document(&manager, 1, draft.id).await?;
    assert_eq!(frozen.party_line1.as_deref(), Some(long_line.as_str()));
    println!(
        "PASS: preview equals committed party facts, including >255-character billing line and inclusive end date"
    );
    let fallback = ar.create_draft(&manager, 1, &invoice("2026-09-01")).await?;
    let issued = ar.issue_document(&manager, 1, fallback.id).await?;
    assert_eq!(issued.party_line1.as_deref(), Some("Primary office"));
    println!("PASS: expired billing attachment falls back to primary address");
    let stale_draft = ar.create_draft(&manager, 1, &invoice("2026-08-31")).await?;
    let stale_input = json!({"company_id":1,"document_id":stale_draft.id});
    let review_tx = mutations
        .begin(
            &mut raw,
            manager.mutation_context("walkthrough.changed-billing"),
        )
        .await?;
    issue
        .preview(review_tx.sql(), &manager, &stale_input)
        .await?;
    // The preview holds the company lock until its transaction ends, so the
    // billing change cannot run beside it: end that transaction, change the
    // address, then try to commit on a new one. The review is local to the
    // transaction it was taken on, so the old preview cannot authorise it.
    review_tx.rollback().await?;
    addresses
        .update(
            &manager,
            billing.address.id,
            &bilibi_address::Update {
                version: billing.address.version,
                address: AddressInput {
                    line1: Some("Changed billing office".into()),
                    ..billing.address.address.clone()
                },
            },
        )
        .await?;
    let mut review_tx = mutations
        .begin(
            &mut raw,
            manager.mutation_context("walkthrough.changed-billing-commit"),
        )
        .await?;
    let changed = issue.commit(&mut review_tx, &manager, &stale_input).await;
    assert!(
        matches!(
            changed,
            Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict
            ))
        ),
        "changed billing must require a fresh issue review"
    );
    review_tx.rollback().await?;
    assert_eq!(
        ar.get_document(&manager, 1, stale_draft.id).await?.status,
        "draft"
    );
    println!(
        "PASS: a billing address changed after preview refuses the reviewed issue without posting"
    );
    assert_eq!(
        ar.get_document(&manager, 1, draft.id).await?.party_line1,
        frozen.party_line1
    );
    assert_eq!(
        bilibi_address::billing_address(&**raw, &manager, owner, &day("2026-09-01"))
            .await?
            .unwrap()
            .address
            .id,
        primary.address.id
    );
    println!("PASS: subsequent address changes leave issued snapshots unchanged");
    let mask_draft = ar.create_draft(&manager, 1, &invoice("2026-08-31")).await?;
    grants
        .set_direct(&admin, manager.user(), "admin.address.view", Some(false))
        .await?;
    refused(
        ar.issue_document(&manager, 1, mask_draft.id).await,
        "customer_id",
        "Addresses view permission",
    );
    grants
        .set_direct(&admin, manager.user(), "admin.address.view", None)
        .await?;
    assert_eq!(
        ar.get_document(&manager, 1, mask_draft.id).await?.status,
        "draft"
    );
    assert!(
        ledger
            .get_entry(&manager, 1, frozen.ledger_entry_id.unwrap())
            .await?
            .id
            > 0
    );
    println!("PASS: address-view denial leaves the invoice unissued");
    println!(
        "PASS: all customer composition checks completed; invoice {} customer {}",
        draft.id, customer.id
    );
    drop(raw);
    if std::env::var("CUSTOMERS_BROWSER").as_deref() == Ok("1") {
        let listener = tokio::net::TcpListener::bind(
            std::env::var("BIND_ADDR").unwrap_or("127.0.0.1:43999".into()),
        )
        .await?;
        println!(
            "Customer browser proof at {origin}; manager@example.test uses the walkthrough password."
        );
        axum::serve(listener, bilibi_web::router(app)).await?;
    }
    Ok(())
}
