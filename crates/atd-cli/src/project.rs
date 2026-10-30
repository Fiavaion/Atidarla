//! `atidarla check` and `atidarla fmt`: project folders (PRD §5.5, A11) and
//! Pack folders (FR-P4, `docs/packs.md`).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use atd_core::graph::{Edit, Network, Node, NodeId};
use atd_core::pack::{Flat, PackError, PackSet};
use atd_core::registry::{Availability, Registry};
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_ops_pnt::project::ProjectKernels;

/// What `check` verifies beyond loading.
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions {
    /// Cook every node on a GPU and fail on node errors.
    pub gpu: bool,
    /// Frames to cook with `gpu`.
    pub frames: u32,
    /// Run the projects' Python operators (`ops/*.py`) when cooking.
    /// Without it they are reported as not run (`docs/python-ops.md`).
    pub allow_python_ops: bool,
}

/// Reads a project's `ops/` declarations (running nothing) and registers
/// them; the first declaration error is an error.
pub(crate) fn python_ops(
    dir: &Path,
    registry: &mut Registry,
    library: &mut OpLibrary,
) -> Result<atd_py::ops::PythonOps> {
    let mut ops = atd_py::ops::PythonOps::open(dir);
    if let Some(e) = ops.errors().first() {
        bail!("{e}");
    }
    ops.register(registry, library);
    Ok(ops)
}

/// Reads and compiles a project's `kernels/` files and registers their
/// operators; the first error in a file is an error.
pub(crate) fn kernel_ops(
    dir: &Path,
    registry: &mut Registry,
    library: &mut OpLibrary,
) -> Result<ProjectKernels> {
    let kernels = ProjectKernels::open(dir);
    if let Some(e) = kernels.errors().first() {
        bail!("{e}");
    }
    kernels.register(registry, library);
    Ok(kernels)
}

/// A network that does not validate, with unknown Python and kernel
/// operator types explained (a missing file, an operator not declared).
pub(crate) fn explain(
    e: atd_core::graph::GraphError,
    ops: &atd_py::ops::PythonOps,
    kernels: &ProjectKernels,
) -> anyhow::Error {
    if let atd_core::graph::GraphError::UnknownType(t) = &e
        && let Some(why) = ops.explain(t).or_else(|| kernels.explain(t))
    {
        return anyhow::anyhow!("{e}: {why}");
    }
    e.into()
}

