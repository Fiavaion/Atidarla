//! Downloading a model's files (FR-A12).
//!
//! - Only after the licence was accepted (every class, commercial-safe
//!   included).
//! - Every connection, redirects included, resolves through
//!   [`crate::egress`]: in Sovereign mode internet hosts are refused before
//!   any packet is sent, unless the caller passes a one-off override for
//!   this download ([`DownloadOptions::allow_internet_once`]).
//! - Each file goes to `<file>.part`, resumes from there with an HTTP
//!   range request, and is renamed into place only after its size and
//!   SHA-256 match the catalogue. A mismatch deletes the `.part`.
//! - HTTPS is rustls (ring) with the operating system's trust store.

use std::io::{Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use ureq::Agent;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use super::accept::Acceptances;
use super::catalogue::{Model, ModelFile};
use super::paths::Store;
use super::status::{Job, Progress, hash_reader, hex};
use crate::egress::{self, EgressError};

/// Where catalogue files are downloaded from.
pub const HUGGING_FACE: &str = "https://huggingface.co";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOptions {
    /// The user chose "Download once anyway" while Sovereign mode is on:
    /// internet hosts are allowed for this download only.
    pub allow_internet_once: bool,
    /// Base URL ([`HUGGING_FACE`]; a local server in tests).
    pub base_url: String,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            allow_internet_once: false,
            base_url: HUGGING_FACE.to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(
        "the {class} licence of {id} ({licence}) has not been accepted on this machine: read it, then accept it before downloading"
    )]
    NotAccepted {
        id: String,
        class: atd_format::LicenceClass,
        licence: String,
    },
    #[error(
        "Sovereign mode blocks {host}: it is on the internet. Download once anyway, or turn Sovereign mode off for this project"
    )]
    Sovereign { host: String },
    #[error("{file}: {message}")]
    Http { file: String, message: String },
    #[error("{file}: expected {expected} bytes, got {got}")]
    Size {
        file: String,
        expected: u64,
        got: u64,
    },
    #[error("{file}: SHA-256 is {got}, the catalogue says {expected}; the download was discarded")]
    Checksum {
        file: String,
        expected: String,
        got: String,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read the licence acceptances: {0}")]
    Acceptances(String),
    #[error("cancelled (partial files are kept and resume next time)")]
    Cancelled,
}

/// The egress decision for one host: the addresses to connect to.
/// `allow_internet_once` lifts Sovereign mode for this call only; the gate
/// is still asked first, so a refusal is counted and logged.
pub fn gate(
    host: &str,
    port: u16,
    allow_internet_once: bool,
) -> Result<Vec<SocketAddr>, EgressError> {
    match egress::resolve(host, port) {
        Err(EgressError::Sovereign { .. }) if allow_internet_once => {
            log::warn!(
                "Sovereign mode overridden once by the user for a model download from {host}"
            );
            use std::net::ToSocketAddrs;
            let addrs: Vec<SocketAddr> = (host, port)
                .to_socket_addrs()
                .map_err(|source| EgressError::Resolve {
                    host: host.to_owned(),
                    source,
                })?
                .filter(|a| !a.ip().is_unspecified())
                .collect();
            if addrs.is_empty() {
                Err(EgressError::NoAddress {
                    host: host.to_owned(),
                })
            } else {
                Ok(addrs)
            }
        }
        other => other,
    }
}

/// ureq's resolver, replaced so every connection (redirects included)
/// goes through [`gate`].
#[derive(Debug)]
struct GateResolver {
    allow_internet_once: bool,
    /// The host Sovereign mode refused, to report it as such.
    refused: Arc<Mutex<Option<String>>>,
}

impl Resolver for GateResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri
            .host()
            .ok_or(ureq::Error::HostNotFound)?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
            Some("http") => 80,
            _ => 443,
        });
        match gate(host, port, self.allow_internet_once) {
            Ok(addrs) => {
                let mut out = self.empty();
                for a in addrs.into_iter().take(16) {
                    out.push(a);
                }
                Ok(out)
            }
            Err(e) => {
                if let EgressError::Sovereign { host } = &e
                    && let Ok(mut r) = self.refused.lock()
                {
                    *r = Some(host.clone());
                }
                Err(ureq::Error::Io(std::io::Error::other(e.to_string())))
            }
        }
    }
}

