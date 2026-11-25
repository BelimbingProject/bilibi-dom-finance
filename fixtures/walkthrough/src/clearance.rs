//! The clearance section of the walkthrough: Company 002's documents issued
//! through the e-invoice facade, submitted by the real submit and poll jobs
//! on Base Queue, and refreshed and cancelled, against a stand-in for the
//! authority's API served on this machine's loopback for the run. The
//! stand-in speaks the MyInvois API's shapes (login, submit, submission
//! status, document details, recent documents, cancel), checks each
//! submission's hash and verifies its signature, and answers by the code
//! number: `REFUSE` is refused, `INVALID` found invalid, `RATE` rate limited
//! once, `DUP` a duplicate it already holds, `DUPLOST` a duplicate it cannot
//! find, `LOSTREPLY` accepted with its answer lost, `LATE` past its
//! cancellation window; anything else is valid. Every printed line is the
//! facade's, the jobs' or the database's answer; the stand-in is the only
//! thing that is not the real authority.
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use bilibi_database::{Db, Error, MutationBoundary};
use bilibi_finance_einvoice::myinvois::signer;
use bilibi_finance_einvoice::{
    DocumentView, Draft, DraftAddress, DraftLine, DraftParty, Einvoice, EinvoiceError, SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{Config, Executor};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::{SignedDuration, Timestamp};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) type Outcome<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The company the section clears documents for.
pub(crate) const COMPANY: i64 = 2;
pub(crate) const CLIENT_ID: &str = "walkthrough-client";
pub(crate) const CLIENT_SECRET: &str = "walkthrough-secret";

pub(crate) fn show<T: Debug>(step: &str, result: Result<T, EinvoiceError>) {
    match result {
        Ok(value) => println!("{step}: {value:?}"),
        Err(EinvoiceError::Invalid(errors)) => {
            let errors: Vec<String> = errors
                .iter()
                .map(|error| format!("{} {}", error.field, error.message))
                .collect();
            println!("{step}: refused ({})", errors.join("; "));
        }
        Err(error) => println!("{step}: refused ({error:?}: {error})"),
    }
}

/// A document on a few lines: its status, identifiers, attention flags and
/// every event with its reason.
pub(crate) fn brief(view: &DocumentView) -> String {
    let events: Vec<String> = view
        .events
        .iter()
        .map(|event| {
            let mut line = format!("{} {}", event.sequence, event.event);
            if let Some(status) = &event.authority_status {
                line.push_str(&format!(" [{status}]"));
            }
            if let Some(status) = event.http_status {
                line.push_str(&format!(" http {status}"));
            }
            if let Some(reason) = &event.reason {
                line.push_str(&format!(" — {reason}"));
            }
            line
        })
        .collect();
    format!(
        "{} {} status={} uuid={:?} long_id={:?} validation_url={:?} attention={:?} queued={}\n    {}",
        view.document.code_number,
        view.document.environment,
        view.status,
        view.document_uuid,
        view.long_id,
        view.validation_url,
        view.attention,
        view.submission_queued,
        events.join("\n    ")
    )
}

pub(crate) fn shown(
    step: &str,
    result: Result<DocumentView, EinvoiceError>,
) -> Option<DocumentView> {
    match result {
        Ok(view) => {
            println!("{step}:\n  {}", brief(&view));
            Some(view)
        }
        Err(error) => {
            show::<()>(step, Err(error));
            None
        }
    }
}

/// What the stand-in holds of one document it accepted.
#[derive(Clone)]
struct Held {
    code: String,
    submission_uid: String,
    uuid: String,
    long_id: String,
    status: &'static str,
    received: String,
    rejection_requested: bool,
}

/// The stand-in authority's state for the run.
#[derive(Default)]
pub(crate) struct StandIn {
    logins: AtomicUsize,
    token: Mutex<String>,
    /// The next authorized call answers 401, as an expired token does.
    revoke_token: AtomicBool,
    /// Every call answers 503.
    outage: AtomicBool,
    held: Mutex<Vec<Held>>,
    reads: Mutex<HashMap<String, usize>>,
    rate_limited: Mutex<Vec<String>>,
    log: Mutex<Vec<String>>,
    counter: AtomicUsize,
}

