//! What the module adds to the shared catalogs (Bilimbi
//! `Bilimbi.Finance.Reports.Contributions`): its two capability keys verbatim,
//! conversion sign-off, and the `tenant_owner` role holding all three. Reports read ledger figures, so
//! they stand on the ledger's capabilities: viewing them is viewing the ledger
//! (`finance.ledger.view`), and arranging them is part of keeping the chart of
//! accounts (`finance.ledger.accounts.manage`, the sections setting's editor
//! capability, in `configuration.rs`). Menu items arrive with the pages that
//! own them.
use crate::MODULE;
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{Contribution, Provenance};

/// Issues an immutable report snapshot.
pub const ISSUE: &str = "finance.reports.issue";
/// Generates and downloads a company auditor pack.
pub const EXPORT: &str = "finance.reports.reports-export";

/// Records an accountant attestation against an issued conversion snapshot.
pub const SIGN_OFF: &str = "finance.reports.sign-off";
pub const CAPABILITIES: [&str; 3] = [ISSUE, EXPORT, SIGN_OFF];

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}
pub(crate) use provenance;

// Bilimbi declares the verb `reports-export` and relies on a sibling for
// `issue`; Bilibi merges verbs across modules, so both are declared here and
// the module needs no sibling to have declared one. The keys are unchanged.
inventory::submit! { Contribution::new(VOCABULARY, "finance/reports", provenance!(), &VocabularyContribution {
    domains: &["finance"],
    verbs: &["issue", "reports-export", "sign-off"],
    capabilities: &[C::tenant(ISSUE), C::company(EXPORT), C::tenant(SIGN_OFF)],
    roles: &[RoleDefinition {
        code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly",
        grant_all: false, capabilities: &CAPABILITIES,
    }, RoleDefinition {
        code: "external_accountant", name: "External accountant",
        description: "Read the company ledger and reports, export accounting records and submit adjusting journals for approval.",
        grant_all: false, capabilities: &[EXPORT],
    }, RoleDefinition {
        code: "appointed_auditor", name: "Appointed auditor",
        description: "Read-only access to one company's ledger, reports and source evidence for a named audit engagement.",
        grant_all: false, capabilities: &[EXPORT],
    }],
    system_principals: &[],
}) }

// Bilimbi reports/priv/web_routes.exs and Reports.Contributions.
use bilibi_module_registry::contributions::{Access, Method, Route, ROUTES};
inventory::submit! { Contribution::new(ROUTES, "finance/reports.index", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/reports", service: "finance/reports.pages",
    operation: "reports", access: Access::Capability(crate::VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "finance/reports.activity", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/reports/accounts/{account_id}", service: "finance/reports.pages",
    operation: "activity", access: Access::Capability(crate::VIEW), framed: true,
}) }
// Menu group (a bookkeeper's section of the Finance menu); its pages sit under it.
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::container("finance.group.bookkeeping", "Bookkeeping")
        .under("finance")
        .icon("gmdi-money-r")
        .order(10)
);

bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page("finance.reports", "Reports", "/finance/reports")
        .company_scoped()
        .icon("gmdi-money-r")
        .under("finance.group.bookkeeping")
        .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
        .order(4)
);

// Bilimbi Reports.Web.{BookkeepingLive,ApprovalExceptionsLive} routes/menu.
inventory::submit! { Contribution::new(ROUTES, "finance/reports.controls", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/reports/controls", service: "finance/reports.control-pages", operation: "controls", access: Access::Capability(crate::VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "finance/reports.approval-exceptions", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/reports/approval-exceptions", service: "finance/reports.control-pages", operation: "approval-exceptions", access: Access::Capability(crate::VIEW), framed: true,
}) }
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page(
        "finance.reports.controls",
        "Bookkeeping controls",
        "/finance/reports/controls"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bookkeeping")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(1)
);
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page(
        "finance.reports.approval-exceptions",
        "Approval exceptions",
        "/finance/reports/approval-exceptions"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bookkeeping")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(3)
);

