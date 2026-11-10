//! The bank account page's "Import statement" dialog. Its first step takes
//! the bank's CSV file. An account whose columns are already mapped imports
//! it at once; one whose columns are not mapped yet (its first import), or
//! whose file no longer reads with the saved columns, goes on to the columns
//! step: the file's first rows, a guess of which column is the date,
//! description and amount (from the header row), and the first lines as they
//! would import, all read again as the person changes a choice. Importing
//! from that step keeps the columns with the bank account
//! (`Reconciliation::import_statement_mapped`), so the next file imports in
//! one step.
use super::{company, format, render, Pages};
use crate::{
    ImportedStatement, ReconciliationError, StatementPreview, DATE_FORMATS, IMPORT_STATEMENTS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_attachments::NewFile;
use bilibi_finance_ledger::MinorUnits;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    params, Alert, BuiltinColumn, Button, ButtonVariant, Cell, Choice, Field, FlexRow, FlexTable,
    Form, Html, InputKind, ListColumns, Modal, ModalWidth, StatusKind, Toggle, Uploads,
};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(super) const UPLOAD: &str = "statement";
const FORM: &str = "columns";
/// The keys of a mapping that name a column.
const COLUMNS: [&str; 7] = [
    "date",
    "description",
    "reference",
    "amount",
    "debit",
    "credit",
    "balance",
];

/// A file the dialog holds between its steps.
struct Held {
    name: String,
    content_type: String,
    bytes: Vec<u8>,
}

/// The open dialog.
pub(super) struct Import {
    /// Whether the person asked to check the columns before importing.
    check: bool,
    file: Option<Held>,
    /// The columns step's choices, as the form posts them.
    draft: HashMap<String, String>,
    refusal: Option<String>,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}<form id="statement-import-form" data-event="import" class="mt-4 space-y-4">{{ alert|safe }}{{ upload|safe }}{{ check|safe }}<div class="flex w-full flex-wrap justify-end gap-2">{% call ui::button(id="statement-import-cancel", button_type="button", event="close_import") %}Cancel{% endcall %}{% call ui::button(id="statement-import-submit", button_type="submit", variant=bilibi_ui::ButtonVariant::Primary, working="Reading…") %}{{ verb }}{% endcall %}</div></form>"#,
    ext = "html"
)]
struct FileStep<'a> {
    alert: Html,
    upload: Html,
    check: Html,
    verb: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<div class="mt-4 mb-4 space-y-4">{{ alert|safe }}<section aria-labelledby="statement-preview-rows-heading"><h3 id="statement-preview-rows-heading" class="mb-2 text-sm font-medium text-ink">{{ rows_title }}</h3>{{ rows|safe }}</section>{{ fields|safe }}<section aria-labelledby="statement-preview-lines-heading"><h3 id="statement-preview-lines-heading" class="mb-2 text-sm font-medium text-ink">{{ lines_title }}</h3>{{ lines|safe }}</section></div>"#,
    ext = "html"
)]
struct ColumnsBody<'a> {
    alert: Html,
    rows_title: &'a str,
    rows: Html,
    fields: Html,
    lines_title: String,
    lines: Html,
}

/// What the dialog's event did.
pub(super) enum Outcome {
    /// Nothing to tell the page.
    Stay,
    /// The statement was imported; the dialog is closed.
    Imported(Box<ImportedStatement>),
}

impl Import {
    pub(super) fn new() -> Self {
        Self {
            check: false,
            file: None,
            draft: HashMap::new(),
            refusal: None,
        }
    }

