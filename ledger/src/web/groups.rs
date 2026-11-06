//! Account groups: the rows of the chart that take no postings and stand for
//! the total of the accounts and groups under them. Managed here, apart from
//! the accounts they hold (`accounts.rs`); each group's page is `group.rs`.
use super::{
    account_name, capability_for, dialog_actions, error_text, event_type, figure, no_company,
    render, shell, unavailable, view, Failure, Flash, Pages, Refusal,
};
use crate::{AccountType, MANAGE_ACCOUNTS};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BuiltinColumn, Button, ButtonVariant, Card, Cell, Choice, ColumnsOutcome,
    Combobox, CreateButton, EmptyState, Field, Filter, FilterToolbar, FlexRow, FlexTable, Form,
    Header, Html, InputKind, ListColumns, ListSpec, ListState, Modal, PageCounts, PageVariant,
    Pagination, RecordLink, SortDir, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 4] = [25, 50, 100, 300];
pub(super) const INDEX: &str = "/finance/ledger/account-groups";

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
        filters: BTreeMap::from([(
            "type".into(),
            Filter::one_of(
                &["all", "asset", "liability", "equity", "income", "expense"],
                "all",
            ),
        )]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("account groups list spec")
});

/// The new-group dialog's values and what the ledger said about them.
#[derive(Default)]
struct Draft {
    code: String,
    name: String,
    account_type: String,
    parent: String,
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
        let fields = params::form_values(value, "group");
        self.code = fields.get("code").cloned().unwrap_or_default();
        self.name = fields.get("name").cloned().unwrap_or_default();
        self.account_type = fields.get("account_type").cloned().unwrap_or_default();
        self.parent = fields.get("parent").cloned().unwrap_or_default();
    }
    /// Drops a group of another type once the type changes; the ledger's own
    /// check still decides.
    fn reconcile(&mut self, chart: &[view::Account]) {
        let eligible = self
            .parent
            .parse::<i64>()
            .ok()
            .and_then(|id| chart.iter().find(|a| a.id == id))
            .is_some_and(|parent| parent.is_group && parent.account_type == self.account_type);
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
        input
    }
}

pub(super) struct Groups {
    company: Option<i64>,
    pinned: bool,
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    flash: Flash,
    draft: Option<Draft>,
    framed: bool,
}

