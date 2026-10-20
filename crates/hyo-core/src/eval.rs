//! Formula evaluator.
//!
//! Values are binary64 throughout, matching Excel's cell storage. Range
//! aggregations are delegated to a [`NumericBackend`] so a CUDA implementation
//! can replace the CPU loop for large inputs without changing formula
//! semantics.

use crate::addr::{CellAddr, CellRange};
use crate::formula::{BinaryOp, CellRef, Expr, RangeRef, UnaryOp};
use crate::numeric::{Aggregate, BackendKind, NumericBackend};
use crate::value::{Error, Value, parse_number_text};
use crate::workbook::{CellKey, Workbook};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Counters collected during one recalculation. The UI uses these to expose
/// whether a range really went through the GPU path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvalStats {
    pub aggregate_calls: usize,
    pub numbers_scanned: usize,
    pub gpu_calls: usize,
    pub dot_products: usize,
    pub last_backend: Option<BackendKind>,
}

impl EvalStats {
    pub fn merge(&mut self, other: Self) {
        self.aggregate_calls += other.aggregate_calls;
        self.numbers_scanned += other.numbers_scanned;
        self.gpu_calls += other.gpu_calls;
        self.dot_products += other.dot_products;
        self.last_backend = other.last_backend.or(self.last_backend);
    }
}

