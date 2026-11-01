//! Bevel: replaces sharp edges by strips, chamfered (one segment) or
//! rounded (several segments on circular arcs tangent to both faces), and
//! closes the corners where strips meet with patches.
//!
//! Each polygon is shrunk away from its bevelled edges: a corner between
//! two bevelled edges moves to where the edges' offset lines cross, a
//! corner next to one bevelled edge slides along its other edge, and a
//! corner between unbevelled edges stays (gaining the slid points of its
//! neighbours, so no edge gets a T-junction). A strip joins the two
//! polygons of each bevelled edge; the holes left at the corners are
//! filled: one polygon when it is flat, otherwise a fan of triangles from
//! its (rounded) centre, or from the original point where a polygon still
//! keeps it. A strip quad twisted by more than `FLAT_TOLERANCE` of its
//! size (a slid point shared by two strips sits off each one's offset
//! line) becomes two triangles; slighter twists stay quads.
//!
//! Where one strip ends at an inside point, the point splits: the two
//! polygons beside the strip lose it to their slid points. With three
//! polygons the third is cut along the strip's end, the arc moved along
//! the strip into that polygon's plane; with four or more the point stays
//! in the others and the end is a fan of flat triangles from it over the
//! strip's end (one triangle for a chamfer).
//!
//! Points on an open border bevel too when their polygons form one open
//! fan: a strip runs out to the border, which keeps its place and gains
//! the slid points. The gap there is open; it is patched (closed along
//! the border) unless it is just the end of one strip. Border edges have
//! one polygon and no angle, so they are never bevelled.

use std::collections::{HashMap, HashSet};

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, newell, v3};
use crate::normals::point_normals;
use crate::topo::{Edges, key, loops};

type V = [f64; 3];

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::float("offset", "Offset", 0.05, Some(0.0), None).help(
            "How far each polygon is pulled back from a bevelled edge (limited to just under half the shortest edge at a bevelled corner).",
        ),
        ParamSchema::int("segments", "Segments", 1, Some(1), Some(64))
            .help("Polygons across each bevel: 1 is a flat chamfer, more round it."),
        ParamSchema::float("angle", "Angle", 30.0, Some(0.0), Some(180.0)).help(
            "Only edges where the two polygons' normals differ by more than this many degrees are bevelled.",
        ),
        ParamSchema::string("group", "Group", "").help(
            "Only edges with both points in this point group are bevelled (empty: all sharp edges).",
        ),
    ]
}

