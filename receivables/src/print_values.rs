//! Converts approved Receivables records into the shared print contract's facts.
use crate::{Document, ReceivableError, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_templates::{CompiledPrint, PrintFacts, PrintValue};
use jiff::civil::Date;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

fn date(value: Option<&str>) -> Result<Date> {
    value
        .and_then(|text| text.parse().ok())
        .ok_or(ReceivableError::Pdf("missing_document_date"))
}

fn required(value: Option<&str>) -> Result<&str> {
    value
        .filter(|text| !text.trim().is_empty())
        .ok_or(ReceivableError::Pdf("missing_issued_fact"))
}

fn nonempty<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    lines
        .into_iter()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) struct Balance {
    pub on: Date,
    pub paid: Money,
    pub adjustments: Money,
    pub outstanding: Money,
    pub units: MinorUnits,
}

impl Balance {
    fn display(&self, currency: &str, malay: bool) -> String {
        let labels = if malay {
            ["Setakat", "Dibayar", "Pelarasan", "Baki tertunggak"]
        } else {
            ["As of", "Paid", "Adjustments", "Outstanding"]
        };
        format!(
            "{} {}\n{}: {currency} {}\n{}: {currency} {}\n{}: {currency} {}",
            labels[0],
            self.on,
            labels[1],
            self.paid.display(self.units),
            labels[2],
            self.adjustments.display(self.units),
            labels[3],
            self.outstanding.display(self.units)
        )
    }
}

pub(super) struct Letterhead<'a> {
    pub lines: &'a str,
    pub terms: &'a str,
    pub bank: &'a str,
    pub footer: &'a str,
    pub logo: Option<(&'a str, &'a [u8])>,
}