impl StandIn {
    fn note(&self, line: String) {
        self.log.lock().expect("log").push(line);
    }
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().expect("log"))
    }
    fn next(&self) -> usize {
        self.counter.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn hold(&self, code: &str) -> Held {
        let n = self.next();
        let pending_retry = code == "NE5-RECHECK" && self.find_code(code).is_some();
        let held = Held {
            code: code.to_owned(),
            submission_uid: format!("SUBMISSION-{n:04}"),
            uuid: format!("DOCUMENT-{n:04}"),
            long_id: format!("LONGID-{n:04}-WALKTHROUGH"),
            status: if pending_retry {
                "Submitted"
            } else if code.contains("INVALID") {
                "Invalid"
            } else {
                "Valid"
            },
            received: Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
            rejection_requested: code == "BROWSER-E-REJECTION",
        };
        self.held.lock().expect("held").push(held.clone());
        held
    }
    fn find(&self, uuid: &str) -> Option<Held> {
        self.held
            .lock()
            .expect("held")
            .iter()
            .find(|held| held.uuid == uuid)
            .cloned()
    }
    pub(crate) fn invalidate(&self, code: &str) {
        self.set(code, |held| held.status = "Invalid");
    }

    pub(crate) fn finish_retry(&self, uuid: &str) {
        let mut held = self.held.lock().expect("held");
        held.iter_mut()
            .find(|held| held.uuid == uuid)
            .expect("retry")
            .status = "Valid";
    }

    fn set(&self, code: &str, change: impl Fn(&mut Held)) {
        for held in self.held.lock().expect("held").iter_mut() {
            if held.code == code {
                change(held);
            }
        }
    }
}

type Shared = State<Arc<StandIn>>;

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": {"code": "ServiceUnavailable"}})),
    )
        .into_response()
}

/// The bearer token check every API call makes.
fn refused_token(standin: &StandIn, headers: &HeaderMap) -> Option<Response> {
    if standin.outage.load(Ordering::SeqCst) {
        return Some(unavailable());
    }
    let expected = format!("Bearer {}", standin.token.lock().expect("token"));
    let sent = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    if standin.revoke_token.swap(false, Ordering::SeqCst) || sent != Some(expected.as_str()) {
        standin.note("401: the token is no longer accepted".into());
        return Some(StatusCode::UNAUTHORIZED.into_response());
    }
    None
}

async fn token(State(standin): Shared, Form(form): Form<HashMap<String, String>>) -> Response {
    if standin.outage.load(Ordering::SeqCst) {
        return unavailable();
    }
    if form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
        || form.get("grant_type").map(String::as_str) != Some("client_credentials")
        || form.get("scope").map(String::as_str) != Some("InvoicingAPI")
    {
        standin.note("login refused: wrong client credentials".into());
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    let n = standin.logins.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("TOKEN-{n}");
    *standin.token.lock().expect("token") = token.clone();
    standin.note(format!("login {n}: token issued"));
    Json(json!({"access_token": token, "token_type": "Bearer", "expires_in": 3600, "scope": "InvoicingAPI"}))
        .into_response()
}

async fn submit(State(standin): Shared, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(refused) = refused_token(&standin, &headers) {
        return refused;
    }
    let document = &body["documents"][0];
    let code = document["codeNumber"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let bytes = BASE64
        .decode(document["document"].as_str().unwrap_or_default())
        .unwrap_or_default();
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let hash_ok = document["documentHash"].as_str() == Some(hash.as_str());
    let signature = match signer::verify(&bytes) {
        Ok(()) => "signature verified".to_owned(),
        Err(error) => format!("signature not verified ({error})"),
    };
    standin.note(format!(
        "submit {code} ({} bytes, {}): hash {}, {signature}",
        bytes.len(),
        document["format"].as_str().unwrap_or_default(),
        if hash_ok { "matches" } else { "differs" }
    ));
    let duplicate = |held: bool| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            [("Retry-After", "2")],
            Json(json!({"error": {"code": "DuplicateSubmission",
                "message": format!("Duplicated submission; {}", if held { "held" } else { "unknown" })}})),
        )
            .into_response()
    };
    if code.contains("DUPLOST") {
        return duplicate(false);
    }
    if code.contains("DUP") {
        standin.hold(&code);
        return duplicate(true);
    }
    if code.contains("RATE") {
        let mut limited = standin.rate_limited.lock().expect("rate");
        if !limited.contains(&code) {
            limited.push(code);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "1")],
                Json(json!({"error": {"code": "TooManyRequests"}})),
            )
                .into_response();
        }
    }
    if code.contains("LOSTREPLY") && standin.find_code(&code).is_none() {
        standin.hold(&code);
        return unavailable();
    }
    if code.contains("REFUSE") {
        let n = standin.next();
        return (
            StatusCode::ACCEPTED,
            Json(
                json!({"submissionUid": format!("SUBMISSION-{n:04}"), "acceptedDocuments": [],
                "rejectedDocuments": [{"invoiceCodeNumber": code, "error": {"code": "CF321",
                "message": "Document issue date is in the future",
                "details": [{"message": "issueDate is after the submission time"}]}}]}),
            ),
        )
            .into_response();
    }
    let held = standin.hold(&code);
    (
        StatusCode::ACCEPTED,
        Json(json!({"submissionUid": held.submission_uid,
            "acceptedDocuments": [{"uuid": held.uuid, "invoiceCodeNumber": code}],
            "rejectedDocuments": []})),
    )
        .into_response()
}

