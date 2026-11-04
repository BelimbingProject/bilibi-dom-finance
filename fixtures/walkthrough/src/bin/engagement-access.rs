//! Issue #95 running proof: an appointed auditor and a sales-workflow person
//! on the Platform's company/date-bound role assignments. Use a fresh
//! disposable DATABASE_URL and, for the pack, AUDITOR_PACK_PROOF_DIR naming a
//! private directory. Append `serve` to keep the app running for the browser
//! (viewer@example.test is the auditor, staff@example.test the sales person;
//! both have desks in Company 001 and Company 002, password bilibi-dev).
//! Every answer here comes from a real facade, contract or authorization
//! decision; nothing reads a Finance table.
use bilibi_authz::grants::{Assignment, Grants};
use bilibi_database::{pool, Db, Error, MutationBoundary};
use bilibi_finance_ledger::{
    EntryQuery, Ledger, LedgerError, Money, NewAccount, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_receivables::{ReceivableError, Receivables};
use bilibi_finance_reports::{Period, Reports, ReportsError};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY};
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use jiff::civil::date;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
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
        inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,concat!("walkthrough.fixture.engagement-access.",$module),bilibi_module_registry::Provenance { module:$module,source:file!(),line:line!() },&bilibi_authz::VocabularyContribution {
            domains:&[],verbs:&[],capabilities:&[],system_principals:&[],roles:&[bilibi_authz::RoleDefinition {code:"manager",name:"manager",description:"Walkthrough manager",grant_all:false,capabilities:&[$cap]}],
        }) }
    };
}
read_role!("finance/sst", bilibi_finance_sst::VIEW);
read_role!("finance/fixed_assets", bilibi_finance_fixed_assets::VIEW);

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
const AUDITOR: i64 = 2;
const SALES: i64 = 3;
const OTHER_COMPANY: i64 = 2;

const AUDIT_READS: [&str; 9] = [
    "finance.ledger.view",
    "finance.attachments.view",
    "finance.receivables.view",
    "finance.payables.view",
    "finance.reconciliation.view",
    "finance.einvoice.view",
    "finance.sst.view",
    "finance.fixed-assets.view",
    "finance.reports.reports-export",
];
const SALES_KEYS: [&str; 3] = [
    "finance.receivables.view",
    "finance.receivables.customers.manage",
    "finance.receivables.documents.manage",
];

fn pass(what: &str) {
    println!("PASS: {what}");
}

/// Every Finance capability, decided for the company the way every owner
/// facade decides it: the answer must be exactly the role's keys, only for the
/// assigned company.
async fn decide_all(
    db: &Db,
    scope: &Scope,
    company: i64,
    granted: &BTreeSet<&str>,
    what: &str,
) -> Outcome<()> {
    let client = db.get().await?;
    let keys: Vec<String> = client
        .query(
            "SELECT DISTINCT capability FROM role_capabilities WHERE capability LIKE 'finance.%' ORDER BY 1",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(keys.len() > 40, "{keys:?}");
    for key in &keys {
        let allowed = bilibi_company::authorize_work_target(&**client, scope, company, key)
            .await
            .is_ok();
        assert_eq!(
            allowed,
            granted.contains(key.as_str()),
            "{what}: {key} for company {company}"
        );
    }
    pass(&format!(
        "{what}: {} of {} Finance capabilities decide as granted for company {company}",
        granted.len(),
        keys.len()
    ));
    Ok(())
}

async fn role_id(db: &Db, code: &str) -> Outcome<i64> {
    Ok(db
        .get()
        .await?
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code=$1",
            &[&code],
        )
        .await?
        .get(0))
}

