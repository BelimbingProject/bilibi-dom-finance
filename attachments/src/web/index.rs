//! `/finance/attachments` (Bilimbi `Finance.Attachments.Web.IndexLive`): the
//! company's source documents by number with the record each supports, its
//! file, SHA-256, retain-until and capture, a download link each, and the
//! company's retention setting with its change history. The list is filtered
//! by the kind of record and by standing; lengthening the retention is a
//! facade call that re-asks its capability.
//!
//! Attaching a file happens on the record's own page through the panel
//! (`panel.rs`).
use super::{download::company_of, format, time_cell, DENIED};
use crate::{
    AttachmentError, AttachmentQuery, Attachments, FieldError, Retention, RetentionChange, Status,
    MANAGE_RETENTION,
};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Cell, Choice, ColumnType, EmptyState, Field, Filter,
    FlashMap, FlexRow, FlexTable, Html, InputKind, ListColumns, ListSpec, ListState, StatusKind,
    TableEmpty, NO_FLASH,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

/// Bilimbi's `@page`: rows per request; "Show more" asks for the next.
const PAGE: i64 = 200;
const ATTACHMENTS_EVENT: &str = "attachments_grid";
const CHANGES_EVENT: &str = "retention_grid";

/// Bilimbi's `filter` form has no search, sort or pager; the two selects keep
/// to the URL so Back and reload restore them.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        filters: [
            ("source".to_owned(), Filter::string("")),
            (
                "status".to_owned(),
                Filter::one_of(&["standing", "replaced", "removed", "all"], "standing"),
            ),
        ]
        .into(),
        page_sizes: vec![PAGE as u64],
        default_page_size: PAGE as u64,
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("source documents list spec")
});

fn attachment_columns() -> Vec<BuiltinColumn> {
    let mut number = BuiltinColumn::new("number", "#");
    number.kind = ColumnType::Integer;
    let mut dated = BuiltinColumn::new("dated", "Dated");
    dated.kind = ColumnType::Date;
    let mut captured = BuiltinColumn::new("captured", "Captured");
    captured.kind = ColumnType::Datetime;
    vec![
        number,
        BuiltinColumn::new("record", "Record"),
        dated,
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("sha256", "SHA-256"),
        BuiltinColumn::new("kept_until", "Kept until"),
        captured,
        BuiltinColumn::new("standing", "Current"),
    ]
}

fn change_columns() -> Vec<BuiltinColumn> {
    let mut number = BuiltinColumn::new("number", "#");
    number.kind = ColumnType::Integer;
    let mut years = BuiltinColumn::new("years", "Years").right();
    years.kind = ColumnType::Integer;
    let mut changed = BuiltinColumn::new("changed", "Changed");
    changed.kind = ColumnType::Datetime;
    vec![
        number,
        years,
        BuiltinColumn::new("reason", "Reason"),
        changed,
    ]
}

/// The module's page adapter, shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub attachments: Arc<Attachments>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct RetentionView {
    years: i32,
    since: Option<Html>,
    reason: String,
}

struct RetentionForm {
    years: Field,
    reason: Field,
}

#[derive(Template)]
#[template(path = "index.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    retention: Option<RetentionView>,
    retention_form: Option<RetentionForm>,
    changes: Html,
    count: Option<usize>,
    filter: Html,
    table: Html,
    more: bool,
}

#[derive(Template)]
#[template(
    source = r#"{% import "forms.html" as form %}<form id="attachments-filter" data-event="filter" data-change class="flex gap-2">{% call form::input(field=source) %}{% endcall %}{% call form::input(field=status) %}{% endcall %}</form>"#,
    ext = "html"
)]
struct FilterForm {
    source: Field,
    status: Field,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// A refusal told as the page tells it; a failure is not a refusal.
fn told<T>(
    result: std::result::Result<T, AttachmentError>,
) -> Result<std::result::Result<T, String>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(error) => Ok(Err(format::refusal(error)?)),
    }
}

/// Bilimbi renders an unreadable list as an empty one; only a failure is raised.
fn or_empty<T: Default>(result: std::result::Result<T, AttachmentError>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(AttachmentError::Failed(error)) => Err(error),
        Err(_) => Ok(T::default()),
    }
}

