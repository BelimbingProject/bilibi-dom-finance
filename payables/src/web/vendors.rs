//! Vendor settings and explicit historical matches, through the same
//! contracts as agents. Company creation and relationships belong to Core.
//! The page's state and its reads are here; the list and the historical keys
//! are drawn in `vendors/list.rs`, the Add supplier and match dialogs (and the
//! "Add to…" drop that opens the first) in `vendors/form.rs`, and every live
//! event in `vendors/events.rs`.
mod events;
mod form;
mod list;

use super::{company, render, Pages, PAGES};
use crate::{SupplierHistory, Vendor, MANAGE_SETTINGS};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MultiPartyChooser, PartyErrors, PartyPickerContext};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    *,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const FORM: &str = "vendor";
static SPEC: LazyLock<Arc<ListSpec>> =
    LazyLock::new(|| ListSpec::default().shared().expect("vendor list"));
struct Create {
    chooser: MultiPartyChooser,
    values: HashMap<String, String>,
    errors: PartyErrors,
}
pub(super) struct Vendors {
    pages: Pages,
    /// Records an "Add to…" link carried (`?drop=`), opened once on first use.
    dropped: Option<String>,
    all: Vec<Vendor>,
    history: Vec<SupplierHistory>,
    list: ListState,
    columns: ListColumns,
    history_columns: ListColumns,
    can_manage: bool,
    create: Option<Create>,
    mapping: Option<(String, HashMap<String, String>, PartyErrors)>,
    notice: Option<(StatusKind, String)>,
    company_name: String,
    states: CommitStates,
}
impl Vendors {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            dropped: query.get(bilibi_records::DROP_PARAM).cloned(),
            all: vec![],
            history: vec![],
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Invalid)?,
            columns: ListColumns::mount(
                "payables-vendors-table",
                [
                    ("code", "Code"),
                    ("company", "Supplier"),
                    ("terms", "Terms (days)"),
                    ("payable", "Payable account"),
                    ("expense", "Default expense"),
                    ("tax", "Default tax"),
                    ("active", "Status"),
                    ("relationship", "Company record"),
                ]
                .into_iter()
                .map(|(k, l)| BuiltinColumn::new(k, l))
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
            history_columns: ListColumns::mount(
                "payables-vendor-keys",
                [
                    ("key", "Supplier key"),
                    ("facts", "Details on bills"),
                    ("collisions", "Similar keys"),
                    ("vendor", "Matched supplier"),
                ]
                .into_iter()
                .map(|(k, l)| BuiltinColumn::new(k, l))
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
            can_manage: false,
            create: None,
            mapping: None,
            notice: None,
            company_name: String::new(),
            states: CommitStates::default(),
        })
    }
    fn context(&self) -> PartyPickerContext {
        PartyPickerContext {
            records: self.pages.records.clone(),
            contracts: self.pages.contracts.clone(),
            service: PAGES,
            role: "vendor",
        }
    }
    pub(super) async fn reload(&mut self, s: &Scope) -> Result<()> {
        self.can_manage = self.pages.allowed(s, MANAGE_SETTINGS).await?;
        let Some(company) = company(s) else {
            self.all.clear();
            self.history.clear();
            return Ok(());
        };
        self.company_name = self.pages.company_name(s).await;
        let mut input = json!({"company_id":company,"page_size":100});
        self.all.clear();
        loop {
            let result = self
                .read(s, "finance.payables.vendor.list", input.clone())
                .await?;
            for v in result["records"].as_array().into_iter().flatten() {
                self.all
                    .push(serde_json::from_value(v.clone()).map_err(|_| Error::Internal)?);
            }
            if result["next_after_id"].is_null() {
                break;
            }
            input["after_id"] = result["next_after_id"].clone();
        }
        let history = self
            .read(
                s,
                "finance.payables.vendor.supplier_keys",
                json!({"company_id":company}),
            )
            .await?;
        self.history =
            serde_json::from_value(history["records"].clone()).map_err(|_| Error::Internal)?;
        self.columns.load(&self.all, |v| v.id.to_string());
        self.history_columns
            .load(&self.history, |h| h.supplier_key.clone());
        Ok(())
    }
    async fn read(&self, s: &Scope, key: &str, input: Value) -> Result<Value> {
        self.pages
            .contracts
            .read(s, key, &input)
            .await
            .map(|mut v| v["result"].take())
            .map_err(|e| match e {
                bilibi_agent_api::Error::Forbidden => Error::Forbidden,
                bilibi_agent_api::Error::Backend(e) => e,
                _ => Error::Internal,
            })
    }
    fn button(id: &str, event: &str, label: &str, name: &str, value: &str) -> Result<Html> {
        render(Button {
            id,
            button_type: "button",
            event,
            name,
            value,
            body: Html::text(label),
            ..Button::default()
        })
    }
    fn inline(&self, v: &Vendor, key: &str, label: &str, value: String) -> InlineField {
        let mut field = InlineField::new(format!("vendor-{}-{key}", v.id), key, label, value)
            .status(self.states.get(&format!("{}-{key}", v.id)));
        field.id_value = v.id.to_string();
        field.save_event = "save_vendor".into();
        field.allow_empty = true;
        if !self.can_manage {
            field = field.readonly();
        }
        field
    }
}
impl LiveState<Scope, Error> for Vendors {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Event(v) => return self.event(r.scope, r.credential, v).await,
                LiveInput::Refresh | LiveInput::Params(_) => self.reload(r.scope).await?,
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}
