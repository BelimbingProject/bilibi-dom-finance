//! A source schedule pasted from a spreadsheet into the "+ Schedule" dialog.
//! The paste is read as rows (`bilibi_finance_ledger::columns`), its columns
//! are guessed from the header words and the cells, and the person sees the
//! first rows as pasted and as they would be kept, corrects a column if
//! needed, then previews the whole schedule through the import contract.
//! Nothing is kept until that review is confirmed. This is the same column
//! step as the opening balances grid's paste.
use super::render;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{
    columns::{self, DateOrder, Role, Sheet},
    MinorUnits, Money,
};
use bilibi_ui::{
    BuiltinColumn, Cell, Choice, Field, FlexRow, FlexTable, Html, InputKind, ListColumns,
};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The column form's name.
pub(super) const FORM: &str = "schedule_columns";
/// How many rows the column step shows.
const SHOWN: usize = 5;
/// The most rows one schedule takes (the import contract's limit).
pub(super) const MOST: usize = 500;

const SOURCE: Role = Role {
    key: "source",
    words: &["source id", "source", "line", "row"],
};
const DEBIT: Role = Role {
    key: "debit",
    words: &["debit", "dr"],
};
const CREDIT: Role = Role {
    key: "credit",
    words: &["credit", "cr"],
};
const BALANCE: Role = Role {
    key: "balance",
    words: &["balance", "net", "movement", "amount", "closing"],
};
const CODE: Role = Role {
    key: "code",
    words: &["code", "account no", "account number", "acct", "gl"],
};
const NAME: Role = Role {
    key: "name",
    words: &["account", "name", "description", "particular"],
};
const DATE: Role = Role {
    key: "date",
    words: &["date", "filed on", "paid on"],
};
const AMOUNT: Role = Role {
    key: "amount",
    words: &["amount", "tax", "total", "paid", "remitted"],
};
const REFERENCE: Role = Role {
    key: "reference",
    words: &[
        "reference",
        "ref",
        "receipt",
        "acknowledg",
        "filing",
        "payment no",
    ],
};
const AUTHORITY: Role = Role {
    key: "authority",
    words: &["authority", "agency", "payee", "department", "fund"],
};

/// A chart account a pasted row may name.
#[derive(Clone)]
pub(super) struct Account {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub is_group: bool,
}

/// Whether amounts come as a debit and a credit column or one signed figure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Sides,
    Balance,
}

/// One pasted row as it would be kept.
pub(super) enum Row {
    /// The import contract's row, with what the column step shows of it.
    Keep {
        input: Value,
        item: String,
        side: String,
        date: String,
        amount: Money,
    },
    /// No figure: a heading, a blank or a zero row of account movements.
    Skipped,
    /// Why the row is left out.
    Left(String),
}

pub(super) struct Paste {
    sheet: Sheet,
    pub headed: bool,
    /// Rows name an account and a side (comparatives, monthly movements);
    /// otherwise a filing or remittance (filed SST, payroll remittances).
    accounts: bool,
    source: Option<usize>,
    account: Option<usize>,
    layout: Layout,
    debit: Option<usize>,
    credit: Option<usize>,
    balance: Option<usize>,
    /// A positive figure is a debit (or a credit).
    positive_debit: bool,
    reference: Option<usize>,
    authority: Option<usize>,
    date: Option<usize>,
    amount: Option<usize>,
    mark: char,
    order: DateOrder,
    /// Why the paste is not read as a table at all.
    pub refusal: Option<String>,
}

impl Paste {
    pub(super) fn new(text: &str, accounts: bool) -> Self {
        let mut paste = Self {
            sheet: Sheet::default(),
            headed: false,
            accounts,
            source: None,
            account: None,
            layout: Layout::Sides,
            debit: None,
            credit: None,
            balance: None,
            positive_debit: true,
            reference: None,
            authority: None,
            date: None,
            amount: None,
            mark: '.',
            order: DateOrder::DayFirst,
            refusal: None,
        };
        match Sheet::read(text) {
            Ok(sheet) if sheet.rows.is_empty() => {
                paste.refusal = Some("Nothing was pasted.".into())
            }
            Ok(sheet) => {
                paste.headed = sheet.looks_headed();
                paste.sheet = sheet;
                paste.guess();
            }
            Err(error) => paste.refusal = Some(error.to_string()),
        }
        paste
    }

