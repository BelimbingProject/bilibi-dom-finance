//! Mirrors Bilimbi `Finance.Reconciliation.Web.BankAccountsLive`: the
//! company's bank accounts. "+ Bank account" asks only the name and the
//! ledger account; the bank's columns are mapped on the account's first
//! statement import. A row's own actions (edit, match its lines) are icon
//! buttons and its Active is a toggle; the row's Non-local actions menu holds
//! only what opens another module (its ledger account).
use super::{
    clear_flash, company, event_type, flash_map, format::told, index_dialogs as dialogs, record_id,
    render, Pages, DENIED,
};
use crate::{
    BankAccount, BankAccountEdit, FieldError, NewBankAccount, ReconciliationError,
    MANAGE_BANK_ACCOUNTS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{Account, AccountName, LedgerError};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, AddToItem, AddToMenu, Alert, Badge, BadgeKind, BuiltinColumn, Cell,
    CreateButton, EmptyState, FlashMap, FlexRow, FlexTable, Header, Html, IconButton, ListColumns,
    ListSpec, ListState, StatusKind, TableEmpty, Toggle,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const ACCOUNT: &str = "/finance/reconciliation/accounts";
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("bank accounts list")
});
fn columns() -> Vec<BuiltinColumn> {
    [
        ("name", "Name"),
        ("ledger", "Ledger account"),
        ("charges", "Bank charges to"),
        ("interest", "Interest to"),
        ("window", "Match window"),
        ("active", "Active"),
    ]
    .into_iter()
    .map(|(k, v)| BuiltinColumn::new(k, v))
    .collect()
}

/// The dialog the page holds open, with what the person typed so far.
enum Dialog {
    New {
        name: String,
        ledger: String,
        errors: Vec<FieldError>,
    },
    Edit {
        account: Box<BankAccount>,
        name: String,
        charges: String,
        interest: String,
        window: String,
        reason: String,
        errors: Vec<FieldError>,
    },
}

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    dialog: Option<Dialog>,
    flash: Option<(StatusKind, String)>,
    redirect: Option<String>,
    workspace: bool,
}

#[derive(Template)]
#[template(path = "bank_accounts.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    table: Option<Html>,
    forbidden: Html,
    notice: Html,
    dialog: Html,
}

