//! Aged payables: what each supplier is owed, by how late it is, as of a day.
//! Read through `finance.payables.aging.get`, which fixes every figure to one
//! posting number so the buckets and the bills add up.
use super::{company, format as f, render, Pages, AGED};
use crate::{AgedDocument, AgedPayables, AgingQuery};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BuiltinColumn, Cell, EmptyState, FilterToolbar, FlexRow, FlexTable, Html, ListColumns,
    TableEmpty, ToolbarControl,
};
use jiff::civil::Date;
use std::collections::HashMap;

pub(super) struct Aged {
    pages: Pages,
    as_of: Option<Date>,
    /// The one supplier's bucket a figure was opened on (`?supplier=&bucket=`).
    supplier: Option<String>,
    bucket: Option<usize>,
    aged: Option<AgedPayables>,
    units: Option<MinorUnits>,
    parties: ListColumns,
    bills: ListColumns,
    credits: ListColumns,
    opening_credits: ListColumns,
    unavailable: bool,
}

/// `Not yet due`, `1–30 days late`, … `Over 90 days late`, from the report's
/// boundaries.
fn bucket_labels(days: &[i32]) -> Vec<String> {
    let mut labels = vec!["Not yet due".to_owned()];
    let mut from = 1;
    for to in days {
        labels.push(format!("{from}–{to} days late"));
        from = to + 1;
    }
    labels.push(format!(
        "Over {} days late",
        days.last().copied().unwrap_or(0)
    ));
    labels
}

/// The one supplier's bucket an address narrows the bills to: both or neither.
fn narrow(query: &HashMap<String, String>) -> (Option<String>, Option<usize>) {
    let supplier = query
        .get("supplier")
        .filter(|key| !key.is_empty() && key.len() <= 200)
        .cloned();
    let bucket = query.get("bucket").and_then(|b| b.parse::<usize>().ok());
    match (supplier, bucket) {
        (Some(s), Some(b)) => (Some(s), Some(b)),
        _ => (None, None),
    }
}

fn bill_columns(id: &str) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        [
            ("bill", "Bill"),
            ("party", "Supplier"),
            ("party_number", "Supplier's invoice number"),
            ("date", "Date"),
            ("due", "Due"),
            ("late", "Days late"),
            ("outstanding", "Owed"),
        ]
        .into_iter()
        .map(|(id, label)| {
            let c = BuiltinColumn::new(id, label);
            if matches!(id, "late" | "outstanding") {
                c.right()
            } else {
                c
            }
        })
        .collect(),
    )
    .map_err(|_| Error::Internal)
}

