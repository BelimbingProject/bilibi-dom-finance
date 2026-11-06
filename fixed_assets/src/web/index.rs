//! Bilimbi FixedAssets.Web.ListPage and its three small page adapters.
use super::{
    input::{self, Draft},
    *,
};
use crate::{RegisterReport, ReportPins, ACQUIRE, CONFIGURE, DEPRECIATE, REVERSE, VIEW};
use bilibi_datetime::{filter::Outcome, Clock, Period, PeriodFilter};
use bilibi_ui::{
    Button, Choice, Filter, FilterToolbar, FlashMap, InputKind, ListSpec, ListState, Pagination,
    RecordLink, ToolbarControl,
};
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
    /// The empty-table line: "yet" when nothing is filtered, "match" only when a search is.
    fn empty(self, filtered: bool) -> &'static str {
        match (self, filtered) {
            (Self::Register, false) => "No assets yet.",
            (Self::Register, true) => "No assets match.",
            (Self::Categories, false) => "No categories yet.",
            (Self::Categories, true) => "No categories match.",
            (Self::Runs, false) => "No depreciation runs yet.",
            (Self::Runs, true) => "No depreciation runs match.",
        }
    }
    fn verb(self) -> &'static str {
        match self {
            Self::Register => "Register asset",
            Self::Categories => "Save category",
            Self::Runs => "Run depreciation",
        }
    }
    fn heading(self) -> &'static str {
        match self {
            Self::Register => "Register acquisition",
            Self::Categories => "Record category defaults",
            Self::Runs => "Run monthly depreciation",
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
                ("number", "Number"),
                ("description", "Asset"),
                ("cost", "Cost"),
                ("accumulated", "Depreciation"),
                ("carrying", "Carrying value"),
                ("source", "Source"),
                ("status", "Status"),
            ],
            Self::Categories => &[
                ("id", "Category ID"),
                ("name", "Category"),
                ("method", "Method"),
                ("life", "Life (months)"),
                ("revision", "Revision"),
            ],
            Self::Runs => &[
                ("id", "Run ID"),
                ("period", "Period"),
                ("total", "Depreciation"),
                ("entry", "Entry"),
                ("reversed", "Reversed"),
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
    draft: Draft,
    reverse: Draft,
    flash: Option<(StatusKind, String)>,
    opening: Option<super::opening::Opening>,
}
#[derive(Template)]
#[template(path = "index.html")]
struct View<'a> {
    page_dom: String,
    title: &'static str,
    title_actions: Html,
    actions: Html,
    opening: Html,
    flash: FlashMap<'a>,
    toolbar: Html,
    period: Html,
    unavailable: Html,
    table: Html,
    pager: Html,
    reconciliation: Html,
    heading: &'static str,
    form: Html,
    reverse: Html,
}
#[derive(Template)]
#[template(path = "reconciliation.html")]
struct ReconciliationView {
    heading: String,
    reproduce: String,
    rows: Vec<String>,
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
            draft: Draft::default(),
            reverse: Draft::default(),
            flash: None,
            opening: None,
        })
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
            self.period = Some(
                PeriodFilter::new("assets-report-period", parsed, &clock).label("Report through"),
            );
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
    fn fields(&self, accounts: &[(i64, String, String, bool, String)]) -> Vec<Field> {
        use InputKind::{Date, Number, Select, Text};
        let d = &self.draft;
        let mut fields = Vec::new();
        let specs: &[(&str, &str, InputKind, bool)] = match self.kind {
            Kind::Categories => &[
                (
                    "category_id",
                    "Existing category ID (blank creates one)",
                    Number,
                    false,
                ),
                ("name", "New category name", Text, false),
                ("cost_account_id", "Cost account", Select, true),
                (
                    "accumulated_account_id",
                    "Accumulated depreciation account",
                    Select,
                    true,
                ),
                (
                    "expense_account_id",
                    "Depreciation expense account",
                    Select,
                    true,
                ),
                ("gain_account_id", "Disposal gain account", Select, true),
                ("loss_account_id", "Disposal loss account", Select, true),
            ],
            Kind::Register => &[
                ("source", "Acquisition source", Select, false),
                ("category_revision_id", "Category revision ID", Number, true),
                ("description", "Asset description", Text, true),
                ("entry_id", "Manual entry ID", Number, false),
                (
                    "entry_line_number",
                    "Manual entry line number",
                    Number,
                    false,
                ),
                ("bill_id", "Supplier bill ID", Number, false),
                (
                    "bill_line_number",
                    "Supplier bill line number",
                    Number,
                    false,
                ),
                ("in_service_date", "In-service date", Date, true),
                (
                    "residual_value",
                    "Residual value (blank means zero)",
                    Text,
                    false,
                ),
            ],
            Kind::Runs => &[("period", "First day of month", Date, true)],
        };
        for (key, label, kind, required) in specs {
            let mut f = d.field(key, label, *kind, *required, "asset");
            if key.ends_with("account_id") {
                let expected = match *key {
                    "cost_account_id" | "accumulated_account_id" => "asset",
                    "expense_account_id" | "loss_account_id" => "expense",
                    _ => "income",
                };
                f = f.options(
                    std::iter::once(Choice::new("Choose an account", ""))
                        .chain(
                            accounts
                                .iter()
                                .filter(|a| a.3 && a.2 == expected)
                                .map(|a| Choice::new(&a.1, a.0.to_string()).detail(&a.4)),
                        )
                        .collect(),
                );
            }
            if *key == "source" {
                f = f.options(vec![
                    Choice::new("Manual ledger entry", "entry"),
                    Choice::new("Posted supplier bill", "bill"),
                ]);
                if d.value(key).is_empty() {
                    f = f.value("entry");
                }
            }
            fields.push(f);
        }
        if self.kind == Kind::Categories {
            fields.push(d.method_field());
            fields.extend([
                d.field(
                    "life_months",
                    "Default useful life (months)",
                    Number,
                    true,
                    "asset",
                ),
                d.field(
                    "annual_rate",
                    "Annual percentage (reducing balance)",
                    Text,
                    false,
                    "asset",
                ),
                d.field("reason", "Reason", Text, true, "asset"),
            ]);
        }
        fields
    }
    async fn write_form(
        &self,
        s: &Scope,
        accounts: &[(i64, String, String, bool, String)],
    ) -> Result<Html> {
        if !self.pages.allowed(s, self.kind.cap()).await? {
            return withheld(
                "assets-write-withheld",
                "Read-only access",
                "Ask your company administrator for permission to record this operation.",
            );
        }
        render(Operation {
            form_dom: "assets-write-form",
            event_name: "record_asset",
            button_dom: "assets-record",
            verb: self.kind.verb(),
            fields: self.fields(accounts),
            help: if self.kind == Kind::Runs {
                "One charge through this month, including any scheduled depreciation still unposted. Closed periods are refused."
            } else {
                ""
            },
        })
    }
    async fn reverse_form(&self, s: &Scope) -> Result<Html> {
        if self.kind != Kind::Runs {
            return Ok(Html::EMPTY);
        }
        let form = if self.pages.allowed(s, REVERSE).await? {
            render(Operation {
                form_dom: "assets-reverse-form",
                event_name: "reverse",
                button_dom: "assets-reverse",
                verb: "Post reversal",
                help: "",
                fields: vec![
                    self.reverse
                        .field("run_id", "Run ID", InputKind::Number, true, "asset"),
                    self.reverse.field(
                        "effective_date",
                        "Reversal date in an open period",
                        InputKind::Date,
                        true,
                        "asset",
                    ),
                    self.reverse
                        .field("reason", "Reason", InputKind::Text, true, "asset"),
                ],
            })?
        } else {
            withheld(
                "assets-reverse-withheld",
                "Reversals unavailable",
                "Ask your company administrator for asset reversal permission.",
            )?
        };
        let mut body = render(CardHeading {
            text: "Reverse a run",
        })?;
        body.push(&form);
        render(bilibi_ui::Card {
            class: "mt-4",
            body,
            ..Default::default()
        })
    }
    async fn rows(
        &self,
        s: &Scope,
        units: Option<bilibi_finance_ledger::MinorUnits>,
    ) -> crate::Result<(Vec<(String, FlexRow)>, Option<RegisterReport>)> {
        let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
        let mut rows = Vec::new();
        let report = match self.kind {
            Kind::Register => {
                let as_of = self.period.as_ref().expect("period initialized").period.to;
                let report = self
                    .pages
                    .read::<RegisterReport>(
                        s,
                        "finance.fixed_assets.register.get",
                        {
                            let mut value =
                                serde_json::to_value(self.pins()).map_err(|_| Error::Internal)?;
                            value["company_id"] = c.into();
                            value["as_of"] = serde_json::json!(as_of.to_string());
                            for key in ["source", "status"] {
                                if let Some(filter) =
                                    self.list.filters.get(key).filter(|v| !v.is_empty())
                                {
                                    value[key] = serde_json::json!(filter);
                                }
                            }
                            value
                        },
                        "report",
                    )
                    .await?;
                for r in &report.rows {
                    let a = &r.asset;
                    let link = render(RecordLink {
                        workspace: self.framed,
                        kind: MODULE,
                        record_id: &a.id.to_string(),
                        href: &format!("/finance/fixed-assets/register/{}", a.id),
                        body: Html::text(&a.description),
                        class: "",
                    })?;
                    let row = FlexRow::new(a.id.to_string())
                        .cell("number", Cell::text(a.asset_number.to_string()))
                        .slot("description", link)
                        .cell("cost", Cell::text(a.cost.display(units)))
                        .cell("accumulated", Cell::text(r.accumulated.display(units)))
                        .cell("carrying", Cell::text(r.carrying_value.display(units)))
                        .cell(
                            "source",
                            Cell::text(if a.opening_date.is_some() {
                                "Opening"
                            } else {
                                "Ordinary"
                            }),
                        )
                        .cell(
                            "status",
                            Cell::text(if r.voided {
                                "Voided"
                            } else if r.disposed {
                                "Disposed"
                            } else {
                                "In service"
                            }),
                        );
                    rows.push((format!("{} {}", a.asset_number, a.description), row));
                }
                Some(report)
            }
            Kind::Categories => {
                for c in self
                    .pages
                    .read::<Vec<crate::Category>>(
                        s,
                        "finance.fixed_assets.category.list",
                        serde_json::json!({"company_id":c}),
                        "records",
                    )
                    .await?
                {
                    let row = FlexRow::new(c.id.to_string())
                        .cell("id", Cell::text(c.id.to_string()))
                        .cell("name", Cell::text(c.name.clone()))
                        .cell(
                            "method",
                            Cell::text(input::method(c.revision.defaults.assumptions.method)),
                        )
                        .cell(
                            "life",
                            Cell::text(c.revision.defaults.assumptions.life_months.to_string()),
                        )
                        .cell("revision", Cell::text(c.revision.id.to_string()));
                    rows.push((c.name, row));
                }
                None
            }
            Kind::Runs => {
                for r in self
                    .pages
                    .read::<Vec<crate::Run>>(
                        s,
                        "finance.fixed_assets.run.list",
                        serde_json::json!({"company_id":c}),
                        "records",
                    )
                    .await?
                {
                    let skipped = if r.exceptions.is_empty() {
                        "None".into()
                    } else {
                        r.exceptions
                            .iter()
                            .map(|e| format!("#{} {}", e.asset_number, e.description))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let row = FlexRow::new(r.id.to_string())
                        .cell("id", Cell::text(r.id.to_string()))
                        .cell("period", Cell::text(r.period.to_string()))
                        .cell("total", Cell::text(r.total.display(units)))
                        .cell(
                            "entry",
                            Cell::text(
                                r.entry_number
                                    .map(|n| n.to_string())
                                    .unwrap_or_else(|| "No charge".into()),
                            ),
                        )
                        .cell(
                            "reversed",
                            Cell::text(if r.reversed { "Yes" } else { "No" }),
                        )
                        .cell("exceptions", Cell::text(skipped));
                    rows.push((r.period.to_string(), row));
                }
                None
            }
        };
        for (_, row) in &mut rows {
            row.dom_id = Some(format!("asset-row-{}", row.key));
        }
        Ok((rows, report))
    }
    async fn body(&mut self, s: &Scope) -> Result<String> {
        let (units, accounts) = self.pages.account_context(s).await?;
        let (mut rows, report, unavailable) = match self.rows(s, units).await {
            Ok((rows, r)) => (rows, r, Html::EMPTY),
            Err(e) => (
                vec![],
                None,
                render(
                    EmptyState::missing("Records unavailable", refusal(e)).id("assets-unavailable"),
                )?,
            ),
        };
        let search = self.list.search.to_lowercase();
        rows.retain(|(text, _)| text.to_lowercase().contains(&search));
        let page = self.list.paginate(&rows);
        let counts = page.counts;
        let rows = page.entries.iter().map(|(_, row)| row.clone()).collect();
        let table = if unavailable.is_empty() {
            table(
                &format!("assets-{}-table", self.kind.key()),
                &self.columns,
                rows,
                self.kind.title(),
                "grid",
                self.kind.empty(!self.list.search.is_empty()),
            )?
        } else {
            Html::EMPTY
        };
        let mut toolbar = FilterToolbar::new(
            "assets-filters",
            vec![ToolbarControl::search(
                "assets-search",
                "search",
                "Search",
                &self.list.search,
                "Search records…",
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
        let reconciliation = if let Some(r) = &report {
            // The reproduce link carries the filters, period and pins.
            let mut query = self.list.to_params();
            query.extend(self.period_params());
            for (key, value) in PINS.into_iter().zip([
                r.through_entry_number,
                r.through_asset_number,
                r.through_event_number,
                r.through_category_revision_id,
            ]) {
                query.insert(key.into(), value.to_string());
            }
            render(ReconciliationView {
                heading: format!("Whole register reconciliation as of {}", r.as_of),
                reproduce: params::address("/finance/fixed-assets/register", &query),
                rows: r
                    .reconciliation
                    .iter()
                    .map(|r| {
                        format!(
                            "{}: ledger {}, register {}, difference {}.",
                            accounts
                                .iter()
                                .find(|a| a.0 == r.account_id)
                                .map(|a| a.1.as_str())
                                .unwrap_or("Account unavailable"),
                            r.ledger_balance.display(units),
                            r.register_balance.display(units),
                            r.difference.display(units)
                        )
                    })
                    .collect(),
            })?
        } else {
            Html::EMPTY
        };
        let view = View {
            page_dom: format!("assets-{}", self.kind.key()),
            title: self.kind.title(),
            title_actions: bilibi_ui::Html::EMPTY,
            actions: if self.kind == Kind::Register && self.pages.allowed(s, VIEW).await? {
                render(Button {
                    id: "assets-opening",
                    button_type: "button",
                    event: "open_opening",
                    body: Html::text("Opening asset register"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
            opening: if let Some(opening) = &mut self.opening {
                opening.body(s).await?
            } else {
                Html::EMPTY
            },
            flash: flash_map(&self.flash),
            toolbar: render(toolbar)?,
            period: if self.kind == Kind::Register {
                render(self.period.as_ref().expect("period initialized").clone())?
            } else {
                Html::EMPTY
            },
            unavailable,
            table,
            pager: render(pager)?,
            reconciliation,
            heading: self.kind.heading(),
            form: self.write_form(s, &accounts).await?,
            reverse: self.reverse_form(s).await?,
        };
        view.render().map_err(|_| Error::Internal)
    }
    async fn record(&self, s: &Scope, credential: &str) -> crate::Result<Option<crate::Run>> {
        let c = company(s).ok_or(FixedAssetsError::CompanyNotFound)?;
        let d = &self.draft;
        let (key, input) = match self.kind {
            Kind::Categories => {
                let mut input = serde_json::to_value(d.revision()?).map_err(|_| Error::Internal)?;
                if let Some(id) = d.optional_id("category_id")? {
                    input["category_id"] = id.into();
                } else {
                    input["name"] = serde_json::json!(d.value("name"));
                }
                ("finance.fixed_assets.category.save", input)
            }
            Kind::Register => {
                let bill = d.value("source") == "bill";
                let (id, line) = if bill {
                    d.source("bill_id", "bill_line_number")?
                } else {
                    d.source("entry_id", "entry_line_number")?
                };
                let units = self.pages.minor_units(s, c).await?;
                let mut input =
                    serde_json::to_value(d.acquisition(units)?).map_err(|_| Error::Internal)?;
                input["source"] = serde_json::json!(if bill { "bill" } else { "entry" });
                input["source_id"] = id.into();
                input["line_number"] = line.into();
                ("finance.fixed_assets.asset.acquire", input)
            }
            Kind::Runs => (
                "finance.fixed_assets.run.post",
                serde_json::json!({"period":d.date("period").map_err(|_|FixedAssetsError::InvalidPeriod)?.to_string()}),
            ),
        };
        let result = self
            .pages
            .submit(s, credential, "record_asset", key, input)
            .await?;
        if self.kind == Kind::Runs {
            Ok(Some(
                serde_json::from_value(result).map_err(|_| Error::Internal)?,
            ))
        } else {
            Ok(None)
        }
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
            "browser_timezone" => {
                self.browser_zone = v["timezone"].as_str().map(str::to_owned);
                self.period(s).await?;
            }
            "record_asset" => {
                self.draft = Draft::posted(v);
                if !self.pages.allowed(s, self.kind.cap()).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                match self.record(s, credential).await {
                    Ok(run) => {
                        self.flash = Some(
                            if let Some(r) = run.filter(|r| !r.exceptions.is_empty()) {
                                (
                                    StatusKind::Error,
                                    format!(
                                        "Recorded without asset {}: its acquisition entry was reversed. Void the acquisition from the asset page.",
                                        r.exceptions
                                            .iter()
                                            .map(|e| format!("#{}", e.asset_number))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                )
                            } else {
                                (StatusKind::Success, "Recorded.".into())
                            },
                        );
                        self.draft = Draft::default();
                    }
                    Err(e) => {
                        self.draft.refused(&e);
                        self.flash = Some((StatusKind::Error, refusal(e)));
                    }
                }
            }
            "reverse" => {
                if self.kind != Kind::Runs {
                    return Err(Error::Invalid);
                }
                self.reverse = Draft::posted(v);
                if !self.pages.allowed(s, REVERSE).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(());
                }
                let result = async {
                    let mut input = self.reverse.event_values();
                    input["run_id"] = self.reverse.id("run_id")?.into();
                    self.pages
                        .submit(
                            s,
                            credential,
                            "reverse",
                            "finance.fixed_assets.run.reverse",
                            input,
                        )
                        .await
                }
                .await;
                match result {
                    Ok(_) => {
                        self.flash = Some((StatusKind::Success, "Depreciation reversed.".into()))
                    }
                    Err(e) => {
                        self.reverse.refused(&e);
                        self.flash = Some((StatusKind::Error, refusal(e)));
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
            let html = self.body(r.scope).await?;
            Ok(LiveBody {
                html,
                query: self.canonical(),
                ..Default::default()
            })
        })
    }
}

/// Bilimbi's plain card heading (`<h2 class="font-medium">`), not a title bar.
#[derive(Template)]
#[template(source = r#"<h2 class="font-medium">{{ text }}</h2>"#, ext = "html")]
struct CardHeading<'a> {
    text: &'a str,
}
