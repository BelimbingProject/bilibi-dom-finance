//! Bilimbi Finance.Einvoice.Web DocumentsLive, DocumentLive and PdfController.
pub(crate) mod control;
mod detail;
mod index;
mod messages;
mod new_document;

use crate::{contributions::provenance, Authority, Einvoice, EinvoiceError, MODULE};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_module_registry::contributions::{
    Access, Contribution, Download as DownloadRoute, Method, Route, DOWNLOADS, ROUTES,
};
use bilibi_module_registry::Provenance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::*, page::*, Badge, BadgeKind, Cell, FlexRow, FlexTable, Html, Instant, ListColumns,
};
use std::sync::Arc;

pub(crate) const PAGES: &str = "finance/einvoice.pages";
pub(crate) const FILES: &str = "finance/einvoice.files";
pub(super) const ADDRESS: &str = "/finance/einvoice/documents";

// Menu group (a bookkeeper's section of the Finance menu); its pages sit under it.
bilibi_menu::contribute!(
    "finance/einvoice",
    bilibi_menu::MenuItem::container("finance.group.einvoicing", "E-invoicing")
        .under("finance")
        .icon("vaadin-invoice")
        .order(60)
);

bilibi_menu::contribute!(
    "finance/einvoice",
    bilibi_menu::MenuItem::page(
        "finance.einvoice.documents",
        "E-invoices",
        "/finance/einvoice/documents"
    )
    .company_scoped()
    .icon("vaadin-invoice")
    .under("finance.group.einvoicing")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(1)
);

inventory::submit! { Contribution::new(ROUTES, "finance/einvoice.documents", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: ADDRESS, service: PAGES, operation: "index",
    access: Access::Capability(crate::VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "finance/einvoice.document", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/einvoice/documents/{id}", service: PAGES, operation: "detail",
    access: Access::Capability(crate::VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "finance/einvoice.pdf", provenance!(), &DownloadRoute {
    path: "/finance/einvoice/documents/{id}/pdf/{artifact_id}", service: FILES, operation: "pdf",
    access: Access::Capability(crate::VIEW),
}) }

/// A refused submission as the pages word it, for the contract that an
/// agent calls too.
pub(crate) fn submit_refusal(error: EinvoiceError) -> String {
    messages::refusal("submit", error)
}

#[derive(Clone)]
pub(crate) struct Pages {
    pub einvoice: Arc<Einvoice>,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
}

impl Pages {
    /// One read contract for the current company; the answer is its `result`.
    pub(crate) async fn read(
        &self,
        scope: &Scope,
        key: &str,
        mut input: serde_json::Value,
    ) -> Result<serde_json::Value> {
        use bilibi_agent_api::Error as ContractError;
        input["company_id"] = serde_json::json!(company(scope)?);
        let mut answer = self
            .contracts
            .read(scope, key, &input)
            .await
            .map_err(|error| match error {
                ContractError::Backend(error) => error,
                ContractError::Forbidden => Error::Forbidden,
                ContractError::NotFound => Error::NotFound,
                ContractError::InvalidInput(_) => Error::Invalid,
                _ => Error::Internal,
            })?;
        Ok(answer["result"].take())
    }

    /// The names of the people a page mentions.
    pub(crate) async fn people(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Result<bilibi_finance_ledger::People> {
        let client = self.einvoice.db.get().await?;
        Ok(bilibi_finance_ledger::People::read(&self.directory, &**client, scope, ids).await)
    }
}

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::NotFound)
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn type_label(code: &str) -> String {
    Einvoice::authority().document_type_name(code)
}
/// A stored code as a person reads it: "posting_failed" as "Posting failed".
fn sentence(code: &str) -> String {
    let spaced = code.replace('_', " ");
    let mut chars = spaced.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
fn status_badge(status: &str) -> Result<Html> {
    render(Badge {
        kind: match status {
            "valid" => BadgeKind::Success,
            "submitted" | "consolidated" | "issued" | "released" => BadgeKind::Plain,
            "cancelled" | "rejected" => BadgeKind::Warning,
            _ => BadgeKind::Danger,
        },
        dot: true,
        class: "",
        body: Html::text(crate::fiscal::status_label(status)),
    })
}
fn table(
    id: &str,
    caption: &str,
    event: &str,
    columns: &ListColumns,
    rows: Vec<FlexRow>,
) -> Result<Html> {
    let mut table = FlexTable::new(id, columns.flex_columns(), rows);
    table.caption = caption;
    table.event = event;
    table.framed = false;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    render(table)
}
fn time_cell(display: &DisplayContext, at: jiff::Timestamp, precision: Precision) -> Cell {
    let r = display.render(at, Format::DateTime, precision);
    let mut instant = Instant::new(&r.iso, &r.text, &r.company, &r.utc, r.mode.as_str());
    instant.precision = match precision {
        Precision::Minute => "minute",
        Precision::Second => "second",
    };
    Cell::instant(instant)
}

#[derive(Template)]
#[template(
    source = r#"<time id="{{ element }}" datetime="{{ r.iso }}" data-hook="DateTime" data-format="datetime" data-precision="second" data-mode="{{ r.mode.as_str() }}" data-text-company="{{ r.company }}" data-text-utc="{{ r.utc }}" data-follow-shell="true">{{ r.text }}</time>"#,
    ext = "html"
)]
struct Time<'a> {
    element: &'a str,
    r: bilibi_datetime::Rendered,
}

