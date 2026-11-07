//! The Monthly movements tab's bridge to the opening balances, read through
//! `finance.reports.source_period.bridge` so a person and an agent see the
//! same months, figures and differences.
use super::*;
use crate::source_period_bridge::BRIDGE;
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_ui::{Badge, BadgeKind, Card, SectionHeading};
use rust_decimal::Decimal;

/// A link to the place that clears a bridge's refusal.
#[derive(Template)]
#[template(
    source = r#"{{ text }} <a id="{{ dom_id }}" href="{{ href }}" class="text-link hover:underline">{{ link }}</a>"#,
    ext = "html"
)]
struct GoTo<'a> {
    text: &'a str,
    dom_id: &'a str,
    href: &'a str,
    link: &'a str,
}

/// A debit-positive figure with its side, as an accountant reads it.
fn sided(value: &Value, units: MinorUnits) -> String {
    let amount: Decimal = value
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or_default();
    let shown = Money::display_decimal(amount.abs(), units);
    if amount.is_zero() {
        shown
    } else if amount.is_sign_positive() {
        format!("{shown} Dr")
    } else {
        format!("{shown} Cr")
    }
}

impl State {
    pub(super) async fn bridge(&self, scope: &Scope, company: i64) -> Result<Html> {
        let bridge = self
            .read(scope, BRIDGE, json!({"company_id": company}))
            .await?;
        let mut body = render(SectionHeading {
            id: "source-period-bridge-heading",
            title: "Bridge to the opening balances",
            description: "Check that each income and expense account's monthly movements this fiscal year add up to its opening balance.",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        let (text, link, href) = match bridge["state"].as_str().unwrap_or("") {
            "ready" => ("", "", ""),
            "no_fiscal_year" => (
                "The conversion date is in no open fiscal year.",
                "Go to fiscal years",
                "/finance/ledger/fiscal-years",
            ),
            "no_book" => (
                "The current company has no book yet.",
                "Go to the book",
                "/finance/ledger/books",
            ),
            _ => (
                "The bridge starts once the opening balances are approved.",
                "Go to opening balances",
                "/finance/ledger/opening-balances",
            ),
        };
        if !text.is_empty() {
            body.push(&html_alert(
                StatusKind::Info,
                render(GoTo {
                    text,
                    dom_id: "source-period-bridge-go",
                    href,
                    link,
                })?,
            )?);
            return card(body);
        }
        let units = bridge["minor_units"]
            .as_i64()
            .and_then(MinorUnits::new)
            .ok_or(Error::Internal)?;
        let count = |key: &str| bridge[key].as_u64().unwrap_or_default();
        let plural =
            |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let summary = if bridge["agrees"] == true {
            (
                StatusKind::Success,
                "Every month is retained once and every account agrees. Review it before signing off the conversion.".to_owned(),
            )
        } else {
            let mut parts = Vec::new();
            if count("missing_months") > 0 {
                parts.push(format!(
                    "{} not retained",
                    plural(count("missing_months"), "month", "months")
                ));
            }
            if count("repeated_months") > 0 {
                parts.push(format!(
                    "{} retained more than once",
                    plural(count("repeated_months"), "month", "months")
                ));
            }
            if count("differences") > 0 {
                parts.push(format!(
                    "{} differ from the opening balances",
                    plural(count("differences"), "account", "accounts")
                ));
            }
            let other = bridge["other_cut_off"].as_array().map_or(0, Vec::len);
            if other > 0 {
                parts.push(format!(
                    "{} cover another cut-off and are not added",
                    plural(other as u64, "schedule", "schedules")
                ));
            }
            (
                StatusKind::Warning,
                format!(
                    "{}. Explain each difference in the conversion review.",
                    parts.join("; ")
                ),
            )
        };
        body.push(&html_alert(summary.0, Html::text(&summary.1))?);
        body.push(&render(self.months_table(&bridge)?)?);
        body.push(&render(self.accounts_table(&bridge, units)?)?);
        let not_bridged = bridge["not_bridged"].as_array().map_or(0, Vec::len);
        if not_bridged > 0 {
            body.push(&html_alert(
                StatusKind::Info,
                Html::text(&format!(
                    "{} in the monthly movements {} left out: {} opening balance includes earlier years.",
                    plural(not_bridged as u64, "balance sheet account", "balance sheet accounts"),
                    if not_bridged == 1 { "is" } else { "are" },
                    if not_bridged == 1 { "its" } else { "their" }
                )),
            )?);
        }
        card(body)
    }

    fn months_table(&self, bridge: &Value) -> Result<FlexTable<'_>> {
        let columns = ListColumns::mount(
            "source-period-bridge-months",
            vec![
                BuiltinColumn::new("month", "Month"),
                BuiltinColumn::new("schedule", "Schedule"),
                BuiltinColumn::new("status", "Status"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        let mut rows = Vec::new();
        for month in bridge["months"].as_array().into_iter().flatten() {
            let (kind, status) = match month["status"].as_str().unwrap_or("") {
                "retained" => (BadgeKind::Success, "Retained"),
                "more_than_one" => (BadgeKind::Warning, "More than once"),
                _ => (BadgeKind::Neutral, "Not retained"),
            };
            let mut schedules = Html::EMPTY;
            for (n, schedule) in month["schedules"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if n > 0 {
                    schedules.push(&Html::text(", "));
                }
                let id = schedule["id"].as_i64().unwrap_or_default();
                let href = format!("{PATH}/movement/{id}");
                schedules.push(&render(bilibi_ui::lists::RecordLink {
                    workspace: self.framed,
                    kind: MODULE,
                    record_id: &id.to_string(),
                    href: &href,
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(schedule["source_reference"].as_str().unwrap_or("")),
                })?);
            }
            let period = format!(
                "{} – {}",
                month["period_start"].as_str().unwrap_or(""),
                month["period_end"].as_str().unwrap_or("")
            );
            rows.push(
                FlexRow::new(month["month"].as_str().unwrap_or(""))
                    .cell("month", Cell::text(&period))
                    .slot("schedule", schedules)
                    .slot("status", render(Badge::status(kind, status))?),
            );
        }
        let mut table = FlexTable::new("source-period-bridge-months", columns.flex_columns(), rows);
        table.caption = "Source months";
        table.framed = false;
        Ok(table)
    }

    fn accounts_table(&self, bridge: &Value, units: MinorUnits) -> Result<FlexTable<'_>> {
        let columns = ListColumns::mount(
            "source-period-bridge-accounts",
            vec![
                BuiltinColumn::new("account", "Account"),
                BuiltinColumn::new("source", "Source months").right(),
                BuiltinColumn::new("opening", "Opening balance").right(),
                BuiltinColumn::new("difference", "Difference").right(),
            ],
        )
        .map_err(|_| Error::Internal)?;
        let mut rows = Vec::new();
        for account in bridge["accounts"].as_array().into_iter().flatten() {
            rows.push(
                FlexRow::new(account["account_id"].to_string())
                    .cell(
                        "account",
                        Cell::text(format!(
                            "{} · {}",
                            account["name"].as_str().unwrap_or(""),
                            account["code"].as_str().unwrap_or("")
                        )),
                    )
                    .cell("source", Cell::text(sided(&account["source"], units)))
                    .cell("opening", Cell::text(sided(&account["opening"], units)))
                    .cell(
                        "difference",
                        Cell::text(sided(&account["difference"], units)),
                    ),
            );
        }
        if !rows.is_empty() {
            let totals = &bridge["totals"];
            // No net difference: accounts that differ both ways would add up
            // to a zero that reads as agreement.
            let mut total = FlexRow::new("total")
                .cell("account", Cell::text("Net"))
                .cell("source", Cell::text(sided(&totals["source"], units)))
                .cell("opening", Cell::text(sided(&totals["opening"], units)))
                .cell("difference", Cell::text(""));
            total.dom_id = Some("source-period-bridge-total".into());
            rows.push(total);
        }
        let mut table = FlexTable::new(
            "source-period-bridge-accounts",
            columns.flex_columns(),
            rows,
        );
        table.caption = "Income and expense accounts";
        table.framed = false;
        table.empty = Some(TableEmpty::State(
            EmptyState::missing("No income or expense movements or opening balances yet", "")
                .id("source-period-bridge-empty"),
        ));
        Ok(table)
    }
}

/// An alert inside the card's padding, not on its edge.
#[derive(Template)]
#[template(
    source = r#"<div class="px-5 pt-4">{{ alert|safe }}</div>"#,
    ext = "html"
)]
struct Inset {
    alert: Html,
}

fn html_alert(kind: StatusKind, body: Html) -> Result<Html> {
    let alert = render(Alert {
        kind,
        body,
        ..Alert::default()
    })?;
    render(Inset { alert })
}

fn card(body: Html) -> Result<Html> {
    render(Card {
        id: "source-period-bridge",
        class: "mt-4",
        inner_class: "p-0 pb-5",
        role: "region",
        labelledby: "source-period-bridge-heading",
        body,
        ..Card::default()
    })
}
