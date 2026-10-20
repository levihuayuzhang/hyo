//! Recursive-descent parser for the formula grammar.

use super::lexer::{FormulaError, Token, tokenize_spanned};
use super::{BinaryOp, CellRef, Expr, RangeRef, UnaryOp};
use crate::addr::CellRange;
use crate::value::Value;

pub(crate) struct Parser {
    tokens: Vec<(Token, usize)>,
    pos: usize,
}

impl Parser {
    pub(crate) fn new(source: &str) -> Result<Self, FormulaError> {
        Ok(Self {
            tokens: tokenize_spanned(source)?,
            pos: 0,
        })
    }

    pub(crate) fn parse(&mut self) -> Result<Expr, FormulaError> {
        if matches!(self.peek(), Token::Eof) {
            return Err(self.error("empty formula"));
        }
        let expr = self.parse_comparison()?;
        if !matches!(self.peek(), Token::Eof) {
            return Err(self.error(format!("unexpected token {}", self.peek())));
        }
        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr, FormulaError> {
        let mut lhs = self.parse_concat()?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinaryOp::Eq,
                Token::Ne => BinaryOp::Ne,
                Token::Lt => BinaryOp::Lt,
                Token::Le => BinaryOp::Le,
                Token::Gt => BinaryOp::Gt,
                Token::Ge => BinaryOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_concat()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_concat(&mut self) -> Result<Expr, FormulaError> {
        let mut lhs = self.parse_additive()?;
        while matches!(self.peek(), Token::Amp) {
            self.pos += 1;
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary(BinaryOp::Concat, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, FormulaError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, FormulaError> {
        let mut lhs = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinaryOp::Mul,
                Token::Slash => BinaryOp::Div,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_power()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_power(&mut self) -> Result<Expr, FormulaError> {
        let mut lhs = self.parse_unary()?;
        // Excel evaluates chained powers left-to-right: 2^3^2 == (2^3)^2 == 64.
        while matches!(self.peek(), Token::Caret) {
            self.pos += 1;
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(BinaryOp::Pow, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, FormulaError> {
        match self.peek() {
            Token::Plus => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Plus, Box::new(self.parse_unary()?)))
            }
            Token::Minus => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Minus, Box::new(self.parse_unary()?)))
            }
            // `@` is Excel's implicit-intersection marker. For scalar formulas
            // it is a no-op; array semantics are intentionally out of scope.
            Token::At => {
                self.pos += 1;
                self.parse_unary()
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, FormulaError> {
        let mut expr = self.parse_primary()?;
        while matches!(self.peek(), Token::Percent) {
            self.pos += 1;
            expr = Expr::Percent(Box::new(expr));
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, FormulaError> {
        let (token, _) = self.current().clone();
        match token {
            Token::Number(value) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Number(value)))
            }
            Token::Text(value) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::text(value)))
            }
            Token::Error(error) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Error(error)))
            }
            Token::Cell(addr) => {
                self.pos += 1;
                self.finish_ref(None, addr)
            }
            Token::SheetName(sheet) => {
                self.pos += 1;
                self.expect(Token::Bang, "expected ! after sheet name")?;
                let addr = self.expect_cell()?;
                self.finish_ref(Some(sheet), addr)
            }
            Token::Ident(name) => {
                self.pos += 1;
                if matches!(self.peek(), Token::Bang) {
                    self.pos += 1;
                    let addr = self.expect_cell()?;
                    return self.finish_ref(Some(name), addr);
                }
                if matches!(self.peek(), Token::LParen) {
                    return self.parse_call(name);
                }
                if name.eq_ignore_ascii_case("TRUE") {
                    Ok(Expr::Literal(Value::Bool(true)))
                } else if name.eq_ignore_ascii_case("FALSE") {
                    Ok(Expr::Literal(Value::Bool(false)))
                } else {
                    // Named ranges are not implemented. Treat the bare name as
                    // a zero-argument unknown function so evaluation yields the
                    // Excel-compatible #NAME? value instead of failing to load.
                    Ok(Expr::Call(normalize_name(&name), Vec::new()))
                }
            }
            Token::LParen => {
                self.pos += 1;
                let expr = self.parse_comparison()?;
                self.expect(Token::RParen, "expected )")?;
                Ok(expr)
            }
            other => Err(self.error(format!("unexpected token {other}"))),
        }
    }

    fn parse_call(&mut self, name: String) -> Result<Expr, FormulaError> {
        self.expect(Token::LParen, "expected (")?;
        let mut args = Vec::new();
        if matches!(self.peek(), Token::RParen) {
            self.pos += 1;
            return Ok(Expr::Call(normalize_name(&name), args));
        }
        loop {
            args.push(self.parse_comparison()?);
            match self.peek() {
                Token::Comma | Token::Semicolon => {
                    self.pos += 1;
                    // Excel tolerates a trailing separator in some UI paths;
                    // reject it here so typos are visible.
                    if matches!(self.peek(), Token::RParen) {
                        return Err(self.error("expected an argument after separator"));
                    }
                }
                Token::RParen => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error(format!("expected , or ), found {}", self.peek()))),
            }
        }
        Ok(Expr::Call(normalize_name(&name), args))
    }

    /// Completes a reference. If a colon follows, consumes the second endpoint
    /// and produces a range.
    fn finish_ref(
        &mut self,
        sheet: Option<String>,
        addr: crate::addr::CellAddr,
    ) -> Result<Expr, FormulaError> {
        let start = CellRef {
            sheet: sheet.clone(),
            addr,
        };
        if !matches!(self.peek(), Token::Colon) {
            return Ok(Expr::Ref(start));
        }
        self.pos += 1;

        let (end_sheet, end_addr) = match self.peek().clone() {
            Token::Cell(addr) => {
                self.pos += 1;
                (None, addr)
            }
            Token::SheetName(name) => {
                self.pos += 1;
                self.expect(Token::Bang, "expected ! after sheet name")?;
                (Some(name), self.expect_cell()?)
            }
            Token::Ident(name) => {
                self.pos += 1;
                self.expect(Token::Bang, "expected ! after sheet name")?;
                (Some(name), self.expect_cell()?)
            }
            other => return Err(self.error(format!("expected range endpoint, found {other}"))),
        };

        if let (Some(a), Some(b)) = (&sheet, &end_sheet)
            && !a.eq_ignore_ascii_case(b)
        {
            return Err(self.error("3-D references across sheets are not supported"));
        }
        Ok(Expr::Range(RangeRef {
            sheet: sheet.or(end_sheet),
            range: CellRange::new(addr, end_addr),
        }))
    }

    fn expect_cell(&mut self) -> Result<crate::addr::CellAddr, FormulaError> {
        match self.peek().clone() {
            Token::Cell(addr) => {
                self.pos += 1;
                Ok(addr)
            }
            other => Err(self.error(format!("expected a cell reference, found {other}"))),
        }
    }

    fn expect(&mut self, token: Token, message: &str) -> Result<(), FormulaError> {
        if self.peek() == &token {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn current(&self) -> &(Token, usize) {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek(&self) -> &Token {
        &self.current().0
    }

    fn error(&self, message: impl Into<String>) -> FormulaError {
        FormulaError::new(message, self.current().1)
    }
}

fn normalize_name(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    upper.strip_prefix("_XLFN.").unwrap_or(&upper).to_owned()
}
