//! The opening balances page's checklist: the balances first, then each
//! owner's opening register, saying what each step waits for and linking to
//! the owner's own import once the balances journal is posted.
use super::{Opening, Progress, Stage};
use crate::web::render;
use askama::Template;
use bilibi_database::Result;
use bilibi_ui::{Badge, BadgeKind, Card, Cell, FlexRow, FlexTable, Html, SectionHeading};

/// A later step: an owner's opening register.
struct Later {
    id: &'static str,
    title: &'static str,
    owner: &'static str,
    href: &'static str,
    ready: &'static str,
}

const LATER: [Later; 3] = [
    Later {
        id: "invoices",
        title: "Unpaid invoices",
        owner: "finance/receivables",
        href: "/finance/receivables/invoices?opening=1",
        ready: "Import the invoices customers still owed on the conversion date. Skip this if there were none.",
    },
    Later {
        id: "bills",
        title: "Unpaid bills",
        owner: "finance/payables",
        href: "/finance/payables/bills?opening=1",
        ready: "Import the bills still owed to suppliers on the conversion date. Skip this if there were none.",
    },
    Later {
        id: "assets",
        title: "Assets",
        owner: "finance/fixed_assets",
        href: "/finance/fixed-assets/register?opening=1",
        ready: "Import the asset register, with each asset's cost and depreciation so far. Skip this if the company has no fixed assets.",
    },
];

impl Opening {
    pub(super) fn checklist(&mut self, stage: Stage, progress: Option<&Progress>) -> Result<Html> {
        let mut rows = Vec::new();
        let (kind, status, detail) = match (stage, progress) {
            (Stage::Posted, Some(p)) => (BadgeKind::Success, "Posted", format!("Journal {} is approved and posted.", p.journal_number)),
            (Stage::Waiting | Stage::Correcting, Some(p)) => (BadgeKind::Warning, "Waiting for approval", format!("Journal {} waits for another person to approve it. Nothing posts until then.", p.journal_number)),
            (Stage::Reversed, Some(p)) => (BadgeKind::Warning, "Reversed", format!("Journal {} was posted and then reversed. Correct the balances with a manual journal.", p.journal_number)),
            (Stage::Open, Some(p)) if p.status == "rejected" => (BadgeKind::Danger, "Rejected", format!("Journal {} was rejected. Its balances are filled in below: correct them and submit again.", p.journal_number)),
            (Stage::Open, Some(p)) => (BadgeKind::Neutral, "Withdrawn", format!("Journal {} was withdrawn. Its balances are filled in below: correct them and submit again.", p.journal_number)),
            _ => (BadgeKind::Neutral, "Not started", "Fill in each account's opening balance below, then submit them for approval.".to_owned()),
        };
        let journal_link = match (stage, progress) {
            (Stage::Open, _) | (_, None) => None,
            (_, Some(p)) => Some(format!("/finance/ledger/journals/{}", p.journal_id)),
        };
        rows.push(
            FlexRow::new("balances")
                .slot(
                    "step",
                    render(StepName {
                        id: "balances",
                        name: "1. Account balances",
                        href: journal_link.as_deref().unwrap_or("#opening-balances"),
                    })?,
                )
                .slot("status", render(Badge::status(kind, status))?)
                .cell("detail", Cell::text(&detail))
                .slot("detail", Html::text(&detail)),
        );
        let posted = stage == Stage::Posted;
        for (n, later) in LATER.iter().enumerate() {
            let imported = progress
                .into_iter()
                .flat_map(|p| &p.imported)
                .find(|i| i.owner_module == later.owner);
            let (kind, status, detail, linked) = match (posted, imported) {
                (_, Some(i)) => (
                    BadgeKind::Success,
                    "Imported",
                    format!(
                        "Imported against {} {}.",
                        i.accounts,
                        if i.accounts == 1 {
                            "account"
                        } else {
                            "accounts"
                        }
                    ),
                    true,
                ),
                (true, None) => (BadgeKind::Neutral, "Ready", later.ready.to_owned(), true),
                (false, None) => (
                    BadgeKind::Neutral,
                    "Waiting",
                    "Opens once the balances journal is approved.".to_owned(),
                    false,
                ),
            };
            let name = format!("{}. {}", n + 2, later.title);
            let step = if linked {
                render(StepName {
                    id: later.id,
                    name: &name,
                    href: later.href,
                })?
            } else {
                Html::text(&name)
            };
            rows.push(
                FlexRow::new(later.id)
                    .slot("step", step)
                    .slot("status", render(Badge::status(kind, status))?)
                    .cell("detail", Cell::text(&detail))
                    .slot("detail", Html::text(&detail)),
            );
        }
        let keys: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
        self.steps.load(&keys, Clone::clone);
        let mut table = FlexTable::new("opening-steps", self.steps.flex_columns(), rows);
        table.caption = "Steps";
        table.framed = false;
        table.zoom = self.steps.zoom;
        let mut card = render(SectionHeading {
            id: "opening-steps-heading",
            title: "Steps",
            description:
                "Balances come first. The other steps open once that journal is approved, because each invoice, bill or asset must add up to its account's opening balance.",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        card.push(&render(table)?);
        render(Card {
            id: "opening-steps-card",
            inner_class: "p-0",
            role: "region",
            labelledby: "opening-steps-heading",
            body: card,
            ..Card::default()
        })
    }
}

/// A step's name, linked to where it is done.
#[derive(Template)]
#[template(
    source = r#"<a id="opening-step-{{ id }}" href="{{ href }}" class="text-link hover:underline">{{ name }}</a>"#,
    ext = "html"
)]
struct StepName<'a> {
    id: &'a str,
    name: &'a str,
    href: &'a str,
}
