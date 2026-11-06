//! Payables setup: the account bills are owed on, the matching tolerances and
//! the company's tax codes. Bilimbi configured these through its API; this
//! page makes the same changes through `finance.payables.settings.put` and
//! `.tax_code.create` / `.update`, the contracts the agent doors call.
use super::{company, format as f, render, Pages, Written};
use crate::{Settings, TaxCode, Tolerance, ToleranceKind, MANAGE_SETTINGS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountType};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    error_sentence,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BadgeKind, BuiltinColumn, Button, ButtonVariant, Cell, Choice, Combobox,
    ErrorSummary, Facts, Field, FlashMap, FlexRow, FlexTable, Html, InputKind, ListColumns,
    StatusKind, TableEmpty, NO_FLASH,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
struct SettingsDraft {
    account: String,
    quantity_kind: String,
    quantity_value: String,
    price_kind: String,
    price_value: String,
    reason: String,
}

#[derive(Default)]
struct TaxDraft {
    code: String,
    name: String,
    rate: String,
    account: String,
}

pub(super) struct Setup {
    pages: Pages,
    settings: Option<Settings>,
    taxes: Vec<TaxCode>,
    accounts: Vec<Account>,
    can_manage: bool,
    draft: SettingsDraft,
    tax: TaxDraft,
    started: bool,
    settings_errors: Vec<(String, String)>,
    tax_errors: Vec<(String, String)>,
    notice: Option<(&'static str, String)>,
    flash: Option<(StatusKind, String)>,
    columns: ListColumns,
}

fn kind_name(t: &Option<Tolerance>) -> &'static str {
    match t.as_ref().map(|t| t.kind) {
        None => "",
        Some(ToleranceKind::Percent) => "percent",
        Some(ToleranceKind::Amount) => "amount",
    }
}

fn tolerance_text(t: &Option<Tolerance>) -> String {
    match t {
        None => "Exact".into(),
        Some(t) if t.kind == ToleranceKind::Percent => format!("{}%", t.value.normalize()),
        Some(t) => format!("{} per unit", t.value.normalize()),
    }
}

