//! Bilimbi Reports.Web.{ReportsLive,AccountActivityLive}, Finance 4abfaf82.
//! Page state owns navigation and posting invalidation; figures remain in Reports.
mod figures;
mod sections;

use crate::{ActivityQuery, Reports, ReportsError};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{
    filter::Outcome, Clock, DisplayContext, DisplayPolicy, Period, PeriodFilter,
};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        self, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest, Subscription,
    },
    params, Field, FlashMap, Html, InputKind, ListColumns, ListSpec, ListState, StatusKind, Tab,
    Tabs, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{sync::broadcast, task::JoinHandle};

const VIEWS: [(&str, &str); 4] = [
    ("overview", "Overview"),
    ("profit-and-loss", "Profit and loss"),
    ("balance-sheet", "Balance sheet"),
    ("cash-position", "Cash position"),
];
const PATH: &str = "/finance/reports";

#[derive(Clone)]
pub(crate) struct Pages {
    pub reports: Arc<Reports>,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct State {
    pages: Pages,
    tenant: i64,
    activity: bool,
    account_id: Option<i64>,
    query: HashMap<String, String>,
    list: ListState,
    filter: PeriodFilter,
    clock: Clock,
    display: DisplayContext,
    browser_zone: Option<String>,
    timezone_request: String,
    loaded: Option<crate::Result<figures::Report>>,
    columns: HashMap<&'static str, ListColumns>,
    posting: Option<crate::PostingSubscription>,
    bridge: Option<JoinHandle<()>>,
    dirty: Arc<AtomicBool>,
    notices_unavailable: bool,
    framed: bool,
    auditor_pack: Option<String>,
    flash: Option<(StatusKind, String)>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(task) = self.bridge.take() {
            task.abort();
        }
    }
}

impl State {
    async fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        let activity = match request.operation {
            "reports" => false,
            "activity" => true,
            _ => return Err(Error::NotFound),
        };
        let display = pages.display(scope).await?;
        let clock = display.clock(None, Timestamp::now());
        let spec = ListSpec {
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        let mut query = request.query.clone();
        if !activity
            && !["period", "from", "to"]
                .iter()
                .any(|key| query.contains_key(*key))
        {
            let book = pages
                .contracts
                .read(
                    scope,
                    "finance.ledger.book.get",
                    &serde_json::json!({"company_id":company(scope)?}),
                )
                .await
                .map_err(|_| Error::Internal)?;
            if let Some(date) = book["result"]["latest_entry_date"]
                .as_str()
                .and_then(|s| s.parse::<jiff::civil::Date>().ok())
            {
                query.insert("period".into(), "custom".into());
                query.insert("from".into(), date.first_of_month().to_string());
                query.insert("to".into(), date.last_of_month().to_string());
            }
        }
        let mut state = Self {
            pages,
            tenant: scope.tenant(),
            activity,
            account_id: params::positive_id(request.path.get("account_id").map(String::as_str)),
            query: query.clone(),
            list: ListState::parse(request.query, spec).map_err(|_| Error::Internal)?,
            filter: PeriodFilter::new("report-period", period(&query, &clock), &clock),
            clock,
            display,
            browser_zone: None,
            timezone_request: Timestamp::now().to_string(),
            loaded: None,
            columns: HashMap::new(),
            posting: None,
            bridge: None,
            dirty: Arc::new(AtomicBool::new(false)),
            notices_unavailable: false,
            framed: request.framed,
            auditor_pack: None,
            flash: None,
        };
        state.canonicalize();
        Ok(state)
    }

