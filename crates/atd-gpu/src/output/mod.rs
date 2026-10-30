//! Output mapping (FR-S6, M3): how a composition reaches a projector or a
//! screen. An [`OutputMap`] is a list of [`Slice`]s; each takes a rectangle
//! of the source image and places it in the output through a warp (a corner
//! pin, a bilinear or Catmull-Rom control grid, or a Bezier patch), with an
//! optional polygon mask and soft edges for blending overlapping
//! projectors. [`OutputMapper`] renders it on the GPU; test patterns replace
//! the source to line projectors up.
//!
//! The description is text (the `slices` parameter of `cmp.window`, rule 8):
//! one `[slice name]` block per slice, one `key = values` line per setting,
//! coordinates as fractions of the image (x right, y down). [`OutputMap::parse`]
//! reads it and [`OutputMap::to_text`] writes the canonical form (defaults
//! left out, one control-point row per line, four decimals at most), so a
//! dragged control point changes one line of a diff. Format reference:
//! `docs/outputs.md`.
//!
//! Tessellation runs on the CPU when the description changes (an edit),
//! never per frame: the mesh is uploaded once and the frame draws it.

mod pass;

use std::fmt::Write as _;

pub use pass::OutputMapper;

/// Most slices per output.
pub const MAX_SLICES: usize = 16;
/// Most mask polygon points per slice.
pub const MAX_MASK_POINTS: usize = 32;
/// Most control points per axis of a warp grid.
pub const MAX_GRID: u32 = 16;
/// Most tessellation segments per axis of a slice.
pub const MAX_SUBDIV: u32 = 64;
/// Default tessellation segments per axis.
pub const DEFAULT_SUBDIV: u32 = 32;

/// How a slice's control points shape it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Warp {
    /// Piecewise bilinear between control points.
    #[default]
    Bilinear,
    /// A projective corner pin (2×2 points only): straight lines stay
    /// straight, as a keystoned projector needs.
    Perspective,
    /// A smooth Catmull-Rom surface through every control point.
    CatmullRom,
    /// Bicubic Bezier patches: (3n+1)×(3m+1) points, the corners of each
    /// patch on the surface and the others pulling it.
    Bezier,
}

impl Warp {
    pub const NAMES: [&'static str; 4] = ["bilinear", "perspective", "catmull_rom", "bezier"];

    pub fn name(self) -> &'static str {
        match self {
            Warp::Bilinear => "bilinear",
            Warp::Perspective => "perspective",
            Warp::CatmullRom => "catmull_rom",
            Warp::Bezier => "bezier",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "bilinear" => Warp::Bilinear,
            "perspective" => Warp::Perspective,
            "catmull_rom" => Warp::CatmullRom,
            "bezier" => Warp::Bezier,
            _ => return None,
        })
    }
}

/// A test pattern drawn in every slice instead of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pattern {
    #[default]
    Off,
    /// An 8×8 grid with a centre cross and a border: shows the warp.
    Grid,
    /// Eight 75% colour bars.
    Bars,
    /// A horizontal black-to-white ramp: shows edge blends.
    Gradient,
    /// Concentric circles on a colour per slice, with the slice number.
    Circles,
}

