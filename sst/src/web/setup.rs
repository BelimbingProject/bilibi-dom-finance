//! Bilimbi Sst.Web.SetupLive: the company's SST setup. Its registration
//! (numbers, taxable period cycle, service tax basis) with every version
//! recorded, and which return box each receivables and payables tax code
//! reports in, with every mapping recorded. Every write is a facade call that
//! re-asks its capability; nothing here is edited in place.
use super::{
    clear_flash, columns, company, flash, format as f, render, table, time, Pages, DENIED, INDEX,
};
use crate::{
    Mapping, MappingInput, Registration, RegistrationInput, Sst, SstError, TaxCodeMapping,
    TaxSource, MANAGE_SETTINGS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::FieldError;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Cell, Choice, EmptyState, Fact, Facts, Field, FlashMap, FlexRow, Html, InputKind,
    ListColumns, StatusKind,
};
use std::collections::HashMap;

/// A form's values and the facade's field errors for it.
#[derive(Default)]
struct Draft {
    values: HashMap<String, String>,
    errors: Vec<FieldError>,
}
impl Draft {
    fn value(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }
    fn field(&self, form: &str, key: &str, id: &str, label: &str) -> Field {
        Field::in_form(form, key)
            .id(id)
            .label(label)
            .value(self.value(key))
            .errors(
                self.errors
                    .iter()
                    .filter(|e| e.field == key)
                    .map(|e| e.message.clone())
                    .collect(),
            )
    }
}

pub(super) struct Setup {
    pages: Pages,
    registrations: Option<Vec<Registration>>,
    tax_codes: Option<Vec<TaxCodeMapping>>,
    mappings: Vec<Mapping>,
    can_manage: bool,
    registration_form: Draft,
    mapping_form: Draft,
    flash: Option<(StatusKind, String)>,
    history: ListColumns,
    codes: ListColumns,
    mapping_history: ListColumns,
}

