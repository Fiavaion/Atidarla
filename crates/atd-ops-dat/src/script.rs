//! Script's schema. Its implementation runs Python, so it lives with the
//! embedded interpreter (`atd_py::script`), which adds it to the library.

use atd_core::registry::{OperatorSchema, ParamSchema};

use crate::{dat_in, op_schema};

pub const DEFAULT_CODE: &str = "def cook(inputs):
    # inputs[0], inputs[1]: None, a str (text) or a list of rows (a table).
    table = inputs[0] or []
    return [[cell.upper() for cell in row] for row in table]
";

pub fn schema() -> OperatorSchema {
    let mut s = op_schema(
        "script",
        "Script",
        "Python that turns DAT inputs into a DAT value: the code defines `cook(inputs)`, where `inputs` has one entry per input (`None`, a `str` for text, a list of rows of `str` for a table), and returns a `str` (text), a list of rows or of values (a table), a dict (name/value rows) or `None` (empty). It runs on a worker thread whenever the code or an input changes, never on the frame (offline renders wait for it). Script is project code: it runs only once you allow the project's Python (the app's Allow dialog; `--allow-python-ops` for `check` and `render`).",
        vec![
            ParamSchema::string("code", "Code", DEFAULT_CODE)
                .multiline()
                .help("Python defining `cook(inputs)`; the rest of the file runs once per edit."),
        ],
        vec![dat_in("in0", true), dat_in("in1", true)],
        50,
    );
    // Polls its worker every frame (cheap); consumers cook only on change.
    s.time_dependent = true;
    s
}
