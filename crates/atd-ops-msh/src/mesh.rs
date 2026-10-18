//! The CPU mesh MSH operators work on (design `docs/design/msh-operators.md`
//! M1): indexed polygons and polylines with typed point and primitive
//! columns, converted to and from the GEO value by copying.

use std::sync::Arc;

use atd_core::geo::{AttrClass, AttrType, Scalar, names};
use atd_gpu::geo::{Attribute, BufferPool, Geometry, Topology};

/// Size limits of one mesh (design M9): operators check them before
/// allocating.
pub mod limits {
    /// Most points (P alone is 128 MiB at 16 bytes per point).
    pub const POINTS: usize = 8_000_000;
    /// Most primitives.
    pub const PRIMS: usize = 16_000_000;
    /// Most corners (polygon and polyline vertices).
    pub const CORNERS: usize = 32_000_000;

    /// An error unless a mesh of this size fits.
    pub fn check(points: u64, prims: u64, corners: u64) -> Result<(), String> {
        if points > POINTS as u64 {
            return Err(format!(
                "the result would have {points} points; the limit is {POINTS}"
            ));
        }
        if prims > PRIMS as u64 {
            return Err(format!(
                "the result would have {prims} primitives; the limit is {PRIMS}"
            ));
        }
        if corners > CORNERS as u64 {
            return Err(format!(
                "the result would have {corners} corners; the limit is {CORNERS}"
            ));
        }
        Ok(())
    }
}

/// Values of a column, tightly packed (`len × comps`).
#[derive(Debug, Clone, PartialEq)]
pub enum Values {
    F32(Vec<f32>),
    I32(Vec<i32>),
    U32(Vec<u32>),
}

impl Values {
    fn len(&self) -> usize {
        match self {
            Values::F32(v) => v.len(),
            Values::I32(v) => v.len(),
            Values::U32(v) => v.len(),
        }
    }

    fn empty_like(&self, capacity: usize) -> Values {
        match self {
            Values::F32(_) => Values::F32(Vec::with_capacity(capacity)),
            Values::I32(_) => Values::I32(Vec::with_capacity(capacity)),
            Values::U32(_) => Values::U32(Vec::with_capacity(capacity)),
        }
    }

    fn scalar(&self) -> Scalar {
        match self {
            Values::F32(_) => Scalar::F32,
            Values::I32(_) => Scalar::I32,
            Values::U32(_) => Scalar::U32,
        }
    }
}

/// A named attribute column: `comps` (1 to 4) values per element.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub comps: u8,
    pub values: Values,
}

impl Column {
    pub fn f32(name: &str, comps: u8, values: Vec<f32>) -> Self {
        Self {
            name: name.into(),
            comps,
            values: Values::F32(values),
        }
    }

    pub fn u32(name: &str, values: Vec<u32>) -> Self {
        Self {
            name: name.into(),
            comps: 1,
            values: Values::U32(values),
        }
    }

