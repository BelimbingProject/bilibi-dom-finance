//! What a company has set up in Finance and what remains, read through each
//! module's facade (Bilimbi has no counterpart: this is a Bilibi page and
//! contract, `finance.reports.setup.get`).
//!
//! Reports is the one Finance module that depends on every other, so the
//! overview lives here and no module learns of another. Each area reports only
//! what its owner can determine: a module's own setup record exists or it does
//! not. An area with nothing to measure (accountant access)
//! has the state `not_tracked`; it is a link, never a completion claim.
//!
//! The overview works for one company named by the caller, which is the
//! current company on the page and any reachable company for an agent. Every
//! facade call proves its own capability for that company, so an area the
//! person may not read is `no_access`, apart from one that is `not_set_up`.
use crate::{Reports, Result};
use bilibi_finance_ledger::{AccountType, LedgerError, PeriodStatus};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};

/// How far an area is, as its owner can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    /// The owner holds the records the area needs.
    SetUp,
    /// The owner holds none yet.
    NotSetUp,
    /// Some of what the area needs is there and some is missing.
    Incomplete,
    /// The person may work in the company but may not read this area.
    NoAccess,
    /// The owner could not answer now.
    Unavailable,
    /// There is nothing to measure; the area is only a way to the page.
    NotTracked,
}

/// One area of setup, with where to do it.
#[derive(Clone, Debug, Serialize)]
pub struct SetupArea {
    pub id: &'static str,
    pub name: &'static str,
    pub state: SetupState,
    /// A company can work without it (a module it does not use, a conversion
    /// it does not need).
    pub optional: bool,
    pub summary: String,
    pub path: &'static str,
}

/// A company's setup, area by area, in the order work usually starts.
#[derive(Clone, Debug, Serialize)]
pub struct SetupOverview {
    pub company_id: i64,
    pub areas: Vec<SetupArea>,
    /// The required areas that are set up, and how many there are. Both are
    /// absent unless every required area could be read, so a count is never
    /// shown for a company the person can only partly see.
    pub required_set_up: Option<usize>,
    pub required_total: Option<usize>,
}

fn area(
    id: &'static str,
    name: &'static str,
    optional: bool,
    path: &'static str,
    state: SetupState,
    summary: impl Into<String>,
) -> SetupArea {
    SetupArea {
        id,
        name,
        state,
        optional,
        summary: summary.into(),
        path,
    }
}

/// Words a person reads for a read the area's owner refused.
const NO_ACCESS: &str = "You can work in this company but may not read this setup.";
const UNAVAILABLE: &str = "This setup could not be read. Try again.";

/// Sorts a facade answer into an area. `missing` says which error means the
/// owner holds nothing yet, `denied` which means the reader may not know.
fn read<T, E>(
    result: std::result::Result<T, E>,
    denied: impl Fn(&E) -> bool,
    missing: impl Fn(&E) -> bool,
) -> std::result::Result<Option<T>, SetupState> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if denied(&error) => Err(SetupState::NoAccess),
        Err(error) if missing(&error) => Ok(None),
        Err(_) => Err(SetupState::Unavailable),
    }
}

fn failed(state: SetupState) -> String {
    match state {
        SetupState::NoAccess => NO_ACCESS.to_owned(),
        _ => UNAVAILABLE.to_owned(),
    }
}