    /// Handles one of the dialog's events.
    pub(super) async fn event(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        account: i64,
        uploads: &mut Uploads,
        event: &str,
        value: &Value,
    ) -> Result<Outcome> {
        if !pages.allowed(scope, IMPORT_STATEMENTS).await? {
            self.refusal = Some(super::DENIED.into());
            return Ok(Outcome::Stay);
        }
        let company = company(scope)?;
        let r = &pages.reconciliation;
        match event {
            "import_check" => self.check = !self.check,
            "import_choose_file" => {
                self.file = None;
                self.draft.clear();
                self.refusal = None;
            }
            "import_columns" => {
                let posted = params::form_values(value, FORM);
                // A changed separator or header row changes the columns:
                // drop the choices the file no longer has.
                let reshaped = ["delimiter", "header_rows"]
                    .iter()
                    .any(|key| posted.get(*key) != self.draft.get(*key));
                self.draft = posted;
                if reshaped {
                    if let Some(file) = &self.file {
                        let preview = told(
                            r.preview_statement(
                                scope,
                                company,
                                account,
                                &file.bytes,
                                Some(&mapping(&self.draft)),
                            )
                            .await,
                        )?;
                        if let Ok(preview) = preview {
                            let offered = options(&preview);
                            for key in COLUMNS {
                                if self.draft.get(key).is_some_and(|v| {
                                    !v.is_empty() && !offered.iter().any(|c| &c.value == v)
                                }) {
                                    self.draft.insert(key.into(), String::new());
                                }
                            }
                        }
                    }
                }
            }
            "import" => {
                let Some(file) = uploads.consume(UPLOAD).pop() else {
                    self.refusal = Some("Choose the bank's CSV file first.".into());
                    return Ok(Outcome::Stay);
                };
                let held = Held {
                    name: file.name,
                    content_type: file.content_type,
                    bytes: file.bytes,
                };
                self.refusal = None;
                let mapped = r.get_bank_account(scope, company, account).await;
                let mapped = told(mapped)?.map(|a| a.import_mapping.is_some());
                if mapped == Ok(true) && !self.check {
                    let result = r
                        .import_statement(scope, company, account, &new_file(&held))
                        .await;
                    match result {
                        Ok(done) => return Ok(Outcome::Imported(Box::new(done))),
                        // The bank changed its export: map the columns again,
                        // from a fresh guess rather than the saved columns.
                        Err(ReconciliationError::MissingHeaders(_)) => {
                            self.refusal = Some(
                                "This file has different columns from last time. Check them below."
                                    .into(),
                            );
                            return self.columns(pages, scope, account, held, true).await;
                        }
                        Err(
                            e @ (ReconciliationError::InvalidLines(_)
                            | ReconciliationError::RunningBalanceMismatch(_)),
                        ) => {
                            self.refusal =
                                Some(format!("{} Check the columns below.", format::refusal(&e)));
                        }
                        Err(e) => {
                            self.refusal = told::<()>(Err(e))?.err();
                            return Ok(Outcome::Stay);
                        }
                    }
                }
                return self.columns(pages, scope, account, held, false).await;
            }
            "import_mapped" => {
                // The draft is the server's: every change of the form sent it.
                let Some(file) = &self.file else {
                    return Ok(Outcome::Stay);
                };
                let result = r
                    .import_statement_mapped(
                        scope,
                        company,
                        account,
                        &new_file(file),
                        &mapping(&self.draft),
                    )
                    .await;
                match result {
                    Ok(done) => return Ok(Outcome::Imported(Box::new(done))),
                    // Replacing saved columns is a change to the bank account.
                    Err(ReconciliationError::Unauthorized)
                        if !pages.allowed(scope, crate::MANAGE_BANK_ACCOUNTS).await? =>
                    {
                        self.refusal = Some("This account already has saved columns, and only someone who manages bank accounts can change them. Ask them to import this file, or choose a file with the saved columns.".into());
                    }
                    Err(e) => self.refusal = told::<()>(Err(e))?.err(),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(Outcome::Stay)
    }

    /// Goes on to the columns step with `held`: its choices from the account's
    /// saved columns, its own guess for an account with none, or a fresh
    /// guess when `fresh` (the saved ones no longer fit the file).
    async fn columns(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        account: i64,
        held: Held,
        fresh: bool,
    ) -> Result<Outcome> {
        let guess = fresh.then(|| crate::Reconciliation::guess_mapping(&held.bytes));
        let preview = told(
            pages
                .reconciliation
                .preview_statement(scope, company(scope)?, account, &held.bytes, guess.as_ref())
                .await,
        )?;
        match preview {
            Ok(preview) => {
                self.draft = draft(&preview.mapping);
                self.file = Some(held);
            }
            Err(e) => self.refusal = Some(e),
        }
        Ok(Outcome::Stay)
    }

    /// The dialog in its current step.
    pub(super) async fn modal(
        &self,
        pages: &Pages,
        scope: &Scope,
        account: &crate::BankAccount,
        uploads: &Uploads,
        units: MinorUnits,
    ) -> Result<Html> {
        let alert = match &self.refusal {
            Some(text) => render(Alert {
                id: "statement-import-alert",
                kind: StatusKind::Error,
                body: Html::text(text),
                ..Alert::default()
            })?,
            None => Html::EMPTY,
        };
        let Some(file) = &self.file else {
            let mapped = account.import_mapping.is_some();
            let check = if mapped {
                render(Toggle {
                    on: self.check,
                    label: "Check the columns before importing",
                    id: "statement-import-check",
                    event: "import_check",
                    ..Toggle::default()
                })?
            } else {
                Html::EMPTY
            };
            let body = render(FileStep {
                alert,
                upload: render(uploads.input(UPLOAD, "statement-file"))?,
                check: if mapped {
                    render(CheckRow { toggle: check })?
                } else {
                    Html::EMPTY
                },
                verb: if mapped && !self.check {
                    "Import statement"
                } else {
                    "Next"
                },
            })?;
            return render(Modal {
                id: "statement-import-modal",
                title: "Import statement",
                description: if mapped {
                    "Choose the CSV file from the bank. A line cannot be imported twice."
                } else {
                    "Choose the CSV file from the bank. Next, you check which column is which."
                },
                cancel_event: "close_import",
                body,
                ..Modal::default()
            });
        };
        let preview = told(
            pages
                .reconciliation
                .preview_statement(
                    scope,
                    company(scope)?,
                    account.id,
                    &file.bytes,
                    Some(&mapping(&self.draft)),
                )
                .await,
        )?;
        let preview = match preview {
            Ok(preview) => preview,
            Err(e) => {
                let body = render(Alert {
                    id: "statement-import-alert",
                    kind: StatusKind::Error,
                    body: Html::text(&e),
                    ..Alert::default()
                })?;
                return self.columns_modal(file, body, false);
            }
        };
        let body = self.columns_body(&preview, alert, units)?;
        self.columns_modal(file, body, preview.refusal.is_none())
    }

    fn columns_modal(&self, file: &Held, body: Html, ready: bool) -> Result<Html> {
        let mut actions = render(Button {
            id: "statement-import-back",
            button_type: "button",
            event: "import_choose_file",
            body: Html::text("Choose another file"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "statement-import-cancel",
            button_type: "button",
            event: "close_import",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        actions.push(&render(Button {
            id: "statement-import-mapped",
            button_type: "button",
            event: "import_mapped",
            variant: ButtonVariant::Primary,
            working: "Importing…",
            disabled: !ready,
            body: Html::text("Import statement"),
            ..Button::default()
        })?);
        let mut body = body;
        body.push(&super::dialog_row(actions)?);
        render(Modal {
            id: "statement-import-modal",
            title: &format!("Match the columns of {}", file.name),
            description: "Check which column is which. Bilibi remembers this for the next file from this bank account.",
            cancel_event: "close_import",
            width: ModalWidth::Wide,
            body,
            ..Modal::default()
        })
    }

    fn columns_body(
        &self,
        preview: &StatementPreview,
        alert: Html,
        units: MinorUnits,
    ) -> Result<Html> {
        let mut alert = alert;
        if let Some(refusal) = &preview.refusal {
            if self.refusal.is_none() {
                alert.push(&render(Alert {
                    id: "statement-import-preview-alert",
                    kind: StatusKind::Warning,
                    body: Html::text(&format::refusal(refusal)),
                    ..Alert::default()
                })?);
            }
        }
        // The file's first rows, as the bank wrote them.
        let builtins: Vec<BuiltinColumn> = preview
            .columns
            .iter()
            .enumerate()
            .map(|(i, label)| BuiltinColumn::new(&format!("c{}", i + 1), label))
            .collect();
        let first_rows = if builtins.is_empty() {
            Html::text("The file holds no rows.")
        } else {
            let columns = ListColumns::mount("statement-preview-rows", builtins)
                .map_err(|_| Error::Internal)?;
            let rows = preview
                .rows
                .iter()
                .enumerate()
                .map(|(n, row)| {
                    (0..preview.columns.len()).fold(FlexRow::new((n + 1).to_string()), |r, i| {
                        r.cell(
                            &format!("c{}", i + 1),
                            Cell::text(row.get(i).cloned().unwrap_or_default()),
                        )
                    })
                })
                .collect();
            let mut table = FlexTable::new("statement-preview-rows", columns.flex_columns(), rows);
            table.caption = "The file's first rows";
            render(table)?
        };
        // Only the choices are a form: a FlexTable carries forms of its own,
        // which cannot nest in another, so the tables sit outside it and the
        // buttons send their own events (the server keeps the draft).
        let fields = Form {
            id: "statement-columns-form",
            event: "import_columns",
            change: true,
            class: "",
            body: render(FieldGrid {
                fields: self.fields(preview)?,
            })?,
            actions: Html::EMPTY,
        };
        let fields = render(fields)?;
        // The first lines as the import reads them.
        let columns = ListColumns::mount(
            "statement-preview-lines",
            vec![
                BuiltinColumn::new("date", "Date"),
                BuiltinColumn::new("description", "Description"),
                BuiltinColumn::new("amount", "Amount").right(),
            ],
        )
        .map_err(|_| Error::Internal)?;
        let rows = preview
            .lines
            .iter()
            .enumerate()
            .map(|(n, line)| {
                let date = line.date.to_string();
                let mut row = FlexRow::new((n + 1).to_string())
                    .cell(
                        "date",
                        Cell::date(&date, &bilibi_finance_ledger::dates::display_text(&date)),
                    )
                    .slot(
                        "date",
                        super::nowrap(&bilibi_finance_ledger::dates::display_text(&date))?,
                    )
                    .cell("description", Cell::text(&line.description))
                    .cell("amount", Cell::text(line.amount.display(units)));
                row.dom_id = Some(format!("statement-preview-line-{}", n + 1));
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new("statement-preview-lines", columns.flex_columns(), rows);
        table.caption = "How the first lines import";
        table.empty = Some(bilibi_ui::TableEmpty::State(
            bilibi_ui::EmptyState::missing(
                "No lines could be read yet.",
                "Choose the date, description and amount columns above.",
            ),
        ));
        let lines_title = if preview.refusal.is_some() {
            "How the first lines import".to_owned()
        } else {
            let skipped = match preview.skipped_count {
                0 => String::new(),
                1 => "; 1 row with no amount is skipped".into(),
                n => format!("; {n} rows with no amount are skipped"),
            };
            format!(
                "How the first lines import: {} {} in all, money in positive{skipped}",
                preview.line_count,
                if preview.line_count == 1 {
                    "line"
                } else {
                    "lines"
                }
            )
        };
        render(ColumnsBody {
            alert,
            rows_title: "The file's first rows",
            rows: first_rows,
            fields,
            lines_title,
            lines: render(table)?,
        })
    }

    fn fields(&self, preview: &StatementPreview) -> Result<Html> {
        let get = |key: &str| self.draft.get(key).cloned().unwrap_or_default();
        let columns = options(preview);
        let column = |key: &str, label: &str, required: bool, hint: &str| {
            let mut field = Field::in_form(FORM, key)
                .id(format!("statement-columns-{key}"))
                .label(label)
                .kind(InputKind::Select)
                .prompt(if required { "Choose a column" } else { "None" })
                .options(columns.clone())
                .value(get(key));
            if required {
                field = field.required();
            }
            if !hint.is_empty() {
                field = field.hint(hint);
            }
            field
        };
        let split = get("amount_layout") == "split";
        let mut fields = vec![
            column("date", "Date", true, ""),
            Field::in_form(FORM, "date_format")
                .id("statement-columns-date-format")
                .label("Dates are written as")
                .kind(InputKind::Select)
                .options(DATE_FORMATS.iter().map(|f| Choice::new(*f, *f)).collect())
                .value(get("date_format")),
            column("description", "Description", true, ""),
            column(
                "reference",
                "Reference",
                false,
                "A cheque or transfer number, if the file has one.",
            ),
            Field::in_form(FORM, "amount_layout")
                .id("statement-columns-amount-layout")
                .label("Amounts are in")
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("One signed column", "single"),
                    Choice::new("Money out and money in columns", "split"),
                ])
                .value(if split { "split" } else { "single" }),
        ];
        if split {
            fields.push(column("debit", "Money out", true, ""));
            fields.push(column("credit", "Money in", true, ""));
        } else {
            fields.push(column("amount", "Amount", true, ""));
            fields.push(
                Field::in_form(FORM, "amount_sign")
                    .id("statement-columns-amount-sign")
                    .label("A positive amount is")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("Money in", "deposits_positive"),
                        Choice::new("Money out", "withdrawals_positive"),
                    ])
                    .value(get("amount_sign")),
            );
        }
        fields.extend([
            column(
                "balance",
                "Running balance",
                false,
                "With it, the import checks each balance follows from the amounts.",
            ),
            Field::in_form(FORM, "decimal_separator")
                .id("statement-columns-decimal-separator")
                .label("Decimal mark")
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("Point: 1,234.56", "."),
                    Choice::new("Comma: 1.234,56", ","),
                ])
                .value(get("decimal_separator")),
            Field::in_form(FORM, "delimiter")
                .id("statement-columns-delimiter")
                .label("Columns are separated by")
                .kind(InputKind::Select)
                .options(
                    [
                        ("Comma", ","),
                        ("Semicolon", ";"),
                        ("Tab", "\t"),
                        ("Pipe", "|"),
                    ]
                    .into_iter()
                    .map(|(l, v)| Choice::new(l, v))
                    .collect(),
                )
                .value(get("delimiter")),
            Field::in_form(FORM, "header_rows")
                .id("statement-columns-header-rows")
                .label("Rows to skip at the top")
                .kind(InputKind::Number)
                .attr("min", "0")
                .attr("max", "20")
                .hint("Count the heading row. Use 0 when the file has no heading.")
                .value(get("header_rows")),
        ]);
        let mut html = Html::EMPTY;
        for field in fields {
            html.push(&render(field)?);
        }
        Ok(html)
    }
}

#[derive(Template)]
#[template(
    source = r#"<div class="grid gap-x-6 md:grid-cols-2">{{ fields|safe }}</div>"#,
    ext = "html"
)]
struct FieldGrid {
    fields: Html,
}

