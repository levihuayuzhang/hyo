//! Cell values, Excel error codes and General-format number rendering.

use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

/// Excel-compatible error values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    Div0,
    Value,
    Ref,
    Name,
    Num,
    Na,
    Null,
    Circular,
    Spill,
    Calc,
}

impl Error {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Div0 => "#DIV/0!",
            Self::Value => "#VALUE!",
            Self::Ref => "#REF!",
            Self::Name => "#NAME?",
            Self::Num => "#NUM!",
            Self::Na => "#N/A",
            Self::Null => "#NULL!",
            Self::Circular => "#CIRC!",
            Self::Spill => "#SPILL!",
            Self::Calc => "#CALC!",
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A cell value. Formulas are evaluated into this type.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(Error),
}

impl Value {
    pub fn text(value: impl Into<Arc<str>>) -> Self {
        let value = value.into();
        if value.is_empty() {
            Self::Empty
        } else {
            Self::Text(value)
        }
    }

    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    pub const fn error(&self) -> Option<Error> {
        match self {
            Self::Error(error) => Some(*error),
            _ => None,
        }
    }

    /// Coercion used by arithmetic operators and by scalar function arguments.
    ///
    /// Excel rules: empty -> 0, boolean -> 1/0, numeric text -> number, any
    /// other text -> #VALUE!, errors propagate.
    pub fn coerce_number(&self) -> Result<f64, Error> {
        match self {
            Self::Empty => Ok(0.0),
            Self::Number(value) => Ok(*value),
            Self::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
            Self::Text(text) => parse_number_text(text).ok_or(Error::Value),
            Self::Error(error) => Err(*error),
        }
    }

    /// Coercion used by `IF`, `AND`, `OR` and `NOT`.
    pub fn coerce_bool(&self) -> Result<bool, Error> {
        match self {
            Self::Empty => Ok(false),
            Self::Bool(value) => Ok(*value),
            Self::Number(value) => Ok(*value != 0.0),
            Self::Text(text) => {
                if text.eq_ignore_ascii_case("TRUE") {
                    Ok(true)
                } else if text.eq_ignore_ascii_case("FALSE") {
                    Ok(false)
                } else {
                    Err(Error::Value)
                }
            }
            Self::Error(error) => Err(*error),
        }
    }

    /// Text used by `&` and the text functions. Numbers use Excel's General
    /// format (15 significant digits) rather than Rust's shortest round-trip.
    pub fn to_text(&self) -> Result<String, Error> {
        match self {
            Self::Empty => Ok(String::new()),
            Self::Number(value) => Ok(format_number(*value)),
            Self::Text(text) => Ok(text.to_string()),
            Self::Bool(true) => Ok("TRUE".to_owned()),
            Self::Bool(false) => Ok("FALSE".to_owned()),
            Self::Error(error) => Err(*error),
        }
    }

    /// User-facing rendering for the grid.
    pub fn display(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(value) => format_number(*value),
            Self::Text(text) => text.to_string(),
            Self::Bool(true) => "TRUE".to_owned(),
            Self::Bool(false) => "FALSE".to_owned(),
            Self::Error(error) => error.as_str().to_owned(),
        }
    }

    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Number(_) => "number",
            Self::Text(_) => "text",
            Self::Bool(_) => "boolean",
            Self::Error(_) => "error",
        }
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Number(value as f64)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<Error> for Value {
    fn from(value: Error) -> Self {
        Self::Error(value)
    }
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// Parses numeric text using spreadsheet-friendly rules: surrounding spaces
/// are ignored, thousands separators are accepted when they group digits, and
/// a trailing `%` divides by 100.
pub fn parse_number_text(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (body, percent) = match trimmed.strip_suffix('%') {
        Some(body) => (body.trim_end(), true),
        None => (trimmed, false),
    };
    let body = body.strip_prefix('+').unwrap_or(body);
    let candidate = if body.contains(',') {
        let (negative, unsigned) = match body.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, body),
        };
        let (integer, fraction) = match unsigned.split_once('.') {
            Some((integer, fraction)) => (integer, Some(fraction)),
            None => (unsigned, None),
        };
        if !valid_grouping(integer) {
            return None;
        }
        let mut candidate = String::new();
        if negative {
            candidate.push('-');
        }
        candidate.push_str(&integer.replace(',', ""));
        if let Some(fraction) = fraction {
            if !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            candidate.push('.');
            candidate.push_str(fraction);
        }
        candidate
    } else {
        body.to_owned()
    };
    let value: f64 = candidate.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(if percent { value / 100.0 } else { value })
}

fn valid_grouping(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let mut groups = unsigned.split(',');
    let Some(first) = groups.next() else {
        return false;
    };
    if first.is_empty() || first.len() > 3 || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    groups.all(|group| group.len() == 3 && group.bytes().all(|b| b.is_ascii_digit()))
}

/// Excel's "General" formatting uses at most 15 significant decimal digits.
/// This mirrors that contract closely enough for grid display and text
/// concatenation while keeping the stored value binary64.
pub fn format_number(value: f64) -> String {
    if value.is_nan() {
        return Error::Num.as_str().to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-1E+308".to_owned()
        } else {
            "1E+308".to_owned()
        };
    }
    if value == 0.0 {
        return "0".to_owned();
    }

    let abs = value.abs();
    if !(1e-15..1e15).contains(&abs) {
        return format_scientific(value);
    }

    // 15 significant digits -> decimal places depend on the integer magnitude.
    let integer_digits = if abs >= 1.0 {
        abs.log10().floor() as i32 + 1
    } else {
        0
    };
    let decimals = (15 - integer_digits).clamp(0, 15) as usize;
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" { "0".to_owned() } else { text }
}

fn format_scientific(value: f64) -> String {
    let text = format!("{value:.14E}");
    let (mantissa, exponent) = text.split_once('E').unwrap_or((&text, "0"));
    let mut mantissa = mantissa
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    mantissa.push('E');
    mantissa.push(if exponent >= 0 { '+' } else { '-' });
    mantissa.push_str(&format!("{:02}", exponent.abs()));
    mantissa
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_format_uses_spreadsheet_precision() {
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1.0 / 3.0), "0.333333333333333");
        assert_eq!(format_number(123_456_789.125), "123456789.125");
        assert_eq!(format_number(1e20), "1E+20");
        assert_eq!(format_number(1e-20), "1E-20");
    }

    #[test]
    fn numeric_text_rules() {
        assert_eq!(parse_number_text(" 1,234.5 "), Some(1234.5));
        assert_eq!(parse_number_text("12%"), Some(0.12));
        assert_eq!(parse_number_text("1,23"), None);
        assert_eq!(parse_number_text("abc"), None);
    }

    #[test]
    fn coercions() {
        assert_eq!(Value::Bool(true).coerce_number(), Ok(1.0));
        assert_eq!(Value::Empty.coerce_number(), Ok(0.0));
        assert_eq!(Value::text("2.5").coerce_number(), Ok(2.5));
        assert_eq!(Value::text("x").coerce_number(), Err(Error::Value));
        assert_eq!(Value::Error(Error::Div0).coerce_number(), Err(Error::Div0));
    }
}