pub(crate) struct Evaluator<'a> {
    workbook: &'a Workbook,
    computed: &'a HashMap<CellKey, Value>,
    backend: &'a dyn NumericBackend,
    current_sheet: usize,
    pub stats: EvalStats,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(
        workbook: &'a Workbook,
        computed: &'a HashMap<CellKey, Value>,
        backend: &'a dyn NumericBackend,
        current_sheet: usize,
    ) -> Self {
        Self {
            workbook,
            computed,
            backend,
            current_sheet,
            stats: EvalStats::default(),
        }
    }

    pub(crate) fn eval(&mut self, expr: &Expr) -> Value {
        self.eval_expr(expr)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::Literal(value) => value.clone(),
            Expr::Ref(reference) => match self.lookup_ref(reference) {
                Ok(value) => value,
                Err(error) => Value::Error(error),
            },
            Expr::Range(_) => Value::Error(Error::Value),
            Expr::Unary(op, inner) => {
                let value = match self.number(inner) {
                    Ok(value) => value,
                    Err(error) => return Value::Error(error),
                };
                let value = match op {
                    UnaryOp::Plus => value,
                    UnaryOp::Minus => -value,
                };
                finite(value)
            }
            Expr::Percent(inner) => {
                let value = match self.number(inner) {
                    Ok(value) => value,
                    Err(error) => return Value::Error(error),
                };
                finite(value / 100.0)
            }
            Expr::Binary(op, lhs, rhs) => self.eval_binary(*op, lhs, rhs),
            Expr::Call(name, args) => self.eval_call(name, args),
        }
    }

    fn eval_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Value {
        if matches!(op, BinaryOp::Concat) {
            let lhs = match self.eval_expr(lhs).to_text() {
                Ok(value) => value,
                Err(error) => return Value::Error(error),
            };
            let rhs = match self.eval_expr(rhs).to_text() {
                Ok(value) => value,
                Err(error) => return Value::Error(error),
            };
            return Value::text(format!("{lhs}{rhs}"));
        }

        if matches!(
            op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
        ) {
            let lhs = self.eval_expr(lhs);
            let rhs = self.eval_expr(rhs);
            return match compare_values(&lhs, &rhs) {
                Ok(ordering) => {
                    let result = match op {
                        BinaryOp::Eq => ordering == Ordering::Equal,
                        BinaryOp::Ne => ordering != Ordering::Equal,
                        BinaryOp::Lt => ordering == Ordering::Less,
                        BinaryOp::Le => ordering != Ordering::Greater,
                        BinaryOp::Gt => ordering == Ordering::Greater,
                        BinaryOp::Ge => ordering != Ordering::Less,
                        _ => unreachable!(),
                    };
                    Value::Bool(result)
                }
                Err(error) => Value::Error(error),
            };
        }

        let lhs = match self.number(lhs) {
            Ok(value) => value,
            Err(error) => return Value::Error(error),
        };
        let rhs = match self.number(rhs) {
            Ok(value) => value,
            Err(error) => return Value::Error(error),
        };

        let result = match op {
            BinaryOp::Add => lhs + rhs,
            BinaryOp::Sub => lhs - rhs,
            BinaryOp::Mul => lhs * rhs,
            BinaryOp::Div => {
                if rhs == 0.0 {
                    return Value::Error(Error::Div0);
                }
                lhs / rhs
            }
            BinaryOp::Pow => match excel_pow(lhs, rhs) {
                Ok(value) => value,
                Err(error) => return Value::Error(error),
            },
            _ => unreachable!(),
        };
        finite(result)
    }

    fn eval_call(&mut self, name: &str, args: &[Expr]) -> Value {
        macro_rules! tryv {
            ($value:expr) => {
                match $value {
                    Ok(value) => value,
                    Err(error) => return Value::Error(error),
                }
            };
        }

        match name {
            "IF" => {
                if !(2..=3).contains(&args.len()) {
                    return Value::Error(Error::Value);
                }
                let condition = self.eval_expr(&args[0]);
                let condition = tryv!(condition.coerce_bool());
                if condition {
                    self.eval_expr(&args[1])
                } else if let Some(otherwise) = args.get(2) {
                    self.eval_expr(otherwise)
                } else {
                    Value::Bool(false)
                }
            }
            "IFERROR" | "IFNA" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let value = self.eval_expr(&args[0]);
                let replace = match value.error() {
                    Some(Error::Na) if name == "IFNA" => true,
                    Some(_) => name == "IFERROR",
                    None => false,
                };
                if replace {
                    self.eval_expr(&args[1])
                } else {
                    value
                }
            }
            "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "PRODUCT" => {
                let numbers = tryv!(self.collect_numbers(args));
                let aggregate = self.aggregate(&numbers);
                match name {
                    "SUM" => finite(aggregate.sum),
                    "AVERAGE" => match aggregate.mean() {
                        Some(value) => finite(value),
                        None => Value::Error(Error::Div0),
                    },
                    "MIN" => Value::Number(aggregate.min.unwrap_or(0.0)),
                    "MAX" => Value::Number(aggregate.max.unwrap_or(0.0)),
                    "COUNT" => Value::Number(aggregate.count as f64),
                    "PRODUCT" => {
                        if numbers.is_empty() {
                            Value::Number(0.0)
                        } else {
                            finite(numbers.iter().product())
                        }
                    }
                    _ => unreachable!(),
                }
            }
            "COUNTA" => {
                let mut count = 0u64;
                for arg in args {
                    match arg {
                        Expr::Range(range) => {
                            let (sheet, range) = tryv!(self.resolve_range(range));
                            count += self
                                .workbook
                                .sheet(sheet)
                                .map(|sheet| sheet.range_cells(range).count() as u64)
                                .unwrap_or(0);
                        }
                        Expr::Ref(reference) => {
                            let value = tryv!(self.lookup_ref(reference));
                            count += u64::from(!value.is_empty());
                        }
                        _ => {
                            let value = self.eval_expr(arg);
                            if value.is_error() {
                                return value;
                            }
                            count += u64::from(!value.is_empty());
                        }
                    }
                }
                Value::Number(count as f64)
            }
            "COUNTBLANK" => {
                let mut count = 0u64;
                for arg in args {
                    match arg {
                        Expr::Range(range) => {
                            let (sheet, range) = tryv!(self.resolve_range(range));
                            let non_empty = self
                                .workbook
                                .sheet(sheet)
                                .map(|sheet| sheet.range_cells(range).count() as u64)
                                .unwrap_or(0);
                            count += range.cell_count().saturating_sub(non_empty);
                        }
                        Expr::Ref(reference) => {
                            let value = tryv!(self.lookup_ref(reference));
                            count += u64::from(value.is_empty());
                        }
                        _ => {
                            let value = self.eval_expr(arg);
                            if value.is_error() {
                                return value;
                            }
                            count += u64::from(value.is_empty());
                        }
                    }
                }
                Value::Number(count as f64)
            }
            "SUMPRODUCT" => self.eval_sumproduct(args),
            "AND" | "OR" => {
                let mut seen = false;
                let mut result = name == "AND";
                for arg in args {
                    let values = tryv!(self.logical_values(arg));
                    for value in values {
                        seen = true;
                        if name == "AND" {
                            result &= value;
                        } else {
                            result |= value;
                        }
                    }
                }
                if !seen {
                    Value::Error(Error::Value)
                } else {
                    Value::Bool(result)
                }
            }
            "NOT" => {
                if args.len() != 1 {
                    return Value::Error(Error::Value);
                }
                let value = tryv!(self.eval_expr(&args[0]).coerce_bool());
                Value::Bool(!value)
            }
            "TRUE" => Value::Bool(true),
            "FALSE" => Value::Bool(false),
            "NA" => Value::Error(Error::Na),
            "ABS" => self.unary_math(args, f64::abs),
            "SIGN" => self.unary_math(args, |x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }),
            "INT" => self.unary_math(args, f64::floor),
            "TRUNC" => self.unary_math(args, f64::trunc),
            "SQRT" => {
                let value = tryv!(self.number_arg(args, 0, 1));
                if value < 0.0 {
                    Value::Error(Error::Num)
                } else {
                    finite(value.sqrt())
                }
            }
            "EXP" => {
                let value = tryv!(self.number_arg(args, 0, 1));
                finite(value.exp())
            }
            "LN" => {
                let value = tryv!(self.number_arg(args, 0, 1));
                if value <= 0.0 {
                    Value::Error(Error::Num)
                } else {
                    finite(value.ln())
                }
            }
            "LOG10" => {
                let value = tryv!(self.number_arg(args, 0, 1));
                if value <= 0.0 {
                    Value::Error(Error::Num)
                } else {
                    finite(value.log10())
                }
            }
            "LOG" => {
                if !(1..=2).contains(&args.len()) {
                    return Value::Error(Error::Value);
                }
                let value = tryv!(self.number(&args[0]));
                let base = if args.len() == 2 {
                    tryv!(self.number(&args[1]))
                } else {
                    10.0
                };
                if value <= 0.0 || base <= 0.0 || base == 1.0 {
                    Value::Error(Error::Num)
                } else {
                    finite(value.log(base))
                }
            }
            "POWER" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let base = tryv!(self.number(&args[0]));
                let exponent = tryv!(self.number(&args[1]));
                match excel_pow(base, exponent) {
                    Ok(value) => finite(value),
                    Err(error) => Value::Error(error),
                }
            }
            "MOD" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let number = tryv!(self.number(&args[0]));
                let divisor = tryv!(self.number(&args[1]));
                if divisor == 0.0 {
                    Value::Error(Error::Div0)
                } else {
                    finite(number - divisor * (number / divisor).floor())
                }
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let value = tryv!(self.number(&args[0]));
                let digits = tryv!(self.number(&args[1]));
                let digits = digits.trunc();
                if !(-308.0..=308.0).contains(&digits) {
                    return Value::Error(Error::Num);
                }
                let digits = digits as i32;
                let rounded = match round_to(value, digits, name) {
                    Some(value) => value,
                    None => return Value::Error(Error::Num),
                };
                finite(rounded)
            }
            "SIN" => self.trig(args, f64::sin),
            "COS" => self.trig(args, f64::cos),
            "TAN" => self.trig(args, f64::tan),
            "ASIN" => self.trig_checked(args, f64::asin, |x| (-1.0..=1.0).contains(&x)),
            "ACOS" => self.trig_checked(args, f64::acos, |x| (-1.0..=1.0).contains(&x)),
            "ATAN" => self.trig(args, f64::atan),
            "ATAN2" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let x = tryv!(self.number(&args[0]));
                let y = tryv!(self.number(&args[1]));
                finite(y.atan2(x))
            }
            "DEGREES" => self.unary_math(args, f64::to_degrees),
            "RADIANS" => self.unary_math(args, f64::to_radians),
            "PI" => Value::Number(std::f64::consts::PI),
            "LEN" => {
                let text = tryv!(self.text_arg(args, 0, 1));
                Value::Number(text.chars().count() as f64)
            }
            "LEFT" => {
                if !(1..=2).contains(&args.len()) {
                    return Value::Error(Error::Value);
                }
                let text = tryv!(self.text(&args[0]));
                let count = if args.len() == 2 {
                    let count = tryv!(self.number(&args[1]));
                    if count < 0.0 {
                        return Value::Error(Error::Value);
                    }
                    count as usize
                } else {
                    1
                };
                Value::text(text.chars().take(count).collect::<String>())
            }
            "RIGHT" => {
                if !(1..=2).contains(&args.len()) {
                    return Value::Error(Error::Value);
                }
                let text = tryv!(self.text(&args[0]));
                let count = if args.len() == 2 {
                    let count = tryv!(self.number(&args[1]));
                    if count < 0.0 {
                        return Value::Error(Error::Value);
                    }
                    count as usize
                } else {
                    1
                };
                let chars: Vec<char> = text.chars().collect();
                let start = chars.len().saturating_sub(count);
                Value::text(chars[start..].iter().collect::<String>())
            }
            "MID" => {
                if args.len() != 3 {
                    return Value::Error(Error::Value);
                }
                let text = tryv!(self.text(&args[0]));
                let start = tryv!(self.number(&args[1]));
                let count = tryv!(self.number(&args[2]));
                if start < 1.0 || count < 0.0 {
                    return Value::Error(Error::Value);
                }
                let start = start as usize - 1;
                let count = count as usize;
                Value::text(text.chars().skip(start).take(count).collect::<String>())
            }
            "UPPER" => {
                let text = tryv!(self.text_arg(args, 0, 1));
                Value::text(text.to_uppercase())
            }
            "LOWER" => {
                let text = tryv!(self.text_arg(args, 0, 1));
                Value::text(text.to_lowercase())
            }
            "TRIM" => {
                let text = tryv!(self.text_arg(args, 0, 1));
                Value::text(text.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            "REPT" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let text = tryv!(self.text(&args[0]));
                let count = tryv!(self.number(&args[1]));
                if count < 0.0 {
                    return Value::Error(Error::Value);
                }
                let count = count as usize;
                if text.len().saturating_mul(count) > 32_767 {
                    Value::Error(Error::Value)
                } else {
                    Value::text(text.repeat(count))
                }
            }
            "EXACT" => {
                if args.len() != 2 {
                    return Value::Error(Error::Value);
                }
                let lhs = tryv!(self.text(&args[0]));
                let rhs = tryv!(self.text(&args[1]));
                Value::Bool(lhs == rhs)
            }
            "CONCAT" | "CONCATENATE" => {
                let mut out = String::new();
                for arg in args {
                    match arg {
                        Expr::Range(range) => {
                            let (sheet, range) = tryv!(self.resolve_range(range));
                            if let Some(sheet) = self.workbook.sheet(sheet) {
                                for (addr, _) in sheet.range_cells(range) {
                                    let value = lookup_value(
                                        self.workbook,
                                        self.computed,
                                        sheet_index(self.workbook, sheet.name())
                                            .unwrap_or(self.current_sheet),
                                        addr,
                                    );
                                    match value.to_text() {
                                        Ok(text) => out.push_str(&text),
                                        Err(error) => return Value::Error(error),
                                    }
                                }
                            }
                        }
                        _ => match self.eval_expr(arg).to_text() {
                            Ok(text) => out.push_str(&text),
                            Err(error) => return Value::Error(error),
                        },
                    }
                }
                if out.chars().count() > 32_767 {
                    Value::Error(Error::Value)
                } else {
                    Value::text(out)
                }
            }
            "VALUE" => {
                let text = tryv!(self.text_arg(args, 0, 1));
                match parse_number_text(&text) {
                    Some(value) => Value::Number(value),
                    None => Value::Error(Error::Value),
                }
            }
            "ISBLANK" => self.predicate(args, |value| value.is_empty()),
            "ISNUMBER" => self.predicate(args, |value| matches!(value, Value::Number(_))),
            "ISTEXT" => self.predicate(args, |value| matches!(value, Value::Text(_))),
            "ISLOGICAL" => self.predicate(args, |value| matches!(value, Value::Bool(_))),
            "ISERROR" => self.predicate(args, |value| value.is_error()),
            "ISERR" => self.predicate(
                args,
                |value| matches!(value.error(), Some(error) if error != Error::Na),
            ),
            "ISNA" => self.predicate(args, |value| value.error() == Some(Error::Na)),
            _ => Value::Error(Error::Name),
        }
    }

    fn eval_sumproduct(&mut self, args: &[Expr]) -> Value {
        if args.is_empty() {
            return Value::Error(Error::Value);
        }
        let mut columns: Vec<Vec<f64>> = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                Expr::Range(range) => {
                    let (sheet, range) = match self.resolve_range(range) {
                        Ok(value) => value,
                        Err(error) => return Value::Error(error),
                    };
                    if range.cell_count() > 10_000_000 {
                        return Value::Error(Error::Num);
                    }
                    let cols = range.cols() as usize;
                    let mut values = vec![0.0; range.cell_count() as usize];
                    if let Some(worksheet) = self.workbook.sheet(sheet) {
                        for (addr, _) in worksheet.range_cells(range) {
                            let value = lookup_value(self.workbook, self.computed, sheet, addr);
                            if let Some(error) = value.error() {
                                return Value::Error(error);
                            }
                            let index = (addr.row - range.start.row) as usize * cols
                                + (addr.col - range.start.col) as usize;
                            values[index] = value.coerce_number().unwrap_or(0.0);
                        }
                    }
                    columns.push(values);
                }
                _ => {
                    let value = self.eval_expr(arg);
                    if let Some(error) = value.error() {
                        return Value::Error(error);
                    }
                    columns.push(vec![value.coerce_number().unwrap_or(0.0)]);
                }
            }
        }
        let length = columns[0].len();
        if columns.iter().any(|column| column.len() != length) {
            return Value::Error(Error::Value);
        }
        let mut product = vec![1.0; length];
        for column in &columns {
            for (slot, value) in product.iter_mut().zip(column) {
                *slot *= value;
            }
        }
        let ones = vec![1.0; length];
        let dot = self.backend.dot(&product, &ones);
        self.stats.dot_products += 1;
        self.stats.last_backend = Some(dot.backend);
        if dot.backend == BackendKind::Cuda {
            self.stats.gpu_calls += 1;
        }
        finite(dot.value)
    }

    fn collect_numbers(&mut self, args: &[Expr]) -> Result<Vec<f64>, Error> {
        let mut numbers = Vec::new();
        for arg in args {
            match arg {
                Expr::Range(range) => {
                    let (sheet, range) = self.resolve_range(range)?;
                    let Some(worksheet) = self.workbook.sheet(sheet) else {
                        return Err(Error::Ref);
                    };
                    for (addr, _) in worksheet.range_cells(range) {
                        let value = lookup_value(self.workbook, self.computed, sheet, addr);
                        match value {
                            Value::Number(number) => numbers.push(number),
                            Value::Error(error) => return Err(error),
                            _ => {}
                        }
                    }
                }
                Expr::Ref(_) => {
                    let value = self.eval_expr(arg);
                    match value {
                        Value::Number(number) => numbers.push(number),
                        Value::Error(error) => return Err(error),
                        _ => {}
                    }
                }
                _ => {
                    let value = self.eval_expr(arg);
                    if let Value::Error(error) = value {
                        return Err(error);
                    }
                    numbers.push(value.coerce_number()?);
                }
            }
        }
        Ok(numbers)
    }

    fn logical_values(&mut self, arg: &Expr) -> Result<Vec<bool>, Error> {
        match arg {
            Expr::Range(range) => {
                let (sheet, range) = self.resolve_range(range)?;
                let Some(worksheet) = self.workbook.sheet(sheet) else {
                    return Err(Error::Ref);
                };
                let mut values = Vec::new();
                for (addr, _) in worksheet.range_cells(range) {
                    match lookup_value(self.workbook, self.computed, sheet, addr) {
                        Value::Bool(value) => values.push(value),
                        Value::Number(value) => values.push(value != 0.0),
                        Value::Error(error) => return Err(error),
                        _ => {}
                    }
                }
                Ok(values)
            }
            _ => Ok(vec![self.eval_expr(arg).coerce_bool()?]),
        }
    }

    fn aggregate(&mut self, numbers: &[f64]) -> Aggregate {
        let aggregate = self.backend.aggregate(numbers);
        self.stats.aggregate_calls += 1;
        self.stats.numbers_scanned += numbers.len();
        self.stats.last_backend = Some(aggregate.backend);
        if aggregate.backend == BackendKind::Cuda {
            self.stats.gpu_calls += 1;
        }
        aggregate
    }

    fn unary_math(&mut self, args: &[Expr], f: impl Fn(f64) -> f64) -> Value {
        if args.len() != 1 {
            return Value::Error(Error::Value);
        }
        let value = match self.number(&args[0]) {
            Ok(value) => value,
            Err(error) => return Value::Error(error),
        };
        finite(f(value))
    }

    fn trig(&mut self, args: &[Expr], f: impl Fn(f64) -> f64) -> Value {
        self.unary_math(args, f)
    }

    fn trig_checked(
        &mut self,
        args: &[Expr],
        f: impl Fn(f64) -> f64,
        domain: impl Fn(f64) -> bool,
    ) -> Value {
        if args.len() != 1 {
            return Value::Error(Error::Value);
        }
        let value = match self.number(&args[0]) {
            Ok(value) => value,
            Err(error) => return Value::Error(error),
        };
        if !domain(value) {
            return Value::Error(Error::Num);
        }
        finite(f(value))
    }

    fn predicate(&mut self, args: &[Expr], predicate: impl Fn(&Value) -> bool) -> Value {
        if args.len() != 1 {
            return Value::Error(Error::Value);
        }
        let value = self.eval_expr(&args[0]);
        Value::Bool(predicate(&value))
    }

    fn number_arg(&mut self, args: &[Expr], index: usize, arity: usize) -> Result<f64, Error> {
        if args.len() != arity {
            return Err(Error::Value);
        }
        self.number(&args[index])
    }

    fn number(&mut self, expr: &Expr) -> Result<f64, Error> {
        let value = self.eval_expr(expr);
        let number = value.coerce_number()?;
        if number.is_finite() {
            Ok(number)
        } else {
            Err(Error::Num)
        }
    }

    fn text(&mut self, expr: &Expr) -> Result<String, Error> {
        self.eval_expr(expr).to_text()
    }

    fn text_arg(&mut self, args: &[Expr], index: usize, arity: usize) -> Result<String, Error> {
        if args.len() != arity {
            return Err(Error::Value);
        }
        self.text(&args[index])
    }

    fn lookup_ref(&self, reference: &CellRef) -> Result<Value, Error> {
        let sheet = match &reference.sheet {
            Some(name) => self.workbook.sheet_index(name).ok_or(Error::Ref)?,
            None => self.current_sheet,
        };
        Ok(lookup_value(
            self.workbook,
            self.computed,
            sheet,
            reference.addr,
        ))
    }

    fn resolve_range(&self, reference: &RangeRef) -> Result<(usize, CellRange), Error> {
        let sheet = match &reference.sheet {
            Some(name) => self.workbook.sheet_index(name).ok_or(Error::Ref)?,
            None => self.current_sheet,
        };
        Ok((sheet, reference.range))
    }
}

