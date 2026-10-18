//! Neural CA (PRD §6 IMG solvers, §9 M2): a neural cellular automaton on the
//! GPU, after Mordvintsev et al., "Growing Neural Cellular Automata"
//! (Distill, 2020). Implemented from the paper's description only.
//!
//! Every cell of a grid holds `channels` numbers: 0-2 are colour
//! (premultiplied), 3 is alpha ("alive" above a threshold) and the rest are
//! hidden. One step:
//!
//! 1. **Perceive**: for each channel, the cell's own value and three fixed
//!    3×3 filters (Sobel x, Sobel y, Laplacian), 4 × `channels` numbers.
//! 2. **Update**: a tiny MLP, `ds = W2 · relu(W1 · p + b1) + b2`, added to
//!    the cell only if it fires this step (a seeded hash below Fire Rate:
//!    stochastic, but the same for every run).
//! 3. **Live**: a cell is kept only if some cell in its 3×3 neighbourhood
//!    had alpha above the threshold before the step and one has after it;
//!    others are zeroed. The `erase` image zeroes cells too (regeneration).
//!
//! State is two storage buffers ping-ponged on the GPU (stateful, like
//! Reaction-Diffusion); the output shows channels 0-3 over a background.
//!
//! **Weights** come from a built-in rule or a JSON file in the project
//! (format below). The built-in rules are **hand-built, not trained**:
//! [`GrowRule`] writes, unit by unit, an MLP whose alpha channel grows from
//! any live cell and whose two hidden channels form a Turing
//! (activator-inhibitor) pattern inside the growing region, with colour
//! following them. No weights from elsewhere are used or shipped (rule 4).
//!
//! Pipelines are built on a worker thread and a weights file is read and
//! parsed on its own thread (rule 1); offline the cook waits for both.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{CookCtx, GpuOp};
use atd_gpu::fullscreen::FullscreenPass;
use serde_json::Value;

use crate::ops::img;
use crate::stateful::clear;
use crate::watch::{Watcher, watch};

/// Channels per cell, at least colour and alpha.
pub const MIN_CHANNELS: usize = 4;
/// Most channels per cell (the shader's per-cell arrays).
pub const MAX_CHANNELS: usize = 16;
/// Most hidden units.
pub const MAX_HIDDEN: usize = 128;
/// Most steps per frame.
pub const MAX_STEPS: u32 = 64;
/// Perception filters, in the order each channel's four values are stored.
pub const PERCEPTION: [&str; 4] = ["identity", "sobel_x", "sobel_y", "laplacian"];

pub const RULES: &[&str] = &["grow", "spots", "file"];
pub const SEED_MODES: &[&str] = &["centre", "scatter", "noise", "image"];
pub const EDGES: &[&str] = &["zero", "wrap"];
pub const DISPLAYS: &[&str] = &["rgba", "hidden"];

pub fn schema() -> OperatorSchema {
    let mut s = img(
        "nca",
        "Neural CA",
        "A neural cellular automaton: every cell perceives its neighbours through fixed Sobel and Laplacian filters and updates itself with a tiny neural network, at random but repeatable moments. The built-in rules (hand-built, not trained) grow a patterned organism from a seed and regrow what the `erase` image removes; a weights file in the project supplies any other network. `seed` can start it from an image. Reference: docs/nca.md.",
        &[("seed", false), ("erase", false)],
        vec![
            ParamSchema::menu("rule", "Rule", RULES, "grow").help(
                "`grow`: a labyrinth pattern growing from the seed; `spots`: spots instead; `file`: the network in Weights File.",
            ),
            ParamSchema::path("weights", "Weights File").help(
                "A Neural CA weights file (JSON, format in docs/nca.md), used when Rule is `file`; reloads when it changes.",
            ),
            ParamSchema::int("cell_size", "Cell Size", 2, Some(1), Some(16))
                .help("Output pixels per cell; larger is faster and coarser."),
            ParamSchema::int("steps", "Steps", 8, Some(1), Some(MAX_STEPS.into()))
                .help("Updates per frame (growth speed)."),
            ParamSchema::float("fire_rate", "Fire Rate", 0.5, Some(0.0), Some(1.0))
                .help("Chance that a cell updates in a step."),
            ParamSchema::float("threshold", "Alive Threshold", 0.1, Some(0.0), Some(1.0)).help(
                "A cell lives while a neighbour's alpha is above this; others are cleared.",
            ),
            ParamSchema::menu("seed_mode", "Seed", SEED_MODES, "centre").help(
                "How Reset starts: one centre cell, cells scattered at random (Density), every channel random, or the `seed` image (colour and alpha; hidden channels 1 where alive).",
            ),
            ParamSchema::float("density", "Density", 0.0005, Some(0.0), Some(1.0))
                .help("Chance that a cell is a seed, for the `scatter` seed."),
            ParamSchema::int("random_seed", "Random Seed", 1, Some(0), None)
                .help("Selects a different but repeatable sequence of updates and scattered seeds."),
            ParamSchema::menu("edges", "Edges", EDGES, "zero")
                .help("Outside the grid is dead (`zero`) or the opposite edge (`wrap`)."),
            ParamSchema::menu("display", "Display", DISPLAYS, "rgba").help(
                "`rgba`: the colour channels over Background; `hidden`: channels 4-6 as RGB (0.5 is zero).",
            ),
            ParamSchema::colour("background", "Background", [0.0, 0.0, 0.0, 1.0])
                .help("Shown where cells are transparent."),
            ParamSchema::toggle("smooth", "Smooth", false)
                .help("Interpolate between cells instead of showing square cells."),
            ParamSchema::pulse("reset", "Reset").help("Starts again from the seed."),
        ],
        // 8 steps of the built-in rule on a 640×360 grid (1280×720 at cell
        // size 2) take a fraction of this on a discrete GPU.
        2000,
    );
    s.time_dependent = true;
    s
}

