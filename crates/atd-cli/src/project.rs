//! `atidarla check` and `atidarla fmt`: project folders (PRD §5.5, A11).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use atd_core::graph::NodeId;
use atd_core::registry::{Availability, Registry};
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};

/// What `check` verifies beyond loading.
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions {
    /// Cook every node on a GPU and fail on node errors.
    pub gpu: bool,
    /// Frames to cook with `gpu`.
    pub frames: u32,
}

/// Loads each project, validates it against the registry (plus its own
/// shaders), checks the files are in canonical form, and optionally cooks
/// it. Reports every project before failing.
pub fn check(dirs: &[impl AsRef<Path>], opts: CheckOptions) -> Result<()> {
    let gpu = if opts.gpu {
        Some(atd_gpu::GpuContext::headless().context("no GPU adapter for --gpu")?)
    } else {
        None
    };
    let mut failed = 0;
    for dir in dirs {
        let dir = dir.as_ref();
        match check_one(dir, gpu.as_ref(), opts) {
            Ok(summary) => println!("ok    {}  {summary}", dir.display()),
            Err(e) => {
                failed += 1;
                println!("FAIL  {}  {e:#}", dir.display());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} projects failed", dirs.len());
    }
    Ok(())
}

fn library() -> (atd_core::Registry, OpLibrary) {
    let registry = crate::builtin_registry();
    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    atd_ops_sig::library(&mut library);
    (registry, library)
}

fn check_one(dir: &Path, gpu: Option<&atd_gpu::GpuContext>, opts: CheckOptions) -> Result<String> {
    let project = atd_format::load(dir)?;
    let (mut registry, mut library) = library();
    let (shaders, failed) =
        atd_ops_img::isf::register_project_shaders(dir, &mut registry, &mut library);
    if let Some((name, e)) = failed.first() {
        bail!("shaders/{name}: {e}");
    }
    for (id, net) in &project.networks {
        net.clone()
            .validated(&registry)
            .with_context(|| format!("networks/{id}.toml"))?;
    }
    // Saving what was loaded must reproduce the files byte for byte.
    let canonical = |rel: &str, text: String| -> Result<()> {
        let on_disk = std::fs::read_to_string(dir.join(rel))?;
        if on_disk != text {
            bail!("{rel} is not in canonical form (run `atidarla fmt`)");
        }
        Ok(())
    };
    canonical("project.toml", atd_format::project_to_string(&project))?;
    for (id, net) in &project.networks {
        canonical(
            &format!("networks/{id}.toml"),
            atd_format::network_to_string(net)?,
        )?;
    }
    let main = project.main().cloned().unwrap_or_default();
    let mut summary = format!("{} nodes", main.len());
    if !shaders.is_empty() {
        summary += &format!(", {} shaders", shaders.len());
    }
    let Some(gpu) = gpu else {
        return Ok(summary);
    };
    let [w, h] = project.resolution;
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
    engine.set_project_dir(Some(dir.to_owned()));
    engine.set_python(Box::new(atd_py::expr::PyExprHost::start()?));
    engine.load(main)?;
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
    for n in engine.network().nodes() {
        let Some(e) = engine.error(n.id) else {
            continue;
        };
        let fallback = engine.registry().get(&n.type_id).is_some_and(|s| {
            matches!(
                Registry::availability(s, &platform),
                Availability::Fallback { .. }
            )
        });
        if fallback {
            unavailable.push(n.name.as_str());
        } else {
            errors.push(format!("{}: {e}", n.name));
        }
    }
    if !unavailable.is_empty() {
        summary += &format!(
            ", not available on this platform: {}",
            unavailable.join(", ")
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
            let found = engine.channels(bind.node).is_some_and(|c| {
                if bind.channel.is_empty() {
                    !c.is_empty()
                } else {
                    c.index(&bind.channel).is_some()
                }
            });
            if !found {
                let source = net.node(bind.node).map_or("?", |n| n.name.as_str());
                let names: Vec<&str> = engine
                    .channels(bind.node)
                    .map(|c| c.names().collect())
                    .unwrap_or_default();
                missing.push(format!(
                    "{}.{param} is bound to {source}:{} (it has: {})",
                    node.name,
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

/// Rewrites each project in canonical form.
pub fn fmt(dirs: &[impl AsRef<Path>]) -> Result<()> {
    for dir in dirs {
        let dir = dir.as_ref();
        let project = atd_format::load(dir)?;
        atd_format::save(dir, &project)?;
        println!("formatted {}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::registry::OpFamily;

    fn examples() -> Vec<std::path::PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut found: Vec<_> = std::fs::read_dir(dir)
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
        };
        for dir in &all {
            check_one(dir, None, opts).unwrap_or_else(|e| panic!("{}: {e:#}", dir.display()));
        }
    }

    /// CLAUDE.md rule 7: every IMG and SIG operator has a runnable example.
    #[test]
    fn every_img_and_sig_operator_has_an_example() {
        let used = crate::docs::examples_by_type(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        )
        .unwrap();
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
}
