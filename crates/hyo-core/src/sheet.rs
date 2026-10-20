//! Sheet storage: a sparse row-major cell map plus formula/literal parsing.

use crate::addr::{CellAddr, CellRange};
use crate::formula::{Formula, FormulaError};
use crate::value::{Error, Value, parse_number_text};
use std::collections::BTreeMap;

/// One cell. Literal and formula cells share storage; `formula` is `Some` for
/// any cell whose input started with `=`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// Exactly what the user typed (including the leading `=` for formulas).
    pub input: String,
    pub value: Value,
    pub formula: Option<Formula>,
}

impl Cell {
    pub fn literal(input: impl Into<String>, value: Value) -> Self {
        Self {
            input: input.into(),
            value,
            formula: None,
        }
    }

    pub fn formula(input: impl Into<String>, formula: Formula) -> Self {
        let value = formula.error.map(Value::Error).unwrap_or(Value::Empty);
        Self {
            input: input.into(),
            value,
            formula: Some(formula),
        }
    }

    /// Text shown in the formula bar.
    pub fn editor_text(&self) -> &str {
        &self.input
    }

    pub fn is_formula(&self) -> bool {
        self.formula.is_some()
    }
}

/// A worksheet. Cells are sparse: only non-empty cells are stored, so a full
/// column range (`A:A`) costs as many iterations as there are used cells.
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    name: String,
    cells: BTreeMap<CellAddr, Cell>,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cells: BTreeMap::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn cell(&self, addr: CellAddr) -> Option<&Cell> {
        self.cells.get(&addr)
    }

    pub fn cell_mut(&mut self, addr: CellAddr) -> Option<&mut Cell> {
        self.cells.get_mut(&addr)
    }

    pub fn value(&self, addr: CellAddr) -> Value {
        self.cells
            .get(&addr)
            .map(|cell| cell.value.clone())
            .unwrap_or(Value::Empty)
    }

    pub fn non_empty_len(&self) -> usize {
        self.cells.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellAddr, &Cell)> {
        self.cells.iter().map(|(addr, cell)| (*addr, cell))
    }

    pub fn used_bounds(&self) -> Option<CellRange> {
        let first = self.cells.keys().next()?;
        let last = self.cells.keys().next_back()?;
        Some(CellRange::new(*first, *last))
    }

    /// Stored cells inside `range`, in row-major order.
    pub fn range_cells(&self, range: CellRange) -> impl Iterator<Item = (CellAddr, &Cell)> {
        self.cells
            .range(range.start..=range.end)
            .filter(move |(addr, _)| range.contains(**addr))
            .map(|(addr, cell)| (*addr, cell))
    }

    pub fn set_input(&mut self, addr: CellAddr, input: &str) -> Result<(), FormulaError> {
        if input.is_empty() {
            self.cells.remove(&addr);
            return Ok(());
        }
        if let Some(source) = input.strip_prefix('=') {
            let formula = Formula::parse(source)?;
            self.cells.insert(addr, Cell::formula(input, formula));
            return Ok(());
        }

        let value = parse_literal(input);
        if value.is_empty() {
            self.cells.remove(&addr);
        } else {
            self.cells.insert(addr, Cell::literal(input, value));
        }
        Ok(())
    }

    /// Imports a formula from XLSX. A malformed formula is preserved as a
    /// visible `#VALUE!` cell rather than being dropped.
    pub fn set_formula_source(&mut self, addr: CellAddr, source: &str) {
        let source = source.strip_prefix('=').unwrap_or(source);
        let formula = match Formula::parse(source) {
            Ok(formula) => formula,
            Err(error) => Formula::invalid(source, error, Error::Value),
        };
        self.cells
            .insert(addr, Cell::formula(format!("={source}"), formula));
    }

    /// Imports a literal value from XLSX or another data source.
    pub fn set_value(&mut self, addr: CellAddr, value: Value) {
        if value.is_empty() {
            self.cells.remove(&addr);
            return;
        }
        let input = value.display();
        self.cells.insert(addr, Cell::literal(input, value));
    }

    pub fn set_error(&mut self, addr: CellAddr, error: Error) {
        self.set_value(addr, Value::Error(error));
    }

    pub fn clear(&mut self, addr: CellAddr) {
        self.cells.remove(&addr);
    }

    pub fn clear_range(&mut self, range: CellRange) {
        let keys: Vec<_> = self
            .cells
            .range(range.start..=range.end)
            .filter(|(addr, _)| range.contains(**addr))
            .map(|(addr, _)| *addr)
            .collect();
        for key in keys {
            self.cells.remove(&key);
        }
    }

    /// A compact row-major dump, useful for tests and debugging.
    pub fn to_tsv(&self) -> String {
        let Some(bounds) = self.used_bounds() else {
            return String::new();
        };
        let mut out = String::new();
        for row in bounds.start.row..=bounds.end.row {
            for col in bounds.start.col..=bounds.end.col {
                if col > bounds.start.col {
                    out.push('\t');
                }
                out.push_str(&self.value(CellAddr::new(row, col)).display());
            }
            out.push('\n');
        }
        out
    }
}

/// Parses the literal part of a cell input. Returns `Value::Empty` for an
/// empty string; the caller removes the cell in that case.
pub fn parse_literal(input: &str) -> Value {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Value::Empty;
    }
    if let Some(text) = input.strip_prefix('\'') {
        return Value::text(text);
    }
    if trimmed.eq_ignore_ascii_case("TRUE") {
        return Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("FALSE") {
        return Value::Bool(false);
    }
    if let Some(error) = parse_error_literal(trimmed) {
        return Value::Error(error);
    }
    if let Some(number) = parse_number_text(trimmed)
        && number.is_finite()
    {
        return Value::Number(number);
    }
    Value::text(input.to_owned())
}

pub fn parse_error_literal(text: &str) -> Option<Error> {
    match text.to_ascii_uppercase().as_str() {
        "#DIV/0!" => Some(Error::Div0),
        "#VALUE!" => Some(Error::Value),
        "#REF!" => Some(Error::Ref),
        "#NAME?" => Some(Error::Name),
        "#NUM!" => Some(Error::Num),
        "#N/A" => Some(Error::Na),
        "#NULL!" => Some(Error::Null),
        "#SPILL!" => Some(Error::Spill),
        "#CALC!" => Some(Error::Calc),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_detection() {
        assert_eq!(parse_literal("42"), Value::Number(42.0));
        assert_eq!(parse_literal(" 1,234.5 "), Value::Number(1234.5));
        assert_eq!(parse_literal("12%"), Value::Number(0.12));
        assert_eq!(parse_literal("true"), Value::Bool(true));
        assert_eq!(parse_literal("#DIV/0!"), Value::Error(Error::Div0));
        assert_eq!(parse_literal("'42"), Value::text("42"));
        assert_eq!(parse_literal("1e3"), Value::Number(1000.0));
        assert_eq!(parse_literal("hello"), Value::text("hello"));
    }

    #[test]
    fn formulas_and_literals_round_trip() {
        let mut sheet = Sheet::new("Sheet1");
        let a1 = CellAddr::new(0, 0);
        sheet.set_input(a1, "=1+2").unwrap();
        assert!(sheet.cell(a1).unwrap().is_formula());
        assert_eq!(sheet.cell(a1).unwrap().editor_text(), "=1+2");

        sheet.set_input(a1, "").unwrap();
        assert!(sheet.cell(a1).is_none());
    }
}