impl Setup {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            settings: None,
            taxes: vec![],
            accounts: vec![],
            can_manage: false,
            draft: SettingsDraft::default(),
            tax: TaxDraft::default(),
            started: false,
            settings_errors: vec![],
            tax_errors: vec![],
            notice: None,
            flash: None,
            columns: ListColumns::mount(
                "payables-tax-codes-table",
                [
                    ("code", "Code"),
                    ("name", "Name"),
                    ("rate", "Rate"),
                    ("account", "Account"),
                    ("status", "Status"),
                ]
                .into_iter()
                .map(|(id, label)| {
                    let c = BuiltinColumn::new(id, label);
                    if id == "rate" {
                        c.right()
                    } else {
                        c
                    }
                })
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.can_manage = self.pages.allowed(scope, MANAGE_SETTINGS).await?;
        self.settings = None;
        self.taxes.clear();
        self.accounts.clear();
        let Some(company) = company(scope) else {
            return Ok(());
        };
        self.settings = self.pages.payables.get_settings(scope, company).await.ok();
        self.taxes = self
            .pages
            .payables
            .list_tax_codes(scope, company)
            .await
            .unwrap_or_default();
        self.accounts = self
            .pages
            .ledger
            .list_accounts(scope, company)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.postable())
            .collect();
        if !self.started {
            if let Some(s) = &self.settings {
                self.draft = SettingsDraft {
                    account: s.payable_account_id.to_string(),
                    quantity_kind: kind_name(&s.quantity_tolerance).into(),
                    quantity_value: s
                        .quantity_tolerance
                        .as_ref()
                        .map(|t| t.value.normalize().to_string())
                        .unwrap_or_default(),
                    price_kind: kind_name(&s.price_tolerance).into(),
                    price_value: s
                        .price_tolerance
                        .as_ref()
                        .map(|t| t.value.normalize().to_string())
                        .unwrap_or_default(),
                    reason: String::new(),
                };
            }
            self.started = true;
        }
        self.columns.load(&self.taxes, |t| t.id.to_string());
        Ok(())
    }

    fn account_label(&self, id: i64) -> String {
        self.accounts.iter().find(|a| a.id == id).map_or_else(
            || "Account unavailable".into(),
            |a| format!("{} {}", a.code, a.name),
        )
    }

    fn errors_for(errors: &[(String, String)], path: &str, label: &str) -> Vec<String> {
        errors
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, m)| error_sentence(label, m))
            .collect()
    }

    pub(super) fn body(&self) -> Result<String> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let account_choices = |types: &[AccountType]| -> Vec<Choice> {
            self.accounts
                .iter()
                .filter(|a| types.is_empty() || types.contains(&a.account_type))
                .map(|a| Choice::new(&a.name, a.id.to_string()).detail(&a.code))
                .collect()
        };
        // What is saved, read first.
        let saved = match &self.settings {
            Some(s) => render(Facts {
                id: "payables-settings-facts",
                items: vec![
                    f::fact("Payable account", &self.account_label(s.payable_account_id)),
                    f::fact("Quantity tolerance", &tolerance_text(&s.quantity_tolerance)),
                    f::fact("Price tolerance", &tolerance_text(&s.price_tolerance)),
                ],
            })?,
            None => Html::text(""),
        };
        let kind_choices = || {
            vec![
                Choice::new("Exact", ""),
                Choice::new("Percent of the ordered figure", "percent"),
                Choice::new("Amount per unit", "amount"),
            ]
        };
        let d = &self.draft;
        let mut settings_fields = Vec::new();
        let field = |key: &str, id: &str, label: &str, value: &str, path: &str| {
            Field::in_form("settings", key)
                .id(format!("payables-settings-{id}"))
                .label(label)
                .value(value)
                .errors(Self::errors_for(&self.settings_errors, path, label))
        };
        settings_fields.push(
            field(
                "quantity_kind",
                "quantity-kind",
                "Quantity tolerance",
                &d.quantity_kind,
                "quantity_tolerance",
            )
            .kind(InputKind::Select)
            .options(kind_choices())
            .hint("How far a bill's quantity may differ from the order before it is held."),
        );
        if !d.quantity_kind.is_empty() {
            settings_fields.push(
                field(
                    "quantity_value",
                    "quantity-value",
                    "Quantity tolerance value",
                    &d.quantity_value,
                    "quantity_tolerance.value",
                )
                .required(),
            );
        }
        settings_fields.push(
            field(
                "price_kind",
                "price-kind",
                "Price tolerance",
                &d.price_kind,
                "price_tolerance",
            )
            .kind(InputKind::Select)
            .options(kind_choices())
            .hint("How far a bill's unit price may differ from the order before it is held."),
        );
        if !d.price_kind.is_empty() {
            settings_fields.push(
                field(
                    "price_value",
                    "price-value",
                    "Price tolerance value",
                    &d.price_value,
                    "price_tolerance.value",
                )
                .required(),
            );
        }
        settings_fields.push(
            field(
                "reason",
                "reason",
                "Reason for the change (optional)",
                &d.reason,
                "reason",
            )
            .wrapper_class("md:col-span-2"),
        );
        let payable = Combobox::new("payables-settings-account", "settings[account]".to_owned())
            .label("Payable account")
            .value(d.account.clone())
            .placeholder("Search liability accounts")
            .options(account_choices(&[AccountType::Liability]))
            .errors(Self::errors_for(
                &self.settings_errors,
                "payable_account_id",
                "Payable account",
            ))
            .hint(
                "The liability account, such as Trade Payables, that every posted bill is owed on.",
            );
        let save_settings = render(Button {
            id: "payables-settings-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save settings"),
            ..Button::default()
        })?;
        let settings_summary =
            self.summary("payables-settings-errors", &self.settings_errors, &|p| {
                Some((
                    match p {
                        "payable_account_id" => "payables-settings-account",
                        "quantity_tolerance" | "quantity_tolerance.value" => {
                            "payables-settings-quantity-value"
                        }
                        "price_tolerance" | "price_tolerance.value" => {
                            "payables-settings-price-value"
                        }
                        "reason" => "payables-settings-reason",
                        _ => return None,
                    },
                    match p {
                        "payable_account_id" => "Payable account",
                        "quantity_tolerance" | "quantity_tolerance.value" => "Quantity tolerance",
                        "price_tolerance" | "price_tolerance.value" => "Price tolerance",
                        _ => "Reason",
                    },
                ))
            })?;
        // Tax codes.
        let mut rows = Vec::new();
        for t in &self.taxes {
            let mut row = FlexRow::new(t.id.to_string());
            row.dom_id = Some(format!("payables-tax-code-{}", t.id));
            row = row
                .cell("code", Cell::text(&t.code))
                .cell("name", Cell::text(&t.name))
                .cell("rate", Cell::text(format!("{}%", t.rate.normalize())))
                .cell("account", Cell::text(self.account_label(t.account_id)))
                .slot(
                    "status",
                    f::badge(
                        if t.active { "in use" } else { "off" },
                        if t.active {
                            BadgeKind::Success
                        } else {
                            BadgeKind::Neutral
                        },
                        true,
                    )?,
                );
            if self.can_manage {
                row.actions = render(Button {
                    id: &format!("payables-tax-code-{}-toggle", t.id),
                    button_type: "button",
                    event: "toggle_tax_code",
                    name: "tax_code",
                    value: &format!("{}:{}", t.id, !t.active),
                    working: "Saving…",
                    class: "whitespace-nowrap",
                    body: Html::text(if t.active { "Turn off" } else { "Turn on" }),
                    ..Button::default()
                })?;
            }
            rows.push(row);
        }
        let mut table = FlexTable::new(
            "payables-tax-codes-table",
            self.columns.flex_columns(),
            rows,
        );
        table.event = "tax_codes_grid";
        table.caption = "Tax codes";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.actions = self.can_manage;
        table.empty = Some(TableEmpty::Content(Html::text(
            "No tax codes yet. Add one to charge tax on bill lines.",
        )));
        let taxes = render(table)?;
        let t = &self.tax;
        let tax_field = |key: &str, id: &str, label: &str, value: &str, path: &str| {
            Field::in_form("tax", key)
                .id(format!("payables-tax-{id}"))
                .label(label)
                .value(value)
                .errors(Self::errors_for(&self.tax_errors, path, label))
        };
        let tax_fields = vec![
            tax_field("code", "code", "Code", &t.code, "code")
                .required()
                .hint("Short and permanent, such as SST-6."),
            tax_field("name", "name", "Name", &t.name, "name").required(),
            tax_field("rate", "rate", "Rate (%)", &t.rate, "rate")
                .required()
                .attr("inputmode", "decimal"),
        ];
        let tax_account = Combobox::new("payables-tax-account", "tax[account]".to_owned())
            .label("Tax is kept in")
            .value(t.account.clone())
            .placeholder("Search accounts")
            .options(account_choices(&[]))
            .errors(Self::errors_for(
                &self.tax_errors,
                "account_id",
                "Tax is kept in",
            ))
            .hint("The account the tax on a posted bill is debited to.");
        let tax_summary = self.summary("payables-tax-errors", &self.tax_errors, &|p| {
            Some((
                match p {
                    "code" => "payables-tax-code",
                    "name" => "payables-tax-name",
                    "rate" => "payables-tax-rate",
                    "account_id" => "payables-tax-account",
                    _ => return None,
                },
                match p {
                    "code" => "Code",
                    "name" => "Name",
                    "rate" => "Rate",
                    _ => "Tax is kept in",
                },
            ))
        })?;
        let add_tax = render(Button {
            id: "payables-tax-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Adding…",
            body: Html::text("Add tax code"),
            ..Button::default()
        })?;
        let notice = |which: &str| {
            self.notice
                .as_ref()
                .filter(|(w, _)| *w == which)
                .map(|(_, message)| {
                    render(Alert {
                        kind: StatusKind::Error,
                        id: if which == "settings" {
                            "payables-settings-notice"
                        } else {
                            "payables-tax-notice"
                        },
                        body: Html::text(message),
                        ..Alert::default()
                    })
                })
                .transpose()
        };
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            flash,
            configured: self.settings.is_some(),
            can_manage: self.can_manage,
            saved,
            settings_summary,
            settings_notice: notice("settings")?,
            settings_fields,
            payable,
            save_settings,
            taxes,
            tax_summary,
            tax_notice: notice("tax")?,
            tax_fields,
            tax_account,
            add_tax,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn summary(
        &self,
        id: &'static str,
        errors: &[(String, String)],
        locate: &dyn Fn(&str) -> Option<(&'static str, &'static str)>,
    ) -> Result<Option<Html>> {
        if errors.is_empty() {
            return Ok(None);
        }
        let mut summary = ErrorSummary::new(id);
        let mut any = false;
        for (path, message) in errors {
            if let Some((element, label)) = locate(path) {
                summary.push(element, error_sentence(label, message));
                any = true;
            }
        }
        any.then(|| render(summary)).transpose()
    }

    fn put_errors(
        into: &mut Vec<(String, String)>,
        fields: BTreeMap<String, Vec<String>>,
    ) -> Vec<String> {
        into.clear();
        let mut loose = Vec::new();
        for (path, messages) in fields {
            let known = matches!(
                path.as_str(),
                "payable_account_id"
                    | "quantity_tolerance"
                    | "quantity_tolerance.value"
                    | "price_tolerance"
                    | "price_tolerance.value"
                    | "reason"
                    | "code"
                    | "name"
                    | "rate"
                    | "account_id"
            );
            for m in messages {
                if known {
                    into.push((path.clone(), m));
                } else {
                    loose.push(m);
                }
            }
        }
        loose
    }

    async fn save_settings(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        self.settings_errors.clear();
        self.notice = None;
        let Some(company) = company(scope) else {
            return Ok(());
        };
        let d = &self.draft;
        let mut input = json!({"company_id": company});
        if let Ok(id) = d.account.trim().parse::<i64>() {
            input["payable_account_id"] = json!(id);
        }
        for (name, clear, kind, value) in [
            (
                "quantity_tolerance",
                "clear_quantity_tolerance",
                &d.quantity_kind,
                &d.quantity_value,
            ),
            (
                "price_tolerance",
                "clear_price_tolerance",
                &d.price_kind,
                &d.price_value,
            ),
        ] {
            if kind.is_empty() {
                input[clear] = json!(true);
            } else {
                input[name] = json!({"kind": kind, "value": value.trim()});
            }
        }
        if !d.reason.trim().is_empty() {
            input["reason"] = json!(d.reason.trim());
        }
        match self
            .pages
            .write(
                scope,
                credential,
                "save_settings",
                "finance.payables.settings.put",
                input,
            )
            .await?
        {
            Written::Done(_) => {
                self.flash = Some((StatusKind::Success, "Payables settings saved.".into()));
                self.draft.reason.clear();
                self.started = false;
            }
            Written::Forbidden => {
                self.notice = Some((
                    "settings",
                    "You do not have permission to change the payables settings.".into(),
                ));
            }
            Written::Changed => {
                self.notice = Some((
                    "settings",
                    "Nothing was saved because something changed. Check the form and try again."
                        .into(),
                ));
            }
            Written::Fields(fields) => {
                let loose = Self::put_errors(&mut self.settings_errors, fields);
                if !loose.is_empty() || self.settings_errors.is_empty() {
                    self.notice = Some((
                        "settings",
                        if loose.is_empty() {
                            "The settings were not saved. Check the form and try again.".into()
                        } else {
                            loose.join(" ")
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    async fn add_tax(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        self.tax_errors.clear();
        self.notice = None;
        let Some(company) = company(scope) else {
            return Ok(());
        };
        let t = &self.tax;
        let mut input = json!({"company_id": company, "active": true});
        for (key, text) in [("code", &t.code), ("name", &t.name), ("rate", &t.rate)] {
            if !text.trim().is_empty() {
                input[key] = json!(text.trim());
            }
        }
        if let Ok(id) = t.account.trim().parse::<i64>() {
            input["account_id"] = json!(id);
        }
        match self
            .pages
            .write(
                scope,
                credential,
                "save_tax_code",
                "finance.payables.tax_code.create",
                input,
            )
            .await?
        {
            Written::Done(_) => {
                self.flash = Some((StatusKind::Success, "Tax code added.".into()));
                self.tax = TaxDraft::default();
            }
            Written::Forbidden => {
                self.notice = Some(("tax", "You do not have permission to add tax codes.".into()));
            }
            Written::Changed => {
                self.notice = Some((
                    "tax",
                    "Nothing was saved because something changed. Check the form and try again."
                        .into(),
                ));
            }
            Written::Fields(fields) => {
                let loose = Self::put_errors(&mut self.tax_errors, fields);
                if !loose.is_empty() || self.tax_errors.is_empty() {
                    self.notice = Some((
                        "tax",
                        if loose.is_empty() {
                            "The tax code was not added. Check the form and try again.".into()
                        } else {
                            loose.join(" ")
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<bool> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        match event {
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| v["key"].as_str() == Some(kind.name()))
                {
                    self.flash = None;
                }
                Ok(false)
            }
            "tax_codes_grid" => {
                self.columns.handle(&params::string_values(v));
                Ok(false)
            }
            "settings_edit" | "save_settings" => {
                let values = params::form_values(v, "settings");
                let d = &mut self.draft;
                for (key, into) in [
                    ("account", &mut d.account),
                    ("quantity_kind", &mut d.quantity_kind),
                    ("quantity_value", &mut d.quantity_value),
                    ("price_kind", &mut d.price_kind),
                    ("price_value", &mut d.price_value),
                    ("reason", &mut d.reason),
                ] {
                    if let Some(text) = values.get(key) {
                        *into = text.clone();
                    }
                }
                if event == "save_settings" {
                    self.flash = None;
                    if self.pages.allowed(scope, MANAGE_SETTINGS).await? {
                        self.save_settings(scope, credential).await?;
                        return Ok(true);
                    }
                    self.notice = Some((
                        "settings",
                        "You do not have permission to change the payables settings.".into(),
                    ));
                }
                Ok(false)
            }
            "tax_edit" | "save_tax_code" => {
                let values = params::form_values(v, "tax");
                let t = &mut self.tax;
                for (key, into) in [
                    ("code", &mut t.code),
                    ("name", &mut t.name),
                    ("rate", &mut t.rate),
                    ("account", &mut t.account),
                ] {
                    if let Some(text) = values.get(key) {
                        *into = text.clone();
                    }
                }
                if event == "save_tax_code" {
                    self.flash = None;
                    if self.pages.allowed(scope, MANAGE_SETTINGS).await? {
                        self.add_tax(scope, credential).await?;
                        return Ok(true);
                    }
                    self.notice =
                        Some(("tax", "You do not have permission to add tax codes.".into()));
                }
                Ok(false)
            }
            "toggle_tax_code" => {
                let Some((id, to)) = v["tax_code"]
                    .as_str()
                    .and_then(|s| s.split_once(':'))
                    .and_then(|(id, to)| Some((id.parse::<i64>().ok()?, to == "true")))
                else {
                    return Err(Error::Invalid);
                };
                let Some(company) = company(scope) else {
                    return Ok(false);
                };
                self.flash = Some(
                    match self
                        .pages
                        .write(
                            scope,
                            credential,
                            "toggle_tax_code",
                            "finance.payables.tax_code.update",
                            json!({"company_id": company, "tax_code_id": id, "active": to}),
                        )
                        .await?
                    {
                        Written::Done(_) => (
                            StatusKind::Success,
                            if to {
                                "Tax code turned on."
                            } else {
                                "Tax code turned off."
                            }
                            .into(),
                        ),
                        Written::Forbidden => (
                            StatusKind::Error,
                            "You do not have permission to change tax codes.".into(),
                        ),
                        Written::Changed => (
                            StatusKind::Error,
                            "The tax code changed before this went through. Try again.".into(),
                        ),
                        Written::Fields(fields) => {
                            (StatusKind::Error, f::refused("The change", &fields))
                        }
                    },
                );
                Ok(true)
            }
            _ => Err(Error::Invalid),
        }
    }
}

impl LiveState<Scope, Error> for Setup {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => true,
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await?,
            };
            if reload {
                self.reload(r.scope).await?;
            }
            Ok(LiveBody {
                html: self.body()?,
                ..LiveBody::default()
            })
        })
    }
}

#[derive(Template)]
#[template(path = "setup.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    configured: bool,
    can_manage: bool,
    saved: Html,
    settings_summary: Option<Html>,
    settings_notice: Option<Html>,
    settings_fields: Vec<Field>,
    payable: Combobox,
    save_settings: Html,
    taxes: Html,
    tax_summary: Option<Html>,
    tax_notice: Option<Html>,
    tax_fields: Vec<Field>,
    tax_account: Combobox,
    add_tax: Html,
}
