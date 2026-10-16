//! Text (PRD §6, IMG generators): lines of text in a built-in or file font.
//! Outlines come from `skrifa` (the reader egui uses) and are filled with
//! `ab_glyph_rasterizer`. Glyphs advance without kerning or shaping.
//!
//! The text block is rasterised to a coverage texture on a worker thread
//! (font files are read there too, rule 1) whenever the text, font, size or
//! output height changes. Position, alignment and colours are applied in
//! the shader, so animating them costs nothing extra. While a new raster is
//! on its way the previous one stays on screen.

use std::sync::mpsc::{Receiver, channel};

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

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
            ParamSchema::string("text", "Text", "Atidarla")
                .help("The text; several lines are allowed.")
                .multiline(),
            ParamSchema::menu("font", "Font", FONTS, "sans")
                .help("Built-in font: `sans` (Ubuntu Light) or `mono` (Hack)."),
            ParamSchema::path("font_file", "Font File")
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
    /// As written in the parameter, and resolved to a file.
    font_file: String,
    font_path: std::path::PathBuf,
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

/// Font bytes: a bundled font or a file's contents.
fn load_font(job: &Job) -> Result<std::borrow::Cow<'static, [u8]>, String> {
    if !job.font_file.is_empty() {
        let bytes = std::fs::read(&job.font_path).map_err(|e| format!("{}: {e}", job.font_file))?;
        FontRef::new(&bytes).map_err(|e| format!("{}: {e}", job.font_file))?;
        return Ok(bytes.into());
    }
    Ok(if job.font == "mono" {
        epaint_default_fonts::HACK_REGULAR.into()
    } else {
        epaint_default_fonts::UBUNTU_LIGHT.into()
    })
}

/// Sends outline segments to the rasteriser, in raster space (y down) with
/// the glyph origin at `origin`.
struct Pen<'r> {
    raster: &'r mut Rasterizer,
    /// Rightmost column (width − 1).
    max_x: f32,
    origin: Point,
    current: Point,
    start: Point,
}

impl Pen<'_> {
    /// A raster-space point, kept inside the raster's columns. The
    /// rasteriser indexes rows as `row * width + x`: an x left of 0 or past
    /// the width lands in a neighbouring row (and overflows on huge values
    /// from a damaged font). NaN becomes 0, and y is bounded so curve
    /// subdivision, which grows with distance, stays cheap.
    fn at(&self, x: f32, y: f32) -> Point {
        let fit = |v: f32, hi: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, hi) };
        point(
            fit(self.origin.x + x, self.max_x),
            fit(self.origin.y - y, MAX_SIDE as f32),
        )
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.current = self.at(x, y);
        self.start = self.current;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.raster.draw_line(self.current, p);
        self.current = p;
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (c, p) = (self.at(cx, cy), self.at(x, y));
        self.raster.draw_quad(self.current, c, p);
        self.current = p;
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.raster.draw_cubic(self.current, c0, c1, p);
        self.current = p;
    }
    fn close(&mut self) {
        if self.current != self.start {
            self.raster.draw_line(self.current, self.start);
        }
        self.current = self.start;
    }
}

/// Lays out and rasterises `text` with lines `px` pixels tall.
pub fn rasterise(font: &FontRef<'_>, text: &str, px: f32, spacing: f32, align: &str) -> Raster {
    let size = Size::new(px);
    let location = LocationRef::default();
    let metrics = font.metrics(size, location);
    let advances = font.glyph_metrics(size, location);
    let charmap = font.charmap();
    let outlines = font.outline_glyphs();
    let (ascent, descent) = (metrics.ascent, metrics.descent);
    let line_height = (ascent - descent + metrics.leading) * spacing;
    let pad = 2.0;
    // Glyphs and their x offsets per line, and each line's width.
    let lines: Vec<(Vec<(GlyphId, f32)>, f32)> = text
        .split('\n')
        .map(|line| {
            let mut x = 0.0;
            let glyphs = line
                .chars()
                .map(|ch| {
                    let id = charmap.map(ch).unwrap_or(GlyphId::NOTDEF);
                    let at = x;
                    x += advances.advance_width(id).unwrap_or(0.0);
                    (id, at)
                })
                .collect();
            (glyphs, x)
        })
        .collect();
    let widest = lines.iter().map(|(_, w)| *w).fold(0.0f32, f32::max);
    let text_h = ascent - descent + line_height * (lines.len() as f32 - 1.0);
    let width = ((widest + 2.0 * pad).ceil() as u32).clamp(1, MAX_SIDE);
    let height = ((text_h + 2.0 * pad).ceil() as u32).clamp(1, MAX_SIDE);
    let mut raster = Rasterizer::new(width as usize, height as usize);
    for (row, (glyphs, line_w)) in lines.iter().enumerate() {
        let shift = match align {
            "left" => 0.0,
            "right" => widest - line_w,
            _ => (widest - line_w) / 2.0,
        };
        let baseline = pad + ascent + line_height * row as f32;
        for (id, x) in glyphs {
            let Some(glyph) = outlines.get(*id) else {
                continue;
            };
            let origin = point(pad + shift + x, baseline);
            let mut pen = Pen {
                raster: &mut raster,
                max_x: (width - 1) as f32,
                origin,
                current: origin,
                start: origin,
            };
            let _ = glyph.draw(DrawSettings::unhinted(size, location), &mut pen);
        }
    }
    let mut coverage = vec![0u8; (width * height) as usize];
    raster.for_each_pixel(|i, c| {
        coverage[i] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    });
    Raster {
        width,
        height,
        coverage,
    }
}

