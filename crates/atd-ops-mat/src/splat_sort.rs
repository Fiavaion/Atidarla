//! A stable GPU radix sort of `u32` keys carrying `u32` values, for Splat
//! MAT's back-to-front order (FR-C1 "sort-on-GPU").
//!
//! Least-significant-digit first, 8 bits per pass (one to four passes,
//! as wide as the keys), in WGSL with no subgroup operations (so it runs on
//! every adapter wgpu offers). Each pass:
//!
//! 1. **histogram:** each workgroup counts the digits of its tile of
//!    [`TILE`] keys, written digit-major (`hist[digit * tiles + tile]`);
//! 2. **scan:** an exclusive prefix sum over that table (blocks of 1024, the
//!    block sums scanned by one workgroup, added back), which makes each
//!    entry the first output slot of its digit in its tile;
//! 3. **scatter:** each workgroup walks its tile in order, 256 keys at a
//!    time; a key's rank among the lower lanes with the same digit comes
//!    from the digits packed four to a word in workgroup memory, so equal
//!    keys keep their order (stable, hence deterministic).
//!
//! Depth keys share their high digits, so neither step lets many lanes
//! hit one counter: the histogram adds runs of a digit at once, and the
//! scatter's per-digit counts are written by each digit's last lane.
//!
//! Buffers grow with the largest count seen (warm-up) and are reused;
//! a steady frame allocates nothing (rule 2).

use atd_gpu::geo::kernels::{self, Bind, Kernel, Uniforms, Work};

/// Keys per workgroup tile.
pub const TILE: u32 = 4096;
/// The most keys one sort takes: the scan covers 1024 × 1024 table entries.
pub const MAX_KEYS: u32 = 1024 * 1024 / 256 * TILE;
const MAX_PASSES: usize = 4;

const COMMON: &str = r#"
struct SortParams {
    n: u32,
    shift: u32,
    tiles: u32,
    len: u32,
};
const TILE: u32 = 4096u;
const ITEMS: u32 = 16u;
@group(0) @binding(0) var<uniform> u: SortParams;
"#;

const HISTOGRAM: &str = r#"
@group(0) @binding(1) var<storage, read> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> hist: array<u32>;
var<workgroup> h: array<atomic<u32>, 256>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3u) {
    atomicStore(&h[li], 0u);
    workgroupBarrier();
    let base = wg.x * TILE;
    // Runs of one digit count as one atomic: depth keys share their high
    // digits, and 256 lanes adding to one bin would serialise.
    var run_digit = 256u;
    var run = 0u;
    for (var k = 0u; k < ITEMS; k++) {
        let i = base + k * 256u + li;
        if (i < u.n) {
            let d = (keys[i] >> u.shift) & 255u;
            if (d == run_digit) {
                run += 1u;
            } else {
                if (run > 0u) { atomicAdd(&h[run_digit], run); }
                run_digit = d;
                run = 1u;
            }
        }
    }
    if (run > 0u) { atomicAdd(&h[run_digit], run); }
    workgroupBarrier();
    hist[li * u.tiles + wg.x] = atomicLoad(&h[li]);
}
"#;

const SCAN: &str = r#"
@group(0) @binding(1) var<storage, read_write> data: array<u32>;
@group(0) @binding(2) var<storage, read_write> sums: array<u32>;
var<workgroup> t: array<u32, 256>;

// Exclusive prefix sum of `u.len` entries in blocks of 1024; each block's
// total goes to `sums`.
@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3u) {
    let base = wg.x * 1024u + li * 4u;
    var v: array<u32, 4>;
    var s = 0u;
    for (var k = 0u; k < 4u; k++) {
        var x = 0u;
        if (base + k < u.len) { x = data[base + k]; }
        v[k] = s;
        s += x;
    }
    t[li] = s;
    workgroupBarrier();
    for (var off = 1u; off < 256u; off = off << 1u) {
        var add = 0u;
        if (li >= off) { add = t[li - off]; }
        workgroupBarrier();
        t[li] += add;
        workgroupBarrier();
    }
    let prefix = t[li] - s;
    for (var k = 0u; k < 4u; k++) {
        if (base + k < u.len) { data[base + k] = v[k] + prefix; }
    }
    if (li == 255u) { sums[wg.x] = t[255u]; }
}
"#;

