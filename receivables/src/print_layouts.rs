//! Layouts for the commercial issued copies. The posting flow owns their facts.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_templates::{
    PrintContract, PrintDefault, PrintFacts, PrintField, PrintKind, PrintValue, PRINT_CATALOGUE,
};
use jiff::civil::date;
use rust_decimal::Decimal;

const INVOICE_KEY: &str = "finance.receivables.invoice";
const CREDIT_KEY: &str = "finance.receivables.credit_note";
const CURRENCY: &str = "MYR";
const PRICE: Decimal = Decimal::from_parts(14500, 0, 0, false, 2);
const SAMPLE_TOTAL: Decimal = Decimal::from_parts(15370, 0, 0, false, 2);

const fn field(
    id: &'static str,
    labels: &'static [(&'static str, &'static str)],
    kind: PrintKind,
    sample: PrintValue<'static>,
) -> PrintField {
    PrintField {
        id,
        labels,
        kind,
        optional: false,
        required_output: true,
        sample,
    }
}

const fn optional(mut definition: PrintField) -> PrintField {
    definition.optional = true;
    definition.required_output = false;
    definition
}

const fn text(
    id: &'static str,
    labels: &'static [(&'static str, &'static str)],
    sample: &'static str,
) -> PrintField {
    field(id, labels, PrintKind::Text, PrintValue::Text(sample))
}

const fn money(
    id: &'static str,
    labels: &'static [(&'static str, &'static str)],
    amount: Decimal,
) -> PrintField {
    field(
        id,
        labels,
        PrintKind::Money,
        PrintValue::Money {
            amount,
            currency: CURRENCY,
        },
    )
}

const ISSUE_DATE: PrintField = field(
    "issue_date",
    &[("en", "Issue date"), ("ms", "Tarikh dikeluarkan")],
    PrintKind::Date,
    PrintValue::Date(date(2026, 10, 15)),
);

const LINE_COLUMNS: &[PrintField] = &[
    field(
        "line",
        &[("en", "Line"), ("ms", "Baris")],
        PrintKind::Integer,
        PrintValue::Integer(1),
    ),
    text(
        "description",
        &[("en", "Description"), ("ms", "Perihal")],
        "Annual support",
    ),
    field(
        "quantity",
        &[("en", "Quantity"), ("ms", "Kuantiti")],
        PrintKind::Decimal,
        PrintValue::Decimal(Decimal::ONE),
    ),
    money(
        "price",
        &[("en", "Unit price"), ("ms", "Harga seunit")],
        PRICE,
    ),
    money(
        "net",
        &[("en", "Before tax"), ("ms", "Sebelum cukai")],
        PRICE,
    ),
    text("tax_code", &[("en", "Tax code"), ("ms", "Kod cukai")], "SV"),
    text("rate", &[("en", "Tax rate"), ("ms", "Kadar cukai")], "6%"),
    money(
        "tax",
        &[("en", "Tax"), ("ms", "Cukai")],
        Decimal::from_parts(870, 0, 0, false, 2),
    ),
];

const SAMPLE_LINE: PrintFacts<'static> = &[
    ("line", PrintValue::Integer(1)),
    ("description", PrintValue::Text("Annual support")),
    ("quantity", PrintValue::Decimal(Decimal::ONE)),
    (
        "price",
        PrintValue::Money {
            amount: PRICE,
            currency: CURRENCY,
        },
    ),
    (
        "net",
        PrintValue::Money {
            amount: PRICE,
            currency: CURRENCY,
        },
    ),
    ("tax_code", PrintValue::Text("SV")),
    ("rate", PrintValue::Text("6%")),
    (
        "tax",
        PrintValue::Money {
            amount: Decimal::from_parts(870, 0, 0, false, 2),
            currency: CURRENCY,
        },
    ),
];

