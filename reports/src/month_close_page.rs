//! The month-end checklist page: what is left before a month of the current
//! company closes, each check with a link to the place that clears it, and
//! the close itself. It reads `finance.reports.month_close.get`, the contract
//! an agent calls, and closes through the ledger's
//! `finance.ledger.period.close`; the checks warn and never block.
//!
//! There is one page per month, reached from Fiscal years (the list of
//! months) and from the month before it once that one is closed, so the page
//! has no menu item. Like every Finance list its links act on the session's
//! current company, so that is the company it reads.
use crate::month_close::CheckState;
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Db, Error, Result};
use bilibi_finance_ledger::MANAGE_PERIODS;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell,
    Choice, ConfirmDialog, EmptyState, Field, FlashGroup, FlexRow, FlexTable, Form, Header, Html,
    InputKind, ListColumns, Modal, PageVariant, SectionHeading, StatusKind, TableEmpty, NO_FLASH,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub(crate) const PAGES: &str = "finance/reports.month-close-pages";
const READ: &str = "finance.reports.month_close.get";
const CLOSE: &str = "finance.ledger.period.close";
const YEARS: &str = "/finance/ledger/fiscal-years";
const DENIED: &str = "You do not have permission to do that.";
const UNCHANGED: &str = "Nothing was changed. Reload the page and try again.";

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<PageContracts>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
}

fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

/// What a check found: its count as a badge, then the sentence.
#[derive(Template)]
#[template(
    source = r#"<span class="inline-flex flex-wrap items-baseline gap-x-2 gap-y-1">{{ badge|safe }}<span>{{ found }}</span></span>"#,
    ext = "html"
)]
struct Found<'a> {
    badge: Html,
    found: &'a str,
}

/// What is still left in a run of months, inside the "Close several months"
/// dialog.
#[derive(Template)]
#[template(
    source = r#"<div id="close-months-left" class="mt-3 text-sm"><p class="font-medium">{{ heading }}</p>{% if !lines.is_empty() %}<ul class="mt-1 list-disc space-y-1 pl-5">{% for line in lines %}<li>{{ line }}</li>{% endfor %}</ul>{% endif %}</div>"#,
    ext = "html"
)]
struct Left<'a> {
    heading: &'a str,
    lines: Vec<String>,
}

/// Dialog actions sit at the end of the row, Cancel then the one verb.
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct DialogActions {
    buttons: Html,
}

/// One check, as the contract answers it.
#[derive(Deserialize)]
struct Check {
    id: String,
    name: String,
    state: CheckState,
    badge: String,
    found: String,
    path: String,
    action: String,
}

#[derive(Clone, Deserialize)]
struct Month {
    period_id: i64,
    name: String,
    month: String,
    end_date: String,
    status: String,
}

#[derive(Deserialize)]
struct CloseStep {
    allowed: bool,
    blocked: String,
    blocked_path: String,
    blocked_action: String,
}

/// The checklist, as the contract answers it.
#[derive(Deserialize)]
struct Checklist {
    fiscal_year: String,
    period: Month,
    name: String,
    checks: Vec<Check>,
    open_checks: Option<usize>,
    close: CloseStep,
    later: Vec<Month>,
    next: Option<Month>,
}

/// A reviewed close waiting for the person's answer.
struct Asked {
    input: Value,
    name: String,
    month: String,
    left: Option<usize>,
    /// The month to show once this one is closed.
    next: Option<i64>,
}

/// The "Close several months" dialog: the last month to close, and what the
/// ledger said when it refused.
struct Several {
    through: i64,
    notice: String,
}

/// Why a read or a write did not happen, in a sentence.
enum Refused {
    Denied,
    Said(String),
}

impl Refused {
    fn text(self) -> String {
        match self {
            Self::Denied => DENIED.to_owned(),
            Self::Said(text) => text,
        }
    }
}

/// A contract's own words for a refusal, never its code. A signed-out
/// browser or a missing sign-in code ends the page instead.
fn refused(error: ContractError) -> Result<Refused> {
    Ok(match error {
        ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. })) => {
            return Err(error)
        }
        ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => Refused::Denied,
        ContractError::InvalidInput(fields) => {
            Refused::Said(sentences(fields.into_values().flatten()))
        }
        ContractError::PlanRejected(results) => rejected(&results[0]),
        ContractError::Backend(Error::Conflict) => Refused::Said(
            "The month changed while you were looking at it. Reload the page and try again."
                .to_owned(),
        ),
        _ => Refused::Said(UNCHANGED.to_owned()),
    })
}

