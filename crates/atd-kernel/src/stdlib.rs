//! The WGSL helper library. Each helper is emitted only when a kernel uses it (with its
//! dependencies), always in the order of [`Lib`], so the output is stable text.
//!
//! The noise helpers mirror `crate::noise` operation for operation: change both together
//! (the GPU parity test compares them).

use std::collections::BTreeSet;

use crate::types::Type;

/// A library function. The derived order is the emission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Lib {
    Pcg3d,
    U01,
    U01v3,
    Hash2,
    Hash3,
    Grad2,
    Grad3,
    Idiv,
    Imod,
    /// Componentwise helpers over `f32` (`Type::Float`) or a float vector.
    Normalize(Type),
    Length2(Type),
    Smoothstep(Type),
    Fit(Type),
    Col(u8),
    Inverse3,
    Inverse4,
    Quaternion,
    QMultiply,
    QRotate,
    QInvert,
    QConvert,
    Slerp,
    Perlin2,
    Perlin3d,
    Perlin3,
    Simplex2,
    Simplex3,
    Worley2,
    Worley3,
    Curl,
    RandI,
    RandF,
    Rand2,
    Rand3,
}

impl Lib {
    pub fn name(self) -> String {
        let t = |t: Type| t.wgsl().to_owned();
        match self {
            Lib::Pcg3d => "atd_pcg3d".into(),
            Lib::U01 => "atd_u01".into(),
            Lib::U01v3 => "atd_u01v3".into(),
            Lib::Hash2 => "atd_hash2".into(),
            Lib::Hash3 => "atd_ihash3".into(),
            Lib::Grad2 => "atd_grad2".into(),
            Lib::Grad3 => "atd_grad3".into(),
            Lib::Idiv => "atd_idiv".into(),
            Lib::Imod => "atd_imod".into(),
            Lib::Normalize(ty) => format!("atd_normalize_{}", t(ty)),
            Lib::Length2(ty) => format!("atd_length2_{}", t(ty)),
            Lib::Smoothstep(ty) => format!("atd_smoothstep_{}", t(ty)),
            Lib::Fit(ty) => format!("atd_fit_{}", t(ty)),
            Lib::Col(n) => format!("atd_col{n}"),
            Lib::Inverse3 => "atd_inverse3".into(),
            Lib::Inverse4 => "atd_inverse4".into(),
            Lib::Quaternion => "atd_quaternion".into(),
            Lib::QMultiply => "atd_qmultiply".into(),
            Lib::QRotate => "atd_qrotate".into(),
            Lib::QInvert => "atd_qinvert".into(),
            Lib::QConvert => "atd_qconvert".into(),
            Lib::Slerp => "atd_slerp".into(),
            Lib::Perlin2 => "atd_perlin2".into(),
            Lib::Perlin3d => "atd_perlin3d".into(),
            Lib::Perlin3 => "atd_perlin3".into(),
            Lib::Simplex2 => "atd_simplex2".into(),
            Lib::Simplex3 => "atd_simplex3".into(),
            Lib::Worley2 => "atd_worley2".into(),
            Lib::Worley3 => "atd_worley3".into(),
            Lib::Curl => "atd_curlnoise".into(),
            Lib::RandI => "atd_rand_i".into(),
            Lib::RandF => "atd_rand_f".into(),
            Lib::Rand2 => "atd_rand_v2".into(),
            Lib::Rand3 => "atd_rand_v3".into(),
        }
    }

