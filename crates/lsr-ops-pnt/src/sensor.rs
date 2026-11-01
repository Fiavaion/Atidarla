//! Sensor In (FR-C3): depth and body tracking from a depth sensor, as
//! points.
//!
//! **Capability (PRD A19, CLAUDE.md rule 6).** Live capture talks to the
//! Azure Kinect Sensor SDK's C API (`k4a`), which Orbbec's Femto cameras
//! also speak through Orbbec's K4A wrapper; it is Windows-first, so the
//! operator carries the flag `win+sdk:k4a`. Where the flag does not hold
//! (macOS, Linux, or Windows without the SDK) the fallback is `Degrade`:
//! the node still loads and runs its other sources (a built-in test scene
//! with a moving figure, and recordings played from point-cache
//! sequences), and choosing the `k4a` device reports why it is not
//! available. No SDK is linked into this crate: the capture backend is to
//! come as a plugin crate that loads the SDK at run time
//! (`docs/point-caches.md`, "Sensor In"), so `cargo deny` and the macOS
//! build are unaffected.
//!
//! **Outputs.** `points`: one point per cell of a Columns × rows grid over
//! the depth image, back-projected like Points from Depth (`P`, `Cd`,
//! `depth`, `valid`, `body`: the tracked body's index or −1, `id`).
//! `body`: the 32 joints of each tracked body in the K4A joint order (`P`,
//! `Cd`, `joint`, `body`, `id`) with the bones as two-point polylines.

use std::sync::Arc;