impl Setup {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            registrations: None,
            tax_codes: None,
            mappings: vec![],
            can_manage: false,
            registration_form: Draft::default(),
            mapping_form: Draft::default(),
            flash: None,
            history: columns(
                "sst-registration-history",
                &[
                    ("version", "Version"),
                    ("numbers", "Numbers"),
                    ("cycle", "Cycle"),
                    ("service_tax", "Service tax"),
                    ("by", "By"),
                    ("when", "When"),
                    ("reason", "Reason"),
                ],
                &[],
            )?,
            codes: columns(
                "sst-tax-codes-table",
                &[
                    ("source", "Source"),
                    ("code", "Code"),
                    ("name", "Name"),
                    ("rate", "Rate"),
                    ("reports_in", "Reports in"),
                ],
                &["rate"],
            )?,
            mapping_history: columns(
                "sst-mapping-history-table",
                &[
                    ("number", "No."),
                    ("tax_code", "Tax code"),
                    ("item", "Item"),
                    ("by", "By"),
                    ("when", "When"),
                    ("reason", "Reason"),
                ],
                &[],
            )?,
        })
    }

    fn registration(&self) -> Option<&Registration> {
        self.registrations.as_ref().and_then(|r| r.last())
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        let sst = &self.pages.sst;
        let company = company(scope);
        self.registrations = match company {
            Some(c) => sst.list_registrations(scope, c).await.ok(),
            None => None,
        };
        self.tax_codes = match company {
            Some(c) => sst.list_tax_codes(scope, c).await.ok(),
            None => None,
        };
        self.mappings = match company {
            Some(c) => sst
                .list_mappings(scope, c, Some(500), None)
                .await
                .unwrap_or_default(),
            None => vec![],
        };
        self.mappings.reverse();
        self.can_manage = self.pages.allowed(scope, MANAGE_SETTINGS).await?;
        if let Some(registrations) = &self.registrations {
            self.history.load(registrations, |r| r.version.to_string());
        }
        if let Some(codes) = &self.tax_codes {
            self.codes
                .load(codes, |c| format!("{}-{}", c.source.as_str(), c.id));
        }
        self.mapping_history
            .load(&self.mappings, |m| m.mapping_number.to_string());
        Ok(())
    }

    /// Bilimbi `reset_registration_form`: the current version's values and a
    /// blank reason.
    pub(super) fn reset_registration_form(&mut self) {
        let current = self.registration();
        let text = |v: Option<&String>| v.cloned().unwrap_or_default();
        self.registration_form = Draft {
            values: HashMap::from([
                (
                    "sales_tax_number".into(),
                    text(current.and_then(|r| r.sales_tax_number.as_ref())),
                ),
                (
                    "service_tax_number".into(),
                    text(current.and_then(|r| r.service_tax_number.as_ref())),
                ),
                (
                    "period_months".into(),
                    current.map_or(2, |r| r.period_months).to_string(),
                ),
                (
                    "period_start".into(),
                    current.map_or_else(String::new, |r| r.period_start.to_string()),
                ),
                (
                    "service_tax_basis".into(),
                    current.map_or_else(|| "payment".into(), |r| r.service_tax_basis.clone()),
                ),
                ("reason".into(), String::new()),
            ]),
            errors: vec![],
        };
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let actions = render(super::returns::Link {
            link_id: "sst-setup-returns",
            href: INDEX.into(),
            class: "",
            text: "SST returns".into(),
        })?;
        let registration = match &self.registrations {
            None => None,
            Some(registrations) => Some(self.registration_card(registrations, &display)?),
        };
        let codes = match &self.tax_codes {
            None => None,
            Some(codes) => Some(self.codes_card(codes)?),
        };
        let mapping_history = match &self.registrations {
            None => None,
            Some(_) => {
                let mut rows = Vec::new();
                for m in &self.mappings {
                    let mut row = FlexRow::new(m.mapping_number.to_string());
                    row.dom_id = Some(format!("mapping-{}", m.mapping_number));
                    rows.push(
                        row.cell("number", Cell::text(m.mapping_number.to_string()))
                            .cell(
                                "tax_code",
                                Cell::text(format!("{} {}", f::source_name(m.source), m.tax_code)),
                            )
                            .cell("item", Cell::text(f::mapped_box_name(m.box_key.as_deref())))
                            .cell(
                                "by",
                                Cell::text(f::actor(
                                    m.recorded_by_user_id,
                                    m.recorded_by_impersonator_id,
                                )),
                            )
                            .slot(
                                "when",
                                time(
                                    &display,
                                    &format!("mapping-{}-at", m.mapping_number),
                                    &m.recorded_at,
                                )?,
                            )
                            .cell("reason", Cell::text(m.reason.clone().unwrap_or_default())),
                    );
                }
                Some(table(
                    &self.mapping_history,
                    "mapping_history_grid",
                    "Mapping history",
                    rows,
                    Some((
                        "No mappings yet",
                        "Map each tax code to the item it reports in.",
                    )),
                    false,
                )?)
            }
        };
        let forbidden = self
            .registrations
            .is_none()
            .then(|| render(EmptyState::forbidden("view SST").id("sst-setup-forbidden")))
            .transpose()?;
        let codes_forbidden = (self.registrations.is_some() && self.tax_codes.is_none())
            .then(|| {
                render(
                    EmptyState::forbidden("view the receivables and payables tax codes")
                        .id("sst-tax-codes-forbidden"),
                )
            })
            .transpose()?;
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            flash: flash(&self.flash),
            actions,
            forbidden,
            registration,
            codes_forbidden,
            codes,
            mapping_history,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn registration_card(
        &self,
        registrations: &[Registration],
        display: &bilibi_datetime::DisplayContext,
    ) -> Result<RegistrationView> {
        let none = self
            .registration()
            .is_none()
            .then(|| {
                render(
                    EmptyState::missing(
                        "Not registered yet",
                        "Record the registration numbers and the taxable period cycle to prepare returns.",
                    )
                    .id("sst-registration-none"),
                )
            })
            .transpose()?;
        let facts = self
            .registration()
            .map(|r| {
                render(Facts {
                    id: "sst-registration-facts",
                    items: vec![
                        fact(
                            "Sales tax number",
                            "sst-sales-tax-number",
                            dash(&r.sales_tax_number),
                        ),
                        fact(
                            "Service tax number",
                            "sst-service-tax-number",
                            dash(&r.service_tax_number),
                        ),
                        fact(
                            "Taxable period",
                            "sst-cycle",
                            &format!(
                                "{} months, the first from {}",
                                r.period_months, r.period_start
                            ),
                        ),
                        fact(
                            "Service tax due",
                            "sst-basis",
                            f::basis_name(Some(&r.service_tax_basis)),
                        ),
                    ],
                })
            })
            .transpose()?;
        let history = if registrations.is_empty() {
            None
        } else {
            let mut rows = Vec::new();
            for r in registrations.iter().rev() {
                let mut row = FlexRow::new(r.version.to_string());
                row.dom_id = Some(format!("registration-{}", r.version));
                rows.push(
                    row.cell("version", Cell::text(r.version.to_string()))
                        .cell(
                            "numbers",
                            Cell::text(format!(
                                "{} / {}",
                                dash(&r.sales_tax_number),
                                dash(&r.service_tax_number)
                            )),
                        )
                        .cell(
                            "cycle",
                            Cell::text(format!(
                                "{} months from {}",
                                r.period_months, r.period_start
                            )),
                        )
                        .cell("service_tax", Cell::text(&r.service_tax_basis))
                        .cell(
                            "by",
                            Cell::text(f::actor(
                                r.recorded_by_user_id,
                                r.recorded_by_impersonator_id,
                            )),
                        )
                        .slot(
                            "when",
                            time(
                                display,
                                &format!("registration-{}-at", r.version),
                                &r.recorded_at,
                            )?,
                        )
                        .cell("reason", Cell::text(r.reason.clone().unwrap_or_default())),
                );
            }
            Some(table(
                &self.history,
                "history_grid",
                "Registration versions",
                rows,
                None,
                false,
            )?)
        };
        let manage_forbidden = (!self.can_manage)
            .then(|| {
                let mut empty = EmptyState::forbidden("change the SST registration")
                    .id("sst-registration-manage-forbidden");
                empty.class = "mt-4".into();
                render(empty)
            })
            .transpose()?;
        let d = &self.registration_form;
        let fields = self.can_manage.then(|| {
            vec![
                d.field(
                    "registration",
                    "sales_tax_number",
                    "registration-sales-tax-number",
                    "Sales tax registration number",
                )
                .placeholder("A00-0000-00000000")
                .attr("maxlength", "30"),
                d.field(
                    "registration",
                    "service_tax_number",
                    "registration-service-tax-number",
                    "Service tax registration number",
                )
                .placeholder("A00-0000-00000000")
                .attr("maxlength", "30"),
                d.field(
                    "registration",
                    "period_months",
                    "registration-period-months",
                    "Months in a taxable period",
                )
                .kind(InputKind::Number)
                .attr("min", "1")
                .attr("max", "12")
                .hint("Two, unless the Director General approved another."),
                d.field(
                    "registration",
                    "period_start",
                    "registration-period-start",
                    "First taxable period starts",
                )
                .kind(InputKind::Date)
                .hint("The first period ends on the last day of its final month."),
                d.field(
                    "registration",
                    "service_tax_basis",
                    "registration-service-tax-basis",
                    "Service tax is due",
                )
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("When payment is received (section 11(1))", "payment"),
                    Choice::new(
                        "When invoiced, with the Director General's approval (section 11(1A))",
                        "invoice",
                    ),
                ]),
                d.field(
                    "registration",
                    "reason",
                    "registration-reason",
                    "Reason for the change",
                )
                .attr("maxlength", "500"),
            ]
        });
        Ok(RegistrationView {
            none,
            facts,
            history,
            manage_forbidden,
            fields,
        })
    }

    fn codes_card(&self, codes: &[TaxCodeMapping]) -> Result<CodesView> {
        let mut rows = Vec::new();
        for code in codes {
            let mut row = FlexRow::new(format!("{}-{}", code.source.as_str(), code.id));
            row.dom_id = Some(format!("tax-code-{}-{}", code.source.as_str(), code.id));
            let reports_in = match &code.mapping {
                Some(m) => Html::text(&f::mapped_box_name(m.box_key.as_deref())),
                None => f::badge("Not chosen", bilibi_ui::BadgeKind::Warning, "")?,
            };
            rows.push(
                row.cell("source", Cell::text(f::source_name(code.source)))
                    .cell("code", Cell::text(&code.code))
                    .cell("name", Cell::text(&code.name))
                    .cell("rate", Cell::text(format!("{}%", f::rate(code.rate))))
                    .slot("reports_in", reports_in),
            );
        }
        let table = table(
            &self.codes,
            "codes_grid",
            "Tax codes and SST-02 items",
            rows,
            Some((
                "No tax codes yet",
                "Tax codes are kept in receivables and payables.",
            )),
            false,
        )?;
        let d = &self.mapping_form;
        let fields = (self.can_manage && !codes.is_empty()).then(|| {
            let mut boxes = vec![Choice::new("Not reported", "")];
            for source in TaxSource::ALL {
                for b in Sst::mappable_boxes(source) {
                    if !boxes.iter().any(|c| c.value == b.form_box.key) {
                        boxes.push(Choice::new(
                            f::mapped_box_name(Some(b.form_box.key)),
                            b.form_box.key,
                        ));
                    }
                }
            }
            vec![
                d.field("mapping", "tax_code", "mapping-tax-code", "Tax code")
                    .kind(InputKind::Select)
                    .prompt("Choose a tax code")
                    .options(
                        codes
                            .iter()
                            .map(|c| {
                                Choice::new(
                                    format!(
                                        "{} {} {} ({}%)",
                                        f::source_name(c.source),
                                        c.code,
                                        c.name,
                                        f::rate(c.rate)
                                    ),
                                    format!("{}:{}", c.source.as_str(), c.id),
                                )
                            })
                            .collect(),
                    )
                    .required(),
                d.field("mapping", "box", "mapping-box", "SST-02 item")
                    .kind(InputKind::Select)
                    .options(boxes),
                d.field("mapping", "reason", "mapping-reason", "Reason")
                    .attr("maxlength", "500"),
            ]
        });
        Ok(CodesView {
            count: Some(codes.len()),
            table,
            fields,
        })
    }

    async fn register(&mut self, scope: &Scope) -> Result<bool> {
        let d = &self.registration_form;
        let text = |k: &str| Some(d.value(k).to_owned());
        let mut errors = Vec::new();
        let period_months = match d.value("period_months").trim() {
            "" => None,
            v => v.parse().map(Some).unwrap_or_else(|_| {
                errors.push(FieldError::new("period_months", "is invalid"));
                None
            }),
        };
        let period_start = match d.value("period_start").trim() {
            "" => None,
            v => v.parse().map(Some).unwrap_or_else(|_| {
                errors.push(FieldError::new("period_start", "is invalid"));
                None
            }),
        };
        let input = RegistrationInput {
            sales_tax_number: text("sales_tax_number"),
            service_tax_number: text("service_tax_number"),
            period_months,
            period_start,
            service_tax_basis: text("service_tax_basis"),
            reason: text("reason"),
        };
        let result = if errors.is_empty() {
            match company(scope) {
                Some(c) => self.pages.sst.register(scope, c, &input).await,
                None => Err(SstError::Unauthorized),
            }
        } else {
            Err(SstError::Invalid(errors))
        };
        match result {
            Ok(r) => {
                self.flash = Some((
                    StatusKind::Success,
                    format!("Registration version {} recorded.", r.version),
                ));
                self.reload(scope).await?;
                self.reset_registration_form();
                Ok(false)
            }
            Err(e) => {
                self.flash = Some((StatusKind::Error, f::refusal(&e)));
                if let SstError::Invalid(errors) = e {
                    self.registration_form.errors = errors;
                }
                Ok(false)
            }
        }
    }

    async fn map(&mut self, scope: &Scope) -> Result<bool> {
        let d = &self.mapping_form;
        let (source, tax_code_id) = d
            .value("tax_code")
            .split_once(':')
            .map_or((String::new(), 0), |(s, id)| {
                (s.to_owned(), id.parse().unwrap_or(0))
            });
        let input = MappingInput {
            source,
            tax_code_id,
            box_key: params::blank_to_none(Some(d.value("box"))),
            reason: Some(d.value("reason").to_owned()),
        };
        let result = match company(scope) {
            Some(c) => self.pages.sst.map_tax_code(scope, c, &input).await,
            None => Err(SstError::Unauthorized),
        };
        match result {
            Ok(m) => {
                self.flash = Some((
                    StatusKind::Success,
                    format!(
                        "{} reports in {}.",
                        m.tax_code,
                        f::mapped_box_name(m.box_key.as_deref())
                    ),
                ));
                self.mapping_form = Draft::default();
                Ok(true)
            }
            Err(e) => {
                self.flash = Some((StatusKind::Error, f::refusal(&e)));
                if let SstError::Invalid(errors) = e {
                    self.mapping_form.errors = errors;
                }
                Ok(false)
            }
        }
    }

    async fn event(&mut self, scope: &Scope, v: &serde_json::Value) -> Result<bool> {
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => {
                clear_flash(&mut self.flash, v);
                Ok(false)
            }
            "history_grid" => {
                self.history.handle(&params::string_values(v));
                Ok(false)
            }
            "codes_grid" => {
                self.codes.handle(&params::string_values(v));
                Ok(false)
            }
            "mapping_history_grid" => {
                self.mapping_history.handle(&params::string_values(v));
                Ok(false)
            }
            "registration_change" => {
                self.registration_form.values = form_fields(v, "registration");
                Ok(false)
            }
            "mapping_change" => {
                self.mapping_form.values = form_fields(v, "mapping");
                Ok(false)
            }
            action @ ("register" | "map") => {
                if !self.pages.allowed(scope, MANAGE_SETTINGS).await? {
                    self.flash = Some((StatusKind::Error, DENIED.into()));
                    return Ok(false);
                }
                if action == "register" {
                    self.registration_form.values = form_fields(v, "registration");
                    self.registration_form.errors.clear();
                    self.register(scope).await
                } else {
                    self.mapping_form.values = form_fields(v, "mapping");
                    self.mapping_form.errors.clear();
                    self.map(scope).await
                }
            }
            _ => Err(Error::Invalid),
        }
    }
}

