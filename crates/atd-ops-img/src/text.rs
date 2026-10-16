//! Text (PRD §6, IMG generators): lines of text in a built-in or file font.
//!
//! The text block is rasterised to a coverage texture on a worker thread
//! (font files are read there too, rule 1) whenever the text, font, size or
//! output height changes. Position, alignment and colours are applied in
//! the shader, so animating them costs nothing extra. While a new raster is
//! on its way the previous one stays on screen.

use std::sync::mpsc::{Receiver, channel};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;

const FONTS: &[&str] = &["sans", "mono"];
const ALIGN: &[&str] = &["left", "centre", "right"];
const VALIGN: &[&str] = &["top", "middle", "bottom"];
/// Largest raster side (textures this big are supported everywhere).
const MAX_SIDE: u32 = 8192;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "img.text".into(),
        family: OpFamily::Img,
        label: "Text".into(),
        summary: "Draws text (several lines allowed) over the input or a background colour, in a built-in or TrueType/OpenType font.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("text", "Text", "Atidarla").help("The text; `\\n` starts a new line."),
            ParamSchema::menu("font", "Font", FONTS, "sans")
                .help("Built-in font: `sans` (Ubuntu Light) or `mono` (Hack)."),
            ParamSchema::string("font_file", "Font File", "")
                .help("Path to a .ttf or .otf file; overrides Font when set."),
            ParamSchema::float("size", "Size", 0.1, Some(0.0), None)
                .help("Line height as a fraction of the image height."),
            ParamSchema::float("spacing", "Line Spacing", 1.0, Some(0.0), None)
                .help("Distance between lines, as a multiple of the font's line height."),
            ParamSchema::vec("position", "Position", &[0.5, 0.5])
                .help("Anchor point as a fraction of the image from the bottom-left."),
            ParamSchema::menu("align", "Align", ALIGN, "centre")
                .help("Horizontal alignment of the lines and of the block on the anchor."),
            ParamSchema::menu("valign", "Vertical Align", VALIGN, "middle")
                .help("Which part of the block sits on the anchor."),
            ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0]).help("Text colour."),
            ParamSchema::colour("background", "Background", [0.0, 0.0, 0.0, 0.0])
                .help("Colour behind the text when nothing is connected to `in`."),
        ],
        inputs: vec![SocketSchema {
            name: "in".into(),
            wire: WireType::Img,
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
    let base = select(params.v[2], textureSample(in0, samp, uv), params.v[3].x > 0.5);
    let r = params.v[0];
    let t = (uv - r.xy) / max(r.zw - r.xy, vec2f(1e-6));
    let inside = all(t >= vec2f(0.0)) && all(t <= vec2f(1.0));
    let c = textureSampleLevel(in1, samp, clamp(t, vec2f(0.0), vec2f(1.0)), 0.0).r;
    let a = select(0.0, c, inside) * params.v[1].a;
    return vec4f(mix(base.rgb, params.v[1].rgb, a), base.a + a * (1.0 - base.a));
"#;

/// What a raster depends on.
#[derive(Debug, Clone, PartialEq)]
struct Job {
    text: String,
    font: String,
    font_file: String,
    px: u32,
    spacing: f32,
    align: String,
}

/// A rasterised text block: coverage, one byte per pixel.
#[derive(Debug, Clone, Default)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

fn load_font(job: &Job) -> Result<FontArc, String> {
    if !job.font_file.is_empty() {
        let bytes = std::fs::read(&job.font_file).map_err(|e| format!("{}: {e}", job.font_file))?;
        return FontArc::try_from_vec(bytes).map_err(|e| format!("{}: {e}", job.font_file));
    }
    let data = if job.font == "mono" {
        epaint_default_fonts::HACK_REGULAR
    } else {
        epaint_default_fonts::UBUNTU_LIGHT
    };
    FontArc::try_from_slice(data).map_err(|e| e.to_string())
}

/// Lays out and rasterises `text` with lines `px` pixels tall.
pub fn rasterise(font: &FontArc, text: &str, px: f32, spacing: f32, align: &str) -> Raster {
    let sf = font.as_scaled(PxScale::from(px));
    let line_height = (sf.ascent() - sf.descent() + sf.line_gap()) * spacing;
    let pad = 2.0;
    // Glyph positions per line (x relative to the line start).
    let mut lines: Vec<(Vec<(ab_glyph::GlyphId, f32)>, f32)> = Vec::new();
    for line in text.split('\n') {
        let mut glyphs = Vec::new();
        let mut x = 0.0;
        let mut prev = None;
        for ch in line.chars() {
            let id = sf.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            glyphs.push((id, x));
            x += sf.h_advance(id);
            prev = Some(id);
        }
        lines.push((glyphs, x));
    }
    let widest = lines.iter().map(|(_, w)| *w).fold(0.0f32, f32::max);
    let text_h = sf.ascent() - sf.descent() + line_height * (lines.len() as f32 - 1.0);
    let width = ((widest + 2.0 * pad).ceil() as u32).clamp(1, MAX_SIDE);
    let height = ((text_h + 2.0 * pad).ceil() as u32).clamp(1, MAX_SIDE);
    let mut coverage = vec![0u8; (width * height) as usize];
    for (row, (glyphs, line_w)) in lines.iter().enumerate() {
        let shift = match align {
            "left" => 0.0,
            "right" => widest - line_w,
            _ => (widest - line_w) / 2.0,
        };
        let baseline = pad + sf.ascent() + line_height * row as f32;
        for (id, x) in glyphs {
            let glyph =
                id.with_scale_and_position(sf.scale(), ab_glyph::point(pad + shift + x, baseline));
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let (x, y) = (
                    b.min.x as i64 + i64::from(gx),
                    b.min.y as i64 + i64::from(gy),
                );
                if x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height {
                    let i = (y as u32 * width + x as u32) as usize;
                    let v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                    coverage[i] = coverage[i].max(v);
                }
            });
        }
    }
    Raster {
        width,
        height,
        coverage,
    }
}