impl Groups {
    pub(super) fn new(
        pages: Pages,
        query: &HashMap<String, String>,
        framed: bool,
        company: Option<i64>,
    ) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("code", "Code").sortable("code"),
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("type", "Type").sortable("type"),
            BuiltinColumn::new("group", "Group"),
            BuiltinColumn::new("holds", "Holds"),
            BuiltinColumn::new("total", "Total"),
        ];
        Ok(Self {
            company,
            pinned: query.contains_key("company"),
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("ledger-groups-table", columns)
                .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            draft: query.get("new").is_some().then(Draft::new),
            framed,
        })
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let can_manage = self.pages.allowed(scope, MANAGE_ACCOUNTS).await?;
        let mut body = Html::EMPTY;
        let mut actions = Html::EMPTY;
        let mut rows_shown = 0;
        let mut chart: Vec<view::Account> = Vec::new();
        if self.company.is_none() {
            body.push(&no_company("ledger-groups-no-company")?);
        } else {
            let company = self.company.ok_or(Error::Invalid)?;
            match self.pages.chart_in(scope, company).await? {
                Err(refused) => body.push(&unavailable(
                    "ledger-groups-refused",
                    "the account groups",
                    &refused,
                )?),
                Ok(listed) => {
                    chart = listed;
                    let book_open = self
                        .pages
                        .book_in(scope, company)
                        .await?
                        .ok()
                        .is_some_and(|b| b.book.is_some());
                    if can_manage {
                        actions.push(&render(CreateButton {
                            noun: "Group",
                            id: "ledger-group-new",
                            class: "text-xs",
                            event: "new",
                            button_type: "button",
                            ..CreateButton::default()
                        })?);
                    }
                    let figures = if book_open {
                        self.pages.chart_figures_in(scope, company).await?
                    } else {
                        None
                    };
                    rows_shown =
                        self.table(&chart, figures.as_ref(), &mut body, company, can_manage)?;
                }
            }
        }
        let company_name = match self.company {
            Some(company) => self.pages.company_name_in(scope, company).await?,
            None => String::new(),
        };
        let mut page = render(Header {
            subtitle: &company_name,
            title: Html::text("Account groups"),
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
        })?;
        page.push(&body);
        if let Some(draft) = &mut self.draft {
            draft.reconcile(&chart);
        }
        if self.draft.is_some() && can_manage {
            let form = self.form(&chart)?;
            page.push(&render(Modal {
                id: "ledger-group-modal",
                title: "New account group",
                description: &format!("Add an account group to {company_name}."),
                cancel_event: "close_modal",
                body: form,
                ..Modal::default()
            })?);
        }
        let mut query = self.list.to_params();
        if let Some(company) = self.company.filter(|_| self.pinned) {
            query.insert("company".into(), company.to_string());
        }
        if self.draft.is_some() {
            query.insert("new".into(), "1".into());
        }
        Ok(LiveBody {
            html: shell("ledger-groups", PageVariant::List, &self.flash, page)?,
            query,
            rows: rows_shown,
            ..LiveBody::default()
        })
    }

    fn table(
        &mut self,
        chart: &[view::Account],
        figures: Option<&view::ChartBalances>,
        body: &mut Html,
        company: i64,
        can_manage: bool,
    ) -> Result<usize> {
        let search = self.list.search.trim().to_lowercase();
        let by_id: HashMap<i64, &view::Account> = chart.iter().map(|a| (a.id, a)).collect();
        let mut shown: Vec<&view::Account> = chart
            .iter()
            .filter(|a| {
                let kind = self.list.filters["type"].as_str();
                a.is_group
                    && (kind == "all" || a.account_type == kind)
                    && (search.is_empty()
                        || a.code.to_lowercase().contains(&search)
                        || a.name.to_lowercase().contains(&search))
            })
            .collect();
        let descending = self.list.sort_dir == Some(SortDir::Desc);
        match self.list.sort_by.as_deref() {
            Some("name") => shown.sort_by_key(|a| a.name.to_lowercase()),
            Some("type") => shown.sort_by(|a, b| {
                (a.account_type.as_str(), a.code.as_str())
                    .cmp(&(b.account_type.as_str(), b.code.as_str()))
            }),
            _ => shown.sort_by(|a, b| a.code.cmp(&b.code)),
        }
        if descending {
            shown.reverse();
        }
        let total = shown.len() as u64;
        self.list
            .clamp_to_last_page(total.div_ceil(self.list.page_size), true);
        let start = ((self.list.page - 1) * self.list.page_size) as usize;
        let page: Vec<&view::Account> = shown
            .into_iter()
            .skip(start)
            .take(self.list.page_size as usize)
            .collect();
        self.columns.load(&page, |a| a.id.to_string());

        let form = self.list.filters_form();
        body.push(&render(FilterToolbar::new(
            "ledger-groups-filters",
            vec![
                ToolbarControl::search(
                    "ledger-groups-search",
                    "search",
                    "Search groups",
                    &form["search"],
                    "Search by code or name…",
                ),
                ToolbarControl::select(
                    "ledger-groups-type",
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
            ],
        ))?);

        let mut rows = Vec::new();
        for group in &page {
            let parent = match group.parent_id.and_then(|id| by_id.get(&id)) {
                Some(parent) => account_name(parent)?,
                None => Html::text("—"),
            };
            let held = chart.iter().filter(|a| a.parent_id == Some(group.id)).fold(
                (0, 0),
                |(accounts, groups), a| {
                    if a.is_group {
                        (accounts, groups + 1)
                    } else {
                        (accounts + 1, groups)
                    }
                },
            );
            let total = match (
                figures.and_then(|f| f.rows.iter().find(|r| r.id == group.id)),
                figures,
            ) {
                (Some(row), Some(f)) => match &row.total {
                    Some(total) => figure(&view::amount(total, f.minor_units))?,
                    None => Html::text("—"),
                },
                _ => Html::text("—"),
            };
            rows.push({
                let mut row = FlexRow::new(group.id.to_string())
                    .slot(
                        "code",
                        render(RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &group.id.to_string(),
                            href: &format!("{INDEX}/{}?company={company}", group.id),
                            class: "font-medium text-ink-strong hover:underline",
                            body: Html::text(&group.code),
                        })?,
                    )
                    .cell("name", Cell::text(&group.name))
                    .cell("type", Cell::text(view::type_label(&group.account_type)))
                    .slot("group", parent)
                    .cell("holds", Cell::text(holds(held.0, held.1)))
                    .slot("total", total);
                row.dom_id = Some(format!("ledger-group-{}", group.id));
                row
            });
        }
        let mut table = FlexTable::new("ledger-groups-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Account groups";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        // "Match" only when a filter is set; a chart with no group yet offers
        // the button that adds the first one.
        let filtered = !self.list.search.trim().is_empty() || self.list.filters["type"] != "all";
        table.empty = Some(TableEmpty::State(if filtered {
            EmptyState::missing("No account groups match these filters", "")
                .id("ledger-groups-empty")
        } else {
            let mut empty =
                EmptyState::missing("No account groups yet", "").id("ledger-groups-empty");
            if can_manage {
                empty = empty.action(render(CreateButton {
                    noun: "Group",
                    id: "ledger-group-new-empty",
                    event: "new",
                    button_type: "button",
                    ..CreateButton::default()
                })?);
            }
            empty
        }));
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "ledger-groups-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        ))?);
        body.push(&render(Card {
            id: "ledger-groups-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        Ok(page.len())
    }

    fn form(&self, chart: &[view::Account]) -> Result<Html> {
        let draft = self.draft.as_ref().ok_or(Error::Internal)?;
        let type_word = view::type_label(&draft.account_type).to_lowercase();
        let eligible = view::by_name(
            chart
                .iter()
                .filter(|a| a.is_group && a.account_type == draft.account_type),
        );
        let mut parents = vec![Choice::new("No group", "")];
        parents.extend(eligible.iter().map(|a| view::account_choice(a)));
        let parent_hint = if eligible.is_empty() {
            format!("Optional. This company has no {type_word} group to put it in yet.")
        } else {
            format!("Optional. Another group of the same type ({type_word}).")
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
            Field::in_form("group", key)
                .id(format!("ledger-group-{key}"))
                .label(label)
                .value(value)
                .errors(errors(key))
                .attr("data-debounce", "300")
        };
        let mut body = Html::EMPTY;
        if !draft.notice.is_empty() {
            body.push(&render(Alert {
                id: "ledger-group-notice",
                kind: StatusKind::Error,
                body: Html::text(&draft.notice),
                ..Alert::default()
            })?);
        }
        let base = draft
            .refusal
            .alert(&["code", "name", "account_type", "parent_id"]);
        if !base.is_empty() {
            body.push(&render(Alert {
                id: "ledger-group-base",
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
                .hint("The type cannot be changed later. A group takes no postings."),
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
        let actions = {
            let mut a = render(Button {
                id: "ledger-group-cancel",
                button_type: "button",
                event: "close_modal",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            a.push(&render(Button {
                id: "ledger-group-create",
                button_type: "button",
                event: "create_group",
                variant: ButtonVariant::Primary,
                working: "Adding…",
                body: Html::text("Add group"),
                ..Button::default()
            })?);
            a
        };
        render(Form {
            id: "ledger-group-form",
            event: "validate",
            change: true,
            class: "mt-4",
            body,
            actions: dialog_actions(actions)?,
        })
    }

    async fn create(&mut self, r: &LiveRequest<'_, Scope>) -> Result<()> {
        let Some(draft) = &mut self.draft else {
            return Ok(());
        };
        match self
            .pages
            .submit_in(
                r.scope,
                self.company.ok_or(Error::Invalid)?,
                r.credential,
                "create_group",
                "finance.ledger.account_group.create",
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
                self.flash.success(format!("Group {code} added."));
            }
            Err(Failure::Refused(refusal)) => {
                draft.refusal = refusal;
                draft.notice.clear();
            }
            Err(failure) => {
                draft.notice = failure.text("The group");
            }
        }
        Ok(())
    }
}

/// What a group directly holds, as a short phrase.
pub(super) fn holds(accounts: usize, groups: usize) -> String {
    let noun = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match (accounts, groups) {
        (0, 0) => "Nothing yet".to_owned(),
        (a, 0) => noun(a, "account", "accounts"),
        (0, g) => noun(g, "group", "groups"),
        (a, g) => format!(
            "{}, {}",
            noun(a, "account", "accounts"),
            noun(g, "group", "groups")
        ),
    }
}

impl LiveState<Scope, Error> for Groups {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    let company = super::target_company(values, Some(r.scope))?;
                    self.pinned = values.contains_key("company");
                    if company != self.company {
                        self.company = company;
                        self.draft = None;
                    }
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    if values.get("new").is_some() && self.draft.is_none() {
                        self.draft = Some(Draft::new());
                    }
                }
                LiveInput::Event(value) => match event_type(value)? {
                    "filters" => self
                        .list
                        .apply_filters(&params::form_values(value, FILTERS_FORM)),
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
                    "validate" => {
                        if let Some(draft) = &mut self.draft {
                            draft.read(value);
                            draft.refusal = Refusal::default();
                            draft.notice.clear();
                        }
                    }
                    "create_group" => self.create(&r).await?,
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(r.scope).await
        })
    }
}

/// The empty state a group's page shows for an id that is not one.
pub(super) fn missing() -> Result<Html> {
    render(
        EmptyState::missing(
            "This account group does not exist",
            "It may belong to another company. Go back to the account groups.",
        )
        .id("ledger-group-missing"),
    )
}
