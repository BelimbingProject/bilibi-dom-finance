//! Payables setup: the account bills are owed on, the matching tolerances and
//! the company's tax codes, each edited in place as Receivables settings are
//! (`receivables/src/web/settings.rs`): an account or a tolerance saves when
//! it is changed, a tax code's name, rate and account edit in its row and its
//! Active is a toggle, and a new tax code opens a dialog. Every change calls
//! `finance.payables.settings.put` or `.tax_code.create` / `.update`, the
//! contracts the agent doors call; the account's change history is folded
//! under the settings.
use super::{company, format as f, render, Pages, Written};
use crate::{Settings, TaxCode, Tolerance, ToleranceKind, MANAGE_SETTINGS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountName, AccountType, People};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    error_sentence,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice, Combobox,
    CommitStates, CommitStatus, ErrorSummary, Facts, Field, FlashMap, FlexRow, FlexTable, Fold,
    Form, Header, Html, InlineChoice, InlineEdit, InlineField, ListColumns, Modal, ModalWidth,
    SectionHeading, StatusKind, TableEmpty, Toggle, NO_FLASH,
};
use rust_decimal::Decimal;
use serde_json::{json, Value};

/// The New tax code dialog while it is open.
#[derive(Default)]
struct TaxDraft {
    code: String,
    name: String,
    rate: String,
    account: String,
    errors: Vec<(String, String)>,
    notice: Option<String>,
}

pub(super) struct Setup {
    pages: Pages,
    company: String,
    settings: Option<Settings>,
    taxes: Vec<TaxCode>,
    accounts: Vec<Account>,
    changes: Vec<Value>,
    can_manage: bool,
    statuses: CommitStates,
    tax: Option<TaxDraft>,
    flash: Option<(StatusKind, String)>,
    columns: ListColumns,
    history: ListColumns,
}

fn tolerance_text(t: &Option<Tolerance>) -> String {
    match t {
        None => "Exact".into(),
        Some(t) if t.kind == ToleranceKind::Percent => format!("{}%", t.value.normalize()),
        Some(t) => format!("{} per unit", t.value.normalize()),
    }
}

/// What the field holds while it is edited: `5%`, `0.5`, or empty for exact.
fn tolerance_value(t: &Option<Tolerance>) -> String {
    match t {
        None => String::new(),
        Some(t) if t.kind == ToleranceKind::Percent => format!("{}%", t.value.normalize()),
        Some(t) => t.value.normalize().to_string(),
    }
}

/// A typed tolerance as the contract takes it: `None` clears it (exact),
/// `5%` is a percentage, `0.50` or `0.50 per unit` an amount per unit.
fn parse_tolerance(text: &str) -> std::result::Result<Option<Value>, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("exact") {
        return Ok(None);
    }
    let (kind, number) = match text.strip_suffix('%') {
        Some(number) => ("percent", number.trim()),
        None => (
            "amount",
            text.strip_suffix("per unit").unwrap_or(text).trim(),
        ),
    };
    match number.parse::<Decimal>() {
        Ok(value) if !value.is_sign_negative() => {
            Ok(Some(json!({"kind": kind, "value": value.to_string()})))
        }
        _ => Err("must be a percentage such as 5%, an amount such as 0.50, or empty for exact"),
    }
}

