//! Opening supplier residuals share Payables' settlement records without
//! recording purchases, tax or a second control-account posting.
use crate::*;
use bilibi_database::Change;
use bilibi_finance_ledger::{AccountType, EntryOwner, OpeningContext, PartyLinks};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const HEADER: &[&str] = &[
    "source_id",
    "vendor_code",
    "vendor_name",
    "document_reference",
    "document_date",
    "due_date",
    "original_total",
    "outstanding",
    "control_account_code",
    "party_kind",
    "item_kind",
    "source_kind",
    "source_system",
    "currency_code",
    "source_tax_facts",
];
pub(super) struct Prepared {
    pub review: Value,
    context: OpeningContext,
    rows: Vec<Value>,
    balances: BTreeMap<i64, Money>,
    replaced_accounts: BTreeSet<i64>,
    existing: Option<Value>,
}

pub(super) fn opening_classifications(input: &Value) -> Result<BTreeMap<i64, String>> {
    let value = &input["opening_account_classifications"];
    let entries: Vec<(i64, &Value)> = if let Some(values) = value.as_object() {
        values
            .iter()
            .map(|(id, value)| {
                id.parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .map(|id| (id, value))
                    .ok_or_else(|| {
                        invalid(
                            "opening_account_classifications",
                            "account IDs must be positive integers",
                        )
                    })
            })
            .collect::<Result<_>>()?
    } else if let Some(values) = value.as_array() {
        values
            .iter()
            .map(|value| {
                value["account_id"]
                    .as_i64()
                    .filter(|id| *id > 0)
                    .map(|id| (id, &value["classification"]))
                    .ok_or_else(|| {
                        invalid(
                            "opening_account_classifications",
                            "account IDs must be positive integers",
                        )
                    })
            })
            .collect::<Result<_>>()?
    } else {
        return Err(invalid(
            "opening_account_classifications",
            "must classify opening accounts by account ID",
        ));
    };
    if entries.len() > 10_000 {
        return Err(invalid(
            "opening_account_classifications",
            "must contain at most 10000 account classifications",
        ));
    }
    let mut classifications = BTreeMap::new();
    for (id, value) in entries {
        let classification = value.as_str().unwrap_or("");
        if !["receivables_control", "payables_control", "not_a_control"].contains(&classification) {
            return Err(invalid(
                "opening_account_classifications",
                "each account must be a receivables control, payables control, or not a control",
            ));
        }
        if classifications
            .insert(id, classification.to_owned())
            .is_some()
        {
            return Err(invalid(
                "opening_account_classifications",
                format!("account ID {id} is classified more than once"),
            ));
        }
    }
    Ok(classifications)
}

// Vendor controls name vendor_id on ordinary bills; an import edits the CSV.
fn vendor_refusal(error: PayablesError, source: &str) -> PayablesError {
    let fields = match error {
        PayablesError::Invalid(fields) | PayablesError::Ledger(LedgerError::Invalid(fields)) => {
            fields
        }
        other => return other,
    };
    PayablesError::Invalid(
        fields
            .into_iter()
            .map(|field| FieldError::new("csv", format!("{source}: vendor_code {}", field.message)))
            .collect(),
    )
}

