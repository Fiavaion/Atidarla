//! Share Out and Share In (PRD §6 "Share In/Out (Spout/Syphon)"): GPU
//! texture sharing with other applications on the same computer.
//!
//! On Windows they speak Spout 2 through [`atd_interop::spout`], on macOS
//! Syphon through [`atd_interop::syphon`]. Every Spout or Syphon call runs
//! on a worker thread per node, and the cook only records GPU work and
//! hands textures over (see `spout.rs` and `syphon.rs`, CLAUDE.md rules 1
//! and 2). On Linux the nodes still load (projects stay portable, rule 8)
//! and report that sharing is not available, as their capability flag
//! declares (PRD A19).

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use atd_core::capability::{CapabilityRequirement, Fallback};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::GpuOp;

#[cfg(windows)]
mod spout;

#[cfg(target_os = "macos")]
mod syphon;

/// Shown where the operators are not available (A19 fallback, node error).
const UNAVAILABLE: &str = "Texture sharing uses Spout on Windows and Syphon on macOS; Linux has no sharing backend. The node loads and outputs nothing.";

/// The suggestion list of sender names (see [`senders`]).
pub const SENDERS_KEY: &str = "share.senders";

fn capabilities() -> Vec<CapabilityRequirement> {
    vec![CapabilityRequirement {
        flag: "win|mac".parse().expect("valid capability flag"),
        fallback: Fallback::Hide {
            explanation: UNAVAILABLE.into(),
        },
    }]
}

fn socket(name: &str) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback: false,
    }
}

pub fn out_schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.share_out".into(),
        family: OpFamily::Img,
        label: "Share Out".into(),
        summary: "Publishes its input to other applications on this computer (a Spout sender on Windows, a Syphon server on macOS) and passes it through. Receivers get 8-bit BGRA at the input's size. Spout: if the name is taken, the sender becomes `name_1`, `name_2`, ...; Syphon allows shared names. The published name shows in the parameter panel. A Share In in the same project can receive it (a loopback): give the Share Out a name of its own and type it into the Share In. Spout needs the DX12 graphics backend and Syphon the Metal one (the defaults; elsewhere the node reports an error).".into(),
        version: 1,
        params: vec![
            ParamSchema::string("name", "Name", "Atidarla")
                .help("Sender name other applications see: up to 255 bytes; on Windows at least 1 byte and no backslash."),
            ParamSchema::toggle("enabled", "Enabled", true)
                .help("Publish the input; off removes the sender."),
        ],
        inputs: vec![socket("in")],
        outputs: vec![socket("out")],
        capabilities: capabilities(),
        cook_budget_us: 150,
        time_dependent: false,
        sink: true,
    }
}

pub fn in_schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.share_in".into(),
        family: OpFamily::Img,
        label: "Share In".into(),
        summary: "Receives an image another application publishes on this computer (a Spout sender on Windows, a Syphon server on macOS), or a Share Out's. The output takes the sender's size and follows it when it changes; while there is no sender it is black. Spout needs the DX12 graphics backend and Syphon the Metal one (the defaults; elsewhere the node reports an error).".into(),
        version: 1,
        params: vec![
            ParamSchema::string("name", "Sender", "")
                .help("Sender to receive from; empty follows the active sender (Spout: the newest one, unless the user chose another in a Spout tool; Syphon: the newest server). A Syphon server matches by its name, by \"App - Name\" as Syphon apps list it, or by its app's name when it has no name of its own.")
                .suggest(SENDERS_KEY),
        ],
        inputs: vec![],
        outputs: vec![socket("out")],
        capabilities: capabilities(),
        cook_budget_us: 150,
        time_dependent: true,
        sink: false,
    }
}

/// Names of the senders currently published on this computer, sorted,
/// for the Share In name list. May wait up to 67 ms for a Spout mutex, or
/// take Syphon's directory lock, so call it off the render thread. Syphon
/// lists other processes' servers only while the main thread runs its run
/// loop (the app does; see [`atd_interop::syphon`]).
pub fn senders() -> Vec<String> {
    #[cfg(windows)]
    {
        atd_interop::spout::SpoutReceiver::list_senders()
    }
    #[cfg(target_os = "macos")]
    {
        let mut names: Vec<String> = atd_interop::syphon::list_servers()
            .unwrap_or_default()
            .iter()
            .map(|s| s.label().to_owned())
            .collect();
        names.sort();
        names.dedup();
        names
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Vec::new()
    }
}

