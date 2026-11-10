//! The receipts list and its Record receipt dialog. Reads `payment.list`,
//! `customer.list`, `series.list`, `document.list` and the ledger's account
//! list; recording calls `payment.record`. A receipt pays issued invoices of
//! one customer, so the dialog lists that customer's open invoices and asks
//! how much of the receipt goes to each. What goes to no invoice (a deposit
//! or an overpayment) is kept as the customer's credit, and the dialog says
//! how much that is while the amounts are typed.
mod dialog;

use super::{
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written, DENIED, RECEIPTS,
};
use crate::{Customer, Document, Payment, Series, MANAGE_SETUP, POST_PAYMENTS};
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{RecordAction, RecordActions};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BadgeKind, BuiltinColumn, Cell, Choice, ColumnsOutcome, CreateButton, EmptyState,
    Filter, FilterToolbar, FlexRow, FlexTable, Header, Html, ListColumns, ListSpec, ListState,
    PageCounts, PageVariant, Pagination, RecordLink, SortDir, StatusKind, TableEmpty,
    ToolbarControl,
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
        Ok(bilibi_ui::status_badge(BadgeKind::Warning, "Reversed"))
    } else {
        Ok(bilibi_ui::status_badge(BadgeKind::Success, "Posted"))
    }
}

/// The Record receipt dialog while it is open.
struct Draft {
    values: HashMap<String, String>,
    /// The chosen customer's issued invoices that still owe something.
    open: Vec<Document>,
    errors: Errors,
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
    /// Whether the company has chosen where customer credits are held; a
    /// receipt can keep an unapplied amount only once it has.
    keeps_credit: bool,
    /// The person may choose that account, so the dialog offers it in place.
    can_setup: bool,
    figures: f::Figures,
    cut: bool,
    can_record: bool,
    draft: Option<Draft>,
    /// The invoice an address asked to record a receipt for (`?receive=`),
    /// as an invoice's own "Record receipt" sends it here.
    receive: Option<i64>,
    /// The customer an address asked to record a receipt from (`?customer=`),
    /// as a customer's own page sends it here.
    receive_from: Option<i64>,
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
            keeps_credit: false,
            can_setup: false,
            figures: f::Figures::default(),
            cut: false,
            can_record: false,
            draft: None,
            receive: params::positive_id(query.get("receive").map(String::as_str)),
            receive_from: params::positive_id(query.get("customer").map(String::as_str)),
            flash: Flash::default(),
            framed,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.mounted(scope).await?;
        self.body()
    }

    /// Load, then open Record receipt on the invoice the address named.
    pub(super) async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        self.load(scope).await?;
        if let Some(invoice) = self.receive.take() {
            self.start_receipt(scope, Some(invoice)).await?;
        } else if let Some(customer) = self.receive_from.take() {
            self.start_receipt(scope, None).await?;
            let open = self.open_invoices(scope, &customer.to_string()).await?;
            if let Some(draft) = &mut self.draft {
                draft
                    .values
                    .insert("customer_id".into(), customer.to_string());
                draft.open = open;
            }
        }
        Ok(())
    }

    /// Open the Record receipt dialog: today, the only open series, and for
    /// an invoice, its customer with what it still owes applied to it.
    async fn start_receipt(&mut self, scope: &Scope, invoice: Option<i64>) -> Result<()> {
        if !self.can_record {
            self.flash.set(StatusKind::Error, DENIED);
            return Ok(());
        }
        let mut values =
            HashMap::from([("payment_date".to_owned(), self.pages.today(scope).await)]);
        let mut open = self
            .series
            .iter()
            .filter(|s| s.kind == "payment" && s.active);
        if let (Some(only), None) = (open.next(), open.next()) {
            values.insert("series_id".into(), only.id.to_string());
        }
        let mut owed = vec![];
        if let Some(id) = invoice {
            let invoice: Option<Document> = self
                .pages
                .read(
                    scope,
                    "finance.receivables.document.get",
                    json!({"document_id": id}),
                )
                .await
                .ok()
                .and_then(|v| serde_json::from_value(v).ok());
            match invoice.filter(|d| d.outstanding.is_some_and(|m| m.is_positive())) {
                Some(d) => {
                    let customer = d.customer_id.to_string();
                    owed = self.open_invoices(scope, &customer).await?;
                    let due = d
                        .outstanding
                        .map(|m| self.figures.shown(m))
                        .unwrap_or_default();
                    values.insert("customer_id".into(), customer);
                    values.insert("amount".into(), due.clone());
                    values.insert(format!("alloc_{}", d.id), due);
                }
                None => self
                    .flash
                    .set(StatusKind::Info, "That invoice owes nothing now."),
            }
        }
        self.draft = Some(Draft {
            values,
            open: owed,
            errors: Errors::default(),
        });
        Ok(())
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
        let setup = self
            .pages
            .read(scope, "finance.receivables.setup.get", json!({}))
            .await
            .ok();
        self.control = setup
            .as_ref()
            .and_then(|v| v["setup"]["receivable_account_id"].as_i64());
        self.keeps_credit = setup
            .as_ref()
            .is_some_and(|v| v["setup"]["customer_credit_account_id"].as_i64().is_some());
        self.figures = self.pages.figures(scope).await;
        self.can_record = self.pages.allowed(scope, POST_PAYMENTS).await?;
        self.can_setup = self.pages.allowed(scope, MANAGE_SETUP).await?;
        Ok(())
    }

    fn customer_name(&self, id: i64) -> String {
        self.customers
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| format!("Customer #{id}"), |c| c.name.clone())
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
                    || self.customers.iter().any(|c| {
                        c.id == p.customer_id
                            && (c.name.to_lowercase().contains(&search)
                                || c.code.to_lowercase().contains(&search))
                    })
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
                .map(|c| Choice::new(&c.name, c.id.to_string())),
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
            let mut items = Vec::new();
            if self.can_record && p.reversal.is_none() {
                items.push(RecordAction::here(
                    "arrow-uturn-left",
                    "Reverse receipt",
                    &format!("{RECEIPTS}/{}", p.id),
                    "reverse",
                ));
            }
            row.actions = RecordActions::row(
                &format!("receivables-receipt-{}-actions", p.id),
                &p.payment_reference,
                items,
            )?;
            rows.push(
                row.slot(
                    "number",
                    render(RecordLink {
                        workspace: framed,
                        kind: crate::MODULE,
                        record_id: &p.id.to_string(),
                        href: &format!("{RECEIPTS}/{}", p.id),
                        class: "font-medium whitespace-nowrap text-ink-strong hover:underline",
                        body: Html::text(&p.payment_reference),
                    })?,
                )
                .cell("customer", Cell::text(self.customer_name(p.customer_id)))
                .cell(
                    "payment_date",
                    Cell::text(bilibi_finance_ledger::dates::display_text(&p.payment_date)),
                )
                .slot("amount", f::tabular(&self.figures.money(p.amount))?)
                .slot(
                    "deposit",
                    super::account_html(&self.accounts, p.deposit_account_id)?,
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
        table.actions = self.can_record;
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        let mut empty = if self.all.is_empty() {
            EmptyState::missing(
                "No receipts yet",
                "Record money a customer paid. It pays their invoices, and anything left over is kept as their credit.",
            )
        } else {
            EmptyState::missing(
                "No receipts match",
                "Change the filters to see more receipts.",
            )
        };
        if self.all.is_empty() && self.can_record {
            empty.action = render(CreateButton {
                noun: "Receipt",
                id: "receivables-receipts-empty-add",
                event: "show_record",
                ..CreateButton::default()
            })?;
        }
        table.empty = Some(TableEmpty::Content(render(empty)?));
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
            "show_record" => self.start_receipt(scope, None).await?,
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
                    // The customer credit account chosen in this dialog, when
                    // the receipt keeps an amount and the company has none yet.
                    let (applied, received) = Self::totals(draft);
                    let mut credit_account = None;
                    if received > applied && !self.keeps_credit && self.can_setup {
                        match f::posted(&draft.values, "credit_account_id").parse::<i64>() {
                            Ok(id) if id > 0 => credit_account = Some(id),
                            _ => local
                                .0
                                .entry("customer_credit_account_id".into())
                                .or_default()
                                .push("can't be blank".into()),
                        }
                    }
                    if !local.is_empty() {
                        draft.errors = local;
                    } else {
                        let mut draft = self.draft.take().ok_or(Error::Internal)?;
                        if let Some(id) = credit_account {
                            match self
                                .pages
                                .write(
                                    scope,
                                    credential,
                                    "save_receipt",
                                    "finance.receivables.setup.configure",
                                    json!({"customer_credit_account_id": id}),
                                )
                                .await?
                            {
                                Written::Done(_) => self.keeps_credit = true,
                                Written::Invalid(fields) => {
                                    draft.errors = Errors(fields);
                                    self.draft = Some(draft);
                                    return self.answer(None);
                                }
                                Written::Refused(message) => {
                                    self.flash.set(
                                        StatusKind::Error,
                                        super::worded(
                                            message,
                                            "You do not have permission to choose the customer credit account.",
                                        ),
                                    );
                                    self.draft = Some(draft);
                                    return self.answer(None);
                                }
                            }
                        }
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
                                    if record["credit"].is_null() {
                                        "Receipt recorded and posted to the ledger."
                                    } else {
                                        "Receipt recorded and posted to the ledger. The amount not applied to an invoice is kept as the customer's credit."
                                    },
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
        self.answer(redirect)
    }

    /// The page after an event, carrying its flash across a redirect.
    fn answer(&mut self, redirect: Option<String>) -> Result<LiveBody> {
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
