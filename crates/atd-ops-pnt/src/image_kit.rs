//! Reading IMG inputs from point kernels: a compute kernel like
//! [`crate::kit::PointKernel`] with one or two textures bound after the
//! header (`img0`, `img1`, read with `textureLoad`, so any float, unorm or
//! snorm image format works), and the schema helper for PNT operators with
//! IMG sockets.
//!
//! Bind groups are cached by texture and buffer identity, as
//! [`atd_gpu::geo::kernels::BindGroupCache`] does for buffers: IMG results
//! and pooled attributes cycle among a few generations, so steady-state
//! frames create none (rule 2). An unconnected image binds a 1 × 1 black
//! texture.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::GeoCtx;
use atd_gpu::geo::kernels::{Bind, Uniforms};

use crate::kit::{MAX_BINDS, prelude};

/// Bind groups kept per kernel.
const CACHE: usize = 16;

/// WGSL helpers for image kernels, after the texture declarations:
/// `img_size(k)` (`vec2u`), `img_load(k, xy)` (clamped to the image) and
/// `img_sample(k, uv, bilinear, repeat)` (uv 0-1, v up: row 0 is the top of
/// the image, as IMG operators draw it), for texture `k` (0 or 1).
fn helpers(textures: usize) -> String {
    let mut s = String::new();
    for k in 0..textures {
        s.push_str(&format!(
            "fn img{k}_load(xy: vec2i) -> vec4f {{
    let size = vec2i(textureDimensions(img{k}));
    return textureLoad(img{k}, clamp(xy, vec2i(0), size - vec2i(1)), 0);
}}
fn img{k}_sample(uv: vec2f, bilinear: bool, repeat: bool) -> vec4f {{
    let size = vec2f(textureDimensions(img{k}));
    var t = vec2f(uv.x, 1.0 - uv.y);
    if (repeat) {{ t = fract(t); }} else {{ t = clamp(t, vec2f(0.0), vec2f(1.0)); }}
    let s = t * size - vec2f(0.5);
    if (!bilinear) {{
        let n = vec2i(floor(t * size));
        if (repeat) {{ return img{k}_load(n % vec2i(size)); }}
        return img{k}_load(n);
    }}
    let f = fract(s);
    var a = vec2i(floor(s));
    var b = a + vec2i(1);
    if (repeat) {{
        let w = vec2i(size);
        a = (a % w + w) % w;
        b = (b % w + w) % w;
    }}
    let c00 = img{k}_load(a);
    let c10 = img{k}_load(vec2i(b.x, a.y));
    let c01 = img{k}_load(vec2i(a.x, b.y));
    let c11 = img{k}_load(b);
    return mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y);
}}
"
        ));
    }
    s
}

/// A PNT operator schema whose inputs are a mix of GEO and IMG sockets
/// (`(name, wire)`), with one GEO output.
pub fn schema(
    id: &str,
    label: &str,
    summary: &str,
    inputs: &[(&str, WireType)],
    params: Vec<ParamSchema>,
    cook_budget_us: u32,
) -> OperatorSchema {
    let socket = |name: &str, wire: WireType| SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: format!("pnt.{id}"),
        family: OpFamily::Pnt,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: inputs.iter().map(|(n, w)| socket(n, *w)).collect(),
        outputs: vec![socket("out", WireType::Geo)],
        capabilities: vec![],
        cook_budget_us,
        time_dependent: false,
        sink: false,
    }
}

struct Entry {
    textures: [wgpu::Texture; 2],
    buffers: [Option<wgpu::Buffer>; MAX_BINDS + 2],
    bind: wgpu::BindGroup,
    used: u64,
}

/// A compute kernel over points that reads images. `source` declares
/// `struct Params`, the buffer bindings from `2 + textures` on (`binds`, in
/// order) and `fn point(i: u32)`, run for every `i < hdr.count`; the
/// uniforms (`params`, binding 0), the header (`hdr`, 1) and the textures
/// (`img0`, `img1`: 2, 3) are declared here.
pub struct ImageKernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: Uniforms,
    textures: usize,
    black: wgpu::Texture,
    cache: Vec<Entry>,
    uses: u64,
}