async fn capabilities_of(db: &Db, role: i64) -> Outcome<Vec<String>> {
    Ok(db
        .get()
        .await?
        .query(
            "SELECT capability FROM role_capabilities WHERE tenant_id=1 AND role_id=$1 ORDER BY 1",
            &[&role],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

struct Assigner<'a> {
    db: &'a Db,
    grants: std::sync::Arc<Grants>,
    boundary: std::sync::Arc<MutationBoundary>,
    admin: Scope,
}
impl Assigner<'_> {
    async fn set(&self, user: i64, role: i64, assigned: bool, bounds: &Assignment) -> Outcome<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                self.admin.mutation_context("finance.engagement.proof"),
            )
            .await?;
        self.grants
            .assign_role_with_in(&mut tx, &self.admin, user, role, assigned, bounds)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn engagement(company: i64, from: Option<&str>, until: Option<&str>) -> Assignment {
    Assignment {
        company_id: Some(company),
        valid_from: from.map(str::to_owned),
        valid_until: until.map(str::to_owned),
        purpose: "Statutory audit FY2026; appointment ref AUD-2026-001".into(),
    }
}

async fn zip_files(bytes: &[u8]) -> Outcome<HashMap<String, Vec<u8>>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut files = HashMap::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        files.insert(file.name()?.into_owned(), data);
    }
    Ok(files)
}

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4325".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let services: &Services = &app.services;
    // Steering for the HTTP MCP proof: change an assignment through the same Authz
    // facade between a plan and its commit. Never reinstalls the fixture.
    if let Some(mode) = std::env::args().nth(1).filter(|m| {
        [
            "revoke-sales",
            "restore-sales",
            "revoke-auditor",
            "restore-auditor",
        ]
        .contains(&m.as_str())
    }) {
        let admin = sign_in(&db, services, 1, 4).await?;
        let assigner = Assigner {
            db: &db,
            grants: services.get::<Grants>("base/authz.grants")?,
            boundary: services.get::<MutationBoundary>("base/database.mutations")?,
            admin,
        };
        let (user, role, bounds) = if mode.ends_with("sales") {
            (
                SALES,
                role_id(&db, "sales_workflow").await?,
                Assignment {
                    company_id: Some(COMPANY),
                    purpose: "Sales team".into(),
                    ..Assignment::default()
                },
            )
        } else {
            (
                AUDITOR,
                role_id(&db, "appointed_auditor").await?,
                engagement(COMPANY, None, Some("2099-12-31")),
            )
        };
        assigner
            .set(user, role, mode.starts_with("restore"), &bounds)
            .await?;
        println!("Steering applied: {mode}");
        return Ok(());
    }
    install(&db, services).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let ar = services.get::<Receivables>(bilibi_finance_receivables::SERVICE)?;
    let payroll =
        services.get::<bilibi_finance_payroll::Payroll>(bilibi_finance_payroll::SERVICE)?;
    let payables =
        services.get::<bilibi_finance_payables::Payables>(bilibi_finance_payables::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let root = std::env::var("AUDITOR_PACK_PROOF_DIR")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let storage = format!("{root}/artifacts");
    std::fs::create_dir_all(&storage)?;
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700))?;
    bilibi_finance_walkthrough_fixture::attachments::configure_storage(
        &db, services, &admin, &storage,
    )
    .await?;
    let assigner = Assigner {
        db: &db,
        grants: grants.clone(),
        boundary: services.get::<MutationBoundary>("base/database.mutations")?,
        admin: admin.clone(),
    };

    // People: affiliated, with desks in both companies and no other role.
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Auditor','human','viewer@example.test'),(5,1,'person_5','Sam Sales','human','staff@example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(3,1,4,1,'Auditor'),(4,1,5,1,'Sales') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL;
         UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL;
         INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT 1,person_id,c FROM users,(VALUES(1),(2)) v(c) WHERE tenant_id=1 AND id IN (2,3) AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))"
    ).await?;
    let old: Vec<(i64, i64)> = db
        .get()
        .await?
        .query(
            "SELECT user_id,role_id FROM principal_roles WHERE tenant_id=1 AND user_id IN (2,3)",
            &[],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    for (user, role) in old {
        grants.assign_role(&admin, user, role, false).await?;
    }

    // Company 002 has no book: every refusal below is the target-company decision, made before any book is read.

    // Company 001: receivables setup, a customer and a posted mixed entry.
    let chart = ledger.list_accounts(&manager, COMPANY).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    ar.configure(
        &manager,
        COMPANY,
        &json!({"receivable_account_id":account("1100"),"income_account_id":account("4000")}),
    )
    .await?;
    let series = ar
        .create_series(
            &manager,
            COMPANY,
            &json!({"code":"INV","name":"Invoices","kind":"invoice","prefix":"I-","number_width":4}),
        )
        .await?;
    let customer = ar
        .create_customer(
            &manager,
            COMPANY,
            &json!({"record_kind":"company","record_id":"10","payment_terms_days":"30"}),
        )
        .await?;
    let units = ledger.get_book(&manager, COMPANY).await?.minor_units;
    let amount = Money::parse("7.25", units)?;
    let mixed = NewEntry {
        entry_date: date(2026, 3, 15),
        description: "Payroll and sales allocation".into(),
        reference: Some("Mixed entry".into()),
        lines: vec![
            NewLine::debit(account("5000"), amount).described("Payroll component"),
            NewLine::debit(account("1000"), amount),
            NewLine::credit(account("2000"), amount),
            NewLine::credit(account("4000"), amount),
        ],
    };
    let posted = ledger
        .post_entry(&manager, COMPANY, &mixed, &PostOptions::default())
        .await?;
    let draft = ar
        .create_draft(
            &manager,
            COMPANY,
            &json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id}),
        )
        .await?;

    // ---- Roles contributed to the Platform's vocabulary ----
    let auditor_role = role_id(&db, "appointed_auditor").await?;
    let payroll_role = role_id(&db, "appointed_auditor_payroll").await?;
    let sales_role = role_id(&db, "sales_workflow").await?;
    let mut expected: Vec<String> = AUDIT_READS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(capabilities_of(&db, auditor_role).await?, expected);
    assert_eq!(
        capabilities_of(&db, payroll_role).await?,
        ["finance.payroll.view"]
    );
    let mut expected: Vec<String> = SALES_KEYS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(capabilities_of(&db, sales_role).await?, expected);
    for role in [auditor_role, payroll_role] {
        assert!(db
            .get()
            .await?
            .query_one(
                "SELECT requires_engagement FROM roles WHERE tenant_id=1 AND id=$1",
                &[&role]
            )
            .await?
            .get::<_, bool>(0));
    }
    pass("roles hold exactly their read keys; auditor roles require a company and a purpose");

    // ---- Engagement assignments need a company and a purpose ----
    for bad in [
        Assignment::default(),
        Assignment {
            company_id: Some(COMPANY),
            ..Assignment::default()
        },
        Assignment {
            purpose: "Audit".into(),
            ..Assignment::default()
        },
    ] {
        assert!(assigner
            .set(AUDITOR, auditor_role, true, &bad)
            .await
            .is_err());
    }
    pass("an appointed auditor is refused without a company or without a purpose; dates are optional");

    // ---- Appointed auditor: indefinite engagement for Company 001 only ----
    let auditor = sign_in(&db, services, 1, AUDITOR).await?; // retained scope
    let none: BTreeSet<&str> = BTreeSet::new();
    decide_all(&db, &auditor, COMPANY, &none, "before appointment").await?;
    assigner
        .set(
            AUDITOR,
            auditor_role,
            true,
            &engagement(COMPANY, None, None),
        )
        .await?;
    let reads: BTreeSet<&str> = AUDIT_READS.into_iter().collect();
    decide_all(&db, &auditor, COMPANY, &reads, "appointed auditor").await?;
    decide_all(&db, &auditor, OTHER_COMPANY, &none, "appointed auditor").await?;

    // Facade doors: reads in the assigned company only.
    ledger.get_book(&auditor, COMPANY).await?;
    assert_eq!(
        ledger
            .get_entry(&auditor, COMPANY, posted.id)
            .await?
            .lines
            .len(),
        4,
        "a mixed entry is shown complete"
    );
    ledger
        .list_entries(&auditor, COMPANY, &EntryQuery::default())
        .await?;
    ledger
        .trial_balance(&auditor, COMPANY, date(2026, 3, 31), None)
        .await?;
    ar.list_customers(&auditor, COMPANY).await?;
    ar.get_document(&auditor, COMPANY, draft.id).await?;
    payables_read(&payables, &auditor, COMPANY).await?;
    reports
        .profit_and_loss(
            &auditor,
            COMPANY,
            &[Period {
                from: date(2026, 3, 1),
                to: date(2026, 3, 31),
            }],
            None,
        )
        .await?;
    for dataset in ledger_datasets() {
        ledger
            .evidence_page(&auditor, COMPANY, dataset, Some(1), None)
            .await?;
    }
    assert!(matches!(
        ledger.get_book(&auditor, OTHER_COMPANY).await,
        Err(LedgerError::Unauthorized)
    ));
    assert!(ledger.get_entry(&auditor, OTHER_COMPANY, 1).await.is_err());
    assert!(ar.list_customers(&auditor, OTHER_COMPANY).await.is_err());
    assert!(reports
        .profit_and_loss(
            &auditor,
            OTHER_COMPANY,
            &[Period {
                from: date(2026, 3, 1),
                to: date(2026, 3, 31)
            }],
            None
        )
        .await
        .is_err());
    pass("auditor reads Company 001 ledger, reports and sources; Company 002 is refused on every read");

    // Read-only: no write or approval authority at any facade door.
    assert!(matches!(
        ledger
            .post_entry(&auditor, COMPANY, &mixed, &PostOptions::default())
            .await,
        Err(LedgerError::Unauthorized)
    ));
    assert!(matches!(
        ledger.submit_journal(&auditor, COMPANY, &mixed).await,
        Err(LedgerError::Unauthorized)
    ));
    assert!(matches!(
        ledger
            .create_account(
                &auditor,
                COMPANY,
                &NewAccount::new(
                    "9999",
                    "Audit adjustment",
                    bilibi_finance_ledger::AccountType::Expense
                )
            )
            .await,
        Err(LedgerError::Unauthorized)
    ));
    assert!(matches!(
        ar.create_draft(
            &auditor,
            COMPANY,
            &json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id})
        )
        .await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(matches!(
        ar.issue_document(&auditor, COMPANY, draft.id).await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(matches!(
        ar.configure(&auditor, COMPANY, &json!({})).await,
        Err(ReceivableError::Unauthorized)
    ));
    pass("auditor cannot post, submit, change the chart, draft, issue or configure");

    // Payroll person-level detail needs the separate payroll evidence role.
    assert!(payroll
        .list_posting_events(&auditor, COMPANY)
        .await
        .is_err());
    assert!(payroll
        .evidence_page(&auditor, COMPANY, "posting_event", None, None)
        .await
        .is_err());
    pass("payroll source records are refused to the auditor role alone");

    // Export: without payroll the pack is the labelled ledger-and-reports pack.
    let restricted = reports
        .export_auditor_pack(&auditor, COMPANY, date(2026, 3, 31))
        .await?;
    assert_eq!(restricted.reference.kind, "ledger-pack");
    let files = zip_files(
        &reports
            .read_auditor_pack(&auditor, COMPANY, &restricted.id)
            .await?
            .bytes,
    )
    .await?;
    let manifest: Value = serde_json::from_slice(&files["manifest.json"])?;
    assert_eq!(manifest["coverage"], "ledger-and-reports");
    assert!(manifest["coverage_notice"]
        .as_str()
        .unwrap()
        .contains("not a complete audit evidence pack"));
    assert!(reports
        .export_auditor_pack(&auditor, OTHER_COMPANY, date(2026, 3, 31))
        .await
        .is_err());
    pass("auditor alone exports the labelled ledger-and-reports pack, never presented as a full audit pack");

    // Add payroll evidence (same company, same purpose): the full pack.
    assigner
        .set(
            AUDITOR,
            payroll_role,
            true,
            &engagement(COMPANY, None, None),
        )
        .await?;
    payroll.list_posting_events(&auditor, COMPANY).await?;
    let mut with_payroll = reads.clone();
    with_payroll.insert("finance.payroll.view");
    decide_all(
        &db,
        &auditor,
        COMPANY,
        &with_payroll,
        "auditor with payroll evidence",
    )
    .await?;
    decide_all(
        &db,
        &auditor,
        OTHER_COMPANY,
        &none,
        "auditor with payroll evidence",
    )
    .await?;
    let full = reports
        .export_auditor_pack(&auditor, COMPANY, date(2026, 3, 31))
        .await?;
    assert_eq!(full.reference.kind, "auditor-pack");
    let files = zip_files(
        &reports
            .read_auditor_pack(&auditor, COMPANY, &full.id)
            .await?
            .bytes,
    )
    .await?;
    let manifest: Value = serde_json::from_slice(&files["manifest.json"])?;
    assert_eq!(manifest["coverage"], "all-finance");
    std::fs::write(format!("{root}/appointed-auditor-pack.zip"), {
        reports
            .read_auditor_pack(&auditor, COMPANY, &full.id)
            .await?
            .bytes
    })?;
    pass("with the payroll evidence role the auditor exports the full all-finance pack");

    // Revoking payroll evidence blocks the old full pack at once, retained scope included.
    assigner
        .set(
            AUDITOR,
            payroll_role,
            false,
            &engagement(COMPANY, None, None),
        )
        .await?;
    assert!(reports
        .read_auditor_pack(&auditor, COMPANY, &full.id)
        .await
        .is_err());
    assert!(payroll
        .list_posting_events(&auditor, COMPANY)
        .await
        .is_err());
    reports
        .read_auditor_pack(&auditor, COMPANY, &restricted.id)
        .await?;
    pass("revoking payroll evidence refuses the old full pack and payroll reads immediately");

    // Dates: a future start, then an inclusive end that has passed, refuse the
    // same retained scope; an open end restores it; removal refuses it again.
    assigner
        .set(
            AUDITOR,
            auditor_role,
            true,
            &engagement(COMPANY, Some("2099-01-01"), None),
        )
        .await?;
    decide_all(&db, &auditor, COMPANY, &none, "future start").await?;
    assert!(ledger.get_book(&auditor, COMPANY).await.is_err());
    assigner
        .set(
            AUDITOR,
            auditor_role,
            true,
            &engagement(COMPANY, Some("2020-01-01"), Some("2020-12-31")),
        )
        .await?;
    decide_all(&db, &auditor, COMPANY, &none, "expired engagement").await?;
    assert!(reports
        .read_auditor_pack(&auditor, COMPANY, &restricted.id)
        .await
        .is_err());
    assert!(reports
        .export_auditor_pack(&auditor, COMPANY, date(2026, 3, 31))
        .await
        .is_err());
    assigner
        .set(
            AUDITOR,
            auditor_role,
            true,
            &engagement(COMPANY, None, None),
        )
        .await?;
    decide_all(&db, &auditor, COMPANY, &reads, "open-ended engagement").await?;
    assigner
        .set(
            AUDITOR,
            auditor_role,
            false,
            &engagement(COMPANY, None, None),
        )
        .await?;
    decide_all(&db, &auditor, COMPANY, &none, "revoked engagement").await?;
    assert!(ledger.get_book(&auditor, COMPANY).await.is_err());
    assert!(reports
        .read_auditor_pack(&auditor, COMPANY, &restricted.id)
        .await
        .is_err());
    pass("future start, passed end and revocation each refuse the retained scope, reads and old pack links; no end date means no expiry");

    // Re-appoint with a real end date so the browser shows a dated engagement.
    assigner
        .set(
            AUDITOR,
            auditor_role,
            true,
            &engagement(COMPANY, None, Some("2099-12-31")),
        )
        .await?;
    ledger.get_book(&auditor, COMPANY).await?;

    // ---- Sales workflow: customers and drafts, no ledger ----
    let sales = sign_in(&db, services, 1, SALES).await?;
    assigner
        .set(
            SALES,
            sales_role,
            true,
            &Assignment {
                company_id: Some(COMPANY),
                purpose: "Sales team".into(),
                ..Assignment::default()
            },
        )
        .await?;
    let sales_keys: BTreeSet<&str> = SALES_KEYS.into_iter().collect();
    decide_all(&db, &sales, COMPANY, &sales_keys, "sales workflow").await?;
    decide_all(&db, &sales, OTHER_COMPANY, &none, "sales workflow").await?;
    ar.list_customers(&sales, COMPANY)
        .await
        .map_err(|e| format!("sales list_customers: {e:?}"))?;
    ar.get_document(&sales, COMPANY, draft.id)
        .await
        .map_err(|e| format!("sales get_document: {e:?}"))?;
    // An invoice draft also reads the customer relationship in Core Company, which
    // asks for Core's own tenant-wide `admin.company.view`; the role cannot hold
    // it (Core keys are not company-scoped), so it is granted on its own.
    assert!(ar
        .create_draft(
            &sales,
            COMPANY,
            &json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id})
        )
        .await
        .is_err());
    grants
        .set_direct(&admin, SALES, "admin.company.view", Some(true))
        .await?;
    let mine = ar
        .create_draft(
            &sales,
            COMPANY,
            &json!({"kind":"invoice","series_id":series.id,"customer_id":customer.id,
                    "lines":[{"description":"Consulting","quantity":"2","unit_price":"150.00"}]}),
        )
        .await
        .map_err(|e| format!("sales create_draft: {e:?}"))?;
    ar.update_draft(
        &sales,
        COMPANY,
        mine.id,
        &json!({"description":"Updated by sales"}),
    )
    .await?;
    ar.discard_draft(&sales, COMPANY, mine.id).await?;
    assert!(ar
        .create_draft(&sales, OTHER_COMPANY, &json!({}))
        .await
        .is_err());
    pass("sales workflow lists customers, reads drafts and prepares, edits and discards a draft in Company 001 without ledger view");

    // Everything outside the workflow: ledger, reports, aging, issue, receipts, payables, payroll.
    assert!(matches!(
        ledger.get_book(&sales, COMPANY).await,
        Err(LedgerError::Unauthorized)
    ));
    assert!(ledger.list_accounts(&sales, COMPANY).await.is_err());
    assert!(ledger
        .list_entries(&sales, COMPANY, &EntryQuery::default())
        .await
        .is_err());
    assert!(ledger.get_entry(&sales, COMPANY, posted.id).await.is_err());
    assert!(ledger
        .trial_balance(&sales, COMPANY, date(2026, 3, 31), None)
        .await
        .is_err());
    assert!(reports
        .profit_and_loss(
            &sales,
            COMPANY,
            &[Period {
                from: date(2026, 3, 1),
                to: date(2026, 3, 31)
            }],
            None
        )
        .await
        .is_err());
    assert!(reports
        .export_auditor_pack(&sales, COMPANY, date(2026, 3, 31))
        .await
        .is_err());
    assert!(matches!(
        reports
            .read_auditor_pack(&sales, COMPANY, &restricted.id)
            .await,
        Err(ReportsError::Unauthorized)
    ));
    assert!(ar
        .aged_receivables(&sales, COMPANY, date(2026, 3, 31), &Default::default())
        .await
        .is_err());
    assert!(matches!(
        ar.issue_document(&sales, COMPANY, draft.id).await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(matches!(
        ar.configure(&sales, COMPANY, &json!({})).await,
        Err(ReceivableError::Unauthorized)
    ));
    assert!(payables_read(&payables, &sales, COMPANY).await.is_err());
    assert!(payroll.list_posting_events(&sales, COMPANY).await.is_err());
    for dataset in ledger_datasets() {
        assert!(ledger
            .evidence_page(&sales, COMPANY, dataset, Some(1), None)
            .await
            .is_err());
    }
    pass("sales workflow is refused the ledger, chart, entries, trial balance, reports, packs, aging, issue, setup, payables and payroll");

    // Revocation refuses the retained scope at once.
    assigner
        .set(
            SALES,
            sales_role,
            false,
            &Assignment {
                company_id: Some(COMPANY),
                ..Assignment::default()
            },
        )
        .await?;
    decide_all(&db, &sales, COMPANY, &none, "revoked sales workflow").await?;
    assert!(ar.list_customers(&sales, COMPANY).await.is_err());
    assigner
        .set(
            SALES,
            sales_role,
            true,
            &Assignment {
                company_id: Some(COMPANY),
                purpose: "Sales team".into(),
                ..Assignment::default()
            },
        )
        .await?;

    println!(
        "Appointed auditor and sales workflow: company/date-bound, read-only auditor, every Finance capability decided, facade doors refused, exports, dates and revocation proved."
    );
    if std::env::args().any(|v| v == "serve") {
        credentials(&db, services, &root).await?;
        println!("Serving {origin}: viewer@example.test (appointed auditor, Company 001 only), staff@example.test (sales workflow, Company 001 only), manager@example.test (every company); password bilibi-dev");
        axum::serve(
            tokio::net::TcpListener::bind(
                std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4325".into()),
            )
            .await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}

fn ledger_datasets() -> Vec<&'static str> {
    Ledger::evidence_datasets().into_iter().take(1).collect()
}
async fn payables_read(
    payables: &bilibi_finance_payables::Payables,
    scope: &Scope,
    company: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let dataset = payables.evidence_datasets()[0];
    payables
        .evidence_page(scope, company, dataset, Some(1), None)
        .await?;
    Ok(())
}

async fn credentials(db: &Db, services: &Services, root: &str) -> Outcome<()> {
    use bilibi_core::agent::{
        code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange,
    };
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
    let access = services.get::<AgentAccess>("core/agent.grants")?;
    let callback = "https://example.test/engagement/callback";
    let registration=access.register_client(&serde_json::from_value::<ClientRegistration>(json!({"client_name":"Engagement access proof","redirect_uris":[callback],"token_endpoint_auth_method":"none"}))?).await?;
    let client_id = registration["client_id"].as_str().ok_or(Error::Invalid)?;
    let resource = format!(
        "{}/mcp",
        std::env::var("APP_ORIGIN")
            .unwrap_or_else(|_| "http://127.0.0.1:4324".into())
            .trim_end_matches('/')
    );
    let mut saved = serde_json::Map::new();
    for (name, user) in [("auditor", AUDITOR), ("sales", SALES), ("manager", 1)] {
        let scope = sign_in(db, services, 1, user).await?;
        let verifier = "engagement-proof-verifier-with-at-least-forty-three-characters";
        let params = AuthorizeParams {
            client_id: client_id.into(),
            redirect_uri: callback.into(),
            response_type: "code".into(),
            resource: resource.clone(),
            scope: Some("bilibi".into()),
            code_challenge: code_challenge(verifier),
            code_challenge_method: "S256".into(),
            state: None,
        };
        let code = access.authorize(&scope, &params, &resource).await?;
        let token = access
            .exchange_code(
                &CodeExchange {
                    code,
                    client_id: params.client_id,
                    redirect_uri: params.redirect_uri,
                    code_verifier: verifier.into(),
                    resource: resource.clone(),
                },
                &resource,
            )
            .await?;
        saved.insert(name.into(), json!(token.access_token));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(format!("{root}/mcp-credentials.json"))?;
    file.write_all(serde_json::to_string(&saved)?.as_bytes())?;
    Ok(())
}
