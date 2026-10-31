//! `atidarla soak` (PT-10): run a project headless for hours and check it
//! neither leaks memory nor stalls. The project cooks at 60 frames a second
//! as under `atidarla serve` (every node, every frame); each frame's cook
//! time and its time to GPU completion are measured, the process's resident
//! memory is sampled on a separate thread, and after a warm-up period the
//! run is judged against PT-10: resident memory grows by no more than
//! 100 MB and no frame takes longer than 50 ms. `--cycle-seconds` launches
//! the Stage deck's columns in turn, as a performer would.
//!
//! Writes `<OUT>/soak.csv` (one row per sample window, written as the run
//! goes, so a crash keeps what was measured) and `<OUT>/soak.md` (the
//! report). Exits non-zero when PT-10 fails. See docs/soak.md.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::serve::ServeWorkspace;

/// PT-10: resident memory may grow by at most this much after warm-up.
pub const LEAK_LIMIT_MB: f64 = 100.0;
/// PT-10: no frame after warm-up may take longer than this.
pub const SPIKE_LIMIT_MS: f64 = 50.0;
/// The frame rate the show is paced at (as `serve`).
const FPS: f64 = 60.0;

#[derive(Debug, clap::Args)]
pub struct SoakArgs {
    /// Project folder (`*.atidarla`).
    pub project: PathBuf,
    /// Hours to run (added to --minutes and --seconds; default 24 hours
    /// when none is given).
    #[arg(long)]
    pub hours: Option<f64>,
    #[arg(long)]
    pub minutes: Option<f64>,
    #[arg(long)]
    pub seconds: Option<f64>,
    /// Seconds at the start that are not judged (shader compiles, pools
    /// filling, caches warming).
    #[arg(long, default_value_t = 120.0)]
    pub warmup: f64,
    /// Folder for soak.md and soak.csv (created if missing).
    #[arg(long, default_value = "soak")]
    pub out: PathBuf,
    /// Seconds per sample window (one CSV row, one memory reading).
    #[arg(long, default_value_t = 10.0)]
    pub sample: f64,
    /// Launch the next column of the Stage deck every this many seconds.
    #[arg(long)]
    pub cycle_seconds: Option<f64>,
}

/// What a soak run does, independent of the command line.
#[derive(Debug, Clone)]
pub struct SoakOptions {
    pub duration: Duration,
    pub warmup: Duration,
    pub sample: Duration,
    pub cycle: Option<Duration>,
}

impl SoakArgs {
    fn options(&self) -> Result<SoakOptions> {
        let given = [
            self.hours.map(|h| h * 3600.0),
            self.minutes.map(|m| m * 60.0),
            self.seconds,
        ];
        let seconds = if given.iter().all(Option::is_none) {
            24.0 * 3600.0
        } else {
            given.iter().flatten().sum()
        };
        let secs = |name: &str, s: f64| {
            if s.is_finite() && s >= 0.0 {
                Ok(Duration::from_secs_f64(s))
            } else {
                bail!("--{name} must be a number of seconds >= 0")
            }
        };
        let options = SoakOptions {
            duration: secs("hours/--minutes/--seconds", seconds)?,
            warmup: secs("warmup", self.warmup)?,
            sample: secs("sample", self.sample)?,
            cycle: self
                .cycle_seconds
                .map(|c| secs("cycle-seconds", c))
                .transpose()?,
        };
        if options.sample.is_zero() || options.cycle.is_some_and(|c| c.is_zero()) {
            bail!("--sample and --cycle-seconds must be above 0");
        }
        if options.warmup >= options.duration {
            bail!("the run must be longer than --warmup, or nothing is measured");
        }
        Ok(options)
    }
}

/// Frame times of one sample window.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    frames: u32,
    sum_ms: f64,
    max_frame_ms: f64,
    max_cook_ms: f64,
    max_interval_ms: f64,
    spikes: u32,
    launches: u32,
}

impl Window {
    fn add(&mut self, frame_ms: f64, cook_ms: f64, interval_ms: Option<f64>) {
        self.frames += 1;
        self.sum_ms += frame_ms;
        self.max_frame_ms = self.max_frame_ms.max(frame_ms);
        self.max_cook_ms = self.max_cook_ms.max(cook_ms);
        if let Some(i) = interval_ms {
            self.max_interval_ms = self.max_interval_ms.max(i);
        }
        if frame_ms > SPIKE_LIMIT_MS {
            self.spikes += 1;
        }
    }
}