impl StandIn {
    fn find_code(&self, code: &str) -> Option<Held> {
        self.held
            .lock()
            .expect("held")
            .iter()
            .find(|held| held.code == code)
            .cloned()
    }
}

async fn submission_status(
    State(standin): Shared,
    headers: HeaderMap,
    Path(uid): Path<String>,
) -> Response {
    if let Some(refused) = refused_token(&standin, &headers) {
        return refused;
    }
    let held: Vec<Held> = standin
        .held
        .lock()
        .expect("held")
        .iter()
        .filter(|held| held.submission_uid == uid)
        .cloned()
        .collect();
    let Some(first) = held.first() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let reads = {
        let mut reads = standin.reads.lock().expect("reads");
        let count = reads.entry(uid.clone()).or_default();
        *count += 1;
        *count
    };
    // The first read of each submission finds it still processing.
    let overall = if reads == 1 || first.status == "Submitted" {
        "In Progress"
    } else {
        first.status
    };
    standin.note(format!("submission {uid} read {reads}: {overall}"));
    Json(json!({"submissionUid": uid, "documentCount": 1, "overallStatus": overall,
        "documentSummary": [{"uuid": first.uuid, "internalId": first.code, "status": first.status}]}))
    .into_response()
}

async fn details(State(standin): Shared, headers: HeaderMap, Path(uuid): Path<String>) -> Response {
    if let Some(refused) = refused_token(&standin, &headers) {
        return refused;
    }
    let Some(held) = standin.find(&uuid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "NotFound", "message": "not authorized to get document details"}})),
        )
            .into_response();
    };
    standin.note(format!(
        "details {uuid}: {}{}",
        held.status,
        if held.rejection_requested {
            ", rejection requested"
        } else {
            ""
        }
    ));
    let mut body = json!({"uuid": held.uuid, "submissionUid": held.submission_uid,
        "internalId": held.code, "status": held.status, "dateTimeReceived": held.received,
        "documentStatusReason": Value::Null});
    if matches!(held.status, "Valid" | "Cancelled") {
        body["longId"] = json!(held.long_id);
        body["dateTimeValidated"] = if held.code == "BROWSER-E-EXPIRED" {
            json!((Timestamp::now() - SignedDuration::from_hours(73))
                .strftime("%Y-%m-%dT%H:%M:%SZ")
                .to_string())
        } else {
            json!(held.received)
        };
    } else if held.status == "Invalid" {
        body["validationResults"] = json!({"status": "Invalid", "validationSteps": [
            {"status": "Valid", "name": "Step01-Structure Validator"},
            {"status": "Invalid", "name": "Step03-Codes Validator", "error": {
                "errorCode": "CV302", "error": "Invalid tax type. Please refer to https://sdk.myinvois.hasil.gov.my/codes/",
                "innerError": [{"errorCode": "CV303", "error": "Tax type 01 is not registered for the supplier"}]}}]});
    }
    if held.status == "Cancelled" {
        body["documentStatusReason"] = json!("Issued in error");
    }
    if held.rejection_requested {
        body["rejectRequestDateTime"] = json!(held.received);
        body["documentStatusReason"] = json!("Wrong buyer address");
    }
    Json(body).into_response()
}

async fn recent(
    State(standin): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = refused_token(&standin, &headers) {
        return refused;
    }
    let result: Vec<Value> = standin
        .held
        .lock()
        .expect("held")
        .iter()
        .map(|held| {
            json!({"uuid": held.uuid, "submissionUid": held.submission_uid,
                "internalId": held.code, "dateTimeReceived": held.received})
        })
        .collect();
    standin.note(format!(
        "recent documents from {} to {}: {} held",
        query.get("submissionDateFrom").map_or("?", String::as_str),
        query.get("submissionDateTo").map_or("?", String::as_str),
        result.len()
    ));
    Json(json!({"result": result, "metadata": {"totalPages": 1, "totalCount": result.len()}}))
        .into_response()
}

