//! What receivables needs before anyone can invoice: the accounts new
//! documents post to, the numbering series, and the tax codes, for the
//! company the person works in. Reads `setup.get`, `setup.changes`,
//! `series.list` and `tax_code.list`; every write calls `setup.configure`,
//! `series.create/update` or `tax_code.create/update`.
//!
//! The page reads first and edits in place (`DESIGN.md`, Record pages): each
//! default account is an inline choice whether or not it is set yet, a
//! series' or tax code's name, account and status are edited in their row,
//! and only a new series or tax code opens a dialog, because its code, kind,
//! prefix, digits and rate are fixed once saved. A setup that is not yet
//! configured needs both accounts in one `setup.configure`, so the first
//! choice is kept on the page (never persisted) until the second arrives.
use super::{
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written, DENIED, READ_LIMIT,
};
use crate::{Payment, Series, Setup, SetupChange, TaxCode, MANAGE_SETUP};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Attrs, Badge, BadgeKind, Button, ButtonVariant, Card, Cell, Choice, Combobox,
    CommitStates, CommitStatus, ConfirmDialog, Facts, Field, FlexRow, Fold, Form, Header, Html,
    InlineChoice, InlineEdit, InlineField, InlineLongText, InputKind, ListColumns, ModalWidth,
    PageVariant, SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// The chart of accounts page, where an account the setup needs is added.
const CHART: &str = "/finance/ledger/accounts";

/// The series kinds every company needs before its first invoice, credit note
/// and receipt, in the order the page names them.
const SERIES_KINDS: [(&str, &str); 3] = [
    ("invoice", "invoices"),
    ("credit_note", "credit notes"),
    ("payment", "receipts"),
];

/// A dialog for a new series or tax code; existing ones are edited in place.
enum Dialog {
    Series {
        values: HashMap<String, String>,
        errors: Errors,
    },
    Tax {
        values: HashMap<String, String>,
        errors: Errors,
    },
}

pub(super) struct Settings {
    pages: Pages,
    company: String,
    setup: Option<Setup>,
    /// The accounts chosen so far while the setup is not configured yet:
    /// `setup.configure` needs both, so one choice waits here for the other.
    pending: HashMap<&'static str, i64>,
    /// A different receivable account picked while the setup exists, waiting
    /// on the confirmation before it is written.
    switching: Option<i64>,
    changes: Vec<SetupChange>,
    series: Vec<Series>,
    taxes: Vec<TaxCode>,
    accounts: Vec<Account>,
    /// Whether the chart of accounts was read: an empty chart and an unread one
    /// are told apart.
    chart_read: bool,
    /// The accounts this company's receipts are recorded as paid into.
    receipts_into: HashSet<i64>,
    /// Whether that read stopped at the page's read limit, so the marking may
    /// be incomplete.
    receipts_cut: bool,
    can_manage: bool,
    statuses: CommitStates,
    flash: Flash,
    dialog: Option<Dialog>,
    series_cols: ListColumns,
    tax_cols: ListColumns,
    change_cols: ListColumns,
}

impl Settings {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            company: String::new(),
            setup: None,
            pending: HashMap::new(),
            switching: None,
            changes: vec![],
            series: vec![],
            taxes: vec![],
            accounts: vec![],
            chart_read: false,
            receipts_into: HashSet::new(),
            receipts_cut: false,
            can_manage: false,
            statuses: CommitStates::default(),
            flash: Flash::default(),
            dialog: None,
            series_cols: f::columns(
                "receivables-series-table",
                &[
                    ("code", "Code"),
                    ("name", "Name"),
                    ("kind", "Used for"),
                    ("prefix", "Prefix"),
                    ("width", "Digits"),
                    ("example", "Numbers like"),
                    ("status", "Status"),
                ],
                &["width"],
            )?,
            tax_cols: f::columns(
                "receivables-tax-table",
                &[
                    ("code", "Code"),
                    ("name", "Name"),
                    ("rate", "Rate"),
                    ("account", "Tax account"),
                    ("status", "Status"),
                ],
                &["rate"],
            )?,
            change_cols: f::columns(
                "receivables-setup-changes-table",
                &[
                    ("when", "When"),
                    ("field", "Account"),
                    ("from", "From"),
                    ("to", "To"),
                    ("by", "By"),
                ],
                &[],
            )?,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        if let Ok((payments, cut)) = self
            .pages
            .read_all::<Payment>(
                scope,
                "finance.receivables.payment.list",
                json!({}),
                ("after", "next_after"),
            )
            .await
        {
            self.receipts_into = payments.iter().map(|p| p.deposit_account_id).collect();
            self.receipts_cut = cut;
        }
        self.load(scope).await?;
        self.body(scope).await
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        if self.company.is_empty() {
            self.company = self.pages.company_name(scope).await;
        }
        self.setup = self
            .pages
            .read(scope, "finance.receivables.setup.get", json!({}))
            .await?
            .get("setup")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        if self.setup.is_some() {
            self.pending.clear();
        } else {
            self.switching = None;
        }
        self.changes = self
            .pages
            .read_all(
                scope,
                "finance.receivables.setup.changes",
                json!({}),
                ("after", "next_after"),
            )
            .await?
            .0;
        self.series = self
            .pages
            .read_all(
                scope,
                "finance.receivables.series.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        self.taxes = self
            .pages
            .read_all(
                scope,
                "finance.receivables.tax_code.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        let chart = self.pages.chart(scope).await;
        self.chart_read = chart.is_some();
        self.accounts = chart.unwrap_or_default();
        self.can_manage = self.pages.allowed(scope, MANAGE_SETUP).await?;
        self.series_cols.load(&self.series, |s| s.id.to_string());
        self.tax_cols.load(&self.taxes, |t| t.id.to_string());
        self.change_cols.load(&self.changes, |c| c.id.to_string());
        Ok(())
    }

    /// The active accounts of one type, name first, as a combobox's options.
    fn account_choices(&self, kind: &str) -> Vec<Choice> {
        self.accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == kind)
            .map(Account::choice)
            .collect()
    }