// --------------------------------------------------------------- weights

/// The network of a Neural CA. With `p = 4 × channels` perception values
/// (per channel: identity, Sobel x, Sobel y, Laplacian):
/// `w1` is `hidden` rows of `p`, `b1` is `hidden`, `w2` is `channels` rows
/// of `hidden`, `b2` is `channels`.
#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    pub name: String,
    pub channels: usize,
    pub hidden: usize,
    pub w1: Vec<f32>,
    pub b1: Vec<f32>,
    pub w2: Vec<f32>,
    pub b2: Vec<f32>,
}

/// The file format's name, in its `format` field.
pub const FORMAT: &str = "atidarla-nca";

impl Weights {
    pub fn perception(&self) -> usize {
        4 * self.channels
    }

    /// Checks the sizes and values.
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_CHANNELS..=MAX_CHANNELS).contains(&self.channels) {
            return Err(format!(
                "channels is {}; it must be {MIN_CHANNELS} to {MAX_CHANNELS}",
                self.channels
            ));
        }
        if !(1..=MAX_HIDDEN).contains(&self.hidden) {
            return Err(format!(
                "hidden is {}; it must be 1 to {MAX_HIDDEN}",
                self.hidden
            ));
        }
        let (p, h, c) = (self.perception(), self.hidden, self.channels);
        for (name, v, want) in [
            ("w1", &self.w1, h * p),
            ("b1", &self.b1, h),
            ("w2", &self.w2, c * h),
            ("b2", &self.b2, c),
        ] {
            if v.len() != want {
                return Err(format!("{name} has {} numbers; it needs {want}", v.len()));
            }
            if v.iter().any(|x| !x.is_finite()) {
                return Err(format!("{name} holds a number that is not finite"));
            }
        }
        Ok(())
    }

    /// The GPU layout: `w1`, `b1`, `w2`, `b2` back to back, little-endian.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.w1
            .iter()
            .chain(&self.b1)
            .chain(&self.w2)
            .chain(&self.b2)
            .flat_map(|f| f.to_le_bytes())
            .collect()
    }

    /// Parses a weights file. `w1` and `w2` are arrays of rows (or flat).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("weights file: {e}"))?;
        let field = |k: &str| v.get(k).ok_or_else(|| format!("weights file has no `{k}`"));
        if field("format")?.as_str() != Some(FORMAT) {
            return Err(format!("weights file: `format` must be \"{FORMAT}\""));
        }
        if field("version")?.as_u64() != Some(1) {
            return Err("weights file: only version 1 is understood".into());
        }
        let perception: Vec<&str> = field("perception")?
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if perception != PERCEPTION {
            return Err(format!(
                "weights file: `perception` must be {PERCEPTION:?} (the only filters supported)"
            ));
        }
        let int = |k: &str| -> Result<usize, String> {
            field(k)?
                .as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| format!("weights file: `{k}` must be a whole number"))
        };
        let numbers = |k: &str| -> Result<Vec<f32>, String> {
            let mut out = Vec::new();
            let bad = || format!("weights file: `{k}` must be numbers (or rows of numbers)");
            for x in field(k)?.as_array().ok_or_else(bad)? {
                match x {
                    Value::Array(row) => {
                        for y in row {
                            out.push(y.as_f64().ok_or_else(bad)? as f32);
                        }
                    }
                    _ => out.push(x.as_f64().ok_or_else(bad)? as f32),
                }
            }
            Ok(out)
        };
        let w = Self {
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            channels: int("channels")?,
            hidden: int("hidden")?,
            w1: numbers("w1")?,
            b1: numbers("b1")?,
            w2: numbers("w2")?,
            b2: numbers("b2")?,
        };
        w.validate().map_err(|e| format!("weights file: {e}"))?;
        Ok(w)
    }

    /// The weights file text: canonical (fixed key order, one matrix row
    /// per line, shortest round-trip numbers), so it diffs well (rule 8).
    pub fn to_json(&self, licence: &str, source: &str) -> String {
        let num = |x: &f32| {
            let s = format!("{x}");
            if s.contains('.') || s.contains('e') {
                s
            } else {
                format!("{s}.0")
            }
        };
        let row = |r: &[f32]| r.iter().map(num).collect::<Vec<_>>().join(", ");
        let rows = |m: &[f32], width: usize| {
            m.chunks(width)
                .map(|r| format!("    [{}]", row(r)))
                .collect::<Vec<_>>()
                .join(",\n")
        };
        let q = |s: &str| serde_json::to_string(s).expect("strings serialise");
        format!(
            "{{\n  \"format\": {},\n  \"version\": 1,\n  \"name\": {},\n  \"licence\": {},\n  \"source\": {},\n  \"channels\": {},\n  \"hidden\": {},\n  \"perception\": [\"identity\", \"sobel_x\", \"sobel_y\", \"laplacian\"],\n  \"w1\": [\n{}\n  ],\n  \"b1\": [{}],\n  \"w2\": [\n{}\n  ],\n  \"b2\": [{}]\n}}\n",
            q(FORMAT),
            q(&self.name),
            q(licence),
            q(source),
            self.channels,
            self.hidden,
            rows(&self.w1, self.perception()),
            row(&self.b1),
            rows(&self.w2, self.hidden),
            row(&self.b2),
        )
    }
}

