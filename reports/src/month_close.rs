//! What is left before a month closes, read through each module's facade
//! (Bilimbi has no counterpart: this is a Bilibi page and contract,
//! `finance.reports.month_close.get`).
//!
//! Reports is the one Finance module that depends on every other, so the
//! checklist lives here and no module learns of another. Each check asks its
//! owner one question about the month and answers with what the owner found
//! and the page that clears it. A check warns; it never blocks. What may
//! close, and in what order, stays the ledger's rule
//! (`finance.ledger.period.close`), and `close` here only repeats that rule
//! so a page and an agent can say why a month cannot close yet.
//!
//! The checklist reads one month, or a run of months from the earliest open
//! one through a later one ("close several months"): the dated checks then
//! cover the whole run and the as-of checks read its last day.
use crate::{Reports, ReportsError, Result};
use bilibi_finance_ledger::{
    dates, FiscalYear, JournalQuery, JournalStatus, LateRecordingQuery, LedgerError, Period,
    PeriodStatus, YearStatus,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The page of one month's checklist. Its links assume the company is the
/// session's current company, as every Finance list does.
pub const PATH: &str = "/finance/close-month";

/// How a check stands, as its owner can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// Something is left to do.
    Open,
    /// Nothing is left.
    Clear,
    /// The company does not use this area.
    NotUsed,
    /// The person may work in the company but may not read this area.
    NoAccess,
    /// The owner could not answer now.
    Unavailable,
}

/// One check: what its owner found for the month and where to act on it.
#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub name: String,
    pub state: CheckState,
    /// How many things are left, where the owner counts them.
    pub count: Option<u64>,
    /// The count in two or three words ("4 lines", "Not run", "Done").
    pub badge: String,
    pub found: String,
    /// The page that clears it; empty when there is nothing to do there.
    pub path: String,
    /// What the link to `path` is called ("See drafts").
    pub action: String,
}

/// A month of a fiscal year, named as pages name it.
#[derive(Clone, Debug, Serialize)]
pub struct Month {
    pub period_id: i64,
    /// "October 2026".
    pub name: String,
    /// "October".
    pub month: String,
    pub start_date: String,
    pub end_date: String,
    pub status: PeriodStatus,
}

/// Whether the ledger's closing rules let the month (or the run) close now,
/// and the write that closes it.
#[derive(Clone, Debug, Serialize)]
pub struct CloseStep {
    pub allowed: bool,
    /// Why not, in a sentence; empty when `allowed`.
    pub blocked: String,
    /// Where to go first; empty when `allowed` or there is nowhere to go.
    pub blocked_path: String,
    /// What the link to `blocked_path` is called.
    pub blocked_action: String,
    pub operation: &'static str,
    pub input: Value,
}

/// A company's month-end checklist.
#[derive(Clone, Debug, Serialize)]
pub struct MonthClose {
    pub company_id: i64,
    /// The fiscal year the first month is in: "January to December 2026".
    pub fiscal_year: String,
    /// The month the checklist is for, or the first of a run.
    pub period: Month,
    /// The last month of a run of several; absent for one month.
    pub through: Option<Month>,
    /// The months read, as one name: "October 2026", "October to December 2026".
    pub name: String,
    pub from_date: String,
    pub to_date: String,
    pub checks: Vec<Check>,
    /// How many checks have something left. Absent unless every check could
    /// be read, so a count is never shown for a company the person can only
    /// partly see.
    pub open_checks: Option<usize>,
    pub close: CloseStep,
    /// The open months after this one that may close together with it, in
    /// date order; empty unless this is the earliest open month.
    pub later: Vec<Month>,
    /// The month after the last one read, whatever its status: where a
    /// person goes once this one is closed.
    pub next: Option<Month>,
}

/// Which month to read. With neither `period_id` nor `month`, the earliest
/// open month.
#[derive(Clone, Debug, Default)]
pub struct MonthQuery {
    pub period_id: Option<i64>,
    /// A calendar month, `YYYY-MM`.
    pub month: Option<String>,
    /// Read a run of months, from the first through this one.
    pub through_period_id: Option<i64>,
}

