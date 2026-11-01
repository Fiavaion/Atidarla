//! The AI assist switch (PRD A70 b): one switch at the top of the AI Hosts
//! window. On installs the AI Host extras this machine can use, on a worker
//! thread, then starts the local host again so nodes find the new
//! adapters; off stops using the local host and keeps the packages. In
//! Sovereign mode turning it on first asks to allow this one download. The
//! state is kept per user (`ui.toml`), not in the project.
//!
//! [`Assist`] is the state machine; it returns an [`Effect`] for the caller
//! to apply to the supervisor, so tests run it with a fake install job.
//! Finding the bundled host and the recommendation (which runs
//! `nvidia-smi`) is a worker job too: nothing here waits (rule 1).

use std::path::PathBuf;
use std::time::Instant;

use lsr_ai::client::HostCommand;
use lsr_ai::hostenv::{self, Bundled, InstallError, Recommendation, Source};
use lsr_ai::models::Job;
use lsr_ai::storage::human_size;
use lsr_ai::supervisor::{HostKind, HostSpec, Supervisor, default_local_command};

/// What an install asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub extras: Vec<String>,
    pub source: Source,
    pub allow_internet_once: bool,
}

/// How an install ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The extras now installed, and the local host command that uses them.
    Installed {
        extras: Vec<String>,
        command: HostCommand,
    },
    Failed(String),
    Cancelled,
}

/// What this machine has, found on a worker.
#[derive(Debug, Clone)]
pub struct Probe {
    /// `None` in a development checkout.
    pub bundled: Option<Bundled>,
    pub installed: Vec<String>,
    pub recommendation: Recommendation,
    pub command: HostCommand,
}

impl Probe {
    /// Finds the bundled host, its installed extras and the recommended
    /// ones. Blocks (disk, `nvidia-smi`): call it on a worker.
    pub fn find() -> Self {
        let bundled = Bundled::find();
        let (installed, recommendation) = match &bundled {
            Some(b) => (
                b.packages(&hostenv::data_root()).installed(),
                hostenv::recommended_extras(b),
            ),
            None => (
                vec![],
                Recommendation {
                    extras: vec![],
                    reason: String::new(),
                },
            ),
        };
        Self {
            bundled,
            installed,
            recommendation,
            command: default_local_command(),
        }
    }

    /// The recommended extras not installed yet.
    pub fn missing(&self) -> Vec<String> {
        self.recommendation
            .extras
            .iter()
            .filter(|e| !self.installed.contains(e))
            .cloned()
            .collect()
    }
}

/// What the caller does to the supervisor after a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// (Re)start using the local host with this command.
    Use(HostCommand),
    /// Stop using the local host.
    Stop,
}

impl Effect {
    /// Applies it to the process's supervisor (short locks only).
    pub fn apply(self) {
        let sup = Supervisor::global();
        match self {
            Effect::None => {}
            Effect::Use(command) => {
                // Removed first so a running host restarts with the new
                // packages even when its command did not change.
                sup.remove("local");
                sup.set(HostSpec {
                    name: "local".into(),
                    kind: HostKind::Local(command),
                });
            }
            Effect::Stop => sup.remove("local"),
        }
    }
}

#[derive(Debug)]
pub enum Phase {
    Off,
    /// Sovereign mode: asking to allow this one download.
    Confirm,
    Installing {
        job: Job<Finish>,
        started: Instant,
        cancelling: bool,
    },
    On,
}

/// Starts an install job (the real one, or a fake in tests).
pub type Starter = Box<dyn Fn(Request) -> Job<Finish>>;

pub struct Assist {
    pub phase: Phase,
    probe: Probe,
    start: Starter,
    /// The last result: (text, is an error).
    pub note: Option<(String, bool)>,
    /// The question asked in [`Phase::Confirm`], built once.
    pub question: String,
    /// "Installing… (pip running, 12 s)", rebuilt once a second.
    pub progress: String,
    /// What off and on mean on this machine, rebuilt when the installed
    /// extras change.
    pub off_text: String,
    pub on_text: String,
    progress_secs: Option<u64>,
    /// The user settled the switch since [`Assist::take_changed`].
    changed: bool,
}

