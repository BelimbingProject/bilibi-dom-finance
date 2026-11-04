//! Bilimbi Receivables.Documents issuing and Payments: record and owned entry
//! commit together under the company's writable lock. Saved lines never change.
use crate::{
    authorize, documents, invalid,
    store::{self, Table, CUSTOMERS, DOCUMENTS, SERIES, SETUPS},
    validation as v, Document, Payment, PaymentQuery, ReceivableError as R, Receivables, Result,
    ISSUE, MODULE, POST_PAYMENTS, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_attachments::{AttachmentError, Refresh};
use bilibi_finance_ledger::{
    Account, AccountType, EntryOwner, MinorUnits, Money, NewEntry, NewLine, PostOptions, Reversal,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct PreparedIssue {
    pub before: Value,
    pub after: Value,
    pub entry: NewEntry,
    pub options: PostOptions,
    pub facts: Value,
}
pub(super) struct PreparedPayment {
    pub after: Value,
    pub allocations: Vec<Value>,
    pub entry: NewEntry,
    pub options: PostOptions,
    pub facts: Value,
}
pub(super) struct PreparedReversal {
    pub before: Payment,
    pub after: Value,
    pub owner: EntryOwner,
    pub reversal: Reversal,
}

const ISSUED: Table = Table {
    columns: &[
        "status",
        "document_number",
        "document_reference",
        "receivable_account_id",
        "due_date",
        "party_code",
        "party_name",
        "party_tax_identifier",
        "party_registration_identifier",
        "party_email",
        "party_phone",
        "party_line1",
        "party_line2",
        "party_line3",
        "party_locality",
        "party_postcode",
        "party_admin1_code",
        "party_country_iso",
        "issued_by_user_id",
        "issued_by_impersonator_id",
        "ledger_entry_id",
        "ledger_entry_number",
    ],
    ..DOCUMENTS
};
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
 'reversal',(SELECT to_jsonb(r) FROM finance_receivables_payment_reversals r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.payment_id=p.id))
 FROM finance_receivables_payments p WHERE p.tenant_id=$1 AND p.company_id=$2";

impl Receivables {
    /// Issue the saved draft as it stands; party/control are frozen now,
    /// whereas saved line/tax snapshots are never recalculated.
    pub async fn issue_document(&self, scope: &Scope, company: i64, id: i64) -> Result<Document> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                ISSUE,
                "finance.receivables.issue_document",
            )
            .await?;
        let result = self.issue_document_on(&mut tx, scope, company, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn prepare_issue(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        id: i64,
    ) -> Result<PreparedIssue> {
        self.join(client, scope, company, ISSUE).await?;
        let tenant = scope.tenant();
        self.ledger.get_book_on(client, scope, company).await?;
        let setup = store::find(client, SETUPS, tenant, company, None)
            .await?
            .ok_or(R::NotConfigured)?;
        let chart = self.ledger.list_accounts_on(client, scope, company).await?;
        let draft = documents::read(client, tenant, company, id).await?;
        if draft.status != "draft" {
            return Err(R::NotADraft);
        }
        let old = store::find(client, DOCUMENTS, tenant, company, Some(id))
            .await?
            .ok_or(R::DocumentNotFound)?;
        let mut data = old.clone();
        let customer_row = store::find(client, CUSTOMERS, tenant, company, Some(draft.customer_id))
            .await?
            .ok_or(R::CustomerNotFound)?;
        let customer = self.customer(client, scope, customer_row.clone()).await?;
        let series = store::find(client, SERIES, tenant, company, Some(draft.series_id))
            .await?
            .ok_or(R::SeriesNotFound)?;
        let date: Date = draft
            .issue_date
            .as_deref()
            .ok_or_else(|| invalid("issue_date", "can't be blank"))?
            .parse()
            .map_err(|_| invalid("issue_date", "is invalid"))?;
        if draft.kind == "invoice" {
            if !customer.active {
                return Err(invalid("customer_id", "is inactive"));
            }
            self.invoiceable(client, scope, company, &customer).await?;
        }
        if series["active"] != true {
            return Err(invalid("series_id", "is inactive"));
        }
        if draft.lines.is_empty() {
            return Err(invalid("lines", "needs at least one line"));
        }
        if !draft.total.is_positive() {
            return Err(invalid("total", "must be greater than zero"));
        }
        let credit = draft.kind == "credit_note";
        let mut credited = None;
        let control = if credit {
            let invoice = documents::read(
                client,
                tenant,
                company,
                draft.credited_document_id.ok_or(R::DocumentNotFound)?,
            )
            .await?;
            if date.to_string() < invoice.issue_date.clone().ok_or(R::DocumentNotFound)? {
                return Err(invalid(
                    "issue_date",
                    "cannot be before the credited invoice's",
                ));
            }
            crate::write_offs::settlement_date(
                client,
                scope,
                company,
                invoice.id,
                date,
                "issue_date",
            )
            .await?;
            if crate::settlements::lowest(client, scope.tenant(), company, invoice.id, date).await?
                < draft.total
            {
                return Err(invalid(
                    "total",
                    "exceeds what the credited invoice still owes",
                ));
            }
            let control = invoice.receivable_account_id.ok_or(R::DocumentNotFound)?;
            credited = Some(invoice);
            control
        } else {
            setup["receivable_account_id"]
                .as_i64()
                .ok_or(Error::Internal)?
        };
        posting_account(&chart, control, AccountType::Asset, !credit)
            .map_err(|p| invalid("lines", format!("receivable account {p}")))?;
        for line in &draft.lines {
            posting_account(&chart, line.account_id, AccountType::Income, true)
                .map_err(|p| invalid("lines", format!("line {} account {p}", line.line_number)))?;
        }
        for line in &draft.tax_lines {
            posting_account(&chart, line.account_id, AccountType::Liability, true)
                .map_err(|p| invalid("lines", format!("tax code {} account {p}", line.tax_code)))?;
        }
        if !credit && draft.due_date.is_none() {
            let days = i64::from(customer.payment_terms_days);
            data["due_date"] = json!(date
                .checked_add(jiff::SignedDuration::from_hours(days * 24))
                .map_err(|_| invalid("due_date", "is invalid"))?
                .to_string());
        }
        let (number, reference) = next_number(client, tenant, company, &series, false).await?;
        data["status"] = json!("issued");
        data["document_number"] = json!(number);
        data["document_reference"] = json!(reference);
        data["receivable_account_id"] = json!(control);
        data["issued_by_user_id"] = json!(scope.user());
        data["issued_by_impersonator_id"] = json!(scope.impersonator().map(|s| s.user()));
        // Preview and commit freeze the same Core facts on the handed connection.
        let day = date.to_string().try_into().map_err(|_| Error::Internal)?;
        let party = self
            .frozen_party(client, scope, &customer.party, &day)
            .await?;
        for (field, value) in party.as_object().into_iter().flatten() {
            data[field] = value.clone();
        }
        let mut lines = vec![if credit {
            NewLine::credit(control, draft.total)
        } else {
            NewLine::debit(control, draft.total)
        }];
        // Keep income and tax groupings separate, as in the reference.
        for amounts in [
            group(draft.lines.iter().map(|l| (l.account_id, l.amount))),
            group(draft.tax_lines.iter().map(|l| (l.account_id, l.tax_amount))),
        ] {
            for (account, amount) in amounts {
                if !amount.is_zero() {
                    lines.push(if credit {
                        NewLine::debit(account, amount)
                    } else {
                        NewLine::credit(account, amount)
                    });
                }
            }
        }
        let entry = NewEntry {
            entry_date: date,
            description: format!(
                "{} {reference} to {}",
                if credit { "Credit note" } else { "Invoice" },
                customer.name
            ),
            reference: Some(reference),
            lines,
        };
        let options = PostOptions {
            owner: Some(owner("document", id)),
            settlement_account_ids: if credit { vec![control] } else { vec![] },
        };
        Ok(PreparedIssue {
            before: old,
            after: data,
            entry,
            options,
            // The effective control is frozen in `after` and the posting;
            // unrelated defaults cannot change this saved draft's entry.
            facts: json!({"draft":draft,"customer":customer,"party":customer.party,"series":series,"credited_invoice":credited}),
        })
    }

    pub(super) async fn issue_document_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Document> {
        let prepared = self.prepare_issue(tx.sql(), scope, company, id).await?;
        self.issue_prepared_on(tx, scope, company, id, prepared)
            .await
    }

    /// Store the exact preparation bound by the transaction-local review fence.
    /// Re-reading Core facts here could freeze a concurrent, unreviewed change.
    pub(super) async fn issue_prepared_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: i64,
        prepared: PreparedIssue,
    ) -> Result<Document> {
        self.join(tx.sql(), scope, company, ISSUE).await?;
        let PreparedIssue {
            before: old,
            after: mut data,
            entry,
            options,
            ..
        } = prepared;
        let tenant = scope.tenant();
        let entry = self
            .ledger
            .post_entry_in(tx, scope, company, &entry, &options)
            .await?;
        data["ledger_entry_id"] = json!(entry.id);
        data["ledger_entry_number"] = json!(entry.entry_number);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                store::save(sql, ISSUED, tenant, company, Some(old), data, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        // Source authorization refuses before any refresh mutation, so report
        // it without adding VIEW to the issuer's grants (Bilimbi parity).
        // A failed queue write must still roll back a potentially poisoned tx.
        let refresh = match self
            .attachments
            .refresh_retention(tx, scope, company, "finance/receivables/document", id)
            .await
        {
            Ok(Refresh::Queued) => "queued",
            Ok(Refresh::None) => "none",
            Err(AttachmentError::Unauthorized) => "error: unauthorized",
            Err(error) => return Err(error.into()),
        };
        let mut result = documents::read(tx.sql(), tenant, company, id).await?;
        result.retention_refresh = Some(refresh.into());
        Ok(result)
    }

    /// Immutable receipt, allocated in full to distinct invoices at record time.
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
        if inputs.is_empty() {
            return Err(invalid("allocations", "needs at least one invoice"));
        }
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
        if total != amount {
            return Err(invalid(
                "allocations",
                "do not add up to the payment amount",
            ));
        }
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
            entry,
            options,
            facts: json!({"book":book,"customer":customer,"series":series,"invoices":invoices,"owner":{"module":MODULE,"record":"payment:<allocated at commit>"}}),
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
    /// Follow issued documents in posting order, for consumers such as e-invoice.
    pub async fn list_issued_documents(
        &self,
        scope: &Scope,
        company: i64,
        limit: Option<i64>,
        after_entry_number: Option<i64>,
    ) -> Result<Vec<Document>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, company, VIEW).await?;
        let limit = limit.unwrap_or(100);
        let after = after_entry_number.unwrap_or(0);
        if !(1..=500).contains(&limit) || after < 0 {
            return Err(R::InvalidOption);
        }
        client.query(&format!("{} AND d.status='issued' AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id) AND d.ledger_entry_number>$3 ORDER BY d.ledger_entry_number LIMIT $4", documents::READ), &[&scope.tenant(), &company, &after, &limit]).await.map_err(Error::from)?.into_iter().map(|r| store::decode(r.get(0)).map_err(R::from)).collect()
    }
}

