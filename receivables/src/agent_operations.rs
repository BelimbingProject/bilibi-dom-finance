//! H2 reach for Bilimbi.Finance.Receivables's facade-only actions. Transport,
//! consent codes and the outer audited commit belong to Base Agent API.
use super::*;
use crate::store::{self, Table, CUSTOMERS, HISTORY, SERIES, SETUPS, TAX};
use bilibi_agent_api::{
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock};

static OWNER: OnceLock<Arc<Receivables>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Receivables>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate receivables operation binding".into()))
}
fn owner() -> bilibi_agent_api::Result<&'static Receivables> {
    OWNER.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}

pub(super) fn failure(error: ReceivableError, children: &str) -> bilibi_agent_api::Error {
    use bilibi_agent_api::Error as E;
    let errors = match error {
        ReceivableError::Unauthorized | ReceivableError::Ledger(LedgerError::Unauthorized) => {
            return E::Forbidden;
        }
        ReceivableError::CompanyNotFound
        | ReceivableError::SeriesNotFound
        | ReceivableError::TaxCodeNotFound
        | ReceivableError::CustomerNotFound
        | ReceivableError::DocumentNotFound
        | ReceivableError::PaymentNotFound => return E::NotFound,
        ReceivableError::Failed(error) | ReceivableError::Ledger(LedgerError::Failed(error)) => {
            return error.into();
        }
        ReceivableError::Invalid(errors) => errors,
        ReceivableError::Ledger(LedgerError::Invalid(errors)) => errors
            .into_iter()
            .map(|e| FieldError {
                field: e.field.into(),
                line: e.line,
                message: e.message,
            })
            .collect(),
        other => {
            return E::InvalidInput(BTreeMap::from([("base".into(), vec![other.to_string()])]));
        }
    };
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in errors {
        let field = e.line.map_or_else(
            || e.field.clone(),
            |n| format!("{children}.{}.{}", n - 1, e.field),
        );
        fields.entry(field).or_default().push(e.message);
    }
    E::InvalidInput(fields)
}
fn id(input: &Value, field: &str) -> Result<i64> {
    input[field]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(ReceivableError::InvalidOption)
}

