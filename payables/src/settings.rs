//! Bilimbi.Finance.Payables settings and tax codes with append-only history.
use crate::store::Table;
use crate::*;
use bilibi_finance_ledger::{Account, AccountType};
use serde_json::{json, Value};

pub(crate) fn account(
    accounts: &[Account],
    id: i64,
    field: &'static str,
    types: &[AccountType],
) -> Result<()> {
    let a = accounts
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| invalid(field, "is not an account of this company"))?;
    if a.is_group {
        return Err(invalid(
            field,
            "is an account group; choose an account under it",
        ));
    }
    if !a.active {
        return Err(invalid(field, "is inactive"));
    }
    if !types.is_empty() && !types.contains(&a.account_type) {
        return Err(invalid(
            field,
            format!(
                "must be an account of type {}",
                types
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        ));
    }
    Ok(())
}
pub(crate) fn string(
    value: &str,
    field: &'static str,
    max: usize,
    required: bool,
) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        if required {
            return Err(invalid(field, "can't be blank"));
        }
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(invalid(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
    Ok(Some(value.to_owned()))
}
pub(crate) fn optional(
    value: &Option<String>,
    field: &'static str,
    max: usize,
) -> Result<Option<String>> {
    string(value.as_deref().unwrap_or_default(), field, max, false)
}
fn tolerance(t: &Option<Tolerance>, field: &'static str) -> Result<()> {
    if let Some(t) = t {
        let units =
            bilibi_finance_ledger::MinorUnits::new(4).ok_or(PayablesError::InvalidOption)?;
        Money::new(t.value, units).map_err(|e| invalid(field, e.to_string()))?;
        if t.value < Decimal::ZERO {
            return Err(invalid(field, "must be greater than or equal to 0"));
        }
    }
    Ok(())
}
impl Payables {
    pub async fn get_settings(&self, s: &Scope, company: i64) -> Result<Settings> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.get_settings_on(&**c, s, company).await
    }

    /// Authorized settings read on the caller's connection.
    pub async fn get_settings_on(
        &self,
        c: &(impl tokio_postgres::GenericClient + Sync),
        s: &Scope,
        company: i64,
    ) -> Result<Settings> {
        authorize(c, s, company, VIEW).await?;
        store::settings(c, s.tenant(), company)
            .await?
            .map(|(_, s)| s)
            .ok_or(PayablesError::PayablesNotConfigured)
    }
    pub async fn put_settings(
        &self,
        s: &Scope,
        company: i64,
        input: &SettingsInput,
    ) -> Result<Settings> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                MANAGE_SETTINGS,
                "finance.payables.put_settings",
            )
            .await?;
        let result = self.put_settings_on(&mut tx, s, company, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn validate_settings(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &SettingsInput,
    ) -> Result<(Option<(i64, Settings)>, Option<String>)> {
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        account(
            &accounts,
            input.settings.payable_account_id,
            "payable_account_id",
            &[AccountType::Liability],
        )?;
        tolerance(&input.settings.quantity_tolerance, "quantity_tolerance")?;
        tolerance(&input.settings.price_tolerance, "price_tolerance")?;
        let reason = optional(&input.reason, "reason", 500)?;
        let previous = store::settings(c, s.tenant(), company).await?;
        Ok((previous, reason))
    }
    pub(super) async fn put_settings_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        input: &SettingsInput,
    ) -> Result<Settings> {
        join(tx, s, company, MANAGE_SETTINGS).await?;
        let (previous, reason) = self.validate_settings(tx.sql(), s, company, input).await?;
        if previous.as_ref().is_some_and(|(_, p)| p == &input.settings) {
            return Ok(input.settings.clone());
        }
        let next = input.settings.clone();
        let tenant = s.tenant();
        let actor = s.user();
        let imp = s.impersonator().map(|p| p.user());
        tx.mutate(move |sql|Box::pin(async move {
            let values=store::settings_json(&next);
            let mut changes=Vec::new();
            if let Some((id,_))=&previous {
                let before=store::raw(sql,Table::Settings,tenant,company,*id).await?;
                sql.execute("UPDATE finance_payables_settings SET payable_account_id=$4,quantity_tolerance_kind=$5,quantity_tolerance=$6,price_tolerance_kind=$7,price_tolerance=$8,updated_at=timezone('UTC',clock_timestamp()) WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,id,&next.payable_account_id,&next.quantity_tolerance.as_ref().map(|t|match t.kind {ToleranceKind::Percent=>"percent",ToleranceKind::Amount=>"amount"}),&next.quantity_tolerance.as_ref().map(|t|t.value),&next.price_tolerance.as_ref().map(|t|match t.kind {ToleranceKind::Percent=>"percent",ToleranceKind::Amount=>"amount"}),&next.price_tolerance.as_ref().map(|t|t.value)]).await?;
                let after=store::raw(sql,Table::Settings,tenant,company,*id).await?;
                changes.push(store::update_change(Table::Settings,*id,before,after));
            } else {
                let mut row=values.clone(); row["tenant_id"]=json!(tenant);row["company_id"]=json!(company);
                changes.push(store::insert(sql,Table::Settings,row).await?.1);
            }
            let mut history=json!({"tenant_id":tenant,"company_id":company,"reason":reason,"changed_by_user_id":actor,"changed_by_impersonator_id":imp});
            let old=previous.as_ref().map(|(_,p)|store::settings_json(p)).unwrap_or(Value::Null);
            for key in ["payable_account_id","quantity_tolerance_kind","quantity_tolerance","price_tolerance_kind","price_tolerance"] {
                history[format!("old_{key}")]=old[key].clone();history[format!("new_{key}")]=values[key].clone();
            }
            changes.push(store::insert(sql,Table::SettingsChanges,history).await?.1);
            Ok(((),changes))
        })).await?;
        Ok(input.settings.clone())
    }
    pub async fn list_tax_codes(&self, s: &Scope, company: i64) -> Result<Vec<TaxCode>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        Ok(store::taxes(&**c, s.tenant(), company).await?)
    }
    pub async fn create_tax_code(
        &self,
        s: &Scope,
        company: i64,
        input: &NewTaxCode,
    ) -> Result<TaxCode> {
        self.save_tax(s, company, None, Some(&input.code), &input.details)
            .await
    }
    pub async fn update_tax_code(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &TaxCodeDetails,
    ) -> Result<TaxCode> {
        self.save_tax(s, company, Some(id), None, input).await
    }
    async fn save_tax(
        &self,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        code: Option<&str>,
        input: &TaxCodeDetails,
    ) -> Result<TaxCode> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                MANAGE_SETTINGS,
                "finance.payables.tax_code",
            )
            .await?;
        let result = self
            .save_tax_on(&mut tx, s, company, id, code, input)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn prepare_tax(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        code: Option<&str>,
        input: &TaxCodeDetails,
    ) -> Result<(Option<TaxCode>, TaxCode, Option<String>)> {
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        account(&accounts, input.account_id, "account_id", &[])?;
        let codes = store::taxes(c, s.tenant(), company).await?;
        let previous = match id {
            Some(id) => Some(
                codes
                    .iter()
                    .find(|c| c.id == id)
                    .cloned()
                    .ok_or(PayablesError::TaxCodeNotFound)?,
            ),
            None => None,
        };
        let code = match previous.as_ref() {
            Some(p) => p.code.clone(),
            None => string(code.unwrap_or_default(), "code", 50, true)?
                .ok_or(PayablesError::InvalidOption)?,
        };
        if previous.is_none() && codes.iter().any(|c| c.code == code) {
            return Err(invalid("code", "has already been taken"));
        }
        let name = string(&input.name, "name", 200, true)?.ok_or(PayablesError::InvalidOption)?;
        let rate = Decimal::from_str_exact(input.rate.trim())
            .map_err(|_| invalid("rate", "is invalid"))?;
        if rate < Decimal::ZERO {
            return Err(invalid("rate", "must be greater than or equal to 0"));
        }
        if rate >= Decimal::from(1000) {
            return Err(invalid("rate", "must be less than 1000"));
        }
        if rate.round_dp(4) != rate {
            return Err(invalid("rate", "has more than 4 decimal places"));
        }
        let reason = optional(&input.reason, "reason", 500)?;
        let next = TaxCode {
            id: id.unwrap_or(0),
            code,
            name,
            rate,
            account_id: input.account_id,
            active: input.active,
        };
        Ok((previous, next, reason))
    }
    pub(super) async fn save_tax_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        code: Option<&str>,
        input: &TaxCodeDetails,
    ) -> Result<TaxCode> {
        join(tx, s, company, MANAGE_SETTINGS).await?;
        let (previous, next, reason) = self
            .prepare_tax(tx.sql(), s, company, id, code, input)
            .await?;
        if previous.as_ref() == Some(&next) {
            return Ok(next);
        }
        let tenant = s.tenant();
        let actor = s.user();
        let imp = s.impersonator().map(|p| p.user());
        let result=tx.mutate(move |sql|Box::pin(async move {
            let mut changes=Vec::new();
            let id=if let Some(p)=&previous {
                let before=store::raw(sql,Table::TaxCodes,tenant,company,p.id).await?;
                sql.execute("UPDATE finance_payables_tax_codes SET name=$4,rate=$5,account_id=$6,active=$7,updated_at=timezone('UTC',clock_timestamp()) WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&p.id,&next.name,&next.rate,&next.account_id,&next.active]).await?;
                let after=store::raw(sql,Table::TaxCodes,tenant,company,p.id).await?;
                changes.push(store::update_change(Table::TaxCodes,p.id,before,after));p.id
            } else {
                let (row,change)=store::insert(sql,Table::TaxCodes,json!({"tenant_id":tenant,"company_id":company,"code":next.code,"name":next.name,"rate":next.rate,"account_id":next.account_id,"active":next.active})).await?;
                changes.push(change);row["id"].as_i64().ok_or(Error::Invalid)?
            };
            let mut h=json!({"tenant_id":tenant,"company_id":company,"tax_code_id":id,"reason":reason,"changed_by_user_id":actor,"changed_by_impersonator_id":imp});
            let before=previous.as_ref().map(|p|json!(p)).unwrap_or(Value::Null);let after=json!(next);
            for k in ["name","rate","account_id","active"] {h[format!("old_{k}")]=before[k].clone();h[format!("new_{k}")]=after[k].clone();}
            changes.push(store::insert(sql,Table::TaxCodeChanges,h).await?.1);
            Ok((TaxCode {id,..next},changes))
        })).await?;
        Ok(result)
    }
    pub async fn list_settings_changes(
        &self,
        s: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<HistoryPage> {
        self.history(s, company, Table::SettingsChanges, after, limit)
            .await
    }
    pub async fn list_tax_code_changes(
        &self,
        s: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<HistoryPage> {
        self.history(s, company, Table::TaxCodeChanges, after, limit)
            .await
    }
    async fn history(
        &self,
        s: &Scope,
        company: i64,
        t: Table,
        after: i64,
        limit: i64,
    ) -> Result<HistoryPage> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        if after < 0 || !(1..=500).contains(&limit) {
            return Err(PayablesError::InvalidOption);
        }
        let n = t.name();
        let mut rows:Vec<Value>=c.query(&format!("SELECT to_jsonb(t)-'tenant_id'-'company_id' FROM {n} t WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4"),&[&s.tenant(),&company,&after,&(limit+1)]).await?.iter().map(|r|r.get(0)).collect();
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_after = if more {
            rows.last().and_then(|v| v["id"].as_i64())
        } else {
            None
        };
        Ok(HistoryPage { rows, next_after })
    }
}
