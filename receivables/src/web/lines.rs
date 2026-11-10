//! The lines of an invoice or credit note as a person types them: one editor
//! for the New invoice dialog (`documents/invoice_form.rs`) and a draft's
//! Edit lines dialog (`document.rs`). Each line shows its amount and the
//! dialog its totals while the person types, worked out as `draft.create`
//! will (quantity times unit price, rounded half up; tax per line). Rows
//! pasted from a spreadsheet are read with `bilibi_finance_ledger::columns`
//! and land in the same fields, where the person checks them before saving.
//! The fields sit inside the owner's change-tracking `Form`; the editor reads
//! its own `line[...]` keys, because `params::form_values` keeps only 64.
use super::{
    format::{self as f, Errors},
    render, Account,
};
use crate::{DocumentLine, TaxCode};
use askama::Template;
use bilibi_database::Result;
use bilibi_finance_ledger::{columns, Money};
use bilibi_ui::{
    Alert, Button, Choice, Combobox, Field, Html, IconButton, IconContext, InputKind, StatusKind,
};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};

/// The most lines one document takes from a page.
const MAX_LINES: usize = 100;
const FORM: &str = "line";
const FIELDS: [(&str, &str); 5] = [
    ("description", "Description"),
    ("quantity", "Quantity"),
    ("unit_price", "Unit price"),
    ("account_id", "Account"),
    ("tax_code_id", "Tax code"),
];
/// The header words that name a pasted column, most specific first.
const ROLES: [columns::Role; 4] = [
    columns::Role {
        key: "quantity",
        words: &["quantity", "qty", "units", "hours"],
    },
    columns::Role {
        key: "unit_price",
        words: &["unit price", "price", "rate", "unit cost"],
    },
    columns::Role {
        key: "description",
        words: &["description", "item", "details", "particulars", "service"],
    },
    columns::Role {
        key: "amount",
        words: &["amount", "total"],
    },
];

#[derive(Clone, Default)]
struct Row {
    id: u32,
    description: String,
    quantity: String,
    unit_price: String,
    account: String,
    tax: String,
}

impl Row {
    /// Nothing typed that would make a line: such a row is not sent.
    fn blank(&self) -> bool {
        [&self.description, &self.quantity, &self.unit_price]
            .iter()
            .all(|text| text.trim().is_empty())
    }
}

/// The lines as typed, and what the last save sent.
pub(super) struct Lines {
    rows: Vec<Row>,
    next: u32,
    /// The rows of the last input, in the order the contract numbered them.
    sent: Vec<u32>,
    /// The paste box: open, its text, and what the last paste came to.
    pasting: bool,
    paste: String,
    note: Option<(StatusKind, String)>,
}

/// A typed figure: plain or with thousands commas, never negative.
fn typed(text: &str) -> Option<Decimal> {
    Money::ungrouped(text)
        .and_then(|plain| Decimal::from_str_exact(&plain).ok())
        .filter(|value| !value.is_sign_negative())
}

impl Lines {
    /// One empty line, as a new document starts.
    pub(super) fn new() -> Self {
        let mut lines = Self {
            rows: vec![],
            next: 0,
            sent: vec![],
            pasting: false,
            paste: String::new(),
            note: None,
        };
        lines.add();
        lines
    }

    /// The saved lines of a draft, or one empty line when it has none.
    pub(super) fn of(saved: &[DocumentLine]) -> Self {
        let mut lines = Self::new();
        if !saved.is_empty() {
            lines.rows.clear();
            for line in saved {
                lines.add();
                let row = lines.rows.last_mut().expect("added row");
                row.description = line.description.clone();
                row.quantity = f::decimal(line.quantity);
                row.unit_price = f::price(line.unit_price);
                row.account = line.account_id.to_string();
                row.tax = line
                    .tax_code_id
                    .map(|id| id.to_string())
                    .unwrap_or_default();
            }
        }
        lines
    }

    fn add(&mut self) {
        if self.rows.len() < MAX_LINES {
            self.next += 1;
            self.rows.push(Row {
                id: self.next,
                ..Row::default()
            });
        }
    }