/// A polygon corner: polygon `f`, its point `v`, the previous point `u`
/// and the next point `w`.
#[derive(Debug, Clone, Copy)]
struct Corner {
    f: u32,
    u: u32,
    v: u32,
    w: u32,
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let np = m.points();
    let segments = s.i64("segments").clamp(1, 64) as usize;
    let cos_angle = s.f64("angle").clamp(0.0, 180.0).to_radians().cos();
    let name = s.str("group");
    let in_group: Option<Vec<bool>> = if name.is_empty() {
        None
    } else {
        Some(
            m.group_members(AttrClass::Point, name)
                .ok_or_else(|| format!("the input has no point group `{name}`"))?,
        )
    };
    let edges = Edges::new(m);
    let border = edges.border_points(m);
    let normal: Vec<V> = (0..m.prims()).map(|f| v3::norm(m.prim_normal(f))).collect();
    let pos: Vec<V> = m.p.iter().map(|&p| v3::of(p)).collect();
    // Corners, and the corner of each directed polygon edge v → w.
    let mut corners: Vec<Corner> = Vec::with_capacity(m.corners.len());
    let mut first_corner = vec![0usize; m.prims() + 1];
    for f in 0..m.prims() {
        first_corner[f] = corners.len();
        if m.closed[f] {
            let p = m.prim(f);
            let k = p.len();
            for i in 0..k {
                corners.push(Corner {
                    f: f as u32,
                    u: p[(i + k - 1) % k],
                    v: p[i],
                    w: p[(i + 1) % k],
                });
            }
        }
    }
    first_corner[m.prims()] = corners.len();
    let mut directed: HashMap<(u32, u32), usize> = HashMap::with_capacity(corners.len());
    let mut bad = vec![false; np];
    for (c, k) in corners.iter().enumerate() {
        if directed.insert((k.v, k.w), c).is_some() || k.v == k.w {
            bad[k.v as usize] = true;
            bad[k.w as usize] = true;
        }
    }
    // Points whose polygons do not form one consistently wound fan, closed
    // inside the mesh or open at a border, are left alone.
    let mut around: Vec<Vec<usize>> = vec![Vec::new(); np];
    for (c, k) in corners.iter().enumerate() {
        around[k.v as usize].push(c);
    }
    for v in 0..np {
        job.every(v)?;
        if bad[v] || around[v].is_empty() {
            continue;
        }
        // A border fan starts at the one corner whose outgoing edge v → w
        // has no polygon on its other side.
        let start = if border[v] {
            let mut open = around[v]
                .iter()
                .copied()
                .filter(|&c| !directed.contains_key(&(corners[c].w, corners[c].v)));
            match (open.next(), open.next()) {
                (Some(c), None) => c,
                _ => {
                    bad[v] = true;
                    continue;
                }
            }
        } else {
            around[v][0]
        };
        let mut c = start;
        let mut visited = 1;
        loop {
            // Across the incoming edge u → v lies the polygon with v → u.
            let k = corners[c];
            match directed.get(&(k.v, k.u)) {
                Some(&next) if next != start => c = next,
                Some(_) => break,
                None => {
                    bad[v] |= !border[v];
                    break;
                }
            }
            visited += 1;
            if visited > around[v].len() {
                break;
            }
        }
        if visited != around[v].len() {
            bad[v] = true;
        }
    }
    // The bevelled edges.
    let mut selected: HashSet<(u32, u32)> = HashSet::new();
    let mut keys: Vec<(u32, u32)> = edges.uses.keys().copied().collect();
    keys.sort_unstable();
    for (a, b) in keys {
        let uses = edges.of(a, b);
        if uses.len() != 2 || bad[a as usize] || bad[b as usize] {
            continue;
        }
        if let Some(g) = &in_group
            && !(g[a as usize] && g[b as usize])
        {
            continue;
        }
        let (f, g) = (uses[0].0 as usize, uses[1].0 as usize);
        if v3::dot(normal[f], normal[g]) < cos_angle - 1e-9 {
            selected.insert((a, b));
        }
    }
    let bevelled = |a: u32, b: u32| selected.contains(&key(a, b));
    let mut touched = vec![false; np];
    let mut ends = vec![0u32; np];
    // The other end of a point's bevelled edge (where it has just one).
    let mut partner = vec![0u32; np];
    for &(a, b) in &selected {
        touched[a as usize] = true;
        touched[b as usize] = true;
        ends[a as usize] += 1;
        ends[b as usize] += 1;
        partner[a as usize] = b;
        partner[b as usize] = a;
    }
    if selected.is_empty() {
        let mut out = m.clone();
        out.set_group(AttrClass::Prim, "bevel", &vec![false; m.prims()])?;
        return Ok(out);
    }
    let mut out = Mesh {
        p: m.p.clone(),
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    // The offset, kept below half the shortest edge at a bevelled corner.
    let mut shortest = f64::MAX;
    for k in &corners {
        if touched[k.v as usize] {
            shortest = shortest.min(v3::len(v3::sub(pos[k.w as usize], pos[k.v as usize])));
        }
    }
    let off = s.f64("offset").max(0.0).min(0.49 * shortest);
    let selected_count = selected.len();
    let valence: usize = (0..np)
        .filter(|&v| touched[v])
        .map(|v| around[v].len())
        .sum();
    limits::check(
        (np + corners.len() + 2 * selected_count * (segments + 1) + np) as u64,
        (m.prims() + selected_count * segments + valence * segments * 2) as u64,
        (m.corners.len() * 2 + selected_count * segments * 4 + valence * segments * 6) as u64,
    )?;
    let mut pts = Interp::new(&m.point_attrs, np + corners.len());
    for i in 0..np {
        pts.copy(i as u32);
    }
    // owner[p]: the original point a new point belongs to.
    let mut owner: Vec<u32> = (0..np as u32).collect();
    // Slides along unbevelled edges v → w: requested distances first.
    let mut slide_req: HashMap<(u32, u32), (f64, u32)> = HashMap::new();
    let along = |v: u32, w: u32| v3::norm(v3::sub(pos[w as usize], pos[v as usize]));
    let inward = |f: u32, a: u32, b: u32| v3::cross(normal[f as usize], along(a, b));
    for k in &corners {
        if !touched[k.v as usize] {
            continue;
        }
        let (bp, bn) = (bevelled(k.u, k.v), bevelled(k.v, k.w));
        let req = match (bp, bn) {
            (true, false) => Some((
                k.w,
                off / v3::dot(along(k.v, k.w), inward(k.f, k.u, k.v)).max(0.1),
            )),
            (false, true) => Some((
                k.u,
                off / v3::dot(along(k.v, k.u), inward(k.f, k.v, k.w)).max(0.1),
            )),
            _ => None,
        };
        if let Some((to, d)) = req {
            let e = slide_req.entry((k.v, to)).or_insert((0.0, 0));
            e.0 += d;
            e.1 += 1;
        }
    }
    let mut req_keys: Vec<(u32, u32)> = slide_req.keys().copied().collect();
    req_keys.sort_unstable();
    let mut slide: HashMap<(u32, u32), u32> = HashMap::new();
    for (v, w) in req_keys {
        let (sum, n) = slide_req[&(v, w)];
        let len = v3::len(v3::sub(pos[w as usize], pos[v as usize]));
        let t = (sum / f64::from(n) / len.max(1e-30)).min(0.49);
        let p = v3::lerp(pos[v as usize], pos[w as usize], t);
        pts.blend(&[(v, (1.0 - t) as f32), (w, t as f32)]);
        owner.push(v);
        slide.insert((v, w), out.add_point(v3::f32(p)));
    }
    // Each corner's chain of points in its polygon. Where one bevelled edge
    // ends at an inside point of three polygons, the strip's end runs
    // across the third polygon, which is cut along it (`cut`) instead of
    // keeping the point and folding a patch back over itself.
    let mut chain: Vec<Vec<u32>> = Vec::with_capacity(corners.len());
    let mut cut = vec![false; corners.len()];
    for (c, k) in corners.iter().enumerate() {
        let v = k.v;
        if !touched[v as usize] {
            chain.push(vec![v]);
            continue;
        }
        let vi = v as usize;
        if ends[vi] == 1
            && !border[vi]
            && around[vi].len() == 3
            && let (Some(&a), Some(&b)) = (slide.get(&(v, k.u)), slide.get(&(v, k.w)))
        {
            cut[c] = true;
            chain.push(vec![a, b]);
            continue;
        }
        let (bp, bn) = (bevelled(k.u, v), bevelled(v, k.w));
        chain.push(match (bp, bn) {
            (true, true) => {
                let pa = inward(k.f, k.u, v);
                let pb = inward(k.f, v, k.w);
                let d = v3::scale(v3::add(pa, pb), off / (1.0 + v3::dot(pa, pb)).max(0.1));
                pts.copy(v);
                owner.push(v);
                vec![out.add_point(v3::f32(v3::add(pos[v as usize], d)))]
            }
            (true, false) => vec![slide[&(v, k.w)]],
            (false, true) => vec![slide[&(v, k.u)]],
            (false, false) => {
                let mut c = Vec::with_capacity(3);
                c.extend(slide.get(&(v, k.u)));
                c.push(v);
                c.extend(slide.get(&(v, k.w)));
                c
            }
        });
    }
    let mut ids: Vec<u32> = Vec::new();
    // Strips (added after the polygons), with arcs shared by strips that
    // end on the same two points.
    let mut strips: Vec<(Vec<u32>, u32)> = Vec::with_capacity(selected_count * segments);
    let mut is_arc: HashSet<u32> = HashSet::new();
    let mut arcs: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let mut centre_sum: Vec<(V, u32)> = vec![([0.0; 3], 0); np];
    let mut sel: Vec<(u32, u32)> = selected.iter().copied().collect();
    sel.sort_unstable();
    for (a, b) in sel {
        job.check()?;
        let (Some(&cf), Some(&cg)) = (directed.get(&(a, b)), directed.get(&(b, a))) else {
            continue;
        };
        let (f, g) = (corners[cf].f, corners[cg].f);
        let (nf, ng) = (normal[f as usize], normal[g as usize]);
        let next_of = |c: usize| {
            let f = corners[c].f as usize;
            let (lo, hi) = (first_corner[f], first_corner[f + 1]);
            if c + 1 == hi { lo } else { c + 1 }
        };
        // f runs a → b: its corner at a ends on the strip, its corner at
        // b starts on it; g runs b → a.
        let ef_a = *chain[cf].last().expect("a point");
        let sf_b = chain[next_of(cf)][0];
        let eg_b = *chain[cg].last().expect("a point");
        let sg_a = chain[next_of(cg)][0];
        let mut arc = |p: u32,
                       q: u32,
                       v: u32,
                       out: &mut Mesh,
                       pts: &mut Interp,
                       owner: &mut Vec<u32>|
         -> Vec<u32> {
            let mut row = vec![p];
            if segments > 1 {
                if let Some(r) = arcs.get(&(q, p)) {
                    row.extend(r.iter().rev());
                } else {
                    let r = arcs.entry((p, q)).or_insert_with(|| {
                        let (pp, pq) = (v3::of(out.p[p as usize]), v3::of(out.p[q as usize]));
                        let points = arc_points(pp, pq, nf, ng, segments);
                        if let Some(o) = points.1 {
                            let c = &mut centre_sum[v as usize];
                            c.0 = v3::add(c.0, o);
                            c.1 += 1;
                        }
                        points
                            .0
                            .into_iter()
                            .map(|x| {
                                pts.copy(v);
                                owner.push(v);
                                let id = out.add_point(v3::f32(x));
                                is_arc.insert(id);
                                id
                            })
                            .collect()
                    });
                    row.extend_from_slice(r);
                }
            }
            row.push(q);
            row
        };
        let left = arc(ef_a, sg_a, a, &mut out, &mut pts, &mut owner);
        let right = arc(sf_b, eg_b, b, &mut out, &mut pts, &mut owner);
        for i in 0..segments {
            let quad = [right[i], left[i], left[i + 1], right[i + 1]];
            ids.clear();
            ids.extend_from_slice(&quad);
            ids.dedup();
            if ids.len() == 4 && !flat(&out, &ids, FLAT_TOLERANCE) {
                // Twisted, where a slid point is shared with another strip
                // (two strips ending at one point between them): two
                // triangles.
                strips.push((vec![ids[0], ids[1], ids[2]], f));
                strips.push((vec![ids[0], ids[2], ids[3]], f));
            } else if ids.len() >= 3 {
                strips.push((ids.clone(), f));
            }
        }
    }
    let mut prims = Interp::new(&m.prim_attrs, m.prims() + strips.len());
    let mut is_bevel: Vec<bool> = Vec::with_capacity(m.prims() + strips.len());
    for f in 0..m.prims() {
        job.every(f)?;
        ids.clear();
        if m.closed[f] {
            for c in first_corner[f]..first_corner[f + 1] {
                ids.push(chain[c][0]);
                if cut[c] {
                    // The strip's end between the two slid points, its arc
                    // moved along the strip into this polygon's plane.
                    let (p, q) = (chain[c][0], chain[c][1]);
                    let start = ids.len();
                    if let Some(r) = arcs.get(&(p, q)) {
                        ids.extend_from_slice(r);
                    } else if let Some(r) = arcs.get(&(q, p)) {
                        ids.extend(r.iter().rev());
                    }
                    let v = corners[c].v;
                    let e = along(v, partner[v as usize]);
                    let n = normal[f];
                    let across = v3::dot(n, e);
                    if across.abs() > 1e-9 {
                        for &id in &ids[start..] {
                            let x = v3::of(out.p[id as usize]);
                            let t = v3::dot(n, v3::sub(pos[v as usize], x)) / across;
                            out.p[id as usize] = v3::f32(v3::add(x, v3::scale(e, t)));
                        }
                    }
                }
                ids.extend_from_slice(&chain[c][1..]);
            }
        } else {
            ids.extend_from_slice(m.prim(f));
        }
        out.add_prim(&ids, m.closed[f]);
        prims.copy(f as u32);
        is_bevel.push(false);
    }
    for (strip, f) in &strips {
        out.add_polygon(strip);
        prims.copy(*f);
        is_bevel.push(true);
    }
    out.point_attrs = pts.finish();
    out.prim_attrs = prims.finish();
    // Corner patches: the new borders around bevelled points.
    let mut dir: HashSet<(u32, u32)> = HashSet::new();
    for f in 0..out.prims() {
        if out.closed[f] {
            let p = out.prim(f);
            for i in 0..p.len() {
                dir.insert((p[i], p[(i + 1) % p.len()]));
            }
        }
    }
    let fresh = |p: u32| p as usize >= np || touched[p as usize];
    // Pieces of the input's open border stay open: both points lie on
    // one border edge (as its ends, or slid along it).
    let mut on_border: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for (&(a, b), uses) in &edges.uses {
        if uses.len() == 1 {
            on_border.entry(a).or_default().push((a, b));
            on_border.entry(b).or_default().push((a, b));
        }
    }
    for (&(v, w), &p) in &slide {
        if edges.of(v, w).len() == 1 {
            on_border.entry(p).or_default().push(key(v, w));
        }
    }
    let on = |p: u32| on_border.get(&p).map_or(&[][..], Vec::as_slice);
    let border_piece = |a: u32, b: u32| on(a).iter().any(|e| on(b).contains(e));
    let mut holes: Vec<(u32, u32)> = dir
        .iter()
        .filter(|&&(a, b)| !dir.contains(&(b, a)) && (fresh(a) || fresh(b)) && !border_piece(a, b))
        .map(|&(a, b)| (b, a))
        .collect();
    holes.sort_unstable();
    // Around a border point the hole is open, from the border back to it:
    // it is closed along the border, unless it is just the end of one
    // strip (a strip reaching the border leaves its end open).
    let (open, holes) = paths(holes);
    let mut rings = loops(holes);
    for path in open {
        let (first, last) = (path[0], path[path.len() - 1]);
        if on(first).is_empty() || on(last).is_empty() {
            continue;
        }
        if path.len() >= 3 && !path[1..path.len() - 1].iter().all(|p| is_arc.contains(p)) {
            rings.push(path);
        }
    }
    let mut patch_src: Vec<u32> = Vec::new();
    let mut extra_points: Vec<(V, u32)> = Vec::new();
    let mut patches: Vec<Vec<u32>> = Vec::new();
    for ring in rings {
        let Some(v) = ring.iter().find(|&&p| fresh(p)).map(|&p| owner[p as usize]) else {
            continue;
        };
        let src = corners[around[v as usize][0]].f;
        // An inside point of four or more polygons that a polygon away from
        // the strips keeps (one strip ends there, or two or more pass with
        // a polygon between them unbevelled): the gap is a fan of
        // triangles from it (one for a single chamfer's end).
        if let Some(i) = ring.iter().position(|&p| {
            let p = p as usize;
            p < np && touched[p] && !border[p]
        }) {
            let k = ring.len();
            for j in 1..k - 1 {
                patches.push(vec![ring[i], ring[(i + j) % k], ring[(i + j + 1) % k]]);
                patch_src.push(src);
            }
            continue;
        }
        let ring_pos: Vec<V> = ring.iter().map(|&p| v3::of(out.p[p as usize])).collect();
        let mean = v3::scale(
            ring_pos.iter().fold([0.0; 3], |a, &b| v3::add(a, b)),
            1.0 / ring.len() as f64,
        );
        // A gap that is not flat (where four or more strips meet at a
        // point unevenly) is a fan from its centre instead of one polygon.
        if flat(&out, &ring, GAP_TOLERANCE) && (segments == 1 || ring.len() <= 4) {
            patches.push(ring);
            patch_src.push(src);
            continue;
        }
        let (sum, n) = centre_sum[v as usize];
        let centre = if n > 0 {
            let c = v3::scale(sum, 1.0 / f64::from(n));
            let r = ring_pos
                .iter()
                .map(|&p| v3::len(v3::sub(p, c)))
                .sum::<f64>()
                / ring.len() as f64;
            v3::add(c, v3::scale(v3::norm(v3::sub(pos[v as usize], c)), r))
        } else {
            mean
        };
        let id = (out.points() + extra_points.len()) as u32;
        extra_points.push((centre, v));
        for i in 0..ring.len() {
            patches.push(vec![id, ring[i], ring[(i + 1) % ring.len()]]);
            patch_src.push(src);
        }
    }
    if !extra_points.is_empty() {
        let mut more = Interp::new(&m.point_attrs, extra_points.len());
        for &(_, v) in &extra_points {
            more.copy(v);
        }
        for (col, add) in out.point_attrs.iter_mut().zip(more.finish()) {
            for i in 0..add.len() {
                col.push_copy(&add, i);
            }
        }
        for &(p, _) in &extra_points {
            out.add_point(v3::f32(p));
        }
    }
    let mut more = Interp::new(&m.prim_attrs, patches.len());
    for (ring, &src) in patches.iter().zip(&patch_src) {
        out.add_polygon(ring);
        more.copy(src);
        is_bevel.push(true);
    }
    for (col, add) in out.prim_attrs.iter_mut().zip(more.finish()) {
        for i in 0..add.len() {
            col.push_copy(&add, i);
        }
    }
    out.set_group(AttrClass::Prim, "bevel", &is_bevel)?;
    out.remove_unused_points();
    if m.point_attr(names::N).is_some() {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}

/// How far off its mean plane a strip quad may be, as a share of its size
/// (the largest distance of a point from the mean), before it is split
/// into two triangles. Rounded rims of capped tubes twist their quads by
/// 1.2–1.6e-3 at 32 columns (about 2e-4 absolute, invisible), and
/// chamfers ending in a shared slid point by under 3e-4: those stay
/// quads. Rounded strips ending in a shared slid point twist by 2–10e-3
/// and are split. Corner gaps are held to `GAP_TOLERANCE`.
pub const FLAT_TOLERANCE: f64 = 2e-3;

/// The same for a corner gap, filled as one polygon only when it is this
/// flat (uneven gaps are 2e-3 to 0.1 off and become fans).
const GAP_TOLERANCE: f64 = 1e-6;

/// Whether the polygon `ids` of `m` lies in one plane, to `tol` of its
/// size.
fn flat(m: &Mesh, ids: &[u32], tol: f64) -> bool {
    if ids.len() <= 3 {
        return true;
    }
    let pts: Vec<V> = ids.iter().map(|&p| v3::of(m.p[p as usize])).collect();
    let mean = v3::scale(
        pts.iter().fold([0.0; 3], |a, &b| v3::add(a, b)),
        1.0 / pts.len() as f64,
    );
    let n = v3::norm(newell(ids.iter().map(|&p| m.p[p as usize])));
    let size = pts
        .iter()
        .map(|&p| v3::len(v3::sub(p, mean)))
        .fold(0.0, f64::max);
    pts.iter()
        .all(|&p| v3::dot(n, v3::sub(p, mean)).abs() <= tol * size)
}

/// Splits sorted directed edges into the open paths that start at a point
/// no edge ends on, and the edges left over (for closed loops).
fn paths(edges: Vec<(u32, u32)>) -> (Vec<Vec<u32>>, Vec<(u32, u32)>) {
    let ends: HashSet<u32> = edges.iter().map(|e| e.1).collect();
    let mut used = vec![false; edges.len()];
    let from = |a: u32, used: &[bool]| -> Option<usize> {
        let i = edges.partition_point(|e| e.0 < a);
        (i..edges.len())
            .take_while(|&j| edges[j].0 == a)
            .find(|&j| !used[j])
    };
    let mut out = Vec::new();
    for s in 0..edges.len() {
        if used[s] || ends.contains(&edges[s].0) {
            continue;
        }
        let mut path = vec![edges[s].0];
        let mut j = s;
        loop {
            used[j] = true;
            path.push(edges[j].1);
            match from(edges[j].1, &used) {
                Some(n) if path.len() <= edges.len() => j = n,
                _ => break,
            }
        }
        out.push(path);
    }
    let rest = edges
        .iter()
        .zip(&used)
        .filter(|&(_, &u)| !u)
        .map(|(&e, _)| e)
        .collect();
    (out, rest)
}

/// The `segments - 1` inner points of an arc from `p` (on a plane with
/// normal `np`) to `q` (normal `nq`), tangent to both planes, and the
/// arc's centre (`None` when the planes are parallel: a straight line).
/// The points lie on the cylinder round the crease at equal angles, moving
/// along its axis as far as `q` is ahead of `p`, so the arcs at a strip's
/// two ends lie on the same lines along it.
fn arc_points(p: V, q: V, np: V, nq: V, segments: usize) -> (Vec<V>, Option<V>) {
    let dn = v3::sub(np, nq);
    let dd = v3::dot(dn, dn);
    let t = |i: usize| i as f64 / segments as f64;
    if dd < 1e-12 {
        return ((1..segments).map(|i| v3::lerp(p, q, t(i))).collect(), None);
    }
    let r = v3::dot(v3::sub(p, q), dn) / dd;
    let (op, oq) = (v3::sub(p, v3::scale(np, r)), v3::sub(q, v3::scale(nq, r)));
    let o = v3::scale(v3::add(op, oq), 0.5);
    // The axis points beside p and q.
    let skew = v3::sub(oq, op);
    let op = v3::sub(o, v3::scale(skew, 0.5));
    let oq = v3::add(o, v3::scale(skew, 0.5));
    let (dp, dq) = (v3::sub(p, op), v3::sub(q, oq));
    let (lp, lq) = (v3::len(dp), v3::len(dq));
    let (up, uq) = (v3::norm(dp), v3::norm(dq));
    let theta = v3::dot(up, uq).clamp(-1.0, 1.0).acos();
    if theta < 1e-9 || lp < 1e-30 || lq < 1e-30 {
        return ((1..segments).map(|i| v3::lerp(p, q, t(i))).collect(), None);
    }
    let points = (1..segments)
        .map(|i| {
            let s = t(i);
            let (wa, wb) = (
                ((1.0 - s) * theta).sin() / theta.sin(),
                (s * theta).sin() / theta.sin(),
            );
            let d = v3::add(v3::scale(up, wa), v3::scale(uq, wb));
            v3::add(
                v3::add(op, v3::scale(skew, s)),
                v3::scale(v3::norm(d), lp + (lq - lp) * s),
            )
        })
        .collect();
    (points, Some(o))
}
