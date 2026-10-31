//! Operators that read images (IMG inputs) on the GPU: Points from Image
//! (a grid of points coloured by an image), Points from Depth (a depth
//! image back-projected through a pinhole camera into a point cloud) and
//! Attribute from Image (an image looked up at each point into any
//! attribute).
//!
//! The generators' point count follows the image's size, which the CPU
//! knows, so their headers are CPU headers; they allocate only from the
//! attribute pool (rule 2). Attribute from Image dispatches from its
//! input's header, so it works on GPU-counted geometry too.

use std::sync::Arc;

use atd_core::geo::{AttrType, names};
use atd_core::registry::{OperatorSchema, ParamSchema, WireType};
use atd_gpu::engine::{GeoCtx, GeoOp, Params};
use atd_gpu::geo::kernels::{Bind, Packer};

use crate::image_kit::{self, ImageKernel};
use crate::kit::{self, Missing, desc, menu_index, typed_access, vec3};

/// Most points across an image.
pub const MAX_COLUMNS: i64 = 4096;

const MEASURES: &[&str] = &["luminance", "red", "green", "blue", "alpha"];

/// WGSL: the measure of a colour (`params.measure`, a [`MEASURES`] index).
const MEASURE: &str = "fn measure(c: vec4f, m: u32) -> f32 {
    switch m {
        case 1u: { return c.r; }
        case 2u: { return c.g; }
        case 3u: { return c.b; }
        case 4u: { return c.a; }
        default: { return dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)); }
    }
}
";

/// The grid a generator lays over an image of `size`: columns and rows.
pub(crate) fn grid(columns: i64, size: wgpu::Extent3d) -> (u32, u32) {
    let cols = columns.clamp(1, MAX_COLUMNS) as u32;
    let cols = cols.min(size.width.max(1));
    let rows = ((f64::from(cols) * f64::from(size.height) / f64::from(size.width.max(1))).round()
        as u32)
        .clamp(1, size.height.max(1));
    (cols, rows)
}

fn columns_param(default: i64) -> ParamSchema {
    ParamSchema::int("columns", "Columns", default, Some(1), Some(MAX_COLUMNS)).help(
        "Points across the image (at most one per pixel); the rows follow the image's aspect ratio, so the count is Columns × rows. Each point samples the image at its cell's centre.",
    )
}

/// Pooled attributes of `count` elements, by name and type.
fn alloc_all<const N: usize>(
    ctx: &mut GeoCtx<'_>,
    attrs: &[(Arc<str>, AttrType); N],
    count: u32,
) -> [atd_gpu::geo::Attribute; N] {
    std::array::from_fn(|k| ctx.alloc(attrs[k].0.clone(), attrs[k].1, count))
}

fn gpu(a: &atd_gpu::geo::Attribute) -> wgpu::Buffer {
    a.gpu_buffer().expect("allocated on the GPU").clone()
}

// ---------------------------------------------------------- Points from Image

