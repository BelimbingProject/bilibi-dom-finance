//! The `finance_reconciliation_bank_accounts` rows and their change log
//! (Bilimbi `Reconciliation.BankAccounts`, `BankAccount` and
//! `BankAccountChange`). The facade authorizes and holds the company lock;
//! these run inside it.
use crate::{
    BankAccount, BankAccountChange, BankAccountEdit, FieldError, ImportMapping, NewBankAccount,
};
use bilibi_database::{Change, Result};
use bilibi_finance_ledger::{Account, AccountType};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_postgres::{GenericClient, Row};

const COLUMNS: &str = "id,name,ledger_account_id,charges_account_id,interest_account_id,match_window_days,import_mapping,active";
const NAME_LIMIT: usize = 200;
const REASON_LIMIT: usize = 500;
const DEFAULT_WINDOW: i32 = 5;
const KIND: &str = "finance.reconciliation.bank_account";
const LOG_KIND: &str = "finance.reconciliation.bank_account_change";

/// The company's bank accounts by name.
pub(crate) async fn list(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
) -> Result<Vec<BankAccount>> {
    client
        .query(
            &format!("SELECT {COLUMNS} FROM finance_reconciliation_bank_accounts WHERE tenant_id=$1 AND company_id=$2 ORDER BY name"),
            &[&tenant, &company_id],
        )
        .await?
        .iter()
        .map(from_row)
        .collect()
}

/// One bank account of the company, or `None` for a missing or other
/// company's id.
pub(crate) async fn find(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
) -> Result<Option<BankAccount>> {
    client
        .query_opt(
            &format!("SELECT {COLUMNS} FROM finance_reconciliation_bank_accounts WHERE tenant_id=$1 AND company_id=$2 AND id=$3"),
            &[&tenant, &company_id, &bank_account_id],
        )
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

/// The bank account's change log, oldest first.
pub(crate) async fn log(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    limit: i64,
    after: i64,
) -> Result<Vec<BankAccountChange>> {
    client
        .query(
            "SELECT id,field,old_value,new_value,reason,changed_by_user_id,changed_by_impersonator_id,\
             to_char(changed_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
             FROM finance_reconciliation_bank_account_changes \
             WHERE tenant_id=$1 AND company_id=$2 AND bank_account_id=$3 AND id>$4 ORDER BY id LIMIT $5",
            &[&tenant, &company_id, &bank_account_id, &after, &limit],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(BankAccountChange {
                id: row.get(0),
                field: row.get(1),
                old_value: row.get(2),
                new_value: row.get(3),
                reason: row.get(4),
                changed_by_user_id: row.get(5),
                changed_by_impersonator_id: row.get(6),
                changed_at: row
                    .get::<_, String>(7)
                    .parse()
                    .map_err(|_| bilibi_database::Error::Internal)?,
            })
        })
        .collect()
}

/// A new bank account as `BankAccount.create_changeset/2` accepts it.
pub(crate) struct ValidNew {
    name: String,
    ledger_account_id: i64,
    charges_account_id: Option<i64>,
    interest_account_id: Option<i64>,
    match_window_days: i32,
    import_mapping: Option<ImportMapping>,
    active: bool,
    reason: Option<String>,
}

