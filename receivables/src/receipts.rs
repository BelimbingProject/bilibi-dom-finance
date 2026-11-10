//! Bilimbi Receivables.Payments: a receipt and its owned entry commit together
//! under the company's writable lock, with its allocations and, for what it
//! applies to no invoice, the customer credit it keeps. Rows never change;
//! a reversal is a dated mirror entry.
use crate::{
    authorize, documents, invalid,
    posting::{group, next_number, owner, posting_account, ISSUED},
    store::{self, Table, CUSTOMERS, DOCUMENTS, SERIES, SETUPS},
    validation as v, Payment, PaymentQuery, ReceivableError as R, Receivables, Result, MODULE,
    POST_PAYMENTS, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{
    AccountType, EntryOwner, MinorUnits, Money, NewEntry, NewLine, PostOptions, Reversal,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) struct PreparedPayment {
    pub after: Value,
    pub allocations: Vec<Value>,
    /// The amount applied to no invoice, kept as a customer credit.
    pub credit: Option<PreparedCredit>,
    pub entry: NewEntry,
    pub options: PostOptions,
    pub facts: Value,
}
/// What a receipt's unapplied amount becomes: a customer credit document on
/// the setup's customer credit account, named and dated as its receipt.
pub(super) struct PreparedCredit {
    pub amount: Money,
    pub account_id: i64,
    pub party_code: String,
    pub party_name: String,
}
pub(super) struct PreparedReversal {
    pub before: Payment,
    pub after: Value,
    pub owner: EntryOwner,
    pub reversal: Reversal,
}

const PAYMENTS: Table = Table {
    name: "finance_receivables_payments",
    kind: "finance.receivables.payment",
    columns: &[
        "id",
        "series_id",
        "customer_id",
        "payment_number",
        "payment_reference",
        "payment_date",
        "amount",
        "currency_code",
        "deposit_account_id",
        "reference",
        "description",
        "ledger_entry_id",
        "ledger_entry_number",
        "recorded_by_user_id",
        "recorded_by_impersonator_id",
    ],
    timestamps: false,
};
const ALLOCATIONS: Table = Table {
    name: "finance_receivables_allocations",
    kind: "finance.receivables.allocation",
    columns: &["payment_id", "document_id", "amount"],
    timestamps: false,
};
const REVERSALS: Table = Table {
    name: "finance_receivables_payment_reversals",
    kind: "finance.receivables.payment_reversal",
    columns: &[
        "payment_id",
        "reversal_date",
        "reason",
        "ledger_entry_id",
        "ledger_entry_number",
        "reversed_by_user_id",
        "reversed_by_impersonator_id",
    ],
    timestamps: false,
};
pub(super) const PAYMENT_READ: &str = "SELECT to_jsonb(p) || jsonb_build_object(
 'allocations',COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM finance_receivables_allocations a WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.payment_id=p.id),'[]'::jsonb),
 'credit',(SELECT jsonb_build_object('document_id',k.document_id,'amount',k.amount,'outstanding',finance_receivables_outstanding(k.document_id)) FROM finance_receivables_receipt_credits k WHERE k.tenant_id=$1 AND k.company_id=$2 AND k.payment_id=p.id),
 'reversal',(SELECT to_jsonb(r) FROM finance_receivables_payment_reversals r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.payment_id=p.id))
 FROM finance_receivables_payments p WHERE p.tenant_id=$1 AND p.company_id=$2";