pub fn points_schema() -> OperatorSchema {
    image_kit::schema(
        "points_from_image",
        "Points from Image",
        "A grid of points over an image, one per cell of Columns × rows, coloured by the image: writes `P` (in the XY plane, Width wide, centred on Centre, optionally pushed along Z by a measure of the pixel), `Cd` (the pixel's colour), `uv` (0-1, v up), `value` (the measure: luminance or a channel) and `id`. The count follows the image's size; with no image there are no points. Remove dark points with Delete on `value`.",
        &[("image", WireType::Img)],
        vec![
            columns_param(128),
            ParamSchema::float("width", "Width", 2.0, Some(0.0), None)
                .help("The grid's width; its height follows the image's aspect ratio."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("The grid's centre."),
            ParamSchema::menu("measure", "Measure", MEASURES, "luminance")
                .help("What `value` holds: the pixel's Rec. 709 luminance or one channel."),
            ParamSchema::float("displace", "Displace", 0.0, None, None)
                .help("Moves each point along +Z by Displace × value (a relief from the image)."),
            ParamSchema::toggle("bilinear", "Bilinear", true)
                .help("Interpolates between pixels; off takes the nearest pixel."),
        ],
        120,
    )
}

const POINTS: &str = "struct Params {
    centre: vec4f,
    // width, height, displace, unused
    size: vec4f,
    // columns, rows, measure, bilinear
    g: vec4u,
};
@group(0) @binding(3) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(5) var<storage, read_write> UV: array<vec2f>;
@group(0) @binding(6) var<storage, read_write> V: array<f32>;
@group(0) @binding(7) var<storage, read_write> ID: array<u32>;
fn point(i: u32) {
    let c = i % params.g.x;
    let r = i / params.g.x;
    // Row 0 is the bottom of the image (v up).
    let uv = (vec2f(f32(c), f32(r)) + vec2f(0.5)) / vec2f(f32(params.g.x), f32(params.g.y));
    let col = img0_sample(uv, params.g.w == 1u, false);
    let m = measure(col, params.g.z);
    let p = vec3f((uv.x - 0.5) * params.size.x, (uv.y - 0.5) * params.size.y, m * params.size.z);
    P[i] = vec4f(p + params.centre.xyz, 0.0);
    Cd[i] = col;
    UV[i] = uv;
    V[i] = m;
    ID[i] = i;
}
";

pub struct PointsFromImage {
    kernel: ImageKernel,
    attrs: [(Arc<str>, AttrType); 5],
}

pub fn create_points(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(PointsFromImage {
        kernel: ImageKernel::new(
            device,
            "pnt.points_from_image",
            1,
            &format!("{POINTS}{MEASURE}"),
            &[ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite],
            48,
        ),
        attrs: [
            (Arc::from(names::P), AttrType::VEC3),
            (Arc::from(names::CD), AttrType::VEC4),
            (Arc::from(names::UV), AttrType::VEC2),
            (Arc::from("value"), AttrType::F32),
            (Arc::from(names::ID), AttrType::U32),
        ],
    })
}

impl GeoOp for PointsFromImage {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(image) = ImageKernel::image(ctx, 0).cloned() else {
            ctx.output.clear();
            return Ok(());
        };
        let p = ctx.params;
        let size = image.size();
        let (cols, rows) = grid(p.i64("columns"), size);
        let count = cols * rows;
        let width = p.f32("width").max(0.0);
        let height = width * size.height as f32 / size.width.max(1) as f32;
        let mut u = Packer::<48>::new();
        u.vec4(vec3(&p, "centre"))
            .vec4([width, height, p.f32("displace"), 0.0])
            .u32(cols)
            .u32(rows)
            .u32(menu_index(MEASURES, p.str("measure")))
            .u32(u32::from(p.bool("bilinear")));
        let header = ctx.cpu_header(count);
        let out = alloc_all(ctx, &self.attrs, count);
        let b = [&out[0], &out[1], &out[2], &out[3], &out[4]].map(gpu);
        self.kernel.run(
            ctx,
            header.buffer(),
            [Some(&image), None],
            &[&b[0], &b[1], &b[2], &b[3], &b[4]],
            u.bytes(),
            Some(count),
        );
        let g = &mut *ctx.output;
        g.clear();
        g.set_points(count, Some(header));
        for a in out {
            g.set_point(a);
        }
        Ok(())
    }
}

// ---------------------------------------------------------- Points from Depth

const ENCODINGS: &[&str] = &["linear", "inverse", "scaled"];
const CAMERAS: &[&str] = &["fov", "pinhole"];

