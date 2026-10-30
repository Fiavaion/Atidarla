//! `atidarla run`: a supervisor that keeps the app running (FR-H2).
//!
//! It launches `atidarla-app` on a project, restarts it when it exits
//! abnormally or stops heartbeating, backs off between restarts, logs each
//! one to `<project>/.cache/restarts.log` and stops the child on Ctrl-C.
//! The heartbeat protocol is in `docs/run.md`. The supervisor never touches
//! the app's render thread: it only reads a file the app writes from its own
//! thread.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

/// How the supervisor restarts and when it gives up.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Consecutive restarts allowed before giving up (0: never restart).
    pub max_restarts: u32,
    /// Delay before the first restart; doubles with each consecutive one.
    pub restart_delay: Duration,
    /// The longest delay between restarts.
    pub max_delay: Duration,
    /// Kill and restart an app whose heartbeat has not changed for this long
    /// (`None`: do not watch the heartbeat).
    pub heartbeat_timeout: Option<Duration>,
    /// Time allowed from launch to the first heartbeat (project load, shader
    /// compiles); at least `heartbeat_timeout`.
    pub startup_grace: Duration,
    /// A run at least this long counts as healthy: the restart count and the
    /// back-off start over.
    pub stable_after: Duration,
    /// How often the child and the heartbeat are checked.
    pub poll: Duration,
    /// On stop, how long the child gets to exit before it is killed.
    pub stop_grace: Duration,
}

impl Policy {
    pub fn new(
        max_restarts: u32,
        restart_delay: Duration,
        heartbeat_timeout: Option<Duration>,
        startup_grace: Duration,
    ) -> Self {
        Self {
            max_restarts,
            restart_delay,
            max_delay: Duration::from_secs(60),
            heartbeat_timeout,
            startup_grace,
            stable_after: Duration::from_secs(60),
            poll: Duration::from_millis(200),
            stop_grace: Duration::from_secs(3),
        }
    }
}

/// Why the supervisor returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The app exited with status 0 (the operator quit it): not restarted.
    Clean { launches: u32 },
    /// Stopped by Ctrl-C / SIGTERM; the child was stopped.
    Stopped { launches: u32 },
    /// More failures in a row than `max_restarts`.
    GaveUp { launches: u32, last: String },
}

/// How one run of the child ended.
enum Ended {
    Exited(ExitStatus),
    Hung,
    Stopped,
}

/// Runs `launch(n)` (the n-th launch, from 0) and keeps it running per
/// `policy`, reading `heartbeat` and reporting events to `log`.
pub fn supervise(
    launch: &mut dyn FnMut(u32) -> Command,
    heartbeat: &Path,
    log: &mut dyn FnMut(&str),
    policy: &Policy,
    stop: &AtomicBool,
) -> Result<Outcome> {
    let mut launches = 0u32;
    let mut consecutive = 0u32;
    loop {
        // A stale file from the last run must not look like a heartbeat.
        let _ = std::fs::remove_file(heartbeat);
        let mut cmd = launch(launches);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("launching {:?}", cmd.get_program()))?;
        log(&format!("launch {} (pid {})", launches + 1, child.id()));
        launches += 1;
        let started = Instant::now();
        let ended = watch(&mut child, heartbeat, policy, stop)?;
        let reason = match ended {
            Ended::Stopped => {
                log("stop requested: child stopped");
                return Ok(Outcome::Stopped { launches });
            }
            Ended::Exited(status) if status.success() => {
                log("app exited cleanly: not restarting");
                return Ok(Outcome::Clean { launches });
            }
            Ended::Exited(status) => format!("app exited abnormally ({status})"),
            Ended::Hung => {
                let t = policy.heartbeat_timeout.unwrap_or_default();
                format!("no heartbeat for {:.1} s: app killed", t.as_secs_f64())
            }
        };
        if started.elapsed() >= policy.stable_after {
            consecutive = 0;
        }
        if consecutive >= policy.max_restarts {
            log(&format!(
                "{reason}; {} restarts in a row: giving up",
                policy.max_restarts
            ));
            return Ok(Outcome::GaveUp {
                launches,
                last: reason,
            });
        }
        let delay = policy
            .restart_delay
            .saturating_mul(1u32.checked_shl(consecutive).unwrap_or(u32::MAX))
            .min(policy.max_delay);
        consecutive += 1;
        log(&format!(
            "{reason}; restart {consecutive} of {} in {:.1} s",
            policy.max_restarts,
            delay.as_secs_f64()
        ));
        let until = Instant::now() + delay;
        while Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                log("stop requested while waiting to restart");
                return Ok(Outcome::Stopped { launches });
            }
            std::thread::sleep(policy.poll.min(until - Instant::now()));
        }
    }
}