async fn cancel(
    State(standin): Shared,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(refused) = refused_token(&standin, &headers) {
        return refused;
    }
    let Some(held) = standin.find(&uuid) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    standin.note(format!(
        "cancel {uuid} ({}) because {:?}",
        held.code, body["reason"]
    ));
    if held.code == "BROWSER-E-TIMEOUT" {
        standin.set(&held.code, |held| held.status = "Cancelled");
        // The authority accepted the request, but its response arrives after
        // Integration's 30-second timeout. A refresh must resolve the outcome.
        tokio::time::sleep(Duration::from_secs(35)).await;
        return Json(json!({"uuid": uuid, "status": "Cancelled"})).into_response();
    }
    if held.code.contains("LATE") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "OperationPeriodOver",
                "message": "The time limit for cancellation has expired"}})),
        )
            .into_response();
    }
    standin.set(&held.code, |held| held.status = "Cancelled");
    Json(json!({"uuid": uuid, "status": "Cancelled"})).into_response()
}

/// Serves the stand-in on a free loopback port and answers its base URL.
pub(crate) async fn serve(standin: Arc<StandIn>) -> Outcome<String> {
    let app = Router::new()
        .route("/connect/token", post(token))
        .route("/api/v1.0/documentsubmissions/", post(submit))
        .route(
            "/api/v1.0/documentsubmissions/{uid}",
            get(submission_status),
        )
        .route("/api/v1.0/documents/recent", get(recent))
        .route("/api/v1.0/documents/{uuid}/details", get(details))
        .route("/api/v1.0/documents/state/{uuid}/state", put(cancel))
        .with_state(standin);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(url)
}

/// Saves one setting as the Settings screen would.
pub(crate) async fn save(
    db: &Db,
    services: &Services,
    scope: &Scope,
    target: Target,
    key: &str,
    value: Value,
) -> Outcome<String> {
    let mutations = services.get::<MutationBoundary>("base/database.mutations")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let mut client = db.get().await?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("finance.einvoice.walkthrough.settings.save"),
        )
        .await?;
    match settings.put(&mut tx, scope, target, key, value).await {
        Ok(()) => {
            tx.commit().await?;
            Ok("stored".into())
        }
        Err(error) => {
            tx.rollback().await?;
            Ok(format!("refused ({error:?})"))
        }
    }
}

/// A throwaway RSA certificate whose subject carries the company's tax and
/// registration numbers, and its key, as PEM; the files are removed on return.
pub(crate) fn certificate() -> Outcome<(String, String)> {
    let dir = std::env::temp_dir().join(format!("einvoice-clearance-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let status = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "30", "-subj",
            "/C=MY/O=Walkthrough Two Sdn Bhd/organizationIdentifier=C2584563222/serialNumber=201901234567/CN=Walkthrough Two",
            "-keyout"])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()?
        .status;
    let pems = (
        std::fs::read_to_string(&cert),
        std::fs::read_to_string(&key),
    );
    std::fs::remove_dir_all(&dir)?;
    if !status.success() {
        return Err("openssl failed".into());
    }
    Ok((pems.0?, pems.1?))
}

pub(crate) fn d(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("a decimal")
}

pub(crate) fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

pub(crate) fn buyer() -> DraftParty {
    DraftParty {
        name: text("Buyer Two Sdn Bhd"),
        tax_id: text("C29702635060"),
        registration_type: text("BRN"),
        registration_number: text("D12345678"),
        sales_tax_registration: None,
        email: text("buyer@example.test"),
        phone: text("+60123456789"),
        industry_code: None,
        business_activity: None,
        address: Some(DraftAddress {
            lines: vec!["Lot 66".into(), "Bangunan Merdeka".into()],
            city: text("Kuala Lumpur"),
            postcode: text("50480"),
            state_code: text("14"),
            country_code: text("MYS"),
        }),
    }
}

/// An invoice (or, with `original`, a credit note) on two lines.
pub(crate) fn draft(code: &str, original: Option<i64>) -> Draft {
    let line = |description: &str, quantity: &str, price: &str| DraftLine {
        description: text(description),
        classification_code: text("003"),
        tariff_code: None,
        origin_country_code: None,
        quantity: Some(d(quantity)),
        unit_code: text("C62"),
        unit_price: Some(d(price)),
        discount_amount: None,
        charge_amount: None,
        tax_type: text("01"),
        tax_rate: Some(d("6")),
        tax_exemption_reason: None,
        tax_amount: None,
    };
    Draft {
        document_type: text(if original.is_some() { "02" } else { "01" }),
        code_number: text(code),
        issued_at: None,
        currency_code: text("MYR"),
        exchange_rate: None,
        payment_means_code: text("03"),
        payment_terms: None,
        billing_period_start: None,
        billing_period_end: None,
        billing_period_description: None,
        original_document_id: original,
        buyer: Some(buyer()),
        supplier: None,
        source: None,
        lines: if original.is_some() {
            vec![line("Returned keyboard", "1", "40.00")]
        } else {
            vec![
                line("Keyboard", "2", "40.00"),
                line("Support hours", "1.5", "120.00"),
            ]
        },
    }
}

