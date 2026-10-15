//! IMG operator definitions, batch 2: compositing, keying, filters and
//! distortion (PRD §6). Same conventions as `ops.rs`.

use atd_core::registry::{OperatorSchema, ParamSchema};
use atd_gpu::engine::{CookCtx, Params};

use crate::ops::img;
use crate::{Def, Packed};

fn menu(p: &Params<'_>, name: &str, options: &[&str]) -> f32 {
    let v = p.str(name);
    options.iter().position(|o| *o == v).unwrap_or(0) as f32
}

fn texel(c: &CookCtx<'_>) -> [f32; 2] {
    [
        1.0 / c.output.width() as f32,
        1.0 / c.output.height() as f32,
    ]
}

fn flag(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

const SHAPES: &[&str] = &["circle", "rectangle", "polygon"];
const EDGE_MODES: &[&str] = &["magnitude", "colour"];

/// Luminance and 3×3 Sobel gradient of input 0, shared by Edge, Normal Map and Slope.
const SOBEL_WGSL: &str = r#"
fn lum(c: vec4f) -> f32 { return dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)); }
fn tap(uv: vec2f, d: vec2f) -> f32 { return lum(textureSampleLevel(in0, samp, uv + d, 0.0)); }
fn sobel(uv: vec2f, px: vec2f) -> vec2f {
    let tl = tap(uv, vec2f(-px.x, -px.y)); let t = tap(uv, vec2f(0.0, -px.y)); let tr = tap(uv, vec2f(px.x, -px.y));
    let l = tap(uv, vec2f(-px.x, 0.0));                                  let r = tap(uv, vec2f(px.x, 0.0));
    let bl = tap(uv, vec2f(-px.x, px.y)); let b = tap(uv, vec2f(0.0, px.y)); let br = tap(uv, vec2f(px.x, px.y));
    let gx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
    let gy = (bl + 2.0 * b + br) - (tl + 2.0 * t + tr);
    return vec2f(gx, gy);
}
"#;

pub fn shape_schema() -> OperatorSchema {
    img(
        "shape",
        "Shape",
        "A circle, rectangle or regular polygon with soft edges.",
        &[],
        vec![
            ParamSchema::menu("shape", "Shape", SHAPES, "circle").help("Which shape to draw."),
            ParamSchema::vec("centre", "Centre", &[0.5, 0.5])
                .help("Centre as a fraction of the image from the bottom-left."),
            ParamSchema::vec("size", "Size", &[0.5, 0.5])
                .help("Width and height as fractions of the image height (so circles stay round)."),
            ParamSchema::int("sides", "Sides", 6, Some(3), Some(64)).help("Polygon sides."),
            ParamSchema::float("rotate", "Rotate", 0.0, None, None)
                .help("Rotation in degrees, counter-clockwise."),
            ParamSchema::float("softness", "Softness", 0.005, Some(0.0), None)
                .help("Edge blur as a fraction of the image height."),
            ParamSchema::colour("fill", "Fill", [1.0, 1.0, 1.0, 1.0])
                .help("Colour inside the shape."),
            ParamSchema::colour("background", "Background", [0.0, 0.0, 0.0, 0.0])
                .help("Colour outside the shape."),
        ],
        80,
    )
}

