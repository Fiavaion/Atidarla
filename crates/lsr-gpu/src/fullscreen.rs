//! Full-screen fragment passes: the building block for most IMG operators.
//!
//! A [`FullscreenPass`] is a render pipeline that draws one triangle over the
//! output and runs a WGSL fragment function. The operator supplies the body
//! of `fn shade(uv: vec2f) -> vec4f`; inputs are bound as `in0`, `in1`, …
//! with sampler `samp`, and parameters as a uniform struct `params` of
//! `vec4f`s (`params.v[0]`, `params.v[1]`, …).
//!
//! [`FullscreenPass::try_from_module`] instead takes a complete module (a
//! Pixels kernel's, say) that binds its uniforms and sampler in group 0 and
//! its input textures in group 1 ([`ModuleLayout`]).

/// Maximum uniform `vec4f` slots per pass.
pub const MAX_PARAM_VEC4: usize = 8;

const VERTEX: &str = r#"
struct VsOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle covering the viewport; uv (0,0) is the top-left texel.
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4f(p * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0);
    out.uv = p;
    return out;
}

struct Params { v: array<vec4f, 8> };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
"#;

/// Bind groups and target views kept per pass: pooled textures cycle among
/// a few, so steady-state frames create no GPU objects (rule 2).
const CACHE: usize = 8;

/// Entries unused for this many draws are dropped. They hold their
/// textures alive, so without this a pass would keep up to [`CACHE`] dead
/// textures (say, every size a dragged Resolution passed through) after the
/// pool has let them go.
const STALE_DRAWS: u64 = 2 * CACHE as u64;

#[derive(Debug, Default)]
struct Caches {
    /// (input textures, bind group, last draw), most recently used last.
    binds: Vec<(Vec<wgpu::Texture>, wgpu::BindGroup, u64)>,
    /// (output texture, render target view, last draw), most recently used last.
    targets: Vec<(wgpu::Texture, wgpu::TextureView, u64)>,
    /// Draws so far.
    draws: u64,
    /// Uniform bytes last written (the buffer's size), to skip identical
    /// writes.
    uniforms: Vec<u8>,
    written: bool,
}

/// A compiled full-screen pass with `inputs` sampled textures.
///
/// Parameters live in one uniform buffer per pass: drawing the same pass
/// several times in a frame uses the last parameters for every draw.
#[derive(Debug)]
pub struct FullscreenPass {
    pipeline: wgpu::RenderPipeline,
    /// Layout of the group holding the inputs.
    layout: wgpu::BindGroupLayout,
    /// A module's own layout: group 0 (uniforms and sampler), bound once;
    /// the inputs are in group 1. `None`: everything is in group 0.
    fixed: Option<wgpu::BindGroup>,
    /// Binding of input 0 in its group.
    input_base: u32,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    inputs: usize,
    caches: std::sync::Mutex<Caches>,
}

/// How a complete module ([`FullscreenPass::try_from_module`]) binds its
/// resources: group 0 holds the uniform buffer (binding 0) and a filtering
/// sampler (binding 1, linear, clamp to edge), group 1 the input textures
/// (`texture_2d<f32>`, input `i` at binding `i`). The layout may declare
/// inputs the module does not use.
#[derive(Debug, Clone, Copy)]
pub struct ModuleLayout<'a> {
    pub vertex_entry: &'a str,
    pub fragment_entry: &'a str,
    /// Bytes of the uniform buffer.
    pub uniform_size: u64,
    /// Input textures in group 1.
    pub inputs: usize,
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_sampler(device: &wgpu::Device, label: &str) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    })
}

/// The render pipeline of a full-screen pass writing `format`.
fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    module: &wgpu::ShaderModule,
    entries: (&str, &str),
    groups: &[Option<&wgpu::BindGroupLayout>],
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: groups,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(entries.0),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entries.1),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn caches(uniform_size: u64) -> std::sync::Mutex<Caches> {
    std::sync::Mutex::new(Caches {
        uniforms: vec![0; uniform_size as usize],
        ..Default::default()
    })
}

/// The complete WGSL module, and the 1-based line where `shade_body` starts.
fn source(inputs: usize, helpers: &str, shade_body: &str) -> (String, usize) {
    let mut source = String::from(VERTEX);
    for i in 0..inputs {
        source.push_str(&format!(
            "@group(0) @binding({}) var in{i}: texture_2d<f32>;\n",
            i + 2
        ));
    }
    source.push_str(helpers);
    source.push_str("\nfn shade(uv: vec2f) -> vec4f {\n");
    let body_line = source.lines().count() + 1;
    source.push_str(shade_body);
    source.push_str(
        "\n}\n\n@fragment\nfn fs(v: VsOut) -> @location(0) vec4f { return shade(v.uv); }\n",
    );
    (source, body_line)
}

