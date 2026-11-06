//! The book's currency against Core's currency list (`core/currency`), which
//! owns the ISO 4217 codes, names and decimal places. The ledger keeps only
//! the code and the decimal places the person confirmed.
//!
//! Three states of the list decide what a book may open with: *listed* (the
//! tenant enabled some currencies, or the person may enable one: a person who
//! holds `admin.currency.manage` may choose any current currency and opening
//! the book enables it), *none enabled* (the catalog is installed, nothing is
//! enabled and the person may not enable one: no book opens until someone
//! does) and *unavailable* (the catalog was never imported: a well-formed code
//! is accepted *unchecked* and the decimal places must be given, and the
//! review says so).
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

/// The currency for a code on the write's own transaction. The enabled row is
/// locked until commit, so a concurrent disable waits for the book. `enable`
/// accepts a current currency the tenant has not enabled when the person may
/// enable it; the commit enables it with [`enable_on`] first.
pub(crate) async fn choose_on(
    tx: &bilibi_database::MutationTransaction<'_>,
    scope: &Scope,
    code: &str,
    enable: bool,
) -> Result<std::result::Result<Chosen, FieldError>> {
    if let Ok(currency) = bound()?.currencies.check(tx, scope, code).await? {
        return Ok(Ok(Some(currency)));
    }
    decide(tx.sql(), scope, code, enable).await
}

/// Enables the chosen currency for the tenant through Core's own write, in
/// the book's transaction, so the book and the switch commit together. Only
/// for a person [`can_enable`] admits: Core refuses anyone else as forbidden.
async fn enable_on(
    tx: &mut bilibi_database::MutationTransaction<'_>,
    scope: &Scope,
    code: &str,
) -> Result<std::result::Result<(), FieldError>> {
    Ok(
        match bound()?
            .currencies
            .set_enabled(tx, scope, code, true)
            .await?
        {
            Ok(_) => Ok(()),
            Err(errors) => Err(FieldError::new(
                "currency_code",
                errors
                    .first()
                    .map_or("cannot be enabled", |e| e.message.as_str()),
            )),
        },
    )
}

/// [`choose_on`] inside a write, else [`choose`], with `book.open`'s
/// `enable_currency`.
pub(crate) async fn choose_for(
    client: &dyn OperationClient,
    tx: Option<&bilibi_database::MutationTransaction<'_>>,
    scope: &Scope,
    code: &str,
    input: &Value,
) -> Result<std::result::Result<Chosen, FieldError>> {
    let enable = input["enable_currency"].as_bool().unwrap_or(false);
    match tx {
        Some(tx) => choose_on(tx, scope, code, enable).await,
        None => choose(client, scope, code, enable).await,
    }
}

/// Enables the currency when the commit's own review admitted it
/// (`enables_currency`: asked for, not enabled, and the person may).
pub(crate) async fn enable_reviewed(
    tx: &mut bilibi_database::MutationTransaction<'_>,
    scope: &Scope,
    code: &str,
    review: &Value,
) -> Result<std::result::Result<(), FieldError>> {
    if review["changes"]["after"]["currency"]["enables_currency"].as_bool() != Some(true) {
        return Ok(Ok(()));
    }
    enable_on(tx, scope, code).await
}

/// Whether the person may enable a currency (`admin.currency.manage`), asked
/// without refusing, so a page and a review can offer the choice.
pub(crate) async fn can_enable(client: &dyn OperationClient, scope: &Scope) -> Result<bool> {
    Ok(
        bilibi_authz::decide_proof(client, scope, bilibi_currency::MANAGE)
            .await?
            .allowed,
    )
}

/// How far the list can help: see the module notes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reference {
    Listed,
    NoneEnabled,
    Unavailable,
}
impl Reference {
    fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "iso4217",
            Self::NoneEnabled => "none_enabled",
            Self::Unavailable => "unavailable",
        }
    }
}

fn none_enabled() -> FieldError {
    FieldError::new(
        "currency_code",
        "has nothing to choose from: no currency is enabled yet; enable one on the Currencies page",
    )
}

async fn reference(client: &dyn OperationClient, enabled: &[Currency]) -> Result<Reference> {
    if !enabled.is_empty() {
        return Ok(Reference::Listed);
    }
    // `TODO(platform)`: Core offers no capability-free "is the catalog
    // installed" read; its `catalog` needs the view capability and a
    // `GenericClient`.
    let installed = client
        .query_opt("SELECT 1 FROM currencies LIMIT 1", &[])
        .await?
        .is_some();
    Ok(if installed {
        Reference::NoneEnabled
    } else {
        Reference::Unavailable
    })
}