impl std::fmt::Debug for Assist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assist")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl Assist {
    /// The switch for `probe`, on or off as `saved` says, else on when
    /// extras are installed. Returns the effect of that start.
    pub fn new(probe: Probe, saved: Option<bool>, start: Starter) -> (Self, Effect) {
        let on = saved.unwrap_or(!probe.installed.is_empty());
        let mut assist = Self {
            phase: if on { Phase::On } else { Phase::Off },
            probe,
            start,
            note: None,
            question: String::new(),
            progress: String::new(),
            off_text: String::new(),
            on_text: String::new(),
            progress_secs: None,
            changed: false,
        };
        assist.texts();
        // On at start: the supervisor already has the local host.
        (assist, if on { Effect::None } else { Effect::Stop })
    }

    /// The real install job for `bundled`.
    pub fn installer(bundled: Bundled) -> Starter {
        Box::new(move |req: Request| {
            let bundled = bundled.clone();
            Job::spawn("lsr-ai-assist-install", move |p| {
                let root = hostenv::data_root();
                let done = hostenv::install_cancellable(
                    &bundled,
                    &root,
                    &req.extras,
                    &req.source,
                    req.allow_internet_once,
                    &|| p.cancelled(),
                );
                match done {
                    Ok(extras) => Finish::Installed {
                        extras,
                        command: default_local_command(),
                    },
                    Err(InstallError::Cancelled) => Finish::Cancelled,
                    Err(e) => Finish::Failed(e.to_string()),
                }
            })
        })
    }

    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    /// The switch reads on (also while asking or installing).
    pub fn is_on(&self) -> bool {
        !matches!(self.phase, Phase::Off)
    }

    /// What to keep in `ui.toml`, once, after the user turned the switch on
    /// (and any install finished) or off. A start, a refused question or a
    /// failed install keep nothing, so the default still follows the
    /// installed extras.
    pub fn take_changed(&mut self) -> Option<bool> {
        std::mem::take(&mut self.changed).then_some(matches!(self.phase, Phase::On))
    }

    /// The user turned the switch on.
    pub fn turn_on(&mut self, sovereign: bool) -> Effect {
        if !matches!(self.phase, Phase::Off) {
            return Effect::None;
        }
        self.note = None;
        let missing = self.probe.missing();
        if missing.is_empty() {
            self.phase = Phase::On;
            self.changed = true;
            return Effect::Use(self.probe.command.clone());
        }
        if sovereign {
            self.question = self.question_for(&missing);
            self.phase = Phase::Confirm;
            return Effect::None;
        }
        self.install(missing, Source::Index, false);
        Effect::None
    }

    /// The answer to the Sovereign question: download once, or not.
    pub fn answer(&mut self, allow: bool) {
        if !matches!(self.phase, Phase::Confirm) {
            return;
        }
        if allow {
            self.install(self.probe.missing(), Source::Index, true);
        } else {
            self.phase = Phase::Off;
        }
    }

    /// Installs from a folder of wheels instead of the internet.
    pub fn install_from_folder(&mut self, dir: PathBuf) {
        if matches!(self.phase, Phase::Off | Phase::Confirm) {
            self.note = None;
            let mut extras = self.probe.missing();
            if extras.is_empty() {
                extras.clone_from(&self.probe.recommendation.extras);
            }
            self.install(extras, Source::Wheelhouse(dir), false);
        }
    }

    /// The user turned the switch off: stops an install, or stops using
    /// the local host (the packages stay).
    pub fn turn_off(&mut self) -> Effect {
        match &mut self.phase {
            Phase::Installing {
                job, cancelling, ..
            } => {
                job.cancel();
                *cancelling = true;
                Effect::None
            }
            Phase::Confirm => {
                self.phase = Phase::Off;
                Effect::None
            }
            Phase::On => {
                self.phase = Phase::Off;
                self.changed = true;
                self.note = Some((
                    "AI assist is off. The packages stay installed.".into(),
                    false,
                ));
                Effect::Stop
            }
            Phase::Off => Effect::None,
        }
    }

    /// The packages were removed (Storage): off, without a note.
    pub fn packages_removed(&mut self) -> Effect {
        let effect = self.turn_off();
        self.probe.installed.clear();
        self.texts();
        self.note = None;
        effect
    }

    /// Takes a finished install's result and keeps the progress text
    /// current. Cheap when nothing changed.
    pub fn poll(&mut self) -> Effect {
        let Phase::Installing {
            job,
            started,
            cancelling,
        } = &mut self.phase
        else {
            return Effect::None;
        };
        let secs = started.elapsed().as_secs();
        if self.progress_secs != Some(secs) {
            self.progress_secs = Some(secs);
            let what = if *cancelling {
                "Cancelling…"
            } else {
                "Installing…"
            };
            self.progress = format!("{what} (pip running, {})", elapsed(secs));
        }
        let finish = match job.try_take() {
            None => return Effect::None,
            Some(Ok(finish)) => finish,
            Some(Err(_)) => Finish::Failed("the install stopped unexpectedly".into()),
        };
        match finish {
            Finish::Installed { extras, command } => {
                self.note = Some((format!("Installed {}.", extras.join(", ")), false));
                self.probe.installed = extras;
                self.probe.command = command.clone();
                self.texts();
                self.phase = Phase::On;
                self.changed = true;
                Effect::Use(command)
            }
            Finish::Failed(why) => {
                self.note = Some((why, true));
                self.phase = Phase::Off;
                Effect::None
            }
            Finish::Cancelled => {
                self.note = Some((
                    "Cancelled. The installed packages are as they were.".into(),
                    false,
                ));
                self.phase = Phase::Off;
                Effect::None
            }
        }
    }

    fn texts(&mut self) {
        let missing = self.probe.missing();
        self.off_text = if missing.is_empty() {
            "Off: AI nodes skip the local AI Host. Its packages are installed.".into()
        } else {
            format!(
                "Off: AI nodes skip the local AI Host. On installs {} (about {}).",
                missing.join(", "),
                human_size(hostenv::download_estimate(&missing))
            )
        };
        self.on_text = if self.probe.installed.is_empty() {
            "On: AI nodes use the local AI Host (built-in adapter only).".into()
        } else {
            format!(
                "On: AI nodes use the local AI Host with {}.",
                self.probe.installed.join(", ")
            )
        };
    }

    fn install(&mut self, extras: Vec<String>, source: Source, allow_internet_once: bool) {
        let job = (self.start)(Request {
            extras,
            source,
            allow_internet_once,
        });
        self.progress_secs = None;
        self.progress = "Installing… (pip running)".into();
        self.phase = Phase::Installing {
            job,
            started: Instant::now(),
            cancelling: false,
        };
    }

    /// The Sovereign question: the size of the download and its servers.
    /// pip downloads every extra it installs, the installed ones too.
    fn question_for(&self, missing: &[String]) -> String {
        let all = hostenv::merge_extras(&self.probe.installed, missing)
            .unwrap_or_else(|_| missing.to_vec());
        format!(
            "Sovereign mode is on. Turning AI assist on downloads the AI Host packages \
             ({}), about {}, from {}. This is allowed once, for this install only.",
            all.join(", "),
            human_size(hostenv::download_estimate(&all)),
            hostenv::index_servers(&all).join(", ")
        )
    }
}

