//! A retained schedule's record page: its facts and rows, whether it was
//! withdrawn or replaced (and by which schedule), and its local actions,
//! Correct and Withdraw, as icon buttons beside Back.
use super::*;
use bilibi_ui::{BackLink, IconButton};

/// A sentence naming another schedule, with a link to it.
#[derive(Template)]
#[template(
    source = r#"{{ before }}<a id="{{ dom_id }}" href="{{ href }}" class="text-link hover:underline">{{ reference }}</a>{{ after }}"#,
    ext = "html"
)]
struct Linked<'a> {
    before: &'a str,
    dom_id: &'a str,
    href: &'a str,
    reference: &'a str,
    after: &'a str,
}

/// The record page's sections with the standard gap between them.
#[derive(Template)]
#[template(
    source = r#"<div class="space-y-4">{{ body|safe }}</div>"#,
    ext = "html"
)]
struct Stack {
    body: Html,
}

/// The date part of a recorded instant, as pages show a date.
fn day(instant: &Value) -> String {
    bilibi_finance_ledger::dates::display_text(
        instant.as_str().and_then(|t| t.get(..10)).unwrap_or(""),
    )
}

impl State {
    /// Back to this kind's list, and, while the schedule is current, its
    /// local actions.
    pub(super) fn detail_actions(&self) -> Result<Html> {
        let back = format!("{PATH}?kind={}", self.kind);
        let mut actions = render(BackLink {
            href: &back,
            id: "source-period-back",
            title: "Source-period schedules",
            ..BackLink::default()
        })?;
        let current = self
            .records
            .first()
            .is_some_and(|r| r["correction"].is_null());
        if current {
            actions.push(&render(IconButton {
                id: "source-period-correct",
                event: "correct_schedule",
                button_type: "button",
                ..IconButton::new("pencil-square", "Correct this schedule")
            })?);
            actions.push(&render(IconButton {
                id: "source-period-withdraw",
                event: "withdraw_schedule",
                button_type: "button",
                ..IconButton::new("x-circle", "Withdraw this schedule")
            })?);
        }
        Ok(actions)
    }

