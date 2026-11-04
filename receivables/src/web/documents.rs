//! The invoices and credit notes list, and its New invoice and New credit
//! note dialogs. Reads `document.page`, `customer.list` and `series.list`;
//! saving a draft calls `draft.create`.
use super::opening::Opening;
use super::{
    format::{self as f, Errors},
    render, Flash, Pages, Written, INVOICES,
};
use crate::{records::DocumentPageRow, Customer, Document, Series, MANAGE_DOCUMENTS, VIEW};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BuiltinColumn, Button, ButtonVariant, Cell, Choice, ColumnsOutcome, CreateButton,
    EmptyState, Field, Filter, FilterToolbar, FlexRow, FlexTable, Form, Header, Html, InputKind,
    ListColumns, ListSpec, ListState, ModalWidth, PageCounts, PageVariant, Pagination, RecordLink,
    SortDir, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("number".into(), SortDir::Desc),
            ("issue_date".into(), SortDir::Desc),
            ("due_date".into(), SortDir::Asc),
            ("total".into(), SortDir::Desc),
        ]),
        default_sort: Some("number".into()),
        page_sizes: SIZES.to_vec(),
        filters: [
            (
                "kind".into(),
                Filter::one_of(&["", "invoice", "credit_note", "opening_credit"], ""),
            ),
            (
                "status".into(),
                Filter::one_of(&["", "draft", "issued", "unpaid", "discarded"], ""),
            ),
            ("customer".into(), Filter::string("")),
            (
                "source".into(),
                Filter::one_of(&["", "ordinary", "opening"], ""),
            ),
        ]
        .into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("documents list spec")
});

/// What an invoice or credit note is called: its issued reference, or a draft
/// by the number the system keeps for it.
pub(super) fn reference(d: &Document) -> String {
    d.document_reference
        .clone()
        .unwrap_or_else(|| format!("Draft #{}", d.id))
}

/// The dialog to start a draft: which kind, and what the person has typed.
struct Draft {
    kind: &'static str,
    values: HashMap<String, String>,
    errors: Errors,
}

pub(super) struct Documents {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    documents: Vec<DocumentPageRow>,
    customers: Vec<Customer>,
    series: Vec<Series>,
    figures: f::Figures,
    total: u64,
    can_add: bool,
    create: Option<Draft>,
    opening: Option<Opening>,
    can_open: bool,
    credit_invoices: Vec<Document>,
    flash: Flash,
    framed: bool,
}