/// A hand-built growth rule (8 channels): alpha spreads from live cells
/// and saturates near 1; hidden channels `u` (4) and `v` (5) follow
/// piecewise-linear activator-inhibitor kinetics with diffusion (a Turing
/// pattern) and are kicked where the growth front passes; colour relaxes
/// towards `base·alpha + hi·relu(u) + lo·relu(−u)`.
#[derive(Debug, Clone, Copy)]
pub struct GrowRule {
    pub name: &'static str,
    /// Growth into neighbours (× relu of alpha's Laplacian).
    pub spread: f32,
    /// Growth of live cells (× relu(alpha − 0.1)).
    pub rise: f32,
    /// Pull back above 0.9 (× relu(alpha − 0.9)).
    pub cap: f32,
    /// Time step of the u-v kinetics.
    pub dt: f32,
    /// du = a·u + b·v + du·lap(u) + cu·alpha, dv = c·u + d·v + dv·lap(v) + cv·alpha.
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub du: f32,
    pub dv: f32,
    pub cu: f32,
    pub cv: f32,
    /// Pull back beyond ±1.
    pub sat: f32,
    /// Front kick: u += kick × relu(lap(alpha)).
    pub kick: f32,
    /// Colour relaxation rate.
    pub kc: f32,
    pub base: [f32; 3],
    pub hi: [f32; 3],
    pub lo: [f32; 3],
}

/// Built-in rule `grow`: a labyrinth.
pub const GROW: GrowRule = GrowRule {
    name: "grow",
    spread: 0.15,
    rise: 0.1,
    cap: 1.0,
    dt: 0.02,
    a: 1.0,
    b: -1.0,
    c: 3.0,
    d: -2.0,
    du: 2.0,
    dv: 40.0,
    cu: 0.0,
    cv: 0.0,
    sat: 0.5,
    kick: 2.0,
    kc: 0.2,
    base: [0.05, 0.1, 0.2],
    hi: [1.0, 0.6, 0.1],
    lo: [0.1, 0.5, 0.9],
};

/// Built-in rule `spots`: the inhibitor is fed, so the pattern breaks up.
pub const SPOTS: GrowRule = GrowRule {
    name: "spots",
    cv: 0.5,
    base: [0.02, 0.05, 0.04],
    hi: [0.95, 0.3, 0.55],
    lo: [0.05, 0.3, 0.25],
    ..GROW
};

/// The rule of `examples/neural-growth.atidarla/nca/coral.json`.
pub const CORAL: GrowRule = GrowRule {
    name: "coral",
    cu: -0.15,
    base: [0.1, 0.04, 0.04],
    hi: [1.0, 0.45, 0.35],
    lo: [0.95, 0.9, 0.8],
    ..GROW
};

impl GrowRule {
    pub fn weights(&self) -> Weights {
        const C: usize = 8;
        const P: usize = 4 * C;
        let (r, g, b, a, u, v) = (0, 1, 2, 3, 4, 5);
        let (id, lap) = (0, 3);
        let at = |c: usize, k: usize| c * 4 + k;
        let mut units: Vec<(Vec<(usize, f32)>, f32)> = Vec::new();
        let mut outs: Vec<(usize, usize, f32)> = Vec::new();
        let mut unit = |ws: &[(usize, f32)], bias: f32| {
            units.push((ws.to_vec(), bias));
            units.len() - 1
        };
        // Alpha.
        let j = unit(&[(at(a, lap), 1.0)], 0.0);
        outs.push((a, j, self.spread));
        let j = unit(&[(at(a, id), 1.0)], -0.1);
        outs.push((a, j, self.rise));
        let j = unit(&[(at(a, id), 1.0)], -0.9);
        outs.push((a, j, -self.cap));
        let alive = unit(&[(at(a, id), 1.0)], 0.0);
        // Linear terms as relu pairs: x = relu(x) − relu(−x).
        let mut pair = |i: usize| (unit(&[(i, 1.0)], 0.0), unit(&[(i, -1.0)], 0.0));
        let (up, un) = pair(at(u, id));
        let (vp, vn) = pair(at(v, id));
        let (lup, lun) = pair(at(u, lap));
        let (lvp, lvn) = pair(at(v, lap));
        let dt = self.dt;
        for (c, (jp, jn), w) in [
            (u, (up, un), self.a),
            (u, (vp, vn), self.b),
            (u, (lup, lun), self.du),
            (v, (up, un), self.c),
            (v, (vp, vn), self.d),
            (v, (lvp, lvn), self.dv),
        ] {
            outs.push((c, jp, dt * w));
            outs.push((c, jn, -dt * w));
        }
        outs.push((u, alive, dt * self.cu));
        outs.push((v, alive, dt * self.cv));
        let j = unit(&[(at(a, lap), 1.0)], 0.0);
        outs.push((u, j, self.kick));
        // Saturation beyond ±1.
        for ch in [u, v] {
            let j = unit(&[(at(ch, id), 1.0)], -1.0);
            outs.push((ch, j, -self.sat));
            let j = unit(&[(at(ch, id), -1.0)], -1.0);
            outs.push((ch, j, self.sat));
        }
        // Colour relaxes towards its target.
        for (i, ch) in [r, g, b].into_iter().enumerate() {
            let j = unit(&[(at(ch, id), 1.0)], 0.0);
            outs.push((ch, j, -self.kc));
            outs.push((ch, alive, self.kc * self.base[i]));
            outs.push((ch, up, self.kc * self.hi[i]));
            outs.push((ch, un, self.kc * self.lo[i]));
        }
        let h = units.len();
        let mut w1 = vec![0.0; h * P];
        let mut b1 = vec![0.0; h];
        for (j, (ws, bias)) in units.iter().enumerate() {
            for &(i, w) in ws {
                w1[j * P + i] = w;
            }
            b1[j] = *bias;
        }
        let mut w2 = vec![0.0; C * h];
        for (c, j, w) in outs {
            w2[c * h + j] += w;
        }
        Weights {
            name: self.name.into(),
            channels: C,
            hidden: h,
            w1,
            b1,
            w2,
            b2: vec![0.0; C],
        }
    }
}

