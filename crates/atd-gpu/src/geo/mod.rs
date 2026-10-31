//! The `GEO` value carried on geometry wires (PRD §5.4), shared by the PNT
//! and MSH families: points and primitives with typed point, prim and detail
//! attributes. Design: `docs/design/geometry-attributes.md` (D1-D7).
//!
//! - **Structure of arrays.** Each attribute is its own buffer (layout rules
//!   in [`atd_core::geo`]: 3-component types at a 16-byte stride).
//! - **Residency per attribute.** An attribute holds a GPU buffer, a CPU copy
//!   or both ([`Residency`]). A copy on the other side is added only when an
//!   operator asks for it ([`Attribute::ensure_gpu`], [`readback`]); both
//!   copies always hold the same values.
//! - **Published values never change.** A [`Geometry`] is a cheap handle:
//!   attributes are reference-counted and shared between the values that
//!   use them. An operator writes in place only into an attribute it holds
//!   uniquely ([`Attribute::unique_gpu`]); otherwise it writes a new one from
//!   the pool ([`BufferPool`]) and shares the rest.
//! - **GPU-side counts.** A simulation's point count stays on the GPU in the
//!   geometry's header ([`atd_core::geo::header`]), with ready-made
//!   `draw_indirect` and `dispatch_workgroups_indirect` arguments;
//!   [`Geometry::points`] is then an upper bound (the capacity).
//!
//! Operators reach all of this through `GeoCtx` (`crate::engine`).

pub mod kernels;
pub mod pool;
pub mod readback;
pub mod wgsl;

use std::sync::{Arc, OnceLock};

pub use atd_core::geo::{AttrClass, AttrType, Scalar, header, names};
pub use pool::{ATTR_USAGE, BufferPool, GeoBuffer};

/// Where an attribute's values are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Residency {
    /// Only in CPU memory (for example an MSH cook's result).
    Cpu,
    /// Only in a GPU buffer (for example a PNT operator's result).
    Gpu,
    /// In both, with the same values.
    Both,
}

/// The values of one attribute: a GPU buffer, a CPU copy, or both.
#[derive(Debug)]
pub struct AttrData {
    len: u32,
    gpu: OnceLock<GeoBuffer>,
    cpu: OnceLock<Box<[u8]>>,
}

impl AttrData {
    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A named, typed attribute: a cheap handle (cloning shares the values).
#[derive(Debug, Clone)]
pub struct Attribute {
    name: Arc<str>,
    ty: AttrType,
    data: Arc<AttrData>,
}

impl Attribute {
    /// An attribute of `len` elements held in `buffer` (at least
    /// `len × stride` bytes; typically from [`BufferPool::attr`]).
    pub fn gpu(name: impl Into<Arc<str>>, ty: AttrType, len: u32, buffer: GeoBuffer) -> Self {
        let need = u64::from(len) * u64::from(ty.stride());
        assert!(buffer.size() >= need, "attribute buffer too small");
        Self {
            name: name.into(),
            ty,
            data: Arc::new(AttrData {
                len,
                gpu: OnceLock::from(buffer),
                cpu: OnceLock::new(),
            }),
        }
    }

    /// An attribute from CPU bytes: `len × stride` bytes laid out by the
    /// layout rules (little-endian, 3-component types padded to 16 bytes).
    pub fn cpu(name: impl Into<Arc<str>>, ty: AttrType, bytes: Vec<u8>) -> Self {
        let stride = ty.stride() as usize;
        assert_eq!(bytes.len() % stride, 0, "whole elements");
        let len = u32::try_from(bytes.len() / stride).expect("fewer than 2^32 elements");
        Self {
            name: name.into(),
            ty,
            data: Arc::new(AttrData {
                len,
                gpu: OnceLock::new(),
                cpu: OnceLock::from(bytes.into_boxed_slice()),
            }),
        }
    }