/// Frame times in 0.1 ms buckets up to 200 ms (the last bucket holds the
/// rest): percentiles over a day of frames in constant memory.
struct Histogram {
    buckets: Box<[u64; Histogram::LEN]>,
    count: u64,
}

impl Histogram {
    const LEN: usize = 2001;

    fn new() -> Self {
        Self {
            buckets: Box::new([0; Self::LEN]),
            count: 0,
        }
    }

    fn add(&mut self, ms: f64) {
        let i = ((ms * 10.0) as usize).min(Self::LEN - 1);
        self.buckets[i] += 1;
        self.count += 1;
    }

    /// The upper edge of the bucket the `p`th percentile falls in.
    fn percentile(&self, p: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let rank = ((p / 100.0) * self.count as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, n) in self.buckets.iter().enumerate() {
            seen += n;
            if seen >= rank {
                return (i + 1) as f64 / 10.0;
            }
        }
        Self::LEN as f64 / 10.0
    }
}

/// The result of a soak run.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub project: String,
    pub adapter: String,
    pub platform: String,
    pub started: String,
    pub ran_s: f64,
    pub warmup_s: f64,
    pub frames: u64,
    pub frames_measured: u64,
    /// Resident memory at the first sample.
    pub rss_start_mb: f64,
    /// Resident memory at the first sample after warm-up: what growth is
    /// measured from.
    pub rss_baseline_mb: f64,
    pub rss_end_mb: f64,
    /// The highest sample after warm-up.
    pub rss_peak_mb: f64,
    pub mean_frame_ms: f64,
    pub p99_frame_ms: f64,
    pub worst_frame_ms: f64,
    /// Seconds into the run of the worst frame.
    pub worst_frame_at_s: f64,
    pub worst_cook_ms: f64,
    pub worst_interval_ms: f64,
    /// Frames after warm-up over [`SPIKE_LIMIT_MS`].
    pub spikes: u64,
    pub launches: u64,
    /// Nodes with an error when the run ended.
    pub errors: Vec<String>,
}

impl Summary {
    pub fn growth_mb(&self) -> f64 {
        self.rss_end_mb - self.rss_baseline_mb
    }

    pub fn measured(&self) -> bool {
        self.frames_measured > 0 && self.rss_baseline_mb > 0.0
    }

    pub fn leak_ok(&self) -> bool {
        self.measured() && self.growth_mb() <= LEAK_LIMIT_MB
    }

    pub fn spikes_ok(&self) -> bool {
        self.measured() && self.worst_frame_ms <= SPIKE_LIMIT_MS
    }

    pub fn passed(&self) -> bool {
        self.leak_ok() && self.spikes_ok()
    }

