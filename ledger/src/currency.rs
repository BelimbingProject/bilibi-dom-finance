//! The book's currency against Core's currency list (`core/currency`), which
//! owns the ISO 4217 codes, names and decimal places. The ledger keeps only
//! the code and the decimal places the person confirmed.
//!
//! Two states of the list decide what a book may open with: *listed* (any
//! current currency; opening the book marks it used through Core's `check`,
//! so currency controls list it first from then on) and *unavailable* (the
//! catalog was never imported: a well-formed code is accepted *unchecked* and
//! the decimal places must be given, and the review says so).
use crate::FieldError;
use bilibi_agent_api::OperationClient;
use bilibi_currency::{normalize, Currencies, Currency, Unusable, BASE_KEY};
use bilibi_database::{Error, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tokio_postgres::Row;

struct Bound {
    currencies: Arc<Currencies>,
    settings: Arc<Settings>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();

pub(crate) fn bind(
    currencies: Arc<Currencies>,
    settings: Arc<Settings>,
) -> bilibi_module_registry::Result<()> {
    BOUND
        .set(Bound {
            currencies,
            settings,
        })
        .map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate ledger currency binding".into(),
            )
        })
}

fn bound() -> Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal)
}

/// What a code resolved to: a currency of the list, or `None` when the list
/// is unavailable and the code is taken as typed.
pub(crate) type Chosen = Option<Currency>;

fn refusal(why: Unusable) -> FieldError {
    FieldError::new("currency_code", why.message())
}

/// The currency for a code in the book's own write: Core's `check` accepts
/// any current currency and marks a first use in this transaction. When the
/// list was never imported, the code is taken unchecked.
pub(crate) async fn choose_on(
    tx: &mut bilibi_database::MutationTransaction<'_>,
    scope: &Scope,
    code: &str,
) -> Result<std::result::Result<Chosen, FieldError>> {
    if let Ok(currency) = bound()?.currencies.check(tx, scope, code).await? {
        return Ok(Ok(Some(currency)));
    }
    decide(tx.sql(), scope, code).await
}

/// How far the list can help: see the module notes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reference {
    Listed,
    Unavailable,
}
impl Reference {
    fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "iso4217",
            Self::Unavailable => "unavailable",
        }
    }
}

async fn reference(client: &dyn OperationClient) -> Result<Reference> {
    // `TODO(platform)`: Core offers no capability-free "is the catalog
    // installed" read; its `catalog` needs the view capability and a
    // `GenericClient`.
    let installed = client
        .query_opt("SELECT 1 FROM currencies LIMIT 1", &[])
        .await?
        .is_some();
    Ok(if installed {
        Reference::Listed
    } else {
        Reference::Unavailable
    })
}

// Core's `Currencies` reads take a `GenericClient`, which the handed
// `OperationClient` is not, and a pooled connection taken under the company
// lock can exhaust the pool. These reads run Core's own catalog query
// (`currencies` joined to the tenant's `currency_tenant_used` rows) on the
// connection the caller already holds. `TODO(platform)`: drop them once
// `Currencies` reads through the object-safe client.
const COLUMNS: &str =
    "c.code,c.numeric_code,c.name,c.common_names,c.minor_units,c.withdrawn,(u.code IS NOT NULL) AS used";
const FROM: &str =
    "FROM currencies c LEFT JOIN currency_tenant_used u ON u.tenant_id=$1 AND u.code=c.code";

fn listed(row: &Row) -> Currency {
    Currency {
        code: row.get("code"),
        numeric_code: row.get("numeric_code"),
        name: row.get("name"),
        common_names: row.get("common_names"),
        minor_units: row.get::<_, i16>("minor_units") as u32,
        withdrawn: row.get("withdrawn"),
        used: row.get("used"),
    }
}

async fn get(client: &dyn OperationClient, scope: &Scope, code: &str) -> Result<Option<Currency>> {
    let Some(code) = normalize(code) else {
        return Ok(None);
    };
    Ok(client
        .query_opt(
            &format!("SELECT {COLUMNS} {FROM} WHERE c.code=$2"),
            &[&scope.tenant(), &code],
        )
        .await?
        .as_ref()
        .map(listed))
}

