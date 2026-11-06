//! One receipt: who paid, how much and where it went, the invoices it settles,
//! the ledger entries, and its reversal if it has one. Reads `payment.get`;
//! Reverse calls `payment.reverse`, which posts a dated mirror entry and keeps
//! the original receipt as it was.
use super::{
    documents::reference,
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written, CUSTOMERS, DENIED, INVOICES, RECEIPTS,
};
use crate::{Customer, Document, Payment as Record, POST_PAYMENTS};
use bilibi_database::{Error, Result};
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::{DocumentLayout, EntryLink, People, RecordAction, RecordActions};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, BackLink, Button, ButtonVariant, Card, Cell, EmptyState, Facts, Field, FlexRow, Form,
    Header, Html, InputKind, ListColumns, ModalWidth, PageVariant, SectionHeading, StatusKind,
};
use serde_json::json;
use std::collections::HashMap;

pub(super) struct Payment {
    pages: Pages,
    id: i64,
    record: Option<Record>,
    customer: Option<Customer>,
    documents: Vec<Document>,
    accounts: Vec<Account>,
    figures: f::Figures,
    can_post: bool,
    flash: Flash,
    reversing: Option<(HashMap<String, String>, Errors)>,
    /// The dialog an address asked for (`?do=reverse`).
    requested: Option<String>,
    allocations: ListColumns,
}

