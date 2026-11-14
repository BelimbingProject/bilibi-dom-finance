//! What the module adds to the shared catalogs (Bilimbi
//! `Bilimbi.Finance.Einvoice.Contributions`): the `finance` authorization
//! domain, the five capability keys verbatim and the `tenant_owner` role
//! holding all of them. Settings are in `configuration.rs`. Menu items arrive
//! with the pages that own them.
use crate::MODULE;
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{Contribution, Provenance};

/// The capability every e-invoice read asks for.
pub const VIEW: &str = "finance.einvoice.view";
/// Issues a fiscal document.
pub const ISSUE: &str = "finance.einvoice.documents.issue";
/// Submits a document to the authority and asks for its status.
pub const SUBMIT: &str = "finance.einvoice.documents.submit";
/// Cancels a validated document.
pub const CANCEL: &str = "finance.einvoice.documents.cancel";
/// Edits the e-invoice settings, the code overrides and tests the connection.
pub const SETTINGS: &str = "finance.einvoice.settings.manage";

pub const CAPABILITIES: [&str; 5] = [VIEW, ISSUE, SUBMIT, CANCEL, SETTINGS];

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

// Bilimbi relies on Base for the verbs `view` and `manage`; Bilibi merges
// verbs across modules, so `issue` and `cancel` are declared here, and
// `submit`, which the ledger also declares, so the module needs no sibling to
// have declared it. The keys are unchanged.
inventory::submit! { Contribution::new(VOCABULARY, "finance/einvoice", provenance!(), &VocabularyContribution {
    domains: &["finance"],
    verbs: &["issue", "submit", "cancel"],
    capabilities: &[C::company(VIEW), C::tenant(ISSUE), C::tenant(SUBMIT), C::tenant(CANCEL), C::tenant(SETTINGS)],
    roles: &[RoleDefinition {
        code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly",
        grant_all: false, capabilities: &CAPABILITIES,
    }, RoleDefinition {
        code: "appointed_auditor", name: "Appointed auditor",
        description: "Read-only access to one company's ledger, reports and source evidence for a named audit engagement.",
        grant_all: false, capabilities: &[VIEW],
    }],
    system_principals: &[],
}) }

// Bilimbi Einvoice priv/web_routes.exs and Contributions.menu/0.
use bilibi_module_registry::contributions::{Access, Method, Route, ROUTES};
macro_rules! page {
    ($operation:literal, $path:literal, $label:literal, $capability:expr, $icon:literal, $order:expr) => {
        inventory::submit! { Contribution::new(ROUTES, concat!("finance/einvoice.", $operation), provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
            method: Method::Get, path: $path, service: crate::web::control::SERVICE,
            operation: $operation, access: Access::Capability($capability), framed: true,
        }) }
        bilibi_menu::contribute!("finance/einvoice",
            bilibi_menu::MenuItem::page(concat!("finance.einvoice.", $operation), $label, $path).company_scoped()
                .under("finance.group.einvoicing").icon($icon)
                .requires(bilibi_menu::Requirement::Capability($capability)).order($order)
        );
    };
}
page!(
    "consolidation",
    "/finance/einvoice/consolidation",
    "E-invoice consolidation",
    VIEW,
    "vaadin-invoice",
    2
);
page!(
    "reconciliation",
    "/finance/einvoice/reconciliation",
    "E-invoices not in the books",
    VIEW,
    "vaadin-invoice",
    3
);
page!(
    "setup",
    "/finance/einvoice/setup",
    "E-invoice setup",
    SETTINGS,
    "vaadin-invoice",
    4
);