impl Pattern {
    pub const NAMES: [&'static str; 5] = ["off", "grid", "bars", "gradient", "circles"];

    pub fn from_name(s: &str) -> Self {
        match s {
            "grid" => Pattern::Grid,
            "bars" => Pattern::Bars,
            "gradient" => Pattern::Gradient,
            "circles" => Pattern::Circles,
            _ => Pattern::Off,
        }
    }

    fn index(self) -> f32 {
        match self {
            Pattern::Off => 0.0,
            Pattern::Grid => 1.0,
            Pattern::Bars => 2.0,
            Pattern::Gradient => 3.0,
            Pattern::Circles => 4.0,
        }
    }
}

/// One slice: a source rectangle placed in the output.
#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub name: String,
    /// Source rectangle `x y w h` (fractions of the input).
    pub source: [f32; 4],
    /// Destination rectangle `x y w h` in the output, used while `points`
    /// is `None` (control points spread evenly over it).
    pub dst: [f32; 4],
    pub warp: Warp,
    /// Control points per axis: columns, rows.
    pub grid: [u32; 2],
    /// Control points in the output, row-major from the top left.
    pub points: Option<Vec<[f32; 2]>>,
    /// Soft-edge widths `left right top bottom`, as fractions of the slice.
    pub blend: [f32; 4],
    /// Steepness of the blend curve (1 linear, 2 the usual S-curve).
    pub blend_power: f32,
    /// Display gamma the blend compensates (2.2 for most projectors; 1 for
    /// a linear ramp).
    pub blend_gamma: f32,
    /// A polygon in output coordinates; outside it (inside with
    /// `mask_invert`) the slice is black. Empty: no mask.
    pub mask: Vec<[f32; 2]>,
    pub mask_invert: bool,
    /// Tessellation segments per axis.
    pub subdiv: u32,
}

impl Slice {
    /// A slice showing the whole source over the whole output.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            source: [0.0, 0.0, 1.0, 1.0],
            dst: [0.0, 0.0, 1.0, 1.0],
            warp: Warp::Bilinear,
            grid: [2, 2],
            points: None,
            blend: [0.0; 4],
            blend_power: 2.0,
            blend_gamma: 2.2,
            mask: Vec::new(),
            mask_invert: false,
            subdiv: DEFAULT_SUBDIV,
        }
    }

    /// The control points: `points`, or the grid spread over `dst`.
    pub fn control_points(&self) -> Vec<[f32; 2]> {
        if let Some(p) = &self.points {
            return p.clone();
        }
        let [cols, rows] = self.grid;
        let [x, y, w, h] = self.dst;
        let mut out = Vec::with_capacity((cols * rows) as usize);
        for j in 0..rows {
            for i in 0..cols {
                out.push([
                    x + w * i as f32 / (cols - 1) as f32,
                    y + h * j as f32 / (rows - 1) as f32,
                ]);
            }
        }
        out
    }

    /// Where the slice's position `(u, v)` (0..1 over the slice) lands in
    /// the output.
    pub fn eval(&self, points: &[[f32; 2]], u: f32, v: f32) -> [f32; 2] {
        let [cols, rows] = [self.grid[0] as usize, self.grid[1] as usize];
        let at = |i: usize, j: usize| points[j * cols + i];
        match self.warp {
            Warp::Perspective => homography(points)(u, v),
            Warp::Bilinear => {
                let (i, s) = cell(u, cols - 1);
                let (j, t) = cell(v, rows - 1);
                let top = lerp2(at(i, j), at(i + 1, j), s);
                let bottom = lerp2(at(i, j + 1), at(i + 1, j + 1), s);
                lerp2(top, bottom, t)
            }
            Warp::CatmullRom => {
                let (i, s) = cell(u, cols - 1);
                let (j, t) = cell(v, rows - 1);
                // A point outside the grid is extrapolated linearly.
                let get = |i: isize, j: isize| -> [f32; 2] {
                    let ci = i.clamp(0, cols as isize - 1);
                    let cj = j.clamp(0, rows as isize - 1);
                    let p = at(ci as usize, cj as usize);
                    let (di, dj) = (i - ci, j - cj);
                    let mut out = p;
                    if di != 0 {
                        let q = at((ci - di) as usize, cj as usize);
                        out = [out[0] + (p[0] - q[0]), out[1] + (p[1] - q[1])];
                    }
                    if dj != 0 {
                        let q = at(ci as usize, (cj - dj) as usize);
                        out = [out[0] + (p[0] - q[0]), out[1] + (p[1] - q[1])];
                    }
                    out
                };
                let (i, j) = (i as isize, j as isize);
                let row = |jj: isize| {
                    catmull_rom(
                        get(i - 1, jj),
                        get(i, jj),
                        get(i + 1, jj),
                        get(i + 2, jj),
                        s,
                    )
                };
                catmull_rom(row(j - 1), row(j), row(j + 1), row(j + 2), t)
            }
            Warp::Bezier => {
                let (pi, s) = cell(u, (cols - 1) / 3);
                let (pj, t) = cell(v, (rows - 1) / 3);
                let bu = bernstein(s);
                let bv = bernstein(t);
                let mut out = [0.0f32; 2];
                for (b, wv) in bv.iter().enumerate() {
                    for (a, wu) in bu.iter().enumerate() {
                        let p = at(pi * 3 + a, pj * 3 + b);
                        out[0] += p[0] * wu * wv;
                        out[1] += p[1] * wu * wv;
                    }
                }
                out
            }
        }
    }
}

