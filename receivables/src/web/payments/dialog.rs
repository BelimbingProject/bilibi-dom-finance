//! The Record receipt dialog: who paid, how much and where it went, an amount
//! for each open invoice, and what is left over to keep as the customer's
//! credit, said while the amounts are typed.
use super::{Draft, Payments};
use crate::web::{documents::reference, format as f, render, Account, SETTINGS};
use bilibi_database::Result;
use bilibi_ui::{
    Alert, Attrs, Button, ButtonVariant, Choice, Combobox, Field, Form, Html, InputKind,
    ModalWidth, SectionHeading, StatusKind,
};
use rust_decimal::Decimal;

#[derive(askama::Template)]
#[template(
    source = r#"<p id="{{ id }}" class="text-xs text-ink-muted">{{ text }}</p>"#,
    ext = "html"
)]
struct Note<'a> {
    id: &'a str,
    text: &'a str,
}

impl Payments {
    pub(super) fn dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let mut labels = vec![
            (
                "customer_id".to_owned(),
                "receivables-receipt-customer_id".to_owned(),
                "Customer".to_owned(),
            ),
            (
                "series_id".into(),
                "receivables-receipt-series_id".into(),
                "Series".into(),
            ),
            (
                "payment_date".into(),
                "receivables-receipt-payment_date".into(),
                "Date received".into(),
            ),
            (
                "amount".into(),
                "receivables-receipt-amount".into(),
                "Amount received".into(),
            ),
            (
                "deposit_account_id".into(),
                "receivables-receipt-deposit_account_id".into(),
                "Deposited to".into(),
            ),
        ];
        labels.push((
            "allocations".into(),
            "receivables-receipt-total".into(),
            "The amounts applied".into(),
        ));
        labels.push((
            "customer_credit_account_id".into(),
            "receivables-receipt-credit_account_id".into(),
            "Keep customer credit in".into(),
        ));
        for (i, d) in draft.open.iter().enumerate() {
            labels.push((
                format!("allocations.{i}.amount"),
                format!("receivables-receipt-alloc-{}", d.id),
                format!("Amount for {}", reference(d)),
            ));
        }
        let mut body = render(draft.errors.summary("receivables-receipt-errors", &labels))?;
        let customers: Vec<Choice> = self
            .customers
            .iter()
            .map(|c| Choice::new(&c.name, c.id.to_string()).detail(&c.code))
            .collect();
        let series: Vec<Choice> = self
            .series
            .iter()
            .filter(|s| s.kind == "payment" && s.active)
            .map(|s| Choice::new(format!("{} ({})", s.name, s.code), s.id.to_string()))
            .collect();
        let deposits: Vec<Choice> = self
            .accounts
            .iter()
            .filter(|a| a.postable() && a.account_type == "asset" && Some(a.id) != self.control)
            .map(Account::choice)
            .collect();
        let mut who = Html::EMPTY;
        who.push(&render(
            Combobox::new("receivables-receipt-customer_id", "receipt[customer_id]")
                .label("Customer *")
                .aria_label("Customer")
                .placeholder("Choose a customer")
                .options(customers)
                .value(value("customer_id"))
                .errors(draft.errors.under("customer_id", "Customer"))
                .attrs(Attrs {
                    required: true,
                    ..Attrs::default()
                }),
        )?);
        who.push(&render(
            Field::in_form("receipt", "series_id")
                .id("receivables-receipt-series_id")
                .label("Series")
                .kind(InputKind::Select)
                .prompt(if series.is_empty() {
                    "No receipt series yet — add one in Settings"
                } else {
                    "Choose a series"
                })
                .options(series)
                .value(value("series_id"))
                .required()
                .errors(draft.errors.under("series_id", "Series")),
        )?);
        body.push(&f::two(who)?);
        let mut money = Html::EMPTY;
        money.push(&render(
            Field::in_form("receipt", "payment_date")
                .id("receivables-receipt-payment_date")
                .label("Date received")
                .kind(InputKind::Date)
                .value(value("payment_date"))
                .required()
                .hint("A receipt cannot settle an invoice dated after it.")
                .errors(draft.errors.under("payment_date", "Date received")),
        )?);
        money.push(&render(
            Field::in_form("receipt", "amount")
                .id("receivables-receipt-amount")
                .label("Amount received")
                .attr("inputmode", "decimal")
                .value(value("amount"))
                .required()
                .errors(draft.errors.under("amount", "Amount received")),
        )?);
        body.push(&f::two(money)?);
        let mut deposit = Html::EMPTY;
        deposit.push(&render(
            Combobox::new(
                "receivables-receipt-deposit_account_id",
                "receipt[deposit_account_id]",
            )
            .label("Deposited to *")
            .aria_label("Deposited to")
            .placeholder("Choose the bank or cash account")
            .options(deposits)
            .value(value("deposit_account_id"))
            .errors(draft.errors.under("deposit_account_id", "Deposited to"))
            .attrs(Attrs {
                required: true,
                ..Attrs::default()
            }),
        )?);
        deposit.push(&render(
            Field::in_form("receipt", "reference")
                .id("receivables-receipt-reference")
                .label("Reference")
                .attr("maxlength", "100")
                .hint("A cheque or transfer number, if there is one.")
                .value(value("reference"))
                .errors(draft.errors.under("reference", "Reference")),
        )?);
        body.push(&f::two(deposit)?);
        body.push(&render(
            Field::in_form("receipt", "description")
                .id("receivables-receipt-description")
                .label("Description")
                .attr("maxlength", "500")
                .value(value("description"))
                .errors(draft.errors.under("description", "Description")),
        )?);
        body.push(&render(SectionHeading {
            title: "Apply to invoices",
            description: "Give each invoice this receipt pays an amount, up to what it still owes. Anything left over is kept as the customer's credit.",
            ..SectionHeading::default()
        })?);
        if value("customer_id").is_empty() {
            body.push(&render(Note {
                id: "receivables-receipt-pick",
                text: "Choose a customer to see what they owe.",
            })?);
        } else if draft.open.is_empty() {
            body.push(&render(Note {
                id: "receivables-receipt-none",
                text: "This customer has no unpaid invoice, so the whole amount is kept as their credit.",
            })?);
        }
        for d in &draft.open {
            body.push(&render(
                Field::in_form("receipt", &format!("alloc_{}", d.id))
                    .id(format!("receivables-receipt-alloc-{}", d.id))
                    .label(format!(
                        "{} · due {} · {} unpaid",
                        reference(d),
                        bilibi_finance_ledger::dates::display_text(
                            d.due_date.as_deref().unwrap_or("—")
                        ),
                        self.figures.optional(d.outstanding)
                    ))
                    .attr("inputmode", "decimal")
                    .value(value(&format!("alloc_{}", d.id))),
            )?);
        }
        let (applied, received) = Self::totals(draft);
        let rest = received - applied;
        let mut total = format!(
            "Applied {} of {} received.",
            self.figures.decimal(applied),
            self.figures.decimal(received)
        );
        if rest > Decimal::ZERO && !value("customer_id").is_empty() {
            total.push_str(&format!(
                " {} is kept as the customer's credit, to use on a later invoice or refund.",
                self.figures.decimal(rest)
            ));
        } else if rest < Decimal::ZERO {
            total.push_str(&format!(
                " That is {} more than was received.",
                self.figures.decimal(-rest)
            ));
        }
        body.push(&render(Note {
            id: "receivables-receipt-total",
            text: &total,
        })?);
        // The first receipt that keeps a credit needs to know where the
        // company holds such money. Whoever may change the setup chooses it
        // here, once; it is saved with this receipt.
        if rest > Decimal::ZERO && !self.keeps_credit && !value("customer_id").is_empty() {
            if self.can_setup {
                body.push(&render(
                    Combobox::new(
                        "receivables-receipt-credit_account_id",
                        "receipt[credit_account_id]",
                    )
                    .label("Keep customer credit in *")
                    .aria_label("Keep customer credit in")
                    .placeholder("Choose a liability account")
                    .wrapper_class("mt-3")
                    .hint("Asked once. This account holds money customers have paid that no invoice has used yet; later receipts use it too. Change it in Receivables settings.")
                    .options(
                        self.accounts
                            .iter()
                            .filter(|a| a.postable() && a.account_type == "liability")
                            .map(Account::choice)
                            .collect(),
                    )
                    .value(value("credit_account_id"))
                    .errors(
                        draft
                            .errors
                            .under("customer_credit_account_id", "Keep customer credit in"),
                    )
                    .attrs(Attrs {
                        required: true,
                        ..Attrs::default()
                    }),
                )?);
            } else {
                let mut notice = Html::text(
                    "The company has not chosen its customer credit account, so this receipt cannot keep an amount as credit yet. Ask someone who manages Receivables settings to choose it under Default accounts. ",
                );
                notice.push(&f::link(SETTINGS, "Go to Receivables settings")?);
                body.push(&render(Alert {
                    id: "receivables-receipt-credit-account",
                    kind: StatusKind::Warning,
                    class: "mt-3",
                    body: notice,
                })?);
            }
        }
        let form = render(Form {
            id: "receivables-receipt-form",
            event: "receipt_change",
            change: true,
            class: "mt-4",
            body,
            actions: Html::EMPTY,
        })?;
        let mut buttons = render(Button {
            id: "receivables-receipt-fill",
            button_type: "button",
            event: "apply_oldest",
            body: Html::text("Apply to oldest first"),
            ..Button::default()
        })?;
        buttons.push(&render(Button {
            id: "receivables-receipt-cancel",
            button_type: "button",
            event: "cancel_record",
            class: "ml-auto",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        buttons.push(&render(Button {
            id: "receivables-receipt-save",
            button_type: "button",
            event: "save_receipt",
            variant: ButtonVariant::Primary,
            working: "Recording…",
            body: Html::text("Record receipt"),
            ..Button::default()
        })?);
        let mut inner = form;
        inner.push(&f::buttons(buttons)?);
        f::modal(
            "receivables-receipt-modal",
            "Record receipt",
            "Recording numbers the receipt and posts it to the ledger.",
            "cancel_record",
            ModalWidth::Wide,
            &self.flash,
            inner,
        )
    }

    /// What the typed allocations add up to, and the amount received.
    pub(super) fn totals(draft: &Draft) -> (Decimal, Decimal) {
        // Amounts read as the pages write them, thousands commas included.
        let number = |text: &str| {
            bilibi_finance_ledger::Money::ungrouped(text)
                .and_then(|plain| plain.parse::<Decimal>().ok())
                .unwrap_or(Decimal::ZERO)
        };
        let applied = draft
            .open
            .iter()
            .map(|d| {
                number(
                    draft
                        .values
                        .get(&format!("alloc_{}", d.id))
                        .map_or("", String::as_str),
                )
            })
            .sum();
        (
            applied,
            number(draft.values.get("amount").map_or("", String::as_str)),
        )
    }
}