#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center justify-between gap-3 text-sm"><span>Check the columns before importing</span>{{ toggle|safe }}</div>"#,
    ext = "html"
)]
struct CheckRow {
    toggle: Html,
}

fn new_file(file: &Held) -> NewFile {
    NewFile {
        file_name: file.name.clone(),
        content: file.bytes.clone(),
        content_type: Some(file.content_type.clone()),
    }
}

fn told<T>(result: crate::Result<T>) -> Result<std::result::Result<T, String>> {
    format::told(result)
}

/// The file's columns as choices: by header text where it names one column,
/// else by position (`#3`), which is how [`mapping`] reads them back.
fn options(preview: &StatementPreview) -> Vec<Choice> {
    let headed = preview.mapping["header_rows"].as_u64().unwrap_or(1) > 0;
    preview
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let unique = preview
                .columns
                .iter()
                .filter(|other| *other == name)
                .count()
                == 1;
            if headed && !name.is_empty() && unique {
                Choice::new(name, name)
            } else {
                Choice::new(format!("Column {}", i + 1), format!("#{}", i + 1))
            }
        })
        .collect()
}

/// The form's choices from a mapping.
fn draft(mapping: &Value) -> HashMap<String, String> {
    let mut draft = HashMap::new();
    for (key, value) in mapping.as_object().into_iter().flatten() {
        let text = match value {
            Value::Null => String::new(),
            Value::Number(n) if COLUMNS.contains(&key.as_str()) => format!("#{n}"),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        draft.insert(key.clone(), text);
    }
    let split = !draft.get("debit").is_none_or(String::is_empty)
        || !draft.get("credit").is_none_or(String::is_empty);
    draft.insert(
        "amount_layout".into(),
        if split { "split" } else { "single" }.into(),
    );
    draft
}

/// The mapping the form's choices make, as `ImportMapping::parse` reads it.
fn mapping(draft: &HashMap<String, String>) -> Value {
    let get = |key: &str| draft.get(key).map(String::as_str).unwrap_or_default();
    let split = get("amount_layout") == "split";
    let column = |key: &str| -> Value {
        let skip = match key {
            "amount" => split,
            "debit" | "credit" => !split,
            _ => false,
        };
        match get(key) {
            _ if skip => Value::Null,
            "" => Value::Null,
            text => match text.strip_prefix('#').and_then(|n| n.parse::<u32>().ok()) {
                Some(position) => json!(position),
                None => json!(text),
            },
        }
    };
    let mut map = serde_json::Map::new();
    for key in COLUMNS {
        map.insert(key.into(), column(key));
    }
    for (key, default) in [
        ("delimiter", ","),
        ("date_format", "YYYY-MM-DD"),
        ("amount_sign", "deposits_positive"),
        ("decimal_separator", "."),
    ] {
        let text = draft
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .unwrap_or(default);
        map.insert(key.into(), json!(text));
    }
    map.insert(
        "header_rows".into(),
        get("header_rows")
            .trim()
            .parse::<i64>()
            .map_or(json!(1), |rows| json!(rows)),
    );
    Value::Object(map)
}
