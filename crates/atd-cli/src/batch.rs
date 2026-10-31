//! `atidarla batch`: runs a project's wedges (FR-B1–B3, `docs/batch.md`).
//!
//! A wedge (`batch/<name>.toml`) becomes work items: the main network with
//! the item's parameter values set, cooked offline for the wedge's target
//! node. Each item's cache key is computed here (it needs the registry and
//! the project's Packs); `atd_ai::batch` skips cached items, picks hosts
//! and runs the rest. Every worker is an offline [`Renderer`] on the shared
//! GPU device whose AI nodes are pinned to the worker's host (their
//! `route`, or a ComfyUI node's `server`). The batch has its own
//! supervisor, so its hosts are not the app's.
//!
//! AI nodes answer one frame behind offline (`docs/render.md`), so an item
//! cooks frame F and keeps frame F + 1: the AI result for frame F's input.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use atd_ai::batch::{Cooker, Event, Image, Item, Override, Report, RunOptions};
use atd_ai::supervisor::{HostKind, HostSpec, Supervisor};
use atd_core::graph::{Edit, Network, Param};
use atd_core::registry::Registry;
use atd_format::batch::{Wedge, comfy_addr};
use clap::Subcommand;

use crate::render::{RenderOptions, Renderer};

/// How long AI Hosts get to become ready before items are handed out.
const HOST_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    /// Run a wedge: cook every work item not already in the media bin and
    /// add it to media/bin (PNG + index.toml). Items whose inputs did not
    /// change since the last run are not cooked again.
    Run {
        /// Project folder (`*.atidarla`).
        project: std::path::PathBuf,
        /// The wedge: batch/<WEDGE>.toml.
        wedge: String,
        /// Items in flight per host (default: the wedge's `concurrency`).
        #[arg(long)]
        jobs: Option<u32>,
        /// Cook every item, even those already in the bin.
        #[arg(long)]
        force: bool,
        /// Run the project's Python (ops/*.py operators, Script nodes, Python
        /// expressions); see docs/python-ops.md.
        #[arg(long)]
        allow_python_ops: bool,
    },
    /// Show each wedge's items, how many are in the media bin and where
    /// they would run.
    Status {
        /// Project folder (`*.atidarla`).
        project: std::path::PathBuf,
        /// Only this wedge.
        wedge: Option<String>,
    },
}

pub fn run(cmd: BatchCommand) -> Result<()> {
    match cmd {
        BatchCommand::Run {
            project,
            wedge,
            jobs,
            force,
            allow_python_ops,
        } => {
            let plan = plan(&project, &wedge)?;
            let report = execute(
                &project,
                &plan,
                &ExecOptions {
                    jobs,
                    force,
                    allow_python_ops,
                    supervisor: None,
                },
                &print_event(plan.items.len()),
            )?;
            println!("{}", summary(&report));
            if !report.failed.is_empty() {
                bail!("{} of {} items failed", report.failed.len(), report.total);
            }
            Ok(())
        }
        BatchCommand::Status { project, wedge } => status(&project, wedge.as_deref()),
    }
}

/// A wedge turned into work items.
#[derive(Debug)]
pub struct Plan {
    pub name: String,
    pub wedge: Wedge,
    pub project: atd_format::Project,
    pub items: Vec<Item>,
    /// Adapters the target's AI nodes need (empty: none).
    pub adapters: BTreeSet<String>,
    /// The target depends on ComfyUI workflow nodes.
    pub comfy: bool,
    pub size: [u32; 2],
    pub target_name: String,
}

/// Sets each override as a constant (replacing any expression or binding).
fn apply_overrides(net: &mut Network, registry: &Registry, overrides: &[Override]) -> Result<()> {
    for o in overrides {
        net.apply(
            registry,
            Edit::SetParam {
                node: o.node,
                param: o.param.clone(),
                value: Some(Param::constant(o.value.clone())),
            },
        )
        .map_err(|e| anyhow!("{}.{}: {e}", o.node, o.param))?;
    }
    Ok(())
}

