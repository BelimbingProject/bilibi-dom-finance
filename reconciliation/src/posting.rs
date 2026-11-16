//! Posts a statement line the books do not have, a bank charge or interest,
//! through the ledger facade and matches it to the entry in the same
//! transaction, and reverses such a posting (Bilimbi
//! `Reconciliation.Posting`). The facade authorizes the call and holds the
//! company lock; the ledger authorizes the posting itself on that
//! transaction, and its refusal rolls the whole write back, so nothing is
//! recorded then.
use crate::{
    matching, BankAccount, Match, PostedStatementLine, ReconciliationError, Result,
    ReversedStatementLine, StatementLine, MODULE,
};
use bilibi_database::MutationTransaction;
use bilibi_finance_ledger::{
    EntryOwner, Ledger, LedgerError, NewEntry, NewLine, PostOptions, Reversal,
};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, tz::TimeZone, Timestamp};

/// The kinds of statement line this module posts.
pub const KINDS: [&str; 2] = ["charge", "interest"];
const POSTING_NOTE: &str = "Posted from statement line as ";
const TEXT_LIMIT: usize = 500;

/// The owner identity of the entry a statement line posted.
pub(crate) fn owner(line: &StatementLine) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("statement_line:{}", line.id),
    }
}

/// Posts `line`, an unmatched statement line of the bank account, as `kind`
/// and matches the line to the new entry, on `tx`.
pub(crate) async fn post(
    tx: &mut MutationTransaction<'_>,
    ledger: &Ledger,
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    line: &StatementLine,
    kind: &str,
    description: Option<&str>,
) -> Result<PostedStatementLine> {
    let counter_account_id = match kind {
        "charge" => account
            .charges_account_id
            .ok_or(ReconciliationError::NoChargesAccount)?,
        "interest" => account
            .interest_account_id
            .ok_or(ReconciliationError::NoInterestAccount)?,
        _ => return Err(ReconciliationError::InvalidKind),
    };
    // A charge takes money out; interest brings it in.
    match kind {
        "charge" if line.amount.is_positive() => return Err(ReconciliationError::NotACharge),
        "interest" if !line.amount.is_positive() => return Err(ReconciliationError::NotInterest),
        _ => {}
    }
    let tenant = scope.tenant();
    let (lines_taken, _) = matching::taken(tx.sql(), tenant, company_id, account.id, None).await?;
    if lines_taken.contains(&line.id) {
        return Err(ReconciliationError::StatementLineMatched);
    }
    let description = description
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(&line.description)
        .to_owned();
    let absolute = line.amount.abs();
    // Money out: debit the counter account, credit the bank; money in: the
    // reverse.
    let lines = if line.amount.is_positive() {
        vec![
            NewLine::debit(account.ledger_account_id, absolute),
            NewLine::credit(counter_account_id, absolute),
        ]
    } else {
        vec![
            NewLine::debit(counter_account_id, absolute),
            NewLine::credit(account.ledger_account_id, absolute),
        ]
    };
    let new = NewEntry {
        entry_date: line.line_date,
        description,
        reference: line.reference.clone(),
        lines,
    };
    let options = PostOptions {
        owner: Some(owner(line)),
        ..PostOptions::default()
    };
    // The entry names the statement line as its owner, so the ledger holds
    // one entry per line: a line unmatched since it posted is matched to that
    // entry again, never posted again.
    let entry = match ledger
        .post_entry_in(tx, scope, company_id, &new, &options)
        .await
    {
        Err(LedgerError::Invalid(errors))
            if errors.iter().any(|error| error.field == "owner_record") =>
        {
            return Err(ReconciliationError::StatementLinePosted);
        }
        other => other?,
    };
    let item = matching::ledger_item(&entry, account.ledger_account_id).ok_or(
        ReconciliationError::Failed(bilibi_database::Error::Internal),
    )?;
    let match_id = matching::insert(
        tx,
        tenant,
        company_id,
        crate::people(scope),
        account.id,
        std::slice::from_ref(line),
        &[item],
        Some(format!("{POSTING_NOTE}{kind}")),
    )
    .await?;
    let posted = matching::find(tx.sql(), tenant, company_id, match_id)
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    Ok(PostedStatementLine {
        entry,
        posted_match: posted,
    })
}

/// Reverses the entry `line` posted, on `tx`: `line` must stand matched,
/// alone, to that entry. The match is unmatched with `reason` (so never once
/// a finalised reconciliation counted it) and the entry reversed as its
/// owner, dated `entry_date` (today, UTC, when absent).
pub(crate) async fn reverse(
    tx: &mut MutationTransaction<'_>,
    ledger: &Ledger,
    scope: &Scope,
    company_id: i64,
    account: &BankAccount,
    line: &StatementLine,
    reason: Option<&str>,
    entry_date: Option<Date>,
) -> Result<ReversedStatementLine> {
    let reason = match reason.map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() <= TEXT_LIMIT => text.to_owned(),
        _ => return Err(ReconciliationError::InvalidReason),
    };
    let tenant = scope.tenant();
    let standing = matching::standing_for_line(tx.sql(), tenant, company_id, account.id, line.id)
        .await?
        .ok_or(ReconciliationError::StatementLineNotMatched)?;
    let entry = posted_entry(ledger, tx, scope, company_id, line, &standing).await?;
    matching::check_unmatch(tx.sql(), tenant, company_id, &standing).await?;
    matching::record_unmatch(
        tx,
        tenant,
        company_id,
        crate::people(scope),
        standing.id,
        reason,
    )
    .await?;
    let reversal = ledger
        .reverse_owned_entry(
            tx,
            scope,
            company_id,
            entry.id,
            &owner(line),
            &Reversal {
                entry_date: entry_date
                    .unwrap_or_else(|| Timestamp::now().to_zoned(TimeZone::UTC).date()),
                description: format!("Reversal of {}", entry.description)
                    .chars()
                    .take(TEXT_LIMIT)
                    .collect(),
                reference: entry.reference.clone(),
            },
        )
        .await?;
    let unmatched = matching::find(tx.sql(), tenant, company_id, standing.id)
        .await?
        .ok_or(bilibi_database::Error::Internal)?;
    Ok(ReversedStatementLine {
        reversal,
        unmatched_match: unmatched,
    })
}

/// The entry the line posted: the match holds the line alone and one entry,
/// owned by the line.
async fn posted_entry(
    ledger: &Ledger,
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    line: &StatementLine,
    standing: &Match,
) -> Result<bilibi_finance_ledger::Entry> {
    let ([_], [only]) = (
        standing.statement_line_ids.as_slice(),
        standing.entries.as_slice(),
    ) else {
        return Err(ReconciliationError::StatementLineNotPosted);
    };
    let entry = ledger
        .get_entry_on(tx.sql(), scope, company_id, only.entry_id)
        .await?;
    if entry.owner.as_ref() == Some(&owner(line)) {
        Ok(entry)
    } else {
        Err(ReconciliationError::StatementLineNotPosted)
    }
}