pub static DEFS2: &[Def] = &[
    Def {
        schema: shape_schema,
        helpers: "",
        body: r#"
    let aspect = params.v[4].x;
    let k = i32(params.v[0].x);
    var q = (uv - params.v[0].yz) * vec2f(aspect, 1.0);
    let a = params.v[0].w;
    q = vec2f(q.x * cos(a) - q.y * sin(a), q.x * sin(a) + q.y * cos(a));
    let half = params.v[1].xy * 0.5;
    var d = 0.0;
    if (k == 0) {
        d = length(q / max(half, vec2f(1e-5))) - 1.0;
        d = d * min(half.x, half.y);
    } else if (k == 1) {
        let e = abs(q) - half;
        d = length(max(e, vec2f(0.0))) + min(max(e.x, e.y), 0.0);
    } else {
        let n = max(params.v[1].z, 3.0);
        let seg = 6.2831853 / n;
        let ang = atan2(q.y, q.x);
        let r = cos(seg * 0.5) / cos(ang - seg * floor(ang / seg) - seg * 0.5);
        d = length(q) - r * min(half.x, half.y);
    }
    let soft = max(params.v[1].w, 1e-5);
    let inside = 1.0 - smoothstep(-soft * 0.5, soft * 0.5, d);
    return mix(params.v[3], params.v[2], inside);"#,
        pack: |c| {
            let p = &c.params;
            let centre = p.vec4("centre");
            let size = p.vec4("size");
            let aspect = c.output.width() as f32 / c.output.height() as f32;
            // A counter-clockwise turn in y-up space rotates the y-down query
            // point by the same positive angle.
            *Packed::default()
                .push([
                    menu(p, "shape", SHAPES),
                    centre[0],
                    1.0 - centre[1],
                    p.f32("rotate").to_radians(),
                ])
                .push([size[0], size[1], p.i64("sides") as f32, p.f32("softness")])
                .push(p.vec4("fill"))
                .push(p.vec4("background"))
                .push([aspect, 0.0, 0.0, 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "blend",
                "Blend",
                "Crossfades from input A to input B.",
                &[("a", false), ("b", false)],
                vec![
                    ParamSchema::float("mix", "Mix", 0.5, Some(0.0), Some(1.0))
                        .help("0 shows A, 1 shows B."),
                ],
                60,
            )
        },
        helpers: "",
        body: "return mix(textureSample(in0, samp, uv), textureSample(in1, samp, uv), params.v[0].x);",
        pack: |c| *Packed::default().push([c.params.f32("mix"), 0.0, 0.0, 0.0]),
    },
    Def {
        schema: || {
            img(
                "channel_mix",
                "Channel Mix",
                "Rebuilds each output channel as a weighted sum of the input's R, G, B and A.",
                &[("in", false)],
                vec![
                    ParamSchema::vec("red", "Red", &[1.0, 0.0, 0.0, 0.0])
                        .help("Weights of input R, G, B, A in the output red."),
                    ParamSchema::vec("green", "Green", &[0.0, 1.0, 0.0, 0.0])
                        .help("Weights for the output green."),
                    ParamSchema::vec("blue", "Blue", &[0.0, 0.0, 1.0, 0.0])
                        .help("Weights for the output blue."),
                    ParamSchema::vec("alpha", "Alpha", &[0.0, 0.0, 0.0, 1.0])
                        .help("Weights for the output alpha."),
                ],
                60,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    return vec4f(dot(c, params.v[0]), dot(c, params.v[1]), dot(c, params.v[2]), dot(c, params.v[3]));"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default()
                .push(p.vec4("red"))
                .push(p.vec4("green"))
                .push(p.vec4("blue"))
                .push(p.vec4("alpha"))
        },
    },
    Def {
        schema: || {
            img(
                "lookup",
                "Lookup",
                "Recolours input A through a lookup image B: each pixel's luminance picks a colour along B's horizontal centre line.",
                &[("in", false), ("lut", false)],
                vec![
                    ParamSchema::float("offset", "Offset", 0.0, None, None)
                        .help("Shifts the lookup position (wraps)."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    let l = fract(dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)) * 0.999 + params.v[0].x);
    let m = textureSampleLevel(in1, samp, vec2f(l, 0.5), 0.0);
    return vec4f(m.rgb, c.a * m.a);"#,
        pack: |c| *Packed::default().push([c.params.f32("offset"), 0.0, 0.0, 0.0]),
    },
    Def {
        schema: || {
            img(
                "remap",
                "Remap",
                "Samples input A at the coordinates stored in input B's red and green channels.",
                &[("in", false), ("map", false)],
                vec![],
                60,
            )
        },
        helpers: "",
        body: r#"
    let m = textureSample(in1, samp, uv);
    return textureSampleLevel(in0, samp, vec2f(m.r, 1.0 - m.g), 0.0);"#,
        pack: |_| Packed::default(),
    },
    Def {
        schema: || {
            img(
                "displace",
                "Displace",
                "Offsets input A by input B: red moves horizontally, green vertically, around mid-grey.",
                &[("in", false), ("by", false)],
                vec![
                    ParamSchema::float("amount", "Amount", 0.05, None, None)
                        .help("Displacement as a fraction of the image at full deviation."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let d = (textureSample(in1, samp, uv).rg - vec2f(0.5)) * params.v[0].x;
    return textureSampleLevel(in0, samp, uv + vec2f(d.x, -d.y), 0.0);"#,
        pack: |c| *Packed::default().push([c.params.f32("amount"), 0.0, 0.0, 0.0]),
    },
    Def {
        schema: || {
            img(
                "chroma_key",
                "Chroma Key",
                "Makes pixels close to a key colour transparent.",
                &[("in", false)],
                vec![
                    ParamSchema::colour("key", "Key", [0.0, 1.0, 0.0, 1.0])
                        .help("Colour to remove."),
                    ParamSchema::float("tolerance", "Tolerance", 0.3, Some(0.0), None)
                        .help("Colour distance treated as fully keyed."),
                    ParamSchema::float("softness", "Softness", 0.1, Some(0.0), None)
                        .help("Extra distance over which the key fades."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    let d = distance(c.rgb, params.v[0].rgb);
    let a = smoothstep(params.v[1].x, params.v[1].x + max(params.v[1].y, 1e-5), d);
    return vec4f(c.rgb, c.a * a);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push(p.vec4("key")).push([
                p.f32("tolerance"),
                p.f32("softness"),
                0.0,
                0.0,
            ])
        },
    },
    Def {
        schema: || {
            img(
                "luma_key",
                "Luma Key",
                "Makes pixels outside a luminance range transparent.",
                &[("in", false)],
                vec![
                    ParamSchema::float("low", "Low", 0.0, None, None)
                        .help("Lowest luminance kept."),
                    ParamSchema::float("high", "High", 0.5, None, None).help(
                        "Highest luminance kept; set Low above it to key out the range instead.",
                    ),
                    ParamSchema::float("softness", "Softness", 0.05, Some(0.0), None)
                        .help("Fade width at both edges."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    let l = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    let s = max(params.v[0].z, 1e-5);
    let lo = params.v[0].x;
    let hi = params.v[0].y;
    var keep = smoothstep(lo - s, lo, l) * (1.0 - smoothstep(hi, hi + s, l));
    if (lo > hi) { keep = 1.0 - smoothstep(hi - s, hi, l) * (1.0 - smoothstep(lo, lo + s, l)); }
    return vec4f(c.rgb, c.a * keep);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push([p.f32("low"), p.f32("high"), p.f32("softness"), 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "edge",
                "Edge",
                "Outlines: the strength of luminance edges (Sobel).",
                &[("in", false)],
                vec![
                    ParamSchema::float("strength", "Strength", 1.0, Some(0.0), None)
                        .help("Multiplies the edge strength."),
                    ParamSchema::menu("mode", "Mode", EDGE_MODES, "magnitude")
                        .help("Grey edges, or the input colour masked by edges."),
                ],
                120,
            )
        },
        helpers: SOBEL_WGSL,
        body: r#"
    let g = length(sobel(uv, params.v[0].yz)) * params.v[0].x;
    if (params.v[0].w > 0.5) {
        let c = textureSample(in0, samp, uv);
        return vec4f(c.rgb * clamp(g, 0.0, 1.0), 1.0);
    }
    let v = clamp(g, 0.0, 1.0);
    return vec4f(v, v, v, 1.0);"#,
        pack: |c| {
            let t = texel(c);
            *Packed::default().push([
                c.params.f32("strength"),
                t[0],
                t[1],
                menu(&c.params, "mode", EDGE_MODES),
            ])
        },
    },
    Def {
        schema: || {
            img(
                "normal_map",
                "Normal Map",
                "Treats luminance as height and outputs tangent-space normals (RGB = XYZ × 0.5 + 0.5).",
                &[("in", false)],
                vec![
                    ParamSchema::float("strength", "Strength", 2.0, Some(0.0), None)
                        .help("Height scale; higher gives steeper normals."),
                ],
                120,
            )
        },
        helpers: SOBEL_WGSL,
        body: r#"
    let g = sobel(uv, params.v[0].yz) * params.v[0].x;
    let n = normalize(vec3f(-g.x, g.y, 1.0));
    return vec4f(n * 0.5 + vec3f(0.5), 1.0);"#,
        pack: |c| {
            let t = texel(c);
            *Packed::default().push([c.params.f32("strength"), t[0], t[1], 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "slope",
                "Slope",
                "The luminance gradient: red = horizontal slope, green = vertical slope, around mid-grey.",
                &[("in", false)],
                vec![
                    ParamSchema::float("strength", "Strength", 1.0, Some(0.0), None)
                        .help("Multiplies the slope."),
                ],
                120,
            )
        },
        helpers: SOBEL_WGSL,
        body: r#"
    let g = sobel(uv, params.v[0].yz) * params.v[0].x;
    return vec4f(vec2f(g.x, -g.y) * 0.5 + vec2f(0.5), 0.5, 1.0);"#,
        pack: |c| {
            let t = texel(c);
            *Packed::default().push([c.params.f32("strength"), t[0], t[1], 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "bloom",
                "Bloom",
                "Adds a glow around bright areas.",
                &[("in", false)],
                vec![
                    ParamSchema::float("threshold", "Threshold", 0.7, None, None)
                        .help("Luminance above which pixels glow."),
                    ParamSchema::float("size", "Size", 12.0, Some(0.0), Some(256.0))
                        .help("Glow radius in pixels."),
                    ParamSchema::float("intensity", "Intensity", 1.0, Some(0.0), None)
                        .help("Strength of the added glow."),
                ],
                800,
            )
        },
        helpers: "",
        body: r#"
    let base = textureSample(in0, samp, uv);
    let r = params.v[0].y;
    let px = params.v[1].xy;
    let sigma2 = max(2.0 * (r * 0.5) * (r * 0.5), 1e-4);
    var acc = vec3f(0.0);
    var wsum = 0.0;
    for (var j = -4; j <= 4; j++) {
        for (var i = -4; i <= 4; i++) {
            let o = vec2f(f32(i), f32(j)) * (r / 4.0);
            let w = exp(-dot(o, o) / sigma2);
            let s = textureSampleLevel(in0, samp, uv + o * px, 0.0).rgb;
            let l = dot(s, vec3f(0.2126, 0.7152, 0.0722));
            acc += s * max(l - params.v[0].x, 0.0) / max(l, 1e-4) * w;
            wsum += w;
        }
    }
    return vec4f(base.rgb + acc / wsum * params.v[0].z, base.a);"#,
        pack: |c| {
            let p = &c.params;
            let t = texel(c);
            *Packed::default()
                .push([p.f32("threshold"), p.f32("size"), p.f32("intensity"), 0.0])
                .push([t[0], t[1], 0.0, 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "crop",
                "Crop",
                "Keeps a rectangle of the input and stretches it to fill the output.",
                &[("in", false)],
                vec![
                    ParamSchema::vec("min", "Min", &[0.25, 0.25])
                        .help("Bottom-left corner of the kept area, as a fraction of the image."),
                    ParamSchema::vec("max", "Max", &[0.75, 0.75])
                        .help("Top-right corner of the kept area."),
                ],
                60,
            )
        },
        helpers: "",
        body: r#"
    let lo = params.v[0].xy;
    let hi = params.v[0].zw;
    return textureSampleLevel(in0, samp, mix(lo, hi, uv), 0.0);"#,
        pack: |c| {
            let lo = c.params.vec4("min");
            let hi = c.params.vec4("max");
            // y-up user space to y-down texture space.
            *Packed::default().push([lo[0], 1.0 - hi[1], hi[0], 1.0 - lo[1]])
        },
    },
    Def {
        schema: || {
            img(
                "switch",
                "Switch",
                "Outputs one of up to four inputs, optionally blending between neighbours.",
                &[
                    ("in0", false),
                    ("in1", false),
                    ("in2", false),
                    ("in3", false),
                ],
                vec![
                    ParamSchema::float("index", "Index", 0.0, Some(0.0), Some(3.0))
                        .help("Which input to show; fractional values blend when Blend is on."),
                    ParamSchema::toggle("blend", "Blend", false)
                        .help("Crossfade between neighbouring inputs for fractional indices."),
                ],
                60,
            )
        },
        helpers: "",
        body: r#"
    let s0 = textureSample(in0, samp, uv);
    let s1 = textureSample(in1, samp, uv);
    let s2 = textureSample(in2, samp, uv);
    let s3 = textureSample(in3, samp, uv);
    var x = clamp(params.v[0].x, 0.0, 3.0);
    if (params.v[0].y < 0.5) { x = round(x); }
    let i = min(floor(x), 2.0);
    let f = x - i;
    var a = s0; var b = s1;
    if (i >= 2.0) { a = s2; b = s3; } else if (i >= 1.0) { a = s1; b = s2; }
    return mix(a, b, f);"#,
        pack: |c| {
            *Packed::default().push([
                c.params.f32("index"),
                flag(c.params.bool("blend")),
                0.0,
                0.0,
            ])
        },
    },
];
