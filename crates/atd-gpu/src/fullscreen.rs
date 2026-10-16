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

#[derive(Debug, Default)]
struct Caches {
    /// (input textures, bind group), most recently used last.
    binds: Vec<(Vec<wgpu::Texture>, wgpu::BindGroup)>,
    /// (output texture, render target view), most recently used last.
    targets: Vec<(wgpu::Texture, wgpu::TextureView)>,
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

impl FullscreenPass {
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
        let mut source = String::from(VERTEX);
        for i in 0..inputs {
            source.push_str(&format!(
                "@group(0) @binding({}) var in{i}: texture_2d<f32>;\n",
                i + 2
            ));
        }
        source.push_str(helpers);
        source.push_str(&format!(
            "\nfn shade(uv: vec2f) -> vec4f {{\n{shade_body}\n}}\n\n@fragment\nfn fs(v: VsOut) -> @location(0) vec4f {{ return shade(v.uv); }}\n"
        ));
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
        let bind = match caches
            .binds
            .iter()
            .position(|(key, _)| key.iter().eq(inputs.iter().copied()))
        {
            Some(i) => {
                let hit = caches.binds.remove(i);
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
                caches.binds.push((key, bind));
                &caches.binds.last().expect("pushed").1
            }
        };
        let target = match caches.targets.iter().position(|(t, _)| t == output) {
            Some(i) => {
                let hit = caches.targets.remove(i);
                caches.targets.push(hit);
                &caches.targets.last().expect("pushed").1
            }
            None => {
                if caches.targets.len() == CACHE {
                    caches.targets.remove(0);
                }
                let view = output.create_view(&Default::default());
                caches.targets.push((output.clone(), view));
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
