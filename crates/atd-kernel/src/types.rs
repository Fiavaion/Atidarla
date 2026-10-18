//! Kernel value types and attribute storage types.

use std::fmt;

/// The type of a kernel value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Type {
    #[default]
    Void,
    Bool,
    Int,
    Float,
    /// `vec2`, `vec3`, `vec4` (2 to 4 `f32` components).
    Vec(u8),
    /// `mat3`, `mat4` (column-major `f32`).
    Mat(u8),
}

impl Type {
    /// Parses a type keyword (Atidarla and VEX spellings).
    pub fn from_keyword(word: &str) -> Option<Type> {
        Some(match word {
            "float" => Type::Float,
            "int" => Type::Int,
            "bool" => Type::Bool,
            "vec2" | "vector2" => Type::Vec(2),
            "vec3" | "vector" => Type::Vec(3),
            "vec4" | "vector4" | "quat" => Type::Vec(4),
            "mat3" | "matrix3" => Type::Mat(3),
            "mat4" | "matrix" => Type::Mat(4),
            _ => return None,
        })
    }

    /// The WGSL spelling.
    pub fn wgsl(self) -> &'static str {
        match self {
            Type::Void => "void",
            Type::Bool => "bool",
            Type::Int => "i32",
            Type::Float => "f32",
            Type::Vec(2) => "vec2f",
            Type::Vec(3) => "vec3f",
            Type::Vec(_) => "vec4f",
            Type::Mat(3) => "mat3x3f",
            Type::Mat(_) => "mat4x4f",
        }
    }

    pub fn is_scalar_number(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// `float` or a vector.
    pub fn is_float_like(self) -> bool {
        matches!(self, Type::Float | Type::Vec(_))
    }

    /// Components of a vector, 1 for scalars, 0 otherwise.
    pub fn components(self) -> u8 {
        match self {
            Type::Bool | Type::Int | Type::Float => 1,
            Type::Vec(n) => n,
            _ => 0,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Type::Void => "void",
            Type::Bool => "bool",
            Type::Int => "int",
            Type::Float => "float",
            Type::Vec(2) => "vec2",
            Type::Vec(3) => "vec3",
            Type::Vec(_) => "vec4",
            Type::Mat(3) => "mat3",
            Type::Mat(_) => "mat4",
        })
    }
}

/// The scalar kind of an attribute's storage (the M2 attribute model:
/// `docs/design/geometry-attributes.md` §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scalar {
    F32,
    I32,
    U32,
}

/// How an attribute is stored: scalar kind × 1 to 4 components.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttrType {
    pub scalar: Scalar,
    pub components: u8,
}

impl AttrType {
    pub const fn new(scalar: Scalar, components: u8) -> Self {
        Self { scalar, components }
    }

    pub const F32: AttrType = AttrType::new(Scalar::F32, 1);
    pub const I32: AttrType = AttrType::new(Scalar::I32, 1);
    pub const U32: AttrType = AttrType::new(Scalar::U32, 1);
    pub const VEC2: AttrType = AttrType::new(Scalar::F32, 2);
    pub const VEC3: AttrType = AttrType::new(Scalar::F32, 3);
    pub const VEC4: AttrType = AttrType::new(Scalar::F32, 4);

    /// Bytes from one element to the next in a storage buffer. 3-component types use a
    /// 16-byte stride (design §2.1; WGSL `array<vec3f>` has stride 16 anyway).
    pub fn stride(self) -> u32 {
        match self.components {
            1 => 4,
            2 => 8,
            _ => 16,
        }
    }

    /// The WGSL element type of the storage array.
    pub fn wgsl(self) -> String {
        let s = match self.scalar {
            Scalar::F32 => "f32",
            Scalar::I32 => "i32",
            Scalar::U32 => "u32",
        };
        match self.components {
            1 => s.to_owned(),
            n => format!("vec{n}<{s}>"),
        }
    }

    /// The kernel type this storage reads as, if kernels support it.
    pub fn kernel_type(self) -> Option<Type> {
        match (self.scalar, self.components) {
            (Scalar::F32, 1) => Some(Type::Float),
            (Scalar::F32, n @ 2..=4) => Some(Type::Vec(n)),
            (Scalar::I32 | Scalar::U32, 1) => Some(Type::Int),
            _ => None,
        }
    }

    /// The storage used for a kernel type when nothing else decides it.
    pub fn for_kernel_type(ty: Type) -> Option<AttrType> {
        match ty {
            Type::Float => Some(AttrType::F32),
            Type::Int => Some(AttrType::I32),
            Type::Vec(n) => Some(AttrType::new(Scalar::F32, n)),
            _ => None,
        }
    }
}

impl fmt::Display for AttrType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.scalar {
            Scalar::F32 => "f32",
            Scalar::I32 => "i32",
            Scalar::U32 => "u32",
        };
        if self.components == 1 {
            f.pad(s)
        } else {
            f.pad(&format!("{s}x{}", self.components))
        }
    }
}

/// The standard attribute names and their storage (docs/kernel-dsl.md).
pub fn standard_attribute(name: &str) -> Option<AttrType> {
    Some(match name {
        "P" | "v" | "N" | "accel" | "force" | "up" | "rest" | "scale" => AttrType::VEC3,
        "Cd" | "orient" => AttrType::VEC4,
        "uv" => AttrType::VEC2,
        "age" | "life" | "pscale" | "mass" => AttrType::F32,
        "id" => AttrType::U32,
        _ => return None,
    })
}
