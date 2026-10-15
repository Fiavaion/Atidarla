//! IMG operator definitions (PRD §6, first batch).
//!
//! Conventions: `uv` (0,0) is the top-left texel; colours are straight
//! (not premultiplied) RGBA; distances and translations are fractions of
//! the image, with +y up for user-facing parameters.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::Params;

use crate::{Def, Packed};

fn img(
    id: &str,
    label: &str,
    summary: &str,
    inputs: &[(&str, bool)],
    params: Vec<ParamSchema>,
    cook_budget_us: u32,
) -> OperatorSchema {
    let socket = |name: &str, feedback: bool| SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback,
    };
    OperatorSchema {
        type_id: format!("img.{id}"),
        family: OpFamily::Img,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: inputs.iter().map(|(n, fb)| socket(n, *fb)).collect(),
        outputs: vec![socket("out", false)],
        capabilities: vec![],
        cook_budget_us,
        time_dependent: false,
    }
}

/// Index of a menu parameter's current option, as a float for the shader.
fn menu(p: &Params<'_>, name: &str, options: &[&str]) -> f32 {
    let v = p.str(name);
    options.iter().position(|o| *o == v).unwrap_or(0) as f32
}

fn vec2(p: &Params<'_>, name: &str) -> [f32; 2] {
    let v = p.vec4(name);
    [v[0], v[1]]
}

const RAMP_TYPES: &[&str] = &["horizontal", "vertical", "radial", "angular"];
const COMPOSITE_OPS: &[&str] = &[
    "over",
    "add",
    "multiply",
    "screen",
    "subtract",
    "difference",
    "maximum",
    "minimum",
    "average",
];
const CHANNELS: &[&str] = &["luminance", "red", "green", "blue", "alpha"];
const MIRROR_AXES: &[&str] = &["left_to_right", "top_to_bottom", "both"];
const EXTEND: &[&str] = &["zero", "hold", "repeat", "mirror"];

const NOISE_WGSL: &str = r#"
fn hash3(p: vec3i) -> vec3f {
    // PCG-style 3D hash; returns gradients in [-1, 1].
    var v = vec3u(bitcast<u32>(p.x), bitcast<u32>(p.y), bitcast<u32>(p.z)) * 1664525u + vec3u(1013904223u);
    v.x += v.y * v.z; v.y += v.z * v.x; v.z += v.x * v.y;
    v = v ^ (v >> vec3u(16u));
    v.x += v.y * v.z; v.y += v.z * v.x; v.z += v.x * v.y;
    return vec3f(v) * (2.0 / 4294967295.0) - vec3f(1.0);
}

fn corner(i: vec3i, f: vec3f, o: vec3i) -> f32 {
    return dot(hash3(i + o), f - vec3f(o));
}