/// The depth back-projection shared with Sensor In: `fn depth_of(v)`
/// (metres from an image value), `fn valid(v, d)` and `fn unproject(uv,
/// d)` (camera space, Y up, looking down −Z), from `params.depth`
/// (near, far, scale, zero_invalid), `params.cam` (fx, fy, cx, cy as
/// fractions of the image: focal lengths over width and height) and
/// `params.enc` (encoding).
pub(crate) const DEPTH: &str = "fn depth_of(v: f32) -> f32 {
    let near = params.depth.x;
    let far = params.depth.y;
    switch params.enc {
        case 0u: { return mix(near, far, v); }
        case 1u: {
            let inv = mix(1.0 / far, 1.0 / near, clamp(v, 0.0, 1.0));
            return 1.0 / max(inv, 1e-9);
        }
        default: { return v * params.depth.z; }
    }
}
fn valid(v: f32, d: f32) -> bool {
    if (params.depth.w > 0.5 && v <= 0.0) { return false; }
    return d >= params.depth.x && d <= params.depth.y;
}
fn unproject(uv: vec2f, d: f32) -> vec3f {
    // uv has v up; the principal point and focal lengths are fractions of
    // the image (cx, cy from its top-left corner, as in OpenCV).
    let x = (uv.x - params.cam.z) / params.cam.x;
    let y = ((1.0 - uv.y) - params.cam.w) / params.cam.y;
    return vec3f(x * d, -y * d, -d);
}
";

pub fn depth_schema() -> OperatorSchema {
    image_kit::schema(
        "points_from_depth",
        "Points from Depth",
        "A depth image back-projected into a point cloud through a pinhole camera, one point per cell of Columns × rows: writes `P` (camera space: Y up, looking down −Z, then moved by Translate and Rotate), `Cd` (the colour image at the same place, or white), `uv`, `depth` (metres), `valid` (1, or 0 where the depth is missing or outside Near to Far: those points get a transparent `Cd`; remove them with Delete on `valid`) and `id`. Reads depth from AI depth estimation (inverse), a linear depth image, or a sensor's raw depth (scaled: a 16-bit image in millimetres is the default).",
        &[("depth", WireType::Img), ("colour", WireType::Img)],
        vec![
            columns_param(160),
            ParamSchema::menu("encoding", "Encoding", ENCODINGS, "inverse").help(
                "How a pixel's value (its red channel) gives depth: linear: Near at 0 to Far at 1; inverse: bright is near (disparity, as AI depth estimation outputs), Near at 1 to Far at 0; scaled: value × Scale metres (a sensor's raw depth).",
            ),
            ParamSchema::float("near", "Near", 0.5, Some(0.0), None).help("The nearest depth in metres (linear, inverse: at the end of the range; scaled: closer points are invalid)."),
            ParamSchema::float("far", "Far", 4.0, Some(0.0), None).help("The farthest depth in metres (farther points are invalid)."),
            ParamSchema::float("scale", "Scale", 65.535, Some(0.0), None).help("scaled: metres per unit of the value (65.535: a 16-bit image whose integer values are millimetres, as Kinect and Orbbec sensors give)."),
            ParamSchema::toggle("zero_invalid", "Zero Is Invalid", true).help("A value of 0 means no depth (sensors write 0 where they see nothing)."),
            ParamSchema::menu("camera", "Camera", CAMERAS, "fov").help("fov: a centred pinhole camera with the vertical field of view FOV; pinhole: the focal lengths and principal point below (from a sensor's calibration)."),
            ParamSchema::float("fov", "FOV", 60.0, Some(1.0), Some(179.0)).help("fov: the vertical field of view in degrees."),
            ParamSchema::vec("focal", "Focal", &[504.0, 504.0]).help("pinhole: fx, fy in pixels of the depth image."),
            ParamSchema::vec("principal", "Principal Point", &[320.0, 288.0]).help("pinhole: cx, cy in pixels from the image's top-left corner."),
            ParamSchema::vec("translate", "Translate", &[0.0, 0.0, 0.0]).help("Moves the cloud (the camera's position)."),
            ParamSchema::vec("rotate", "Rotate", &[0.0, 0.0, 0.0]).help("Rotates the cloud about X, Y, Z in degrees (the camera's orientation), before Translate."),
        ],
        150,
    )
}