impl Index {
    pub(super) fn new(
        pages: &Pages,
        query: &HashMap<String, String>,
        workspace: bool,
    ) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Invalid)?,
            columns: ListColumns::mount("bank-accounts", columns()).map_err(|_| Error::Internal)?,
            dialog: None,
            flash: None,
            redirect: None,
            workspace,
        })
    }

    fn account_cell(chart: Option<&[Account]>, id: Option<i64>) -> Result<(String, Html)> {
        let Some(id) = id else {
            return Ok(("—".into(), Html::text("—")));
        };
        match chart.and_then(|c| c.iter().find(|a| a.id == id)) {
            Some(a) => Ok((
                format!("{} · {}", a.name, a.code),
                render(AccountName {
                    name: &a.name,
                    code: &a.code,
                })?,
            )),
            None => Ok((
                "Account unavailable".into(),
                Html::text("Account unavailable"),
            )),
        }
    }

    fn row(&self, a: &BankAccount, chart: Option<&[Account]>, manage: bool) -> Result<FlexRow> {
        let key = a.id.to_string();
        let href = format!("{ACCOUNT}/{}", a.id);
        let link = render(bilibi_ui::RecordLink {
            workspace: self.workspace,
            kind: "finance/reconciliation/bank_account",
            record_id: &key,
            href: &href,
            class: "font-medium text-ink-strong hover:underline",
            body: Html::text(&a.name),
        })?;
        let (ledger_text, ledger) = Self::account_cell(chart, Some(a.ledger_account_id))?;
        let (charges_text, charges) = Self::account_cell(chart, a.charges_account_id)?;
        let (interest_text, interest) = Self::account_cell(chart, a.interest_account_id)?;
        let active = if manage {
            render(Toggle {
                on: a.active,
                label: &format!("{} is active", a.name),
                id: &format!("bank-account-{}-active", a.id),
                event: "toggle_active",
                name: "id",
                value: &key,
                disabled: false,
            })?
        } else {
            render(Badge {
                kind: if a.active {
                    BadgeKind::Success
                } else {
                    BadgeKind::Neutral
                },
                dot: false,
                class: "",
                body: Html::text(if a.active { "Active" } else { "Inactive" }),
            })?
        };
        let mut row = FlexRow::new(key.clone())
            .cell("name", Cell::text(&a.name))
            .slot("name", link)
            .cell("ledger", Cell::text(ledger_text))
            .slot("ledger", ledger)
            .cell("charges", Cell::text(charges_text))
            .slot("charges", charges)
            .cell("interest", Cell::text(interest_text))
            .slot("interest", interest)
            .cell(
                "window",
                Cell::text(format!(
                    "{} {}",
                    a.match_window_days,
                    if a.match_window_days == 1 {
                        "day"
                    } else {
                        "days"
                    }
                )),
            )
            .cell("active", Cell::text(if a.active { "Yes" } else { "No" }))
            .slot("active", active);
        row.dom_id = Some(format!("bank-account-{}", a.id));
        // Local actions on this bank account: plain icon buttons.
        let mut actions = Html::EMPTY;
        if a.active {
            let label = format!("Match statement lines of {}", a.name);
            actions.push(&render(IconButton {
                id: &format!("bank-account-{}-matching", a.id),
                href: &format!("{ACCOUNT}/{}/matching", a.id),
                ..IconButton::new("arrows-right-left", &label)
            })?);
        }
        if manage {
            let label = format!("Edit {}", a.name);
            actions.push(&render(IconButton {
                id: &format!("bank-account-{}-edit", a.id),
                event: "edit",
                name: "id",
                value: &key,
                ..IconButton::new("pencil-square", &label)
            })?);
        }
        // Non-local: the ledger account it posts to, in the Ledger module.
        actions.push(&render(AddToMenu::new(
            format!("bank-account-{}-actions", a.id),
            a.name.clone(),
            vec![AddToItem {
                label: "Ledger account".into(),
                href: format!("/finance/ledger/accounts/{}", a.ledger_account_id),
                disabled: None,
            }],
        ))?);
        row.actions = actions;
        Ok(row)
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let company = company(scope)?;
        let accounts = told(
            self.pages
                .reconciliation
                .list_bank_accounts(scope, company)
                .await,
        )?;
        let chart = match self.pages.ledger.list_accounts(scope, company).await {
            Ok(v) => Some(v),
            Err(LedgerError::Failed(e)) => return Err(e),
            Err(_) => None,
        };
        let manage = self.pages.allowed(scope, MANAGE_BANK_ACCOUNTS).await?;
        let can_add = accounts.is_ok() && manage && chart.is_some();
        let add = |id: &'static str| -> Result<Html> {
            render(CreateButton {
                noun: "Bank account",
                id,
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })
        };
        let header = render(Header {
            title: Html::text("Bank accounts"),
            subtitle: "",
            title_actions: Html::EMPTY,
            actions: if can_add {
                add("bank-account-new")?
            } else {
                Html::EMPTY
            },
        })?;
        let mut view = View {
            flash: flash_map(&self.flash),
            header,
            table: None,
            forbidden: Html::EMPTY,
            notice: Html::EMPTY,
            dialog: Html::EMPTY,
        };
        let accounts = match accounts {
            Ok(accounts) => accounts,
            Err(_) => {
                view.forbidden = render(
                    EmptyState::forbidden("view bank accounts").id("bank-accounts-forbidden"),
                )?;
                return view.render().map_err(|_| Error::Internal);
            }
        };
        let rows = accounts
            .iter()
            .map(|a| self.row(a, chart.as_deref(), manage))
            .collect::<Result<Vec<_>>>()?;
        let mut empty = EmptyState::missing(
            "No bank accounts yet",
            "Add a bank account to import its statements and match them with the books.",
        );
        if can_add {
            empty = empty.action(add("bank-accounts-empty-new")?);
        }
        let mut table = FlexTable::new("bank-accounts-table", self.columns.flex_columns(), rows);
        table.event = "accounts_grid";
        table.caption = "Bank accounts";
        table.actions = !accounts.is_empty();
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::State(empty));
        view.table = Some(render(table)?);
        if chart.is_none() && manage {
            view.notice = render(Alert {
                id: "bank-accounts-chart-forbidden",
                kind: StatusKind::Info,
                body: Html::text(
                    "Adding or changing a bank account needs access to view the chart of accounts.",
                ),
                ..Alert::default()
            })?;
        }
        if let (Some(dialog), Some(chart), true) = (&self.dialog, &chart, manage) {
            view.dialog = match dialog {
                Dialog::New {
                    name,
                    ledger,
                    errors,
                } => {
                    let taken: Vec<i64> = accounts.iter().map(|a| a.ledger_account_id).collect();
                    let company_name = self.pages.company_name(scope).await?;
                    dialogs::new_dialog(&company_name, chart, &taken, name, ledger, errors)?
                }
                Dialog::Edit {
                    account,
                    name,
                    charges,
                    interest,
                    window,
                    reason,
                    errors,
                } => dialogs::edit_dialog(
                    chart, account, name, charges, interest, window, reason, errors,
                )?,
            };
        }
        view.render().map_err(|_| Error::Internal)
    }

    /// Keeps what the person typed in the open dialog.
    fn keep(&mut self, value: &serde_json::Value) {
        let form = params::form_values(value, "bank_account");
        let get = |key: &str| form.get(key).cloned().unwrap_or_default();
        match &mut self.dialog {
            Some(Dialog::New { name, ledger, .. }) => {
                *name = get("name");
                *ledger = get("ledger_account_id");
            }
            Some(Dialog::Edit {
                name,
                charges,
                interest,
                window,
                reason,
                ..
            }) => {
                *name = get("name");
                *charges = get("charges_account_id");
                *interest = get("interest_account_id");
                *window = get("match_window_days");
                *reason = get("reason");
            }
            None => {}
        }
    }

    async fn create(&mut self, scope: &Scope) -> Result<()> {
        let Some(Dialog::New {
            name,
            ledger,
            errors,
        }) = &mut self.dialog
        else {
            return Ok(());
        };
        let Some(ledger_account_id) = dialogs::optional_id(ledger) else {
            *errors = vec![FieldError {
                field: "ledger_account_id",
                message: "can't be blank".into(),
            }];
            return Ok(());
        };
        let result = self
            .pages
            .reconciliation
            .create_bank_account(
                scope,
                company(scope)?,
                &NewBankAccount {
                    name: name.clone(),
                    ledger_account_id,
                    import_mapping: serde_json::Value::Null,
                    charges_account_id: None,
                    interest_account_id: None,
                    match_window_days: None,
                    active: None,
                    reason: None,
                },
            )
            .await;
        if let Err(ReconciliationError::Invalid(refused)) = &result {
            *errors = refused.clone();
            return Ok(());
        }
        match told(result)? {
            Ok(a) => {
                self.dialog = None;
                self.flash = Some((
                    StatusKind::Success,
                    format!("{} added. Import its first statement.", a.name),
                ));
                // The next step is the account's first import.
                self.redirect = Some(format!("{ACCOUNT}/{}", a.id));
            }
            Err(e) => {
                *errors = vec![FieldError {
                    field: "base",
                    message: e,
                }]
            }
        }
        Ok(())
    }

    async fn save(&mut self, scope: &Scope) -> Result<()> {
        let Some(Dialog::Edit {
            account,
            name,
            charges,
            interest,
            window,
            reason,
            errors,
        }) = &mut self.dialog
        else {
            return Ok(());
        };
        let result = self
            .pages
            .reconciliation
            .update_bank_account(
                scope,
                company(scope)?,
                account.id,
                &BankAccountEdit {
                    name: Some(name.clone()),
                    charges_account_id: Some(dialogs::optional_id(charges)),
                    interest_account_id: Some(dialogs::optional_id(interest)),
                    match_window_days: Some(window.trim().parse().unwrap_or(-1)),
                    reason: Some(reason.clone()),
                    ..Default::default()
                },
            )
            .await;
        if let Err(ReconciliationError::Invalid(refused)) = &result {
            *errors = refused.clone();
            return Ok(());
        }
        match told(result)? {
            Ok(a) => {
                self.dialog = None;
                self.flash = Some((StatusKind::Success, format!("{} saved.", a.name)));
            }
            Err(e) => {
                *errors = vec![FieldError {
                    field: "base",
                    message: e,
                }]
            }
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<()> {
        let event = event_type(value)?;
        if matches!(event, "new" | "edit" | "create" | "save" | "toggle_active")
            && !self.pages.allowed(scope, MANAGE_BANK_ACCOUNTS).await?
        {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(());
        }
        match event {
            "clear-flash" => clear_flash(&mut self.flash, value),
            "accounts_grid" => {
                self.columns.handle(&params::string_values(value));
            }
            "close_dialog" => self.dialog = None,
            "new_change" | "edit_change" => self.keep(value),
            "new" => {
                self.dialog = Some(Dialog::New {
                    name: String::new(),
                    ledger: String::new(),
                    errors: vec![],
                })
            }
            "create" => {
                self.keep(value);
                self.create(scope).await?;
            }
            "save" => {
                self.keep(value);
                self.save(scope).await?;
            }
            "edit" => {
                let Some(id) = record_id(value) else {
                    return Ok(());
                };
                match told(
                    self.pages
                        .reconciliation
                        .get_bank_account(scope, company(scope)?, id)
                        .await,
                )? {
                    Ok(a) => {
                        let id = |id: Option<i64>| id.map(|id| id.to_string()).unwrap_or_default();
                        self.dialog = Some(Dialog::Edit {
                            name: a.name.clone(),
                            charges: id(a.charges_account_id),
                            interest: id(a.interest_account_id),
                            window: a.match_window_days.to_string(),
                            reason: String::new(),
                            errors: vec![],
                            account: Box::new(a),
                        });
                    }
                    Err(e) => self.flash = Some((StatusKind::Error, e)),
                }
            }
            "toggle_active" => {
                let Some(id) = record_id(value) else {
                    return Ok(());
                };
                let company = company(scope)?;
                let r = &self.pages.reconciliation;
                let toggled = match told(r.get_bank_account(scope, company, id).await)? {
                    Ok(a) => {
                        let edit = BankAccountEdit {
                            active: Some(!a.active),
                            ..Default::default()
                        };
                        told(r.update_bank_account(scope, company, a.id, &edit).await)?
                    }
                    Err(e) => Err(e),
                };
                self.flash = Some(match toggled {
                    Ok(a) if a.active => (StatusKind::Success, format!("{} is active.", a.name)),
                    Ok(a) => (
                        StatusKind::Success,
                        format!("{} is inactive: it imports and reconciles no more.", a.name),
                    ),
                    Err(e) => (StatusKind::Error, e),
                });
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(v) => {
                    self.list = ListState::parse(v, SPEC.clone()).map_err(|_| Error::Invalid)?
                }
                LiveInput::Event(v) => self.event(request.scope, v).await?,
            }
            if let Some(redirect) = self.redirect.take() {
                return Ok(LiveBody {
                    redirect: Some(redirect),
                    flash: self.flash.take(),
                    ..Default::default()
                });
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                query: self.list.to_params(),
                ..Default::default()
            })
        })
    }
}