fn owner(kind: &str, id: i64) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("{kind}:{id}"),
    }
}
fn group(rows: impl IntoIterator<Item = (i64, Money)>) -> BTreeMap<i64, Money> {
    let mut totals = BTreeMap::new();
    for (id, amount) in rows {
        let total = totals.entry(id).or_insert(Money::ZERO);
        *total = *total + amount;
    }
    totals
}
fn posting_account(
    chart: &[Account],
    id: i64,
    kind: AccountType,
    active: bool,
) -> std::result::Result<(), String> {
    let account = chart
        .iter()
        .find(|a| a.id == id)
        .ok_or("is not a ledger account of this company")?;
    if account.is_group {
        return Err("is a ledger account group; choose an account under it".into());
    }
    if active && !account.active {
        return Err("is an inactive ledger account".into());
    }
    if account.account_type != kind {
        return Err(format!("must be a ledger {} account", kind.as_str()));
    }
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
async fn next_number(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    series: &Value,
    payment: bool,
) -> Result<(i64, String)> {
    let (table, column) = if payment {
        (PAYMENTS.name, "payment_number")
    } else {
        (DOCUMENTS.name, "document_number")
    };
    let id = series["id"].as_i64().ok_or(Error::Internal)?;
    let number: i64 = client.query_opt(&format!("SELECT COALESCE(max({column}),0)+1 FROM {table} WHERE tenant_id=$1 AND company_id=$2 AND series_id=$3"), &[&tenant, &company, &id]).await?.ok_or(Error::Internal)?.get(0);
    let width = series["number_width"].as_u64().ok_or(Error::Internal)? as usize;
    Ok((
        number,
        format!(
            "{}{:0width$}",
            series["prefix"].as_str().ok_or(Error::Internal)?,
            number
        ),
    ))
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
