//! JSON: parses JSON (typed in, or from an input such as File In or Web
//! Client), picks a part of it with a JSON Pointer and gives it as a table
//! or as pretty text.

use lsr_core::dat::{Dat, Table};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{DatCtx, DatOp};
use serde_json::Value;

use crate::{dat_in, op_schema};

pub fn schema() -> OperatorSchema {
    op_schema(
        "json",
        "JSON",
        "Parses JSON from its input (File In, Web Client, Text) or, with nothing connected, from the JSON parameter; `Path` picks a part of it (a JSON Pointer such as `/items` or `/items/0/name`). As a table: an array of objects becomes a header row of their keys and a row per object, an array of arrays a row per array, an object a key/value row per member (keys in alphabetical order), and a single value one cell; strings are cells as they are, other values their JSON text. As text: the part pretty-printed (strings as they are).",
        vec![
            ParamSchema::string(
                "json",
                "JSON",
                "{\"subject\": \"a lighthouse\", \"style\": \"ink on paper\"}",
            )
            .multiline()
            .help("The JSON to parse when the input is not connected."),
            ParamSchema::string("path", "Path", "")
                .help("A JSON Pointer to the part to output (`/items/0`); empty: all of it."),
            ParamSchema::menu("output", "Output", &["table", "text"], "table")
                .help("A table (see above) or the part as text."),
        ],
        vec![dat_in("in", true)],
        300,
    )
}

/// A JSON value as one cell.
fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A JSON value as a table (see the schema summary).
pub(crate) fn to_table(v: &Value) -> Table {
    match v {
        Value::Array(items) if items.iter().all(Value::is_object) && !items.is_empty() => {
            let mut keys: Vec<&str> = Vec::new();
            for item in items {
                for k in item.as_object().into_iter().flat_map(|o| o.keys()) {
                    if !keys.contains(&k.as_str()) {
                        keys.push(k);
                    }
                }
            }
            let mut t = Table::new(keys.len());
            t.push_row(keys.iter().map(|k| (*k).to_owned()));
            for item in items {
                t.push_row(
                    keys.iter()
                        .map(|k| item.get(*k).map(cell).unwrap_or_default()),
                );
            }
            t
        }
        Value::Array(items) => Table::from_rows(items.iter().map(|item| match item {
            Value::Array(cells) => cells.iter().map(cell).collect::<Vec<_>>(),
            other => vec![cell(other)],
        })),
        Value::Object(members) => {
            Table::from_rows(members.iter().map(|(k, v)| vec![k.clone(), cell(v)]))
        }
        other => Table::from_rows([[cell(other)]]),
    }
}

#[derive(Debug)]
struct JsonOp;

impl DatOp for JsonOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let parsed: Result<Value, _> = match ctx.inputs.get(0) {
            // Nothing yet (a file still loading): empty, not an error.
            Some(input) if input.as_text().trim().is_empty() => {
                ctx.output.replace(Dat::table(Table::new(0)));
                return Ok(());
            }
            Some(input) => serde_json::from_str(&input.as_text()),
            None => serde_json::from_str(p.str("json")),
        };
        let v = parsed.map_err(|e| format!("not JSON: {e}"))?;
        let path = p.str("path").trim();
        let part = if path.is_empty() {
            &v
        } else {
            v.pointer(path)
                .ok_or_else(|| format!("`{path}` is not in the JSON"))?
        };
        let new = match p.str("output") {
            "text" => Dat::text(match part {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).map_err(|e| e.to_string())?,
            }),
            _ => Dat::table(to_table(part)),
        };
        ctx.output.replace(new);
        Ok(())
    }
}

pub fn create() -> Box<dyn DatOp> {
    Box::new(JsonOp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use serde_json::json;

    #[test]
    fn json_shapes_become_tables() {
        let t = to_table(&json!([{"a": 1, "b": "x"}, {"b": "y", "c": null}]));
        assert_eq!(t.row(0).unwrap(), ["a", "b", "c"]);
        assert_eq!(t.row(1).unwrap(), ["1", "x", ""]);
        assert_eq!(t.row(2).unwrap(), ["", "y", ""]);
        let t = to_table(&json!([[1, 2], [3], "z"]));
        assert_eq!(t.row(1).unwrap(), ["3", ""]);
        assert_eq!(t.row(2).unwrap(), ["z", ""]);
        let t = to_table(&json!({"k": {"n": true}}));
        assert_eq!(t.row(0).unwrap(), ["k", "{\"n\":true}"]);
        assert_eq!(to_table(&json!(2.5)).cell(0, 0), Some("2.5"));
        assert_eq!(to_table(&json!([])).rows(), 0);
    }

    #[test]
    fn the_input_wins_and_paths_pick_parts() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.cook(op.as_mut()).unwrap();
        // Object members come in key order.
        assert_eq!(h.out.cell(0, 0), Some("style"));
        h.input(
            0,
            Some(Dat::text(r#"{"items": [{"name": "a"}, {"name": "b"}]}"#)),
        )
        .set("path", json!("/items/1/name"))
        .set("output", json!("text"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "b");
        h.set("path", json!("/items")).set("output", json!("table"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.out.cell(2, 0), Some("b"));
        h.set("path", json!("/nope"));
        let e = h.cook(op.as_mut()).unwrap_err();
        assert!(e.contains("/nope"), "{e}");
        // The last good value stays.
        assert_eq!(h.out.cell(2, 0), Some("b"));
        h.input(0, Some(Dat::text(" ")));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.out.rows(), 0, "empty input: empty table");
        h.input(0, Some(Dat::text("{oops")));
        assert!(h.cook(op.as_mut()).unwrap_err().starts_with("not JSON"));
    }
}
