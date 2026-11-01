//! GPU pixel tests for output mapping (FR-S6): the identity map copies,
//! a slice lands where its quad says, perspective pins follow the
//! homography, soft edges follow the blend curve, masks cut, test patterns
//! draw, and steady-state draws create no GPU objects.

use lsr_gpu::output::{OutputMap, OutputMapper, Pattern, blend_weight, homography};
use lsr_interop::readback::{read_rgba8, test_pattern, write_rgba8};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A texture filled with `pixels(x, y)`.
fn image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    pixels: impl Fn(u32, u32) -> [u8; 4],
) -> wgpu::Texture {
    let t = texture(device, w, h);
    let data: Vec<u8> = (0..w * h).flat_map(|i| pixels(i % w, i / w)).collect();
    write_rgba8(queue, &t, &data);
    t
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    mapper: OutputMapper,
    black: wgpu::Texture,
}

impl Rig {
    fn new() -> Option<Self> {
        let (device, queue) = gpu()?;
        let mapper = OutputMapper::new(&device, FORMAT);
        let black = image(&device, &queue, 1, 1, |_, _| [0; 4]);
        Some(Self {
            device,
            queue,
            mapper,
            black,
        })
    }

    fn map(&mut self, text: &str) {
        let map = OutputMap::parse(text).unwrap();
        self.mapper.set_map(&self.device, &self.queue, &map);
    }

    fn run(
        &mut self,
        source: &wgpu::Texture,
        mask: Option<&wgpu::Texture>,
        pattern: Pattern,
        out: &wgpu::Texture,
    ) -> Vec<u8> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.mapper.draw(
            &self.device,
            &self.queue,
            &mut encoder,
            source,
            mask,
            &self.black,
            pattern,
            out,
        );
        self.queue.submit([encoder.finish()]);
        read_rgba8(&self.device, &self.queue, out).unwrap()
    }
}

macro_rules! rig_or_skip {
    () => {
        match Rig::new() {
            Some(r) => r,
            None => return,
        }
    };
}

fn px(data: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    data[i..i + 4].try_into().unwrap()
}

#[track_caller]
fn near(got: [u8; 4], want: [u8; 4], tol: u8) {
    assert!(
        got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= tol),
        "got {got:?}, want {want:?}"
    );
}

#[test]
fn identity_map_copies_the_source() {
    let mut r = rig_or_skip!();
    let (w, h) = (64, 32);
    let data = test_pattern(w, h, 5);
    let src = texture(&r.device, w, h);
    write_rgba8(&r.queue, &src, &data);
    let out = texture(&r.device, w, h);
    r.map("");
    let got = r.run(&src, None, Pattern::Off, &out);
    let worst = got.iter().zip(&data).map(|(a, b)| a.abs_diff(*b)).max();
    assert!(worst <= Some(1), "largest difference {worst:?}");
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255; 4];
const CLEAR: [u8; 4] = [0; 4];

/// Quadrants: red top left, green top right, blue bottom left, white
/// bottom right.
fn quadrants(r: &Rig, n: u32) -> wgpu::Texture {
    image(&r.device, &r.queue, n, n, |x, y| {
        match (x < n / 2, y < n / 2) {
            (true, true) => RED,
            (false, true) => GREEN,
            (true, false) => BLUE,
            (false, false) => WHITE,
        }
    })
}

#[test]
fn a_slice_lands_on_its_destination_quad() {
    let mut r = rig_or_skip!();
    let n = 64;
    let src = quadrants(&r, n);
    let out = texture(&r.device, n, n);
    // The red quadrant, scaled up into the right half.
    r.map("[slice a]\nsource = 0 0 0.5 0.5\ndst = 0.5 0 0.5 1\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    for (x, y) in [(33, 1), (62, 62), (48, 32)] {
        near(px(&got, n, x, y), RED, 0);
    }
    for (x, y) in [(1, 1), (30, 62), (20, 32)] {
        near(px(&got, n, x, y), CLEAR, 0);
    }
}

#[test]
fn a_perspective_pin_follows_the_homography() {
    let mut r = rig_or_skip!();
    let n = 128;
    let src = quadrants(&r, n);
    let out = texture(&r.device, n, n);
    // A strong keystone: the top edge narrow.
    let pts = [[0.3, 0.05], [0.7, 0.05], [0.0, 0.95], [1.0, 0.95]];
    r.map(
        "[slice k]\nwarp = perspective\npoints = 0.3 0.05 0.7 0.05\npoints = 0 0.95 1 0.95\nsubdiv = 64\n",
    );
    let got = r.run(&src, None, Pattern::Off, &out);
    let h = homography(&pts);
    for (u, v, want) in [
        (0.3, 0.3, RED),
        (0.7, 0.3, GREEN),
        (0.3, 0.7, BLUE),
        (0.7, 0.7, WHITE),
        // Next to the centre, which perspective moves up from the middle
        // of the quad (bilinear would put the colours elsewhere).
        (0.45, 0.45, RED),
        (0.55, 0.55, WHITE),
    ] {
        let p = h(u, v);
        let (x, y) = ((p[0] * n as f32) as u32, (p[1] * n as f32) as u32);
        near(px(&got, n, x, y), want, 2);
    }
    // Outside the quad.
    near(px(&got, n, 2, 2), CLEAR, 0);
}

