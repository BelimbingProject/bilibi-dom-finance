//! Bilimbi Reports.Web.{BookkeepingLive,ApprovalExceptionsLive} at
//! 4abfaf829321f8540581d833752a086b21b45ba9. Ledger owns all reads and writes.
use crate::{Reports, VIEW};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{
    Clock, DisplayContext, DisplayPolicy, Format, Period, PeriodFilter, Precision,
};
use bilibi_finance_ledger::{
    ApprovalExceptionQuery, LedgerError, People, MANAGE_CONTROLLED_BOOKKEEPING,
    MANAGE_DUTY_SEPARATION, REVIEW_EXCEPTIONS,
};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, EmptyState, Field,
    Filter, FlashMap, FlexRow, FlexTable, Header, Html, InputKind, ListColumns, ListSpec,
    ListState, Modal, RowMode, SectionHeading, StatusKind, Tab, Tabs, Toggle, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
pub(crate) struct Pages {
    pub reports: Arc<Reports>,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
}

impl Pages {
    /// The names of the people a page mentions.
    async fn people(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Result<People> {
        let client = self.reports.db.get().await?;
        Ok(People::read(&self.directory, &**client, scope, ids).await)
    }

    async fn company_name(&self, scope: &Scope) -> Result<String> {
        let Some(company) = scope.current_company() else {
            return Ok(String::new());
        };
        let client = self.reports.db.get().await?;
        Ok(self
            .parties
            .organisation_name(&**client, scope, company)
            .await?
            .map(|c| c.name)
            .unwrap_or_else(|| "Company unavailable".into()))
    }
}

#[derive(Clone, Copy)]
enum Dialog {
    ControlledOff,
    Separation { required: bool },
    Review { id: i64 },
}

struct State {
    pages: Pages,
    bookkeeping: bool,
    company_name: String,
    framed: bool,
    list: ListState,
    columns: Vec<ListColumns>,
    browser_zone: Option<String>,
    period: Option<PeriodFilter>,
    dialog: Option<Dialog>,
    text: String,
    flash: Option<(StatusKind, String)>,
}

impl State {
    fn new(
        pages: Pages,
        operation: &str,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        let bookkeeping = match operation {
            "controls" => true,
            "approval-exceptions" => false,
            _ => return Err(Error::NotFound),
        };
        let mut spec = ListSpec {
            page_sizes: vec![100],
            default_page_size: 100,
            omit_defaults: true,
            ..ListSpec::default()
        };
        if !bookkeeping {
            spec.filters.insert(
                "review".into(),
                Filter::one_of(&["all", "pending", "reviewed"], "all"),
            );
            spec.filters.insert("after".into(), Filter::string(""));
            spec.filters.insert(
                "area".into(),
                Filter::one_of(&["journals", "other"], "journals"),
            );
            spec.filters.insert(
                "period".into(),
                Filter::one_of(
                    &[
                        "today",
                        "this_month",
                        "last_7_days",
                        "last_30_days",
                        "last_90_days",
                        "custom",
                    ],
                    "this_month",
                ),
            );
            for key in ["from", "to"] {
                spec.filters.insert(key.into(), Filter::string(""));
            }
        }
        let tables: &[(&str, &[(&str, &str)])] = if bookkeeping {
            &[(
                "controlled-bookkeeping-changes",
                &[
                    ("when", "When"),
                    ("set", "Set to"),
                    ("reason", "Reason"),
                    ("by", "By"),
                ],
            )]
        } else {
            &[
                (
                    "duty-separation-changes",
                    &[
                        ("when", "When"),
                        ("set", "Set to"),
                        ("reason", "Reason"),
                        ("by", "By"),
                    ],
                ),
                (
                    "exceptions-table",
                    &[
                        ("number", "#"),
                        ("when", "When"),
                        ("kind", "Kind"),
                        ("what", "What"),
                        ("prepared", "Prepared by"),
                        ("approved", "Approved by"),
                        ("reason", "Reason"),
                        ("review", "Review"),
                    ],
                ),
            ]
        };
        let mut columns: Vec<ListColumns> = tables
            .iter()
            .map(|(id, fields)| {
                ListColumns::mount(
                    id,
                    fields
                        .iter()
                        .map(|(key, label)| BuiltinColumn::new(key, label))
                        .collect(),
                )
                .map_err(|_| Error::Internal)
            })
            .collect::<Result<_>>()?;
        if !bookkeeping {
            columns.push(
                ListColumns::mount(
                    "journal-approval-exceptions",
                    vec![
                        BuiltinColumn::new("when", "When"),
                        BuiltinColumn::new("subject", "Journal"),
                        BuiltinColumn::new("who", "Approved by"),
                        BuiltinColumn::new("reason", "Reason"),
                        BuiltinColumn::new("review", "Review"),
                    ],
                )
                .map_err(|_| Error::Internal)?,
            );
        }
        Ok(Self {
            pages,
            bookkeeping,
            company_name: String::new(),
            framed,
            list: ListState::parse(query, spec.shared().map_err(|_| Error::Internal)?)
                .map_err(|_| Error::Invalid)?,
            columns,
            browser_zone: None,
            period: None,
            dialog: None,
            text: String::new(),
            flash: None,
        })
    }

