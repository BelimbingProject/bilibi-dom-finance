//! Bilimbi.Payables.Balances, TaxReport and TieOut; all figures share a posting fence.
use crate::*;
use bilibi_finance_ledger::{MinorUnits, TotalsQuery};
use std::collections::BTreeMap;

/// All three boundaries are required to replay entryless imports and applications.
#[derive(Clone, Debug, Default)]
pub struct BalancePins {
    pub through_entry_number: Option<i64>,
    pub through_opening_batch_id: Option<i64>,
    pub through_settlement_number: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct AgingQuery {
    pub through_opening_batch_id: Option<i64>,
    pub through_settlement_number: Option<i64>,
    pub through_entry_number: Option<i64>,
    pub bucket_days: Vec<i32>,
}
impl Default for AgingQuery {
    fn default() -> Self {
        Self {
            through_opening_batch_id: None,
            through_settlement_number: None,
            through_entry_number: None,
            bucket_days: vec![30, 60, 90],
        }
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AgedDocument {
    pub id: i64,
    pub document_number: i64,
    pub control_account_id: i64,
    pub kind: String,
    pub party_key: String,
    pub party_name: String,
    pub party_code: String,
    pub party_document_number: Option<String>,
    #[serde(serialize_with = "date_text", deserialize_with = "read_date")]
    pub document_date: Date,
    #[serde(serialize_with = "date_text", deserialize_with = "read_date")]
    pub due_date: Date,
    pub currency_code: String,
    #[serde(deserialize_with = "read_money")]
    pub total: Money,
    #[serde(deserialize_with = "read_money")]
    pub outstanding: Money,
    pub days_overdue: i32,
    pub bucket: usize,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AgedParty {
    pub vendor_id: Option<i64>,
    pub party_key: String,
    pub party_name: String,
    pub party_code: String,
    pub documents: Vec<AgedDocument>,
    pub credits: Vec<AgedDocument>,
    #[serde(deserialize_with = "read_money")]
    pub credit_total: Money,
    #[serde(deserialize_with = "read_money_list")]
    pub buckets: Vec<Money>,
    #[serde(deserialize_with = "read_money")]
    pub total: Money,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AgedPayables {
    pub through_opening_batch_id: i64,
    pub through_settlement_number: i64,
    pub company_id: i64,
    #[serde(serialize_with = "date_text", deserialize_with = "read_date")]
    pub as_of: Date,
    pub through_entry_number: i64,
    pub bucket_days: Vec<i32>,
    pub parties: Vec<AgedParty>,
    #[serde(deserialize_with = "read_money")]
    pub gross_debits: Money,
    #[serde(deserialize_with = "read_money")]
    pub gross_credits: Money,
    #[serde(deserialize_with = "read_money_list")]
    pub buckets: Vec<Money>,
    #[serde(deserialize_with = "read_money")]
    pub total: Money,
    pub exceptions: Vec<AgedDocument>,
    #[serde(deserialize_with = "read_money")]
    pub exceptions_total: Money,
}
#[derive(Clone, Debug, Default)]
pub struct TaxQuery {
    pub from: Option<Date>,
    pub to: Option<Date>,
    pub through_entry_number: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct TaxTotal {
    pub document_type: String,
    pub tax_code_id: i64,
    pub tax_code: String,
    pub rate: Decimal,
    pub account_id: i64,
    pub document_count: i64,
    pub taxable_amount: Money,
    pub tax_amount: Money,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct OtherPosting {
    pub owner_module: Option<String>,
    pub balance: Money,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ControlAccount {
    pub account_id: i64,
    pub code: String,
    pub name: String,
    pub subledger_balance: Money,
    pub ledger_balance: Money,
    pub owned_balance: Money,
    pub other_postings: Vec<OtherPosting>,
    pub difference: Money,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct TieOut {
    pub through_opening_batch_id: i64,
    pub through_settlement_number: i64,
    pub company_id: i64,
    #[serde(serialize_with = "date_text")]
    pub as_of: Date,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub owner_module: String,
    pub accounts: Vec<ControlAccount>,
    pub total_difference: Money,
    pub tied_out: bool,
}

// Own-table sums only. Each reduction counts by both effective date and posting number.
const OWED: &str = r#"
SELECT b.id,finance_payables_residual($1,$2,b.id,$3::text::date,$4,$6) AS owed
FROM finance_payables_documents b WHERE b.tenant_id=$1 AND b.company_id=$2 AND b.document_type IN ('bill','opening_credit') AND b.status='posted' AND b.document_date<=$3::text::date AND b.entry_number<=$4
AND NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i JOIN finance_payables_opening_batches o ON o.id=i.batch_id AND o.tenant_id=$1 AND o.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=b.id AND o.conversion_date>$3::text::date)
"#;
pub(crate) async fn owed(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    bill: i64,
    date: Date,
    through: i64,
) -> bilibi_database::Result<Money> {
    let r = c
        .query_opt(
            &format!("SELECT owed FROM ({OWED}) o WHERE id=$5"),
            &[
                &tenant,
                &company,
                &date.to_string(),
                &through,
                &bill,
                &i64::MAX,
            ],
        )
        .await?
        .ok_or(Error::NotFound)?;
    store::money(r.get(0))
}
/// Minimum at the proposed date and every future balance-changing date.
pub(crate) async fn lowest(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    bill: i64,
    since: Date,
) -> bilibi_database::Result<Money> {
    let dates=c.query("SELECT document_date::text FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND corrects_document_id=$3 AND status='posted' AND document_date>$4::text::date UNION SELECT p.payment_date::text FROM finance_payables_payments p JOIN finance_payables_payment_allocations a ON a.tenant_id=p.tenant_id AND a.company_id=p.company_id AND a.payment_id=COALESCE(p.reverses_payment_id,p.id) WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.document_id=$3 AND p.payment_date>$4::text::date UNION SELECT effective_date::text FROM finance_payables_settlements WHERE tenant_id=$1 AND company_id=$2 AND (credit_document_id=$3 OR target_document_id=$3) AND effective_date>$4::text::date",&[&tenant,&company,&bill,&since.to_string()]).await?;
    let mut minimum = owed(c, tenant, company, bill, since, i64::MAX).await?;
    for d in dates {
        let date = d.get::<_, String>(0).parse().map_err(|_| Error::Invalid)?;
        minimum = minimum.min(owed(c, tenant, company, bill, date, i64::MAX).await?);
    }
    Ok(minimum)
}
fn fence(through: Option<i64>) -> Result<()> {
    if through.is_some_and(|n| n < 0) {
        Err(PayablesError::InvalidOption)
    } else {
        Ok(())
    }
}
impl Payables {
    pub async fn aged_payables(
        &self,
        s: &Scope,
        company: i64,
        as_of: Date,
        q: &AgingQuery,
    ) -> Result<AgedPayables> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let tx = c
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        let result = self.aged_payables_on(&*tx, s, company, as_of, q).await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(result)
    }

    /// Authorized figures on the caller's database snapshot.
    pub async fn aged_payables_on(
        &self,
        c: &(impl tokio_postgres::GenericClient + Sync),
        s: &Scope,
        company: i64,
        as_of: Date,
        q: &AgingQuery,
    ) -> Result<AgedPayables> {
        authorize(c, s, company, VIEW).await?;
        crate::settlements::complete_pins(
            q.through_entry_number,
            q.through_opening_batch_id,
            q.through_settlement_number,
        )?;
        fence(q.through_entry_number)?;
        if q.bucket_days.is_empty()
            || q.bucket_days.iter().any(|v| *v <= 0)
            || q.bucket_days.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(PayablesError::InvalidOption);
        }
        let opening_pin = crate::opening::pin(c, s, company, q.through_opening_batch_id).await?;
        let through = self
            .ledger
            .trial_balance_on(c, s, company, as_of, q.through_entry_number)
            .await?
            .through_entry_number;
        let settlement_pin = crate::settlements::pin(
            c,
            s,
            company,
            q.through_settlement_number,
            through,
            opening_pin,
            as_of,
        )
        .await?;
        let rows=c.query(&format!("SELECT b.id,b.document_number,b.party_key,b.party_name,b.party_document_number,b.document_date::text,b.due_date::text,b.currency_code,b.total,o.owed,b.party_code,b.document_type,b.payable_account_id FROM ({OWED}) o JOIN finance_payables_documents b ON b.id=o.id AND b.tenant_id=$1 AND b.company_id=$2 WHERE o.owed<>0 AND NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=b.id AND (i.batch_id>$5 OR EXISTS(SELECT 1 FROM finance_payables_opening_decisions own WHERE own.tenant_id=$1 AND own.company_id=$2 AND own.batch_id=i.batch_id AND own.effective_date>$3::text::date) OR EXISTS(SELECT 1 FROM finance_payables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=i.batch_id AND x.effective_date<=$3::text::date AND x.batch_id<=$5))) ORDER BY b.party_key,b.due_date,b.id"),&[&s.tenant(),&company,&as_of.to_string(),&through,&opening_pin,&settlement_pin]).await?;
        let empty = vec![Money::ZERO; q.bucket_days.len() + 2];
        let mut parties: BTreeMap<String, AgedParty> = BTreeMap::new();
        let mut latest: BTreeMap<String, (Date, i64)> = BTreeMap::new();
        let mut exceptions = Vec::new();
        for r in rows {
            let due: Date = r.get::<_, String>(6).parse().map_err(|_| Error::Invalid)?;
            let document_date: Date = r.get::<_, String>(5).parse().map_err(|_| Error::Invalid)?;
            let kind: String = r.get(11);
            let aging_date = if kind == "opening_credit" {
                document_date
            } else {
                due
            };
            let days = aging_date
                .until(as_of)
                .map_err(|_| Error::Invalid)?
                .get_days()
                .max(0);
            let bucket = if days == 0 {
                0
            } else {
                q.bucket_days
                    .iter()
                    .position(|v| days <= *v)
                    .map_or(q.bucket_days.len() + 1, |i| i + 1)
            };
            let d = AgedDocument {
                id: r.get(0),
                document_number: r.get(1),
                kind,
                control_account_id: r.get(12),
                party_key: r.get(2),
                party_name: r.get(3),
                party_code: r.get(10),
                party_document_number: r.get(4),
                document_date,
                due_date: due,
                currency_code: r.get(7),
                total: store::money(r.get(8))?,
                outstanding: store::money(r.get(9))?,
                days_overdue: days,
                bucket,
            };
            if d.outstanding < Money::ZERO {
                exceptions.push(d);
                continue;
            }
            let party = parties
                .entry(d.party_key.clone())
                .or_insert_with(|| AgedParty {
                    vendor_id: None,
                    party_key: d.party_key.clone(),
                    party_name: d.party_name.clone(),
                    party_code: d.party_code.clone(),
                    documents: vec![],
                    credits: vec![],
                    credit_total: Money::ZERO,
                    buckets: empty.clone(),
                    total: Money::ZERO,
                });
            let date = (d.document_date, d.id);
            if latest.get(&d.party_key).is_none_or(|v| date > *v) {
                party.party_name = d.party_name.clone();
                latest.insert(d.party_key.clone(), date);
            }
            if d.kind == "opening_credit" {
                party.credit_total = party.credit_total + d.outstanding;
                party.total = party.total - d.outstanding;
                party.credits.push(d);
                continue;
            }
            party.buckets[bucket] = party.buckets[bucket] + d.outstanding;
            party.total = party.total + d.outstanding;
            party.documents.push(d);
        }
        let mut parties: Vec<_> = parties.into_values().collect();
        for p in &mut parties {
            p.vendor_id = vendors::of_key(c, s.tenant(), company, &p.party_key).await?;
        }
        parties.sort_by(|a, b| (&a.party_code, &a.party_key).cmp(&(&b.party_code, &b.party_key)));
        let mut buckets = empty;
        for p in &parties {
            for (i, a) in p.buckets.iter().enumerate() {
                buckets[i] = buckets[i] + *a;
            }
        }
        Ok(AgedPayables {
            through_opening_batch_id: opening_pin,
            through_settlement_number: settlement_pin,
            company_id: company,
            as_of,
            through_entry_number: through,
            bucket_days: q.bucket_days.clone(),
            total: parties.iter().map(|p| p.total).sum(),
            gross_debits: parties.iter().map(|p| p.credit_total).sum(),
            gross_credits: buckets.iter().copied().sum(),
            parties,
            buckets,
            exceptions_total: exceptions.iter().map(|e| e.outstanding).sum(),
            exceptions,
        })
    }
    pub async fn tax_totals(&self, s: &Scope, company: i64, q: &TaxQuery) -> Result<Vec<TaxTotal>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        fence(q.through_entry_number)?;
        if q.from.zip(q.to).is_some_and(|(f, t)| f > t) {
            return Err(PayablesError::InvalidOption);
        }
        let rows=c.query("SELECT d.document_type,t.tax_code_id,t.tax_code,t.rate,t.account_id,count(DISTINCT d.id),sum(t.taxable_amount),sum(t.tax_amount) FROM finance_payables_document_tax_lines t JOIN finance_payables_documents d ON d.id=t.document_id AND d.tenant_id=t.tenant_id AND d.company_id=t.company_id WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.status='posted' AND ($3::text IS NULL OR d.document_date>=$3::text::date) AND ($4::text IS NULL OR d.document_date<=$4::text::date) AND ($5::bigint IS NULL OR d.entry_number<=$5) GROUP BY d.document_type,t.tax_code_id,t.tax_code,t.rate,t.account_id ORDER BY t.tax_code,d.document_type,t.rate,t.account_id",&[&s.tenant(),&company,&q.from.map(|d|d.to_string()),&q.to.map(|d|d.to_string()),&q.through_entry_number]).await?;
        rows.iter()
            .map(|r| {
                Ok(TaxTotal {
                    document_type: r.get(0),
                    tax_code_id: r.get(1),
                    tax_code: r.get(2),
                    rate: r.get(3),
                    account_id: r.get(4),
                    document_count: r.get(5),
                    taxable_amount: store::money(r.get(6))?,
                    tax_amount: store::money(r.get(7))?,
                })
            })
            .collect()
    }
    pub async fn tie_out_control_accounts(
        &self,
        s: &Scope,
        company: i64,
        as_of: Date,
        pins: &BalancePins,
    ) -> Result<TieOut> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        let result = self
            .tie_out_control_accounts_on(&*tx, s, company, as_of, pins)
            .await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(result)
    }

    /// Authorized control tie-out on the caller's snapshot, including other owners.
    pub async fn tie_out_control_accounts_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        s: &Scope,
        company: i64,
        as_of: Date,
        pins: &BalancePins,
    ) -> Result<TieOut> {
        authorize(client, s, company, VIEW).await?;
        crate::settlements::complete_pins(
            pins.through_entry_number,
            pins.through_opening_batch_id,
            pins.through_settlement_number,
        )?;
        fence(pins.through_entry_number)?;
        let ledger = self
            .ledger
            .account_totals_by_owner_on(
                client,
                s,
                company,
                &TotalsQuery {
                    to: Some(as_of),
                    through_entry_number: pins.through_entry_number,
                    ..Default::default()
                },
            )
            .await?;
        let chart = self.ledger.list_accounts_on(client, s, company).await?;
        let opening_pin =
            crate::opening::pin(client, s, company, pins.through_opening_batch_id).await?;
        let settlement_pin = crate::settlements::pin(
            client,
            s,
            company,
            pins.through_settlement_number,
            ledger.through_entry_number,
            opening_pin,
            as_of,
        )
        .await?;
        let rows=client.query(&format!("SELECT b.payable_account_id,sum(CASE WHEN b.document_type='opening_credit' THEN -o.owed ELSE o.owed END) FROM ({OWED}) o JOIN finance_payables_documents b ON b.id=o.id AND b.tenant_id=$1 AND b.company_id=$2 WHERE NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=b.id AND (i.batch_id>$5 OR EXISTS(SELECT 1 FROM finance_payables_opening_decisions own WHERE own.tenant_id=$1 AND own.company_id=$2 AND own.batch_id=i.batch_id AND own.effective_date>$3::text::date) OR EXISTS(SELECT 1 FROM finance_payables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=i.batch_id AND x.effective_date<=$3::text::date AND x.batch_id<=$5))) GROUP BY b.payable_account_id"),&[&s.tenant(),&company,&as_of.to_string(),&ledger.through_entry_number,&opening_pin,&settlement_pin]).await?;
        let mut subledger = BTreeMap::new();
        for r in rows {
            subledger.insert(r.get::<_, i64>(0), store::money(r.get(1))?);
        }
        let ids=client.query("SELECT payable_account_id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND payable_account_id IS NOT NULL UNION SELECT payable_account_id FROM finance_payables_settings WHERE tenant_id=$1 AND company_id=$2",&[&s.tenant(),&company]).await?;
        let mut accounts = Vec::new();
        for r in ids {
            let id: i64 = r.get(0);
            let a = chart.iter().find(|a| a.id == id).ok_or(Error::Invalid)?;
            let postings: Vec<_> = ledger.rows.iter().filter(|p| p.account.id == id).collect();
            let balance = postings.iter().map(|p| p.credits - p.debits).sum();
            let owned = postings
                .iter()
                .filter(|p| p.owner_module.as_deref() == Some(MODULE))
                .map(|p| p.credits - p.debits)
                .sum();
            let sub = subledger.get(&id).copied().unwrap_or(Money::ZERO);
            let other_postings = postings
                .iter()
                .filter(|p| p.owner_module.as_deref() != Some(MODULE))
                .map(|p| OtherPosting {
                    owner_module: p.owner_module.clone(),
                    balance: p.credits - p.debits,
                })
                .collect();
            accounts.push(ControlAccount {
                account_id: id,
                code: a.code.clone(),
                name: a.name.clone(),
                subledger_balance: sub,
                ledger_balance: balance,
                owned_balance: owned,
                other_postings,
                difference: sub - balance,
            });
        }
        accounts.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(TieOut {
            through_opening_batch_id: opening_pin,
            through_settlement_number: settlement_pin,
            company_id: company,
            as_of,
            through_entry_number: ledger.through_entry_number,
            currency_code: ledger.currency_code,
            minor_units: ledger.minor_units,
            owner_module: MODULE.into(),
            total_difference: accounts.iter().map(|a| a.difference).sum(),
            tied_out: accounts.iter().all(|a| a.difference.is_zero()),
            accounts,
        })
    }
}

fn date_text<S: serde::Serializer>(
    date: &Date,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(date)
}

impl TieOut {
    pub fn pins(&self) -> BalancePins {
        BalancePins {
            through_entry_number: Some(self.through_entry_number),
            through_opening_batch_id: Some(self.through_opening_batch_id),
            through_settlement_number: Some(self.through_settlement_number),
        }
    }
}
impl AgedPayables {
    pub fn pins(&self) -> BalancePins {
        BalancePins {
            through_entry_number: Some(self.through_entry_number),
            through_opening_batch_id: Some(self.through_opening_batch_id),
            through_settlement_number: Some(self.through_settlement_number),
        }
    }
}

// Trusted contract output, already validated at the book boundary, retains the
// database's maximum precision when decoded for the server-rendered page.
fn read_money<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Money, D::Error> {
    use serde::Deserialize;
    Money::parse(&String::deserialize(d)?, MinorUnits::new(4).unwrap())
        .map_err(serde::de::Error::custom)
}
fn read_money_list<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<Money>, D::Error> {
    use serde::Deserialize;
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|s| Money::parse(&s, MinorUnits::new(4).unwrap()).map_err(serde::de::Error::custom))
        .collect()
}
fn read_date<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Date, D::Error> {
    use serde::Deserialize;
    String::deserialize(d)?
        .parse()
        .map_err(serde::de::Error::custom)
}