fn run(job: &Job) -> Result<Raster, String> {
    let font = load_font(job)?;
    Ok(rasterise(
        &font,
        &job.text,
        job.px as f32,
        job.spacing,
        &job.align,
    ))
}

#[derive(Debug)]
pub struct Text {
    pass: FullscreenPass,
    /// Job of the texture on screen, and of the raster being made.
    shown: Option<Job>,
    pending: Option<(Job, Receiver<Result<Raster, String>>)>,
    texture: Option<wgpu::Texture>,
    error: Option<String>,
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Text {
        pass: FullscreenPass::new(device, "img.text", format, 2, "", BODY),
        shown: None,
        pending: None,
        texture: None,
        error: None,
    })
}

impl Text {
    fn upload(&mut self, ctx: &CookCtx<'_>, r: &Raster) {
        let size = wgpu::Extent3d {
            width: r.width,
            height: r.height,
            depth_or_array_layers: 1,
        };
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atd-text"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &r.coverage,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(r.width),
                rows_per_image: Some(r.height),
            },
            size,
        );
        self.texture = Some(texture);
    }
}

impl GpuOp for Text {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let out_h = ctx.output.height() as f32;
        let out_w = ctx.output.width() as f32;
        let px = (p.f32("size").max(0.0) * out_h).round().clamp(1.0, 2048.0) as u32;
        let wanted_matches = |j: &Job| {
            j.text == p.str("text")
                && j.font == p.str("font")
                && j.font_file == p.str("font_file")
                && j.px == px
                && j.spacing == p.f32("spacing")
                && j.align == p.str("align")
        };
        let up_to_date = self.shown.as_ref().is_some_and(wanted_matches)
            || self
                .pending
                .as_ref()
                .is_some_and(|(j, _)| wanted_matches(j));
        if !up_to_date {
            let job = Job {
                text: p.str("text").to_owned(),
                font: p.str("font").to_owned(),
                font_file: p.str("font_file").to_owned(),
                px,
                spacing: p.f32("spacing"),
                align: p.str("align").to_owned(),
            };
            let (tx, rx) = channel();
            let work = job.clone();
            std::thread::Builder::new()
                .name("atd-text".into())
                .spawn(move || {
                    let _ = tx.send(run(&work));
                })
                .map_err(|e| format!("cannot start text thread: {e}"))?;
            self.pending = Some((job, rx));
        }
        if let Some((job, rx)) = &self.pending {
            match rx.try_recv() {
                Ok(Ok(raster)) => {
                    let job = job.clone();
                    self.upload(ctx, &raster);
                    self.shown = Some(job);
                    self.pending = None;
                    self.error = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.shown = self.pending.take().map(|(j, _)| j);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.cook_again = true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some("text rasteriser stopped".into());
                }
            }
        }

        // Block size in uv, from its pixel size; anchor from the bottom-left.
        let (bw, bh) = self.texture.as_ref().map_or((0.0, 0.0), |t| {
            (t.width() as f32 / out_w, t.height() as f32 / out_h)
        });
        let pos = p.vec4("position");
        let (ax, ay) = (pos[0], 1.0 - pos[1]);
        let x0 = match p.str("align") {
            "left" => ax,
            "right" => ax - bw,
            _ => ax - bw / 2.0,
        };
        let y0 = match p.str("valign") {
            "top" => ay,
            "bottom" => ay - bh,
            _ => ay - bh / 2.0,
        };
        let has_input = ctx.inputs.first().is_some_and(Option::is_some);
        let input = ctx.input_or_black(0).clone();
        let coverage = self.texture.as_ref().unwrap_or(ctx.black);
        let (x1, y1) = if self.texture.is_some() {
            (x0 + bw, y0 + bh)
        } else {
            (x0, y0)
        };
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&input, coverage],
            &[
                [x0, y0, x1, y1],
                p.vec4("colour"),
                p.vec4("background"),
                [f32::from(u8::from(has_input)), 0.0, 0.0, 0.0],
            ],
            ctx.output,
        );
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sans() -> FontArc {
        FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap()
    }

    #[test]
    fn raster_grows_with_text_and_lines() {
        let f = sans();
        let one = rasterise(&f, "Hi", 40.0, 1.0, "centre");
        let longer = rasterise(&f, "Hi there", 40.0, 1.0, "centre");
        let two = rasterise(&f, "Hi\nthere", 40.0, 1.0, "centre");
        assert!(longer.width > one.width * 2);
        assert!(two.height > one.height + 30);
        assert!(one.coverage.contains(&255), "solid strokes");
        let empty = rasterise(&f, "", 40.0, 1.0, "left");
        assert!(empty.coverage.iter().all(|v| *v == 0));
    }

    #[test]
    fn alignment_moves_short_lines() {
        let f = sans();
        // Column of the leftmost ink in the bottom (short) line.
        let first_ink = |r: &Raster| {
            let rows = r.height as usize;
            (0..r.width as usize)
                .find(|&x| {
                    ((rows * 2 / 3)..rows).any(|y| r.coverage[y * r.width as usize + x] > 128)
                })
                .unwrap()
        };
        let text = "WWWWWWWW\ni";
        let left = first_ink(&rasterise(&f, text, 32.0, 1.0, "left"));
        let centre = first_ink(&rasterise(&f, text, 32.0, 1.0, "centre"));
        let right = first_ink(&rasterise(&f, text, 32.0, 1.0, "right"));
        assert!(left < centre && centre < right, "{left} {centre} {right}");
    }
}