    /// Reads the browser's `line[...]` fields into the rows.
    pub(super) fn read(&mut self, v: &Value) {
        for (name, value) in v.as_object().into_iter().flatten() {
            let Some(key) = name
                .strip_prefix(FORM)
                .and_then(|n| n.strip_prefix('['))
                .and_then(|n| n.strip_suffix(']'))
            else {
                continue;
            };
            let Some(text) = value.as_str() else { continue };
            if key == "paste" {
                self.paste = text.chars().take(columns::MAX_BYTES).collect();
                continue;
            }
            let Some((field, id)) = key
                .rsplit_once('_')
                .and_then(|(field, id)| Some((field, id.parse::<u32>().ok()?)))
            else {
                continue;
            };
            let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
                continue;
            };
            let text: String = text.chars().take(1024).collect();
            match field {
                "description" => row.description = text,
                "quantity" => row.quantity = text,
                "unit_price" => row.unit_price = text,
                "account_id" => row.account = text,
                "tax_code_id" => row.tax = text,
                _ => {}
            }
        }
    }

    /// One of the editor's own buttons. False when the event is not its own.
    pub(super) fn event(&mut self, v: &Value) -> bool {
        match v["type"].as_str().unwrap_or("") {
            "add_line" => self.add(),
            "remove_line" => {
                let id = v["line"].as_str().and_then(|id| id.parse::<u32>().ok());
                self.rows.retain(|row| Some(row.id) != id);
                if self.rows.is_empty() {
                    self.add();
                }
            }
            "paste_open" => {
                self.pasting = true;
                self.note = None;
            }
            "paste_close" => {
                self.pasting = false;
                self.paste.clear();
                self.note = None;
            }
            "paste_lines" => self.take_paste(),
            _ => return false,
        }
        true
    }

    /// Whether any line would be sent.
    pub(super) fn any(&self) -> bool {
        self.rows.iter().any(|row| !row.blank())
    }

    /// The contract's `lines`: every row that has something typed, as typed.
    pub(super) fn input(&mut self) -> Value {
        self.sent.clear();
        let mut lines = Vec::new();
        for row in self.rows.iter().filter(|row| !row.blank()) {
            self.sent.push(row.id);
            let mut line = json!({
                "description": row.description.trim(),
                "quantity": row.quantity.trim(),
                "unit_price": row.unit_price.trim(),
            });
            if let Ok(id) = row.account.parse::<i64>() {
                line["account_id"] = json!(id);
            }
            if let Ok(id) = row.tax.parse::<i64>() {
                line["tax_code_id"] = json!(id);
            }
            lines.push(line);
        }
        json!(lines)
    }

    /// Where the row's refusals are kept: `lines.<n>` of the last input.
    fn path(&self, row: &Row, key: &str) -> Option<String> {
        let at = self.sent.iter().position(|id| *id == row.id)?;
        Some(format!("lines.{at}.{key}"))
    }

    /// Each line field's path, element and name, for the form's error summary.
    pub(super) fn labels(&self) -> Vec<(String, String, String)> {
        let mut labels = vec![(
            "lines".to_owned(),
            "receivables-lines-add".to_owned(),
            "The document".to_owned(),
        )];
        for (n, row) in self.rows.iter().enumerate() {
            for (key, label) in FIELDS {
                if let Some(path) = self.path(row, key) {
                    labels.push((
                        path,
                        format!("receivables-line-{key}-{}", row.id),
                        format!("Line {} {}", n + 1, label.to_lowercase()),
                    ));
                }
            }
        }
        labels
    }

    /// Adds the pasted rows after the lines already typed. The guess only
    /// fills the fields; nothing is kept until the person saves.
    fn take_paste(&mut self) {
        let sheet = match columns::Sheet::read(&self.paste) {
            Ok(sheet) if !sheet.rows.is_empty() => sheet,
            Ok(_) => {
                self.note = Some((
                    StatusKind::Error,
                    "Paste at least one row: a description, a quantity and a unit price.".into(),
                ));
                return;
            }
            Err(error) => {
                self.note = Some((StatusKind::Error, error.to_string()));
                return;
            }
        };
        let headed = sheet.looks_headed();
        let found = columns::by_words(&sheet.names(headed), &ROLES);
        let mark = columns::decimal_mark(
            sheet
                .data(headed)
                .iter()
                .flat_map(|row| row.iter().map(String::as_str)),
        );
        let figures = sheet.numeric_columns(headed, mark);
        let description = found
            .get("description")
            .copied()
            .or_else(|| (0..sheet.width()).find(|i| !figures.contains(i)));
        // Without header words, the first column of figures is the quantity
        // and the next the unit price; a lone one is the price of one unit.
        let mut spare = figures
            .iter()
            .copied()
            .filter(|i| !found.values().any(|taken| taken == i) && Some(*i) != description);
        let mut quantity = found.get("quantity").copied();
        let mut price = found
            .get("unit_price")
            .copied()
            .or_else(|| found.get("amount").copied().filter(|_| quantity.is_none()));
        if price.is_none() {
            let (first, second) = (spare.next(), spare.next());
            match (quantity, first, second) {
                (None, Some(a), Some(b)) => (quantity, price) = (Some(a), Some(b)),
                (_, Some(a), _) => price = Some(a),
                _ => {}
            }
        }
        let figure = |row: &[String], column: Option<usize>| {
            let cell = column.and_then(|i| row.get(i)).map_or("", String::as_str);
            match columns::number(cell, mark) {
                Ok(Some(value)) => value.normalize().to_string(),
                Ok(None) => String::new(),
                // Left as pasted, so the person sees what could not be read.
                Err(_) => cell.to_owned(),
            }
        };
        self.rows.retain(|row| !row.blank());
        let (mut added, mut left) = (0, 0);
        for cells in sheet.data(headed) {
            let text = description
                .and_then(|i| cells.get(i))
                .cloned()
                .unwrap_or_default();
            let unit_price = figure(cells, price);
            if text.is_empty() && unit_price.is_empty() {
                continue;
            }
            if self.rows.len() >= MAX_LINES {
                left += 1;
                continue;
            }
            let typed_quantity = figure(cells, quantity);
            self.add();
            let row = self.rows.last_mut().expect("added row");
            row.description = text;
            row.quantity = if typed_quantity.is_empty() {
                "1".into()
            } else {
                typed_quantity
            };
            row.unit_price = unit_price;
            added += 1;
        }
        if self.rows.is_empty() {
            self.add();
        }
        self.note = Some(match (added, left) {
            (0, 0) => (
                StatusKind::Error,
                "No rows could be read. Each row needs a description or a price.".into(),
            ),
            (_, 0) => {
                self.pasting = false;
                self.paste.clear();
                (
                    StatusKind::Success,
                    format!(
                        "{added} {} added. Check them before you save.",
                        if added == 1 { "line" } else { "lines" }
                    ),
                )
            }
            _ => (
                StatusKind::Warning,
                format!("{added} lines added and {left} left out: a document takes up to {MAX_LINES} lines."),
            ),
        });
    }

    /// The amount and tax of one row as the contract will work them out, or
    /// nothing while its quantity or price is not a number yet.
    fn amounts(
        row: &Row,
        taxes: &[TaxCode],
        places: u32,
    ) -> Option<(Decimal, Decimal, Option<i64>)> {
        let round = |value: Decimal| {
            value.round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero)
        };
        let amount = round(typed(&row.quantity)?.checked_mul(typed(&row.unit_price)?)?);
        let code = row
            .tax
            .parse::<i64>()
            .ok()
            .and_then(|id| taxes.iter().find(|t| t.id == id));
        let tax = match code {
            Some(code) => round(amount.checked_mul(code.rate)? / Decimal::from(100)),
            None => Decimal::ZERO,
        };
        Some((amount, tax, code.map(|code| code.id)))
    }

    /// The line fields, the add and paste controls and the totals, to place
    /// inside the owner's form.
    pub(super) fn body(
        &self,
        errors: &Errors,
        accounts: &[Account],
        taxes: &[TaxCode],
        figures: &f::Figures,
    ) -> Result<Html> {
        let places = figures.units().map(|units| u32::from(units.get()));
        let account_choices: Vec<Choice> = accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == "income")
            .map(Account::choice)
            .collect();
        let tax_choices: Vec<Choice> = taxes
            .iter()
            .filter(|t| t.active)
            .map(|t| {
                Choice::new(
                    format!("{} ({}%)", t.code, f::decimal(t.rate)),
                    t.id.to_string(),
                )
            })
            .collect();
        let mut body = Html::EMPTY;
        let mut subtotal = Decimal::ZERO;
        let mut tax_totals: Vec<(i64, Decimal)> = vec![];
        let mut complete = true;
        for (n, row) in self.rows.iter().enumerate() {
            let under = |key: &str, label: &str| match self.path(row, key) {
                Some(path) => errors.under(&path, label),
                None => vec![],
            };
            let field = |key: &str, label: &str, value: &str| {
                Field::in_form(FORM, &format!("{key}_{}", row.id))
                    .id(format!("receivables-line-{key}-{}", row.id))
                    .label(label)
                    .wrapper_class("mb-0")
                    .value(value)
                    .errors(under(key, label))
            };
            let figure = |key: &str, label: &str, value: &str| {
                field(key, label, value)
                    .attr("inputmode", "decimal")
                    .attr("data-debounce", "400")
            };
            let amounts = places.and_then(|places| Self::amounts(row, taxes, places));
            match amounts {
                Some((amount, tax, code)) => {
                    subtotal += amount;
                    if let Some(code) = code {
                        match tax_totals.iter_mut().find(|(id, _)| *id == code) {
                            Some((_, total)) => *total += tax,
                            None => tax_totals.push((code, tax)),
                        }
                    }
                }
                None => complete &= row.blank(),
            }
            body.push(&render(LineRow {
                id: row.id,
                first: n == 0,
                description: render(
                    field("description", "Description", &row.description)
                        .attr("maxlength", "500")
                        .attr("data-debounce", "400"),
                )?,
                quantity: render(figure("quantity", "Quantity", &row.quantity))?,
                unit_price: render(figure("unit_price", "Unit price", &row.unit_price))?,
                account: render(
                    Combobox::new(
                        format!("receivables-line-account_id-{}", row.id),
                        format!("{FORM}[account_id_{}]", row.id),
                    )
                    .label("Account")
                    .placeholder("Company default")
                    .wrapper_class("mb-0")
                    .options(account_choices.clone())
                    .value(row.account.clone())
                    .errors(under("account_id", "Account")),
                )?,
                tax: render(
                    field("tax_code_id", "Tax code", &row.tax)
                        .kind(InputKind::Select)
                        .prompt("No tax")
                        .options(tax_choices.clone()),
                )?,
                amount: amounts.map_or_else(|| "—".into(), |(amount, ..)| figures.decimal(amount)),
                remove: render(IconButton {
                    title: "Remove line",
                    context: IconContext::Inline,
                    id: &format!("receivables-line-remove-{}", row.id),
                    button_type: "button",
                    event: "remove_line",
                    name: "line",
                    value: &row.id.to_string(),
                    ..IconButton::new("trash", &format!("Remove line {}", n + 1))
                })?,
            })?);
        }
        let mut controls = Html::EMPTY;
        if self.rows.len() < MAX_LINES {
            controls.push(&render(Button {
                id: "receivables-lines-add",
                button_type: "button",
                event: "add_line",
                body: Html::text("Add line"),
                ..Button::default()
            })?);
        }
        if !self.pasting {
            controls.push(&render(Button {
                id: "receivables-lines-paste-open",
                button_type: "button",
                event: "paste_open",
                body: Html::text("Paste from a spreadsheet"),
                ..Button::default()
            })?);
        }
        let mut paste = Html::EMPTY;
        if let Some((kind, message)) = &self.note {
            paste.push(&render(Alert {
                kind: *kind,
                id: "receivables-lines-paste-note",
                class: "mt-3",
                body: Html::text(message),
            })?);
        }
        if self.pasting {
            paste.push(&render(
                Field::in_form(FORM, "paste")
                    .id("receivables-lines-paste")
                    .label("Rows copied from a spreadsheet")
                    .kind(InputKind::Textarea)
                    .wrapper_class("mt-3 mb-0")
                    .hint("One row for each line: description, quantity, unit price.")
                    .value(&self.paste),
            )?);
            let mut buttons = render(Button {
                id: "receivables-lines-paste-cancel",
                button_type: "button",
                event: "paste_close",
                body: Html::text("Cancel paste"),
                ..Button::default()
            })?;
            buttons.push(&render(Button {
                id: "receivables-lines-paste-add",
                button_type: "button",
                event: "paste_lines",
                body: Html::text("Add these lines"),
                ..Button::default()
            })?);
            paste.push(&render(Controls {
                id: "receivables-lines-paste-actions",
                buttons,
            })?);
        }
        body.push(&render(Controls {
            id: "receivables-lines-controls",
            buttons: controls,
        })?);
        body.push(&paste);
        if places.is_some() {
            let tax = tax_totals
                .iter()
                .map(|(id, total)| {
                    let name = taxes.iter().find(|t| t.id == *id).map_or_else(
                        || "Tax".to_owned(),
                        |t| format!("{} {}%", t.code, f::decimal(t.rate)),
                    );
                    (name, figures.decimal(*total))
                })
                .collect();
            let total = subtotal + tax_totals.iter().map(|(_, total)| *total).sum::<Decimal>();
            body.push(&render(Totals {
                subtotal: figures.decimal(subtotal),
                tax,
                label: if complete { "Total" } else { "Total so far" },
                currency: &figures.currency,
                total: figures.decimal(total),
            })?);
        }
        Ok(body)
    }
}

