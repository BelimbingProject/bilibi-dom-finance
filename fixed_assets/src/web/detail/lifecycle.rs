//! The asset's lifecycle: what its state allows next, the dialog for each
//! step and the facts the page leads with.
use super::*;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Act {
    Dispose,
    Reverse,
    Void,
}
impl Act {
    pub(super) fn parse(s: &str) -> Option<Self> {
        match s {
            "dispose" => Some(Self::Dispose),
            "reverse" => Some(Self::Reverse),
            "void" => Some(Self::Void),
            _ => None,
        }
    }
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Dispose => "dispose",
            Self::Reverse => "reverse",
            Self::Void => "void",
        }
    }
    pub(super) fn cap(self) -> &'static str {
        match self {
            Self::Dispose => DISPOSE,
            Self::Reverse => REVERSE,
            Self::Void => VOID,
        }
    }
    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Dispose => "Sell or write off",
            Self::Reverse => "Reverse disposal",
            Self::Void => "Void acquisition",
        }
    }
}

/// The asset's lifecycle now: what the header may offer.
pub(super) struct Standing {
    pub(super) disposed: bool,
    pub(super) voided: bool,
    /// The acquisition entry was reversed in the ledger; `None` when this
    /// person cannot read the ledger entry.
    pub(super) source_reversed: Option<bool>,
}
impl Standing {
    pub(super) fn of(d: &AssetDetail, source_reversed: Option<bool>) -> Self {
        let mut disposed = false;
        for e in &d.events {
            match e.action {
                EventAction::Disposed => disposed = true,
                EventAction::DisposalReversed => disposed = false,
                _ => {}
            }
        }
        Self {
            disposed,
            voided: d.voided,
            source_reversed,
        }
    }
    /// The one next step, if any.
    pub(super) fn next(&self) -> Option<Act> {
        if self.voided {
            None
        } else if self.disposed {
            Some(Act::Reverse)
        } else if self.source_reversed == Some(true) {
            Some(Act::Void)
        } else {
            Some(Act::Dispose)
        }
    }
    pub(super) fn status(&self) -> &'static str {
        if self.voided {
            "Voided"
        } else if self.disposed {
            "Disposed"
        } else if self.source_reversed == Some(true) {
            "Purchase reversed"
        } else {
            "In service"
        }
    }
}

pub(super) fn event_name(action: EventAction) -> &'static str {
    match action {
        EventAction::Disposed => "Sold or written off",
        EventAction::DisposalReversed => "Disposal reversed",
        EventAction::AcquisitionVoided => "Acquisition voided",
        EventAction::RunReversed => "Depreciation reversed",
    }
}

fn fact<'a>(title: &'a str, value: Html) -> Fact<'a> {
    Fact {
        title,
        value,
        ..Default::default()
    }
}

#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="text-link hover:underline">{{ label }}</a>"#,
    ext = "html"
)]
struct Link<'a> {
    href: &'a str,
    label: &'a str,
}

impl Detail {
    /// Whether the asset's purchase entry was reversed in the ledger.
    pub(super) async fn source_reversed(&self, s: &Scope, d: &AssetDetail) -> Option<bool> {
        let c = company(s)?;
        let entry = self
            .pages
            .contracts
            .read(
                s,
                "finance.ledger.entry.get",
                &serde_json::json!({"company_id":c,"entry_id":d.asset.entry_id}),
            )
            .await
            .ok()?;
        Some(!entry["result"]["reversed_by_entry_id"].is_null())
    }

