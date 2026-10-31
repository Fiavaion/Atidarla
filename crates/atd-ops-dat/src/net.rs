//! What the network operators share: URL parts and a worker mailbox.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Locks, recovering from a poisoned mutex (a panic on a worker).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The parts of `scheme://host[:port][/path]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UrlParts {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Starts with `/` (`/` when the URL has none).
    pub path: String,
}

/// Splits a URL; `default_port` gives the port of a scheme without one.
pub(crate) fn split_url(
    url: &str,
    default_port: impl Fn(&str) -> Option<u16>,
) -> Result<UrlParts, String> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("`{url}` has no scheme (`http://`, `ws://`)"))?;
    let scheme = scheme.to_ascii_lowercase();
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("`{url}`: unclosed `[`"))?;
        (h, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err(format!("`{url}` has no host"));
    }
    let port = match port {
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("`{url}`: `{p}` is not a port"))?,
        None => {
            default_port(&scheme).ok_or_else(|| format!("`{url}`: unknown scheme `{scheme}`"))?
        }
    };
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    Ok(UrlParts {
        scheme,
        host: host.to_owned(),
        port,
        path,
    })
}

/// A one-slot mailbox between the cook and a worker thread: the cook
/// offers a job without waiting (a busy worker leaves it for the next
/// frame) and picks up results with `try_lock`.
#[derive(Debug)]
pub(crate) struct Mailbox<J, R> {
    pub state: Mutex<Slots<J, R>>,
    pub wake: Condvar,
}

#[derive(Debug)]
pub(crate) struct Slots<J, R> {
    pub job: Option<J>,
    pub result: Option<R>,
    /// Bumped with every result.
    pub seq: u64,
    pub busy: bool,
    pub stop: bool,
}

impl<J, R> Default for Mailbox<J, R> {
    fn default() -> Self {
        Self {
            state: Mutex::new(Slots {
                job: None,
                result: None,
                seq: 0,
                busy: false,
                stop: false,
            }),
            wake: Condvar::new(),
        }
    }
}

impl<J, R> Mailbox<J, R> {
    /// Hands `job` over unless the worker is busy or the lock is held this
    /// instant; gives it back when it was not taken.
    pub fn offer(&self, job: J) -> Result<(), J> {
        let Ok(mut s) = self.state.try_lock() else {
            return Err(job);
        };
        if s.busy || s.job.is_some() {
            return Err(job);
        }
        s.job = Some(job);
        drop(s);
        self.wake.notify_all();
        Ok(())
    }

    /// The worker's side: waits for a job (or a stop), marks itself busy.
    pub fn next(&self) -> Option<J> {
        let mut s = lock(&self.state);
        loop {
            if s.stop {
                return None;
            }
            if let Some(job) = s.job.take() {
                s.busy = true;
                return Some(job);
            }
            s = self
                .wake
                .wait_timeout(s, Duration::from_millis(100))
                .map_or_else(|e| e.into_inner().0, |r| r.0);
        }
    }

    /// The worker's side: posts a result.
    pub fn finish(&self, result: R) {
        let mut s = lock(&self.state);
        s.result = Some(result);
        s.seq += 1;
        s.busy = false;
    }

    pub fn stop(&self) {
        lock(&self.state).stop = true;
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(s: &str) -> Option<u16> {
        match s {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            _ => None,
        }
    }

    #[test]
    fn urls_split_into_parts() {
        let u = split_url("http://127.0.0.1:9980/a/b?x=1", http).unwrap();
        assert_eq!(
            (u.scheme.as_str(), u.host.as_str(), u.port, u.path.as_str()),
            ("http", "127.0.0.1", 9980, "/a/b?x=1")
        );
        let u = split_url("WS://[::1]/live", http).unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.scheme.as_str()),
            ("::1", 80, "ws")
        );
        let u = split_url("https://user@example.com?q", http).unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("example.com", 443, "/?q")
        );
        assert!(split_url("example.com", http).is_err());
        assert!(split_url("ftp://x", http).is_err());
        assert!(split_url("http://:80", http).is_err());
        assert!(split_url("http://x:port", http).is_err());
    }
}
