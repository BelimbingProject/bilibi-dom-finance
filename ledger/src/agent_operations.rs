//! H1 reach for Bilimbi.Finance.Ledger's facade-only actions. The shared
//! agent API owns transport, plan identity, expiry, replay and outer commit.
use super::*;
use bilibi_agent_api::{
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn invalid(error: LedgerError) -> bilibi_agent_api::Error {
    use bilibi_agent_api::Error as E;
    match error {
        LedgerError::Unauthorized => E::Forbidden,
        LedgerError::CompanyNotFound
        | LedgerError::AccountNotFound
        | LedgerError::EntryNotFound
        | LedgerError::JournalNotFound
        | LedgerError::FiscalYearNotFound
        | LedgerError::PeriodNotFound => E::NotFound,
        LedgerError::Failed(error) => error.into(),
        LedgerError::Invalid(errors) => {
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for e in errors {
                let field = e.line.map_or_else(
                    || e.field.into(),
                    |line| format!("lines.{}.{}", line - 1, e.field),
                );
                fields.entry(field).or_default().push(e.message);
            }
            E::InvalidInput(fields)
        }
        other => E::InvalidInput(BTreeMap::from([("base".into(), vec![other.to_string()])])),
    }
}

pub(super) async fn authorize_on(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    capability: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(client, scope, company, capability)
        .await
        .map(|_| ())
        .map_err(refusal)
}

fn id(input: &Value, field: &str) -> Result<i64> {
    input[field]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or(LedgerError::InvalidOption)
}
fn text<'a>(input: &'a Value, field: &str) -> Result<&'a str> {
    input[field].as_str().ok_or(LedgerError::InvalidOption)
}
/// The book's currency code, trimmed and upcased, once its shape is valid;
/// the list is asked only after, so a typo reads as a format error.
fn shaped_code(input: &Value) -> Result<String> {
    let code = text(input, "currency_code")?.trim().to_ascii_uppercase();
    books::validate(&NewBook {
        currency_code: code.clone(),
        minor_units: 0,
    })
    .map_err(LedgerError::Invalid)?;
    Ok(code)
}
fn date(input: &Value, field: &str) -> Result<Date> {
    text(input, field)?.parse().map_err(|_| {
        LedgerError::Invalid(vec![FieldError::new("entry_date", "has invalid format")])
    })
}
fn optional_date(input: &Value, field: &str) -> Result<Option<Date>> {
    input.get(field).map(|_| date(input, field)).transpose()
}
fn sections(input: &Value) -> Result<Vec<String>> {
    match &input["sections"] {
        Value::Null => Ok(Vec::new()),
        Value::Array(keys) => keys
            .iter()
            .map(|key| {
                key.as_str()
                    .map(str::to_owned)
                    .ok_or(LedgerError::InvalidOption)
            })
            .collect(),
        _ => Err(LedgerError::InvalidOption),
    }
}
fn opt_text(input: &Value, field: &str) -> Option<String> {
    input[field].as_str().map(str::to_owned)
}
fn new_account(input: &Value) -> Result<NewAccount> {
    Ok(NewAccount {
        code: text(input, "code")?.into(),
        name: text(input, "name")?.into(),
        account_type: AccountType::parse(text(input, "account_type")?)
            .ok_or(LedgerError::InvalidOption)?,
        parent_id: input
            .get("parent_id")
            .map(|_| id(input, "parent_id"))
            .transpose()?,
        active: input["active"].as_bool().unwrap_or(true),
        retained_earnings: input["retained_earnings"].as_bool().unwrap_or(false),
    })
}
fn account_change(input: &Value) -> Result<AccountChange> {
    Ok(AccountChange {
        name: opt_text(input, "name"),
        parent_id: if input["clear_parent"].as_bool() == Some(true) {
            if input.get("parent_id").is_some() {
                return Err(LedgerError::Invalid(vec![FieldError::new(
                    "parent_id",
                    "cannot be set with clear_parent",
                )]));
            }
            Some(None)
        } else {
            input
                .get("parent_id")
                .map(|_| id(input, "parent_id").map(Some))
                .transpose()?
        },
        active: input["active"].as_bool(),
        retained_earnings: input["retained_earnings"].as_bool(),
    })
}
fn new_group(input: &Value) -> Result<NewAccountGroup> {
    Ok(NewAccountGroup {
        code: text(input, "code")?.into(),
        name: text(input, "name")?.into(),
        account_type: AccountType::parse(text(input, "account_type")?)
            .ok_or(LedgerError::InvalidOption)?,
        parent_id: input
            .get("parent_id")
            .map(|_| id(input, "parent_id"))
            .transpose()?,
    })
}
fn group_change(input: &Value) -> Result<AccountGroupChange> {
    let change = account_change(input)?;
    Ok(AccountGroupChange {
        name: change.name,
        parent_id: change.parent_id,
    })
}
fn new_year(input: &Value) -> Result<NewFiscalYear> {
    Ok(NewFiscalYear {
        start_date: optional_date(input, "start_date")?,
        end_date: optional_date(input, "end_date")?,
    })
}
fn year_close(input: &Value) -> YearClose {
    YearClose {
        description: opt_text(input, "description"),
        reference: opt_text(input, "reference"),
    }
}
fn reversal(input: &Value) -> Result<Reversal> {
    Ok(Reversal {
        entry_date: date(input, "entry_date")?,
        description: text(input, "description")?.into(),
        reference: opt_text(input, "reference"),
    })
}
fn new_entry(input: &Value, units: MinorUnits) -> Result<NewEntry> {
    let mut lines = Vec::new();
    let mut balancing = None;
    for (index, line) in input["lines"]
        .as_array()
        .ok_or(LedgerError::InvalidOption)?
        .iter()
        .enumerate()
    {
        let amount = |field: &'static str| -> Result<Money> {
            line.get(field)
                .map(|v| {
                    Money::parse(v.as_str().ok_or(LedgerError::InvalidOption)?, units).map_err(
                        |e| {
                            LedgerError::Invalid(vec![
                                FieldError::new(field, e.to_string()).on_line(index + 1)
                            ])
                        },
                    )
                })
                .unwrap_or(Ok(Money::ZERO))
        };
        let (debit, credit) = (amount("debit")?, amount("credit")?);
        if line.get("balance").and_then(Value::as_bool) == Some(true) {
            if line.get("debit").is_some() || line.get("credit").is_some() {
                return Err(balance_refusal(
                    index,
                    "cannot be combined with a debit or credit; the line takes the amount that balances the journal",
                ));
            }
            if balancing.replace(index).is_some() {
                return Err(balance_refusal(index, "can be set on only one line"));
            }
        }
        lines.push(NewLine {
            account_id: id(line, "account_id")?,
            debit,
            credit,
            description: opt_text(line, "description"),
        });
    }
    if let Some(index) = balancing {
        // The other lines' difference goes on the opposite side, so the
        // journal balances exactly at the book's minor units.
        let (debits, credits) = lines.iter().fold((Money::ZERO, Money::ZERO), |(d, c), l| {
            (d + l.debit, c + l.credit)
        });
        if debits == credits {
            return Err(balance_refusal(
                index,
                "has nothing to balance; the other lines already match",
            ));
        }
        let difference = Money::new((debits - credits).amount().abs(), units)
            .map_err(|e| balance_refusal(index, &e.to_string()))?;
        if debits.amount() > credits.amount() {
            lines[index].credit = difference;
        } else {
            lines[index].debit = difference;
        }
    }
    Ok(NewEntry {
        entry_date: date(input, "entry_date")?,
        description: text(input, "description")?.into(),
        reference: opt_text(input, "reference"),
        lines,
    })
}

fn balance_refusal(index: usize, message: &str) -> LedgerError {
    LedgerError::Invalid(vec![FieldError::new("balance", message).on_line(index + 1)])
}

