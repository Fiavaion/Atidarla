//! Channel Image: SIG channels → IMG (PRD §6, "SIG to IMG").
//!
//! Two layouts. **Samples** puts samples along x, one row per channel, with
//! channel 0 at the bottom (the layout of ISF `audio` images). **Channels**
//! puts channels along x, one row per sample, which makes a row of
//! Spectrum bands into an ISF `audioFFT`-style image. Values are written to
//! an internal `R16Float` texture and stretched over the output, so they
//! keep their precision until the final conversion.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

/// Widest internal texture; longer blocks are decimated.
const MAX_WIDTH: usize = 4096;
const LAYOUTS: &[&str] = &["samples", "channels"];

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.channel_image".into(),
        family: OpFamily::Img,
        label: "Channel Image".into(),
        summary: "Draws SIG channels as an image: waveforms (samples along x, a row per channel) or band rows (channels along x). Feeds ISF audio inputs.".into(),
        version: 1,
        params: vec![
            ParamSchema::menu("layout", "Layout", LAYOUTS, "samples").help(
                "`samples`: samples along x, one row per channel (channel 0 at the bottom). `channels`: channels along x, one row per sample.",
            ),
            ParamSchema::float("scale", "Scale", 1.0, None, None)
                .help("Multiplies each value before it is written."),
            ParamSchema::float("offset", "Offset", 0.0, None, None)
                .help("Added after scaling; 0.5 with scale 0.5 maps a −1…1 waveform to 0…1."),
            ParamSchema::toggle("interpolate", "Interpolate", true)
                .help("Blend linearly between neighbouring values; off shows hard steps."),
        ],
        inputs: vec![SocketSchema {
            name: "sig".into(),
            wire: WireType::Sig,
            optional: true,
            feedback: false,
        }],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us: 80,
        time_dependent: false,
        sink: false,
    }
}

const BODY: &str = r#"
    let size = vec2f(textureDimensions(in0));
    // Rows (channels or samples) never blend into each other.
    let row = min(floor(uv.y * size.y), size.y - 1.0);
    var v: f32;
    if (params.v[0].x > 0.5) {
        // Texel centres map to the output's left and right edges.
        let p = vec2f((uv.x * (size.x - 1.0) + 0.5) / size.x, (row + 0.5) / size.y);
        v = textureSampleLevel(in0, samp, p, 0.0).r;
    } else {
        let col = min(floor(uv.x * size.x), size.x - 1.0);
        v = textureLoad(in0, vec2i(i32(col), i32(row)), 0).r;
    }
    return vec4f(vec3f(v), 1.0);
"#;

/// Runs Channel Image.
#[derive(Debug)]
pub struct ChannelImage {
    pass: FullscreenPass,
    data: wgpu::Texture,
    /// Little-endian half floats, reused between cooks.
    staging: Vec<u8>,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(ChannelImage {
        pass: FullscreenPass::new(device, "img.channel_image", format, 1, "", BODY),
        data: data_texture(device, 1, 1),
        staging: Vec::new(),
    })
}

fn data_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atd-channel-image"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl GpuOp for ChannelImage {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let scale = ctx.params.f32("scale");
        let offset = ctx.params.f32("offset");
        let by_channel = ctx.params.str("layout") == "channels";
        let (channels, samples) = match ctx.sig_inputs.get(0) {
            Some(ch) if !ch.is_empty() => (ch.len(), ch.samples().max(1)),
            _ => (0, 0),
        };
        // Texture size: (columns, rows) and the sample stride along x.
        let step = if by_channel {
            1
        } else {
            samples.div_ceil(MAX_WIDTH).max(1)
        };
        let (w, h) = match (channels, by_channel) {
            (0, _) => (1, 1),
            (c, true) => (c.min(MAX_WIDTH), samples.min(MAX_WIDTH)),
            (c, false) => (samples.div_ceil(step), c.min(MAX_WIDTH)),
        };
        self.staging.clear();
        self.staging.resize(w * h * 2, 0);
        if let Some(ch) = ctx.sig_inputs.get(0).filter(|_| channels > 0) {
            // Row 0 of the texture is the top of the image; put channel or
            // sample 0 at the bottom (y up, as in ISF).
            for row in 0..h {
                let dst = &mut self.staging[(h - 1 - row) * w * 2..(h - row) * w * 2];
                for (col, out) in dst.chunks_exact_mut(2).enumerate() {
                    let v = if by_channel {
                        ch.channel(col)[row]
                    } else {
                        ch.channel(row)[(col * step).min(samples - 1)]
                    };
                    out.copy_from_slice(&f16_bits(v * scale + offset).to_le_bytes());
                }
            }
        }
        if self.data.width() != w as u32 || self.data.height() != h as u32 {
            self.data = data_texture(ctx.device, w as u32, h as u32);
        }
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.data,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.staging,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w as u32 * 2),
                rows_per_image: Some(h as u32),
            },
            wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
        );
        let interpolate = f32::from(u8::from(ctx.params.bool("interpolate")));
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&self.data],
            &[[interpolate, 0.0, 0.0, 0.0]],
            ctx.output,
        );
        Ok(())
    }
}

/// IEEE 754 binary16 bits of `v`, rounded to nearest even; values beyond
/// the half range become ±infinity and tiny values flush to zero.
pub(crate) fn f16_bits(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let man = x & 0x7f_ffff;
    if exp == 0xff {
        // Inf or NaN (keep a quiet NaN bit).
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal half.
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let round = u32::from(rem > mid || (rem == mid && half & 1 == 1));
        return sign | (half + round) as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    let round = u32::from(rem > 0x1000 || (rem == 0x1000 && half & 1 == 1));
    sign | (half + round) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16_bits(0.0), 0x0000);
        assert_eq!(f16_bits(-0.0), 0x8000);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(-2.0), 0xc000);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(65504.0), 0x7bff);
        assert_eq!(f16_bits(1.0e6), 0x7c00);
        assert_eq!(f16_bits(f32::NEG_INFINITY), 0xfc00);
        assert_eq!(f16_bits(f32::NAN) & 0x7e00, 0x7e00);
        // Smallest subnormal half, 2^-24.
        assert_eq!(f16_bits(5.960_464_5e-8), 0x0001);
        assert_eq!(f16_bits(1.0e-10), 0x0000);
        // 1 + 2^-11 is halfway between 1 and the next half: ties to even.
        assert_eq!(f16_bits(1.000_488_3), 0x3c00);
        assert_eq!(f16_bits(0.333_333_34), 0x3555);
    }
}
