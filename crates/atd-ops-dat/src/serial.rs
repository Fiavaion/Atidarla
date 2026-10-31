//! Serial: lines from a serial port (an Arduino, a sensor, a DMX or show
//! controller) as rows, and each new value of the `send` input written to
//! it. The port is opened, read and written on a worker thread (CLAUDE.md
//! rule 1); the cook only swaps lines with it.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atd_core::dat::{Dat, Table};
use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{DatCtx, DatOp};

use crate::net::lock;
use crate::serial_port::{self, Port};
use crate::{dat_in, op_schema};

const RETRY: Duration = Duration::from_secs(1);
/// A line longer than this is cut (a stream with no newlines).
const MAX_LINE: usize = 4096;
const MAX_OUTBOX: usize = 256;

pub fn schema() -> OperatorSchema {
    let mut s = op_schema(
        "serial",
        "Serial",
        "Reads lines from a serial port (8 data bits, no parity, one stop bit) on a worker thread: each line received becomes a row of its output table (a `line` header row, then the newest `Keep` lines, newest last). Each new value on the `send` input is written to the port, followed by the line ending. Reopens the port every second while Active and it is missing or unplugged.",
        vec![
            ParamSchema::string("port", "Port", "").help(
                "The port: `/dev/tty.usbmodem1101` or `/dev/cu.usbserial-…` on macOS, `/dev/ttyUSB0` on Linux, `COM3` on Windows. Empty: closed.",
            ),
            ParamSchema::int("baud", "Baud", 9600, Some(300), Some(4_000_000))
                .help("Bits per second; it must match the device (9600 and 115200 are common)."),
            ParamSchema::menu("line_end", "Line End", &["lf", "crlf", "cr", "none"], "lf")
                .help("Written after each value sent. Lines received may end with any of them."),
            ParamSchema::toggle("active", "Active", true)
                .help("Keeps the port open while on; off closes it."),
            ParamSchema::int("keep", "Keep", 20, Some(1), Some(1000))
                .help("Lines kept in the output table."),
        ],
        vec![dat_in("send", true)],
        40,
    );
    s.time_dependent = true;
    s.sink = true;
    s
}

/// Opens a port by name and baud rate (the real one, or a test double).
pub(crate) type Opener = Arc<dyn Fn(&str, u32) -> io::Result<Box<dyn Port>> + Send + Sync>;

#[derive(Debug, Default)]
struct State {
    lines: VecDeque<String>,
    keep: usize,
    /// Bumped with every line received.
    seq: u64,
    outbox: VecDeque<Vec<u8>>,
    open: bool,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
}

