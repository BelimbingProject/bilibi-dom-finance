//! One customer: its Finance facts edited in place, the Core company it is
//! (identity, status, tax and registration numbers, contact details and
//! billing address read from Core through its contracts), the customer
//! relationship, and the invoices and receipts that belong to it. Reads
//! `customer.get`, `document.list`, `payment.list`, `core.address.company.list`
//! and `core.company.relationship.list`; each edit calls `customer.update`;
//! every customer permanently names a Core Company or Person.
use super::{
    company_pick::{self, Relation},
    format::{self as f},
    render, Flash, Pages, Written, CUSTOMERS, INVOICES, RECEIPTS,
};
use crate::{Customer as Record, Document, PartyFacts, Payment, MANAGE_CUSTOMERS};
use bilibi_database::{Error, Result};
use bilibi_records::{Reference, Resolved};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, Button, Card, Cell, CommitStates, CommitStatus, EmptyState, Facts, Field,
    FlexRow, Header, Html, InlineChoice, InlineEdit, InputKind, ListColumns, PageVariant,
    Restricted, SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// `(key, label)` of the Finance facts a customer edits in place.
const OWN: &[(&str, &str)] = &[("payment_terms_days", "Payment terms (days)")];
/// What the page read from Core about the linked company.
#[derive(Default)]
struct PartyView {
    /// The reference as Base Records names it for this reader.
    resolved: Option<Resolved>,
    /// The company's current billing address, one line per row.
    billing: Vec<String>,
    /// Addresses are not readable for this person.
    addresses_hidden: bool,
    relation: Option<Relation>,
}

pub(super) struct Customer {
    pages: Pages,
    id: i64,
    record: Option<Record>,
    company: PartyView,
    company_name: String,
    documents: Vec<Document>,
    payments: Vec<Payment>,
    can_edit: bool,
    statuses: CommitStates,
    flash: Flash,
    figures: f::Figures,
    invoices: ListColumns,
    receipts: ListColumns,
    framed: bool,
}