#[derive(Clone, Copy)]
enum Write {
    OpenBook,
    CreateAccount,
    UpdateAccount,
    CreateGroup,
    UpdateGroup,
    ApplyTemplate,
    SaveTemplate,
    ProposeTemplate,
    Submit,
    Opening,
    Approve,
    Reject,
    Withdraw,
    Replace,
    Reverse,
    ReviewLate,
    OpenYear,
    ClosePeriod,
    ReopenPeriod,
    LockPeriod,
    CloseYear,
    ReopenYear,
    LockYear,
}
impl Write {
    fn capability(self) -> &'static str {
        match self {
            Self::SaveTemplate | Self::ProposeTemplate => MANAGE_TEMPLATES,
            Self::OpenBook
            | Self::CreateAccount
            | Self::UpdateAccount
            | Self::CreateGroup
            | Self::UpdateGroup
            | Self::ApplyTemplate => MANAGE_ACCOUNTS,
            Self::Submit | Self::Withdraw | Self::Replace | Self::Opening => SUBMIT_JOURNALS,
            Self::Approve | Self::Reject => APPROVE_JOURNALS,
            Self::Reverse => POST_ENTRIES,
            Self::ReviewLate => REVIEW_LATE_RECORDINGS,
            Self::OpenYear | Self::ClosePeriod | Self::CloseYear => MANAGE_PERIODS,
            Self::ReopenPeriod | Self::ReopenYear => REOPEN_PERIODS,
            Self::LockPeriod | Self::LockYear => LOCK_PERIODS,
        }
    }
    fn consequence(self) -> &'static str {
        match self {
            Self::OpenBook => {
                "Open the book once; currency and minor units are permanent. Controlled bookkeeping starts on."
            }
            Self::CreateAccount => "Create an account; its code and account type are permanent.",
            Self::UpdateAccount => {
                "Change the account; deactivation requires a zero balance. A new retained-earnings designation releases the previous one."
            }
            Self::Opening => {
                "Submit opening balances for approval; no entry posts yet. With replaces_journal_id, withdraw your waiting opening journal and submit this correction in its place. Open operational items and historical transactions are excluded."
            }
            Self::CreateGroup => {
                "Create an account group; it takes no postings and shows the total of what is under it. Its code and account type are permanent."
            }
            Self::UpdateGroup => {
                "Change the account group's name or the group it sits in; its code and account type are permanent."
            }
            Self::SaveTemplate => {
                "Publish an immutable snapshot of the current company chart for reuse within this tenant. No balances or settings are copied. Review every row before saving."
            }
            Self::ProposeTemplate => {
                "Set or clear the starter chart proposed for new books in this tenant. Existing books and accounts are unchanged; application always needs its own review."
            }
            Self::ApplyTemplate => {
                "Create this starter chart's core groups and accounts, and those of the optional sections chosen, in an empty chart. Sections not chosen are not created. They become the company's own, with no link to the template. A chart that already has accounts is never changed."
            }
            Self::Submit => "Submit a manual journal for approval; no entry posts yet.",
            Self::Approve => {
                "Record an eligible approval under the journal rules. The final required approval posts the immutable entry."
            }
            Self::Reject => "Reject the journal permanently with this reason; no entry posts.",
            Self::Withdraw => {
                "Withdraw your submitted journal before it is approved; it stays on record, can no longer be approved, and no entry posts."
            }
            Self::Replace => {
                "Withdraw your submitted journal and submit the corrected one in its place; the correction needs its own approval and no entry posts."
            }
            Self::ReviewLate => "Keep an independent review of the late recording, with its reason, reviewer and time. The review cannot be changed.",
            Self::Reverse => {
                "Post one mirror entry on the chosen date; the original remains. Owned and closing entries cannot use this action."
            }
            Self::OpenYear => {
                "Open the next fiscal year and its monthly periods; dates must be contiguous with the previous year."
            }
            Self::ClosePeriod => "Close the period to posting until a privileged reopening.",
            Self::ReopenPeriod => {
                "Reopen the period to posting and record the control override with its reason."
            }
            Self::LockPeriod => "Lock the closed period permanently; it can never reopen.",
            Self::CloseYear => {
                "Close the fiscal year and its open periods; post the computed year-end entry to the designated retained-earnings account, if needed."
            }
            Self::ReopenYear => {
                "Reopen the year and only its last period; reverse its closing entry at year-end and record the control override."
            }
            Self::LockYear => {
                "Lock every remaining closed period of this closed fiscal year permanently; the year can never reopen."
            }
        }
    }
    async fn review(
        self,
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
        tx: Option<&MutationTransaction<'_>>,
    ) -> Result<Value> {
        let company = id(input, "company_id")?;
        authorize_on(client, scope, company, self.capability()).await?;
        if matches!(self, Self::CloseYear | Self::ReopenYear) {
            authorize_on(client, scope, company, POST_ENTRIES).await?;
        }
        // Same connection as Plans, including uncommitted owner changes. NOWAIT
        // refuses opposite-company lock orders rather than holding a deadlock.
        let row = client.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT", &[&scope.tenant(), &company]).await?.ok_or(LedgerError::CompanyNotFound)?;
        if row.get::<_, String>(0) == "archived" {
            return Err(LedgerError::CompanyArchived);
        }
        let tenant = scope.tenant();
        let book = books::find(client, tenant, company).await?;
        let changes = match self {
            Self::OpenBook => {
                if book.is_some() {
                    return Err(LedgerError::BookAlreadyOpen);
                }
                let code = shaped_code(input)?;
                let chosen = currency::choose(client, scope, &code)
                    .await?
                    .map_err(|refused| LedgerError::Invalid(vec![refused]))?;
                let units = match input["minor_units"].as_i64() {
                    Some(units) => units,
                    None => currency::default_units(&chosen)
                        .map_err(|missing| LedgerError::Invalid(vec![missing]))?,
                };
                let valid = books::validate(&NewBook {
                    currency_code: code,
                    minor_units: units,
                })
                .map_err(LedgerError::Invalid)?;
                let currency = currency::facts(&valid.currency_code, units, &chosen);
                let latest = duties::latest_change(client, tenant, company).await?;
                let separation_required = input["separation_required"]
                    .as_bool()
                    .unwrap_or(duties::required(latest.as_ref()));
                let reason = if separation_required != duties::required(latest.as_ref()) {
                    authorize_on(client, scope, company, MANAGE_DUTY_SEPARATION).await?;
                    duties::validate_change(separation_required, input["approval_reason"].as_str())
                        .map_err(LedgerError::Invalid)?
                } else {
                    None
                };
                json!({"before":null,"after":{"company_id":company,"currency_code":valid.currency_code,"minor_units":valid.minor_units,"currency":currency,"controlled":true,"separation_required":separation_required,"approval_reason":reason},"duty_separation":duties::latest_change(client,tenant,company).await?})
            }
            Self::CreateAccount | Self::UpdateAccount => {
                let all = accounts::list(client, tenant, company).await?;
                let (before, after) = if matches!(self, Self::CreateAccount) {
                    let valid =
                        accounts::validate_new(client, tenant, company, &new_account(input)?)
                            .await?
                            .map_err(LedgerError::Invalid)?;
                    (
                        Value::Null,
                        json!({"code":valid.code,"name":valid.name,"account_type":valid.account_type,"parent_id":valid.parent_id,"active":valid.active,"retained_earnings":valid.retained_earnings}),
                    )
                } else {
                    let current = accounts::find(client, tenant, company, id(input, "account_id")?)
                        .await?
                        .ok_or(LedgerError::AccountNotFound)?;
                    let after = accounts::validate_change(
                        client,
                        tenant,
                        company,
                        &current,
                        &account_change(input)?,
                        book.as_ref().map_or(MinorUnits(0), |b| b.minor_units),
                    )
                    .await?
                    .map_err(LedgerError::Invalid)?;
                    (json!(current), json!(after))
                };
                let released = all.iter().find(|a| {
                    a.retained_earnings
                        && after["retained_earnings"] == true
                        && after["id"].as_i64() != Some(a.id)
                });
                json!({"before":before,"after":after,"released_retained_earnings":released,"accounts":all})
            }
            Self::Opening => {
                opening::prepare(client, scope, company, &opening_input(input)?)
                    .await?
                    .review
            }
            Self::CreateGroup | Self::UpdateGroup => {
                let all = accounts::list(client, tenant, company).await?;
                let (before, after) = if matches!(self, Self::CreateGroup) {
                    let valid =
                        accounts::validate_new_group(client, tenant, company, &new_group(input)?)
                            .await?
                            .map_err(LedgerError::Invalid)?;
                    (
                        Value::Null,
                        json!({"code":valid.code,"name":valid.name,"account_type":valid.account_type,"parent_id":valid.parent_id,"is_group":true}),
                    )
                } else {
                    let current = accounts::find(client, tenant, company, id(input, "account_id")?)
                        .await?
                        .ok_or(LedgerError::AccountNotFound)?;
                    let after = accounts::validate_group_change(
                        client,
                        tenant,
                        company,
                        &current,
                        &group_change(input)?,
                    )
                    .await?
                    .map_err(LedgerError::Invalid)?;
                    (json!(current), json!(after))
                };
                json!({"before":before,"after":after,"accounts":all})
            }
            Self::SaveTemplate => {
                json!({"after": saved_templates::prepare(client, scope, company, &new_template(input)?).await?})
            }
            Self::ProposeTemplate => {
                let after = match proposal_key(input)? {
                    Some(key) => Some(saved_templates::require(client, tenant, key).await?),
                    None => None,
                };
                json!({"before": saved_templates::proposal(client, scope).await?, "after":after})
            }
            Self::ApplyTemplate => {
                let preview = chart_template_preview(
                    client,
                    tenant,
                    company,
                    text(input, "template")?,
                    &sections(input)?,
                )
                .await?;
                if !preview.applicable {
                    return Err(LedgerError::Invalid(vec![FieldError::new(
                        "base",
                        format!(
                            "The chart already has {} account(s) or group(s). A starter chart applies only to an empty chart.",
                            preview.existing_rows
                        ),
                    )]));
                }
                let groups = preview.template.rows.iter().filter(|r| r.is_group).count();
                let offered: Vec<Value> = preview.template.sections.iter().map(|s| json!({"key":s.key,"name":s.name,"description":s.description,"chosen":preview.chosen_sections.contains(&s.key)})).collect();
                json!({"template":{"key":preview.template.key,"name":preview.template.name,"review":preview.template.review,"notes":preview.template.notes},"sections":offered,"groups":groups,"accounts":preview.template.rows.len() - groups,"rows":preview.template.rows})
            }
            Self::Submit | Self::Approve | Self::Reject | Self::Withdraw | Self::Replace => {
                let journal_policy = if matches!(self, Self::Submit | Self::Replace) {
                    let sql = tx
                        .map(|t| t.sql())
                        .or_else(|| client.transaction())
                        .ok_or(Error::Internal)?;
                    Some(journal_approvals::submission_policy(sql, scope, company).await?)
                } else {
                    None
                };
                let current = if matches!(self, Self::Submit) {
                    None
                } else if matches!(self, Self::Withdraw | Self::Replace) {
                    Some(
                        withdrawable_journal(client, scope, company, id(input, "journal_id")?)
                            .await?,
                    )
                } else {
                    Some(
                        submitted_journal(client, tenant, company, id(input, "journal_id")?)
                            .await?,
                    )
                };
                let approval = if matches!(self, Self::Approve | Self::Reject) {
                    let sql = tx
                        .map(|t| t.sql())
                        .or_else(|| client.transaction())
                        .ok_or(Error::Internal)?;
                    Some(
                        journal_approvals::preview(sql, scope, company, id(input, "journal_id")?)
                            .await?,
                    )
                } else {
                    None
                };
                if let Some(item) = &approval {
                    journal_approvals::single_step(item)?;
                    if matches!(self, Self::Approve) && item.approval_reason_required {
                        return Err(LedgerError::ApprovalRefused("These rules require an approval reason. Decide this journal from Waiting for my approval.".into()));
                    }
                }
                if matches!(self, Self::Replace) {
                    opening::refuse_manual_replacement(
                        client,
                        scope,
                        company,
                        id(input, "journal_id")?,
                    )
                    .await?;
                }
                if matches!(self, Self::Withdraw | Self::Replace) {
                    journals::validate_withdrawal_reason(opt_text(input, "reason").as_deref())
                        .map_err(LedgerError::Invalid)?;
                }
                if matches!(self, Self::Reject) {
                    let reason = journals::validate_reason(text(input, "reason")?)
                        .map_err(LedgerError::Invalid)?;
                    json!({"before":current,"status":"rejected","reason":reason,"approval":approval})
                } else if matches!(self, Self::Withdraw) {
                    json!({"before":current,"status":"withdrawn","reason":opt_text(input, "reason"),"posts":false})
                } else {
                    let book = book.as_ref().ok_or(LedgerError::BookNotOpen)?;
                    let new = match &current {
                        Some(journal) if matches!(self, Self::Approve) => journals::entry(journal),
                        _ => new_entry(input, book.minor_units)?,
                    };
                    let valid = entries::validate(
                        client,
                        tenant,
                        company,
                        book.minor_units,
                        &new,
                        None,
                        entries::Inactive::Listed(&[]),
                    )
                    .await?
                    .map_err(LedgerError::Invalid)?;
                    json!({"before":current,"entry":valid,"posts_when_approval_complete":matches!(self,Self::Approve),"withdraws":matches!(self,Self::Replace),"approval":approval,"journal_policy":journal_policy,"accounts":accounts::list(client,tenant,company).await?,"periods":periods::state(client,tenant,company).await?.years})
                }
            }
            Self::ReviewLate => {
                let entry_id = id(input, "entry_id")?;
                let reason = late_recordings::prepare(
                    client,
                    scope,
                    company,
                    entry_id,
                    text(input, "reason")?,
                )
                .await?;
                let entry = entries::find(client, tenant, company, entry_id)
                    .await?
                    .ok_or(LedgerError::EntryNotFound)?;
                json!({"entry_id":entry_id,"entry_number":entry.entry_number,"entry_date":entry.entry_date.to_string(),"recording":late_recordings::facts(client,tenant,company,entry_id).await?,"reason":reason})
            }
            Self::Reverse => {
                let (_, original, valid) = prepare_reverse(
                    client,
                    tenant,
                    company,
                    id(input, "entry_id")?,
                    None,
                    &reversal(input)?,
                )
                .await?;
                json!({"before":original,"entry":valid})
            }
            Self::OpenYear => {
                book.as_ref().ok_or(LedgerError::BookNotOpen)?;
                let state = periods::state(client, tenant, company).await?;
                let (start, end) = periods::validate_year(state.years.last(), &new_year(input)?)
                    .map_err(LedgerError::Invalid)?;
                json!({"previous":state.years.last(),"start_date":start.to_string(),"end_date":end.to_string(),"periods":"calendar months, all open"})
            }
            Self::ClosePeriod | Self::ReopenPeriod | Self::LockPeriod => {
                let state = periods::state(client, tenant, company).await?;
                let p = state
                    .period(id(input, "period_id")?)
                    .ok_or(LedgerError::PeriodNotFound)?;
                let (previous, next) = state.neighbours(p.id);
                let required = if matches!(self, Self::ReopenPeriod) {
                    Some("is required to reopen")
                } else {
                    None
                };
                match self {
                    Self::ClosePeriod => {
                        if p.status != PeriodStatus::Open {
                            return Err(LedgerError::PeriodNotOpen);
                        }
                        if previous.is_some_and(|p| p.status == PeriodStatus::Open) {
                            return Err(LedgerError::EarlierPeriodOpen);
                        }
                        if state.year(p.fiscal_year_id).is_some_and(|y| {
                            y.end_date == p.end_date && y.status != YearStatus::Closed
                        }) {
                            return Err(LedgerError::ClosesWithFiscalYear);
                        }
                    }
                    Self::ReopenPeriod => {
                        if p.status == PeriodStatus::Locked {
                            return Err(LedgerError::PeriodLocked);
                        }
                        if p.status != PeriodStatus::Closed {
                            return Err(LedgerError::PeriodNotClosed);
                        }
                        if state
                            .year(p.fiscal_year_id)
                            .is_some_and(|y| y.status == YearStatus::Closed)
                        {
                            return Err(LedgerError::FiscalYearClosed);
                        }
                        if next.is_some_and(|p| p.status != PeriodStatus::Open) {
                            return Err(LedgerError::LaterPeriodClosed);
                        }
                    }
                    _ => {
                        if p.status == PeriodStatus::Locked {
                            return Err(LedgerError::PeriodLocked);
                        }
                        if p.status != PeriodStatus::Closed {
                            return Err(LedgerError::PeriodNotClosed);
                        }
                        if previous.is_some_and(|p| p.status != PeriodStatus::Locked) {
                            return Err(LedgerError::EarlierPeriodUnlocked);
                        }
                    }
                }
                let reason = periods::reason(input["reason"].as_str(), required)
                    .map_err(LedgerError::Invalid)?;
                json!({"before":p,"reason":reason,"approval_exception":matches!(self,Self::ReopenPeriod)})
            }
            Self::CloseYear | Self::ReopenYear | Self::LockYear => {
                self.review_year(client, scope, company, input).await?
            }
        };
        Ok(json!({"company_id":company,"book":book,"summary":self.consequence(),"changes":changes}))
    }

    async fn review_year(
        self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        let tenant = scope.tenant();
        let book = books::find(client, tenant, company)
            .await?
            .ok_or(LedgerError::BookNotOpen)?;
        let state = periods::state(client, tenant, company).await?;
        let year = state
            .year(id(input, "fiscal_year_id")?)
            .ok_or(LedgerError::FiscalYearNotFound)?;
        let last = year.periods.last().ok_or(Error::Internal)?;
        match self {
            Self::LockYear => {
                if year.status != YearStatus::Closed {
                    return Err(LedgerError::FiscalYearNotClosed);
                }
                let first = year.periods.first().ok_or(Error::Internal)?;
                if state
                    .neighbours(first.id)
                    .0
                    .is_some_and(|p| p.status != PeriodStatus::Locked)
                {
                    return Err(LedgerError::EarlierPeriodUnlocked);
                }
                let reason = periods::reason(input["reason"].as_str(), None)
                    .map_err(LedgerError::Invalid)?;
                Ok(
                    json!({"before":year,"lock_period_ids":year.periods.iter().filter(|p|p.status != PeriodStatus::Locked).map(|p|p.id).collect::<Vec<_>>(),"reason":reason}),
                )
            }
            Self::CloseYear => {
                if year.status == YearStatus::Closed {
                    return Ok(json!({"before":year,"unchanged":true}));
                }
                let open = year
                    .periods
                    .iter()
                    .find(|p| p.status == PeriodStatus::Open)
                    .ok_or(Error::Internal)?;
                if state
                    .neighbours(open.id)
                    .0
                    .is_some_and(|p| p.status == PeriodStatus::Open)
                {
                    return Err(LedgerError::EarlierPeriodOpen);
                }
                let all = accounts::list(client, tenant, company).await?;
                let retained = all
                    .iter()
                    .find(|a| a.retained_earnings && a.active)
                    .ok_or(LedgerError::NoRetainedEarningsAccount)?;
                let balances = periods::closing_balances(client, tenant, company, year).await?;
                let closing = if balances.is_empty() {
                    None
                } else {
                    let net: rust_decimal::Decimal = balances.iter().map(|(_, n)| *n).sum();
                    let mut lines: Vec<_> = balances.iter().map(|(id, n)| side(*id, -*n)).collect();
                    if !net.is_zero() {
                        lines.push(side(retained.id, net));
                    }
                    let new = NewEntry {
                        entry_date: year.end_date,
                        description: opt_text(input, "description").unwrap_or_else(|| {
                            format!("Close fiscal year {} to {}", year.start_date, year.end_date)
                        }),
                        reference: opt_text(input, "reference"),
                        lines,
                    };
                    Some(
                        entries::validate(
                            client,
                            tenant,
                            company,
                            book.minor_units,
                            &new,
                            None,
                            entries::Inactive::Any,
                        )
                        .await?
                        .map_err(LedgerError::Invalid)?,
                    )
                };
                Ok(json!({"before":year,"retained_earnings":retained,"entry":closing}))
            }
            _ => {
                if year.status == YearStatus::Open {
                    return Err(LedgerError::FiscalYearNotClosed);
                }
                if last.status == PeriodStatus::Locked {
                    return Err(LedgerError::PeriodLocked);
                }
                if state
                    .neighbours(last.id)
                    .1
                    .is_some_and(|p| p.status != PeriodStatus::Open)
                {
                    return Err(LedgerError::LaterPeriodClosed);
                }
                let reason =
                    periods::reason(input["reason"].as_str(), Some("is required to reopen"))
                        .map_err(LedgerError::Invalid)?;
                let reversal = if let Some(entry) = year.closing_entry_id {
                    let original = entries::find(client, tenant, company, entry)
                        .await?
                        .ok_or(Error::Internal)?;
                    let new = entries::mirror(
                        &original,
                        year.end_date,
                        format!(
                            "Reopen fiscal year {} to {}",
                            year.start_date, year.end_date
                        ),
                    );
                    Some(
                        entries::validate_reopening(
                            client,
                            tenant,
                            company,
                            book.minor_units,
                            &new,
                            year,
                        )
                        .await?
                        .map_err(LedgerError::Invalid)?
                        .reversing(&original),
                    )
                } else {
                    None
                };
                Ok(
                    json!({"before":year,"entry":reversal,"reopen_period_id":last.id,"reason":reason,"approval_exception":true}),
                )
            }
        }
    }
}

