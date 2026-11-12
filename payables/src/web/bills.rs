//! Bilimbi Payables.Web.BillsLive: filters, paging and built-in ListColumns.
use super::opening::Opening;
use super::{bill_form::BillForm, company, format, render, Pages, INDEX, NEW_PAYMENT};
use crate::{DocumentPageRow, APPROVE_DOCUMENTS, CREATE_DOCUMENTS, SUBMIT_PAYMENTS, VIEW};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{
    MinorUnits, Money, Owed, OwedFigure, OwedSummary, RecordAction, RecordActions, Tally,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BadgeKind, BuiltinColumn, Button, Cell, Choice, CreateButton, EmptyState, Filter,
    FilterToolbar, FlashGroup, FlexRow, FlexTable, Html, ListColumns, ListSpec, ListState,
    PageCounts, Pagination, RecordLink, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: SIZES.to_vec(),
        filters: [
            (
                "source".into(),
                Filter::one_of(&["", "ordinary", "opening"], ""),
            ),
            (
                "status".into(),
                Filter::one_of(
                    &[
                        "", "draft", "approved", "held", "unpaid", "overdue", "paid", "posted",
                        "voided",
                    ],
                    "",
                ),
            ),
            (
                "document_type".into(),
                Filter::one_of(&["", "bill", "debit_note", "opening_credit"], ""),
            ),
        ]
        .into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("bills list spec")
});

pub(super) struct Bills {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    documents: Vec<DocumentPageRow>,
    opening: Option<Opening>,
    /// The New bill / debit note / correction dialog while it is open.
    record: Option<Box<BillForm>>,
    /// What an address asked the dialog to open on (`?record=bill`,
    /// `?correct=<draft>`, `?debit_note=<bill>`), opened once the list loads.
    requested: Option<(bool, HashMap<String, String>)>,
    /// The address asked for the opening bills dialog (`?opening=1`, the
    /// opening balances page's step), opened once the page knows it may.
    open_opening: bool,
    can_open: bool,
    success: bool,
    total: u64,
    units: Option<MinorUnits>,
    /// The three figures above the list; absent when they cannot be read.
    summary: Option<OwedSummary>,
    currency: String,
    can_pay: bool,
    can_create: bool,
    can_approve: bool,
    can_match: bool,
    unavailable: bool,
    framed: bool,
}