impl Setup {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        let mount = |id: &str, cols: &[(&str, &str)], right: &[&str]| {
            ListColumns::mount(
                id,
                cols.iter()
                    .map(|(id, label)| {
                        let c = BuiltinColumn::new(id, label);
                        if right.contains(id) {
                            c.right()
                        } else {
                            c
                        }
                    })
                    .collect(),
            )
            .map_err(|_| Error::Internal)
        };
        Ok(Self {
            pages,
            company: String::new(),
            settings: None,
            taxes: vec![],
            accounts: vec![],
            changes: vec![],
            can_manage: false,
            statuses: CommitStates::default(),
            tax: None,
            flash: None,
            columns: mount(
                "payables-tax-codes-table",
                &[
                    ("code", "Code"),
                    ("name", "Name"),
                    ("rate", "Rate"),
                    ("account", "Account"),
                    ("active", "Active"),
                ],
                &["rate"],
            )?,
            history: mount(
                "payables-settings-history-table",
                &[
                    ("when", "When"),
                    ("from", "From"),
                    ("to", "To"),
                    ("reason", "Reason"),
                    ("by", "By"),
                ],
                &[],
            )?,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.can_manage = self.pages.allowed(scope, MANAGE_SETTINGS).await?;
        self.company = self.pages.company_name(scope).await;
        self.settings = None;
        self.taxes.clear();
        self.accounts.clear();
        self.changes.clear();
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
        self.changes = self
            .pages
            .payables
            .list_settings_changes(scope, company, 0, 500)
            .await
            .map(|page| page.rows)
            .unwrap_or_default();
        self.columns.load(&self.taxes, |t| t.id.to_string());
        self.history.load(&self.changes, |c| c["id"].to_string());
        Ok(())
    }

    fn account_label(&self, id: i64) -> String {
        self.accounts.iter().find(|a| a.id == id).map_or_else(
            || "Account unavailable".into(),
            |a| format!("{} · {}", a.name, a.code),
        )
    }

    fn account_choices(&self, types: &[AccountType]) -> Vec<Choice> {
        self.accounts
            .iter()
            .filter(|a| types.is_empty() || types.contains(&a.account_type))
            .map(|a| Choice::new(&a.name, a.id.to_string()).detail(&a.code))
            .collect()
    }

    /// One inline field of this page: its outcome is kept under `key`.
    fn inline(&self, key: &str, field: &str, label: &str, value: String, record: &str) -> InlineField {
        let mut inline = InlineField::new(format!("payables-{key}"), field, label, value)
            .version(0)
            .status(self.statuses.get(key));
        inline.id_value = record.into();
        inline.save_event = if record == "settings" {
            "save_settings"
        } else {
            "save_tax_code"
        }
        .into();
        if !self.can_manage {
            inline = inline.readonly();
        }
        inline
    }

    /// An account edited in place with the combobox.
    fn account_editor(
        &self,
        key: &str,
        field: InlineField,
        label: &str,
        current: Option<i64>,
        types: &[AccountType],
    ) -> Result<Html> {
        let display = match current {
            Some(id) => AccountName::label(&self.account_label(id))?,
            None if self.can_manage => Html::text("Choose an account"),
            None => Html::text("Not set"),
        };
        let editor = Combobox::new(format!("payables-{key}-input"), field.name.clone())
            .aria_label(label)
            .placeholder("Choose an account")
            .wrapper_class("mb-0")
            .value(current.map(|id| id.to_string()).unwrap_or_default())
            .options(self.account_choices(types));
        render(InlineChoice {
            field,
            display,
            editor: render(editor)?,
        })
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let mut body = render(Header {
            title: Html::text("Payables setup"),
            subtitle: &self.company,
            title_actions: Html::EMPTY,
            ..Header::default()
        })?;
        body.push(&self.settings_card(scope).await?);
        body.push(&self.taxes_card()?);
        View {
            flash,
            body,
            dialog: self.tax.as_ref().map(|t| self.tax_dialog(t)).transpose()?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// The payable account and the tolerances, each saved as it changes,
    /// then the account's history folded away.
    async fn settings_card(&self, scope: &Scope) -> Result<Html> {
        let s = self.settings.as_ref();
        let account = s.map(|s| s.payable_account_id);
        let mut field = self.inline(
            "payable-account",
            "payable_account_id",
            "Payable account",
            account.map(|id| id.to_string()).unwrap_or_default(),
            "settings",
        );
        field = field.hint("The liability account every posted bill is owed on.");
        let mut items = vec![fact_html(
            "Payable account",
            self.account_editor(
                "payable-account",
                field,
                "Payable account",
                account,
                &[AccountType::Liability],
            )?,
        )];
        for (key, field, label, value, edit, hint) in [
            (
                "quantity-tolerance",
                "quantity_tolerance",
                "Quantity tolerance",
                s.map(|s| tolerance_text(&s.quantity_tolerance)),
                s.map(|s| tolerance_value(&s.quantity_tolerance)),
                "How far a bill's quantity may differ from the order: 5%, 0.50 per unit, or empty for exact.",
            ),
            (
                "price-tolerance",
                "price_tolerance",
                "Price tolerance",
                s.map(|s| tolerance_text(&s.price_tolerance)),
                s.map(|s| tolerance_value(&s.price_tolerance)),
                "How far a bill's unit price may differ from the order: 5%, 0.50 per unit, or empty for exact.",
            ),
        ] {
            let mut inline = self
                .inline(key, field, label, edit.unwrap_or_default(), "settings")
                .allow_empty()
                .placeholder("Exact");
            inline.display_value = value;
            inline = if s.is_some() {
                inline.hint(hint)
            } else {
                inline.readonly().hint("Choose the payable account first.")
            };
            items.push(fact_html(label, render(InlineEdit { field: inline })?));
        }
        let mut heading = render(SectionHeading {
            id: "payables-settings-heading",
            title: "Payable account and matching",
            title_actions: if s.is_none() {
                render(Badge::status(BadgeKind::Warning, "Not set"))?
            } else {
                Html::EMPTY
            },
            description: "Bills post to this account. A bill held by the three-way match waits for these tolerances.",
            ..SectionHeading::default()
        })?;
        let mut b = render(Facts {
            id: "payables-settings-facts",
            items,
        })?;
        b.push(&self.history(scope).await?);
        heading.push(&render(Card {
            id: "payables-settings",
            role: "region",
            labelledby: "payables-settings-heading",
            body: b,
            ..Card::default()
        })?);
        Ok(heading)
    }

    /// Changes to the payable account, folded under one line until the
    /// reader asks (`DESIGN.md`: History is a quiet disclosure, not a button).
    async fn history(&self, scope: &Scope) -> Result<Html> {
        let people = self
            .pages
            .people(
                scope,
                self.changes.iter().flat_map(|c| {
                    [
                        c["changed_by_user_id"].as_i64(),
                        c["changed_by_impersonator_id"].as_i64(),
                    ]
                }),
            )
            .await?;
        let display = self.pages.display(scope).await?;
        let mut rows = Vec::new();
        for c in self.changes.iter().rev() {
            let id = c["id"].as_i64().unwrap_or_default();
            let mut row = FlexRow::new(id.to_string());
            row.dom_id = Some(format!("payables-settings-change-{id}"));
            let account = |key: &str| {
                c[key]
                    .as_i64()
                    .map_or_else(|| "—".to_owned(), |a| self.account_label(a))
            };
            rows.push(
                row.slot(
                    "when",
                    f::time(
                        &display,
                        &format!("payables-settings-change-at-{id}"),
                        c["changed_at"].as_str().unwrap_or(""),
                    )?,
                )
                .cell("from", Cell::text(account("old_payable_account_id")))
                .cell("to", Cell::text(account("new_payable_account_id")))
                .cell("reason", Cell::text(c["reason"].as_str().unwrap_or("—")))
                .cell("by", Cell::text(who(&people, c))),
            );
        }
        let summary = match self.changes.len() {
            0 => "No changes yet".to_owned(),
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        };
        let mut table = FlexTable::new(&self.history.id, self.history.flex_columns(), rows);
        table.event = "history_grid";
        table.caption = "Changes to the payables settings";
        table.framed = true;
        table.zoom = self.history.zoom;
        table.suggestions = self.history.flex_suggestions();
        table.add_query = &self.history.add_query;
        table.empty = Some(TableEmpty::Content(Html::text("No changes yet.")));
        render(Fold {
            id: "payables-settings-history",
            title: "History",
            summary: &summary,
            open: false,
            body: render(table)?,
        })
    }

    /// The tax codes, each row edited in place, and "Add tax code".
    fn taxes_card(&self) -> Result<Html> {
        let mut rows = Vec::new();
        for t in &self.taxes {
            let key = |field: &str| format!("tax-{}-{field}", t.id);
            let record = t.id.to_string();
            let mut row = FlexRow::new(t.id.to_string());
            row.dom_id = Some(format!("payables-tax-code-{}", t.id));
            row = row
                .cell("code", Cell::text(&t.code))
                .slot(
                    "name",
                    render(InlineEdit {
                        field: self.inline(&key("name"), "name", "Name", t.name.clone(), &record),
                    })?,
                )
                .slot(
                    "rate",
                    render(InlineEdit {
                        field: {
                            let mut rate = self.inline(
                                &key("rate"),
                                "rate",
                                "Rate (%)",
                                t.rate.normalize().to_string(),
                                &record,
                            );
                            rate.display_value = Some(format!("{}%", t.rate.normalize()));
                            rate
                        },
                    })?,
                )
                .slot(
                    "account",
                    self.account_editor(
                        &key("account"),
                        self.inline(
                            &key("account"),
                            "account_id",
                            "Account",
                            t.account_id.to_string(),
                            &record,
                        ),
                        "Account",
                        Some(t.account_id),
                        &[],
                    )?,
                )
                .slot(
                    "active",
                    render(Toggle {
                        on: t.active,
                        label: &format!("{} active", t.code),
                        id: &format!("payables-tax-code-{}-active", t.id),
                        event: "toggle_tax_code",
                        name: "tax_code",
                        value: &format!("{}:{}", t.id, !t.active),
                        disabled: !self.can_manage,
                    })?,
                );
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
        table.empty = Some(TableEmpty::Content(Html::text(if self.can_manage {
            "No tax codes yet. Add one when bill lines carry tax."
        } else {
            "No tax codes yet."
        })));
        let mut heading = render(SectionHeading {
            id: "payables-tax-codes-heading",
            title: "Tax codes",
            class: "mt-6",
            description: "A bill line picks a tax code. Posted bills keep the rate they were recorded with.",
            actions: if self.can_manage {
                render(Button {
                    id: "payables-tax-add",
                    button_type: "button",
                    event: "show_tax",
                    body: Html::text("Add tax code"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
            ..SectionHeading::default()
        })?;
        heading.push(&render(Card {
            id: "payables-tax-codes",
            role: "region",
            labelledby: "payables-tax-codes-heading",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?);
        Ok(heading)
    }

    fn tax_dialog(&self, t: &TaxDraft) -> Result<Html> {
        let errors = |path: &str, label: &str| -> Vec<String> {
            t.errors
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, m)| error_sentence(label, m))
                .collect()
        };
        let mut summary = ErrorSummary::new("payables-tax-errors");
        let mut listed = false;
        for (path, message) in &t.errors {
            let (element, label) = match path.as_str() {
                "code" => ("payables-tax-code", "Code"),
                "name" => ("payables-tax-name", "Name"),
                "rate" => ("payables-tax-rate", "Rate"),
                "account_id" => ("payables-tax-account", "Account"),
                _ => continue,
            };
            summary.push(element, error_sentence(label, message));
            listed = true;
        }
        let mut body = if listed {
            render(summary)?
        } else {
            Html::EMPTY
        };
        if let Some(notice) = &t.notice {
            body.push(&render(bilibi_ui::Alert {
                id: "payables-tax-notice",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(notice),
            })?);
        }
        let field = |key: &str, label: &str, value: &str| {
            Field::in_form("tax", key)
                .id(format!("payables-tax-{key}"))
                .label(label)
                .value(value)
                .errors(errors(key, label))
        };
        body.push(&render(
            field("code", "Code", &t.code)
                .required()
                .attr("maxlength", "40")
                .hint("Short and fixed once saved, such as SST-6."),
        )?);
        body.push(&render(field("name", "Name", &t.name).required())?);
        body.push(&render(
            field("rate", "Rate (%)", &t.rate)
                .required()
                .attr("inputmode", "decimal"),
        )?);
        body.push(&render(
            Combobox::new("payables-tax-account", "tax[account]")
                .label("Account *")
                .aria_label("Account")
                .value(t.account.clone())
                .placeholder("Choose an account")
                .options(self.account_choices(&[]))
                .errors(errors("account_id", "Account"))
                .hint("The account the tax on a posted bill is debited to.")
                .attrs(bilibi_ui::Attrs {
                    required: true,
                    ..bilibi_ui::Attrs::default()
                }),
        )?);
        let mut actions = render(Button {
            id: "payables-tax-cancel",
            button_type: "button",
            event: "cancel_tax",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "payables-tax-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Adding…",
            body: Html::text("Add tax code"),
            ..Button::default()
        })?);
        render(Modal {
            id: "payables-tax-modal",
            title: "New tax code",
            description: &format!("For {}.", self.company),
            cancel_event: "cancel_tax",
            width: ModalWidth::Narrow,
            body: Form {
                id: "payables-tax-form",
                event: "tax_edit",
                change: true,
                class: "mt-4",
                body,
                actions: f::actions(actions)?,
            }
            .submit_event("save_tax_code_new")
            .map_err(|_| Error::Internal)?,
            ..Modal::default()
        })
    }

    /// A settings fact changed in place: one `settings.put` with that value
    /// alone (omitted values keep what is saved).
    async fn save_setting(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let field = v["field"].as_str().ok_or(Error::Invalid)?;
        let (key, label) = match field {
            "payable_account_id" => ("payable-account", "Payable account"),
            "quantity_tolerance" => ("quantity-tolerance", "Quantity tolerance"),
            "price_tolerance" => ("price-tolerance", "Price tolerance"),
            _ => return Err(Error::Invalid),
        };
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if !self.can_manage {
            self.statuses.put(
                key,
                CommitStatus::Error(
                    "You do not have permission to change the payables setup.".into(),
                ),
            );
            return Ok(());
        }
        let Some(company) = company(scope) else {
            return Ok(());
        };
        let mut input = json!({"company_id": company});
        let shown = match field {
            "payable_account_id" => {
                let Ok(id) = submitted.parse::<i64>() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be an account"]),
                    );
                    return Ok(());
                };
                input["payable_account_id"] = json!(id);
                self.account_label(id)
            }
            _ => match parse_tolerance(&submitted) {
                Ok(Some(tolerance)) => {
                    input[field] = tolerance;
                    submitted.clone()
                }
                Ok(None) => {
                    input[format!("clear_{field}")] = json!(true);
                    "Exact".into()
                }
                Err(message) => {
                    self.statuses
                        .put(key, CommitStatus::refusal(label, &submitted, &[message]));
                    return Ok(());
                }
            },
        };
        let written = self
            .pages
            .write(
                scope,
                credential,
                "save_settings",
                "finance.payables.settings.put",
                input,
            )
            .await?;
        self.statuses.put(key, commit_status(written, label, &shown));
        Ok(())
    }

    /// A tax code's name, rate or account changed in its row.
    async fn save_tax(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let id = v["id"]
            .as_str()
            .and_then(|i| i.parse::<i64>().ok())
            .ok_or(Error::Invalid)?;
        let field = v["field"].as_str().ok_or(Error::Invalid)?;
        let (part, label) = match field {
            "name" => ("name", "Name"),
            "rate" => ("rate", "Rate"),
            "account_id" => ("account", "Account"),
            _ => return Err(Error::Invalid),
        };
        let key = format!("tax-{id}-{part}");
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if !self.can_manage {
            self.statuses.put(
                key,
                CommitStatus::Error("You do not have permission to change tax codes.".into()),
            );
            return Ok(());
        }
        let Some(company) = company(scope) else {
            return Ok(());
        };
        let mut input = json!({"company_id": company, "tax_code_id": id});
        let shown = match field {
            "account_id" => {
                let Ok(account) = submitted.parse::<i64>() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be an account"]),
                    );
                    return Ok(());
                };
                input["account_id"] = json!(account);
                self.account_label(account)
            }
            _ => {
                if submitted.is_empty() {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["can't be blank"]),
                    );
                    return Ok(());
                }
                input[field] = json!(submitted.trim_end_matches('%').trim());
                submitted.clone()
            }
        };
        let written = self
            .pages
            .write(
                scope,
                credential,
                "save_tax_code",
                "finance.payables.tax_code.update",
                input,
            )
            .await?;
        self.statuses.put(key, commit_status(written, label, &shown));
        Ok(())
    }

    async fn add_tax(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        let Some(company) = company(scope) else {
            return Ok(());
        };
        let Some(t) = &mut self.tax else {
            return Ok(());
        };
        t.errors.clear();
        t.notice = None;
        let mut input = json!({"company_id": company, "active": true});
        for (key, text) in [("code", &t.code), ("name", &t.name), ("rate", &t.rate)] {
            if !text.trim().is_empty() {
                input[key] = json!(text.trim().trim_end_matches('%').trim());
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
                "save_tax_code_new",
                "finance.payables.tax_code.create",
                input,
            )
            .await?
        {
            Written::Done(_) => {
                self.flash = Some((StatusKind::Success, "Tax code added.".into()));
                self.tax = None;
            }
            Written::Forbidden => {
                t.notice = Some("You do not have permission to add tax codes.".into());
            }
            Written::Changed => {
                t.notice = Some(
                    "Nothing was saved because something changed. Check the form and try again."
                        .into(),
                );
            }
            Written::Fields(fields) => {
                let mut loose = Vec::new();
                for (path, messages) in fields {
                    let known = matches!(path.as_str(), "code" | "name" | "rate" | "account_id");
                    for m in messages {
                        if known {
                            t.errors.push((path.clone(), m));
                        } else {
                            loose.push(m);
                        }
                    }
                }
                if !loose.is_empty() || t.errors.is_empty() {
                    t.notice = Some(if loose.is_empty() {
                        "The tax code was not added. Check the form and try again.".into()
                    } else {
                        loose.join(" ")
                    });
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
            "history_grid" => {
                self.history.handle(&params::string_values(v));
                Ok(false)
            }
            "save_settings" => {
                self.save_setting(scope, credential, v).await?;
                Ok(true)
            }
            "save_tax_code" => {
                self.save_tax(scope, credential, v).await?;
                Ok(true)
            }
            "show_tax" => {
                if self.can_manage {
                    self.tax = Some(TaxDraft::default());
                } else {
                    self.flash = Some((
                        StatusKind::Error,
                        "You do not have permission to add tax codes.".into(),
                    ));
                }
                Ok(false)
            }
            "cancel_tax" => {
                self.tax = None;
                Ok(false)
            }
            "tax_edit" | "save_tax_code_new" => {
                let values = params::form_values(v, "tax");
                if let Some(t) = &mut self.tax {
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
                }
                if event == "save_tax_code_new" {
                    self.add_tax(scope, credential).await?;
                    return Ok(true);
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
                let written = self
                    .pages
                    .write(
                        scope,
                        credential,
                        "toggle_tax_code",
                        "finance.payables.tax_code.update",
                        json!({"company_id": company, "tax_code_id": id, "active": to}),
                    )
                    .await?;
                self.flash = match written {
                    Written::Done(_) => None,
                    Written::Forbidden => Some((
                        StatusKind::Error,
                        "You do not have permission to change tax codes.".into(),
                    )),
                    Written::Changed => Some((
                        StatusKind::Error,
                        "The tax code changed before this went through. Try again.".into(),
                    )),
                    Written::Fields(fields) => {
                        Some((StatusKind::Error, f::refused("The change", &fields)))
                    }
                };
                Ok(true)
            }
            _ => Err(Error::Invalid),
        }
    }
}

/// Who changed the settings, by name.
fn who(people: &People, c: &Value) -> String {
    people.who(
        c["changed_by_user_id"].as_i64(),
        c["changed_by_impersonator_id"].as_i64(),
    )
}

/// An in-place save's outcome where the person typed: saved, or the refusal
/// beside the field (never a fatal error, `docs/inline-editing.md`).
fn commit_status(written: Written, label: &str, shown: &str) -> CommitStatus {
    match written {
        Written::Done(_) => {
            let _ = shown;
            CommitStatus::Saved
        }
        Written::Forbidden => {
            CommitStatus::Error("You do not have permission to change the payables setup.".into())
        }
        Written::Changed => {
            CommitStatus::Error("It changed before this went through. Reload and try again.".into())
        }
        Written::Fields(fields) => {
            let messages: Vec<&str> = fields.values().flatten().map(String::as_str).collect();
            CommitStatus::refusal(label, shown, &messages)
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
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}

#[derive(Template)]
#[template(path = "setup.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    body: Html,
    dialog: Option<Html>,
}

fn fact_html(title: &'static str, value: Html) -> bilibi_ui::Fact<'static> {
    bilibi_ui::Fact {
        title,
        value,
        ..bilibi_ui::Fact::default()
    }
}
