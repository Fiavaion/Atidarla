//! `atidarla` — headless entry points (PRD §7.10).

mod bench;
mod docs;
mod export;
mod isf_import;
mod kernel;
mod models;
mod project;
mod pt8;
mod render;

/// The mock ComfyUI server of `atd-ops-ai`'s tests, for the render test.
#[cfg(test)]
#[path = "../../atd-ops-ai/tests/mock_comfy/mod.rs"]
mod mock_comfy;

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
    /// Deterministic offline render of a project to a numbered PNG sequence
    /// (FR-H1).
    ///
    /// Frame i is cooked at i/fps seconds, every frame one fixed step after
    /// the last, with no wall clock: operators that work on worker threads
    /// (movies, audio files, shader compiles, text, Python expressions and
    /// operators, Analyze) are waited for, live inputs (audio, MIDI, OSC, input
    /// devices, timecode, Share In) output their idle value and are named
    /// in a warning, and sinks (OSC, DMX, MIDI and audio out, Share Out)
    /// do not emit. Two renders on the same machine (same GPU adapter and
    /// backend) give byte-identical frames; other machines may differ.
    ///
    /// Writes <OUT>/frame_000000.png, frame_000001.png, ... (numbered by
    /// frame), or, when built with FFmpeg (`--features atd-io/ffmpeg`), a
    /// movie: --out clip.mov (ProRes) or clip.mp4 (H.264), see --codec.
    /// Only LGPL encoders are used. See docs/render.md.
    Render {
        /// Project folder (`*.atidarla`).
        project: PathBuf,
        /// Folder to write the PNG frames into (created if missing), or a
        /// .mov / .mp4 file.
        #[arg(long)]
        out: PathBuf,
        /// Frames START-END: START inclusive, END exclusive (0-300 is
        /// frames 0 to 299, five seconds at 60 fps).
        #[arg(long, default_value = "0-60", value_parser = render::parse_range)]
        range: std::ops::Range<u64>,
        /// Frames per second: frame i is at i/fps seconds. Project files
        /// have no frame rate yet.
        #[arg(long, default_value_t = render::DEFAULT_FPS, value_parser = render::parse_fps)]
        fps: f64,
        /// Fixed time step. Always on: accepted for the PRD's command line.
        #[arg(long)]
        fixed_step: bool,
        /// The node to render, by name; default: the output node the app's
        /// viewer shows first (the last IMG node nothing reads from).
        #[arg(long)]
        node: Option<String>,
        /// Run the project's Python operators (ops/*.py). They are code
        /// with your user's privileges; without this flag a render that
        /// needs one fails. See docs/python-ops.md.
        #[arg(long)]
        allow_python_ops: bool,
        /// Movie codec for --out *.mov / *.mp4: prores (422, default for
        /// .mov), prores-hq, prores4444 (with alpha), h264 (default for
        /// .mp4), h265. H.264/H.265 use the platform's hardware encoder.
        #[arg(long)]
        codec: Option<String>,
        /// H.264/H.265 bit rate in bit/s (default about 0.15 bit per pixel
        /// per frame).
        #[arg(long)]
        bitrate: Option<u64>,
    },
    /// Export a project as one portable show bundle (FR-S9): the project
    /// folder with every file its nodes use from outside it copied into
    /// media/, parameters rewritten as relative paths, and bundle.toml
    /// listing every file's SHA-256 and the platform capabilities nodes
    /// need. The copy is verified as `verify-show` does. See
    /// docs/show-export.md.
    ExportShow {
        /// Project folder (`*.atidarla`).
        project: PathBuf,
        /// Folder to write the bundle into, as <OUT>/<project folder name>.
        out: PathBuf,
        /// Also write <OUT>/<project folder name>.zip.
        #[arg(long)]
        zip: bool,
        /// Verify without a GPU (structure, paths and hashes only).
        #[arg(long)]
        no_gpu: bool,
    },
    /// Verify a show bundle on this machine: path parameters are relative
    /// and exist with their exact case, every file matches its SHA-256 in
    /// bundle.toml, and `check` passes (with --gpu, every node cooks).
    VerifyShow {
        /// The bundle folder.
        bundle: PathBuf,
        /// Also cook every node on a GPU.
        #[arg(long)]
        gpu: bool,
    },
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
        /// With --gpu, also run the projects' Python operators (ops/*.py).
        /// They are code with your user's privileges; without this flag
        /// their nodes are listed as not run. See docs/python-ops.md.
        #[arg(long)]
        allow_python_ops: bool,
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
    /// ComfyUI workflows as project operators (FR-A10). See docs/comfy.md.
    Comfy {
        #[command(subcommand)]
        command: ComfyCommand,
    },
    /// Compile a Kernel DSL file (experimental, PRD A12) and print its errors,
    /// bindings and parameters, or the generated WGSL. See docs/kernel-dsl.md.
    Kernel {
        /// The kernel source (`*.atk`).
        file: PathBuf,
        /// `points` or `pixels`; default: pixels for `*.pixels.atk`, else points.
        #[arg(long)]
        target: Option<String>,
        /// Print the generated WGSL instead of the summary.
        #[arg(long)]
        emit_wgsl: bool,
    },
    /// The AI model manager: catalogue, licence acceptance, downloads and
    /// checks (FR-A12, docs/models.md).
    Models {
        #[command(subcommand)]
        action: models::ModelsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ComfyCommand {
    /// Import an API-format workflow (ComfyUI: Workflow > Export (API)) into
    /// a project as operator comfy.<name>: reads the server's object_info
    /// and writes comfy/<name>.json and comfy/<name>.info.json, so the
    /// project opens without the server. The server must be on this machine
    /// or the LAN (Sovereign mode).
    Import {
        /// The workflow file (`*.json`, API format).
        workflow: PathBuf,
        /// Project folder (`*.atidarla`).
        #[arg(long)]
        project: PathBuf,
        /// The ComfyUI server, host:port.
        #[arg(long, default_value = atd_ops_ai::comfy::DEFAULT_SERVER)]
        server: String,
        /// Operator name (lower-case letters, digits, _); default: from the
        /// file name.
        #[arg(long)]
        name: Option<String>,
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
            allow_python_ops,
        } => project::check(
            &projects,
            project::CheckOptions {
                gpu,
                frames,
                allow_python_ops,
            },
        ),
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
        Command::Render {
            project,
            out,
            range,
            fps,
            fixed_step: _,
            node,
            allow_python_ops,
            codec,
            bitrate,
        } => render::run(
            &project,
            &out,
            &render::RenderOptions {
                range,
                fps,
                node,
                size: None,
                allow_python_ops,
            },
            &render::MovieOptions { codec, bitrate },
        ),
        Command::Kernel {
            file,
            target,
            emit_wgsl,
        } => kernel::run(&file, target.as_deref(), emit_wgsl),
        Command::Comfy {
            command:
                ComfyCommand::Import {
                    workflow,
                    project,
                    server,
                    name,
                },
        } => atd_ops_ai::comfy::import(&workflow, &project, &server, name.as_deref())
            .map(|i| {
                println!(
                    "imported {}: wrote {} and {}; {} parameters, {} image inputs{}",
                    i.type_id,
                    i.files[0],
                    i.files[1],
                    i.params,
                    i.images,
                    if i.has_output {
                        ""
                    } else {
                        " (no SaveImage or PreviewImage node: the output stays empty)"
                    }
                );
            })
            .map_err(anyhow::Error::msg),
        Command::ExportShow {
            project,
            out,
            zip,
            no_gpu,
        } => export::run(&project, &out, export::ExportOptions { zip, gpu: !no_gpu }),
        Command::VerifyShow { bundle, gpu } => export::verify(&bundle, gpu),
        Command::Models { action } => models::run(action),
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
