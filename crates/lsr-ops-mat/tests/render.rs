//! `img.render` and the MAT operators on the GPU: known geometry gives
//! known pixels (coverage, depth, culling, lighting, textures, wireframes,
//! lines, sprites), hostile geometry is drawn safely, and frames repeat
//! exactly. Skips without a GPU unless `LSR_REQUIRE_GPU` is set. The tests
//! share one lock: WARP crashes with two devices rendering in parallel.

use std::sync::{Arc, Mutex, MutexGuard};

use lsr_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use lsr_gpu::engine::{CookMode, Engine, EngineConfig, GeoCtx, GeoOp, OpLibrary};
use lsr_gpu::geo::{Attribute, Geometry, PrimKind, Topology};
use serde_json::{Value, json};

static GPU: Mutex<()> = Mutex::new(());

/// Whether the adapter is a software rasterizer (WARP on the Windows runner).
static SOFTWARE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    SOFTWARE.store(
        adapter.get_info().device_type == wgpu::DeviceType::Cpu,
        std::sync::atomic::Ordering::Relaxed,
    );
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: lsr_gpu::required_limits(&adapter),
        ..Default::default()
    }))
    .ok()
}

/// Geometries a test hands to its `msh.test_geo` nodes (parameter `which`).
type Shapes = Arc<Mutex<Vec<Geometry>>>;

struct Source(Shapes);

impl GeoOp for Source {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let which = ctx.params.i64("which") as usize;
        let shapes = self.0.lock().unwrap();
        *ctx.output = shapes.get(which).cloned().unwrap_or_default();
        Ok(())
    }
}

