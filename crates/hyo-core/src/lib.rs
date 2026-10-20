//! Hyo Office spreadsheet core.
//!
//! This crate is UI- and GPU-runtime-agnostic. It owns the Excel-compatible
//! value model, formula parser/evaluator and dependency graph. A numeric
//! backend (CPU by default, CUDA through `hyo-gpu`) only accelerates range
//! aggregations; it cannot change formula semantics.
//!
//! ```
//! use hyo_core::{CellAddr, CpuBackend, Value, Workbook};
//!
//! let mut workbook = Workbook::new();
//! workbook.set_input(0, CellAddr::from_a1("A1").unwrap(), "2").unwrap();
//! workbook.set_input(0, CellAddr::from_a1("A2").unwrap(), "=A1*3").unwrap();
//! workbook.recalc(&CpuBackend);
//! assert_eq!(
//!     workbook.sheet(0).unwrap().value(CellAddr::from_a1("A2").unwrap()),
//!     Value::Number(6.0)
//! );
//! ```

pub mod addr;
pub mod eval;
pub mod formula;
pub mod numeric;
pub mod sheet;
pub mod value;
pub mod workbook;

pub use addr::{CellAddr, CellRange, MAX_COLS, MAX_ROWS};
pub use eval::EvalStats;
pub use formula::{BinaryOp, CellRef, Expr, Formula, FormulaError, RangeRef, UnaryOp};
pub use numeric::{Aggregate, BackendKind, CpuBackend, DotProduct, NumericBackend};
pub use sheet::{Cell, Sheet, parse_literal};
pub use value::{Error, Value, format_number};
pub use workbook::{CellKey, RecalcReport, Workbook};
