//! Issue #61: the production role on two real accountant identities. The full
//! ledger is permitted, including mixed/payroll postings; source records are not.
use crate::{sign_in, COMPANY};
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error};
use bilibi_finance_ledger::{
    EntryOwner, Ledger, LedgerError, Money, NewEntry, NewLine, PostOptions,
};
use bilibi_finance_reports::{Period, Reports, ReportsError};
use bilibi_module_registry::Services;
use jiff::civil::date;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

pub async fn run(db: &Db, services: &Services, root: &str, full_pack: &str) -> Outcome<()> {
    // Identity-only fixture rows, as in ledger-ui; no Finance business tables.
    db.get().await?.batch_execute(
        "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Accountant','human','viewer@example.test'),(5,1,'person_5','Sam Accountant','human','staff@example.test') ON CONFLICT DO NOTHING;
         INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(3,1,4,1,'Accountant'),(4,1,5,1,'Accountant') ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=3 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=3 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
         SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
         SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))"
    ).await?;
    let admin = sign_in(db, services, 1, 4).await?;
    let supervisor = sign_in(db, services, 1, 1).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let c = db.get().await?;
    let external: i64 = c
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=1 AND code='external_accountant'",
            &[],
        )
        .await?
        .get(0);
    let capabilities: Vec<String> = c.query("SELECT capability FROM role_capabilities WHERE tenant_id=1 AND role_id=$1 ORDER BY capability", &[&external]).await?.iter().map(|r| r.get(0)).collect();
    assert_eq!(
        capabilities,
        [
            "finance.ledger.journals.submit",
            "finance.ledger.view",
            "finance.reports.reports-export",
            "workflow.approvals.view"
        ]
    );
    let old_roles: Vec<(i64, i64)> = c
        .query(
            "SELECT user_id,role_id FROM principal_roles WHERE tenant_id=1 AND user_id IN (2,3)",
            &[],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    drop(c);
    for (user, role) in old_roles {
        grants.assign_role(&admin, user, role, false).await?;
    }
    for user in [2, 3] {
        grants.assign_role(&admin, user, external, true).await?;
    }
    let first = sign_in(db, services, 1, 2).await?;
    let second = sign_in(db, services, 1, 3).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let reports = services.get::<Reports>(bilibi_finance_reports::SERVICE)?;
    let payroll =
        services.get::<bilibi_finance_payroll::Payroll>(bilibi_finance_payroll::SERVICE)?;
    let attachments = services
        .get::<bilibi_finance_attachments::Attachments>(bilibi_finance_attachments::SERVICE)?;
    let chart = ledger.list_accounts(&first, COMPANY).await?;
    let account = |code: &str| chart.iter().find(|a| a.code == code).unwrap().id;
    let amount = Money::parse("7.25", ledger.get_book(&first, COMPANY).await?.minor_units)?;
    let entry = NewEntry {
        entry_date: date(2026, 3, 15),
        description: "Historical payroll and sales allocation".into(),
        reference: Some("Payroll journal detail is permitted".into()),
        lines: vec![
            NewLine::debit(account("5000"), amount).described("Payroll component"),
            NewLine::debit(account("1000"), amount),
            NewLine::credit(account("2000"), amount),
            NewLine::credit(account("4000"), amount),
        ],
    };
    // Explicit historical fixture entry, not a synthetic People source record.
    let mixed = ledger
        .post_entry(
            &supervisor,
            COMPANY,
            &entry,
            &PostOptions {
                owner: Some(EntryOwner {
                    module: bilibi_finance_payroll::MODULE.into(),
                    record: "payroll_run:9061".into(),
                }),
                ..Default::default()
            },
        )
        .await?;
    for scope in [&first, &second] {
        assert_eq!(
            ledger
                .get_entry(scope, COMPANY, mixed.id)
                .await?
                .lines
                .len(),
            4
        );
        assert!(matches!(
            ledger.get_book(scope, 2).await,
            Err(LedgerError::Unauthorized)
        ));
        assert!(ledger.get_book(scope, 100).await.is_err());
        assert!(payroll.list_posting_events(scope, COMPANY).await.is_err());
        assert!(payroll
            .evidence_page(scope, COMPANY, "posting_event", None, None)
            .await
            .is_err());
        assert!(attachments
            .evidence_page(scope, COMPANY, "attachment", None, None)
            .await
            .is_err());
        assert!(matches!(
            ledger
                .post_entry(scope, COMPANY, &entry, &PostOptions::default())
                .await,
            Err(LedgerError::Unauthorized)
        ));
        assert!(matches!(
            ledger
                .create_account(
                    scope,
                    COMPANY,
                    &bilibi_finance_ledger::NewAccount::new(
                        "restricted",
                        "Restricted",
                        bilibi_finance_ledger::AccountType::Expense
                    )
                )
                .await,
            Err(LedgerError::Unauthorized)
        ));
        let journal = ledger.submit_journal(scope, COMPANY, &entry).await?;
        assert!(matches!(
            ledger.approve_journal(scope, COMPANY, journal.id).await,
            Err(LedgerError::Unauthorized)
        ));
        ledger
            .approve_journal(&supervisor, COMPANY, journal.id)
            .await?;
        assert_eq!(
            ledger
                .trial_balance(scope, COMPANY, date(2026, 3, 31), None)
                .await?
                .rows
                .len(),
            ledger
                .trial_balance(&supervisor, COMPANY, date(2026, 3, 31), None)
                .await?
                .rows
                .len()
        );
        reports
            .profit_and_loss(
                scope,
                COMPANY,
                &[Period {
                    from: date(2026, 3, 1),
                    to: date(2026, 3, 31),
                }],
                None,
            )
            .await?;
        assert!(reports
            .profit_and_loss(
                scope,
                2,
                &[Period {
                    from: date(2026, 3, 1),
                    to: date(2026, 3, 31)
                }],
                None
            )
            .await
            .is_err());
        assert!(matches!(
            reports.read_auditor_pack(scope, COMPANY, full_pack).await,
            Err(ReportsError::Unauthorized)
        ));
    }
    let metadata = reports
        .export_auditor_pack(&first, COMPANY, date(2026, 3, 31))
        .await?;
    assert_eq!(metadata.reference.kind, "ledger-pack");
    let pack = reports
        .read_auditor_pack(&first, COMPANY, &metadata.id)
        .await?;
    let mut zip = zip::ZipArchive::new(Cursor::new(&pack.bytes))?;
    let mut files = HashMap::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        files.insert(file.name()?.into_owned(), bytes);
    }
    let manifest: Value = serde_json::from_slice(&files["manifest.json"])?;
    assert_eq!(manifest["coverage"], "ledger-and-reports");
    assert!(!files.contains_key("receivables-customer.csv"));
    assert!(manifest["coverage_notice"]
        .as_str()
        .unwrap()
        .contains("Payroll source records"));
    assert!(files.keys().all(|name| {
        name.starts_with("ledger-")
            || [
                "manifest.json",
                "general-ledger.csv",
                "trial-balance.json",
                "trial-balance.csv",
                "issued-reports.csv",
                "source-file-index.csv",
            ]
            .contains(&name.as_str())
    }));
    assert!(std::str::from_utf8(&files["general-ledger.csv"])?.contains("Payroll component"));
    assert_eq!(
        manifest["control_totals"]["debits"],
        manifest["control_totals"]["credits"]
    );
    std::fs::write(format!("{root}/external-accountant-pack.zip"), &pack.bytes)?;
    // Live scopes and old links must lose authority immediately. Another
    // accountant's independent role assignment is unaffected.
    for key in [bilibi_finance_ledger::VIEW, bilibi_finance_reports::EXPORT] {
        grants.set_direct(&admin, 2, key, Some(false)).await?;
        assert!(reports
            .read_auditor_pack(&first, COMPANY, &metadata.id)
            .await
            .is_err());
        assert!(reports
            .export_auditor_pack(&first, COMPANY, date(2026, 3, 31))
            .await
            .is_err());
        reports
            .read_auditor_pack(&second, COMPANY, &metadata.id)
            .await?;
        grants.set_direct(&admin, 2, key, None).await?;
    }
    grants.assign_role(&admin, 2, external, false).await?;
    assert!(ledger.get_book(&first, COMPANY).await.is_err());
    assert!(reports
        .read_auditor_pack(&first, COMPANY, &metadata.id)
        .await
        .is_err());
    ledger.get_book(&second, COMPANY).await?;
    grants.assign_role(&admin, 2, external, true).await?;
    // Seed authority state for a real MCP wire proof and browser login. OAuth
    // secrets stay only in the task's private proof directory, never stdout.
    credentials(db, services, root).await?;
    println!(
        "External accountant: two affiliated people; complete mixed/payroll ledger, reports and scoped ZIP allowed; other companies, source evidence, settings, direct posting and approval refused; independent review and live revocation proved."
    );
    Ok(())
}

async fn credentials(db: &Db, services: &Services, root: &str) -> Outcome<()> {
    use bilibi_core::agent::{
        code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange,
    };
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
    let access = services.get::<AgentAccess>("core/agent.grants")?;
    let callback = "https://example.test/accountant/callback";
    let registration=access.register_client(&serde_json::from_value::<ClientRegistration>(json!({"client_name":"External accountant proof","redirect_uris":[callback],"token_endpoint_auth_method":"none"}))?).await?;
    let client_id = registration["client_id"].as_str().ok_or(Error::Invalid)?;
    let resource = format!(
        "{}/mcp",
        std::env::var("APP_ORIGIN")
            .unwrap_or_else(|_| "http://127.0.0.1:4324".into())
            .trim_end_matches('/')
    );
    let mut saved = serde_json::Map::new();
    for (name, user) in [("first", 2), ("second", 3), ("supervisor", 1)] {
        let scope = sign_in(db, services, 1, user).await?;
        let verifier = "accountant-proof-verifier-with-at-least-forty-three-characters";
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
