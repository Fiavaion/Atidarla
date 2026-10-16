//! `atidarla` — headless entry points (PRD §7.10).

mod bench;
mod docs;
mod isf_import;
mod project;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "atidarla", version = atd_core::VERSION, about = "Atidarla headless tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the performance suite and write JSON (FR-H4).
    Bench {
        /// Write results here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Skip cases that need a GPU.
        #[arg(long)]
        no_gpu: bool,
    },
    /// Deterministic offline render of a project (FR-H1, M2).
    Render { project: PathBuf },
    /// Run a project headless, optionally as a kiosk with watchdog (FR-H2, M3).
    Run { project: PathBuf },
    /// Bake a network to disk (M2).
    Bake { project: PathBuf },
    /// Expose MCP + OSC/REST without UI (FR-H3, M5).
    Serve { project: Option<PathBuf> },
    /// Check project folders: they load, validate against the operator
    /// registry and are in canonical form; with --gpu, every node cooks
    /// without errors.
    Check {
        /// Project folders (`*.atidarla`).
        #[arg(required = true)]
        projects: Vec<PathBuf>,
        /// Also cook every node on a GPU.
        #[arg(long)]
        gpu: bool,
        /// Frames to cook with --gpu, at 60 per second.
        #[arg(long, default_value_t = 60)]
        frames: u32,
    },
    /// Rewrite project folders in canonical form (after editing by hand).
    Fmt {
        #[arg(required = true)]
        projects: Vec<PathBuf>,
    },
    /// Print the operator registry as JSON (schemas + capability flags).
    Registry,
    /// Generate operator reference pages from the registry.
    Docs {
        /// Output directory.
        #[arg(long, default_value = "docs/operators")]
        out: PathBuf,
        /// Example projects to link from each page.
        #[arg(long, default_value = "examples")]
        examples: PathBuf,
        /// Fail if the committed pages differ from the generated ones.
        #[arg(long)]
        check: bool,
    },
    /// Translate ISF shaders (GLSL + JSON header) to WGSL operators (FR-K6).
    IsfImport {
        /// Directory searched recursively for `.fs` files (`.vs` alongside).
        dir: PathBuf,
        /// List every failure and group failures by kind.
        #[arg(long)]
        report: bool,
        /// Also build every shader on a GPU and cook it for two frames.
        #[arg(long)]
        gpu: bool,
        /// Write per-shader results as JSON.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Write the generated WGSL here.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Fail unless at least this percentage of shaders pass.
        #[arg(long)]
        min_pass: Option<f64>,
        /// With --gpu, seconds one shader may take to compile.
        #[arg(long, default_value_t = 60)]
        compile_timeout: u64,
        /// With --gpu, write a contact sheet of every output to this PNG.
        #[arg(long, requires = "gpu")]
        thumbs: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,naga=off"))
        .init();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Bench { out, no_gpu } => bench::run(out.as_deref(), !no_gpu),
        Command::Registry => {
            println!(
                "{}",
                serde_json::to_string_pretty(&builtin_registry().export_json()).expect("json")
            );
            Ok(())
        }
        Command::Docs {
            out,
            examples,
            check,
        } => docs::run(&builtin_registry(), &out, &examples, check),
        Command::Check {
            projects,
            gpu,
            frames,
        } => project::check(&projects, project::CheckOptions { gpu, frames }),
        Command::Fmt { projects } => project::fmt(&projects),
        Command::IsfImport {
            dir,
            report,
            gpu,
            json,
            out,
            thumbs,
            min_pass,
            compile_timeout,
        } => isf_import::run(
            &dir,
            &isf_import::Options {
                report,
                gpu,
                json: json.as_deref(),
                out: out.as_deref(),
                thumbs: thumbs.as_deref(),
                min_pass,
                compile_timeout: std::time::Duration::from_secs(compile_timeout),
            },
        ),
        Command::Render { .. } => return not_yet("render", "M2 (FR-H1)"),
        Command::Run { .. } => return not_yet("run", "M3 (FR-H2)"),
        Command::Bake { .. } => return not_yet("bake", "M2"),
        Command::Serve { .. } => return not_yet("serve", "M5 (FR-H3)"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn not_yet(command: &str, milestone: &str) -> ExitCode {
    eprintln!("`atidarla {command}` is not implemented yet; it is scheduled for {milestone}.");
    ExitCode::from(2)
}

/// The registry with every built-in operator family registered.
pub(crate) fn builtin_registry() -> atd_core::Registry {
    let mut registry = atd_core::Registry::default();
    atd_ops_img::register(&mut registry);
    atd_ops_sig::register(&mut registry);
    atd_ops_pnt::register(&mut registry);
    atd_ops_msh::register(&mut registry);
    atd_ops_mat::register(&mut registry);
    atd_ops_dat::register(&mut registry);
    atd_ops_cmp::register(&mut registry);
    atd_ops_ai::register(&mut registry);
    registry
}
