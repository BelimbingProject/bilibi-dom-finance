//! One invoice or credit note: header, party, lines, tax and posting status,
//! the receipts and credits applied to it, and its source documents. A draft
//! edits in place (dates and description) and its lines open in a dialog; it
//! is issued or discarded after a confirmation. Reads `document.get`; writes
//! call `draft.update`, `draft.discard` and `document.issue`.
use super::{
    documents::reference,
    format::{self as f, Errors},
    render, Account, Flash, Pages, Written, CUSTOMERS, INVOICES, RECEIPTS,
};
use crate::{Customer, Document as Record, Payment, Series, TaxCode, ISSUE, MANAGE_DOCUMENTS};
use bilibi_database::{Error, Result};
use bilibi_datetime::DisplayContext;
use bilibi_finance_ledger::{Money, PartyReview};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    params, BackLink, Button, ButtonVariant, Card, Cell, Choice, CommitStates, CommitStatus,
    ConfirmDialog, EmptyState, Facts, Field, FlexRow, Form, Header, Html, InlineEdit,
    InlineLongText, InputKind, ListColumns, ModalWidth, PageVariant, SectionHeading, StatusKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

enum Dialog {
    Issue,
    Discard,
}

/// One line of the lines dialog, as typed.
#[derive(Clone, Default)]
struct Line {
    description: String,
    quantity: String,
    unit_price: String,
    account_id: String,
    tax_code_id: String,
}

struct Lines {
    rows: Vec<Line>,
    errors: Errors,
}

pub(super) struct Document {
    pages: Pages,
    id: i64,
    balance_input: Value,
    balance_pins: Value,
    record: Option<Record>,
    customer: Option<Customer>,
    series: Option<Series>,
    accounts: Vec<Account>,
    tax_codes: Vec<TaxCode>,
    /// Receipts that settle this invoice, and credit notes that credit it.
    receipts: Vec<Payment>,
    credits: Vec<Record>,
    figures: f::Figures,
    can_manage: bool,
    can_issue: bool,
    statuses: CommitStates,
    flash: Flash,
    dialog: Option<Dialog>,
    issue_review: Option<Value>,
    issue_banner: Html,
    editing: Option<Lines>,
    lines_cols: ListColumns,
    tax_cols: ListColumns,
    applied_cols: ListColumns,
    write_offs: super::write_offs::Journals,
    credit_actions: super::credits::Credits,
}