#[derive(Clone, Copy)]
enum Write {
    Configure,
    Code(Table, Option<&'static str>),
    Draft(bool),
    Discard,
    Issue,
    Payment,
    Reverse,
    BadDebt(&'static str),
}
impl Write {
    fn capability(self) -> &'static str {
        match self {
            Self::Configure => MANAGE_SETUP,
            Self::Code(t, _) => {
                if t.name == CUSTOMERS.name {
                    MANAGE_CUSTOMERS
                } else {
                    MANAGE_SETUP
                }
            }
            Self::Draft(_) | Self::Discard => MANAGE_DOCUMENTS,
            Self::Issue => ISSUE,
            Self::Payment | Self::Reverse => POST_PAYMENTS,
            Self::BadDebt("submit") => SUBMIT_WRITE_OFFS,
            Self::BadDebt(_) => APPROVE_WRITE_OFFS,
        }
    }
    fn summary(self) -> &'static str {
        match self {
            Self::BadDebt("submit") => {
                "Propose an invoice-linked bad-debt journal; nothing posts until approval. No tax relief is inferred."
            }
            Self::BadDebt("approve") => {
                "Approve and post the bad-debt journal atomically with the invoice write-off. Revenue and tax remain unchanged."
            }
            Self::BadDebt("reverse") => {
                "Reverse the owned write-off journal and restore the invoice balance before recording any recovery receipt."
            }
            Self::BadDebt(_) => {
                "Reject the proposed bad-debt journal without posting; retain its evidence and decision."
            }
            Self::Configure => {
                "Change document defaults or the printed layout; issued invoices keep their original control account."
            }
            Self::Code(t, None) if t.name == SERIES.name => {
                "Create a numbering series; code, kind, prefix and width are permanent."
            }
            Self::Code(t, Some(_)) if t.name == SERIES.name => {
                "Change the series name or active flag; preserve its numbering definition."
            }
            Self::Code(t, None) if t.name == TAX.name => {
                "Create a tax code; code and rate are permanent. Saved document snapshots do not change."
            }
            Self::Code(t, Some(_)) if t.name == TAX.name => {
                "Change tax code name, account or active flag; saved document snapshots do not change."
            }
            Self::Code(_, None) => {
                "Create the company when requested, link the Finance customer and record its current customer relationship in one transaction."
            }
            Self::Code(_, Some(_)) => {
                "Change the customer's payment terms or availability; already issued documents retain their party snapshot."
            }
            Self::Draft(false) => {
                "Keep an unnumbered draft with rounded line and tax snapshots; no entry posts."
            }
            Self::Draft(true) => {
                "Change the draft; omitted lines keep snapshots, an array replaces them, and an empty array clears them."
            }
            Self::Discard => {
                "Discard the draft permanently, retaining its lines and totals without numbering or posting."
            }
            Self::Issue => {
                "Issue this saved invoice or credit note permanently: freeze party and due date, number it and post its owned entry."
            }
            Self::Payment => {
                "Number and post a receipt allocated in full to these invoices using their original control accounts."
            }
            Self::Reverse => {
                "Correct the receipt once with a dated mirror entry; retain its original record and allocations."
            }
        }
    }
    fn error(self, error: ReceivableError) -> bilibi_agent_api::Error {
        failure(
            error,
            if matches!(self, Self::Payment) {
                "allocations"
            } else {
                "lines"
            },
        )
    }
    async fn review(
        self,
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
    ) -> Result<(Value, Option<posting::PreparedIssue>)> {
        let service = OWNER.get().ok_or(Error::Internal)?;
        let company = id(input, "company_id")?;
        service
            .join(client, scope, company, self.capability())
            .await?;
        let mut prepared_issue = None;
        let changes = match self {
            Self::BadDebt(action) => {
                service
                    .review_write_off(client, scope, company, action, input)
                    .await?
            }
            Self::Configure | Self::Code(_, _) => {
                let p = match self {
                    Self::Configure => service.prepare_setup(client, scope, company, input).await?,
                    Self::Code(table, field) => {
                        service
                            .prepare_code(
                                client,
                                scope,
                                company,
                                table,
                                field.map(|f| id(input, f)).transpose()?,
                                input,
                            )
                            .await?
                    }
                    _ => unreachable!(),
                };
                json!({"before":p.before,"after":p.after,"accounts":p.accounts,"target":p.target})
            }
            Self::Draft(update) => {
                let p = service
                    .prepare_draft(
                        client,
                        scope,
                        company,
                        update.then(|| id(input, "document_id")).transpose()?,
                        input,
                    )
                    .await?;
                json!({"before":p.before,"after":p.after,"replacement_lines_and_tax":p.replacement,"facts":p.facts})
            }
            Self::Discard => {
                let document = id(input, "document_id")?;
                documents::draft(client, scope.tenant(), company, document).await?;
                json!({"before":documents::read(client,scope.tenant(),company,document).await?,"status":"discarded"})
            }
            Self::Issue => {
                let p = service
                    .prepare_issue(client, scope, company, id(input, "document_id")?)
                    .await?;
                let posting = service
                    .ledger
                    .preview_post_entry_on(client, scope, company, &p.entry, &p.options)
                    .await?;
                let changes = json!({"after":p.after,"facts":p.facts,"posting":posting});
                prepared_issue = Some(p);
                changes
            }
            Self::Payment => {
                let p = service
                    .prepare_payment(client, scope, company, input)
                    .await?;
                let posting = service
                    .ledger
                    .preview_post_entry_on(client, scope, company, &p.entry, &p.options)
                    .await?;
                json!({"after":p.after,"allocations":p.allocations,"facts":p.facts,"posting":posting})
            }
            Self::Reverse => {
                let p = service
                    .prepare_reversal(client, scope, company, id(input, "payment_id")?, input)
                    .await?;
                let posting = service
                    .ledger
                    .preview_reverse_owned_entry_on(
                        client,
                        scope,
                        company,
                        p.before.ledger_entry_id,
                        &p.owner,
                        &p.reversal,
                    )
                    .await?;
                json!({"before":p.before,"after":p.after,"posting":posting})
            }
        };
        let mut review = json!({"company_id":company,"summary":self.summary(),"changes":changes});
        decimal_text(&mut review);
        Ok((review, prepared_issue))
    }
}

// SQL JSON snapshots retain arbitrary precision. Wire reviews use text for
// decimals too, including before images nested in facts and tax snapshots.
fn decimal_text(value: &mut Value) {
    match value {
        Value::Array(rows) => rows.iter_mut().for_each(decimal_text),
        Value::Object(fields) => {
            for (key, v) in fields {
                if matches!(
                    key.as_str(),
                    "rate"
                        | "quantity"
                        | "unit_price"
                        | "amount"
                        | "tax_rate"
                        | "tax_amount"
                        | "taxable_amount"
                        | "subtotal"
                        | "tax_total"
                        | "total"
                        | "outstanding"
                ) && v.is_number()
                {
                    *v = Value::String(v.to_string());
                } else {
                    decimal_text(v);
                }
            }
        }
        _ => (),
    }
}

