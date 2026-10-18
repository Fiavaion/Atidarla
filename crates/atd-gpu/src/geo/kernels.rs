//! Compute building blocks for GEO operators: a compute [`Kernel`] that
//! caches its bind groups (steady-state frames create no GPU objects, rule 2),
//! an exclusive prefix sum ([`Scan`]), stream compaction ([`Compactor`]) and
//! the generic [`Gather`] that moves any attribute by an index map (design
//! §2.1, §4.3).
//!
//! **Determinism** (design §4.3): no atomics decide where an element goes.
//! Compaction scans integer flags with a multi-level reduce-then-scan (no
//! single-pass look-back, which needs forward-progress guarantees WebGPU and
//! Metal do not give), so survivors keep their order on every run.

use std::sync::Mutex;

use atd_core::geo::{WORKGROUP, header, workgroups};

/// Bind groups kept per kernel: pooled buffers alternate between a few
/// generations, so a steady state hits the cache.
const CACHE: usize = 32;
/// Entries unused for this many dispatches are dropped (they keep their
/// buffers alive).
const STALE: u64 = 4 * CACHE as u64;

#[derive(Debug, Default)]
struct BindCache {
    /// (buffers, bind group, last use), most recently used last.
    entries: Vec<(Vec<wgpu::Buffer>, wgpu::BindGroup, u64)>,
    uses: u64,
    created: usize,
}

/// How many workgroups a dispatch runs.
#[derive(Debug, Clone, Copy)]
pub enum Work<'a> {
    /// A fixed number of workgroups.
    Groups(u32),
    /// Enough workgroups for `n` elements ([`WORKGROUP`] per group).
    Elements(u32),
    /// `dispatch_workgroups_indirect` from a geometry header (its count).
    Header(&'a wgpu::Buffer),
}

/// One binding of a [`Kernel`]'s group 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// `var<uniform>`.
    Uniform,
    /// `var<storage, read>`.
    Read,
    /// `var<storage, read_write>`.
    ReadWrite,
}

/// A compute pipeline whose group 0 is bound to whole buffers, in binding
/// order, with an explicit layout (a binding the shader does not use is
/// still bound).
#[derive(Debug)]
pub struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    cache: Mutex<BindCache>,
}

impl Kernel {
    /// Compiles `source` with entry point `entry`; `binds` lists group 0's
    /// bindings 0, 1, … in order.
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        source: &str,
        entry: &str,
        binds: &[Bind],
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let entries: Vec<wgpu::BindGroupLayoutEntry> = binds
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupLayoutEntry {
                binding: i as u32,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: match b {
                        Bind::Uniform => wgpu::BufferBindingType::Uniform,
                        Bind::Read => wgpu::BufferBindingType::Storage { read_only: true },
                        Bind::ReadWrite => wgpu::BufferBindingType::Storage { read_only: false },
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            layout,
            cache: Mutex::default(),
        }
    }

    /// Bind groups created so far (a steady-state frame must not raise it).
    pub fn bind_groups_created(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).created
    }

    /// Records one dispatch with `buffers` bound to group 0, bindings 0, 1, ….
    pub fn dispatch(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        buffers: &[&wgpu::Buffer],
        work: Work<'_>,
    ) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let cache = &mut *cache;
        cache.uses += 1;
        let now = cache.uses;
        cache.entries.retain(|e| now - e.2 <= STALE);
        let bind = match cache
            .entries
            .iter()
            .position(|(key, _, _)| key.iter().eq(buffers.iter().copied()))
        {
            Some(i) => {
                let mut hit = cache.entries.remove(i);
                hit.2 = now;
                cache.entries.push(hit);
                &cache.entries.last().expect("pushed").1
            }
            None => {
                let entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
                    .iter()
                    .enumerate()
                    .map(|(i, b)| wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: b.as_entire_binding(),
                    })
                    .collect();
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &entries,
                });
                cache.created += 1;
                if cache.entries.len() == CACHE {
                    cache.entries.remove(0);
                }
                let key = buffers.iter().map(|b| (*b).clone()).collect();
                cache.entries.push((key, bind, now));
                &cache.entries.last().expect("pushed").1
            }
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        match work {
            Work::Groups(n) => pass.dispatch_workgroups(n.max(1), 1, 1),
            Work::Elements(n) => pass.dispatch_workgroups(workgroups(n).max(1), 1, 1),
            Work::Header(h) => pass.dispatch_workgroups_indirect(h, header::DISPATCH_OFFSET),
        }
    }
}

