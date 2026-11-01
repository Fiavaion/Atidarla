//! The model downloader against a local HTTP server on 127.0.0.1: resume,
//! checksum mismatch, refusal without acceptance, Sovereign refusal (of a
//! TEST-NET address, so nothing leaves the machine) and its override.
//!
//! A separate test binary: Sovereign mode is process-wide, and these tests
//! hold a lock while they use it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use lsr_ai::egress;
use lsr_ai::models::{
    Acceptances, DownloadError, DownloadOptions, Model, Progress, Status, Store, download, gate,
    parse_catalogue, start_download, status, verify,
};

static SOVEREIGN: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    SOVEREIGN.lock().unwrap_or_else(|e| e.into_inner())
}

const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// Files `b"hello"` and `b"world!"`.
fn tiny() -> Model {
    let text = format!(
        r#"format = 1
[[model]]
id = "tiny"
name = "Tiny"
adapter = "onnx"
task = "t"
vram_mb = 1
platforms = "any"
main = "w/b.bin"
licence = {{ id = "MIT", class = "commercial-safe", url = "https://example.invalid/licence" }}
source = {{ repo = "o/r", revision = "{REV}" }}
files = [
  {{ path = "a.txt", size = 5, sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" }},
  {{ path = "w/b.bin", size = 6, sha256 = "711e9609339e92b03ddc0a211827dba421f38f9ed8b9d806e1ffdd8c15ffa03d" }},
]
"#
    );
    parse_catalogue(&text).unwrap().get("tiny").unwrap().clone()
}

#[derive(Default)]
struct Behaviour {
    files: HashMap<String, Vec<u8>>,
    /// Paths whose first response stops after this many body bytes.
    cut_once: HashMap<String, usize>,
    /// Paths answered with a redirect to this URL.
    redirect: HashMap<String, String>,
    /// (path, Range header) of every request.
    seen: Vec<(String, Option<String>)>,
}

struct Server {
    base: String,
    state: Arc<Mutex<Behaviour>>,
}

fn path(rel: &str) -> String {
    format!("/o/r/resolve/{REV}/{rel}")
}

impl Server {
    fn start(files: &[(&str, &[u8])]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(Behaviour {
            files: files.iter().map(|(p, b)| (path(p), b.to_vec())).collect(),
            ..Default::default()
        }));
        let s = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || serve(conn, &s));
            }
        });
        Self { base, state }
    }

    fn opts(&self) -> DownloadOptions {
        DownloadOptions {
            allow_internet_once: false,
            base_url: self.base.clone(),
        }
    }

    fn seen(&self) -> Vec<(String, Option<String>)> {
        self.state.lock().unwrap().seen.clone()
    }
}

fn serve(conn: TcpStream, state: &Mutex<Behaviour>) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut conn = conn;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
        let mut range = None;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some(v) = h
                .strip_prefix("range: ")
                .or_else(|| h.strip_prefix("Range: "))
            {
                range = Some(v.to_owned());
            }
        }
        let (body, cut, redirect) = {
            let mut st = state.lock().unwrap();
            st.seen.push((target.clone(), range.clone()));
            let cut = st.cut_once.remove(&target);
            (
                st.files.get(&target).cloned(),
                cut,
                st.redirect.get(&target).cloned(),
            )
        };
        if let Some(to) = redirect {
            let _ = write!(
                conn,
                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n"
            );
            continue;
        }
        let Some(body) = body else {
            let _ = write!(conn, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        };
        let start = range
            .as_deref()
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.strip_suffix('-'))
            .and_then(|n| n.parse::<usize>().ok());
        let (status, part) = match start {
            Some(s) if s < body.len() => ("206 Partial Content", &body[s..]),
            _ => ("200 OK", &body[..]),
        };
        let _ = write!(
            conn,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n",
            part.len()
        );
        match cut {
            Some(n) => {
                let _ = conn.write_all(&part[..n.min(part.len())]);
                let _ = conn.flush();
                let _ = conn.shutdown(std::net::Shutdown::Both);
                return;
            }
            None => {
                let _ = conn.write_all(part);
            }
        }
    }
}

