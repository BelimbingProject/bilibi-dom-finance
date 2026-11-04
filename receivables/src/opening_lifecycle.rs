//! Module-owned conversion lifecycle. Staging never creates settleable records;
//! final reconciliation and append-only replacement share the company lock.
use crate::*;
use bilibi_agent_api::OperationClient;
use bilibi_database::Change;
use bilibi_finance_ledger::{Approval, OpeningContext, Preparer};
use serde_json::{json, Value};

impl Receivables {
    pub(super) async fn authorize_opening_stage(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
    ) -> Result<()> {
        self.join(c, s, company, ISSUE).await?;
        Ok(())
    }
    pub(super) async fn prepare_opening_stage(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        self.authorize_opening_stage(c, s, company).await?;
        let reference = input["source_reference"].as_str().unwrap_or("").trim();
        if reference.is_empty() || reference.chars().count() > 100 || reference.contains('\0') {
            return Err(invalid(
                "source_reference",
                "must contain 1 to 100 characters without null characters",
            ));
        }
        let part = input["part"]
            .as_i64()
            .filter(|n| (1..=200).contains(n))
            .ok_or_else(|| invalid("part", "must be 1 to 200"))?;
        let count = input["part_count"]
            .as_i64()
            .filter(|n| (part..=200).contains(n))
            .ok_or_else(|| invalid("part_count", "must include this batch and be at most 200"))?;
        let csv = input["csv"]
            .as_str()
            .filter(|csv| csv.len() <= 131072 && !csv.contains('\0'))
            .ok_or_else(|| invalid("csv", "must fit within 128 KiB without null characters"))?;
        let rows = crate::opening_csv::parse(csv.as_bytes(), ",")
            .map_err(|e| invalid("csv", format!("cannot read CSV: {e:?}")))?;
        if !(2..=501).contains(&rows.len())
            || rows[0].iter().map(String::as_str).collect::<Vec<_>>() != crate::opening::HEADER
        {
            return Err(invalid(
                "csv",
                "requires the module's CSV header and 1 to 500 rows per batch",
            ));
        }
        let book = self.ledger.get_book_on(c, s, company).await?;
        if input["currency_code"] != book.currency_code {
            return Err(invalid("currency_code", "must match the book's currency"));
        }
        self.validate_opening_classifications(c, s, company, input)
            .await?;
        let payload = stage_payload(input)?;
        if payload.to_string().len() > 262144 {
            return Err(invalid(
                "csv",
                "the encoded source batch is too large; split it into smaller batches",
            ));
        }
        let stages=c.query("SELECT part,payload FROM finance_receivables_opening_stages WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3 ORDER BY part",&[&s.tenant(),&company,&reference]).await?;
        let mut retry = false;
        for row in &stages {
            let existing: Value = row.get(1);
            if stage_manifest(&existing) != stage_manifest(&payload)
                || (row.get::<_, i32>(0) as i64 == part && existing != payload)
            {
                return Err(invalid(
                    "source_reference",
                    "already identifies different source bytes or register instructions; use a new source reference",
                ));
            }
            retry |= row.get::<_, i32>(0) as i64 == part;
        }
        if !retry && c.query_opt("SELECT id FROM finance_receivables_opening_batches WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3",&[&s.tenant(),&company,&reference]).await?.is_some() {return Err(invalid("source_reference","has already been reconciled; use a new source reference"));}
        Ok(
            json!({"state":"previewed","source_reference":reference,"part":part,"part_count":count,"received_parts":stages.iter().map(|r|r.get::<_,i32>(0)).collect::<Vec<_>>(),"rows":rows.len()-1,"already_staged":retry,"posts":false,"settleable":false}),
        )
    }
    /// Retain one immutable source batch. An identical retry is a no-op.
    pub async fn stage_opening_import(
        &self,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        let mut c = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .write(
                &mut c,
                s,
                company,
                ISSUE,
                "finance.receivables.opening.stage",
            )
            .await?;
        let result = self.stage_opening_on(&mut tx, s, company, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn stage_opening_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        let mut review = self
            .prepare_opening_stage(tx.sql(), s, company, input)
            .await?;
        if review["already_staged"] != true {
            let tenant = s.tenant();
            let actor = s.user();
            let imp = s.impersonator().map(|p| p.user());
            let reference = input["source_reference"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_owned();
            let part = input["part"].as_i64().unwrap_or(0) as i32;
            let payload = stage_payload(input)?;
            tx.mutate(move |sql|Box::pin(async move {
                let row=sql.query_one("INSERT INTO finance_receivables_opening_stages(tenant_id,company_id,source_reference,part,payload,recorded_by_user_id,recorded_by_impersonator_id) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING to_jsonb(finance_receivables_opening_stages)",&[&tenant,&company,&reference,&part,&payload,&actor,&imp]).await?;
                let new:Value=row.get(0);let id=new["id"].as_i64().ok_or(Error::Invalid)?;
                Ok(((),vec![Change{kind:"finance.receivables.opening_stage".into(),id,old:Value::Null,new}]))
            })).await?;
        }
        review["state"] = json!("committed");
        Ok(review)
    }
    pub(super) async fn resolve_opening(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        self.authorize_opening_stage(c, s, company).await?;
        if input["staged"] != true {
            return Ok(input.clone());
        }
        let reference = input["source_reference"].as_str().unwrap_or("").trim();
        let stages=c.query("SELECT part,payload FROM finance_receivables_opening_stages WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3 ORDER BY part",&[&s.tenant(),&company,&reference]).await?;
        let first: Value = stages
            .first()
            .ok_or_else(|| invalid("source_reference", "has no staged batches"))?
            .get(1);
        let count = first["part_count"].as_i64().unwrap_or(0);
        if stages.len() as i64 != count
            || stages
                .iter()
                .enumerate()
                .any(|(i, r)| r.get::<_, i32>(0) != i as i32 + 1)
        {
            return Err(invalid(
                "source_reference",
                "has missing batches; stage every batch before reconciliation",
            ));
        }
        for key in ["currency_code", "series_id", "replaces_batch_id", "reason"] {
            if !input[key].is_null() && input[key] != first[key] {
                return Err(invalid(
                    "source_reference",
                    "does not match the staged register instructions",
                ));
            }
        }
        if !input["opening_account_classifications"].is_null()
            && crate::opening::opening_classifications(input)?
                != crate::opening::opening_classifications(&first)?
        {
            return Err(invalid(
                "source_reference",
                "does not match the staged register instructions",
            ));
        }
        if input["csv"].as_str().is_some_and(|v| !v.is_empty()) {
            return Err(invalid(
                "csv",
                "leave empty when reconciling staged batches",
            ));
        }
        let mut combined = String::new();
        for (index, stage) in stages.iter().enumerate() {
            let payload: Value = stage.get(1);
            let rows =
                crate::opening_csv::parse(payload["csv"].as_str().unwrap_or("").as_bytes(), ",")
                    .map_err(|_| invalid("csv", "cannot read retained batch"))?;
            for row in rows.iter().skip(usize::from(index > 0)) {
                combined.push_str(
                    &row.iter()
                        .map(|cell| format!("\"{}\"", cell.replace('"', "\"\"")))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                combined.push('\n');
            }
        }
        let mut resolved = first;
        resolved["csv"] = json!(combined);
        resolved["staged"] = json!(true);
        resolved["override_reason"] = input["override_reason"].clone();
        Ok(resolved)
    }
    pub(super) async fn opening_approval(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        context: &OpeningContext,
        input: &Value,
    ) -> Result<Approval> {
        let mut approval =
            context.approval(MODULE, input["override_reason"].as_str().map(str::to_owned));
        if input["staged"] == true {
            let reference = input["source_reference"].as_str().unwrap_or("").trim();
            for row in c.query("SELECT DISTINCT recorded_by_user_id,recorded_by_impersonator_id FROM finance_receivables_opening_stages WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3",&[&s.tenant(),&company,&reference]).await? {
                approval.prepared_by.push(Preparer{user_id:row.get(0),impersonator_id:row.get(1)});
            }
        }
        Ok(approval)
    }
    pub(super) async fn opening_replacement(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Option<Value>> {
        let current=c.query_opt("SELECT to_jsonb(b) FROM finance_receivables_opening_batches b WHERE tenant_id=$1 AND company_id=$2 ORDER BY id DESC LIMIT 1",&[&s.tenant(),&company]).await?.map(|r|r.get::<_,Value>(0));
        match (current, input["replaces_batch_id"].as_i64()) {
            (None, None) => Ok(None),
            (Some(batch), Some(id)) if batch["id"] == id => {
                if input["staged"] != true {
                    return Err(invalid(
                        "source_reference",
                        "stage the correction for independent approval before reconciliation",
                    ));
                }
                let reason = input["reason"].as_str().unwrap_or("").trim();
                if reason.is_empty() || reason.chars().count() > 500 || reason.contains('\0') {
                    return Err(invalid(
                        "reason",
                        "must contain 1 to 500 characters without null characters",
                    ));
                }
                self.opening_dependents(c, s, company, id).await?;
                Ok(Some(batch))
            }
            _ => Err(invalid(
                "replaces_batch_id",
                "must name the current register when correcting an import; retain the prior source and use a new source reference",
            )),
        }
    }
    pub(super) async fn record_opening_decision(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        batch: &Value,
        input: &Value,
        review: &Value,
    ) -> Result<()> {
        let tenant = s.tenant();
        let actor = s.user();
        let imp = s.impersonator().map(|p| p.user());
        let id = batch["id"].as_i64().ok_or(Error::Invalid)?;
        let replaces = input["replaces_batch_id"].as_i64();
        let reason = input["reason"].as_str().map(str::trim).map(str::to_owned);
        let effective = batch["conversion_date"].as_str().unwrap_or("").to_owned();
        let mut review = review.clone();
        if let Some(rows) = review.as_object_mut().and_then(|r| r.remove("rows")) {
            review["row_count"] = json!(rows.as_array().map_or(0, Vec::len));
        }
        let approval = review["approval"].clone();
        tx.mutate(move |sql|Box::pin(async move {
            let row=sql.query_one("INSERT INTO finance_receivables_opening_decisions(tenant_id,company_id,batch_id,replaces_batch_id,reason,review,approval,recorded_by_user_id,recorded_by_impersonator_id,effective_date) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::text::date) RETURNING to_jsonb(finance_receivables_opening_decisions)",&[&tenant,&company,&id,&replaces,&reason,&review,&approval,&actor,&imp,&effective]).await?;
            let new:Value=row.get(0);let id=new["id"].as_i64().ok_or(Error::Invalid)?;
            Ok(((),vec![Change{kind:"finance.receivables.opening_decision".into(),id,old:Value::Null,new}]))
        })).await?;
        Ok(())
    }
}
fn stage_payload(input: &Value) -> Result<Value> {
    let classifications = crate::opening::opening_classifications(input)?;
    let mut value = input.clone();
    if let Some(object) = value.as_object_mut() {
        object.retain(|key, _| {
            [
                "source_reference",
                "currency_code",
                "csv",
                "series_id",
                "opening_account_classifications",
                "replaces_batch_id",
                "reason",
                "part",
                "part_count",
            ]
            .contains(&key.as_str())
        });
    }
    value["source_reference"] = json!(input["source_reference"].as_str().unwrap_or("").trim());
    value["opening_account_classifications"] = json!(classifications);
    Ok(value)
}
fn stage_manifest(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("csv");
        object.remove("part");
    }
    value
}

impl Receivables {
    pub(super) async fn opening_dependents(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        batch: i64,
    ) -> Result<()> {
        if c.query_opt("SELECT e.id FROM finance_receivables_settlements e JOIN finance_receivables_opening_items i ON i.tenant_id=$1 AND i.company_id=$2 AND (i.document_id=e.credit_document_id OR i.document_id=e.target_document_id) WHERE e.tenant_id=$1 AND e.company_id=$2 AND i.batch_id=$3 AND e.kind<>'reverse' AND NOT EXISTS(SELECT 1 FROM finance_receivables_settlements x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.reverses_settlement_id=e.id) LIMIT 1",&[&s.tenant(),&company,&batch]).await?.is_some() {return Err(invalid("replaces_batch_id","reverse dependent credit applications and refunds through their settlement operation before replacing the register"));}
        if c.query_opt("SELECT i.id FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.batch_id=$3 AND (
 EXISTS(SELECT 1 FROM finance_receivables_allocations a JOIN finance_receivables_payments p ON p.id=a.payment_id AND p.tenant_id=$1 AND p.company_id=$2 WHERE a.tenant_id=$1 AND a.company_id=$2 AND a.document_id=i.document_id AND NOT EXISTS(SELECT 1 FROM finance_receivables_payment_reversals r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.payment_id=p.id))
 OR EXISTS(SELECT 1 FROM finance_receivables_documents d WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.credited_document_id=i.document_id AND d.status='issued')
 OR EXISTS(SELECT 1 FROM finance_receivables_write_offs w WHERE w.tenant_id=$1 AND w.company_id=$2 AND w.document_id=i.document_id AND NOT EXISTS(SELECT 1 FROM finance_receivables_write_off_events e WHERE e.tenant_id=$1 AND e.company_id=$2 AND e.write_off_id=w.id AND e.event IN ('rejected','withdrawn','reversed')))) LIMIT 1",&[&s.tenant(),&company,&batch]).await?.is_some() {return Err(invalid("replaces_batch_id","reverse dependent settlements, write-offs, depreciation and disposals, and cancel pending requests or notes before correcting the register"));}
        Ok(())
    }
}
