//! The register's own parts: its rows with their next steps and the ledger
//! check of the whole register.
use super::*;

#[derive(Template)]
#[template(
    source = r#"<a id="assets-report-reproduce" href="{{ href }}" data-patch class="text-link hover:underline">Keep these figures</a>"#,
    ext = "html"
)]
struct Reproduce {
    href: String,
}

impl Index {
    /// The register's rows as of the period's end, each with its next step
    /// as an icon button, and the dated report they came from.
    pub(super) async fn register_rows(
        &self,
        s: &Scope,
        c: i64,
        units: Option<bilibi_finance_ledger::MinorUnits>,
        rows: &mut Vec<(String, FlexRow)>,
    ) -> crate::Result<RegisterReport> {
        let as_of = self.period.as_ref().expect("period initialized").period.to;
        let report = self
            .pages
            .read::<RegisterReport>(
                s,
                "finance.fixed_assets.register.get",
                {
                    let mut value =
                        serde_json::to_value(self.pins()).map_err(|_| Error::Internal)?;
                    value["company_id"] = c.into();
                    value["as_of"] = serde_json::json!(as_of.to_string());
                    for key in ["source", "status"] {
                        if let Some(filter) = self.list.filters.get(key).filter(|v| !v.is_empty()) {
                            value[key] = serde_json::json!(filter);
                        }
                    }
                    value
                },
                "report",
            )
            .await?;
        let (dispose, reverse) = (
            self.pages.allowed(s, DISPOSE).await?,
            self.pages.allowed(s, REVERSE).await?,
        );
        for r in &report.rows {
            let a = &r.asset;
            let link = render(RecordLink {
                workspace: self.framed,
                kind: MODULE,
                record_id: &a.id.to_string(),
                href: &format!("/finance/fixed-assets/register/{}", a.id),
                body: Html::text(&a.description),
                class: "font-medium text-ink-strong hover:underline",
            })?;
            let mut row = FlexRow::new(a.id.to_string())
                .cell("number", Cell::text(a.asset_number.to_string()))
                .slot("description", link)
                .cell("category", Cell::text(r.revision.name.clone()))
                .cell("cost", Cell::text(a.cost.display(units)))
                .cell("accumulated", Cell::text(r.accumulated.display(units)))
                .cell("carrying", Cell::text(r.carrying_value.display(units)))
                .cell(
                    "source",
                    Cell::text(if a.opening_date.is_some() {
                        "Opening"
                    } else {
                        "Ordinary"
                    }),
                )
                .cell(
                    "status",
                    Cell::text(if r.voided {
                        "Voided"
                    } else if r.disposed {
                        "Disposed"
                    } else {
                        "In service"
                    }),
                );
            // Each row's next step, on the asset's own page.
            let page = format!("/finance/fixed-assets/register/{}", a.id);
            // These act on the asset itself, so they are plain icon buttons,
            // not the "Non-local actions" menu, which holds only other modules' actions.
            if !r.voided && !r.disposed && dispose {
                row.actions = row_icon(
                    &format!("assets-asset-{}-dispose", a.id),
                    "archive-box-x-mark",
                    &format!("Sell or write off {}", a.description),
                    &format!("{page}?act=dispose"),
                )?;
            }
            if r.disposed && reverse {
                row.actions = row_icon(
                    &format!("assets-asset-{}-reverse", a.id),
                    "arrow-uturn-left",
                    &format!("Reverse the disposal of {}", a.description),
                    &format!("{page}?act=reverse"),
                )?;
            }
            rows.push((
                format!("{} {} {}", a.asset_number, a.description, r.revision.name),
                row,
            ));
        }
        Ok(report)
    }

    /// The whole register's tie-out to the ledger, one row per account.
    pub(super) fn ledger_check(
        &self,
        r: &RegisterReport,
        units: Option<bilibi_finance_ledger::MinorUnits>,
        accounts: &categories::Accounts,
    ) -> Result<Html> {
        // The link keeps the filters, the period and the four report pins.
        let mut query = self.list.to_params();
        query.extend(self.period_params());
        for (key, value) in PINS.into_iter().zip([
            r.through_entry_number,
            r.through_asset_number,
            r.through_event_number,
            r.through_category_revision_id,
        ]) {
            query.insert(key.into(), value.to_string());
        }
        let mut html = render(SectionHeading {
            title: &format!("Ledger check as of {}", r.as_of),
            id: "assets-reconciliation",
            class: "mt-4",
            description:
                "Each account's ledger balance beside what the register says it should be.",
            actions: render(Reproduce {
                href: params::address("/finance/fixed-assets/register", &query),
            })?,
            ..Default::default()
        })?;
        let mut rows = Vec::new();
        for line in &r.reconciliation {
            rows.push(
                FlexRow::new(line.account_id.to_string())
                    .slot(
                        "account",
                        categories::account_name(accounts, line.account_id)?,
                    )
                    .cell("ledger", Cell::text(line.ledger_balance.display(units)))
                    .cell("register", Cell::text(line.register_balance.display(units)))
                    .cell("difference", Cell::text(line.difference.display(units)))
                    .cell(
                        "other",
                        Cell::text(if line.other_postings.is_zero() {
                            "—".into()
                        } else {
                            line.other_postings.display(units)
                        }),
                    ),
            );
        }
        html.push(&table(
            "assets-ledger-check",
            &self.check,
            rows,
            "Ledger check",
            "check_grid",
            "No category accounts yet.",
        )?);
        Ok(html)
    }
}
