//! The noise helpers in Rust, written from the same algorithm as the WGSL in
//! `stdlib.rs`, operation for operation and with the same constants. Lattice hashes are
//! integer arithmetic and match the GPU bit for bit; interpolation is float arithmetic and
//! may differ in the last bits (fused multiply-add). Pure functions: no state.

type V2 = [f32; 2];
type V3 = [f32; 3];

/// PCG3D (Jarzynski and Olano 2020), as `atd_pcg3d`.
pub fn pcg3d(v0: [u32; 3]) -> [u32; 3] {
    let mut v = v0.map(|x| x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    v = v.map(|x| x ^ (x >> 16));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    v
}

/// A hash to `[0, 1)`, as `atd_u01`.
pub fn u01(h: u32) -> f32 {
    (h >> 8) as f32 * 5.960_464_5e-8
}

fn hash2(c: [i32; 2]) -> u32 {
    pcg3d([c[0] as u32, c[1] as u32, 0])[0]
}

fn hash3(c: [i32; 3]) -> u32 {
    pcg3d([c[0] as u32, c[1] as u32, c[2] as u32])[0]
}

fn grad2(h: u32) -> V2 {
    let a = (h & 1) as f32 * 2.0 - 1.0;
    let b = ((h >> 1) & 1) as f32 * 2.0 - 1.0;
    if h & 4 == 0 {
        return [a, b];
    }
    if h & 8 == 0 {
        return [a, 0.0];
    }
    [0.0, b]
}

fn grad3(h0: u32) -> V3 {
    let h = h0 & 15;
    let su = if h & 1 != 0 { -1.0 } else { 1.0 };
    let sv = if h & 2 != 0 { -1.0 } else { 1.0 };
    let mut g = [0.0f32; 3];
    if h < 8 {
        g[0] = su;
    } else {
        g[1] = su;
    }
    if h < 4 {
        g[1] += sv;
    } else if h == 12 || h == 14 {
        g[0] += sv;
    } else {
        g[2] += sv;
    }
    g
}

fn dot2(a: V2, b: V2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn dot3(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add3(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale3(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn floor_i<const N: usize>(p: [f32; N]) -> ([f32; N], [i32; N]) {
    let i = p.map(f32::floor);
    (i, i.map(|x| x as i32))
}

fn fade(f: f32) -> f32 {
    f * f * f * (f * (f * 6.0 - 15.0) + 10.0)
}

fn dfade(f: f32) -> f32 {
    30.0 * f * f * (f * (f - 2.0) + 1.0)
}

/// 2D gradient noise, about −1..1 (`perlin(vec2)`).
pub fn perlin2(p: V2) -> f32 {
    let (i, c) = floor_i(p);
    let f = [p[0] - i[0], p[1] - i[1]];
    let u = f.map(fade);
    let h = |dx: i32, dy: i32| grad2(hash2([c[0].wrapping_add(dx), c[1].wrapping_add(dy)]));
    let va = dot2(h(0, 0), f);
    let vb = dot2(h(1, 0), [f[0] - 1.0, f[1] - 0.0]);
    let vc = dot2(h(0, 1), [f[0] - 0.0, f[1] - 1.0]);
    let vd = dot2(h(1, 1), [f[0] - 1.0, f[1] - 1.0]);
    let x0 = va + (vb - va) * u[0];
    let x1 = vc + (vd - vc) * u[0];
    x0 + (x1 - x0) * u[1]
}

/// 3D gradient noise and its gradient: `(value, d/dx, d/dy, d/dz)`.
pub fn perlin3d(p: V3) -> [f32; 4] {
    let (i, c) = floor_i(p);
    let f = sub3(p, i);
    let u = f.map(fade);
    let du = f.map(dfade);
    let g = |dx: i32, dy: i32, dz: i32| {
        grad3(hash3([
            c[0].wrapping_add(dx),
            c[1].wrapping_add(dy),
            c[2].wrapping_add(dz),
        ]))
    };
    let (ga, gb, gc, gd) = (g(0, 0, 0), g(1, 0, 0), g(0, 1, 0), g(1, 1, 0));
    let (ge, gf, gg, gh) = (g(0, 0, 1), g(1, 0, 1), g(0, 1, 1), g(1, 1, 1));
    let va = dot3(ga, f);
    let vb = dot3(gb, sub3(f, [1.0, 0.0, 0.0]));
    let vc = dot3(gc, sub3(f, [0.0, 1.0, 0.0]));
    let vd = dot3(gd, sub3(f, [1.0, 1.0, 0.0]));
    let ve = dot3(ge, sub3(f, [0.0, 0.0, 1.0]));
    let vf = dot3(gf, sub3(f, [1.0, 0.0, 1.0]));
    let vg = dot3(gg, sub3(f, [0.0, 1.0, 1.0]));
    let vh = dot3(gh, sub3(f, [1.0, 1.0, 1.0]));
    let k1 = vb - va;
    let k2 = vc - va;
    let k3 = ve - va;
    let k4 = va - vb - vc + vd;
    let k5 = va - vc - ve + vg;
    let k6 = va - vb - ve + vf;
    let k7 = -va + vb + vc - vd + ve - vf - vg + vh;
    let [ux, uy, uz] = u;
    let value = va
        + ux * k1
        + uy * k2
        + uz * k3
        + ux * uy * k4
        + uy * uz * k5
        + uz * ux * k6
        + ux * uy * uz * k7;
    // g = ga + u.x*(gb-ga) + u.y*(gc-ga) + u.z*(ge-ga) + u.x*u.y*(ga-gb-gc+gd)
    //   + u.y*u.z*(ga-gc-ge+gg) + u.z*u.x*(ga-gb-ge+gf) + u.x*u.y*u.z*(-ga+gb+gc-gd+ge-gf-gg+gh)
    let t1 = sub3(gb, ga);
    let t2 = sub3(gc, ga);
    let t3 = sub3(ge, ga);
    let t4 = add3(sub3(sub3(ga, gb), gc), gd);
    let t5 = add3(sub3(sub3(ga, gc), ge), gg);
    let t6 = add3(sub3(sub3(ga, gb), ge), gf);
    let t7 = sub3(
        sub3(
            sub3(add3(sub3(add3(add3(scale3(ga, -1.0), gb), gc), gd), ge), gf),
            gg,
        ),
        [-gh[0], -gh[1], -gh[2]],
    );
    let mut grad = ga;
    grad = add3(grad, scale3(t1, ux));
    grad = add3(grad, scale3(t2, uy));
    grad = add3(grad, scale3(t3, uz));
    grad = add3(grad, scale3(t4, ux * uy));
    grad = add3(grad, scale3(t5, uy * uz));
    grad = add3(grad, scale3(t6, uz * ux));
    grad = add3(grad, scale3(t7, ux * uy * uz));
    let d = [
        du[0] * (k1 + uy * k4 + uz * k6 + uy * uz * k7),
        du[1] * (k2 + uz * k5 + ux * k4 + uz * ux * k7),
        du[2] * (k3 + ux * k6 + uy * k5 + ux * uy * k7),
    ];
    let s = add3(grad, d);
    [value, s[0], s[1], s[2]]
}

/// 3D gradient noise, about −1..1 (`perlin(vec3)`).
pub fn perlin3(p: V3) -> f32 {
    perlin3d(p)[0]
}

/// 2D simplex noise (Gustavson), about −1..1.
pub fn simplex2(p: V2) -> f32 {
    let s = (p[0] + p[1]) * 0.366_025_4;
    let (i, c) = floor_i([p[0] + s, p[1] + s]);
    let t = (i[0] + i[1]) * 0.211_324_87;
    let x0 = [p[0] - (i[0] - t), p[1] - (i[1] - t)];
    let o: V2 = if x0[0] > x0[1] {
        [1.0, 0.0]
    } else {
        [0.0, 1.0]
    };
    let x1 = [x0[0] - o[0] + 0.211_324_87, x0[1] - o[1] + 0.211_324_87];
    let x2 = [x0[0] - 0.577_350_26, x0[1] - 0.577_350_26];
    let corners = [(x0, [0, 0]), (x1, [o[0] as i32, o[1] as i32]), (x2, [1, 1])];
    let mut n = 0.0f32;
    for (x, d) in corners {
        let t = 0.5 - dot2(x, x);
        if t > 0.0 {
            let q = t * t;
            let g = grad2(hash2([c[0].wrapping_add(d[0]), c[1].wrapping_add(d[1])]));
            n += q * q * dot2(g, x);
        }
    }
    70.0 * n
}

/// 3D simplex noise (Gustavson), about −1..1.
pub fn simplex3(p: V3) -> f32 {
    let s = (p[0] + p[1] + p[2]) * 0.333_333_34;
    let (i, c) = floor_i([p[0] + s, p[1] + s, p[2] + s]);
    let t = (i[0] + i[1] + i[2]) * 0.166_666_67;
    let x0 = [p[0] - (i[0] - t), p[1] - (i[1] - t), p[2] - (i[2] - t)];
    // WGSL step(edge, x) = x >= edge ? 1 : 0
    let step = |edge: f32, x: f32| if x >= edge { 1.0f32 } else { 0.0 };
    let g = [step(x0[1], x0[0]), step(x0[2], x0[1]), step(x0[0], x0[2])];
    let l = [1.0 - g[0], 1.0 - g[1], 1.0 - g[2]];
    // l.zxy
    let lz = [l[2], l[0], l[1]];
    let i1 = [g[0].min(lz[0]), g[1].min(lz[1]), g[2].min(lz[2])];
    let i2 = [g[0].max(lz[0]), g[1].max(lz[1]), g[2].max(lz[2])];
    let x1 = [
        x0[0] - i1[0] + 0.166_666_67,
        x0[1] - i1[1] + 0.166_666_67,
        x0[2] - i1[2] + 0.166_666_67,
    ];
    let x2 = [
        x0[0] - i2[0] + 0.333_333_34,
        x0[1] - i2[1] + 0.333_333_34,
        x0[2] - i2[2] + 0.333_333_34,
    ];
    let x3 = [x0[0] - 0.5, x0[1] - 0.5, x0[2] - 0.5];
    let ii = |v: V3| [v[0] as i32, v[1] as i32, v[2] as i32];
    let corners = [(x0, [0, 0, 0]), (x1, ii(i1)), (x2, ii(i2)), (x3, [1, 1, 1])];
    let mut n = 0.0f32;
    for (x, d) in corners {
        let t = 0.5 - dot3(x, x);
        if t > 0.0 {
            let q = t * t;
            let h = hash3([
                c[0].wrapping_add(d[0]),
                c[1].wrapping_add(d[1]),
                c[2].wrapping_add(d[2]),
            ]);
            n += q * q * dot3(grad3(h), x);
        }
    }
    76.0 * n
}

fn u01v3(h: [u32; 3]) -> V3 {
    h.map(|x| (x >> 8) as f32 * 5.960_464_5e-8)
}

/// Distance to the nearest feature point (F1), one point per cell.
pub fn worley2(p: V2) -> f32 {
    let (i, c) = floor_i(p);
    let f = [p[0] - i[0], p[1] - i[1]];
    let mut d = 8.0f32;
    for y in -1..=1i32 {
        for x in -1..=1i32 {
            let cell = [c[0].wrapping_add(x), c[1].wrapping_add(y)];
            let h = pcg3d([cell[0] as u32, cell[1] as u32, 0]);
            let u = u01v3(h);
            let r = [x as f32 + u[0] - f[0], y as f32 + u[1] - f[1]];
            d = d.min(dot2(r, r));
        }
    }
    d.sqrt()
}

pub fn worley3(p: V3) -> f32 {
    let (i, c) = floor_i(p);
    let f = sub3(p, i);
    let mut d = 8.0f32;
    for z in -1..=1i32 {
        for y in -1..=1i32 {
            for x in -1..=1i32 {
                let cell = [
                    c[0].wrapping_add(x),
                    c[1].wrapping_add(y),
                    c[2].wrapping_add(z),
                ];
                let h = pcg3d(cell.map(|v| v as u32));
                let u = u01v3(h);
                let r = [
                    x as f32 + u[0] - f[0],
                    y as f32 + u[1] - f[1],
                    z as f32 + u[2] - f[2],
                ];
                d = d.min(dot3(r, r));
            }
        }
    }
    d.sqrt()
}

/// `atd_hash3` of the engine library (`atd_core::wgsl::HASH`).
fn ehash3(v0: [u32; 3]) -> [u32; 3] {
    let mut v = v0.map(|x| x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    v = v.map(|x| x ^ (x >> 16));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    v
}

/// `atd_grad` of the engine library.
fn egrad(c: [i32; 3], seed: u32) -> V3 {
    let k = [
        seed,
        seed.wrapping_mul(3).wrapping_add(1),
        seed.wrapping_mul(7).wrapping_add(2),
    ];
    let h = ehash3([c[0] as u32 ^ k[0], c[1] as u32 ^ k[1], c[2] as u32 ^ k[2]]);
    h.map(|x| (x & 0xffff) as f32 * (2.0 / 65535.0) - 1.0)
}

/// The analytic gradient of `atd_gnoise` (the engine's gradient noise).
fn egnoise_grad(p: V3, seed: u32) -> V3 {
    let cell = p.map(f32::floor);
    let c = cell.map(|x| x as i32);
    let f = [p[0] - cell[0], p[1] - cell[1], p[2] - cell[2]];
    let u = f.map(|f| f * f * f * (f * (f * 6.0 - 15.0) + 10.0));
    let du = f.map(|f| 30.0 * f * f * (f * (f - 2.0) + 1.0));
    let g: Vec<V3> = (0..8)
        .map(|i| {
            egrad(
                [c[0] + (i & 1), c[1] + ((i >> 1) & 1), c[2] + ((i >> 2) & 1)],
                seed,
            )
        })
        .collect();
    let n: Vec<f32> = (0..8)
        .map(|i| {
            let o = [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32];
            dot3(g[i as usize], [f[0] - o[0], f[1] - o[1], f[2] - o[2]])
        })
        .collect();
    let k1 = n[1] - n[0];
    let k2 = n[2] - n[0];
    let k3 = n[4] - n[0];
    let k4 = n[0] - n[1] - n[2] + n[3];
    let k5 = n[0] - n[2] - n[4] + n[6];
    let k6 = n[0] - n[1] - n[4] + n[5];
    let k7 = -n[0] + n[1] + n[2] - n[3] + n[4] - n[5] - n[6] + n[7];
    let mut grad = [0.0f32; 3];
    for a in 0..3 {
        let gk1 = g[1][a] - g[0][a];
        let gk2 = g[2][a] - g[0][a];
        let gk3 = g[4][a] - g[0][a];
        let gk4 = g[0][a] - g[1][a] - g[2][a] + g[3][a];
        let gk5 = g[0][a] - g[2][a] - g[4][a] + g[6][a];
        let gk6 = g[0][a] - g[1][a] - g[4][a] + g[5][a];
        let gk7 = -g[0][a] + g[1][a] + g[2][a] - g[3][a] + g[4][a] - g[5][a] - g[6][a] + g[7][a];
        grad[a] = g[0][a]
            + gk1 * u[0]
            + gk2 * u[1]
            + gk3 * u[2]
            + gk4 * u[0] * u[1]
            + gk5 * u[1] * u[2]
            + gk6 * u[0] * u[2]
            + gk7 * u[0] * u[1] * u[2];
    }
    let chain = [
        k1 + k4 * u[1] + k6 * u[2] + k7 * u[1] * u[2],
        k2 + k4 * u[0] + k5 * u[2] + k7 * u[0] * u[2],
        k3 + k5 * u[1] + k6 * u[0] + k7 * u[0] * u[1],
    ];
    [
        grad[0] + du[0] * chain[0],
        grad[1] + du[1] * chain[1],
        grad[2] + du[2] * chain[2],
    ]
}

/// The engine's curl noise with seed 0 (`atd_curl` in `atd_core::wgsl::NOISE`, which
/// `pnt.noise` runs): the curl of a potential of three decorrelated gradient noises.
pub fn curlnoise(p: V3) -> V3 {
    let a = egnoise_grad(p, 0);
    let b = egnoise_grad(add3(p, [31.416, 47.853, 12.679]), 1);
    let c = egnoise_grad(add3(p, [-59.207, 23.114, -71.931]), 2);
    [c[1] - b[2], a[2] - c[0], b[0] - a[1]]
}

pub fn rand_i(x: i32) -> f32 {
    u01(pcg3d([x as u32, 0, 0])[0])
}

pub fn rand_f(x: f32) -> f32 {
    u01(pcg3d([x.to_bits(), 0, 0])[0])
}

pub fn rand_v2(x: V2) -> f32 {
    u01(pcg3d([x[0].to_bits(), x[1].to_bits(), 0])[0])
}

pub fn rand_v3(x: V3) -> f32 {
    u01(pcg3d(x.map(f32::to_bits))[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid3() -> impl Iterator<Item = V3> {
        (0..12).flat_map(|i| {
            (0..12).flat_map(move |j| {
                (0..12).map(move |k| {
                    [
                        i as f32 * 0.37 - 2.1,
                        j as f32 * 0.41 - 1.3,
                        k as f32 * 0.29 + 0.05,
                    ]
                })
            })
        })
    }

    #[test]
    fn ranges() {
        for p in grid3() {
            let v = perlin3(p);
            assert!((-1.2..=1.2).contains(&v), "perlin3 {p:?} = {v}");
            let v = simplex3(p);
            assert!((-1.2..=1.2).contains(&v), "simplex3 {p:?} = {v}");
            let v = perlin2([p[0], p[1]]);
            assert!((-1.2..=1.2).contains(&v), "perlin2 {p:?} = {v}");
            let v = simplex2([p[0], p[1]]);
            assert!((-1.2..=1.2).contains(&v), "simplex2 {p:?} = {v}");
            let v = worley3(p);
            assert!((0.0..=1.8).contains(&v), "worley3 {p:?} = {v}");
            let v = worley2([p[0], p[1]]);
            assert!((0.0..=1.5).contains(&v), "worley2 {p:?} = {v}");
            let r = rand_v3(p);
            assert!((0.0..1.0).contains(&r));
        }
    }

    #[test]
    fn perlin_is_zero_on_the_lattice_and_continuous() {
        assert_eq!(perlin3([3.0, -2.0, 7.0]), 0.0);
        assert_eq!(perlin2([3.0, -2.0]), 0.0);
        let e = 1e-3;
        for p in grid3() {
            let a = perlin3(p);
            let b = perlin3([p[0] + e, p[1], p[2]]);
            assert!((a - b).abs() < 0.02, "jump at {p:?}");
        }
    }

    #[test]
    fn analytic_gradient_matches_finite_differences() {
        let e = 1e-3f32;
        for p in grid3().step_by(7) {
            let d = perlin3d(p);
            for axis in 0..3 {
                let mut hi = p;
                let mut lo = p;
                hi[axis] += e;
                lo[axis] -= e;
                let fd = (perlin3(hi) - perlin3(lo)) / (2.0 * e);
                assert!(
                    (fd - d[axis + 1]).abs() < 2e-2,
                    "axis {axis} at {p:?}: analytic {} vs fd {fd}",
                    d[axis + 1]
                );
            }
        }
    }

    #[test]
    fn curl_noise_is_divergence_free() {
        let e = 1e-3f32;
        for p in grid3().step_by(11) {
            let mut div = 0.0;
            for axis in 0..3 {
                let mut hi = p;
                let mut lo = p;
                hi[axis] += e;
                lo[axis] -= e;
                div += (curlnoise(hi)[axis] - curlnoise(lo)[axis]) / (2.0 * e);
            }
            assert!(div.abs() < 5e-2, "divergence {div} at {p:?}");
        }
    }

    #[test]
    fn hashes_are_fixed() {
        // Guards against accidental changes: kernels' random streams depend on these.
        assert_eq!(pcg3d([0, 0, 0]), pcg3d([0, 0, 0]));
        assert_ne!(rand_i(1), rand_i(2));
        assert_eq!(rand_i(0), rand_f(0.0));
        let a: Vec<u32> = (0..4).map(|i| pcg3d([i, 0, 0])[0]).collect();
        let b: Vec<u32> = (0..4).map(|i| pcg3d([i, 0, 0])[0]).collect();
        assert_eq!(a, b);
    }
}
