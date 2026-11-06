//! The company's book: its currency and decimal places, opened once, and the
//! two controls kept beside it (the Bookkeeping controls page changes them).
use super::{
    error_text, event_type, no_company, render, shell, unavailable, view, Failure, Flash, Pages,
    Read, Refusal, Review,
};
use crate::MANAGE_ACCOUNTS;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Attrs, BackLink, Badge, BadgeKind, Button, ButtonVariant, Card, Choice,
    Combobox, ConfirmDialog, Fact, Facts, Field, Form, Header, Html, InputKind, PageVariant,
    SectionHeading, StatusKind,
};
use serde::Deserialize;
use serde_json::json;

pub(super) const INDEX: &str = "/finance/ledger/books";

/// What `finance.ledger.book.currencies` answers: the currencies offered (every
/// current one for a person who may enable currencies, else the enabled ones)
/// with their ISO 4217 decimal places, and the proposed one.
#[derive(Default, Deserialize)]
struct Options {
    #[serde(default)]
    reference: String,
    #[serde(default)]
    can_enable: bool,
    proposed: Option<Proposed>,
    #[serde(default)]
    currencies: Vec<Listed>,
}
#[derive(Deserialize)]
struct Proposed {
    code: String,
    minor_units: u32,
    source: String,
}
#[derive(Deserialize)]
struct Listed {
    code: String,
    name: String,
    minor_units: u32,
    #[serde(default = "enabled")]
    enabled: bool,
}
fn enabled() -> bool {
    true
}
impl Options {
    fn find(&self, code: &str) -> Option<&Listed> {
        let code = code.trim().to_uppercase();
        self.currencies.iter().find(|c| c.code == code)
    }
    fn listed(&self) -> bool {
        self.reference == "iso4217"
    }
    fn none_enabled(&self) -> bool {
        self.reference == "none_enabled"
    }
}

pub(super) struct Book {
    pages: Pages,
    company: i64,
    currency: String,
    units: String,
    approver: String,
    approval_reason: String,
    refusal: Refusal,
    notice: String,
    options: Options,
    seeded: bool,
    review: Option<Review>,
    flash: Flash,
    /// Chart of accounts, once the book opened: its empty chart offers the
    /// starter charts.
    destination: Option<String>,
}

impl Book {
    pub(super) fn new(pages: Pages, company: i64) -> Self {
        Self {
            pages,
            company,
            currency: String::new(),
            units: "2".into(),
            approver: "someone_else".into(),
            approval_reason: String::new(),
            refusal: Refusal::default(),
            notice: String::new(),
            options: Options::default(),
            seeded: false,
            review: None,
            flash: Flash::default(),
            destination: None,
        }
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut body = Html::EMPTY;
        if !self
            .pages
            .companies(scope)
            .await?
            .iter()
            .any(|c| c.id == self.company)
        {
            body.push(&no_company("ledger-book-no-company")?);
        } else {
            match self.pages.book_in(scope, self.company).await? {
                Err(refused) => {
                    body.push(&unavailable("ledger-book-refused", "the book", &refused)?)
                }
                Ok(state) => match &state.book {
                    Some(book) => body.push(&self.facts(&state, book)?),
                    None => {
                        self.load_options(scope).await?;
                        body.push(&self.open_form(scope).await?)
                    }
                },
            }
        }
        let company_name = self.pages.company_name_in(scope, self.company).await?;
        let mut page = render(Header {
            title: Html::text("Book"),
            subtitle: &company_name,
            title_actions: bilibi_ui::Html::EMPTY,
            actions: render(BackLink {
                href: INDEX,
                id: "ledger-book-back",
                title: "Back to company books",
                ..BackLink::default()
            })?,
        })?;
        page.push(&render(super::Announce {
            kind: "finance/ledger.book",
            record_id: &self.company.to_string(),
        })?);
        page.push(&body);
        let redirect = self.destination.take();
        let flash = redirect.as_ref().and_then(|_| self.flash.0.take());
        Ok(LiveBody {
            html: shell("ledger-book", PageVariant::Detail, &self.flash, page)?,
            redirect,
            flash,
            ..LiveBody::default()
        })
    }

    /// The currency list and the proposal, read through the same contract an
    /// agent calls; the first read fills the form with the proposal.
    async fn load_options(&mut self, scope: &Scope) -> Result<()> {
        self.options = match self
            .pages
            .read_in(
                scope,
                self.company,
                "finance.ledger.book.currencies",
                json!({}),
            )
            .await?
        {
            Read::Found(value) => view::parse(value).unwrap_or_default(),
            _ => Options::default(),
        };
        if !self.seeded && !self.options.reference.is_empty() {
            self.seeded = true;
            if let Some(proposed) = &self.options.proposed {
                self.currency = proposed.code.clone();
                self.units = proposed.minor_units.to_string();
            }
        }
        Ok(())
    }