pub(crate) async fn validate_new(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    chart: &HashMap<i64, Account>,
    new: &NewBankAccount,
) -> Result<std::result::Result<ValidNew, Vec<FieldError>>> {
    let mut errors = Vec::new();
    let name = name(&new.name, &mut errors);
    let window = new.match_window_days.unwrap_or(DEFAULT_WINDOW);
    window_in_range(window, &mut errors);
    // A new account may leave its columns to its first import.
    let mapping = if new.import_mapping.is_null() {
        Some(None)
    } else {
        mapping(&new.import_mapping, &mut errors).map(Some)
    };
    account(
        chart,
        "ledger_account_id",
        Some(new.ledger_account_id),
        AccountType::Asset,
        &mut errors,
    );
    counter_accounts(
        chart,
        new.charges_account_id,
        new.interest_account_id,
        &mut errors,
    );
    if errors.is_empty() {
        // Ecto reports a unique index only once the rest is valid, and the
        // first one the database finds.
        let taken: Option<(&'static str, &str)> = if client
            .query_opt(
                "SELECT 1 FROM finance_reconciliation_bank_accounts WHERE tenant_id=$1 AND company_id=$2 AND ledger_account_id=$3",
                &[&tenant, &company_id, &new.ledger_account_id],
            )
            .await?
            .is_some()
        {
            Some(("ledger_account_id", "already has a bank account"))
        } else if name_taken(client, tenant, company_id, &name, None).await? {
            Some(("name", NAME_TAKEN))
        } else {
            None
        };
        if let Some((field, message)) = taken {
            errors.push(FieldError::new(field, message));
        }
    }
    Ok(match mapping {
        Some(import_mapping) if errors.is_empty() => Ok(ValidNew {
            name,
            ledger_account_id: new.ledger_account_id,
            charges_account_id: new.charges_account_id,
            interest_account_id: new.interest_account_id,
            match_window_days: window,
            import_mapping,
            active: new.active.unwrap_or(true),
            reason: reason(new.reason.as_deref()),
        }),
        _ => Err(errors),
    })
}

/// Adds the bank account and logs every value it was created with.
pub(crate) async fn insert(
    sql: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    valid: &ValidNew,
) -> Result<(BankAccount, Vec<Change>)> {
    let row = sql
        .query_one(
            "INSERT INTO finance_reconciliation_bank_accounts(tenant_id,company_id,name,ledger_account_id,charges_account_id,interest_account_id,match_window_days,import_mapping,active,inserted_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING id",
            &[
                &tenant,
                &company_id,
                &valid.name,
                &valid.ledger_account_id,
                &valid.charges_account_id,
                &valid.interest_account_id,
                &(valid.match_window_days as i16),
                &valid.import_mapping.as_ref().map(ImportMapping::to_json),
                &valid.active,
            ],
        )
        .await?;
    let account = BankAccount {
        id: row.get(0),
        name: valid.name.clone(),
        ledger_account_id: valid.ledger_account_id,
        charges_account_id: valid.charges_account_id,
        interest_account_id: valid.interest_account_id,
        match_window_days: valid.match_window_days,
        import_mapping: valid.import_mapping.clone(),
        active: valid.active,
    };
    let mut changes = vec![Change {
        kind: KIND.into(),
        id: account.id,
        old: Value::Null,
        new: to_json(company_id, &account),
    }];
    // A new account's first values are logged with no old value, the unset
    // counter accounts left out.
    let first: [(&str, Option<String>); 6] = [
        ("name", Some(account.name.clone())),
        (
            "charges_account_id",
            account.charges_account_id.map(|id| id.to_string()),
        ),
        (
            "interest_account_id",
            account.interest_account_id.map(|id| id.to_string()),
        ),
        (
            "match_window_days",
            Some(account.match_window_days.to_string()),
        ),
        ("import_mapping", text(&account, "import_mapping")),
        ("active", Some(account.active.to_string())),
    ];
    for (field, new) in first {
        if new.is_some() {
            changes.push(
                record(
                    sql,
                    tenant,
                    company_id,
                    account.id,
                    (field, None, new),
                    valid.reason.as_deref(),
                    (actor, impersonator),
                )
                .await?,
            );
        }
    }
    Ok((account, changes))
}

/// A bank account after a change, and what changed in it.
pub(crate) struct Next {
    account: BankAccount,
    fields: Vec<&'static str>,
    reason: Option<String>,
}

/// `None` when the change changes nothing.
pub(crate) async fn validate_change(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    chart: &HashMap<i64, Account>,
    current: &BankAccount,
    change: &BankAccountEdit,
) -> Result<std::result::Result<Option<Next>, Vec<FieldError>>> {
    let mut errors = Vec::new();
    let mut next = current.clone();
    let mut fields = Vec::new();
    if let Some(requested) = &change.name {
        let requested = name(requested, &mut errors);
        if requested != current.name {
            next.name = requested;
            fields.push("name");
        }
    }
    if let Some(charges) = change.charges_account_id {
        if charges != current.charges_account_id {
            account(
                chart,
                "charges_account_id",
                charges,
                AccountType::Expense,
                &mut errors,
            );
            next.charges_account_id = charges;
            fields.push("charges_account_id");
        }
    }
    if let Some(interest) = change.interest_account_id {
        if interest != current.interest_account_id {
            account(
                chart,
                "interest_account_id",
                interest,
                AccountType::Income,
                &mut errors,
            );
            next.interest_account_id = interest;
            fields.push("interest_account_id");
        }
    }
    if let Some(window) = change.match_window_days {
        if window != current.match_window_days {
            window_in_range(window, &mut errors);
            next.match_window_days = window;
            fields.push("match_window_days");
        }
    }
    if let Some(requested) = &change.import_mapping {
        if let Some(parsed) = mapping(requested, &mut errors) {
            if current.import_mapping.as_ref() != Some(&parsed) {
                next.import_mapping = Some(parsed);
                fields.push("import_mapping");
            }
        }
    }
    if let Some(active) = change.active {
        if active != current.active {
            next.active = active;
            fields.push("active");
        }
    }
    if errors.is_empty()
        && fields.contains(&"name")
        && name_taken(client, tenant, company_id, &next.name, Some(current.id)).await?
    {
        errors.push(FieldError::new("name", NAME_TAKEN));
    }
    if !errors.is_empty() {
        return Ok(Err(errors));
    }
    // Ecto's field order, whatever order the change was built in.
    fields.sort_by_key(|field| {
        [
            "name",
            "charges_account_id",
            "interest_account_id",
            "match_window_days",
            "import_mapping",
            "active",
        ]
        .iter()
        .position(|known| known == field)
    });
    Ok(Ok((!fields.is_empty()).then(|| Next {
        account: next,
        fields,
        reason: reason(change.reason.as_deref()),
    })))
}

/// The account with `mapping` as its import mapping, logged with `reason`;
/// `None` when it already reads that way. A statement import keeps the
/// mapping it read the file through this way.
pub(crate) fn remap(current: &BankAccount, mapping: &ImportMapping, reason: &str) -> Option<Next> {
    if current.import_mapping.as_ref() == Some(mapping) {
        return None;
    }
    let mut account = current.clone();
    account.import_mapping = Some(mapping.clone());
    Some(Next {
        account,
        fields: vec!["import_mapping"],
        reason: self::reason(Some(reason)),
    })
}

/// Saves the change and logs every field that changed.
pub(crate) async fn update(
    sql: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    actor: i64,
    impersonator: Option<i64>,
    current: &BankAccount,
    next: &Next,
) -> Result<(BankAccount, Vec<Change>)> {
    let saved = &next.account;
    sql.execute(
        "UPDATE finance_reconciliation_bank_accounts SET name=$4,charges_account_id=$5,interest_account_id=$6,match_window_days=$7,import_mapping=$8,active=$9,updated_at=now() AT TIME ZONE 'UTC' \
         WHERE tenant_id=$1 AND company_id=$2 AND id=$3",
        &[
            &tenant,
            &company_id,
            &saved.id,
            &saved.name,
            &saved.charges_account_id,
            &saved.interest_account_id,
            &(saved.match_window_days as i16),
            &saved.import_mapping.as_ref().map(ImportMapping::to_json),
            &saved.active,
        ],
    )
    .await?;
    let mut changes = vec![Change {
        kind: KIND.into(),
        id: saved.id,
        old: to_json(company_id, current),
        new: to_json(company_id, saved),
    }];
    for &field in &next.fields {
        changes.push(
            record(
                sql,
                tenant,
                company_id,
                saved.id,
                (field, text(current, field), text(saved, field)),
                next.reason.as_deref(),
                (actor, impersonator),
            )
            .await?,
        );
    }
    Ok((saved.clone(), changes))
}

/// Appends one logged field; the database stamps its time.
async fn record(
    sql: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    bank_account_id: i64,
    (field, old, new): (&str, Option<String>, Option<String>),
    reason: Option<&str>,
    (actor, impersonator): (i64, Option<i64>),
) -> Result<Change> {
    let row = sql
        .query_one(
            "INSERT INTO finance_reconciliation_bank_account_changes(tenant_id,company_id,bank_account_id,field,old_value,new_value,reason,changed_by_user_id,changed_by_impersonator_id,changed_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,NULL) RETURNING id",
            &[&tenant, &company_id, &bank_account_id, &field, &old, &new, &reason, &actor, &impersonator],
        )
        .await?;
    Ok(Change {
        kind: LOG_KIND.into(),
        id: row.get(0),
        old: Value::Null,
        new: json!({
            "company_id": company_id,
            "bank_account_id": bank_account_id,
            "field": field,
            "old_value": old,
            "new_value": new,
            "reason": reason,
            "changed_by_user_id": actor,
            "changed_by_impersonator_id": impersonator,
        }),
    })
}

const NAME_TAKEN: &str = "is already a bank account of this company";

async fn name_taken(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    name: &str,
    except: Option<i64>,
) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM finance_reconciliation_bank_accounts WHERE tenant_id=$1 AND company_id=$2 AND name=$3 AND id<>$4",
            &[&tenant, &company_id, &name, &except.unwrap_or(0)],
        )
        .await?
        .is_some())
}

