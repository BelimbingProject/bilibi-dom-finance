//! The shape every Finance document page shares (an invoice, a bill, a
//! receipt, a payment), so the twins come out the same: the header leads with
//! Back, the record's own (local) actions as icon buttons, one "Non-local
//! actions" menu for what opens another record or module, and the one primary
//! verb for the record's state (on a list row, one named next-step button),
//! and the body shows the document itself on the left with a narrow summary
//! (status, amount due, payments, journal, files) on the right. Below `lg`
//! the two stack, summary first (on a phone it leads). The grid is `dir="rtl"`
//! with the summary first in the markup, so it takes the right column at `lg`;
//! both children are `dir="ltr"` so their content reads left to right.
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_ui::{AddToItem, AddToMenu, Button, Html, IconButton};

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

/// One action on a record. A non-local action opens another record or module
/// (New credit note, Record debit note, Add to…) and goes in the shared
/// "Non-local actions" menu; a local action acts on this record itself (edit,
/// void, reverse, print, discard) and is a plain icon button with its name as
/// the tooltip, never in that menu. Every action is a link, so the keyboard,
/// a phone and an agent reach it alike; one that opens this page's own dialog
/// goes to this page with `?do=<action>` ([`RecordActions::requested`]).
pub struct RecordAction {
    pub label: String,
    pub href: String,
    /// The icon of a local action; `None` for a non-local one.
    pub icon: Option<&'static str>,
    /// The row's one named next step ([`RecordAction::next`]).
    pub next: bool,
}

impl RecordAction {
    /// A non-local action: a link to another record or module.
    pub fn link(label: &str, href: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            href: href.into(),
            icon: None,
            next: false,
        }
    }

    /// A list row's one named next step (Record receipt on an unpaid invoice,
    /// Request payment on a posted bill): the commonest thing to do with the
    /// record in its state, drawn as a button with its words even when it
    /// opens another kind of record (captain, 2026-11-11). A row has at most
    /// one; a record page draws the same step as its primary verb instead.
    pub fn next(label: &str, href: impl Into<String>) -> Self {
        Self {
            next: true,
            ..Self::link(label, href)
        }
    }

    /// A local action on this record that is a link of its own (print,
    /// correct a draft), drawn as an icon button.
    pub fn local(icon: &'static str, label: &str, href: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            href: href.into(),
            icon: Some(icon),
            next: false,
        }
    }

    /// A local action that opens this page's own dialog through `?do=`,
    /// drawn as an icon button.
    pub fn here(icon: &'static str, label: &str, path: &str, action: &str) -> Self {
        Self::local(icon, label, format!("{path}?{DO}={action}"))
    }
}

/// The query parameter that opens a record page's own dialog.
pub const DO: &str = "do";

/// A record header's actions: Back first, the local actions as icon buttons,
/// the one "Non-local actions" menu (only when there is a non-local action),
/// then the one primary verb at the end of the row.
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
        let (next, items): (Vec<_>, Vec<_>) = items.into_iter().partition(|item| item.next);
        if let Some(item) = next.first() {
            html.push(
                &Html::render(&Button {
                    id: &format!("{menu_id}-next"),
                    href: &item.href,
                    class: "whitespace-nowrap",
                    body: Html::text(&item.label),
                    ..Button::default()
                })
                .map_err(|_| Error::Internal)?,
            );
        }
        let (local, items): (Vec<_>, Vec<_>) =
            items.into_iter().partition(|item| item.icon.is_some());
        for (i, item) in local.iter().enumerate() {
            let id = format!("{menu_id}-local-{i}");
            html.push(
                &Html::render(&IconButton {
                    id: &id,
                    href: &item.href,
                    title: &item.label,
                    ..IconButton::new(item.icon.unwrap_or("manage"), &item.label)
                })
                .map_err(|_| Error::Internal)?,
            );
        }
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

    /// A list row's named next step ([`RecordAction::next`]), its local
    /// actions as icon buttons and its one "Non-local actions" menu, or
    /// nothing when it has none of them.
    pub fn row(menu_id: &str, subject: &str, items: Vec<RecordAction>) -> Result<Html> {
        Self::render(Html::EMPTY, menu_id, subject, items, None)
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

    /// The same link under the entry's own words ("Receipt RCP-00001 · Acme
    /// Trading"), for a page that names what was posted, not its number.
    pub fn named(id: i64, text: &str) -> Result<Html> {
        Html::render(&NamedEntryLink { id, text }).map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct NamedEntryLink<'a> {
    id: i64,
    text: &'a str,
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