// The pinned engine previews every step before running any. Fence again at
// execution so earlier plan steps cannot silently change later reviewed facts.
// Only hashes live in this transaction-local FIFO, never inputs or credentials.
pub(super) async fn fence(
    client: &dyn OperationClient,
    facts: &Value,
    consume: bool,
) -> bilibi_agent_api::Result<()> {
    let encoded = facts.to_string();
    let row = client.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'), COALESCE(NULLIF(current_setting('finance.receivables_reviews',true),''),'[]')::jsonb", &[&encoded]).await?.ok_or(Error::Internal)?;
    let digest: String = row.get(0);
    let mut pending: Vec<String> =
        serde_json::from_value(row.get(1)).map_err(|_| Error::Internal)?;
    if consume {
        if pending.first() != Some(&digest) {
            return Err(bilibi_agent_api::Error::Backend(
                bilibi_database::Error::Conflict,
            ));
        }
        pending.remove(0);
    } else {
        pending.push(digest);
    }
    client
        .query_opt(
            "SELECT set_config('finance.receivables_reviews',$1,true)",
            &[&json!(pending).to_string()],
        )
        .await?;
    Ok(())
}
impl Write {
    /// The page a person opens to see what this write changed: the record's
    /// own page, or the settings page for setup, series and tax codes.
    fn page(&self, record: &Value) -> Value {
        use crate::web::{CUSTOMERS as CUSTOMER_PAGES, INVOICES, RECEIPTS, SETTINGS};
        let record_page = |base: &str| json!(format!("{base}/{}", record["id"]));
        match self {
            Self::BadDebt(_) => json!(format!("{INVOICES}/{}", record["document_id"])),
            Self::Configure => json!(SETTINGS),
            Self::Code(table, _) if table.name == CUSTOMERS.name => record_page(CUSTOMER_PAGES),
            Self::Code(..) => json!(SETTINGS),
            Self::Draft(_) | Self::Discard | Self::Issue => record_page(INVOICES),
            Self::Payment | Self::Reverse => record_page(RECEIPTS),
        }
    }
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (review, _) = self
                .review(client, scope, input)
                .await
                .map_err(|e| self.error(e))?;
            fence(
                client,
                &json!({"action":self.summary(),"input":input,"review":review}),
                false,
            )
            .await?;
            Ok(review.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = owner()?;
            let company = id(input, "company_id").map_err(|e| self.error(e))?;
            let (review, prepared_issue) = self
                .review(tx.sql(), scope, input)
                .await
                .map_err(|e| self.error(e))?;
            fence(
                tx.sql(),
                &json!({"action":self.summary(),"input":input,"review":review}),
                true,
            )
            .await?;
            let record: Result<Value> = async {
                Ok(match *self {
                    Self::BadDebt(action) => json!(
                        service
                            .write_off_on(tx, scope, company, action, input)
                            .await?
                    ),
                    Self::Configure => {
                        json!(service.configure_on(tx, scope, company, input).await?)
                    }
                    Self::Code(table, field) => {
                        let id = field.map(|f| id(input, f)).transpose()?;
                        if table.name == CUSTOMERS.name {
                            json!(
                                service
                                    .save_customer_on(tx, scope, company, id, input)
                                    .await?
                            )
                        } else {
                            let row = service
                                .save_code_on(tx, scope, company, table, id, input)
                                .await?;
                            if table.name == TAX.name {
                                json!(store::decode::<TaxCode>(row)?)
                            } else {
                                json!(store::decode::<Series>(row)?)
                            }
                        }
                    }
                    Self::Draft(update) => json!(
                        service
                            .save_draft_on(
                                tx,
                                scope,
                                company,
                                update.then(|| id(input, "document_id")).transpose()?,
                                input
                            )
                            .await?
                    ),
                    Self::Discard => json!(
                        service
                            .discard_draft_on(tx, scope, company, id(input, "document_id")?)
                            .await?
                    ),
                    Self::Issue => json!(
                        service
                            .issue_prepared_on(
                                tx,
                                scope,
                                company,
                                id(input, "document_id")?,
                                prepared_issue.ok_or(Error::Internal)?
                            )
                            .await?
                    ),
                    Self::Payment => {
                        json!(service.record_payment_on(tx, scope, company, input).await?)
                    }
                    Self::Reverse => json!(
                        service
                            .reverse_payment_on(tx, scope, company, id(input, "payment_id")?, input)
                            .await?
                    ),
                })
            }
            .await;
            let record = record.map_err(|e| self.error(e))?;
            let path = self.page(&record);
            Ok(json!({"record":record,"path":path}).into())
        })
    }
}

