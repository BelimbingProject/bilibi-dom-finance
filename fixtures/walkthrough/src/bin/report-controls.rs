//! NT3 running proof: the actual page service calls the actual ledger facade
//! against a fresh disposable database. No mocks, unit tests or product seeds.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_finance_ledger::{
    ApprovalExceptionQuery, Ledger, NewBook, MANAGE_CONTROLLED_BOOKKEEPING, REVIEW_EXCEPTIONS,
};
use bilibi_finance_walkthrough_fixture::sign_in;
use bilibi_module_registry::Services;
use bilibi_tenancy::Scope;
use bilibi_ui::page::{LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PageService};
use jiff::{Timestamp, ToSpan};
use serde_json::{json, Value};
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
bilibi_finance_walkthrough_fixture::walkthrough_ledger_roles!();
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

async fn mount(
    services: &Services,
    scope: &Scope,
    operation: &'static str,
) -> Outcome<Box<dyn LiveState<Scope, Error>>> {
    let pages = services.get::<PageService<Scope, Error>>("finance/reports.control-pages")?;
    let map = HashMap::new();
    let query = if operation == "approval-exceptions" {
        HashMap::from([("area".into(), "other".into())])
    } else {
        HashMap::new()
    };
    Ok(pages
        .0
        .mount(PageRequest {
            operation,
            scope: Some(scope),
            path: &map,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
        .unwrap())
}
async fn update(
    state: &mut dyn LiveState<Scope, Error>,
    scope: &Scope,
    input: LiveInput<'_>,
) -> Outcome<LiveBody> {
    Ok(state
        .update(LiveRequest {
            scope,
            credential: "",
            input,
        })
        .await?)
}
async fn event(
    state: &mut dyn LiveState<Scope, Error>,
    scope: &Scope,
    value: Value,
) -> Outcome<String> {
    Ok(update(state, scope, LiveInput::Event(&value)).await?.html)
}
async fn refresh(state: &mut dyn LiveState<Scope, Error>, scope: &Scope) -> Outcome<String> {
    Ok(update(state, scope, LiveInput::Refresh).await?.html)
}
fn contains(html: &str, text: &str) {
    assert!(html.contains(text), "page missing {text:?}");
}

#[tokio::main]
async fn main() -> Outcome<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let services = bilibi_finance_walkthrough_fixture::services(&db, &modules)?;
    bilibi_finance_walkthrough_fixture::install_platform(&db).await?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    if std::env::args().any(|arg| arg == "--empty") {
        println!("NT3: empty reference state installed for browser capture");
        return Ok(());
    }
    let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let late_date = today.checked_sub(120.days())?;
    let manager = sign_in(&db, &services, 1, 1).await?;
    let authority = sign_in(&db, &services, 1, 4).await?;
    // A page needs the session's current company, not a user's organisation field.
    // The grant-all fixture administrator is intentionally unaffiliated.
    let mut missing = mount(&services, &authority, "controls").await?;
    contains(
        &refresh(missing.as_mut(), &authority).await?,
        "No current company",
    );
    for capability in [
        bilibi_finance_ledger::REVIEW_LATE_RECORDINGS,
        REVIEW_EXCEPTIONS,
    ] {
        grants
            .set_direct(&authority, 7, capability, Some(true))
            .await?;
    }
    let affiliations =
        services.get::<bilibi_affiliation::AffiliationService>(bilibi_affiliation::SERVICE)?;
    let users = services.get::<bilibi_user::UserService>("core/user.accounts")?;
    let affiliation = affiliations
        .create(
            &authority,
            &bilibi_affiliation::AffiliationInput {
                person_id: bilibi_affiliation::PersonId::new(2)?,
                organisation_id: bilibi_affiliation::OrganisationId::new(1)?,
                title: Some("Independent ledger reviewer".into()),
                work_email: None,
                work_phone: None,
                dates: bilibi_affiliation::EffectiveDates::new(None, None)?,
            },
        )
        .await?;
    users
        .link_person(&authority, 7, affiliation.affiliation.person_id)
        .await?;
    users
        .set_desk(
            &authority,
            affiliation.affiliation.organisation_id.get(),
            affiliation.affiliation.person_id.get(),
            true,
        )
        .await?;
    // The reviewer works in Company 001 now; its first desk is Company 002.
    db.get()
        .await?
        .execute(
            "UPDATE users SET last_desk_id=1 WHERE tenant_id=1 AND id=7",
            &[],
        )
        .await?;
    let reviewer_scope = sign_in(&db, &services, 1, 7).await?;
    let viewer = sign_in(&db, &services, 1, 7).await?;
    let ledger = services.get::<Ledger>(bilibi_finance_ledger::SERVICE)?;
    ledger
        .open_book(
            &manager,
            1,
            &NewBook {
                currency_code: "MYR".into(),
                minor_units: 2,
            },
        )
        .await?;
    let mut book = mount(&services, &manager, "controls").await?;
    let page = refresh(book.as_mut(), &manager).await?;
    contains(
        &page,
        "No fiscal year is open, so no entry can be posted yet.",
    );
    contains(&page, "/finance/ledger/fiscal-years");
    ledger
        .open_fiscal_year(
            &manager,
            1,
            &bilibi_finance_ledger::NewFiscalYear {
                start_date: Some(late_date.first_of_month()),
                end_date: None,
            },
        )
        .await?;
    println!("NT3: controls link to the single Fiscal years home");

    event(
        book.as_mut(),
        &manager,
        json!({"type":"open_dialog","dialog":"controlled-off"}),
    )
    .await?;
    let invalid = event(
        book.as_mut(),
        &manager,
        json!({"type":"turn_off","dialog[text]":""}),
    )
    .await?;
    contains(&invalid, "Give a reason of 1 to 500 characters.");
    contains(&invalid, "controlled-off-modal");
    let reason = "界".repeat(400);
    let changed = event(
        book.as_mut(),
        &manager,
        json!({"type":"turn_off","dialog[text]":reason}),
    )
    .await?;
    contains(
        &changed,
        "Bookkeeping is not controlled; the change is listed as an override.",
    );
    assert!(
        !ledger
            .get_controlled_bookkeeping(&manager, 1)
            .await?
            .controlled
    );
    assert_eq!(
        ledger
            .list_controlled_bookkeeping_changes(&manager, 1)
            .await?
            .last()
            .unwrap()
            .reason
            .as_deref(),
        Some(reason.as_str())
    );
    let exceptions = ledger
        .list_approval_exceptions(&manager, 1, &ApprovalExceptionQuery::default())
        .await?;
    assert_eq!(exceptions.len(), 1);
    assert!(exceptions[0].overrides);
    event(book.as_mut(), &manager, json!({"type":"turn_on"})).await?;
    assert!(
        ledger
            .get_controlled_bookkeeping(&manager, 1)
            .await?
            .controlled
    );
    println!("NT3: controlled off requires reason, Unicode is preserved, override audited, controlled on restored");

    contains(
        &refresh(book.as_mut(), &manager).await?,
        "/finance/ledger/journals?review=pending",
    );

    let mut approvals = mount(&services, &manager, "approval-exceptions").await?;
    let initial = refresh(approvals.as_mut(), &manager).await?;
    contains(&initial, "exceptions-period-period");
    contains(&initial, "Override");
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"open_dialog","dialog":"separation"}),
    )
    .await?;
    let invalid = event(
        approvals.as_mut(),
        &manager,
        json!({"type":"change_separation","dialog[text]":""}),
    )
    .await?;
    contains(&invalid, "Give a reason of 1 to 500 characters.");
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"change_separation","dialog[text]":"Only one finance preparer this month"}),
    )
    .await?;
    assert!(!ledger.get_duty_separation(&manager, 1).await?.required);
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"open_dialog","dialog":"separation"}),
    )
    .await?;
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"change_separation","dialog[text]":""}),
    )
    .await?;
    assert!(ledger.get_duty_separation(&manager, 1).await?.required);
    let exception_id = exceptions[0].id.to_string();
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"open_dialog","exception":exception_id}),
    )
    .await?;
    let refused = event(
        approvals.as_mut(),
        &manager,
        json!({"type":"review","dialog[text]":""}),
    )
    .await?;
    contains(
        &refused,
        "An exception is reviewed by someone who neither approved nor prepared what it records.",
    );
    let mut reviewer = mount(&services, &reviewer_scope, "approval-exceptions").await?;
    event(
        reviewer.as_mut(),
        &reviewer_scope,
        json!({"type":"open_dialog","exception":exception_id}),
    )
    .await?;
    let reviewed = event(
        reviewer.as_mut(),
        &reviewer_scope,
        json!({"type":"review","dialog[text]":""}),
    )
    .await?;
    contains(&reviewed, "Review of exception 1 recorded.");
    assert!(ledger
        .get_approval_exception(&manager, 1, exceptions[0].id)
        .await?
        .review
        .is_some());
    let period = HashMap::from([
        ("area".into(), "other".into()),
        ("period".into(), "custom".into()),
        ("from".into(), today.to_string()),
        ("to".into(), today.to_string()),
        ("review".into(), "reviewed".into()),
    ]);
    let filtered = update(approvals.as_mut(), &manager, LiveInput::Params(&period)).await?;
    contains(&filtered.html, "exception-review-1");
    let unreadable = HashMap::from([
        ("area".into(), "other".into()),
        ("period".into(), "custom".into()),
        ("from".into(), "2026-10-20".into()),
        ("to".into(), "2026-10-01".into()),
    ]);
    let refused_period =
        update(approvals.as_mut(), &manager, LiveInput::Params(&unreadable)).await?;
    contains(&refused_period.html, "The exceptions cannot be shown");
    contains(
        &refused_period.html,
        "Choose another period or reload the page.",
    );
    update(approvals.as_mut(), &manager, LiveInput::Params(&period)).await?;
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"period_select:exceptions-period","period_filter[period]":"custom"}),
    )
    .await?;
    let backwards = event(
        approvals.as_mut(),
        &manager,
        json!({"type":"period_apply:exceptions-period","range[from]":"2026-10-20","range[to]":"2026-10-01"}),
    )
    .await?;
    contains(&backwards, "Start date must be on or before end date.");
    event(
        approvals.as_mut(),
        &manager,
        json!({"type":"period_cancel:exceptions-period"}),
    )
    .await?;
    println!("NT3: period/review filtering, separation reason, require again, approver refused, independent optional-note review committed");

    let mut restricted = mount(&services, &viewer, "controls").await?;
    let withheld = refresh(restricted.as_mut(), &viewer).await?;
    // As in Bilimbi, a control the viewer may not use is simply absent.
    assert!(
        !withheld.contains("id=\"controlled-bookkeeping-toggle\""),
        "restricted page shows the controlled bookkeeping toggle"
    );
    let forged = event(restricted.as_mut(), &viewer, json!({"type":"turn_on"})).await?;
    contains(&forged, "You do not have permission for that action.");
    event(
        book.as_mut(),
        &manager,
        json!({"type":"open_dialog","dialog":"controlled-off"}),
    )
    .await?;
    grants
        .set_direct(&authority, 1, MANAGE_CONTROLLED_BOOKKEEPING, Some(false))
        .await?;
    let revoked = event(
        book.as_mut(),
        &manager,
        json!({"type":"turn_off","dialog[text]":"Stale page"}),
    )
    .await?;
    contains(&revoked, "You do not have permission for that action.");
    assert!(
        ledger
            .get_controlled_bookkeeping(&manager, 1)
            .await?
            .controlled
    );
    grants
        .set_direct(&authority, 1, MANAGE_CONTROLLED_BOOKKEEPING, None)
        .await?;
    grants
        .set_direct(&authority, 1, REVIEW_EXCEPTIONS, Some(false))
        .await?;
    let forged = event(
        approvals.as_mut(),
        &manager,
        json!({"type":"review","dialog[text]":"forged"}),
    )
    .await?;
    contains(&forged, "You do not have permission for that action.");
    grants
        .set_direct(&authority, 1, REVIEW_EXCEPTIONS, None)
        .await?;
    println!("NT3: withheld controls, forged writes and mid-page grant revocation refuse without losing the live page");

    let raw = db.get().await?;
    raw.batch_execute("UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=1")
        .await?;
    event(
        book.as_mut(),
        &manager,
        json!({"type":"open_dialog","dialog":"controlled-off"}),
    )
    .await?;
    let archived = event(
        book.as_mut(),
        &manager,
        json!({"type":"turn_off","dialog[text]":"Archived"}),
    )
    .await?;
    // The Platform's reach check leaves out an archived company, so the page
    // refuses the change as unauthorized before the lock reports it archived.
    assert!(
        archived.contains("The company is archived; nothing can be changed.")
            || archived.contains("You do not have permission for that action."),
        "archived write was not refused"
    );
    raw.batch_execute("UPDATE companies SET status='active' WHERE tenant_id=1 AND id=1")
        .await?;
    for statement in ["DELETE FROM finance_ledger_approval_exception_reviews"] {
        assert!(
            raw.batch_execute(statement).await.is_err(),
            "append-only refusal missing"
        );
    }
    println!("NT3 proof passed: controls link to record homes, archived write refuses, append-only approval reviews refuse direct edits");
    Ok(())
}
