//! FY1: real People events -> delivery -> Finance projection -> ledger.
//! `prepare`, approve records in People, `post`, then unmount People and `absent`.
//! No invented People events, provider stubs or People crate dependency.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::Ledger;
use bilibi_finance_payroll::{MappingChange, Payroll, PayrollError, SettingsChange};
use bilibi_finance_walkthrough_fixture::{install, sign_in};
use bilibi_module_registry::Services;
use bilibi_queue::{Config, Executor};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveInput, LiveRequest, PageRequest, PageService};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_payroll_roles!();
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "post".into());
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    install(&db, &services).await?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let admin = sign_in(&db, &services, 1, 4).await?;
    let viewer = sign_in(&db, &services, 1, 2).await?;
    let payroll = services.get::<Payroll>(bilibi_finance_payroll::SERVICE)?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    if mode == "prepare" {
        if payroll.people_available() {
            assert!(payroll.list_payroll_runs(&manager, 1).await?.is_empty());
            assert!(payroll.list_claims(&manager, 1).await?.is_empty());
            let html = page(&services, &manager, None).await?;
            assert!(html.contains("No payroll runs yet."));
            assert!(html.contains("No approved claims."));
        }
        println!("FY1: ledger chart and open fiscal year ready; no People facts injected");
        return Ok(());
    }
    if mode == "absent" {
        assert!(!payroll.people_available());
        assert!(matches!(
            payroll.list_payroll_runs(&manager, 1).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert!(matches!(
            payroll.list_claims(&manager, 1).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        let postings = payroll.list_postings(&manager, 1).await?;
        assert!(postings.iter().any(|p| p.source_kind == "payroll_run"));
        assert!(postings.iter().any(|p| p.source_kind == "claim"));
        let before = payroll.list_posting_events(&manager, 1).await?.len();
        assert!(matches!(
            payroll.post_claim(&manager, 1, 1).await,
            Err(PayrollError::PeopleUnavailable)
        ));
        assert_eq!(
            before,
            payroll.list_posting_events(&manager, 1).await?.len()
        );
        let html = page(&services, &manager, None).await?;
        assert!(html.contains("Payroll is not in use"));
        assert!(!html.contains("id=\"payroll-runs-table\""));
        assert!(html.contains("FY1"));
        println!("FY1: People absent; postings retained, source writes make no attempt");
        return Ok(());
    }
    assert!(payroll.people_available());
    if mode == "retained" {
        let runs = payroll.list_payroll_runs(&manager, 1).await?;
        let claims = payroll.list_claims(&manager, 1).await?;
        assert!(runs
            .iter()
            .any(|r| r["reference"] == "FY1-run" && !r["posting"].is_null()));
        assert_eq!(claims.len(), 3);
        let postings = payroll.list_postings(&manager, 1).await?;
        assert!(postings.len() >= 2);
        let html = page(&services, &manager, None).await?;
        assert!(html.contains("id=\"payroll-runs-table\""));
        assert!(!html.contains("Payroll is not in use"));
        println!("FY1: People remounted; copied sources and original postings retained");
        return Ok(());
    }
    grants
        .set_system(
            &admin,
            "domain-events",
            "events.delivery.deliver",
            Some(true),
        )
        .await?;
    let events = services.get::<bilibi_base::events::Events>(bilibi_base::events::SERVICE)?;
    events
        .replay(
            &admin,
            "finance/payroll.people",
            bilibi_base::events::ReplayFrom::Event(1),
            1000,
        )
        .await?;
    let executor = Executor::from_services(
        &services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    let delivered = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let c = db.get().await?;
            let pending = events.deliveries(&**c, &admin, 1, true, 500).await?;
            drop(c);
            if !pending
                .iter()
                .any(|d| d.subscriber == "finance/payroll.people")
            {
                return Ok::<_, Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    worker.await??;
    delivered??;
    let runs = payroll.list_payroll_runs(&manager, 1).await?;
    let claims = payroll.list_claims(&manager, 1).await?;
    let run = runs
        .iter()
        .find(|r| r["reference"] == "FY1-run")
        .ok_or("approve FY1-run in People first")?;
    let claim = claims.first().ok_or("approve claims in People first")?;
    let run_id = run["id"].as_i64().ok_or(Error::Internal)?;
    let claim_id = claim["id"].as_i64().ok_or(Error::Internal)?;
    let accounts = ledger.list_accounts(&manager, 1).await?;
    let account = |code: &str| {
        accounts
            .iter()
            .find(|a| a.code == code)
            .expect("fixture chart")
            .id
    };
    payroll
        .put_settings(
            &manager,
            1,
            &SettingsChange {
                net_pay_account_id: Some(account("2000")),
                claims_payable_account_id: Some(account("2100")),
                reason: None,
            },
        )
        .await?;
    assert!(matches!(
        payroll.post_payroll_run(&viewer, 1, run_id).await,
        Err(PayrollError::Unauthorized)
    ));
    assert!(matches!(
        payroll.list_claims(&manager, 100).await,
        Err(PayrollError::CompanyNotFound)
    ));
    assert!(payroll.post_payroll_run(&manager, 1, run_id).await.is_err());
    assert!(payroll
        .list_payroll_runs(&manager, 1)
        .await?
        .iter()
        .any(|r| r["last_failure"]["reason"].as_str().is_some()));
    for dataset in ["source_component", "source_claim"] {
        for row in payroll
            .evidence_page(&manager, 1, dataset, Some(500), None)
            .await?
            .rows
        {
            let component = dataset == "source_component";
            let direction = if component {
                row["direction"].as_str().ok_or(Error::Internal)?
            } else {
                "claim"
            };
            payroll
                .put_mapping(
                    &manager,
                    1,
                    &MappingChange {
                        kind: if component {
                            "component"
                        } else {
                            "claim_category"
                        }
                        .into(),
                        code: row[if component { "code" } else { "category_code" }]
                            .as_str()
                            .ok_or(Error::Internal)?
                            .into(),
                        direction: direction.into(),
                        expense_account_id: matches!(direction, "earning" | "employer" | "claim")
                            .then(|| account("5000")),
                        liability_account_id: matches!(direction, "deduction" | "employer")
                            .then(|| account("2000")),
                        reason: None,
                    },
                )
                .await?;
        }
    }
    let components = payroll
        .evidence_page(&manager, 1, "source_component", Some(1), None)
        .await?;
    assert_eq!(components.rows[0]["amount"], "15.241566651426");
    assert!(payroll
        .evidence_page(&manager, 1, "source_component", Some(1), components.next)
        .await?
        .rows
        .is_empty());
    let claim_evidence = payroll
        .evidence_page(&manager, 1, "source_claim", Some(500), None)
        .await?;
    assert_eq!(claim_evidence.rows.len(), 3);
    assert!(claim_evidence.rows.iter().any(|r| r["reimbursed"] == true));
    let post = payroll.post_payroll_run(&manager, 1, run_id).await?;
    assert_eq!(post.total, "15.2400");
    assert!(matches!(
        payroll.post_payroll_run(&manager, 1, run_id).await,
        Err(PayrollError::AlreadyPosted)
    ));
    let claimed = payroll.post_claim(&manager, 1, claim_id).await?;
    assert_eq!(claimed.total, "80.0000");
    let source = payroll.list_payroll_runs(&manager, 1).await?;
    assert!(source
        .iter()
        .any(|r| r["posting"]["id"] == post.id && r["last_failure"].is_null()));
    let html = page(&services, &manager, None).await?;
    for text in [
        "id=\"payroll-runs-table\"",
        "id=\"payroll-claims-table\"",
        "FY1-run",
    ] {
        assert!(html.contains(text));
    }
    assert!(!html.contains("Payroll is not in use"));
    grants
        .set_direct(
            &admin,
            1,
            bilibi_finance_payroll::POST_POSTINGS,
            Some(false),
        )
        .await?;
    let refused = page(
        &services,
        &manager,
        Some(json!({"type":"post_claim","id":claim_id.to_string()})),
    )
    .await?;
    assert!(refused.contains("You do not have permission"));
    grants
        .set_direct(&admin, 1, bilibi_finance_payroll::POST_POSTINGS, None)
        .await?;
    events
        .replay(
            &admin,
            "finance/payroll.people",
            bilibi_base::events::ReplayFrom::Event(1),
            1000,
        )
        .await?;
    assert_eq!(payroll.list_postings(&manager, 1).await?.len(), 2);
    let c = db.get().await?;
    let audited:i64=c.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'finance.payroll.source_%'",&[]).await?.get(0);
    assert!(audited > 0);
    println!(
        "FY1: real approvals delivered, exact components retained, run and claim posted once; retry, permissions, audit and replay proved"
    );
    Ok(())
}
async fn page(services: &Services, scope: &Scope, event: Option<Value>) -> Outcome<String> {
    let pages = services.get::<PageService<Scope, Error>>("finance/payroll.pages")?;
    let empty = HashMap::new();
    let mut state = pages
        .0
        .mount(PageRequest {
            operation: "postings",
            scope: Some(scope),
            path: &empty,
            query: &empty,
            body: &[],
            framed: false,
        })
        .await?
        .ok_or(Error::Internal)?;
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input: match event.as_ref() {
                Some(e) => LiveInput::Event(e),
                None => LiveInput::Refresh,
            },
        })
        .await?
        .html)
}
