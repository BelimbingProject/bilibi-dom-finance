//! The attachments pages (Bilimbi `Finance.Attachments.Web`, `web_routes.exs`
//! and the menu item of `Contributions`): the Source documents page, the
//! download route and the embeddable panel a record's page mounts. The pages
//! own no CSS or JavaScript and compose Base UI's components only.
mod download;
mod format;
mod index;
mod panel;

use crate::MODULE;
pub(crate) use download::Downloads;
pub(crate) use index::Pages;
pub(crate) use panel::Panels;

use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_module_registry::{
    contributions::{
        Access, Contribution, Download, Embed, EmbedContext, Method, Route, DOWNLOADS, EMBEDS,
        ROUTES,
    },
    Provenance,
};
use bilibi_ui::{Cell, Html, Instant};

/// The service ids the registry resolves for the routes below.
pub(crate) const PAGES: &str = "finance/attachments.pages";
pub(crate) const FILES: &str = "finance/attachments.files";
pub(crate) const PANELS: &str = "finance/attachments.panels";

/// The embed a record's page mounts as a panel (see `panel.rs`).
pub const EMBED: &str = "finance.attachments-panel";

/// Bilimbi `LiveAuthorization`'s denied message.
pub(crate) const DENIED: &str = "You do not have permission to do that.";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

// Bilimbi `Contributions.menu`: id, label, icon, parent, route, capability, order.
bilibi_menu::contribute!(
    "finance/attachments",
    bilibi_menu::MenuItem::page(
        "finance.attachments",
        "Source documents",
        "/finance/attachments"
    )
    .company_scoped()
    .icon("gmdi-money-r")
    .under("finance.group.bookkeeping")
    .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
    .order(2)
);

inventory::submit! { Contribution::new(ROUTES, "finance/attachments.index", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
    method: Method::Get, path: "/finance/attachments", service: PAGES,
    operation: "index", access: Access::Capability(crate::VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "finance/attachments.download", provenance!(), &Download {
    path: "/finance/attachments/{id}/download", service: FILES,
    operation: "download", access: Access::Capability(crate::VIEW),
}) }
// Bilimbi mounts the panel from the record's own page with no gate of its
// own; it asks the facade for every read and write, so the embed is only
// `SignedIn`.
inventory::submit! { Contribution::new(EMBEDS, EMBED, provenance!(), &Embed {
    slot: "finance.attachments", context: EmbedContext::Record { kind: "*" },
    service: PANELS, operation: "panel", access: Access::SignedIn,
}) }

/// Bilimbi `<.datetime>`: both server-decidable texts travel with the element,
/// so the shell's display-mode control swaps them without a re-render.
#[derive(Template)]
#[template(
    source = r#"<time id="{{ id }}" datetime="{{ iso }}" data-format="datetime" data-precision="minute" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums">{{ text }}</time>"#,
    ext = "html"
)]
struct Time<'a> {
    id: &'a str,
    iso: String,
    text: String,
    company: String,
    utc: String,
    mode: &'static str,
}

pub(crate) fn time_html(display: &DisplayContext, id: &str, at: jiff::Timestamp) -> Result<Html> {
    let r = display.render(at, Format::DateTime, Precision::Minute);
    Html::render(&Time {
        id,
        iso: r.iso,
        text: r.text,
        company: r.company,
        utc: r.utc,
        mode: r.mode.as_str(),
    })
    .map_err(|_| Error::Internal)
}

/// A timestamp cell that stays typed through every lens of the table.
pub(crate) fn time_cell(display: &DisplayContext, at: jiff::Timestamp) -> Cell {
    let r = display.render(at, Format::DateTime, Precision::Minute);
    Cell::instant(Instant::new(
        &r.iso,
        &r.text,
        &r.company,
        &r.utc,
        r.mode.as_str(),
    ))
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: PAGES,
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Retention years write through the Attachments facade; no contract yet." },
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PANELS, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: PANELS,
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Attaching, replacing and removing source documents write through the Attachments facade; no contract yet." },
}) }
