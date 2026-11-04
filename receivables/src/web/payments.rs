//! The receipts list and its Record receipt dialog. Reads `payment.list`,
//! `customer.list`, `series.list`, `document.list` and the ledger's account
//! list; recording calls `payment.record`. A receipt is allocated in full to
//! issued invoices of one customer, so the dialog lists that customer's open
//! invoices and asks how much of the receipt goes to each.
use super::{
    documents::reference,
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written, DENIED, RECEIPTS,
};
use crate::{Customer, Document, Payment, Series, POST_PAYMENTS};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BadgeKind, BuiltinColumn, Button, ButtonVariant, Cell, Choice, ColumnsOutcome,
    CreateButton, EmptyState, Field, Filter, FilterToolbar, FlexRow, FlexTable, Form, Header, Html,
    InputKind, ListColumns, ListSpec, ListState, ModalWidth, PageCounts, PageVariant, Pagination,
    RecordLink, SectionHeading, SortDir, StatusKind, TableEmpty, ToolbarControl,
};
use rust_decimal::Decimal;
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
            ("payment_date".into(), SortDir::Desc),
            ("amount".into(), SortDir::Desc),
        ]),
        default_sort: Some("number".into()),
        page_sizes: SIZES.to_vec(),
        filters: [
            (
                "status".into(),
                Filter::one_of(&["", "posted", "reversed"], ""),
            ),
            ("customer".into(), Filter::string("")),
        ]
        .into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("receipts list spec")
});

pub(super) fn status_badge(p: &Payment) -> Result<Html> {
    if p.reversal.is_some() {
        f::badge("Reversed", BadgeKind::Warning)
    } else {
        f::badge("Posted", BadgeKind::Success)
    }
}

/// The Record receipt dialog while it is open.
struct Draft {
    values: HashMap<String, String>,
    /// The chosen customer's issued invoices that still owe something.
    open: Vec<Document>,
    errors: Errors,
}

#[derive(askama::Template)]
#[template(
    source = r#"<p id="{{ id }}" class="text-xs text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Note<'a> {
    id: &'a str,
    text: &'a str,
}

pub(super) struct Payments {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    all: Vec<Payment>,
    customers: Vec<Customer>,
    series: Vec<Series>,
    accounts: Vec<Account>,
    control: Option<i64>,
    figures: f::Figures,
    cut: bool,
    can_record: bool,
    draft: Option<Draft>,
    flash: Flash,
    framed: bool,
}

