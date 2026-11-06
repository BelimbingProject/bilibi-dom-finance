//! Money in a book's currency (Bilimbi `Decimal` amounts under
//! `Line.changeset/3`'s scale rule): a `rust_decimal` amount held to the
//! book's minor units. The ledger's columns are `numeric(20,4)`, so an amount
//! has at most sixteen digits before the point, and one finer than the book's
//! minor units is refused, never rounded, because PostgreSQL would otherwise
//! round it silently.
use rust_decimal::Decimal;
use serde::Serialize;
use std::{
    fmt,
    iter::Sum,
    ops::{Add, Neg, Sub},
};

/// A currency's number of decimal places, 0 to 4 (a book's `minor_units`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct MinorUnits(pub(crate) u8);
impl MinorUnits {
    pub const MAX: u8 = 4;
    /// `None` outside 0 to 4.
    pub fn new(units: i64) -> Option<Self> {
        (0..=i64::from(Self::MAX))
            .contains(&units)
            .then_some(Self(units as u8))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}
impl fmt::Display for MinorUnits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An amount in the book's currency, within the column's range and no finer
/// than the book's minor units. Sums and differences of amounts are amounts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Money(Decimal);

/// Why a submitted amount is not money of this book, in the words a form
/// shows beside the field.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("is invalid")]
    Unparsable,
    #[error("has more than {0} decimal places")]
    TooFine(MinorUnits),
    #[error("must be less than 1E+16")]
    TooLarge,
}

impl Money {
    pub const ZERO: Money = Money(Decimal::ZERO);
    /// Sixteen digits before the point: the column is `numeric(20,4)`.
    const LIMIT: Decimal = Decimal::from_parts(0x6FC1_0000, 0x0023_86F2, 0, false, 0);

    /// An amount of this book: refused when its magnitude reaches 1E+16 or
    /// it carries more decimal places than the book's minor units.
    pub fn new(amount: Decimal, units: MinorUnits) -> Result<Self, MoneyError> {
        if amount.abs() >= Self::LIMIT {
            return Err(MoneyError::TooLarge);
        }
        if amount.round_dp(u32::from(units.get())) != amount {
            return Err(MoneyError::TooFine(units));
        }
        Ok(Self(amount.normalize()))
    }
    /// An amount read back from a `numeric(20,4)` column, which only stores
    /// what [`Money::new`] accepted or a sum of such amounts.
    pub(crate) fn stored(amount: Decimal) -> Self {
        Self(amount.normalize())
    }
    /// An amount written in decimal notation (`"12.50"`, `"-3"`).
    pub fn parse(text: &str, units: MinorUnits) -> Result<Self, MoneyError> {
        let amount = Decimal::from_str_exact(text.trim()).map_err(|_| MoneyError::Unparsable)?;
        Self::new(amount, units)
    }
    /// A page amount with thousands separators and the book's decimal places.
    /// `shown` remains plain decimal notation for editable values and contracts.
    /// Without book access, retain the stored precision with at least two places.
    pub fn display(self, units: impl Into<Option<MinorUnits>>) -> String {
        Self::display_decimal(self.0, units)
    }
    /// [`Money::display`] for a figure that is not an amount of the book, a
    /// unit price or a report total read back as a bare decimal.
    pub fn display_decimal(value: Decimal, units: impl Into<Option<MinorUnits>>) -> String {
        let places = match units.into() {
            Some(units) => u32::from(units.get()),
            None => value.normalize().scale().max(2),
        };
        bilibi_locale::money(value, places)
    }
    pub fn amount(self) -> Decimal {
        self.0
    }
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }
    pub fn is_positive(self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }
    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }
    /// The amount written with exactly the book's minor units (`12.50` in a
    /// two-place currency), as a page or a refusal shows it.
    pub fn shown(self, units: MinorUnits) -> String {
        let mut amount = self.0;
        amount.rescale(u32::from(units.get()));
        amount.to_string()
    }
}
impl Add for Money {
    type Output = Money;
    fn add(self, other: Money) -> Money {
        Money(self.0 + other.0)
    }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, other: Money) -> Money {
        Money(self.0 - other.0)
    }
}
impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(-self.0)
    }
}
impl Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, Add::add)
    }
}
impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