impl Receivables {
    /// Immutable receipt, allocated to distinct invoices at record time. What
    /// it applies to no invoice (a deposit or an overpayment) is kept as a
    /// customer credit on the setup's customer credit account.
    pub async fn record_payment(
        &self,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Payment> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                POST_PAYMENTS,
                "finance.receivables.record_payment",
            )
            .await?;
        let result = self
            .record_payment_on(&mut tx, scope, company, attrs)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn prepare_payment(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<PreparedPayment> {
        self.join(client, scope, company, POST_PAYMENTS).await?;
        let tenant = scope.tenant();
        let book = self.ledger.get_book_on(client, scope, company).await?;
        let setup = store::find(client, SETUPS, tenant, company, None)
            .await?
            .ok_or(R::NotConfigured)?;
        let chart = self.ledger.list_accounts_on(client, scope, company).await?;
        let mut data = v::merge(
            None,
            attrs,
            &[
                "series_id",
                "customer_id",
                "payment_date",
                "amount",
                "deposit_account_id",
                "reference",
                "description",
            ],
        )?;
        for field in ["series_id", "customer_id", "deposit_account_id"] {
            v::integer(&mut data, field, true)?;
        }
        let date = required_date(&mut data, "payment_date")?;
        let amount = input_money(&mut data, book.minor_units)?;
        v::text(&mut data, "reference", false, 100)?;
        v::text(&mut data, "description", false, 500)?;
        v::account(&mut data, "deposit_account_id", &chart, AccountType::Asset)?;
        let customer = store::find(
            client,
            CUSTOMERS,
            tenant,
            company,
            data["customer_id"].as_i64(),
        )
        .await?
        .ok_or_else(|| invalid("customer_id", "is not a customer of this company"))?;
        // A receipt may come from any customer, linked or not, whatever its
        // company's status; the entry names the customer as it reads now.
        let customer = self.customer(client, scope, customer).await?;
        let series = store::find(client, SERIES, tenant, company, data["series_id"].as_i64())
            .await?
            .ok_or_else(|| invalid("series_id", "is not a series of this company"))?;
        if series["kind"] != "payment" {
            return Err(invalid("series_id", "does not number payments"));
        }
        if series["active"] != true {
            return Err(invalid("series_id", "is an inactive series"));
        }
        let inputs = match &attrs["allocations"] {
            Value::Null => &[][..],
            Value::Array(rows) => rows.as_slice(),
            singleton => std::slice::from_ref(singleton),
        };
        let mut seen = BTreeSet::new();
        let mut allocations = Vec::new();
        let mut credits = Vec::new();
        let mut invoices = Vec::new();
        let mut total = Money::ZERO;
        for (index, input) in inputs.iter().enumerate() {
            let allocation = async {
                let mut a = v::merge(None, input, &["document_id", "amount"])?;
                let id = v::integer(&mut a, "document_id", true)?.ok_or(Error::Internal)?;
                let allocated = input_money(&mut a, book.minor_units)?;
                if !seen.insert(id) {
                    return Err(invalid("document_id", "is allocated twice"));
                }
                let invoice = match documents::read(client, tenant, company, id).await {
                    Err(R::DocumentNotFound) => {
                        return Err(invalid(
                            "document_id",
                            "is not an issued invoice of this customer",
                        ));
                    }
                    other => other?,
                };
                if invoice.kind != "invoice"
                    || invoice.status != "issued"
                    || invoice.customer_id != data["customer_id"].as_i64().ok_or(Error::Internal)?
                {
                    return Err(invalid(
                        "document_id",
                        "is not an issued invoice of this customer",
                    ));
                }
                if invoice
                    .issue_date
                    .as_deref()
                    .is_some_and(|d| d > date.to_string().as_str())
                {
                    return Err(invalid("document_id", "was issued after the payment date"));
                }
                crate::write_offs::settlement_date(client, scope, company, id, date, "document_id")
                    .await?;
                if crate::settlements::lowest(client, scope.tenant(), company, id, date).await?
                    < allocated
                {
                    return Err(invalid(
                        "document_id",
                        "owes less than the allocated amount",
                    ));
                }
                let control = invoice.receivable_account_id.ok_or(Error::Internal)?;
                posting_account(&chart, control, AccountType::Asset, false).map_err(|p| {
                    invalid(
                        "document_id",
                        format!(
                            "{} cannot be settled: its receivable account {p}",
                            invoice.document_reference.as_deref().unwrap_or_default()
                        ),
                    )
                })?;
                Ok((a, control, allocated, invoice))
            }
            .await
            .map_err(|e| match e {
                R::Invalid(mut errors) => {
                    for e in &mut errors {
                        e.line = Some(index + 1);
                    }
                    R::Invalid(errors)
                }
                other => other,
            })?;
            total = total + allocation.2;
            credits.push((allocation.1, allocation.2));
            allocations.push(allocation.0);
            invoices.push(allocation.3);
        }
        if total > amount {
            return Err(invalid(
                "allocations",
                "add up to more than the payment amount",
            ));
        }
        // A deposit or an overpayment: the rest is owed back to the customer
        // until it is applied to a later invoice or refunded.
        let unapplied = amount - total;
        let credit = if unapplied.is_positive() {
            let Some(account_id) = setup["customer_credit_account_id"].as_i64() else {
                return Err(invalid(
                    "allocations",
                    format!(
                        "leave {} not applied to an invoice. To keep it as the customer's credit, first choose the customer credit account in Receivables settings",
                        unapplied.display(book.minor_units)
                    ),
                ));
            };
            posting_account(&chart, account_id, AccountType::Liability, true).map_err(|p| {
                invalid(
                    "allocations",
                    format!("cannot keep the rest as credit: the customer credit account {p}"),
                )
            })?;
            Some(PreparedCredit {
                amount: unapplied,
                account_id,
                party_code: customer.code.clone(),
                party_name: customer.name.clone(),
            })
        } else {
            None
        };
        let deposit = data["deposit_account_id"].as_i64().ok_or(Error::Internal)?;
        if setup["receivable_account_id"].as_i64() == Some(deposit)
            || credits.iter().any(|(control, _)| *control == deposit)
        {
            return Err(invalid(
                "deposit_account_id",
                "is an account that records amounts customers owe; choose the bank or cash account receipts are paid into",
            ));
        }
        let (number, reference) = next_number(client, tenant, company, &series, true).await?;
        let credits = group(credits);
        let settlement_account_ids = credits.keys().copied().collect();
        let mut lines = vec![NewLine::debit(deposit, amount)];
        lines.extend(
            credits
                .into_iter()
                .map(|(id, amount)| NewLine::credit(id, amount)),
        );
        if let Some(credit) = &credit {
            lines.push(NewLine::credit(credit.account_id, credit.amount));
        }
        let credit_facts = credit.as_ref().map(|c| {
            json!({"amount":c.amount,"account":chart.iter().find(|a| a.id == c.account_id)})
        });
        let entry = NewEntry {
            entry_date: date,
            description: format!("Payment {reference} from {}", customer.name),
            reference: Some(reference.clone()),
            lines,
        };
        let options = PostOptions {
            owner: None,
            settlement_account_ids,
        };
        data["payment_number"] = json!(number);
        data["payment_reference"] = json!(reference);
        data["currency_code"] = json!(book.currency_code);
        data["recorded_by_user_id"] = json!(scope.user());
        data["recorded_by_impersonator_id"] = json!(scope.impersonator().map(|s| s.user()));
        Ok(PreparedPayment {
            after: data,
            allocations,
            credit,
            entry,
            options,
            facts: json!({"book":book,"customer":customer,"series":series,"invoices":invoices,"kept_as_credit":credit_facts,"owner":{"module":MODULE,"record":"payment:<allocated at commit>"}}),
        })
    }

    pub(super) async fn record_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        attrs: &Value,
    ) -> Result<Payment> {
        let PreparedPayment {
            after: mut data,
            allocations,
            credit,
            entry,
            mut options,
            ..
        } = self
            .prepare_payment(tx.sql(), scope, company, attrs)
            .await?;
        let tenant = scope.tenant();
        // Business preview never calls nextval. Only execution reserves the
        // identity; the series reference still rolls back with the whole plan.
        let id: i64 = tx
            .sql()
            .query_one(
                "SELECT nextval(pg_get_serial_sequence('finance_receivables_payments','id'))",
                &[],
            )
            .await
            .map_err(Error::from)?
            .get(0);
        options.owner = Some(owner("payment", id));
        let entry = self
            .ledger
            .post_entry_in(tx, scope, company, &entry, &options)
            .await?;
        data["id"] = json!(id);
        data["ledger_entry_id"] = json!(entry.id);
        data["ledger_entry_number"] = json!(entry.entry_number);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                let payment =
                    store::save(sql, PAYMENTS, tenant, company, None, data, &mut changes).await?;
                for mut allocation in allocations {
                    allocation["payment_id"] = json!(id);
                    store::save(
                        sql,
                        ALLOCATIONS,
                        tenant,
                        company,
                        None,
                        allocation,
                        &mut changes,
                    )
                    .await?;
                }
                if let Some(credit) = credit {
                    save_credit(sql, tenant, company, &payment, credit, &mut changes).await?;
                }
                Ok(((), changes))
            })
        })
        .await?;
        let result = read_payment(tx.sql(), tenant, company, id).await?;
        Ok(result)
    }

    pub async fn reverse_payment(
        &self,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<Payment> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                POST_PAYMENTS,
                "finance.receivables.reverse_payment",
            )
            .await?;
        let result = self
            .reverse_payment_on(&mut tx, scope, company, id, attrs)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn prepare_reversal(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<PreparedReversal> {
        self.join(client, scope, company, POST_PAYMENTS).await?;
        let payment = read_payment(client, scope.tenant(), company, id).await?;
        if payment.reversal.is_some() {
            return Err(R::AlreadyReversed);
        }
        let mut data = v::merge(None, attrs, &["reversal_date", "reason"])?;
        let date = required_date(&mut data, "reversal_date")?;
        v::text(&mut data, "reason", true, 500)?;
        if date.to_string() < payment.payment_date {
            return Err(invalid(
                "reversal_date",
                "cannot be before the payment date",
            ));
        }
        // Reversing takes the receipt's credit with it, so the whole credit
        // must still be there from the reversal date on.
        if let Some(credit) = &payment.credit {
            if crate::settlements::lowest(client, scope.tenant(), company, credit.document_id, date)
                .await?
                < credit.amount
            {
                return Err(invalid(
                    "base",
                    "This receipt left a credit that has since been applied or refunded. Reverse that on the credit first, then reverse the receipt.",
                ));
            }
        }
        let reversal = Reversal {
            entry_date: date,
            description: format!("Reversal of payment {}", payment.payment_reference),
            reference: None,
        };
        Ok(PreparedReversal {
            before: payment,
            after: data,
            owner: owner("payment", id),
            reversal,
        })
    }

    pub(super) async fn reverse_payment_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
        attrs: &Value,
    ) -> Result<Payment> {
        let PreparedReversal {
            before: payment,
            after: mut data,
            owner,
            reversal,
        } = self
            .prepare_reversal(tx.sql(), scope, company, id, attrs)
            .await?;
        let tenant = scope.tenant();
        let entry = self
            .ledger
            .reverse_owned_entry(
                tx,
                scope,
                company,
                payment.ledger_entry_id,
                &owner,
                &reversal,
            )
            .await?;
        data["payment_id"] = json!(id);
        data["ledger_entry_id"] = json!(entry.id);
        data["ledger_entry_number"] = json!(entry.entry_number);
        data["reversed_by_user_id"] = json!(scope.user());
        data["reversed_by_impersonator_id"] = json!(scope.impersonator().map(|s| s.user()));
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                store::save(sql, REVERSALS, tenant, company, None, data, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        let result = read_payment(tx.sql(), tenant, company, id).await?;
        Ok(result)
    }

    pub async fn get_payment(&self, scope: &Scope, company: i64, id: i64) -> Result<Payment> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        read_payment(&**client, scope.tenant(), company, id).await
    }
    pub async fn list_payments(
        &self,
        scope: &Scope,
        company: i64,
        query: &PaymentQuery,
    ) -> Result<Vec<Payment>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let (limit, after) = query.page.bounds()?;
        if query.customer_id.is_some_and(|id| id <= 0) {
            return Err(R::InvalidOption);
        }
        client.query(&format!("{PAYMENT_READ} AND p.id>$3 AND ($4::bigint IS NULL OR p.customer_id=$4) ORDER BY p.id LIMIT $5"), &[&scope.tenant(), &company, &after, &query.customer_id, &limit]).await.map_err(Error::from)?.into_iter().map(|r| store::decode(r.get(0)).map_err(R::from)).collect()
    }
}