    fn facts(&self, state: &view::BookState, book: &view::BookRow) -> Result<Html> {
        let controlled = state
            .controlled_bookkeeping
            .as_ref()
            .and_then(|c| c.controlled)
            .unwrap_or(true);
        let required = state
            .duty_separation
            .as_ref()
            .and_then(|c| c.required)
            .unwrap_or(true);
        let currency = state.currency.clone().unwrap_or_default();
        let yes_no = |on: bool, yes: &str, no: &str| -> Result<Html> {
            render(Badge::status(
                if on {
                    BadgeKind::Success
                } else {
                    BadgeKind::Warning
                },
                if on { yes } else { no },
            ))
        };
        let mut body = render(SectionHeading {
            id: "ledger-book-heading",
            title: "Currency",
            description: "Chosen when the book was opened. It cannot be changed.",
            ..SectionHeading::default()
        })?;
        body.push(&render(Facts {
            id: "ledger-book-facts",
            items: vec![
                Fact {
                    title: "Currency",
                    id: "ledger-book-currency",
                    value: Html::text(&match &currency.currency_name {
                        Some(name) => format!("{} {}", book.currency_code, name),
                        None => book.currency_code.clone(),
                    }),
                },
                Fact {
                    title: "Decimal places",
                    id: "ledger-book-units",
                    value: Html::text(&match currency.standard_minor_units {
                        Some(standard) if u32::from(book.minor_units) != standard => {
                            format!(
                                "{} (nonstandard; ISO 4217 uses {})",
                                book.minor_units, standard
                            )
                        }
                        _ => book.minor_units.to_string(),
                    }),
                },
            ],
        })?);
        let mut controls = render(SectionHeading {
            id: "ledger-book-controls-heading",
            title: "Controls",
            description: "Controlled bookkeeping lets entries post only into open periods. Manual journals follow their own approval rules.",
            actions: render(bilibi_ui::ActionLink {
                id: "ledger-book-controls-change",
                href: "/finance/reports/controls",
                title: "Change on Bookkeeping controls",
                body: Html::text("Change on Bookkeeping controls"),
                ..bilibi_ui::ActionLink::default()
            })?,
            ..SectionHeading::default()
        })?;
        controls.push(&render(Facts {
            id: "ledger-book-controls",
            items: vec![
                Fact {
                    title: "Controlled bookkeeping",
                    id: "ledger-book-controlled",
                    value: yes_no(controlled, "On", "Off")?,
                },
                Fact {
                    title: "Someone else approves Finance records",
                    id: "ledger-book-separation",
                    value: yes_no(required, "Required", "Not required")?,
                },
            ],
        })?);
        let mut html = render(Card {
            id: "ledger-book-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-book-heading",
            body,
            ..Card::default()
        })?;
        html.push(&render(Card {
            id: "ledger-book-controls-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-book-controls-heading",
            class: "mt-4",
            body: controls,
            ..Card::default()
        })?);
        Ok(html)
    }