impl Customer {
    pub(super) fn new(pages: Pages, path: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            record: None,
            company: PartyView::default(),
            company_name: String::new(),
            documents: vec![],
            payments: vec![],
            can_edit: false,
            statuses: CommitStates::default(),
            flash: Flash::default(),
            figures: f::Figures::default(),
            invoices: f::columns(
                "receivables-customer-invoices-table",
                &[
                    ("number", "Number"),
                    ("kind", "Type"),
                    ("issue_date", "Issued"),
                    ("due_date", "Due"),
                    ("total", "Total"),
                    ("outstanding", "Outstanding"),
                    ("status", "Status"),
                ],
                &["total", "outstanding"],
            )?,
            receipts: f::columns(
                "receivables-customer-receipts-table",
                &[
                    ("number", "Receipt"),
                    ("payment_date", "Date"),
                    ("amount", "Amount"),
                    ("status", "Status"),
                ],
                &["amount"],
            )?,
            framed,
        })
    }

    pub(super) fn title(&self) -> Option<String> {
        self.record.as_ref().map(|c| c.name.clone())
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.body()
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.record = match self
            .pages
            .read(
                scope,
                "finance.receivables.customer.get",
                json!({"customer_id": self.id}),
            )
            .await
        {
            Ok(value) => serde_json::from_value(value).ok(),
            Err(Error::NotFound) => None,
            Err(error) => return Err(error),
        };
        self.can_edit = self.pages.allowed(scope, MANAGE_CUSTOMERS).await?;
        self.figures = self.pages.figures(scope).await;
        self.company_name = self.pages.company_name(scope).await;
        self.company = PartyView::default();
        if let Some(company) = self.record.as_ref().map(|c| c.party.clone()) {
            self.company = self.read_company(scope, &company).await?;
        }
        if self.record.is_some() {
            self.documents = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.document.list",
                    json!({"customer_id": self.id}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.payments = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.payment.list",
                    json!({"customer_id": self.id}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.documents.reverse();
            self.payments.reverse();
            self.invoices.load(&self.documents, |d| d.id.to_string());
            self.receipts.load(&self.payments, |p| p.id.to_string());
        }
        Ok(())
    }

    /// What Core says about the linked company for this reader: its name
    /// and page (Base Records), its current billing address (Core Address)
    /// and whether the current company records the relationship.
    async fn read_company(&self, scope: &Scope, company: &PartyFacts) -> Result<PartyView> {
        let resolved = self
            .pages
            .records
            .resolve(
                scope,
                &[Reference::new(&company.kind, company.id.to_string())],
            )
            .await?
            .into_iter()
            .next();
        let today = self.pages.today(scope).await;
        let (billing, addresses_hidden) = match self
            .pages
            .read_core(
                scope,
                if company.kind == "person" {
                    "core.address.person.list"
                } else {
                    "core.address.company.list"
                },
                if company.kind == "person" {
                    json!({"person_id":company.id})
                } else {
                    json!({"company_id":company.id})
                },
            )
            .await
        {
            Ok(rows) => (billing_lines(&rows, &today), false),
            Err(Error::Forbidden | Error::Unauthorized) => (Vec::new(), true),
            Err(error) => return Err(error),
        };
        let relation =
            Some(company_pick::relation(&self.pages, scope, &company.kind, company.id).await?);
        Ok(PartyView {
            resolved,
            billing,
            addresses_hidden,
            relation,
        })
    }

    fn value(c: &Record, key: &str) -> String {
        match key {
            "payment_terms_days" => c.payment_terms_days.to_string(),
            _ => String::new(),
        }
    }

    fn facts(&self, c: &Record, id: &str, rows: &[(&'static str, &'static str)]) -> Result<Html> {
        let mut items = Vec::new();
        for (key, label) in rows {
            let mut field = f::inline(
                key,
                label,
                &Self::value(c, key),
                c.id,
                "save_customer",
                self.can_edit,
                &self.statuses,
            );
            if *key == "name" {
                field.allow_empty = false;
            }
            items.push(f::fact_html(label, render(InlineEdit { field })?));
        }
        render(Facts { id, items })
    }

    /// A fact of the company as the reader may see it: the value, a
    /// restricted marker.
    fn company_fact(
        &self,
        company: &PartyFacts,
        key: &str,
        value: &Option<String>,
    ) -> Result<Html> {
        if company.restricted.iter().any(|r| r == key) {
            return render(Restricted::new(format!(
                "receivables-customer-company-{key}-restricted"
            )));
        }
        Ok(Html::text(f::optional(value)))
    }

    fn company_card(&self, c: &Record, company: &PartyFacts) -> Result<Html> {
        let name = match &self.company.resolved {
            Some(resolved) if resolved.label.is_some() => {
                render(resolved.pill("receivables-customer-company-pill", self.framed))?
            }
            _ => Html::text(&company.name),
        };
        let mut items = vec![
            f::fact_html("Core party", name),
            f::fact_html("Core status", super::customers::company_badge(c)?),
            f::fact_html(
                "Tax ID",
                self.company_fact(company, "tax_id", &company.tax_id)?,
            ),
            f::fact_html(
                "Registration number",
                self.company_fact(company, "registration_number", &company.registration_number)?,
            ),
            f::fact_html(
                "Email",
                self.company_fact(company, "email", &company.email)?,
            ),
            f::fact_html(
                "Phone",
                self.company_fact(company, "phone", &company.phone)?,
            ),
        ];
        let billing = if self.company.addresses_hidden {
            render(
                Restricted::new("receivables-customer-billing-restricted")
                    .reason("Not visible to you: viewing addresses needs permission."),
            )?
        } else if self.company.billing.is_empty() {
            Html::text("No billing address in Core yet.")
        } else {
            render(Lines {
                id: "receivables-customer-billing",
                lines: self.company.billing.clone(),
            })?
        };
        items.push(f::fact_html("Billing address", billing));
        let relation = match &self.company.relation {
            Some(Relation::Recorded { since }) => Html::text(&match since {
                Some(day) => format!(
                    "Recorded as a customer of {} since {day}.",
                    self.company_name
                ),
                None => format!("Recorded as a customer of {}.", self.company_name),
            }),
            Some(Relation::Missing) => {
                let mut html = Html::text(&format!(
                    "Not recorded as a customer of {} in Companies.",
                    self.company_name
                ));
                if self.can_edit {
                    html.push(&render(Button {
                        id: "receivables-customer-record-relationship",
                        button_type: "button",
                        event: "record_relationship",
                        class: "ml-2",
                        working: "Recording…",
                        body: Html::text("Record relationship"),
                        ..Button::default()
                    })?);
                }
                html
            }
            Some(Relation::Hidden) | None => render(
                Restricted::new("receivables-customer-relationship-restricted")
                    .reason("Not visible to you: viewing this Core party needs permission."),
            )?,
        };
        items.push(f::fact_html("Relationship", relation));
        render(Card {
            id: "receivables-customer-company",
            role: "region",
            class: "mt-4",
            body: {
                let mut b = render(SectionHeading {
                    title: "Core party",
                    description: "Read from Core. Invoices issued from now on carry these details; invoices already issued keep the details they were issued with.",
                    ..SectionHeading::default()
                })?;
                b.push(&render(Facts {
                    id: "receivables-customer-company-facts",
                    items,
                })?);
                b
            },
            ..Card::default()
        })
    }

    fn body(&self) -> Result<String> {
        let back = render(BackLink {
            href: CUSTOMERS,
            id: "receivables-customer-back",
            title: "Back to customers",
            ..BackLink::default()
        })?;
        let Some(c) = &self.record else {
            let mut missing = EmptyState::missing(
                "Customer not found",
                "There is no such customer in the current company, or you cannot read it.",
            );
            missing.id = "receivables-customer-missing".into();
            missing.action = back;
            return f::frame(
                "receivables-customer",
                PageVariant::Detail,
                &self.flash,
                render(missing)?,
                None,
            );
        };
        let owed = self
            .documents
            .iter()
            .filter(|d| d.kind == "invoice" && d.status == "issued")
            .filter_map(|d| d.outstanding)
            .fold(rust_decimal::Decimal::ZERO, |sum, m| sum + m.amount());
        let currency = self
            .documents
            .first()
            .map_or("", |d| d.currency_code.as_str());
        let mut status_editor = Field::named("active")
            .id("receivables-customer-active-editor")
            .label_class("sr-only")
            .label("Status")
            .kind(InputKind::Select)
            .value(if c.active { "true" } else { "false" })
            .options(bilibi_ui::choices(&[
                ("Active", "true"),
                ("Inactive", "false"),
            ]));
        status_editor = status_editor.aria_label("Status");
        let mut status_field = f::inline(
            "active",
            "Status",
            if c.active { "true" } else { "false" },
            c.id,
            "save_customer",
            self.can_edit,
            &self.statuses,
        );
        status_field.allow_empty = false;
        let mut items = vec![
            f::fact("Code", &c.code),
            f::fact_html(
                "Status",
                render(InlineChoice {
                    field: status_field,
                    display: f::active_badge(c.active)?,
                    editor: render(status_editor)?,
                })?,
            ),
            f::fact(
                "Owed",
                &if owed.is_zero() {
                    "Nothing outstanding".into()
                } else {
                    format!("{} {currency}", self.figures.decimal(owed))
                },
            ),
        ];
        let mut summary = render(Facts {
            id: "receivables-customer-summary",
            items: std::mem::take(&mut items),
        })?;
        summary.push(&self.facts(c, "receivables-customer-details", OWN)?);
        let mut parts = vec![render(Card {
            id: "receivables-customer-identity",
            role: "region",
            body: {
                let mut b = render(SectionHeading {
                    title: "In Finance",
                    description:
                        "Inactive stops new invoices; receipts and credit notes stay possible.",
                    ..SectionHeading::default()
                })?;
                b.push(&summary);
                b
            },
            ..Card::default()
        })?];
        parts.push(self.company_card(c, &c.party)?);
        let mut invoice_rows = Vec::new();
        for d in &self.documents {
            let mut row = FlexRow::new(d.id.to_string());
            row.dom_id = Some(format!("receivables-customer-invoice-{}", d.id));
            invoice_rows.push(
                row.slot(
                    "number",
                    f::link(
                        &format!("{INVOICES}/{}", d.id),
                        &super::documents::reference(d),
                    )?,
                )
                .cell("kind", Cell::text(f::kind_label(&d.kind)))
                .cell("issue_date", Cell::text(f::optional(&d.issue_date)))
                .cell("due_date", Cell::text(f::optional(&d.due_date)))
                .slot("total", f::tabular(&self.figures.money(d.total))?)
                .slot(
                    "outstanding",
                    f::tabular(&self.figures.optional(d.outstanding))?,
                )
                .slot("status", f::status_badge(&d.status)?),
            );
        }
        let mut receipt_rows = Vec::new();
        for p in &self.payments {
            let mut row = FlexRow::new(p.id.to_string());
            row.dom_id = Some(format!("receivables-customer-receipt-{}", p.id));
            receipt_rows.push(
                row.slot(
                    "number",
                    f::link(&format!("{RECEIPTS}/{}", p.id), &p.payment_reference)?,
                )
                .cell("payment_date", Cell::text(&p.payment_date))
                .slot("amount", f::tabular(&self.figures.money(p.amount))?)
                .slot("status", super::payments::status_badge(p)?),
            );
        }
        parts.push(render(Card {
            id: "receivables-customer-invoices",
            role: "region",
            class: "mt-4",
            inner_class: "p-0",
            body: {
                let mut b = render(SectionHeading {
                    title: "Invoices and credit notes",
                    class: "p-5 pb-0",
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.invoices,
                    "invoices_grid",
                    "Invoices and credit notes",
                    invoice_rows,
                    "Nothing has been invoiced to this customer yet.",
                )?);
                b
            },
            ..Card::default()
        })?);
        parts.push(render(Card {
            id: "receivables-customer-receipts",
            role: "region",
            class: "mt-4",
            inner_class: "p-0",
            body: {
                let mut b = render(SectionHeading {
                    title: "Receipts",
                    class: "p-5 pb-0",
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.receipts,
                    "receipts_grid",
                    "Receipts",
                    receipt_rows,
                    "No receipts have been recorded for this customer.",
                )?);
                b
            },
            ..Card::default()
        })?);
        let mut body = f::announce(c.id)?;
        body.push(&render(Header {
            title: Html::text(&c.name),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle: &format!("Customer {}", c.code),
            actions: back,
        })?);
        for part in parts {
            body.push(&part);
        }
        f::frame(
            "receivables-customer",
            PageVariant::Detail,
            &self.flash,
            body,
            None,
        )
    }

    async fn save_field(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let key = v["field"].as_str().ok_or(Error::Invalid)?;
        let label = OWN
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, l)| *l)
            .or((key == "active").then_some("Status"))
            .ok_or(Error::Invalid)?;
        if v["id"].as_str() != Some(&self.id.to_string()) {
            return Err(Error::Invalid);
        }
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        let mut input = json!({"customer_id": self.id});
        match key {
            "active" => input["active"] = json!(submitted == "true"),
            "payment_terms_days" => match submitted.parse::<i64>() {
                Ok(days) => input[key] = json!(days),
                Err(_) => {
                    self.statuses.put(
                        key,
                        CommitStatus::refusal(
                            label,
                            &submitted,
                            &["must be a whole number of days"],
                        ),
                    );
                    return Ok(());
                }
            },
            "country_iso" => input[key] = json!(submitted.to_uppercase()),
            _ => input[key] = json!(submitted),
        }
        if !self.can_edit {
            self.statuses.clear_saved();
            self.statuses.put(
                key,
                CommitStatus::Error("You do not have permission to change customers.".into()),
            );
            return Ok(());
        }
        let written: Written = self
            .pages
            .write(
                scope,
                credential,
                "save_customer",
                "finance.receivables.customer.update",
                input,
            )
            .await?;
        self.statuses
            .put(key, f::commit_status(&written, key, label, &submitted));
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let pages = self.pages.clone();
        match v["type"].as_str().ok_or(Error::Invalid)? {
            "clear-flash" => self.flash.dismiss(v),
            "invoices_grid" => {
                self.invoices.handle(&params::string_values(v));
            }
            "receipts_grid" => {
                self.receipts.handle(&params::string_values(v));
            }
            "save_customer" => self.save_field(scope, credential, v).await?,
            "record_relationship" => {
                let target = self
                    .record
                    .as_ref()
                    .map(|c| (c.party.kind.clone(), c.party.id));
                match target {
                    Some((target_kind, target)) if self.can_edit => {
                        let recorded = company_pick::record_relationship(
                            &pages,
                            scope,
                            credential,
                            "record_relationship",
                            &target_kind,
                            target,
                        )
                        .await?;
                        let (kind, message) = match recorded {
                            Ok(()) => (
                                StatusKind::Success,
                                "The customer relationship is recorded in Companies.".to_owned(),
                            ),
                            Err(reason) => (
                                StatusKind::Error,
                                format!("The customer relationship was not recorded: {reason}."),
                            ),
                        };
                        self.flash.set(kind, message);
                    }
                    _ => self.flash.set(StatusKind::Error, super::DENIED),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

use bilibi_finance_ledger::party_billing_lines as billing_lines;

#[derive(askama::Template)]
#[template(
    source = r#"<div id="{{ id }}">{% for line in lines %}<div>{{ line }}</div>{% endfor %}</div>"#,
    ext = "html"
)]
struct Lines {
    id: &'static str,
    lines: Vec<String>,
}

impl LiveState<Scope, Error> for Customer {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await?,
            }
            self.load(r.scope).await?;
            if self.record.is_none() {
                self.flash
                    .set(StatusKind::Error, "That customer is no longer available.");
            }
            Ok(LiveBody {
                html: self.body()?,
                ..LiveBody::default()
            })
        })
    }
}
