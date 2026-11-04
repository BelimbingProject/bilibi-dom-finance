//! Mirrors Bilimbi Receivables.{Balances,Aging,TaxReport,TieOut}.
use crate::{authorize, store, ReceivableError, Receivables, Result, MODULE, VIEW};
use bilibi_database::Error;
use bilibi_finance_ledger::{MinorUnits, TotalsQuery};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::GenericClient;

type TaxShare = (
    String,
    i64,
    Decimal,
    Decimal,
    i64,
    String,
    Decimal,
    i64,
    Decimal,
    Decimal,
);
type TaxGroupKey = (String, i64, String, Decimal, i64);
type TaxGroups = BTreeMap<TaxGroupKey, (ReceivedTaxTotal, BTreeSet<i64>)>;

/// All three boundaries are required to replay entryless imports and applications.
#[derive(Clone, Debug, Default)]
pub struct BalancePins {
    pub through_entry_number: Option<i64>,
    pub through_opening_batch_id: Option<i64>,
    pub through_settlement_number: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct AgingOptions {
    pub through_opening_batch_id: Option<i64>,
    pub through_settlement_number: Option<i64>,
    pub bucket_days: Vec<i64>,
    pub through_entry_number: Option<i64>,
}
impl Default for AgingOptions {
    fn default() -> Self {
        Self {
            through_opening_batch_id: None,
            through_settlement_number: None,
            bucket_days: vec![30, 60, 90],
            through_entry_number: None,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct TaxOptions {
    pub from: Option<Date>,
    pub to: Option<Date>,
    pub through_entry_number: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct ReceivedTaxOptions {
    pub from: Date,
    pub to: Date,
    pub unpaid_months: i32,
    pub through_entry_number: Option<i64>,
}
impl ReceivedTaxOptions {
    pub fn new(from: Date, to: Date) -> Self {
        Self {
            from,
            to,
            unpaid_months: 12,
            through_entry_number: None,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct AgingBucket {
    pub from_days: Option<i64>,
    pub to_days: Option<i64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct AgedDocument {
    pub document_id: i64,
    pub control_account_id: i64,
    pub kind: String,
    pub document_reference: String,
    pub party_name: String,
    pub issue_date: String,
    pub due_date: String,
    pub total: Decimal,
    pub outstanding: Decimal,
    pub days_overdue: i64,
    pub bucket: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct AgedCustomer {
    pub customer_id: i64,
    pub code: String,
    pub name: String,
    pub documents: Vec<AgedDocument>,
    pub credits: Vec<AgedDocument>,
    pub credit_total: Decimal,
    pub bucket_totals: Vec<Decimal>,
    pub total: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct AgedReceivables {
    pub through_opening_batch_id: i64,
    pub through_settlement_number: i64,
    pub company_id: i64,
    pub as_of: String,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub buckets: Vec<AgingBucket>,
    pub customers: Vec<AgedCustomer>,
    pub gross_debits: Decimal,
    pub gross_credits: Decimal,
    pub bucket_totals: Vec<Decimal>,
    pub total: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaxTotal {
    pub kind: String,
    pub credits_prior_period: bool,
    pub tax_code_id: i64,
    pub tax_code: String,
    pub tax_rate: Decimal,
    pub account_id: i64,
    pub document_count: i64,
    pub taxable_amount: Decimal,
    pub tax_amount: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReceivedTaxTotal {
    pub measure: String,
    pub tax_code_id: i64,
    pub tax_code: String,
    pub tax_rate: Decimal,
    pub account_id: i64,
    pub document_count: i64,
    pub taxable_amount: Decimal,
    pub tax_amount: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct OtherPosting {
    pub owner_module: Option<String>,
    pub balance: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct ControlAccountTieOut {
    pub account_id: i64,
    pub code: String,
    pub name: String,
    pub subledger_balance: Decimal,
    pub ledger_balance: Decimal,
    pub owned_balance: Decimal,
    pub other_postings: Vec<OtherPosting>,
    pub difference: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct ControlAccountsTieOut {
    pub through_opening_batch_id: i64,
    pub through_settlement_number: i64,
    pub company_id: i64,
    pub as_of: String,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: MinorUnits,
    pub owner_module: String,
    pub accounts: Vec<ControlAccountTieOut>,
    pub total_difference: Decimal,
    pub tied_out: bool,
}

fn window(from: Option<Date>, to: Option<Date>, through: Option<i64>) -> Result<()> {
    if from.zip(to).is_some_and(|(a, b)| a > b) || through.is_some_and(|n| n < 0) {
        return Err(ReceivableError::InvalidOption);
    }
    Ok(())
}

// Correlated aggregates keep independent credit and allocation children apart.
// $1 tenant, $2 company, $3 inclusive event date, $4 inclusive posting pin.
const BALANCES: &str = "SELECT d.*, d.issue_date::text issue_day, d.due_date::text due_day,
 ($3::text::date-CASE WHEN d.kind='opening_credit' THEN d.issue_date ELSE d.due_date END)::bigint days_overdue,
 finance_receivables_residual($1,$2,d.id,$3::text::date,$4,$6) AS outstanding, 0::numeric AS written_off
 FROM finance_receivables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2
 AND d.kind IN ('invoice','opening_credit') AND d.status='issued' AND d.issue_date<=$3::text::date AND d.ledger_entry_number<=$4
 AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_items i JOIN finance_receivables_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id AND (b.conversion_date>$3::text::date OR ($5::bigint IS NOT NULL AND b.id>$5) OR EXISTS(SELECT 1 FROM finance_receivables_opening_decisions own WHERE own.tenant_id=$1 AND own.company_id=$2 AND own.batch_id=b.id AND own.effective_date>$3::text::date) OR EXISTS(SELECT 1 FROM finance_receivables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=b.id AND x.effective_date<=$3::text::date AND ($5::bigint IS NULL OR x.batch_id<=$5))))";

async fn balances(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: i64,
    day: &str,
    pin: i64,
    opening_pin: Option<i64>,
    settlement_pin: i64,
) -> Result<Vec<tokio_postgres::Row>> {
    Ok(client
        .query(
            BALANCES,
            &[
                &scope.tenant(),
                &company,
                &day,
                &pin,
                &opening_pin,
                &settlement_pin,
            ],
        )
        .await
        .map_err(Error::from)?)
}

impl Receivables {
    pub async fn aged_receivables(
        &self,
        scope: &Scope,
        company: i64,
        as_of: Date,
        options: &AgingOptions,
    ) -> Result<AgedReceivables> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        let result = self
            .aged_receivables_on(&*tx, scope, company, as_of, options)
            .await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(result)
    }

    /// Authorized figures on the caller's database snapshot.
    pub async fn aged_receivables_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
        as_of: Date,
        options: &AgingOptions,
    ) -> Result<AgedReceivables> {
        authorize(client, scope, company, VIEW).await?;
        crate::settlements::complete_pins(
            options.through_entry_number,
            options.through_opening_batch_id,
            options.through_settlement_number,
        )?;
        window(None, None, options.through_entry_number)?;
        if options.bucket_days.is_empty()
            || options.bucket_days.iter().any(|n| *n <= 0 || *n > 36_500)
            || options.bucket_days.windows(2).any(|p| p[0] >= p[1])
        {
            return Err(ReceivableError::InvalidOption);
        }
        let book = self.ledger.get_book_on(client, scope, company).await?;
        let pin = self
            .ledger
            .trial_balance_on(client, scope, company, as_of, options.through_entry_number)
            .await?
            .through_entry_number;
        let opening_pin =
            crate::opening::pin(client, scope, company, options.through_opening_batch_id).await?;
        let settlement_pin = crate::settlements::pin(
            client,
            scope,
            company,
            options.through_settlement_number,
            pin,
            opening_pin,
            as_of,
        )
        .await?;
        let mut buckets = vec![AgingBucket {
            from_days: None,
            to_days: Some(0),
        }];
        let mut start = 1;
        for &end in &options.bucket_days {
            buckets.push(AgingBucket {
                from_days: Some(start),
                to_days: Some(end),
            });
            start = end + 1;
        }
        buckets.push(AgingBucket {
            from_days: Some(start),
            to_days: None,
        });
        // A linked customer's name is its company's, as Core names it now.
        let mut labels: BTreeMap<i64, (String, String)> = BTreeMap::new();
        for row in store::list(client, store::CUSTOMERS, scope.tenant(), company).await? {
            let customer = self.customer(client, scope, row).await?;
            labels.insert(customer.id, (customer.code, customer.name));
        }
        let mut customers: BTreeMap<i64, AgedCustomer> = BTreeMap::new();
        let mut totals = vec![Decimal::ZERO; buckets.len()];
        for row in balances(
            client,
            scope,
            company,
            &as_of.to_string(),
            pin,
            Some(opening_pin),
            settlement_pin,
        )
        .await?
        {
            let outstanding: Decimal =
                row.get::<_, Decimal>("outstanding") - row.get::<_, Decimal>("written_off");
            if outstanding.is_zero() {
                continue;
            }
            // Date columns are read as text without requiring a PostgreSQL date codec.
            let id: i64 = row.get("id");
            let days: i64 = row.get("days_overdue");
            let bucket = if days <= 0 {
                0
            } else {
                options
                    .bucket_days
                    .iter()
                    .position(|n| days <= *n)
                    .map_or(buckets.len() - 1, |n| n + 1)
            };
            let customer_id = row.get("customer_id");
            let (code, name) = labels[&customer_id].clone();
            let customer = customers
                .entry(customer_id)
                .or_insert_with(|| AgedCustomer {
                    customer_id,
                    code,
                    name,
                    documents: vec![],
                    credits: vec![],
                    credit_total: Decimal::ZERO,
                    bucket_totals: vec![Decimal::ZERO; buckets.len()],
                    total: Decimal::ZERO,
                });
            let document = AgedDocument {
                control_account_id: row.get("receivable_account_id"),
                kind: row.get("kind"),
                document_id: id,
                document_reference: row.get("document_reference"),
                party_name: row.get("party_name"),
                issue_date: row.get("issue_day"),
                due_date: row.get("due_day"),
                total: row.get("total"),
                outstanding,
                days_overdue: days.max(0),
                bucket,
            };
            if document.kind == "opening_credit" {
                customer.credit_total += outstanding;
                customer.total -= outstanding;
                customer.credits.push(document);
                continue;
            }
            customer.documents.push(document);
            customer.total += outstanding;
            customer.bucket_totals[bucket] += outstanding;
            totals[bucket] += outstanding;
        }
        let mut customers: Vec<_> = customers.into_values().collect();
        for c in &mut customers {
            c.documents
                .sort_by(|a, b| (&a.due_date, a.document_id).cmp(&(&b.due_date, b.document_id)));
        }
        customers.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(AgedReceivables {
            through_opening_batch_id: opening_pin,
            through_settlement_number: settlement_pin,
            company_id: company,
            as_of: as_of.to_string(),
            through_entry_number: pin,
            currency_code: book.currency_code,
            minor_units: book.minor_units,
            buckets,
            gross_debits: totals.iter().copied().sum(),
            gross_credits: customers.iter().map(|c| c.credit_total).sum(),
            total: customers.iter().map(|c| c.total).sum(),
            customers,
            bucket_totals: totals,
        })
    }

    pub async fn tax_totals(
        &self,
        scope: &Scope,
        company: i64,
        options: &TaxOptions,
    ) -> Result<Vec<TaxTotal>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        window(options.from, options.to, options.through_entry_number)?;
        let rows=client.query("SELECT d.kind,COALESCE(i.issue_date<$3::text::date,false) prior,l.tax_code_id,l.tax_code,l.tax_rate,l.account_id,count(DISTINCT d.id),sum(l.taxable_amount),sum(l.tax_amount)
 FROM finance_receivables_document_tax_lines l JOIN finance_receivables_documents d ON d.id=l.document_id AND d.tenant_id=$1 AND d.company_id=$2
 LEFT JOIN finance_receivables_documents i ON i.id=d.credited_document_id AND i.tenant_id=$1 AND i.company_id=$2
 WHERE l.tenant_id=$1 AND l.company_id=$2 AND d.status='issued' AND ($3::text IS NULL OR d.issue_date>=$3::text::date) AND ($4::text IS NULL OR d.issue_date<=$4::text::date) AND d.ledger_entry_number<=$5
 GROUP BY d.kind,prior,l.tax_code_id,l.tax_code,l.tax_rate,l.account_id ORDER BY l.tax_code,d.kind,prior,l.tax_rate,l.account_id", &[&scope.tenant(),&company,&options.from.map(|d|d.to_string()),&options.to.map(|d|d.to_string()),&options.through_entry_number.unwrap_or(i64::MAX)]).await.map_err(Error::from)?;
        Ok(rows
            .iter()
            .map(|r| TaxTotal {
                kind: r.get(0),
                credits_prior_period: r.get(1),
                tax_code_id: r.get(2),
                tax_code: r.get(3),
                tax_rate: r.get(4),
                account_id: r.get(5),
                document_count: r.get(6),
                taxable_amount: r.get(7),
                tax_amount: r.get(8),
            })
            .collect())
    }

    pub async fn received_tax_totals(
        &self,
        scope: &Scope,
        company: i64,
        options: &ReceivedTaxOptions,
    ) -> Result<Vec<ReceivedTaxTotal>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        window(
            Some(options.from),
            Some(options.to),
            options.through_entry_number,
        )?;
        if !(1..=120).contains(&options.unpaid_months) {
            return Err(ReceivableError::InvalidOption);
        }
        let pin = options.through_entry_number.unwrap_or(i64::MAX);
        // Fetch individual shares: division and multiplication stay Decimal, not
        // rounded PostgreSQL numeric or Money. UNION ALL preserves zero-net groups.
        let sql="WITH docs AS (SELECT d.*,((issue_date+make_interval(months=>$6::integer))::date+1) due FROM finance_receivables_documents d WHERE tenant_id=$1 AND company_id=$2),
 shares AS (
 SELECT 'received'::text measure,d.id,a.amount amount,d.total FROM docs d JOIN finance_receivables_allocations a ON a.document_id=d.id AND a.tenant_id=$1 AND a.company_id=$2 JOIN finance_receivables_payments p ON p.id=a.payment_id AND p.tenant_id=$1 AND p.company_id=$2 WHERE d.kind='invoice' AND d.status='issued' AND d.ledger_entry_number<=$5 AND p.ledger_entry_number<=$5 AND p.payment_date<d.due AND p.payment_date BETWEEN $3::text::date AND $4::text::date
 UNION ALL
 SELECT 'received',d.id,-a.amount,d.total FROM docs d JOIN finance_receivables_allocations a ON a.document_id=d.id AND a.tenant_id=$1 AND a.company_id=$2 JOIN finance_receivables_payments p ON p.id=a.payment_id AND p.tenant_id=$1 AND p.company_id=$2 JOIN finance_receivables_payment_reversals r ON r.payment_id=p.id AND r.tenant_id=$1 AND r.company_id=$2 WHERE d.kind='invoice' AND d.status='issued' AND d.ledger_entry_number<=$5 AND p.ledger_entry_number<=$5 AND r.ledger_entry_number<=$5 AND p.payment_date<d.due AND r.reversal_date<d.due AND r.reversal_date BETWEEN $3::text::date AND $4::text::date
 UNION ALL
 SELECT 'credited_after_due',c.id,c.total,c.total FROM docs c JOIN docs d ON d.id=c.credited_document_id WHERE c.kind='credit_note' AND c.status='issued' AND c.ledger_entry_number<=$5 AND c.issue_date>=d.due AND c.issue_date BETWEEN $3::text::date AND $4::text::date)
 SELECT s.measure,s.id,s.amount,s.total,l.tax_code_id,l.tax_code,l.tax_rate,l.account_id,l.taxable_amount,l.tax_amount FROM shares s JOIN finance_receivables_document_tax_lines l ON l.document_id=s.id AND l.tenant_id=$1 AND l.company_id=$2";
        let rows = client
            .query(
                sql,
                &[
                    &scope.tenant(),
                    &company,
                    &options.from.to_string(),
                    &options.to.to_string(),
                    &pin,
                    &options.unpaid_months,
                ],
            )
            .await
            .map_err(Error::from)?;
        let mut shares: Vec<TaxShare> = rows
            .iter()
            .map(|r| {
                (
                    r.get(0),
                    r.get(1),
                    r.get(2),
                    r.get(3),
                    r.get(4),
                    r.get(5),
                    r.get(6),
                    r.get(7),
                    r.get(8),
                    r.get(9),
                )
            })
            .collect();
        let due=client.query("SELECT id,((issue_date+make_interval(months=>$6::integer))::date)::text FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND kind='invoice' AND status='issued' AND ledger_entry_number<=$5 AND ((issue_date+make_interval(months=>$6::integer))::date+1) BETWEEN $3::text::date AND $4::text::date",&[&scope.tenant(),&company,&options.from.to_string(),&options.to.to_string(),&pin,&options.unpaid_months]).await.map_err(Error::from)?;
        let mut days: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
        for r in due {
            days.entry(r.get(1)).or_default().insert(r.get(0));
        }
        for (day, ids) in days {
            // Source-backed opening credits carry no newly taxable receipt or
            // inferred tax share. Their settlement stream changes aging only;
            // retain the native receipt/note basis for current-period tax.
            for d in balances(&**client, scope, company, &day, pin, None, 0).await? {
                let id: i64 = d.get("id");
                let amount: Decimal = d.get("outstanding");
                if !ids.contains(&id) || amount <= Decimal::ZERO {
                    continue;
                }
                for l in client.query("SELECT tax_code_id,tax_code,tax_rate,account_id,taxable_amount,tax_amount FROM finance_receivables_document_tax_lines WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3",&[&scope.tenant(),&company,&id]).await.map_err(Error::from)? {
                    shares.push(("unpaid".into(),id,amount,d.get("total"),l.get(0),l.get(1),l.get(2),l.get(3),l.get(4),l.get(5)));
                }
            }
        }
        let mut groups = TaxGroups::new();
        for (
            measure,
            id,
            amount,
            total,
            tax_code_id,
            tax_code,
            tax_rate,
            account_id,
            taxable,
            tax,
        ) in shares
        {
            let (row, ids) = groups
                .entry((
                    measure.clone(),
                    tax_code_id,
                    tax_code.clone(),
                    tax_rate,
                    account_id,
                ))
                .or_insert_with(|| {
                    (
                        ReceivedTaxTotal {
                            measure,
                            tax_code_id,
                            tax_code,
                            tax_rate,
                            account_id,
                            document_count: 0,
                            taxable_amount: Decimal::ZERO,
                            tax_amount: Decimal::ZERO,
                        },
                        BTreeSet::new(),
                    )
                });
            let factor = amount / total;
            row.taxable_amount += taxable * factor;
            row.tax_amount += tax * factor;
            ids.insert(id);
        }
        let mut rows: Vec<_> = groups
            .into_values()
            .map(|(mut r, ids)| {
                r.document_count = ids.len() as i64;
                r
            })
            .collect();
        rows.sort_by_key(|r| {
            (
                r.tax_code.clone(),
                r.measure.clone(),
                r.tax_rate.to_string(),
                r.account_id,
            )
        });
        Ok(rows)
    }

    pub async fn tie_out_control_accounts(
        &self,
        scope: &Scope,
        company: i64,
        as_of: Date,
        pins: &BalancePins,
    ) -> Result<ControlAccountsTieOut> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await
            .map_err(Error::from)?;
        let result = self
            .tie_out_control_accounts_on(&*tx, scope, company, as_of, pins)
            .await?;
        tx.commit().await.map_err(Error::from)?;
        Ok(result)
    }

    /// Authorized control tie-out on the caller's snapshot, including other owners.
    pub async fn tie_out_control_accounts_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
        as_of: Date,
        pins: &BalancePins,
    ) -> Result<ControlAccountsTieOut> {
        authorize(client, scope, company, VIEW).await?;
        crate::settlements::complete_pins(
            pins.through_entry_number,
            pins.through_opening_batch_id,
            pins.through_settlement_number,
        )?;
        window(None, None, pins.through_entry_number)?;
        let ledger = self
            .ledger
            .account_totals_by_owner_on(
                client,
                scope,
                company,
                &TotalsQuery {
                    to: Some(as_of),
                    through_entry_number: pins.through_entry_number,
                    ..Default::default()
                },
            )
            .await?;
        let chart = self.ledger.list_accounts_on(client, scope, company).await?;
        let opening_pin =
            crate::opening::pin(client, scope, company, pins.through_opening_batch_id).await?;
        let settlement_pin = crate::settlements::pin(
            client,
            scope,
            company,
            pins.through_settlement_number,
            ledger.through_entry_number,
            opening_pin,
            as_of,
        )
        .await?;
        let mut subledger: BTreeMap<i64, Decimal> = BTreeMap::new();
        for d in balances(
            client,
            scope,
            company,
            &as_of.to_string(),
            ledger.through_entry_number,
            Some(opening_pin),
            settlement_pin,
        )
        .await?
        {
            let sign = if d.get::<_, String>("kind") == "opening_credit" {
                -Decimal::ONE
            } else {
                Decimal::ONE
            };
            *subledger.entry(d.get("receivable_account_id")).or_default() +=
                sign * (d.get::<_, Decimal>("outstanding") - d.get::<_, Decimal>("written_off"));
        }
        let controls=client.query("SELECT receivable_account_id FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND status='issued' AND receivable_account_id IS NOT NULL UNION SELECT receivable_account_id FROM finance_receivables_setups WHERE tenant_id=$1 AND company_id=$2",&[&scope.tenant(),&company]).await.map_err(Error::from)?;
        let mut ids: BTreeSet<i64> = controls.iter().map(|r| r.get(0)).collect();
        ids.extend(subledger.keys());
        let mut accounts = Vec::new();
        for id in ids {
            let account = chart
                .iter()
                .find(|a| a.id == id)
                .ok_or(ReceivableError::InvalidOption)?;
            let mut balance = Decimal::ZERO;
            let mut owned = Decimal::ZERO;
            let mut others = vec![];
            for r in ledger.rows.iter().filter(|r| r.account.id == id) {
                let amount = r.debits.amount() - r.credits.amount();
                balance += amount;
                if r.owner_module.as_deref() == Some(MODULE) {
                    owned += amount;
                } else {
                    others.push(OtherPosting {
                        owner_module: r.owner_module.clone(),
                        balance: amount,
                    });
                }
            }
            others.sort_by(|a, b| a.owner_module.cmp(&b.owner_module));
            let sub = subledger.get(&id).copied().unwrap_or_default();
            accounts.push(ControlAccountTieOut {
                account_id: id,
                code: account.code.clone(),
                name: account.name.clone(),
                subledger_balance: sub,
                ledger_balance: balance,
                owned_balance: owned,
                other_postings: others,
                difference: sub - balance,
            });
        }
        accounts.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(ControlAccountsTieOut {
            through_opening_batch_id: opening_pin,
            through_settlement_number: settlement_pin,
            company_id: company,
            as_of: as_of.to_string(),
            through_entry_number: ledger.through_entry_number,
            currency_code: ledger.currency_code,
            minor_units: ledger.minor_units,
            owner_module: MODULE.into(),
            total_difference: accounts.iter().map(|r| r.difference).sum(),
            tied_out: accounts.iter().all(|r| r.difference.is_zero()),
            accounts,
        })
    }
}

impl ControlAccountsTieOut {
    pub fn pins(&self) -> BalancePins {
        BalancePins {
            through_entry_number: Some(self.through_entry_number),
            through_opening_batch_id: Some(self.through_opening_batch_id),
            through_settlement_number: Some(self.through_settlement_number),
        }
    }
}
impl AgedReceivables {
    pub fn pins(&self) -> BalancePins {
        BalancePins {
            through_entry_number: Some(self.through_entry_number),
            through_opening_batch_id: Some(self.through_opening_batch_id),
            through_settlement_number: Some(self.through_settlement_number),
        }
    }
}