impl Bills {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = [
            ("number", "Number"),
            ("document_type", "Type"),
            ("party", "Supplier"),
            ("party_document_number", "Supplier's invoice number"),
            ("document_date", "Date"),
            ("due_date", "Due"),
            ("total", "Total"),
            ("outstanding", "Owed"),
            ("status", "Status"),
            ("match_status", "Match"),
        ]
        .into_iter()
        .map(|(id, label)| {
            let c = BuiltinColumn::new(id, label);
            if id == "total" || id == "outstanding" {
                c.right()
            } else {
                c
            }
        })
        .collect();
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("payables-bills-table", columns)
                .map_err(|_| Error::Internal)?,
            documents: vec![],
            opening: None,
            record: None,
            requested: requested(query),
            open_opening: query.get("opening").map(String::as_str) == Some("1"),
            can_open: false,
            success: false,
            total: 0,
            units: None,
            summary: None,
            currency: String::new(),
            can_pay: false,
            can_create: false,
            can_approve: false,
            can_match: false,
            unavailable: false,
            framed,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.can_create = self.pages.allowed(scope, CREATE_DOCUMENTS).await?;
        self.can_approve = self.pages.allowed(scope, APPROVE_DOCUMENTS).await?;
        self.can_match = self.pages.allowed(scope, crate::APPROVE_MATCH).await?;
        self.can_open = self.pages.allowed(scope, VIEW).await?;
        self.can_pay = self.pages.allowed(scope, SUBMIT_PAYMENTS).await?;
        if std::mem::take(&mut self.open_opening) && self.can_open {
            self.opening = Some(Opening::new(self.pages.clone())?);
        }
        self.documents.clear();
        self.total = 0;
        self.unavailable = true;
        self.summary = None;
        if let Some(company) = company(scope) {
            let today = self.pages.today(scope).await?.to_string();
            let mut input = json!({"company_id":company,"search":self.list.search,"page":self.list.page,"page_size":self.list.page_size,"as_of":today});
            for key in ["document_type", "status", "source"] {
                if let Some(value) = self.list.filters.get(key).filter(|value| !value.is_empty()) {
                    input[key] = json!(value);
                }
            }
            if let Ok(answer) = self
                .pages
                .contracts
                .read(scope, "finance.payables.document.page", &input)
                .await
            {
                let page = &answer["result"];
                self.documents = serde_json::from_value(page["documents"].clone())
                    .map_err(|_| Error::Internal)?;
                self.total = page["total"].as_u64().ok_or(Error::Internal)?;
                self.list.page = page["page"].as_u64().ok_or(Error::Internal)?;
                self.units = self.pages.units(scope).await;
                self.unavailable = false;
                if let Some(first) = self.documents.first() {
                    self.currency = first.currency_code.clone();
                }
                self.summary = self
                    .pages
                    .contracts
                    .read(
                        scope,
                        "finance.payables.document.summary",
                        &json!({"company_id":company,"as_of":today}),
                    )
                    .await
                    .ok()
                    .and_then(|answer| serde_json::from_value(answer["result"].clone()).ok());
            }
        }
        self.columns.load(&self.documents, |d| d.id.to_string());
        Ok(())
    }

    pub(super) async fn body(&mut self, scope: &Scope) -> Result<String> {
        // Header actions: the rarer one first, the one primary verb last.
        // Held bills are the Status filter's "Held", not a link of their own.
        let mut actions = Html::EMPTY;
        if self.can_open {
            actions.push(&render(Button {
                id: "payables-opening",
                event: "open_opening",
                button_type: "button",
                body: Html::text("Opening bills"),
                ..Button::default()
            })?);
        }
        if self.can_create {
            actions.push(&render(CreateButton {
                noun: "Bill",
                event: "show_record",
                id: "payables-bills-new",
                ..CreateButton::default()
            })?);
        }
        let mut content = Html::EMPTY;
        if self.success {
            let mut flash = bilibi_ui::NO_FLASH;
            flash.success =
                "The opening bills were reconciled. Open a bill below to read its source evidence.";
            content.push(&render(FlashGroup {
                id: "payables-bills-flash",
                flash,
            })?);
        }
        let mut toolbar = FilterToolbar::new(
            "payables-bills-filters",
            vec![
                ToolbarControl::search(
                    "payables-bills-search",
                    "search",
                    "Search bills",
                    &self.list.search,
                    "Search by supplier or invoice number…",
                ),
                ToolbarControl::select(
                    "payables-bills-source",
                    "source",
                    "Filter by source",
                    &self.list.filters["source"],
                    [
                        ("All sources", ""),
                        ("Ordinary", "ordinary"),
                        ("Opening", "opening"),
                    ]
                    .map(|(label, value)| Choice::new(label, value))
                    .to_vec(),
                ),
                ToolbarControl::select(
                    "payables-bills-status",
                    "status",
                    "Filter by status",
                    &self.list.filters["status"],
                    [
                        ("All statuses", ""),
                        ("Draft", "draft"),
                        ("Approved", "approved"),
                        ("Held", "held"),
                        ("Awaiting payment", "unpaid"),
                        ("Overdue", "overdue"),
                        ("Paid", "paid"),
                        ("Posted", "posted"),
                        ("Voided", "voided"),
                    ]
                    .map(|(l, v)| Choice::new(l, v))
                    .to_vec(),
                ),
                ToolbarControl::select(
                    "payables-bills-type",
                    "document_type",
                    "Filter by type",
                    &self.list.filters["document_type"],
                    [
                        ("All types", ""),
                        ("Bill", "bill"),
                        ("Debit note", "debit_note"),
                        ("Supplier credit", "opening_credit"),
                    ]
                    .map(|(l, v)| Choice::new(l, v))
                    .to_vec(),
                ),
            ],
        );
        toolbar.event = "filter";
        if !self.unavailable {
            content.push(&self.strips()?);
        }
        content.push(&render(toolbar)?);
        if self.unavailable {
            let mut empty = EmptyState::missing(
                "Bills unavailable",
                "This company's bills cannot be read with your current access.",
            );
            empty.id = "payables-bills-unavailable".into();
            content.push(&render(empty)?);
        } else {
            let mut rows = Vec::new();
            for d in &self.documents {
                let mut row = FlexRow::new(d.id.to_string());
                row.dom_id = Some(format!("payables-bill-{}", d.id));
                // The supplier's own invoice number is what a person looks a
                // bill up by, so it is the link; ours comes only on posting.
                let kind = format::kind(&d.document_type);
                row = row
                    .cell("number", Cell::text(format::number(d.document_number)))
                    .slot(
                        "document_type",
                        format::badge(
                            &if d.opening && d.document_type == "bill" {
                                format!("{kind} · Opening")
                            } else {
                                kind.to_owned()
                            },
                            BadgeKind::Neutral,
                            false,
                        )?,
                    )
                    .cell("party", Cell::text(&d.party_name))
                    .slot(
                        "party_document_number",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &d.id.to_string(),
                            href: &format!("{INDEX}/{}", d.id),
                            class: "font-medium whitespace-nowrap text-ink-strong hover:underline",
                            // A bill not posted yet has no number of ours,
                            // so the supplier's invoice number is its link.
                            body: Html::text(
                                d.party_document_number.as_deref().unwrap_or("No number"),
                            ),
                        })?,
                    )
                    .cell(
                        "document_date",
                        Cell::text(bilibi_finance_ledger::dates::display_text(&d.document_date)),
                    )
                    .cell(
                        "due_date",
                        Cell::text(if d.document_type == "opening_credit" {
                            "—".to_owned()
                        } else {
                            bilibi_finance_ledger::dates::display_text(&d.due_date)
                        }),
                    )
                    .slot(
                        "total",
                        render(Tabular {
                            text: format!(
                                "{} {}",
                                Money::display_decimal(d.total, self.units),
                                d.currency_code
                            ),
                        })?,
                    )
                    .slot(
                        "outstanding",
                        render(Tabular {
                            text: d.outstanding.map_or_else(
                                || "—".into(),
                                |owed| Money::display_decimal(owed, self.units),
                            ),
                        })?,
                    )
                    .slot(
                        "status",
                        match Owed::read(d.payment_status, d.days_overdue) {
                            Some(owed) => owed.badges(),
                            None if d.replaced && d.status == "posted" => {
                                format::badge("Replaced", BadgeKind::Neutral, true)?
                            }
                            None => format::status(&d.status)?,
                        },
                    )
                    .slot(
                        "match_status",
                        match &d.match_status {
                            Some(s) => format::match_badge(s, "")?,
                            None => Html::text("—"),
                        },
                    );
                row.actions = self.row_actions(d)?;
                rows.push(row);
            }
            let mut table =
                FlexTable::new("payables-bills-table", self.columns.flex_columns(), rows);
            table.actions = self.can_create || self.can_approve || self.can_match || self.can_pay;
            table.event = "bills_grid";
            table.caption = "Bills";
            table.framed = false;
            table.zoom = self.columns.zoom;
            table.suggestions = self.columns.flex_suggestions();
            table.add_query = &self.columns.add_query;
            table.empty = Some(TableEmpty::Content(Html::text(if self.can_create {
                "No documents match the current filters. Record a bill to start."
            } else {
                "No documents match the current filters."
            })));
            content.push(&render(table)?);
            let mut pager = Pagination::new(
                "payables-bills-pagination",
                PageCounts::new(self.list.page, self.list.page_size, self.total),
                &SIZES,
            );
            pager.filters_event = "filter";
            content.push(&render(pager)?);
        }
        if let Some(opening) = &mut self.opening {
            content.push(&opening.body(scope).await?);
        }
        if let Some(record) = &self.record {
            content.push(&record.dialog()?);
        }
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
            content,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "bills.html")]
