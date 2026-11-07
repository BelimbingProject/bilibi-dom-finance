//! One manual journal, read-only. While it waits, someone with the approval
//! permission approves it (and it posts) or rejects it with a reason, and the
//! person who submitted it corrects it (the list page's dialog, prefilled) or
//! withdraws it. The ledger refuses the person who submitted it when the
//! company separates duties.
use super::{
    dialog_actions, error_text, event_type, figure, instant,
    journals::{status_badge, INDEX},
    no_company, render, shell, unavailable, view, Announce, EntryLink, Failure, Flash, JournalLink,
    Pages, Read, Refusal, Review,
};
use crate::{APPROVE_JOURNALS, SUBMIT_JOURNALS};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BackLink, BuiltinColumn, Button, ButtonVariant, Card, Cell, ConfirmDialog,
    EmptyState, Fact, Facts, Field, FlexRow, FlexTable, Form, Header, Html, InputKind, ListColumns,
    Modal, PageVariant, SectionHeading, StatusKind, TableEmpty,
};
use rust_decimal::Decimal;
use serde_json::json;

/// A dialog that takes one reason: to reject a journal (required) or to
/// withdraw one (optional).
#[derive(Default)]
struct Reasoning {
    reason: String,
    refusal: Refusal,
    notice: String,
}

/// What differs between the two reason dialogs.
struct ReasonDialog {
    /// The form's name, which prefixes its fields' ids.
    form: &'static str,
    required: bool,
    hint: &'static str,
    /// The events the buttons raise.
    cancel: &'static str,
    confirm: &'static str,
    validate: &'static str,
    variant: ButtonVariant,
    label: &'static str,
    working: &'static str,
}

const REJECT: ReasonDialog = ReasonDialog {
    form: "reject",
    required: true,
    hint: "Up to 500 characters. The submitter sees it and can submit a corrected journal.",
    cancel: "close_reject",
    confirm: "reject",
    validate: "validate",
    variant: ButtonVariant::Danger,
    label: "Reject journal",
    working: "Rejecting…",
};

const WITHDRAW: ReasonDialog = ReasonDialog {
    form: "withdraw",
    required: false,
    hint: "Optional. Up to 500 characters. Kept with the withdrawn journal.",
    cancel: "close_withdraw",
    confirm: "withdraw",
    validate: "validate_withdraw",
    variant: ButtonVariant::Primary,
    label: "Withdraw journal",
    working: "Withdrawing…",
};

pub(super) struct Journal {
    pages: Pages,
    id: i64,
    flash: Flash,
    lines: Option<ListColumns>,
    approving: Option<Review>,
    rejecting: Option<Reasoning>,
    withdrawing: Option<Reasoning>,
}

impl Journal {
    pub(super) fn new(pages: Pages, id: i64) -> Self {
        Self {
            pages,
            id,
            flash: Flash::default(),
            lines: None,
            approving: None,
            rejecting: None,
            withdrawing: None,
        }
    }