impl FullscreenPass {
    /// Builds a pass from code a user wrote: validates it with naga first,
    /// so errors name a line of `shade_body`, then creates the pipeline in
    /// an error scope. Blocks while the driver compiles; call it off the
    /// render thread.
    pub fn try_new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        inputs: usize,
        helpers: &str,
        shade_body: &str,
    ) -> Result<Self, String> {
        let (src, body_line) = source(inputs, helpers, shade_body);
        let place = |loc: Option<naga::SourceLocation>| match loc {
            Some(l) if l.line_number as usize >= body_line => {
                format!("line {}: ", l.line_number as usize - body_line + 1)
            }
            _ => String::new(),
        };
        let module = naga::front::wgsl::parse_str(&src).map_err(|e| {
            // Prefer the first real (non-empty) labelled span; some errors
            // also carry an empty placeholder span at offset 0.
            let loc = e
                .labels()
                .find(|(span, _)| span.to_range().is_some_and(|r| !r.is_empty()))
                .map(|(span, _)| span.location(&src))
                .or_else(|| e.location(&src));
            format!("{}{}", place(loc), e.message())
        })?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .map_err(|e| {
            let loc = e.spans().next().map(|(span, _)| span.location(&src));
            format!("{}{}", place(loc), e.as_inner())
        })?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pass = Self::new(device, label, format, inputs, helpers, shade_body);
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(e.to_string()),
            None => Ok(pass),
        }
    }

    /// Builds the pass. `shade_body` is the WGSL body of
    /// `fn shade(uv: vec2f) -> vec4f`; `helpers` is extra WGSL placed before it.
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        inputs: usize,
        helpers: &str,
        shade_body: &str,
    ) -> Self {
        let (source, _) = source(inputs, helpers, shade_body);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });

        let mut entries = vec![uniform_entry(0), sampler_entry(1)];
        for i in 0..inputs {
            entries.push(texture_entry((i + 2) as u32));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline = create_pipeline(
            device,
            label,
            format,
            &module,
            ("vs", "fs"),
            &[Some(&layout)],
        );
        let size = (MAX_PARAM_VEC4 * 16) as u64;
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            fixed: None,
            input_base: 2,
            uniforms,
            sampler: create_sampler(device, label),
            inputs,
            caches: caches(size),
        }
    }

    /// Builds a pass from a complete WGSL module laid out as [`ModuleLayout`]
    /// says, in an error scope (validate the module first: the driver's
    /// errors carry no line). Blocks while the driver compiles; call it off
    /// the render thread.
    pub fn try_from_module(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        wgsl: &str,
        layout: &ModuleLayout<'_>,
    ) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let g0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[uniform_entry(0), sampler_entry(1)],
        });
        let entries: Vec<_> = (0..layout.inputs as u32).map(texture_entry).collect();
        let g1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline = create_pipeline(
            device,
            label,
            format,
            &module,
            (layout.vertex_entry, layout.fragment_entry),
            &[Some(&g0), Some(&g1)],
        );
        // A buffer binding is never empty.
        let size = layout.uniform_size.max(16);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = create_sampler(device, label);
        let fixed = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &g0,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pass = Self {
            pipeline,
            layout: g1,
            fixed: Some(fixed),
            input_base: 0,
            uniforms,
            sampler,
            inputs: layout.inputs,
            caches: caches(size),
        };
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(e.to_string()),
            None => Ok(pass),
        }
    }

    /// Records the pass into `encoder`, writing `output`. Missing inputs must
    /// be replaced by the caller (e.g. with a black texture).
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &[&wgpu::Texture],
        params: &[[f32; 4]],
        output: &wgpu::Texture,
    ) {
        assert!(params.len() <= MAX_PARAM_VEC4, "too many parameter slots");
        let mut data = [[0f32; 4]; MAX_PARAM_VEC4];
        data[..params.len()].copy_from_slice(params);
        let mut bytes = [0u8; MAX_PARAM_VEC4 * 16];
        for (chunk, f) in bytes.chunks_exact_mut(4).zip(data.iter().flatten()) {
            chunk.copy_from_slice(&f.to_le_bytes());
        }
        self.draw_bytes(device, queue, encoder, inputs, &bytes, output);
    }

    /// Like [`Self::draw`], with the uniform buffer's bytes as they are (at
    /// most its size; the rest keeps its last contents). Allocates nothing
    /// once the inputs and outputs have been seen.
    pub fn draw_bytes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &[&wgpu::Texture],
        uniforms: &[u8],
        output: &wgpu::Texture,
    ) {
        assert_eq!(inputs.len(), self.inputs, "input count");
        let mut caches = self.caches.lock().unwrap_or_else(|e| e.into_inner());
        let caches = &mut *caches;
        assert!(uniforms.len() <= caches.uniforms.len(), "uniform bytes");
        if !caches.written || caches.uniforms[..uniforms.len()] != *uniforms {
            queue.write_buffer(&self.uniforms, 0, uniforms);
            caches.uniforms[..uniforms.len()].copy_from_slice(uniforms);
            caches.written = true;
        }
        caches.draws += 1;
        let now = caches.draws;
        // Removing in place allocates nothing.
        caches.binds.retain(|e| now - e.2 <= STALE_DRAWS);
        caches.targets.retain(|e| now - e.2 <= STALE_DRAWS);
        let bind = match caches
            .binds
            .iter()
            .position(|(key, _, _)| key.iter().eq(inputs.iter().copied()))
        {
            Some(i) => {
                let mut hit = caches.binds.remove(i);
                hit.2 = now;
                caches.binds.push(hit);
                &caches.binds.last().expect("pushed").1
            }
            None => {
                let views: Vec<wgpu::TextureView> = inputs
                    .iter()
                    .map(|t| t.create_view(&Default::default()))
                    .collect();
                let mut entries = Vec::with_capacity(views.len() + 2);
                if self.fixed.is_none() {
                    entries.push(wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    });
                }
                for (i, view) in views.iter().enumerate() {
                    entries.push(wgpu::BindGroupEntry {
                        binding: self.input_base + i as u32,
                        resource: wgpu::BindingResource::TextureView(view),
                    });
                }
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &entries,
                });
                if caches.binds.len() == CACHE {
                    caches.binds.remove(0);
                }
                let key = inputs.iter().map(|t| (*t).clone()).collect();
                caches.binds.push((key, bind, now));
                &caches.binds.last().expect("pushed").1
            }
        };
        let target = match caches.targets.iter().position(|(t, _, _)| t == output) {
            Some(i) => {
                let mut hit = caches.targets.remove(i);
                hit.2 = now;
                caches.targets.push(hit);
                &caches.targets.last().expect("pushed").1
            }
            None => {
                if caches.targets.len() == CACHE {
                    caches.targets.remove(0);
                }
                let view = output.create_view(&Default::default());
                caches.targets.push((output.clone(), view, now));
                &caches.targets.last().expect("pushed").1
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_pipeline(&self.pipeline);
        match &self.fixed {
            Some(fixed) => {
                pass.set_bind_group(0, fixed, &[]);
                pass.set_bind_group(1, bind, &[]);
            }
            None => pass.set_bind_group(0, bind, &[]),
        }
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_drop_entries_left_unused() {
        let instance = crate::primary_instance();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            let required =
                std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "LSR_REQUIRE_GPU is set but there is no GPU adapter"
            );
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pass = FullscreenPass::new(
            &device,
            "test",
            format,
            1,
            "",
            "return textureSample(in0, samp, uv);",
        );
        let texture = |w| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: w,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let (old_in, old_out) = (texture(3), texture(5));
        let (a, b, input) = (texture(8), texture(8), texture(8));
        let mut encoder = device.create_command_encoder(&Default::default());
        pass.draw(&device, &queue, &mut encoder, &[&old_in], &[], &old_out);
        // Steady ping-pong between two outputs keeps both, drops the old.
        for i in 0..=STALE_DRAWS {
            let out = if i % 2 == 0 { &a } else { &b };
            pass.draw(&device, &queue, &mut encoder, &[&input], &[], out);
        }
        queue.submit([encoder.finish()]);
        let caches = pass.caches.lock().unwrap();
        assert!(caches.targets.iter().all(|(t, _, _)| *t != old_out));
        assert!(caches.binds.iter().all(|(k, _, _)| k[0] != old_in));
        assert_eq!(caches.targets.len(), 2);
        assert_eq!(caches.binds.len(), 1);
    }
}
