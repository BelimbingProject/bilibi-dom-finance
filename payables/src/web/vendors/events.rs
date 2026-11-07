//! Every live event of the Suppliers page. Writes go through `Pages::write`
//! (the contract the agent doors call), never the facade, and each event that
//! changes a supplier is refused first for a person without
//! `finance.payables.settings.manage`.
use super::Vendors;
use crate::web::{company, Written};
use crate::MANAGE_SETTINGS;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{record_party_relationship, PartyErrors, PartySubmission};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::LiveBody, params, CommitStatus, StatusKind};
use serde_json::{json, Value};
use std::collections::BTreeMap;

impl Vendors {
    pub(super) async fn event(
        &mut self,
        s: &Scope,
        credential: &str,
        v: &Value,
    ) -> Result<LiveBody> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        let context = self.context();
        if matches!(
            event,
            "create_vendor"
                | "save_vendor"
                | "record_vendor_relationship"
                | bilibi_records::DROP_EVENT
        ) && !self.pages.allowed(s, MANAGE_SETTINGS).await?
        {
            self.notice = Some((
                StatusKind::Error,
                "You do not have permission to manage suppliers.".into(),
            ));
            self.reload(s).await?;
            return Ok(LiveBody {
                html: self.body(s).await?,
                ..LiveBody::default()
            });
        }
        match event {
            "filters" => self.list.apply_filters(&params::form_values(v, "filters")),
            "page" => self.list.put_page(v["page"].as_str()),
            "grid" => {
                self.columns.handle(&params::string_values(v));
            }
            "show_vendor" => {
                if self.pages.allowed(s, MANAGE_SETTINGS).await? {
                    self.open_create(s).await?;
                }
            }
            bilibi_records::DROP_EVENT => {
                let text = v["records"].as_str().ok_or(Error::Invalid)?;
                self.receive_drop(s, text).await?;
            }
            "cancel_vendor" => {
                self.create = None;
            }
            "vendor_change" => self.take(s, v).await?,
            "search_companies" => {
                if let Some(c) = &mut self.create {
                    c.chooser
                        .search(&context, s, v["query"].as_str().unwrap_or(""))
                        .await?;
                }
            }
            "pick_companies" => {
                if let Some(c) = &mut self.create {
                    c.chooser
                        .pick(&context, s, v["value"].as_str().ok_or(Error::Invalid)?)
                        .await?;
                    c.errors = PartyErrors::default();
                }
            }
            "remove_company" => {
                if let Some(c) = &mut self.create {
                    c.chooser.remove(v["value"].as_str().ok_or(Error::Invalid)?);
                }
            }
            "new_company" => {
                if let Some(c) = &mut self.create {
                    c.chooser.start_new(v["query"].as_str().unwrap_or(""));
                }
            }
            "cancel_new_company" => {
                if let Some(c) = &mut self.create {
                    c.chooser.cancel_new();
                }
            }
            "use_candidate" => {
                if let Some(c) = &mut self.create {
                    c.chooser
                        .use_candidate(&context, s, v["company"].as_str().ok_or(Error::Invalid)?)
                        .await?;
                    c.errors = PartyErrors::default();
                }
            }
            "create_vendor" => {
                self.take(s, v).await?;
                if let Some(c) = &mut self.create {
                    c.errors = PartyErrors::default();
                    let terms = c
                        .values
                        .get("payment_terms_days")
                        .map(String::as_str)
                        .unwrap_or("");
                    let days = if terms.is_empty() {
                        Some(0)
                    } else {
                        terms.parse::<i32>().ok().filter(|n| (0..=3650).contains(n))
                    };
                    let Some(days) = days else {
                        c.errors = PartyErrors(BTreeMap::from([(
                            "payment_terms_days".into(),
                            vec!["must be between 0 and 3650".into()],
                        )]));
                        return Ok(LiveBody {
                            html: self.body(s).await?,
                            ..LiveBody::default()
                        });
                    };
                    if c.chooser.count() == 0 {
                        c.errors = PartyErrors(BTreeMap::from([(
                            "base".into(),
                            vec!["Choose a company or person.".into()],
                        )]));
                    } else {
                        let mut added = 0;
                        let mut pending_new = false;
                        if c.chooser.is_new() {
                            match c
                                .chooser
                                .chooser()
                                .submit_composed(
                                    &context,
                                    s,
                                    credential,
                                    event,
                                    "finance.payables.vendor.create",
                                    json!({"payment_terms_days":days,"active":true}),
                                )
                                .await?
                            {
                                PartySubmission::Done(_) => {
                                    added += 1;
                                    c.chooser.cancel_new();
                                }
                                PartySubmission::Invalid(errors) => {
                                    c.errors = errors;
                                    pending_new = true;
                                }
                                PartySubmission::Refused(message) => {
                                    c.errors = PartyErrors(BTreeMap::from([(
                                        "base".into(),
                                        vec![message],
                                    )]));
                                    pending_new = true;
                                }
                                PartySubmission::Review => pending_new = true,
                            }
                        }
                        let rows: Vec<_> = c.chooser.rows().iter().map(|r| r.id.clone()).collect();
                        for id in rows {
                            c.chooser.activate(&id)?;
                            match c
                                .chooser
                                .chooser()
                                .submit_composed(
                                    &context,
                                    s,
                                    credential,
                                    event,
                                    "finance.payables.vendor.create",
                                    json!({"payment_terms_days":days,"active":true}),
                                )
                                .await?
                            {
                                PartySubmission::Done(record) => {
                                    added += 1;
                                    if let Some(vendor_id) = record["id"].as_i64() {
                                        c.chooser.mark_existing(&id,format!("/finance/payables/vendors#payables-vendors-table-row-{vendor_id}"));
                                    }
                                    c.chooser.remove(&id);
                                }
                                PartySubmission::Invalid(errors) => {
                                    c.chooser.row_errors(&id, errors)
                                }
                                PartySubmission::Refused(message) => c.chooser.row_errors(
                                    &id,
                                    PartyErrors(BTreeMap::from([("base".into(), vec![message])])),
                                ),
                                PartySubmission::Review => {}
                            }
                        }
                        if pending_new {
                            c.chooser.resume_new();
                        }
                        if added > 0 {
                            self.notice =
                                Some((StatusKind::Success, format!("{added} supplier(s) added.")));
                        }
                        if c.chooser.count() == 0 {
                            self.create = None;
                        }
                    }
                }
            }
            "save_vendor" => {
                let id = v["id"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let key = v["field"].as_str().ok_or(Error::Invalid)?;
                let value = v["value"].as_str().ok_or(Error::Invalid)?;
                if !self.all.iter().any(|v| v.id == id) {
                    return Err(Error::Invalid);
                }
                let mut input = json!({"company_id":company(s),"vendor_id":id});
                if matches!(
                    key,
                    "payable_account_id" | "expense_account_id" | "tax_code_id"
                ) && value.trim().is_empty()
                {
                    input[match key {
                        "payable_account_id" => "clear_payable_account",
                        "expense_account_id" => "clear_expense_account",
                        _ => "clear_tax_code",
                    }] = json!(true);
                } else {
                    input[key] = match key {
                        "payment_terms_days" => {
                            value.parse::<i32>().map(|v| json!(v)).unwrap_or(json!(-1))
                        }
                        "active" => json!(value == "true"),
                        "payable_account_id" | "expense_account_id" | "tax_code_id" => {
                            value.parse::<i64>().map(|v| json!(v)).unwrap_or(json!(0))
                        }
                        _ => return Err(Error::Invalid),
                    };
                }
                let status = match self
                    .pages
                    .write(
                        s,
                        credential,
                        event,
                        "finance.payables.vendor.update",
                        input,
                    )
                    .await?
                {
                    Written::Done(_) => CommitStatus::Saved,
                    Written::Fields(e) => CommitStatus::Error(
                        e.into_values().flatten().collect::<Vec<_>>().join("; "),
                    ),
                    _ => CommitStatus::Error(
                        "The change was not saved. Check your permission and try again.".into(),
                    ),
                };
                self.states.put(format!("{id}-{key}"), status);
            }
            "record_vendor_relationship" => {
                let id = v["vendor"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let target = self.all.iter().find(|v| v.id == id).ok_or(Error::Invalid)?;
                let recorded = record_party_relationship(
                    &context,
                    s,
                    credential,
                    event,
                    &target.party.kind,
                    target.party.id,
                )
                .await?;
                self.notice = Some(match recorded {
                    Ok(()) => (
                        StatusKind::Success,
                        "Recorded as a supplier in Companies.".into(),
                    ),
                    Err(e) => (StatusKind::Warning, e),
                });
            }
            _ => return Err(Error::Invalid),
        }
        self.reload(s).await?;
        Ok(LiveBody {
            html: self.body(s).await?,
            query: self.list.to_params(),
            ..LiveBody::default()
        })
    }
}