impl Aged {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let (supplier, bucket) = narrow(query);
        Ok(Self {
            pages,
            as_of: query.get("as_of").and_then(|d| d.parse().ok()),
            supplier,
            bucket,
            aged: None,
            units: None,
            // The buckets are known only once the report is read, so the
            // supplier table is mounted for the default boundaries.
            parties: ListColumns::mount(
                "payables-aged-table",
                std::iter::once(BuiltinColumn::new("party", "Supplier"))
                    .chain(
                        bucket_labels(&AgingQuery::default().bucket_days)
                            .into_iter()
                            .enumerate()
                            .map(|(i, label)| BuiltinColumn::new(&format!("b{i}"), &label).right()),
                    )
                    .chain(std::iter::once(
                        BuiltinColumn::new("credit", "Unapplied credits").right(),
                    ))
                    .chain(std::iter::once(
                        BuiltinColumn::new("total", "Net owed").right(),
                    ))
                    .collect(),
            )
            .map_err(|_| Error::Internal)?,
            bills: bill_columns("payables-aged-bills-table")?,
            credits: bill_columns("payables-aged-credits-table")?,
            opening_credits: bill_columns("payables-aged-opening-credits-table")?,
            unavailable: false,
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.aged = None;
        self.unavailable = false;
        let Some(company) = company(scope) else {
            self.unavailable = true;
            return Ok(());
        };
        let as_of = match self.as_of {
            Some(date) => date,
            None => self.pages.today(scope).await?,
        };
        self.units = self.pages.units(scope).await;
        let answer = self
            .pages
            .contracts
            .read(
                scope,
                "finance.payables.aging.get",
                &serde_json::json!({"company_id":company,"as_of":as_of.to_string()}),
            )
            .await;
        let aged = answer.and_then(|v| {
            serde_json::from_value::<AgedPayables>(v["result"].clone())
                .map_err(|_| bilibi_agent_api::Error::from(Error::Internal))
        });
        match aged {
            Ok(aged) => {
                let docs: Vec<&AgedDocument> =
                    aged.parties.iter().flat_map(|p| &p.documents).collect();
                self.bills.load(&docs, |d| d.id.to_string());
                self.credits.load(&aged.exceptions, |d| d.id.to_string());
                let credits: Vec<_> = aged.parties.iter().flat_map(|p| &p.credits).collect();
                self.opening_credits.load(&credits, |d| d.id.to_string());
                self.parties.load(&aged.parties, |p| p.party_key.clone());
                self.aged = Some(aged);
            }
            Err(_) => self.unavailable = true,
        }
        Ok(())
    }

    /// The address of this report narrowed to one supplier's bucket, or of
    /// the whole report when `narrow` is `None`.
    fn href(&self, as_of: Date, narrow: Option<(&str, usize)>) -> String {
        match narrow {
            Some((supplier, bucket)) => format!(
                "{AGED}?as_of={as_of}&supplier={}&bucket={bucket}",
                supplier.replace('&', "%26")
            ),
            None => format!("{AGED}?as_of={as_of}"),
        }
    }

    /// An amount in a bucket cell: a link to the bills behind it, or "—".
    fn bucket_cell(&self, amount: &Money, href: Option<String>) -> Result<Html> {
        if amount.amount().is_zero() {
            return Ok(Html::text("—"));
        }
        let text = amount.display(self.units);
        match href {
            Some(href) => render(Link { href, text }),
            None => Ok(Html::text(&text)),
        }
    }

    fn bill_rows(&self, documents: Vec<&AgedDocument>, prefix: &str) -> Result<Vec<FlexRow>> {
        let mut rows = Vec::new();
        for d in documents {
            let mut row = FlexRow::new(d.id.to_string());
            row.dom_id = Some(format!("{prefix}-{}", d.id));
            rows.push(
                row.slot(
                    "bill",
                    render(BillLink {
                        id: d.id,
                        text: format!("{} #{}", f::kind(&d.kind), d.document_number),
                        query:self.aged.as_ref().map(|r|format!("?as_of={}&through_entry_number={}&through_opening_batch_id={}&through_settlement_number={}",r.as_of,r.through_entry_number,r.through_opening_batch_id,r.through_settlement_number)).unwrap_or_default(),
                    })?,
                )
                .cell("party", Cell::text(&d.party_name))
                .cell(
                    "party_number",
                    Cell::text(f::optional(&d.party_document_number)),
                )
                .cell("date", Cell::text(d.document_date.to_string()))
                .cell("due", Cell::text(d.due_date.to_string()))
                .cell(
                    "late",
                    Cell::text(if d.days_overdue > 0 {
                        d.days_overdue.to_string()
                    } else {
                        "—".into()
                    }),
                )
                .cell("outstanding", Cell::text(d.outstanding.display(self.units))),
            );
        }
        Ok(rows)
    }

    fn table(
        columns: &ListColumns,
        event: &'static str,
        caption: &'static str,
        rows: Vec<FlexRow>,
    ) -> Result<Html> {
        let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
        table.event = event;
        table.caption = caption;
        table.framed = false;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            "Nothing is owed on this date.",
        )));
        render(table)
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let as_of = match self.as_of {
            Some(date) => date,
            None => self.pages.today(scope).await?,
        };
        let toolbar = FilterToolbar {
            event: "filters",
            ..FilterToolbar::new(
                "payables-aged-filters",
                vec![ToolbarControl::date(
                    "payables-aged-as-of",
                    "as_of",
                    "As of",
                    &as_of.to_string(),
                )],
            )
        };
        let mut cards = Vec::new();
        let Some(aged) = &self.aged else {
            let mut empty = EmptyState::missing(
                "Aged payables unavailable",
                "What is owed to suppliers cannot be read with your current access.",
            );
            empty.id = "payables-aged-unavailable".into();
            cards.push(render(empty)?);
            return View {
                title_actions: Html::EMPTY,
                toolbar,
                cards,
            }
            .render()
            .map_err(|_| Error::Internal);
        };
        let labels = bucket_labels(&aged.bucket_days);
        let mut rows = Vec::new();
        let mut credit_total = rust_decimal::Decimal::ZERO;
        for p in &aged.parties {
            credit_total += p.credit_total.amount();
            let mut row = FlexRow::new(p.party_key.clone());
            row.dom_id = Some(format!("payables-aged-party-{}", p.party_key));
            row = row.cell("party", Cell::text(&p.party_name));
            for (i, amount) in p.buckets.iter().enumerate() {
                row = row.slot(
                    &format!("b{i}"),
                    self.bucket_cell(amount, Some(self.href(as_of, Some((&p.party_key, i)))))?,
                );
            }
            rows.push(
                row.slot("credit", self.bucket_cell(&p.credit_total, None)?)
                    .cell("total", Cell::text(p.total.display(self.units))),
            );
        }
        if !aged.parties.is_empty() {
            let mut row = FlexRow::new("all");
            row.dom_id = Some("payables-aged-total".into());
            row = row.slot(
                "party",
                render(Strong {
                    text: "Total".into(),
                })?,
            );
            for (i, amount) in aged.buckets.iter().enumerate().take(labels.len()) {
                row = row.slot(&format!("b{i}"), self.bucket_cell(amount, None)?);
            }
            rows.push(
                row.cell(
                    "credit",
                    Cell::text(if credit_total.is_zero() {
                        "—".to_owned()
                    } else {
                        bilibi_finance_ledger::Money::display_decimal(credit_total, self.units)
                    }),
                )
                .slot(
                    "total",
                    render(Strong {
                        text: format!(
                            "{} {}",
                            aged.total.display(self.units),
                            aged.parties
                                .first()
                                .and_then(|p| p.documents.first().or(p.credits.first()))
                                .map_or("", |d| d.currency_code.as_str())
                        ),
                    })?,
                ),
            );
        }
        cards.push(render(Section {
            id: "payables-aged-parties",
            label: "By supplier".into(),
            description: "",
            actions: Html::EMPTY,
            body: Self::table(&self.parties, "parties_grid", "Owed by supplier", rows)?,
        })?);

        // The bills behind the figures, or behind the one figure chosen.
        let documents = aged
            .parties
            .iter()
            .filter(|p| self.supplier.as_ref().is_none_or(|key| *key == p.party_key))
            .flat_map(|p| &p.documents)
            .filter(|d| self.bucket.is_none_or(|b| b == d.bucket))
            .collect();
        let (title, show_all) = match (&self.supplier, self.bucket) {
            (Some(key), Some(bucket)) => {
                let name = aged
                    .parties
                    .iter()
                    .find(|p| p.party_key == *key)
                    .map_or("", |p| p.party_name.as_str());
                (
                    format!(
                        "Bills still owed · {name} · {}",
                        labels.get(bucket).map_or("", String::as_str)
                    ),
                    render(bilibi_ui::Button {
                        id: "payables-aged-show-all",
                        href: &self.href(as_of, None),
                        body: Html::text("Show all"),
                        ..bilibi_ui::Button::default()
                    })?,
                )
            }
            _ => ("Bills still owed".to_owned(), Html::EMPTY),
        };
        cards.push(render(Section {
            id: "payables-aged-bills",
            label: title,
            description: "",
            actions: show_all,
            body: Self::table(
                &self.bills,
                "bills_grid",
                "Bills still owed",
                self.bill_rows(documents, "payables-aged-bill")?,
            )?,
        })?);
        if !aged.exceptions.is_empty() {
            cards.push(render(Section {
                id: "payables-aged-credits",
                label: "Credit balances".into(),
                description: "Bills a payment or debit note has paid more than in full.",
                actions: Html::EMPTY,
                body: Self::table(
                    &self.credits,
                    "credits_grid",
                    "Credit balances",
                    self.bill_rows(aged.exceptions.iter().collect(), "payables-aged-credit")?,
                )?,
            })?);
        }
        let opening: Vec<&AgedDocument> = aged.parties.iter().flat_map(|p| &p.credits).collect();
        if !opening.is_empty() {
            cards.push(render(Section {
                id: "payables-aged-opening-credits",
                label: "Unapplied supplier credits".into(),
                description: "They age from their original dates, apart from unpaid bills.",
                actions: Html::EMPTY,
                body: Self::table(
                    &self.opening_credits,
                    "opening_credits_grid",
                    "Unapplied supplier credits",
                    self.bill_rows(opening, "payables-aged-opening-credit")?,
                )?,
            })?);
        }
        View {
            title_actions: Html::EMPTY,
            toolbar,
            cards,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "aged.html")]