/// A small uniform buffer of `u32` words, written only when they change.
#[derive(Debug)]
pub struct Uniforms {
    buffer: wgpu::Buffer,
    last: Mutex<Vec<u8>>,
}

impl Uniforms {
    /// `bytes` of uniforms (rounded up to 16).
    pub fn new(device: &wgpu::Device, label: &str, bytes: u64) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.next_multiple_of(16),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            last: Mutex::new(Vec::new()),
        }
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Writes `data` (little-endian words) if it differs from the last write.
    /// Note: one buffer per dispatch site; writing twice in one frame makes
    /// every dispatch of that frame see the last values.
    pub fn write(&self, queue: &wgpu::Queue, data: &[u8]) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if *last != data {
            queue.write_buffer(&self.buffer, 0, data);
            last.clear();
            last.extend_from_slice(data);
        }
    }
}

/// Four `u32` words, little-endian (one `vec4u` of uniforms).
pub fn words(values: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (chunk, v) in out.chunks_exact_mut(4).zip(values) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// Packs uniform values little-endian into a fixed-size block without
/// allocating. Follow WGSL alignment yourself (a `vec3f`/`vec4f` starts at a
/// multiple of 16 bytes; [`Packer::vec4`] pads to it).
#[derive(Debug, Clone, Copy)]
pub struct Packer<const BYTES: usize> {
    buf: [u8; BYTES],
    at: usize,
}

impl<const BYTES: usize> Default for Packer<BYTES> {
    fn default() -> Self {
        Self {
            buf: [0; BYTES],
            at: 0,
        }
    }
}

impl<const BYTES: usize> Packer<BYTES> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf[self.at..self.at + 4].copy_from_slice(&v.to_le_bytes());
        self.at += 4;
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.u32(v.to_bits())
    }

    /// Pads to 16 bytes, then writes four floats.
    pub fn vec4(&mut self, v: [f32; 4]) -> &mut Self {
        self.at = self.at.next_multiple_of(16);
        for c in v {
            self.f32(c);
        }
        self
    }

    /// The whole block (unwritten bytes are zero).
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

