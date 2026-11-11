//! Mirrors Bilimbi `Finance.Reconciliation.Web.ReconciliationLive`: the
//! statement balance, the book balance, the items that explain the
//! difference, and the difference itself. An open reconciliation reads its
//! figures now, follows the ledger's postings, lets a statement line the books
//! lack be posted as a bank charge or interest, or recorded where it belongs
//! (a receipt for money in, a payment for money out, each in its own module's
//! form through `MoneySeen`), and is finalised once the difference is zero or
//! abandoned with a reason; finalising while statement lines are not in the
//! books asks for a reason first. A finalised one shows the figures and items
//! it was finalised with, which never change. Every write is a facade call
//! that re-asks its capability.
use super::{clear_flash, company, event_type, flash_map, format::told, render, Follow, Pages};
use crate::{
    BankAccount, BankReconciliation, Figures, ItemKind, PostLine, ReconciliationError,
    ReconciliationStatus, FINALISE_RECONCILIATIONS, MANAGE_MATCHES, MANAGE_RECONCILIATIONS,
    POST_STATEMENT_LINES,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{Format, Precision};
use bilibi_finance_ledger::{MinorUnits, POST_ENTRIES};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Cell,
    EmptyState, Fact, Facts, Field, FlashMap, FlexRow, FlexTable, Header, Html, IconButton,
    IconKind, Instant, ListColumns, RowMode, StatusKind, TableEmpty,
};
mod elsewhere;
use elsewhere::{PAYMENTS, RECEIPTS};

pub(super) struct Detail {
    pages: Pages,
    id: i64,
    report: Option<BankReconciliation>,
    account: Option<BankAccount>,
    refusal: Option<String>,
    can_finalise: bool,
    can_abandon: bool,
    can_post: bool,
    can_match: bool,
    can_receive: bool,
    can_pay: bool,
    abandoning: bool,
    reason: String,
    /// The Finalise dialog, open while statement lines are not in the books.
    finalising: bool,
    finalise_reason: String,
    finalise_refusal: Option<String>,
    /// A statement line and the entry another page posted for it, as the
    /// address the person came back to named them; matched once on arrival.
    returned: Option<(i64, i64)>,
    items: ListColumns,
    events: ListColumns,
    flash: Option<(StatusKind, String)>,
    follow: Follow,
}

#[derive(Template)]
#[template(path = "reconciliation.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    back: Html,
    refusal: Html,
    record: Option<RecordView>,
}
struct RecordView {
    id: i64,
    header: Html,
    figures: Html,
    controls: Html,
    items: Option<(Option<usize>, Html)>,
    events: Html,
    abandon: Option<Field>,
    finalise: Option<FinaliseView>,
}
struct FinaliseView {
    title: String,
    description: String,
    refusal: Html,
    reason: Field,
}

#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums">{{ text }}</span>{% if let Some(badge) = badge %} {{ badge|safe }}{% endif %}"#,
    ext = "html"
)]
struct Figure {
    text: String,
    badge: Option<Html>,
}

#[derive(Template)]
#[template(
    source = r#"{% if let Some(forbidden) = forbidden %}<div class="mt-4">{{ forbidden|safe }}</div>{% endif %}"#,
    ext = "html"
)]
struct Controls {
    forbidden: Option<Html>,
}

#[derive(Template)]
#[template(
    source = r#"{{ text }}{% if let Some(reference) = reference %}<span class="block text-xs text-ink-muted">{{ reference }}</span>{% endif %}"#,
    ext = "html"
)]
struct Described<'a> {
    text: &'a str,
    reference: Option<&'a str>,
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::StatementLine => "On the statement, not in the books",
        ItemKind::Entry => "In the books, not on the statement",
        ItemKind::Timing => "Matched, cleared after the statement date",
        ItemKind::OpeningItem => "Opening item not yet cleared",
    }
}

/// A reconciliation's status as a page names it.
pub(super) fn status_label(status: ReconciliationStatus) -> &'static str {
    match status {
        ReconciliationStatus::Open => "Open",
        ReconciliationStatus::Finalised => "Finalised",
        ReconciliationStatus::Abandoned => "Abandoned",
    }
}

fn event_label(action: &str) -> &str {
    match action {
        "opened" => "Opened",
        "finalised" => "Finalised",
        "abandoned" => "Abandoned",
        other => other,
    }
}

/// "1 statement line not in the books", "4 statement lines not in the books".
fn not_in_books(count: usize) -> String {
    format!(
        "{count} statement line{} not in the books",
        if count == 1 { "" } else { "s" }
    )
}