    /// The Markdown report.
    pub fn markdown(&self) -> String {
        let verdict = |ok: bool| if ok { "pass" } else { "FAIL" };
        let mut s = String::new();
        let _ = writeln!(s, "# Soak: {}\n", self.project);
        let _ = writeln!(
            s,
            "**PT-10: {}** (no leak > {LEAK_LIMIT_MB} MB, no frame > {SPIKE_LIMIT_MS} ms after warm-up)\n",
            verdict(self.passed())
        );
        let _ = writeln!(s, "| | |\n|---|---|");
        let _ = writeln!(s, "| Started | {} |", self.started);
        let _ = writeln!(
            s,
            "| Ran | {} (warm-up {}) |",
            hms(self.ran_s),
            hms(self.warmup_s)
        );
        let _ = writeln!(s, "| Platform | {} |", self.platform);
        let _ = writeln!(s, "| GPU | {} |", self.adapter);
        let _ = writeln!(
            s,
            "| Frames | {} ({} after warm-up) |",
            self.frames, self.frames_measured
        );
        let _ = writeln!(s, "| Stage launches | {} |", self.launches);
        let _ = writeln!(s, "\n## Memory (resident set)\n");
        let _ = writeln!(s, "| | MB |\n|---|---:|");
        let _ = writeln!(s, "| At start | {:.1} |", self.rss_start_mb);
        let _ = writeln!(
            s,
            "| After warm-up (baseline) | {:.1} |",
            self.rss_baseline_mb
        );
        let _ = writeln!(s, "| Peak after warm-up | {:.1} |", self.rss_peak_mb);
        let _ = writeln!(s, "| At end | {:.1} |", self.rss_end_mb);
        let _ = writeln!(
            s,
            "| **Growth after warm-up** | **{:+.1}** ({}) |",
            self.growth_mb(),
            verdict(self.leak_ok())
        );
        let _ = writeln!(s, "\n## Frame time after warm-up\n");
        let _ = writeln!(
            s,
            "Frame time is one frame's cook plus waiting for the GPU to finish it \
             (headless, there is no present). The interval is the wall time between \
             frame starts, which also includes the 60 fps pacing and the OS scheduler.\n"
        );
        let _ = writeln!(s, "| | ms |\n|---|---:|");
        let _ = writeln!(s, "| Mean | {:.2} |", self.mean_frame_ms);
        let _ = writeln!(s, "| 99th percentile | {:.1} |", self.p99_frame_ms);
        let _ = writeln!(
            s,
            "| **Worst** | **{:.2}** at {} ({}) |",
            self.worst_frame_ms,
            hms(self.worst_frame_at_s),
            verdict(self.spikes_ok())
        );
        let _ = writeln!(s, "| Worst cook (CPU) | {:.2} |", self.worst_cook_ms);
        let _ = writeln!(s, "| Worst interval | {:.2} |", self.worst_interval_ms);
        let _ = writeln!(s, "| Frames over {SPIKE_LIMIT_MS} ms | {} |", self.spikes);
        if !self.errors.is_empty() {
            let _ = writeln!(s, "\n## Node errors at the end\n");
            for e in &self.errors {
                let _ = writeln!(s, "- {e}");
            }
        }
        let _ = writeln!(
            s,
            "\nPer-window samples are in soak.csv (elapsed seconds, phase, resident MB, \
             frames, mean/max frame, max cook, max interval, spikes, launches)."
        );
        s
    }
}

/// `h:mm:ss`.
fn hms(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// `YYYY-MM-DD HH:MM UTC` from seconds since the Unix epoch.
fn utc(secs: u64) -> String {
    // Civil from days (H. Hinnant), for days since 1970-01-01.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let t = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        t / 3600,
        t / 60 % 60
    )
}

/// This process's resident set size in bytes. Reads `/proc` on Linux and
/// asks `ps` (macOS) or `tasklist` (Windows) elsewhere, so it is called on
/// the sampler thread, never the frame loop.
pub fn rss_bytes() -> Option<u64> {
    let pid = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let _ = pid;
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(windows)]
    {
        // "atidarla.exe","1234","Console","1","123,456 K"
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let field = text.trim().rsplit("\",\"").next()?;
        let kb: u64 = field
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()?;
        Some(kb * 1024)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(kb * 1024)
    }
}

