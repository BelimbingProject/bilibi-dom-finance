//! Bilimbi Receivables.Contributions, capability keys verbatim.
use crate::MODULE;
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{Contribution, Provenance};

pub const VIEW: &str = "finance.receivables.view";
pub const MANAGE_SETUP: &str = "finance.receivables.setup.manage";
pub const MANAGE_CUSTOMERS: &str = "finance.receivables.customers.manage";
pub const MANAGE_DOCUMENTS: &str = "finance.receivables.documents.manage";
pub const ISSUE: &str = "finance.receivables.documents.issue";
pub const POST_PAYMENTS: &str = "finance.receivables.payments.post";
pub const SUBMIT_WRITE_OFFS: &str = "finance.receivables.write-offs.submit";
pub const APPROVE_WRITE_OFFS: &str = "finance.receivables.write-offs.approve";
/// The workflow-only sales role: a person who prepares customers and draft
/// invoices without ledger access. It narrows what the person may do; it does
/// not hide any record from them (see `ledger/appointed-auditor-and-sales-access.md`).
pub const SALES_WORKFLOW: &str = "sales_workflow";
pub const CAPABILITIES: [&str; 8] = [
    VIEW,
    MANAGE_SETUP,
    MANAGE_CUSTOMERS,
    MANAGE_DOCUMENTS,
    ISSUE,
    POST_PAYMENTS,
    SUBMIT_WRITE_OFFS,
    APPROVE_WRITE_OFFS,
];

inventory::submit! { Contribution::new(VOCABULARY, MODULE, Provenance { module: MODULE, source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["finance"], verbs: &["issue"],
    capabilities: &[C::company(VIEW), C::tenant(MANAGE_SETUP), C::company(MANAGE_CUSTOMERS), C::company(MANAGE_DOCUMENTS), C::tenant(ISSUE), C::tenant(POST_PAYMENTS), C::tenant(SUBMIT_WRITE_OFFS), C::tenant(APPROVE_WRITE_OFFS)],
    roles: &[RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &CAPABILITIES },
        RoleDefinition { code: "appointed_auditor", name: "Appointed auditor", description: "Read-only access to one company's ledger, reports and source evidence for a named audit engagement.", grant_all: false, capabilities: &[VIEW] },
        RoleDefinition { code: SALES_WORKFLOW, name: "Sales workflow", description: "Customers and draft invoices only: no ledger, reports, receipts, issuing, payables or payroll. A workflow boundary, not strict data segregation.", grant_all: false, capabilities: &[VIEW, MANAGE_CUSTOMERS, MANAGE_DOCUMENTS] }],
    system_principals: &[],
}) }
