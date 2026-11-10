//! NE6 running proof: real pages, facades, audited settings, ledger and HTTP
//! client. The clearance walkthrough's local authority is the only stand-in.
use crate::clearance::{certificate, draft, save, serve, StandIn, CLIENT_ID, CLIENT_SECRET};
use crate::{install, sign_in, COMPANY};
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Db, Error};
use bilibi_finance_einvoice::{
    Authority, ConsolidationRequest, Einvoice, SERVICE, SETTINGS, SUBMIT,
};
use bilibi_finance_ledger::{Ledger, PeriodStatus, POST_ENTRIES};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::{civil::Date, Timestamp};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;
type State = Box<dyn LiveState<Scope, Error>>;

async fn mount(
    services: &Services,
    scope: &Scope,
    operation: &'static str,
    month: Date,
) -> Outcome<State> {
    let pages = services.get::<PageService<Scope, Error>>("finance/einvoice.control_pages")?;
    Ok(pages
        .0
        .mount(PageRequest {
            operation,
            scope: Some(scope),
            path: &HashMap::new(),
            query: &HashMap::from([("month".into(), month.strftime("%Y-%m").to_string())]),
            body: &[],
            framed: false,
        })
        .await?
        .unwrap())
}
async fn update(state: &mut State, scope: &Scope, input: LiveInput<'_>) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input,
        })
        .await?)
}
async fn event(state: &mut State, scope: &Scope, value: Value) -> Outcome<LiveBody> {
    update(state, scope, LiveInput::Event(&value)).await
}
fn has(body: &LiveBody, text: &str) {
    assert!(body.html.contains(text), "missing {text}");
}

async fn setting(
    db: &Db,
    services: &Services,
    scope: &Scope,
    target: Target,
    key: &str,
    value: Value,
) -> Outcome<()> {
    assert_eq!(
        save(db, services, scope, target, key, value).await?,
        "stored",
        "setting {key}"
    );
    Ok(())
}

async fn issue(
    einvoice: &Einvoice,
    scope: &Scope,
    code: &str,
    at: Option<Timestamp>,
    large: bool,
) -> Outcome<i64> {
    let mut draft = draft(code, None);
    draft.issued_at = at;
    if large {
        draft.lines[0].quantity = Some(crate::clearance::d("1000"));
    }
    Ok(einvoice
        .issue_document(scope, COMPANY, &draft)
        .await?
        .document
        .id)
}
async fn clear(einvoice: &Einvoice, scope: &Scope, id: i64) -> Outcome<()> {
    einvoice
        .run_submission(scope, COMPANY, id, Timestamp::now())
        .await;
    for _ in 0..3 {
        einvoice.run_poll(scope, COMPANY, id).await;
    }
    assert_eq!(
        einvoice.get_document(scope, COMPANY, id).await?.status,
        "valid"
    );
    Ok(())
}

