//! Money a person saw somewhere else (a line on a bank statement) and now
//! records where it belongs: a receipt in Receivables, a payment in Payables.
//! The page that shows the money builds a link with [`MoneySeen::href`]; the
//! page that records it reads the link with [`MoneySeen::read`], opens its own
//! form filled in, and sends the person back with [`MoneySeen::back`] once it
//! is saved. The two pages share only this address, never a facade: the
//! recording page does not know who sent the person, and the sender does not
//! know how the money is recorded.
use bilibi_ui::params;
use rust_decimal::Decimal;
use std::collections::HashMap;

const DATE: &str = "seen_date";
const AMOUNT: &str = "seen_amount";
const ACCOUNT: &str = "seen_account";
const TEXT: &str = "seen_text";
const REFERENCE: &str = "seen_reference";
const RETURN: &str = "return_to";
/// The parameter [`MoneySeen::back`] adds: the ledger entry that was posted.
pub const ENTRY: &str = "entry";

/// What the sending page knows about the money.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoneySeen {
    /// The day it moved, `YYYY-MM-DD`.
    pub date: String,
    /// How much, always positive, as the book shows it without grouping.
    pub amount: String,
    /// The ledger account it moved in or out of.
    pub account_id: Option<i64>,
    /// What the other side called it ("DUITNOW CAHAYA RETAIL").
    pub text: String,
    pub reference: String,
    /// The page to go back to, a path on this site.
    pub return_to: Option<String>,
}

impl MoneySeen {
    /// The link that opens `path`'s form with this money filled in.
    pub fn href(&self, path: &str) -> String {
        let mut query = HashMap::from([
            (DATE.to_owned(), self.date.clone()),
            (AMOUNT.to_owned(), self.amount.clone()),
        ]);
        if let Some(account) = self.account_id {
            query.insert(ACCOUNT.into(), account.to_string());
        }
        for (key, value) in [(TEXT, &self.text), (REFERENCE, &self.reference)] {
            if !value.is_empty() {
                query.insert(key.into(), value.chars().take(200).collect());
            }
        }
        if let Some(back) = &self.return_to {
            query.insert(RETURN.into(), back.clone());
        }
        params::address(path, &query)
    }

    /// The money an address carries, or `None` when it carries no amount.
    pub fn read(query: &HashMap<String, String>) -> Option<Self> {
        let text = |key: &str| {
            query
                .get(key)
                .map(|value| value.trim().chars().take(200).collect::<String>())
                .unwrap_or_default()
        };
        let amount = text(AMOUNT);
        amount
            .parse::<Decimal>()
            .ok()
            .filter(|value| value.is_sign_positive() && !value.is_zero())?;
        Some(Self {
            date: text(DATE),
            amount,
            account_id: params::positive_id(query.get(ACCOUNT).map(String::as_str)),
            text: text(TEXT),
            reference: text(REFERENCE),
            return_to: query.get(RETURN).and_then(|to| local_path(to)),
        })
    }

    /// Where the person goes once the money is recorded: the page that sent
    /// them, told which ledger entry was posted (none yet for a payment that
    /// waits for approval). `None` when no page sent them.
    pub fn back(&self, entry_id: Option<i64>) -> Option<String> {
        let back = self.return_to.as_ref()?;
        Some(match entry_id {
            Some(entry) => format!(
                "{back}{}{ENTRY}={entry}",
                if back.contains('?') { '&' } else { '?' }
            ),
            None => back.clone(),
        })
    }

    /// The amount as a number.
    pub fn value(&self) -> Decimal {
        self.amount.parse().unwrap_or(Decimal::ZERO)
    }

    /// Which party the money most likely belongs to, by position: the one
    /// whose name shares the most words with the text, then the one with an
    /// unpaid document of exactly this amount. `None` when nothing points to
    /// one party, so the form asks instead of guessing.
    pub fn likely<'a>(
        &self,
        parties: impl IntoIterator<Item = (&'a str, &'a [Decimal])>,
    ) -> Option<usize> {
        let said = words(&self.text);
        let amount = self.value();
        let mut best: Option<((usize, bool), usize)> = None;
        let mut tied = false;
        for (index, (name, unpaid)) in parties.into_iter().enumerate() {
            let named = words(name)
                .iter()
                .filter(|word| word.len() >= 4 && said.contains(*word))
                .count();
            let score = (named, unpaid.contains(&amount));
            if score == (0, false) {
                continue;
            }
            match best {
                Some((top, _)) if score < top => {}
                Some((top, _)) if score == top => tied = true,
                _ => {
                    best = Some((score, index));
                    tied = false;
                }
            }
        }
        best.filter(|_| !tied).map(|(_, index)| index)
    }

    /// How the amount splits over a party's unpaid documents, oldest first as
    /// given: the one document of exactly this amount, or else each in turn
    /// until the amount is used up.
    pub fn split(&self, unpaid: &[Decimal]) -> Vec<Decimal> {
        let amount = self.value();
        if let Some(exact) = unpaid.iter().position(|owed| *owed == amount) {
            return unpaid
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    if index == exact {
                        amount
                    } else {
                        Decimal::ZERO
                    }
                })
                .collect();
        }
        let mut left = amount;
        unpaid
            .iter()
            .map(|owed| {
                let take = left.min(*owed).max(Decimal::ZERO);
                left -= take;
                take
            })
            .collect()
    }
}

/// A path on this site, never another site's address: one leading slash, no
/// backslash or control character, at most 300 characters.
fn local_path(to: &str) -> Option<String> {
    let safe = to.starts_with('/')
        && !to.starts_with("//")
        && to.len() <= 300
        && !to.chars().any(|c| c == '\\' || c.is_control());
    safe.then(|| to.to_owned())
}

/// The lower-case words of a name or a statement text, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}