use lsr_core::capability::{CapabilityRequirement, Fallback};
use lsr_core::geo::{AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::kernels::{Bind, Packer};
use lsr_gpu::geo::{Attribute, Geometry, PrimKind, Topology};

use crate::file_in::Loader;
use crate::from_image::{DEPTH, MAX_COLUMNS};
use crate::kit::{PointKernel, vec3};

/// The capability flag of live capture.
pub const FLAG: &str = "win+sdk:k4a";

/// Shown where live capture is not available (the A19 fallback).
pub const UNAVAILABLE: &str = "Live depth capture uses the Azure Kinect Sensor SDK (or Orbbec's K4A wrapper for Femto cameras) on Windows. Elsewhere Sensor In plays recordings (point-cache sequences) and its test scene; the k4a device reports that it is not available.";

/// Joints per body, in the K4A order.
pub const JOINTS: usize = 32;

/// The K4A joint names, in order (`joint` attribute values).
pub const JOINT_NAMES: [&str; JOINTS] = [
    "pelvis",
    "spine_navel",
    "spine_chest",
    "neck",
    "clavicle_left",
    "shoulder_left",
    "elbow_left",
    "wrist_left",
    "hand_left",
    "handtip_left",
    "thumb_left",
    "clavicle_right",
    "shoulder_right",
    "elbow_right",
    "wrist_right",
    "hand_right",
    "handtip_right",
    "thumb_right",
    "hip_left",
    "knee_left",
    "ankle_left",
    "foot_left",
    "hip_right",
    "knee_right",
    "ankle_right",
    "foot_right",
    "head",
    "nose",
    "eye_left",
    "ear_left",
    "eye_right",
    "ear_right",
];

/// Each joint's parent (the pelvis is the root).
const PARENT: [usize; JOINTS] = [
    0, 0, 1, 2, 2, 4, 5, 6, 7, 8, 7, 2, 11, 12, 13, 14, 15, 14, 0, 18, 19, 20, 0, 22, 23, 24, 3,
    26, 26, 26, 26, 26,
];

/// The capsule radius of the bone ending at each joint (test scene).
const RADIUS: [f32; JOINTS] = [
    0.0, 0.13, 0.15, 0.06, 0.05, 0.06, 0.05, 0.04, 0.035, 0.025, 0.02, 0.05, 0.06, 0.05, 0.04,
    0.035, 0.025, 0.02, 0.08, 0.07, 0.05, 0.04, 0.08, 0.07, 0.05, 0.04, 0.1, 0.03, 0.015, 0.02,
    0.015, 0.02,
];

/// The test scene's depth image size (a binned K4A narrow field of view).
const TEST_SIZE: wgpu::Extent3d = wgpu::Extent3d {
    width: 320,
    height: 288,
    depth_or_array_layers: 1,
};
/// The test camera's vertical field of view (the K4A NFOV mode's).
const TEST_FOV: f32 = 65.0;

fn rot(v: [f32; 2], a: f32) -> [f32; 2] {
    let (s, c) = a.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

/// The test figure's joints at time `t` (seconds), in camera space: a
/// person 2.2 m away, swaying and waving their right arm.
pub fn test_pose(t: f32) -> [[f32; 3]; JOINTS] {
    let mut j = [
        [0.0, 0.0, 0.0],
        [0.0, 0.2, 0.0],
        [0.0, 0.42, 0.0],
        [0.0, 0.62, 0.0],
        [0.06, 0.58, 0.0],
        [0.19, 0.56, 0.0],
        [0.23, 0.3, 0.0],
        [0.25, 0.06, 0.02],
        [0.26, -0.02, 0.02],
        [0.27, -0.1, 0.02],
        [0.23, -0.03, 0.06],
        [-0.06, 0.58, 0.0],
        [-0.19, 0.56, 0.0],
        [0.0; 3],
        [0.0; 3],
        [0.0; 3],
        [0.0; 3],
        [0.0; 3],
        [0.1, -0.02, 0.0],
        [0.11, -0.45, 0.02],
        [0.11, -0.88, 0.0],
        [0.11, -0.93, 0.12],
        [-0.1, -0.02, 0.0],
        [-0.11, -0.45, 0.02],
        [-0.11, -0.88, 0.0],
        [-0.11, -0.93, 0.12],
        [0.0, 0.76, 0.0],
        [0.0, 0.75, 0.1],
        [0.03, 0.79, 0.08],
        [0.07, 0.77, 0.0],
        [-0.03, 0.79, 0.08],
        [-0.07, 0.77, 0.0],
    ];
    // The right arm: the upper arm raised and swinging, the forearm
    // waving.
    let w = std::f32::consts::TAU * 0.8;
    let a1 = 2.3 + 0.25 * (w * t).sin();
    let a2 = a1 - 0.5 + 0.6 * (w * t + 1.0).sin();
    let sh = j[12];
    let e = rot([0.0, -0.27], -a1);
    let elbow = [sh[0] + e[0], sh[1] + e[1], 0.03];
    let f = rot([0.0, -0.25], -a2);
    let wrist = [elbow[0] + f[0], elbow[1] + f[1], 0.05];
    let h = rot([0.0, -0.08], -a2);
    j[13] = elbow;
    j[14] = wrist;
    j[15] = [wrist[0] + h[0], wrist[1] + h[1], 0.05];
    j[16] = [wrist[0] + 2.0 * h[0], wrist[1] + 2.0 * h[1], 0.05];
    j[17] = [wrist[0] + h[0] * 0.6 + 0.03, wrist[1] + h[1] * 0.6, 0.09];
    // The whole figure sways side to side and leans with it.
    let sway = 0.15 * (0.5 * t).sin();
    let lean = 0.05 * (0.5 * t).sin();
    for p in &mut j {
        let r = rot([p[0], p[1]], -lean * p[1].max(0.0));
        *p = [r[0] + sway, r[1] - 0.07, p[2] - 2.2];
    }
    j
}

const DEVICES: &[&str] = &["test", "recording", "k4a"];
const OUTPUTS: &[&str] = &["points", "body"];

pub fn schema() -> OperatorSchema {
    let mut s = crate::pnt(
        "sensor_in",
        "Sensor In",
        "Depth and body tracking from a depth sensor, as points (FR-C3). Devices: test, a built-in scene (a waving figure in front of a wall, 320 × 288 depth with sensor noise and its body tracking); recording, a sequence of point caches played at Recording FPS (looping); k4a, a live Azure Kinect or Orbbec Femto camera (Windows with the K4A SDK; see Platforms). Output points: the depth image as a point cloud (`P` in camera space, Y up, looking down −Z, then Rotate and Translate; `Cd`, `depth`, `valid`, `body`: the tracked body's index or −1, `id`); body: the 32 joints of each tracked body in the K4A order (`joint` 0 pelvis … 26 head … 31 ear_right, `body`, `Cd`) with the bones as lines.",
        &[],
        vec![
            ParamSchema::menu("device", "Device", DEVICES, "test").help("test: the built-in scene; recording: point caches from Recording; k4a: a live Azure Kinect or Orbbec Femto camera (where available)."),
            ParamSchema::menu("output", "Output", OUTPUTS, "points").help("points: the depth as a point cloud; body: the tracked joints and bones (recording: whatever the files hold)."),
            ParamSchema::int("columns", "Columns", 160, Some(1), Some(MAX_COLUMNS)).help("points: points across the depth image (at most one per pixel); the rows follow its aspect ratio."),
            ParamSchema::float("near", "Near", 0.5, Some(0.0), None).help("Depth closer than this (metres) is invalid."),
            ParamSchema::float("far", "Far", 4.0, Some(0.0), None).help("Depth farther than this (metres) is invalid."),
            ParamSchema::path("recording", "Recording").help("recording: a point-cache sequence with `$F` for the frame number (for example `capture/depth.$F4.lsrpc`, made with `lasair points convert`)."),
            ParamSchema::float("recording_fps", "Recording FPS", 30.0, Some(0.1), None).help("recording: frames per second of the recording."),
            ParamSchema::int("first", "First Frame", 1, None, None).help("recording: the first file's frame number."),
            ParamSchema::int("length", "Length", 300, Some(1), None).help("recording: frames in the recording; playback loops over them."),
            ParamSchema::vec("translate", "Translate", &[0.0, 0.0, 0.0]).help("Moves the result (the sensor's position)."),
            ParamSchema::vec("rotate", "Rotate", &[0.0, 0.0, 0.0]).help("Rotates the result about X, Y, Z in degrees (the sensor's orientation), before Translate."),
        ],
        200,
    );
    s.capabilities = vec![CapabilityRequirement {
        flag: FLAG.parse().expect("valid capability flag"),
        fallback: Fallback::Degrade {
            explanation: UNAVAILABLE.into(),
        },
    }];
    s.time_dependent = true;
    s
}

const SCENE: &str = "struct Params {
    // near, far, unused, zero invalid (0)
    depth: vec4f,
    // fx, fy, cx, cy as fractions of the image
    cam: vec4f,
    translate: vec4f,
    rotate: vec4f,
    cols: u32,
    rows: u32,
    enc: u32,
    frame: u32,
};
@group(0) @binding(2) var<storage, read_write> P: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> Cd: array<vec4f>;
@group(0) @binding(4) var<storage, read_write> D: array<f32>;
@group(0) @binding(5) var<storage, read_write> OK: array<f32>;
@group(0) @binding(6) var<storage, read_write> BODY: array<i32>;
@group(0) @binding(7) var<storage, read_write> ID: array<u32>;
// Per joint: its position and the radius of the bone to it (w), and its
// parent's position.
@group(0) @binding(8) var<storage, read> bones: array<vec4f, 64>;
";

const SCENE_BODY: &str = "fn capsule(ro: vec3f, rd: vec3f, a: vec3f, b: vec3f, r: f32) -> f32 {
    // The nearest hit of a ray on a capsule, or -1 (after Inigo Quilez's
    // closed form).
    let ba = b - a;
    let oa = ro - a;
    let baba = dot(ba, ba);
    let bard = dot(ba, rd);
    let baoa = dot(ba, oa);
    let rdoa = dot(rd, oa);
    let oaoa = dot(oa, oa);
    let qa = baba - bard * bard;
    let qb = baba * rdoa - baoa * bard;
    let qc = baba * oaoa - baoa * baoa - r * r * baba;
    let h = qb * qb - qa * qc;
    if (h >= 0.0 && qa > 1e-12) {
        let t = (-qb - sqrt(h)) / qa;
        let y = baoa + t * bard;
        if (y > 0.0 && y < baba) { return t; }
        var oc = oa;
        if (y > 0.0) { oc = ro - b; }
        let bb = dot(rd, oc);
        let cc = dot(oc, oc) - r * r;
        let hh = bb * bb - cc;
        if (hh > 0.0) { return -bb - sqrt(hh); }
    }
    return -1.0;
}
fn point(i: u32) {
    let c = i % params.cols;
    let r = i / params.cols;
    let uv = (vec2f(f32(c), f32(r)) + vec2f(0.5)) / vec2f(f32(params.cols), f32(params.rows));
    // The ray through the cell (depth 1 along -Z).
    let dir = unproject(uv, 1.0);
    let rd = normalize(dir);
    // The wall at 3.6 m and the floor 1 m below the sensor.
    var d = 3.6;
    var col = vec3f(0.55, 0.56, 0.6);
    if (dir.y < 0.0) {
        let df = -1.0 / dir.y;
        if (df < d) {
            d = df;
            let q = dir * df;
            let k = (i32(floor(q.x * 2.0)) + i32(floor(q.z * 2.0))) & 1;
            col = select(vec3f(0.32, 0.3, 0.28), vec3f(0.42, 0.4, 0.37), k == 1);
        }
    }
    var body = -1;
    for (var j = 1u; j < 32u; j = j + 1u) {
        let a = bones[j * 2u];
        let b = bones[j * 2u + 1u];
        let t = capsule(vec3f(0.0), rd, b.xyz, a.xyz, a.w);
        if (t > 0.0) {
            // Depth along -Z, not along the ray.
            let dz = t * -rd.z;
            if (dz < d) {
                d = dz;
                body = 0;
                col = vec3f(0.9, 0.55, 0.35) * (0.6 + 0.4 * -rd.z);
            }
        }
    }
    // Sensor noise that grows with depth (about 1 mm at 1 m).
    d = d + (lsr_rand(params.frame, i) - 0.5) * 0.002 * d * d;
    let ok = valid(1.0, d);
    P[i] = vec4f(lsr_euler(dir * d, params.rotate.xyz) + params.translate.xyz, 0.0);
    Cd[i] = select(vec4f(0.0), vec4f(col, 1.0), ok);
    D[i] = d;
    OK[i] = select(0.0, 1.0, ok);
    BODY[i] = select(-1, body, ok);
    ID[i] = i;
}
";

