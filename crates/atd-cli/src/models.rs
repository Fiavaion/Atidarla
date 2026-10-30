//! `atidarla models list|accept|download|verify|path` (FR-A12, docs/models.md).

use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use atd_ai::models::{
    Acceptances, DownloadOptions, FileState, Model, Status, Store, catalogue, start_download,
    start_verify, status, today,
};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// List the catalogue: licence class, size and status in the cache.
    List,
    /// Accept a model's licence (needed before it downloads).
    Accept {
        id: String,
        /// Accept without the interactive prompt (scripts).
        #[arg(long)]
        yes: bool,
    },
    /// Download a model into the per-user cache (resumes partial files).
    Download {
        id: String,
        /// Allow this one download to reach the internet although Sovereign
        /// mode (the default) blocks it.
        #[arg(long)]
        allow_internet: bool,
    },
    /// Check every file's SHA-256 against the catalogue.
    Verify { id: String },
    /// Print the model cache folder, or a model's folder.
    Path { id: Option<String> },
}

fn model(id: &str) -> Result<&'static Model> {
    catalogue().get(id).ok_or_else(|| {
        let ids: Vec<&str> = catalogue().iter().map(|m| m.id.as_str()).collect();
        anyhow!(
            "no model `{id}` in the catalogue (known: {})",
            ids.join(", ")
        )
    })
}

fn gb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

pub fn run(cmd: ModelsCommand) -> Result<()> {
    let store = Store::default();
    match cmd {
        ModelsCommand::Path { id: None } => println!("{}", store.root().display()),
        ModelsCommand::Path { id: Some(id) } => {
            println!("{}", store.model_dir(&model(&id)?.id).display());
        }
        ModelsCommand::List => {
            let accepted = Acceptances::load(store.accepted_path()).map_err(|e| anyhow!(e))?;
            println!(
                "{:<26} {:<16} {:<34} {:>9}  {:<15} accepted",
                "id", "class", "licence", "size", "status"
            );
            for m in catalogue().iter() {
                println!(
                    "{:<26} {:<16} {:<34} {:>9}  {:<15} {}",
                    m.id,
                    m.licence.class.as_str(),
                    m.licence.id,
                    gb(m.total_size()),
                    status(&store, m).as_str(),
                    if accepted.is_accepted(m) { "yes" } else { "no" }
                );
            }
        }
        ModelsCommand::Accept { id, yes } => {
            let m = model(&id)?;
            let mut accepted = Acceptances::load(store.accepted_path()).map_err(|e| anyhow!(e))?;
            println!(
                "{} ({}) is under {}: {}.\nRead it here: {}",
                m.name,
                m.id,
                m.licence.id,
                m.licence.class.as_str(),
                m.licence.url
            );
            if !yes {
                print!("Type `accept` to accept this {} licence: ", m.licence.class);
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                if line.trim() != "accept" {
                    bail!("not accepted");
                }
            }
            accepted.accept(m, today());
            accepted
                .save(store.accepted_path())
                .map_err(|e| anyhow!(e))?;
            println!("accepted; recorded in {}", store.accepted_path().display());
        }
        ModelsCommand::Download { id, allow_internet } => {
            let m = model(&id)?;
            let job = start_download(
                store.clone(),
                m.clone(),
                DownloadOptions {
                    allow_internet_once: allow_internet,
                    ..Default::default()
                },
            );
            let mut last = String::new();
            while !job.is_finished() {
                let p = job.progress();
                let line = format!("{:>5.1}% {}", p.fraction() * 100.0, p.file);
                if line != last {
                    eprint!("\r{line:<80}");
                    last = line;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            eprintln!();
            job.wait()
                .map_err(|_| anyhow!("the download thread panicked"))?
                .with_context(|| format!("downloading {id}"))?;
            println!("{id}: {}", status(&store, m));
        }
        ModelsCommand::Verify { id } => {
            let m = model(&id)?;
            let report = start_verify(store.clone(), m.clone())
                .wait()
                .map_err(|_| anyhow!("the verify thread panicked"))?;
            let ok = report
                .files
                .iter()
                .filter(|f| f.state == FileState::Ok)
                .count();
            for f in report.files.iter().filter(|f| f.state != FileState::Ok) {
                eprintln!(
                    "  {}: {:?} (expected {})",
                    f.path, f.state, f.expected_sha256
                );
            }
            let s = report.status();
            println!(
                "{id}: {s} ({ok}/{} files match their SHA-256, {}) in {}",
                m.files.len(),
                gb(m.total_size()),
                store.model_dir(&m.id).display()
            );
            if s != Status::Ready {
                bail!("{id} is {s}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Args {
        #[command(subcommand)]
        cmd: ModelsCommand,
    }

    #[test]
    fn subcommands_parse() {
        let p = |a: &[&str]| Args::try_parse_from([&["models"], a].concat()).map(|a| a.cmd);
        assert!(matches!(p(&["list"]), Ok(ModelsCommand::List)));
        assert!(matches!(
            p(&["download", "sd-turbo", "--allow-internet"]),
            Ok(ModelsCommand::Download {
                allow_internet: true,
                ..
            })
        ));
        assert!(matches!(
            p(&["accept", "taesd"]),
            Ok(ModelsCommand::Accept { yes: false, .. })
        ));
        assert!(matches!(p(&["path"]), Ok(ModelsCommand::Path { id: None })));
        assert!(p(&["verify"]).is_err(), "verify needs an id");
    }

    #[test]
    fn unknown_models_name_the_known_ones() {
        let e = model("nope").unwrap_err().to_string();
        assert!(e.contains("sd-turbo") && e.contains("taesd"), "{e}");
        assert_eq!(gb(2_580_000_000), "2.58 GB");
        assert_eq!(gb(9_793_889), "9.8 MB");
    }
}