    fn simple(&self, title: &str, body: Html) -> Result<LiveBody> {
        let mut page = render(Header {
            title: Html::text(title),
            ..Header::default()
        })?;
        page.push(&body);
        Ok(LiveBody {
            html: shell(
                "ledger-journal-page",
                PageVariant::Detail,
                &self.flash,
                page,
            )?,
            ..LiveBody::default()
        })
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        if Pages::company(scope).is_none() {
            return self.simple("Journal", no_company("ledger-journal-no-company")?);
        }
        let journal: view::Journal = match self
            .pages
            .read(
                scope,
                "finance.ledger.journal.get",
                json!({"journal_id": self.id}),
            )
            .await?
        {
            Read::Found(value) => match view::parse(value) {
                Some(journal) => journal,
                None => {
                    return self.simple(
                        "Journal",
                        unavailable("ledger-journal-refused", "this journal", &Read::Failed)?,
                    );
                }
            },
            Read::Failed | Read::Refused(_) => {
                return self.simple(
                    "Journal",
                    render(
                        EmptyState::missing(
                            "This journal does not exist",
                            "It may belong to another company. Go back to Journals.",
                        )
                        .id("ledger-journal-missing"),
                    )?,
                );
            }
            other => {
                return self.simple(
                    "Journal",
                    unavailable("ledger-journal-refused", "this journal", &other)?,
                );
            }
        };
        let state = self.pages.book(scope).await?.ok();
        let units = state
            .as_ref()
            .and_then(|s| s.book.as_ref())
            .map_or(2, |b| b.minor_units);
        let chart = self.pages.chart(scope).await?.unwrap_or_default();
        let display = self.pages.display(scope).await?;
        let people = self
            .pages
            .people(
                scope,
                [
                    Some(journal.submitted_by_user_id),
                    journal.submitted_by_impersonator_id,
                    journal.decision.as_ref().map(|d| d.actor_user_id),
                    journal
                        .decision
                        .as_ref()
                        .and_then(|d| d.actor_impersonator_id),
                ],
            )
            .await?;
        let can_decide = self.pages.allowed(scope, APPROVE_JOURNALS).await?;
        let can_submit = self.pages.allowed(scope, SUBMIT_JOURNALS).await?;
        let waiting = journal.status == "submitted";
        let mine = scope.user() == journal.submitted_by_user_id;
        let can_correct = waiting && mine && can_submit;
        let can_restart = mine
            && can_submit
            && matches!(journal.status.as_str(), "rejected" | "withdrawn")
            && journal.replaced_by_journal_id.is_none();

        let mut content = Html::EMPTY;
        if waiting && mine {
            let approval = "Approval follows the company's journal rules. Eligible decisions are listed in Waiting for my approval.";
            content.push(&render(Alert {
                id: "ledger-journal-own",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text(&if can_correct {
                    format!("You submitted this journal. {approval} To change it, use Correct: this journal is withdrawn and stays on record, and the corrected journal needs its own approval.")
                } else {
                    format!("You submitted this journal. {approval}")
                }),
            })?);
        } else if waiting && can_decide {
            content.push(&render(Alert {
                id: "ledger-journal-review-note",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text("Nothing posts until the journal is approved. To ask for a change, reject it with a reason: the submitter can then start a corrected journal from it, which needs its own approval."),
            })?);
        } else if journal.replaced_by_journal_id.is_some() {
            content.push(&render(Alert {
                id: "ledger-journal-replaced-note",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text("This journal was withdrawn and nothing posted from it. The corrected journal is linked under Replaced by."),
            })?);
        } else if journal.status == "withdrawn" {
            content.push(&render(Alert {
                id: "ledger-journal-withdrawn-note",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text(if can_restart {
                    "This journal was withdrawn and nothing posted from it. Start again from it to submit a corrected journal."
                } else {
                    "This journal was withdrawn and nothing posted from it."
                }),
            })?);
        } else if journal.status == "rejected" && can_restart {
            content.push(&render(Alert {
                id: "ledger-journal-rejected-note",
                kind: StatusKind::Info,
                class: "mb-3",
                body: Html::text("This journal was rejected and nothing posted from it. Start again from it to submit a corrected journal; it needs its own approval."),
            })?);
        }
        let total: Decimal = journal.lines.iter().map(|l| view::decimal(&l.debit)).sum();
        let mut facts = vec![
            Fact {
                title: "Request",
                id: "ledger-journal-number",
                value: Html::text(&journal.journal_number.to_string()),
            },
            Fact {
                title: "Date",
                id: "ledger-journal-date",
                value: Html::text(&journal.entry_date),
            },
            Fact {
                title: "Description",
                id: "ledger-journal-description",
                value: Html::text(&journal.description),
            },
            Fact {
                title: "Reference",
                id: "ledger-journal-reference",
                value: Html::text(journal.reference.as_deref().unwrap_or("—")),
            },
            Fact {
                title: "Currency",
                id: "ledger-journal-currency",
                value: Html::text(&journal.currency_code),
            },
            Fact {
                title: "Total",
                id: "ledger-journal-total",
                value: Html::text(&view::amount(&total.to_string(), units)),
            },
            Fact {
                title: "Status",
                id: "ledger-journal-status",
                value: status_badge(&journal.status)?,
            },
            Fact {
                title: "Submitted by",
                id: "ledger-journal-by",
                value: Html::text(&people.who(
                    Some(journal.submitted_by_user_id),
                    journal.submitted_by_impersonator_id,
                )),
            },
            Fact {
                title: "Submitted",
                id: "ledger-journal-at",
                value: instant(
                    &display,
                    &journal.submitted_at,
                    "ledger-journal-submitted-at",
                )?,
            },
        ];
        if let Some(decision) = &journal.decision {
            facts.push(Fact {
                title: match decision.event.as_str() {
                    "approved" => "Approved by",
                    "withdrawn" => "Withdrawn by",
                    _ => "Rejected by",
                },
                id: "ledger-journal-decided-by",
                value: Html::text(
                    &people.who(Some(decision.actor_user_id), decision.actor_impersonator_id),
                ),
            });
            facts.push(Fact {
                title: match decision.event.as_str() {
                    "approved" => "Approved",
                    "withdrawn" => "Withdrawn",
                    _ => "Rejected",
                },
                id: "ledger-journal-decided-at",
                value: instant(
                    &display,
                    &decision.occurred_at,
                    "ledger-journal-decided-time",
                )?,
            });
            if let Some(reason) = &decision.reason {
                facts.push(Fact {
                    title: "Reason",
                    id: "ledger-journal-reason",
                    value: Html::text(reason),
                });
            }
            if let Some(entry) = decision.entry_id {
                facts.push(Fact {
                    title: "Posted as",
                    id: "ledger-journal-entry",
                    value: render(EntryLink {
                        id: entry,
                        label: "See the journal entry",
                    })?,
                });
            }
        }
        if let (Some(old), Some(number)) =
            (journal.replaces_journal_id, journal.replaces_journal_number)
        {
            facts.push(Fact {
                title: "Replaces",
                id: "ledger-journal-replaces",
                value: render(JournalLink {
                    id: old,
                    label: &format!("Journal {number}, withdrawn"),
                })?,
            });
        }
        if let (Some(new), Some(number)) = (
            journal.replaced_by_journal_id,
            journal.replaced_by_journal_number,
        ) {
            facts.push(Fact {
                title: "Replaced by",
                id: "ledger-journal-replaced-by",
                value: render(JournalLink {
                    id: new,
                    label: &format!("Journal {number}"),
                })?,
            });
        }
        let mut head = render(SectionHeading {
            id: "ledger-journal-heading",
            title: "Journal",
            ..SectionHeading::default()
        })?;
        head.push(&render(Facts {
            id: "ledger-journal-facts",
            items: facts,
        })?);
        content.push(&render(Card {
            id: "ledger-journal-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-journal-heading",
            body: head,
            ..Card::default()
        })?);

        if self.lines.is_none() {
            self.lines = Some(
                ListColumns::mount(
                    "ledger-journal-lines",
                    vec![
                        BuiltinColumn::new("line", "Line"),
                        BuiltinColumn::new("account", "Account"),
                        BuiltinColumn::new("description", "Description"),
                        BuiltinColumn::new("debit", "Debit").right(),
                        BuiltinColumn::new("credit", "Credit").right(),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        let columns = self.lines.as_mut().ok_or(Error::Internal)?;
        columns.load(&journal.lines, |l| l.line_number.to_string());
        let mut rows = Vec::new();
        for line in &journal.lines {
            let account = match chart.iter().find(|a| a.id == line.account_id) {
                Some(account) => super::account_name(account)?,
                None => Html::text(&format!("Account #{}", line.account_id)),
            };
            rows.push(
                FlexRow::new(line.line_number.to_string())
                    .cell("line", Cell::text(line.line_number.to_string()))
                    .slot(
                        "account",
                        render(bilibi_ui::ActionLink {
                            href: &format!("/finance/ledger/accounts/{}", line.account_id),
                            body: account,
                            ..bilibi_ui::ActionLink::default()
                        })?,
                    )
                    .cell(
                        "description",
                        Cell::text(line.description.as_deref().unwrap_or("—")),
                    )
                    .slot("debit", figure(&view::amount_or_dash(&line.debit, units))?)
                    .slot(
                        "credit",
                        figure(&view::amount_or_dash(&line.credit, units))?,
                    ),
            );
        }
        let credit_total: Decimal = journal.lines.iter().map(|l| view::decimal(&l.credit)).sum();
        rows.push(
            FlexRow::new("total")
                .cell("line", Cell::text("Total"))
                .slot("debit", figure(&view::amount(&total.to_string(), units))?)
                .slot(
                    "credit",
                    figure(&view::amount(&credit_total.to_string(), units))?,
                ),
        );
        let mut table = FlexTable::new("ledger-journal-lines", columns.flex_columns(), rows);
        table.caption = "Lines of the journal";
        table.framed = false;
        table.zoom = columns.zoom;
        table.empty = Some(TableEmpty::Content(Html::text(
            "This journal has no lines.",
        )));
        let mut lines = render(SectionHeading {
            id: "ledger-journal-lines-heading",
            title: "Lines",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        lines.push(&render(table)?);
        content.push(&render(Card {
            id: "ledger-journal-lines-card",
            inner_class: "p-0",
            class: "mt-4",
            role: "region",
            labelledby: "ledger-journal-lines-heading",
            body: lines,
            ..Card::default()
        })?);

        let mut actions = Html::EMPTY;
        if waiting && can_decide {
            actions.push(&render(Button {
                id: "ledger-journal-approve",
                button_type: "button",
                event: "ask_approve",
                variant: ButtonVariant::Primary,
                body: Html::text("Approve"),
                ..Button::default()
            })?);
            actions.push(&render(Button {
                id: "ledger-journal-reject",
                button_type: "button",
                event: "open_reject",
                body: Html::text("Reject"),
                ..Button::default()
            })?);
        }
        if can_correct {
            actions.push(&render(Button {
                id: "ledger-journal-correct",
                href: &if journal.opening {
                    format!("{}?correct={}", super::opening::PATH, journal.id)
                } else {
                    format!("{INDEX}?correct={}", journal.id)
                },
                body: Html::text("Correct"),
                ..Button::default()
            })?);
            actions.push(&render(Button {
                id: "ledger-journal-withdraw",
                button_type: "button",
                event: "open_withdraw",
                body: Html::text("Withdraw"),
                ..Button::default()
            })?);
        }
        if can_restart {
            actions.push(&render(Button {
                id: "ledger-journal-restart",
                href: &if journal.opening {
                    super::opening::PATH.to_owned()
                } else {
                    format!("{INDEX}?copy={}", journal.id)
                },
                body: Html::text("Start again from this journal"),
                ..Button::default()
            })?);
        }
        actions.push(&render(BackLink {
            href: INDEX,
            id: "ledger-journal-back",
            title: "Back to Journals",
            ..BackLink::default()
        })?);
        let posted = match journal.decision.as_ref().and_then(|d| d.entry_id) {
            Some(entry_id) => match self
                .pages
                .read(
                    scope,
                    "finance.ledger.entry.get",
                    json!({"entry_id": entry_id}),
                )
                .await?
            {
                Read::Found(value) => view::parse::<view::Entry>(value).map(|e| e.entry_number),
                _ => None,
            },
            None => None,
        };
        let name = match posted {
            Some(number) => format!("Journal {number}"),
            None => "Draft journal".to_owned(),
        };
        let subtitle = format!(
            "Request {} · {}",
            journal.journal_number, journal.description
        );
        let mut page = render(Header {
            title: Html::text(&name),
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&render(Announce {
            kind: crate::MODULE,
            record_id: &format!("journal-{}", journal.id),
        })?);
        page.push(&content);
        if self.approving.is_some() {
            page.push(&render(ConfirmDialog {
                id: "ledger-journal-confirm",
                consequence: &format!("Approve journal {}?", journal.journal_number),
                detail: "Your decision is recorded under the journal's approval rules. The final required approval posts an immutable entry; a posted entry is corrected by reversal.",
                confirm: "Approve",
                working: "Recording…",
                confirm_event: "approve",
                cancel_event: "cancel_approve",
            })?);
        }
        if let Some(rejecting) = &self.rejecting {
            page.push(&render(Modal {
                id: "ledger-journal-reject-modal",
                title: "Reject journal",
                cancel_event: "close_reject",
                description: "Nothing posts. The journal stays on record with your reason.",
                body: self.reason_form(&REJECT, rejecting)?,
                ..Modal::default()
            })?);
        }
        if let Some(withdrawing) = &self.withdrawing {
            page.push(&render(Modal {
                id: "ledger-journal-withdraw-modal",
                title: "Withdraw journal",
                cancel_event: "close_withdraw",
                description: "Nothing posts and the journal can no longer be approved. It stays on record with your name and the time. To change it, correct it instead.",
                body: self.reason_form(&WITHDRAW, withdrawing)?,
                ..Modal::default()
            })?);
        }
        Ok(LiveBody {
            html: shell(
                "ledger-journal-page",
                PageVariant::Detail,
                &self.flash,
                page,
            )?,
            rows: journal.lines.len(),
            ..LiveBody::default()
        })
    }

    fn reason_form(&self, dialog: &ReasonDialog, state: &Reasoning) -> Result<Html> {
        let form = dialog.form;
        let errors: Vec<String> = state
            .refusal
            .field("reason")
            .iter()
            .map(|m| error_text("reason", m))
            .collect();
        let mut body = Html::EMPTY;
        let mut messages = state.refusal.alert(&["reason"]);
        if !state.notice.is_empty() {
            messages.push(state.notice.clone());
        }
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: &format!("ledger-journal-{form}-notice"),
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&messages.join(" ")),
            })?);
        }
        let mut field = Field::in_form(form, "reason")
            .id(format!("ledger-{form}-reason"))
            .label("Reason")
            .kind(InputKind::Textarea)
            .value(&state.reason)
            .hint(dialog.hint)
            .errors(errors)
            .attr("data-debounce", "300");
        if dialog.required {
            field = field.required();
        }
        body.push(&render(field)?);
        let mut actions = render(Button {
            id: &format!("ledger-{form}-cancel"),
            button_type: "button",
            event: dialog.cancel,
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: &format!("ledger-{form}-submit"),
            button_type: "button",
            event: dialog.confirm,
            variant: dialog.variant,
            working: dialog.working,
            body: Html::text(dialog.label),
            ..Button::default()
        })?);
        render(Form {
            id: &format!("ledger-{form}-form"),
            event: dialog.validate,
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }

    async fn ask_approve(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        match self
            .pages
            .review(
                r.scope,
                r.credential,
                "ask_approve",
                "finance.ledger.journal.approve",
                json!({"journal_id": self.id}),
            )
            .await?
        {
            Ok(review) => self.approving = Some(review),
            Err(failure) => self.flash.error(failure.text("The journal")),
        }
        Ok(())
    }

    async fn approve(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(review) = self.approving.take() else {
            return Ok(());
        };
        match self
            .pages
            .commit(
                r.scope,
                r.credential,
                "approve",
                "finance.ledger.journal.approve",
                review,
            )
            .await?
        {
            Ok(_) => self.flash.success("Your approval is recorded."),
            Err(failure) => self.flash.error(failure.text("The journal")),
        }
        Ok(())
    }

    async fn withdraw(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(withdrawing) = &mut self.withdrawing else {
            return Ok(());
        };
        let mut input = json!({"journal_id": self.id});
        if !withdrawing.reason.trim().is_empty() {
            input["reason"] = json!(withdrawing.reason.trim());
        }
        match self
            .pages
            .submit(
                r.scope,
                r.credential,
                "withdraw",
                "finance.ledger.journal.withdraw",
                input,
            )
            .await?
        {
            Ok(_) => {
                self.withdrawing = None;
                self.flash
                    .success("The journal is withdrawn. Nothing was posted.");
            }
            Err(Failure::Refused(refusal)) => {
                withdrawing.refusal = refusal;
                withdrawing.notice.clear();
            }
            Err(failure) => withdrawing.notice = failure.text("The withdrawal"),
        }
        Ok(())
    }

    async fn reject(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(rejecting) = &mut self.rejecting else {
            return Ok(());
        };
        match self
            .pages
            .submit(
                r.scope,
                r.credential,
                "reject",
                "finance.ledger.journal.reject",
                json!({"journal_id": self.id, "reason": rejecting.reason.trim()}),
            )
            .await?
        {
            Ok(_) => {
                self.rejecting = None;
                self.flash
                    .success("The journal is rejected. Nothing was posted.");
            }
            Err(Failure::Refused(refusal)) => {
                rejecting.refusal = refusal;
                rejecting.notice.clear();
            }
            Err(failure) => rejecting.notice = failure.text("The rejection"),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Journal {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "ask_approve" => self.ask_approve(&r).await?,
                    "cancel_approve" => self.approving = None,
                    "approve" => self.approve(&r).await?,
                    "open_reject" => self.rejecting = Some(Reasoning::default()),
                    "close_reject" => self.rejecting = None,
                    "open_withdraw" => self.withdrawing = Some(Reasoning::default()),
                    "close_withdraw" => self.withdrawing = None,
                    "validate_withdraw" => {
                        if let Some(withdrawing) = &mut self.withdrawing {
                            withdrawing.reason = params::form_values(value, "withdraw")
                                .get("reason")
                                .cloned()
                                .unwrap_or_default();
                            withdrawing.refusal = Refusal::default();
                            withdrawing.notice.clear();
                        }
                    }
                    "withdraw" => self.withdraw(&r).await?,
                    "validate" => {
                        if let Some(rejecting) = &mut self.rejecting {
                            rejecting.reason = params::form_values(value, "reject")
                                .get("reason")
                                .cloned()
                                .unwrap_or_default();
                            rejecting.refusal = Refusal::default();
                            rejecting.notice.clear();
                        }
                    }
                    "reject" => self.reject(&r).await?,
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}