    /// The first guess: header words, then the shape of the cells.
    fn guess(&mut self) {
        let names = self.sheet.names(self.headed);
        let roles: &[Role] = if self.accounts {
            &[SOURCE, DEBIT, CREDIT, BALANCE, CODE, NAME]
        } else {
            &[SOURCE, DATE, AMOUNT, REFERENCE, AUTHORITY]
        };
        let found = if self.headed {
            columns::by_words(&names, roles)
        } else {
            Default::default()
        };
        let numeric: Vec<usize> = {
            let mut all = self.sheet.numeric_columns(self.headed, '.');
            for i in self.sheet.numeric_columns(self.headed, ',') {
                if !all.contains(&i) {
                    all.push(i);
                }
            }
            all.sort_unstable();
            all
        };
        let width = self.sheet.width();
        self.source = found.get("source").copied();
        let taken = |chosen: &[Option<usize>], i: usize| chosen.contains(&Some(i));
        if self.accounts {
            self.account = found.get("code").or_else(|| found.get("name")).copied();
            match (
                found.get("debit"),
                found.get("credit"),
                found.get("balance"),
            ) {
                (Some(d), Some(c), _) => {
                    self.layout = Layout::Sides;
                    (self.debit, self.credit) = (Some(*d), Some(*c));
                }
                (_, _, Some(b)) => {
                    self.layout = Layout::Balance;
                    self.balance = Some(*b);
                }
                _ => {
                    let figures: Vec<usize> = numeric
                        .iter()
                        .copied()
                        .filter(|i| Some(*i) != self.account && Some(*i) != self.source)
                        .collect();
                    match figures.as_slice() {
                        [.., d, c] => {
                            self.layout = Layout::Sides;
                            (self.debit, self.credit) = (Some(*d), Some(*c));
                        }
                        [b] => {
                            self.layout = Layout::Balance;
                            self.balance = Some(*b);
                        }
                        [] => {}
                    }
                }
            }
            if self.account.is_none() {
                let chosen = [self.debit, self.credit, self.balance, self.source];
                self.account = (0..width).find(|i| !taken(&chosen, *i));
            }
        } else {
            // The column with the most cells read as dates, so one row's
            // unreadable date is shown as left out, not the whole column.
            self.date = found.get("date").copied().or_else(|| {
                (0..width)
                    .map(|i| {
                        let dates = self
                            .sheet
                            .cells(self.headed, i)
                            .iter()
                            .filter(|c| {
                                columns::date(c, DateOrder::DayFirst).is_some()
                                    || columns::date(c, DateOrder::MonthFirst).is_some()
                            })
                            .count();
                        (dates, i)
                    })
                    .filter(|(dates, _)| *dates > 0)
                    .max_by_key(|(dates, i)| (*dates, std::cmp::Reverse(*i)))
                    .map(|(_, i)| i)
            });
            self.amount = found.get("amount").copied().or_else(|| {
                numeric
                    .iter()
                    .rev()
                    .copied()
                    .find(|i| Some(*i) != self.date && Some(*i) != self.source)
            });
            self.reference = found.get("reference").copied();
            self.authority = found.get("authority").copied();
            let mut chosen = vec![
                self.source,
                self.date,
                self.amount,
                self.reference,
                self.authority,
            ];
            if self.reference.is_none() {
                self.reference = (0..width).find(|i| !taken(&chosen, *i));
                chosen.push(self.reference);
            }
            if self.authority.is_none() {
                self.authority = (0..width).find(|i| !taken(&chosen, *i));
            }
            if let Some(i) = self.date {
                self.order = columns::date_order(self.sheet.cells(self.headed, i));
            }
        }
        let figures: Vec<&str> = [self.debit, self.credit, self.balance, self.amount]
            .into_iter()
            .flatten()
            .flat_map(|i| self.sheet.cells(self.headed, i))
            .collect();
        self.mark = columns::decimal_mark(figures);
    }

    /// The first row is (or is not) the column names: the columns are guessed again.
    pub(super) fn set_headed(&mut self, headed: bool) {
        self.headed = headed;
        self.guess();
    }

