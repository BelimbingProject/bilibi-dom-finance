//! The two tables of the Source documents page (attachments, retention
//! history), their column sets and the cells they draw.
use super::super::{format, time_cell};
use super::{render, Index, ATTACHMENTS_EVENT, CHANGES_EVENT};
use crate::{RetentionChange, Status};
use askama::Template;
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_ui::{
    Badge, BadgeKind, BuiltinColumn, Cell, ColumnType, EmptyState, FlexRow, FlexTable, Fold, Html,
    TableEmpty,
};

pub(super) fn attachment_columns() -> Vec<BuiltinColumn> {
    let mut number = BuiltinColumn::new("number", "#");
    number.kind = ColumnType::Integer;
    let mut dated = BuiltinColumn::new("dated", "Dated");
    dated.kind = ColumnType::Date;
    let mut captured = BuiltinColumn::new("captured", "Captured");
    captured.kind = ColumnType::Datetime;
    vec![
        number,
        BuiltinColumn::new("record", "Record"),
        dated,
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("kept_until", "Kept until"),
        captured,
        BuiltinColumn::new("standing", "Current"),
    ]
}

pub(super) fn change_columns() -> Vec<BuiltinColumn> {
    let mut number = BuiltinColumn::new("number", "#");
    number.kind = ColumnType::Integer;
    let mut years = BuiltinColumn::new("years", "Years").right();
    years.kind = ColumnType::Integer;
    let mut changed = BuiltinColumn::new("changed", "Changed");
    changed.kind = ColumnType::Datetime;
    vec![
        number,
        years,
        BuiltinColumn::new("reason", "Reason"),
        changed,
    ]
}

/// The record a file supports, linked to its page, then its kind muted.
#[derive(Template)]
#[template(
    source = r#"{% if let Some(href) = href %}<a href="{{ href }}" class="text-link hover:underline">{{ label }}</a>{% else %}{{ label }}{% endif %}{% if !kind.is_empty() %} <span class="text-ink-muted">{{ kind }}</span>{% endif %}"#,
    ext = "html"
)]
struct RecordCell<'a> {
    href: Option<String>,
    label: &'a str,
    kind: &'a str,
}

impl Index {
    pub(super) fn attachment_table(&self, display: &DisplayContext) -> Result<Html> {
        let mut rows = Vec::with_capacity(self.shown.len());
        for a in &self.shown {
            let mut row = FlexRow::new(a.id.to_string());
            row.dom_id = Some(format!("attachment-{}", a.id));
            let record = render(RecordCell {
                href: self.pages.attachments.record_href(&a.source, a.document_id),
                label: &a.document_label,
                kind: a.source_label.as_deref().unwrap_or(&a.source),
            })?;
            let date = a.document_date.to_string();
            let file = render(FileCell {
                id: a.id,
                name: &a.file_name,
                size: format::bytes(a.byte_size),
            })?;
            let hash = render(HashCell {
                full: &a.sha256,
                short: format::short_hash(&a.sha256),
            })?;
            let kept = render(KeptCell {
                until: format::retain_until(a.retain_until),
                on_hold: a.on_hold,
            })?;
            let standing = render(Badge {
                kind: if a.status == Status::Standing {
                    BadgeKind::Success
                } else {
                    BadgeKind::Neutral
                },
                dot: true,
                class: "",
                body: Html::text(match a.status {
                    Status::Standing => "Current",
                    Status::Replaced => "Replaced",
                    Status::Removed => "Removed",
                }),
            })?;
            row = row
                .cell("number", Cell::text(a.attachment_number.to_string()))
                .slot("record", record)
                .cell("dated", Cell::date(&date, &date))
                .slot("file", file)
                .cell("sha256", Cell::text(a.sha256.clone()))
                .slot("sha256", hash)
                .slot("kept_until", kept)
                .cell("captured", time_cell(display, a.captured_at))
                .slot("standing", standing);
            rows.push(row);
        }
        let mut table = FlexTable::new("attachments-table", self.columns.flex_columns(), rows);
        table.event = ATTACHMENTS_EVENT;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Attachments";
        if self.shown.is_empty() {
            let filtered = self
                .list
                .filters
                .get("source")
                .is_some_and(|s| !s.is_empty())
                || self.list.filters.get("status").map(String::as_str) != Some("standing");
            table.empty = Some(TableEmpty::State(if filtered {
                EmptyState::missing(
                    "No source documents match",
                    "Choose every kind of record and Current to see the rest.",
                )
            } else {
                EmptyState::missing(
                    "No source documents yet",
                    "Files are attached on each record's own page, such as a bill or an invoice.",
                )
            }));
        }
        render(table)
    }

    pub(super) fn change_table(
        &self,
        changes: &[RetentionChange],
        display: &DisplayContext,
    ) -> Result<Html> {
        if changes.is_empty() {
            return Ok(Html::EMPTY);
        }
        let summary = match changes.len() {
            1 => "1 change".to_owned(),
            n => format!("{n} changes"),
        };
        let rows = changes
            .iter()
            .map(|c| {
                let mut row = FlexRow::new(c.id.to_string());
                row.dom_id = Some(format!("retention-change-{}", c.id));
                row.cell("number", Cell::text(c.change_number.to_string()))
                    .cell("years", Cell::text(c.years.to_string()))
                    .cell("reason", Cell::text(c.reason.clone()))
                    .cell("changed", time_cell(display, c.changed_at))
            })
            .collect();
        let mut table = FlexTable::new(
            "attachments-retention-changes",
            self.change_columns.flex_columns(),
            rows,
        );
        table.event = CHANGES_EVENT;
        table.zoom = self.change_columns.zoom;
        table.suggestions = self.change_columns.flex_suggestions();
        table.add_query = &self.change_columns.add_query;
        table.framed = false;
        table.caption = "Retention changes";
        render(Fold {
            id: "attachments-retention-history",
            title: "History",
            summary: &summary,
            open: false,
            body: render(table)?,
        })
    }
}

#[derive(Template)]
#[template(
    source = r#"<a id="attachment-{{ id }}-download" href="/finance/attachments/{{ id }}/download" download="{{ name }}">{{ name }}</a> <span class="text-ink-muted">({{ size }})</span>"#,
    ext = "html"
)]
struct FileCell<'a> {
    id: i64,
    name: &'a str,
    size: String,
}

#[derive(Template)]
#[template(
    source = r#"<span class="font-mono" title="{{ full }}">{{ short }}</span>"#,
    ext = "html"
)]
struct HashCell<'a> {
    full: &'a str,
    short: String,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{{ until }}{% if on_hold %} {% call ui::badge() %}Legal hold{% endcall %}{% endif %}"#,
    ext = "html"
)]
struct KeptCell {
    until: String,
    on_hold: bool,
}