/// What a refused review says.
fn rejected(result: &Value) -> Refused {
    if result["error"]["code"].as_str() == Some("missing_capability") {
        return Refused::Denied;
    }
    let messages: Vec<String> = result["error"]["fields"]
        .as_object()
        .into_iter()
        .flat_map(|fields| fields.values())
        .flat_map(|messages| messages.as_array().into_iter().flatten())
        .filter_map(|message| message.as_str().map(str::to_owned))
        .collect();
    if messages.is_empty() {
        Refused::Said(UNCHANGED.to_owned())
    } else {
        Refused::Said(sentences(messages.into_iter()))
    }
}

fn sentences(messages: impl Iterator<Item = String>) -> String {
    messages
        .map(|message| {
            if message.ends_with(['.', '!', '?']) {
                message
            } else {
                format!("{message}.")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "3 things still need a look.", or nothing when all is done or unknown.
fn left_words(left: Option<usize>) -> String {
    match left {
        Some(0) | None => String::new(),
        Some(1) => "1 thing still needs a look. ".to_owned(),
        Some(count) => format!("{count} things still need a look. "),
    }
}

struct MonthClose {
    pages: Pages,
    /// The month the address names; `None` is the earliest open month.
    period: Option<i64>,
    flash: Option<(StatusKind, String)>,
    asked: Option<Asked>,
    several: Option<Several>,
    columns: Option<ListColumns>,
}

impl MonthClose {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Self {
        Self {
            pages,
            period: query.get("period").and_then(|id| id.parse().ok()),
            flash: None,
            asked: None,
            several: None,
            columns: None,
        }
    }

    async fn read(
        &self,
        scope: &Scope,
        company: i64,
        through: Option<i64>,
    ) -> Result<std::result::Result<Checklist, Refused>> {
        let mut input = json!({"company_id": company});
        if let Some(period) = self.period {
            input["period_id"] = json!(period);
        }
        if let Some(through) = through {
            input["through_period_id"] = json!(through);
        }
        match self.pages.contracts.read(scope, READ, &input).await {
            Ok(value) => Ok(serde_json::from_value(value["result"].clone())
                .map_err(|_| Refused::Said("This could not be shown. Reload the page.".into()))),
            Err(error) => Ok(Err(refused(error)?)),
        }
    }

    fn table(&mut self, checks: &[Check]) -> Result<Html> {
        if self.columns.is_none() {
            self.columns = Some(
                ListColumns::mount(
                    "month-close-checks",
                    vec![
                        BuiltinColumn::new("check", "Check"),
                        BuiltinColumn::new("found", "What Bilibi found"),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        let columns = self.columns.as_mut().ok_or(Error::Internal)?;
        columns.load(checks, |check| check.id.clone());
        let mut rows = Vec::new();
        for check in checks {
            let kind = match check.state {
                CheckState::Open => BadgeKind::Warning,
                CheckState::Clear => BadgeKind::Success,
                _ => BadgeKind::Neutral,
            };
            let mut row = FlexRow::new(check.id.clone())
                .cell("check", Cell::text(&check.name))
                .slot(
                    "found",
                    render(Found {
                        badge: render(Badge::status(kind, &check.badge))?,
                        found: &check.found,
                    })?,
                );
            // Every check with something to do names the place to do it.
            if !check.path.is_empty() {
                row.actions = render(Button {
                    id: &format!("month-close-{}-go", check.id),
                    href: &check.path,
                    body: Html::text(&check.action),
                    ..Button::default()
                })?;
            }
            row.dom_id = Some(format!("month-close-{}", check.id));
            rows.push(row);
        }
        let mut table = FlexTable::new("month-close-table", columns.flex_columns(), rows);
        table.caption = "Checks";
        table.framed = false;
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.actions = true;
        table.empty = Some(TableEmpty::Content(Html::text("Nothing to check.")));
        render(table)
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        let mut actions = render(BackLink {
            href: YEARS,
            id: "month-close-back",
            title: "Back to fiscal years",
            ..BackLink::default()
        })?;
        let mut title = "Close month".to_owned();
        let mut subtitle = String::new();
        let mut dialogs = Html::EMPTY;
        let mut shown = None;
        match scope.current_company() {
            None => body.push(&render(
                EmptyState::missing(
                    "No current company",
                    "Choose a current company in the top bar to see its month-end checklist.",
                )
                .id("month-close-no-company"),
            )?),
            Some(company) => {
                let client = self.pages.db.get().await?;
                let company_name = self
                    .pages
                    .parties
                    .organisation_name(&**client, scope, company)
                    .await?
                    .map(|c| c.name)
                    .unwrap_or_else(|| "Company unavailable".into());
                let may_close = bilibi_authz::effective_capabilities(&**client, scope)
                    .await?
                    .contains(MANAGE_PERIODS);
                drop(client);
                subtitle = company_name.clone();
                match self.read(scope, company.get(), None).await? {
                    Ok(list) => {
                        shown = Some(list.period.period_id);
                        title = format!("Close {}", list.period.name);
                        subtitle = format!("{company_name} · fiscal year {}", list.fiscal_year);
                        let can_close = list.close.allowed && may_close;
                        if !list.close.allowed {
                            if !list.close.blocked_path.is_empty() {
                                actions.push(&render(Button {
                                    id: "month-close-first",
                                    href: &list.close.blocked_path,
                                    variant: ButtonVariant::Primary,
                                    body: Html::text(&list.close.blocked_action),
                                    ..Button::default()
                                })?);
                            }
                            body.push(&render(Alert {
                                id: "month-close-blocked",
                                kind: StatusKind::Info,
                                class: "mb-3",
                                body: Html::text(&list.close.blocked),
                            })?);
                        }
                        if can_close && !list.later.is_empty() {
                            actions.push(&render(Button {
                                id: "month-close-several",
                                button_type: "button",
                                event: "close_several",
                                body: Html::text("Close several months"),
                                ..Button::default()
                            })?);
                        }
                        if can_close {
                            actions.push(&render(Button {
                                id: "month-close-close",
                                button_type: "button",
                                event: "close",
                                variant: ButtonVariant::Primary,
                                working: "Checking…",
                                body: Html::text(&format!("Close {}", list.period.month)),
                                ..Button::default()
                            })?);
                        }
                        let summary = match list.open_checks {
                            Some(0) => format!("Nothing is left for {}.", list.name),
                            Some(count) => format!(
                                "{}{}",
                                left_words(Some(count)),
                                if can_close {
                                    "You can close the month anyway."
                                } else {
                                    ""
                                }
                            ),
                            None => {
                                "Some checks could not be read, so no total is shown.".to_owned()
                            }
                        };
                        let mut card = render(SectionHeading {
                            id: "month-close-heading",
                            title: "Before you close",
                            description: summary.trim(),
                            class: "p-5 pb-0",
                            ..SectionHeading::default()
                        })?;
                        card.push(&self.table(&list.checks)?);
                        body.push(&render(Card {
                            id: "month-close-card",
                            inner_class: "p-0",
                            role: "region",
                            labelledby: "month-close-heading",
                            body: card,
                            ..Card::default()
                        })?);
                        if let Some(asked) = &self.asked {
                            dialogs.push(&render(ConfirmDialog {
                                id: "month-close-confirm",
                                consequence: &format!("Close {}?", asked.name),
                                detail: &format!(
                                    "{}No one can add or change entries dated in {} until someone reopens it.",
                                    left_words(asked.left),
                                    asked.month
                                ),
                                confirm: &format!("Close {}", asked.month),
                                working: "Closing…",
                                confirm_event: "do_close",
                                cancel_event: "cancel",
                            })?);
                        }
                        if self.several.is_some() && can_close && !list.later.is_empty() {
                            dialogs.push(
                                &self
                                    .several_dialog(scope, company.get(), &company_name, &list)
                                    .await?,
                            );
                        } else {
                            self.several = None;
                        }
                    }
                    Err(Refused::Denied) => body.push(&render(
                        EmptyState::forbidden("see the month-end checklist")
                            .id("month-close-forbidden"),
                    )?),
                    Err(Refused::Said(text)) => {
                        let mut empty = EmptyState::missing("This month could not be shown", text)
                            .id("month-close-missing");
                        empty.action = render(Button {
                            id: "month-close-years",
                            href: YEARS,
                            body: Html::text("Go to Fiscal years"),
                            ..Button::default()
                        })?;
                        body.push(&render(empty)?);
                    }
                }
            }
        }
        let mut page = render(Header {
            title: Html::text(&title),
            subtitle: &subtitle,
            title_actions: Html::EMPTY,
            actions,
        })?;
        page.push(&body);
        page.push(&dialogs);
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = text,
                _ => flash.error = text,
            }
        }
        let mut html = render(FlashGroup {
            id: "flash-group",
            flash,
        })?;
        html.push(&render(bilibi_ui::Page {
            id: "month-close",
            variant: PageVariant::Detail,
            body: page,
            ..bilibi_ui::Page::default()
        })?);
        // The address names the month shown, so a reload stays on it.
        let query = shown
            .filter(|_| self.period.is_some())
            .map(|id| HashMap::from([("period".to_owned(), id.to_string())]))
            .unwrap_or_default();
        Ok(LiveBody {
            html: html.to_string(),
            query,
            ..LiveBody::default()
        })
    }

    /// The dialog that closes this month and the ones after it together: the
    /// last month to close, and what the checks find across the run.
    async fn several_dialog(
        &mut self,
        scope: &Scope,
        company: i64,
        company_name: &str,
        list: &Checklist,
    ) -> Result<Html> {
        let Some(several) = &mut self.several else {
            return Ok(Html::EMPTY);
        };
        if !list.later.iter().any(|m| m.period_id == several.through) {
            several.through = list.later[0].period_id;
        }
        let (through, notice) = (several.through, several.notice.clone());
        let months = 1 + list
            .later
            .iter()
            .position(|m| m.period_id == through)
            .map_or(0, |index| index + 1);
        let mut body = Html::EMPTY;
        if !notice.is_empty() {
            body.push(&render(Alert {
                id: "close-months-notice",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&notice),
            })?);
        }
        body.push(&render(
            Field::in_form("several", "through")
                .id("close-months-through")
                .label("Last month to close")
                .kind(InputKind::Select)
                .value(&through.to_string())
                .options(
                    list.later
                        .iter()
                        .map(|m| Choice::new(&m.name, m.period_id.to_string()))
                        .collect(),
                ),
        )?);
        let run = self.read(scope, company, Some(through)).await?;
        let run_name = match &run {
            Ok(run) => run.name.clone(),
            Err(_) => list.name.clone(),
        };
        body.push(&match run {
            Ok(run) => {
                let lines: Vec<String> = run
                    .checks
                    .iter()
                    .filter(|check| check.state == CheckState::Open)
                    .map(|check| format!("{}: {}", check.name, check.found))
                    .collect();
                let heading = if lines.is_empty() {
                    format!("Nothing is left for {}.", run.name)
                } else {
                    format!("Still left for {}:", run.name)
                };
                render(Left {
                    heading: &heading,
                    lines,
                })?
            }
            Err(refused) => render(Alert {
                id: "close-months-unread",
                kind: StatusKind::Warning,
                class: "mt-3",
                body: Html::text(&refused.text()),
            })?,
        });
        let mut buttons = render(Button {
            id: "close-months-cancel",
            button_type: "button",
            event: "cancel",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "close-months-submit",
            button_type: "button",
            event: "do_close_several",
            variant: ButtonVariant::Primary,
            working: "Closing…",
            body: Html::text(&format!("Close {months} months")),
            ..Button::default()
        })?);
        render(Modal {
            id: "close-months-modal",
            title: "Close several months",
            cancel_event: "cancel",
            description: &format!(
                "{company_name}. {run_name} close together. No one can add or change entries dated in them until someone reopens them."
            ),
            body: render(Form {
                id: "close-months-form",
                event: "several_change",
                change: true,
                class: "mt-4",
                body,
                actions: render(DialogActions { buttons })?,
            })?,
            ..Modal::default()
        })
    }

    fn call<'a>(&self, r: &'a LiveRequest<'a, Scope>, event: &'a str) -> Call<'a> {
        Call {
            scope: r.scope,
            credential: r.credential,
            service: PAGES,
            event,
            contract: CLOSE,
        }
    }

    /// "Close October": ask the ledger what closing would do, then confirm.
    async fn ask(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(company) = r.scope.current_company() else {
            return Ok(());
        };
        let list = match self.read(r.scope, company.get(), None).await? {
            Ok(list) => list,
            Err(refused) => {
                self.flash = Some((StatusKind::Error, refused.text()));
                return Ok(());
            }
        };
        let input = json!({"company_id": company.get(), "period_id": list.period.period_id});
        let call = self.call(r, "close");
        match self.pages.contracts.preview(&call, input.clone()).await {
            Ok(review) if review["results"][0]["valid"].as_bool() == Some(true) => {
                self.asked = Some(Asked {
                    input,
                    name: list.period.name,
                    month: list.period.month,
                    left: list.open_checks,
                    next: list
                        .next
                        .filter(|m| m.status == "open")
                        .map(|m| m.period_id),
                });
            }
            Ok(review) => {
                self.flash = Some((StatusKind::Error, rejected(&review["results"][0]).text()))
            }
            Err(error) => self.flash = Some((StatusKind::Error, refused(error)?.text())),
        }
        Ok(())
    }

    async fn close(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(asked) = self.asked.take() else {
            return Ok(());
        };
        let call = self.call(r, "do_close");
        match self.pages.contracts.commit(&call, asked.input).await {
            Ok(_) => {
                // A finished step lands on the next one: the month after.
                if let Some(next) = asked.next {
                    self.period = Some(next);
                }
                self.flash = Some((StatusKind::Success, format!("{} is closed.", asked.name)));
            }
            Err(error) => self.flash = Some((StatusKind::Error, refused(error)?.text())),
        }
        Ok(())
    }

    async fn close_several(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let (Some(company), Some(several)) = (r.scope.current_company(), &self.several) else {
            return Ok(());
        };
        let through = several.through;
        let name = match self.read(r.scope, company.get(), Some(through)).await? {
            Ok(run) => run.name,
            Err(refused) => {
                if let Some(several) = &mut self.several {
                    several.notice = refused.text();
                }
                return Ok(());
            }
        };
        let input =
            json!({"company_id": company.get(), "period_id": through, "with_earlier": true});
        let call = self.call(r, "do_close_several");
        let notice = match self.pages.contracts.submit(&call, input).await {
            Ok(_) => {
                self.several = None;
                // On to the earliest month still open.
                self.period = None;
                self.flash = Some((StatusKind::Success, format!("{name} are closed.")));
                return Ok(());
            }
            Err(error) => refused(error)?.text(),
        };
        if let Some(several) = &mut self.several {
            several.notice = notice;
        }
        Ok(())
    }

    /// Open the dialog on the latest month that has already ended, else the
    /// month after this one.
    async fn open_several(&mut self, scope: &Scope) -> Result<()> {
        let Some(company) = scope.current_company() else {
            return Ok(());
        };
        let Ok(list) = self.read(scope, company.get(), None).await? else {
            return Ok(());
        };
        let today = jiff::Zoned::now().date().to_string();
        let through = list
            .later
            .iter()
            .rev()
            .find(|m| m.end_date < today)
            .or(list.later.first())
            .map(|m| m.period_id);
        self.several = through.map(|through| Several {
            through,
            notice: String::new(),
        });
        Ok(())
    }
}

impl LiveState<Scope, Error> for MonthClose {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Params(query) = r.input {
                self.period = query.get("period").and_then(|id| id.parse().ok());
                self.asked = None;
                self.several = None;
            }
            if let LiveInput::Event(value) = r.input {
                let event = value.get("type").and_then(Value::as_str);
                // A new action starts clean: an old refusal never shows in
                // the next dialog.
                if !matches!(event, Some("grid" | "clear-flash")) {
                    self.flash = None;
                }
                match event {
                    Some("close") => self.ask(&r).await?,
                    Some("do_close") => self.close(&r).await?,
                    Some("close_several") => self.open_several(r.scope).await?,
                    Some("several_change") => {
                        if let Some(several) = &mut self.several {
                            if let Some(through) = params::form_values(value, "several")
                                .get("through")
                                .and_then(|id| id.parse().ok())
                            {
                                several.through = through;
                            }
                            several.notice.clear();
                        }
                    }
                    Some("do_close_several") => self.close_several(&r).await?,
                    Some("cancel") => {
                        self.asked = None;
                        self.several = None;
                    }
                    Some("grid") => {
                        if let Some(columns) = &mut self.columns {
                            columns.handle(&params::string_values(value));
                        }
                    }
                    Some("clear-flash") => self.flash = None,
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Close month")
    }

    fn render<'a>(&'a self, r: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = r.scope.ok_or(Error::Unauthorized)?;
            let mut state = MonthClose::new(self.clone(), r.query);
            let body = state.view(scope).await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, r: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(Box::new(MonthClose::new(self.clone(), r.query))
                as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