const ADD: &str = r#"
@group(0) @binding(1) var<storage, read_write> data: array<u32>;
@group(0) @binding(2) var<storage, read> sums: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i < u.len) { data[i] += sums[i / 1024u]; }
}
"#;

const SCATTER: &str = r#"
@group(0) @binding(1) var<storage, read> keys_in: array<u32>;
@group(0) @binding(2) var<storage, read> vals_in: array<u32>;
@group(0) @binding(3) var<storage, read> hist: array<u32>;
@group(0) @binding(4) var<storage, read_write> keys_out: array<u32>;
@group(0) @binding(5) var<storage, read_write> vals_out: array<u32>;
var<workgroup> offs: array<u32, 256>;
var<workgroup> cnt: array<u32, 256>;
var<workgroup> one: array<u32, 256>;
var<workgroup> dig: array<u32, 64>;

// Bytes of `x` that are zero.
fn zero_bytes(x: u32) -> u32 {
    return countOneBits(~(((x & 0x7f7f7f7fu) + 0x7f7f7f7fu) | x | 0x7f7f7f7fu));
}

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3u) {
    offs[li] = hist[li * u.tiles + wg.x];
    let base = wg.x * TILE;
    for (var k = 0u; k < ITEMS; k++) {
        cnt[li] = 0u;
        let i = base + k * 256u + li;
        let valid = i < u.n;
        var key = 0u;
        // Lanes past the end sit above every valid lane, so their digit
        // is never counted.
        var d = 255u;
        if (valid) {
            key = keys_in[i];
            d = (key >> u.shift) & 255u;
        }
        one[li] = d;
        workgroupBarrier();
        if (li < 64u) {
            let o = li * 4u;
            dig[li] = one[o] | (one[o + 1u] << 8u) | (one[o + 2u] << 16u) | (one[o + 3u] << 24u);
        }
        workgroupBarrier();
        // Lanes with this digit below this one (the rank) and in all (the
        // count), over the valid lanes; the last lane of each digit writes
        // its count, so no atomics contend on a common digit.
        let pattern = d * 0x01010101u;
        let word = li >> 2u;
        let lane = li & 3u;
        let valid_lanes = min(u.n - min(u.n, base + k * 256u), 256u);
        var rank = 0u;
        var total = 0u;
        for (var w = 0u; w < 64u; w++) {
            var x = dig[w] ^ pattern;
            let first = w * 4u;
            if (first + 4u > valid_lanes) {
                let keep = select(0u, valid_lanes - first, valid_lanes > first);
                x = x | (0xffffffffu << (keep * 8u));
            }
            let c = zero_bytes(x);
            total += c;
            if (w < word) {
                rank += c;
            } else if (w == word && lane > 0u) {
                rank += zero_bytes(x | (0xffffffffu << (lane * 8u)));
            }
        }
        if (valid) {
            let dst = offs[d] + rank;
            keys_out[dst] = key;
            vals_out[dst] = vals_in[i];
            if (rank + 1u == total) { cnt[d] = total; }
        }
        workgroupBarrier();
        offs[li] += cnt[li];
        workgroupBarrier();
    }
}
"#;

/// Scratch of one capacity.
#[derive(Debug)]
struct Scratch {
    capacity: u32,
    keys: wgpu::Buffer,
    vals: wgpu::Buffer,
    hist: wgpu::Buffer,
    sums: wgpu::Buffer,
    sums2: wgpu::Buffer,
}

/// The sorter: pipelines, per-pass uniforms and scratch buffers.
#[derive(Debug)]
pub struct RadixSort {
    histogram: Kernel,
    scan: Kernel,
    add: Kernel,
    scatter: Kernel,
    /// One per pass (the digit shift differs), and the block-sum scan.
    uniforms: [Uniforms; MAX_PASSES + 1],
    scratch: Option<Scratch>,
}

