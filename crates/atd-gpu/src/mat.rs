//! The `MAT` value carried on material wires (PRD §5.4: "material handle"),
//! produced by the MAT operators and read by the renderer (`img.render`).
//!
//! **Contract** (docs/milestones/m2/mat-render.md):
//!
//! - A [`Material`] is small plain data: a shading [`Model`], a fixed
//!   uniform block of colours and numbers ([`Material::pack`], the WGSL
//!   struct [`WGSL`]) and at most one texture. MAT operators fill it from
//!   their parameters on the CPU; nothing is allocated on the GPU per
//!   material.
//! - **The texture is not held by the material.** A MAT operator's first
//!   IMG input socket is its texture; the engine records the node feeding
//!   it ([`Material::texture_source`]) and a renderer looks the image up when
//!   it draws ([`crate::engine::MatInputs::texture`]), so a material never
//!   keeps a pooled texture that has since been reused.
//! - **Shading is one WGSL function**, [`SHADE`]'s `shade()`, that switches
//!   on the model ID. A renderer prepends [`WGSL`] and [`SHADE`] to its
//!   shaders, so every renderer shades a model the same way.
//! - **Adding a model:** a new [`Model`] variant with the next ID, a branch
//!   in `shade()` that reads only the fields of `Material` (`colour`,
//!   `emission`, `colour2`, `params`, `pbr`), and a MAT operator that sets
//!   them. A WGSL Material (PRD §6) or a Kernel-built one will instead bring
//!   its own `shade()`, and the renderer will build a pipeline per distinct
//!   shader; that is not built yet.

use atd_core::graph::NodeId;

/// Shading models (the `model` field of the WGSL `Material`). IDs are
/// fixed: files and shaders do not store them, but `shade()` switches on
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Model {
    /// Colour (× `Cd`, × texture) plus emission; no lighting.
    Unlit,
    /// Lambert diffuse plus GGX specular from one directional light, with
    /// ambient and emission (roughness, metallic).
    #[default]
    Pbr,
    /// A lit-sphere image looked up by the view-space normal.
    Matcap,
    /// Banded diffuse between a shadow and a lit colour, rim light and a
    /// hard highlight.
    Toon,
    /// Polygon edges (and polylines) of a set width in pixels, over an
    /// optional fill.
    Line,
    /// Points drawn as camera-facing discs or squares.
    Sprite,
}

impl Model {
    /// The ID `shade()` switches on.
    pub const fn id(self) -> u32 {
        match self {
            Model::Unlit => 0,
            Model::Pbr => 1,
            Model::Matcap => 2,
            Model::Toon => 3,
            Model::Line => 4,
            Model::Sprite => 5,
        }
    }
}

/// Bytes of [`Material::pack`] (the WGSL `Material` struct).
pub const BYTES: usize = 96;

/// A material: what a MAT operator outputs. Colours are RGBA in the
/// engine's working space (the values images hold, no conversion).
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    pub model: Model,
    /// Base colour (albedo, line or sprite colour; matcap tint).
    pub colour: [f32; 4],
    /// Added after lighting (RGB; strength already applied).
    pub emission: [f32; 4],
    /// Second colour: Toon shadow colour, Line fill colour (alpha 0: no
    /// fill).
    pub colour2: [f32; 4],
    /// Model-specific numbers (see each MAT operator): Toon bands, rim,
    /// highlight size, rim width; Line width; Sprite size, size mode,
    /// shape, sphere shading.
    pub params: [f32; 4],
    /// PBR roughness (0.02 – 1).
    pub roughness: f32,
    /// PBR metallic (0 – 1).
    pub metallic: f32,
    /// Multiply the base colour by the geometry's `Cd` (point, else prim).
    pub use_cd: bool,
    /// The node whose image is this material's texture (the MAT operator's
    /// first IMG input), set by the engine after each cook.
    pub texture_source: Option<NodeId>,
}

impl Default for Material {
    /// The renderer's default when nothing is connected: a light grey PBR
    /// surface, roughness 0.5, coloured by `Cd` when the geometry has it.
    fn default() -> Self {
        Self {
            model: Model::Pbr,
            colour: [0.8, 0.8, 0.8, 1.0],
            emission: [0.0; 4],
            colour2: [0.0; 4],
            params: [0.0; 4],
            roughness: 0.5,
            metallic: 0.0,
            use_cd: true,
            texture_source: None,
        }
    }
}

impl Material {
    /// The uniform block (the WGSL `Material`), little-endian. `has_texture`
    /// tells the shader whether a texture is bound.
    pub fn pack(&self, has_texture: bool) -> [u8; BYTES] {
        let mut out = [0u8; BYTES];
        let mut at = 0;
        let mut put = |w: u32| {
            out[at..at + 4].copy_from_slice(&w.to_le_bytes());
            at += 4;
        };
        for v in [self.colour, self.emission, self.colour2, self.params] {
            for c in v {
                put(c.to_bits());
            }
        }
        put(self.roughness.clamp(0.02, 1.0).to_bits());
        put(self.metallic.clamp(0.0, 1.0).to_bits());
        put(0);
        put(0);
        put(self.model.id());
        put(u32::from(self.use_cd));
        put(u32::from(has_texture));
        put(0);
        out
    }
}

