//! Separation of duties for every Finance approval (Bilimbi `Ledger.Duties`,
//! `Ledger.DutySeparationChange`, `Ledger.ApprovalException` and
//! `Ledger.ApprovalExceptionReview`): the company's append-only setting, the
//! same-person rule, and the approval exceptions log an independent reviewer
//! reads. The setting is the latest change's; with none, separation is
//! required. Overlap counts impersonation: two people overlap when either
//! one's user or impersonator is the other's user or impersonator. The
//! database numbers and stamps every row and refuses a same-person exception
//! while separation is required (see `004-periods.sql`); the facade decides
//! each refusal first, in these words. The facade authorizes and holds the
//! company lock; these run inside it.
use crate::{
    same_person, Approval, ApprovalException, ApprovalExceptionQuery, ApprovalExceptionReview,
    DutySeparationChange, FieldError, LedgerError,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Change, Result};
use serde_json::{json, Value};
use tokio_postgres::{GenericClient, Row};

const SOURCE: &str = "finance/ledger";
const SUBJECT_LIMIT: usize = 200;
const TEXT_LIMIT: usize = 500;
const SOURCE_LIMIT: usize = 64;
const CODE_LIMIT: usize = 40;
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;

const STAMP: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'";

// The separation setting

fn change_columns() -> String {
    format!(
        "id,change_number,required,reason,changed_by_user_id,changed_by_impersonator_id,\
         to_char(changed_at,{STAMP})"
    )
}

/// The company's latest change, or `None` before any.
pub(crate) async fn latest_change(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
) -> Result<Option<DutySeparationChange>> {
    let row = client
        .query_opt(
            &format!(
                "SELECT {} FROM finance_ledger_duty_separation_changes WHERE tenant_id=$1 AND company_id=$2 \
                 ORDER BY change_number DESC LIMIT 1",
                change_columns()
            ),
            &[&tenant, &company_id],
        )
        .await?;
    row.as_ref().map(change_from_row).transpose()
}

/// Every change of the company, oldest first.
pub(crate) async fn changes(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
) -> Result<Vec<DutySeparationChange>> {
    client
        .query(
            &format!(
                "SELECT {} FROM finance_ledger_duty_separation_changes WHERE tenant_id=$1 AND company_id=$2 \
                 ORDER BY change_number",
                change_columns()
            ),
            &[&tenant, &company_id],
        )
        .await?
        .iter()
        .map(change_from_row)
        .collect()
}

/// Whether the latest change (or none) requires separation.
pub(crate) fn required(latest: Option<&DutySeparationChange>) -> bool {
    latest.is_none_or(|change| change.required)
}

/// `DutySeparationChange.changeset/2`'s `reason`: trimmed, blank as none, at
/// most 500 characters, and required to turn separation off.
pub(crate) fn validate_change(
    required: bool,
    reason: Option<&str>,
) -> std::result::Result<Option<String>, Vec<FieldError>> {
    let reason = bilibi_ui::params::blank_to_none(reason);
    match &reason {
        Some(text) if text.chars().count() > TEXT_LIMIT => Err(vec![too_long("reason")]),
        None if !required => Err(vec![FieldError::new(
            "reason",
            "is required to turn separation off",
        )]),
        _ => Ok(reason),
    }
}

/// Appends a change of the setting. The database numbers and stamps it and
/// refuses one that changes nothing, so the caller decides that first.
pub(crate) async fn record_change(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    required: bool,
    reason: Option<&str>,
) -> Result<Change> {
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_duty_separation_changes(tenant_id,company_id,change_number,required,reason,\
             changed_by_user_id,changed_by_impersonator_id,changed_at) \
             VALUES($1,$2,NULL,$3,$4,$5,$6,NULL) RETURNING id,change_number",
            &[&tenant, &company_id, &required, &reason, &actor, &impersonator],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.duty_separation_change".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "company_id": company_id,
            "change_number": row.get::<_, i64>(1),
            "required": required,
            "reason": reason,
            "changed_by_user_id": actor,
            "changed_by_impersonator_id": impersonator,
        }),
    })
}

