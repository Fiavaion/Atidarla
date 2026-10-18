//! Scatter (seeded, area-weighted points on a surface) and Iso Surface
//! (marching tetrahedra over a scalar field: spheres around points, or an
//! analytic shape). The PNT↔MSH link: Scatter's points feed PNT operators
//! directly, and Iso Surface turns points back into a mesh.

use std::collections::HashMap;

use atd_core::geo::{AttrClass, names};
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::edit::triangulate_polygon;
use crate::mesh::{Column, Interp, Mesh, limits, v3};

/// A 64-bit hash of (seed, index, stream) (SplitMix64 finaliser).
pub fn hash(seed: u64, i: u64, stream: u64) -> u64 {
    let mut z = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(i.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(stream.wrapping_mul(0x94D0_49BB_1331_11EB));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform number in [0, 1) from [`hash`].
pub fn unit(seed: u64, i: u64, stream: u64) -> f64 {
    (hash(seed, i, stream) >> 11) as f64 / (1u64 << 53) as f64
}

pub fn scatter_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::int("count", "Count", 1000, Some(0), Some(limits::POINTS as i64))
            .help("Number of points."),
        ParamSchema::int("seed", "Seed", 1, Some(0), None)
            .help("Random seed: the same seed gives the same points."),
        ParamSchema::string("group", "Group", "")
            .help("Scatter only on this primitive group (empty: every polygon)."),
        ParamSchema::float("pscale", "Point Scale", 0.0, Some(0.0), None)
            .help("Writes `pscale` with this value when above 0 (Iso Surface multiplies its radius by it)."),
    ]
}

/// Points on the input's polygons, evenly by area. Writes `P`, `N` (the
/// surface normal: interpolated `N` when the input has it), `id`, and
/// interpolates every other float point attribute (for example `Cd`, `uv`).
pub fn scatter(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let Some(m) = inputs.first().and_then(Option::as_ref) else {
        return Ok(Mesh::new());
    };
    let count = s.i64("count").clamp(0, limits::POINTS as i64) as usize;
    let seed = s.i64("seed").max(0) as u64;
    let group = s.str("group");
    let members = if group.is_empty() {
        None
    } else {
        Some(
            m.group_members(AttrClass::Prim, group)
                .ok_or_else(|| format!("the input has no primitive group `{group}`"))?,
        )
    };
    // Triangles and their cumulative areas.
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for f in 0..m.prims() {
        job.every(f)?;
        if m.closed[f] && members.as_ref().is_none_or(|g| g[f]) {
            triangulate_polygon(&m.p, m.prim(f), &mut tris);
        }
    }
    let mut cum = Vec::with_capacity(tris.len());
    let mut total = 0.0f64;
    for t in &tris {
        let [a, b, c] = t.map(|i| v3::of(m.p[i as usize]));
        total += v3::len(v3::cross(v3::sub(b, a), v3::sub(c, a))) / 2.0;
        cum.push(total);
    }
    let mut out = Mesh::new();
    if total <= 0.0 || count == 0 {
        return Ok(out);
    }
    let smooth_n = m.vec3(names::N);
    let carried: Vec<Column> = m
        .point_attrs
        .iter()
        .filter(|c| c.floats().is_some() && c.name != names::N)
        .cloned()
        .collect();
    let mut interp = Interp::new(&carried, count);
    let mut normals = Vec::with_capacity(count);
    out.p.reserve(count);
    for i in 0..count {
        job.every(i)?;
        let i64_ = i as u64;
        let pick = unit(seed, i64_, 0) * total;
        let t = cum.partition_point(|&c| c <= pick).min(tris.len() - 1);
        let [ia, ib, ic] = tris[t];
        let r1 = unit(seed, i64_, 1).sqrt();
        let r2 = unit(seed, i64_, 2);
        let w = [1.0 - r1, r1 * (1.0 - r2), r1 * r2];
        let [a, b, c] = [ia, ib, ic].map(|k| v3::of(m.p[k as usize]));
        let p = v3::add(
            v3::add(v3::scale(a, w[0]), v3::scale(b, w[1])),
            v3::scale(c, w[2]),
        );
        out.p.push(v3::f32(p));
        let n = match &smooth_n {
            Some(nn) => {
                let [x, y, z] = [ia, ib, ic].map(|k| v3::of(nn[k as usize]));
                v3::norm(v3::add(
                    v3::add(v3::scale(x, w[0]), v3::scale(y, w[1])),
                    v3::scale(z, w[2]),
                ))
            }
            None => v3::norm(v3::cross(v3::sub(b, a), v3::sub(c, a))),
        };
        normals.push(v3::f32(n));
        interp.blend(&[(ia, w[0] as f32), (ib, w[1] as f32), (ic, w[2] as f32)]);
    }
    out.point_attrs = interp.finish();
    out.set_point_attr(Column::vec3(names::N, &normals));
    out.set_point_attr(Column::u32(names::ID, (0..count as u32).collect()));
    let ps = s.f32("pscale");
    if ps > 0.0 {
        out.set_point_attr(Column::f32(names::PSCALE, 1, vec![ps; count]));
    }
    Ok(out)
}