fn run(job: &Job) -> Result<Raster, String> {
    let bytes = load_font(job)?;
    render(&bytes, &job.text, job.px as f32, job.spacing, &job.align)
}

/// Parses `font` and rasterises `text`. The font parser can panic on some
/// damaged files; that becomes an error instead of a dead worker.
pub fn render(
    font: &[u8],
    text: &str,
    px: f32,
    spacing: f32,
    align: &str,
) -> Result<Raster, String> {
    let font = FontRef::new(font).map_err(|e| e.to_string())?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rasterise(&font, text, px, spacing, align)
    }))
    .map_err(|_| "the font file is damaged".to_string())
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
                font_path: p.resolve(p.str("font_file")),
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

    fn sans() -> FontRef<'static> {
        FontRef::new(epaint_default_fonts::UBUNTU_LIGHT).unwrap()
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
    #[test]
    fn outlines_outside_the_raster_stay_in_their_rows() {
        // A square from x = -5 to 3 over rows 1–3 of a 10×4 raster. Unclamped,
        // the part left of x = 0 lands at the end of the row above (and
        // overflows in debug builds); huge and NaN points must not panic.
        let mut raster = Rasterizer::new(10, 4);
        let mut pen = Pen {
            raster: &mut raster,
            max_x: 9.0,
            origin: point(0.0, 0.0),
            current: point(0.0, 0.0),
            start: point(0.0, 0.0),
        };
        pen.move_to(-5.0, -1.0);
        pen.line_to(3.0, -1.0);
        pen.line_to(3.0, -4.0);
        pen.line_to(-5.0, -4.0);
        pen.close();
        let mut coverage = vec![0.0f32; 40];
        raster.for_each_pixel(|i, c| coverage[i] = c);
        assert!(
            coverage[..10].iter().all(|c| c.abs() < 1e-3),
            "row 0 untouched: {coverage:?}"
        );
        for row in 1..4 {
            let line = &coverage[row * 10..row * 10 + 10];
            assert!(line[..3].iter().all(|c| (c - 1.0).abs() < 1e-3), "{line:?}");
            assert!(line[4..].iter().all(|c| c.abs() < 1e-3), "{line:?}");
        }
        // Huge, infinite and NaN points: no panic, no hang.
        let mut raster = Rasterizer::new(10, 4);
        let mut pen = Pen {
            raster: &mut raster,
            max_x: 9.0,
            origin: point(0.0, 0.0),
            current: point(0.0, 0.0),
            start: point(0.0, 0.0),
        };
        pen.move_to(1e30, f32::NAN);
        pen.quad_to(-1e30, 1e30, f32::INFINITY, -3.0);
        pen.curve_to(f32::NAN, 0.0, 1e20, -1e20, f32::NEG_INFINITY, 0.0);
        pen.close();
        raster.for_each_pixel(|_, _| {});
    }

    /// Damaged font files (bytes changed, cut, duplicated ranges, table
    /// fields forced to extremes) never panic or hang the rasteriser.
    #[test]
    fn mutated_fonts_never_panic_or_hang() {
        let mut seed = 0xbb67_ae85_84ca_a73b_u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let seeds = [
            epaint_default_fonts::UBUNTU_LIGHT,
            epaint_default_fonts::HACK_REGULAR,
        ];
        let rounds: usize = std::env::var("ATD_FUZZ_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let mut slowest = std::time::Duration::ZERO;
        for round in 0..rounds {
            let mut bytes = seeds[(rand() % 2) as usize].to_vec();
            let len = bytes.len();
            match rand() % 4 {
                0 => {
                    // Table directory and headers live near the start.
                    for _ in 0..1 + rand() % 16 {
                        let i = (rand() as usize) % len.min(4096);
                        bytes[i] = rand() as u8;
                    }
                }
                1 => {
                    for _ in 0..1 + rand() % 64 {
                        let i = (rand() as usize) % len;
                        bytes[i] = [0, 0x7F, 0x80, 0xFF][(rand() % 4) as usize];
                    }
                }
                2 => bytes.truncate((rand() as usize) % len),
                _ => {
                    let a = (rand() as usize) % len;
                    let b = (a + (rand() as usize) % 4096).min(len);
                    let chunk = bytes[a..b].to_vec();
                    let at = (rand() as usize) % len;
                    let end = (at + chunk.len()).min(len);
                    bytes[at..end].copy_from_slice(&chunk[..end - at]);
                }
            }
            let px = [8.0, 64.0, 512.0][(rand() % 3) as usize];
            let start = std::time::Instant::now();
            let r = render(&bytes, "Ag\nW@ 9é", px, 1.0, "centre");
            slowest = slowest.max(start.elapsed());
            let Ok(r) = r else {
                continue;
            };
            assert_eq!(
                r.coverage.len(),
                (r.width * r.height) as usize,
                "round {round}"
            );
        }
        assert!(slowest < std::time::Duration::from_secs(5), "{slowest:?}");
    }
}