const DEPTH_POINTS: &str = "struct Params {
    // near, far, scale, zero invalid
    depth: vec4f,
    // fx, fy, cx, cy as fractions of the image
    cam: vec4f,
    translate: vec4f,
    rotate: vec4f,
    cols: u32,
    rows: u32,
    enc: u32,
    // 1: a colour image is bound
    colour: u32,
};
@group(0) @binding(4) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(5) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(6) var<storage, read_write> UV: array<vec2f>;
@group(0) @binding(7) var<storage, read_write> D: array<f32>;
@group(0) @binding(8) var<storage, read_write> OK: array<f32>;
@group(0) @binding(9) var<storage, read_write> ID: array<u32>;
";

const DEPTH_BODY: &str = "fn point(i: u32) {
    let c = i % params.cols;
    let r = i / params.cols;
    let uv = (vec2f(f32(c), f32(r)) + vec2f(0.5)) / vec2f(f32(params.cols), f32(params.rows));
    // Nearest: interpolating across a depth edge invents points between
    // the foreground and the background.
    let v = img0_sample(uv, false, false).r;
    let d = depth_of(v);
    let ok = valid(v, d);
    P[i] = vec4f(atd_euler(unproject(uv, d), params.rotate.xyz) + params.translate.xyz, 0.0);
    var col = vec4f(1.0);
    if (params.colour == 1u) { col = img1_sample(uv, true, false); }
    if (!ok) { col = vec4f(0.0); }
    Cd[i] = col;
    UV[i] = uv;
    D[i] = d;
    OK[i] = select(0.0, 1.0, ok);
    ID[i] = i;
}
";

/// Camera intrinsics as fractions of an image of `size`: fx / width, fy /
/// height, cx / width, cy / height.
pub(crate) fn intrinsics(p: &Params<'_>, size: wgpu::Extent3d) -> [f32; 4] {
    let (w, h) = (size.width.max(1) as f32, size.height.max(1) as f32);
    if p.str("camera") == "pinhole" {
        let f = p.vec4("focal");
        let c = p.vec4("principal");
        [f[0].max(1e-6) / w, f[1].max(1e-6) / h, c[0] / w, c[1] / h]
    } else {
        let fy = 0.5 / (p.f32("fov").clamp(1.0, 179.0).to_radians() * 0.5).tan();
        // Square pixels: fx in pixels equals fy in pixels.
        [fy * h / w, fy, 0.5, 0.5]
    }
}

/// The depth uniforms (near, far, scale, zero invalid) of `p`.
pub(crate) fn depth_range(p: &Params<'_>) -> [f32; 4] {
    let near = p.f32("near").max(0.0);
    let far = p.f32("far").max(near + 1e-6);
    [
        near,
        far,
        p.f32("scale").max(0.0),
        if p.bool("zero_invalid") { 1.0 } else { 0.0 },
    ]
}

pub struct PointsFromDepth {
    kernel: ImageKernel,
    attrs: [(Arc<str>, AttrType); 6],
}

pub fn create_depth(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(PointsFromDepth {
        kernel: ImageKernel::new(
            device,
            "pnt.points_from_depth",
            2,
            &format!("{DEPTH_POINTS}{DEPTH}{DEPTH_BODY}"),
            &[
                ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite,
            ],
            80,
        ),
        attrs: [
            (Arc::from(names::P), AttrType::VEC3),
            (Arc::from(names::CD), AttrType::VEC4),
            (Arc::from(names::UV), AttrType::VEC2),
            (Arc::from("depth"), AttrType::F32),
            (Arc::from("valid"), AttrType::F32),
            (Arc::from(names::ID), AttrType::U32),
        ],
    })
}