    async fn open_form(&self, scope: &Scope) -> Result<Html> {
        if !self.pages.allowed(scope, MANAGE_ACCOUNTS).await? {
            return render(
                bilibi_ui::EmptyState::missing(
                    "The book is not open yet",
                    "Ask someone who manages the chart of accounts to open it.",
                )
                .id("ledger-book-not-open"),
            );
        }
        // Only for a person who may not enable a currency: anyone who may is
        // offered every currency, and opening the book enables the one chosen.
        if self.options.none_enabled() {
            return render(
                bilibi_ui::EmptyState::missing(
                    "No currency is enabled yet",
                    "Opening the book needs its currency enabled first. Ask an administrator who manages currencies to enable it, then come back.",
                )
                .id("ledger-book-none-enabled"),
            );
        }
        if self.options.reference.is_empty() {
            return render(
                bilibi_ui::EmptyState::missing(
                    "The currency list could not be read",
                    "Reload the page to try again.",
                )
                .id("ledger-book-currencies-unread"),
            );
        }
        let errors = |name: &str| -> Vec<String> {
            self.refusal
                .field(name)
                .iter()
                .map(|m| error_text(name, m))
                .collect()
        };
        let mut body = Html::EMPTY;
        let mut messages = self.refusal.alert(&["currency_code", "minor_units"]);
        if !self.notice.is_empty() {
            messages.push(self.notice.clone());
        }
        if !messages.is_empty() {
            body.push(&render(Alert {
                id: "ledger-book-notice",
                kind: StatusKind::Error,
                body: Html::text(&messages.join(" ")),
                ..Alert::default()
            })?);
        }
        let chosen = self.options.find(&self.currency);
        if self.options.listed() {
            let mut hint = match (&self.options.proposed, chosen) {
                (Some(proposed), Some(c)) if proposed.code == c.code => format!(
                    "Proposed from the {} base currency. You cannot change it later.",
                    if proposed.source == "company" {
                        "company's"
                    } else {
                        "tenant's"
                    }
                ),
                _ => "Search by code or name. You cannot change it later.".into(),
            };
            if !self.options.can_enable {
                hint.push_str(" Only enabled currencies are listed; ask an administrator who manages currencies to enable another.");
            }
            if let Some(c) = chosen.filter(|c| !c.enabled) {
                hint.push_str(&format!(
                    " {} is not enabled yet: opening the book also enables it on the Currencies page.",
                    c.code
                ));
            }
            body.push(&render(
                Combobox::new("ledger-book-currency-code", "book[currency_code]")
                    .label("Currency *")
                    .aria_label("Currency")
                    .value(self.currency.trim().to_uppercase())
                    .options(
                        self.options
                            .currencies
                            .iter()
                            .map(|c| Choice::new(format!("{} {}", c.code, c.name), c.code.clone()))
                            .collect(),
                    )
                    .placeholder("Search currencies")
                    .hint(hint)
                    .errors(errors("currency_code"))
                    .attrs(Attrs {
                        required: true,
                        ..Attrs::default()
                    }),
            )?);
        } else {
            body.push(&render(
                Field::in_form("book", "currency_code")
                    .id("ledger-book-currency-code")
                    .label("Currency code")
                    .value(&self.currency)
                    .required()
                    .hint("The currency list has not been imported, so this code is not checked against ISO 4217. Three letters, like MYR. You cannot change it later.")
                    .errors(errors("currency_code"))
                    .attr("maxlength", "3")
                    .attr("data-debounce", "300"),
            )?);
        }
        let units_hint = match chosen {
            Some(c) if self.units.parse::<u32>().ok() != Some(c.minor_units) => format!(
                "Nonstandard: ISO 4217 uses {} for {}. You cannot change it later.",
                c.minor_units, c.code
            ),
            Some(c) => format!(
                "ISO 4217 standard for {}. You cannot change it later.",
                c.code
            ),
            None if self.options.listed() => {
                "Choose a currency to see its standard decimal places.".into()
            }
            None => "How many digits follow the point in an amount: 2 for 12.50. You cannot change it later.".into(),
        };
        body.push(&render(
            Field::in_form("book", "minor_units")
                .id("ledger-book-minor-units")
                .label("Decimal places")
                .value(&self.units)
                .kind(InputKind::Select)
                .options(
                    (0..=4)
                        .map(|n| Choice::new(n.to_string(), n.to_string()))
                        .collect(),
                )
                .hint(units_hint)
                .errors(errors("minor_units")),
        )?);
        body.push(&render(
            Field::in_form("book", "approver")
                .id("ledger-book-approver")
                .label("Finance approvals")
                .kind(InputKind::Select)
                .value(&self.approver)
                .options(vec![
                    Choice::new("Someone else must approve", "someone_else"),
                    Choice::new("The preparer may approve their own", "one_person"),
                ])
                .hint("For bills, payments and other Finance records. Manual journals follow their own approval rules."),
        )?);
        if self.approver == "one_person" {
            body.push(&render(
                Field::in_form("book", "approval_reason")
                    .id("ledger-book-approval-reason")
                    .label("Why the preparer may approve")
                    .value(&self.approval_reason)
                    .required()
                    .errors(errors("reason")),
            )?);
        }
        let actions = render(Button {
            id: "ledger-book-open",
            button_type: "button",
            event: "review_open",
            variant: ButtonVariant::Primary,
            body: Html::text("Open the book"),
            ..Button::default()
        })?;
        let mut card = render(SectionHeading {
            id: "ledger-book-open-heading",
            title: "Open the book",
            ..SectionHeading::default()
        })?;
        card.push(&render(Form {
            id: "ledger-book-form",
            event: "validate",
            change: true,
            class: "mt-4 max-w-md",
            body,
            actions,
        })?);
        let mut html = render(Card {
            id: "ledger-book-open-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "ledger-book-open-heading",
            body: card,
            ..Card::default()
        })?;
        if let Some(review) = &self.review {
            let after = &review.facts["after"];
            let currency = &after["currency"];
            let code = after["currency_code"].as_str().unwrap_or_default();
            let units = after["minor_units"].as_i64().unwrap_or_default();
            let label = match currency["currency_name"].as_str() {
                Some(name) => format!("{code} {name}"),
                None => code.to_owned(),
            };
            let mut detail = String::new();
            if currency["nonstandard_minor_units"].as_bool() == Some(true) {
                detail.push_str(&format!(
                    "Nonstandard: ISO 4217 uses {} decimal places for {code}. ",
                    currency["standard_minor_units"]
                ));
            }
            if currency["enables_currency"].as_bool() == Some(true) {
                detail.push_str(&format!(
                    "This also enables {code} on the Currencies page, for every company. "
                ));
            }
            if currency["reference"].as_str() == Some("unchecked") {
                detail.push_str(&format!(
                    "The currency list has not been imported, so {code} was not checked against ISO 4217. "
                ));
            }
            detail.push_str(if self.approver == "one_person" { "The currency and decimal places are permanent. The preparer may approve their own bills, payments and other Finance records; each such approval is listed in Approval exceptions. Manual journals follow their own approval rules." } else { "The currency and decimal places are permanent. Bills, payments and other Finance records need someone else's approval. Manual journals follow their own approval rules." });
            html.push(&render(ConfirmDialog {
                id: "ledger-book-confirm",
                consequence: &format!(
                    "Open the book for {} in {label} with {units} decimal places?",
                    self.pages.company_name_in(scope, self.company).await?
                ),
                detail: &detail,
                confirm: "Open the book",
                working: "Opening…",
                confirm_event: "open_book",
                cancel_event: "cancel_open",
            })?);
        }
        Ok(html)
    }
}

