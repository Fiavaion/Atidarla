//! The GPU half of output mapping: one render pipeline drawing every
//! slice's tessellated mesh, sampling the source (or a test pattern), with
//! soft edges and masks in the fragment shader.

use super::{MAX_MASK_POINTS, MAX_SLICES, OutputMap, Pattern, VERTEX_BYTES, tessellate};

const SHADER: &str = r#"
struct Slice { blend: vec4f, curve: vec4f, extra: vec4f };
struct U {
    // Output width, height; pattern; 1 when a mask image is wired.
    size: vec4f,
    slices: array<Slice, 16>,
    // Mask polygon points, two per vec4f.
    mask: array<vec4f, 256>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var mask_img: texture_2d<f32>;

struct VsIn {
    @location(0) pos: vec2f,
    @location(1) uv: vec2f,
    @location(2) local: vec2f,
    @location(3) slice: u32,
};
struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
    @location(1) local: vec2f,
    @location(2) @interpolate(flat) slice: u32,
};

@vertex
fn vs(v: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = vec4f(v.pos.x * 2.0 - 1.0, 1.0 - v.pos.y * 2.0, 0.0, 1.0);
    o.uv = v.uv;
    o.local = v.local;
    o.slice = v.slice;
    return o;
}

fn ramp(t: f32, power: f32, gamma: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    var s = 1.0 - 0.5 * pow(2.0 * (1.0 - x), power);
    if (x < 0.5) { s = 0.5 * pow(2.0 * x, power); }
    return pow(s, 1.0 / max(gamma, 0.01));
}

fn edge(d: f32, width: f32, power: f32, gamma: f32) -> f32 {
    if (width <= 0.0) { return 1.0; }
    return ramp(d / width, power, gamma);
}

// 3×5 digits, rows top to bottom, three bits per row (left bit first).
const DIGITS = array<u32, 10>(31599u, 11415u, 29671u, 29647u, 23497u, 31183u, 31215u, 29257u, 31727u, 31695u);

fn digit_on(d: u32, cx: i32, cy: i32) -> bool {
    if (cx < 0 || cx > 2 || cy < 0 || cy > 4) { return false; }
    return ((DIGITS[d] >> u32(14 - (cy * 3 + cx))) & 1u) == 1u;
}

