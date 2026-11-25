use crate::{documents::DocumentView, Authority, Einvoice, EinvoiceError, PartySnapshot, Result};
use bilibi_finance_ledger::dates;
use bilibi_templates::{CompiledPrint, PrintFacts, PrintValue};
use rust_decimal::Decimal;

fn joined<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    items
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn party(party: &PartySnapshot, malay: bool) -> String {
    let address = &party.address;
    let labels = if malay {
        [
            "TIN",
            "Jenis pendaftaran",
            "No. pendaftaran",
            "No. SST",
            "No. cukai pelancongan",
            "Kod industri",
        ]
    } else {
        [
            "TIN",
            "Registration type",
            "Registration no.",
            "SST no.",
            "Tourism tax no.",
            "Industry code",
        ]
    };
    let identity: Vec<_> = [
        party.tax_id.as_deref(),
        party.registration_type.as_deref(),
        party.registration_number.as_deref(),
        party.sales_tax_registration.as_deref(),
        party.tourism_tax_registration.as_deref(),
        party.industry_code.as_deref(),
    ]
    .into_iter()
    .zip(labels)
    .filter_map(|(value, label)| {
        value
            .filter(|text| !text.is_empty())
            .map(|text| format!("{label}: {text}"))
    })
    .collect();
    joined(
        party
            .name
            .as_deref()
            .into_iter()
            .chain(identity.iter().map(String::as_str))
            .chain(party.business_activity.as_deref())
            .chain(address.lines.iter().map(String::as_str))
            .chain(
                [
                    address.postcode.as_deref(),
                    address.city.as_deref(),
                    address.state_code.as_deref(),
                    address.country_code.as_deref(),
                    party.phone.as_deref(),
                    party.email.as_deref(),
                ]
                .into_iter()
                .flatten(),
            ),
    )
}

fn status(value: &str, malay: bool) -> Result<&'static str> {
    let text = match value {
        "issued" => [
            "Issued, awaiting submission",
            "Dikeluarkan, menunggu penghantaran",
        ],
        "submitted" => ["Submitted", "Dihantar"],
        "refused" => ["Submission refused", "Penghantaran ditolak"],
        "valid" => ["Validated", "Disahkan"],
        "invalid" => ["Invalid", "Tidak sah"],
        "cancelled" => ["Cancelled", "Dibatalkan"],
        "rejected" => ["Buyer requested rejection", "Pembeli memohon penolakan"],
        "consolidated" => ["Consolidated", "Disatukan"],
        "released" => [
            "Released from consolidation",
            "Dikeluarkan daripada penyatuan",
        ],
        _ => return Err(EinvoiceError::Artifacts("invalid_fiscal_status")),
    };
    Ok(text[usize::from(malay)])
}

fn kind(code: &str, malay: bool) -> String {
    let translated = if malay {
        match code {
            "01" => Some("Invois"),
            "02" => Some("Nota kredit"),
            "03" => Some("Nota debit"),
            "04" => Some("Nota bayaran balik"),
            "11" => Some("Invois bil kendiri"),
            "12" => Some("Nota kredit bil kendiri"),
            "13" => Some("Nota debit bil kendiri"),
            "14" => Some("Nota bayaran balik bil kendiri"),
            _ => None,
        }
    } else {
        None
    };
    let name = translated
        .map(str::to_owned)
        .unwrap_or_else(|| Einvoice::authority().document_type_name(code));
    format!("{name} ({code})")
}

struct LineText {
    classification: String,
    quantity: String,
    adjustment: String,
    tax: String,
}

