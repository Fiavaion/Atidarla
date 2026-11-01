//! Splat MAT through `img.render` on the GPU (FR-C1): a splat falls off as
//! a Gaussian, overlapping splats blend far to near whatever their order,
//! opaque geometry hides what is behind it, spherical harmonics change the
//! colour with the view, and a synthetic capture renders the same bytes
//! every run with nothing created after warm-up. Skips without a GPU unless
//! `LSR_REQUIRE_GPU` is set.

use std::sync::{Arc, Mutex, MutexGuard};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use lsr_gpu::engine::{CookMode, Engine, EngineConfig, GeoCtx, GeoOp, OpLibrary};
use lsr_gpu::geo::{Attribute, Geometry, PrimKind, Topology};
use lsr_ops_pnt::splat_file::{SplatCloud, synthetic};
use serde_json::{Value, json};

static GPU: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

type Shapes = Arc<Mutex<Vec<Geometry>>>;

struct Source(Shapes);

impl GeoOp for Source {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let which = ctx.params.i64("which") as usize;
        *ctx.output = self
            .0
            .lock()
            .unwrap()
            .get(which)
            .cloned()
            .unwrap_or_default();
        Ok(())
    }
}

fn engine(size: u32, shapes: &Shapes) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    lsr_ops_mat::register(&mut reg);
    reg.register(OperatorSchema {
        type_id: "msh.test_geo".into(),
        family: OpFamily::Msh,
        label: "Test Geometry".into(),
        summary: "A geometry the test made.".into(),
        version: 1,
        params: vec![ParamSchema::int("which", "Which", 0, Some(0), None)],
        inputs: vec![],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Geo,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 10,
        time_dependent: false,
        sink: false,
    })
    .unwrap();
    let mut lib = OpLibrary::default();
    lsr_ops_mat::library(&mut lib);
    let s = shapes.clone();
    lib.add_geo("msh.test_geo", move |_| Box::new(Source(s.clone())));
    let config = EngineConfig {
        width: size,
        height: size,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    Some(Engine::new(device, queue, reg, lib, config))
}

struct Build<'a> {
    e: &'a mut Engine,
    edits: Vec<Edit>,
}

impl<'a> Build<'a> {
    fn new(e: &'a mut Engine) -> Self {
        Self {
            e,
            edits: Vec::new(),
        }
    }

    fn node(&mut self, ty: &str, params: &[(&str, Value)]) -> NodeId {
        let id = self.e.alloc_id();
        self.edits.push(Edit::AddNode {
            node: Node::new(id, ty, format!("n{}", id.0)),
        });
        self.set(id, params);
        id
    }

    fn set(&mut self, id: NodeId, params: &[(&str, Value)]) {
        for (p, v) in params {
            self.edits.push(Edit::SetParam {
                node: id,
                param: (*p).into(),
                value: Some(Param::constant(v.clone())),
            });
        }
    }

    fn wire(&mut self, from: NodeId, to: NodeId, socket: &str) {
        self.edits.push(Edit::Connect {
            wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, socket)),
        });
    }

    fn done(self) {
        self.e.edit("build", self.edits).unwrap();
    }
}

/// An orthographic camera at `position` looking at the origin, showing
/// [-1, 1]², without antialiasing.
fn camera(position: [f64; 3]) -> Vec<(&'static str, Value)> {
    vec![
        ("projection", json!("orthographic")),
        ("camera", json!("look_at")),
        ("position", json!(position)),
        ("ortho_height", json!(2.0)),
        ("antialias", json!(false)),
    ]
}

fn pixels(e: &Engine, node: NodeId) -> Vec<[u8; 4]> {
    assert_eq!(e.error(node), None);
    lsr_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap())
        .unwrap()
        .chunks_exact(4)
        .map(|c| c.try_into().unwrap())
        .collect()
}

/// Round splats of standard deviation `sigma` at `positions`, colours
/// `colours` (alpha: opacity), with no SH.
fn splats(positions: &[[f32; 3]], colours: &[[f32; 4]], sigma: f32) -> Geometry {
    SplatCloud {
        positions: positions.to_vec(),
        scales: vec![[sigma; 3]; positions.len()],
        rotations: vec![[0.0, 0.0, 0.0, 1.0]; positions.len()],
        colours: colours.to_vec(),
        sh_degree: 0,
        sh: vec![],
    }
    .to_geometry(None)
}

