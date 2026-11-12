//! Which accounts are linked to a bank account. The ledger knows no account
//! as "bank"; a module that does (`finance/reconciliation`) contributes a
//! reader here, so a sibling's page can list those accounts first ("Paid
//! from", "Deposited to") without depending on that module. It is an order
//! for a list of choices only: no posting rule reads it.
use crate::{Account, AccountType, MODULE};
use bilibi_agent_api::OperationClient;
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use std::{future::Future, pin::Pin, sync::OnceLock};

/// The catalog key readers are contributed to.
pub const BANK_LINKS: &str = "finance/ledger.bank_links";

pub type BankLinksFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<i64>, bilibi_database::Error>> + Send + 'a>>;

/// Reads, on the connection it is handed, the ledger accounts a company's
/// active bank accounts are linked to. The ledger authorizes the caller
/// (`finance.ledger.view`) before it asks.
pub trait BankLinks: Send + Sync {
    fn account_ids<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        tenant: i64,
        company_id: i64,
    ) -> BankLinksFuture<'a>;
}

/// The contribution payload: `Contribution::new(BANK_LINKS, "<module id>/bank_accounts", provenance, &BankLinksDefinition { links: &Linked })`.
pub struct BankLinksDefinition {
    pub links: &'static dyn BankLinks,
}

fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        entry.get::<BankLinksDefinition>()?;
        if !entry.id.starts_with(entry.provenance.module) {
            return Err(RegistryError::Contribution(format!(
                "invalid bank links reader {} at {}:{}",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<BankLinksDefinition>(BANK_LINKS, MODULE, validate) }

static READERS: OnceLock<Vec<&'static dyn BankLinks>> = OnceLock::new();

pub(crate) fn bind() -> bilibi_module_registry::Result<()> {
    let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
    let mut readers = Vec::new();
    for entry in catalog.entries::<BankLinksDefinition>(BANK_LINKS)? {
        readers.push(entry.get::<BankLinksDefinition>()?.links);
    }
    READERS
        .set(readers)
        .map_err(|_| RegistryError::Service("duplicate ledger bank links binding".into()))
}

/// Every mounted reader's accounts; none when no module links bank accounts.
pub(crate) async fn account_ids(
    client: &dyn OperationClient,
    tenant: i64,
    company_id: i64,
) -> Result<Vec<i64>, bilibi_database::Error> {
    let mut ids = Vec::new();
    for reader in READERS.get().into_iter().flatten() {
        ids.extend(reader.account_ids(client, tenant, company_id).await?);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// The order a "Paid from" or "Deposited to" list offers accounts in: those
/// linked to a bank account, then other asset accounts, then the rest, each
/// part keeping the order it came in. It drops nothing.
pub fn money_first(mut accounts: Vec<Account>, bank_linked: &[i64]) -> Vec<Account> {
    accounts.sort_by_key(|a| money_rank(bank_linked.contains(&a.id), a.account_type));
    accounts
}

fn money_rank(bank_linked: bool, account_type: AccountType) -> u8 {
    match (bank_linked, account_type) {
        (true, _) => 0,
        (false, AccountType::Asset) => 1,
        _ => 2,
    }
}
