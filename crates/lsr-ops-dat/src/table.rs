//! Table and Text (values typed in) and Convert (between the forms).

use lsr_core::dat::{Dat, Table};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{DatCtx, DatOp};

use crate::{dat_in, delim, op_schema};

pub fn table_schema() -> OperatorSchema {
    op_schema(
        "table",
        "Table",
        "A table typed in: one row per line, cells split by the delimiter (CSV-style quotes allowed for every delimiter but tab). Rows shorter than the widest are padded with empty cells. Feeds Select, Prompt Template, Convert and anything that reads a DAT.",
        vec![
            ParamSchema::string("rows", "Rows", "name\tvalue\nsubject\ta lighthouse\nstyle\tink on paper")
                .multiline()
                .help("The table as text, one row per line."),
            ParamSchema::menu("delimiter", "Delimiter", &delim::MENU, "tab").help(
                "What separates cells: tab, comma, semicolon, pipe, a space, or `whitespace` (any run of spaces and tabs).",
            ),
        ],
        vec![],
        100,
    )
}

pub fn text_schema() -> OperatorSchema {
    op_schema(
        "text",
        "Text",
        "Text typed in: a prompt, a note, JSON or a script for other DAT nodes to read. Read as a table it is one row per line.",
        vec![
            ParamSchema::string("text", "Text", "")
                .multiline()
                .help("The text."),
        ],
        vec![],
        20,
    )
}

pub fn convert_schema() -> OperatorSchema {
    op_schema(
        "convert",
        "Convert",
        "Converts its input between text, table and JSON: text to a table (split by the delimiter), a table to delimited text (CSV-style quoting where needed), or either to JSON (rows as objects keyed by the header row, or as arrays). A value already in the wanted form passes through unchanged.",
        vec![
            ParamSchema::menu("to", "To", &["table", "text", "json"], "table")
                .help("The form to convert to."),
            ParamSchema::menu("delimiter", "Delimiter", &delim::MENU, "tab").help(
                "The cell delimiter for text: splitting text into a table, or joining a table into text.",
            ),
            ParamSchema::toggle("header", "Header Row", true).help(
                "To JSON: the first row names the columns, and every other row becomes an object. Off: every row becomes an array.",
            ),
            ParamSchema::toggle("trim", "Trim", false)
                .help("Removes spaces around every cell of a table made from text."),
        ],
        vec![dat_in("in", false)],
        200,
    )
}

#[derive(Debug)]
struct TableOp;

impl DatOp for TableOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        ctx.output
            .replace(Dat::table(delim::parse(p.str("rows"), p.str("delimiter"))));
        Ok(())
    }
}

pub fn create_table() -> Box<dyn DatOp> {
    Box::new(TableOp)
}

#[derive(Debug)]
struct TextOp;

impl DatOp for TextOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let text = ctx.params.str("text");
        if !(ctx.output.is_text() && ctx.output.as_text() == text) {
            *ctx.output = Dat::text(text);
        }
        Ok(())
    }
}

pub fn create_text() -> Box<dyn DatOp> {
    Box::new(TextOp)
}

/// A table as JSON: objects keyed by the first row, or arrays of strings.
pub(crate) fn table_json(table: &Table, header: bool) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut rows = table.iter_rows();
    if !header {
        return Value::Array(
            rows.map(|r| r.iter().map(|c| Value::from(c.as_str())).collect())
                .collect(),
        );
    }
    let Some(names) = rows.next() else {
        return Value::Array(vec![]);
    };
    Value::Array(
        rows.map(|r| {
            let obj: Map<String, Value> = names
                .iter()
                .zip(r)
                .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
                .collect();
            Value::Object(obj)
        })
        .collect(),
    )
}

#[derive(Debug)]
struct ConvertOp;

impl DatOp for ConvertOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.replace(Dat::default());
            return Ok(());
        };
        let delimiter = p.str("delimiter");
        let new = match p.str("to") {
            "text" if input.is_text() => input.clone(),
            "text" => {
                let mut s = delim::write(&input.as_table(), delimiter);
                s.pop();
                Dat::text(s)
            }
            "json" => {
                let v = table_json(&input.as_table(), p.bool("header"));
                Dat::text(serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?)
            }
            _ if !input.is_text() => input.clone(),
            _ => {
                let mut t = delim::parse(&input.as_text(), delimiter);
                if p.bool("trim") {
                    t = Table::from_rows(
                        t.iter_rows()
                            .map(|r| r.iter().map(|c| c.trim().to_owned()).collect::<Vec<_>>()),
                    );
                }
                Dat::table(t)
            }
        };
        if !Dat::ptr_eq(ctx.output, &new) {
            ctx.output.replace(new);
        }
        Ok(())
    }
}

pub fn create_convert() -> Box<dyn DatOp> {
    Box::new(ConvertOp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use serde_json::json;

    #[test]
    fn tables_are_typed_as_delimited_rows() {
        let mut h = Harness::new(table_schema());
        let mut op = create_table();
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.out.cell(1, 1), Some("a lighthouse"));
        let first = h.out.clone();
        h.cook(op.as_mut()).unwrap();
        assert!(Dat::ptr_eq(&first, &h.out), "unchanged rows keep the value");
        h.set("rows", json!("a,\"b, c\"\nd"))
            .set("delimiter", json!("comma"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!((h.out.rows(), h.out.cols()), (2, 2));
        assert_eq!(h.out.cell(0, 1), Some("b, c"));
        assert_eq!(h.out.cell(1, 1), Some(""));
    }

    #[test]
    fn text_is_text() {
        let mut h = Harness::new(text_schema());
        let mut op = create_text();
        h.set("text", json!("line one\nline two"));
        h.cook(op.as_mut()).unwrap();
        assert!(h.out.is_text());
        assert_eq!(h.out.cell(1, 0), Some("line two"));
        let first = h.out.clone();
        h.cook(op.as_mut()).unwrap();
        assert!(Dat::ptr_eq(&first, &h.out));
    }

    #[test]
    fn convert_moves_between_forms() {
        let mut h = Harness::new(convert_schema());
        let mut op = create_convert();
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "", "no input: empty");
        h.input(0, Some(Dat::text(" a , b \nc,d")))
            .set("delimiter", json!("comma"))
            .set("trim", json!(true));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.out.cell(0, 1), Some("b"));
        let table = h.out.clone();
        h.input(0, Some(table.clone())).set("to", json!("text"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "a,b\nc,d");
        h.set("to", json!("json"));
        h.cook(op.as_mut()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&h.text()).unwrap();
        assert_eq!(v, json!([{"a": "c", "b": "d"}]));
        h.set("header", json!(false));
        h.cook(op.as_mut()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&h.text()).unwrap();
        assert_eq!(v, json!([["a", "b"], ["c", "d"]]));
        // Already a table: passed through as the same value.
        h.set("to", json!("table"));
        h.cook(op.as_mut()).unwrap();
        assert!(Dat::ptr_eq(&h.out, &table));
    }
}
