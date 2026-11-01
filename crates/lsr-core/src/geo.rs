//! Geometry attribute types and layout rules (PRD §5.4; design:
//! `docs/design/geometry-attributes.md` §1, §2.1, §5).
//!
//! This is the pure-data half of the attribute-buffer framework: element
//! classes, attribute types, standard names, the byte layout every attribute
//! buffer follows on the GPU and the CPU, the GPU header of a geometry, and a
//! WGSL declaration generator. It has no GPU dependency, so the Kernel DSL
//! (`lsr-kernel`) and file formats can use it; the buffers themselves live in
//! `lsr_gpu::geo`.
//!
//! Layout rules (fixed, so the CPU, kernels and files agree):
//! - little-endian; one buffer per attribute (structure of arrays);
//! - 1-, 2- and 4-component types are tightly packed (4, 8, 16 bytes);
//!   **3-component types use a 16-byte stride**, as WGSL `array<vec3f>` does;
//! - buffer sizes are multiples of [`BUFFER_ALIGN`] (256 bytes).

use std::fmt;

/// Which element an attribute belongs to (design D1: no vertex class in M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttrClass {
    /// One value per point.
    Point,
    /// One value per primitive (polygon or polyline).
    Prim,
    /// One value for the whole geometry.
    Detail,
}

impl AttrClass {
    pub const ALL: [AttrClass; 3] = [AttrClass::Point, AttrClass::Prim, AttrClass::Detail];

    pub const fn name(self) -> &'static str {
        match self {
            AttrClass::Point => "point",
            AttrClass::Prim => "prim",
            AttrClass::Detail => "detail",
        }
    }
}

impl fmt::Display for AttrClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Scalar kind of an attribute's components. No `f16`/8-bit storage in M2
/// (`SHADER_F16` is not universal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scalar {
    F32,
    I32,
    U32,
}

/// An attribute's type: a scalar kind and 1 to 4 components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttrType {
    pub scalar: Scalar,
    comps: u8,
}

impl AttrType {
    pub const F32: AttrType = AttrType::new(Scalar::F32, 1);
    pub const VEC2: AttrType = AttrType::new(Scalar::F32, 2);
    pub const VEC3: AttrType = AttrType::new(Scalar::F32, 3);
    pub const VEC4: AttrType = AttrType::new(Scalar::F32, 4);
    pub const I32: AttrType = AttrType::new(Scalar::I32, 1);
    pub const U32: AttrType = AttrType::new(Scalar::U32, 1);

    /// A type of `comps` (1 to 4) components.
    pub const fn new(scalar: Scalar, comps: u8) -> Self {
        assert!(
            comps >= 1 && comps <= 4,
            "attributes have 1 to 4 components"
        );
        Self { scalar, comps }
    }

    pub const fn comps(self) -> u8 {
        self.comps
    }

    /// Bytes from one element to the next: 4, 8, 16 or 16 (3 components are
    /// padded to 16 bytes, on the CPU as on the GPU).
    pub const fn stride(self) -> u32 {
        match self.comps {
            1 => 4,
            2 => 8,
            _ => 16,
        }
    }

    /// The WGSL element type, e.g. `vec3f` or `u32`.
    pub const fn wgsl(self) -> &'static str {
        match (self.scalar, self.comps) {
            (Scalar::F32, 1) => "f32",
            (Scalar::F32, 2) => "vec2f",
            (Scalar::F32, 3) => "vec3f",
            (Scalar::F32, _) => "vec4f",
            (Scalar::I32, 1) => "i32",
            (Scalar::I32, 2) => "vec2i",
            (Scalar::I32, 3) => "vec3i",
            (Scalar::I32, _) => "vec4i",
            (Scalar::U32, 1) => "u32",
            (Scalar::U32, 2) => "vec2u",
            (Scalar::U32, 3) => "vec3u",
            (Scalar::U32, _) => "vec4u",
        }
    }
}

impl fmt::Display for AttrType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wgsl())
    }
}