pub(super) fn render(
    compiled: &CompiledPrint<'_>,
    document: &Document,
    malay: bool,
    balance: Option<&Balance>,
    letterhead: Letterhead<'_>,
    validation_url: Option<&str>,
    accent: bilibi_ui::Accent,
) -> Result<bilibi_artifacts::InlineDocument> {
    let currency = document.currency_code.as_str();
    let print = document
        .print_facts
        .as_ref()
        .ok_or(ReceivableError::Pdf("missing_issue_snapshot"))?;
    let seller_registration = print.seller_registration.as_deref();
    let seller_tax = print.seller_tax.as_deref();
    let registration_label = if malay {
        "No. pendaftaran"
    } else {
        "Registration no."
    };
    let tax_label = if malay { "No. cukai" } else { "Tax ID" };
    let sst_label = if malay {
        "No. pendaftaran SST"
    } else {
        "SST registration no."
    };
    let seller_ids = [
        seller_registration.map(|id| format!("{registration_label}: {id}")),
        seller_tax.map(|id| format!("{tax_label}: {id}")),
        print
            .seller_sst
            .as_ref()
            .map(|id| format!("{sst_label}: {id}")),
    ];
    let seller = nonempty(
        std::iter::once(print.seller_name.as_str())
            .chain(seller_ids.iter().filter_map(Option::as_deref))
            .chain(print.seller_address.iter().map(String::as_str)),
    );
    let buyer = nonempty(
        [
            document.party_name.as_deref(),
            document.party_registration_identifier.as_deref(),
            document.party_line1.as_deref(),
            document.party_line2.as_deref(),
            document.party_line3.as_deref(),
            document.party_postcode.as_deref(),
            document.party_locality.as_deref(),
            document.party_admin1_code.as_deref(),
            document.party_country_iso.as_deref(),
            document.party_email.as_deref(),
            document.party_phone.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    let money = |amount: Decimal| PrintValue::Money { amount, currency };
    let rates: Vec<String> = document
        .lines
        .iter()
        .map(|line| {
            line.tax_rate
                .map_or_else(|| "—".to_owned(), |rate| format!("{}%", rate.normalize()))
        })
        .collect();
    let tax_codes: BTreeMap<_, _> = document
        .tax_lines
        .iter()
        .map(|line| (line.tax_code_id, line.tax_code.as_str()))
        .collect();
    let rows: Vec<_> = document
        .lines
        .iter()
        .zip(&rates)
        .map(|(line, rate)| {
            [
                ("line", PrintValue::Integer(i64::from(line.line_number))),
                ("description", PrintValue::Text(line.description.as_str())),
                ("quantity", PrintValue::Decimal(line.quantity.normalize())),
                ("price", money(line.unit_price)),
                ("net", money(line.amount.amount())),
                (
                    "tax_code",
                    PrintValue::Text(
                        line.tax_code_id
                            .and_then(|id| tax_codes.get(&id).copied())
                            .unwrap_or("—"),
                    ),
                ),
                ("rate", PrintValue::Text(rate)),
                ("tax", money(line.tax_amount.amount())),
            ]
        })
        .collect();
    let rows: Vec<PrintFacts<'_>> = rows.iter().map(|row| row.as_slice()).collect();
    let balance = balance.map(|balance| balance.display(currency, malay));
    let mut facts = vec![
        (
            "number",
            PrintValue::Text(required(document.document_reference.as_deref())?),
        ),
        (
            "issue_date",
            PrintValue::Date(date(document.issue_date.as_deref())?),
        ),
        ("seller", PrintValue::Text(&seller)),
        ("buyer", PrintValue::Text(&buyer)),
        ("lines", PrintValue::Rows(&rows)),
        ("subtotal", money(document.subtotal.amount())),
        ("tax_total", money(document.tax_total.amount())),
        ("total", money(document.total.amount())),
    ];
    if document.kind == "credit_note" {
        facts.extend([
            (
                "original_invoice",
                PrintValue::Text(required(print.original_invoice.as_deref())?),
            ),
            (
                "original_date",
                PrintValue::Date(date(print.original_date.as_deref())?),
            ),
            (
                "reason",
                PrintValue::Text(required(document.description.as_deref())?),
            ),
        ]);
    } else {
        facts.push((
            "due_date",
            PrintValue::Date(date(document.due_date.as_deref())?),
        ));
    }
    for (id, value) in [
        (
            "sales_order",
            document
                .sales_order
                .as_ref()
                .map(|order| order.order_number.as_str()),
        ),
        ("letterhead", Some(letterhead.lines)),
        ("terms", Some(letterhead.terms)),
        ("bank", Some(letterhead.bank)),
        (
            "remarks",
            (document.kind == "invoice")
                .then_some(document.description.as_deref())
                .flatten(),
        ),
        ("footer", Some(letterhead.footer)),
        ("balance", balance.as_deref()),
    ] {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            facts.push((id, PrintValue::Text(value)));
        }
    }
    let ringgit_display = print.ringgit.as_ref().map(|amounts| {
        let labels = if malay {
            ["Kadar pertukaran", "Sebelum cukai", "Cukai", "Jumlah"]
        } else {
            ["Exchange rate", "Before tax", "Tax", "Total"]
        };
        format!(
            "{}: {} MYR / {}\n{}: MYR {}\n{}: MYR {}\n{}: MYR {}",
            labels[0],
            amounts.rate,
            currency,
            labels[1],
            amounts.net,
            labels[2],
            amounts.tax,
            labels[3],
            amounts.total
        )
    });
    if let Some(ringgit) = ringgit_display.as_deref() {
        facts.push(("ringgit", PrintValue::Text(ringgit)));
    }
    if let Some((mime, bytes)) = letterhead.logo {
        facts.push(("logo", PrintValue::Raster { mime, bytes }));
    }
    if let Some(url) = validation_url {
        facts.push(("validation", PrintValue::QrCode(url)));
    }
    compiled
        .document(&document.status, &facts, accent)
        .map_err(|_| ReceivableError::Pdf("invalid_print_facts"))
}