impl Payment {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            record: None,
            customer: None,
            documents: vec![],
            accounts: vec![],
            figures: f::Figures::default(),
            can_post: false,
            flash: Flash::default(),
            reversing: None,
            requested: RecordActions::requested(query),
            allocations: f::columns(
                "receivables-receipt-allocations-table",
                &[
                    ("invoice", "Invoice"),
                    ("due", "Due"),
                    ("amount", "Applied"),
                ],
                &["amount"],
            )?,
        })
    }

    pub(super) fn title(&self) -> Option<String> {
        self.record
            .as_ref()
            .map(|p| format!("Receipt {}", p.payment_reference))
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.mounted(scope).await?;
        self.body(scope).await
    }

    /// Load, then open the dialog the address asked for (`?do=reverse`).
    pub(super) async fn mounted(&mut self, scope: &Scope) -> Result<()> {
        self.load(scope).await?;
        if self.requested.take().as_deref() == Some("reverse") {
            self.ask_reverse(scope).await;
        }
        Ok(())
    }

    async fn ask_reverse(&mut self, scope: &Scope) {
        if self.can_post && self.record.as_ref().is_some_and(|p| p.reversal.is_none()) {
            let today = self.pages.today(scope).await;
            self.reversing = Some((
                HashMap::from([("reversal_date".to_owned(), today)]),
                Errors::default(),
            ));
        } else {
            self.flash.set(
                StatusKind::Error,
                "You do not have permission to reverse receipts.",
            );
        }
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.record = match self
            .pages
            .read(
                scope,
                "finance.receivables.payment.get",
                json!({"payment_id": self.id}),
            )
            .await
        {
            Ok(value) => serde_json::from_value(value).ok(),
            Err(Error::NotFound) => None,
            Err(error) => return Err(error),
        };
        self.can_post = self.pages.allowed(scope, POST_PAYMENTS).await?;
        let Some(p) = &self.record else { return Ok(()) };
        let customer_id = p.customer_id;
        self.figures = self.pages.figures(scope).await;
        self.accounts = self.pages.accounts(scope).await;
        self.customer = self
            .pages
            .read(
                scope,
                "finance.receivables.customer.get",
                json!({"customer_id": customer_id}),
            )
            .await
            .ok()
            .and_then(|v| serde_json::from_value(v).ok());
        self.documents = self
            .pages
            .read_all(
                scope,
                "finance.receivables.document.list",
                json!({"customer_id": customer_id, "kind": "invoice", "status": "issued"}),
                ("after", "next_after"),
            )
            .await?
            .0;
        Ok(())
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let back = render(BackLink {
            href: RECEIPTS,
            id: "receivables-receipt-back",
            title: "Back to receipts",
            ..BackLink::default()
        })?;
        let Some(p) = self.record.clone() else {
            let mut missing = EmptyState::missing(
                "Receipt not found",
                "There is no such receipt in this company, or you cannot read it.",
            );
            missing.id = "receivables-receipt-missing".into();
            missing.action = back;
            return f::frame(
                "receivables-receipt",
                PageVariant::Detail,
                &self.flash,
                render(missing)?,
                None,
            );
        };
        let display = self.pages.display(scope).await?;
        let people = self
            .pages
            .people(
                scope,
                [
                    Some(p.recorded_by_user_id),
                    p.recorded_by_impersonator_id,
                    p.reversal.as_ref().map(|r| r.reversed_by_user_id),
                    p.reversal.as_ref().and_then(|r| r.reversed_by_impersonator_id),
                ],
            )
            .await?;
        let mut items = Vec::new();
        if self.can_post && p.reversal.is_none() {
            items.push(RecordAction::here(
                "Reverse receipt",
                &format!("{RECEIPTS}/{}", p.id),
                "reverse",
            ));
        }
        let pin_name = if p.payment_reference.trim().is_empty() {
            format!("Receipt {}", p.id)
        } else {
            p.payment_reference.clone()
        };
        let actions = RecordActions::render(
            back,
            "receivables-receipt-actions",
            &pin_name,
            items,
            None,
        )?;
        let customer_name = self.customer.as_ref().map_or_else(
            || format!("Customer #{}", p.customer_id),
            |c| c.name.clone(),
        );
        let facts = render(Facts {
            id: "receivables-receipt-facts",
            items: vec![
                f::fact_html(
                    "Customer",
                    f::link(&format!("{CUSTOMERS}/{}", p.customer_id), &customer_name)?,
                ),
                f::fact("Date received", &p.payment_date),
                f::fact(
                    "Amount",
                    &self.figures.with_currency(p.amount, &p.currency_code),
                ),
                f::fact_html(
                    "Deposited to",
                    super::account_html(&self.accounts, p.deposit_account_id)?,
                ),
                f::fact("Reference", f::optional(&p.reference)),
                f::fact("Description", f::optional(&p.description)),
            ],
        })?;
        self.allocations
            .load(&p.allocations, |a| a.document_id.to_string());
        let mut rows = Vec::new();
        for a in &p.allocations {
            let doc = self.documents.iter().find(|d| d.id == a.document_id);
            let mut row = FlexRow::new(a.document_id.to_string());
            row.dom_id = Some(format!("receivables-receipt-allocation-{}", a.document_id));
            rows.push(
                row.slot(
                    "invoice",
                    f::link(
                        &format!("{INVOICES}/{}", a.document_id),
                        &doc.map_or_else(|| format!("Invoice #{}", a.document_id), reference),
                    )?,
                )
                .cell(
                    "due",
                    Cell::text(doc.map_or("—", |d| f::optional(&d.due_date))),
                )
                .slot("amount", f::tabular(&self.figures.money(a.amount))?),
            );
        }
        let mut body = f::announce(p.id)?;
        body.push(&render(Header {
            title: Html::text(&pin_name),
            title_actions: super::payments::status_badge(&p)?,
            subtitle: &format!("Receipt from {customer_name}"),
            actions,
        })?);
        let mut document = render(Card {
            id: "receivables-receipt-details",
            role: "region",
            labelledby: "receivables-receipt-details-heading",
            body: {
                let mut b = render(SectionHeading {
                    id: "receivables-receipt-details-heading",
                    title: "Receipt",
                    ..SectionHeading::default()
                })?;
                b.push(&facts);
                b
            },
            ..Card::default()
        })?;
        document.push(&render(Card {
            id: "receivables-receipt-allocations",
            role: "region",
            labelledby: "receivables-receipt-allocations-heading",
            inner_class: "p-0",
            body: {
                let mut b = render(SectionHeading {
                    id: "receivables-receipt-allocations-heading",
                    title: "Applied to",
                    class: "p-5 pb-0",
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.allocations,
                    "allocations_grid",
                    "Invoices this receipt settles",
                    rows,
                    "This receipt settles no invoice.",
                )?);
                b
            },
            ..Card::default()
        })?);
        let mut summary_items = vec![
            f::fact_html("Status", super::payments::status_badge(&p)?),
            f::fact_html(
                "Journal",
                EntryLink::html(Some(p.ledger_entry_id), Some(p.ledger_entry_number))?,
            ),
            f::fact_html(
                "Recorded",
                stamp(
                    &display,
                    &people,
                    p.recorded_by_user_id,
                    p.recorded_by_impersonator_id,
                    &p.recorded_at,
                    "receivables-receipt-recorded-at",
                )?,
            ),
        ];
        if let Some(r) = &p.reversal {
            summary_items.extend([
                f::fact("Reversal date", &r.reversal_date),
                f::fact("Reversal reason", &r.reason),
                f::fact_html(
                    "Reversal journal",
                    EntryLink::html(Some(r.ledger_entry_id), Some(r.ledger_entry_number))?,
                ),
                f::fact_html(
                    "Reversed",
                    stamp(
                        &display,
                        &people,
                        r.reversed_by_user_id,
                        r.reversed_by_impersonator_id,
                        &r.reversed_at,
                        "receivables-receipt-reversed-at",
                    )?,
                ),
            ]);
        }
        let mut summary = render(Card {
            id: "receivables-receipt-summary-card",
            role: "region",
            labelledby: "receivables-receipt-summary-heading",
            body: {
                let mut b = render(SectionHeading {
                    id: "receivables-receipt-summary-heading",
                    title: "Summary",
                    ..SectionHeading::default()
                })?;
                b.push(&render(Facts {
                    id: "receivables-receipt-summary",
                    items: summary_items,
                })?);
                b
            },
            ..Card::default()
        })?;
        summary.push(&render(SlotHtml)?);
        body.push(&DocumentLayout::render(
            "receivables-receipt-layout",
            document,
            summary,
        )?);
        let modal = match &self.reversing {
            Some((values, errors)) => Some(self.dialog(&p, values, errors)?),
            None => None,
        };
        f::frame(
            "receivables-receipt",
            PageVariant::Detail,
            &self.flash,
            body,
            modal,
        )
    }

    fn dialog(
        &self,
        p: &Record,
        values: &HashMap<String, String>,
        errors: &Errors,
    ) -> Result<Html> {
        let labels = vec![
            (
                "reversal_date".to_owned(),
                "receivables-reverse-reversal_date".to_owned(),
                "Reversal date".to_owned(),
            ),
            (
                "reason".into(),
                "receivables-reverse-reason".into(),
                "Reason".into(),
            ),
        ];
        let mut body = render(errors.summary("receivables-reverse-errors", &labels))?;
        body.push(&render(
            Field::in_form("reverse", "reversal_date")
                .id("receivables-reverse-reversal_date")
                .label("Reversal date")
                .kind(InputKind::Date)
                .value(values.get("reversal_date").cloned().unwrap_or_default())
                .required()
                .hint(format!("On or after {}, the receipt date.", p.payment_date))
                .errors(errors.under("reversal_date", "Reversal date")),
        )?);
        body.push(&render(
            Field::in_form("reverse", "reason")
                .id("receivables-reverse-reason")
                .label("Reason (1 to 500 characters)")
                .kind(InputKind::Textarea)
                .attr("maxlength", "500")
                .value(values.get("reason").cloned().unwrap_or_default())
                .required()
                .errors(errors.under("reason", "Reason (1 to 500 characters)")),
        )?);
        for message in errors.base() {
            body.push(&Html::text(&message));
        }
        let mut actions = render(Button {
            id: "receivables-reverse-cancel",
            button_type: "button",
            event: "cancel_reverse",
            body: Html::text("Keep receipt"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "receivables-reverse-confirm",
            button_type: "submit",
            variant: ButtonVariant::Danger,
            working: "Reversing…",
            body: Html::text("Reverse receipt"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "receivables-reverse-form",
            event: "reverse",
            change: false,
            class: "mt-4",
            body,
            actions: f::actions(actions)?,
        })?;
        f::modal(
            "receivables-reverse",
            "Reverse this receipt",
            "The invoices it settled owe their money again. This is recorded with your name and cannot be undone.",
            "cancel_reverse",
            ModalWidth::Narrow,
            &self.flash,
            form,
        )
    }
}

fn stamp(
    display: &DisplayContext,
    people: &People,
    user: i64,
    impersonator: Option<i64>,
    at: &str,
    element: &str,
) -> Result<Html> {
    let mut value = Html::text(&people.who(Some(user), impersonator));
    value.push(&Html::text(" · "));
    value.push(&f::time(display, element, at)?);
    Ok(value)
}

#[derive(askama::Template)]
#[template(
    source = r#"<div id="receivables-receipt-attachments-slot" data-live-ignore><div id="receivables-receipt-attachments"></div></div>"#,
    ext = "html"
)]
struct SlotHtml;