/// Standard attribute names (the viewer, the renderer and exporters use
/// them; RESEARCH §5.4 uses the same `@P`, `@Cd`, `@v`, `@id`).
pub mod names {
    /// Position, `vec3f`. Every geometry with points has it.
    pub const P: &str = "P";
    /// Normal, `vec3f`.
    pub const N: &str = "N";
    /// Colour, `vec4f` (RGBA, linear).
    pub const CD: &str = "Cd";
    /// Texture coordinate, `vec2f`.
    pub const UV: &str = "uv";
    /// Velocity, `vec3f` (units per second).
    pub const V: &str = "v";
    /// Age in seconds, `f32`.
    pub const AGE: &str = "age";
    /// Life expectancy in seconds, `f32`.
    pub const LIFE: &str = "life";
    /// Identifier, `u32` (wrapping; unique among live particles).
    pub const ID: &str = "id";
    /// Point scale, `f32`.
    pub const PSCALE: &str = "pscale";
    /// Orientation quaternion, `vec4f`.
    pub const ORIENT: &str = "orient";
    /// Group membership mask, `u32`: bit `i` is the class's `i`-th group.
    pub const GROUP: &str = "group";
    /// Per-axis scale, `vec3f` (Gaussian splats: the standard deviations
    /// along the axes `orient` turns).
    pub const SCALE: &str = "scale";
    /// Gaussian-splat spherical-harmonic coefficients beyond the base colour,
    /// `vec4f` each: the per-splat floats (coefficient 1 R, G, B, then
    /// coefficient 2…, in the 3DGS order) four to an attribute, `sh0` holding
    /// floats 0–3. Degree 3 uses all twelve.
    pub const SH: [&str; 12] = [
        "sh0", "sh1", "sh2", "sh3", "sh4", "sh5", "sh6", "sh7", "sh8", "sh9", "sh10", "sh11",
    ];
    /// Detail value (`I32`): the spherical-harmonic degree (0–3) of the
    /// [`SH`] attributes.
    pub const SH_DEGREE: &str = "sh_degree";
}

/// Most groups per class: one bit each in the [`names::GROUP`] mask.
pub const MAX_GROUPS: usize = 32;

/// Buffer offsets and sizes are multiples of this (the WebGPU
/// `min_storage_buffer_offset_alignment`).
pub const BUFFER_ALIGN: u64 = 256;

/// Rounds `bytes` up to [`BUFFER_ALIGN`], at least one block.
pub const fn align_up(bytes: u64) -> u64 {
    let b = if bytes == 0 { 1 } else { bytes };
    b.div_ceil(BUFFER_ALIGN) * BUFFER_ALIGN
}

/// Bytes a buffer of `count` elements of `ty` needs (aligned).
pub const fn buffer_bytes(count: u32, ty: AttrType) -> u64 {
    align_up(count as u64 * ty.stride() as u64)
}

/// Threads per workgroup of every GEO compute kernel; a 1-D dispatch reaches
/// 65,535 × 256 ≈ 16.7M elements.
pub const WORKGROUP: u32 = 256;

/// Workgroups to cover `n` elements.
pub const fn workgroups(n: u32) -> u32 {
    n.div_ceil(WORKGROUP)
}

/// The GPU header of a geometry: its point count on the GPU, so a
/// simulation's count never has to come back to the CPU (design §4.2).
///
/// | Bytes | Field | Meaning |
/// |---|---|---|
/// | 0 | `count` | live points |
/// | 4 | `capacity` | elements the attribute buffers hold |
/// | 8 | `mapped` | after a compaction: elements taken from the map (the rest were appended) |
/// | 12 | — | padding |
/// | 16..32 | `draw` | `draw_indirect` arguments for instanced sprites: 4 vertices, `count` instances |
/// | 32..44 | `dispatch` | `dispatch_workgroups_indirect` arguments covering `count` |
pub mod header {
    /// Size of the header in bytes.
    pub const BYTES: u64 = 48;
    /// Offset of the `draw_indirect` arguments.
    pub const DRAW_OFFSET: u64 = 16;
    /// Offset of the `dispatch_workgroups_indirect` arguments.
    pub const DISPATCH_OFFSET: u64 = 32;
    /// Vertices per sprite (a triangle strip).
    pub const SPRITE_VERTICES: u32 = 4;

