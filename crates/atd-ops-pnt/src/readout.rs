//! PNT outputs (PRD §6 "Outputs"): To Channels (point attributes → SIG) and
//! Pixel Out (points → DMX/LED). Both are GPU→SIG operators (PRD A65): the
//! cook records a copy of one point attribute into a ring of staging buffers
//! ([`PointReadback`]) that is mapped after the frame is submitted and read
//! on a later cook without waiting (rule 1), so live channels trail the
//! points by one frame or two. Offline the cook submits and waits for its
//! own copy, so frame `i` outputs frame `i`'s points (`docs/render.md`).
//! After warm-up the channels, their names and the staging buffers are
//! reused (rule 2).

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{GpuToSigCtx, GpuToSigOp};
use atd_gpu::geo::AttrClass;
use atd_gpu::geo::readback::{PointReadback, Recorded};
use atd_io::fixture::Mapping;
use atd_ops_img::DmxSenders;
use atd_ops_img::watch::{Watcher, watch};

/// Most points To Channels reads (its `max` parameter's ceiling).
pub const MAX_POINTS: i64 = 4096;

/// A readback taking longer than this is reported as an error.
const STUCK: std::time::Duration = std::time::Duration::from_secs(5);

fn schema(
    id: &str,
    label: &str,
    summary: &str,
    params: Vec<ParamSchema>,
    cook_budget_us: u32,
    sink: bool,
) -> OperatorSchema {
    OperatorSchema {
        type_id: format!("pnt.{id}"),
        family: OpFamily::Pnt,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: vec![SocketSchema {
            name: "in".into(),
            wire: WireType::Geo,
            optional: true,
            feedback: false,
        }],
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Sig,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        cook_budget_us,
        time_dependent: false,
        sink,
    }
}

pub fn to_channels_schema() -> OperatorSchema {
    schema(
        "to_channels",
        "To Channels",
        "Reads one point attribute back from the GPU as channels: one channel per point and component (`P0_x`, `P0_y`, …), or one channel per component with a sample per point. At most `max` points. Values trail the points by a frame or two live (GPU readback).",
        vec![
            ParamSchema::string("attr", "Attribute", "P")
                .help("The point attribute to read (`P`, `Cd`, `v`, `age`, any float or integer attribute)."),
            ParamSchema::menu("layout", "Layout", &["points", "samples"], "points")
                .help("`points`: one channel per point and component, `<attr><i>_<c>` (`P0_x`; `age3` for one component), at control rate. `samples`: one channel per component, `<attr>_<c>` (`P_x`; `age`), each with one sample per point in point order, like a Pattern. Components are `x y z w`, or `r g b a` for `Cd`."),
            ParamSchema::int("max", "Max Points", 64, Some(1), Some(MAX_POINTS))
                .help("Reads the first `max` points (of a simulation, the first `max` live ones). Caps the channel count at `max` × 4."),
        ],
        60,
        false,
    )
}

const PROTOCOLS: &[&str] = &["off", "artnet", "sacn"];

pub fn pixel_out_schema() -> OperatorSchema {
    schema(
        "pixel_out",
        "Pixel Out",
        "Sends points to DMX fixtures and LED pixels: point `i`'s colour drives the `i`-th pixel of a DMX patch file (fixtures in file order). Outputs the DMX slot values (0–1) as channels `u<universe>_<slot>`; optionally sends every patched universe over Art-Net or sACN. Values trail the points by a frame or two live (GPU readback).",
        vec![
            ParamSchema::path("patch", "Patch")
                .help("The patch file (TOML, `docs/dmx.md`): fixtures with type, universe, address and count. Points fill the pixels in file order; the regions Pixel Map samples are ignored, so one patch serves both. Fixture types come from the project's `fixtures/` folder, then the built-ins."),
            ParamSchema::string("attr", "Colour", "Cd")
                .help("The point attribute giving each pixel's colour (RGB 0–1; a one-component attribute is grey). Points without one, and pixels past the last point, are black."),
            ParamSchema::int("universe", "Channels Of", 0, Some(0), Some(63_999))
                .help("The universe whose slots become output channels (to wire into DMX Out, which sends one universe); 0 outputs every patched universe in order."),
            ParamSchema::menu("protocol", "Send", PROTOCOLS, "off")
                .help("`off`: channels only (wire them to DMX Out); `artnet` or `sacn`: also send every patched universe (at most 44 times a second, repeated every second)."),
            ParamSchema::string("host", "Host", "")
                .help("Destination node; empty broadcasts (Art-Net) or uses each universe's multicast group (sACN)."),
            ParamSchema::int("port", "Port", 0, Some(0), Some(65_535))
                .help("UDP port; 0 for the protocol's standard port."),
        ],
        80,
        true,
    )
}

