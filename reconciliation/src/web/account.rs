//! Mirrors Bilimbi `Finance.Reconciliation.Web.ImportLive`.
use super::{clear_flash, company, event_type, flash_map, format::told, render, Pages, DENIED};
use crate::{
    BankAccount, BankReconciliation, NewReconciliation, PageOptions, ReconciliationStatus,
    Statement, IMPORT_STATEMENTS, MANAGE_RECONCILIATIONS, STATEMENT_SOURCE,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{Format, Precision};
use bilibi_finance_attachments::{AttachmentError, AttachmentQuery, NewFile};
use bilibi_finance_ledger::{LedgerError, MinorUnits};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, Badge, BadgeKind, BuiltinColumn, Cell, EmptyState, Field, FlashMap, FlexRow,
    FlexTable, Html, InputKind, Instant, ListColumns, ListSpec, ListState, StatusKind, TableEmpty,
    UploadSpec, Uploads,
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
const UPLOAD: &str = "statement";
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
    flash: Option<(StatusKind, String)>,
    workspace: bool,
    opening: super::opening::Opening,
}
#[derive(Template)]
#[template(path = "account.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    account: Option<BankAccount>,
    refusal: Html,
    mapping: String,
    matching: Html,
    opening: Html,
    statement_count: Option<usize>,
    reconciliation_count: Option<usize>,
    upload: Option<Html>,
    import_forbidden: Html,
    open_forbidden: Html,
    statements: Html,
    reconciliations: Html,
    end_date: Option<Field>,
    balance: Option<Field>,
}
#[derive(Template)]
#[template(
    source = r#"{% if let Some(attachment) = attachment %}<a id="statement-{{ statement.id }}-file" href="/finance/attachments/{{ attachment }}/download" download="{{ statement.file_name }}" title="SHA-256 {{ statement.file_sha256 }}">{{ statement.file_name }}</a>{% else %}<span title="SHA-256 {{ statement.file_sha256 }}; no source file is linked">{{ statement.file_name }}</span>{% endif %}"#,
    ext = "html"
)]
struct File<'a> {
    statement: &'a Statement,
    attachment: Option<i64>,
}
fn column_text(column: &Option<crate::Column>) -> String {
    match column {
        Some(crate::Column::Header(s)) => s.clone(),
        Some(crate::Column::Position(p)) => p.to_string(),
        None => String::new(),
    }
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
            title_actions: Html::EMPTY,
            flash: flash_map(&self.flash),
            account: None,
            refusal: Html::EMPTY,
            mapping: String::new(),
            matching: Html::EMPTY,
            opening: Html::EMPTY,
            statement_count: None,
            reconciliation_count: None,
            upload: None,
            import_forbidden: Html::EMPTY,
            open_forbidden: Html::EMPTY,
            statements: Html::EMPTY,
            reconciliations: Html::EMPTY,
            end_date: None,
            balance: None,
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
        view.title_actions = bilibi_ui::Html::EMPTY;
        let (statements, reconciliations, files) = self.data(scope).await?;
        view.statement_count = Some(statements.len());
        view.reconciliation_count = Some(reconciliations.len());
        view.matching = render(Matching {
            href: format!("/finance/reconciliation/accounts/{}/matching", account.id),
        })?;
        let units = match self.pages.ledger.get_book(scope, company(scope)?).await {
            Ok(book) => book.minor_units.get(),
            Err(LedgerError::Failed(e)) => return Err(e),
            Err(_) => 2,
        };
        if self.draft.get("end_date").is_none_or(String::is_empty)
            && !reconciliations
                .iter()
                .any(|r| r.status == ReconciliationStatus::Open)
        {
            if let Some(s) = statements.last() {
                self.draft.insert("end_date".into(), s.end_date.to_string());
                self.draft.insert(
                    "statement_balance".into(),
                    s.closing_balance
                        .and_then(|m| MinorUnits::new(i64::from(units)).map(|minor| m.shown(minor)))
                        .unwrap_or_default(),
                );
            }
        }
        let display = self.pages.display(scope).await?;
        let rows = statements
            .iter()
            .rev()
            .map(|s| {
                let from = s.start_date.to_string();
                let to = s.end_date.to_string();
                let time = display.render(s.imported_at, Format::DateTime, Precision::Minute);
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
                    .cell("to", Cell::date(&to, &to))
                    .cell("lines", Cell::text(s.line_count.to_string()))
                    .cell(
                        "opening",
                        Cell::text(s.opening_balance.map_or_else(
                            || "—".into(),
                            |v| v.display(MinorUnits::new(i64::from(units))),
                        )),
                    )
                    .cell(
                        "closing",
                        Cell::text(s.closing_balance.map_or_else(
                            || "—".into(),
                            |v| v.display(MinorUnits::new(i64::from(units))),
                        )),
                    )
                    .cell(
                        "imported",
                        Cell::instant(Instant::new(
                            &time.iso,
                            &time.text,
                            &time.company,
                            &time.utc,
                            time.mode.as_str(),
                        )),
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
        table.empty = Some(TableEmpty::State(EmptyState::missing(
            "No statements yet",
            "Import the bank's first CSV export above.",
        )));
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
                    .cell(
                        "balance",
                        Cell::text(
                            r.statement_balance
                                .display(MinorUnits::new(i64::from(units))),
                        ),
                    )
                    .slot(
                        "status",
                        render(Badge {
                            kind: match r.status.as_str() {
                                "open" => BadgeKind::Warning,
                                "finalised" => BadgeKind::Success,
                                _ => BadgeKind::Neutral,
                            },
                            dot: false,
                            class: "",
                            body: Html::text(r.status.as_str()),
                        })?,
                    );
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
        table.empty = Some(TableEmpty::State(EmptyState::missing(
            "No reconciliations yet",
            "Open the first one below once a statement is imported.",
        )));
        view.reconciliations = render(table)?;
        if self.pages.allowed(scope, IMPORT_STATEMENTS).await? {
            // Render the typed control independently, as the Design Library
            // does, so its entry/error fragment owns its complete context.
            view.upload = Some(render(self.uploads.input(UPLOAD, "statement-file"))?);
        } else {
            view.import_forbidden = render(
                EmptyState::forbidden("import statements").id("statement-import-forbidden"),
            )?;
        }
        if self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await? {
            let field = |key, dom, label| {
                Field::in_form("reconciliation", key)
                    .id(dom)
                    .label(label)
                    .value(self.draft.get(key).cloned().unwrap_or_default())
                    .required()
            };
            view.end_date = Some(
                field("end_date", "reconciliation-end-date", "Statement end date")
                    .kind(InputKind::Date),
            );
            view.balance = Some(field(
                "statement_balance",
                "reconciliation-statement-balance",
                "Balance per statement",
            ));
        } else {
            view.open_forbidden = render(
                EmptyState::forbidden("open reconciliations").id("reconciliation-open-forbidden"),
            )?;
        }
        let m = &account.import_mapping;
        view.mapping = format!(
            "Reads the description from {}, the amount from {}{}. A file or line already imported is refused.",
            column_text(&m.description),
            if m.amount.is_some() {
                column_text(&m.amount)
            } else {
                format!("{} and {}", column_text(&m.debit), column_text(&m.credit))
            },
            if m.balance.is_some() {
                format!(", and the running balance from {}", column_text(&m.balance))
            } else {
                String::new()
            }
        );
        view.opening = self.opening.body(&self.pages, scope).await?;
        view.account = Some(account);
        view.render().map_err(|_| Error::Internal)
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
            "reconciliation_change" => self.draft = params::form_values(value, "reconciliation"),
            "import" => {
                if !self.pages.allowed(scope, IMPORT_STATEMENTS).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(None);
                }
                if let Some(file) = self.uploads.consume(UPLOAD).pop() {
                    match told(
                        self.pages
                            .reconciliation
                            .import_statement(
                                scope,
                                company(scope)?,
                                self.id,
                                &NewFile {
                                    file_name: file.name,
                                    content: file.bytes,
                                    content_type: Some(file.content_type),
                                },
                            )
                            .await,
                    )? {
                        Ok(imported) => {
                            let s = imported.statement;
                            let skipped = if imported.skipped_count > 0 {
                                format!(
                                    " {} zero-amount rows were skipped.",
                                    imported.skipped_count
                                )
                            } else {
                                String::new()
                            };
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "Statement {} imported: {} lines from {} to {}.{skipped}",
                                    s.statement_number, s.line_count, s.start_date, s.end_date
                                ),
                            ));
                        }
                        Err(e) => self.flash = Some((StatusKind::Error, e)),
                    }
                } else {
                    self.flash = Some((
                        StatusKind::Error,
                        "Choose the bank's CSV file first.".into(),
                    ));
                }
            }
            "open_reconciliation" => {
                self.draft = params::form_values(value, "reconciliation");
                if !self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
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
                        self.flash = Some((
                            StatusKind::Success,
                            format!("Reconciliation {} opened.", r.reconciliation_number),
                        ));
                        return Ok(Some(format!(
                            "/finance/reconciliation/reconciliations/{}",
                            r.id
                        )));
                    }
                    Err(e) => self.flash = Some((StatusKind::Error, e)),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
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

#[derive(Template)]
#[template(
    source = r#"<a id="bank-account-matching" href="{{ href }}">Match statement lines</a>"#,
    ext = "html"
)]
struct Matching {
    href: String,
}
