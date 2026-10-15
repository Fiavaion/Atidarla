//! PyO3 bindings: the `atidarla` Python module embedded in the app, with a
//! TouchDesigner-familiar API (`op('/proj/noise1').par.period = 2`),
//! callbacks and custom-op base classes (PRD §5.2 `atd-py`).
//!
//! Python runs on a worker thread and never on the render thread
//! (CLAUDE.md rule 1). Python 3.13 is the default, 3.11 the floor (PRD A5).
//!
//! **Status (M0):** the module skeleton and embedding entry point, so CI
//! proves PyO3 builds and links against CPython on every platform. The
//! object model lands in M1 (expressions) and M2 (textport, Python ops).

use pyo3::prelude::*;

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
/// Must be called once, before any other Python use in the process.
pub fn init_embedded() {
    pyo3::append_to_inittab!(atidarla);
    Python::initialize();
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