struct Index {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    change_columns: ListColumns,
    /// The attachments shown: `pages` requests of [`PAGE`] rows.
    shown: Vec<crate::Attachment>,
    pages_loaded: usize,
    more: bool,
    draft_years: String,
    draft_reason: String,
    flash: Option<(StatusKind, String)>,
}

impl Index {
    fn new(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("attachments", attachment_columns())
                .map_err(|_| Error::Internal)?,
            change_columns: ListColumns::mount("attachments-retention-changes", change_columns())
                .map_err(|_| Error::Internal)?,
            shown: Vec::new(),
            pages_loaded: 1,
            more: false,
            draft_years: String::new(),
            draft_reason: String::new(),
            flash: None,
        })
    }

    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    fn query(&self, after: Option<i64>) -> AttachmentQuery {
        let source = self.list.filters.get("source").filter(|s| !s.is_empty());
        AttachmentQuery {
            source: source.cloned(),
            document_ids: None,
            status: self.list.filters.get("status").cloned(),
            limit: Some(PAGE),
            after,
        }
    }

    /// Reads the first `pages_loaded` requests again (a filter resets it to one).
    async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.shown.clear();
        self.more = false;
        let Some(company) = company_of(scope) else {
            return Ok(());
        };
        for _ in 0..self.pages_loaded {
            let after = self.shown.last().map(|a| a.attachment_number);
            let page = or_empty(
                self.pages
                    .attachments
                    .list_company_attachments(scope, company, &self.query(after))
                    .await,
            )?;
            self.more = page.len() as i64 == PAGE;
            self.shown.extend(page);
            if !self.more {
                break;
            }
        }
        Ok(())
    }

    async fn more(&mut self, scope: &Scope) -> Result<()> {
        let Some(company) = company_of(scope) else {
            return Ok(());
        };
        let Some(last) = self.shown.last().map(|a| a.attachment_number) else {
            self.more = false;
            return Ok(());
        };
        let page = or_empty(
            self.pages
                .attachments
                .list_company_attachments(scope, company, &self.query(Some(last)))
                .await,
        )?;
        self.more = page.len() as i64 == PAGE;
        self.pages_loaded += 1;
        self.shown.extend(page);
        Ok(())
    }

    async fn can_manage(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        Ok(bilibi_authz::decide(&**client, scope, MANAGE_RETENTION)
            .await?
            .allowed)
    }

    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        self.pages
            .display
            .display(&**client, scope, None, locale)
            .await
    }

    fn attachment_table(&self, display: &DisplayContext) -> Result<Html> {
        let mut rows = Vec::with_capacity(self.shown.len());
        for a in &self.shown {
            let mut row = FlexRow::new(a.id.to_string());
            row.dom_id = Some(format!("attachment-{}", a.id));
            let record = format!(
                "{} · {}",
                a.source_label.as_deref().unwrap_or(&a.source),
                a.document_label
            );
            let date = a.document_date.to_string();
            let file = render(FileCell {
                id: a.id,
                name: &a.file_name,
                size: format::bytes(a.byte_size),
            })?;
            let hash = render(HashCell {
                full: &a.sha256,
                short: format::short_hash(&a.sha256),
            })?;
            let kept = render(KeptCell {
                until: format::retain_until(a.retain_until),
                on_hold: a.on_hold,
            })?;
            let standing = render(Badge {
                kind: if a.status == Status::Standing {
                    BadgeKind::Success
                } else {
                    BadgeKind::Neutral
                },
                dot: true,
                class: "",
                body: Html::text(match a.status {
                    Status::Standing => "Current",
                    Status::Replaced => "Replaced",
                    Status::Removed => "Removed",
                }),
            })?;
            row = row
                .cell("number", Cell::text(a.attachment_number.to_string()))
                .cell("record", Cell::text(record))
                .cell("dated", Cell::date(&date, &date))
                .slot("file", file)
                .cell("sha256", Cell::text(a.sha256.clone()))
                .slot("sha256", hash)
                .slot("kept_until", kept)
                .cell("captured", time_cell(display, a.captured_at))
                .slot("standing", standing);
            rows.push(row);
        }
        let mut table = FlexTable::new("attachments-table", self.columns.flex_columns(), rows);
        table.event = ATTACHMENTS_EVENT;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Attachments";
        if self.shown.is_empty() {
            table.empty = Some(TableEmpty::State(EmptyState::missing(
                "No source documents",
                "Nothing matching has been attached to this company's records.",
            )));
        }
        render(table)
    }

    fn change_table(&self, changes: &[RetentionChange], display: &DisplayContext) -> Result<Html> {
        if changes.is_empty() {
            return Ok(Html::EMPTY);
        }
        let rows = changes
            .iter()
            .map(|c| {
                let mut row = FlexRow::new(c.id.to_string());
                row.dom_id = Some(format!("retention-change-{}", c.id));
                row.cell("number", Cell::text(c.change_number.to_string()))
                    .cell("years", Cell::text(c.years.to_string()))
                    .cell("reason", Cell::text(c.reason.clone()))
                    .cell("changed", time_cell(display, c.changed_at))
            })
            .collect();
        let mut table = FlexTable::new(
            "attachments-retention-changes",
            self.change_columns.flex_columns(),
            rows,
        );
        table.event = CHANGES_EVENT;
        table.zoom = self.change_columns.zoom;
        table.suggestions = self.change_columns.flex_suggestions();
        table.add_query = &self.change_columns.add_query;
        table.framed = false;
        table.caption = "Retention changes";
        render(table)
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let display = self.display(scope).await?;
        let company = company_of(scope);
        let can_manage = self.can_manage(scope).await?;

        let sources = match company {
            Some(company) => {
                let mut sources: Vec<(String, String)> = or_empty(
                    self.pages
                        .attachments
                        .list_sources(scope, company)
                        .await
                        .map(|kinds| {
                            kinds
                                .into_iter()
                                .map(|k| {
                                    (k.source_label.unwrap_or_else(|| k.source.clone()), k.source)
                                })
                                .collect()
                        }),
                )?;
                sources.sort();
                sources
            }
            None => Vec::new(),
        };
        let (retention, changes): (Option<Retention>, Vec<RetentionChange>) = match company {
            Some(company) => {
                let retention = self.pages.attachments.get_retention(scope, company).await;
                let changes = self
                    .pages
                    .attachments
                    .list_retention_changes(scope, company)
                    .await;
                match (retention, changes) {
                    (Ok(retention), Ok(mut changes)) => {
                        changes.reverse();
                        (Some(retention), changes)
                    }
                    (Err(AttachmentError::Failed(e)), _) | (_, Err(AttachmentError::Failed(e))) => {
                        return Err(e);
                    }
                    _ => (None, Vec::new()),
                }
            }
            None => (None, Vec::new()),
        };

        let retention_view = match &retention {
            Some(r) => Some(RetentionView {
                years: r.years,
                since: match &r.change {
                    Some(change) => Some(super::time_html(
                        &display,
                        "attachments-retention-since",
                        change.changed_at,
                    )?),
                    None => None,
                },
                reason: r
                    .change
                    .as_ref()
                    .map(|c| c.reason.clone())
                    .unwrap_or_default(),
            }),
            None => None,
        };
        let retention_form = can_manage.then(|| RetentionForm {
            years: Field::in_form("retention", "years")
                .id("retention_years")
                .kind(InputKind::Number)
                .label("Years")
                .attr("min", "1")
                .attr("max", "100")
                .required()
                .value(self.draft_years.clone()),
            reason: Field::in_form("retention", "reason")
                .id("retention_reason")
                .label("Reason")
                .attr("maxlength", "500")
                .required()
                .value(self.draft_reason.clone()),
        });

        let filter = render(FilterForm {
            source: Field::named("filter[source]")
                .id("attachments-filter-source")
                .kind(InputKind::Select)
                .prompt("Every kind of record")
                .options(
                    sources
                        .iter()
                        .map(|(l, v)| Choice::new(l.clone(), v.clone()))
                        .collect(),
                )
                .value(self.list.filters.get("source").cloned().unwrap_or_default()),
            status: Field::named("filter[status]")
                .id("attachments-filter-status")
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("Current", "standing"),
                    Choice::new("Replaced", "replaced"),
                    Choice::new("Removed", "removed"),
                    Choice::new("All", "all"),
                ])
                .value(
                    self.list
                        .filters
                        .get("status")
                        .cloned()
                        .unwrap_or_else(|| "standing".into()),
                ),
        })?;

        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            flash,
            retention: retention_view,
            retention_form,
            changes: self.change_table(&changes, &display)?,
            count: Some(self.shown.len()),
            filter,
            table: self.attachment_table(&display)?,
            more: self.more,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    async fn set_retention(&mut self, scope: &Scope) -> Result<()> {
        if !self.can_manage(scope).await? {
            self.flash(StatusKind::Error, DENIED);
            return Ok(());
        }
        let Some(company) = company_of(scope) else {
            self.flash(
                StatusKind::Error,
                "This company cannot be read with your current access.",
            );
            return Ok(());
        };
        // Bilimbi casts the years: blank is not a change, anything else that
        // is not a whole number is invalid, and the changeset says so after
        // the reason.
        let years = self.draft_years.trim();
        let Ok(years) = years.parse::<i32>() else {
            let mut errors = crate::operations::reason_errors(&self.draft_reason);
            errors.push(FieldError::new(
                "years",
                if years.is_empty() {
                    "can't be blank"
                } else {
                    "is invalid"
                },
            ));
            self.flash(
                StatusKind::Error,
                format::refusal(AttachmentError::Invalid(errors))?,
            );
            return Ok(());
        };
        match told(
            self.pages
                .attachments
                .set_retention(scope, company, years, &self.draft_reason)
                .await,
        )? {
            Ok(retention) => {
                self.flash(
                    StatusKind::Success,
                    format!(
                        "Source documents are now kept {} years after the end of their financial year.",
                        retention.years
                    ),
                );
                self.draft_years.clear();
                self.draft_reason.clear();
            }
            Err(message) => self.flash(StatusKind::Error, message),
        }
        Ok(())
    }

    /// One live event; returns whether the rows must be read again.
    async fn event(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<bool> {
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filter" => {
                self.list
                    .apply_filters(&params::form_values(value, "filter"));
                self.pages_loaded = 1;
                Ok(true)
            }
            "more" => {
                self.more(scope).await?;
                Ok(false)
            }
            "retention_change" => {
                let posted = params::form_values(value, "retention");
                self.draft_years = posted.get("years").cloned().unwrap_or_default();
                self.draft_reason = posted.get("reason").cloned().unwrap_or_default();
                Ok(false)
            }
            "set_retention" => {
                let posted = params::form_values(value, "retention");
                self.draft_years = posted.get("years").cloned().unwrap_or_default();
                self.draft_reason = posted.get("reason").cloned().unwrap_or_default();
                self.set_retention(scope).await?;
                // A recorded change reads the page again (Bilimbi `load/1`).
                Ok(matches!(self.flash, Some((StatusKind::Success, _))))
            }
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| get("key") == Some(kind.name()))
                {
                    self.flash = None;
                }
                Ok(false)
            }
            "attachments_grid" => {
                let posted = params::string_values(value);
                self.columns.handle(&posted);
                Ok(false)
            }
            "retention_grid" => {
                let posted = params::string_values(value);
                self.change_columns.handle(&posted);
                Ok(false)
            }
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Template)]
#[template(
    source = r#"<a id="attachment-{{ id }}-download" href="/finance/attachments/{{ id }}/download" download="{{ name }}">{{ name }}</a> <span class="text-ink-muted">({{ size }})</span>"#,
    ext = "html"
)]
struct FileCell<'a> {
    id: i64,
    name: &'a str,
    size: String,
}

#[derive(Template)]
#[template(
    source = r#"<span class="font-mono" title="{{ full }}">{{ short }}</span>"#,
    ext = "html"
)]
struct HashCell<'a> {
    full: &'a str,
    short: String,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{{ until }}{% if on_hold %} {% call ui::badge() %}Legal hold{% endcall %}{% endif %}"#,
    ext = "html"
)]
struct KeptCell {
    until: String,
    on_hold: bool,
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Source documents")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut page = Index::new(self, request.query)?;
            page.reload(scope).await?;
            Ok(PageBody {
                html: page.body(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Index::new(self, request.query)?);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match request.input {
                LiveInput::Refresh => true,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.pages_loaded = 1;
                    true
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            };
            if reload || self.shown.is_empty() {
                self.reload(request.scope).await?;
            }
            let html = self.body(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
