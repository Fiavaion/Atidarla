//! Delimited text (TSV, CSV and friends) to and from tables.
//!
//! Tab-separated text is split literally (a tab cannot appear in a cell).
//! Other delimiters follow RFC 4180 quoting: a cell in double quotes may
//! hold the delimiter, newlines and `""` for a quote, and cells that need
//! it are quoted on the way out.

use atd_core::dat::Table;

/// Delimiter choices shared by the menus of Table, Convert, File In and
/// File Out.
pub const MENU: [&str; 6] = ["tab", "comma", "semicolon", "pipe", "space", "whitespace"];

/// The character a menu entry names (`whitespace` splits on runs of
/// spaces and tabs on the way in and writes a space).
pub fn from_menu(name: &str) -> char {
    match name {
        "comma" => ',',
        "semicolon" => ';',
        "pipe" => '|',
        "space" | "whitespace" => ' ',
        _ => '\t',
    }
}

/// Parses `text` into a table. `name` is a [`MENU`] entry.
pub fn parse(text: &str, name: &str) -> Table {
    match name {
        "whitespace" => Table::from_rows(
            atd_core::dat::lines(text).map(|l| l.split_whitespace().collect::<Vec<_>>()),
        ),
        "tab" => Table::parse(text, '\t'),
        _ => parse_quoted(text, from_menu(name)),
    }
}

fn parse_quoted(text: &str, delim: char) -> Table {
    let mut table = Table::new(0);
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut cell));
            table.push_row(std::mem::take(&mut row));
            any = false;
        } else {
            cell.push(c);
        }
    }
    if any {
        row.push(cell);
        table.push_row(row);
    }
    table
}

/// Writes `table` as delimited text (rows end with `\n`, the last too).
pub fn write(table: &Table, name: &str) -> String {
    let delim = from_menu(name);
    let mut out = String::new();
    for row in table.iter_rows().take(table.rows()) {
        for (i, c) in row.iter().enumerate() {
            if i > 0 {
                out.push(delim);
            }
            let needs = delim != '\t'
                && (c.contains(delim) || c.contains('"') || c.contains('\n') || c.contains('\r'));
            if needs {
                out.push('"');
                out.push_str(&c.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(c);
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_cells_round_trip() {
        let t = Table::from_rows([
            vec!["name", "note"],
            vec!["a,b", "say \"hi\""],
            vec!["two\nlines", ""],
        ]);
        let csv = write(&t, "comma");
        assert_eq!(
            csv,
            "name,note\n\"a,b\",\"say \"\"hi\"\"\"\n\"two\nlines\",\n"
        );
        assert_eq!(parse(&csv, "comma"), t);
    }

    #[test]
    fn delimiters_split_cells() {
        assert_eq!(parse("a\tb\nc\td\n", "tab").cell(1, 1), Some("d"));
        assert_eq!(parse("a;b\r\nc;d", "semicolon").cell(1, 0), Some("c"));
        let ws = parse("  x   y\tz\n", "whitespace");
        assert_eq!(ws.row(0).unwrap(), ["x", "y", "z"]);
        assert_eq!(parse("", "comma").rows(), 0);
        assert_eq!(write(&Table::from_rows([["a", "b"]]), "tab"), "a\tb\n");
    }
}
