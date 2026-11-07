//! Bilimbi.Finance.Payables payment requests, decisions and owned reversals.
use crate::*;
use crate::{
    settings::{account, optional, string},
    store::Table,
};
use bilibi_finance_ledger::{
    AccountType, Approval, EntryOwner, NewEntry, NewLine, PostOptions, Preparer, Reversal,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct AllocationInput {
    pub document_id: i64,
    pub amount: Money,
}
#[derive(Clone, Debug, Serialize)]
pub struct Allocation {
    pub line_number: i32,
    pub document_id: i64,
    pub amount: Money,
}
#[derive(Clone, Debug)]
pub struct NewPayment {
    pub payment_date: Date,
    pub vendor_id: Option<i64>,
    pub supplier_key: Option<String>,
    pub paid_from_account_id: i64,
    pub reference: Option<String>,
    pub description: Option<String>,
    pub allocations: Vec<AllocationInput>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PaymentRequest {
    pub id: i64,
    pub vendor_id: Option<i64>,
    pub request_number: i64,
    #[serde(serialize_with = "as_text")]
    pub payment_date: Date,
    pub party_key: String,
    pub party_name: String,
    pub party_code: String,
    pub paid_from_account_id: i64,
    pub amount: Money,
    pub currency_code: String,
    pub reference: Option<String>,
    pub description: Option<String>,
    pub allocations: Vec<Allocation>,
    pub submitted_by_user_id: i64,
    pub submitted_by_impersonator_id: Option<i64>,
    pub submitted_at: String,
    pub status: String,
    pub decision: Option<PaymentDecision>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PaymentDecision {
    pub event: String,
    pub reason: Option<String>,
    pub payment_id: Option<i64>,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub occurred_at: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Payment {
    pub id: i64,
    pub vendor_id: Option<i64>,
    pub payment_number: i64,
    #[serde(serialize_with = "as_text")]
    pub payment_date: Date,
    pub party_key: String,
    pub party_name: String,
    pub party_code: String,
    pub paid_from_account_id: i64,
    pub amount: Money,
    pub currency_code: String,
    pub reference: Option<String>,
    pub description: Option<String>,
    pub reverses_payment_id: Option<i64>,
    pub reversed_by_payment_id: Option<i64>,
    pub entry_id: i64,
    pub entry_number: i64,
    pub posted_by_user_id: i64,
    pub posted_by_impersonator_id: Option<i64>,
    pub posted_at: String,
    pub allocations: Vec<Allocation>,
}
/// Oldest first by request number, `after` being the last number seen. With
/// `newest_first` the order is reversed and `after` is the last (lowest)
/// number seen, so the next page holds the lower ones; no `after` starts at
/// the newest.
#[derive(Clone, Debug, Default)]
pub struct PaymentRequestQuery {
    pub status: Option<String>,
    pub after: Option<i64>,
    pub limit: Option<i64>,
    pub newest_first: bool,
}
/// Oldest first by `(payment_date, id)`, `after` being the last pair seen.
/// With `newest_first` the order is reversed and `after` is the last (lowest)
/// pair seen; no `after` starts at the newest.
#[derive(Clone, Debug, Default)]
pub struct PaymentQuery {
    pub party_key: Option<String>,
    pub document_id: Option<i64>,
    pub from: Option<Date>,
    pub to: Option<Date>,
    pub after: Option<(Date, i64)>,
    pub limit: Option<i64>,
    pub newest_first: bool,
}
/// One page of payment requests, newest first, by page number and size.
#[derive(Clone, Debug)]
pub struct PaymentRequestPageQuery {
    pub status: Option<String>,
    /// Case-insensitive literal search in the supplier's name and code, the
    /// reference and the request number (up to 255 characters).
    pub search: String,
    pub page: i64,
    pub page_size: i64,
}
impl Default for PaymentRequestPageQuery {
    fn default() -> Self {
        Self {
            status: None,
            search: String::new(),
            page: 1,
            page_size: 25,
        }
    }
}
#[derive(Clone, Debug)]
pub struct PaymentRequestPage {
    pub requests: Vec<PaymentRequest>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

fn limit(value: Option<i64>) -> Result<i64> {
    let n = value.unwrap_or(100);
    if !(1..=500).contains(&n) {
        return Err(PayablesError::InvalidOption);
    }
    Ok(n)
}
impl Payables {
    pub fn payment_request_statuses() -> &'static [&'static str] {
        &["submitted", "posted", "rejected"]
    }
    pub async fn submit_payment(
        &self,
        s: &Scope,
        company: i64,
        input: &NewPayment,
    ) -> Result<PaymentRequest> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                SUBMIT_PAYMENTS,
                "finance.payables.submit_payment",
            )
            .await?;
        let result = self.submit_payment_on(&mut tx, s, company, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn submit_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        input: &NewPayment,
    ) -> Result<PaymentRequest> {
        join(tx, s, company, SUBMIT_PAYMENTS).await?;
        let (mut value, _) = self
            .validate_payment(tx.sql(), s, company, input, false)
            .await?;
        value["submitted_by_user_id"] = json!(s.user());
        value["submitted_by_impersonator_id"] = json!(s.impersonator().map(|p| p.user()));
        let allocations = input.allocations.clone();
        let tenant = s.tenant();
        let id=tx.mutate(move |sql| Box::pin(async move {
            let (row,change)=store::insert(sql,Table::Requests,value).await?;
            let id=row["id"].as_i64().ok_or(Error::Invalid)?;
            let mut changes=vec![change];
            for (i,a) in allocations.iter().enumerate() {
                let (_,change)=store::insert(sql,Table::RequestAllocations,json!({"tenant_id":tenant,"company_id":company,"request_id":id,"line_number":i+1,"document_id":a.document_id,"amount":a.amount})).await?;
                changes.push(change);
            }
            Ok((id,changes))
        })).await?;
        let result = request(tx.sql(), tenant, company, id).await?;
        Ok(result)
    }
    pub async fn approve_payment(&self, s: &Scope, company: i64, id: i64) -> Result<Payment> {
        self.approve_payment_with_override(s, company, id, None)
            .await
    }
    pub async fn approve_payment_with_override(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        override_reason: Option<String>,
    ) -> Result<Payment> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                POST_PAYMENTS,
                "finance.payables.approve_payment",
            )
            .await?;
        let result = self
            .approve_payment_on(&mut tx, s, company, id, override_reason)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn approve_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        override_reason: Option<String>,
    ) -> Result<Payment> {
        join(tx, s, company, POST_PAYMENTS).await?;
        let r = request(tx.sql(), s.tenant(), company, id).await?;
        if r.status != "submitted" {
            return Err(PayablesError::PaymentRequestDecided);
        }
        self.ledger
            .check_approval(tx, s, company, &approval(&r, override_reason))
            .await?;
        let input = requested(&r);
        let (mut value, groups) = self
            .validate_payment(tx.sql(), s, company, &input, true)
            .await?;
        // Reserve only the record ID; the database assigns the payment number.
        let payment_id: i64 = tx
            .sql()
            .query_one(
                "SELECT nextval(pg_get_serial_sequence('finance_payables_payments','id'))",
                &[],
            )
            .await?
            .get(0);
        let (new, mut options) = posting(&input, &value, &groups)?;
        options.owner = Some(owner(payment_id));
        let entry = self
            .ledger
            .post_entry_in(tx, s, company, &new, &options)
            .await?;
        value["entry_id"] = json!(entry.id);
        value["entry_number"] = json!(entry.entry_number);
        value["posted_by_user_id"] = json!(s.user());
        value["posted_by_impersonator_id"] = json!(s.impersonator().map(|p| p.user()));
        insert_payment(tx, s, company, payment_id, value, input.allocations).await?;
        decide(tx, s, company, id, "posted", None, Some(payment_id)).await?;
        let result = payment(tx.sql(), s.tenant(), company, payment_id).await?;
        Ok(result)
    }
    pub async fn reject_payment(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        reason: &str,
    ) -> Result<PaymentRequest> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                POST_PAYMENTS,
                "finance.payables.reject_payment",
            )
            .await?;
        let result = self
            .reject_payment_on(&mut tx, s, company, id, reason)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn reject_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        reason: &str,
    ) -> Result<PaymentRequest> {
        join(tx, s, company, POST_PAYMENTS).await?;
        let r = request(tx.sql(), s.tenant(), company, id).await?;
        if r.status != "submitted" {
            return Err(PayablesError::PaymentRequestDecided);
        }
        let reason = string(reason, "reason", 500, true)?;
        decide(tx, s, company, id, "rejected", reason, None).await?;
        let result = request(tx.sql(), s.tenant(), company, id).await?;
        Ok(result)
    }
    pub async fn reverse_payment(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        date: Date,
        description: Option<String>,
    ) -> Result<Payment> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                POST_PAYMENTS,
                "finance.payables.reverse_payment",
            )
            .await?;
        let result = self
            .reverse_payment_on(&mut tx, s, company, id, date, description)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn prepare_payment_reversal(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
        date: Date,
        description: Option<String>,
    ) -> Result<(Payment, Reversal)> {
        let p = payment(c, s.tenant(), company, id).await?;
        if p.reverses_payment_id.is_some() {
            return Err(PayablesError::ReversalNotReversible);
        }
        if p.reversed_by_payment_id.is_some() {
            return Err(PayablesError::AlreadyReversed);
        }
        if date < p.payment_date {
            return Err(invalid(
                "payment_date",
                "cannot be before the reversed payment's date",
            ));
        }
        let description = optional(&description, "description", 500)?
            .unwrap_or_else(|| format!("Reversal of payment {}", p.payment_number));
        Ok((
            p,
            Reversal {
                entry_date: date,
                description,
                reference: None,
            },
        ))
    }
    pub(super) async fn reverse_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        date: Date,
        description: Option<String>,
    ) -> Result<Payment> {
        join(tx, s, company, POST_PAYMENTS).await?;
        let (p, reversal) = self
            .prepare_payment_reversal(tx.sql(), s, company, id, date, description)
            .await?;
        let description = reversal.description.clone();
        let entry = self
            .ledger
            .reverse_owned_entry(tx, s, company, p.entry_id, &owner(id), &reversal)
            .await?;
        let payment_id = tx
            .sql()
            .query_one(
                "SELECT nextval(pg_get_serial_sequence('finance_payables_payments','id'))",
                &[],
            )
            .await?
            .get(0);
        insert_payment(tx,s,company,payment_id,json!({"tenant_id":s.tenant(),"company_id":company,"payment_date":date.to_string(),"party_key":p.party_key,"party_name":p.party_name,"paid_from_account_id":p.paid_from_account_id,"amount":p.amount,"currency_code":p.currency_code,"reference":p.reference,"description":description,"reverses_payment_id":id,"entry_id":entry.id,"entry_number":entry.entry_number,"posted_by_user_id":s.user(),"posted_by_impersonator_id":s.impersonator().map(|p|p.user())}),vec![]).await?;
        let result = payment(tx.sql(), s.tenant(), company, payment_id).await?;
        Ok(result)
    }
    pub(super) async fn validate_payment(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &NewPayment,
        decided_request: bool,
    ) -> Result<(Value, BTreeMap<i64, Money>)> {
        let book = self.ledger.get_book_on(c, s, company).await?;
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        account(
            &accounts,
            input.paid_from_account_id,
            "paid_from_account_id",
            &[AccountType::Asset, AccountType::Liability],
        )?;
        let first = input
            .allocations
            .first()
            .ok_or_else(|| invalid("allocations", "needs at least one allocation"))?;
        let first_bill = store::document_record(c, s.tenant(), company, first.document_id)
            .await?
            .ok_or_else(|| invalid("allocations", "is not a document of this company"))?;
        let key = first_bill.party.key.clone();
        let owner = vendors::of_key(c, s.tenant(), company, &key).await?;
        if let Some(vendor) = input.vendor_id {
            self.vendor_on(c, s, company, vendor).await?;
            if owner != Some(vendor) {
                return Err(invalid(
                    "vendor_id",
                    "does not match the allocated bill's supplier",
                ));
            }
        } else if input.supplier_key.as_deref() != Some(key.as_str()) {
            return Err(invalid(
                "vendor_id",
                "is required; a bill group that belongs to no vendor may instead name its supplier_key",
            ));
        }
        if input.vendor_id.is_none() && !decided_request && owner.is_some() {
            return Err(invalid(
                "vendor_id",
                "is required for a linked vendor's bills",
            ));
        }
        if input.supplier_key.as_ref().is_some_and(|k| k != &key) {
            return Err(invalid(
                "supplier_key",
                "does not match the allocated bills",
            ));
        }
        let reference = optional(&input.reference, "reference", 100)?;
        let description = optional(&input.description, "description", 500)?;
        if input.allocations.is_empty() {
            return Err(invalid("allocations", "needs at least one allocation"));
        }
        if input.allocations.len() > 500 {
            return Err(invalid("allocations", "should have at most 500 item(s)"));
        }
        let mut seen = BTreeSet::new();
        let mut groups = BTreeMap::new();
        let mut total = Money::ZERO;
        let mut latest: Option<(Date, i64, String)> = None;
        for (i, a) in input.allocations.iter().enumerate() {
            let checked: Result<Document> = async {
                if !seen.insert(a.document_id) {
                    return Err(invalid("document_id", "is allocated twice"));
                }
                let b = store::document(c, s.tenant(), company, a.document_id)
                    .await?
                    .ok_or_else(|| invalid("document_id", "is not a document of this company"))?;
                if b.document_type != "bill" || b.status != "posted" {
                    return Err(invalid("document_id", "is not a posted bill"));
                }
                if b.party.key != key {
                    return Err(invalid(
                        "document_id",
                        "has another supplier key; pay each supplier key separately",
                    ));
                }
                crate::opening::settlement_date(
                    c,
                    s.tenant(),
                    company,
                    b.id,
                    input.payment_date,
                    "document_id",
                )
                .await?;
                if b.document_date > input.payment_date {
                    return Err(invalid("document_id", "is dated after the payment"));
                }
                let amount = Money::new(a.amount.amount(), book.minor_units)
                    .map_err(|e| invalid("amount", e.to_string()))?;
                if !amount.is_positive() {
                    return Err(invalid("amount", "must be greater than 0"));
                }
                if amount
                    > balances::lowest(c, s.tenant(), company, b.id, input.payment_date).await?
                {
                    return Err(invalid("amount", "exceeds what is owed on the bill"));
                }
                Ok(b)
            }
            .await;
            let b = checked.map_err(|e| match e {
                PayablesError::Invalid(mut es) => {
                    for e in &mut es {
                        e.line = Some(i + 1);
                    }
                    PayablesError::Invalid(es)
                }
                e => e,
            })?;
            let group = groups
                .entry(b.payable_account_id.ok_or(PayablesError::NotPostedBill)?)
                .or_insert(Money::ZERO);
            *group = *group + a.amount;
            total = total + a.amount;
            if latest
                .as_ref()
                .is_none_or(|(date, id, _)| (b.document_date, b.id) > (*date, *id))
            {
                latest = Some((b.document_date, b.id, b.party.name));
            }
        }
        Money::new(total.amount(), book.minor_units)
            .map_err(|e| invalid("amount", e.to_string()))?;
        let name = latest.ok_or(PayablesError::InvalidOption)?.2;
        Ok((
            json!({"tenant_id":s.tenant(),"company_id":company,"payment_date":input.payment_date.to_string(),"party_key":key,"party_name":name,"paid_from_account_id":input.paid_from_account_id,"amount":total,"currency_code":book.currency_code,"reference":reference,"description":description}),
            groups,
        ))
    }
    pub async fn get_payment_request(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<PaymentRequest> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        request(&**c, s.tenant(), company, id).await
    }
    pub async fn get_payment(&self, s: &Scope, company: i64, id: i64) -> Result<Payment> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        payment(&**c, s.tenant(), company, id).await
    }
    pub async fn list_payment_requests(
        &self,
        s: &Scope,
        company: i64,
        q: &PaymentRequestQuery,
    ) -> Result<Vec<PaymentRequest>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let n = limit(q.limit)?;
        if q.after.is_some_and(|a| a < 0) || !valid_status(&q.status) {
            return Err(PayablesError::InvalidOption);
        }
        let (cmp, order) = if q.newest_first {
            ("<", "DESC")
        } else {
            (">", "")
        };
        let rows=c.query(&format!("SELECT r.id FROM finance_payables_payment_requests r LEFT JOIN finance_payables_payment_request_events e ON e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.request_id=r.id WHERE r.tenant_id=$1 AND r.company_id=$2 AND ($3::text IS NULL OR COALESCE(e.event,'submitted')=$3) AND ($4::bigint IS NULL OR r.request_number{cmp}$4) ORDER BY r.request_number {order} LIMIT $5"),&[&s.tenant(),&company,&q.status,&q.after,&n]).await?;
        let mut result = Vec::new();
        for row in rows {
            result.push(request(&**c, s.tenant(), company, row.get(0)).await?);
        }
        Ok(result)
    }
    pub async fn page_payment_requests(
        &self,
        s: &Scope,
        company: i64,
        q: &PaymentRequestPageQuery,
    ) -> Result<PaymentRequestPage> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        if q.page < 1
            || ![25, 50, 100].contains(&q.page_size)
            || q.search.chars().count() > 255
            || !valid_status(&q.status)
        {
            return Err(PayablesError::InvalidOption);
        }
        let search = q.search.trim();
        let total: i64=c.query_one("SELECT count(*) FROM finance_payables_payment_requests r LEFT JOIN finance_payables_payment_request_events e ON e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.request_id=r.id WHERE r.tenant_id=$1 AND r.company_id=$2 AND ($3::text IS NULL OR COALESCE(e.event,'submitted')=$3) AND (strpos(lower(r.party_name),lower($4))>0 OR EXISTS(SELECT 1 FROM finance_payables_payment_request_allocations a JOIN finance_payables_documents d ON d.tenant_id=a.tenant_id AND d.company_id=a.company_id AND d.id=a.document_id WHERE a.tenant_id=r.tenant_id AND a.company_id=r.company_id AND a.request_id=r.id AND strpos(lower(d.party_code),lower($4))>0) OR strpos(lower(COALESCE(r.reference,'')),lower($4))>0 OR strpos(r.request_number::text,$4)>0)",&[&s.tenant(),&company,&q.status,&search]).await?.get(0);
        let page = q.page.min(((total + q.page_size - 1) / q.page_size).max(1));
        let offset = (page - 1) * q.page_size;
        let rows=c.query("SELECT r.id FROM finance_payables_payment_requests r LEFT JOIN finance_payables_payment_request_events e ON e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.request_id=r.id WHERE r.tenant_id=$1 AND r.company_id=$2 AND ($3::text IS NULL OR COALESCE(e.event,'submitted')=$3) AND (strpos(lower(r.party_name),lower($4))>0 OR EXISTS(SELECT 1 FROM finance_payables_payment_request_allocations a JOIN finance_payables_documents d ON d.tenant_id=a.tenant_id AND d.company_id=a.company_id AND d.id=a.document_id WHERE a.tenant_id=r.tenant_id AND a.company_id=r.company_id AND a.request_id=r.id AND strpos(lower(d.party_code),lower($4))>0) OR strpos(lower(COALESCE(r.reference,'')),lower($4))>0 OR strpos(r.request_number::text,$4)>0) ORDER BY r.request_number DESC LIMIT $5 OFFSET $6",&[&s.tenant(),&company,&q.status,&search,&q.page_size,&offset]).await?;
        let mut requests = Vec::new();
        for row in rows {
            requests.push(request(&**c, s.tenant(), company, row.get(0)).await?);
        }
        Ok(PaymentRequestPage {
            requests,
            total,
            page,
            page_size: q.page_size,
        })
    }
    pub async fn list_payments(
        &self,
        s: &Scope,
        company: i64,
        q: &PaymentQuery,
    ) -> Result<Vec<Payment>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        let n = limit(q.limit)?;
        if q.from.zip(q.to).is_some_and(|(f, t)| f > t) || q.after.is_some_and(|(_, id)| id <= 0) {
            return Err(PayablesError::InvalidOption);
        }
        let (cmp, order) = if q.newest_first {
            ("<", "DESC")
        } else {
            (">", "")
        };
        let from = q.from.map(|d| d.to_string());
        let to = q.to.map(|d| d.to_string());
        let after = q.after.map(|(d, _)| d.to_string());
        let after_id = q.after.map(|(_, id)| id);
        let rows=c.query(&format!("SELECT p.id FROM finance_payables_payments p WHERE p.tenant_id=$1 AND p.company_id=$2 AND ($3::text IS NULL OR p.party_key=$3) AND ($4::bigint IS NULL OR EXISTS (SELECT 1 FROM finance_payables_payment_allocations a WHERE a.tenant_id=p.tenant_id AND a.company_id=p.company_id AND a.document_id=$4 AND a.payment_id=COALESCE(p.reverses_payment_id,p.id))) AND ($5::text IS NULL OR p.payment_date >= $5::text::date) AND ($6::text IS NULL OR p.payment_date <= $6::text::date) AND ($7::text IS NULL OR (p.payment_date,p.id){cmp}($7::text::date,$8)) ORDER BY p.payment_date {order},p.id {order} LIMIT $9"),&[&s.tenant(),&company,&q.party_key,&q.document_id,&from,&to,&after,&after_id,&n]).await?;
        let mut result = Vec::new();
        for row in rows {
            result.push(payment(&**c, s.tenant(), company, row.get(0)).await?);
        }
        Ok(result)
    }
}
fn valid_status(status: &Option<String>) -> bool {
    status
        .as_ref()
        .is_none_or(|v| Payables::payment_request_statuses().contains(&v.as_str()))
}
pub(super) fn approval(r: &PaymentRequest, override_reason: Option<String>) -> Approval {
    Approval {
        source: MODULE.into(),
        action: "payment_approval".into(),
        subject_type: "payment_request".into(),
        subject_id: r.id,
        subject: format!(
            "Payment request {} to {}, {} {}",
            r.request_number, r.party_name, r.amount, r.currency_code
        )
        .chars()
        .take(200)
        .collect(),
        prepared_by: vec![Preparer {
            user_id: r.submitted_by_user_id,
            impersonator_id: r.submitted_by_impersonator_id,
        }],
        override_reason,
    }
}
pub(super) fn requested(r: &PaymentRequest) -> NewPayment {
    NewPayment {
        payment_date: r.payment_date,
        vendor_id: r.vendor_id,
        supplier_key: Some(r.party_key.clone()),
        paid_from_account_id: r.paid_from_account_id,
        reference: r.reference.clone(),
        description: r.description.clone(),
        allocations: r
            .allocations
            .iter()
            .map(|a| AllocationInput {
                document_id: a.document_id,
                amount: a.amount,
            })
            .collect(),
    }
}
pub(super) fn posting(
    input: &NewPayment,
    value: &Value,
    groups: &BTreeMap<i64, Money>,
) -> Result<(NewEntry, PostOptions)> {
    let name = value["party_name"]
        .as_str()
        .ok_or(PayablesError::InvalidOption)?;
    let amount = store::money(input.allocations.iter().map(|a| a.amount.amount()).sum())?;
    let mut lines: Vec<_> = groups
        .iter()
        .map(|(id, a)| NewLine::debit(*id, *a).described(name))
        .collect();
    lines.push(NewLine::credit(input.paid_from_account_id, amount).described(name));
    Ok((
        NewEntry {
            entry_date: input.payment_date,
            description: input
                .description
                .clone()
                .unwrap_or_else(|| format!("Payment to {name}").chars().take(500).collect()),
            reference: input.reference.clone(),
            lines,
        },
        PostOptions {
            owner: None,
            settlement_account_ids: groups.keys().copied().collect(),
        },
    ))
}
pub(super) fn owner(id: i64) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("payment:{id}"),
    }
}
async fn insert_payment(
    tx: &mut MutationTransaction<'_>,
    s: &Scope,
    company: i64,
    id: i64,
    mut value: Value,
    allocations: Vec<AllocationInput>,
) -> Result<()> {
    let tenant = s.tenant();
    value["id"] = json!(id);
    tx.mutate(move |sql|Box::pin(async move {
        // The reserved ID links the original entry and payment atomically.
        let row=sql.query_one("INSERT INTO finance_payables_payments SELECT (jsonb_populate_record(NULL::finance_payables_payments,$1)).* RETURNING to_jsonb(finance_payables_payments)",&[&value]).await?;
        let row:Value=row.get(0);
        let mut changes=vec![bilibi_database::Change {kind:"finance.payables.payment".into(),id,old:Value::Null,new:row}];
        for (i,a) in allocations.iter().enumerate() {let (_,change)=store::insert(sql,Table::Allocations,json!({"tenant_id":tenant,"company_id":company,"payment_id":id,"line_number":i+1,"document_id":a.document_id,"amount":a.amount})).await?;changes.push(change);}
        Ok(((),changes))
    })).await?;
    Ok(())
}
async fn decide(
    tx: &mut MutationTransaction<'_>,
    s: &Scope,
    company: i64,
    id: i64,
    event: &str,
    reason: Option<String>,
    payment_id: Option<i64>,
) -> Result<()> {
    let value = json!({"tenant_id":s.tenant(),"company_id":company,"request_id":id,"event":event,"reason":reason,"payment_id":payment_id,"actor_user_id":s.user(),"actor_impersonator_id":s.impersonator().map(|p|p.user())});
    tx.mutate(move |sql| {
        Box::pin(async move {
            let (_, change) = store::insert(sql, Table::RequestEvents, value).await?;
            Ok(((), vec![change]))
        })
    })
    .await?;
    Ok(())
}
async fn allocations(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
    requests: bool,
) -> Result<Vec<Allocation>> {
    let (table, key) = if requests {
        ("finance_payables_payment_request_allocations", "request_id")
    } else {
        ("finance_payables_payment_allocations", "payment_id")
    };
    let rows=c.query(&format!("SELECT line_number,document_id,amount FROM {table} WHERE tenant_id=$1 AND company_id=$2 AND {key}=$3 ORDER BY line_number"),&[&tenant,&company,&id]).await?;
    rows.iter()
        .map(|r| {
            Ok(Allocation {
                line_number: r.get(0),
                document_id: r.get(1),
                amount: store::money(r.get(2))?,
            })
        })
        .collect()
}
// Codes are immutable Core facts already frozen on the allocated bills.
async fn allocation_code(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
    requests: bool,
) -> Result<String> {
    let (table, key) = if requests {
        ("finance_payables_payment_request_allocations", "request_id")
    } else {
        ("finance_payables_payment_allocations", "payment_id")
    };
    let row=c.query_opt(&format!("SELECT d.party_code FROM {table} a JOIN finance_payables_documents d ON d.tenant_id=a.tenant_id AND d.company_id=a.company_id AND d.id=a.document_id WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.{key}=$3 ORDER BY a.line_number LIMIT 1"),&[&tenant,&company,&id]).await?.ok_or(Error::Internal)?;
    Ok(row.get(0))
}
pub(crate) async fn request(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<PaymentRequest> {
    let r=c.query_opt("SELECT request_number,payment_date::text,party_key,party_name,paid_from_account_id,amount,currency_code,reference,description,submitted_by_user_id,submitted_by_impersonator_id,submitted_at::text FROM finance_payables_payment_requests WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id]).await?.ok_or(PayablesError::PaymentRequestNotFound)?;
    let decision=c.query_opt("SELECT event,reason,payment_id,actor_user_id,actor_impersonator_id,occurred_at::text FROM finance_payables_payment_request_events WHERE tenant_id=$1 AND company_id=$2 AND request_id=$3",&[&tenant,&company,&id]).await?.map(|e|PaymentDecision {event:e.get(0),reason:e.get(1),payment_id:e.get(2),actor_user_id:e.get(3),actor_impersonator_id:e.get(4),occurred_at:e.get(5)});
    Ok(PaymentRequest {
        id,
        vendor_id: vendors::of_key(c, tenant, company, &r.get::<_, String>(2)).await?,
        request_number: r.get(0),
        payment_date: r.get::<_, String>(1).parse().map_err(|_| Error::Invalid)?,
        party_key: r.get(2),
        party_name: r.get(3),
        party_code: allocation_code(c, tenant, company, id, true).await?,
        paid_from_account_id: r.get(4),
        amount: store::money(r.get(5))?,
        currency_code: r.get(6),
        reference: r.get(7),
        description: r.get(8),
        submitted_by_user_id: r.get(9),
        submitted_by_impersonator_id: r.get(10),
        submitted_at: r.get(11),
        status: decision.as_ref().map_or("submitted", |e| &e.event).into(),
        decision,
        allocations: allocations(c, tenant, company, id, true).await?,
    })
}
pub(crate) async fn payment(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<Payment> {
    let r=c.query_opt("SELECT payment_number,payment_date::text,party_key,party_name,paid_from_account_id,amount,currency_code,reference,description,reverses_payment_id,entry_id,entry_number,posted_by_user_id,posted_by_impersonator_id,posted_at::text FROM finance_payables_payments WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id]).await?.ok_or(PayablesError::PaymentNotFound)?;
    let reversed=c.query_opt("SELECT id FROM finance_payables_payments WHERE tenant_id=$1 AND company_id=$2 AND reverses_payment_id=$3",&[&tenant,&company,&id]).await?.map(|r|r.get(0));
    Ok(Payment {
        id,
        vendor_id: vendors::of_key(c, tenant, company, &r.get::<_, String>(2)).await?,
        payment_number: r.get(0),
        payment_date: r.get::<_, String>(1).parse().map_err(|_| Error::Invalid)?,
        party_key: r.get(2),
        party_name: r.get(3),
        party_code: allocation_code(
            c,
            tenant,
            company,
            r.get::<_, Option<i64>>(9).unwrap_or(id),
            false,
        )
        .await?,
        paid_from_account_id: r.get(4),
        amount: store::money(r.get(5))?,
        currency_code: r.get(6),
        reference: r.get(7),
        description: r.get(8),
        reverses_payment_id: r.get(9),
        entry_id: r.get(10),
        entry_number: r.get(11),
        posted_by_user_id: r.get(12),
        posted_by_impersonator_id: r.get(13),
        posted_at: r.get(14),
        reversed_by_payment_id: reversed,
        allocations: allocations(c, tenant, company, id, false).await?,
    })
}