    pub(super) fn dialog(&self, accounts: &categories::Accounts, title: &str) -> Result<Html> {
        let Some((act, d)) = &self.dialog else {
            return Ok(Html::EMPTY);
        };
        let (date_label, description) = match act {
            Act::Dispose => (
                "Disposal date",
                "Removes the asset's cost and depreciation, records the proceeds and posts the gain or loss. Depreciation must be posted through the last completed month.",
            ),
            Act::Reverse => (
                "Reversal date",
                "Reverses the disposal's journal entry and puts the asset back in service.",
            ),
            Act::Void => (
                "Void date",
                "For an asset whose purchase was reversed in the ledger. Depreciation already posted for it is reversed; other assets keep theirs.",
            ),
        };
        let mut fields =
            vec![d.field("effective_date", date_label, InputKind::Date, true, "asset")];
        if *act == Act::Dispose {
            fields.push(
                d.field("proceeds", "Proceeds", InputKind::Text, true, "asset")
                    .hint("Enter 0 for a write-off."),
            );
            fields.push(
                d.field(
                    "settlement_account_id",
                    "Proceeds received into",
                    InputKind::Select,
                    false,
                    "asset",
                )
                .options(categories::account_choices(accounts, "asset"))
                .placeholder("Choose an account")
                .hint("Needed when there are proceeds."),
            );
        }
        fields.push(d.field("reason", "Reason", InputKind::Text, true, "asset"));
        let alert = d
            .errors
            .iter()
            .filter(|e| e.field == "base")
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join(" ");
        // The prerequisite a disposal names is one click away: the runs
        // page with its dialog open.
        let extra = if alert.contains(&FixedAssetsError::DepreciationRequired.to_string()) {
            render(bilibi_ui::Button {
                id: "asset-dispose-run-depreciation",
                href: "/finance/fixed-assets/runs?new=1",
                body: Html::text("Run depreciation"),
                ..bilibi_ui::Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let heading = format!("{}: {title}", act.verb());
        Dialog {
            id: &format!("asset-{}-modal", act.key()),
            title: &heading,
            description,
            change: "",
            submit: act.key(),
            cancel: "close_dialog",
            verb: match act {
                Act::Dispose => "Post disposal",
                Act::Reverse => "Reverse disposal",
                Act::Void => "Review void",
            },
            working: "Posting…",
            alert: &alert,
            fields,
            extra,
            can_submit: true,
        }
        .render()
    }

    pub(super) async fn facts(
        &self,
        s: &Scope,
        d: &AssetDetail,
        standing: &Standing,
        units: Option<bilibi_finance_ledger::MinorUnits>,
        people: &People,
    ) -> Result<Html> {
        let display = self.pages.display(s).await?;
        let a = &d.asset;
        let bought = match (a.bill_id, a.entry_line_number) {
            (Some(bill), _) => {
                // The supplier's invoice number, when this person may read the bill.
                let number = match company(s) {
                    Some(c) => self
                        .pages
                        .contracts
                        .read(
                            s,
                            "finance.payables.document.get",
                            &serde_json::json!({"company_id":c,"document_id":bill}),
                        )
                        .await
                        .ok()
                        .and_then(|v| {
                            v["result"]["party_document_number"]
                                .as_str()
                                .map(str::to_owned)
                        }),
                    None => None,
                };
                let line = a.bill_line_number.unwrap_or(1);
                render(Link {
                    href: &format!("/finance/payables/bills/{bill}"),
                    label: &match number {
                        Some(n) => format!("Bill {n}, line {line}"),
                        None => format!("Bill, line {line}"),
                    },
                })?
            }
            (None, Some(line)) => {
                let mut html = entry_link(Some(a.entry_id), Some(a.entry_number))?;
                html.push(&Html::text(&format!(", line {line}")));
                html
            }
            (None, None) => {
                let mut html = Html::text("Opening register, ");
                html.push(&entry_link(Some(a.entry_id), Some(a.entry_number))?);
                html
            }
        };
        let mut items = vec![
            fact("Status", Html::text(standing.status())),
            fact("Category", Html::text(&d.revision.name)),
            fact("Cost", Html::text(&a.cost.display(units))),
            fact(
                "Residual value",
                Html::text(&a.residual_value.display(units)),
            ),
            fact("Bought", Html::text(&a.acquisition_date.to_string())),
            fact(
                "In service from",
                Html::text(&a.in_service_date.to_string()),
            ),
            fact(
                "Method",
                Html::text(super::input::method(a.assumptions.method)),
            ),
            fact(
                "Useful life",
                Html::text(&format!("{} months", a.assumptions.life_months)),
            ),
        ];
        if let Some(rate) = a.assumptions.annual_rate {
            items.push(fact(
                "Annual rate",
                Html::text(&format!("{}%", rate.normalize())),
            ));
        }
        items.push(fact("Bought on", bought));
        items.push(fact(
            "Registered by",
            Html::text(&people.name(a.recorded_by_user_id, a.recorded_by_impersonator_id)),
        ));
        items.push(fact(
            "Registered",
            render(Recorded {
                cell: time_cell(&display, &a.recorded_at)?,
            })?,
        ));
        render(Card {
            id: "asset-facts",
            body: render(Facts {
                id: "asset-facts-list",
                items,
            })?,
            ..Default::default()
        })
    }
}
