//! `lasair ai-host status|check|install|remove`: the AI Host an installer
//! ships and its optional packages (PRD A68, A70, `lsr_ai::hostenv`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use clap::Subcommand;
use lsr_ai::hostenv::{self, Bundled, DIFFUSION, DIFFUSION_CUDA, Recommendation, Source};
use lsr_ai::supervisor::{HostState, Supervisor, default_local_command};

#[derive(Debug, Subcommand)]
pub enum AiHostCommand {
    /// Show which local host runs, the extras this build offers, those
    /// installed and those this machine should get.
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
    /// `diffusion` takes PyTorch's CUDA build (`diffusion-cuda`) on a
    /// Windows or Linux machine with a new enough NVIDIA driver.
    Install {
        /// Extras to add; those already installed stay (`diffusion` and
        /// `diffusion-cuda` replace each other).
        #[arg(
            required_unless_present = "recommended",
            conflicts_with = "recommended"
        )]
        extras: Vec<String>,
        /// Install the extras this machine should get (`status` shows them
        /// and why); `cloud` stays opt-in.
        #[arg(long)]
        recommended: bool,
        /// Use PyTorch's standard build for diffusion even on an NVIDIA
        /// machine.
        #[arg(long)]
        cpu: bool,
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
            "this is not an installed Lasair, so there is no bundled AI Host: in a source \
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
            recommended,
            cpu,
            allow_internet,
            wheelhouse,
        } => {
            let b = bundled()?;
            let (extras, note) =
                choose(extras, recommended, cpu, || hostenv::recommended_extras(&b));
            if let Some(note) = note {
                println!("{note}");
            }
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

/// The extras to install for a request, and what to tell the user about
/// the diffusion build. `diffusion` becomes `diffusion-cuda` when this
/// machine is recommended it (`recommend` runs `nvidia-smi`, so it is only
/// called when needed); `cpu` turns either back into `diffusion`.
fn choose(
    requested: Vec<String>,
    recommended: bool,
    cpu: bool,
    recommend: impl FnOnce() -> Recommendation,
) -> (Vec<String>, Option<String>) {
    let (mut extras, mut note) = if recommended {
        let rec = recommend();
        (rec.extras, Some(rec.reason))
    } else if !cpu && requested.iter().any(|e| e == DIFFUSION) {
        let rec = recommend();
        let cuda = rec.extras.iter().any(|e| e == DIFFUSION_CUDA);
        let extras = requested
            .into_iter()
            .map(|e| {
                if cuda && e == DIFFUSION {
                    DIFFUSION_CUDA.to_owned()
                } else {
                    e
                }
            })
            .collect();
        let note = if cuda {
            format!(
                "{} (`{DIFFUSION_CUDA}`; `--cpu` installs the standard build)",
                rec.reason
            )
        } else {
            rec.reason
        };
        (extras, Some(note))
    } else {
        (requested, None)
    };
    if cpu && extras.iter().any(|e| e == DIFFUSION_CUDA) {
        extras = extras
            .into_iter()
            .map(|e| {
                if e == DIFFUSION_CUDA {
                    DIFFUSION.to_owned()
                } else {
                    e
                }
            })
            .collect();
        note = Some(format!(
            "`--cpu`: installing `{DIFFUSION}`, PyTorch's standard build"
        ));
    }
    extras.sort();
    extras.dedup();
    (extras, note)
}

fn status() -> Result<()> {
    let cmd = default_local_command();
    let program = PathBuf::from(&cmd.program);
    let args: Vec<String> = cmd
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let source = if std::env::var_os("LSR_AI_HOST_CMD").is_some() {
        "LSR_AI_HOST_CMD"
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
        let rec = hostenv::recommended_extras(&b);
        println!("recommended: {}", rec.extras.join(", "));
        println!("             {}", rec.reason);
    }
    Ok(())
}

fn check(timeout: Duration) -> Result<()> {
    let sup = Supervisor::new();
    sup.set(lsr_ai::supervisor::HostSpec {
        name: "local".into(),
        kind: lsr_ai::supervisor::HostKind::Local(default_local_command()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn rec(extras: &[&str]) -> Recommendation {
        Recommendation {
            extras: strings(extras),
            reason: "because".into(),
        }
    }

    #[test]
    fn diffusion_takes_the_cuda_build_when_recommended_unless_cpu() {
        let nvidia = || rec(&["diffusion-cuda", "stt", "vision"]);
        let (extras, note) = choose(strings(&["diffusion", "vision"]), false, false, nvidia);
        assert_eq!(extras, ["diffusion-cuda", "vision"]);
        assert!(note.unwrap().contains("--cpu"));

        let (extras, _) = choose(strings(&["diffusion"]), false, true, || unreachable!());
        assert_eq!(extras, ["diffusion"], "--cpu needs no driver query");
        let (extras, _) = choose(strings(&["diffusion-cuda"]), false, true, || unreachable!());
        assert_eq!(extras, ["diffusion"]);

        let cpu_only = || rec(&["diffusion", "stt", "vision"]);
        let (extras, note) = choose(strings(&["diffusion"]), false, false, cpu_only);
        assert_eq!(extras, ["diffusion"]);
        assert_eq!(note.as_deref(), Some("because"), "says why not CUDA");

        // No diffusion asked for: no driver query, no note.
        let (extras, note) = choose(strings(&["vision"]), false, false, || unreachable!());
        assert_eq!((extras, note), (strings(&["vision"]), None));
    }

    #[test]
    fn recommended_installs_the_recommendation() {
        let nvidia = || rec(&["diffusion-cuda", "stt", "vision"]);
        let (extras, note) = choose(vec![], true, false, nvidia);
        assert_eq!(extras, ["diffusion-cuda", "stt", "vision"]);
        assert_eq!(note.as_deref(), Some("because"));
        let (extras, _) = choose(vec![], true, true, nvidia);
        assert_eq!(extras, ["diffusion", "stt", "vision"]);
    }

    #[test]
    fn install_takes_extras_or_recommended() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: AiHostCommand,
        }
        let parse = |args: &[&str]| Cli::try_parse_from([&["x"], args].concat()).map(|c| c.cmd);
        assert!(matches!(
            parse(&["install", "--recommended", "--cpu"]),
            Ok(AiHostCommand::Install {
                recommended: true,
                cpu: true,
                ..
            })
        ));
        assert!(
            parse(&["install"]).is_err(),
            "needs extras or --recommended"
        );
        assert!(parse(&["install", "vision", "--recommended"]).is_err());
    }
}