    fn filter(&self, key: &str) -> &str {
        self.list.filters.get(key).map(String::as_str).unwrap_or("")
    }
    fn after(&self) -> Option<i64> {
        params::positive_id(Some(self.filter("after")))
    }
    fn reviewed(&self) -> Option<bool> {
        match self.filter("review") {
            "pending" => Some(false),
            "reviewed" => Some(true),
            _ => None,
        }
    }
    fn path(&self) -> &'static str {
        if self.bookkeeping {
            "/finance/reports/controls"
        } else {
            "/finance/reports/approval-exceptions"
        }
    }
    fn query(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        // ListState carries the cursor; only signed positive database numbers leave the page.
        query.remove("after");
        if let Some(after) = self.after() {
            query.insert("after".into(), after.to_string());
        }
        query
    }
    fn link(&self, key: &str, value: &str) -> String {
        let mut query = self.query();
        query.remove("after");
        if value.is_empty() || value == "all" {
            query.remove(key);
        } else {
            query.insert(key.into(), value.into());
        }
        params::address(self.path(), &query)
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.pages.reports.db.get().await?;
        match bilibi_company::authorize_work_target(&**client, scope, company(scope)?, capability)
            .await
        {
            Ok(_) => Ok(true),
            Err(Error::Forbidden | Error::Unauthorized | Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.pages.reports.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        match self
            .pages
            .display
            .display(&**client, scope, scope.current_company(), locale)
            .await
        {
            // TODO(platform): Settings' company proof requires an unarchived
            // company even for display reads (also see Reports::layout).
            // Ledger history stays readable and a refused write must leave
            // its page alive, so fall back to the user's tenant clock.
            Err(Error::Forbidden) => {
                self.pages
                    .display
                    .display(&**client, scope, None, locale)
                    .await
            }
            result => result,
        }
    }
    fn sync_period(&mut self, display: &DisplayContext) -> Clock {
        let clock = Clock::resolve(display, self.browser_zone.as_deref(), Timestamp::now());
        let period = Period::parse(|key| self.list.filters.get(key).cloned(), clock.today);
        if let Some(filter) = &mut self.period {
            filter.set(period, &clock);
        } else {
            self.period = Some(PeriodFilter::new("exceptions-period", period, &clock));
        }
        clock
    }
    fn close(&mut self) {
        self.dialog = None;
        self.text.clear();
    }
    fn refused(&mut self, error: LedgerError) {
        let invalid = matches!(error, LedgerError::Invalid(_));
        let text = if invalid {
            match self.dialog {
                Some(Dialog::Review { .. }) if !self.bookkeeping => {
                    "Keep the note to 500 characters.".into()
                }
                _ => "Give a reason of 1 to 500 characters.".into(),
            }
        } else {
            refusal(&error)
        };
        if !invalid {
            self.close();
        }
        self.flash = Some((StatusKind::Error, text));
    }
    fn success(&mut self, text: impl Into<String>) {
        self.close();
        self.flash = Some((StatusKind::Success, text.into()));
    }

    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        if scope.current_company().is_none() {
            self.close();
            self.flash = Some((
                StatusKind::Error,
                "Choose a current company to open its ledger.".into(),
            ));
            return Ok(());
        }
        let event = payload["type"].as_str().unwrap_or("");
        let dialog_kind = payload["dialog"].as_str().or_else(|| {
            (payload.get("entry").is_some() || payload.get("exception").is_some())
                .then_some("review")
        });
        if let Some(index) = event
            .strip_prefix("control_grid_")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if let Some(columns) = self.columns.get_mut(index) {
                columns.handle(&params::string_values(payload));
            }
            return Ok(());
        }
        if !self.bookkeeping {
            let display = self.display(scope).await?;
            let clock = self.sync_period(&display);
            if let Some(outcome) = self
                .period
                .as_mut()
                .and_then(|filter| filter.event(event, payload, &clock))
            {
                if let bilibi_datetime::filter::Outcome::Applied(period) = outcome {
                    self.list
                        .filters
                        .extend(period.params().into_iter().map(|(k, v)| (k.into(), v)));
                    self.list.filters.remove("after");
                }
                return Ok(());
            }
        }
        if event == "browser_timezone" {
            self.browser_zone = payload["timezone"]
                .as_str()
                .filter(|s| bilibi_datetime::valid_timezone(s))
                .map(str::to_owned);
            return Ok(());
        }
        if event == "clear-flash" {
            crate::web::clear_flash(&mut self.flash, payload);
            return Ok(());
        }
        if event == "close_dialog" {
            self.close();
            return Ok(());
        }
        if event == "text" {
            if let Some(text) = payload["dialog[text]"].as_str() {
                self.text = text.into();
            }
            return Ok(());
        }
        let capability = match event {
            "turn_on" | "turn_off" if self.bookkeeping => MANAGE_CONTROLLED_BOOKKEEPING,
            "change_separation" if !self.bookkeeping => MANAGE_DUTY_SEPARATION,
            "review" if !self.bookkeeping => REVIEW_EXCEPTIONS,
            "open_dialog" => match dialog_kind {
                Some("controlled-off") if self.bookkeeping => MANAGE_CONTROLLED_BOOKKEEPING,
                Some("separation") if !self.bookkeeping => MANAGE_DUTY_SEPARATION,
                Some("review") if !self.bookkeeping => REVIEW_EXCEPTIONS,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        if !self.allowed(scope, capability).await? {
            self.refused(LedgerError::Unauthorized);
            return Ok(());
        }
        let ledger = self.pages.reports.ledger.clone();
        let company = company(scope)?;
        self.flash = None;
        if event == "open_dialog" {
            self.close();
            self.dialog = match dialog_kind {
                Some("controlled-off") => Some(Dialog::ControlledOff),
                Some("separation") => match ledger.get_duty_separation(scope, company).await {
                    Ok(state) => Some(Dialog::Separation {
                        required: !state.required,
                    }),
                    Err(error) => {
                        self.refused(error);
                        None
                    }
                },
                Some("review") if !self.bookkeeping => {
                    params::positive_id(payload["exception"].as_str())
                        .map(|id| Dialog::Review { id })
                }
                _ => None,
            };
            return Ok(());
        }
        // Typed form values are kept intact for facade validation, including
        // 500-character Unicode reasons longer than generic form_values' 1 KiB.
        let text = match payload.get("dialog[text]") {
            Some(Value::String(text)) => text.clone(),
            None => self.text.clone(),
            _ => {
                self.refused(LedgerError::InvalidOption);
                return Ok(());
            }
        };
        self.text = text.clone();
        match (event, self.dialog) {
            ("turn_on", _) => match ledger
                .set_controlled_bookkeeping(scope, company, true, None)
                .await
            {
                Ok(_) => {
                    self.success("Bookkeeping is controlled: entries need an open fiscal period.")
                }
                Err(error) => self.refused(error),
            },
            ("turn_off", Some(Dialog::ControlledOff)) => match ledger
                .set_controlled_bookkeeping(scope, company, false, Some(&text))
                .await
            {
                Ok(_) => self
                    .success("Bookkeeping is not controlled; the change is listed as an override."),
                Err(error) => self.refused(error),
            },
            ("change_separation", Some(Dialog::Separation { required })) => match ledger
                .set_duty_separation(scope, company, required, Some(&text))
                .await
            {
                Ok(state) => self.success(if state.required {
                    "Separation of duties is required again."
                } else {
                    "Separation of duties is off; same-person approvals are listed here."
                }),
                Err(error) => self.refused(error),
            },
            ("review", Some(Dialog::Review { id })) => match ledger
                .review_approval_exception(scope, company, id, Some(&text))
                .await
            {
                Ok(exception) => self.success(format!(
                    "Review of exception {} recorded.",
                    exception.exception_number
                )),
                Err(error) => self.refused(error),
            },
            _ => self.refused(LedgerError::InvalidOption),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        self.company_name = self.pages.company_name(scope).await?;
        let title = if self.bookkeeping {
            "Bookkeeping controls"
        } else {
            "Approval exceptions"
        };
        let subtitle = if self.bookkeeping {
            "Control posting dates and review changes to bookkeeping controls"
        } else {
            "Same-person approvals and overrides of controls, for someone independent to review."
        };
        let subtitle = if self.company_name.is_empty() {
            subtitle.to_owned()
        } else {
            format!("{} · {subtitle}", self.company_name)
        };
        let mut parts = vec![html(Header {
            title: Html::text(title),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle: &subtitle,
            ..Header::default()
        })?];
        if scope.current_company().is_none() {
            self.close();
            parts.push(html(
                EmptyState::missing(
                    "No current company",
                    "Choose a current company to open its ledger.",
                )
                .id("reports-company-missing"),
            )?);
            return PageView {
                id: if self.bookkeeping {
                    "bookkeeping-controls-page"
                } else {
                    "approval-exceptions-page"
                },
                flash: self.flash_map(),
                parts,
            }
            .render()
            .map_err(|_| Error::Internal);
        }
        let display = self.display(scope).await?;
        if !self.allowed(scope, VIEW).await? {
            self.close();
            parts.push(self.read_refusal(&LedgerError::Unauthorized)?);
        } else {
            let result = if self.bookkeeping {
                self.bookkeeping_parts(scope, &display).await
            } else {
                self.exception_parts(scope, &display).await
            };
            match result {
                Ok(content) => parts.extend(content),
                Err(error) => {
                    self.close();
                    parts.push(self.read_refusal(&error)?);
                }
            }
            if self.dialog.is_some() {
                parts.push(self.dialog_view()?);
            }
        }
        PageView {
            id: if self.bookkeeping {
                "bookkeeping-controls-page"
            } else {
                "approval-exceptions-page"
            },
            flash: self.flash_map(),
            parts,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
    fn read_refusal(&self, error: &LedgerError) -> Result<Html> {
        if self.bookkeeping {
            html(Alert {
                id: "bookkeeping-refusal",
                kind: StatusKind::Error,
                class: "mb-3",
                body: Html::text(&refusal(error)),
            })
        } else if matches!(error, LedgerError::Unauthorized) {
            html(EmptyState::forbidden("view the ledger").id("exceptions-refusal"))
        } else {
            html(EmptyState::missing("The exceptions cannot be shown", "The company or the period could not be read. Choose another period or reload the page.").id("exceptions-refusal"))
        }
    }
    fn flash_map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = text,
                StatusKind::Error => flash.error = text,
                StatusKind::Warning => flash.warning = text,
                StatusKind::Info => flash.info = text,
            }
        }
        flash
    }
    async fn bookkeeping_parts(
        &mut self,
        scope: &Scope,
        display: &DisplayContext,
    ) -> std::result::Result<Vec<Html>, LedgerError> {
        let ledger = self.pages.reports.ledger.clone();
        let company = company(scope)?;
        let controlled = ledger.get_controlled_bookkeeping(scope, company).await?;
        let mut changes = ledger
            .list_controlled_bookkeeping_changes(scope, company)
            .await?;
        changes.reverse();
        let people = self
            .pages
            .people(
                scope,
                changes
                    .iter()
                    .chain(controlled.change.iter())
                    .flat_map(|c| [Some(c.changed_by_user_id), c.changed_by_impersonator_id]),
            )
            .await?;
        let can_manage = self.allowed(scope, MANAGE_CONTROLLED_BOOKKEEPING).await?;
        let change = controlled.change.as_ref();
        // On/off is the Toggle: switching it off asks for the reason the
        // override is listed under; switching it back on does not.
        let mut content = html(Summary {
            status_id: "controlled-bookkeeping-status",
            badge: if can_manage {
                html(Toggle {
                    on: controlled.controlled,
                    label: "Controlled bookkeeping",
                    id: "controlled-bookkeeping-toggle",
                    event: if controlled.controlled {
                        "open_dialog"
                    } else {
                        "turn_on"
                    },
                    name: if controlled.controlled { "dialog" } else { "" },
                    value: if controlled.controlled {
                        "controlled-off"
                    } else {
                        ""
                    },
                    ..Toggle::default()
                })?
            } else {
                html(Badge::status(
                    if controlled.controlled {
                        BadgeKind::Success
                    } else {
                        BadgeKind::Warning
                    },
                    if controlled.controlled {
                        "Controlled"
                    } else {
                        "Not controlled"
                    },
                ))?
            },
            since: change
                .map(|c| datetime(display, c.changed_at, "controlled-bookkeeping-changed-at"))
                .transpose()?
                .unwrap_or_default(),
            actor: change
                .map(|c| people.who(Some(c.changed_by_user_id), c.changed_by_impersonator_id))
                .unwrap_or_default(),
            reason: change.and_then(|c| c.reason.as_deref()).unwrap_or(""),
            action: Html::EMPTY,
        })?;
        if controlled.controlled && controlled.fiscal_years == 0 {
            content.push(&html(Alert { id: "no-fiscal-year", kind: StatusKind::Warning, class: "mt-4", body: Html::text("No fiscal year is open, so no entry can be posted yet. Open the first fiscal year to start.") })?);
        }
        // Where these controls are acted on, side by side in the card.
        content.push(&html(Links {
            id: "controls-links",
            body: {
                let mut links = html(Button {
                    id: "controls-fiscal-years",
                    href: "/finance/ledger/fiscal-years",
                    body: Html::text("Fiscal years"),
                    ..Button::default()
                })?;
                links.push(&html(Button {
                    id: "controls-late-journals",
                    href: "/finance/ledger/journals?review=pending",
                    body: Html::text("Journals awaiting late-recording review"),
                    ..Button::default()
                })?);
                links
            },
        })?);
        if !changes.is_empty() {
            let rows = changes
                .iter()
                .map(|c| {
                    history_row(
                        "controlled-bookkeeping-change",
                        c.change_number,
                        c.controlled,
                        c.reason.as_deref(),
                        c.changed_by_user_id,
                        c.changed_by_impersonator_id,
                        c.changed_at,
                        display,
                        &people,
                        true,
                    )
                })
                .collect::<Result<_>>()?;
            content.push(&self.table(0, rows, "Changes to controlled bookkeeping", None)?);
        }
        Ok(vec![section(
            "controlled-bookkeeping",
            "Controlled bookkeeping",
            "When enabled, entries can be posted only to open fiscal periods. Open a fiscal year before posting to its periods. Late entries can still be posted to an open period.",
            content,
            false,
        )?])
    }
    async fn exception_parts(
        &mut self,
        scope: &Scope,
        display: &DisplayContext,
    ) -> std::result::Result<Vec<Html>, LedgerError> {
        if self.filter("area") == "journals" {
            return self.journal_exception_parts(scope, display).await;
        }
        let ledger = self.pages.reports.ledger.clone();
        let company = company(scope)?;
        let clock = self.sync_period(display);
        let period = self.period.as_ref().expect("period initialized");
        let (from, before) = period
            .period
            .bounds(&clock.timezone)
            .map_err(|_| LedgerError::InvalidOption)?;
        let separation = ledger.get_duty_separation(scope, company).await?;
        let mut changes = ledger.list_duty_separation_changes(scope, company).await?;
        changes.reverse();
        let exceptions = ledger
            .list_approval_exceptions(
                scope,
                company,
                &ApprovalExceptionQuery {
                    recorded_from: Some(from),
                    recorded_before: Some(before),
                    reviewed: self.reviewed(),
                    limit: Some(100),
                    after: self.after(),
                },
            )
            .await?;
        let mut mentioned: Vec<Option<i64>> = changes
            .iter()
            .chain(separation.change.iter())
            .flat_map(|c| [Some(c.changed_by_user_id), c.changed_by_impersonator_id])
            .collect();
        for e in &exceptions {
            mentioned.extend([
                e.prepared_by_user_id,
                e.prepared_by_impersonator_id,
                Some(e.approved_by_user_id),
                e.approved_by_impersonator_id,
            ]);
            if let Some(review) = &e.review {
                mentioned.extend([
                    Some(review.reviewed_by_user_id),
                    review.reviewed_by_impersonator_id,
                ]);
            }
        }
        let people = self.pages.people(scope, mentioned).await?;
        let can_manage = self.allowed(scope, MANAGE_DUTY_SEPARATION).await?;
        let can_review = self.allowed(scope, REVIEW_EXCEPTIONS).await?;
        let change = separation.change.as_ref();
        let mut content = html(Summary {
            status_id: "duty-separation-status",
            // Either way the change is recorded with a reason, so the Toggle
            // opens the reason dialog.
            badge: if can_manage {
                html(Toggle {
                    on: separation.required,
                    label: "Separate approver required",
                    id: "duty-separation-toggle",
                    event: "open_dialog",
                    name: "dialog",
                    value: "separation",
                    ..Toggle::default()
                })?
            } else {
                html(Badge::status(
                    if separation.required {
                        BadgeKind::Success
                    } else {
                        BadgeKind::Warning
                    },
                    if separation.required {
                        "Required"
                    } else {
                        "Not required"
                    },
                ))?
            },
            since: change
                .map(|c| datetime(display, c.changed_at, "duty-separation-changed-at"))
                .transpose()?
                .unwrap_or_default(),
            actor: change
                .map(|c| people.who(Some(c.changed_by_user_id), c.changed_by_impersonator_id))
                .unwrap_or_default(),
            reason: change.and_then(|c| c.reason.as_deref()).unwrap_or(""),
            action: Html::EMPTY,
        })?;
        if !changes.is_empty() {
            let rows = changes
                .iter()
                .map(|c| {
                    history_row(
                        "duty-separation-change",
                        c.change_number,
                        c.required,
                        c.reason.as_deref(),
                        c.changed_by_user_id,
                        c.changed_by_impersonator_id,
                        c.changed_at,
                        display,
                        &people,
                        false,
                    )
                })
                .collect::<Result<_>>()?;
            content.push(&self.table(0, rows, "Changes to separation of duties", None)?);
        }
        let mut parts = vec![self.tabs(
            "approval-areas",
            "Approval area",
            "area",
            &[
                ("journals", "Manual journals"),
                ("other", "Other Finance approvals"),
            ],
        )?];
        parts.push(section("duty-separation", "Other Finance approvals", "This control applies to the other Finance approvals. Manual journals follow their rules on Approval rules.",content,false)?);
        let mut filters = html(self.period.as_ref().expect("period initialized"))?;
        filters.push(&self.tabs_in(
            "exceptions-review",
            "Review status",
            "review",
            &[
                ("all", "All"),
                ("pending", "Awaiting review"),
                ("reviewed", "Reviewed"),
            ],
            "min-w-0 flex-1",
        )?);
        parts.push(html(FiltersView {
            id: "exceptions-filters",
            class: "mt-4 mb-3 flex flex-wrap items-center gap-4",
            content: filters,
        })?);
        let compact = self.columns[1].mode() == RowMode::Compact;
        let mut rows = Vec::new();
        for exception in &exceptions {
            let mut row = row(
                &format!("exception-{}", exception.exception_number),
                &[
                    ("number", exception.exception_number.to_string()),
                    (
                        "prepared",
                        people.who(
                            exception.prepared_by_user_id,
                            exception.prepared_by_impersonator_id,
                        ),
                    ),
                    (
                        "approved",
                        people.who(
                            Some(exception.approved_by_user_id),
                            exception.approved_by_impersonator_id,
                        ),
                    ),
                    ("reason", exception.reason.clone()),
                ],
            );
            row = row.slot(
                "when",
                datetime(
                    display,
                    exception.recorded_at,
                    &format!("exception-at-{}", exception.exception_number),
                )?,
            );
            let mut kind = Html::default();
            if exception.same_person {
                kind.push(&classification(BadgeKind::Warning, "Same person")?);
            }
            if exception.overrides {
                kind.push(&classification(BadgeKind::Neutral, "Override")?);
            }
            row = row.slot("kind", kind);
            row = row.slot(
                "what",
                html(CellView {
                    id: "",
                    main: &format!(
                        "{}: {}",
                        source_label(&exception.source),
                        action_label(&exception.action)
                    ),
                    detail: &exception.subject,
                    compact,
                })?,
            );
            let review = match &exception.review {
                Some(review) => html(CellView {
                    id: &format!("exception-review-{}", exception.exception_number),
                    main: &people.who(
                        Some(review.reviewed_by_user_id),
                        review.reviewed_by_impersonator_id,
                    ),
                    detail: review.note.as_deref().unwrap_or(""),
                    compact,
                })?,
                None if can_review => review_button(
                    &format!("exception-review-open-{}", exception.exception_number),
                    "exception",
                    exception.id,
                )?,
                None => Html::text("Awaiting review"),
            };
            rows.push(row.slot("review", review));
        }
        parts.push(section(
            "exceptions-card",
            "",
            "",
            self.table(
                1,
                rows,
                "Approval exceptions",
                Some((
                    "exceptions-empty",
                    "No approval exceptions",
                    "Nobody approved their own work or overrode a control in this period.",
                )),
            )?,
            true,
        )?);
        if exceptions.len() == 100 {
            parts.push(self.next(
                "exceptions-next",
                exceptions.last().expect("100 exceptions").exception_number,
                "Later exceptions",
            )?);
        }
        Ok(parts)
    }
    async fn journal_exception_parts(
        &mut self,
        scope: &Scope,
        display: &DisplayContext,
    ) -> std::result::Result<Vec<Html>, LedgerError> {
        let company = company(scope)?;
        let mut parts = vec![self.tabs(
            "approval-areas",
            "Approval area",
            "area",
            &[
                ("journals", "Manual journals"),
                ("other", "Other Finance approvals"),
            ],
        )?];
        // Where journal approvals are set and decided: the standard buttons.
        let links = html(Links {
            id: "journal-approval-links",
            body: {
                let mut links = html(Button {
                    id: "journal-approval-rules",
                    href: &format!(
                        "/workflow/approval-rules?case=finance.ledger.manual_journal&company={company}"
                    ),
                    body: Html::text("Journal approval rules"),
                    ..Button::default()
                })?;
                links.push(&html(Button {
                    id: "journal-approval-waiting",
                    href: "/workflow/approvals",
                    body: Html::text("Waiting for my approval"),
                    ..Button::default()
                })?);
                links
            },
        })?;
        parts.push(section("journal-approval-controls","Journal approvals","Journal rules decide who may approve. Approvals by the person who prepared a journal are kept here for independent review.",links,false)?);
        let clock = self.sync_period(display);
        let (from, before) = self
            .period
            .as_ref()
            .expect("period initialized")
            .period
            .bounds(&clock.timezone)
            .map_err(|_| LedgerError::InvalidOption)?;
        let mut after = self.after().unwrap_or(0);
        let mut shown: Vec<Value> = Vec::new();
        let mut next_cursor = None;
        // The contract filters only by review state and cursor; the period and the Reviewed tab are
        // applied here, so keep reading pages until one has a row to show or the list ends.
        for _ in 0..20 {
            let envelope=match self.pages.contracts.read(scope,"base.workflow.approvals.exceptions",&json!({"company":company,"case":"finance.ledger.manual_journal","after":after,"page_size":100,"unreviewed":self.filter("review")=="pending"})).await {
                Ok(value)=>value,
                Err(error)=>{parts.push(html(Alert {id:"journal-exceptions-refused",kind:StatusKind::Error,body:Html::text(match error {bilibi_agent_api::Error::Forbidden=>"Your role does not allow you to read approval exceptions.",_=>"The journal approval exceptions could not be read. Reload the page and try again."}),..Alert::default()})?);return Ok(parts);}
            };
            let result = &envelope["result"];
            for e in result["entries"].as_array().ok_or(Error::Internal)? {
                let stamp = e["created_at"]
                    .as_str()
                    .and_then(|s| s.parse::<Timestamp>().ok())
                    .ok_or(Error::Internal)?;
                if stamp >= from
                    && stamp < before
                    && (self.filter("review") != "reviewed" || e["reviewed"] == true)
                {
                    shown.push(e.clone());
                }
            }
            next_cursor = result["next_cursor"].as_i64();
            match next_cursor {
                Some(cursor) if shown.is_empty() => after = cursor,
                _ => break,
            }
        }
        let mut filters = html(self.period.as_ref().expect("period initialized"))?;
        filters.push(&self.tabs_in(
            "journal-exceptions-review",
            "Review status",
            "review",
            &[
                ("all", "All"),
                ("pending", "Not reviewed"),
                ("reviewed", "Reviewed"),
            ],
            "min-w-0 flex-1",
        )?);
        parts.push(html(FiltersView {
            id: "exceptions-filters",
            class: "mt-4 mb-3 flex flex-wrap items-center gap-4",
            content: filters,
        })?);
        let people = self
            .pages
            .people(
                scope,
                shown.iter().flat_map(|e| {
                    [
                        e["reviewer"].as_i64(),
                        e["actor"].as_i64(),
                        e["impersonator"].as_i64(),
                    ]
                }),
            )
            .await?;
        let mut rows = Vec::new();
        for e in &shown {
            let id = e["id"].as_i64().ok_or(Error::Internal)?;
            let label = e["label"].as_str().unwrap_or("Journal");
            let url = e["owner_url"]
                .as_str()
                .unwrap_or("/finance/ledger/journals");
            let review = if e["reviewed"] == true {
                let by = e["reviewer"].as_i64();
                Html::text(&format!(
                    "Reviewed by {}. {}",
                    people.who(by, None),
                    e["review_note"].as_str().unwrap_or("")
                ))
            } else if e["reviewable"] == true {
                html(bilibi_ui::ActionLink {
                    id: &format!("journal-exception-review-{id}"),
                    href: &format!(
                        "/workflow/approval-exceptions?case=finance.ledger.manual_journal&company={company}&cursor={}",
                        id - 1
                    ),
                    body: Html::text("Record review"),
                    ..bilibi_ui::ActionLink::default()
                })?
            } else {
                Html::text("Waiting for an independent review")
            };
            let stamp = e["created_at"]
                .as_str()
                .and_then(|s| s.parse::<Timestamp>().ok())
                .ok_or(Error::Internal)?;
            rows.push(
                FlexRow::new(id.to_string())
                    .slot(
                        "when",
                        datetime(display, stamp, &format!("journal-exception-at-{id}"))?,
                    )
                    .slot(
                        "subject",
                        html(bilibi_ui::lists::RecordLink {
                            workspace: self.framed,
                            kind: bilibi_finance_ledger::MODULE,
                            record_id: &format!(
                                "journal-{}",
                                e["subject_id"].as_i64().ok_or(Error::Internal)?
                            ),
                            href: url,
                            class: "",
                            body: Html::text(label),
                        })?,
                    )
                    .cell(
                        "who",
                        bilibi_ui::Cell::text(
                            people.who(e["actor"].as_i64(), e["impersonator"].as_i64()),
                        ),
                    )
                    .cell(
                        "reason",
                        bilibi_ui::Cell::text(e["reason"].as_str().unwrap_or("")),
                    )
                    .slot("review", review),
            );
        }
        parts.push(self.table(
            2,
            rows,
            "Manual journal approval exceptions",
            Some((
                "journal-exceptions-empty",
                "No journal approval exceptions",
                "Approvals by the person who prepared a journal appear here.",
            )),
        )?);
        if let Some(next) = next_cursor {
            parts.push(self.next("journal-exceptions-next", next, "Later exceptions")?);
        }
        Ok(parts)
    }

    fn tabs(&self, id: &str, label: &str, key: &str, choices: &[(&str, &str)]) -> Result<Html> {
        self.tabs_in(id, label, key, choices, "")
    }
    /// Tabs with a page-context class: the review status shares its row with
    /// the period filter (`min-w-0 flex-1`), as one toolbar.
    fn tabs_in(
        &self,
        id: &str,
        label: &str,
        key: &str,
        choices: &[(&str, &str)],
        class: &str,
    ) -> Result<Html> {
        let links: Vec<_> = choices
            .iter()
            .map(|(value, _)| self.link(key, value))
            .collect();
        html(Tabs {
            id,
            label,
            class,
            tabs: choices
                .iter()
                .zip(&links)
                .map(|((value, title), link)| Tab::patch(link, title, self.filter(key) == *value))
                .collect(),
        })
    }
    fn next(&self, id: &str, after: i64, label: &str) -> Result<Html> {
        let mut query = self.query();
        query.insert("after".into(), after.to_string());
        html(NextLink {
            id,
            href: &params::address(self.path(), &query),
            label,
        })
    }
    fn table(
        &self,
        index: usize,
        rows: Vec<FlexRow>,
        caption: &str,
        empty: Option<(&str, &str, &str)>,
    ) -> Result<Html> {
        if rows.is_empty() {
            if let Some((id, title, reason)) = empty {
                return html(EmptyState::missing(title, reason).id(id));
            }
        }
        let columns = &self.columns[index];
        let event = format!("control_grid_{index}");
        let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.event = &event;
        table.caption = caption;
        table.framed = false;
        html(table)
    }
    fn dialog_view(&self) -> Result<Html> {
        let Some(dialog) = self.dialog else {
            return Ok(Html::default());
        };
        let (prefix, title, description, submit, label, working, variant, field_label, required) =
            match dialog {
                Dialog::ControlledOff => (
                    "controlled-off",
                    "Turn off controlled bookkeeping",
                    "Entries may then post into a date no fiscal period covers, after the last year. The change is listed in the approval exceptions for an independent reviewer, with this reason.",
                    "turn_off",
                    "Turn off",
                    "Saving…",
                    ButtonVariant::Danger,
                    "Reason (1 to 500 characters)",
                    true,
                ),
                Dialog::Separation { required: false } => (
                    "duty-separation",
                    "Turn separation of duties off",
                    "Whoever prepares a record may then approve it. Each such approval is listed here with this reason, for someone independent to review.",
                    "change_separation",
                    "Turn off",
                    "Saving…",
                    ButtonVariant::Primary,
                    "Reason (1 to 500 characters)",
                    true,
                ),
                Dialog::Separation { required: true } => (
                    "duty-separation",
                    "Require separation of duties again",
                    "Nobody may then approve, post, or finalise a record they prepared.",
                    "change_separation",
                    "Require again",
                    "Saving…",
                    ButtonVariant::Primary,
                    "Reason (optional)",
                    false,
                ),
                Dialog::Review { .. } => (
                    "exception-review",
                    "Record your review",
                    "Your review is kept with your name and the time; it cannot be changed. You cannot review an approval you made or a record you prepared.",
                    "review",
                    "Record review",
                    "Recording…",
                    ButtonVariant::Primary,
                    "Note (optional, up to 500 characters)",
                    false,
                ),
            };
        let fields = {
            let id = if prefix == "exception-review" {
                "exception-review-note".into()
            } else if prefix == "duty-separation" {
                "duty-separation-reason".into()
            } else {
                format!("{prefix}-reason")
            };
            let field = Field::in_form("dialog", "text")
                .id(id)
                .label(field_label)
                .kind(InputKind::Textarea)
                .value(&self.text)
                .attr("maxlength", "500");
            vec![if required { field.required() } else { field }]
        };
        let form = html(FormView {
            id: &format!("{prefix}-form"),
            fields,
            submit,
            cancel_id: &format!("{prefix}-close"),
            confirm_id: &format!("{prefix}-confirm"),
            label,
            working,
            variant,
        })?;
        html(Modal {
            id: &format!("{prefix}-modal"),
            title,
            description: &format!("{} · {description}", self.company_name),
            cancel_event: "close_dialog",
            body: form,
            flash: Some(self.flash_map()),
            ..Modal::default()
        })
    }
}

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::Forbidden)
}
fn html(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn button(
    id: &str,
    event: &str,
    name: &str,
    value: &str,
    label: &str,
    variant: ButtonVariant,
) -> Result<Html> {
    html(Button {
        id,
        event,
        name,
        value,
        variant,
        button_type: "button",
        body: Html::text(label),
        ..Button::default()
    })
}
fn review_button(id: &str, name: &str, value: i64) -> Result<Html> {
    button(
        id,
        "open_dialog",
        name,
        &value.to_string(),
        "Record review",
        ButtonVariant::Secondary,
    )
}
fn row(id: &str, cells: &[(&str, String)]) -> FlexRow {
    let mut row = FlexRow::new(id);
    row.dom_id = Some(id.into());
    for (key, value) in cells {
        row = row.slot(key, Html::text(value));
    }
    row
}
#[allow(clippy::too_many_arguments)]
fn history_row(
    prefix: &str,
    number: i64,
    enabled: bool,
    reason: Option<&str>,
    user: i64,
    by: Option<i64>,
    at: Timestamp,
    display: &DisplayContext,
    people: &People,
    bookkeeping: bool,
) -> Result<FlexRow> {
    let label = match (bookkeeping, enabled) {
        (true, true) => "Controlled",
        (true, false) => "Not controlled",
        (false, true) => "Required",
        (false, false) => "Not required",
    };
    Ok(row(
        &format!("{prefix}-{number}"),
        &[
            ("set", label.into()),
            ("reason", reason.unwrap_or("—").into()),
            ("by", people.who(Some(user), by)),
        ],
    )
    .slot(
        "when",
        datetime(display, at, &format!("{prefix}-at-{number}"))?,
    ))
}
fn section(id: &str, title: &str, description: &str, content: Html, flat: bool) -> Result<Html> {
    let heading = format!("{id}-heading");
    let mut body = if title.is_empty() {
        Html::default()
    } else {
        html(SectionHeading {
            id: &heading,
            title,
            description,
            ..SectionHeading::default()
        })?
    };
    body.push(&content);
    html(Card {
        id,
        inner_class: match id {
            _ if flat => "p-0",
            _ => "p-5 sm:p-6",
        },
        role: "region",
        labelledby: if title.is_empty() { "" } else { &heading },
        body,
        ..Card::default()
    })
}
fn datetime(display: &DisplayContext, at: Timestamp, id: &str) -> Result<Html> {
    let rendered = display.render(at, Format::DateTime, Precision::Second);
    html(TimeView {
        id,
        instant: bilibi_ui::flex_table::Instant {
            precision: "second",
            ..bilibi_ui::flex_table::Instant::new(
                &rendered.iso,
                &rendered.text,
                &rendered.company,
                &rendered.utc,
                rendered.mode.as_str(),
            )
        },
    })
}
fn source_label(source: &str) -> &str {
    match source {
        "finance/ledger" => "Ledger",
        "finance/payables" => "Payables",
        "finance/reconciliation" => "Bank reconciliation",
        _ => source,
    }
}
fn action_label(action: &str) -> String {
    match action {
        "document_approval" => "Document approved".into(),
        "match_override" => "Match exceptions approved".into(),
        "payment_approval" => "Payment approved".into(),
        "journal_approval" => "Journal approved".into(),
        "reconciliation_finalisation" => "Reconciliation finalised".into(),
        "period_reopen" => "Period reopened".into(),
        "fiscal_year_reopen" => "Fiscal year reopened".into(),
        _ => {
            let text = action.replace('_', " ");
            let mut chars = text.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}
fn refusal(error: &LedgerError) -> String {
    match error {
        LedgerError::Unauthorized => "You do not have permission for that action.",
        LedgerError::CompanyArchived => "The company is archived; nothing can be changed.",
        LedgerError::BookNotOpen => "The ledger's book is not open yet.",
        LedgerError::EntryNotFound => "That entry no longer exists.",
        LedgerError::ExceptionNotFound => "That exception no longer exists.",
        LedgerError::AlreadyReviewed => "Someone has already reviewed that exception.",
        LedgerError::ReviewerNotIndependent => {
            "An exception is reviewed by someone who neither approved nor prepared what it records."
        }
        _ => return format!("That could not be done ({error})."),
    }
    .into()
}
#[derive(Template)]
#[template(path = "control_page.html")]
struct PageView<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
#[derive(Template)]
#[template(path = "control_summary.html")]
struct Summary<'a> {
    status_id: &'a str,
    badge: Html,
    since: Html,
    actor: String,
    reason: &'a str,
    action: Html,
}
/// Buttons that lead from a control to where it is acted on, in one row.
#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" class="mt-3 flex flex-wrap items-center gap-2">{{ body }}</div>"#,
    ext = "html"
)]
struct Links<'a> {
    id: &'a str,
    body: Html,
}
#[derive(Template)]
#[template(path = "control_cell.html")]
struct CellView<'a> {
    id: &'a str,
    main: &'a str,
    detail: &'a str,
    compact: bool,
}
#[derive(Template)]
#[template(path = "control_form.html")]
struct FormView<'a> {
    id: &'a str,
    submit: &'a str,
    fields: Vec<Field>,
    cancel_id: &'a str,
    confirm_id: &'a str,
    label: &'a str,
    working: &'a str,
    variant: ButtonVariant,
}
#[derive(Template)]
#[template(
    source = "{% import \"flex_table.html\" as table %}{% call table::instant(id=id, i=instant) %}{% endcall %}",
    ext = "html"
)]
struct TimeView<'a> {
    id: &'a str,
    instant: bilibi_ui::flex_table::Instant,
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "controls" => Some("Bookkeeping controls"),
            "approval-exceptions" => Some("Approval exceptions"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = State::new(
                self.clone(),
                request.operation,
                request.query,
                request.framed,
            )?;
            Ok(PageBody {
                html: state
                    .render(request.scope.ok_or(Error::Unauthorized)?)
                    .await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(State::new(
                self.clone(),
                request.operation,
                request.query,
                request.framed,
            )?);
            Ok(Some(state))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Event(payload) => self.event(request.scope, payload).await?,
                LiveInput::Params(query) => {
                    self.list = ListState::parse(
                        query,
                        self.list
                            .spec()
                            .clone()
                            .shared()
                            .map_err(|_| Error::Internal)?,
                    )
                    .map_err(|_| Error::Invalid)?;
                    self.period = None;
                    self.close();
                }
                LiveInput::Refresh => {}
            }
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                query: self.query(),
                ..LiveBody::default()
            })
        })
    }
}

#[derive(Template)]
#[template(path = "control_filters.html")]
struct FiltersView<'a> {
    id: &'a str,
    class: &'a str,
    content: Html,
}
/// Bilimbi's plain "Later …" patch link under a capped list.
#[derive(Template)]
#[template(
    source = r#"<div class="mt-3"><a id="{{ id }}" href="{{ href }}" data-patch class="text-action hover:underline">{{ label }}</a></div>"#,
    ext = "html"
)]
struct NextLink<'a> {
    id: &'a str,
    href: &'a str,
    label: &'a str,
}
fn classification(kind: BadgeKind, text: &str) -> Result<Html> {
    html(Badge {
        dot: false,
        ..Badge::status(kind, text)
    })
}
