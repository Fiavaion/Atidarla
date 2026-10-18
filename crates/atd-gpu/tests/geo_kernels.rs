//! The GEO compute building blocks against CPU references: multi-level
//! scan, compaction (map and header) and the generic gather, with sizes that
//! need one, two and three scan levels.

use atd_core::geo::header;
use atd_gpu::geo::kernels::{Compactor, Gather, GatherMap, Scan};
use atd_gpu::geo::readback::read_buffer_blocking;
use wgpu::util::DeviceExt as _;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn buffer(device: &wgpu::Device, words: &[u32]) -> wgpu::Buffer {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: &bytes,
        usage: atd_gpu::geo::ATTR_USAGE,
    })
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, b: &wgpu::Buffer, n: usize) -> Vec<u32> {
    read_buffer_blocking(device, queue, b, (n * 4) as u64)
        .unwrap()
        .chunks_exact(4)
        .take(n)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// A deterministic pseudo-random sequence.
fn lcg(n: usize, seed: u32) -> Vec<u32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            x >> 8
        })
        .collect()
}

/// The shared noise library: `atd_gnoise`'s analytic gradient matches
/// central differences of its value, and `atd_curl` is divergence-free.
#[test]
fn gradient_noise_derivatives_and_curl_are_right() {
    use atd_gpu::geo::kernels::{Bind, Kernel, Work};
    let Some((device, queue)) = gpu() else { return };
    // Per sample: value, gradient, the value at ±h along each axis, and the
    // divergence of the curl by central differences.
    let src = format!(
        "{}{}
@group(0) @binding(0) var<storage, read> pts: array<vec4f>;
@group(0) @binding(1) var<storage, read_write> out: array<array<vec4f, 3>>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    if (i >= arrayLength(&pts)) {{ return; }}
    let p = pts[i].xyz;
    let h = 1e-3;
    let n = atd_gnoise(p, 7u);
    let dx = (atd_gnoise(p + vec3f(h, 0.0, 0.0), 7u).x - atd_gnoise(p - vec3f(h, 0.0, 0.0), 7u).x) / (2.0 * h);
    let dy = (atd_gnoise(p + vec3f(0.0, h, 0.0), 7u).x - atd_gnoise(p - vec3f(0.0, h, 0.0), 7u).x) / (2.0 * h);
    let dz = (atd_gnoise(p + vec3f(0.0, 0.0, h), 7u).x - atd_gnoise(p - vec3f(0.0, 0.0, h), 7u).x) / (2.0 * h);
    let div = (atd_curl(p + vec3f(h, 0.0, 0.0), 3u).x - atd_curl(p - vec3f(h, 0.0, 0.0), 3u).x
        + atd_curl(p + vec3f(0.0, h, 0.0), 3u).y - atd_curl(p - vec3f(0.0, h, 0.0), 3u).y
        + atd_curl(p + vec3f(0.0, 0.0, h), 3u).z - atd_curl(p - vec3f(0.0, 0.0, h), 3u).z) / (2.0 * h);
    out[i][0] = n;
    out[i][1] = vec4f(dx, dy, dz, div);
    out[i][2] = vec4f(atd_curl(p, 3u), 0.0);
}}
",
        atd_gpu::geo::wgsl::HASH,
        atd_gpu::geo::wgsl::NOISE
    );
    let kernel = Kernel::new(
        &device,
        "noise-test",
        &src,
        "main",
        &[Bind::Read, Bind::ReadWrite],
    );
    let n = 512;
    let pts: Vec<u32> = lcg(n * 4, 5)
        .chunks_exact(4)
        .flat_map(|c| {
            // Points in [-4, 4)³, avoiding exact lattice planes.
            let f = |v: u32| (v % 80_000) as f32 / 10_000.0 - 4.0 + 0.000_37;
            [f(c[0]), f(c[1]), f(c[2]), 0.0].map(f32::to_bits)
        })
        .collect();
    let pts_buf = buffer(&device, &pts);
    let out = buffer(&device, &vec![0; n * 12]);
    let mut encoder = device.create_command_encoder(&Default::default());
    kernel.dispatch(
        &device,
        &mut encoder,
        &[&pts_buf, &out],
        Work::Groups(n as u32 / 64),
    );
    queue.submit([encoder.finish()]);
    let r: Vec<f32> = read(&device, &queue, &out, n * 12)
        .into_iter()
        .map(f32::from_bits)
        .collect();
    let (mut worst_grad, mut worst_div, mut range, mut curl_max) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for s in r.chunks_exact(12) {
        let (value, grad, fd, div) = (s[0], &s[1..4], &s[4..7], s[7]);
        for (g, f) in grad.iter().zip(fd) {
            worst_grad = worst_grad.max((g - f).abs());
        }
        worst_div = worst_div.max(div.abs());
        range = range.max(value.abs());
        curl_max = curl_max.max(s[8..11].iter().map(|c| c * c).sum::<f32>().sqrt());
    }
    eprintln!(
        "noise: |value| ≤ {range}, gradient error ≤ {worst_grad}, |div curl| ≤ {worst_div}, |curl| ≤ {curl_max}"
    );
    assert!(range > 0.1 && range < 1.5, "value range {range}");
    assert!(worst_grad < 2e-2, "analytic gradient off by {worst_grad}");
    assert!(worst_div < 2e-2, "curl divergence {worst_div}");
    assert!(curl_max > 0.1, "the curl field is not zero");
}

