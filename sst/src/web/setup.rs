//! Bilimbi Sst.Web.SetupLive: the company's SST setup. Its registration
//! (numbers, taxable period cycle, service tax basis) with every version
//! recorded, and which return box each receivables and payables tax code
//! reports in, with every mapping recorded. The registration is recorded in
//! a dialog (each change is a new version); a tax code's item is chosen in
//! place on its row. Both histories fold away under the card they record.
//! Every write is a facade call that re-asks its capability.
use super::{
    clear_flash, columns, company, dialog_actions, flash, format as f, nav_button, render, table,
    time, Pages, DENIED, INDEX,
};
use crate::{
    Mapping, MappingInput, Registration, RegistrationInput, Sst, SstError, TaxCodeMapping, TaxType,
    MANAGE_SETTINGS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::FieldError;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Button, ButtonVariant, Cell, Choice, Combobox, CommitStates, CommitStatus,
    EmptyState, Fact, Facts, Field, FlashMap, FlexRow, Fold, Html, InlineChoice, InlineField,
    InputKind, ListColumns, StatusKind,
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
    /// The registration dialog is open, with a refusal that has no field.
    registering: bool,
    registration_alert: Option<String>,
    /// `?do=register` asked for the dialog; it opens after the first read.
    requested: bool,
    /// Each tax code row's in-place outcome, keyed `source-id`.
    statuses: CommitStates,
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
            registering: false,
            registration_alert: None,
            requested: false,
            statuses: CommitStates::default(),
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

    /// `?do=register` (the Returns page's "Record registration") asks for
    /// the dialog, which [`Self::open_requested`] opens after a read.
    pub(super) fn requested(mut self, query: &HashMap<String, String>) -> Self {
        self.requested = query.get("do").map(String::as_str) == Some("register");
        self
    }

    /// Opens the asked-for dialog for someone who may record it.
    pub(super) fn open_requested(&mut self) {
        if std::mem::take(&mut self.requested) && self.can_manage && self.registrations.is_some() {
            self.registering = true;
        }
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let people = self
            .pages
            .people(
                scope,
                self.registrations
                    .iter()
                    .flatten()
                    .flat_map(|r| [Some(r.recorded_by_user_id), r.recorded_by_impersonator_id])
                    .chain(self.mappings.iter().flat_map(|m| {
                        [Some(m.recorded_by_user_id), m.recorded_by_impersonator_id]
                    })),
            )
            .await?;
        let actions = nav_button("sst-setup-returns", INDEX, "SST returns")?;
        let registration = match &self.registrations {
            None => None,
            Some(registrations) => {
                Some(self.registration_card(registrations, &display, &people)?)
            }
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
                                Cell::text(people.who(
                                    Some(m.recorded_by_user_id),
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
                let summary = changes(self.mappings.len());
                let table = table(
                    &self.mapping_history,
                    "mapping_history_grid",
                    "Mapping history",
                    rows,
                    Some((
                        "No items chosen yet",
                        "Each choice of a tax code's item is listed here.",
                    )),
                    false,
                )?;
                Some(render(Fold {
                    id: "sst-mapping-history-fold",
                    title: "History",
                    summary: &summary,
                    open: false,
                    body: table,
                })?)
            }
        };
        let modal = (self.registering && self.can_manage && self.registrations.is_some())
            .then(|| self.registration_dialog())
            .transpose()?;
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
            modal,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn registration_card(
        &self,
        registrations: &[Registration],
        display: &bilibi_datetime::DisplayContext,
        people: &bilibi_finance_ledger::People,
    ) -> Result<RegistrationView> {
        let none = self
            .registration()
            .is_none()
            .then(|| {
                let mut empty = EmptyState::missing(
                    "Not registered yet",
                    if self.can_manage {
                        "Record the registration numbers and the taxable period cycle to prepare returns."
                    } else {
                        "Someone who manages SST setup records the registration numbers and the taxable period cycle."
                    },
                )
                .id("sst-registration-none");
                if self.can_manage {
                    empty = empty.action(render(Button {
                        id: "sst-registration-record",
                        event: "open_registration",
                        button_type: "button",
                        variant: ButtonVariant::Primary,
                        body: Html::text("Record registration"),
                        ..Button::default()
                    })?);
                }
                render(empty)
            })
            .transpose()?;
        let change = (self.can_manage && self.registration().is_some())
            .then(|| {
                render(Button {
                    id: "sst-registration-change",
                    event: "open_registration",
                    button_type: "button",
                    body: Html::text("Change registration"),
                    ..Button::default()
                })
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
        let history =
            if registrations.is_empty() {
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
                            .cell(
                                "service_tax",
                                Cell::text(f::basis_name(Some(&r.service_tax_basis))),
                            )
                            .cell(
                                "by",
                                Cell::text(people.who(
                                    Some(r.recorded_by_user_id),
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
                let summary = match registrations.len() {
                    1 => "1 version".to_owned(),
                    n => format!("{n} versions"),
                };
                Some(render(Fold {
                    id: "sst-registration-history-fold",
                    title: "History",
                    summary: &summary,
                    open: false,
                    body: table(
                        &self.history,
                        "history_grid",
                        "Registration versions",
                        rows,
                        None,
                        false,
                    )?,
                })?)
            };
        Ok(RegistrationView {
            none,
            facts,
            change,
            history,
        })
    }

    /// The registration dialog: the current version's values, saved as the
    /// next version; a change after the first needs its reason.
    fn registration_dialog(&self) -> Result<ModalView> {
        let d = &self.registration_form;
        let changing = self.registration().is_some();
        let mut fields = vec![
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
        ];
        if changing {
            fields.push(
                d.field(
                    "registration",
                    "reason",
                    "registration-reason",
                    "Reason for the change",
                )
                .attr("maxlength", "500"),
            );
        }
        let alert = self
            .registration_alert
            .as_ref()
            .map(|message| {
                render(Alert {
                    kind: StatusKind::Error,
                    id: "sst-registration-alert",
                    class: "mt-4",
                    body: Html::text(message),
                })
            })
            .transpose()?;
        Ok(ModalView {
            title: if changing {
                "Change registration"
            } else {
                "Record registration"
            },
            description: if changing {
                "Saved as a new version. Returns already prepared keep the version they were prepared with."
            } else {
                "The numbers and the taxable period cycle the company's returns are prepared with."
            },
            alert,
            fields,
            actions: dialog_actions(
                "sst-registration",
                "close_registration",
                if changing {
                    "Save registration"
                } else {
                    "Record registration"
                },
                "Saving…",
            )?,
        })
    }

    fn codes_card(&self, codes: &[TaxCodeMapping]) -> Result<CodesView> {
        let mut rows = Vec::new();
        for code in codes {
            let mut row = FlexRow::new(format!("{}-{}", code.source.as_str(), code.id));
            row.dom_id = Some(format!("tax-code-{}-{}", code.source.as_str(), code.id));
            let reports_in = self.reports_in(code)?;
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
                "Tax codes are added in Receivables settings and Payables setup.",
            )),
            false,
        )?;
        Ok(CodesView {
            count: Some(codes.len()),
            table,
        })
    }

    /// A tax code's item, chosen in place on its row from the items its
    /// source can report in ("Not reported" too), by someone who manages
    /// the setup; read alone by anyone else.
    fn reports_in(&self, code: &TaxCodeMapping) -> Result<Html> {
        let key = format!("{}-{}", code.source.as_str(), code.id);
        let current = code.mapping.as_ref().map(|m| m.box_key.clone());
        let display = match &current {
            None => f::badge(
                "Not chosen",
                bilibi_ui::BadgeKind::Warning,
                "whitespace-nowrap",
            )?,
            Some(None) => Html::text("Not reported"),
            Some(Some(b)) => box_html(b)?,
        };
        let value = match &current {
            None => String::new(),
            Some(None) => NOT_REPORTED.to_owned(),
            Some(Some(b)) => b.clone(),
        };
        let label = format!("Reports in, for {}", code.code);
        let mut field = InlineField::new(format!("sst-map-{key}"), "box", &label, value.clone())
            .version(0)
            .status(self.statuses.get(&key));
        field.id_value = key.clone();
        field.save_event = "save_mapping".into();
        if !self.can_manage {
            field = field.readonly();
        }
        let mut options = vec![Choice::new("Not reported", NOT_REPORTED)];
        options.extend(Sst::mappable_boxes(code.source).into_iter().map(|b| {
            Choice::new(box_title(b.form_box.key), b.form_box.key)
                .detail(format!("item {}", b.form_box.item))
        }));
        let editor = Combobox::new(format!("sst-map-{key}-input"), "box")
            .aria_label(&label)
            .placeholder("Search SST-02 items")
            .wrapper_class("mb-0")
            .value(value)
            .options(options);
        render(InlineChoice {
            field,
            display,
            editor: render(editor)?,
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
                self.registering = false;
                self.registration_alert = None;
                self.reload(scope).await?;
                self.reset_registration_form();
                Ok(false)
            }
            // A field's refusal shows beside it; anything else in the
            // dialog's own alert, which stays open.
            Err(SstError::Invalid(errors)) => {
                let known = [
                    "sales_tax_number",
                    "service_tax_number",
                    "period_months",
                    "period_start",
                    "service_tax_basis",
                    "reason",
                ];
                let other: Vec<String> = errors
                    .iter()
                    .filter(|e| !known.contains(&e.field))
                    .map(|e| format!("{} {}", e.field.replace('_', " "), e.message))
                    .collect();
                self.registration_alert = (!other.is_empty()).then(|| other.join(" "));
                self.registration_form.errors = errors;
                Ok(false)
            }
            Err(e) => {
                self.registration_alert = Some(f::refusal(&e));
                Ok(false)
            }
        }
    }

    /// One tax code's item saved from its row (`id` is `source-id`); the
    /// outcome shows on the row.
    async fn map(&mut self, scope: &Scope, v: &serde_json::Value) -> Result<bool> {
        let key = v["id"].as_str().ok_or(Error::Invalid)?.to_owned();
        let (source, tax_code_id) = key
            .split_once('-')
            .and_then(|(s, id)| Some((s.to_owned(), id.parse::<i64>().ok()?)))
            .ok_or(Error::Invalid)?;
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if submitted.is_empty() {
            self.statuses.put(
                key,
                CommitStatus::refusal(
                    "Reports in",
                    &submitted,
                    &["choose an item or Not reported"],
                ),
            );
            return Ok(false);
        }
        if !self.can_manage {
            self.statuses.put(key, CommitStatus::Error(DENIED.into()));
            return Ok(false);
        }
        let input = MappingInput {
            source,
            tax_code_id,
            box_key: (submitted != NOT_REPORTED).then_some(submitted.clone()),
            reason: None,
        };
        let result = match company(scope) {
            Some(c) => self.pages.sst.map_tax_code(scope, c, &input).await,
            None => Err(SstError::Unauthorized),
        };
        match result {
            Ok(_) => {
                self.statuses.put(key, CommitStatus::Saved);
                Ok(true)
            }
            Err(SstError::Invalid(errors)) => {
                let reasons: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                self.statuses.put(
                    key,
                    CommitStatus::refusal("Reports in", &submitted, &reasons),
                );
                Ok(false)
            }
            Err(e) => {
                self.statuses.put(key, CommitStatus::Error(f::refusal(&e)));
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
            "open_registration" => {
                if self.can_manage {
                    self.reset_registration_form();
                    self.registering = true;
                    self.registration_alert = None;
                    self.flash = None;
                }
                Ok(false)
            }
            "close_registration" => {
                self.registering = false;
                self.registration_alert = None;
                self.reset_registration_form();
                Ok(false)
            }
            "register" => {
                if !self.pages.allowed(scope, MANAGE_SETTINGS).await? {
                    self.registration_alert = Some(DENIED.into());
                    return Ok(false);
                }
                self.registration_form.values = form_fields(v, "registration");
                self.registration_form.errors.clear();
                self.registration_alert = None;
                self.register(scope).await
            }
            "save_mapping" => {
                self.can_manage = self.pages.allowed(scope, MANAGE_SETTINGS).await?;
                self.map(scope, v).await
            }
            _ => Err(Error::Invalid),
        }
    }
}

impl LiveState<Scope, Error> for Setup {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Refresh => {
                    let first = self.registration_form.values.is_empty();
                    self.reload(r.scope).await?;
                    if first {
                        self.reset_registration_form();
                    }
                    self.open_requested();
                    false
                }
                LiveInput::Params(query) => {
                    let first = self.registration_form.values.is_empty();
                    self.reload(r.scope).await?;
                    if first {
                        self.reset_registration_form();
                    }
                    self.requested = query.get("do").map(String::as_str) == Some("register");
                    self.open_requested();
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

/// The value an item editor sends for "Not reported" (a combobox's empty
/// value means nothing chosen).
const NOT_REPORTED: &str = "none";

/// "3 changes", the folded mapping history's summary.
fn changes(n: usize) -> String {
    match n {
        0 => "No changes yet".to_owned(),
        1 => "1 change".to_owned(),
        n => format!("{n} changes"),
    }
}

/// An item's name without its number ("Sales tax: Taxable sales at 10%").
fn box_title(key: &str) -> String {
    TaxType::ALL
        .into_iter()
        .find_map(|tax_type| {
            Sst::boxes(tax_type)
                .iter()
                .find(|b| b.key == key)
                .map(|b| format!("{}: {}", f::tax_name(tax_type), b.label))
        })
        .unwrap_or_else(|| key.to_owned())
}

/// An item as its row shows it: the name, then its number muted, as the
/// combobox draws the choice.
fn box_html(key: &str) -> Result<Html> {
    let item = TaxType::ALL.into_iter().find_map(|tax_type| {
        Sst::boxes(tax_type)
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.item)
    });
    render(BoxName {
        name: box_title(key),
        item: item.map(|i| format!("item {i}")).unwrap_or_default(),
    })
}

#[derive(Template)]
#[template(
    source = r#"<span>{{ name }}</span>{% if !item.is_empty() %} <span class="italic text-ink-muted">{{ item }}</span>{% endif %}"#,
    ext = "html"
)]
struct BoxName {
    name: String,
    item: String,
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
    modal: Option<ModalView>,
}

struct RegistrationView {
    none: Option<Html>,
    facts: Option<Html>,
    change: Option<Html>,
    history: Option<Html>,
}

struct CodesView {
    count: Option<usize>,
    table: Html,
}

struct ModalView {
    title: &'static str,
    description: &'static str,
    alert: Option<Html>,
    fields: Vec<Field>,
    actions: Html,
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