    /// A `vec3f` attribute from points (padded to 16 bytes each).
    pub fn from_vec3(name: impl Into<Arc<str>>, values: &[[f32; 3]]) -> Self {
        let mut bytes = Vec::with_capacity(values.len() * 16);
        for v in values {
            for c in [v[0], v[1], v[2], 0.0] {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        Self::cpu(name, AttrType::VEC3, bytes)
    }

    /// A `f32` attribute.
    pub fn from_f32(name: impl Into<Arc<str>>, values: &[f32]) -> Self {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self::cpu(name, AttrType::F32, bytes)
    }

    /// A `vec4f` attribute.
    pub fn from_vec4(name: impl Into<Arc<str>>, values: &[[f32; 4]]) -> Self {
        let bytes = values
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        Self::cpu(name, AttrType::VEC4, bytes)
    }

    /// A `u32` attribute.
    pub fn from_u32(name: impl Into<Arc<str>>, values: &[u32]) -> Self {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self::cpu(name, AttrType::U32, bytes)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name as a shared string (cloning it does not allocate).
    pub fn name_arc(&self) -> &Arc<str> {
        &self.name
    }

    pub fn ty(&self) -> AttrType {
        self.ty
    }

    /// Number of elements.
    pub fn len(&self) -> u32 {
        self.data.len
    }

    pub fn is_empty(&self) -> bool {
        self.data.len == 0
    }

    /// Bytes of the values (`len × stride`).
    pub fn bytes(&self) -> u64 {
        u64::from(self.data.len) * u64::from(self.ty.stride())
    }

    pub fn residency(&self) -> Residency {
        match (self.data.gpu.get().is_some(), self.data.cpu.get().is_some()) {
            (true, true) => Residency::Both,
            (false, true) => Residency::Cpu,
            _ => Residency::Gpu,
        }
    }

    /// The GPU buffer, if the values are on the GPU.
    pub fn gpu_buffer(&self) -> Option<&wgpu::Buffer> {
        self.data.gpu.get().map(GeoBuffer::buffer)
    }

    /// The CPU copy, if the values are on the CPU.
    pub fn cpu_bytes(&self) -> Option<&[u8]> {
        self.data.cpu.get().map(|b| &b[..])
    }

    /// The GPU buffer, uploading the CPU copy first if the values are only
    /// on the CPU (a staged `write_buffer`: it never blocks). Shared values
    /// gain the GPU copy too, so it is uploaded once.
    pub fn ensure_gpu(&self, queue: &wgpu::Queue, pool: &BufferPool) -> &wgpu::Buffer {
        self.data
            .gpu
            .get_or_init(|| {
                let buffer = pool.attr(self.bytes().max(4));
                if let Some(cpu) = self.data.cpu.get()
                    && !cpu.is_empty()
                {
                    queue.write_buffer(&buffer, 0, cpu);
                }
                buffer
            })
            .buffer()
    }

    /// Adds a CPU copy of values that are on the GPU (see [`readback`]; the
    /// caller guarantees `bytes` are this attribute's values). Ignored when a
    /// CPU copy exists.
    pub fn set_cpu_copy(&self, bytes: Box<[u8]>) {
        debug_assert_eq!(bytes.len() as u64, self.bytes());
        let _ = self.data.cpu.set(bytes);
    }

    /// Whether no other value shares these values, so they may be written in
    /// place.
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.data) == 1
    }

    /// The GPU buffer for writing in place, if this handle is the only one
    /// and the values are on the GPU. Writing makes any CPU copy stale, so
    /// it is dropped.
    pub fn unique_gpu(&mut self) -> Option<&wgpu::Buffer> {
        let data = Arc::get_mut(&mut self.data)?;
        data.gpu.get()?;
        data.cpu.take();
        data.gpu.get().map(GeoBuffer::buffer)
    }

    /// Whether two handles share the same values.
    pub fn shares(&self, other: &Attribute) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    /// An identity of these values while they are alive, for change
    /// detection without keeping them alive (a held handle would keep its
    /// buffer out of the pool). Two live handles have the same identity
    /// exactly when they [`Self::shares`]; a freed identity may be reused.
    pub fn values_id(&self) -> usize {
        Arc::as_ptr(&self.data).addr()
    }
}

/// Polygon or polyline (design D1; no NURBS or Bézier in M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimKind {
    Polygon,
    Polyline,
}

/// Primitive topology as compressed rows (CSR): primitive `p` uses the
/// points `corners[start[p]..start[p + 1]]`. `kinds`, `start` and `corners`
/// are `u32` attributes (named `__kind`, `__start`, `__corners`), so they
/// share, upload and read back like any other.
#[derive(Debug, Clone)]
pub struct Topology {
    pub kinds: Attribute,
    pub start: Attribute,
    pub corners: Attribute,
}

impl Topology {
    /// Builds topology on the CPU from primitives given as point lists.
    pub fn from_prims(prims: &[(PrimKind, &[u32])]) -> Self {
        let mut kinds = Vec::with_capacity(prims.len());
        let mut start = Vec::with_capacity(prims.len() + 1);
        let mut corners = Vec::new();
        start.push(0);
        for (kind, points) in prims {
            kinds.push(match kind {
                PrimKind::Polygon => 0,
                PrimKind::Polyline => 1,
            });
            corners.extend_from_slice(points);
            start.push(u32::try_from(corners.len()).expect("fewer than 2^32 corners"));
        }
        Self {
            kinds: Attribute::from_u32("__kind", &kinds),
            start: Attribute::from_u32("__start", &start),
            corners: Attribute::from_u32("__corners", &corners),
        }
    }