/// Reads one point attribute of GEO input 0 back (shared by both operators).
#[derive(Debug, Default)]
struct Reader {
    rb: PointReadback,
    /// The attribute and limit of the last copy, to see changes.
    attr: String,
    max: u32,
    /// The values copied last (`Attribute::values_id`; holding the
    /// attribute would keep its buffer out of the pool).
    last: Option<usize>,
    /// Points changed while every staging buffer was busy.
    stale: bool,
}

impl Reader {
    /// Takes arrived values and records a new copy when the points may have
    /// changed. `Ok(false)` when there are no points to read (no input, or
    /// an empty one).
    fn read(&mut self, ctx: &mut GpuToSigCtx<'_>, attr: &str, max: u32) -> Result<bool, String> {
        let offline = ctx.params.mode().is_offline();
        if !offline {
            self.rb.receive(ctx.device)?;
        }
        let Some(geo) = ctx.geo_inputs.get(0).filter(|g| g.points() > 0) else {
            self.rb.clear();
            self.last = None;
            return Ok(false);
        };
        let Some(current) = geo.attr(AttrClass::Point, attr) else {
            self.rb.clear();
            self.last = None;
            self.stale = false;
            return Err(format!("the points have no attribute `{attr}`"));
        };
        let changed = self.stale
            || ctx.inputs_cooked
            || self.attr != attr
            || self.max != max
            || self.last != Some(current.values_id());
        if changed {
            match self
                .rb
                .record(ctx.device, ctx.encoder, geo, attr, max, ctx.frame)
            {
                Recorded::Copy | Recorded::Now => {
                    if offline && self.rb.busy() {
                        self.rb.finish_now(ctx.device, ctx.queue, ctx.encoder)?;
                    }
                    self.stale = false;
                    if self.attr != attr {
                        self.attr.clear();
                        self.attr.push_str(attr);
                    }
                    self.max = max;
                    self.last = Some(current.values_id());
                }
                Recorded::Busy => self.stale = true,
                Recorded::Missing => {
                    // Checked above; an attribute neither on the GPU nor
                    // the CPU has no values to read.
                    self.rb.clear();
                    self.last = None;
                    return Err(format!("`{attr}` has no values"));
                }
            }
        }
        ctx.cook_again |= self.stale || self.rb.busy();
        if let Some(waited) = self.rb.oldest().filter(|t| *t > STUCK) {
            return Err(format!(
                "GPU readback has not completed for {:.1} s",
                waited.as_secs_f32()
            ));
        }
        Ok(true)
    }
}

/// Component suffixes: colour attributes use `r g b a`.
fn suffix(attr: &str, c: u8) -> &'static str {
    let colour = attr == atd_core::geo::names::CD;
    match (colour, c) {
        (true, 0) => "r",
        (true, 1) => "g",
        (true, 2) => "b",
        (true, _) => "a",
        (false, 0) => "x",
        (false, 1) => "y",
        (false, 2) => "z",
        (false, _) => "w",
    }
}