    fn view(&self) -> &'static str {
        VIEWS
            .iter()
            .find(|(id, _)| Some(*id) == self.query.get("view").map(String::as_str))
            .map_or("overview", |(id, _)| *id)
    }
    fn pinned(&self) -> Option<i64> {
        params::positive_id(self.query.get("through").map(String::as_str))
    }
    fn compare(&self) -> bool {
        self.query.get("compare").is_none_or(|s| s != "none")
    }

    fn canonicalize(&mut self) {
        let mut query = HashMap::new();
        if self.activity {
            for key in ["from", "to", "after_date"] {
                if let Some(date) = self
                    .query
                    .get(key)
                    .and_then(|v| v.parse::<jiff::civil::Date>().ok())
                {
                    query.insert(key.into(), date.to_string());
                }
            }
            if let Some(id) = params::positive_id(self.query.get("after_id").map(String::as_str)) {
                query.insert("after_id".into(), id.to_string());
            }
            if self.query.get("closing").is_some_and(|v| v == "excluded") {
                query.insert("closing".into(), "excluded".into());
            }
        } else {
            query.extend(self.filter.period.params().map(|(k, v)| (k.into(), v)));
            query.insert("view".into(), self.view().into());
            if !self.compare() {
                query.insert("compare".into(), "none".into());
            }
        }
        if let Some(pin) = self.pinned() {
            query.insert("through".into(), pin.to_string());
        }
        self.query = query;
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        // Clear before reading: a commit during the read schedules another pass.
        self.dirty.store(false, Ordering::SeqCst);
        let company = company(scope)?;
        let through = self.pinned();
        let reports = &self.pages.reports;
        self.loaded = Some(if self.activity {
            let date = |k: &str| self.query.get(k).and_then(|v| v.parse().ok());
            let query = ActivityQuery {
                from: date("from"),
                to: date("to"),
                through_entry_number: through,
                exclude_closing: self.query.get("closing").is_some_and(|s| s == "excluded"),
                limit: Some(100),
                after: date("after_date").zip(params::positive_id(
                    self.query.get("after_id").map(String::as_str),
                )),
            };
            match self.account_id {
                Some(id) => reports
                    .account_activity(scope, company, id, &query)
                    .await
                    .map(figures::Report::Activity),
                None => Err(ReportsError::AccountNotFound),
            }
        } else {
            let current = crate::Period {
                from: self.filter.period.from,
                to: self.filter.period.to,
            };
            if self.view() == "overview" {
                reports
                    .overview(scope, company, current, through)
                    .await
                    .map(figures::Report::Overview)
            } else {
                let periods = if self.compare() {
                    crate::previous_period(current).map(|previous| vec![current, previous])
                } else {
                    Ok(vec![current])
                };
                match periods {
                    Err(e) => Err(e),
                    Ok(periods) => {
                        let dates = periods.iter().map(|p| p.to).collect::<Vec<_>>();
                        match self.view() {
                            "profit-and-loss" => reports
                                .profit_and_loss(scope, company, &periods, through)
                                .await
                                .map(figures::Report::Profit),
                            "balance-sheet" => reports
                                .balance_sheet(scope, company, &dates, through)
                                .await
                                .map(figures::Report::Balance),
                            _ => reports
                                .cash_position(scope, company, &dates, through)
                                .await
                                .map(figures::Report::Cash),
                        }
                    }
                }
            }
        });
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let kind = value["type"].as_str().unwrap_or_default();
        if kind == "clear-flash" {
            clear_flash(&mut self.flash, value);
            return Ok(());
        }
        if let Some(id) = kind.strip_prefix("grid:") {
            if let Some(columns) = self.columns.get_mut(id) {
                columns.handle(&params::string_values(value));
            }
            return Ok(());
        }
        if self.activity {
            return Ok(());
        }
        if kind == "export_auditor_pack" {
            // A mounted control is never authority: every event asks again,
            // and a missing export grant gets Bilimbi's generic denial, not
            // the facade's auditor-access sentence about source modules.
            if !self.may_export(scope).await? {
                self.flash = Some((
                    StatusKind::Error,
                    "You do not have permission for that action.".into(),
                ));
                return Ok(());
            }
            self.reclock(scope).await?;
            let result = match company(scope) {
                Ok(company) => {
                    self.pages
                        .reports
                        .export_auditor_pack(scope, company, period(&self.query, &self.clock).to)
                        .await
                }
                Err(_) => Err(ReportsError::Unauthorized),
            };
            self.flash = Some(match result {
                Ok(artifact) => {
                    let message = if artifact.reference.kind == "ledger-pack" {
                        "Ledger pack generated. Payroll and other source records are excluded. Posted payroll journals are included."
                    } else {
                        "Auditor pack generated."
                    };
                    self.auditor_pack = Some(artifact.id);
                    (StatusKind::Success, message.into())
                }
                Err(error) => (
                    StatusKind::Error,
                    crate::auditor_pack::export_refusal(&error).into(),
                ),
            });
            return Ok(());
        }
        if kind == "browser_timezone" {
            self.browser_zone = value["timezone"].as_str().map(str::to_owned);
            self.timezone_request.clear();
        }
        // The control's events are `period_select:<id>` and the same for apply
        // and cancel. Refresh the clock before the filter reads "today".
        if period_event(kind) || matches!(kind, "compare" | "pin" | "unpin" | "browser_timezone") {
            self.reclock(scope).await?;
        }
        match self.filter.event(kind, value, &self.clock) {
            Some(Outcome::Applied(period)) => {
                self.auditor_pack = None;
                self.filter.set(period, &self.clock);
                self.canonicalize();
                return self.load(scope).await;
            }
            Some(Outcome::Update) => return Ok(()),
            None => {}
        }
        if matches!(kind, "compare" | "pin" | "unpin") {
            self.auditor_pack = None;
        }
        match kind {
            "compare" => {
                let compare = params::form_values(value, "report");
                if compare.get("compare").is_some_and(|v| v == "true") {
                    self.query.remove("compare");
                } else {
                    self.query.insert("compare".into(), "none".into());
                }
            }
            "pin" => {
                // Freeze what the clicked control displayed, even if a posting
                // refresh advanced server state while the click was in flight.
                if let Some(through) = params::positive_id(value["through"].as_str()) {
                    self.query.insert("through".into(), through.to_string());
                }
            }
            "unpin" => {
                self.query.remove("through");
            }
            "browser_timezone" => {}
            _ => return Ok(()),
        }
        self.filter
            .set(period(&self.query, &self.clock), &self.clock);
        self.canonicalize();
        self.load(scope).await
    }

    async fn reclock(&mut self, scope: &Scope) -> Result<()> {
        self.display = self.pages.display(scope).await?;
        self.clock = self
            .display
            .clock(self.browser_zone.as_deref(), Timestamp::now());
        Ok(())
    }

    fn title(&self) -> Option<String> {
        match &self.loaded {
            Some(Ok(figures::Report::Activity(a))) => {
                Some(format!("{} {}", a.account.code, a.account.name))
            }
            _ => None,
        }
    }

    fn render(&mut self) -> Result<String> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let report = self.loaded.as_ref().and_then(|v| v.as_ref().ok());
        before.push(figures::header(
            report,
            self.activity,
            self.pinned().is_some(),
            &self.query,
            self.auditor_pack.as_deref(),
        )?);
        if !self.activity {
            let addresses = VIEWS.map(|(view, _)| self.address(view));
            before.push(render(Tabs {
                id: "report-views",
                label: "Reports",
                class: "",
                tabs: VIEWS
                    .iter()
                    .zip(addresses.iter())
                    .map(|((view, label), url)| Tab::patch(url, label, self.view() == *view))
                    .collect(),
            })?);
            after.push(figures::notice()?);
            if self.notices_unavailable {
                after.push(figures::live_warning()?);
            }
        }
        if let Some(Err(error)) = &self.loaded {
            after.push(figures::refusal(error, self.activity)?);
        }
        if let Some(Ok(report)) = &self.loaded {
            after.extend(report.render(
                &mut self.columns,
                &self.list,
                &self.query,
                &self.display,
                self.framed,
            )?);
        }
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = text,
                StatusKind::Error => flash.error = text,
                StatusKind::Warning => flash.warning = text,
                StatusKind::Info => flash.info = text,
            }
        }
        PageView {
            flash,
            id: if self.activity {
                "account-activity"
            } else {
                "management-reports"
            },
            before,
            after,
            filter: if self.activity {
                None
            } else {
                Some(Html::render(&self.filter).map_err(|_| Error::Internal)?)
            },
            compare: (!self.activity && self.view() != "overview").then(|| {
                Field::in_form("report", "compare")
                    .id("report-compare-input")
                    .kind(InputKind::Checkbox)
                    .label("Compare with the previous period")
                    .checked(self.compare())
            }),
            timezone_request: &self.timezone_request,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn address(&self, view: &str) -> String {
        let mut query = self.query.clone();
        query.insert("view".into(), view.into());
        params::address(PATH, &query)
    }
}

