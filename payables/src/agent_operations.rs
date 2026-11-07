//! H3 reach for Bilimbi.Finance.Payables' facade-only workflows. The host
//! owns authentication, consent-code binding and the outer audited commit.
use crate::*;
use bilibi_agent_api::{
    Channels, Definitions, Guide, Handler, Operation, OperationFuture, ReadHandler, WriteHandler,
    DEFINITIONS,
};
use bilibi_finance_ledger::MinorUnits;
use bilibi_module_registry::{Contribution, Provenance, RegistryError};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock};

static OWNER: OnceLock<Arc<Payables>> = OnceLock::new();
pub(super) fn bind(owner: Arc<Payables>) -> bilibi_module_registry::Result<()> {
    OWNER
        .set(owner)
        .map_err(|_| RegistryError::Service("duplicate payables operation binding".into()))
}
fn owner() -> Result<&'static Payables> {
    OWNER
        .get()
        .map(|p| p.as_ref())
        .ok_or_else(|| Error::Internal.into())
}
pub(super) async fn authorize_on(
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    cap: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(c, s, company, cap)
        .await
        .map(|_| ())
        .map_err(refusal)
}
fn fields(errors: Vec<FieldError>, array: &str) -> bilibi_agent_api::Error {
    let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in errors {
        let path = e.line.map_or_else(
            || e.field.into(),
            |i| format!("{array}.{}.{}", i - 1, e.field),
        );
        result.entry(path).or_default().push(e.message);
    }
    bilibi_agent_api::Error::InvalidInput(result)
}
pub(super) fn error(e: PayablesError, array: &str) -> bilibi_agent_api::Error {
    use bilibi_agent_api::Error as E;
    match e {
        PayablesError::Unauthorized | PayablesError::Ledger(LedgerError::Unauthorized) => {
            E::Forbidden
        }
        PayablesError::CompanyNotFound
        | PayablesError::DocumentNotFound
        | PayablesError::TaxCodeNotFound
        | PayablesError::PaymentRequestNotFound
        | PayablesError::PaymentNotFound
        | PayablesError::Ledger(
            LedgerError::CompanyNotFound
            | LedgerError::AccountNotFound
            | LedgerError::EntryNotFound,
        ) => E::NotFound,
        PayablesError::Invalid(es) => fields(es, array),
        PayablesError::Ledger(LedgerError::Invalid(es)) => fields(es, "lines"),
        PayablesError::Failed(e) | PayablesError::Ledger(LedgerError::Failed(e)) => e.into(),
        e => E::InvalidInput(BTreeMap::from([("base".into(), vec![e.to_string()])])),
    }
}
fn id(v: &Value, field: &'static str) -> Result<i64> {
    v[field]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(field, "is required"))
}
fn text<'a>(v: &'a Value, field: &'static str) -> Result<&'a str> {
    v[field]
        .as_str()
        .ok_or_else(|| invalid(field, "is required"))
}
fn opt(v: &Value, field: &str) -> Option<String> {
    v[field].as_str().map(str::to_owned)
}
fn date(v: &Value, field: &'static str) -> Result<Date> {
    text(v, field)?
        .parse()
        .map_err(|_| invalid(field, "has invalid format"))
}
fn money(v: &Value, field: &'static str, units: MinorUnits) -> Result<Money> {
    Money::parse(text(v, field)?, units).map_err(|e| invalid(field, e.to_string()))
}
fn tolerance(
    v: &Value,
    name: &'static str,
    clear: &str,
    previous: Option<Tolerance>,
) -> Result<Option<Tolerance>> {
    if v[clear].as_bool() == Some(true) {
        if v.get(name).is_some() {
            return Err(invalid(name, "cannot be set and cleared together"));
        }
        return Ok(None);
    }
    let Some(t) = v.get(name) else {
        return Ok(previous);
    };
    let kind = match text(t, "kind")? {
        "percent" => ToleranceKind::Percent,
        "amount" => ToleranceKind::Amount,
        _ => return Err(invalid(name, "is invalid")),
    };
    let value =
        Decimal::from_str_exact(text(t, "value")?).map_err(|_| invalid(name, "is invalid"))?;
    Ok(Some(Tolerance { kind, value }))
}
async fn settings_input(
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    v: &Value,
) -> Result<SettingsInput> {
    let previous = store::settings(c, s.tenant(), company)
        .await?
        .map(|(_, p)| p);
    Ok(SettingsInput {
        settings: Settings {
            payable_account_id: v
                .get("payable_account_id")
                .map(|_| id(v, "payable_account_id"))
                .transpose()?
                .or(previous.as_ref().map(|p| p.payable_account_id))
                .ok_or_else(|| invalid("payable_account_id", "is required"))?,
            quantity_tolerance: tolerance(
                v,
                "quantity_tolerance",
                "clear_quantity_tolerance",
                previous.as_ref().and_then(|p| p.quantity_tolerance.clone()),
            )?,
            price_tolerance: tolerance(
                v,
                "price_tolerance",
                "clear_price_tolerance",
                previous.and_then(|p| p.price_tolerance),
            )?,
        },
        reason: opt(v, "reason"),
    })
}
async fn tax_input(
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    v: &Value,
    update: bool,
) -> Result<TaxCodeDetails> {
    let old = if update {
        Some(
            store::taxes(c, s.tenant(), company)
                .await?
                .into_iter()
                .find(|t| Some(t.id) == v["tax_code_id"].as_i64())
                .ok_or(PayablesError::TaxCodeNotFound)?,
        )
    } else {
        None
    };
    Ok(TaxCodeDetails {
        name: opt(v, "name")
            .or_else(|| old.as_ref().map(|t| t.name.clone()))
            .ok_or_else(|| invalid("name", "is required"))?,
        rate: opt(v, "rate")
            .or_else(|| old.as_ref().map(|t| t.rate.to_string()))
            .ok_or_else(|| invalid("rate", "is required"))?,
        account_id: v
            .get("account_id")
            .map(|_| id(v, "account_id"))
            .transpose()?
            .or(old.as_ref().map(|t| t.account_id))
            .ok_or_else(|| invalid("account_id", "is required"))?,
        active: v["active"]
            .as_bool()
            .unwrap_or_else(|| old.as_ref().is_none_or(|t| t.active)),
        reason: opt(v, "reason"),
    })
}
async fn vendor_input(
    c: &dyn OperationClient,
    s: &Scope,
    company: i64,
    v: &Value,
    update: bool,
) -> Result<VendorInput> {
    if v.get("code").is_some() {
        return Err(invalid(
            "code",
            "is read from the Core party and cannot be supplied",
        ));
    }
    let mut input = if update {
        let old = owner()?
            .vendor_on(c, s, company, id(v, "vendor_id")?)
            .await?;
        json!({"record_kind":old.record_kind,"record_id":old.party.id,"payment_terms_days":old.payment_terms_days,"payable_account_id":old.payable_account_id,"expense_account_id":old.expense_account_id,"tax_code_id":old.tax_code_id,"active":old.active})
    } else {
        json!({"payment_terms_days":0,"active":true})
    };
    for (key, value) in v.as_object().ok_or(PayablesError::InvalidOption)? {
        if !matches!(
            key.as_str(),
            "company_id"
                | "vendor_id"
                | "clear_payable_account"
                | "clear_expense_account"
                | "clear_tax_code"
        ) {
            input[key] = value.clone();
        }
    }
    for (field, clear) in [
        ("payable_account_id", "clear_payable_account"),
        ("expense_account_id", "clear_expense_account"),
        ("tax_code_id", "clear_tax_code"),
    ] {
        if v[clear].as_bool() == Some(true) {
            if v.get(field).is_some() {
                return Err(invalid(clear, format!("cannot be combined with {field}")));
            }
            input[field] = Value::Null;
        }
    }
    serde_json::from_value(input).map_err(|_| invalid("base", "vendor settings are invalid"))
}
fn vendor_schema(update: bool) -> Value {
    let mut props = json!({"payment_terms_days":{"type":"integer","minimum":0,"maximum":3650},"payable_account_id":identifier(),"expense_account_id":identifier(),"tax_code_id":identifier(),"clear_payable_account":boolean(),"clear_expense_account":boolean(),"clear_tax_code":boolean(),"active":boolean()});
    let mut required = vec![];
    if !update {
        props["record_kind"] = json!({"type":"string","enum":["company","person"]});
        props["record_id"] = identifier();
        props["new_company"] = json!({"type":"object","required":["name"],"properties":{"name":string(160),"registration_number":string(255),"tax_id":string(64),"email":string(254),"phone":string(64),"create_anyway":{"type":"object"}}});
        props["new_person"] = json!({"type":"object","required":["name","kind"],"properties":{"name":string(160),"kind":json!({"type":"string","enum":["human","agent"]}),"email":string(254),"phone":string(64)}});
    }
    if update {
        props["vendor_id"] = identifier();
        required.push("vendor_id");
    }
    schema(&required, props)
}
fn document_input(v: &Value, units: MinorUnits) -> Result<NewDocument> {
    let mut lines = Vec::new();
    for (i, l) in v["lines"]
        .as_array()
        .ok_or(PayablesError::InvalidOption)?
        .iter()
        .enumerate()
    {
        let line = (|| {
            Ok(DocumentLineInput {
                description: text(l, "description")?.into(),
                account_id: id(l, "account_id")?,
                amount: money(l, "amount", units)?,
                tax_code_id: l["tax_code_id"].as_i64(),
                external_order_line_id: opt(l, "external_order_line_id"),
                order_line_id: l["order_line_id"].as_i64(),
                quantity: opt(l, "quantity")
                    .map(|q| {
                        Decimal::from_str_exact(&q).map_err(|_| invalid("quantity", "is invalid"))
                    })
                    .transpose()?,
            })
        })()
        .map_err(|e| match e {
            PayablesError::Invalid(mut es) => {
                for e in &mut es {
                    e.line = Some(i + 1);
                }
                PayablesError::Invalid(es)
            }
            e => e,
        })?;
        lines.push(line);
    }
    let taxes = v
        .get("tax_lines")
        .map(|ts| {
            ts.as_array()
                .ok_or(PayablesError::InvalidOption)?
                .iter()
                .map(|t| {
                    Ok(TaxAmountInput {
                        tax_code_id: id(t, "tax_code_id")?,
                        tax_amount: money(t, "tax_amount", units)?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    Ok(NewDocument {
        vendor_id: v["vendor_id"].as_i64(),
        party_document_number: opt(v, "party_document_number"),
        document_date: date(v, "document_date")?,
        due_date: v.get("due_date").map(|_| date(v, "due_date")).transpose()?,
        description: opt(v, "description"),
        lines,
        tax_lines: taxes,
    })
}
fn payment_input(v: &Value, units: MinorUnits) -> Result<NewPayment> {
    let allocations = v["allocations"]
        .as_array()
        .ok_or(PayablesError::InvalidOption)?
        .iter()
        .enumerate()
        .map(|(i, a)| {
            Ok(AllocationInput {
                document_id: id(a, "document_id")?,
                amount: money(a, "amount", units).map_err(|e| match e {
                    PayablesError::Invalid(mut es) => {
                        for e in &mut es {
                            e.line = Some(i + 1);
                        }
                        PayablesError::Invalid(es)
                    }
                    e => e,
                })?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(NewPayment {
        payment_date: date(v, "payment_date")?,
        vendor_id: v["vendor_id"].as_i64(),
        supplier_key: opt(v, "supplier_key"),
        paid_from_account_id: id(v, "paid_from_account_id")?,
        reference: opt(v, "reference"),
        description: opt(v, "description"),
        allocations,
    })
}

#[derive(Clone, Copy)]
enum Write {
    VendorCreate,
    VendorUpdate,
    Settings,
    TaxCreate,
    TaxUpdate,
    Bill,
    Replace,
    Note,
    Approve,
    Post,
    Void,
    Submit,
    Pay,
    Reject,
    Reverse,
}
impl Write {
    fn capability(self) -> &'static str {
        match self {
            Self::VendorCreate
            | Self::VendorUpdate
            | Self::Settings
            | Self::TaxCreate
            | Self::TaxUpdate => MANAGE_SETTINGS,
            Self::Bill | Self::Replace | Self::Note => CREATE_DOCUMENTS,
            Self::Approve | Self::Void => APPROVE_DOCUMENTS,
            Self::Post => POST_DOCUMENTS,
            Self::Submit => SUBMIT_PAYMENTS,
            Self::Pay | Self::Reject | Self::Reverse => POST_PAYMENTS,
        }
    }
    fn array(self) -> &'static str {
        if matches!(self, Self::Submit | Self::Pay) {
            "allocations"
        } else {
            "lines"
        }
    }
    async fn review(self, c: &dyn OperationClient, s: &Scope, v: &Value) -> Result<Value> {
        let p = owner()?;
        let company = id(v, "company_id")?;
        authorize_on(c, s, company, self.capability()).await?;
        if matches!(self, Self::Replace) {
            authorize_on(c, s, company, APPROVE_DOCUMENTS).await?;
        }
        // Same connection and lock as the facade, no business writes. NOWAIT
        // also prevents opposite-company plans from waiting on one another.
        let row = c.query_opt("SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE NOWAIT",&[&s.tenant(),&company]).await?.ok_or(PayablesError::CompanyNotFound)?;
        if row.get::<_, String>(0) == "archived" {
            return Err(PayablesError::CompanyArchived);
        }
        let changes = match self {
            Self::VendorCreate | Self::VendorUpdate => {
                let input =
                    vendor_input(c, s, company, v, matches!(self, Self::VendorUpdate)).await?;
                let after = p
                    .prepare_vendor(c, s, company, v["vendor_id"].as_i64(), &input)
                    .await?;
                let before = match v["vendor_id"].as_i64() {
                    Some(id) => json!(p.vendor_on(c, s, company, id).await?),
                    None => Value::Null,
                };
                json!({"before":before,"target":after["_target"],"after":after})
            }
            Self::Settings => {
                let input = settings_input(c, s, company, v).await?;
                let (before, reason) = p.validate_settings(c, s, company, &input).await?;
                json!({"before":before.map(|(_,s)|s),"after":input.settings,"reason":reason,"accounts":p.ledger.list_accounts_on(c,s,company).await?})
            }
            Self::TaxCreate | Self::TaxUpdate => {
                let input = tax_input(c, s, company, v, matches!(self, Self::TaxUpdate)).await?;
                let (before, after, reason) = p
                    .prepare_tax(
                        c,
                        s,
                        company,
                        v["tax_code_id"].as_i64(),
                        v["code"].as_str(),
                        &input,
                    )
                    .await?;
                json!({"before":before,"after":after,"reason":reason,"accounts":p.ledger.list_accounts_on(c,s,company).await?})
            }
            Self::Bill | Self::Replace | Self::Note => {
                let book = p.ledger.get_book_on(c, s, company).await?;
                let input = document_input(v, book.minor_units)?;
                let replaced = if matches!(self, Self::Replace) {
                    Some(
                        documents::replaceable(c, s.tenant(), company, id(v, "document_id")?)
                            .await?,
                    )
                } else {
                    None
                };
                let (header, lines, taxes, capacity) = p
                    .prepare_document(
                        c,
                        s,
                        company,
                        v["bill_id"].as_i64(),
                        &input,
                        replaced.as_ref().map(|d| d.id),
                    )
                    .await?;
                let mut review = json!({"document":header,"lines":lines,"tax_lines":taxes,"capacity":capacity,"book":book,"accounts":p.ledger.list_accounts_on(c,s,company).await?});
                if let Some(old) = replaced {
                    review["replaces"] = document_value(old);
                    review["void_reason"] = json!(documents::REPLACED);
                }
                review
            }
            Self::Approve | Self::Void => {
                let d = store::document_record(c, s.tenant(), company, id(v, "document_id")?)
                    .await?
                    .ok_or(PayablesError::DocumentNotFound)?;
                if matches!(self, Self::Approve) {
                    if d.status != "draft" {
                        return Err(PayablesError::NotDraft);
                    }
                    let approval = p
                        .ledger
                        .preview_approval_on(c, s, company, &documents::approval(&d))
                        .await?;
                    json!({"before":document_value(d),"status":"approved","approval":approval})
                } else {
                    if !matches!(d.status.as_str(), "draft" | "approved" | "held") {
                        return Err(PayablesError::NotVoidable);
                    }
                    let reason =
                        settings::string(text(v, "void_reason")?, "void_reason", 500, true)?;
                    json!({"before":document_value(d),"status":"voided","void_reason":reason})
                }
            }
            Self::Post => {
                let prepared = p
                    .prepare_post_document(c, s, company, id(v, "document_id")?)
                    .await?;
                match prepared {
                    documents::PreparedPosting::Held(d, lines) => {
                        json!({"before":document_value(d),"status":"held","match_lines":lines,"posting":null})
                    }
                    documents::PreparedPosting::Post {
                        document,
                        payable,
                        entry,
                        options,
                        matched,
                        capacity,
                    } => {
                        let entry = p
                            .ledger
                            .preview_post_entry_on(c, s, company, &entry, &options)
                            .await?;
                        json!({"before":document_value(document),"status":"posted","payable_account_id":payable,"posting":entry,"match_lines":matched,"capacity":capacity})
                    }
                }
            }
            Self::Submit | Self::Pay => {
                let request = if matches!(self, Self::Pay) {
                    let r = payments::request(c, s.tenant(), company, id(v, "request_id")?).await?;
                    if r.status != "submitted" {
                        return Err(PayablesError::PaymentRequestDecided);
                    }
                    Some(r)
                } else {
                    None
                };
                let input = if let Some(r) = &request {
                    payments::requested(r)
                } else {
                    payment_input(v, p.ledger.get_book_on(c, s, company).await?.minor_units)?
                };
                let (value, groups) = p
                    .validate_payment(c, s, company, &input, request.is_some())
                    .await?;
                let mut capacity = Vec::new();
                for a in &input.allocations {
                    capacity.push(json!({"document_id":a.document_id,"lowest_outstanding":balances::lowest(c,s.tenant(),company,a.document_id,input.payment_date).await?}));
                }
                let posting = if let Some(r) = &request {
                    let approval = p
                        .ledger
                        .preview_approval_on(
                            c,
                            s,
                            company,
                            &payments::approval(r, opt(v, "override_reason")),
                        )
                        .await?;
                    // A future payment ID is allocated ONLY during commit. The
                    // entry is owned by that payment, not this request's ID.
                    let (new, options) = payments::posting(&input, &value, &groups)?;
                    let entry = p
                        .ledger
                        .preview_post_entry_on(c, s, company, &new, &options)
                        .await?;
                    json!({"entry":entry,"approval":approval,"owner":"the new payment for this request"})
                } else {
                    Value::Null
                };
                json!({"request":request,"payment":value,"allocations":input.allocations,"capacity":capacity,"posting":posting,"accounts":p.ledger.list_accounts_on(c,s,company).await?})
            }
            Self::Reject => {
                let r = payments::request(c, s.tenant(), company, id(v, "request_id")?).await?;
                if r.status != "submitted" {
                    return Err(PayablesError::PaymentRequestDecided);
                }
                let reason = settings::string(text(v, "reason")?, "reason", 500, true)?;
                json!({"before":r,"status":"rejected","reason":reason})
            }
            Self::Reverse => {
                let (payment, reversal) = p
                    .prepare_payment_reversal(
                        c,
                        s,
                        company,
                        id(v, "payment_id")?,
                        date(v, "payment_date")?,
                        opt(v, "description"),
                    )
                    .await?;
                let entry = p
                    .ledger
                    .preview_reverse_owned_entry_on(
                        c,
                        s,
                        company,
                        payment.entry_id,
                        &payments::owner(payment.id),
                        &reversal,
                    )
                    .await?;
                json!({"before":payment,"posting":entry,"effect":"Restore the allocated bills from the reversal date"})
            }
        };
        Ok(json!({"company_id":company,"changes":changes}))
    }
}

// The pin previews all steps before executing any. Re-review each step at
// execution to fence changes made by earlier steps; only hashes are queued.
async fn fence(c: &dyn OperationClient, v: &Value, consume: bool) -> bilibi_agent_api::Result<()> {
    let encoded = v.to_string();
    let row=c.query_opt("SELECT encode(sha256(convert_to($1,'UTF8')),'hex'),COALESCE(NULLIF(current_setting('finance.payables_reviews',true),''),'[]')::jsonb",&[&encoded]).await?.ok_or(Error::Internal)?;
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
    c.query_opt(
        "SELECT set_config('finance.payables_reviews',$1,true)",
        &[&json!(pending).to_string()],
    )
    .await?;
    Ok(())
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        v: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let review = self
                .review(c, s, v)
                .await
                .map_err(|e| error(e, self.array()))?;
            fence(
                c,
                &json!({"operation":*self as u8,"input":v,"review":review}),
                false,
            )
            .await?;
            Ok(review.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        s: &'a Scope,
        v: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = id(v, "company_id").map_err(|e| error(e, self.array()))?;
            // review locks NOWAIT before the concrete facade joins the write.
            let review = self
                .review(tx.sql(), s, v)
                .await
                .map_err(|e| error(e, self.array()))?;
            fence(
                tx.sql(),
                &json!({"operation":*self as u8,"input":v,"review":review}),
                true,
            )
            .await?;
            let p = owner().map_err(|e| error(e, self.array()))?;
            let result: Result<Value> = async {
                Ok(match self {
                    Self::VendorCreate | Self::VendorUpdate => {
                        let input = vendor_input(
                            tx.sql(),
                            s,
                            company,
                            v,
                            matches!(self, Self::VendorUpdate),
                        )
                        .await?;
                        json!(
                            p.save_vendor_on(tx, s, company, v["vendor_id"].as_i64(), &input)
                                .await?
                        )
                    }
                    Self::Settings => json!(
                        p.put_settings_on(
                            tx,
                            s,
                            company,
                            &settings_input(tx.sql(), s, company, v).await?
                        )
                        .await?
                    ),
                    Self::TaxCreate | Self::TaxUpdate => {
                        let input =
                            tax_input(tx.sql(), s, company, v, matches!(self, Self::TaxUpdate))
                                .await?;
                        json!(
                            p.save_tax_on(
                                tx,
                                s,
                                company,
                                v["tax_code_id"].as_i64(),
                                v["code"].as_str(),
                                &input
                            )
                            .await?
                        )
                    }
                    Self::Bill | Self::Note => {
                        let units = p
                            .ledger
                            .get_book_on(tx.sql(), s, company)
                            .await?
                            .minor_units;
                        json!(
                            p.create_document_on(
                                tx,
                                s,
                                company,
                                v["bill_id"].as_i64(),
                                &document_input(v, units)?
                            )
                            .await?
                        )
                    }
                    Self::Replace => {
                        let units = p
                            .ledger
                            .get_book_on(tx.sql(), s, company)
                            .await?
                            .minor_units;
                        json!(
                            p.replace_draft_on(
                                tx,
                                s,
                                company,
                                id(v, "document_id")?,
                                &document_input(v, units)?
                            )
                            .await?
                        )
                    }
                    Self::Approve => json!(
                        p.approve_document_on(tx, s, company, id(v, "document_id")?)
                            .await?
                    ),
                    Self::Post => json!(
                        p.post_document_on(tx, s, company, id(v, "document_id")?)
                            .await?
                    ),
                    Self::Void => json!(
                        p.void_document_on(
                            tx,
                            s,
                            company,
                            id(v, "document_id")?,
                            text(v, "void_reason")?
                        )
                        .await?
                    ),
                    Self::Submit => {
                        let units = p
                            .ledger
                            .get_book_on(tx.sql(), s, company)
                            .await?
                            .minor_units;
                        json!(
                            p.submit_payment_on(tx, s, company, &payment_input(v, units)?)
                                .await?
                        )
                    }
                    Self::Pay => json!(
                        p.approve_payment_on(
                            tx,
                            s,
                            company,
                            id(v, "request_id")?,
                            opt(v, "override_reason")
                        )
                        .await?
                    ),
                    Self::Reject => json!(
                        p.reject_payment_on(
                            tx,
                            s,
                            company,
                            id(v, "request_id")?,
                            text(v, "reason")?
                        )
                        .await?
                    ),
                    Self::Reverse => json!(
                        p.reverse_payment_on(
                            tx,
                            s,
                            company,
                            id(v, "payment_id")?,
                            date(v, "payment_date")?,
                            opt(v, "description")
                        )
                        .await?
                    ),
                })
            }
            .await;
            let mut record = result.map_err(|e| error(e, self.array()))?;
            if matches!(
                self,
                Self::Bill | Self::Replace | Self::Note | Self::Approve | Self::Post | Self::Void
            ) {
                let id = record["id"].as_i64().ok_or(Error::Internal)?;
                record = document_value(
                    store::document_record(tx.sql(), s.tenant(), company, id)
                        .await?
                        .ok_or(Error::Internal)?,
                );
            }
            Ok(json!({"record":record,"path":null}).into())
        })
    }
}

enum Read {
    Aging,
    Vendor,
    Vendors,
    Settings,
    Taxes,
    Document,
    Documents,
    DocumentPage,
    Request,
    Requests,
    Payment,
    Payments,
}
fn document_value(document: Document) -> Value {
    let mut value = json!(document);
    let fields = value.as_object_mut().expect("document object");
    fields.remove("debit_note_ids");
    let context = fields
        .remove("match_events")
        .expect("document match context");
    fields.insert("match_context".into(), context);
    value
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        c: &'a dyn OperationClient,
        s: &'a Scope,
        v: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let result:Result<Value>=async {
                let company=id(v,"company_id")?;
                authorize_on(c,s,company,VIEW).await?;
                let tenant=s.tenant();
                let limit=v["page_size"].as_i64().unwrap_or(100);
                // Requests and payments may list newest first: the cursor is then
                // the lowest ID seen, and none starts from the newest.
                let newest=matches!(self,Self::Requests|Self::Payments)&&v["newest_first"].as_bool()==Some(true);
                let after=v["after_id"].as_i64().filter(|n|!newest||*n>0).unwrap_or(if newest {i64::MAX} else {0});
                let (cmp,order)=if newest {("<","DESC")} else {(">","")};
                let result=match self {
                    Self::Aging=>{
                        let as_of=v["as_of"].as_str().ok_or(PayablesError::InvalidOption)?.parse().map_err(|_|invalid("as_of","must be YYYY-MM-DD"))?;
                        let mut options=AgingQuery{through_entry_number:v["through_entry_number"].as_i64(),through_opening_batch_id:v["through_opening_batch_id"].as_i64(),through_settlement_number:v["through_settlement_number"].as_i64(),..Default::default()};
                        if let Some(days)=v["bucket_days"].as_array(){options.bucket_days=days.iter().map(|n|n.as_i64().and_then(|n|i32::try_from(n).ok()).ok_or(PayablesError::InvalidOption)).collect::<Result<Vec<_>>>()?;}
                        // A read has no company write lock; use the same report facade as the page.
                        json!(owner()?.aged_payables(s,company,as_of,&options).await?)
                    },
                    Self::Vendor=>json!(owner()?.get_vendor(s,company,id(v,"vendor_id")?).await?),
                    Self::Vendors=>{
                        let records=owner()?.list_vendors(s,company,after,limit).await?;
                        let more=records.len()==limit as usize;
                        let next=if more { records.last().map(|v|v.id) } else { None };
                        json!({"records":records,"next_after_id":next})
                    },
                    Self::Settings=>json!({"settings":store::settings(c,tenant,company).await?.map(|(_,s)|s)}),
                    Self::DocumentPage=>json!(owner()?.page_documents_on(c,s,company,&DocumentPageQuery {
                        source:opt(v,"source"), document_type:opt(v,"document_type"), status:opt(v,"status"),
                        search:opt(v,"search").unwrap_or_default(), page:v["page"].as_i64().unwrap_or(1),
                        page_size:v["page_size"].as_i64().unwrap_or(25),
                    }).await?),
                    Self::Document=>{
                        if !v["as_of"].is_null()||["through_entry_number","through_opening_batch_id","through_settlement_number"].iter().any(|key|!v[key].is_null()) {
                            let (document,pins)=owner()?.document_at(s,company,id(v,"document_id")?,v).await?;
                            let mut record=document_value(document);
                            record["balance_pins"]=pins;
                            record
                        } else {
                            document_value(store::document_record(c,tenant,company,id(v,"document_id")?).await?.ok_or(PayablesError::DocumentNotFound)?)
                        }
                    },
                    Self::Request=>json!(payments::request(c,tenant,company,id(v,"request_id")?).await?),
                    Self::Payment=>json!(payments::payment(c,tenant,company,id(v,"payment_id")?).await?),
                    Self::Taxes|Self::Documents|Self::Requests|Self::Payments=>{
                        let vendor=v["vendor_id"].as_i64();
                        let source=opt(v,"source");
                        let status=opt(v,"status"); let party=opt(v,"party_key");let kind=opt(v,"document_type"); let corrects=v["corrects_document_id"].as_i64();
                        // Cursor and ordering use the same stable ID. These
                        // agent lists deliberately don't reuse page offsets.
                        let query=match self {
                            Self::Taxes=>"SELECT id FROM finance_payables_tax_codes WHERE tenant_id=$1 AND company_id=$2 AND id>$3 ORDER BY id LIMIT $4".to_owned(),
                            Self::Documents=>"SELECT id FROM finance_payables_documents WHERE tenant_id=$1 AND company_id=$2 AND id>$3 AND ($5::text IS NULL OR status=$5) AND ($6::text IS NULL OR party_key=$6) AND ($7::text IS NULL OR document_type=$7) AND ($8::bigint IS NULL OR corrects_document_id=$8) AND ($9::bigint IS NULL OR party_key='vendor:'||$9::bigint) AND ($10::text IS NULL OR EXISTS (SELECT 1 FROM finance_payables_opening_items i WHERE i.tenant_id=$1 AND i.company_id=$2 AND i.document_id=finance_payables_documents.id)=($10='opening')) ORDER BY id LIMIT $4".to_owned(),
                            Self::Requests=>format!("SELECT r.id FROM finance_payables_payment_requests r LEFT JOIN finance_payables_payment_request_events e ON e.tenant_id=r.tenant_id AND e.company_id=r.company_id AND e.request_id=r.id WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id{cmp}$3 AND ($5::text IS NULL OR COALESCE(e.event,'submitted')=$5) AND ($6::text IS NULL OR r.party_key=$6) AND ($7::bigint IS NULL OR r.party_key='vendor:'||$7::bigint) ORDER BY r.id {order} LIMIT $4"),
                            _=>format!("SELECT id FROM finance_payables_payments WHERE tenant_id=$1 AND company_id=$2 AND id{cmp}$3 AND ($5::text IS NULL OR party_key=$5) AND ($6::bigint IS NULL OR party_key='vendor:'||$6::bigint) ORDER BY id {order} LIMIT $4"),
                        };
                        let n=limit+1;
                        let mut params:Vec<&(dyn tokio_postgres::types::ToSql+Sync)>=vec![&tenant,&company,&after,&n];
                        match self {
                            Self::Documents => { params.push(&status); params.push(&party); params.push(&kind); params.push(&corrects); params.push(&vendor); params.push(&source); }
                            Self::Requests => { params.push(&status); params.push(&party); params.push(&vendor); }
                            Self::Payments => { params.push(&party); params.push(&vendor); },
                            _ => {}
                        }
                        let mut ids:Vec<i64>=c.query(&query,&params).await?.iter().map(|r|r.get(0)).collect();
                        let more=ids.len()>limit as usize; ids.truncate(limit as usize);
                        let next=if more {ids.last().copied()} else {None};
                        let mut records=Vec::new();
                        for id in ids {records.push(match self {
                            Self::Taxes=>{let row=c.query_opt("SELECT id,code,name,rate,account_id,active FROM finance_payables_tax_codes WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&tenant,&company,&id]).await?.ok_or(Error::NotFound)?;json!({"id":row.get::<_,i64>(0),"code":row.get::<_,String>(1),"name":row.get::<_,String>(2),"rate":row.get::<_,Decimal>(3),"account_id":row.get::<_,i64>(4),"active":row.get::<_,bool>(5)})},
                            Self::Documents=>document_value(store::document_record(c,tenant,company,id).await?.ok_or(PayablesError::DocumentNotFound)?),
                            Self::Requests=>json!(payments::request(c,tenant,company,id).await?),
                            _=>json!(payments::payment(c,tenant,company,id).await?),
                        });}
                        json!({"records":records,"next_after_id":next,"page_size":limit})
                    }
                };Ok(result)
            }.await;
            Ok(result.map_err(|e| error(e, "lines"))?.into())
        })
    }
}

fn identifier() -> Value {
    json!({"type":"integer","minimum":1})
}
fn string(max: usize) -> Value {
    json!({"type":"string","maxLength":max})
}
fn day() -> Value {
    json!({"type":"string","format":"date"})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
fn object(required: &[&str], props: Value) -> Value {
    json!({"type":"object","required":required,"properties":props})
}
fn schema(required: &[&str], props: Value) -> Value {
    let mut props = props.as_object().cloned().unwrap_or_default();
    props.insert("company_id".into(), identifier());
    let required: Vec<_> = std::iter::once("company_id")
        .chain(required.iter().copied())
        .collect();
    object(&required, json!(props))
}
fn complete_pins(value: Value) -> Value {
    // The operation schema dialect has no cross-field combinators. The facade
    // validates that callers supply all three accounting pins or none.
    value
}
fn aging() -> Value {
    complete_pins(schema(
        &["as_of"],
        json!({"as_of":day(),"through_entry_number":{"type":"integer","minimum":0},"through_opening_batch_id":{"type":"integer","minimum":0},"through_settlement_number":{"type":"integer","minimum":0},"bucket_days":{"type":"array","items":{"type":"integer","minimum":1,"maximum":36500}}}),
    ))
}
fn output() -> Value {
    json!({"type":"object","description":"Payables records or normalized review facts. Amounts and rates are exact decimal strings; dates are ISO dates. Generated IDs, numbers and timestamps exist only after commit. No Payables page is added by these operations; committed path is null."})
}
fn settings_schema() -> Value {
    let t = object(
        &["kind", "value"],
        json!({"kind":{"type":"string","enum":["percent","amount"]},"value":string(25)}),
    );
    schema(
        &[],
        json!({"payable_account_id":identifier(),"quantity_tolerance":t,"price_tolerance":t,"clear_quantity_tolerance":boolean(),"clear_price_tolerance":boolean(),"reason":string(500)}),
    )
}
fn tax_schema(update: bool) -> Value {
    if update {
        schema(
            &["tax_code_id"],
            json!({"tax_code_id":identifier(),"name":string(200),"rate":string(25),"account_id":identifier(),"active":boolean(),"reason":string(500)}),
        )
    } else {
        schema(
            &["code", "name", "rate", "account_id"],
            json!({"code":string(50),"name":string(200),"rate":string(25),"account_id":identifier(),"active":boolean(),"reason":string(500)}),
        )
    }
}
fn document_schema(note: bool) -> Value {
    let line = object(
        &["description", "account_id", "amount"],
        json!({"description":string(500),"account_id":identifier(),"amount":string(25),"tax_code_id":identifier(),"external_order_line_id":string(100),"order_line_id":identifier(),"quantity":string(25)}),
    );
    let tax = object(
        &["tax_code_id", "tax_amount"],
        json!({"tax_code_id":identifier(),"tax_amount":string(25)}),
    );
    let mut props = json!({"party_document_number":string(100),"document_date":day(),"due_date":day(),"description":string(500),"lines":{"type":"array","items":line},"tax_lines":{"type":"array","items":tax}});
    if note {
        props["bill_id"] = identifier();
        schema(&["bill_id", "document_date", "lines"], props)
    } else {
        props["vendor_id"] = identifier();
        schema(
            &[
                "vendor_id",
                "party_document_number",
                "document_date",
                "lines",
            ],
            props,
        )
    }
}
fn replace_schema() -> Value {
    let mut schema = document_schema(false);
    schema["properties"]["document_id"] = identifier();
    schema["required"]
        .as_array_mut()
        .expect("required list")
        .push(json!("document_id"));
    schema
}
fn payment_schema() -> Value {
    schema(
        &["payment_date", "paid_from_account_id", "allocations"],
        json!({"payment_date":day(),"vendor_id":identifier(),"supplier_key":string(100),"paid_from_account_id":identifier(),"reference":string(100),"description":string(500),"allocations":{"type":"array","items":object(&["document_id","amount"],json!({"document_id":identifier(),"amount":string(25)}))}}),
    )
}
fn document_id() -> Value {
    schema(&["document_id"], json!({"document_id":identifier()}))
}
fn request_id() -> Value {
    schema(&["request_id"], json!({"request_id":identifier()}))
}
fn payment_id() -> Value {
    schema(&["payment_id"], json!({"payment_id":identifier()}))
}
fn document_page_schema() -> Value {
    schema(
        &[],
        json!({
            "source":{"type":"string","enum":["ordinary","opening"]},
            "status":{"type":"string","enum":["draft","approved","held","posted","voided"]},
            "document_type":{"type":"string","enum":["bill","debit_note","opening_credit"]},
            "search":string(255), "page":{"type":"integer","minimum":1},
            "page_size":{"type":"integer","enum":[25,50,100]}
        }),
    )
}
fn list_schema(kind: &str) -> Value {
    let mut props = json!({"after_id":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100}});
    if kind != "tax" && kind != "vendor" {
        props["party_key"] = string(100);
        props["vendor_id"] = identifier();
    }
    if kind == "document" {
        props["status"] =
            json!({"type":"string","enum":["draft","approved","held","posted","voided"]});
        props["document_type"] =
            json!({"type":"string","enum":["bill","debit_note","opening_credit"]});
        props["corrects_document_id"] = identifier();
        props["source"] = json!({"type":"string","enum":["ordinary","opening"]});
    }
    if kind == "request" {
        props["status"] = json!({"type":"string","enum":["submitted","posted","rejected"]});
    }
    if kind == "request" || kind == "payment" {
        props["newest_first"] = boolean();
    }
    schema(&[], props)
}

inventory::submit! {Contribution::new(DEFINITIONS,"finance/payables.operations",Provenance {module:MODULE,source:file!(),line:line!()},&Definitions {
    operations:&[
        Operation {key:"finance.payables.aging.get",title:"Read payables aging",summary:"Read supplier residuals at a date and posting pin. Bills age from their due dates; unapplied credits age from their source dates. Opening records are excluded before conversion.",keywords:&["conversion"],capability:VIEW,input:aging,output,handler:Handler::Read(&Read::Aging),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.vendor.get",title:"Read a vendor",summary:"Read Finance settings and the linked Core company.",keywords:&[],capability:VIEW,input:||schema(&["vendor_id"],json!({"vendor_id":identifier()})),output,handler:Handler::Read(&Read::Vendor),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.vendor.list",title:"List vendors",summary:"Read the current company's vendors by ID.",keywords:&[],capability:VIEW,input:||list_schema("vendor"),output,handler:Handler::Read(&Read::Vendors),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.vendor.create",title:"Add a vendor",summary:"Link a Core company through Base Records; retain Finance settings only.",keywords:&[],capability:MANAGE_SETTINGS,input:||vendor_schema(false),output,handler:Handler::SafeWrite(&Write::VendorCreate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.vendor.update",title:"Change vendor settings",summary:"Change terms, availability and account or tax defaults; code and company are permanent.",keywords:&[],capability:MANAGE_SETTINGS,input:||vendor_schema(true),output,handler:Handler::SafeWrite(&Write::VendorUpdate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.settings.get",title:"Read payables settings",summary:"Read the control account and matching tolerances; null before configuration.",keywords:&[],capability:VIEW,input:||schema(&[],json!({})),output,handler:Handler::Read(&Read::Settings),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.tax_code.list",title:"List payables tax codes",summary:"Read configured tax codes by ID with a bounded cursor.",keywords:&[],capability:VIEW,input:||list_schema("tax"),output,handler:Handler::Read(&Read::Taxes),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.get",title:"Read a payable document",summary:"Read a bill or debit note with immutable snapshots and either current or pinned outstanding and activity.",keywords:&[],capability:VIEW,input:dated_document,output,handler:Handler::Read(&Read::Document),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.page",title:"Read bills page",summary:"Read bills and debit notes with source and status filters, search, SQL totals and bounded page offsets.",keywords:&["opening"],capability:VIEW,input:document_page_schema,output,handler:Handler::Read(&Read::DocumentPage),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.list",title:"List payable documents",summary:"Read bills and debit notes by ID, optionally filtered by source, party, type and status.",keywords:&[],capability:VIEW,input:||list_schema("document"),output,handler:Handler::Read(&Read::Documents),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment_request.get",title:"Read a payment request",summary:"Read retained allocations, submitter, decision and posted payment ID.",keywords:&[],capability:VIEW,input:request_id,output,handler:Handler::Read(&Read::Request),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment_request.list",title:"List payment requests",summary:"Read requests by ID, oldest first or with newest_first newest first, with party and status filters and a bounded cursor.",keywords:&[],capability:VIEW,input:||list_schema("request"),output,handler:Handler::Read(&Read::Requests),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.get",title:"Read a payable payment",summary:"Read a posted payment, allocations, entry and reversal identifiers.",keywords:&[],capability:VIEW,input:payment_id,output,handler:Handler::Read(&Read::Payment),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.list",title:"List payable payments",summary:"Read payments and reversals by ID, oldest first or with newest_first newest first, with a party filter and a bounded cursor.",keywords:&[],capability:VIEW,input:||list_schema("payment"),output,handler:Handler::Read(&Read::Payments),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.settings.put",title:"Configure payables",summary:"Set the active liability control account and tolerances. Omitted values preserve settings; explicit flags clear tolerances.",keywords:&[],capability:MANAGE_SETTINGS,input:settings_schema,output,handler:Handler::SafeWrite(&Write::Settings),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.tax_code.create",title:"Create a payables tax code",summary:"Create a company-defined code, rate and account; keep an audited change history.",keywords:&[],capability:MANAGE_SETTINGS,input:||tax_schema(false),output,handler:Handler::SafeWrite(&Write::TaxCreate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.tax_code.update",title:"Change a payables tax code",summary:"Change name, rate, account or active flag; omissions preserve values and code is immutable.",keywords:&[],capability:MANAGE_SETTINGS,input:||tax_schema(true),output,handler:Handler::SafeWrite(&Write::TaxUpdate),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.bill.create",title:"Record a bill",summary:"Freeze the party, positive net lines and tax snapshots in a draft without posting.",keywords:&[],capability:CREATE_DOCUMENTS,input:||document_schema(false),output,handler:Handler::SafeWrite(&Write::Bill),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.bill.replace_draft",title:"Replace a draft bill",summary:"Void a draft bill and record its corrected draft in one step: if the corrected draft is refused, the draft is left as it was, and its supplier invoice number may be reused.",keywords:&[],capability:CREATE_DOCUMENTS,input:replace_schema,output,handler:Handler::SafeWrite(&Write::Replace),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.debit_note.create",title:"Record a debit note",summary:"Draft a reduction against a posted bill, retaining its party and respecting future outstanding balances.",keywords:&[],capability:CREATE_DOCUMENTS,input:||document_schema(true),output,handler:Handler::SafeWrite(&Write::Note),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.approve",title:"Approve a payable document",summary:"Approve a draft through separation of duties; no accounting entry is posted yet.",keywords:&[],capability:APPROVE_DOCUMENTS,input:document_id,output,handler:Handler::SafeWrite(&Write::Approve),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.post",title:"Post a payable document",summary:"Match an approved or held bill; record a hold or post its owned entry atomically. Actual posting requires ledger capability and an open period.",keywords:&[],capability:POST_DOCUMENTS,input:document_id,output,handler:Handler::SafeWrite(&Write::Post),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.document.void",title:"Void a payable draft",summary:"Void an unposted draft, approved or held document permanently with a reason, retaining its evidence.",keywords:&[],capability:APPROVE_DOCUMENTS,input:||schema(&["document_id","void_reason"],json!({"document_id":identifier(),"void_reason":string(500)})),output,handler:Handler::SafeWrite(&Write::Void),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.submit",title:"Submit a payment request",summary:"Retain positive bill allocations for approval; no balance reservation or posting occurs.",keywords:&[],capability:SUBMIT_PAYMENTS,input:payment_schema,output,handler:Handler::SafeWrite(&Write::Submit),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.approve",title:"Approve and post a payment",summary:"Independently approve a submitted request and post against the bills' original control accounts. Requires ledger posting capability.",keywords:&[],capability:POST_PAYMENTS,input:||schema(&["request_id"],json!({"request_id":identifier(),"override_reason":string(500)})),output,handler:Handler::SafeWrite(&Write::Pay),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.reject",title:"Reject a payment request",summary:"Record a permanent rejection and reason without posting a payment.",keywords:&[],capability:POST_PAYMENTS,input:||schema(&["request_id","reason"],json!({"request_id":identifier(),"reason":string(500)})),output,handler:Handler::SafeWrite(&Write::Reject),channels:Channels::Every,step_up:&[]},
        Operation {key:"finance.payables.payment.reverse",title:"Reverse a posted payment",summary:"Post its exact owned mirror once, restoring the bills from the reversal date. Use the payment ID, not the request ID.",keywords:&[],capability:POST_PAYMENTS,input:||schema(&["payment_id","payment_date"],json!({"payment_id":identifier(),"payment_date":day(),"description":string(500)})),output,handler:Handler::SafeWrite(&Write::Reverse),channels:Channels::Every,step_up:&[]},
    ],
    guides:&[Guide {key:"finance.payables",title:"Bills, debit notes and payments",summary:"Reviewed drafts, independent approvals, owned posting, payment requests and dated reversals.",keywords:&[],capability:VIEW,body:include_str!("../guide.md")}],
})}

fn dated_document() -> Value {
    let mut s = document_id();
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
