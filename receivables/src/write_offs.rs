//! Invoice-linked bad-debt journals extend Bilimbi Receivables' settlement seam.
//! Tax eligibility and tax adjustments are deliberately separate decisions.
use crate::{
    documents, invalid,
    store::{self, Table},
    validation as v, Receivables, Result, APPROVE_WRITE_OFFS, MODULE, SUBMIT_WRITE_OFFS,
};
use bilibi_agent_api::OperationClient;
use bilibi_database::{Error, MutationTransaction};
use bilibi_finance_ledger::{
    Approval, EntryOwner, Money, NewEntry, NewLine, PostOptions, Preparer, Reversal,
};
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOff {
    pub id: i64,
    pub document_id: i64,
    pub entry_date: String,
    #[serde(deserialize_with = "crate::records::money")]
    pub amount: Money,
    pub offset_account_id: i64,
    pub reason: String,
    /// A durable collection/assessment evidence reference, not a claim approval.
    pub evidence: String,
    pub submitted_by_user_id: i64,
    pub submitted_by_impersonator_id: Option<i64>,
    pub submitted_at: String,
    pub events: Vec<WriteOffEvent>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOffEvent {
    pub event: String,
    pub effective_date: String,
    pub reason: String,
    pub ledger_entry_id: Option<i64>,
    pub ledger_entry_number: Option<i64>,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub occurred_at: String,
}
impl WriteOff {
    pub fn status(&self) -> &str {
        self.events.last().map_or("submitted", |e| e.event.as_str())
    }
}
const JOURNALS: Table = Table {
    name: "finance_receivables_write_offs",
    kind: "finance.receivables.write_off",
    timestamps: false,
    columns: &[
        "document_id",
        "entry_date",
        "amount",
        "offset_account_id",
        "reason",
        "evidence",
        "submitted_by_user_id",
        "submitted_by_impersonator_id",
    ],
};
const EVENTS: Table = Table {
    name: "finance_receivables_write_off_events",
    kind: "finance.receivables.write_off_event",
    timestamps: false,
    columns: &[
        "write_off_id",
        "event",
        "effective_date",
        "reason",
        "ledger_entry_id",
        "ledger_entry_number",
        "actor_user_id",
        "actor_impersonator_id",
    ],
};
fn date(input: &Value, key: &str) -> Result<Date> {
    input[key]
        .as_str()
        .ok_or_else(|| invalid(key, "is required"))?
        .parse()
        .map_err(|_| invalid(key, "is not a date"))
}
fn id(input: &Value, key: &str) -> Result<i64> {
    input[key]
        .as_i64()
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid(key, "is required"))
}
fn capability(action: &str) -> &'static str {
    if action == "submit" {
        SUBMIT_WRITE_OFFS
    } else {
        APPROVE_WRITE_OFFS
    }
}
fn owner(id: i64) -> EntryOwner {
    EntryOwner {
        module: MODULE.into(),
        record: format!("write_off:{id}"),
    }
}
fn independent_reviewer() -> crate::ReceivableError {
    invalid(
        "base",
        "Someone else must review this bad-debt journal, including when acting through an agent or impersonating. The book's separation setting does not waive this requirement.",
    )
}
fn approval(w: &WriteOff, action: &str) -> Approval {
    Approval {
        source: MODULE.into(),
        action: format!("write_off_{action}"),
        subject_type: "write_off".into(),
        subject_id: w.id,
        subject: format!("Write-off #{} for invoice #{}", w.id, w.document_id),
        prepared_by: vec![Preparer {
            user_id: w.submitted_by_user_id,
            impersonator_id: w.submitted_by_impersonator_id,
        }],
        override_reason: None,
    }
}
pub(super) async fn read(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    write_off: i64,
) -> Result<WriteOff> {
    let row=client.query_opt("SELECT to_jsonb(w) || jsonb_build_object('events', COALESCE((SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM finance_receivables_write_off_events e WHERE e.write_off_id=w.id AND e.tenant_id=$1 AND e.company_id=$2),'[]'::jsonb)) FROM finance_receivables_write_offs w WHERE w.tenant_id=$1 AND w.company_id=$2 AND w.id=$3", &[&scope.tenant(), &company, &write_off]).await?.ok_or_else(|| invalid("write_off_id","was not found"))?;
    Ok(store::decode(row.get(0))?)
}
/// Refuse backdating across a write-off or restoration. This keeps dated
/// balances nonnegative without changing the original invoice or tax snapshots.
pub(super) async fn settlement_date(
    client: &dyn OperationClient,
    scope: &Scope,
    company: i64,
    document: i64,
    day: Date,
    field: &str,
) -> Result<()> {
    crate::opening::settlement_date(client, scope, company, document, day).await?;
    if client.query_opt("SELECT EXISTS(SELECT 1 FROM finance_receivables_write_offs w JOIN finance_receivables_write_off_events e ON e.write_off_id=w.id AND e.tenant_id=$1 AND e.company_id=$2 WHERE w.tenant_id=$1 AND w.company_id=$2 AND w.document_id=$3 AND e.event IN ('approved','reversed') AND e.effective_date>$4::text::date)", &[&scope.tenant(),&company,&document,&day.to_string()]).await?.ok_or(Error::Internal)?.get(0) {
        return Err(invalid(field,"cannot precede a write-off or its reversal; use a later settlement date"));
    }
    Ok(())
}
pub(super) async fn eligible_offsets(
    client: &dyn OperationClient,
    service: &Receivables,
    scope: &Scope,
    company: i64,
) -> Result<Vec<bilibi_finance_ledger::Account>> {
    let chart = service
        .ledger
        .list_accounts_on(client, scope, company)
        .await?;
    let rows=client.query("SELECT receivable_account_id FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND receivable_account_id IS NOT NULL UNION SELECT receivable_account_id FROM finance_receivables_setups WHERE tenant_id=$1 AND company_id=$2",&[&scope.tenant(),&company]).await?;
    let mut barred: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
    let deposits = client.query("SELECT DISTINCT deposit_account_id FROM finance_receivables_payments WHERE tenant_id=$1 AND company_id=$2",&[&scope.tenant(),&company]).await?;
    barred.extend(deposits.iter().map(|r| r.get::<_, i64>(0)));
    Ok(chart
        .into_iter()
        .filter(|a| {
            a.active
                && matches!(a.account_type.as_str(), "asset" | "expense")
                && !barred.contains(&a.id)
        })
        .collect())
}
async fn validate(
    client: &dyn OperationClient,
    service: &Receivables,
    scope: &Scope,
    company: i64,
    input: &Value,
) -> Result<(Value, NewEntry, PostOptions)> {
    let document = id(input, "document_id")?;
    let invoice = documents::read(client, scope.tenant(), company, document).await?;
    let day = date(input, "entry_date")?;
    if invoice.kind != "invoice" || invoice.status != "issued" {
        return Err(invalid("document_id", "must be an issued invoice"));
    }
    if invoice
        .due_date
        .as_ref()
        .is_none_or(|d| d >= &day.to_string())
    {
        return Err(invalid("entry_date", "must be after the invoice due date"));
    }
    settlement_date(client, scope, company, document, day, "entry_date").await?;
    // A proposed journal cannot retrospectively consume a balance that a later
    // receipt/credit has already settled, including subsequently reversed receipts.
    let later:bool=client.query_opt("SELECT EXISTS(SELECT 1 FROM finance_receivables_allocations a JOIN finance_receivables_payments p ON p.id=a.payment_id AND p.tenant_id=$1 AND p.company_id=$2 LEFT JOIN finance_receivables_payment_reversals r ON r.payment_id=p.id AND r.tenant_id=$1 AND r.company_id=$2 WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.document_id=$3 AND (p.payment_date>$4::text::date OR r.reversal_date>$4::text::date)) OR EXISTS(SELECT 1 FROM finance_receivables_documents c WHERE c.tenant_id=$1 AND c.company_id=$2 AND c.credited_document_id=$3 AND c.status='issued' AND c.issue_date>$4::text::date)",&[&scope.tenant(),&company,&document,&day.to_string()]).await?.ok_or(Error::Internal)?.get(0);
    if later {
        return Err(invalid(
            "entry_date",
            "cannot precede a recorded receipt, reversal or credit note",
        ));
    }
    let book = service.ledger.get_book_on(client, scope, company).await?;
    let amount = Money::parse(
        input["amount"]
            .as_str()
            .ok_or_else(|| invalid("amount", "is required"))?,
        book.minor_units,
    )
    .map_err(|e| invalid("amount", e.to_string()))?;
    if amount <= Money::ZERO
        || crate::settlements::lowest(client, scope.tenant(), company, document, day).await?
            < amount
    {
        return Err(invalid(
            "amount",
            "must be positive and no more than the invoice outstanding",
        ));
    }
    let offset = id(input, "offset_account_id")?;
    let control = invoice.receivable_account_id.ok_or(Error::Internal)?;
    if !eligible_offsets(client, service, scope, company)
        .await?
        .iter()
        .any(|a| a.id == offset)
    {
        return Err(invalid(
            "offset_account_id",
            "must be an active allowance asset or bad-debt expense account, not a receivables control or an account receipts are paid into (a bank or cash account)",
        ));
    }
    let mut data = v::merge(
        None,
        input,
        &[
            "document_id",
            "entry_date",
            "amount",
            "offset_account_id",
            "reason",
            "evidence",
        ],
    )?;
    v::text(&mut data, "reason", true, 500)?;
    v::text(&mut data, "evidence", true, 500)?;
    data["amount"] = json!(amount.amount().to_string());
    data["submitted_by_user_id"] = json!(scope.user());
    data["submitted_by_impersonator_id"] = json!(scope.impersonator().map(|p| p.user()));
    let entry = NewEntry {
        entry_date: day,
        description: format!(
            "Bad debt: {}",
            invoice.document_reference.as_deref().unwrap_or_default()
        ),
        reference: invoice.document_reference.clone(),
        lines: vec![
            NewLine::debit(offset, amount),
            NewLine::credit(control, amount),
        ],
    };
    Ok((
        data,
        entry,
        PostOptions {
            owner: None,
            settlement_account_ids: vec![control],
        },
    ))
}
impl Receivables {
    pub async fn submit_write_off(
        &self,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<WriteOff> {
        self.write_off(scope, company, "submit", input).await
    }
    pub async fn approve_write_off(
        &self,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<WriteOff> {
        self.write_off(scope, company, "approve", input).await
    }
    pub async fn reject_write_off(
        &self,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<WriteOff> {
        self.write_off(scope, company, "reject", input).await
    }
    pub async fn reverse_write_off(
        &self,
        scope: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<WriteOff> {
        self.write_off(scope, company, "reverse", input).await
    }
    async fn write_off(
        &self,
        scope: &Scope,
        company: i64,
        action: &str,
        input: &Value,
    ) -> Result<WriteOff> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut client,
                scope,
                company,
                capability(action),
                "finance.receivables.write_off",
            )
            .await?;
        let result = self
            .write_off_on(&mut tx, scope, company, action, input)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn review_write_off(
        &self,
        client: &dyn OperationClient,
        scope: &Scope,
        company: i64,
        action: &str,
        input: &Value,
    ) -> Result<Value> {
        self.join(client, scope, company, capability(action))
            .await?;
        if action == "submit" {
            let (data, entry, options) = validate(client, self, scope, company, input).await?;
            let invoice =
                documents::read(client, scope.tenant(), company, id(input, "document_id")?).await?;
            let posting = self
                .ledger
                .preview_post_entry_on(client, scope, company, &entry, &options)
                .await?;
            return Ok(
                json!({"invoice":invoice,"proposed":data,"posting":posting,"effect":"No posting until independently approved. No tax relief or filing."}),
            );
        }
        let w = read(client, scope, company, id(input, "write_off_id")?).await?;
        let invoice = documents::read(client, scope.tenant(), company, w.document_id).await?;
        if (action == "reverse" && w.status() != "approved")
            || (action != "reverse" && w.status() != "submitted")
        {
            return Err(invalid(
                "write_off_id",
                "has already been decided or cannot be reversed",
            ));
        }
        let mut reason = v::merge(None, input, &["reason"])?;
        v::text(&mut reason, "reason", true, 500)?;
        // The ledger owns identity overlap, including impersonation. Its public
        // review names a same-person exception when the book permits one; this
        // workflow always requires independence, so that exception cannot waive it.
        let approval = match self
            .ledger
            .preview_approval_on(client, scope, company, &approval(&w, action))
            .await
        {
            Err(crate::LedgerError::SamePerson) => return Err(independent_reviewer()),
            other => other?,
        };
        if approval["approval_exception"]["same_person"] == true {
            return Err(independent_reviewer());
        }
        let posting = if action == "approve" {
            let mut attrs = serde_json::to_value(&w).map_err(|_| Error::Internal)?;
            attrs["amount"] = json!(w.amount.amount().to_string());
            let (_, entry, mut options) = validate(client, self, scope, company, &attrs).await?;
            options.owner = Some(owner(w.id));
            self.ledger
                .preview_post_entry_on(client, scope, company, &entry, &options)
                .await?
        } else if action == "reverse" {
            let day = date(input, "reversal_date")?;
            if day.to_string() < w.entry_date {
                return Err(invalid(
                    "reversal_date",
                    "cannot precede the write-off date",
                ));
            }
            settlement_date(client, scope, company, w.document_id, day, "reversal_date").await?;
            let entry = w
                .events
                .iter()
                .find(|e| e.event == "approved")
                .and_then(|e| e.ledger_entry_id)
                .ok_or(Error::Internal)?;
            self.ledger
                .preview_reverse_owned_entry_on(
                    client,
                    scope,
                    company,
                    entry,
                    &owner(w.id),
                    &Reversal {
                        entry_date: day,
                        description: reason["reason"].as_str().unwrap_or_default().into(),
                        reference: None,
                    },
                )
                .await?
        } else {
            Value::Null
        };
        Ok(
            json!({"before":w,"invoice":invoice,"action":action,"reason":reason,"approval":approval,"posting":posting}),
        )
    }
    pub(super) async fn write_off_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        action: &str,
        input: &Value,
    ) -> Result<WriteOff> {
        self.review_write_off(tx.sql(), scope, company, action, input)
            .await?;
        let tenant = scope.tenant();
        if action == "submit" {
            let (data, _, _) = validate(tx.sql(), self, scope, company, input).await?;
            let saved = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let mut changes = vec![];
                        let row =
                            store::save(sql, JOURNALS, tenant, company, None, data, &mut changes)
                                .await?;
                        Ok((row, changes))
                    })
                })
                .await?;
            return read(
                tx.sql(),
                scope,
                company,
                saved["id"].as_i64().ok_or(Error::Internal)?,
            )
            .await;
        }
        let w = read(tx.sql(), scope, company, id(input, "write_off_id")?).await?;
        self.ledger
            .check_approval(tx, scope, company, &approval(&w, action))
            .await?;
        let (event, day, entry) = if action == "approve" {
            let mut attrs = serde_json::to_value(&w).map_err(|_| Error::Internal)?;
            attrs["amount"] = json!(w.amount.amount().to_string());
            let (_, entry, mut options) = validate(tx.sql(), self, scope, company, &attrs).await?;
            options.owner = Some(owner(w.id));
            let posted = self
                .ledger
                .post_entry_in(tx, scope, company, &entry, &options)
                .await?;
            ("approved", w.entry_date.clone(), Some(posted))
        } else if action == "reverse" {
            let day = date(input, "reversal_date")?;
            let entry = w
                .events
                .iter()
                .find(|e| e.event == "approved")
                .and_then(|e| e.ledger_entry_id)
                .ok_or(Error::Internal)?;
            let posted = self
                .ledger
                .reverse_owned_entry(
                    tx,
                    scope,
                    company,
                    entry,
                    &owner(w.id),
                    &Reversal {
                        entry_date: day,
                        description: input["reason"].as_str().unwrap_or_default().trim().into(),
                        reference: None,
                    },
                )
                .await?;
            ("reversed", day.to_string(), Some(posted))
        } else {
            ("rejected", w.entry_date.clone(), None)
        };
        let data = json!({"write_off_id":w.id,"event":event,"effective_date":day,"reason":input["reason"].as_str().unwrap_or_default().trim(),"ledger_entry_id":entry.as_ref().map(|e|e.id),"ledger_entry_number":entry.as_ref().map(|e|e.entry_number),"actor_user_id":scope.user(),"actor_impersonator_id":scope.impersonator().map(|p|p.user())});
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = vec![];
                store::save(sql, EVENTS, tenant, company, None, data, &mut changes).await?;
                Ok(((), changes))
            })
        })
        .await?;
        read(tx.sql(), scope, company, w.id).await
    }
}