pub struct SensorIn {
    scene: PointKernel,
    bones: wgpu::Buffer,
    /// This frame's joint positions, copied into the output's `P`.
    joints: wgpu::Buffer,
    loader: Loader,
    /// The recording file of the last request, and its frame.
    recording: String,
    frame: i64,
    attrs: [(Arc<str>, AttrType); 6],
    joint_names: [Arc<str>; 5],
    /// The bones as polylines (made once; every output shares it).
    topology: Topology,
    /// Constant joint attributes: `joint`, `body`, `id`, `Cd`.
    joint_attrs: [Attribute; 4],
}

pub fn create(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    let bones: Vec<[u32; 2]> = (1..JOINTS as u32)
        .map(|j| [PARENT[j as usize] as u32, j])
        .collect();
    let prims: Vec<(PrimKind, &[u32])> =
        bones.iter().map(|b| (PrimKind::Polyline, &b[..])).collect();
    let joint: Vec<u8> = (0..JOINTS as i32).flat_map(i32::to_le_bytes).collect();
    let ids: Vec<u32> = (0..JOINTS as u32).collect();
    let colours: Vec<[f32; 4]> = (0..JOINTS)
        .map(|j| match j {
            4..=10 => [0.3, 0.8, 1.0, 1.0],
            11..=17 => [1.0, 0.5, 0.3, 1.0],
            18..=25 => [0.5, 1.0, 0.4, 1.0],
            26..=31 => [1.0, 1.0, 0.6, 1.0],
            _ => [1.0, 1.0, 1.0, 1.0],
        })
        .collect();
    Box::new(SensorIn {
        scene: PointKernel::new(
            device,
            "pnt.sensor_in.test",
            &format!("{SCENE}{DEPTH}{SCENE_BODY}"),
            &[
                ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite, ReadWrite, Read,
            ],
            80,
        ),
        bones: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.sensor_in.bones"),
            size: (JOINTS * 2 * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        joints: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pnt.sensor_in.joints"),
            size: (JOINTS * 16) as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        loader: Loader::default(),
        recording: String::new(),
        frame: i64::MIN,
        attrs: [
            (Arc::from(names::P), AttrType::VEC3),
            (Arc::from(names::CD), AttrType::VEC4),
            (Arc::from("depth"), AttrType::F32),
            (Arc::from("valid"), AttrType::F32),
            (Arc::from("body"), AttrType::I32),
            (Arc::from(names::ID), AttrType::U32),
        ],
        joint_names: [names::P, "joint", "body", names::ID, names::CD].map(Arc::from),
        topology: Topology::from_prims(&prims),
        joint_attrs: [
            Attribute::cpu("joint", AttrType::I32, joint),
            Attribute::cpu(
                "body",
                AttrType::I32,
                [0i32; JOINTS]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
            Attribute::from_u32(names::ID, &ids),
            Attribute::from_vec4(names::CD, &colours),
        ],
    })
}

/// A point rotated by Euler angles in degrees (X, then Y, then Z) and
/// moved: as `lsr_euler` in the kernels.
fn place(p: [f32; 3], rotate: [f32; 4], translate: [f32; 4]) -> [f32; 3] {
    let [ax, ay, az] = [rotate[0], rotate[1], rotate[2]].map(f32::to_radians);
    let [mut x, mut y, mut z] = p;
    (y, z) = (y * ax.cos() - z * ax.sin(), y * ax.sin() + z * ax.cos());
    (x, z) = (x * ay.cos() + z * ay.sin(), -x * ay.sin() + z * ay.cos());
    (x, y) = (x * az.cos() - y * az.sin(), x * az.sin() + y * az.cos());
    [x + translate[0], y + translate[1], z + translate[2]]
}

impl SensorIn {
    fn test_points(&mut self, ctx: &mut GeoCtx<'_>, joints: &[[f32; 3]; JOINTS]) {
        let p = ctx.params;
        let (cols, rows) = crate::from_image::grid(p.i64("columns"), TEST_SIZE);
        let count = cols * rows;
        let mut bones = [0u8; JOINTS * 2 * 16];
        for (j, chunk) in bones.chunks_exact_mut(32).enumerate() {
            let a = joints[j];
            let b = joints[PARENT[j]];
            let words = [a[0], a[1], a[2], RADIUS[j], b[0], b[1], b[2], 0.0];
            for (w, out) in words.iter().zip(chunk.chunks_exact_mut(4)) {
                out.copy_from_slice(&w.to_le_bytes());
            }
        }
        ctx.queue.write_buffer(&self.bones, 0, &bones);
        let fy = 0.5 / (TEST_FOV.to_radians() * 0.5).tan();
        let aspect = TEST_SIZE.height as f32 / TEST_SIZE.width as f32;
        let mut u = Packer::<80>::new();
        u.vec4(range(&p))
            .vec4([fy * aspect, fy, 0.5, 0.5])
            .vec4(vec3(&p, "translate"))
            .vec4(vec3(&p, "rotate"))
            .u32(cols)
            .u32(rows)
            .u32(2)
            .u32(ctx.frame as u32);
        let header = ctx.cpu_header(count);
        let out: [Attribute; 6] =
            std::array::from_fn(|k| ctx.alloc(self.attrs[k].0.clone(), self.attrs[k].1, count));
        let b = [&out[0], &out[1], &out[2], &out[3], &out[4], &out[5]]
            .map(|a| a.gpu_buffer().expect("on the GPU").clone());
        self.scene.run(
            ctx,
            header.buffer(),
            &[&b[0], &b[1], &b[2], &b[3], &b[4], &b[5], &self.bones],
            u.bytes(),
        );
        let g = &mut *ctx.output;
        g.clear();
        g.set_points(count, Some(header));
        for a in out {
            g.set_point(a);
        }
    }

    fn test_body(&mut self, ctx: &mut GeoCtx<'_>, joints: &[[f32; 3]; JOINTS]) {
        let p = ctx.params;
        let (r, t) = (vec3(&p, "rotate"), vec3(&p, "translate"));
        let mut bytes = [0u8; JOINTS * 16];
        for (j, chunk) in bytes.chunks_exact_mut(16).enumerate() {
            let q = place(joints[j], r, t);
            for (w, out) in q.iter().zip(chunk.chunks_exact_mut(4)) {
                out.copy_from_slice(&w.to_le_bytes());
            }
        }
        // Written to a buffer of the node's own and copied in the frame's
        // encoder: a pooled attribute is cleared in the encoder offline,
        // after any `write_buffer` to it would have landed.
        ctx.queue.write_buffer(&self.joints, 0, &bytes);
        let pos = ctx.alloc(self.joint_names[0].clone(), AttrType::VEC3, JOINTS as u32);
        ctx.encoder.copy_buffer_to_buffer(
            &self.joints,
            0,
            pos.gpu_buffer().expect("on the GPU"),
            0,
            bytes.len() as u64,
        );
        let header = ctx.cpu_header(JOINTS as u32);
        let g = &mut *ctx.output;
        g.clear();
        g.set_points(JOINTS as u32, Some(header));
        g.set_point(pos);
        for a in &self.joint_attrs {
            g.set_point(a.clone());
        }
        g.set_topology(Some(self.topology.clone()));
    }

    fn recording(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let file = p.str("recording");
        if file.is_empty() {
            self.loader.clear();
            self.recording.clear();
            ctx.output.clear();
            return Ok(());
        }
        let length = p.i64("length").max(1);
        let fps = f64::from(p.f32("recording_fps").max(0.1));
        let frame = p.i64("first") + ((ctx.time * fps).floor() as i64).rem_euclid(length);
        if file != self.recording || frame != self.frame {
            self.recording.clear();
            self.recording.push_str(file);
            self.frame = frame;
            let path = p.resolve(&crate::cache::frame_path(file, frame));
            self.loader.request(ctx, path);
        }
        if self.loader.poll(p.mode()) {
            ctx.cook_again = true;
        }
        if let Some(e) = self.loader.error() {
            return Err(e.to_owned());
        }
        *ctx.output = self.loader.geometry().clone();
        Ok(())
    }
}

fn range(p: &Params<'_>) -> [f32; 4] {
    let near = p.f32("near").max(0.0);
    [near, p.f32("far").max(near + 1e-6), 0.0, 0.0]
}

impl GeoOp for SensorIn {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        match p.str("device") {
            "recording" => self.recording(ctx),
            "k4a" => {
                *ctx.output = Geometry::new();
                Err(if cfg!(windows) {
                    "the k4a capture backend is not in this build yet: Sensor In plays its test scene and recordings (docs/point-caches.md)"
                } else {
                    UNAVAILABLE
                }
                .into())
            }
            _ => {
                let joints = test_pose(ctx.time as f32);
                if p.str("output") == "body" {
                    self.test_body(ctx, &joints);
                } else {
                    self.test_points(ctx, &joints);
                }
                Ok(())
            }
        }
    }

    fn status(&self) -> Option<&str> {
        self.loader.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_figure_stands_on_the_floor_in_view() {
        for t in [0.0, 0.7, 3.0] {
            let j = test_pose(t);
            for (k, p) in j.iter().enumerate() {
                assert!(p.iter().all(|c| c.is_finite()), "{}", JOINT_NAMES[k]);
                assert!(p[2] < -1.8 && p[2] > -2.6, "{} at {p:?}", JOINT_NAMES[k]);
            }
            // Feet near the floor 1 m below the sensor, head above it.
            assert!((j[20][1] + 0.95).abs() < 0.05, "{:?}", j[20]);
            assert!(j[26][1] > 0.5);
            // The waving hand is above the shoulder.
            assert!(j[15][1] > j[12][1]);
        }
        assert_eq!(PARENT[0], 0);
        assert!(PARENT.iter().enumerate().skip(1).all(|(j, &p)| p < j));
    }

    #[test]
    fn placing_matches_the_kernels_euler_order() {
        let q = place([1.0, 0.0, 0.0], [0.0, 90.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]);
        assert!((q[0]).abs() < 1e-6 && (q[1] - 1.0).abs() < 1e-6 && (q[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_capability_falls_back_to_degraded_running() {
        let s = schema();
        let req = &s.capabilities[0];
        assert_eq!(req.flag.to_string(), FLAG);
        assert!(matches!(req.fallback, Fallback::Degrade { .. }));
        let mac = lsr_core::platform::PlatformInfo {
            os: lsr_core::platform::Os::MacOs,
            ..lsr_core::platform::PlatformInfo::host()
        };
        assert!(!req.flag.is_satisfied_by(&mac));
        let win = lsr_core::platform::PlatformInfo {
            os: lsr_core::platform::Os::Windows,
            ..lsr_core::platform::PlatformInfo::host()
        }
        .with_sdk("k4a");
        assert!(req.flag.is_satisfied_by(&win));
    }
}
