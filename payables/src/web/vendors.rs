//! Vendor settings and explicit historical matches, through the same
//! contracts as agents. Company creation and relationships belong to Core.
use super::{company, render, Pages, Written, PAGES};
use crate::{SupplierHistory, Vendor, MANAGE_SETTINGS};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{
    record_party_relationship, MultiPartyChooser, PartyErrors, PartyPickerContext, PartySubmission,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    *,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
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
    /// Opens the Add vendor dialog, as "Add vendor" does.
    async fn open_create(&mut self, s: &Scope) -> Result<()> {
        self.reload(s).await?;
        let existing = self
            .all
            .iter()
            .map(|vendor| {
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("search", &vendor.code)
                    .finish();
                (
                    format!("{}:{}", vendor.record_kind, vendor.record_id),
                    format!(
                        "/finance/payables/vendors?{query}#payables-vendors-table-row-{}",
                        vendor.id
                    ),
                )
            })
            .collect();
        self.create = Some(Create {
            chooser: MultiPartyChooser::new("vendor-new", FORM).existing(
                "Already a supplier",
                "View supplier",
                existing,
            ),
            values: HashMap::new(),
            errors: PartyErrors::default(),
        });
        Ok(())
    }

    /// The records an "Add to…" link carried, opened once.
    pub(super) async fn open_dropped(&mut self, s: &Scope) -> Result<()> {
        match self.dropped.take() {
            Some(text) => self.receive_drop(s, &text).await,
            None => Ok(()),
        }
    }

    /// Companies and contacts dropped on the page, or carried by "Add to…":
    /// the Add vendor dialog opens with them chosen, as if each had been found
    /// in the chooser. The package is a request only: `Records::receive`
    /// re-reads every record under this person's scope, the chooser reads it
    /// again, and `vendor.create` decides when the dialog is confirmed.
    async fn receive_drop(&mut self, s: &Scope, text: &str) -> Result<()> {
        if !self.pages.allowed(s, MANAGE_SETTINGS).await? {
            self.notice = Some((
                StatusKind::Error,
                "You do not have permission to manage suppliers.".into(),
            ));
            return Ok(());
        }
        let dropped = match self
            .pages
            .records
            .receive(s, super::VENDOR_TARGET, text)
            .await?
        {
            Ok(dropped) => dropped,
            Err(refusal) => {
                self.notice = Some((StatusKind::Error, refusal.message()));
                return Ok(());
            }
        };
        if self.create.is_none() {
            self.open_create(s).await?;
        }
        let context = self.context();
        let mut refused = 0;
        if let Some(create) = self.create.as_mut() {
            for record in &dropped {
                match create
                    .chooser
                    .add(&context, s, &record.reference.kind, &record.id)
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Invalid | Error::NotFound | Error::Forbidden) => refused += 1,
                    Err(error) => return Err(error),
                }
            }
            create.errors = PartyErrors::default();
        }
        if refused > 0 {
            self.notice = Some((
                StatusKind::Error,
                format!(
                    "{refused} of the dropped records could not be added: they are already suppliers or not available to you."
                ),
            ));
        }
        Ok(())
    }
    pub(super) async fn body(&self, s: &Scope) -> Result<String> {
        let mut actions = Html::EMPTY;
        if self.can_manage {
            actions = render(CreateButton {
                noun: "Supplier",
                id: "vendor-add",
                event: "show_vendor",
                ..CreateButton::default()
            })?;
        }
        let mut body = render(Header {
            title: Html::text("Suppliers"),
            subtitle: "",
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        if let Some((kind, message)) = &self.notice {
            body.push(&render(Alert {
                id: "vendors-notice",
                kind: *kind,
                body: Html::text(message),
                ..Alert::default()
            })?);
        }
        let (accounts, taxes) = if let Some(company) = company(s) {
            let mut input = json!({"company_id":company,"page_size":100});
            let mut accounts = Vec::new();
            loop {
                let a = self
                    .read(s, "finance.ledger.account.list", input.clone())
                    .await;
                let a = match a {
                    Ok(a) => a,
                    Err(Error::Forbidden | Error::Unauthorized) => break,
                    Err(e) => return Err(e),
                };
                for row in a["records"].as_array().into_iter().flatten() {
                    if row["active"] == true && row["is_group"] != true {
                        accounts.push(row.clone());
                    }
                }
                if a["next_after_code"].is_null() {
                    break;
                }
                input["after_code"] = a["next_after_code"].clone();
            }
            let mut tax_input = json!({"company_id":company,"page_size":100});
            let mut taxes = Vec::new();
            loop {
                let page = self
                    .read(s, "finance.payables.tax_code.list", tax_input.clone())
                    .await?;
                taxes.extend(page["records"].as_array().into_iter().flatten().cloned());
                if page["next_after_id"].is_null() {
                    break;
                }
                tax_input["after_id"] = page["next_after_id"].clone();
            }
            (accounts, taxes)
        } else {
            (vec![], vec![])
        };
        let mut rows = vec![];
        body.push(&render(FilterToolbar::new(
            "vendor-filters",
            vec![ToolbarControl::search(
                "vendor-search",
                "search",
                "Search suppliers",
                &self.list.search,
                "Search suppliers",
            )],
        ))?);
        let search = self.list.search.to_lowercase();
        let filtered: Vec<_> = self
            .all
            .iter()
            .filter(|v| {
                v.code.to_lowercase().contains(&search)
                    || v.party.name.to_lowercase().contains(&search)
            })
            .collect();
        let total = filtered.len() as u64;
        let start = ((self.list.page.saturating_sub(1)) * self.list.page_size) as usize;
        for v in filtered
            .into_iter()
            .skip(start)
            .take(self.list.page_size as usize)
        {
            let mut row = FlexRow::new(v.id.to_string()).cell("code", Cell::text(&v.code));
            let mut company_body = render(ActionLink {
                href: &v.party.path,
                body: Html::text(&v.party.name),
                ..ActionLink::default()
            })?;
            if self.columns.mode() != RowMode::Compact {
                let mut facts = vec![];
                for (key, label, value) in [
                    ("tax_id", "Tax ID", &v.party.tax_id),
                    (
                        "registration_number",
                        "Registration number",
                        &v.party.registration_number,
                    ),
                    ("email", "Email", &v.party.email),
                    ("phone", "Phone", &v.party.phone),
                ] {
                    let value = if v.party.restricted.iter().any(|r| r == key) {
                        render(Restricted::new(format!("vendor-{}-{key}-restricted", v.id)))?
                    } else {
                        Html::text(value.as_deref().unwrap_or("—"))
                    };
                    facts.push(Fact {
                        title: label,
                        value,
                        ..Fact::default()
                    });
                }
                let billing = match self
                    .read(
                        s,
                        if v.party.kind == "person" {
                            "core.address.person.list"
                        } else {
                            "core.address.company.list"
                        },
                        if v.party.kind == "person" {
                            json!({"person_id":v.party.id})
                        } else {
                            json!({"company_id":v.party.id})
                        },
                    )
                    .await
                {
                    Ok(rows) => {
                        let lines = bilibi_finance_ledger::party_billing_lines(
                            &rows,
                            &self.pages.today(s).await?.to_string(),
                        );
                        Html::text(&if lines.is_empty() {
                            "No billing address yet".into()
                        } else {
                            lines.join(", ")
                        })
                    }
                    Err(Error::Forbidden | Error::Unauthorized) => render(
                        Restricted::new(format!("vendor-{}-billing-restricted", v.id))
                            .reason("Viewing addresses needs permission."),
                    )?,
                    Err(e) => return Err(e),
                };
                facts.push(Fact {
                    title: "Billing address",
                    value: billing,
                    ..Fact::default()
                });
                company_body.push(&render(Fold {
                    id: &format!("vendor-{}-company-details", v.id),
                    title: "Company details",
                    summary: "",
                    open: false,
                    body: render(Facts {
                        id: &format!("vendor-{}-facts", v.id),
                        items: facts,
                    })?,
                })?);
            }
            row = row.slot("company", company_body);
            row = row.slot(
                "terms",
                render(InlineEdit {
                    field: self.inline(
                        v,
                        "payment_terms_days",
                        "Payment terms",
                        v.payment_terms_days.to_string(),
                    ),
                })?,
            );
            for (key, label, current, kind) in [
                (
                    "payable_account_id",
                    "Payable account",
                    v.payable_account_id,
                    "liability",
                ),
                (
                    "expense_account_id",
                    "Default expense",
                    v.expense_account_id,
                    "expense",
                ),
            ] {
                let choices = accounts
                    .iter()
                    .filter(|a| {
                        a["account_type"] == kind
                            || (kind == "expense" && a["account_type"] == "asset")
                    })
                    .map(|a| {
                        Choice::new(a["name"].as_str().unwrap_or(""), a["id"].to_string())
                            .detail(a["code"].as_str().unwrap_or(""))
                    })
                    .collect();
                let display = accounts
                    .iter()
                    .find(|a| a["id"].as_i64() == current)
                    .map(|a| {
                        Html::text(&format!(
                            "{} · {}",
                            a["name"].as_str().unwrap_or(""),
                            a["code"].as_str().unwrap_or("")
                        ))
                    })
                    .unwrap_or_else(|| {
                        Html::text(&current.map_or_else(
                            || {
                                if kind == "liability" {
                                    "Company default"
                                } else {
                                    "No default"
                                }
                                .into()
                            },
                            |id| format!("Account {id}"),
                        ))
                    });
                let editor = render(
                    Combobox::new(format!("vendor-{}-{key}-input", v.id), key)
                        .aria_label(label)
                        .value(current.map(|i| i.to_string()).unwrap_or_default())
                        .options(choices),
                )?;
                row = row.slot(
                    if kind == "liability" {
                        "payable"
                    } else {
                        "expense"
                    },
                    render(InlineChoice {
                        field: self.inline(
                            v,
                            key,
                            label,
                            current.map(|i| i.to_string()).unwrap_or_default(),
                        ),
                        display,
                        editor,
                    })?,
                );
            }
            let options = taxes
                .iter()
                .filter(|t| t["active"] == true)
                .map(|t| {
                    Choice::new(t["name"].as_str().unwrap_or(""), t["id"].to_string())
                        .detail(t["code"].as_str().unwrap_or(""))
                })
                .collect();
            row = row.slot(
                "tax",
                render(InlineChoice {
                    field: self.inline(
                        v,
                        "tax_code_id",
                        "Default tax",
                        v.tax_code_id.map(|id| id.to_string()).unwrap_or_default(),
                    ),
                    display: Html::text(
                        taxes
                            .iter()
                            .find(|t| t["id"].as_i64() == v.tax_code_id)
                            .and_then(|t| t["name"].as_str())
                            .unwrap_or("No default"),
                    ),
                    editor: render(
                        Combobox::new(format!("vendor-{}-tax_code_id-input", v.id), "tax_code_id")
                            .aria_label("Default tax")
                            .value(v.tax_code_id.map(|id| id.to_string()).unwrap_or_default())
                            .options(options),
                    )?,
                })?,
            );
            row = row.slot(
                "active",
                render(InlineChoice {
                    field: self.inline(v, "active", "Status", v.active.to_string()),
                    display: bilibi_ui::active_badge(v.active),
                    editor: render(
                        Field::named("active")
                            .id(format!("vendor-{}-active-input", v.id))
                            .label("Status")
                            .kind(InputKind::Select)
                            .value(v.active.to_string())
                            .options(vec![
                                Choice::new("Active", "true"),
                                Choice::new("Inactive", "false"),
                            ]),
                    )?,
                })?,
            );
            let relation = bilibi_finance_ledger::party_relation(
                &self.context(),
                s,
                &v.party.kind,
                v.party.id,
            )
            .await?;
            let relation = match relation {
                // Recorded is the normal case and says nothing; only a missing
                // relationship needs the person.
                bilibi_finance_ledger::PartyRelation::Recorded { .. } => Html::EMPTY,
                bilibi_finance_ledger::PartyRelation::Hidden => {
                    Html::text("No access to company relationships")
                }
                bilibi_finance_ledger::PartyRelation::Missing if self.can_manage => Self::button(
                    &format!("vendor-{}-relationship", v.id),
                    "record_vendor_relationship",
                    "Record as supplier",
                    "vendor",
                    &v.id.to_string(),
                )?,
                _ => Html::text("Not recorded"),
            };
            row = row.slot("relationship", relation);
            rows.push(row);
        }
        let mut table = FlexTable::new("payables-vendors-table", self.columns.flex_columns(), rows);
        table.caption = "Suppliers";
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(render(EmptyState::missing(
            "No suppliers yet",
            "Add a supplier before recording a bill.",
        ))?));
        body.push(&render(table)?);
        body.push(&render(Pagination::new(
            "vendor-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &[25, 50, 100],
        ))?);
        body.push(&render(SectionHeading{class:"mt-6",title:"Supplier keys on old bills",description:"Match each key to a supplier after checking its details and similar keys. A match cannot be undone. Bills, payment keys and aging groups stay as they are.",..SectionHeading::default()})?);
        let mut rows = vec![];
        for h in &self.history {
            let snapshots = h
                .snapshots
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    format!(
                        "{} | Tax ID: {} | Registration: {} | Address: {}",
                        v["name"].as_str().unwrap_or("—"),
                        v["tax_id"].as_str().unwrap_or("—"),
                        v["registration_id"].as_str().unwrap_or("—"),
                        v["address"].as_str().unwrap_or("—")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            let matched = match h.vendor_id {
                Some(id) => Html::text(&self.all.iter().find(|v| v.id == id).map_or_else(
                    || format!("Supplier {id}"),
                    |v| format!("{} · {}", v.code, v.party.name),
                )),
                None if self.can_manage => Self::button(
                    &format!("vendor-key-{}", h.supplier_key),
                    "show_mapping",
                    "Review match",
                    "key",
                    &h.supplier_key,
                )?,
                None => Html::text("Unmatched: review required"),
            };
            rows.push(
                FlexRow::new(&h.supplier_key)
                    .cell(
                        "key",
                        Cell::text(format!("{} · {} documents", h.supplier_key, h.documents)),
                    )
                    .cell("facts", Cell::text(snapshots))
                    .cell("collisions", Cell::text(h.collision_keys.join(", ")))
                    .slot("vendor", matched),
            );
        }
        let mut history = FlexTable::new(
            "payables-vendor-keys",
            self.history_columns.flex_columns(),
            rows,
        );
        history.caption = "Historical supplier keys";
        history.event = "keys_grid";
        history.zoom = self.history_columns.zoom;
        body.push(&render(history)?);
        if let Some(create) = &self.create {
            body.push(&self.create_dialog(create)?);
        }
        if let Some((key, values, errors)) = &self.mapping {
            body.push(&self.map_dialog(key, values, errors)?);
        }
        let html = render(bilibi_ui::Page {
            id: "payables-vendors",
            variant: PageVariant::List,
            class: "",
            body,
        })?;
        // Companies and contacts dropped on the page open the same dialog as
        // "Add vendor", so the tile accepts them only while the button shows.
        let zone = self
            .pages
            .records
            .drop_target(super::VENDOR_TARGET)
            .filter(|_| self.can_manage);
        Ok(match zone {
            Some(target) => target
                .zone()
                .wrap("payables-vendors-drop", html)
                .as_str()
                .to_owned(),
            None => html.as_str().to_owned(),
        })
    }
    fn modal(&self, id: &str, title: &str, body: Html, event: &str, label: &str) -> Result<Html> {
        let mut actions = Self::button(&format!("{id}-cancel"), "cancel_vendor", "Cancel", "", "")?;
        actions.push(&render(Button {
            id: &format!("{id}-submit"),
            event,
            button_type: "button",
            variant: ButtonVariant::Primary,
            body: Html::text(label),
            ..Button::default()
        })?);
        render(Modal {
            id,
            title,
            cancel_event: "cancel_vendor",
            width: ModalWidth::Wide,
            description: &format!("For {}", self.company_name),
            role: "dialog",
            flash: None,
            body: render(Form {
                id: &format!("{id}-form"),
                event: "vendor_change",
                change: true,
                class: "mt-4",
                body,
                actions: render(Actions { body: actions })?,
            })?,
        })
    }
    fn create_dialog(&self, c: &Create) -> Result<Html> {
        let mut labels = c.chooser.labels();
        labels.push((
            "payment_terms_days".into(),
            "vendor-payment_terms_days".into(),
            "Payment terms".into(),
        ));
        let mut body = render(c.errors.summary("vendor-errors", &labels))?;
        body.push(&c.chooser.render(&c.errors)?);
        body.push(&render(
            Field::in_form(FORM, "payment_terms_days")
                .id("vendor-payment_terms_days")
                .label("Payment terms (days)")
                .kind(InputKind::Number)
                .attr("min", "0")
                .attr("max", "3650")
                .value(
                    c.values
                        .get("payment_terms_days")
                        .cloned()
                        .unwrap_or_default(),
                )
                .errors(c.errors.under("payment_terms_days", "Payment terms")),
        )?);
        self.modal(
            "vendor-new-modal",
            "Add suppliers",
            body,
            "create_vendor",
            if c.chooser.count() == 1 {
                "Add supplier"
            } else {
                "Add suppliers"
            },
        )
    }
    fn map_dialog(
        &self,
        key: &str,
        values: &HashMap<String, String>,
        errors: &PartyErrors,
    ) -> Result<Html> {
        let mut body = render(errors.summary(
            "vendor-map-errors",
            &[
                (
                    "vendor_id".into(),
                    "vendor-map-choice".into(),
                    "Supplier".into(),
                ),
                ("reason".into(), "vendor-map-reason".into(), "Reason".into()),
            ],
        ))?;
        body.push(&Html::text(&format!(
            "Supplier key: {key}. Review all recorded facts in the row before confirming."
        )));
        body.push(&render(
            Combobox::new("vendor-map-choice", format!("{FORM}[vendor_id]"))
                .label("Supplier *")
                .aria_label("Supplier")
                .attrs(bilibi_ui::Attrs {
                    required: true,
                    aria_label: Some("Supplier".into()),
                    ..bilibi_ui::Attrs::default()
                })
                .value(values.get("vendor_id").cloned().unwrap_or_default())
                .options(
                    self.all
                        .iter()
                        .map(|v| Choice::new(&v.party.name, v.id.to_string()).detail(&v.code))
                        .collect(),
                )
                .errors(errors.under("vendor_id", "Supplier")),
        )?);
        body.push(&render(
            Field::in_form(FORM, "reason")
                .id("vendor-map-reason")
                .label("Reason for this match")
                .required()
                .value(values.get("reason").cloned().unwrap_or_default())
                .errors(errors.under("reason", "Reason")),
        )?);
        self.modal(
            "vendor-map-modal",
            "Confirm supplier match",
            body,
            "confirm_vendor_mapping",
            "Confirm match",
        )
    }
    async fn take(&mut self, _s: &Scope, v: &Value) -> Result<()> {
        let form = params::form_values(v, FORM);
        if let Some(c) = &mut self.create {
            c.values.extend(form.clone());
            c.chooser.read(&form);
        }
        if let Some((_, values, _)) = &mut self.mapping {
            values.extend(form);
        }
        Ok(())
    }
    async fn event(&mut self, s: &Scope, credential: &str, v: &Value) -> Result<LiveBody> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        let context = self.context();
        if matches!(
            event,
            "create_vendor"
                | "save_vendor"
                | "confirm_vendor_mapping"
                | "record_vendor_relationship"
                | "show_mapping"
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
            "keys_grid" => {
                self.history_columns.handle(&params::string_values(v));
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
                self.mapping = None;
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
            "show_mapping" => {
                let key = v["key"].as_str().ok_or(Error::Invalid)?;
                if self
                    .history
                    .iter()
                    .any(|h| h.supplier_key == key && h.vendor_id.is_none())
                {
                    self.mapping = Some((key.into(), HashMap::new(), PartyErrors::default()));
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
            "confirm_vendor_mapping" => {
                self.take(s, v).await?;
                if let Some((key, values, errors)) = &mut self.mapping {
                    let input = json!({"company_id":company(s),"vendor_id":values.get("vendor_id").and_then(|v|v.parse::<i64>().ok()),"supplier_key":key,"reason":values.get("reason").cloned().unwrap_or_default()});
                    match self
                        .pages
                        .write(
                            s,
                            credential,
                            event,
                            "finance.payables.vendor.map_supplier_key",
                            input,
                        )
                        .await?
                    {
                        Written::Done(_) => {
                            self.mapping = None;
                            self.notice=Some((StatusKind::Success,"Supplier match confirmed. Historical records and balances are unchanged.".into()));
                        }
                        Written::Fields(e) => *errors = PartyErrors(e),
                        _ => {
                            *errors = PartyErrors(BTreeMap::from([(
                                "base".into(),
                                vec![
                                    "Match was not confirmed. Check your permission and try again."
                                        .into(),
                                ],
                            )]))
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
#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex w-full justify-end gap-2">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    body: Html,
}
