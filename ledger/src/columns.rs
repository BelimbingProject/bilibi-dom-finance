//! A table a person pasted from a spreadsheet (or a CSV file), read as rows
//! of cells, and a first guess at which column holds what. A page that maps
//! columns shows the first rows read through the guess and lets the person
//! correct it before anything is kept: the guess only proposes.
//!
//! Shared by the Finance pages that read a person's table: the opening
//! balances grid reads a pasted trial balance with it, and Reconciliation's
//! statement import can adopt it for the same header words and amounts.
//! Nothing here knows an account, a currency or a particular export's layout.
use crate::opening_csv;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// The largest table read, in bytes.
pub const MAX_BYTES: usize = opening_csv::MAX_BYTES;
/// The separators tried, after a tab (a spreadsheet's own copy).
const DELIMITERS: [&str; 3] = [",", ";", "|"];

/// Why pasted text is not read as a table, in a sentence a page shows.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SheetError {
    #[error("The pasted text is larger than 128 KiB.")]
    TooLarge,
    #[error("Row {0} has a quote that is never closed or one in the middle of a cell.")]
    Quote(usize),
}

/// The rows of a pasted table, blank rows dropped, the header row (when the
/// person says there is one) kept as the first row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sheet {
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    /// Reads text copied from a spreadsheet (cells separated by tabs) or a
    /// CSV export (by commas, semicolons or bars, whichever the first lines
    /// share), with quoted cells as a spreadsheet writes them.
    pub fn read(text: &str) -> Result<Self, SheetError> {
        let delimiter = delimiter(text);
        let rows = opening_csv::parse(text.as_bytes(), delimiter).map_err(|error| match error {
            opening_csv::CsvError::TooLarge | opening_csv::CsvError::NotUtf8 => {
                SheetError::TooLarge
            }
            opening_csv::CsvError::StrayQuote(row)
            | opening_csv::CsvError::UnterminatedQuote(row) => SheetError::Quote(row),
        })?;
        Ok(Self {
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(|cell| cell.trim().to_owned()).collect())
                .collect(),
        })
    }

    /// The number of columns of the widest row.
    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Whether the first row looks like column names: no cell of it is a
    /// number while a later row has one, or it is the only row of words above
    /// rows of figures.
    pub fn looks_headed(&self) -> bool {
        let Some((first, rest)) = self.rows.split_first() else {
            return false;
        };
        let figure =
            |cell: &String| !cell.is_empty() && number(cell, '.').is_ok_and(|n| n.is_some());
        !first.iter().any(figure) && rest.iter().any(|row| row.iter().any(figure))
    }

    /// Each column's name: the header cell, or "Column 3" without a header
    /// (or for a column the header leaves blank).
    pub fn names(&self, headed: bool) -> Vec<String> {
        (0..self.width())
            .map(|i| {
                headed
                    .then(|| self.rows.first().and_then(|row| row.get(i)))
                    .flatten()
                    .filter(|cell| !cell.is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("Column {}", i + 1))
            })
            .collect()
    }

    /// The rows below the header.
    pub fn data(&self, headed: bool) -> &[Vec<String>] {
        &self.rows[usize::from(headed && !self.rows.is_empty())..]
    }

    /// One column's non-blank cells below the header.
    pub fn cells(&self, headed: bool, column: usize) -> Vec<&str> {
        self.data(headed)
            .iter()
            .filter_map(|row| row.get(column))
            .map(String::as_str)
            .filter(|cell| !cell.is_empty())
            .collect()
    }

    /// The columns whose every non-blank cell is a number (at least one).
    pub fn numeric_columns(&self, headed: bool, mark: char) -> Vec<usize> {
        (0..self.width())
            .filter(|&i| {
                let cells = self.cells(headed, i);
                !cells.is_empty() && cells.iter().all(|cell| number(cell, mark).is_ok())
            })
            .collect()
    }
}

/// What a page looks for in a table: its key and the header words that name
/// it, most specific first.
#[derive(Clone, Copy, Debug)]
pub struct Role {
    pub key: &'static str,
    pub words: &'static [&'static str],
}

