//! A first guess at how a bank's export is read, from the file itself, for a
//! bank account whose columns are not mapped yet: the column separator, the
//! rows above the data, which column is the date, description, reference,
//! amount (or money out and money in) and running balance, the date format
//! and the decimal point. It only proposes: the person sees the first rows
//! read through the guess and corrects it before the import keeps it.
//!
//! Columns are found by the usual words of a bank's header row ("Date",
//! "Description", "Withdrawal", ...), then by their values (the first column
//! of dates, the widest text, the first column of numbers). The words are the
//! file format's, never a company's chart or a particular bank's layout.
use crate::{
    csv,
    mapping::{parse_amount, parse_date, DATE_FORMATS, DELIMITERS},
};
use serde_json::{json, Value};

/// How many rows the guess reads, after the header.
const SAMPLE: usize = 20;
/// The most rows a guess skips above the header, as the mapping allows.
const MAX_HEADER_ROWS: usize = 20;

/// Header words for each column, tried in this order so that "Balance" is
/// not taken for an amount and "Value date" not for a description.
const WORDS: [(&str, &[&str]); 7] = [
    ("balance", &["balance"]),
    ("date", &["date"]),
    (
        "debit",
        &["debit", "withdrawal", "money out", "paid out", "dr"],
    ),
    (
        "credit",
        &["credit", "deposit", "money in", "paid in", "cr"],
    ),
    ("amount", &["amount", "value"]),
    ("reference", &["reference", "ref", "cheque", "chq"]),
    (
        "description",
        &[
            "description",
            "details",
            "detail",
            "narrative",
            "particular",
            "memo",
            "remark",
            "transaction",
            "payee",
        ],
    ),
];

/// The guessed mapping, every key present, as [`crate::ImportMapping::parse`]
/// reads it. A column it cannot find is left out, so the person picks it.
pub(crate) fn mapping(content: &[u8]) -> Value {
    let delimiter = delimiter(content);
    let rows = csv::parse(content, delimiter).unwrap_or_default();
    let header_rows = header_rows(&rows);
    let data: Vec<&Vec<String>> = rows.iter().skip(header_rows).take(SAMPLE).collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let values = |index: usize| -> Vec<&str> {
        data.iter()
            .filter_map(|row| row.get(index))
            .map(|cell| cell.trim())
            .filter(|cell| !cell.is_empty())
            .collect()
    };
    let mut found: Vec<(&str, usize)> = Vec::new();
    let taken = |found: &[(&str, usize)], index: usize| found.iter().any(|(_, i)| *i == index);
    if header_rows > 0 {
        let header = &rows[header_rows - 1];
        for (key, words) in WORDS {
            if let Some(index) = (0..header.len())
                .find(|&index| !taken(&found, index) && named(&header[index], words))
            {
                found.push((key, index));
            }
        }
    }
    let has = |found: &[(&str, usize)], key: &str| found.iter().any(|(k, _)| *k == key);
    if !has(&found, "date") {
        if let Some(index) = (0..width).find(|&i| !taken(&found, i) && dates(&values(i))) {
            found.push(("date", index));
        }
    }
    let sided = has(&found, "debit") && has(&found, "credit");
    if !sided {
        found.retain(|(key, _)| *key != "debit" && *key != "credit");
        if !has(&found, "amount") {
            if let Some(index) = (0..width).find(|&i| !taken(&found, i) && numbers(&values(i))) {
                found.push(("amount", index));
            }
        }
    }
    if !has(&found, "description") {
        let widest = (0..width)
            .filter(|&i| {
                let cells = values(i);
                !taken(&found, i) && !cells.is_empty() && !numbers(&cells) && !dates(&cells)
            })
            .max_by_key(|&i| values(i).iter().map(|cell| cell.len()).sum::<usize>());
        if let Some(index) = widest {
            found.push(("description", index));
        }
    }
    let column = |key: &str| -> Value {
        found
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(Value::Null, |(_, index)| {
                if header_rows > 0 {
                    Value::from(rows[header_rows - 1][*index].trim())
                } else {
                    Value::from(index + 1)
                }
            })
    };
    let index = |key: &str| found.iter().find(|(k, _)| *k == key).map(|(_, i)| *i);
    let date_format = index("date")
        .and_then(|i| {
            let cells = values(i);
            DATE_FORMATS.into_iter().find(|format| {
                !cells.is_empty() && cells.iter().all(|c| parse_date(c, format).is_some())
            })
        })
        .unwrap_or(DATE_FORMATS[0]);
    let amounts: Vec<&str> = ["amount", "debit", "credit", "balance"]
        .into_iter()
        .filter_map(index)
        .flat_map(values)
        .collect();
    json!({
        "delimiter": delimiter,
        "header_rows": header_rows,
        "date": column("date"),
        "date_format": date_format,
        "description": column("description"),
        "reference": column("reference"),
        "amount": column("amount"),
        "amount_sign": "deposits_positive",
        "debit": column("debit"),
        "credit": column("credit"),
        "balance": column("balance"),
        "decimal_separator": if decimal_comma(&amounts) { "," } else { "." },
    })
}

/// The separator the first lines use most consistently: the one with the
/// highest count that every one of them shares; a comma when none does.
fn delimiter(content: &[u8]) -> &'static str {
    let head = &content[..content.len().min(64 * 1024)];
    let text = String::from_utf8_lossy(head);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(1)
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

/// The rows above the data: none when the first row already holds a date,
/// else up to and including the last row of words above the first row with a
/// date (the header). One when nothing in the file looks like a date.
fn header_rows(rows: &[Vec<String>]) -> usize {
    let dated = |row: &Vec<String>| row.iter().any(|cell| date(cell.trim()));
    match rows.iter().take(MAX_HEADER_ROWS + 1).position(dated) {
        Some(0) => 0,
        Some(first) => first,
        None => 1,
    }
}

fn named(header: &str, words: &[&str]) -> bool {
    let header = header.trim().to_lowercase();
    words.iter().any(|word| {
        if word.len() <= 3 {
            // A short word only as a word of its own ("Dr", "Ref No"), never
            // inside another ("address", "credit").
            header
                .split(|c: char| !c.is_alphanumeric())
                .any(|part| part == *word)
        } else {
            header.contains(word)
        }
    })
}

fn date(cell: &str) -> bool {
    DATE_FORMATS
        .into_iter()
        .any(|format| parse_date(cell, format).is_some())
}

fn dates(cells: &[&str]) -> bool {
    !cells.is_empty() && cells.iter().all(|cell| date(cell))
}

fn numbers(cells: &[&str]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            cell.bytes().any(|b| b.is_ascii_digit())
                && !date(cell)
                && (matches!(parse_amount(Some(*cell), "."), Ok(Some(_)))
                    || matches!(parse_amount(Some(*cell), ","), Ok(Some(_))))
        })
}

/// Whether the amounts are written with a decimal comma ("1.234,56",
/// "12,5"): a comma followed by one or two digits at the end, and never a
/// point there.
fn decimal_comma(cells: &[&str]) -> bool {
    let ends = |cell: &str, mark: char| {
        let digits: String = cell
            .trim()
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .to_owned();
        digits
            .rfind(mark)
            .is_some_and(|at| (1..=2).contains(&(digits.len() - at - 1)))
    };
    cells.iter().any(|cell| ends(cell, ',')) && !cells.iter().any(|cell| ends(cell, '.'))
}