/// Pins the network's AI nodes to `host` (their `route`), or its ComfyUI
/// nodes to the server of a `comfy:<addr>` host.
fn place(net: &mut Network, registry: &Registry, host: &str) -> Result<()> {
    let (param, value, wanted): (&str, &str, fn(&str) -> bool) = match comfy_addr(host) {
        Some(addr) => ("server", addr, |t| t.starts_with("comfy.")),
        None => ("route", host, |t| atd_ops_ai::schemas::kind(t).is_some()),
    };
    let ids: Vec<_> = net
        .nodes()
        .filter(|n| wanted(&n.type_id))
        .map(|n| n.id)
        .collect();
    for id in ids {
        net.apply(
            registry,
            Edit::SetParam {
                node: id,
                param: param.into(),
                value: Some(Param::constant(serde_json::json!(value))),
            },
        )
        .map_err(|e| anyhow!("placing {id} on {host}: {e}"))?;
    }
    Ok(())
}

/// The adapter an AI node needs, as the node asks for it.
fn adapter(net: &Network, registry: &Registry, node: &atd_core::graph::Node) -> Option<String> {
    use atd_ops_ai::schemas::Kind;
    Some(match atd_ops_ai::schemas::kind(&node.type_id)? {
        Kind::Diffusion => "diffusion.sd".into(),
        Kind::Video => "video.v2v".into(),
        Kind::Vision(_) => "onnx".into(),
        Kind::Model => net
            .param_value(registry, node.id, "adapter")
            .and_then(|v| v.as_str())
            .unwrap_or("builtin")
            .to_owned(),
    })
}

/// Reads wedge `name` of the project at `dir` and makes its work items,
/// each with its cache key.
pub fn plan(dir: &Path, name: &str) -> Result<Plan> {
    let loaded = crate::project::load_checked(dir)?;
    let wedge = atd_format::batch::load_wedge(dir, name)?;
    let file = format!("batch/{name}.toml");
    wedge.check().map_err(|e| anyhow!("{file}: {e}"))?;
    let project = loaded.project;
    let registry = loaded.registry;
    let main = project.main().cloned().unwrap_or_default();
    let target = main.node(wedge.target).with_context(|| {
        format!(
            "{file}: target {} is not in networks/main.toml",
            wedge.target
        )
    })?;
    let target_name = target.name.clone();
    let mut names = BTreeSet::new();
    let mut attrs = Vec::new();
    for (i, a) in wedge.axes.iter().enumerate() {
        let at = format!("{file}: axis {}", i + 1);
        let node = main
            .node(a.node)
            .with_context(|| format!("{at}: node {} is not in networks/main.toml", a.node))?;
        let has = registry
            .get(&node.type_id)
            .is_some_and(|s| s.params.iter().any(|p| p.name == a.param));
        if !has {
            bail!(
                "{at}: {} ({}) has no parameter `{}`",
                node.name,
                node.type_id,
                a.param
            );
        }
        let attr = a
            .name
            .clone()
            .unwrap_or_else(|| format!("{}.{}", node.name, a.param));
        if !names.insert(attr.clone()) {
            bail!("{at}: attribute `{attr}` is used twice (give the axis a `name`)");
        }
        attrs.push(attr);
    }
    let size = wedge.size.unwrap_or(project.resolution);
    let models: BTreeMap<String, String> = project
        .models
        .iter()
        .map(|(id, pin)| (id.clone(), pin.sha256.clone()))
        .collect();
    let mut adapters = BTreeSet::new();
    let mut comfy = false;
    let mut items = Vec::with_capacity(wedge.len());
    for (index, values) in wedge.items().into_iter().enumerate() {
        let overrides: Vec<Override> = wedge
            .axes
            .iter()
            .zip(&values)
            .map(|(a, v)| Override {
                node: a.node,
                param: a.param.clone(),
                value: v.clone(),
            })
            .collect();
        let mut net = main.clone();
        apply_overrides(&mut net, &registry, &overrides)
            .with_context(|| format!("{file}: item {index}"))?;
        let flat = project
            .packs
            .flatten(&net, &registry)
            .map_err(crate::project::pack_error)?;
        let flat_target = flat
            .view(wedge.target)
            .with_context(|| format!("{file}: {target_name} has no image output"))?;
        for id in atd_ai::batch::key::upstream(&flat.network, flat_target) {
            let n = flat.network.node(id).expect("upstream node");
            comfy |= n.type_id.starts_with("comfy.");
            if let Some(a) = adapter(&flat.network, &registry, n) {
                adapters.insert(a);
            }
        }
        let key = atd_ai::batch::cache_key(&atd_ai::batch::KeyInput {
            net: &flat.network,
            registry: &registry,
            target: flat_target,
            frame: wedge.frame,
            fps: wedge.fps,
            size,
            models: &models,
        });
        items.push(Item {
            index,
            key,
            attributes: attrs.iter().cloned().zip(values).collect(),
            overrides,
        });
    }
    Ok(Plan {
        name: name.to_owned(),
        wedge,
        project,
        items,
        adapters,
        comfy,
        size,
        target_name,
    })
}

