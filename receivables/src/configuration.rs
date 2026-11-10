//! Bilimbi Receivables.Configuration and Customers: setup history and code
//! lists. All mutations hold the company lock and audit persisted row images.
use crate::{
    authorize, companies, invalid,
    store::{self, Table, CUSTOMERS, HISTORY, SERIES, SETUPS, TAX},
    validation as v, Customer, Page, ReceivableError as R, Receivables, Result, Series, Setup,
    SetupChange, TaxCode, MANAGE_CUSTOMERS, MANAGE_SETUP, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{Account, AccountType};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

pub(super) struct Prepared {
    pub before: Option<Value>,
    pub after: Value,
    pub accounts: Vec<Account>,
    /// The Core company a customer write links to, as the actor sees it.
    pub target: Option<companies::Target>,
}

impl Receivables {
    pub async fn get_setup(&self, scope: &Scope, company: i64) -> Result<Setup> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        Ok(store::decode(
            store::find(&**client, SETUPS, scope.tenant(), company, None)
                .await?
                .ok_or(R::NotConfigured)?,
        )?)
    }

    pub async fn configure(&self, scope: &Scope, company: i64, attrs: &Value) -> Result<Setup> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                MANAGE_SETUP,
                "finance.receivables.configure",
            )
            .await?;
        let setup = self.configure_on(&mut tx, scope, company, attrs).await?;
        tx.commit().await?;
        Ok(setup)
    }

    pub(super) async fn prepare_setup(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Prepared> {
        self.join(client, scope, company, MANAGE_SETUP).await?;
        let tenant = scope.tenant();
        let old = store::find(client, SETUPS, tenant, company, None).await?;
        let mut data = v::merge(old.as_ref(), attrs, SETUPS.columns)?;
        let mut layout_value = old
            .as_ref()
            .map(|o| o["invoice_layout"].clone())
            .unwrap_or_else(|| json!({}));
        if let Some(patch) = attrs.get("invoice_layout") {
            for (key, value) in patch
                .as_object()
                .ok_or_else(|| invalid("invoice_layout", "is invalid"))?
            {
                layout_value[key] = value.clone();
            }
        }
        let layout: crate::InvoiceLayout = serde_json::from_value(layout_value)
            .map_err(|_| invalid("invoice_layout", "is invalid"))?;
        for (field, text, limit) in [
            ("company_name", &layout.company_name, 200),
            ("registration_number", &layout.registration_number, 100),
            ("address", &layout.address, 2000),
            ("contact", &layout.contact, 500),
            ("footer", &layout.footer, 4000),
        ] {
            if text.chars().count() > limit
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err(invalid(
                    &format!("invoice_layout.{field}"),
                    format!("must be text with at most {limit} characters"),
                ));
            }
        }
        data["invoice_layout"] = serde_json::to_value(layout).map_err(|_| Error::Internal)?;
        let chart = self.ledger.list_accounts_on(client, scope, company).await?;
        v::account(
            &mut data,
            "receivable_account_id",
            &chart,
            AccountType::Asset,
        )?;
        v::account(&mut data, "income_account_id", &chart, AccountType::Income)?;
        // Optional until a receipt first keeps an unapplied amount; once
        // chosen it is replaced, never cleared, so every credit has a home.
        let credit_before = old
            .as_ref()
            .and_then(|o| o["customer_credit_account_id"].as_i64());
        if data["customer_credit_account_id"].is_null()
            || data["customer_credit_account_id"].as_str() == Some("")
        {
            if credit_before.is_some() {
                return Err(invalid(
                    "customer_credit_account_id",
                    "can't be blank once it has been chosen",
                ));
            }
            data["customer_credit_account_id"] = Value::Null;
        } else if data["customer_credit_account_id"].as_i64() != credit_before {
            v::account(
                &mut data,
                "customer_credit_account_id",
                &chart,
                AccountType::Liability,
            )?;
        }
        let control = data["receivable_account_id"]
            .as_i64()
            .expect("validated account id");
        let deposit = old.as_ref().map(|o| o["receivable_account_id"].as_i64()) != Some(Some(control))
            && client
                .query(
                    "SELECT EXISTS (SELECT 1 FROM finance_receivables_payments WHERE tenant_id=$1 AND company_id=$2 AND deposit_account_id=$3)",
                    &[&tenant, &company, &control],
                )
                .await?[0]
                .get::<_, bool>(0);
        if deposit {
            return Err(crate::invalid(
                "receivable_account_id",
                "is an account receipts are paid into (a bank or cash account); choose the account that records amounts customers owe",
            ));
        }
        let accounts = chart
            .into_iter()
            .filter(|a| {
                [
                    "receivable_account_id",
                    "income_account_id",
                    "customer_credit_account_id",
                ]
                .iter()
                    .any(|field| data[*field].as_i64() == Some(a.id))
            })
            .collect();
        Ok(Prepared {
            before: old,
            after: data,
            accounts,
            target: None,
        })
    }

    pub(super) async fn configure_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Setup> {
        let Prepared {
            before: old,
            after: data,
            ..
        } = self.prepare_setup(tx.sql(), scope, company, attrs).await?;
        let tenant = scope.tenant();
        let fields: Vec<_> = SETUPS
            .columns
            .iter()
            .copied()
            .filter(|f| old.as_ref().map(|o| &o[*f]) != Some(&data[*f]))
            .collect();
        let result = if fields.is_empty() {
            old.expect("existing unchanged setup")
        } else {
            let actor = scope.user();
            let impersonator = scope.impersonator().map(|a| a.user());
            tx.mutate(move |sql| Box::pin(async move {
                let mut changes = Vec::new();
                let new = store::save(sql, SETUPS, tenant, company, old.clone(), data, &mut changes).await?;
                for field in fields.into_iter().filter(|field| *field != "invoice_layout" && !new[*field].is_null()) {
                    store::save(sql, HISTORY, tenant, company, None, json!({"field": field, "old_value": old.as_ref().map(|o| &o[field]), "new_value": new[field], "changed_by_user_id": actor, "changed_by_impersonator_id": impersonator}), &mut changes).await?;
                }
                Ok((new, changes))
            })).await?
        };
        let setup = store::decode(result)?;
        Ok(setup)
    }

    pub async fn list_setup_changes(
        &self,
        scope: &Scope,
        company: i64,
        page: &Page,
    ) -> Result<Vec<SetupChange>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let (limit, after) = page.bounds()?;
        client.query("SELECT to_jsonb(r) FROM finance_receivables_setup_changes r WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4", &[&scope.tenant(), &company, &after, &limit]).await.map_err(Error::from)?.iter().map(|r| store::decode(r.get(0)).map_err(R::from)).collect()
    }
    pub async fn list_series(&self, scope: &Scope, company: i64) -> Result<Vec<Series>> {
        self.code_list(scope, company, SERIES).await
    }
    pub async fn create_series(
        &self,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Series> {
        self.save_code(scope, company, SERIES, None, attrs).await
    }
    pub async fn update_series(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<Series> {
        self.save_code(scope, company, SERIES, Some(id), attrs)
            .await
    }
    pub async fn list_tax_codes(&self, scope: &Scope, company: i64) -> Result<Vec<TaxCode>> {
        self.code_list(scope, company, TAX).await
    }
    pub async fn create_tax_code(
        &self,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<TaxCode> {
        self.save_code(scope, company, TAX, None, attrs).await
    }
    pub async fn update_tax_code(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<TaxCode> {
        self.save_code(scope, company, TAX, Some(id), attrs).await
    }
    /// Customers in code order, each with its linked company as this reader
    /// may see it (name and status for everyone, facts under Companies view).
    pub async fn list_customers(&self, scope: &Scope, company: i64) -> Result<Vec<Customer>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let mut customers = Vec::new();
        for row in store::list(&**client, CUSTOMERS, scope.tenant(), company).await? {
            customers.push(self.customer(&**client, scope, row).await?);
        }
        customers.sort_by(|a, b| a.code.cmp(&b.code).then(a.id.cmp(&b.id)));
        Ok(customers)
    }
    pub async fn get_customer(&self, scope: &Scope, company: i64, id: i64) -> Result<Customer> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let row = store::find(&**client, CUSTOMERS, scope.tenant(), company, Some(id))
            .await?
            .ok_or(R::CustomerNotFound)?;
        self.customer(&**client, scope, row).await
    }
    /// Creates a customer for a Core Company or Person reference or new party. Core
    /// creation (with duplicate review), the customer and its current customer
    /// relationship share one transaction. Finance retains only code and terms.
    pub async fn create_customer(
        &self,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Customer> {
        self.save_customer(scope, company, None, attrs).await
    }
    /// Changes payment terms or availability; the linked Core party's facts
    /// are refused here.
    pub async fn update_customer(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<Customer> {
        self.save_customer(scope, company, Some(id), attrs).await
    }
    async fn save_customer(
        &self,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Customer> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                MANAGE_CUSTOMERS,
                if id.is_some() {
                    "finance.receivables.update_customer"
                } else {
                    "finance.receivables.create_customer"
                },
            )
            .await?;
        let record = self
            .save_customer_on(&mut tx, scope, company, id, attrs)
            .await?;
        tx.commit().await?;
        Ok(record)
    }

    /// Saves a customer row through the code preparation and answers it with
    /// its company's facts, on the write's own transaction.
    pub(super) async fn save_customer_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Customer> {
        let row = self
            .save_code_on(tx, scope, company, CUSTOMERS, id, attrs)
            .await?;
        self.customer(tx.sql(), scope, row).await
    }

    async fn code_list<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        company: i64,
        table: Table,
    ) -> Result<Vec<T>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        store::list(&**client, table, scope.tenant(), company)
            .await?
            .into_iter()
            .map(|v| store::decode(v).map_err(R::from))
            .collect()
    }

    async fn save_code<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        company: i64,
        table: Table,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<T> {
        let customer = table.name == CUSTOMERS.name;
        let tax = table.name == TAX.name;
        let capability = if customer {
            MANAGE_CUSTOMERS
        } else {
            MANAGE_SETUP
        };
        let action = format!(
            "finance.receivables.{}_{}",
            if id.is_some() { "update" } else { "create" },
            if customer {
                "customer"
            } else if tax {
                "tax_code"
            } else {
                "series"
            }
        );
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(&mut client, scope, company, capability, &action)
            .await?;
        let record = store::decode(
            self.save_code_on(&mut tx, scope, company, table, id, attrs)
                .await?,
        )?;
        tx.commit().await?;
        Ok(record)
    }

    pub(super) async fn prepare_code(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        table: Table,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Prepared> {
        let customer = table.name == CUSTOMERS.name;
        let tax = table.name == TAX.name;
        self.join(
            client,
            scope,
            company,
            if customer {
                MANAGE_CUSTOMERS
            } else {
                MANAGE_SETUP
            },
        )
        .await?;
        let tenant = scope.tenant();
        let old = if let Some(id) = id {
            Some(
                store::find(client, table, tenant, company, Some(id))
                    .await?
                    .ok_or(if customer {
                        R::CustomerNotFound
                    } else if tax {
                        R::TaxCodeNotFound
                    } else {
                        R::SeriesNotFound
                    })?,
            )
        } else {
            None
        };
        let editable: Vec<_> = table
            .columns
            .iter()
            .copied()
            .filter(|f| {
                !matches!(*f, "record_kind" | "record_id")
                    && (id.is_none()
                        || !matches!(*f, "code" | "kind" | "prefix" | "number_width" | "rate"))
            })
            .collect();
        let defaults = if customer {
            json!({"active":true,"payment_terms_days":0,"record_kind":null,"record_id":null})
        } else if tax {
            json!({"active":true})
        } else {
            json!({"active":true,"number_width":1,"prefix":""})
        };
        let mut data = v::merge(Some(old.as_ref().unwrap_or(&defaults)), attrs, &editable)?;
        // Ecto.cast resets a blank editable value to its schema default,
        // even on update. Omitted values keep the stored value; null does not.
        for field in &editable {
            if attrs[*field].as_str().is_some_and(|s| s.trim().is_empty()) {
                if let Some(default) = defaults.get(*field) {
                    data[*field] = default.clone();
                }
            }
        }
        if !customer {
            v::text(&mut data, "code", true, 50)?;
        }
        v::flag(&mut data, "active")?;
        let mut chart = Vec::new();
        let mut target = None;
        if customer {
            for field in [
                "name",
                "tax_identifier",
                "registration_identifier",
                "email",
                "phone",
                "line1",
                "line2",
                "line3",
                "locality",
                "postcode",
                "admin1_code",
                "country_iso",
            ] {
                if attrs.get(field).is_some() {
                    return Err(invalid(field, "is read from the linked Core party"));
                }
            }
            if attrs.get("code").is_some() {
                return Err(invalid(
                    "code",
                    "is read from the Core party and cannot be supplied",
                ));
            }
            if let Some(old) = &old {
                if attrs.get("record_kind").is_some()
                    || attrs.get("record_id").is_some()
                    || attrs.get("new_company").is_some()
                    || attrs.get("new_person").is_some()
                {
                    return Err(invalid("record_id", "the linked Core party cannot change"));
                }
                data["record_kind"] = old["record_kind"].clone();
                data["record_id"] = old["record_id"].clone();
            } else {
                let prepared = self.prepare_target(client, scope, company, attrs).await?;
                data["record_kind"] = json!(prepared.kind);
                data["record_id"] = json!(prepared.id.map(|id| id.to_string()));
                target = Some(prepared);
            }
            v::range(&mut data, "payment_terms_days", 0, 3650)?;
        } else if tax {
            v::text(&mut data, "name", true, 200)?;
            v::decimal(&mut data, "rate", false, Decimal::from(1000), 4)?;
            chart = self.ledger.list_accounts_on(client, scope, company).await?;
            v::integer(&mut data, "account_id", true)?;
            if old.as_ref().map(|o| &o["account_id"]) != Some(&data["account_id"]) {
                v::account(&mut data, "account_id", &chart, AccountType::Liability)?;
            }
        } else {
            v::text(&mut data, "name", true, 200)?;
            v::choice(&mut data, "kind", Self::series_kinds())?;
            if data["prefix"].is_null() {
                data["prefix"] = json!("");
            }
            let prefix = data["prefix"]
                .as_str()
                .ok_or_else(|| invalid("prefix", "is invalid"))?;
            if prefix.chars().count() > 20 {
                return Err(invalid("prefix", "should be at most 20 character(s)"));
            }
            v::range(&mut data, "number_width", 1, 12)?;
        }
        if id.is_none() && !customer {
            for field in if table.name == SERIES.name {
                &["code", "prefix"][..]
            } else {
                &["code"][..]
            } {
                if store::taken(client, table, tenant, company, field, &data[*field]).await? {
                    return Err(invalid(field, "has already been taken"));
                }
            }
        }
        let accounts = chart
            .into_iter()
            .filter(|a| data["account_id"].as_i64() == Some(a.id))
            .collect();
        Ok(Prepared {
            before: old,
            after: data,
            accounts,
            target,
        })
    }

    /// Saves a series, tax code or customer row and answers the stored row.
    pub(super) async fn save_code_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        table: Table,
        id: Option<i64>,
        attrs: &Value,
    ) -> Result<Value> {
        let Prepared {
            before: old,
            after: mut data,
            ..
        } = self
            .prepare_code(tx.sql(), scope, company, table, id, attrs)
            .await?;
        if table.name == CUSTOMERS.name && id.is_none() {
            self.compose_target(tx, scope, company, attrs, &mut data)
                .await?;
        }
        let tenant = scope.tenant();
        let new = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    let new =
                        store::save(sql, table, tenant, company, old, data, &mut changes).await?;
                    Ok((new, changes))
                })
            })
            .await?;
        Ok(new)
    }
}