    /// The header's WGSL struct (`GeoHeader`).
    pub const WGSL: &str = "struct GeoHeader {
    count: u32,
    capacity: u32,
    mapped: u32,
    pad: u32,
    draw: array<u32, 4>,
    dispatch: array<u32, 4>,
};
";

    /// The header of a geometry whose count the CPU knows, as bytes.
    pub fn bytes(count: u32, capacity: u32) -> [u8; BYTES as usize] {
        let words = [
            count,
            capacity,
            count,
            0,
            SPRITE_VERTICES,
            count,
            0,
            0,
            super::workgroups(count).max(1),
            1,
            1,
            0,
        ];
        let mut out = [0u8; BYTES as usize];
        for (chunk, w) in out.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// How a kernel uses an attribute binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    Read,
    ReadWrite,
}

/// The WGSL declaration of one attribute binding, e.g.
/// `@group(1) @binding(0) var<storage, read_write> P: array<vec3f>;`
/// (design §5 item 2). Attribute names that are not WGSL identifiers must be
/// mapped by the caller.
pub fn wgsl_binding(group: u32, binding: u32, name: &str, ty: AttrType, access: Access) -> String {
    let access = match access {
        Access::Read => "read",
        Access::ReadWrite => "read_write",
    };
    format!(
        "@group({group}) @binding({binding}) var<storage, {access}> {name}: array<{}>;\n",
        ty.wgsl()
    )
}

/// Whether `name` can be an attribute name: an ASCII letter or `_` first,
/// then letters, digits or `_` (so it is also a WGSL identifier).
pub fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name != "_"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strides_follow_the_layout_rules() {
        assert_eq!(AttrType::F32.stride(), 4);
        assert_eq!(AttrType::VEC2.stride(), 8);
        assert_eq!(AttrType::VEC3.stride(), 16, "vec3 is padded to 16 bytes");
        assert_eq!(AttrType::VEC4.stride(), 16);
        assert_eq!(AttrType::new(Scalar::I32, 3).stride(), 16);
        assert_eq!(AttrType::VEC3.wgsl(), "vec3f");
        assert_eq!(AttrType::new(Scalar::U32, 2).wgsl(), "vec2u");
        assert_eq!(buffer_bytes(0, AttrType::VEC3), 256);
        assert_eq!(buffer_bytes(16, AttrType::VEC3), 256);
        assert_eq!(buffer_bytes(17, AttrType::VEC3), 512);
        assert_eq!(buffer_bytes(2_000_000, AttrType::VEC3), 32_000_000);
        assert_eq!(workgroups(2_000_000), 7813);
    }

    #[test]
    fn header_bytes_carry_draw_and_dispatch_arguments() {
        let h = header::bytes(1000, 4096);
        let word = |i: usize| u32::from_le_bytes(h[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!((word(0), word(1), word(2)), (1000, 4096, 1000));
        let d = header::DRAW_OFFSET as usize / 4;
        assert_eq!(
            (word(d), word(d + 1)),
            (4, 1000),
            "4 vertices, count instances"
        );
        let x = header::DISPATCH_OFFSET as usize / 4;
        assert_eq!((word(x), word(x + 1), word(x + 2)), (4, 1, 1));
        // An empty geometry still dispatches one (idle) workgroup.
        let e = header::bytes(0, 0);
        assert_eq!(e[x * 4], 1);
    }

    #[test]
    fn bindings_and_names() {
        assert_eq!(
            wgsl_binding(1, 0, "P", AttrType::VEC3, Access::ReadWrite),
            "@group(1) @binding(0) var<storage, read_write> P: array<vec3f>;\n"
        );
        for ok in ["P", "Cd", "_tmp", "age2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "_", "2x", "a-b", "é"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(AttrClass::Point < AttrClass::Prim && AttrClass::Prim < AttrClass::Detail);
    }
}