impl LiveState<Scope, Error> for Setup {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => {
                    let first = self.registration_form.values.is_empty();
                    self.reload(r.scope).await?;
                    if first {
                        self.reset_registration_form();
                    }
                    false
                }
                LiveInput::Event(v) => self.event(r.scope, v).await?,
            };
            if reload {
                self.reload(r.scope).await?;
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}

fn dash(text: &Option<String>) -> &str {
    text.as_deref().unwrap_or("—")
}

fn fact<'a>(title: &'a str, id: &'a str, value: &str) -> Fact<'a> {
    Fact {
        title,
        id,
        value: Html::text(value),
    }
}

#[derive(Template)]
#[template(path = "setup.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    actions: Html,
    forbidden: Option<Html>,
    registration: Option<RegistrationView>,
    codes_forbidden: Option<Html>,
    codes: Option<CodesView>,
    mapping_history: Option<Html>,
}

struct RegistrationView {
    none: Option<Html>,
    facts: Option<Html>,
    history: Option<Html>,
    manage_forbidden: Option<Html>,
    fields: Option<Vec<Field>>,
}

struct CodesView {
    count: Option<usize>,
    table: Html,
    fields: Option<Vec<Field>>,
}

/// A posted form's fields in full: `params::form_values` drops a value over
/// 1 KiB, and a 500 character reason can take 2 KiB; the facade bounds it.
fn form_fields(v: &serde_json::Value, form: &str) -> HashMap<String, String> {
    let Some(object) = v.as_object() else {
        return HashMap::new();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let key = key
                .strip_prefix(form)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            Some((key.to_owned(), value.as_str()?.to_owned()))
        })
        .take(64)
        .collect()
}