    pub fn vec3(name: &str, values: &[[f32; 3]]) -> Self {
        Self::f32(name, 3, values.iter().flatten().copied().collect())
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.values.len() / usize::from(self.comps.max(1))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn ty(&self) -> AttrType {
        AttrType::new(self.values.scalar(), self.comps.clamp(1, 4))
    }

    /// Float values, if this is a float column.
    pub fn floats(&self) -> Option<&[f32]> {
        match &self.values {
            Values::F32(v) => Some(v),
            _ => None,
        }
    }

    pub fn floats_mut(&mut self) -> Option<&mut Vec<f32>> {
        match &mut self.values {
            Values::F32(v) => Some(v),
            _ => None,
        }
    }

    /// Element `i` as a vec3 (float columns with 3 components).
    pub fn get3(&self, i: usize) -> [f32; 3] {
        match &self.values {
            Values::F32(v) if self.comps == 3 => [v[i * 3], v[i * 3 + 1], v[i * 3 + 2]],
            _ => [0.0; 3],
        }
    }

    /// A column like this one with no elements.
    pub fn empty_like(&self, capacity: usize) -> Column {
        Column {
            name: self.name.clone(),
            comps: self.comps,
            values: self.values.empty_like(capacity * usize::from(self.comps)),
        }
    }

    /// Appends element `i` of `src` (same type).
    pub fn push_copy(&mut self, src: &Column, i: usize) {
        let c = usize::from(self.comps);
        let r = i * c..(i + 1) * c;
        match (&mut self.values, &src.values) {
            (Values::F32(d), Values::F32(s)) => d.extend_from_slice(&s[r]),
            (Values::I32(d), Values::I32(s)) => d.extend_from_slice(&s[r]),
            (Values::U32(d), Values::U32(s)) => d.extend_from_slice(&s[r]),
            _ => unreachable!("columns of one type"),
        }
    }

    /// Appends a weighted blend of elements of `src`: floats are summed by
    /// weight; integers take the element with the largest weight.
    pub fn push_blend(&mut self, src: &Column, weights: &[(u32, f32)]) {
        let c = usize::from(self.comps);
        match (&mut self.values, &src.values) {
            (Values::F32(d), Values::F32(s)) => {
                let at = d.len();
                d.resize(at + c, 0.0);
                for &(i, w) in weights {
                    let i = i as usize;
                    for k in 0..c {
                        d[at + k] += s[i * c + k] * w;
                    }
                }
            }
            _ => {
                let best = weights
                    .iter()
                    .copied()
                    .reduce(|a, b| if b.1 > a.1 { b } else { a })
                    .map_or(0, |(i, _)| i as usize);
                self.push_copy(src, best);
            }
        }
    }

    /// Appends one element of zeros (for floats named `Cd` or `pscale`, ones:
    /// a missing colour is white).
    pub fn push_default(&mut self) {
        let c = usize::from(self.comps);
        let one = self.name == names::CD || self.name == names::PSCALE;
        match &mut self.values {
            Values::F32(d) => d.extend(std::iter::repeat_n(if one { 1.0 } else { 0.0 }, c)),
            Values::I32(d) => d.extend(std::iter::repeat_n(0, c)),
            Values::U32(d) => d.extend(std::iter::repeat_n(0, c)),
        }
    }
}

/// Builds new columns from source columns element by element (attribute
/// propagation for operators that change topology).
#[derive(Debug)]
pub struct Interp<'a> {
    src: &'a [Column],
    out: Vec<Column>,
}

impl<'a> Interp<'a> {
    pub fn new(src: &'a [Column], capacity: usize) -> Self {
        Self {
            src,
            out: src.iter().map(|c| c.empty_like(capacity)).collect(),
        }
    }

    /// A new element copying source element `i`.
    pub fn copy(&mut self, i: u32) {
        for (o, s) in self.out.iter_mut().zip(self.src) {
            o.push_copy(s, i as usize);
        }
    }

    /// A new element blending source elements by weight.
    pub fn blend(&mut self, weights: &[(u32, f32)]) {
        for (o, s) in self.out.iter_mut().zip(self.src) {
            o.push_blend(s, weights);
        }
    }

    pub fn finish(self) -> Vec<Column> {
        self.out
    }
}

/// A CPU mesh (see the module documentation).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub p: Vec<[f32; 3]>,
    pub point_attrs: Vec<Column>,
    /// Primitive `i` uses `corners[start[i]..start[i + 1]]`; `start[0] = 0`.
    pub start: Vec<u32>,
    pub corners: Vec<u32>,
    /// Per primitive: a closed polygon (`true`) or an open polyline.
    pub closed: Vec<bool>,
    pub prim_attrs: Vec<Column>,
    pub point_groups: Vec<String>,
    pub prim_groups: Vec<String>,
}

impl Mesh {
    pub fn new() -> Self {
        Self {
            start: vec![0],
            ..Default::default()
        }
    }

    pub fn points(&self) -> usize {
        self.p.len()
    }

    pub fn prims(&self) -> usize {
        self.closed.len()
    }

    /// The points of primitive `i`.
    pub fn prim(&self, i: usize) -> &[u32] {
        &self.corners[self.start[i] as usize..self.start[i + 1] as usize]
    }

    /// Adds a primitive; returns its index.
    pub fn add_prim(&mut self, points: &[u32], closed: bool) -> usize {
        if self.start.is_empty() {
            self.start.push(0);
        }
        self.corners.extend_from_slice(points);
        self.start.push(self.corners.len() as u32);
        self.closed.push(closed);
        self.closed.len() - 1
    }

    pub fn add_polygon(&mut self, points: &[u32]) -> usize {
        self.add_prim(points, true)
    }

    pub fn add_point(&mut self, p: [f32; 3]) -> u32 {
        self.p.push(p);
        (self.p.len() - 1) as u32
    }

    pub fn point_attr(&self, name: &str) -> Option<&Column> {
        self.point_attrs.iter().find(|c| c.name == name)
    }