struct View<'a> {
    title_actions: Html,
    toolbar: FilterToolbar<'a>,
    cards: Vec<Html>,
}

/// One titled table in a flat card, as the receivables aging draws it.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% let heading = "{}-heading"|format(id) %}{% call ui::card(id=id, inner_class="p-0", role="region", labelledby=heading) %}{% call ui::section_heading(id=heading, title=label, class="p-5 pb-0", description=description, actions=actions) %}{% endcall %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
struct Section<'a> {
    id: &'a str,
    /// Not `title`: inside the card's body that name is the card's own.
    label: String,
    description: &'a str,
    actions: Html,
    body: Html,
}

#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="text-action tabular-nums hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct Link {
    href: String,
    text: String,
}

#[derive(Template)]
#[template(
    source = r#"<strong class="font-semibold tabular-nums">{{ text }}</strong>"#,
    ext = "html"
)]
struct Strong {
    text: String,
}

#[derive(Template)]
#[template(
    source = r#"<a href="/finance/payables/bills/{{ id }}{{ query }}" class="text-action hover:underline">{{ text }}</a>"#,
    ext = "html"
)]
struct BillLink {
    query: String,
    id: i64,
    text: String,
}

impl LiveState<Scope, Error> for Aged {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh => true,
                LiveInput::Params(values) => {
                    self.as_of = values.get("as_of").and_then(|d| d.parse().ok());
                    (self.supplier, self.bucket) = narrow(values);
                    true
                }
                LiveInput::Event(v) => match v["type"].as_str() {
                    Some("filters") => {
                        let form = params::form_values(v, "filters");
                        if let Some(date) = form.get("as_of").and_then(|d| d.parse().ok()) {
                            self.as_of = Some(date);
                        }
                        true
                    }
                    Some(
                        event @ ("parties_grid"
                        | "bills_grid"
                        | "credits_grid"
                        | "opening_credits_grid"),
                    ) => {
                        let columns = match event {
                            "parties_grid" => &mut self.parties,
                            "bills_grid" => &mut self.bills,
                            "opening_credits_grid" => &mut self.opening_credits,
                            _ => &mut self.credits,
                        };
                        columns.handle(&params::string_values(v));
                        false
                    }
                    _ => return Err(Error::Invalid),
                },
            };
            if reload {
                self.reload(r.scope).await?;
            }
            let mut query = HashMap::new();
            if let Some(date) = self.as_of {
                query.insert("as_of".to_owned(), date.to_string());
            }
            if let (Some(supplier), Some(bucket)) = (&self.supplier, self.bucket) {
                query.insert("supplier".to_owned(), supplier.clone());
                query.insert("bucket".to_owned(), bucket.to_string());
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query,
                ..LiveBody::default()
            })
        })
    }
}
