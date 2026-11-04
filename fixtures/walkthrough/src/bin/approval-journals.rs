//! Real manual-journal approval proof on a fresh disposable database. Pass
//! --serve to keep the composed app open for the shared waiting list and the
//! Finance approval exceptions page. No credentials are printed.
use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_core::agent::{
    code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange,
};
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{
    Journal, JournalStatus, Ledger, LedgerError, Money, NewEntry, NewLine, APPROVE_JOURNALS,
    MANAGE_DUTY_SEPARATION,
};
use bilibi_finance_walkthrough_fixture::{install, sign_in, COMPANY, PASSWORD};
use bilibi_tenancy::{Authentication, ProofFuture, Scope};
use bilibi_workflow::{ActionRequest, ApprovalItem, ApprovalPolicyChange, ApprovalQuery, Engine};
use jiff::civil::date;
use serde_json::json;
use tokio_postgres::Transaction;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();

const CASE: &str = "finance.ledger.manual_journal";
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
            let scope = self.app.security.sessions.check(tx, self.token).await?;
            Ok(PlanIdentity {
                scope,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
async fn policy(
    db: &bilibi_database::Db,
    ledger: &Ledger,
    engine: &Engine,
    scope: &Scope,
    allow: bool,
) -> Outcome<()> {
    let (expected, mut policy) = engine.approval_policy(scope, CASE, COMPANY).await?;
    policy.allow_preparer = allow;
    let mut client = db.get().await?;
    let mut tx = ledger
        .write(
            &mut client,
            scope,
            COMPANY,
            &[MANAGE_DUTY_SEPARATION],
            "finance.walkthrough.journal_policy",
        )
        .await?;
    engine
        .publish_approval_policy_in(
            &mut tx,
            scope,
            ApprovalPolicyChange {
                case: CASE,
                company: COMPANY,
                expected: Some(expected),
                policy,
                reason: if allow {
                    "Temporary journal approval exception during leave"
                } else {
                    "Independent journal approval required again"
                },
            },
        )
        .await?;
    tx.commit().await?;
    Ok(())
}
async fn item(engine: &Engine, scope: &Scope, journal: &Journal) -> Outcome<ApprovalItem> {
    engine
        .my_approvals(
            scope,
            &ApprovalQuery {
                case: Some(CASE.into()),
                company: Some(COMPANY),
                limit: 100,
                ..Default::default()
            },
        )
        .await?
        .entries
        .into_iter()
        .find(|e| e.request.subject_id == journal.id)
        .ok_or("journal missing from eligible waiting list".into())
}
fn action(item: &ApprovalItem, key: &str) -> ActionRequest {
    let mut action = ActionRequest::new("finance.ledger.journal.decide", key, item.subject_version);
    action.payload = json!({"request":item.request.id,"version":item.request.version,"routing_version":item.request.routing_version,"stage":item.request.stages[item.request.stage_index].key,"seat":item.seats[0],"approve":true,"reason":"","input":{}});
    action
}
async fn pin(ledger: &Ledger, scope: &Scope) -> Outcome<i64> {
    Ok(ledger
        .trial_balance(scope, COMPANY, date(2026, 12, 31), None)
        .await?
        .through_entry_number)
}
async fn agent(app: &bilibi_web::App, scope: &Scope, origin: &str) -> Outcome<Scope> {
    let access = app.services.get::<AgentAccess>("core/agent.grants")?;
    let callback = "https://example.test/journal-proof/callback";
    let client=access.register_client(&serde_json::from_value::<ClientRegistration>(json!({"client_name":"Journal approval walkthrough","redirect_uris":[callback],"token_endpoint_auth_method":"none"}))?).await?;
    let resource = format!("{origin}/mcp");
    let verifier = "journal-approval-proof-verifier-with-at-least-forty-three-characters";
    let params = AuthorizeParams {
        client_id: client["client_id"].as_str().ok_or(Error::Invalid)?.into(),
        redirect_uri: callback.into(),
        response_type: "code".into(),
        resource: resource.clone(),
        scope: Some("bilibi".into()),
        code_challenge: code_challenge(verifier),
        code_challenge_method: "S256".into(),
        state: None,
    };
    let code = access.authorize(scope, &params, &resource).await?;
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
    let claims = access
        .authenticate(&token.access_token, Some(&resource))
        .await?;
    Ok(access
        .seal(&claims, "finance-journal-approval-proof", None)
        .await?)
}

#[tokio::main]
async fn main() -> Outcome<()> {
    if std::env::var("BILIBI_DISPOSABLE_APPROVALS").as_deref() != Ok("yes") {
        return Err("Use BILIBI_DISPOSABLE_APPROVALS=yes with a fresh disposable database".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:43171".into());
    bilibi_database::migrate(&db, &bilibi_module_registry::discover()?).await?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    bilibi_finance_walkthrough_fixture::without_step_up(&app.services)?;
    let installed = install(&db, &app.services).await?;
    // Disposable identity setup only. All business records below use facades.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Approval Reviewer','human','admin@example.test') ON CONFLICT DO NOTHING; INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(3,1,4,1,'Reviewer') ON CONFLICT DO NOTHING; UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons)); SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations));").await?;
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(5,1,'person_5','Approval Reader','human','viewer@example.test') ON CONFLICT DO NOTHING; INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(4,1,5,1,'Reader') ON CONFLICT DO NOTHING; UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING; SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons)); SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations));").await?;
    let ledger = app.services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let engine = app.services.get::<Engine>(bilibi_workflow::SERVICE)?;
    let manager = sign_in(&db, &app.services, 1, 1).await?;
    let reviewer = sign_in(&db, &app.services, 1, 4).await?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_direct(&reviewer, 2, "workflow.approvals.view", Some(true))
        .await?;
    let reader = sign_in(&db, &app.services, 1, 2).await?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let impersonated = {
        let c = db.get().await?;
        auth.authenticate_session(&**c, 1, 1, Some(4), None, String::new())
            .await?
    };
    let agent_manager = agent(&app, &manager, &origin).await?;
    let account = |code: &str| {
        installed
            .accounts
            .iter()
            .find(|a| a.code == code)
            .expect("fixture account")
            .id
    };
    let amount = Money::parse("12.34", installed.book.minor_units)?;
    let entry = |description: &str, day| NewEntry {
        entry_date: day,
        description: description.into(),
        reference: None,
        lines: vec![
            NewLine::debit(account("1000"), amount),
            NewLine::credit(account("4000"), amount),
        ],
    };
    let journal = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Independent approval proof", date(2026, 6, 1)),
        )
        .await?;
    let before = pin(&ledger, &manager).await?;
    for scope in [&manager, &impersonated, &agent_manager] {
        assert!(matches!(
            ledger.approve_journal(scope, COMPANY, journal.id).await,
            Err(LedgerError::ApprovalRefused(message))
                if message.contains("cannot decide this journal")
                    && !message.contains("Waiting for my approval")
        ));
        assert_eq!(
            ledger
                .get_journal(&manager, COMPANY, journal.id)
                .await?
                .status,
            JournalStatus::Submitted
        );
        assert!(!engine
            .my_approvals(
                scope,
                &ApprovalQuery {
                    case: Some(CASE.into()),
                    limit: 100,
                    ..Default::default()
                }
            )
            .await?
            .entries
            .iter()
            .any(|i| i.request.subject_id == journal.id));
    }
    assert_eq!(before, pin(&ledger, &manager).await?);
    println!("PASS: direct, impersonated and live agent self-approval refused; no entry or journal decision");
    let vote = action(
        &item(&engine, &reviewer, &journal).await?,
        "finance-journal-replay-proof",
    );
    for replay in [false, true] {
        let mut client = db.get().await?;
        let mut tx = ledger
            .write(
                &mut client,
                &reviewer,
                COMPANY,
                &[APPROVE_JOURNALS],
                "finance.walkthrough.journal_replay",
            )
            .await?;
        let result = engine
            .execute_action_in(
                &mut tx,
                &reviewer,
                "finance.ledger.journal",
                journal.id,
                vote.clone(),
            )
            .await?;
        assert_eq!(result.replayed, replay);
        tx.commit().await?;
    }
    assert_eq!(pin(&ledger, &manager).await?, before + 1);
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, journal.id)
            .await?
            .status,
        JournalStatus::Approved
    );
    println!("PASS: independent approval and duplicate commit post exactly one entry");

    policy(&db, &ledger, &engine, &manager, true).await?;
    let exceptional = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Permitted preparer approval", date(2026, 6, 2)),
        )
        .await?;
    ledger
        .approve_journal(&manager, COMPANY, exceptional.id)
        .await?;
    let (exceptions, _) = engine
        .approval_exceptions(&reviewer, 0, 100, Some(COMPANY), Some(CASE), false)
        .await?;
    let exception = exceptions
        .iter()
        .find(|e| e.subject_id == exceptional.id)
        .ok_or("shared exception missing")?;
    let (visible, _) = engine
        .approval_exceptions(&reader, 0, 100, Some(COMPANY), Some(CASE), false)
        .await?;
    assert!(visible
        .iter()
        .any(|e| e.id == exception.id && !e.reviewable));
    {
        let mut client = db.get().await?;
        let mut tx = ledger
            .write(
                &mut client,
                &reader,
                COMPANY,
                &[bilibi_finance_ledger::VIEW],
                "finance.walkthrough.reader_cannot_review",
            )
            .await?;
        assert!(matches!(
            engine
                .review_approval_exception_in(&mut tx, &reader, exception.id, "")
                .await,
            Err(Error::Forbidden)
        ));
        tx.rollback().await?;
    }
    assert_eq!(exception.kind, "same_person");
    assert_eq!(
        exception.reason,
        "Temporary journal approval exception during leave"
    );
    assert!(!ledger
        .list_approval_exceptions(&manager, COMPANY, &Default::default())
        .await?
        .iter()
        .any(|e| e.action == "journal_approval"));
    {
        let mut client = db.get().await?;
        let mut tx = ledger
            .write(
                &mut client,
                &manager,
                COMPANY,
                &[bilibi_finance_ledger::REVIEW_EXCEPTIONS],
                "finance.walkthrough.review_own_exception",
            )
            .await?;
        assert!(matches!(
            engine
                .review_approval_exception_in(&mut tx, &manager, exception.id, "")
                .await,
            Err(Error::Forbidden)
        ));
        tx.rollback().await?;
    }
    {
        let mut client = db.get().await?;
        let mut tx = ledger
            .write(
                &mut client,
                &reviewer,
                COMPANY,
                &[bilibi_finance_ledger::REVIEW_EXCEPTIONS],
                "finance.walkthrough.review_journal_exception",
            )
            .await?;
        engine
            .review_approval_exception_in(
                &mut tx,
                &reviewer,
                exception.id,
                "Source and balancing lines checked",
            )
            .await?;
        tx.commit().await?;
    }
    assert!(
        engine
            .approval_exceptions(&reviewer, 0, 100, Some(COMPANY), Some(CASE), false)
            .await?
            .0
            .iter()
            .find(|e| e.id == exception.id)
            .expect("reviewed exception")
            .reviewed
    );
    assert!(
        ledger
            .get_duty_separation(&manager, COMPANY)
            .await?
            .required
    );
    {
        let mut client = db.get().await?;
        let mut tx = ledger
            .write(
                &mut client,
                &manager,
                COMPANY,
                &[bilibi_finance_ledger::VIEW],
                "finance.walkthrough.other_finance_still_separated",
            )
            .await?;
        let approval = bilibi_finance_ledger::Approval {
            source: "finance/walkthrough".into(),
            action: "record_approval".into(),
            subject_type: "record".into(),
            subject_id: 1,
            subject: "Other Finance approval".into(),
            prepared_by: vec![bilibi_finance_ledger::Preparer {
                user_id: manager.user(),
                impersonator_id: None,
            }],
            override_reason: None,
        };
        assert!(matches!(
            ledger
                .check_approval(&mut tx, &manager, COMPANY, &approval)
                .await,
            Err(LedgerError::SamePerson)
        ));
        tx.rollback().await?;
    }
    println!("PASS: journal separation off records only the central exception; self-review refused and independent review recorded; other Finance separation remains required");
    // Keep one unreviewed exception for the Finance page browser proof.
    let browser_exception = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Journal for independent exception review", date(2026, 6, 3)),
        )
        .await?;
    ledger
        .approve_journal(&manager, COMPANY, browser_exception.id)
        .await?;
    policy(&db, &ledger, &engine, &manager, false).await?;

    let recalled = ledger
        .submit_journal(&manager, COMPANY, &entry("Recall proof", date(2026, 6, 4)))
        .await?;
    ledger
        .withdraw_journal(&manager, COMPANY, recalled.id, None)
        .await?;
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, recalled.id)
            .await?
            .status,
        JournalStatus::Withdrawn
    );
    assert!(matches!(
        ledger
            .approve_journal(&reviewer, COMPANY, recalled.id)
            .await,
        Err(LedgerError::JournalDecided)
    ));
    let correction = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Correction proof", date(2026, 6, 5)),
        )
        .await?;
    let mut invalid = entry("Unbalanced correction", date(2026, 6, 5));
    invalid.lines.pop();
    assert!(ledger
        .replace_journal(&manager, COMPANY, correction.id, &invalid, None)
        .await
        .is_err());
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, correction.id)
            .await?
            .status,
        JournalStatus::Submitted
    );
    let replacement = ledger
        .replace_journal(
            &manager,
            COMPANY,
            correction.id,
            &entry("Corrected journal", date(2026, 6, 5)),
            None,
        )
        .await?;
    assert_eq!(replacement.replaces_journal_id, Some(correction.id));
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, correction.id)
            .await?
            .status,
        JournalStatus::Withdrawn
    );
    println!("PASS: recall prevents posting; failed correction rolls back; valid correction keeps the immutable replacement link");

    let january = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Closed period proof", date(2026, 1, 15)),
        )
        .await?;
    let period = ledger.list_fiscal_years(&manager, COMPANY).await?[0].periods[0].id;
    ledger.close_period(&manager, COMPANY, period, None).await?;
    let before = pin(&ledger, &manager).await?;
    assert!(ledger
        .approve_journal(&reviewer, COMPANY, january.id)
        .await
        .is_err());
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, january.id)
            .await?
            .status,
        JournalStatus::Submitted
    );
    assert_eq!(pin(&ledger, &manager).await?, before);
    assert_eq!(
        item(&engine, &reviewer, &january).await?.request.state,
        "awaiting"
    );
    println!("PASS: closed-period posting refusal leaves the approval waiting and posts nothing");

    let stale = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry("Stale plan proof", date(2026, 6, 6)),
        )
        .await?;
    let login = app.user.login("admin@example.test", PASSWORD).await?;
    let live = app.security.guard(&login.token).await?;
    let browser = Browser {
        app: &app,
        token: &login.token,
    };
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let ops = vec![PlannedOperation {
        key: "finance.ledger.journal.approve".into(),
        input: json!({"company_id":COMPANY,"journal_id":stale.id}),
    }];
    let preview = plans.preview(&browser, &ops).await?;
    assert_eq!(preview["valid"], true);
    ledger
        .withdraw_journal(&manager, COMPANY, stale.id, None)
        .await?;
    assert!(matches!(
        plans.commit(&live, &browser, &ops).await,
        Err(bilibi_agent_api::Error::PlanRejected(_)
            | bilibi_agent_api::Error::Backend(bilibi_database::Error::Conflict))
    ));
    assert_eq!(
        ledger
            .get_journal(&manager, COMPANY, stale.id)
            .await?
            .status,
        JournalStatus::Withdrawn
    );
    assert_eq!(pin(&ledger, &manager).await?, before);
    println!("PASS: the real reviewed journal contract refuses its stale preview with no posting");

    let waiting = ledger
        .submit_journal(
            &manager,
            COMPANY,
            &entry(
                "Journal awaiting independent browser approval",
                date(2026, 6, 7),
            ),
        )
        .await?;
    assert_eq!(
        item(&engine, &reviewer, &waiting).await?.request.state,
        "awaiting"
    );
    println!("Browser proof: journal={} exception_journal={} at {origin}; admin@example.test reviews, manager@example.test submitted",waiting.id,browser_exception.id);
    if std::env::args().any(|a| a == "--serve") {
        axum::serve(
            tokio::net::TcpListener::bind(
                std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:43171".into()),
            )
            .await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}
