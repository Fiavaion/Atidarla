//! Prompt Template: text with `{name}` placeholders filled from a table,
//! a text input and channels, for LLM Chat, diffusion prompts or a Web
//! Client request body.

use lsr_core::dat::{Dat, Table};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{Channels, DatCtx, DatOp};

use crate::{dat_in, op_schema, sig_in};

pub fn schema() -> OperatorSchema {
    op_schema(
        "prompt_template",
        "Prompt Template",
        "Fills `{name}` placeholders in its template: from the `values` table (a name and a value per row, or a header row of names over rows of values), from channels on the `sig` input (formatted to `Precision` decimals), and `{text}` from the `text` input (a Speech to Text transcript, say). `{{` and `}}` write braces. Outputs text for LLM Chat, a diffusion prompt or a Web Client body; it changes only when the filled text does.",
        vec![
            ParamSchema::string("template", "Template", "Describe {subject} in the style of {style}. {text}")
                .multiline()
                .help("The text, with `{name}` where a value goes."),
            ParamSchema::menu("layout", "Layout", &["pairs", "columns"], "pairs").help(
                "How the `values` table holds its values: `pairs` is a name and a value per row; `columns` is a header row of names over rows of values (see `Row`).",
            ),
            ParamSchema::int("row", "Row", 1, Some(1), None)
                .help("With `columns`: the row of values to use (1 is the first after the header)."),
            ParamSchema::menu("missing", "Missing", &["keep", "empty", "error"], "keep").help(
                "A placeholder with no value: kept as written, left empty, or a node error.",
            ),
            ParamSchema::int("precision", "Precision", 3, Some(0), Some(9))
                .help("Decimals of channel values."),
        ],
        vec![dat_in("values", true), dat_in("text", true), sig_in("sig")],
        200,
    )
}

/// What placeholders are filled from.
struct Values<'a> {
    table: Option<&'a Table>,
    columns: bool,
    row: usize,
    text: Option<&'a str>,
    sig: Option<&'a Channels>,
    precision: usize,
}

impl Values<'_> {
    fn get(&self, name: &str, out: &mut String) -> bool {
        if let Some(t) = self.table {
            let found = if self.columns {
                t.col_index(name).and_then(|c| t.cell(self.row, c))
            } else {
                t.iter_rows()
                    .find(|r| r.first().is_some_and(|n| n.trim() == name))
                    .map(|r| r.get(1).map_or("", String::as_str))
            };
            if let Some(v) = found {
                out.push_str(v);
                return true;
            }
        }
        if let Some(v) = self.sig.and_then(|c| c.value(name)) {
            use std::fmt::Write as _;
            let _ = write!(out, "{v:.*}", self.precision);
            return true;
        }
        if name == "text"
            && let Some(t) = self.text
        {
            out.push_str(t);
            return true;
        }
        false
    }
}

/// Fills `template` into `out` (cleared first); `missing` is a menu entry
/// of the schema.
fn fill(
    template: &str,
    values: &Values<'_>,
    missing: &str,
    out: &mut String,
) -> Result<(), String> {
    out.clear();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(after) = tail.strip_prefix('}') {
            out.push('}');
            rest = after;
            continue;
        }
        let Some(end) = tail.find('}') else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let name = tail[1..end].trim();
        if !values.get(name, out) {
            match missing {
                "empty" => {}
                "error" => return Err(format!("no value for `{{{name}}}`")),
                _ => out.push_str(&tail[..=end]),
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(())
}

/// Fills into a buffer it keeps, so a frame whose text did not change
/// (channels steady, or rounding to the same digits) allocates nothing.
#[derive(Debug, Default)]
struct TemplateOp {
    buf: String,
}

impl DatOp for TemplateOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let table = ctx.inputs.get(0).map(|d| d.as_table());
        let text = ctx.inputs.get(1).map(|d| d.as_text());
        let values = Values {
            table: table.as_deref(),
            columns: p.str("layout") == "columns",
            row: p.i64("row").max(1) as usize,
            text: text.as_deref(),
            sig: ctx.sig_inputs.get(2),
            precision: p.i64("precision").clamp(0, 9) as usize,
        };
        fill(p.str("template"), &values, p.str("missing"), &mut self.buf)?;
        if !(ctx.output.is_text() && ctx.output.as_text() == self.buf) {
            *ctx.output = Dat::text(self.buf.as_str());
        }
        Ok(())
    }
}

pub fn create() -> Box<dyn DatOp> {
    Box::<TemplateOp>::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use serde_json::json;

    #[test]
    fn placeholders_fill_from_pairs_text_and_channels() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.input(
            0,
            Some(Dat::table(Table::from_rows([
                ["subject", "a lighthouse"],
                ["style", "ink on paper"],
            ]))),
        )
        .input(1, Some(Dat::text("at night")))
        .sig_input(2, Channels::single(&["speed"], &[0.5]))
        .set(
            "template",
            json!("{subject}, {style}, {text}, {speed} {{x}} {nope}"),
        )
        .set("precision", json!(1));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(
            h.text(),
            "a lighthouse, ink on paper, at night, 0.5 {x} {nope}"
        );
        let first = h.out.clone();
        h.cook(op.as_mut()).unwrap();
        assert!(Dat::ptr_eq(&first, &h.out), "same text, same value");
        h.set("missing", json!("empty"));
        h.cook(op.as_mut()).unwrap();
        assert!(h.text().ends_with("{x} "));
        h.set("missing", json!("error"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("{nope}"));
    }

    #[test]
    fn column_layout_reads_a_row_under_the_header() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.input(
            0,
            Some(Dat::table(Table::from_rows([
                ["subject", "style"],
                ["a fox", "pixel art"],
                ["a ship", "oil paint"],
            ]))),
        )
        .set("layout", json!("columns"))
        .set("row", json!(2))
        .set("template", json!("{subject} as {style}} {"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "a ship as oil paint} {");
    }
}