/// An HTTPS agent whose every connection resolves through [`gate`]; a host
/// Sovereign mode refused is recorded in `refused`.
pub(crate) fn agent(opts: &DownloadOptions, refused: Arc<Mutex<Option<String>>>) -> Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    let config = Agent::config_builder()
        .tls_config(tls)
        // Proxies from the environment would bypass the egress gate.
        .proxy(None)
        .http_status_as_error(false)
        .timeout_resolve(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .user_agent(concat!("atidarla/", env!("CARGO_PKG_VERSION")))
        .build();
    Agent::with_parts(
        config,
        DefaultConnector::default(),
        GateResolver {
            allow_internet_once: opts.allow_internet_once,
            refused,
        },
    )
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> DownloadError + '_ {
    move |source| DownloadError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Downloads every file of `model` that is not already there with the
/// right size. Blocking: run it with [`start_download`] from the UI.
pub fn download(
    store: &Store,
    model: &Model,
    opts: &DownloadOptions,
    progress: &Progress,
) -> Result<(), DownloadError> {
    let accepted = Acceptances::load(store.accepted_path()).map_err(DownloadError::Acceptances)?;
    if !accepted.is_accepted(model) {
        return Err(DownloadError::NotAccepted {
            id: model.id.clone(),
            class: model.licence.class,
            licence: model.licence.id.clone(),
        });
    }
    progress.set_total(model.total_size());
    let refused = Arc::new(Mutex::new(None));
    let agent = agent(opts, refused.clone());
    let mut buf = vec![0u8; 1 << 18];
    for file in &model.files {
        progress.set_file(&file.path);
        let dest = store.file(&model.id, &file.path);
        if std::fs::metadata(&dest).is_ok_and(|m| m.len() == file.size) {
            progress.add(file.size);
            continue;
        }
        let result = fetch(store, model, file, opts, &agent, progress, &mut buf);
        if let Err(DownloadError::Http { .. }) = &result
            && let Some(host) = refused.lock().ok().and_then(|mut r| r.take())
        {
            return Err(DownloadError::Sovereign { host });
        }
        result?;
    }
    Ok(())
}

fn fetch(
    store: &Store,
    model: &Model,
    file: &ModelFile,
    opts: &DownloadOptions,
    agent: &Agent,
    progress: &Progress,
    buf: &mut [u8],
) -> Result<(), DownloadError> {
    let dest = store.file(&model.id, &file.path);
    let part = store.part(&model.id, &file.path);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&part)
        .map_err(io(&part))?;
    let mut have = out.metadata().map_err(io(&part))?.len();
    if have > file.size {
        out.set_len(0).map_err(io(&part))?;
        have = 0;
    }
    // Hash what is already there, so the whole file is checked.
    let mut hasher = Sha256::new();
    if have > 0 {
        out.seek(SeekFrom::Start(0)).map_err(io(&part))?;
        let quiet = Progress::default();
        hash_reader(
            std::io::Read::take(&mut out, have),
            &mut hasher,
            &quiet,
            buf,
        )
        .map_err(io(&part))?;
        progress.add(have);
    }
    let http = |message: String| DownloadError::Http {
        file: file.path.clone(),
        message,
    };
    let url = model.file_url(&opts.base_url, file);
    if have < file.size {
        let mut req = agent.get(&url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let resp = req.call().map_err(|e| http(e.to_string()))?;
        match resp.status().as_u16() {
            206 if have > 0 => {}
            200 => {
                // No range support: start again.
                progress.sub(have);
                out.set_len(0).map_err(io(&part))?;
                hasher = Sha256::new();
                have = 0;
            }
            s => return Err(http(format!("HTTP {s} from {url}"))),
        }
        out.seek(SeekFrom::Start(have)).map_err(io(&part))?;
        let mut body = resp.into_body().into_reader();
        loop {
            if progress.cancelled() {
                out.flush().map_err(io(&part))?;
                return Err(DownloadError::Cancelled);
            }
            let n = std::io::Read::read(&mut body, buf).map_err(|e| http(e.to_string()))?;
            if n == 0 {
                break;
            }
            if have + n as u64 > file.size {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err(DownloadError::Size {
                    file: file.path.clone(),
                    expected: file.size,
                    got: have + n as u64,
                });
            }
            out.write_all(&buf[..n]).map_err(io(&part))?;
            hasher.update(&buf[..n]);
            have += n as u64;
            progress.add(n as u64);
        }
    }
    out.sync_all().map_err(io(&part))?;
    drop(out);
    if have != file.size {
        // A short read: keep the part to resume.
        return Err(DownloadError::Size {
            file: file.path.clone(),
            expected: file.size,
            got: have,
        });
    }
    let got = hex(&hasher.finalize());
    if got != file.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(DownloadError::Checksum {
            file: file.path.clone(),
            expected: file.sha256.clone(),
            got,
        });
    }
    std::fs::rename(&part, &dest).map_err(io(&dest))
}

/// [`download`] on its own thread.
pub fn start_download(
    store: Store,
    model: Model,
    opts: DownloadOptions,
) -> Job<Result<(), DownloadError>> {
    Job::spawn("atd-model-download", move |p| {
        download(&store, &model, &opts, p)
    })
}