impl Payments {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("number", "Receipt").sortable("number"),
            BuiltinColumn::new("customer", "Customer"),
            BuiltinColumn::new("payment_date", "Date").sortable("payment_date"),
            BuiltinColumn::new("amount", "Amount")
                .sortable("amount")
                .right(),
            BuiltinColumn::new("deposit", "Deposited to"),
            BuiltinColumn::new("reference", "Reference"),
            BuiltinColumn::new("status", "Status"),
        ];
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("receivables-receipts-table", columns)
                .map_err(|_| Error::Internal)?,
            all: vec![],
            customers: vec![],
            series: vec![],
            accounts: vec![],
            control: None,
            figures: f::Figures::default(),
            cut: false,
            can_record: false,
            draft: None,
            flash: Flash::default(),
            framed,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.body()
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        (self.all, self.cut) = self
            .pages
            .read_all(
                scope,
                "finance.receivables.payment.list",
                json!({}),
                ("after", "next_after"),
            )
            .await?;
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
        self.accounts = self.pages.accounts(scope).await;
        self.control = self
            .pages
            .read(scope, "finance.receivables.setup.get", json!({}))
            .await
            .ok()
            .and_then(|v| v["setup"]["receivable_account_id"].as_i64());
        self.figures = self.pages.figures(scope).await;
        self.can_record = self.pages.allowed(scope, POST_PAYMENTS).await?;
        Ok(())
    }

    fn customer_name(&self, id: i64) -> String {
        self.customers.iter().find(|c| c.id == id).map_or_else(
            || format!("Customer #{id}"),
            |c| format!("{} · {}", c.code, c.name),
        )
    }

    fn shown(&mut self) -> (Vec<Payment>, u64) {
        let search = self.list.search.trim().to_lowercase();
        let filter = |key: &str| self.list.filters.get(key).cloned().unwrap_or_default();
        let (status, customer) = (filter("status"), filter("customer"));
        let mut rows: Vec<&Payment> = self
            .all
            .iter()
            .filter(|p| match status.as_str() {
                "posted" => p.reversal.is_none(),
                "reversed" => p.reversal.is_some(),
                _ => true,
            })
            .filter(|p| customer.is_empty() || p.customer_id.to_string() == customer)
            .filter(|p| {
                search.is_empty()
                    || p.payment_reference.to_lowercase().contains(&search)
                    || self
                        .customer_name(p.customer_id)
                        .to_lowercase()
                        .contains(&search)
                    || p.reference
                        .as_ref()
                        .is_some_and(|t| t.to_lowercase().contains(&search))
            })
            .collect();
        match self.list.sort_by.as_deref() {
            Some("payment_date") => rows.sort_by(|a, b| a.payment_date.cmp(&b.payment_date)),
            Some("amount") => rows.sort_by_key(|p| p.amount.amount()),
            _ => rows.sort_by_key(|p| p.id),
        }
        if self.list.sort_dir == Some(SortDir::Desc) {
            rows.reverse();
        }
        let total = rows.len() as u64;
        let size = self.list.page_size.max(1);
        self.list.page = self.list.page.clamp(1, total.div_ceil(size).max(1));
        let from = ((self.list.page - 1) * size) as usize;
        (
            rows.into_iter()
                .skip(from)
                .take(size as usize)
                .cloned()
                .collect(),
            total,
        )
    }

    fn body(&mut self) -> Result<String> {
        let framed = self.framed;
        let (shown, total) = self.shown();
        self.columns.load(&shown, |p| p.id.to_string());
        let actions = if self.can_record {
            render(CreateButton {
                noun: "Receipt",
                id: "receivables-receipts-add",
                event: "show_record",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut customer_choices = vec![Choice::new("All customers", "")];
        customer_choices.extend(
            self.customers
                .iter()
                .map(|c| Choice::new(format!("{} {}", c.code, c.name), c.id.to_string())),
        );
        let mut content = render(FilterToolbar {
            event: "filter",
            ..FilterToolbar::new(
                "receivables-receipts-filters",
                vec![
                    ToolbarControl::search(
                        "receivables-receipts-search",
                        "search",
                        "Search receipts",
                        &self.list.search,
                        "Search by number, customer or reference…",
                    ),
                    ToolbarControl::select(
                        "receivables-receipts-status",
                        "status",
                        "Filter by status",
                        &self.list.filters["status"],
                        [
                            ("All statuses", ""),
                            ("Posted", "posted"),
                            ("Reversed", "reversed"),
                        ]
                        .map(|(l, v)| Choice::new(l, v))
                        .to_vec(),
                    ),
                    ToolbarControl::select(
                        "receivables-receipts-customer",
                        "customer",
                        "Filter by customer",
                        &self.list.filters["customer"],
                        customer_choices,
                    ),
                ],
            )
        })?;
        let mut rows = Vec::new();
        for p in &shown {
            let mut row = FlexRow::new(p.id.to_string());
            row.dom_id = Some(format!("receivables-receipt-{}", p.id));
            rows.push(
                row.slot(
                    "number",
                    render(RecordLink {
                        workspace: framed,
                        kind: crate::MODULE,
                        record_id: &p.id.to_string(),
                        href: &format!("{RECEIPTS}/{}", p.id),
                        class: "font-medium text-ink-strong hover:underline",
                        body: Html::text(&p.payment_reference),
                    })?,
                )
                .cell("customer", Cell::text(self.customer_name(p.customer_id)))
                .cell("payment_date", Cell::text(&p.payment_date))
                .slot("amount", f::tabular(&self.figures.money(p.amount))?)
                .cell(
                    "deposit",
                    Cell::text(super::account_label(&self.accounts, p.deposit_account_id)),
                )
                .cell("reference", Cell::text(f::optional(&p.reference)))
                .slot("status", status_badge(p)?),
            );
        }
        let mut table = FlexTable::new(
            "receivables-receipts-table",
            self.columns.flex_columns(),
            rows,
        )
        .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Receipts";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(render(EmptyState::missing(
            "No receipts",
            if self.all.is_empty() {
                "Record the money customers pay you. Each receipt is applied to the invoices it settles."
            } else {
                "No receipt matches the current filters."
            },
        ))?));
        content.push(&render(table)?);
        content.push(&render(Pagination {
            filters_event: "filter",
            ..Pagination::new(
                "receivables-receipts-pagination",
                PageCounts::new(self.list.page, self.list.page_size, total),
                &SIZES,
            )
        })?);
        if self.cut {
            content.push(&Html::text(
                "Only the first receipts are listed. Search to narrow them.",
            ));
        }
        let mut body = render(Header {
            title: Html::text("Receipts"),
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
            ..Header::default()
        })?;
        body.push(&content);
        let modal = match &self.draft {
            Some(draft) => Some(self.dialog(draft)?),
            None => None,
        };
        f::frame(
            "receivables-receipts",
            PageVariant::List,
            &self.flash,
            body,
            modal,
        )
    }

    fn dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let mut labels = vec![
            (
                "customer_id".to_owned(),
                "receivables-receipt-customer_id".to_owned(),
                "Customer".to_owned(),
            ),
            (
                "series_id".into(),
                "receivables-receipt-series_id".into(),
                "Series".into(),
            ),
            (
                "payment_date".into(),
                "receivables-receipt-payment_date".into(),
                "Date received".into(),
            ),
            (
                "amount".into(),
                "receivables-receipt-amount".into(),
                "Amount received".into(),
            ),
            (
                "deposit_account_id".into(),
                "receivables-receipt-deposit_account_id".into(),
                "Deposited to".into(),
            ),
        ];
        labels.push((
            "allocations".into(),
            "receivables-receipt-total".into(),
            "The amounts applied".into(),
        ));
        for (i, d) in draft.open.iter().enumerate() {
            labels.push((
                format!("allocations.{i}.amount"),
                format!("receivables-receipt-alloc-{}", d.id),
                format!("Amount for {}", reference(d)),
            ));
        }
        let mut body = render(draft.errors.summary("receivables-receipt-errors", &labels))?;
        let customers: Vec<Choice> = self
            .customers
            .iter()
            .map(|c| Choice::new(format!("{} {}", c.code, c.name), c.id.to_string()))
            .collect();
        let series: Vec<Choice> = self
            .series
            .iter()
            .filter(|s| s.kind == "payment" && s.active)
            .map(|s| Choice::new(format!("{} ({})", s.name, s.code), s.id.to_string()))
            .collect();
        let deposits: Vec<Choice> = self
            .accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == "asset" && Some(a.id) != self.control)
            .map(|a| Choice::new(a.label(), a.id.to_string()))
            .collect();
        let mut who = Html::EMPTY;
        who.push(&render(
            Field::in_form("receipt", "customer_id")
                .id("receivables-receipt-customer_id")
                .label("Customer")
                .kind(InputKind::Select)
                .prompt("Choose a customer")
                .options(customers)
                .value(value("customer_id"))
                .required()
                .errors(draft.errors.under("customer_id", "Customer")),
        )?);
        who.push(&render(
            Field::in_form("receipt", "series_id")
                .id("receivables-receipt-series_id")
                .label("Series")
                .kind(InputKind::Select)
                .prompt(if series.is_empty() {
                    "No receipt series yet — add one in Settings"
                } else {
                    "Choose a series"
                })
                .options(series)
                .value(value("series_id"))
                .required()
                .errors(draft.errors.under("series_id", "Series")),
        )?);
        body.push(&f::two(who)?);
        let mut money = Html::EMPTY;
        money.push(&render(
            Field::in_form("receipt", "payment_date")
                .id("receivables-receipt-payment_date")
                .label("Date received")
                .kind(InputKind::Date)
                .value(value("payment_date"))
                .required()
                .hint("A receipt cannot settle an invoice dated after it.")
                .errors(draft.errors.under("payment_date", "Date received")),
        )?);
        money.push(&render(
            Field::in_form("receipt", "amount")
                .id("receivables-receipt-amount")
                .label("Amount received")
                .attr("inputmode", "decimal")
                .value(value("amount"))
                .required()
                .errors(draft.errors.under("amount", "Amount received")),
        )?);
        body.push(&f::two(money)?);
        let mut deposit = Html::EMPTY;
        deposit.push(&render(
            Field::in_form("receipt", "deposit_account_id")
                .id("receivables-receipt-deposit_account_id")
                .label("Deposited to")
                .kind(InputKind::Select)
                .prompt("Choose the bank or cash account")
                .options(deposits)
                .value(value("deposit_account_id"))
                .required()
                .errors(draft.errors.under("deposit_account_id", "Deposited to")),
        )?);
        deposit.push(&render(
            Field::in_form("receipt", "reference")
                .id("receivables-receipt-reference")
                .label("Reference")
                .attr("maxlength", "100")
                .hint("A cheque or transfer number, if there is one.")
                .value(value("reference"))
                .errors(draft.errors.under("reference", "Reference")),
        )?);
        body.push(&f::two(deposit)?);
        body.push(&render(
            Field::in_form("receipt", "description")
                .id("receivables-receipt-description")
                .label("Description")
                .attr("maxlength", "500")
                .value(value("description"))
                .errors(draft.errors.under("description", "Description")),
        )?);
        body.push(&render(SectionHeading {
            title: "Apply to invoices",
            description: "The whole receipt must be applied. Give each invoice it settles an amount, up to what it still owes.",
            ..SectionHeading::default()
        })?);
        if value("customer_id").is_empty() {
            body.push(&render(Note {
                id: "receivables-receipt-pick",
                text: "Choose a customer to see what they owe.",
            })?);
        } else if draft.open.is_empty() {
            body.push(&render(Note {
                id: "receivables-receipt-none",
                text: "This customer has no issued invoice that is still owed.",
            })?);
        }
        for d in &draft.open {
            body.push(&render(
                Field::in_form("receipt", &format!("alloc_{}", d.id))
                    .id(format!("receivables-receipt-alloc-{}", d.id))
                    .label(format!(
                        "{} · due {} · owes {}",
                        reference(d),
                        d.due_date.as_deref().unwrap_or("—"),
                        self.figures.optional(d.outstanding)
                    ))
                    .attr("inputmode", "decimal")
                    .value(value(&format!("alloc_{}", d.id))),
            )?);
        }
        let (applied, received) = self.totals(draft);
        body.push(&render(Note {
            id: "receivables-receipt-total",
            text: &format!(
                "Applied {} of {} received.",
                self.figures.decimal(applied),
                self.figures.decimal(received)
            ),
        })?);
        let form = render(Form {
            id: "receivables-receipt-form",
            event: "receipt_change",
            change: true,
            class: "mt-4",
            body,
            actions: Html::EMPTY,
        })?;
        let mut buttons = render(Button {
            id: "receivables-receipt-fill",
            button_type: "button",
            event: "apply_oldest",
            body: Html::text("Apply to oldest first"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "receivables-receipt-cancel",
            button_type: "button",
            event: "cancel_record",
            class: "ml-auto",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        buttons.push(&render(Button {
            id: "receivables-receipt-save",
            button_type: "button",
            event: "save_receipt",
            variant: ButtonVariant::Primary,
            working: "Recording…",
            body: Html::text("Record receipt"),
            ..Button::default()
        })?);
        let mut inner = form;
        inner.push(&f::buttons(buttons)?);
        f::modal(
            "receivables-receipt-modal",
            "Record receipt",
            "Recording numbers the receipt and posts it to the ledger.",
            "cancel_record",
            ModalWidth::Wide,
            &self.flash,
            inner,
        )
    }

    /// What the typed allocations add up to, and the amount received.
    fn totals(&self, draft: &Draft) -> (Decimal, Decimal) {
        let number = |text: &str| text.trim().parse::<Decimal>().unwrap_or(Decimal::ZERO);
        let applied = draft
            .open
            .iter()
            .map(|d| {
                number(
                    draft
                        .values
                        .get(&format!("alloc_{}", d.id))
                        .map_or("", String::as_str),
                )
            })
            .sum();
        (
            applied,
            number(draft.values.get("amount").map_or("", String::as_str)),
        )
    }

    /// The issued invoices of one customer that still owe something, oldest due first.
    async fn open_invoices(&self, scope: &Scope, customer: &str) -> Result<Vec<Document>> {
        let Ok(id) = customer.parse::<i64>() else {
            return Ok(vec![]);
        };
        let mut open: Vec<Document> = self
            .pages
            .read_all::<Document>(
                scope,
                "finance.receivables.document.list",
                json!({"customer_id": id, "kind": "invoice", "status": "issued"}),
                ("after", "next_after"),
            )
            .await?
            .0
            .into_iter()
            .filter(|d| d.outstanding.is_some_and(|m| m.is_positive()))
            .collect();
        open.sort_by(|a, b| (&a.due_date, a.id).cmp(&(&b.due_date, b.id)));
        Ok(open)
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
            "filter" => self.list.apply_filters(&params::form_values(v, "filters")),
            "page" => self.list.put_page(v["page"].as_str()),
            "grid" => {
                if let ColumnsOutcome::Sort(column) = self.columns.handle(&params::string_values(v))
                {
                    self.list.next_sort(&column);
                }
            }
            "show_record" => {
                if self.can_record {
                    let mut values =
                        HashMap::from([("payment_date".to_owned(), self.pages.today(scope).await)]);
                    let mut open = self
                        .series
                        .iter()
                        .filter(|s| s.kind == "payment" && s.active);
                    if let (Some(only), None) = (open.next(), open.next()) {
                        values.insert("series_id".into(), only.id.to_string());
                    }
                    self.draft = Some(Draft {
                        values,
                        open: vec![],
                        errors: Errors::default(),
                    });
                } else {
                    self.flash.set(StatusKind::Error, DENIED);
                }
            }
            "cancel_record" => self.draft = None,
            "receipt_change" => {
                let form = params::form_values(v, "receipt");
                let after = form.get("customer_id").cloned().unwrap_or_default();
                let changed = self.draft.as_ref().is_some_and(|d| {
                    d.values.get("customer_id").cloned().unwrap_or_default() != after
                });
                let open = if changed {
                    Some(self.open_invoices(scope, &after).await?)
                } else {
                    None
                };
                if let Some(draft) = &mut self.draft {
                    draft.values = form;
                    if let Some(open) = open {
                        draft.open = open;
                    }
                }
            }
            "apply_oldest" => {
                if let Some(draft) = &mut self.draft {
                    let mut left = draft
                        .values
                        .get("amount")
                        .and_then(|t| t.trim().parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO);
                    let mut assigned = Vec::new();
                    for d in &draft.open {
                        let owes = d.outstanding.map_or(Decimal::ZERO, |m| m.amount());
                        let take = left.min(owes);
                        assigned.push((
                            d.id,
                            if take > Decimal::ZERO {
                                f::decimal(take)
                            } else {
                                String::new()
                            },
                        ));
                        left -= take.max(Decimal::ZERO);
                    }
                    for (id, text) in assigned {
                        draft.values.insert(format!("alloc_{id}"), text);
                    }
                }
            }
            "save_receipt" => {
                if let Some(draft) = &mut self.draft {
                    let mut input = json!({});
                    let mut local = Errors::default();
                    for key in ["series_id", "customer_id", "deposit_account_id"] {
                        match f::posted(&draft.values, key).parse::<i64>() {
                            Ok(id) if id > 0 => input[key] = json!(id),
                            _ => local
                                .0
                                .entry(key.into())
                                .or_default()
                                .push("can't be blank".into()),
                        }
                    }
                    for key in ["payment_date", "amount"] {
                        let text = f::posted(&draft.values, key);
                        if text.is_empty() {
                            local
                                .0
                                .entry(key.into())
                                .or_default()
                                .push("can't be blank".into());
                        } else {
                            input[key] = json!(text);
                        }
                    }
                    for key in ["reference", "description"] {
                        f::put(&mut input, key, f::posted(&draft.values, key));
                    }
                    let allocations: Vec<_> = draft
                        .open
                        .iter()
                        .filter_map(|d| {
                            let text = f::posted(&draft.values, &format!("alloc_{}", d.id));
                            (!text.is_empty()).then(|| json!({"document_id": d.id, "amount": text}))
                        })
                        .collect();
                    input["allocations"] = json!(allocations);
                    if !local.is_empty() {
                        draft.errors = local;
                    } else {
                        let mut draft = self.draft.take().ok_or(Error::Internal)?;
                        match self
                            .pages
                            .write(
                                scope,
                                credential,
                                "save_receipt",
                                "finance.receivables.payment.record",
                                input,
                            )
                            .await?
                        {
                            Written::Done(record) => {
                                self.flash.set(
                                    StatusKind::Success,
                                    "Receipt recorded and posted to the ledger.",
                                );
                                if let Some(id) = record["id"].as_i64() {
                                    redirect = Some(format!("{RECEIPTS}/{id}"));
                                }
                                self.load(scope).await?;
                            }
                            Written::Invalid(fields) => {
                                draft.errors = Errors(fields);
                                self.draft = Some(draft);
                            }
                            Written::Refused(message) => self.flash.set(
                                StatusKind::Error,
                                super::worded(
                                    message,
                                    "You do not have permission to record receipts.",
                                ),
                            ),
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        let flash = redirect.is_some().then(|| self.flash.0.take()).flatten();
        Ok(LiveBody {
            html: self.body()?,
            query: self.list.to_params(),
            redirect,
            flash,
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Payments {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => self.load(r.scope).await?,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(v) => return self.event(r.scope, r.credential, v).await,
            }
            Ok(LiveBody {
                html: self.body()?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
