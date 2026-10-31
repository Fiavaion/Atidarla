//! Select: rows and columns of a table, by index, range, name or match.

use atd_core::dat::{Dat, Table};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{DatCtx, DatOp};

use crate::{dat_in, op_schema};

pub fn schema() -> OperatorSchema {
    op_schema(
        "select",
        "Select",
        "Keeps some rows and columns of its input table (text counts as one row per line). Rows are filtered by a match on one column first, then picked by index; columns are picked by name or index. With Header Row on, the first row names the columns, is not counted and is always kept.",
        vec![
            ParamSchema::toggle("header", "Header Row", true).help(
                "The first row names the columns: it is kept, and row indices start after it.",
            ),
            ParamSchema::string("rows", "Rows", "").help(
                "Rows to keep, counted from 0: indices and ranges separated by commas (`0, 2-4, 7-`). Empty: all.",
            ),
            ParamSchema::string("cols", "Columns", "").help(
                "Columns to keep, in this order: names (with Header Row on) or indices and ranges (`name, 2, 4-`). Empty: all.",
            ),
            ParamSchema::string("match_col", "Match Column", "").help(
                "The column (name or index) whose cells must match `Match`. Empty: no filter.",
            ),
            ParamSchema::string("match", "Match", "*").help(
                "Pattern a row's cell must match to be kept: `*` is any text, `?` any one character; letter case is ignored.",
            ),
        ],
        vec![dat_in("in", false)],
        300,
    )
}

/// Indices picked by `spec` (`0, 2-4, 7-`) among `len`, in order. Items
/// out of range are skipped; anything else is an error.
fn indices(
    spec: &str,
    len: usize,
    mut name: impl FnMut(&str) -> Option<usize>,
) -> Result<Vec<usize>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok((0..len).collect());
    }
    let mut out = Vec::new();
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let num = |s: &str| s.trim().parse::<usize>().ok();
        if let Some(i) = num(item) {
            if i < len {
                out.push(i);
            }
        } else if let Some((a, b)) = item.split_once('-')
            && let Some(a) = num(a)
        {
            let end = if b.trim().is_empty() {
                len
            } else {
                num(b)
                    .ok_or_else(|| format!("`{item}` is not a range"))?
                    .saturating_add(1)
            };
            out.extend(a..end.min(len));
        } else if let Some(i) = name(item) {
            out.push(i);
        } else {
            return Err(format!("no column `{item}`"));
        }
    }
    Ok(out)
}

/// Whether `text` matches `pattern` (`*`, `?`), ignoring letter case.
fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    // Classic wildcard match with one backtrack point.
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn select(table: &Table, p: &atd_gpu::engine::Params<'_>) -> Result<Table, String> {
    let header = p.bool("header") && table.rows() > 0;
    let skip = usize::from(header);
    let col_named = |s: &str| if header { table.col_index(s) } else { None };
    let mut rows: Vec<usize> = (skip..table.rows()).collect();
    let match_col = p.str("match_col").trim();
    if !match_col.is_empty() {
        let c = match match_col.parse::<usize>() {
            Ok(i) if i < table.cols() => i,
            _ => col_named(match_col).ok_or_else(|| format!("no column `{match_col}`"))?,
        };
        let pattern = p.str("match");
        rows.retain(|&r| matches(pattern, table.cell(r, c).unwrap_or("")));
    }
    let picked = indices(p.str("rows"), rows.len(), |_| None)?;
    let cols = indices(p.str("cols"), table.cols(), col_named)?;
    let mut out = Table::new(cols.len());
    let row_cells = |r: usize| {
        cols.iter()
            .map(move |&c| table.cell(r, c).unwrap_or("").to_owned())
    };
    if header {
        out.push_row(row_cells(0));
    }
    for i in picked {
        out.push_row(row_cells(rows[i]));
    }
    Ok(out)
}

#[derive(Debug)]
struct SelectOp;

impl DatOp for SelectOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.replace(Dat::table(Table::new(0)));
            return Ok(());
        };
        let t = select(&input.as_table(), &ctx.params)?;
        ctx.output.replace(Dat::table(t));
        Ok(())
    }
}

pub fn create() -> Box<dyn DatOp> {
    Box::new(SelectOp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use serde_json::json;

    fn people() -> Dat {
        Dat::table(Table::from_rows([
            ["name", "role", "city"],
            ["Ada", "lights", "Vilnius"],
            ["Bo", "sound", "Kaunas"],
            ["Cy", "lights", "Riga"],
            ["Di", "video", "Vilnius"],
        ]))
    }

    #[test]
    fn wildcards_match_ignoring_case() {
        assert!(matches("*", ""));
        assert!(matches("vil*", "Vilnius"));
        assert!(matches("?o", "Bo"));
        assert!(matches("*i*a", "Riga"));
        assert!(!matches("vil", "Vilnius"));
        assert!(!matches("?", ""));
    }

    #[test]
    fn rows_and_columns_are_picked() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.input(0, Some(people()));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.out.rows(), 5, "everything by default");
        h.set("rows", json!("0, 2-")).set("cols", json!("city, 0"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "city\tname\nVilnius\tAda\nRiga\tCy\nVilnius\tDi");
        h.set("rows", json!(""))
            .set("cols", json!("name"))
            .set("match_col", json!("role"))
            .set("match", json!("LIGHTS"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "name\nAda\nCy");
        h.set("header", json!(false))
            .set("match_col", json!(""))
            .set("cols", json!("1"))
            .set("rows", json!("0"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "role");
        h.set("cols", json!("nope"));
        assert!(h.cook(op.as_mut()).unwrap_err().contains("nope"));
        h.set("cols", json!("")).set("rows", json!("1-x"));
        assert!(h.cook(op.as_mut()).is_err());
    }

    #[test]
    fn text_is_selected_by_line() {
        let mut h = Harness::new(schema());
        let mut op = create();
        h.input(0, Some(Dat::text("a\nb\nc")))
            .set("header", json!(false))
            .set("rows", json!("1-"));
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "b\nc");
    }
}
