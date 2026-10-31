//! PyO3 bindings: the `atidarla` Python module embedded in the app, with a
//! TouchDesigner-familiar API (`op('/proj/noise1').par.period = 2`),
//! callbacks and custom-op base classes (PRD §5.2 `atd-py`).
//!
//! Python runs on a worker thread and never on the render thread
//! (CLAUDE.md rule 1). Python 3.13 is the default, 3.11 the floor (PRD A5).
//!
//! **Status (M2 pre-work):** expressions (A22, [`expr`]), Python custom
//! operators ([`ops`], `docs/python-ops.md`), the Script DAT ([`script`])
//! and the textport ([`textport`], `docs/textport.md`).

use pyo3::prelude::*;

pub mod expr;
pub mod ops;
pub mod script;
pub mod textport;

/// The embedded `atidarla` module.
#[pymodule]
pub mod atidarla {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

    /// Atidarla version string.
    #[pyfunction]
    fn version() -> &'static str {
        atd_core::VERSION
    }

    /// Host platform facts (OS, arch) as a dict.
    #[pyfunction]
    fn platform(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
        let host = atd_core::PlatformInfo::host();
        let dict = PyDict::new(py);
        dict.set_item("os", host.os.token())?;
        dict.set_item("arch", format!("{:?}", host.arch).to_lowercase())?;
        Ok(dict)
    }
}

/// Registers the `atidarla` module and starts the embedded interpreter.
/// Must be called before any other Python use in the process; later calls
/// do nothing.
pub fn init_embedded() {
    ensure_initialized();
}

/// Starts the embedded interpreter once per process.
pub(crate) fn ensure_initialized() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        pyo3::append_to_inittab!(atidarla);
        Python::initialize();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_module_imports() {
        init_embedded();
        Python::attach(|py| {
            let module = py.import("atidarla").unwrap();
            let version: String = module
                .getattr("version")
                .unwrap()
                .call0()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(version, atd_core::VERSION);
            let platform = module.getattr("platform").unwrap().call0().unwrap();
            let os: String = platform.get_item("os").unwrap().extract().unwrap();
            assert_eq!(os, atd_core::Os::current().token());
            let minor: u8 = py.version_info().minor;
            assert!(
                py.version_info().major == 3 && minor >= 11,
                "Python floor is 3.11"
            );
        });
    }
}
