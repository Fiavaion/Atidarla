//! `atidarla storage list|remove|uninstall`: what the app keeps per user
//! and removing it (PRD A70 c, `atd_ai::storage`). The Windows MSI runs
//! `storage uninstall` when Atidarla is uninstalled.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, bail};
use atd_ai::storage::{Entry, Kind, Removal, Storage};
use clap::{Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Show what Atidarla keeps for this user, with sizes (the default).
    List,
    /// Remove one or more kinds: packages, models, cache, settings.
    Remove {
        #[arg(required = true)]
        kinds: Vec<Kind>,
    },
    /// Remove everything Atidarla installed for this user (AI Host
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
    /// asked.
    Ask,
}

pub fn run(cmd: Option<StorageCommand>) -> Result<()> {
    let storage = Storage::user();
    match cmd.unwrap_or(StorageCommand::List) {
        StorageCommand::List => {
            let entries = storage.list();
            if entries.is_empty() {
                println!("Atidarla keeps nothing for this user");
            }
            for e in &entries {
                println!("{}", line(e));
            }
            Ok(())
        }
        StorageCommand::Remove { kinds } => report(&storage.remove(&kinds)),
        StorageCommand::Uninstall { models } => {
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
                    Models::Ask => ask(&kept),
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
        human(e.size),
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
    println!("freed {}", human(r.freed()));
    Ok(())
}

/// Bytes for people, in decimal units as file managers show them.
fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", b / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", b / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// The question A70 asks about the model files.
fn question(kept: &[Entry]) -> String {
    let size: u64 = kept.iter().map(|e| e.size).sum();
    let folder = kept
        .iter()
        .find(|e| e.what == "model files")
        .or(kept.first())
        .map(|e| e.path.display().to_string())
        .unwrap_or_default();
    format!(
        "Atidarla removed its AI Host packages, caches, logs and settings.\n\n\
         Its model files ({}, in {folder}) are still in storage. They may be \
         reused: a reinstall of Atidarla, or other tools pointed at that \
         folder, can use them without downloading them again.\n\n\
         Remove the model files too?\n\n\
         Yes removes them. No keeps them in storage.",
        human(size)
    )
}

/// Asks whether to remove the models: a native dialog, else the terminal;
/// keeps them when neither can ask.
fn ask(kept: &[Entry]) -> bool {
    let text = question(kept);
    if let Some(answer) = dialog(&text) {
        return answer;
    }
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        eprint!("{text} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).is_ok() {
            return matches!(answer.trim(), "y" | "Y" | "yes" | "Yes");
        }
    }
    println!("no one to ask: keeping the model files");
    false
}

/// A Yes/No message box; `None` when it could not be shown. Defaults to No
/// (keep), and stays on top of the uninstaller's progress window.
#[cfg(windows)]
fn dialog(text: &str) -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDNO, IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
        MessageBoxW,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide("Uninstall Atidarla"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call;
    // a null owner window is allowed.
    let answer = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
    match answer {
        IDYES => Some(true),
        IDNO => Some(false),
        _ => None,
    }
}

/// No native dialog from the command line here: the app's Remove app
/// data… asks in its own window.
#[cfg(not(windows))]
fn dialog(_text: &str) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human(40), "40 B");
        assert_eq!(human(1_500), "1.5 KB");
        assert_eq!(human(2_340_000), "2.3 MB");
        assert_eq!(human(4_200_000_000), "4.20 GB");
    }

    #[test]
    fn the_question_names_the_folder_size_and_reuse() {
        let kept = [
            Entry {
                kind: Kind::Models,
                what: "model files".into(),
                path: "/c/Atidarla/models".into(),
                size: 2_000_000_000,
            },
            Entry {
                kind: Kind::Models,
                what: "licence acceptances".into(),
                path: "/c/Atidarla/accepted.toml".into(),
                size: 0,
            },
        ];
        let q = question(&kept);
        assert!(q.contains("2.00 GB, in /c/Atidarla/models"), "{q}");
        assert!(q.contains("reinstall") && q.contains("other tools"), "{q}");
    }
}
