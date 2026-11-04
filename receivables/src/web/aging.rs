//! Who owes what, by how late. Reads `finance.receivables.aging.get`, which
//! prices every issued invoice at one posting number so the columns agree.
use super::{
    format::{self as f},
    render, Flash, Pages, CUSTOMERS, INVOICES,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Button, ButtonVariant, Card, Cell, EmptyState, Facts, Field, FlexRow, Form, Header,
    Html, InputKind, ListColumns, PageVariant, SectionHeading, StatusKind,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Report {
    as_of: String,
    through_entry_number: i64,
    through_opening_batch_id: i64,
    through_settlement_number: i64,
    gross_debits: Decimal,
    gross_credits: Decimal,
    currency_code: String,
    minor_units: u32,
    buckets: Vec<Bucket>,
    customers: Vec<Customer>,
    bucket_totals: Vec<Decimal>,
    total: Decimal,
}
#[derive(Deserialize)]
struct Bucket {
    from_days: Option<i64>,
    to_days: Option<i64>,
}
#[derive(Deserialize)]
struct Customer {
    customer_id: i64,
    code: String,
    name: String,
    documents: Vec<Invoice>,
    credits: Vec<Invoice>,
    credit_total: Decimal,
    bucket_totals: Vec<Decimal>,
    total: Decimal,
}
#[derive(Deserialize)]
struct Invoice {
    document_id: i64,
    document_reference: Option<String>,
    party_name: String,
    due_date: String,
    issue_date: String,
    outstanding: Decimal,
    days_overdue: i64,
    bucket: usize,
}

fn bucket_label(b: &Bucket) -> String {
    match (b.from_days, b.to_days) {
        (None, Some(_)) => "Not yet due".into(),
        (Some(from), Some(to)) => format!("{from}–{to} days late"),
        (Some(from), None) => format!("Over {} days late", from - 1),
        (None, None) => "All".into(),
    }
}

pub(super) struct Aging {
    pages: Pages,
    as_of: String,
    error: Option<String>,
    report: Option<Report>,
    figures: f::Figures,
    flash: Flash,
    customers: ListColumns,
    invoices: ListColumns,
    credits: ListColumns,
}

impl Aging {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let as_of = query
            .get("as_of")
            .filter(|d| d.parse::<jiff::civil::Date>().is_ok())
            .cloned()
            .unwrap_or_default();
        // The default cut-offs give five buckets: not yet due, then 1–30,
        // 31–60, 61–90 and over 90 days late.
        let mut columns = vec![("customer", "Customer")];
        columns.extend([
            ("b0", "Not yet due"),
            ("b1", "1–30 days late"),
            ("b2", "31–60 days late"),
            ("b3", "61–90 days late"),
            ("b4", "Over 90 days late"),
            ("credits", "Unapplied credits"),
            ("total", "Net owed"),
        ]);
        Ok(Self {
            pages,
            as_of,
            error: None,
            report: None,
            figures: f::Figures::default(),
            flash: Flash::default(),
            customers: f::columns(
                "receivables-aging-customers-table",
                &columns,
                &["b0", "b1", "b2", "b3", "b4", "credits", "total"],
            )?,
            credits: f::columns(
                "receivables-aging-credits-table",
                &[
                    ("invoice", "Credit"),
                    ("customer", "Customer"),
                    ("due", "Source date"),
                    ("outstanding", "Unapplied"),
                ],
                &["outstanding"],
            )?,
            invoices: f::columns(
                "receivables-aging-invoices-table",
                &[
                    ("invoice", "Invoice"),
                    ("customer", "Customer"),
                    ("due", "Due"),
                    ("late", "Days late"),
                    ("bucket", "Age"),
                    ("outstanding", "Owed"),
                ],
                &["late", "outstanding"],
            )?,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.body()
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        if self.as_of.is_empty() {
            let display = self.pages.display(scope).await?;
            self.as_of = today(&display.timezone);
        }
        self.error = None;
        self.report = match self
            .pages
            .read(
                scope,
                "finance.receivables.aging.get",
                json!({"as_of": self.as_of}),
            )
            .await
        {
            Ok(value) => serde_json::from_value(value).ok(),
            Err(Error::Forbidden | Error::NotFound) => {
                self.error = Some(
                    "Aging needs permission to read the ledger as well as receivables.".into(),
                );
                None
            }
            Err(Error::Invalid) => {
                self.error = Some("Choose a valid date.".into());
                None
            }
            Err(error) => return Err(error),
        };
        if let Some(r) = &self.report {
            self.figures = f::Figures::book(&r.currency_code, r.minor_units);
            self.customers
                .load(&r.customers, |c| c.customer_id.to_string());
            let all: Vec<&Invoice> = r.customers.iter().flat_map(|c| &c.documents).collect();
            self.invoices.load(&all, |d| d.document_id.to_string());
            let credits: Vec<&Invoice> = r.customers.iter().flat_map(|c| &c.credits).collect();
            self.credits.load(&credits, |d| d.document_id.to_string());
        }
        Ok(())
    }

    fn body(&self) -> Result<String> {
        let form = render(Form {
            id: "receivables-aging-form",
            event: "set_date",
            change: false,
            class: "",
            body: render(
                Field::in_form("aging", "as_of")
                    .id("receivables-aging-as-of")
                    .label("As of")
                    .kind(InputKind::Date)
                    .value(self.as_of.clone())
                    .hint("Later invoices and receipts are excluded."),
            )?,
            actions: render(Button {
                id: "receivables-aging-show",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Show"),
                ..Button::default()
            })?,
        })?;
        let mut body = render(Header {
            title: Html::text("Receivables aging"),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle: "Who owes you money, and how late it is.",
            ..Header::default()
        })?;
        body.push(&render(Card {
            id: "receivables-aging-date",
            body: form,
            ..Card::default()
        })?);
        let Some(r) = &self.report else {
            let mut empty = EmptyState::missing(
                "Aging unavailable",
                self.error
                    .as_deref()
                    .unwrap_or("The aging report could not be read."),
            );
            empty.id = "receivables-aging-unavailable".into();
            body.push(&render(empty)?);
            return f::frame(
                "receivables-aging",
                PageVariant::List,
                &self.flash,
                body,
                None,
            );
        };
        body.push(&render(Facts {
            id: "receivables-aging-summary",
            items: vec![
                f::fact("Owed as of", &r.as_of),
                f::fact(
                    "Gross debit balances",
                    &self.figures.decimal(r.gross_debits),
                ),
                f::fact(
                    "Unapplied credit balances",
                    &self.figures.decimal(r.gross_credits),
                ),
                f::fact(
                    "Read through settlement",
                    &format!("#{}", r.through_settlement_number),
                ),
                f::fact(
                    "Total owed",
                    &format!("{} {}", self.figures.decimal(r.total), r.currency_code),
                ),
                f::fact("Customers owing", &r.customers.len().to_string()),
                f::fact(
                    "Read through opening batch",
                    &format!("#{}", r.through_opening_batch_id),
                ),
                f::fact(
                    "Read through ledger entry",
                    &format!("#{}", r.through_entry_number),
                ),
            ],
        })?);
        // The five default buckets name the columns; a bucket beyond them
        // (the contract allows custom cut-offs) folds into the last.
        let mut rows = Vec::new();
        for c in &r.customers {
            let mut row = FlexRow::new(c.customer_id.to_string());
            row.dom_id = Some(format!("receivables-aging-customer-{}", c.customer_id));
            row = row.slot(
                "customer",
                f::link(
                    &format!("{CUSTOMERS}/{}", c.customer_id),
                    &format!("{} {}", c.code, c.name),
                )?,
            );
            for (i, total) in c.bucket_totals.iter().enumerate().take(5) {
                row = row.slot(
                    &format!("b{i}"),
                    f::tabular(&if total.is_zero() {
                        "—".into()
                    } else {
                        self.figures.decimal(*total)
                    })?,
                );
            }
            rows.push(
                row.slot(
                    "credits",
                    f::tabular(&self.figures.decimal(c.credit_total))?,
                )
                .slot("total", f::tabular(&self.figures.decimal(c.total))?),
            );
        }
        let labels: Vec<String> = r.buckets.iter().map(bucket_label).collect();
        let amounts: Vec<String> = r
            .bucket_totals
            .iter()
            .map(|t| self.figures.decimal(*t))
            .collect();
        let totals: Vec<_> = labels
            .iter()
            .zip(&amounts)
            .map(|(l, a)| f::fact(l, a))
            .collect();
        body.push(&render(SectionHeading {
            id: "receivables-aging-customers-heading",
            title: "By customer",
            class: "mt-4",
            description:
                "A day late is counted from the due date. An invoice due today is not yet late.",
            ..SectionHeading::default()
        })?);
        body.push(&render(Card {
            id: "receivables-aging-customers",
            role: "region",
            labelledby: "receivables-aging-customers-heading",
            inner_class: "p-0",
            body: {
                let mut b = f::table(
                    &self.customers,
                    "customers_grid",
                    "Receivables aging by customer",
                    rows,
                    "Nobody owes anything on this date.",
                )?;
                b.push(&render(Facts {
                    id: "receivables-aging-bucket-totals",
                    items: totals,
                })?);
                b
            },
            ..Card::default()
        })?);
        let mut invoice_rows = Vec::new();
        for c in &r.customers {
            for d in &c.documents {
                let mut row = FlexRow::new(d.document_id.to_string());
                row.dom_id = Some(format!("receivables-aging-invoice-{}", d.document_id));
                invoice_rows.push(
                    row.slot(
                        "invoice",
                        f::link(
                            &format!("{INVOICES}/{}?as_of={}&through_entry_number={}&through_opening_batch_id={}&through_settlement_number={}",d.document_id,r.as_of,r.through_entry_number,r.through_opening_batch_id,r.through_settlement_number),
                            d.document_reference.as_deref().unwrap_or("Invoice"),
                        )?,
                    )
                    .cell(
                        "customer",
                        Cell::text(&if d.party_name == c.name {
                            c.name.clone()
                        } else {
                            format!("{} (invoiced as {})", c.name, d.party_name)
                        }),
                    )
                    .cell("due", Cell::text(&d.due_date))
                    .slot("late", f::tabular(&d.days_overdue.to_string())?)
                    .cell(
                        "bucket",
                        Cell::text(
                            r.buckets
                                .get(d.bucket)
                                .map_or_else(String::new, bucket_label),
                        ),
                    )
                    .slot(
                        "outstanding",
                        f::tabular(&self.figures.decimal(d.outstanding))?,
                    ),
                );
            }
        }
        body.push(&render(SectionHeading {
            id: "receivables-aging-invoices-heading",
            title: "Invoices still owed",
            class: "mt-4",
            ..SectionHeading::default()
        })?);
        body.push(&render(Card {
            id: "receivables-aging-invoices",
            role: "region",
            labelledby: "receivables-aging-invoices-heading",
            inner_class: "p-0",
            body: f::table(
                &self.invoices,
                "invoices_grid",
                "Invoices still owed",
                invoice_rows,
                "Nobody owes anything on this date.",
            )?,
            ..Card::default()
        })?);
        let mut credit_rows = Vec::new();
        for c in &r.customers {
            for credit in &c.credits {
                credit_rows.push(
                    FlexRow::new(credit.document_id.to_string())
                        .slot(
                            "invoice",
                            f::link(
                                &format!("{INVOICES}/{}?as_of={}&through_entry_number={}&through_opening_batch_id={}&through_settlement_number={}",credit.document_id,r.as_of,r.through_entry_number,r.through_opening_batch_id,r.through_settlement_number),
                                credit
                                    .document_reference
                                    .as_deref()
                                    .unwrap_or("Customer credit"),
                            )?,
                        )
                        .cell("customer", Cell::text(&c.name))
                        .cell("due", Cell::text(&credit.issue_date))
                        .slot(
                            "outstanding",
                            f::tabular(&self.figures.decimal(credit.outstanding))?,
                        ),
                );
            }
        }
        body.push(&render(SectionHeading{title:"Unapplied customer credits",description:"Customer credits age from their original source dates and are shown separately from unpaid invoices.",class:"mt-4",..SectionHeading::default()})?);
        body.push(&render(Card {
            id: "receivables-aging-credits",
            body: f::table(
                &self.credits,
                "credits_grid",
                "Unapplied customer credits",
                credit_rows,
                "No unapplied customer credits on this date.",
            )?,
            ..Card::default()
        })?);
        f::frame(
            "receivables-aging",
            PageVariant::List,
            &self.flash,
            body,
            None,
        )
    }
}

/// The date today in the company's own zone, as `YYYY-MM-DD`.
pub(super) fn today(timezone: &str) -> String {
    let zone = jiff::tz::TimeZone::get(timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    jiff::Timestamp::now().to_zoned(zone).date().to_string()
}

impl LiveState<Scope, Error> for Aging {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.as_of = values
                        .get("as_of")
                        .filter(|d| d.parse::<jiff::civil::Date>().is_ok())
                        .cloned()
                        .unwrap_or_default();
                }
                LiveInput::Event(v) => match v["type"].as_str().ok_or(Error::Invalid)? {
                    "clear-flash" => self.flash.dismiss(v),
                    "customers_grid" => {
                        self.customers.handle(&params::string_values(v));
                    }
                    "credits_grid" => {
                        self.credits.handle(&params::string_values(v));
                    }
                    "invoices_grid" => {
                        self.invoices.handle(&params::string_values(v));
                    }
                    "set_date" => {
                        let form = params::form_values(v, "aging");
                        let date = f::posted(&form, "as_of");
                        if date.parse::<jiff::civil::Date>().is_ok() {
                            self.as_of = date.into();
                        } else {
                            self.flash.set(StatusKind::Error, "Choose a valid date.");
                        }
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            self.load(r.scope).await?;
            Ok(LiveBody {
                html: self.body()?,
                query: HashMap::from([("as_of".into(), self.as_of.clone())]),
                ..LiveBody::default()
            })
        })
    }
}
