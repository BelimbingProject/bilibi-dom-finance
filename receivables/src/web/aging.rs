//! Who owes what, by how late. Reads `finance.receivables.aging.get`, which
//! prices every issued invoice at one posting number so the columns agree.
use super::{
    format::{self as f},
    render, Flash, Pages, AGING, CUSTOMERS, INVOICES,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Button, Card, Cell, EmptyState, FilterToolbar, FlexRow, Header, Html, ListColumns,
    PageVariant, SectionHeading, StatusKind, ToolbarControl,
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
    /// The one customer's bucket a figure was opened on (`?customer=&bucket=`).
    customer: Option<i64>,
    bucket: Option<usize>,
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
        let (customer, bucket) = narrow(query);
        Ok(Self {
            pages,
            as_of,
            customer,
            bucket,
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
                    ("issued", "Date"),
                    ("outstanding", "Unapplied"),
                ],
                &["outstanding"],
            )?,
            invoices: f::columns(
                "receivables-aging-invoices-table",
                &[
                    ("invoice", "Invoice"),
                    ("customer", "Customer"),
                    ("issued", "Issued"),
                    ("due", "Due"),
                    ("late", "Days late"),
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

    /// The address of this report narrowed to one customer's bucket, or of
    /// the whole report when `narrow` is `None`.
    fn href(&self, narrow: Option<(i64, usize)>) -> String {
        match narrow {
            Some((customer, bucket)) => format!(
                "{AGING}?as_of={}&customer={customer}&bucket={bucket}",
                self.as_of
            ),
            None => format!("{AGING}?as_of={}", self.as_of),
        }
    }

    /// An amount in a bucket cell: a link to the invoices behind it, or "—".
    fn bucket_cell(&self, amount: Decimal, narrow: Option<(i64, usize)>) -> Result<Html> {
        if amount.is_zero() {
            return f::tabular("—");
        }
        let text = self.figures.decimal(amount);
        match narrow {
            Some(narrow) => f::link(&self.href(Some(narrow)), &text),
            None => f::tabular(&text),
        }
    }

    fn body(&self) -> Result<String> {
        let mut body = render(Header {
            title: Html::text("Receivables aging"),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle: "Who owes you money, and how late it is.",
            ..Header::default()
        })?;
        body.push(&render(FilterToolbar {
            event: "filters",
            ..FilterToolbar::new(
                "receivables-aging-filters",
                vec![ToolbarControl::date(
                    "receivables-aging-as-of",
                    "as_of",
                    "As of",
                    &self.as_of,
                )],
            )
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
        let pins = format!(
            "as_of={}&through_entry_number={}&through_opening_batch_id={}&through_settlement_number={}",
            r.as_of, r.through_entry_number, r.through_opening_batch_id, r.through_settlement_number
        );
        let mut cards: Vec<Html> = Vec::new();
        // The five default buckets name the columns; a bucket beyond them
        // (the contract allows custom cut-offs) folds into the last.
        let mut rows = Vec::new();
        let mut credit_total = Decimal::ZERO;
        for c in &r.customers {
            credit_total += c.credit_total;
            let mut row = FlexRow::new(c.customer_id.to_string());
            row.dom_id = Some(format!("receivables-aging-customer-{}", c.customer_id));
            row = row.slot(
                "customer",
                f::link(&format!("{CUSTOMERS}/{}", c.customer_id), &c.name)?,
            );
            for (i, total) in c.bucket_totals.iter().enumerate().take(5) {
                row = row.slot(
                    &format!("b{i}"),
                    self.bucket_cell(*total, Some((c.customer_id, i)))?,
                );
            }
            rows.push(
                row.slot("credits", self.bucket_cell(c.credit_total, None)?)
                    .slot("total", f::tabular(&self.figures.decimal(c.total))?),
            );
        }
        if !r.customers.is_empty() {
            let mut row = FlexRow::new("all");
            row.dom_id = Some("receivables-aging-total".into());
            row = row.slot("customer", f::strong("Total")?);
            for (i, total) in r.bucket_totals.iter().enumerate().take(5) {
                row = row.slot(&format!("b{i}"), self.bucket_cell(*total, None)?);
            }
            rows.push(
                row.slot("credits", self.bucket_cell(credit_total, None)?)
                    .slot(
                        "total",
                        f::strong(&format!(
                            "{} {}",
                            self.figures.decimal(r.total),
                            r.currency_code
                        ))?,
                    ),
            );
        }
        cards.push(render(Card {
            id: "receivables-aging-customers",
            role: "region",
            labelledby: "receivables-aging-customers-heading",
            inner_class: "p-0",
            body: {
                let mut b = render(SectionHeading {
                    id: "receivables-aging-customers-heading",
                    title: "By customer",
                    class: "p-5 pb-0",
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.customers,
                    "customers_grid",
                    "Receivables aging by customer",
                    rows,
                    "Nobody owes anything on this date.",
                )?);
                b
            },
            ..Card::default()
        })?);

        // The invoices behind the figures, or behind the one figure chosen.
        let narrowed = self.customer.zip(self.bucket);
        let mut invoice_rows = Vec::new();
        for c in &r.customers {
            if self.customer.is_some_and(|id| id != c.customer_id) {
                continue;
            }
            for d in &c.documents {
                if self.bucket.is_some_and(|b| b != d.bucket.min(4)) {
                    continue;
                }
                let mut row = FlexRow::new(d.document_id.to_string());
                row.dom_id = Some(format!("receivables-aging-invoice-{}", d.document_id));
                invoice_rows.push(
                    row.slot(
                        "invoice",
                        f::link(
                            &format!("{INVOICES}/{}?{pins}", d.document_id),
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
                    .cell("issued", Cell::text(&d.issue_date))
                    .cell("due", Cell::text(&d.due_date))
                    .slot(
                        "late",
                        f::tabular(&if d.days_overdue > 0 {
                            d.days_overdue.to_string()
                        } else {
                            "—".into()
                        })?,
                    )
                    .slot(
                        "outstanding",
                        f::tabular(&self.figures.decimal(d.outstanding))?,
                    ),
                );
            }
        }
        let (title, show_all) = match narrowed {
            Some((customer, bucket)) => {
                let name = r
                    .customers
                    .iter()
                    .find(|c| c.customer_id == customer)
                    .map_or("", |c| c.name.as_str());
                let age = r.buckets.get(bucket).map(bucket_label).unwrap_or_default();
                (
                    format!("Invoices still owed · {name} · {age}"),
                    render(Button {
                        id: "receivables-aging-show-all",
                        href: &self.href(None),
                        body: Html::text("Show all"),
                        ..Button::default()
                    })?,
                )
            }
            None => ("Invoices still owed".to_owned(), Html::EMPTY),
        };
        cards.push(render(Card {
            id: "receivables-aging-invoices",
            role: "region",
            labelledby: "receivables-aging-invoices-heading",
            inner_class: "p-0",
            body: {
                let mut b = render(SectionHeading {
                    id: "receivables-aging-invoices-heading",
                    title: &title,
                    class: "p-5 pb-0",
                    actions: show_all,
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.invoices,
                    "invoices_grid",
                    "Invoices still owed",
                    invoice_rows,
                    "Nobody owes anything on this date.",
                )?);
                b
            },
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
                                &format!("{INVOICES}/{}?{pins}", credit.document_id),
                                credit
                                    .document_reference
                                    .as_deref()
                                    .unwrap_or("Customer credit"),
                            )?,
                        )
                        .cell("customer", Cell::text(&c.name))
                        .cell("issued", Cell::text(&credit.issue_date))
                        .slot(
                            "outstanding",
                            f::tabular(&self.figures.decimal(credit.outstanding))?,
                        ),
                );
            }
        }
        if !credit_rows.is_empty() {
            cards.push(render(Card {
                id: "receivables-aging-credits",
                role: "region",
                labelledby: "receivables-aging-credits-heading",
                inner_class: "p-0",
                body: {
                    let mut b = render(SectionHeading {
                        id: "receivables-aging-credits-heading",
                        title: "Unapplied customer credits",
                        class: "p-5 pb-0",
                        description: "They age from their original dates, apart from unpaid invoices.",
                        ..SectionHeading::default()
                    })?;
                    b.push(&f::table(
                        &self.credits,
                        "credits_grid",
                        "Unapplied customer credits",
                        credit_rows,
                        "",
                    )?);
                    b
                },
                ..Card::default()
            })?);
        }
        f::frame(
            "receivables-aging",
            PageVariant::List,
            &self.flash,
            {
                body.push(&render(Stack { parts: cards })?);
                body
            },
            None,
        )
    }
}

/// The one customer's bucket an address narrows the invoices to: both or neither.
fn narrow(query: &HashMap<String, String>) -> (Option<i64>, Option<usize>) {
    let customer = params::positive_id(query.get("customer").map(String::as_str));
    let bucket = query
        .get("bucket")
        .and_then(|b| b.parse::<usize>().ok())
        .filter(|b| *b < 5);
    match (customer, bucket) {
        (Some(c), Some(b)) => (Some(c), Some(b)),
        _ => (None, None),
    }
}

/// Cards a section step apart (`ui::stack`, wide).
#[derive(askama::Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stack(parts=parts, wide=true) %}{% endcall %}"#,
    ext = "html"
)]
struct Stack {
    parts: Vec<Html>,
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
                    (self.customer, self.bucket) = narrow(values);
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
                    "filters" => {
                        let form = params::form_values(v, "filters");
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
            let mut query = HashMap::from([("as_of".into(), self.as_of.clone())]);
            if let (Some(customer), Some(bucket)) = (self.customer, self.bucket) {
                query.insert("customer".into(), customer.to_string());
                query.insert("bucket".into(), bucket.to_string());
            }
            Ok(LiveBody {
                html: self.body()?,
                query,
                ..LiveBody::default()
            })
        })
    }
}