/// Every current (not withdrawn) currency of the catalog, the ones the
/// tenant has used first, as Core's `current` orders them.
async fn current(client: &dyn OperationClient, scope: &Scope) -> Result<Vec<Currency>> {
    Ok(client
        .query(
            &format!(
                "SELECT {COLUMNS} {FROM} WHERE NOT c.withdrawn ORDER BY (u.code IS NULL), c.code"
            ),
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(listed)
        .collect())
}

/// The same answer for a review, which only reads, on the connection it
/// holds; the commit's [`choose_on`] marks the currency used.
pub(crate) async fn choose(
    client: &dyn OperationClient,
    scope: &Scope,
    code: &str,
) -> Result<std::result::Result<Chosen, FieldError>> {
    decide(client, scope, code).await
}

async fn decide(
    client: &dyn OperationClient,
    scope: &Scope,
    code: &str,
) -> Result<std::result::Result<Chosen, FieldError>> {
    if reference(client).await? == Reference::Unavailable {
        return Ok(Ok(None));
    }
    Ok(match get(client, scope, code).await? {
        Some(currency) if currency.withdrawn => Err(refusal(Unusable::Withdrawn)),
        Some(currency) => Ok(Some(currency)),
        None => Err(refusal(Unusable::Unknown)),
    })
}

/// The decimal places a book opens with when none are given, or the field
/// error when the list cannot say.
pub(crate) fn default_units(chosen: &Chosen) -> std::result::Result<i64, FieldError> {
    chosen
        .as_ref()
        .map(|currency| i64::from(currency.minor_units))
        .ok_or_else(|| {
            FieldError::new(
                "minor_units",
                "is required: the currency list is not available to suggest it",
            )
        })
}

/// The same facts for an opened book's currency, found by code even after ISO
/// withdrew it; `unchecked` when Core's list has no such code.
pub(crate) async fn describe(
    client: &dyn OperationClient,
    scope: &Scope,
    code: &str,
    units: i64,
) -> Result<Value> {
    let currency = get(client, scope, code).await?;
    Ok(facts(code, units, &currency))
}

/// What a review shows about the choice: the currency's name, its standard
/// decimal places, and whether the chosen places differ from them.
pub(crate) fn facts(code: &str, units: i64, chosen: &Chosen) -> Value {
    match chosen {
        Some(currency) => json!({
            "currency_code": code,
            "currency_name": currency.name,
            "minor_units": units,
            "standard_minor_units": currency.minor_units,
            "nonstandard_minor_units": units != i64::from(currency.minor_units),
            "reference": "iso4217",
        }),
        None => json!({
            "currency_code": code,
            "currency_name": null,
            "minor_units": units,
            "standard_minor_units": null,
            "nonstandard_minor_units": null,
            "reference": "unchecked",
        }),
    }
}

/// What the Book page and the discovery read offer: every current currency
/// that matches `search` (code or name), the used ones first, each with
/// whether the tenant has used it, and the company's base currency, else the
/// tenant's, as the proposal. Read on the connection the caller holds.
pub(crate) async fn options(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    search: &str,
) -> Result<Value> {
    let bound = bound()?;
    let reference = reference(client).await?;
    let offered = if reference == Reference::Listed {
        current(client, scope).await?
    } else {
        Vec::new()
    };
    let company = OrganisationId::new(company).map_err(|_| Error::Invalid)?;
    let own = base_currency(&bound.settings, client, scope, Target::Company(company)).await?;
    let (base, source) = match own {
        Some(own) => (Some(own), "company"),
        None => (
            base_currency(&bound.settings, client, scope, Target::Tenant).await?,
            "tenant",
        ),
    };
    let proposed = base.filter(|c| !c.withdrawn).map(|base| {
        json!({
            "code": base.code,
            "name": base.name,
            "minor_units": base.minor_units,
            "used": base.used,
            "source": source,
        })
    });
    let needle = search.trim().to_lowercase();
    let listed: Vec<Value> = offered
        .iter()
        .filter(|c| {
            needle.is_empty()
                || c.code.to_lowercase().contains(&needle)
                || c.name.to_lowercase().contains(&needle)
        })
        .take(300)
        .map(|c| json!({"code": c.code, "name": c.name, "minor_units": c.minor_units, "used": c.used}))
        .collect();
    Ok(json!({
        "reference": reference.as_str(),
        "proposed": proposed,
        "currencies": listed,
    }))
}

async fn base_currency(
    settings: &Settings,
    client: &dyn OperationClient,
    scope: &Scope,
    target: Target,
) -> Result<Option<Currency>> {
    let resolved = settings.get(client, scope, target, BASE_KEY).await?;
    match resolved.value.as_str() {
        Some(code) if resolved.overridden || matches!(target, Target::Tenant) => {
            get(client, scope, code).await
        }
        _ => Ok(None),
    }
}