#[test]
fn soft_edges_follow_the_blend_curve() {
    let mut r = rig_or_skip!();
    let (w, h) = (256, 4);
    let src = image(&r.device, &r.queue, 4, 4, |_, _| WHITE);
    let out = texture(&r.device, w, h);
    r.map("[slice a]\nblend = 0.25 0.5 0 0\nblend_power = 2\nblend_gamma = 1\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    for x in 0..w {
        let u = (x as f32 + 0.5) / w as f32;
        let want = blend_weight(u / 0.25, 2.0, 1.0) * blend_weight((1.0 - u) / 0.5, 2.0, 1.0);
        let v = (want * 255.0).round() as u8;
        near(px(&got, w, x, 1), [v, v, v, v], 2);
    }
    // Gamma 2.2 lifts the middle of the ramp: 0.5^(1/2.2).
    r.map("[slice a]\nblend = 0 1 0 0\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    let mid = (0.5f32.powf(1.0 / 2.2) * 255.0).round() as u8;
    near(px(&got, w, 127, 1), [mid; 4], 3);
}

#[test]
fn masks_cut_the_output() {
    let mut r = rig_or_skip!();
    let n = 64;
    let src = image(&r.device, &r.queue, 4, 4, |_, _| WHITE);
    let out = texture(&r.device, n, n);
    // A triangle: top left, top right, bottom left.
    r.map("[slice a]\nmask = 0 0 1 0 0 1\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    near(px(&got, n, 10, 10), WHITE, 0);
    near(px(&got, n, 54, 54), CLEAR, 0);
    r.map("[slice a]\nmask = 0 0 1 0 0 1\nmask_invert = true\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    near(px(&got, n, 10, 10), CLEAR, 0);
    near(px(&got, n, 54, 54), WHITE, 0);
    // An image mask: white on the left half, black on the right.
    let mask = image(&r.device, &r.queue, n, n, |x, _| {
        if x < n / 2 { WHITE } else { [0, 0, 0, 255] }
    });
    r.map("");
    let got = r.run(&src, Some(&mask), Pattern::Off, &out);
    near(px(&got, n, 10, 30), WHITE, 0);
    near(px(&got, n, 54, 30), CLEAR, 0);
}

#[test]
fn later_slices_draw_over_earlier_ones() {
    let mut r = rig_or_skip!();
    let n = 64;
    let src = quadrants(&r, n);
    let out = texture(&r.device, n, n);
    r.map("[slice red]\nsource = 0 0 0.5 0.5\n\n[slice green]\nsource = 0.5 0 0.5 0.5\ndst = 0.25 0.25 0.5 0.5\n");
    let got = r.run(&src, None, Pattern::Off, &out);
    near(px(&got, n, 4, 4), RED, 0);
    near(px(&got, n, 32, 32), GREEN, 0);
}

#[test]
fn test_patterns_draw_deterministically() {
    let mut r = rig_or_skip!();
    let (w, h) = (128, 64);
    let src = image(&r.device, &r.queue, 4, 4, |_, _| [0, 0, 0, 255]);
    let out = texture(&r.device, w, h);
    r.map("[slice a]\ndst = 0 0 0.5 1\n\n[slice b]\ndst = 0.5 0 0.5 1\n");
    // Bars: white (75%) first, black last, in each slice.
    let bars = r.run(&src, None, Pattern::Bars, &out);
    near(px(&bars, w, 2, 32), [191, 191, 191, 255], 1);
    near(px(&bars, w, 62, 32), [0, 0, 0, 255], 1);
    near(px(&bars, w, 66, 32), [191, 191, 191, 255], 1);
    // Gradient: a ramp across each slice.
    let grad = r.run(&src, None, Pattern::Gradient, &out);
    assert!(px(&grad, w, 4, 32)[0] < 30 && px(&grad, w, 60, 32)[0] > 225);
    // Grid and circles cover their slices and draw the same twice.
    for p in [Pattern::Grid, Pattern::Circles] {
        let a = r.run(&src, None, p, &out);
        let b = r.run(&src, None, p, &out);
        assert_eq!(a, b, "{p:?}");
        assert!(a.chunks(4).all(|c| c[3] == 255), "{p:?} is opaque");
        // The two slices differ (their colours, their numbers).
        let left: Vec<u8> = (0..h).flat_map(|y| px(&a, w, 20, y)).collect();
        let right: Vec<u8> = (0..h).flat_map(|y| px(&a, w, 84, y)).collect();
        if p == Pattern::Circles {
            assert_ne!(left, right);
        }
    }
}

#[test]
fn steady_draws_create_no_gpu_objects() {
    let mut r = rig_or_skip!();
    let src = image(&r.device, &r.queue, 4, 4, |_, _| WHITE);
    let (a, b) = (texture(&r.device, 16, 16), texture(&r.device, 16, 16));
    r.map("[slice a]\nblend = 0.1 0.1 0 0\n");
    for i in 0..40 {
        let out = if i % 2 == 0 { &a } else { &b };
        r.run(&src, None, Pattern::Off, out);
    }
    // One bind group (source, mask) and the two outputs it alternates.
    assert_eq!(r.mapper.cached(), (1, 2));
}
