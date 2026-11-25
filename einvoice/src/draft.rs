//! A document a caller asks to issue, as plain attributes, before it is
//! recorded (Bilimbi `Bilimbi.Finance.Einvoice.Draft` and its `Party`,
//! `Address` and `Line` embeds).
//!
//! The draft owns the shape every authority shares: the document's type and
//! code number, its issue instant, currency and exchange rate, the
//! counterparty (the buyer, or on a self-billed document the supplier, the
//! company's own profile taking the other side), optional payment and
//! billing-period details, the original document a note corrects, and the
//! lines. It derives each line's amounts and the document's totals, so a
//! caller sends quantities, prices, discounts and tax rates and never a
//! total. The one exception is a consolidated document's line, whose tax is
//! the sum of its sources' tax. The authority then checks codes and its own
//! rules ([`Authority::validate`]), and [`Draft::validate`] runs both.
//!
//! Refusals are [`FieldError`]s in Bilimbi's changeset words, at the field's
//! path (`buyer.address.state_code`, `lines[2].tax_rate`).
use crate::authority::{Authority, ValidationContext};
use crate::FieldError;
use jiff::civil::Date;
use jiff::Timestamp;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// What a caller sends. A blank string is the same as `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Draft {
    pub document_type: Option<String>,
    pub code_number: Option<String>,
    /// The issue instant; now when `None`, never later than now.
    pub issued_at: Option<Timestamp>,
    pub currency_code: Option<String>,
    pub exchange_rate: Option<Decimal>,
    pub payment_means_code: Option<String>,
    pub payment_terms: Option<String>,
    pub billing_period_start: Option<Date>,
    pub billing_period_end: Option<Date>,
    pub billing_period_description: Option<String>,
    /// The original document a note corrects.
    pub original_document_id: Option<i64>,
    /// The counterparty of a sale.
    pub buyer: Option<DraftParty>,
    /// The counterparty of a self-billed document.
    pub supplier: Option<DraftParty>,
    pub lines: Vec<DraftLine>,
    /// The transaction this document is the tax document of, when another
    /// module already recognised it in the ledger; the facade holds it to the
    /// ledger (`Einvoice::issue_document`), not the draft's validation.
    pub source: Option<DraftSource>,
}