fn badge(kind: BadgeKind, text: &str) -> Result<Html> {
    render(Badge {
        kind,
        dot: false,
        class: "",
        body: Html::text(text),
    })
}

fn columns(id: &str, values: &[(&str, &str)]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        values
            .iter()
            .map(|(key, label)| {
                let column = BuiltinColumn::new(key, label);
                if *key == "amount" {
                    column.right()
                } else {
                    column
                }
            })
            .collect(),
    )
    .map_err(|_| Error::Internal)
}

impl Detail {
    pub(super) fn new(pages: &Pages, id: i64) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            id,
            report: None,
            account: None,
            refusal: None,
            can_finalise: false,
            can_abandon: false,
            can_post: false,
            can_match: false,
            can_receive: false,
            can_pay: false,
            abandoning: false,
            reason: String::new(),
            finalising: false,
            finalise_reason: String::new(),
            finalise_refusal: None,
            returned: None,
            items: columns(
                "reconciliation-items-table",
                &[
                    ("kind", "Kind"),
                    ("date", "Date"),
                    ("description", "Description"),
                    ("amount", "Amount"),
                ],
            )?,
            events: columns(
                "reconciliation-events-table",
                &[
                    ("event", "Event"),
                    ("by", "By"),
                    ("when", "When"),
                    ("reason", "Reason"),
                ],
            )?,
            flash: None,
            follow: Follow::default(),
        })
    }
    pub(super) fn follow(mut self, follow: Follow) -> Self {
        self.follow = follow;
        self
    }
    /// Bilimbi `load`: the reconciliation and its bank account, or the
    /// refusal the page shows in their place.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let company = company(scope)?;
        let r = &self.pages.reconciliation;
        let read = match told(r.get_reconciliation(scope, company, self.id).await)? {
            Ok(report) => match told(
                r.get_bank_account(scope, company, report.bank_account_id)
                    .await,
            )? {
                Ok(account) => Ok((report, account)),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        match read {
            Ok((report, account)) => {
                self.report = Some(report);
                self.account = Some(account);
                self.refusal = None;
                self.can_finalise = self.pages.allowed(scope, FINALISE_RECONCILIATIONS).await?;
                self.can_abandon = self.pages.allowed(scope, MANAGE_RECONCILIATIONS).await?;
                self.can_post = self.pages.allowed(scope, POST_STATEMENT_LINES).await?
                    && self.pages.allowed(scope, POST_ENTRIES).await?;
                self.can_match = self.pages.allowed(scope, MANAGE_MATCHES).await?;
                self.can_receive = self.pages.allowed(scope, RECEIPTS.1).await?;
                self.can_pay = self.pages.allowed(scope, PAYMENTS.1).await?;
            }
            Err(e) => {
                self.report = None;
                self.account = None;
                self.refusal = Some(e);
            }
        }
        Ok(())
    }

    /// Bilimbi `authorize_event`: refused, the page says so and nothing else
    /// happens.
    async fn may(&mut self, scope: &Scope, capabilities: &[&str]) -> Result<bool> {
        for capability in capabilities {
            if !self.pages.allowed(scope, capability).await? {
                self.flash = Some((StatusKind::Error, super::DENIED.into()));
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Handles one event; whether the reconciliation is to be read again.
    async fn event(&mut self, scope: &Scope, v: &serde_json::Value) -> Result<bool> {
        let r = self.pages.reconciliation.clone();
        match event_type(v)? {
            "clear-flash" => {
                clear_flash(&mut self.flash, v);
                Ok(false)
            }
            grid @ ("items_grid" | "events_grid") => {
                let columns = if grid == "items_grid" {
                    &mut self.items
                } else {
                    &mut self.events
                };
                columns.handle(&params::string_values(v));
                Ok(false)
            }
            "finalising" => {
                self.finalising = true;
                self.finalise_reason.clear();
                self.finalise_refusal = None;
                self.flash = None;
                Ok(false)
            }
            "cancel_finalise" => {
                self.finalising = false;
                Ok(false)
            }
            "finalise_change" => {
                self.finalise_reason = v["finalise[reason]"].as_str().unwrap_or_default().into();
                Ok(false)
            }
            "finalise" => {
                if let Some(reason) = v["finalise[reason]"].as_str() {
                    self.finalise_reason = reason.into();
                }
                if !self.may(scope, &[FINALISE_RECONCILIATIONS]).await? {
                    return Ok(false);
                }
                let reason = self.finalising.then_some(self.finalise_reason.as_str());
                match r
                    .finalise_reconciliation(scope, company(scope)?, self.id, reason)
                    .await
                {
                    // Lines are missing from the books and no reason came with
                    // the request: ask for it, in the dialog.
                    Err(e @ ReconciliationError::LinesNotInBooks(_)) => {
                        self.finalise_refusal = self.finalising.then(|| e.to_string());
                        self.finalising = true;
                    }
                    result => match told(result)? {
                        Ok(done) => {
                            self.finalising = false;
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "Reconciliation {} finalised; its figures are now fixed.",
                                    done.reconciliation_number
                                ),
                            ));
                        }
                        Err(e) if self.finalising => self.finalise_refusal = Some(e),
                        Err(e) => self.flash = Some((StatusKind::Error, e)),
                    },
                }
                Ok(true)
            }
            kind @ ("post_charge" | "post_interest") => {
                if !self
                    .may(scope, &[POST_STATEMENT_LINES, POST_ENTRIES])
                    .await?
                {
                    return Ok(false);
                }
                let line = params::positive_id(v["line"].as_str()).unwrap_or(0);
                let post = PostLine {
                    kind: if kind == "post_charge" {
                        "charge"
                    } else {
                        "interest"
                    }
                    .into(),
                    description: None,
                };
                let result = r
                    .post_statement_line(scope, company(scope)?, line, &post)
                    .await;
                self.flash = Some(match told(result)? {
                    Ok(posted) => (
                        StatusKind::Success,
                        format!("Posted as entry {} and matched.", posted.entry.entry_number),
                    ),
                    Err(e) => (StatusKind::Error, e),
                });
                Ok(true)
            }
            "abandoning" => {
                self.abandoning = true;
                self.reason.clear();
                Ok(false)
            }
            "cancel_abandon" => {
                self.abandoning = false;
                Ok(false)
            }
            "abandon_change" => {
                self.reason = v["abandon[reason]"].as_str().unwrap_or_default().into();
                Ok(false)
            }
            "abandon" => {
                self.reason = v["abandon[reason]"].as_str().unwrap_or_default().into();
                if !self.may(scope, &[MANAGE_RECONCILIATIONS]).await? {
                    return Ok(false);
                }
                let result = r
                    .abandon_reconciliation(scope, company(scope)?, self.id, Some(&self.reason))
                    .await;
                match told(result)? {
                    Ok(_) => {
                        self.flash =
                            Some((StatusKind::Info, "The reconciliation was abandoned.".into()));
                        self.abandoning = false;
                        Ok(true)
                    }
                    Err(e) => {
                        self.flash = Some((StatusKind::Error, e));
                        Ok(false)
                    }
                }
            }
            _ => Err(Error::Invalid),
        }
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let (back_href, back_title) = self.account.as_ref().map_or_else(
            || {
                (
                    "/finance/reconciliation".to_owned(),
                    "Bank accounts".to_owned(),
                )
            },
            |a| {
                (
                    format!("/finance/reconciliation/accounts/{}", a.id),
                    a.name.clone(),
                )
            },
        );
        let back = render(BackLink {
            href: &back_href,
            id: "reconciliation-back",
            title: &back_title,
            ..BackLink::default()
        })?;
        let mut view = View {
            flash: flash_map(&self.flash),
            back: if self.report.is_none() {
                back.clone()
            } else {
                Html::EMPTY
            },
            refusal: match &self.refusal {
                Some(reason) => render(
                    EmptyState::missing("Reconciliation unavailable", reason)
                        .id("reconciliation-refusal"),
                )?,
                None => Html::EMPTY,
            },
            record: None,
        };
        let (Some(report), Some(account)) = (&self.report, &self.account) else {
            return view.render().map_err(|_| Error::Internal);
        };
        let open = report.status == ReconciliationStatus::Open;
        let figures = report.figures.as_ref();
        let units = figures.map_or(2, |f| f.minor_units.get());

        let status = badge(
            match report.status {
                ReconciliationStatus::Open => BadgeKind::Warning,
                ReconciliationStatus::Finalised => BadgeKind::Success,
                ReconciliationStatus::Abandoned => BadgeKind::Neutral,
            },
            status_label(report.status),
        )?;

        let mut facts = vec![Fact {
            title: "Balance per statement",
            id: "figure-statement-balance",
            value: render(Figure {
                text: format!(
                    "{} {}",
                    report
                        .statement_balance
                        .display(MinorUnits::new(i64::from(units))),
                    figures.map_or("", |f| f.currency_code.as_str())
                ),
                badge: None,
            })?,
        }];
        if let Some(f) = figures {
            facts.push(Fact {
                title: "Balance per books",
                id: "figure-book-balance",
                value: render(Figure {
                    text: f.book_balance.display(MinorUnits::new(i64::from(units))),
                    badge: None,
                })?,
            });
            // A zero difference is not the whole story: statement lines the
            // books do not have explain it too, so their count is said here.
            let waiting = f.lines_not_in_books();
            facts.push(Fact {
                title: "Difference",
                id: "figure-difference",
                value: render(Figure {
                    text: f.difference.display(MinorUnits::new(i64::from(units))),
                    badge: Some(if !f.difference.is_zero() {
                        badge(BadgeKind::Warning, "Not explained yet")?
                    } else if waiting > 0 {
                        badge(BadgeKind::Warning, &not_in_books(waiting))?
                    } else {
                        badge(BadgeKind::Success, "Every statement line is in the books")?
                    }),
                })?,
            });
        }
        let figures_html = render(Facts {
            id: "reconciliation-facts",
            items: facts,
        })?;
        let unexplained = figures.is_none_or(|f| !f.difference.is_zero());
        let waiting = figures.map_or(0, Figures::lines_not_in_books);
        let controls = render(Controls {
            forbidden: if open && !self.can_finalise {
                Some(render(
                    EmptyState::forbidden("finalise reconciliations")
                        .id("reconciliation-finalise-forbidden"),
                )?)
            } else {
                None
            },
        })?;

        let items = match figures {
            Some(f) => {
                let mode = self.items.mode();
                let mut rows = vec![];
                for (index, item) in f.items.iter().enumerate() {
                    let date = item.item_date.to_string();
                    let mut row = FlexRow::new((index + 1).to_string())
                        .cell("kind", Cell::text(kind_label(item.kind)))
                        .cell(
                            "date",
                            Cell::date(&date, &bilibi_finance_ledger::dates::display_text(&date)),
                        )
                        .slot(
                            "date",
                            super::nowrap(&bilibi_finance_ledger::dates::display_text(&date))?,
                        )
                        .cell("description", Cell::text(&item.description))
                        .slot(
                            "description",
                            render(Described {
                                text: &item.description,
                                reference: item
                                    .reference
                                    .as_deref()
                                    .filter(|_| mode != RowMode::Compact),
                            })?,
                        )
                        .cell(
                            "amount",
                            Cell::text(item.amount.display(MinorUnits::new(i64::from(units)))),
                        );
                    row.dom_id = Some(format!("item-{}", index + 1));
                    if open && item.kind == ItemKind::StatementLine {
                        let line = item.statement_line_id.filter(|_| self.can_post);
                        let negative =
                            item.amount.amount().is_sign_negative() && !item.amount.is_zero();
                        // Local actions on the line: plain icon buttons.
                        let mut actions = Html::EMPTY;
                        for (line, event, icon, label) in [
                            (
                                line.filter(|_| negative),
                                "post_charge",
                                "receipt-percent",
                                "Post as bank charge",
                            ),
                            (
                                line.filter(|_| item.amount.is_positive()),
                                "post_interest",
                                "banknotes",
                                "Post as interest",
                            ),
                        ] {
                            if let Some(line) = line {
                                let label = format!("{label}: {}", item.description);
                                let kind = if event == "post_charge" {
                                    "charge"
                                } else {
                                    "interest"
                                };
                                actions.push(&render(IconButton {
                                    id: &format!("item-{line}-post-{kind}"),
                                    event,
                                    name: "line",
                                    value: &line.to_string(),
                                    ..IconButton::new(icon, &label)
                                })?);
                            }
                        }
                        actions.push(&self.elsewhere(account, item, index + 1, units)?);
                        row.actions = actions;
                    }
                    rows.push(row);
                }
                let mut table = FlexTable::new(&self.items.id, self.items.flex_columns(), rows);
                table.event = "items_grid";
                table.framed = false;
                table.actions = true;
                table.zoom = self.items.zoom;
                table.suggestions = self.items.flex_suggestions();
                table.add_query = &self.items.add_query;
                table.empty = Some(TableEmpty::State(EmptyState::missing(
                    "Everything is matched",
                    "Every statement line through the end date is matched, and so is every entry.",
                )));
                Some((Some(f.items.len()), render(table)?))
            }
            None => None,
        };

        let display = self.pages.display(scope).await?;
        let people = self
            .pages
            .people(
                scope,
                report
                    .events
                    .iter()
                    .flat_map(|e| [Some(e.recorded_by_user_id), e.recorded_by_impersonator_id]),
            )
            .await?;
        let mut rows = vec![];
        for e in &report.events {
            let time = display.render(e.recorded_at, Format::DateTime, Precision::Second);
            let mut instant = Instant::new(
                &time.iso,
                &time.text,
                &time.company,
                &time.utc,
                time.mode.as_str(),
            );
            instant.precision = "second";
            let by = people.who(Some(e.recorded_by_user_id), e.recorded_by_impersonator_id);
            let mut row = FlexRow::new(e.event_number.to_string())
                .cell("event", Cell::text(event_label(&e.action)))
                .cell("by", Cell::text(by))
                .cell("when", Cell::instant(instant))
                .cell("reason", Cell::text(e.reason.clone().unwrap_or_default()));
            row.dom_id = Some(format!("event-{}", e.event_number));
            rows.push(row);
        }
        let mut table = FlexTable::new(&self.events.id, self.events.flex_columns(), rows);
        table.event = "events_grid";
        table.framed = false;
        table.zoom = self.events.zoom;
        table.suggestions = self.events.flex_suggestions();
        table.add_query = &self.events.add_query;
        let events = render(table)?;

        let title = format!(
            "Reconciliation {} as of {}",
            report.reconciliation_number,
            bilibi_finance_ledger::dates::display(report.end_date)
        );
        // Back, this account's matching, the local Abandon, then Finalise.
        let mut actions = back;
        if open {
            actions.push(&render(Button {
                id: "reconciliation-matching",
                href: &format!("/finance/reconciliation/accounts/{}/matching", account.id),
                body: Html::text("Match statement lines"),
                ..Button::default()
            })?);
            if self.can_abandon {
                actions.push(&render(IconButton {
                    id: "reconciliation-abandon",
                    kind: IconKind::Danger,
                    event: "abandoning",
                    ..IconButton::new("x-circle", "Abandon this reconciliation")
                })?);
            }
            if self.can_finalise {
                actions.push(&render(Button {
                    id: "reconciliation-finalise",
                    button_type: "button",
                    variant: ButtonVariant::Primary,
                    // Lines not in the books: say so and ask why first.
                    event: if waiting > 0 {
                        "finalising"
                    } else {
                        "finalise"
                    },
                    disabled: unexplained,
                    working: "Finalising…",
                    body: Html::text("Finalise"),
                    ..Button::default()
                })?);
            }
        }
        view.record = Some(RecordView {
            id: report.id,
            header: render(Header {
                title: Html::text(&title),
                subtitle: if open && unexplained {
                    "Finalise once the difference is explained: match or post what is left."
                } else {
                    ""
                },
                title_actions: status,
                actions,
            })?,
            figures: figures_html,
            controls,
            items,
            events,
            abandon: self.abandoning.then(|| {
                Field::in_form("abandon", "reason")
                    .id("abandon-reason")
                    .label("Reason")
                    .value(&self.reason)
                    .attr("maxlength", "500")
                    .required()
            }),
            finalise: if self.finalising && open && waiting > 0 {
                Some(FinaliseView {
                    title: format!("Finalise with {}?", not_in_books(waiting)),
                    description: format!(
                        "{} on the bank statement but not in the books. You can still finalise. Say why, so whoever reads this later knows.",
                        if waiting == 1 {
                            "1 line is".to_owned()
                        } else {
                            format!("{waiting} lines are")
                        }
                    ),
                    refusal: match &self.finalise_refusal {
                        Some(why) => render(Alert {
                            kind: StatusKind::Error,
                            id: "finalise-refusal",
                            body: Html::text(why),
                            ..Alert::default()
                        })?,
                        None => Html::EMPTY,
                    },
                    reason: Field::in_form("finalise", "reason")
                        .id("finalise-reason")
                        .label("Reason")
                        .value(&self.finalise_reason)
                        .attr("maxlength", "500")
                        .required(),
                })
            } else {
                None
            },
        });
        view.render().map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Detail {
    fn subscriptions(&mut self) -> Vec<Subscription> {
        self.follow.subscriptions()
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match request.input {
                // The mount's first read, or a posting notice: an open
                // reconciliation follows the books; a finalised one cannot
                // change.
                LiveInput::Refresh => {
                    let posted = self.follow.posted();
                    !posted
                        || self
                            .report
                            .as_ref()
                            .is_none_or(|r| r.status == ReconciliationStatus::Open)
                }
                LiveInput::Params(_) => true,
                LiveInput::Event(v) => self.event(request.scope, v).await?,
            };
            if reload {
                self.load(request.scope).await?;
            }
            if let Some((line, entry)) = self.returned.take() {
                self.match_returned(request.scope, line, entry).await?;
                self.load(request.scope).await?;
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                ..Default::default()
            })
        })
    }
}