macro_rules! fields {
    ($($extra:expr),* $(,)?) => {
        &[
            text("number", &[("en", "Document number"), ("ms", "Nombor dokumen")], "INV-000034"),
            ISSUE_DATE,
            text("seller", &[("en", "Supplier"), ("ms", "Pembekal")], "Example Trading\n12 Market Way\nKuala Lumpur"),
            text("buyer", &[("en", "Bill to"), ("ms", "Bil kepada")], "Example Customer\n44 Orchard Lane\nKuala Lumpur"),
            optional(text("sales_order", &[("en", "Sales order"), ("ms", "Pesanan jualan")], "SO-000021")),
            field("lines", &[("en", "Items"), ("ms", "Butiran")], PrintKind::Rows(LINE_COLUMNS), PrintValue::Rows(&[SAMPLE_LINE])),
            money("subtotal", &[("en", "Subtotal before tax"), ("ms", "Subjumlah sebelum cukai")], PRICE),
            money("tax_total", &[("en", "Total tax"), ("ms", "Jumlah cukai")], Decimal::from_parts(870, 0, 0, false, 2)),
            money("total", &[("en", "Total including tax"), ("ms", "Jumlah termasuk cukai")], SAMPLE_TOTAL),
            optional(text("balance", &[("en", "Balance summary"), ("ms", "Ringkasan baki")], "As of 15 Oct 2026\nOutstanding: MYR 153.70")),
            optional(text("letterhead", &[("en", "Letterhead"), ("ms", "Kepala surat")], "example.test")),
            optional(text("remarks", &[("en", "Remarks"), ("ms", "Catatan invois")], "Annual service renewal")),
            optional(text("bank", &[("en", "Bank details"), ("ms", "Butiran bank")], "Example Bank")),
            optional(text("terms", &[("en", "Terms"), ("ms", "Terma")], "Payment by bank transfer")),
            PrintField {
                optional: true,
                ..text("ringgit", &[("en", "Ringgit equivalents"), ("ms", "Amaun setara Ringgit")], "Exchange rate: 4.5 MYR / USD")
            },
            optional(text("footer", &[("en", "Notes"), ("ms", "Catatan")], "Keep this copy for your records.")),
            optional(field("logo", &[("en", "Company logo"), ("ms", "Logo syarikat")], PrintKind::Raster, PrintValue::Raster {
                mime: "image/png",
                bytes: include_bytes!("../prints/sample-mark.png"),
            })),
            PrintField {
                optional: true,
                ..field("validation", &[("en", "E-invoice validation"), ("ms", "Pengesahan e-Invois")], PrintKind::QrCode,
                    PrintValue::QrCode("https://example.test/document/validate"))
            },
            $($extra),*
        ]
    };
}

macro_rules! document {
    ($name:ident, $key:ident, $label:literal, $file:literal, $($extra:expr),+ $(,)?) => {
        static $name: PrintContract = PrintContract {
            key: $key,
            label: $label,
            version: "1",
            states: &["issued"],
            fields: fields!($($extra),+),
            base_language: "en",
            defaults: &[
                PrintDefault { language: "en", version: "1", layout: include_str!(concat!("../prints/", $file, ".en.json")) },
                PrintDefault { language: "ms", version: "1", layout: include_str!(concat!("../prints/", $file, ".ms.json")) },
            ],
        };
        inventory::submit! {
            Contribution::new(PRINT_CATALOGUE, $key, Provenance {
                module: crate::MODULE, source: file!(), line: line!()
            }, &$name)
        }
    };
}

document!(
    INVOICE,
    INVOICE_KEY,
    "Invoice",
    "invoice",
    field(
        "due_date",
        &[("en", "Due date"), ("ms", "Tarikh akhir")],
        PrintKind::Date,
        PrintValue::Date(date(2026, 11, 15))
    ),
);
document!(
    CREDIT,
    CREDIT_KEY,
    "Credit note",
    "credit-note",
    text(
        "original_invoice",
        &[("en", "Original invoice"), ("ms", "Invois asal")],
        "INV-000032"
    ),
    field(
        "original_date",
        &[
            ("en", "Original invoice date"),
            ("ms", "Tarikh invois asal")
        ],
        PrintKind::Date,
        PrintValue::Date(date(2026, 10, 3))
    ),
    text(
        "reason",
        &[("en", "Adjustment reason"), ("ms", "Sebab pelarasan")],
        "Service credit"
    ),
);

pub(super) fn for_kind(kind: &str) -> &'static PrintContract {
    if kind == "credit_note" {
        &CREDIT
    } else {
        &INVOICE
    }
}
