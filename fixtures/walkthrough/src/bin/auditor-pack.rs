//! NT4 running proof: real facade, snapshot, artifact, download and page.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_attachments::{Attachments, NewFile};
use bilibi_finance_ledger::{Ledger, Money, NewEntry, NewLine, PostOptions, TotalsQuery};
use bilibi_finance_reports::{Issue, Period, Reports, ReportsError};
use bilibi_finance_walkthrough_fixture::{
    attachments::configure_storage, install, records, sign_in, COMPANY,
};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::{DownloadRequest, DownloadService},
    page::{LiveInput, LiveRequest, PageRequest, PageService},
};
use jiff::civil::date;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    os::unix::fs::PermissionsExt,
};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_attachments_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_receivables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payables_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reconciliation_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_einvoice_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payroll_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_reports_roles!();
macro_rules! read_role {
    ($module:literal,$cap:expr) => {
        inventory::submit! { Contribution::new(bilibi_authz::VOCABULARY,concat!("walkthrough.fixture.auditor-pack.",$module),Provenance { module:$module,source:file!(),line:line!() },&bilibi_authz::VocabularyContribution {
            domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough auditor",grant_all:false,capabilities:&[$cap]}],
        }) }
    };
}
read_role!("finance/sst", bilibi_finance_sst::VIEW);
read_role!("finance/fixed_assets", bilibi_finance_fixed_assets::VIEW);
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
async fn setting(
    db: &bilibi_database::Db,
    services: &Services,
    admin: &Scope,
    key: &str,
    value: Value,
) -> Outcome<()> {
    let mut client = db.get().await?;
    let boundary = services.get::<bilibi_database::MutationBoundary>("base/database.mutations")?;
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("auditor-pack.fixture.configure"),
        )
        .await?;
    services
        .get::<Settings>("base/settings.store")?
        .put(&mut tx, admin, Target::Global, key, value)
        .await?;
    tx.commit().await?;
    Ok(())
}
fn entry(debit: i64, credit: i64, amount: Money) -> NewEntry {
    NewEntry {
        entry_date: date(2026, 3, 15),
        description: " \t=auditor,\"quoted\"\nsecond line".into(),
        reference: None,
        lines: vec![
            NewLine::debit(debit, amount),
            NewLine::credit(credit, amount),
        ],
    }
}
fn inspect(bytes: &[u8]) -> Outcome<(Value, HashMap<String, Vec<u8>>)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut files = HashMap::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        files.insert(file.name()?.into_owned(), bytes);
    }
    let manifest: Value = serde_json::from_slice(&files["manifest.json"])?;
    assert_eq!(
        files.len(),
        manifest["files"].as_object().unwrap().len() + 1
    );
    for (name, meta) in manifest["files"].as_object().unwrap() {
        let sha: String = Sha256::digest(&files[name])
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(meta["sha256"], sha, "bad hash {name}");
    }
    assert_eq!(
        manifest["control_totals"]["debits"],
        manifest["control_totals"]["credits"]
    );
    Ok((manifest, files))
}
#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4324".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services = &app.services;
    // Steering must not reinstall fixtures: install also restores their
    // original roles, which would contaminate the restricted-role proof.
    let grants = services.get::<Grants>("base/authz.grants")?;
    if let Some(mode) = std::env::args().nth(1).filter(|s| {
        [
            "deny-source",
            "restore-source",
            "deny-journals",
            "restore-journals",
            "deny-ledger",
            "restore-ledger",
        ]
        .contains(&s.as_str())
    }) {
        let admin = sign_in(&db, services, 1, 4).await?;
        let c = db.get().await?;
        if c.query_opt(
            "SELECT u.id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id=2 AND i.email='viewer@example.test'",
            &[],
        )
        .await?
        .is_none()
        {
            return Err(Error::Invalid.into());
        }
        drop(c);
        grants
            .set_direct(
                &admin,
                if mode.ends_with("source") { 1 } else { 2 },
                if mode.ends_with("journals") {
                    bilibi_finance_ledger::SUBMIT_JOURNALS
                } else if mode.ends_with("ledger") {
                    bilibi_finance_ledger::VIEW
                } else {
                    bilibi_finance_einvoice::VIEW
                },
                mode.starts_with("deny-").then_some(false),
            )
            .await?;
        println!("Browser source-grant steering applied: {mode}");
        return Ok(());
    }
    let installed = install(&db, services).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let other = sign_in(&db, services, 2, 5).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let root = std::env::var("AUDITOR_PACK_PROOF_DIR")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let storage = format!("{root}/artifacts");
    std::fs::create_dir_all(&storage)?;
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700))?;
    match reports
        .export_auditor_pack(&manager, COMPANY, date(2026, 3, 31))
        .await
    {
        Err(ReportsError::Artifact("policy" | "unsafe_root")) => {
            println!("Unconfigured private artifact storage refused")
        }
        other => panic!("Expected unconfigured private storage, received {other:?}"),
    }
    configure_storage(&db, services, &admin, &storage).await?;
    let debit = installed
        .accounts
        .iter()
        .find(|a| a.code == "1000")
        .unwrap()
        .id;
    let credit = installed
        .accounts
        .iter()
        .find(|a| a.code == "4000")
        .unwrap()
        .id;
    let amount = Money::parse("1.23", installed.book.minor_units)?;
    let mut client = db.get().await?;
    let mut tx = ledger
        .write(
            &mut client,
            &manager,
            COMPANY,
            &[bilibi_finance_ledger::POST_ENTRIES],
            "auditor-pack.fixture.entries",
        )
        .await?;
    for _ in 0..502 {
        ledger
            .post_entry_in(
                &mut tx,
                &manager,
                COMPANY,
                &entry(debit, credit, amount),
                &PostOptions::default(),
            )
            .await?;
    }
    tx.commit().await?;
    drop(client);
    let record = records::add(
        &db,
        1,
        COMPANY,
        "open",
        "Auditor source",
        date(2026, 3, 15),
        false,
    )
    .await?;
    let attachment = services
        .get::<Attachments>(bilibi_finance_attachments::SERVICE)?
        .attach(
            &manager,
            COMPANY,
            records::OPEN_SOURCE,
            record,
            &NewFile {
                file_name: "=supporting.csv".into(),
                content: b"a,b\r\n1,2\r\n".to_vec(),
                content_type: Some("text/csv".into()),
            },
        )
        .await?;
    let reconciliation = services.get::<bilibi_finance_reconciliation::Reconciliation>(
        bilibi_finance_reconciliation::SERVICE,
    )?;
    let bank = reconciliation.create_bank_account(&manager, COMPANY, &bilibi_finance_reconciliation::NewBankAccount {
        name: "Auditor bank account".into(), ledger_account_id: debit,
        import_mapping: json!({"date":"Date","description":"Details","reference":"Ref","amount":"Amount","balance":"Balance"}),
        charges_account_id: None, interest_account_id: None, match_window_days: None, active: None, reason: None,
    }).await?;
    reconciliation.import_statement(&manager, COMPANY, bank.id, &NewFile { file_name: "statement.csv".into(), content: b"Date,Details,Ref,Amount,Balance\n2026-03-15,Overdrawn account,REF,-1.23,-1.23\n".to_vec(), content_type: Some("text/csv".into()) }).await?;
    reports
        .issue_report(
            &manager,
            COMPANY,
            &Issue::TrialBalance(date(2026, 3, 31)),
            None,
        )
        .await?;
    // Core facts are included in the evidence snapshot independently of entry pins.
    let ar = services
        .get::<bilibi_finance_receivables::Receivables>(bilibi_finance_receivables::SERVICE)?;
    let customer = ar
        .create_customer(
            &admin,
            COMPANY,
            &json!({"record_kind":"company","record_id":20}),
        )
        .await?;
    let before_link = reports
        .export_auditor_pack(&admin, COMPANY, date(2026, 3, 31))
        .await?;
    let trial_before = ledger
        .trial_balance(&admin, COMPANY, date(2026, 3, 31), None)
        .await?;
    let report_pin = trial_before.through_entry_number;
    let periods = [Period {
        from: date(2026, 3, 1),
        to: date(2026, 3, 31),
    }];
    let profit_before = reports
        .profit_and_loss(&admin, COMPANY, &periods, Some(report_pin))
        .await?;
    let before_bytes = reports
        .read_auditor_pack(&admin, COMPANY, &before_link.id)
        .await?;
    let (_, before_files) = inspect(&before_bytes.bytes)?;
    let before_csv = String::from_utf8(before_files["receivables-customer.csv"].clone())?;
    assert!(before_csv
        .lines()
        .next()
        .unwrap()
        .ends_with("record_kind,record_id"));
    assert!(before_csv
        .lines()
        .any(|row| row.contains(&customer.code) && row.ends_with(",\"company\",\"20\"")));
    ar.update_customer(
        &admin,
        COMPANY,
        customer.id,
        &json!({"payment_terms_days":30}),
    )
    .await?;
    let trial_after = ledger
        .trial_balance(&admin, COMPANY, date(2026, 3, 31), Some(report_pin))
        .await?;
    let profit_after = reports
        .profit_and_loss(&admin, COMPANY, &periods, Some(report_pin))
        .await?;
    assert_eq!(
        serde_json::to_vec(&trial_before)?,
        serde_json::to_vec(&trial_after)?
    );
    assert_eq!(
        serde_json::to_vec(&profit_before)?,
        serde_json::to_vec(&profit_after)?
    );
    let mut client = db.get().await?;
    let snapshot = (**client)
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    let first = ledger
        .evidence_page_on(&snapshot, &manager, COMPANY, "entry", Some(500), None)
        .await?;
    let pin = ledger
        .account_totals_on(&snapshot, &manager, COMPANY, &TotalsQuery::default())
        .await?
        .through_entry_number;
    ledger
        .post_entry(
            &manager,
            COMPANY,
            &entry(debit, credit, amount),
            &PostOptions::default(),
        )
        .await?;
    ledger
        .update_account(
            &manager,
            COMPANY,
            debit,
            &bilibi_finance_ledger::AccountChange {
                name: Some("Changed after snapshot".into()),
                ..Default::default()
            },
        )
        .await?;
    let old_chart = ledger
        .list_accounts_on(&snapshot, &manager, COMPANY)
        .await?;
    assert_ne!(
        old_chart.iter().find(|a| a.id == debit).unwrap().name,
        "Changed after snapshot"
    );
    let rest = ledger
        .evidence_page_on(&snapshot, &manager, COMPANY, "entry", Some(500), first.next)
        .await?;
    assert_eq!(first.rows.len() + rest.rows.len(), 502);
    assert_eq!(
        ledger
            .account_totals_on(&snapshot, &manager, COMPANY, &TotalsQuery::default())
            .await?
            .through_entry_number,
        pin
    );
    snapshot.commit().await?;
    drop(client);
    println!("Snapshot pages exclude a concurrent committed backdated entry");
    // An auditor needs only source views and export, not source-write or issue grants.
    for cap in [
        bilibi_finance_reports::ISSUE,
        bilibi_finance_ledger::POST_ENTRIES,
        bilibi_finance_attachments::CREATE,
    ] {
        grants.set_direct(&admin, 1, cap, Some(false)).await?;
    }
    let artifact = reports
        .export_auditor_pack(&manager, COMPANY, date(2026, 3, 31))
        .await?;
    let pack = reports
        .read_auditor_pack(&manager, COMPANY, &artifact.id)
        .await?;
    let (manifest, files) = inspect(&pack.bytes)?;
    let customer_csv = String::from_utf8(files["receivables-customer.csv"].clone())?;
    assert!(customer_csv
        .lines()
        .any(|row| row.contains(&customer.code) && row.ends_with(",\"company\",\"20\"")));
    assert_eq!(manifest["through_entry_number"], pin + 1);
    for (name, count) in [
        ("ledger-entry.csv", 503),
        ("ledger-line.csv", 1006),
        ("issued-reports.csv", 1),
        ("attachments-attachment.csv", 2),
    ] {
        assert_eq!(manifest["files"][name]["row_count"], count);
    }
    assert_eq!(manifest["files"]["source-file-index.csv"]["row_count"], 1);
    let statements = String::from_utf8(files["reconciliation-statement.csv"].clone())?;
    assert!(statements.contains("-1.2300"));
    assert!(!statements.contains("'-1.2300"));
    let index = String::from_utf8(files["attachments-attachment.csv"].clone())?;
    assert!(index.contains("reconciliation-statement.csv#"));
    assert!(index.contains(&attachment.sha256));
    assert!(index.contains("'=supporting.csv"));
    assert!(String::from_utf8(files["general-ledger.csv"].clone())?.contains("'=auditor"));
    std::fs::write(format!("{root}/auditor-pack.zip"), &pack.bytes)?;
    std::fs::write(
        format!("{root}/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(format!("{root}/artifact-id"), &artifact.id)?;
    for cap in [
        bilibi_finance_reports::ISSUE,
        bilibi_finance_ledger::POST_ENTRIES,
        bilibi_finance_attachments::CREATE,
    ] {
        grants.set_direct(&admin, 1, cap, None).await?;
    }
    println!(
        "{} archive members; hashes, GL totals, multi-page evidence and formula text checked",
        files.len()
    );
    assert!(reports
        .read_auditor_pack(&other, COMPANY, &artifact.id)
        .await
        .is_err());
    assert!(reports
        .read_auditor_pack(&manager, 2, &artifact.id)
        .await
        .is_err());
    assert!(reports
        .export_auditor_pack(&viewer, COMPANY, date(2026, 3, 31))
        .await
        .is_err());
    assert!(reports
        .read_auditor_pack(&manager, COMPANY, "not-an-artifact")
        .await
        .is_err());
    {
        let client = db.get().await?;
        client
            .execute(
                "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1",
                &[],
            )
            .await?;
        let archived_pack = reports
            .export_auditor_pack(&manager, COMPANY, date(2026, 3, 31))
            .await?;
        let archived_bytes = reports
            .read_auditor_pack(&manager, COMPANY, &archived_pack.id)
            .await?;
        inspect(&archived_bytes.bytes)?;
        match ledger
            .post_entry(
                &manager,
                COMPANY,
                &entry(debit, credit, amount),
                &PostOptions::default(),
            )
            .await
        {
            Err(bilibi_finance_ledger::LedgerError::CompanyArchived) => {
                println!("Archived company: pack generated and read, posting refused")
            }
            other => panic!("Expected CompanyArchived for a posting, received {other:?}"),
        }
        client
            .execute(
                "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1",
                &[],
            )
            .await?;
    }
    let downloads = services.get::<DownloadService<Scope, Error>>("finance/reports.downloads")?;
    let paths = HashMap::from([("artifact_id".into(), artifact.id.clone())]);
    let empty = HashMap::new();
    let file = downloads
        .0
        .serve(DownloadRequest {
            operation: "auditor-pack",
            scope: &manager,
            path: &paths,
            query: &empty,
        })
        .await?;
    assert_eq!(file.bytes, pack.bytes);
    assert!(file
        .headers()
        .iter()
        .any(|(k, v)| *k == "cache-control" && v == "private, no-store"));
    let pages = services.get::<PageService<Scope, Error>>("finance/reports.pages")?;
    let query = HashMap::from([
        ("period".into(), "custom".into()),
        ("from".into(), "2026-03-01".into()),
        ("to".into(), "2026-03-31".into()),
        ("through".into(), "1".into()),
    ]);
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "reports",
            scope: Some(&manager),
            path: &empty,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap();
    state
        .update(LiveRequest {
            scope: &manager,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    let event = json!({"type":"export_auditor_pack"});
    let body = state
        .update(LiveRequest {
            scope: &manager,
            credential: "",
            input: LiveInput::Event(&event),
        })
        .await?;
    assert!(body.html.contains("Auditor pack generated."));
    assert!(body.html.contains("download-auditor-pack"));
    for cap in [
        bilibi_finance_reports::EXPORT,
        bilibi_finance_ledger::VIEW,
        bilibi_finance_attachments::VIEW,
        bilibi_finance_receivables::VIEW,
        bilibi_finance_payables::VIEW,
        bilibi_finance_reconciliation::VIEW,
        bilibi_finance_einvoice::VIEW,
        bilibi_finance_payroll::VIEW,
        bilibi_finance_sst::VIEW,
        bilibi_finance_fixed_assets::VIEW,
    ] {
        grants.set_direct(&admin, 1, cap, Some(false)).await?;
        assert!(
            matches!(
                reports
                    .read_auditor_pack(&manager, COMPANY, &artifact.id)
                    .await,
                Err(ReportsError::Unauthorized)
            ),
            "read did not refuse {cap}"
        );
        assert!(matches!(
            downloads
                .0
                .serve(DownloadRequest {
                    operation: "auditor-pack",
                    scope: &manager,
                    path: &paths,
                    query: &empty
                })
                .await,
            Err(Error::Forbidden)
        ));
        let mandatory =
            [bilibi_finance_reports::EXPORT, bilibi_finance_ledger::VIEW].contains(&cap);
        let generated = reports
            .export_auditor_pack(&manager, COMPANY, date(2026, 3, 31))
            .await;
        if mandatory {
            assert!(generated.is_err());
        } else {
            let scoped = generated?;
            assert_eq!(scoped.reference.kind, "ledger-pack");
            let pack = reports
                .read_auditor_pack(&manager, COMPANY, &scoped.id)
                .await?;
            let (manifest, files) = inspect(&pack.bytes)?;
            assert_eq!(manifest["coverage"], "ledger-and-reports");
            assert!(!files.contains_key("payroll-posting_event.csv"));
        }
        let body = state
            .update(LiveRequest {
                scope: &manager,
                credential: "",
                input: LiveInput::Event(&event),
            })
            .await?;
        // The mandatory export/ledger grants still refuse; a source denial
        // produces a clearly labelled ledger pack, never a partial ledger.
        assert!(body
            .html
            .contains(if cap == bilibi_finance_reports::EXPORT {
                "You do not have permission to do that."
            } else if mandatory {
                "You do not have permission to export this company’s ledger."
            } else {
                "Payroll and other source records are excluded"
            }));
        grants.set_direct(&admin, 1, cap, None).await?;
    }
    println!(
        "Revocation blocks stored full packs; a new ledger-only pack omits all source evidence"
    );
    setting(&db, services, &admin, "artifacts.max_bytes", json!(1)).await?;
    assert!(matches!(
        reports
            .export_auditor_pack(&manager, COMPANY, date(2026, 3, 31))
            .await,
        Err(ReportsError::InvalidSize)
    ));
    setting(
        &db,
        services,
        &admin,
        "artifacts.max_bytes",
        json!(10_485_760),
    )
    .await?;
    bilibi_finance_walkthrough_fixture::external_accountant::run(
        &db,
        services,
        &root,
        &artifact.id,
    )
    .await?;
    if std::env::args().any(|v| v == "serve") {
        println!("Serving {origin}; manager@example.test reviews; viewer@example.test and staff@example.test prepare journals with the walkthrough password");
        let listener = tokio::net::TcpListener::bind(
            std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4324".into()),
        )
        .await?;
        axum::serve(listener, bilibi_web::router(app)).await?;
    }
    Ok(())
}