impl Document {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            write_offs: super::write_offs::Journals::new()?,
            credit_actions: super::credits::Credits::new()?,
            pages,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            balance_input: super::credits::balance_query(query)?,
            balance_pins: Value::Null,
            record: None,
            customer: None,
            series: None,
            accounts: vec![],
            tax_codes: vec![],
            receipts: vec![],
            credits: vec![],
            figures: f::Figures::default(),
            can_manage: false,
            can_issue: false,
            statuses: CommitStates::default(),
            flash: Flash::default(),
            dialog: None,
            issue_review: None,
            issue_banner: Html::default(),
            editing: None,
            lines_cols: f::columns(
                "receivables-invoice-lines-table",
                &[
                    ("number", "#"),
                    ("description", "Description"),
                    ("quantity", "Quantity"),
                    ("unit_price", "Unit price"),
                    ("account", "Account"),
                    ("tax", "Tax code"),
                    ("tax_amount", "Tax"),
                    ("amount", "Amount"),
                ],
                &["quantity", "unit_price", "tax_amount", "amount"],
            )?,
            tax_cols: f::columns(
                "receivables-invoice-tax-table",
                &[
                    ("code", "Tax code"),
                    ("rate", "Rate"),
                    ("taxable", "Taxable amount"),
                    ("tax", "Tax"),
                ],
                &["rate", "taxable", "tax"],
            )?,
            applied_cols: f::columns(
                "receivables-invoice-applied-table",
                &[
                    ("document", "Document"),
                    ("date", "Date"),
                    ("amount", "Amount"),
                    ("status", "Status"),
                ],
                &["amount"],
            )?,
        })
    }

    pub(super) fn title(&self) -> Option<String> {
        self.record
            .as_ref()
            .map(|d| format!("{} {}", f::kind_label(&d.kind), reference(d)))
    }

    pub(super) async fn first(&mut self, scope: &Scope) -> Result<String> {
        self.load(scope).await?;
        self.body(scope).await
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.receipts.clear();
        self.credits.clear();
        let mut input = self.balance_input.clone();
        input["document_id"] = json!(self.id);
        self.record = match self
            .pages
            .read(scope, "finance.receivables.document.get", input)
            .await
        {
            Ok(mut value) => {
                self.balance_pins = value["balance_pins"].clone();
                if self.balance_pins.is_object() {
                    self.balance_input = self.balance_pins.clone();
                    self.receipts = serde_json::from_value(value["receipts"].take())
                        .map_err(|_| Error::Internal)?;
                    self.credits = serde_json::from_value(value["credits"].take())
                        .map_err(|_| Error::Internal)?;
                }
                serde_json::from_value(value).ok()
            }
            Err(Error::NotFound) => None,
            Err(error) => return Err(error),
        };
        let historical = self.balance_pins.is_object();
        if historical {
            self.write_offs = super::write_offs::Journals::new()?;
            self.credit_actions = super::credits::Credits::new()?;
            self.can_manage = false;
            self.can_issue = false;
        } else {
            self.write_offs.load(&self.pages, scope).await?;
            self.can_manage = self.pages.allowed(scope, MANAGE_DOCUMENTS).await?;
            self.can_issue = self.pages.allowed(scope, ISSUE).await?;
        }
        let Some(d) = &self.record else { return Ok(()) };
        if !historical {
            self.credit_actions
                .load(&self.pages, scope, d.id, &d.kind)
                .await?;
        }
        let customer_id = d.customer_id;
        let series_id = d.series_id;
        let issued = d.status == "issued";
        self.figures = self.pages.figures(scope).await;
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
        let series: Vec<Series> = self
            .pages
            .read_all(
                scope,
                "finance.receivables.series.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        self.series = series.into_iter().find(|s| s.id == series_id);
        self.tax_codes = self
            .pages
            .read_all(
                scope,
                "finance.receivables.tax_code.list",
                json!({}),
                ("after_code", "next_after_code"),
            )
            .await?
            .0;
        self.accounts = self.pages.accounts(scope).await;
        if issued && !historical {
            let id = self.id;
            let payments: Vec<Payment> = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.payment.list",
                    json!({"customer_id": customer_id}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.receipts = payments
                .into_iter()
                .filter(|p| p.allocations.iter().any(|a| a.document_id == id))
                .collect();
            let documents: Vec<Record> = self
                .pages
                .read_all(
                    scope,
                    "finance.receivables.document.list",
                    json!({"customer_id": customer_id, "kind": "credit_note", "status": "issued"}),
                    ("after", "next_after"),
                )
                .await?
                .0;
            self.credits = documents
                .into_iter()
                .filter(|c| c.credited_document_id == Some(id))
                .collect();
        }
        Ok(())
    }

    fn draft(&self) -> bool {
        self.record.as_ref().is_some_and(|d| d.status == "draft")
    }

    fn editable(&self) -> bool {
        self.draft() && self.can_manage
    }

    fn account(&self, id: i64) -> String {
        super::account_label(&self.accounts, id)
    }

    fn tax_code(&self, id: Option<i64>) -> String {
        id.map_or_else(
            || "—".into(),
            |id| {
                self.tax_codes
                    .iter()
                    .find(|t| t.id == id)
                    .map_or_else(|| format!("Tax code #{id}"), |t| t.code.clone())
            },
        )
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let back = render(BackLink {
            href: INVOICES,
            id: "receivables-invoice-back",
            title: "Back to invoices",
            ..BackLink::default()
        })?;
        let Some(d) = self.record.clone() else {
            let mut missing = EmptyState::missing(
                "Document not found",
                "There is no such invoice or credit note in this company, or you cannot read it.",
            );
            missing.id = "receivables-invoice-missing".into();
            missing.action = back;
            return f::frame(
                "receivables-invoice",
                PageVariant::Detail,
                &self.flash,
                render(missing)?,
                None,
            );
        };
        let display = self.pages.display(scope).await?;
        let kind = f::kind_label(&d.kind);
        let issued = d.status == "issued";
        let customer_name = d
            .party_name
            .clone()
            .or_else(|| self.customer.as_ref().map(|c| c.name.clone()))
            .unwrap_or_default();

        // Header actions: navigation first, then what changes the document.
        let mut actions = back;
        if issued && d.opening.is_none() && matches!(d.kind.as_str(), "invoice" | "credit_note") {
            actions.push(&render(Button {
                id: "receivables-invoice-print",
                href: &format!("{INVOICES}/{}/pdf", d.id),
                body: Html::text("Print / download PDF"),
                ..Button::default()
            })?);
        }
        if self.draft() {
            if self.can_manage {
                actions.push(&render(Button {
                    id: "receivables-invoice-edit-lines",
                    button_type: "button",
                    event: "edit_lines",
                    body: Html::text("Edit lines"),
                    ..Button::default()
                })?);
                actions.push(&render(Button {
                    id: "receivables-invoice-discard",
                    button_type: "button",
                    event: "ask_discard",
                    variant: ButtonVariant::Danger,
                    body: Html::text("Discard draft"),
                    ..Button::default()
                })?);
            }
            if self.can_issue {
                actions.push(&render(Button {
                    id: "receivables-invoice-issue",
                    button_type: "button",
                    event: "ask_issue",
                    variant: ButtonVariant::Primary,
                    body: Html::text(&format!("Issue {}", kind.to_lowercase())),
                    ..Button::default()
                })?);
            }
        }

        let editable = self.editable();
        let text_fact =
            |key: &str, label: &'static str, value: &str| -> Result<bilibi_ui::Fact<'static>> {
                Ok(f::fact_html(
                    label,
                    render(InlineEdit {
                        field: f::inline(
                            key,
                            label,
                            value,
                            d.id,
                            "save_draft",
                            editable,
                            &self.statuses,
                        ),
                    })?,
                ))
            };
        let mut items = vec![
            f::fact_html(
                "Customer",
                f::link(&format!("{CUSTOMERS}/{}", d.customer_id), &customer_name)?,
            ),
            f::fact("Type", kind),
            f::fact_html("Status", f::status_badge(&d.status)?),
            f::fact(
                "Series",
                self.series.as_ref().map_or("—", |s| s.name.as_str()),
            ),
        ];
        if let Some(credited) = d.credited_document_id {
            items.push(f::fact_html(
                "Credits invoice",
                f::link(
                    &format!("{INVOICES}/{credited}"),
                    &format!("Invoice #{credited}"),
                )?,
            ));
        }
        items.push(text_fact(
            "issue_date",
            "Issue date",
            d.issue_date.as_deref().unwrap_or(""),
        )?);
        items.push(text_fact(
            "due_date",
            "Due date",
            if d.kind == "opening_credit" {
                d.opening
                    .as_ref()
                    .and_then(|v| v["due_date"].as_str())
                    .unwrap_or("—")
            } else {
                d.due_date.as_deref().unwrap_or("")
            },
        )?);
        if self.draft() {
            let mut field = f::inline(
                "description",
                "Description",
                d.description.as_deref().unwrap_or(""),
                d.id,
                "save_draft",
                editable,
                &self.statuses,
            );
            if editable {
                field.placeholder = "Add a description".into();
            }
            items.push(f::fact_html(
                "Description",
                render(InlineLongText { field, rows: 3 })?,
            ));
        } else {
            items.push(f::fact("Description", f::optional(&d.description)));
        }
        if issued && matches!(d.kind.as_str(), "invoice" | "opening_credit") {
            items.push(f::fact(
                if d.kind == "opening_credit" {
                    "Available credit"
                } else {
                    "Outstanding"
                },
                &self
                    .figures
                    .with_currency_opt(d.outstanding, &d.currency_code),
            ));
        }
        if let Some(pins) = self.balance_pins.as_object() {
            for (key, label) in [
                ("as_of", "Balance as of"),
                ("through_entry_number", "Read through ledger entry"),
                ("through_opening_batch_id", "Read through opening batch"),
                ("through_settlement_number", "Read through settlement"),
            ] {
                items.push(f::fact(
                    label,
                    &pins
                        .get(key)
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        })
                        .unwrap_or_default(),
                ));
            }
            items.push(f::fact(
                "Activity shown through",
                pins.get("as_of").and_then(Value::as_str).unwrap_or(""),
            ));
        }
        if let Some(order) = &d.sales_order {
            items.push(f::fact("Sales order", &order.order_number));
            if order.status == "cancelled" {
                items.push(f::fact("Order status", "Cancelled — review this invoice."));
            }
        }
        if let Some(opening) = &d.opening {
            items.push(f::fact(
                "Source item kind",
                f::opening_item_kind_label(opening["item_kind"].as_str().unwrap_or("")),
            ));
            items.push(f::fact(
                "Source kind",
                opening["source_kind"].as_str().unwrap_or(""),
            ));
            items.push(f::fact(
                "Source system",
                opening["source_system"].as_str().unwrap_or(""),
            ));
            items.push(f::fact(
                "Source tax facts",
                &opening["source_tax_facts"].to_string(),
            ));
            if let Some(batch) = opening["replaced_by_batch_id"].as_i64() {
                items.push(f::fact(
                    "Opening register",
                    &format!("Replaced by register {batch}; retained as source evidence."),
                ));
            }

            items.push(f::fact(
                "Opening source",
                opening["source_reference"].as_str().unwrap_or(""),
            ));
            items.push(f::fact(
                "Source document",
                opening["document_reference"].as_str().unwrap_or(""),
            ));
            items.push(f::fact(
                "Conversion date",
                opening["conversion_date"].as_str().unwrap_or(""),
            ));
            if let Some(value) = opening["original_total"]
                .as_str()
                .and_then(|v| v.parse::<rust_decimal::Decimal>().ok())
            {
                items.push(f::fact(
                    "Source original total",
                    &self.figures.decimal(value),
                ));
            }
            items.push(f::fact(
                "Opening amounts",
                "Residual only. Supplied source tax facts are retained as evidence and create no current tax lines.",
            ));
        }
        let document = render(Facts {
            id: "receivables-invoice-facts",
            items,
        })?;

        // Posting status.
        let mut posting = vec![];
        match d.status.as_str() {
            "issued" => {
                posting.push(f::fact("Reference", &reference(&d)));
                posting.push(f::fact("Ledger entry", &f::number(d.ledger_entry_number)));
                posting.push(f::fact(
                    "Receivable account",
                    &d.receivable_account_id
                        .map_or("—".into(), |id| self.account(id)),
                ));
                posting.push(f::fact_html(
                    "Issued",
                    actor_at(
                        &display,
                        d.issued_by_user_id,
                        d.issued_by_impersonator_id,
                        d.issued_at.as_deref(),
                        "receivables-invoice-issued-at",
                    )?,
                ));
            }
            "discarded" => {
                posting.push(f::fact(
                    "Posting",
                    "Never posted. A discarded draft takes no number.",
                ));
                posting.push(f::fact_html(
                    "Discarded",
                    actor_at(
                        &display,
                        d.discarded_by_user_id,
                        None,
                        d.discarded_at.as_deref(),
                        "receivables-invoice-discarded-at",
                    )?,
                ));
            }
            _ => posting.push(f::fact(
                "Posting",
                "Not posted yet. Issuing gives it a number and posts it to the ledger.",
            )),
        }
        let posting = render(Facts {
            id: "receivables-invoice-posting",
            items: posting,
        })?;

        // Party as it will be, or as it was frozen at issue.
        let party = if issued {
            let mut lines: Vec<String> = [&d.party_line1, &d.party_line2, &d.party_line3]
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            let place = [
                &d.party_postcode,
                &d.party_locality,
                &d.party_admin1_code,
                &d.party_country_iso,
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
            if !place.is_empty() {
                lines.push(place);
            }
            Some(render(Facts {
                id: "receivables-invoice-party",
                items: vec![
                    f::fact("Core code", f::optional(&d.party_code)),
                    f::fact("Name", f::optional(&d.party_name)),
                    f::fact("Tax ID", f::optional(&d.party_tax_identifier)),
                    f::fact(
                        "Registration number",
                        f::optional(&d.party_registration_identifier),
                    ),
                    f::fact("Email", f::optional(&d.party_email)),
                    f::fact("Phone", f::optional(&d.party_phone)),
                    f::fact(
                        "Address",
                        &if lines.is_empty() {
                            "—".into()
                        } else {
                            lines.join(", ")
                        },
                    ),
                ],
            })?)
        } else {
            None
        };

        // Lines.
        self.lines_cols
            .load(&d.lines, |l| l.line_number.to_string());
        self.tax_cols
            .load(&d.tax_lines, |t| t.tax_code_id.to_string());
        let mut line_rows = Vec::new();
        for l in &d.lines {
            let mut row = FlexRow::new(l.line_number.to_string());
            row.dom_id = Some(format!("receivables-invoice-line-{}", l.line_number));
            line_rows.push(
                row.cell("number", Cell::text(l.line_number.to_string()))
                    .cell("description", Cell::text(&l.description))
                    .slot("quantity", f::tabular(&f::decimal(l.quantity))?)
                    .slot(
                        "unit_price",
                        f::tabular(&Money::display_decimal(l.unit_price, None))?,
                    )
                    .cell("account", Cell::text(self.account(l.account_id)))
                    .cell(
                        "tax",
                        Cell::text(match (l.tax_code_id, l.tax_rate) {
                            (Some(id), Some(rate)) => {
                                format!("{} ({}%)", self.tax_code(Some(id)), f::decimal(rate))
                            }
                            (Some(id), None) => self.tax_code(Some(id)),
                            _ => "—".into(),
                        }),
                    )
                    .slot("tax_amount", f::tabular(&self.figures.money(l.tax_amount))?)
                    .slot("amount", f::tabular(&self.figures.money(l.amount))?),
            );
        }
        let mut tax_rows = Vec::new();
        for t in &d.tax_lines {
            let mut row = FlexRow::new(t.tax_code_id.to_string());
            row.dom_id = Some(format!("receivables-invoice-tax-{}", t.tax_code_id));
            tax_rows.push(
                row.cell("code", Cell::text(&t.tax_code))
                    .slot("rate", f::tabular(&format!("{}%", f::decimal(t.tax_rate)))?)
                    .slot(
                        "taxable",
                        f::tabular(&self.figures.money(t.taxable_amount))?,
                    )
                    .slot("tax", f::tabular(&self.figures.money(t.tax_amount))?),
            );
        }
        let totals = render(Facts {
            id: "receivables-invoice-totals",
            items: vec![
                f::fact(
                    "Subtotal",
                    &self.figures.with_currency(d.subtotal, &d.currency_code),
                ),
                f::fact(
                    "Tax",
                    &self.figures.with_currency(d.tax_total, &d.currency_code),
                ),
                f::fact(
                    "Total",
                    &self.figures.with_currency(d.total, &d.currency_code),
                ),
            ],
        })?;
        let mut lines_body = render(SectionHeading {
            title: "Lines",
            id: "receivables-invoice-lines-heading",
            class: "p-5 pb-0",
            ..SectionHeading::default()
        })?;
        lines_body.push(&f::table(
            &self.lines_cols,
            "lines_grid",
            "Lines",
            line_rows,
            if self.draft() {
                "No lines yet. Use Edit lines to add what you are invoicing."
            } else {
                "This document has no lines."
            },
        )?);
        if !d.tax_lines.is_empty() {
            lines_body.push(&render(SectionHeading {
                title: "Tax summary",
                class: "p-5 pb-0",
                ..SectionHeading::default()
            })?);
            lines_body.push(&f::table(
                &self.tax_cols,
                "tax_grid",
                "Tax summary",
                tax_rows,
                "",
            )?);
        }
        let lines = render(Card {
            id: "receivables-invoice-lines",
            role: "region",
            labelledby: "receivables-invoice-lines-heading",
            class: "mt-4",
            inner_class: "p-0",
            body: lines_body,
            ..Card::default()
        })?;
        let totals = render(Card {
            id: "receivables-invoice-totals-region",
            role: "region",
            class: "mt-4",
            body: {
                let mut b = render(SectionHeading {
                    title: "Totals",
                    ..SectionHeading::default()
                })?;
                b.push(&totals);
                b
            },
            ..Card::default()
        })?;

        // What has been applied to an issued invoice.
        let mut applied = Vec::new();
        for p in &self.receipts {
            let allocated = p
                .allocations
                .iter()
                .filter(|a| a.document_id == d.id)
                .map(|a| a.amount.amount())
                .sum::<rust_decimal::Decimal>();
            let mut row = FlexRow::new(format!("p{}", p.id));
            row.dom_id = Some(format!("receivables-invoice-receipt-{}", p.id));
            applied.push(
                row.slot(
                    "document",
                    f::link(&format!("{RECEIPTS}/{}", p.id), &p.payment_reference)?,
                )
                .cell("date", Cell::text(&p.payment_date))
                .slot("amount", f::tabular(&self.figures.decimal(allocated))?)
                .slot("status", super::payments::status_badge(p)?),
            );
        }
        for c in &self.credits {
            let mut row = FlexRow::new(format!("c{}", c.id));
            row.dom_id = Some(format!("receivables-invoice-credit-{}", c.id));
            applied.push(
                row.slot(
                    "document",
                    f::link(&format!("{INVOICES}/{}", c.id), &reference(c))?,
                )
                .cell("date", Cell::text(f::optional(&c.issue_date)))
                .slot("amount", f::tabular(&self.figures.money(c.total))?)
                .cell("status", Cell::text("Credit note")),
            );
        }
        let applied = (issued && d.kind == "invoice")
            .then(|| -> Result<Html> {
                let mut b = render(SectionHeading {
                    title: "Receipts and credits",
                    class: "p-5 pb-0",
                    ..SectionHeading::default()
                })?;
                b.push(&f::table(
                    &self.applied_cols,
                    "applied_grid",
                    "Receipts and credits",
                    applied,
                    "Nothing has been received or credited against this invoice yet.",
                )?);
                render(Card {
                    id: "receivables-invoice-applied",
                    role: "region",
                    class: "mt-4",
                    inner_class: "p-0",
                    body: b,
                    ..Card::default()
                })
            })
            .transpose()?;

        let mut body = f::announce(d.id)?;
        let pin_name = reference(&d);
        body.push(&render(Header {
            title: Html::text(&pin_name),
            title_actions: bilibi_ui::Html::EMPTY,
            subtitle: &format!("{kind} for {customer_name}"),
            actions,
        })?);
        let mut top = render(Card {
            id: "receivables-invoice-document",
            role: "region",
            body: {
                let mut b = render(SectionHeading {
                    title: "Document",
                    ..SectionHeading::default()
                })?;
                b.push(&document);
                b
            },
            ..Card::default()
        })?;
        top.push(&render(Card {
            id: "receivables-invoice-posting-card",
            role: "region",
            class: "mt-4",
            body: {
                let mut b = render(SectionHeading {
                    title: "Posting",
                    ..SectionHeading::default()
                })?;
                b.push(&posting);
                b
            },
            ..Card::default()
        })?);
        body.push(&top);
        if let Some(party) = party {
            body.push(&render(Card {
                id: "receivables-invoice-party-card",
                role: "region",
                class: "mt-4",
                body: {
                    let mut b = render(SectionHeading {
                        title: "Billed to",
                        description: "The customer's details as they were when this was issued.",
                        ..SectionHeading::default()
                    })?;
                    b.push(&party);
                    b
                },
                ..Card::default()
            })?);
        }
        body.push(&lines);
        body.push(&totals);
        if let Some(applied) = applied {
            body.push(&applied);
        }
        body.push(
            &self
                .write_offs
                .body(&d, &self.accounts, &self.figures, &display)?,
        );
        body.push(
            &self
                .credit_actions
                .body(
                    &self.pages,
                    d.id,
                    &d.kind,
                    &d.settlements,
                    self.figures.units(),
                )
                .await?,
        );
        if !self.balance_pins.is_object() {
            body.push(&attachments_slot()?);
        }

        let modal = if let Some(dialog) =
            self.write_offs
                .dialog(&self.accounts, &self.flash, &self.figures)?
        {
            Some(dialog)
        } else if let Some(lines) = &self.editing {
            Some(self.lines_dialog(lines)?)
        } else {
            match self.dialog {
                Some(Dialog::Issue) => {
                    let party = self
                        .issue_review
                        .as_ref()
                        .map(|review| &review["changes"]["after"]);
                    let items = [
                        ("party_code", "Core code"),
                        ("party_name", "Name"),
                        ("party_tax_identifier", "Tax ID"),
                        ("party_registration_identifier", "Registration number"),
                        ("party_email", "Email"),
                        ("party_phone", "Phone"),
                        ("party_line1", "Address line 1"),
                        ("party_line2", "Address line 2"),
                        ("party_line3", "Address line 3"),
                        ("party_locality", "City"),
                        ("party_postcode", "Postcode"),
                        ("party_admin1_code", "State or region"),
                        ("party_country_iso", "Country code"),
                    ]
                    .into_iter()
                    .map(|(field, label)| {
                        f::fact(label, party.and_then(|p| p[field].as_str()).unwrap_or("—"))
                    })
                    .collect();
                    Some(render(IssueConfirmation {
                        consequence: format!("Issue this {}?", kind.to_lowercase()),
                        detail: format!(
                            "It is numbered and posted for {}. These customer details will be kept on the document. After issue, it cannot be edited.",
                            self.figures.with_currency(d.total, &d.currency_code)
                        ),
                        body: {
                            let mut body = self.issue_banner.clone();
                            body.push(&render(Facts {
                                id: "receivables-invoice-party-review",
                                items,
                            })?);
                            body
                        },
                    })?)
                }
                Some(Dialog::Discard) => Some(render(ConfirmDialog {
                    id: "receivables-invoice-discard-confirm",
                    consequence: "Discard this draft?",
                    detail: "The draft and its lines are kept on record as discarded. It takes no number and posts nothing. This cannot be undone.",
                    confirm: "Discard",
                    working: "Discarding…",
                    confirm_event: "discard",
                    cancel_event: "cancel_dialog",
                })?),
                None => None,
            }
        };
        f::frame(
            "receivables-invoice",
            PageVariant::Detail,
            &self.flash,
            body,
            modal,
        )
    }

    fn lines_dialog(&self, lines: &Lines) -> Result<Html> {
        let mut labels = vec![];
        for i in 0..lines.rows.len() {
            for (key, label) in [
                ("description", "Description"),
                ("quantity", "Quantity"),
                ("unit_price", "Unit price"),
                ("account_id", "Account"),
                ("tax_code_id", "Tax code"),
            ] {
                labels.push((
                    format!("lines.{i}.{key}"),
                    format!("receivables-line-{key}-{i}"),
                    format!("Line {} {}", i + 1, label.to_lowercase()),
                ));
            }
        }
        let mut body = render(lines.errors.summary("receivables-lines-errors", &labels))?;
        let accounts: Vec<Choice> = self
            .accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == "income")
            .map(|a| Choice::new(a.label(), a.id.to_string()))
            .collect();
        let taxes: Vec<Choice> = self
            .tax_codes
            .iter()
            .filter(|t| t.active)
            .map(|t| {
                Choice::new(
                    format!("{} ({}%)", t.code, f::decimal(t.rate)),
                    t.id.to_string(),
                )
            })
            .collect();
        for (i, line) in lines.rows.iter().enumerate() {
            let err =
                |key: &str, label: &str| lines.errors.under(&format!("lines.{i}.{key}"), label);
            body.push(&render(SectionHeading {
                title: &format!("Line {}", i + 1),
                actions: render(Button {
                    id: &format!("receivables-line-remove-{i}"),
                    button_type: "button",
                    event: "remove_line",
                    name: "index",
                    value: &i.to_string(),
                    body: Html::text("Remove"),
                    ..Button::default()
                })?,
                ..SectionHeading::default()
            })?);
            body.push(&render(
                Field::in_form("line", &format!("description_{i}"))
                    .id(format!("receivables-line-description-{i}"))
                    .label("Description")
                    .attr("maxlength", "500")
                    .value(line.description.clone())
                    .errors(err("description", "Description")),
            )?);
            let mut row = Html::EMPTY;
            row.push(&render(
                Field::in_form("line", &format!("quantity_{i}"))
                    .id(format!("receivables-line-quantity-{i}"))
                    .label("Quantity")
                    .attr("inputmode", "decimal")
                    .value(line.quantity.clone())
                    .errors(err("quantity", "Quantity")),
            )?);
            row.push(&render(
                Field::in_form("line", &format!("unit_price_{i}"))
                    .id(format!("receivables-line-unit_price-{i}"))
                    .label("Unit price")
                    .attr("inputmode", "decimal")
                    .value(line.unit_price.clone())
                    .errors(err("unit_price", "Unit price")),
            )?);
            row.push(&render(
                Field::in_form("line", &format!("account_id_{i}"))
                    .id(format!("receivables-line-account_id-{i}"))
                    .label("Account")
                    .kind(InputKind::Select)
                    .prompt("Company default")
                    .options(accounts.clone())
                    .value(line.account_id.clone())
                    .errors(err("account_id", "Account")),
            )?);
            row.push(&render(
                Field::in_form("line", &format!("tax_code_id_{i}"))
                    .id(format!("receivables-line-tax_code_id-{i}"))
                    .label("Tax code")
                    .kind(InputKind::Select)
                    .prompt("No tax")
                    .options(taxes.clone())
                    .value(line.tax_code_id.clone())
                    .errors(err("tax_code_id", "Tax code")),
            )?);
            body.push(&f::four(row)?);
        }
        let form = render(Form {
            id: "receivables-lines-form",
            event: "lines_change",
            change: true,
            class: "mt-4",
            body,
            actions: Html::EMPTY,
        })?;
        let mut buttons = render(Button {
            id: "receivables-lines-add",
            button_type: "button",
            event: "add_line",
            body: Html::text("Add line"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "receivables-lines-cancel",
            button_type: "button",
            event: "cancel_lines",
            class: "ml-auto",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        buttons.push(&render(Button {
            id: "receivables-lines-save",
            button_type: "button",
            event: "save_lines",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save lines"),
            ..Button::default()
        })?);
        let mut inner = form;
        inner.push(&f::buttons(buttons)?);
        f::modal(
            "receivables-lines",
            "Edit lines",
            "Each line is quantity times unit price, rounded to the book's smallest unit. Saving replaces all the lines.",
            "cancel_lines",
            ModalWidth::Wide,
            &self.flash,
            inner,
        )
    }

    /// Fold the dialog's typed values into the draft lines.
    fn take_lines(&mut self, v: &Value) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let form = params::form_values(v, "line");
        for (i, line) in editing.rows.iter_mut().enumerate() {
            let get = |key: &str| form.get(&format!("{key}_{i}")).cloned();
            if let Some(t) = get("description") {
                line.description = t
            }
            if let Some(t) = get("quantity") {
                line.quantity = t
            }
            if let Some(t) = get("unit_price") {
                line.unit_price = t
            }
            if let Some(t) = get("account_id") {
                line.account_id = t
            }
            if let Some(t) = get("tax_code_id") {
                line.tax_code_id = t
            }
        }
    }

    fn lines_input(&self) -> Value {
        let rows = self
            .editing
            .as_ref()
            .map(|e| e.rows.as_slice())
            .unwrap_or(&[]);
        let lines: Vec<Value> = rows
            .iter()
            .map(|l| {
                let mut line = json!({
                    "description": l.description.trim(),
                    "quantity": l.quantity.trim(),
                    "unit_price": l.unit_price.trim(),
                });
                if let Ok(id) = l.account_id.parse::<i64>() {
                    line["account_id"] = json!(id)
                }
                if let Ok(id) = l.tax_code_id.parse::<i64>() {
                    line["tax_code_id"] = json!(id)
                }
                line
            })
            .collect();
        json!({"document_id": self.id, "lines": lines})
    }

    async fn save_field(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let key = v["field"].as_str().ok_or(Error::Invalid)?;
        let label = match key {
            "issue_date" => "Issue date",
            "due_date" => "Due date",
            "description" => "Description",
            _ => return Err(Error::Invalid),
        };
        if v["id"].as_str() != Some(&self.id.to_string()) {
            return Err(Error::Invalid);
        }
        let submitted = v["value"].as_str().ok_or(Error::Invalid)?.trim().to_owned();
        if !self.editable() {
            self.statuses.put(
                key,
                CommitStatus::Error("This document can no longer be changed.".into()),
            );
            return Ok(());
        }
        let written = self
            .pages
            .write(
                scope,
                credential,
                "save_draft",
                "finance.receivables.draft.update",
                json!({"document_id": self.id, key: submitted}),
            )
            .await?;
        self.statuses
            .put(key, f::commit_status(&written, key, label, &submitted));
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, v: &Value) -> Result<()> {
        let event = v["type"].as_str().ok_or(Error::Invalid)?;
        if self.balance_pins.is_object()
            && !matches!(
                event,
                "clear-flash"
                    | "lines_grid"
                    | "tax_grid"
                    | "applied_grid"
                    | "write_off_grid"
                    | "credit_grid"
            )
        {
            return Err(Error::Invalid);
        }
        match event {
            "ask_credit_apply" | "ask_credit_refund" | "ask_credit_reverse" | "cancel_credit"
            | "change_credit" | "preview_credit" | "commit_credit" | "credit_grid" => {
                self.credit_actions
                    .event(&self.pages, scope, credential, v, self.id)
                    .await?;
            }

            "ask_submit_write_off"
            | "ask_approve_write_off"
            | "ask_reject_write_off"
            | "ask_reverse_write_off"
            | "save_write_off"
            | "cancel_write_off"
            | "write_off_grid" => {
                self.write_offs
                    .event(
                        &self.pages,
                        scope,
                        credential,
                        v,
                        self.record.as_ref(),
                        &mut self.flash,
                    )
                    .await?
            }
            "clear-flash" => self.flash.dismiss(v),
            "lines_grid" => {
                self.lines_cols.handle(&params::string_values(v));
            }
            "tax_grid" => {
                self.tax_cols.handle(&params::string_values(v));
            }
            "applied_grid" => {
                self.applied_cols.handle(&params::string_values(v));
            }
            "save_draft" => self.save_field(scope, credential, v).await?,
            "cancel_dialog" => {
                self.dialog = None;
                self.issue_review = None;
            }
            "ask_issue" => {
                if self.can_issue && self.draft() {
                    let mut input = json!({"document_id":self.id});
                    input["company_id"] = json!(Pages::company(scope));
                    match self
                        .pages
                        .party_picker()
                        .review_core(
                            scope,
                            credential,
                            "ask_issue",
                            "finance.receivables.document.issue",
                            input,
                        )
                        .await?
                    {
                        PartyReview::Valid { review } => {
                            self.issue_banner = self
                                .pages
                                .plans
                                .instance()
                                .await?
                                .banner("receivables-issue-instance", "mb-4")
                                .map_err(|_| Error::Internal)?;
                            self.issue_review = Some(review);
                            self.dialog = Some(Dialog::Issue);
                        }
                        PartyReview::Invalid(fields) => {
                            self.flash.set(StatusKind::Error, f::sentences(&fields))
                        }
                        PartyReview::Refused(message) => self.flash.set(StatusKind::Error, message),
                    }
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to issue documents.",
                    );
                }
            }
            "ask_discard" => {
                if self.editable() {
                    self.dialog = Some(Dialog::Discard);
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to discard drafts.",
                    );
                }
            }
            "issue" | "discard" => {
                let issue = v["type"] == "issue";
                self.dialog = None;
                let (event, contract) = if issue {
                    ("issue", "finance.receivables.document.issue")
                } else {
                    ("discard", "finance.receivables.draft.discard")
                };
                let written = if issue {
                    let Some(_) = self.issue_review.take() else {
                        self.flash
                            .set(StatusKind::Error, "Review this invoice before issuing it.");
                        return Ok(());
                    };
                    let input = json!({"company_id":Pages::company(scope),"document_id":self.id});
                    let answer = self
                        .pages
                        .contracts
                        .commit(&Pages::call(scope, credential, event, contract), input)
                        .await
                        .map(|mut value| value["results"][0]["result"].take());
                    Pages::written(contract, event, answer)?
                } else {
                    self.pages
                        .write(
                            scope,
                            credential,
                            event,
                            contract,
                            json!({"document_id":self.id}),
                        )
                        .await?
                };
                match written {
                    Written::Done(record) => {
                        self.flash.set(
                            StatusKind::Success,
                            if issue {
                                match record["document_reference"].as_str() {
                                    Some(r) => format!("Issued as {r} and posted to the ledger."),
                                    None => "Issued and posted to the ledger.".into(),
                                }
                            } else {
                                "Draft discarded.".to_owned()
                            },
                        );
                    }
                    Written::Invalid(fields) => {
                        let message = f::sentences(&fields);
                        self.flash.set(
                            StatusKind::Error,
                            if message.is_empty() {
                                "That could not be done.".into()
                            } else {
                                message
                            },
                        );
                    }
                    Written::Refused(message) => self.flash.set(
                        StatusKind::Error,
                        super::worded(
                            message,
                            if issue {
                                "You do not have permission to issue documents."
                            } else {
                                "You do not have permission to discard drafts."
                            },
                        ),
                    ),
                }
            }
            "edit_lines" => {
                if let Some(d) = self.record.as_ref().filter(|_| self.editable()) {
                    let mut rows: Vec<Line> = d
                        .lines
                        .iter()
                        .map(|l| Line {
                            description: l.description.clone(),
                            quantity: f::decimal(l.quantity),
                            unit_price: f::price(l.unit_price),
                            account_id: l.account_id.to_string(),
                            tax_code_id: l.tax_code_id.map(|i| i.to_string()).unwrap_or_default(),
                        })
                        .collect();
                    if rows.is_empty() {
                        rows.push(Line::default());
                    }
                    self.editing = Some(Lines {
                        rows,
                        errors: Errors::default(),
                    });
                } else {
                    self.flash.set(
                        StatusKind::Error,
                        "You do not have permission to change this draft.",
                    );
                }
            }
            "lines_change" => self.take_lines(v),
            "add_line" => {
                if let Some(e) = &mut self.editing {
                    e.rows.push(Line::default());
                }
            }
            "remove_line" => {
                let index = v["index"].as_str().and_then(|i| i.parse::<usize>().ok());
                if let (Some(e), Some(i)) = (&mut self.editing, index) {
                    if i < e.rows.len() {
                        e.rows.remove(i);
                    }
                }
            }
            "cancel_lines" => self.editing = None,
            "save_lines" => {
                if self.editing.is_some() {
                    let input = self.lines_input();
                    match self
                        .pages
                        .write(
                            scope,
                            credential,
                            "save_lines",
                            "finance.receivables.draft.update",
                            input,
                        )
                        .await?
                    {
                        Written::Done(_) => {
                            self.editing = None;
                            self.flash.set(StatusKind::Success, "Lines saved.");
                        }
                        Written::Invalid(fields) => {
                            if let Some(e) = &mut self.editing {
                                e.errors = Errors(fields);
                            }
                        }
                        Written::Refused(message) => {
                            self.editing = None;
                            self.flash.set(
                                StatusKind::Error,
                                super::worded(
                                    message,
                                    "You do not have permission to change this draft.",
                                ),
                            );
                        }
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

fn actor_at(
    display: &DisplayContext,
    user: Option<i64>,
    impersonator: Option<i64>,
    at: Option<&str>,
    element: &str,
) -> Result<Html> {
    let mut value = Html::text(&f::actor(user, impersonator));
    if let Some(at) = at {
        value.push(&Html::text(" · "));
        value.push(&f::time(display, element, at)?);
    }
    Ok(value)
}

#[derive(askama::Template)]
#[template(
    source = r#"<div id="receivables-invoice-attachments-slot" class="mt-4" data-live-ignore><div id="receivables-invoice-attachments"></div></div>"#,
    ext = "html"
)]
struct AttachmentsSlot;

fn attachments_slot() -> Result<Html> {
    render(AttachmentsSlot)
}

impl LiveState<Scope, Error> for Document {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Event(v) => {
                    self.event(r.scope, r.credential, v).await?;
                }
                LiveInput::Params(query) => {
                    self.balance_input = super::credits::balance_query(query)?;
                    self.balance_pins = Value::Null;
                }
                LiveInput::Refresh => {}
            }
            self.load(r.scope).await?;
            if self.record.is_none() {
                self.flash
                    .set(StatusKind::Error, "That document is no longer available.");
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                query: super::credits::balance_params(&self.balance_pins),
                ..LiveBody::default()
            })
        })
    }

    fn panels(&self) -> Vec<PanelSpec> {
        if self.record.is_none() || self.balance_pins.is_object() {
            return vec![];
        }
        vec![PanelSpec {
            id: "receivables-invoice-attachments".into(),
            embed: bilibi_finance_attachments::PANEL_EMBED.into(),
            path: [
                ("panel".into(), "receivables-invoice-attachments".into()),
                ("source".into(), "finance/receivables/document".into()),
                ("document".into(), self.id.to_string()),
            ]
            .into(),
            query: HashMap::new(),
        }]
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% call ui::confirm_dialog(id="receivables-invoice-issue-confirm", consequence=consequence, detail=detail, confirm="Issue", working="Issuing…", confirm_event="issue", cancel_event="cancel_dialog", body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct IssueConfirmation {
    consequence: String,
    detail: String,
    body: Html,
}
