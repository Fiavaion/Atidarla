//! Shared helpers: locate a Python interpreter and the in-repo host package.

use std::path::PathBuf;

use atd_ai::HostCommand;

/// `ATD_PYTHON` if set, else `python` on Windows and `python3` elsewhere.
pub fn python() -> String {
    std::env::var("ATD_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    })
}

pub fn host_command() -> HostCommand {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ai-host/src");
    HostCommand::python_module(python(), src.canonicalize().expect("ai-host/src exists"))
}
