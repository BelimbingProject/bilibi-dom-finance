//! Where the money of a document is, as the Invoices and Bills lists and their
//! record pages say it: Awaiting payment, Part paid, Paid, and Overdue with
//! the days late. The twins (Receivables, Payables) derive it here from what
//! they already store (total, what is still owed, the due date) against one
//! date, so a list row, a record page and an agent's read agree. Also the
//! strip of figures above such a list and the totals under a document's lines.
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_ui::{status_badge, BadgeKind, Html, StatItem, StatStrip};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// How much of an issued invoice or a posted bill has been paid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    AwaitingPayment,
    PartPaid,
    Paid,
}

/// A document's payment status and, while money is still owed after its due
/// date, how many days late it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owed {
    pub payment_status: PaymentStatus,
    pub days_overdue: Option<i64>,
}

impl Owed {
    /// From what the document stores, read against `as_of` (today in the
    /// company's zone). A document due on `as_of` is not overdue yet.
    pub fn of(total: Decimal, outstanding: Decimal, due: Option<Date>, as_of: Date) -> Self {
        let payment_status = if outstanding <= Decimal::ZERO {
            PaymentStatus::Paid
        } else if outstanding < total {
            PaymentStatus::PartPaid
        } else {
            PaymentStatus::AwaitingPayment
        };
        let days_overdue = due
            .filter(|due| payment_status != PaymentStatus::Paid && *due < as_of)
            .map(|due| i64::from((as_of - due).get_days()));
        Self {
            payment_status,
            days_overdue,
        }
    }

    /// The status as a contract row carries it, if it carries one.
    pub fn read(payment_status: Option<PaymentStatus>, days_overdue: Option<i64>) -> Option<Self> {
        payment_status.map(|payment_status| Self {
            payment_status,
            days_overdue,
        })
    }

    /// "Overdue 129 days" (and "Part paid" beside it), "Awaiting payment",
    /// "Part paid" or "Paid", as badges.
    pub fn badges(&self) -> Html {
        let mut html = Html::EMPTY;
        if let Some(days) = self.days_overdue {
            html.push(&status_badge(
                BadgeKind::Danger,
                &format!("Overdue {}", days_text(days)),
            ));
        }
        let rest = match (self.payment_status, self.days_overdue) {
            (PaymentStatus::Paid, _) => Some((BadgeKind::Success, "Paid")),
            (PaymentStatus::PartPaid, _) => Some((BadgeKind::Warning, "Part paid")),
            (PaymentStatus::AwaitingPayment, None) => {
                Some((BadgeKind::Neutral, "Awaiting payment"))
            }
            (PaymentStatus::AwaitingPayment, Some(_)) => None,
        };
        if let Some((kind, label)) = rest {
            if self.days_overdue.is_some() {
                html.push(&Html::text(" "));
            }
            html.push(&status_badge(kind, label));
        }
        html
    }

    /// "Overdue by 129 days", for beside the amount due on a record page.
    pub fn overdue_by(&self) -> Option<String> {
        self.days_overdue
            .map(|days| format!("Overdue by {}", days_text(days)))
    }
}

fn days_text(days: i64) -> String {
    if days == 1 {
        "1 day".into()
    } else {
        format!("{days} days")
    }
}

/// How many documents, and what they add up to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub count: i64,
    pub amount: Decimal,
}

/// A company's invoices or bills in three figures, read against `as_of`:
/// what is not in the books yet, what is still owed, and how much of that is
/// past its due date. `overdue` is part of `awaiting_payment`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwedSummary {
    pub as_of: String,
    pub not_posted: Tally,
    pub awaiting_payment: Tally,
    pub overdue: Tally,
    pub oldest_days_overdue: Option<i64>,
}

/// One figure of the strip above a list: its name, how many documents, what
/// they add up to and, when the list can show just those, where.
pub struct OwedFigure<'a> {
    pub title: &'a str,
    pub detail: String,
    pub amount: String,
    pub href: String,
}

#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" class="mb-4 grid gap-3 sm:grid-cols-3">{% for strip in strips %}{{ strip|safe }}{% endfor %}</div>"#,
    ext = "html"
)]
struct Strips<'a> {
    id: &'a str,
    strips: Vec<Html>,
}

impl OwedSummary {
    /// "5 invoices", "1 bill".
    pub fn counted(count: i64, one: &str, many: &str) -> String {
        format!("{count} {}", if count == 1 { one } else { many })
    }

    /// The overdue figure's detail: "4 invoices · oldest 129 days".
    pub fn overdue_detail(&self, one: &str, many: &str) -> String {
        let counted = Self::counted(self.overdue.count, one, many);
        match self.oldest_days_overdue {
            Some(days) => format!("{counted} · oldest {}", days_text(days)),
            None => counted,
        }
    }

    /// The strip above a list: up to three figures side by side, each a link
    /// to the list narrowed to it when it has an `href`.
    pub fn strips(id: &str, figures: Vec<OwedFigure<'_>>) -> Result<Html> {
        let mut strips = Vec::new();
        for (i, figure) in figures.iter().enumerate() {
            strips.push(
                Html::render(&StatStrip {
                    id: &format!("{id}-{i}"),
                    title: figure.title,
                    href: &figure.href,
                    items: vec![StatItem::number(&figure.detail, &figure.amount)],
                })
                .map_err(|_| Error::Internal)?,
            );
        }
        Html::render(&Strips { id, strips }).map_err(|_| Error::Internal)
    }
}

/// The totals under a document's lines, right-aligned under the Amount
/// column; the last row (the total) is the strong one.
#[derive(Template)]
#[template(
    source = r#"<dl id="{{ id }}" class="ml-auto w-full max-w-xs space-y-1 py-4 pl-5 pr-2 text-sm">{% for row in rows %}<div class="flex items-baseline justify-between gap-6{% if loop.last %} border-t border-line pt-2 font-semibold text-ink-strong{% endif %}"><dt{% if !loop.last %} class="text-ink-muted"{% endif %}>{{ row.0 }}</dt><dd class="tabular-nums">{{ row.1 }}</dd></div>{% endfor %}</dl>"#,
    ext = "html"
)]
pub struct DocumentTotals<'a> {
    pub id: &'a str,
    pub rows: Vec<(&'a str, String)>,
}

impl DocumentTotals<'_> {
    pub fn render(id: &str, rows: Vec<(&str, String)>) -> Result<Html> {
        Html::render(&DocumentTotals { id, rows }).map_err(|_| Error::Internal)
    }
}
