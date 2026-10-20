//! Excel-compatibility regression tests.
//!
//! The expected values here are the ones Excel 365 returns for the same
//! formulas. Floating comparisons use a relative tolerance far tighter than
//! Excel's 15-significant-digit display, but loose enough to ignore binary
//! association differences in a reduction.

use hyo_core::{CellAddr, CpuBackend, Error, Value, Workbook};

fn addr(a1: &str) -> CellAddr {
    CellAddr::from_a1(a1).unwrap()
}

fn eval(input: &str) -> Value {
    let mut workbook = Workbook::new();
    workbook.set_input(0, addr("Z1"), input).unwrap();
    workbook.recalc(&CpuBackend);
    workbook.sheet(0).unwrap().value(addr("Z1"))
}

fn assert_number(input: &str, expected: f64) {
    match eval(input) {
        Value::Number(actual) => {
            let tolerance = 1e-12 * expected.abs().max(1.0);
            assert!(
                (actual - expected).abs() <= tolerance,
                "{input}: expected {expected}, got {actual}"
            );
        }
        other => panic!("{input}: expected number {expected}, got {other:?}"),
    }
}

fn assert_bool(input: &str, expected: bool) {
    assert_eq!(eval(input), Value::Bool(expected), "{input}");
}

fn assert_error(input: &str, expected: Error) {
    assert_eq!(eval(input), Value::Error(expected), "{input}");
}

fn assert_text(input: &str, expected: &str) {
    assert_eq!(eval(input), Value::text(expected), "{input}");
}

#[test]
fn excel_operator_precedence_and_powers() {
    assert_number("=1+2*3", 7.0);
    assert_number("=(1+2)*3", 9.0);
    assert_number("=-2^2", 4.0);
    assert_number("=2^-2", 0.25);
    // Excel evaluates chained powers left-to-right: (2^3)^2.
    assert_number("=2^3^2", 64.0);
    assert_number("=10/4", 2.5);
    assert_text("=3+4&\"x\"", "7x");
}

#[test]
fn excel_rounding_traps() {
    assert_number("=ROUND(2.675,2)", 2.68);
    assert_number("=ROUND(-2.675,2)", -2.68);
    assert_number("=ROUND(1.005,2)", 1.01);
    assert_number("=ROUND(2.5,0)", 3.0);
    assert_number("=ROUND(-2.5,0)", -3.0);
    assert_number("=ROUND(1234,-2)", 1200.0);
    assert_number("=ROUNDUP(2.1,0)", 3.0);
    assert_number("=ROUNDDOWN(-2.1,0)", -2.0);
}

#[test]
fn excel_error_values() {
    assert_error("=1/0", Error::Div0);
    assert_error("=SQRT(-1)", Error::Num);
    assert_error("=0^0", Error::Num);
    assert_error("=2^1024", Error::Num);
    assert_error("=LOG(-1)", Error::Num);
    assert_error("=LEFT(\"a\",-1)", Error::Value);
}

#[test]
fn aggregates_ignore_text_and_booleans_in_ranges() {
    let mut workbook = Workbook::new();
    workbook.set_input(0, addr("A1"), "10").unwrap();
    workbook.set_input(0, addr("A2"), "hello").unwrap();
    workbook.set_input(0, addr("A3"), "TRUE").unwrap();
    workbook.set_input(0, addr("A4"), "20").unwrap();
    for (target, formula) in [
        ("B1", "=SUM(A1:A5)"),
        ("B2", "=AVERAGE(A1:A5)"),
        ("B3", "=COUNT(A1:A5)"),
        ("B4", "=COUNTA(A1:A5)"),
        ("B5", "=COUNTBLANK(A1:A5)"),
        ("B6", "=MIN(A1:A5)"),
        ("B7", "=MAX(A1:A5)"),
        ("B8", "=PRODUCT(A1:A5)"),
    ] {
        workbook.set_input(0, addr(target), formula).unwrap();
    }
    workbook.recalc(&CpuBackend);
    let value = |a1: &str| workbook.sheet(0).unwrap().value(addr(a1));
    assert_eq!(value("B1"), Value::Number(30.0));
    assert_eq!(value("B2"), Value::Number(15.0));
    assert_eq!(value("B3"), Value::Number(2.0));
    assert_eq!(value("B4"), Value::Number(4.0));
    assert_eq!(value("B5"), Value::Number(1.0));
    assert_eq!(value("B6"), Value::Number(10.0));
    assert_eq!(value("B7"), Value::Number(20.0));
    assert_eq!(value("B8"), Value::Number(200.0));
}

#[test]
fn conditional_functions_are_lazy() {
    assert_number("=IF(TRUE,1,1/0)", 1.0);
    assert_number("=IF(FALSE,1/0,2)", 2.0);
    assert_number("=IFERROR(1/0,42)", 42.0);
    assert_error("=IFNA(1/0,42)", Error::Div0);
    assert_number("=IFNA(NA(),42)", 42.0);
    assert_bool("=AND(TRUE,1,2)", true);
    assert_bool("=OR(FALSE,0,1)", true);
    assert_bool("=NOT(FALSE)", true);
}

#[test]
fn text_and_comparison_semantics() {
    assert_text("=CONCAT(\"a\",1,TRUE)", "a1TRUE");
    assert_text("=LEFT(\"hello\",2)", "he");
    assert_text("=RIGHT(\"hello\",2)", "lo");
    assert_text("=MID(\"hello\",2,3)", "ell");
    assert_text("=UPPER(\"abc\")", "ABC");
    assert_text("=TRIM(\"  a   b  \")", "a b");
    assert_number("=LEN(\"hé\")", 2.0);
    assert_bool("=\"a\"=\"A\"", true);
    assert_bool("=1<2", true);
    assert_bool("=1=\"1\"", false);
    assert_bool("=2>=2", true);
    assert_bool("=\"a\"<\"b\"", true);
}

#[test]
fn empty_cell_comparisons() {
    let mut workbook = Workbook::new();
    workbook.set_input(0, addr("A1"), "=A2=\"\"").unwrap();
    workbook.recalc(&CpuBackend);
    assert_eq!(
        workbook.sheet(0).unwrap().value(addr("A1")),
        Value::Bool(true)
    );
}

#[test]
fn cross_sheet_and_cycles() {
    let mut workbook = Workbook::with_sheet_names(["Data", "Summary"]);
    workbook.set_input(0, addr("A1"), "1").unwrap();
    workbook.set_input(0, addr("A2"), "2").unwrap();
    workbook.set_input(0, addr("A3"), "3").unwrap();
    workbook
        .set_input(1, addr("A1"), "=SUM(Data!A1:A3)")
        .unwrap();
    workbook.recalc(&CpuBackend);
    assert_eq!(
        workbook.sheet(1).unwrap().value(addr("A1")),
        Value::Number(6.0)
    );

    let mut cycle = Workbook::new();
    cycle.set_input(0, addr("A1"), "=B1+1").unwrap();
    cycle.set_input(0, addr("B1"), "=A1+1").unwrap();
    cycle.recalc(&CpuBackend);
    assert_eq!(
        cycle.sheet(0).unwrap().value(addr("A1")),
        Value::Error(Error::Circular)
    );
}

#[test]
fn display_precision_matches_excel_general_format() {
    assert_eq!(eval("=1/3").display(), "0.333333333333333");
    assert_eq!(eval("=0.1+0.2").display(), "0.3");
    assert_eq!(eval("=123456789.125").display(), "123456789.125");
}
