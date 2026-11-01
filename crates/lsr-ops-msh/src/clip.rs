//! Clip: cuts geometry with a plane and keeps one side or both, optionally
//! filling the cut with cap polygons (holes in the cut, as in a clipped
//! torus, are bridged into their outline and triangulated).

use std::collections::{HashMap, HashSet};

use lsr_core::geo::{AttrClass, names};
use lsr_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::{merged, triangulate_polygon};
use crate::mesh::{Column, Interp, Mesh, limits, v3};
use crate::normals::point_normals;
use crate::topo::loops;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::vec("origin", "Origin", &[0.0, 0.0, 0.0]).help("A point on the plane."),
        ParamSchema::vec("normal", "Normal", &[0.0, 1.0, 0.0])
            .help("The plane's normal; it points to the side called above."),
        ParamSchema::menu("keep", "Keep", &["above", "below", "both"], "above").help(
            "above: keeps what is on the normal's side; below: the other side; both: keeps both halves as separate pieces, the groups `above` and `below`.",
        ),
        ParamSchema::toggle("cap", "Cap", false).help(
            "Fills the cut with polygons (the group `clip_cap`) so a closed input stays closed.",
        ),
    ]
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let n = v3::of(s.vec3("normal"));
    if v3::len(n) < 1e-12 {
        return Err("the plane's normal is zero".into());
    }
    let plane = Plane {
        origin: v3::of(s.vec3("origin")),
        normal: v3::norm(n),
    };
    let cap = s.bool("cap");
    match s.str("keep") {
        "below" => clipped(m, &plane.flipped(), cap, job),
        "both" => {
            let mut a = clipped(m, &plane, cap, job)?;
            let mut b = clipped(m, &plane.flipped(), cap, job)?;
            a.set_group(AttrClass::Prim, "above", &vec![true; a.prims()])?;
            b.set_group(AttrClass::Prim, "below", &vec![true; b.prims()])?;
            merged(&[&a, &b], job)
        }
        _ => clipped(m, &plane, cap, job),
    }
}

/// A plane; the kept side is where the normal points.
#[derive(Debug, Clone, Copy)]
pub struct Plane {
    pub origin: [f64; 3],
    pub normal: [f64; 3],
}

impl Plane {
    pub fn dist(&self, p: [f32; 3]) -> f64 {
        v3::dot(v3::sub(v3::of(p), self.origin), self.normal)
    }

    pub fn flipped(&self) -> Plane {
        Plane {
            origin: self.origin,
            normal: v3::scale(self.normal, -1.0),
        }
    }
}

