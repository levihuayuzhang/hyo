//! A1 addressing, ranges and sheet-name quoting.

use std::fmt::{self, Display, Formatter};

/// Excel's hard row limit (1-based rows 1..=1_048_576).
pub const MAX_ROWS: u32 = 1_048_576;
/// Excel's hard column limit (A..=XFD).
pub const MAX_COLS: u32 = 16_384;

/// A zero-based cell address.
///
/// Field order is deliberate: `row` is compared before `column`, which gives
/// row-major ordering for `BTreeMap<CellAddr, _>`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellAddr {
    pub row: u32,
    pub col: u32,
}

impl CellAddr {
    pub const fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }

    pub const fn in_bounds(self) -> bool {
        self.row < MAX_ROWS && self.col < MAX_COLS
    }

    /// Parses `A1`, `$A$1`, `XFD1048576` into a zero-based address.
    pub fn from_a1(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let mut i = 0;

        if bytes.get(i) == Some(&b'$') {
            i += 1;
        }

        let col_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
        }
        if i == col_start || i - col_start > 3 {
            return None;
        }
        let col = parse_column(&text[col_start..i])?;

        if bytes.get(i) == Some(&b'$') {
            i += 1;
        }

        let row_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == row_start || i != bytes.len() {
            return None;
        }
        let row_1: u32 = text[row_start..i].parse().ok()?;
        if row_1 == 0 {
            return None;
        }
        let addr = CellAddr::new(row_1 - 1, col);
        addr.in_bounds().then_some(addr)
    }

    /// `A1` form, without dollar signs.
    pub fn to_a1(self) -> String {
        format!("{}{}", column_name(self.col), self.row + 1)
    }

    /// `$A$1` form.
    pub fn to_a1_absolute(self) -> String {
        format!("${}${}", column_name(self.col), self.row + 1)
    }

    pub fn saturating_offset(self, d_row: i64, d_col: i64) -> Self {
        let row = (self.row as i64 + d_row).clamp(0, (MAX_ROWS - 1) as i64) as u32;
        let col = (self.col as i64 + d_col).clamp(0, (MAX_COLS - 1) as i64) as u32;
        Self { row, col }
    }
}

impl Display for CellAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_a1())
    }
}

/// A rectangular, inclusive cell range. Endpoints are normalized so
/// `start <= end` in both axes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellRange {
    pub start: CellAddr,
    pub end: CellAddr,
}

impl CellRange {
    pub fn new(a: CellAddr, b: CellAddr) -> Self {
        Self {
            start: CellAddr::new(a.row.min(b.row), a.col.min(b.col)),
            end: CellAddr::new(a.row.max(b.row), a.col.max(b.col)),
        }
    }

    pub const fn single(addr: CellAddr) -> Self {
        Self {
            start: addr,
            end: addr,
        }
    }

    pub const fn contains(self, addr: CellAddr) -> bool {
        addr.row >= self.start.row
            && addr.row <= self.end.row
            && addr.col >= self.start.col
            && addr.col <= self.end.col
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.start.row <= other.end.row
            && other.start.row <= self.end.row
            && self.start.col <= other.end.col
            && other.start.col <= self.end.col
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        self.intersects(other).then(|| Self {
            start: CellAddr::new(
                self.start.row.max(other.start.row),
                self.start.col.max(other.start.col),
            ),
            end: CellAddr::new(
                self.end.row.min(other.end.row),
                self.end.col.min(other.end.col),
            ),
        })
    }

    pub fn cell_count(self) -> u64 {
        (self.end.row - self.start.row + 1) as u64 * (self.end.col - self.start.col + 1) as u64
    }

    pub fn rows(self) -> u32 {
        self.end.row - self.start.row + 1
    }

    pub fn cols(self) -> u32 {
        self.end.col - self.start.col + 1
    }

    /// Iterate the addresses in row-major order. Callers must keep the range
    /// small enough for interactive use (the UI never asks for a full column).
    pub fn iter(self) -> impl Iterator<Item = CellAddr> {
        (self.start.row..=self.end.row).flat_map(move |row| {
            (self.start.col..=self.end.col).map(move |col| CellAddr::new(row, col))
        })
    }

    pub fn to_a1(self) -> String {
        if self.start == self.end {
            self.start.to_a1()
        } else {
            format!("{}:{}", self.start.to_a1(), self.end.to_a1())
        }
    }
}

impl Display for CellRange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_a1())
    }
}

/// Converts a zero-based column index to Excel letters.
pub fn column_name(mut col: u32) -> String {
    let mut out = String::with_capacity(3);
    loop {
        out.push((b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.chars().rev().collect()
}

/// Converts Excel letters to a zero-based column index.
pub fn parse_column(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 3 {
        return None;
    }
    let mut value: u32 = 0;
    for b in text.bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        value = value * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
    }
    let col = value.checked_sub(1)?;
    (col < MAX_COLS).then_some(col)
}

/// Quotes a sheet name for use in a formula the way Excel does.
pub fn quote_sheet_name(name: &str) -> String {
    let needs_quotes = name.is_empty()
        || name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'));
    if needs_quotes {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_owned()
    }
}

/// Renders `Sheet1!A1` (quoting the sheet when needed).
pub fn qualified_a1(sheet: &str, addr: CellAddr) -> String {
    format!("{}!{}", quote_sheet_name(sheet), addr.to_a1())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_round_trip() {
        for col in [0, 1, 25, 26, 27, 701, 702, 16_383] {
            let name = column_name(col);
            assert_eq!(parse_column(&name), Some(col), "{name}");
        }
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(16_383), "XFD");
    }

    #[test]
    fn parses_absolute_and_relative() {
        assert_eq!(CellAddr::from_a1("A1"), Some(CellAddr::new(0, 0)));
        assert_eq!(CellAddr::from_a1("$B$2"), Some(CellAddr::new(1, 1)));
        assert_eq!(
            CellAddr::from_a1("XFD1048576"),
            Some(CellAddr::new(1_048_575, 16_383))
        );
        assert_eq!(CellAddr::from_a1("A0"), None);
        assert_eq!(CellAddr::from_a1("XFE1"), None);
        assert_eq!(CellAddr::from_a1("1048577"), None);
        assert_eq!(CellAddr::from_a1("A1x"), None);
        assert_eq!(CellAddr::from_a1("$A1"), Some(CellAddr::new(0, 0)));
    }

    #[test]
    fn range_normalizes_and_intersects() {
        let r = CellRange::new(CellAddr::new(5, 4), CellAddr::new(1, 2));
        assert_eq!(r.start, CellAddr::new(1, 2));
        assert_eq!(r.end, CellAddr::new(5, 4));
        assert!(r.contains(CellAddr::new(3, 3)));
        assert!(!r.contains(CellAddr::new(0, 3)));
        assert_eq!(r.cell_count(), 15);
        assert_eq!(r.to_a1(), "C2:E6");
    }

    #[test]
    fn quotes_sheet_names() {
        assert_eq!(quote_sheet_name("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet_name("My Sheet"), "'My Sheet'");
        assert_eq!(quote_sheet_name("O'Brien"), "'O''Brien'");
    }
}