/// Loads each project (or Pack folder), validates it against the registry
/// (plus its own shaders and Packs), checks the files are in canonical
/// form, and optionally cooks it. Reports every folder before failing.
pub fn check(dirs: &[impl AsRef<Path>], opts: CheckOptions) -> Result<()> {
    let gpu = if opts.gpu {
        Some(atd_gpu::GpuContext::headless().context("no GPU adapter for --gpu")?)
    } else {
        None
    };
    let mut failed = 0;
    for dir in dirs {
        let dir = dir.as_ref();
        let result = if is_pack_folder(dir) {
            check_pack(dir, gpu.as_ref(), opts)
        } else {
            check_one(dir, gpu.as_ref(), opts)
        };
        match result {
            Ok(summary) => println!("ok    {}  {summary}", dir.display()),
            Err(e) => {
                failed += 1;
                println!("FAIL  {}  {e:#}", dir.display());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} folders failed", dirs.len());
    }
    Ok(())
}

/// A Pack folder (`packs/glow/`) rather than a project.
fn is_pack_folder(dir: &Path) -> bool {
    dir.join("pack.toml").is_file() && !dir.join("project.toml").exists()
}

pub(crate) fn library() -> (atd_core::Registry, OpLibrary) {
    let registry = crate::builtin_registry();
    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    atd_ops_sig::library(&mut library);
    atd_ops_pnt::library(&mut library);
    atd_ops_mat::library(&mut library);
    atd_ops_msh::library(&mut library);
    (registry, library)
}

/// A Pack error, prefixed with the file it is about.
pub(crate) fn pack_error(e: PackError) -> anyhow::Error {
    match atd_format::pack::error_file(&e) {
        Some(file) => anyhow!("{file}: {e}"),
        None => anyhow!("packs: {e}"),
    }
}

/// A project loaded and checked against the registry.
pub(crate) struct Loaded {
    pub project: atd_format::Project,
    /// Built-in operators, the project's shaders and its Packs.
    pub registry: Registry,
    pub library: OpLibrary,
    pub shaders: Vec<String>,
    /// The project's Python operators: declared, not yet allowed to run.
    pub python: atd_py::ops::PythonOps,
    /// The project's kernel operators (`kernels/*.atk`).
    pub kernels: ProjectKernels,
}

/// Loads `dir`, registers its shaders and Packs, and validates every network.
pub(crate) fn load_checked(dir: &Path) -> Result<Loaded> {
    let project = atd_format::load(dir)?;
    let (mut registry, mut library) = library();
    let (shaders, failed) =
        atd_ops_img::isf::register_project_shaders(dir, &mut registry, &mut library);
    if let Some((name, e)) = failed.first() {
        bail!("shaders/{name}: {e}");
    }
    let python = python_ops(dir, &mut registry, &mut library)?;
    let kernels = kernel_ops(dir, &mut registry, &mut library)?;
    let registry = project.packs.register(&registry).map_err(pack_error)?;
    for (id, net) in &project.networks {
        net.clone()
            .validated(&registry)
            .map_err(|e| explain(e, &python, &kernels))
            .with_context(|| format!("networks/{id}.toml"))?;
    }
    Ok(Loaded {
        project,
        registry,
        library,
        shaders,
        python,
        kernels,
    })
}

/// Saving what was loaded must reproduce the file byte for byte.
fn canonical(dir: &Path, rel: &str, text: &str) -> Result<()> {
    let on_disk = std::fs::read_to_string(dir.join(rel)).with_context(|| rel.to_owned())?;
    if on_disk != text {
        bail!("{rel} is not in canonical form (run `atidarla fmt`)");
    }
    Ok(())
}

fn canonical_pack(dir: &Path, prefix: &str, def: &atd_core::pack::PackDef) -> Result<()> {
    for (file, text) in atd_format::pack::pack_files(def)? {
        canonical(dir, &format!("{prefix}{file}"), &text)?;
    }
    if def.help.is_empty() && dir.join(format!("{prefix}help.md")).exists() {
        bail!("{prefix}help.md is empty (run `atidarla fmt`)");
    }
    Ok(())
}

fn check_one(dir: &Path, gpu: Option<&atd_gpu::GpuContext>, opts: CheckOptions) -> Result<String> {
    let Loaded {
        project,
        registry,
        library,
        shaders,
        mut python,
        kernels,
    } = load_checked(dir)?;
    canonical(
        dir,
        "project.toml",
        &atd_format::project_to_string(&project),
    )?;
    for (id, net) in &project.networks {
        canonical(
            dir,
            &format!("networks/{id}.toml"),
            &atd_format::network_to_string(net)?,
        )?;
    }
    for def in project.packs.iter() {
        canonical_pack(dir, &format!("packs/{}/", def.name), def)?;
    }
    let main = project.main().cloned().unwrap_or_default();
    let mut summary = format!("{} nodes", main.len());
    if !shaders.is_empty() {
        summary += &format!(", {} shaders", shaders.len());
    }
    if !project.packs.is_empty() {
        let names: Vec<String> = project
            .packs
            .iter()
            .map(|d| d.reference().to_string())
            .collect();
        summary += &format!(", Packs {}", names.join(" "));
    }
    let py_count = python.ops().count();
    if py_count > 0 {
        summary += &format!(", {py_count} Python operators");
    }
    let kernel_count = kernels.schemas().count();
    if kernel_count > 0 {
        summary += &format!(", {kernel_count} kernel operators");
    }
    let Some(gpu) = gpu else {
        return Ok(summary);
    };
    if opts.allow_python_ops {
        python.allow();
    }
    let flat = project
        .packs
        .flatten(&main, &registry)
        .map_err(pack_error)?;
    let cooked = cook(
        gpu,
        registry,
        library,
        &flat,
        Some(dir),
        project.resolution,
        opts,
    )?;
    Ok(format!("{summary}{cooked}"))
}

/// Checks one Pack folder of a library: it loads, is valid (together with
/// the other Packs of its library, which it may contain) and canonical;
/// with a GPU, one instance of it cooks with nothing connected.
fn check_pack(dir: &Path, gpu: Option<&atd_gpu::GpuContext>, opts: CheckOptions) -> Result<String> {
    let rel = dir.display().to_string();
    let def = atd_format::load_pack(dir, rel.trim_end_matches(['/', '\\']))?;
    let mut packs = PackSet::default();
    if let Some(parent) = dir.parent()
        && let Ok(lib) = atd_format::Library::open(parent)
    {
        for d in lib.latest() {
            packs.insert(d.clone());
        }
    }
    packs.insert(def.clone());
    let (base, library) = library();
    let registry = packs.register(&base).map_err(|e| anyhow!("{e}"))?;
    canonical_pack(dir, "", &def)?;
    let summary = format!(
        "Pack {} ({} nodes, {} parameters)",
        def.reference(),
        def.network.len(),
        def.params.len()
    );
    let Some(gpu) = gpu else {
        return Ok(summary);
    };
    let mut net = Network::default();
    let id = net.alloc_id();
    net.apply(
        &registry,
        Edit::AddNode {
            node: Node::new(id, def.type_id(), format!("{}1", def.name)),
        },
    )?;
    let flat = packs.flatten(&net, &registry).map_err(|e| anyhow!("{e}"))?;
    let cooked = cook(gpu, registry, library, &flat, Some(dir), [320, 180], opts)?;
    Ok(format!("{summary}{cooked}"))
}

/// Cooks every node of `flat` for `opts.frames` frames in real time and
/// fails on node errors and on bindings to channels that do not exist.
fn cook(
    gpu: &atd_gpu::GpuContext,
    registry: Registry,
    library: OpLibrary,
    flat: &Flat,
    dir: Option<&Path>,
    [w, h]: [u32; 2],
    opts: CheckOptions,
) -> Result<String> {
    let mut engine = Engine::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        registry,
        library,
        EngineConfig {
            width: w,
            height: h,
            format: wgpu::TextureFormat::Rgba8Unorm,
        },
    );
    engine.set_project_dir(dir.map(Path::to_owned));
    engine.set_python(Box::new(atd_py::expr::PyExprHost::start()?));
    engine.load(flat.network.clone())?;
    let ids: Vec<NodeId> = engine.network().nodes().map(|n| n.id).collect();
    // Cook in real time, as the app would: movies, audio and shaders load
    // on worker threads while frames go by.
    let start = Instant::now();
    let frame_time = Duration::from_secs_f64(1.0 / 60.0);
    for frame in 0..opts.frames {
        let t = f64::from(frame) / 60.0;
        engine
            .cook(&ids, t)
            .with_context(|| format!("frame {frame}"))?;
        let due = frame_time * (frame + 1);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    // Give anything still loading a little longer. Some operators stay
    // pending for good (Analyze on a changing image always has a readback
    // in flight), so this is bounded.
    let mut t = f64::from(opts.frames) / 60.0;
    let settle = Instant::now();
    while engine.pending() && settle.elapsed() < Duration::from_secs(3) {
        engine.cook(&ids, t)?;
        t += 1.0 / 60.0;
        std::thread::sleep(frame_time);
    }
    let _ = engine.device().poll(wgpu::PollType::wait_indefinitely());
    // Operators this platform does not run (capability flags, PRD A19)
    // report their fallback as a node error; that is expected here.
    let platform = gpu.platform_info();
    let mut errors = Vec::new();
    let mut unavailable = Vec::new();
    let mut not_run = Vec::new();
    for n in engine.network().nodes() {
        let Some(e) = engine.error(n.id) else {
            continue;
        };
        // Python operators run only when allowed: not an error of the
        // project (`docs/python-ops.md`).
        if e.starts_with(atd_py::ops::NOT_ALLOWED) {
            not_run.push(n.name.as_str());
            continue;
        }
        let fallback = engine.registry().get(&n.type_id).is_some_and(|s| {
            matches!(
                Registry::availability(s, &platform),
                Availability::Fallback { .. }
            )
        });
        if fallback {
            unavailable.push(flat.display(n.id));
        } else {
            errors.push(format!("{}: {e}", flat.display(n.id)));
        }
    }
    let mut summary = String::new();
    if !unavailable.is_empty() {
        summary += &format!(
            ", not available on this platform: {}",
            unavailable.join(", ")
        );
    }
    if !not_run.is_empty() {
        summary += &format!(
            ", Python operators not run (pass --allow-python-ops to run the project's ops/ code): {}",
            not_run.join(", ")
        );
    }
    if !errors.is_empty() {
        bail!("node errors: {}", errors.join("; "));
    }
    // A binding to a channel the source does not have silently uses the
    // fallback value: almost always a typo.
    let net = engine.network();
    let mut missing = Vec::new();
    for node in net.nodes() {
        for (param, p) in &node.params {
            let Some(bind) = &p.bind else { continue };
            // A Python operator that did not run has no channels yet.
            if net
                .node(bind.node)
                .is_some_and(|s| not_run.contains(&s.name.as_str()))
            {
                continue;
            }
            let found = engine.channels(bind.node).is_some_and(|c| {
                if bind.channel.is_empty() {
                    !c.is_empty()
                } else {
                    c.index(&bind.channel).is_some()
                }
            });
            if !found {
                let source = flat.display(bind.node);
                let names: Vec<&str> = engine
                    .channels(bind.node)
                    .map(|c| c.names().collect())
                    .unwrap_or_default();
                missing.push(format!(
                    "{}.{param} is bound to {source}:{} (it has: {})",
                    flat.display(node.id),
                    bind.channel,
                    names.join(", ")
                ));
            }
        }
    }
    if !missing.is_empty() {
        bail!("bindings to missing channels: {}", missing.join("; "));
    }
    Ok(format!(
        "{summary}, {} frames cooked in {:.1} s",
        opts.frames,
        start.elapsed().as_secs_f64()
    ))
}

/// Rewrites each project (or Pack folder) in canonical form.
pub fn fmt(dirs: &[impl AsRef<Path>]) -> Result<()> {
    for dir in dirs {
        let dir = dir.as_ref();
        if is_pack_folder(dir) {
            let def = atd_format::load_pack(dir, &dir.display().to_string())?;
            atd_format::save_pack(dir, &def)?;
        } else {
            let project = atd_format::load(dir)?;
            atd_format::save(dir, &project)?;
        }
        println!("formatted {}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::registry::OpFamily;

    fn repo(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    fn examples() -> Vec<std::path::PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(repo("examples"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == atd_format::EXTENSION))
            .collect();
        found.sort();
        found
    }

    /// Every example loads, validates and is in canonical form (the GPU
    /// half runs as `atidarla check --gpu` in CI).
    #[test]
    fn examples_are_valid_and_canonical() {
        let all = examples();
        assert!(all.len() >= 10, "{all:?}");
        let opts = CheckOptions {
            gpu: false,
            frames: 0,
            allow_python_ops: false,
        };
        for dir in &all {
            check_one(dir, None, opts).unwrap_or_else(|e| panic!("{}: {e:#}", dir.display()));
        }
    }

    /// A copy of the Python operators example in the temp directory.
    fn python_example(tag: &str) -> std::path::PathBuf {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/python-ops.atidarla");
        let dir =
            std::env::temp_dir().join(format!("atd-check-{tag}-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["", "networks", "ops"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            for e in std::fs::read_dir(src.join(sub)).unwrap() {
                let p = e.unwrap().path();
                if p.is_file() {
                    std::fs::copy(&p, dir.join(sub).join(p.file_name().unwrap())).unwrap();
                }
            }
        }
        dir
    }

    /// `check` explains a missing Python file and a bad declaration.
    #[test]
    fn check_reports_missing_and_failed_python_operators() {
        let opts = CheckOptions {
            gpu: false,
            frames: 0,
            allow_python_ops: false,
        };
        let dir = python_example("missing");
        std::fs::remove_file(dir.join("ops/pattern.py")).unwrap();
        let e = format!("{:#}", check_one(&dir, None, opts).unwrap_err());
        assert!(
            e.contains("py.pattern.rings") && e.contains("ops/pattern.py does not exist"),
            "{e}"
        );
        std::fs::remove_dir_all(&dir).ok();

        let dir = python_example("bad");
        let path = dir.join("ops/motion.py");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("max=30.0", "max=rate_max")).unwrap();
        let e = format!("{:#}", check_one(&dir, None, opts).unwrap_err());
        assert!(e.starts_with("ops/motion.py, line 13:"), "{e}");
        assert!(e.contains("literals"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With a GPU, `check` cooks a project's Python operators only when
    /// allowed, and otherwise lists them without failing.
    #[test]
    fn check_runs_python_operators_only_when_allowed() {
        let Ok(gpu) = atd_gpu::GpuContext::headless() else {
            let required =
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return;
        };
        let dir = python_example("gate");
        let mut opts = CheckOptions {
            gpu: true,
            frames: 20,
            allow_python_ops: false,
        };
        let summary = check_one(&dir, Some(&gpu), opts).unwrap();
        assert!(
            summary.contains("Python operators not run") && summary.contains("rings1"),
            "{summary}"
        );
        opts.allow_python_ops = true;
        let summary = check_one(&dir, Some(&gpu), opts).unwrap();
        assert!(!summary.contains("not run"), "{summary}");
        // A cook error in Python fails the check, with its line.
        let path = dir.join("ops/pattern.py");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("n = ctx.par.rings", "n = ctx.par.rings / 0"),
        )
        .unwrap();
        let e = format!("{:#}", check_one(&dir, Some(&gpu), opts).unwrap_err());
        assert!(
            e.contains("ZeroDivisionError") && e.contains("ops/pattern.py, line"),
            "{e}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// CLAUDE.md rule 7: every IMG and SIG operator has a runnable example.
    #[test]
    fn every_img_and_sig_operator_has_an_example() {
        let used = crate::docs::examples_by_type(&repo("examples")).unwrap();
        let registry = crate::builtin_registry();
        let missing: Vec<&str> = registry
            .iter()
            .filter(|s| matches!(s.family, OpFamily::Img | OpFamily::Sig))
            .map(|s| s.type_id.as_str())
            .filter(|t| !used.contains_key(*t))
            .collect();
        assert!(
            missing.is_empty(),
            "operators without an example: {missing:?}"
        );
    }

    /// Every shipped Pack is valid and canonical, has help, a summary,
    /// promoted parameters and a version, and an example project that uses
    /// the library's current version of it.
    #[test]
    fn shipped_packs_are_complete_and_their_examples_current() {
        let lib = atd_format::Library::open(&repo("packs")).unwrap();
        // PRD §9 M2: a Palette of at least 15 Packs.
        assert!(lib.latest().count() >= 15, "{} Packs", lib.latest().count());
        let opts = CheckOptions {
            gpu: false,
            frames: 0,
            allow_python_ops: false,
        };
        let projects: Vec<atd_format::Project> = examples()
            .iter()
            .map(|d| atd_format::load(d).unwrap())
            .collect();
        for def in lib.latest() {
            let dir = lib.path(&def.name, def.version).unwrap();
            check_pack(dir, None, opts).unwrap_or_else(|e| panic!("{}: {e:#}", def.name));
            assert!(def.locked, "{}: shipped Packs are locked", def.name);
            assert!(
                def.help.starts_with("# "),
                "{}: help.md starts with a title",
                def.name
            );
            assert!(
                !def.summary.is_empty() && !def.label.is_empty(),
                "{}",
                def.name
            );
            assert!(
                !def.params.is_empty(),
                "{}: no promoted parameters",
                def.name
            );
            let uses: Vec<&atd_format::Project> = projects
                .iter()
                .filter(|p| p.packs.get(&def.name).is_some())
                .collect();
            assert!(!uses.is_empty(), "{}: no example project uses it", def.name);
            for p in uses {
                assert_eq!(
                    p.packs.get(&def.name),
                    Some(def),
                    "{} in example `{}` differs from packs/{} (copy the library version)",
                    def.name,
                    p.name,
                    def.name
                );
                let main = p.main().unwrap();
                assert!(
                    main.nodes().any(|n| n.type_id == def.type_id()),
                    "{}: example `{}` has no instance",
                    def.name,
                    p.name
                );
            }
        }
    }

    /// `check` and `fmt` report Pack errors with the file they are in.
    #[test]
    fn pack_errors_name_their_file() {
        let src = repo("examples/pack-feedback-trails.atidarla");
        let dir =
            std::env::temp_dir().join(format!("atd-cli-packs-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = atd_format::load(&src).unwrap();
        atd_format::save(&dir, &project).unwrap();
        let opts = CheckOptions {
            gpu: false,
            frames: 0,
            allow_python_ops: false,
        };
        check_one(&dir, None, opts).unwrap();
        let pack = dir.join("packs/feedback_trails/pack.toml");
        let text = std::fs::read_to_string(&pack).unwrap();
        std::fs::write(&pack, text.replacen("param = \"", "param = \"nope_", 1)).unwrap();
        let e = format!("{:#}", check_one(&dir, None, opts).unwrap_err());
        assert!(
            e.starts_with("packs/feedback_trails/pack.toml: Pack `feedback_trails`: parameter"),
            "{e}"
        );
        std::fs::write(&pack, text.replace("version = \"", "version = \"x")).unwrap();
        let e = format!("{:#}", check_one(&dir, None, opts).unwrap_err());
        assert!(
            e.starts_with("packs/feedback_trails/pack.toml: invalid version"),
            "{e}"
        );
        std::fs::write(&pack, text.replace("\nlocked", "\n\nlocked")).unwrap();
        let e = format!("{:#}", check_one(&dir, None, opts).unwrap_err());
        assert!(
            e.contains("packs/feedback_trails/pack.toml is not in canonical form"),
            "{e}"
        );
        fmt(&[&dir]).unwrap();
        check_one(&dir, None, opts).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