// Bilimbi Reports.Web.AuditorPackController: every click reauthorizes the artifact.
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::DOWNLOADS, "finance/reports.auditor-pack", provenance!(), &bilibi_module_registry::contributions::Download {
    path: "/finance/reports/auditor-packs/{artifact_id}", service: "finance/reports.downloads",
    operation: "auditor-pack", access: Access::Capability(EXPORT),
}) }

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "finance/reports.pages", bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "finance/reports.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Events { events:&["export_auditor_pack"], reason: "Generating the auditor pack writes through the Reports facade; no contract yet." },
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "finance/reports.control-pages", bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "finance/reports.control-pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Controlled bookkeeping, duty separation and approval-exception reviews write through the Ledger facade, not the finance.ledger contracts yet." },
}) }

inventory::submit! { Contribution::new(ROUTES, "finance/reports.sections", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method:Method::Get,path:"/finance/reports/sections",service:"finance/reports.section-pages",operation:"sections",access:Access::Capability(bilibi_finance_ledger::MANAGE_ACCOUNTS),framed:true,
}) }
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page(
        "finance.reports.sections",
        "Report sections",
        "/finance/reports/sections"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bookkeeping")
    .requires(bilibi_menu::Requirement::Capability(
        bilibi_finance_ledger::MANAGE_ACCOUNTS
    ))
    .order(5)
);

inventory::submit! { Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "finance/reports.section-pages", provenance!(), &bilibi_module_registry::contributions::PageWrites {
    service:"finance/reports.section-pages",contracts:&[("save_sections","finance.reports.sections.set")],pending:bilibi_module_registry::contributions::Pending::None,
}) }

// The Finance setup page: what the current company has set up and what remains.
inventory::submit! { Contribution::new(ROUTES, "finance/reports.setup", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: "/finance/setup", service: crate::setup_page::PAGES,
    operation: "setup", access: Access::Capability(crate::VIEW), framed: true,
}) }
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page("finance.reports.setup", "Finance setup", "/finance/setup")
        .icon("gmdi-money-r")
        .under("finance.group.bookkeeping")
        .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
        .order(0)
);
// It reads through one contract and writes nothing.
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, crate::setup_page::PAGES, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: crate::setup_page::PAGES, contracts: &[], pending: bilibi_module_registry::contributions::Pending::None,
}) }

// One list and record home for every issued conversion report and its sign-off.
inventory::submit! { Contribution::new(ROUTES,"finance/reports.conversions",provenance!(),&Route {company_scope:bilibi_module_registry::contributions::CompanyScope::Company,method:Method::Get,path:"/finance/reports/conversions",service:crate::conversion_page::SERVICE,operation:"conversions",access:Access::Capability(crate::VIEW),framed:true}) }
inventory::submit! { Contribution::new(ROUTES,"finance/reports.conversion",provenance!(),&Route {company_scope:bilibi_module_registry::contributions::CompanyScope::Company,method:Method::Get,path:"/finance/reports/conversions/{report_id}",service:crate::conversion_page::SERVICE,operation:"conversion",access:Access::Capability(crate::VIEW),framed:true}) }
bilibi_menu::contribute!(
    "finance/reports",
    bilibi_menu::MenuItem::page(
        "finance.reports.conversions",
        "Conversion reports",
        "/finance/reports/conversions"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bookkeeping")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(6)
);
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES,"finance/reports.conversion-pages",provenance!(),&bilibi_module_registry::contributions::PageWrites {service:crate::conversion_page::SERVICE,contracts:&[("review_issue","finance.reports.conversion.issue"),("issue","finance.reports.conversion.issue"),("review_sign","finance.reports.conversion.sign"),("sign","finance.reports.conversion.sign")],pending:bilibi_module_registry::contributions::Pending::None}) }
