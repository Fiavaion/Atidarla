//! Time Machine (PRD §6, IMG transform & time): each pixel shows the input
//! from a different moment. The last N frames live in one 2D texture array
//! (copied in, no CPU work); the `time` image's luminance picks how far
//! back each pixel looks, blending between neighbouring frames.

use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{CookCtx, GpuOp};

use crate::ops::img;

pub fn schema() -> OperatorSchema {
    let mut s = img(
        "time_machine",
        "Time Machine",
        "Shows each pixel of `in` from a different moment: the luminance of `time` (black = now, white = oldest) picks how many frames back, blending between frames. Without `time`, the whole image is delayed by Offset.",
        &[("in", false), ("time", false)],
        vec![
            ParamSchema::int("frames", "Frames", 30, Some(2), Some(120))
                .help("How many past frames are kept (memory grows with this and the resolution)."),
            ParamSchema::float("offset", "Offset", 0.0, Some(0.0), Some(1.0))
                .help("Added to the time map: a fraction of the history."),
            ParamSchema::float("scale", "Scale", 1.0, None, None)
                .help("Multiplies the time map's luminance."),
        ],
        200,
    );
    s.time_dependent = true;
    s
}

const SHADER: &str = r#"
struct VsOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4f(p * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0);
    out.uv = p;
    return out;
}

// x = head layer (newest), y = layers kept, z = offset, w = scale
@group(0) @binding(0) var<uniform> params: vec4f;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var history: texture_2d_array<f32>;
@group(0) @binding(3) var time_map: texture_2d<f32>;

@fragment
fn fs(v: VsOut) -> @location(0) vec4f {
    let n = params.y;
    let t = textureSampleLevel(time_map, samp, v.uv, 0.0);
    let back = clamp(params.z + dot(t.rgb, vec3f(0.2126, 0.7152, 0.0722)) * params.w, 0.0, 1.0)
        * (n - 1.0);
    let k = floor(back);
    let f = back - k;
    // Layers go back in time from the head, wrapping around the ring.
    let a = i32((params.x - k + n) % n);
    let b = i32((params.x - k - 1.0 + 2.0 * n) % n);
    let ca = textureSampleLevel(history, samp, v.uv, a, 0.0);
    let cb = textureSampleLevel(history, samp, v.uv, b, 0.0);
    return mix(ca, select(cb, ca, k + 1.0 > n - 1.0), f);
}
"#;

struct History {
    texture: wgpu::Texture,
    bind: Option<(wgpu::Texture, wgpu::BindGroup)>,
    head: u32,
    /// Frames written so far (the ring is full once this reaches its size).
    filled: u32,
}

pub struct TimeMachine {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    history: Option<History>,
    targets: Vec<(wgpu::Texture, wgpu::TextureView)>,
}

impl std::fmt::Debug for TimeMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeMachine").finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("img.time_machine"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let texture_entry = |binding, dim| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("img.time_machine"),
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
            texture_entry(2, wgpu::TextureViewDimension::D2Array),
            texture_entry(3, wgpu::TextureViewDimension::D2),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("img.time_machine"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("img.time_machine"),
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
    Box::new(TimeMachine {
        pipeline,
        layout,
        uniforms: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.time_machine"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        sampler: device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img.time_machine"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
        history: None,
        targets: Vec::new(),
    })
}

impl GpuOp for TimeMachine {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out = ctx.output;
        let layers = p.i64("frames").clamp(2, 120) as u32;
        if let Err(e) = crate::stateful::history_fits(out, u64::from(layers)) {
            crate::stateful::clear(ctx.encoder, out);
            self.history = None;
            return Err(e);
        }
        let fits = self.history.as_ref().is_some_and(|h| {
            h.texture.width() == out.width()
                && h.texture.height() == out.height()
                && h.texture.depth_or_array_layers() == layers
                && h.texture.format() == out.format()
        });
        if !fits {
            let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lsr-time-machine"),
                size: wgpu::Extent3d {
                    width: out.width(),
                    height: out.height(),
                    depth_or_array_layers: layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: out.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.history = Some(History {
                texture,
                bind: None,
                head: layers - 1,
                filled: 0,
            });
        }
        let h = self.history.as_mut().expect("created above");
        let input = ctx.input_or_black(0).clone();
        // Newest frame into the next layer. Until the ring is full, the
        // first frame fills every layer so early history is not black.
        let head = (h.head + 1) % layers;
        let copies = if h.filled == 0 {
            0..layers
        } else {
            head..head + 1
        };
        if input.width() == out.width() && input.height() == out.height() {
            for layer in copies {
                ctx.encoder.copy_texture_to_texture(
                    input.as_image_copy(),
                    wgpu::TexelCopyTextureInfo {
                        texture: &h.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: out.width(),
                        height: out.height(),
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        h.head = head;
        h.filled = (h.filled + 1).min(layers);

        let values = [head as f32, layers as f32, p.f32("offset"), p.f32("scale")];
        let mut bytes = [0u8; 16];
        for (c, v) in bytes.chunks_exact_mut(4).zip(values) {
            c.copy_from_slice(&v.to_le_bytes());
        }
        ctx.queue.write_buffer(&self.uniforms, 0, &bytes);
        let time_map = ctx.input_or_black(1).clone();
        let rebind = h.bind.as_ref().is_none_or(|(t, _)| *t != time_map);
        if rebind {
            let history_view = h.texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            let time_view = time_map.create_view(&Default::default());
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.time_machine"),
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
                        resource: wgpu::BindingResource::TextureView(&history_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&time_view),
                    },
                ],
            });
            h.bind = Some((time_map, bind));
        }
        let bind = &h.bind.as_ref().expect("bound above").1;
        let target = match self.targets.iter().position(|(t, _)| t == out) {
            Some(i) => &self.targets[i].1,
            None => {
                if self.targets.len() == 8 {
                    self.targets.remove(0);
                }
                self.targets
                    .push((out.clone(), out.create_view(&Default::default())));
                &self.targets.last().expect("pushed").1
            }
        };
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img.time_machine"),
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
        Ok(())
    }
}
