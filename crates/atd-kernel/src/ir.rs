//! The typed IR: what the checker produces, the WGSL lowering emits and the reference
//! interpreter runs. Every expression carries its type; every implicit conversion of the
//! source language is an explicit node here.

use crate::Target;
use crate::bindings::ParamDecl;
use crate::diag::Span;
use crate::types::{AttrType, Type};

pub type LocalId = u32;
pub type FnId = u32;
pub type AttrId = u32;

/// The most iterations any one `for` loop runs (docs/kernel-dsl.md).
pub const MAX_TRIP: u32 = 1 << 20;

/// A checked kernel.
#[derive(Clone, Debug)]
pub struct Program {
    pub target: Target,
    /// Attributes in first-use order (`AttrId` indexes this).
    pub attrs: Vec<Attr>,
    pub params: Vec<ParamDecl>,
    /// Input images used, ascending.
    pub images: Vec<u32>,
    pub functions: Vec<Function>,
    /// The kernel body, as a function with no parameters and no result.
    pub body: Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    /// The type kernels see.
    pub ty: Type,
    /// How it is stored.
    pub storage: AttrType,
    /// Read with `@name` (or modified, which reads it).
    pub read: bool,
    /// Written with `@name = ...`.
    pub written: bool,
    /// Read at other elements (`point()`, `nearpoint(s)`), so it needs the input values
    /// even when this kernel writes it.
    pub read_other: bool,
    /// The first use.
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub ret: Type,
    /// The first locals, in order.
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// Declares a local; `None` initialises it to zero.
    Decl(LocalId, Option<Expr>),
    /// The value already has the place's type.
    Assign(Place, Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    /// `for (int var = start; var cmp end; var += step)`, `end` evaluated once, at most
    /// [`MAX_TRIP`] iterations.
    For {
        var: LocalId,
        start: Expr,
        end: Expr,
        cmp: Cmp,
        step: i32,
        body: Vec<Stmt>,
    },
    /// `foreach (int var; nearpoints(0, pos, radius, max))`: points within `radius` of
    /// `pos`, in index order, at most `max` of them.
    NearPoints {
        var: LocalId,
        pos: Expr,
        radius: Expr,
        max: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    /// A call to a user function, result discarded.
    Expr(Expr),
    Block(Vec<Stmt>),
}

#[derive(Clone, Debug)]
pub struct Place {
    pub base: PlaceBase,
    /// One vector component.
    pub comp: Option<Comp>,
    /// The type of the place (the component's type when `comp` is set).
    pub ty: Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceBase {
    Local(LocalId),
    Attr(AttrId),
}

#[derive(Clone, Debug)]
pub enum Comp {
    Const(u8),
    /// An `int` index, clamped to the vector.
    Dynamic(Box<Expr>),
}

/// Read-only values the engine provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Global {
    PtNum,
    NumPt,
    Time,
    TimeInc,
    Frame,
    Ix,
    Iy,
    /// Pixels: pixel centre in 0..1.
    PixelP,
    Res,
}

impl Global {
    pub fn ty(self) -> Type {
        match self {
            Global::PtNum | Global::NumPt | Global::Ix | Global::Iy => Type::Int,
            Global::Time | Global::TimeInc | Global::Frame => Type::Float,
            Global::PixelP | Global::Res => Type::Vec(2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

pub use crate::ast::BinOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Builtin {
    Abs,
    Sign,
    Floor,
    Ceil,
    Fract,
    Trunc,
    Round,
    Sqrt,
    Exp,
    Log,
    Log2,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Radians,
    Degrees,
    Atan2,
    Pow,
    Min,
    Max,
    Step,
    Clamp,
    Mix,
    Smoothstep,
    Fit,
    Length,
    Length2,
    Distance,
    Distance2,
    Normalize,
    Dot,
    Cross,
    Transpose,
    Determinant,
    Invert,
    Ident,
    Quaternion,
    QMultiply,
    QRotate,
    QInvert,
    QConvert,
    Slerp,
    Perlin,
    Simplex,
    Worley,
    CurlNoise,
    Rand,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Bool(bool),
    Int(i32),
    Float(f32),
    Local(LocalId),
    Attr(AttrId),
    Global(Global),
    /// A `ch()` parameter by slot.
    Param(u32),
    Unary(UnOp, Box<Expr>),
    /// Operands already converted: equal types, except matrix products and a matrix
    /// scaled by a `float`.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `cond ? a : b` (both evaluated).
    Select(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Scalar conversion to `ty` (int, float, bool).
    Convert(Box<Expr>),
    /// A `float` copied into every component of `ty`.
    Splat(Box<Expr>),
    /// A vector from `float`s and vectors, or a matrix from scalars or columns; no
    /// arguments means zero.
    Construct(Vec<Expr>),
    Swizzle(Box<Expr>, Vec<u8>),
    /// Vector component or matrix column; the `int` index is clamped.
    Index(Box<Expr>, Box<Expr>),
    Builtin(Builtin, Vec<Expr>),
    Call(FnId, Vec<Expr>),
    /// `point(0, name, index)`.
    PointRead(AttrId, Box<Expr>),
    NumPoints,
    /// `nearpoint(0, pos [, maxdist])`.
    NearPoint(Box<Expr>, Option<Box<Expr>>),
    /// `texture(input, uv)`.
    Texture(u32, Box<Expr>),
}

impl Expr {
    pub fn new(kind: ExprKind, ty: Type, span: Span) -> Self {
        Self { kind, ty, span }
    }
}