impl Pages {
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.einvoice.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.einvoice.db.get().await?;
        match crate::authorize(&**client, scope, company(scope)?, capability).await {
            Ok(()) => Ok(true),
            Err(EinvoiceError::Failed(error)) => Err(error),
            Err(_) => Ok(false),
        }
    }
    fn state(&self, request: &PageRequest<'_, Scope>) -> Result<Box<dyn LiveState<Scope, Error>>> {
        match request.operation {
            "index" => Ok(Box::new(index::Index::new(
                self,
                request.query,
                request.framed,
            )?)),
            "detail" => Ok(Box::new(detail::Detail::new(
                self,
                request.path,
                request.framed,
            )?)),
            _ => Err(Error::NotFound),
        }
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(if operation == "index" {
            "E-invoices"
        } else {
            "E-invoice"
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation == "detail" {
                let mut detail = detail::Detail::new(self, request.path, request.framed)?;
                let body = detail
                    .update(LiveRequest {
                        scope: request.scope.ok_or(Error::Unauthorized)?,
                        credential: "",
                        input: LiveInput::Refresh,
                    })
                    .await?;
                return Ok(PageBody {
                    html: body.html,
                    live: true,
                    title: detail.title(),
                });
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut index = index::Index::new(self, request.query, request.framed)?;
            index.open_requested(scope).await?;
            let mut state: Box<dyn LiveState<Scope, Error>> = Box::new(index);
            let body = state
                .update(LiveRequest {
                    scope: request.scope.ok_or(Error::Unauthorized)?,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Ok(Some(self.state(&request)?));
            }
            let mut index = index::Index::new(self, request.query, request.framed)?;
            index
                .open_requested(request.scope.ok_or(Error::Unauthorized)?)
                .await?;
            Ok(Some(Box::new(index) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}

pub(crate) struct Downloads {
    pub einvoice: Arc<Einvoice>,
}
impl Download<Scope, Error> for Downloads {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "pdf" {
                return Err(Error::NotFound);
            }
            let artifact = request.path.get("artifact_id").ok_or(Error::NotFound)?;
            // As Bilimbi's PdfController, the artifact owner authorizes its
            // actual document; the cosmetic document path never grants access.
            match self
                .einvoice
                .read_pdf(request.scope, company(request.scope)?, artifact)
                .await
            {
                Ok(file) => Ok(FileBody::attachment(
                    file.bytes,
                    "application/pdf",
                    &file.filename,
                )),
                Err(EinvoiceError::Unauthorized) => Err(Error::Forbidden),
                Err(_) => Err(Error::NotFound),
            }
        })
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: PAGES,
    // New e-invoice records the document, then sends it.
    contracts: &[(new_document::EVENT, new_document::ISSUE), (new_document::EVENT, new_document::SUBMIT)],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Refreshing and cancelling documents, generating their PDFs and Submit to authority on a document's page write through the Einvoice facade; no contract yet." },
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, control::SERVICE, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: control::SERVICE,
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Consolidation, posting retries, setup and connection tests write through the Einvoice facade; no contract yet." },
}) }
