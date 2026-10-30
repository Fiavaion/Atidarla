//! The shared WGSL library for geometry kernels (design §5 item 5): the
//! node-built operators (curl noise, attract) and the Kernel DSL
//! (`atd-kernel`) call the same functions, so a kernel and the equivalent
//! node chain compute the same thing. Functions are prefixed `atd_`. Plain
//! text, so it lives here where both the engine and the compiler reach it.
//!
//! Random numbers are hashes of integers (seed, element ID or index, frame),
//! with no RNG state, so every result is a function of its inputs
//! (design §4.3: determinism).

/// Integer hashing and random numbers.
///
/// - `atd_hash(x: u32) -> u32`: a 32-bit integer hash (PCG output
///   permutation of one LCG step).
/// - `atd_hash3(v: vec3u) -> vec3u`: a 3-D integer hash mixing all lanes.
/// - `atd_rand(seed: u32, i: u32) -> f32`: uniform in [0, 1).
/// - `atd_rand3(seed: u32, i: u32) -> vec3f`: three uniforms in [0, 1).
/// - `atd_in_sphere(seed: u32, i: u32) -> vec3f`: uniform in the unit ball.
pub const HASH: &str = r#"
fn atd_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn atd_hash3(v0: vec3u) -> vec3u {
    var v = v0 * 1664525u + vec3u(1013904223u);
    v.x = v.x + v.y * v.z;
    v.y = v.y + v.z * v.x;
    v.z = v.z + v.x * v.y;
    v = v ^ (v >> vec3u(16u));
    v.x = v.x + v.y * v.z;
    v.y = v.y + v.z * v.x;
    v.z = v.z + v.x * v.y;
    return v;
}