/// Rows read per check before it stops counting and says "at least".
const READ_LIMIT: usize = 2000;
const PAGE: i64 = 500;

fn month(period: &Period) -> Month {
    Month {
        period_id: period.id,
        name: dates::months(period.start_date, period.end_date),
        month: dates::month_name(period.start_date).to_owned(),
        start_date: period.start_date.to_string(),
        end_date: period.end_date.to_string(),
        status: period.status,
    }
}

/// "1 draft", "3 drafts".
fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// "is" for one, "are" for more.
fn are(count: usize) -> &'static str {
    if count == 1 {
        "is"
    } else {
        "are"
    }
}

/// A count that stopped at the read limit says so.
fn at_least(count: usize, capped: bool) -> String {
    if capped {
        format!("At least {count}")
    } else {
        count.to_string()
    }
}

struct Named {
    id: String,
    name: String,
}

fn named(id: impl Into<String>, name: impl Into<String>) -> Named {
    Named {
        id: id.into(),
        name: name.into(),
    }
}

impl Named {
    fn open(
        self,
        count: usize,
        badge: impl Into<String>,
        found: impl Into<String>,
        path: impl Into<String>,
        action: &str,
    ) -> Check {
        Check {
            id: self.id,
            name: self.name,
            state: CheckState::Open,
            count: Some(count as u64),
            badge: badge.into(),
            found: found.into(),
            path: path.into(),
            action: action.to_owned(),
        }
    }
    fn clear(self, found: impl Into<String>) -> Check {
        Check {
            id: self.id,
            name: self.name,
            state: CheckState::Clear,
            count: Some(0),
            badge: "Done".into(),
            found: found.into(),
            path: String::new(),
            action: String::new(),
        }
    }
    fn not_used(self, found: impl Into<String>) -> Check {
        Check {
            id: self.id,
            name: self.name,
            state: CheckState::NotUsed,
            count: None,
            badge: "Not used".into(),
            found: found.into(),
            path: String::new(),
            action: String::new(),
        }
    }
    /// A read its owner refused or could not answer.
    fn failed(self, denied: bool) -> Check {
        Check {
            id: self.id,
            name: self.name,
            state: if denied {
                CheckState::NoAccess
            } else {
                CheckState::Unavailable
            },
            count: None,
            badge: if denied {
                "No access".into()
            } else {
                "Could not be read".into()
            },
            found: if denied {
                "You do not have permission to see this.".into()
            } else {
                "This could not be read. Reload the page to try again.".into()
            },
            path: String::new(),
            action: String::new(),
        }
    }
}

/// The dates the checks read and the words for them.
struct Span {
    from: Date,
    to: Date,
    /// "October 2026" or "October to December 2026".
    name: String,
}

impl Span {
    fn holds(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }
    fn holds_text(&self, date: Option<&str>) -> bool {
        date.and_then(|text| text.get(..10))
            .and_then(|text| text.parse::<Date>().ok())
            .is_some_and(|date| self.holds(date))
    }
}

