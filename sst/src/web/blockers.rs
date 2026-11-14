//! What keeps an open return from finalising, as its page lists it: a period
//! still running (with the day it can be finalised from), a tax code in use
//! with no SST-02 item (with the choice in place, for someone who manages the
//! setup), and a rate that does not fit its item.
use super::{format as f, mapping::ItemChoice, render};
use crate::{ReturnReport, ReturnStatus};
use askama::Template;
use bilibi_database::Result;
use bilibi_ui::{CommitStates, Html};

#[derive(Template)]
#[template(path = "return_blockers.html")]
struct Blockers {
    finalise_from: Option<String>,
    unmapped: Vec<Unmapped>,
    rates: Vec<Rate>,
}
struct Unmapped {
    source: &'static str,
    tax_code_id: i64,
    tax_code: String,
    /// The item choice, for someone who may make it.
    choice: Option<Html>,
}
struct Rate {
    line: i32,
    tax_code: String,
    rate: String,
    box_name: String,
}

/// The "Before it can be finalised" list of an open return, or `None` when
/// nothing blocks it.
pub(super) fn render_for(
    report: &ReturnReport,
    can_manage: bool,
    statuses: &CommitStates,
) -> Result<Option<Html>> {
    let Some(figures) = report.figures.as_ref() else {
        return Ok(None);
    };
    if report.tax_return.status != ReturnStatus::Open {
        return Ok(None);
    }
    let mut unmapped = Vec::new();
    for u in &figures.unmapped {
        unmapped.push(Unmapped {
            source: u.source.as_str(),
            tax_code_id: u.tax_code_id,
            tax_code: u.tax_code.clone(),
            choice: can_manage
                .then(|| {
                    ItemChoice {
                        source: u.source,
                        tax_code_id: u.tax_code_id,
                        code: &u.tax_code,
                        current: None,
                    }
                    .render(true, statuses)
                })
                .transpose()?,
        });
    }
    let blockers = Blockers {
        finalise_from: (!report.period_ended)
            .then(|| f::finalise_from(report.tax_return.period_end)),
        unmapped,
        rates: figures
            .rate_mismatches
            .iter()
            .map(|r| Rate {
                line: r.line_number,
                tax_code: r.tax_code.clone(),
                rate: f::rate(r.rate),
                box_name: f::box_name(report.tax_return.tax_type, r.box_key.as_deref()),
            })
            .collect(),
    };
    if blockers.finalise_from.is_none() && blockers.unmapped.is_empty() && blockers.rates.is_empty()
    {
        return Ok(None);
    }
    render(blockers).map(Some)
}
