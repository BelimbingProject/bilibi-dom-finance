//! Labels, badges, field errors and the page frame the Receivables pages share.
use super::{render, Flash};
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_ui::{
    error_sentence, Badge, BadgeKind, BuiltinColumn, Fact, FlashGroup, FlexRow, FlexTable, Html,
    ListColumns, Modal, ModalWidth, Page, PageVariant,
};
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn kind_label(kind: &str) -> &'static str {
    if kind == "opening_credit" {
        "Customer credit"
    } else if kind == "credit_note" {
        "Credit note"
    } else {
        "Invoice"
    }
}

pub(super) fn opening_item_kind_label(kind: &str) -> &str {
    match kind {
        "customer_credit" => "Customer credit",
        "invoice" => "Invoice",
        _ => kind,
    }
}

pub(super) fn series_kind_label(kind: &str) -> &'static str {
    match kind {
        "credit_note" => "Credit note",
        "payment" => "Receipt",
        _ => "Invoice",
    }
}

pub(super) fn status_label(status: &str) -> &'static str {
    match status {
        "issued" => "Issued",
        "discarded" => "Discarded",
        _ => "Draft",
    }
}

pub(super) fn badge(text: &str, kind: BadgeKind) -> Result<Html> {
    render(Badge {
        kind,
        dot: true,
        class: "",
        body: Html::text(text),
    })
}

pub(super) fn status_badge(status: &str) -> Result<Html> {
    badge(
        status_label(status),
        match status {
            "issued" => BadgeKind::Success,
            _ => BadgeKind::Neutral,
        },
    )
}

pub(super) fn active_badge(active: bool) -> Result<Html> {
    badge(
        if active { "Active" } else { "Inactive" },
        if active {
            BadgeKind::Success
        } else {
            BadgeKind::Neutral
        },
    )
}

pub(super) fn optional(text: &Option<String>) -> &str {
    text.as_deref().filter(|t| !t.is_empty()).unwrap_or("—")
}

pub(super) fn number(number: Option<i64>) -> String {
    number.map_or_else(|| "—".into(), |n| format!("#{n}"))
}

/// A quantity or price without trailing zeros.
pub(super) fn decimal(value: Decimal) -> String {
    value.normalize().to_string()
}

/// A unit price: no trailing zeros past the second place.
pub(super) fn price(value: Decimal) -> String {
    let mut value = value.normalize();
    if value.scale() < 2 {
        value.rescale(2);
    }
    value.to_string()
}

pub(super) fn actor(user: Option<i64>, impersonator: Option<i64>) -> String {
    match (user, impersonator) {
        (Some(u), Some(i)) => format!("User #{u} (impersonated by #{i})"),
        (Some(u), None) => format!("User #{u}"),
        (None, _) => "—".into(),
    }
}

pub(super) fn fact<'a>(title: &'a str, value: &str) -> Fact<'a> {
    Fact {
        title,
        value: Html::text(value),
        ..Fact::default()
    }
}

pub(super) fn fact_html(title: &str, value: Html) -> Fact<'_> {
    Fact {
        title,
        value,
        ..Fact::default()
    }
}

/// A PostgreSQL timestamp from a contract has no offset and is UTC; it is
/// shown in the reader's own clock and zone, never in the browser's.
pub(super) fn time(display: &DisplayContext, id: &str, at: &str) -> Result<Html> {
    let at = at
        .parse::<jiff::civil::DateTime>()
        .map_err(|_| Error::Internal)?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|_| Error::Internal)?
        .timestamp();
    let r = display.render(at, Format::DateTime, Precision::Second);
    render(Time {
        element_id: id,
        iso: r.iso,
        text: r.text,
        company: r.company,
        utc: r.utc,
        mode: r.mode.as_str(),
    })
}

#[derive(askama::Template)]
#[template(
    source = r#"<time id="{{ element_id }}" datetime="{{ iso }}" data-format="datetime" data-precision="second" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums">{{ text }}</time>"#,
    ext = "html"
)]
struct Time<'a> {
    element_id: &'a str,
    iso: String,
    text: String,
    company: String,
    utc: String,
    mode: &'static str,
}

#[derive(askama::Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>"#,
    ext = "html"
)]
struct Tabular<'a> {
    text: &'a str,
}

/// Right-aligned figures in a table cell.
pub(super) fn tabular(text: &str) -> Result<Html> {
    render(Tabular { text })
}

#[derive(askama::Template)]
#[template(
    source = r#"<a href="{{ href }}" class="text-action hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct Link<'a> {
    href: &'a str,
    text: &'a str,
}

/// A plain navigation link inside a fact or a cell.
pub(super) fn link(href: &str, text: &str) -> Result<Html> {
    render(Link { href, text })
}