/// Waits for the child to end, stopping it if the heartbeat stalls or `stop`
/// is set.
fn watch(child: &mut Child, heartbeat: &Path, policy: &Policy, stop: &AtomicBool) -> Result<Ended> {
    let mut last = Vec::new();
    let mut last_change = Instant::now();
    let mut seen = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            stop_child(child, policy.stop_grace);
            return Ok(Ended::Stopped);
        }
        if let Some(status) = child.try_wait().context("waiting for the app")? {
            return Ok(if stop.load(Ordering::SeqCst) {
                Ended::Stopped
            } else {
                Ended::Exited(status)
            });
        }
        if let Some(timeout) = policy.heartbeat_timeout {
            if let Ok(now) = std::fs::read(heartbeat)
                && now != last
            {
                last = now;
                last_change = Instant::now();
                seen = true;
            }
            let limit = if seen {
                timeout
            } else {
                policy.startup_grace.max(timeout)
            };
            if last_change.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Ended::Hung);
            }
        }
        std::thread::sleep(policy.poll);
    }
}

/// Gives the child `grace` to exit by itself (a terminal's Ctrl-C reaches it
/// too), then kills it.
fn stop_child(child: &mut Child, grace: Duration) {
    let until = Instant::now() + grace;
    while Instant::now() < until {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `UTC` time as `YYYY-MM-DD HH:MM:SS`.
pub fn timestamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Where the app binary is: `$ATD_APP`, else beside this executable.
pub fn find_app() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("ATD_APP") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        bail!("ATD_APP={} is not a file", p.display());
    }
    let exe = std::env::current_exe().context("finding this executable")?;
    let name = format!("atidarla-app{}", std::env::consts::EXE_SUFFIX);
    let beside = exe.with_file_name(&name);
    if beside.is_file() {
        return Ok(beside);
    }
    bail!(
        "the app binary {} is not beside {}; build it (`cargo build -p atd-app`) or set ATD_APP",
        name,
        exe.display()
    )
}

/// Options of `atidarla run`.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub kiosk: bool,
    pub max_restarts: u32,
    pub restart_delay: f64,
    /// Seconds; 0 turns the heartbeat watch off.
    pub heartbeat_timeout: f64,
    pub startup_grace: f64,
}