/// Each role's column found by its header words, tried in the order given
/// (so "Balance" can be claimed before a looser "Amount"), one column per
/// role. A role no header names is left out, for the caller's own fallback.
pub fn by_words(names: &[String], roles: &[Role]) -> BTreeMap<&'static str, usize> {
    let mut found: BTreeMap<&'static str, usize> = BTreeMap::new();
    for role in roles {
        if let Some(index) = (0..names.len())
            .find(|i| !found.values().any(|taken| taken == i) && named(&names[*i], role.words))
        {
            found.insert(role.key, index);
        }
    }
    found
}

/// Whether a header names one of the words: a word of three letters or fewer
/// only as a word of its own ("Dr", "Ref No"), never inside another
/// ("address"); a longer one anywhere in it ("Debit amount").
pub fn named(header: &str, words: &[&str]) -> bool {
    let header = header.trim().to_lowercase();
    words.iter().any(|word| {
        if word.chars().count() <= 3 {
            header
                .split(|c: char| !c.is_alphanumeric())
                .any(|part| part == *word)
        } else {
            header.contains(word)
        }
    })
}

/// A spreadsheet figure as a decimal: grouping separators, a currency
/// symbol and spaces are dropped; a minus sign before or after it, or
/// parentheses around it, make it negative. `mark` is the decimal mark
/// ('.' or ','). A blank cell or a lone dash is `Ok(None)`; text with letters
/// or more than one decimal mark is `Err`.
pub fn number(cell: &str, mark: char) -> Result<Option<Decimal>, ()> {
    let text: String = cell
        .chars()
        .filter(|c| !c.is_whitespace() && !is_currency_symbol(*c))
        .collect();
    if text.is_empty() || matches!(text.as_str(), "-" | "–" | "—") {
        return Ok(None);
    }
    let (negative, text) =
        if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            (true, inner.to_owned())
        } else if let Some(inner) = text.strip_prefix('-').or_else(|| text.strip_prefix('−')) {
            (true, inner.to_owned())
        } else if let Some(inner) = text.strip_suffix('-') {
            (true, inner.to_owned())
        } else {
            (false, text)
        };
    let grouping = if mark == ',' {
        ['.', '\'']
    } else {
        [',', '\'']
    };
    let plain: String = text.chars().filter(|c| !grouping.contains(c)).collect();
    if plain.is_empty()
        || !plain.chars().all(|c| c.is_ascii_digit() || c == mark)
        || plain.matches(mark).count() > 1
        || !plain.chars().any(|c| c.is_ascii_digit())
    {
        return Err(());
    }
    let value = Decimal::from_str_exact(&plain.replace(mark, ".")).map_err(|_| ())?;
    Ok(Some(if negative { -value } else { value }))
}

/// Whether figures are written with a decimal comma ("1.234,56", "12,5"):
/// some cell ends in a comma and one or two digits, and none in a point and
/// one or two digits.
pub fn decimal_mark<'a>(cells: impl IntoIterator<Item = &'a str>) -> char {
    let ends = |cell: &str, mark: char| {
        let digits = cell.trim().trim_end_matches(|c: char| !c.is_ascii_digit());
        digits
            .rfind(mark)
            .is_some_and(|at| (1..=2).contains(&(digits.len() - at - 1)))
    };
    let (mut comma, mut point) = (false, false);
    for cell in cells {
        comma |= ends(cell, ',');
        point |= ends(cell, '.');
    }
    if comma && !point {
        ','
    } else {
        '.'
    }
}

fn is_currency_symbol(c: char) -> bool {
    matches!(
        c,
        '$' | '€'
            | '£'
            | '¥'
            | '₹'
            | '₩'
            | '₱'
            | '฿'
            | '₫'
            | '₦'
            | '₪'
            | '₺'
            | '₽'
            | '¢'
    )
}

/// A tab when the text has one (a spreadsheet's copy); otherwise the
/// separator every one of the first lines holds most often; a comma when
/// none does.
fn delimiter(text: &str) -> &'static str {
    let head = &text[..floor_char_boundary(text, 64 * 1024)];
    if head.contains('\t') {
        return "\t";
    }
    let lines: Vec<&str> = head
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(10)
        .collect();
    DELIMITERS
        .into_iter()
        .map(|candidate| {
            let least = lines
                .iter()
                .map(|line| line.matches(candidate).count())
                .min()
                .unwrap_or(0);
            (candidate, least)
        })
        .filter(|(_, least)| *least > 0)
        .max_by_key(|(_, least)| *least)
        .map_or(",", |(candidate, _)| candidate)
}

fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}
