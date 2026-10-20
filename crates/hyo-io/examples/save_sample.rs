//! Writes a small workbook that can be opened in Microsoft Excel to verify the
//! writer's formula/result encoding.

use hyo_core::{CellAddr, CpuBackend, Value, Workbook};
use hyo_io::save_xlsx;
use std::path::PathBuf;

fn main() {
    let mut workbook = Workbook::new();
    for (a1, input) in [
        ("A1", "10"),
        ("A2", "20"),
        ("A3", "=SUM(A1:A2)"),
        ("A4", "=ROUND(2.675,2)"),
        ("A5", "=\"a\"&1"),
        ("A6", "=IF(A3>25,\"ok\",\"no\")"),
        ("A7", "=AVERAGE(A1:A2)"),
    ] {
        workbook
            .set_input(0, CellAddr::from_a1(a1).unwrap(), input)
            .unwrap();
    }
    workbook
        .sheet_mut(0)
        .unwrap()
        .set_value(CellAddr::from_a1("C1").unwrap(), Value::text("Hyo Sheet"));
    workbook.recalc(&CpuBackend);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hyo-sample.xlsx");
    save_xlsx(&workbook, &path).unwrap();
    println!("wrote {}", path.display());
}
