//! The customers list and its Add customer dialog. Reads
//! `finance.receivables.customer.list`; Add chooses or creates the Core
//! Company or Person (`company_pick`) and submits `customer.create`, which records
//! the company, Finance customer and relationship in one transaction.
use super::{
    company_pick::Submitted,
    format::{self as f, Errors},
    render, Flash, Pages, CUSTOMERS,
};
use crate::Customer;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::MultiPartyChooser;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BuiltinColumn, Button, ButtonVariant, Cell, Choice, ColumnsOutcome, CreateButton,
    EmptyState, Field, Filter, FilterToolbar, FlexRow, FlexTable, Form, Header, Html, InputKind,
    ListColumns, ListSpec, ListState, ModalWidth, PageCounts, PageVariant, Pagination, RecordLink,
    SortDir, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([("code".into(), SortDir::Asc), ("name".into(), SortDir::Asc)]),
        default_sort: Some("code".into()),
        page_sizes: SIZES.to_vec(),
        filters: [(
            "status".into(),
            Filter::one_of(&["", "active", "inactive"], ""),
        )]
        .into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("customers list spec")
});

/// The customer's own fields in the Add customer form:
/// `(key, label, kind, maximum, required, hint)`.
pub(super) const FIELDS: &[(&str, &str, InputKind, &str, bool, &str)] = &[(
    "payment_terms_days",
    "Payment terms (days)",
    InputKind::Number,
    "4",
    false,
    "Days after the invoice date that payment is due. Leave empty for due on issue.",
)];
const FORM: &str = "customer";
const PREFIX: &str = "receivables-customer-new";

/// The Add customer dialog while it is open.
struct Create {
    values: HashMap<String, String>,
    chooser: MultiPartyChooser,
    errors: Errors,
}

pub(super) struct Customers {
    pages: Pages,
    /// Records an "Add to…" link carried (`?drop=`), opened once on first use.
    dropped: Option<String>,
    list: ListState,
    columns: ListColumns,
    all: Vec<Customer>,
    cut: bool,
    can_add: bool,
    create: Option<Create>,
    flash: Flash,
    framed: bool,
}

impl Customers {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        let columns = vec![
            BuiltinColumn::new("code", "Code").sortable("code"),
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("company", "Core status"),
            BuiltinColumn::new("email", "Email"),
            BuiltinColumn::new("phone", "Phone"),
            BuiltinColumn::new("terms", "Terms").right(),
            BuiltinColumn::new("status", "Status"),
        ];
        Ok(Self {
            pages,
            dropped: query.get(bilibi_records::DROP_PARAM).cloned(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("receivables-customers-table", columns)
                .map_err(|_| Error::Internal)?,
            all: vec![],
            cut: false,
            can_add: false,
            create: None,
            flash: Flash::default(),
            framed,
        })
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.open_dropped(scope).await?;
        self.body()
    }