/// `validate_required` and `validate_length` on the trimmed name.
fn name(requested: &str, errors: &mut Vec<FieldError>) -> String {
    let name = requested.trim().to_owned();
    if name.is_empty() {
        errors.push(FieldError::new("name", "can't be blank"));
    } else if name.chars().count() > NAME_LIMIT {
        errors.push(FieldError::new(
            "name",
            format!("should be at most {NAME_LIMIT} character(s)"),
        ));
    }
    name
}

fn window_in_range(window: i32, errors: &mut Vec<FieldError>) {
    if window < 0 {
        errors.push(FieldError::new(
            "match_window_days",
            "must be greater than or equal to 0",
        ));
    } else if window > 90 {
        errors.push(FieldError::new(
            "match_window_days",
            "must be less than or equal to 90",
        ));
    }
}

fn mapping(requested: &Value, errors: &mut Vec<FieldError>) -> Option<ImportMapping> {
    if requested.is_null() {
        errors.push(FieldError::new("import_mapping", "can't be blank"));
        return None;
    }
    match ImportMapping::parse(requested) {
        Ok(parsed) => Some(parsed),
        Err(message) => {
            errors.push(FieldError::new("import_mapping", message));
            None
        }
    }
}

fn counter_accounts(
    chart: &HashMap<i64, Account>,
    charges: Option<i64>,
    interest: Option<i64>,
    errors: &mut Vec<FieldError>,
) {
    account(
        chart,
        "charges_account_id",
        charges,
        AccountType::Expense,
        errors,
    );
    account(
        chart,
        "interest_account_id",
        interest,
        AccountType::Income,
        errors,
    );
}