/// A source a caller names: the module that recognised the transaction (its
/// descriptor ID, `finance/receivables`), its record in that module's terms
/// (`document:17`), and the ledger entry that record posted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DraftSource {
    pub module: Option<String>,
    pub record: Option<String>,
    pub ledger_entry_id: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftParty {
    pub name: Option<String>,
    pub tax_id: Option<String>,
    pub registration_type: Option<String>,
    pub registration_number: Option<String>,
    pub sales_tax_registration: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// A supplier's (on a self-billed document) industry and activity.
    pub industry_code: Option<String>,
    pub business_activity: Option<String>,
    pub address: Option<DraftAddress>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAddress {
    /// One to three lines; blank ones are dropped.
    pub lines: Vec<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    pub state_code: Option<String>,
    pub country_code: Option<String>,
}

impl crate::Einvoice {
    /// Prefill a counterparty from a Core company selected by the caller.
    /// Restricted facts refuse the read, as they do when Receivables issues.
    /// The caller must supply authority-specific fields and validate the draft.
    pub async fn prefill_party(
        &self,
        scope: &bilibi_tenancy::Scope,
        company_id: i64,
        counterparty_id: i64,
    ) -> crate::Result<DraftParty> {
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        self.prefill_party_on(&**client, scope, company_id, counterparty_id)
            .await
    }

    pub(crate) async fn prefill_party_on(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &bilibi_tenancy::Scope,
        company_id: i64,
        counterparty_id: i64,
    ) -> crate::Result<DraftParty> {
        crate::authorize(client, scope, company_id, crate::VIEW).await?;
        let party = bilibi_finance_ledger::PartyLinks::new(&self.parties)
            .frozen_party(client, scope, "company", counterparty_id, "buyer", true)
            .await
            .map_err(|error| match error {
                bilibi_finance_ledger::LedgerError::Invalid(fields) => {
                    crate::EinvoiceError::Invalid(
                        fields
                            .into_iter()
                            .map(|field| crate::FieldError::new(field.field, field.message))
                            .collect(),
                    )
                }
                bilibi_finance_ledger::LedgerError::Failed(error) => {
                    crate::EinvoiceError::Failed(error)
                }
                _ => crate::EinvoiceError::Failed(bilibi_database::Error::Internal),
            })?;
        let fact = |name: &str| party[name].as_str().map(str::to_owned);
        Ok(DraftParty {
            name: fact("party_name"),
            tax_id: fact("party_tax_identifier"),
            registration_number: fact("party_registration_identifier"),
            email: fact("party_email"),
            phone: fact("party_phone"),
            // TODO(platform): Core does not expose billing addresses on this connection.
            ..DraftParty::default()
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DraftLine {
    pub description: Option<String>,
    pub classification_code: Option<String>,
    pub tariff_code: Option<String>,
    pub origin_country_code: Option<String>,
    pub quantity: Option<Decimal>,
    pub unit_code: Option<String>,
    pub unit_price: Option<Decimal>,
    /// A blank discount or charge is none.
    pub discount_amount: Option<Decimal>,
    pub charge_amount: Option<Decimal>,
    pub tax_type: Option<String>,
    /// A blank tax rate is refused, never zero.
    pub tax_rate: Option<Decimal>,
    pub tax_exemption_reason: Option<String>,
    /// Only a consolidated document's line carries its tax, the sum of its
    /// sources' tax; every other line's tax is derived from the rate.
    pub tax_amount: Option<Decimal>,
}

/// A draft that passed every check, with its derived amounts: what the
/// facade records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidDraft {
    pub document_type: String,
    pub code_number: String,
    pub issued_at: Timestamp,
    pub currency_code: String,
    pub exchange_rate: Option<Decimal>,
    pub payment_means_code: Option<String>,
    pub payment_terms: Option<String>,
    pub billing_period_start: Option<Date>,
    pub billing_period_end: Option<Date>,
    pub billing_period_description: Option<String>,
    pub original_document_id: Option<i64>,
    /// The counterparty of a sale (`supplier` is then `None`).
    pub buyer: Option<ValidParty>,
    /// The counterparty of a self-billed document (`buyer` is then `None`).
    pub supplier: Option<ValidParty>,
    pub lines: Vec<ValidLine>,
    /// The sum of the lines' amounts.
    pub line_extension_amount: Decimal,
    /// The sum of the lines' tax.
    pub tax_amount: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidParty {
    pub name: String,
    pub tax_id: String,
    pub registration_type: String,
    pub registration_number: String,
    pub sales_tax_registration: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub industry_code: Option<String>,
    pub business_activity: Option<String>,
    pub address: ValidAddress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidAddress {
    pub lines: Vec<String>,
    pub city: String,
    pub postcode: Option<String>,
    pub state_code: String,
    pub country_code: String,
}

/// A line with its derived amounts: the subtotal is quantity times unit
/// price, the amount is the subtotal less the discount plus the charge, and
/// the tax is the amount times the rate, each rounded half up to two places.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidLine {
    pub description: String,
    pub classification_code: String,
    pub tariff_code: Option<String>,
    pub origin_country_code: Option<String>,
    pub quantity: Decimal,
    pub unit_code: Option<String>,
    pub unit_price: Decimal,
    pub discount_amount: Decimal,
    pub charge_amount: Decimal,
    pub tax_type: String,
    pub tax_rate: Decimal,
    pub tax_exemption_reason: Option<String>,
    pub subtotal: Decimal,
    pub line_extension_amount: Decimal,
    pub tax_amount: Decimal,
}

const MAX_LINES: usize = 1_000;
// numeric(20,2): amounts are refused above this, never rounded.
const MAX_AMOUNT_TEXT: &str = "999999999999999999.99";
fn max_amount() -> Decimal {
    Decimal::from_str_exact(MAX_AMOUNT_TEXT).unwrap_or(Decimal::MAX)
}

/// Collects refusals at field paths.
struct Errors {
    prefix: String,
    errors: Vec<FieldError>,
}

impl Errors {
    fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            errors: Vec::new(),
        }
    }
    fn add(&mut self, field: &str, message: impl Into<String>) {
        self.errors
            .push(FieldError::new(path(&self.prefix, field), message));
    }
    fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

fn path(prefix: &str, field: &str) -> String {
    if prefix.is_empty() {
        field.to_owned()
    } else {
        format!("{prefix}.{field}")
    }
}

/// A given, non-blank string (Ecto casts a blank string to nil).
fn given(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|text| !text.trim().is_empty())
}

fn required(errors: &mut Errors, field: &str, value: Option<&str>) -> Option<String> {
    match value {
        Some(text) => Some(text.to_owned()),
        None => {
            errors.add(field, "can't be blank");
            None
        }
    }
}

fn at_most(errors: &mut Errors, field: &str, value: Option<&str>, max: usize) {
    if value.is_some_and(|text| text.chars().count() > max) {
        errors.add(field, format!("should be at most {max} character(s)"));
    }
}

fn required_number(errors: &mut Errors, field: &str, value: Option<Decimal>) -> Option<Decimal> {
    if value.is_none() {
        errors.add(field, "can't be blank");
    }
    value
}

fn greater_than(errors: &mut Errors, field: &str, value: Option<Decimal>, bound: Decimal) {
    if value.is_some_and(|value| value <= bound) {
        errors.add(field, format!("must be greater than {bound}"));
    }
}

fn at_least(errors: &mut Errors, field: &str, value: Option<Decimal>, bound: Decimal) {
    if value.is_some_and(|value| value < bound) {
        errors.add(field, format!("must be greater than or equal to {bound}"));
    }
}

fn less_than_max(errors: &mut Errors, field: &str, value: Option<Decimal>) {
    if value.is_some_and(|value| value >= max_amount()) {
        errors.add(field, format!("must be less than {MAX_AMOUNT_TEXT}"));
    }
}

// Amounts are refused, not rounded, when finer than their column.
fn scale(errors: &mut Errors, field: &str, value: Option<Decimal>, places: u32) {
    if value.is_some_and(|value| value.round_dp(places) != value) {
        errors.add(field, format!("has more than {places} decimal places"));
    }
}

// Two places, always written with both (`10.20`, `0.00`), as the columns hold
// them.
fn money(value: Decimal) -> Decimal {
    let mut money = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    money.rescale(2);
    money
}

impl Draft {
    /// Checks the shape every authority shares, then the authority's own
    /// rules, and derives the amounts: the valid draft, or every refusal.
    /// `now` is the issue instant when the caller gives none, and an issue
    /// instant later than `now` is refused.
    pub fn validate(
        &self,
        now: Timestamp,
        authority: &impl Authority,
        context: &ValidationContext<'_>,
    ) -> Result<ValidDraft, Vec<FieldError>> {
        let shape = self.check(
            now,
            &authority.self_billed_document_types(),
            context.consolidated,
        );
        let mut errors = authority.validate(self, context);
        match shape {
            Ok(valid) if errors.is_empty() => Ok(valid),
            Ok(_) => Err(errors),
            Err(shape_errors) => {
                let mut all = shape_errors;
                all.append(&mut errors);
                Err(all)
            }
        }
    }

    /// The shape check alone (Bilimbi `Draft.changeset/3`): `self_billed_types`
    /// are the document types whose counterparty is the `supplier` rather
    /// than the `buyer`, and `source_tax` is true only for a consolidated
    /// document, whose lines carry their `tax_amount`.
    pub(crate) fn check(
        &self,
        now: Timestamp,
        self_billed_types: &[&str],
        source_tax: bool,
    ) -> Result<ValidDraft, Vec<FieldError>> {
        let mut errors = Errors::new("");
        let now = Timestamp::from_second(now.as_second()).unwrap_or(now);

        let document_type = required(&mut errors, "document_type", given(&self.document_type));
        let code_number = required(
            &mut errors,
            "code_number",
            given(&self.code_number).map(str::trim),
        );
        if let Some(code) = &code_number {
            let length = code.chars().count();
            if length > 50 {
                errors.add("code_number", "should be at most 50 character(s)");
            }
        }
        let issued_at = self.issued_at.unwrap_or(now);
        if issued_at > now {
            errors.add("issued_at", "cannot be in the future");
        }
        let currency_code = required(&mut errors, "currency_code", given(&self.currency_code));
        if let Some(currency) = &currency_code {
            if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
                errors.add("currency_code", "has invalid format");
            }
        }
        greater_than(
            &mut errors,
            "exchange_rate",
            self.exchange_rate,
            Decimal::ZERO,
        );
        if self.original_document_id.is_some_and(|id| id <= 0) {
            errors.add("original_document_id", "must be greater than 0");
        }
        at_most(
            &mut errors,
            "payment_terms",
            given(&self.payment_terms),
            300,
        );
        at_most(
            &mut errors,
            "billing_period_description",
            given(&self.billing_period_description),
            50,
        );

        // The counterparty is the buyer, or the supplier on a self-billed
        // document; the company's own profile is the other party, never given.
        let self_billed = document_type
            .as_deref()
            .is_some_and(|t| self_billed_types.contains(&t));
        let (party_field, party, other_field, other, message) = if self_billed {
            (
                "supplier",
                &self.supplier,
                "buyer",
                &self.buyer,
                "is the company on a self-billed document; give the supplier",
            )
        } else {
            (
                "buyer",
                &self.buyer,
                "supplier",
                &self.supplier,
                "is the company; give the buyer",
            )
        };
        let counterparty = match party {
            Some(party) => party.check(party_field, &mut errors.errors),
            None => {
                errors.add(party_field, "can't be blank");
                None
            }
        };
        if other.is_some() {
            errors.add(other_field, message);
        }

        if self.lines.is_empty() {
            errors.add("lines", "can't be blank");
        } else if self.lines.len() > MAX_LINES {
            errors.add("lines", format!("should have at most {MAX_LINES} item(s)"));
        }
        let lines: Vec<Option<ValidLine>> = self
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                line.check(&format!("lines[{index}]"), source_tax, &mut errors.errors)
            })
            .collect();

        if let (Some(start), Some(end)) = (self.billing_period_start, self.billing_period_end) {
            if start > end {
                errors.add("billing_period_end", "cannot be before the period's start");
            }
        }

        if !errors.is_empty() {
            return Err(errors.errors);
        }
        let lines: Vec<ValidLine> = lines.into_iter().flatten().collect();
        let sum = |f: fn(&ValidLine) -> Decimal| {
            lines
                .iter()
                .fold(Decimal::new(0, 2), |total, line| total + f(line))
        };
        let line_extension_amount = sum(|line| line.line_extension_amount);
        let tax_amount = sum(|line| line.tax_amount);
        if line_extension_amount + tax_amount > max_amount() {
            return Err(vec![FieldError::new(
                "lines",
                "makes the document amount too large",
            )]);
        }
        let (buyer, supplier) = if self_billed {
            (None, counterparty)
        } else {
            (counterparty, None)
        };
        Ok(ValidDraft {
            document_type: document_type.unwrap_or_default(),
            code_number: code_number.unwrap_or_default(),
            issued_at,
            currency_code: currency_code.unwrap_or_default(),
            exchange_rate: self.exchange_rate,
            payment_means_code: given(&self.payment_means_code).map(str::to_owned),
            payment_terms: given(&self.payment_terms).map(str::to_owned),
            billing_period_start: self.billing_period_start,
            billing_period_end: self.billing_period_end,
            billing_period_description: given(&self.billing_period_description).map(str::to_owned),
            original_document_id: self.original_document_id,
            buyer,
            supplier,
            line_extension_amount,
            tax_amount,
            lines,
        })
    }
}

