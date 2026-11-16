//! Bilimbi.Finance.Sst.Form: the SST-02 return's boxes, in the form's order,
//! for each of the two taxes it is filed for, each on its own form.
//!
//! The layout is the Royal Malaysian Customs Department's Borang SST-02 as
//! amended on 2 October 2026, read with its completion guide, *Panduan
//! Mengisi Penyata SST-02 (Secara Manual/Pindaan)* of 31 May 2026; the module
//! README cites both. A box's `key` names it in this module's data (item 13A
//! is `13aa`, since keys are lowercase); `item` is how the form prints it.
//! Items the form asks for that no record here holds (Part A's particulars,
//! Part B1's per-item lines, item 15's penalty, Part F's declaration) are left
//! to the filing.
use crate::{TaxSource, TaxType};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;
use std::collections::HashMap;

/// What a box holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxKind {
    /// A value and its tax at the box's `rate` percent (items 11(a) to
    /// 11(d)); a tax code mapped to it must carry that rate.
    Rated,
    /// Second Schedule goods at specific rates (item 17), any rate.
    Specific,
    /// A value with no tax (Parts D and E); a tax code mapped to it must
    /// carry no tax.
    Value,
    /// A tax amount only (item 11(e) and the deductions of item 13).
    Tax,
    /// Computed from other boxes (items 12 and 14).
    Total,
}

/// One box of a tax's form: `sources` lists the modules whose tax codes may
/// be mapped to it, and `adjustable` whether a preparer may add an
/// adjustment to it.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct FormBox {
    pub key: &'static str,
    pub item: &'static str,
    pub part: &'static str,
    pub kind: BoxKind,
    pub rate: Option<u32>,
    pub label: &'static str,
    pub sources: &'static [TaxSource],
    pub adjustable: bool,
}

/// A box a tax code may be mapped to, with the tax whose form holds it.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MappableBox {
    pub tax_type: TaxType,
    #[serde(flatten)]
    pub form_box: FormBox,
}

const R: &[TaxSource] = &[TaxSource::Receivables];
const P: &[TaxSource] = &[TaxSource::Payables];
const RP: &[TaxSource] = &[TaxSource::Receivables, TaxSource::Payables];
const NONE: &[TaxSource] = &[];

const fn b(
    key: &'static str,
    item: &'static str,
    part: &'static str,
    kind: BoxKind,
    rate: Option<u32>,
    label: &'static str,
    sources: &'static [TaxSource],
    adjustable: bool,
) -> FormBox {
    FormBox {
        key,
        item,
        part,
        kind,
        rate,
        label,
        sources,
        adjustable,
    }
}

use BoxKind::{Rated, Specific, Tax, Total, Value};