struct View {
    title_actions: Html,
    actions: Html,
    content: Html,
}

impl Bills {
    /// Awaiting payment, overdue and what is not posted yet above the list;
    /// the first two link to the list narrowed to them. Nothing while there
    /// are no bills.
    fn strips(&self) -> Result<Html> {
        let Some(s) = self.summary.as_ref().filter(|s| {
            s.not_posted.count > 0 || s.awaiting_payment.count > 0 || s.overdue.count > 0
        }) else {
            return Ok(Html::EMPTY);
        };
        let amount = |tally: &Tally| {
            format!(
                "{} {}",
                Money::display_decimal(tally.amount, self.units),
                self.currency
            )
            .trim_end()
            .to_owned()
        };
        let count = |tally: &Tally| OwedSummary::counted(tally.count, "bill", "bills");
        OwedSummary::strips(
            "payables-bills-summary",
            vec![
                OwedFigure {
                    title: "Awaiting payment",
                    detail: count(&s.awaiting_payment),
                    amount: amount(&s.awaiting_payment),
                    href: format!("{INDEX}?status=unpaid"),
                },
                OwedFigure {
                    title: "Overdue",
                    detail: s.overdue_detail("bill", "bills"),
                    amount: amount(&s.overdue),
                    href: format!("{INDEX}?status=overdue"),
                },
                OwedFigure {
                    title: "Not posted yet",
                    detail: count(&s.not_posted),
                    amount: amount(&s.not_posted),
                    href: String::new(),
                },
            ],
        )
    }