fn change_from_row(row: &Row) -> Result<DutySeparationChange> {
    Ok(DutySeparationChange {
        id: row.get(0),
        change_number: row.get(1),
        required: row.get(2),
        reason: row.get(3),
        changed_by_user_id: row.get(4),
        changed_by_impersonator_id: row.get(5),
        changed_at: parse(row.get(6))?,
    })
}

// Approvals

/// An approval exception to append: what [`decide`] found the approval to be.
#[derive(serde::Serialize)]
pub(crate) struct NewException {
    source: String,
    action: String,
    subject_type: String,
    subject_id: i64,
    subject: String,
    same_person: bool,
    overrides: bool,
    prepared_by: (Option<i64>, Option<i64>),
    reason: String,
    separation_change_id: Option<i64>,
}

/// `Duties.check/3` up to its insert: whether the approver (`actor`, any
/// `impersonator`) may approve what `approval` describes, and the exception
/// it must record. A preparer who overlaps the approver is
/// `LedgerError::SamePerson` while the company requires separation and a
/// same-person exception, with the reason separation was turned off, while
/// it does not; an override is always recorded with its own reason. Anything
/// else records nothing (`Ok(None)`).
pub(crate) async fn decide(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
    approver: (i64, Option<i64>),
    approval: &Approval,
) -> std::result::Result<Option<NewException>, LedgerError> {
    let overlapping = approval
        .prepared_by
        .iter()
        .find(|preparer| same_person((preparer.user_id, preparer.impersonator_id), approver));
    let overrides = approval.override_reason.is_some();
    let latest = latest_change(client, tenant, company_id).await?;
    if overlapping.is_some() && required(latest.as_ref()) {
        return Err(LedgerError::SamePerson);
    }
    if overlapping.is_none() && !overrides {
        return Ok(None);
    }
    let preparer = overlapping
        .or(approval.prepared_by.first())
        .map_or((None, None), |p| (Some(p.user_id), p.impersonator_id));
    let reason = if overrides {
        approval.override_reason.as_deref()
    } else {
        latest.as_ref().and_then(|change| change.reason.as_deref())
    };
    let exception = NewException {
        source: approval.source.trim().to_owned(),
        action: approval.action.trim().to_owned(),
        subject_type: approval.subject_type.trim().to_owned(),
        subject_id: approval.subject_id,
        subject: approval
            .subject
            .trim()
            .chars()
            .take(SUBJECT_LIMIT)
            .collect(),
        same_person: overlapping.is_some(),
        overrides,
        prepared_by: preparer,
        reason: reason.map(str::trim).unwrap_or_default().to_owned(),
        separation_change_id: overlapping.and(latest.map(|change| change.id)),
    };
    validate_exception(&exception).map_err(LedgerError::Invalid)?;
    Ok(Some(exception))
}