    fn deps(self) -> &'static [Lib] {
        match self {
            Lib::Hash2 | Lib::Hash3 => &[Lib::Pcg3d],
            Lib::Quaternion => &[Lib::Normalize(Type::Vec(3))],
            Lib::Slerp => &[Lib::Normalize(Type::Vec(4))],
            Lib::Perlin2 => &[Lib::Hash2, Lib::Grad2],
            Lib::Perlin3d => &[Lib::Hash3, Lib::Grad3],
            Lib::Perlin3 => &[Lib::Perlin3d],
            Lib::Simplex2 => &[Lib::Hash2, Lib::Grad2],
            Lib::Simplex3 => &[Lib::Hash3, Lib::Grad3],
            Lib::Worley2 | Lib::Worley3 => &[Lib::Pcg3d, Lib::U01v3],
            Lib::RandI | Lib::RandF | Lib::Rand2 | Lib::Rand3 => &[Lib::Pcg3d, Lib::U01],
            _ => &[],
        }
    }

    fn code(self) -> String {
        match self {
            Lib::Normalize(ty) => {
                let t = ty.wgsl();
                format!(
                    "fn atd_normalize_{t}(v: {t}) -> {t} {{\n    let l = length(v);\n    return select({t}(), v / l, l > 0.0);\n}}\n"
                )
            }
            Lib::Length2(ty) => {
                let t = ty.wgsl();
                format!("fn atd_length2_{t}(v: {t}) -> f32 {{\n    return dot(v, v);\n}}\n")
            }
            Lib::Smoothstep(ty) => {
                let t = ty.wgsl();
                format!(
                    "fn atd_smoothstep_{t}(lo: {t}, hi: {t}, x: {t}) -> {t} {{\n    let d = hi - lo;\n    let z = d == {t}(0.0);\n    let t = select(clamp((x - lo) / select(d, {t}(1.0), z), {t}(0.0), {t}(1.0)), step(lo, x), z);\n    return t * t * ({t}(3.0) - {t}(2.0) * t);\n}}\n"
                )
            }
            Lib::Fit(ty) => {
                let t = ty.wgsl();
                format!(
                    "fn atd_fit_{t}(x: {t}, omin: {t}, omax: {t}, nmin: {t}, nmax: {t}) -> {t} {{\n    let d = omax - omin;\n    let z = d == {t}(0.0);\n    let t = select(clamp((x - omin) / select(d, {t}(1.0), z), {t}(0.0), {t}(1.0)), {t}(0.5), z);\n    return nmin + (nmax - nmin) * t;\n}}\n"
                )
            }
            Lib::Col(n) => {
                let t = Type::Mat(n).wgsl();
                let v = Type::Vec(n).wgsl();
                let hi = n - 1;
                format!(
                    "fn atd_col{n}(m: {t}, i: i32) -> {v} {{\n    var c = m;\n    return c[clamp(i, 0i, {hi}i)];\n}}\n"
                )
            }
            Lib::Curl => format!(
                "{}{}// The engine's curl noise (seed 0), the same code `pnt.noise` runs.
    fn atd_curlnoise(p: vec3f) -> vec3f {{
        return atd_curl(p, 0u);
    }}
    ",
                atd_core::wgsl::HASH,
                atd_core::wgsl::NOISE
            ),
            other => fixed(other).to_owned(),
        }
    }
}