fn gnoise(p: vec3f) -> f32 {
    let i = vec3i(floor(p));
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let x00 = mix(corner(i, f, vec3i(0, 0, 0)), corner(i, f, vec3i(1, 0, 0)), u.x);
    let x10 = mix(corner(i, f, vec3i(0, 1, 0)), corner(i, f, vec3i(1, 1, 0)), u.x);
    let x01 = mix(corner(i, f, vec3i(0, 0, 1)), corner(i, f, vec3i(1, 0, 1)), u.x);
    let x11 = mix(corner(i, f, vec3i(0, 1, 1)), corner(i, f, vec3i(1, 1, 1)), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

fn fbm(p0: vec3f, octaves: i32, gain: f32, seed: i32) -> f32 {
    var p = p0 + vec3f(f32(seed) * 17.17, f32(seed) * 31.31, f32(seed) * 7.77);
    var a = 1.0;
    var sum = 0.0;
    var norm = 0.0;
    for (var o = 0; o < octaves; o++) {
        sum += a * gnoise(p);
        norm += a;
        a *= gain;
        p = p * 2.0 + vec3f(5.3, 1.7, 3.1);
    }
    return sum / max(norm, 1e-6);
}
"#;

const HSV_WGSL: &str = r#"
fn rgb2hsv(c: vec3f) -> vec3f {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    var h = 0.0;
    if (d > 1e-6) {
        if (mx == c.r) { h = (c.g - c.b) / d; }
        else if (mx == c.g) { h = 2.0 + (c.b - c.r) / d; }
        else { h = 4.0 + (c.r - c.g) / d; }
        h = fract(h / 6.0 + 1.0);
    }
    var s = 0.0;
    if (mx > 1e-6) { s = d / mx; }
    return vec3f(h, s, mx);
}

fn hsv2rgb(c: vec3f) -> vec3f {
    let k = vec3f(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3f(c.x) + k) * 6.0 - vec3f(3.0));
    return c.z * mix(vec3f(1.0), clamp(p - vec3f(1.0), vec3f(0.0), vec3f(1.0)), c.y);
}
"#;

pub static DEFS: &[Def] = &[
    Def {
        schema: || {
            img(
                "constant",
                "Constant",
                "Fills the image with one colour.",
                &[],
                vec![
                    ParamSchema::colour("colour", "Colour", [0.0, 0.0, 0.0, 1.0])
                        .help("Fill colour (RGBA)."),
                ],
                50,
            )
        },
        helpers: "",
        body: "return params.v[0];",
        pack: |c| *Packed::default().push(c.params.vec4("colour")),
    },
    Def {
        schema: || {
            img(
                "ramp",
                "Ramp",
                "A gradient between two colours: horizontal, vertical, radial or angular.",
                &[],
                vec![
                    ParamSchema::menu("type", "Type", RAMP_TYPES, "horizontal")
                        .help("Direction of the gradient; radial runs from the centre outwards."),
                    ParamSchema::colour("colour_a", "Colour A", [0.0, 0.0, 0.0, 1.0])
                        .help("Colour at the start."),
                    ParamSchema::colour("colour_b", "Colour B", [1.0, 1.0, 1.0, 1.0])
                        .help("Colour at the end."),
                    ParamSchema::float("phase", "Phase", 0.0, None, None).help(
                        "Shifts the gradient position; the result is clamped to the two colours.",
                    ),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let kind = i32(params.v[2].x);
    var t = uv.x;
    if (kind == 1) { t = 1.0 - uv.y; }
    else if (kind == 2) { t = length(uv - vec2f(0.5)) * 2.0; }
    else if (kind == 3) { t = atan2(0.5 - uv.y, uv.x - 0.5) / 6.2831853 + 0.5; }
    t = clamp(t + params.v[2].y, 0.0, 1.0);
    return mix(params.v[0], params.v[1], t);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default()
                .push(p.vec4("colour_a"))
                .push(p.vec4("colour_b"))
                .push([menu(p, "type", RAMP_TYPES), p.f32("phase"), 0.0, 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "noise",
                "Noise",
                "Fractal gradient noise. Animate `z` to move through it over time.",
                &[],
                vec![
                    ParamSchema::float("scale", "Scale", 4.0, Some(0.0), None)
                        .help("Noise features across the image width."),
                    ParamSchema::int("octaves", "Octaves", 3, Some(1), Some(8))
                        .help("Layers of detail, each twice the frequency of the last."),
                    ParamSchema::float("roughness", "Roughness", 0.5, Some(0.0), Some(1.0))
                        .help("Amplitude ratio between successive octaves."),
                    ParamSchema::int("seed", "Seed", 0, None, None)
                        .help("Selects a different noise pattern."),
                    ParamSchema::float("z", "Z", 0.0, None, None)
                        .help("Position along the third axis; drive it with time to animate."),
                    ParamSchema::float("amplitude", "Amplitude", 0.5, None, None)
                        .help("Output = noise × amplitude + offset."),
                    ParamSchema::float("offset", "Offset", 0.5, None, None)
                        .help("Added after amplitude."),
                    ParamSchema::toggle("monochrome", "Monochrome", true)
                        .help("One noise value in RGB; off gives three independent channels."),
                ],
                400,
            )
        },
        helpers: NOISE_WGSL,
        body: r#"
    let p = vec3f(uv * params.v[0].x, params.v[0].w);
    let oct = i32(params.v[0].y);
    let gain = params.v[0].z;
    let seed = i32(params.v[1].x);
    let amp = params.v[1].y;
    let off = params.v[1].z;
    let r = fbm(p, oct, gain, seed);
    var rgb = vec3f(r);
    if (params.v[1].w < 0.5) {
        rgb = vec3f(r, fbm(p + vec3f(17.3, 9.1, 4.7), oct, gain, seed), fbm(p + vec3f(-5.2, 31.7, 11.3), oct, gain, seed));
    }
    return vec4f(rgb * amp + vec3f(off), 1.0);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default()
                .push([
                    p.f32("scale"),
                    p.i64("octaves") as f32,
                    p.f32("roughness"),
                    p.f32("z"),
                ])
                .push([
                    p.i64("seed") as f32,
                    p.f32("amplitude"),
                    p.f32("offset"),
                    if p.bool("monochrome") { 1.0 } else { 0.0 },
                ])
        },
    },
    Def {
        schema: || {
            img(
                "level",
                "Level",
                "Brightness, contrast, gamma, opacity and invert.",
                &[("in", false)],
                vec![
                    ParamSchema::float("brightness", "Brightness", 1.0, Some(0.0), None)
                        .help("Multiplies RGB."),
                    ParamSchema::float("contrast", "Contrast", 1.0, Some(0.0), None)
                        .help("Scales RGB around mid-grey (0.5)."),
                    ParamSchema::float("gamma", "Gamma", 1.0, Some(0.01), None)
                        .help("Applies RGB^(1/gamma)."),
                    ParamSchema::float("opacity", "Opacity", 1.0, Some(0.0), Some(1.0))
                        .help("Multiplies alpha."),
                    ParamSchema::toggle("invert", "Invert", false)
                        .help("Inverts RGB before the other adjustments."),
                ],
                100,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    var rgb = c.rgb;
    if (params.v[1].x > 0.5) { rgb = vec3f(1.0) - rgb; }
    rgb = (rgb - vec3f(0.5)) * params.v[0].y + vec3f(0.5);
    rgb = rgb * params.v[0].x;
    rgb = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / params.v[0].z));
    return vec4f(rgb, c.a * params.v[0].w);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default()
                .push([
                    p.f32("brightness"),
                    p.f32("contrast"),
                    p.f32("gamma"),
                    p.f32("opacity"),
                ])
                .push([if p.bool("invert") { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "hsv_adjust",
                "HSV Adjust",
                "Rotates hue and scales saturation and value.",
                &[("in", false)],
                vec![
                    ParamSchema::float("hue", "Hue Offset", 0.0, None, None)
                        .help("Hue rotation in degrees."),
                    ParamSchema::float("saturation", "Saturation", 1.0, Some(0.0), None)
                        .help("Multiplies saturation."),
                    ParamSchema::float("value", "Value", 1.0, Some(0.0), None)
                        .help("Multiplies value (brightness)."),
                ],
                100,
            )
        },
        helpers: HSV_WGSL,
        body: r#"
    let c = textureSample(in0, samp, uv);
    var hsv = rgb2hsv(c.rgb);
    hsv.x = fract(hsv.x + params.v[0].x / 360.0 + 1.0);
    hsv.y = clamp(hsv.y * params.v[0].y, 0.0, 1.0);
    hsv.z = hsv.z * params.v[0].z;
    return vec4f(hsv2rgb(hsv), c.a);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push([p.f32("hue"), p.f32("saturation"), p.f32("value"), 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "composite",
                "Composite",
                "Combines input A with input B using a blend operation.",
                &[("a", false), ("b", false)],
                vec![
                    ParamSchema::menu("operation", "Operation", COMPOSITE_OPS, "over").help(
                        "How A combines with B. `over` places A on top of B using A's alpha.",
                    ),
                    ParamSchema::float("opacity", "Opacity", 1.0, Some(0.0), Some(1.0))
                        .help("Mix between B alone (0) and the full result (1)."),
                ],
                120,
            )
        },
        helpers: "",
        body: r#"
    let a = textureSample(in0, samp, uv);
    let b = textureSample(in1, samp, uv);
    let k = i32(params.v[0].x);
    let al = max(a.a, b.a);
    var r = vec4f(0.0);
    switch k {
        case 0: { r = vec4f(a.rgb * a.a + b.rgb * (1.0 - a.a), a.a + b.a * (1.0 - a.a)); }
        case 1: { r = vec4f(a.rgb + b.rgb, al); }
        case 2: { r = a * b; }
        case 3: { r = vec4f(vec3f(1.0) - (vec3f(1.0) - a.rgb) * (vec3f(1.0) - b.rgb), al); }
        case 4: { r = vec4f(a.rgb - b.rgb, al); }
        case 5: { r = vec4f(abs(a.rgb - b.rgb), al); }
        case 6: { r = max(a, b); }
        case 7: { r = min(a, b); }
        default: { r = (a + b) * 0.5; }
    }
    return mix(b, r, params.v[0].y);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push([
                menu(p, "operation", COMPOSITE_OPS),
                p.f32("opacity"),
                0.0,
                0.0,
            ])
        },
    },
    Def {
        schema: || {
            img(
                "threshold",
                "Threshold",
                "White where a channel is above the threshold, black elsewhere.",
                &[("in", false)],
                vec![
                    ParamSchema::float("threshold", "Threshold", 0.5, None, None)
                        .help("Cut-off value."),
                    ParamSchema::float("softness", "Softness", 0.0, Some(0.0), None)
                        .help("Width of the smooth transition around the threshold."),
                    ParamSchema::menu("channel", "Channel", CHANNELS, "luminance")
                        .help("Which value is compared."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let c = textureSample(in0, samp, uv);
    let k = i32(params.v[0].z);
    var x = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    if (k == 1) { x = c.r; } else if (k == 2) { x = c.g; } else if (k == 3) { x = c.b; } else if (k == 4) { x = c.a; }
    let s = max(params.v[0].y, 1e-5);
    let v = smoothstep(params.v[0].x - s * 0.5, params.v[0].x + s * 0.5, x);
    return vec4f(vec3f(v), 1.0);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push([
                p.f32("threshold"),
                p.f32("softness"),
                menu(p, "channel", CHANNELS),
                0.0,
            ])
        },
    },
    Def {
        schema: || {
            img(
                "mirror",
                "Mirror",
                "Reflects one half of the image onto the other.",
                &[("in", false)],
                vec![
                    ParamSchema::menu("axis", "Axis", MIRROR_AXES, "left_to_right").help(
                        "Which half is kept and reflected; `both` mirrors in both directions.",
                    ),
                ],
                60,
            )
        },
        helpers: "",
        body: r#"
    let k = i32(params.v[0].x);
    var p = uv;
    if (k == 0 || k == 2) { p.x = 0.5 - abs(p.x - 0.5); }
    if (k == 1 || k == 2) { p.y = 0.5 - abs(p.y - 0.5); }
    return textureSample(in0, samp, p);"#,
        pack: |c| *Packed::default().push([menu(&c.params, "axis", MIRROR_AXES), 0.0, 0.0, 0.0]),
    },
    Def {
        schema: || {
            img(
                "tile",
                "Tile",
                "Repeats the input in a grid.",
                &[("in", false)],
                vec![
                    ParamSchema::vec("repeat", "Repeat", &[2.0, 2.0])
                        .help("Number of tiles across and down."),
                    ParamSchema::toggle("mirror", "Mirror", false)
                        .help("Mirrors every other tile for seamless edges."),
                ],
                60,
            )
        },
        helpers: "",
        body: r#"
    let q = uv * params.v[0].xy;
    var f = fract(q);
    if (params.v[0].z > 0.5) {
        let odd = abs(floor(q) % vec2f(2.0)) > vec2f(0.5);
        f = select(f, vec2f(1.0) - f, odd);
    }
    return textureSample(in0, samp, f);"#,
        pack: |c| {
            let r = vec2(&c.params, "repeat");
            *Packed::default().push([
                r[0],
                r[1],
                if c.params.bool("mirror") { 1.0 } else { 0.0 },
                0.0,
            ])
        },
    },
    Def {
        schema: || {
            img(
                "kaleidoscope",
                "Kaleidoscope",
                "Folds the input into mirrored segments around the centre.",
                &[("in", false)],
                vec![
                    ParamSchema::int("segments", "Segments", 6, Some(1), Some(64))
                        .help("Number of mirrored wedges."),
                    ParamSchema::float("rotation", "Rotation", 0.0, None, None)
                        .help("Rotation of the pattern in degrees."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let c = uv - vec2f(0.5);
    let seg = 6.2831853 / max(params.v[0].x, 1.0);
    var a = atan2(c.y, c.x) + params.v[0].y;
    a = a - seg * floor(a / seg);
    a = abs(a - seg * 0.5);
    let p = vec2f(cos(a), sin(a)) * length(c) + vec2f(0.5);
    return textureSample(in0, samp, p);"#,
        pack: |c| {
            let p = &c.params;
            *Packed::default().push([
                p.i64("segments") as f32,
                p.f32("rotation").to_radians(),
                0.0,
                0.0,
            ])
        },
    },
    Def {
        schema: || {
            img(
                "transform",
                "Transform",
                "Translates, rotates and scales the input around a pivot.",
                &[("in", false)],
                vec![
                    ParamSchema::vec("translate", "Translate", &[0.0, 0.0])
                        .help("Offset as a fraction of the image; +y is up."),
                    ParamSchema::float("rotate", "Rotate", 0.0, None, None).help("Rotation in degrees, counter-clockwise."),
                    ParamSchema::vec("scale", "Scale", &[1.0, 1.0]).help("Scale factors (must not be zero)."),
                    ParamSchema::vec("pivot", "Pivot", &[0.5, 0.5])
                        .help("Centre of rotation and scale, as a fraction of the image from the bottom-left."),
                    ParamSchema::menu("extend", "Extend", EXTEND, "zero")
                        .help("What appears outside the input: transparent, edge pixels, repeats or mirrored repeats."),
                ],
                80,
            )
        },
        helpers: "",
        body: r#"
    let t = params.v[0].xy;
    let s = params.v[0].zw;
    let piv = params.v[1].xy;
    let ang = params.v[1].z;
    let ext = i32(params.v[1].w);
    var q = uv - piv - t;
    let cs = cos(ang);
    let sn = sin(ang);
    q = vec2f(q.x * cs - q.y * sn, q.x * sn + q.y * cs) / s;
    var p = q + piv;
    let inside = all(p >= vec2f(0.0)) && all(p <= vec2f(1.0));
    if (ext == 2) { p = fract(p); }
    else if (ext == 3) { p = vec2f(1.0) - abs(vec2f(1.0) - 2.0 * fract(p * 0.5)); }
    let c = textureSample(in0, samp, p);
    if (ext == 0 && !inside) { return vec4f(0.0); }
    return c;"#,
        pack: |c| {
            let p = &c.params;
            let t = vec2(p, "translate");
            let s = vec2(p, "scale");
            let piv = vec2(p, "pivot");
            // Shader space is y-down; user space is y-up. Rotation is
            // counter-clockwise on screen, which is clockwise in y-down
            // coordinates, so the inverse rotation angle keeps its sign.
            let guard = |v: f32| {
                if v.abs() < 1e-6 {
                    1e-6_f32.copysign(v)
                } else {
                    v
                }
            };
            *Packed::default()
                .push([t[0], -t[1], guard(s[0]), guard(s[1])])
                .push([
                    piv[0],
                    1.0 - piv[1],
                    p.f32("rotate").to_radians(),
                    menu(p, "extend", EXTEND),
                ])
        },
    },
    Def {
        schema: || {
            img(
                "feedback",
                "Feedback",
                "Outputs the previous frame of the `loop` input, closing a feedback loop; outputs `in` until the loop has a frame or while reset is on.",
                &[("in", false), ("loop", true)],
                vec![
                    ParamSchema::toggle("reset", "Reset", false)
                        .help("While on, outputs `in` and restarts the loop."),
                ],
                40,
            )
        },
        helpers: "",
        body: r#"
    let a = textureSample(in0, samp, uv);
    let b = textureSample(in1, samp, uv);
    return select(a, b, params.v[0].x > 0.5);"#,
        pack: |c| {
            let use_loop = c.inputs.get(1).is_some_and(Option::is_some) && !c.params.bool("reset");
            *Packed::default().push([if use_loop { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0])
        },
    },
    Def {
        schema: || {
            img(
                "blur",
                "Blur",
                "Gaussian blur.",
                &[("in", false)],
                vec![
                    ParamSchema::float("size", "Size", 4.0, Some(0.0), Some(256.0))
                        .help("Blur radius in pixels."),
                ],
                600,
            )
        },
        helpers: "",
        body: r#"
    let r = params.v[0].x;
    let px = params.v[0].yz;
    let sigma2 = max(2.0 * (r * 0.5) * (r * 0.5), 1e-4);
    var acc = vec4f(0.0);
    var wsum = 0.0;
    for (var j = -4; j <= 4; j++) {
        for (var i = -4; i <= 4; i++) {
            let o = vec2f(f32(i), f32(j)) * (r / 4.0);
            let w = exp(-dot(o, o) / sigma2);
            acc += textureSampleLevel(in0, samp, uv + o * px, 0.0) * w;
            wsum += w;
        }
    }
    return acc / wsum;"#,
        pack: |c| {
            let (w, h) = (c.output.width() as f32, c.output.height() as f32);
            *Packed::default().push([c.params.f32("size"), 1.0 / w, 1.0 / h, 0.0])
        },
    },
];