/// `atidarla run`.
pub fn run(project: &Path, opts: &RunOptions) -> Result<()> {
    if !project.is_dir() {
        bail!("{} is not a project folder", project.display());
    }
    let app = find_app()?;
    let cache = project.join(".cache");
    std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
    let heartbeat = cache.join("heartbeat");
    let log_path = cache.join("restarts.log");
    let policy = Policy::new(
        opts.max_restarts,
        Duration::try_from_secs_f64(opts.restart_delay).context("--restart-delay")?,
        (opts.heartbeat_timeout > 0.0)
            .then(|| Duration::try_from_secs_f64(opts.heartbeat_timeout))
            .transpose()
            .context("--heartbeat-timeout")?,
        Duration::try_from_secs_f64(opts.startup_grace).context("--startup-grace")?,
    );
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
        .context("installing the Ctrl-C handler")?;

    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let mut log = |msg: &str| {
        let line = format!("{} {msg}", timestamp(SystemTime::now()));
        eprintln!("atidarla run: {msg}");
        let _ = writeln!(log_file, "{line}");
    };
    let mut launch = |_n: u32| {
        let mut cmd = Command::new(&app);
        cmd.arg(project).arg("--heartbeat").arg(&heartbeat);
        if opts.kiosk {
            cmd.arg("--kiosk");
        }
        cmd
    };
    match supervise(&mut launch, &heartbeat, &mut log, &policy, stop)? {
        Outcome::Clean { .. } | Outcome::Stopped { .. } => Ok(()),
        Outcome::GaveUp { launches, last } => {
            bail!("gave up after {launches} launches; last: {last}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// The fake app is this test binary running [`fake_child`]; its
    /// behaviour comes from `ATD_FAKE` (a mode) and `ATD_FAKE_N` (the launch
    /// number) so no script or second binary is needed on either OS.
    fn fake(mode: &str, heartbeat: &Path, n: u32) -> Command {
        let mut cmd = Command::new(std::env::current_exe().expect("test exe"));
        cmd.args(["--exact", "supervise::tests::fake_child", "--nocapture"])
            .env("ATD_FAKE", mode)
            .env("ATD_FAKE_N", n.to_string())
            .env("ATD_FAKE_HEARTBEAT", heartbeat)
            .stdout(std::process::Stdio::null());
        cmd
    }

    #[test]
    fn fake_child() {
        let Ok(mode) = std::env::var("ATD_FAKE") else {
            return; // a normal test run: nothing to do
        };
        let n: u32 = std::env::var("ATD_FAKE_N").unwrap().parse().unwrap();
        let hb = PathBuf::from(std::env::var("ATD_FAKE_HEARTBEAT").unwrap());
        let beat = |i: u32| std::fs::write(&hb, format!("{i}\n")).unwrap();
        match (mode.as_str(), n) {
            // Crashes on the first two launches, then runs a while and quits.
            ("crash2", 0 | 1) => std::process::exit(3),
            ("crash2", _) => {
                for i in 0..5 {
                    beat(i);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            ("crash", _) => std::process::exit(1),
            // Beats twice, then hangs without exiting.
            ("hang", _) => {
                beat(1);
                std::thread::sleep(Duration::from_millis(100));
                beat(2);
                std::thread::sleep(Duration::from_secs(60));
            }
            // Never beats (stuck in start-up).
            ("silent", _) => std::thread::sleep(Duration::from_secs(60)),
            // Healthy until killed.
            ("run", _) => {
                for i in 0.. {
                    beat(i);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            _ => std::process::exit(9),
        }
    }

    fn policy(max: u32, delay_ms: u64) -> Policy {
        let mut p = Policy::new(
            max,
            Duration::from_millis(delay_ms),
            Some(Duration::from_millis(500)),
            Duration::from_secs(3),
        );
        p.poll = Duration::from_millis(20);
        p.stop_grace = Duration::from_millis(100);
        p.stable_after = Duration::from_secs(3_600);
        p
    }

    struct Rig {
        dir: PathBuf,
        log: Vec<(Instant, String)>,
    }
    impl Rig {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("atd-supervise-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                log: Vec::new(),
            }
        }
        fn go(&mut self, mode: &str, policy: &Policy, stop: &AtomicBool) -> Outcome {
            let hb = self.dir.join("heartbeat");
            let mode = mode.to_owned();
            let hb2 = hb.clone();
            let log = &mut self.log;
            supervise(
                &mut |n| fake(&mode, &hb2, n),
                &hb,
                &mut |m| log.push((Instant::now(), m.to_owned())),
                policy,
                stop,
            )
            .unwrap()
        }
        fn count(&self, needle: &str) -> usize {
            self.log.iter().filter(|(_, m)| m.contains(needle)).count()
        }
        /// Times of the "launch" events.
        fn launch_times(&self) -> Vec<Instant> {
            self.log
                .iter()
                .filter(|(_, m)| m.starts_with("launch"))
                .map(|(t, _)| *t)
                .collect()
        }
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn crash_twice_then_clean_run_restarts_twice_with_backoff() {
        let mut rig = Rig::new("crash2");
        let out = rig.go("crash2", &policy(5, 200), &AtomicBool::new(false));
        assert_eq!(out, Outcome::Clean { launches: 3 });
        assert_eq!(rig.count("abnormally"), 2);
        let t = rig.launch_times();
        // First restart waits >= 200 ms, the second >= 400 ms (doubling).
        assert!(
            t[1] - t[0] >= Duration::from_millis(200),
            "{:?}",
            t[1] - t[0]
        );
        assert!(
            t[2] - t[1] >= Duration::from_millis(400),
            "{:?}",
            t[2] - t[1]
        );
    }

    #[test]
    fn stops_after_max_restarts() {
        let mut rig = Rig::new("crash");
        let out = rig.go("crash", &policy(3, 10), &AtomicBool::new(false));
        assert!(
            matches!(out, Outcome::GaveUp { launches: 4, .. }),
            "{out:?}"
        );
        assert_eq!(rig.count("launch"), 4);
        assert_eq!(rig.count("giving up"), 1);
    }

    #[test]
    fn zero_max_restarts_never_restarts() {
        let mut rig = Rig::new("zero");
        let out = rig.go("crash", &policy(0, 10), &AtomicBool::new(false));
        assert!(
            matches!(out, Outcome::GaveUp { launches: 1, .. }),
            "{out:?}"
        );
    }

    #[test]
    fn stalled_heartbeat_kills_and_restarts() {
        let mut rig = Rig::new("hang");
        let out = rig.go("hang", &policy(2, 10), &AtomicBool::new(false));
        assert!(
            matches!(out, Outcome::GaveUp { launches: 3, .. }),
            "{out:?}"
        );
        assert_eq!(rig.count("no heartbeat"), 3);
    }

    #[test]
    fn missing_first_heartbeat_waits_for_startup_grace() {
        let mut rig = Rig::new("silent");
        let mut p = policy(0, 10);
        p.startup_grace = Duration::from_millis(1_200);
        let t0 = Instant::now();
        let out = rig.go("silent", &p, &AtomicBool::new(false));
        assert!(
            matches!(out, Outcome::GaveUp { launches: 1, .. }),
            "{out:?}"
        );
        assert!(t0.elapsed() >= Duration::from_millis(1_200));
    }

    #[test]
    fn stop_flag_stops_the_child() {
        let mut rig = Rig::new("stop");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            s.store(true, Ordering::SeqCst);
        });
        let t0 = Instant::now();
        let out = rig.go("run", &policy(5, 10), &stop);
        assert_eq!(out, Outcome::Stopped { launches: 1 });
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn timestamp_formats_utc() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_761_826_245)),
            "2025-10-30 12:10:45 UTC"
        );
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
    }
}