// ------------------------------------------------------------- iso surface --

pub const SHAPES: &[&str] = &["points", "sphere", "box", "torus", "gyroid"];

/// Most cells along an axis.
pub const MAX_RESOLUTION: i64 = 256;

pub fn iso_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("source", "Source", SHAPES, "points").help(
            "points: a sphere around every input point, blended together; the others: an analytic shape (no input needed).",
        ),
        ParamSchema::float("radius", "Radius", 0.1, Some(0.0), None).help(
            "points: each sphere's radius (times the point's `pscale`); sphere and torus: the radius.",
        ),
        ParamSchema::float("blend", "Blend", 0.05, Some(0.0), None)
            .help("points: how far neighbouring spheres melt into each other (a smooth union)."),
        ParamSchema::float("thickness", "Thickness", 0.15, Some(0.0), None)
            .help("torus: tube radius; gyroid: wall thickness."),
        ParamSchema::vec("size", "Size", &[2.0, 2.0, 2.0])
            .help("box: its extent; gyroid: the box it fills."),
        ParamSchema::float("frequency", "Frequency", 6.0, Some(0.0), None)
            .help("gyroid: cells per unit (times 2π)."),
        ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("Centre of an analytic shape."),
        ParamSchema::int("resolution", "Resolution", 64, Some(4), Some(MAX_RESOLUTION))
            .help("Cells along the longest side of the bounds (detail and cost grow with its cube)."),
        ParamSchema::float("offset", "Offset", 0.0, None, None)
            .help("Grows (positive) or shrinks (negative) the surface by this distance."),
    ]
}

/// Polynomial smooth minimum (Quílez).
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

fn sd_box(p: V3, half: V3) -> f64 {
    let q = [0, 1, 2].map(|k| p[k].abs() - half[k]);
    let outside = v3::len(q.map(|x| x.max(0.0)));
    outside + q[0].max(q[1]).max(q[2]).min(0.0)
}

type V3 = [f64; 3];