const SCAN_WGSL: &str = r#"
struct P { n: u32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;
@group(0) @binding(3) var<storage, read_write> sums: array<u32>;
var<workgroup> tmp: array<u32, 256>;

// Exclusive scan of one block of 256; the block's total goes to sums.
@compute @workgroup_size(256)
fn scan_block(@builtin(local_invocation_id) lid: vec3u, @builtin(workgroup_id) wid: vec3u) {
    let i = wid.x * 256u + lid.x;
    var v = 0u;
    if (i < p.n) { v = src[i]; }
    tmp[lid.x] = v;
    workgroupBarrier();
    for (var off = 1u; off < 256u; off = off * 2u) {
        var t = 0u;
        if (lid.x >= off) { t = tmp[lid.x - off]; }
        workgroupBarrier();
        tmp[lid.x] = tmp[lid.x] + t;
        workgroupBarrier();
    }
    if (i < p.n) { dst[i] = tmp[lid.x] - v; }
    if (lid.x == 255u) { sums[wid.x] = tmp[255u]; }
}
"#;

const ADD_WGSL: &str = r#"
struct P { n: u32, pad0: u32, pad1: u32, pad2: u32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<storage, read> sums: array<u32>;

// Adds each block's scanned offset to its elements.
@compute @workgroup_size(256)
fn add_block(@builtin(global_invocation_id) gid: vec3u, @builtin(workgroup_id) wid: vec3u) {
    if (gid.x < p.n) { dst[gid.x] = dst[gid.x] + sums[wid.x]; }
}
"#;

#[derive(Debug)]
struct Level {
    n: u32,
    params: wgpu::Buffer,
    /// Scan output of this level (level 0 writes the caller's buffer).
    out: Option<wgpu::Buffer>,
    sums: wgpu::Buffer,
}

/// Exclusive prefix sum of `u32`s over a fixed capacity: multi-level
/// (256 per block), integer, deterministic. Buffers are made at creation;
/// recording allocates nothing on the GPU.
#[derive(Debug)]
pub struct Scan {
    capacity: u32,
    levels: Vec<Level>,
    block: Kernel,
    add: Kernel,
}

fn storage(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: atd_core::geo::align_up(bytes),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform(device: &wgpu::Device, label: &str, data: &[u8]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt as _;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: data,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

impl Scan {
    pub fn new(device: &wgpu::Device, capacity: u32) -> Self {
        let mut levels = Vec::new();
        let mut n = capacity.max(1);
        loop {
            let blocks = workgroups(n);
            levels.push(Level {
                n,
                params: uniform(device, "atd-scan-n", &words([n, 0, 0, 0])),
                out: (!levels.is_empty())
                    .then(|| storage(device, "atd-scan-out", u64::from(n) * 4)),
                sums: storage(device, "atd-scan-sums", u64::from(blocks) * 4),
            });
            if blocks == 1 {
                break;
            }
            n = blocks;
        }
        use Bind::*;
        Self {
            capacity,
            levels,
            block: Kernel::new(
                device,
                "atd-scan-block",
                SCAN_WGSL,
                "scan_block",
                &[Uniform, Read, ReadWrite, ReadWrite],
            ),
            add: Kernel::new(
                device,
                "atd-scan-add",
                ADD_WGSL,
                "add_block",
                &[Uniform, ReadWrite, Read],
            ),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Records `dst[i] = src[0] + … + src[i - 1]` for `i < capacity`; returns
    /// the buffer whose first word becomes the total.
    pub fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
    ) -> &wgpu::Buffer {
        for (k, level) in self.levels.iter().enumerate() {
            let input = if k == 0 {
                src
            } else {
                &self.levels[k - 1].sums
            };
            let output = level.out.as_ref().unwrap_or(dst);
            self.block.dispatch(
                device,
                encoder,
                &[&level.params, input, output, &level.sums],
                Work::Elements(level.n),
            );
        }
        for k in (0..self.levels.len() - 1).rev() {
            let level = &self.levels[k];
            let output = level.out.as_ref().unwrap_or(dst);
            let scanned = self.levels[k + 1]
                .out
                .as_ref()
                .expect("levels above 0 have outputs");
            self.add.dispatch(
                device,
                encoder,
                &[&level.params, output, scanned],
                Work::Elements(level.n),
            );
        }
        &self.levels.last().expect("at least one level").sums
    }
}

const COMPACT_WGSL: &str = r#"
struct P { n: u32, append: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> flags: array<u32>;
@group(0) @binding(2) var<storage, read> scan: array<u32>;
@group(0) @binding(3) var<storage, read_write> map: array<u32>;

// Kept element i goes to position scan[i]: the map lists kept indices in order.
@compute @workgroup_size(256)
fn build_map(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.x;
    if (i < p.n && flags[i] != 0u) { map[scan[i]] = i; }
}
"#;

const HEADER_WGSL: &str = r#"
struct P { capacity: u32, append: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> total: array<u32>;
@group(0) @binding(2) var<storage, read_write> hdr: GeoHeader;

// count = kept + appended (at most the capacity); arguments for the next passes.
@compute @workgroup_size(1)
fn finish_header() {
    let kept = min(total[0], p.capacity);
    let count = min(kept + min(p.append, p.capacity - kept), p.capacity);
    hdr.count = count;
    hdr.capacity = p.capacity;
    hdr.mapped = kept;
    hdr.pad = 0u;
    hdr.draw[0] = 4u;
    hdr.draw[1] = count;
    hdr.draw[2] = 0u;
    hdr.draw[3] = 0u;
    hdr.dispatch[0] = max(1u, (count + 255u) / 256u);
    hdr.dispatch[1] = 1u;
    hdr.dispatch[2] = 1u;
    hdr.dispatch[3] = 0u;
}
"#;

/// Stream compaction over a fixed capacity: from `0/1` flags, an index map
/// of the kept elements (in their original order) and a header whose count
/// is the kept elements plus room for `append` new ones (positions
/// `mapped..count`, filled by the caller). Gather each attribute through the
/// map with [`Gather`].
#[derive(Debug)]
pub struct Compactor {
    scan: Scan,
    scanned: wgpu::Buffer,
    map: wgpu::Buffer,
    build: Kernel,
    finish: Kernel,
    build_params: Uniforms,
    finish_params: Uniforms,
}

impl Compactor {
    pub fn new(device: &wgpu::Device, capacity: u32) -> Self {
        use Bind::*;
        let bytes = u64::from(capacity.max(1)) * 4;
        Self {
            scan: Scan::new(device, capacity),
            scanned: storage(device, "atd-compact-scan", bytes),
            map: storage(device, "atd-compact-map", bytes),
            build: Kernel::new(
                device,
                "atd-compact-map",
                COMPACT_WGSL,
                "build_map",
                &[Uniform, Read, Read, ReadWrite],
            ),
            finish: Kernel::new(
                device,
                "atd-compact-header",
                &format!("{}{HEADER_WGSL}", header::WGSL),
                "finish_header",
                &[Uniform, Read, ReadWrite],
            ),
            build_params: Uniforms::new(device, "atd-compact-map", 16),
            finish_params: Uniforms::new(device, "atd-compact-header", 16),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.scan.capacity()
    }

    /// The index map: entry `j < mapped` is the original index of the `j`-th
    /// kept element.
    pub fn map(&self) -> &wgpu::Buffer {
        &self.map
    }

    /// Records the compaction of `flags[..capacity]` into the map and
    /// `header` (a header-sized buffer, typically from the pool).
    pub fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        flags: &wgpu::Buffer,
        append: u32,
        header: &wgpu::Buffer,
    ) {
        let cap = self.capacity();
        let total = self.scan.record(device, encoder, flags, &self.scanned);
        self.build_params.write(queue, &words([cap, 0, 0, 0]));
        self.build.dispatch(
            device,
            encoder,
            &[self.build_params.buffer(), flags, &self.scanned, &self.map],
            Work::Elements(cap),
        );
        self.finish_params.write(queue, &words([cap, append, 0, 0]));
        self.finish.dispatch(
            device,
            encoder,
            &[self.finish_params.buffer(), total, header],
            Work::Groups(1),
        );
    }
}

/// A per-element operator on one 16-byte float attribute (`vec3f` or
/// `vec4f`, such as `P`, `v` or `Cd`): the building block of deformers.
///
/// The operator supplies WGSL declarations (`decl`: a `struct Params` and
/// helper functions; the shared library [`super::wgsl`] is prepended) and
/// the body of `fn deform(i: u32, value: vec4f) -> vec4f`, which may read
/// the uniform `params: Params` and `hdr: GeoHeader`. [`Deformer::apply`]
/// writes in place when the attribute is held uniquely (the engine handed
/// the geometry over), else copy-on-write into a pooled buffer; both
/// variants run the same `deform`, so they give identical results. Elements
/// at or beyond the header's count are not touched.
#[derive(Debug)]
pub struct Deformer {
    in_place: Kernel,
    copy: Kernel,
    params: Uniforms,
}

/// Whether [`Deformer::apply`] wrote in place or into a new buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    InPlace,
    Copy,
}

impl Deformer {
    /// `params_bytes`: size of `struct Params` (at least 16).
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        decl: &str,
        body: &str,
        params_bytes: u64,
    ) -> Self {
        let common = format!(
            "{}{}{}{decl}\n@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
fn deform(i: u32, value: vec4f) -> vec4f {{
{body}
}}
",
            super::wgsl::HASH,
            super::wgsl::NOISE,
            header::WGSL,
        );
        let in_place = format!(
            "{common}@group(0) @binding(2) var<storage, read_write> a: array<vec4f>;
@compute @workgroup_size({WORKGROUP})
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    if (i < hdr.count) {{ a[i] = deform(i, a[i]); }}
}}
"
        );
        let copy = format!(
            "{common}@group(0) @binding(2) var<storage, read> src: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> dst: array<vec4f>;
@compute @workgroup_size({WORKGROUP})
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    if (i < hdr.count) {{ dst[i] = deform(i, src[i]); }}
}}
"
        );
        use Bind::*;
        Self {
            in_place: Kernel::new(
                device,
                &format!("{label}-in-place"),
                &in_place,
                "main",
                &[Uniform, Read, ReadWrite],
            ),
            copy: Kernel::new(
                device,
                &format!("{label}-copy"),
                &copy,
                "main",
                &[Uniform, Read, Read, ReadWrite],
            ),
            params: Uniforms::new(device, label, params_bytes.max(16)),
        }
    }