/// Licence and provenance written into generated weights files.
pub const GENERATED_LICENCE: &str = "MIT OR Apache-2.0";
pub const GENERATED_SOURCE: &str = "Hand-built (not trained) by atd_ops_img::nca::GrowRule; regenerate with `cargo test -p atd-ops-img --test nca -- --ignored write_example_weights`";

// ---------------------------------------------------------------- shader

/// The shader for one network shape; `C`, `H` and `P` (and the weight
/// offsets) are constants, and the update's perception and per-unit sums
/// are written out, so they live in registers.
const SHADER: &str = r#"
struct U {
    grid: vec2u,
    step: u32,
    seed: u32,
    fire_rate: f32,
    threshold: f32,
    wrap: u32,
    seed_mode: u32,
    density: f32,
    display: u32,
    pad: vec2u,
    background: vec4f,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var samp: sampler;
@group(0) @binding(5) var seed_image: texture_2d<f32>;
@group(0) @binding(6) var erase_image: texture_2d<f32>;
@group(0) @binding(7) var display: texture_storage_2d<rgba16float, write>;

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// Uniform in [0, 1): the same for every run and every GPU.
fn rand(x: u32, y: u32, z: u32) -> f32 {
    return f32(pcg(x ^ pcg(y ^ pcg(z ^ pcg(u.seed)))) >> 8u) / 16777216.0;
}

// Index of cell (x, y), or -1 outside the grid with zero edges.
fn cell(x: i32, y: i32) -> i32 {
    let g = vec2i(u.grid);
    if (u.wrap != 0u) {
        return ((y % g.y + g.y) % g.y) * g.x + ((x % g.x + g.x) % g.x);
    }
    if (x < 0 || y < 0 || x >= g.x || y >= g.y) {
        return -1;
    }
    return y * g.x + x;
}

fn src_at(i: i32, c: u32) -> f32 {
    if (i < 0) {
        return 0.0;
    }
    return src[u32(i) * C + c];
}

// Some cell of the 3x3 neighbourhood has alpha above the threshold.
fn alive(x: i32, y: i32) -> bool {
    var m = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            m = max(m, src_at(cell(x + dx, y + dy), 3u));
        }
    }
    return m > u.threshold;
}

@compute @workgroup_size(8, 8, 1)
fn update(@builtin(global_invocation_id) id: vec3u) {
    if (id.x >= u.grid.x || id.y >= u.grid.y) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let base = u32(cell(x, y)) * C;
    if (!alive(x, y)) {
        for (var c = 0u; c < C; c++) {
            dst[base + c] = 0.0;
        }
        return;
    }
    let n0 = cell(x - 1, y - 1);
    let n1 = cell(x, y - 1);
    let n2 = cell(x + 1, y - 1);
    let n3 = cell(x - 1, y);
    let n4 = cell(x, y);
    let n5 = cell(x + 1, y);
    let n6 = cell(x - 1, y + 1);
    let n7 = cell(x, y + 1);
    let n8 = cell(x + 1, y + 1);
    // Perception, per channel: the value, Sobel x, Sobel y and Laplacian.
//@PERCEIVE
    // ds = W2 relu(W1 p + b1) + b2, added if the cell fires.
//@UPDATE
}

@compute @workgroup_size(8, 8, 1)
fn mask(@builtin(global_invocation_id) id: vec3u) {
    if (id.x >= u.grid.x || id.y >= u.grid.y) {
        return;
    }
    let live = alive(i32(id.x), i32(id.y));
    let uv = (vec2f(id.xy) + 0.5) / vec2f(u.grid);
    let e = textureSampleLevel(erase_image, samp, uv, 0.0);
    let erased = dot(e.rgb, vec3f(0.2126, 0.7152, 0.0722)) * e.a > 0.5;
    let i = (id.y * u.grid.x + id.x) * C;
    for (var c = 0u; c < C; c++) {
        dst[i + c] = select(0.0, src[i + c], live && !erased);
    }
}