/// Reads resident memory every `every` on its own thread; the frame loop
/// only loads the latest value.
struct Sampler {
    latest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start(every: Duration) -> Self {
        let latest = Arc::new(AtomicU64::new(rss_bytes().unwrap_or(0)));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        let tick = every
            .min(Duration::from_secs(1))
            .max(Duration::from_millis(50));
        let thread = std::thread::Builder::new()
            .name("soak-rss".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    if let Some(b) = rss_bytes() {
                        l.store(b, Ordering::Relaxed);
                    }
                    std::thread::sleep(tick);
                }
            })
            .ok();
        Self {
            latest,
            stop,
            thread,
        }
    }

    fn mb(&self) -> f64 {
        self.latest.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
    }

    /// A reading taken now (end of the run), on this thread.
    fn fresh_mb(&self) -> f64 {
        rss_bytes().map_or_else(|| self.mb(), |b| b as f64 / (1024.0 * 1024.0))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One CSV row, sent to the writer thread.
struct Row {
    elapsed_s: f64,
    measured: bool,
    rss_mb: f64,
    window: Window,
}

const CSV_HEADER: &str = "elapsed_s,phase,rss_mb,frames,mean_frame_ms,max_frame_ms,max_cook_ms,max_interval_ms,spikes,launches";

/// Writes rows to `path` on its own thread, flushing each one.
fn csv_writer(path: &Path) -> Result<(Sender<Row>, std::thread::JoinHandle<()>)> {
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?,
    );
    writeln!(file, "{CSV_HEADER}")?;
    file.flush()?;
    let (tx, rx) = channel::<Row>();
    let thread = std::thread::Builder::new()
        .name("soak-csv".into())
        .spawn(move || {
            for r in rx {
                let w = r.window;
                let mean = if w.frames > 0 {
                    w.sum_ms / f64::from(w.frames)
                } else {
                    0.0
                };
                let _ = writeln!(
                    file,
                    "{:.1},{},{:.1},{},{:.3},{:.3},{:.3},{:.3},{},{}",
                    r.elapsed_s,
                    if r.measured { "measured" } else { "warmup" },
                    r.rss_mb,
                    w.frames,
                    mean,
                    w.max_frame_ms,
                    w.max_cook_ms,
                    w.max_interval_ms,
                    w.spikes,
                    w.launches
                );
                let _ = file.flush();
            }
        })?;
    Ok((tx, thread))
}

/// Launches the deck's next column (wrapping); false without a deck.
fn launch_next(
    ws: &mut ServeWorkspace,
    launcher: &mut atd_stage::Launcher,
    next: &mut usize,
) -> bool {
    let time = ws.time();
    let engine = ws.engine_mut();
    let stage = atd_stage::Stage::read(engine.network(), engine.registry());
    let Some(deck) = stage.deck() else {
        return false;
    };
    let columns = deck.columns();
    if columns == 0 {
        return false;
    }
    let clock = deck.clock().and_then(|c| engine.channels(c));
    let beat = atd_stage::beat_position(
        clock.and_then(|c| c.value("beat")),
        clock.and_then(|c| c.value("beat_phase")),
        time,
    );
    let edits = launcher.column(deck, *next % columns, beat);
    *next += 1;
    if !edits.is_empty()
        && let Err(e) = engine.edit("Soak: launch column", edits)
    {
        log::warn!("soak launch: {e}");
    }
    true
}