/// Splits what was read into lines (`\n`, `\r\n` or `\r`); keeps the rest.
fn take_lines(partial: &mut Vec<u8>, mut emit: impl FnMut(String)) {
    let mut start = 0;
    let mut i = 0;
    while i < partial.len() {
        let b = partial[i];
        if b == b'\n' || b == b'\r' {
            emit(String::from_utf8_lossy(&partial[start..i]).into_owned());
            if b == b'\r' && partial.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    partial.drain(..start);
    if partial.len() > MAX_LINE {
        emit(String::from_utf8_lossy(partial).into_owned());
        partial.clear();
    }
}

fn session(port: &mut dyn Port, shared: &Shared) -> io::Result<()> {
    let mut buf = [0u8; 1024];
    let mut partial = Vec::new();
    while !shared.stop.load(Ordering::Acquire) {
        let out: Vec<Vec<u8>> = lock(&shared.state).outbox.drain(..).collect();
        for bytes in out {
            port.write_all(&bytes)?;
            port.flush()?;
        }
        let n = match port.read(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                0
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            // A read timed out with nothing (the port's own timeout).
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        partial.extend_from_slice(&buf[..n]);
        let mut s = lock(&shared.state);
        take_lines(&mut partial, |line| {
            s.lines.push_back(line);
            s.seq += 1;
        });
        while s.lines.len() > s.keep.max(1) {
            s.lines.pop_front();
        }
    }
    Ok(())
}

struct Connection {
    shared: Arc<Shared>,
}

impl Connection {
    fn start(opener: Opener, port: String, baud: u32, keep: usize) -> Self {
        let shared = Arc::new(Shared::default());
        lock(&shared.state).keep = keep;
        let s = shared.clone();
        std::thread::Builder::new()
            .name("atd-serial".into())
            .spawn(move || {
                while !s.stop.load(Ordering::Acquire) {
                    let r = opener(&port, baud).and_then(|mut p| {
                        {
                            let mut st = lock(&s.state);
                            st.open = true;
                            st.error = None;
                        }
                        session(p.as_mut(), &s)
                    });
                    {
                        let mut st = lock(&s.state);
                        st.open = false;
                        if let Err(e) = r {
                            st.error = Some(format!("{port}: {e}"));
                        }
                    }
                    let mut waited = Duration::ZERO;
                    while waited < RETRY && !s.stop.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(20));
                        waited += Duration::from_millis(20);
                    }
                }
            })
            .expect("spawning a thread");
        Self { shared }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

struct SerialOp {
    opener: Opener,
    conn: Option<Connection>,
    /// Port, baud and Active the connection was made for.
    applied: Option<(String, i64, bool)>,
    sent: Option<Dat>,
    keep: i64,
    seen: Option<u64>,
    status: String,
}

impl std::fmt::Debug for SerialOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialOp")
            .field("applied", &self.applied)
            .finish_non_exhaustive()
    }
}

impl SerialOp {
    pub(crate) fn with_opener(opener: Opener) -> Self {
        Self {
            opener,
            conn: None,
            applied: None,
            sent: None,
            keep: 0,
            seen: None,
            status: String::new(),
        }
    }
}

impl DatOp for SerialOp {
    fn cook(&mut self, ctx: &mut DatCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): the port stays closed.
            return Ok(());
        }
        let port = p.str("port").trim();
        let baud = p.i64("baud").clamp(300, 4_000_000);
        let active = p.bool("active");
        let keep = p.i64("keep").clamp(1, 1000);
        let same = self
            .applied
            .as_ref()
            .is_some_and(|(n, b, a)| n == port && *b == baud && *a == active);
        if !same {
            self.applied = Some((port.to_owned(), baud, active));
            self.conn = None;
            self.sent = None;
            self.seen = None;
            self.status.clear();
            if active && !port.is_empty() {
                self.conn = Some(Connection::start(
                    self.opener.clone(),
                    port.to_owned(),
                    baud as u32,
                    keep as usize,
                ));
                self.keep = keep;
            }
        }
        let Some(conn) = &self.conn else {
            if self.seen.is_none() {
                self.seen = Some(0);
                ctx.output.replace(Dat::table(Table::from_rows([["line"]])));
            }
            return Ok(());
        };
        let Ok(mut s) = conn.shared.state.try_lock() else {
            return Ok(());
        };
        let send = ctx.inputs.get(0);
        if let Some(d) = send.filter(|d| self.sent.as_ref().is_none_or(|old| !Dat::ptr_eq(old, d)))
        {
            let mut bytes = d.as_text().into_owned().into_bytes();
            bytes.extend_from_slice(match p.str("line_end") {
                "crlf" => b"\r\n",
                "cr" => b"\r",
                "none" => b"",
                _ => b"\n",
            });
            s.outbox.push_back(bytes);
            while s.outbox.len() > MAX_OUTBOX {
                s.outbox.pop_front();
            }
            self.sent = Some(d.clone());
        }
        if keep != self.keep {
            s.keep = keep as usize;
            self.keep = keep;
        }
        if self.seen != Some(s.seq) {
            self.seen = Some(s.seq);
            let mut t = Table::new(1);
            t.push_row(["line"]);
            for l in s
                .lines
                .iter()
                .skip(s.lines.len().saturating_sub(keep as usize))
            {
                t.push_row([l.clone()]);
            }
            ctx.output.replace(Dat::table(t));
        }
        let (open, error) = (s.open, s.error.clone());
        drop(s);
        if open {
            if !self.status.starts_with("open") {
                self.status = format!("open: {port} at {baud} baud");
            }
            Ok(())
        } else {
            if !self.status.starts_with("opening") {
                self.status = format!("opening {port}…");
            }
            error.map_or(Ok(()), Err)
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

pub fn create() -> Box<dyn DatOp> {
    Box::new(SerialOp::with_opener(Arc::new(serial_port::open)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::web::tests::until;
    use serde_json::json;
    use std::io::{Read, Write};

    /// Both ends of a pretend serial line: what the device sends, and what
    /// the node wrote.
    #[derive(Debug, Default)]
    struct Wire {
        to_node: VecDeque<u8>,
        from_node: Vec<u8>,
        unplugged: bool,
    }

    struct MockPort(Arc<Mutex<Wire>>);

    impl Read for MockPort {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut w = lock(&self.0);
            if w.unplugged {
                return Err(io::Error::other("device unplugged"));
            }
            let n = buf.len().min(w.to_node.len());
            for b in buf.iter_mut().take(n) {
                *b = w.to_node.pop_front().unwrap();
            }
            drop(w);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(n)
        }
    }

    impl Write for MockPort {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            lock(&self.0).from_node.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The ports opened: name and baud rate.
    type Opened = Arc<Mutex<Vec<(String, u32)>>>;

    fn mock() -> (Arc<Mutex<Wire>>, Box<dyn DatOp>, Opened) {
        let wire = Arc::new(Mutex::new(Wire::default()));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let (w, o) = (wire.clone(), opened.clone());
        let opener: Opener = Arc::new(move |name: &str, baud: u32| {
            lock(&o).push((name.to_owned(), baud));
            if name == "missing" {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such port"));
            }
            lock(&w).unplugged = false;
            Ok(Box::new(MockPort(w.clone())) as Box<dyn Port>)
        });
        (wire, Box::new(SerialOp::with_opener(opener)), opened)
    }

    #[test]
    fn lines_split_at_any_line_ending() {
        let mut got = Vec::new();
        let mut partial = b"a\r\nb\rc\nhalf".to_vec();
        take_lines(&mut partial, |l| got.push(l));
        assert_eq!(got, ["a", "b", "c"]);
        assert_eq!(partial, b"half");
        let mut long = vec![b'x'; MAX_LINE + 1];
        take_lines(&mut long, |l| got.push(l));
        assert_eq!(got.last().unwrap().len(), MAX_LINE + 1);
        assert!(long.is_empty());
    }

    #[test]
    fn lines_come_in_as_rows_and_values_go_out() {
        let (wire, mut op, opened) = mock();
        let mut h = Harness::new(schema());
        h.cook(op.as_mut()).unwrap();
        assert_eq!(h.text(), "line", "no port: closed, just the header");
        assert!(lock(&opened).is_empty());
        h.set("port", json!("/dev/ttyMOCK"))
            .set("baud", json!(115_200));
        lock(&wire).to_node.extend(b"temp 21.5\r\nhum");
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 2).unwrap();
        assert_eq!(h.out.cell(1, 0), Some("temp 21.5"));
        assert_eq!(
            lock(&opened).as_slice(),
            [("/dev/ttyMOCK".to_owned(), 115_200)]
        );
        assert!(op.status().unwrap().starts_with("open"));
        lock(&wire).to_node.extend(b"id 40\n");
        until(&mut h, op.as_mut(), |h, _| h.out.rows() == 3).unwrap();
        assert_eq!(h.out.cell(2, 0), Some("humid 40"), "a partial line waits");
        // A new value is written once, with the line ending.
        h.input(0, Some(Dat::text("LED ON")))
            .set("line_end", json!("crlf"));
        until(&mut h, op.as_mut(), |_, _| {
            !lock(&wire).from_node.is_empty()
        })
        .unwrap();
        for _ in 0..20 {
            h.cook(op.as_mut()).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(lock(&wire).from_node, b"LED ON\r\n");
        // Unplugged: an error, then reopened.
        lock(&wire).unplugged = true;
        until(&mut h, op.as_mut(), |_, r| r.is_err()).unwrap_err();
        until(&mut h, op.as_mut(), |_, r| {
            r.is_ok() && lock(&opened).len() == 2
        })
        .unwrap();
        // Inactive closes it.
        h.set("active", json!(false));
        h.cook(op.as_mut()).unwrap();
        assert!(op.status().is_none());
    }

    #[test]
    fn a_missing_port_is_an_error() {
        let (_, mut op, _) = mock();
        let mut h = Harness::new(schema());
        h.set("port", json!("missing"));
        let e = until(&mut h, op.as_mut(), |_, r| r.is_err()).unwrap_err();
        assert!(e.contains("missing") && e.contains("no such port"), "{e}");
        assert!(op.status().unwrap().starts_with("opening"));
    }
}