fn fixed(lib: Lib) -> &'static str {
    match lib {
        Lib::Pcg3d => {
            "// PCG3D hash (Jarzynski and Olano 2020).
fn atd_pcg3d(v0: vec3u) -> vec3u {
    var v = v0 * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3u(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}
"
        }
        Lib::U01 => {
            "fn atd_u01(h: u32) -> f32 {
    return f32(h >> 8u) * 5.9604645e-8f;
}
"
        }
        Lib::U01v3 => {
            "fn atd_u01v3(h: vec3u) -> vec3f {
    return vec3f(h >> vec3u(8u)) * 5.9604645e-8f;
}
"
        }
        Lib::Hash2 => {
            "fn atd_hash2(c: vec2i) -> u32 {
    return atd_pcg3d(vec3u(bitcast<vec2u>(c), 0u)).x;
}
"
        }
        Lib::Hash3 => {
            "fn atd_ihash3(c: vec3i) -> u32 {
    return atd_pcg3d(bitcast<vec3u>(c)).x;
}
"
        }
        Lib::Grad2 => {
            "fn atd_grad2(h: u32) -> vec2f {
    let a = f32(h & 1u) * 2.0f - 1.0f;
    let b = f32((h >> 1u) & 1u) * 2.0f - 1.0f;
    if ((h & 4u) == 0u) {
        return vec2f(a, b);
    }
    if ((h & 8u) == 0u) {
        return vec2f(a, 0.0f);
    }
    return vec2f(0.0f, b);
}
"
        }
        Lib::Grad3 => {
            "// The 12 cube-edge gradients of improved Perlin noise (16 slots).
fn atd_grad3(h0: u32) -> vec3f {
    let h = h0 & 15u;
    let su = select(1.0f, -1.0f, (h & 1u) != 0u);
    let sv = select(1.0f, -1.0f, (h & 2u) != 0u);
    var g = vec3f(0.0f);
    if (h < 8u) {
        g.x = su;
    } else {
        g.y = su;
    }
    if (h < 4u) {
        g.y = g.y + sv;
    } else if (h == 12u || h == 14u) {
        g.x = g.x + sv;
    } else {
        g.z = g.z + sv;
    }
    return g;
}
"
        }
        Lib::Idiv => {
            "// Integer division that never faults: x / 0 = x, x / -1 = -x (wrapping).
fn atd_idiv(a: i32, b: i32) -> i32 {
    if (b == 0i) {
        return a;
    }
    if (b == -1i) {
        return 0i - a;
    }
    return a / b;
}
"
        }
        Lib::Imod => {
            "fn atd_imod(a: i32, b: i32) -> i32 {
    if (b == 0i || b == -1i) {
        return 0i;
    }
    return a % b;
}
"
        }
        Lib::Inverse3 => {
            "fn atd_inverse3(m: mat3x3f) -> mat3x3f {
    let r0 = cross(m[1], m[2]);
    let r1 = cross(m[2], m[0]);
    let r2 = cross(m[0], m[1]);
    let det = dot(m[0], r0);
    if (det == 0.0f) {
        return mat3x3f();
    }
    return transpose(mat3x3f(r0, r1, r2)) * (1.0f / det);
}
"
        }
        Lib::Inverse4 => {
            "fn atd_inverse4(m: mat4x4f) -> mat4x4f {
    let a00 = m[0].x; let a01 = m[0].y; let a02 = m[0].z; let a03 = m[0].w;
    let a10 = m[1].x; let a11 = m[1].y; let a12 = m[1].z; let a13 = m[1].w;
    let a20 = m[2].x; let a21 = m[2].y; let a22 = m[2].z; let a23 = m[2].w;
    let a30 = m[3].x; let a31 = m[3].y; let a32 = m[3].z; let a33 = m[3].w;
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
    if (det == 0.0f) {
        return mat4x4f();
    }
    let s = 1.0f / det;
    return mat4x4f(
        vec4f(a11 * b11 - a12 * b10 + a13 * b09, a02 * b10 - a01 * b11 - a03 * b09, a31 * b05 - a32 * b04 + a33 * b03, a22 * b04 - a21 * b05 - a23 * b03) * s,
        vec4f(a12 * b08 - a10 * b11 - a13 * b07, a00 * b11 - a02 * b08 + a03 * b07, a32 * b02 - a30 * b05 - a33 * b01, a20 * b05 - a22 * b02 + a23 * b01) * s,
        vec4f(a10 * b10 - a11 * b08 + a13 * b06, a01 * b08 - a00 * b10 - a03 * b06, a30 * b04 - a31 * b02 + a33 * b00, a21 * b02 - a20 * b04 - a23 * b00) * s,
        vec4f(a11 * b07 - a10 * b09 - a12 * b06, a00 * b09 - a01 * b07 + a02 * b06, a31 * b01 - a30 * b03 - a32 * b00, a20 * b03 - a21 * b01 + a22 * b00) * s);
}
"
        }
        Lib::Quaternion => {
            "fn atd_quaternion(angle: f32, axis: vec3f) -> vec4f {
    let h = angle * 0.5f;
    return vec4f(atd_normalize_vec3f(axis) * sin(h), cos(h));
}
"
        }
        Lib::QMultiply => {
            "fn atd_qmultiply(a: vec4f, b: vec4f) -> vec4f {
    return vec4f(a.w * b.xyz + b.w * a.xyz + cross(a.xyz, b.xyz), a.w * b.w - dot(a.xyz, b.xyz));
}
"
        }
        Lib::QRotate => {
            "fn atd_qrotate(q: vec4f, v: vec3f) -> vec3f {
    let t = 2.0f * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}
