//! `lasair storage list|remove|uninstall`: what the app keeps per user
//! and removing it (PRD A70 c, `lsr_ai::storage`). The Windows MSI runs
//! `storage uninstall` when Lasair is uninstalled.
//!
//! An uninstall nobody watches must never wait for a click: the model files
//! go only on an explicit Yes, and every other outcome (no terminal, no
//! visible desktop, a dialog that fails or is not answered in two minutes)
//! keeps them. On Windows, when this process runs as someone other than the
//! person signed in to the session (an elevation prompt answered with an
//! administrator's account), the folders it would see are that account's,
//! not the user's: it removes nothing.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use lsr_ai::storage::{Entry, Kind, Removal, Storage, human_size, models_explanation};

#[cfg(windows)]
#[path = "storage_win.rs"]
mod win;
#[cfg(windows)]
use win::{dialog, other_user};

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Show what Lasair keeps for this user, with sizes (the default).
    List,
    /// Remove one or more kinds: packages, models, cache, settings.
    Remove {
        #[arg(required = true)]
        kinds: Vec<Kind>,
    },
    /// Remove everything Lasair installed for this user (AI Host
    /// packages, caches, logs, settings, host tokens), then the model files
    /// as `--models` says.
    Uninstall {
        #[arg(long, value_enum, default_value_t = Models::Ask)]
        models: Models,
    },
}

/// What `storage uninstall` does with the model files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Models {
    Keep,
    Remove,
    /// Ask with a dialog (or in the terminal); keep them when nobody can be
    /// asked or nobody answers.
    Ask,
}

/// What a native dialog made of the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "only Windows has a native dialog here")
)]
pub enum Dialog {
    Yes,
    No,
    /// Nobody can see a dialog (Windows session 0, or a window station that
    /// is not visible): not shown.
    Unattended,
    /// Shown, and not answered in [`ANSWER_WITHIN`].
    TimedOut,
    /// Could not be shown, or ended without an answer.
    Failed,
}

/// How long the uninstall's question waits before keeping the models.
pub const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

/// Who and what `storage` can ask; [`Host::system`] is this machine's, tests
/// make their own.
pub struct Host<'a> {
    /// Why this process must not act for the person at the screen (it runs
    /// as another account), if it must not.
    pub other_user: Option<String>,
    /// A native dialog; `None` where there is none (macOS, Linux: the app's
    /// Remove app data… asks in its own window).
    pub dialog: AskDialog<'a>,
    /// The answer typed in a terminal, or `None` without one.
    pub terminal: AskTerminal<'a>,
}

pub type AskDialog<'a> = &'a dyn Fn(&str) -> Option<Dialog>;
pub type AskTerminal<'a> = &'a dyn Fn(&str) -> Option<String>;

impl Host<'static> {
    pub fn system() -> Self {
        Self {
            other_user: other_user(),
            dialog: &dialog,
            terminal: &terminal,
        }
    }
}

pub fn run(cmd: Option<StorageCommand>) -> Result<()> {
    run_with(&Storage::user(), cmd, &Host::system())
}

pub fn run_with(storage: &Storage, cmd: Option<StorageCommand>, host: &Host) -> Result<()> {
    match cmd.unwrap_or(StorageCommand::List) {
        StorageCommand::List => {
            let entries = storage.list();
            if entries.is_empty() {
                println!("Lasair keeps nothing for this user");
            }
            for e in &entries {
                println!("{}", line(e));
            }
            Ok(())
        }
        StorageCommand::Remove { kinds } => report(&storage.remove(&kinds)),
        StorageCommand::Uninstall { models } => {
            if let Some(why) = &host.other_user {
                println!("{why}: removing nothing, keeping everything");
                for e in &storage.list() {
                    println!("kept   {}", line(e));
                }
                return Ok(());
            }
            let first = storage.remove_everything_but_models();
            print_removal(&first);
            let kept: Vec<Entry> = storage
                .list()
                .into_iter()
                .filter(|e| e.kind == Kind::Models)
                .collect();
            let remove = !kept.is_empty()
                && match models {
                    Models::Keep => false,
                    Models::Remove => true,
                    Models::Ask => consent(&kept, host),
                };
            let second = if remove {
                storage.remove(&[Kind::Models])
            } else {
                Removal::default()
            };
            let failed = first.failed.len() + second.failed.len();
            print_removal(&second);
            if !remove {
                for e in &kept {
                    println!("kept   {}", line(e));
                }
            }
            if failed > 0 {
                bail!("{failed} item(s) could not be removed");
            }
            Ok(())
        }
    }
}

fn line(e: &Entry) -> String {
    format!(
        "{:<9} {:>9}  {:<26} {}",
        e.kind.as_str(),
        human_size(e.size),
        e.what,
        e.path.display()
    )
}

fn print_removal(r: &Removal) {
    for e in &r.removed {
        println!("removed {}", line(e));
    }
    for f in &r.failed {
        eprintln!("{f}");
    }
}

fn report(r: &Removal) -> Result<()> {
    if r.removed.is_empty() && r.failed.is_empty() {
        println!("nothing to remove");
    }
    print_removal(r);
    if !r.failed.is_empty() {
        bail!("{} item(s) could not be removed", r.failed.len());
    }
    println!("freed {}", human_size(r.freed()));
    Ok(())
}

