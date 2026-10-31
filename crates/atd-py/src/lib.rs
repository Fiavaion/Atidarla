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

pub mod allow;
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

/// Starts the embedded interpreter once per process: with the installer's
/// bundled runtime as its home when there is one (PRD A68), else as linked
/// (a development build's system or Homebrew Python).
pub(crate) fn ensure_initialized() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        pyo3::append_to_inittab!(atidarla);
        if let Some(home) = atd_core::runtime::bundled_python_home() {
            init_bundled(&home);
        }
        // Does nothing when `init_bundled` started the interpreter.
        Python::initialize();
    });
}

/// Starts the interpreter with `home` as `sys.prefix` and the bundled
/// `python3` as `sys.executable` (so `subprocess` and `multiprocessing`
/// start Python, not the app). On failure it logs and leaves the
/// interpreter for [`Python::initialize`] to start as linked.
fn init_bundled(home: &std::path::Path) {
    use pyo3::ffi;
    let program = atd_core::runtime::python_executable(home);
    // SAFETY: runs once, before any other use of Python in the process (the
    // `Once` in `ensure_initialized`); `config` is initialised by
    // `PyConfig_InitPythonConfig` before use and cleared after.
    unsafe {
        let mut config = std::mem::MaybeUninit::<ffi::PyConfig>::uninit();
        let cfg = config.as_mut_ptr();
        ffi::PyConfig_InitPythonConfig(cfg);
        let mut status = set_config_path(cfg, &raw mut (*cfg).home, home);
        if ffi::PyStatus_Exception(status) == 0 {
            status = set_config_path(cfg, &raw mut (*cfg).executable, &program);
        }
        if ffi::PyStatus_Exception(status) == 0 {
            status = ffi::Py_InitializeFromConfig(cfg);
        }
        ffi::PyConfig_Clear(cfg);
        if ffi::PyStatus_Exception(status) != 0 {
            let msg = if status.err_msg.is_null() {
                "unknown error".into()
            } else {
                std::ffi::CStr::from_ptr(status.err_msg).to_string_lossy()
            };
            log::error!("bundled Python at {} did not start: {msg}", home.display());
            return;
        }
        // Release the GIL, as `Python::initialize` does after starting.
        ffi::PyEval_SaveThread();
    }
    log::info!("Python: bundled runtime at {}", home.display());
}

/// Sets a `PyConfig` path field without losing non-ASCII characters: the
/// raw bytes on Unix (decoded as the file system does), UTF-16 on Windows.
///
/// # Safety
/// `cfg` is an initialised config and `field` one of its string fields
/// (`W` is the platform's `wchar_t`).
unsafe fn set_config_path<W>(
    cfg: *mut pyo3::ffi::PyConfig,
    field: *mut *mut W,
    path: &std::path::Path,
) -> pyo3::ffi::PyStatus {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(bytes) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            // SAFETY: no preconditions.
            return unsafe { pyo3::ffi::PyStatus_Error(c"path holds a NUL byte".as_ptr()) };
        };
        // SAFETY: as documented on this function; `bytes` outlives the call,
        // which copies it.
        unsafe { pyo3::ffi::PyConfig_SetBytesString(cfg, field.cast(), bytes.as_ptr()) }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: as documented on this function; `wide` is NUL-terminated
        // UTF-16 (Windows' wchar_t) and outlives the call, which copies it.
        unsafe { pyo3::ffi::PyConfig_SetString(cfg, field.cast(), wide.as_ptr().cast()) }
    }
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
