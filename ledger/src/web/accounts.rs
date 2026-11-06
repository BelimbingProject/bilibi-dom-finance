//! The chart of accounts: every account and account group of the company's
//! book with its balance or group total, a dialog to add an account, and the
//! starter charts a company with an empty chart can preview and apply. Each
//! account's page is `account.rs`; groups are managed on `groups.rs`.
mod publishing;
use super::{
    account_name, capability_for, dialog_actions, error_text, event_type, figure, render, shell,
    unavailable, view, Company, Failure, Flash, Pages, Read, Refusal, Review,
};
use crate::{AccountType, MANAGE_ACCOUNTS};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice,
    ColumnsOutcome, Combobox, CreateButton, EmptyState, Field, Filter, FilterToolbar, FlexRow,
    FlexTable, Form, Header, Html, InputKind, ListColumns, ListSpec, ListState, Modal, PageCounts,
    PageVariant, Pagination, RecordLink, SectionHeading, SortDir, StatusKind, TableEmpty,
    ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 4] = [25, 50, 100, 300];
pub(super) const INDEX: &str = "/finance/ledger/accounts";
const TEMPLATE_CONTRACT: &str = "finance.ledger.chart_template.apply";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("code".into(), SortDir::Asc),
            ("name".into(), SortDir::Asc),
            ("type".into(), SortDir::Asc),
        ]),
        default_sort: Some("code".into()),
        page_sizes: SIZES.to_vec(),
        default_page_size: 50,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([
            ("company".into(), Filter::string("")),
            (
                "type".into(),
                Filter::one_of(
                    &["all", "asset", "liability", "equity", "income", "expense"],
                    "all",
                ),
            ),
            (
                "kind".into(),
                Filter::one_of(&["all", "account", "group"], "all"),
            ),
            (
                "status".into(),
                Filter::one_of(&["all", "active", "inactive"], "all"),
            ),
        ]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("chart of accounts list spec")
});

/// The new-account dialog's values and what the ledger said about them.
#[derive(Default)]
struct Draft {
    code: String,
    name: String,
    account_type: String,
    parent: String,
    retained: bool,
    refusal: Refusal,
    notice: String,
}

impl Draft {
    fn new() -> Self {
        Self {
            account_type: "asset".into(),
            ..Self::default()
        }
    }
    fn read(&mut self, value: &Value) {
        let fields = params::form_values(value, "account");
        self.code = fields.get("code").cloned().unwrap_or_default();
        self.name = fields.get("name").cloned().unwrap_or_default();
        self.account_type = fields.get("account_type").cloned().unwrap_or_default();
        self.parent = fields.get("parent").cloned().unwrap_or_default();
        self.retained = fields.get("retained").map(String::as_str) == Some("true");
    }
    fn equity(&self) -> bool {
        self.account_type == "equity"
    }
    /// Whether `account` can hold an account of the chosen type: an account
    /// group of the same type, as the ledger requires.
    fn can_group(&self, account: &view::Account) -> bool {
        account.is_group && account.account_type == self.account_type
    }
    /// Drops what the chosen type no longer allows, after the person changed
    /// it: a group of another type, and the retained-earnings designation on
    /// anything but an equity account. The ledger's own check still decides.
    fn reconcile(&mut self, chart: &[view::Account]) {
        if !self.equity() {
            self.retained = false;
        }
        let eligible = self
            .parent
            .parse::<i64>()
            .ok()
            .and_then(|id| chart.iter().find(|a| a.id == id))
            .is_some_and(|parent| self.can_group(parent));
        if !eligible {
            self.parent.clear();
        }
    }
    fn input(&self) -> Value {
        let mut input = json!({
            "code": self.code.trim(),
            "name": self.name.trim(),
            "account_type": self.account_type,
        });
        if let Ok(parent) = self.parent.parse::<i64>() {
            input["parent_id"] = json!(parent);
        }
        // A designation ticked while the type was equity never travels with
        // another type.
        if self.retained && self.equity() {
            input["retained_earnings"] = json!(true);
        }
        input
    }
}

/// A starter chart the person is looking at before applying it: the
/// contract's own review of exactly what would be created.
struct Preview {
    review: Review,
    key: String,
    sections: Vec<view::TemplateSection>,
    name: String,
    status: String,
    notes: Vec<String>,
    rows: Vec<view::TemplateRow>,
}

struct Chart {
    company: Company,
    accounts: Vec<view::Account>,
    figures: Option<view::ChartBalances>,
    currency: String,
}

