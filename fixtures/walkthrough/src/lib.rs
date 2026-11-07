//! The Finance walkthrough fixture: the ledger records the slice proofs act
//! on, created through the facade on top of the Platform's walkthrough
//! fixture (its tenants, accounts, roles and companies). It is not product
//! data: only examples depend on this crate, and only a disposable, freshly
//! migrated database may receive it (the Platform fixture refuses any other).
//! `install` is idempotent: a rerun finds the book open and the chart in
//! place and creates nothing.
//!
//! Company 001 (tenant 1's primary company) gets a book in [`CURRENCY`] with
//! [`MINOR_UNITS`] decimal places and the chart in [`CHART`], created by the
//! fixture's `manager@example.test`, whose Person has a desk there, and
//! its first fiscal year, [`FISCAL_YEAR_START`] for twelve months, since a new
//! book is controlled and posts only into a period it has opened. The
//! fixture's roles hold ledger capabilities only through
//! [`walkthrough_ledger_roles!`], which the expanding example declares beside
//! the Platform fixture's two macros: the manager holds every ledger
//! capability, staff view and maintain the chart, the viewer only views.
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error};
use bilibi_finance_ledger::{
    Account, AccountType, Book, Ledger, LedgerError, NewAccount, NewBook, NewFiscalYear, Result,
    SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, Scope};
use jiff::civil::{date, Date};

pub mod attachments;
pub mod charts;
pub mod controls;
pub mod matching;
pub mod payables;
pub mod payments;
pub mod payroll;
mod payroll_ui;
pub mod periods;
pub mod receivables;
pub mod receivables_documents;
pub mod reconciliation;
pub mod reconciliation_first_import;
pub mod reports;
pub mod totals;

pub mod clearance;

mod einvoice_bridge;

// NC2: reconciliation matching, reconciliations and statement-line posting.
pub mod reconciliation_matching;

/// Every fixture account's password.
pub use bilibi_walkthrough_fixture::PASSWORD;
#[doc(hidden)]
pub use {bilibi_authz as authz, bilibi_module_registry as registry, inventory};

/// The company whose book the fixture opens: Company 001, tenant 1's primary.
pub const COMPANY: i64 = 1;
/// The fixture manager's account (`manager@example.test`), with a desk in
/// Company 001.
pub const MANAGER: i64 = 1;
pub const CURRENCY: &str = "MYR";
/// The first day of Company 001's first fiscal year.
pub const FISCAL_YEAR_START: Date = date(2026, 1, 1);
pub const MINOR_UNITS: i64 = 2;
/// Code, name, type and whether the account is the retained-earnings one.
pub const CHART: [(&str, &str, AccountType, bool); 9] = [
    ("1000", "Cash at bank", AccountType::Asset, false),
    ("1100", "Trade receivables", AccountType::Asset, false),
    ("2000", "Trade payables", AccountType::Liability, false),
    ("2100", "Tax payable", AccountType::Liability, false),
    ("3000", "Share capital", AccountType::Equity, false),
    ("3100", "Retained earnings", AccountType::Equity, true),
    ("4000", "Sales", AccountType::Income, false),
    ("5000", "Cost of sales", AccountType::Expense, false),
    ("6000", "Bank charges", AccountType::Expense, false),
];

/// What the fixture left in place.
#[derive(Debug)]
pub struct Installed {
    pub book: Book,
    pub accounts: Vec<Account>,
}

/// Turns Base Step-up enforcement off for a fixture binary that installs its
/// own services, as `BILIBI_ENV=development` does in the host: the fixtures
/// grant disposable capabilities through `Grants`, which asks for step-up,
/// and no fixture account enrolls an authenticator. Call it right after
/// `Services::install` or `bilibi_web::App::new`.
pub fn without_step_up(services: &Services) -> bilibi_module_registry::Result<()> {
    services
        .get::<bilibi_base::step_up::StepUp>(bilibi_base::step_up::SERVICE)?
        .set_enforced(false);
    Ok(())
}