impl Reports {
    /// The setup of `company_id` (`finance.ledger.view` and the view or
    /// settings capability of each area).
    pub async fn setup_overview(&self, scope: &Scope, company_id: i64) -> Result<SetupOverview> {
        let s = &self.sources;
        let mut areas = Vec::new();

        // The book decides what the next three can say.
        let book = read(
            self.ledger.get_book(scope, company_id).await,
            |e| matches!(e, LedgerError::Unauthorized | LedgerError::CompanyNotFound),
            |e| matches!(e, LedgerError::BookNotOpen),
        );
        let book_open = matches!(book, Ok(Some(_)));
        areas.push(match &book {
            Ok(Some(book)) => area(
                "book",
                "Book",
                false,
                "/finance/ledger/books",
                SetupState::SetUp,
                format!(
                    "Currency {}, {} decimal places. Chosen when the book was opened; it cannot be changed.",
                    book.currency_code,
                    book.minor_units.get()
                ),
            ),
            Ok(None) => area(
                "book",
                "Book",
                false,
                "/finance/ledger/books",
                SetupState::NotSetUp,
                "The book is not open. Choose its currency and decimal places.",
            ),
            Err(state) => area(
                "book",
                "Book",
                false,
                "/finance/ledger/books",
                *state,
                failed(*state),
            ),
        });

        let blocked = |id, name, path| {
            area(
                id,
                name,
                false,
                path,
                SetupState::NotSetUp,
                "Open the book first.",
            )
        };
        areas.push(if !book_open {
            match &book {
                Err(state) => area(
                    "chart",
                    "Chart of accounts",
                    false,
                    "/finance/ledger/accounts",
                    *state,
                    failed(*state),
                ),
                _ => blocked("chart", "Chart of accounts", "/finance/ledger/accounts"),
            }
        } else {
            match read(
                self.ledger.list_accounts(scope, company_id).await,
                |e| matches!(e, LedgerError::Unauthorized),
                |_| false,
            ) {
                Ok(Some(accounts)) if accounts.is_empty() => area(
                    "chart",
                    "Chart of accounts",
                    false,
                    "/finance/ledger/accounts",
                    SetupState::NotSetUp,
                    "The chart has no account. Add the company's accounts.",
                ),
                Ok(Some(accounts)) => {
                    let active = accounts.iter().filter(|a| a.active).count();
                    let retained = accounts
                        .iter()
                        .any(|a| a.active && a.account_type == AccountType::Equity && a.retained_earnings);
                    area(
                        "chart",
                        "Chart of accounts",
                        false,
                        "/finance/ledger/accounts",
                        // Year-end closing needs the designation; until then
                        // the chart is usable but not complete.
                        if retained {
                            SetupState::SetUp
                        } else {
                            SetupState::Incomplete
                        },
                        format!(
                            "{} accounts, {active} active. {}",
                            accounts.len(),
                            if retained {
                                "A retained-earnings account is designated."
                            } else {
                                "No retained-earnings account is designated; closing a fiscal year needs one."
                            }
                        ),
                    )
                }
                Ok(None) => unreachable!("`missing` is never true for the chart"),
                Err(state) => area(
                    "chart",
                    "Chart of accounts",
                    false,
                    "/finance/ledger/accounts",
                    state,
                    failed(state),
                ),
            }
        });

        areas.push(if !book_open {
            match &book {
                Err(state) => area(
                    "fiscal-years",
                    "Fiscal years",
                    false,
                    "/finance/ledger/fiscal-years",
                    *state,
                    failed(*state),
                ),
                _ => blocked("fiscal-years", "Fiscal years", "/finance/ledger/fiscal-years"),
            }
        } else {
            match read(
                self.ledger.list_fiscal_years(scope, company_id).await,
                |e| matches!(e, LedgerError::Unauthorized),
                |_| false,
            ) {
                Ok(Some(years)) => match years.last() {
                    None => area(
                        "fiscal-years",
                        "Fiscal years",
                        false,
                        "/finance/ledger/fiscal-years",
                        SetupState::NotSetUp,
                        "No fiscal year is open. A company posts only into a period of an open fiscal year.",
                    ),
                    Some(latest) => {
                        let open = years
                            .iter()
                            .flat_map(|y| y.periods.iter())
                            .filter(|p| p.status == PeriodStatus::Open)
                            .count();
                        area(
                            "fiscal-years",
                            "Fiscal years",
                            false,
                            "/finance/ledger/fiscal-years",
                            if open == 0 {
                                SetupState::Incomplete
                            } else {
                                SetupState::SetUp
                            },
                            format!(
                                "{} fiscal {}; the latest runs {} to {}. {open} open {}.{}",
                                years.len(),
                                if years.len() == 1 { "year" } else { "years" },
                                latest.start_date,
                                latest.end_date,
                                if open == 1 { "period" } else { "periods" },
                                if open == 0 {
                                    " No period is open, so nothing can be posted; open the next fiscal year."
                                } else {
                                    ""
                                }
                            ),
                        )
                    }
                },
                Ok(None) => unreachable!("`missing` is never true for the fiscal years"),
                Err(state) => area(
                    "fiscal-years",
                    "Fiscal years",
                    false,
                    "/finance/ledger/fiscal-years",
                    state,
                    failed(state),
                ),
            }
        });

        // Controls start on for a new book: a measured fact, not a to-do.
        let controls = async {
            let controlled = self
                .ledger
                .get_controlled_bookkeeping(scope, company_id)
                .await;
            let duties = self.ledger.get_duty_separation(scope, company_id).await;
            (controlled, duties)
        }
        .await;
        areas.push(match controls {
            _ if matches!(book, Ok(None)) => area(
                "controls",
                "Bookkeeping controls",
                true,
                "/finance/reports/controls",
                SetupState::NotTracked,
                "They start when the book is opened.",
            ),
            (Ok(controlled), Ok(duties)) => area(
                "controls",
                "Bookkeeping controls",
                true,
                "/finance/reports/controls",
                SetupState::SetUp,
                format!(
                    "Controlled bookkeeping is {}. Separation of duties is {}.",
                    if controlled.controlled { "on" } else { "off" },
                    if duties.required { "on" } else { "off" }
                ),
            ),
            (Err(LedgerError::Unauthorized), _) | (_, Err(LedgerError::Unauthorized)) => area(
                "controls",
                "Bookkeeping controls",
                true,
                "/finance/reports/controls",
                SetupState::NoAccess,
                NO_ACCESS,
            ),
            _ => area(
                "controls",
                "Bookkeeping controls",
                true,
                "/finance/reports/controls",
                SetupState::Unavailable,
                UNAVAILABLE,
            ),
        });

        let (state, summary) = match read(
            self.ledger.opening_progress(scope, company_id).await,
            |e| matches!(e, LedgerError::Unauthorized),
            |_| false,
        ) {
            Ok(Some(Some(opening))) => {
                use bilibi_finance_ledger::JournalStatus;
                let (state, status) = match (opening.status, opening.reversed_by_entry_id) {
                    (_, Some(_)) => (SetupState::Incomplete, "was posted and has been reversed"),
                    (JournalStatus::Approved, _) => (SetupState::SetUp, "is posted"),
                    (JournalStatus::Submitted, _) => (SetupState::Incomplete, "is awaiting approval"),
                    (JournalStatus::Rejected, _) => (SetupState::Incomplete, "was rejected"),
                    (JournalStatus::Withdrawn, _) => (SetupState::Incomplete, "was withdrawn"),
                };
                (state, format!("Opening journal {} dated {} {status}. Open invoices, bills and historical transactions are excluded; this does not establish a complete conversion.", opening.journal_number, opening.conversion_date))
            }
            Ok(Some(None)) => (SetupState::NotSetUp, "No opening balances have been submitted. Only needed when converting from another accounting system; a new company can start without them.".into()),
            Err(state) => (state, failed(state)),
            Ok(None) => unreachable!("`missing` is never true for opening balances"),
        };
        areas.push(area(
            "opening-balances",
            "Opening balances",
            true,
            "/finance/ledger/journals?opening=1",
            state,
            summary,
        ));

        areas.push(
            match read(
                s.receivables.get_setup(scope, company_id).await,
                |e| matches!(e, bilibi_finance_receivables::ReceivableError::Unauthorized),
                |e| {
                    matches!(
                        e,
                        bilibi_finance_receivables::ReceivableError::NotConfigured
                    )
                },
            ) {
                Ok(Some(_)) => area(
                    "receivables",
                    "Receivables",
                    true,
                    "/finance/receivables/settings",
                    SetupState::SetUp,
                    "The receivable control and income accounts are chosen.",
                ),
                Ok(None) => area(
                    "receivables",
                    "Receivables",
                    true,
                    "/finance/receivables/settings",
                    SetupState::NotSetUp,
                    "Choose the receivable control and income accounts before issuing an invoice.",
                ),
                Err(state) => area(
                    "receivables",
                    "Receivables",
                    true,
                    "/finance/receivables/settings",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.payables.get_settings(scope, company_id).await,
                |e| matches!(e, bilibi_finance_payables::PayablesError::Unauthorized),
                |e| {
                    matches!(
                        e,
                        bilibi_finance_payables::PayablesError::PayablesNotConfigured
                    )
                },
            ) {
                Ok(Some(_)) => area(
                    "payables",
                    "Payables",
                    true,
                    "/finance/payables/setup",
                    SetupState::SetUp,
                    "The payable control account is chosen.",
                ),
                Ok(None) => area(
                    "payables",
                    "Payables",
                    true,
                    "/finance/payables/setup",
                    SetupState::NotSetUp,
                    "Choose the payable control account before recording a bill.",
                ),
                Err(state) => area(
                    "payables",
                    "Payables",
                    true,
                    "/finance/payables/setup",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.sst.list_registrations(scope, company_id).await,
                |e| matches!(e, bilibi_finance_sst::SstError::Unauthorized),
                |_| false,
            ) {
                Ok(Some(registrations)) if registrations.is_empty() => area(
                    "tax",
                    "Sales and service tax",
                    true,
                    "/finance/sst/setup",
                    SetupState::NotSetUp,
                    "No SST registration is recorded. Skip this if the company is not registered.",
                ),
                Ok(Some(registrations)) => area(
                    "tax",
                    "Sales and service tax",
                    true,
                    "/finance/sst/setup",
                    SetupState::SetUp,
                    format!(
                        "{} {} recorded.",
                        registrations.len(),
                        if registrations.len() == 1 {
                            "registration"
                        } else {
                            "registrations"
                        }
                    ),
                ),
                Ok(None) => unreachable!("`missing` is never true for registrations"),
                Err(state) => area(
                    "tax",
                    "Sales and service tax",
                    true,
                    "/finance/sst/setup",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.einvoice.setup(scope, company_id).await,
                |e| matches!(e, bilibi_finance_einvoice::EinvoiceError::Unauthorized),
                |_| false,
            ) {
                Ok(Some(setup)) => {
                    let mut lacking: Vec<&str> = Vec::new();
                    if !setup.client_id_set {
                        lacking.push("client ID");
                    }
                    if !setup.client_secret_set {
                        lacking.push("client secret");
                    }
                    if !setup.certificate_set {
                        lacking.push("signing certificate");
                    }
                    if !setup.private_key_set {
                        lacking.push("private key");
                    }
                    if !setup.supplier_missing.is_empty() {
                        lacking.push("supplier profile");
                    }
                    let state = if lacking.is_empty() {
                        SetupState::SetUp
                    } else if lacking.len() == 5 {
                        SetupState::NotSetUp
                    } else {
                        SetupState::Incomplete
                    };
                    area(
                        "einvoice",
                        "E-invoicing",
                        true,
                        "/finance/einvoice/setup",
                        state,
                        if lacking.is_empty() {
                            format!("Ready for the {} environment.", setup.environment)
                        } else {
                            format!("Still needed: {}.", lacking.join(", "))
                        },
                    )
                }
                Ok(None) => unreachable!("`missing` is never true for e-invoicing"),
                Err(state) => area(
                    "einvoice",
                    "E-invoicing",
                    true,
                    "/finance/einvoice/setup",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.reconciliation.list_bank_accounts(scope, company_id).await,
                |e| {
                    matches!(
                        e,
                        bilibi_finance_reconciliation::ReconciliationError::Unauthorized
                    )
                },
                |_| false,
            ) {
                Ok(Some(accounts)) if accounts.is_empty() => area(
                    "bank",
                    "Bank reconciliation",
                    true,
                    "/finance/reconciliation",
                    SetupState::NotSetUp,
                    "No bank account is linked to a ledger account.",
                ),
                Ok(Some(accounts)) => area(
                    "bank",
                    "Bank reconciliation",
                    true,
                    "/finance/reconciliation",
                    SetupState::SetUp,
                    format!(
                        "{} bank {} linked.",
                        accounts.len(),
                        if accounts.len() == 1 {
                            "account"
                        } else {
                            "accounts"
                        }
                    ),
                ),
                Ok(None) => unreachable!("`missing` is never true for bank accounts"),
                Err(state) => area(
                    "bank",
                    "Bank reconciliation",
                    true,
                    "/finance/reconciliation",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.payroll.get_settings(scope, company_id).await,
                |e| matches!(e, bilibi_finance_payroll::PayrollError::Unauthorized),
                |e| {
                    matches!(
                        e,
                        bilibi_finance_payroll::PayrollError::PayrollNotConfigured
                    )
                },
            ) {
                Ok(Some(_)) => area(
                    "payroll",
                    "Payroll postings",
                    true,
                    "/finance/payroll/setup",
                    SetupState::SetUp,
                    "The posting accounts are chosen.",
                ),
                Ok(None) => area(
                    "payroll",
                    "Payroll postings",
                    true,
                    "/finance/payroll/setup",
                    SetupState::NotSetUp,
                    "Choose the posting accounts before payroll can post.",
                ),
                Err(state) => area(
                    "payroll",
                    "Payroll postings",
                    true,
                    "/finance/payroll/setup",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(
            match read(
                s.fixed_assets.list_categories(scope, company_id).await,
                |e| {
                    matches!(
                        e,
                        bilibi_finance_fixed_assets::FixedAssetsError::Unauthorized
                    )
                },
                |_| false,
            ) {
                Ok(Some(categories)) if categories.is_empty() => area(
                    "fixed-assets",
                    "Fixed assets",
                    true,
                    "/finance/fixed-assets/categories",
                    SetupState::NotSetUp,
                    "No asset category is defined.",
                ),
                Ok(Some(categories)) => area(
                    "fixed-assets",
                    "Fixed assets",
                    true,
                    "/finance/fixed-assets/categories",
                    SetupState::SetUp,
                    format!(
                        "{} asset {} defined.",
                        categories.len(),
                        if categories.len() == 1 {
                            "category"
                        } else {
                            "categories"
                        }
                    ),
                ),
                Ok(None) => unreachable!("`missing` is never true for asset categories"),
                Err(state) => area(
                    "fixed-assets",
                    "Fixed assets",
                    true,
                    "/finance/fixed-assets/categories",
                    state,
                    failed(state),
                ),
            },
        );

        areas.push(area(
            "accountant-access",
            "Accountant access",
            true,
            "/authz/roles",
            SetupState::NotTracked,
            "Give the people who keep this company's books a role, such as External accountant.",
        ));

        // A count is shown only when it is true: every required area read.
        let required: Vec<&SetupArea> = areas.iter().filter(|a| !a.optional).collect();
        let readable = required.iter().all(|a| {
            matches!(
                a.state,
                SetupState::SetUp | SetupState::NotSetUp | SetupState::Incomplete
            )
        });
        let (required_set_up, required_total) = if readable {
            (
                Some(
                    required
                        .iter()
                        .filter(|a| a.state == SetupState::SetUp)
                        .count(),
                ),
                Some(required.len()),
            )
        } else {
            (None, None)
        };
        Ok(SetupOverview {
            company_id,
            areas,
            required_set_up,
            required_total,
        })
    }
}