/// `ApprovalException.changeset/2`: the codes' shapes and the lengths.
fn validate_exception(exception: &NewException) -> std::result::Result<(), Vec<FieldError>> {
    let code = |value: &str| {
        value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    let source = |value: &str| {
        value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'/' | b'-')
            })
    };
    let mut errors = Vec::new();
    let mut check = |field: &'static str, value: &str, shape: &dyn Fn(&str) -> bool, max| {
        if value.is_empty() {
            errors.push(FieldError::new(field, "can't be blank"));
        } else if !shape(value) {
            errors.push(FieldError::new(field, "has invalid format"));
        } else if value.chars().count() > max {
            errors.push(FieldError::new(
                field,
                format!("should be at most {max} character(s)"),
            ));
        }
    };
    check("source", &exception.source, &source, SOURCE_LIMIT);
    check("action", &exception.action, &code, CODE_LIMIT);
    check("subject_type", &exception.subject_type, &code, CODE_LIMIT);
    if exception.subject_id < 1 {
        errors.push(FieldError::new("subject_id", "must be greater than 0"));
    }
    if exception.subject.is_empty() {
        errors.push(FieldError::new("subject", "can't be blank"));
    }
    if exception.reason.is_empty() {
        errors.push(FieldError::new("reason", "can't be blank"));
    } else if exception.reason.chars().count() > TEXT_LIMIT {
        errors.push(too_long("reason"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Appends the exception [`decide`] found; the database numbers and stamps it.
pub(crate) async fn record_exception(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    approver: (i64, Option<i64>),
    exception: &NewException,
) -> Result<Change> {
    let (prepared_user, prepared_impersonator) = exception.prepared_by;
    let row = client
        .query_one(
            "INSERT INTO finance_ledger_approval_exceptions(tenant_id,company_id,exception_number,source,action,\
             subject_type,subject_id,subject,same_person,override,prepared_by_user_id,prepared_by_impersonator_id,\
             approved_by_user_id,approved_by_impersonator_id,reason,separation_change_id,recorded_at) \
             VALUES($1,$2,NULL,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,NULL) \
             RETURNING id,exception_number",
            &[
                &tenant,
                &company_id,
                &exception.source,
                &exception.action,
                &exception.subject_type,
                &exception.subject_id,
                &exception.subject,
                &exception.same_person,
                &exception.overrides,
                &prepared_user,
                &prepared_impersonator,
                &approver.0,
                &approver.1,
                &exception.reason,
                &exception.separation_change_id,
            ],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.approval_exception".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "exception_number": row.get::<_, i64>(1),
            "source": exception.source,
            "action": exception.action,
            "subject_type": exception.subject_type,
            "subject_id": exception.subject_id,
            "subject": exception.subject,
            "same_person": exception.same_person,
            "override": exception.overrides,
            "prepared_by_user_id": prepared_user,
            "prepared_by_impersonator_id": prepared_impersonator,
            "approved_by_user_id": approver.0,
            "approved_by_impersonator_id": approver.1,
            "reason": exception.reason,
            "separation_change_id": exception.separation_change_id,
        }),
    })
}

/// An override of one of the ledger's own controls, whoever made it.
pub(crate) struct Override<'a> {
    pub action: &'static str,
    pub subject_type: &'static str,
    pub subject_id: i64,
    pub subject: String,
    /// The control's own reason, already required and trimmed by its caller.
    pub reason: &'a str,
}

/// Appends the override of a ledger control (a reopened period or fiscal
/// year, controlled bookkeeping turned off): an exception with no preparer.
pub(crate) async fn record_override(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    exception: &Override<'_>,
) -> Result<Change> {
    record_exception(
        client,
        tenant,
        company_id,
        (actor, impersonator),
        &NewException {
            source: SOURCE.into(),
            action: exception.action.into(),
            subject_type: exception.subject_type.into(),
            subject_id: exception.subject_id,
            subject: exception
                .subject
                .trim()
                .chars()
                .take(SUBJECT_LIMIT)
                .collect(),
            same_person: false,
            overrides: true,
            prepared_by: (None, None),
            reason: exception.reason.to_owned(),
            separation_change_id: None,
        },
    )
    .await
}

// The exceptions log

fn exception_select() -> String {
    format!(
        "SELECT x.id,x.exception_number,x.source,x.action,x.subject_type,x.subject_id,x.subject,x.same_person,\
         x.override,x.prepared_by_user_id,x.prepared_by_impersonator_id,x.approved_by_user_id,\
         x.approved_by_impersonator_id,x.reason,x.separation_change_id,to_char(x.recorded_at,{STAMP}),\
         r.id,r.note,r.reviewed_by_user_id,r.reviewed_by_impersonator_id,to_char(r.reviewed_at,{STAMP}) \
         FROM finance_ledger_approval_exceptions x \
         LEFT JOIN finance_ledger_approval_exception_reviews r ON r.exception_id = x.id AND r.company_id = x.company_id"
    )
}

