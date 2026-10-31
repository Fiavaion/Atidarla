//! DAT values: tables of strings and text (PRD §5.4).
//!
//! A [`Dat`] is either **text** (one string) or a **table** (rows × columns
//! of strings, row-major). It is immutable and shared: cloning one is an
//! `Arc` clone, so a value moves along wires and into worker threads
//! without copying. A new value is a new `Arc`, which is how the engine
//! tells a changed value from an unchanged one ([`Dat::ptr_eq`]): an
//! operator that produces equal content keeps its old value
//! ([`Dat::replace`]), so its consumers do not cook again.
//!
//! Every DAT reads as both forms. Text read as a table is one row per line
//! with one column; a table read as text is its rows joined by newlines and
//! its cells by tabs (TSV). Conversions with other delimiters are the
//! Convert operator's job.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// Rows × columns of strings, row-major. Every row has [`Table::cols`]
/// cells (short rows are padded with empty cells).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Table {
    cols: usize,
    cells: Vec<String>,
}

impl Table {
    /// An empty table with `cols` columns.
    pub fn new(cols: usize) -> Self {
        Self {
            cols,
            cells: Vec::new(),
        }
    }

    /// A table from rows; the widest row sets the column count.
    pub fn from_rows<R, C>(rows: R) -> Self
    where
        R: IntoIterator<Item = C>,
        C: IntoIterator,
        C::Item: Into<String>,
    {
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(Into::into).collect())
            .collect();
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut t = Self::new(cols);
        for r in rows {
            t.push_row(r);
        }
        t
    }

    /// Splits `text` into rows at newlines and cells at `delimiter`. A
    /// trailing newline does not make an empty last row; `\r\n` counts as a
    /// newline.
    pub fn parse(text: &str, delimiter: char) -> Self {
        Self::from_rows(lines(text).map(|line| line.split(delimiter)))
    }

    /// Appends a row: padded with empty cells to [`Self::cols`], or widening
    /// the table (earlier rows padded) when it is longer.
    pub fn push_row<I>(&mut self, row: I)
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let start = self.cells.len();
        self.cells.extend(row.into_iter().map(Into::into));
        let n = self.cells.len() - start;
        if n > self.cols {
            let rows_before = start.checked_div(self.cols).unwrap_or(0);
            let mut widened = Vec::with_capacity((rows_before + 1) * n);
            let mut old = self.cells.drain(..start);
            for _ in 0..rows_before {
                widened.extend(old.by_ref().take(self.cols));
                widened.extend(std::iter::repeat_n(String::new(), n - self.cols));
            }
            drop(old);
            widened.append(&mut self.cells);
            self.cells = widened;
            self.cols = n;
        } else {
            self.cells
                .extend(std::iter::repeat_n(String::new(), self.cols - n));
        }
    }

    pub fn rows(&self) -> usize {
        self.cells.len().checked_div(self.cols).unwrap_or(0)
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The cell at `row`, `col`, if inside the table.
    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        (col < self.cols)
            .then(|| self.cells.get(row * self.cols + col))
            .flatten()
            .map(String::as_str)
    }

    /// One row's cells.
    pub fn row(&self, row: usize) -> Option<&[String]> {
        let start = row.checked_mul(self.cols)?;
        self.cells.get(start..start + self.cols)
    }

    /// Every row in order.
    pub fn iter_rows(&self) -> impl Iterator<Item = &[String]> {
        self.cells.chunks(self.cols.max(1))
    }

    /// The index of the column whose first-row cell is `name`.
    pub fn col_index(&self, name: &str) -> Option<usize> {
        self.row(0)?.iter().position(|c| c == name)
    }

    /// The rows joined by newlines, cells by `delimiter`.
    pub fn join(&self, delimiter: &str) -> String {
        let mut s = String::new();
        for (i, row) in self.iter_rows().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            for (j, c) in row.iter().enumerate() {
                if j > 0 {
                    s.push_str(delimiter);
                }
                s.push_str(c);
            }
        }
        s
    }
}

/// Lines of `text`: split at `\n` (dropping a `\r` before it), without an
/// empty last line after a trailing newline. Empty text has no lines.
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let empty = text.is_empty();
    body.split('\n')
        .filter(move |_| !empty)
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum Content {
    Text(String),
    Table(Table),
}

/// A DAT value: text or a table, shared (see the module docs).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Dat(Arc<Content>);

impl Default for Dat {
    fn default() -> Self {
        Self::text("")
    }
}

impl fmt::Debug for Dat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            Content::Text(t) => f.debug_tuple("Dat::Text").field(t).finish(),
            Content::Table(t) => f
                .debug_struct("Dat::Table")
                .field("rows", &t.rows())
                .field("cols", &t.cols())
                .finish(),
        }
    }
}

impl From<Table> for Dat {
    fn from(t: Table) -> Self {
        Self::table(t)
    }
}

impl Dat {
    pub fn text(s: impl Into<String>) -> Self {
        Self(Arc::new(Content::Text(s.into())))
    }