/// The part of `m` on the normal's side of `plane`, welded along the cut.
pub fn clipped(m: &Mesh, plane: &Plane, cap: bool, job: &Job) -> Result<Mesh, String> {
    let had = m.point_attr(names::N).is_some();
    let scale = m.bounds().map_or(1.0, |(lo, hi)| {
        v3::len(v3::sub(v3::of(hi), v3::of(lo))).max(1.0)
    });
    let eps = 1e-6 * scale;
    let d: Vec<f64> = m.p.iter().map(|&p| plane.dist(p)).collect();
    let class: Vec<i8> = d
        .iter()
        .map(|&x| {
            if x > eps {
                1
            } else if x < -eps {
                -1
            } else {
                0
            }
        })
        .collect();
    let mut out = Mesh {
        p: m.p.clone(),
        point_groups: m.point_groups.clone(),
        prim_groups: m.prim_groups.clone(),
        ..Mesh::new()
    };
    let mut pts = Interp::new(&m.point_attrs, m.points());
    for i in 0..m.points() {
        pts.copy(i as u32);
    }
    // Points on the plane (old ones within eps, and every cut point).
    let mut on_plane: Vec<bool> = class.iter().map(|&c| c == 0).collect();
    let mut cuts: HashMap<(u32, u32), u32> = HashMap::new();
    let mut cut = |a: u32, b: u32, out: &mut Mesh, pts: &mut Interp, on: &mut Vec<bool>| -> u32 {
        let k = (a.min(b), a.max(b));
        *cuts.entry(k).or_insert_with(|| {
            let (a, b) = k;
            let (da, db) = (d[a as usize], d[b as usize]);
            let t = da / (da - db);
            let p = v3::lerp(v3::of(m.p[a as usize]), v3::of(m.p[b as usize]), t);
            pts.blend(&[(a, (1.0 - t) as f32), (b, t as f32)]);
            on.push(true);
            out.add_point(v3::f32(p))
        })
    };
    let mut prims = Interp::new(&m.prim_attrs, m.prims());
    let mut ids: Vec<u32> = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        let p = m.prim(f);
        let k = p.len();
        if p.iter().all(|&c| class[c as usize] >= 0) {
            // Entirely kept, except a polygon lying in the plane that
            // faces the kept side (it would be inside).
            let in_plane = p.iter().all(|&c| class[c as usize] == 0);
            if in_plane && m.closed[f] && v3::dot(m.prim_normal(f), plane.normal) > 0.0 {
                continue;
            }
            out.add_prim(p, m.closed[f]);
            prims.copy(f as u32);
            continue;
        }
        if m.closed[f] {
            ids.clear();
            for i in 0..k {
                let (a, b) = (p[i], p[(i + 1) % k]);
                let (ca, cb) = (class[a as usize], class[b as usize]);
                if ca >= 0 {
                    ids.push(a);
                }
                if ca * cb < 0 {
                    ids.push(cut(a, b, &mut out, &mut pts, &mut on_plane));
                }
            }
            if ids.len() >= 3 && p.iter().any(|&c| class[c as usize] > 0) {
                out.add_polygon(&ids);
                prims.copy(f as u32);
            }
        } else {
            // A polyline splits into its runs on the kept side.
            ids.clear();
            for i in 0..k {
                let a = p[i];
                let ca = class[a as usize];
                if i > 0 {
                    let b = p[i - 1];
                    if ca * class[b as usize] < 0 {
                        ids.push(cut(b, a, &mut out, &mut pts, &mut on_plane));
                    }
                }
                if ca >= 0 {
                    ids.push(a);
                } else {
                    if ids.len() >= 2 {
                        out.add_prim(&ids, false);
                        prims.copy(f as u32);
                    }
                    ids.clear();
                }
            }
            if ids.len() >= 2 {
                out.add_prim(&ids, false);
                prims.copy(f as u32);
            }
        }
    }
    limits::check(
        out.points() as u64,
        out.prims() as u64,
        out.corners.len() as u64,
    )?;
    out.point_attrs = pts.finish();
    out.prim_attrs = prims.finish();
    if cap {
        let before = out.prims();
        let cap_normal = v3::scale(plane.normal, -1.0);
        fill_plane_holes(&mut out, &on_plane, cap_normal, job)?;
        let added = out.prims() - before;
        for c in &mut out.prim_attrs {
            for _ in 0..added {
                c.push_default();
            }
        }
        let members: Vec<bool> = (0..out.prims()).map(|f| f >= before).collect();
        if added > 0 {
            out.set_group(AttrClass::Prim, "clip_cap", &members)?;
        }
    }
    if m.prims() > 0 {
        out.remove_unused_points();
    }
    if had {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}

