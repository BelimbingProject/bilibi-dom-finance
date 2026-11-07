//! A range pasted from a spreadsheet into the opening balances grid. The
//! paste is read as rows (`crate::columns`), its columns are guessed from the
//! header words (account code or name, debit and credit, or one balance), and
//! the person sees the first rows and how they would fill the grid, corrects a
//! column if needed, then fills the grid. Nothing is submitted: the grid is
//! still what the person checks and submits. This is the same column step as
//! the bank statement import's first file.
use super::super::{render, view};
use super::grid::{Draft, Side};
use crate::{
    columns::{self, Role, Sheet},
    MinorUnits, Money,
};
use bilibi_database::{Error, Result};
use bilibi_ui::{
    Alert, BuiltinColumn, Cell, Choice, Field, FlexRow, FlexTable, Form, Html, InputKind,
    ListColumns, StatusKind, Toggle,
};
use std::collections::HashMap;

/// The mapping form's name.
pub(super) const FORM: &str = "opening_columns";
/// How many rows the preview shows.
const SHOWN: usize = 5;

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
    words: &["balance", "net", "amount", "closing"],
};
const CODE: Role = Role {
    key: "code",
    words: &[
        "code",
        "account no",
        "account number",
        "acct",
        "no",
        "number",
        "gl",
    ],
};
const NAME: Role = Role {
    key: "name",
    words: &["account", "name", "description", "particular"],
};

/// Whether amounts come as a debit and a credit column or one signed balance.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    Sides,
    Balance,
}

pub(super) struct Paste {
    sheet: Sheet,
    pub headed: bool,
    account: Option<usize>,
    layout: Layout,
    debit: Option<usize>,
    credit: Option<usize>,
    balance: Option<usize>,
    /// Which side a positive balance is.
    positive: Side,
    mark: char,
    /// The chart's account codes, to tell a code column from an amount.
    codes: Vec<String>,
    /// Why the paste is not read as a table at all.
    pub refusal: Option<String>,
}

/// One pasted row as it would fill the grid.
pub(super) enum Row<'a> {
    Fills(&'a view::Account, Money),
    /// No amount: a heading, a blank or a zero row.
    Skipped,
    /// The row's account cell and why it is left out.
    Left(String, String),
}

