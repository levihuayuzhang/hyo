//! A small, formula-heavy workbook used on first launch.

use hyo_core::{CellAddr, Value, Workbook};

pub fn workbook() -> Workbook {
    let mut workbook = Workbook::with_sheet_names(["Budget", "Summary"]);

    for (a1, value) in [
        ("A1", "Item"),
        ("B1", "Qty"),
        ("C1", "Unit Price"),
        ("D1", "Total"),
        ("A2", "Workstations"),
        ("A3", "GPUs"),
        ("A4", "Monitors"),
        ("A5", "Licenses"),
        ("A6", "Support"),
        ("A8", "Subtotal"),
        ("A9", "VAT 13%"),
        ("A10", "Grand total"),
        ("A12", "Average line"),
        ("A13", "Largest line"),
        ("A15", "Decision"),
    ] {
        set(&mut workbook, 0, a1, Value::text(value));
    }

    for (a1, qty, price) in [
        ("A2", 12.0, 1850.0),
        ("A3", 4.0, 3200.0),
        ("A4", 16.0, 420.0),
        ("A5", 18.0, 240.0),
        ("A6", 1.0, 5400.0),
    ] {
        let row = a1.trim_start_matches('A');
        set(&mut workbook, 0, &format!("B{row}"), Value::Number(qty));
        set(&mut workbook, 0, &format!("C{row}"), Value::Number(price));
    }

    for row in 2..=6 {
        formula(
            &mut workbook,
            0,
            &format!("D{row}"),
            &format!("=B{row}*C{row}"),
        );
    }
    formula(&mut workbook, 0, "D8", "=SUM(D2:D6)");
    formula(&mut workbook, 0, "D9", "=D8*0.13");
    formula(&mut workbook, 0, "D10", "=D8+D9");
    formula(&mut workbook, 0, "D12", "=AVERAGE(D2:D6)");
    formula(&mut workbook, 0, "D13", "=MAX(D2:D6)");
    formula(
        &mut workbook,
        0,
        "D15",
        "=IF(D10>50000,\"OVER BUDGET\",\"WITHIN BUDGET\")",
    );

    for (a1, value) in [
        ("A1", "Metric"),
        ("B1", "Value"),
        ("A2", "Line items"),
        ("A3", "Grand total"),
        ("A4", "Average line"),
        ("A5", "Spread"),
        ("A7", "Formula preview"),
    ] {
        set(&mut workbook, 1, a1, Value::text(value));
    }
    formula(&mut workbook, 1, "B2", "=COUNT(Budget!D2:D6)");
    formula(&mut workbook, 1, "B3", "=Budget!D10");
    formula(&mut workbook, 1, "B4", "=Budget!D12");
    formula(
        &mut workbook,
        1,
        "B5",
        "=MAX(Budget!D2:D6)-MIN(Budget!D2:D6)",
    );
    formula(
        &mut workbook,
        1,
        "B7",
        "=CONCAT(\"GPU count: \", Budget!B3, \" / total: \", Budget!D10)",
    );

    workbook
}

fn set(workbook: &mut Workbook, sheet: usize, a1: &str, value: Value) {
    let addr = CellAddr::from_a1(a1).expect("demo address");
    workbook
        .sheet_mut(sheet)
        .expect("demo sheet")
        .set_value(addr, value);
}

fn formula(workbook: &mut Workbook, sheet: usize, a1: &str, formula: &str) {
    let addr = CellAddr::from_a1(a1).expect("demo address");
    workbook
        .set_input(sheet, addr, formula)
        .expect("demo formula parses");
}