impl GeoOp for PointsFromDepth {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(depth) = ImageKernel::image(ctx, 0).cloned() else {
            ctx.output.clear();
            return Ok(());
        };
        let colour = ImageKernel::image(ctx, 1).cloned();
        let p = ctx.params;
        let size = depth.size();
        let (cols, rows) = grid(p.i64("columns"), size);
        let count = cols * rows;
        let mut u = Packer::<80>::new();
        u.vec4(depth_range(&p))
            .vec4(intrinsics(&p, size))
            .vec4(vec3(&p, "translate"))
            .vec4(vec3(&p, "rotate"))
            .u32(cols)
            .u32(rows)
            .u32(menu_index(ENCODINGS, p.str("encoding")))
            .u32(u32::from(colour.is_some()));
        let header = ctx.cpu_header(count);
        let out = alloc_all(ctx, &self.attrs, count);
        let b = [&out[0], &out[1], &out[2], &out[3], &out[4], &out[5]].map(gpu);
        self.kernel.run(
            ctx,
            header.buffer(),
            [Some(&depth), colour.as_ref()],
            &[&b[0], &b[1], &b[2], &b[3], &b[4], &b[5]],
            u.bytes(),
            Some(count),
        );
        let g = &mut *ctx.output;
        g.clear();
        g.set_points(count, Some(header));
        for a in out {
            g.set_point(a);
        }
        Ok(())
    }
}

// -------------------------------------------------------- Attribute from Image

const LOOKUPS: &[&str] = &["uv", "planar", "index"];
const PLANES: &[&str] = &["xy", "xz", "zy"];
const IMAGE_OPS: &[&str] = &["set", "add", "multiply"];

pub fn attribute_schema() -> OperatorSchema {
    image_kit::schema(
        "attribute_from_image",
        "Attribute from Image",
        "Looks an image up at each point and writes the colour, or a measure of it, into a point attribute of any type (vec4 gets the colour, vec3 its RGB, vec2 its red and green, float and int the measure): looked up by a uv attribute, by `P` projected on a plane through a rectangle, or by point number (row by row from the top-left, for data images). Set, add or multiply, blended by Amount.",
        &[("in", WireType::Geo), ("image", WireType::Img)],
        vec![
            ParamSchema::string("name", "Name", "Cd").help("The attribute written (created at zero when missing)."),
            ParamSchema::menu("type", "Type", kit::TYPES, "vec4").help("Its type; an attribute of another type with the same name is replaced."),
            ParamSchema::menu("measure", "Measure", MEASURES, "luminance").help("float, int: the pixel's luminance or one channel."),
            ParamSchema::menu("lookup", "Lookup", LOOKUPS, "uv").help("uv: the attribute UV Attribute (0-1, v up); planar: P projected on Plane, mapped through the rectangle Centre ± Size / 2 to the whole image; index: point i reads pixel i, row by row from the top-left."),
            ParamSchema::string("uv", "UV Attribute", "uv").help("uv: the vec2 (or vec3, vec4: its first two components) attribute read."),
            ParamSchema::menu("plane", "Plane", PLANES, "xy").help("planar: the plane P is projected on (its first axis is the image's horizontal)."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("planar: the centre of the rectangle the image covers."),
            ParamSchema::vec("size", "Size", &[2.0, 2.0]).help("planar: the rectangle's size along the plane's two axes."),
            ParamSchema::toggle("bilinear", "Bilinear", true).help("Interpolates between pixels; off takes the nearest pixel."),
            ParamSchema::toggle("repeat", "Repeat", false).help("Repeats the image outside 0-1; off holds its edge pixels."),
            ParamSchema::menu("operation", "Operation", IMAGE_OPS, "set").help("set: the looked-up value; add, multiply: combined with the attribute's value."),
            ParamSchema::float("amount", "Amount", 1.0, None, None).help("Blends from the attribute's value (0) to the result (1)."),
        ],
        80,
    )
}

const ATTR: &str = "struct Params {
    centre: vec4f,
    // size x, y, amount, unused
    s: vec4f,
    d: vec4u,
    du: vec4u,
    lookup: u32,
    plane: u32,
    measure: u32,
    op: u32,
    bilinear: u32,
    repeat: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(3) var<storage, read_write> A: array<u32>;
@group(0) @binding(4) var<storage, read> P: array<vec4f>;
@group(0) @binding(5) var<storage, read> U: array<u32>;
";

const ATTR_BODY: &str = "fn point(i: u32) {
    var uv = vec2f(0.0);
    switch params.lookup {
        case 0u: { uv = ld_U(i).xy; }
        case 1u: {
            let q = P[i].xyz - params.centre.xyz;
            var a = q.xy;
            if (params.plane == 1u) { a = vec2f(q.x, -q.z); }
            if (params.plane == 2u) { a = vec2f(-q.z, q.y); }
            uv = a / max(params.s.xy, vec2f(1e-9)) + vec2f(0.5);
        }
        default: {
            let size = textureDimensions(img0);
            let x = i % size.x;
            let y = i / size.x;
            uv = (vec2f(f32(x), f32(size.y) - 1.0 - f32(y)) + vec2f(0.5)) / vec2f(size);
        }
    }
    let c = img0_sample(uv, params.bilinear == 1u, params.repeat == 1u);
    var v = c;
    if (params.d.x == 1u) { v = vec4f(measure(c, params.measure), 0.0, 0.0, 0.0); }
    let old = ld_A(i);
    var r = v;
    if (params.op == 1u) { r = old + v; }
    if (params.op == 2u) { r = old * v; }
    st_A(i, mix(old, r, params.s.z));
}
";

