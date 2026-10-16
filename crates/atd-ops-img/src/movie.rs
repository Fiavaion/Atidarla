//! Movie In (PRD §6): plays HAP movies (`atd_io::video`).
//!
//! Decoding runs on the player's thread, and the cook only uploads ready
//! frames (rule 1). On devices with BC texture compression, frames upload
//! as-is; HAP Q's YCoCg becomes RGB in the shader. Without BC support, the
//! CPU decodes BC1/BC3/BC4 to RGBA (rule 6); HAP R (BC7) then reports an
//! error.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;
use atd_io::video::hap::{HapFormat, HapFrame};
use atd_io::video::{MoviePlayer, bc};

use crate::stateful::clear;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.movie_in".into(),
        family: OpFamily::Img,
        label: "Movie In".into(),
        summary: "Plays a movie or still image. HAP, HAP Alpha, HAP Q and HAP R are decoded natively and upload as GPU-compressed textures; PNG and JPEG stills load as one frame; other codecs arrive with FFmpeg.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("file", "File", "").help("Path to a `.mov` file (HAP family) or a PNG/JPEG still."),
            ParamSchema::toggle("play", "Play", true).help("Advance the playhead; off holds the frame at Cue."),
            ParamSchema::toggle("loop", "Loop", true).help("Restart at the end; off holds the last frame."),
            ParamSchema::float("speed", "Speed", 1.0, None, None)
                .help("Playback rate; negative plays backwards."),
            ParamSchema::float("cue", "Cue", 0.0, Some(0.0), None)
                .help("Position in seconds while stopped; playback starts from here."),
            ParamSchema::menu("size", "Size", &["native", "project"], "native")
                .help("`native` outputs at the file's resolution; `project` at the project resolution."),
        ],
        inputs: vec![],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 300,
        time_dependent: true,
        sink: false,
    }
}

const BODY: &str = r#"
    let c = textureSample(in0, samp, uv * params.v[0].yz);
    let mode = params.v[0].x;
    if (mode > 1.5) {
        return vec4f(c.rrr, 1.0);
    }
    if (mode > 0.5) {
        // HAP Q: scaled YCoCg (Co, Cg, scale, Y).
        let cocg = c.xy - vec2f(0.50196078);
        let scale = c.z * (255.0 / 8.0) + 1.0;
        let co = cocg.x / scale;
        let cg = cocg.y / scale;
        let y = c.w;
        return vec4f(y + co - cg, y + cg, y - co - cg, 1.0);
    }
    return c;
"#;

/// The texture holding the current frame.
#[derive(Debug)]
struct Upload {
    texture: wgpu::Texture,
    /// Shader mode: 0 RGBA, 1 YCoCg, 2 single channel.
    mode: f32,
    /// Fraction of the texture covered by the picture (BC pads to 4×4).
    uv: [f32; 2],
}

#[derive(Debug)]
pub struct MovieIn {
    pass: FullscreenPass,
    bc: bool,
    path: String,
    player: Option<MoviePlayer>,
    playing: bool,
    playhead: f64,
    last_time: Option<f64>,
    shown: Option<usize>,
    upload: Option<Upload>,
    rgba: Vec<u8>,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(MovieIn {
        pass: FullscreenPass::new(device, "img.movie_in", format, 1, "", BODY),
        bc: device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
        path: String::new(),
        player: None,
        playing: false,
        playhead: 0.0,
        last_time: None,
        shown: None,
        upload: None,
        rgba: Vec::new(),
    })
}