const SALES_TAX: [FormBox; 19] = [
    b(
        "11a",
        "11(a)",
        "B2",
        Rated,
        Some(5),
        "Taxable goods at 5%",
        R,
        true,
    ),
    b(
        "11b",
        "11(b)",
        "B2",
        Rated,
        Some(10),
        "Taxable goods at 10%",
        R,
        true,
    ),
    b(
        "12",
        "12",
        "B2",
        Total,
        None,
        "Total tax payable",
        NONE,
        false,
    ),
    b(
        "13a",
        "13(a)",
        "B2",
        Tax,
        None,
        "Deduction from credit notes",
        NONE,
        true,
    ),
    b(
        "13b",
        "13(b)",
        "B2",
        Tax,
        None,
        "Sales tax deduction",
        NONE,
        true,
    ),
    b(
        "13d",
        "13(d)",
        "B2",
        Tax,
        None,
        "Bad debt relief",
        NONE,
        true,
    ),
    b(
        "13aa",
        "13A",
        "B2",
        Tax,
        None,
        "Adjustment of excess sales tax deduction claimed earlier",
        NONE,
        true,
    ),
    b(
        "14",
        "14",
        "B2",
        Total,
        None,
        "Tax payable before penalty",
        NONE,
        false,
    ),
    b(
        "17",
        "17",
        "C",
        Specific,
        None,
        "Second Schedule goods at specific rates",
        R,
        true,
    ),
    b(
        "18a",
        "18(a)",
        "D",
        Value,
        None,
        "Exports, and sales to a Special or Designated Area",
        R,
        true,
    ),
    b(
        "18b1",
        "18(b)(1)",
        "D",
        Value,
        None,
        "Sales exempted to Schedule A persons",
        R,
        true,
    ),
    b(
        "18b2",
        "18(b)(2)",
        "D",
        Value,
        None,
        "Sales exempted to Schedule B persons",
        R,
        true,
    ),
    b(
        "18b3i",
        "18(b)(3)(i)",
        "D",
        Value,
        None,
        "Sales exempted under Schedule C items 1 and 2",
        R,
        true,
    ),
    b(
        "18b3ii",
        "18(b)(3)(ii)",
        "D",
        Value,
        None,
        "Sales exempted under Schedule C items 3 and 4",
        R,
        true,
    ),
    b(
        "18b3iii",
        "18(b)(3)(iii)",
        "D",
        Value,
        None,
        "Work performed exempted under Schedule C item 5",
        R,
        true,
    ),
    b(
        "18d",
        "18(d)",
        "D",
        Value,
        None,
        "Sales exempted under section 35(3) or 61A",
        R,
        true,
    ),
    b(
        "19",
        "19",
        "E",
        Value,
        None,
        "Purchases exempted under Schedule C items 1 and 2",
        P,
        true,
    ),
    b(
        "20",
        "20",
        "E",
        Value,
        None,
        "Purchases exempted under Schedule C items 3 and 4",
        P,
        true,
    ),
    b(
        "21",
        "21",
        "E",
        Value,
        None,
        "Purchases exempted under Schedule C item 5",
        P,
        true,
    ),
];

const SERVICE_TAX: [FormBox; 12] = [
    b(
        "11c",
        "11(c)",
        "B2",
        Rated,
        Some(6),
        "Taxable services at 6%",
        RP,
        true,
    ),
    b(
        "11d",
        "11(d)",
        "B2",
        Rated,
        Some(8),
        "Taxable services at 8%",
        RP,
        true,
    ),
    b(
        "11e",
        "11(e)",
        "B2",
        Tax,
        None,
        "Group H item 1: credit and charge cards",
        NONE,
        true,
    ),
    b(
        "12",
        "12",
        "B2",
        Total,
        None,
        "Total tax payable",
        NONE,
        false,
    ),
    b(
        "13a",
        "13(a)",
        "B2",
        Tax,
        None,
        "Deduction from credit notes",
        NONE,
        true,
    ),
    b(
        "13c",
        "13(c)",
        "B2",
        Tax,
        None,
        "Service tax deduction",
        NONE,
        true,
    ),
    b(
        "13d",
        "13(d)",
        "B2",
        Tax,
        None,
        "Bad debt relief",
        NONE,
        true,
    ),
    b(
        "14",
        "14",
        "B2",
        Total,
        None,
        "Tax payable before penalty",
        NONE,
        false,
    ),
    b(
        "18c1",
        "18(c)(1)",
        "D",
        Value,
        None,
        "Services exempted business to business",
        R,
        true,
    ),
    b(
        "18c2",
        "18(c)(2)",
        "D",
        Value,
        None,
        "Services exempted under group relief",
        R,
        true,
    ),
    b(
        "18c3",
        "18(c)(3)",
        "D",
        Value,
        None,
        "Services exempted under section 34(3)(a)",
        R,
        true,
    ),
    b(
        "18c4",
        "18(c)(4)",
        "D",
        Value,
        None,
        "Other non-taxable services",
        R,
        true,
    ),
];

/// The box holding the return's tax payable.
pub const PAYABLE_BOX: &str = "14";
/// Where a credit note on tax already reported deducts.
pub const CREDIT_NOTE_BOX: &str = "13a";