impl LiveState<Scope, Error> for Payment {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(v) = r.input {
                match v["type"].as_str().ok_or(Error::Invalid)? {
                    "clear-flash" => self.flash.dismiss(v),
                    "allocations_grid" => {
                        self.allocations.handle(&params::string_values(v));
                    }
                    "ask_reverse" => self.ask_reverse(r.scope).await,
                    "cancel_reverse" => self.reversing = None,
                    "reverse" => {
                        let form = params::form_values(v, "reverse");
                        let input = json!({
                            "payment_id": self.id,
                            "reversal_date": f::posted(&form, "reversal_date"),
                            "reason": f::posted(&form, "reason"),
                        });
                        match self
                            .pages
                            .write(
                                r.scope,
                                r.credential,
                                "reverse",
                                "finance.receivables.payment.reverse",
                                input,
                            )
                            .await?
                        {
                            Written::Done(_) => {
                                self.reversing = None;
                                self.flash.set(
                                    StatusKind::Success,
                                    "Receipt reversed. Its invoices owe their money again.",
                                );
                            }
                            Written::Invalid(fields) => {
                                self.reversing = Some((form, Errors(fields)))
                            }
                            Written::Refused(message) => {
                                self.reversing = None;
                                self.flash.set(
                                    StatusKind::Error,
                                    if message == DENIED {
                                        "You do not have permission to reverse receipts.".into()
                                    } else {
                                        message
                                    },
                                );
                            }
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
            self.load(r.scope).await?;
            if self.record.is_none() {
                self.flash
                    .set(StatusKind::Error, "That receipt is no longer available.");
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..LiveBody::default()
            })
        })
    }

    fn panels(&self) -> Vec<PanelSpec> {
        if self.record.is_none() {
            return vec![];
        }
        vec![PanelSpec {
            id: "receivables-receipt-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "receivables-receipt-attachments".into()),
                ("source".into(), "finance/receivables/payment".into()),
                ("document".into(), self.id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}
