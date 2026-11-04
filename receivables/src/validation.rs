//! Bilimbi Receivables changesets: cast only editable fields, then validate
//! effective values. Calculated and immutable attributes never enter SQL.
use crate::{invalid, Result};
use bilibi_finance_ledger::{Account, AccountType};
use rust_decimal::Decimal;
use serde_json::{json, Value};

pub(crate) fn merge(old: Option<&Value>, attrs: &Value, fields: &[&str]) -> Result<Value> {
    let attrs = attrs
        .as_object()
        .ok_or_else(|| invalid("attributes", "is invalid"))?;
    let mut value = old.cloned().unwrap_or_else(|| json!({}));
    for field in fields {
        if let Some(v) = attrs.get(*field) {
            value[*field] = v.clone();
        }
    }
    Ok(value)
}
pub(crate) fn text(data: &mut Value, field: &str, required: bool, max: usize) -> Result<()> {
    let value = match &data[field] {
        Value::Null => None,
        Value::String(s) => Some(s.trim()).filter(|s| !s.is_empty()),
        _ => return Err(invalid(field, "is invalid")),
    };
    if required && value.is_none() {
        return Err(invalid(field, "can't be blank"));
    }
    if value.is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
    data[field] = value.map_or(Value::Null, |s| json!(s));
    Ok(())
}
pub(crate) fn integer(data: &mut Value, field: &str, required: bool) -> Result<Option<i64>> {
    let v = match &data[field] {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::String(s) => Some(s.parse().map_err(|_| invalid(field, "is invalid"))?),
        Value::Number(n) => Some(n.as_i64().ok_or_else(|| invalid(field, "is invalid"))?),
        _ => return Err(invalid(field, "is invalid")),
    };
    if required && v.is_none() {
        return Err(invalid(field, "can't be blank"));
    }
    data[field] = json!(v);
    Ok(v)
}
pub(crate) fn range(data: &mut Value, field: &str, min: i64, max: i64) -> Result<()> {
    let n = integer(data, field, true)?.expect("required integer");
    if n < min {
        return Err(invalid(
            field,
            format!("must be greater than or equal to {min}"),
        ));
    }
    if n > max {
        return Err(invalid(
            field,
            format!("must be less than or equal to {max}"),
        ));
    }
    Ok(())
}
pub(crate) fn flag(data: &mut Value, field: &str) -> Result<()> {
    let value = match &data[field] {
        Value::Bool(v) => *v,
        Value::String(s) if s == "true" || s == "1" => true,
        Value::String(s) if s == "false" || s == "0" => false,
        Value::Null => return Err(invalid(field, "can't be blank")),
        _ => return Err(invalid(field, "is invalid")),
    };
    data[field] = json!(value);
    Ok(())
}
pub(crate) fn choice(data: &mut Value, field: &str, choices: &[&str]) -> Result<()> {
    text(data, field, true, usize::MAX)?;
    if !choices.contains(&data[field].as_str().expect("validated text")) {
        return Err(invalid(field, "is invalid"));
    }
    Ok(())
}
pub(crate) fn decimal(
    data: &mut Value,
    field: &str,
    positive: bool,
    limit: Decimal,
    scale: u8,
) -> Result<Decimal> {
    let text = match &data[field] {
        Value::String(s) if !s.trim().is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null | Value::String(_) => return Err(invalid(field, "can't be blank")),
        _ => return Err(invalid(field, "is invalid")),
    };
    let (negative, digits, exponent) =
        decimal_parts(&text).ok_or_else(|| invalid(field, "is invalid"))?;
    if negative || (positive && digits.is_empty()) {
        return Err(invalid(
            field,
            if positive {
                "must be greater than 0"
            } else {
                "must be greater than or equal to 0"
            },
        ));
    }
    // All callers' bounds are powers of ten. Check magnitude and scale as
    // digit strings first, so even 1e6000 / 1e-6000 gets a domain refusal,
    // never an overflow or rounded rust_decimal conversion.
    if !digits.is_empty() && digits.len() as i64 + exponent > limit.to_string().len() as i64 - 1 {
        return Err(invalid(
            field,
            format!(
                "must be less than {}",
                if limit == Decimal::from(10_000_000_000_000_000_i64) {
                    "1E+16".into()
                } else {
                    limit.to_string()
                }
            ),
        ));
    }
    if !digits.is_empty() && exponent < -i64::from(scale) {
        return Err(invalid(
            field,
            format!("has more than {scale} decimal places"),
        ));
    }
    let n = if digits.is_empty() {
        Decimal::ZERO
    } else if exponent >= 0 {
        Decimal::from_str_exact(&format!("{digits}{}", "0".repeat(exponent as usize)))
            .map_err(|_| invalid(field, "is invalid"))?
    } else {
        let coefficient =
            Decimal::from_str_exact(&digits).map_err(|_| invalid(field, "is invalid"))?;
        coefficient
            .checked_div(Decimal::from(10_i64.pow((-exponent) as u32)))
            .ok_or_else(|| invalid(field, "is invalid"))?
    }
    .normalize();
    data[field] = json!(n.to_string());
    Ok(n)
}

