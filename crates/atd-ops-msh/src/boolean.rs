//! Boolean: union, intersection and difference of two closed meshes with
//! BSP trees (Thibault & Naylor, "Set operations on polyhedra using binary
//! space partitioning trees", SIGGRAPH 1987), in `f64` with a plane
//! tolerance. The trees live in an arena and are walked iteratively, so deep
//! trees cannot overflow the stack.
//!
//! Limits (stated in the operator's docs): inputs should be closed; the
//! result is welded by position but can keep T-junctions where splits meet,
//! so it is closed visually though not always by index; work grows roughly
//! with the square of the polygon count (inputs are capped).

use atd_core::geo::{AttrClass, names};
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::{fused, triangulated};
use crate::mesh::{Column, Mesh, Values, v3};

/// Most polygons (after triangulation) per input.
pub const MAX_POLYGONS: usize = 200_000;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu(
            "operation",
            "Operation",
            &["union", "intersect", "subtract"],
            "union",
        )
        .help(
            "union: the space inside A or B; intersect: inside both; subtract: inside A but not B.",
        ),
    ]
}

#[derive(Debug, Clone, Copy)]
struct Plane {
    n: [f64; 3],
    w: f64,
}

impl Plane {
    fn flip(&mut self) {
        self.n = v3::scale(self.n, -1.0);
        self.w = -self.w;
    }
}

#[derive(Debug, Clone)]
struct Vert {
    p: [f64; 3],
    /// Float point attributes, concatenated (see `Layout`).
    a: Vec<f32>,
}

#[derive(Debug, Clone)]
struct Poly {
    v: Vec<Vert>,
    plane: Plane,
    /// 0: from A, 1: from B.
    side: u8,
}

impl Poly {
    fn flip(&mut self) {
        self.v.reverse();
        self.plane.flip();
    }
}

const COPLANAR: u8 = 0;
const FRONT: u8 = 1;
const BACK: u8 = 2;
const SPANNING: u8 = 3;

/// Sorts `poly` relative to `plane`: coplanar pieces go to `same` or
/// `opposite` by facing, the rest to `front` and `back`, split if needed.
fn split(
    plane: Plane,
    eps: f64,
    poly: Poly,
    same: &mut Vec<Poly>,
    opposite: &mut Vec<Poly>,
    front: &mut Vec<Poly>,
    back: &mut Vec<Poly>,
) {
    let mut kinds = 0u8;
    let types: Vec<u8> = poly
        .v
        .iter()
        .map(|v| {
            let t = v3::dot(plane.n, v.p) - plane.w;
            let k = if t < -eps {
                BACK
            } else if t > eps {
                FRONT
            } else {
                COPLANAR
            };
            kinds |= k;
            k
        })
        .collect();
    match kinds {
        COPLANAR => {
            if v3::dot(plane.n, poly.plane.n) > 0.0 {
                same.push(poly);
            } else {
                opposite.push(poly);
            }
        }
        FRONT => front.push(poly),
        BACK => back.push(poly),
        _ => {
            let n = poly.v.len();
            let (mut f, mut b) = (Vec::with_capacity(n + 1), Vec::with_capacity(n + 1));
            for i in 0..n {
                let j = (i + 1) % n;
                let (ti, tj) = (types[i], types[j]);
                let (vi, vj) = (&poly.v[i], &poly.v[j]);
                if ti != BACK {
                    f.push(vi.clone());
                }
                if ti != FRONT {
                    b.push(vi.clone());
                }
                if (ti | tj) == SPANNING {
                    let d = v3::dot(plane.n, v3::sub(vj.p, vi.p));
                    let t = (plane.w - v3::dot(plane.n, vi.p)) / d;
                    let v = Vert {
                        p: v3::lerp(vi.p, vj.p, t),
                        a: vi
                            .a
                            .iter()
                            .zip(&vj.a)
                            .map(|(x, y)| x + (y - x) * t as f32)
                            .collect(),
                    };
                    f.push(v.clone());
                    b.push(v);
                }
            }
            if f.len() >= 3 {
                front.push(Poly {
                    v: f,
                    plane: poly.plane,
                    side: poly.side,
                });
            }
            if b.len() >= 3 {
                back.push(Poly {
                    v: b,
                    plane: poly.plane,
                    side: poly.side,
                });
            }
        }
    }
}

#[derive(Debug, Default)]
struct Node {
    plane: Option<Plane>,
    front: Option<usize>,
    back: Option<usize>,
    polys: Vec<Poly>,
}

#[derive(Debug)]
struct Tree {
    nodes: Vec<Node>,
    eps: f64,
}

