//! Full-screen fragment passes: the building block for most IMG operators.
//!
//! A [`FullscreenPass`] is a render pipeline that draws one triangle over the
//! output and runs a WGSL fragment function. The operator supplies the body
//! of `fn shade(uv: vec2f) -> vec4f`; inputs are bound as `in0`, `in1`, …
//! with sampler `samp`, and parameters as a uniform struct `params` of
//! `vec4f`s (`params.v[0]`, `params.v[1]`, …).

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
    /// Uniform bytes last written, to skip identical writes.
    uniforms: Option<[u8; MAX_PARAM_VEC4 * 16]>,
}

/// A compiled full-screen pass with `inputs` sampled textures.
///
/// Parameters live in one uniform buffer per pass: drawing the same pass
/// several times in a frame uses the last parameters for every draw.
#[derive(Debug)]
pub struct FullscreenPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    inputs: usize,
    caches: std::sync::Mutex<Caches>,
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

        let mut entries = vec![
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
        ];
        for i in 0..inputs {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: (i + 2) as u32,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
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
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (MAX_PARAM_VEC4 * 16) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(label),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            uniforms,
            sampler,
            inputs,
            caches: Default::default(),
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
        assert_eq!(inputs.len(), self.inputs, "input count");
        assert!(params.len() <= MAX_PARAM_VEC4, "too many parameter slots");
        let mut data = [[0f32; 4]; MAX_PARAM_VEC4];
        data[..params.len()].copy_from_slice(params);
        let mut bytes = [0u8; MAX_PARAM_VEC4 * 16];
        for (chunk, f) in bytes.chunks_exact_mut(4).zip(data.iter().flatten()) {
            chunk.copy_from_slice(&f.to_le_bytes());
        }
        let mut caches = self.caches.lock().unwrap_or_else(|e| e.into_inner());
        let caches = &mut *caches;
        if caches.uniforms != Some(bytes) {
            queue.write_buffer(&self.uniforms, 0, &bytes);
            caches.uniforms = Some(bytes);
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
                let mut entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ];
                for (i, view) in views.iter().enumerate() {
                    entries.push(wgpu::BindGroupEntry {
                        binding: (i + 2) as u32,
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
        pass.set_bind_group(0, bind, &[]);
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
                std::env::var("ATD_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ATD_REQUIRE_GPU is set but there is no GPU adapter"
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
