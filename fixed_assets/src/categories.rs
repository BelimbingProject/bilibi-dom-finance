//! Bilimbi.Finance.FixedAssets category defaults: revisions replace, never edit.
use crate::*;
use bilibi_finance_ledger::AccountType;
use serde_json::{json, Value};
use store::Table;

impl FixedAssets {
    pub async fn create_category(
        &self,
        s: &Scope,
        company: i64,
        name: &str,
        input: &RevisionInput,
    ) -> Result<CategoryRevision> {
        self.save_category(s, company, None, Some(name), input)
            .await
    }
    pub async fn revise_category(
        &self,
        s: &Scope,
        company: i64,
        category_id: i64,
        input: &RevisionInput,
    ) -> Result<CategoryRevision> {
        self.save_category(s, company, Some(category_id), None, input)
            .await
    }
    async fn save_category(
        &self,
        s: &Scope,
        company: i64,
        category_id: Option<i64>,
        name: Option<&str>,
        input: &RevisionInput,
    ) -> Result<CategoryRevision> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                CONFIGURE,
                "finance.fixed-assets.category",
            )
            .await?;
        let revision = self
            .save_category_on(&mut tx, s, company, category_id, name, input)
            .await?;
        tx.commit().await?;
        Ok(revision)
    }
    pub(super) async fn prepare_category(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        category_id: Option<i64>,
        name: Option<&str>,
        input: &RevisionInput,
    ) -> Result<(String, RevisionInput)> {
        self.opening_authorize(c, s, company, CONFIGURE).await?;
        let name = if let Some(id) = category_id {
            let v = store::raw(c, Table::Category, s.tenant(), company, id)
                .await?
                .ok_or(FixedAssetsError::NotFound)?;
            store::string(&v, "name")?
        } else {
            let name = text(name.unwrap_or_default(), "name", 200)?;
            if c.query_opt("SELECT id FROM finance_fixed_assets_categories WHERE tenant_id=$1 AND company_id=$2 AND name=$3",&[&s.tenant(),&company,&name]).await?.is_some() {return Err(invalid("name","has already been taken"));}
            name
        };
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        for (id, field, kind) in [
            (input.cost_account_id, "cost_account_id", AccountType::Asset),
            (
                input.accumulated_account_id,
                "accumulated_account_id",
                AccountType::Asset,
            ),
            (
                input.expense_account_id,
                "expense_account_id",
                AccountType::Expense,
            ),
            (
                input.gain_account_id,
                "gain_account_id",
                AccountType::Income,
            ),
            (
                input.loss_account_id,
                "loss_account_id",
                AccountType::Expense,
            ),
        ] {
            if !accounts
                .iter()
                .any(|a| a.id == id && a.postable() && a.account_type == kind)
            {
                return Err(invalid(
                    field,
                    format!("must be an active company {} account", kind.as_str()),
                ));
            }
        }
        if input.cost_account_id == input.accumulated_account_id {
            return Err(invalid(
                "accumulated_account_id",
                "must differ from the cost account",
            ));
        }
        let mut input = input.clone();
        input.reason = text(&input.reason, "reason", 500)?;
        input.assumptions = schedule::assumptions(input.assumptions)?;
        Ok((name, input))
    }
    pub(super) async fn save_category_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        category_id: Option<i64>,
        name: Option<&str>,
        input: &RevisionInput,
    ) -> Result<CategoryRevision> {
        self.lifecycle_writable(tx, s, company, CONFIGURE).await?;
        let (name, input) = self
            .prepare_category(tx.sql(), s, company, category_id, name, input)
            .await?;
        let identity = store::identity(s, company);
        let result = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    let category_id = if let Some(id) = category_id {
                        id
                    } else {
                        let mut row = identity.clone();
                        row["name"] = json!(name);
                        let (v, change) = store::insert(sql, Table::Category, row).await?;
                        changes.push(change);
                        store::int(&v, "id")?
                    };
                    let mut row = identity;
                    row["category_id"] = json!(category_id);
                    let Value::Object(fields) =
                        serde_json::to_value(input).map_err(|_| Error::Invalid)?
                    else {
                        return Err(Error::Invalid);
                    };
                    row.as_object_mut().ok_or(Error::Invalid)?.extend(fields);
                    let (v, change) = store::insert(sql, Table::Revision, row).await?;
                    changes.push(change);
                    Ok((store::int(&v, "id")?, changes))
                })
            })
            .await?;
        let revision = store::revision(tx.sql(), s.tenant(), company, result).await?;
        Ok(revision)
    }
    pub async fn list_categories(&self, s: &Scope, company: i64) -> Result<Vec<Category>> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.list_categories_on(&**c, s, company).await
    }
    /// Authorized category history on the caller's connection.
    pub async fn list_categories_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
    ) -> Result<Vec<Category>> {
        self.opening_authorize(c, s, company, VIEW).await?;
        let categories=c.query("SELECT id,name FROM finance_fixed_assets_categories WHERE tenant_id=$1 AND company_id=$2 ORDER BY name",&[&s.tenant(),&company]).await?;
        let mut result = Vec::new();
        for row in categories {
            let id = row.get::<_, i64>(0);
            let mut revisions = Vec::new();
            for r in c.query("SELECT id FROM finance_fixed_assets_category_revisions WHERE tenant_id=$1 AND company_id=$2 AND category_id=$3 ORDER BY id",&[&s.tenant(),&company,&id]).await? {
                revisions.push(store::revision(c,s.tenant(),company,r.get(0)).await?);
            }
            let revision = revisions.last().cloned().ok_or(Error::Invalid)?;
            result.push(Category {
                id,
                name: row.get(1),
                revision,
                revisions,
            });
        }
        Ok(result)
    }
}
