//! What a compiled kernel needs from the engine: its resource layout, its parameters and
//! the byte layout of its uniform buffer.
//!
//! Layout (follows `docs/design/geometry-attributes.md` §2.1 and §5):
//!
//! | Target | Group 0 (engine) | Group 1 | Group 2 |
//! |---|---|---|---|
//! | Points | 0: header (storage, read: `count`, `capacity`), 1: uniforms, 2: sampler (only with images) | attributes, sorted by name (a snapshot right after its attribute) | images, binding = input index |
//! | Pixels | 0: uniforms, 1: sampler | images, binding = input index (input 0 always) | — |
//!
//! Attribute buffers are arrays of the attribute's storage type at its stride (16 bytes
//! for 3-component types).

use crate::Target;
use crate::diag::Span;
use crate::types::{AttrType, Type};

/// Invocations per workgroup of a Points kernel.
pub const WORKGROUP_SIZE: u32 = 64;

/// Bytes of the Points header buffer: `count: u32, capacity: u32`.
pub const HEADER_SIZE: u64 = 8;

/// Bytes of the uniform buffer before the first parameter: `globals: vec4f` (time,
/// time step, frame, 0) and `res: vec4f` (width, height, 1/width, 1/height).
pub const PARAMS_OFFSET: u64 = 32;

/// The most `ch()` parameters one kernel may create.
pub const MAX_PARAMS: usize = 64;

/// A `group`/`binding` pair and the WGSL variable bound there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub group: u32,
    pub binding: u32,
    pub wgsl_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

/// One attribute storage buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrBinding {
    /// The attribute's name (`P`, `v`, ...): together with `ty`, the hot-reload key.
    pub name: String,
    /// Storage type.
    pub ty: AttrType,
    /// What the kernel sees.
    pub kernel_type: Type,
    pub access: Access,
    /// The input values, unchanged during the dispatch, for `point()`/`nearpoints()` of
    /// an attribute this kernel also writes. Must be a different buffer from the
    /// attribute's read-write binding.
    pub snapshot: bool,
    /// The kernel writes it: if the input has no such attribute, the engine creates it
    /// zero-filled before the dispatch.
    pub created_if_missing: bool,
    pub group: u32,
    pub binding: u32,
    /// Bytes per element.
    pub stride: u32,
    pub wgsl_name: String,
}

/// One input image (`texture_2d<f32>`, sampled at level 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBinding {
    pub input: u32,
    pub group: u32,
    pub binding: u32,
    pub wgsl_name: String,
}

/// The resources a compiled kernel binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    pub target: Target,
    /// Points: invocations per workgroup. Dispatch `ceil(count / size)` workgroups as
    /// `(x, y, 1)` with `x ≤ 65535`; the kernel computes its index as
    /// `gid.x + gid.y * num_workgroups.x * size`. Pixels: 0 (a full-screen draw).
    pub workgroup_size: u32,
    /// Points: the compute entry point. Pixels: the fragment entry point.
    pub entry_point: String,
    /// Pixels: the vertex entry point (one triangle, `draw(0..3, 0..1)`).
    pub vertex_entry_point: Option<String>,
    /// Points only: `struct { count: u32, capacity: u32 }`, read-only storage.
    pub header: Option<Slot>,
    pub uniforms: Slot,
    /// Bytes of the uniform buffer.
    pub uniform_size: u64,
    /// A filtering sampler (linear, clamp to edge), when images are used.
    pub sampler: Option<Slot>,
    pub attributes: Vec<AttrBinding>,
    pub images: Vec<ImageBinding>,
}

impl Bindings {
    /// Storage buffers bound in one stage.
    pub fn storage_buffers(&self) -> u32 {
        u32::from(self.header.is_some()) + self.attributes.len() as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Float,
    Int,
    Vec2,
    Vec3,
    Vec4,
}

impl ParamType {
    pub fn kernel_type(self) -> Type {
        match self {
            ParamType::Float => Type::Float,
            ParamType::Int => Type::Int,
            ParamType::Vec2 => Type::Vec(2),
            ParamType::Vec3 => Type::Vec(3),
            ParamType::Vec4 => Type::Vec(4),
        }
    }

    pub fn components(self) -> usize {
        match self {
            ParamType::Float | ParamType::Int => 1,
            ParamType::Vec2 => 2,
            ParamType::Vec3 => 3,
            ParamType::Vec4 => 4,
        }
    }
}

/// A parameter a `ch()` call creates (FR-K1: the operator shows a control for it).
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: String,
    pub ty: ParamType,
    /// Default value; unused components are 0.
    pub default: [f32; 4],
    pub min: Option<f32>,
    pub max: Option<f32>,
    /// Index of its `vec4f` in the uniform buffer.
    pub slot: u32,
    /// The first `ch()` call that names it.
    pub span: Span,
}

impl ParamDecl {
    /// Byte offset of its value in the uniform buffer.
    pub fn offset(&self) -> u64 {
        PARAMS_OFFSET + 16 * u64::from(self.slot)
    }
}

/// Per-frame values the engine supplies.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameGlobals {
    /// `@Time`, seconds.
    pub time: f32,
    /// `@TimeInc`, seconds.
    pub time_inc: f32,
    /// `@Frame`.
    pub frame: f32,
    /// Pixels: output size (`@res`); ignored by Points.
    pub width: u32,
    pub height: u32,
}

/// Writes the uniform buffer contents into `out` (at least `uniform_size` bytes;
/// nothing is allocated). `values[slot]` is the value of each parameter; a missing
/// entry uses the default. `int` parameters are stored as `i32` bits.
pub fn write_uniforms(
    params: &[ParamDecl],
    globals: &FrameGlobals,
    values: &[[f32; 4]],
    out: &mut [u8],
) {
    let mut put = |offset: usize, bits: u32| {
        if let Some(dst) = out.get_mut(offset..offset + 4) {
            dst.copy_from_slice(&bits.to_le_bytes());
        }
    };
    let (w, h) = (globals.width as f32, globals.height as f32);
    let inv = |x: f32| if x > 0.0 { 1.0 / x } else { 0.0 };
    for (i, v) in [
        globals.time,
        globals.time_inc,
        globals.frame,
        0.0,
        w,
        h,
        inv(w),
        inv(h),
    ]
    .into_iter()
    .enumerate()
    {
        put(i * 4, v.to_bits());
    }
    for p in params {
        let value = values.get(p.slot as usize).copied().unwrap_or(p.default);
        let base = p.offset() as usize;
        for (c, v) in value.iter().enumerate() {
            let bits = if p.ty == ParamType::Int {
                if c == 0 { (*v as i32) as u32 } else { 0 }
            } else if c < p.ty.components() {
                v.to_bits()
            } else {
                0
            };
            put(base + 4 * c, bits);
        }
    }
}