    pub fn prim_attr(&self, name: &str) -> Option<&Column> {
        self.prim_attrs.iter().find(|c| c.name == name)
    }

    pub fn point_attr_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.point_attrs.iter_mut().find(|c| c.name == name)
    }

    /// Adds or replaces a point column.
    pub fn set_point_attr(&mut self, col: Column) {
        set(&mut self.point_attrs, col);
    }

    pub fn set_prim_attr(&mut self, col: Column) {
        set(&mut self.prim_attrs, col);
    }

    /// A point vec3 column's values, if it exists with 3 float components.
    pub fn vec3(&self, name: &str) -> Option<Vec<[f32; 3]>> {
        let c = self.point_attr(name).filter(|c| c.comps == 3)?;
        let v = c.floats()?;
        Some(v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
    }

    /// Axis-aligned bounds of the points (`None` without points).
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.p.first()?;
        let mut lo = first;
        let mut hi = first;
        for p in &self.p {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Some((lo, hi))
    }

    /// Bit of a point group (adding the name), or `None` past 32 groups.
    pub fn point_group_bit(&mut self, name: &str) -> Option<u32> {
        group_bit(&mut self.point_groups, name)
    }

    pub fn prim_group_bit(&mut self, name: &str) -> Option<u32> {
        group_bit(&mut self.prim_groups, name)
    }

    /// Membership mask of `class`'s group `name` per element (`None` when
    /// there is no such group).
    pub fn group_members(&self, class: AttrClass, name: &str) -> Option<Vec<bool>> {
        let (groups, attrs, n) = match class {
            AttrClass::Point => (&self.point_groups, &self.point_attrs, self.points()),
            AttrClass::Prim => (&self.prim_groups, &self.prim_attrs, self.prims()),
            AttrClass::Detail => return None,
        };
        let bit = groups.iter().position(|g| g == name)?;
        let mask = attrs.iter().find(|c| c.name == names::GROUP);
        Some(match mask.map(|c| &c.values) {
            Some(Values::U32(v)) if v.len() == n => v.iter().map(|m| m & (1 << bit) != 0).collect(),
            _ => vec![false; n],
        })
    }

    /// Sets group `name` of `class` to `members` (one flag per element).
    pub fn set_group(
        &mut self,
        class: AttrClass,
        name: &str,
        members: &[bool],
    ) -> Result<(), String> {
        let (bit, attrs, n) = match class {
            AttrClass::Point => (
                group_bit(&mut self.point_groups, name),
                &mut self.point_attrs,
                self.p.len(),
            ),
            AttrClass::Prim => (
                group_bit(&mut self.prim_groups, name),
                &mut self.prim_attrs,
                self.closed.len(),
            ),
            AttrClass::Detail => return Err("detail values have no groups".into()),
        };
        let bit = bit.ok_or("at most 32 groups per class")?;
        if attrs.iter().all(|c| c.name != names::GROUP) {
            attrs.push(Column::u32(names::GROUP, vec![0; n]));
        }
        let col = attrs
            .iter_mut()
            .find(|c| c.name == names::GROUP)
            .expect("added above");
        let Values::U32(v) = &mut col.values else {
            return Err("the `group` attribute is not u32".into());
        };
        v.resize(n, 0);
        for (m, &on) in v.iter_mut().zip(members) {
            if on {
                *m |= 1 << bit;
            } else {
                *m &= !(1 << bit);
            }
        }
        Ok(())
    }

    /// Checks the invariants (lengths, offsets, indices); file readers and
    /// GEO conversion call it so later operators can index freely.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.p.len();
        limits::check(
            n as u64,
            self.closed.len() as u64,
            self.corners.len() as u64,
        )?;
        if self.start.len() != self.closed.len() + 1 || self.start.first() != Some(&0) {
            return Err("primitive offsets do not match the primitive count".into());
        }
        if self.start.windows(2).any(|w| w[0] > w[1])
            || *self.start.last().expect("non-empty") as usize != self.corners.len()
        {
            return Err("primitive offsets are not increasing to the corner count".into());
        }
        if let Some(c) = self.corners.iter().find(|&&c| c as usize >= n) {
            return Err(format!("a primitive uses point {c} of {n}"));
        }
        for (cols, len, class) in [
            (&self.point_attrs, n, "point"),
            (&self.prim_attrs, self.closed.len(), "primitive"),
        ] {
            for c in cols {
                if !(1..=4).contains(&c.comps) || c.values.len() != len * usize::from(c.comps) {
                    return Err(format!(
                        "{class} attribute `{}` does not have {len} elements",
                        c.name
                    ));
                }
            }
        }
        if self.point_groups.len() > 32 || self.prim_groups.len() > 32 {
            return Err("at most 32 groups per class".into());
        }
        Ok(())
    }

    /// Removes points no primitive uses (when the mesh has primitives),
    /// keeping order; returns the new index of each old point.
    pub fn remove_unused_points(&mut self) -> Vec<Option<u32>> {
        let mut used = vec![false; self.p.len()];
        for &c in &self.corners {
            used[c as usize] = true;
        }
        self.keep_points(&used)
    }

    /// Keeps the points flagged in `keep` (primitives must use only kept
    /// points); returns the new index of each old point.
    pub fn keep_points(&mut self, keep: &[bool]) -> Vec<Option<u32>> {
        let mut map = vec![None; self.p.len()];
        let mut next = 0u32;
        let mut interp = Interp::new(&self.point_attrs, self.p.len());
        let mut p = Vec::with_capacity(self.p.len());
        for (i, &k) in keep.iter().enumerate() {
            if k {
                map[i] = Some(next);
                next += 1;
                p.push(self.p[i]);
                interp.copy(i as u32);
            }
        }
        let cols = interp.finish();
        self.p = p;
        self.point_attrs = cols;
        for c in &mut self.corners {
            *c = map[*c as usize].expect("primitives use only kept points");
        }
        map
    }

    /// Converts to a GEO value. With `upload`, point attributes are put on
    /// the GPU as well (from this thread) and a header is written, so GPU
    /// operators read the result without copying on the render thread.
    pub fn to_geometry(&self, upload: Option<(&wgpu::Queue, &BufferPool)>) -> Geometry {
        let mut g = Geometry::new();
        let n = self.p.len() as u32;
        let header = match upload {
            Some((queue, pool)) if n > 0 => Some(atd_gpu::geo::cpu_header(queue, pool, n)),
            _ => None,
        };
        g.set_points(n, header);
        if n > 0 {
            g.set_point(Attribute::from_vec3(names::P, &self.p));
            for c in &self.point_attrs {
                g.set_point(column_attribute(c));
            }
        }
        if !self.closed.is_empty() {
            let kinds: Vec<u32> = self.closed.iter().map(|&c| u32::from(!c)).collect();
            g.set_topology(Some(Topology {
                kinds: Attribute::from_u32("__kind", &kinds),
                start: Attribute::from_u32("__start", &self.start),
                corners: Attribute::from_u32("__corners", &self.corners),
            }));
            for c in &self.prim_attrs {
                let _ = g.set(AttrClass::Prim, column_attribute(c));
            }
        }
        for name in &self.point_groups {
            g.add_group(AttrClass::Point, name);
        }
        for name in &self.prim_groups {
            g.add_group(AttrClass::Prim, name);
        }
        if let Some((queue, pool)) = upload {
            for a in g.attrs(AttrClass::Point) {
                a.ensure_gpu(queue, pool);
            }
        }
        g
    }

    /// Converts a GEO value whose attributes all have CPU copies (see
    /// `cook::Inputs`). `count` overrides the point count (a GPU-counted
    /// input's live count). Attributes that are not 32-bit or whose names
    /// start with `__` are skipped.
    pub fn from_geometry(g: &Geometry, count: Option<u32>) -> Result<Mesh, String> {
        let n = count.unwrap_or(g.points()).min(g.points()) as usize;
        limits::check(n as u64, u64::from(g.prims()), 0)?;
        let mut m = Mesh::new();
        if n > 0 {
            let p = g
                .point_typed(names::P, AttrType::VEC3)
                .ok_or("the input has no `P` (vec3f) attribute")?;
            let bytes = cpu(p)?;
            m.p = (0..n)
                .map(|i| {
                    let f = |k: usize| f32_at(bytes, i * 16 + k * 4);
                    [f(0), f(1), f(2)]
                })
                .collect();
            for a in g.attrs(AttrClass::Point) {
                if a.name() == names::P || a.name().starts_with("__") {
                    continue;
                }
                m.point_attrs.push(attribute_column(a, n)?);
            }
        }
        if let Some(t) = g.topology().filter(|_| n > 0) {
            let words = |a: &Attribute| -> Result<Vec<u32>, String> {
                Ok(cpu(a)?
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes(c.try_into().expect("4 bytes")))
                    .collect())
            };
            let kinds = words(&t.kinds)?;
            m.start = words(&t.start)?;
            m.corners = words(&t.corners)?;
            m.closed = kinds.iter().map(|&k| k == 0).collect();
            for a in g.attrs(AttrClass::Prim) {
                if !a.name().starts_with("__") {
                    m.prim_attrs.push(attribute_column(a, m.closed.len())?);
                }
            }
        }
        m.point_groups = g
            .groups(AttrClass::Point)
            .iter()
            .map(|s| s.to_string())
            .collect();
        m.prim_groups = g
            .groups(AttrClass::Prim)
            .iter()
            .map(|s| s.to_string())
            .collect();
        // A GPU-counted input trimmed to its live count: drop primitives
        // past it (simulations have none).
        m.validate()?;
        Ok(m)
    }

    /// The geometric normal of polygon `i` (Newell's method; its length is
    /// twice the polygon's area).
    pub fn prim_normal(&self, i: usize) -> [f64; 3] {
        newell(self.prim(i).iter().map(|&c| self.p[c as usize]))
    }
}

