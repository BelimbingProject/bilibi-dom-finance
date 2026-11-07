//! Vendor master. Core owns identity; a vendor's bills, payments and aged
//! balances are grouped under its one settlement key, `vendor:{id}`.
use crate::*;
use crate::{settings::account, store::Table};
use bilibi_finance_ledger::{AccountType, PartyFacts, PartyLinks};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vendor {
    pub id: i64,
    pub code: String,
    pub record_kind: String,
    pub record_id: String,
    pub payment_terms_days: i32,
    pub payable_account_id: Option<i64>,
    pub expense_account_id: Option<i64>,
    pub tax_code_id: Option<i64>,
    pub active: bool,
    pub party: PartyFacts,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorInput {
    #[serde(default)]
    pub record_kind: Option<String>,
    #[serde(default)]
    pub record_id: Option<i64>,
    #[serde(default)]
    pub new_company: Option<Value>,
    #[serde(default)]
    pub new_person: Option<Value>,
    #[serde(default)]
    pub payment_terms_days: i32,
    #[serde(default)]
    pub payable_account_id: Option<i64>,
    #[serde(default)]
    pub expense_account_id: Option<i64>,
    #[serde(default)]
    pub tax_code_id: Option<i64>,
    pub active: bool,
}
/// The vendor a settlement key belongs to, when it is the key of a vendor of
/// this company.
pub(crate) async fn of_key(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    key: &str,
) -> bilibi_database::Result<Option<i64>> {
    let Some(id) = key
        .strip_prefix("vendor:")
        .and_then(|id| id.parse::<i64>().ok())
    else {
        return Ok(None);
    };
    if self::key(id) != key {
        return Ok(None);
    }
    Ok(c.query_opt(
        "SELECT id FROM finance_payables_vendors WHERE tenant_id=$1 AND company_id=$2 AND id=$3",
        &[&tenant, &company, &id],
    )
    .await?
    .map(|r| r.get(0)))
}
pub(crate) fn key(id: i64) -> String {
    format!("vendor:{id}")
}
impl Payables {
    pub(crate) async fn vendor_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Vendor> {
        let row = c.query_opt("SELECT to_jsonb(v) FROM finance_payables_vendors v WHERE tenant_id=$1 AND company_id=$2 AND id=$3", &[&s.tenant(), &company,&id]).await?.ok_or_else(|| invalid("vendor_id","is not a vendor of this company"))?;
        let mut value: Value = row.get(0);
        let core_id = value["record_id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .ok_or(Error::Invalid)?;
        let kind = value["record_kind"].as_str().ok_or(Error::Invalid)?;
        let facts = PartyLinks::new(&self.parties)
            .summary(c, s, kind, core_id)
            .await?;
        value["code"] = json!(facts.code);
        value["party"] = json!(facts);
        serde_json::from_value(value).map_err(|_| Error::Invalid.into())
    }
    pub async fn get_vendor(&self, s: &Scope, company: i64, id: i64) -> Result<Vendor> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        self.vendor_on(&**c, s, company, id).await
    }
    pub async fn list_vendors(
        &self,
        s: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Vendor>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        self.vendors_on(&**c, s, company, after, limit).await
    }
    pub(crate) async fn vendors_on(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Vendor>> {
        if after < 0 || !(1..=500).contains(&limit) {
            return Err(PayablesError::InvalidOption);
        }
        let mut records = Vec::new();
        for row in c.query("SELECT id FROM finance_payables_vendors WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4",&[&s.tenant(),&company,&after,&limit]).await? { records.push(self.vendor_on(c,s,company,row.get(0)).await?); }
        Ok(records)
    }
    pub(crate) async fn prepare_vendor(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        input: &VendorInput,
    ) -> Result<Value> {
        if !(0..=3650).contains(&input.payment_terms_days) {
            return Err(invalid("payment_terms_days", "must be between 0 and 3650"));
        }
        let attrs = serde_json::to_value(input).map_err(|_| Error::Internal)?;
        let (kind, party_id, target) = if let Some(id) = id {
            let old = self.vendor_on(c, s, company, id).await?;
            if input.new_company.is_some()
                || input.new_person.is_some()
                || input
                    .record_kind
                    .as_ref()
                    .is_some_and(|k| k != &old.record_kind)
                || input.record_id.is_some_and(|id| id != old.party.id)
            {
                return Err(invalid("record_id", "the linked Core party cannot change"));
            }
            (old.record_kind, Some(old.party.id), Value::Null)
        } else {
            let target = PartyLinks::new(&self.parties)
                .prepare_target(&self.records, c, s, company, &attrs)
                .await?;
            if let Some(id) = target.id {
                if c.query_opt("SELECT id FROM finance_payables_vendors WHERE tenant_id=$1 AND company_id=$2 AND record_kind=$3 AND record_id=$4",&[&s.tenant(),&company,&target.kind,&id.to_string()]).await?.is_some(){return Err(invalid("record_id","is already a supplier of the current company"));}
            }
            let relationship = PartyLinks::new(&self.parties)
                .relationship(c, s, company, &target.kind, target.id, "vendor")
                .await?;
            let mut review = json!(target);
            review["relationship"] = relationship;
            (target.kind, target.id, review)
        };
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        if let Some(id) = input.payable_account_id {
            account(
                &accounts,
                id,
                "payable_account_id",
                &[AccountType::Liability],
            )?;
        }
        if let Some(id) = input.expense_account_id {
            account(
                &accounts,
                id,
                "expense_account_id",
                &[AccountType::Expense, AccountType::Asset],
            )?;
        }
        if let Some(id) = input.tax_code_id {
            if !store::taxes(c, s.tenant(), company)
                .await?
                .iter()
                .any(|t| t.id == id && t.active)
            {
                return Err(invalid(
                    "tax_code_id",
                    "is not an active tax code of this company",
                ));
            }
        }
        Ok(
            json!({"tenant_id":s.tenant(),"company_id":company,"record_kind":kind,"record_id":party_id.map(|id|id.to_string()),"_target":target,"payment_terms_days":input.payment_terms_days,"payable_account_id":input.payable_account_id,"expense_account_id":input.expense_account_id,"tax_code_id":input.tax_code_id,"active":input.active}),
        )
    }
    pub async fn create_vendor(
        &self,
        s: &Scope,
        company: i64,
        input: &VendorInput,
    ) -> Result<Vendor> {
        self.save_vendor(s, company, None, input).await
    }
    pub async fn update_vendor(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &VendorInput,
    ) -> Result<Vendor> {
        self.save_vendor(s, company, Some(id), input).await
    }
    async fn save_vendor(
        &self,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        input: &VendorInput,
    ) -> Result<Vendor> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                MANAGE_SETTINGS,
                "finance.payables.vendor.save",
            )
            .await?;
        let v = self.save_vendor_on(&mut tx, s, company, id, input).await?;
        tx.commit().await?;
        Ok(v)
    }
    pub(crate) async fn save_vendor_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: Option<i64>,
        input: &VendorInput,
    ) -> Result<Vendor> {
        join(tx, s, company, MANAGE_SETTINGS).await?;
        let mut value = self.prepare_vendor(tx.sql(), s, company, id, input).await?;
        if id.is_none() {
            let attrs = serde_json::to_value(input).map_err(|_| Error::Internal)?;
            let party = PartyLinks::new(&self.parties)
                .compose_target(&self.records, tx, s, company, &attrs)
                .await?;
            PartyLinks::new(&self.parties)
                .assign_relationship(tx, s, company, &party, "vendor")
                .await?;
            value["record_kind"] = json!(party.kind);
            value["record_id"] = json!(party.id.to_string());
        }
        value
            .as_object_mut()
            .ok_or(Error::Internal)?
            .remove("_target");
        let reserved = if id.is_none() {
            let reserved: i64 = tx
                .sql()
                .query_one(
                    "SELECT nextval(pg_get_serial_sequence('finance_payables_vendors','id'))",
                    &[],
                )
                .await?
                .get(0);
            let key = key(reserved);
            if tx.sql().query_opt("SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND party_key=$3 LIMIT 1",&[&s.tenant(),&company,&key]).await?.is_some() {
                return Err(invalid("base","the new vendor key collides with historical evidence; retry to allocate another identifier"));
            }
            value["id"] = json!(reserved);
            Some(reserved)
        } else {
            None
        };
        let tenant = s.tenant();
        let id=tx.mutate(move |sql| Box::pin(async move {
            if let Some(id)=id {
                let before=store::raw(sql,Table::Vendors,tenant,company,id).await?;
                sql.execute("UPDATE finance_payables_vendors SET payment_terms_days=($4::jsonb->>'payment_terms_days')::int,payable_account_id=($4::jsonb->>'payable_account_id')::bigint,expense_account_id=($4::jsonb->>'expense_account_id')::bigint,tax_code_id=($4::jsonb->>'tax_code_id')::bigint,active=($4::jsonb->>'active')::boolean,updated_at=timezone('UTC',clock_timestamp()) WHERE tenant_id=$1 AND company_id=$2 AND id=$3", &[&tenant,&company,&id,&value]).await?;
                let after=store::raw(sql,Table::Vendors,tenant,company,id).await?;
                Ok((id,vec![store::update_change(Table::Vendors,id,before,after)]))
            } else {
                let id=reserved.ok_or(Error::Invalid)?;
                let row=sql.query_one("INSERT INTO finance_payables_vendors SELECT (jsonb_populate_record(NULL::finance_payables_vendors,$1::jsonb || jsonb_build_object('inserted_at',timezone('UTC',clock_timestamp()),'updated_at',timezone('UTC',clock_timestamp())))).* RETURNING to_jsonb(finance_payables_vendors)",&[&value]).await?;
                let change=bilibi_database::Change{kind:"finance.payables.vendor".into(),id,old:Value::Null,new:row.get(0)};
                Ok((id,vec![change]))
            }
        })).await?;
        self.vendor_on(tx.sql(), s, company, id).await
    }
    pub(crate) async fn frozen_vendor_party(
        &self,
        c: &(impl GenericClient + Sync),
        s: &Scope,
        kind: &str,
        core_party: i64,
    ) -> Result<Value> {
        let facts = bilibi_finance_ledger::PartyLinks::new(&self.parties)
            .frozen_party(c, s, kind, core_party, "vendor_id", false)
            .await?;
        for (field, label, max) in [
            ("party_name", "name", 200),
            ("party_tax_identifier", "tax ID", 50),
            ("party_registration_identifier", "registration number", 50),
        ] {
            if facts[field]
                .as_str()
                .is_some_and(|v| v.chars().count() > max)
            {
                return Err(invalid(
                    "vendor_id",
                    format!("the party's {label} exceeds the document's {max} character limit"),
                ));
            }
        }
        Ok(facts)
    }
    pub(crate) fn billable(active: bool, party_status: &str) -> Result<()> {
        if !active {
            return Err(invalid("vendor_id", "is an inactive vendor"));
        }
        match party_status {
            "active" => Ok(()),
            "pending" => Err(invalid(
                "vendor_id",
                "is a pending company; activate it in Companies before recording a bill",
            )),
            "suspended" => Err(invalid(
                "vendor_id",
                "is a suspended company; reactivate it in Companies before recording a bill",
            )),
            "archived" => Err(invalid("vendor_id", "is an archived company")),
            _ => Err(invalid(
                "vendor_id",
                "is linked to a company that is not available",
            )),
        }
    }
}
