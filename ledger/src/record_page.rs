//! The shape every Finance document page shares (an invoice, a bill, a
//! receipt, a payment), so the twins come out the same: the header leads with
//! Back, one "Actions…" menu and the one primary verb for the record's state,
//! and the body shows the document itself on the left with a narrow summary
//! (status, amount due, payments, journal, files) on the right. Below `lg`
//! the two stack, summary first (on a phone it leads). The grid is `dir="rtl"`
//! with the summary first in the markup, so it takes the right column at `lg`;
//! both children are `dir="ltr"` so their content reads left to right.
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_ui::{AddToItem, AddToMenu, Html};

/// The document and its summary side by side at `lg`, summary first below it.
#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" dir="rtl" class="grid items-start gap-4 lg:grid-cols-3"><aside id="{{ id }}-summary" dir="ltr" aria-label="Summary" class="min-w-0 space-y-4">{{ summary|safe }}</aside><div id="{{ id }}-document" dir="ltr" class="min-w-0 space-y-4 lg:col-span-2">{{ document|safe }}</div></div>"#,
    ext = "html"
)]
pub struct DocumentLayout {
    pub id: String,
    pub document: Html,
    pub summary: Html,
}

impl DocumentLayout {
    pub fn render(id: &str, document: Html, summary: Html) -> Result<Html> {
        Html::render(&DocumentLayout {
            id: id.to_owned(),
            document,
            summary,
        })
        .map_err(|_| Error::Internal)
    }
}

/// One item of a record's "Actions…" menu. Every item is a link, so the
/// keyboard, a phone and an agent reach it alike: one to another page, or to
/// this page with `?do=<action>`, which opens that action's dialog
/// ([`RecordActions::requested`]).
pub struct RecordAction {
    pub label: String,
    pub href: String,
}

impl RecordAction {
    /// A link to another page (print, new credit note on another list).
    pub fn link(label: &str, href: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            href: href.into(),
        }
    }

    /// This page's own dialog for `action`, opened through `?do=`.
    pub fn here(label: &str, path: &str, action: &str) -> Self {
        Self {
            label: label.to_owned(),
            href: format!("{path}?{DO}={action}"),
        }
    }
}

/// The query parameter that opens a record page's own dialog.
pub const DO: &str = "do";

/// A record header's actions: Back first, then the one "Actions…" menu, then
/// the one primary verb at the end of the row.
pub struct RecordActions;

impl RecordActions {
    pub fn render(
        back: Html,
        menu_id: &str,
        subject: &str,
        items: Vec<RecordAction>,
        primary: Option<Html>,
    ) -> Result<Html> {
        let mut html = back;
        if !items.is_empty() {
            html.push(
                &Html::render(&AddToMenu::new(
                    menu_id,
                    subject,
                    items
                        .into_iter()
                        .map(|item| AddToItem {
                            label: item.label,
                            href: item.href,
                            disabled: None,
                        })
                        .collect(),
                ))
                .map_err(|_| Error::Internal)?,
            );
        }
        if let Some(primary) = primary {
            html.push(&primary);
        }
        Ok(html)
    }

    /// The action an address asked this page to open (`?do=void`), if any.
    pub fn requested(query: &std::collections::HashMap<String, String>) -> Option<String> {
        query
            .get(DO)
            .filter(|action| {
                !action.is_empty()
                    && action.len() <= 40
                    && action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            })
            .cloned()
    }
}

/// A ledger entry as a record page names it ("Journal 12"), linked to its
/// journal page, so a person can open what a document posted.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">Journal {{ number }}</a>"#,
    ext = "html"
)]
pub struct EntryLink {
    pub id: i64,
    pub number: i64,
}

impl EntryLink {
    /// The link, or "—" while nothing is posted.
    pub fn html(id: Option<i64>, number: Option<i64>) -> Result<Html> {
        match (id, number) {
            (Some(id), Some(number)) => {
                Html::render(&EntryLink { id, number }).map_err(|_| Error::Internal)
            }
            _ => Ok(Html::text("—")),
        }
    }
}

/// An account as a record page shows it: the name, then the code muted and
/// italic, as the combobox draws a choice (`view::account_choice`).
#[derive(Template)]
#[template(
    source = r#"<span>{{ name }}</span> <span class="italic text-ink-muted">{{ code }}</span>"#,
    ext = "html"
)]
pub struct AccountName<'a> {
    pub name: &'a str,
    pub code: &'a str,
}

impl AccountName<'_> {
    /// From a "Name · code" label; a label without a code shows as it is.
    pub fn label(label: &str) -> Result<Html> {
        match label.rsplit_once(" · ") {
            Some((name, code)) => {
                Html::render(&AccountName { name, code }).map_err(|_| Error::Internal)
            }
            None => Ok(Html::text(label)),
        }
    }
}
