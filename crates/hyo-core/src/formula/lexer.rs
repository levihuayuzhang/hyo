//! Formula tokenizer.

use crate::addr::CellAddr;
use crate::value::Error;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Number(f64),
    Text(String),
    Ident(String),
    SheetName(String),
    Cell(CellAddr),
    Error(Error),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Percent,
    Amp,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    Comma,
    Semicolon,
    Colon,
    Bang,
    At,
    Eof,
}

impl Display for Token {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::Text(value) => write!(f, "\"{value}\""),
            Self::Ident(value) => f.write_str(value),
            Self::SheetName(value) => write!(f, "'{value}'"),
            Self::Cell(addr) => f.write_str(&addr.to_a1()),
            Self::Error(error) => f.write_str(error.as_str()),
            Self::Plus => f.write_str("+"),
            Self::Minus => f.write_str("-"),
            Self::Star => f.write_str("*"),
            Self::Slash => f.write_str("/"),
            Self::Caret => f.write_str("^"),
            Self::Percent => f.write_str("%"),
            Self::Amp => f.write_str("&"),
            Self::Eq => f.write_str("="),
            Self::Ne => f.write_str("<>"),
            Self::Lt => f.write_str("<"),
            Self::Le => f.write_str("<="),
            Self::Gt => f.write_str(">"),
            Self::Ge => f.write_str(">="),
            Self::LParen => f.write_str("("),
            Self::RParen => f.write_str(")"),
            Self::Comma => f.write_str(","),
            Self::Semicolon => f.write_str(";"),
            Self::Colon => f.write_str(":"),
            Self::Bang => f.write_str("!"),
            Self::At => f.write_str("@"),
            Self::Eof => f.write_str("end of formula"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaError {
    pub message: String,
    pub offset: usize,
}

impl FormulaError {
    pub fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

impl Display for FormulaError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for FormulaError {}

#[cfg(test)]
pub fn tokenize(source: &str) -> Result<Vec<Token>, FormulaError> {
    Ok(tokenize_spanned(source)?
        .into_iter()
        .map(|(token, _)| token)
        .collect())
}

pub(crate) fn tokenize_spanned(source: &str) -> Result<Vec<(Token, usize)>, FormulaError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut starts = Vec::new();
    let mut i = 0;

    macro_rules! push_token {
        ($token:expr, $start:expr) => {{
            tokens.push($token);
            starts.push($start);
        }};
    }

    while i < bytes.len() {
        let i_start = i;
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'0'..=b'9' | b'.' if b != b'.' || bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let start = i;
                i = scan_number(bytes, i)?;
                let text = &source[start..i];
                let value: f64 = text
                    .parse()
                    .map_err(|_| FormulaError::new("invalid number", start))?;
                push_token!(Token::Number(value), i_start);
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut value = String::new();
                loop {
                    match bytes.get(i) {
                        None => {
                            return Err(FormulaError::new("unterminated string", start));
                        }
                        Some(b'"') if bytes.get(i + 1) == Some(&b'"') => {
                            value.push('"');
                            i += 2;
                        }
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(_) => {
                            let ch = source[i..].chars().next().unwrap();
                            value.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                push_token!(Token::Text(value), i_start);
            }
            b'\'' => {
                let start = i;
                i += 1;
                let mut value = String::new();
                loop {
                    match bytes.get(i) {
                        None => {
                            return Err(FormulaError::new("unterminated sheet name", start));
                        }
                        Some(b'\'') if bytes.get(i + 1) == Some(&b'\'') => {
                            value.push('\'');
                            i += 2;
                        }
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(_) => {
                            let ch = source[i..].chars().next().unwrap();
                            value.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                push_token!(Token::SheetName(value), i_start);
            }
            b'#' => {
                let start = i;
                let rest = source[start..].to_ascii_uppercase();
                let (literal, error) = ERROR_LITERALS
                    .iter()
                    .find(|(literal, _)| rest.starts_with(literal))
                    .ok_or_else(|| FormulaError::new("unknown error literal", start))?;
                i += literal.len();
                push_token!(Token::Error(*error), i_start);
            }
            b'$' | b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                if let Some((addr, end)) = scan_cell_ref(source, i) {
                    push_token!(Token::Cell(addr), i_start);
                    i = end;
                    continue;
                }
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'.'))
                {
                    i += 1;
                }
                if i == start {
                    return Err(FormulaError::new("unexpected character", start));
                }
                push_token!(Token::Ident(source[start..i].to_owned()), i_start);
            }
            b'+' => {
                push_token!(Token::Plus, i_start);
                i += 1;
            }
            b'-' => {
                push_token!(Token::Minus, i_start);
                i += 1;
            }
            b'*' => {
                push_token!(Token::Star, i_start);
                i += 1;
            }
            b'/' => {
                push_token!(Token::Slash, i_start);
                i += 1;
            }
            b'^' => {
                push_token!(Token::Caret, i_start);
                i += 1;
            }
            b'%' => {
                push_token!(Token::Percent, i_start);
                i += 1;
            }
            b'&' => {
                push_token!(Token::Amp, i_start);
                i += 1;
            }
            b'=' => {
                push_token!(Token::Eq, i_start);
                i += 1;
            }
            b'<' => {
                if bytes.get(i + 1) == Some(&b'>') {
                    push_token!(Token::Ne, i_start);
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'=') {
                    push_token!(Token::Le, i_start);
                    i += 2;
                } else {
                    push_token!(Token::Lt, i_start);
                    i += 1;
                }
            }
            b'>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    push_token!(Token::Ge, i_start);
                    i += 2;
                } else {
                    push_token!(Token::Gt, i_start);
                    i += 1;
                }
            }
            b'(' => {
                push_token!(Token::LParen, i_start);
                i += 1;
            }
            b')' => {
                push_token!(Token::RParen, i_start);
                i += 1;
            }
            b',' => {
                push_token!(Token::Comma, i_start);
                i += 1;
            }
            b';' => {
                push_token!(Token::Semicolon, i_start);
                i += 1;
            }
            b':' => {
                push_token!(Token::Colon, i_start);
                i += 1;
            }
            b'!' => {
                push_token!(Token::Bang, i_start);
                i += 1;
            }
            b'@' => {
                push_token!(Token::At, i_start);
                i += 1;
            }
            _ => {
                return Err(FormulaError::new(
                    format!("unexpected character {:?}", b as char),
                    i,
                ));
            }
        }
    }

    tokens.push(Token::Eof);
    starts.push(source.len());
    Ok(tokens.into_iter().zip(starts).collect())
}

fn scan_number(bytes: &[u8], start: usize) -> Result<usize, FormulaError> {
    let mut i = start;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let exponent_mark = i;
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let digits = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == digits {
            return Err(FormulaError::new("invalid exponent", exponent_mark));
        }
    }
    Ok(i)
}