impl Tree {
    fn new(polys: Vec<Poly>, eps: f64, job: &Job) -> Result<Self, String> {
        let mut t = Tree {
            nodes: vec![Node::default()],
            eps,
        };
        t.build(polys, job)?;
        Ok(t)
    }

    fn build(&mut self, polys: Vec<Poly>, job: &Job) -> Result<(), String> {
        let mut stack = vec![(0usize, polys)];
        let mut steps = 0;
        while let Some((ni, polys)) = stack.pop() {
            if polys.is_empty() {
                continue;
            }
            steps += 1;
            job.every(steps)?;
            let plane = *self.nodes[ni].plane.get_or_insert(polys[0].plane);
            let (mut same, mut opp, mut front, mut back) = (vec![], vec![], vec![], vec![]);
            for p in polys {
                split(
                    plane, self.eps, p, &mut same, &mut opp, &mut front, &mut back,
                );
            }
            self.nodes[ni].polys.extend(same);
            self.nodes[ni].polys.extend(opp);
            for (list, is_front) in [(front, true), (back, false)] {
                if list.is_empty() {
                    continue;
                }
                let slot = if is_front {
                    self.nodes[ni].front
                } else {
                    self.nodes[ni].back
                };
                let child = match slot {
                    Some(c) => c,
                    None => {
                        self.nodes.push(Node::default());
                        let c = self.nodes.len() - 1;
                        if is_front {
                            self.nodes[ni].front = Some(c);
                        } else {
                            self.nodes[ni].back = Some(c);
                        }
                        c
                    }
                };
                stack.push((child, list));
            }
        }
        Ok(())
    }

    /// The parts of `polys` outside this solid (in front of its surface).
    fn clip(&self, polys: Vec<Poly>, job: &Job) -> Result<Vec<Poly>, String> {
        let mut out = Vec::new();
        let mut stack = vec![(0usize, polys)];
        let mut steps = 0;
        while let Some((ni, polys)) = stack.pop() {
            steps += 1;
            job.every(steps)?;
            let node = &self.nodes[ni];
            let Some(plane) = node.plane else {
                out.extend(polys);
                continue;
            };
            let (mut front, mut back) = (Vec::new(), Vec::new());
            for p in polys {
                // Coplanar pieces facing the same way count as in front.
                let (mut same, mut opp) = (Vec::new(), Vec::new());
                split(
                    plane, self.eps, p, &mut same, &mut opp, &mut front, &mut back,
                );
                front.extend(same);
                back.extend(opp);
            }
            match node.front {
                Some(f) => stack.push((f, front)),
                None => out.extend(front),
            }
            if let Some(b) = node.back {
                stack.push((b, back));
            }
        }
        Ok(out)
    }

    /// Removes the parts of this tree's polygons inside `other`.
    fn clip_to(&mut self, other: &Tree, job: &Job) -> Result<(), String> {
        for i in 0..self.nodes.len() {
            let polys = std::mem::take(&mut self.nodes[i].polys);
            self.nodes[i].polys = other.clip(polys, job)?;
        }
        Ok(())
    }

    fn invert(&mut self) {
        for n in &mut self.nodes {
            for p in &mut n.polys {
                p.flip();
            }
            if let Some(pl) = &mut n.plane {
                pl.flip();
            }
            std::mem::swap(&mut n.front, &mut n.back);
        }
    }

    fn all(&self) -> Vec<Poly> {
        self.nodes
            .iter()
            .flat_map(|n| n.polys.iter().cloned())
            .collect()
    }
}

/// Float point columns carried through the operation (by name; `N` is
/// dropped: it would be wrong on cut faces).
struct Layout {
    cols: Vec<(String, u8)>,
}

impl Layout {
    fn new(meshes: &[&Mesh]) -> Self {
        let mut cols: Vec<(String, u8)> = Vec::new();
        for m in meshes {
            for c in &m.point_attrs {
                if c.floats().is_some()
                    && c.name != names::N
                    && cols.iter().all(|(n, _)| *n != c.name)
                {
                    cols.push((c.name.clone(), c.comps));
                }
            }
        }
        Self { cols }
    }

    fn vert(&self, m: &Mesh, i: usize) -> Vert {
        let mut a = Vec::new();
        for (name, comps) in &self.cols {
            let k = usize::from(*comps);
            match m
                .point_attr(name)
                .filter(|c| c.comps == *comps)
                .and_then(Column::floats)
            {
                Some(v) => a.extend_from_slice(&v[i * k..(i + 1) * k]),
                None => {
                    let one = name == names::CD || name == names::PSCALE;
                    a.extend(std::iter::repeat_n(if one { 1.0 } else { 0.0 }, k));
                }
            }
        }
        Vert {
            p: v3::of(m.p[i]),
            a,
        }
    }
}

