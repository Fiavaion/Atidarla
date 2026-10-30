//! `img.kernel` (Pixels kernels, A12): pixels against a CPU expectation,
//! inputs by index with unconnected ones black, errors with line and column
//! while the last kernel keeps running. Offline, so each compile is waited
//! for. Skips without a GPU unless `ATD_REQUIRE_GPU` is set.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{CookMode, Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::json;

const W: u32 = 16;
const H: u32 = 8;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return None;
    };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let mut reg = Registry::default();
    atd_ops_img::register(&mut reg);
    let mut lib = OpLibrary::default();
    atd_ops_img::library(&mut lib);
    let config = EngineConfig {
        width: W,
        height: H,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let mut e = Engine::new(device, queue, reg, lib, config);
    e.set_mode(CookMode::Offline { fps: 60.0 });
    Some(e)
}

fn kernel(e: &mut Engine, code: &str) -> NodeId {
    let id = e.alloc_id();
    e.edit(
        "add",
        [
            Edit::AddNode {
                node: Node::new(id, "img.kernel", format!("k{}", id.0)),
            },
            Edit::SetParam {
                node: id,
                param: "code".into(),
                value: Some(Param::constant(json!(code))),
            },
        ],
    )
    .unwrap();
    id
}

fn set_code(e: &mut Engine, node: NodeId, code: &str) {
    e.edit(
        "code",
        [Edit::SetParam {
            node,
            param: "code".into(),
            value: Some(Param::constant(json!(code))),
        }],
    )
    .unwrap();
}

fn pixels(e: &Engine, node: NodeId) -> Vec<u8> {
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

/// `x` in 0..1 as an 8-bit UNORM channel.
fn unorm(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Each pixel against the same arithmetic on the CPU (one step of 8-bit
/// rounding allowed).
fn assert_pixels(got: &[u8], want: impl Fn(u32, u32) -> [f32; 4]) {
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let w = want(x, y).map(unorm);
            for c in 0..4 {
                assert!(
                    got[i + c].abs_diff(w[c]) <= 1,
                    "pixel ({x}, {y}) channel {c}: {} vs {}",
                    got[i + c],
                    w[c]
                );
            }
        }
    }
}

#[test]
fn pixel_kernels_match_the_cpu_and_read_their_inputs() {
    let Some(mut e) = engine() else { return };
    // A generator: uv, pixel indices, resolution, time and a ch() default.
    let gen_code = "float k = ch(\"k\", 0.25, 0, 1);
@Cd = vec4(@P.x, @P.y, float(@ix + @iy) / (@res.x + @res.y) + k * 0.0, @Time);";
    let gen_node = kernel(&mut e, gen_code);
    // A filter of input 0, with input 1 unconnected (black).
    let filter = kernel(
        &mut e,
        "vec4 other = texture(1, @P);\n@Cd = vec4(1 - @Cd.r, @Cd.g * 0.5, ch(\"b\", 0.75), 1) + other;",
    );
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(gen_node, "out"), Endpoint::new(filter, "in0")),
        }],
    )
    .unwrap();
    e.set_frame(30);
    e.cook(&[gen_node, filter], 0.5).unwrap();
    assert_eq!(e.error(gen_node), None);
    assert_eq!(e.error(filter), None);
    let uv = |x: u32, y: u32| ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
    assert_pixels(&pixels(&e, gen_node), |x, y| {
        let (u, v) = uv(x, y);
        [u, v, (x + y) as f32 / (W + H) as f32, 0.5]
    });
    // The filter samples the 8-bit generator output at pixel centres.
    let g = pixels(&e, gen_node);
    assert_pixels(&pixels(&e, filter), |x, y| {
        let i = ((y * W + x) * 4) as usize;
        let (r, gr) = (g[i] as f32 / 255.0, g[i + 1] as f32 / 255.0);
        [1.0 - r, gr * 0.5, 0.75, 1.0]
    });
}

#[test]
fn a_bad_edit_names_the_line_and_keeps_the_last_kernel() {
    let Some(mut e) = engine() else { return };
    let k = kernel(&mut e, "@Cd = vec4(1, 0, 0, 1);");
    e.cook(&[k], 0.0).unwrap();
    assert_eq!(e.error(k), None);
    assert_eq!(&pixels(&e, k)[..4], &[255, 0, 0, 255]);
    set_code(&mut e, k, "@Cd = vec4(0, 1, 0, 1);\n@Cd = nope;");
    e.cook(&[k], 1.0 / 60.0).unwrap();
    let err = e.error(k).expect("an error").to_string();
    assert!(err.contains("kernel:2:"), "{err}");
    assert_eq!(
        &pixels(&e, k)[..4],
        &[255, 0, 0, 255],
        "still the last kernel"
    );
    set_code(&mut e, k, "@Cd = vec4(0, 0, 1, 1);");
    e.cook(&[k], 2.0 / 60.0).unwrap();
    assert_eq!(e.error(k), None);
    assert_eq!(&pixels(&e, k)[..4], &[0, 0, 255, 255]);
}