pub struct AttributeFromImage {
    kernel: ImageKernel,
    dummy: wgpu::Buffer,
}

pub fn create_attribute(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(AttributeFromImage {
        kernel: ImageKernel::new(
            device,
            "pnt.attribute_from_image",
            1,
            &format!(
                "{ATTR}{}{}{MEASURE}{ATTR_BODY}",
                typed_access("A", "params.d", false),
                typed_access("U", "params.du", true)
            ),
            &[ReadWrite, Read, Read],
            96,
        ),
        dummy: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.attribute_from_image.unused"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        }),
    })
}

impl GeoOp for AttributeFromImage {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        let image = ImageKernel::image(ctx, 1).cloned();
        let Some(image) = image.filter(|_| g.points() > 0) else {
            *ctx.output = g;
            return Ok(());
        };
        let p = ctx.params;
        let name = p.str("name");
        if !atd_core::geo::valid_name(name) {
            return Err(format!("`{name}` is not a valid attribute name"));
        }
        let ty = kit::type_named(p.str("type"));
        let lookup = menu_index(LOOKUPS, p.str("lookup"));
        let (uv, uv_ty) = if lookup == 0 {
            let uv_name = p.str("uv");
            if uv_name == name {
                return Err(format!("`{name}` cannot be both the lookup and the result"));
            }
            let (b, t) = kit::readable_any(ctx, &g, uv_name)?;
            if t.comps() < 2 {
                return Err(format!("the uv attribute `{uv_name}` needs two components"));
            }
            (b, t)
        } else {
            (self.dummy.clone(), AttrType::VEC2)
        };
        let pos = if lookup == 1 {
            if name == names::P {
                return Err("planar lookup reads `P`; write another attribute".into());
            }
            kit::readable(ctx, &g, names::P, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        let hdr = kit::header_of(&g)?;
        let a = kit::writable(ctx, &mut g, name, ty, Missing::Zero)?;
        let s = p.vec4("size");
        let mut u = Packer::<96>::new();
        u.vec4(vec3(&p, "centre"))
            .vec4([s[0], s[1], p.f32("amount"), 0.0]);
        for w in desc(ty).into_iter().chain(desc(uv_ty)) {
            u.u32(w);
        }
        u.u32(lookup)
            .u32(menu_index(PLANES, p.str("plane")))
            .u32(menu_index(MEASURES, p.str("measure")))
            .u32(menu_index(IMAGE_OPS, p.str("operation")))
            .u32(u32::from(p.bool("bilinear")))
            .u32(u32::from(p.bool("repeat")));
        self.kernel.run(
            ctx,
            &hdr,
            [Some(&image), None],
            &[&a, &pos, &uv],
            u.bytes(),
            None,
        );
        *ctx.output = g;
        Ok(())
    }
}
