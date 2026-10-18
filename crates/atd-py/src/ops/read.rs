//! Reading `ops/*.py` declarations without running them.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema};
use pyo3::prelude::*;
use pyo3::types::PyModule;
use serde::Deserialize;

use super::{OPS_DIR, OpDecl, OpsError, OpsFile};

/// What identifies a file's version cheaply: modification time and size.
pub(crate) type Stamp = Option<(SystemTime, u64)>;

/// `ops/` as read.
#[derive(Debug)]
pub(crate) struct Scan {
    pub files: BTreeMap<String, OpsFile>,
    /// The combined hash; `None` when nothing changed since `known`.
    pub hash: Option<String>,
    /// A file was added, removed or changed since `known`.
    pub changed: bool,
}

#[derive(Deserialize)]
struct RawRead {
    sha256: String,
    #[serde(default)]
    ops: Vec<RawOp>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    line: Option<u32>,
    message: String,
}

#[derive(Deserialize)]
struct RawOp {
    name: String,
    family: String,
    label: String,
    summary: String,
    params: Vec<ParamSchema>,
    inputs: Vec<SocketSchema>,
    state: serde_json::Map<String, serde_json::Value>,
    size: Option<[u32; 2]>,
    timeout: f64,
    line: u32,
}

/// The process's declaration reader (the helper module; it runs no
/// project code).
fn reader(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    static READER: OnceLock<Py<PyModule>> = OnceLock::new();
    if let Some(m) = READER.get() {
        return Ok(m.bind(py).clone());
    }
    let m = super::host::helper_module(py, "atd_ops_reader")?;
    let _ = READER.set(m.clone().unbind());
    Ok(m)
}

/// A file name usable as a Python module and in a type ID.
fn valid_stem(stem: &str) -> bool {
    let mut b = stem.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// Reads `dir` (a project's `ops/`), re-reading only files whose stamp
/// differs from `known`.
pub(crate) fn scan(dir: &Path, known: &BTreeMap<String, OpsFile>) -> Scan {
    crate::ensure_initialized();
    let mut files = BTreeMap::new();
    let entries: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "py"))
            .collect(),
        Err(_) => Vec::new(),
    };
    for path in entries {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let stamp = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if let Some(old) = known
            .get(&stem)
            .filter(|o| o.stamp.is_some() && o.stamp == stamp)
        {
            files.insert(stem, old.clone());
            continue;
        }
        let file = read_file(&path, &stem, stamp);
        files.insert(stem, file);
    }
    let changed = files.len() != known.len()
        || files
            .iter()
            .any(|(k, f)| known.get(k).is_none_or(|o| o.sha256 != f.sha256));
    let hash = (changed || known.is_empty()).then(|| {
        let pairs: Vec<(String, String)> = files
            .values()
            .map(|f| (format!("{}.py", f.stem), f.sha256.clone()))
            .collect();
        Python::attach(|py| {
            reader(py)
                .and_then(|r| {
                    r.getattr("combined_hash")?
                        .call1((pairs,))?
                        .extract::<String>()
                })
                .unwrap_or_else(|e| {
                    log::error!("Python operators: {e}");
                    String::new()
                })
        })
    });
    Scan {
        files,
        hash,
        changed,
    }
}

fn read_file(path: &Path, stem: &str, stamp: Stamp) -> OpsFile {
    let rel = format!("{OPS_DIR}/{stem}.py");
    let err = |line, message: String| OpsError {
        file: rel.clone(),
        line,
        message,
    };
    let bytes = std::fs::read(path).unwrap_or_default();
    let (source, mut error) = match String::from_utf8(bytes) {
        Ok(s) => (s, None),
        Err(e) => (
            String::from_utf8_lossy(e.as_bytes()).into_owned(),
            Some(err(None, "the file is not UTF-8 text".into())),
        ),
    };
    if error.is_none() && !valid_stem(stem) {
        error = Some(err(
            None,
            "file names in ops/ use a-z, 0-9 and _ (they become part of operator type IDs)".into(),
        ));
    }
    let (sha256, result) = read_source(stem, &source);
    let ops = match (error.is_none(), result) {
        (true, Ok(ops)) => ops,
        (true, Err(e)) => {
            error = Some(e);
            Vec::new()
        }
        (false, _) => Vec::new(),
    };
    OpsFile {
        stem: stem.to_owned(),
        sha256,
        ops,
        error,
        source: Arc::from(source),
        stamp,
    }
}

/// The SHA-256 of `source` and its declarations (or the first error).
pub(crate) fn read_source(
    stem: &str,
    source: &str,
) -> (String, Result<Vec<Arc<OpDecl>>, OpsError>) {
    let file = format!("{OPS_DIR}/{stem}.py");
    let err = |line, message: String| OpsError {
        file: file.clone(),
        line,
        message,
    };
    let json = Python::attach(|py| -> PyResult<String> {
        reader(py)?
            .getattr("read")?
            .call1((source, &file))?
            .extract::<String>()
    });
    let raw: RawRead = match json.map_err(|e| e.to_string()).and_then(|j| {
        serde_json::from_str(&j).map_err(|e| format!("reading the declarations: {e}"))
    }) {
        Ok(r) => r,
        Err(e) => return (String::new(), Err(err(None, e))),
    };
    if let Some(e) = raw.error {
        return (raw.sha256, Err(err(e.line, e.message)));
    }
    let mut ops = Vec::new();
    for op in raw.ops {
        match decl(stem, op) {
            Ok(d) => ops.push(Arc::new(d)),
            Err((line, message)) => return (raw.sha256, Err(err(Some(line), message))),
        }
    }
    (raw.sha256, Ok(ops))
}

fn decl(stem: &str, op: RawOp) -> Result<OpDecl, (u32, String)> {
    let line = op.line;
    let family = match op.family.as_str() {
        "SIG" => OpFamily::Sig,
        "IMG" => OpFamily::Img,
        other => return Err((line, format!("family {other} is not supported"))),
    };
    for p in &op.params {
        p.validate(&p.default).map_err(|e| (line, e))?;
    }
    let summary = if op.summary.is_empty() {
        format!("Python operator from {OPS_DIR}/{stem}.py.")
    } else {
        op.summary
    };
    let schema = OperatorSchema {
        type_id: format!("py.{stem}.{}", op.name),
        family,
        label: op.label,
        summary,
        version: 1,
        params: op.params,
        inputs: op.inputs,
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: super::family_wire(family),
            optional: false,
            feedback: false,
        }],
        capabilities: Vec::new(),
        // The cook thread's share: copying values into the job and the
        // result out (the Python cook itself runs on the worker).
        cook_budget_us: 100,
        // Python operators cook every frame (state, time; results arrive
        // asynchronously).
        time_dependent: true,
        sink: false,
    };
    let size = op.size.map_or((0, 0), |[w, h]| (w, h));
    Ok(OpDecl {
        param_names: schema
            .params
            .iter()
            .map(|p| Arc::from(p.name.as_str()))
            .collect(),
        input_names: schema
            .inputs
            .iter()
            .map(|s| Arc::from(s.name.as_str()))
            .collect(),
        schema,
        file: stem.to_owned(),
        name: op.name,
        line,
        size,
        timeout: Duration::from_secs_f64(op.timeout),
        state: op.state,
    })
}