impl DraftParty {
    fn check(&self, prefix: &str, all: &mut Vec<FieldError>) -> Option<ValidParty> {
        let mut errors = Errors::new(prefix);
        let name = required(&mut errors, "name", given(&self.name));
        let tax_id = required(&mut errors, "tax_id", given(&self.tax_id));
        let registration_type = required(
            &mut errors,
            "registration_type",
            given(&self.registration_type),
        );
        let registration_number = required(
            &mut errors,
            "registration_number",
            given(&self.registration_number),
        );
        at_most(&mut errors, "name", given(&self.name), 300);
        at_most(&mut errors, "tax_id", given(&self.tax_id), 14);
        at_most(
            &mut errors,
            "registration_number",
            given(&self.registration_number),
            20,
        );
        at_most(
            &mut errors,
            "sales_tax_registration",
            given(&self.sales_tax_registration),
            35,
        );
        at_most(&mut errors, "email", given(&self.email), 320);
        at_most(&mut errors, "phone", given(&self.phone), 20);
        at_most(&mut errors, "industry_code", given(&self.industry_code), 10);
        at_most(
            &mut errors,
            "business_activity",
            given(&self.business_activity),
            300,
        );
        let address = match &self.address {
            Some(address) => address.check(&path(prefix, "address"), &mut errors.errors),
            None => {
                errors.add("address", "can't be blank");
                None
            }
        };
        let valid = errors.is_empty();
        all.append(&mut errors.errors);
        if !valid {
            return None;
        }
        Some(ValidParty {
            name: name?,
            tax_id: tax_id?,
            registration_type: registration_type?,
            registration_number: registration_number?,
            sales_tax_registration: given(&self.sales_tax_registration).map(str::to_owned),
            email: given(&self.email).map(str::to_owned),
            phone: given(&self.phone).map(str::to_owned),
            industry_code: given(&self.industry_code).map(str::to_owned),
            business_activity: given(&self.business_activity).map(str::to_owned),
            address: address?,
        })
    }
}

