//! Reference interpreter for Points kernels: runs the typed IR on the CPU with the
//! semantics of the generated WGSL (wrapping `int`s, `x / 0 = x`, clamped indices,
//! column-major matrices, the same helper formulas and the Rust twin of the noise
//! library). Tests use it to check that the WGSL means what the language says.
//!
//! Not for the frame path: it allocates and is slow. `texture()` is not supported.

use crate::Compiled;
use crate::ast::BinOp;
use crate::bindings::{FrameGlobals, ParamType};
use crate::ir::*;
use crate::noise;
use crate::types::{AttrType, Scalar, Type};

/// One attribute's data in GPU layout: 32-bit words, `stride / 4` per element (so a
/// `vec3` uses four words, the last one padding).
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub ty: AttrType,
    pub words: Vec<u32>,
}

impl Attribute {
    pub fn zeroed(ty: AttrType, count: usize) -> Self {
        Self {
            ty,
            words: vec![0; count * (ty.stride() as usize / 4)],
        }
    }

    pub fn from_f32(values: &[f32]) -> Self {
        Self {
            ty: AttrType::F32,
            words: values.iter().map(|v| v.to_bits()).collect(),
        }
    }

    pub fn from_vec3(values: &[[f32; 3]]) -> Self {
        Self {
            ty: AttrType::VEC3,
            words: values
                .iter()
                .flat_map(|v| [v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), 0])
                .collect(),
        }
    }

    pub fn from_u32(values: &[u32]) -> Self {
        Self {
            ty: AttrType::U32,
            words: values.to_vec(),
        }
    }

    fn per(&self) -> usize {
        self.ty.stride() as usize / 4
    }

    /// Components of element `i` as words (missing elements read as zero).
    pub fn element(&self, i: usize) -> [u32; 4] {
        let per = self.per();
        let mut out = [0u32; 4];
        for (c, o) in out
            .iter_mut()
            .enumerate()
            .take(usize::from(self.ty.components))
        {
            *o = self.words.get(i * per + c).copied().unwrap_or(0);
        }
        out
    }

    fn set_element(&mut self, i: usize, w: [u32; 4]) {
        let per = self.per();
        for (c, v) in w.iter().enumerate().take(usize::from(self.ty.components)) {
            if let Some(slot) = self.words.get_mut(i * per + c) {
                *slot = *v;
            }
        }
    }

    pub fn vec3(&self, i: usize) -> [f32; 3] {
        let w = self.element(i);
        [
            f32::from_bits(w[0]),
            f32::from_bits(w[1]),
            f32::from_bits(w[2]),
        ]
    }

    pub fn f32(&self, i: usize) -> f32 {
        f32::from_bits(self.element(i)[0])
    }
}

/// Point geometry for the interpreter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub count: usize,
    /// By name; order does not matter.
    pub attributes: Vec<(String, Attribute)>,
}

impl Geometry {
    pub fn get(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, a)| a)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Attribute> {
        self.attributes
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, a)| a)
    }
}

/// A runtime value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum V {
    B(bool),
    I(i32),
    F(f32),
    /// Components, size.
    Vec([f32; 4], u8),
    /// Columns, size.
    Mat([[f32; 4]; 4], u8),
    Void,
}

type R<T> = Result<T, String>;

impl V {
    fn f(self) -> f32 {
        match self {
            V::F(x) => x,
            V::I(i) => i as f32,
            V::B(b) => f32::from(u8::from(b)),
            V::Vec(v, _) => v[0],
            _ => 0.0,
        }
    }

    fn i(self) -> i32 {
        match self {
            V::I(i) => i,
            V::F(x) => x as i32,
            V::B(b) => i32::from(b),
            _ => 0,
        }
    }

    fn b(self) -> bool {
        match self {
            V::B(b) => b,
            V::I(i) => i != 0,
            V::F(x) => x != 0.0,
            _ => false,
        }
    }

    /// Float components (a scalar is one component).
    fn comps(self) -> ([f32; 4], u8) {
        match self {
            V::Vec(v, n) => (v, n),
            V::F(x) => ([x, 0.0, 0.0, 0.0], 1),
            V::I(i) => ([i as f32, 0.0, 0.0, 0.0], 1),
            _ => ([0.0; 4], 1),
        }
    }

    fn mat(self) -> ([[f32; 4]; 4], u8) {
        match self {
            V::Mat(m, n) => (m, n),
            _ => ([[0.0; 4]; 4], 3),
        }
    }

    fn from_comps(c: [f32; 4], n: u8) -> V {
        if n == 1 { V::F(c[0]) } else { V::Vec(c, n) }
    }

    fn zero(ty: Type) -> V {
        match ty {
            Type::Bool => V::B(false),
            Type::Int => V::I(0),
            Type::Float => V::F(0.0),
            Type::Vec(n) => V::Vec([0.0; 4], n),
            Type::Mat(n) => V::Mat([[0.0; 4]; 4], n),
            Type::Void => V::Void,
        }
    }
}

fn map1(v: V, f: impl Fn(f32) -> f32) -> V {
    let (c, n) = v.comps();
    V::from_comps(c.map(&f), n)
}