@compute @workgroup_size(8, 8, 1)
fn seed(@builtin(global_invocation_id) id: vec3u) {
    if (id.x >= u.grid.x || id.y >= u.grid.y) {
        return;
    }
    let i = (id.y * u.grid.x + id.x) * C;
    let uv = (vec2f(id.xy) + 0.5) / vec2f(u.grid);
    let s = textureSampleLevel(seed_image, samp, uv, 0.0);
    let centre = all(id.xy == u.grid / 2u);
    let scattered = rand(id.x, id.y, 0xfffffff0u) < u.density;
    for (var c = 0u; c < C; c++) {
        var v = 0.0;
        switch u.seed_mode {
            case 0u: {
                v = select(0.0, 1.0, centre && c >= 3u);
            }
            case 1u: {
                v = select(0.0, 1.0, scattered && c >= 3u);
            }
            case 2u: {
                v = rand(id.x, id.y, 0xfffffff1u + c);
            }
            default: {
                if (c < 3u) {
                    v = s[c] * s.a;
                } else if (c == 3u) {
                    v = s.a;
                } else {
                    v = select(0.0, 1.0, s.a > u.threshold);
                }
            }
        }
        dst[i + c] = v;
    }
}

@compute @workgroup_size(8, 8, 1)
fn show(@builtin(global_invocation_id) id: vec3u) {
    if (id.x >= u.grid.x || id.y >= u.grid.y) {
        return;
    }
    let i = (id.y * u.grid.x + id.x) * C;
    var o: vec4f;
    if (u.display == 0u) {
        // Premultiplied colour over the background, shown straight.
        let a = clamp(src[i + 3u], 0.0, 1.0);
        let rgb = clamp(vec3f(src[i], src[i + 1u], src[i + 2u]), vec3f(0.0), vec3f(1.0));
        let bg = u.background;
        let out_a = a + bg.a * (1.0 - a);
        let premul = rgb + bg.rgb * bg.a * (1.0 - a);
        o = vec4f(select(vec3f(0.0), premul / out_a, out_a > 0.0), out_a);
    } else {
        var h = vec3f(0.0);
        for (var k = 0u; k < 3u; k++) {
            if (4u + k < C) {
                h[k] = src[i + 4u + k];
            }
        }
        o = vec4f(clamp(0.5 + 0.5 * h, vec3f(0.0), vec3f(1.0)), 1.0);
    }
    textureStore(display, id.xy, o);
}
"#;

/// Scales the cell image to the output: square cells, or interpolated.
const BLIT: &str = r#"
    if (params.v[0].x > 0.5) {
        return textureSampleLevel(in0, samp, uv, 0.0);
    }
    let size = vec2f(textureDimensions(in0));
    return textureLoad(in0, vec2i(min(uv * size, size - 1.0)), 0);"#;

/// [`SHADER`] for networks of `c` channels and `h` hidden units.
fn shader(c: usize, h: usize) -> String {
    use std::fmt::Write as _;
    let p = 4 * c;
    let mut perceive = String::new();
    for ch in 0..c {
        for k in 0..9 {
            let _ = writeln!(perceive, "    let a{ch}_{k} = src_at(n{k}, {ch}u);");
        }
        let a = |k: usize| format!("a{ch}_{k}");
        let _ = writeln!(perceive, "    let p{} = {};", 4 * ch, a(4));
        let _ = writeln!(
            perceive,
            "    let p{} = (({} + 2.0 * {} + {}) - ({} + 2.0 * {} + {})) / 8.0;",
            4 * ch + 1,
            a(2),
            a(5),
            a(8),
            a(0),
            a(3),
            a(6)
        );
        let _ = writeln!(
            perceive,
            "    let p{} = (({} + 2.0 * {} + {}) - ({} + 2.0 * {} + {})) / 8.0;",
            4 * ch + 2,
            a(6),
            a(7),
            a(8),
            a(0),
            a(1),
            a(2)
        );
        let _ = writeln!(
            perceive,
            "    let p{} = (({} + {} + {} + {}) + 2.0 * ({} + {} + {} + {}) - 12.0 * {}) / 16.0;",
            4 * ch + 3,
            a(0),
            a(2),
            a(6),
            a(8),
            a(1),
            a(3),
            a(5),
            a(7),
            a(4)
        );
    }
    let mut update = String::new();
    for ch in 0..c {
        let _ = writeln!(update, "    var d{ch} = weights[OB2 + {ch}u];");
    }
    let _ = writeln!(update, "    for (var j = 0u; j < H; j++) {{");
    let _ = writeln!(update, "        let row = j * P;");
    let _ = writeln!(update, "        var a = weights[OB1 + j];");
    for i in 0..p {
        let _ = writeln!(update, "        a += weights[row + {i}u] * p{i};");
    }
    let _ = writeln!(update, "        let h = max(a, 0.0);");
    let _ = writeln!(update, "        if (h > 0.0) {{");
    for ch in 0..c {
        let _ = writeln!(
            update,
            "            d{ch} += weights[OW2 + {ch}u * H + j] * h;"
        );
    }
    let _ = writeln!(update, "        }}\n    }}");
    let _ = writeln!(
        update,
        "    let fire = rand(id.x, id.y, u.step) < u.fire_rate;"
    );
    for ch in 0..c {
        let _ = writeln!(
            update,
            "    dst[base + {ch}u] = select(p{0}, p{0} + d{ch}, fire);",
            4 * ch
        );
    }
    format!(
        "const C: u32 = {c}u;\nconst H: u32 = {h}u;\nconst P: u32 = {p}u;\nconst OB1: u32 = H * P;\nconst OW2: u32 = OB1 + H;\nconst OB2: u32 = OW2 + C * H;\n{}",
        SHADER
            .replace("//@PERCEIVE", perceive.trim_end())
            .replace("//@UPDATE", update.trim_end())
    )
}

