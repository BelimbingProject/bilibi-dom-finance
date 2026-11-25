//! Settings/mapping changes and their atomic logs, mirroring Bilimbi Payroll.
use crate::{
    authorize, ledger_error, reason, store, Mapping, MappingChange, Payroll, PayrollError, Result,
    Settings, SettingsChange, MANAGE_SETTINGS, VIEW,
};
use bilibi_database::{Change, Error};
use bilibi_finance_ledger::{Account, AccountType, FieldError};
use bilibi_tenancy::Scope;
use serde_json::Value;

impl Payroll {
    pub async fn get_settings(&self, scope: &Scope, company: i64) -> Result<Settings> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.get_settings_on(&**client, scope, company).await
    }

    /// Authorized settings read on the caller's connection.
    pub async fn get_settings_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
    ) -> Result<Settings> {
        authorize(client, scope, company, VIEW).await?;
        store::decode(
            store::settings(client, scope.tenant(), company)
                .await?
                .ok_or(PayrollError::PayrollNotConfigured)?,
        )
    }
    pub async fn list_mappings(&self, scope: &Scope, company: i64) -> Result<Vec<Mapping>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        store::mappings(&**client, scope.tenant(), company)
            .await?
            .into_iter()
            .map(store::decode)
            .collect()
    }

    pub async fn put_settings(
        &self,
        scope: &Scope,
        company: i64,
        new: &SettingsChange,
    ) -> Result<Settings> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .ledger
            .write(
                &mut client,
                scope,
                company,
                &[MANAGE_SETTINGS],
                "finance.payroll.put_settings",
            )
            .await
            .map_err(ledger_error)?;
        let reason = reason(new.reason.as_deref(), false)?;
        let tenant = scope.tenant();
        let old = store::settings(tx.sql(), tenant, company).await?;
        let net = new
            .net_pay_account_id
            .or_else(|| old.as_ref().and_then(|v| v["net_pay_account_id"].as_i64()));
        let claims = new.claims_payable_account_id.or_else(|| {
            old.as_ref()
                .and_then(|v| v["claims_payable_account_id"].as_i64())
        });
        let accounts = self
            .ledger
            .list_accounts_on(tx.sql(), scope, company)
            .await?;
        let mut errors = Vec::new();
        account(
            &accounts,
            "net_pay_account_id",
            net,
            true,
            true,
            &mut errors,
        );
        account(
            &accounts,
            "claims_payable_account_id",
            claims,
            true,
            true,
            &mut errors,
        );
        if !errors.is_empty() {
            return Err(PayrollError::Invalid(errors));
        }
        let (net, claims) = (
            net.expect("validated net account"),
            claims.expect("validated claims account"),
        );
        if old.as_ref().is_some_and(|o| {
            o["net_pay_account_id"] == net && o["claims_payable_account_id"] == claims
        }) {
            return store::decode(old.expect("existing settings"));
        }
        let actor = scope.user();
        let impersonator = scope.impersonator().map(|p| p.user());
        let saved = tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one("INSERT INTO finance_payroll_settings AS s(tenant_id,company_id,net_pay_account_id,claims_payable_account_id,inserted_at,updated_at) VALUES($1,$2,$3,$4,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') ON CONFLICT(company_id) DO UPDATE SET net_pay_account_id=$3,claims_payable_account_id=$4,updated_at=now() AT TIME ZONE 'UTC' RETURNING to_jsonb(s)", &[&tenant,&company,&net,&claims]).await?;
            let saved: Value = row.get(0);
            let old_net = old.as_ref().and_then(|o| o["net_pay_account_id"].as_i64());
            let old_claims = old.as_ref().and_then(|o| o["claims_payable_account_id"].as_i64());
            let row = sql.query_one("INSERT INTO finance_payroll_settings_changes AS c(tenant_id,company_id,old_net_pay_account_id,new_net_pay_account_id,old_claims_payable_account_id,new_claims_payable_account_id,reason,changed_by_user_id,changed_by_impersonator_id,changed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,NULL) RETURNING to_jsonb(c)", &[&tenant,&company,&old_net,&net,&old_claims,&claims,&reason,&actor,&impersonator]).await?;
            let changes = vec![store::change("settings",old.unwrap_or(Value::Null),saved.clone()),store::change("settings_change",Value::Null,row.get(0))];
            Ok((saved,changes))
        })).await?;
        tx.commit().await?;
        store::decode(saved)
    }

    pub async fn put_mapping(
        &self,
        scope: &Scope,
        company: i64,
        new: &MappingChange,
    ) -> Result<Mapping> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .ledger
            .write(
                &mut client,
                scope,
                company,
                &[MANAGE_SETTINGS],
                "finance.payroll.put_mapping",
            )
            .await
            .map_err(ledger_error)?;
        let reason = reason(new.reason.as_deref(), false)?;
        let (kind, code, direction) = (
            new.kind.trim().to_owned(),
            new.code.trim().to_owned(),
            new.direction.trim().to_owned(),
        );
        let mut errors = Vec::new();
        if !Self::mapping_kinds().contains(&kind.as_str()) {
            errors.push(FieldError::new("kind", "is invalid"));
        }
        if code.is_empty() {
            errors.push(FieldError::new("code", "can't be blank"));
        }
        if code.chars().count() > 60 {
            errors.push(FieldError::new("code", "should be at most 60 character(s)"));
        }
        if !matches!(
            (kind.as_str(), direction.as_str()),
            ("component", "earning" | "deduction" | "employer") | ("claim_category", "claim")
        ) {
            errors.push(FieldError::new("direction", "is invalid"));
        }
        let accounts = self
            .ledger
            .list_accounts_on(tx.sql(), scope, company)
            .await?;
        let expense_needed = matches!(direction.as_str(), "earning" | "employer" | "claim");
        let liability_needed = matches!(direction.as_str(), "deduction" | "employer");
        for (field, id, needed, liability) in [
            (
                "expense_account_id",
                new.expense_account_id,
                expense_needed,
                false,
            ),
            (
                "liability_account_id",
                new.liability_account_id,
                liability_needed,
                true,
            ),
        ] {
            if needed && id.is_none() {
                errors.push(FieldError::new(
                    field,
                    format!("is needed for a {direction}"),
                ));
            } else if !needed && id.is_some() {
                errors.push(FieldError::new(
                    field,
                    format!("is not used by a {direction}"),
                ));
            } else if id.is_some() {
                account(&accounts, field, id, false, liability, &mut errors);
            }
        }
        if !errors.is_empty() {
            return Err(PayrollError::Invalid(errors));
        }
        let tenant = scope.tenant();
        let old = store::mappings(tx.sql(), tenant, company)
            .await?
            .into_iter()
            .find(|v| v["kind"] == kind && v["code"] == code && v["direction"] == direction);
        let expense = new.expense_account_id;
        let liability = new.liability_account_id;
        if old.as_ref().is_some_and(|v| {
            v["expense_account_id"].as_i64() == expense
                && v["liability_account_id"].as_i64() == liability
        }) {
            return store::decode(old.expect("existing mapping"));
        }
        let actor = scope.user();
        let impersonator = scope.impersonator().map(|p| p.user());
        let saved = tx.mutate(move |sql| Box::pin(async move {
            let saved: Value = sql.query_one("INSERT INTO finance_payroll_mappings AS m(tenant_id,company_id,kind,code,direction,expense_account_id,liability_account_id,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') ON CONFLICT(company_id,kind,code,direction) DO UPDATE SET expense_account_id=$6,liability_account_id=$7,updated_at=now() AT TIME ZONE 'UTC' RETURNING to_jsonb(m)", &[&tenant,&company,&kind,&code,&direction,&expense,&liability]).await?.get(0);
            let id = saved["id"].as_i64().ok_or(Error::Internal)?;
            let old_expense = old.as_ref().and_then(|v| v["expense_account_id"].as_i64());
            let old_liability = old.as_ref().and_then(|v| v["liability_account_id"].as_i64());
            let history: Value = sql.query_one("INSERT INTO finance_payroll_mapping_changes AS c(tenant_id,company_id,mapping_id,old_expense_account_id,new_expense_account_id,old_liability_account_id,new_liability_account_id,reason,changed_by_user_id,changed_by_impersonator_id,changed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,NULL) RETURNING to_jsonb(c)", &[&tenant,&company,&id,&old_expense,&expense,&old_liability,&liability,&reason,&actor,&impersonator]).await?.get(0);
            let changes: Vec<Change> = vec![store::change("mapping",old.unwrap_or(Value::Null),saved.clone()),store::change("mapping_change",Value::Null,history)];
            Ok((saved,changes))
        })).await?;
        tx.commit().await?;
        store::decode(saved)
    }
}

fn account(
    accounts: &[Account],
    field: &'static str,
    id: Option<i64>,
    required: bool,
    liability: bool,
    errors: &mut Vec<FieldError>,
) {
    let message = match id {
        None if required => Some("can't be blank"),
        None => None,
        Some(id) => match accounts.iter().find(|a| a.id == id) {
            None => Some("is not an account of this company"),
            Some(a) if a.is_group => Some("is an account group; choose an account under it"),
            Some(a) if !a.active => Some("is inactive"),
            Some(a) if liability && a.account_type != AccountType::Liability => {
                Some("must be an account of type liability")
            }
            Some(a)
                if !liability
                    && !matches!(a.account_type, AccountType::Asset | AccountType::Expense) =>
            {
                Some("must be an account of type asset or expense")
            }
            _ => None,
        },
    };
    if let Some(message) = message {
        errors.push(FieldError::new(field, message));
    }
}
