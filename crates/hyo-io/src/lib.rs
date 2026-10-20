//! XLSX import/export.
//!
//! Reading and writing are deliberately separated from the engine: `hyo-core`
//! knows nothing about ZIP containers, shared strings or Excel's XML dialect.
//! Formulas are imported as source text and re-parsed by the engine; cached
//! values are ignored so a recalculation cannot silently trust a stale cache.

use calamine::{CellErrorType, Data, Reader as CalamineReader, open_workbook_auto};
use hyo_core::{CellAddr, Error, Value, Workbook, parse_literal};
use rust_xlsxwriter::{Formula, Workbook as XlsxWriter, XlsxError as WriterError};
use std::fmt::{self, Display, Formatter};
use std::path::Path;

/// Errors from reading or writing an XLSX file.
#[derive(Debug)]
pub struct XlsxError {
    message: String,
}

impl XlsxError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for XlsxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for XlsxError {}

impl From<calamine::Error> for XlsxError {
    fn from(error: calamine::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<WriterError> for XlsxError {
    fn from(error: WriterError) -> Self {
        Self::new(error.to_string())
    }
}

/// Loads an XLSX workbook. The returned workbook has formulas parsed but not
/// yet evaluated; call `Workbook::recalc` afterwards with the desired backend.
pub fn load_xlsx(path: impl AsRef<Path>) -> Result<Workbook, XlsxError> {
    let path = path.as_ref();
    let mut book = open_workbook_auto(path)
        .map_err(|error| XlsxError::new(format!("{}: {error}", path.display())))?;
    let names = book.sheet_names();
    if names.is_empty() {
        return Ok(Workbook::new());
    }
    let formula_names = names.clone();
    let mut workbook = Workbook::with_sheet_names(names);

    for (sheet_index, name) in formula_names.iter().enumerate() {
        let range = book.worksheet_range(name)?;
        let (start_row, start_col) = range.start().unwrap_or((0, 0));
        for (row, col, data) in range.cells() {
            let Some(addr) = in_bounds(row as u32 + start_row, col as u32 + start_col) else {
                continue;
            };
            if let Some(value) = data_to_value(data) {
                workbook
                    .sheet_mut(sheet_index)
                    .expect("sheet index from sheet_names")
                    .set_value(addr, value);
            }
        }

        let formulas = book.worksheet_formula(name)?;
        let (start_row, start_col) = formulas.start().unwrap_or((0, 0));
        for (row, col, formula) in formulas.cells() {
            let Some(addr) = in_bounds(row as u32 + start_row, col as u32 + start_col) else {
                continue;
            };
            if !formula.trim().is_empty() {
                workbook
                    .sheet_mut(sheet_index)
                    .expect("sheet index from sheet_names")
                    .set_formula_source(addr, formula);
            }
        }
    }

    Ok(workbook)
}

/// Saves a workbook as XLSX, including cached formula results so viewers that
/// do not recalculate still show the current engine output.
pub fn save_xlsx(workbook: &Workbook, path: impl AsRef<Path>) -> Result<(), XlsxError> {
    let mut writer = XlsxWriter::new();
    for sheet in workbook.sheets() {
        let worksheet = writer.add_worksheet();
        worksheet.set_name(sheet.name())?;
        for (addr, cell) in sheet.iter() {
            let row = addr.row;
            let col =
                u16::try_from(addr.col).map_err(|_| XlsxError::new("column exceeds XLSX limit"))?;
            if let Some(formula) = &cell.formula {
                let cached = cell.value.display();
                let formula = Formula::new(format!("={}", formula.source)).set_result(cached);
                worksheet.write_formula(row, col, formula)?;
                continue;
            }
            match &cell.value {
                Value::Empty => {}
                Value::Number(number) => {
                    worksheet.write_number(row, col, *number)?;
                }
                Value::Text(text) => {
                    worksheet.write_string(row, col, text.to_string())?;
                }
                Value::Bool(value) => {
                    worksheet.write_boolean(row, col, *value)?;
                }
                Value::Error(error) => {
                    // rust_xlsxwriter has no typed error cell; a string that
                    // matches the error literal round-trips through our reader.
                    worksheet.write_string(row, col, error.as_str())?;
                }
            }
        }
    }
    writer.save(path.as_ref())?;
    Ok(())
}

fn in_bounds(row: u32, col: u32) -> Option<CellAddr> {
    let addr = CellAddr::new(row, col);
    addr.in_bounds().then_some(addr)
}

fn data_to_value(data: &Data) -> Option<Value> {
    match data {
        Data::Empty => None,
        Data::Int(value) => Some(Value::Number(*value as f64)),
        Data::Float(value) => Some(Value::Number(*value)),
        Data::String(text) => Some(string_to_value(text)),
        Data::Bool(value) => Some(Value::Bool(*value)),
        // Excel dates are serial numbers. Keeping the serial number preserves
        // formula semantics even before number formats are implemented.
        Data::DateTime(value) => Some(Value::Number(value.as_f64())),
        Data::DateTimeIso(text) | Data::DurationIso(text) => Some(Value::text(text.clone())),
        Data::Error(error) => Some(Value::Error(map_cell_error(error))),
    }
}

fn string_to_value(text: &str) -> Value {
    if let Some(error) = hyo_core::sheet::parse_error_literal(text) {
        return Value::Error(error);
    }
    // A leading apostrophe means "force text" in the editor. The stored text
    // does not include it in XLSX, so only parse numbers when the whole string
    // is numeric.
    if !text.is_empty()
        && let Some(number) = hyo_core::value::parse_number_text(text)
        && text.trim().parse::<f64>().is_ok()
    {
        return Value::Number(number);
    }
    Value::text(text.to_owned())
}

fn map_cell_error(error: &CellErrorType) -> Error {
    match error {
        CellErrorType::Div0 => Error::Div0,
        CellErrorType::NA => Error::Na,
        CellErrorType::Name => Error::Name,
        CellErrorType::Null => Error::Null,
        CellErrorType::Num => Error::Num,
        CellErrorType::Ref => Error::Ref,
        CellErrorType::Value => Error::Value,
        CellErrorType::GettingData => Error::Value,
    }
}

/// Parses a single literal using the same rules as the editor. Exposed for
/// tests and for future CSV import.
pub fn parse_cell_literal(input: &str) -> Value {
    parse_literal(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyo_core::{CellAddr, CpuBackend};

    fn value(workbook: &Workbook, sheet: usize, a1: &str) -> Value {
        workbook
            .sheet(sheet)
            .unwrap()
            .value(CellAddr::from_a1(a1).unwrap())
    }

    #[test]
    fn xlsx_round_trip_preserves_values_and_formulas() {
        let mut workbook = Workbook::with_sheet_names(["Data", "Summary"]);
        let a1 = CellAddr::from_a1("A1").unwrap();
        let a2 = CellAddr::from_a1("A2").unwrap();
        let b1 = CellAddr::from_a1("B1").unwrap();
        workbook
            .sheet_mut(0)
            .unwrap()
            .set_value(a1, Value::Number(2.5));
        workbook
            .sheet_mut(0)
            .unwrap()
            .set_value(a2, Value::text("hello"));
        workbook
            .sheet_mut(1)
            .unwrap()
            .set_input(b1, "=SUM(Data!A1:A2)")
            .unwrap();
        workbook.recalc(&CpuBackend);

        let path = std::env::temp_dir().join(format!(
            "hyo-xlsx-roundtrip-{}-{}.xlsx",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        save_xlsx(&workbook, &path).unwrap();
        let mut loaded = load_xlsx(&path).unwrap();
        loaded.recalc(&CpuBackend);
        assert_eq!(value(&loaded, 0, "A1"), Value::Number(2.5));
        assert_eq!(value(&loaded, 0, "A2"), Value::text("hello"));
        assert_eq!(value(&loaded, 1, "B1"), Value::Number(2.5));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn error_literals_are_typed() {
        assert_eq!(string_to_value("#DIV/0!"), Value::Error(Error::Div0));
        assert_eq!(string_to_value("plain"), Value::text("plain"));
    }

    #[test]
    fn committed_sample_workbook_recalculates() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/hyo-sample.xlsx");
        let mut workbook = load_xlsx(&path).expect("hyo-sample.xlsx fixture");
        workbook.recalc(&CpuBackend);
        let sheet = workbook.sheet(0).unwrap();
        assert_eq!(
            sheet.value(CellAddr::from_a1("A3").unwrap()),
            Value::Number(30.0)
        );
        assert_eq!(
            sheet.value(CellAddr::from_a1("A4").unwrap()),
            Value::Number(2.68)
        );
        assert_eq!(
            sheet.value(CellAddr::from_a1("A6").unwrap()),
            Value::text("ok")
        );
    }
}