fn square(h: f32, z: f32) -> Geometry {
    let mut g = Geometry::new();
    g.set_points(4, None);
    g.set_topology(Some(Topology::from_prims(&[(
        PrimKind::Polygon,
        &[0, 1, 2, 3],
    )])));
    g.set_point(Attribute::from_vec3(
        "P",
        &[[-h, -h, z], [h, -h, z], [h, h, z], [-h, h, z]],
    ));
    g
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

#[test]
fn a_splat_falls_off_as_a_gaussian_and_is_culled_off_screen() {
    let _gpu = lock();
    let shapes: Shapes = Arc::new(Mutex::new(vec![
        splats(&[[0.0; 3]], &[RED], 0.2),
        splats(&[[5.0, 0.0, 0.0]], &[RED], 0.2),
    ]));
    let Some(mut e) = engine(32, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let on = b.node("msh.test_geo", &[]);
    let off = b.node("msh.test_geo", &[("which", json!(1))]);
    let mat = b.node("mat.splat", &[]);
    let r = b.node("img.render", &camera([0.0, 0.0, 5.0]));
    let r_off = b.node("img.render", &camera([0.0, 0.0, 5.0]));
    b.wire(on, r, "geo");
    b.wire(mat, r, "mat");
    b.wire(off, r_off, "geo");
    b.wire(mat, r_off, "mat");
    b.done();
    e.cook(&[r, r_off], 0.0).unwrap();
    assert_eq!(e.material(mat).unwrap().model, lsr_gpu::mat::Model::Splat);
    let px = pixels(&e, r);
    // 16 px per unit: σ = 3.2 px. Pixel centres sit half a pixel off.
    let at = |x: usize, y: usize| px[y * 32 + x];
    let centre = at(16, 16);
    assert!(
        centre[0] > 200 && centre[1] < 5 && centre[2] < 5,
        "{centre:?}"
    );
    let reds: Vec<u8> = (16..28).map(|x| at(x, 16)[0]).collect();
    assert!(reds.windows(2).all(|w| w[0] >= w[1]), "falls off: {reds:?}");
    // 1.5 px off the centre: exp(-(1.5/3.2)²/2) ≈ 0.90; 4.5 px: ≈ 0.37.
    let expect = |d: f32| 255.0 * (-(d / 3.2).powi(2) / 2.0).exp();
    for (x, d) in [(17usize, 1.5f32), (20, 4.5), (22, 6.5)] {
        let got = f32::from(at(x, 15)[0]);
        let want = expect((d * d + 0.25).sqrt()) * 0.99;
        assert!((got - want).abs() < 14.0, "x {x}: {got} vs {want}");
    }
    assert_eq!(at(0, 0), [0, 0, 0, 255], "the corner is background");
    assert!(
        pixels(&e, r_off).iter().all(|p| *p == [0, 0, 0, 255]),
        "an off-screen splat draws nothing"
    );
}

#[test]
fn splats_blend_far_to_near_whatever_their_order_and_opaque_geometry_hides_them() {
    let _gpu = lock();
    let pos = [[0.0, 0.0, 0.0], [0.0, 0.0, 0.5]];
    let shapes: Shapes = Arc::new(Mutex::new(vec![
        splats(&pos, &[RED, BLUE], 0.3),
        splats(&[pos[1], pos[0]], &[BLUE, RED], 0.3),
        square(0.1, 1.0),
    ]));
    let Some(mut e) = engine(32, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let a = b.node("msh.test_geo", &[]);
    let swapped = b.node("msh.test_geo", &[("which", json!(1))]);
    let wall = b.node("msh.test_geo", &[("which", json!(2))]);
    let mat = b.node("mat.splat", &[]);
    let white = b.node("mat.unlit", &[]);
    let front = b.node("img.render", &camera([0.0, 0.0, 5.0]));
    let front2 = b.node("img.render", &camera([0.0, 0.0, 5.0]));
    let back = b.node(
        "img.render",
        &[camera([0.0, 0.0, -5.0]), vec![("cull", json!("none"))]].concat(),
    );
    let walled = b.node("img.render", &camera([0.0, 0.0, 5.0]));
    for (g, r) in [(a, front), (swapped, front2), (a, back), (a, walled)] {
        b.wire(g, r, "geo");
        b.wire(mat, r, "mat");
    }
    b.wire(wall, walled, "geo2");
    b.wire(white, walled, "mat2");
    b.done();
    e.cook(&[front, front2, back, walled], 0.0).unwrap();
    let f = pixels(&e, front);
    assert_eq!(f, pixels(&e, front2), "the input order does not matter");
    let c = f[16 * 32 + 16];
    assert!(c[2] > 240 && c[0] < 10, "blue (near) over red: {c:?}");
    let c = pixels(&e, back)[16 * 32 + 16];
    assert!(
        c[0] > 240 && c[2] < 10,
        "seen from behind, red is near: {c:?}"
    );
    let w = pixels(&e, walled);
    assert_eq!(
        w[16 * 32 + 16],
        [255, 255, 255, 255],
        "the wall hides the splats"
    );
    let beside = w[16 * 32 + 22];
    assert!(
        beside[2] > 100,
        "beside the wall the splats show: {beside:?}"
    );
}

#[test]
fn spherical_harmonics_colour_the_splat_by_view() {
    let _gpu = lock();
    // Degree 1, coefficient 3 (−C1·x) on red: red seen looking along −X
    // (from +X), none looking along +X.
    let mut cloud = SplatCloud {
        positions: vec![[0.0; 3]],
        scales: vec![[0.3; 3]],
        rotations: vec![[0.0, 0.0, 0.0, 1.0]],
        colours: vec![[0.5, 0.5, 0.5, 1.0]],
        sh_degree: 1,
        sh: vec![[0.0; 3], [0.0; 3], [1.0, 0.0, 0.0]],
    };
    let lit = cloud.to_geometry(None);
    cloud.sh_degree = 0;
    cloud.sh.clear();
    let shapes: Shapes = Arc::new(Mutex::new(vec![lit]));
    let Some(mut e) = engine(32, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let g = b.node("msh.test_geo", &[]);
    let sh = b.node("mat.splat", &[]);
    let flat = b.node("mat.splat", &[("sh_degree", json!(0))]);
    let renders: Vec<(NodeId, NodeId)> = [
        (sh, [5.0, 0.0, 0.0]),
        (sh, [-5.0, 0.0, 0.0]),
        (flat, [5.0, 0.0, 0.0]),
    ]
    .into_iter()
    .map(|(m, eye)| (m, b.node("img.render", &camera(eye))))
    .collect();
    for (m, r) in &renders {
        b.wire(g, *r, "geo");
        b.wire(*m, *r, "mat");
    }
    b.done();
    let ids: Vec<NodeId> = renders.iter().map(|r| r.1).collect();
    e.cook(&ids, 0.0).unwrap();
    let red = |r: NodeId| pixels(&e, r)[16 * 32 + 16][0];
    let (from_px, from_nx, base) = (red(ids[0]), red(ids[1]), red(ids[2]));
    // 0.5 ± C1 (0.4886), times 0.99 coverage.
    assert!(from_px > 240, "seen from +X: {from_px}");
    assert!(from_nx < 10, "seen from −X: {from_nx}");
    assert!(
        (i32::from(base) - 126).abs() <= 3,
        "degree 0: the base colour ({base})"
    );
}

/// A synthetic degree-3 capture, the camera orbiting: frames are the same
/// bytes on two engines, and steady frames create no GPU objects.
#[test]
fn a_capture_renders_the_same_every_run_and_steady_frames_create_nothing() {
    let _gpu = lock();
    let run = || {
        let shapes: Shapes = Arc::new(Mutex::new(vec![synthetic(20_000, 3).to_geometry(None)]));
        let mut e = engine(96, &shapes)?;
        e.set_mode(CookMode::Offline { fps: 30.0 });
        let mut b = Build::new(&mut e);
        let g = b.node("msh.test_geo", &[]);
        let mat = b.node("mat.splat", &[]);
        let r = b.node("img.render", &[("distance", json!(3.2))]);
        b.wire(g, r, "geo");
        b.wire(mat, r, "mat");
        b.done();
        let mut frames = Vec::new();
        let mut created = None;
        for f in 0..10u64 {
            let mut b = Build::new(&mut e);
            b.set(r, &[("azimuth", json!(f as f64 * 36.0))]);
            b.done();
            e.set_frame(f);
            e.cook(&[r], f as f64 / 30.0).unwrap();
            frames.push(pixels(&e, r));
            if f == 2 {
                created = Some((
                    e.buffer_pool().created(),
                    lsr_gpu::geo::kernels::bind_groups_created_total(),
                    e.pool().created(),
                ));
            }
        }
        let after = (
            e.buffer_pool().created(),
            lsr_gpu::geo::kernels::bind_groups_created_total(),
            e.pool().created(),
        );
        assert_eq!(Some(after), created, "steady frames create no GPU objects");
        Some(frames)
    };
    let Some(a) = run() else { return };
    let b = run().unwrap();
    assert_eq!(a, b, "byte-identical frames");
    assert_ne!(a[0], a[3], "the picture changes with the view");
    let covered = a[0].iter().filter(|p| **p != [0, 0, 0, 255]).count();
    assert!(covered > 96 * 96 / 4, "the capture is drawn ({covered} px)");
}