/// The cell index and the position within it of `u` over `n` cells.
fn cell(u: f32, n: usize) -> (usize, f32) {
    let x = u.clamp(0.0, 1.0) * n as f32;
    let i = (x.floor() as usize).min(n - 1);
    (i, x - i as f32)
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn catmull_rom(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let f = |a: f32, b: f32, c: f32, d: f32| {
        0.5 * (2.0 * b
            + (-a + c) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
            + (-a + 3.0 * b - 3.0 * c + d) * t * t * t)
    };
    [f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])]
}

fn bernstein(t: f32) -> [f32; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t]
}

/// The projective map from the unit square to the quad of 2×2 row-major
/// `points` (top left, top right, bottom left, bottom right); Heckbert's
/// square-to-quad construction.
pub fn homography(points: &[[f32; 2]]) -> impl Fn(f32, f32) -> [f32; 2] + use<> {
    let p = |i: usize| [f64::from(points[i][0]), f64::from(points[i][1])];
    // Corners in order around the quad.
    let ([x0, y0], [x1, y1], [x2, y2], [x3, y3]) = (p(0), p(1), p(3), p(2));
    let sx = x0 - x1 + x2 - x3;
    let sy = y0 - y1 + y2 - y3;
    let (a, b, c, d, e, f, g, h);
    if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
        (a, b, c) = (x1 - x0, x3 - x0, x0);
        (d, e, f) = (y1 - y0, y3 - y0, y0);
        (g, h) = (0.0, 0.0);
    } else {
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let det = dx1 * dy2 - dx2 * dy1;
        let det = if det.abs() < 1e-12 { 1e-12 } else { det };
        g = (sx * dy2 - dx2 * sy) / det;
        h = (dx1 * sy - sx * dy1) / det;
        (a, b, c) = (x1 - x0 + g * x1, x3 - x0 + h * x3, x0);
        (d, e, f) = (y1 - y0 + g * y1, y3 - y0 + h * y3, y0);
    }
    move |u, v| {
        let (u, v) = (f64::from(u), f64::from(v));
        let w = g * u + h * v + 1.0;
        [
            ((a * u + b * v + c) / w) as f32,
            ((d * u + e * v + f) / w) as f32,
        ]
    }
}

/// The soft-edge weight at `t` (0 at the slice's edge, 1 where the blend
/// zone ends): an S-curve of steepness `power` (Paul Bourke's edge-blend
/// function), raised to `1 / gamma` so two overlapping projectors sum to
/// constant light. The shader computes the same function.
pub fn blend_weight(t: f32, power: f32, gamma: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let s = if t < 0.5 {
        0.5 * (2.0 * t).powf(power)
    } else {
        1.0 - 0.5 * (2.0 * (1.0 - t)).powf(power)
    };
    s.powf(1.0 / gamma.max(0.01))
}

/// A whole output: its slices, drawn in order (later ones on top).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputMap {
    pub slices: Vec<Slice>,
}

