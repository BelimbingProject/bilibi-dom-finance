//! NC3's disposable UI proof. Run on an empty database with DATABASE_URL,
//! APP_ORIGIN and BIND_ADDR set. The fixture owns its records and storage;
//! no product seed or route is added. Its CSV may also be uploaded by hand.
use bilibi_database::{pool, Error};
use bilibi_finance_attachments::{Attachments, NewFile};
use bilibi_finance_ledger::{AccountType, Ledger, NewAccount};
use bilibi_finance_reconciliation::{
    NewBankAccount, NewReconciliation, Reconciliation, ReconciliationError,
};
use bilibi_finance_walkthrough_fixture::{
    attachments::configure_storage, install, sign_in, COMPANY,
};
use serde_json::json;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN")?;
    let bind = std::env::var("BIND_ADDR")?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    let installed = install(&db, services).await?;
    // Same disposable affiliation facts as the attachments walkthrough.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Viewer','human','lee@example.test'),(5,1,'person_5','Sam Staff','human','sam@example.test') ON CONFLICT DO NOTHING;
        INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','viewer@example.test'),(4,1,5,1,'Clerk','staff@example.test') ON CONFLICT DO NOTHING;
        UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
        UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=3 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
        SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1));
        SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1))").await?;
    let root = std::env::var("NC3_STORAGE_ROOT")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let admin = sign_in(&db, services, 1, 4).await?;
    configure_storage(&db, services, &admin, &root).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let other = sign_in(&db, services, 2, 5).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let cash = installed
        .accounts
        .iter()
        .find(|a| a.code == "1000")
        .ok_or(Error::NotFound)?
        .id;
    let charges = installed
        .accounts
        .iter()
        .find(|a| a.code == "6000")
        .ok_or(Error::NotFound)?
        .id;
    let reserve_ledger = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("1810", "Reserve bank", AccountType::Asset),
        )
        .await?
        .id;
    let interest = ledger
        .create_account(
            &manager,
            COMPANY,
            &NewAccount::new("7000", "Interest income", AccountType::Income),
        )
        .await?
        .id;
    let facade = services.get::<Reconciliation>(bilibi_finance_reconciliation::SERVICE)?;
    let account=facade.create_bank_account(&manager,COMPANY,&NewBankAccount{name:"Operating account".into(),ledger_account_id:cash,charges_account_id:Some(charges),interest_account_id:Some(interest),match_window_days:None,active:None,reason:None,import_mapping:json!({"date":"Date","description":"Details","reference":"Ref","amount":"Amount","balance":"Balance"})}).await?;
    let csv=b"Date,Details,Ref,Amount,Balance\n2026-10-01,Deposit,DEP1,100.00,1100.00\n2026-10-02,Bank charge,FEE1,-5.00,1095.00\n2026-10-03,No movement,ZERO,0.00,1095.00\n";
    let file = NewFile {
        file_name: "opening-statement.csv".into(),
        content: csv.to_vec(),
        content_type: Some("text/csv".into()),
    };
    let imported = facade
        .import_statement(&manager, COMPANY, account.id, &file)
        .await?;
    assert_eq!(imported.statement.line_count, 2);
    assert_eq!(imported.skipped_count, 1);
    assert!(matches!(
        facade
            .import_statement(&manager, COMPANY, account.id, &file)
            .await,
        Err(ReconciliationError::DuplicateFile { .. })
    ));
    let open = || NewReconciliation {
        end_date: Some(jiff::civil::date(2026, 10, 3)),
        statement_balance: "1095.00".into(),
    };
    assert!(matches!(
        facade
            .open_reconciliation(&viewer, COMPANY, account.id, &open())
            .await,
        Err(ReconciliationError::Unauthorized)
    ));
    assert!(matches!(
        facade
            .open_reconciliation(&other, COMPANY, account.id, &open())
            .await,
        Err(ReconciliationError::Unauthorized | ReconciliationError::CompanyNotFound)
    ));
    assert!(matches!(
        facade
            .open_reconciliation(
                &manager,
                COMPANY,
                account.id,
                &NewReconciliation {
                    end_date: open().end_date,
                    statement_balance: "1.001".into()
                }
            )
            .await,
        Err(ReconciliationError::Invalid(_))
    ));
    // A separate account proves open/duplicate-open without pre-filling the
    // account used for browser opening and latest-statement suggestions.
    let reserve = facade
        .create_bank_account(
            &manager,
            COMPANY,
            &NewBankAccount {
                name: "Reserve account".into(),
                ledger_account_id: reserve_ledger,
                charges_account_id: None,
                interest_account_id: None,
                match_window_days: None,
                active: None,
                reason: None,
                import_mapping: json!({"date":"Date","description":"Details","amount":"Amount"}),
            },
        )
        .await?;
    let attributes = open();
    let (first, second) = tokio::join!(
        facade.open_reconciliation(&manager, COMPANY, reserve.id, &attributes),
        facade.open_reconciliation(&manager, COMPANY, reserve.id, &attributes)
    );
    let opened = match (first, second) {
        (Ok(record), Err(ReconciliationError::Invalid(_)))
        | (Err(ReconciliationError::Invalid(_)), Ok(record)) => record,
        outcomes => return Err(format!("concurrent opening outcomes: {outcomes:?}").into()),
    };
    assert_eq!(opened.status.as_str(), "open");
    assert_eq!(
        facade
            .list_reconciliations(&manager, COMPANY, reserve.id)
            .await?
            .len(),
        1
    );
    let attachments = services.get::<Attachments>(bilibi_finance_attachments::SERVICE)?;
    let source = attachments
        .list_company_attachments(
            &manager,
            COMPANY,
            &bilibi_finance_attachments::AttachmentQuery {
                source: Some(bilibi_finance_reconciliation::STATEMENT_SOURCE.into()),
                document_ids: Some(vec![imported.statement.id]),
                ..Default::default()
            },
        )
        .await?;
    let downloaded = attachments
        .read_attachment(&manager, COMPANY, source[0].id)
        .await?;
    assert_eq!(downloaded.bytes, csv);
    // Exercise the real mounted page's upload rendering, including its
    // owner refresh after the host refuses a file before receiving bytes.
    let pages = services.get::<bilibi_ui::page::PageService<bilibi_tenancy::Scope, Error>>(
        "finance/reconciliation.pages",
    )?;
    let path = std::collections::HashMap::from([("id".into(), account.id.to_string())]);
    // The upload control is in the Import statement dialog, which `?do=import`
    // opens on load.
    let query = std::collections::HashMap::from([("do".into(), "import".into())]);
    let mut mounted = pages
        .0
        .mount(bilibi_ui::page::PageRequest {
            operation: "account",
            scope: Some(&manager),
            path: &path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .ok_or(Error::Internal)?;
    let mut next = 0;
    for number in 1..=2 {
        let name = format!("oversize-{number}.csv");
        let registered = mounted
            .uploads()
            .ok_or(Error::Internal)?
            .register(
                "statement",
                vec![bilibi_ui::ClientEntry {
                    client_ref: number.to_string(),
                    name: name.clone(),
                    size: 5 * 1024 * 1024 + 1,
                    content_type: "text/csv".into(),
                }],
                &mut next,
            )
            .ok_or(Error::Internal)?;
        assert!(matches!(
            registered.as_slice(),
            [bilibi_ui::Registration::Refused {
                error: bilibi_ui::UploadError::TooLarge,
                ..
            }]
        ));
        let body = mounted
            .update(bilibi_ui::page::LiveRequest {
                scope: &manager,
                credential: "",
                input: bilibi_ui::page::LiveInput::Refresh,
            })
            .await?;
        assert!(body.html.contains(&name));
        assert!(body
            .html
            .contains("The file is larger than this page accepts."));
        assert_eq!(
            mounted
                .uploads()
                .ok_or(Error::Internal)?
                .entries("statement")
                .next()
                .ok_or(Error::Internal)?
                .received(),
            0
        );
    }
    println!("NC3 mounted upload proof passed: oversized filenames and per-entry refusal render twice before receiving any bytes.");
    println!("NC3 proof passed: import/skipped rows, duplicate file, source bytes, permissions, tenant isolation, Money precision, opening and duplicate-open refusal.");
    println!(
        "Bank account page: {origin}/finance/reconciliation/accounts/{}",
        account.id
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