/// Adds polygons closing the border loops whose points all lie on a plane
/// (`on_plane`), facing `normal`. A loop without holes becomes one
/// polygon; a loop with holes inside it is bridged and triangulated.
pub fn fill_plane_holes(
    m: &mut Mesh,
    on_plane: &[bool],
    normal: [f64; 3],
    job: &Job,
) -> Result<(), String> {
    let mut directed: HashSet<(u32, u32)> = HashSet::new();
    for f in 0..m.prims() {
        if !m.closed[f] {
            continue;
        }
        let p = m.prim(f);
        for i in 0..p.len() {
            directed.insert((p[i], p[(i + 1) % p.len()]));
        }
    }
    let mut border: Vec<(u32, u32)> = directed
        .iter()
        .filter(|&&(a, b)| {
            !directed.contains(&(b, a)) && on_plane[a as usize] && on_plane[b as usize]
        })
        .map(|&(a, b)| (b, a))
        .collect();
    border.sort_unstable();
    job.check()?;
    let rings = loops(border);
    // 2D coordinates in the plane, with u × v = normal.
    let helper = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = v3::norm(v3::cross(helper, normal));
    let v = v3::cross(normal, u);
    let coords: Vec<(f64, f64)> =
        m.p.iter()
            .map(|&p| (v3::dot(v3::of(p), u), v3::dot(v3::of(p), v)))
            .collect();
    let flat = |i: u32| coords[i as usize];
    let area = |ring: &[u32]| -> f64 {
        let k = ring.len();
        (0..k)
            .map(|i| {
                let (a, b) = (flat(ring[i]), flat(ring[(i + 1) % k]));
                a.0 * b.1 - b.0 * a.1
            })
            .sum::<f64>()
            / 2.0
    };
    let mut outers: Vec<(Vec<u32>, f64, Vec<Vec<u32>>)> = Vec::new();
    let mut holes: Vec<Vec<u32>> = Vec::new();
    for r in rings {
        let a = area(&r);
        if a > 0.0 {
            outers.push((r, a, Vec::new()));
        } else if a < 0.0 {
            holes.push(r);
        }
    }
    for h in holes {
        let q = flat(h[0]);
        let best = outers
            .iter()
            .enumerate()
            .filter(|(_, (o, _, _))| inside(&o.iter().map(|&i| flat(i)).collect::<Vec<_>>(), q))
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .map(|(i, _)| i);
        if let Some(i) = best {
            outers[i].2.push(h);
        }
    }
    let mut tris = Vec::new();
    let mut caps: Vec<Vec<u32>> = Vec::new();
    for (outer, _, mut hs) in outers {
        job.check()?;
        if hs.is_empty() {
            caps.push(outer);
            continue;
        }
        // Rightmost holes first (Eberly, "Triangulation by Ear Clipping").
        let right = |r: &[u32]| r.iter().map(|&i| flat(i).0).fold(f64::MIN, f64::max);
        hs.sort_by(|a, b| right(b).total_cmp(&right(a)));
        let mut poly = outer;
        for (k, hole) in hs.iter().enumerate() {
            let hi = (0..hole.len())
                .max_by(|&a, &b| flat(hole[a]).0.total_cmp(&flat(hole[b]).0))
                .unwrap_or(0);
            let hp = flat(hole[hi]);
            let mut order: Vec<usize> = (0..poly.len()).collect();
            let dist = |i: usize| {
                let p = flat(poly[i]);
                (p.0 - hp.0).powi(2) + (p.1 - hp.1).powi(2)
            };
            order.sort_by(|&a, &b| dist(a).total_cmp(&dist(b)));
            let blocked = |c: usize| {
                let cp = flat(poly[c]);
                let crosses = |ring: &[u32]| {
                    (0..ring.len()).any(|i| {
                        let (a, b) = (flat(ring[i]), flat(ring[(i + 1) % ring.len()]));
                        proper_cross(hp, cp, a, b)
                    })
                };
                crosses(&poly) || hs[k..].iter().any(|r| crosses(r))
            };
            let ci = order
                .iter()
                .copied()
                .find(|&c| !blocked(c))
                .unwrap_or(order[0]);
            let mut next = Vec::with_capacity(poly.len() + hole.len() + 2);
            next.extend_from_slice(&poly[..=ci]);
            next.extend_from_slice(&hole[hi..]);
            next.extend_from_slice(&hole[..=hi]);
            next.extend_from_slice(&poly[ci..]);
            poly = next;
        }
        tris.clear();
        triangulate_polygon(&m.p, &poly, &mut tris);
        caps.extend(tris.iter().map(|t| t.to_vec()));
    }
    for c in &caps {
        m.add_polygon(c);
    }
    Ok(())
}

/// Point in polygon (even-odd).
fn inside(poly: &[(f64, f64)], q: (f64, f64)) -> bool {
    let mut c = false;
    let k = poly.len();
    for i in 0..k {
        let (a, b) = (poly[i], poly[(i + 1) % k]);
        if (a.1 > q.1) != (b.1 > q.1) && q.0 < a.0 + (q.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

/// Segments `p`–`q` and `a`–`b` cross at a point inside both.
fn proper_cross(p: (f64, f64), q: (f64, f64), a: (f64, f64), b: (f64, f64)) -> bool {
    let o = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let (d1, d2) = (o(p, q, a), o(p, q, b));
    let (d3, d4) = (o(a, b, p), o(a, b, q));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}
