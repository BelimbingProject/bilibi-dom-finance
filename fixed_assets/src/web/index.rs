//! Bilimbi FixedAssets.Web.ListPage and its three small page adapters: the
//! register, categories and runs share search, filters and pages; each one's
//! "+ X" opens its own dialog (`acquire`, `categories`, `runs`).
mod register;
use super::{acquire::Acquire, categories, input::Draft, runs, *};
use crate::{
    Category, RegisterReport, ReportPins, Run, ACQUIRE, CONFIGURE, DEPRECIATE, DISPOSE, REVERSE,
    VIEW,
};
use bilibi_datetime::{filter::Outcome, Clock, Period, PeriodFilter};
use bilibi_ui::{
    Choice, CreateButton, Filter, FilterToolbar, FlashMap, ListSpec, ListState, Pagination,
    RecordLink, SectionHeading, ToolbarControl,
};
use jiff::ToSpan;
use std::sync::LazyLock;

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: vec![25, 50, 100],
        default_page_size: 25,
        filters: [
            (
                "source".into(),
                Filter::one_of(&["", "ordinary", "opening"], ""),
            ),
            (
                "status".into(),
                Filter::one_of(&["", "in_service", "disposed", "voided"], ""),
            ),
        ]
        .into(),
        page_size_param: "page_size".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("fixed assets list spec")
});
const PINS: [&str; 4] = [
    "through_entry_number",
    "through_asset_number",
    "through_event_number",
    "through_category_revision_id",
];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Register,
    Categories,
    Runs,
}
impl Kind {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "register" => Ok(Self::Register),
            "categories" => Ok(Self::Categories),
            "runs" => Ok(Self::Runs),
            _ => Err(Error::NotFound),
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Categories => "categories",
            Self::Runs => "runs",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Register => "Fixed assets",
            Self::Categories => "Asset categories",
            Self::Runs => "Depreciation runs",
        }
    }
    /// What the list holds, for its empty state.
    fn noun(self) -> &'static str {
        match self {
            Self::Register => "fixed assets",
            Self::Categories => "asset categories",
            Self::Runs => "depreciation runs",
        }
    }
    fn cap(self) -> &'static str {
        match self {
            Self::Register => ACQUIRE,
            Self::Categories => CONFIGURE,
            Self::Runs => DEPRECIATE,
        }
    }
    fn columns(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Register => &[
                ("number", "No."),
                ("description", "Asset"),
                ("category", "Category"),
                ("cost", "Cost"),
                ("accumulated", "Depreciation"),
                ("carrying", "Carrying value"),
                ("source", "Source"),
                ("status", "Status"),
            ],
            Self::Categories => &[
                ("name", "Category"),
                ("method", "Method"),
                ("life", "Useful life"),
                ("rate", "Annual rate"),
                ("cost_account_id", "Cost account"),
                ("accumulated_account_id", "Accumulated depreciation"),
                ("expense_account_id", "Depreciation expense"),
                ("gain_account_id", "Disposal gain"),
                ("loss_account_id", "Disposal loss"),
            ],
            Self::Runs => &[
                ("period", "Month"),
                ("total", "Depreciation"),
                ("entry", "Journal entry"),
                ("status", "Status"),
                ("exceptions", "Skipped assets"),
            ],
        }
    }
}
pub(super) struct Index {
    pages: Pages,
    kind: Kind,
    list: ListState,
    columns: ListColumns,
    framed: bool,
    period: Option<PeriodFilter>,
    query: HashMap<String, String>,
    browser_zone: Option<String>,
    flash: Option<(StatusKind, String)>,
    opening: Option<super::opening::Opening>,
    /// The register's "+ Asset" dialog while it is open.
    acquire: Option<Acquire>,
    categories: categories::State,
    runs: runs::State,
    /// What the categories and runs pages last read, for their writes.
    category_list: Vec<Category>,
    run_list: Vec<Run>,
    purchases: Option<Html>,
    history: ListColumns,
    check: ListColumns,
}
#[derive(Template)]
#[template(path = "index.html")]
struct View<'a> {
    page_dom: String,
    actions: Html,
    title: &'static str,
    opening: Html,
    purchases: Html,
    flash: FlashMap<'a>,
    filters: Html,
    unavailable: Html,
    list: Html,
    after: Html,
    dialogs: Html,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex flex-wrap items-start gap-x-3"><div class="min-w-0 flex-1">{{ toolbar|safe }}</div>{{ period|safe }}</div>"#,
    ext = "html"
)]
struct Filters {
    toolbar: Html,
    period: Html,
}
impl Index {
    pub(super) fn new(
        pages: &Pages,
        operation: &str,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        let kind = Kind::parse(operation)?;
        Ok(Self {
            pages: pages.clone(),
            kind,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: cols(&format!("assets-{}-table", kind.key()), kind.columns())?,
            framed,
            period: None,
            query: query.clone(),
            browser_zone: None,
            flash: None,
            opening: None,
            acquire: None,
            categories: categories::State::default(),
            runs: runs::State::default(),
            category_list: Vec::new(),
            run_list: Vec::new(),
            purchases: None,
            history: categories::history_columns()?,
            check: cols(
                "assets-ledger-check",
                &[
                    ("account", "Account"),
                    ("ledger", "Ledger"),
                    ("register", "Register"),
                    ("difference", "Difference"),
                    ("other", "Not from fixed assets"),
                ],
            )?,
        })
    }
    /// A dialog the address asks for: `?new=1` opens the page's "+ X",
    /// `?reverse=<run>` the reversal of that run, and `?opening=1` the opening
    /// register (the opening balances page's step).
    async fn open_from_query(&mut self, s: &Scope) -> Result<()> {
        if self.query.remove("opening").as_deref() == Some("1")
            && self.kind == Kind::Register
            && self.pages.allowed(s, VIEW).await?
        {
            self.opening = Some(super::opening::Opening::new(self.pages.clone())?);
        }
        if self.query.get("new").map(String::as_str) == Some("1") {
            self.query.remove("new");
            self.open_new(s).await?;
        }
        if let Some(id) = self.query.remove("reverse").and_then(|v| v.parse().ok()) {
            if self.kind == Kind::Runs && self.pages.allowed(s, REVERSE).await? {
                self.runs.reverse = Some((id, Draft::default()));
            }
        }
        Ok(())
    }
    async fn open_new(&mut self, s: &Scope) -> Result<()> {
        if !self.pages.allowed(s, self.kind.cap()).await? {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(());
        }
        match self.kind {
            Kind::Register => match Acquire::open(&self.pages, s).await {
                Ok(a) => self.acquire = Some(a),
                Err(e) => self.flash = Some((StatusKind::Error, refusal(e))),
            },
            Kind::Categories => self.categories.new = Some(Draft::default()),
            Kind::Runs => {
                let c = company(s).ok_or(Error::Invalid)?;
                if let Ok(runs) = self
                    .pages
                    .read::<Vec<Run>>(
                        s,
                        "finance.fixed_assets.run.list",
                        serde_json::json!({"company_id": c}),
                        "records",
                    )
                    .await
                {
                    self.run_list = runs;
                }
                let today = self.clock(s).await?.today;
                let period = if let Some(last) = self.run_list.iter().map(|r| r.period).max() {
                    last.checked_add(1.month())
                        .unwrap_or(today.first_of_month())
                } else {
                    self.pages
                        .read::<RegisterReport>(
                            s,
                            "finance.fixed_assets.register.get",
                            serde_json::json!({"company_id": c, "as_of": today.to_string()}),
                            "report",
                        )
                        .await
                        .ok()
                        .and_then(|report| {
                            report
                                .rows
                                .iter()
                                .filter(|r| !r.voided && !r.disposed)
                                .map(|r| {
                                    r.asset
                                        .opening_date
                                        .unwrap_or(r.asset.in_service_date)
                                        .first_of_month()
                                })
                                .min()
                        })
                        .unwrap_or(today.first_of_month())
                };
                let mut draft = Draft::default();
                draft.values.insert("period".into(), period.to_string());
                self.runs.new = Some(draft);
            }
        }
        Ok(())
    }
    fn pins(&self) -> ReportPins {
        let pin = |key| {
            self.query
                .get(key)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|n| *n >= 0)
        };
        ReportPins {
            through_entry_number: pin(PINS[0]),
            through_asset_number: pin(PINS[1]),
            through_event_number: pin(PINS[2]),
            through_category_revision_id: pin(PINS[3]),
        }
    }
    async fn clock(&self, s: &Scope) -> Result<Clock> {
        Ok(Clock::resolve(
            &self.pages.display(s).await?,
            self.browser_zone.as_deref(),
            jiff::Timestamp::now(),
        ))
    }
    async fn period(&mut self, s: &Scope) -> Result<()> {
        let clock = self.clock(s).await?;
        let parsed = Period::parse(|k| self.query.get(k).cloned(), clock.today);
        if let Some(filter) = &mut self.period {
            filter.set(parsed, &clock);
        } else {
            self.period =
                Some(PeriodFilter::new("assets-report-period", parsed, &clock).label("As of"));
        }
        Ok(())
    }
    fn canonical(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        query.extend(self.period_params());
        query.extend(
            PINS.into_iter()
                .filter_map(|key| self.query.get(key).map(|value| (key.into(), value.clone()))),
        );
        if self.acquire.is_some() || self.categories.new.is_some() || self.runs.new.is_some() {
            query.insert("new".into(), "1".into());
        }
        if let Some((id, _)) = &self.runs.reverse {
            query.insert("reverse".into(), id.to_string());
        }
        query
    }
    fn clear_pins(&mut self) {
        for key in PINS {
            self.query.remove(key);
        }
    }
    fn period_params(&self) -> HashMap<String, String> {
        self.period
            .iter()
            .flat_map(|p| p.period.params().map(|(k, v)| (k.into(), v)))
            .collect()
    }
    async fn rows(
        &mut self,
        s: &Scope,
        units: Option<bilibi_finance_ledger::MinorUnits>,
        accounts: &categories::Accounts,
    ) -> crate::Result<(Vec<(String, FlexRow)>, Option<RegisterReport>)> {
        let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
        let mut rows = Vec::new();
        let report = match self.kind {
            Kind::Register => Some(self.register_rows(s, c, units, &mut rows).await?),
            Kind::Categories => {
                self.category_list = self
                    .pages
                    .read::<Vec<Category>>(
                        s,
                        "finance.fixed_assets.category.list",
                        serde_json::json!({"company_id":c}),
                        "records",
                    )
                    .await?;
                let can = self.pages.allowed(s, CONFIGURE).await?;
                for c in &self.category_list {
                    rows.push((c.name.clone(), self.categories.row(c, accounts, can)?));
                }
                None
            }
            Kind::Runs => {
                self.run_list = self
                    .pages
                    .read::<Vec<Run>>(
                        s,
                        "finance.fixed_assets.run.list",
                        serde_json::json!({"company_id":c}),
                        "records",
                    )
                    .await?;
                let reversible = if self.pages.allowed(s, REVERSE).await? {
                    runs::reversible(&self.run_list)
                } else {
                    None
                };
                for r in &self.run_list {
                    rows.push((
                        format!("{} {}", month(r.period), r.period),
                        runs::row(r, units, reversible == Some(r.id))?,
                    ));
                }
                None
            }
        };
        for (_, row) in &mut rows {
            row.dom_id = Some(format!("asset-row-{}", row.key));
        }
        Ok((rows, report))
    }
    /// The page's one create action, for someone who may take it.
    async fn create_button(&self, s: &Scope, id: &'static str) -> Result<Html> {
        if !self.pages.allowed(s, self.kind.cap()).await? {
            return Ok(Html::EMPTY);
        }
        match self.kind {
            Kind::Register => render(CreateButton {
                noun: "Asset",
                id,
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            }),
            Kind::Categories => render(CreateButton {
                noun: "Category",
                id,
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            }),
            Kind::Runs => header_button(id, "new", "Run depreciation", true),
        }
    }
    async fn read_purchases(&self, s: &Scope, accounts: &categories::Accounts) -> Result<Html> {
        if self.kind != Kind::Register || !self.pages.allowed(s, ACQUIRE).await? {
            return Ok(Html::EMPTY);
        }
        let c = company(s).ok_or(Error::Invalid)?;
        let Ok(categories): std::result::Result<Vec<Category>, _> = self
            .pages
            .read(
                s,
                "finance.fixed_assets.category.list",
                serde_json::json!({"company_id":c}),
                "records",
            )
            .await
        else {
            return Ok(Html::EMPTY);
        };
        let mut seen = std::collections::HashSet::new();
        let mut result = Html::EMPTY;
        for category in categories {
            let account = category.revision.defaults.cost_account_id;
            if !seen.insert(account) {
                continue;
            }
            let Ok(sources): std::result::Result<Vec<crate::AcquisitionSource>, _> = self
                .pages
                .read(
                    s,
                    "finance.fixed_assets.acquisition.sources",
                    serde_json::json!({"company_id":c,"category_revision_id":category.revision.id}),
                    "records",
                )
                .await
            else {
                continue;
            };
            if sources.is_empty() {
                continue;
            }
            let name = accounts
                .iter()
                .find(|a| a.0 == account)
                .map(|a| a.1.as_str())
                .unwrap_or("this account");
            let mut body = Html::text(&format!(
                "{} {} on {} {} not in the register. ",
                if sources.len() == 500 {
                    "500+".to_owned()
                } else {
                    sources.len().to_string()
                },
                if sources.len() == 1 {
                    "purchase"
                } else {
                    "purchases"
                },
                name,
                if sources.len() == 1 { "is" } else { "are" }
            ));
            body.push(&render(Button {
                id: &format!("assets-register-purchase-{account}"),
                button_type: "button",
                event: "new",
                name: "category_revision_id",
                value: &category.revision.id.to_string(),
                body: Html::text("Register asset"),
                ..Button::default()
            })?);
            result.push(&render(bilibi_ui::Alert {
                id: &format!("assets-unregistered-{account}"),
                kind: StatusKind::Info,
                body,
                ..Default::default()
            })?);
        }
        Ok(result)
    }
    async fn body(&mut self, s: &Scope) -> Result<String> {
        let (units, accounts) = self.pages.account_context(s).await?;
        let (mut rows, report, unavailable) = match self.rows(s, units, &accounts).await {
            Ok((rows, r)) => (rows, r, Html::EMPTY),
            Err(e) => (
                vec![],
                None,
                render(
                    EmptyState::missing("Records unavailable", refusal(e)).id("assets-unavailable"),
                )?,
            ),
        };
        let unfiltered = rows.len();
        let search = self.list.search.to_lowercase();
        rows.retain(|(text, _)| text.to_lowercase().contains(&search));
        let page = self.list.paginate(&rows);
        let counts = page.counts;
        let rows = page.entries.iter().map(|(_, row)| row.clone()).collect();
        let filtered =
            !self.list.search.is_empty() || self.list.filters.values().any(|v| !v.is_empty());
        let table = if !unavailable.is_empty() {
            Html::EMPTY
        } else if unfiltered == 0 && !filtered {
            // A true empty list says so and offers the way to fill it.
            let mut empty = EmptyState::missing(
                format!("No {} yet", self.kind.noun()),
                match self.kind {
                    Kind::Register => {
                        "Register an asset from the bill or journal entry that bought it."
                    }
                    Kind::Categories => {
                        "A category sets the accounts and useful life its assets start with."
                    }
                    Kind::Runs => "Run depreciation once a month for all assets in use.",
                },
            )
            .id("assets-empty");
            empty = empty.action(self.create_button(s, "assets-empty-create").await?);
            render(empty)?
        } else {
            table(
                &format!("assets-{}-table", self.kind.key()),
                &self.columns,
                rows,
                self.kind.title(),
                "grid",
                &format!("No {} match these filters.", self.kind.noun()),
            )?
        };
        let mut toolbar = FilterToolbar::new(
            "assets-filters",
            vec![ToolbarControl::search(
                "assets-search",
                "search",
                "Search",
                &self.list.search,
                match self.kind {
                    Kind::Register => "Search assets…",
                    Kind::Categories => "Search categories…",
                    Kind::Runs => "Search months…",
                },
            )],
        );
        if self.kind == Kind::Register {
            toolbar.controls.push(ToolbarControl::select(
                "assets-source",
                "source",
                "Filter by source",
                &self.list.filters["source"],
                [
                    ("All sources", ""),
                    ("Ordinary", "ordinary"),
                    ("Opening", "opening"),
                ]
                .map(|(l, v)| Choice::new(l, v))
                .to_vec(),
            ));
            toolbar.controls.push(ToolbarControl::select(
                "assets-status",
                "status",
                "Filter by status",
                &self.list.filters["status"],
                [
                    ("All statuses", ""),
                    ("In service", "in_service"),
                    ("Disposed", "disposed"),
                    ("Voided", "voided"),
                ]
                .map(|(l, v)| Choice::new(l, v))
                .to_vec(),
            ));
        }
        toolbar.event = "filter";
        let mut pager = Pagination::new("assets-pagination", counts, &[25, 50, 100]);
        pager.filters_event = "filter";
        let after = match self.kind {
            Kind::Register => match &report {
                Some(r) => self.ledger_check(r, units, &accounts)?,
                None => Html::EMPTY,
            },
            Kind::Categories if !self.category_list.is_empty() => {
                let people = self
                    .pages
                    .people(
                        s,
                        self.category_list
                            .iter()
                            .flat_map(|c| &c.revisions)
                            .flat_map(|r| {
                                std::iter::once(r.recorded_by_user_id)
                                    .chain(r.recorded_by_impersonator_id)
                            }),
                    )
                    .await?;
                let display = self.pages.display(s).await?;
                categories::history(&self.category_list, &people, &display, &self.history)?
            }
            _ => Html::EMPTY,
        };
        let mut actions = Html::EMPTY;
        if self.kind == Kind::Register && self.pages.allowed(s, VIEW).await? {
            actions.push(&header_button(
                "assets-opening",
                "open_opening",
                "Opening asset register",
                false,
            )?);
        }
        actions.push(&self.create_button(s, "assets-create").await?);
        let mut dialogs = Html::EMPTY;
        if let Some(a) = &self.acquire {
            dialogs.push(&a.render(units)?);
        }
        dialogs.push(&self.categories.dialog(&accounts)?);
        dialogs.push(&self.runs.dialogs(&self.run_list)?);
        let filters = render(Filters {
            toolbar: render(toolbar)?,
            period: if self.kind == Kind::Register {
                render(self.period.as_ref().expect("period initialized").clone())?
            } else {
                Html::EMPTY
            },
        })?;
        let view = View {
            page_dom: format!("assets-{}", self.kind.key()),
            title: self.kind.title(),
            actions,
            opening: if let Some(opening) = &mut self.opening {
                opening.body(s).await?
            } else {
                Html::EMPTY
            },
            purchases: {
                if self.purchases.is_none() {
                    self.purchases = Some(self.read_purchases(s, &accounts).await?);
                }
                let mut purchases = Html::EMPTY;
                purchases.push(self.purchases.as_ref().expect("purchases read"));
                purchases
            },
            flash: flash_map(&self.flash),
            filters,
            unavailable,
            list: {
                let mut list = table;
                list.push(&render(pager)?);
                list
            },
            after,
            dialogs,
        };
        view.render().map_err(|_| Error::Internal)
    }
    async fn event(&mut self, s: &Scope, credential: &str, v: &serde_json::Value) -> Result<()> {
        let event = v["type"].as_str().unwrap_or("");
        let clock = self.clock(s).await?;
        if let Some(outcome) = self.period.as_mut().and_then(|p| p.event(event, v, &clock)) {
            if let Outcome::Applied(p) = outcome {
                self.clear_pins();
                self.period
                    .as_mut()
                    .expect("period initialized")
                    .set(p, &clock);
                self.query.extend(p.params().map(|(k, v)| (k.into(), v)));
                self.list.put_page(Some("1"));
            }
            return Ok(());
        }
        match event {
            "clear-flash" => {
                clear_flash(&mut self.flash, v);
            }
            "filter" => {
                self.clear_pins();
                self.list.apply_filters(&params::form_values(v, "filters"));
            }
            "page" => {
                self.clear_pins();
                self.list.put_page(v["page"].as_str());
            }
            "grid" => {
                self.columns.handle(&params::string_values(v));
            }
            "history_grid" => {
                self.history.handle(&params::string_values(v));
            }
            "check_grid" => {
                self.check.handle(&params::string_values(v));
            }
            "browser_timezone" => {
                self.browser_zone = v["timezone"].as_str().map(str::to_owned);
                self.period(s).await?;
            }
            "new" => {
                self.open_new(s).await?;
                if let (Some(a), Some(id)) = (
                    &mut self.acquire,
                    v["category_revision_id"]
                        .as_str()
                        .and_then(|v| v.parse().ok()),
                ) {
                    if let Err(e) = a.select_category(&self.pages, s, id).await {
                        a.refused(&e);
                    }
                }
            }
            "close_asset" => self.acquire = None,
            "close_category" => self.categories.new = None,
            "close_run" => self.runs.new = None,
            "close_reverse" => self.runs.reverse = None,
            "asset_change" if self.kind == Kind::Register => {
                if let Some(a) = &mut self.acquire {
                    if let Err(e) = a.change(&self.pages, s, v).await {
                        a.refused(&e);
                    }
                }
            }
            "register_asset" if self.kind == Kind::Register => {
                let Some(mut a) = self.acquire.take() else {
                    return Ok(());
                };
                if let Err(e) = a.change(&self.pages, s, v).await {
                    a.refused(&e);
                    self.acquire = Some(a);
                    return Ok(());
                }
                if !self.pages.allowed(s, ACQUIRE).await? {
                    a.refused(&FixedAssetsError::Unauthorized);
                    self.acquire = Some(a);
                    return Ok(());
                }
                match a.submit(&self.pages, s, credential).await {
                    Ok(description) => {
                        self.purchases = None;
                        self.flash =
                            Some((StatusKind::Success, format!("Registered {description}.")));
                    }
                    Err(e) => {
                        a.refused(&e);
                        self.acquire = Some(a);
                    }
                }
            }
            "category_change" if self.kind == Kind::Categories => self.categories.change(v),
            "create_category" if self.kind == Kind::Categories => {
                self.categories.change(v);
                if !self.pages.allowed(s, CONFIGURE).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                match self.categories.create(&self.pages, s, credential).await {
                    Ok(name) => {
                        self.categories.new = None;
                        self.flash = Some((StatusKind::Success, format!("Added {name}.")));
                    }
                    Err(e) => {
                        if let Some(d) = &mut self.categories.new {
                            d.refused(&e);
                            if d.errors.is_empty() {
                                d.errors.push(bilibi_finance_ledger::FieldError::new(
                                    "base",
                                    refusal(e),
                                ));
                            }
                        }
                    }
                }
            }
            "save_category" if self.kind == Kind::Categories => {
                if !self.pages.allowed(s, CONFIGURE).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                let (_, accounts) = self.pages.account_context(s).await?;
                let list = std::mem::take(&mut self.category_list);
                let saved = self
                    .categories
                    .save(&self.pages, s, credential, &list, &accounts, v)
                    .await;
                self.category_list = list;
                saved?;
            }
            "run_depreciation" if self.kind == Kind::Runs => {
                if let Some(d) = &mut self.runs.new {
                    *d = Draft::posted(v);
                }
                if !self.pages.allowed(s, DEPRECIATE).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                match self.runs.run(&self.pages, s, credential).await {
                    Ok(run) => {
                        self.runs.new = None;
                        self.flash = Some(if run.exceptions.is_empty() {
                            (
                                StatusKind::Success,
                                format!("Depreciation for {} posted.", month(run.period)),
                            )
                        } else {
                            (
                                StatusKind::Error,
                                format!(
                                    "Posted without asset {}: its acquisition entry was reversed. Void the acquisition from the asset page.",
                                    run.exceptions
                                        .iter()
                                        .map(|e| format!("#{}", e.asset_number))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            )
                        });
                    }
                    Err(e) => {
                        if let Some(d) = &mut self.runs.new {
                            d.refused(&e);
                            if d.errors.is_empty() {
                                d.errors.push(bilibi_finance_ledger::FieldError::new(
                                    "base",
                                    refusal(e),
                                ));
                            }
                        }
                    }
                }
            }
            "reverse_run" if self.kind == Kind::Runs => {
                if let Some((_, d)) = &mut self.runs.reverse {
                    *d = Draft::posted(v);
                }
                if !self.pages.allowed(s, REVERSE).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                match self.runs.reverse(&self.pages, s, credential).await {
                    Ok(()) => {
                        self.runs.reverse = None;
                        self.flash = Some((StatusKind::Success, "Depreciation reversed.".into()));
                    }
                    Err(e) => {
                        if let Some((_, d)) = &mut self.runs.reverse {
                            d.refused(&e);
                            if d.errors.is_empty() {
                                d.errors.push(bilibi_finance_ledger::FieldError::new(
                                    "base",
                                    refusal(e),
                                ));
                            }
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.period(r.scope).await?;
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.query = query.clone();
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.period(r.scope).await?;
                }
                LiveInput::Event(v) => match v["type"].as_str().unwrap_or("") {
                    "open_opening"
                        if self.kind == Kind::Register
                            && self.pages.allowed(r.scope, VIEW).await? =>
                    {
                        self.opening = Some(super::opening::Opening::new(self.pages.clone())?)
                    }
                    "close_opening" => self.opening = None,
                    event
                        if event.starts_with("source_evidence_grid:") && self.opening.is_some() =>
                    {
                        self.opening
                            .as_mut()
                            .expect("opening dialog")
                            .update(&r)
                            .await?;
                    }
                    "preview_opening" | "import_opening" | "cancel_opening" | "stage_grid"
                    | "balance_grid" | "class_grid" | "opening_grid" | "clear-flash"
                        if self.opening.is_some() =>
                    {
                        let opening = self.opening.as_mut().expect("opening dialog");
                        opening.update(&r).await?;
                        if opening.completed {
                            self.opening = None;
                            self.purchases = None;
                            self.flash = Some((StatusKind::Success, "The opening register was reconciled. Open an asset below to read its source evidence.".into()));
                            self.list.search.clear();
                            self.list.apply_filters(&HashMap::from([
                                ("source".into(), "opening".into()),
                                ("status".into(), "".into()),
                            ]));
                            self.query.clear();
                            self.period = None;
                            self.period(r.scope).await?;
                        }
                    }
                    _ => self.event(r.scope, r.credential, v).await?,
                },
            }
            self.open_from_query(r.scope).await?;
            let html = self.body(r.scope).await?;
            Ok(LiveBody {
                html,
                query: self.canonical(),
                ..Default::default()
            })
        })
    }
}