struct ToChannels {
    reader: Reader,
    /// Channel names of the current shape, and the shape they are for.
    names: Vec<String>,
    shape: Option<(String, bool, u32, u8)>,
}

pub fn create_to_channels(_: &wgpu::Device) -> Box<dyn GpuToSigOp> {
    Box::new(ToChannels {
        reader: Reader::default(),
        names: Vec::new(),
        shape: None,
    })
}

impl ToChannels {
    /// Rebuilds the channel names when the attribute, layout, point count
    /// or component count changes (not in steady state).
    fn names(&mut self, attr: &str, per_point: bool, points: u32, comps: u8) {
        let same = self.shape.as_ref().is_some_and(|(a, p, n, c)| {
            a == attr && *p == per_point && *n == points && *c == comps
        });
        if same {
            return;
        }
        self.names.clear();
        let part = |c: u8| {
            if comps == 1 {
                String::new()
            } else {
                format!("_{}", suffix(attr, c))
            }
        };
        if per_point {
            for i in 0..points {
                for c in 0..comps {
                    self.names.push(format!("{attr}{i}{}", part(c)));
                }
            }
        } else {
            for c in 0..comps {
                self.names.push(format!("{attr}{}", part(c)));
            }
        }
        self.shape = Some((attr.to_owned(), per_point, points, comps));
    }
}

impl GpuToSigOp for ToChannels {
    fn cook(&mut self, ctx: &mut GpuToSigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let attr = p.str("attr");
        let max = p.i64("max").clamp(1, MAX_POINTS) as u32;
        let per_point = p.str("layout") != "samples";
        let read = self.reader.read(ctx, attr, max);
        let rb = &self.reader.rb;
        if !matches!(read, Ok(true)) || rb.frame().is_none() {
            ctx.output.set_single(&[], &[]);
            return read.map(|_| ());
        }
        let (n, comps) = (rb.count(), rb.ty().comps());
        self.names(attr, per_point, n, comps);
        let rb = &self.reader.rb;
        if per_point {
            let mut w = ctx.output.writer();
            let mut names = self.names.iter();
            for i in 0..n {
                for c in 0..comps {
                    let name = names.next().expect("one name per point and component");
                    w.push(name, rb.value(i, c));
                }
            }
        } else {
            let mut refs = [""; 4];
            for (r, n) in refs.iter_mut().zip(&self.names) {
                *r = n;
            }
            ctx.output
                .set_block(&refs[..usize::from(comps)], 0.0, n as usize);
            for c in 0..comps {
                for (i, v) in ctx
                    .output
                    .channel_mut(usize::from(c))
                    .iter_mut()
                    .enumerate()
                {
                    *v = rb.value(i as u32, c);
                }
            }
        }
        Ok(())
    }

    fn after_submit(&mut self) {
        self.reader.rb.start();
    }
}

/// A laid-out patch and its slot buffers.
struct Patched {
    mapping: Mapping,
    colours: Vec<[f32; 4]>,
    universes: Vec<Vec<u8>>,
    /// Channel names, `u<universe>_<slot>`, per universe.
    names: Vec<Vec<String>>,
}

impl Patched {
    fn new(mapping: Mapping) -> Self {
        let universes = mapping
            .universes
            .iter()
            .map(|&(_, len)| vec![0u8; len])
            .collect();
        let names = mapping
            .universes
            .iter()
            .map(|&(u, len)| (1..=len).map(|s| format!("u{u}_{s}")).collect())
            .collect();
        Patched {
            colours: vec![[0.0, 0.0, 0.0, 1.0]; mapping.cells.len()],
            mapping,
            universes,
            names,
        }
    }
}

struct PixelOut {
    reader: Reader,
    watcher: Option<Watcher<Mapping>>,
    patch: Option<Patched>,
    patch_error: Option<String>,
    senders: DmxSenders,
    /// The readback frame the slots were last filled from.
    filled: Option<u64>,
}

