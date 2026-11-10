//! Bilimbi Sst.Web.ReturnLive: one SST return. Its SST-02 items in the
//! form's order, what keeps it from finalising (unmapped tax codes, a rate
//! that does not fit its item, a period not yet ended), its adjustments, the
//! source rows behind each item, and how the documents' tax compares with
//! each tax account's ledger postings, the difference shown as it is. An open
//! return reads its figures now and follows the ledger's postings; a
//! finalised one shows the figures it was finalised with, which never change,
//! and what documents posted since would change; an amendment shows what it
//! changes. Every write is a facade call that re-asks its capability, and the
//! dialog holds the adjustment it withdraws, never a value the browser sends.
use super::{
    clear_flash, columns, company, dialog_actions, flash, format as f, render, table, time, Pages,
    DENIED, INDEX, SETUP,
};
use crate::{
    AdjustmentInput, BoxKind, ReturnReport, ReturnStatus, Sst, SstError, TaxType, AMEND_RETURNS,
    FINALISE_RETURNS, PREPARE_RETURNS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_finance_ledger::{
    AccountName, FieldError, MinorUnits, Money, PostingSubscription, RecordAction, RecordActions,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{self, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    params, Alert, BackLink, Cell, Choice, Combobox, CreateButton, EmptyState, Fact, Facts, Field,
    FlashMap, FlexRow, Html, IconButton, ListColumns, RowMode, StatusKind,
};
use rust_decimal::Decimal;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{sync::broadcast, task::JoinHandle};

/// The reason dialog Bilimbi opens before a discard, an amendment or a
/// withdrawal.
#[derive(Clone, Copy, PartialEq)]
enum Modal {
    Discard,
    Amend,
    Withdraw(i64),
}

const ADJUSTMENT_FIELDS: [&str; 5] = [
    "box",
    "taxable_amount",
    "tax_amount",
    "description",
    "reference",
];

pub(super) struct Return {
    pages: Pages,
    id: i64,
    report: Option<ReturnReport>,
    refusal: Option<SstError>,
    can_prepare: bool,
    can_finalise: bool,
    can_amend: bool,
    modal: Option<Modal>,
    reason: String,
    adjustment: HashMap<String, String>,
    adjustment_errors: Vec<FieldError>,
    /// The "+ Adjustment" dialog is open, with a refusal that has no field.
    adding: bool,
    adjustment_alert: Option<String>,
    flash: Option<(StatusKind, String)>,
    redirect: Option<(String, String)>,
    posted_since: ListColumns,
    items: ListColumns,
    adjustments: ListColumns,
    sources: ListColumns,
    accounts: ListColumns,
    events: ListColumns,
    posting: Option<PostingSubscription>,
    bridge: Option<JoinHandle<()>>,
    dirty: Arc<AtomicBool>,
    tenant: i64,
}

impl Drop for Return {
    fn drop(&mut self) {
        if let Some(task) = self.bridge.take() {
            task.abort();
        }
    }
}

fn items_columns(change: bool) -> Result<ListColumns> {
    let mut cols = vec![
        ("item", "Item"),
        ("description", "Description"),
        ("value", "Value"),
        ("tax", "Tax"),
    ];
    if change {
        cols.push(("change", "Change from the amended version"));
    }
    columns("sst-return-items-table", &cols, &["value", "tax", "change"])
}

impl Return {
    pub(super) fn new(pages: Pages, path: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            report: None,
            refusal: None,
            can_prepare: false,
            can_finalise: false,
            can_amend: false,
            modal: None,
            reason: String::new(),
            adjustment: HashMap::new(),
            adjustment_errors: vec![],
            adding: false,
            adjustment_alert: None,
            flash: None,
            redirect: None,
            posted_since: columns(
                "sst-return-posted-since-table",
                &[
                    ("item", "Item"),
                    ("value_change", "Value change"),
                    ("tax_change", "Tax change"),
                ],
                &["value_change", "tax_change"],
            )?,
            items: items_columns(false)?,
            adjustments: columns(
                "sst-return-adjustments-table",
                &[
                    ("number", "No."),
                    ("item", "Item"),
                    ("description", "Description"),
                    ("value", "Value"),
                    ("tax", "Tax"),
                    ("by", "By"),
                ],
                &["value", "tax"],
            )?,
            sources: columns(
                "sst-return-sources-table",
                &[
                    ("source", "Source"),
                    ("tax_code", "Tax code"),
                    ("documents", "Documents"),
                    ("item", "Item"),
                    ("value", "Value"),
                    ("tax", "Tax"),
                ],
                &["documents", "value", "tax"],
            )?,
            accounts: columns(
                "sst-return-accounts-table",
                &[
                    ("account", "Account"),
                    ("ledger_debits", "Ledger debits"),
                    ("ledger_credits", "Ledger credits"),
                    ("document_debits", "Document debits"),
                    ("document_credits", "Document credits"),
                    ("difference", "Difference"),
                ],
                &[
                    "ledger_debits",
                    "ledger_credits",
                    "document_debits",
                    "document_credits",
                    "difference",
                ],
            )?,
            events: columns(
                "sst-return-events-table",
                &[
                    ("event", "Event"),
                    ("by", "By"),
                    ("when", "When"),
                    ("reason", "Reason"),
                ],
                &[],
            )?,
            posting: None,
            bridge: None,
            dirty: Arc::new(AtomicBool::new(false)),
            tenant: 0,
        })
    }

    /// Bilimbi subscribes to the ledger's postings when the page connects; a
    /// scope without the ledger's view gets no notices and reads on demand.
    pub(super) async fn subscribe(&mut self, scope: &Scope) {
        self.tenant = scope.tenant();
        if let Some(c) = company(scope) {
            self.posting = self.pages.ledger.subscribe_postings(scope, c).await.ok();
        }
    }

    pub(super) async fn reload(&mut self, scope: &Scope) -> Result<()> {
        let report = match company(scope) {
            Some(c) => self.pages.sst.get_return(scope, c, self.id).await,
            None => Err(SstError::Unauthorized),
        };
        match report {
            Ok(report) => {
                self.can_prepare = self.pages.allowed(scope, PREPARE_RETURNS).await?;
                self.can_finalise = self.pages.allowed(scope, FINALISE_RETURNS).await?;
                self.can_amend = self.pages.allowed(scope, AMEND_RETURNS).await?;
                let change = report.amends.is_some();
                if self.items.builtins().iter().any(|c| c.id == "change") != change {
                    self.items = items_columns(change)?;
                }
                if let Some(figures) = &report.figures {
                    self.items.load(&figures.boxes, |b| b.key.to_owned());
                    self.sources
                        .load(&figures.sources, |s| s.line_number.to_string());
                    self.accounts
                        .load(&figures.accounts, |a| a.account_id.to_string());
                }
                if let Some(since) = &report.posted_since {
                    self.posted_since.load(&since.boxes, |b| b.key.to_owned());
                }
                self.adjustments
                    .load(&report.adjustments, |a| a.id.to_string());
                self.events
                    .load(&report.tax_return.events, |e| e.event_number.to_string());
                self.report = Some(report);
                self.refusal = None;
            }
            Err(e) => {
                self.report = None;
                self.refusal = Some(e);
                self.can_prepare = false;
                self.can_finalise = false;
                self.can_amend = false;
            }
        }
        Ok(())
    }

    pub(super) fn title(&self) -> Option<String> {
        self.report.as_ref().map(|r| {
            format!(
                "{} return {}",
                f::tax_name(r.tax_return.tax_type),
                r.tax_return.return_number
            )
        })
    }

    /// What keeps an open return from finalising: unmapped codes, rates that
    /// do not fit, a period not yet ended.
    fn blockers(report: &ReturnReport) -> Option<Blockers> {
        let figures = report.figures.as_ref()?;
        if report.tax_return.status != ReturnStatus::Open {
            return None;
        }
        let blockers = Blockers {
            period_end: (!report.period_ended)
                .then(|| bilibi_finance_ledger::dates::display(report.tax_return.period_end)),
            unmapped: figures
                .unmapped
                .iter()
                .map(|u| Unmapped {
                    source: u.source.as_str(),
                    source_name: f::source_name(u.source),
                    tax_code_id: u.tax_code_id,
                    tax_code: u.tax_code.clone(),
                })
                .collect(),
            rates: figures
                .rate_mismatches
                .iter()
                .map(|r| Rate {
                    line: r.line_number,
                    tax_code: r.tax_code.clone(),
                    rate: f::rate(r.rate),
                    box_name: f::box_name(report.tax_return.tax_type, r.box_key.as_deref()),
                })
                .collect(),
            setup: SETUP,
        };
        (blockers.period_end.is_some()
            || !blockers.unmapped.is_empty()
            || !blockers.rates.is_empty())
        .then_some(blockers)
    }

    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.pages.display(scope).await?;
        let refusal = self
            .refusal
            .as_ref()
            .map(|e| {
                render(
                    EmptyState::missing("Return unavailable", f::refusal(e))
                        .id("sst-return-refusal"),
                )
            })
            .transpose()?;
        let Some(report) = &self.report else {
            return View {
                flash: flash(&self.flash),
                refusal,
                record: None,
            }
            .render()
            .map_err(|_| Error::Internal);
        };
        let r = &report.tax_return;
        let tax_type = r.tax_type;
        let units = report
            .figures
            .as_ref()
            .map(|f| f.minor_units)
            .or(r.minor_units)
            .unwrap_or(0);
        let money = |amount: Option<Decimal>| {
            amount.map_or_else(
                || "—".into(),
                |amount| Money::display_decimal(amount, MinorUnits::new(i64::from(units))),
            )
        };
        let compact = |columns: &ListColumns| columns.mode() == RowMode::Compact;

        // Header.
        let mut status = String::from(r.status.as_str());
        if let Some(v) = r.version {
            status.push_str(&format!(" v{v}"));
        }
        let subtitle = render(Subtitle {
            status: f::badge(&status, f::status_kind(r.status), "")?,
            amends: r.amends_return_id.map(|_| {
                format!(
                    "amends return {}: {}",
                    report
                        .amends
                        .as_ref()
                        .map_or_else(String::new, |a| a.return_number.to_string()),
                    r.reason.clone().unwrap_or_default()
                )
            }),
            superseded: r.superseded_by_return_id.is_some(),
        })?;
        // Back, then the return's own actions as icon buttons.
        let back = render(BackLink {
            href: INDEX,
            id: "sst-return-back",
            title: "Back to SST returns",
            ..BackLink::default()
        })?;
        let mut local = Vec::new();
        if r.status != ReturnStatus::Discarded {
            local.push(RecordAction::local(
                "arrow-down-tray",
                "Export CSV",
                format!("{INDEX}/returns/{}/export.csv", r.id),
            ));
            local.push(RecordAction::local(
                "printer",
                "Print",
                format!("{INDEX}/returns/{}/print", r.id),
            ));
        }
        let actions = RecordActions::render(
            back,
            "sst-return-actions",
            &format!("Return {}", r.return_number),
            local,
            None,
        )?;
        let people = self
            .pages
            .people(
                scope,
                report
                    .adjustments
                    .iter()
                    .flat_map(|a| [Some(a.recorded_by_user_id), a.recorded_by_impersonator_id])
                    .chain(r.events.iter().flat_map(|e| {
                        [Some(e.recorded_by_user_id), e.recorded_by_impersonator_id]
                    })),
            )
            .await?;

        // Part A.
        let mut items = vec![
            fact(
                "SST registration number",
                "sst-return-number",
                report.registration_number.as_deref().unwrap_or("—"),
            ),
            fact(
                "Taxable period",
                "sst-return-period",
                &format!(
                    "{} to {}",
                    bilibi_finance_ledger::dates::display(r.period_start),
                    bilibi_finance_ledger::dates::display(r.period_end)
                ),
            ),
            fact(
                "Return and payment due",
                "sst-return-due",
                &bilibi_finance_ledger::dates::display(r.due_date),
            ),
        ];
        if let Some(figures) = &report.figures {
            if tax_type == TaxType::ServiceTax {
                let basis = figures.service_tax_basis.as_deref().or(report
                    .registration
                    .as_ref()
                    .map(|g| g.service_tax_basis.as_str()));
                items.push(fact(
                    "Service tax counted",
                    "sst-return-basis",
                    f::basis_name(basis),
                ));
            }
            items.push(fact(
                "Read through",
                "sst-return-through",
                &format!(
                    "ledger entry {}, mapping {}",
                    figures.through_entry_number, figures.through_mapping_number
                ),
            ));
        }
        let particulars = render(Facts {
            id: "sst-return-particulars",
            items,
        })?;
        let blockers = Self::blockers(report);
        let open = r.status == ReturnStatus::Open;
        let controls = render(Controls {
            open,
            finalise: open && self.can_finalise,
            blocked: blockers.is_some(),
            discard: open && self.can_prepare,
            finalise_forbidden: (open && !self.can_finalise)
                .then(|| {
                    let mut empty = EmptyState::forbidden("finalise SST returns")
                        .id("sst-return-finalise-forbidden");
                    empty.class = "mt-4".into();
                    render(empty)
                })
                .transpose()?,
            amend: r.status == ReturnStatus::Finalised
                && r.superseded_by_return_id.is_none()
                && self.can_amend,
        })?;
        let blockers = blockers.map(render).transpose()?;

        // Posted since this return was finalised.
        let posted_since = match &report.posted_since {
            Some(since) if !since.boxes.is_empty() => {
                let mut rows = Vec::new();
                for b in &since.boxes {
                    let mut row = FlexRow::new(b.key);
                    row.dom_id = Some(format!("posted-since-{}", b.key));
                    rows.push(
                        row.cell("item", Cell::text(format!("{} {}", b.item, b.label)))
                            .cell("value_change", Cell::text(money(b.taxable_change)))
                            .cell("tax_change", Cell::text(money(b.tax_change))),
                    );
                }
                Some(table(
                    &self.posted_since,
                    "posted_since_grid",
                    "Posted since this return was finalised",
                    rows,
                    None,
                    false,
                )?)
            }
            _ => None,
        };

        // SST-02 items.
        let items = match &report.figures {
            Some(figures) => {
                let mut rows = Vec::new();
                for b in &figures.boxes {
                    let total = b.kind == BoxKind::Total;
                    let mut row = FlexRow::new(b.key);
                    row.dom_id = Some(format!("box-{}", b.key));
                    row = row
                        .cell("item", Cell::text(b.item))
                        .slot(
                            "description",
                            render(Strong {
                                text: b.label,
                                strong: total,
                            })?,
                        )
                        .cell("value", Cell::text(money(b.taxable_amount)))
                        .slot(
                            "tax",
                            render(Strong {
                                text: &money(b.tax_amount),
                                strong: total,
                            })?,
                        );
                    if let Some(amends) = &report.amends {
                        let change = amends
                            .boxes
                            .iter()
                            .find(|c| c.key == b.key)
                            .map_or_else(String::new, |c| money(c.tax_change.or(c.taxable_change)));
                        // Blank when unchanged, as Bilimbi; an empty cell would read as "—".
                        row = row.slot("change", Html::text(&change));
                    }
                    rows.push(row);
                }
                Some(ItemsView {
                    description: format!(
                        "In {}. Items 15 and 16 (penalty), Part B1's item lines, and Part F's declaration are completed when filing.",
                        figures.currency_code
                    ),
                    table: table(&self.items, "items_grid", "SST-02 items", rows, None, false)?,
                })
            }
            None => None,
        };

        // Adjustments.
        let adjustments = if r.status == ReturnStatus::Discarded {
            None
        } else {
            let mut rows = Vec::new();
            for a in &report.adjustments {
                let mut row = FlexRow::new(a.id.to_string());
                row.dom_id = Some(format!("adjustment-{}", a.adjustment_number));
                let mut lines = Vec::new();
                if !compact(&self.adjustments) {
                    if let Some(reference) = &a.reference {
                        lines.push(reference.clone());
                    }
                    if let Some(w) = &a.withdrawal {
                        lines.push(format!("Withdrawn: {}", w.description));
                    }
                }
                row = row
                    .cell("number", Cell::text(a.adjustment_number.to_string()))
                    .cell("item", Cell::text(f::box_name(tax_type, Some(&a.box_key))))
                    .slot(
                        "description",
                        render(Lines {
                            text: &a.description,
                            lines,
                        })?,
                    )
                    .cell("value", Cell::text(money(Some(a.taxable_amount))))
                    .cell("tax", Cell::text(money(Some(a.tax_amount))))
                    .cell(
                        "by",
                        Cell::text(
                            people.who(Some(a.recorded_by_user_id), a.recorded_by_impersonator_id),
                        ),
                    );
                // Withdrawing acts on this adjustment here: an icon button.
                if open && a.standing && self.can_prepare {
                    let label = format!("Withdraw adjustment {}", a.adjustment_number);
                    row.actions = render(IconButton {
                        id: &format!("adjustment-{}-withdraw", a.id),
                        event: "open_withdraw",
                        name: "id",
                        value: &a.id.to_string(),
                        button_type: "button",
                        title: &label,
                        ..IconButton::new("x-mark", &label)
                    })?;
                }
                rows.push(row);
            }
            let add = (open && self.can_prepare)
                .then(|| {
                    render(CreateButton {
                        noun: "Adjustment",
                        id: "sst-adjustment-new",
                        event: "new_adjustment",
                        button_type: "button",
                        ..CreateButton::default()
                    })
                })
                .transpose()?
                .unwrap_or(Html::EMPTY);
            Some(AdjustmentsView {
                count: Some(report.adjustments.iter().filter(|a| a.standing).count()),
                table: table(
                    &self.adjustments,
                    "adjustments_grid",
                    "Adjustments",
                    rows,
                    Some((
                        "No adjustments",
                        "The figures come from posted documents alone.",
                    )),
                    true,
                )?,
                add,
            })
        };

        // Where the figures come from, and the ledger reconciliation.
        let (sources, accounts) = match &report.figures {
            Some(figures) => {
                let mut rows = Vec::new();
                for s in &figures.sources {
                    let mut row = FlexRow::new(s.line_number.to_string());
                    row.dom_id = Some(format!("source-{}", s.line_number));
                    let mut lines = Vec::new();
                    if !compact(&self.sources) {
                        let mut measure = f::measure_name(&s.measure).to_owned();
                        if s.prior_period && s.measure == "documents" {
                            measure.push_str(", on an earlier period's invoice");
                        }
                        lines.push(measure);
                    }
                    rows.push(
                        row.slot(
                            "source",
                            render(Lines {
                                text: &format!(
                                    "{} {}",
                                    f::source_name(s.source),
                                    f::kind_name(&s.document_kind)
                                ),
                                lines,
                            })?,
                        )
                        .cell(
                            "tax_code",
                            Cell::text(format!("{} ({}%)", s.tax_code, f::rate(s.rate))),
                        )
                        .cell("documents", Cell::text(s.document_count.to_string()))
                        .cell(
                            "item",
                            Cell::text(f::box_name(tax_type, s.box_key.as_deref())),
                        )
                        .cell("value", Cell::text(money(Some(s.taxable_amount))))
                        .cell("tax", Cell::text(money(Some(s.tax_amount)))),
                    );
                }
                let sources = table(
                    &self.sources,
                    "sources_grid",
                    "Where the figures come from",
                    rows,
                    Some((
                        "No documents",
                        "No posted document in the period carries a tax code of this return.",
                    )),
                    false,
                )?;
                let mut rows = Vec::new();
                for a in &figures.accounts {
                    let mut row = FlexRow::new(a.account_id.to_string());
                    row.dom_id = Some(format!("account-{}", a.account_id));
                    let mut difference = Html::text(&money(Some(a.difference)));
                    difference.push(&if a.difference.is_zero() {
                        f::badge("agrees", bilibi_ui::BadgeKind::Success, "ml-1")?
                    } else {
                        f::badge("differs", bilibi_ui::BadgeKind::Warning, "ml-1")?
                    });
                    rows.push(
                        row.slot(
                            "account",
                            render(AccountName {
                                name: &a.account_name,
                                code: &a.account_code,
                            })?,
                        )
                        .cell("ledger_debits", Cell::text(money(Some(a.ledger_debits))))
                        .cell("ledger_credits", Cell::text(money(Some(a.ledger_credits))))
                        .cell(
                            "document_debits",
                            Cell::text(money(Some(a.document_debits))),
                        )
                        .cell(
                            "document_credits",
                            Cell::text(money(Some(a.document_credits))),
                        )
                        .slot("difference", difference),
                    );
                }
                let accounts = table(
                    &self.accounts,
                    "accounts_grid",
                    "Ledger reconciliation",
                    rows,
                    Some((
                        "No tax account",
                        "No tax code of this return names a ledger account yet.",
                    )),
                    false,
                )?;
                (Some(sources), Some(accounts))
            }
            None => (None, None),
        };

        // History.
        let mut rows = Vec::new();
        for e in &r.events {
            let mut row = FlexRow::new(e.event_number.to_string());
            row.dom_id = Some(format!("event-{}", e.event_number));
            let mut label = match e.action.as_str() {
                "opened" => "Opened".to_owned(),
                "finalised" => "Finalised".to_owned(),
                "discarded" => "Discarded".to_owned(),
                other => other.to_owned(),
            };
            if let Some(v) = e.version {
                label.push_str(&format!(" as version {v}"));
            }
            rows.push(
                row.cell("event", Cell::text(label))
                    .cell(
                        "by",
                        Cell::text(
                            people.who(Some(e.recorded_by_user_id), e.recorded_by_impersonator_id),
                        ),
                    )
                    .slot(
                        "when",
                        time(
                            &display,
                            &format!("event-{}-at", e.event_number),
                            &e.recorded_at,
                        )?,
                    )
                    .cell("reason", Cell::text(e.reason.clone().unwrap_or_default())),
            );
        }
        let events = table(&self.events, "events_grid", "History", rows, None, false)?;

        let modal = self.modal.map(|m| ModalView {
            title: match m {
                Modal::Discard => "Discard this return",
                Modal::Amend => "Amend this return",
                Modal::Withdraw(_) => "Withdraw this adjustment",
            },
            description: match m {
                Modal::Discard => "It stays on record as discarded with this reason, and the period can be prepared again.",
                Modal::Amend => "A new return for the same period opens, naming this one; this one stays as it was finalised.",
                Modal::Withdraw(_) => "It stays on record as withdrawn with this reason and no longer counts.",
            },
            reason_field: Field::in_form("reason", "reason")
                .id("sst-reason")
                .label("Reason")
                .value(&self.reason)
                .attr("maxlength", "500")
                .required(),
        });
        let adjustment = (self.adding && open && self.can_prepare)
            .then(|| -> Result<AdjustmentModal> {
                Ok(AdjustmentModal {
                    alert: self
                        .adjustment_alert
                        .as_ref()
                        .map(|message| {
                            render(Alert {
                                kind: StatusKind::Error,
                                id: "sst-adjustment-alert",
                                class: "mt-4",
                                body: Html::text(message),
                            })
                        })
                        .transpose()?,
                    fields: self.adjustment_fields(tax_type)?,
                    actions: dialog_actions(
                        "sst-adjustment",
                        "close_adjustment",
                        "Add adjustment",
                        "Adding…",
                    )?,
                })
            })
            .transpose()?;

        let title = format!(
            "{} return {}: {} to {}",
            f::tax_name(tax_type),
            r.return_number,
            bilibi_finance_ledger::dates::display(r.period_start),
            bilibi_finance_ledger::dates::display(r.period_end)
        );
        View {
            flash: flash(&self.flash),
            refusal,
            record: Some(RecordView {
                record: r.id,
                title_actions: bilibi_ui::Html::EMPTY,
                title,
                subtitle,
                actions,
                particulars,
                controls,
                blockers,
                posted_since,
                items,
                adjustments,
                sources,
                accounts,
                events,
                modal,
                adjustment,
            }),
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// The dialog's fields; the item is a searchable choice, name first and
    /// its number muted.
    fn adjustment_fields(&self, tax_type: TaxType) -> Result<Vec<Html>> {
        let field = |key: &str, id: &str, label: &str| {
            Field::in_form("adjustment", key)
                .id(id)
                .label(label)
                .value(self.adjustment.get(key).map_or("", String::as_str))
                .errors(
                    self.adjustment_errors
                        .iter()
                        .filter(|e| e.field == key)
                        .map(|e| e.message.clone())
                        .collect(),
                )
        };
        let item = Combobox::new("adjustment-box", "adjustment[box]")
            .label("Item *")
            .aria_label("Item")
            .placeholder("Search SST-02 items")
            .value(self.adjustment.get("box").map_or("", String::as_str))
            .options(
                Sst::boxes(tax_type)
                    .iter()
                    .filter(|b| b.kind != BoxKind::Total)
                    .map(|b| Choice::new(b.label, b.key).detail(format!("item {}", b.item)))
                    .collect(),
            )
            .errors(
                self.adjustment_errors
                    .iter()
                    .filter(|e| e.field == "box")
                    .map(|e| e.message.clone())
                    .collect(),
            );
        let mut fields = vec![render(item)?];
        for f in [
            field("description", "adjustment-description", "Description")
                .attr("maxlength", "500")
                .required(),
            field("taxable_amount", "adjustment-taxable", "Value").attr("inputmode", "decimal"),
            field("tax_amount", "adjustment-tax", "Tax").attr("inputmode", "decimal"),
            field("reference", "adjustment-reference", "Reference").attr("maxlength", "100"),
        ] {
            fields.push(render(f)?);
        }
        Ok(fields)
    }

    /// Bilimbi `act`: the capability again, then the facade; a success closes
    /// the dialog, and either outcome reads the return again.
    async fn act(&mut self, scope: &Scope, capability: &str, action: Action) -> Result<bool> {
        if !self.pages.allowed(scope, capability).await? {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(false);
        }
        let Some(company) = company(scope) else {
            self.flash = Some((StatusKind::Error, f::refusal(&SstError::Unauthorized)));
            return Ok(true);
        };
        let sst = &self.pages.sst;
        let reason = Some(self.reason.as_str());
        let outcome = match action {
            Action::Finalise => sst.finalise_return(scope, company, self.id).await.map(|r| {
                format!(
                    "Return {} finalised as version {}; its figures are now fixed.",
                    r.return_number,
                    r.version.unwrap_or_default()
                )
            }),
            Action::Discard => sst
                .discard_return(scope, company, self.id, reason)
                .await
                .map(|_| "The return was discarded; it stays on record.".to_owned()),
            Action::Withdraw(adjustment) => sst
                .withdraw_adjustment(scope, company, adjustment, reason)
                .await
                .map(|_| "The adjustment was withdrawn; it stays on record.".to_owned()),
        };
        match outcome {
            Ok(message) => {
                self.flash = Some((StatusKind::Success, message));
                self.modal = None;
                self.reason.clear();
            }
            Err(e) => self.flash = Some((StatusKind::Error, f::refusal(&e))),
        }
        Ok(true)
    }

    async fn amend(&mut self, scope: &Scope) -> Result<bool> {
        if !self.pages.allowed(scope, AMEND_RETURNS).await? {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(false);
        }
        let result = match company(scope) {
            Some(c) => {
                self.pages
                    .sst
                    .amend_return(scope, c, self.id, Some(&self.reason))
                    .await
            }
            None => Err(SstError::Unauthorized),
        };
        match result {
            Ok(amendment) => {
                self.redirect = Some((
                    format!("{INDEX}/returns/{}", amendment.id),
                    format!("Amendment opened as return {}.", amendment.return_number),
                ));
            }
            Err(e) => self.flash = Some((StatusKind::Error, f::refusal(&e))),
        }
        Ok(false)
    }

    async fn add_adjustment(&mut self, scope: &Scope) -> Result<bool> {
        if !self.pages.allowed(scope, PREPARE_RETURNS).await? {
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(false);
        }
        let value = |k: &str| self.adjustment.get(k).cloned().unwrap_or_default();
        let amount = |k: &str| Some(value(k)).filter(|v| !v.is_empty());
        let input = AdjustmentInput {
            box_key: value("box"),
            taxable_amount: amount("taxable_amount"),
            tax_amount: amount("tax_amount"),
            description: Some(value("description")),
            reference: Some(value("reference")),
        };
        let result = match company(scope) {
            Some(c) => {
                self.pages
                    .sst
                    .add_adjustment(scope, c, self.id, &input)
                    .await
            }
            None => Err(SstError::Unauthorized),
        };
        match result {
            Ok(a) => {
                self.flash = Some((
                    StatusKind::Success,
                    format!("Adjustment {} added.", a.adjustment_number),
                ));
                self.adjustment.clear();
                self.adjustment_errors.clear();
                self.adding = false;
                self.adjustment_alert = None;
                Ok(true)
            }
            // A field's refusal shows beside it; the rest in the dialog's alert.
            Err(SstError::Invalid(errors)) => {
                let other: Vec<String> = errors
                    .iter()
                    .filter(|e| !ADJUSTMENT_FIELDS.contains(&e.field))
                    .map(|e| format!("{} {}", e.field.replace('_', " "), e.message))
                    .collect();
                self.adjustment_alert = (!other.is_empty()).then(|| other.join(" "));
                self.adjustment_errors = errors;
                Ok(false)
            }
            Err(e) => {
                self.adjustment_alert = Some(f::refusal(&e));
                Ok(true)
            }
        }
    }

    async fn event(&mut self, scope: &Scope, v: &serde_json::Value) -> Result<bool> {
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => {
                clear_flash(&mut self.flash, v);
                Ok(false)
            }
            grid @ ("posted_since_grid" | "items_grid" | "adjustments_grid" | "sources_grid"
            | "accounts_grid" | "events_grid") => {
                let columns = match grid {
                    "posted_since_grid" => &mut self.posted_since,
                    "items_grid" => &mut self.items,
                    "adjustments_grid" => &mut self.adjustments,
                    "sources_grid" => &mut self.sources,
                    "accounts_grid" => &mut self.accounts,
                    _ => &mut self.events,
                };
                columns.handle(&params::string_values(v));
                Ok(false)
            }
            "finalise" => self.act(scope, FINALISE_RETURNS, Action::Finalise).await,
            "open_modal" => {
                self.modal = match v["modal"].as_str() {
                    Some("discard") => Some(Modal::Discard),
                    Some("amend") => Some(Modal::Amend),
                    _ => return Err(Error::Invalid),
                };
                self.reason.clear();
                Ok(false)
            }
            "open_withdraw" => {
                let id = params::positive_id(v["id"].as_str());
                // Only an adjustment this return shows; never an id of the browser's choosing.
                self.modal = self
                    .report
                    .as_ref()
                    .and_then(|r| r.adjustments.iter().find(|a| Some(a.id) == id))
                    .map(|a| Modal::Withdraw(a.id));
                self.reason.clear();
                Ok(false)
            }
            "close_modal" => {
                self.modal = None;
                self.reason.clear();
                Ok(false)
            }
            "reason_change" => {
                self.reason = text(v, "reason", "reason");
                Ok(false)
            }
            "submit_reason" => {
                self.reason = text(v, "reason", "reason");
                match self.modal {
                    Some(Modal::Discard) => self.act(scope, PREPARE_RETURNS, Action::Discard).await,
                    Some(Modal::Amend) => self.amend(scope).await,
                    Some(Modal::Withdraw(id)) => {
                        self.act(scope, PREPARE_RETURNS, Action::Withdraw(id)).await
                    }
                    None => Ok(false),
                }
            }
            "adjustment_change" => {
                self.adjustment = adjustment_values(v);
                Ok(false)
            }
            "new_adjustment" => {
                self.adding = true;
                self.adjustment_alert = None;
                self.adjustment_errors.clear();
                self.flash = None;
                Ok(false)
            }
            "close_adjustment" => {
                self.adding = false;
                self.adjustment_alert = None;
                self.adjustment.clear();
                self.adjustment_errors.clear();
                Ok(false)
            }
            "add_adjustment" => {
                self.adjustment = adjustment_values(v);
                self.add_adjustment(scope).await
            }
            _ => Err(Error::Invalid),
        }
    }
}