fn map2(a: V, b: V, f: impl Fn(f32, f32) -> f32) -> V {
    let (x, n) = a.comps();
    let (y, _) = b.comps();
    V::from_comps(
        [f(x[0], y[0]), f(x[1], y[1]), f(x[2], y[2]), f(x[3], y[3])],
        n,
    )
}

fn map3(a: V, b: V, c: V, f: impl Fn(f32, f32, f32) -> f32) -> V {
    let (x, n) = a.comps();
    let (y, _) = b.comps();
    let (z, _) = c.comps();
    V::from_comps(
        [
            f(x[0], y[0], z[0]),
            f(x[1], y[1], z[1]),
            f(x[2], y[2], z[2]),
            f(x[3], y[3], z[3]),
        ],
        n,
    )
}

fn dot(a: [f32; 4], b: [f32; 4], n: u8) -> f32 {
    let mut s = a[0] * b[0];
    for i in 1..usize::from(n) {
        s += a[i] * b[i];
    }
    s
}

fn v3(a: V) -> [f32; 3] {
    let (c, _) = a.comps();
    [c[0], c[1], c[2]]
}

fn vec3(v: [f32; 3]) -> V {
    V::Vec([v[0], v[1], v[2], 0.0], 3)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// WGSL `x / y` and `x % y` for `i32`, as `lsr_idiv` / `lsr_imod`.
fn idiv(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else if b == -1 {
        0i32.wrapping_sub(a)
    } else {
        a / b
    }
}

fn imod(a: i32, b: i32) -> i32 {
    if b == 0 || b == -1 { 0 } else { a % b }
}

/// WGSL float `%`: `x - y * trunc(x / y)`.
fn fmod(x: f32, y: f32) -> f32 {
    x - y * (x / y).trunc()
}

fn fsign(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn mat_vec(m: [[f32; 4]; 4], v: [f32; 4], n: u8) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for (r, o) in out.iter_mut().enumerate().take(usize::from(n)) {
        let mut s = m[0][r] * v[0];
        for c in 1..usize::from(n) {
            s += m[c][r] * v[c];
        }
        *o = s;
    }
    out
}

fn transpose(m: [[f32; 4]; 4], n: u8) -> [[f32; 4]; 4] {
    let mut t = [[0.0f32; 4]; 4];
    for c in 0..usize::from(n) {
        for r in 0..usize::from(n) {
            t[c][r] = m[r][c];
        }
    }
    t
}

fn determinant(m: [[f32; 4]; 4], n: u8) -> f32 {
    if n == 3 {
        let r0 = cross([m[1][0], m[1][1], m[1][2]], [m[2][0], m[2][1], m[2][2]]);
        dot(
            [m[0][0], m[0][1], m[0][2], 0.0],
            [r0[0], r0[1], r0[2], 0.0],
            3,
        )
    } else {
        let (_, det) = inverse4_parts(m);
        det
    }
}

fn inverse3(m: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let c = |i: usize| [m[i][0], m[i][1], m[i][2]];
    let r0 = cross(c(1), c(2));
    let r1 = cross(c(2), c(0));
    let r2 = cross(c(0), c(1));
    let det = c(0)[0] * r0[0] + c(0)[1] * r0[1] + c(0)[2] * r0[2];
    if det == 0.0 {
        return [[0.0; 4]; 4];
    }
    let s = 1.0 / det;
    // transpose(mat3(r0, r1, r2)) * s
    let mut out = [[0.0f32; 4]; 4];
    for (col, o) in out.iter_mut().enumerate().take(3) {
        *o = [r0[col] * s, r1[col] * s, r2[col] * s, 0.0];
    }
    out
}

/// (inverse, determinant) as `lsr_inverse4` computes them.
fn inverse4_parts(m: [[f32; 4]; 4]) -> ([[f32; 4]; 4], f32) {
    let [a00, a01, a02, a03] = m[0];
    let [a10, a11, a12, a13] = m[1];
    let [a20, a21, a22, a23] = m[2];
    let [a30, a31, a32, a33] = m[3];
    let b00 = a00 * a11 - a01 * a10;
    let b01 = a00 * a12 - a02 * a10;
    let b02 = a00 * a13 - a03 * a10;
    let b03 = a01 * a12 - a02 * a11;
    let b04 = a01 * a13 - a03 * a11;
    let b05 = a02 * a13 - a03 * a12;
    let b06 = a20 * a31 - a21 * a30;
    let b07 = a20 * a32 - a22 * a30;
    let b08 = a20 * a33 - a23 * a30;
    let b09 = a21 * a32 - a22 * a31;
    let b10 = a21 * a33 - a23 * a31;
    let b11 = a22 * a33 - a23 * a32;
    let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    if det == 0.0 {
        return ([[0.0; 4]; 4], det);
    }
    let s = 1.0 / det;
    let col = |v: [f32; 4]| v.map(|x| x * s);
    (
        [
            col([
                a11 * b11 - a12 * b10 + a13 * b09,
                a02 * b10 - a01 * b11 - a03 * b09,
                a31 * b05 - a32 * b04 + a33 * b03,
                a22 * b04 - a21 * b05 - a23 * b03,
            ]),
            col([
                a12 * b08 - a10 * b11 - a13 * b07,
                a00 * b11 - a02 * b08 + a03 * b07,
                a32 * b02 - a30 * b05 - a33 * b01,
                a20 * b05 - a22 * b02 + a23 * b01,
            ]),
            col([
                a10 * b10 - a11 * b08 + a13 * b06,
                a01 * b08 - a00 * b10 - a03 * b06,
                a30 * b04 - a31 * b02 + a33 * b00,
                a21 * b02 - a20 * b04 - a23 * b00,
            ]),
            col([
                a11 * b07 - a10 * b09 - a12 * b06,
                a00 * b09 - a01 * b07 + a02 * b06,
                a31 * b01 - a30 * b03 - a32 * b00,
                a20 * b03 - a21 * b01 + a22 * b00,
            ]),
        ],
        det,
    )
}

fn normalize(c: [f32; 4], n: u8) -> [f32; 4] {
    let l = dot(c, c, n).sqrt();
    if l > 0.0 { c.map(|x| x / l) } else { [0.0; 4] }
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let d = hi - lo;
    let z = d == 0.0;
    let t = if z {
        if x >= lo { 1.0 } else { 0.0 }
    } else {
        ((x - lo) / d).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

fn fit(x: f32, omin: f32, omax: f32, nmin: f32, nmax: f32) -> f32 {
    let d = omax - omin;
    let t = if d == 0.0 {
        0.5
    } else {
        ((x - omin) / d).clamp(0.0, 1.0)
    };
    nmin + (nmax - nmin) * t
}

fn qmultiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let c = cross([a[0], a[1], a[2]], [b[0], b[1], b[2]]);
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        out[i] = a[3] * b[i] + b[3] * a[i] + c[i];
    }
    out[3] = a[3] * b[3] - (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]);
    out
}