    /// Takes the column form's choices. "None" for the source column numbers
    /// the rows instead.
    pub(super) fn choose(&mut self, values: &HashMap<String, String>) {
        let width = self.sheet.width();
        let column = |key: &str| {
            values
                .get(key)
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|i| *i < width)
        };
        self.source = column("source");
        self.mark = if values.get("mark").map(String::as_str) == Some(",") {
            ','
        } else {
            '.'
        };
        if self.accounts {
            self.account = column("account");
            self.layout = if values.get("layout").map(String::as_str) == Some("balance") {
                Layout::Balance
            } else {
                Layout::Sides
            };
            self.debit = column("debit");
            self.credit = column("credit");
            self.balance = column("balance");
            self.positive_debit = values.get("positive").map(String::as_str) != Some("credit");
        } else {
            self.reference = column("reference");
            self.authority = column("authority");
            self.date = column("date");
            self.amount = column("amount");
            self.order = if values.get("order").map(String::as_str) == Some("month") {
                DateOrder::MonthFirst
            } else {
                DateOrder::DayFirst
            };
        }
    }

    /// Every pasted row, with its row number in the paste (1 for the first
    /// row below any header), as it would be kept.
    pub(super) fn rows(&self, accounts: &[Account], units: MinorUnits) -> Vec<(usize, Row)> {
        self.sheet
            .data(self.headed)
            .iter()
            .enumerate()
            .map(|(n, row)| (n + 1, self.row(n + 1, row, accounts, units)))
            .collect()
    }

    fn row(&self, number: usize, row: &[String], accounts: &[Account], units: MinorUnits) -> Row {
        let cell = |i: Option<usize>| i.and_then(|i| row.get(i)).map_or("", |c| c.as_str()).trim();
        let source_id = match cell(self.source) {
            "" => format!("row {number}"),
            id => id.to_owned(),
        };
        let figure = |i: Option<usize>| columns::number(cell(i), self.mark);
        if self.accounts {
            let net = match self.layout {
                Layout::Sides => match (figure(self.debit), figure(self.credit)) {
                    (Ok(d), Ok(c)) => Ok(d.unwrap_or_default() - c.unwrap_or_default()),
                    _ => Err(columns::NotANumber),
                },
                Layout::Balance => figure(self.balance).map(|b| {
                    let b = b.unwrap_or_default();
                    if self.positive_debit {
                        b
                    } else {
                        -b
                    }
                }),
            };
            let net = match net {
                Ok(net) if net.is_zero() => return Row::Skipped,
                Ok(net) => net,
                Err(_) => return Row::Left("its amount is not a number".into()),
            };
            let amount = match Money::new(net.abs(), units) {
                Ok(amount) => amount,
                Err(error) => return Row::Left(format!("its amount {error}")),
            };
            let account =
                match columns::pick(cell(self.account), accounts, |a| &a.code, |a| &a.name) {
                    Ok(account) if account.is_group => {
                        return Row::Left(format!(
                            "{} · {} is an account group",
                            account.name, account.code
                        ))
                    }
                    Ok(account) => account,
                    Err(columns::Pick::Blank) => return Row::Left("it names no account".into()),
                    Err(columns::Pick::Ambiguous) => {
                        return Row::Left("more than one account has this name".into())
                    }
                    Err(columns::Pick::Unknown) => {
                        return Row::Left("no account has this code or name".into())
                    }
                };
            let side = if net > Decimal::ZERO {
                "debit"
            } else {
                "credit"
            };
            Row::Keep {
                input: json!({"source_id":source_id,"account_id":account.id,"side":side,"amount":amount.shown(units)}),
                item: format!("{} · {}", account.name, account.code),
                side: if side == "debit" { "Debit" } else { "Credit" }.into(),
                date: String::new(),
                amount,
            }
        } else {
            let amount = match figure(self.amount) {
                Ok(Some(amount)) if amount < Decimal::ZERO => {
                    return Row::Left("its amount is negative".into())
                }
                Ok(Some(amount)) => amount,
                Ok(None) => return Row::Left("it has no amount".into()),
                Err(_) => return Row::Left("its amount is not a number".into()),
            };
            let amount = match Money::new(amount, units) {
                Ok(amount) => amount,
                Err(error) => return Row::Left(format!("its amount {error}")),
            };
            let (reference, authority) = (cell(self.reference), cell(self.authority));
            if reference.is_empty() {
                return Row::Left("it has no reference".into());
            }
            if authority.is_empty() {
                return Row::Left("it names no authority".into());
            }
            let Some(date) = columns::date(cell(self.date), self.order) else {
                return Row::Left("its date is not a date".into());
            };
            Row::Keep {
                input: json!({"source_id":source_id,"reference":reference,"authority":authority,"event_date":date.to_string(),"amount":amount.shown(units)}),
                item: reference.to_owned(),
                side: authority.to_owned(),
                date: date.to_string(),
                amount,
            }
        }
    }

    /// The column step: the first rows as pasted, the column choices, and
    /// the first rows as they would be kept.
    pub(super) fn view(&self, accounts: &[Account], units: MinorUnits) -> Result<Html> {
        if let Some(refusal) = &self.refusal {
            return render(bilibi_ui::Alert {
                id: "source-period-paste-refusal",
                kind: bilibi_ui::StatusKind::Error,
                body: Html::text(refusal),
                ..bilibi_ui::Alert::default()
            });
        }
        let mut body = Html::EMPTY;
        let names = self.sheet.names(self.headed);
        let pasted = ListColumns::mount(
            "source-period-paste-rows",
            names
                .iter()
                .enumerate()
                .map(|(i, name)| BuiltinColumn::new(&format!("c{i}"), name))
                .collect(),
        )
        .map_err(|_| Error::Internal)?;
        let rows = self
            .sheet
            .data(self.headed)
            .iter()
            .take(SHOWN)
            .enumerate()
            .map(|(n, row)| {
                (0..names.len()).fold(FlexRow::new(n.to_string()), |r, i| {
                    r.cell(
                        &format!("c{i}"),
                        Cell::text(row.get(i).cloned().unwrap_or_default()),
                    )
                })
            })
            .collect();
        let mut table = FlexTable::new("source-period-paste-rows", pasted.flex_columns(), rows);
        table.caption = "What you pasted";
        body.push(&render(Heading {
            text: "What you pasted",
        })?);
        body.push(&render(table)?);
        let toggle = render(bilibi_ui::Toggle {
            on: self.headed,
            label: "The first row holds the column names",
            id: "source-period-paste-headed",
            event: "paste_headed",
            name: "on",
            value: if self.headed { "false" } else { "true" },
            ..bilibi_ui::Toggle::default()
        })?;
        body.push(&render(Labelled {
            label: "The first row holds the column names",
            control: toggle,
        })?);
        body.push(&render(bilibi_ui::Form {
            id: "source-period-columns-form",
            event: "paste_columns",
            change: true,
            class: "mt-4",
            body: render(FieldGrid {
                fields: self.fields(&names)?,
            })?,
            actions: Html::EMPTY,
        })?);
        let read = self.rows(accounts, units);
        let kept = read
            .iter()
            .filter(|(_, r)| matches!(r, Row::Keep { .. }))
            .count();
        let left = read
            .iter()
            .filter(|(_, r)| matches!(r, Row::Left(_)))
            .count();
        let mut columns = vec![
            BuiltinColumn::new("row", "Row"),
            BuiltinColumn::new(
                "item",
                if self.accounts {
                    "Account"
                } else {
                    "Reference"
                },
            ),
            BuiltinColumn::new("side", if self.accounts { "Side" } else { "Authority" }),
        ];
        if !self.accounts {
            columns.push(BuiltinColumn::new("date", "Date"));
        }
        columns.push(BuiltinColumn::new("amount", "Amount").right());
        let result = ListColumns::mount("source-period-paste-result", columns)
            .map_err(|_| Error::Internal)?;
        let mut lines = Vec::new();
        for (number, row) in read
            .iter()
            .filter(|(_, r)| !matches!(r, Row::Skipped))
            .take(SHOWN)
        {
            let line = FlexRow::new(number.to_string()).cell("row", Cell::text(number.to_string()));
            lines.push(match row {
                Row::Keep {
                    item,
                    side,
                    date,
                    amount,
                    ..
                } => line
                    .cell("item", Cell::text(item))
                    .cell("side", Cell::text(side))
                    .cell(
                        "date",
                        Cell::text(bilibi_finance_ledger::dates::display_text(date)),
                    )
                    .cell("amount", Cell::text(amount.display(units))),
                Row::Left(reason) => line.cell("item", Cell::text(format!("Left out: {reason}"))),
                Row::Skipped => line,
            });
        }
        let mut table = FlexTable::new("source-period-paste-result", result.flex_columns(), lines);
        table.caption = "How the first rows are read";
        table.empty = Some(bilibi_ui::TableEmpty::State(
            bilibi_ui::EmptyState::missing("No row has an amount in the chosen columns", "")
                .id("source-period-paste-result-empty"),
        ));
        body.push(&render(Heading {
            text: "How the first rows are read",
        })?);
        body.push(&render(table)?);
        let mut summary = format!(
            "{kept} {} kept.",
            if kept == 1 { "row is" } else { "rows are" }
        );
        if left > 0 {
            summary.push_str(&format!(
                " {left} {} left out; correct the columns or the source report and paste again.",
                if left == 1 { "row is" } else { "rows are" }
            ));
        }
        if kept > MOST {
            summary = format!("{kept} rows: a schedule takes at most {MOST}. Split the source report into schedules of {MOST} rows or fewer.");
        }
        body.push(&render(bilibi_ui::Alert {
            id: "source-period-paste-summary",
            kind: if left > 0 || kept > MOST || kept == 0 {
                bilibi_ui::StatusKind::Warning
            } else {
                bilibi_ui::StatusKind::Info
            },
            body: Html::text(&summary),
            ..bilibi_ui::Alert::default()
        })?);
        Ok(body)
    }

    fn fields(&self, names: &[String]) -> Result<Html> {
        let options: Vec<Choice> = names
            .iter()
            .enumerate()
            .map(|(i, name)| Choice::new(name, i.to_string()))
            .collect();
        let column = |key: &str, label: &str, chosen: Option<usize>| {
            Field::in_form(FORM, key)
                .id(format!("source-period-columns-{key}"))
                .label(label)
                .kind(InputKind::Select)
                .prompt("Choose a column")
                .options(options.clone())
                .value(chosen.map(|i| i.to_string()).unwrap_or_default())
                .required()
        };
        let mut sources = vec![Choice::new("None: number the rows", "")];
        sources.extend(options.iter().cloned());
        let mut fields = vec![Field::in_form(FORM, "source")
            .id("source-period-columns-source")
            .label("Row reference")
            .kind(InputKind::Select)
            .options(sources)
            .value(self.source.map(|i| i.to_string()).unwrap_or_default())];
        if self.accounts {
            fields.push(column("account", "Account code or name", self.account));
            fields.push(
                Field::in_form(FORM, "layout")
                    .id("source-period-columns-layout")
                    .label("Amounts are in")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("A debit and a credit column", "sides"),
                        Choice::new("One amount column", "balance"),
                    ])
                    .value(if self.layout == Layout::Balance {
                        "balance"
                    } else {
                        "sides"
                    }),
            );
            if self.layout == Layout::Sides {
                fields.push(column("debit", "Debit", self.debit));
                fields.push(column("credit", "Credit", self.credit));
            } else {
                fields.push(column("balance", "Amount", self.balance));
                fields.push(
                    Field::in_form(FORM, "positive")
                        .id("source-period-columns-positive")
                        .label("A positive amount is")
                        .kind(InputKind::Select)
                        .options(vec![
                            Choice::new("A debit", "debit"),
                            Choice::new("A credit", "credit"),
                        ])
                        .value(if self.positive_debit {
                            "debit"
                        } else {
                            "credit"
                        }),
                );
            }
        } else {
            fields.push(column(
                "reference",
                "Filing or payment reference",
                self.reference,
            ));
            fields.push(column("authority", "Authority", self.authority));
            fields.push(column("date", "Filing or payment date", self.date));
            fields.push(
                Field::in_form(FORM, "order")
                    .id("source-period-columns-order")
                    .label("Dates are written as")
                    .kind(InputKind::Select)
                    .options(vec![
                        Choice::new("Day first (15/05/2026)", "day"),
                        Choice::new("Month first (05/15/2026)", "month"),
                    ])
                    .value(if self.order == DateOrder::MonthFirst {
                        "month"
                    } else {
                        "day"
                    }),
            );
            fields.push(column("amount", "Amount", self.amount));
        }
        fields.push(
            Field::in_form(FORM, "mark")
                .id("source-period-columns-mark")
                .label("Decimals are written as")
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("1,234.56", "."),
                    Choice::new("1.234,56", ","),
                ])
                .value(self.mark.to_string()),
        );
        let mut html = Html::EMPTY;
        for field in fields {
            html.push(&render(field.wrapper_class("mb-0"))?);
        }
        Ok(html)
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"<h3 class="mb-2 mt-4 text-sm font-medium text-ink">{{ text }}</h3>"#,
    ext = "html"
)]
struct Heading<'a> {
    text: &'a str,
}

/// A control with its visible label beside it (a `Toggle` names itself only
/// to assistive technology).
#[derive(askama::Template)]
#[template(
    source = r#"<div class="mt-3 flex items-center gap-2 text-sm text-ink">{{ control|safe }}<span>{{ label }}</span></div>"#,
    ext = "html"
)]
struct Labelled<'a> {
    label: &'a str,
    control: Html,
}

#[derive(askama::Template)]
#[template(
    source = r#"<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{{ fields|safe }}</div>"#,
    ext = "html"
)]
struct FieldGrid {
    fields: Html,
}