#[derive(Template)]
#[template(
    source = r#"<div id="receivables-line-{{ id }}" class="grid grid-cols-1 items-start gap-2 sm:grid-cols-4{% if !first %} mt-3 border-t border-line pt-3{% endif %}"><div class="sm:col-span-2">{{ description|safe }}</div>{{ quantity|safe }}{{ unit_price|safe }}<div class="sm:col-span-2">{{ account|safe }}</div>{{ tax|safe }}<div class="flex items-center justify-between gap-2 pt-2 text-sm"><span class="text-ink-muted">Amount</span><span class="flex items-center gap-2"><span id="receivables-line-amount-{{ id }}" class="font-medium tabular-nums">{{ amount }}</span>{{ remove|safe }}</span></div></div>"#,
    ext = "html"
)]
struct LineRow {
    id: u32,
    first: bool,
    description: Html,
    quantity: Html,
    unit_price: Html,
    account: Html,
    tax: Html,
    amount: String,
    remove: Html,
}

#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" class="mt-3 flex flex-wrap items-center gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct Controls<'a> {
    id: &'a str,
    buttons: Html,
}

#[derive(Template)]
#[template(
    source = r#"<dl id="receivables-lines-totals" class="mt-3 ml-auto w-64 max-w-full text-sm"><div class="flex justify-between gap-2 py-1"><dt class="text-ink-muted">Subtotal</dt><dd id="receivables-lines-subtotal" class="tabular-nums">{{ subtotal }}</dd></div>{% for (name, amount) in tax %}<div class="flex justify-between gap-2 py-1"><dt class="text-ink-muted">{{ name }}</dt><dd class="tabular-nums">{{ amount }}</dd></div>{% endfor %}<div class="flex justify-between gap-2 border-t border-line py-1 font-semibold"><dt>{{ label }}{% if !currency.is_empty() %} ({{ currency }}){% endif %}</dt><dd id="receivables-lines-total" class="tabular-nums">{{ total }}</dd></div></dl>"#,
    ext = "html"
)]
struct Totals<'a> {
    subtotal: String,
    tax: Vec<(String, String)>,
    label: &'static str,
    currency: &'a str,
    total: String,
}