fn lookup_value(
    workbook: &Workbook,
    computed: &HashMap<CellKey, Value>,
    sheet: usize,
    addr: CellAddr,
) -> Value {
    let key = CellKey { sheet, addr };
    if let Some(value) = computed.get(&key) {
        return value.clone();
    }
    let Some(cell) = workbook.sheet(sheet).and_then(|sheet| sheet.cell(addr)) else {
        return Value::Empty;
    };
    if cell.is_formula() {
        // Formula cells are always seeded in `computed` before evaluation.
        // Seeing one here means it is part of a cycle that could not be
        // topologically ordered.
        Value::Error(Error::Circular)
    } else {
        cell.value.clone()
    }
}

fn sheet_index(workbook: &Workbook, name: &str) -> Option<usize> {
    workbook.sheet_index(name)
}

fn finite(value: f64) -> Value {
    if value.is_finite() {
        Value::Number(value)
    } else {
        Value::Error(Error::Num)
    }
}

fn excel_pow(base: f64, exponent: f64) -> Result<f64, Error> {
    if base == 0.0 {
        if exponent == 0.0 {
            return Err(Error::Num);
        }
        if exponent < 0.0 {
            return Err(Error::Div0);
        }
    }
    if base < 0.0 && exponent.fract() != 0.0 {
        return Err(Error::Num);
    }
    let value = base.powf(exponent);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Num)
    }
}

