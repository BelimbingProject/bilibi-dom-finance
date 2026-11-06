//! Depreciation runs: "Run depreciation" opens a dialog for a month, and the
//! newest run that posted a charge offers Reverse as an icon button on its row.
use super::{input::Draft, *};
use crate::Run;
use bilibi_ui::InputKind;

#[derive(Default)]
pub(super) struct State {
    /// The "Run depreciation" dialog's draft while it is open.
    pub new: Option<Draft>,
    /// The run being reversed and its dialog's draft.
    pub reverse: Option<(i64, Draft)>,
}

fn base_alert(d: &Draft) -> String {
    d.errors
        .iter()
        .filter(|e| e.field == "base")
        .map(|e| e.message.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The run Reverse may act on: the newest one standing with a charge.
/// Reversing an older one is refused until every later run is reversed.
pub(super) fn reversible(runs: &[Run]) -> Option<i64> {
    runs.iter()
        .filter(|r| !r.reversed)
        .max_by_key(|r| r.period)
        .filter(|r| r.entry_id.is_some())
        .map(|r| r.id)
}

impl State {
    pub(super) fn dialogs(&self, runs: &[Run]) -> Result<Html> {
        let mut html = Html::EMPTY;
        if let Some(d) = &self.new {
            // A month field posts "2026-10"; the run takes its first day.
            let mut month = d.field("period", "Month", InputKind::Month, true, "asset");
            month.value = d.value("period").get(..7).unwrap_or("").to_owned();
            html.push(
                &Dialog {
                    id: "run-modal",
                    title: "Run depreciation",
                    description: "Posts each asset's depreciation through the end of the month, including any earlier months not yet posted.",
                    change: "",
                    submit: "run_depreciation",
                    cancel: "close_run",
                    verb: "Run depreciation",
                    working: "Posting…",
                    alert: &base_alert(d),
                    fields: vec![month],
                    extra: Html::EMPTY,
                    can_submit: true,
                }
                .render()?,
            );
        }
        if let Some((id, d)) = &self.reverse {
            let title = runs
                .iter()
                .find(|r| r.id == *id)
                .map(|r| format!("Reverse depreciation for {}", month(r.period)))
                .unwrap_or_else(|| "Reverse depreciation".into());
            html.push(
                &Dialog {
                    id: "run-reverse-modal",
                    title: &title,
                    description: "Posts a reversal of this run's journal entry. The month stays used; the next run catches its depreciation up.",
                    change: "",
                    submit: "reverse_run",
                    cancel: "close_reverse",
                    verb: "Reverse run",
                    working: "Reversing…",
                    alert: &base_alert(d),
                    fields: vec![
                        d.field(
                            "effective_date",
                            "Reversal date",
                            InputKind::Date,
                            true,
                            "asset",
                        ),
                        d.field("reason", "Reason", InputKind::Text, true, "asset"),
                    ],
                    extra: Html::EMPTY,
                    can_submit: true,
                }
                .render()?,
            );
        }
        Ok(html)
    }

    pub(super) async fn run(
        &self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
    ) -> crate::Result<Run> {
        let d = self.new.as_ref().ok_or(Error::Invalid)?;
        let raw = d.value("period");
        let first = if raw.len() == 7 {
            format!("{raw}-01")
        } else {
            raw.to_owned()
        };
        let period: jiff::civil::Date = first.parse().map_err(|_| {
            crate::invalid(
                "period",
                if raw.is_empty() {
                    "can't be blank"
                } else {
                    "is invalid"
                },
            )
        })?;
        let result = pages
            .submit(
                s,
                credential,
                "run_depreciation",
                "finance.fixed_assets.run.post",
                serde_json::json!({"period":period.to_string()}),
            )
            .await?;
        Ok(serde_json::from_value(result).map_err(|_| Error::Internal)?)
    }

    pub(super) async fn reverse(
        &self,
        pages: &Pages,
        s: &Scope,
        credential: &str,
    ) -> crate::Result<()> {
        let (id, d) = self.reverse.as_ref().ok_or(Error::Invalid)?;
        let mut input = d.event_values();
        input["run_id"] = (*id).into();
        pages
            .submit(
                s,
                credential,
                "reverse_run",
                "finance.fixed_assets.run.reverse",
                input,
            )
            .await?;
        Ok(())
    }
}

/// One run's row; the reversible run offers Reverse as an icon button.
pub(super) fn row(
    r: &Run,
    units: Option<bilibi_finance_ledger::MinorUnits>,
    reversible: bool,
) -> Result<FlexRow> {
    let skipped = if r.exceptions.is_empty() {
        "None".into()
    } else {
        r.exceptions
            .iter()
            .map(|e| format!("#{} {}", e.asset_number, e.description))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let status = if r.reversed {
        "Reversed"
    } else if r.entry_id.is_none() {
        "Nothing to post"
    } else {
        "Posted"
    };
    let mut row = FlexRow::new(r.id.to_string())
        .cell("period", Cell::text(month(r.period)))
        .cell("total", Cell::text(r.total.display(units)))
        .slot("entry", entry_link(r.entry_id, r.entry_number)?)
        .cell("status", Cell::text(status))
        .cell("exceptions", Cell::text(skipped));
    if reversible {
        row.actions = row_icon(
            &format!("assets-run-{}-reverse", r.id),
            "arrow-uturn-left",
            &format!("Reverse depreciation for {}", month(r.period)),
            &format!("/finance/fixed-assets/runs?reverse={}", r.id),
        )?;
    }
    Ok(row)
}