/// A supervisor for one batch run: the local host, Decart and the
/// project's hosts with the user's tokens.
fn batch_supervisor(project: &atd_format::Project) -> &'static Supervisor {
    let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
    sup.set(HostSpec {
        name: "local".into(),
        kind: HostKind::Local(atd_ai::supervisor::default_local_command()),
    });
    atd_ai::cloud::register_decart(sup, 72.0);
    let tokens = atd_ai::hosts::HostTokens::load(&atd_ai::models::Store::default().hosts_path())
        .unwrap_or_else(|e| {
            eprintln!("warning: AI host tokens: {e}");
            Default::default()
        });
    atd_ai::hosts::apply(&project.ai, &tokens, sup);
    atd_ai::router::spend().reset();
    sup
}

/// One worker: an offline engine with the project loaded.
struct EngineCooker {
    renderer: Renderer,
    host: String,
    frame: u64,
}

impl Cooker for EngineCooker {
    fn cook(&mut self, item: &Item) -> Result<Image, String> {
        let host = &self.host;
        let mut go = || -> Result<Image> {
            self.renderer.load_variant(
                |net, reg| apply_overrides(net, reg, &item.overrides),
                |net, reg| place(net, reg, host),
            )?;
            self.renderer.frame(self.frame)?;
            let f = self.renderer.frame(self.frame + 1)?;
            // Warnings repeat per item; `atidarla render` shows them.
            self.renderer.take_warnings();
            Ok(Image {
                width: f.width,
                height: f.height,
                rgba: f.rgba,
            })
        };
        go().map_err(|e| format!("{e:#}"))
    }
}

/// How [`execute`] runs a plan.
#[derive(Debug, Default)]
pub struct ExecOptions {
    pub jobs: Option<u32>,
    pub force: bool,
    pub allow_python_ops: bool,
    /// Hosts to use instead of a supervisor made from the project (tests).
    pub supervisor: Option<&'static Supervisor>,
}