/// The registry of a fixture binary that composes the modules itself rather
/// than through `bilibi_web::App::new`: both bootstrap values the pinned
/// Platform needs at install (`bootstrap.database`, and
/// `bootstrap.composition`, which Base Software reads), the modules
/// installed, and step-up off. Boot with this, never a bare
/// `Services::default()` plus `provide("bootstrap.database")`, which the
/// registry now refuses as `Service("bootstrap.composition")`.
pub fn services(
    db: &Db,
    modules: &[&'static bilibi_module_registry::Module],
) -> bilibi_module_registry::Result<Services> {
    let mut services = Services::default();
    services.provide("bootstrap.database", std::sync::Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(modules)?;
    without_step_up(&services)?;
    Ok(services)
}

/// The Platform fixture's accounts and tenants, plus the Core default
/// relationship roles (customer, supplier, ...) its tenants are installed
/// without. A binary that skips [`install`] still calls this, never
/// `bilibi_walkthrough_fixture::install` alone: a customer or supplier cannot
/// be added until the tenant has its relationship roles.
pub async fn install_platform(db: &Db) -> Result<()> {
    bilibi_walkthrough_fixture::install(db).await?;
    let client = db.get().await.map_err(Error::from)?;
    bilibi_company::ensure_default_relationship_roles(&**client, 1).await?;
    bilibi_company::ensure_default_relationship_roles(&**client, 2).await?;
    Ok(())
}

/// The Platform fixture's staff accounts that work in a Finance company need a
/// Person, an affiliation and a desk before the Platform lets them act there:
/// Company 001's administrator (user 4, who reviews) and viewer (user 2), and
/// tenant 2's manager (user 5) in Company 100. Fixture identity SQL only,
/// idempotent; every Finance record is still created by its facade.
/// [`install`] calls it, so a binary that signs in as one of them needs no copy
/// of its own; one that proves what an unaffiliated account sees
/// (`report-controls`) calls only [`install_platform`]. Tenant 2's Person and
/// affiliation take IDs of their own (106, 105) because Person IDs are global
/// and `conversion` and `ledger-ui` insert their own at 6 and 5.
pub async fn install_staff(db: &Db) -> Result<()> {
    let client = db.get().await.map_err(Error::from)?;
    client
        .batch_execute(
            "INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(4,1,'person_4','Alex Admin','human','admin@example.test'),(5,1,'person_5','Lee Viewer','human','viewer@example.test') ON CONFLICT DO NOTHING;
             INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(3,1,4,1,'Reviewer','admin@example.test'),(4,1,5,1,'Reader','viewer@example.test') ON CONFLICT DO NOTHING;
             UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=4 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
             UPDATE users SET person_id=5 WHERE tenant_id=1 AND id=2 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,1 FROM users WHERE tenant_id=1 AND id=2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
             INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(106,2,'person_106','Other Manager','human','manager@other.example.test') ON CONFLICT DO NOTHING;
             INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title) VALUES(105,2,106,100,'Recorder') ON CONFLICT DO NOTHING;
             UPDATE users SET person_id=106 WHERE tenant_id=2 AND id=5 AND person_id IS NULL; INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,100 FROM users WHERE tenant_id=2 AND id=5 AND person_id IS NOT NULL ON CONFLICT DO NOTHING;
             SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));
             SELECT setval(pg_get_serial_sequence('person_affiliations','id'),(SELECT max(id) FROM person_affiliations))",
        )
        .await
        .map_err(Error::from)?;
    Ok(())
}

