//! What the pages read back from the ledger's read contracts. The contracts
//! answer JSON; these are the pages' own typed copies of the fields they show.
use bilibi_ui::Choice;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use std::str::FromStr;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BookRow {
    pub currency_code: String,
    pub minor_units: u8,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct BookCurrency {
    pub currency_name: Option<String>,
    pub standard_minor_units: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Change {
    pub controlled: Option<bool>,
    pub required: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BookState {
    pub book: Option<BookRow>,
    /// Null until the book is opened.
    #[serde(default)]
    pub currency: Option<BookCurrency>,
    pub controlled_bookkeeping: Option<Change>,
    pub duty_separation: Option<Change>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Account {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub account_type: String,
    pub parent_id: Option<i64>,
    pub is_group: bool,
    pub active: bool,
    pub retained_earnings: bool,
}
impl Account {
    /// Whether an entry may name it: an active account, never a group.
    pub(crate) fn postable(&self) -> bool {
        self.active && !self.is_group
    }
}

/// One row of the chart's figures: an account's own `balance`, or a group's
/// `total` over the accounts under it, never both.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChartRow {
    pub id: i64,
    pub balance: Option<String>,
    pub total: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChartBalances {
    pub minor_units: u8,
    pub rows: Vec<ChartRow>,
}

/// What applying a starter chart would create, as the contract previews it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TemplateRow {
    pub code: String,
    pub name: String,
    pub account_type: String,
    pub is_group: bool,
    pub parent_code: Option<String>,
    pub retained_earnings: bool,
}

/// An optional part of a starter chart; `chosen` is whether the review being
/// shown includes its rows.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TemplateSection {
    pub key: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub chosen: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TemplateSummary {
    pub key: String,
    pub name: String,
    pub description: String,
    pub sections: Vec<TemplateSection>,
    pub groups: usize,
    pub accounts: usize,
    pub applicable: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Owner {
    pub module: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Line {
    pub line_number: i32,
    pub account_id: i64,
    pub debit: String,
    pub credit: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Entry {
    pub id: i64,
    pub entry_number: i64,
    pub entry_date: String,
    pub description: String,
    pub reference: Option<String>,
    pub currency_code: String,
    pub reverses_entry_id: Option<i64>,
    pub owner: Option<Owner>,
    pub posted_by_user_id: i64,
    pub posted_by_impersonator_id: Option<i64>,
    pub posted_at: String,
    pub reversed_by_entry_id: Option<i64>,
    #[serde(default)]
    pub request_number: Option<i64>,
    #[serde(default)]
    pub late_recording: Option<LateRecording>,
    #[serde(default)]
    pub opening: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Decision {
    pub event: String,
    pub reason: Option<String>,
    pub entry_id: Option<i64>,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub occurred_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Journal {
    pub id: i64,
    pub journal_number: i64,
    pub entry_date: String,
    pub description: String,
    pub reference: Option<String>,
    pub currency_code: String,
    pub submitted_by_user_id: i64,
    pub submitted_by_impersonator_id: Option<i64>,
    pub submitted_at: String,
    pub status: String,
    pub decision: Option<Decision>,
    pub replaces_journal_id: Option<i64>,
    pub replaced_by_journal_id: Option<i64>,
    pub replaces_journal_number: Option<i64>,
    pub replaced_by_journal_number: Option<i64>,
    #[serde(default)]
    pub opening: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Period {
    pub id: i64,
    pub period_number: i16,
    pub start_date: String,
    pub end_date: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Year {
    pub id: i64,
    pub start_date: String,
    pub end_date: String,
    pub status: String,
    pub closing_entry_id: Option<i64>,
    pub periods: Vec<Period>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TrialRow {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub account_type: String,
    pub debits: String,
    pub credits: String,
    pub debit_balance: String,
    pub credit_balance: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TrialBalance {
    pub as_of: String,
    pub through_entry_number: i64,
    pub currency_code: String,
    pub minor_units: u8,
    pub rows: Vec<TrialRow>,
    pub total_debit: String,
    pub total_credit: String,
}

/// Reads a contract answer into a page type; anything else is a contract the
/// page does not understand, which only a mismatched build can produce.
pub(crate) fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Option<T> {
    serde_json::from_value(value).ok()
}

/// An amount written with the book's decimal places, as a person reads it.
pub(crate) fn amount(text: &str, minor_units: u8) -> String {
    match Decimal::from_str(text) {
        Ok(value) => crate::MinorUnits::new(i64::from(minor_units))
            .and_then(|units| {
                crate::Money::new(value, units)
                    .ok()
                    .map(|money| money.display(units))
            })
            .unwrap_or_else(|| text.to_owned()),
        Err(_) => text.to_owned(),
    }
}

/// The amount, or a dash when it is zero (a trial balance column holds one side).
pub(crate) fn amount_or_dash(text: &str, minor_units: u8) -> String {
    match Decimal::from_str(text) {
        Ok(value) if value.is_zero() => "—".to_owned(),
        _ => amount(text, minor_units),
    }
}

pub(crate) fn decimal(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap_or_default()
}

/// An account as a person names it: the name first, then its code
/// ("Bank Accounts · bank-accounts"), for a select option or a sentence.
/// `super::account_name` is the same in HTML, with the code in muted italics.
pub(crate) fn account_label(account: &Account) -> String {
    format!("{} · {}", account.name, account.code)
}

/// An account as a combobox choice: the name, with the code drawn muted and
/// italic after it by the shared control.
pub(crate) fn account_choice(account: &Account) -> Choice {
    Choice::new(&account.name, account.id.to_string()).detail(&account.code)
}

/// Accounts in the order a name-first list reads: by name, then by code.
pub(crate) fn by_name<'a>(accounts: impl Iterator<Item = &'a Account>) -> Vec<&'a Account> {
    let mut list: Vec<&Account> = accounts.collect();
    list.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.code.cmp(&b.code))
    });
    list
}

/// A word for the account type in the plain form people say it.
pub(crate) fn type_label(account_type: &str) -> &'static str {
    match account_type {
        "asset" => "Asset",
        "liability" => "Liability",
        "equity" => "Equity",
        "income" => "Income",
        "expense" => "Expense",
        _ => "Other",
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LateRecording {
    pub days: i32,
    pub status: String,
    pub review: Option<LateReview>,
}
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LateReview {
    pub reason: String,
    pub reviewed_by_user_id: i64,
    pub reviewed_by_impersonator_id: Option<i64>,
    pub reviewed_at: String,
}
