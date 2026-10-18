//! A file watched on its own thread: each new version is read and built
//! (compiled, parsed) there and handed over a channel, so neither the read
//! nor the build touches the render thread (rule 1). Used by Compute and
//! Neural CA; WGSL keeps its own copy of the same loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// How often the file is read for changes.
const POLL: Duration = Duration::from_millis(250);

/// Watches one file. Dropping it stops the thread.
pub(crate) struct Watcher<T> {
    /// The parameter text the file came from, for change detection.
    pub(crate) path: String,
    stop: Arc<AtomicBool>,
    pub(crate) rx: Receiver<Result<T, String>>,
}

impl<T> Drop for Watcher<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Watches `file` (from the parameter text `path`), building every version
/// that differs from the last with `build`. A read error is sent as an
/// error naming `path`.
pub(crate) fn watch<T: Send + 'static>(
    path: String,
    file: std::path::PathBuf,
    name: &str,
    build: impl Fn(&str) -> Result<T, String> + Send + 'static,
) -> Watcher<T> {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_path = path.clone();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let mut last: Option<Result<String, String>> = None;
            while !thread_stop.load(Ordering::Relaxed) {
                let now = std::fs::read_to_string(&file).map_err(|e| format!("{thread_path}: {e}"));
                if last.as_ref() != Some(&now) {
                    let built = match &now {
                        Ok(text) => build(text),
                        Err(e) => Err(e.clone()),
                    };
                    if tx.send(built).is_err() {
                        return;
                    }
                    last = Some(now);
                }
                std::thread::sleep(POLL);
            }
        });
    if let Err(e) = spawned {
        let (tx2, rx2) = channel();
        let _ = tx2.send(Err(format!("cannot start file watcher: {e}")));
        return Watcher {
            path,
            stop,
            rx: rx2,
        };
    }
    Watcher { path, stop, rx }
}