    /// Number of primitives.
    pub fn len(&self) -> u32 {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

/// A small named detail value (CPU-first: seeds, counters, bounds).
#[derive(Debug, Clone, PartialEq)]
pub enum DetailValue {
    F32([f32; 4]),
    I32(i64),
    U32(u64),
}

/// A geometry: points with attributes, optional primitives with attributes,
/// detail values and group names. Cloning is cheap and shares every
/// attribute's values.
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    points: u32,
    gpu_counted: bool,
    header: Option<Arc<GeoBuffer>>,
    point_attrs: Vec<Attribute>,
    prim_attrs: Vec<Attribute>,
    topology: Option<Topology>,
    detail: Vec<(Arc<str>, DetailValue)>,
    /// Group names per class (point, prim); bit `i` of the class's `group`
    /// attribute is membership of the `i`-th.
    groups: [Vec<Arc<str>>; 2],
}

/// Error from [`Geometry::set`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GeoError {
    #[error("attribute `{name}` has {len} elements but the geometry has {expected} {class}s")]
    Length {
        name: String,
        class: AttrClass,
        len: u32,
        expected: u32,
    },
    #[error("`{0}` is not a valid attribute name")]
    Name(String),
    #[error("detail values are set with `set_detail`")]
    Detail,
}

impl Geometry {
    /// An empty geometry (no points).
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets to no points, attributes, primitives, detail or groups,
    /// keeping storage (no allocation when refilled with the same shape).
    pub fn clear(&mut self) {
        self.points = 0;
        self.gpu_counted = false;
        self.header = None;
        self.point_attrs.clear();
        self.prim_attrs.clear();
        self.topology = None;
        self.detail.clear();
        self.groups.iter_mut().for_each(Vec::clear);
    }

    /// Number of points; with a GPU-side count ([`Self::is_gpu_counted`])
    /// the capacity, an upper bound of the live count.
    pub fn points(&self) -> u32 {
        self.points
    }

    /// Whether the live point count is only on the GPU, in the header.
    pub fn is_gpu_counted(&self) -> bool {
        self.gpu_counted
    }

    /// Number of primitives.
    pub fn prims(&self) -> u32 {
        self.topology.as_ref().map_or(0, Topology::len)
    }

    pub fn topology(&self) -> Option<&Topology> {
        self.topology.as_ref()
    }

    /// The GPU header (count, capacity, indirect arguments), if any.
    pub fn header(&self) -> Option<&wgpu::Buffer> {
        self.header.as_deref().map(GeoBuffer::buffer)
    }

    /// The header handle (to share it with another geometry).
    pub fn header_handle(&self) -> Option<&Arc<GeoBuffer>> {
        self.header.as_ref()
    }

    /// Sets the point count the CPU knows, with a header holding it (or none
    /// for a geometry nothing draws or dispatches over).
    pub fn set_points(&mut self, points: u32, header: Option<Arc<GeoBuffer>>) {
        self.points = points;
        self.gpu_counted = false;
        self.header = header;
    }

    /// Sets a GPU-side count: attribute buffers hold `capacity` elements and
    /// the live count is in `header`.
    pub fn set_gpu_counted(&mut self, capacity: u32, header: Arc<GeoBuffer>) {
        self.points = capacity;
        self.gpu_counted = true;
        self.header = Some(header);
    }

    pub fn set_topology(&mut self, topology: Option<Topology>) {
        self.topology = topology;
    }

    fn list(&self, class: AttrClass) -> &[Attribute] {
        match class {
            AttrClass::Point => &self.point_attrs,
            AttrClass::Prim => &self.prim_attrs,
            AttrClass::Detail => &[],
        }
    }

    /// The attributes of `class` in the order they were added (binding
    /// order). For listings sort by name (design §6: determinism).
    pub fn attrs(&self, class: AttrClass) -> &[Attribute] {
        self.list(class)
    }