impl Paste {
    pub(super) fn new(text: &str, codes: Vec<String>) -> Self {
        let mut paste = Self {
            sheet: Sheet::default(),
            headed: false,
            account: None,
            layout: Layout::Sides,
            debit: None,
            credit: None,
            balance: None,
            positive: Side::Debit,
            mark: '.',
            codes,
            refusal: None,
        };
        match Sheet::read(text) {
            Ok(sheet) if sheet.rows.is_empty() => {
                paste.refusal = Some("Nothing was pasted.".into());
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

    /// The first guess: header words, then the shape of the values.
    fn guess(&mut self) {
        let names = self.sheet.names(self.headed);
        let found = if self.headed {
            columns::by_words(&names, &[DEBIT, CREDIT, BALANCE, CODE, NAME])
        } else {
            Default::default()
        };
        let code_like = |i: &usize| {
            let cells = self.sheet.cells(self.headed, *i);
            !cells.is_empty()
                && cells.iter().all(|cell| {
                    cell.chars().all(|c| c.is_ascii_digit())
                        && self
                            .codes
                            .iter()
                            .any(|code| code.eq_ignore_ascii_case(cell))
                })
        };
        self.account = found
            .get("code")
            .or_else(|| found.get("name"))
            .copied()
            .or_else(|| (0..self.sheet.width()).find(code_like));
        let amounts: Vec<usize> = self
            .sheet
            .numeric_columns(self.headed, '.')
            .into_iter()
            .chain(self.sheet.numeric_columns(self.headed, ','))
            .filter(|i| Some(*i) != self.account)
            .collect();
        let first_figures = |n: usize| -> Vec<usize> {
            let mut unique: Vec<usize> = Vec::new();
            for i in &amounts {
                if !unique.contains(i) {
                    unique.push(*i);
                }
            }
            unique.sort_unstable();
            unique.into_iter().rev().take(n).rev().collect()
        };
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
            _ => match first_figures(2).as_slice() {
                [d, c] => {
                    self.layout = Layout::Sides;
                    (self.debit, self.credit) = (Some(*d), Some(*c));
                }
                [b] => {
                    self.layout = Layout::Balance;
                    self.balance = Some(*b);
                }
                _ => {}
            },
        }
        if self.account.is_none() {
            let taken = [self.debit, self.credit, self.balance];
            self.account = (0..self.sheet.width()).find(|i| !taken.contains(&Some(*i)));
        }
        let cells: Vec<&str> = [self.debit, self.credit, self.balance]
            .into_iter()
            .flatten()
            .flat_map(|i| self.sheet.cells(self.headed, i))
            .collect();
        self.mark = columns::decimal_mark(cells);
    }

    /// Takes the mapping form's choices.
    pub(super) fn choose(&mut self, values: &HashMap<String, String>) {
        let column = |key: &str| {
            values
                .get(key)
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|i| *i < self.sheet.width())
        };
        self.account = column("account");
        self.layout = if values.get("layout").map(String::as_str) == Some("balance") {
            Layout::Balance
        } else {
            Layout::Sides
        };
        self.debit = column("debit");
        self.credit = column("credit");
        self.balance = column("balance");
        self.positive = if values.get("positive").map(String::as_str) == Some("credit") {
            Side::Credit
        } else {
            Side::Debit
        };
        self.mark = if values.get("mark").map(String::as_str) == Some(",") {
            ','
        } else {
            '.'
        };
    }

    /// The first row is (or is not) the column names: the columns are guessed again.
    pub(super) fn set_headed(&mut self, headed: bool) {
        self.headed = headed;
        self.guess();
    }

    /// Every pasted row as it would fill the grid.
    pub(super) fn read<'a>(
        &self,
        accounts: &'a [view::Account],
        units: MinorUnits,
    ) -> Vec<Row<'a>> {
        let Some(account_column) = self.account else {
            return Vec::new();
        };
        self.sheet
            .data(self.headed)
            .iter()
            .map(|row| {
                let cell = |i: Option<usize>| i.and_then(|i| row.get(i)).map_or("", String::as_str);
                let figure = |i: Option<usize>| columns::number(cell(i), self.mark);
                let balance = match self.layout {
                    Layout::Sides => match (figure(self.debit), figure(self.credit)) {
                        (Ok(d), Ok(c)) => Ok(d.unwrap_or_default() - c.unwrap_or_default()),
                        _ => Err(columns::NotANumber),
                    },
                    Layout::Balance => figure(self.balance).map(|b| {
                        let b = b.unwrap_or_default();
                        if self.positive == Side::Debit {
                            b
                        } else {
                            -b
                        }
                    }),
                };
                let name = cell(Some(account_column)).to_owned();
                let balance = match balance {
                    Ok(b) if b.is_zero() => return Row::Skipped,
                    Ok(b) => b,
                    Err(_) => return Row::Left(name, "its amount is not a number".into()),
                };
                let balance = match Money::new(balance, units) {
                    Ok(m) => m,
                    Err(e) => return Row::Left(name, format!("its amount {e}")),
                };
                match find(&name, accounts) {
                    Ok(account) => Row::Fills(account, balance),
                    Err(reason) => Row::Left(name, reason),
                }
            })
            .collect()
    }

    /// Fills the grid: each matched account takes the sum of its rows,
    /// replacing what its cells held; other accounts keep theirs. Returns how
    /// many accounts were filled and the rows left out.
    pub(super) fn apply(
        &self,
        draft: &mut Draft,
        accounts: &[view::Account],
        units: MinorUnits,
    ) -> (usize, Vec<(String, String)>) {
        let mut sums: Vec<(i64, Money)> = Vec::new();
        let mut left = Vec::new();
        for row in self.read(accounts, units) {
            match row {
                Row::Fills(account, money) => {
                    match sums.iter_mut().find(|(id, _)| *id == account.id) {
                        Some((_, sum)) => *sum = *sum + money,
                        None => sums.push((account.id, money)),
                    }
                }
                Row::Left(name, reason) => left.push((name, reason)),
                Row::Skipped => {}
            }
        }
        for (account, sum) in &sums {
            draft.set(*account, *sum, units);
        }
        (sums.len(), left)
    }

    /// The column step: the first rows as pasted, the column choices, and
    /// the first rows as they would fill the grid.
    pub(super) fn view(&self, accounts: &[view::Account], units: MinorUnits) -> Result<Html> {
        let mut body = Html::EMPTY;
        if let Some(refusal) = &self.refusal {
            return render(Alert {
                id: "opening-paste-refusal",
                kind: StatusKind::Error,
                body: Html::text(refusal),
                ..Alert::default()
            });
        }
        let names = self.sheet.names(self.headed);
        let builtins: Vec<BuiltinColumn> = names
            .iter()
            .enumerate()
            .map(|(i, name)| BuiltinColumn::new(&format!("c{i}"), name))
            .collect();
        let pasted =
            ListColumns::mount("opening-paste-rows", builtins).map_err(|_| Error::Internal)?;
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
        let mut table = FlexTable::new("opening-paste-rows", pasted.flex_columns(), rows);
        table.caption = "What you pasted";
        body.push(&render(Heading {
            text: "What you pasted",
        })?);
        body.push(&render(table)?);
        let toggle = render(Toggle {
            on: self.headed,
            label: "The first row holds the column names",
            id: "opening-paste-headed",
            event: "paste_headed",
            name: "on",
            value: if self.headed { "false" } else { "true" },
            ..Toggle::default()
        })?;
        body.push(&render(Labelled {
            label: "The first row holds the column names",
            control: toggle,
        })?);
        body.push(&render(Form {
            id: "opening-columns-form",
            event: "paste_columns",
            change: true,
            class: "mt-4",
            body: render(FieldGrid {
                fields: self.fields(&names)?,
            })?,
            actions: Html::EMPTY,
        })?);
        let read = self.read(accounts, units);
        let filled = read.iter().filter(|r| matches!(r, Row::Fills(..))).count();
        let left: Vec<&Row> = read.iter().filter(|r| matches!(r, Row::Left(..))).collect();
        let result = ListColumns::mount(
            "opening-paste-result",
            vec![
                BuiltinColumn::new("pasted", "Pasted account"),
                BuiltinColumn::new("account", "Fills"),
                BuiltinColumn::new("debit", "Debit").right(),
                BuiltinColumn::new("credit", "Credit").right(),
            ],
        )
        .map_err(|_| Error::Internal)?;
        let mut rows = Vec::new();
        for (n, row) in read
            .iter()
            .zip(self.sheet.data(self.headed))
            .filter(|(r, _)| !matches!(r, Row::Skipped))
            .take(SHOWN)
            .enumerate()
        {
            let pasted = self
                .account
                .and_then(|i| row.1.get(i))
                .cloned()
                .unwrap_or_default();
            let mut line = FlexRow::new(n.to_string()).cell("pasted", Cell::text(&pasted));
            match row.0 {
                Row::Fills(account, money) => {
                    let (debit, credit) = if money.is_positive() {
                        (money.display(units), String::new())
                    } else {
                        (String::new(), money.abs().display(units))
                    };
                    line = line
                        .cell("account", Cell::text(view::account_label(account)))
                        .slot("account", super::super::account_name(account)?)
                        .cell("debit", Cell::text(debit))
                        .cell("credit", Cell::text(credit));
                }
                Row::Left(_, reason) => {
                    line = line.cell("account", Cell::text(format!("Left out: {reason}")));
                }
                Row::Skipped => {}
            }
            rows.push(line);
        }
        let mut table = FlexTable::new("opening-paste-result", result.flex_columns(), rows);
        table.caption = "How the first rows fill the grid";
        table.empty = Some(bilibi_ui::TableEmpty::State(
            bilibi_ui::EmptyState::missing(
                "Nothing fills the grid yet",
                "Choose the account column and the amount columns above.",
            ),
        ));
        body.push(&render(Heading {
            text: &format!(
                "How they fill the grid: {filled} {} with an amount",
                if filled == 1 { "row" } else { "rows" }
            ),
        })?);
        body.push(&render(table)?);
        if !left.is_empty() {
            let listed: Vec<String> = left
                .iter()
                .take(8)
                .filter_map(|r| match r {
                    Row::Left(name, reason) if name.is_empty() => {
                        Some(format!("a row with no account ({reason})"))
                    }
                    Row::Left(name, reason) => Some(format!("“{name}” ({reason})")),
                    _ => None,
                })
                .collect();
            let more = left.len().saturating_sub(listed.len());
            body.push(&render(Alert {
                id: "opening-paste-left",
                kind: StatusKind::Warning,
                body: Html::text(&format!(
                    "{} {} left out: {}{}. Add the account to the chart, or type its amount in the grid.",
                    left.len(),
                    if left.len() == 1 { "row is" } else { "rows are" },
                    listed.join("; "),
                    if more > 0 { format!("; and {more} more") } else { String::new() }
                )),
                ..Alert::default()
            })?);
        }
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
                .id(format!("opening-columns-{key}"))
                .label(label)
                .kind(InputKind::Select)
                .prompt("Choose a column")
                .options(options.clone())
                .value(chosen.map(|i| i.to_string()).unwrap_or_default())
                .required()
        };
        let mut fields = vec![
            column("account", "Account code or name", self.account),
            Field::in_form(FORM, "layout")
                .id("opening-columns-layout")
                .label("Amounts are in")
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("A debit and a credit column", "sides"),
                    Choice::new("One balance column", "balance"),
                ])
                .value(match self.layout {
                    Layout::Sides => "sides",
                    Layout::Balance => "balance",
                }),
        ];
        match self.layout {
            Layout::Sides => {
                fields.push(column("debit", "Debit", self.debit));
                fields.push(column("credit", "Credit", self.credit));
            }
            Layout::Balance => {
                fields.push(column("balance", "Balance", self.balance));
                fields.push(
                    Field::in_form(FORM, "positive")
                        .id("opening-columns-positive")
                        .label("A positive balance is")
                        .kind(InputKind::Select)
                        .options(vec![
                            Choice::new("A debit", "debit"),
                            Choice::new("A credit", "credit"),
                        ])
                        .value(self.positive.key()),
                );
            }
        }
        fields.push(
            Field::in_form(FORM, "mark")
                .id("opening-columns-mark")
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

/// The account a pasted cell names: its code, else its name (when only one
/// account has it), else a code at the start of the cell ("1000 Cash",
/// "1000 - Cash"). Only an account that takes postings is filled.
fn find<'a>(
    cell: &str,
    accounts: &'a [view::Account],
) -> std::result::Result<&'a view::Account, String> {
    let cell = cell.trim();
    if cell.is_empty() {
        return Err("it names no account".into());
    }
    let by_code = |code: &str| accounts.iter().find(|a| a.code.eq_ignore_ascii_case(code));
    let named: Vec<&view::Account> = accounts
        .iter()
        .filter(|a| a.name.trim().eq_ignore_ascii_case(cell))
        .collect();
    let leading = cell
        .split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .filter(|code| *code != cell)
        .and_then(by_code);
    let account = match (by_code(cell), named.as_slice(), leading) {
        (Some(a), _, _) => a,
        (None, [a], _) => a,
        (None, [_, _, ..], _) => return Err("more than one account has this name".into()),
        (None, [], Some(a)) => a,
        (None, [], None) => return Err("no account has this code or name".into()),
    };
    if account.is_group {
        return Err(format!(
            "{} is an account group, which takes no balance",
            view::account_label(account)
        ));
    }
    if !account.active {
        return Err(format!("{} is inactive", view::account_label(account)));
    }
    Ok(account)
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