fn hue(h: f32) -> vec3f {
    return clamp(abs(fract(h + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
}

fn pattern(kind: i32, l: vec2f, fw: vec2f, id: u32) -> vec4f {
    switch kind {
        case 1: {
            // Grid: 8×8 cells, a centre cross and a border.
            let g = abs(fract(l * 8.0 + 0.5) - 0.5) / (fw * 8.0);
            let line = 1.0 - clamp(min(g.x, g.y) - 0.5, 0.0, 1.0);
            let c = abs(l - 0.5) / fw;
            let cross = 1.0 - clamp(min(c.x, c.y) - 1.5, 0.0, 1.0);
            let b = min(l, 1.0 - l) / fw;
            let border = 1.0 - clamp(min(b.x, b.y) - 3.0, 0.0, 1.0);
            var col = mix(vec3f(0.12), vec3f(0.85), line);
            col = mix(col, vec3f(1.0, 0.85, 0.1), cross);
            col = mix(col, hue(f32(id) * 0.13), border);
            return vec4f(col, 1.0);
        }
        case 2: {
            // 75% bars: white, yellow, cyan, green, magenta, red, blue, black.
            let i = u32(clamp(floor(l.x * 8.0), 0.0, 7.0));
            let r = select(0.0, 0.75, i == 0u || i == 1u || i == 4u || i == 5u);
            let g = select(0.0, 0.75, i <= 3u);
            let b = select(0.0, 0.75, i == 0u || i == 2u || i == 4u || i == 6u);
            return vec4f(r, g, b, 1.0);
        }
        case 3: {
            return vec4f(vec3f(clamp(l.x, 0.0, 1.0)), 1.0);
        }
        case 4: {
            // Circles on the slice's colour, with its number (1-based).
            let d = length(l - 0.5);
            let ring = abs(fract(d * 10.0 + 0.5) - 0.5) / (max(fw.x, fw.y) * 10.0);
            let on = 1.0 - clamp(ring - 0.75, 0.0, 1.0);
            var col = mix(hue(f32(id) * 0.13) * 0.45, vec3f(0.95), on);
            let n = id + 1u;
            let count = select(1, 2, n >= 10u);
            // Digits 0.3 of the slice high, centred.
            let cell = 0.3 / 5.0;
            let width = (f32(count) * 4.0 - 1.0) * cell;
            let p = (l - vec2f(0.5 - width * 0.5, 0.35)) / cell;
            let cx = i32(floor(p.x));
            let cy = i32(floor(p.y));
            let k = cx / 4;
            if (p.x >= 0.0 && p.y >= 0.0 && k < count) {
                var dd = n % 10u;
                if (count == 2 && k == 0) { dd = (n / 10u) % 10u; }
                if (digit_on(dd, cx - k * 4, cy)) {
                    col = vec3f(1.0);
                } else if (p.y < 5.0 && cx - k * 4 < 3) {
                    col = vec3f(0.0);
                }
            }
            return vec4f(col, 1.0);
        }
        default: {}
    }
    return vec4f(0.0);
}

fn inside_mask(p: vec2f, start: u32, count: u32) -> bool {
    var inside = false;
    var j = count - 1u;
    for (var i = 0u; i < count; i = i + 1u) {
        let a = mask_point(start + i);
        let b = mask_point(start + j);
        if ((a.y > p.y) != (b.y > p.y)) {
            let x = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
            if (p.x < x) { inside = !inside; }
        }
        j = i;
    }
    return inside;
}

fn mask_point(k: u32) -> vec2f {
    let v = u.mask[k / 2u];
    return select(v.xy, v.zw, (k & 1u) == 1u);
}

@fragment
fn fs(v: VsOut) -> @location(0) vec4f {
    let fw = max(fwidth(v.local), vec2f(1e-5));
    let s = u.slices[v.slice];
    let kind = i32(u.size.z + 0.5);
    var c = textureSampleLevel(src, samp, v.uv, 0.0);
    if (kind != 0) { c = pattern(kind, v.local, fw, u32(s.extra.y + 0.5)); }
    let l = v.local;
    var w = edge(l.x, s.blend.x, s.curve.x, s.curve.y)
        * edge(1.0 - l.x, s.blend.y, s.curve.x, s.curve.y)
        * edge(l.y, s.blend.z, s.curve.x, s.curve.y)
        * edge(1.0 - l.y, s.blend.w, s.curve.x, s.curve.y);
    let p = v.clip.xy / u.size.xy;
    let count = u32(s.curve.w + 0.5);
    if (count >= 3u) {
        let inside = inside_mask(p, u32(s.curve.z + 0.5), count);
        if (inside == (s.extra.x > 0.5)) { w = 0.0; }
    }
    if (u.size.w > 0.5) {
        let m = textureSampleLevel(mask_img, samp, p, 0.0);
        w = w * dot(m.rgb, vec3f(0.2126, 0.7152, 0.0722)) * m.a;
    }
    return c * w;
}
"#;

/// Uniform block size: the header, 16 slices of three `vec4f`, 256 mask
/// `vec4f`.
const UNIFORM_BYTES: usize = 16 + MAX_SLICES * 48 + 256 * 16;

/// Bind groups and target views kept: pooled textures cycle among a few.
const CACHE: usize = 4;
/// Entries unused for this many draws are dropped (they keep textures alive).
const STALE_DRAWS: u64 = 16;

/// Renders an [`OutputMap`]: [`Self::set_map`] when the description
/// changes (tessellates and uploads), [`Self::draw`] every frame (records
/// one render pass; creates no GPU objects once its caches are warm, and
/// writes the uniform buffer only when it changed).
#[derive(Debug)]
pub struct OutputMapper {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    vertices: Option<wgpu::Buffer>,
    indices: Option<wgpu::Buffer>,
    index_count: u32,
    /// The map's per-slice uniform data (header filled per draw).
    block: Vec<u8>,
    /// Bytes last written to `uniforms`.
    written: Vec<u8>,
    /// Scratch for tessellation, kept between edits.
    scratch_v: Vec<u8>,
    scratch_i: Vec<u32>,
    /// (source, mask, bind group, last draw).
    binds: Vec<(wgpu::Texture, wgpu::Texture, wgpu::BindGroup, u64)>,
    /// (output, view, last draw).
    targets: Vec<(wgpu::Texture, wgpu::TextureView, u64)>,
    draws: u64,
}

fn put(block: &mut [u8], at: usize, values: [f32; 4]) {
    for (i, v) in values.iter().enumerate() {
        block[at + i * 4..at + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

impl OutputMapper {
    /// Builds the pipeline for output textures of `format`, with an empty
    /// map (call [`Self::set_map`] before drawing anything).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let label = Some("atd-output-map");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(2),
                texture(3),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_BYTES as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Later slices over earlier ones (premultiplied).
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label,
            size: UNIFORM_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            uniforms,
            sampler,
            vertices: None,
            indices: None,
            index_count: 0,
            block: vec![0; UNIFORM_BYTES],
            written: Vec::with_capacity(UNIFORM_BYTES),
            scratch_v: Vec::new(),
            scratch_i: Vec::new(),
            binds: Vec::with_capacity(CACHE),
            targets: Vec::with_capacity(CACHE),
            draws: 0,
        }
    }

    /// Uses `map` from the next draw: tessellates it and uploads the mesh,
    /// growing the GPU buffers only when it no longer fits. Call it when
    /// the description changes, not every frame.
    pub fn set_map(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, map: &OutputMap) {
        tessellate(map, &mut self.scratch_v, &mut self.scratch_i);
        let vbytes = self.scratch_v.len() as u64;
        let ibytes = (self.scratch_i.len() * 4) as u64;
        let fits = |b: &Option<wgpu::Buffer>, n: u64| b.as_ref().is_some_and(|b| b.size() >= n);
        if !fits(&self.vertices, vbytes) {
            self.vertices = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atd-output-map vertices"),
                size: vbytes.max(VERTEX_BYTES as u64),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if !fits(&self.indices, ibytes) {
            self.indices = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atd-output-map indices"),
                size: ibytes.max(4),
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let mut ib = Vec::with_capacity(self.scratch_i.len() * 4);
        for i in &self.scratch_i {
            ib.extend_from_slice(&i.to_le_bytes());
        }
        if let (Some(v), Some(i)) = (&self.vertices, &self.indices) {
            queue.write_buffer(v, 0, &self.scratch_v);
            queue.write_buffer(i, 0, &ib);
        }
        self.index_count = self.scratch_i.len() as u32;

        self.block.fill(0);
        let mut mask_at = 0usize;
        for (k, s) in map.slices.iter().enumerate().take(MAX_SLICES) {
            let at = 16 + k * 48;
            put(&mut self.block, at, s.blend);
            let count = s.mask.len().min(MAX_MASK_POINTS);
            put(
                &mut self.block,
                at + 16,
                [s.blend_power, s.blend_gamma, mask_at as f32, count as f32],
            );
            put(
                &mut self.block,
                at + 32,
                [f32::from(u8::from(s.mask_invert)), k as f32, 0.0, 0.0],
            );
            for p in &s.mask[..count] {
                let off = 16 + MAX_SLICES * 48 + mask_at * 8;
                for (i, v) in p.iter().enumerate() {
                    self.block[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                mask_at += 1;
            }
        }
    }

    /// Records the mapped image of `source` into `output` (cleared to
    /// transparent black first). `mask` multiplies the result by its
    /// luminance × alpha in output space; `black` stands in when there is
    /// none. Creates no GPU objects once its caches hold the textures in
    /// use, and allocates nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
        mask: Option<&wgpu::Texture>,
        black: &wgpu::Texture,
        pattern: Pattern,
        output: &wgpu::Texture,
    ) {
        put(
            &mut self.block,
            0,
            [
                output.width() as f32,
                output.height() as f32,
                pattern.index(),
                f32::from(u8::from(mask.is_some())),
            ],
        );
        if self.written != self.block {
            queue.write_buffer(&self.uniforms, 0, &self.block);
            self.written.clear();
            self.written.extend_from_slice(&self.block);
        }
        self.draws += 1;
        let now = self.draws;
        self.binds.retain(|e| now - e.3 <= STALE_DRAWS);
        self.targets.retain(|e| now - e.2 <= STALE_DRAWS);
        let mask = mask.unwrap_or(black);
        let bind = match self
            .binds
            .iter()
            .position(|(s, m, _, _)| s == source && m == mask)
        {
            Some(i) => {
                self.binds[i].3 = now;
                i
            }
            None => {
                let sv = source.create_view(&Default::default());
                let mv = mask.create_view(&Default::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&sv),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&mv),
                        },
                    ],
                });
                if self.binds.len() == CACHE {
                    self.binds.remove(0);
                }
                self.binds.push((source.clone(), mask.clone(), bind, now));
                self.binds.len() - 1
            }
        };
        let target = match self.targets.iter().position(|(t, _, _)| t == output) {
            Some(i) => {
                self.targets[i].2 = now;
                i
            }
            None => {
                if self.targets.len() == CACHE {
                    self.targets.remove(0);
                }
                let view = output.create_view(&Default::default());
                self.targets.push((output.clone(), view, now));
                self.targets.len() - 1
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atd-output-map"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets[target].1,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (Some(v), Some(i)) = (&self.vertices, &self.indices) else {
            return;
        };
        if self.index_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.binds[bind].2, &[]);
        pass.set_vertex_buffer(0, v.slice(..));
        pass.set_index_buffer(i.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }

    /// Bind groups and target views currently cached (for tests).
    pub fn cached(&self) -> (usize, usize) {
        (self.binds.len(), self.targets.len())
    }
}