/// Scans a cell address (`$A$1`). Returns `None` when the text is an
/// identifier instead. A cell-looking token followed by `(` is a function
/// name: `LOG10(...)` is the logarithm function, not cell LOG10.
fn scan_cell_ref(source: &str, start: usize) -> Option<(CellAddr, usize)> {
    let bytes = source.as_bytes();
    let mut i = start;
    if bytes.get(i) == Some(&b'$') {
        i += 1;
    }
    let col_start = i;
    while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) && i - col_start < 4 {
        i += 1;
    }
    let letters = i - col_start;
    if letters == 0 || letters > 3 {
        return None;
    }
    if bytes.get(i) == Some(&b'$') {
        i += 1;
    }
    let row_start = i;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if i == row_start {
        return None;
    }

    let next = bytes.get(i).copied();
    if next.is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'?' | b'!')) {
        return None;
    }
    let mut lookahead = i;
    while bytes
        .get(lookahead)
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        lookahead += 1;
    }
    if bytes.get(lookahead) == Some(&b'(') {
        return None;
    }

    let addr = CellAddr::from_a1(&source[start..i])?;
    Some((addr, i))
}

const ERROR_LITERALS: &[(&str, Error)] = &[
    ("#DIV/0!", Error::Div0),
    ("#VALUE!", Error::Value),
    ("#REF!", Error::Ref),
    ("#NAME?", Error::Name),
    ("#NUM!", Error::Num),
    ("#N/A", Error::Na),
    ("#NULL!", Error::Null),
    ("#SPILL!", Error::Spill),
    ("#CALC!", Error::Calc),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_sheet_refs() {
        let tokens = tokenize("'My Sheet'!$B$2 + Sheet1!A1").unwrap();
        assert_eq!(tokens[0], Token::SheetName("My Sheet".into()));
        assert_eq!(tokens[1], Token::Bang);
        assert_eq!(tokens[2], Token::Cell(CellAddr::new(1, 1)));
        assert_eq!(tokens[3], Token::Plus);
        assert_eq!(tokens[4], Token::Ident("Sheet1".into()));
        assert_eq!(tokens[5], Token::Bang);
    }

    #[test]
    fn function_names_win_over_cell_lookalikes() {
        let tokens = tokenize("LOG10(100)").unwrap();
        assert_eq!(tokens[0], Token::Ident("LOG10".into()));
        assert_eq!(tokens[1], Token::LParen);

        let tokens = tokenize("LOG10").unwrap();
        assert_eq!(tokens[0], Token::Cell(CellAddr::new(9, 8508)));
    }

    #[test]
    fn tokenizes_operators_and_errors() {
        let tokens = tokenize("1<=2<>3>=4/0#N/A").unwrap();
        assert!(tokens.contains(&Token::Le));
        assert!(tokens.contains(&Token::Ne));
        assert!(tokens.contains(&Token::Ge));
        assert!(tokens.contains(&Token::Error(Error::Na)));
    }
}