/// What a refused write said, by field path (`lines.0.quantity`, `base`).
pub(super) use bilibi_finance_ledger::PartyErrors as Errors;

/// The text of one posted form value, trimmed.
pub(super) fn posted<'a>(
    form: &'a std::collections::HashMap<String, String>,
    key: &str,
) -> &'a str {
    form.get(key).map_or("", |v| v.trim())
}

/// Set an optional text field on a contract input: sent when it has text.
pub(super) fn put(input: &mut Value, key: &str, text: &str) {
    if !text.is_empty() {
        input[key] = Value::String(text.into());
    }
}

/// A page with its flash outlet and body, as every Receivables page draws it.
/// A dialog goes inside the page element, never after it: the page carries
/// the `data-shared-ui` boundary that stops the Platform's legacy element
/// rules (`legacy-compat.css`), and a dialog outside it gets its labels laid
/// out as columns, with required stars and checkboxes dropped under their
/// text. A modal `<dialog>` renders in the top layer wherever it sits.
pub(super) fn frame(
    id: &str,
    variant: PageVariant,
    flash: &Flash,
    mut body: Html,
    modal: Option<Html>,
) -> Result<String> {
    if let Some(modal) = modal {
        body.push(&modal);
    }
    let mut html = render(FlashGroup {
        id: "flash-group",
        flash: flash.map(),
    })?;
    html.push(&render(Page {
        id,
        variant,
        class: "",
        body,
    })?);
    Ok(html.to_string())
}

/// A form dialog over the page; it shows the page's flash itself while open.
/// The shared dialog owns `{id}-title`, `{id}-description` and `{id}-body`,
/// so give the dialog an id no field inside it extends that way (a
/// `description` field in a `receivables-draft` dialog would share its id
/// with the dialog's description, and its label would bind to the wrong
/// element): name the dialog `<record>-modal`.
pub(super) fn modal(
    id: &str,
    title: &str,
    description: &str,
    cancel_event: &str,
    width: ModalWidth,
    flash: &Flash,
    body: Html,
) -> Result<Html> {
    render(Modal {
        id,
        title,
        cancel_event,
        width,
        description,
        role: "",
        flash: Some(flash.map()),
        body,
    })
}

/// The columns of a table on a record page; `right` ones hold figures.
pub(super) fn columns(id: &str, cols: &[(&str, &str)], right: &[&str]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        cols.iter()
            .map(|(id, label)| {
                let column = BuiltinColumn::new(id, label);
                if right.contains(id) {
                    column.right()
                } else {
                    column
                }
            })
            .collect(),
    )
    .map_err(|_| Error::Internal)
}

/// A table of a record page, drawn with the shared notch, inside a `p-0`
/// card that is its one frame.
pub(super) fn table(
    columns: &ListColumns,
    event: &str,
    caption: &str,
    rows: Vec<FlexRow>,
    empty: &str,
) -> Result<Html> {
    table_in(columns, event, caption, rows, empty, false)
}

/// The same table; `framed` draws its own border where no `p-0` card does.
pub(super) fn table_in(
    columns: &ListColumns,
    event: &str,
    caption: &str,
    rows: Vec<FlexRow>,
    empty: &str,
    framed: bool,
) -> Result<Html> {
    let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
    table.event = event;
    table.caption = caption;
    table.framed = framed;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.empty = Some(bilibi_ui::TableEmpty::Content(Html::text(empty)));
    render(table)
}

/// A fact that reads first and edits in place. Receivables' contracts carry no
/// version fence, so the field's version is fixed and the last save wins.
pub(super) fn inline(
    key: &str,
    label: &str,
    value: &str,
    record: i64,
    event: &str,
    editable: bool,
    statuses: &bilibi_ui::CommitStates,
) -> bilibi_ui::InlineField {
    let mut field =
        bilibi_ui::InlineField::new(format!("receivables-field-{key}"), key, label, value)
            .version(0)
            .status(statuses.get(key))
            .allow_empty();
    field.id_value = record.to_string();
    field.save_event = event.into();
    if !editable {
        field = field.readonly();
    }
    field
}

/// The outcome of one inline save, worded for the fact it belongs to.
pub(super) fn commit_status(
    written: &super::Written,
    key: &str,
    label: &str,
    submitted: &str,
) -> bilibi_ui::CommitStatus {
    use bilibi_ui::CommitStatus;
    match written {
        super::Written::Done(_) => CommitStatus::Saved,
        super::Written::Invalid(fields) => {
            let reasons: Vec<&str> = fields
                .iter()
                .filter(|(path, _)| path.as_str() == key || path.as_str() == "base")
                .flat_map(|(_, messages)| messages.iter().map(String::as_str))
                .collect();
            if reasons.is_empty() {
                CommitStatus::refusal(label, submitted, &["is not valid"])
            } else {
                CommitStatus::refusal(label, submitted, &reasons)
            }
        }
        super::Written::Refused(message) => CommitStatus::Error(message.clone()),
    }
}

