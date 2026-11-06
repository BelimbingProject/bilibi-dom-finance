//! Aged payables: what each supplier is owed, by how late it is, as of a day.
//! Read through `finance.payables.aging.get`, which fixes every figure to one
//! posting number so the buckets and the bills add up.
use super::{company, format as f, render, Pages};
use crate::{AgedDocument, AgedPayables, AgingQuery};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::MinorUnits;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BuiltinColumn, Cell, EmptyState, Field, FlexRow, FlexTable, Html, InputKind,
    ListColumns, TableEmpty,
};
use jiff::civil::Date;
use std::collections::HashMap;

pub(super) struct Aged {
    pages: Pages,
    as_of: Option<Date>,
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

fn bill_columns(id: &str) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        [
            ("party", "Supplier"),
            ("bill", "Bill"),
            ("party_number", "Supplier's number"),
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
        Ok(Self {
            pages,
            as_of: query.get("as_of").and_then(|d| d.parse().ok()),
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

    fn bill_rows(&self, documents: Vec<&AgedDocument>, prefix: &str) -> Result<Vec<FlexRow>> {
        let mut rows = Vec::new();
        for d in documents {
            let mut row = FlexRow::new(d.id.to_string());
            row.dom_id = Some(format!("{prefix}-{}", d.id));
            rows.push(
                row.cell(
                    "party",
                    Cell::text(format!("{} · {}", d.party_code, d.party_name)),
                )
                .slot(
                    "bill",
                    render(BillLink {
                        id: d.id,
                        text: format!("{} #{}", f::kind(&d.kind), d.document_number),
                        query:self.aged.as_ref().map(|r|format!("?as_of={}&through_entry_number={}&through_opening_batch_id={}&through_settlement_number={}",r.as_of,r.through_entry_number,r.through_opening_batch_id,r.through_settlement_number)).unwrap_or_default(),
                    })?,
                )
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
            "No balances as of this date.",
        )));
        render(table)
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let as_of = match self.as_of {
            Some(date) => date,
            None => self.pages.today(scope).await?,
        };
        let date = render(DateForm {
            field: Field::named("as_of")
                .id("payables-aged-as-of")
                .kind(InputKind::Date)
                .label("As of")
                .value(as_of.to_string()),
        })?;
        let Some(aged) = &self.aged else {
            let mut empty = EmptyState::missing(
                "Aged payables unavailable",
                "What is owed to suppliers cannot be read with your current access.",
            );
            empty.id = "payables-aged-unavailable".into();
            return View {
                title_actions: bilibi_ui::Html::EMPTY,
                date,
                parties: None,
                bills: None,
                credits: None,
                empty: Some(render(empty)?),
                opening_credits: None,
                summary: Html::EMPTY,
            }
            .render()
            .map_err(|_| Error::Internal);
        };
        let labels = bucket_labels(&aged.bucket_days);
        let mut rows = Vec::new();
        for p in &aged.parties {
            let mut row = FlexRow::new(p.party_key.clone());
            row.dom_id = Some(format!("payables-aged-party-{}", p.party_key));
            row = row.cell(
                "party",
                Cell::text(format!("{} · {}", p.party_code, p.party_name)),
            );
            for (i, amount) in p.buckets.iter().enumerate() {
                row = row.cell(&format!("b{i}"), Cell::text(amount.display(self.units)));
            }
            rows.push(
                row.cell("credit", Cell::text(p.credit_total.display(self.units)))
                    .cell("total", Cell::text(p.total.display(self.units))),
            );
        }
        if !aged.parties.is_empty() {
            let mut row = FlexRow::new("all");
            row.dom_id = Some("payables-aged-total".into());
            row = row.cell("party", Cell::text("All suppliers"));
            for (i, amount) in aged.buckets.iter().enumerate().take(labels.len()) {
                row = row.cell(&format!("b{i}"), Cell::text(amount.display(self.units)));
            }
            rows.push(row.cell("total", Cell::text(aged.total.display(self.units))));
        }
        let documents = aged.parties.iter().flat_map(|p| &p.documents).collect();
        let bills = (!aged.parties.is_empty())
            .then(|| {
                Self::table(
                    &self.bills,
                    "bills_grid",
                    "Bills owed",
                    self.bill_rows(documents, "payables-aged-bill")?,
                )
            })
            .transpose()?;
        let credits = (!aged.exceptions.is_empty())
            .then(|| {
                Self::table(
                    &self.credits,
                    "credits_grid",
                    "Credit balances",
                    self.bill_rows(aged.exceptions.iter().collect(), "payables-aged-credit")?,
                )
            })
            .transpose()?;
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            date,
            parties: Some(Self::table(
                &self.parties,
                "parties_grid",
                "Owed by supplier",
                rows,
            )?),
            bills,
            credits,
            empty: None,
            summary: Html::EMPTY,
            opening_credits: Some(Self::table(
                &self.opening_credits,
                "opening_credits_grid",
                "Unapplied supplier credits",
                self.bill_rows(
                    aged.parties.iter().flat_map(|p| &p.credits).collect(),
                    "payables-aged-opening-credit",
                )?,
            )?),
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "aged.html")]
struct View {
    title_actions: Html,
    date: Html,
    parties: Option<Html>,
    bills: Option<Html>,
    credits: Option<Html>,
    empty: Option<Html>,
    opening_credits: Option<Html>,
    summary: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "forms.html" as form %}<form id="payables-aged-filters" data-event="params" data-change class="mb-3 max-w-xs">{% call form::input(field=field) %}{% endcall %}</form>"#,
    ext = "html"
)]
struct DateForm {
    field: Field,
}

#[derive(Template)]
#[template(
    source = r#"<a href="/finance/payables/bills/{{ id }}{{ query }}" class="font-medium text-ink-strong hover:underline">{{ text }}</a>"#,
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
                    true
                }
                LiveInput::Event(v) => match v["type"].as_str() {
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
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query,
                ..LiveBody::default()
            })
        })
    }
}