/// Excel rounds the decimal representation, not the raw binary expansion, and
/// only carries 15 significant digits. `ROUND(2.675, 2)` is therefore `2.68`,
/// while a naive `(2.675 * 100.0).round()` yields `2.67`. This routine decodes
/// the 15-digit scientific form and performs integer rounding on that decimal
/// mantissa, then reapplies the exponent.
fn round_to(value: f64, digits: i32, mode: &str) -> Option<f64> {
    if value == 0.0 {
        return Some(0.0);
    }
    if !value.is_finite() || !(-308..=308).contains(&digits) {
        return None;
    }
    let sign = if value.is_sign_negative() { -1.0 } else { 1.0 };
    let absolute = value.abs();

    let scientific = format!("{absolute:.14e}");
    let (mantissa, exponent) = scientific.split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let mantissa_digits: String = mantissa
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    let mantissa: u128 = mantissa_digits.parse().ok()?;

    // value = mantissa * 10^(exponent - 14)
    // q = round(value * 10^digits) = round(mantissa * 10^k)
    let k = exponent - 14 + digits;
    let q = if k >= 0 {
        // Already at or beyond the requested precision; no rounding needed.
        if k > 30 {
            return Some(value);
        }
        mantissa.checked_mul(10u128.checked_pow(k as u32)?)?
    } else {
        let places = (-k) as u32;
        if places > 38 {
            return Some(match mode {
                "ROUNDUP" => sign * 10f64.powi(digits),
                _ => 0.0,
            });
        }
        let divisor = 10u128.checked_pow(places)?;
        match mode {
            "ROUND" => (mantissa + divisor / 2) / divisor,
            "ROUNDUP" => mantissa.div_ceil(divisor),
            "ROUNDDOWN" => mantissa / divisor,
            _ => unreachable!(),
        }
    };

    let result = q as f64 * 10f64.powi(-digits) * sign;
    result.is_finite().then_some(result)
}