fn atd_unit(h: u32) -> f32 {
    // 24 bits: exactly representable, in [0, 1).
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

fn atd_rand(seed: u32, i: u32) -> f32 {
    return atd_unit(atd_hash(i ^ atd_hash(seed)));
}

fn atd_rand3(seed: u32, i: u32) -> vec3f {
    let h = atd_hash3(vec3u(i, seed, i ^ 0x9e3779b9u));
    return vec3f(atd_unit(h.x), atd_unit(h.y), atd_unit(h.z));
}

fn atd_in_sphere(seed: u32, i: u32) -> vec3f {
    let r = atd_rand3(seed, i);
    // Direction from two uniforms, radius from the cube root of the third.
    let z = r.x * 2.0 - 1.0;
    let a = r.y * 6.28318530718;
    let s = sqrt(max(0.0, 1.0 - z * z));
    return vec3f(s * cos(a), s * sin(a), z) * pow(r.z, 1.0 / 3.0);
}
"#;

/// Gradient noise and curl noise (needs [`HASH`]).
///
/// - `atd_gnoise(p: vec3f, seed: u32) -> vec4f`: 3-D gradient noise with
///   quintic interpolation; `.x` is the value (about −1 to 1), `.yzw` its
///   analytic gradient.
/// - `atd_curl(p: vec3f, seed: u32) -> vec3f`: the curl of a vector
///   potential of three decorrelated gradient noises: a divergence-free
///   field (particles following it swirl and do not bunch up).
pub const NOISE: &str = r#"
fn atd_grad(c: vec3i, seed: u32) -> vec3f {
    let h = atd_hash3(bitcast<vec3u>(c) ^ vec3u(seed, seed * 3u + 1u, seed * 7u + 2u));
    // Components uniform in [-1, 1].
    return vec3f(h & vec3u(0xffffu)) * (2.0 / 65535.0) - vec3f(1.0);
}

fn atd_gnoise(p: vec3f, seed: u32) -> vec4f {
    let cell = floor(p);
    let c = vec3i(cell);
    let f = p - cell;
    // Quintic fade and its derivative.
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let du = 30.0 * f * f * (f * (f - 2.0) + 1.0);
    // Corner gradients, x fastest: 000 100 010 110 001 101 011 111.
    let g0 = atd_grad(c, seed);
    let g1 = atd_grad(c + vec3i(1, 0, 0), seed);
    let g2 = atd_grad(c + vec3i(0, 1, 0), seed);
    let g3 = atd_grad(c + vec3i(1, 1, 0), seed);
    let g4 = atd_grad(c + vec3i(0, 0, 1), seed);
    let g5 = atd_grad(c + vec3i(1, 0, 1), seed);
    let g6 = atd_grad(c + vec3i(0, 1, 1), seed);
    let g7 = atd_grad(c + vec3i(1, 1, 1), seed);
    // Corner values: the gradient dotted with the offset to the corner.
    let n0 = dot(g0, f);
    let n1 = dot(g1, f - vec3f(1.0, 0.0, 0.0));
    let n2 = dot(g2, f - vec3f(0.0, 1.0, 0.0));
    let n3 = dot(g3, f - vec3f(1.0, 1.0, 0.0));
    let n4 = dot(g4, f - vec3f(0.0, 0.0, 1.0));
    let n5 = dot(g5, f - vec3f(1.0, 0.0, 1.0));
    let n6 = dot(g6, f - vec3f(0.0, 1.0, 1.0));
    let n7 = dot(g7, f - vec3f(1.0, 1.0, 1.0));
    // Trilinear interpolation written as a polynomial in u:
    // n = k0 + k1 ux + k2 uy + k3 uz + k4 ux uy + k5 uy uz + k6 ux uz + k7 ux uy uz.
    let k1 = n1 - n0;
    let k2 = n2 - n0;
    let k3 = n4 - n0;
    let k4 = n0 - n1 - n2 + n3;
    let k5 = n0 - n2 - n4 + n6;
    let k6 = n0 - n1 - n4 + n5;
    let k7 = -n0 + n1 + n2 - n3 + n4 - n5 - n6 + n7;
    let value = n0 + k1 * u.x + k2 * u.y + k3 * u.z + k4 * u.x * u.y + k5 * u.y * u.z
        + k6 * u.x * u.z + k7 * u.x * u.y * u.z;
    // The same polynomial over the corner gradients (each corner value's
    // own derivative) ...
    let gk1 = g1 - g0;
    let gk2 = g2 - g0;
    let gk3 = g4 - g0;
    let gk4 = g0 - g1 - g2 + g3;
    let gk5 = g0 - g2 - g4 + g6;
    let gk6 = g0 - g1 - g4 + g5;
    let gk7 = -g0 + g1 + g2 - g3 + g4 - g5 - g6 + g7;
    var grad = g0 + gk1 * u.x + gk2 * u.y + gk3 * u.z + gk4 * u.x * u.y + gk5 * u.y * u.z
        + gk6 * u.x * u.z + gk7 * u.x * u.y * u.z;
    // ... plus the chain rule through the fade.
    grad = grad + du * vec3f(
        k1 + k4 * u.y + k6 * u.z + k7 * u.y * u.z,
        k2 + k4 * u.x + k5 * u.z + k7 * u.x * u.z,
        k3 + k5 * u.y + k6 * u.x + k7 * u.x * u.y,
    );
    return vec4f(value, grad);
}

fn atd_curl(p: vec3f, seed: u32) -> vec3f {
    let a = atd_gnoise(p, seed).yzw;
    let b = atd_gnoise(p + vec3f(31.416, 47.853, 12.679), seed + 1u).yzw;
    let c = atd_gnoise(p + vec3f(-59.207, 23.114, -71.931), seed + 2u).yzw;
    // Potential (a, b, c): curl = (dc/dy - db/dz, da/dz - dc/dx, db/dx - da/dy).
    return vec3f(c.y - b.z, a.z - c.x, b.x - a.y);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_valid_wgsl() {
        let src = format!("{HASH}{NOISE}");
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{e}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