fn elapsed(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        _ => format!("{} min {} s", secs / 60, secs % 60),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn command(tag: &str) -> HostCommand {
        HostCommand {
            program: tag.into(),
            args: vec![],
            env: vec![],
        }
    }

    fn probe(installed: &[&str]) -> Probe {
        Probe {
            bundled: None,
            installed: strings(installed),
            recommendation: Recommendation {
                extras: strings(&["diffusion-cuda", "stt", "vision"]),
                reason: "NVIDIA driver 581.57 runs CUDA 13.4".into(),
            },
            command: command("before"),
        }
    }

    /// A fake install: waits until the test hands it a result, or until it
    /// is cancelled. Records every request.
    #[derive(Clone, Default)]
    struct Fake {
        result: Arc<Mutex<Option<Finish>>>,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    impl Fake {
        fn starter(&self) -> Starter {
            let fake = self.clone();
            Box::new(move |req| {
                fake.requests.lock().unwrap().push(req);
                let result = fake.result.clone();
                Job::spawn("fake-install", move |p| {
                    loop {
                        if p.cancelled() {
                            return Finish::Cancelled;
                        }
                        if let Some(r) = result.lock().unwrap().take() {
                            return r;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                })
            })
        }

        fn finish(&self, f: Finish) {
            *self.result.lock().unwrap() = Some(f);
        }
    }

    /// Polls until the install has finished.
    fn settle(a: &mut Assist) -> Effect {
        for _ in 0..2000 {
            let effect = a.poll();
            if !matches!(a.phase, Phase::Installing { .. }) {
                return effect;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the install never finished");
    }

    #[test]
    fn it_starts_off_until_extras_are_installed_unless_saved() {
        let fake = Fake::default();
        let (a, effect) = Assist::new(probe(&[]), None, fake.starter());
        assert!(matches!(a.phase, Phase::Off));
        assert_eq!(effect, Effect::Stop);
        let (a, effect) = Assist::new(probe(&["vision"]), None, fake.starter());
        assert!(matches!(a.phase, Phase::On));
        assert_eq!(effect, Effect::None);
        let (a, _) = Assist::new(probe(&["vision"]), Some(false), fake.starter());
        assert!(matches!(a.phase, Phase::Off));
        let (a, _) = Assist::new(probe(&[]), Some(true), fake.starter());
        assert!(matches!(a.phase, Phase::On));
    }

    #[test]
    fn on_installs_what_is_missing_then_restarts_the_host() {
        let fake = Fake::default();
        let (mut a, _) = Assist::new(probe(&["vision"]), Some(false), fake.starter());
        assert_eq!(a.turn_on(false), Effect::None);
        assert!(matches!(a.phase, Phase::Installing { .. }));
        assert!(a.is_on() && a.take_changed().is_none());
        assert!(a.progress.starts_with("Installing… (pip running"));
        assert_eq!(
            fake.requests.lock().unwrap()[0],
            Request {
                extras: strings(&["diffusion-cuda", "stt"]),
                source: Source::Index,
                allow_internet_once: false,
            }
        );
        let extras = strings(&["diffusion-cuda", "stt", "vision"]);
        fake.finish(Finish::Installed {
            extras: extras.clone(),
            command: command("after"),
        });
        assert_eq!(settle(&mut a), Effect::Use(command("after")));
        assert!(matches!(a.phase, Phase::On));
        assert_eq!(a.take_changed(), Some(true));
        assert_eq!(a.take_changed(), None, "once");
        assert_eq!(a.probe().installed, extras);
        assert!(a.note.as_ref().unwrap().0.contains("Installed"));

        // Off keeps the packages; on again needs no install.
        assert_eq!(a.turn_off(), Effect::Stop);
        assert_eq!(a.take_changed(), Some(false));
        assert_eq!(a.turn_on(false), Effect::Use(command("after")));
        assert!(matches!(a.phase, Phase::On));
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn sovereign_mode_asks_first_naming_size_and_servers() {
        let fake = Fake::default();
        let (mut a, _) = Assist::new(probe(&[]), None, fake.starter());
        a.turn_on(true);
        assert!(matches!(a.phase, Phase::Confirm));
        assert!(fake.requests.lock().unwrap().is_empty(), "nothing yet");
        for part in ["GB", "pypi.org", "download.pytorch.org", "once"] {
            assert!(a.question.contains(part), "{part}: {}", a.question);
        }
        a.answer(false);
        assert!(matches!(a.phase, Phase::Off));

        a.turn_on(true);
        a.answer(true);
        assert!(matches!(a.phase, Phase::Installing { .. }));
        let req = fake.requests.lock().unwrap()[0].clone();
        assert!(req.allow_internet_once);
        assert_eq!(req.source, Source::Index);
    }

    #[test]
    fn a_folder_of_wheels_needs_no_internet() {
        let fake = Fake::default();
        let (mut a, _) = Assist::new(probe(&[]), None, fake.starter());
        a.turn_on(true);
        a.install_from_folder("/wheels".into());
        let req = fake.requests.lock().unwrap()[0].clone();
        assert_eq!(req.source, Source::Wheelhouse("/wheels".into()));
        assert!(!req.allow_internet_once);
    }

    #[test]
    fn failures_and_cancels_turn_it_back_off() {
        let fake = Fake::default();
        let (mut a, _) = Assist::new(probe(&[]), None, fake.starter());
        a.turn_on(false);
        fake.finish(Finish::Failed("pip failed (exit status: 1)".into()));
        assert_eq!(settle(&mut a), Effect::None);
        assert!(matches!(a.phase, Phase::Off));
        assert_eq!(a.note, Some(("pip failed (exit status: 1)".into(), true)));
        assert_eq!(a.take_changed(), None, "a failure keeps the saved state");

        a.turn_on(false);
        assert_eq!(a.turn_off(), Effect::None, "cancelling, not stopped yet");
        assert!(matches!(
            a.phase,
            Phase::Installing {
                cancelling: true,
                ..
            }
        ));
        assert_eq!(settle(&mut a), Effect::None);
        assert!(matches!(a.phase, Phase::Off));
        let (note, error) = a.note.clone().unwrap();
        assert!(note.starts_with("Cancelled") && !error, "{note}");
    }

    #[test]
    fn what_the_user_settles_is_what_ui_toml_keeps() {
        let dir = std::env::temp_dir().join(format!("lsr-assist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("ui.toml");
        let mut settings = crate::settings::UiSettings::load(path.clone());
        let fake = Fake::default();
        let (mut a, _) = Assist::new(probe(&[]), settings.ai_assist, fake.starter());
        assert_eq!(a.take_changed(), None, "the start is not saved");
        a.turn_on(false);
        fake.finish(Finish::Installed {
            extras: strings(&["vision"]),
            command: command("after"),
        });
        settle(&mut a);
        settings.ai_assist = a.take_changed();
        settings.save();
        let mut back = None;
        for _ in 0..100 {
            back = crate::settings::UiSettings::load(path.clone()).ai_assist;
            if back.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(back, Some(true));
        // The next start reads it back.
        let (a, effect) = Assist::new(probe(&[]), back, fake.starter());
        assert!(matches!(a.phase, Phase::On));
        assert_eq!(effect, Effect::None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn elapsed_time_reads_naturally() {
        assert_eq!(elapsed(5), "5 s");
        assert_eq!(elapsed(125), "2 min 5 s");
    }
}