// Ecto.Type.cast(:decimal) -> Decimal.parse, finite whole-string grammar.
// Unlike rust_decimal's parser this accepts exponents and rejects underscores.
// Apply Decimal 3's default parse limits before stripping trailing zeros.
fn decimal_parts(text: &str) -> Option<(bool, String, i64)> {
    let (negative, unsigned) = if let Some(s) = text.strip_prefix('-') {
        (true, s)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    let mut pieces = unsigned.split(['e', 'E']);
    let mantissa = pieces.next()?;
    let exponent = if let Some(e) = pieces.next() {
        let unsigned = e.strip_prefix(['+', '-']).unwrap_or(e);
        if unsigned.is_empty() || !unsigned.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let magnitude = unsigned.trim_start_matches('0');
        let n: i64 = if magnitude.is_empty() {
            0
        } else {
            magnitude.parse().ok()?
        };
        if e.starts_with('-') {
            -n
        } else {
            n
        }
    } else {
        0
    };
    if pieces.next().is_some() {
        return None;
    }
    let mut points = mantissa.split('.');
    let whole = points.next()?;
    let fraction = points.next().unwrap_or("");
    if points.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    if !(-6144..=6144).contains(&exponent) {
        return None;
    }
    let mut digits = format!("{whole}{fraction}")
        .trim_start_matches('0')
        .to_owned();
    if digits.len() > 34 {
        return None;
    }
    while digits.ends_with('0') {
        digits.pop();
        exponent += 1;
    }
    Some((negative && !digits.is_empty(), digits, exponent))
}
pub(crate) fn date(data: &mut Value, field: &str) -> Result<()> {
    if data[field].is_null() || data[field] == "" {
        data[field] = Value::Null;
        return Ok(());
    }
    let date: jiff::civil::Date = data[field]
        .as_str()
        .ok_or_else(|| invalid(field, "is invalid"))?
        .parse()
        .map_err(|_| invalid(field, "is invalid"))?;
    data[field] = json!(date.to_string());
    Ok(())
}
pub(crate) fn account(
    data: &mut Value,
    field: &str,
    chart: &[Account],
    kind: AccountType,
) -> Result<()> {
    let id = integer(data, field, true)?.expect("required id");
    let Some(account) = chart.iter().find(|a| a.id == id) else {
        return Err(invalid(field, "is not a ledger account of this company"));
    };
    if account.is_group {
        return Err(invalid(
            field,
            "is a ledger account group; choose an account under it",
        ));
    }
    if !account.active {
        return Err(invalid(field, "is an inactive ledger account"));
    }
    if account.account_type != kind {
        return Err(invalid(
            field,
            format!("must be a ledger {} account", kind.as_str()),
        ));
    }
    Ok(())
}