impl Reports {
    /// The month-end checklist of `company_id` (`finance.ledger.view`, and
    /// the view capability of each area: a check the person may not read is
    /// `no_access`, apart from one with nothing left).
    pub async fn month_close(
        &self,
        scope: &Scope,
        company_id: i64,
        query: &MonthQuery,
    ) -> Result<MonthClose> {
        let years = self.ledger.list_fiscal_years(scope, company_id).await?;
        let periods: Vec<(&FiscalYear, &Period)> = years
            .iter()
            .flat_map(|year| year.periods.iter().map(move |period| (year, period)))
            .collect();
        let earliest_open = periods
            .iter()
            .position(|(_, period)| period.status == PeriodStatus::Open);
        let first = match (query.period_id, query.month.as_deref()) {
            (Some(id), _) => periods.iter().position(|(_, period)| period.id == id),
            (None, Some(text)) => {
                let start = format!("{text}-01")
                    .parse::<Date>()
                    .map_err(|_| ReportsError::InvalidOption)?;
                periods
                    .iter()
                    .position(|(_, period)| period.start_date == start)
            }
            (None, None) => earliest_open.or(periods.len().checked_sub(1)),
        }
        .ok_or(ReportsError::MonthNotFound)?;
        let last = match query.through_period_id {
            None => first,
            Some(id) => periods
                .iter()
                .position(|(_, period)| period.id == id)
                .filter(|last| *last >= first)
                .ok_or(ReportsError::MonthNotFound)?,
        };
        let (year, period) = periods[first];
        let through = periods[last].1;
        let span = Span {
            from: period.start_date,
            to: through.end_date,
            name: dates::months(period.start_date, through.end_date),
        };
        // A year's last month closes only with the year.
        let with_year = |(year, period): &(&FiscalYear, &Period)| {
            year.end_date == period.end_date && year.status != YearStatus::Closed
        };
        let years_path = "/finance/ledger/fiscal-years".to_owned();
        let close_input = json!({
            "company_id": company_id,
            "period_id": through.id,
            "with_earlier": last > first,
        });
        let blocked = |blocked: String, path: String, action: &str| CloseStep {
            allowed: false,
            blocked,
            blocked_path: path,
            blocked_action: action.to_owned(),
            operation: "finance.ledger.period.close",
            input: close_input.clone(),
        };
        let close = if period.status != PeriodStatus::Open {
            blocked(
                format!(
                    "{} is already {}.",
                    span_name(period),
                    if period.status == PeriodStatus::Locked {
                        "locked"
                    } else {
                        "closed"
                    }
                ),
                String::new(),
                "",
            )
        } else if let Some(earlier) = earliest_open.filter(|index| *index < first) {
            let earlier = periods[earlier].1;
            blocked(
                format!("Close {} first.", span_name(earlier)),
                format!("{PATH}?period={}", earlier.id),
                &format!("Go to {}", dates::month_name(earlier.start_date)),
            )
        } else if let Some((_, ends)) = periods[first..=last].iter().find(|pair| with_year(pair)) {
            blocked(
                format!(
                    "{} is the last month of the fiscal year, so it closes with the year.",
                    span_name(ends)
                ),
                years_path.clone(),
                "Go to Fiscal years",
            )
        } else {
            CloseStep {
                allowed: true,
                blocked: String::new(),
                blocked_path: String::new(),
                blocked_action: String::new(),
                operation: "finance.ledger.period.close",
                input: close_input.clone(),
            }
        };
        // The months that may close with this one: the open ones after it,
        // up to the month before a year's last.
        let later = if earliest_open == Some(first) {
            periods[first + 1..]
                .iter()
                .take_while(|pair| pair.1.status == PeriodStatus::Open && !with_year(pair))
                .map(|(_, period)| month(period))
                .collect()
        } else {
            Vec::new()
        };

        let mut checks = self.bank(scope, company_id, &span).await;
        checks.push(self.draft_invoices(scope, company_id, &span).await);
        checks.extend(self.bills(scope, company_id, &span).await);
        checks.push(self.payments(scope, company_id, &span).await);
        checks.push(self.journals(scope, company_id, &span).await);
        checks.push(self.late_entries(scope, company_id, &span).await);
        checks.push(self.depreciation(scope, company_id, &span).await);
        checks.push(self.payroll(scope, company_id, &span).await);
        checks.push(self.sst(scope, company_id, &span).await);
        checks.push(self.einvoices(scope, company_id, through).await);
        checks.push(self.trial_balance(scope, company_id, &span).await);

        // A count is shown only when it is true: every check was read.
        let readable = checks.iter().all(|check| {
            !matches!(
                check.state,
                CheckState::NoAccess | CheckState::Unavailable
            )
        });
        let open_checks = readable.then(|| {
            checks
                .iter()
                .filter(|check| check.state == CheckState::Open)
                .count()
        });
        Ok(MonthClose {
            company_id,
            fiscal_year: dates::months(year.start_date, year.end_date),
            period: month(period),
            through: (last > first).then(|| month(through)),
            name: span.name.clone(),
            from_date: span.from.to_string(),
            to_date: span.to.to_string(),
            checks,
            open_checks,
            close,
            later,
            next: periods.get(last + 1).map(|(_, period)| month(period)),
        })
    }