fn engine(size: u32, shapes: &Shapes) -> Option<Engine> {
    let (device, queue) = gpu()?;
    let mut reg = Registry::default();
    lsr_ops_mat::register(&mut reg);
    lsr_ops_img::register(&mut reg);
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
    lsr_ops_img::library(&mut lib);
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

/// An orthographic camera on +Z showing [-1, 1]² (16 × 16: 8 px per unit).
fn front(extra: &[(&'static str, Value)]) -> Vec<(&'static str, Value)> {
    let mut p = vec![
        ("projection", json!("orthographic")),
        ("camera", json!("look_at")),
        ("position", json!([0.0, 0.0, 5.0])),
        ("ortho_height", json!(2.0)),
        ("antialias", json!(false)),
    ];
    p.extend(extra.iter().cloned());
    p
}

fn pixels(e: &Engine, node: NodeId) -> Vec<[u8; 4]> {
    assert_eq!(e.error(node), None);
    lsr_interop::readback::read_rgba8(e.device(), e.queue(), e.result(node).unwrap())
        .unwrap()
        .chunks_exact(4)
        .map(|c| c.try_into().unwrap())
        .collect()
}

/// A square of half-size `h` at depth `z` facing +Z (counter-clockwise).
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
    let uv = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let bytes = uv.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    g.set_point(Attribute::cpu("uv", lsr_core::geo::AttrType::VEC2, bytes));
    g
}

fn inside(x: usize, y: usize, lo: usize, hi: usize) -> bool {
    (lo..=hi).contains(&x) && (lo..=hi).contains(&y)
}

const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn an_unlit_square_covers_exactly_its_pixels_with_and_without_msaa() {
    let _gpu = lock();
    let shapes: Shapes = Arc::new(Mutex::new(vec![square(0.5, 0.0)]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let geo = b.node("msh.test_geo", &[]);
    let mat = b.node("mat.unlit", &[("colour", json!([1.0, 0.2, 0.0, 1.0]))]);
    let aliased = b.node("img.render", &front(&[]));
    let msaa = b.node("img.render", &front(&[("antialias", json!(true))]));
    for r in [aliased, msaa] {
        b.wire(geo, r, "geo");
        b.wire(mat, r, "mat");
    }
    b.done();
    e.cook(&[aliased, msaa], 0.0).unwrap();
    for r in [aliased, msaa] {
        let px = pixels(&e, r);
        for y in 0..16 {
            for x in 0..16 {
                let want = if inside(x, y, 4, 11) {
                    [255, 51, 0, 255]
                } else {
                    BLACK
                };
                assert_eq!(px[y * 16 + x], want, "pixel ({x}, {y})");
            }
        }
    }
    let m = e.material(mat).unwrap();
    assert_eq!(m.model, lsr_gpu::mat::Model::Unlit);
    assert_eq!(m.texture_source, None);
}

#[test]
fn depth_keeps_the_nearest_surface_whatever_the_order_and_culling_hides_backs() {
    let _gpu = lock();
    let shapes: Shapes = Arc::new(Mutex::new(vec![square(0.5, 0.0), square(0.25, 1.0)]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let far = b.node("msh.test_geo", &[]);
    let near = b.node("msh.test_geo", &[("which", json!(1))]);
    let red = b.node("mat.unlit", &[("colour", json!([1.0, 0.0, 0.0, 1.0]))]);
    let blue = b.node("mat.unlit", &[("colour", json!([0.0, 0.0, 1.0, 1.0]))]);
    // Near first, then far; and the other way round.
    let a = b.node("img.render", &front(&[]));
    b.wire(near, a, "geo");
    b.wire(blue, a, "mat");
    b.wire(far, a, "geo2");
    b.wire(red, a, "mat2");
    let c = b.node("img.render", &front(&[]));
    b.wire(far, c, "geo");
    b.wire(red, c, "mat");
    b.wire(near, c, "geo2");
    b.wire(blue, c, "mat2");
    // Seen from behind: culled by default, drawn with cull none.
    let behind = [("position", json!([0.0, 0.0, -5.0]))];
    let culled = b.node("img.render", &front(&behind));
    b.wire(far, culled, "geo");
    let unculled = b.node(
        "img.render",
        &front(&[behind[0].clone(), ("cull", json!("none"))]),
    );
    b.wire(far, unculled, "geo");
    b.wire(red, unculled, "mat");
    b.done();
    e.cook(&[a, c, culled, unculled], 0.0).unwrap();
    let pa = pixels(&e, a);
    assert_eq!(pa, pixels(&e, c), "draw order does not matter");
    assert_eq!(pa[8 * 16 + 8], [0, 0, 255, 255], "the near square in front");
    assert_eq!(pa[4 * 16 + 4], [255, 0, 0, 255], "the far one around it");
    assert_eq!(pa[0], BLACK);
    assert!(
        pixels(&e, culled).iter().all(|p| *p == BLACK),
        "back faces culled"
    );
    let pu = pixels(&e, unculled);
    assert_eq!(pu[8 * 16 + 8], [255, 0, 0, 255]);
}

#[test]
fn pbr_toon_and_matcap_follow_the_light_and_the_normal() {
    let _gpu = lock();
    let shapes: Shapes = Arc::new(Mutex::new(vec![square(1.0, 0.0)]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let geo = b.node("msh.test_geo", &[]);
    let lit_front = [
        ("light_azimuth", json!(0.0)),
        ("light_elevation", json!(0.0)),
    ];
    let lit_back = [
        ("light_azimuth", json!(180.0)),
        ("light_elevation", json!(0.0)),
    ];
    let pbr_front = b.node("img.render", &front(&lit_front));
    let pbr_back = b.node("img.render", &front(&lit_back));
    for r in [pbr_front, pbr_back] {
        b.wire(geo, r, "geo");
    }
    let toon = b.node(
        "mat.toon",
        &[
            ("colour", json!([1.0, 1.0, 1.0, 1.0])),
            ("shadow", json!([0.0, 0.0, 0.0, 1.0])),
            ("rim", json!(0.0)),
            ("highlight", json!(0.0)),
        ],
    );
    let toon_front = b.node("img.render", &front(&lit_front));
    let toon_back = b.node("img.render", &front(&lit_back));
    for r in [toon_front, toon_back] {
        b.wire(geo, r, "geo");
        b.wire(toon, r, "mat");
    }
    let green = b.node("img.constant", &[("colour", json!([0.0, 1.0, 0.0, 1.0]))]);
    let matcap = b.node("mat.matcap", &[("tint", json!([1.0, 0.25, 1.0, 1.0]))]);
    b.wire(green, matcap, "matcap");
    let mc = b.node("img.render", &front(&[]));
    b.wire(geo, mc, "geo");
    b.wire(matcap, mc, "mat");
    b.done();
    let all = [pbr_front, pbr_back, toon_front, toon_back, mc];
    e.cook(&all, 0.0).unwrap();
    let centre = |n| pixels(&e, n)[8 * 16 + 8];
    let (f, k) = (centre(pbr_front), centre(pbr_back));
    // Default material: 0.8 grey; lit head-on it is at least the diffuse
    // 0.8; from behind only the ambient 0.15 × 0.8 (0.18 for blue).
    assert!(f[0] >= 204 && f[0] == f[1], "{f:?}");
    assert_eq!(&k[..3], &[31, 31, 37], "{k:?}");
    assert_eq!(centre(toon_front), [255, 255, 255, 255]);
    assert_eq!(centre(toon_back), [0, 0, 0, 255]);
    assert_eq!(centre(mc), [0, 64, 0, 255], "matcap × tint");
    assert_eq!(
        e.material(matcap).unwrap().texture_source,
        Some(green),
        "the texture is wired by the engine"
    );
}

#[test]
fn textures_map_upright_through_uv() {
    let _gpu = lock();
    let shapes: Shapes = Arc::new(Mutex::new(vec![square(1.0, 0.0)]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let geo = b.node("msh.test_geo", &[]);
    let across = b.node("img.ramp", &[]);
    let down = b.node("img.ramp", &[("type", json!("vertical"))]);
    let r = [across, down].map(|ramp| {
        let mat = b.node("mat.unlit", &[]);
        b.wire(ramp, mat, "map");
        let r = b.node("img.render", &front(&[]));
        b.wire(geo, r, "geo");
        b.wire(mat, r, "mat");
        (ramp, r)
    });
    b.done();
    let ids: Vec<NodeId> = r.iter().flat_map(|(a, b)| [*a, *b]).collect();
    e.cook(&ids, 0.0).unwrap();
    // The square fills the image, so the render shows the ramp itself.
    for (ramp, render) in r {
        let want = pixels(&e, ramp);
        let got = pixels(&e, render);
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                g.iter().zip(w).all(|(a, b)| a.abs_diff(*b) <= 1),
                "pixel {i}: {g:?} vs {w:?}"
            );
        }
    }
}

#[test]
fn line_materials_draw_polygon_sides_not_diagonals_and_polylines_and_sprites() {
    let _gpu = lock();
    // A square, and a polyline across the middle of the image.
    let mut line = Geometry::new();
    line.set_points(2, None);
    line.set_topology(Some(Topology::from_prims(&[(PrimKind::Polyline, &[0, 1])])));
    line.set_point(Attribute::from_vec3(
        "P",
        &[[-2.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
    ));
    // One point at the image centre, no primitives.
    let mut dot = Geometry::new();
    dot.set_points(1, None);
    dot.set_point(Attribute::from_vec3("P", &[[0.0, 0.0, 0.0]]));
    let shapes: Shapes = Arc::new(Mutex::new(vec![square(0.5, 0.0), line, dot]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let sq = b.node("msh.test_geo", &[]);
    let pl = b.node("msh.test_geo", &[("which", json!(1))]);
    let pt = b.node("msh.test_geo", &[("which", json!(2))]);
    let wire = b.node("mat.line", &[("width", json!(2.0))]);
    let filled = b.node(
        "mat.line",
        &[
            ("width", json!(2.0)),
            ("fill", json!(true)),
            ("fill_colour", json!([0.0, 0.0, 1.0, 1.0])),
        ],
    );
    let rw = b.node("img.render", &front(&[]));
    b.wire(sq, rw, "geo");
    b.wire(wire, rw, "mat");
    let rf = b.node("img.render", &front(&[]));
    b.wire(sq, rf, "geo");
    b.wire(filled, rf, "mat");
    let rl = b.node("img.render", &front(&[]));
    b.wire(pl, rl, "geo");
    b.wire(wire, rl, "mat");
    let sprite = b.node(
        "mat.point_sprite",
        &[
            ("size_mode", json!("pixels")),
            ("size", json!(4.0)),
            ("shape", json!("square")),
        ],
    );
    let rs = b.node("img.render", &front(&[]));
    b.wire(pt, rs, "geo");
    b.wire(sprite, rs, "mat");
    // The overlay on a lit surface.
    let ro = b.node(
        "img.render",
        &front(&[("wireframe", json!(true)), ("wire_width", json!(2.0))]),
    );
    b.wire(sq, ro, "geo");
    b.done();
    e.cook(&[rw, rf, rl, rs, ro], 0.0).unwrap();
    // Lines one pixel wide sit exactly on pixel boundaries here, and software
    // rasterizers differ on those: the Windows runner's WARP left pixels (4, 4)
    // and (5, 4) of the ring black where NVIDIA, Vulkan, Metal and a local WARP
    // cover them. The exact-pixel checks run on real GPUs.
    if SOFTWARE.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("skipping the exact line pixels: software adapter");
        return;
    }
    let white = [255, 255, 255, 255];
    let ring = |x: usize, y: usize| inside(x, y, 4, 11) && !inside(x, y, 5, 10);
    for (node, interior) in [(rw, BLACK), (rf, [0, 0, 255, 255])] {
        let px = pixels(&e, node);
        for y in 0..16 {
            for x in 0..16 {
                let want = if ring(x, y) {
                    white
                } else if inside(x, y, 5, 10) {
                    interior
                } else {
                    BLACK
                };
                assert_eq!(px[y * 16 + x], want, "pixel ({x}, {y}) of {node:?}");
            }
        }
    }
    let px = pixels(&e, rl);
    for y in 0..16 {
        for x in 0..16 {
            let want = if (7..=8).contains(&y) { white } else { BLACK };
            assert_eq!(px[y * 16 + x], want, "polyline pixel ({x}, {y})");
        }
    }
    let px = pixels(&e, rs);
    for y in 0..16 {
        for x in 0..16 {
            let want = if inside(x, y, 6, 9) { white } else { BLACK };
            assert_eq!(px[y * 16 + x], want, "sprite pixel ({x}, {y})");
        }
    }
    let px = pixels(&e, ro);
    assert_eq!(px[4 * 16 + 8], white, "overlay on the side");
    assert_ne!(px[8 * 16 + 8], white, "no overlay on the diagonal");
    assert_ne!(px[8 * 16 + 8], BLACK, "the surface under it");
}

/// Hostile topology and values: nothing faults, the valid parts draw, a
/// malformed size is a node error.
#[test]
fn hostile_geometry_is_drawn_safely() {
    let _gpu = lock();
    let base = || {
        let mut g = Geometry::new();
        g.set_points(4, None);
        g.set_point(Attribute::from_vec3(
            "P",
            &[
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
        ));
        g
    };
    let u32s = |name: &str, v: &[u32]| Attribute::from_u32(name, v);
    // 0: a good triangle (0 1 2), one with an index out of range, an empty
    // polygon, a two-corner polygon, a one-point polyline, an unknown kind,
    // a backwards range and one past the end of the corner list.
    let mut wild = base();
    wild.set_topology(Some(Topology {
        kinds: u32s("__kind", &[0, 0, 0, 0, 1, 7, 0, 0]),
        start: u32s("__start", &[0, 3, 6, 6, 8, 9, 12, 10, 99]),
        corners: u32s("__corners", &[0, 1, 2, 0, 9, 3, 1, 2, 3, 0, 1, 2]),
    }));
    // 1: NaN and infinite positions in an otherwise good square.
    let mut nan = square(0.5, 0.0);
    nan.set_point(Attribute::from_vec3(
        "P",
        &[
            [-0.5, -0.5, 0.0],
            [f32::NAN, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, f32::INFINITY, 0.0],
        ],
    ));
    // 2: start has the wrong length: an error, not a fault.
    let mut short = base();
    short.set_topology(Some(Topology {
        kinds: u32s("__kind", &[0, 0]),
        start: u32s("__start", &[0, 3]),
        corners: u32s("__corners", &[0, 1, 2]),
    }));
    // 3: one polygon of 100,000 corners on a circle (99,998 fan triangles).
    let n = 100_000u32;
    let mut big = Geometry::new();
    big.set_points(n, None);
    let ring: Vec<[f32; 3]> = (0..n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            [0.5 * a.cos(), 0.5 * a.sin(), 0.0]
        })
        .collect();
    big.set_point(Attribute::from_vec3("P", &ring));
    let corners: Vec<u32> = (0..n).collect();
    big.set_topology(Some(Topology::from_prims(&[(PrimKind::Polygon, &corners)])));
    // 4: topology with no primitives, and points with no `P`.
    let mut empty = base();
    empty.set_topology(Some(Topology::from_prims(&[])));
    let mut no_p = Geometry::new();
    no_p.set_points(1, None);
    no_p.set_point(Attribute::from_f32("age", &[0.0]));
    let shapes: Shapes = Arc::new(Mutex::new(vec![wild, nan, short, big, empty, no_p]));
    let Some(mut e) = engine(16, &shapes) else {
        return;
    };
    let mut b = Build::new(&mut e);
    let renders: Vec<NodeId> = (0..6)
        .map(|i| {
            let g = b.node("msh.test_geo", &[("which", json!(i))]);
            let mat = b.node("mat.unlit", &[]);
            let r = b.node("img.render", &front(&[]));
            b.wire(g, r, "geo");
            b.wire(mat, r, "mat");
            r
        })
        .collect();
    b.done();
    e.cook(&renders, 0.0).unwrap();
    let white = [255, 255, 255, 255];
    // The good triangle (0 1 2) is the lower-right half of the square.
    let px = pixels(&e, renders[0]);
    assert_eq!(px[10 * 16 + 10], white, "lower right: drawn");
    assert_eq!(px[5 * 16 + 5], BLACK, "upper left: nothing else");
    // The NaN square: both fan triangles touch a bad point, so neither draws.
    assert!(pixels(&e, renders[1]).iter().all(|p| *p == BLACK));
    let err = e.error(renders[2]).expect("malformed start is an error");
    assert!(err.contains("start offsets"), "{err}");
    let px = pixels(&e, renders[3]);
    assert_eq!(px[8 * 16 + 8], white, "the 100k-gon is drawn");
    assert_eq!(px[0], BLACK);
    // No primitives: the four points as 3-px discs.
    let px = pixels(&e, renders[4]);
    assert_eq!(px[4 * 16 + 4], white, "a point sprite at a corner");
    assert_eq!(px[8 * 16 + 8], BLACK);
    let err = e.error(renders[5]).expect("no P is an error");
    assert!(err.contains("`P`"), "{err}");
}

/// Offline frames are byte-identical across engines, and steady frames
/// create no GPU objects.
#[test]
fn frames_repeat_exactly_and_steady_frames_create_nothing() {
    let _gpu = lock();
    let run = || {
        let shapes: Shapes = Arc::new(Mutex::new(vec![]));
        let mut e = engine(64, &shapes)?;
        e.set_mode(CookMode::Offline { fps: 30.0 });
        let mut b = Build::new(&mut e);
        let solid = b.node(
            "msh.platonic",
            &[
                ("shape", json!("dodecahedron")),
                ("face_colours", json!(true)),
            ],
        );
        let ico = b.node("msh.platonic", &[("radius", json!(0.6))]);
        let pbr = b.node("mat.pbr", &[("roughness", json!(0.3))]);
        let toon = b.node("mat.toon", &[]);
        let r = b.node("img.render", &[("wireframe", json!(true))]);
        b.wire(solid, r, "geo");
        b.wire(pbr, r, "mat");
        b.wire(ico, r, "geo2");
        b.wire(toon, r, "mat2");
        b.done();
        // The camera orbits: every frame differs.
        let mut frames = Vec::new();
        let mut created = None;
        for f in 0..12u64 {
            let mut b = Build::new(&mut e);
            b.set(r, &[("azimuth", json!(f as f64 * 30.0))]);
            b.done();
            e.set_frame(f);
            e.cook(&[r], f as f64 / 30.0).unwrap();
            frames.push(pixels(&e, r));
            if f == 3 {
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
    assert_ne!(a[0], a[5], "the picture changes");
    assert!(
        a[0].iter().filter(|p| **p != BLACK).count() > 500,
        "the solids are drawn"
    );
}
