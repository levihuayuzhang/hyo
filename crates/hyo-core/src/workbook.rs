//! Workbook, dependency graph and topological recalculation.

use crate::addr::{CellAddr, CellRange};
use crate::eval::{EvalStats, Evaluator};
use crate::formula::{Formula, FormulaError};
use crate::numeric::NumericBackend;
use crate::sheet::Sheet;
use crate::value::{Error, Value};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::{Duration, Instant};

/// Identifies a cell across the whole workbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellKey {
    pub sheet: usize,
    pub addr: CellAddr,
}

/// Result of one full recalculation.
#[derive(Debug, Clone, PartialEq)]
pub struct RecalcReport {
    pub formula_count: usize,
    pub cycle_count: usize,
    pub duration: Duration,
    pub stats: EvalStats,
}

impl RecalcReport {
    pub fn gpu_used(&self) -> bool {
        self.stats.gpu_calls > 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workbook {
    sheets: Vec<Sheet>,
    active: usize,
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Workbook {
    pub fn new() -> Self {
        Self {
            sheets: vec![Sheet::new("Sheet1")],
            active: 0,
        }
    }

    pub fn with_sheet_names(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut sheets: Vec<Sheet> = names.into_iter().map(Sheet::new).collect();
        if sheets.is_empty() {
            sheets.push(Sheet::new("Sheet1"));
        }
        Self { sheets, active: 0 }
    }

    pub fn sheets(&self) -> impl Iterator<Item = &Sheet> {
        self.sheets.iter()
    }

    pub fn sheet_count(&self) -> usize {
        self.sheets.len()
    }

    pub fn active_sheet(&self) -> usize {
        self.active
    }

    pub fn set_active_sheet(&mut self, index: usize) -> bool {
        if index < self.sheets.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    pub fn sheet(&self, index: usize) -> Option<&Sheet> {
        self.sheets.get(index)
    }

    pub fn sheet_mut(&mut self, index: usize) -> Option<&mut Sheet> {
        self.sheets.get_mut(index)
    }

    /// Case-insensitive sheet lookup, like Excel.
    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|sheet| sheet.name().eq_ignore_ascii_case(name.trim()))
    }

    pub fn add_sheet(&mut self, name: impl Into<String>) -> Result<usize, String> {
        let name = name.into();
        validate_sheet_name(&name)?;
        if self.sheet_index(&name).is_some() {
            return Err(format!("a sheet named {name:?} already exists"));
        }
        self.sheets.push(Sheet::new(name));
        Ok(self.sheets.len() - 1)
    }

    pub fn rename_sheet(&mut self, index: usize, name: impl Into<String>) -> Result<(), String> {
        let name = name.into();
        validate_sheet_name(&name)?;
        if self
            .sheet_index(&name)
            .is_some_and(|existing| existing != index)
        {
            return Err(format!("a sheet named {name:?} already exists"));
        }
        let sheet = self
            .sheets
            .get_mut(index)
            .ok_or_else(|| format!("no sheet at index {index}"))?;
        sheet.set_name(name);
        Ok(())
    }

    pub fn set_input(
        &mut self,
        sheet: usize,
        addr: CellAddr,
        input: &str,
    ) -> Result<(), FormulaError> {
        let sheet = self
            .sheets
            .get_mut(sheet)
            .ok_or_else(|| FormulaError::new("unknown sheet", 0))?;
        sheet.set_input(addr, input)
    }

    pub fn formula_count(&self) -> usize {
        self.sheets
            .iter()
            .flat_map(|sheet| sheet.iter())
            .filter(|(_, cell)| cell.is_formula())
            .count()
    }

    /// Recalculates every formula after rebuilding the dependency graph.
    ///
    /// Cyclic cells are assigned `#CIRC!`; the remaining acyclic formulas are
    /// still evaluated so a single mistake does not blank the worksheet.
    pub fn recalc(&mut self, backend: &dyn NumericBackend) -> RecalcReport {
        let started = Instant::now();

        let formula_keys: Vec<CellKey> = self
            .sheets
            .iter()
            .enumerate()
            .flat_map(|(sheet_index, sheet)| {
                sheet
                    .iter()
                    .filter(|(_, cell)| cell.is_formula())
                    .map(move |(addr, _)| CellKey {
                        sheet: sheet_index,
                        addr,
                    })
            })
            .collect();
        let formula_count = formula_keys.len();
        let formula_map: HashMap<CellKey, usize> = formula_keys
            .iter()
            .enumerate()
            .map(|(id, key)| (*key, id))
            .collect();

        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); formula_count];
        let mut indegree = vec![0usize; formula_count];
        for (id, key) in formula_keys.iter().enumerate() {
            let Some(cell) = self
                .sheets
                .get(key.sheet)
                .and_then(|sheet| sheet.cell(key.addr))
            else {
                continue;
            };
            let Some(formula) = &cell.formula else {
                continue;
            };
            let Some(expr) = &formula.expr else {
                continue;
            };

            let mut direct = HashSet::new();
            let mut ranged = HashSet::new();
            expr.visit_refs(
                &mut |reference| {
                    let sheet = match &reference.sheet {
                        Some(name) => self.sheet_index(name),
                        None => Some(key.sheet),
                    };
                    if let Some(sheet) = sheet
                        && let Some(precedent) = formula_map.get(&CellKey {
                            sheet,
                            addr: reference.addr,
                        })
                    {
                        direct.insert(*precedent);
                    }
                },
                &mut |range| {
                    let sheet = match &range.sheet {
                        Some(name) => self.sheet_index(name),
                        None => Some(key.sheet),
                    };
                    let Some(sheet) = sheet else {
                        return;
                    };
                    for (precedent, other) in formula_keys.iter().enumerate() {
                        if other.sheet == sheet && range.range.contains(other.addr) {
                            ranged.insert(precedent);
                        }
                    }
                },
            );
            direct.extend(ranged);

            for precedent in direct {
                dependents[precedent].push(id);
                indegree[id] += 1;
            }
        }

        let mut ready: BinaryHeap<Reverse<usize>> = indegree
            .iter()
            .enumerate()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| Reverse(id))
            .collect();
        let mut order = Vec::with_capacity(formula_count);
        while let Some(Reverse(id)) = ready.pop() {
            order.push(id);
            for &dependent in &dependents[id] {
                indegree[dependent] -= 1;
                if indegree[dependent] == 0 {
                    ready.push(Reverse(dependent));
                }
            }
        }
        let cycle_count = formula_count.saturating_sub(order.len());