    /// A row's next step, local icons and menu: what its bill page offers for
    /// its state. Request payment is the named next step on a bill still owed.
    fn row_actions(&self, d: &DocumentPageRow) -> Result<Html> {
        let here = format!("{INDEX}/{}", d.id);
        let mut items = Vec::new();
        if d.document_type == "bill"
            && d.status == "posted"
            && self.can_pay
            && d.outstanding
                .is_some_and(|owed| owed > rust_decimal::Decimal::ZERO)
        {
            items.push(RecordAction::next(
                "Request payment",
                format!("{NEW_PAYMENT}?bill={}", d.id),
            ));
        }
        if d.document_type == "bill" && d.status == "posted" && self.can_create {
            items.push(RecordAction::link(
                "Record debit note",
                format!("{INDEX}?debit_note={}", d.id),
            ));
        }
        if self.can_match
            && matches!(d.status.as_str(), "approved" | "held")
            && d.match_status.as_deref() == Some("held")
        {
            items.push(RecordAction::here(
                "check-badge",
                "Approve exceptions",
                &here,
                "approve_exception",
            ));
        }
        if self.can_approve && matches!(d.status.as_str(), "draft" | "approved" | "held") {
            items.push(RecordAction::here("no-symbol", "Void", &here, "void"));
        }
        RecordActions::row(
            &format!("payables-bill-{}-actions", d.id),
            &format!(
                "{} {}",
                format::kind(&d.document_type),
                format::number(d.document_number)
            ),
            items,
        )
    }
}

impl Bills {
    /// Opens the bill dialog: a new bill, or what an address asked for.
    pub(super) async fn open_record(
        &mut self,
        scope: &Scope,
        note: bool,
        query: HashMap<String, String>,
    ) -> Result<()> {
        if !self.pages.allowed(scope, CREATE_DOCUMENTS).await? {
            return Ok(());
        }
        let mut form = BillForm::new(self.pages.clone(), note, &query)?;
        form.reload(scope).await?;
        self.record = Some(Box::new(form));
        Ok(())
    }