pub fn create_pixel_out(_: &wgpu::Device) -> Box<dyn GpuToSigOp> {
    Box::new(PixelOut {
        reader: Reader::default(),
        watcher: None,
        patch: None,
        patch_error: None,
        senders: DmxSenders::default(),
        filled: None,
    })
}

impl PixelOut {
    /// Picks up new versions of the patch file from its watcher thread.
    fn watch_patch(&mut self, ctx: &mut GpuToSigCtx<'_>) {
        let p = ctx.params;
        let file = p.str("patch");
        if file.is_empty() {
            self.watcher = None;
            self.patch = None;
            self.patch_error = None;
            return;
        }
        if self.watcher.as_ref().is_none_or(|w| w.path != file) {
            let fixtures = p.resolve("fixtures");
            self.watcher = Some(watch(
                file.to_owned(),
                p.resolve(file),
                "atd-pixel-out-patch",
                move |text| {
                    let patch = atd_io::fixture::load_patch(text, &fixtures)?;
                    Mapping::new(&patch)
                },
            ));
            self.patch = None;
            self.patch_error = None;
        }
        let Some(w) = &self.watcher else { return };
        let mode = p.mode();
        let mut latest = None;
        if mode.is_offline() {
            // Offline the file is read once, as it was when the render started.
            if self.patch.is_none() && self.patch_error.is_none() {
                latest = Some(
                    mode.receive(&w.rx)
                        .unwrap_or_else(|_| Err("patch file watcher stopped".into())),
                );
            }
        } else {
            while let Ok(r) = w.rx.try_recv() {
                latest = Some(r);
            }
            // Keep looking for saves.
            ctx.cook_again = true;
        }
        match latest {
            Some(Ok(mapping)) => {
                self.patch = Some(Patched::new(mapping));
                self.patch_error = None;
                self.filled = None;
            }
            Some(Err(e)) => {
                self.patch = None;
                self.patch_error = Some(format!("{file}: {e}"));
            }
            None => {}
        }
    }
}

impl GpuToSigOp for PixelOut {
    fn cook(&mut self, ctx: &mut GpuToSigCtx<'_>) -> Result<(), String> {
        self.watch_patch(ctx);
        if let Some(e) = &self.patch_error {
            ctx.output.set_single(&[], &[]);
            return Err(e.clone());
        }
        let Some(patch) = &mut self.patch else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        self.senders
            .update(&ctx.params, "Pixel Out", &patch.mapping.universes)?;
        let attr = ctx.params.str("attr");
        let pixels = patch.mapping.cells.len() as u32;
        let read = self.reader.read(ctx, attr, pixels.max(1));
        let rb = &self.reader.rb;
        let fresh = rb.frame().is_some() && rb.frame() != self.filled;
        if fresh || (self.filled.is_some() && rb.frame().is_none()) {
            // Point `i` lights pixel `i`; pixels past the last point are black.
            let comps = rb.ty().comps();
            for (i, c) in patch.colours.iter_mut().enumerate() {
                let i = i as u32;
                let v = |k: u8| rb.value(i, k);
                *c = match comps {
                    1 => [v(0), v(0), v(0), 1.0],
                    2 => [v(0), v(1), 0.0, 1.0],
                    _ => [v(0), v(1), v(2), 1.0],
                };
            }
            patch.mapping.fill(&patch.colours, &mut patch.universes);
            self.senders.publish(&patch.universes);
            self.filled = rb.frame();
        }
        let only = ctx.params.i64("universe");
        let mut w = ctx.output.writer();
        for ((data, names), (u, _)) in patch
            .universes
            .iter()
            .zip(&patch.names)
            .zip(&patch.mapping.universes)
        {
            if only != 0 && i64::from(*u) != only {
                continue;
            }
            for (v, name) in data.iter().zip(names) {
                w.push(name, f32::from(*v) / 255.0);
            }
        }
        drop(w);
        read.map(|_| ())
    }

    fn after_submit(&mut self) {
        self.reader.rb.start();
    }
}
