//! Bilimbi Finance.FixedAssets.Web: shared lists and the asset record page.
mod acquire;
mod categories;
mod detail;
mod index;
mod input;
mod opening;
mod runs;
mod source_evidence;

use crate::{FixedAssetsError, MODULE};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Provenance,
};
use bilibi_principal_directory::{PrincipalDirectory, PrincipalKind, PrincipalRef, Query};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::*, params, BuiltinColumn, Button, ButtonVariant, Cell, EmptyState, Field, FlexRow,
    FlexTable, Html, Instant, ListColumns, StatusKind, TableEmpty,
};
use std::{collections::HashMap, sync::Arc};

pub(crate) const PAGES: &str = "finance/fixed_assets.pages";
const DENIED: &str = "You do not have permission to do that.";
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub contracts: Arc<bilibi_agent_api::page::PageContracts>,
    pub plans: Arc<bilibi_agent_api::plans::Plans>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub directory: Arc<PrincipalDirectory>,
}
macro_rules! route {
    ($op:literal, $path:literal) => {
        inventory::submit! { Contribution::new(ROUTES, concat!("finance/fixed_assets.", $op), Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
            method: Method::Get, path: $path, service: PAGES, operation: $op,
            access: Access::Capability(crate::VIEW), framed: true,
        }) }
    };
}
route!("register", "/finance/fixed-assets/register");
route!("categories", "/finance/fixed-assets/categories");
route!("runs", "/finance/fixed-assets/runs");
route!("asset", "/finance/fixed-assets/register/{id}");
// Menu group (a bookkeeper's section of the Finance menu); its pages sit under it.
bilibi_menu::contribute!(
    "finance/fixed_assets",
    bilibi_menu::MenuItem::container("finance.group.fixed-assets", "Fixed assets")
        .under("finance")
        .icon("carbon-financial-assets")
        .order(40)
);

macro_rules! menu {
    ($kind:literal, $label:literal, $order:literal) => {
        bilibi_menu::contribute!(
            "finance/fixed_assets",
            bilibi_menu::MenuItem::page(
                concat!("finance.fixed-assets.", $kind),
                $label,
                concat!("/finance/fixed-assets/", $kind)
            )
            .company_scoped()
            .icon("carbon-financial-assets")
            .under("finance.group.fixed-assets")
            .requires(bilibi_menu::Requirement::Capability(crate::VIEW))
            .order($order)
        );
    };
}
menu!("register", "Fixed assets", 1);
menu!("runs", "Depreciation runs", 2);
menu!("categories", "Asset categories", 3);