impl Default for OutputMap {
    /// The identity map: one slice, the whole source over the whole output.
    fn default() -> Self {
        Self {
            slices: vec![Slice::new("main")],
        }
    }
}

fn numbers(line: usize, key: &str, values: &str) -> Result<Vec<f32>, String> {
    values
        .split_whitespace()
        .map(|w| {
            w.parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("line {line}: `{key}`: `{w}` is not a number"))
        })
        .collect()
}

fn fixed<const N: usize>(line: usize, key: &str, values: &str) -> Result<[f32; N], String> {
    let v = numbers(line, key, values)?;
    v.as_slice()
        .try_into()
        .map_err(|_| format!("line {line}: `{key}` takes {N} numbers, got {}", v.len()))
}

fn pairs(line: usize, key: &str, values: &str) -> Result<Vec<[f32; 2]>, String> {
    let v = numbers(line, key, values)?;
    if v.len() % 2 != 0 {
        return Err(format!(
            "line {line}: `{key}` takes x y pairs, got {} numbers",
            v.len()
        ));
    }
    Ok(v.chunks_exact(2).map(|c| [c[0], c[1]]).collect())
}

/// A number in canonical form: at most four decimals, no trailing zeros.
fn num(out: &mut String, x: f32) {
    let r = (f64::from(x) * 10_000.0).round() / 10_000.0;
    let r = if r == 0.0 { 0.0 } else { r };
    let s = format!("{r:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.push_str(s);
}

fn nums(out: &mut String, key: &str, values: &[f32]) {
    out.push_str(key);
    out.push_str(" =");
    for v in values {
        out.push(' ');
        num(out, *v);
    }
    out.push('\n');
}

impl OutputMap {
    /// Reads a description. Empty text (or only comments) is the identity
    /// map. Errors name the line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut slices: Vec<Slice> = Vec::new();
        // Lines each slice's points came from, to report count mismatches.
        let mut points_line: Vec<usize> = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = n + 1;
            let l = raw.split('#').next().unwrap_or("").trim();
            if l.is_empty() {
                continue;
            }
            if let Some(head) = l.strip_prefix('[') {
                let Some(head) = head.strip_suffix(']') else {
                    return Err(format!("line {line}: a header ends with `]`"));
                };
                let mut words = head.trim().splitn(2, char::is_whitespace);
                if words.next() != Some("slice") {
                    return Err(format!("line {line}: expected `[slice name]`"));
                }
                if slices.len() == MAX_SLICES {
                    return Err(format!("line {line}: at most {MAX_SLICES} slices"));
                }
                let name = words.next().map(str::trim).unwrap_or("");
                let name = if name.is_empty() {
                    format!("{}", slices.len() + 1)
                } else {
                    name.to_owned()
                };
                if slices.iter().any(|s| s.name == name) {
                    return Err(format!("line {line}: a second slice named `{name}`"));
                }
                slices.push(Slice::new(&name));
                points_line.push(0);
                continue;
            }
            let Some((key, value)) = l.split_once('=') else {
                return Err(format!("line {line}: expected `key = values`"));
            };
            let (key, value) = (key.trim(), value.trim());
            let Some(s) = slices.last_mut() else {
                return Err(format!(
                    "line {line}: `{key}` before the first `[slice name]`"
                ));
            };
            match key {
                "source" => s.source = fixed(line, key, value)?,
                "dst" => s.dst = fixed(line, key, value)?,
                "warp" => {
                    s.warp = Warp::from_name(value).ok_or_else(|| {
                        format!(
                            "line {line}: unknown warp `{value}` (one of {})",
                            Warp::NAMES.join(", ")
                        )
                    })?;
                }
                "grid" => {
                    let [c, r] = fixed::<2>(line, key, value)?;
                    let ok = |v: f32| v.fract() == 0.0 && (2.0..=MAX_GRID as f32).contains(&v);
                    if !ok(c) || !ok(r) {
                        return Err(format!(
                            "line {line}: `grid` takes two whole numbers from 2 to {MAX_GRID}"
                        ));
                    }
                    s.grid = [c as u32, r as u32];
                }
                "points" => {
                    s.points
                        .get_or_insert_with(Vec::new)
                        .extend(pairs(line, key, value)?);
                    *points_line.last_mut().expect("pushed with the slice") = line;
                }
                "blend" => s.blend = fixed(line, key, value)?,
                "blend_power" => {
                    let [p] = fixed(line, key, value)?;
                    s.blend_power = p.clamp(0.1, 16.0);
                }
                "blend_gamma" => {
                    let [g] = fixed(line, key, value)?;
                    s.blend_gamma = g.clamp(0.1, 8.0);
                }
                "mask" => s.mask.extend(pairs(line, key, value)?),
                "mask_invert" => {
                    s.mask_invert = match value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("line {line}: `mask_invert` is true or false")),
                    }
                }
                "subdiv" => {
                    let [d] = fixed(line, key, value)?;
                    if d.fract() != 0.0 || !(1.0..=MAX_SUBDIV as f32).contains(&d) {
                        return Err(format!(
                            "line {line}: `subdiv` is a whole number from 1 to {MAX_SUBDIV}"
                        ));
                    }
                    s.subdiv = d as u32;
                }
                _ => return Err(format!("line {line}: unknown key `{key}`")),
            }
        }
        if slices.is_empty() {
            return Ok(Self::default());
        }
        for (s, line) in slices.iter().zip(points_line) {
            let [cols, rows] = s.grid;
            if let Some(p) = &s.points
                && p.len() != (cols * rows) as usize
            {
                return Err(format!(
                    "line {line}: slice `{}` has {} points; a {cols}×{rows} grid needs {}",
                    s.name,
                    p.len(),
                    cols * rows
                ));
            }
            if s.warp == Warp::Perspective && s.grid != [2, 2] {
                return Err(format!(
                    "slice `{}`: a perspective warp takes a 2×2 grid (four corners)",
                    s.name
                ));
            }
            if s.warp == Warp::Bezier && ((cols - 1) % 3 != 0 || (rows - 1) % 3 != 0) {
                return Err(format!(
                    "slice `{}`: a Bezier warp takes 4, 7, 10, 13 or 16 points per axis",
                    s.name
                ));
            }
            if !s.mask.is_empty() && !(3..=MAX_MASK_POINTS).contains(&s.mask.len()) {
                return Err(format!(
                    "slice `{}`: a mask takes 3 to {MAX_MASK_POINTS} points",
                    s.name
                ));
            }
        }
        Ok(Self { slices })
    }

    /// The canonical text: defaults left out, points one grid row per line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.slices.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let _ = writeln!(out, "[slice {}]", s.name);
            let d = Slice::new("");
            if s.source != d.source {
                nums(&mut out, "source", &s.source);
            }
            if s.points.is_none() && s.dst != d.dst {
                nums(&mut out, "dst", &s.dst);
            }
            if s.warp != d.warp {
                let _ = writeln!(out, "warp = {}", s.warp.name());
            }
            if s.grid != d.grid {
                let _ = writeln!(out, "grid = {} {}", s.grid[0], s.grid[1]);
            }
            if let Some(p) = &s.points {
                for row in p.chunks(s.grid[0] as usize) {
                    let flat: Vec<f32> = row.iter().flatten().copied().collect();
                    nums(&mut out, "points", &flat);
                }
            }
            if s.blend != d.blend {
                nums(&mut out, "blend", &s.blend);
            }
            if s.blend_power != d.blend_power {
                nums(&mut out, "blend_power", &[s.blend_power]);
            }
            if s.blend_gamma != d.blend_gamma {
                nums(&mut out, "blend_gamma", &[s.blend_gamma]);
            }
            if !s.mask.is_empty() {
                let flat: Vec<f32> = s.mask.iter().flatten().copied().collect();
                nums(&mut out, "mask", &flat);
            }
            if s.mask_invert {
                out.push_str("mask_invert = true\n");
            }
            if s.subdiv != d.subdiv {
                let _ = writeln!(out, "subdiv = {}", s.subdiv);
            }
        }
        out
    }
}

