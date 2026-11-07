//! Bilimbi `Web.DocumentsLive`: URL-backed filters and paging, fixed newest-first order.
use super::*;
use crate::{DocumentPage, DocumentQuery};
use bilibi_ui::{
    params, BuiltinColumn, Choice, EmptyState, Filter, FilterToolbar, ListSpec, ListState,
    PageCounts, Pagination, RecordLink, TableEmpty, ToolbarControl,
};
use std::{collections::HashMap, sync::LazyLock};

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        filters: [
            (
                "status".into(),
                Filter::one_of(
                    &[
                        "",
                        "issued",
                        "submitted",
                        "refused",
                        "valid",
                        "invalid",
                        "cancelled",
                        "rejected",
                        "consolidated",
                        "released",
                    ],
                    "",
                ),
            ),
            (
                "document_type".into(),
                Filter::one_of(&["", "01", "02", "03", "04", "11", "12", "13", "14"], ""),
            ),
            (
                "environment".into(),
                Filter::one_of(&["", "sandbox", "production"], ""),
            ),
        ]
        .into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("e-invoice list spec")
});

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    documents: Option<DocumentPage>,
    framed: bool,
}
#[derive(Template)]
#[template(path = "documents.html")]
struct View {
    title_actions: Html,
    actions: Html,
    toolbar: Html,
    table: Html,
    pagination: Html,
    unavailable: bool,
}

/// The header's buttons to the two other document views.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::button(id="einvoice-documents-consolidation", href="/finance/einvoice/consolidation") %}Consolidation{% endcall %}{% call ui::button(id="einvoice-documents-reconciliation", href="/finance/einvoice/reconciliation") %}Reconciliation{% endcall %}"#,
    ext = "html"
)]
struct HeaderLinks;

