//! The opening balances grid's draft: the conversion date, the source
//! reference and what the person typed (or pasted) for each account. The
//! server keeps it between the form's change events, so totals and the
//! difference follow every keystroke, and it becomes the contract's input
//! only on submit.
use super::super::view;
use crate::{columns, MinorUnits, Money};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The form the date, reference and every grid cell belong to.
pub(super) const FORM: &str = "opening";

/// A debit or a credit cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Debit,
    Credit,
}

impl Side {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Debit => "debit",
            Self::Credit => "credit",
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Draft {
    pub date: String,
    pub reference: String,
    /// What each account's cells hold as typed, by account id.
    pub cells: BTreeMap<i64, (String, String)>,
}

/// One account's amount read from its cells.
pub(super) enum Amount {
    Empty,
    Debit(Money),
    Credit(Money),
    /// Why the cells cannot be read, beside the cell it names.
    Refused(Side, String),
}

impl Draft {
    /// The cell a form field names: `debit_12`, `credit_12`.
    pub(super) fn cell_key(side: Side, account: i64) -> String {
        format!("{}_{account}", side.key())
    }

    pub(super) fn cell(&self, side: Side, account: i64) -> &str {
        self.cells
            .get(&account)
            .map_or("", |(debit, credit)| match side {
                Side::Debit => debit,
                Side::Credit => credit,
            })
    }