// The pinned engine previews all operations before running any. Its digest
// fences other transactions, but not an earlier operation in this same plan.
// Keep only hashes in a transaction-local FIFO; compare again immediately
// before each write. No persistent state, pooled side reads or business writes.
async fn fence(
    client: &dyn OperationClient,
    value: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let encoded = value.to_string();
    let row = client.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'), COALESCE(NULLIF(current_setting('finance.ledger_reviews',true),''),'[]')::jsonb", &[&encoded]).await?.ok_or(Error::Internal)?;
    let digest: String = row.get(0);
    let mut pending: Vec<String> =
        serde_json::from_value(row.get(1)).map_err(|_| Error::Internal)?;
    if consume {
        if pending.first() != Some(&digest) {
            return Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict,
            ));
        }
        pending.remove(0);
    } else {
        pending.push(digest);
    }
    client
        .query_opt(
            "SELECT set_config('finance.ledger_reviews',$1,true)",
            &[&json!(pending).to_string()],
        )
        .await?;
    Ok(())
}

impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let review = self
                .review(client, scope, input, None)
                .await
                .map_err(invalid)?;
            fence(
                client,
                &json!({"action":self.consequence(),"input":input,"review":review}),
                false,
            )
            .await?;
            Ok(review.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = id(input, "company_id").map_err(invalid)?;
            join(tx, scope, company, self.capability())
                .await
                .map_err(invalid)?;
            let review = self
                .review(tx.sql(), scope, input, Some(&*tx))
                .await
                .map_err(invalid)?;
            fence(
                tx.sql(),
                &json!({"action":self.consequence(),"input":input,"review":review}),
                true,
            )
            .await?;
            let record = match self {
                Self::OpenBook => {
                    let code = shaped_code(input).map_err(invalid)?;
                    // This transaction's own review chose the places (given, or the standard).
                    let minor_units = review["changes"]["after"]["minor_units"].as_i64();
                    let minor_units = minor_units.ok_or(Error::Internal)?;
                    let book = Ledger::open_book_on(
                        tx,
                        scope,
                        company,
                        &NewBook {
                            currency_code: code,
                            minor_units,
                        },
                    )
                    .await
                    .map_err(invalid)?;
                    if let Some(required) = input["separation_required"].as_bool() {
                        let latest =
                            duties::latest_change(tx.sql(), scope.tenant(), company).await?;
                        if required != duties::required(latest.as_ref()) {
                            authorize_on(tx.sql(), scope, company, MANAGE_DUTY_SEPARATION)
                                .await
                                .map_err(invalid)?;
                            let reason = duties::validate_change(
                                required,
                                input["approval_reason"].as_str(),
                            )
                            .map_err(|e| invalid(LedgerError::Invalid(e)))?;
                            let tenant = scope.tenant();
                            let (actor, impersonator) = people(scope);
                            tx.mutate(move |sql| {
                                Box::pin(async move {
                                    let change = duties::record_change(
                                        sql,
                                        tenant,
                                        company,
                                        actor,
                                        impersonator,
                                        required,
                                        reason.as_deref(),
                                    )
                                    .await?;
                                    Ok(((), vec![change]))
                                })
                            })
                            .await?;
                        }
                    }
                    json!(book)
                }
                Self::CreateAccount => json!(Ledger::create_account_on(
                    tx,
                    scope,
                    company,
                    &new_account(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::UpdateAccount => json!(Ledger::update_account_on(
                    tx,
                    scope,
                    company,
                    id(input, "account_id").map_err(invalid)?,
                    &account_change(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::Opening => json!(Ledger::submit_opening_balances_on(
                    tx,
                    scope,
                    company,
                    &opening_input(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::CreateGroup => json!(Ledger::create_account_group_on(
                    tx,
                    scope,
                    company,
                    &new_group(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::UpdateGroup => json!(Ledger::update_account_group_on(
                    tx,
                    scope,
                    company,
                    id(input, "account_id").map_err(invalid)?,
                    &group_change(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::SaveTemplate => json!(Ledger::save_chart_template_on(
                    tx,
                    scope,
                    company,
                    &new_template(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::ProposeTemplate => Ledger::propose_chart_template_on(
                    tx,
                    scope,
                    company,
                    proposal_key(input).map_err(invalid)?,
                )
                .await
                .map_err(invalid)?,
                Self::ApplyTemplate => json!(Ledger::apply_chart_template_on(
                    tx,
                    scope,
                    company,
                    text(input, "template").map_err(invalid)?,
                    &sections(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::Submit => {
                    let book = books::find(tx.sql(), scope.tenant(), company)
                        .await?
                        .ok_or(Error::Internal)?;
                    json!(Ledger::submit_journal_on(
                        tx,
                        scope,
                        company,
                        &new_entry(input, book.minor_units).map_err(invalid)?
                    )
                    .await
                    .map_err(invalid)?)
                }
                Self::Approve => json!(Ledger::approve_journal_on(
                    tx,
                    scope,
                    company,
                    id(input, "journal_id").map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::Withdraw => json!(Ledger::withdraw_journal_on(
                    tx,
                    scope,
                    company,
                    id(input, "journal_id").map_err(invalid)?,
                    opt_text(input, "reason").as_deref()
                )
                .await
                .map_err(invalid)?),
                Self::Replace => {
                    let book = books::find(tx.sql(), scope.tenant(), company)
                        .await?
                        .ok_or(Error::Internal)?;
                    json!(Ledger::replace_journal_on(
                        tx,
                        scope,
                        company,
                        id(input, "journal_id").map_err(invalid)?,
                        &new_entry(input, book.minor_units).map_err(invalid)?,
                        opt_text(input, "reason").as_deref()
                    )
                    .await
                    .map_err(invalid)?)
                }
                Self::Reject => json!(Ledger::reject_journal_on(
                    tx,
                    scope,
                    company,
                    id(input, "journal_id").map_err(invalid)?,
                    text(input, "reason").map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::ReviewLate => json!(Ledger::record_late_recording_review_on(
                    tx,
                    scope,
                    company,
                    id(input, "entry_id").map_err(invalid)?,
                    text(input, "reason").map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::Reverse => json!(reverse(
                    tx,
                    scope,
                    company,
                    id(input, "entry_id").map_err(invalid)?,
                    None,
                    &reversal(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::OpenYear => json!(Ledger::open_fiscal_year_on(
                    tx,
                    scope,
                    company,
                    &new_year(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::ClosePeriod => json!(Ledger::close_period_on(
                    tx,
                    scope,
                    company,
                    id(input, "period_id").map_err(invalid)?,
                    input["reason"].as_str()
                )
                .await
                .map_err(invalid)?),
                Self::ReopenPeriod => json!(Ledger::reopen_period_on(
                    tx,
                    scope,
                    company,
                    id(input, "period_id").map_err(invalid)?,
                    text(input, "reason").map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::LockPeriod => json!(Ledger::lock_period_on(
                    tx,
                    scope,
                    company,
                    id(input, "period_id").map_err(invalid)?,
                    input["reason"].as_str()
                )
                .await
                .map_err(invalid)?),
                Self::CloseYear => json!(Ledger::close_fiscal_year_on(
                    tx,
                    scope,
                    company,
                    id(input, "fiscal_year_id").map_err(invalid)?,
                    &year_close(input)
                )
                .await
                .map_err(invalid)?),
                Self::ReopenYear => json!(Ledger::reopen_fiscal_year_on(
                    tx,
                    scope,
                    company,
                    id(input, "fiscal_year_id").map_err(invalid)?,
                    text(input, "reason").map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::LockYear => {
                    let year_id = id(input, "fiscal_year_id").map_err(invalid)?;
                    let year = fiscal_year(tx.sql(), scope.tenant(), company, year_id)
                        .await
                        .map_err(invalid)?;
                    for p in year
                        .periods
                        .iter()
                        .filter(|p| p.status != PeriodStatus::Locked)
                    {
                        Ledger::lock_period_on(tx, scope, company, p.id, input["reason"].as_str())
                            .await
                            .map_err(invalid)?;
                    }
                    json!(fiscal_year(tx.sql(), scope.tenant(), company, year_id)
                        .await
                        .map_err(invalid)?)
                }
            };
            Ok(json!({"record":record,"path":null}).into())
        })
    }
}

enum Read {
    Book,
    OpeningProgress,
    Currencies,
    Accounts,
    Journal,
    Journals,
    JournalList,
    Entry,
    Entries,
    Years,
    TrialBalance,
    ChartBalances,
    ChartTemplates,
    ChartTemplate,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = id(input, "company_id").map_err(invalid)?;
            authorize_on(client, scope, company, VIEW)
                .await
                .map_err(invalid)?;
            let tenant = scope.tenant();
            let limit = input["page_size"].as_u64().unwrap_or(100).min(100) as usize;
            let result = match self {
                Self::OpeningProgress => {
                    json!({"opening": opening::progress(client, scope, company).await.map_err(invalid)?})
                }
                Self::Book => {
                    let book = books::find(client, tenant, company).await?;
                    let currency = match &book {
                        Some(book) => {
                            currency::describe(
                                client,
                                scope,
                                &book.currency_code,
                                i64::from(book.minor_units.get()),
                            )
                            .await?
                        }
                        None => Value::Null,
                    };
                    json!({"starter_chart":saved_templates::proposal(client, scope).await.map_err(invalid)?,"book":book,"currency":currency,"controlled_bookkeeping":bookkeeping::latest(client,tenant,company).await?,"duty_separation":duties::latest_change(client,tenant,company).await?,"latest_entry_date":entries::latest_date(client,tenant,company).await?})
                }
                Self::Currencies => {
                    currency::options(
                        client,
                        scope,
                        company,
                        input["search"].as_str().unwrap_or(""),
                    )
                    .await?
                }
                Self::Accounts => {
                    let mut rows = accounts::page(
                        client,
                        tenant,
                        company,
                        input["after_code"].as_str(),
                        limit as i64 + 1,
                    )
                    .await?;
                    let has_more = rows.len() > limit;
                    rows.truncate(limit);
                    let next = has_more
                        .then(|| rows.last().map(|a| a.code.clone()))
                        .flatten();
                    json!({"records":rows,"next_after_code":next})
                }
                Self::Journal => {
                    let journal_id = id(input, "journal_id").map_err(invalid)?;
                    let journal = journals::find(client, tenant, company, journal_id)
                        .await?
                        .ok_or(bilibi_agent_api::Error::NotFound)?;
                    let mut record = json!(journal);
                    record["opening"] =
                        json!(opening::is_opening(client, tenant, company, journal_id)
                            .await
                            .map_err(invalid)?);
                    record
                }
                Self::Journals => {
                    let rows = journals::list(
                        client,
                        tenant,
                        company,
                        &JournalQuery {
                            status: input["status"].as_str().and_then(JournalStatus::parse),
                            limit: Some(limit as u32),
                            after: input["after"].as_i64(),
                        },
                    )
                    .await
                    .map_err(invalid)?;
                    json!({"next_after":rows.last().map(|j|j.journal_number),"records":rows})
                }
                Self::JournalList => {
                    let from = optional_date(input, "from").map_err(invalid)?;
                    let to = optional_date(input, "to").map_err(invalid)?;
                    if matches!((from, to), (Some(a), Some(b)) if a > b) {
                        return Err(invalid(LedgerError::InvalidOption));
                    }
                    let cursor = (
                        input["after_time"].as_str(),
                        input["after_id"].as_i64(),
                        input["after_kind"].as_str(),
                    );
                    match cursor {
                        (None, None, None) => {}
                        (Some(time), Some(id), Some(kind))
                            if id > 0
                                && matches!(kind, "entry" | "journal")
                                && time.parse::<jiff::civil::DateTime>().is_ok() => {}
                        _ => return Err(invalid(LedgerError::InvalidOption)),
                    }
                    journal_list::list(client, tenant, company, input, limit).await?
                }
                Self::Entry => {
                    let entry = entries::find(
                        client,
                        tenant,
                        company,
                        id(input, "entry_id").map_err(invalid)?,
                    )
                    .await?
                    .ok_or(bilibi_agent_api::Error::NotFound)?;
                    let request = client
                        .query_opt(
                            "SELECT j.journal_number FROM finance_ledger_journal_events v JOIN finance_ledger_journals j ON j.id=v.journal_id AND j.tenant_id=v.tenant_id AND j.company_id=v.company_id WHERE v.tenant_id=$1 AND v.company_id=$2 AND v.entry_id=$3",
                            &[&tenant, &company, &entry.id],
                        )
                        .await?
                        .map(|row| row.get::<_, i64>(0));
                    let mut value = json!(entry);
                    value["request_number"] = json!(request);
                    value["opening"] = json!(client.query_opt("SELECT o.id FROM finance_ledger_opening_balances o JOIN finance_ledger_journal_events v ON v.journal_id=o.journal_id AND v.tenant_id=o.tenant_id AND v.company_id=o.company_id WHERE o.tenant_id=$1 AND o.company_id=$2 AND v.entry_id=$3", &[&tenant,&company,&entry.id]).await?.is_some());
                    value["late_recording"] =
                        late_recordings::facts(client, tenant, company, entry.id).await?;
                    value
                }
                Self::Entries => {
                    let after =
                        if input.get("after_id").is_some() || input.get("after_date").is_some() {
                            Some((
                                date(input, "after_date").map_err(invalid)?,
                                id(input, "after_id").map_err(invalid)?,
                            ))
                        } else {
                            None
                        };
                    let rows = entries::list(
                        client,
                        tenant,
                        company,
                        &EntryQuery {
                            from: optional_date(input, "from").map_err(invalid)?,
                            to: optional_date(input, "to").map_err(invalid)?,
                            limit: Some(limit as u32),
                            after,
                            account_id: input["account_id"].as_i64(),
                            through_entry_number: input["through_entry_number"].as_i64(),
                            exclude_closing: input["exclude_closing"].as_bool().unwrap_or(false),
                        },
                    )
                    .await
                    .map_err(invalid)?;
                    json!({"next_after":rows.last().map(|e|json!({"after_date":e.entry_date.to_string(),"after_id":e.id})),"records":rows})
                }
                Self::TrialBalance => json!(trial_balance_of(
                    client,
                    tenant,
                    company,
                    date(input, "as_of").map_err(invalid)?,
                    input["through_entry_number"].as_i64(),
                )
                .await
                .map_err(invalid)?),
                Self::ChartBalances => json!(chart_balances_of(
                    client,
                    tenant,
                    company,
                    date(input, "as_of").map_err(invalid)?,
                    input["through_entry_number"].as_i64(),
                )
                .await
                .map_err(invalid)?),
                Self::ChartTemplates => {
                    let mut records = Vec::new();
                    for template in saved_templates::all(client, tenant)
                        .await
                        .map_err(invalid)?
                    {
                        let preview =
                            chart_template_preview(client, tenant, company, &template.key, &[])
                                .await
                                .map_err(invalid)?;
                        let core = &preview.template.rows;
                        let groups = core.iter().filter(|r| r.is_group).count();
                        records.push(json!({"key":template.key,"name":template.name,"review":template.review,"description":template.description,"sections":template.sections,"groups":groups,"accounts":core.len() - groups,"applicable":preview.applicable}));
                    }
                    json!({"records":records,"proposal":saved_templates::proposal(client, scope).await.map_err(invalid)?})
                }
                Self::ChartTemplate => json!(chart_template_preview(
                    client,
                    tenant,
                    company,
                    text(input, "template").map_err(invalid)?,
                    &sections(input).map_err(invalid)?
                )
                .await
                .map_err(invalid)?),
                Self::Years => {
                    let years = periods::state(client, tenant, company).await?.years;
                    let after = optional_date(input, "after_start_date").map_err(invalid)?;
                    let mut rows = years
                        .into_iter()
                        .filter(|y| after.is_none_or(|d| y.start_date > d))
                        .filter(|y| {
                            input["status"].as_str().is_none_or(|status| {
                                status
                                    == if y
                                        .periods
                                        .iter()
                                        .all(|p| p.status == crate::PeriodStatus::Locked)
                                    {
                                        "locked"
                                    } else {
                                        y.status.as_str()
                                    }
                            })
                        });
                    let page: Vec<_> = rows.by_ref().take(limit).collect();
                    let next = rows
                        .next()
                        .and_then(|_| page.last().map(|y| y.start_date.to_string()));
                    json!({"records":page,"next_after_start_date":next})
                }
            };
            Ok(result.into())
        })
    }
}

fn identifier() -> Value {
    json!({"type":"integer","minimum":1})
}
fn string(max: usize) -> Value {
    json!({"type":"string","maxLength":max})
}
fn day() -> Value {
    json!({"type":"string","format":"date"})
}
fn balance() -> Value {
    json!({"type":"boolean","description":"On at most one line, instead of debit and credit: the line takes the debit or credit that balances the other lines. The preview shows the figure."})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
fn schema(required: &[&str], properties: Value) -> Value {
    let mut properties = properties.as_object().cloned().unwrap_or_default();
    properties.insert("company_id".into(), identifier());
    let required: Vec<_> = std::iter::once("company_id")
        .chain(required.iter().copied())
        .collect();
    json!({"type":"object","required":required,"properties":properties})
}
fn output() -> Value {
    json!({"type":"object","description":"Ledger records or deterministic review facts. Exact amounts are decimal strings. A write commits no page path; it answers null."})
}
fn book_schema() -> Value {
    schema(
        &["currency_code"],
        json!({"currency_code":{"type":"string","maxLength":3,"description":"A currency from finance.ledger.book.currencies, by its three-letter code. Permanent."},"minor_units":{"type":"integer","minimum":0,"maximum":4,"description":"Decimal places. Omit to use the currency's ISO 4217 standard, which the review shows; a different value is allowed and flagged as nonstandard. Required when the currency list is unavailable. Permanent."},"separation_required":boolean(),"approval_reason":string(500)}),
    )
}
fn account_schema() -> Value {
    schema(
        &["code", "name", "account_type"],
        json!({"code":string(50),"name":string(200),"account_type":{"type":"string","enum":["asset","liability","equity","income","expense"]},"parent_id":identifier(),"active":boolean(),"retained_earnings":boolean()}),
    )
}
fn group_schema() -> Value {
    schema(
        &["code", "name", "account_type"],
        json!({"code":string(50),"name":string(200),"account_type":{"type":"string","enum":["asset","liability","equity","income","expense"]},"parent_id":identifier()}),
    )
}
fn group_change_schema() -> Value {
    schema(
        &["account_id"],
        json!({"account_id":identifier(),"name":string(200),"parent_id":identifier(),"clear_parent":boolean()}),
    )
}
fn new_template(input: &Value) -> Result<NewChartTemplate> {
    Ok(NewChartTemplate {
        key: text(input, "key")?.into(),
        name: text(input, "name")?.into(),
        description: text(input, "description")?.into(),
        optional_group_codes: match &input["optional_group_codes"] {
            Value::Null => Vec::new(),
            value => {
                serde_json::from_value(value.clone()).map_err(|_| LedgerError::InvalidOption)?
            }
        },
    })
}
fn save_template_schema() -> Value {
    schema(
        &["key", "name", "description"],
        json!({"key":string(64),"name":string(200),"description":string(1000),"optional_group_codes":{"type":"array","items":string(50)}}),
    )
}
fn proposal_key(input: &Value) -> Result<Option<&str>> {
    if input["clear"].as_bool() == Some(true) {
        if input.get("template").is_some() {
            return Err(LedgerError::Invalid(vec![FieldError::new(
                "template",
                "cannot be set with clear",
            )]));
        }
        Ok(None)
    } else {
        input["template"].as_str().map(Some).ok_or_else(|| {
            LedgerError::Invalid(vec![FieldError::new(
                "template",
                "choose a template, or set clear to true",
            )])
        })
    }
}
fn propose_template_schema() -> Value {
    schema(
        &[],
        json!({"template":string(100),"clear":{"type":"boolean"}}),
    )
}
fn template_schema() -> Value {
    schema(
        &["template"],
        json!({"template":string(100),"sections":{"type":"array","items":string(100)}}),
    )
}
fn change_schema() -> Value {
    schema(
        &["account_id"],
        json!({"account_id":identifier(),"name":string(200),"parent_id":identifier(),"clear_parent":boolean(),"active":boolean(),"retained_earnings":boolean()}),
    )
}
fn submit_schema() -> Value {
    schema(
        &["entry_date", "description", "lines"],
        json!({"entry_date":day(),"description":string(500),"reference":string(100),"lines":{"type":"array","items":{"type":"object","required":["account_id"],"properties":{"account_id":identifier(),"debit":string(25),"credit":string(25),"balance":balance(),"description":string(500)}}}}),
    )
}
fn withdraw_schema() -> Value {
    schema(
        &["journal_id"],
        json!({"journal_id":identifier(),"reason":string(500)}),
    )
}
fn replace_schema() -> Value {
    schema(
        &["journal_id", "entry_date", "description", "lines"],
        json!({"journal_id":identifier(),"reason":string(500),"entry_date":day(),"description":string(500),"reference":string(100),"lines":{"type":"array","items":{"type":"object","required":["account_id"],"properties":{"account_id":identifier(),"debit":string(25),"credit":string(25),"balance":balance(),"description":string(500)}}}}),
    )
}
fn opening_input(input: &Value) -> Result<OpeningBalances> {
    Ok(OpeningBalances {
        conversion_date: text(input, "conversion_date")?.parse().map_err(|_| {
            LedgerError::Invalid(vec![FieldError::new(
                "conversion_date",
                "has invalid format",
            )])
        })?,
        currency_code: text(input, "currency_code")?.into(),
        source_reference: text(input, "source_reference")?.into(),
        csv: text(input, "csv")?.into(),
        replaces_journal_id: input
            .get("replaces_journal_id")
            .filter(|value| !value.is_null())
            .map(|_| id(input, "replaces_journal_id"))
            .transpose()?,
    })
}
fn opening_schema() -> Value {
    schema(
        &[
            "conversion_date",
            "currency_code",
            "source_reference",
            "csv",
        ],
        json!({
            "conversion_date":{"type":"string","format":"date","description":"First date Bilibi owns transaction processing; use the source closing trial balance immediately before it."},
            "currency_code":{"type":"string","maxLength":3,"description":"Must match the book currency exactly; no currency conversion."},
            "source_reference":{"type":"string","maxLength":100,"description":"Permanent source export/batch identifier. Identical retries return the recorded journal."},
            "replaces_journal_id":{"type":"integer","minimum":1,"description":"Optional. The waiting opening journal you submitted, to correct: it is withdrawn and this submission becomes the book's opening, needing its own approval. Use a new source_reference. Omit for a first submission, or after the earlier opening was rejected or withdrawn."},
            "csv":{"type":"string","maxLength":131072,"description":"UTF-8 CSV with header source_id,account_code,debit,credit; 2–500 nonzero rows, unique source IDs, existing active account codes, one positive ungrouped amount per row. At most 128 KiB."}
        }),
    )
}
fn journal_schema() -> Value {
    schema(&["journal_id"], json!({"journal_id":identifier()}))
}
fn reject_schema() -> Value {
    schema(
        &["journal_id", "reason"],
        json!({"journal_id":identifier(),"reason":string(500)}),
    )
}
fn reverse_schema() -> Value {
    schema(
        &["entry_id", "entry_date", "description"],
        json!({"entry_id":identifier(),"entry_date":day(),"description":string(500),"reference":string(100)}),
    )
}
fn open_year_schema() -> Value {
    schema(&[], json!({"start_date":day(),"end_date":day()}))
}
fn period_schema() -> Value {
    schema(
        &["period_id"],
        json!({"period_id":identifier(),"reason":string(500)}),
    )
}
fn reopen_period_schema() -> Value {
    schema(
        &["period_id", "reason"],
        json!({"period_id":identifier(),"reason":string(500)}),
    )
}
fn close_year_schema() -> Value {
    schema(
        &["fiscal_year_id"],
        json!({"fiscal_year_id":identifier(),"description":string(500),"reference":string(100)}),
    )
}
fn reopen_year_schema() -> Value {
    schema(
        &["fiscal_year_id", "reason"],
        json!({"fiscal_year_id":identifier(),"reason":string(500)}),
    )
}
fn lock_year_schema() -> Value {
    schema(
        &["fiscal_year_id"],
        json!({"fiscal_year_id":identifier(),"reason":string(500)}),
    )
}
fn target_schema() -> Value {
    schema(&[], json!({}))
}
fn accounts_schema() -> Value {
    schema(
        &[],
        json!({"after_code":string(50),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn journals_schema() -> Value {
    schema(
        &[],
        json!({"status":{"type":"string","enum":["submitted","approved","rejected","withdrawn"]},"after":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn journal_list_schema() -> Value {
    schema(
        &[],
        json!({"from":day(),"to":day(),"status":string(32),"source":string(100),"recording":{"type":"string","enum":["on_time","approaching","late","unposted"]},"review":{"type":"string","enum":["pending","reviewed"]},"after_time":string(32),"after_id":identifier(),"after_kind":string(16),"account_id":identifier(),"through_entry_number":{"type":"integer","minimum":0},"search":string(20),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn late_review_schema() -> Value {
    schema(
        &["entry_id", "reason"],
        json!({"entry_id":identifier(), "reason":string(500)}),
    )
}
fn entry_schema() -> Value {
    schema(&["entry_id"], json!({"entry_id":identifier()}))
}
fn entries_schema() -> Value {
    schema(
        &[],
        json!({"from":day(),"to":day(),"after_date":day(),"after_id":identifier(),"account_id":identifier(),"through_entry_number":{"type":"integer","minimum":0},"exclude_closing":boolean(),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn trial_balance_schema() -> Value {
    schema(
        &["as_of"],
        json!({"as_of":day(),"through_entry_number":{"type":"integer","minimum":0}}),
    )
}
fn currencies_schema() -> Value {
    schema(
        &[],
        json!({"search":{"type":"string","maxLength":100,"description":"Part of a currency code or name."}}),
    )
}
fn years_schema() -> Value {
    schema(
        &[],
        json!({"status":{"type":"string","enum":["open","closed","locked"]},"after_start_date":day(),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}

inventory::submit! { Contribution::new(DEFINITIONS,"finance/ledger.operations",Provenance { module:MODULE,source:file!(),line:line!() }, &Definitions {
    operations:&[
        Operation {key:"finance.ledger.book.get",title:"Read the book",summary:"Read currency, its name and standard decimal places, minor units and bookkeeping controls; book is null before opening.",keywords:&[],capability:VIEW,input:target_schema,output,handler:Handler::Read(&Read::Book),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.opening_balances.get",title:"Read opening-balance progress",summary:"Read the latest opening submission, its approval status, posted entry and reversal. opening is null before submission; this does not measure a complete accounting conversion.",keywords:&["conversion"],capability:VIEW,input:target_schema,output,handler:Handler::Read(&Read::OpeningProgress),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.book.currencies",title:"Choose the book's currency",summary:"Currencies a book may open in (every current ISO 4217 currency, with its places and whether the tenant has used it; used ones first) matching a code or name, and the proposal: the company's base currency, else the tenant's. Opening the book marks its currency used. reference is unavailable when the list was never installed (a code is accepted unchecked).",keywords:&[],capability:VIEW,input:currencies_schema,output,handler:Handler::Read(&Read::Currencies),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.get",title:"Read a journal",summary:"Read lines, submission, decision, the approved entry identifier and whether it is the book's opening balances journal.",keywords:&[],capability:VIEW,input:journal_schema,output,handler:Handler::Read(&Read::Journal),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.account.list",title:"List accounts",summary:"Read the chart by code with a bounded cursor page: accounts, and account groups marked is_group.",keywords:&[],capability:VIEW,input:accounts_schema,output,handler:Handler::Read(&Read::Accounts),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_balance.get",title:"Read the chart with balances",summary:"Read every account with its own balance and every account group with the total of the accounts under it, as of a date and counted through a pinned entry number. A group never has a balance of its own.",keywords:&[],capability:VIEW,input:trial_balance_schema,output,handler:Handler::Read(&Read::ChartBalances),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_template.list",title:"List starter charts",summary:"Read the starter charts a company can start from, its optional sections, how many core groups and accounts each creates, how far an accountant has reviewed it, and whether it can be applied to this company now.",keywords:&[],capability:VIEW,input:target_schema,output,handler:Handler::Read(&Read::ChartTemplates),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_template.get",title:"Preview a starter chart",summary:"Read the exact groups and accounts a starter chart would create with the optional `sections` chosen, and any code the company's chart already uses.",keywords:&[],capability:VIEW,input:template_schema,output,handler:Handler::Read(&Read::ChartTemplate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.list",title:"List journals",summary:"Read journals by number, optionally by status, after a journal number.",keywords:&[],capability:VIEW,input:journals_schema,output,handler:Handler::Read(&Read::Journals),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.entry.get",title:"Read an entry",summary:"Read immutable lines, ownership and reversal identifiers, and request_number, the manual journal request it was posted from (null for other sources).",keywords:&[],capability:VIEW,input:entry_schema,output,handler:Handler::Read(&Read::Entry),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journals.list",title:"List postings and journals",summary:"Read a bounded, combined list of postings and unposted manual journals. number is the book's entry number, null until posted; request is the manual journal's request number, null for other sources; search matches either exactly.",keywords:&[],capability:VIEW,input:journal_list_schema,output,handler:Handler::Read(&Read::JournalList),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.entry.list",title:"List entries",summary:"Read entries by date and identifier with date, account and pinned-entry bounds.",keywords:&[],capability:VIEW,input:entries_schema,output,handler:Handler::Read(&Read::Entries),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.fiscal_year.list",title:"List fiscal years and periods",summary:"Read years in date order with periods, statuses and closing entry identifiers.",keywords:&[],capability:VIEW,input:years_schema,output,handler:Handler::Read(&Read::Years),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.trial_balance.get",title:"Read the trial balance",summary:"Read every account with a posting on or before a date, with debit and credit totals and its balance on one side, counted through a pinned entry number that reproduces the figures later.",keywords:&[],capability:VIEW,input:trial_balance_schema,output,handler:Handler::Read(&Read::TrialBalance),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.book.open",title:"Open the book",summary:"Permanently choose currency and minor units; enable controlled bookkeeping.",keywords:&[],capability:MANAGE_ACCOUNTS,input:book_schema,output,handler:Handler::SafeWrite(&Write::OpenBook),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.account.create",title:"Create an account",summary:"Create an account that takes postings, optionally inside an account group of the same type or designated for retained earnings.",keywords:&[],capability:MANAGE_ACCOUNTS,input:account_schema,output,handler:Handler::SafeWrite(&Write::CreateAccount),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.account.update",title:"Change an account",summary:"Change an account's name, group, active flag or retained-earnings designation; preserve code and type. Not for an account group.",keywords:&[],capability:MANAGE_ACCOUNTS,input:change_schema,output,handler:Handler::SafeWrite(&Write::UpdateAccount),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.account_group.create",title:"Create an account group",summary:"Create a group that takes no postings and shows the total of the accounts and groups under it; optionally inside another group of the same type.",keywords:&[],capability:MANAGE_ACCOUNTS,input:group_schema,output,handler:Handler::SafeWrite(&Write::CreateGroup),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.account_group.update",title:"Change an account group",summary:"Change a group's name or the group it sits in; preserve code and type. The account_id is the group's id.",keywords:&[],capability:MANAGE_ACCOUNTS,input:group_change_schema,output,handler:Handler::SafeWrite(&Write::UpdateGroup),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_template.save",title:"Save a reusable chart",summary:"Publish the current company's active chart as an immutable template shared within this tenant. Review exact rows. optional_group_codes makes each named group and its descendants an optional section; nested optional roots are refused. No balances or other settings are copied. Keys are unique and saved templates cannot be overwritten.",keywords:&[],capability:MANAGE_TEMPLATES,input:save_template_schema,output,handler:Handler::SafeWrite(&Write::SaveTemplate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_template.propose",title:"Propose a starter chart",summary:"Set this tenant's default starter template, or clear it with clear:true. New books offer it for a separate preview and apply; existing charts are unchanged.",keywords:&[],capability:MANAGE_TEMPLATES,input:propose_template_schema,output,handler:Handler::SafeWrite(&Write::ProposeTemplate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.chart_template.apply",title:"Apply a starter chart",summary:"Create a starter chart's core groups and accounts, and those of the optional `sections` chosen, in a company whose chart is empty. Optional `sections` choose the conditional parts, none by default; sections not chosen are not created. The review lists every row; a chart that already has accounts or groups is refused and never changed.",keywords:&[],capability:MANAGE_ACCOUNTS,input:template_schema,output,handler:Handler::SafeWrite(&Write::ApplyTemplate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.opening_balances.submit",title:"Submit opening balances",summary:"Map a source closing trial balance CSV to existing accounts, preview balanced opening amounts and retain source evidence in one approval journal. Identical source retries return the recorded journal; replaces_journal_id corrects your waiting opening journal; open items and history are excluded.",keywords:&["conversion", "opening balances", "CSV"],capability:SUBMIT_JOURNALS,input:opening_schema,output,handler:Handler::SafeWrite(&Write::Opening),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.submit",title:"Submit a journal",summary:"Keep balanced manual lines for approval without posting an entry.",keywords:&[],capability:SUBMIT_JOURNALS,input:submit_schema,output,handler:Handler::SafeWrite(&Write::Submit),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.approve",title:"Approve a journal",summary:"Record the next eligible approval through Workflow; the final required approval rechecks accounts and period, then posts atomically.",keywords:&[],capability:APPROVE_JOURNALS,input:journal_schema,output,handler:Handler::SafeWrite(&Write::Approve),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.withdraw",title:"Withdraw a journal",summary:"Take back your own submitted journal before approval; it stays on record and no entry posts.",keywords:&[],capability:SUBMIT_JOURNALS,input:withdraw_schema,output,handler:Handler::SafeWrite(&Write::Withdraw),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.replace",title:"Replace a journal",summary:"Withdraw your submitted journal and submit its correction, which needs its own approval.",keywords:&[],capability:SUBMIT_JOURNALS,input:replace_schema,output,handler:Handler::SafeWrite(&Write::Replace),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.journal.reject",title:"Reject a journal",summary:"Record a permanent rejection with a reason and no posting.",keywords:&[],capability:APPROVE_JOURNALS,input:reject_schema,output,handler:Handler::SafeWrite(&Write::Reject),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.entry.review_late",title:"Review a late recording",summary:"Record a permanent independent review with a reason. Refuses the poster, impersonation overlap and a second review.",keywords:&["late", "review"],capability:REVIEW_LATE_RECORDINGS,input:late_review_schema,output,handler:Handler::SafeWrite(&Write::ReviewLate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.entry.reverse",title:"Reverse a manual entry",summary:"Post the mirror once, never reverse an owned entry or a year-end closing entry here.",keywords:&[],capability:POST_ENTRIES,input:reverse_schema,output,handler:Handler::SafeWrite(&Write::Reverse),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.fiscal_year.open",title:"Open a fiscal year",summary:"Open the next year and calendar-month periods; the first start date is explicit.",keywords:&[],capability:MANAGE_PERIODS,input:open_year_schema,output,handler:Handler::SafeWrite(&Write::OpenYear),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.period.close",title:"Close a period",summary:"Close in date order; the last period closes only with its year.",keywords:&[],capability:MANAGE_PERIODS,input:period_schema,output,handler:Handler::SafeWrite(&Write::ClosePeriod),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.period.reopen",title:"Reopen a period",summary:"Reopen in reverse date order with a reason and an approval exception.",keywords:&[],capability:REOPEN_PERIODS,input:reopen_period_schema,output,handler:Handler::SafeWrite(&Write::ReopenPeriod),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.period.lock",title:"Lock a period permanently",summary:"Lock closed periods in date order; a locked period never reopens.",keywords:&[],capability:LOCK_PERIODS,input:period_schema,output,handler:Handler::SafeWrite(&Write::LockPeriod),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.fiscal_year.close",title:"Close a fiscal year",summary:"Post computed closing balances to retained earnings and close remaining open periods. Also requires entries.post.",keywords:&[],capability:MANAGE_PERIODS,input:close_year_schema,output,handler:Handler::SafeWrite(&Write::CloseYear),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.fiscal_year.reopen",title:"Reopen a fiscal year",summary:"Reverse the closing entry at year-end and reopen only the last period. Also requires entries.post.",keywords:&[],capability:REOPEN_PERIODS,input:reopen_year_schema,output,handler:Handler::SafeWrite(&Write::ReopenYear),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.ledger.fiscal_year.lock",title:"Lock a fiscal year permanently",summary:"Lock every remaining closed period of a closed year in date order, atomically.",keywords:&[],capability:LOCK_PERIODS,input:lock_year_schema,output,handler:Handler::SafeWrite(&Write::LockYear),channels:Channels::Every,step_up:&[]},
    ],
    guides:&[Guide {key:"finance.ledger",title:"Book, accounts, journals and closing",summary:"Reviewed ledger writes, approval controls, immutable entries and permanent period locks.",keywords:&[],capability:VIEW,body:include_str!("../guide.md")}],
}) }