    /// One check per active bank account: the statement lines up to the last
    /// day that are not matched, and whether the account is reconciled to it.
    async fn bank(&self, scope: &Scope, company: i64, span: &Span) -> Vec<Check> {
        use bilibi_finance_reconciliation::{ReconciliationError as E, ReconciliationStatus};
        let s = &self.sources.reconciliation;
        let accounts = match s.list_bank_accounts(scope, company).await {
            Ok(accounts) => accounts,
            Err(error) => {
                return vec![named("bank", "Bank accounts").failed(matches!(error, E::Unauthorized))]
            }
        };
        let active: Vec<_> = accounts.into_iter().filter(|a| a.active).collect();
        if active.is_empty() {
            return vec![named("bank", "Bank accounts").not_used("No bank account is set up.")];
        }
        let day = dates::display(span.to);
        let mut checks = Vec::new();
        for account in active {
            let check = named(
                format!("bank-{}", account.id),
                format!("Bank: {}", account.name),
            );
            let read = async {
                let unmatched = s
                    .unmatched_items(scope, company, account.id, Some(span.to))
                    .await?;
                let reconciled = s
                    .list_reconciliations(scope, company, account.id)
                    .await?
                    .into_iter()
                    .filter(|r| r.status == ReconciliationStatus::Finalised)
                    .map(|r| r.end_date)
                    .max();
                Ok::<_, E>((unmatched.lines.len(), reconciled))
            };
            checks.push(match read.await {
                Err(error) => check.failed(matches!(error, E::Unauthorized)),
                Ok((_, Some(reconciled))) if reconciled >= span.to => check.clear(format!(
                    "Reconciled to {}.",
                    dates::display(reconciled)
                )),
                Ok((lines, reconciled)) => {
                    let last = match reconciled {
                        Some(date) => format!("Last reconciled to {}.", dates::display(date)),
                        None => "Never reconciled.".to_owned(),
                    };
                    if lines > 0 {
                        check.open(
                            lines,
                            counted(lines, "line", "lines"),
                            format!(
                                "{} up to {day} {} not matched. {last}",
                                counted(lines, "statement line", "statement lines"),
                                are(lines)
                            ),
                            format!("/finance/reconciliation/accounts/{}/matching", account.id),
                            "Match lines",
                        )
                    } else {
                        check.open(
                            1,
                            "Not reconciled",
                            format!("Not reconciled to {day} yet. {last}"),
                            format!("/finance/reconciliation/accounts/{}", account.id),
                            "Reconcile",
                        )
                    }
                }
            });
        }
        checks
    }

    /// Draft invoices and credit notes dated in the months. A draft with no
    /// date takes the day it is issued, so it is not counted for a month.
    async fn draft_invoices(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        use bilibi_finance_receivables::{DocumentQuery, Page, ReceivableError as E};
        let check = named("draft-invoices", "Draft invoices");
        let (mut invoices, mut notes, mut read, mut after) = (0, 0, 0, None);
        loop {
            let query = DocumentQuery {
                status: Some("draft".into()),
                page: Page {
                    limit: Some(PAGE),
                    after,
                },
                ..DocumentQuery::default()
            };
            let page = match self
                .sources
                .receivables
                .list_documents(scope, company, &query)
                .await
            {
                Ok(page) => page,
                Err(error) => return check.failed(matches!(error, E::Unauthorized)),
            };
            for document in &page {
                if span.holds_text(document.issue_date.as_deref()) {
                    match document.kind.as_str() {
                        "credit_note" => notes += 1,
                        _ => invoices += 1,
                    }
                }
            }
            read += page.len();
            after = page.last().map(|document| document.id);
            if (page.len() as i64) < PAGE || read >= READ_LIMIT {
                break;
            }
        }
        let total = invoices + notes;
        if total == 0 {
            return check.clear(format!("No draft is dated in {}.", span.name));
        }
        let mut parts = Vec::new();
        if invoices > 0 {
            parts.push(counted(invoices, "invoice", "invoices"));
        }
        if notes > 0 {
            parts.push(counted(notes, "credit note", "credit notes"));
        }
        check.open(
            total,
            counted(total, "draft", "drafts"),
            format!(
                "{} dated in {} {} still {}.",
                parts.join(" and "),
                span.name,
                are(total),
                if total == 1 { "a draft" } else { "drafts" }
            ),
            "/finance/receivables/invoices?status=draft",
            "See drafts",
        )
    }

