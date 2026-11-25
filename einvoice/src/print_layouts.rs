//! The authority document owns the immutable facts used by these layouts.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_templates::{
    PrintContract, PrintDefault, PrintFacts, PrintField, PrintKind, PrintValue, PRINT_CATALOGUE,
};
use rust_decimal::Decimal;

pub(crate) const KEY: &str = "finance.einvoice.fiscal_document";
const SAMPLE_MONEY: PrintValue<'static> = PrintValue::Money {
    amount: Decimal::ONE,
    currency: "MYR",
};

macro_rules! text {
    ($key:literal, $en:literal, $ms:literal, $sample:literal) => {
        PrintField {
            id: $key,
            labels: &[("en", $en), ("ms", $ms)],
            kind: PrintKind::Text,
            optional: false,
            required_output: true,
            sample: PrintValue::Text($sample),
        }
    };
}
macro_rules! money {
    ($key:literal, $en:literal, $ms:literal) => {
        PrintField {
            id: $key,
            labels: &[("en", $en), ("ms", $ms)],
            kind: PrintKind::Money,
            optional: false,
            required_output: true,
            sample: SAMPLE_MONEY,
        }
    };
}
const fn optional(mut field: PrintField) -> PrintField {
    field.optional = true;
    field
}

const COLUMNS: &[PrintField] = &[
    PrintField {
        id: "line",
        labels: &[("en", "Line"), ("ms", "Baris")],
        kind: PrintKind::Integer,
        optional: false,
        required_output: true,
        sample: PrintValue::Integer(1),
    },
    text!("classification", "Classification", "Klasifikasi", "022"),
    text!("description", "Description", "Perihal", "Service agreement"),
    text!("quantity", "Quantity", "Kuantiti", "1 HUR"),
    money!("unit_price", "Unit price", "Harga seunit"),
    optional(text!(
        "adjustment",
        "Discount or charge",
        "Diskaun atau caj",
        "MYR 0.50"
    )),
    money!("net", "Net amount", "Amaun bersih"),
    text!("tax_details", "Tax", "Cukai", "02 / 6%"),
    money!("tax", "Tax amount", "Amaun cukai"),
];
const SAMPLE: PrintFacts<'static> = &[
    ("line", COLUMNS[0].sample),
    ("classification", COLUMNS[1].sample),
    ("description", COLUMNS[2].sample),
    ("quantity", COLUMNS[3].sample),
    ("unit_price", SAMPLE_MONEY),
    ("adjustment", COLUMNS[5].sample),
    ("net", SAMPLE_MONEY),
    ("tax_details", COLUMNS[7].sample),
    ("tax", SAMPLE_MONEY),
];

pub(crate) static CONTRACT: PrintContract = PrintContract {
    key: KEY,
    label: "E-invoice",
    version: "1",
    base_language: "en",
    states: &crate::documents::STATUS_EVENTS,
    fields: &[
        text!(
            "type",
            "Fiscal document type",
            "Jenis dokumen fiskal",
            "Invoice (01)"
        ),
        text!(
            "reference",
            "Reference number",
            "Nombor rujukan",
            "INV-000034"
        ),
        text!(
            "issued",
            "Issued at",
            "Masa dikeluarkan",
            "2026-10-15T04:30:00Z"
        ),
        text!("status", "Document status", "Status dokumen", "Valid"),
        optional(text!(
            "uuid",
            "Authority UUID",
            "UUID pihak berkuasa",
            "DEMO-VALIDATION-UUID"
        )),
        optional(text!(
            "validated",
            "Validated at",
            "Masa disahkan",
            "2026-10-15T04:41:02Z"
        )),
        optional(text!(
            "original",
            "Original document",
            "Dokumen asal",
            "INV-000032"
        )),
        optional(text!(
            "source",
            "Source document",
            "Dokumen sumber",
            "finance/receivables INV-1042"
        )),
        text!(
            "supplier",
            "Supplier",
            "Pembekal",
            "Example Supplier\nTIN C123\n12 Market Way"
        ),
        text!(
            "buyer",
            "Buyer",
            "Pembeli",
            "Example Buyer\nTIN C456\n34 Orchard Lane"
        ),
        optional(text!(
            "exchange",
            "Exchange rate",
            "Kadar pertukaran",
            "4.5"
        )),
        optional(text!(
            "billing",
            "Billing period",
            "Tempoh bil",
            "1 Oct 2026\n30 Oct 2026"
        )),
        PrintField {
            id: "lines",
            labels: &[],
            kind: PrintKind::Rows(COLUMNS),
            optional: false,
            required_output: true,
            sample: PrintValue::Rows(&[SAMPLE]),
        },
        money!("net", "Total excluding tax", "Jumlah tidak termasuk cukai"),
        money!("tax", "Total tax", "Jumlah cukai"),
        money!("gross", "Total including tax", "Jumlah termasuk cukai"),
        money!("payable", "Payable", "Perlu dibayar"),
        optional(PrintField {
            id: "validation",
            labels: &[("en", "Validate e-invoice"), ("ms", "Sahkan e-Invois")],
            kind: PrintKind::QrCode,
            optional: false,
            required_output: true,
            sample: PrintValue::QrCode("https://example.test/validate/DEMO-VALIDATION-UUID"),
        }),
    ],
    defaults: &[
        PrintDefault {
            language: "en",
            version: "1",
            layout: include_str!("../prints/fiscal.en.json"),
        },
        PrintDefault {
            language: "ms",
            version: "1",
            layout: include_str!("../prints/fiscal.ms.json"),
        },
    ],
};
inventory::submit! { Contribution::new(PRINT_CATALOGUE, KEY, Provenance {
    module: crate::MODULE, source: file!(), line: line!(),
}, &CONTRACT) }