enum Action {
    Finalise,
    Discard,
    Withdraw(i64),
}

/// A posted form field in full: unlike `form_values`' 1 KiB limit, a 500
/// character reason or description can take 2 KiB, and the facade bounds it.
fn text(v: &serde_json::Value, form: &str, key: &str) -> String {
    v[format!("{form}[{key}]")]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn adjustment_values(v: &serde_json::Value) -> HashMap<String, String> {
    ADJUSTMENT_FIELDS
        .iter()
        .map(|k| ((*k).to_owned(), text(v, "adjustment", k)))
        .collect()
}

fn fact<'a>(title: &'a str, id: &'a str, value: &str) -> Fact<'a> {
    Fact {
        title,
        id,
        value: Html::text(value),
    }
}

impl LiveState<Scope, Error> for Return {
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let Some(mut posting) = self.posting.take() else {
            return vec![];
        };
        // The receiver exists before forwarding begins; the mount's first
        // Refresh then reads after LISTEN, so no posting falls in between.
        let (sender, receiver) = broadcast::channel(1);
        let dirty = self.dirty.clone();
        let tenant = self.tenant;
        self.bridge = Some(tokio::spawn(async move {
            while posting.next().await.is_some() {
                if !dirty.swap(true, Ordering::SeqCst) {
                    let _ = sender.send(page::Change { tenant, user: None });
                }
            }
        }));
        vec![Subscription { receiver }]
    }

    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let reload = match r.input {
                LiveInput::Params(_) => true,
                // A posting notice, or the mount's first read. An open return
                // follows the books; a finalised one cannot change, but what
                // documents posted since would change is read again.
                LiveInput::Refresh => {
                    let posted = self.dirty.swap(false, Ordering::SeqCst);
                    self.report.is_none()
                        || !posted
                        || self
                            .report
                            .as_ref()
                            .is_some_and(|r| r.tax_return.status != ReturnStatus::Discarded)
                }
                LiveInput::Event(v) => self.event(r.scope, v).await?,
            };
            if let Some((path, message)) = self.redirect.take() {
                return Ok(LiveBody {
                    redirect: Some(path),
                    flash: Some((StatusKind::Success, message)),
                    ..LiveBody::default()
                });
            }
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

#[derive(Template)]
#[template(path = "return.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    refusal: Option<Html>,
    record: Option<RecordView>,
}