/// Bytes of the uniform block, and the stride between blocks (the
/// guaranteed `min_uniform_buffer_offset_alignment`).
const UNIFORM_BYTES: u64 = 64;
const STRIDE: u64 = 256;
/// Blocks: one for seeding and showing, one per step.
const SLOTS: u64 = MAX_STEPS as u64 + 1;

/// A network shape: (channels, hidden units).
type Shape = (usize, usize);

struct Pipelines {
    shape: Shape,
    layout: wgpu::BindGroupLayout,
    update: wgpu::ComputePipeline,
    mask: wgpu::ComputePipeline,
    seed: wgpu::ComputePipeline,
    show: wgpu::ComputePipeline,
    blit: FullscreenPass,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

/// Builds the pipelines for one network shape. Blocks while the driver
/// compiles: call it off the render thread.
fn build(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    shape: Shape,
) -> Result<Pipelines, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img.nca"),
            source: wgpu::ShaderSource::Wgsl(shader(shape.0, shape.1).into()),
        });
        let compute = wgpu::ShaderStages::COMPUTE;
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty,
            count: None,
        };
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let texture = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.nca"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
                    },
                ),
                entry(1, storage(true)),
                entry(2, storage(false)),
                entry(3, storage(true)),
                entry(
                    4,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                entry(5, texture),
                entry(6, texture),
                entry(
                    7,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img.nca"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("img.nca"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipes = Pipelines {
            shape,
            update: pipeline("update"),
            mask: pipeline("mask"),
            seed: pipeline("seed"),
            show: pipeline("show"),
            layout,
            blit: FullscreenPass::new(device, "img.nca.blit", format, 1, "", BLIT),
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img.nca"),
                size: SLOTS * STRIDE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("img.nca"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        };
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(format!("Neural CA pipelines: {e}")),
            None => Ok(pipes),
        }
    }))
    .unwrap_or_else(|_| Err("Neural CA pipeline build panicked".into()))
}

// ----------------------------------------------------------------- state

/// Bind groups kept per pair of input textures (pooled inputs cycle among
/// a few), so steady-state frames create none (rule 2).
const CACHE: usize = 8;

struct State {
    grid: (u32, u32),
    channels: usize,
    /// The state; `b` holds each step's unmasked update.
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    display: wgpu::Texture,
    display_view: wgpu::TextureView,
    /// (seed, erase, [reading a, reading b], last use).
    binds: Vec<(wgpu::Texture, wgpu::Texture, [wgpu::BindGroup; 2], u64)>,
    uses: u64,
}

impl State {
    fn new(device: &wgpu::Device, grid: (u32, u32), channels: usize) -> Result<Self, String> {
        let bytes = u64::from(grid.0) * u64::from(grid.1) * channels as u64 * 4;
        let limit = device.limits().max_storage_buffer_binding_size;
        if bytes > limit {
            return Err(format!(
                "a {}×{} grid of {channels} channels needs {} MB, over this GPU's {} MB buffer limit; raise Cell Size",
                grid.0,
                grid.1,
                bytes >> 20,
                limit >> 20
            ));
        }
        let buffer = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img.nca.state"),
                size: bytes.max(4),
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        let display = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("img.nca.display"),
            size: wgpu::Extent3d {
                width: grid.0,
                height: grid.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let display_view = display.create_view(&Default::default());
        Ok(Self {
            grid,
            channels,
            a: buffer(),
            b: buffer(),
            display,
            display_view,
            binds: Vec::new(),
            uses: 0,
        })
    }

    fn binds(
        &mut self,
        device: &wgpu::Device,
        pipes: &Pipelines,
        weights: &wgpu::Buffer,
        seed: &wgpu::Texture,
        erase: &wgpu::Texture,
    ) -> &[wgpu::BindGroup; 2] {
        self.uses += 1;
        let now = self.uses;
        self.binds.retain(|e| now - e.3 <= 2 * CACHE as u64);
        if let Some(i) = self
            .binds
            .iter()
            .position(|(s, e, _, _)| s == seed && e == erase)
        {
            self.binds[i].3 = now;
            return &self.binds[i].2;
        }
        let (seed_view, erase_view) = (
            seed.create_view(&Default::default()),
            erase.create_view(&Default::default()),
        );
        let group = |src: &wgpu::Buffer, dst: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.nca"),
                layout: &pipes.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &pipes.uniforms,
                            offset: 0,
                            size: wgpu::BufferSize::new(UNIFORM_BYTES),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: src.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: dst.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: weights.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&pipes.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&seed_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&erase_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&self.display_view),
                    },
                ],
            })
        };
        let groups = [group(&self.a, &self.b), group(&self.b, &self.a)];
        if self.binds.len() == CACHE {
            self.binds.remove(0);
        }
        self.binds.push((seed.clone(), erase.clone(), groups, now));
        &self.binds.last().expect("pushed").2
    }
}

// -------------------------------------------------------------- operator