/// Waits until no e-invoice job is waiting or running, then prints them.
pub(crate) async fn jobs_settle(client: &tokio_postgres::Client) -> Outcome<()> {
    for _ in 0..240 {
        let waiting: i64 = client
            .query_one(
                "SELECT count(*) FROM base_queue_jobs WHERE worker_id LIKE 'finance/einvoice-%' \
                 AND state IN ('available','scheduled','executing','retryable')",
                &[],
            )
            .await?
            .get(0);
        if waiting == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

pub(crate) async fn job_states(client: &tokio_postgres::Client, since: i64) -> Outcome<String> {
    let rows = client
        .query(
            "SELECT id, worker_id, state, attempt, COALESCE(failure_code, '-'), args::text \
             FROM base_queue_jobs WHERE worker_id LIKE 'finance/einvoice-%' AND id > $1 ORDER BY id",
            &[&since],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            format!(
                "job {} {} {} attempt {} code={} args={}",
                r.get::<_, i64>(0),
                r.get::<_, String>(1),
                r.get::<_, String>(2),
                r.get::<_, i32>(3),
                r.get::<_, String>(4),
                r.get::<_, String>(5)
            )
        })
        .collect::<Vec<_>>()
        .join("\n  "))
}

pub(crate) async fn last_job(client: &tokio_postgres::Client) -> Outcome<i64> {
    Ok(client
        .query_one("SELECT COALESCE(max(id), 0) FROM base_queue_jobs", &[])
        .await?
        .get(0))
}