/// Installs the Platform fixture, reconciles the contributed roles, and opens
/// Company 001's book and chart through the ledger facade as the manager.
pub async fn install(db: &Db, services: &Services) -> Result<Installed> {
    install_platform(db).await?;
    install_staff(db).await?;
    // The administrator proves cross-company scope in Companies 002 and 003
    // (003 is the disposable company later walkthroughs archive).
    for company in [2, 3] {
        give_desk(db, 1, 4, company).await?;
    }
    services
        .get::<Grants>("base/authz.grants")
        .map_err(Error::from)?
        .reconcile()
        .await?;
    let ledger = services.get::<Ledger>(SERVICE).map_err(Error::from)?;
    let administrator = sign_in(db, services, 1, 4).await?;
    // The desk facade's write must change a row. Do not send a duplicate
    // assignment on a fixture rerun: an empty mutation poisons its boundary.
    let assigned = db
        .get()
        .await
        .map_err(Error::from)?
        .query_opt(
            "SELECT id FROM user_desks WHERE tenant_id=1 AND company_id=2 AND person_id=1",
            &[],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if !assigned {
        services
            .get::<bilibi_core::user::UserService>("core/user.accounts")
            .map_err(Error::from)?
            .set_desk(&administrator, 2, 1, true)
            .await?;
    }
    let manager = sign_in(db, services, 1, MANAGER).await?;
    let book = match ledger.get_book(&manager, COMPANY).await {
        Err(LedgerError::BookNotOpen) => {
            ledger
                .open_book(
                    &manager,
                    COMPANY,
                    &NewBook {
                        currency_code: CURRENCY.into(),
                        minor_units: MINOR_UNITS,
                    },
                )
                .await?
        }
        other => other?,
    };
    let existing = ledger.list_accounts(&manager, COMPANY).await?;
    for (code, name, account_type, retained_earnings) in CHART {
        if existing.iter().any(|account| account.code == code) {
            continue;
        }
        let mut new = NewAccount::new(code, name, account_type);
        new.retained_earnings = retained_earnings;
        ledger.create_account(&manager, COMPANY, &new).await?;
    }
    let accounts = ledger.list_accounts(&manager, COMPANY).await?;
    if ledger
        .list_fiscal_years(&manager, COMPANY)
        .await?
        .is_empty()
    {
        ledger
            .open_fiscal_year(
                &manager,
                COMPANY,
                &NewFiscalYear {
                    start_date: Some(FISCAL_YEAR_START),
                    end_date: None,
                },
            )
            .await?;
    }
    initialize_journal_approvals(db, services, &manager, COMPANY).await?;
    records::install(db).await?;
    Ok(Installed { book, accounts })
}

/// Explicit disposable setup through Workflow, never a boot-time seed.
pub async fn initialize_journal_approvals(
    db: &Db,
    services: &Services,
    scope: &Scope,
    company: i64,
) -> Result<()> {
    let engine = services
        .get::<bilibi_workflow::Engine>(bilibi_workflow::SERVICE)
        .map_err(Error::from)?;
    match engine
        .approval_policy(scope, "finance.ledger.manual_journal", company)
        .await
    {
        Ok(_) => return Ok(()),
        Err(Error::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    let ledger = services.get::<Ledger>(SERVICE).map_err(Error::from)?;
    let mut client = db.get().await.map_err(Error::from)?;
    let mut tx = ledger
        .write(
            &mut client,
            scope,
            company,
            &[bilibi_finance_ledger::MANAGE_DUTY_SEPARATION],
            "finance.walkthrough.journal_rules",
        )
        .await?;
    engine
        .publish_approval_policy_in(
            &mut tx,
            scope,
            bilibi_workflow::ApprovalPolicyChange {
                case: "finance.ledger.manual_journal",
                company,
                expected: None,
                policy: Ledger::journal_approval_policy(),
                reason: "Walkthrough manual journal approval rules",
            },
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A sealed scope for one fixture account, as a signed-in session would hold.
/// The account also gets the session row a sign-in writes (one per account,
/// carrying the scope's empty CSRF value): Agent Access approves a connection
/// from the person's own browser session and refuses a scope with none.
pub async fn sign_in(db: &Db, services: &Services, tenant: i64, user: i64) -> Result<Scope> {
    let authentication = services
        .get::<Authentication>("base/tenancy.authentication")
        .map_err(Error::from)?;
    let client = db.get().await.map_err(Error::from)?;
    client
        .execute(
            "INSERT INTO sessions(id,tenant_id,user_id,csrf,expires_at,user_agent) VALUES($1,$2,$3,'',now()+interval '1 day','Walkthrough browser') ON CONFLICT DO NOTHING",
            &[&format!("walkthrough-{tenant}-{user}"), &tenant, &user],
        )
        .await
        .map_err(Error::from)?;
    Ok(authentication
        .authenticate(&**client, tenant, user, String::new())
        .await?)
}

/// Gives a fixture account a desk in a company a walkthrough created, which
/// the Platform requires before the account can act there (an installed
/// manager has Companies 001 and 002 only). Idempotent fixture identity SQL.
pub async fn give_desk(db: &Db, tenant: i64, user: i64, company: i64) -> Result<()> {
    db.get()
        .await
        .map_err(Error::from)?
        .execute(
            "INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT tenant_id,person_id,$3 FROM users WHERE tenant_id=$1 AND id=$2 AND person_id IS NOT NULL ON CONFLICT DO NOTHING",
            &[&tenant, &user, &company],
        )
        .await
        .map_err(Error::from)?;
    Ok(())
}

/// What the Platform fixture's roles hold in the ledger, declared in the
/// expanding example's own crate beside
/// `bilibi_walkthrough_fixture::walkthrough_vocabulary!` and
/// `walkthrough_module_roles!`.
#[macro_export]
macro_rules! walkthrough_ledger_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.ledger", $crate::registry::Provenance { module: "finance/ledger", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.ledger.view", "finance.ledger.accounts.manage", "finance.ledger.entries.post",
                    "finance.ledger.templates.manage",
                    "finance.ledger.periods.manage", "finance.ledger.periods.reopen", "finance.ledger.periods.lock",
                    "finance.ledger.duty-separation.manage", "finance.ledger.controlled-bookkeeping.manage",
                    "finance.ledger.late-recordings.review", "finance.ledger.journals.submit",
                    "finance.ledger.journals.approve", "finance.ledger.approval-exceptions.review",
                    "workflow.approvals.view",
                ] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.ledger.view", "finance.ledger.accounts.manage",
                ] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.ledger.view",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}

/// The walkthrough's records: what the specimen sources of source documents
/// describe, in a table only a disposable database holds. A real Finance
/// module owns its records and contributes its own `Source`; this table
/// stands in for them so the attachments facade can be shown on a draft and
/// a final record.
pub mod records {
    use bilibi_database::{Db, Error};
    use bilibi_finance_attachments::{
        on_source_client, Source, SourceClient, SourceDefinition, SourceDocument, SourceFuture,
        SourceRefusal, SOURCES,
    };
    use bilibi_module_registry::{Contribution, Provenance};
    use bilibi_tenancy::Scope;
    use jiff::civil::Date;
    use tokio_postgres::GenericClient;

    /// A record any ledger reader reads.
    pub const OPEN_SOURCE: &str = "finance/attachments/specimen_record";
    /// A record only a poster of entries reads.
    pub const RESTRICTED_SOURCE: &str = "finance/attachments/restricted_record";

    struct Records {
        kind: &'static str,
        label: &'static str,
        view_capabilities: &'static [&'static str],
    }

    impl Source for Records {
        fn label(&self) -> &'static str {
            self.label
        }
        fn view_capabilities(&self) -> &'static [&'static str] {
            self.view_capabilities
        }
        fn document<'a>(
            &'a self,
            client: SourceClient<'a>,
            scope: &'a Scope,
            company_id: i64,
            document_id: i64,
        ) -> SourceFuture<'a> {
            Box::pin(async move {
                on_source_client!(client, |c| self
                    .read(c, scope, company_id, document_id)
                    .await)
            })
        }
    }

    impl Records {
        async fn read(
            &self,
            client: &(impl GenericClient + Sync),
            scope: &Scope,
            company_id: i64,
            document_id: i64,
        ) -> Result<SourceDocument, SourceRefusal> {
            for capability in self.view_capabilities {
                bilibi_company::authorize_work_target(client, scope, company_id, capability)
                    .await
                    .map_err(|error| match error {
                        Error::Forbidden | Error::Unauthorized => SourceRefusal::Unauthorized,
                        Error::NotFound => SourceRefusal::NotFound,
                        other => SourceRefusal::Failed(other),
                    })?;
            }
            let row = client
                .query_opt(
                    "SELECT label,to_char(record_date,'YYYY-MM-DD'),is_final FROM finance_walkthrough_records \
                     WHERE tenant_id=$1 AND company_id=$2 AND id=$3 AND kind=$4",
                    &[&scope.tenant(), &company_id, &document_id, &self.kind],
                )
                .await
                .map_err(Error::from)?
                .ok_or(SourceRefusal::NotFound)?;
            Ok(SourceDocument {
                label: row.get(0),
                date: row.get::<_, &str>(1).parse().map_err(|_| Error::Internal)?,
                is_final: row.get(2),
            })
        }
    }

    static OPEN: Records = Records {
        kind: "open",
        label: "Specimen record",
        view_capabilities: &["finance.ledger.view"],
    };
    static RESTRICTED: Records = Records {
        kind: "restricted",
        label: "Restricted record",
        view_capabilities: &["finance.ledger.entries.post"],
    };
    // The specimen sources are contributed as the attachments module, the
    // only registered module this example-only crate can speak for.
    inventory::submit! { Contribution::new(SOURCES, OPEN_SOURCE,
    Provenance { module: "finance/attachments", source: file!(), line: line!() },
    &SourceDefinition { source: &OPEN }) }
    inventory::submit! { Contribution::new(SOURCES, RESTRICTED_SOURCE,
    Provenance { module: "finance/attachments", source: file!(), line: line!() },
    &SourceDefinition { source: &RESTRICTED }) }

    /// Creates the table in a disposable database; a rerun finds it in place.
    pub async fn install(db: &Db) -> Result<(), Error> {
        let client = db.get().await?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS finance_walkthrough_records (\
                   id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL, \
                   kind text NOT NULL, label text NOT NULL, record_date date NOT NULL, \
                   is_final boolean NOT NULL DEFAULT false)",
            )
            .await
            .map_err(Error::from)
    }

    /// Adds a record of `kind` (`open` or `restricted`) and returns its id.
    pub async fn add(
        db: &Db,
        tenant: i64,
        company_id: i64,
        kind: &str,
        label: &str,
        date: Date,
        is_final: bool,
    ) -> Result<i64, Error> {
        let client = db.get().await?;
        Ok(client
            .query_one(
                "INSERT INTO finance_walkthrough_records(tenant_id,company_id,kind,label,record_date,is_final) \
                 VALUES($1,$2,$3,$4,$5::text::date,$6) RETURNING id",
                &[&tenant, &company_id, &kind, &label, &date.to_string(), &is_final],
            )
            .await
            .map_err(Error::from)?
            .get(0))
    }

    /// Re-dates a record and/or finalises it, as its owning module would.
    pub async fn set(db: &Db, id: i64, date: Date, is_final: bool) -> Result<(), Error> {
        let client = db.get().await?;
        client
            .execute(
                "UPDATE finance_walkthrough_records SET record_date=$2::text::date, is_final=$3 WHERE id=$1",
                &[&id, &date.to_string(), &is_final],
            )
            .await
            .map_err(Error::from)?;
        Ok(())
    }
}