enum Read {
    Setup,
    History,
    Codes(Table),
    Customer,
    Document,
    Documents,
    DocumentPage,
    DocumentSummary,
    Payment,
    Payments,
    Aging,
    WriteOffAccounts,
    BookFigures,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = owner()?;
            let company = id(input, "company_id").map_err(|e| failure(e, "lines"))?;
            service
                .authorize_on(client, scope, company, VIEW)
                .await
                .map_err(|e| failure(e, "lines"))?;
            let tenant = scope.tenant();
            let limit = input["page_size"].as_i64().unwrap_or(100).clamp(1, 100);
            let after = input["after"].as_i64().unwrap_or(0);
            let result:Result<Value> = async {
                Ok(match *self {
                    // Only how an amount is written (currency and decimal places), decided
                    // under Receivables view, so a page can format money without ledger view.
                    Self::BookFigures => json!({"book":service.ledger.book_figures_on(client,scope,company,VIEW).await?}),
                    Self::WriteOffAccounts => json!({"accounts":write_offs::eligible_offsets(client,service,scope,company).await?}),
                    Self::Setup => json!({"setup":store::find(client,SETUPS,tenant,company,None).await?.map(store::decode::<Setup>).transpose()?}),
                    Self::Codes(table) => {
                        let customer=table.name==CUSTOMERS.name;
                        let mut rows:Vec<Value> = if customer {
                            client.query("SELECT to_jsonb(r) FROM finance_receivables_customers r WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4",&[&tenant,&company,&after,&(limit+1)]).await?.iter().map(|r|r.get(0)).collect()
                        } else {
                            let cursor=input["after_code"].as_str();
                            let sql=format!("SELECT to_jsonb(r) FROM {} r WHERE tenant_id=$1 AND company_id=$2 AND ($3::text IS NULL OR code>$3) ORDER BY code LIMIT $4",table.name);
                            client.query(&sql,&[&tenant,&company,&cursor,&(limit+1)]).await?.iter().map(|r|r.get(0)).collect()
                        };
                        let more=rows.len()>limit as usize;rows.truncate(limit as usize);
                        let next=more.then(||rows.last().map(|r|r[if customer {"id"} else {"code"}].clone())).flatten();
                        let mut records=Vec::with_capacity(rows.len());
                        for row in rows {records.push(if customer {json!(service.customer(client,scope,row).await?)} else if table.name==TAX.name {json!(store::decode::<TaxCode>(row)?)} else {json!(store::decode::<Series>(row)?)});}
                        if customer {json!({"records":records,"next_after_id":next})} else {json!({"records":records,"next_after_code":next})}
                    }
                    Self::Aging => {
                        let as_of: jiff::civil::Date = input["as_of"].as_str().ok_or(ReceivableError::InvalidOption)?.parse().map_err(|_| invalid("as_of", "is not a date"))?;
                        let mut options = AgingOptions {through_entry_number:input["through_entry_number"].as_i64(),through_opening_batch_id:input["through_opening_batch_id"].as_i64(),through_settlement_number:input["through_settlement_number"].as_i64(),..Default::default()};
                        if let Some(days) = input["bucket_days"].as_array() {
                            options.bucket_days = days.iter().filter_map(Value::as_i64).collect();
                        }
                        // A read holds no company lock, so the facade's own connection is safe here.
                        json!(service.aged_receivables(scope, company, as_of, &options).await?)
                    }
                    Self::Customer => json!(service.get_customer(scope, company, id(input,"customer_id")?).await?),
                    Self::Document => {
                        if !input["as_of"].is_null()||["through_entry_number","through_opening_batch_id","through_settlement_number"].iter().any(|key|!input[key].is_null()) {
                            service.document_at(scope,company,id(input,"document_id")?,input).await?
                        } else {
                            let mut document = documents::read(client,tenant,company,id(input,"document_id")?).await?;
                            let asked = [(document.id, document.kind.as_str(), document.status.as_str(), document.opening.is_some())];
                            document.fiscal = service.fiscal_on(client, scope, company, &asked).await?.0.pop().unwrap_or_default();
                            json!(document)
                        }
                    },
                    Self::DocumentPage => crate::document_page::page(client, service, scope, company, input).await?,
                    Self::DocumentSummary => crate::document_page::summary(client, tenant, company, input).await?,
                    Self::Payment => json!(posting::read_payment(client,tenant,company,id(input,"payment_id")?).await?),
                    Self::History | Self::Documents | Self::Payments => {
                        let customer = input["customer_id"].as_i64();
                        let kind = input["kind"].as_str();
                        let status = input["status"].as_str();
                        let source = input["source"].as_str();
                        let (sql,params): (String,Vec<&(dyn tokio_postgres::types::ToSql+Sync)>) = match self {
                            Self::History => (format!("SELECT to_jsonb(r) FROM {} r WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4",HISTORY.name),vec![&tenant,&company,&after,&(limit)]),
                            Self::Documents => (format!("{} AND d.id>$3 AND ($4::bigint IS NULL OR d.customer_id=$4) AND ($5::text IS NULL OR d.kind=$5) AND ($6::text IS NULL OR d.status=$6) AND ($8::text IS NULL OR finance_receivables_document_source($1,$2,d.id)=$8) ORDER BY d.id LIMIT $7",documents::READ),vec![&tenant,&company,&after,&customer,&kind,&status,&limit,&source]),
                            _ => (format!("{} AND p.id>$3 AND ($4::bigint IS NULL OR p.customer_id=$4) ORDER BY p.id LIMIT $5",posting::PAYMENT_READ),vec![&tenant,&company,&after,&customer,&limit]),
                        };
                        let records:Vec<Value> = client.query(&sql,&params).await?.iter().map(|r| {
                            if matches!(self,Self::History) {store::decode::<SetupChange>(r.get(0)).map(|r|json!(r))}
                            else if matches!(self,Self::Documents) {store::decode::<Document>(r.get(0)).map(|r|json!(r))}
                            else {store::decode::<Payment>(r.get(0)).map(|r|json!(r))}
                        }).collect::<std::result::Result<_,_>>()?;
                        json!({"next_after":records.last().map(|r|r["id"].clone()),"records":records})
                    }
                })
            }.await;
            Ok(result.map_err(|e| failure(e, "lines"))?.into())
        })
    }
}