/// Installs and proves this slice; `--serve` leaves the composed app and local
/// authority running for browser checks. FAKETIME is an external process clock,
/// never a product switch: use 2026-10-03 to exercise an open monthly window.
pub async fn run() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4396".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    crate::without_step_up(&app.services)?;
    let services = &app.services;
    install(&db, services).await?;
    let manager = sign_in(&db, services, 1, 1).await?;
    let admin = sign_in(&db, services, 1, 4).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let einvoice = services.get::<Einvoice>(SERVICE)?;
    let target = Target::Company(OrganisationId::new(COMPANY)?);
    if std::env::args().any(|a| a == "--serve-existing") {
        let url = serve(Arc::new(StandIn::default())).await?;
        setting(
            &db,
            services,
            &admin,
            Target::Global,
            "finance.einvoice.sandbox.api_url",
            json!(url),
        )
        .await?;
        let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4396".into());
        axum::serve(
            tokio::net::TcpListener::bind(bind).await?,
            bilibi_web::router(app),
        )
        .await?;
        return Ok(());
    }
    let now = Timestamp::now();
    let month = now
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .first_of_month()
        .yesterday()?
        .first_of_month();
    let window = Einvoice::authority().consolidation_window(month);
    let mut setup = mount(services, &manager, "setup", month).await?;
    let initial = update(&mut setup, &manager, LiveInput::Refresh).await?;
    has(&initial, "einvoice-certificate-missing");
    has(&initial, "Supplier profile incomplete");
    has(
        &event(&mut setup, &manager, json!({"type":"test_connection"})).await?,
        "set the client ID and client secret",
    );
    let url = serve(Arc::new(StandIn::default())).await?;
    setting(
        &db,
        services,
        &admin,
        Target::Global,
        "finance.einvoice.sandbox.api_url",
        json!(url),
    )
    .await?;
    for (key, value) in [
        ("supplier.tax_id", json!("C2584563222")),
        ("supplier.registration_number", json!("201901234567")),
        ("supplier.industry_code", json!("47111")),
        ("supplier.business_activity", json!("Retail")),
        ("supplier.phone", json!("+60123456789")),
        ("supplier.address_line1", json!("2 Jalan Contoh")),
        ("supplier.city", json!("Kuala Lumpur")),
        ("supplier.state_code", json!("14")),
        ("supplier.country_code", json!("MYS")),
        ("sandbox.client_id", json!(CLIENT_ID)),
        ("sandbox.client_secret", json!("wrong-for-proof")),
        (
            "sandbox.secret_expires_on",
            json!(now.to_zoned(jiff::tz::TimeZone::UTC).date().to_string()),
        ),
    ] {
        setting(
            &db,
            services,
            &manager,
            target,
            &format!("finance.einvoice.{key}"),
            value,
        )
        .await?;
    }
    has(
        &event(&mut setup, &manager, json!({"type":"test_connection"})).await?,
        "MyInvois did not accept the client ID and secret.",
    );
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.sandbox.client_secret",
        json!(CLIENT_SECRET),
    )
    .await?;
    has(
        &event(&mut setup, &manager, json!({"type":"test_connection"})).await?,
        "Connected: the authority accepted the sandbox credentials.",
    );
    // HTTP 429 with Retry-After, through Base Integration, not a mocked facade.
    let rate = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let rate_url = format!("http://{}", rate.local_addr()?);
    tokio::spawn(async move {
        let router = axum::Router::new().route(
            "/connect/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("Retry-After", "37")],
                    "not displayed",
                )
            }),
        );
        let _ = axum::serve(rate, router).await;
    });
    setting(
        &db,
        services,
        &admin,
        Target::Global,
        "finance.einvoice.sandbox.api_url",
        json!(rate_url),
    )
    .await?;
    has(
        &event(&mut setup, &manager, json!({"type":"test_connection"})).await?,
        "The authority is limiting logins. Try again in 37 seconds.",
    );
    setting(
        &db,
        services,
        &admin,
        Target::Global,
        "finance.einvoice.sandbox.api_url",
        json!(url),
    )
    .await?;
    let (cert, key) = certificate()?;
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.certificate_pem",
        json!(cert),
    )
    .await?;
    let missing_key = update(&mut setup, &manager, LiveInput::Refresh).await?;
    has(&missing_key, "Missing or does not match the certificate");
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.private_key_pem",
        json!(key),
    )
    .await?;
    let configured = update(&mut setup, &manager, LiveInput::Refresh).await?;
    has(&configured, "Matches the certificate");
    has(&configured, "Renew soon");
    has(&configured, "Every field a document needs is set.");
    assert!(!configured.html.contains(CLIENT_SECRET));
    assert!(!configured.html.contains("PRIVATE KEY"));
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.supplier.tax_id",
        json!("C2584563223"),
    )
    .await?;
    has(
        &update(&mut setup, &manager, LiveInput::Refresh).await?,
        "Differs from the certificate",
    );
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.supplier.tax_id",
        json!("C2584563222"),
    )
    .await?;
    println!(
        "NE6 setup: missing/complete profile; missing/matching key; TIN mismatch; expiry; no secrets; missing/400/success/429 login feedback passed"
    );

    let source = issue(
        &einvoice,
        &manager,
        "NE6-SALE-A",
        Some(
            window
                .from
                .checked_add(jiff::SignedDuration::from_hours(24))?,
        ),
        false,
    )
    .await?;
    issue(
        &einvoice,
        &manager,
        "NE6-INDIVIDUAL",
        Some(
            window
                .from
                .checked_add(jiff::SignedDuration::from_hours(48))?,
        ),
        true,
    )
    .await?;
    let mut consolidation = mount(services, &manager, "consolidation", month).await?;
    let loaded = update(&mut consolidation, &manager, LiveInput::Refresh).await?;
    has(&loaded, "NE6-SALE-A");
    has(&loaded, "NE6-INDIVIDUAL");
    has(&loaded, "Documents that need an individual e-invoice");
    if window.until <= now && now < window.closes_at {
        has(&loaded, "einvoice-consolidation-issue");
        let invalid = event(
            &mut consolidation,
            &manager,
            json!({"type":"issue", "issue[code_number]":"   "}),
        )
        .await?;
        has(
            &invalid,
            "The consolidated document was refused: code_number can&#39;t be blank.",
        );
        // A later arrival must not enter the page's already displayed snapshot.
        let late = issue(
            &einvoice,
            &manager,
            "NE6-SALE-LATER",
            Some(
                window
                    .from
                    .checked_add(jiff::SignedDuration::from_hours(72))?,
            ),
            false,
        )
        .await?;
        let issued = event(
            &mut consolidation,
            &manager,
            json!({"type":"issue", "issue[code_number]":"NE6-CONS-A"}),
        )
        .await?;
        assert!(issued
            .redirect
            .as_ref()
            .unwrap()
            .starts_with("/finance/einvoice/documents/"));
        assert_eq!(
            issued.flash.as_ref().unwrap().1,
            "Consolidated document NE6-CONS-A issued."
        );
        let consolidated_id: i64 = issued
            .redirect
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .parse()?;
        let view = einvoice
            .get_document(&manager, COMPANY, consolidated_id)
            .await?;
        assert_eq!(
            view.sources
                .iter()
                .map(|s| s.document.id)
                .collect::<Vec<_>>(),
            vec![source]
        );
        let mut stale = mount(services, &manager, "consolidation", month).await?;
        einvoice
            .issue_consolidated_document(
                &manager,
                COMPANY,
                &ConsolidationRequest {
                    month,
                    code_number: Some("NE6-CONS-OTHER".into()),
                    document_ids: Some(vec![late]),
                },
                now,
            )
            .await?;
        let refused = event(
            &mut stale,
            &manager,
            json!({"type":"issue", "issue[code_number]":"NE6-STALE"}),
        )
        .await?;
        has(&refused, "can no longer be consolidated; reload the page.");
        assert!(!refused.html.contains("NE6-SALE-LATER"));
        println!(
            "NE6 consolidation: exact displayed snapshot excludes later arrivals; competing consolidation refused and refreshed; success redirect/flash passed"
        );
    } else {
        assert!(!loaded.html.contains("id=\"einvoice-consolidation-issue\""));
        println!("NE6 consolidation: closed window withheld; run at 2026-10-03 for issuance proof");
    }
    issue(
        &einvoice,
        &manager,
        "NE6-DRAFT-SALE",
        Some(
            window
                .from
                .checked_add(jiff::SignedDuration::from_hours(80))?,
        ),
        false,
    )
    .await?;
    update(&mut consolidation, &manager, LiveInput::Refresh).await?;
    event(
        &mut consolidation,
        &manager,
        json!({"type":"issue_change", "issue[code_number]":"Retained <draft>"}),
    )
    .await?;
    let changed = event(
        &mut consolidation,
        &manager,
        json!({"type":"month", "month":"2026-10"}),
    )
    .await?;
    assert_eq!(changed.query.get("month").unwrap(), "2026-10");
    let back = update(
        &mut consolidation,
        &manager,
        LiveInput::Params(&HashMap::from([(
            "month".into(),
            month.strftime("%Y-%m").to_string(),
        )])),
    )
    .await?;
    assert_eq!(
        back.query.get("month").unwrap(),
        &month.strftime("%Y-%m").to_string()
    );
    if window.until <= now && now < window.closes_at {
        has(&back, "value=\"Retained &#60;draft&#62;\"");
        has(&back, "einvoice-consolidation-issue");
    }
    let mut reconciliation = mount(services, &manager, "reconciliation", month).await?;
    let id = issue(&einvoice, &manager, "NE6-RETRY-POST", None, false).await?;
    clear(&einvoice, &manager, id).await?;
    let failure = event(
        &mut reconciliation,
        &manager,
        json!({"type":"retry_posting", "id":id.to_string()}),
    )
    .await?;
    has(&failure, "It failed again:");
    for (field, account) in [
        ("receivable_account", "1100"),
        ("revenue_account", "4000"),
        ("tax_account", "2100"),
    ] {
        setting(
            &db,
            services,
            &manager,
            target,
            &format!("finance.einvoice.posting.{field}"),
            json!(account),
        )
        .await?;
    }
    for capability in [SUBMIT, POST_ENTRIES] {
        grants
            .set_direct(&admin, 1, capability, Some(false))
            .await?;
        let refused = event(
            &mut reconciliation,
            &manager,
            json!({"type":"retry_posting", "id":id.to_string()}),
        )
        .await?;
        has(
            &refused,
            "You do not have permission to post to the ledger.",
        );
        assert!(einvoice
            .get_document(&manager, COMPANY, id)
            .await?
            .ledger_entry_id
            .is_none());
        grants.set_direct(&admin, 1, capability, None).await?;
    }
    has(
        &event(
            &mut reconciliation,
            &manager,
            json!({"type":"retry_posting", "id":id.to_string()}),
        )
        .await?,
        "The entry was posted.",
    );
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    let year = ledger.list_fiscal_years(&manager, COMPANY).await?.remove(0);
    let current = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    let mut closed = None;
    for period in year
        .periods
        .iter()
        .filter(|p| p.start_date <= current && p.status == PeriodStatus::Open)
    {
        ledger
            .close_period(
                &manager,
                COMPANY,
                period.id,
                Some("NE6 closed-period reversal proof"),
            )
            .await?;
        closed = Some(period.id);
    }
    let cancelled = einvoice
        .cancel_document(&manager, COMPANY, id, "NE6 reversal proof")
        .await?;
    assert!(cancelled.reversal_entry_id.is_none());
    has(
        &event(
            &mut reconciliation,
            &manager,
            json!({"type":"retry_reversal", "id":id.to_string()}),
        )
        .await?,
        "It failed again:",
    );
    ledger
        .reopen_period(
            &manager,
            COMPANY,
            closed.unwrap(),
            "NE6 restore writable period",
        )
        .await?;
    has(
        &event(
            &mut reconciliation,
            &manager,
            json!({"type":"retry_reversal", "id":id.to_string()}),
        )
        .await?,
        "The entry was reversed.",
    );
    println!(
        "NE6 reconciliation: actual failed posting, grant revocation, successful posting, closed-period reversal failure and successful retry passed"
    );
    // Leave one actual failed posting and candidates for browser review.
    setting(
        &db,
        services,
        &manager,
        target,
        "finance.einvoice.posting.revenue_account",
        Value::Null,
    )
    .await?;
    let browser_retry = issue(&einvoice, &manager, "NE6-BROWSER-RETRY", None, false).await?;
    clear(&einvoice, &manager, browser_retry).await?;
    issue(
        &einvoice,
        &manager,
        "NE6-BROWSER-SALE",
        Some(
            window
                .from
                .checked_add(jiff::SignedDuration::from_hours(96))?,
        ),
        false,
    )
    .await?;
    // The viewer joins the company in disposable fixture data, never product seeds.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Lee Viewer','human','lee@example.test') ON CONFLICT DO NOTHING; INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','viewer@example.test') ON CONFLICT DO NOTHING; UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING").await?;
    let viewer = sign_in(&db, services, 1, 2).await?;
    let mut read_only = mount(services, &viewer, "reconciliation", month).await?;
    let body = update(&mut read_only, &viewer, LiveInput::Refresh).await?;
    assert!(!body.html.contains("data-event=\"retry_posting\""));
    let mut no_setup = mount(services, &viewer, "setup", month).await?;
    has(
        &update(&mut no_setup, &viewer, LiveInput::Refresh).await?,
        "Setup unavailable",
    );
    grants.set_direct(&admin, 1, SETTINGS, Some(false)).await?;
    has(
        &event(&mut setup, &manager, json!({"type":"test_connection"})).await?,
        "you do not have permission to manage e-invoice settings.",
    );
    grants.set_direct(&admin, 1, SETTINGS, None).await?;
    println!("NE6: read-only controls withheld; setup refusal and stale manage grant passed");
    if std::env::args().any(|a| a == "--serve") {
        let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4396".into());
        println!("NE6 browser app ready; authority remains local; manager/viewer fixture accounts");
        axum::serve(
            tokio::net::TcpListener::bind(bind).await?,
            bilibi_web::router(app),
        )
        .await?;
    }
    Ok(())
}
