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

#[test]
fn scan_matches_the_cpu_at_every_level_count() {
    let Some((device, queue)) = gpu() else { return };
    for n in [1usize, 200, 256, 257, 5000, 65_536, 70_001] {
        let values: Vec<u32> = lcg(n, n as u32).iter().map(|v| v % 4).collect();
        let src = buffer(&device, &values);
        let dst = buffer(&device, &vec![0; n]);
        let scan = Scan::new(&device, n as u32);
        let mut encoder = device.create_command_encoder(&Default::default());
        let total = scan.record(&device, &mut encoder, &src, &dst).clone();
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
    compactor.record(&device, &queue, &mut encoder, &flag_buf, append, &hdr);
    let map = GatherMap {
        header: &hdr,
        map: compactor.map(),
    };
    gather.record(&device, &mut encoder, map, 4, &s4, &d4);
    gather.record(&device, &mut encoder, map, 8, &s8, &d8);
    gather.record(&device, &mut encoder, map, 16, &s16, &d16);
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
    compactor.record(&device, &queue, &mut encoder, &all, 5, &hdr);
    queue.submit([encoder.finish()]);
    let h = read(&device, &queue, &hdr, 3);
    assert_eq!(h, [n as u32, n as u32, n as u32]);
}