impl RadixSort {
    pub fn new(device: &wgpu::Device) -> Self {
        let kernel = |label, body: &str, binds: &[Bind]| {
            Kernel::new(device, label, &format!("{COMMON}{body}"), "main", binds)
        };
        use Bind::{Read, ReadWrite, Uniform};
        Self {
            histogram: kernel(
                "splat.sort.histogram",
                HISTOGRAM,
                &[Uniform, Read, ReadWrite],
            ),
            scan: kernel("splat.sort.scan", SCAN, &[Uniform, ReadWrite, ReadWrite]),
            add: kernel("splat.sort.add", ADD, &[Uniform, ReadWrite, Read]),
            scatter: kernel(
                "splat.sort.scatter",
                SCATTER,
                &[Uniform, Read, Read, Read, ReadWrite, ReadWrite],
            ),
            uniforms: std::array::from_fn(|_| Uniforms::new(device, "splat.sort", 16)),
            scratch: None,
        }
    }

    fn scratch(&mut self, device: &wgpu::Device, n: u32) -> &Scratch {
        if self.scratch.as_ref().is_none_or(|s| s.capacity < n) {
            let capacity = n.next_power_of_two().max(TILE);
            let tiles = capacity.div_ceil(TILE);
            let buffer = |label, bytes: u64| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: bytes.max(16),
                    // Copied from by tests that read the sorted values back.
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                })
            };
            self.scratch = Some(Scratch {
                capacity,
                keys: buffer("splat.sort.keys", u64::from(capacity) * 4),
                vals: buffer("splat.sort.vals", u64::from(capacity) * 4),
                hist: buffer("splat.sort.hist", u64::from(tiles) * 256 * 4),
                sums: buffer("splat.sort.sums", 1024 * 4),
                sums2: buffer("splat.sort.sums2", 16),
            });
        }
        self.scratch.as_ref().expect("made above")
    }

    /// Records a stable ascending sort of the first `n` keys of `keys`
    /// (carrying `values`) into `encoder`, keys of `bits` bits (1–32; higher
    /// bits must be zero). Both buffers need `STORAGE` usage and room for
    /// `n` words. Returns the buffer holding the sorted values: `values`
    /// after an even number of 8-bit passes, else this sorter's scratch
    /// (valid until the next sort of more keys than ever before).
    #[allow(clippy::too_many_arguments)]
    pub fn sort<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        keys: &'a wgpu::Buffer,
        values: &'a wgpu::Buffer,
        n: u32,
        bits: u32,
    ) -> Result<&'a wgpu::Buffer, String> {
        if n > MAX_KEYS {
            return Err(format!("{n} keys; the sort takes at most {MAX_KEYS}"));
        }
        if n == 0 {
            return Ok(values);
        }
        let passes = bits.clamp(1, 32).div_ceil(8) as usize;
        let tiles = n.div_ceil(TILE);
        let len = tiles * 256;
        let blocks = len.div_ceil(1024);
        for (p, u) in self.uniforms[..passes].iter().enumerate() {
            u.write(queue, &kernels::words([n, 8 * p as u32, tiles, len]));
        }
        self.uniforms[MAX_PASSES].write(queue, &kernels::words([n, 0, tiles, blocks]));
        self.scratch(device, n);
        let s = self.scratch.as_ref().expect("made above");
        let mut pass = kernels::pass(encoder);
        for (p, u) in self.uniforms[..passes].iter().enumerate() {
            let (src, dst) = if p % 2 == 0 {
                ((keys, values), (&s.keys, &s.vals))
            } else {
                ((&s.keys, &s.vals), (keys, values))
            };
            let u = u.buffer();
            self.histogram.dispatch_in(
                device,
                &mut pass,
                &[u, src.0, &s.hist],
                Work::Groups(tiles),
            );
            self.scan.dispatch_in(
                device,
                &mut pass,
                &[u, &s.hist, &s.sums],
                Work::Groups(blocks),
            );
            self.scan.dispatch_in(
                device,
                &mut pass,
                &[self.uniforms[MAX_PASSES].buffer(), &s.sums, &s.sums2],
                Work::Groups(1),
            );
            self.add.dispatch_in(
                device,
                &mut pass,
                &[u, &s.hist, &s.sums],
                Work::Elements(len),
            );
            self.scatter.dispatch_in(
                device,
                &mut pass,
                &[u, src.0, src.1, &s.hist, dst.0, dst.1],
                Work::Groups(tiles),
            );
        }
        Ok(if passes.is_multiple_of(2) { values } else { &s.vals })
    }

    /// Bind groups created so far (a steady frame must not raise it).
    #[cfg(test)]
    fn bind_groups_created(&self) -> usize {
        [&self.histogram, &self.scan, &self.add, &self.scatter]
            .iter()
            .map(|k| k.bind_groups_created())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
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
            eprintln!("skipping: no GPU adapter");
            return None;
        };
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: atd_gpu::required_limits(&adapter),
            ..Default::default()
        }))
        .ok()
    }

    fn upload(device: &wgpu::Device, queue: &wgpu::Queue, words: &[u32]) -> wgpu::Buffer {
        let b = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (words.len() as u64 * 4).max(16),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        queue.write_buffer(&b, 0, &bytes);
        b
    }

    fn download(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        b: &wgpu::Buffer,
        n: usize,
    ) -> Vec<u32> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (n as u64 * 4).max(16),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(b, 0, &staging, 0, (n as u64 * 4).max(16));
        queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("mapped"));
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = slice.get_mapped_range().expect("mapped");
        data.chunks_exact(4)
            .take(n)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn the_gpu_sort_matches_a_stable_cpu_sort() {
        let Some((device, queue)) = gpu() else { return };
        let mut sorter = RadixSort::new(&device);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u32
        };
        // Sizes around tile and block edges; keys full-range, clustered
        // (many equal keys) and float-depth-like.
        for (n, kind, bits) in [
            (1usize, 0, 32),
            (255, 0, 32),
            (4096, 1, 32),
            (4097, 2, 24),
            (100_003, 0, 32),
            (100_003, 1, 8),
            (100_003, 0, 16),
            (1 << 20, 2, 24),
        ] {
            let mask = if bits == 32 {
                u32::MAX
            } else {
                (1 << bits) - 1
            };
            let keys: Vec<u32> = (0..n)
                .map(|i| match kind {
                    0 => rand(),
                    1 => rand() % 37 * 0x0101_0101,
                    _ => (u32::MAX - (1.0f32 + (i * 7919 % n) as f32 * 0.001).to_bits()) >> 8,
                } & mask)
                .collect();
            let vals: Vec<u32> = (0..n as u32).collect();
            let kb = upload(&device, &queue, &keys);
            let vb = upload(&device, &queue, &vals);
            let mut enc = device.create_command_encoder(&Default::default());
            let out = sorter
                .sort(&device, &queue, &mut enc, &kb, &vb, n as u32, bits)
                .unwrap()
                .clone();
            queue.submit([enc.finish()]);
            let got_v = download(&device, &queue, &out, n);
            let got_k: Vec<u32> = got_v.iter().map(|&v| keys[v as usize]).collect();
            let mut want: Vec<(u32, u32)> = keys.iter().copied().zip(vals).collect();
            want.sort_by_key(|p| p.0);
            let (want_k, want_v): (Vec<u32>, Vec<u32>) = want.into_iter().unzip();
            assert_eq!(got_k, want_k, "keys, n = {n}, kind {kind}, {bits} bits");
            assert_eq!(
                got_v, want_v,
                "values (stable), n = {n}, kind {kind}, {bits} bits"
            );
        }
        // Sorting the same buffers again creates nothing.
        let kb = upload(&device, &queue, &[3, 1, 2]);
        let vb = upload(&device, &queue, &[0, 1, 2]);
        let mut created = 0;
        for frame in 0..3 {
            let mut enc = device.create_command_encoder(&Default::default());
            sorter
                .sort(&device, &queue, &mut enc, &kb, &vb, 3, 32)
                .unwrap();
            queue.submit([enc.finish()]);
            if frame == 0 {
                created = sorter.bind_groups_created();
            }
        }
        assert_eq!(sorter.bind_groups_created(), created);
    }
}