impl Documents {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("number", "Number").sortable("number"),
            BuiltinColumn::new("kind", "Type"),
            BuiltinColumn::new("customer", "Customer"),
            BuiltinColumn::new("issue_date", "Issued").sortable("issue_date"),
            BuiltinColumn::new("due_date", "Due").sortable("due_date"),
            BuiltinColumn::new("total", "Total")
                .sortable("total")
                .right(),
            BuiltinColumn::new("outstanding", "Outstanding").right(),
            BuiltinColumn::new("source", "Source"),
            BuiltinColumn::new("status", "Status"),
        ];
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("receivables-invoices-table", columns)
                .map_err(|_| Error::Internal)?,
            documents: vec![],
            customers: vec![],
            series: vec![],
            figures: f::Figures::default(),
            total: 0,
            can_add: false,
            create: None,
            opening: None,
            can_open: false,
            credit_invoices: vec![],
            flash: Flash::default(),
            framed,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.body(scope).await
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.load_page(scope).await?;
        self.load_choices(scope).await
    }

    async fn load_page(&mut self, scope: &Scope) -> Result<()> {
        let mut input = json!({"search":self.list.search,"page":self.list.page,"page_size":self.list.page_size});
        for (key, input_key) in [
            ("source", "source"),
            ("kind", "kind"),
            ("status", "status"),
            ("customer", "customer_id"),
        ] {
            if let Some(value) = self.list.filters.get(key).filter(|value| !value.is_empty()) {
                input[input_key] = if input_key == "customer_id" {
                    json!(value.parse::<i64>().map_err(|_| Error::Invalid)?)
                } else {
                    json!(value)
                };
            }
        }
        if let Some(sort) = self.list.sort_by.as_deref() {
            input["sort"] = json!(sort);
        }
        if let Some(direction) = self.list.sort_dir {
            input["direction"] = json!(if direction == SortDir::Asc {
                "asc"
            } else {
                "desc"
            });
        }
        let page = self
            .pages
            .read(scope, "finance.receivables.document.page", input)
            .await?;
        self.documents =
            serde_json::from_value(page["documents"].clone()).map_err(|_| Error::Internal)?;
        self.total = page["total"].as_u64().ok_or(Error::Internal)?;
        self.list.page = page["page"].as_u64().ok_or(Error::Internal)?;
        Ok(())
    }

    async fn load_choices(&mut self, scope: &Scope) -> Result<()> {
        self.customers = self
            .pages
            .read_all(
                scope,
                "finance.receivables.customer.list",
                json!({}),
                ("after", "next_after_id"),
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
        self.figures = self.pages.figures(scope).await;
        self.can_add = self.pages.allowed(scope, MANAGE_DOCUMENTS).await?;
        self.can_open = self.pages.allowed(scope, VIEW).await?;
        Ok(())
    }

    fn customer_name(&self, d: &Document) -> String {
        let customer = self.customers.iter().find(|c| c.id == d.customer_id);
        let name = d
            .party_name
            .as_deref()
            .or(customer.map(|c| c.name.as_str()))
            .unwrap_or("Customer unavailable");
        let code = d
            .party_code
            .as_deref()
            .or(customer.map(|c| c.code.as_str()))
            .unwrap_or("");
        format!("{code} · {name}")
    }

    fn page_customer_name(&self, d: &DocumentPageRow) -> String {
        let customer = self.customers.iter().find(|c| c.id == d.customer_id);
        let name = d
            .party_name
            .as_deref()
            .or(customer.map(|c| c.name.as_str()))
            .unwrap_or("Customer unavailable");
        let code = d
            .party_code
            .as_deref()
            .or(customer.map(|c| c.code.as_str()))
            .unwrap_or("");
        format!("{code} · {name}")
    }

    fn page_reference(d: &DocumentPageRow) -> String {
        d.document_reference
            .clone()
            .unwrap_or_else(|| format!("Draft #{}", d.id))
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let framed = self.framed;
        self.columns.load(&self.documents, |d| d.id.to_string());
        let mut actions = if self.can_add {
            let mut a = render(CreateButton {
                noun: "Invoice",
                id: "receivables-invoices-add",
                event: "show_create_invoice",
                ..CreateButton::default()
            })?;
            a.push(&render(Button {
                id: "receivables-credit-notes-add",
                button_type: "button",
                event: "show_create_credit_note",
                body: Html::text("New credit note"),
                ..Button::default()
            })?);
            a
        } else {
            Html::EMPTY
        };
        if self.can_open {
            actions.push(&render(Button {
                id: "receivables-opening",
                button_type: "button",
                event: "open_opening",
                body: Html::text("Opening invoices"),
                ..Button::default()
            })?);
        }
        let mut customer_choices = vec![Choice::new("All customers", "")];
        customer_choices.extend(
            self.customers
                .iter()
                .map(|c| Choice::new(format!("{} {}", c.code, c.name), c.id.to_string())),
        );
        let mut content = render(FilterToolbar {
            event: "filter",
            ..FilterToolbar::new(
                "receivables-invoices-filters",
                vec![
                    ToolbarControl::search(
                        "receivables-invoices-search",
                        "search",
                        "Search invoices",
                        &self.list.search,
                        "Search by number, customer or description…",
                    ),
                    ToolbarControl::select(
                        "receivables-invoices-kind",
                        "kind",
                        "Filter by type",
                        &self.list.filters["kind"],
                        [
                            ("All types", ""),
                            ("Invoices", "invoice"),
                            ("Credit notes", "credit_note"),
                            ("Customer credits", "opening_credit"),
                        ]
                        .map(|(l, v)| Choice::new(l, v))
                        .to_vec(),
                    ),
                    ToolbarControl::select(
                        "receivables-invoices-source",
                        "source",
                        "Filter by source",
                        &self.list.filters["source"],
                        [
                            ("All sources", ""),
                            ("Ordinary", "ordinary"),
                            ("Opening", "opening"),
                        ]
                        .map(|(l, v)| Choice::new(l, v))
                        .to_vec(),
                    ),
                    ToolbarControl::select(
                        "receivables-invoices-status",
                        "status",
                        "Filter by status",
                        &self.list.filters["status"],
                        [
                            ("All statuses", ""),
                            ("Draft", "draft"),
                            ("Issued", "issued"),
                            ("Unpaid", "unpaid"),
                            ("Discarded", "discarded"),
                        ]
                        .map(|(l, v)| Choice::new(l, v))
                        .to_vec(),
                    ),
                    ToolbarControl::select(
                        "receivables-invoices-customer",
                        "customer",
                        "Filter by customer",
                        &self.list.filters["customer"],
                        customer_choices,
                    ),
                ],
            )
        })?;
        let mut rows = Vec::new();
        for d in &self.documents {
            let mut row = FlexRow::new(d.id.to_string());
            row.dom_id = Some(format!("receivables-invoice-{}", d.id));
            rows.push(
                row.slot(
                    "number",
                    render(RecordLink {
                        workspace: framed,
                        kind: crate::MODULE,
                        record_id: &d.id.to_string(),
                        href: &format!("{INVOICES}/{}", d.id),
                        class: "font-medium text-ink-strong hover:underline",
                        body: Html::text(&Self::page_reference(d)),
                    })?,
                )
                .cell("kind", Cell::text(f::kind_label(&d.kind)))
                .cell("customer", Cell::text(self.page_customer_name(d)))
                .cell("issue_date", Cell::text(f::optional(&d.issue_date)))
                .cell(
                    "due_date",
                    Cell::text(if d.kind == "opening_credit" {
                        "—"
                    } else {
                        f::optional(&d.due_date)
                    }),
                )
                .slot("total", f::tabular(&self.figures.money(d.total))?)
                .slot(
                    "outstanding",
                    f::tabular(&if d.status == "issued"
                        && matches!(d.kind.as_str(), "invoice" | "opening_credit")
                    {
                        self.figures.optional(d.outstanding)
                    } else {
                        "—".into()
                    })?,
                )
                .cell(
                    "source",
                    Cell::text(if d.opening { "Opening" } else { "Ordinary" }),
                )
                .slot("status", f::status_badge(&d.status)?),
            );
        }
        let mut table = FlexTable::new(
            "receivables-invoices-table",
            self.columns.flex_columns(),
            rows,
        )
        .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Invoices and credit notes";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(render(EmptyState::missing(
            "No invoices",
            if self.total == 0
                && self.list.search.is_empty()
                && self.list.filters.values().all(String::is_empty)
            {
                "Start an invoice for a customer. It stays a draft until you issue it."
            } else {
                "No invoice or credit note matches the current filters."
            },
        ))?));
        content.push(&render(table)?);
        content.push(&render(Pagination {
            filters_event: "filter",
            ..Pagination::new(
                "receivables-invoices-pagination",
                PageCounts::new(self.list.page, self.list.page_size, self.total),
                &SIZES,
            )
        })?);
        let mut body = render(Header {
            title: Html::text("Invoices"),
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
            ..Header::default()
        })?;
        body.push(&content);
        let modal = if let Some(opening) = &mut self.opening {
            Some(opening.body(scope).await?)
        } else {
            self.create
                .as_ref()
                .map(|draft| self.dialog(draft))
                .transpose()?
        };
        f::frame(
            "receivables-invoices",
            PageVariant::List,
            &self.flash,
            body,
            modal,
        )
    }

    fn dialog(&self, draft: &Draft) -> Result<Html> {
        let credit = draft.kind == "credit_note";
        let mut labels = vec![
            ("series_id", "Series"),
            ("customer_id", "Customer"),
            ("issue_date", "Issue date"),
            ("due_date", "Due date"),
            ("description", "Description"),
        ];
        if credit {
            labels.push(("credited_document_id", "Credited invoice"));
        }
        let labels: Vec<_> = labels
            .into_iter()
            .map(|(k, l)| (k.to_owned(), format!("receivables-draft-{k}"), l.to_owned()))
            .collect();
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let mut body = render(draft.errors.summary("receivables-draft-errors", &labels))?;
        let series: Vec<Choice> = self
            .series
            .iter()
            .filter(|s| s.kind == draft.kind && s.active)
            .map(|s| Choice::new(format!("{} ({})", s.name, s.code), s.id.to_string()))
            .collect();
        let customers: Vec<Choice> = self
            .customers
            .iter()
            .filter(|c| c.active || credit)
            .map(|c| Choice::new(format!("{} {}", c.code, c.name), c.id.to_string()))
            .collect();
        let mut who = Html::EMPTY;
        who.push(&render(
            Field::in_form("draft", "series_id")
                .id("receivables-draft-series_id")
                .label("Series")
                .kind(InputKind::Select)
                .prompt(if series.is_empty() {
                    "No series yet — add one in Settings"
                } else {
                    "Choose a series"
                })
                .options(series)
                .value(value("series_id"))
                .required()
                .hint("The numbering the document takes when it is issued.")
                .errors(draft.errors.under("series_id", "Series")),
        )?);
        who.push(&render(
            Field::in_form("draft", "customer_id")
                .id("receivables-draft-customer_id")
                .label("Customer")
                .kind(InputKind::Select)
                .prompt("Choose a customer")
                .options(customers)
                .value(value("customer_id"))
                .required()
                .errors(draft.errors.under("customer_id", "Customer")),
        )?);
        body.push(&f::two(who)?);
        if credit {
            let invoices: Vec<Choice> = self
                .credit_invoices
                .iter()
                .filter(|d| d.kind == "invoice" && d.status == "issued")
                .map(|d| {
                    Choice::new(
                        format!("{} · {}", reference(d), self.customer_name(d)),
                        d.id.to_string(),
                    )
                })
                .collect();
            body.push(&render(
                Field::in_form("draft", "credited_document_id")
                    .id("receivables-draft-credited_document_id")
                    .label("Credited invoice")
                    .kind(InputKind::Select)
                    .prompt("Choose the invoice this credits")
                    .options(invoices)
                    .value(value("credited_document_id"))
                    .required()
                    .hint("It must be an issued invoice to the same customer.")
                    .errors(
                        draft
                            .errors
                            .under("credited_document_id", "Credited invoice"),
                    ),
            )?);
        }
        let mut dates = Html::EMPTY;
        for (key, label, hint) in [
            (
                "issue_date",
                "Issue date",
                "Leave empty to set it when you issue.",
            ),
            (
                "due_date",
                "Due date",
                "Leave empty to use the customer's payment terms.",
            ),
        ] {
            dates.push(&render(
                Field::in_form("draft", key)
                    .id(format!("receivables-draft-{key}"))
                    .label(label)
                    .kind(InputKind::Date)
                    .value(value(key))
                    .hint(hint)
                    .errors(draft.errors.under(key, label)),
            )?);
        }
        body.push(&f::two(dates)?);
        body.push(&render(
            Field::in_form("draft", "description")
                .id("receivables-draft-description")
                .label("Description")
                .kind(InputKind::Textarea)
                .attr("maxlength", "500")
                .value(value("description"))
                .errors(draft.errors.under("description", "Description")),
        )?);
        let mut actions = render(Button {
            id: "receivables-draft-cancel",
            button_type: "button",
            event: "cancel_create",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "receivables-draft-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save draft"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "receivables-draft-form",
            event: "create_draft",
            change: false,
            class: "mt-4",
            body,
            actions: f::actions(actions)?,
        })?;
        f::modal(
            "receivables-draft-modal",
            if credit {
                "New credit note"
            } else {
                "New invoice"
            },
            "A draft has no number and posts nothing until you issue it.",
            "cancel_create",
            ModalWidth::Wide,
            &self.flash,
            form,
        )
    }

    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        v: &serde_json::Value,
    ) -> Result<LiveBody> {
        let mut redirect = None;
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => self.flash.dismiss(v),
            "filter" => {
                self.list.apply_filters(&params::form_values(v, "filters"));
                self.load_page(scope).await?;
            }
            "page" => {
                self.list.put_page(v["page"].as_str());
                self.load_page(scope).await?;
            }
            "grid" => {
                if let ColumnsOutcome::Sort(column) = self.columns.handle(&params::string_values(v))
                {
                    self.list.next_sort(&column);
                    self.load_page(scope).await?;
                }
            }
            kind @ ("show_create_invoice" | "show_create_credit_note") => {
                if self.pages.allowed(scope, MANAGE_DOCUMENTS).await? {
                    let kind = if kind == "show_create_invoice" {
                        "invoice"
                    } else {
                        "credit_note"
                    };
                    self.opening = None;
                    if kind == "credit_note" {
                        self.credit_invoices = self
                            .pages
                            .read_all(
                                scope,
                                "finance.receivables.document.list",
                                json!({"kind":"invoice","status":"issued"}),
                                ("after", "next_after"),
                            )
                            .await?
                            .0;
                    }
                    let mut values = HashMap::new();
                    let mut open = self.series.iter().filter(|s| s.kind == kind && s.active);
                    if let (Some(only), None) = (open.next(), open.next()) {
                        values.insert("series_id".into(), only.id.to_string());
                    }
                    self.create = Some(Draft {
                        kind,
                        values,
                        errors: Errors::default(),
                    });
                } else {
                    self.flash.set(StatusKind::Error, super::DENIED);
                }
            }
            "cancel_create" => self.create = None,
            "create_draft" => {
                let kind = self.create.as_ref().map_or("invoice", |d| d.kind);
                let form = params::form_values(v, "draft");
                let mut input = json!({"kind": kind});
                let mut local = Errors::default();
                for key in ["series_id", "customer_id", "credited_document_id"] {
                    let text = f::posted(&form, key);
                    match text.parse::<i64>() {
                        Ok(id) if id > 0 => input[key] = json!(id),
                        _ if text.is_empty() && key != "series_id" && key != "customer_id" => {}
                        _ => local
                            .0
                            .entry(key.into())
                            .or_default()
                            .push("can't be blank".into()),
                    }
                }
                for key in ["issue_date", "due_date", "description"] {
                    f::put(&mut input, key, f::posted(&form, key));
                }
                if kind == "credit_note" && input.get("credited_document_id").is_none() {
                    local
                        .0
                        .entry("credited_document_id".into())
                        .or_default()
                        .push("can't be blank".into());
                }
                let draft_kind = if kind == "credit_note" {
                    "credit_note"
                } else {
                    "invoice"
                };
                if !local.is_empty() {
                    self.create = Some(Draft {
                        kind: draft_kind,
                        values: form,
                        errors: local,
                    });
                } else {
                    match self
                        .pages
                        .write(
                            scope,
                            credential,
                            "create_draft",
                            "finance.receivables.draft.create",
                            input,
                        )
                        .await?
                    {
                        Written::Done(record) => {
                            self.create = None;
                            self.flash.set(
                                StatusKind::Success,
                                "Draft saved. Add its lines, then issue it.",
                            );
                            if let Some(id) = record["id"].as_i64() {
                                redirect = Some(format!("{INVOICES}/{id}"));
                            }
                            self.load_page(scope).await?;
                        }
                        Written::Invalid(fields) => {
                            self.create = Some(Draft {
                                kind: draft_kind,
                                values: form,
                                errors: Errors(fields),
                            });
                        }
                        Written::Refused(message) => {
                            self.create = None;
                            self.flash.set(
                                StatusKind::Error,
                                super::worded(
                                    message,
                                    "You do not have permission to start documents.",
                                ),
                            );
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        let flash = redirect.is_some().then(|| self.flash.0.take()).flatten();
        Ok(LiveBody {
            html: self.body(scope).await?,
            query: self.list.to_params(),
            redirect,
            flash,
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Documents {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match value["type"].as_str().unwrap_or("") {
                    "open_opening" if self.can_open => {
                        self.create = None;
                        self.opening = Some(Opening::new(self.pages.clone())?);
                    }
                    "close_opening" => self.opening = None,
                    "preview_opening"
                    | "import_opening"
                    | "cancel_opening"
                    | "stage_grid"
                    | "balance_grid"
                    | "classification_grid"
                    | "opening_grid"
                    | "clear-flash"
                        if self.opening.is_some() =>
                    {
                        let opening = self.opening.as_mut().expect("opening dialog");
                        opening.update(&r).await?;
                        if opening.completed {
                            self.opening = None;
                            self.flash.set(StatusKind::Success, "The opening invoices were reconciled. Open an invoice below to read its source evidence.");
                            self.list.search.clear();
                            self.list.apply_filters(&HashMap::from([
                                ("source".into(), "opening".into()),
                                ("status".into(), "".into()),
                                ("kind".into(), "".into()),
                                ("customer".into(), "".into()),
                            ]));
                            self.load_page(r.scope).await?;
                        }
                    }
                    _ => return self.event(r.scope, r.credential, value).await,
                }
            }
            match r.input {
                LiveInput::Refresh => self.load(r.scope).await?,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.load_page(r.scope).await?;
                }
                LiveInput::Event(_) => {}
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