    pub(super) async fn detail(&self, scope: &Scope, company: i64) -> Result<Html> {
        let record = self.records.first().ok_or(Error::NotFound)?;
        let label = record["source_reference"].as_str().unwrap_or("");
        let mut body = match &self.message {
            Some(message) => render(Alert {
                id: "source-period-message",
                kind: StatusKind::Info,
                body: Html::text(message),
                ..Alert::default()
            })?,
            None => Html::EMPTY,
        };
        body.push(&render(Alert {
            id: "source-period-detail",
            body: Html::text(&format!(
                "Source {label}; {} to {}; {}. These are retained source facts, not postings or submissions.",
                bilibi_finance_ledger::dates::display_text(record["period_start"].as_str().unwrap_or("")),
                bilibi_finance_ledger::dates::display_text(record["period_end"].as_str().unwrap_or("")),
                record["currency_code"].as_str().unwrap_or("")
            )),
            ..Alert::default()
        })?);
        let correction = &record["correction"];
        if !correction.is_null() {
            let reason = correction["reason"].as_str().unwrap_or("");
            let on = day(&correction["recorded_at"]);
            let text = match correction["replacement_id"].as_i64() {
                Some(id) => render(Linked {
                    before: &format!("Replaced on {on} by "),
                    dom_id: "source-period-replacement",
                    href: &format!("{PATH}/{}/{id}", self.kind),
                    reference: correction["replacement_reference"].as_str().unwrap_or(""),
                    after: &format!(". It no longer counts. Reason: {reason}"),
                })?,
                None => Html::text(&format!(
                    "Withdrawn on {on}. It no longer counts. Reason: {reason}"
                )),
            };
            body.push(&render(Alert {
                id: "source-period-correction",
                kind: StatusKind::Warning,
                body: text,
                ..Alert::default()
            })?);
        }
        let replaces = &record["replaces"];
        if let Some(id) = replaces["id"].as_i64() {
            body.push(&render(Alert {
                id: "source-period-replaces",
                body: render(Linked {
                    before: "Replaces ",
                    dom_id: "source-period-replaced",
                    href: &format!("{PATH}/{}/{id}", self.kind),
                    reference: replaces["source_reference"].as_str().unwrap_or(""),
                    after: &format!(". Reason: {}", replaces["reason"].as_str().unwrap_or("")),
                })?,
                ..Alert::default()
            })?);
        }
        body.push(&render(Facts {
            id: "source-period-facts",
            items: vec![
                Fact {
                    title: "Source system",
                    value: Html::text(record["source_system"].as_str().unwrap_or("")),
                    ..Fact::default()
                },
                Fact {
                    title: "Accounting time zone",
                    value: Html::text(
                        &record["accounting_timezone"]
                            .as_str()
                            .unwrap_or("")
                            .replace('_', " "),
                    ),
                    ..Fact::default()
                },
                Fact {
                    title: "Conversion date",
                    value: Html::text(&bilibi_finance_ledger::dates::display_text(
                        record["conversion_date"].as_str().unwrap_or(""),
                    )),
                    ..Fact::default()
                },
                Fact {
                    title: "Opening entry",
                    value: match record["opening_entry_id"].as_i64() {
                        Some(id) => render(EntryLink { id })?,
                        None => Html::text("—"),
                    },
                    ..Fact::default()
                },
            ],
        })?);
        let book = self
            .read(
                scope,
                "finance.ledger.book.get",
                json!({"company_id":company}),
            )
            .await?;
        let units = book["book"]["minor_units"]
            .as_i64()
            .and_then(bilibi_finance_ledger::MinorUnits::new)
            .ok_or(Error::Internal)?;
        let statutory = self.kind == "sst" || self.kind == "payroll";
        let mut detail_columns = vec![
            BuiltinColumn::new("source", "Source ID"),
            BuiltinColumn::new("item", "Account or reference"),
            BuiltinColumn::new("side", "Side or authority"),
        ];
        if statutory {
            detail_columns.extend([
                BuiltinColumn::new("event_date", "Event date"),
                BuiltinColumn::new("status", "Status"),
            ]);
        }
        detail_columns.push(BuiltinColumn::new("amount", "Amount").right());
        let columns = ListColumns::mount("source-period-detail-rows", detail_columns)
            .map_err(|_| Error::Internal)?;
        let rows = record["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                let amount = r["amount"]
                    .as_str()
                    .and_then(|v| bilibi_finance_ledger::Money::parse(v, units).ok())
                    .map(|v| v.display(units))
                    .unwrap_or_default();
                let item = r["account_name"]
                    .as_str()
                    .map(|name| format!("{} · {}", name, r["account_code"].as_str().unwrap_or("")))
                    .unwrap_or_else(|| r["reference"].as_str().unwrap_or("").to_owned());
                let side = r["side"]
                    .as_str()
                    .or_else(|| r["authority"].as_str())
                    .unwrap_or("");
                let mut row = FlexRow::new(r["source_id"].as_str().unwrap_or(""))
                    .cell("source", Cell::text(r["source_id"].as_str().unwrap_or("")))
                    .cell("item", Cell::text(item))
                    .cell("side", Cell::text(side));
                if statutory {
                    row = row
                        .cell(
                            "event_date",
                            Cell::text(bilibi_finance_ledger::dates::display_text(
                                r["event_date"].as_str().unwrap_or(""),
                            )),
                        )
                        .cell("status", Cell::text(r["status"].as_str().unwrap_or("")));
                }
                row.cell("amount", Cell::text(amount))
            })
            .collect();
        let mut table = FlexTable::new("source-period-detail-rows", columns.flex_columns(), rows);
        table.caption = "Retained source rows";
        body.push(&render(table)?);
        body.push(&render(AttachmentSlot)?);
        render(Stack { body })
    }
}
