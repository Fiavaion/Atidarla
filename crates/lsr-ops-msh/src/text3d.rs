//! Text 3D: glyph outlines of a built-in or file font, flattened to
//! polygons, filled (holes bridged, then ear-clipped) and extruded. The
//! fonts and the outline reader are the ones `img.text` uses: `skrifa`
//! with the egui default fonts (Ubuntu Light, Hack; licences in
//! `deny.toml`). Glyphs advance without kerning or shaping.

use std::path::Path;

use lsr_core::geo::AttrClass;
use lsr_core::registry::ParamSchema;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::cook::{Job, Snap};
use crate::mesh::{Mesh, limits};

type P2 = [f64; 2];

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::string("text", "Text", "Lasair")
            .help("The text; several lines are allowed.")
            .multiline(),
        ParamSchema::menu("font", "Font", &["sans", "mono"], "sans")
            .help("Built-in font: `sans` (Ubuntu Light) or `mono` (Hack)."),
        ParamSchema::path("font_file", "Font File")
            .help("Path to a .ttf or .otf file; overrides Font when set."),
        ParamSchema::float("size", "Size", 0.5, Some(0.0), None)
            .help("The font size (its em height) in world units."),
        ParamSchema::float("depth", "Depth", 0.1, Some(0.0), None)
            .help("Thickness along Z, centred on Z = 0; 0 makes flat letters (front faces only)."),
        ParamSchema::float("spacing", "Line Spacing", 1.0, Some(0.0), None)
            .help("Distance between lines, as a multiple of the font's line height."),
        ParamSchema::menu("align", "Align", &["left", "centre", "right"], "centre")
            .help("Horizontal alignment of the lines and of the block on the origin."),
        ParamSchema::menu("valign", "Vertical Align", &["top", "middle", "baseline", "bottom"], "middle")
            .help("Which part of the block sits on the origin (baseline: the first line's)."),
        ParamSchema::float("tolerance", "Curve Tolerance", 0.002, Some(0.0), None).help(
            "Largest gap between a curve and its straight segments, as a fraction of the size (smaller: more points).",
        ),
    ]
}

/// Collects flattened contours in font units (y up).
struct Pen {
    contours: Vec<Vec<P2>>,
    origin: P2,
    tol: f64,
}

impl Pen {
    fn at(&self, x: f32, y: f32) -> P2 {
        let f = |v: f32| if v.is_finite() { f64::from(v) } else { 0.0 };
        [self.origin[0] + f(x), self.origin[1] + f(y)]
    }

    fn last(&self) -> P2 {
        self.contours
            .last()
            .and_then(|c| c.last())
            .copied()
            .unwrap_or(self.origin)
    }

    fn push(&mut self, p: P2) {
        if let Some(c) = self.contours.last_mut()
            && c.last() != Some(&p)
        {
            c.push(p);
        }
    }

    /// Segments for a curve whose control polygon is `len` long: the gap
    /// of a quadratic shrinks with the square of the step.
    fn steps(&self, len: f64) -> usize {
        if self.tol <= 0.0 {
            return 16;
        }
        ((len / self.tol).sqrt().ceil() as usize).clamp(1, 64)
    }
}

fn dist(a: P2, b: P2) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.contours.push(vec![p]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.push(p);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (a, c, b) = (self.last(), self.at(cx, cy), self.at(x, y));
        let n = self.steps(dist(a, c) + dist(c, b));
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            self.push([
                u * u * a[0] + 2.0 * u * t * c[0] + t * t * b[0],
                u * u * a[1] + 2.0 * u * t * c[1] + t * t * b[1],
            ]);
        }
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, c0, c1, b) = (
            self.last(),
            self.at(cx0, cy0),
            self.at(cx1, cy1),
            self.at(x, y),
        );
        let n = self.steps(dist(a, c0) + dist(c0, c1) + dist(c1, b));
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.push([
                w0 * a[0] + w1 * c0[0] + w2 * c1[0] + w3 * b[0],
                w0 * a[1] + w1 * c0[1] + w2 * c1[1] + w3 * b[1],
            ]);
        }
    }
    fn close(&mut self) {
        if let Some(c) = self.contours.last_mut()
            && c.len() > 1
            && c.first() == c.last()
        {
            c.pop();
        }
    }
}

