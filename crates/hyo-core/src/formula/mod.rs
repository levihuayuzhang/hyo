//! Excel formula lexer, parser and AST.
//!
//! The supported grammar is intentionally close to Excel's:
//!
//! ```text
//! comparison := concat (("=" | "<>" | "<" | "<=" | ">" | ">=") concat)*
//! concat     := additive ("&" additive)*
//! additive   := multiplicative (("+" | "-") multiplicative)*
//! multiplicative := power (("*" | "/") power)*
//! power      := unary ("^" unary)*
//! unary      := ("+" | "-" | "@") unary | postfix
//! postfix    := primary ("%")*
//! primary    := number | string | boolean | error | ref | range | call | "(" expr ")"
//! ```
//!
//! Excel gives unary minus higher precedence than `^`, so `=-2^2` is `4`.

use crate::addr::{CellAddr, CellRange};
use crate::value::{Error, Value};
use std::fmt::{self, Display, Formatter};

mod lexer;
mod parser;

pub use lexer::{FormulaError, Token};
use parser::Parser;

/// A reference to one cell, optionally on another sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRef {
    pub sheet: Option<String>,
    pub addr: CellAddr,
}

/// A rectangular range, optionally on another sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeRef {
    pub sheet: Option<String>,
    pub range: CellRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Ref(CellRef),
    Range(RangeRef),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Percent(Box<Expr>),
}

impl Expr {
    /// Visits every direct cell/range reference in the expression tree.
    /// References inside every function argument are visited, including
    /// branches that may not be evaluated at runtime; dependency tracking must
    /// be conservative.
    pub fn visit_refs(
        &self,
        on_ref: &mut impl FnMut(&CellRef),
        on_range: &mut impl FnMut(&RangeRef),
    ) {
        match self {
            Self::Literal(_) => {}
            Self::Ref(reference) => on_ref(reference),
            Self::Range(range) => on_range(range),
            Self::Unary(_, expr) | Self::Percent(expr) => expr.visit_refs(on_ref, on_range),
            Self::Binary(_, lhs, rhs) => {
                lhs.visit_refs(on_ref, on_range);
                rhs.visit_refs(on_ref, on_range);
            }
            Self::Call(_, args) => {
                for arg in args {
                    arg.visit_refs(on_ref, on_range);
                }
            }
        }
    }

    pub fn is_range(&self) -> bool {
        matches!(self, Self::Range(_))
    }
}

/// A parsed formula. `expr` is `None` when the source could not be parsed;
/// imported workbooks keep the source text and surface `error` instead of
/// silently dropping the formula.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub source: String,
    pub expr: Option<Expr>,
    pub error: Option<Error>,
    pub parse_error: Option<String>,
}

impl Formula {
    pub fn parse(source: &str) -> Result<Self, FormulaError> {
        let expr = Parser::new(source)?.parse()?;
        Ok(Self {
            source: source.to_owned(),
            expr: Some(expr),
            error: None,
            parse_error: None,
        })
    }

    pub fn invalid(source: &str, error: FormulaError, value_error: Error) -> Self {
        Self {
            source: source.to_owned(),
            expr: None,
            error: Some(value_error),
            parse_error: Some(error.to_string()),
        }
    }

    /// Source text including the leading `=`.
    pub fn input(&self) -> String {
        format!("={}", self.source)
    }
}

impl Display for Formula {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("=")?;
        f.write_str(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Expr {
        Formula::parse(source).unwrap().expr.unwrap()
    }

    fn number(expr: &Expr) -> f64 {
        match expr {
            Expr::Literal(Value::Number(value)) => *value,
            other => panic!("not a number: {other:?}"),
        }
    }

    #[test]
    fn precedence_follows_excel() {
        let Expr::Binary(BinaryOp::Add, lhs, rhs) = parse("1+2*3") else {
            panic!()
        };
        assert_eq!(number(&lhs), 1.0);
        let Expr::Binary(BinaryOp::Mul, mul_lhs, mul_rhs) = &*rhs else {
            panic!("multiplication must bind tighter than addition")
        };
        assert_eq!(number(mul_lhs), 2.0);
        assert_eq!(number(mul_rhs), 3.0);

        // Excel evaluates -2^2 as (-2)^2 = 4, unlike mathematical notation.
        let Expr::Binary(BinaryOp::Pow, base, exponent) = parse("-2^2") else {
            panic!()
        };
        assert!(matches!(*base, Expr::Unary(UnaryOp::Minus, _)));
        assert_eq!(number(&exponent), 2.0);

        let Expr::Binary(BinaryOp::Pow, _, exponent) = parse("2^-2") else {
            panic!()
        };
        assert!(matches!(*exponent, Expr::Unary(UnaryOp::Minus, _)));

        // Excel evaluates chained powers left-to-right: (2^3)^2 == 64.
        let Expr::Binary(BinaryOp::Pow, base, exponent) = parse("2^3^2") else {
            panic!()
        };
        assert!(matches!(*base, Expr::Binary(BinaryOp::Pow, _, _)));
        assert_eq!(number(&exponent), 2.0);
    }

    #[test]
    fn parses_refs_ranges_and_calls() {
        let expr = parse("SUM(A1:B2, Sheet2!C3, 4)");
        let Expr::Call(name, args) = expr else {
            panic!()
        };
        assert_eq!(name, "SUM");
        assert_eq!(args.len(), 3);
        let Expr::Range(range) = &args[0] else {
            panic!()
        };
        assert_eq!(range.range.to_a1(), "A1:B2");
        let Expr::Ref(reference) = &args[1] else {
            panic!()
        };
        assert_eq!(reference.sheet.as_deref(), Some("Sheet2"));
        assert_eq!(reference.addr, CellAddr::new(2, 2));
    }

    #[test]
    fn quoted_sheet_names() {
        let expr = parse("'Q1 Sales'!$B$7");
        let Expr::Ref(reference) = expr else { panic!() };
        assert_eq!(reference.sheet.as_deref(), Some("Q1 Sales"));
        assert_eq!(reference.addr, CellAddr::new(6, 1));
    }

    #[test]
    fn strings_double_quotes() {
        let Expr::Literal(Value::Text(text)) = parse("\"a\"\"b\"") else {
            panic!()
        };
        assert_eq!(&*text, "a\"b");
    }

    #[test]
    fn errors_have_offsets() {
        let error = Formula::parse("1+").unwrap_err();
        assert!(error.offset >= 2, "{error}");
    }
}