/// Where the weights come from.
enum Source {
    Builtin(&'static str),
    File(Watcher<Weights>),
}

pub struct Nca {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    /// Pipelines being built, for this shape.
    building: Option<(Shape, Receiver<Result<Pipelines, String>>)>,
    pipes: Option<Pipelines>,
    /// A shape whose pipelines failed to build, and why.
    failed: Option<(Shape, String)>,
    source: Option<Source>,
    /// The weights in use and their GPU copy.
    weights: Option<(Arc<Weights>, wgpu::Buffer)>,
    /// A weights file that failed to load or parse.
    weights_error: Option<String>,
    state: Option<State>,
    /// Steps since the last seed (the update hash's third input).
    step: u32,
    last_reset: Option<bool>,
    /// Uniform blocks for one cook, allocated once.
    bytes: Vec<u8>,
}

impl std::fmt::Debug for Nca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nca")
            .field("ready", &self.pipes.is_some())
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Nca {
        device: device.clone(),
        format,
        building: None,
        pipes: None,
        failed: None,
        source: None,
        weights: None,
        weights_error: None,
        state: None,
        step: 0,
        last_reset: None,
        bytes: vec![0; (SLOTS * STRIDE) as usize],
    })
}

fn menu_index(value: &str, options: &[&str]) -> u32 {
    options.iter().position(|o| *o == value).unwrap_or(0) as u32
}