fn load_font(s: &Snap) -> Result<std::borrow::Cow<'static, [u8]>, String> {
    let file = s.str("font_file");
    if !file.is_empty() {
        let bytes = std::fs::read(Path::new(file)).map_err(|e| format!("{file}: {e}"))?;
        FontRef::new(&bytes).map_err(|e| format!("{file}: {e}"))?;
        return Ok(bytes.into());
    }
    Ok(if s.str("font") == "mono" {
        epaint_default_fonts::HACK_REGULAR.into()
    } else {
        epaint_default_fonts::UBUNTU_LIGHT.into()
    })
}

/// Twice the signed area (positive: counter-clockwise).
fn area2(c: &[P2]) -> f64 {
    let n = c.len();
    (0..n)
        .map(|i| {
            let (a, b) = (c[i], c[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// Whether `q` is inside contour `c` (even-odd).
fn contains(c: &[P2], q: P2) -> bool {
    let mut inside = false;
    let n = c.len();
    for i in 0..n {
        let (a, b) = (c[i], c[(i + 1) % n]);
        if (a[1] > q[1]) != (b[1] > q[1]) {
            let x = a[0] + (q[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x > q[0] {
                inside = !inside;
            }
        }
    }
    inside
}

/// Drops repeated points and points on a straight run (a straight
/// outline segment flattened from a curve), which no ear could cut.
fn straighten(c: &mut Vec<P2>) {
    let mut changed = true;
    while changed && c.len() >= 3 {
        changed = false;
        let mut i = 0;
        while i < c.len() && c.len() >= 3 {
            let n = c.len();
            let (a, b, d) = (c[(i + n - 1) % n], c[i], c[(i + 1) % n]);
            let (u, w) = ([b[0] - a[0], b[1] - a[1]], [d[0] - b[0], d[1] - b[1]]);
            let cr = u[0] * w[1] - u[1] * w[0];
            let scale = (u[0].hypot(u[1]) * w[0].hypot(w[1])).max(1e-300);
            let along = u[0] * w[0] + u[1] * w[1] >= 0.0;
            if a == b || (cr.abs() <= 1e-7 * scale && along) {
                c.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
}

/// Contours grouped into shapes: an outer contour (counter-clockwise) and
/// its holes (clockwise), by nesting depth, whatever the font's winding.
fn shapes(mut contours: Vec<Vec<P2>>) -> Vec<(Vec<P2>, Vec<Vec<P2>>)> {
    for c in &mut contours {
        straighten(c);
    }
    contours.retain(|c| c.len() >= 3 && area2(c).abs() > 1e-12);
    let n = contours.len();
    // A point surely inside each contour's area: the midpoint of an edge,
    // nudged inward. Containment of that point by the others gives depth.
    let probe: Vec<P2> = contours
        .iter()
        .map(|c| {
            let ccw = area2(c) > 0.0;
            let (a, b) = (c[0], c[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = (dx * dx + dy * dy).sqrt().max(1e-12);
            let s = if ccw { 1e-4 } else { -1e-4 };
            [
                (a[0] + b[0]) / 2.0 - dy / l * s,
                (a[1] + b[1]) / 2.0 + dx / l * s,
            ]
        })
        .collect();
    let parents: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            (0..n)
                .filter(|&j| j != i && contains(&contours[j], probe[i]))
                .collect()
        })
        .collect();
    let depth: Vec<usize> = parents.iter().map(Vec::len).collect();
    let mut out: Vec<(Vec<P2>, Vec<Vec<P2>>)> = Vec::new();
    let mut slot = vec![usize::MAX; n];
    for i in 0..n {
        if depth[i].is_multiple_of(2) {
            let mut c = contours[i].clone();
            if area2(&c) < 0.0 {
                c.reverse();
            }
            slot[i] = out.len();
            out.push((c, Vec::new()));
        }
    }
    for i in 0..n {
        if !depth[i].is_multiple_of(2) {
            // The nearest enclosing outer contour: the parent one level up.
            if let Some(&p) = parents[i].iter().find(|&&j| depth[j] + 1 == depth[i]) {
                let mut c = contours[i].clone();
                if area2(&c) > 0.0 {
                    c.reverse();
                }
                out[slot[p]].1.push(c);
            }
        }
    }
    out
}

/// One polygon from an outer contour and its holes: each hole is joined
/// to the outer boundary by a bridge from its rightmost point to a visible
/// boundary point (the method of Eberly, "Triangulation by Ear Clipping").
/// Returns indices into `pts`, where the shape's points are appended;
/// each contour's (first point, length) goes to `contours`.
fn bridge(
    outer: &[P2],
    holes: &[Vec<P2>],
    pts: &mut Vec<P2>,
    contours: &mut Vec<(usize, usize)>,
) -> Vec<usize> {
    let base = pts.len();
    pts.extend_from_slice(outer);
    contours.push((base, outer.len()));
    let mut ring: Vec<usize> = (base..pts.len()).collect();
    // (first point, length, rightmost point) of each hole, rightmost first.
    let mut placed: Vec<(usize, usize, usize)> = Vec::new();
    for h in holes_sorted(holes) {
        let start = pts.len();
        pts.extend_from_slice(h);
        let right = (0..h.len())
            .max_by(|&a, &b| h[a][0].total_cmp(&h[b][0]))
            .unwrap_or(0);
        placed.push((start, h.len(), right));
        contours.push((start, h.len()));
    }
    for (start, len, right) in placed {
        let m = pts[start + right];
        // The nearest crossing of the ray from m to +x with the ring.
        let mut best: Option<(f64, usize)> = None;
        for k in 0..ring.len() {
            let (a, b) = (pts[ring[k]], pts[ring[(k + 1) % ring.len()]]);
            if (a[1] > m[1]) == (b[1] > m[1]) {
                continue;
            }
            let x = a[0] + (m[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x >= m[0] && best.is_none_or(|(bx, _)| x < bx) {
                // The edge's endpoint with the larger x is a candidate.
                let pick = if a[0] > b[0] { k } else { (k + 1) % ring.len() };
                best = Some((x, pick));
            }
        }
        let Some((ix, mut pick)) = best else {
            continue;
        };
        // A ring point inside the triangle (m, crossing, candidate) would
        // block the bridge: take the one making the smallest angle with
        // the ray instead.
        let i = [ix, m[1]];
        let p = pts[ring[pick]];
        // (When the ray meets the candidate itself the triangle is flat
        // and nothing can block.)
        let flat = ((i[0] - m[0]) * (p[1] - m[1]) - (i[1] - m[1]) * (p[0] - m[0])).abs()
            <= 1e-12 * (p[0] - m[0]).abs().max(1e-12);
        let mut best_angle = f64::MAX;
        for k in 0..ring.len() {
            let q = pts[ring[k]];
            if flat || k == pick || q == m || q[0] < m[0] {
                continue;
            }
            if in_triangle(m, i, p, q) {
                let ang = (q[1] - m[1]).abs().atan2(q[0] - m[0]);
                if ang < best_angle {
                    best_angle = ang;
                    pick = k;
                }
            }
        }
        let mut seq = Vec::with_capacity(ring.len() + len + 2);
        seq.extend_from_slice(&ring[..=pick]);
        for j in 0..=len {
            seq.push(start + (right + j) % len);
        }
        seq.extend_from_slice(&ring[pick..]);
        ring = seq;
    }
    ring
}

fn holes_sorted(holes: &[Vec<P2>]) -> Vec<&Vec<P2>> {
    let mut hs: Vec<&Vec<P2>> = holes.iter().collect();
    let right = |h: &Vec<P2>| h.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
    hs.sort_by(|a, b| right(b).total_cmp(&right(a)));
    hs
}

fn in_triangle(a: P2, b: P2, c: P2, q: P2) -> bool {
    let s = |o: P2, u: P2, v: P2| (u[0] - o[0]) * (v[1] - o[1]) - (u[1] - o[1]) * (v[0] - o[0]);
    let (d1, d2, d3) = (s(a, b, q), s(b, c, q), s(c, a, q));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// Ear clipping of a counter-clockwise polygon whose holes are bridged in
/// (repeated points do not block an ear). When numerical trouble leaves no
/// ear, the flattest corner is cut anyway, so the cap stays one closed
/// piece. Returns the number of such forced cuts.
fn ear_clip(pts: &[P2], ring: &[usize], out: &mut Vec<[u32; 3]>) -> usize {
    let cross = |a: P2, b: P2, c: P2| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    let mut v: Vec<usize> = ring.to_vec();
    let mut forced = 0;
    let (mut i, mut misses) = (0usize, 0usize);
    while v.len() > 3 {
        let m = v.len();
        i %= m;
        let (ia, ib, ic) = (v[(i + m - 1) % m], v[i], v[(i + 1) % m]);
        let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
        let ear = cross(a, b, c) > 0.0
            && !v.iter().any(|&j| {
                let q = pts[j];
                q != a && q != b && q != c && in_triangle(a, b, c, q)
            });
        if ear {
            out.push([ia as u32, ib as u32, ic as u32]);
            v.remove(i);
            i = i.saturating_sub(1);
            misses = 0;
            continue;
        }
        i += 1;
        misses += 1;
        if misses > m {
            let k = (0..m)
                .min_by(|&x, &y| {
                    let f =
                        |k: usize| cross(pts[v[(k + m - 1) % m]], pts[v[k]], pts[v[(k + 1) % m]]);
                    // Prefer convex corners, then the flattest.
                    (f(x) <= 0.0)
                        .cmp(&(f(y) <= 0.0))
                        .then(f(x).abs().total_cmp(&f(y).abs()))
                })
                .unwrap_or(0);
            out.push([
                v[(k + m - 1) % m] as u32,
                v[k] as u32,
                v[(k + 1) % m] as u32,
            ]);
            v.remove(k);
            forced += 1;
            misses = 0;
        }
    }
    if v.len() == 3 {
        out.push([v[0] as u32, v[1] as u32, v[2] as u32]);
    }
    forced
}

pub fn run(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let text = s.str("text");
    if text.is_empty() {
        return Ok(Mesh::new());
    }
    let bytes = load_font(s)?;
    let font = FontRef::new(&bytes).map_err(|e| format!("the font: {e}"))?;
    let em = s.f64("size").max(0.0);
    if em <= 0.0 {
        return Ok(Mesh::new());
    }
    let size = Size::new(em as f32);
    let location = LocationRef::default();
    let metrics = font.metrics(size, location);
    let advances = font.glyph_metrics(size, location);
    let charmap = font.charmap();
    let outlines = font.outline_glyphs();
    let (ascent, descent) = (f64::from(metrics.ascent), f64::from(metrics.descent));
    let line_h = (ascent - descent + f64::from(metrics.leading)) * s.f64("spacing");
    let lines: Vec<(Vec<(GlyphId, f64)>, f64)> = text
        .split('\n')
        .map(|line| {
            let mut x = 0.0;
            let glyphs = line
                .chars()
                .map(|ch| {
                    let id = charmap.map(ch).unwrap_or(GlyphId::NOTDEF);
                    let at = x;
                    x += f64::from(advances.advance_width(id).unwrap_or(0.0));
                    (id, at)
                })
                .collect();
            (glyphs, x)
        })
        .collect();
    let widest = lines.iter().map(|l| l.1).fold(0.0, f64::max);
    let last_baseline = -line_h * (lines.len() as f64 - 1.0);
    let x0 = match s.str("align") {
        "left" => 0.0,
        "right" => -widest,
        _ => -widest / 2.0,
    };
    let y0 = match s.str("valign") {
        "top" => -ascent,
        "baseline" => 0.0,
        "bottom" => -(last_baseline + descent),
        _ => -(ascent + last_baseline + descent) / 2.0,
    };
    let tol = s.f64("tolerance").max(0.0) * em;
    let mut pen = Pen {
        contours: Vec::new(),
        origin: [0.0; 2],
        tol,
    };
    for (row, (glyphs, w)) in lines.iter().enumerate() {
        job.check()?;
        let shift = match s.str("align") {
            "left" => 0.0,
            "right" => widest - w,
            _ => (widest - w) / 2.0,
        };
        for (id, x) in glyphs {
            let Some(glyph) = outlines.get(*id) else {
                continue;
            };
            pen.origin = [x0 + shift + x, y0 - line_h * row as f64];
            let _ = glyph.draw(DrawSettings::unhinted(size, location), &mut pen);
        }
    }
    let shapes = shapes(pen.contours);
    extrude(&shapes, s.f64("depth").max(0.0), job)
}

/// Front faces at +depth/2 (facing +Z), back faces and side walls; groups
/// `front`, `back` and `side`.
fn extrude(shapes: &[(Vec<P2>, Vec<Vec<P2>>)], depth: f64, job: &Job) -> Result<Mesh, String> {
    let mut pts: Vec<P2> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut contours: Vec<(usize, usize)> = Vec::new();
    for (i, (outer, holes)) in shapes.iter().enumerate() {
        job.every(i)?;
        let ring = bridge(outer, holes, &mut pts, &mut contours);
        ear_clip(&pts, &ring, &mut tris);
    }
    let n = pts.len();
    let walls: usize = contours.iter().map(|c| c.1).sum();
    let solid = depth > 0.0;
    let layers = if solid { 2 } else { 1 };
    limits::check(
        (n * layers) as u64,
        (tris.len() * layers + if solid { walls } else { 0 }) as u64,
        (tris.len() * 3 * layers + walls * 4) as u64,
    )?;
    let z = (depth / 2.0) as f32;
    let mut m = Mesh::new();
    for p in &pts {
        m.add_point([p[0] as f32, p[1] as f32, if solid { z } else { 0.0 }]);
    }
    for t in &tris {
        m.add_polygon(t);
    }
    let mut role: Vec<u8> = vec![0; tris.len()];
    if solid {
        for p in &pts {
            m.add_point([p[0] as f32, p[1] as f32, -z]);
        }
        let k = n as u32;
        for t in &tris {
            m.add_polygon(&[t[0] + k, t[2] + k, t[1] + k]);
        }
        role.extend(std::iter::repeat_n(1, tris.len()));
        // Side walls between the caps' outline points (welded: a closed
        // solid; without normals it renders faceted, add Normals with a
        // cusp angle for rounded sides).
        for &(first, len) in &contours {
            let (first, len) = (first as u32, len as u32);
            for e in 0..len {
                let (a, b) = (first + e, first + (e + 1) % len);
                m.add_polygon(&[a + k, b + k, b, a]);
                role.push(2);
            }
        }
    }
    for (name, r) in [("front", 0u8), ("back", 1), ("side", 2)] {
        if solid || r == 0 {
            m.set_group(
                AttrClass::Prim,
                name,
                &role.iter().map(|&x| x == r).collect::<Vec<_>>(),
            )?;
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every printable ASCII glyph of both built-in fonts fills without a
    /// forced cut (a forced cut means a bridge or an ear went wrong).
    #[test]
    fn every_ascii_glyph_fills_cleanly() {
        for bytes in [
            epaint_default_fonts::UBUNTU_LIGHT,
            epaint_default_fonts::HACK_REGULAR,
        ] {
            let font = FontRef::new(bytes).unwrap();
            let size = Size::new(0.5);
            let outlines = font.outline_glyphs();
            for ch in '!'..='~' {
                let Some(glyph) = font.charmap().map(ch).and_then(|id| outlines.get(id)) else {
                    continue;
                };
                let mut pen = Pen {
                    contours: Vec::new(),
                    origin: [0.0; 2],
                    tol: 0.001,
                };
                glyph
                    .draw(
                        DrawSettings::unhinted(size, LocationRef::default()),
                        &mut pen,
                    )
                    .unwrap();
                for (outer, holes) in shapes(pen.contours) {
                    let (mut pts, mut cs, mut tris) = (Vec::new(), Vec::new(), Vec::new());
                    let ring = bridge(&outer, &holes, &mut pts, &mut cs);
                    assert_eq!(ring.len(), pts.len() + 2 * holes.len(), "{ch}");
                    assert_eq!(ear_clip(&pts, &ring, &mut tris), 0, "{ch}: forced cuts");
                    assert_eq!(tris.len(), ring.len() - 2, "{ch}");
                }
            }
        }
    }

    #[test]
    fn nesting_finds_holes_whatever_the_winding() {
        let sq = |x: f64, y: f64, r: f64| {
            vec![
                [x - r, y - r],
                [x + r, y - r],
                [x + r, y + r],
                [x - r, y + r],
            ]
        };
        // Both counter-clockwise: the inner one is still a hole; a square
        // inside the hole is a new outer shape.
        let s = shapes(vec![
            sq(0.0, 0.0, 3.0),
            sq(0.0, 0.0, 2.0),
            sq(0.0, 0.0, 1.0),
        ]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.iter().map(|x| x.1.len()).sum::<usize>(), 1);
        for (outer, holes) in &s {
            assert!(area2(outer) > 0.0);
            assert!(holes.iter().all(|h| area2(h) < 0.0));
        }
    }
}