// Core's `Currencies` reads take a `GenericClient`, which the handed
// `OperationClient` is not, and a pooled connection taken under the company
// lock can exhaust the pool. These reads run Core's own catalog query
// (`currencies` joined to the tenant's `currency_tenant_enabled` rows) on the
// connection the caller already holds. `TODO(platform)`: drop them once
// `Currencies` reads through the object-safe client.
const COLUMNS: &str =
    "c.code,c.numeric_code,c.name,c.common_names,c.minor_units,c.withdrawn,(e.code IS NOT NULL) AS enabled";
const FROM: &str =
    "FROM currencies c LEFT JOIN currency_tenant_enabled e ON e.tenant_id=$1 AND e.code=c.code";

fn listed(row: &Row) -> Currency {
    Currency {
        code: row.get("code"),
        numeric_code: row.get("numeric_code"),
        name: row.get("name"),
        common_names: row.get("common_names"),
        minor_units: row.get::<_, i16>("minor_units") as u32,
        withdrawn: row.get("withdrawn"),
        enabled: row.get("enabled"),
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

async fn enabled(client: &dyn OperationClient, scope: &Scope) -> Result<Vec<Currency>> {
    Ok(client
        .query(
            &format!("SELECT {COLUMNS} {FROM} WHERE e.code IS NOT NULL AND NOT c.withdrawn ORDER BY c.code"),
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(listed)
        .collect())
}

/// Every current (not withdrawn) currency of the catalog, enabled or not.
async fn current(client: &dyn OperationClient, scope: &Scope) -> Result<Vec<Currency>> {
    Ok(client
        .query(
            &format!("SELECT {COLUMNS} {FROM} WHERE NOT c.withdrawn ORDER BY c.code"),
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(listed)
        .collect())
}

/// The same answer for a review, which only reads, on the connection it holds.
pub(crate) async fn choose(
    client: &dyn OperationClient,
    scope: &Scope,
    code: &str,
    enable: bool,
) -> Result<std::result::Result<Chosen, FieldError>> {
    decide(client, scope, code, enable).await
}

async fn decide(
    client: &dyn OperationClient,
    scope: &Scope,
    code: &str,
    enable: bool,
) -> Result<std::result::Result<Chosen, FieldError>> {
    let enabled = enabled(client, scope).await?;
    let reference = reference(client, &enabled).await?;
    if reference == Reference::Unavailable {
        return Ok(Ok(None));
    }
    Ok(match get(client, scope, code).await? {
        Some(currency) if currency.withdrawn => Err(refusal(Unusable::Withdrawn)),
        Some(currency) if currency.enabled => Ok(Some(currency)),
        Some(currency) if enable && can_enable(client, scope).await? => Ok(Some(currency)),
        _ if reference == Reference::NoneEnabled => Err(none_enabled()),
        Some(_) => Err(refusal(Unusable::NotEnabled)),
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

/// The same facts for an opened book's currency, found by code whether or not
/// it is still enabled; `unchecked` when Core's list has no such code.
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
            "enables_currency": !currency.enabled,
            "reference": "iso4217",
        }),
        None => json!({
            "currency_code": code,
            "currency_name": null,
            "minor_units": units,
            "standard_minor_units": null,
            "nonstandard_minor_units": null,
            "enables_currency": false,
            "reference": "unchecked",
        }),
    }
}

/// What the Book page and the discovery read offer: the currencies that match
/// `search` (code or name), each with whether the tenant enabled it, and the
/// company's base currency, else the tenant's, as the proposal. A person who
/// may enable a currency is offered every current one; anyone else only the
/// enabled ones. Read on the connection the caller holds.
pub(crate) async fn options(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    search: &str,
) -> Result<Value> {
    let bound = bound()?;
    let enabled_only = enabled(client, scope).await?;
    let mut reference = reference(client, &enabled_only).await?;
    let may_enable = reference != Reference::Unavailable && can_enable(client, scope).await?;
    if may_enable {
        reference = Reference::Listed;
    }
    let offered = if may_enable {
        current(client, scope).await?
    } else {
        enabled_only
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
    let proposed = base
        .filter(|c| !c.withdrawn && (c.enabled || may_enable))
        .map(|base| {
            json!({
                "code": base.code,
                "name": base.name,
                "minor_units": base.minor_units,
                "enabled": base.enabled,
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
        .map(|c| json!({"code": c.code, "name": c.name, "minor_units": c.minor_units, "enabled": c.enabled}))
        .collect();
    Ok(json!({
        "reference": reference.as_str(),
        "can_enable": may_enable,
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