/// What the Platform fixture's roles hold in the attachments module: the
/// manager every capability, staff view and attach, the viewer only view.
/// Declared in the expanding example's own crate beside
/// [`walkthrough_ledger_roles!`].
#[macro_export]
macro_rules! walkthrough_attachments_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.attachments", $crate::registry::Provenance { module: "finance/attachments", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.attachments.view", "finance.attachments.create", "finance.attachments.delete",
                    "finance.attachments.retention.manage",
                ] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.attachments.view", "finance.attachments.create",
                ] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.attachments.view",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}

/// What the Platform fixture's roles hold in e-invoicing, declared beside
/// [`walkthrough_ledger_roles!`]: the manager every capability, staff and the
/// viewer only the read.
#[macro_export]
macro_rules! walkthrough_einvoice_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.einvoice", $crate::registry::Provenance { module: "finance/einvoice", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.einvoice.view", "finance.einvoice.documents.issue", "finance.einvoice.documents.submit",
                    "finance.einvoice.documents.cancel", "finance.einvoice.settings.manage",
                ] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.einvoice.view",
                ] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.einvoice.view",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}

/// What the Platform fixture's roles hold in the reports module, declared
/// beside [`walkthrough_ledger_roles!`]: the manager issues reports and
/// exports the auditor pack; staff and the viewer read reports through the
/// ledger's view capability only.
#[macro_export]
macro_rules! walkthrough_reports_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.reports", $crate::registry::Provenance { module: "finance/reports", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.reports.issue", "finance.reports.reports-export",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}

