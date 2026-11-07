//! `/finance/attachments` (Bilimbi `Finance.Attachments.Web.IndexLive`): the
//! company's source documents by number with the record each supports (linked
//! to its page), its file, retain-until and capture (the SHA-256 waits in the
//! column picker), a download link each, and the company's retention setting
//! with its change history folded away. The list is filtered in the shared
//! toolbar by the kind of record and by standing; lengthening the retention
//! is a dialog whose save is a facade call that re-asks its capability.
//!
//! Attaching a file happens on the record's own page through the panel
//! (`panel.rs`).
mod tables;
use super::{download::company_of, format, DENIED};
use crate::{
    AttachmentError, AttachmentQuery, Attachments, FieldError, Retention, RetentionChange,
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
    params, Alert, Button, ButtonVariant, Choice, Field, Filter, FilterToolbar, FlashMap, Html,
    InputKind, ListColumns, ListSpec, ListState, RecordField, StatusKind, ToolbarControl, NO_FLASH,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};
use tables::{attachment_columns, change_columns};

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

/// The "Lengthen retention" dialog.
struct RetentionForm {
    alert: Option<Html>,
    years: Field,
    reason: Field,
    actions: Html,
}

#[derive(Template)]
#[template(path = "index.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    retention: Option<RetentionView>,
    lengthen: Html,
    retention_form: Option<RetentionForm>,
    changes: Html,
    count: Option<usize>,
    filter: Html,
    table: Html,
    more: bool,
}

/// The dialog's actions: Cancel, then the one verb at the end of the row.
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct DialogActions {
    buttons: Html,
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
    /// The "Lengthen retention" dialog is open, with its refusal.
    lengthening: bool,
    dialog_alert: Option<String>,
    flash: Option<(StatusKind, String)>,
}

impl Index {
    fn new(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            // The fingerprint is for an auditor checking a file; it waits
            // in the column picker.
            columns: ListColumns::mount("attachments", attachment_columns())
                .map_err(|_| Error::Internal)?
                .with_record_fields([RecordField::new("sha256", "SHA-256")]),
            change_columns: ListColumns::mount("attachments-retention-changes", change_columns())
                .map_err(|_| Error::Internal)?,
            shown: Vec::new(),
            pages_loaded: 1,
            more: false,
            draft_years: String::new(),
            draft_reason: String::new(),
            lengthening: false,
            dialog_alert: None,
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
        let lengthen = if can_manage && retention.is_some() {
            render(Button {
                id: "attachments-retention-open",
                event: "open_retention",
                button_type: "button",
                body: Html::text("Lengthen retention"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut buttons = render(Button {
            id: "attachments-retention-cancel",
            event: "close_retention",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "attachments-retention-submit",
            variant: ButtonVariant::Primary,
            button_type: "submit",
            working: "Saving…",
            body: Html::text("Lengthen retention"),
            ..Button::default()
        })?);
        let actions = render(DialogActions { buttons })?;
        let alert = self
            .dialog_alert
            .as_ref()
            .map(|message| {
                render(Alert {
                    kind: StatusKind::Error,
                    id: "attachments-retention-alert",
                    class: "mt-4",
                    body: Html::text(message),
                })
            })
            .transpose()?;
        let retention_form = (can_manage && self.lengthening).then(|| RetentionForm {
            alert,
            actions,
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

        // The filters sit in the shared toolbar, the height of every list's.
        let mut kinds = vec![Choice::new("Every kind of record", "")];
        kinds.extend(
            sources
                .iter()
                .map(|(l, v)| Choice::new(l.clone(), v.clone())),
        );
        let mut toolbar = FilterToolbar::new(
            "attachments-filter",
            vec![
                ToolbarControl::select(
                    "attachments-filter-source",
                    "source",
                    "Filter by kind of record",
                    &self.list.filters.get("source").cloned().unwrap_or_default(),
                    kinds,
                ),
                ToolbarControl::select(
                    "attachments-filter-status",
                    "status",
                    "Filter by standing",
                    &self
                        .list
                        .filters
                        .get("status")
                        .cloned()
                        .unwrap_or_else(|| "standing".into()),
                    vec![
                        Choice::new("Current", "standing"),
                        Choice::new("Replaced", "replaced"),
                        Choice::new("Removed", "removed"),
                        Choice::new("All", "all"),
                    ],
                ),
            ],
        );
        toolbar.event = "filter";
        toolbar.class = "px-5";
        let filter = render(toolbar)?;

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
            lengthen,
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
            self.dialog_alert = Some(DENIED.into());
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
            self.dialog_alert = Some(format::refusal(AttachmentError::Invalid(errors))?);
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
                self.lengthening = false;
                self.dialog_alert = None;
            }
            // The dialog stays open with the refusal inside it.
            Err(message) => self.dialog_alert = Some(message),
        }
        Ok(())
    }

    /// One live event; returns whether the rows must be read again.
    async fn event(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<bool> {
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filter" => {
                self.list
                    .apply_filters(&params::form_values(value, "filters"));
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
            "open_retention" => {
                self.lengthening = true;
                self.dialog_alert = None;
                self.flash = None;
                Ok(false)
            }
            "close_retention" => {
                self.lengthening = false;
                self.dialog_alert = None;
                self.draft_years.clear();
                self.draft_reason.clear();
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