/// The WGSL struct [`Material::pack`] fills, and the model IDs.
pub const WGSL: &str = r#"
struct Material {
    colour: vec4f,
    emission: vec4f,
    colour2: vec4f,
    params: vec4f,
    // x roughness, y metallic
    pbr: vec4f,
    // x model, y use Cd, z has texture
    flags: vec4u,
};
const MODEL_UNLIT: u32 = 0u;
const MODEL_PBR: u32 = 1u;
const MODEL_MATCAP: u32 = 2u;
const MODEL_TOON: u32 = 3u;
const MODEL_LINE: u32 = 4u;
const MODEL_SPRITE: u32 = 5u;
"#;

/// The shared shading function. Inputs, all in world space: unit normal
/// `n` (facing the viewer), unit direction to the viewer `v`, unit
/// direction to the light `l`, light colour (intensity applied) and
/// ambient colour; `cd` is the geometry colour (white without `Cd`);
/// `texel` the texture sample (white without a texture; for Matcap the
/// matcap sample). Returns linear RGB; alpha is the base colour's.
pub const SHADE: &str = r#"
fn mat_base(m: Material, cd: vec4f, texel: vec4f) -> vec4f {
    var c = m.colour;
    if (m.flags.y != 0u) { c = c * cd; }
    if (m.flags.z != 0u && m.flags.x != MODEL_MATCAP) { c = c * texel; }
    return c;
}

fn ggx(n: vec3f, v: vec3f, l: vec3f, rough: f32, f0: vec3f) -> vec3f {
    let h = normalize(v + l);
    let nl = max(dot(n, l), 0.0);
    let nv = max(dot(n, v), 1e-4);
    let nh = max(dot(n, h), 0.0);
    let vh = max(dot(v, h), 0.0);
    let a = rough * rough;
    let a2 = a * a;
    let d = nh * nh * (a2 - 1.0) + 1.0;
    let ndf = a2 / max(3.14159265 * d * d, 1e-6);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let g = (nv / (nv * (1.0 - k) + k)) * (nl / (nl * (1.0 - k) + k));
    let f = f0 + (vec3f(1.0) - f0) * pow(1.0 - vh, 5.0);
    return ndf * g * f / max(4.0 * nv * nl, 1e-4) * nl;
}

fn shade(m: Material, n: vec3f, v: vec3f, l: vec3f, light: vec3f, ambient: vec3f, cd: vec4f, texel: vec4f) -> vec4f {
    let base = mat_base(m, cd, texel);
    let nl = max(dot(n, l), 0.0);
    var rgb = base.rgb;
    switch (m.flags.x) {
        case 1u: { // PBR
            let rough = m.pbr.x;
            let metal = m.pbr.y;
            let f0 = mix(vec3f(0.04), base.rgb, metal);
            let diffuse = base.rgb * (1.0 - metal) * nl;
            let spec = ggx(n, v, l, rough, f0);
            rgb = (diffuse + spec) * light + ambient * base.rgb;
        }
        case 2u: { // Matcap: the texel already looked up by the normal
            if (m.flags.z != 0u) { rgb = base.rgb * texel.rgb; }
            else { rgb = base.rgb * (0.25 + 0.75 * max(n.y * 0.5 + 0.5, 0.0)); }
        }
        case 3u: { // Toon
            let bands = max(m.params.x, 1.0);
            let q = min(floor(nl * bands + 0.5), bands) / bands;
            let lit = mix(m.colour2.rgb, base.rgb, q);
            let h = normalize(v + l);
            let hi = step(1.0 - max(m.params.z, 0.0) * 0.1, max(dot(n, h), 0.0)) * step(0.001, m.params.z);
            let rim_w = clamp(m.params.w, 0.0, 1.0);
            let rim = smoothstep(1.0 - rim_w, 1.0 - rim_w * 0.5, 1.0 - max(dot(n, v), 0.0)) * m.params.y;
            rgb = lit * light + vec3f(hi) * light + vec3f(rim) * base.rgb;
        }
        case 4u: { // Line: the fill (edges are drawn by the renderer)
            rgb = m.colour2.rgb;
        }
        default: { // Unlit, Sprite
            rgb = base.rgb;
        }
    }
    return vec4f(rgb + m.emission.rgb, base.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_follows_the_wgsl_layout() {
        let m = Material {
            model: Model::Toon,
            colour: [1.0, 0.5, 0.25, 1.0],
            roughness: 5.0,
            use_cd: true,
            ..Material::default()
        };
        let b = m.pack(true);
        let word = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(f32::from_bits(word(1)), 0.5);
        assert_eq!(f32::from_bits(word(16)), 1.0, "roughness clamped");
        assert_eq!(
            (word(20), word(21), word(22)),
            (Model::Toon.id(), 1, 1),
            "model, use Cd, has texture"
        );
        assert_eq!(BYTES % 16, 0);
    }
}