    /// Bind groups created so far (both variants).
    pub fn bind_groups_created(&self) -> usize {
        self.in_place.bind_groups_created() + self.copy.bind_groups_created()
    }

    /// Runs `deform` over point attribute `name` of `geo` with `params`
    /// (the bytes of `struct Params`). The geometry must have a header and
    /// the attribute must be a 16-byte float type.
    pub fn apply(
        &self,
        ctx: &mut crate::engine::GeoCtx<'_>,
        geo: &mut super::Geometry,
        name: &str,
        params: &[u8],
    ) -> Result<Wrote, String> {
        let header = geo
            .header_handle()
            .cloned()
            .ok_or("the input geometry has no GPU header")?;
        let attr = geo
            .attr_mut(super::AttrClass::Point, name)
            .ok_or_else(|| format!("the input has no point attribute `{name}`"))?;
        let ty = attr.ty();
        if ty.scalar != super::Scalar::F32 || ty.stride() != 16 {
            return Err(format!("`{name}` is {ty}; expected vec3f or vec4f"));
        }
        self.params.write(ctx.queue, params);
        let hdr = header.buffer();
        if let Some(buffer) = attr.unique_gpu() {
            let buffer = buffer.clone();
            self.in_place.dispatch(
                ctx.device,
                ctx.encoder,
                &[self.params.buffer(), hdr, &buffer],
                Work::Header(hdr),
            );
            return Ok(Wrote::InPlace);
        }
        let src = attr.ensure_gpu(ctx.queue, ctx.buffers).clone();
        let (len, name) = (attr.len(), attr.name_arc().clone());
        let out = ctx.alloc(name, ty, len);
        let dst = out.gpu_buffer().expect("allocated on the GPU").clone();
        self.copy.dispatch(
            ctx.device,
            ctx.encoder,
            &[self.params.buffer(), hdr, &src, &dst],
            Work::Header(hdr),
        );
        geo.set_point(out);
        Ok(Wrote::Copy)
    }
}

/// The index map of a gather: `header.mapped` entries of `map` (for example
/// a [`Compactor`]'s map and the header it wrote).
#[derive(Debug, Clone, Copy)]
pub struct GatherMap<'a> {
    pub header: &'a wgpu::Buffer,
    pub map: &'a wgpu::Buffer,
}