fn render(t: impl Template) -> Result<Html> {
    Html::render(&t).map_err(|_| Error::Internal)
}
fn company(s: &Scope) -> Option<i64> {
    s.current_company().map(|p| p.get())
}
fn refusal(e: FixedAssetsError) -> String {
    refusal_text(&e)
}
fn refusal_text(e: &FixedAssetsError) -> String {
    use bilibi_finance_ledger::LedgerError;
    match e {
        FixedAssetsError::Invalid(fields)
        | FixedAssetsError::Ledger(LedgerError::Invalid(fields)) => fields
            .iter()
            .map(|f| match f.field {
                "base" => f.message.clone(),
                field => format!("{field}: {}", f.message),
            })
            .collect::<Vec<_>>()
            .join("; "),
        FixedAssetsError::Failed(_) | FixedAssetsError::Ledger(LedgerError::Failed(_)) => {
            "The record changed while you were working. Reload the page and check it.".into()
        }
        e => e.to_string(),
    }
}
fn cols(id: &str, labels: &[(&str, &str)]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        labels
            .iter()
            .map(|(key, label)| BuiltinColumn::new(key, label))
            .collect(),
    )
    .map_err(|_| Error::Internal)
}
fn table(
    id: &str,
    columns: &ListColumns,
    rows: Vec<FlexRow>,
    caption: &str,
    event: &'static str,
    empty: &str,
) -> Result<Html> {
    let mut t = FlexTable::new(id, columns.flex_columns(), rows);
    t.event = event;
    t.zoom = columns.zoom;
    t.suggestions = columns.flex_suggestions();
    t.add_query = &columns.add_query;
    t.caption = caption;
    t.framed = true;
    // Rows that offer a next step get the trailing actions column.
    t.actions = t.rows.iter().any(|r| !r.actions.is_empty());
    t.empty = Some(TableEmpty::Content(Html::text(empty)));
    render(t)
}
fn time_cell(display: &DisplayContext, value: &str) -> Result<Cell> {
    let at = value
        .parse::<jiff::civil::DateTime>()
        .map_err(|_| Error::Invalid)?;
    // The schema stores naive UTC, as DisplayContext::render_naive_utc defines.
    let r = display
        .render_naive_utc(
            at,
            bilibi_datetime::Format::DateTime,
            bilibi_datetime::Precision::Minute,
        )
        .ok_or(Error::Invalid)?;
    Ok(Cell::instant(Instant::new(
        &r.iso,
        &r.text,
        &r.company,
        &r.utc,
        r.mode.as_str(),
    )))
}
/// People's names for the users a page shows, from the Platform's principal
/// directory; someone it cannot name keeps the durable "User 5".
pub(super) struct People(HashMap<i64, String>);
impl People {
    pub(super) fn name(&self, user: i64, impersonator: Option<i64>) -> String {
        let one = |id: i64| {
            self.0
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("User {id}"))
        };
        match impersonator {
            None => one(user),
            Some(by) => format!("{} (impersonated by {})", one(user), one(by)),
        }
    }
}

/// A ledger entry's number as a link to its journal page.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/ledger/journals/entry-{{ id }}" class="text-link hover:underline tabular-nums whitespace-nowrap">{{ label }}</a>"#,
    ext = "html"
)]
struct EntryLink<'a> {
    id: i64,
    label: &'a str,
}
pub(super) fn entry_link(id: Option<i64>, number: Option<i64>) -> Result<Html> {
    match (id, number) {
        (Some(id), Some(number)) => render(EntryLink {
            id,
            label: &format!("Entry {number}"),
        }),
        _ => Ok(Html::text("—")),
    }
}

/// A month as people say it ("October 2026").
pub(super) fn month(date: jiff::civil::Date) -> String {
    date.strftime("%B %Y").to_string()
}

/// A form dialog laid out as the shared specimen: the form under the title,
/// Cancel then the one primary verb at the end of the row. A `change` event
/// keeps the draft on the server as fields change; `submit` is what the
/// primary button raises. `alert` is a refusal that has no field.
pub(super) struct Dialog<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub change: &'a str,
    pub submit: &'a str,
    pub cancel: &'a str,
    pub verb: &'a str,
    pub working: &'a str,
    pub alert: &'a str,
    pub fields: Vec<Field>,
    /// Shown under the alert, before the fields: the way to clear a refusal,
    /// or what the dialog shows instead of fields.
    pub extra: Html,
    /// False when nothing can be submitted yet; only Cancel is offered.
    pub can_submit: bool,
}
impl Dialog<'_> {
    pub(super) fn render(self) -> Result<Html> {
        let mut body = Html::EMPTY;
        if !self.alert.is_empty() {
            body.push(&render(bilibi_ui::Alert {
                id: &format!("{}-alert", self.id),
                kind: StatusKind::Error,
                body: Html::text(self.alert),
                ..Default::default()
            })?);
        }
        body.push(&self.extra);
        body.push(&fields(&format!("{}-form", self.id), &self.fields)?);
        let mut actions = render(Button {
            id: &format!("{}-cancel", self.id),
            button_type: "button",
            event: self.cancel,
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        if self.can_submit {
            actions.push(&render(Button {
                id: &format!("{}-submit", self.id),
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: self.working,
                body: Html::text(self.verb),
                ..Button::default()
            })?);
        }
        let form = bilibi_ui::Form {
            id: &format!("{}-form", self.id),
            event: if self.change.is_empty() {
                self.submit
            } else {
                self.change
            },
            change: !self.change.is_empty(),
            class: "mt-4",
            body,
            actions: render(DialogActions { buttons: actions })?,
        };
        let form = if self.change.is_empty() {
            render(form)?
        } else {
            form.submit_event(self.submit)
                .map_err(|_| Error::Internal)?
        };
        render(bilibi_ui::Modal {
            id: self.id,
            title: self.title,
            description: self.description,
            cancel_event: self.cancel,
            body: form,
            ..Default::default()
        })
    }
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex w-full flex-wrap justify-end gap-2">{{ buttons|safe }}</div>"#,
    ext = "html"
)]
struct DialogActions {
    buttons: Html,
}