/// Runs a Points kernel over every point of `geo`, in place. Attributes the kernel uses
/// but `geo` lacks are created zero-filled. `values[slot]` are the parameter values
/// (missing: the defaults).
pub fn run_points(
    c: &Compiled,
    geo: &mut Geometry,
    values: &[[f32; 4]],
    globals: &FrameGlobals,
) -> Result<(), String> {
    let p = &c.program;
    if p.target != crate::Target::Points {
        return Err("the interpreter runs Points kernels only".into());
    }
    for a in &p.attrs {
        match geo.get(&a.name) {
            Some(existing) if existing.ty != a.storage => {
                return Err(format!(
                    "attribute `{}` is {} in the geometry but the kernel needs {}",
                    a.name, existing.ty, a.storage
                ));
            }
            Some(_) => {}
            None => geo
                .attributes
                .push((a.name.clone(), Attribute::zeroed(a.storage, geo.count))),
        }
    }
    // Every element reads the input; writes go to the output (as on the GPU, where
    // `point()` reads a snapshot of anything this kernel writes).
    let input = geo.clone();
    let params: Vec<[f32; 4]> = p
        .params
        .iter()
        .map(|d| values.get(d.slot as usize).copied().unwrap_or(d.default))
        .collect();
    for i in 0..geo.count {
        let mut m = Machine {
            p,
            input: &input,
            params: &params,
            globals,
            index: i,
            attrs: Vec::new(),
        };
        m.attrs = p
            .attrs
            .iter()
            .map(|a| m.load(&a.name, a.storage, a.ty, i))
            .collect();
        m.run_body()?;
        for (a, v) in p.attrs.iter().zip(&m.attrs) {
            if a.written {
                let w = store(*v, a.storage);
                if let Some(attr) = geo.get_mut(&a.name) {
                    attr.set_element(i, w);
                }
            }
        }
    }
    Ok(())
}

fn store(v: V, ty: AttrType) -> [u32; 4] {
    match ty.scalar {
        Scalar::U32 | Scalar::I32 => [v.i() as u32, 0, 0, 0],
        Scalar::F32 => {
            let (c, _) = v.comps();
            c.map(f32::to_bits)
        }
    }
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(V),
}

struct Machine<'a> {
    p: &'a Program,
    input: &'a Geometry,
    params: &'a [[f32; 4]],
    globals: &'a FrameGlobals,
    index: usize,
    /// The per-element copies (`a_name` in the WGSL).
    attrs: Vec<V>,
}

