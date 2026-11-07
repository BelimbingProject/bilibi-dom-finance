//! Form decoding only; accounting validation remains in the facade.
use super::*;
use crate::{AcquisitionInput, Assumptions, Method, RevisionInput};
use bilibi_finance_ledger::{FieldError, MinorUnits, Money};
use bilibi_ui::{Choice, InputKind};
use jiff::civil::Date;
use rust_decimal::Decimal;

#[derive(Default, Clone)]
pub(super) struct Draft {
    pub values: HashMap<String, String>,
    pub errors: Vec<FieldError>,
}
impl Draft {
    /// Every posted field in full: `params::form_values` drops a value over
    /// 1 KiB, and a 500 character description or reason can take 2 KiB; the
    /// facade bounds it.
    pub fn posted(payload: &serde_json::Value) -> Self {
        let values = payload
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let key = key
                    .strip_prefix("asset")?
                    .strip_prefix('[')?
                    .strip_suffix(']')?;
                Some((key.to_owned(), value.as_str()?.to_owned()))
            })
            .take(64)
            .collect();
        Self {
            values,
            errors: vec![],
        }
    }
    pub fn value(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }
    pub fn field(
        &self,
        key: &str,
        label: &str,
        kind: InputKind,
        required: bool,
        prefix: &str,
    ) -> Field {
        let mut f = Field::in_form("asset", key)
            .id(if prefix == "asset" {
                format!("asset_{key}")
            } else {
                format!("{prefix}-{}", key.replace('_', "-"))
            })
            .label(label)
            .kind(kind)
            .value(self.value(key))
            .errors(
                self.errors
                    .iter()
                    .filter(|e| e.field == key)
                    .map(|e| e.message.clone())
                    .collect(),
            );
        if required {
            f = f.required();
        }
        f
    }
    pub fn method_field(&self) -> Field {
        let mut f = self
            .field(
                "method",
                "Default method",
                InputKind::Select,
                false,
                "asset",
            )
            .options(vec![
                Choice::new("Straight line", "straight_line"),
                Choice::new("Reducing balance", "reducing_balance"),
            ]);
        if self.value("method").is_empty() {
            f = f.value("straight_line");
        }
        f
    }
    pub fn id(&self, key: &'static str) -> crate::Result<i64> {
        params::positive_integer(Some(self.value(key)))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(|| {
                crate::invalid(
                    key,
                    if self.value(key).trim().is_empty() {
                        "can't be blank"
                    } else {
                        "is invalid"
                    },
                )
            })
    }
    pub fn date(&self, key: &'static str) -> crate::Result<Date> {
        self.value(key).parse().map_err(|_| {
            crate::invalid(
                key,
                if self.value(key).is_empty() {
                    "can't be blank"
                } else {
                    "is invalid"
                },
            )
        })
    }
    fn decimal(&self, key: &'static str) -> crate::Result<Option<Decimal>> {
        if self.value(key).trim().is_empty() {
            return Ok(None);
        }
        self.value(key)
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| crate::invalid(key, "is invalid"))
    }
    fn money(&self, key: &'static str, units: MinorUnits) -> crate::Result<Money> {
        if self.value(key).trim().is_empty() {
            return Err(crate::invalid(key, "can't be blank"));
        }
        Money::parse(self.value(key), units).map_err(|e| crate::invalid(key, e.to_string()))
    }
    pub fn revision(&self) -> crate::Result<RevisionInput> {
        let mut c = Collect::default();
        let method = c.take(match self.value("method") {
            "" | "straight_line" => Ok(Method::StraightLine),
            "reducing_balance" => Ok(Method::ReducingBalance),
            _ => Err(crate::invalid("method", "is invalid")),
        })?;
        let cost = c.take(self.id("cost_account_id"))?;
        let accumulated = c.take(self.id("accumulated_account_id"))?;
        let expense = c.take(self.id("expense_account_id"))?;
        let gain = c.take(self.id("gain_account_id"))?;
        let loss = c.take(self.id("loss_account_id"))?;
        let life = c.take(self.id("life_months").and_then(|n| {
            i32::try_from(n).map_err(|_| crate::invalid("life_months", "is invalid"))
        }))?;
        let rate = c.take(self.decimal("annual_rate"))?;
        c.finish()?;
        Ok(RevisionInput {
            cost_account_id: got(cost)?,
            accumulated_account_id: got(accumulated)?,
            expense_account_id: got(expense)?,
            gain_account_id: got(gain)?,
            loss_account_id: got(loss)?,
            assumptions: Assumptions {
                method: got(method)?,
                life_months: got(life)?,
                annual_rate: got(rate)?,
            },
            reason: self.value("reason").into(),
        })
    }
    pub fn acquisition(&self, units: MinorUnits) -> crate::Result<AcquisitionInput> {
        let mut c = Collect::default();
        let revision = c.take(self.id("category_revision_id"))?;
        let in_service = c.take(self.date("in_service_date"))?;
        let residual = c.take(if self.value("residual_value").trim().is_empty() {
            Ok(None)
        } else {
            self.money("residual_value", units).map(Some)
        })?;
        c.finish()?;
        Ok(AcquisitionInput {
            category_revision_id: got(revision)?,
            description: self.value("description").into(),
            in_service_date: got(in_service)?,
            residual_value: got(residual)?,
            method: None,
            life_months: None,
            annual_rate: None,
        })
    }
    pub fn event_values(&self) -> serde_json::Value {
        serde_json::json!({"effective_date":self.value("effective_date"),"reason":self.value("reason")})
    }
    pub fn refused(&mut self, e: &FixedAssetsError) {
        self.errors = match e {
            FixedAssetsError::Invalid(f)
            | FixedAssetsError::Ledger(bilibi_finance_ledger::LedgerError::Invalid(f)) => f.clone(),
            _ => vec![],
        };
    }
}
pub(super) fn method(m: Method) -> &'static str {
    match m {
        Method::StraightLine => "Straight line",
        Method::ReducingBalance => "Reducing balance",
    }
}

/// Bilimbi's changesets report every bad field at once, so a form gathers
/// each field's refusal instead of stopping at the first.
#[derive(Default)]
struct Collect(Vec<FieldError>);
impl Collect {
    fn take<T>(&mut self, result: crate::Result<T>) -> crate::Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(FixedAssetsError::Invalid(fields)) => {
                self.0.extend(fields);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    /// Fields in Bilimbi's order: its flash joins `traverse_errors`' map,
    /// whose keys come out sorted.
    fn finish(mut self) -> crate::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            self.0.sort_by_key(|error| error.field);
            Err(FixedAssetsError::Invalid(self.0))
        }
    }
}
/// A value `Collect::finish` has already proven present.
fn got<T>(value: Option<T>) -> crate::Result<T> {
    value.ok_or_else(|| FixedAssetsError::Invalid(vec![]))
}