pub(super) fn document(
    view: &DocumentView,
    source: &str,
    compiled: &CompiledPrint<'_>,
    malay: bool,
    accent: bilibi_ui::Accent,
) -> Result<bilibi_artifacts::InlineDocument> {
    let document = &view.document;
    let currency = document.currency_code.as_str();
    let money = |amount: Decimal| PrintValue::Money { amount, currency };
    let columns: Vec<_> = document
        .lines
        .iter()
        .map(|line| {
            let classification = joined(
                [
                    Some(line.classification_code.as_str()),
                    line.tariff_code.as_deref(),
                    line.origin_country_code.as_deref(),
                ]
                .into_iter()
                .flatten(),
            );
            let quantity = format!(
                "{} {}",
                line.quantity.normalize(),
                line.unit_code.as_deref().unwrap_or_default()
            );
            let adjustment = if line.discount_amount.is_zero() && line.charge_amount.is_zero() {
                String::new()
            } else {
                let labels = if malay {
                    ["Diskaun", "Caj"]
                } else {
                    ["Discount", "Charge"]
                };
                format!(
                    "{}: {currency} {}\n{}: {currency} {}",
                    labels[0], line.discount_amount, labels[1], line.charge_amount
                )
            };
            let rate = line.tax_rate.map(|d| format!("{}%", d.normalize()));
            let tax = joined(
                [
                    Some(line.tax_type.as_str()),
                    rate.as_deref(),
                    line.tax_exemption_reason.as_deref(),
                ]
                .into_iter()
                .flatten(),
            );
            LineText {
                classification,
                quantity,
                adjustment,
                tax,
            }
        })
        .collect();
    let rows: Vec<_> = document
        .lines
        .iter()
        .zip(&columns)
        .map(|(line, text)| {
            let mut fields = vec![
                ("line", PrintValue::Integer(i64::from(line.line_number))),
                ("classification", PrintValue::Text(&text.classification)),
                ("description", PrintValue::Text(&line.description)),
                ("quantity", PrintValue::Text(text.quantity.trim())),
                ("unit_price", money(line.unit_price)),
                ("net", money(line.line_extension_amount)),
                ("tax_details", PrintValue::Text(&text.tax)),
                ("tax", money(line.tax_amount)),
            ];
            if !text.adjustment.is_empty() {
                fields.push(("adjustment", PrintValue::Text(&text.adjustment)));
            }
            fields
        })
        .collect();
    let rows: Vec<PrintFacts<'_>> = rows.iter().map(|row| row.as_slice()).collect();

    let document_type = kind(&document.document_type, malay);
    let issued_at = dates::display_utc(document.issued_at);
    let supplier = party(&document.supplier, malay);
    let buyer = party(&document.buyer, malay);
    let mut facts = vec![
        ("type", PrintValue::Text(&document_type)),
        ("reference", PrintValue::Text(&document.code_number)),
        ("issued", PrintValue::Text(&issued_at)),
        ("status", PrintValue::Text(status(&view.status, malay)?)),
        ("supplier", PrintValue::Text(&supplier)),
        ("buyer", PrintValue::Text(&buyer)),
        ("lines", PrintValue::Rows(&rows)),
        ("subtotal", money(document.line_extension_amount)),
        ("net", money(document.tax_exclusive_amount)),
        ("tax", money(document.tax_amount)),
        ("gross", money(document.tax_inclusive_amount)),
        ("payable", money(document.payable_amount)),
    ];
    let original = joined(
        [
            document.details.original_code_number.as_deref(),
            document.details.original_document_uuid.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    let billing_start = document
        .details
        .billing_period_start
        .as_deref()
        .map(dates::display_text);
    let billing_end = document
        .details
        .billing_period_end
        .as_deref()
        .map(dates::display_text);
    let consolidation_month = document
        .details
        .consolidation_month
        .as_deref()
        .and_then(|month| {
            format!("{month}-01")
                .parse::<jiff::civil::Date>()
                .ok()
                .map(dates::month)
        });
    let billing = joined(
        [
            billing_start.as_deref(),
            billing_end.as_deref(),
            document.details.billing_period_description.as_deref(),
            consolidation_month.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    let published = view.validated_at.map(dates::display_utc);
    let exchange = document.exchange_rate.map(|rate| rate.to_string());
    for (id, value) in [
        ("uuid", view.document_uuid.as_deref()),
        ("validated", published.as_deref()),
        ("original", Some(original.as_str())),
        ("source", Some(source)),
        ("exchange", exchange.as_deref()),
        ("billing", Some(billing.as_str())),
    ] {
        if let Some(value) = value.filter(|text| !text.trim().is_empty()) {
            facts.push((id, PrintValue::Text(value)));
        }
    }
    if crate::documents::VALID_STATUSES.contains(&view.status.as_str()) {
        let url = view
            .validation_url
            .as_deref()
            .ok_or(EinvoiceError::Artifacts("missing_validation_url"))?;
        facts.push(("validation", PrintValue::QrCode(url)));
    }

    compiled
        .document(&view.status, &facts, accent)
        .map_err(|_| EinvoiceError::Artifacts("invalid_fiscal_print_facts"))
}