    /// The live page opens on the same records the first render did.
    pub(super) async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        self.open_dropped(scope).await
    }

    async fn open_dropped(&mut self, scope: &Scope) -> Result<()> {
        match self.dropped.take() {
            Some(text) => self.receive_drop(scope, &text).await,
            None => Ok(()),
        }
    }

    /// Opens the Add customers dialog, as "+ Customer" does. False when the
    /// person may not add customers.
    async fn open_create(&mut self, scope: &Scope) -> Result<bool> {
        if !self.pages.allowed(scope, crate::MANAGE_CUSTOMERS).await? {
            self.flash.set(StatusKind::Error, super::DENIED);
            return Ok(false);
        }
        let (customers, _) = self
            .pages
            .read_all::<Customer>(
                scope,
                "finance.receivables.customer.list",
                json!({}),
                ("after", "next_after_id"),
            )
            .await?;
        let existing = customers
            .into_iter()
            .map(|customer| {
                (
                    customer.party.reference(),
                    format!("/finance/receivables/customers/{}", customer.id),
                )
            })
            .collect();
        self.create = Some(Create {
            values: HashMap::new(),
            chooser: MultiPartyChooser::new(PREFIX, FORM).existing(
                "Already a customer",
                "View customer",
                existing,
            ),
            errors: Errors::default(),
        });
        Ok(true)
    }

    /// Companies and contacts dropped on the page, or carried by "Add to…":
    /// the Add customers dialog opens with them chosen, as if each had been
    /// found in the chooser. The package is a request only: `Records::receive`
    /// re-reads every record under this person's scope, the chooser reads it
    /// again, and `customer.create` decides when the dialog is confirmed.
    async fn receive_drop(&mut self, scope: &Scope, text: &str) -> Result<()> {
        if !self.pages.allowed(scope, crate::MANAGE_CUSTOMERS).await? {
            self.flash.set(StatusKind::Error, super::DENIED);
            return Ok(());
        }
        let dropped = match self
            .pages
            .records
            .receive(scope, super::CUSTOMER_TARGET, text)
            .await?
        {
            Ok(dropped) => dropped,
            Err(refusal) => {
                self.flash.set(StatusKind::Error, refusal.message());
                return Ok(());
            }
        };
        if self.create.is_none() && !self.open_create(scope).await? {
            return Ok(());
        }
        let picker = self.pages.party_picker();
        let mut refused = 0;
        if let Some(create) = self.create.as_mut() {
            for record in &dropped {
                match create
                    .chooser
                    .add(&picker, scope, &record.reference.kind, &record.id)
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Invalid | Error::NotFound | Error::Forbidden) => refused += 1,
                    Err(error) => return Err(error),
                }
            }
            create.errors = Errors::default();
        }
        if refused > 0 {
            self.flash.set(
                StatusKind::Error,
                format!(
                    "{refused} of the dropped records could not be added: they are already customers or not available to you."
                ),
            );
        }
        Ok(())
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        (self.all, self.cut) = self
            .pages
            .read_all(
                scope,
                "finance.receivables.customer.list",
                json!({}),
                ("after", "next_after_id"),
            )
            .await?;
        self.can_add = self.pages.allowed(scope, crate::MANAGE_CUSTOMERS).await?;
        Ok(())
    }

    /// The rows the filters, search and sort leave, and the page asked for.
    fn shown(&mut self) -> (Vec<&Customer>, u64) {
        let search = self.list.search.trim().to_lowercase();
        let status = self.list.filters.get("status").cloned().unwrap_or_default();
        let mut rows: Vec<&Customer> = self
            .all
            .iter()
            .filter(|c| match status.as_str() {
                "active" => c.active,
                "inactive" => !c.active,
                _ => true,
            })
            .filter(|c| {
                search.is_empty()
                    || [Some(&c.code), Some(&c.name), email(c), phone(c)]
                        .into_iter()
                        .flatten()
                        .any(|t| t.to_lowercase().contains(&search))
            })
            .collect();
        match self.list.sort_by.as_deref() {
            Some("name") => rows.sort_by_key(|c| c.name.to_lowercase()),
            _ => rows.sort_by(|a, b| a.code.cmp(&b.code)),
        }
        if self.list.sort_dir == Some(SortDir::Desc) {
            rows.reverse();
        }
        let total = rows.len() as u64;
        let size = self.list.page_size.max(1);
        self.list.page = self.list.page.clamp(1, total.div_ceil(size).max(1));
        let from = ((self.list.page - 1) * size) as usize;
        (
            rows.into_iter().skip(from).take(size as usize).collect(),
            total,
        )
    }

    fn body(&mut self) -> Result<String> {
        let framed = self.framed;
        let (rows, total) = self.shown();
        let shown: Vec<Customer> = rows.into_iter().cloned().collect();
        self.columns.load(&shown, |c| c.id.to_string());
        let actions = if self.can_add {
            render(CreateButton {
                noun: "Customer",
                id: "receivables-customers-add",
                event: "show_create",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let mut content = render(FilterToolbar {
            event: "filter",
            ..FilterToolbar::new(
                "receivables-customers-filters",
                vec![
                    ToolbarControl::search(
                        "receivables-customers-search",
                        "search",
                        "Search customers",
                        &self.list.search,
                        "Search by code, name, email or phone…",
                    ),
                    ToolbarControl::select(
                        "receivables-customers-status",
                        "status",
                        "Filter by status",
                        &self.list.filters["status"],
                        [
                            ("All customers", ""),
                            ("Active", "active"),
                            ("Inactive", "inactive"),
                        ]
                        .map(|(l, v)| Choice::new(l, v))
                        .to_vec(),
                    ),
                ],
            )
        })?;
        let mut flex_rows = Vec::new();
        for c in &shown {
            let mut row = FlexRow::new(c.id.to_string());
            row.dom_id = Some(format!("receivables-customer-{}", c.id));
            flex_rows.push(
                row.slot(
                    "code",
                    render(RecordLink {
                        workspace: framed,
                        kind: crate::MODULE,
                        record_id: &c.id.to_string(),
                        href: &format!("{CUSTOMERS}/{}", c.id),
                        class: "font-medium text-ink-strong hover:underline",
                        body: Html::text(&c.code),
                    })?,
                )
                .cell("name", Cell::text(&c.name))
                .slot("company", company_badge(c)?)
                .cell("email", Cell::text(f::optional(&email(c).cloned())))
                .cell("phone", Cell::text(f::optional(&phone(c).cloned())))
                .slot(
                    "terms",
                    f::tabular(&format!("{} days", c.payment_terms_days))?,
                )
                .slot("status", f::active_badge(c.active)?),
            );
        }
        let mut table = FlexTable::new(
            "receivables-customers-table",
            self.columns.flex_columns(),
            flex_rows,
        )
        .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Customers";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(render(EmptyState::missing(
            "No customers",
            if self.all.is_empty() {
                "Add the companies you invoice, then raise invoices to them."
            } else {
                "No customer matches the current filters."
            },
        ))?));
        content.push(&render(table)?);
        content.push(&render(Pagination {
            filters_event: "filter",
            ..Pagination::new(
                "receivables-customers-pagination",
                PageCounts::new(self.list.page, self.list.page_size, total),
                &SIZES,
            )
        })?);
        if self.cut {
            content.push(&Html::text(
                "Only the first customers are listed. Search to narrow them.",
            ));
        }
        let header = render(Header {
            title: Html::text("Customers"),
            title_actions: bilibi_ui::Html::EMPTY,
            actions,
            ..Header::default()
        })?;
        let mut body = header;
        body.push(&content);
        let modal = match &self.create {
            Some(create) => Some(self.dialog(create)?),
            None => None,
        };
        let html = f::frame(
            "receivables-customers",
            PageVariant::List,
            &self.flash,
            body,
            modal,
        )?;
        // Companies and contacts dropped on the page open the same dialog as
        // "+ Customer", so the tile accepts them only while the button shows.
        let zone = self
            .pages
            .records
            .drop_target(super::CUSTOMER_TARGET)
            .filter(|_| self.can_add)
            .map(|target| target.zone());
        Ok(match zone {
            Some(zone) => zone
                .wrap("receivables-customers-drop", Html::raw(html))
                .to_string(),
            None => html,
        })
    }

    fn dialog(&self, create: &Create) -> Result<Html> {
        let mut labels: Vec<_> = FIELDS
            .iter()
            .map(|(key, label, ..)| {
                (
                    (*key).to_owned(),
                    format!("{PREFIX}-{key}"),
                    (*label).to_owned(),
                )
            })
            .collect();

        labels.extend(create.chooser.labels());
        let mut body = render(create.errors.summary(&format!("{PREFIX}-errors"), &labels))?;
        let mut own = Html::EMPTY;
        for (key, label, kind, max, required, hint) in FIELDS {
            if *key == "code" && !create.chooser.is_new() {
                continue;
            }
            let mut field = Field::in_form(FORM, key)
                .id(format!("{PREFIX}-{key}"))
                .label(*label)
                .kind(*kind)
                .value(create.values.get(*key).cloned().unwrap_or_default())
                .errors(create.errors.under(key, label));
            field = if *kind == InputKind::Number {
                field.attr("min", "0").attr("max", "3650").attr("step", "1")
            } else {
                field.attr("maxlength", max)
            };
            if *required {
                field = field.required();
            }
            if !hint.is_empty() {
                field = field.hint(*hint);
            }
            own.push(&render(field)?);
        }
        body.push(&f::group(
            "receivables-customer-company",
            "Company or person",
            create.chooser.render(&create.errors)?,
        )?);
        body.push(&f::group("receivables-customer-terms", "In Finance", own)?);
        let actions = {
            let mut a = render(Button {
                id: &format!("{PREFIX}-cancel"),
                button_type: "button",
                event: "cancel_create",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            a.push(&render(Button {
                id: &format!("{PREFIX}-save"),
                button_type: "button",
                event: "create_customer",
                variant: ButtonVariant::Primary,
                working: "Adding…",
                body: Html::text(if create.chooser.count() == 1 {
                    "Add customer"
                } else {
                    "Add customers"
                }),
                ..Button::default()
            })?);
            f::actions(a)?
        };
        let form = render(Form {
            id: &format!("{PREFIX}-form"),
            event: "customer_change",
            change: true,
            class: "mt-4",
            body,
            actions,
        })?;
        f::modal(
            "receivables-customer-new-modal",
            "Add customers",
            "",
            "cancel_create",
            ModalWidth::Wide,
            &self.flash,
            form,
        )
    }

    /// The dialog's own values and the company chooser's, from a posted form.
    async fn take_form(&mut self, _scope: &Scope, v: &serde_json::Value) -> Result<()> {
        let form = params::form_values(v, FORM);
        if form.is_empty() {
            return Ok(());
        }
        let Some(create) = self.create.as_mut() else {
            return Ok(());
        };
        for (key, ..) in FIELDS {
            if let Some(value) = form.get(*key) {
                create.values.insert((*key).to_owned(), value.clone());
            }
        }
        create.chooser.read(&form);
        Ok(())
    }

    async fn add(&mut self, scope: &Scope, credential: &str) -> Result<Option<String>> {
        let pages = self.pages.clone();
        let Some(create) = self.create.as_mut() else {
            return Ok(None);
        };
        create.errors = Errors::default();
        let terms = f::posted(&create.values, "payment_terms_days");
        let days = if terms.is_empty() {
            0
        } else if let Ok(days) = terms.parse::<i64>() {
            days
        } else {
            create.errors.0.insert(
                "payment_terms_days".into(),
                vec!["must be a whole number of days".into()],
            );
            return Ok(None);
        };
        if create.chooser.count() == 0 {
            create
                .errors
                .0
                .insert("base".into(), vec!["Choose a company or person.".into()]);
            return Ok(None);
        }
        let mut added = 0;
        let mut pending_new = false;
        if create.chooser.is_new() {
            let input = json!({"payment_terms_days":days});
            match create
                .chooser
                .chooser()
                .submit_composed(
                    &pages.party_picker(),
                    scope,
                    credential,
                    "create_customer",
                    "finance.receivables.customer.create",
                    input,
                )
                .await?
            {
                Submitted::Done(_) => {
                    added += 1;
                    create.chooser.cancel_new();
                }
                Submitted::Invalid(errors) => {
                    create.errors = errors;
                    pending_new = true;
                }
                Submitted::Refused(message) => {
                    create.errors.0.insert("base".into(), vec![message]);
                    pending_new = true;
                }
                Submitted::Review => {
                    create.errors = Errors::default();
                    pending_new = true;
                }
            }
        }
        let rows: Vec<_> = create
            .chooser
            .rows()
            .iter()
            .map(|r| (r.id.clone(), r.code.clone()))
            .collect();
        for (id, _code) in rows {
            create.chooser.activate(&id)?;
            let input = json!({"payment_terms_days":days});
            match create
                .chooser
                .chooser()
                .submit_composed(
                    &pages.party_picker(),
                    scope,
                    credential,
                    "create_customer",
                    "finance.receivables.customer.create",
                    input,
                )
                .await?
            {
                Submitted::Done(record) => {
                    added += 1;
                    if let Some(customer_id) = record["id"].as_i64() {
                        create
                            .chooser
                            .mark_existing(&id, format!("{CUSTOMERS}/{customer_id}"));
                    }
                    create.chooser.remove(&id);
                }
                Submitted::Invalid(errors) => create.chooser.row_errors(&id, errors),
                Submitted::Refused(message) => create.chooser.row_errors(
                    &id,
                    Errors(BTreeMap::from([("base".into(), vec![message])])),
                ),
                Submitted::Review => return Err(Error::Invalid),
            }
        }
        if pending_new {
            create.chooser.resume_new();
        }
        if create.chooser.count() == 0 {
            self.create = None;
            self.flash.set(
                StatusKind::Success,
                if added == 1 {
                    "Customer added."
                } else {
                    "Customers added."
                },
            );
            self.load(scope).await?;
        } else if added > 0 {
            self.flash.set(
                StatusKind::Success,
                format!("{added} customers added. Review the remaining rows."),
            );
        }
        Ok(None)
    }

    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        v: &serde_json::Value,
    ) -> Result<LiveBody> {
        let mut redirect = None;
        let pages = self.pages.clone();
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => self.flash.dismiss(v),
            "filter" => self.list.apply_filters(&params::form_values(v, "filters")),
            "page" => self.list.put_page(v["page"].as_str()),
            "grid" => {
                if let ColumnsOutcome::Sort(column) = self.columns.handle(&params::string_values(v))
                {
                    self.list.next_sort(&column);
                }
            }
            "show_create" => {
                self.open_create(scope).await?;
            }
            bilibi_records::DROP_EVENT => {
                let text = v["records"].as_str().ok_or(Error::Invalid)?;
                self.receive_drop(scope, text).await?;
            }
            "cancel_create" => self.create = None,
            "customer_change" => self.take_form(scope, v).await?,
            "search_companies" => {
                if let Some(create) = self.create.as_mut() {
                    create
                        .chooser
                        .search(
                            &pages.party_picker(),
                            scope,
                            v["query"].as_str().unwrap_or(""),
                        )
                        .await?;
                }
            }
            "pick_companies" => {
                if let Some(create) = self.create.as_mut() {
                    create
                        .chooser
                        .pick(
                            &pages.party_picker(),
                            scope,
                            v["value"].as_str().ok_or(Error::Invalid)?,
                        )
                        .await?;
                    create.errors = Errors::default();
                }
            }
            "remove_company" => {
                if let Some(create) = self.create.as_mut() {
                    create
                        .chooser
                        .remove(v["value"].as_str().ok_or(Error::Invalid)?);
                }
            }
            "new_company" => {
                if let Some(create) = self.create.as_mut() {
                    create.chooser.start_new(v["query"].as_str().unwrap_or(""));
                }
            }
            "cancel_new_company" => {
                if let Some(create) = self.create.as_mut() {
                    create.chooser.cancel_new();
                }
            }
            "use_candidate" => {
                if let Some(create) = self.create.as_mut() {
                    create
                        .chooser
                        .use_candidate(
                            &pages.party_picker(),
                            scope,
                            v["company"].as_str().ok_or(Error::Invalid)?,
                        )
                        .await?;
                    create.errors = Errors::default();
                }
            }
            "create_customer" => {
                self.take_form(scope, v).await?;
                redirect = self.add(scope, credential).await?;
            }
            _ => return Err(Error::Invalid),
        }
        let flash = redirect.is_some().then(|| self.flash.0.take()).flatten();
        Ok(LiveBody {
            html: self.body()?,
            query: self.list.to_params(),
            redirect,
            flash,
            ..LiveBody::default()
        })
    }
}

fn email(c: &Customer) -> Option<&String> {
    c.party.email.as_ref()
}
fn phone(c: &Customer) -> Option<&String> {
    c.party.phone.as_ref()
}
pub(super) fn company_badge(c: &Customer) -> Result<Html> {
    use bilibi_ui::BadgeKind;
    match c.party.status.as_str() {
        "active" => f::badge("Active", BadgeKind::Success),
        "pending" => f::badge("Pending", BadgeKind::Warning),
        "suspended" => f::badge("Suspended", BadgeKind::Warning),
        "archived" | "retired" => f::badge("Unavailable", BadgeKind::Neutral),
        _ => f::badge("No access", BadgeKind::Neutral),
    }
}

impl LiveState<Scope, Error> for Customers {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh => self.load(r.scope).await?,
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(v) => return self.event(r.scope, r.credential, v).await,
            }
            Ok(LiveBody {
                html: self.body()?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