/// The fields of a dialog: the problems first, each linking to its field,
/// then each field; a record choice (an account, a category, a purchase) is a
/// searchable combobox, name first.
pub(super) fn fields(form: &str, fields: &[Field]) -> Result<Html> {
    let mut summary = bilibi_ui::ErrorSummary::new(format!("{form}-errors"));
    for field in fields {
        for error in &field.errors {
            summary.push(&field.id, bilibi_ui::error_sentence(&field.label, error));
        }
    }
    let mut body = render(summary)?;
    for original in fields {
        let mut field = original.clone();
        field.errors = field
            .errors
            .iter()
            .map(|e| bilibi_ui::error_sentence(&field.label, e))
            .collect();
        if field.name.ends_with("_id]") || field.name.ends_with("[source]") {
            // The combobox draws no required mark of its own yet.
            let label = if field.attrs.required {
                format!("{} *", field.label)
            } else {
                field.label.clone()
            };
            let control = bilibi_ui::Combobox::new(&field.id, &field.name)
                .label(label)
                .value(&field.value)
                .placeholder(field.attrs.placeholder.clone().unwrap_or_default())
                .hint(field.hint.clone())
                .options(
                    field
                        .options
                        .iter()
                        .filter(|c| !c.value.is_empty())
                        .cloned()
                        .collect(),
                )
                .errors(field.errors.clone())
                .attrs(field.attrs.clone())
                .aria_label(&field.label);
            body.push(&render(control)?);
        } else {
            body.push(&render(field.clone())?);
        }
    }
    Ok(body)
}

/// An action on the row's own record: a plain icon button with a tooltip
/// that opens its dialog. The "Non-local actions" menu holds only actions that take
/// the record to another module (New invoice, Add to…), and Fixed assets
/// rows have none, so they show no such menu.
pub(super) fn row_icon(id: &str, icon: &str, label: &str, href: &str) -> Result<Html> {
    let mut button = bilibi_ui::IconButton::new(icon, label);
    button.id = id;
    button.href = href;
    button.title = label;
    render(button)
}

/// An action this person may not take is left out, not offered and refused.
pub(super) fn header_button(id: &str, event: &str, label: &str, primary: bool) -> Result<Html> {
    render(Button {
        id,
        button_type: "button",
        event,
        variant: if primary {
            ButtonVariant::Primary
        } else {
            ButtonVariant::Secondary
        },
        body: Html::text(label),
        ..Button::default()
    })
}
impl Pages {
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let c = self.db.get().await?;
        Ok(bilibi_authz::decide(&**c, scope, capability).await?.allowed)
    }
    pub(super) async fn people(
        &self,
        scope: &Scope,
        users: impl IntoIterator<Item = i64>,
    ) -> Result<People> {
        let wanted: Vec<PrincipalRef> = users
            .into_iter()
            .filter_map(|id| PrincipalRef::new(PrincipalKind::User, id).ok())
            .collect();
        if wanted.is_empty() {
            return Ok(People(HashMap::new()));
        }
        let c = self.db.get().await?;
        // A directory that cannot answer leaves the durable "User 5".
        let named = match self
            .directory
            .rank(&**c, scope, &wanted, &Query::default())
            .await
        {
            Ok(ranked) => ranked
                .named
                .into_iter()
                .map(|n| (n.principal.id(), n.name))
                .collect(),
            Err(_) => HashMap::new(),
        };
        Ok(People(named))
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let c = self.db.get().await?;
        let locale = self.locales.resolve(&**c, scope, None).await?.locale;
        self.display.display(&**c, scope, None, locale).await
    }
    fn state(
        &self,
        operation: &str,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        if operation == "asset" {
            Ok(Box::new(detail::Detail::new(self, path, query, framed)?))
        } else {
            Ok(Box::new(index::Index::new(self, operation, query, framed)?))
        }
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "register" => Some("Fixed assets"),
            "categories" => Some("Asset categories"),
            "runs" => Some("Depreciation runs"),
            "asset" => Some("Fixed asset"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(
                request.operation,
                request.path,
                request.query,
                request.framed,
            )?;
            let html = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?
                .html;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(self.state(
                request.operation,
                request.path,
                request.query,
                request.framed,
            )?))
        })
    }
}