    /// Reads the form's values. Every cell is the form's own key
    /// (`opening[debit_12]`), read here rather than through
    /// `params::form_values`, which keeps only the first 64 fields.
    pub(super) fn read(&mut self, value: &Value) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, value) in object {
            let Some(name) = key
                .strip_prefix(FORM)
                .and_then(|k| k.strip_prefix('['))
                .and_then(|k| k.strip_suffix(']'))
            else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            // Each cell is a figure, the date and reference short lines.
            let text: String = text.chars().take(100).collect();
            match name {
                "conversion_date" => self.date = text,
                "source_reference" => self.reference = text,
                _ => {
                    let side = if name.starts_with("debit_") {
                        Side::Debit
                    } else if name.starts_with("credit_") {
                        Side::Credit
                    } else {
                        continue;
                    };
                    let Some(account) = name
                        .split_once('_')
                        .and_then(|(_, id)| id.parse::<i64>().ok())
                    else {
                        continue;
                    };
                    let cell = self.cells.entry(account).or_default();
                    match side {
                        Side::Debit => cell.0 = text,
                        Side::Credit => cell.1 = text,
                    }
                }
            }
        }
        self.cells
            .retain(|_, (debit, credit)| !debit.trim().is_empty() || !credit.trim().is_empty());
    }

    /// Sets one account's balance (debit-positive), as a paste fills it.
    pub(super) fn set(&mut self, account: i64, balance: Money, units: MinorUnits) {
        if balance.is_zero() {
            self.cells.remove(&account);
        } else if balance.is_positive() {
            self.cells
                .insert(account, (balance.shown(units), String::new()));
        } else {
            self.cells
                .insert(account, (String::new(), balance.abs().shown(units)));
        }
    }

    /// The account's amount, as the ledger would read it: one positive side,
    /// at the book's precision. A typed figure may group thousands
    /// ("1,234.50"); a negative one counts on the other side.
    pub(super) fn amount(&self, account: i64, units: MinorUnits) -> Amount {
        let read = |side: Side| -> Result<Option<Money>, String> {
            let text = self.cell(side, account);
            match columns::number(text, '.') {
                Ok(None) => Ok(None),
                Err(()) => Err("is not a number".into()),
                Ok(Some(value)) => Money::new(value, units)
                    .map(|m| (!m.is_zero()).then_some(m))
                    .map_err(|e| e.to_string()),
            }
        };
        let debit = match read(Side::Debit) {
            Ok(m) => m,
            Err(e) => return Amount::Refused(Side::Debit, e),
        };
        let credit = match read(Side::Credit) {
            Ok(m) => m,
            Err(e) => return Amount::Refused(Side::Credit, e),
        };
        let balance = match (debit, credit) {
            (None, None) => return Amount::Empty,
            (Some(_), Some(_)) => {
                return Amount::Refused(Side::Credit, "and debit cannot both hold an amount".into())
            }
            (Some(d), None) => d,
            (None, Some(c)) => -c,
        };
        if balance.is_positive() {
            Amount::Debit(balance)
        } else {
            Amount::Credit(balance.abs())
        }
    }

    /// Total debits, total credits, and whether every cell reads, over the
    /// accounts that take postings: the ones the grid draws and `rows` sends.
    pub(super) fn totals(
        &self,
        accounts: &[view::Account],
        units: MinorUnits,
    ) -> (Money, Money, bool) {
        let (mut debits, mut credits, mut readable) = (Money::ZERO, Money::ZERO, true);
        for account in accounts.iter().filter(|a| a.postable()) {
            match self.amount(account.id, units) {
                Amount::Debit(m) => debits = debits + m,
                Amount::Credit(m) => credits = credits + m,
                Amount::Refused(..) => readable = false,
                Amount::Empty => {}
            }
        }
        (debits, credits, readable)
    }

    /// Accounts that hold an amount here but no longer take postings, so the
    /// grid does not draw them and the submission leaves them out.
    pub(super) fn left_out<'a>(
        &self,
        accounts: &'a [view::Account],
        units: MinorUnits,
    ) -> Vec<(&'a view::Account, Money)> {
        accounts
            .iter()
            .filter(|account| !account.postable())
            .filter_map(|account| match self.amount(account.id, units) {
                Amount::Debit(m) => Some((account, m)),
                Amount::Credit(m) => Some((account, -m)),
                _ => None,
            })
            .collect()
    }

    /// The accounts the contract's rows name, in the grid's order, so a
    /// refusal of "row N" can be shown beside that account.
    pub(super) fn rows<'a>(
        &self,
        accounts: &'a [view::Account],
        units: MinorUnits,
    ) -> Vec<(&'a view::Account, Side, Money)> {
        accounts
            .iter()
            .filter(|account| account.postable())
            .filter_map(|account| match self.amount(account.id, units) {
                Amount::Debit(m) => Some((account, Side::Debit, m)),
                Amount::Credit(m) => Some((account, Side::Credit, m)),
                _ => None,
            })
            .collect()
    }

    /// The submission the opening balances contract reads: one source row per
    /// account the grid fills, named by the account's code, in the CSV the
    /// ledger keeps as the opening's source evidence.
    pub(super) fn input(
        &self,
        accounts: &[view::Account],
        units: MinorUnits,
        currency: &str,
        replaces: Option<i64>,
    ) -> Value {
        let mut csv = String::from("source_id,account_code,debit,credit\n");
        for (account, side, money) in self.rows(accounts, units) {
            let code = quoted(&account.code);
            let amount = money.shown(units);
            let (debit, credit) = match side {
                Side::Debit => (amount.as_str(), ""),
                Side::Credit => ("", amount.as_str()),
            };
            csv.push_str(&format!("{code},{code},{debit},{credit}\n"));
        }
        let mut input = json!({
            "conversion_date": self.date.trim(),
            "currency_code": currency,
            "source_reference": self.reference.trim(),
            "csv": csv,
        });
        if let Some(journal) = replaces {
            input["replaces_journal_id"] = json!(journal);
        }
        input
    }

    /// The draft a journal's lines fill, for a correction or a fresh start
    /// from a rejected or withdrawn opening.
    pub(super) fn from_lines(date: &str, lines: &[view::Line], units: MinorUnits) -> Self {
        let mut draft = Self {
            date: date.to_owned(),
            ..Self::default()
        };
        let mut balances: BTreeMap<i64, Money> = BTreeMap::new();
        for line in lines {
            let amount = |text: &str| Money::parse(text, units).unwrap_or(Money::ZERO);
            *balances.entry(line.account_id).or_insert(Money::ZERO) = balances
                .get(&line.account_id)
                .copied()
                .unwrap_or(Money::ZERO)
                + amount(&line.debit)
                - amount(&line.credit);
        }
        for (account, balance) in balances {
            draft.set(account, balance, units);
        }
        draft
    }
}

/// A CSV field, quoted when it holds a separator, a quote or a line end.
fn quoted(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}
