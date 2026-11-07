//! The Payroll and claims page: runs and claims from the People copy with
//! their standing in the books, the postings made with their journals linked,
//! and the reversal dialog.
use super::*;

impl State {
    pub(super) async fn postings_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let mut parts = Vec::new();
        if self.pages.payroll.people_available() {
            parts.extend(self.source_parts(scope).await?);
        } else {
            parts.push(render(EmptyState::missing("Payroll is not in use", "Payroll runs and claims appear here once payroll is in use. The postings already made are listed below.").id("payroll-source-unavailable"))?);
        }
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let can_post = self.allowed(scope, POST_POSTINGS).await?;
        let postings = payroll.list_postings(scope, company).await;
        let body = match postings {
            Err(_) => render(
                EmptyState::forbidden("view payroll postings").id("payroll-postings-unavailable"),
            )?,
            Ok(postings) => {
                let places = payroll
                    .ledger
                    .get_book(scope, company)
                    .await
                    .map(|b| b.minor_units.get())
                    .unwrap_or(2);
                let mut rows = Vec::new();
                for posting in postings {
                    let source = if posting.source_kind == "payroll_run" {
                        "Payroll run"
                    } else {
                        "Claim"
                    };
                    let mut row = row(
                        &format!("payroll-posting-{}", posting.id),
                        &[
                            ("source", format!("{source} {}", posting.source_reference)),
                            ("date", posting.entry_date.clone()),
                            (
                                "total",
                                format!(
                                    "{} {}",
                                    posting.total.parse::<Decimal>().map_or_else(
                                        |_| "—".into(),
                                        |amount| {
                                            Money::display_decimal(
                                                amount,
                                                MinorUnits::new(i64::from(places)),
                                            )
                                        },
                                    ),
                                    posting.currency_code
                                ),
                            ),
                            (
                                "reason",
                                posting
                                    .reversal
                                    .as_ref()
                                    .map_or("—".into(), |r| r.reason.clone()),
                            ),
                        ],
                    );
                    row = row.slot(
                        "entry",
                        EntryLink::html(Some(posting.entry_id), Some(posting.entry_number))?,
                    );
                    row = row.slot(
                        "standing",
                        match &posting.reversal {
                            None => render(Badge::status(BadgeKind::Success, "Posted"))?,
                            Some(r) => {
                                let mut html =
                                    render(Badge::status(BadgeKind::Warning, "Reversed"))?;
                                html.push(&Html::text(" by "));
                                html.push(&EntryLink::html(
                                    Some(r.entry_id),
                                    Some(r.entry_number),
                                )?);
                                html
                            }
                        },
                    );
                    if can_post && posting.reversal.is_none() {
                        row.actions = icon_action(
                            &format!("payroll-reverse-posting-{}", posting.id),
                            "reverse",
                            posting.id,
                            "arrow-uturn-left",
                            &format!("Reverse journal {}", posting.entry_number),
                        )?;
                    }
                    rows.push(row);
                }
                self.table(
                    2,
                    rows,
                    "Postings made",
                    "Nothing has been posted yet.",
                    true,
                )?
            }
        };
        parts.push(section(
            "payroll-postings-list",
            "payroll-postings-heading",
            "Postings made",
            "",
            None,
            body,
        )?);
        if self.reversing.is_some() && can_post {
            let reason = self
                .field(
                    "reversal",
                    "reason",
                    "payroll-reversal-reason",
                    "Reason",
                    &self.reversal,
                )
                .required()
                .attr("maxlength", "500");
            let date = self
                .field(
                    "reversal",
                    "entry_date",
                    "payroll-reversal-date",
                    "Entry date",
                    &self.reversal,
                )
                .hint("Leave it empty for the company's today.")
                .kind(InputKind::Date);
            // An irreversible write, confirmed as the shared confirmation
            // is (consequence first, Cancel beside the danger verb), with the
            // reason as its body. The verb submits the form, so what was just
            // typed always travels with it.
            parts.push(render(ReversalDialog {
                alert: self.dialog_alert("payroll-reversal-alert")?,
                fields: vec![render(reason)?, render(date)?],
            })?);
        }
        Ok(parts)
    }

    async fn source_parts(&mut self, scope: &Scope) -> Result<Vec<Html>> {
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        let can_post = self.allowed(scope, POST_POSTINGS).await?;
        let units = payroll
            .ledger
            .get_book(scope, company)
            .await
            .map(|b| Some(b.minor_units))
            .unwrap_or(MinorUnits::new(2));
        let mut parts = Vec::new();
        for (index, claims) in [false, true].into_iter().enumerate() {
            let (kind, title, empty) = if claims {
                ("claim", "Approved claims", "No approved claims.")
            } else {
                ("run", "Payroll runs", "No payroll runs yet.")
            };
            let result = if claims {
                payroll.list_claims(scope, company).await
            } else {
                payroll.list_payroll_runs(scope, company).await
            };
            let mut content = Html::EMPTY;
            let mut actions = Html::EMPTY;
            match result {
                Err(_) => content.push(&render(
                    EmptyState::forbidden(if claims {
                        "read the company's approved claims"
                    } else {
                        "read the company's payroll runs"
                    })
                    .id(format!("payroll-{kind}s-unavailable")),
                )?),
                Ok(sources) => {
                    if claims && can_post && sources.iter().any(|c| c["posting"].is_null()) {
                        actions = render(Button {
                            id: "payroll-post-all-claims",
                            event: "post_all_claims",
                            name: "id",
                            value: "0",
                            button_type: "button",
                            working: "Posting…",
                            body: Html::text("Post every unposted claim"),
                            ..Button::default()
                        })?;
                    }
                    let mut rows = Vec::new();
                    for source in sources {
                        let id = source["id"].as_i64().ok_or(Error::Internal)?;
                        let posting = &source["posting"];
                        let reversed = !posting["reversal"].is_null();
                        let posted = !posting.is_null();
                        let failure = source["last_failure"]["reason"].as_str();
                        let reference = source["reference"].as_str().unwrap_or("—").to_owned();
                        let mut cells = vec![
                            ("reference", reference.clone()),
                            (
                                "date",
                                source[if claims { "approved_on" } else { "pay_date" }]
                                    .as_str()
                                    .unwrap_or(if claims {
                                        "unknown"
                                    } else {
                                        "missing or invalid"
                                    })
                                    .to_owned(),
                            ),
                            ("failure", failure.unwrap_or("—").to_owned()),
                        ];
                        if claims {
                            let amount = source["amount"]
                                .as_str()
                                .and_then(|v| v.parse::<Decimal>().ok())
                                .map(|v| Money::display_decimal(v, units))
                                .unwrap_or_else(|| "—".into());
                            cells.push((
                                "amount",
                                format!(
                                    "{amount} {}",
                                    source["currency_code"].as_str().unwrap_or("")
                                ),
                            ));
                            cells.push((
                                "category",
                                source["category_code"].as_str().unwrap_or("—").to_owned(),
                            ));
                        } else {
                            cells.push(("status", "Approved".into()));
                        }
                        // In the books: the journal it posted (or that
                        // reversed it), linked; else whether it failed.
                        let standing = if posted {
                            let mut html = render(Badge::status(
                                if reversed {
                                    BadgeKind::Warning
                                } else {
                                    BadgeKind::Success
                                },
                                if reversed { "Reversed" } else { "Posted" },
                            ))?;
                            html.push(&Html::text(" "));
                            html.push(&EntryLink::html(
                                posting["entry_id"].as_i64(),
                                posting["entry_number"].as_i64(),
                            )?);
                            html
                        } else if failure.is_some() {
                            render(Badge::status(BadgeKind::Danger, "Failed"))?
                        } else {
                            render(Badge::status(BadgeKind::Neutral, "Not posted"))?
                        };
                        let mut row =
                            row(&format!("payroll-{kind}-{id}"), &cells).slot("standing", standing);
                        if can_post && !posted {
                            let what = if claims { "claim" } else { "run" };
                            row.actions.push(&if failure.is_some() {
                                icon_action(
                                    &format!("payroll-post-{kind}-{id}"),
                                    if claims { "post_claim" } else { "post_run" },
                                    id,
                                    "arrow-path",
                                    &format!("Retry posting {what} {reference}"),
                                )?
                            } else {
                                icon_action(
                                    &format!("payroll-post-{kind}-{id}"),
                                    if claims { "post_claim" } else { "post_run" },
                                    id,
                                    "document-check",
                                    &format!("Post {what} {reference}"),
                                )?
                            });
                        } else if can_post && posted && !reversed {
                            row.actions.push(&icon_action(
                                &format!("payroll-reverse-{kind}-{id}"),
                                "reverse",
                                posting["id"].as_i64().ok_or(Error::Internal)?,
                                "arrow-uturn-left",
                                &format!(
                                    "Reverse journal {}",
                                    posting["entry_number"].as_i64().unwrap_or_default()
                                ),
                            )?);
                        }
                        rows.push(row);
                    }
                    content.push(&self.table(index, rows, title, empty, true)?);
                }
            }
            let section_id = if claims {
                "payroll-claims"
            } else {
                "payroll-runs"
            };
            let heading_id = if claims {
                "payroll-claims-heading"
            } else {
                "payroll-runs-heading"
            };
            let mut body = render(SectionHeading {
                id: heading_id,
                title,
                class: "p-5 pb-0",
                actions,
                ..SectionHeading::default()
            })?;
            body.push(&content);
            parts.push(render(Card {
                id: section_id,
                class: "mt-6",
                inner_class: "p-0",
                role: "region",
                labelledby: heading_id,
                body,
                ..Card::default()
            })?);
        }
        Ok(parts)
    }
}

/// The reversal's confirmation: its reason and date, then Cancel and the
/// danger verb, which submits them.
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as dialog %}{% import "components.html" as ui %}{% call dialog::modal(id="payroll-reversal-modal", title="Reverse this posting", cancel_event="cancel_reversal", width=bilibi_ui::ModalWidth::Compact, description="The reversal is a new journal with the sides swapped; the original stays in the books.", role="alertdialog") %}{{ alert|safe }}<form id="payroll-reversal-form" data-change data-event="change_reversal" data-submit-event="confirm_reversal" class="mt-4 space-y-3">{% for field in fields %}{{ field|safe }}{% endfor %}<div class="mt-3 flex flex-wrap justify-end gap-2">{% call ui::button(id="payroll-reversal-modal-cancel", button_type="button", event="cancel_reversal") %}Cancel{% endcall %}{% call ui::button(id="payroll-reversal-modal-confirm", button_type="submit", variant=bilibi_ui::ButtonVariant::Danger, working="Reversing…") %}Reverse posting{% endcall %}</div></form>{% endcall %}"#,
    ext = "html"
)]
struct ReversalDialog {
    alert: Html,
    fields: Vec<Html>,
}