impl DraftAddress {
    fn check(&self, prefix: &str, all: &mut Vec<FieldError>) -> Option<ValidAddress> {
        let mut errors = Errors::new(prefix);
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        if lines.is_empty() {
            errors.add("lines", "should have at least 1 item(s)");
        } else if lines.len() > 3 {
            errors.add("lines", "should have at most 3 item(s)");
        }
        if lines.iter().any(|line| line.chars().count() > 150) {
            errors.add("lines", "each line must be at most 150 characters");
        }
        let city = required(&mut errors, "city", given(&self.city));
        let state_code = required(&mut errors, "state_code", given(&self.state_code));
        let country_code = required(&mut errors, "country_code", given(&self.country_code));
        at_most(&mut errors, "city", given(&self.city), 50);
        at_most(&mut errors, "postcode", given(&self.postcode), 50);
        let valid = errors.is_empty();
        all.append(&mut errors.errors);
        if !valid {
            return None;
        }
        Some(ValidAddress {
            lines,
            city: city?,
            postcode: given(&self.postcode).map(str::to_owned),
            state_code: state_code?,
            country_code: country_code?,
        })
    }
}

impl DraftLine {
    fn check(
        &self,
        prefix: &str,
        source_tax: bool,
        all: &mut Vec<FieldError>,
    ) -> Option<ValidLine> {
        let mut errors = Errors::new(prefix);
        let discount = self.discount_amount.unwrap_or(Decimal::ZERO);
        let charge = self.charge_amount.unwrap_or(Decimal::ZERO);
        let description = required(&mut errors, "description", given(&self.description));
        let classification_code = required(
            &mut errors,
            "classification_code",
            given(&self.classification_code),
        );
        let quantity = required_number(&mut errors, "quantity", self.quantity);
        let unit_price = required_number(&mut errors, "unit_price", self.unit_price);
        let tax_type = required(&mut errors, "tax_type", given(&self.tax_type));
        let tax_rate = required_number(&mut errors, "tax_rate", self.tax_rate);
        at_most(&mut errors, "description", given(&self.description), 300);
        at_most(&mut errors, "tariff_code", given(&self.tariff_code), 32);
        at_most(
            &mut errors,
            "tax_exemption_reason",
            given(&self.tax_exemption_reason),
            300,
        );
        greater_than(&mut errors, "quantity", quantity, Decimal::ZERO);
        less_than_max(&mut errors, "quantity", quantity);
        at_least(&mut errors, "unit_price", unit_price, Decimal::ZERO);
        less_than_max(&mut errors, "unit_price", unit_price);
        at_least(
            &mut errors,
            "discount_amount",
            Some(discount),
            Decimal::ZERO,
        );
        less_than_max(&mut errors, "discount_amount", Some(discount));
        at_least(&mut errors, "charge_amount", Some(charge), Decimal::ZERO);
        less_than_max(&mut errors, "charge_amount", Some(charge));
        at_least(&mut errors, "tax_rate", tax_rate, Decimal::ZERO);
        if tax_rate.is_some_and(|rate| rate > Decimal::ONE_HUNDRED) {
            errors.add("tax_rate", "must be less than or equal to 100");
        }
        scale(&mut errors, "quantity", quantity, 6);
        scale(&mut errors, "unit_price", unit_price, 6);
        // Both operands are numeric(20,6), not the amounts' numeric(20,2).
        for (field, value) in [("quantity", quantity), ("unit_price", unit_price)] {
            if value.is_some_and(|v| v >= Decimal::from(100_000_000_000_000_i64)) {
                errors.add(field, "must be less than 100000000000000");
            }
        }
        scale(&mut errors, "discount_amount", Some(discount), 2);
        scale(&mut errors, "charge_amount", Some(charge), 2);
        scale(&mut errors, "tax_rate", tax_rate, 4);
        let source_tax_amount = if source_tax {
            let amount = required_number(&mut errors, "tax_amount", self.tax_amount);
            at_least(&mut errors, "tax_amount", amount, Decimal::ZERO);
            less_than_max(&mut errors, "tax_amount", amount);
            scale(&mut errors, "tax_amount", amount, 2);
            amount
        } else {
            None
        };

        let derived = if errors.is_empty() {
            let (Some(quantity), Some(unit_price), Some(tax_rate)) =
                (quantity, unit_price, tax_rate)
            else {
                return None;
            };
            // subtotal = quantity x unit price; line amount = subtotal -
            // discount + charge; tax = line amount x rate. Each rounds half up
            // to two places.
            match quantity.checked_mul(unit_price).map(money) {
                Some(subtotal) if subtotal <= max_amount() => {
                    let line_amount = subtotal - discount + charge;
                    if line_amount.is_sign_negative() && !line_amount.is_zero() {
                        errors.add("discount_amount", "exceeds the line's amount");
                        None
                    } else if line_amount > max_amount() {
                        errors.add("quantity", "makes the line amount too large");
                        None
                    } else {
                        // The stored subtotal and line amount are bounded
                        // before tax arithmetic, so multiplication is safe.
                        let tax = source_tax_amount.unwrap_or_else(|| {
                            money(line_amount * tax_rate / Decimal::ONE_HUNDRED)
                        });
                        Some((subtotal, line_amount, tax))
                    }
                }
                _ => {
                    errors.add("quantity", "makes the line amount too large");
                    None
                }
            }
        } else {
            None
        };
        let valid = errors.is_empty();
        all.append(&mut errors.errors);
        if !valid {
            return None;
        }
        let (subtotal, line_extension_amount, tax_amount) = derived?;
        Some(ValidLine {
            description: description?,
            classification_code: classification_code?,
            tariff_code: given(&self.tariff_code).map(str::to_owned),
            origin_country_code: given(&self.origin_country_code).map(str::to_owned),
            quantity: quantity?,
            unit_code: given(&self.unit_code).map(str::to_owned),
            unit_price: unit_price?,
            discount_amount: discount,
            charge_amount: charge,
            tax_type: tax_type?,
            tax_rate: tax_rate?,
            tax_exemption_reason: given(&self.tax_exemption_reason).map(str::to_owned),
            subtotal,
            line_extension_amount,
            tax_amount,
        })
    }
}