pub fn create_out(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    #[cfg(windows)]
    {
        spout::create_out(device, format)
    }
    #[cfg(target_os = "macos")]
    {
        syphon::create_out(device, format)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (device, format);
        Box::new(Unavailable)
    }
}

pub fn create_in(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    #[cfg(windows)]
    {
        spout::create_in(device, format)
    }
    #[cfg(target_os = "macos")]
    {
        syphon::create_in(device, format)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (device, format);
        Box::new(Unavailable)
    }
}

/// How many Share workers are running, and a signal when one ends.
static WORKERS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// Starts a Share worker thread, counted until `f` has returned and
/// dropped everything it owns (see [`wait_for_workers`]).
#[cfg(any(windows, target_os = "macos"))]
fn spawn_worker(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            let (count, ended) = &WORKERS;
            *count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
            ended.notify_all();
        }
    }
    *WORKERS.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    let running = Running;
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _running = running;
            f();
        })
        .map(drop)
        .map_err(|e| format!("cannot start the {name} thread: {e}"))
}

/// Waits up to `timeout` for every Share worker to finish; `false` if some
/// are still running. A worker ends on its own once its node is dropped,
/// after stopping its Spout sender or Syphon server (which may wait up to
/// 100 ms for clients) and waiting for the GPU. Call it before the process
/// exits, after the engine is dropped, and never on the render thread
/// (rule 1): on macOS, exiting while a worker is still tearing Syphon and
/// Metal down can trap in CoreAnalytics' exit handler (SIGTRAP in
/// `_dispatch_group_dispose`).
pub fn wait_for_workers(timeout: Duration) -> bool {
    let (count, ended) = &WORKERS;
    let count = count.lock().unwrap_or_else(PoisonError::into_inner);
    let (count, _) = ended
        .wait_timeout_while(count, timeout, |n| *n > 0)
        .unwrap_or_else(PoisonError::into_inner);
    *count == 0
}

/// Both operators where no sharing backend exists: a node error, and no
/// output, so whatever reads it sees black.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
#[derive(Debug)]
struct Unavailable;

#[cfg(any(not(any(windows, target_os = "macos")), test))]
impl GpuOp for Unavailable {
    fn cook(&mut self, _ctx: &mut atd_gpu::engine::CookCtx<'_>) -> Result<(), String> {
        Err(UNAVAILABLE.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::graph::{Edit, Node};
    use atd_core::platform::{Os, PlatformInfo};
    use atd_core::registry::{Availability, Registry};
    use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};

    #[test]
    fn schemas_declare_windows_and_macos_with_a_fallback() {
        let on = |os| PlatformInfo {
            os,
            ..PlatformInfo::host()
        };
        for schema in [out_schema(), in_schema()] {
            match Registry::availability(&schema, &on(Os::Linux)) {
                Availability::Fallback { flag, fallback } => {
                    assert_eq!(flag.to_string(), "win|mac");
                    assert!(matches!(fallback, Fallback::Hide { .. }));
                    assert!(fallback.explanation().contains("Linux"));
                }
                other => panic!("{}: {other:?}", schema.type_id),
            }
            for os in [Os::Windows, Os::MacOs] {
                assert_eq!(
                    Registry::availability(&schema, &on(os)),
                    Availability::Native
                );
            }
        }
    }

    /// What Linux runs, on any OS: the nodes load and cook to a node error
    /// with no output, every frame, without panicking.
    #[test]
    fn unavailable_platforms_load_and_report_an_error() {
        let instance = atd_gpu::primary_instance();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            let required =
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut reg = Registry::default();
        reg.register(out_schema()).unwrap();
        reg.register(in_schema()).unwrap();
        let mut lib = OpLibrary::default();
        lib.add("img.share_out", |_, _| Box::new(Unavailable));
        lib.add("img.share_in", |_, _| Box::new(Unavailable));
        let mut e = Engine::new(device, queue, reg, lib, EngineConfig::default());
        let (out, inp) = (e.alloc_id(), e.alloc_id());
        e.edit(
            "add",
            [
                Edit::AddNode {
                    node: Node::new(out, "img.share_out", "share_out1"),
                },
                Edit::AddNode {
                    node: Node::new(inp, "img.share_in", "share_in1"),
                },
            ],
        )
        .unwrap();
        for frame in 0..3 {
            e.cook_frame(Some(inp), f64::from(frame) / 60.0).unwrap();
            for id in [out, inp] {
                assert_eq!(e.error(id), Some(UNAVAILABLE));
                assert!(e.result(id).is_none(), "no output: black downstream");
            }
        }
    }
}