struct RecordView {
    record: i64,
    title: String,
    title_actions: Html,
    subtitle: Html,
    actions: Html,
    particulars: Html,
    controls: Html,
    blockers: Option<Html>,
    posted_since: Option<Html>,
    items: Option<ItemsView>,
    adjustments: Option<AdjustmentsView>,
    sources: Option<Html>,
    accounts: Option<Html>,
    events: Html,
    modal: Option<ModalView>,
    adjustment: Option<AdjustmentModal>,
}

/// The "+ Adjustment" dialog.
struct AdjustmentModal {
    alert: Option<Html>,
    fields: Vec<Html>,
    actions: Html,
}

struct ItemsView {
    description: String,
    table: Html,
}

struct AdjustmentsView {
    count: Option<usize>,
    table: Html,
    add: Html,
}

struct ModalView {
    title: &'static str,
    description: &'static str,
    reason_field: Field,
}

#[derive(Template)]
#[template(
    source = r#"<span id="sst-return-status">{{ status|safe }}</span>{% if let Some(amends) = amends %}<span id="sst-return-amends" class="ml-2">{{ amends }}</span>{% endif %}{% if superseded %}<span id="sst-return-superseded" class="ml-2">amended by a later version</span>{% endif %}"#,
    ext = "html"
)]
struct Subtitle {
    status: Html,
    amends: Option<String>,
    superseded: bool,
}

#[derive(Template)]
#[template(path = "return_controls.html")]
struct Controls {
    open: bool,
    finalise: bool,
    blocked: bool,
    discard: bool,
    finalise_forbidden: Option<Html>,
    amend: bool,
}

#[derive(Template)]
#[template(path = "return_blockers.html")]
struct Blockers {
    period_end: Option<String>,
    unmapped: Vec<Unmapped>,
    rates: Vec<Rate>,
    setup: &'static str,
}
struct Unmapped {
    source: &'static str,
    source_name: &'static str,
    tax_code_id: i64,
    tax_code: String,
}
struct Rate {
    line: i32,
    tax_code: String,
    rate: String,
    box_name: String,
}

#[derive(Template)]
#[template(
    source = r#"<span{% if strong %} class="font-semibold"{% endif %}>{{ text }}</span>"#,
    ext = "html"
)]
struct Strong<'a> {
    text: &'a str,
    strong: bool,
}

#[derive(Template)]
#[template(
    source = r#"{{ text }}{% for line in lines %}<span class="block text-xs text-ink-muted">{{ line }}</span>{% endfor %}"#,
    ext = "html"
)]
struct Lines<'a> {
    text: &'a str,
    lines: Vec<String>,
}