fn store(name: &str) -> Store {
    let dir: PathBuf = std::env::temp_dir().join(format!("lsr-dl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::new(dir.join("models"))
}

fn accept(store: &Store, m: &Model) {
    let mut a = Acceptances::load(store.accepted_path()).unwrap();
    a.accept(m, "2026-10-30");
    a.save(store.accepted_path()).unwrap();
}

fn cleanup(store: &Store) {
    let _ = std::fs::remove_dir_all(store.root().parent().unwrap());
}

const FILES: [(&str, &[u8]); 2] = [("a.txt", b"hello"), ("w/b.bin", b"world!")];

#[test]
fn downloads_refuse_without_an_accepted_licence() {
    let _g = lock();
    let server = Server::start(&FILES);
    let store = store("unaccepted");
    let m = tiny();
    let err = download(&store, &m, &server.opts(), &Progress::default()).unwrap_err();
    assert!(matches!(err, DownloadError::NotAccepted { .. }), "{err}");
    assert!(
        err.to_string().contains("commercial-safe licence of tiny"),
        "{err}"
    );
    assert!(server.seen().is_empty(), "no request before acceptance");
    assert_eq!(status(&store, &m), Status::NotDownloaded);
    cleanup(&store);
}

#[test]
fn an_interrupted_download_resumes_from_its_part_file() {
    let _g = lock();
    let server = Server::start(&FILES);
    server
        .state
        .lock()
        .unwrap()
        .cut_once
        .insert(path("w/b.bin"), 3);
    let store = store("resume");
    let m = tiny();
    accept(&store, &m);
    let err = download(&store, &m, &server.opts(), &Progress::default()).unwrap_err();
    assert!(
        matches!(err, DownloadError::Http { .. } | DownloadError::Size { .. }),
        "{err}"
    );
    assert_eq!(status(&store, &m), Status::Partial);
    assert_eq!(
        std::fs::read(store.part("tiny", "w/b.bin")).unwrap(),
        b"wor"
    );
    assert!(!store.file("tiny", "w/b.bin").exists());

    let job = start_download(store.clone(), m.clone(), server.opts());
    job.wait().unwrap().unwrap();
    assert_eq!(status(&store, &m), Status::Ready);
    assert!(!store.part("tiny", "w/b.bin").exists());
    assert_eq!(
        verify(&store, &m, &Progress::default()).status(),
        Status::Ready
    );
    let seen = server.seen();
    assert_eq!(
        seen.last().unwrap(),
        &(path("w/b.bin"), Some("bytes=3-".to_owned())),
        "{seen:?}"
    );
    // a.txt was complete after the first run, so it was not fetched again.
    assert_eq!(seen.iter().filter(|(p, _)| *p == path("a.txt")).count(), 1);
    cleanup(&store);
}

#[test]
fn a_checksum_mismatch_is_never_renamed_into_place() {
    let _g = lock();
    let server = Server::start(&[("a.txt", b"hello"), ("w/b.bin", b"WORLD!")]);
    let store = store("mismatch");
    let m = tiny();
    accept(&store, &m);
    let err = download(&store, &m, &server.opts(), &Progress::default()).unwrap_err();
    match &err {
        DownloadError::Checksum { file, .. } => assert_eq!(file, "w/b.bin"),
        other => panic!("{other}"),
    }
    assert!(!store.file("tiny", "w/b.bin").exists());
    assert!(!store.part("tiny", "w/b.bin").exists());
    assert_ne!(status(&store, &m), Status::Ready);
    cleanup(&store);
}

#[test]
fn a_cancelled_download_keeps_its_part() {
    let _g = lock();
    let server = Server::start(&FILES);
    let store = store("cancel");
    let m = tiny();
    accept(&store, &m);
    let p = Progress::default();
    p.cancel();
    let err = download(&store, &m, &server.opts(), &p).unwrap_err();
    assert!(matches!(err, DownloadError::Cancelled), "{err}");
    assert!(store.part("tiny", "a.txt").exists());
    assert_eq!(status(&store, &m), Status::Partial);
    cleanup(&store);
}

#[test]
fn sovereign_mode_refuses_internet_hosts_unless_overridden_once() {
    let _g = lock();
    egress::set_sovereign(true);
    let store = store("sovereign");
    let m = tiny();
    accept(&store, &m);
    // A TEST-NET-3 address: refused before any packet is sent.
    let opts = DownloadOptions {
        allow_internet_once: false,
        base_url: "http://203.0.113.9:9".into(),
    };
    let before = egress::refused();
    let err = download(&store, &m, &opts, &Progress::default()).unwrap_err();
    match &err {
        DownloadError::Sovereign { host } => assert_eq!(host, "203.0.113.9"),
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("Download once anyway"), "{err}");
    assert!(egress::refused() > before);

    // A redirect to the internet is refused the same way.
    let server = Server::start(&FILES);
    server
        .state
        .lock()
        .unwrap()
        .redirect
        .insert(path("a.txt"), "http://203.0.113.9:9/elsewhere".into());
    let err = download(&store, &m, &server.opts(), &Progress::default()).unwrap_err();
    assert!(matches!(err, DownloadError::Sovereign { .. }), "{err}");

    // The override lifts the gate for that one download only (checked at
    // the gate, so nothing connects to 203.0.113.9).
    assert!(gate("203.0.113.9", 443, false).is_err());
    let addrs = gate("203.0.113.9", 443, true).unwrap();
    assert_eq!(addrs[0].to_string(), "203.0.113.9:443");
    assert!(gate("203.0.113.9", 443, false).is_err(), "still on");
    assert!(egress::sovereign());

    // Loopback and LAN stay reachable in Sovereign mode, override or not.
    let server = Server::start(&FILES);
    let mut opts = server.opts();
    opts.allow_internet_once = true;
    download(&store, &m, &opts, &Progress::default()).unwrap();
    assert_eq!(status(&store, &m), Status::Ready);
    cleanup(&store);
}
