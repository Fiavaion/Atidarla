//! Spout sender: renders an animated test pattern with wgpu and publishes it.
//!
//! `cargo run -p lsr-interop --example spout_send [name] [seconds] [width] [height] [flags]`
//!
//! Defaults: "Lasair Test", 30 s, 1280x720. Open it in any Spout
//! receiver, or with the `spout_receive` example. Flags, used by the SDK
//! interop check (`docs/spikes/spout-interop/`):
//! - `--pattern`: send the stamped test pattern (`common/stamped.rs`) with
//!   a new seed every frame, instead of the shader.
//! - `--timing`: the pattern with seed 0, uploaded once; each frame only
//!   rewrites the 16-byte stamp, so 4K frames do not pay a full upload.
//! - `--resize`: halve the size half-way through.
//! - `--rgba`: send RGBA8 instead of BGRA8.

#[cfg(windows)]
#[allow(dead_code)]
#[path = "common/stamped.rs"]
mod stamped;

#[cfg(not(windows))]
fn main() {
    eprintln!("Spout is Windows-only");
}

#[cfg(windows)]
const SHADER: &str = r"
@group(0) @binding(0) var<uniform> u: vec4<f32>; // time, width, height

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / u.yz;
    let bars = step(0.5, fract(uv.x * 8.0 - u.x * 0.5));
    let rgb = vec3<f32>(uv.x, uv.y, 0.5 + 0.5 * sin(u.x * 2.0));
    return vec4<f32>(rgb * (0.6 + 0.4 * bars), 1.0);
}
";

/// The animated shader pattern.
#[cfg(windows)]
struct Shader {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

#[cfg(windows)]
impl Shader {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("spout_send"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spout_send"),
            layout: None,
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
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("spout_send"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spout_send"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        Self {
            pipeline,
            uniforms,
            bind,
        }
    }

    fn render(&self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture, t: f32) {
        let size = target.size();
        let data: Vec<u8> = [t, size.width as f32, size.height as f32, 0.0]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        queue.write_buffer(&self.uniforms, 0, &data);
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use lsr_interop::dx12::SharedTexture;
    use lsr_interop::readback::{test_pattern, write_rgba8};
    use lsr_interop::spout::SpoutSender;

    let (flags, args): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a.starts_with("--"));
    let flag = |f: &str| flags.iter().any(|a| a == f);
    let (pattern, timing, resize) = (flag("--pattern"), flag("--timing"), flag("--resize"));
    let mut args = args.into_iter();
    let name = args.next().unwrap_or_else(|| "Lasair Test".into());
    let seconds: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let mut width: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1280);
    let mut height: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(720);
    let format = if flag("--rgba") {
        wgpu::TextureFormat::Rgba8Unorm
    } else {
        wgpu::TextureFormat::Bgra8Unorm
    };

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(desc);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("no DX12 adapter");
    println!("adapter: {}", adapter.get_info().name);
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");

    let shader = Shader::new(&device, format);
    let new_frame = |width, height| {
        let frame = SharedTexture::create(&device, width, height, format, None).expect("texture");
        if timing {
            write_rgba8(&queue, &frame.texture, &test_pattern(width, height, 0));
        }
        frame
    };
    let mut frame = new_frame(width, height);
    let mut sender = SpoutSender::new(&device, &name, width, height, format).expect("Spout sender");
    println!(
        "sending \"{}\" {width}x{height} {format:?} for {seconds} s",
        sender.name()
    );

    let start = Instant::now();
    let frame_time = Duration::from_secs_f64(1.0 / 60.0);
    let mut frames = 0u32;
    let mut resized = false;
    let mut send_ms = Vec::new();
    while start.elapsed().as_secs_f32() < seconds {
        let tick = Instant::now();
        let t = start.elapsed().as_secs_f32();
        if resize && !resized && t > seconds / 2.0 {
            resized = true;
            (width, height) = (width / 2, height / 2);
            println!("resizing to {width}x{height}");
            frame = new_frame(width, height);
        }
        if timing {
            let mut stamp = [0; stamped::STAMP_LEN];
            stamped::stamp(&mut stamp, frames, 0);
            queue.write_texture(
                frame.texture.as_image_copy(),
                &stamp,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stamped::STAMP_LEN as u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        } else if pattern {
            let pixels = stamped::frame_pixels(width, height, frames, frames as u8);
            write_rgba8(&queue, &frame.texture, &pixels);
        } else {
            shader.render(&device, &queue, &frame.texture, t);
        }
        let sent_at = Instant::now();
        if sender.send_wgpu(&queue, &frame).expect("send") {
            send_ms.push(sent_at.elapsed().as_secs_f64() * 1e3);
            frames += 1;
        }
        // Retire finished work without waiting for the GPU.
        let _ = device.poll(wgpu::PollType::Poll);
        if let Some(rest) = frame_time.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    println!(
        "sent {frames} frames; send_wgpu ms mean / p50 / p95 / max: {}",
        stamped::summary(&mut send_ms)
    );
}