// The shared image setting editor owns upload inspection, Media storage and
// removal; this setting belongs to the company's Receivables setup.
pub(crate) const INVOICE_LOGO: &str = "finance.receivables.invoice_logo_asset_id";
macro_rules! define {
    ($key:expr, $label:expr) => {
        inventory::submit! { bilibi_module_registry::Contribution::new(
            bilibi_settings::DEFINITIONS, $key,
            bilibi_module_registry::Provenance { module: crate::MODULE, source: file!(), line: line!() },
            &bilibi_settings::Setting::Definition(bilibi_settings::Definition {
                value_type: bilibi_settings::ValueType::Integer,
                scopes: &[bilibi_settings::ScopeKind::Company], default: || Value::Null,
                nullable: true, encrypted: false, minimum: Some(1.0), maximum: None,
                validator: Some(bilibi_settings::Validator { accepts: |v| v.is_null() || v.as_i64().is_some_and(|id| id > 0), message: "must name a stored invoice logo" }),
                editor: Some(bilibi_settings::Editor { group: "Receivables", label: $label, help: "Printed on invoices and credit notes for this company. PNG, JPEG or WebP.", capability: Some(MANAGE_SETUP), reveal_duration_ms: 5_000 }),
            })
        ) }
    };
}
define!(INVOICE_LOGO, "Invoice logo");
inventory::submit! { bilibi_module_registry::Contribution::new(
    bilibi_settings_editor::IMAGE_SETTINGS, INVOICE_LOGO,
    bilibi_module_registry::Provenance { module: crate::MODULE, source: file!(), line: line!() },
    &bilibi_settings_editor::ImageSetting { directory: "receivables-invoice-logos", max_bytes: 1024 * 1024 }
) }
