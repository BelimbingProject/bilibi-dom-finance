//! Bilimbi Sst.Web.ReturnsLive: the recent taxable periods of the company's
//! registration with each registered tax's return for the period (open,
//! finalised with its version, or none yet, with a control that prepares
//! it), "+ Return" opening a dialog that prepares the return of any period,
//! and every return on record. Preparing is a facade call that re-asks its
//! capability.
use super::{
    clear_flash, columns, company, dialog_actions, flash, format as f, nav_button, render, table,
    table_with, Pages, DENIED, INDEX, SETUP,
};
use crate::{
    Registration, SstError, TaxReturn, TaxType, TaxablePeriod, MANAGE_SETTINGS, PREPARE_RETURNS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{MinorUnits, Money};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Button, ButtonVariant, Cell, Choice, CreateButton, EmptyState, Field, FlashMap,
    FlexRow, Html, InputKind, ListColumns, RecordLink, StatusKind,
};
use jiff::civil::Date;
use std::collections::HashMap;

pub(super) struct Returns {
    pages: Pages,
    registration: Option<Registration>,
    periods: Vec<TaxablePeriod>,
    returns: Option<Vec<TaxReturn>>,
    refusal: Option<SstError>,
    can_prepare: bool,
    can_manage: bool,
    form: HashMap<String, String>,
    /// The "+ Return" dialog is open, with the facade's refusal inside it.
    adding: bool,
    dialog_error: Option<String>,
    flash: Option<(StatusKind, String)>,
    framed: bool,
    /// Today in the company's zone, which due dates are counted from.
    today: Date,
    /// The day the first registration was recorded here. A period due before
    /// it was filed before SST was kept here, so its date is not counted.
    kept_from: Option<Date>,
    period_columns: ListColumns,
    list_columns: ListColumns,
}