impl LiveState<Scope, Error> for Book {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = r.input {
                match event_type(value)? {
                    "validate" => {
                        let fields = params::form_values(value, "book");
                        let before = self.currency.trim().to_uppercase();
                        self.currency = fields.get("currency_code").cloned().unwrap_or_default();
                        self.units = fields
                            .get("minor_units")
                            .cloned()
                            .unwrap_or_else(|| "2".into());
                        // A new currency brings its own standard decimal places.
                        if self.currency.trim().to_uppercase() != before {
                            if let Some(listed) = self.options.find(&self.currency) {
                                self.units = listed.minor_units.to_string();
                            }
                        }
                        self.approver = fields
                            .get("approver")
                            .cloned()
                            .unwrap_or_else(|| "someone_else".into());
                        if let Some(reason) = fields.get("approval_reason") {
                            self.approval_reason = reason.clone();
                        }
                        self.refusal = Refusal::default();
                        self.notice.clear();
                    }
                    "review_open" => {
                        self.refusal = Refusal::default();
                        self.notice.clear();
                        let units: i64 = self.units.parse().unwrap_or(-1);
                        let enable = self
                            .options
                            .find(&self.currency)
                            .is_some_and(|c| !c.enabled);
                        match self
                            .pages
                            .review_in(
                                r.scope,
                                self.company,
                                r.credential,
                                "review_open",
                                "finance.ledger.book.open",
                                json!({"currency_code": self.currency.trim().to_uppercase(), "minor_units": units,"separation_required":self.approver != "one_person","approval_reason": self.approval_reason,"enable_currency": enable}),
                            )
                            .await?
                        {
                            Ok(review) => self.review = Some(review),
                            Err(Failure::Refused(refusal)) => self.refusal = refusal,
                            Err(failure) => self.notice = failure.text("The book"),
                        }
                    }
                    "cancel_open" => self.review = None,
                    "open_book" => {
                        let Some(review) = self.review.take() else {
                            return self.view(r.scope).await;
                        };
                        if review.input["company_id"].as_i64() != Some(self.company) {
                            self.notice = "The company changed. Review the book again.".into();
                            return self.view(r.scope).await;
                        }
                        match self
                            .pages
                            .commit(
                                r.scope,
                                r.credential,
                                "open_book",
                                "finance.ledger.book.open",
                                review,
                            )
                            .await?
                        {
                            Ok(_) => {
                                self.flash.success("The book is open.");
                                self.destination = Some(format!(
                                    "{}?company={}",
                                    super::accounts::INDEX,
                                    self.company
                                ));
                            }
                            Err(Failure::Refused(refusal)) => self.refusal = refusal,
                            Err(failure) => self.notice = failure.text("The book"),
                        }
                    }
                    "clear-flash" => self.flash.dismiss(value),
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(r.scope).await
        })
    }
}