/// How the company's book writes an amount: its minor units and currency. A
/// person without ledger view cannot read the book, so the amounts then show
/// with at least two places and the currency the record itself carries.
#[derive(Clone, Debug, Default)]
pub(crate) struct Figures {
    pub currency: String,
    units: Option<MinorUnits>,
}

impl Figures {
    pub(super) fn book(currency: &str, minor_units: u32) -> Self {
        Self {
            currency: currency.into(),
            units: MinorUnits::new(i64::from(minor_units)),
        }
    }

    pub(super) fn decimal(&self, value: Decimal) -> String {
        Money::display_decimal(value, self.units)
    }

    pub(super) fn shown(&self, value: Money) -> String {
        self.units
            .map(|units| value.shown(units))
            .unwrap_or_default()
    }

    pub(super) fn money(&self, value: Money) -> String {
        value.display(self.units)
    }

    pub(super) fn optional(&self, value: Option<Money>) -> String {
        value.map_or_else(|| "—".into(), |m| self.money(m))
    }

    pub(super) fn with_currency(&self, value: Money, fallback: &str) -> String {
        format!(
            "{} {}",
            self.money(value),
            if self.currency.is_empty() {
                fallback
            } else {
                &self.currency
            }
        )
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="mt-4 flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct ButtonRow {
    buttons: Html,
}

/// Buttons that sit under a dialog's change-tracking form: they raise their
/// own events, so the form's values must already be on the server. Cancel
/// comes first; the primary action follows, at the end of the row.
pub(super) fn buttons(buttons: Html) -> Result<Html> {
    render(ButtonRow { buttons })
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct ActionRow {
    buttons: Html,
}

/// The actions of a shared `Form` in a dialog. The form's own row starts at
/// the left, so this row fills that line and keeps Cancel, then the verb, at
/// the end.
pub(super) fn actions(buttons: Html) -> Result<Html> {
    render(ActionRow { buttons })
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::section(labelledby=id, heading=heading) %}<div class="grid items-start gap-x-3 sm:grid-cols-2">{{ body|safe }}</div>{% endcall %}"#,
    ext = "html"
)]
struct FieldGroup<'a> {
    id: &'a str,
    heading: &'a str,
    body: Html,
}

/// Related fields in one bordered section, two columns from the `sm` width up.
pub(super) fn group(id: &str, heading: &str, body: Html) -> Result<Html> {
    render(FieldGroup { id, heading, body })
}

impl Figures {
    pub(super) fn units(&self) -> Option<MinorUnits> {
        self.units
    }

    pub(super) fn with_currency_opt(&self, value: Option<Money>, fallback: &str) -> String {
        value.map_or_else(|| "—".into(), |m| self.with_currency(m, fallback))
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "workspace.html" as workspace %}{% call workspace::announce(kind=kind, record_id=record_id) %}{% endcall %}"#,
    ext = "html"
)]
struct Announce<'a> {
    kind: &'a str,
    record_id: &'a str,
}

/// Marks a record's page so another workspace tile can follow the selection.
pub(super) fn announce(record: i64) -> Result<Html> {
    render(Announce {
        kind: crate::MODULE,
        record_id: &record.to_string(),
    })
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="grid grid-cols-2 items-start gap-x-3 sm:grid-cols-4">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Four {
    body: Html,
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="grid items-start gap-x-3 sm:grid-cols-2">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Two {
    body: Html,
}

/// Fields side by side: four on a wide screen, two on a phone.
pub(super) fn four(body: Html) -> Result<Html> {
    render(Four { body })
}

/// Fields in two columns on a wide screen, one on a phone.
pub(super) fn two(body: Html) -> Result<Html> {
    render(Two { body })
}

/// Every refusal of a write as one plain message, each beginning with the
/// field it is about ("Issue date can't be blank."). A refusal of the whole
/// request (`base`) reads as it is.
pub(super) fn sentences(fields: &BTreeMap<String, Vec<String>>) -> String {
    let mut parts = Vec::new();
    for (path, messages) in fields {
        let name = path.rsplit('.').next().unwrap_or(path);
        for message in messages {
            if name == "base" {
                parts.push(message.clone());
            } else {
                let label = name.replace('_', " ");
                let mut chars = label.chars();
                let label: String = chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default();
                parts.push(error_sentence(&label, message));
            }
        }
    }
    parts.join(" ")
}
