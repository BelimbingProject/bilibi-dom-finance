//! A customer row's actions menu: what the pages that accept its Core company
//! or contact offer (`bilibi_records::drop_target!`), such as New invoice,
//! each declared by the module that owns the action. Picking one opens that
//! page's own review with the customer chosen; the menu commits nothing.
use super::render;
use crate::Customer;
use bilibi_database::Result;
use bilibi_records::{AddTo, Records, Reference};
use bilibi_tenancy::Scope;
use bilibi_ui::{AddToItem, AddToMenu, FlexRow};
use std::collections::BTreeMap;

/// The record kinds a customer's party can be (`PartyFacts::kind`).
const KINDS: [&str; 2] = ["company", "person"];

/// The actions this person may take on a customer, by the kind of its party.
/// "Add as customer" is left out: every row here already is one.
#[derive(Default)]
pub(super) struct Actions(BTreeMap<&'static str, Vec<AddTo>>);

impl Actions {
    pub(super) async fn read(records: &Records, scope: &Scope) -> Result<Self> {
        let mut by_kind = BTreeMap::new();
        for kind in KINDS {
            let mut offered = records.add_to(scope, kind).await?;
            offered.retain(|target| target.id != super::CUSTOMER_TARGET);
            by_kind.insert(kind, offered);
        }
        Ok(Self(by_kind))
    }

    /// Whether any row may show a menu, so the table draws its actions column.
    pub(super) fn any(&self) -> bool {
        self.0.values().any(|offered| !offered.is_empty())
    }

    /// Puts the customer's menu on its row and lets the row be dragged as its
    /// party, when anything accepts that kind.
    pub(super) fn attach(&self, customer: &Customer, row: &mut FlexRow) -> Result<()> {
        let Some(offered) = self
            .0
            .get(customer.party.kind.as_str())
            .filter(|offered| !offered.is_empty())
        else {
            return Ok(());
        };
        let party = Reference::new(&customer.party.kind, customer.party.id.to_string());
        row.record = Some((party.kind.clone(), party.id.clone()));
        row.actions = render(AddToMenu::new(
            format!("receivables-customer-{}-actions", customer.id),
            format!("Actions for {}", customer.name),
            offered
                .iter()
                .map(|target| AddToItem {
                    label: target.label.into(),
                    href: target.href(std::slice::from_ref(&party)),
                    disabled: None,
                })
                .collect(),
        ))?;
        Ok(())
    }
}