impl Payables {
    pub(super) async fn validate_opening_classifications(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<()> {
        let classifications = opening_classifications(input)?;
        let opening = self.ledger.opening_context_on(c, s, company).await?;
        let book = self.ledger.get_book_on(c, s, company).await?;
        let chart = self.ledger.list_accounts_on(c, s, company).await?;
        let mut expected = BTreeSet::new();
        for account in chart {
            let balance = opening
                .balances
                .get(&account.id)
                .copied()
                .unwrap_or(Money::ZERO);
            if (balance == Money::ZERO && !classifications.contains_key(&account.id))
                || account.is_group
                || ![AccountType::Asset, AccountType::Liability].contains(&account.account_type)
            {
                continue;
            }
            expected.insert(account.id);
            let classification = classifications.get(&account.id).ok_or_else(|| {
                invalid(
                    "opening_account_classifications",
                    format!(
                        "classify {} ({}) as a receivables control, payables control, or not a control",
                        account.code,
                        balance.display(book.minor_units)
                    ),
                )
            })?;
            if (classification == "receivables_control"
                && account.account_type != AccountType::Asset)
                || (classification == "payables_control"
                    && account.account_type != AccountType::Liability)
            {
                return Err(invalid(
                    "opening_account_classifications",
                    format!(
                        "{} has an incompatible control classification",
                        account.code
                    ),
                ));
            }
        }
        if let Some(id) = classifications.keys().find(|id| !expected.contains(id)) {
            return Err(invalid(
                "opening_account_classifications",
                format!("account ID {id} is not an eligible asset or liability account"),
            ));
        }
        Ok(())
    }

    pub(super) async fn opening_context(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        crate::agent_operations::authorize_on(c, s, company, VIEW).await?;
        let opening = self.ledger.opening_context_on(c, s, company).await?;
        let opening_accounts = self
            .ledger
            .list_accounts_on(c, s, company)
            .await?
            .into_iter()
            .filter(|account| {
                [AccountType::Asset, AccountType::Liability].contains(&account.account_type)
                    && !account.is_group

            })
            .map(|account| json!({"id":account.id,"code":account.code,"name":account.name,"account_type":account.account_type,"opening_balance":opening.balances.get(&account.id).copied().unwrap_or(Money::ZERO)}))
            .collect::<Vec<_>>();
        let after = input["items_after"].as_i64().unwrap_or(0);
        let limit = input["limit"].as_i64().unwrap_or(200).clamp(1, 500);
        let items: Vec<Value> = c.query("SELECT to_jsonb(i) FROM finance_payables_opening_items i WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4", &[&s.tenant(), &company, &after, &(limit + 1)]).await?.iter().map(|row| row.get(0)).collect();
        let more = items.len() as i64 > limit;
        let items: Vec<Value> = items.into_iter().take(limit as usize).collect();
        let next = if more {
            items.last().map(|i| i["id"].clone())
        } else {
            None
        };
        let total: i64 = c.query_opt("SELECT count(*) FROM finance_payables_opening_items WHERE tenant_id=$1 AND company_id=$2", &[&s.tenant(), &company]).await?.ok_or(Error::Internal)?.get(0);
        let stages = c.query("SELECT to_jsonb(s)-'payload'||jsonb_build_object('part_count',payload->'part_count','csv_bytes',octet_length(payload->>'csv')) FROM finance_payables_opening_stages s WHERE tenant_id=$1 AND company_id=$2 ORDER BY id", &[&s.tenant(), &company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>();
        let stage_sources = c.query("SELECT jsonb_build_object('source_reference',source_reference,'opening_account_classifications',payload->'opening_account_classifications') FROM (SELECT DISTINCT ON (source_reference) source_reference,payload,id FROM finance_payables_opening_stages WHERE tenant_id=$1 AND company_id=$2 ORDER BY source_reference,id DESC) source ORDER BY id", &[&s.tenant(), &company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>();
        Ok(
            json!({"opening":opening,"book":self.ledger.get_book_on(c,s,company).await?,"opening_accounts":opening_accounts,"batch":c.query_opt("SELECT to_jsonb(b) || jsonb_build_object('state','reconciled') FROM finance_payables_opening_batches b WHERE tenant_id=$1 AND company_id=$2 ORDER BY id DESC LIMIT 1",&[&s.tenant(),&company]).await?.map(|r|r.get::<_,Value>(0)),"stages":stages,"stage_sources":stage_sources,"decisions":c.query("SELECT to_jsonb(d) FROM finance_payables_opening_decisions d WHERE tenant_id=$1 AND company_id=$2 ORDER BY id",&[&s.tenant(),&company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>(),"header":HEADER,"items":items,"item_count":total,"items_next":next}),
        )
    }
    pub(super) async fn prepare_opening(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Prepared> {
        crate::agent_operations::authorize_on(c, s, company, POST_DOCUMENTS).await?;
        let company_row=c.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT",&[&s.tenant(),&company]).await?.ok_or(PayablesError::CompanyNotFound)?;
        if company_row.get::<_, String>(0) == "archived" {
            return Err(PayablesError::CompanyArchived);
        }
        let context = self.ledger.opening_context_on(c, s, company).await?;
        let book = self.ledger.get_book_on(c, s, company).await?;
        let reference = input["source_reference"].as_str().unwrap_or("").trim();
        if reference.is_empty() || reference.chars().count() > 100 || reference.contains('\0') {
            return Err(invalid(
                "source_reference",
                "must contain 1 to 100 characters without null characters",
            ));
        }
        let csv = input["csv"]
            .as_str()
            .ok_or_else(|| invalid("csv", "is required"))?;
        if csv.contains('\0') {
            return Err(invalid("csv", "must not contain null characters"));
        }
        if input["currency_code"] != book.currency_code {
            return Err(invalid("currency_code", "must match the book's currency"));
        }
        let classifications = opening_classifications(input)?;
        if let Some(row)=c.query_opt("SELECT to_jsonb(b) || jsonb_build_object('state','reconciled') FROM finance_payables_opening_batches b WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3",&[&s.tenant(),&company,&reference]).await? {
            let batch:Value=row.get(0);
            if batch["source_reference"]!=reference || batch["source_csv"]!=csv {return Err(invalid("source_reference","opening bills have already been imported; the same source reference and exact CSV are required for a retry"));}
            let retained=c.query_opt("SELECT review FROM finance_payables_opening_decisions WHERE tenant_id=$1 AND company_id=$2 AND batch_id=$3",&[&s.tenant(),&company,&batch["id"].as_i64().ok_or(Error::Invalid)?]).await?;
            if let Some(row)=retained {let review:Value=row.get(0);if review["replaces_batch_id"]!=input["replaces_batch_id"] || review["reason"]!=input["reason"] || opening_classifications(&review)?!=classifications {return Err(invalid("source_reference","already identifies different correction instructions or opening account classifications"));}}
            return Ok(Prepared{review:json!({"state":"reconciled","batch":batch,"already_imported":true,"rows":[],"posts":false}),context,rows:vec![],balances:BTreeMap::new(),replaced_accounts:BTreeSet::new(),existing:Some(batch)});
        }
        let replacement = self.opening_replacement(c, s, company, input).await?;
        let parsed = crate::opening_csv::parse(csv.as_bytes(), ",")
            .map_err(|e| invalid("csv", format!("cannot read CSV: {e:?}")))?;
        if parsed
            .first()
            .map(|r| r.iter().map(String::as_str).collect::<Vec<_>>())
            != Some(HEADER.to_vec())
            || !(2..=10001).contains(&parsed.len())
            || (input["staged"] != true && (parsed.len() > 501 || csv.len() > 131072))
        {
            return Err(invalid(
                "csv",
                format!(
                    "requires header {} and 1 to 500 rows directly, or up to 10000 rows from staged batches",
                    HEADER.join(",")
                ),
            ));
        }
        let (_, setup) = store::settings(c, s.tenant(), company)
            .await?
            .ok_or(PayablesError::PayablesNotConfigured)?;
        let control = setup.payable_account_id;
        let chart = self.ledger.list_accounts_on(c, s, company).await?;
        if !chart.iter().any(|a| a.id == control && a.postable()) {
            return Err(invalid(
                "csv",
                "the payables control account must be active and postable",
            ));
        }
        let mut classified_accounts = Vec::new();
        let mut control_accounts = BTreeSet::new();
        let mut expected = BTreeSet::new();
        for account in &chart {
            let balance = context
                .balances
                .get(&account.id)
                .copied()
                .unwrap_or(Money::ZERO);
            if (balance == Money::ZERO && !classifications.contains_key(&account.id))
                || account.is_group
                || ![AccountType::Asset, AccountType::Liability].contains(&account.account_type)
            {
                continue;
            }
            expected.insert(account.id);
            let classification = classifications.get(&account.id).ok_or_else(|| {
                invalid(
                    "opening_account_classifications",
                    format!(
                        "classify {} ({}) as a receivables control, payables control, or not a control",
                        account.code,
                        balance.display(book.minor_units)
                    ),
                )
            })?;
            if (classification == "receivables_control"
                && account.account_type != AccountType::Asset)
                || (classification == "payables_control"
                    && account.account_type != AccountType::Liability)
            {
                return Err(invalid(
                    "opening_account_classifications",
                    format!(
                        "{} has an incompatible control classification",
                        account.code
                    ),
                ));
            }
            if classification == "payables_control" {
                control_accounts.insert(account.id);
            }
            classified_accounts.push(json!({"account_id":account.id,"account_code":account.code,"account_name":account.name,"account_type":account.account_type,"opening_balance":balance,"classification":classification}));
        }
        if let Some(id) = classifications.keys().find(|id| !expected.contains(id)) {
            return Err(invalid(
                "opening_account_classifications",
                format!("account ID {id} is not an eligible asset or liability account"),
            ));
        }
        if context
            .balances
            .get(&control)
            .is_some_and(|balance| *balance != Money::ZERO)
            && classifications.get(&control).map(String::as_str) != Some("payables_control")
        {
            return Err(invalid(
                "opening_account_classifications",
                "the configured payables account must be classified as a payables control",
            ));
        }
        if control_accounts.is_empty() {
            return Err(invalid(
                "opening_account_classifications",
                "classify at least one approved opening account as a payables control",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut docs = BTreeSet::new();
        let mut vendors = std::collections::BTreeMap::new();
        let codes: std::collections::HashSet<String> = parsed
            .iter()
            .skip(1)
            .filter_map(|row| row.get(1).map(|v| v.trim().to_owned()))
            .collect();
        let identities = PartyLinks::new(&self.parties)
            .by_codes(c, s, &codes)
            .await?;
        let kinds: Vec<&str> = identities.iter().map(|i| i.record_kind()).collect();
        let ids: Vec<String> = identities
            .iter()
            .map(|i| i.record_id().to_string())
            .collect();
        let by_party: std::collections::HashMap<_, _> = kinds
            .iter()
            .zip(&ids)
            .zip(&identities)
            .map(|((kind, id), identity)| (((*kind).to_owned(), id.clone()), identity))
            .collect();
        let mut vendor_ids = std::collections::HashMap::new();
        for row in c
            .query(
                "SELECT id, record_kind, record_id, active FROM finance_payables_vendors WHERE tenant_id=$1 AND company_id=$2 AND (record_kind, record_id) IN (SELECT * FROM unnest($3::text[], $4::text[]))",
                &[&s.tenant(), &company, &kinds, &ids],
            )
            .await?
        {
            let (id, kind, record, active): (i64, String, String, bool) =
                (row.get(0), row.get(1), row.get(2), row.get(3));
            if let Some(&party) = by_party.get(&(kind.clone(), record)) {
                vendor_ids.insert((kind, party.code.clone()), (id, active, party));
            }
        }
        let mut recorded: Vec<(i64, String, String, i32)> = vec![];
        let mut rows = vec![];
        let mut balances = BTreeMap::<i64, Money>::new();
        let mut debits = BTreeMap::<i64, Money>::new();
        let mut credits = BTreeMap::<i64, Money>::new();
        for (index, row) in parsed.into_iter().enumerate().skip(1) {
            let fail = |text: &str| invalid("csv", format!("row {}: {text}", index + 1));
            if row.len() != HEADER.len() {
                return Err(fail("has the wrong number of columns"));
            }
            let row: Vec<_> = row.iter().map(|v| v.trim()).collect();
            if row[0].is_empty() || row[0].chars().count() > 200 || !seen.insert(row[0].to_owned())
            {
                return Err(fail(
                    "source_id must be unique and contain 1 to 200 characters",
                ));
            }
            if row[1].is_empty()
                || row[1].chars().count() > 255
                || row[2].is_empty()
                || row[2].chars().count() > 200
            {
                return Err(fail(
                    "vendor_code and vendor_name are required, at most 255 and 200 characters",
                ));
            }
            if vendors
                .insert((row[9].to_owned(), row[1].to_owned()), row[2].to_owned())
                .is_some_and(|name| name != row[2])
            {
                return Err(fail("vendor_code identifies more than one name"));
            }
            if row[3].is_empty()
                || row[3].chars().count() > 100
                || !docs.insert((row[9].to_owned(), row[1].to_owned(), row[3].to_owned()))
            {
                return Err(fail(
                    "document_reference must contain 1 to 100 characters and be unique for the vendor",
                ));
            }
            if !["company", "person"].contains(&row[9]) {
                return Err(fail("party_kind must be company or person"));
            }
            let &(vendor_id, active, party) = vendor_ids.get(&(row[9].to_owned(),row[1].to_owned())).ok_or_else(|| {
                fail("vendor_code and party_kind must identify an existing Core-linked vendor of the current company")
            })?;
            Self::billable(active, &party.status)
                .map_err(|e| vendor_refusal(e, &format!("row {}", index + 1)))?;
            if party.name.chars().count() > 200 {
                return Err(fail(
                    "the party's name exceeds the document's 200 character limit",
                ));
            }
            let vendor = json!({"id":vendor_id,"active":active,"party":{"kind":party.record_kind(),"id":party.record_id(),"code":party.code,"name":party.name,"status":party.status}});
            let party_key = vendors::key(vendor_id);
            recorded.push((
                vendor_id,
                party_key.clone(),
                row[3].to_owned(),
                index as i32 + 1,
            ));
            let date: Date = row[4]
                .parse()
                .map_err(|_| fail("document_date must be YYYY-MM-DD"))?;
            let is_credit = row[10] == "supplier_credit";
            if !["bill", "supplier_credit"].contains(&row[10]) {
                return Err(fail("item_kind must be bill or supplier_credit"));
            }
            let source_kind = row[11];
            if (!is_credit && source_kind != "bill")
                || (is_credit && !["credit_note", "payment"].contains(&source_kind))
            {
                return Err(fail("source_kind does not match the item kind"));
            }
            if row[12].is_empty() || row[12].chars().count() > 200 {
                return Err(fail("source_system must contain 1 to 200 characters"));
            }
            if row[13] != book.currency_code {
                return Err(fail(
                    "currency_code must match the book's currency; translation is not supported",
                ));
            }
            let due: jiff::civil::Date = if is_credit && row[5].is_empty() {
                date
            } else {
                row[5]
                    .parse()
                    .map_err(|_| fail("due_date must be YYYY-MM-DD"))?
            };
            if date.year() < 1 || due.year() < 1 {
                return Err(fail("document and due dates must be in year 0001 or later"));
            }
            if date >= context.entry.entry_date || due < date {
                return Err(fail(
                    "document_date must precede conversion and due_date must not precede document_date",
                ));
            }
            let original = Money::parse(row[6], book.minor_units)
                .map_err(|_| fail("original_total must be a book-precision amount"))?;
            let residual = Money::parse(row[7], book.minor_units)
                .map_err(|_| fail("outstanding must be a book-precision amount"))?;
            if !original.is_positive() || !residual.is_positive() || residual > original {
                return Err(fail(
                    "outstanding must be positive and no greater than original_total",
                ));
            }
            let tax_facts = source_tax_facts(row[14], original, book.minor_units)
                .map_err(|message| fail(&message))?;
            let account = chart
                .iter()
                .find(|a| a.code == row[8] && control_accounts.contains(&a.id))
                .ok_or_else(|| {
                    fail("control_account_code must identify an account classified as a payables control")
                })?;
            let balance = balances.entry(account.id).or_insert(Money::ZERO);
            *balance = Money::new(
                (*balance + if is_credit { -residual } else { residual }).amount(),
                book.minor_units,
            )
            .map_err(|e| fail(&e.to_string()))?;
            let gross = if is_credit { &mut credits } else { &mut debits };
            let value = gross.entry(account.id).or_insert(Money::ZERO);
            *value = Money::new((*value + residual).amount(), book.minor_units)
                .map_err(|e| fail(&e.to_string()))?;
            rows.push(json!({"item_kind":row[10],"source_kind":source_kind,"source_system":row[12],"currency_code":row[13],"source_tax_facts":tax_facts,"source_id":row[0],"vendor_code":row[1],"party_kind":row[9],"vendor_name":row[2],"vendor_id":vendor_id,"vendor":vendor,"party_key":party_key,"document_reference":row[3],"document_date":date.to_string(),"due_date":if is_credit && row[5].is_empty() { Value::Null } else { json!(due.to_string()) },"original_total":original,"outstanding":residual,"settled_before_conversion":original-residual,"account_id":account.id,"account_code":account.code}));
        }
        if !recorded.is_empty() {
            let (ids, keys, numbers, rows_at): (Vec<i64>, Vec<String>, Vec<String>, Vec<i32>) =
                recorded.into_iter().fold(
                    (vec![], vec![], vec![], vec![]),
                    |mut acc, (a, b, c, d)| {
                        acc.0.push(a);
                        acc.1.push(b);
                        acc.2.push(c);
                        acc.3.push(d);
                        acc
                    },
                );
            if let Some(row) = c.query_opt("SELECT u.pos FROM unnest($3::bigint[],$4::text[],$5::text[],$6::int[]) AS u(vendor_id,party_key,doc,pos) JOIN finance_payables_documents d ON d.tenant_id=$1 AND d.company_id=$2 AND d.party_document_number=u.doc AND d.status<>'voided' AND d.party_key=u.party_key WHERE NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id AND (i.batch_id=$7 OR EXISTS(SELECT 1 FROM finance_payables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=i.batch_id))) ORDER BY u.pos LIMIT 1", &[&s.tenant(), &company, &ids, &keys, &numbers, &rows_at, &input["replaces_batch_id"].as_i64()]).await? {
                return Err(invalid("csv", format!("row {}: the vendor document is already recorded", row.get::<_, i32>(0))));
            }
        }
        let mut reconciliation = Vec::new();
        for &id in &control_accounts {
            let account = chart.iter().find(|a| a.id == id).ok_or(Error::Internal)?;
            let balance = balances.get(&id).copied().unwrap_or(Money::ZERO);
            let expected = context.balances.get(&id).copied().unwrap_or(Money::ZERO);
            if expected == Money::ZERO && (!debits.contains_key(&id) || !credits.contains_key(&id))
            {
                return Err(invalid(
                    "opening_account_classifications",
                    format!(
                        "{} may be classified as a zero-balance payables control only with offsetting bill and supplier-credit rows",
                        account.code
                    ),
                ));
            }
            let signed = Money::ZERO - balance;
            if signed != expected {
                return Err(invalid("csv", format!("opening bills on {} total {} but the approved opening balance is {} (difference {})", account.code, signed.display(book.minor_units), expected.display(book.minor_units), (signed - expected).display(book.minor_units))));
            }
            reconciliation.push(json!({"gross_debits":credits.get(&id).copied().unwrap_or(Money::ZERO),"gross_credits":debits.get(&id).copied().unwrap_or(Money::ZERO),"account_id":id,"account_code":account.code,"register_balance":balance,"opening_balance":Money::ZERO-expected,"ledger_opening_balance":expected,"balance_basis":"credit_positive","difference":Money::ZERO}));
        }
        let source_ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r["source_id"].as_str())
            .collect();
        if c.query_opt("SELECT i.id FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.source_id=ANY($3::text[]) AND i.batch_id<>COALESCE($4::bigint,0::bigint) AND NOT EXISTS(SELECT 1 FROM finance_payables_opening_decisions d WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.replaces_batch_id=i.batch_id) LIMIT 1", &[&s.tenant(), &company, &source_ids, &input["replaces_batch_id"].as_i64()]).await?.is_some() {
            return Err(invalid("csv", "a source ID is already used by another opening register"));
        }
        if replacement.is_none() && c.query_opt("SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND status='posted' AND NOT EXISTS(SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=finance_payables_documents.id) LIMIT 1",&[&s.tenant(),&company]).await?.is_some() {return Err(invalid("csv","import opening bills before posting ordinary bills"));}
        let mut replaced_accounts = BTreeSet::new();
        if let Some(old) = &replacement {
            let old_id = old["id"].as_i64().ok_or(Error::Invalid)?;
            let accounts=c.query("SELECT (r->>'account_id')::bigint FROM finance_payables_opening_decisions d CROSS JOIN LATERAL jsonb_array_elements(d.review->'reconciliation') r WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.batch_id=$3",&[&s.tenant(),&company,&old_id]).await?;
            for row in accounts {
                let id: i64 = row.get(0);
                if !balances.contains_key(&id) {
                    return Err(invalid("csv","a correction must retain every previously recognized opening account and its balance"));
                }
                replaced_accounts.insert(id);
            }
        }
        let approval = self
            .ledger
            .preview_approval_on(
                c,
                s,
                company,
                &self
                    .opening_approval(c, s, company, &context, input)
                    .await?,
            )
            .await?;
        Ok(Prepared {
            review: json!({"state":"previewed","replaces_batch_id":input["replaces_batch_id"],"reason":input["reason"],"staged":input["staged"]==true,"conversion_date":context.entry.entry_date.to_string(),"entry_id":context.entry.id,"entry_number":context.entry.entry_number,"source_reference":reference,"currency_code":book.currency_code,"minor_units":book.minor_units,"opening_account_classifications":classifications,"classified_opening_accounts":classified_accounts,"rows":rows,"reconciliation":reconciliation,"posts":false,"approval":approval,"already_imported":false}),
            context,
            rows,
            balances,
            replaced_accounts,
            existing: None,
        })
    }
    pub async fn import_opening_bills(
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
                POST_DOCUMENTS,
                "finance.payables.opening.import",
            )
            .await?;
        let result = self.import_opening_on(&mut tx, s, company, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn import_opening_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
        join(tx, s, company, POST_DOCUMENTS).await?;
        let resolved = self.resolve_opening(tx.sql(), s, company, input).await?;
        let input = &resolved;
        let p = self.prepare_opening(tx.sql(), s, company, input).await?;
        if let Some(mut batch) = p.existing {
            batch["state"] = json!("reconciled");
            return Ok(batch);
        }
        self.ledger
            .check_approval(
                tx,
                s,
                company,
                &self
                    .opening_approval(tx.sql(), s, company, &p.context, input)
                    .await?,
            )
            .await?;
        let review = p.review.clone();
        let tenant = s.tenant();
        let actor = s.user();
        let imp = s.impersonator().map(|p| p.user());
        let source = input["source_reference"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let csv = input["csv"].clone();
        let entry = p.context.entry;
        let mut rows = p.rows;
        let mut frozen = std::collections::HashMap::new();
        for facts in &mut rows {
            let vendor_id = facts["vendor_id"].as_i64().ok_or(Error::Invalid)?;
            if let std::collections::hash_map::Entry::Vacant(e) = frozen.entry(vendor_id) {
                e.insert(
                    self.frozen_vendor_party(
                        tx.sql(),
                        s,
                        facts["vendor"]["party"]["kind"]
                            .as_str()
                            .ok_or(Error::Invalid)?,
                        facts["vendor"]["party"]["id"]
                            .as_i64()
                            .ok_or(Error::Invalid)?,
                    )
                    .await
                    .map_err(|e| {
                        vendor_refusal(
                            e,
                            &format!("source {}", facts["source_id"].as_str().unwrap_or("")),
                        )
                    })?,
                );
            }
            facts["party"] = frozen[&vendor_id].clone();
        }
        let batch=tx.mutate(move |sql|Box::pin(async move {
            let (batch,change)=store::insert(sql,store::Table::OpeningBatch,json!({"tenant_id":tenant,"company_id":company,"source_reference":source,"source_csv":csv,"currency_code":entry.currency_code,"conversion_date":entry.entry_date.to_string(),"entry_id":entry.id,"entry_number":entry.entry_number,"recorded_by_user_id":actor,"recorded_by_impersonator_id":imp,"recorded_at":jiff::Timestamp::now().to_string()})).await?;
            let mut changes=vec![change];
            for facts in rows {
                let control = facts["account_id"].as_i64().ok_or(Error::Invalid)?;
                let (old,change)=store::insert(sql,store::Table::Documents,json!({"tenant_id":tenant,"company_id":company,"opening_batch_id":batch["id"],"document_type":if facts["item_kind"]=="supplier_credit" {"opening_credit"} else {"bill"},"status":"draft","party_key":facts["party_key"],"party_code":facts["party"]["party_code"],"party_name":facts["party"]["party_name"],"party_tax_id":facts["party"]["party_tax_identifier"],"party_registration_id":facts["party"]["party_registration_identifier"],"party_document_number":facts["document_reference"],"document_date":facts["document_date"],"due_date":if facts["due_date"].is_null() { &facts["document_date"] } else { &facts["due_date"] },"currency_code":entry.currency_code,"description":"Opening outstanding balance; no new purchase","total_net":facts["outstanding"],"total_tax":"0","total":facts["outstanding"],"created_by_user_id":actor,"created_by_impersonator_id":imp})).await?;
                let id=old["id"].as_i64().ok_or(Error::Invalid)?;changes.push(change);
                changes.push(store::insert(sql,store::Table::Lines,json!({"tenant_id":tenant,"company_id":company,"document_id":id,"line_number":1,"description":"Opening outstanding balance; no new purchase","account_id":control,"amount":facts["outstanding"]})).await?.1);
                let approved:Value=sql.query_one("UPDATE finance_payables_documents d SET status='approved',approved_by_user_id=$4,approved_by_impersonator_id=$5 WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING to_jsonb(d)",&[&tenant,&company,&id,&actor,&imp]).await?.get(0);
                changes.push(Change{kind:"finance.payables.document".into(),id,old,new:approved.clone()});
                let posted:Value=sql.query_one("UPDATE finance_payables_documents d SET status='posted',posted_by_user_id=$4,posted_by_impersonator_id=$5,payable_account_id=$6,entry_id=$7,entry_number=$8 WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING to_jsonb(d)",&[&tenant,&company,&id,&actor,&imp,&control,&entry.id,&entry.entry_number]).await?.get(0);
                changes.push(Change{kind:"finance.payables.document".into(),id,old:approved,new:posted});
                changes.push(store::insert(sql,store::Table::OpeningItem,json!({"tenant_id":tenant,"company_id":company,"batch_id":batch["id"],"source_id":facts["source_id"],"document_id":id,"source_facts":facts})).await?.1);
            }
            Ok((batch,changes))
        })).await?;
        let previous = input["replaces_batch_id"]
            .as_i64()
            .map(|id| format!("opening:{id}"));
        for (control, balance) in p.balances {
            self.ledger
                .replace_opening_recognition_on(
                    tx,
                    s,
                    company,
                    &EntryOwner {
                        module: MODULE.into(),
                        record: format!("opening:{}", batch["id"]),
                    },
                    if p.replaced_accounts.contains(&control) {
                        previous.as_deref()
                    } else {
                        None
                    },
                    control,
                    Money::ZERO - balance,
                )
                .await?;
        }
        self.record_opening_decision(tx, s, company, &batch, input, &review)
            .await?;
        let mut batch = batch;
        batch["state"] = json!("reconciled");
        Ok(batch)
    }
}

/// Refuse settlement before Bilibi owns processing, independently of the
/// original historical document date retained for aging.
pub(crate) async fn settlement_date(
    c: &dyn OperationClient,
    tenant: i64,
    company: i64,
    document: i64,
    date: jiff::civil::Date,
    field: &'static str,
) -> Result<()> {
    if c.query_opt("SELECT i.id FROM finance_payables_opening_items i JOIN finance_payables_opening_decisions d ON d.replaces_batch_id=i.batch_id AND d.tenant_id=$1 AND d.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3",&[&tenant,&company,&document]).await?.is_some() {return Err(invalid(field,"belongs to a replaced opening register; use the replacement record"));}
    if c.query_opt("SELECT i.id FROM finance_payables_opening_items i JOIN finance_payables_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3 AND b.conversion_date>$4::text::date",&[&tenant,&company,&document,&date.to_string()]).await?.is_some() {return Err(invalid(field,"cannot be settled before the conversion date"));}
    Ok(())
}

/// Opening records add no posting, so a reproduced subledger report also keeps
/// this independent immutable-register boundary.
pub(crate) async fn pin(
    c: &dyn bilibi_agent_api::OperationClient,
    s: &Scope,
    company: i64,
    through: Option<i64>,
) -> Result<i64> {
    if through.is_some_and(|id| id < 0) {
        return Err(PayablesError::InvalidOption);
    }
    let maximum:i64=c.query_opt("SELECT COALESCE(max(id),0) FROM finance_payables_opening_batches WHERE tenant_id=$1 AND company_id=$2",&[&s.tenant(),&company]).await?.ok_or(Error::Internal)?.get(0);
    let pin = through.unwrap_or(maximum);
    if pin > maximum {
        return Err(PayablesError::InvalidOption);
    }
    Ok(pin)
}

// Source facts are evidence, never tax lines or a tax calculation on a residual.
fn source_tax_facts(
    text: &str,
    gross: Money,
    units: bilibi_finance_ledger::MinorUnits,
) -> std::result::Result<Value, String> {
    let facts: Value = if ["unknown", "not_applicable"].contains(&text) {
        json!({"status":text})
    } else {
        serde_json::from_str(text)
            .map_err(|_| "source_tax_facts must be a JSON object".to_owned())?
    };
    let object = facts
        .as_object()
        .ok_or("source_tax_facts must be a JSON object")?;
    let status = facts["status"].as_str().unwrap_or("");
    if ["unknown", "not_applicable"].contains(&status) {
        if object.len() != 1 {
            return Err(
                "unknown or not_applicable tax facts cannot contain supplied amounts".into(),
            );
        }
        return Ok(facts);
    }
    if status != "supplied" {
        return Err("source_tax_facts status must be unknown, not_applicable or supplied".into());
    }
    let amount = |value: &Value| -> std::result::Result<Money, String> {
        let value = value
            .as_str()
            .ok_or("source tax amounts must be decimal strings")?;
        let money =
            Money::parse(value, units).map_err(|_| "source tax amounts must use book precision")?;
        if money < Money::ZERO {
            return Err("source tax amounts cannot be negative".into());
        }
        Ok(money)
    };
    let tax = facts
        .get("tax_total")
        .filter(|v| !v.is_null())
        .map(amount)
        .transpose()?;
    let net = facts
        .get("net")
        .filter(|v| !v.is_null())
        .map(amount)
        .transpose()?;
    if tax.zip(net).is_some_and(|(tax, net)| tax + net != gross) {
        return Err("source net plus tax_total must equal original_total".into());
    }
    let mut line_net = Money::ZERO;
    let mut line_tax = Money::ZERO;
    let mut line_total = Money::ZERO;
    let mut complete_net = true;
    let mut complete_tax = true;
    if let Some(lines) = facts.get("lines") {
        let lines = lines
            .as_array()
            .filter(|l| !l.is_empty() && l.len() <= 500)
            .ok_or("source tax lines must contain 1 to 500 lines")?;
        for (index, line) in lines.iter().enumerate() {
            let number = index + 1;
            if !line.is_object() {
                return Err(format!("source tax line {number} must be an object"));
            }
            let line_amount = |key: &str| -> std::result::Result<Option<Money>, String> {
                line.get(key)
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        amount(value)
                            .map_err(|message| format!("source tax line {number} {key}: {message}"))
                    })
                    .transpose()
            };
            let taxable = line_amount("taxable_amount")?;
            let net = line_amount("net")?;
            let tax_amount = line_amount("tax_amount")?;
            let gross = line_amount("gross")?;
            let add = |total: Money, value: Money| {
                Money::new((total + value).amount(), units)
                    .map_err(|error| format!("source tax line {number}: {error}"))
            };
            if let Some(value) = net {
                line_net = add(line_net, value)?;
            } else {
                complete_net = false;
            }
            if let Some(value) = tax_amount {
                line_tax = add(line_tax, value)?;
            } else {
                complete_tax = false;
            }
            if let (Some(net), Some(tax_amount), Some(gross)) = (net, tax_amount, gross) {
                if net + tax_amount != gross {
                    return Err(format!(
                        "source tax line {number}: net plus tax_amount must equal gross"
                    ));
                }
            }
            let total = match (gross, net, tax_amount) {
                (Some(gross), _, _) => gross,
                (None, Some(net), Some(tax_amount)) => add(net, tax_amount)?,
                _ => {
                    return Err(format!(
                        "source tax line {number}: gross or both net and tax_amount are required to reconcile with original_total"
                    ));
                }
            };
            line_total = add(line_total, total)?;
            let rate = line
                .get("rate")
                .filter(|value| !value.is_null())
                .map(|rate| {
                    let rate = rate
                        .as_str()
                        .ok_or_else(|| {
                            format!("source tax line {number} rate must be a decimal string")
                        })?
                        .parse::<rust_decimal::Decimal>()
                        .map_err(|_| {
                            format!("source tax line {number} rate must be a decimal string")
                        })?;
                    if rate < rust_decimal::Decimal::ZERO {
                        return Err(format!("source tax line {number} rate cannot be negative"));
                    }
                    Ok(rate)
                })
                .transpose()?;
            if let (Some(taxable), Some(rate), Some(tax_amount)) = (taxable, rate, tax_amount) {
                let calculated = taxable
                    .amount()
                    .checked_mul(rate)
                    .and_then(|value| value.checked_div(rust_decimal::Decimal::from(100)))
                    .ok_or_else(|| {
                        format!("source tax line {number} rate calculation is too large")
                    })?
                    .round_dp_with_strategy(
                        u32::from(units.get()),
                        rust_decimal::RoundingStrategy::MidpointAwayFromZero,
                    );
                if calculated != tax_amount.amount() {
                    return Err(format!(
                        "source tax line {number}: taxable_amount multiplied by rate must round to tax_amount"
                    ));
                }
            }
        }
        let line_subject = if lines.len() == 1 {
            "source tax line 1".to_owned()
        } else {
            format!("source tax lines 1 to {}", lines.len())
        };
        if net.is_some() && !complete_net {
            return Err(format!(
                "{line_subject}: every net amount is required to agree with net"
            ));
        }
        if net.is_some_and(|total| total != line_net) {
            return Err(format!("{line_subject}: net amounts must equal net"));
        }
        if tax.is_some() && !complete_tax {
            return Err(format!(
                "{line_subject}: every tax_amount is required to agree with tax_total"
            ));
        }
        if tax.is_some_and(|total| total != line_tax) {
            return Err(format!("{line_subject}: tax amounts must equal tax_total"));
        }
        if line_total != gross {
            return Err(format!(
                "{line_subject}: line totals must equal original_total"
            ));
        }
    } else if tax.is_none() && net.is_none() {
        return Err("supplied tax facts require a net, tax_total or source lines".into());
    }
    Ok(facts)
}
