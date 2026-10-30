//! Shared helpers: start hosts (the Rust reference host, or the Python host
//! in `ai-host/`) listening on loopback.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

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

/// The Python host from the repository (standard library only for the
/// built-in models).
pub fn python_host() -> HostCommand {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ai-host/src");
    HostCommand::python_module(python(), src.canonicalize().expect("ai-host/src exists"))
}

/// The Rust reference host.
pub fn ref_host() -> HostCommand {
    HostCommand {
        program: env!("CARGO_BIN_EXE_atd-ai-refhost").into(),
        args: vec![],
        env: vec![],
    }
}

/// A host process listening on loopback; killed on drop.
pub struct Listening {
    pub child: Child,
    pub addr: String,
}

impl Drop for Listening {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `cmd` with `--listen 127.0.0.1:0` and `extra` arguments.
pub fn listen(cmd: &HostCommand, extra: &[&str]) -> Listening {
    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args)
        .args(["--listen", "127.0.0.1:0"])
        .args(extra)
        .stdout(Stdio::piped())
        .stdin(Stdio::null());
    for (k, v) in &cmd.env {
        c.env(k, v);
    }
    let mut child = c.spawn().expect("host starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("listening line");
    let addr = line
        .trim()
        .strip_prefix("atd-ai-host listening on ")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .to_owned();
    Listening { child, addr }
}

pub fn fill(frame: &mut [u8], seed: u64) {
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i as u64).wrapping_mul(31).wrapping_add(seed) as u8;
    }
}

pub fn inverted(frame: &[u8]) -> Vec<u8> {
    frame
        .chunks_exact(4)
        .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]])
        .collect()
}