impl Returns {
    pub(super) fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            registration: None,
            periods: vec![],
            returns: None,
            refusal: None,
            can_prepare: false,
            can_manage: false,
            adding: false,
            dialog_error: None,
            form: HashMap::from([
                ("tax_type".into(), "sales_tax".into()),
                ("period_start".into(), String::new()),
            ]),
            flash: None,
            framed: false,
            today: crate::today(),
            kept_from: None,
            period_columns: columns(
                "sst-periods-table",
                &[("period", "Period"), ("due", "Due")],
                &[],
            )?,
            list_columns: columns(
                "sst-returns-table",
                &[
                    ("number", "No."),
                    ("tax", "Tax"),
                    ("period", "Period"),
                    ("status", "Status"),
                    ("due", "Due"),
                    ("tax_payable", "Tax payable"),
                ],
                &["tax_payable"],
            )?,
        })
    }

    pub(super) fn framed(mut self, framed: bool) -> Self {
        self.framed = framed;
        self
    }

    fn tax_types(&self) -> Vec<TaxType> {
        self.registration.as_ref().map_or_else(Vec::new, |r| {
            TaxType::ALL
                .into_iter()
                .filter(|t| r.number(*t).is_some())
                .collect()
        })
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        let timezone = self.pages.display(scope).await?.timezone;
        self.today = f::today(&timezone);
        let sst = &self.pages.sst;
        let loaded = async {
            let company = company(scope).ok_or(SstError::Unauthorized)?;
            let registrations = sst.list_registrations(scope, company).await?;
            let periods = sst.taxable_periods(scope, company, None, Some(6)).await?;
            let returns = sst
                .list_returns(scope, company, None, Some(500), None)
                .await?;
            Ok::<_, SstError>((registrations, periods, returns))
        }
        .await;
        match loaded {
            Ok((mut registrations, periods, mut returns)) => {
                returns.reverse();
                self.kept_from = registrations
                    .first()
                    .and_then(|first| f::day(&first.recorded_at, &timezone));
                self.registration = registrations.pop();
                self.periods = periods;
                self.returns = Some(returns);
                self.refusal = None;
                self.can_prepare = self.pages.allowed(scope, PREPARE_RETURNS).await?;
                self.can_manage = self.pages.allowed(scope, MANAGE_SETTINGS).await?;
            }
            Err(e) => {
                self.registration = None;
                self.kept_from = None;
                self.periods.clear();
                self.returns = None;
                self.refusal = Some(e);
                self.can_prepare = false;
                self.can_manage = false;
            }
        }
        // One column per registered tax, as Bilimbi's `:for` column.
        let mut cols = vec![("period", "Period"), ("due", "Due")];
        let taxes = self.tax_types();
        cols.extend(taxes.iter().map(|t| (t.as_str(), f::tax_name(*t))));
        if !self
            .period_columns
            .builtins()
            .iter()
            .map(|c| c.id.as_str())
            .eq(cols.iter().map(|(id, _)| *id))
        {
            self.period_columns = columns("sst-periods-table", &cols, &[])?;
        }
        self.period_columns
            .load(&self.periods, |p| p.period_start.to_string());
        if let Some(returns) = &self.returns {
            self.list_columns.load(returns, |r| r.id.to_string());
        }
        Ok(())
    }

    /// The return that stands for each tax and period: an open one, else the
    /// latest finalised version; discarded returns stay in the list below.
    fn standing(&self, tax_type: TaxType, period_start: Date) -> Option<&TaxReturn> {
        let group: Vec<&TaxReturn> = self
            .returns
            .iter()
            .flatten()
            .filter(|r| {
                r.tax_type == tax_type
                    && r.period_start == period_start
                    && r.status != crate::ReturnStatus::Discarded
            })
            .collect();
        group
            .iter()
            .find(|r| r.status == crate::ReturnStatus::Open)
            .or_else(|| group.iter().max_by_key(|r| r.version))
            .copied()
    }

    /// The "+ Return" button, opening the prepare dialog.
    fn new_button(id: &str) -> Result<Html> {
        render(CreateButton {
            noun: "Return",
            id,
            event: "new_return",
            button_type: "button",
            ..CreateButton::default()
        })
    }

    pub(super) fn body(&self) -> Result<String> {
        let prepare = self.registration.is_some() && self.can_prepare;
        let mut actions = nav_button("sst-returns-setup", SETUP, "SST setup")?;
        if prepare {
            actions.push(&Self::new_button("sst-return-new")?);
        }
        let refusal = self
            .refusal
            .as_ref()
            .map(|e| {
                render(
                    EmptyState::missing("SST returns unavailable", f::refusal(e))
                        .id("sst-returns-refusal"),
                )
            })
            .transpose()?;
        let unregistered = (self.refusal.is_none() && self.registration.is_none())
            .then(|| {
                // The registration is recorded in SST setup's own dialog.
                let mut empty = if self.can_manage {
                    EmptyState::missing(
                        "Not registered yet",
                        "Returns are prepared once the company's SST registration is recorded.",
                    )
                    .action(render(Button {
                        id: "sst-returns-register",
                        href: &format!("{SETUP}?do=register"),
                        variant: ButtonVariant::Primary,
                        body: Html::text("Record registration"),
                        ..Button::default()
                    })?)
                } else {
                    EmptyState::missing(
                        "Not registered yet",
                        "Returns are prepared once someone who manages SST setup records the company's registration.",
                    )
                };
                empty = empty.id("sst-returns-unregistered");
                render(empty)
            })
            .transpose()?;
        let periods = self
            .registration
            .is_some()
            .then(|| self.periods_card())
            .transpose()?;
        // Before the registration there is nothing to list, unless returns
        // were prepared under an earlier one.
        let list = match &self.returns {
            Some(returns) if self.registration.is_some() || !returns.is_empty() => {
                Some(self.returns_table(returns)?)
            }
            _ => None,
        };
        let modal = (self.adding && prepare)
            .then(|| self.prepare_dialog())
            .transpose()?;
        View {
            title_actions: bilibi_ui::Html::EMPTY,
            flash: flash(&self.flash),
            actions,
            refusal,
            unregistered,
            periods,
            list,
            count: self.returns.as_ref().map(Vec::len),
            modal,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn periods_card(&self) -> Result<PeriodsView> {
        let taxes = self.tax_types();
        let mut rows = Vec::new();
        for p in &self.periods {
            let mut row = FlexRow::new(p.period_start.to_string());
            row.dom_id = Some(format!("period-{}", p.period_start));
            // A return of a period still running can be prepared and read,
            // and finalised only after it ends: the row says from when.
            let running = p.period_end >= crate::today();
            // The due date counts down until every registered tax's return
            // of the period is finalised: an open one always, and one not
            // prepared yet unless it fell due before SST was kept here.
            let kept = self.kept_from.is_none_or(|from| p.due_date >= from);
            let owed = taxes
                .iter()
                .any(|tax| match self.standing(*tax, p.period_start) {
                    None => kept,
                    Some(r) => r.status == crate::ReturnStatus::Open,
                });
            row = row
                .slot(
                    "period",
                    render(PeriodName {
                        row: p.period_start.to_string(),
                        name: f::period(p.period_start, p.period_end),
                        finalise_from: running.then(|| f::finalise_from(p.period_end)),
                    })?,
                )
                .slot("due", f::due(p.due_date, self.today, owed)?);
            for tax in &taxes {
                let html = match self.standing(*tax, p.period_start) {
                    None if self.can_prepare => render(Button {
                        id: &format!("prepare-{tax}-{}", p.period_start),
                        event: "prepare",
                        name: "period",
                        value: &format!("{tax}|{}", p.period_start),
                        button_type: "button",
                        body: Html::text("Prepare"),
                        ..Button::default()
                    })?,
                    None => render(Muted { text: "none" })?,
                    Some(r) => {
                        let mut html = render(Link {
                            link_id: &format!("period-{tax}-{}-return", p.period_start),
                            href: format!("{INDEX}/returns/{}", r.id),
                            class: "",
                            text: format!("Return {}", r.return_number),
                        })?;
                        html.push(&f::badge(
                            &f::status_label(r),
                            f::status_kind(r.status),
                            "ml-1",
                        )?);
                        html
                    }
                };
                row = row.slot(tax.as_str(), html);
            }
            rows.push(row);
        }
        let table = table(
            &self.period_columns,
            "periods_grid",
            "Taxable periods",
            rows,
            None,
            false,
        )?;
        let forbidden = (!self.can_prepare)
            .then(|| {
                let mut empty =
                    EmptyState::forbidden("prepare SST returns").id("sst-prepare-forbidden");
                empty.class = "p-5".into();
                render(empty)
            })
            .transpose()?;
        Ok(PeriodsView { table, forbidden })
    }

    /// The "+ Return" dialog: the tax and the first day of its period, for a
    /// period the list above does not show.
    fn prepare_dialog(&self) -> Result<ModalView> {
        let taxes = self.tax_types();
        let value = |k: &str| self.form.get(k).cloned().unwrap_or_default();
        let alert = self
            .dialog_error
            .as_ref()
            .map(|message| {
                render(Alert {
                    kind: StatusKind::Error,
                    id: "sst-prepare-alert",
                    class: "mt-4",
                    body: Html::text(message),
                })
            })
            .transpose()?;
        Ok(ModalView {
            alert,
            tax_field: Field::in_form("prepare", "tax_type")
                .id("prepare-tax-type")
                .kind(InputKind::Select)
                .label("Tax")
                .options(
                    taxes
                        .iter()
                        .map(|t| Choice::new(f::tax_name(*t), t.as_str()))
                        .collect(),
                )
                .value(value("tax_type")),
            period_field: Field::in_form("prepare", "period_start")
                .id("prepare-period-start")
                .kind(InputKind::Date)
                .label("Period starts")
                .value(value("period_start"))
                .required(),
            actions: dialog_actions(
                "sst-prepare",
                "close_return",
                "Prepare return",
                "Preparing…",
            )?,
        })
    }

    fn returns_table(&self, returns: &[TaxReturn]) -> Result<Html> {
        let mut rows = Vec::new();
        for r in returns {
            let mut row = FlexRow::new(r.id.to_string());
            row.dom_id = Some(format!("return-{}", r.return_number));
            let mut status = f::badge(&f::status_label(r), f::status_kind(r.status), "")?;
            if r.amends_return_id.is_some() {
                status.push(&render(Note { text: "amendment" })?);
            }
            rows.push(
                row.slot(
                    "number",
                    render(RecordLink {
                        workspace: self.framed,
                        kind: crate::MODULE,
                        record_id: &r.id.to_string(),
                        href: &format!("{INDEX}/returns/{}", r.id),
                        class: "font-medium hover:underline",
                        body: Html::text(&r.return_number.to_string()),
                    })?,
                )
                .cell("tax", Cell::text(f::tax_name(r.tax_type)))
                .cell(
                    "period",
                    Cell::text(f::period(r.period_start, r.period_end)),
                )
                .slot("status", status)
                .slot(
                    "due",
                    f::due(
                        r.due_date,
                        self.today,
                        r.status == crate::ReturnStatus::Open,
                    )?,
                )
                .cell(
                    "tax_payable",
                    Cell::text(r.tax_payable.map_or_else(
                        || "—".into(),
                        |amount| {
                            Money::display_decimal(
                                amount,
                                MinorUnits::new(i64::from(r.minor_units.unwrap_or(0))),
                            )
                        },
                    )),
                ),
            );
        }
        let mut empty = EmptyState::missing(
            "No returns yet",
            if self.can_prepare {
                ""
            } else {
                "Someone who prepares SST returns starts each one from its taxable period."
            },
        )
        .id("sst-returns-empty");
        if self.can_prepare && self.registration.is_some() {
            empty = empty.action(Self::new_button("sst-return-new-empty")?);
        }
        table_with(
            &self.list_columns,
            "returns_grid",
            "Returns on record",
            rows,
            Some(empty),
            false,
        )
    }

    async fn prepare(
        &mut self,
        scope: &Scope,
        tax_type: &str,
        period_start: &str,
        from_dialog: bool,
    ) -> Result<LiveBody> {
        if !self.pages.allowed(scope, PREPARE_RETURNS).await? {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return self.live(false, scope).await;
        }
        let opened = async {
            let company = company(scope).ok_or(SstError::Unauthorized)?;
            let tax_type: TaxType = tax_type.parse()?;
            let period_start: Date = period_start
                .trim()
                .parse()
                .map_err(|_| SstError::NotATaxablePeriod)?;
            self.pages
                .sst
                .open_return(scope, company, tax_type, period_start)
                .await
        }
        .await;
        match opened {
            Ok(r) => Ok(LiveBody {
                redirect: Some(format!("{INDEX}/returns/{}", r.id)),
                flash: Some((
                    StatusKind::Success,
                    format!("Return {} opened.", r.return_number),
                )),
                ..LiveBody::default()
            }),
            Err(e) if from_dialog => {
                // The refusal shows inside the dialog, which stays open.
                self.dialog_error = Some(f::refusal(&e));
                self.live(true, scope).await
            }
            Err(e) => {
                self.flash = Some((StatusKind::Error, f::refusal(&e)));
                self.live(true, scope).await
            }
        }
    }

    async fn live(&mut self, reload: bool, scope: &Scope) -> Result<LiveBody> {
        if reload {
            self.reload(scope).await?;
        }
        Ok(LiveBody {
            html: self.body()?,
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Returns {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let v = match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => return self.live(true, r.scope).await,
                LiveInput::Event(v) => v,
            };
            match v["type"].as_str() {
                Some("clear-flash") => {
                    clear_flash(&mut self.flash, v);
                    self.live(false, r.scope).await
                }
                Some("periods_grid") => {
                    self.period_columns.handle(&params::string_values(v));
                    self.live(false, r.scope).await
                }
                Some("returns_grid") => {
                    self.list_columns.handle(&params::string_values(v));
                    self.live(false, r.scope).await
                }
                Some("prepare_change") => {
                    self.form.extend(params::form_values(v, "prepare"));
                    self.live(false, r.scope).await
                }
                Some("new_return") => {
                    self.adding = true;
                    self.dialog_error = None;
                    self.flash = None;
                    self.live(false, r.scope).await
                }
                Some("close_return") => {
                    self.adding = false;
                    self.dialog_error = None;
                    self.form.insert("period_start".into(), String::new());
                    self.live(false, r.scope).await
                }
                Some("prepare") => {
                    // A period's button sends `tax|start`; the form sends its fields.
                    let (tax_type, period_start, from_dialog) = match v["period"].as_str() {
                        Some(period) => {
                            let (t, s) = period.split_once('|').unwrap_or((period, ""));
                            (t.to_owned(), s.to_owned(), false)
                        }
                        None => {
                            self.form.extend(params::form_values(v, "prepare"));
                            let get = |k: &str| self.form.get(k).cloned().unwrap_or_default();
                            (get("tax_type"), get("period_start"), true)
                        }
                    };
                    self.dialog_error = None;
                    self.prepare(r.scope, &tax_type, &period_start, from_dialog)
                        .await
                }
                _ => Err(Error::Invalid),
            }
        })
    }
}

#[derive(Template)]
#[template(path = "returns.html")]
struct View<'a> {
    title_actions: Html,
    flash: FlashMap<'a>,
    actions: Html,
    refusal: Option<Html>,
    unregistered: Option<Html>,
    periods: Option<PeriodsView>,
    list: Option<Html>,
    count: Option<usize>,
    modal: Option<ModalView>,
}

