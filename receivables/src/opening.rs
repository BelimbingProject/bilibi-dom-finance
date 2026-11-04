//! Conversion residuals use Receivables' settlement records without issuing a
//! new supply or posting the source control balance again.
use crate::*;
use bilibi_agent_api::OperationClient;
use bilibi_database::Change;
use bilibi_finance_ledger::{AccountType, EntryOwner, Money, OpeningContext, PartyLinks};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const HEADER: &[&str] = &[
    "source_id",
    "customer_code",
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
const BATCH: store::Table = store::Table {
    name: "finance_receivables_opening_batches",
    kind: "finance.receivables.opening_batch",
    columns: &[
        "source_reference",
        "source_csv",
        "currency_code",
        "conversion_date",
        "entry_id",
        "entry_number",
        "series_id",
        "recorded_by_user_id",
        "recorded_by_impersonator_id",
    ],
    timestamps: false,
};
const ITEM: store::Table = store::Table {
    name: "finance_receivables_opening_items",
    kind: "finance.receivables.opening_item",
    columns: &["batch_id", "source_id", "document_id", "source_facts"],
    timestamps: false,
};

pub(super) struct Prepared {
    pub review: Value,
    context: OpeningContext,
    rows: Vec<Value>,
    balances: BTreeMap<i64, Money>,
    replaced_accounts: BTreeSet<i64>,
    series: Value,
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

impl Receivables {
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
        self.authorize_on(c, s, company, VIEW).await?;
        let opening = self.ledger.opening_context_on(c, s, company).await?;
        let book = self.ledger.get_book_on(c, s, company).await?;
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
        let items: Vec<Value> = c.query("SELECT to_jsonb(i) FROM finance_receivables_opening_items i WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4", &[&s.tenant(), &company, &after, &(limit + 1)]).await?.iter().map(|row| row.get(0)).collect();
        let more = items.len() as i64 > limit;
        let items: Vec<Value> = items.into_iter().take(limit as usize).collect();
        let next = if more {
            items.last().map(|i| i["id"].clone())
        } else {
            None
        };
        let total: i64 = c.query_opt("SELECT count(*) FROM finance_receivables_opening_items WHERE tenant_id=$1 AND company_id=$2", &[&s.tenant(), &company]).await?.ok_or(Error::Internal)?.get(0);
        let stages = c.query("SELECT to_jsonb(s)-'payload'||jsonb_build_object('part_count',payload->'part_count','csv_bytes',octet_length(payload->>'csv')) FROM finance_receivables_opening_stages s WHERE tenant_id=$1 AND company_id=$2 ORDER BY id", &[&s.tenant(), &company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>();
        let stage_sources = c.query("SELECT jsonb_build_object('source_reference',source_reference,'opening_account_classifications',payload->'opening_account_classifications') FROM (SELECT DISTINCT ON (source_reference) source_reference,payload,id FROM finance_receivables_opening_stages WHERE tenant_id=$1 AND company_id=$2 ORDER BY source_reference,id DESC) source ORDER BY id", &[&s.tenant(), &company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>();
        Ok(
            json!({"opening":opening,"book":book,"opening_accounts":opening_accounts,"batch":c.query_opt("SELECT to_jsonb(b) || jsonb_build_object('state','reconciled') FROM finance_receivables_opening_batches b WHERE tenant_id=$1 AND company_id=$2 ORDER BY id DESC LIMIT 1",&[&s.tenant(),&company]).await?.map(|r|r.get::<_,Value>(0)),"stages":stages,"stage_sources":stage_sources,"decisions":c.query("SELECT to_jsonb(d) FROM finance_receivables_opening_decisions d WHERE tenant_id=$1 AND company_id=$2 ORDER BY id",&[&s.tenant(),&company]).await?.iter().map(|r|r.get::<_,Value>(0)).collect::<Vec<_>>(),"header":HEADER,"items":items,"item_count":total,"items_next":next}),
        )
    }

    pub(super) async fn prepare_opening(
        &self,
        c: &dyn OperationClient,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Prepared> {
        self.join(c, s, company, ISSUE).await?;
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
        let series_id = input["series_id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("series_id", "select an invoice series"))?;
        let series = store::find(c, store::SERIES, s.tenant(), company, Some(series_id))
            .await?
            .ok_or(ReceivableError::SeriesNotFound)?;
        let classifications = opening_classifications(input)?;
        let existing = c.query_opt("SELECT to_jsonb(b) || jsonb_build_object('state','reconciled') FROM finance_receivables_opening_batches b WHERE tenant_id=$1 AND company_id=$2 AND source_reference=$3",&[&s.tenant(),&company,&reference]).await?.map(|r|r.get::<_,Value>(0));
        if let Some(batch) = existing {
            if batch["source_reference"] != reference
                || batch["source_csv"] != csv
                || batch["series_id"] != series_id
            {
                return Err(invalid(
                    "source_reference",
                    "opening invoices have already been imported; the same source reference and exact CSV are required for a retry",
                ));
            }
            let retained=c.query_opt("SELECT review FROM finance_receivables_opening_decisions WHERE tenant_id=$1 AND company_id=$2 AND batch_id=$3",&[&s.tenant(),&company,&batch["id"].as_i64().ok_or(Error::Invalid)?]).await?;
            if let Some(row) = retained {
                let review: Value = row.get(0);
                if review["replaces_batch_id"] != input["replaces_batch_id"]
                    || review["reason"] != input["reason"]
                    || opening_classifications(&review)? != classifications
                {
                    return Err(invalid(
                        "source_reference",
                        "already identifies different correction instructions or opening account classifications",
                    ));
                }
            }
            return Ok(Prepared {
                review: json!({"state":"reconciled","batch":batch,"already_imported":true,"rows":[],"posts":false}),
                context,
                rows: vec![],
                balances: BTreeMap::new(),
                replaced_accounts: BTreeSet::new(),
                series,
                existing: Some(batch),
            });
        }
        if series["kind"] != "invoice" || series["active"] != true {
            return Err(invalid(
                "series_id",
                "must be an active invoice series; use a dedicated opening series",
            ));
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
        let setup = store::find(c, store::SETUPS, s.tenant(), company, None)
            .await?
            .ok_or(ReceivableError::NotConfigured)?;
        let control = setup["receivable_account_id"]
            .as_i64()
            .ok_or(Error::Internal)?;
        let chart = self.ledger.list_accounts_on(c, s, company).await?;
        if !chart.iter().any(|a| a.id == control && a.postable()) {
            return Err(invalid(
                "csv",
                "the receivables control account must be active and postable",
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
            if classification == "receivables_control" {
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
            && classifications.get(&control).map(String::as_str) != Some("receivables_control")
        {
            return Err(invalid(
                "opening_account_classifications",
                "the configured receivables account must be classified as a receivables control",
            ));
        }
        if control_accounts.is_empty() {
            return Err(invalid(
                "opening_account_classifications",
                "classify at least one approved opening account as a receivables control",
            ));
        }
        let requested: std::collections::HashSet<String> = parsed
            .iter()
            .skip(1)
            .filter_map(|row| row.get(1).map(|v| v.trim().to_owned()))
            .collect();
        let identities = PartyLinks::new(&self.parties)
            .by_codes(c, s, &requested)
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
        let mut customers = std::collections::HashMap::new();
        for row in c
            .query(
                "SELECT id, record_kind, record_id FROM finance_receivables_customers WHERE tenant_id=$1 AND company_id=$2 AND active AND (record_kind, record_id) IN (SELECT * FROM unnest($3::text[], $4::text[]))",
                &[&s.tenant(), &company, &kinds, &ids],
            )
            .await?
        {
            let (id, kind, record): (i64, String, String) = (row.get(0), row.get(1), row.get(2));
            if let Some(party) = by_party.get(&(kind.clone(), record)) {
                customers.insert(
                    (kind, party.code.clone()),
                    json!({"id":id,"code":party.code,"name":party.name,"record_kind":party.record_kind(),"record_id":party.record_id()}),
                );
            }
        }
        let mut seen = BTreeSet::new();
        let mut documents = BTreeSet::new();
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
            if row[2].is_empty() || row[2].chars().count() > 100 {
                return Err(fail("document_reference must contain 1 to 100 characters"));
            }
            if !["company", "person"].contains(&row[8]) {
                return Err(fail("party_kind must be company or person"));
            }
            if row[1].is_empty() {
                return Err(fail("customer_code is required"));
            }
            let customer = customers.get(&(row[8].to_owned(),row[1].to_owned())).ok_or_else(|| {
                fail("customer_code and party_kind must identify an active Core-linked customer in the current company")
            })?;
            let customer_id = customer["id"].as_i64().ok_or(Error::Internal)?;
            let customer_name = customer["name"].as_str().ok_or(Error::Internal)?;
            if !documents.insert((customer_id, row[2].to_owned())) {
                return Err(fail("the customer document reference is duplicated"));
            }
            let date: jiff::civil::Date = row[3]
                .parse()
                .map_err(|_| fail("document_date must be YYYY-MM-DD"))?;
            let is_credit = row[9] == "customer_credit";
            if !["invoice", "customer_credit"].contains(&row[9]) {
                return Err(fail("item_kind must be invoice or customer_credit"));
            }
            let source_kind = row[10];
            if (!is_credit && source_kind != "invoice")
                || (is_credit && !["credit_note", "receipt"].contains(&source_kind))
            {
                return Err(fail("source_kind does not match the item kind"));
            }
            if row[11].is_empty() || row[11].chars().count() > 200 {
                return Err(fail("source_system must contain 1 to 200 characters"));
            }
            if row[12] != book.currency_code {
                return Err(fail(
                    "currency_code must match the book's currency; translation is not supported",
                ));
            }
            let due: jiff::civil::Date = if is_credit && row[4].is_empty() {
                date
            } else {
                row[4]
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
            let original = Money::parse(row[5], book.minor_units)
                .map_err(|_| fail("original_total must be a book-precision amount"))?;
            let residual = Money::parse(row[6], book.minor_units)
                .map_err(|_| fail("outstanding must be a book-precision amount"))?;
            if !original.is_positive() || !residual.is_positive() || residual > original {
                return Err(fail(
                    "outstanding must be positive and no greater than original_total",
                ));
            }
            let tax_facts = source_tax_facts(row[13], original, book.minor_units)
                .map_err(|message| fail(&message))?;
            let account = chart
                .iter()
                .find(|a| a.code == row[7] && control_accounts.contains(&a.id))
                .ok_or_else(|| {
                    fail("control_account_code must identify an account classified as a receivables control")
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
            rows.push(json!({"item_kind":row[9],"source_kind":source_kind,"source_system":row[11],"currency_code":row[12],"source_tax_facts":tax_facts,"source_id":row[0],"customer_id":customer_id,"customer_code":row[1],"party_kind":row[8],"customer_name":customer_name,"customer":customer,"document_reference":row[2],"document_date":date.to_string(),"due_date":if is_credit && row[4].is_empty() { Value::Null } else { json!(due.to_string()) },"original_total":original,"outstanding":residual,"settled_before_conversion":original-residual,"account_id":account.id,"account_code":account.code}));
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
                        "{} may be classified as a zero-balance receivables control only with offsetting invoice and customer-credit rows",
                        account.code
                    ),
                ));
            }
            if balance != expected {
                return Err(invalid("csv", format!("opening invoices on {} total {} but the approved opening balance is {} (difference {})", account.code, balance.display(book.minor_units), expected.display(book.minor_units), (balance - expected).display(book.minor_units))));
            }
            reconciliation.push(json!({"gross_debits":debits.get(&id).copied().unwrap_or(Money::ZERO),"gross_credits":credits.get(&id).copied().unwrap_or(Money::ZERO),"account_id":id,"account_code":account.code,"register_balance":balance,"opening_balance":expected,"difference":Money::ZERO}));
        }
        let account_ids: Vec<i64> = control_accounts.iter().copied().collect();
        if c.query_opt("SELECT id FROM finance_receivables_payments WHERE tenant_id=$1 AND company_id=$2 AND deposit_account_id=ANY($3::bigint[]) LIMIT 1", &[&s.tenant(), &company, &account_ids]).await?.is_some() {
            return Err(invalid("csv", "an account used for receipt deposits cannot be a receivables control"));
        }
        let parties: Vec<i64> = rows
            .iter()
            .filter_map(|r| r["customer_id"].as_i64())
            .collect();
        let references: Vec<&str> = rows
            .iter()
            .filter_map(|r| r["document_reference"].as_str())
            .collect();
        if c.query_opt("SELECT d.id FROM unnest($3::bigint[],$4::text[]) u(party,reference) JOIN finance_receivables_documents d ON d.tenant_id=$1 AND d.company_id=$2 AND d.customer_id=u.party AND d.status='issued' LEFT JOIN finance_receivables_opening_items i ON i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=d.id WHERE (d.document_reference=u.reference OR i.source_facts->>'document_reference'=u.reference) AND (i.id IS NULL OR (i.batch_id<>COALESCE($5::bigint,0) AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_decisions x WHERE x.tenant_id=$1 AND x.company_id=$2 AND x.replaces_batch_id=i.batch_id))) LIMIT 1",&[&s.tenant(),&company,&parties,&references,&input["replaces_batch_id"].as_i64()]).await?.is_some() {return Err(invalid("csv","a customer source document is already recorded"));}
        let source_ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r["source_id"].as_str())
            .collect();
        if c.query_opt("SELECT i.id FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.source_id=ANY($3::text[]) AND i.batch_id<>COALESCE($4::bigint,0::bigint) AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_decisions d WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.replaces_batch_id=i.batch_id) LIMIT 1", &[&s.tenant(), &company, &source_ids, &input["replaces_batch_id"].as_i64()]).await?.is_some() {
            return Err(invalid("csv", "a source ID is already used by another opening register"));
        }
        if replacement.is_none() && c.query_opt("SELECT id FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND status='issued' AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=finance_receivables_documents.id) LIMIT 1",&[&s.tenant(),&company]).await?.is_some() {return Err(invalid("csv","import opening invoices before issuing ordinary invoices"));}
        let mut replaced_accounts = BTreeSet::new();
        if let Some(old) = &replacement {
            let old_id = old["id"].as_i64().ok_or(Error::Invalid)?;
            let accounts=c.query("SELECT (r->>'account_id')::bigint FROM finance_receivables_opening_decisions d CROSS JOIN LATERAL jsonb_array_elements(d.review->'reconciliation') r WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.batch_id=$3",&[&s.tenant(),&company,&old_id]).await?;
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
            series,
            existing: None,
        })
    }

    /// Import a complete, reconciled opening receivables register. No entry or
    /// tax lines are created. Identical retries return the retained batch.
    pub async fn import_opening_invoices(
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
                "finance.receivables.opening.import",
            )
            .await?;
        let record = self.import_opening_on(&mut tx, s, company, input).await?;
        tx.commit().await?;
        Ok(record)
    }
    pub(super) async fn import_opening_on(
        &self,
        tx: &mut MutationTransaction<'_>,
        s: &Scope,
        company: i64,
        input: &Value,
    ) -> Result<Value> {
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
        let series = p.series;
        let rows = p.rows;
        let batch=tx.mutate(move |sql|Box::pin(async move {
            let mut changes=vec![];
            let batch=store::save(sql,BATCH,tenant,company,None,json!({"source_reference":source,"source_csv":csv,"currency_code":entry.currency_code,"conversion_date":entry.entry_date.to_string(),"entry_id":entry.id,"entry_number":entry.entry_number,"series_id":series["id"],"recorded_by_user_id":actor,"recorded_by_impersonator_id":imp}),&mut changes).await?;
            for facts in rows {
                let control = facts["account_id"].as_i64().ok_or(Error::Invalid)?;
                let total=&facts["outstanding"];
                let description = format!(
                    "Opening {} {}",
                    if facts["item_kind"] == "customer_credit" { "customer credit" } else { "invoice" },
                    facts["document_reference"].as_str().unwrap_or("")
                );
                let old=store::save(sql,store::DOCUMENTS,tenant,company,None,json!({"kind":if facts["item_kind"]=="customer_credit" {"opening_credit"} else {"invoice"},"status":"draft","series_id":series["id"],"customer_id":facts["customer_id"],"currency_code":entry.currency_code,"issue_date":facts["document_date"],"due_date":if facts["due_date"].is_null() { &facts["document_date"] } else { &facts["due_date"] },"description":description,"subtotal":total,"tax_total":"0","total":total}),&mut changes).await?;
                let id=old["id"].as_i64().ok_or(Error::Invalid)?;
                store::save(sql,store::LINES,tenant,company,None,json!({"document_id":id,"line_number":1,"description":"Opening outstanding balance; no new supply","quantity":"1","unit_price":total,"amount":total,"account_id":control,"tax_amount":"0"}),&mut changes).await?;
                let number:i64=sql.query_one("SELECT COALESCE(max(document_number),0)+1 FROM finance_receivables_documents WHERE tenant_id=$1 AND company_id=$2 AND series_id=$3",&[&tenant,&company,&series["id"].as_i64().ok_or(Error::Invalid)?]).await?.get(0);
                let reference=format!("{}{:0width$}",series["prefix"].as_str().unwrap_or(""),number,width=series["number_width"].as_u64().unwrap_or(1) as usize);
                let name=facts["customer_name"].as_str().ok_or(Error::Invalid)?;
                let row=sql.query_one("UPDATE finance_receivables_documents d SET status='issued',document_number=$4,document_reference=$5,receivable_account_id=$6,party_name=$7,party_code=$12,issued_by_user_id=$8,issued_by_impersonator_id=$9,ledger_entry_id=$10,ledger_entry_number=$11 WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING to_jsonb(d)",&[&tenant,&company,&id,&number,&reference,&control,&name,&actor,&imp,&entry.id,&entry.entry_number,&facts["customer_code"].as_str().ok_or(Error::Invalid)?]).await?;
                let new:Value=row.get(0);changes.push(Change{kind:store::DOCUMENTS.kind.into(),id,old,new});
                store::save(sql,ITEM,tenant,company,None,json!({"batch_id":batch["id"],"source_id":facts["source_id"],"document_id":id,"source_facts":facts}),&mut changes).await?;
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
                    balance,
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
    s: &Scope,
    company: i64,
    document: i64,
    date: jiff::civil::Date,
) -> Result<()> {
    if c.query_opt("SELECT i.id FROM finance_receivables_opening_items i JOIN finance_receivables_opening_decisions d ON d.replaces_batch_id=i.batch_id AND d.tenant_id=$1 AND d.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3",&[&s.tenant(),&company,&document]).await?.is_some() {return Err(invalid("document_id","belongs to a replaced opening register; use the replacement record"));}
    if c.query_opt("SELECT i.id FROM finance_receivables_opening_items i JOIN finance_receivables_opening_batches b ON b.id=i.batch_id AND b.tenant_id=$1 AND b.company_id=$2 WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=$3 AND b.conversion_date>$4::text::date",&[&s.tenant(),&company,&document,&date.to_string()]).await?.is_some() {return Err(invalid("document_id","cannot be settled before the conversion date"));}
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
        return Err(ReceivableError::InvalidOption);
    }
    let maximum:i64=c.query_opt("SELECT COALESCE(max(id),0) FROM finance_receivables_opening_batches WHERE tenant_id=$1 AND company_id=$2",&[&s.tenant(),&company]).await?.ok_or(Error::Internal)?.get(0);
    let pin = through.unwrap_or(maximum);
    if pin > maximum {
        return Err(ReceivableError::InvalidOption);
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
