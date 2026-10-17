//! Text on the GPU: rasterised off the render thread, placed and coloured
//! in the shader.

use atd_core::graph::{Edit, Endpoint, Node, NodeId, Param, Wire};
use atd_core::registry::Registry;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use serde_json::{Value, json};

const W: u32 = 128;
const H: u32 = 64;

fn engine() -> Option<Engine> {
    let instance = atd_gpu::primary_instance();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
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
    Some(Engine::new(device, queue, reg, lib, config))
}

fn add(e: &mut Engine, ty: &str, params: &[(&str, Value)]) -> NodeId {
    let id = e.alloc_id();
    let name = e.network().unique_name("n");
    let mut edits = vec![Edit::AddNode {
        node: Node::new(id, ty, name),
    }];
    for (p, v) in params {
        edits.push(Edit::SetParam {
            node: id,
            param: (*p).into(),
            value: Some(Param::constant(v.clone())),
        });
    }
    e.edit("add", edits).unwrap();
    id
}

fn set(e: &mut Engine, node: NodeId, p: &str, v: Value) {
    e.edit(
        "set",
        [Edit::SetParam {
            node,
            param: p.into(),
            value: Some(Param::constant(v)),
        }],
    )
    .unwrap();
}

/// Cooks until the raster has arrived; returns RGBA pixels.
fn settle(e: &mut Engine, node: NodeId) -> Vec<u8> {
    let start = std::time::Instant::now();
    loop {
        e.cook(&[node], 0.0).unwrap();
        if !e.pending() {
            break;
        }
        assert!(start.elapsed().as_secs() < 10, "raster arrived");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    read_rgba8(e.device(), e.queue(), e.result(node).unwrap()).unwrap()
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

/// Bounding box of pixels whose red channel exceeds 128 (x0, y0, x1, y1).
fn ink(img: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            if px(img, x, y)[0] > 128 {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((a, c, d, f)) => (a.min(x), c.min(y), d.max(x), f.max(y)),
                });
            }
        }
    }
    b
}

#[test]
fn text_is_drawn_placed_and_composited() {
    let Some(mut e) = engine() else {
        let required = std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ATD_REQUIRE_GPU is set but there is no GPU adapter"
        );
        return;
    };
    let t = add(
        &mut e,
        "img.text",
        &[
            ("text", json!("I")),
            ("font", json!("mono")),
            ("size", json!(0.8)),
            ("background", json!([0.0, 0.0, 1.0, 1.0])),
        ],
    );
    let img = settle(&mut e, t);
    assert_eq!(e.error(t), None);
    let (x0, y0, x1, y1) = ink(&img).expect("text drawn");
    // A centred, tall, narrow stroke.
    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    assert!(
        cx.abs_diff(W / 2) <= 6 && cy.abs_diff(H / 2) <= 8,
        "centre {cx},{cy}"
    );
    assert!(
        y1 - y0 > H / 3 && x1 - x0 < W / 4,
        "shape {x0},{y0}-{x1},{y1}"
    );
    assert_eq!(px(&img, 1, 1), [0, 0, 255, 255], "background");

    // Anchored top-left: ink moves to the top-left corner.
    set(&mut e, t, "align", json!("left"));
    set(&mut e, t, "valign", json!("top"));
    set(&mut e, t, "position", json!([0.05, 0.95]));
    let img = settle(&mut e, t);
    let (x0, y0, _, _) = ink(&img).unwrap();
    assert!(x0 < W / 5 && y0 < H / 4, "top-left {x0},{y0}");

    // Over an input: the input replaces the background.
    let red = add(
        &mut e,
        "img.constant",
        &[("colour", json!([1.0, 0.0, 0.0, 1.0]))],
    );
    e.edit(
        "wire",
        [Edit::Connect {
            wire: Wire::new(Endpoint::new(red, "out"), Endpoint::new(t, "in")),
        }],
    )
    .unwrap();
    set(&mut e, t, "colour", json!([0.0, 1.0, 0.0, 1.0]));
    let img = settle(&mut e, t);
    assert_eq!(
        px(&img, W - 2, H - 2),
        [255, 0, 0, 255],
        "input shows through"
    );
    let green = (0..W * H).filter(|i| {
        let p = px(&img, i % W, i / W);
        p[1] > 200 && p[0] < 60
    });
    assert!(green.count() > 20, "green text over red");

    // A missing font file is reported; the last raster stays.
    set(&mut e, t, "font_file", json!("/no/such/font.ttf"));
    let start = std::time::Instant::now();
    while e.error(t).is_none() {
        e.cook(&[t], 0.0).unwrap();
        assert!(start.elapsed().as_secs() < 10);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(e.error(t).unwrap().contains("font.ttf"));
}