        let mut computed: HashMap<CellKey, Value> = HashMap::with_capacity(formula_count);
        for key in &formula_keys {
            let seed = self
                .sheets
                .get(key.sheet)
                .and_then(|sheet| sheet.cell(key.addr))
                .and_then(|cell| cell.formula.as_ref())
                .and_then(|formula| formula.error)
                .map(Value::Error)
                .unwrap_or(Value::Error(Error::Circular));
            computed.insert(*key, seed);
        }

        let mut stats = EvalStats::default();
        for id in order {
            let key = formula_keys[id];
            let formula = self
                .sheets
                .get(key.sheet)
                .and_then(|sheet| sheet.cell(key.addr))
                .and_then(|cell| cell.formula.as_ref());
            let expr = formula.and_then(|formula| formula.expr.clone());
            let parse_error = formula.and_then(|formula| formula.error);
            let value = if let Some(expr) = expr {
                let mut evaluator = Evaluator::new(self, &computed, backend, key.sheet);
                let value = evaluator.eval(&expr);
                stats.merge(evaluator.stats);
                value
            } else {
                Value::Error(parse_error.unwrap_or(Error::Value))
            };
            computed.insert(key, value);
        }

        for key in &formula_keys {
            let value = computed
                .get(key)
                .cloned()
                .unwrap_or(Value::Error(Error::Circular));
            if let Some(cell) = self
                .sheets
                .get_mut(key.sheet)
                .and_then(|sheet| sheet.cell_mut(key.addr))
            {
                cell.value = value;
            }
        }

        RecalcReport {
            formula_count,
            cycle_count,
            duration: started.elapsed(),
            stats,
        }
    }

    pub fn recalculate(&mut self, backend: &dyn NumericBackend) -> RecalcReport {
        self.recalc(backend)
    }

    /// Substitutes values for the cells in a range (used for status-bar sums).
    pub fn range_values(&self, sheet: usize, range: CellRange) -> Vec<Value> {
        let Some(sheet) = self.sheets.get(sheet) else {
            return Vec::new();
        };
        sheet
            .range_cells(range)
            .map(|(_, cell)| cell.value.clone())
            .collect()
    }
}