// A delayed success timeout must not dismiss a newer refusal.
fn clear_flash(flash: &mut Option<(StatusKind, String)>, value: &serde_json::Value) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| value["key"].as_str() == Some(kind.name()))
    {
        *flash = None;
    }
}

fn flash_map(flash: &Option<(StatusKind, String)>) -> bilibi_ui::FlashMap<'_> {
    let mut messages = bilibi_ui::NO_FLASH;
    if let Some((kind, text)) = flash {
        match kind {
            StatusKind::Error => messages.error = text,
            StatusKind::Success => messages.success = text,
            StatusKind::Warning => messages.warning = text,
            StatusKind::Info => messages.info = text,
        }
    }
    messages
}

inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, bilibi_module_registry::Provenance { module: MODULE, source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: PAGES,
    contracts: &[
        ("preview_opening", "finance.fixed_assets.opening.import"),
        ("import_opening", "finance.fixed_assets.opening.import"),
        ("preview_opening", "finance.fixed_assets.opening.stage"),
        ("import_opening", "finance.fixed_assets.opening.stage"),

        ("create_category","finance.fixed_assets.category.save"),
        ("save_category","finance.fixed_assets.category.save"),
        ("register_asset","finance.fixed_assets.asset.acquire"),
        ("run_depreciation","finance.fixed_assets.run.post"),
        ("reverse_run","finance.fixed_assets.run.reverse"),
        ("dispose","finance.fixed_assets.asset.dispose"),
        ("reverse_disposal","finance.fixed_assets.disposal.reverse"),
        ("void","finance.fixed_assets.acquisition.void"),
        ("confirm_void","finance.fixed_assets.acquisition.void"),
    ],
    pending: bilibi_module_registry::contributions::Pending::None,
}) }

