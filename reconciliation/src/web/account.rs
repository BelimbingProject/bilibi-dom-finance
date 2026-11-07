//! Mirrors Bilimbi `Finance.Reconciliation.Web.ImportLive`: one bank
//! account's statements and reconciliations. The header leads with Back, the
//! account's matching and the next step: "Import statement" (a dialog,
//! [`super::import`], that maps the bank's columns on the first import) and,
//! once a statement is in, "Reconcile" (a dialog for the statement's end
//! date and balance).
use super::{
    clear_flash, company, event_type, flash_map, format::told, import, render, Pages, DENIED,
};
use crate::{
    BankAccount, BankAccountEdit, BankReconciliation, NewReconciliation, PageOptions,
    ReconciliationStatus, Statement, IMPORT_STATEMENTS, MANAGE_BANK_ACCOUNTS,
    MANAGE_RECONCILIATIONS, STATEMENT_SOURCE,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{Format, Precision};
use bilibi_finance_attachments::{AttachmentError, AttachmentQuery};
use bilibi_finance_ledger::{LedgerError, MinorUnits};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Cell,
    EmptyState, Field, FlashMap, FlexRow, FlexTable, Form, Header, Html, InputKind, Instant,
    ListColumns, ListSpec, ListState, Modal, StatusKind, TableEmpty, Toggle, UploadSpec, Uploads,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        omit_defaults: true,
        ..Default::default()
    }
    .shared()
    .expect("bank statements list")
});
const UPLOAD: &str = import::UPLOAD;
const MAX_BYTES: u64 = 5 * 1024 * 1024;
fn columns(values: &[(&str, &str)]) -> Vec<BuiltinColumn> {
    values
        .iter()
        .map(|(k, l)| {
            if ["lines", "opening", "closing", "balance"].contains(k) {
                BuiltinColumn::new(k, l).right()
            } else {
                BuiltinColumn::new(k, l)
            }
        })
        .collect()
}
pub(super) struct Account {
    pages: Pages,
    id: i64,
    list: ListState,
    statements: ListColumns,
    reconciliations: ListColumns,
    uploads: Uploads,
    draft: HashMap<String, String>,
    /// The open "Import statement" dialog.
    import: Option<import::Import>,
    /// Whether the "Reconcile" dialog is open.
    reconciling: bool,
    reconcile_errors: Vec<String>,
    flash: Option<(StatusKind, String)>,
    workspace: bool,
    opening: super::opening::Opening,
}
#[derive(Template)]
#[template(path = "account.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    account: Option<BankAccount>,
    refusal: Html,
    header: Html,
    notice: Html,
    opening: Html,
    statement_count: Option<usize>,
    reconciliation_count: Option<usize>,
    statements: Html,
    reconciliations: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(
    source = r#"{% if let Some(attachment) = attachment %}<a id="statement-{{ statement.id }}-file" href="/finance/attachments/{{ attachment }}/download" download="{{ statement.file_name }}" class="text-link hover:underline">{{ statement.file_name }}</a>{% else %}{{ statement.file_name }}{% endif %}"#,
    ext = "html"
)]
struct File<'a> {
    statement: &'a Statement,
    attachment: Option<i64>,
}
impl Account {
    pub(super) fn new(
        pages: &Pages,
        id: i64,
        query: &HashMap<String, String>,
        workspace: bool,
    ) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            id,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Invalid)?,
            statements: ListColumns::mount(
                "statements",
                columns(&[
                    ("number", "Number"),
                    ("file", "File"),
                    ("from", "From"),
                    ("to", "To"),
                    ("lines", "Lines"),
                    ("opening", "Opening"),
                    ("closing", "Closing"),
                    ("imported", "Imported"),
                    ("by", "By"),
                ]),
            )
            .map_err(|_| Error::Internal)?,
            reconciliations: ListColumns::mount(
                "reconciliations",
                columns(&[
                    ("number", "Number"),
                    ("end_date", "As of"),
                    ("balance", "Statement balance"),
                    ("status", "Status"),
                ]),
            )
            .map_err(|_| Error::Internal)?,
            uploads: Uploads::new([UploadSpec::new(UPLOAD, MAX_BYTES).accept([
                ".csv",
                ".txt",
                "text/csv",
                "text/plain",
            ])]),
            draft: HashMap::new(),
            // `?do=import` lands on the import dialog (an empty matching page
            // links here); the page drops it for a reader who may not import.
            import: (query.get("do").map(String::as_str) == Some("import"))
                .then(import::Import::new),
            reconciling: false,
            reconcile_errors: vec![],
            flash: None,
            workspace,
            opening: super::opening::Opening::new(id)?,
        })
    }
    async fn data(
        &self,
        scope: &Scope,
    ) -> Result<(Vec<Statement>, Vec<BankReconciliation>, HashMap<i64, i64>)> {
        let company = company(scope)?;
        let mut statements = vec![];
        loop {
            let after = statements.last().map(|s: &Statement| s.id);
            let page = match told(
                self.pages
                    .reconciliation
                    .list_statements(
                        scope,
                        company,
                        self.id,
                        &PageOptions {
                            limit: Some(500),
                            after,
                        },
                    )
                    .await,
            )? {
                Ok(page) => page,
                Err(_) => break,
            };
            let done = page.len() < 500;
            statements.extend(page);
            if done {
                break;
            }
        }
        let reconciliations = told(
            self.pages
                .reconciliation
                .list_reconciliations(scope, company, self.id)
                .await,
        )?
        .unwrap_or_default();
        let mut files = HashMap::new();
        for chunk in statements.chunks(100) {
            let mut after = None;
            loop {
                let page = match self
                    .pages
                    .attachments
                    .list_company_attachments(
                        scope,
                        company,
                        &AttachmentQuery {
                            source: Some(STATEMENT_SOURCE.into()),
                            document_ids: Some(chunk.iter().map(|s| s.id).collect()),
                            limit: Some(500),
                            after,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    Ok(page) => page,
                    Err(AttachmentError::Failed(e)) => return Err(e),
                    Err(_) => break,
                };
                let done = page.len() < 500;
                after = page.last().map(|a| a.attachment_number);
                for a in page {
                    files.insert(a.document_id, a.id);
                }
                if done {
                    break;
                }
            }
        }
        Ok((statements, reconciliations, files))
    }
    async fn body(&mut self, scope: &Scope) -> Result<String> {
        self.opening.clear_bridge();
        let account = told(
            self.pages
                .reconciliation
                .get_bank_account(scope, company(scope)?, self.id)
                .await,
        )?;
        let mut view = View {
            flash: flash_map(&self.flash),
            account: None,
            refusal: Html::EMPTY,
            header: Html::EMPTY,
            notice: Html::EMPTY,
            opening: Html::EMPTY,
            statement_count: None,
            reconciliation_count: None,
            statements: Html::EMPTY,
            reconciliations: Html::EMPTY,
            dialog: Html::EMPTY,
        };
        let account = match account {
            Ok(a) => a,
            Err(e) => {
                view.refusal = render(
                    EmptyState::missing("Bank account unavailable", &e).id("bank-account-refusal"),
                )?;
                return view.render().map_err(|_| Error::Internal);
            }
        };
        let (statements, reconciliations, files) = self.data(scope).await?;
        view.statement_count = Some(statements.len());
        view.reconciliation_count = Some(reconciliations.len());
        let units = match self.pages.ledger.get_book(scope, company(scope)?).await {
            Ok(book) => book.minor_units.get(),
            Err(LedgerError::Failed(e)) => return Err(e),
            Err(_) => 2,
        };
        let minor = MinorUnits::new(i64::from(units));
        let can_import = account.active && self.pages.allowed(scope, IMPORT_STATEMENTS).await?;
        let can_reconcile =
            account.active && self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await?;
        let open = reconciliations
            .iter()
            .find(|r| r.status == ReconciliationStatus::Open);
        // Reconciling needs a statement, and one reconciliation open at a time.
        let reconcile = can_reconcile && !statements.is_empty() && open.is_none();
        if !reconcile {
            self.reconciling = false;
        }
        if !can_import {
            self.import = None;
        }
        if self.draft.get("end_date").is_none_or(String::is_empty) {
            if let Some(s) = statements.last() {
                self.draft.insert("end_date".into(), s.end_date.to_string());
                self.draft.insert(
                    "statement_balance".into(),
                    s.closing_balance
                        .and_then(|m| minor.map(|minor| m.shown(minor)))
                        .unwrap_or_default(),
                );
            }
        }

        // Header: Back, this account's matching, then the next step.
        let mut actions = render(BackLink {
            href: "/finance/reconciliation",
            id: "bank-account-back",
            title: "Bank accounts",
            ..BackLink::default()
        })?;
        if !statements.is_empty() {
            actions.push(&render(Button {
                id: "bank-account-matching",
                href: &format!("/finance/reconciliation/accounts/{}/matching", account.id),
                body: Html::text("Match statement lines"),
                ..Button::default()
            })?);
        }
        if let Some(r) = open {
            actions.push(&render(Button {
                id: "bank-account-open-reconciliation",
                href: &format!("/finance/reconciliation/reconciliations/{}", r.id),
                body: Html::text(&format!("Reconciliation {}", r.reconciliation_number)),
                ..Button::default()
            })?);
        } else if reconcile {
            actions.push(&render(Button {
                id: "bank-account-reconcile",
                button_type: "button",
                event: "reconcile",
                body: Html::text("Reconcile"),
                ..Button::default()
            })?);
        }
        let import_button = |id: &'static str| -> Result<Html> {
            render(Button {
                id,
                button_type: "button",
                event: "open_import",
                variant: ButtonVariant::Primary,
                body: Html::text("Import statement"),
                ..Button::default()
            })
        };
        if can_import {
            actions.push(&import_button("bank-account-import")?);
        }
        view.header = render(Header {
            title: Html::text(&account.name),
            subtitle: "",
            title_actions: Html::EMPTY,
            actions,
        })?;
        if !account.active {
            let manage = self.pages.allowed(scope, MANAGE_BANK_ACCOUNTS).await?;
            let mut body =
                Html::text("This bank account is inactive: it imports and reconciles no more.");
            if manage {
                body.push(&render(Toggle {
                    on: false,
                    label: "Active",
                    id: "bank-account-active",
                    event: "activate",
                    ..Toggle::default()
                })?);
            }
            view.notice = render(Alert {
                id: "bank-account-inactive",
                kind: StatusKind::Info,
                body,
                ..Alert::default()
            })?;
        }

        let display = self.pages.display(scope).await?;
        let people = self
            .pages
            .people(
                scope,
                statements.iter().map(|s| Some(s.imported_by_user_id)),
            )
            .await?;
        let rows = statements
            .iter()
            .rev()
            .map(|s| {
                let from = s.start_date.to_string();
                let to = s.end_date.to_string();
                let time = display.render(s.imported_at, Format::DateTime, Precision::Minute);
                let money = |v: Option<bilibi_finance_ledger::Money>| {
                    v.map_or_else(|| "—".into(), |v| v.display(minor))
                };
                let mut row = FlexRow::new(s.id.to_string())
                    .cell("number", Cell::text(s.statement_number.to_string()))
                    .slot(
                        "file",
                        render(File {
                            statement: s,
                            attachment: files.get(&s.id).copied(),
                        })?,
                    )
                    .cell("from", Cell::date(&from, &from))
                    .slot("from", super::nowrap(&from)?)
                    .cell("to", Cell::date(&to, &to))
                    .slot("to", super::nowrap(&to)?)
                    .cell("lines", Cell::text(s.line_count.to_string()))
                    .cell("opening", Cell::text(money(s.opening_balance)))
                    .slot("opening", super::nowrap(&money(s.opening_balance))?)
                    .cell("closing", Cell::text(money(s.closing_balance)))
                    .slot("closing", super::nowrap(&money(s.closing_balance))?)
                    .cell(
                        "imported",
                        Cell::instant(Instant::new(
                            &time.iso,
                            &time.text,
                            &time.company,
                            &time.utc,
                            time.mode.as_str(),
                        )),
                    )
                    .cell(
                        "by",
                        Cell::text(
                            people.who(Some(s.imported_by_user_id), s.imported_by_impersonator_id),
                        ),
                    );
                row.dom_id = Some(format!("statement-{}", s.id));
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new("statements-table", self.statements.flex_columns(), rows);
        table.event = "statements_grid";
        table.framed = false;
        table.zoom = self.statements.zoom;
        table.suggestions = self.statements.flex_suggestions();
        table.add_query = &self.statements.add_query;
        let mut empty = EmptyState::missing(
            "No statements yet",
            "Import the CSV file the bank exports for this account.",
        );
        if can_import {
            empty = empty.action(import_button("statements-empty-import")?);
        }
        table.empty = Some(TableEmpty::State(empty));
        view.statements = render(table)?;

        let rows = reconciliations
            .iter()
            .rev()
            .map(|r| {
                let date = r.end_date.to_string();
                let key = r.id.to_string();
                let href = format!("/finance/reconciliation/reconciliations/{}", r.id);
                let link = render(bilibi_ui::RecordLink {
                    workspace: self.workspace,
                    kind: "finance/reconciliation/reconciliation",
                    record_id: &key,
                    href: &href,
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(&format!("Reconciliation {}", r.reconciliation_number)),
                })?;
                let mut row = FlexRow::new(key)
                    .slot("number", link)
                    .cell("end_date", Cell::date(&date, &date))
                    .slot("end_date", super::nowrap(&date)?)
                    .cell("balance", Cell::text(r.statement_balance.display(minor)))
                    .slot("status", status_badge(r.status)?);
                row.dom_id = Some(format!("reconciliation-{}", r.id));
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new(
            "reconciliations-table",
            self.reconciliations.flex_columns(),
            rows,
        );
        table.event = "reconciliations_grid";
        table.framed = false;
        table.zoom = self.reconciliations.zoom;
        table.suggestions = self.reconciliations.flex_suggestions();
        table.add_query = &self.reconciliations.add_query;
        let empty = if statements.is_empty() {
            EmptyState::missing(
                "No reconciliations yet",
                "Import a statement first; then reconcile the account to it.",
            )
        } else if reconcile {
            EmptyState::missing(
                "No reconciliations yet",
                "Reconcile the account to its latest statement's balance.",
            )
            .action(render(Button {
                id: "reconciliations-empty-reconcile",
                button_type: "button",
                event: "reconcile",
                variant: ButtonVariant::Primary,
                body: Html::text("Reconcile"),
                ..Button::default()
            })?)
        } else {
            EmptyState::missing(
                "No reconciliations yet",
                "Reconciliations of this account to its statements appear here.",
            )
        };
        table.empty = Some(TableEmpty::State(empty));
        view.reconciliations = render(table)?;

        view.opening = self.opening.body(&self.pages, scope).await?;
        if let Some(dialog) = &self.import {
            view.dialog = dialog
                .modal(
                    &self.pages,
                    scope,
                    &account,
                    &self.uploads,
                    minor.ok_or(Error::Internal)?,
                )
                .await?;
        } else if self.reconciling {
            view.dialog = self.reconcile_modal(&account)?;
        }
        view.account = Some(account);
        view.render().map_err(|_| Error::Internal)
    }

    fn reconcile_modal(&self, account: &BankAccount) -> Result<Html> {
        let field = |key, dom, label| {
            Field::in_form("reconciliation", key)
                .id(dom)
                .label(label)
                .value(self.draft.get(key).cloned().unwrap_or_default())
                .required()
        };
        let mut body = Html::EMPTY;
        if !self.reconcile_errors.is_empty() {
            body.push(&render(Alert {
                id: "reconciliation-open-alert",
                kind: StatusKind::Error,
                body: Html::text(&self.reconcile_errors.join(" ")),
                ..Alert::default()
            })?);
        }
        body.push(&render(
            field("end_date", "reconciliation-end-date", "Statement end date")
                .kind(InputKind::Date),
        )?);
        body.push(&render(
            field(
                "statement_balance",
                "reconciliation-statement-balance",
                "Balance per statement",
            )
            .hint("The closing balance the bank's statement shows on that date."),
        )?);
        let mut buttons = render(Button {
            id: "reconciliation-open-cancel",
            button_type: "button",
            event: "close_reconcile",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "reconciliation-open",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Opening…",
            body: Html::text("Reconcile"),
            ..Button::default()
        })?);
        let form = Form {
            id: "reconciliation-open-form",
            event: "reconciliation_change",
            change: true,
            class: "mt-4",
            body,
            actions: super::dialog_row(buttons)?,
        }
        .submit_event("open_reconciliation")
        .map_err(|_| Error::Internal)?;
        render(Modal {
            id: "reconciliation-open-modal",
            title: &format!("Reconcile {}", account.name),
            description: "Compare the books with the bank's statement as of its end date. Prefilled from the latest statement.",
            cancel_event: "close_reconcile",
            body: form,
            ..Modal::default()
        })
    }

    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &serde_json::Value,
    ) -> Result<Option<String>> {
        match event_type(value)? {
            event @ ("open_bank_opening"
            | "cancel_bank_opening"
            | "preview_opening"
            | "import_opening"
            | "opening_grid") => {
                if let Some(message) = self
                    .opening
                    .event(&self.pages, scope, credential, event, value)
                    .await?
                {
                    self.flash = Some((StatusKind::Success, message));
                }
            }
            "clear-flash" => clear_flash(&mut self.flash, value),
            "statements_grid" => {
                self.statements.handle(&params::string_values(value));
            }
            "reconciliations_grid" => {
                self.reconciliations.handle(&params::string_values(value));
            }
            "open_import" => {
                if self.pages.allowed(scope, IMPORT_STATEMENTS).await? {
                    self.reconciling = false;
                    self.import = Some(import::Import::new());
                } else {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                }
            }
            "close_import" => self.import = None,
            event @ ("import" | "import_check" | "import_choose_file" | "import_columns"
            | "import_mapped") => {
                let Some(dialog) = &mut self.import else {
                    return Ok(None);
                };
                let outcome = dialog
                    .event(&self.pages, scope, self.id, &mut self.uploads, event, value)
                    .await?;
                if let import::Outcome::Imported(imported) = outcome {
                    self.import = None;
                    let skipped_count = imported.skipped_count;
                    let s = imported.statement;
                    let skipped = match skipped_count {
                        0 => String::new(),
                        1 => " 1 row with no amount was skipped.".into(),
                        n => format!(" {n} rows with no amount were skipped."),
                    };
                    self.flash = Some((
                        StatusKind::Success,
                        format!(
                            "Statement {} imported: {} lines from {} to {}.{skipped} Match its lines next.",
                            s.statement_number, s.line_count, s.start_date, s.end_date
                        ),
                    ));
                    // The draft follows the newest statement.
                    self.draft.clear();
                }
            }
            "activate" => {
                if !self.pages.allowed(scope, MANAGE_BANK_ACCOUNTS).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(None);
                }
                let edit = BankAccountEdit {
                    active: Some(true),
                    ..Default::default()
                };
                let result = self
                    .pages
                    .reconciliation
                    .update_bank_account(scope, company(scope)?, self.id, &edit)
                    .await;
                self.flash = Some(match told(result)? {
                    Ok(a) => (StatusKind::Success, format!("{} is active.", a.name)),
                    Err(e) => (StatusKind::Error, e),
                });
            }
            "reconcile" => {
                if self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await? {
                    self.import = None;
                    self.reconciling = true;
                    self.reconcile_errors.clear();
                } else {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                }
            }
            "close_reconcile" => self.reconciling = false,
            "reconciliation_change" => self.draft = params::form_values(value, "reconciliation"),
            "open_reconciliation" => {
                self.draft = params::form_values(value, "reconciliation");
                if !self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await? {
                    self.reconcile_errors = vec![DENIED.into()];
                    return Ok(None);
                }
                match told(
                    self.pages
                        .reconciliation
                        .open_reconciliation(
                            scope,
                            company(scope)?,
                            self.id,
                            &NewReconciliation {
                                end_date: self.draft.get("end_date").and_then(|s| s.parse().ok()),
                                statement_balance: self
                                    .draft
                                    .get("statement_balance")
                                    .cloned()
                                    .unwrap_or_default(),
                            },
                        )
                        .await,
                )? {
                    Ok(r) => {
                        self.reconciling = false;
                        self.flash = Some((
                            StatusKind::Success,
                            format!("Reconciliation {} opened.", r.reconciliation_number),
                        ));
                        return Ok(Some(format!(
                            "/finance/reconciliation/reconciliations/{}",
                            r.id
                        )));
                    }
                    Err(e) => self.reconcile_errors = vec![e],
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}

fn status_badge(status: ReconciliationStatus) -> Result<Html> {
    render(Badge {
        kind: match status {
            ReconciliationStatus::Open => BadgeKind::Warning,
            ReconciliationStatus::Finalised => BadgeKind::Success,
            ReconciliationStatus::Abandoned => BadgeKind::Neutral,
        },
        dot: false,
        class: "",
        body: Html::text(super::reconciliation::status_label(status)),
    })
}

impl LiveState<Scope, Error> for Account {
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.uploads)
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let redirect = match request.input {
                LiveInput::Refresh => None,
                LiveInput::Params(v) => {
                    self.list = ListState::parse(v, SPEC.clone()).map_err(|_| Error::Invalid)?;
                    None
                }
                LiveInput::Event(v) => self.event(request.scope, request.credential, v).await?,
            };
            let flash = redirect.as_ref().and(self.flash.clone());
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                query: self.list.to_params(),
                redirect,
                flash,
                ..Default::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        let Some(bridge) = self.opening.bridge() else {
            return vec![];
        };
        vec![PanelSpec {
            id: "reconciliation-opening-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "reconciliation-opening-attachments".into()),
                ("source".into(), crate::OPENING_BRIDGE_SOURCE.into()),
                ("document".into(), bridge.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}