    /// Bills dated in the months that are still drafts, and bills that are
    /// approved or on hold but not posted.
    async fn bills(&self, scope: &Scope, company: i64, span: &Span) -> Vec<Check> {
        use bilibi_finance_payables::{DocumentQuery, PayablesError as E};
        let count = |status: &'static str| async move {
            let (mut total, mut after) = (0usize, None);
            loop {
                let query = DocumentQuery {
                    status: Some(status.into()),
                    from: Some(span.from),
                    to: Some(span.to),
                    after,
                    limit: PAGE,
                    ..DocumentQuery::default()
                };
                let page = self
                    .sources
                    .payables
                    .list_documents(scope, company, &query)
                    .await?;
                total += page.len();
                after = page.last().map(|d| (d.document_date, d.id));
                if (page.len() as i64) < PAGE || total >= READ_LIMIT {
                    return Ok::<_, E>(total);
                }
            }
        };
        let drafts = named("draft-bills", "Draft bills");
        let unposted = named("unposted-bills", "Bills not posted");
        let read = async { Ok::<_, E>((count("draft").await?, count("approved").await?, count("held").await?)) };
        let (draft, approved, held) = match read.await {
            Ok(counts) => counts,
            Err(error) => {
                let denied = matches!(error, E::Unauthorized);
                return vec![drafts.failed(denied), unposted.failed(denied)];
            }
        };
        let waiting = approved + held;
        vec![
            if draft == 0 {
                drafts.clear(format!("No draft bill is dated in {}.", span.name))
            } else {
                drafts.open(
                    draft,
                    counted(draft, "draft", "drafts"),
                    format!(
                        "{} dated in {} {} still {}.",
                        counted(draft, "bill", "bills"),
                        span.name,
                        are(draft),
                        if draft == 1 { "a draft" } else { "drafts" }
                    ),
                    "/finance/payables/bills?status=draft",
                    "See drafts",
                )
            },
            if waiting == 0 {
                unposted.clear(format!(
                    "Every approved bill dated in {} is posted.",
                    span.name
                ))
            } else {
                unposted.open(
                    waiting,
                    counted(waiting, "bill", "bills"),
                    format!(
                        "{} dated in {} {} approved but not posted{}.",
                        counted(waiting, "bill", "bills"),
                        span.name,
                        are(waiting),
                        if held == 0 {
                            String::new()
                        } else {
                            format!(" ({held} on hold)")
                        }
                    ),
                    if approved == 0 {
                        "/finance/payables/bills?status=held"
                    } else {
                        "/finance/payables/bills?status=approved"
                    },
                    "See bills",
                )
            },
        ]
    }

    /// Payment requests dated in the months that nobody has approved yet.
    async fn payments(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        use bilibi_finance_payables::{PayablesError as E, PaymentRequestQuery};
        let check = named("payments", "Payments waiting for approval");
        let (mut waiting, mut read, mut after) = (0, 0, None);
        loop {
            let query = PaymentRequestQuery {
                status: Some("submitted".into()),
                after,
                limit: Some(PAGE),
                newest_first: false,
            };
            let page = match self
                .sources
                .payables
                .list_payment_requests(scope, company, &query)
                .await
            {
                Ok(page) => page,
                Err(error) => return check.failed(matches!(error, E::Unauthorized)),
            };
            waiting += page.iter().filter(|r| span.holds(r.payment_date)).count();
            read += page.len();
            after = page.last().map(|r| r.id);
            if (page.len() as i64) < PAGE || read >= READ_LIMIT {
                break;
            }
        }
        if waiting == 0 {
            return check.clear(format!(
                "No payment dated in {} is waiting for approval.",
                span.name
            ));
        }
        check.open(
            waiting,
            counted(waiting, "payment", "payments"),
            format!(
                "{} dated in {} {} waiting for approval.",
                counted(waiting, "payment", "payments"),
                span.name,
                are(waiting)
            ),
            "/finance/payables/payments?status=submitted",
            "See payments",
        )
    }

    /// Manual journals dated in the months that are waiting for approval.
    async fn journals(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        let check = named("journals", "Journals waiting for approval");
        let (mut waiting, mut read, mut after) = (0, 0, None);
        loop {
            let query = JournalQuery {
                status: Some(JournalStatus::Submitted),
                limit: Some(PAGE as u32),
                after,
            };
            let page = match self.ledger.list_journals(scope, company, &query).await {
                Ok(page) => page,
                Err(error) => return check.failed(matches!(error, LedgerError::Unauthorized)),
            };
            waiting += page.iter().filter(|j| span.holds(j.entry_date)).count();
            read += page.len();
            after = page.last().map(|j| j.journal_number);
            if (page.len() as i64) < PAGE || read >= READ_LIMIT {
                break;
            }
        }
        if waiting == 0 {
            return check.clear(format!(
                "No journal dated in {} is waiting for approval.",
                span.name
            ));
        }
        check.open(
            waiting,
            counted(waiting, "journal", "journals"),
            format!(
                "{} dated in {} {} waiting for approval. A journal posts only into an open month.",
                counted(waiting, "journal", "journals"),
                span.name,
                are(waiting)
            ),
            format!(
                "/finance/ledger/journals?status=submitted&from={}&to={}",
                span.from, span.to
            ),
            "See journals",
        )
    }

    /// Entries dated in the months that were entered late and that nobody
    /// has reviewed.
    async fn late_entries(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        let check = named("late-entries", "Late entries to review");
        let (mut waiting, mut read, mut after) = (0, 0, None);
        loop {
            let query = LateRecordingQuery {
                status: None,
                reviewed: Some(false),
                limit: Some(PAGE as u32),
                after,
            };
            let page = match self
                .ledger
                .list_late_recordings(scope, company, &query)
                .await
            {
                Ok(page) => page,
                Err(error) => return check.failed(matches!(error, LedgerError::Unauthorized)),
            };
            waiting += page.iter().filter(|e| span.holds(e.entry_date)).count();
            read += page.len();
            after = page.last().map(|e| e.entry_number);
            if (page.len() as i64) < PAGE || read >= READ_LIMIT {
                break;
            }
        }
        if waiting == 0 {
            return check.clear(format!(
                "No late entry dated in {} is waiting for review.",
                span.name
            ));
        }
        let capped = read >= READ_LIMIT;
        check.open(
            waiting,
            counted(waiting, "entry", "entries"),
            format!(
                "{} {} dated in {} {} entered late and not reviewed yet.",
                at_least(waiting, capped),
                if waiting == 1 { "entry" } else { "entries" },
                span.name,
                if waiting == 1 { "was" } else { "were" }
            ),
            format!(
                "/finance/ledger/journals?review=pending&from={}&to={}",
                span.from, span.to
            ),
            "Review",
        )
    }

    /// Depreciation the asset register schedules up to the last day that is
    /// not posted yet: what a depreciation run for that month would charge.
    async fn depreciation(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        use bilibi_finance_fixed_assets::{FixedAssetsError as E, ReportPins};
        let check = named("depreciation", "Depreciation");
        let report = match self
            .sources
            .fixed_assets
            .register_report(scope, company, span.to, &ReportPins::default())
            .await
        {
            Ok(report) => report,
            Err(error) => return check.failed(matches!(error, E::Unauthorized)),
        };
        if report.rows.is_empty() {
            return check.not_used("No asset is registered.");
        }
        let due: Vec<_> = report
            .rows
            .iter()
            .filter(|row| {
                !row.disposed && !row.voided && row.scheduled_depreciation > row.accumulated
            })
            .map(|row| row.scheduled_depreciation - row.accumulated)
            .collect();
        if due.is_empty() {
            return check.clear(format!(
                "Depreciation is posted up to {}.",
                dates::display(span.to)
            ));
        }
        let assets = due.len();
        let total: bilibi_finance_ledger::Money = due.into_iter().sum();
        check.open(
            assets,
            "Not run",
            format!(
                "Depreciation up to {} is {} for {}, and it is not posted.",
                dates::display(span.to),
                total.display(report.minor_units),
                counted(assets, "asset", "assets")
            ),
            "/finance/fixed-assets/runs",
            "Run depreciation",
        )
    }

    /// Payroll runs paid in the months that are not in the books.
    async fn payroll(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        use bilibi_finance_payroll::PayrollError as E;
        let check = named("payroll", "Payroll");
        let runs = match self.sources.payroll.list_payroll_runs(scope, company).await {
            Ok(runs) => runs,
            Err(E::PeopleUnavailable) => {
                return check.not_used("Payroll is not connected to Finance.")
            }
            Err(error) => return check.failed(matches!(error, E::Unauthorized)),
        };
        let paid: Vec<&Value> = runs
            .iter()
            .filter(|run| span.holds_text(run["pay_date"].as_str()))
            .collect();
        let unposted = paid.iter().filter(|run| run["posting"].is_null()).count();
        if unposted == 0 {
            return check.clear(if paid.is_empty() {
                format!("No payroll run was paid in {}.", span.name)
            } else {
                format!(
                    "{} paid in {} {} posted.",
                    counted(paid.len(), "payroll run", "payroll runs"),
                    span.name,
                    are(paid.len())
                )
            });
        }
        check.open(
            unposted,
            counted(unposted, "run", "runs"),
            format!(
                "{} paid in {} {} not posted.",
                counted(unposted, "payroll run", "payroll runs"),
                span.name,
                are(unposted)
            ),
            "/finance/payroll",
            "Post payroll",
        )
    }

    /// SST returns for the taxable periods that end by the last day: one not
    /// prepared yet, or one still open.
    async fn sst(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        use bilibi_finance_sst::{ReturnStatus, SstError as E};
        let check = named("sst", "SST");
        let s = &self.sources.sst;
        let read = async {
            let periods = s
                .taxable_periods(scope, company, Some(span.to), Some(2))
                .await?;
            let (mut returns, mut after) = (Vec::new(), None);
            loop {
                let page = s
                    .list_returns(scope, company, None, Some(PAGE), after)
                    .await?;
                after = page.last().map(|r| r.id);
                let done = (page.len() as i64) < PAGE;
                returns.extend(page);
                if done || returns.len() >= READ_LIMIT {
                    return Ok::<_, E>((periods, returns));
                }
            }
        };
        let (periods, returns) = match read.await {
            Ok(read) => read,
            Err(error) => return check.failed(matches!(error, E::Unauthorized)),
        };
        if periods.is_empty() && returns.is_empty() {
            return check.not_used("The company is not registered for SST.");
        }
        let due = |date: Date| format!("Due {}.", dates::display(date));
        // An open return for a period that has ended is the nearest thing to do.
        if let Some(open) = returns
            .iter()
            .filter(|r| r.status == ReturnStatus::Open && r.period_end <= span.to)
            .min_by_key(|r| r.due_date)
        {
            return check.open(
                1,
                "Open",
                format!(
                    "The return for {} is open. {}",
                    dates::months(open.period_start, open.period_end),
                    due(open.due_date)
                ),
                format!("/finance/sst/returns/{}", open.id),
                "Open return",
            );
        }
        // A taxable period that ends in these months needs a return.
        let ending = periods
            .iter()
            .find(|p| span.holds(p.period_end))
            .filter(|p| {
                !returns.iter().any(|r| {
                    r.period_start == p.period_start && r.status == ReturnStatus::Finalised
                })
            });
        match ending {
            Some(period) => check.open(
                1,
                "Not prepared",
                format!(
                    "No return is prepared for {}. {}",
                    dates::months(period.period_start, period.period_end),
                    due(period.due_date)
                ),
                "/finance/sst",
                "Prepare return",
            ),
            None => check.clear(format!("No SST return is waiting for {}.", span.name)),
        }
    }

    /// E-invoices the books do not agree with, and receipts of the last
    /// month that still wait for its consolidated e-invoice.
    async fn einvoices(&self, scope: &Scope, company: i64, month: &Period) -> Check {
        use bilibi_finance_einvoice::{DocumentQuery, EinvoiceError as E};
        let check = named("einvoices", "E-invoices");
        let s = &self.sources.einvoice;
        let read = async {
            let any = s
                .list_documents(
                    scope,
                    company,
                    &DocumentQuery {
                        page_size: 25,
                        ..DocumentQuery::default()
                    },
                )
                .await?
                .total_entries;
            let books = s.reconciliation(scope, company).await?;
            Ok::<_, E>((any, books.unposted.len(), books.unreversed.len()))
        };
        let (any, unposted, unreversed) = match read.await {
            Ok(read) => read,
            Err(error) => return check.failed(matches!(error, E::Unauthorized)),
        };
        if any == 0 {
            return check.not_used("No e-invoice has been issued.");
        }
        let differences = unposted + unreversed;
        if differences > 0 {
            let mut parts = Vec::new();
            if unposted > 0 {
                parts.push(format!(
                    "{} {} valid but not posted.",
                    counted(unposted, "e-invoice", "e-invoices"),
                    are(unposted)
                ));
            }
            if unreversed > 0 {
                parts.push(format!(
                    "{} {} cancelled but still in the books.",
                    counted(unreversed, "e-invoice", "e-invoices"),
                    are(unreversed)
                ));
            }
            return check.open(
                differences,
                counted(differences, "e-invoice", "e-invoices"),
                parts.join(" "),
                "/finance/einvoice/reconciliation",
                "See e-invoices",
            );
        }
        // The consolidated e-invoice of a month: what still waits for it.
        let waiting = s
            .consolidation_candidates(scope, company, month.start_date, jiff::Timestamp::now())
            .await
            .map_or(0, |candidates| candidates.documents.len());
        if waiting > 0 {
            return check.open(
                waiting,
                counted(waiting, "document", "documents"),
                format!(
                    "{} of {} {} waiting for the consolidated e-invoice.",
                    counted(waiting, "document", "documents"),
                    span_name(month),
                    are(waiting)
                ),
                "/finance/einvoice/consolidation",
                "Consolidate",
            );
        }
        check.clear("Every e-invoice agrees with the books.")
    }

    /// The trial balance on the last day, to read before closing.
    async fn trial_balance(&self, scope: &Scope, company: i64, span: &Span) -> Check {
        let check = named("trial-balance", "Trial balance");
        let path = format!("/finance/ledger/trial-balance?as_of={}", span.to);
        match self.ledger.trial_balance(scope, company, span.to, None).await {
            Err(error) => check.failed(matches!(error, LedgerError::Unauthorized)),
            Ok(balance) if balance.total_debit == balance.total_credit => Check {
                badge: "Balanced".into(),
                path,
                action: "Open".into(),
                ..check.clear(format!(
                    "Debits equal credits on {}: {}.",
                    dates::display(span.to),
                    balance.total_debit.display(balance.minor_units)
                ))
            },
            Ok(_) => check.open(
                1,
                "Not balanced",
                format!("Debits and credits differ on {}.", dates::display(span.to)),
                path,
                "Open",
            ),
        }
    }
}

/// "October 2026".
fn span_name(period: &Period) -> String {
    dates::months(period.start_date, period.end_date)
}