    pub fn attr(&self, class: AttrClass, name: &str) -> Option<&Attribute> {
        self.list(class).iter().find(|a| a.name() == name)
    }

    /// A point attribute.
    pub fn point(&self, name: &str) -> Option<&Attribute> {
        self.attr(AttrClass::Point, name)
    }

    /// A point attribute of type `ty`, or `None` when missing or another type.
    pub fn point_typed(&self, name: &str, ty: AttrType) -> Option<&Attribute> {
        self.point(name).filter(|a| a.ty() == ty)
    }

    /// A mutable handle to an attribute (to write in place with
    /// [`Attribute::unique_gpu`]).
    pub fn attr_mut(&mut self, class: AttrClass, name: &str) -> Option<&mut Attribute> {
        let list = match class {
            AttrClass::Point => &mut self.point_attrs,
            AttrClass::Prim => &mut self.prim_attrs,
            AttrClass::Detail => return None,
        };
        list.iter_mut().find(|a| a.name() == name)
    }

    /// Adds or replaces an attribute. Its length must match the class's
    /// element count (point attributes: [`Self::points`]).
    pub fn set(&mut self, class: AttrClass, attr: Attribute) -> Result<(), GeoError> {
        if !atd_core::geo::valid_name(attr.name()) && !attr.name().starts_with("__") {
            return Err(GeoError::Name(attr.name().to_owned()));
        }
        let (list, expected) = match class {
            AttrClass::Point => (&mut self.point_attrs, self.points),
            AttrClass::Prim => (
                &mut self.prim_attrs,
                self.topology.as_ref().map_or(0, Topology::len),
            ),
            AttrClass::Detail => return Err(GeoError::Detail),
        };
        if attr.len() != expected {
            return Err(GeoError::Length {
                name: attr.name().to_owned(),
                class,
                len: attr.len(),
                expected,
            });
        }
        match list.iter_mut().find(|a| a.name() == attr.name()) {
            Some(slot) => *slot = attr,
            None => list.push(attr),
        }
        Ok(())
    }

    /// Adds or replaces a point attribute; panics on a length mismatch
    /// (operator code that sized it itself).
    pub fn set_point(&mut self, attr: Attribute) {
        if let Err(e) = self.set(AttrClass::Point, attr) {
            panic!("{e}");
        }
    }

    /// Removes an attribute; returns it.
    pub fn remove(&mut self, class: AttrClass, name: &str) -> Option<Attribute> {
        let list = match class {
            AttrClass::Point => &mut self.point_attrs,
            AttrClass::Prim => &mut self.prim_attrs,
            AttrClass::Detail => return None,
        };
        let i = list.iter().position(|a| a.name() == name)?;
        Some(list.remove(i))
    }

    pub fn detail(&self, name: &str) -> Option<&DetailValue> {
        self.detail
            .iter()
            .find(|(n, _)| &**n == name)
            .map(|(_, v)| v)
    }

    pub fn set_detail(&mut self, name: impl Into<Arc<str>>, value: DetailValue) {
        let name = name.into();
        match self.detail.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => self.detail.push((name, value)),
        }
    }

    /// Detail values in the order they were added.
    pub fn details(&self) -> impl Iterator<Item = (&str, &DetailValue)> {
        self.detail.iter().map(|(n, v)| (&**n, v))
    }

    /// Group names of `class` (point or prim): bit `i` of the class's
    /// `group` attribute is membership of the `i`-th.
    pub fn groups(&self, class: AttrClass) -> &[Arc<str>] {
        match class {
            AttrClass::Point => &self.groups[0],
            AttrClass::Prim => &self.groups[1],
            AttrClass::Detail => &[],
        }
    }

    /// Adds a group name; returns its bit, or `None` when the class already
    /// has [`atd_core::geo::MAX_GROUPS`].
    pub fn add_group(&mut self, class: AttrClass, name: &str) -> Option<u32> {
        let list = match class {
            AttrClass::Point => &mut self.groups[0],
            AttrClass::Prim => &mut self.groups[1],
            AttrClass::Detail => return None,
        };
        if let Some(i) = list.iter().position(|n| &**n == name) {
            return Some(i as u32);
        }
        if list.len() >= atd_core::geo::MAX_GROUPS {
            return None;
        }
        list.push(name.into());
        Some(list.len() as u32 - 1)
    }