/// Item 12 sums the taxed boxes; item 14 deducts item 13 from it, as the
/// completion guide gives the formula for each tax.
fn totals(
    tax_type: TaxType,
) -> [(
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
); 2] {
    match tax_type {
        TaxType::SalesTax => [
            ("12", &["11a", "11b", "17"], &[]),
            ("14", &["12"], &["13a", "13b", "13d", "13aa"]),
        ],
        TaxType::ServiceTax => [
            ("12", &["11c", "11d", "11e"], &[]),
            ("14", &["12"], &["13a", "13c", "13d"]),
        ],
    }
}

/// The boxes of a tax's form, in the form's order.
pub fn boxes(tax_type: TaxType) -> &'static [FormBox] {
    match tax_type {
        TaxType::SalesTax => &SALES_TAX,
        TaxType::ServiceTax => &SERVICE_TAX,
    }
}

/// One box of a tax's form.
pub fn find(tax_type: TaxType, key: &str) -> Option<&'static FormBox> {
    boxes(tax_type).iter().find(|b| b.key == key)
}

/// The tax whose form holds a box a tax code can be mapped to.
pub fn mapped_tax_type(key: &str) -> Option<TaxType> {
    TaxType::ALL.into_iter().find(|t| {
        boxes(*t)
            .iter()
            .any(|b| b.key == key && !b.sources.is_empty())
    })
}

/// The boxes a tax code of `source` may be mapped to, across both forms.
pub fn mappable_boxes(source: TaxSource) -> Vec<MappableBox> {
    TaxType::ALL
        .into_iter()
        .flat_map(|tax_type| {
            boxes(tax_type)
                .iter()
                .filter(move |b| b.sources.contains(&source))
                .map(move |b| MappableBox {
                    tax_type,
                    form_box: *b,
                })
        })
        .collect()
}

impl FormBox {
    pub fn takes_taxable(&self) -> bool {
        matches!(self.kind, Rated | Specific | Value)
    }
    pub fn takes_tax(&self) -> bool {
        !matches!(self.kind, Value)
    }
    pub fn rate(&self) -> Option<Decimal> {
        self.rate.map(Decimal::from)
    }
}

/// One box's figures: `taxable_amount` and `tax_amount` are `None` where the
/// box takes no such column.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoxFigure {
    pub key: &'static str,
    pub item: &'static str,
    pub part: &'static str,
    pub kind: BoxKind,
    pub label: &'static str,
    pub rate: Option<Decimal>,
    pub taxable_amount: Option<Decimal>,
    pub tax_amount: Option<Decimal>,
}

/// Half up, as Bilimbi's `Decimal.round/3` with `:half_up` rounds.
pub(crate) fn round(amount: Decimal, places: u32) -> Decimal {
    amount.round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero)
}

/// The form's figures from each box's summed `(taxable, tax)`, rounded half
/// up to `minor_units` per box; the totals are computed from the rounded
/// boxes.
pub(crate) fn figures(
    tax_type: TaxType,
    sums: &HashMap<String, (Decimal, Decimal)>,
    minor_units: u32,
) -> Vec<BoxFigure> {
    let mut rounded: HashMap<&str, (Decimal, Decimal)> = boxes(tax_type)
        .iter()
        .filter(|b| b.kind != Total)
        .map(|b| {
            let (taxable, tax) = sums.get(b.key).copied().unwrap_or_default();
            (
                b.key,
                (round(taxable, minor_units), round(tax, minor_units)),
            )
        })
        .collect();
    for (key, added, deducted) in totals(tax_type) {
        let sum = |keys: &[&str]| keys.iter().map(|k| rounded[k].1).sum::<Decimal>();
        let tax = sum(added) - sum(deducted);
        rounded.insert(key, (Decimal::ZERO, tax));
    }
    boxes(tax_type)
        .iter()
        .map(|b| {
            let (taxable, tax) = rounded[b.key];
            BoxFigure {
                key: b.key,
                item: b.item,
                part: b.part,
                kind: b.kind,
                label: b.label,
                rate: b.rate(),
                taxable_amount: b.takes_taxable().then_some(taxable),
                tax_amount: b.takes_tax().then_some(tax),
            }
        })
        .collect()
}