pub(super) struct Accounts {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    template_columns: ListColumns,
    flash: Flash,
    draft: Option<Draft>,
    preview: Option<Preview>,
    framed: bool,
    publication: Option<publishing::Publication>,
}

impl Accounts {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("company", "Company"),
            BuiltinColumn::new("currency", "Currency"),
            BuiltinColumn::new("code", "Code").sortable("code"),
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("kind", "Kind"),
            BuiltinColumn::new("type", "Type").sortable("type"),
            BuiltinColumn::new("group", "Group"),
            BuiltinColumn::new("balance", "Balance"),
            BuiltinColumn::new("total", "Group total"),
            BuiltinColumn::new("status", "Status"),
            BuiltinColumn::new("retained", "Retained earnings"),
        ];
        let template_columns = vec![
            BuiltinColumn::new("code", "Code"),
            BuiltinColumn::new("name", "Name"),
            BuiltinColumn::new("kind", "Kind"),
            BuiltinColumn::new("type", "Type"),
            BuiltinColumn::new("group", "Group"),
            BuiltinColumn::new("retained", "Retained earnings"),
        ];
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("ledger-accounts-table", columns)
                .map_err(|_| Error::Internal)?,
            template_columns: ListColumns::mount("ledger-template-table", template_columns)
                .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            draft: query.get("new").is_some().then(Draft::new),
            preview: None,
            framed,
            publication: None,
        })
    }

    /// The default is the current company. All companies is an explicit
    /// comparison of separate charts; it never changes the session company.
    fn selected(&self, scope: &Scope) -> Option<i64> {
        match self.list.filters["company"].as_str() {
            "all" => None,
            "" => Pages::company(scope),
            value => value.parse::<i64>().ok().filter(|id| *id > 0),
        }
    }

    async fn target(&self, scope: &Scope) -> Result<Option<i64>> {
        let Some(company) = self.selected(scope) else {
            return Ok(None);
        };
        if !self
            .pages
            .companies(scope)
            .await?
            .iter()
            .any(|c| c.id == company)
        {
            return Ok(None);
        }
        Ok(Some(company))
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let companies = self.pages.companies(scope).await?;
        let selected = self.selected(scope);
        let all = self.list.filters["company"] == "all";
        let listed: Vec<&Company> = companies
            .iter()
            .filter(|c| all || Some(c.id) == selected)
            .collect();
        let can_manage = self.pages.allowed(scope, MANAGE_ACCOUNTS).await?;
        let mut body = Html::EMPTY;
        let mut actions = Html::EMPTY;
        let mut charts = Vec::new();
        let (mut no_books, mut empty_charts, mut denied, mut failed) = (0, 0, 0, 0);
        let mut draft_chart = Vec::new();
        let mut editable = false;
        let subtitle = match listed.as_slice() {
            [one] => one.name.clone(),
            _ if all => "All companies".into(),
            _ => String::new(),
        };
        // The filter is always visible, including the single-company case:
        // it offers only names the shared company directory authorizes.
        let mut options = Vec::new();
        if companies.len() > 1 {
            options.push(Choice::new("All companies", "all"));
        }
        options.extend(
            companies
                .iter()
                .map(|c| Choice::new(&c.name, c.id.to_string())),
        );
        body.push(&render(Form {
            id: "ledger-accounts-company-filter",
            event: "company_filter",
            change: true,
            class: "mb-4 max-w-sm",
            body: render(
                Combobox::new("ledger-accounts-company", "company_filter[company]")
                    .label("Company")
                    .wrapper_class("mb-0")
                    .value(if all && companies.len() > 1 {
                        "all".into()
                    } else {
                        selected.map(|id| id.to_string()).unwrap_or_default()
                    })
                    .options(options),
            )?,
            actions: Html::EMPTY,
        })?);
        if listed.is_empty() {
            body.push(&render(
                EmptyState::missing("No company available", "Select a company you can work in.")
                    .id("ledger-accounts-no-company"),
            )?);
        }
        for company in &listed {
            let chart = match self.pages.chart_in(scope, company.id).await? {
                Ok(chart) => chart,
                Err(refused) => {
                    if all {
                        if matches!(refused, Read::Denied) {
                            denied += 1;
                        } else {
                            failed += 1;
                        }
                        continue;
                    }
                    body.push(&render(SectionHeading {
                        id: &format!("ledger-chart-{}", company.id),
                        title: &company.name,
                        ..SectionHeading::default()
                    })?);
                    body.push(&unavailable(
                        &format!("ledger-chart-refused-{}", company.id),
                        "the chart of accounts",
                        &refused,
                    )?);
                    continue;
                }
            };
            let book = self
                .pages
                .book_in(scope, company.id)
                .await?
                .ok()
                .and_then(|b| b.book);
            let book_open = book.is_some();
            // A chart template is proposed from, or saved out of, the current company.
            if !all
                && Some(company.id) == Pages::company(scope)
                && self.pages.allowed(scope, crate::MANAGE_TEMPLATES).await?
            {
                for (event, label) in [
                    ("propose_chart", "Template proposal"),
                    ("publish_chart", "Save reusable chart"),
                ] {
                    if event == "publish_chart" && chart.is_empty() {
                        continue;
                    }
                    actions.push(&render(Button {
                        id: &format!("ledger-{event}"),
                        event,
                        button_type: "button",
                        body: Html::text(label),
                        ..Button::default()
                    })?);
                }
            }
            if !all && can_manage && book_open {
                editable = true;
                draft_chart = chart.clone();
                actions.push(&render(Button {
                    id: "ledger-account-groups-link",
                    href: &if self.list.filters["company"].is_empty() {
                        super::groups::INDEX.to_string()
                    } else {
                        format!("{}?company={}", super::groups::INDEX, company.id)
                    },
                    body: Html::text("Account groups"),
                    ..Button::default()
                })?);
                actions.push(&render(CreateButton {
                    noun: "Account",
                    id: "ledger-account-new",
                    event: "new",
                    button_type: "button",
                    ..CreateButton::default()
                })?);
            }
            if all && chart.is_empty() {
                if book_open {
                    empty_charts += 1;
                } else {
                    no_books += 1;
                }
            } else if !book_open && chart.is_empty() {
                let current = Some(company.id) == Pages::company(scope);
                let mut empty = EmptyState::missing(
                    format!("{}: book not open", company.name),
                    if current {
                        "Open the company's book to choose its currency and decimal places."
                    } else {
                        "Choose this company in the top bar, then open its book."
                    },
                )
                .id(format!("ledger-chart-no-book-{}", company.id));
                if current {
                    empty = empty.action(render(Button {
                        id: "ledger-accounts-open-book",
                        href: &format!("{}/{}", super::book::INDEX, company.id),
                        body: Html::text("Go to the book"),
                        variant: ButtonVariant::Primary,
                        ..Button::default()
                    })?);
                }
                body.push(&render(empty)?);
            } else if chart.is_empty() && editable && !all {
                body.push(&self.starters(scope, company.id).await?);
            } else if chart.is_empty() {
                body.push(&render(
                    EmptyState::missing(
                        format!("{}: no accounts yet", company.name),
                        "You do not have permission to set up this chart.",
                    )
                    .id(format!("ledger-chart-empty-{}", company.id)),
                )?);
            }
            let figures = if book_open {
                self.pages.chart_figures_in(scope, company.id).await?
            } else {
                None
            };
            charts.push(Chart {
                company: (*company).clone(),
                accounts: chart,
                figures,
                currency: book.map(|b| b.currency_code).unwrap_or_default(),
            });
        }
        let rows_shown = if charts.iter().any(|c| !c.accounts.is_empty()) {
            self.table(&charts, &mut body)?
        } else {
            0
        };
        if all {
            if no_books + empty_charts + denied + failed > 0 {
                body.push(&render(Alert { id: "ledger-accounts-company-states", kind: StatusKind::Info,
                    body: Html::text(&format!("Companies without chart rows — books not open: {no_books}; empty charts: {empty_charts}; no access: {denied}; could not be read: {failed}. Select one company for details.")), ..Alert::default() })?);
            }
            body.push(&render(Note { id: "ledger-accounts-comparison", text: "Select one company to add accounts or apply a starter chart. Each balance is in its company's currency; no consolidation or currency translation is performed." })?);
        }
        let mut page = render(Header {
            title: Html::text("Chart of accounts"),
            subtitle: &subtitle,
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&body);
        if let Some(draft) = &mut self.draft {
            draft.reconcile(&draft_chart);
        }
        if self.draft.is_some() && editable {
            page.push(&render(Modal {
                id: "ledger-account-modal",
                title: "New account",
                description: &format!("Add an account to {subtitle}."),
                cancel_event: "close_modal",
                body: self.form(&draft_chart)?,
                ..Modal::default()
            })?);
        }
        page.push(&self.publication_dialog()?);
        let mut query = self.list.to_params();
        if self.draft.is_some() {
            query.insert("new".into(), "1".into());
        }
        Ok(LiveBody {
            html: shell("ledger-accounts", PageVariant::List, &self.flash, page)?,
            query,
            rows: rows_shown,
            ..LiveBody::default()
        })
    }

    fn table(&mut self, charts: &[Chart], body: &mut Html) -> Result<usize> {
        let search = self.list.search.trim().to_lowercase();
        let by_id: HashMap<i64, &view::Account> = charts
            .iter()
            .flat_map(|c| c.accounts.iter())
            .map(|a| (a.id, a))
            .collect();
        let mut shown: Vec<(&Chart, &view::Account)> = charts
            .iter()
            .flat_map(|c| c.accounts.iter().map(move |a| (c, a)))
            .filter(|(_, a)| {
                let kind = self.list.filters["type"].as_str();
                let status = self.list.filters["status"].as_str();
                (kind == "all" || a.account_type == kind)
                    && match self.list.filters["kind"].as_str() {
                        "account" => !a.is_group,
                        "group" => a.is_group,
                        _ => true,
                    }
                    && match status {
                        "active" => a.active,
                        "inactive" => !a.active,
                        _ => true,
                    }
                    && (search.is_empty()
                        || a.code.to_lowercase().contains(&search)
                        || a.name.to_lowercase().contains(&search))
            })
            .collect();
        let descending = self.list.sort_dir == Some(SortDir::Desc);
        match self.list.sort_by.as_deref() {
            Some("name") => shown.sort_by_key(|(c, a)| (a.name.to_lowercase(), c.company.id)),
            Some("type") => shown.sort_by(|(_, a), (_, b)| {
                (a.account_type.as_str(), a.code.as_str())
                    .cmp(&(b.account_type.as_str(), b.code.as_str()))
            }),
            _ => shown.sort_by(|(ca, a), (cb, b)| {
                (a.code.as_str(), ca.company.id).cmp(&(b.code.as_str(), cb.company.id))
            }),
        }
        if descending {
            shown.reverse();
        }
        let total = shown.len() as u64;
        self.list
            .clamp_to_last_page(total.div_ceil(self.list.page_size), true);
        let start = ((self.list.page - 1) * self.list.page_size) as usize;
        let page: Vec<(&Chart, &view::Account)> = shown
            .into_iter()
            .skip(start)
            .take(self.list.page_size as usize)
            .collect();
        self.columns.load(&page, |(_, a)| a.id.to_string());

        let form = self.list.filters_form();
        body.push(&render(FilterToolbar::new(
            "ledger-accounts-filters",
            vec![
                ToolbarControl::search(
                    "ledger-accounts-search",
                    "search",
                    "Search accounts",
                    &form["search"],
                    "Search by code or name…",
                ),
                ToolbarControl::select(
                    "ledger-accounts-type",
                    "type",
                    "Type",
                    &form["type"],
                    vec![
                        Choice::new("All types", "all"),
                        Choice::new("Asset", "asset"),
                        Choice::new("Liability", "liability"),
                        Choice::new("Equity", "equity"),
                        Choice::new("Income", "income"),
                        Choice::new("Expense", "expense"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-accounts-kind",
                    "kind",
                    "Kind",
                    &form["kind"],
                    vec![
                        Choice::new("Accounts and groups", "all"),
                        Choice::new("Accounts", "account"),
                        Choice::new("Groups", "group"),
                    ],
                ),
                ToolbarControl::select(
                    "ledger-accounts-status",
                    "status",
                    "Status",
                    &form["status"],
                    vec![
                        Choice::new("All accounts", "all"),
                        Choice::new("Active", "active"),
                        Choice::new("Inactive", "inactive"),
                    ],
                ),
            ],
        ))?);

        let mut rows = Vec::new();
        for (chart, account) in &page {
            let figures = chart.figures.as_ref();
            let units = figures.map(|f| f.minor_units);
            let group = match account.parent_id.and_then(|id| by_id.get(&id)) {
                Some(parent) => account_name(parent)?,
                None => Html::text("—"),
            };
            let href = if account.is_group {
                format!(
                    "{}/{}?company={}",
                    super::groups::INDEX,
                    account.id,
                    chart.company.id
                )
            } else {
                format!("{INDEX}/{}?company={}", account.id, chart.company.id)
            };
            let shown = |text: Option<&String>| -> Result<Html> {
                match (text, units) {
                    (Some(text), Some(units)) => figure(&view::amount(text, units)),
                    _ => Ok(Html::text("—")),
                }
            };
            let own = figures.and_then(|f| f.rows.iter().find(|r| r.id == account.id));
            rows.push({
                let mut row = FlexRow::new(account.id.to_string())
                    .cell("company", Cell::text(&chart.company.name))
                    .cell("currency", Cell::text(&chart.currency))
                    .slot(
                        "code",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &account.id.to_string(),
                            href: &href,
                            class: "font-medium text-ink-strong hover:underline",
                            body: Html::text(&account.code),
                        })?,
                    )
                    .cell("name", Cell::text(&account.name))
                    .cell(
                        "kind",
                        Cell::text(if account.is_group { "Group" } else { "Account" }),
                    )
                    .cell("type", Cell::text(view::type_label(&account.account_type)))
                    .slot("group", group)
                    .slot("balance", shown(own.and_then(|r| r.balance.as_ref()))?)
                    .slot("total", shown(own.and_then(|r| r.total.as_ref()))?)
                    .slot(
                        "status",
                        if account.is_group {
                            Html::text("—")
                        } else {
                            bilibi_ui::active_badge(account.active)
                        },
                    )
                    .cell(
                        "retained",
                        Cell::text(if account.retained_earnings {
                            "Yes"
                        } else {
                            "—"
                        }),
                    );
                row.dom_id = Some(format!("ledger-account-{}", account.id));
                row
            });
        }
        let mut table = FlexTable::new("ledger-accounts-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Chart of accounts";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            "Nothing matches. Change the filters, or add an account.",
        )));
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "ledger-accounts-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        ))?);
        body.push(&render(Card {
            id: "ledger-accounts-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        Ok(page.len())
    }

    fn form(&self, chart: &[view::Account]) -> Result<Html> {
        let draft = self.draft.as_ref().ok_or(Error::Internal)?;
        let type_word = view::type_label(&draft.account_type).to_lowercase();
        // Only an account group of the same type can hold the new account.
        let eligible = view::by_name(chart.iter().filter(|a| draft.can_group(a)));
        let mut parents = vec![Choice::new("No group", "")];
        parents.extend(eligible.iter().map(|a| view::account_choice(a)));
        let parent_hint = if eligible.is_empty() {
            format!(
                "Optional. This company has no {type_word} account group yet. Add one on Account groups."
            )
        } else {
            format!("Optional. An account group of the same type ({type_word}).")
        };
        // Designating a new account releases the account that holds retained
        // earnings now, so the dialog says which one before the person ticks it.
        let retained_hint = if !draft.equity() {
            "Only an equity account can hold retained earnings.".to_owned()
        } else {
            match chart.iter().find(|a| a.retained_earnings) {
                Some(holder) => format!(
                    "The year-end close posts profit or loss here. This replaces {}, which holds retained earnings now.",
                    view::account_label(holder)
                ),
                None => "The year-end close posts profit or loss here.".to_owned(),
            }
        };
        let errors = |name: &str| {
            draft
                .refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect::<Vec<_>>()
        };
        let field = |key: &str, label: &str, value: &str| {
            Field::in_form("account", key)
                .id(format!("ledger-account-{key}"))
                .label(label)
                .value(value)
                .errors(errors(key))
                .attr("data-debounce", "300")
        };
        let mut body = Html::EMPTY;
        if !draft.notice.is_empty() {
            body.push(&render(Alert {
                id: "ledger-account-notice",
                kind: StatusKind::Error,
                body: Html::text(&draft.notice),
                ..Alert::default()
            })?);
        }
        let base = draft.refusal.alert(&[
            "code",
            "name",
            "account_type",
            "parent_id",
            "retained_earnings",
        ]);
        if !base.is_empty() {
            body.push(&render(Alert {
                id: "ledger-account-base",
                kind: StatusKind::Error,
                body: Html::text(&base.join(" ")),
                ..Alert::default()
            })?);
        }
        for f in [
            field("code", "Code", &draft.code)
                .required()
                .hint("Up to 50 characters. The code cannot be changed later."),
            field("name", "Name", &draft.name).required(),
            field("account_type", "Type", &draft.account_type)
                .kind(InputKind::Select)
                .options(
                    AccountType::ALL
                        .iter()
                        .map(|t| Choice::new(view::type_label(t.as_str()), t.as_str()))
                        .collect(),
                )
                .hint("The type cannot be changed later."),
        ] {
            body.push(&render(f)?);
        }
        let parent = field("parent", "Group under", &draft.parent);
        body.push(&render(
            Combobox::new(parent.id.clone(), parent.name.clone())
                .label("Group under")
                .placeholder("No group")
                .value(&draft.parent)
                .options(parents)
                .errors(errors("parent_id"))
                .hint(parent_hint),
        )?);
        let mut retained = field("retained", "Use for retained earnings", "true")
            .errors(errors("retained_earnings"))
            .kind(InputKind::Checkbox)
            .checked(draft.retained && draft.equity())
            .hint(retained_hint);
        if !draft.equity() {
            // Greyed, with the line above saying what unlocks it; a disabled
            // box posts nothing, so no designation rides along.
            retained = retained.disabled();
        }
        body.push(&render(retained)?);
        let actions = {
            let mut a = render(Button {
                id: "ledger-account-cancel",
                button_type: "button",
                event: "close_modal",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            a.push(&render(Button {
                id: "ledger-account-create",
                button_type: "button",
                event: "create",
                variant: ButtonVariant::Primary,
                working: "Adding…",
                body: Html::text("Add account"),
                ..Button::default()
            })?);
            a
        };
        render(Form {
            id: "ledger-account-form",
            event: "validate",
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }
}

impl LiveState<Scope, Error> for Accounts {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    let list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    if list.filters["company"] != self.list.filters["company"] {
                        self.draft = None;
                        self.preview = None;
                    }
                    self.list = list;
                    if values.get("new").is_some() && self.draft.is_none() {
                        self.draft = Some(Draft::new());
                    }
                }
                LiveInput::Event(value) => match event_type(value)? {
                    event @ ("publish_chart"
                    | "propose_chart"
                    | "validate_publication"
                    | "review_publication"
                    | "commit_publication"
                    | "close_publication"
                    | "publication_grid") => self.publication_event(&r, value, event).await?,
                    "company_filter" => {
                        let company = params::form_values(value, "company_filter")
                            .get("company")
                            .cloned()
                            .unwrap_or_default();
                        let mut filters = self.list.filters_form();
                        filters.insert("company".into(), company);
                        self.list.apply_filters(&filters);
                        self.draft = None;
                        self.preview = None;
                    }
                    "filters" => {
                        let mut filters = params::form_values(value, FILTERS_FORM);
                        filters.insert("company".into(), self.list.filters["company"].clone());
                        self.list.apply_filters(&filters);
                    }
                    "page" => self.list.put_page(value["page"].as_str()),
                    "sort" => {
                        self.list.next_sort(value["sort"].as_str().unwrap_or(""));
                    }
                    "grid" => {
                        if let ColumnsOutcome::Sort(key) =
                            self.columns.handle(&params::string_values(value))
                        {
                            self.list.next_sort(&key);
                        }
                    }
                    "new" => {
                        if self
                            .pages
                            .allowed(r.scope, capability_for("create_account"))
                            .await?
                        {
                            self.draft = Some(Draft::new());
                        }
                    }
                    "close_modal" => self.draft = None,
                    "ask_template" => {
                        let key = value["template"].as_str().ok_or(Error::Invalid)?;
                        self.ask_template(&r, "ask_template", key, &[]).await?;
                    }
                    "choose_sections" => {
                        if let Some(key) = self.preview.as_ref().map(|p| p.key.clone()) {
                            let chosen: Vec<String> = self
                                .preview
                                .as_ref()
                                .into_iter()
                                .flat_map(|p| &p.sections)
                                .filter(|s| {
                                    value[format!("sections[{}]", s.key)].as_str() == Some("true")
                                })
                                .map(|s| s.key.clone())
                                .collect();
                            self.ask_template(&r, "choose_sections", &key, &chosen)
                                .await?;
                        }
                    }
                    "apply_template" => self.apply_template(&r).await?,
                    "cancel_template" => self.preview = None,
                    "validate" => {
                        if let Some(draft) = &mut self.draft {
                            draft.read(value);
                            draft.refusal = Refusal::default();
                            draft.notice.clear();
                        }
                    }
                    "create" => self.create(&r).await?,
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(r.scope).await
        })
    }
}

impl Accounts {
    /// What an empty chart offers: each starter chart to preview, or an empty
    /// start (the header's New account, and Account groups). The chart is
    /// never filled in before a person chooses.
    async fn starters(&mut self, scope: &Scope, company: i64) -> Result<Html> {
        if self.preview.is_some() {
            return self.preview_card();
        }
        let response = match self
            .pages
            .read_in(
                scope,
                company,
                "finance.ledger.chart_template.list",
                json!({}),
            )
            .await?
        {
            Read::Found(value) => value,
            _ => json!({}),
        };
        let mut listed = view::parse::<Vec<view::TemplateSummary>>(response["records"].clone())
            .unwrap_or_default();
        let proposed = response["proposal"]["configured"].as_str();
        listed.sort_by_key(|t| Some(t.key.as_str()) != proposed);
        let mut body = render(SectionHeading {
            id: "ledger-starters-heading",
            title: "Start your chart of accounts",
            ..SectionHeading::default()
        })?;
        body.push(&render(Note {
            id: "ledger-starters-intro",
            text: "The chart is empty. Preview a starter chart and apply it, or start empty and add account groups and accounts yourself. Nothing is added until you apply.",
        })?);
        if proposed.is_some() && !listed.iter().any(|t| Some(t.key.as_str()) == proposed) {
            body.push(&render(Alert {id:"ledger-starter-unavailable",kind:StatusKind::Warning,body:Html::text("The proposed starter chart is unavailable. Choose another template or ask an administrator to change the proposal."),..Alert::default()})?);
        }
        let mut html = render(Card {
            id: "ledger-starters-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-starters-heading",
            body,
            ..Card::default()
        })?;
        for (index, template) in listed.iter().filter(|t| t.applicable).enumerate() {
            let mut card = render(SectionHeading {
                id: &format!("ledger-starter-{index}-heading"),
                title: &template.name,
                ..SectionHeading::default()
            })?;
            if Some(template.key.as_str()) == proposed {
                card.push(&render(Badge::status(
                    BadgeKind::Neutral,
                    "Proposed starter chart",
                ))?);
            }
            card.push(&render(Badge::status(
                BadgeKind::Warning,
                &template.review,
            ))?);
            card.push(&render(Note {
                id: &format!("ledger-starter-{index}-description"),
                text: &template.description,
            })?);
            card.push(&render(Note {
                id: &format!("ledger-starter-{index}-counts"),
                text: &format!(
                    "Creates {} account groups and {} accounts.{} You can rename them afterwards.",
                    template.groups,
                    template.accounts,
                    if template.sections.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Optional, chosen in the preview: {}.",
                            template
                                .sections
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
            })?);
            card.push(&render(Button {
                id: &format!("ledger-starter-{index}-preview"),
                button_type: "button",
                event: "ask_template",
                name: "template",
                value: &template.key,
                class: "mt-4",
                variant: ButtonVariant::Primary,
                body: Html::text("Preview this chart"),
                ..Button::default()
            })?);
            html.push(&render(Card {
                id: &format!("ledger-starter-{index}-card"),
                inner_class: "p-5 sm:p-6",
                role: "region",
                labelledby: &format!("ledger-starter-{index}-heading"),
                body: card,
                ..Card::default()
            })?);
        }
        Ok(html)
    }

    /// The exact groups and accounts the contract's review says it would
    /// create, with the choice to apply them or leave the chart empty.
    fn preview_card(&mut self) -> Result<Html> {
        let Some(preview) = &self.preview else {
            return Ok(Html::EMPTY);
        };
        let by_code: HashMap<&str, &view::TemplateRow> =
            preview.rows.iter().map(|r| (r.code.as_str(), r)).collect();
        self.template_columns
            .load(&preview.rows, |r| r.code.clone());
        let rows: Vec<FlexRow> = preview
            .rows
            .iter()
            .map(|row| {
                let group = row
                    .parent_code
                    .as_deref()
                    .and_then(|c| by_code.get(c))
                    .map_or_else(|| "—".to_owned(), |p| format!("{} · {}", p.name, p.code));
                FlexRow::new(row.code.clone())
                    .cell("code", Cell::text(&row.code))
                    .cell("name", Cell::text(&row.name))
                    .cell(
                        "kind",
                        Cell::text(if row.is_group { "Group" } else { "Account" }),
                    )
                    .cell("type", Cell::text(view::type_label(&row.account_type)))
                    .cell("group", Cell::text(&group))
                    .cell(
                        "retained",
                        Cell::text(if row.retained_earnings { "Yes" } else { "—" }),
                    )
            })
            .collect();
        let mut table = FlexTable::new(
            "ledger-template-table",
            self.template_columns.flex_columns(),
            rows,
        );
        table.caption = "Groups and accounts this chart would create";
        table.framed = false;
        table.zoom = self.template_columns.zoom;
        table.suggestions = self.template_columns.flex_suggestions();
        table.add_query = &self.template_columns.add_query;
        let mut body = render(SectionHeading {
            id: "ledger-preview-heading",
            title: &preview.name,
            ..SectionHeading::default()
        })?;
        body.push(&render(Badge::status(BadgeKind::Warning, &preview.status))?);
        body.push(&render(Note {
            id: "ledger-preview-note",
            text: "Review every group and account below. The core chart is always created; tick the optional sections that apply to this business and the list shows exactly what would be created. Nothing is ticked to start. Applying creates them as this company's own chart, with the retained earnings account marked. The chart is never changed once it has accounts.",
        })?);
        if !preview.sections.is_empty() {
            let mut choices = Html::EMPTY;
            for section in &preview.sections {
                choices.push(&render(
                    Field::in_form("sections", &section.key)
                        .id(format!("ledger-section-{}", section.key))
                        .label(&section.name)
                        .value("true")
                        .kind(InputKind::Checkbox)
                        .checked(section.chosen)
                        .hint(section.description.clone()),
                )?);
            }
            body.push(&render(Form {
                id: "ledger-sections-form",
                event: "choose_sections",
                change: true,
                class: "mt-4",
                body: choices,
                actions: Html::EMPTY,
            })?);
        }
        for (index, note) in preview.notes.iter().enumerate() {
            body.push(&render(Note {
                id: &format!("ledger-preview-note-{index}"),
                text: note,
            })?);
        }
        let mut actions = render(Button {
            id: "ledger-preview-cancel",
            button_type: "button",
            event: "cancel_template",
            body: Html::text("Start empty instead"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "ledger-preview-apply",
            button_type: "button",
            event: "apply_template",
            variant: ButtonVariant::Primary,
            working: "Applying…",
            body: Html::text("Apply this chart"),
            ..Button::default()
        })?);
        body.push(&render(Actions { buttons: actions })?);
        let mut html = render(Card {
            id: "ledger-preview-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-preview-heading",
            body,
            ..Card::default()
        })?;
        html.push(&render(Card {
            id: "ledger-preview-rows",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?);
        Ok(html)
    }

    async fn ask_template(
        &mut self,
        r: &LiveRequest<'_, Scope>,
        event: &str,
        key: &str,
        sections: &[String],
    ) -> Result<()> {
        let Some(company) = self.target(r.scope).await? else {
            self.flash.error("Select one company you can work in.");
            return Ok(());
        };
        match self
            .pages
            .review_in(
                r.scope,
                company,
                r.credential,
                event,
                TEMPLATE_CONTRACT,
                json!({"template": key, "sections": sections}),
            )
            .await?
        {
            Ok(review) => {
                let rows = view::parse::<Vec<view::TemplateRow>>(review.facts["rows"].clone())
                    .unwrap_or_default();
                self.preview = Some(Preview {
                    key: key.to_owned(),
                    sections: view::parse(review.facts["sections"].clone()).unwrap_or_default(),
                    name: review.facts["template"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    status: review.facts["template"]["review"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    notes: view::parse(review.facts["template"]["notes"].clone())
                        .unwrap_or_default(),
                    rows,
                    review,
                });
            }
            Err(failure) => self.flash.error(failure.text("The starter chart")),
        }
        Ok(())
    }

    async fn apply_template(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(preview) = self.preview.take() else {
            return Ok(());
        };
        match self
            .pages
            .commit(
                r.scope,
                r.credential,
                "apply_template",
                TEMPLATE_CONTRACT,
                preview.review,
            )
            .await?
        {
            Ok(done) => {
                let record = &done["results"][0]["result"]["result"]["record"];
                self.flash.success(format!(
                    "Chart applied: {} account groups and {} accounts.",
                    record["groups"].as_u64().unwrap_or_default(),
                    record["accounts"].as_u64().unwrap_or_default()
                ));
            }
            Err(failure) => self.flash.error(failure.text("The starter chart")),
        }
        Ok(())
    }

    async fn create(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(company) = self.target(r.scope).await? else {
            self.flash.error("Select one company you can work in.");
            return Ok(());
        };
        let Some(draft) = &mut self.draft else {
            return Ok(());
        };
        match self
            .pages
            .submit_in(
                r.scope,
                company,
                r.credential,
                "create",
                "finance.ledger.account.create",
                draft.input(),
            )
            .await?
        {
            Ok(done) => {
                let code = done["result"]["record"]["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.draft = None;
                self.flash.success(format!("Account {code} added."));
            }
            Err(Failure::Refused(refusal)) => {
                draft.refusal = refusal;
                draft.notice.clear();
            }
            Err(failure) => {
                draft.notice = failure.text("The account");
            }
        }
        Ok(())
    }
}

/// A short explanatory line under a heading.
#[derive(askama::Template)]
#[template(
    source = r#"<p id="{{ id }}" class="mt-2 text-sm text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
pub(super) struct Note<'a> {
    pub(super) id: &'a str,
    pub(super) text: &'a str,
}

/// Actions at the end of the row, as dialogs end theirs.
#[derive(askama::Template)]
#[template(
    source = r#"<div class="mt-4 flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct Actions {
    buttons: Html,
}