    pub fn table(t: Table) -> Self {
        Self(Arc::new(Content::Table(t)))
    }

    /// Whether this is text (else a table).
    pub fn is_text(&self) -> bool {
        matches!(&*self.0, Content::Text(_))
    }

    /// The same value (not just equal content): cloned from one another.
    pub fn ptr_eq(a: &Dat, b: &Dat) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }

    /// Sets `self` to `new` unless their content is equal, keeping the old
    /// value (and so telling consumers nothing changed). Whether it changed.
    pub fn replace(&mut self, new: Dat) -> bool {
        if *self == new {
            return false;
        }
        *self = new;
        true
    }

    /// The text, or the table as TSV.
    pub fn as_text(&self) -> Cow<'_, str> {
        match &*self.0 {
            Content::Text(t) => Cow::Borrowed(t),
            Content::Table(t) => Cow::Owned(t.join("\t")),
        }
    }

    /// The table, or the text as one row per line with one column.
    pub fn as_table(&self) -> Cow<'_, Table> {
        match &*self.0 {
            Content::Table(t) => Cow::Borrowed(t),
            Content::Text(t) => Cow::Owned(Table::from_rows(lines(t).map(|l| [l]))),
        }
    }

    /// Rows: of the table, or lines of the text.
    pub fn rows(&self) -> usize {
        match &*self.0 {
            Content::Table(t) => t.rows(),
            Content::Text(t) => lines(t).count(),
        }
    }

    /// Columns: of the table, or 1 for non-empty text.
    pub fn cols(&self) -> usize {
        match &*self.0 {
            Content::Table(t) => t.cols(),
            Content::Text(t) => usize::from(!t.is_empty()),
        }
    }

    /// A cell (for text: column 0 of line `row`).
    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        match &*self.0 {
            Content::Table(t) => t.cell(row, col),
            Content::Text(t) if col == 0 => lines(t).nth(row),
            Content::Text(_) => None,
        }
    }

    /// As JSON: text is a string, a table an array of rows of strings.
    pub fn to_json(&self) -> serde_json::Value {
        match &*self.0 {
            Content::Text(t) => serde_json::Value::from(t.as_str()),
            Content::Table(t) => serde_json::Value::Array(
                t.iter_rows()
                    .map(|r| {
                        r.iter()
                            .map(|c| serde_json::Value::from(c.as_str()))
                            .collect()
                    })
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_pad_and_widen_rows() {
        let mut t = Table::new(2);
        t.push_row(["a", "b"]);
        t.push_row(["c"]);
        assert_eq!((t.rows(), t.cols()), (2, 2));
        assert_eq!(t.cell(1, 1), Some(""));
        t.push_row(["d", "e", "f"]);
        assert_eq!((t.rows(), t.cols()), (3, 3));
        assert_eq!(t.row(0).unwrap(), ["a", "b", ""]);
        assert_eq!(t.row(1).unwrap(), ["c", "", ""]);
        assert_eq!(t.row(2).unwrap(), ["d", "e", "f"]);
        assert_eq!(t.cell(3, 0), None);
        assert_eq!(t.cell(0, 3), None);
        assert_eq!(t.col_index("b"), Some(1));
        assert_eq!(t.join(","), "a,b,\nc,,\nd,e,f");
        let empty = Table::new(0);
        assert_eq!((empty.rows(), empty.iter_rows().count()), (0, 0));
    }

    #[test]
    fn text_is_parsed_into_rows_and_lines() {
        let t = Table::parse("name\tvalue\r\nspeed\t2\n", '\t');
        assert_eq!((t.rows(), t.cols()), (2, 2));
        assert_eq!(t.cell(1, 1), Some("2"));
        assert_eq!(lines("").count(), 0);
        assert_eq!(lines("\n").collect::<Vec<_>>(), [""]);
        assert_eq!(lines("a\n\nb").collect::<Vec<_>>(), ["a", "", "b"]);
    }

    #[test]
    fn both_forms_read_as_text_and_table() {
        let text = Dat::text("one\ntwo");
        assert!(text.is_text());
        assert_eq!((text.rows(), text.cols()), (2, 1));
        assert_eq!(text.cell(1, 0), Some("two"));
        assert_eq!(text.cell(0, 1), None);
        assert_eq!(text.as_table().row(0).unwrap(), ["one"]);
        let table = Dat::table(Table::from_rows([["a", "b"], ["c", "d"]]));
        assert_eq!(table.as_text(), "a\tb\nc\td");
        assert_eq!(table.to_json(), serde_json::json!([["a", "b"], ["c", "d"]]));
        assert_eq!(Dat::default().cols(), 0);
    }

    #[test]
    fn replacing_with_equal_content_keeps_the_value() {
        let mut d = Dat::text("x");
        let before = d.clone();
        assert!(!d.replace(Dat::text("x")));
        assert!(Dat::ptr_eq(&d, &before));
        assert!(d.replace(Dat::text("y")));
        assert!(!Dat::ptr_eq(&d, &before));
        // Equal content, different values: equal, not the same.
        assert_eq!(Dat::text("y"), d);
    }
}