/// The company's exceptions, oldest number first, as the query bounds them.
pub(crate) async fn exceptions(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    query: &ApprovalExceptionQuery,
) -> std::result::Result<Vec<ApprovalException>, LedgerError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) || query.after.is_some_and(|after| after < 0) {
        return Err(LedgerError::InvalidOption);
    }
    let (from, before) = (
        query.recorded_from.map(|at| at.to_string()),
        query.recorded_before.map(|at| at.to_string()),
    );
    // `recorded_at` holds UTC: an instant compares with it in UTC.
    let rows = client
        .query(
            &format!(
                "{} WHERE x.tenant_id=$1 AND x.company_id=$2 \
                 AND ($3::text IS NULL OR x.recorded_at >= ($3::text::timestamptz AT TIME ZONE 'UTC')) \
                 AND ($4::text IS NULL OR x.recorded_at < ($4::text::timestamptz AT TIME ZONE 'UTC')) \
                 AND ($5::boolean IS NULL OR (r.id IS NOT NULL) = $5) \
                 AND ($6::bigint IS NULL OR x.exception_number > $6) \
                 ORDER BY x.exception_number LIMIT $7",
                exception_select()
            ),
            &[
                &tenant,
                &company_id,
                &from,
                &before,
                &query.reviewed,
                &query.after,
                &i64::from(limit),
            ],
        )
        .await
        .map_err(bilibi_database::Error::from)?;
    Ok(rows.iter().map(exception_from_row).collect::<Result<_>>()?)
}

/// One exception of the company with its review.
pub(crate) async fn exception(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    exception_id: i64,
) -> Result<Option<ApprovalException>> {
    client
        .query_opt(
            &format!(
                "{} WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.id=$3",
                exception_select()
            ),
            &[&tenant, &company_id, &exception_id],
        )
        .await?
        .as_ref()
        .map(exception_from_row)
        .transpose()
}

/// `ApprovalExceptionReview.changeset/2`'s `note`: trimmed, blank as none, at
/// most 500 characters.
pub(crate) fn validate_note(
    note: Option<&str>,
) -> std::result::Result<Option<String>, Vec<FieldError>> {
    let note = bilibi_ui::params::blank_to_none(note);
    if note
        .as_ref()
        .is_some_and(|text| text.chars().count() > TEXT_LIMIT)
    {
        Err(vec![too_long("note")])
    } else {
        Ok(note)
    }
}

/// Appends the review; the database stamps it and refuses a second one and a
/// reviewer who overlaps the approver or the preparer.
pub(crate) async fn record_review(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    exception_id: i64,
    reviewer: (i64, Option<i64>),
    note: Option<&str>,
) -> Result<Change> {
    let row = client
        .query_one(
            &format!(
                "INSERT INTO finance_ledger_approval_exception_reviews(tenant_id,company_id,exception_id,note,\
                 reviewed_by_user_id,reviewed_by_impersonator_id,reviewed_at) \
                 VALUES($1,$2,$3,$4,$5,$6,NULL) RETURNING id,to_char(reviewed_at,{STAMP})"
            ),
            &[&tenant, &company_id, &exception_id, &note, &reviewer.0, &reviewer.1],
        )
        .await?;
    Ok(Change {
        kind: "finance.ledger.approval_exception_review".into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "exception_id": exception_id, "note": note, "reviewed_by_user_id": reviewer.0,
            "reviewed_by_impersonator_id": reviewer.1, "reviewed_at": row.get::<_, String>(1),
        }),
    })
}

fn exception_from_row(row: &Row) -> Result<ApprovalException> {
    let review = match row.get::<_, Option<i64>>(16) {
        Some(_) => Some(ApprovalExceptionReview {
            note: row.get(17),
            reviewed_by_user_id: row.get(18),
            reviewed_by_impersonator_id: row.get(19),
            reviewed_at: parse(row.get(20))?,
        }),
        None => None,
    };
    Ok(ApprovalException {
        id: row.get(0),
        exception_number: row.get(1),
        source: row.get(2),
        action: row.get(3),
        subject_type: row.get(4),
        subject_id: row.get(5),
        subject: row.get(6),
        same_person: row.get(7),
        overrides: row.get(8),
        prepared_by_user_id: row.get(9),
        prepared_by_impersonator_id: row.get(10),
        approved_by_user_id: row.get(11),
        approved_by_impersonator_id: row.get(12),
        reason: row.get(13),
        separation_change_id: row.get(14),
        recorded_at: parse(row.get(15))?,
        review,
    })
}

fn too_long(field: &'static str) -> FieldError {
    FieldError::new(
        field,
        format!("should be at most {TEXT_LIMIT} character(s)"),
    )
}

fn parse<T: std::str::FromStr>(text: String) -> Result<T> {
    text.parse().map_err(|_| bilibi_database::Error::Internal)
}