struct PeriodsView {
    table: Html,
    forbidden: Option<Html>,
}

struct ModalView {
    alert: Option<Html>,
    tax_field: Field,
    period_field: Field,
    actions: Html,
}

#[derive(Template)]
#[template(
    source = r#"<a id="{{ link_id }}" href="{{ href }}"{% if !class.is_empty() %} class="{{ class }}"{% endif %}>{{ text }}</a>"#,
    ext = "html"
)]
pub(super) struct Link<'a> {
    pub link_id: &'a str,
    pub href: String,
    pub class: &'a str,
    pub text: String,
}

#[derive(Template)]
#[template(
    source = r#"<span class="text-ink-muted">{{ text }}</span>"#,
    ext = "html"
)]
struct Muted<'a> {
    text: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<span class="ml-1 text-xs text-ink-muted">{{ text }}</span>"#,
    ext = "html"
)]
struct Note<'a> {
    text: &'a str,
}

/// A taxable period's name, and under it the day its return can be
/// finalised from while the period is still running.
#[derive(Template)]
#[template(
    source = r#"<span>{{ name }}</span>{% if let Some(from) = finalise_from %}<span id="period-{{ row }}-finalise-from" class="block text-xs text-ink-muted">Can be finalised from {{ from }}</span>{% endif %}"#,
    ext = "html"
)]
struct PeriodName {
    row: String,
    name: String,
    finalise_from: Option<String>,
}