/// The customer credit a receipt leaves: a document issued with the receipt,
/// carrying its number, reference, date and entry, with one line on the
/// customer credit account, linked to the receipt it came from.
async fn save_credit(
    sql: &(impl tokio_postgres::GenericClient + Sync),
    tenant: i64,
    company: i64,
    payment: &Value,
    credit: PreparedCredit,
    changes: &mut Vec<bilibi_database::Change>,
) -> bilibi_database::Result<()> {
    let reference = payment["payment_reference"]
        .as_str()
        .ok_or(Error::Internal)?;
    let draft = store::save(
        sql,
        DOCUMENTS,
        tenant,
        company,
        None,
        json!({
            "kind":"customer_credit","status":"draft","series_id":payment["series_id"],
            "customer_id":payment["customer_id"],"currency_code":payment["currency_code"],
            "issue_date":payment["payment_date"],"due_date":payment["payment_date"],
            "description":format!("Amount of receipt {reference} not applied to an invoice"),
            "subtotal":credit.amount,"tax_total":"0","total":credit.amount,
        }),
        changes,
    )
    .await?;
    let id = draft["id"].as_i64().ok_or(Error::Internal)?;
    store::save(
        sql,
        store::LINES,
        tenant,
        company,
        None,
        json!({
            "document_id":id,"line_number":1,
            "description":"Received and not applied to an invoice",
            "quantity":"1","unit_price":credit.amount,"amount":credit.amount,
            "account_id":credit.account_id,"tax_amount":"0",
        }),
        changes,
    )
    .await?;
    let mut issued = draft.clone();
    issued["status"] = json!("issued");
    issued["document_number"] = payment["payment_number"].clone();
    issued["document_reference"] = json!(reference);
    issued["receivable_account_id"] = json!(credit.account_id);
    issued["party_code"] = json!(credit.party_code);
    issued["party_name"] = json!(credit.party_name);
    issued["issued_by_user_id"] = payment["recorded_by_user_id"].clone();
    issued["issued_by_impersonator_id"] = payment["recorded_by_impersonator_id"].clone();
    issued["ledger_entry_id"] = payment["ledger_entry_id"].clone();
    issued["ledger_entry_number"] = payment["ledger_entry_number"].clone();
    store::save(sql, ISSUED, tenant, company, Some(draft), issued, changes).await?;
    store::save(
        sql,
        store::RECEIPT_CREDITS,
        tenant,
        company,
        None,
        json!({"payment_id":payment["id"],"document_id":id,"amount":credit.amount}),
        changes,
    )
    .await?;
    Ok(())
}

fn required_date(data: &mut Value, field: &str) -> Result<Date> {
    v::date(data, field)?;
    data[field]
        .as_str()
        .ok_or_else(|| invalid(field, "can't be blank"))?
        .parse()
        .map_err(|_| invalid(field, "is invalid"))
}
fn input_money(data: &mut Value, units: MinorUnits) -> Result<Money> {
    let decimal = v::decimal(
        data,
        "amount",
        true,
        Decimal::from(10_000_000_000_000_000_i64),
        units.get(),
    )?;
    let amount = Money::new(decimal, units).map_err(|e| invalid("amount", e.to_string()))?;
    data["amount"] = json!(amount);
    Ok(amount)
}
pub(super) async fn read_payment(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<Payment> {
    let row = client
        .query_opt(
            &format!("{PAYMENT_READ} AND p.id=$3"),
            &[&tenant, &company, &id],
        )
        .await?
        .ok_or(R::PaymentNotFound)?;
    Ok(store::decode(row.get(0))?)
}
