//! Share Out and Share In (PRD §6 "Share In/Out (Spout/Syphon)"): GPU
//! texture sharing with other applications on the same computer.
//!
//! On Windows they speak Spout 2 through [`atd_interop::spout`]; every
//! Spout call (shared-memory mutexes, D3D11 device creation, copies) runs
//! on a worker thread per node, and the cook only records GPU work and
//! hands textures over (see [`spout`], CLAUDE.md rules 1 and 2). Syphon
//! on macOS is not wired yet: there, and on Linux, the nodes still load
//! (projects stay portable, rule 8) and report that sharing is not
//! available, as their capability flag declares (PRD A19).

use atd_core::capability::{CapabilityRequirement, Fallback};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::GpuOp;

#[cfg(windows)]
mod spout;

/// Shown where the operators are not available (A19 fallback, node error).
const UNAVAILABLE: &str = "Texture sharing uses Spout and runs on Windows only for now: Syphon on macOS is not wired yet, and Linux has no sharing backend. The node loads and outputs nothing.";

/// The suggestion list of sender names (see [`senders`]).
pub const SENDERS_KEY: &str = "share.senders";

fn capabilities() -> Vec<CapabilityRequirement> {
    vec![CapabilityRequirement {
        flag: "win".parse().expect("valid capability flag"),
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
        summary: "Publishes its input to other applications on this computer (a Spout sender on Windows) and passes it through. Receivers get 8-bit BGRA at the input's size. If the name is taken, the sender becomes `name_1`, `name_2`, ...; the published name shows in the parameter panel. A Share In in the same project can receive it (a loopback): give the Share Out a name of its own and type it into the Share In. Spout needs the DX12 graphics backend (elsewhere the node reports an error); Syphon on macOS is not wired yet.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("name", "Name", "Atidarla")
                .help("Sender name other applications see: 1-255 bytes, no backslash."),
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
        summary: "Receives an image another application publishes on this computer (a Spout sender on Windows), or a Share Out's. The output takes the sender's size and follows it when it changes; while there is no sender it is black. Spout needs the DX12 graphics backend (elsewhere the node reports an error); Syphon on macOS is not wired yet.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("name", "Sender", "")
                .help("Sender to receive from; empty follows the active sender (the newest one, unless the user chose another in a Spout tool).")
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
/// for the Share In name list. May wait up to 67 ms for a Spout mutex, so
/// call it off the render thread.
pub fn senders() -> Vec<String> {
    #[cfg(windows)]
    {
        atd_interop::spout::SpoutReceiver::list_senders()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn create_out(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    #[cfg(windows)]
    {
        spout::create_out(device, format)
    }
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
    {
        let _ = (device, format);
        Box::new(Unavailable)
    }
}

/// Both operators where no sharing backend exists: a node error, and no
/// output, so whatever reads it sees black.
#[cfg(any(not(windows), test))]
#[derive(Debug)]
struct Unavailable;

#[cfg(any(not(windows), test))]
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
    fn schemas_declare_windows_with_a_fallback() {
        for schema in [out_schema(), in_schema()] {
            let mac = PlatformInfo {
                os: Os::MacOs,
                ..PlatformInfo::host()
            };
            match Registry::availability(&schema, &mac) {
                Availability::Fallback { flag, fallback } => {
                    assert_eq!(flag.to_string(), "win");
                    assert!(matches!(fallback, Fallback::Hide { .. }));
                    assert!(fallback.explanation().contains("Syphon"));
                }
                other => panic!("{}: {other:?}", schema.type_id),
            }
            let win = PlatformInfo {
                os: Os::Windows,
                ..PlatformInfo::host()
            };
            assert_eq!(Registry::availability(&schema, &win), Availability::Native);
        }
    }

    /// What macOS and Linux run, on any OS: the nodes load and cook to a
    /// node error with no output, every frame, without panicking.
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
