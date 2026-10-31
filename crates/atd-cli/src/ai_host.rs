//! `atidarla ai-host status|check|install|remove`: the AI Host an installer
//! ships and its optional packages (PRD A68, `atd_ai::hostenv`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use atd_ai::hostenv::{self, Bundled, Source};
use atd_ai::supervisor::{HostState, Supervisor, default_local_command};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum AiHostCommand {
    /// Show which local host runs, the extras this build offers and those
    /// installed.
    Status,
    /// Start the local host, wait until it is ready and list its adapters
    /// and built-in models. Reaches nothing beyond this machine.
    Check {
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Install optional adapters' packages (vision, stt, diffusion, cloud)
    /// for the bundled host into your user folder, from the locked,
    /// hash-pinned requirements shipped with the app (wheels only).
    Install {
        /// Extras to add; those already installed stay.
        #[arg(required = true)]
        extras: Vec<String>,
        /// Allow this one install to download from PyPI although Sovereign
        /// mode (the default) blocks the internet.
        #[arg(long, conflicts_with = "wheelhouse")]
        allow_internet: bool,
        /// Install offline from a folder of wheels instead (see
        /// docs/install.md for preparing one on another machine).
        #[arg(long)]
        wheelhouse: Option<PathBuf>,
    },
    /// Remove every installed extra (the built-in adapter keeps working).
    Remove,
}

fn bundled() -> Result<Bundled> {
    Bundled::find().ok_or_else(|| {
        anyhow!(
            "this is not an installed Atidarla, so there is no bundled AI Host: in a source \
             checkout, add extras with `uv sync --extra <extra>` in ai-host/ (ai-host/README.md)"
        )
    })
}

pub fn run(cmd: AiHostCommand) -> Result<()> {
    match cmd {
        AiHostCommand::Status => status(),
        AiHostCommand::Check { timeout } => check(Duration::from_secs(timeout)),
        AiHostCommand::Install {
            extras,
            allow_internet,
            wheelhouse,
        } => {
            let b = bundled()?;
            let source = wheelhouse.map_or(Source::Index, Source::Wheelhouse);
            let root = hostenv::data_root();
            let now = hostenv::install(&b, &root, &extras, &source, allow_internet)?;
            println!(
                "installed: {} (in {})",
                now.join(", "),
                b.packages(&root).site().display()
            );
            println!("the AI Host picks them up the next time it starts");
            Ok(())
        }
        AiHostCommand::Remove => {
            let b = bundled()?;
            hostenv::remove_all(&b, &hostenv::data_root())?;
            println!("removed the AI Host extras");
            Ok(())
        }
    }
}

fn status() -> Result<()> {
    let cmd = default_local_command();
    let program = PathBuf::from(&cmd.program);
    let args: Vec<String> = cmd
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let source = if std::env::var_os("ATD_AI_HOST_CMD").is_some() {
        "ATD_AI_HOST_CMD"
    } else if Bundled::find().is_some() {
        "bundled with the app"
    } else {
        "development checkout"
    };
    println!("local host:  {source}");
    println!("command:     {} {}", program.display(), args.join(" "));
    for p in hostenv::python_path(&cmd) {
        println!("python path: {}", p.display());
    }
    if let Some(b) = Bundled::find() {
        let root = hostenv::data_root();
        let installed = b.packages(&root).installed();
        println!(
            "python:      {} ({})",
            b.python().display(),
            b.python_version
        );
        println!("extras:      {}", b.extras().join(", "));
        println!(
            "installed:   {}",
            if installed.is_empty() {
                "none (built-in adapter only)".to_owned()
            } else {
                installed.join(", ")
            }
        );
        println!("packages:    {}", b.packages(&root).site().display());
    }
    Ok(())
}

fn check(timeout: Duration) -> Result<()> {
    let sup = Supervisor::new();
    sup.set(atd_ai::supervisor::HostSpec {
        name: "local".into(),
        kind: atd_ai::supervisor::HostKind::Local(default_local_command()),
    });
    sup.want("local");
    let deadline = Instant::now() + timeout;
    let started = Instant::now();
    loop {
        let snap = sup.host("local").expect("local host");
        match &snap.state {
            HostState::Ready => {
                let caps = snap.caps.unwrap_or_default();
                println!(
                    "local host ready in {:.1} s (pid {})",
                    started.elapsed().as_secs_f64(),
                    snap.pid.map_or("?".into(), |p| p.to_string())
                );
                println!("adapters: {}", caps.adapters.join(", "));
                println!("models:   {}", caps.models.join(", "));
                println!("runtimes: {}", caps.runtimes.join(", "));
                sup.shutdown();
                return Ok(());
            }
            HostState::Failed(why) => {
                sup.shutdown();
                bail!("the local AI host failed to start: {why}");
            }
            _ if Instant::now() > deadline => {
                sup.shutdown();
                bail!("the local AI host was not ready within {timeout:?}");
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