    /// The receivable account choices: the active asset accounts, without
    /// those receipts have been recorded as paid into (a bank or cash account),
    /// except the one already saved; `setup.configure` refuses them too, which
    /// covers receipts not read here.
    fn receivable_choices(&self) -> Vec<Choice> {
        self.accounts
            .iter()
            .filter(|a| {
                a.postable()
                    && a.account_type == "asset"
                    && (!self.receipts_into.contains(&a.id)
                        || self.setup.as_ref().map(|s| s.receivable_account_id) == Some(a.id))
            })
            .map(Account::choice)
            .collect()
    }

    /// The default account the page shows for a field: the configured one,
    /// or the one chosen on this page while the setup waits for its pair.
    fn default_of(&self, key: &'static str) -> Option<i64> {
        match &self.setup {
            Some(setup) if key == "receivable_account_id" => Some(setup.receivable_account_id),
            Some(setup) => Some(setup.income_account_id),
            None => self.pending.get(key).copied(),
        }
    }

    /// A default account: read first, a choice in place, set or not yet.
    fn default_fact(
        &self,
        key: &'static str,
        label: &'static str,
        kind: &str,
        hint: &str,
    ) -> Result<bilibi_ui::Fact<'static>> {
        let current = self.default_of(key);
        let mut field = InlineField::new(
            format!("receivables-field-{key}"),
            key,
            label,
            current.map(|id| id.to_string()).unwrap_or_default(),
        )
        .version(0)
        .status(self.statuses.get(key));
        field.id_value = "setup".into();
        field.save_event = "configure".into();
        if !self.can_manage {
            field = field.readonly();
        }
        let waiting = self.setup.is_none() && current.is_some();
        field = field.hint(if waiting {
            format!(
                "Chosen. Both accounts are saved together once the {} is chosen too.",
                if key == "receivable_account_id" {
                    "default income account"
                } else {
                    "receivable account"
                }
            )
        } else if key == "receivable_account_id" && self.receipts_cut {
            format!(
                "{hint} Only the first {READ_LIMIT} receipts were checked for accounts receipts are paid into; the ledger refuses such an account."
            )
        } else {
            hint.to_owned()
        });
        let display = match current {
            Some(id) => super::account_html(&self.accounts, id)?,
            None if self.can_manage => Html::text("Choose an account"),
            None => Html::text("Not set"),
        };
        let editor = Combobox::new(format!("receivables-{key}-input"), key)
            .aria_label(label)
            .placeholder("Choose an account")
            .wrapper_class("mb-0")
            .value(current.map(|id| id.to_string()).unwrap_or_default())
            .options(if key == "receivable_account_id" {
                self.receivable_choices()
            } else {
                self.account_choices(kind)
            });
        Ok(f::fact_html(
            label,
            render(InlineChoice {
                field,
                display,
                editor: render(editor)?,
            })?,
        ))
    }

    /// What stands between this company and its first invoice, as the notice
    /// above the default accounts: the setup itself, or an account type the
    /// chart does not offer yet. Nothing when both accounts are set.
    fn setup_notice(&self) -> Result<Html> {
        let mut lines: Vec<Html> = Vec::new();
        if self.setup.is_none() {
            lines.push(Html::text(if self.can_manage {
                "Receivables is not set up yet. Choose the receivable account and the default income account; they are saved together."
            } else {
                "Receivables is not set up yet. Someone who manages its setup must choose the default accounts."
            }));
        }
        if self.can_manage && !self.chart_read {
            lines.push(Html::text(
                "The chart of accounts could not be read, so no account can be chosen here. Viewing the ledger is needed to choose accounts.",
            ));
        } else if self.can_manage {
            let missing: Vec<&str> = [("asset", "asset"), ("income", "income")]
                .into_iter()
                .filter(|(kind, _)| {
                    !self
                        .accounts
                        .iter()
                        .any(|a| a.postable() && a.account_type == *kind)
                })
                .map(|(_, word)| word)
                .collect();
            if self.accounts.is_empty() {
                let mut line = Html::text(
                    "No accounts can be chosen yet: open the company's book and add its chart of accounts first, on ",
                );
                line.push(&f::link(CHART, "Chart of accounts")?);
                line.push(&Html::text("."));
                lines.push(line);
            } else if !missing.is_empty() {
                let mut line = Html::text(&format!(
                    "The chart has no active {} account to choose. Add one on ",
                    missing.join(" or ")
                ));
                line.push(&f::link(CHART, "Chart of accounts")?);
                line.push(&Html::text("."));
                lines.push(line);
            }
        }
        if lines.is_empty() {
            return Ok(Html::EMPTY);
        }
        let mut body = Html::EMPTY;
        for (n, line) in lines.into_iter().enumerate() {
            body.push(&render(Line {
                id: &format!("receivables-setup-notice-{n}"),
                body: line,
            })?);
        }
        render(Alert {
            kind: StatusKind::Info,
            id: "receivables-setup-notice",
            class: "mb-3",
            body,
        })
    }

    /// The series kinds with no active series yet, in words.
    fn missing_series(&self) -> Vec<&'static str> {
        SERIES_KINDS
            .iter()
            .filter(|(kind, _)| !self.series.iter().any(|s| s.active && s.kind == *kind))
            .map(|(_, word)| *word)
            .collect()
    }

    /// The inline field of one row's value: keyed per row, so each row keeps
    /// its own outcome, saved through the row's `save_event` with the record
    /// id, and read-only for someone who cannot manage the setup.
    fn row_field(&self, row: &Row) -> InlineField {
        let key = row.key();
        let mut inline = InlineField::new(
            format!("receivables-{key}"),
            row.field,
            row.label,
            row.value.clone(),
        )
        .version(0)
        .status(self.statuses.get(&key));
        inline.id_value = row.record.to_string();
        inline.save_event = row.event.into();
        if !self.can_manage {
            inline = inline.readonly();
        }
        inline
    }

    /// A row's text edited in place, or read alone by someone who cannot.
    fn row_text(&self, row: &Row) -> Result<Html> {
        render(InlineEdit {
            field: self.row_field(row),
        })
    }

    /// A row's account, edited in place with the combobox so the code stays
    /// muted and italic after the name.
    fn row_account(&self, row: &Row, display: Html, options: Vec<Choice>) -> Result<Html> {
        let editor = Combobox::new(format!("receivables-{}-input", row.key()), row.field)
            .aria_label(row.label)
            .placeholder("Choose an account")
            .wrapper_class("mb-0")
            .value(row.value.clone())
            .options(options);
        render(InlineChoice {
            field: self.row_field(row),
            display,
            editor: render(editor)?,
        })
    }

    /// A row's choice edited in place: Active or Inactive.
    fn row_choice(&self, row: &Row, display: Html, options: Vec<Choice>) -> Result<Html> {
        let editor = Field::named(row.field)
            .id(format!("receivables-{}-editor", row.key()))
            .label_class("sr-only")
            .label(row.label)
            .kind(InputKind::Select)
            .value(row.value.clone())
            .options(options);
        render(InlineChoice {
            field: self.row_field(row),
            display,
            editor: render(editor)?,
        })
    }

    fn status_choices() -> Vec<Choice> {
        vec![
            Choice::new("Active", "true"),
            Choice::new("Inactive", "false"),
        ]
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let mut body = render(Header {
            title: Html::text("Receivables settings"),
            subtitle: &self.company,
            title_actions: bilibi_ui::Html::EMPTY,
            ..Header::default()
        })?;

        // Default accounts: two facts, set or waiting to be, then their history.
        body.push(&render(SectionHeading {
            title: "Default accounts",
            id: "receivables-defaults-heading",
            title_actions: if self.setup.is_none() {
                render(Badge::status(BadgeKind::Warning, "Not set"))?
            } else {
                Html::EMPTY
            },
            description: "New invoices post to these accounts. An issued invoice keeps the receivable account it was issued with.",
            ..SectionHeading::default()
        })?);
        let mut defaults = self.setup_notice()?;
        defaults.push(&render(Facts {
            id: "receivables-defaults",
            items: vec![
                self.default_fact(
                    "receivable_account_id",
                    "Receivable account",
                    "asset",
                    "The control account for what customers owe. It is not the account receipts are paid into.",
                )?,
                self.default_fact(
                    "income_account_id",
                    "Default income account",
                    "income",
                    "Used for an invoice line that names no account.",
                )?,
            ],
        })?);
        defaults.push(&self.history(&display)?);
        body.push(&render(Card {
            id: "receivables-defaults-card",
            role: "region",
            labelledby: "receivables-defaults-heading",
            body: defaults,
            ..Card::default()
        })?);

        body.push(&render(SectionHeading {
            title: "Invoice layout",
            id: "receivables-layout-heading",
            description: "Used for printed invoices and credit notes for this company.",
            ..SectionHeading::default()
        })?);
        let mut items = Vec::new();
        let layout = self
            .setup
            .as_ref()
            .map(|s| s.invoice_layout.clone())
            .unwrap_or_default();
        for (key, label, value) in [
            ("company_name", "Company name", &layout.company_name),
            (
                "registration_number",
                "Registration number",
                &layout.registration_number,
            ),
            ("address", "Address", &layout.address),
            ("contact", "Contact details", &layout.contact),
            ("footer", "Footer text", &layout.footer),
        ] {
            let mut field =
                InlineField::new(format!("receivables-layout-{key}"), key, label, value)
                    .version(0)
                    .status(self.statuses.get(key))
                    .allow_empty();
            field.save_event = "save_layout".into();
            field.id_value = "layout".into();
            if !self.can_manage || self.setup.is_none() {
                field = field.readonly();
            }
            items.push(f::fact_html(
                label,
                render(InlineLongText { field, rows: 3 })?,
            ));
        }
        items.push(f::fact_html(
            "Invoice logo",
            f::link(
                &format!(
                    "/system/settings?company={}&q=Invoice+logo",
                    Pages::company(scope).unwrap_or(0)
                ),
                "Upload or remove the invoice logo (administrator)",
            )?,
        ));
        body.push(&render(Card {
            id: "receivables-layout-card",
            body: render(Facts {
                id: "receivables-layout",
                items,
            })?,
            ..Card::default()
        })?);

        // Numbering series.
        let mut rows = Vec::new();
        for s in &self.series {
            let field = |field: &'static str, label: &'static str, value: &str| Row {
                kind: "series",
                record: s.id,
                field,
                label,
                value: value.to_owned(),
                event: "save_series",
            };
            let mut row = FlexRow::new(s.id.to_string());
            row.dom_id = Some(format!("receivables-series-{}", s.id));
            row = row
                .cell("code", Cell::text(&s.code))
                .slot("name", self.row_text(&field("name", "Name", &s.name))?)
                .cell("kind", Cell::text(f::series_kind_label(&s.kind)))
                .cell("prefix", Cell::text(&s.prefix))
                .cell("width", Cell::text(s.number_width.to_string()))
                .slot(
                    "example",
                    f::tabular(&format!(
                        "{}{:0width$}",
                        s.prefix,
                        1,
                        width = usize::try_from(s.number_width).unwrap_or(1)
                    ))?,
                )
                .slot(
                    "status",
                    self.row_choice(
                        &field("active", "Status", &s.active.to_string()),
                        f::active_badge(s.active)?,
                        Self::status_choices(),
                    )?,
                );
            rows.push(row);
        }
        let missing = self.missing_series();
        let series_description = if missing.is_empty() {
            "Invoice and credit note numbers are assigned on issue, receipt numbers when a receipt is recorded. Code, use, prefix and digits are fixed once saved.".to_owned()
        } else {
            format!(
                "{} Invoice and credit note numbers are assigned on issue, receipt numbers when a receipt is recorded. Code, use, prefix and digits are fixed once saved.",
                if self.can_manage {
                    format!("Add an active series for {}.", join_words(&missing))
                } else {
                    format!(
                        "No active series for {} yet; someone who manages the setup must add one.",
                        join_words(&missing)
                    )
                }
            )
        };
        body.push(&render(SectionHeading {
            title: "Numbering series",
            id: "receivables-series-heading",
            class: "mt-6",
            title_actions: if missing.is_empty() {
                Html::EMPTY
            } else {
                render(Badge::status(BadgeKind::Warning, "Incomplete"))?
            },
            description: &series_description,
            actions: if self.can_manage {
                render(Button {
                    id: "receivables-series-add",
                    button_type: "button",
                    event: "show_series",
                    body: Html::text("Add series"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
            ..SectionHeading::default()
        })?);
        body.push(&render(Card {
            id: "receivables-series-card",
            role: "region",
            labelledby: "receivables-series-heading",
            inner_class: "p-0",
            body: f::table(
                &self.series_cols,
                "series_grid",
                "Numbering series",
                rows,
                "No numbering series yet. Invoices, credit notes and receipts each need one.",
            )?,
            ..Card::default()
        })?);

        // Tax codes.
        let mut rows = Vec::new();
        for t in &self.taxes {
            let field = |field: &'static str, label: &'static str, value: &str| Row {
                kind: "tax",
                record: t.id,
                field,
                label,
                value: value.to_owned(),
                event: "save_tax",
            };
            let mut row = FlexRow::new(t.id.to_string());
            row.dom_id = Some(format!("receivables-tax-code-{}", t.id));
            row = row
                .cell("code", Cell::text(&t.code))
                .slot("name", self.row_text(&field("name", "Name", &t.name))?)
                .slot("rate", f::tabular(&format!("{}%", f::decimal(t.rate)))?)
                .slot(
                    "account",
                    self.row_account(
                        &field("account_id", "Tax account", &t.account_id.to_string()),
                        super::account_html(&self.accounts, t.account_id)?,
                        self.account_choices("liability"),
                    )?,
                )
                .slot(
                    "status",
                    self.row_choice(
                        &field("active", "Status", &t.active.to_string()),
                        f::active_badge(t.active)?,
                        Self::status_choices(),
                    )?,
                );
            rows.push(row);
        }
        body.push(&render(SectionHeading {
            title: "Tax codes",
            id: "receivables-tax-heading",
            class: "mt-6",
            title_actions: render(Badge::status(BadgeKind::Neutral, "Optional"))?,
            description: "For invoice lines that carry tax. A code's rate is fixed once saved; issued documents keep the rate they recorded.",
            actions: if self.can_manage {
                render(Button {
                    id: "receivables-tax-add",
                    button_type: "button",
                    event: "show_tax",
                    body: Html::text("Add tax code"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
            ..SectionHeading::default()
        })?);
        body.push(&render(Card {
            id: "receivables-tax-card",
            role: "region",
            labelledby: "receivables-tax-heading",
            inner_class: "p-0",
            body: f::table(
                &self.tax_cols,
                "tax_grid",
                "Tax codes",
                rows,
                "No tax codes. Add one when invoice lines carry tax.",
            )?,
            ..Card::default()
        })?);

        let modal = self.switch_dialog()?.or(self.dialog_html()?);
        f::frame(
            "receivables-settings",
            PageVariant::Detail,
            &self.flash,
            body,
            modal,
        )
    }

    /// The changes to the default accounts, folded under one line until the
    /// reader asks (`DESIGN.md`: History is a quiet disclosure, not a button).
    fn history(&self, display: &bilibi_datetime::DisplayContext) -> Result<Html> {
        let mut rows = Vec::new();
        for c in self.changes.iter().rev() {
            let mut row = FlexRow::new(c.id.to_string());
            row.dom_id = Some(format!("receivables-setup-change-{}", c.id));
            rows.push(
                row.slot(
                    "when",
                    f::time(
                        display,
                        &format!("receivables-setup-change-at-{}", c.id),
                        &c.changed_at,
                    )?,
                )
                .cell(
                    "field",
                    Cell::text(if c.field == "income_account_id" {
                        "Default income account"
                    } else {
                        "Receivable account"
                    }),
                )
                .slot(
                    "from",
                    match c.old_value {
                        Some(id) => super::account_html(&self.accounts, id)?,
                        None => Html::text("—"),
                    },
                )
                .slot("to", super::account_html(&self.accounts, c.new_value)?)
                .cell(
                    "by",
                    Cell::text(f::actor(
                        Some(c.changed_by_user_id),
                        c.changed_by_impersonator_id,
                    )),
                ),
            );
        }
        let summary = match self.changes.len() {
            0 => "No changes yet".to_owned(),
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        };
        let table = f::table_in(
            &self.change_cols,
            "changes_grid",
            "Changes to the default accounts",
            rows,
            "The default accounts have not been changed.",
            true,
        )?;
        render(Fold {
            id: "receivables-defaults-history",
            title: "History",
            summary: &summary,
            open: false,
            body: table,
        })
    }

    /// The confirmation before the receivable account is changed: the new
    /// account applies to new invoices only.
    fn switch_dialog(&self) -> Result<Option<Html>> {
        let (Some(setup), Some(picked)) = (&self.setup, self.switching) else {
            return Ok(None);
        };
        let named = |id: i64| {
            self.accounts.iter().find(|a| a.id == id).map_or_else(
                || "an account that is not shown".into(),
                Account::choice_label,
            )
        };
        render(ConfirmDialog {
            id: "receivables-receivable-confirm",
            consequence: &format!(
                "Change the receivable account from {} to {}?",
                named(setup.receivable_account_id),
                named(picked)
            ),
            detail: "It applies to new invoices only. Invoices already issued keep the receivable account they were issued with.",
            confirm: "Change account",
            working: "Changing…",
            confirm_event: "confirm_receivable",
            cancel_event: "cancel_receivable",
        })
        .map(Some)
    }

    fn dialog_html(&self) -> Result<Option<Html>> {
        let Some(dialog) = &self.dialog else {
            return Ok(None);
        };
        let (title, kind, fixed, values, errors) = match dialog {
            Dialog::Series { values, errors } => (
                "Add numbering series",
                "series",
                "Code, use, prefix and digits cannot be changed after saving.",
                values,
                errors,
            ),
            Dialog::Tax { values, errors } => (
                "Add tax code",
                "tax",
                "Code and rate cannot be changed after saving.",
                values,
                errors,
            ),
        };
        let description = format!("{}. {fixed}", self.company);
        let value = |key: &str| values.get(key).cloned().unwrap_or_default();
        let specs = std::cell::RefCell::new(Vec::<(String, String)>::new());
        let mut body = Html::EMPTY;
        let field = |key: &str, label: &str| -> Field {
            specs.borrow_mut().push((key.to_owned(), label.to_owned()));
            Field::in_form(kind, key)
                .id(format!("receivables-{kind}-{key}"))
                .label(label)
                .value(value(key))
                .errors(errors.under(key, label))
        };
        if kind == "series" {
            body.push(&render(
                field("code", "Code")
                    .attr("maxlength", "50")
                    .required()
                    .hint("A short identifier, such as INV."),
            )?);
            body.push(&render(
                field("name", "Name").attr("maxlength", "200").required(),
            )?);
            body.push(&render(
                field("kind", "Used for")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("Invoices", "invoice"),
                        Choice::new("Credit notes", "credit_note"),
                        Choice::new("Receipts", "payment"),
                    ])
                    .prompt("Choose what it numbers")
                    .required(),
            )?);
            body.push(&render(
                field("prefix", "Prefix")
                    .attr("maxlength", "20")
                    .hint("Printed before the number, such as INV-."),
            )?);
            body.push(&render(
                field("number_width", "Digits")
                    .kind(InputKind::Number)
                    .attr("min", "1")
                    .attr("max", "12")
                    .hint("Numbers are padded to this many digits: prefix INV- with 5 digits gives INV-00001, then INV-00002. Empty means no padding: INV-1."),
            )?);
        } else {
            body.push(&render(
                field("code", "Code")
                    .attr("maxlength", "50")
                    .required()
                    .hint("A short identifier, such as SST."),
            )?);
            body.push(&render(
                field("name", "Name").attr("maxlength", "200").required(),
            )?);
            body.push(&render(
                field("rate", "Rate (percent)")
                    .attr("inputmode", "decimal")
                    .required()
                    .hint("6 means 6%."),
            )?);
            let account = field("account_id", "Tax account");
            body.push(&render(
                Combobox::new(account.id.clone(), account.name.clone())
                    .label("Tax account *")
                    .aria_label("Tax account")
                    .value(account.value.clone())
                    .placeholder("Search liability accounts")
                    .options(self.account_choices("liability"))
                    .errors(account.errors.clone())
                    .attrs(Attrs { required: true, ..Attrs::default() })
                    .hint("Records tax payable to the tax authority. Tax timing depends on the tax type and applicable basis. Choosing an account does not determine your tax treatment."),
            )?);
        }
        body.push(&render(
            Field::in_form(kind, "active")
                .id(format!("receivables-{kind}-active"))
                .label("Active")
                .kind(InputKind::Checkbox)
                .checked(values.get("active").is_none_or(|v| v == "true")),
        )?);
        let labels: Vec<_> = specs
            .borrow()
            .iter()
            .map(|(k, l)| (k.clone(), format!("receivables-{kind}-{k}"), l.clone()))
            .collect();
        let mut inner = render(errors.summary(&format!("receivables-{kind}-errors"), &labels))?;
        inner.push(&body);
        let mut actions = render(Button {
            id: &format!("receivables-{kind}-cancel"),
            button_type: "button",
            event: "cancel_dialog",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: &format!("receivables-{kind}-save"),
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: &format!("receivables-{kind}-form"),
            event: if kind == "series" {
                "save_series"
            } else {
                "save_tax"
            },
            change: false,
            class: "mt-4",
            body: inner,
            actions: f::actions(actions)?,
        })?;
        Ok(Some(f::modal(
            &format!("receivables-{kind}"),
            title,
            &description,
            "cancel_dialog",
            ModalWidth::Narrow,
            &self.flash,
            form,
        )?))
    }

    /// One default account chosen in place. With the setup configured it is
    /// saved alone; before that it waits on the page until both are chosen,
    /// then both go in one `setup.configure`, so nothing half-set is kept.
    async fn save_default(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let key: &'static str = match v["field"].as_str() {
            Some("receivable_account_id") => "receivable_account_id",
            Some("income_account_id") => "income_account_id",
            _ => return Err(Error::Invalid),
        };
        let label = if key == "receivable_account_id" {
            "Receivable account"
        } else {
            "Default income account"
        };
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        let Ok(id) = submitted.parse::<i64>() else {
            self.statuses.put(
                key,
                CommitStatus::refusal(label, &submitted, &["must be an account"]),
            );
            return Ok(());
        };
        if !self.can_manage {
            self.statuses.put(
                key,
                CommitStatus::Error("You do not have permission to change the setup.".into()),
            );
            return Ok(());
        }
        let chosen = super::account_label(&self.accounts, id);
        if let Some(setup) = &self.setup {
            if key == "receivable_account_id" {
                self.switching = (id != setup.receivable_account_id).then_some(id);
                self.statuses.put(key, CommitStatus::None);
                return Ok(());
            }
            let written = self
                .pages
                .write(
                    scope,
                    credential,
                    "configure",
                    "finance.receivables.setup.configure",
                    json!({key: id}),
                )
                .await?;
            self.statuses
                .put(key, f::commit_status(&written, key, label, &chosen));
            return Ok(());
        }
        self.pending.insert(key, id);
        self.statuses.put(key, CommitStatus::None);
        let (Some(receivable), Some(income)) = (
            self.pending.get("receivable_account_id").copied(),
            self.pending.get("income_account_id").copied(),
        ) else {
            return Ok(());
        };
        let written = self
            .pages
            .write(
                scope,
                credential,
                "configure",
                "finance.receivables.setup.configure",
                json!({"receivable_account_id": receivable, "income_account_id": income}),
            )
            .await?;
        for (field, name, account) in [
            ("receivable_account_id", "Receivable account", receivable),
            ("income_account_id", "Default income account", income),
        ] {
            let status = f::commit_status(
                &written,
                field,
                name,
                &super::account_label(&self.accounts, account),
            );
            // A refusal of the other field alone leaves this one chosen, not wrong.
            let status = match (&written, &status) {
                (Written::Invalid(fields), CommitStatus::Error(_))
                    if !fields.contains_key(field) && !fields.contains_key("base") =>
                {
                    CommitStatus::None
                }
                _ => status,
            };
            self.statuses.put(field, status);
        }
        if matches!(written, Written::Done(_)) {
            self.pending.clear();
            self.flash
                .set(StatusKind::Success, "Default accounts saved.");
        }
        Ok(())
    }

    /// The receivable account change the person confirmed.
    async fn confirm_receivable(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        let key = "receivable_account_id";
        let Some(id) = self.switching.take() else {
            return Ok(());
        };
        if self.setup.is_none() {
            return Ok(());
        }
        if !self.can_manage {
            self.statuses.put(
                key,
                CommitStatus::Error("You do not have permission to change the setup.".into()),
            );
            return Ok(());
        }
        let written = self
            .pages
            .write(
                scope,
                credential,
                "confirm_receivable",
                "finance.receivables.setup.configure",
                json!({key: id}),
            )
            .await?;
        self.statuses.put(
            key,
            f::commit_status(
                &written,
                key,
                "Receivable account",
                &super::account_label(&self.accounts, id),
            ),
        );
        Ok(())
    }

    /// One field of a series or tax code row edited in place: its name, its
    /// status, or (a tax code) its account; the code, kind, prefix, digits
    /// and rate have no editor. Fields the contract does not take are refused
    /// before any write.
    async fn save_row(
        &mut self,
        scope: &Scope,
        credential: &str,
        series: bool,
        v: &Value,
    ) -> Result<()> {
        let id = v["id"]
            .as_str()
            .and_then(|i| i.parse::<i64>().ok())
            .ok_or(Error::Invalid)?;
        let field = v["field"].as_str().ok_or(Error::Invalid)?;
        let (label, allowed) = match (series, field) {
            (_, "name") => ("Name", true),
            (_, "active") => ("Status", true),
            (false, "account_id") => ("Tax account", true),
            _ => ("", false),
        };
        if !allowed {
            return Err(Error::Invalid);
        }
        let key = format!("{}-{id}-{field}", if series { "series" } else { "tax" });
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if !self.can_manage {
            self.statuses.put(key, CommitStatus::Error(DENIED.into()));
            return Ok(());
        }
        let (event, contract, id_field) = if series {
            (
                "save_series",
                "finance.receivables.series.update",
                "series_id",
            )
        } else {
            (
                "save_tax",
                "finance.receivables.tax_code.update",
                "tax_code_id",
            )
        };
        let mut input = json!({id_field: id});
        let shown = match field {
            "active" => {
                let active = match submitted.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        self.statuses.put(
                            key,
                            CommitStatus::refusal(
                                label,
                                &submitted,
                                &["must be Active or Inactive"],
                            ),
                        );
                        return Ok(());
                    }
                };
                input["active"] = json!(active);
                if active { "Active" } else { "Inactive" }.to_owned()
            }
            "account_id" => {
                let Ok(account) = submitted.parse::<i64>() else {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["must be an account"]),
                    );
                    return Ok(());
                };
                input["account_id"] = json!(account);
                super::account_label(&self.accounts, account)
            }
            _ => {
                if submitted.is_empty() {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(label, &submitted, &["can't be blank"]),
                    );
                    return Ok(());
                }
                input["name"] = json!(submitted);
                submitted.clone()
            }
        };
        let written = self
            .pages
            .write(scope, credential, event, contract, input)
            .await?;
        self.statuses
            .put(key, f::commit_status(&written, field, label, &shown));
        Ok(())
    }

    /// The dialog's new series or tax code.
    async fn save_new(
        &mut self,
        scope: &Scope,
        credential: &str,
        series: bool,
        v: &Value,
    ) -> Result<()> {
        let form = params::form_values(v, if series { "series" } else { "tax" });
        let (keys, contract): (&[&str], _) = match &self.dialog {
            Some(Dialog::Series { .. }) if series => (
                &["code", "name", "kind", "prefix"][..],
                "finance.receivables.series.create",
            ),
            Some(Dialog::Tax { .. }) if !series => (
                &["code", "name", "rate"][..],
                "finance.receivables.tax_code.create",
            ),
            _ => return Err(Error::Invalid),
        };
        let mut input = json!({});
        let mut local = Errors::default();
        for key in keys {
            // A prefix is the one text that keeps its spaces.
            let text = if *key == "prefix" {
                form.get(*key).map_or("", String::as_str)
            } else {
                f::posted(&form, key)
            };
            f::put(&mut input, key, text);
        }
        input["active"] = json!(form.get("active").is_some_and(|a| a == "true"));
        if series {
            match f::posted(&form, "number_width") {
                "" => {}
                w => match w.parse::<i64>() {
                    Ok(n) => input["number_width"] = json!(n),
                    Err(_) => local
                        .0
                        .entry("number_width".into())
                        .or_default()
                        .push("must be a whole number".into()),
                },
            }
        } else if let Ok(account) = f::posted(&form, "account_id").parse::<i64>() {
            input["account_id"] = json!(account);
        } else {
            local
                .0
                .entry("account_id".into())
                .or_default()
                .push("can't be blank".into());
        }
        let restore = |this: &mut Self, errors: Errors, form: HashMap<String, String>| {
            this.dialog = Some(if series {
                Dialog::Series {
                    values: form,
                    errors,
                }
            } else {
                Dialog::Tax {
                    values: form,
                    errors,
                }
            });
        };
        if !local.is_empty() {
            restore(self, local, form);
            return Ok(());
        }
        let event = if series { "save_series" } else { "save_tax" };
        match self
            .pages
            .write(scope, credential, event, contract, input)
            .await?
        {
            Written::Done(_) => {
                self.dialog = None;
                self.flash.set(
                    StatusKind::Success,
                    if series {
                        "Numbering series saved."
                    } else {
                        "Tax code saved."
                    },
                );
            }
            Written::Invalid(fields) => restore(self, Errors(fields), form),
            Written::Refused(message) => {
                self.dialog = None;
                self.flash.set(
                    StatusKind::Error,
                    super::worded(
                        message,
                        "You do not have permission to change the receivables setup.",
                    ),
                );
            }
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => self.flash.dismiss(v),
            "series_grid" => {
                self.series_cols.handle(&params::string_values(v));
            }
            "tax_grid" => {
                self.tax_cols.handle(&params::string_values(v));
            }
            "changes_grid" => {
                self.change_cols.handle(&params::string_values(v));
            }
            "save_layout" => {
                let key = v["field"].as_str().ok_or(Error::Invalid)?;
                if ![
                    "company_name",
                    "registration_number",
                    "address",
                    "contact",
                    "footer",
                ]
                .contains(&key)
                {
                    return Err(Error::Invalid);
                }
                if !self.can_manage {
                    self.statuses
                        .put(key.to_owned(), CommitStatus::Error(DENIED.into()));
                    return Ok(());
                }
                let value = v["value"].as_str().ok_or(Error::Invalid)?;
                let layout = json!({key: value});
                let written = self
                    .pages
                    .write(
                        scope,
                        credential,
                        "save_layout",
                        "finance.receivables.setup.configure",
                        json!({"invoice_layout":layout}),
                    )
                    .await?;
                self.statuses.put(
                    key.to_owned(),
                    f::commit_status(
                        &written,
                        &format!("invoice_layout.{key}"),
                        "Invoice layout",
                        value,
                    ),
                );
            }
            "configure" => self.save_default(scope, credential, v).await?,
            "confirm_receivable" => self.confirm_receivable(scope, credential).await?,
            "cancel_receivable" => self.switching = None,
            "show_series" | "show_tax" => {
                if !self.can_manage {
                    self.flash.set(StatusKind::Error, DENIED);
                } else {
                    let mut values = HashMap::new();
                    values.insert("active".into(), "true".to_owned());
                    self.dialog = Some(if v["type"] == "show_series" {
                        Dialog::Series {
                            values,
                            errors: Errors::default(),
                        }
                    } else {
                        Dialog::Tax {
                            values,
                            errors: Errors::default(),
                        }
                    });
                }
            }
            "cancel_dialog" => self.dialog = None,
            "save_series" | "save_tax" => {
                let series = v["type"] == "save_series";
                if v["field"].is_string() {
                    self.save_row(scope, credential, series, v).await?
                } else {
                    self.save_new(scope, credential, series, v).await?
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

/// One value of a series or tax code row that is edited in place.
struct Row {
    /// `series` or `tax`, the first part of the row's outcome key.
    kind: &'static str,
    record: i64,
    field: &'static str,
    label: &'static str,
    value: String,
    event: &'static str,
}

impl Row {
    /// The key its outcome is kept under (`series-4-name`), the same one
    /// `save_row` writes.
    fn key(&self) -> String {
        format!("{}-{}-{}", self.kind, self.record, self.field)
    }
}

/// Words joined for a sentence: "invoices", "invoices and receipts",
/// "invoices, credit notes and receipts".
fn join_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [all @ .., last] => format!("{} and {last}", all.join(", ")),
    }
}

#[derive(askama::Template)]
#[template(source = r#"<p id="{{ id }}">{{ body|safe }}</p>"#, ext = "html")]
struct Line<'a> {
    id: &'a str,
    body: Html,
}

impl LiveState<Scope, Error> for Settings {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(v) = r.input {
                self.event(r.scope, r.credential, v).await?;
            }
            self.load(r.scope).await?;
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}