/// `dst[j] = src[map[j]]` for `j < header.mapped`, for elements of 4, 8 or
/// 16 bytes: one kernel moves any attribute, with four bindings whatever the
/// attribute count (design §2.1). Dispatched from the header.
#[derive(Debug)]
pub struct Gather {
    kernels: [Kernel; 3],
}

impl Gather {
    pub fn new(device: &wgpu::Device) -> Self {
        let kernel = |ty: &str, label: &str| {
            let src = format!(
                "{}\n@group(0) @binding(0) var<storage, read> hdr: GeoHeader;
@group(0) @binding(1) var<storage, read> map: array<u32>;
@group(0) @binding(2) var<storage, read> src: array<{ty}>;
@group(0) @binding(3) var<storage, read_write> dst: array<{ty}>;
@compute @workgroup_size({WORKGROUP})
fn gather(@builtin(global_invocation_id) gid: vec3u) {{
    let j = gid.x;
    if (j < hdr.mapped) {{ dst[j] = src[map[j]]; }}
}}
",
                header::WGSL
            );
            use Bind::*;
            Kernel::new(
                device,
                label,
                &src,
                "gather",
                &[Read, Read, Read, ReadWrite],
            )
        };
        Self {
            kernels: [
                kernel("u32", "atd-gather-4"),
                kernel("vec2u", "atd-gather-8"),
                kernel("vec4u", "atd-gather-16"),
            ],
        }
    }

    /// Records the gather of one attribute of `stride` bytes (4, 8 or 16)
    /// from `src` into `dst` through `map`.
    pub fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        map: GatherMap<'_>,
        stride: u32,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
    ) {
        let kernel = match stride {
            4 => &self.kernels[0],
            8 => &self.kernels[1],
            16 => &self.kernels[2],
            _ => panic!("attribute strides are 4, 8 or 16 bytes"),
        };
        kernel.dispatch(
            device,
            encoder,
            &[map.header, map.map, src, dst],
            Work::Header(map.header),
        );
    }

    /// Bind groups created by the three kernels so far.
    pub fn bind_groups_created(&self) -> usize {
        self.kernels.iter().map(Kernel::bind_groups_created).sum()
    }
}
