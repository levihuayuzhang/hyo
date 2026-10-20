//! Compares a recalculated Hyo workbook against a fixture produced by Excel.
//!
//! Regenerate the fixture with `scripts/make-excel-golden.ps1` on a machine
//! with Microsoft Excel. CI only needs the committed `.xlsx`/`.tsv` pair.

use hyo_core::{CellAddr, CpuBackend, Value};
use hyo_io::load_xlsx;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn matches_excel_golden_values() {
    let tsv = std::fs::read_to_string(fixture("excel-golden.tsv"))
        .expect("fixtures/excel-golden.tsv missing; run scripts/make-excel-golden.ps1");
    let mut workbook = load_xlsx(fixture("excel-golden.xlsx")).expect("golden workbook");
    let report = workbook.recalc(&CpuBackend);
    assert_eq!(report.cycle_count, 0);

    let mut checked = 0;
    for (index, line) in tsv.lines().enumerate() {
        if index == 0 || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(7, '\t').collect();
        assert_eq!(fields.len(), 7, "bad TSV row {index}: {line}");
        let case = fields[0];
        let sheet_name = fields[1];
        let address = CellAddr::from_a1(fields[2]).unwrap();
        let expected_text = fields[4];
        let expected_type = fields[5];
        let expected_raw = fields[6];

        let sheet = workbook
            .sheet_index(sheet_name)
            .unwrap_or_else(|| panic!("{case}: sheet {sheet_name}"));
        let actual = workbook.sheet(sheet).unwrap().value(address);

        if expected_text.starts_with('#') {
            assert_eq!(
                actual.display(),
                expected_text,
                "{case} ({}): Excel reported {expected_text}, Hyo reported {actual:?}",
                fields[3]
            );
            checked += 1;
            continue;
        }

        match expected_type {
            "Boolean" => {
                let expected = expected_raw.eq_ignore_ascii_case("true");
                assert_eq!(
                    actual,
                    Value::Bool(expected),
                    "{case} ({}): expected {expected}, got {actual:?}",
                    fields[3]
                );
            }
            "String" => {
                assert_eq!(
                    actual,
                    Value::text(expected_text),
                    "{case} ({}): text mismatch",
                    fields[3]
                );
            }
            "Double" | "Int32" | "Int64" => {
                let expected: f64 = expected_raw.parse().expect("numeric golden value");
                match actual {
                    Value::Number(actual) => {
                        let tolerance = 1e-12 * expected.abs().max(1.0);
                        assert!(
                            (actual - expected).abs() <= tolerance,
                            "{case} ({}): expected {expected}, got {actual}",
                            fields[3]
                        );
                    }
                    other => panic!(
                        "{case} ({}): expected number {expected}, got {other:?}",
                        fields[3]
                    ),
                }
            }
            other => panic!("{case}: unsupported Excel type {other}"),
        }
        checked += 1;
    }

    assert!(checked >= 40, "only {checked} golden cases were checked");
}
