//! Bilimbi.Finance.Payables document recording, approval, void and owned post.
use crate::*;
use crate::{
    settings::{account, optional, string},
    store::Table,
};
use bilibi_finance_ledger::{
    AccountType, Approval, EntryOwner, MinorUnits, NewEntry, NewLine, PostOptions, Preparer,
};
use rust_decimal::RoundingStrategy;
use serde_json::json;

/// Why a corrected draft's predecessor is voided.
pub(super) const REPLACED: &str = "Replaced by a corrected draft.";

/// Why approving a document did not also post it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotPosted {
    /// The three-way match would hold the bill; posting it records the hold.
    Match,
    /// The approver may not post documents or ledger entries.
    Permission,
    /// Posting refused it (a closed period, no payable account), in the
    /// owner's words.
    Refused { message: String },
}
fn not_posted<T>(e: PayablesError) -> Result<std::result::Result<T, NotPosted>> {
    use bilibi_finance_ledger::LedgerError;
    match e {
        PayablesError::Failed(_) | PayablesError::Ledger(LedgerError::Failed(_)) => Err(e),
        PayablesError::Unauthorized | PayablesError::Ledger(LedgerError::Unauthorized) => {
            Ok(Err(NotPosted::Permission))
        }
        PayablesError::Invalid(errors) | PayablesError::Ledger(LedgerError::Invalid(errors)) => {
            Ok(Err(NotPosted::Refused {
                message: errors
                    .iter()
                    .map(|e| match e.field {
                        "entry_date" => format!("The document date {}.", e.message),
                        _ => e.message.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            }))
        }
        e => Ok(Err(NotPosted::Refused {
            message: e.to_string(),
        })),
    }
}

pub(super) enum PreparedPosting {
    Held(Document, Vec<MatchLine>),
    Post {
        document: Document,
        payable: i64,
        entry: NewEntry,
        options: PostOptions,
        matched: Option<Vec<MatchLine>>,
        capacity: serde_json::Value,
    },
}

fn checked(amount: Money, units: MinorUnits, field: &'static str, positive: bool) -> Result<Money> {
    let amount = Money::new(amount.amount(), units).map_err(|e| invalid(field, e.to_string()))?;
    if amount < Money::ZERO || (positive && amount.is_zero()) {
        return Err(invalid(
            field,
            if positive {
                "must be greater than 0"
            } else {
                "must be greater than or equal to 0"
            },
        ));
    }
    Ok(amount)
}
// Retain the dated capacity as a review fact, even when a changed balance
// would still permit the note. Consent covers the facts, not just validity.
async fn note_capacity(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    bill: Option<i64>,
    as_of: Date,
    amount: Money,
) -> Result<serde_json::Value> {
    let Some(bill) = bill else {
        return Ok(serde_json::Value::Null);
    };
    crate::opening::settlement_date(c, tenant, company, bill, as_of, "document_date").await?;
    let lowest = balances::lowest(c, tenant, company, bill, as_of).await?;
    if amount > lowest {
        return Err(PayablesError::ExceedsOutstanding);
    }
    Ok(json!({"bill_id":bill,"as_of":as_of.to_string(),"lowest_outstanding":lowest}))
}
/// The draft bill a corrected draft replaces.
pub(super) async fn replaceable(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    id: i64,
) -> Result<Document> {
    let d = store::document_record(c, tenant, company, id)
        .await?
        .ok_or(PayablesError::DocumentNotFound)?;
    if d.document_type != "bill" {
        return Err(invalid("document_id", "is not a bill"));
    }
    if d.status != "draft" {
        return Err(PayablesError::NotDraft);
    }
    Ok(d)
}
fn filters(kind: &Option<String>, status: &Option<String>) -> Result<()> {
    if kind
        .as_ref()
        .is_some_and(|k| !matches!(k.as_str(), "bill" | "debit_note" | "opening_credit"))
        || status.as_ref().is_some_and(|k| {
            !matches!(
                k.as_str(),
                "draft" | "approved" | "held" | "posted" | "voided"
            )
        })
    {
        return Err(PayablesError::InvalidOption);
    }
    Ok(())
}
impl Payables {
    pub async fn create_bill(
        &self,
        s: &Scope,
        company: i64,
        input: &NewDocument,
    ) -> Result<Document> {
        self.create_document(s, company, None, input).await
    }
    pub async fn create_debit_note(
        &self,
        s: &Scope,
        company: i64,
        bill: i64,
        input: &NewDocument,
    ) -> Result<Document> {
        self.create_document(s, company, Some(bill), input).await
    }
    async fn create_document(
        &self,
        s: &Scope,
        company: i64,
        bill: Option<i64>,
        input: &NewDocument,
    ) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                CREATE_DOCUMENTS,
                "finance.payables.create_document",
            )
            .await?;
        let result = self
            .create_document_on(&mut tx, s, company, bill, input)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn prepare_document(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        bill: Option<i64>,
        input: &NewDocument,
        replacing: Option<i64>,
    ) -> Result<(
        serde_json::Value,
        Vec<serde_json::Value>,
        Vec<serde_json::Value>,
        serde_json::Value,
    )> {
        let tenant = s.tenant();
        let actor = s.user();
        let imp = s.impersonator().map(|p| p.user());
        let original = if let Some(id) = bill {
            let b = store::document_record(c, tenant, company, id)
                .await?
                .ok_or(PayablesError::DocumentNotFound)?;
            if b.document_type != "bill" || b.status != "posted" {
                return Err(PayablesError::NotPostedBill);
            }
            if input.document_date < b.document_date {
                return Err(invalid(
                    "document_date",
                    "cannot be before the corrected bill's date",
                ));
            }
            Some(b)
        } else {
            None
        };
        let vendor = if original.is_none() {
            let id = input
                .vendor_id
                .ok_or_else(|| invalid("vendor_id", "is required"))?;
            let v = self.vendor_on(c, s, company, id).await?;
            Self::billable(v.active, &v.party.status)?;
            Some(v)
        } else {
            None
        };
        let party = original
            .as_ref()
            .map(|b| b.party.clone())
            .unwrap_or_else(|| {
                let v = vendor.as_ref().expect("new bill vendor");
                Party {
                    key: vendors::key(v.id),
                    code: v.code.clone(),
                    name: v.party.name.clone(),
                    ..Party::default()
                }
            });
        let code = party.code;
        let key = party.key;
        let name = party.name;
        let tax_id = party.tax_id;
        let registration = party.registration_id;
        let address = party.address;
        let number = string(
            input.party_document_number.as_deref().unwrap_or_default(),
            "party_document_number",
            100,
            bill.is_none(),
        )?;
        let description = optional(&input.description, "description", 500)?;
        let due = input.due_date.unwrap_or(
            input
                .document_date
                .checked_add(jiff::Span::new().days(i64::from(
                    vendor.as_ref().map_or(0, |v| v.payment_terms_days),
                )))
                .map_err(|_| invalid("due_date", "is out of range"))?,
        );
        if due < input.document_date {
            return Err(invalid("due_date", "cannot be before the document date"));
        }
        if bill.is_none() && c.query_opt("SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND party_key=$3 AND party_document_number=$4 AND document_type='bill' AND status<>'voided' AND id<>COALESCE($5::bigint,0) AND NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i JOIN finance_payables_opening_decisions x ON x.replaces_batch_id=i.batch_id AND x.tenant_id=$1 AND x.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=finance_payables_documents.id)",&[&tenant,&company,&key,&number,&replacing]).await?.is_some() {
            return Err(invalid("party_document_number","is already recorded for this vendor"));
        }
        let book = self.ledger.get_book_on(c, s, company).await?;
        let accounts = self.ledger.list_accounts_on(c, s, company).await?;
        let codes = store::taxes(c, tenant, company).await?;
        if input.lines.is_empty() {
            return Err(invalid("lines", "needs at least one line"));
        }
        if input.lines.len() > 500 {
            return Err(invalid("lines", "should have at most 500 item(s)"));
        }
        let mut lines = Vec::new();
        let mut taxable: Vec<(i64, Money)> = Vec::new();
        let mut net = Money::ZERO;
        for (i, line) in input.lines.iter().enumerate() {
            let validation = (|| -> Result<(String, Money, Option<String>)> {
                let description = string(&line.description, "description", 500, true)?
                    .ok_or(PayablesError::InvalidOption)?;
                account(
                    &accounts,
                    line.account_id,
                    "account_id",
                    &[AccountType::Asset, AccountType::Expense],
                )?;
                let amount = checked(line.amount, book.minor_units, "amount", true)?;
                let external =
                    optional(&line.external_order_line_id, "external_order_line_id", 100)?;
                if line.order_line_id.is_some_and(|id| id < 1) {
                    return Err(invalid("order_line_id", "must be greater than 0"));
                }
                if let Some(q) = line.quantity {
                    if q <= Decimal::ZERO {
                        return Err(invalid("quantity", "must be greater than 0"));
                    }
                    if q >= Decimal::from(10_i64.pow(16)) {
                        return Err(invalid("quantity", "must be less than 1E+16"));
                    }
                    if q.normalize().scale() > 4 {
                        return Err(invalid("quantity", "has more than 4 decimal places"));
                    }
                }
                match (line.order_line_id, line.quantity) {
                    (None, Some(_)) => {
                        return Err(invalid("order_line_id", "is needed with a quantity"));
                    }
                    (Some(_), None) => {
                        return Err(invalid("quantity", "is needed with an order line"));
                    }
                    // A bill's order lines are checked against Payables' copy of the
                    // order source below; a debit note's against its bill.
                    (Some(_), Some(_)) if bill.is_none() && !self.order_source => {
                        return Err(invalid("order_line_id", "names no installed order source"));
                    }
                    _ => {}
                }
                if let Some(id) = line.tax_code_id {
                    let code = codes.iter().find(|c| c.id == id).ok_or_else(|| {
                        invalid("tax_code_id", "is not a tax code of this company")
                    })?;
                    if !code.active {
                        return Err(invalid("tax_code_id", "is inactive"));
                    }
                }
                Ok((description, amount, external))
            })();
            let (description, amount, external) = validation.map_err(|e| match e {
                PayablesError::Invalid(mut errors) => {
                    for e in &mut errors {
                        e.line = Some(i + 1);
                    }
                    PayablesError::Invalid(errors)
                }
                e => e,
            })?;
            net = net + amount;
            if let Some(id) = line.tax_code_id {
                if let Some((_, sum)) = taxable.iter_mut().find(|(k, _)| *k == id) {
                    *sum = *sum + amount;
                } else {
                    taxable.push((id, amount));
                }
            }
            lines.push(json!({"tenant_id":tenant,"company_id":company,"line_number":i+1,"description":description,"account_id":line.account_id,"amount":amount,"tax_code_id":line.tax_code_id,"external_order_line_id":external,"order_line_id":line.order_line_id,"quantity":line.quantity}));
        }
        checked(net, book.minor_units, "total_net", true)?;
        let named: Vec<(usize, Option<i64>, Option<Decimal>)> = input
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i + 1, l.order_line_id, l.quantity))
            .collect();
        let quantity_capacity = if let Some(b) = &original {
            matching::within_billed(c, tenant, company, b, &named, None).await?
        } else {
            let ids: Vec<i64> = named.iter().filter_map(|n| n.1).collect();
            let known = orders::known(c, tenant, company, &ids).await?;
            let missing: Vec<FieldError> = named
                .iter()
                .filter(|n| n.1.is_some_and(|id| !known.contains(&id)))
                .map(|n| FieldError {
                    line: Some(n.0),
                    ..FieldError::new(
                        "order_line_id",
                        "is not an order line of this company in the order source",
                    )
                })
                .collect();
            if !missing.is_empty() {
                return Err(PayablesError::Invalid(missing));
            }
            Default::default()
        };
        let mut stated = std::collections::BTreeMap::new();
        if let Some(taxes) = &input.tax_lines {
            for tax in taxes {
                if !taxable.iter().any(|(id, _)| *id == tax.tax_code_id) {
                    return Err(invalid("tax_lines", "names a tax code no line uses"));
                }
                let amount = checked(tax.tax_amount, book.minor_units, "tax_lines", false)?;
                if stated.insert(tax.tax_code_id, amount).is_some() {
                    return Err(invalid("tax_lines", "names a tax code twice"));
                }
            }
        }
        let mut taxes = Vec::new();
        let mut tax_total = Money::ZERO;
        for (i, (id, base)) in taxable.iter().enumerate() {
            let code = codes
                .iter()
                .find(|c| c.id == *id)
                .ok_or(PayablesError::TaxCodeNotFound)?;
            let amount = match stated.get(id) {
                Some(a) => *a,
                None => {
                    let computed = (base.amount() * code.rate / Decimal::from(100))
                        .round_dp_with_strategy(
                            u32::from(book.minor_units.get()),
                            RoundingStrategy::MidpointAwayFromZero,
                        );
                    Money::new(computed, book.minor_units)
                        .map_err(|e| invalid("tax_lines", e.to_string()))?
                }
            };
            tax_total = tax_total + amount;
            taxes.push(json!({"tenant_id":tenant,"company_id":company,"line_number":i+1,"tax_code_id":id,"tax_code":code.code,"rate":code.rate,"account_id":code.account_id,"taxable_amount":base,"tax_amount":amount}));
        }
        let total = checked(net + tax_total, book.minor_units, "total", true)?;
        let mut capacity =
            note_capacity(c, tenant, company, bill, input.document_date, total).await?;
        if bill.is_some() {
            capacity["order_line_quantities"] = json!(quantity_capacity);
        }
        let kind = if bill.is_some() { "debit_note" } else { "bill" };
        let header = json!({"tenant_id":tenant,"company_id":company,"document_type":kind,"status":"draft","corrects_document_id":bill,"party_key":key,"party_code":code,"party_name":name,"party_tax_id":tax_id,"party_registration_id":registration,"party_address":address,"party_document_number":number,"document_date":input.document_date.to_string(),"due_date":due.to_string(),"currency_code":book.currency_code,"description":description,"total_net":net,"total_tax":tax_total,"total":total,"created_by_user_id":actor,"created_by_impersonator_id":imp});
        Ok((header, lines, taxes, capacity))
    }
    pub(super) async fn create_document_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        bill: Option<i64>,
        input: &NewDocument,
    ) -> Result<Document> {
        join(tx, s, company, CREATE_DOCUMENTS).await?;
        let (mut header, lines, taxes, _) = self
            .prepare_document(tx.sql(), s, company, bill, input, None)
            .await?;
        if bill.is_none() {
            let vendor = self
                .vendor_on(
                    tx.sql(),
                    s,
                    company,
                    input
                        .vendor_id
                        .ok_or_else(|| invalid("vendor_id", "is required"))?,
                )
                .await?;
            let facts = self
                .frozen_vendor_party(tx.sql(), s, &vendor.party.kind, vendor.party.id)
                .await?;
            header["party_code"] = facts["party_code"].clone();
            header["party_name"] = facts["party_name"].clone();
            header["party_tax_id"] = facts["party_tax_identifier"].clone();
            header["party_registration_id"] = facts["party_registration_identifier"].clone();
        }
        let tenant = s.tenant();
        let document = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let (row, change) = store::insert(sql, Table::Documents, header).await?;
                    let id = row["id"].as_i64().ok_or(Error::Invalid)?;
                    let mut changes = vec![change];
                    for mut row in lines {
                        row["document_id"] = json!(id);
                        changes.push(store::insert(sql, Table::Lines, row).await?.1);
                    }
                    for mut row in taxes {
                        row["document_id"] = json!(id);
                        changes.push(store::insert(sql, Table::TaxLines, row).await?.1);
                    }
                    let result = store::document(sql, tenant, company, id)
                        .await?
                        .ok_or(Error::NotFound)?;
                    Ok((result, changes))
                })
            })
            .await?;
        Ok(document)
    }
    /// Voids a draft bill and records its corrected draft in one audited
    /// transaction, so a refused replacement leaves the draft as it was and
    /// the supplier's invoice number stays free for the replacement.
    pub async fn replace_draft(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        input: &NewDocument,
    ) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write_all(
                &mut c,
                s,
                company,
                &[CREATE_DOCUMENTS, APPROVE_DOCUMENTS],
                "finance.payables.replace_draft",
            )
            .await?;
        let result = self
            .replace_draft_on(&mut tx, s, company, id, input)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn replace_draft_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        input: &NewDocument,
    ) -> Result<Document> {
        join(tx, s, company, CREATE_DOCUMENTS).await?;
        join(tx, s, company, APPROVE_DOCUMENTS).await?;
        let old = replaceable(tx.sql(), s.tenant(), company, id).await?;
        self.prepare_document(tx.sql(), s, company, None, input, Some(old.id))
            .await?;
        self.void_document_on(tx, s, company, old.id, REPLACED)
            .await?;
        self.create_document_on(tx, s, company, None, input).await
    }
    /// Approves a draft and, in the same write, posts it: one press puts a
    /// bill in the books. It stays `approved`, as [`NotPosted`] says why, when
    /// the three-way match would hold it, the approver may not post, or
    /// posting refuses it; [`Payables::post_document`] is then the next step.
    pub async fn approve_document(&self, s: &Scope, company: i64, id: i64) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                APPROVE_DOCUMENTS,
                "finance.payables.approve_document",
            )
            .await?;
        let (result, _) = self.approve_document_on(&mut tx, s, company, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn approve_document_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<(Document, Option<NotPosted>)> {
        join(tx, s, company, APPROVE_DOCUMENTS).await?;
        let d = store::document(tx.sql(), s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        if d.status != "draft" {
            return Err(PayablesError::NotDraft);
        }
        self.ledger
            .check_approval(tx, s, company, &approval(&d))
            .await?;
        let approved = advance(tx, s, company, id, "approved", None, None).await?;
        let d = store::document_record(tx.sql(), s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        // Decided on reads before anything posts, so a refusal leaves the
        // approval standing instead of rolling it back.
        match self.posting_after_approval(tx.sql(), s, company, d).await? {
            Ok((prepared, _)) => Ok((
                self.post_prepared(tx, s, company, id, prepared).await?,
                None,
            )),
            Err(why) => Ok((approved, Some(why))),
        }
    }
    /// What approving `d` does next: the posting and the ledger's preview of
    /// its entry, or why the document stays approved. A hold is never
    /// recorded here; the separate Post does that, as it always has.
    pub(super) async fn posting_after_approval(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        d: Document,
    ) -> Result<std::result::Result<(PreparedPosting, serde_json::Value), NotPosted>> {
        if let Err(e) = crate::agent_operations::authorize_on(c, s, company, POST_DOCUMENTS).await {
            return not_posted(e);
        }
        let prepared = match self.prepare_posting(c, s, company, d).await {
            Ok(PreparedPosting::Held(..)) => return Ok(Err(NotPosted::Match)),
            Ok(prepared) => prepared,
            Err(e) => return not_posted(e),
        };
        let PreparedPosting::Post { entry, options, .. } = &prepared else {
            return Ok(Err(NotPosted::Match));
        };
        match self
            .ledger
            .preview_post_entry_on(c, s, company, entry, options)
            .await
        {
            Ok(preview) => Ok(Ok((prepared, preview))),
            Err(e) => not_posted(e.into()),
        }
    }
    pub async fn void_document(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        reason: &str,
    ) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                APPROVE_DOCUMENTS,
                "finance.payables.void_document",
            )
            .await?;
        let result = self
            .void_document_on(&mut tx, s, company, id, reason)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn void_document_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        reason: &str,
    ) -> Result<Document> {
        join(tx, s, company, APPROVE_DOCUMENTS).await?;
        let d = store::document(tx.sql(), s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        if !matches!(d.status.as_str(), "draft" | "approved" | "held") {
            return Err(PayablesError::NotVoidable);
        }
        let reason = string(reason, "void_reason", 500, true)?;
        let result = advance(tx, s, company, id, "voided", reason, None).await?;
        Ok(result)
    }
    pub async fn post_document(&self, s: &Scope, company: i64, id: i64) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                POST_DOCUMENTS,
                "finance.payables.post_document",
            )
            .await?;
        let result = self.post_document_on(&mut tx, s, company, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn prepare_post_document(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<PreparedPosting> {
        let d = store::document_record(c, s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        if !matches!(d.status.as_str(), "approved" | "held") {
            return Err(PayablesError::NotApproved);
        }
        self.prepare_posting(c, s, company, d).await
    }
    /// The match and the entry for `d`, whatever its status: the caller has
    /// decided it may be posted.
    async fn prepare_posting(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        d: Document,
    ) -> Result<PreparedPosting> {
        let id = d.id;
        let mut capacity = note_capacity(
            c,
            s.tenant(),
            company,
            d.corrects_document_id,
            d.document_date,
            d.total,
        )
        .await?;
        let mut matched = None;
        if d.document_type == "bill" && d.lines.iter().any(|l| l.order_line_id.is_some()) {
            let settings = store::settings(c, s.tenant(), company)
                .await?
                .ok_or(PayablesError::PayablesNotConfigured)?
                .1;
            let book = self.ledger.get_book_on(c, s, company).await?;
            let lines = matching::evaluate(
                c,
                s.tenant(),
                company,
                &d,
                &settings,
                &book.currency_code,
                self.order_source,
            )
            .await?;
            if !lines.iter().all(|l| l.exceptions.is_empty())
                && !matching::covered(&approved_lines(&d), &lines)
            {
                return Ok(PreparedPosting::Held(d, lines));
            }
            matched = Some(lines);
        }
        let payable = if let Some(bill) = d.corrects_document_id {
            let b = store::document_record(c, s.tenant(), company, bill)
                .await?
                .ok_or(PayablesError::DocumentNotFound)?;
            let named: Vec<_> = d
                .lines
                .iter()
                .map(|l| (l.line_number as usize, l.order_line_id, l.quantity))
                .collect();
            let quantities = matching::within_billed(c, s.tenant(), company, &b, &named, Some(id))
                .await
                .map_err(|e| match e {
                    PayablesError::Invalid(_) => PayablesError::ExceedsBilledQuantity,
                    e => e,
                })?;
            capacity["order_line_quantities"] = json!(quantities);
            b.payable_account_id.ok_or(PayablesError::NotPostedBill)?
        } else {
            let default = store::settings(c, s.tenant(), company)
                .await?
                .ok_or(PayablesError::PayablesNotConfigured)?
                .1
                .payable_account_id;
            if let Some(vendor) = d.vendor_id {
                self.vendor_on(c, s, company, vendor)
                    .await?
                    .payable_account_id
                    .unwrap_or(default)
            } else {
                default
            }
        };
        let debit = d.document_type == "bill";
        let side = |id, amount| {
            if debit {
                NewLine::debit(id, amount)
            } else {
                NewLine::credit(id, amount)
            }
        };
        let mut lines: Vec<NewLine> = d
            .lines
            .iter()
            .map(|l| side(l.account_id, l.amount).described(&l.description))
            .collect();
        lines.extend(
            d.tax_lines
                .iter()
                .filter(|t| !t.tax_amount.is_zero())
                .map(|t| side(t.account_id, t.tax_amount).described(&t.tax_code)),
        );
        lines.push(if debit {
            NewLine::credit(payable, d.total)
        } else {
            NewLine::debit(payable, d.total)
        });
        let description = if debit {
            format!(
                "Bill {} from {}",
                d.party_document_number.as_deref().unwrap_or_default(),
                d.party.name
            )
        } else {
            format!("Debit note to {}", d.party.name)
        };
        let new = NewEntry {
            entry_date: d.document_date,
            description: description.chars().take(500).collect(),
            reference: d.party_document_number.clone(),
            lines,
        };
        let options = PostOptions {
            owner: Some(EntryOwner {
                module: MODULE.into(),
                record: format!("document:{id}"),
            }),
            settlement_account_ids: if debit { vec![] } else { vec![payable] },
        };
        Ok(PreparedPosting::Post {
            document: d,
            payable,
            entry: new,
            options,
            matched,
            capacity,
        })
    }
    pub(super) async fn post_document_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Document> {
        join(tx, s, company, POST_DOCUMENTS).await?;
        let prepared = self.prepare_post_document(tx.sql(), s, company, id).await?;
        self.post_prepared(tx, s, company, id, prepared).await
    }
    /// Records the hold, or posts the owned entry and marks the document posted.
    async fn post_prepared(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        id: i64,
        prepared: PreparedPosting,
    ) -> Result<Document> {
        let actor = (s.user(), s.impersonator().map(|p| p.user()));
        let (d, payable, new, options, matched) = match prepared {
            PreparedPosting::Held(d, lines) => {
                return self.hold(tx, s, company, d, actor, lines).await;
            }
            PreparedPosting::Post {
                document,
                payable,
                entry,
                options,
                matched,
                ..
            } => (document, payable, entry, options, matched),
        };
        let entry = self
            .ledger
            .post_entry_in(tx, s, company, &new, &options)
            .await?;
        if let Some(lines) = matched {
            let (tenant, document) = (s.tenant(), d.clone());
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let changes = store::record_match(
                        sql,
                        &document,
                        (tenant, company),
                        actor,
                        "posted",
                        None,
                        &lines,
                    )
                    .await?;
                    Ok(((), changes))
                })
            })
            .await?;
        }
        let result = advance(
            tx,
            s,
            company,
            id,
            "posted",
            None,
            Some((payable, entry.id, entry.entry_number)),
        )
        .await?;
        Ok(result)
    }
    /// Records the hold and keeps (or makes) the bill `held`; nothing posts.
    async fn hold(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        d: Document,
        actor: (i64, Option<i64>),
        lines: Vec<MatchLine>,
    ) -> Result<Document> {
        let (tenant, id, held) = (s.tenant(), d.id, d.status == "held");
        let document = d.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let changes = store::record_match(
                    sql,
                    &document,
                    (tenant, company),
                    actor,
                    "held",
                    None,
                    &lines,
                )
                .await?;
                Ok(((), changes))
            })
        })
        .await?;
        let result = if held {
            store::document(tx.sql(), tenant, company, id)
                .await?
                .ok_or(PayablesError::DocumentNotFound)?
        } else {
            advance(tx, s, company, id, "held", None, None).await?
        };
        Ok(result)
    }
    /// Approves a held bill's match exceptions with a reason and the hold the
    /// approver saw, so the next posting may post over them. The ledger's
    /// separation of duties applies (the bill's recorder and whoever tried to
    /// post it into the hold are its preparers), and the approval is always
    /// listed in its approval exceptions as an override.
    pub async fn approve_match_exception(
        &self,
        s: &Scope,
        company: i64,
        id: i64,
        approval: &MatchApproval,
    ) -> Result<Document> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                APPROVE_MATCH,
                "finance.payables.approve_match_exception",
            )
            .await?;
        let d = store::document(tx.sql(), s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        if d.status != "held" {
            return Err(PayablesError::NotHeld);
        }
        let held = match d.match_events.last() {
            Some(e) if e.event == "held" => e.clone(),
            _ => return Err(PayablesError::AlreadyApproved),
        };
        if approval.held_event_id != held.id {
            return Err(PayablesError::StaleHold);
        }
        let reason =
            string(&approval.reason, "reason", 500, true)?.ok_or(PayablesError::InvalidOption)?;
        self.ledger
            .check_approval(
                &mut tx,
                s,
                company,
                &Approval {
                    source: MODULE.into(),
                    action: "match_override".into(),
                    subject_type: "bill".into(),
                    subject_id: id,
                    subject: d.label(),
                    prepared_by: vec![
                        Preparer {
                            user_id: d.created_by_user_id,
                            impersonator_id: d.created_by_impersonator_id,
                        },
                        Preparer {
                            user_id: held.actor_user_id,
                            impersonator_id: held.actor_impersonator_id,
                        },
                    ],
                    override_reason: Some(reason.clone()),
                },
            )
            .await?;
        let (tenant, actor) = (s.tenant(), (s.user(), s.impersonator().map(|p| p.user())));
        let document = d.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let changes = store::record_match(
                    sql,
                    &document,
                    (tenant, company),
                    actor,
                    "exception_approved",
                    Some((&reason, held.id)),
                    &[],
                )
                .await?;
                Ok(((), changes))
            })
        })
        .await?;
        let result = store::document(tx.sql(), tenant, company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)?;
        tx.commit().await?;
        Ok(result)
    }
    /// The quantity billed against each of the order lines, by ID: posted
    /// bill lines naming it less posted debit note lines, in the company. An
    /// order line nothing posted names is absent. This is this module's own
    /// record, the figure the match takes off what was received.
    pub async fn billed_quantities(
        &self,
        s: &Scope,
        company: i64,
        order_line_ids: &[i64],
    ) -> Result<std::collections::BTreeMap<i64, Decimal>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        if order_line_ids.iter().any(|id| *id < 1) || order_line_ids.len() > 500 {
            return Err(PayablesError::InvalidOption);
        }
        matching::billed_quantities(&**c, s.tenant(), company, order_line_ids, None).await
    }
    pub async fn get_document(&self, s: &Scope, company: i64, id: i64) -> Result<Document> {
        let c = self.db.get().await.map_err(Error::from)?;
        self.get_document_on(&**c, s, company, id).await
    }
    /// Read and authorize on a sibling's transaction, without taking another
    /// pool connection while the caller holds the company write lock.
    pub async fn get_document_on(
        &self,
        c: &dyn bilibi_agent_api::OperationClient,
        s: &Scope,
        company: i64,
        id: i64,
    ) -> Result<Document> {
        crate::agent_operations::authorize_on(c, s, company, VIEW).await?;
        store::document(c, s.tenant(), company, id)
            .await?
            .ok_or(PayablesError::DocumentNotFound)
    }
    pub async fn list_documents(
        &self,
        s: &Scope,
        company: i64,
        q: &DocumentQuery,
    ) -> Result<Vec<Document>> {
        let c = self.db.get().await.map_err(Error::from)?;
        authorize(&**c, s, company, VIEW).await?;
        filters(&q.document_type, &q.status)?;
        if !(1..=500).contains(&q.limit)
            || q.after.is_some_and(|(_, id)| id < 1)
            || matches!((q.from,q.to),(Some(f),Some(t)) if f>t)
        {
            return Err(PayablesError::InvalidOption);
        }
        let from = q.from.map(|d| d.to_string());
        let to = q.to.map(|d| d.to_string());
        let date = q.after.map(|(d, _)| d.to_string());
        let id = q.after.map(|(_, id)| id);
        let ids=c.query("SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND ($3::text IS NULL OR document_type=$3) AND ($4::text IS NULL OR status=$4) AND ($5::text IS NULL OR party_key=$5) AND ($6::text IS NULL OR document_date>=$6::date) AND ($7::text IS NULL OR document_date<=$7::date) AND ($8::text IS NULL OR (document_date,id)>($8::date,$9::bigint)) ORDER BY document_date,id LIMIT $10",&[&s.tenant(),&company,&q.document_type,&q.status,&q.party_key,&from,&to,&date,&id,&q.limit]).await?;
        let mut docs = Vec::new();
        for id in ids {
            docs.push(
                store::document(&**c, s.tenant(), company, id.get(0))
                    .await?
                    .ok_or(PayablesError::DocumentNotFound)?,
            );
        }
        Ok(docs)
    }
}
pub(super) fn approval(d: &Document) -> Approval {
    Approval {
        source: MODULE.into(),
        action: "document_approval".into(),
        subject_type: d.document_type.clone(),
        subject_id: d.id,
        subject: d.label(),
        prepared_by: vec![Preparer {
            user_id: d.created_by_user_id,
            impersonator_id: d.created_by_impersonator_id,
        }],
        override_reason: None,
    }
}
async fn advance(
    tx: &mut MutationTransaction<'_>,
    s: &Scope,
    company: i64,
    id: i64,
    status: &'static str,
    reason: Option<String>,
    posting: Option<(i64, i64, i64)>,
) -> Result<Document> {
    let tenant = s.tenant();
    let actor = s.user();
    let imp = s.impersonator().map(|p| p.user());
    Ok(tx.mutate(move |sql|Box::pin(async move {
        let before=store::raw(sql,Table::Documents,tenant,company,id).await?;
        match status {
            "approved"=> {sql.execute("UPDATE finance_payables_documents SET status='approved',approved_by_user_id=$4,approved_by_impersonator_id=$5 WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id,&actor,&imp]).await?;},
            "held"=> {sql.execute("UPDATE finance_payables_documents SET status='held' WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id]).await?;},
            "voided"=> {sql.execute("UPDATE finance_payables_documents SET status='voided',voided_by_user_id=$4,voided_by_impersonator_id=$5,void_reason=$6 WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id,&actor,&imp,&reason]).await?;},
            "posted"=> {let (account,entry,number)=posting.ok_or(Error::Invalid)?;sql.execute("UPDATE finance_payables_documents SET status='posted',posted_by_user_id=$4,posted_by_impersonator_id=$5,payable_account_id=$6,entry_id=$7,entry_number=$8 WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id,&actor,&imp,&account,&entry,&number]).await?;},
            _=>return Err(Error::Invalid),
        }
        let after=store::raw(sql,Table::Documents,tenant,company,id).await?;
        let d=store::document(sql,tenant,company,id).await?.ok_or(Error::NotFound)?;
        Ok((d,vec![store::update_change(Table::Documents,id,before,after)]))
    })).await?)
}

/// The lines of the hold the bill's latest approval covers, with the figures
/// the approver saw, or none when the latest event is not an approval.
fn approved_lines(d: &Document) -> Vec<MatchLine> {
    match d.match_events.last() {
        Some(e) if e.event == "exception_approved" => d
            .match_events
            .iter()
            .find(|h| Some(h.id) == e.held_event_id)
            .map(|h| h.lines.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
