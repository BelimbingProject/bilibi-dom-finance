//! Bilimbi Payables.Web.BillsLive: filters, paging and built-in ListColumns.
use super::opening::Opening;
use super::{company, format, render, Pages, INDEX, NEW_BILL, NEW_NOTE};
use crate::{DocumentPageRow, APPROVE_DOCUMENTS, CREATE_DOCUMENTS, VIEW};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money, RecordAction, RecordActions};
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
                Filter::one_of(&["", "draft", "approved", "held", "posted", "voided"], ""),
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
    can_open: bool,
    success: bool,
    total: u64,
    units: Option<MinorUnits>,
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
            ("total", "Total"),
            ("source", "Source"),
            ("status", "Status"),
            ("match_status", "Match"),
        ]
        .into_iter()
        .map(|(id, label)| {
            let c = BuiltinColumn::new(id, label);
            if id == "total" {
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
            can_open: false,
            success: false,
            total: 0,
            units: None,
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
        self.documents.clear();
        self.total = 0;
        self.unavailable = true;
        if let Some(company) = company(scope) {
            let mut input = json!({"company_id":company,"search":self.list.search,"page":self.list.page,"page_size":self.list.page_size});
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
                href: NEW_BILL,
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
                row = row
                    .slot(
                        "number",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &d.id.to_string(),
                            href: &format!("{INDEX}/{}", d.id),
                            class: "font-medium whitespace-nowrap text-ink-strong hover:underline",
                            body: Html::text(
                                &d.document_number
                                    .map_or_else(|| "Not posted".to_owned(), |n| format!("#{n}")),
                            ),
                        })?,
                    )
                    .slot(
                        "document_type",
                        format::badge(format::kind(&d.document_type), BadgeKind::Neutral, false)?,
                    )
                    .cell("party", Cell::text(&d.party_name))
                    .cell(
                        "party_document_number",
                        Cell::text(format::optional(&d.party_document_number)),
                    )
                    .cell("document_date", Cell::text(d.document_date.to_string()))
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
                    .cell(
                        "source",
                        Cell::text(if d.opening { "Opening" } else { "Ordinary" }),
                    )
                    .slot("status", format::status(&d.status)?)
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
            table.actions = self.can_create || self.can_approve || self.can_match;
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
    /// A row's one Actions… menu: what its bill page offers for its state.
    fn row_actions(&self, d: &DocumentPageRow) -> Result<Html> {
        let here = format!("{INDEX}/{}", d.id);
        let mut items = Vec::new();
        if d.document_type == "bill" && d.status == "posted" && self.can_create {
            items.push(RecordAction::link(
                "Record debit note",
                format!("{NEW_NOTE}?bill={}", d.id),
            ));
        }
        if self.can_match
            && matches!(d.status.as_str(), "approved" | "held")
            && d.match_status.as_deref() == Some("held")
        {
            items.push(RecordAction::here(
                "Approve exceptions",
                &here,
                "approve_exception",
            ));
        }
        if self.can_approve && matches!(d.status.as_str(), "draft" | "approved" | "held") {
            items.push(RecordAction::here("Void", &here, "void"));
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

impl LiveState<Scope, Error> for Bills {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match value["type"].as_str().unwrap_or("") {
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
                    Some("open_opening" | "close_opening" | "clear-flash") => false,
                    _ => return Err(Error::Invalid),
                },
            };
            if reload {
                self.reload(r.scope).await?;
            }
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