fn validate_sheet_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("sheet name cannot be empty".to_owned());
    }
    if name.chars().count() > 31 {
        return Err("sheet name cannot exceed 31 characters".to_owned());
    }
    if name
        .chars()
        .any(|c| matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
    {
        return Err("sheet name contains a character Excel forbids".to_owned());
    }
    Ok(())
}

/// Convenience for building a formula cell without going through user input.
pub fn formula_cell(formula: Formula) -> crate::sheet::Cell {
    crate::sheet::Cell::formula(format!("={}", formula.source), formula)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::CpuBackend;

    fn set(workbook: &mut Workbook, sheet: usize, a1: &str, input: &str) {
        let addr = CellAddr::from_a1(a1).unwrap();
        workbook.set_input(sheet, addr, input).unwrap();
    }

    fn value(workbook: &Workbook, sheet: usize, a1: &str) -> Value {
        workbook
            .sheet(sheet)
            .unwrap()
            .value(CellAddr::from_a1(a1).unwrap())
    }

    #[test]
    fn evaluates_a_dependency_chain() {
        let mut workbook = Workbook::new();
        set(&mut workbook, 0, "A1", "2");
        set(&mut workbook, 0, "A2", "=A1*3");
        set(&mut workbook, 0, "A3", "=A2+1");
        let report = workbook.recalc(&CpuBackend);
        assert_eq!(report.formula_count, 2);
        assert_eq!(report.cycle_count, 0);
        assert_eq!(value(&workbook, 0, "A2"), Value::Number(6.0));
        assert_eq!(value(&workbook, 0, "A3"), Value::Number(7.0));
    }

    #[test]
    fn detects_cycles_without_losing_other_formulas() {
        let mut workbook = Workbook::new();
        set(&mut workbook, 0, "A1", "=A2+1");
        set(&mut workbook, 0, "A2", "=A1+1");
        set(&mut workbook, 0, "B1", "=1+1");
        let report = workbook.recalc(&CpuBackend);
        assert_eq!(report.cycle_count, 2);
        assert_eq!(value(&workbook, 0, "A1"), Value::Error(Error::Circular));
        assert_eq!(value(&workbook, 0, "A2"), Value::Error(Error::Circular));
        assert_eq!(value(&workbook, 0, "B1"), Value::Number(2.0));
    }

    #[test]
    fn cross_sheet_ranges() {
        let mut workbook = Workbook::with_sheet_names(["Data", "Summary"]);
        set(&mut workbook, 0, "A1", "1");
        set(&mut workbook, 0, "A2", "2");
        set(&mut workbook, 0, "A3", "3");
        set(&mut workbook, 1, "B2", "=SUM(Data!A1:A3)");
        workbook.recalc(&CpuBackend);
        assert_eq!(value(&workbook, 1, "B2"), Value::Number(6.0));
    }

    #[test]
    fn aggregate_ignores_text_in_ranges() {
        let mut workbook = Workbook::new();
        set(&mut workbook, 0, "A1", "10");
        set(&mut workbook, 0, "A2", "hello");
        set(&mut workbook, 0, "A3", "TRUE");
        set(&mut workbook, 0, "A4", "20");
        set(&mut workbook, 0, "B1", "=SUM(A1:A4)");
        set(&mut workbook, 0, "B2", "=COUNT(A1:A4)");
        set(&mut workbook, 0, "B3", "=AVERAGE(A1:A4)");
        workbook.recalc(&CpuBackend);
        assert_eq!(value(&workbook, 0, "B1"), Value::Number(30.0));
        assert_eq!(value(&workbook, 0, "B2"), Value::Number(2.0));
        assert_eq!(value(&workbook, 0, "B3"), Value::Number(15.0));
    }

    #[test]
    fn excel_error_values_propagate() {
        let mut workbook = Workbook::new();
        set(&mut workbook, 0, "A1", "=1/0");
        set(&mut workbook, 0, "A2", "=A1+1");
        workbook.recalc(&CpuBackend);
        assert_eq!(value(&workbook, 0, "A1"), Value::Error(Error::Div0));
        assert_eq!(value(&workbook, 0, "A2"), Value::Error(Error::Div0));
    }
}
