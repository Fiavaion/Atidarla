//! Heartbeat for the `atidarla run` supervisor (FR-H2, `docs/run.md`).
//!
//! The UI thread only bumps an atomic counter once per frame; a separate
//! thread writes `<beats> <frames>` to the file once a second, and only if
//! the counter moved since the last write. A frozen or deadlocked UI thread
//! therefore stops the heartbeat, while the render thread never touches the
//! disk (CLAUDE.md rule 1).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How often the writer thread looks at the frame counter.
const PERIOD: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Heartbeat {
    frames: Arc<AtomicU64>,
}

impl Heartbeat {
    /// Starts the writer thread for `path` (its folder is created).
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let frames = Arc::new(AtomicU64::new(0));
        let counter = frames.clone();
        std::thread::Builder::new()
            .name("heartbeat".into())
            .spawn(move || {
                let (mut seen, mut beats) = (0, 0u64);
                loop {
                    let now = counter.load(Ordering::Relaxed);
                    if now != seen {
                        seen = now;
                        beats += 1;
                        // Beside it and renamed, so the supervisor never
                        // reads half a line.
                        let tmp = path.with_extension("tmp");
                        let line = format!("{beats} {now}\n");
                        if std::fs::write(&tmp, line).is_ok() {
                            let _ = std::fs::rename(&tmp, &path);
                        }
                    }
                    std::thread::sleep(PERIOD);
                }
            })?;
        Ok(Self { frames })
    }

    /// Called once per UI frame.
    pub fn tick(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    /// When the UI should wake up even if nothing is animating, so an idle
    /// show still beats.
    pub const IDLE_WAKE: Duration = Duration::from_millis(500);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_while_frames_advance() {
        let dir = std::env::temp_dir().join(format!("atd-heartbeat-{}", std::process::id()));
        let path = dir.join("hb");
        let hb = Heartbeat::start(path.clone()).unwrap();
        assert!(!path.exists(), "no frame yet: no beat");
        hb.tick();
        let wait = |pred: &dyn Fn(&str) -> bool| {
            for _ in 0..40 {
                if std::fs::read_to_string(&path).is_ok_and(|s| pred(&s)) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        };
        assert!(wait(&|s| s.starts_with("1 ")));
        hb.tick();
        assert!(wait(&|s| s.starts_with("2 ")));
        // Frozen UI: the content stays.
        let before = std::fs::read_to_string(&path).unwrap();
        std::thread::sleep(Duration::from_millis(2_500));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