    /// The worst residency over all attributes (the wire badge, design
    /// D3): `Cpu` or `Gpu` if any attribute is only there, else `Both`.
    /// `None` without attributes.
    pub fn residency(&self) -> Option<Residency> {
        let mut out: Option<Residency> = None;
        for a in self.point_attrs.iter().chain(&self.prim_attrs) {
            let r = a.residency();
            out = Some(match (out, r) {
                (None, r) => r,
                (Some(Residency::Both), r) => r,
                (Some(o), _) => o,
            });
        }
        out
    }

    /// Bytes of all attribute values (points and prims).
    pub fn bytes(&self) -> u64 {
        self.point_attrs
            .iter()
            .chain(&self.prim_attrs)
            .map(Attribute::bytes)
            .sum()
    }
}

/// Writes a CPU-count header for `points` into a pooled buffer (a staged
/// `write_buffer`; call it when the count changes, not every frame).
pub fn cpu_header(queue: &wgpu::Queue, pool: &BufferPool, points: u32) -> Arc<GeoBuffer> {
    let buffer = pool.attr(header::BYTES);
    queue.write_buffer(&buffer, 0, &header::bytes(points, points));
    Arc::new(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_are_shared_until_written() {
        let mut g = Geometry::new();
        g.set_points(2, None);
        g.set_point(Attribute::from_vec3(
            "P",
            &[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
        ));
        let p = g.point("P").unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p.bytes(), 32, "vec3 at a 16-byte stride");
        assert_eq!(p.residency(), Residency::Cpu);
        let bytes = p.cpu_bytes().unwrap();
        assert_eq!(f32::from_le_bytes(bytes[16..20].try_into().unwrap()), 4.0);

        let mut copy = g.clone();
        assert!(copy.point("P").unwrap().shares(g.point("P").unwrap()));
        assert!(!copy.point("P").unwrap().is_unique());
        // Values only on the CPU cannot be written on the GPU in place.
        assert!(
            copy.attr_mut(AttrClass::Point, "P")
                .unwrap()
                .unique_gpu()
                .is_none()
        );
        drop(g);
        assert!(copy.point("P").unwrap().is_unique());
    }

    #[test]
    fn set_checks_lengths_names_and_replaces() {
        let mut g = Geometry::new();
        g.set_points(3, None);
        let err = g
            .set(AttrClass::Point, Attribute::from_f32("age", &[0.0; 2]))
            .unwrap_err();
        assert!(err.to_string().contains("2 elements"), "{err}");
        assert!(matches!(
            g.set(AttrClass::Point, Attribute::from_f32("a-b", &[0.0; 3])),
            Err(GeoError::Name(_))
        ));
        g.set_point(Attribute::from_f32("age", &[0.0; 3]));
        g.set_point(Attribute::from_f32("life", &[1.0; 3]));
        g.set_point(Attribute::from_f32("age", &[2.0; 3]));
        let names: Vec<&str> = g
            .attrs(AttrClass::Point)
            .iter()
            .map(Attribute::name)
            .collect();
        assert_eq!(names, ["age", "life"], "replaced in place, order kept");
        assert_eq!(g.residency(), Some(Residency::Cpu));
        assert!(g.remove(AttrClass::Point, "age").is_some());
        assert!(g.point("age").is_none());
    }

    #[test]
    fn topology_detail_and_groups() {
        let mut g = Geometry::new();
        g.set_points(4, None);
        g.set_topology(Some(Topology::from_prims(&[
            (PrimKind::Polygon, &[0, 1, 2]),
            (PrimKind::Polyline, &[2, 3]),
        ])));
        assert_eq!(g.prims(), 2);
        let start = g.topology().unwrap().start.cpu_bytes().unwrap();
        assert_eq!(start.len(), 12, "prims + 1 offsets");
        g.set(AttrClass::Prim, Attribute::from_f32("w", &[1.0, 2.0]))
            .unwrap();
        g.set_detail("seed", DetailValue::U32(7));
        g.set_detail("seed", DetailValue::U32(8));
        assert_eq!(g.detail("seed"), Some(&DetailValue::U32(8)));
        assert_eq!(g.add_group(AttrClass::Point, "top"), Some(0));
        assert_eq!(g.add_group(AttrClass::Point, "bottom"), Some(1));
        assert_eq!(g.add_group(AttrClass::Point, "top"), Some(0));
        for i in 2..32 {
            assert!(g.add_group(AttrClass::Point, &format!("g{i}")).is_some());
        }
        assert_eq!(g.add_group(AttrClass::Point, "one_too_many"), None);
        g.clear();
        assert_eq!((g.points(), g.prims()), (0, 0));
    }
}