#[test]
fn scan_matches_the_cpu_at_every_level_count() {
    let Some((device, queue)) = gpu() else { return };
    for n in [1usize, 200, 256, 257, 5000, 65_536, 70_001] {
        let values: Vec<u32> = lcg(n, n as u32).iter().map(|v| v % 4).collect();
        let src = buffer(&device, &values);
        let dst = buffer(&device, &vec![0; n]);
        let scan = Scan::new(&device, n as u32);
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut pass = atd_gpu::geo::kernels::pass(&mut encoder);
        let total = scan.record(&device, &mut pass, &src, &dst).clone();
        drop(pass);
        queue.submit([encoder.finish()]);
        let mut expected = Vec::with_capacity(n);
        let mut sum = 0u32;
        for v in &values {
            expected.push(sum);
            sum += v;
        }
        assert_eq!(read(&device, &queue, &dst, n), expected, "n = {n}");
        assert_eq!(read(&device, &queue, &total, 1), [sum], "n = {n}");
    }
}

#[test]
fn compaction_keeps_order_and_gather_moves_values() {
    let Some((device, queue)) = gpu() else { return };
    let n = 70_001usize;
    let flags: Vec<u32> = lcg(n, 9).iter().map(|v| u32::from(v % 3 != 0)).collect();
    let kept: Vec<u32> = (0..n as u32).filter(|&i| flags[i as usize] == 1).collect();
    let flag_buf = buffer(&device, &flags);
    let hdr = buffer(&device, &[0; 12]);
    let compactor = Compactor::new(&device, n as u32);
    let gather = Gather::new(&device);
    // Values of 4, 8 and 16 bytes.
    let v4: Vec<u32> = (0..n as u32).map(|i| i * 7).collect();
    let v16: Vec<u32> = (0..n as u32 * 4).collect();
    let v8: Vec<u32> = (0..n as u32 * 2).map(|i| i ^ 0xabcd).collect();
    let (s4, s8, s16) = (
        buffer(&device, &v4),
        buffer(&device, &v8),
        buffer(&device, &v16),
    );
    let (d4, d8, d16) = (
        buffer(&device, &vec![0; n]),
        buffer(&device, &vec![0; n * 2]),
        buffer(&device, &vec![0; n * 4]),
    );
    let append = 1000;
    let mut encoder = device.create_command_encoder(&Default::default());
    let mut pass = atd_gpu::geo::kernels::pass(&mut encoder);
    compactor.record(&device, &queue, &mut pass, &flag_buf, append, &hdr);
    let map = GatherMap {
        header: &hdr,
        map: compactor.map(),
    };
    gather.record(&device, &mut pass, map, 4, &s4, &d4);
    gather.record(&device, &mut pass, map, 8, &s8, &d8);
    gather.record(&device, &mut pass, map, 16, &s16, &d16);
    drop(pass);
    queue.submit([encoder.finish()]);

    let h = read(&device, &queue, &hdr, 12);
    let k = kept.len() as u32;
    assert_eq!(h[0], k + append, "count = kept + appended");
    assert_eq!((h[1], h[2]), (n as u32, k), "capacity, mapped");
    let d = header::DRAW_OFFSET as usize / 4;
    assert_eq!(&h[d..d + 2], &[4, k + append]);
    let x = header::DISPATCH_OFFSET as usize / 4;
    assert_eq!(h[x], (k + append).div_ceil(256));
    assert_eq!(read(&device, &queue, compactor.map(), kept.len()), kept);
    let got4 = read(&device, &queue, &d4, kept.len());
    assert_eq!(
        got4,
        kept.iter().map(|&i| v4[i as usize]).collect::<Vec<_>>()
    );
    let got8 = read(&device, &queue, &d8, kept.len() * 2);
    let want8: Vec<u32> = kept
        .iter()
        .flat_map(|&i| [v8[i as usize * 2], v8[i as usize * 2 + 1]])
        .collect();
    assert_eq!(got8, want8);
    let got16 = read(&device, &queue, &d16, kept.len() * 4);
    let want16: Vec<u32> = kept
        .iter()
        .flat_map(|&i| (0..4).map(move |c| i * 4 + c))
        .collect();
    assert_eq!(got16, want16);

    // Appending past the capacity is clamped.
    let all = buffer(&device, &vec![1; n]);
    let mut encoder = device.create_command_encoder(&Default::default());
    let mut pass = atd_gpu::geo::kernels::pass(&mut encoder);
    compactor.record(&device, &queue, &mut pass, &all, 5, &hdr);
    drop(pass);
    queue.submit([encoder.finish()]);
    let h = read(&device, &queue, &hdr, 3);
    assert_eq!(h, [n as u32, n as u32, n as u32]);
}
