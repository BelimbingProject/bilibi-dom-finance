//! Permanent Core party links and issue-time facts for Receivables, mirroring
//! Bilimbi's customer seam without a second source of party identity or code.
use crate::{invalid, store, Customer, Receivables, Result};
use bilibi_agent_api::OperationClient;
use bilibi_company::RelationshipRole;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{PartyFacts, PartyLinks};
use bilibi_party::{
    CalendarDate, EffectiveDates, OrganisationId, PartyId, PersonId, RelationshipInput,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

pub(super) fn reference(row: &Value) -> Result<(&str, i64)> {
    Ok((
        row["record_kind"].as_str().ok_or(Error::Invalid)?,
        row["record_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::Invalid)?,
    ))
}
fn target_id(kind: &str, id: i64) -> Result<PartyId> {
    match kind {
        "company" => Ok(PartyId::Organisation(
            OrganisationId::new(id).map_err(|_| Error::Invalid)?,
        )),
        "person" => Ok(PartyId::Person(
            PersonId::new(id).map_err(|_| Error::Invalid)?,
        )),
        _ => Err(invalid("record_kind", "must be company or person")),
    }
}
fn organisation(id: i64) -> Result<OrganisationId> {
    OrganisationId::new(id).map_err(|_| Error::Invalid.into())
}
#[derive(serde::Serialize)]
pub(super) struct Target {
    pub kind: String,
    pub id: Option<i64>,
    pub party: Option<PartyFacts>,
    pub creation: Option<Value>,
    pub relationship: Value,
}
impl Receivables {
    pub(crate) async fn customer(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        mut row: Value,
    ) -> Result<Customer> {
        let (kind, id) = reference(&row)?;
        let party = PartyLinks::new(&self.parties)
            .summary(client, scope, kind, id)
            .await?;
        row["code"] = json!(party.code);
        row["name"] = json!(party.name);
        row["party"] = json!(party);
        Ok(store::decode(row)?)
    }
    pub(crate) async fn frozen_party(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        party: &PartyFacts,
        date: &CalendarDate,
    ) -> Result<Value> {
        let mut frozen = PartyLinks::new(&self.parties)
            .frozen_party(client, scope, &party.kind, party.id, "customer_id", true)
            .await?;
        let address = bilibi_address::billing_address(client, scope, party.party_id()?, date)
            .await
            .map_err(|e| match e {
                Error::Forbidden => invalid(
                    "customer_id",
                    "needs Addresses view permission to freeze the party's billing address",
                ),
                other => other.into(),
            })?
            .map(|a| a.address.address);
        for (field, value) in [
            ("line1", address.as_ref().and_then(|a| a.line1.clone())),
            ("line2", address.as_ref().and_then(|a| a.line2.clone())),
            ("line3", address.as_ref().and_then(|a| a.line3.clone())),
            (
                "locality",
                address.as_ref().and_then(|a| a.locality.clone()),
            ),
            (
                "postcode",
                address.as_ref().and_then(|a| a.postcode.clone()),
            ),
            (
                "admin1_code",
                address.as_ref().and_then(|a| a.admin1_code.clone()),
            ),
            (
                "country_iso",
                address.as_ref().and_then(|a| a.country_iso.clone()),
            ),
        ] {
            frozen[format!("party_{field}")] = json!(value);
        }
        Ok(frozen)
    }
    pub(crate) async fn invoiceable(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        source: i64,
        customer: &Customer,
    ) -> Result<()> {
        if !customer.active {
            return Err(invalid("customer_id", "is an inactive customer"));
        }
        if !customer.party.active() {
            return Err(invalid(
                "customer_id",
                match (customer.party.kind.as_str(), customer.party.status.as_str()) {
                    ("company", "pending") => {
                        "is a pending company; activate it in Companies before invoicing"
                    }
                    ("company", "suspended") => {
                        "is a suspended company; reactivate it in Companies before invoicing"
                    }
                    ("company", "archived") => "is an archived company",
                    ("person", "retired") => "is a retired person",
                    _ => "the linked Core party is not available",
                },
            ));
        }
        let role = self.customer_role(client, scope).await?;
        if self
            .current_customer_relationship(
                client,
                scope,
                source,
                &customer.party.kind,
                customer.party.id,
                &role,
            )
            .await?
            .is_none()
        {
            return Err(invalid(
                "customer_id",
                "has no current customer relationship from the current company; record it in Core before invoicing",
            ));
        }
        Ok(())
    }
    async fn customer_role(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
    ) -> Result<RelationshipRole> {
        bilibi_company::relationship_role_by_code(client, scope, "customer")
            .await?
            .ok_or_else(|| {
                invalid(
                    "base",
                    "add the customer relationship role in Companies before adding a customer",
                )
            })
    }
    async fn current_customer_relationship(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        source: i64,
        kind: &str,
        target: i64,
        role: &RelationshipRole,
    ) -> Result<Option<bilibi_company::Relationship>> {
        Ok(bilibi_company::current_relationship(
            client,
            scope,
            organisation(source)?,
            target_id(kind, target)?,
            role.id,
        )
        .await?)
    }
    pub(super) async fn prepare_target(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        source: i64,
        attrs: &Value,
    ) -> Result<Target> {
        let target = PartyLinks::new(&self.parties)
            .prepare_target(&self.records, client, scope, source, attrs)
            .await?;
        if let Some(id) = target.id {
            if client.query_opt("SELECT id FROM finance_receivables_customers WHERE tenant_id=$1 AND company_id=$2 AND record_kind=$3 AND record_id=$4",&[&scope.tenant(),&source,&target.kind,&id.to_string()]).await?.is_some(){return Err(invalid("record_id","is already a customer of the current company"));}
        }
        let role = self.customer_role(client, scope).await?;
        let current = match target.id {
            Some(id) => {
                self.current_customer_relationship(client, scope, source, &target.kind, id, &role)
                    .await?
            }
            None => None,
        };
        if current.is_none() {
            if role.retired() {
                return Err(invalid("base", "the customer relationship role is retired"));
            }
            self.authorize_on(client, scope, source, bilibi_company::UPDATE)
                .await?;
        }
        Ok(Target {
            kind: target.kind,
            id: target.id,
            party: target.party,
            creation: target.creation,
            relationship: json!({"source_company_id":source,"role":role,"current":current,"will_assign":current.is_none()}),
        })
    }
    pub(super) async fn compose_target(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        source: i64,
        attrs: &Value,
        data: &mut Value,
    ) -> Result<()> {
        let party = PartyLinks::new(&self.parties)
            .compose_target(&self.records, tx, scope, source, attrs)
            .await?;
        let role = self.customer_role(tx.sql(), scope).await?;
        if self
            .current_customer_relationship(tx.sql(), scope, source, &party.kind, party.id, &role)
            .await?
            .is_none()
        {
            let input = RelationshipInput {
                source_organisation_id: organisation(source)?,
                target: party.party_id()?,
                role_id: role.id,
                dates: EffectiveDates::new(None, None).map_err(|_| Error::Internal)?,
            };
            match bilibi_company::assign_relationship(tx, scope, &self.parties, &input)
                .await
                .map_err(|e| invalid("base", e.to_string()))?
            {
                bilibi_company::TypeOutcome::Saved(_) => {}
                bilibi_company::TypeOutcome::Refused(fields) => {
                    return Err(invalid(
                        "base",
                        fields
                            .into_iter()
                            .map(|f| f.message)
                            .collect::<Vec<_>>()
                            .join("; "),
                    ));
                }
            }
        }
        data["record_kind"] = json!(party.kind);
        data["record_id"] = json!(party.id.to_string());
        Ok(())
    }
}