/// Runs an open project for `opts.duration`, writing CSV rows to `csv`.
pub(crate) fn soak(
    ws: &mut ServeWorkspace,
    project: &str,
    opts: &SoakOptions,
    csv: &Path,
) -> Result<Summary> {
    let (rows, writer) = csv_writer(csv)?;
    let sampler = Sampler::start(opts.sample);
    let mut summary = Summary {
        project: project.to_owned(),
        adapter: format!("{} ({:?})", ws.adapter.name, ws.adapter.backend),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        started: utc(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())),
        warmup_s: opts.warmup.as_secs_f64(),
        rss_start_mb: sampler.mb(),
        ..Summary::default()
    };
    let mut hist = Histogram::new();
    let mut measured_sum_ms = 0.0;
    let mut window = Window::default();
    let mut launcher = atd_stage::Launcher::default();
    let mut next_column = 0usize;
    let mut has_deck = true;

    let period = Duration::from_secs_f64(1.0 / FPS);
    let start = Instant::now();
    let mut next_frame = start;
    let mut next_sample = start + opts.sample;
    let mut next_cycle = opts.cycle.map(|c| start + c);
    let mut last_start: Option<Instant> = None;
    loop {
        let t0 = Instant::now();
        let elapsed = t0 - start;
        if elapsed >= opts.duration {
            break;
        }
        let measuring = elapsed >= opts.warmup;
        let interval_ms = last_start.map(|l| (t0 - l).as_secs_f64() * 1e3);
        last_start = Some(t0);

        if has_deck
            && let (Some(at), Some(every)) = (next_cycle, opts.cycle)
            && t0 >= at
        {
            has_deck = launch_next(ws, &mut launcher, &mut next_column);
            if has_deck {
                window.launches += 1;
                summary.launches += 1;
            } else {
                log::warn!("soak: --cycle-seconds given but the project has no Stage deck");
            }
            next_cycle = Some(at + every);
        }
        ws.frame();
        let cook_ms = t0.elapsed().as_secs_f64() * 1e3;
        let _ = ws
            .engine_mut()
            .device()
            .poll(wgpu::PollType::wait_indefinitely());
        let frame_ms = t0.elapsed().as_secs_f64() * 1e3;

        summary.frames += 1;
        window.add(frame_ms, cook_ms, interval_ms);
        if measuring {
            summary.frames_measured += 1;
            measured_sum_ms += frame_ms;
            hist.add(frame_ms);
            if frame_ms > summary.worst_frame_ms {
                summary.worst_frame_ms = frame_ms;
                summary.worst_frame_at_s = elapsed.as_secs_f64();
            }
            summary.worst_cook_ms = summary.worst_cook_ms.max(cook_ms);
            // The first interval after warm-up spans the warm-up boundary;
            // it is a real interval all the same.
            if let Some(i) = interval_ms {
                summary.worst_interval_ms = summary.worst_interval_ms.max(i);
            }
            if frame_ms > SPIKE_LIMIT_MS {
                summary.spikes += 1;
            }
        }

        let now = Instant::now();
        if now >= next_sample {
            let rss = sampler.mb();
            if measuring {
                if summary.rss_baseline_mb == 0.0 {
                    summary.rss_baseline_mb = rss;
                }
                summary.rss_peak_mb = summary.rss_peak_mb.max(rss);
            }
            let _ = rows.send(Row {
                elapsed_s: (now - start).as_secs_f64(),
                measured: measuring,
                rss_mb: rss,
                window: std::mem::take(&mut window),
            });
            next_sample += opts.sample;
        }

        next_frame += period;
        let now = Instant::now();
        if next_frame > now {
            std::thread::sleep(next_frame - now);
        } else if now - next_frame > period {
            // Fell behind by more than a frame: start counting again from now
            // rather than racing to catch up.
            next_frame = now;
        }
    }

    summary.ran_s = start.elapsed().as_secs_f64();
    summary.rss_end_mb = sampler.fresh_mb();
    if summary.rss_baseline_mb == 0.0 && summary.frames_measured > 0 {
        // The run ended before its first sample after warm-up.
        summary.rss_baseline_mb = summary.rss_end_mb;
    }
    summary.rss_peak_mb = summary.rss_peak_mb.max(summary.rss_end_mb);
    if window.frames > 0 {
        let _ = rows.send(Row {
            elapsed_s: summary.ran_s,
            measured: true,
            rss_mb: summary.rss_end_mb,
            window,
        });
    }
    drop(rows);
    let _ = writer.join();

    if summary.frames_measured > 0 {
        summary.mean_frame_ms = measured_sum_ms / summary.frames_measured as f64;
        summary.p99_frame_ms = hist.percentile(99.0);
    }
    let engine = ws.engine_mut();
    summary.errors = engine
        .network()
        .nodes()
        .filter_map(|n| engine.error(n.id).map(|e| format!("{}: {e}", n.name)))
        .collect();
    Ok(summary)
}