fn contract_error(error: bilibi_agent_api::Error) -> FixedAssetsError {
    use bilibi_agent_api::Error as E;
    match error {
        E::Forbidden => FixedAssetsError::Unauthorized,
        E::InvalidInput(fields) => FixedAssetsError::Invalid(
            fields
                .into_iter()
                .flat_map(|(key, messages)| {
                    let field = FIELD_NAMES
                        .iter()
                        .find(|name| **name == key)
                        .copied()
                        .unwrap_or("base");
                    messages
                        .into_iter()
                        .map(move |message| bilibi_finance_ledger::FieldError::new(field, message))
                })
                .collect(),
        ),
        E::Backend(Error::Conflict) => crate::invalid(
            "base",
            "The reviewed facts changed. Refresh and review this action again.",
        ),
        E::Backend(e) => FixedAssetsError::Failed(e),
        e => crate::invalid("base", e.to_string()),
    }
}
const FIELD_NAMES: &[&str] = &[
    "base",
    "name",
    "category_id",
    "cost_account_id",
    "accumulated_account_id",
    "expense_account_id",
    "gain_account_id",
    "loss_account_id",
    "method",
    "life_months",
    "annual_rate",
    "reason",
    "source_id",
    "line_number",
    "entry_id",
    "entry_line_number",
    "bill_id",
    "bill_line_number",
    "category_revision_id",
    "description",
    "in_service_date",
    "residual_value",
    "period",
    "run_id",
    "asset_id",
    "effective_date",
    "proceeds",
    "settlement_account_id",
];
impl Pages {
    async fn minor_units(
        &self,
        s: &Scope,
        company: i64,
    ) -> crate::Result<bilibi_finance_ledger::MinorUnits> {
        let book: Option<serde_json::Value> = self
            .read(
                s,
                "finance.ledger.book.get",
                serde_json::json!({"company_id":company}),
                "book",
            )
            .await?;
        book.and_then(|b| b["minor_units"].as_i64())
            .and_then(bilibi_finance_ledger::MinorUnits::new)
            .ok_or(FixedAssetsError::Ledger(
                bilibi_finance_ledger::LedgerError::BookNotOpen,
            ))
    }
    fn call<'a>(
        &self,
        s: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> bilibi_agent_api::page::Call<'a> {
        bilibi_agent_api::page::Call {
            scope: s,
            credential,
            service: PAGES,
            event,
            contract,
        }
    }
    async fn read<T: serde::de::DeserializeOwned>(
        &self,
        s: &Scope,
        key: &str,
        mut input: serde_json::Value,
        field: &str,
    ) -> crate::Result<T> {
        omit_empty(&mut input);
        let result = self
            .contracts
            .read(s, key, &input)
            .await
            .map_err(contract_error)?;
        serde_json::from_value(result["result"][field].clone())
            .map_err(|_| FixedAssetsError::Failed(Error::Internal))
    }
    async fn submit(
        &self,
        s: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: serde_json::Value,
    ) -> crate::Result<serde_json::Value> {
        input["company_id"] = company(s).ok_or(FixedAssetsError::CompanyNotFound)?.into();
        omit_empty(&mut input);
        let result = self
            .contracts
            .submit(&self.call(s, credential, event, contract), input)
            .await
            .map_err(contract_error)?;
        Ok(result["result"]["record"].clone())
    }
}

impl Pages {
    async fn account_context(
        &self,
        scope: &Scope,
    ) -> Result<(
        Option<bilibi_finance_ledger::MinorUnits>,
        Vec<(i64, String, String, bool, String)>,
    )> {
        let Some(company) = scope.current_company().map(|c| c.get()) else {
            return Ok((None, Vec::new()));
        };
        let mut input = serde_json::json!({"company_id":company});
        let book = match self
            .contracts
            .read(scope, "finance.ledger.book.get", &input)
            .await
        {
            Ok(book) => book,
            // A document viewer need not have permission to read the chart.
            Err(
                bilibi_agent_api::Error::Forbidden
                | bilibi_agent_api::Error::Backend(Error::Forbidden | Error::Unauthorized),
            ) => return Ok((None, Vec::new())),
            Err(_) => return Err(Error::Internal),
        };
        let units = book["result"]["book"]["minor_units"]
            .as_i64()
            .and_then(bilibi_finance_ledger::MinorUnits::new);
        let mut accounts = Vec::new();
        loop {
            let page = self
                .contracts
                .read(scope, "finance.ledger.account.list", &input)
                .await
                .map_err(|_| Error::Internal)?;
            for a in page["result"]["records"].as_array().into_iter().flatten() {
                if let (Some(id), Some(code), Some(name), Some(kind), Some(active)) = (
                    a["id"].as_i64(),
                    a["code"].as_str(),
                    a["name"].as_str(),
                    a["account_type"].as_str(),
                    // A group takes no postings, so it is never a choice.
                    a["active"]
                        .as_bool()
                        .map(|active| active && a["is_group"].as_bool() != Some(true)),
                ) {
                    accounts.push((
                        id,
                        name.to_owned(),
                        kind.to_owned(),
                        active,
                        code.to_owned(),
                    ));
                }
            }
            if let Some(code) = page["result"]["next_after_code"].as_str() {
                input["after_code"] = serde_json::json!(code);
            } else {
                break;
            }
        }
        Ok((units, accounts))
    }
}

fn omit_empty(input: &mut serde_json::Value) {
    if let Some(fields) = input.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
}