/// Runs a plan: picks hosts, cooks what is not cached, fills the bin.
pub fn execute(
    dir: &Path,
    plan: &Plan,
    opts: &ExecOptions,
    on: &(dyn Fn(Event<'_>) + Sync),
) -> Result<Report> {
    let own = opts.supervisor.is_none();
    let sup = opts
        .supervisor
        .unwrap_or_else(|| batch_supervisor(&plan.project));
    let result = execute_on(dir, plan, opts, sup, on);
    if own {
        sup.shutdown();
    }
    result
}

fn execute_on(
    dir: &Path,
    plan: &Plan,
    opts: &ExecOptions,
    sup: &'static Supervisor,
    on: &(dyn Fn(Event<'_>) + Sync),
) -> Result<Report> {
    let picked = atd_ai::batch::pick_hosts(
        &plan.wedge.scheduler,
        sup,
        &plan.adapters,
        plan.comfy,
        HOST_WAIT,
    )
    .map_err(|e| anyhow!("batch/{}.toml: {e}", plan.name))?;
    for (host, why) in &picked.skipped {
        eprintln!("warning: {host} takes no items: {why}");
    }
    let gpu = atd_gpu::GpuContext::headless().context("no GPU adapter for the batch")?;
    let platform = gpu.platform_info();
    let render = RenderOptions {
        range: plan.wedge.frame..plan.wedge.frame + 2,
        fps: plan.wedge.fps,
        node: Some(plan.target_name.clone()),
        size: Some(plan.size),
        allow_python_ops: opts.allow_python_ops,
    };
    let make = |host: &str| -> Result<EngineCooker, String> {
        let mut renderer =
            Renderer::with_supervisor(gpu.device.clone(), gpu.queue.clone(), platform.clone(), sup)
                .map_err(|e| format!("{e:#}"))?;
        renderer.load(dir, &render).map_err(|e| format!("{e:#}"))?;
        Ok(EngineCooker {
            renderer,
            host: host.to_owned(),
            frame: plan.wedge.frame,
        })
    };
    let run = RunOptions {
        wedge: plan.name.clone(),
        hosts: picked.hosts,
        per_host: opts.jobs.unwrap_or(plan.wedge.concurrency).max(1),
        force: opts.force,
        attempts: 3,
    };
    atd_ai::batch::run(dir, plan.items.clone(), &run, make, on).map_err(anyhow::Error::msg)
}

fn print_event(total: usize) -> impl Fn(Event<'_>) + Sync {
    let done = std::sync::atomic::AtomicUsize::new(0);
    move |e| {
        let n = || done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        match e {
            Event::Cooked { index, host, ms } => {
                println!(
                    "[{:>5}/{total}] item {index} cooked on {host} in {ms:.0} ms",
                    n()
                );
            }
            Event::Failed { index, error } => {
                println!("[{:>5}/{total}] item {index} FAILED: {error}", n());
            }
            Event::Retry { index, host, error } => {
                eprintln!("warning: item {index} failed on {host}, trying again: {error}");
            }
            Event::WorkerStopped { host, error } => {
                eprintln!("warning: a worker on {host} stopped: {error}");
            }
            Event::Cached { .. } => {
                n();
            }
        }
    }
}

fn summary(r: &Report) -> String {
    let hosts: Vec<String> = r.per_host.iter().map(|(h, n)| format!("{h} {n}")).collect();
    format!(
        "{} items: {} cooked ({}), {} already in the bin, {} failed, in {:.1} s",
        r.total,
        r.cooked,
        hosts.join(", "),
        r.cached,
        r.failed.len(),
        r.wall.as_secs_f64()
    )
}

/// `atidarla batch status`.
fn status(dir: &Path, only: Option<&str>) -> Result<()> {
    let names = match only {
        Some(n) => vec![n.to_owned()],
        None => atd_format::batch::list_wedges(dir)?,
    };
    let bin = atd_format::media_bin::load_bin(dir)?;
    if names.is_empty() {
        println!("no wedges (batch/*.toml)");
    }
    for name in &names {
        let plan = plan(dir, name)?;
        let have = plan
            .items
            .iter()
            .filter(|i| {
                atd_ai::batch::run::image_path(dir, &i.key).is_file() && bin.contains(name, &i.key)
            })
            .count();
        let scheduler = match &plan.wedge.scheduler {
            atd_format::batch::Scheduler::Lan => "lan:*".to_owned(),
            atd_format::batch::Scheduler::Hosts(h) => h.join(", "),
        };
        println!(
            "{name}: {} items of {} at frame {} ({}×{}), {have} in the bin, {} to cook · scheduler {scheduler} · AI adapters: {}",
            plan.items.len(),
            plan.target_name,
            plan.wedge.frame,
            plan.size[0],
            plan.size[1],
            plan.items.len() - have,
            if plan.adapters.is_empty() {
                "none".to_owned()
            } else {
                plan.adapters.iter().cloned().collect::<Vec<_>>().join(", ")
            },
        );
    }
    let missing = bin
        .entries()
        .iter()
        .filter(|e| !atd_ai::batch::run::image_path(dir, &e.hash).is_file())
        .count();
    println!(
        "media bin: {} entries{}",
        bin.len(),
        if missing > 0 {
            format!(", {missing} without their image")
        } else {
            String::new()
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    //! M5 AC4: a 100-item seed/prompt wedge across two LAN hosts lands in
    //! the media bin; a re-run cooks nothing; changing one prompt recooks
    //! exactly its items.

    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;

    use atd_ai::refhost::{RefHostOptions, serve};
    use atd_core::graph::{Endpoint, Node, NodeId, Wire};
    use atd_format::batch::{Axis, AxisValues, Scheduler};
    use serde_json::json;

    /// A reference AI Host on loopback; its address.
    fn refhost() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let opts = RefHostOptions {
            // A little GPU time, so the hosts take turns.
            delay: Duration::from_millis(5),
            ..Default::default()
        };
        std::thread::spawn(move || serve(l, opts));
        addr
    }

    fn has_gpu() -> bool {
        if atd_gpu::GpuContext::headless().is_ok() {
            return true;
        }
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        false
    }

    /// The colour `builtin.prompts` paints for one prompt (FNV-1a of its text).
    fn prompt_colour(text: &str) -> [u8; 3] {
        let mut h: u32 = 0x811c_9dc5;
        for b in text.bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        [h as u8, (h >> 8) as u8, (h >> 16) as u8]
    }

    const TARGET: NodeId = NodeId(2);

    /// constant → Diffusion (`builtin.prompts`), hosts lan:a and lan:b.
    fn project(dir: &Path, hosts: [&str; 2]) {
        let reg = crate::builtin_registry();
        let mut net = Network::default();
        for (id, ty, name) in [
            (1, "img.constant", "src"),
            (2, "ai.diffusion", "diffusion1"),
        ] {
            net.apply(
                &reg,
                Edit::AddNode {
                    node: Node::new(NodeId(id), ty, name),
                },
            )
            .unwrap();
        }
        for (p, v) in [
            ("model", json!("builtin.prompts")),
            ("width", json!(16)),
            ("height", json!(16)),
            ("policy", json!("hold")),
            ("effect", json!(0.0)),
        ] {
            apply_overrides(
                &mut net,
                &reg,
                &[Override {
                    node: TARGET,
                    param: p.into(),
                    value: v,
                }],
            )
            .unwrap();
        }
        net.apply(
            &reg,
            Edit::Connect {
                wire: Wire::new(
                    Endpoint::new(NodeId(1), "out"),
                    Endpoint::new(TARGET, "image"),
                ),
            },
        )
        .unwrap();
        let mut p = atd_format::Project::with_main("batch test", net);
        p.resolution = [24, 24];
        p.ai.hosts.insert("lan:a".into(), hosts[0].into());
        p.ai.hosts.insert("lan:b".into(), hosts[1].into());
        atd_format::save(dir, &p).unwrap();
    }

    fn prompts() -> Vec<String> {
        (0..10).map(|i| format!("prompt number {i}")).collect()
    }

    fn wedge(prompts: &[String]) -> Wedge {
        let mut w = Wedge::new(TARGET);
        w.size = Some([32, 16]);
        w.scheduler = Scheduler::Lan;
        w.concurrency = 2;
        w.axes = vec![
            Axis {
                node: TARGET,
                param: "prompt1".into(),
                name: Some("prompt".into()),
                values: AxisValues::List(prompts.iter().map(|p| json!(p)).collect()),
            },
            Axis {
                node: TARGET,
                param: "seed".into(),
                name: None,
                values: AxisValues::Seeds { from: 1, count: 10 },
            },
        ];
        w
    }

    fn exec(dir: &Path, cooked: &Mutex<Vec<usize>>) -> Report {
        let plan = plan(dir, "seeds").unwrap();
        cooked.lock().unwrap().clear();
        let on = |e: Event<'_>| {
            if let Event::Cooked { index, .. } = e {
                cooked.lock().unwrap().push(index);
            }
        };
        execute(dir, &plan, &ExecOptions::default(), &on).unwrap_or_else(|e| panic!("{e:#}"))
    }

    #[test]
    fn a_100_item_wedge_runs_across_two_lan_hosts_into_the_media_bin() {
        if !has_gpu() {
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("atd-batch-ac4-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        project(&dir, [&refhost(), &refhost()]);
        let mut ps = prompts();
        atd_format::batch::save_wedge(&dir, "seeds", &wedge(&ps)).unwrap();
        let cooked = Mutex::new(Vec::new());

        let r = exec(&dir, &cooked);
        eprintln!(
            "AC4 first run: {} cooked, per host {:?}, {:.2} s",
            r.cooked,
            r.per_host,
            r.wall.as_secs_f64()
        );
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!((r.total, r.cooked, r.cached), (100, 100, 0));
        assert_eq!(r.per_host.keys().collect::<Vec<_>>(), ["lan:a", "lan:b"]);
        for (h, n) in &r.per_host {
            assert!(*n >= 25, "{h} took only {n} of 100: {:?}", r.per_host);
        }
        let bin = atd_format::media_bin::load_bin(&dir).unwrap();
        assert_eq!(bin.len(), 100);
        let pngs = std::fs::read_dir(dir.join("media/bin"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "png")
            })
            .count();
        assert_eq!(pngs, 100);
        // Every entry: its wedge, a host, its attributes; pixels on a few.
        for e in bin.entries() {
            assert_eq!(e.wedge, "seeds");
            assert!(e.host == "lan:a" || e.host == "lan:b", "{}", e.host);
            assert_eq!(e.size, [32, 16]);
            let i = e.index as usize;
            assert_eq!(e.attributes["prompt"], json!(ps[i / 10]));
            assert_eq!(e.attributes["diffusion1.seed"], json!(1 + (i % 10) as i64));
        }
        for e in bin.entries().iter().step_by(17) {
            let img = atd_io::image::load(&dir.join(e.image())).unwrap();
            assert_eq!((img.width, img.height), (32, 16));
            let want = prompt_colour(&ps[e.index as usize / 10]);
            for px in [0, img.rgba.len() / 4 - 1] {
                let got = &img.rgba[px * 4..px * 4 + 3];
                assert!(
                    got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 2),
                    "item {}: got {got:?}, want {want:?}",
                    e.index
                );
            }
        }

        // Nothing changed: nothing cooks.
        let r = exec(&dir, &cooked);
        assert_eq!((r.cooked, r.cached), (0, 100), "{r:?}");

        // One prompt changes: exactly its ten items cook.
        ps[3] = "a changed prompt".into();
        atd_format::batch::save_wedge(&dir, "seeds", &wedge(&ps)).unwrap();
        let r = exec(&dir, &cooked);
        assert_eq!((r.cooked, r.cached), (10, 90), "{r:?}");
        let mut got = cooked.lock().unwrap().clone();
        got.sort_unstable();
        assert_eq!(got, (30..40).collect::<Vec<_>>());
        let bin = atd_format::media_bin::load_bin(&dir).unwrap();
        let changed = bin
            .entries()
            .iter()
            .find(|e| e.attributes["prompt"] == json!("a changed prompt"))
            .unwrap();
        let img = atd_io::image::load(&dir.join(changed.image())).unwrap();
        let want = prompt_colour("a changed prompt");
        assert!(
            img.rgba[..3]
                .iter()
                .zip(want)
                .all(|(a, b)| a.abs_diff(b) <= 2),
            "{:?} vs {want:?}",
            &img.rgba[..3]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plans_check_axes_and_place_pins_routes() {
        let dir =
            std::env::temp_dir().join(format!("atd-batch-plan-{}.atidarla", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        project(&dir, ["127.0.0.1:1", "127.0.0.1:2"]);
        let mut w = wedge(&prompts());
        w.axes[1].param = "no_such".into();
        atd_format::batch::save_wedge(&dir, "seeds", &w).unwrap();
        let err = format!("{:#}", plan(&dir, "seeds").unwrap_err());
        assert!(err.contains("has no parameter `no_such`"), "{err}");
        w.axes[1].param = "seed".into();
        w.axes[1].name = Some("prompt".into());
        atd_format::batch::save_wedge(&dir, "seeds", &w).unwrap();
        let err = format!("{:#}", plan(&dir, "seeds").unwrap_err());
        assert!(err.contains("used twice"), "{err}");
        atd_format::batch::save_wedge(&dir, "seeds", &wedge(&prompts())).unwrap();
        let p = plan(&dir, "seeds").unwrap();
        assert_eq!(p.items.len(), 100);
        assert_eq!(p.adapters, BTreeSet::from(["diffusion.sd".to_owned()]));
        assert!(!p.comfy);
        let keys: BTreeSet<&str> = p.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys.len(), 100, "every item has its own key");

        let reg = crate::builtin_registry();
        let mut net = p.project.main().cloned().unwrap();
        place(&mut net, &reg, "lan:b").unwrap();
        assert_eq!(
            net.param_value(&reg, TARGET, "route"),
            Some(&json!("lan:b"))
        );
        assert_eq!(net.param_value(&reg, NodeId(1), "route"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
