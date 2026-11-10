//! Bilimbi Receivables.Documents issuing: record and owned entry commit
//! together under the company's writable lock. Saved lines never change.
//! Receipts are in `receipts.rs`, which shares the numbering and posting
//! helpers at the end of this file.
use crate::{
    authorize, documents, invalid,
    store::{self, Table, CUSTOMERS, DOCUMENTS, SERIES, SETUPS},
    Document, ReceivableError as R, Receivables, Result, ISSUE, MODULE, VIEW,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_attachments::{AttachmentError, Refresh};
use bilibi_finance_ledger::{
    Account, AccountType, EntryOwner, Money, NewEntry, NewLine, PostOptions,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct PreparedIssue {
    pub before: Value,
    pub after: Value,
    pub entry: NewEntry,
    pub options: PostOptions,
    pub facts: Value,
}

pub(super) const ISSUED: Table = Table {
    columns: &[
        "status",
        "document_number",
        "document_reference",
        "receivable_account_id",
        "issue_date",
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
        if client.query_opt("SELECT 1 FROM finance_receivables_sales_orders WHERE tenant_id=$1 AND company_id=$2 AND document_id=$3 AND status='cancelled'", &[&tenant,&company,&id]).await?.is_some() {
            return Err(invalid("base", "The sales order was cancelled. Discard this draft or review the order before invoicing."));
        }
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
        // A draft saved without an issue date is issued today, in the
        // company's own time. The review shows the date before anyone confirms.
        let date: Date = match draft.issue_date.as_deref() {
            Some(text) => text
                .parse()
                .map_err(|_| invalid("issue_date", "is invalid"))?,
            None => {
                let today = self.today(client, scope, company).await?;
                if draft
                    .due_date
                    .as_deref()
                    .is_some_and(|due| due < today.to_string().as_str())
                {
                    return Err(invalid("due_date", "cannot be before the issue date"));
                }
                data["issue_date"] = json!(today.to_string());
                today
            }
        };
        if draft.kind == "invoice" {
            Self::invoiceable(&customer)?;
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

    /// Today's date where the company is, from its time zone setting.
    async fn today(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
    ) -> Result<Date> {
        let zone = self
            .settings
            .get(
                client,
                scope,
                bilibi_settings::Target::Company(company.try_into().map_err(|_| Error::Invalid)?),
                bilibi_datetime::TIMEZONE_KEY,
            )
            .await?;
        let zone = zone
            .value
            .as_str()
            .and_then(|name| jiff::tz::TimeZone::get(name).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        Ok(jiff::Timestamp::now().to_zoned(zone).date())
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
        client.query(&format!("{} AND d.status='issued' AND d.kind IN ('invoice','credit_note') AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id) AND d.ledger_entry_number>$3 ORDER BY d.ledger_entry_number LIMIT $4", documents::READ), &[&scope.tenant(), &company, &after, &limit]).await.map_err(Error::from)?.into_iter().map(|r| store::decode(r.get(0)).map_err(R::from)).collect()
    }
}

pub(super) fn owner(kind: &str, id: i64) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("{kind}:{id}"),
    }
}
pub(super) fn group(rows: impl IntoIterator<Item = (i64, Money)>) -> BTreeMap<i64, Money> {
    let mut totals = BTreeMap::new();
    for (id, amount) in rows {
        let total = totals.entry(id).or_insert(Money::ZERO);
        *total = *total + amount;
    }
    totals
}
pub(super) fn posting_account(
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
pub(super) async fn next_number(
    client: &dyn OperationClient,
    tenant: i64,
    company: i64,
    series: &Value,
    payment: bool,
) -> Result<(i64, String)> {
    let (table, column) = if payment {
        ("finance_receivables_payments", "payment_number")
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