/// The question A70 asks about the model files.
fn question(kept: &[Entry]) -> String {
    format!(
        "Lasair removed its AI Host packages, caches, logs and settings. Its model files are \
         still in storage.\n\n{}\n\n\
         Remove the model files too?\n\n\
         Yes removes them. No keeps them in storage. With no answer in {} minutes, they are \
         kept.",
        models_explanation(kept),
        ANSWER_WITHIN.as_secs() / 60
    )
}

/// Whether to remove the models: only an explicit Yes, in the dialog or
/// else the terminal, removes them.
fn consent(kept: &[Entry], host: &Host) -> bool {
    let text = question(kept);
    match (host.dialog)(&text) {
        Some(Dialog::Yes) => return true,
        Some(Dialog::No) => return false,
        Some(Dialog::Unattended) => {
            println!("no one can see a dialog: keeping the model files");
            return false;
        }
        Some(Dialog::TimedOut) => {
            println!("no answer: keeping the model files");
            return false;
        }
        Some(Dialog::Failed) | None => {}
    }
    match (host.terminal)(&format!("{text} [y/N] ")) {
        Some(answer) => matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"),
        None => {
            println!("no one to ask: keeping the model files");
            false
        }
    }
}

/// The answer line, when stdin and stderr are a terminal.
fn terminal(prompt: &str) -> Option<String> {
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(answer)
}

/// No native dialog from the command line here: the app's Remove app
/// data… asks in its own window.
#[cfg(not(windows))]
fn dialog(_text: &str) -> Option<Dialog> {
    None
}

/// Elsewhere the process is the user who ran it.
#[cfg(not(windows))]
fn other_user() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_question_names_the_folder_size_and_reuse() {
        let kept = [
            Entry {
                kind: Kind::Models,
                what: "model files".into(),
                path: "/c/Lasair/models".into(),
                size: 2_000_000_000,
            },
            Entry {
                kind: Kind::Models,
                what: "licence acceptances".into(),
                path: "/c/Lasair/accepted.toml".into(),
                size: 0,
            },
        ];
        let q = question(&kept);
        assert!(q.contains("2.00 GB, in /c/Lasair/models"), "{q}");
        assert!(q.contains("reinstall") && q.contains("other tools"), "{q}");
        assert!(q.contains("no answer in 2 minutes"), "{q}");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lsr-cli-consent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A user's storage under `root` with packages and a model in it.
    fn storage(root: &Path) -> Storage {
        let s = Storage {
            packages: root.join("data/ai-host"),
            models: root.join("cache/models"),
            models_chosen: false,
            settings: root.join("cache"),
            config: None,
        };
        for f in [
            s.packages.join("site-packages/a.py"),
            s.models.join("sd-turbo/w.bin"),
        ] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"x").unwrap();
        }
        s
    }

    /// Runs `uninstall --models <models>` with `dialog` and `terminal`;
    /// returns whether the models are still there and whether the packages
    /// are.
    fn uninstall(
        name: &str,
        models: Models,
        other_user: Option<&str>,
        dialog: AskDialog,
        terminal: AskTerminal,
    ) -> (bool, bool) {
        let root = scratch(name);
        let s = storage(&root);
        let host = Host {
            other_user: other_user.map(str::to_owned),
            dialog,
            terminal,
        };
        run_with(&s, Some(StorageCommand::Uninstall { models }), &host).unwrap();
        let kept = (s.models.exists(), s.packages.exists());
        let _ = std::fs::remove_dir_all(&root);
        kept
    }

    fn panics(_: &str) -> Option<Dialog> {
        panic!("asked")
    }

    fn no_tty(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn only_an_explicit_yes_removes_the_models() {
        let says = |d: Dialog| move |_: &str| Some(d);
        let types = |s: &'static str| move |_: &str| Some(s.to_owned());
        // (name, dialog, terminal, models removed)
        let cases: [(&str, AskDialog, AskTerminal, bool); 9] = [
            ("yes", &says(Dialog::Yes), &no_tty, true),
            ("no", &says(Dialog::No), &types("y"), false),
            ("timeout", &says(Dialog::TimedOut), &types("y"), false),
            ("unattended", &says(Dialog::Unattended), &types("y"), false),
            ("failed-no-tty", &says(Dialog::Failed), &no_tty, false),
            ("none-no-tty", &|_| None, &no_tty, false),
            ("tty-yes", &|_| None, &types("yes\n"), true),
            ("tty-enter", &|_| None, &types("\n"), false),
            ("failed-tty-yes", &says(Dialog::Failed), &types("y\n"), true),
        ];
        for (name, dialog, terminal, removed) in cases {
            let (models, packages) = uninstall(name, Models::Ask, None, dialog, terminal);
            assert_eq!(!models, removed, "{name}");
            assert!(!packages, "{name}: the packages always go");
        }
    }

    #[test]
    fn keep_and_remove_never_ask() {
        let (models, packages) = uninstall("keep", Models::Keep, None, &panics, &no_tty);
        assert!(models && !packages);
        let (models, _) = uninstall("remove", Models::Remove, None, &panics, &no_tty);
        assert!(!models);
    }

    #[test]
    fn another_account_removes_nothing() {
        let (models, packages) = uninstall(
            "other",
            Models::Remove,
            Some("running as admin, not as mark"),
            &panics,
            &no_tty,
        );
        assert!(models && packages, "everything is kept");
    }
}
