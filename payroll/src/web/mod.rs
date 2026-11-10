//! Bilimbi Payroll.Web.{PostingsLive,SetupLive} at 4abfaf829321f8540581d833752a086b21b45ba9.
//! Runs and claims come from Finance-owned copies of People events.
use crate::{
    MappingChange, Payroll, PayrollError, Reversal, SettingsChange, MANAGE_SETTINGS, POST_POSTINGS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_finance_ledger::{
    Account, AccountName, AccountType, EntryLink, FieldError, LedgerError, MinorUnits, Money,
    People,
};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    forms::{Choice, Field, InputKind},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, Card, Combobox, CommitStates,
    CommitStatus, CreateButton, EmptyState, Fact, Facts, FlashMap, FlexRow, FlexTable, Fold,
    Header, Html, IconButton, InlineChoice, InlineField, ListColumns, ListSpec, ListState,
    SectionHeading, StatusKind, TableEmpty, NO_FLASH,
};
use jiff::Timestamp;
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

mod postings;
mod setup;

#[derive(Clone)]
pub(crate) struct Pages {
    pub payroll: Arc<Payroll>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
}

impl Pages {
    /// The names of the people a page mentions ("Ana Lim", never "#5").
    async fn people(
        &self,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Result<People> {
        let client = self.payroll.db.get().await?;
        Ok(People::read(&self.directory, &**client, scope, ids).await)
    }
}

/// The two accounts owed to staff, as the setup page names them.
const SETTINGS_ACCOUNTS: [(&str, &str); 2] = [
    ("net_pay_account_id", "Net pay"),
    ("claims_payable_account_id", "Claims payable"),
];

struct State {
    pages: Pages,
    setup: bool,
    list: ListState,
    columns: Vec<ListColumns>,
    /// Before the posting accounts are set, the first one chosen waits here
    /// until the other is chosen too; both are then saved together.
    settings: HashMap<String, String>,
    /// Each value edited in place, keyed `settings-<field>` or
    /// `mapping-<id>-<field>`.
    statuses: CommitStates,
    /// The "+ Mapping" dialog is open, with its draft.
    adding: bool,
    mapping: HashMap<String, String>,
    errors: Vec<FieldError>,
    error_form: &'static str,
    /// A dialog's refusal that has no field of its own.
    dialog_alert: Option<String>,
    /// The posting whose reversal dialog is open.
    reversing: Option<i64>,
    reversal: HashMap<String, String>,
    flash: Option<(StatusKind, String)>,
}

impl State {
    fn new(pages: Pages, operation: &str, query: &HashMap<String, String>) -> Result<Self> {
        let setup = match operation {
            "setup" => true,
            "postings" => false,
            _ => return Err(Error::NotFound),
        };
        let tables: &[(&str, &[(&str, &str)])] = if setup {
            &[
                (
                    "payroll-mappings-table",
                    &[
                        ("kind", "Kind"),
                        ("code", "Code"),
                        ("direction", "Direction"),
                        ("expense", "Expense account"),
                        ("liability", "Liability account"),
                    ],
                ),
                (
                    "payroll-settings-changes-table",
                    &[
                        ("when", "When"),
                        ("by", "By"),
                        ("net", "Net pay"),
                        ("claims", "Claims payable"),
                        ("reason", "Reason"),
                    ],
                ),
                (
                    "payroll-mapping-changes-table",
                    &[
                        ("when", "When"),
                        ("by", "By"),
                        ("mapping", "Mapping"),
                        ("expense", "Expense account"),
                        ("liability", "Liability account"),
                        ("reason", "Reason"),
                    ],
                ),
            ]
        } else {
            &[
                (
                    "payroll-runs-table",
                    &[
                        ("reference", "Run"),
                        ("date", "Pay date"),
                        ("status", "Run status"),
                        ("standing", "In the books"),
                        ("failure", "Last failure"),
                    ],
                ),
                (
                    "payroll-claims-table",
                    &[
                        ("reference", "Claim"),
                        ("category", "Category"),
                        ("date", "Approved on"),
                        ("amount", "Amount"),
                        ("standing", "In the books"),
                        ("failure", "Last failure"),
                    ],
                ),
                (
                    "payroll-postings-table",
                    &[
                        ("source", "Source"),
                        ("entry", "Journal"),
                        ("date", "Entry date"),
                        ("total", "Total"),
                        ("standing", "Status"),
                        ("reason", "Reversal reason"),
                    ],
                ),
            ]
        };
        let columns = tables
            .iter()
            .map(|(id, fields)| {
                ListColumns::mount(
                    id,
                    fields
                        .iter()
                        .map(|(key, label)| {
                            let col = BuiltinColumn::new(key, label);
                            if matches!(*key, "total" | "amount") {
                                col.right()
                            } else {
                                col
                            }
                        })
                        .collect(),
                )
                .map_err(|_| Error::Internal)
            })
            .collect::<Result<Vec<_>>>()?;
        // No sort, filter or pager in Bilimbi Payroll. ListState validates the
        // address without allowing a forged query to hide operational rows.
        let spec = ListSpec {
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            setup,
            columns,
            list: ListState::parse(query, spec).map_err(|_| Error::Internal)?,
            settings: HashMap::new(),
            statuses: CommitStates::default(),
            adding: false,
            mapping: default_mapping(),
            errors: Vec::new(),
            error_form: "",
            dialog_alert: None,
            reversing: None,
            reversal: HashMap::new(),
            flash: None,
        })
    }

    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.pages.payroll.db.get().await?;
        match crate::authorize(&**client, scope, company(scope)?, capability).await {
            Ok(()) => Ok(true),
            Err(PayrollError::Unauthorized) => Ok(false),
            Err(_) => Ok(false),
        }
    }

    /// A refusal: a dialog's field errors beside their fields and the rest
    /// in the dialog's own alert; outside a dialog, the page's message.
    fn refusal(&mut self, form: &'static str, error: PayrollError) {
        let dialog_fields: &[&str] = match form {
            "mapping" => &[
                "kind",
                "code",
                "direction",
                "expense_account_id",
                "liability_account_id",
            ],
            "reversal" => &["reason", "entry_date"],
            _ => &[],
        };
        let message = match &error {
            PayrollError::Invalid(fields) | PayrollError::Ledger(LedgerError::Invalid(fields)) => {
                self.errors = fields.clone();
                self.error_form = form;
                let rest = fields
                    .iter()
                    .filter(|e| !dialog_fields.contains(&e.field))
                    .map(|e| format!("{} {}", e.field.replace('_', " "), e.message))
                    .collect::<Vec<_>>();
                if rest.is_empty() {
                    None
                } else {
                    Some(format!("Refused: {}.", rest.join("; ")))
                }
            }
            PayrollError::Failed(_) => {
                Some("Nothing was changed. Reload the page and try again.".into())
            }
            _ => Some(error.to_string()),
        };
        if dialog_fields.is_empty() {
            self.flash = message.map(|m| (StatusKind::Error, m));
        } else {
            self.dialog_alert = message;
        }
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let event = value["type"].as_str().unwrap_or_default();
        if let Some(index) = event
            .strip_prefix("grid_")
            .and_then(|i| i.parse::<usize>().ok())
        {
            if let Some(columns) = self.columns.get_mut(index) {
                columns.handle(&params::string_values(value));
            }
            return Ok(());
        }
        if event == "clear-flash" {
            if self
                .flash
                .as_ref()
                .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
            {
                self.flash = None;
            }
            return Ok(());
        }
        match event {
            "close_mapping" => {
                self.adding = false;
                self.mapping = default_mapping();
                self.errors.clear();
                self.dialog_alert = None;
                return Ok(());
            }
            "cancel_reversal" => {
                self.reversing = None;
                self.reversal.clear();
                self.errors.clear();
                self.dialog_alert = None;
                return Ok(());
            }
            _ => {}
        }
        let settings_event = matches!(
            event,
            "save_account" | "save_mapping_account" | "new_mapping" | "save_mapping"
        );
        let posting_event = matches!(
            event,
            "post_run" | "post_claim" | "post_all_claims" | "reverse" | "confirm_reversal"
        );
        if (settings_event && !self.setup) || (posting_event && self.setup) {
            return Ok(());
        }
        if (settings_event && !self.allowed(scope, MANAGE_SETTINGS).await?)
            || (posting_event && !self.allowed(scope, POST_POSTINGS).await?)
        {
            match event {
                "save_account" | "save_mapping_account" => {
                    self.statuses
                        .put(inline_key(value), CommitStatus::Error(DENIED.into()));
                }
                "save_mapping" => self.dialog_alert = Some(DENIED.into()),
                "confirm_reversal" => self.dialog_alert = Some(DENIED.into()),
                _ => self.refusal("", PayrollError::Unauthorized),
            }
            return Ok(());
        }
        let payroll = self.pages.payroll.clone();
        let company = company(scope)?;
        match event {
            "save_account" => self.save_account(scope, company, value).await?,
            "save_mapping_account" => self.save_mapping_account(scope, company, value).await?,
            "new_mapping" => {
                self.adding = true;
                self.mapping = default_mapping();
                self.errors.clear();
                self.dialog_alert = None;
                self.flash = None;
            }
            "change_mapping" | "save_mapping" => {
                self.mapping = match read_form(
                    value,
                    "mapping",
                    &[
                        "kind",
                        "code",
                        "direction",
                        "expense_account_id",
                        "liability_account_id",
                    ],
                ) {
                    Ok(form) => form,
                    Err(error) => {
                        self.refusal("mapping", error);
                        return Ok(());
                    }
                };
                reconcile_mapping(&mut self.mapping);
                if event == "save_mapping" {
                    self.errors.clear();
                    self.dialog_alert = None;
                    let result = match (
                        account_id(&self.mapping, "expense_account_id", false),
                        account_id(&self.mapping, "liability_account_id", false),
                    ) {
                        (Ok(expense), Ok(liability)) => {
                            payroll
                                .put_mapping(
                                    scope,
                                    company,
                                    &MappingChange {
                                        kind: get(&self.mapping, "kind").into(),
                                        code: get(&self.mapping, "code").into(),
                                        direction: get(&self.mapping, "direction").into(),
                                        expense_account_id: expense,
                                        liability_account_id: liability,
                                        reason: None,
                                    },
                                )
                                .await
                        }
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    };
                    match result {
                        Ok(m) => {
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "Mapping {} ({}) saved.",
                                    m.code,
                                    direction_name(&m.direction)
                                ),
                            ));
                            self.mapping = default_mapping();
                            self.adding = false;
                        }
                        Err(e) => self.refusal("mapping", e),
                    }
                }
            }
            "reverse" => {
                self.reversing = params::positive_id(value["id"].as_str());
                self.reversal.clear();
                self.errors.clear();
                self.dialog_alert = None;
                self.flash = None;
            }
            "change_reversal" | "confirm_reversal" => {
                // A submit carries the fields; an event without them confirms
                // the reason and date the page holds from typing.
                let posted = value
                    .as_object()
                    .is_some_and(|o| o.keys().any(|k| k.starts_with("reversal[")));
                if event == "change_reversal" || posted {
                    self.reversal = match read_form(value, "reversal", &["reason", "entry_date"]) {
                        Ok(form) => form,
                        Err(error) => {
                            self.refusal("reversal", error);
                            return Ok(());
                        }
                    };
                }
                if event == "confirm_reversal" {
                    self.errors.clear();
                    self.dialog_alert = None;
                    let Some(id) = self.reversing else {
                        return Ok(());
                    };
                    // The dialog is the confirmation: it names the reason the
                    // reversal is recorded with.
                    let result = match reversal(&self.reversal) {
                        Ok(r) => payroll.reverse_posting(scope, company, id, &r).await,
                        Err(e) => Err(e),
                    };
                    match result {
                        Ok(p) => {
                            self.reversing = None;
                            self.reversal.clear();
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "Reversed as journal {}.",
                                    p.reversal.expect("committed reversal").entry_number
                                ),
                            ));
                        }
                        Err(e) => self.refusal("reversal", e),
                    }
                }
            }
            "post_run" | "post_claim" | "post_all_claims" => {
                // Resolve the source and standing again for every write.
                let id = params::positive_id(value["id"].as_str()).unwrap_or(0);
                if event == "post_all_claims" {
                    match payroll.list_claims(scope, company).await {
                        Ok(claims) => {
                            let ids = claims
                                .iter()
                                .filter(|c| c["posting"].is_null())
                                .filter_map(|c| c["id"].as_i64())
                                .collect::<Vec<_>>();
                            match payroll.post_claims(scope, company, &ids).await {
                                Ok(outcomes) => {
                                    let posted = outcomes.iter().filter(|(_, r)| r.is_ok()).count();
                                    let failed = outcomes.len() - posted;
                                    self.flash = Some((
                                        if failed == 0 {
                                            StatusKind::Success
                                        } else {
                                            StatusKind::Error
                                        },
                                        format!("{posted} claims posted, {failed} failed."),
                                    ));
                                }
                                Err(e) => self.refusal("", e),
                            }
                        }
                        Err(e) => self.refusal("", e),
                    }
                } else {
                    let result = if event == "post_run" {
                        payroll.post_payroll_run(scope, company, id).await
                    } else {
                        payroll.post_claim(scope, company, id).await
                    };
                    match result {
                        Ok(p) => {
                            self.flash = Some((
                                StatusKind::Success,
                                format!(
                                    "{} posted as journal {}.",
                                    if event == "post_run" {
                                        "Payroll run"
                                    } else {
                                        "Claim"
                                    },
                                    p.entry_number
                                ),
                            ))
                        }
                        Err(e) => self.refusal("", e),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let mut parts = Vec::new();
        let (title, subtitle, link_id, link, label) = if self.setup {
            (
                "Payroll posting accounts",
                "Where net pay and approved claims are owed, and which accounts each payroll component and claim category posts to.",
                "payroll-setup-postings",
                "/finance/payroll",
                "Payroll and claims",
            )
        } else {
            (
                "Payroll and claims",
                "Finalised payroll runs and approved claims, and whether each is in the books.",
                "payroll-postings-setup",
                "/finance/payroll/setup",
                "Posting accounts",
            )
        };
        let mut actions = render(Button {
            id: link_id,
            href: link,
            body: Html::text(label),
            ..Button::default()
        })?;
        if self.setup && self.allowed(scope, MANAGE_SETTINGS).await? {
            actions.push(&render(CreateButton {
                noun: "Mapping",
                id: "payroll-mapping-new",
                event: "new_mapping",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        parts.push(render(Header {
            title: Html::text(title),
            title_actions: Html::EMPTY,
            subtitle,
            actions,
        })?);
        if self.setup {
            parts.extend(self.setup_parts(scope).await?);
        } else {
            parts.extend(self.postings_parts(scope).await?);
        }
        let mut flash = NO_FLASH;
        if let Some((kind, text)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = text,
                StatusKind::Error => flash.error = text,
                StatusKind::Info => flash.info = text,
                StatusKind::Warning => flash.warning = text,
            }
        }
        PageView {
            id: if self.setup {
                "payroll-setup"
            } else {
                "payroll-postings"
            },
            flash,
            parts,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// A dialog's refusal that has no field, in the dialog's own alert.
    fn dialog_alert(&self, id: &str) -> Result<Html> {
        match &self.dialog_alert {
            Some(message) => render(Alert {
                kind: StatusKind::Error,
                id,
                class: "mt-4",
                body: Html::text(message),
            }),
            None => Ok(Html::EMPTY),
        }
    }

    fn field_errors(&self, form: &str, key: &str) -> Vec<String> {
        if self.error_form == form {
            self.errors
                .iter()
                .filter(|e| e.field == key)
                .map(|e| e.message.clone())
                .collect()
        } else {
            vec![]
        }
    }

    fn field(
        &self,
        form: &str,
        key: &str,
        id: &str,
        label: &str,
        values: &HashMap<String, String>,
    ) -> Field {
        Field::in_form(form, key)
            .id(id)
            .label(label)
            .value(get(values, key))
            .errors(self.field_errors(form, key))
    }

    fn table(
        &mut self,
        index: usize,
        rows: Vec<FlexRow>,
        caption: &str,
        empty: &str,
        actions: bool,
    ) -> Result<Html> {
        self.table_with(
            index,
            rows,
            caption,
            EmptyState::missing(empty, ""),
            actions,
        )
    }

    fn table_with(
        &mut self,
        index: usize,
        rows: Vec<FlexRow>,
        caption: &str,
        empty: EmptyState,
        actions: bool,
    ) -> Result<Html> {
        let columns = &self.columns[index];
        let event = format!("grid_{index}");
        let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = columns.zoom;
        table.suggestions = columns.flex_suggestions();
        table.add_query = &columns.add_query;
        table.event = &event;
        table.caption = caption;
        table.framed = false;
        table.actions = actions;
        table.empty = Some(TableEmpty::State(empty));
        render(table)
    }
}

fn company(scope: &Scope) -> Result<i64> {
    scope
        .current_company()
        .map(|id| id.get())
        .ok_or(Error::Forbidden)
}
fn get<'a>(values: &'a HashMap<String, String>, key: &str) -> &'a str {
    values.get(key).map(String::as_str).unwrap_or_default()
}
fn default_mapping() -> HashMap<String, String> {
    HashMap::from([
        ("kind".into(), "component".into()),
        ("direction".into(), "earning".into()),
    ])
}

/// Bilimbi LiveAuthorization's denied message.
const DENIED: &str = "You do not have permission to do that.";

/// The key an in-place value keeps its outcome under: `settings-<field>`
/// or `mapping-<id>-<field>`.
fn inline_key(value: &Value) -> String {
    let field = value["field"].as_str().unwrap_or_default();
    match value["id"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => format!("mapping-{id}-{field}"),
        None => format!("settings-{field}"),
    }
}

/// Which accounts a direction posts to: (expense, liability).
fn needed(direction: &str) -> (bool, bool) {
    (
        matches!(direction, "earning" | "employer" | "claim"),
        matches!(direction, "deduction" | "employer"),
    )
}

fn direction_name(direction: &str) -> &str {
    match direction {
        "earning" => "Earning",
        "deduction" => "Deduction",
        "employer" => "Employer contribution",
        "claim" => "Claim",
        other => other,
    }
}

/// The dialog's draft after a change: a claim category posts as a claim,
/// and an account its direction does not post to is dropped, so the
/// dialog offers only what the save accepts.
fn reconcile_mapping(mapping: &mut HashMap<String, String>) {
    if get(mapping, "kind") == "claim_category" {
        mapping.insert("direction".into(), "claim".into());
    } else if !matches!(
        get(mapping, "direction"),
        "earning" | "deduction" | "employer"
    ) {
        mapping.insert("direction".into(), "earning".into());
    }
    let (expense, liability) = needed(get(mapping, "direction"));
    if !expense {
        mapping.remove("expense_account_id");
    }
    if !liability {
        mapping.remove("liability_account_id");
    }
}

/// Generic form_values drops strings over 1 KiB. A valid 500-character
/// Unicode reason can occupy 2 KiB: preserve known fields for facade validation.
fn read_form(
    value: &Value,
    form: &str,
    fields: &[&'static str],
) -> crate::Result<HashMap<String, String>> {
    fields
        .iter()
        .map(|key| {
            let text = match value.get(format!("{form}[{key}]")) {
                None => "",
                Some(value) => value.as_str().ok_or_else(|| invalid(key, "is invalid"))?,
            };
            // Never turn malformed values or a truncated reason into a
            // different, valid input before facade validation.
            Ok(((*key).into(), text.to_owned()))
        })
        .collect()
}
fn invalid(field: &'static str, message: &str) -> PayrollError {
    PayrollError::Invalid(vec![FieldError::new(field, message)])
}
fn account_id(
    values: &HashMap<String, String>,
    key: &'static str,
    required: bool,
) -> crate::Result<Option<i64>> {
    let text = get(values, key);
    if text.is_empty() {
        return if required {
            Err(invalid(key, "can't be blank"))
        } else {
            Ok(None)
        };
    }
    params::positive_id(Some(text))
        .map(Some)
        .ok_or_else(|| invalid(key, "is invalid"))
}
fn reversal(values: &HashMap<String, String>) -> crate::Result<Reversal> {
    let reason = crate::reason(Some(get(values, "reason")), true)?.expect("required reason");
    let date = get(values, "entry_date");
    let entry_date = if date.is_empty() {
        None
    } else {
        Some(
            date.parse()
                .map_err(|_| invalid("entry_date", "is invalid"))?,
        )
    };
    Ok(Reversal { reason, entry_date })
}
fn row(id: &str, cells: &[(&str, String)]) -> FlexRow {
    let mut row = FlexRow::new(id);
    row.dom_id = Some(id.into());
    for (key, text) in cells {
        row = row.slot(key, Html::text(text));
    }
    row
}
/// A row's own action (post, retry, reverse) as an icon button with its
/// name as the tooltip.
fn icon_action(id: &str, event: &str, value: i64, icon: &str, label: &str) -> Result<Html> {
    render(IconButton {
        id,
        event,
        name: "id",
        value: &value.to_string(),
        button_type: "button",
        title: label,
        ..IconButton::new(icon, label)
    })
}
fn section(
    id: &str,
    heading: &str,
    title: &str,
    description: &str,
    count: Option<usize>,
    content: Html,
) -> Result<Html> {
    let flat = matches!(
        id,
        "payroll-mappings"
            | "payroll-changes"
            | "payroll-postings-list"
            | "payroll-runs"
            | "payroll-claims"
    );
    let mut body = render(SectionHeading {
        id: heading,
        title,
        description,
        count,
        class: if flat { "p-5 pb-0" } else { "" },
        ..SectionHeading::default()
    })?;
    body.push(&content);
    render(Card {
        id,
        class: "mt-6",
        inner_class: if flat { "p-0" } else { "" },
        role: "region",
        labelledby: heading,
        body,
        ..Card::default()
    })
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageView<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
/// A dialog's form over its rendered fields (a combobox among them), then
/// Cancel and the one verb at the end of the row.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}<form id="{{ id }}" data-change data-event="{{ change }}" data-submit-event="{{ submit }}" class="mt-4 space-y-3">{{ body|safe }}<div class="flex w-full flex-wrap justify-end gap-2">{% call ui::button(id="payroll-mapping-cancel", button_type="button", event=cancel_event) %}Cancel{% endcall %}{% call ui::button(id=button_id, button_type="submit", variant=bilibi_ui::ButtonVariant::Primary, working=working) %}{{ label }}{% endcall %}</div></form>"#,
    ext = "html"
)]
struct HtmlFormView<'a> {
    id: &'a str,
    change: &'a str,
    submit: &'a str,
    body: Html,
    cancel_event: &'a str,
    button_id: &'a str,
    label: &'a str,
    working: &'a str,
}
/// A dialog inside the page, with Cancel in every state.
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as dialog %}{% call dialog::modal(id=id, title=title, cancel_event=cancel_event, description=description) %}{{ alert|safe }}{{ form|safe }}{% endcall %}"#,
    ext = "html"
)]
struct DialogView<'a> {
    id: &'a str,
    title: &'a str,
    description: &'a str,
    cancel_event: &'a str,
    alert: Html,
    form: Html,
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "postings" => Some("Payroll and claims"),
            "setup" => Some("Payroll posting accounts"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = State::new(self.clone(), request.operation, request.query)?;
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
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(State::new(self.clone(), request.operation, request.query)?);
            Ok(Some(state))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, value).await?;
            }
            // Bilimbi Payroll has no URL-driven controls; no inherited page or
            // page_size parameters are published from the shared ListState.
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}