    /// Opens the dialog an address asked for, once.
    pub(super) async fn open_requested(&mut self, scope: &Scope) -> Result<()> {
        if let Some((note, query)) = self.requested.take() {
            self.open_record(scope, note, query).await?;
        }
        Ok(())
    }
}

/// `?record=bill` (a new bill), `?correct=<draft>` or `?debit_note=<bill>`:
/// the dialog an address opens, as `BillForm::new` reads it.
fn requested(query: &HashMap<String, String>) -> Option<(bool, HashMap<String, String>)> {
    let id = |key: &str| params::positive_id(query.get(key).map(String::as_str));
    if let Some(bill) = id("debit_note") {
        return Some((true, HashMap::from([("bill".into(), bill.to_string())])));
    }
    if let Some(draft) = id("correct") {
        return Some((
            false,
            HashMap::from([("correct".into(), draft.to_string())]),
        ));
    }
    (query.get("record").map(String::as_str) == Some("bill")).then(|| (false, HashMap::new()))
}

impl LiveState<Scope, Error> for Bills {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                let kind = value["type"].as_str().unwrap_or("");
                if kind == "show_record" {
                    self.open_record(r.scope, false, HashMap::new()).await?;
                } else if kind == "cancel_record" {
                    self.record = None;
                } else if matches!(kind, "edit" | "add_line" | "remove_line" | "save") {
                    let Some(record) = self.record.as_mut() else {
                        return Err(Error::Invalid);
                    };
                    if let Some(to) = record.act(r.scope, r.credential, value).await? {
                        let message = record.saved();
                        self.record = None;
                        return Ok(LiveBody {
                            redirect: Some(to),
                            flash: Some((StatusKind::Success, message)),
                            ..LiveBody::default()
                        });
                    }
                    return Ok(LiveBody {
                        html: self.body(r.scope).await?,
                        query: self.list.to_params(),
                        ..LiveBody::default()
                    });
                }
                match kind {
                    "open_opening" if self.can_open => {
                        self.opening = Some(Opening::new(self.pages.clone())?)
                    }
                    "close_opening" => self.opening = None,
                    "clear-flash" if self.opening.is_none() => {
                        if value["key"].as_str() == Some(StatusKind::Success.name()) {
                            self.success = false;
                        }
                    }
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
                            self.success = true;
                            self.list.search.clear();
                            self.list.apply_filters(&HashMap::from([
                                ("source".into(), "opening".into()),
                                ("status".into(), "".into()),
                                ("document_type".into(), "".into()),
                            ]));
                            self.reload(r.scope).await?;
                        }
                        return Ok(LiveBody {
                            html: self.body(r.scope).await?,
                            query: self.list.to_params(),
                            ..LiveBody::default()
                        });
                    }
                    _ => {}
                }
            }
            let reload = match r.input {
                LiveInput::Refresh => true,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    true
                }
                LiveInput::Event(v) => match v["type"].as_str() {
                    Some("filter") => {
                        self.list.apply_filters(&params::form_values(v, "filters"));
                        true
                    }
                    Some("page") => {
                        self.list.put_page(v["page"].as_str());
                        true
                    }
                    Some("bills_grid") => {
                        self.columns.handle(&params::string_values(v));
                        false
                    }
                    Some(
                        "open_opening" | "close_opening" | "clear-flash" | "show_record"
                        | "cancel_record",
                    ) => false,
                    _ => return Err(Error::Invalid),
                },
            };
            if reload {
                self.reload(r.scope).await?;
            }
            self.open_requested(r.scope).await?;
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>"#,
    ext = "html"
)]
struct Tabular {
    text: String,
}