impl Machine<'_> {
    fn load(&self, name: &str, storage: AttrType, ty: Type, i: usize) -> V {
        let Some(a) = self.input.get(name) else {
            return V::zero(ty);
        };
        if i >= self.input.count {
            return V::zero(ty);
        }
        let w = a.element(i);
        match (storage.scalar, ty) {
            (Scalar::U32 | Scalar::I32, _) => V::I(w[0] as i32),
            (Scalar::F32, Type::Float) => V::F(f32::from_bits(w[0])),
            (Scalar::F32, Type::Vec(n)) => V::Vec(w.map(f32::from_bits), n),
            _ => V::zero(ty),
        }
    }

    fn run_body(&mut self) -> R<()> {
        let body = &self.p.body;
        let mut frame: Vec<V> = body.locals.iter().map(|l| V::zero(l.ty)).collect();
        self.block(&mut frame, &body.body)?;
        Ok(())
    }

    fn call(&mut self, f: &Function, args: Vec<V>) -> R<V> {
        let mut frame: Vec<V> = f.locals.iter().map(|l| V::zero(l.ty)).collect();
        for (&id, v) in f.params.iter().zip(args) {
            frame[id as usize] = v;
        }
        match self.block(&mut frame, &f.body)? {
            Flow::Return(v) => Ok(v),
            _ => Ok(V::Void),
        }
    }

    fn block(&mut self, frame: &mut [V], stmts: &[Stmt]) -> R<Flow> {
        for s in stmts {
            match self.stmt(frame, s)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn stmt(&mut self, frame: &mut [V], s: &Stmt) -> R<Flow> {
        match &s.kind {
            StmtKind::Decl(id, init) => {
                let v = match init {
                    Some(e) => self.eval(frame, e)?,
                    None => V::zero(self.local_ty(frame, *id)),
                };
                frame[*id as usize] = v;
            }
            StmtKind::Assign(place, value) => {
                let v = self.eval(frame, value)?;
                let comp = match &place.comp {
                    None => None,
                    Some(Comp::Const(k)) => Some(i32::from(*k)),
                    Some(Comp::Dynamic(i)) => Some(self.eval(frame, i)?.i()),
                };
                let slot = match place.base {
                    PlaceBase::Local(id) => &mut frame[id as usize],
                    PlaceBase::Attr(id) => &mut self.attrs[id as usize],
                };
                match comp {
                    None => *slot = v,
                    Some(k) => match slot {
                        V::Vec(c, n) => {
                            c[k.clamp(0, i32::from(*n) - 1) as usize] = v.f();
                        }
                        V::Mat(m, n) => {
                            let (col, _) = v.comps();
                            m[k.clamp(0, i32::from(*n) - 1) as usize] = col;
                        }
                        _ => return Err("component of a scalar".into()),
                    },
                }
            }
            StmtKind::If(c, t, e) => {
                let branch = if self.eval(frame, c)?.b() { t } else { e };
                return self.block(frame, branch);
            }
            StmtKind::For {
                var,
                start,
                end,
                cmp,
                step,
                body,
            } => {
                frame[*var as usize] = V::I(self.eval(frame, start)?.i());
                let end = self.eval(frame, end)?.i();
                let mut trips = 0u32;
                loop {
                    let i = frame[*var as usize].i();
                    let go = match cmp {
                        Cmp::Lt => i < end,
                        Cmp::Le => i <= end,
                        Cmp::Gt => i > end,
                        Cmp::Ge => i >= end,
                    };
                    if !go {
                        break;
                    }
                    match self.block(frame, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Next | Flow::Continue => {}
                    }
                    frame[*var as usize] = V::I(frame[*var as usize].i().wrapping_add(*step));
                    trips += 1;
                    if trips >= MAX_TRIP {
                        break;
                    }
                }
            }
            StmtKind::NearPoints {
                var,
                pos,
                radius,
                max,
                body,
            } => {
                let pos = v3(self.eval(frame, pos)?);
                let r = self.eval(frame, radius)?.f();
                let max = self.eval(frame, max)?.i();
                let mut found = 0i32;
                for k in 0..self.input.count {
                    if found >= max {
                        break;
                    }
                    let q = self.position(k);
                    let d = [q[0] - pos[0], q[1] - pos[1], q[2] - pos[2]];
                    if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > r * r {
                        continue;
                    }
                    found += 1;
                    frame[*var as usize] = V::I(k as i32);
                    match self.block(frame, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Next | Flow::Continue => {}
                    }
                }
            }
            StmtKind::Break => return Ok(Flow::Break),
            StmtKind::Continue => return Ok(Flow::Continue),
            StmtKind::Return(v) => {
                let v = match v {
                    Some(e) => self.eval(frame, e)?,
                    None => V::Void,
                };
                return Ok(Flow::Return(v));
            }
            StmtKind::Expr(e) => {
                self.eval(frame, e)?;
            }
            StmtKind::Block(b) => return self.block(frame, b),
        }
        Ok(Flow::Next)
    }

    fn local_ty(&self, frame: &[V], id: LocalId) -> Type {
        match frame.get(id as usize) {
            Some(V::B(_)) => Type::Bool,
            Some(V::I(_)) => Type::Int,
            Some(V::Vec(_, n)) => Type::Vec(*n),
            Some(V::Mat(_, n)) => Type::Mat(*n),
            _ => Type::Float,
        }
    }

    fn position(&self, k: usize) -> [f32; 3] {
        self.input.get("P").map_or([0.0; 3], |a| a.vec3(k))
    }

    fn eval(&mut self, frame: &[V], e: &Expr) -> R<V> {
        Ok(match &e.kind {
            ExprKind::Bool(b) => V::B(*b),
            ExprKind::Int(i) => V::I(*i),
            ExprKind::Float(x) => V::F(*x),
            ExprKind::Local(id) => frame[*id as usize],
            ExprKind::Attr(id) => self.attrs[*id as usize],
            ExprKind::Global(g) => match g {
                Global::PtNum => V::I(self.index as i32),
                Global::NumPt => V::I(self.input.count as i32),
                Global::Time => V::F(self.globals.time),
                Global::TimeInc => V::F(self.globals.time_inc),
                Global::Frame => V::F(self.globals.frame),
                _ => return Err("pixel globals in a point kernel".into()),
            },
            ExprKind::Param(slot) => {
                let d = self.p.params.iter().find(|d| d.slot == *slot);
                let v = self.params.get(*slot as usize).copied().unwrap_or([0.0; 4]);
                match d.map(|d| d.ty) {
                    Some(ParamType::Int) => V::I(v[0] as i32),
                    Some(ParamType::Float) | None => V::F(v[0]),
                    Some(t) => V::Vec(v, t.components() as u8),
                }
            }
            ExprKind::Unary(op, a) => {
                let a = self.eval(frame, a)?;
                match (op, a) {
                    (UnOp::Neg, V::I(i)) => V::I(i.wrapping_neg()),
                    (UnOp::Neg, v) => map1(v, |x| -x),
                    (UnOp::Not, v) => V::B(!v.b()),
                    (UnOp::BitNot, v) => V::I(!v.i()),
                }
            }
            ExprKind::Binary(op, l, r) => {
                if *op == BinOp::And {
                    return Ok(V::B(self.eval(frame, l)?.b() && self.eval(frame, r)?.b()));
                }
                if *op == BinOp::Or {
                    return Ok(V::B(self.eval(frame, l)?.b() || self.eval(frame, r)?.b()));
                }
                let a = self.eval(frame, l)?;
                let b = self.eval(frame, r)?;
                binary(*op, a, b, l.ty, r.ty)?
            }
            ExprKind::Select(c, a, b) => {
                let c = self.eval(frame, c)?.b();
                let a = self.eval(frame, a)?;
                let b = self.eval(frame, b)?;
                if c { a } else { b }
            }
            ExprKind::Convert(a) => {
                let a = self.eval(frame, a)?;
                match e.ty {
                    Type::Float => V::F(a.f()),
                    Type::Int => V::I(a.i()),
                    Type::Bool => V::B(a.b()),
                    _ => a,
                }
            }
            ExprKind::Splat(a) => {
                let x = self.eval(frame, a)?.f();
                V::Vec([x; 4], e.ty.components())
            }
            ExprKind::Construct(args) => {
                let vals = args
                    .iter()
                    .map(|a| self.eval(frame, a))
                    .collect::<R<Vec<_>>>()?;
                match e.ty {
                    Type::Vec(n) => {
                        let mut c = [0.0f32; 4];
                        let mut k = 0;
                        for v in vals {
                            let (vc, vn) = v.comps();
                            for x in vc.iter().take(usize::from(vn)) {
                                if k < 4 {
                                    c[k] = *x;
                                }
                                k += 1;
                            }
                        }
                        V::Vec(c, n)
                    }
                    Type::Mat(n) => {
                        let mut m = [[0.0f32; 4]; 4];
                        let nn = usize::from(n);
                        if vals.len() == nn {
                            for (col, v) in vals.iter().enumerate() {
                                m[col] = v.comps().0;
                            }
                        } else {
                            for (k, v) in vals.iter().enumerate() {
                                m[k / nn][k % nn] = v.f();
                            }
                        }
                        V::Mat(m, n)
                    }
                    _ => return Err("bad constructor".into()),
                }
            }
            ExprKind::Swizzle(a, idx) => {
                let (c, _) = self.eval(frame, a)?.comps();
                let mut out = [0.0f32; 4];
                for (o, &i) in out.iter_mut().zip(idx) {
                    *o = c[usize::from(i).min(3)];
                }
                V::from_comps(out, idx.len() as u8)
            }
            ExprKind::Index(a, i) => {
                let a = self.eval(frame, a)?;
                let i = self.eval(frame, i)?.i();
                match a {
                    V::Vec(c, n) => V::F(c[i.clamp(0, i32::from(n) - 1) as usize]),
                    V::Mat(m, n) => V::Vec(m[i.clamp(0, i32::from(n) - 1) as usize], n),
                    _ => return Err("index of a scalar".into()),
                }
            }
            ExprKind::Builtin(b, args) => {
                let vals = args
                    .iter()
                    .map(|a| self.eval(frame, a))
                    .collect::<R<Vec<_>>>()?;
                builtin(*b, &vals, e.ty)?
            }
            ExprKind::Call(id, args) => {
                let vals = args
                    .iter()
                    .map(|a| self.eval(frame, a))
                    .collect::<R<Vec<_>>>()?;
                let p = self.p;
                let f = p
                    .functions
                    .get(*id as usize)
                    .ok_or_else(|| "unknown function".to_owned())?;
                self.call(f, vals)?
            }
            ExprKind::PointRead(id, i) => {
                let i = self.eval(frame, i)?.i();
                let a = &self.p.attrs[*id as usize];
                if i < 0 || i as usize >= self.input.count {
                    V::zero(a.ty)
                } else {
                    self.load(&a.name, a.storage, a.ty, i as usize)
                }
            }
            ExprKind::NumPoints => V::I(self.input.count as i32),
            ExprKind::NearPoint(pos, maxd) => {
                let pos = v3(self.eval(frame, pos)?);
                let maxd = match maxd {
                    Some(d) => self.eval(frame, d)?.f(),
                    None => -1.0,
                };
                let mut best = -1i32;
                let mut best_d2 = 0.0f32;
                for k in 0..self.input.count {
                    let q = self.position(k);
                    let d = [q[0] - pos[0], q[1] - pos[1], q[2] - pos[2]];
                    let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    if maxd >= 0.0 && d2 > maxd * maxd {
                        continue;
                    }
                    if best < 0 || d2 < best_d2 {
                        best = k as i32;
                        best_d2 = d2;
                    }
                }
                V::I(best)
            }
            ExprKind::Texture(..) => {
                return Err("texture() is not supported by the reference interpreter".into());
            }
        })
    }
}

fn binary(op: BinOp, a: V, b: V, lt: Type, rt: Type) -> R<V> {
    use BinOp as B;
    Ok(match op {
        B::Lt | B::Le | B::Gt | B::Ge | B::Eq | B::Ne => {
            let r = match (a, b) {
                (V::I(x), V::I(y)) => cmp(op, x.cmp(&y)),
                (V::B(x), V::B(y)) => match op {
                    B::Eq => x == y,
                    _ => x != y,
                },
                (V::Vec(x, n), V::Vec(y, _)) => {
                    let eq = (0..usize::from(n)).all(|i| x[i] == y[i]);
                    let ne = (0..usize::from(n)).any(|i| x[i] != y[i]);
                    if op == B::Eq { eq } else { ne }
                }
                _ => {
                    let (x, y) = (a.f(), b.f());
                    match op {
                        B::Lt => x < y,
                        B::Le => x <= y,
                        B::Gt => x > y,
                        B::Ge => x >= y,
                        B::Eq => x == y,
                        _ => x != y,
                    }
                }
            };
            V::B(r)
        }
        B::BitAnd => V::I(a.i() & b.i()),
        B::BitOr => V::I(a.i() | b.i()),
        B::BitXor => V::I(a.i() ^ b.i()),
        B::Shl => V::I(a.i().wrapping_shl(b.i() as u32 & 31)),
        B::Shr => V::I(a.i().wrapping_shr(b.i() as u32 & 31)),
        B::And => V::B(a.b() && b.b()),
        B::Or => V::B(a.b() || b.b()),
        B::Add | B::Sub | B::Mul | B::Div | B::Rem => {
            if let (V::I(x), V::I(y)) = (a, b) {
                return Ok(V::I(match op {
                    B::Add => x.wrapping_add(y),
                    B::Sub => x.wrapping_sub(y),
                    B::Mul => x.wrapping_mul(y),
                    B::Div => idiv(x, y),
                    _ => imod(x, y),
                }));
            }
            match (lt, rt) {
                (Type::Mat(n), Type::Mat(_)) => {
                    let (x, _) = a.mat();
                    let (y, _) = b.mat();
                    let mut out = [[0.0f32; 4]; 4];
                    for c in 0..usize::from(n) {
                        out[c] = match op {
                            B::Add => std::array::from_fn(|r| x[c][r] + y[c][r]),
                            B::Sub => std::array::from_fn(|r| x[c][r] - y[c][r]),
                            _ => mat_vec(x, y[c], n),
                        };
                    }
                    V::Mat(out, n)
                }
                (Type::Mat(n), Type::Vec(_)) => V::Vec(mat_vec(a.mat().0, b.comps().0, n), n),
                (Type::Vec(n), Type::Mat(_)) => {
                    let (m, _) = b.mat();
                    let (v, _) = a.comps();
                    let mut out = [0.0f32; 4];
                    for (c, o) in out.iter_mut().enumerate().take(usize::from(n)) {
                        *o = dot(v, m[c], n);
                    }
                    V::Vec(out, n)
                }
                (Type::Mat(n), _) => {
                    let s = b.f();
                    V::Mat(a.mat().0.map(|c| c.map(|x| x * s)), n)
                }
                (_, Type::Mat(n)) => {
                    let s = a.f();
                    V::Mat(b.mat().0.map(|c| c.map(|x| s * x)), n)
                }
                _ => map2(a, b, |x, y| match op {
                    B::Add => x + y,
                    B::Sub => x - y,
                    B::Mul => x * y,
                    B::Div => x / y,
                    _ => fmod(x, y),
                }),
            }
        }
    })
}

fn cmp(op: BinOp, o: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        BinOp::Lt => o == Less,
        BinOp::Le => o != Greater,
        BinOp::Gt => o == Greater,
        BinOp::Ge => o != Less,
        BinOp::Eq => o == Equal,
        _ => o != Equal,
    }
}