pub fn run(args: &SoakArgs) -> Result<()> {
    let opts = args.options()?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("create {}", args.out.display()))?;
    let mut ws = ServeWorkspace::open(&args.project, false)?;
    let name = args.project.file_name().map_or_else(
        || args.project.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    eprintln!(
        "atidarla soak: {name} for {} (warm-up {}), report in {}",
        hms(opts.duration.as_secs_f64()),
        hms(opts.warmup.as_secs_f64()),
        args.out.display()
    );
    let summary = soak(&mut ws, &name, &opts, &args.out.join("soak.csv"))?;
    let report = args.out.join("soak.md");
    std::fs::write(&report, summary.markdown())
        .with_context(|| format!("write {}", report.display()))?;
    println!(
        "PT-10 {}: memory {:+.1} MB after warm-up (peak {:.1} MB), worst frame {:.2} ms; {}",
        if summary.passed() { "pass" } else { "FAIL" },
        summary.growth_mb(),
        summary.rss_peak_mb,
        summary.worst_frame_ms,
        report.display()
    );
    if !summary.passed() {
        bail!("PT-10 failed; see {}", report.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_or_skip() -> bool {
        if atd_gpu::GpuContext::headless().is_ok() {
            return true;
        }
        assert!(
            !std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        false
    }

    #[test]
    fn verdict_follows_the_pt10_thresholds() {
        let ok = Summary {
            frames_measured: 100,
            rss_baseline_mb: 300.0,
            rss_end_mb: 399.0,
            worst_frame_ms: 49.9,
            ..Summary::default()
        };
        assert!(ok.passed());
        let leak = Summary {
            rss_end_mb: 400.5,
            ..ok.clone()
        };
        assert!(!leak.leak_ok() && leak.spikes_ok() && !leak.passed());
        let spike = Summary {
            worst_frame_ms: 50.1,
            ..ok.clone()
        };
        assert!(spike.leak_ok() && !spike.spikes_ok());
        let unmeasured = Summary {
            frames_measured: 0,
            ..ok
        };
        assert!(!unmeasured.passed(), "nothing after warm-up is not a pass");
        assert!(leak.markdown().contains("**PT-10: FAIL**"));
    }

    #[test]
    fn histogram_percentiles() {
        let mut h = Histogram::new();
        for _ in 0..99 {
            h.add(16.0);
        }
        h.add(500.0);
        assert!((h.percentile(50.0) - 16.1).abs() < 1e-9);
        assert!((h.percentile(99.0) - 16.1).abs() < 1e-9);
        assert!(
            (h.percentile(100.0) - 200.1).abs() < 1e-9,
            "overflow bucket"
        );
    }

    #[test]
    fn durations_add_up_and_default_to_a_day() {
        let args = |h, m, s| SoakArgs {
            project: PathBuf::new(),
            hours: h,
            minutes: m,
            seconds: s,
            warmup: 1.0,
            out: PathBuf::new(),
            sample: 1.0,
            cycle_seconds: None,
        };
        assert_eq!(
            args(None, None, None).options().unwrap().duration,
            Duration::from_secs(86_400)
        );
        assert_eq!(
            args(Some(1.0), Some(2.0), Some(3.0))
                .options()
                .unwrap()
                .duration,
            Duration::from_secs(3723)
        );
        assert!(
            args(None, None, Some(0.5)).options().is_err(),
            "shorter than warm-up"
        );
    }

    #[test]
    fn dates_and_clock_times() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_793_444_400), "2026-10-31 11:00 UTC");
        assert_eq!(hms(3723.4), "1:02:03");
    }

    #[test]
    fn resident_memory_is_readable() {
        let b = rss_bytes().expect("rss on this platform");
        assert!(b > 1024 * 1024, "{b} bytes");
    }

    /// A few seconds of the Stage deck example with column launches: frames
    /// are measured, memory is sampled, both files are written.
    #[test]
    fn a_short_soak_writes_its_report() {
        if !gpu_or_skip() {
            return;
        }
        let project =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/stage-deck.atidarla");
        let out = std::env::temp_dir().join(format!("atd-soak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let mut ws = ServeWorkspace::open(&project, false).unwrap();
        let opts = SoakOptions {
            duration: Duration::from_secs(3),
            warmup: Duration::from_secs(1),
            sample: Duration::from_millis(500),
            cycle: Some(Duration::from_millis(400)),
        };
        let csv = out.join("soak.csv");
        let s = soak(&mut ws, "stage-deck", &opts, &csv).unwrap();
        assert!(s.frames > 60, "{} frames in 3 s", s.frames);
        assert!(s.frames_measured > 0 && s.frames_measured < s.frames);
        assert!(s.launches >= 5, "{} launches", s.launches);
        assert!(s.rss_baseline_mb > 1.0 && s.rss_end_mb > 1.0);
        assert!(s.worst_frame_ms > 0.0 && s.mean_frame_ms > 0.0);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let rows = std::fs::read_to_string(&csv).unwrap();
        let mut lines = rows.lines();
        assert_eq!(lines.next(), Some(CSV_HEADER));
        let body: Vec<&str> = lines.collect();
        assert!(body.len() >= 5, "{rows}");
        assert!(body.iter().any(|l| l.contains(",warmup,")));
        assert!(body.iter().any(|l| l.contains(",measured,")));
        assert!(s.markdown().contains("Growth after warm-up"));
        std::fs::remove_dir_all(&out).unwrap();
    }
}