fn polygons(m: &Mesh, layout: &Layout, side: u8, job: &Job) -> Result<Vec<Poly>, String> {
    let t = triangulated(m, job)?;
    let tris = (0..t.prims()).filter(|&f| t.closed[f]).count();
    if tris > MAX_POLYGONS {
        return Err(format!(
            "an input has {tris} triangles; Boolean takes at most {MAX_POLYGONS} (reduce it first)"
        ));
    }
    let mut out = Vec::with_capacity(tris);
    for f in 0..t.prims() {
        job.every(f)?;
        if !t.closed[f] {
            continue;
        }
        let v: Vec<Vert> = t
            .prim(f)
            .iter()
            .map(|&i| layout.vert(&t, i as usize))
            .collect();
        let n = v3::cross(v3::sub(v[1].p, v[0].p), v3::sub(v[2].p, v[0].p));
        if v3::len(n) <= 1e-300 {
            continue;
        }
        let n = v3::norm(n);
        let plane = Plane {
            n,
            w: v3::dot(n, v[0].p),
        };
        out.push(Poly { v, plane, side });
    }
    Ok(out)
}

pub fn run(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let get = |i: usize| inputs.get(i).and_then(Option::as_ref);
    let op = s.str("operation");
    let (a, b) = match (get(0), get(1)) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => return Ok(a.clone()),
        (None, Some(b)) if op == "union" => return Ok(b.clone()),
        _ => return Ok(Mesh::new()),
    };
    let layout = Layout::new(&[a, b]);
    let diag = match (a.bounds(), b.bounds()) {
        (Some((l0, h0)), Some((l1, h1))) => {
            let lo = [0, 1, 2].map(|k| f64::from(l0[k].min(l1[k])));
            let hi = [0, 1, 2].map(|k| f64::from(h0[k].max(h1[k])));
            v3::len(v3::sub(hi, lo))
        }
        _ => 1.0,
    };
    let eps = 1e-7 * diag.max(1e-3);
    let mut ta = Tree::new(polygons(a, &layout, 0, job)?, eps, job)?;
    let mut tb = Tree::new(polygons(b, &layout, 1, job)?, eps, job)?;
    match op {
        "intersect" => {
            ta.invert();
            tb.clip_to(&ta, job)?;
            tb.invert();
            ta.clip_to(&tb, job)?;
            tb.clip_to(&ta, job)?;
            ta.build(tb.all(), job)?;
            ta.invert();
        }
        "subtract" => {
            ta.invert();
            ta.clip_to(&tb, job)?;
            tb.clip_to(&ta, job)?;
            tb.invert();
            tb.clip_to(&ta, job)?;
            tb.invert();
            ta.build(tb.all(), job)?;
            ta.invert();
        }
        _ => {
            ta.clip_to(&tb, job)?;
            tb.clip_to(&ta, job)?;
            tb.invert();
            tb.clip_to(&ta, job)?;
            tb.invert();
            ta.build(tb.all(), job)?;
        }
    }
    let polys = ta.all();
    // A polygon soup, then welded by position.
    let mut soup = Mesh::new();
    let mut cols: Vec<Column> = layout
        .cols
        .iter()
        .map(|(n, c)| Column::f32(n, *c, Vec::new()))
        .collect();
    let mut side = Vec::with_capacity(polys.len());
    let mut ids = Vec::new();
    for (k, p) in polys.iter().enumerate() {
        job.every(k)?;
        ids.clear();
        for v in &p.v {
            ids.push(soup.add_point(v3::f32(v.p)));
            let mut at = 0;
            for c in &mut cols {
                let n = usize::from(c.comps);
                if let Values::F32(vals) = &mut c.values {
                    vals.extend_from_slice(&v.a[at..at + n]);
                }
                at += n;
            }
        }
        soup.add_polygon(&ids);
        side.push(p.side);
    }
    soup.point_attrs = cols;
    soup.set_group(
        AttrClass::Prim,
        "from_a",
        &side.iter().map(|&s| s == 0).collect::<Vec<_>>(),
    )?;
    soup.set_group(
        AttrClass::Prim,
        "from_b",
        &side.iter().map(|&s| s == 1).collect::<Vec<_>>(),
    )?;
    crate::mesh::limits::check(
        soup.points() as u64,
        soup.prims() as u64,
        soup.corners.len() as u64,
    )?;
    fused(&soup, eps * 10.0, job)
}