/// Runs the section: `manager` clears Company 002's documents, `viewer` holds
/// only `finance.einvoice.view`, `admin` is the platform operator.
pub async fn run(
    db: &Db,
    services: &Services,
    manager: &Scope,
    viewer: &Scope,
    admin: &Scope,
) -> Outcome<()> {
    println!("\n== E-invoice clearance against a stand-in authority (Company 002)");
    let einvoice = services.get::<Einvoice>(SERVICE)?;
    let standin = Arc::new(StandIn::default());
    let url = serve(standin.clone()).await?;
    let raw = db.get().await?;
    let company = Target::Company(OrganisationId::new(COMPANY)?);

    println!("\n-- The sandbox API URL: https, or a stand-in on this machine's loopback");
    for (key, value) in [
        (
            "finance.einvoice.sandbox.api_url",
            json!("http://example.com"),
        ),
        (
            "finance.einvoice.sandbox.api_url",
            json!(format!("{url}/path")),
        ),
        ("finance.einvoice.production.api_url", json!(url.clone())),
        ("finance.einvoice.sandbox.api_url", json!(url.clone())),
    ] {
        println!(
            "operator sets {key} to {value}: {}",
            save(db, services, admin, Target::Global, key, value.clone()).await?
        );
    }

    println!("\n-- Company 002 before it has credentials");
    let profile = [
        ("finance.einvoice.environment", json!("sandbox")),
        ("finance.einvoice.document_version", json!("1.1")),
        ("finance.einvoice.supplier.tax_id", json!("C2584563222")),
        (
            "finance.einvoice.supplier.registration_number",
            json!("201901234567"),
        ),
        ("finance.einvoice.supplier.industry_code", json!("47111")),
        (
            "finance.einvoice.supplier.business_activity",
            json!("Retail"),
        ),
        ("finance.einvoice.supplier.phone", json!("+60123456789")),
        (
            "finance.einvoice.supplier.address_line1",
            json!("2 Jalan Contoh"),
        ),
        ("finance.einvoice.supplier.city", json!("Kuala Lumpur")),
        ("finance.einvoice.supplier.state_code", json!("14")),
        ("finance.einvoice.supplier.country_code", json!("MYS")),
    ];
    for (key, value) in profile {
        save(db, services, manager, company, key, value).await?;
    }
    println!(
        "manager saved the sandbox environment, version 1.1 (signed) and the supplier profile"
    );
    show(
        "test connection without credentials",
        einvoice.test_connection(manager, COMPANY).await,
    );
    let first = shown(
        "manager issues invoice NE3-INV-0001",
        einvoice
            .issue_document(manager, COMPANY, &draft("NE3-INV-0001", None))
            .await,
    )
    .ok_or(Error::Internal)?;
    show(
        "manager submits it without credentials",
        einvoice
            .submit_document(manager, COMPANY, first.document.id)
            .await,
    );
    save(
        db,
        services,
        manager,
        company,
        "finance.einvoice.sandbox.client_id",
        json!(CLIENT_ID),
    )
    .await?;
    save(
        db,
        services,
        manager,
        company,
        "finance.einvoice.sandbox.client_secret",
        json!("not-the-secret"),
    )
    .await?;
    show(
        "test connection with a wrong secret",
        einvoice.test_connection(manager, COMPANY).await,
    );
    save(
        db,
        services,
        manager,
        company,
        "finance.einvoice.sandbox.client_secret",
        json!(CLIENT_SECRET),
    )
    .await?;
    show(
        "test connection",
        einvoice.test_connection(manager, COMPANY).await,
    );
    show(
        "viewer tests the connection",
        einvoice.test_connection(viewer, COMPANY).await,
    );
    show(
        "manager submits version 1.1 without a signing certificate",
        einvoice
            .submit_document(manager, COMPANY, first.document.id)
            .await,
    );
    let (certificate, key) = certificate()?;
    save(
        db,
        services,
        manager,
        company,
        "finance.einvoice.certificate_pem",
        json!(certificate),
    )
    .await?;
    save(
        db,
        services,
        manager,
        company,
        "finance.einvoice.private_key_pem",
        json!(key),
    )
    .await?;
    println!("manager stored a signing certificate and its key");

    println!("\n-- Issue: refusals");
    show(
        "manager issues NE3-INV-0001 again",
        einvoice
            .issue_document(manager, COMPANY, &draft("NE3-INV-0001", None))
            .await
            .map(|view| view.document.id),
    );
    let mut bad = draft("NE3-BAD-1", None);
    bad.lines[0].tax_type = text("99");
    bad.buyer.as_mut().expect("buyer").tax_id = text("12345");
    show(
        "manager issues a draft with an unknown tax type and a malformed buyer TIN",
        einvoice
            .issue_document(manager, COMPANY, &bad)
            .await
            .map(|view| view.document.id),
    );
    show(
        "viewer issues NE3-VIEWER-1",
        einvoice
            .issue_document(viewer, COMPANY, &draft("NE3-VIEWER-1", None))
            .await
            .map(|view| view.document.id),
    );

    println!("\n-- Submit: the queued jobs run as the manager");
    show(
        "viewer submits NE3-INV-0001",
        einvoice
            .submit_document(viewer, COMPANY, first.document.id)
            .await,
    );
    let mark = last_job(&raw).await?;
    show(
        "manager submits NE3-INV-0001",
        einvoice
            .submit_document(manager, COMPANY, first.document.id)
            .await,
    );
    show(
        "manager submits it again while the job waits",
        einvoice
            .submit_document(manager, COMPANY, first.document.id)
            .await,
    );
    shown(
        "before the executor runs",
        einvoice
            .get_document(manager, COMPANY, first.document.id)
            .await,
    );
    let executor = Executor::from_services(
        services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(120),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));
    jobs_settle(&raw).await?;
    println!("jobs:\n  {}", job_states(&raw, mark).await?);
    println!("stand-in saw:\n  {}", standin.drain().join("\n  "));
    let first = shown(
        "after the jobs",
        einvoice
            .get_document(manager, COMPANY, first.document.id)
            .await,
    )
    .ok_or(Error::Internal)?;
    show(
        "manager submits the valid document",
        einvoice
            .submit_document(manager, COMPANY, first.document.id)
            .await,
    );

    println!("\n-- Every outcome of a submission");
    let mark_outcomes = last_job(&raw).await?;
    let mut documents: BTreeMap<&str, i64> = BTreeMap::new();
    for code in [
        "NE3-REFUSE-1",
        "NE3-INVALID-1",
        "NE3-RATE-1",
        "NE3-DUP-1",
        "NE3-DUPLOST-1",
        "NE3-LOSTREPLY-1",
        "NE3-LATE-1",
        "NE3-PORTAL-1",
    ] {
        let view = einvoice
            .issue_document(manager, COMPANY, &draft(code, None))
            .await?;
        documents.insert(code, view.document.id);
        einvoice
            .submit_document(manager, COMPANY, view.document.id)
            .await?;
    }
    jobs_settle(&raw).await?;
    println!("jobs:\n  {}", job_states(&raw, mark_outcomes).await?);
    println!("stand-in saw:\n  {}", standin.drain().join("\n  "));
    for (code, id) in &documents {
        shown(code, einvoice.get_document(manager, COMPANY, *id).await);
    }
    println!(
        "logins so far: {} (the token is cached per company, environment and client ID)",
        standin.logins.load(Ordering::SeqCst)
    );
    let refused = documents["NE3-REFUSE-1"];
    show(
        "a refused document may be submitted again",
        einvoice.submit_document(manager, COMPANY, refused).await,
    );
    jobs_settle(&raw).await?;
    shown(
        "NE3-REFUSE-1 after its second refusal",
        einvoice.get_document(manager, COMPANY, refused).await,
    );

    println!("\n-- An outage: the 72-hour overdue notice, then recovery");
    let outage = einvoice
        .issue_document(manager, COMPANY, &draft("NE3-OUTAGE-1", None))
        .await?
        .document
        .id;
    standin.outage.store(true, Ordering::SeqCst);
    let now = Timestamp::now();
    show(
        "attempt at once (the job's body, run directly)",
        Ok::<_, EinvoiceError>(einvoice.run_submission(manager, COMPANY, outage, now).await),
    );
    show(
        "attempt again 71 hours on",
        Ok::<_, EinvoiceError>(
            einvoice
                .run_submission(
                    manager,
                    COMPANY,
                    outage,
                    now + SignedDuration::from_hours(71),
                )
                .await,
        ),
    );
    show(
        "attempt again 73 hours on",
        Ok::<_, EinvoiceError>(
            einvoice
                .run_submission(
                    manager,
                    COMPANY,
                    outage,
                    now + SignedDuration::from_hours(73),
                )
                .await,
        ),
    );
    show(
        "attempt again 80 hours on (already noticed)",
        Ok::<_, EinvoiceError>(
            einvoice
                .run_submission(
                    manager,
                    COMPANY,
                    outage,
                    now + SignedDuration::from_hours(80),
                )
                .await,
        ),
    );
    shown(
        "during the outage",
        einvoice.get_document(manager, COMPANY, outage).await,
    );
    standin.outage.store(false, Ordering::SeqCst);
    show(
        "the authority is back: the attempt searches before sending",
        Ok::<_, EinvoiceError>(
            einvoice
                .run_submission(manager, COMPANY, outage, Timestamp::now())
                .await,
        ),
    );
    jobs_settle(&raw).await?;
    println!("stand-in saw:\n  {}", standin.drain().join("\n  "));
    shown(
        "after the outage",
        einvoice.get_document(manager, COMPANY, outage).await,
    );

    println!("\n-- An expired token is dropped and the next call logs in again");
    let before = standin.logins.load(Ordering::SeqCst);
    standin.revoke_token.store(true, Ordering::SeqCst);
    show(
        "refresh NE3-INV-0001 with a token the authority no longer accepts",
        einvoice
            .refresh_status(manager, COMPANY, first.document.id)
            .await
            .map(|view| view.status),
    );
    shown(
        "refresh it again",
        einvoice
            .refresh_status(manager, COMPANY, first.document.id)
            .await,
    );
    println!(
        "logins: {} before, {} after",
        before,
        standin.logins.load(Ordering::SeqCst)
    );
    println!("stand-in saw:\n  {}", standin.drain().join("\n  "));

    println!("\n-- Refresh: rejection requests and portal cancellations");
    show(
        "viewer refreshes NE3-INV-0001",
        einvoice
            .refresh_status(viewer, COMPANY, first.document.id)
            .await
            .map(|view| view.status),
    );
    let issued = einvoice
        .issue_document(manager, COMPANY, &draft("NE3-ISSUED-1", None))
        .await?
        .document
        .id;
    show(
        "refresh a document never submitted",
        einvoice
            .refresh_status(manager, COMPANY, issued)
            .await
            .map(|view| view.status),
    );
    standin.set("NE3-INV-0001", |held| held.rejection_requested = true);
    shown(
        "the buyer asked for NE3-INV-0001's rejection; refresh",
        einvoice
            .refresh_status(manager, COMPANY, first.document.id)
            .await,
    );
    standin.set("NE3-PORTAL-1", |held| held.status = "Cancelled");
    shown(
        "NE3-PORTAL-1 was cancelled on the portal; refresh",
        einvoice
            .refresh_status(manager, COMPANY, documents["NE3-PORTAL-1"])
            .await,
    );

    println!("\n-- Cancel");
    let rate = documents["NE3-RATE-1"];
    let note = shown(
        "manager issues credit note NE3-CN-1 against NE3-RATE-1",
        einvoice
            .issue_document(manager, COMPANY, &draft("NE3-CN-1", Some(rate)))
            .await,
    )
    .ok_or(Error::Internal)?;
    einvoice
        .submit_document(manager, COMPANY, note.document.id)
        .await?;
    jobs_settle(&raw).await?;
    shown(
        "the credit note after its jobs",
        einvoice
            .get_document(manager, COMPANY, note.document.id)
            .await,
    );
    show(
        "cancel NE3-RATE-1, which the valid note references",
        einvoice
            .cancel_document(manager, COMPANY, rate, "Issued in error")
            .await
            .map(|view| view.status),
    );
    show(
        "cancel NE3-INV-0001 without a reason",
        einvoice
            .cancel_document(manager, COMPANY, first.document.id, "  ")
            .await
            .map(|view| view.status),
    );
    show(
        "cancel the never-submitted NE3-ISSUED-1",
        einvoice
            .cancel_document(manager, COMPANY, issued, "Issued in error")
            .await
            .map(|view| view.status),
    );
    show(
        "viewer cancels NE3-INV-0001",
        einvoice
            .cancel_document(viewer, COMPANY, first.document.id, "Issued in error")
            .await
            .map(|view| view.status),
    );
    shown(
        "manager cancels NE3-INV-0001 (rejection requested)",
        einvoice
            .cancel_document(manager, COMPANY, first.document.id, "Wrong buyer address")
            .await,
    );
    show(
        "cancel NE3-LATE-1, which the authority says is past its window",
        einvoice
            .cancel_document(manager, COMPANY, documents["NE3-LATE-1"], "Issued in error")
            .await
            .map(|view| view.status),
    );
    shown(
        "NE3-LATE-1 afterwards",
        einvoice
            .get_document(manager, COMPANY, documents["NE3-LATE-1"])
            .await,
    );
    println!("stand-in saw:\n  {}", standin.drain().join("\n  "));
    crate::einvoice_bridge::run(db, services, manager, viewer).await?;
    let _ = stop.send(true);
    let _ = worker.await;

    println!("\n-- What the database holds");
    let rows = raw
        .query(
            "SELECT d.code_number, s.id, s.document_version, octet_length(s.payload), \
             s.payload_sha256 = encode(sha256(s.payload), 'hex') \
             FROM finance_einvoice_submissions s JOIN finance_einvoice_documents d ON d.id = s.document_id \
             WHERE s.company_id = $1 ORDER BY s.id",
            &[&COMPANY],
        )
        .await?;
    for row in rows {
        println!(
            "submission {} of {}: version {}, {} bytes, hash matches the bytes: {}",
            row.get::<_, i64>(1),
            row.get::<_, String>(0),
            row.get::<_, String>(2),
            row.get::<_, i32>(3),
            row.get::<_, bool>(4)
        );
    }
    for (label, sql) in [
        (
            "UPDATE a submission's payload",
            "UPDATE finance_einvoice_submissions SET payload = 'x' WHERE company_id = 2",
        ),
        (
            "DELETE a status event",
            "DELETE FROM finance_einvoice_status_events WHERE company_id = 2",
        ),
        (
            "INSERT a submission whose hash is not its bytes' hash",
            "INSERT INTO finance_einvoice_submissions (tenant_id, company_id, document_id, environment, format, document_version, payload, payload_sha256, actor_type, actor_user_id) SELECT tenant_id, company_id, id, environment, 'JSON', '1.1', 'abc', repeat('0', 64), 'user', 1 FROM finance_einvoice_documents WHERE company_id = 2 LIMIT 1",
        ),
    ] {
        match raw.batch_execute(sql).await {
            Ok(()) => println!("{label}: accepted"),
            Err(error) => println!(
                "{label}: refused by database — {}",
                error.as_db_error().map_or("unavailable", |e| e.message())
            ),
        }
    }
    let rows = raw
        .query(
            "SELECT operation, method, path, response_status, outcome, COALESCE(error_class, '-'), \
             request_body IS NULL AND response_body IS NULL AND request_headers IS NULL, count(*) \
             FROM base_integration_exchanges WHERE system = 'myinvois' \
             GROUP BY 1, 2, 3, 4, 5, 6, 7 ORDER BY 1, 4",
            &[],
        )
        .await?;
    println!("exchanges recorded by Base Integration (system myinvois):");
    for row in rows {
        println!(
            "  {} {} {} -> {:?} {} {} bodies and headers omitted={} x{}",
            row.get::<_, String>(0),
            row.get::<_, Option<String>>(1).unwrap_or_default(),
            row.get::<_, String>(2),
            row.get::<_, Option<i16>>(3),
            row.get::<_, String>(4),
            row.get::<_, String>(5),
            row.get::<_, bool>(6),
            row.get::<_, i64>(7)
        );
    }
    let leaked: i64 = raw
        .query_one(
            "SELECT count(*) FROM base_integration_exchanges WHERE endpoint LIKE '%secret%' \
             OR request_body::text LIKE '%walkthrough-secret%'",
            &[],
        )
        .await?
        .get(0);
    println!("exchange rows carrying the client secret: {leaked}");
    let rows = raw
        .query(
            "SELECT event, payload::text FROM base_audit_actions \
             WHERE event LIKE 'finance.einvoice.%' \
               AND payload->>'result' IS DISTINCT FROM 'committed' ORDER BY id",
            &[],
        )
        .await?;
    println!("audit actions:");
    for row in rows {
        println!("  {} {}", row.get::<_, String>(0), row.get::<_, String>(1));
    }
    Ok(())
}