/// Newell's normal of a polygon (length = 2 × area).
pub fn newell(mut points: impl Iterator<Item = [f32; 3]>) -> [f64; 3] {
    let mut n = [0.0f64; 3];
    let Some(first) = points.next() else {
        return n;
    };
    let mut a = first;
    for b in points.chain(std::iter::once(first)) {
        let (ax, ay, az) = (f64::from(a[0]), f64::from(a[1]), f64::from(a[2]));
        let (bx, by, bz) = (f64::from(b[0]), f64::from(b[1]), f64::from(b[2]));
        n[0] += (ay - by) * (az + bz);
        n[1] += (az - bz) * (ax + bx);
        n[2] += (ax - bx) * (ay + by);
        a = b;
    }
    n
}

fn set(list: &mut Vec<Column>, col: Column) {
    match list.iter_mut().find(|c| c.name == col.name) {
        Some(slot) => *slot = col,
        None => list.push(col),
    }
}

fn group_bit(groups: &mut Vec<String>, name: &str) -> Option<u32> {
    if let Some(i) = groups.iter().position(|g| g == name) {
        return Some(i as u32);
    }
    if groups.len() >= atd_core::geo::MAX_GROUPS {
        return None;
    }
    groups.push(name.to_owned());
    Some(groups.len() as u32 - 1)
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn cpu(a: &Attribute) -> Result<&[u8], String> {
    a.cpu_bytes()
        .ok_or_else(|| format!("attribute `{}` has no CPU copy", a.name()))
}

/// A column as a GEO attribute (3 components padded to 16 bytes).
fn column_attribute(c: &Column) -> Attribute {
    let comps = usize::from(c.comps);
    let ty = c.ty();
    let stride = ty.stride() as usize;
    let len = c.len();
    let mut bytes = vec![0u8; len * stride];
    let word = |i: usize| -> [u8; 4] {
        match &c.values {
            Values::F32(v) => v[i].to_le_bytes(),
            Values::I32(v) => v[i].to_le_bytes(),
            Values::U32(v) => v[i].to_le_bytes(),
        }
    };
    for e in 0..len {
        for k in 0..comps {
            let at = e * stride + k * 4;
            bytes[at..at + 4].copy_from_slice(&word(e * comps + k));
        }
    }
    Attribute::cpu(Arc::<str>::from(c.name.as_str()), ty, bytes)
}

/// A GEO attribute's first `n` elements as a column.
fn attribute_column(a: &Attribute, n: usize) -> Result<Column, String> {
    let ty = a.ty();
    let bytes = cpu(a)?;
    let comps = usize::from(ty.comps());
    let stride = ty.stride() as usize;
    if bytes.len() < n * stride {
        return Err(format!(
            "attribute `{}` is shorter than its count",
            a.name()
        ));
    }
    let word = |e: usize, k: usize| -> [u8; 4] {
        let at = e * stride + k * 4;
        bytes[at..at + 4].try_into().expect("4 bytes")
    };
    let values = match ty.scalar {
        Scalar::F32 => Values::F32(
            (0..n)
                .flat_map(|e| (0..comps).map(move |k| (e, k)))
                .map(|(e, k)| f32::from_le_bytes(word(e, k)))
                .collect(),
        ),
        Scalar::I32 => Values::I32(
            (0..n)
                .flat_map(|e| (0..comps).map(move |k| (e, k)))
                .map(|(e, k)| i32::from_le_bytes(word(e, k)))
                .collect(),
        ),
        Scalar::U32 => Values::U32(
            (0..n)
                .flat_map(|e| (0..comps).map(move |k| (e, k)))
                .map(|(e, k)| u32::from_le_bytes(word(e, k)))
                .collect(),
        ),
    };
    Ok(Column {
        name: a.name().to_owned(),
        comps: ty.comps(),
        values,
    })
}

/// Vector helpers in `f64`.
pub mod v3 {
    pub type V = [f64; 3];

    pub fn of(p: [f32; 3]) -> V {
        [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
    }

    pub fn f32(p: V) -> [f32; 3] {
        [p[0] as f32, p[1] as f32, p[2] as f32]
    }

    pub fn add(a: V, b: V) -> V {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    pub fn sub(a: V, b: V) -> V {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    pub fn scale(a: V, s: f64) -> V {
        [a[0] * s, a[1] * s, a[2] * s]
    }

    pub fn dot(a: V, b: V) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    pub fn cross(a: V, b: V) -> V {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    pub fn len(a: V) -> f64 {
        dot(a, a).sqrt()
    }

    pub fn lerp(a: V, b: V, t: f64) -> V {
        add(a, scale(sub(b, a), t))
    }

    /// `a` scaled to unit length (zero stays zero).
    pub fn norm(a: V) -> V {
        let l = len(a);
        if l > 1e-30 {
            scale(a, 1.0 / l)
        } else {
            [0.0; 3]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> Mesh {
        let mut m = Mesh::new();
        for p in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ] {
            m.add_point(p);
        }
        m.add_polygon(&[0, 1, 2, 3]);
        m.set_point_attr(Column::f32("w", 1, vec![1.0, 2.0, 3.0, 4.0]));
        m
    }

    #[test]
    fn geometry_round_trip_keeps_everything() {
        let mut m = quad();
        m.set_prim_attr(Column::u32("tag", vec![7]));
        m.set_group(AttrClass::Point, "left", &[true, false, false, true])
            .unwrap();
        m.set_point_attr(Column::vec3(names::N, &[[0.0, 0.0, 1.0]; 4]));
        let g = m.to_geometry(None);
        assert_eq!((g.points(), g.prims()), (4, 1));
        let back = Mesh::from_geometry(&g, None).unwrap();
        assert_eq!(back, m);
        assert_eq!(
            back.group_members(AttrClass::Point, "left").unwrap(),
            [true, false, false, true]
        );
    }

    #[test]
    fn validate_rejects_bad_indices_and_lengths() {
        let mut m = quad();
        m.corners[2] = 9;
        assert!(m.validate().unwrap_err().contains("point 9"));
        let mut m = quad();
        m.set_point_attr(Column::f32("short", 1, vec![1.0]));
        assert!(m.validate().is_err());
        assert!(limits::check(limits::POINTS as u64 + 1, 0, 0).is_err());
    }

    #[test]
    fn interp_blends_floats_and_picks_integers() {
        let src = [
            Column::f32("a", 2, vec![0.0, 0.0, 2.0, 4.0]),
            Column::u32("i", vec![5, 9]),
        ];
        let mut it = Interp::new(&src, 1);
        it.blend(&[(0, 0.25), (1, 0.75)]);
        let out = it.finish();
        assert_eq!(out[0].floats().unwrap(), [1.5, 3.0]);
        assert_eq!(out[1].values, Values::U32(vec![9]));
    }

    #[test]
    fn removing_unused_points_remaps_corners() {
        let mut m = quad();
        m.add_point([5.0, 5.0, 5.0]);
        m.p.swap(0, 4);
        m.point_attr_mut("w")
            .unwrap()
            .floats_mut()
            .unwrap()
            .push(5.0);
        // Point 0 is now unused; the quad uses 4, 1, 2, 3.
        m.corners[0] = 4;
        m.remove_unused_points();
        assert_eq!(m.points(), 4);
        assert_eq!(m.prim(0), [3, 0, 1, 2]);
        assert_eq!(
            m.point_attr("w").unwrap().floats().unwrap(),
            [2.0, 3.0, 4.0, 5.0]
        );
        m.validate().unwrap();
    }

    #[test]
    fn newell_gives_twice_the_area() {
        let m = quad();
        assert_eq!(m.prim_normal(0), [0.0, 0.0, 2.0]);
    }
}
