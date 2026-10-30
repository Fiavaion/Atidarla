//! The M2 PNT operator batch on the GPU: known points give known results.
//! Skips without a GPU unless `ATD_REQUIRE_GPU` is set. One test at a time
//! (`serial`): two devices running compute passes in parallel crash WARP.

mod attributes;
mod fields;
mod foreach;
mod fusion;
mod neighbours;
mod shaping;
mod solver;
mod sources;

use std::sync::{Mutex, MutexGuard};

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_gpu::geo::readback::{read_blocking, read_buffer_blocking};
use serde_json::{Value, json};

static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: atd_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

/// An offline engine (fixed step of 1/50 s) with the PNT operators.
pub fn engine(size: u32) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    atd_ops_pnt::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_pnt::library(&mut lib);
    let config = EngineConfig {
        width: size,
        height: size,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    e.set_mode(CookMode::Offline { fps: 50.0 });
    Some(e)
}

/// The fixed step of [`engine`].
pub const DT: f32 = 1.0 / 50.0;

#[allow(missing_debug_implementations)]
pub struct Build<'a> {
    pub e: &'a mut Engine,
    edits: Vec<Edit>,
}

impl<'a> Build<'a> {
    pub fn new(e: &'a mut Engine) -> Self {
        Self {
            e,
            edits: Vec::new(),
        }
    }

    pub fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let id = self.e.alloc_id();
        self.edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        for (p, v) in params {
            self.edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
        id
    }

    /// A node fed from `from` on `in`.
    pub fn after(&mut self, from: NodeId, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let n = self.node(ty, params);
        self.wire(from, n, "in");
        n
    }

    /// Sets a parameter of a node added earlier.
    pub fn set(&mut self, node: NodeId, param: &str, value: Value) {
        self.edits.push(Edit::SetParam {
            node,
            param: param.into(),
            value: Some(Param::constant(value)),
        });
    }

    pub fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
        self.edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        });
    }

    pub fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

/// Cooks `nodes` at frame `f` and asserts none has an error.
pub fn cook(e: &mut Engine, nodes: &[NodeId], f: u64) {
    e.set_frame(f);
    e.cook(nodes, f as f64 * f64::from(DT)).unwrap();
    for n in nodes {
        assert_eq!(e.error(*n), None, "node {n:?}");
    }
}

/// Point attribute `name` of `node` as rows of its components, the live
/// points only.
pub fn floats(e: &Engine, node: NodeId, name: &str) -> Vec<Vec<f32>> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let bytes = read_blocking(e.device(), e.queue(), e.buffer_pool(), a).unwrap();
    let stride = a.ty().stride() as usize;
    let comps = a.ty().comps() as usize;
    let n = count(e, node) as usize;
    bytes
        .chunks_exact(stride)
        .take(n)
        .map(|c| {
            (0..comps)
                .map(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
                .collect()
        })
        .collect()
}

/// Point attribute `name` of `node` as words, the live points only.
pub fn words(e: &Engine, node: NodeId, name: &str) -> Vec<u32> {
    let g = e.geometry(node).expect("geometry");
    let a = g.point(name).unwrap_or_else(|| panic!("no {name}"));
    let n = count(e, node) as usize;
    read_blocking(e.device(), e.queue(), e.buffer_pool(), a)
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .take(n * (a.ty().stride() / 4) as usize)
        .collect()
}

/// The live count from a geometry's GPU header.
pub fn count(e: &Engine, node: NodeId) -> u32 {
    let g = e.geometry(node).expect("geometry");
    if g.points() == 0 {
        return 0;
    }
    let h = read_buffer_blocking(e.device(), e.queue(), g.header().unwrap(), 48).unwrap();
    u32::from_le_bytes(h[..4].try_into().unwrap())
}

pub fn close(a: &[f32], b: &[f32], eps: f32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
}

#[track_caller]
pub fn assert_rows<W: AsRef<[f32]> + std::fmt::Debug>(got: &[Vec<f32>], want: &[W], eps: f32) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        let w = w.as_ref();
        assert!(close(&g[..w.len()], w, eps), "got {got:?}, want {want:?}");
    }
}

pub fn pixels(e: &Engine, node: NodeId) -> Vec<[u8; 4]> {
    assert_eq!(e.error(node), None);
    atd_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap())
        .unwrap()
        .chunks_exact(4)
        .map(|c| c.try_into().unwrap())
        .collect()
}

/// A 2 × 2 grid at (±1, ±1, 0): ids 0 (−1, −1), 1 (1, −1), 2 (−1, 1), 3 (1, 1).
pub fn grid4(b: &mut Build<'_>) -> NodeId {
    b.node(
        "pnt.points",
        &[("count", json!(4)), ("size", json!([2.0, 2.0, 0.0]))],
    )
}

/// Three points on X: (−1, 0, 0), (0, 0, 0), (1, 0, 0).
pub fn line3(b: &mut Build<'_>) -> NodeId {
    b.node(
        "pnt.points",
        &[
            ("shape", json!("line")),
            ("count", json!(3)),
            ("size", json!([2.0, 0.0, 0.0])),
        ],
    )
}
