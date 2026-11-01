//! `cmp.window` (PRD §6 CMP "Window", FR-S6, decision A54): an output.
//! Its cook maps the input through the node's slices (warps, masks, soft
//! edges, test patterns; `lsr_gpu::output`) into an image of the output's
//! resolution, so headless checks and renders see exactly what a projector
//! gets. The app shows that image in a native window per enabled node, on
//! the chosen display; nothing in the cook touches a window, so the
//! operator is pure and runs the same on every platform.

use std::fmt::Write as _;

use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use lsr_gpu::engine::{CookCtx, GpuOp, Params};
use lsr_gpu::output::{OutputMap, OutputMapper, Pattern};

pub const TYPE_ID: &str = "cmp.window";

/// Default output resolution (a 1080p projector).
pub const DEFAULT_SIZE: [i64; 2] = [1920, 1080];

pub fn schema() -> OperatorSchema {
    let img = |name: &str| SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: TYPE_ID.into(),
        family: OpFamily::Cmp,
        label: "Window".into(),
        summary: "An output to a projector or screen: maps its input through slices (source rectangle to a warped destination: corner pin, grid or Bezier warp), masks and soft edges, or shows a test pattern, at the output's resolution. The app opens a native window per enabled Window on the chosen display; headless runs and renders produce the mapped image only.".into(),
        version: 1,
        params: vec![
            ParamSchema::toggle("enabled", "Enabled", true)
                .help("Opens the window in the app. The image is mapped either way (the Outputs panel previews it); closing the window turns this off."),
            ParamSchema::string("title", "Title", "Output")
                .help("The window's title (windowed mode)."),
            ParamSchema::int("display", "Display", 0, Some(0), Some(15))
                .help("The display a fullscreen window opens on, in the system's order (0 is the first). An index past the last display leaves the window where it is."),
            ParamSchema::toggle("fullscreen", "Fullscreen", false)
                .help("Borderless fullscreen on Display; off, a normal window at Position and Window Size."),
            ParamSchema::vec("position", "Position", &[80.0, 80.0])
                .help("Top-left corner of the window in screen points (windowed mode)."),
            ParamSchema::vec("window_size", "Window Size", &[960.0, 540.0])
                .help("Inner size of the window in points (windowed mode). The image is scaled to fill it."),
            ParamSchema::int("width", "Width", DEFAULT_SIZE[0], Some(0), Some(8192))
                .help("Width of the mapped image in pixels; set it to the display's. 0 uses the project resolution."),
            ParamSchema::int("height", "Height", DEFAULT_SIZE[1], Some(0), Some(8192))
                .help("Height of the mapped image in pixels; 0 uses the project resolution."),
            ParamSchema::menu("pattern", "Test Pattern", &Pattern::NAMES, "off")
                .help("Replaces the input in every slice to line projectors up: `grid` (shows the warp), `bars` (75% colour bars), `gradient` (shows edge blends), `circles` (each slice's colour and number)."),
            ParamSchema::string("slices", "Slices", "")
                .multiline()
                .help("The mapping, one `[slice name]` block per slice with `key = values` lines (x right, y down, fractions of the image): `source = x y w h`, `dst = x y w h`, `warp = bilinear|perspective|catmull_rom|bezier`, `grid = cols rows`, `points = x y …` (one line per row of control points), `blend = left right top bottom`, `blend_power`, `blend_gamma`, `mask = x y …`, `mask_invert`, `subdiv`. Empty: the whole input over the whole output. See docs/outputs.md."),
        ],
        inputs: vec![img("in"), img("mask")],
        outputs: vec![img("out")],
        capabilities: vec![],
        cook_budget_us: 150,
        time_dependent: false,
        sink: false,
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Window {
        mapper: OutputMapper::new(device, format),
        text: None,
        error: None,
        slices: 0,
        status: String::with_capacity(48),
        shown: None,
    })
}

struct Window {
    mapper: OutputMapper,
    /// The `slices` text the mapper holds (`None` before the first cook).
    text: Option<String>,
    /// Why that text did not parse.
    error: Option<String>,
    slices: usize,
    status: String,
    /// (slices, width, height) the status line was written for.
    shown: Option<(usize, u32, u32)>,
}

/// The output size from the `width` and `height` parameters (0: project).
pub fn output_size(p: &Params<'_>, project: (u32, u32)) -> (u32, u32) {
    let side = |v: i64, fallback: u32| {
        if v <= 0 { fallback } else { v.min(8192) as u32 }
    };
    (
        side(p.i64("width"), project.0),
        side(p.i64("height"), project.1),
    )
}

impl GpuOp for Window {
    fn output_size(
        &self,
        params: &Params<'_>,
        _input: Option<(u32, u32)>,
        project: (u32, u32),
    ) -> Option<(u32, u32)> {
        Some(output_size(params, project))
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let text = ctx.params.str("slices");
        if self.text.as_deref() != Some(text) {
            // Parsing and tessellating allocate: only when the text changed.
            self.text = Some(text.to_owned());
            match OutputMap::parse(text) {
                Ok(map) => {
                    self.mapper.set_map(ctx.device, ctx.queue, &map);
                    self.slices = map.slices.len();
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("slices: {e}")),
            }
        }
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let pattern = Pattern::from_name(ctx.params.str("pattern"));
        let (inputs, black) = (ctx.inputs, ctx.black);
        let source = inputs.first().and_then(Option::as_ref).unwrap_or(black);
        let mask = inputs.get(1).and_then(Option::as_ref);
        self.mapper.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            source,
            mask,
            black,
            pattern,
            ctx.output,
        );
        let now = (self.slices, ctx.output.width(), ctx.output.height());
        if self.shown != Some(now) {
            self.shown = Some(now);
            self.status.clear();
            let _ = write!(
                self.status,
                "{} slice{} · {}×{}",
                now.0,
                if now.0 == 1 { "" } else { "s" },
                now.1,
                now.2
            );
        }
        Ok(())
    }

    fn status(&self) -> Option<&str> {
        Some(self.status.as_str()).filter(|s| !s.is_empty())
    }
}
