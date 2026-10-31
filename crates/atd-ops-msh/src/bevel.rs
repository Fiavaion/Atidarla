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
//! filled (one polygon, or a fan from a rounded centre).

use std::collections::{HashMap, HashSet};

use atd_core::geo::{AttrClass, names};
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::mesh::{Column, Interp, Mesh, limits, v3};
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
    // Points whose polygons do not form one closed, consistently wound
    // fan are left alone (as are border points).
    let mut around: Vec<Vec<usize>> = vec![Vec::new(); np];
    for (c, k) in corners.iter().enumerate() {
        around[k.v as usize].push(c);
    }
    for v in 0..np {
        job.every(v)?;
        if border[v] {
            bad[v] = true;
        }
        if bad[v] || around[v].is_empty() {
            continue;
        }
        let start = around[v][0];
        let mut c = start;
        let mut steps = 0;
        loop {
            // Across the incoming edge u → v lies the polygon with v → u.
            let k = corners[c];
            match directed.get(&(k.v, k.u)) {
                Some(&next) => c = next,
                None => {
                    bad[v] = true;
                    break;
                }
            }
            steps += 1;
            if c == start || steps > around[v].len() {
                break;
            }
        }
        if steps != around[v].len() {
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
    for &(a, b) in &selected {
        touched[a as usize] = true;
        touched[b as usize] = true;
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
    // Each corner's chain of points in its polygon.
    let mut chain: Vec<Vec<u32>> = Vec::with_capacity(corners.len());
    for k in &corners {
        let v = k.v;
        if !touched[v as usize] {
            chain.push(vec![v]);
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
    let mut prims = Interp::new(&m.prim_attrs, m.prims() + selected_count * segments);
    let mut is_bevel: Vec<bool> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        ids.clear();
        if m.closed[f] {
            for c in first_corner[f]..first_corner[f + 1] {
                ids.extend_from_slice(&chain[c]);
            }
        } else {
            ids.extend_from_slice(m.prim(f));
        }
        out.add_prim(&ids, m.closed[f]);
        prims.copy(f as u32);
        is_bevel.push(false);
    }
    // Strips, with arcs shared by strips that end on the same two points.
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
                                out.add_point(v3::f32(x))
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
            if ids.len() >= 3 {
                out.add_polygon(&ids);
                prims.copy(f);
                is_bevel.push(true);
            }
        }
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
    let holes: Vec<(u32, u32)> = dir
        .iter()
        .filter(|&&(a, b)| !dir.contains(&(b, a)) && (fresh(a) || fresh(b)))
        .map(|&(a, b)| (b, a))
        .collect();
    let mut patch_src: Vec<u32> = Vec::new();
    let mut extra_points: Vec<(V, u32)> = Vec::new();
    let mut patches: Vec<Vec<u32>> = Vec::new();
    for ring in loops(holes) {
        let Some(v) = ring.iter().find(|&&p| fresh(p)).map(|&p| owner[p as usize]) else {
            continue;
        };
        let src = corners[around[v as usize][0]].f;
        if segments == 1 || ring.len() <= 4 {
            patches.push(ring);
            patch_src.push(src);
            continue;
        }
        let ring_pos: Vec<V> = ring.iter().map(|&p| v3::of(out.p[p as usize])).collect();
        let mean = v3::scale(
            ring_pos.iter().fold([0.0; 3], |a, &b| v3::add(a, b)),
            1.0 / ring.len() as f64,
        );
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

/// The `segments - 1` inner points of an arc from `p` (on a plane with
/// normal `np`) to `q` (normal `nq`), tangent to both planes, and the
/// arc's centre (`None` when the planes are parallel: a straight line).
fn arc_points(p: V, q: V, np: V, nq: V, segments: usize) -> (Vec<V>, Option<V>) {
    let dn = v3::sub(np, nq);
    let dd = v3::dot(dn, dn);
    let t = |i: usize| i as f64 / segments as f64;
    if dd < 1e-12 {
        return ((1..segments).map(|i| v3::lerp(p, q, t(i))).collect(), None);
    }
    let r = v3::dot(v3::sub(p, q), dn) / dd;
    let o = v3::scale(
        v3::add(v3::sub(p, v3::scale(np, r)), v3::sub(q, v3::scale(nq, r))),
        0.5,
    );
    let (dp, dq) = (v3::sub(p, o), v3::sub(q, o));
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
            v3::add(o, v3::scale(v3::norm(d), lp + (lq - lp) * s))
        })
        .collect();
    (points, Some(o))
}