impl Nca {
    /// Uses `w` from now on; a different channel count restarts the state.
    fn use_weights(&mut self, w: Weights) {
        // Never empty: b2 has at least MIN_CHANNELS numbers.
        let bytes = w.to_bytes();
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.nca.weights"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        if let Ok(mut view) = buffer.slice(..).get_mapped_range_mut() {
            view.copy_from_slice(&bytes);
        }
        buffer.unmap();
        if let Some(st) = &mut self.state {
            // The bind groups hold the old weights.
            st.binds.clear();
            if st.channels != w.channels {
                self.state = None;
            }
        }
        self.weights = Some((Arc::new(w), buffer));
        self.weights_error = None;
    }

    /// Picks up the rule's weights; `Ok(false)` while a file is loading.
    fn weights_for(&mut self, ctx: &mut CookCtx<'_>) -> Result<bool, String> {
        let p = ctx.params;
        let rule = p.str("rule");
        if rule == "file" {
            let file = p.str("weights");
            if file.is_empty() {
                return Err("Rule is `file` but Weights File is empty".into());
            }
            if !matches!(&self.source, Some(Source::File(w)) if w.path == file) {
                self.source = Some(Source::File(watch(
                    file.to_owned(),
                    p.resolve(file),
                    "atd-nca-weights",
                    Weights::from_json,
                )));
                self.weights = None;
                self.weights_error = None;
            }
            let mode = p.mode();
            let Some(Source::File(w)) = &self.source else {
                unreachable!("set above")
            };
            let mut latest = None;
            if mode.is_offline() {
                // Offline the file is read once, as it was when the render
                // started.
                if self.weights.is_none() && self.weights_error.is_none() {
                    latest = Some(
                        mode.receive(&w.rx)
                            .unwrap_or_else(|_| Err("weights file watcher stopped".into())),
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
                Some(Ok(w)) => self.use_weights(w),
                Some(Err(e)) => self.weights_error = Some(e),
                None => {}
            }
        } else {
            let name = if rule == "spots" { "spots" } else { "grow" };
            if !matches!(self.source, Some(Source::Builtin(n)) if n == name) {
                self.source = Some(Source::Builtin(name));
                let w = if name == "spots" { SPOTS } else { GROW }.weights();
                self.use_weights(w);
            }
        }
        if let Some(e) = &self.weights_error {
            // A bad file: keep the last good weights running, and say so.
            if self.weights.is_none() {
                return Err(e.clone());
            }
        }
        Ok(self.weights.is_some())
    }

    /// Whether pipelines for `shape` are ready; starts building them on a
    /// worker thread when they are not (the driver compile can take a
    /// while, rule 1). Offline it waits for them (FR-H1).
    fn pipelines_for(
        &mut self,
        shape: Shape,
        mode: atd_gpu::engine::CookMode,
    ) -> Result<bool, String> {
        if self.pipes.as_ref().is_some_and(|p| p.shape == shape) {
            return Ok(true);
        }
        if let Some((s, e)) = &self.failed
            && *s == shape
        {
            return Err(e.clone());
        }
        if self.building.as_ref().is_none_or(|(s, _)| *s != shape) {
            let (tx, rx) = channel();
            let (device, format) = (self.device.clone(), self.format);
            std::thread::Builder::new()
                .name("atd-nca-build".into())
                .spawn(move || {
                    let _ = tx.send(build(&device, format, shape));
                })
                .map_err(|e| format!("cannot start build thread: {e}"))?;
            self.building = Some((shape, rx));
        }
        let (_, rx) = self.building.as_ref().expect("set above");
        match mode.receive(rx) {
            Ok(Ok(pipes)) => {
                self.building = None;
                // New pipelines have a new layout: rebuild the bind groups.
                if let Some(st) = &mut self.state {
                    st.binds.clear();
                }
                self.pipes = Some(pipes);
                Ok(true)
            }
            Ok(Err(e)) => {
                self.building = None;
                self.failed = Some((shape, e.clone()));
                Err(e)
            }
            Err(TryRecvError::Empty) => Ok(false),
            Err(TryRecvError::Disconnected) => {
                self.building = None;
                Err("Neural CA build thread stopped".into())
            }
        }
    }
}

impl GpuOp for Nca {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let ready = self.weights_for(ctx).and_then(|ready| {
            let Some((w, _)) = &self.weights else {
                return Ok(false);
            };
            let shape = (w.channels, w.hidden);
            Ok(ready && self.pipelines_for(shape, p.mode())?)
        });
        match ready {
            Ok(true) => {}
            Ok(false) => {
                ctx.cook_again = true;
                clear(ctx.encoder, ctx.output);
                return Ok(());
            }
            Err(e) => {
                clear(ctx.encoder, ctx.output);
                return Err(e);
            }
        }
        let pipes = self.pipes.as_ref().expect("ready");
        let (weights, weights_buffer) = self.weights.as_ref().expect("ready");
        let out = ctx.output;
        let cell = p.i64("cell_size").clamp(1, 16) as u32;
        let grid = (out.width().div_ceil(cell), out.height().div_ceil(cell));
        let reset = p.bool("reset");
        let mut seed = self.last_reset.is_some_and(|r| r != reset);
        self.last_reset = Some(reset);
        if !self
            .state
            .as_ref()
            .is_some_and(|s| s.grid == grid && s.channels == weights.channels)
        {
            match State::new(ctx.device, grid, weights.channels) {
                Ok(s) => self.state = Some(s),
                Err(e) => {
                    self.state = None;
                    clear(ctx.encoder, out);
                    return Err(e);
                }
            }
            seed = true;
        }
        if seed {
            self.step = 0;
        }
        let steps = p.i64("steps").clamp(1, MAX_STEPS.into()) as u32;

        // Uniform blocks: slot 0 seeds and shows, slot k + 1 is step k.
        let bg = p.vec4("background");
        let mut block = [0u32; (UNIFORM_BYTES / 4) as usize];
        block[0] = grid.0;
        block[1] = grid.1;
        block[3] = p.i64("random_seed").clamp(0, i64::from(u32::MAX)) as u32;
        block[4] = p.f32("fire_rate").clamp(0.0, 1.0).to_bits();
        block[5] = p.f32("threshold").to_bits();
        block[6] = menu_index(p.str("edges"), EDGES);
        block[7] = menu_index(p.str("seed_mode"), SEED_MODES);
        block[8] = p.f32("density").clamp(0.0, 1.0).to_bits();
        block[9] = menu_index(p.str("display"), DISPLAYS);
        for (k, v) in bg.iter().enumerate() {
            block[12 + k] = v.to_bits();
        }
        for slot in 0..=steps {
            block[2] = self.step.wrapping_add(slot.saturating_sub(1));
            let at = (u64::from(slot) * STRIDE) as usize;
            for (k, v) in block.iter().enumerate() {
                self.bytes[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        ctx.queue.write_buffer(
            &pipes.uniforms,
            0,
            &self.bytes[..((u64::from(steps) + 1) * STRIDE) as usize],
        );

        let (seed_img, erase_img) = (ctx.input_or_black(0), ctx.input_or_black(1));
        let st = self.state.as_mut().expect("created above");
        let groups = (grid.0.div_ceil(8), grid.1.div_ceil(8));
        {
            let binds = st.binds(ctx.device, pipes, weights_buffer, seed_img, erase_img);
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("img.nca"),
                    timestamp_writes: None,
                });
            if seed {
                // binds[1] writes buffer a.
                pass.set_pipeline(&pipes.seed);
                pass.set_bind_group(0, &binds[1], &[0]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            for k in 1..=steps {
                let offset = k * STRIDE as u32;
                pass.set_pipeline(&pipes.update);
                pass.set_bind_group(0, &binds[0], &[offset]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
                pass.set_pipeline(&pipes.mask);
                pass.set_bind_group(0, &binds[1], &[offset]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            pass.set_pipeline(&pipes.show);
            pass.set_bind_group(0, &binds[0], &[0]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        self.step = self.step.wrapping_add(steps);
        pipes.blit.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[&st.display],
            &[[f32::from(u8::from(p.bool("smooth"))), 0.0, 0.0, 0.0]],
            out,
        );
        match &self.weights_error {
            Some(e) => Err(format!("{e} (still running the last good weights)")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_rules_are_valid_and_files_round_trip() {
        for rule in [GROW, SPOTS, CORAL] {
            let w = rule.weights();
            w.validate().unwrap();
            assert_eq!((w.channels, w.hidden), (8, 20));
            let text = w.to_json(GENERATED_LICENCE, GENERATED_SOURCE);
            let back = Weights::from_json(&text).unwrap();
            assert_eq!(back, w, "{}", rule.name);
            // Canonical: writing what was read gives the same text.
            assert_eq!(back.to_json(GENERATED_LICENCE, GENERATED_SOURCE), text);
        }
        assert_ne!(GROW.weights(), SPOTS.weights());
    }

    #[test]
    fn bad_files_say_what_is_wrong() {
        let good = GROW.weights().to_json("x", "y");
        let err = |text: &str| Weights::from_json(text).unwrap_err();
        assert!(err("{").starts_with("weights file: "));
        assert_eq!(
            err(&good.replace("\"atidarla-nca\"", "\"other\"")),
            "weights file: `format` must be \"atidarla-nca\""
        );
        assert_eq!(
            err(&good.replace("\"channels\": 8", "\"channels\": 9")),
            "weights file: w1 has 640 numbers; it needs 720"
        );
        assert_eq!(
            err(&good.replace("\"channels\": 8", "\"channels\": 2")),
            "weights file: channels is 2; it must be 4 to 16"
        );
        assert!(err(&good.replace("\"laplacian\"", "\"gauss\"")).contains("`perception`"));
        // A flat w1 is accepted too.
        let w = GROW.weights();
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["w1"] = serde_json::json!(w.w1);
        assert_eq!(Weights::from_json(&v.to_string()).unwrap(), w);
    }
}