fn texture(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atd-movie-frame"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl MovieIn {
    /// Uploads `frame`, reusing the texture when its shape is unchanged.
    fn upload(
        &mut self,
        ctx: &CookCtx<'_>,
        frame: &HapFrame,
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        let (format, mode, bytes, tw, th, rows, bpr) = if frame.format == HapFormat::Rgba8 {
            (
                wgpu::TextureFormat::Rgba8Unorm,
                0.0,
                &frame.data[..],
                w,
                h,
                h,
                w * 4,
            )
        } else if self.bc {
            let (fmt, mode) = match frame.format {
                HapFormat::Bc1 => (wgpu::TextureFormat::Bc1RgbaUnorm, 0.0),
                HapFormat::Bc3 => (wgpu::TextureFormat::Bc3RgbaUnorm, 0.0),
                HapFormat::YCoCgBc3 => (wgpu::TextureFormat::Bc3RgbaUnorm, 1.0),
                HapFormat::Bc7 => (wgpu::TextureFormat::Bc7RgbaUnorm, 0.0),
                HapFormat::Bc4 => (wgpu::TextureFormat::Bc4RUnorm, 2.0),
                HapFormat::Rgba8 => unreachable!("handled above"),
            };
            let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
            let bpr = bw * frame.format.block_bytes() as u32;
            (fmt, mode, &frame.data[..], bw * 4, bh * 4, bh, bpr)
        } else {
            bc::to_rgba(frame, w, h, &mut self.rgba)
                .ok_or_else(|| "HAP R (BC7) needs a GPU with BC texture compression".to_owned())?;
            (
                wgpu::TextureFormat::Rgba8Unorm,
                0.0,
                &self.rgba[..],
                w,
                h,
                h,
                w * 4,
            )
        };
        let reuse = self.upload.as_ref().is_some_and(|u| {
            u.texture.format() == format && u.texture.width() == tw && u.texture.height() == th
        });
        if !reuse {
            self.upload = Some(Upload {
                texture: texture(ctx.device, tw, th, format),
                mode,
                uv: [w as f32 / tw as f32, h as f32 / th as f32],
            });
        }
        let upload = self.upload.as_mut().expect("set above");
        upload.mode = mode;
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &upload.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}

impl GpuOp for MovieIn {
    fn output_size(
        &self,
        p: &atd_gpu::engine::Params<'_>,
        _input: Option<(u32, u32)>,
        _project: (u32, u32),
    ) -> Option<(u32, u32)> {
        if p.str("size") != "native" {
            return None;
        }
        let info = self.player.as_ref()?.info()?;
        Some((info.width, info.height))
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let path = p.str("file");
        if path != self.path {
            self.path = path.to_owned();
            self.player = (!path.is_empty()).then(|| MoviePlayer::open(path));
            self.shown = None;
            self.upload = None;
            self.playing = false;
        }
        let dt = self.last_time.map_or(0.0, |t| (ctx.time - t).max(0.0));
        self.last_time = Some(ctx.time);
        let Some(player) = &self.player else {
            clear(ctx.encoder, ctx.output);
            return Ok(());
        };
        if let Some(e) = player.error() {
            clear(ctx.encoder, ctx.output);
            return Err(e.to_owned());
        }
        let Some(info) = player.info() else {
            clear(ctx.encoder, ctx.output);
            return Ok(());
        };

        // Playhead: Cue while stopped; advances from Cue when playing.
        let cue = f64::from(p.f32("cue"));
        let play = p.bool("play");
        if !play || !self.playing {
            self.playhead = cue;
        } else {
            self.playhead += dt * f64::from(p.f32("speed"));
        }
        self.playing = play;
        let duration = info.duration.max(1e-6);
        if p.bool("loop") {
            self.playhead = self.playhead.rem_euclid(duration);
        } else {
            self.playhead = self.playhead.clamp(0.0, duration - 1e-6);
        }
        let index = info.frame_at(self.playhead);
        let (w, h) = (info.width, info.height);
        player.request(index);
        if self.shown != Some(index)
            && let Some(frame) = player.frame(index)
        {
            self.upload(ctx, &frame, w, h)?;
            self.shown = Some(index);
        }
        let Some(upload) = &self.upload else {
            // First frame still decoding.
            clear(ctx.encoder, ctx.output);
            return Ok(());
        };
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&upload.texture],
            &[[upload.mode, upload.uv[0], upload.uv[1], 0.0]],
            ctx.output,
        );
        Ok(())
    }
}