fn builtin(b: Builtin, a: &[V], ty: Type) -> R<V> {
    use Builtin as B;
    let x = a.first().copied().unwrap_or(V::F(0.0));
    let y = a.get(1).copied().unwrap_or(V::F(0.0));
    let z = a.get(2).copied().unwrap_or(V::F(0.0));
    Ok(match b {
        B::Abs => match x {
            V::I(i) => V::I(i.wrapping_abs()),
            v => map1(v, f32::abs),
        },
        B::Sign => match x {
            V::I(i) => V::I(i.signum()),
            v => map1(v, fsign),
        },
        B::Floor => map1(x, f32::floor),
        B::Ceil => map1(x, f32::ceil),
        B::Fract => map1(x, |v| v - v.floor()),
        B::Trunc => map1(x, f32::trunc),
        B::Round => map1(x, f32::round_ties_even),
        B::Sqrt => map1(x, f32::sqrt),
        B::Exp => map1(x, f32::exp),
        B::Log => map1(x, f32::ln),
        B::Log2 => map1(x, f32::log2),
        B::Sin => map1(x, f32::sin),
        B::Cos => map1(x, f32::cos),
        B::Tan => map1(x, f32::tan),
        B::Asin => map1(x, f32::asin),
        B::Acos => map1(x, f32::acos),
        B::Atan => map1(x, f32::atan),
        B::Radians => map1(x, f32::to_radians),
        B::Degrees => map1(x, f32::to_degrees),
        B::Atan2 => map2(x, y, f32::atan2),
        B::Pow => map2(x, y, f32::powf),
        B::Min => match (x, y) {
            (V::I(p), V::I(q)) => V::I(p.min(q)),
            _ => map2(x, y, f32::min),
        },
        B::Max => match (x, y) {
            (V::I(p), V::I(q)) => V::I(p.max(q)),
            _ => map2(x, y, f32::max),
        },
        B::Step => map2(x, y, |edge, v| if v >= edge { 1.0 } else { 0.0 }),
        B::Clamp => match (x, y, z) {
            (V::I(v), V::I(lo), V::I(hi)) => V::I(v.max(lo).min(hi)),
            _ => map3(x, y, z, |v, lo, hi| v.max(lo).min(hi)),
        },
        B::Mix => map3(x, y, z, |p, q, t| p * (1.0 - t) + q * t),
        B::Smoothstep => map3(x, y, z, smoothstep),
        B::Fit => {
            let (c, n) = x.comps();
            let o = [
                a[1].comps().0,
                a[2].comps().0,
                a[3].comps().0,
                a[4].comps().0,
            ];
            let mut out = [0.0f32; 4];
            for i in 0..usize::from(n) {
                out[i] = fit(c[i], o[0][i], o[1][i], o[2][i], o[3][i]);
            }
            V::from_comps(out, n)
        }
        B::Length => {
            let (c, n) = x.comps();
            V::F(dot(c, c, n).sqrt())
        }
        B::Length2 => {
            let (c, n) = x.comps();
            V::F(dot(c, c, n))
        }
        B::Distance | B::Distance2 => {
            let d = map2(x, y, |p, q| p - q);
            let (c, n) = d.comps();
            let d2 = dot(c, c, n);
            V::F(if b == B::Distance { d2.sqrt() } else { d2 })
        }
        B::Normalize => {
            let (c, n) = x.comps();
            V::Vec(normalize(c, n), n)
        }
        B::Dot => {
            let (p, n) = x.comps();
            V::F(dot(p, y.comps().0, n))
        }
        B::Cross => vec3(cross(v3(x), v3(y))),
        B::Transpose => {
            let (m, n) = x.mat();
            V::Mat(transpose(m, n), n)
        }
        B::Determinant => {
            let (m, n) = x.mat();
            V::F(determinant(m, n))
        }
        B::Invert => {
            let (m, n) = x.mat();
            V::Mat(
                if n == 3 {
                    inverse3(m)
                } else {
                    inverse4_parts(m).0
                },
                n,
            )
        }
        B::Ident => {
            let n = if ty == Type::Mat(3) { 3 } else { 4 };
            let mut m = [[0.0f32; 4]; 4];
            for (i, col) in m.iter_mut().enumerate().take(n) {
                col[i] = 1.0;
            }
            V::Mat(m, n as u8)
        }
        B::Quaternion => {
            let h = x.f() * 0.5;
            let (axis, _) = y.comps();
            let n = normalize(axis, 3);
            let s = h.sin();
            V::Vec([n[0] * s, n[1] * s, n[2] * s, h.cos()], 4)
        }
        B::QMultiply => V::Vec(qmultiply(x.comps().0, y.comps().0), 4),
        B::QRotate => {
            let (q, _) = x.comps();
            let qv = [q[0], q[1], q[2]];
            let v = v3(y);
            let t = cross(qv, v).map(|c| 2.0 * c);
            let c2 = cross(qv, t);
            vec3([
                v[0] + q[3] * t[0] + c2[0],
                v[1] + q[3] * t[1] + c2[1],
                v[2] + q[3] * t[2] + c2[2],
            ])
        }
        B::QInvert => {
            let (q, _) = x.comps();
            let d = dot(q, q, 4);
            if d > 0.0 {
                V::Vec([-q[0] / d, -q[1] / d, -q[2] / d, q[3] / d], 4)
            } else {
                V::Vec([0.0; 4], 4)
            }
        }
        B::QConvert => {
            let [qx, qy, qz, qw] = x.comps().0;
            V::Mat(
                [
                    [
                        1.0 - 2.0 * (qy * qy + qz * qz),
                        2.0 * (qx * qy + qz * qw),
                        2.0 * (qx * qz - qy * qw),
                        0.0,
                    ],
                    [
                        2.0 * (qx * qy - qz * qw),
                        1.0 - 2.0 * (qx * qx + qz * qz),
                        2.0 * (qy * qz + qx * qw),
                        0.0,
                    ],
                    [
                        2.0 * (qx * qz + qy * qw),
                        2.0 * (qy * qz - qx * qw),
                        1.0 - 2.0 * (qx * qx + qy * qy),
                        0.0,
                    ],
                    [0.0; 4],
                ],
                3,
            )
        }
        B::Slerp => {
            let (qa, _) = x.comps();
            let (mut qb, _) = y.comps();
            let t = z.f();
            let mut c = dot(qa, qb, 4);
            if c < 0.0 {
                qb = qb.map(|v| -v);
                c = -c;
            }
            if c > 0.9995 {
                let m: [f32; 4] = std::array::from_fn(|i| qa[i] * (1.0 - t) + qb[i] * t);
                V::Vec(normalize(m, 4), 4)
            } else {
                let th = c.acos();
                let (sa, sb, s) = (((1.0 - t) * th).sin(), (t * th).sin(), th.sin());
                V::Vec(std::array::from_fn(|i| (qa[i] * sa + qb[i] * sb) / s), 4)
            }
        }
        B::Perlin | B::Simplex | B::Worley => {
            let (c, n) = x.comps();
            V::F(match (b, n) {
                (B::Perlin, 2) => noise::perlin2([c[0], c[1]]),
                (B::Perlin, _) => noise::perlin3([c[0], c[1], c[2]]),
                (B::Simplex, 2) => noise::simplex2([c[0], c[1]]),
                (B::Simplex, _) => noise::simplex3([c[0], c[1], c[2]]),
                (_, 2) => noise::worley2([c[0], c[1]]),
                _ => noise::worley3([c[0], c[1], c[2]]),
            })
        }
        B::CurlNoise => vec3(noise::curlnoise(v3(x))),
        B::Rand => V::F(match x {
            V::I(i) => noise::rand_i(i),
            V::F(f) => noise::rand_f(f),
            V::Vec(c, 2) => noise::rand_v2([c[0], c[1]]),
            V::Vec(c, _) => noise::rand_v3([c[0], c[1], c[2]]),
            _ => 0.0,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompileOptions, Target, compile};

    fn run(src: &str, geo: &mut Geometry) {
        let c = compile(src, &CompileOptions::new(Target::Points)).expect("compiles");
        run_points(
            &c,
            geo,
            &[],
            &FrameGlobals {
                time: 1.0,
                time_inc: 0.5,
                frame: 2.0,
                ..FrameGlobals::default()
            },
        )
        .expect("runs");
    }

    fn line(n: usize) -> Geometry {
        Geometry {
            count: n,
            attributes: vec![(
                "P".into(),
                Attribute::from_vec3(&(0..n).map(|i| [i as f32, 0.0, 0.0]).collect::<Vec<_>>()),
            )],
        }
    }

    #[test]
    fn arithmetic_and_control_flow() {
        let mut g = line(4);
        run(
            "int s = 0;\nfor (int i = 0; i < @ptnum + 1; i++) { if (i == 1) continue; s += i; }\n\
             f@s = s; f@q = 7 / 2 + 7 % 3 + (-7) / 0 + 5 % 0; @P.y = @Time + @TimeInc * @Frame;",
            &mut g,
        );
        let s: Vec<f32> = (0..4).map(|i| g.get("s").unwrap().f32(i)).collect();
        assert_eq!(s, [0.0, 0.0, 2.0, 5.0]);
        // 7/2 = 3, 7%3 = 1, -7/0 = -7, 5%0 = 0
        assert_eq!(g.get("q").unwrap().f32(0), -3.0);
        assert_eq!(g.get("P").unwrap().vec3(2), [2.0, 2.0, 0.0]);
    }

    #[test]
    fn point_reads_the_input_not_the_writes() {
        let mut g = line(3);
        run("@P.x = point(0, \"P\", @ptnum + 1).x + 10;", &mut g);
        let xs: Vec<f32> = (0..3).map(|i| g.get("P").unwrap().vec3(i)[0]).collect();
        assert_eq!(xs, [11.0, 12.0, 10.0]);
    }

    #[test]
    fn matrices_invert() {
        let mut g = line(1);
        run(
            "mat4 m = ident(); m[3] = vec4(1, 2, 3, 1); m[0] = vec4(2, 0, 0, 0);\n\
             mat4 r = invert(m) * m; mat3 q = qconvert(quaternion(0.7, {1, 2, 3}));\n\
             mat3 s = invert(q) * q;\n\
             f@e = length(r[0] - vec4(1,0,0,0)) + length(r[3] - vec4(0,0,0,1)) + length(s[1] - {0,1,0});\n\
             f@d = determinant(m) + determinant(q);",
            &mut g,
        );
        assert!(g.get("e").unwrap().f32(0) < 1e-5);
        assert!((g.get("d").unwrap().f32(0) - 3.0).abs() < 1e-5);
    }
}