/// Bytes per vertex: output position, source uv, slice uv (`vec2f` each)
/// and the slice index (`u32`, padded).
pub(crate) const VERTEX_BYTES: usize = 32;

/// Writes the triangle mesh of `map` into `vertices` (bytes) and `indices`,
/// replacing their contents (their capacity is kept for the next edit).
pub(crate) fn tessellate(map: &OutputMap, vertices: &mut Vec<u8>, indices: &mut Vec<u32>) {
    vertices.clear();
    indices.clear();
    for (k, s) in map.slices.iter().enumerate() {
        let points = s.control_points();
        let n = s.subdiv;
        let base = (vertices.len() / VERTEX_BYTES) as u32;
        for j in 0..=n {
            for i in 0..=n {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                let p = s.eval(&points, u, v);
                let [sx, sy, sw, sh] = s.source;
                for f in [p[0], p[1], sx + u * sw, sy + v * sh, u, v] {
                    vertices.extend_from_slice(&f.to_le_bytes());
                }
                vertices.extend_from_slice(&(k as u32).to_le_bytes());
                vertices.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        let row = n + 1;
        for j in 0..n {
            for i in 0..n {
                let a = base + j * row + i;
                indices.extend_from_slice(&[a, a + 1, a + row, a + 1, a + row + 1, a + row]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_the_identity_map() {
        assert_eq!(OutputMap::parse("").unwrap(), OutputMap::default());
        assert_eq!(
            OutputMap::parse("# nothing\n\n").unwrap(),
            OutputMap::default()
        );
    }

    #[test]
    fn text_round_trips_in_canonical_form() {
        let text = "\
[slice left]
source = 0 0 0.55 1
warp = perspective
points = 0.02 0.05 0.98 0
points = 0 1 1 0.96
blend = 0 0.1 0 0

[slice right]
source = 0.45 0 0.55 1
dst = 0.5 0 0.5 1
warp = catmull_rom
grid = 3 2
blend = 0.1 0 0 0
blend_power = 3
blend_gamma = 1.8
mask = 0.5 0 1 0 1 1 0.5 1
mask_invert = true
subdiv = 16
";
        let map = OutputMap::parse(text).unwrap();
        assert_eq!(map.slices.len(), 2);
        assert_eq!(map.slices[0].points.as_ref().unwrap().len(), 4);
        assert_eq!(map.slices[1].warp, Warp::CatmullRom);
        assert_eq!(map.to_text(), text);
        assert_eq!(OutputMap::parse(&map.to_text()).unwrap(), map);
        // Numbers are written with at most four decimals.
        let mut s = Slice::new("a");
        s.blend = [1.0 / 3.0, 0.0, -0.0, 0.123_449];
        let t = OutputMap { slices: vec![s] }.to_text();
        assert!(t.contains("blend = 0.3333 0 0 0.1234\n"), "{t}");
    }

    #[test]
    fn errors_name_the_line() {
        let e = |t: &str| OutputMap::parse(t).unwrap_err();
        assert!(e("source = 0 0 1 1").starts_with("line 1: `source` before"));
        assert!(e("[slice a]\nsource = 0 0 1").starts_with("line 2: `source` takes 4"));
        assert!(e("[slice a]\nwarp = twist").contains("unknown warp"));
        assert!(e("[slice a]\ncolour = 1").contains("unknown key `colour`"));
        assert!(e("[slice a]\ngrid = 3 3\npoints = 0 0 1 1").starts_with("line 3:"));
        assert!(e("[slice a]\nwarp = perspective\ngrid = 3 3").contains("2×2"));
        assert!(e("[slice a]\nwarp = bezier\ngrid = 3 4").contains("Bezier"));
        assert!(e("[slice a]\n[slice a]").contains("second slice"));
        assert!(e("[slice a]\nmask = 0 0 1 1").contains("mask takes 3"));
        assert!(e("[slice a]\nsource = 0 0 x 1").contains("`x` is not a number"));
    }

    #[test]
    fn warps_pass_through_their_corners() {
        let mut s = Slice::new("a");
        s.points = Some(vec![[0.1, 0.0], [0.9, 0.1], [0.0, 1.0], [1.0, 0.8]]);
        let p = s.points.clone().unwrap();
        for warp in [Warp::Bilinear, Warp::Perspective, Warp::CatmullRom] {
            s.warp = warp;
            for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                .into_iter()
                .enumerate()
            {
                let got = s.eval(&p, u, v);
                assert!(
                    (got[0] - p[i][0]).abs() < 1e-5 && (got[1] - p[i][1]).abs() < 1e-5,
                    "{warp:?} corner {i}: {got:?}"
                );
            }
        }
        // The perspective centre is where the diagonals cross.
        let h = homography(&p);
        let c = h(0.5, 0.5);
        let (a, d) = (p[0], p[3]);
        let (b, e) = (p[1], p[2]);
        let cross = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
            (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        };
        assert!(cross(a, d, c).abs() < 1e-5 && cross(b, e, c).abs() < 1e-5);
    }

    #[test]
    fn an_even_grid_is_the_identity_for_every_warp() {
        for (warp, n) in [
            (Warp::Bilinear, 3),
            (Warp::CatmullRom, 4),
            (Warp::Bezier, 4),
            (Warp::Bezier, 7),
        ] {
            let mut s = Slice::new("a");
            s.warp = warp;
            s.grid = [n, n];
            let p = s.control_points();
            for (u, v) in [(0.3, 0.7), (0.5, 0.5), (0.9, 0.1)] {
                let got = s.eval(&p, u, v);
                assert!(
                    (got[0] - u).abs() < 1e-5 && (got[1] - v).abs() < 1e-5,
                    "{warp:?} {n}: ({u}, {v}) -> {got:?}"
                );
            }
        }
    }

    #[test]
    fn blend_curve_values() {
        assert_eq!(blend_weight(0.0, 2.0, 1.0), 0.0);
        assert!((blend_weight(0.25, 2.0, 1.0) - 0.125).abs() < 1e-6);
        assert!((blend_weight(0.5, 2.0, 1.0) - 0.5).abs() < 1e-6);
        assert!((blend_weight(0.75, 2.0, 1.0) - 0.875).abs() < 1e-6);
        assert_eq!(blend_weight(1.0, 2.0, 2.2), 1.0);
        // Two overlapping ramps sum to constant light at gamma 2.2.
        for t in [0.1f32, 0.3, 0.5, 0.8] {
            let a = blend_weight(t, 2.0, 2.2).powf(2.2);
            let b = blend_weight(1.0 - t, 2.0, 2.2).powf(2.2);
            assert!((a + b - 1.0).abs() < 1e-5, "{t}: {a} + {b}");
        }
    }

    #[test]
    fn tessellation_sizes() {
        let mut map = OutputMap::default();
        map.slices[0].subdiv = 4;
        map.slices.push(Slice::new("b"));
        let (mut v, mut i) = (Vec::new(), Vec::new());
        tessellate(&map, &mut v, &mut i);
        assert_eq!(v.len(), (25 + 33 * 33) * VERTEX_BYTES);
        assert_eq!(i.len(), (16 + 32 * 32) * 6);
        assert_eq!(*i.iter().max().unwrap() as usize, 25 + 33 * 33 - 1);
    }
}