impl Index {
    pub fn new(pages: &Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = [
            ("code_number", "Number"),
            ("document_type", "Type"),
            ("issued_at", "Issued"),
            ("buyer", "Counterparty"),
            ("payable_amount", "Payable"),
            ("status", "Status"),
        ]
        .into_iter()
        .map(|(key, name)| {
            let c = BuiltinColumn::new(key, name);
            if key == "payable_amount" {
                c.right()
            } else {
                c
            }
        })
        .collect();
        Ok(Self {
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            // Every row of a company is in the same environment; the column
            // waits in the picker.
            columns: ListColumns::mount("einvoice-documents", columns)
                .map_err(|_| Error::Internal)?
                .with_record_fields([bilibi_ui::RecordField::new("environment", "Environment")]),
            documents: None,
            framed,
        })
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let query = DocumentQuery {
            search: self.list.search.clone(),
            status: self.list.filters["status"].clone(),
            document_type: self.list.filters["document_type"].clone(),
            environment: self.list.filters["environment"].clone(),
            page: self.list.page,
            page_size: self.list.page_size,
        };
        self.documents = match self
            .pages
            .einvoice
            .list_documents(scope, company(scope)?, &query)
            .await
        {
            Ok(page) => {
                self.list.clamp_to_last_page(page.total_pages, true);
                Some(page)
            }
            Err(EinvoiceError::Failed(error)) => return Err(error),
            Err(_) => None,
        };
        self.columns.load(
            self.documents
                .as_ref()
                .map_or(&[], |p| p.entries.as_slice()),
            |d| d.id.to_string(),
        );
        Ok(())
    }
    async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let mut statuses = vec![Choice::new("All statuses", "")];
        statuses.extend(
            crate::documents::STATUS_EVENTS
                .iter()
                .map(|s| Choice::new(*s, *s)),
        );
        let mut types = vec![Choice::new("All types", "")];
        types.extend(
            crate::Einvoice::authority()
                .issuable_document_types()
                .iter()
                .map(|s| Choice::new(type_label(s), *s)),
        );
        let mut toolbar = FilterToolbar::new(
            "einvoice-documents-filters",
            vec![
                ToolbarControl::search(
                    "einvoice-documents-search",
                    "search",
                    "Search documents",
                    &self.list.search,
                    "Search by number or counterparty…",
                ),
                ToolbarControl::select(
                    "einvoice-documents-status",
                    "status",
                    "Filter by status",
                    &self.list.filters["status"],
                    statuses,
                ),
                ToolbarControl::select(
                    "einvoice-documents-type",
                    "document_type",
                    "Filter by type",
                    &self.list.filters["document_type"],
                    types,
                ),
                ToolbarControl::select(
                    "einvoice-documents-environment",
                    "environment",
                    "Filter by environment",
                    &self.list.filters["environment"],
                    vec![
                        Choice::new("All environments", ""),
                        Choice::new("Sandbox", "sandbox"),
                        Choice::new("Production", "production"),
                    ],
                ),
            ],
        );
        toolbar.event = "filter";
        let mut rows = Vec::new();
        if let Some(page) = &self.documents {
            for d in &page.entries {
                let key = d.id.to_string();
                let address = format!("{ADDRESS}/{}", d.id);
                let mut row = FlexRow::new(&key);
                row.dom_id = Some(format!("einvoice-document-{}", d.id));
                rows.push(
                    row.slot(
                        "code_number",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: "finance/einvoice",
                            record_id: &key,
                            href: &address,
                            body: Html::text(&d.code_number),
                            class: "font-medium text-ink-strong hover:underline",
                        })?,
                    )
                    .slot(
                        "document_type",
                        render(Badge {
                            kind: BadgeKind::Neutral,
                            dot: false,
                            class: "",
                            body: Html::text(&type_label(&d.document_type)),
                        })?,
                    )
                    .cell(
                        "issued_at",
                        time_cell(&display, d.issued_at, Precision::Minute),
                    )
                    .cell(
                        "buyer",
                        Cell::text(d.counterparty_name.as_deref().unwrap_or("")),
                    )
                    .cell(
                        "payable_amount",
                        Cell::text(format!("{} {}", d.payable_amount, d.currency_code)),
                    )
                    .slot("status", status_badge(&d.status)?)
                    .cell("environment", Cell::text(sentence(&d.environment))),
                );
            }
        }
        let mut grid = FlexTable::new(
            "einvoice-documents-table",
            self.columns.flex_columns(),
            rows,
        );
        grid.event = "grid";
        grid.framed = false;
        grid.caption = "E-invoices";
        grid.zoom = self.columns.zoom;
        grid.suggestions = self.columns.flex_suggestions();
        grid.add_query = &self.columns.add_query;
        grid.empty = Some(TableEmpty::Content(Html::text(
            "No documents match the current filters.",
        )));
        let pagination = if let Some(page) = &self.documents {
            let mut p = Pagination::new(
                "einvoice-documents-pagination",
                PageCounts::new(self.list.page, self.list.page_size, page.total_entries),
                &[25, 50, 100, 300],
            );
            p.filters_event = "filter";
            render(p)?
        } else {
            Html::EMPTY
        };
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            actions: render(HeaderLinks)?,
            toolbar: render(toolbar)?,
            table: render(grid)?,
            pagination,
            unavailable: self.documents.is_none(),
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match request.input {
                LiveInput::Refresh => true,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    true
                }
                LiveInput::Event(value) => {
                    match value.get("type").and_then(serde_json::Value::as_str) {
                        Some("filter") => {
                            self.list
                                .apply_filters(&params::form_values(value, "filters"));
                            true
                        }
                        Some("page") => {
                            let p = params::string_values(value);
                            self.list.put_page(p.get("page").map(String::as_str));
                            true
                        }
                        Some("grid") => {
                            self.columns.handle(&params::string_values(value));
                            false
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            };
            if reload {
                self.load(request.scope).await?;
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                query: self.list.to_params(),
                ..Default::default()
            })
        })
    }
}