fn compare_values(lhs: &Value, rhs: &Value) -> Result<Ordering, Error> {
    if let Value::Error(error) = lhs {
        return Err(*error);
    }
    if let Value::Error(error) = rhs {
        return Err(*error);
    }
    if lhs.is_empty() && rhs.is_empty() {
        return Ok(Ordering::Equal);
    }
    if lhs.is_empty() {
        return compare_values(&coerce_empty_like(rhs), rhs);
    }
    if rhs.is_empty() {
        return compare_values(lhs, &coerce_empty_like(lhs));
    }

    match (lhs, rhs) {
        (Value::Number(a), Value::Number(b)) => Ok(a.partial_cmp(b).unwrap_or(Ordering::Equal)),
        (Value::Text(a), Value::Text(b)) => Ok(a.to_lowercase().cmp(&b.to_lowercase())),
        (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
        _ => Ok(type_rank(lhs).cmp(&type_rank(rhs))),
    }
}

fn coerce_empty_like(other: &Value) -> Value {
    match other {
        Value::Number(_) => Value::Number(0.0),
        Value::Text(_) => Value::Text("".into()),
        Value::Bool(_) => Value::Bool(false),
        _ => Value::Empty,
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Number(_) => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_is_case_insensitive_for_text() {
        assert_eq!(
            compare_values(&Value::text("abc"), &Value::text("ABC")),
            Ok(Ordering::Equal)
        );
        assert_eq!(
            compare_values(&Value::Number(1.0), &Value::text("1")),
            Ok(Ordering::Less)
        );
    }

    #[test]
    fn round_is_half_away_from_zero() {
        assert_eq!(round_to(2.5, 0, "ROUND"), Some(3.0));
        assert_eq!(round_to(-2.5, 0, "ROUND"), Some(-3.0));
        assert_eq!(round_to(2.1, 0, "ROUNDUP"), Some(3.0));
        assert_eq!(round_to(-2.1, 0, "ROUNDDOWN"), Some(-2.0));
        assert_eq!(round_to(1234.0, -2, "ROUND"), Some(1200.0));
        // The classic binary-float trap: Excel returns 2.68 here.
        assert_eq!(round_to(2.675, 2, "ROUND"), Some(2.68));
        assert_eq!(round_to(1.005, 2, "ROUND"), Some(1.01));
        assert_eq!(round_to(-2.675, 2, "ROUND"), Some(-2.68));
    }
}