/// What the Platform fixture's roles hold in the reconciliation module: the
/// manager every capability, staff view and import statements, the viewer
/// only view. Declared in the expanding example's own crate beside
/// [`walkthrough_ledger_roles!`].
#[macro_export]
macro_rules! walkthrough_reconciliation_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.finance.reconciliation", $crate::registry::Provenance { module: "finance/reconciliation", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.reconciliation.view", "finance.reconciliation.bank-accounts.manage",
                    "finance.reconciliation.statements.import", "finance.reconciliation.matches.manage",
                    "finance.reconciliation.reconciliations.manage", "finance.reconciliation.reconciliations.finalise",
                    "finance.reconciliation.statement-lines.post", "finance.reconciliation.statement-lines.manage",
                ] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.reconciliation.view", "finance.reconciliation.statements.import",
                ] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "finance.reconciliation.view",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}

// NE5: real e-invoice pages, downloads and local authority browser proof.
pub mod einvoice_ui;

// NE6: executable page and browser proof on disposable data.
pub mod einvoice_pages;

// NU1: records for the combined, disposable browser sweep.
pub mod browser;

// First-adopter setup and shared operation-transaction proofs.
pub mod adopter_setup;

// Restricted accountant access: production role, scoped exports and live revocation.
pub mod external_accountant;
// Saved starter charts, configured proposals and cross-company reuse.
pub mod chart_templates;

pub mod vendors;

// The opening balances page: paste, guessed columns, live totals, submit and later steps.
pub mod opening_page;

// #111: real OAuth provisioning for the prospective asset continuation proof.
pub mod assets_lifecycle;