"
        }
        Lib::QInvert => {
            "fn atd_qinvert(q: vec4f) -> vec4f {
    let d = dot(q, q);
    return select(vec4f(), vec4f(-q.xyz, q.w) / d, d > 0.0f);
}
"
        }
        Lib::QConvert => {
            "fn atd_qconvert(q: vec4f) -> mat3x3f {
    let x = q.x;
    let y = q.y;
    let z = q.z;
    let w = q.w;
    return mat3x3f(
        vec3f(1.0f - 2.0f * (y * y + z * z), 2.0f * (x * y + z * w), 2.0f * (x * z - y * w)),
        vec3f(2.0f * (x * y - z * w), 1.0f - 2.0f * (x * x + z * z), 2.0f * (y * z + x * w)),
        vec3f(2.0f * (x * z + y * w), 2.0f * (y * z - x * w), 1.0f - 2.0f * (x * x + y * y)));
}
"
        }
        Lib::Slerp => {
            "fn atd_slerp(a: vec4f, b0: vec4f, t: f32) -> vec4f {
    var b = b0;
    var c = dot(a, b);
    if (c < 0.0f) {
        b = -b;
        c = -c;
    }
    if (c > 0.9995f) {
        return atd_normalize_vec4f(mix(a, b, vec4f(t)));
    }
    let th = acos(c);
    return (a * sin((1.0f - t) * th) + b * sin(t * th)) / sin(th);
}
"
        }
        Lib::Perlin2 => {
            "// Gradient noise, quintic fade, about -1..1.
fn atd_perlin2(p: vec2f) -> f32 {
    let i = floor(p);
    let f = p - i;
    let c = vec2i(i);
    let u = f * f * f * (f * (f * 6.0f - 15.0f) + 10.0f);
    let va = dot(atd_grad2(atd_hash2(c)), f);
    let vb = dot(atd_grad2(atd_hash2(c + vec2i(1i, 0i))), f - vec2f(1.0f, 0.0f));
    let vc = dot(atd_grad2(atd_hash2(c + vec2i(0i, 1i))), f - vec2f(0.0f, 1.0f));
    let vd = dot(atd_grad2(atd_hash2(c + vec2i(1i, 1i))), f - vec2f(1.0f, 1.0f));
    let x0 = va + (vb - va) * u.x;
    let x1 = vc + (vd - vc) * u.x;
    return x0 + (x1 - x0) * u.y;
}
"
        }
        Lib::Perlin3d => {
            "// 3D gradient noise and its analytic gradient: (value, d/dx, d/dy, d/dz).
fn atd_perlin3d(p: vec3f) -> vec4f {
    let i = floor(p);
    let f = p - i;
    let c = vec3i(i);
    let u = f * f * f * (f * (f * 6.0f - 15.0f) + 10.0f);
    let du = 30.0f * f * f * (f * (f - 2.0f) + 1.0f);
    let ga = atd_grad3(atd_ihash3(c));
    let gb = atd_grad3(atd_ihash3(c + vec3i(1i, 0i, 0i)));
    let gc = atd_grad3(atd_ihash3(c + vec3i(0i, 1i, 0i)));
    let gd = atd_grad3(atd_ihash3(c + vec3i(1i, 1i, 0i)));
    let ge = atd_grad3(atd_ihash3(c + vec3i(0i, 0i, 1i)));
    let gf = atd_grad3(atd_ihash3(c + vec3i(1i, 0i, 1i)));
    let gg = atd_grad3(atd_ihash3(c + vec3i(0i, 1i, 1i)));
    let gh = atd_grad3(atd_ihash3(c + vec3i(1i, 1i, 1i)));
    let va = dot(ga, f);
    let vb = dot(gb, f - vec3f(1.0f, 0.0f, 0.0f));
    let vc = dot(gc, f - vec3f(0.0f, 1.0f, 0.0f));
    let vd = dot(gd, f - vec3f(1.0f, 1.0f, 0.0f));
    let ve = dot(ge, f - vec3f(0.0f, 0.0f, 1.0f));
    let vf = dot(gf, f - vec3f(1.0f, 0.0f, 1.0f));
    let vg = dot(gg, f - vec3f(0.0f, 1.0f, 1.0f));
    let vh = dot(gh, f - vec3f(1.0f, 1.0f, 1.0f));
    let k1 = vb - va;
    let k2 = vc - va;
    let k3 = ve - va;
    let k4 = va - vb - vc + vd;
    let k5 = va - vc - ve + vg;
    let k6 = va - vb - ve + vf;
    let k7 = -va + vb + vc - vd + ve - vf - vg + vh;
    let value = va + u.x * k1 + u.y * k2 + u.z * k3 + u.x * u.y * k4 + u.y * u.z * k5 + u.z * u.x * k6 + u.x * u.y * u.z * k7;
    let g = ga + u.x * (gb - ga) + u.y * (gc - ga) + u.z * (ge - ga) + u.x * u.y * (ga - gb - gc + gd) + u.y * u.z * (ga - gc - ge + gg) + u.z * u.x * (ga - gb - ge + gf) + u.x * u.y * u.z * (-ga + gb + gc - gd + ge - gf - gg + gh);
    let d = du * vec3f(k1 + u.y * k4 + u.z * k6 + u.y * u.z * k7, k2 + u.z * k5 + u.x * k4 + u.z * u.x * k7, k3 + u.x * k6 + u.y * k5 + u.x * u.y * k7);
    return vec4f(value, g + d);
}
"
        }
        Lib::Perlin3 => {
            "fn atd_perlin3(p: vec3f) -> f32 {
    return atd_perlin3d(p).x;
}
"
        }
        Lib::Simplex2 => {
            "// 2D simplex noise (Gustavson), about -1..1.
fn atd_simplex2(p: vec2f) -> f32 {
    let s = (p.x + p.y) * 0.3660254f;
    let i = floor(p + vec2f(s));
    let t = (i.x + i.y) * 0.21132487f;
    let x0 = p - (i - vec2f(t));
    let o = select(vec2f(0.0f, 1.0f), vec2f(1.0f, 0.0f), x0.x > x0.y);
    let x1 = x0 - o + vec2f(0.21132487f);
    let x2 = x0 - vec2f(0.57735026f);
    let c = vec2i(i);
    var n = 0.0f;
    let t0 = 0.5f - dot(x0, x0);
    if (t0 > 0.0f) {
        let q = t0 * t0;
        n = n + q * q * dot(atd_grad2(atd_hash2(c)), x0);
    }
    let t1 = 0.5f - dot(x1, x1);
    if (t1 > 0.0f) {
        let q = t1 * t1;
        n = n + q * q * dot(atd_grad2(atd_hash2(c + vec2i(o))), x1);
    }
    let t2 = 0.5f - dot(x2, x2);
    if (t2 > 0.0f) {
        let q = t2 * t2;
        n = n + q * q * dot(atd_grad2(atd_hash2(c + vec2i(1i, 1i))), x2);
    }
    return 70.0f * n;
}
"
        }
        Lib::Simplex3 => {
            "// 3D simplex noise (Gustavson), about -1..1.
fn atd_simplex3(p: vec3f) -> f32 {
    let s = (p.x + p.y + p.z) * 0.33333334f;
    let i = floor(p + vec3f(s));
    let t = (i.x + i.y + i.z) * 0.16666667f;
    let x0 = p - (i - vec3f(t));
    let g = step(x0.yzx, x0.xyz);
    let l = vec3f(1.0f) - g;
    let i1 = min(g.xyz, l.zxy);
    let i2 = max(g.xyz, l.zxy);
    let x1 = x0 - i1 + vec3f(0.16666667f);
    let x2 = x0 - i2 + vec3f(0.33333334f);
    let x3 = x0 - vec3f(0.5f);
    let c = vec3i(i);
    var n = 0.0f;
    let t0 = 0.5f - dot(x0, x0);
    if (t0 > 0.0f) {
        let q = t0 * t0;
        n = n + q * q * dot(atd_grad3(atd_ihash3(c)), x0);
    }
    let t1 = 0.5f - dot(x1, x1);
    if (t1 > 0.0f) {
        let q = t1 * t1;
        n = n + q * q * dot(atd_grad3(atd_ihash3(c + vec3i(i1))), x1);
    }
    let t2 = 0.5f - dot(x2, x2);
    if (t2 > 0.0f) {
        let q = t2 * t2;
        n = n + q * q * dot(atd_grad3(atd_ihash3(c + vec3i(i2))), x2);
    }
    let t3 = 0.5f - dot(x3, x3);
    if (t3 > 0.0f) {
        let q = t3 * t3;
        n = n + q * q * dot(atd_grad3(atd_ihash3(c + vec3i(1i, 1i, 1i))), x3);
    }
    return 76.0f * n;
}
"
        }
        Lib::Worley2 => {
            "// Distance to the nearest feature point (F1), one point per cell.
fn atd_worley2(p: vec2f) -> f32 {
    let i = floor(p);
    let f = p - i;
    let c = vec2i(i);
    var d = 8.0f;
    for (var y = -1i; y <= 1i; y++) {
        for (var x = -1i; x <= 1i; x++) {
            let o = vec2i(x, y);
            let h = atd_pcg3d(vec3u(bitcast<vec2u>(c + o), 0u));
            let r = vec2f(o) + atd_u01v3(h).xy - f;
            d = min(d, dot(r, r));
        }
    }
    return sqrt(d);
}
"
        }
        Lib::Worley3 => {
            "fn atd_worley3(p: vec3f) -> f32 {
    let i = floor(p);
    let f = p - i;
    let c = vec3i(i);
    var d = 8.0f;
    for (var z = -1i; z <= 1i; z++) {
        for (var y = -1i; y <= 1i; y++) {
            for (var x = -1i; x <= 1i; x++) {
                let o = vec3i(x, y, z);
                let h = atd_pcg3d(bitcast<vec3u>(c + o));
                let r = vec3f(o) + atd_u01v3(h) - f;
                d = min(d, dot(r, r));
            }
        }
    }
    return sqrt(d);
}
"
        }
        Lib::RandI => {
            "fn atd_rand_i(x: i32) -> f32 {
    return atd_u01(atd_pcg3d(vec3u(bitcast<u32>(x), 0u, 0u)).x);
}
"
        }
        Lib::RandF => {
            "fn atd_rand_f(x: f32) -> f32 {
    return atd_u01(atd_pcg3d(vec3u(bitcast<u32>(x), 0u, 0u)).x);
}
"
        }
        Lib::Rand2 => {
            "fn atd_rand_v2(x: vec2f) -> f32 {
    return atd_u01(atd_pcg3d(vec3u(bitcast<vec2u>(x), 0u)).x);
}
"
        }
        Lib::Rand3 => {
            "fn atd_rand_v3(x: vec3f) -> f32 {
    return atd_u01(atd_pcg3d(bitcast<vec3u>(x)).x);
}
"
        }
        _ => "",
    }
}

/// The text of `used` and everything they depend on, in [`Lib`] order.
pub(crate) fn emit(used: &BTreeSet<Lib>) -> String {
    let mut all = BTreeSet::new();
    let mut stack: Vec<Lib> = used.iter().copied().collect();
    while let Some(l) = stack.pop() {
        if all.insert(l) {
            stack.extend(l.deps().iter().copied());
        }
    }
    let mut out = String::new();
    for l in all {
        out.push('\n');
        out.push_str(&l.code());
    }
    out
}

/// The whole noise library as one WGSL module fragment, for operators that want the same
/// functions as kernels (design §5.5: node-built curl noise and a kernel must call the same
/// code for AC2). Names are prefixed `atd_`.
pub fn noise_library() -> String {
    let used: BTreeSet<Lib> = [
        Lib::Perlin2,
        Lib::Perlin3,
        Lib::Simplex2,
        Lib::Simplex3,
        Lib::Worley2,
        Lib::Worley3,
        Lib::Curl,
        Lib::RandI,
        Lib::RandF,
        Lib::Rand2,
        Lib::Rand3,
    ]
    .into_iter()
    .collect();
    emit(&used)
}