impl ImageKernel {
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        textures: usize,
        source: &str,
        binds: &[Bind],
        params_bytes: u64,
    ) -> Self {
        assert!((1..=2).contains(&textures), "{label}: one or two textures");
        // Textures do not count toward the storage-buffer limit.
        assert!(binds.len() <= MAX_BINDS, "{label}: too many bindings");
        let mut decls = String::new();
        for k in 0..textures {
            decls.push_str(&format!(
                "@group(0) @binding({}) var img{k}: texture_2d<f32>;\n",
                2 + k
            ));
        }
        let src = format!(
            "{}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hdr: GeoHeader;
{decls}{}{source}
@compute @workgroup_size({})
fn main(@builtin(global_invocation_id) gid: vec3u) {{
    let i = gid.x;
    if (i < hdr.count) {{ point(i); }}
}}
",
            prelude(),
            helpers(textures),
            atd_core::geo::WORKGROUP,
        );
        let mut entries = atd_gpu::geo::kernels::layout_entries(
            &[Bind::Uniform, Bind::Read],
            wgpu::ShaderStages::COMPUTE,
        );
        for k in 0..textures {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: (2 + k) as u32,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        for (b, mut e) in atd_gpu::geo::kernels::layout_entries(binds, wgpu::ShaderStages::COMPUTE)
            .into_iter()
            .enumerate()
        {
            e.binding = (2 + textures + b) as u32;
            entries.push(e);
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
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
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let black = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pnt.image.black"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            pipeline,
            layout,
            params: Uniforms::new(device, label, params_bytes.max(16)),
            textures,
            black,
            cache: Vec::with_capacity(CACHE),
            uses: 0,
        }
    }

    /// The image on input `i` of `ctx`, if connected and cooked.
    pub fn image<'a>(ctx: &'a GeoCtx<'_>, i: usize) -> Option<&'a wgpu::Texture> {
        ctx.images.get(i).and_then(Option::as_ref)
    }

    fn bind_group(
        &mut self,
        device: &wgpu::Device,
        textures: [&wgpu::Texture; 2],
        all: &[&wgpu::Buffer],
    ) -> wgpu::BindGroup {
        self.uses += 1;
        let now = self.uses;
        let hit = self.cache.iter_mut().find(|e| {
            e.textures[0] == *textures[0]
                && e.textures[1] == *textures[1]
                && e.buffers.iter().flatten().eq(all.iter().copied())
        });
        if let Some(e) = hit {
            e.used = now;
            return e.bind.clone();
        }
        let views: Vec<wgpu::TextureView> = textures[..self.textures]
            .iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::with_capacity(all.len() + 2);
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: all[0].as_entire_binding(),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 1,
            resource: all[1].as_entire_binding(),
        });
        for (k, v) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: (2 + k) as u32,
                resource: wgpu::BindingResource::TextureView(v),
            });
        }
        for (b, buf) in all[2..].iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: (2 + self.textures + b) as u32,
                resource: buf.as_entire_binding(),
            });
        }
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &entries,
        });
        if self.cache.len() == CACHE {
            let oldest = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map_or(0, |(i, _)| i);
            self.cache.swap_remove(oldest);
        }
        let mut buffers: [Option<wgpu::Buffer>; MAX_BINDS + 2] = Default::default();
        for (slot, b) in buffers.iter_mut().zip(all) {
            *slot = Some((*b).clone());
        }
        self.cache.push(Entry {
            textures: [textures[0].clone(), textures[1].clone()],
            buffers,
            bind: bind.clone(),
            used: now,
        });
        bind
    }

    /// Writes `params` and dispatches over `count` points (the header's
    /// count bounds the work), or from the header when `count` is `None`
    /// (GPU-counted inputs), in a pass of its own, with `images` (missing
    /// ones black) and `buffers` bound in order.
    pub fn run(
        &mut self,
        ctx: &mut GeoCtx<'_>,
        hdr: &wgpu::Buffer,
        images: [Option<&wgpu::Texture>; 2],
        buffers: &[&wgpu::Buffer],
        params: &[u8],
        count: Option<u32>,
    ) {
        self.params.write(ctx.queue, params);
        let n = buffers.len();
        assert!(n <= MAX_BINDS);
        let uniforms = self.params.buffer().clone();
        let mut all: [&wgpu::Buffer; MAX_BINDS + 2] = [hdr; MAX_BINDS + 2];
        all[0] = &uniforms;
        all[2..2 + n].copy_from_slice(buffers);
        let black = self.black.clone();
        let textures = [images[0].unwrap_or(&black), images[1].unwrap_or(&black)];
        let bind = self.bind_group(ctx.device, textures, &all[..2 + n]);
        let mut pass = atd_gpu::geo::kernels::pass(ctx.encoder);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        match count {
            Some(n) => pass.dispatch_workgroups(atd_core::geo::workgroups(n).max(1), 1, 1),
            None => pass.dispatch_workgroups_indirect(hdr, atd_core::geo::header::DISPATCH_OFFSET),
        }
    }
}