/// An account named by a field must be an active account of the chart of the
/// given type; an unset one is not checked.
fn account(
    chart: &HashMap<i64, Account>,
    field: &'static str,
    id: Option<i64>,
    account_type: AccountType,
    errors: &mut Vec<FieldError>,
) {
    let Some(id) = id else { return };
    match chart.get(&id) {
        None => errors.push(FieldError::new(
            field,
            "is not a ledger account of this company",
        )),
        Some(account) if account.is_group => errors.push(FieldError::new(
            field,
            "is a ledger account group; choose an account under it",
        )),
        Some(account) if !account.active => {
            errors.push(FieldError::new(field, "is an inactive ledger account"))
        }
        Some(account) if account.account_type != account_type => errors.push(FieldError::new(
            field,
            format!("must be a ledger {} account", account_type.as_str()),
        )),
        Some(_) => {}
    }
}

/// `BankAccounts.reason/1`: trimmed, cut at 500 characters, blank as none.
fn reason(requested: Option<&str>) -> Option<String> {
    let reason: String = requested?.trim().chars().take(REASON_LIMIT).collect();
    (!reason.is_empty()).then_some(reason)
}

/// A logged field's value as text.
fn text(account: &BankAccount, field: &str) -> Option<String> {
    match field {
        "name" => Some(account.name.clone()),
        "charges_account_id" => account.charges_account_id.map(|id| id.to_string()),
        "interest_account_id" => account.interest_account_id.map(|id| id.to_string()),
        "match_window_days" => Some(account.match_window_days.to_string()),
        "import_mapping" => account
            .import_mapping
            .as_ref()
            .map(|mapping| canonical(&mapping.to_json())),
        _ => Some(account.active.to_string()),
    }
}

/// A JSON value written with its object keys in order, so the same mapping is
/// always the same text (Bilimbi sorts the map for `inspect/2`).
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let entries: Vec<String> = entries
                .into_iter()
                .map(|(key, value)| format!("{}:{}", Value::from(key.as_str()), canonical(value)))
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        other => other.to_string(),
    }
}

pub(crate) fn to_json(company_id: i64, account: &BankAccount) -> Value {
    json!({
        "company_id": company_id,
        "name": account.name,
        "ledger_account_id": account.ledger_account_id,
        "charges_account_id": account.charges_account_id,
        "interest_account_id": account.interest_account_id,
        "match_window_days": account.match_window_days,
        "import_mapping": account.import_mapping.as_ref().map(ImportMapping::to_json),
        "active": account.active,
    })
}

fn from_row(row: &Row) -> Result<BankAccount> {
    Ok(BankAccount {
        id: row.get(0),
        name: row.get(1),
        ledger_account_id: row.get(2),
        charges_account_id: row.get(3),
        interest_account_id: row.get(4),
        match_window_days: i32::from(row.get::<_, i16>(5)),
        import_mapping: row
            .get::<_, Option<Value>>(6)
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| bilibi_database::Error::Internal)?,
        active: row.get(7),
    })
}