/// A mesh of the surface where the field crosses zero (negative inside):
/// marching tetrahedra over a regular grid.
pub fn iso_surface(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let source = s.str("source");
    let res = s.i64("resolution").clamp(4, MAX_RESOLUTION) as usize;
    let offset = s.f64("offset");
    let centre = v3::of(s.vec3("centre"));
    let r = s.f64("radius").max(0.0);
    let thick = s.f64("thickness").max(0.0);
    let size = v3::of(s.vec3("size")).map(f64::abs);
    // Bounds of the field.
    let (lo, hi, pts) = if source == "points" {
        let Some(m) = inputs
            .first()
            .and_then(Option::as_ref)
            .filter(|m| m.points() > 0)
        else {
            return Ok(Mesh::new());
        };
        let scale = m
            .point_attr(names::PSCALE)
            .and_then(|c| (c.comps == 1).then(|| c.floats()).flatten());
        let pts: Vec<(V3, f64)> =
            m.p.iter()
                .enumerate()
                .map(|(i, &p)| (v3::of(p), r * scale.map_or(1.0, |v| f64::from(v[i]))))
                .collect();
        let maxr = pts.iter().fold(0.0f64, |a, p| a.max(p.1));
        let (l, h) = m.bounds().expect("has points");
        let pad = maxr + s.f64("blend").max(0.0) + offset.max(0.0);
        (
            v3::sub(v3::of(l), [pad; 3]),
            v3::add(v3::of(h), [pad; 3]),
            pts,
        )
    } else {
        let half = match source {
            "sphere" => [r + offset.max(0.0); 3],
            "torus" => [r + thick, thick, r + thick].map(|x| x + offset.max(0.0)),
            _ => v3::scale(size, 0.5).map(|x| x + offset.max(0.0)),
        };
        (v3::sub(centre, half), v3::add(centre, half), Vec::new())
    };
    let ext = v3::sub(hi, lo);
    let longest = ext[0].max(ext[1]).max(ext[2]);
    if !(longest > 0.0 && longest.is_finite()) {
        return Ok(Mesh::new());
    }
    let h = longest / res as f64;
    // One empty cell of margin on every side keeps the surface closed.
    let lo = v3::sub(lo, [1.5 * h; 3]);
    let dims = ext.map(|e| ((e / h).ceil() as usize + 3).max(2));
    let [nx, ny, nz] = dims;
    let nodes = nx * ny * nz;
    if nodes > (MAX_RESOLUTION as usize + 4).pow(3) {
        return Err("the field grid is too large".into());
    }
    let at = |i: usize, j: usize, k: usize| {
        [
            lo[0] + i as f64 * h,
            lo[1] + j as f64 * h,
            lo[2] + k as f64 * h,
        ]
    };
    let idx = |i: usize, j: usize, k: usize| (k * ny + j) * nx + i;
    let mut field = vec![f32::MAX; nodes];
    let shift = offset as f32;
    if source == "points" {
        let k = s.f32("blend").max(0.0);
        for (n, &(p, pr)) in pts.iter().enumerate() {
            job.every(n)?;
            let reach = pr + f64::from(k) + offset.abs() + 2.0 * h;
            let lo_i = [0, 1, 2].map(|a| (((p[a] - reach - lo[a]) / h).floor().max(0.0)) as usize);
            let hi_i =
                [0, 1, 2].map(|a| (((p[a] + reach - lo[a]) / h).ceil() as usize).min(dims[a] - 1));
            for kk in lo_i[2]..=hi_i[2] {
                for jj in lo_i[1]..=hi_i[1] {
                    for ii in lo_i[0]..=hi_i[0] {
                        let d = (v3::len(v3::sub(at(ii, jj, kk), p)) - pr) as f32;
                        let f = &mut field[idx(ii, jj, kk)];
                        *f = if *f == f32::MAX { d } else { smin(*f, d, k) };
                    }
                }
            }
        }
        // Nodes no sphere reaches are far outside.
        let far = longest as f32;
        for f in &mut field {
            *f = if *f == f32::MAX { far } else { *f - shift };
        }
    } else {
        let freq = s.f64("frequency") * std::f64::consts::TAU;
        for kk in 0..nz {
            job.check()?;
            for jj in 0..ny {
                for ii in 0..nx {
                    let q = v3::sub(at(ii, jj, kk), centre);
                    let d = match source {
                        "sphere" => v3::len(q) - r,
                        "torus" => {
                            let a = (q[0] * q[0] + q[2] * q[2]).sqrt() - r;
                            (a * a + q[1] * q[1]).sqrt() - thick
                        }
                        "gyroid" => {
                            let g = (freq * q[0]).sin() * (freq * q[1]).cos()
                                + (freq * q[1]).sin() * (freq * q[2]).cos()
                                + (freq * q[2]).sin() * (freq * q[0]).cos();
                            let sheet = g.abs() / freq.max(1e-9) - thick * 0.5;
                            sheet.max(sd_box(q, v3::scale(size, 0.5)))
                        }
                        _ => sd_box(q, v3::scale(size, 0.5)),
                    };
                    field[idx(ii, jj, kk)] = d as f32 - shift;
                }
            }
        }
    }
    // Marching tetrahedra: each cell is cut into six tetrahedra around its
    // main diagonal (the same split in every cell, so neighbours agree),
    // and each tetrahedron gets a triangle or a quad. Vertices sit on grid
    // edges and are shared, so a closed field gives a closed manifold.
    const TETS: [[usize; 4]; 6] = [
        [0, 1, 3, 7],
        [0, 1, 5, 7],
        [0, 2, 3, 7],
        [0, 2, 6, 7],
        [0, 4, 5, 7],
        [0, 4, 6, 7],
    ];
    let inside = |v: f32| v < 0.0;
    let node = |n: usize| (n % nx, (n / nx) % ny, n / (nx * ny));
    let grad = |n: usize| {
        let (i, j, k) = node(n);
        let d =
            |a: usize, b: usize, span: usize| f64::from(field[b] - field[a]) / (span as f64 * h);
        let gx = d(
            idx(i.saturating_sub(1), j, k),
            idx((i + 1).min(nx - 1), j, k),
            (i + 1).min(nx - 1) - i.saturating_sub(1),
        );
        let gy = d(
            idx(i, j.saturating_sub(1), k),
            idx(i, (j + 1).min(ny - 1), k),
            (j + 1).min(ny - 1) - j.saturating_sub(1),
        );
        let gz = d(
            idx(i, j, k.saturating_sub(1)),
            idx(i, j, (k + 1).min(nz - 1)),
            (k + 1).min(nz - 1) - k.saturating_sub(1),
        );
        [gx, gy, gz]
    };
    let mut out = Mesh::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut on_edge: HashMap<(usize, usize), u32> = HashMap::new();
    let mut vertex = |a: usize, b: usize, out: &mut Mesh, normals: &mut Vec<[f32; 3]>| -> u32 {
        let key = (a.min(b), a.max(b));
        *on_edge.entry(key).or_insert_with(|| {
            let (va, vb) = (field[key.0], field[key.1]);
            let t = f64::from(va / (va - vb)).clamp(0.0, 1.0);
            let pos = |n: usize| {
                let (i, j, k) = node(n);
                at(i, j, k)
            };
            let p = v3::lerp(pos(key.0), pos(key.1), t);
            normals.push(v3::f32(v3::norm(v3::lerp(grad(key.0), grad(key.1), t))));
            out.add_point(v3::f32(p))
        })
    };
    let mut ids: Vec<u32> = Vec::with_capacity(4);
    for kk in 0..nz - 1 {
        job.check()?;
        for jj in 0..ny - 1 {
            for ii in 0..nx - 1 {
                let n: [usize; 8] =
                    std::array::from_fn(|c| idx(ii + (c & 1), jj + ((c >> 1) & 1), kk + (c >> 2)));
                let ins = n.map(|x| inside(field[x]));
                if ins.iter().all(|&x| x) || ins.iter().all(|&x| !x) {
                    continue;
                }
                for t in TETS {
                    let (inn, outs): (Vec<usize>, Vec<usize>) =
                        t.iter().map(|&c| n[c]).partition(|&x| inside(field[x]));
                    ids.clear();
                    match (inn.len(), outs.len()) {
                        (1, 3) => {
                            for &o in &outs {
                                ids.push(vertex(inn[0], o, &mut out, &mut normals));
                            }
                        }
                        (3, 1) => {
                            for &i in &inn {
                                ids.push(vertex(i, outs[0], &mut out, &mut normals));
                            }
                        }
                        (2, 2) => {
                            let (a, b, c, d) = (inn[0], inn[1], outs[0], outs[1]);
                            for (x, y) in [(a, c), (a, d), (b, d), (b, c)] {
                                ids.push(vertex(x, y, &mut out, &mut normals));
                            }
                        }
                        _ => continue,
                    }
                    ids.dedup();
                    if ids.len() > 1 && ids.first() == ids.last() {
                        ids.pop();
                    }
                    if ids.len() < 3 {
                        continue;
                    }
                    // Face from inside to outside.
                    let centre = |list: &[usize]| {
                        let mut c = [0.0; 3];
                        for &x in list {
                            let (i, j, k) = node(x);
                            c = v3::add(c, at(i, j, k));
                        }
                        v3::scale(c, 1.0 / list.len() as f64)
                    };
                    let dir = v3::sub(centre(&outs), centre(&inn));
                    let nrm = crate::mesh::newell(ids.iter().map(|&i| out.p[i as usize]));
                    if v3::dot(nrm, dir) < 0.0 {
                        ids.reverse();
                    }
                    out.add_polygon(&ids);
                }
            }
        }
        limits::check(
            out.points() as u64,
            out.prims() as u64,
            out.corners.len() as u64,
        )?;
    }
    out.set_point_attr(Column::vec3(names::N, &normals));
    Ok(out)
}