fn identifier() -> Value {
    json!({"type":"integer","minimum":1})
}
fn string(max: usize) -> Value {
    json!({"type":"string","maxLength":max})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
fn day() -> Value {
    json!({"type":"string","format":"date"})
}
fn choice(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn object(required: &[&str], properties: Value) -> Value {
    json!({"type":"object","required":required,"properties":properties})
}
fn schema(required: &[&str], properties: Value) -> Value {
    let mut properties = properties.as_object().cloned().unwrap_or_default();
    properties.insert("company_id".into(), identifier());
    let required: Vec<_> = std::iter::once("company_id")
        .chain(required.iter().copied())
        .collect();
    object(&required, Value::Object(properties))
}
fn complete_pins(value: Value) -> Value {
    // The operation schema dialect has no cross-field combinators. The facade
    // validates that callers supply all three accounting pins or none.
    value
}
fn output() -> Value {
    json!({"type":"object","description":"Receivables record, cursor page, or deterministic review facts. Exact decimals are strings; dates are ISO. Committed path is the page of the record written, or the settings page for setup, series and tax codes."})
}
fn target() -> Value {
    schema(&[], json!({}))
}
fn configure() -> Value {
    schema(
        &[],
        json!({"receivable_account_id":identifier(),"income_account_id":identifier(),"invoice_layout":object(&[], json!({"company_name":string(200),"registration_number":string(100),"address":string(2000),"contact":string(500),"footer":string(4000)}))}),
    )
}
fn series_create() -> Value {
    schema(
        &["code", "name", "kind"],
        json!({"code":string(50),"name":string(200),"kind":choice(Receivables::series_kinds()),"prefix":string(20),"number_width":{"type":"integer","minimum":1,"maximum":12},"active":boolean()}),
    )
}
fn series_update() -> Value {
    schema(
        &["series_id"],
        json!({"series_id":identifier(),"name":string(200),"active":boolean()}),
    )
}
fn tax_create() -> Value {
    schema(
        &["code", "name", "rate", "account_id"],
        json!({"code":string(50),"name":string(200),"rate":string(40),"account_id":identifier(),"active":boolean()}),
    )
}
fn tax_update() -> Value {
    schema(
        &["tax_code_id"],
        json!({"tax_code_id":identifier(),"name":string(200),"account_id":identifier(),"active":boolean()}),
    )
}
fn customer_own_fields() -> Value {
    json!({"payment_terms_days":{"type":"integer","minimum":0,"maximum":3650},"active":boolean()})
}
/// A new Core company to create and link in the same transaction.
fn company_creation() -> Value {
    object(
        &["name"],
        json!({
            "name":string(160),"code":string(255),"status":choice(&["active","pending"]),
            "legal_name":string(255),"registration_number":string(255),"tax_id":string(64),
            "email":string(254),"phone":string(64),"website":string(255),"jurisdiction":string(2),
            "parent_id":identifier(),"legal_entity_type_id":identifier(),
            "create_anyway":object(&["candidate_ids","reason"],json!({"candidate_ids":{"type":"array","items":identifier()},"reason":string(500)}))
        }),
    )
}
fn person_creation() -> Value {
    object(
        &["name", "kind"],
        json!({"name":string(160),"kind":choice(&["human","agent"]),"email":string(254),"phone":string(64)}),
    )
}
fn customer_create() -> Value {
    let mut p = customer_own_fields();
    p["record_kind"] = choice(&["company", "person"]);
    p["record_id"] = identifier();
    p["new_company"] = company_creation();
    p["new_person"] = person_creation();
    schema(&[], p)
}
fn customer_update() -> Value {
    let mut p = customer_own_fields();
    p["customer_id"] = identifier();
    schema(&["customer_id"], p)
}
fn customers() -> Value {
    schema(
        &[],
        json!({"after":identifier(),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn draft_fields() -> Value {
    // Empty text clears an optional date; the facade validates ISO dates.
    // Required settlement dates retain the strict date schema.
    json!({"series_id":identifier(),"customer_id":identifier(),"credited_document_id":identifier(),"issue_date":string(10),"due_date":string(10),"description":string(500),"lines":{"type":"array","items":object(&["description","quantity","unit_price"],json!({"description":string(500),"quantity":string(40),"unit_price":string(40),"account_id":identifier(),"tax_code_id":identifier()}))}})
}
fn draft_create() -> Value {
    let mut p = draft_fields();
    p["kind"] = choice(Receivables::document_kinds());
    schema(&["kind", "series_id", "customer_id"], p)
}
fn draft_update() -> Value {
    let mut p = draft_fields();
    p["document_id"] = identifier();
    schema(&["document_id"], p)
}
fn document() -> Value {
    schema(&["document_id"], json!({"document_id":identifier()}))
}
fn payment() -> Value {
    schema(&["payment_id"], json!({"payment_id":identifier()}))
}
fn customer() -> Value {
    schema(&["customer_id"], json!({"customer_id":identifier()}))
}
fn payment_record() -> Value {
    schema(
        &[
            "series_id",
            "customer_id",
            "payment_date",
            "amount",
            "deposit_account_id",
            "allocations",
        ],
        json!({"series_id":identifier(),"customer_id":identifier(),"payment_date":day(),"amount":string(40),"deposit_account_id":identifier(),"reference":string(100),"description":string(500),"allocations":{"type":"array","items":object(&["document_id","amount"],json!({"document_id":identifier(),"amount":string(40)}))}}),
    )
}
fn payment_reverse() -> Value {
    schema(
        &["payment_id", "reversal_date", "reason"],
        json!({"payment_id":identifier(),"reversal_date":day(),"reason":string(500)}),
    )
}
fn write_off_submit() -> Value {
    schema(
        &[
            "document_id",
            "entry_date",
            "amount",
            "offset_account_id",
            "reason",
            "evidence",
        ],
        json!({"document_id":identifier(),"entry_date":day(),"amount":string(40),"offset_account_id":identifier(),"reason":string(500),"evidence":string(500)}),
    )
}
fn write_off_decide() -> Value {
    schema(
        &["write_off_id", "reason"],
        json!({"write_off_id":identifier(),"reason":string(500)}),
    )
}
fn write_off_reverse() -> Value {
    schema(
        &["write_off_id", "reason", "reversal_date"],
        json!({"write_off_id":identifier(),"reason":string(500),"reversal_date":day()}),
    )
}
fn codes() -> Value {
    schema(
        &[],
        json!({"after_code":string(50),"page_size":{"type":"integer","minimum":1,"maximum":100}}),
    )
}
fn page_fields() -> Value {
    json!({"after":identifier(),"page_size":{"type":"integer","minimum":1,"maximum":100}})
}
fn history() -> Value {
    schema(&[], page_fields())
}
fn documents() -> Value {
    let mut p = page_fields();
    p["source"] = choice(&["ordinary", "opening", "sales_order"]);
    p["kind"] = choice(&["invoice", "credit_note", "opening_credit"]);
    p["status"] = choice(&["draft", "issued", "discarded"]);
    p["customer_id"] = identifier();
    schema(&[], p)
}
fn document_page() -> Value {
    schema(
        &[],
        json!({
            "source":choice(&["ordinary","opening","sales_order"]),
            "kind":choice(&["invoice","credit_note","opening_credit"]),
            "status":{"type":"string","enum":["draft","issued","unpaid","overdue","paid","discarded"],"description":"unpaid: issued invoices still owed, overdue ones included. overdue: unpaid and due before as_of. paid: issued invoices with nothing owed."},
            "as_of":{"type":"string","format":"date","description":"The day overdue is counted against: today where the company is. Defaults to today in UTC."},
            "customer_id":identifier(),
            "search":string(255),
            "sort":choice(&["number","issue_date","due_date","total"]),
            "direction":choice(&["asc","desc"]),
            "page":{"type":"integer","minimum":1},
            "page_size":{"type":"integer","enum":[25,50,100]}
        }),
    )
}
fn document_summary() -> Value {
    schema(
        &[],
        json!({"as_of":{"type":"string","format":"date","description":"The day overdue is counted against: today where the company is. Defaults to today in UTC."}}),
    )
}
fn aging() -> Value {
    complete_pins(schema(
        &["as_of"],
        json!({"as_of":day(),"through_entry_number":{"type":"integer","minimum":0},"through_opening_batch_id":{"type":"integer","minimum":0},"through_settlement_number":{"type":"integer","minimum":0},"bucket_days":{"type":"array","items":{"type":"integer","minimum":1,"maximum":36500}}}),
    ))
}
fn payments() -> Value {
    let mut p = page_fields();
    p["customer_id"] = identifier();
    schema(&[], p)
}

inventory::submit! { Contribution::new(DEFINITIONS,"finance/receivables.operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {
    operations:&[
        Operation {key:"finance.receivables.write_off.accounts",title:"Read bad-debt journal accounts",summary:"Active allowance assets and expense accounts, excluding every current or historical receivables control and every account receipts are paid into.",keywords:&[],capability:VIEW,input:target,output,handler:Handler::Read(&Read::WriteOffAccounts),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.write_off.submit",title:"Propose a bad-debt journal",summary:"Propose a dated invoice write-off against the book's allowance or expense account, with reason and evidence. No posting until approval.",keywords:&[],capability:SUBMIT_WRITE_OFFS,input:write_off_submit,output,handler:Handler::SafeWrite(&Write::BadDebt("submit")),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.write_off.approve",title:"Approve a bad-debt journal",summary:"Require a different reviewer, then post the owned journal and reduce the invoice balance atomically. No tax relief or filing.",keywords:&[],capability:APPROVE_WRITE_OFFS,input:write_off_decide,output,handler:Handler::SafeWrite(&Write::BadDebt("approve")),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.write_off.reject",title:"Reject a bad-debt journal",summary:"Keep the proposed journal and rejection reason without posting.",keywords:&[],capability:APPROVE_WRITE_OFFS,input:write_off_decide,output,handler:Handler::SafeWrite(&Write::BadDebt("reject")),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.write_off.reverse",title:"Reverse a bad-debt journal",summary:"Restore the invoice balance with an owned mirror journal before recording any recovery receipt. Also requires a different reviewer.",keywords:&[],capability:APPROVE_WRITE_OFFS,input:write_off_reverse,output,handler:Handler::SafeWrite(&Write::BadDebt("reverse")),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.book_figures.get",title:"Read how amounts are written",summary:"Read the company's currency code and decimal places so amounts display correctly. Needs only receivables view, not ledger view; nothing else of the book is returned.",keywords:&[],capability:VIEW,input:target,output,handler:Handler::Read(&Read::BookFigures),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.setup.get",title:"Read receivables setup",summary:"Read default accounts and the invoice layout; setup is null until configured.",keywords:&[],capability:VIEW,input:target,output,handler:Handler::Read(&Read::Setup),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.setup.changes",title:"Read setup history",summary:"Read account-default changes in ascending identifier order.",keywords:&[],capability:VIEW,input:history,output,handler:Handler::Read(&Read::History),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.series.list",title:"List numbering series",summary:"Read active and inactive series in code order with a bounded cursor.",keywords:&[],capability:VIEW,input:codes,output,handler:Handler::Read(&Read::Codes(SERIES)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.tax_code.list",title:"List tax codes",summary:"Read code, frozen rate, posting account and active flag in code order.",keywords:&[],capability:VIEW,input:codes,output,handler:Handler::Read(&Read::Codes(TAX)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.customer.list",title:"List customers",summary:"Read customers by ID cursor: Core Company or Person code and facts, payment terms and availability.",keywords:&[],capability:VIEW,input:customers,output,handler:Handler::Read(&Read::Codes(CUSTOMERS)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.customer.get",title:"Read a customer",summary:"Read one customer with its linked company (name, status, tax ID, registration number, email, phone as you may see them) and its own payment terms and availability.",keywords:&[],capability:VIEW,input:customer,output,handler:Handler::Read(&Read::Customer),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.list",title:"List receivables documents",summary:"Read drafts, issued invoices and credit notes, or discarded drafts by identifier with optional filters.",keywords:&[],capability:VIEW,input:documents,output,handler:Handler::Read(&Read::Documents),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.page",title:"Read invoices page",summary:"Read a bounded invoices page after applying source, type, status, customer and search filters, stable sorting, count and offset in SQL. Each issued invoice says where its money is: payment_status (awaiting_payment, part_paid, paid) and days_overdue.",keywords:&["opening","overdue","unpaid","paid"],capability:VIEW,input:document_page,output,handler:Handler::Read(&Read::DocumentPage),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.summary",title:"Read what customers owe in three figures",summary:"Read the count and amount of draft invoices, of issued invoices still owed, and of those past their due date with the oldest days late.",keywords:&["overdue","unpaid","awaiting payment"],capability:VIEW,input:document_summary,output,handler:Handler::Read(&Read::DocumentSummary),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.get",title:"Read a receivables document",summary:"Read saved lines, tax and party snapshots, and either current or pinned outstanding and activity.",keywords:&[],capability:VIEW,input:dated_document,output,handler:Handler::Read(&Read::Document),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.payment.list",title:"List receipts",summary:"Read numbered receipts with allocations and reversal, by identifier and optional customer.",keywords:&[],capability:VIEW,input:payments,output,handler:Handler::Read(&Read::Payments),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.payment.get",title:"Read a receipt",summary:"Read the original receipt, its allocations and optional dated reversal.",keywords:&[],capability:VIEW,input:payment,output,handler:Handler::Read(&Read::Payment),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.aging.get",title:"Read receivables aging",summary:"Read customer residuals at a date and posting pin. Invoices age from their due dates; unapplied credits age from their source dates. Also requires ledger view.",keywords:&[],capability:VIEW,input:aging,output,handler:Handler::Read(&Read::Aging),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.setup.configure",title:"Configure receivables",summary:"Choose default accounts or update invoice layout text. Also requires ledger view.",keywords:&[],capability:MANAGE_SETUP,input:configure,output,handler:Handler::SafeWrite(&Write::Configure),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.series.create",title:"Create a numbering series",summary:"Create an invoice, credit note or payment series with a unique code and prefix.",keywords:&[],capability:MANAGE_SETUP,input:series_create,output,handler:Handler::SafeWrite(&Write::Code(SERIES,None)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.series.update",title:"Change a numbering series",summary:"Change name or active flag; never change its code, kind, prefix or width.",keywords:&[],capability:MANAGE_SETUP,input:series_update,output,handler:Handler::SafeWrite(&Write::Code(SERIES,Some("series_id"))),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.tax_code.create",title:"Create a tax code",summary:"Create a percentage tax code on an active liability account. Also requires ledger view.",keywords:&[],capability:MANAGE_SETUP,input:tax_create,output,handler:Handler::SafeWrite(&Write::Code(TAX,None)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.tax_code.update",title:"Change a tax code",summary:"Change name, account or active flag, preserving code and rate. Also requires ledger view.",keywords:&[],capability:MANAGE_SETUP,input:tax_update,output,handler:Handler::SafeWrite(&Write::Code(TAX,Some("tax_code_id"))),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.customer.create",title:"Create a customer",summary:"Add a Core Company or Person as a customer with payment terms and availability. Supply record_kind and record_id, new_company, or new_person. Use the Core party code everywhere. Party creation, permanent Finance link and customer relationship commit atomically.",keywords:&["company","party"],capability:MANAGE_CUSTOMERS,input:customer_create,output,handler:Handler::SafeWrite(&Write::Code(CUSTOMERS,None)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.customer.update",title:"Change a customer",summary:"Change payment terms or availability; The linked Core party's facts are refused. Code, link and issued-document snapshots never change.",keywords:&[],capability:MANAGE_CUSTOMERS,input:customer_update,output,handler:Handler::SafeWrite(&Write::Code(CUSTOMERS,Some("customer_id"))),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.draft.create",title:"Create a receivables draft",summary:"Save an unnumbered invoice or credit note with derived amounts. Also requires ledger view and configured setup.",keywords:&[],capability:MANAGE_DOCUMENTS,input:draft_create,output,handler:Handler::SafeWrite(&Write::Draft(false)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.draft.update",title:"Change a receivables draft",summary:"Change header or atomically replace lines; omit lines to keep snapshots. Also requires ledger view.",keywords:&[],capability:MANAGE_DOCUMENTS,input:draft_update,output,handler:Handler::SafeWrite(&Write::Draft(true)),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.draft.discard",title:"Discard a receivables draft",summary:"Keep the draft and its lines permanently as discarded, without numbering or posting.",keywords:&[],capability:MANAGE_DOCUMENTS,input:document,output,handler:Handler::SafeWrite(&Write::Discard),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.document.issue",title:"Issue an invoice or credit note",summary:"Freeze the saved draft, number it and post its owned entry atomically. Also requires ledger view and entries.post.",keywords:&[],capability:ISSUE,input:document,output,handler:Handler::SafeWrite(&Write::Issue),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.payment.record",title:"Record a receipt",summary:"Number and post a fully allocated payment using each invoice's original control. Also requires ledger view and entries.post.",keywords:&[],capability:POST_PAYMENTS,input:payment_record,output,handler:Handler::SafeWrite(&Write::Payment),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.receivables.payment.reverse",title:"Reverse a receipt",summary:"Correct a payment once with a dated owned mirror entry and reason. Also requires ledger entries.post, not ledger view.",keywords:&[],capability:POST_PAYMENTS,input:payment_reverse,output,handler:Handler::SafeWrite(&Write::Reverse),channels:Channels::Every,step_up:&[]},
    ],
    guides:&[Guide {key:"finance.receivables",title:"Receivables drafts, issuance and receipts",summary:"Configure, draft, review and issue invoices or credit notes; settle and correct receipts without editing posted evidence.",keywords:&[],capability:VIEW,body:include_str!("../guide.md")}],
}) }

fn dated_document() -> Value {
    let mut s = document();
    s["properties"]["as_of"] = json!({"type":"string","format":"date"});
    for key in [
        "through_entry_number",
        "through_opening_batch_id",
        "through_settlement_number",
    ] {
        s["properties"][key] = json!({"type":"integer","minimum":0});
    }
    complete_pins(s)
}