impl Pages {
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.reports.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display
            .display(&**client, scope, scope.current_company(), locale)
            .await
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(if operation == "activity" {
            "Account entries"
        } else {
            "Reports"
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation == "sections" {
                let state =
                    sections::State::new(self.clone(), request.scope.ok_or(Error::Unauthorized)?)
                        .await?;
                return Ok(PageBody {
                    html: state.body()?,
                    live: true,
                    title: Some("Report sections".into()),
                });
            }
            let mut state = State::new(self.clone(), &request).await?;
            state
                .load(request.scope.ok_or(Error::Unauthorized)?)
                .await?;
            Ok(PageBody {
                title: state.title(),
                html: state.render()?,
                live: true,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation == "sections" {
                return Ok(Some(Box::new(
                    sections::State::new(self.clone(), request.scope.ok_or(Error::Unauthorized)?)
                        .await?,
                ) as Box<dyn LiveState<Scope, Error>>));
            }
            let mut state = State::new(self.clone(), &request).await?;
            if !state.activity {
                match self
                    .reports
                    .subscribe(
                        request.scope.ok_or(Error::Unauthorized)?,
                        company(request.scope.ok_or(Error::Unauthorized)?)?,
                    )
                    .await
                {
                    Ok(posting) => state.posting = Some(posting),
                    Err(_) => state.notices_unavailable = true,
                }
            }
            Ok(Some(Box::new(state) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}

impl LiveState<Scope, Error> for State {
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let Some(mut posting) = self.posting.take() else {
            return vec![];
        };
        // Receiver exists before forwarding begins; mount's first Refresh then
        // reads after LISTEN. No committed posting falls between read and listen.
        let (sender, receiver) = broadcast::channel(1);
        let dirty = self.dirty.clone();
        let tenant = self.tenant;
        self.bridge = Some(tokio::spawn(async move {
            while posting.next().await.is_some() {
                if !dirty.swap(true, Ordering::SeqCst) {
                    let _ = sender.send(page::Change { tenant, user: None });
                }
            }
        }));
        vec![Subscription { receiver }]
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Params(query) => {
                    // A patch keeps the flash, as Bilimbi's handle_params does.
                    self.auditor_pack = None;
                    self.query = query.clone();
                    self.list = ListState::parse(query, self.list.spec().clone().into())
                        .map_err(|_| Error::Internal)?;
                    self.reclock(request.scope).await?;
                    self.filter.set(period(query, &self.clock), &self.clock);
                    self.canonicalize();
                    self.load(request.scope).await?;
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
                LiveInput::Refresh => {
                    let dirty = self.dirty.swap(false, Ordering::SeqCst);
                    if self.loaded.is_none()
                        || (!self.activity
                            && self.pinned().is_none()
                            && (dirty || self.notices_unavailable))
                    {
                        self.load(request.scope).await?;
                    }
                }
            }
            Ok(LiveBody {
                html: self.render()?,
                query: self.query.clone(),
                ..LiveBody::default()
            })
        })
    }
}

impl State {
    async fn may_export(&self, scope: &Scope) -> Result<bool> {
        let Ok(company) = company(scope) else {
            return Ok(false);
        };
        let client = self.pages.reports.db.get().await?;
        match bilibi_company::authorize_work_target(&**client, scope, company, crate::EXPORT).await
        {
            Ok(_) => Ok(true),
            Err(Error::Forbidden | Error::Unauthorized | Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::Forbidden)
}
fn period_event(kind: &str) -> bool {
    matches!(
        kind.split_once(':').map(|(name, _)| name).unwrap_or(kind),
        "period_select" | "period_apply" | "period_cancel"
    )
}

fn period(query: &HashMap<String, String>, clock: &Clock) -> Period {
    Period::parse(
        |k| {
            query
                .get(k)
                .cloned()
                .or_else(|| (k == "period").then(|| "this_month".into()))
        },
        clock.today,
    )
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageView<'a> {
    id: &'a str,
    before: Vec<Html>,
    after: Vec<Html>,
    filter: Option<Html>,
    compare: Option<Field>,
    timezone_request: &'a str,
    flash: FlashMap<'a>,
}

// The browser may dismiss an older success while a refusal is in flight.
pub(super) fn clear_flash(flash: &mut Option<(StatusKind, String)>, value: &Value) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
    {
        *flash = None;
    }
}
