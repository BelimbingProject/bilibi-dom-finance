//! Core party facts and creation shared by Finance customers and suppliers.
//! Mirrors Bilimbi Finance's party seam, with Core Company/Person as owners.
use crate::{FieldError, LedgerError, Result};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_party::PersonInput;
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_records::Records;
use bilibi_tenancy::{PartyIdentity, PartyProofs, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyFacts {
    pub kind: String,
    pub id: i64,
    /// The saved Core code, never a Finance-owned code or a current-name slug.
    pub code: String,
    pub name: String,
    pub status: String,
    pub path: String,
    pub tax_id: Option<String>,
    pub registration_number: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub restricted: Vec<String>,
}
impl PartyFacts {
    pub fn active(&self) -> bool {
        self.status == "active"
    }
    pub fn reference(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
    pub fn party_id(&self) -> Result<bilibi_party::PartyId> {
        party_id(&self.kind, self.id)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PartyTarget {
    pub kind: String,
    pub id: Option<i64>,
    pub party: Option<PartyFacts>,
    pub creation: Option<Value>,
}

pub struct PartyLinks<'a> {
    parties: &'a PartyProofs,
}
fn invalid(field: &'static str, message: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(vec![FieldError::new(field, message)])
}
fn party_id(kind: &str, id: i64) -> Result<bilibi_party::PartyId> {
    match kind {
        "company" => Ok(bilibi_party::PartyId::Organisation(
            OrganisationId::new(id).map_err(|_| invalid("record_id", "is invalid"))?,
        )),
        "person" => Ok(bilibi_party::PartyId::Person(
            PersonId::new(id).map_err(|_| invalid("record_id", "is invalid"))?,
        )),
        _ => Err(invalid("record_kind", "must be company or person")),
    }
}
fn prefixed(prefix: &'static str, field: &str) -> &'static str {
    match (prefix, field) {
        ("new_person", "name") => "new_person.name",
        ("new_person", "kind") => "new_person.kind",
        ("new_person", "email") => "new_person.email",
        ("new_person", "phone") => "new_person.phone",
        ("new_company", "name") => "new_company.name",
        ("new_company", "code") => "new_company.code",
        ("new_company", "legal_name") => "new_company.legal_name",
        ("new_company", "status") => "new_company.status",
        ("new_company", "registration_number") => "new_company.registration_number",
        ("new_company", "tax_id") => "new_company.tax_id",
        ("new_company", "email") => "new_company.email",
        ("new_company", "phone") => "new_company.phone",
        ("new_company", "website") => "new_company.website",
        ("new_company", "jurisdiction") => "new_company.jurisdiction",
        ("new_company", "create_anyway") => "new_company.create_anyway",
        _ => prefix,
    }
}

fn person_error(error: bilibi_person::WriteError) -> LedgerError {
    match error {
        bilibi_person::WriteError::Validation(fields) => LedgerError::Invalid(
            fields
                .into_iter()
                .map(|f| FieldError::new(prefixed("new_person", f.field), f.message))
                .collect(),
        ),
        bilibi_person::WriteError::Backend(Error::Forbidden) => {
            invalid("new_person", "needs People create permission")
        }
        bilibi_person::WriteError::Backend(error) => error.into(),
        other => invalid("new_person", other.to_string()),
    }
}
fn company_fields(fields: Vec<bilibi_company::FieldError>) -> LedgerError {
    LedgerError::Invalid(
        fields
            .into_iter()
            .map(|f| FieldError::new(prefixed("new_company", &f.field), f.message))
            .collect(),
    )
}
impl<'a> PartyLinks<'a> {
    pub fn new(parties: &'a PartyProofs) -> Self {
        Self { parties }
    }

    pub async fn summary(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        kind: &str,
        id: i64,
    ) -> Result<PartyFacts> {
        Ok(self.read(client, scope, kind, id).await?.0)
    }

    /// The summary, and whether the reader may see the party's record at all:
    /// without that view every fact is withheld, which a freeze names as the
    /// missing permission rather than as a restricted fact.
    async fn read(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        kind: &str,
        id: i64,
    ) -> Result<(PartyFacts, bool)> {
        let mut seen = true;
        let identity = self
            .parties
            .party_identity(client, scope, party_id(kind, id)?)
            .await
            .map_err(|e| match e {
                Error::NotFound => invalid("record_id", "the linked Core party is not available"),
                other => other.into(),
            })?;
        let mut facts = PartyFacts {
            kind: kind.into(),
            id,
            code: identity.code,
            name: identity.name,
            status: identity.status,
            path: format!(
                "/{}/{id}",
                if kind == "person" {
                    "contacts"
                } else {
                    "companies"
                }
            ),
            tax_id: None,
            registration_number: None,
            email: None,
            phone: None,
            restricted: Vec::new(),
        };
        if kind == "company" {
            match bilibi_company::detail(client, scope, id).await {
                Ok((company, masked)) => {
                    for (key, value) in [
                        ("tax_id", Some(company.tax_id)),
                        ("registration_number", company.registration_number),
                        ("email", Some(company.email)),
                        ("phone", Some(company.phone)),
                    ] {
                        if masked.contains(key) {
                            facts.restricted.push(key.into());
                            continue;
                        }
                        let value =
                            value.and_then(|value| bilibi_ui::params::blank_to_none(Some(&value)));
                        match key {
                            "tax_id" => facts.tax_id = value,
                            "registration_number" => facts.registration_number = value,
                            "email" => facts.email = value,
                            _ => facts.phone = value,
                        }
                    }
                }
                Err(error @ (Error::Forbidden | Error::NotFound)) => {
                    seen = !matches!(error, Error::Forbidden);
                    facts.restricted = ["tax_id", "registration_number", "email", "phone"]
                        .map(String::from)
                        .into();
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            match bilibi_person::read_person(
                client,
                scope,
                PersonId::new(id).map_err(|_| Error::Invalid)?,
            )
            .await
            {
                Ok(person) => {
                    facts.email = person.email;
                    facts.phone = person.phone;
                    facts.restricted = person.restricted;
                    // A partial personal value is not an authentic document
                    // fact. Finance treats it as withheld, including in summaries.
                    for field in person.masked {
                        match field.as_str() {
                            "email" => facts.email = None,
                            "phone" => facts.phone = None,
                            _ => {}
                        }
                        facts.restricted.push(field);
                    }
                }
                Err(error @ (Error::Forbidden | Error::NotFound)) => {
                    seen = !matches!(error, Error::Forbidden);
                    facts.restricted = ["email", "phone"].map(String::from).into();
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok((facts, seen))
    }

    /// Core identities for exact stored codes, in one call for the whole set.
    /// A blank or unusable code matches nothing; the caller keeps each result's
    /// kind and id together and reports an unmatched code against its own row.
    pub async fn by_codes(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        codes: &HashSet<String>,
    ) -> Result<Vec<PartyIdentity>> {
        let codes: Vec<String> = codes
            .iter()
            .filter(|c| !c.is_empty() && c.chars().count() <= 255 && !c.contains('\0'))
            .cloned()
            .collect();
        Ok(self.parties.parties_by_codes(client, scope, &codes).await?)
    }

    /// Exactly one permanent party reference or one requested Core creation.
    /// Base Records proves the reference on the caller's connection.
    pub async fn prepare_target(
        &self,
        records: &Records,
        client: &dyn OperationClient,
        scope: &Scope,
        source: i64,
        attrs: &Value,
    ) -> Result<PartyTarget> {
        let company = !attrs["new_company"].is_null();
        let person = !attrs["new_person"].is_null();
        let existing = !attrs["record_id"].is_null() || !attrs["record_kind"].is_null();
        if usize::from(company) + usize::from(person) + usize::from(existing) != 1 {
            return Err(invalid(
                "record_id",
                "choose one existing company or person, or create one",
            ));
        }
        if company {
            let new: bilibi_company::NewCompany =
                serde_json::from_value(attrs["new_company"].clone())
                    .map_err(|_| invalid("new_company", "is invalid"))?;
            let preview = bilibi_company::preview_create(client, scope, &new)
                .await
                .map_err(|error| match error {
                    Error::Forbidden => invalid("new_company", "needs Companies create permission"),
                    other => other.into(),
                })?
                .map_err(company_fields)?;
            return Ok(PartyTarget {
                kind: "company".into(),
                id: None,
                party: None,
                creation: Some(
                    json!({"name":preview.company.name,"code":preview.company.code,"status":preview.company.status,"duplicate_candidates":preview.matches.candidates,"duplicates_likely":preview.matches.likely}),
                ),
            });
        }
        if person {
            let new: PersonInput = serde_json::from_value(attrs["new_person"].clone())
                .map_err(|_| invalid("new_person", "is invalid"))?;
            let preview = bilibi_person::preview_person(client, scope, &new)
                .await
                .map_err(person_error)?;
            return Ok(PartyTarget {
                kind: "person".into(),
                id: None,
                party: None,
                creation: Some(
                    json!({"name":preview.identity.name,"code":preview.code,"status":"active"}),
                ),
            });
        }
        let kind = attrs["record_kind"]
            .as_str()
            .ok_or_else(|| invalid("record_kind", "must be company or person"))?;
        let id = attrs["record_id"]
            .as_i64()
            .or_else(|| attrs["record_id"].as_str().and_then(|s| s.parse().ok()))
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("record_id", "is invalid"))?;
        party_id(kind, id)?;
        if kind == "company" && id == source {
            return Err(invalid("record_id", "cannot be the current company itself"));
        }
        records
            .check(client, scope, kind, &id.to_string())
            .await?
            .map_err(|e| invalid("record_id", e.message()))?;
        let facts = self.summary(client, scope, kind, id).await?;
        if matches!(facts.status.as_str(), "archived" | "retired") {
            return Err(invalid(
                "record_id",
                format!(
                    "is {} {}",
                    if facts.status == "archived" {
                        "an archived"
                    } else {
                        "a retired"
                    },
                    if kind == "company" {
                        "company"
                    } else {
                        "person"
                    }
                ),
            ));
        }
        Ok(PartyTarget {
            kind: kind.into(),
            id: Some(id),
            party: Some(facts),
            creation: None,
        })
    }

    /// Core creation and the Finance record share the audited transaction.
    pub async fn compose_target(
        &self,
        records: &Records,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        source: i64,
        attrs: &Value,
    ) -> Result<PartyFacts> {
        let target = self
            .prepare_target(records, tx.sql(), scope, source, attrs)
            .await?;
        let id = if let Some(id) = target.id {
            id
        } else if target.kind == "person" {
            let input: PersonInput = serde_json::from_value(attrs["new_person"].clone())
                .map_err(|_| invalid("new_person", "is invalid"))?;
            bilibi_person::create_person(tx, scope, &input)
                .await
                .map_err(person_error)?
                .id
                .get()
        } else {
            let input: bilibi_company::NewCompany =
                serde_json::from_value(attrs["new_company"].clone())
                    .map_err(|_| invalid("new_company", "is invalid"))?;
            match bilibi_company::create(tx, scope, &input).await? {
                bilibi_company::CreateOutcome::Created(company) => company.id,
                bilibi_company::CreateOutcome::Refused(fields) => {
                    return Err(company_fields(fields));
                }
            }
        };
        records
            .check(tx.sql(), scope, &target.kind, &id.to_string())
            .await?
            .map_err(|e| invalid("record_id", e.message()))?;
        self.summary(tx.sql(), scope, &target.kind, id).await
    }

    /// Core's business role is recorded in the same Finance creation transaction.
    pub async fn relationship(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        source: i64,
        kind: &str,
        id: Option<i64>,
        role_code: &'static str,
    ) -> Result<Value> {
        let role = bilibi_company::relationship_role_by_code(client, scope, role_code)
            .await
            .map_err(|e| match e {
                Error::Forbidden => invalid(
                    "base",
                    "needs Companies view permission to read relationship roles",
                ),
                other => other.into(),
            })?
            .ok_or_else(|| {
                invalid(
                    "base",
                    format!("add the {role_code} relationship role in Companies first"),
                )
            })?;
        let current = match id {
            Some(id) => {
                bilibi_company::current_relationship(
                    client,
                    scope,
                    OrganisationId::new(source).map_err(|_| Error::Invalid)?,
                    party_id(kind, id)?,
                    role.id,
                )
                .await?
            }
            None => None,
        };
        if current.is_none() {
            if role.retired() {
                return Err(invalid("base", "the relationship role is retired"));
            }
            self.parties
                .organisation_target(
                    client,
                    scope,
                    OrganisationId::new(source).map_err(|_| Error::Invalid)?,
                    bilibi_company::UPDATE,
                )
                .await
                .map_err(|e| match e {
                    Error::Forbidden => invalid(
                        "base",
                        "needs Companies update permission to record the relationship",
                    ),
                    other => other.into(),
                })?;
        }
        Ok(json!({"role":role,"current":current,"will_assign":current.is_none()}))
    }
    pub async fn assign_relationship(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        source: i64,
        party: &PartyFacts,
        role_code: &'static str,
    ) -> Result<()> {
        let review = self
            .relationship(
                tx.sql(),
                scope,
                source,
                &party.kind,
                Some(party.id),
                role_code,
            )
            .await?;
        if review["will_assign"] == true {
            let input = bilibi_party::RelationshipInput {
                source_organisation_id: OrganisationId::new(source).map_err(|_| Error::Invalid)?,
                target: party.party_id()?,
                role_id: bilibi_party_ids::RelationshipRoleId::new(
                    review["role"]["id"].as_i64().ok_or(Error::Invalid)?,
                )
                .map_err(|_| Error::Invalid)?,
                dates: bilibi_party::EffectiveDates::new(None, None).map_err(|_| Error::Invalid)?,
            };
            match bilibi_company::assign_relationship(tx, scope, self.parties, &input)
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
        Ok(())
    }

    /// Facts frozen on a document, with the saved Core code, read under the
    /// issuer's own Core permissions. A fact the issuer may not see is refused,
    /// naming the missing view or the hidden fact, instead of being written as
    /// an absent or masked fact.
    pub async fn frozen_party(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        kind: &str,
        id: i64,
        field: &'static str,
        include_contacts: bool,
    ) -> Result<Value> {
        let (facts, seen) = self.read(client, scope, kind, id).await?;
        let required = if include_contacts {
            &["tax_id", "registration_number", "email", "phone"][..]
        } else {
            &["tax_id", "registration_number"][..]
        };
        if let Some((_, label)) = [
            ("tax_id", "tax ID"),
            ("registration_number", "registration number"),
            ("email", "email"),
            ("phone", "phone"),
        ]
        .iter()
        .find(|(key, _)| required.contains(key) && facts.restricted.iter().any(|f| f == key))
        {
            let (who, view) = if kind == "company" {
                ("company", "Companies")
            } else {
                ("person", "People")
            };
            return Err(invalid(
                field,
                if seen {
                    format!("the {who}'s {label} is restricted for you, so it cannot be copied onto the document; ask an administrator to lift the restriction")
                } else {
                    format!("needs {view} view permission to copy the {who}'s {label} onto the document")
                },
            ));
        }
        let name = if kind == "company" {
            let (company, masked) = bilibi_company::detail(client, scope, id).await?;
            if masked.contains("legal_name") || masked.contains("name") {
                return Err(invalid(field, "the party's name is restricted"));
            }
            company.display_name().to_owned()
        } else {
            facts.name
        };
        Ok(
            json!({"party_code":facts.code,"party_name":name,"party_tax_identifier":facts.tax_id,"party_registration_identifier":facts.registration_number,"party_email":if include_contacts {facts.email} else {None},"party_phone":if include_contacts {facts.phone} else {None}}),
        )
    }
}
