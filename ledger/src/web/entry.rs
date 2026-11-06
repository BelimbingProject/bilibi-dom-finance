//! One journal entry, read-only. A manual entry can be reversed from here: the
//! ledger posts a mirror entry and keeps the original.
use super::{
    dialog_actions, error_text, event_type, figure, instant, journals::INDEX, no_company, render,
    shell, source_label, unavailable, view, Announce, Failure, Flash, Pages, Read, Refusal,
};
use crate::POST_ENTRIES;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell,
    EmptyState, Fact, Facts, Field, FlexRow, FlexTable, Form, Header, Html, InputKind, ListColumns,
    Modal, PageVariant, SectionHeading, StatusKind, TableEmpty,
};
use rust_decimal::Decimal;
use serde_json::json;

#[derive(Default)]
struct Reverse {
    date: String,
    description: String,
    reference: String,
    refusal: Refusal,
    notice: String,
}

#[derive(Default)]
struct LateReview {
    reason: String,
    refusal: Refusal,
    notice: String,
}

pub(super) struct Entry {
    pages: Pages,
    id: i64,
    flash: Flash,
    lines: Option<ListColumns>,
    reverse: Option<Reverse>,
    late_review: Option<LateReview>,
}

impl Entry {
    pub(super) fn new(pages: Pages, id: i64) -> Self {
        Self {
            pages,
            id,
            flash: Flash::default(),
            lines: None,
            reverse: None,
            late_review: None,
        }
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        if Pages::company(scope).is_none() {
            return self.simple("Journal", no_company("ledger-entry-no-company")?);
        }
        let entry: view::Entry = match self
            .pages
            .read(
                scope,
                "finance.ledger.entry.get",
                json!({"entry_id": self.id}),
            )
            .await?
        {
            Read::Found(value) => match view::parse(value) {
                Some(entry) => entry,
                None => {
                    return self.simple(
                        "Journal",
                        unavailable("ledger-entry-refused", "this entry", &Read::Failed)?,
                    );
                }
            },
            Read::Failed | Read::Refused(_) => {
                return self.simple(
                    "Journal",
                    render(
                        EmptyState::missing(
                            "This entry does not exist",
                            "It may belong to another company. Go back to Journals.",
                        )
                        .id("ledger-entry-missing"),
                    )?,
                );
            }
            other => {
                return self.simple(
                    "Journal",
                    unavailable("ledger-entry-refused", "this entry", &other)?,
                );
            }
        };
        let units = self
            .pages
            .book(scope)
            .await?
            .ok()
            .and_then(|b| b.book)
            .map_or(2, |b| b.minor_units);
        let chart = self.pages.chart(scope).await?.unwrap_or_default();
        let display = self.pages.display(scope).await?;
        let can_post = self.pages.allowed(scope, POST_ENTRIES).await?;
        let review = entry.late_recording.as_ref().and_then(|r| r.review.as_ref());
        let people = self
            .pages
            .people(
                scope,
                [
                    Some(entry.posted_by_user_id),
                    entry.posted_by_impersonator_id,
                    review.map(|r| r.reviewed_by_user_id),
                    review.and_then(|r| r.reviewed_by_impersonator_id),
                ],
            )
            .await?;

        let mut notices = Html::EMPTY;
        let mut note = |id: &str, text: String, link: Option<(i64, &str)>| -> Result<()> {
            let mut body = Html::text(&text);
            if let Some((target, label)) = link {
                body.push(&Html::text(" "));
                body.push(&render(super::EntryLink { id: target, label })?);
            }
            notices.push(&render(Alert {
                id,
                kind: StatusKind::Info,
                class: "mb-3",
                body,
            })?);
            Ok(())
        };
        if let Some(by) = entry.reversed_by_entry_id {
            note(
                "ledger-entry-reversed",
                "This entry has been reversed.".into(),
                Some((by, "See the reversal")),
            )?;
        }
        if let Some(of) = entry.reverses_entry_id {
            note(
                "ledger-entry-reversal",
                "This entry reverses another entry.".into(),
                Some((of, "See the original")),
            )?;
        }
        if let Some(owner) = &entry.owner {
            match super::owner_page(&owner.module, &owner.record) {
                Some((href, noun)) => {
                    let mut body = Html::text(&format!("Posted from a {noun}. Correct it on the "));
                    body.push(&render(super::RecordLink { href: &href, label: noun })?);
                    body.push(&Html::text("."));
                    notices.push(&render(Alert {
                        id: "ledger-entry-owned",
                        kind: StatusKind::Info,
                        class: "mb-3",
                        body,
                    })?);
                }
                None => note(
                    "ledger-entry-owned",
                    format!(
                        "Posted by {}. Correct it there, not here.",
                        source_label(&owner.module)
                    ),
                    None,
                )?,
            }
        }

        let debit_total: Decimal = entry.lines.iter().map(|l| view::decimal(&l.debit)).sum();
        let facts = vec![
            Fact {
                title: "Number",
                id: "ledger-entry-number",
                value: Html::text(&entry.entry_number.to_string()),
            },
            Fact {
                title: "Date",
                id: "ledger-entry-date",
                value: Html::text(&entry.entry_date),
            },
            Fact {
                title: "Description",
                id: "ledger-entry-description",
                value: Html::text(&entry.description),
            },
            Fact {
                title: "Reference",
                id: "ledger-entry-reference",
                value: Html::text(entry.reference.as_deref().unwrap_or("—")),
            },
            Fact {
                title: "Source",
                id: "ledger-entry-source",
                value: Html::text(&if entry.opening {
                    "Opening".to_owned()
                } else {
                    entry
                        .owner
                        .as_ref()
                        .map_or("Manual".to_owned(), |o| source_label(&o.module))
                }),
            },
            Fact {
                title: "Currency",
                id: "ledger-entry-currency",
                value: Html::text(&entry.currency_code),
            },
            Fact {
                title: "Total",
                id: "ledger-entry-total",
                value: Html::text(&view::amount(&debit_total.to_string(), units)),
            },
            Fact {
                title: "Status",
                id: "ledger-entry-status",
                value: entry_status(&entry)?,
            },
            Fact {
                title: "Posted by",
                id: "ledger-entry-by",
                value: Html::text(&people.who(
                    Some(entry.posted_by_user_id),
                    entry.posted_by_impersonator_id,
                )),
            },
            Fact {
                title: "Posted at",
                id: "ledger-entry-at",
                value: instant(&display, &entry.posted_at, "ledger-entry-posted-at")?,
            },
        ];
        let mut head = render(SectionHeading {
            id: "ledger-entry-heading",
            title: "Entry",
            ..SectionHeading::default()
        })?;
        head.push(&render(Facts {
            id: "ledger-entry-facts",
            items: facts,
        })?);
        let mut content = notices;
        content.push(&render(Card {
            id: "ledger-entry-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-entry-heading",
            body: head,
            ..Card::default()
        })?);

        // The lines.
        if self.lines.is_none() {
            self.lines = Some(
                ListColumns::mount(
                    "ledger-entry-lines",
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
        columns.load(&entry.lines, |l| l.line_number.to_string());
        let mut rows = Vec::new();
        for line in &entry.lines {
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
        let credit_total: Decimal = entry.lines.iter().map(|l| view::decimal(&l.credit)).sum();
        rows.push(
            FlexRow::new("total")
                .cell("line", Cell::text("Total"))
                .slot(
                    "debit",
                    figure(&view::amount(&debit_total.to_string(), units))?,
                )
                .slot(
                    "credit",
                    figure(&view::amount(&credit_total.to_string(), units))?,
                ),
        );
        let mut table = FlexTable::new("ledger-entry-lines", columns.flex_columns(), rows);
        table.caption = "Lines of the entry";
        table.framed = false;
        table.zoom = columns.zoom;
        table.empty = Some(TableEmpty::Content(Html::text("This entry has no lines.")));
        let mut lines = render(SectionHeading {
            id: "ledger-entry-lines-heading",
            title: "Lines",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        lines.push(&render(table)?);
        content.push(&render(Card {
            id: "ledger-entry-lines-card",
            inner_class: "p-0",
            class: "mt-4",
            role: "region",
            labelledby: "ledger-entry-lines-heading",
            body: lines,
            ..Card::default()
        })?);

        let mut actions = Html::EMPTY;
        if let Some(recording) = &entry.late_recording {
            let mut body = render(SectionHeading {
                title: "Late recording",
                ..SectionHeading::default()
            })?;
            body.push(&Html::text(&format!(
                "Recorded {} days after its transaction date ({}). ",
                recording.days,
                if recording.status == "late" {
                    "late"
                } else {
                    "approaching late"
                }
            )));
            if let Some(review) = &recording.review {
                body.push(&render(Facts {
                    id: "ledger-entry-review-facts",
                    items: vec![
                        Fact {
                            title: "Reason",
                            id: "ledger-entry-review-reason",
                            value: Html::text(&review.reason),
                        },
                        Fact {
                            title: "Reviewed by",
                            id: "ledger-entry-review-by",
                            value: Html::text(&people.who(
                                Some(review.reviewed_by_user_id),
                                review.reviewed_by_impersonator_id,
                            )),
                        },
                        Fact {
                            title: "Reviewed at",
                            id: "ledger-entry-review-at",
                            value: instant(
                                &display,
                                &review.reviewed_at,
                                "ledger-entry-review-time",
                            )?,
                        },
                    ],
                })?);
            } else {
                body.push(&Html::text("Pending independent review."));
                if self
                    .pages
                    .allowed(scope, crate::REVIEW_LATE_RECORDINGS)
                    .await?
                {
                    actions.push(&render(Button {
                        id: "ledger-entry-review",
                        event: "open_late_review",
                        button_type: "button",
                        body: Html::text("Review late recording"),
                        ..Button::default()
                    })?);
                }
            }
            content.push(&render(Card {
                id: "ledger-entry-recording",
                class: "mt-4",
                body,
                ..Card::default()
            })?);
        }

        let mut reversible = entry.owner.is_none()
            && entry.reverses_entry_id.is_none()
            && entry.reversed_by_entry_id.is_none();
        if can_post && reversible {
            if let Read::Found(value) = self
                .pages
                .read(
                    scope,
                    "finance.ledger.fiscal_year.list",
                    json!({"page_size": 100}),
                )
                .await?
            {
                let years: Vec<view::Year> =
                    view::parse(value["records"].clone()).unwrap_or_default();
                reversible = !years.iter().any(|y| y.closing_entry_id == Some(entry.id));
            }
        }
        if can_post && reversible {
            actions.push(&render(Button {
                id: "ledger-entry-reverse",
                button_type: "button",
                event: "open_reverse",
                body: Html::text("Reverse entry"),
                ..Button::default()
            })?);
        }
        actions.push(&render(BackLink {
            href: INDEX,
            id: "ledger-entry-back",
            title: "Back to Journals",
            ..BackLink::default()
        })?);
        let name = format!("Journal {}", entry.entry_number);
        let subtitle = match entry.request_number {
            Some(request) => format!("Request {request} · {}", entry.description),
            None => entry.description.clone(),
        };

        let mut page = render(Header {
            title: Html::text(&name),
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&render(Announce {
            kind: crate::MODULE,
            record_id: &entry.id.to_string(),
        })?);
        page.push(&content);
        if let Some(reverse) = &self.reverse {
            page.push(&render(Modal {
                id: "ledger-entry-reverse-modal",
                title: "Reverse entry",
                cancel_event: "close_reverse",
                description: "The ledger posts a new entry with the debits and credits swapped. The original entry stays.",
                body: self.reverse_form(reverse)?,
                ..Modal::default()
            })?);
        }
        if let Some(review) = &self.late_review {
            let mut body = render(
                Field::in_form("late_review", "reason")
                    .id("ledger-entry-review-input")
                    .label("Why it was recorded late")
                    .kind(InputKind::Textarea)
                    .value(&review.reason)
                    .required()
                    .attr("maxlength", "500")
                    .errors(
                        review
                            .refusal
                            .field("reason")
                            .iter()
                            .map(|m| error_text("reason", m))
                            .collect::<Vec<_>>(),
                    ),
            )?;
            let mut messages = review.refusal.alert(&["reason"]);
            if !review.notice.is_empty() {
                messages.push(review.notice.clone());
            }
            if !messages.is_empty() {
                body.push(&render(Alert {
                    id: "ledger-entry-review-refusal",
                    kind: StatusKind::Error,
                    body: Html::text(&messages.join(" ")),
                    ..Alert::default()
                })?);
            }
            let mut actions = render(Button {
                event: "close_late_review",
                button_type: "button",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                id: "ledger-entry-review-submit",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Record review"),
                working: "Recording…",
                ..Button::default()
            })?);
            page.push(&render(Modal { id: "ledger-entry-review-modal", title: "Review late recording", description: "Your reason, name and review time are permanent. Someone who recorded the entry, or acted as that person, cannot review it.", cancel_event: "close_late_review", body: render(Form { id: "ledger-entry-review-form", event: "record_late_review", class: "mt-4", body, actions: dialog_actions(actions)?, ..Form::default() })?, ..Modal::default() })?);
        }
        Ok(LiveBody {
            html: shell("ledger-entry-page", PageVariant::Detail, &self.flash, page)?,
            rows: entry.lines.len(),
            ..LiveBody::default()
        })
    }

    fn simple(&self, title: &str, body: Html) -> Result<LiveBody> {
        let mut page = render(Header {
            title: Html::text(title),
            ..Header::default()
        })?;
        page.push(&body);
        if let Some(review) = &self.late_review {
            let mut body = render(
                Field::in_form("late_review", "reason")
                    .id("ledger-entry-review-input")
                    .label("Why it was recorded late")
                    .kind(InputKind::Textarea)
                    .value(&review.reason)
                    .required()
                    .attr("maxlength", "500")
                    .errors(
                        review
                            .refusal
                            .field("reason")
                            .iter()
                            .map(|m| error_text("reason", m))
                            .collect::<Vec<_>>(),
                    ),
            )?;
            let mut messages = review.refusal.alert(&["reason"]);
            if !review.notice.is_empty() {
                messages.push(review.notice.clone());
            }
            if !messages.is_empty() {
                body.push(&render(Alert {
                    id: "ledger-entry-review-refusal",
                    kind: StatusKind::Error,
                    body: Html::text(&messages.join(" ")),
                    ..Alert::default()
                })?);
            }
            let mut actions = render(Button {
                event: "close_late_review",
                button_type: "button",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                id: "ledger-entry-review-submit",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Record review"),
                working: "Recording…",
                ..Button::default()
            })?);
            page.push(&render(Modal { id: "ledger-entry-review-modal", title: "Review late recording", description: "Your reason, name and review time are permanent. Someone who recorded the entry, or acted as that person, cannot review it.", cancel_event: "close_late_review", body: render(Form { id: "ledger-entry-review-form", event: "record_late_review", class: "mt-4", body, actions: dialog_actions(actions)?, ..Form::default() })?, ..Modal::default() })?);
        }
        Ok(LiveBody {
            html: shell("ledger-entry-page", PageVariant::Detail, &self.flash, page)?,
            ..LiveBody::default()
        })
    }

    fn reverse_form(&self, reverse: &Reverse) -> Result<Html> {
        let errors = |name: &str| -> Vec<String> {
            reverse
                .refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect()
        };
        let mut body = Html::EMPTY;
        let mut messages = reverse
            .refusal
            .alert(&["entry_date", "description", "reference"]);
        if !reverse.notice.is_empty() {
            messages.push(reverse.notice.clone());
        }
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: "ledger-entry-reverse-notice",
                kind: StatusKind::Error,
                body: Html::text(&messages.join(" ")),
                ..Alert::default()
            })?);
        }
        for field in [
            Field::in_form("reverse", "entry_date")
                .id("ledger-reverse-date")
                .label("Date of the reversal")
                .kind(InputKind::Date)
                .value(&reverse.date)
                .required()
                .hint(
                    "The date must not precede the original entry and must fall in an open period.",
                )
                .errors(errors("entry_date")),
            Field::in_form("reverse", "description")
                .id("ledger-reverse-description")
                .label("Description")
                .value(&reverse.description)
                .required()
                .errors(errors("description"))
                .attr("data-debounce", "300"),
            Field::in_form("reverse", "reference")
                .id("ledger-reverse-reference")
                .label("Reference")
                .value(&reverse.reference)
                .errors(errors("reference"))
                .attr("data-debounce", "300"),
        ] {
            body.push(&render(field)?);
        }
        let mut actions = render(Button {
            id: "ledger-reverse-cancel",
            button_type: "button",
            event: "close_reverse",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "ledger-reverse-submit",
            button_type: "button",
            event: "reverse",
            variant: ButtonVariant::Primary,
            working: "Reversing…",
            body: Html::text("Reverse entry"),
            ..Button::default()
        })?);
        render(Form {
            id: "ledger-reverse-form",
            event: "validate",
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }

    async fn open_reverse(&mut self, scope: &Scope) -> Result<()> {
        if !self.pages.allowed(scope, POST_ENTRIES).await? {
            self.flash.error(super::DENIED);
            return Ok(());
        }
        let Read::Found(value) = self
            .pages
            .read(
                scope,
                "finance.ledger.entry.get",
                json!({"entry_id": self.id}),
            )
            .await?
        else {
            return Ok(());
        };
        let Some(entry) = view::parse::<view::Entry>(value) else {
            return Ok(());
        };
        let today = self.pages.today(scope).await?.to_string();
        let date = if today > entry.entry_date {
            today
        } else {
            entry.entry_date.clone()
        };
        let mut description = format!(
            "Reversal of entry {}: {}",
            entry.entry_number, entry.description
        );
        description.truncate(
            description
                .char_indices()
                .nth(500)
                .map_or(description.len(), |(i, _)| i),
        );
        self.reverse = Some(Reverse {
            date,
            description,
            ..Reverse::default()
        });
        Ok(())
    }

    async fn reverse_now(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(reverse) = &mut self.reverse else {
            return Ok(());
        };
        let mut input = json!({
            "entry_id": self.id,
            "entry_date": reverse.date,
            "description": reverse.description.trim(),
        });
        if !reverse.reference.trim().is_empty() {
            input["reference"] = json!(reverse.reference.trim());
        }
        match self
            .pages
            .submit(
                r.scope,
                r.credential,
                "reverse",
                "finance.ledger.entry.reverse",
                input,
            )
            .await?
        {
            Ok(done) => {
                let number = done["result"]["record"]["entry_number"].as_i64();
                self.reverse = None;
                self.flash.success(match number {
                    Some(n) => format!("The entry is reversed. The reversal is entry {n}."),
                    None => "The entry is reversed.".into(),
                });
            }
            Err(Failure::Refused(refusal)) => {
                reverse.refusal = refusal;
                reverse.notice.clear();
            }
            Err(failure) => reverse.notice = failure.text("The reversal"),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Entry {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "open_late_review" => {
                        if self
                            .pages
                            .allowed(r.scope, crate::REVIEW_LATE_RECORDINGS)
                            .await?
                        {
                            self.late_review = Some(LateReview::default());
                        }
                    }
                    "close_late_review" => self.late_review = None,
                    "record_late_review" => {
                        if let Some(review) = &mut self.late_review {
                            let fields = params::form_values_checked(value, "late_review", 4096)
                                .map_err(Error::refused)?;
                            review.reason = fields.get("reason").cloned().unwrap_or_default();
                            review.refusal = Refusal::default();
                            review.notice.clear();
                            match self
                                .pages
                                .submit(
                                    r.scope,
                                    r.credential,
                                    "record_late_review",
                                    "finance.ledger.entry.review_late",
                                    json!({"entry_id": self.id, "reason": review.reason}),
                                )
                                .await?
                            {
                                Ok(_) => {
                                    self.late_review = None;
                                    self.flash.success("The independent review is recorded.");
                                }
                                Err(Failure::Refused(refusal)) => review.refusal = refusal,
                                Err(failure) => review.notice = failure.text("The review"),
                            }
                        }
                    }
                    "open_reverse" => self.open_reverse(r.scope).await?,
                    "close_reverse" => self.reverse = None,
                    "validate" => {
                        if let Some(reverse) = &mut self.reverse {
                            let fields = params::form_values(value, "reverse");
                            reverse.date = fields.get("entry_date").cloned().unwrap_or_default();
                            reverse.description =
                                fields.get("description").cloned().unwrap_or_default();
                            reverse.reference =
                                fields.get("reference").cloned().unwrap_or_default();
                            reverse.refusal = Refusal::default();
                            reverse.notice.clear();
                        }
                    }
                    "reverse" => self.reverse_now(&r).await?,
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}

fn entry_status(entry: &view::Entry) -> Result<Html> {
    render(if entry.reversed_by_entry_id.is_some() {
        Badge::status(BadgeKind::Warning, "Reversed")
    } else if entry.reverses_entry_id.is_some() {
        Badge::status(BadgeKind::Neutral, "Reversal")
    } else {
        Badge::status(BadgeKind::Success, "Posted")
    })
}
