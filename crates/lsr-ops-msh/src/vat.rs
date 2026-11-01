//! VAT In: plays vertex animation textures in the layout of Houdini's
//! Vertex Animation Textures 3.0 export (FR-C2), from its public
//! description: a mesh, a position texture whose rows hold one block per
//! frame, optional rotation, colour and lookup textures, and the data file
//! with the frame count and the bounds. The reading rules are in
//! `docs/design/vat-in.md`.
//!
//! Playback is a CPU cook like every MSH operator (rule 1: on a worker,
//! never the render thread): the files are decoded once and kept, and each
//! frame change builds the frame's mesh from them. That keeps every mode
//! in one place, including fluid, whose topology changes every frame, and
//! hands PNT and the renderer an ordinary GEO value.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use lsr_core::geo::names;
use lsr_core::registry::ParamSchema;
use serde_json::Value;

use crate::cook::{Job, Snap};
use crate::io::{self, ObjPoints};
use crate::mesh::{Column, Mesh, Values, v3};
use crate::normals::point_normals;

/// Largest texture accepted (pixels).
pub const MAX_PIXELS: usize = 16 << 20;

pub fn params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("mode", "Mode", &["soft", "rigid", "fluid", "sprite"], "soft").help(
            "The export's mode: soft (one texel per point, fixed topology), rigid (one texel per piece: its position and rotation), fluid (topology changing per frame; unused triangles collapse), sprite (one texel per particle quad).",
        ),
        ParamSchema::path("mesh", "Mesh")
            .help("The exported mesh (.obj, .ply, .gltf, .glb). Points find their texel from `uv2` (glTF TEXCOORD_1), else by their order."),
        ParamSchema::path("position", "Position Texture").help(
            "The position texture (.exr: positions as they are; .png: 0 to 1 across the data file's bounds).",
        ),
        ParamSchema::path("rotation", "Rotation Texture")
            .help("Rigid only: piece rotations as quaternions (x, y, z, w; .png stores them as 0.5 + q / 2)."),
        ParamSchema::path("colour", "Colour Texture")
            .help("Optional: writes the point colour `Cd` (RGBA) per frame."),
        ParamSchema::path("lookup", "Lookup Texture")
            .help("Fluid only, optional: per point and frame, where its position is in the position texture (red, green: 0 to 1 across it; alpha below 0.5: unused)."),
        ParamSchema::path("data", "Data File")
            .help("The export's JSON data file: frame count (`_frameCount`) and bounds (`_boundingMin` / `_boundingMax` X, Y, Z)."),
        ParamSchema::float("frame", "Frame", 0.0, None, None)
            .help("The frame shown, from 0; drive it with an expression such as `time * 24` to play."),
        ParamSchema::int("frames", "Frames", 0, Some(0), None)
            .help("Number of frames; 0 takes it from the data file, else from the texture height."),
        ParamSchema::menu("positions", "Positions", &["absolute", "offset"], "absolute")
            .help("Soft and fluid: absolute: the texture holds positions; offset: it holds offsets added to the mesh's points."),
        ParamSchema::toggle("loop", "Loop", true)
            .help("Frames past the end start again (else the last frame holds)."),
        ParamSchema::toggle("interpolate", "Interpolate", true)
            .help("Blends between neighbouring frames (not in fluid mode, whose topology changes)."),
    ]
}

/// A decoded texture: RGBA floats, row 0 at the top.
#[derive(Debug)]
pub struct Texture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<f32>,
    /// Values as stored (EXR), else normalised 0 to 1 (PNG).
    pub hdr: bool,
}

impl Texture {
    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

/// Reads a PNG (8 or 16 bits, grey, grey-alpha, RGB, RGBA or palette) or
/// an OpenEXR file.
pub fn read_texture(path: &Path) -> Result<Texture, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("exr") => {
            use exr::prelude::{ReadChannels as _, ReadLayers as _};
            // On this worker thread: no thread pool of exr's own.
            let img = exr::prelude::read()
                .no_deep_data()
                .largest_resolution_level()
                .rgba_channels(
                    |size: exr::math::Vec2<usize>, _: &exr::prelude::RgbaChannels| {
                        let (w, h) = (size.width(), size.height());
                        (w, h, vec![0.0f32; (w * h * 4).min(MAX_PIXELS * 4)])
                    },
                    |px: &mut (usize, usize, Vec<f32>),
                     pos: exr::math::Vec2<usize>,
                     (r, g, b, a): (f32, f32, f32, f32)| {
                        let i = (pos.y() * px.0 + pos.x()) * 4;
                        if i + 3 < px.2.len() {
                            px.2[i..i + 4].copy_from_slice(&[r, g, b, a]);
                        }
                    },
                )
                .first_valid_layer()
                .all_attributes()
                .non_parallel()
                .from_file(path)
                .map_err(|e| err(&e))?;
            let (w, h, rgba) = img.layer_data.channel_data.pixels;
            if w * h > MAX_PIXELS || w == 0 || h == 0 {
                return Err(format!(
                    "{}: {w} × {h} is not a usable texture size",
                    path.display()
                ));
            }
            Ok(Texture {
                width: w,
                height: h,
                rgba,
                hdr: true,
            })
        }
        Some("png") => {
            let file = std::fs::File::open(path).map_err(|e| err(&e))?;
            let mut dec = png::Decoder::new(std::io::BufReader::new(file));
            dec.set_transformations(png::Transformations::EXPAND);
            let mut reader = dec.read_info().map_err(|e| err(&e))?;
            let (w, h) = (reader.info().width as usize, reader.info().height as usize);
            if w * h > MAX_PIXELS || w == 0 || h == 0 {
                return Err(format!(
                    "{}: {w} × {h} is not a usable texture size",
                    path.display()
                ));
            }
            let mut buf = vec![0u8; reader.output_buffer_size().ok_or("PNG too large")?];
            let info = reader.next_frame(&mut buf).map_err(|e| err(&e))?;
            let chans = info.color_type.samples();
            let wide = info.bit_depth == png::BitDepth::Sixteen;
            let sample = |i: usize| -> f32 {
                if wide {
                    f32::from(u16::from_be_bytes([buf[2 * i], buf[2 * i + 1]])) / 65535.0
                } else {
                    f32::from(buf[i]) / 255.0
                }
            };
            let mut rgba = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    let base = (y * info.line_size / if wide { 2 } else { 1 }) + x * chans;
                    let v: [f32; 4] = match chans {
                        1 => {
                            let g = sample(base);
                            [g, g, g, 1.0]
                        }
                        2 => {
                            let g = sample(base);
                            [g, g, g, sample(base + 1)]
                        }
                        3 => [sample(base), sample(base + 1), sample(base + 2), 1.0],
                        _ => [
                            sample(base),
                            sample(base + 1),
                            sample(base + 2),
                            sample(base + 3),
                        ],
                    };
                    rgba.extend_from_slice(&v);
                }
            }
            Ok(Texture {
                width: w,
                height: h,
                rgba,
                hdr: false,
            })
        }
        _ => Err(format!("{}: use a .exr or .png texture", path.display())),
    }
}

/// What a data file says (missing values are `None`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    pub frames: Option<usize>,
    pub min: Option<[f64; 3]>,
    pub max: Option<[f64; 3]>,
    pub fps: Option<f64>,
}

/// Reads the values VAT In uses from a data file: an object, or an array
/// whose first object holds them. Keys are matched ignoring case, spaces
/// and underscores (`_frameCount`, `Frame Count`, `_boundingMaxX`, ...).
pub fn read_data(text: &str) -> Result<Data, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("the data file: {e}"))?;
    let obj = match &v {
        Value::Array(a) => a.iter().find_map(Value::as_object),
        Value::Object(o) => Some(o),
        _ => None,
    }
    .ok_or("the data file holds no object")?;
    let keys: HashMap<String, &Value> = obj
        .iter()
        .map(|(k, v)| {
            (
                k.chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase(),
                v,
            )
        })
        .collect();
    let num = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| keys.get(*n).and_then(|v| v.as_f64()))
    };
    let vec3 = |stems: &[&str]| -> Option<[f64; 3]> {
        stems.iter().find_map(|s| {
            if let Some(a) = keys.get(*s).and_then(|v| v.as_array())
                && a.len() >= 3
            {
                return Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?]);
            }
            let c = |k: &str| keys.get(&format!("{s}{k}")).and_then(|v| v.as_f64());
            Some([c("x")?, c("y")?, c("z")?])
        })
    };
    Ok(Data {
        frames: num(&["framecount", "frames", "numofframes", "numframes"])
            .filter(|f| *f >= 1.0)
            .map(|f| f as usize),
        min: vec3(&["boundingmin", "boundmin", "posmin", "min"]),
        max: vec3(&["boundingmax", "boundmax", "posmax", "max"]),
        fps: num(&["houdinifps", "fps"]),
    })
}

// --------------------------------------------------------------- cache --

/// Decoded files, kept while unchanged (path, modified time, length).
struct Cache<T> {
    items: Vec<(PathBuf, Option<SystemTime>, u64, Arc<T>)>,
}

const CACHED: usize = 12;

fn cached<T>(
    cache: &'static OnceLock<Mutex<Cache<T>>>,
    path: &Path,
    load: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<Arc<T>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (time, len) = (meta.modified().ok(), meta.len());
    let cell = cache.get_or_init(|| Mutex::new(Cache { items: Vec::new() }));
    {
        let c = cell.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = c
            .items
            .iter()
            .find(|i| i.0 == path && i.1 == time && i.2 == len)
        {
            return Ok(hit.3.clone());
        }
    }
    let value = Arc::new(load(path)?);
    let mut c = cell.lock().unwrap_or_else(|e| e.into_inner());
    c.items.retain(|i| i.0 != path);
    if c.items.len() >= CACHED {
        c.items.remove(0);
    }
    c.items.push((path.to_owned(), time, len, value.clone()));
    Ok(value)
}

fn texture(path: &Path) -> Result<Arc<Texture>, String> {
    static C: OnceLock<Mutex<Cache<Texture>>> = OnceLock::new();
    cached(&C, path, read_texture)
}

fn mesh(path: &Path) -> Result<Arc<Mesh>, String> {
    static C: OnceLock<Mutex<Cache<Mesh>>> = OnceLock::new();
    cached(&C, path, |p| io::read(p, ObjPoints::Position))
}

fn optional(s: &Snap, name: &str) -> Result<Option<Arc<Texture>>, String> {
    match s.str(name) {
        "" => Ok(None),
        p => texture(Path::new(p)).map(Some),
    }
}

// ---------------------------------------------------------------- play --

/// Piece of each point: `piece` when the mesh has it, else connected
/// components in primitive order.
fn pieces(m: &Mesh) -> Vec<u32> {
    if let Some(c) = m.point_attr("piece").filter(|c| c.comps == 1) {
        match &c.values {
            Values::U32(v) => return v.clone(),
            Values::I32(v) => return v.iter().map(|&x| x.max(0) as u32).collect(),
            Values::F32(v) => return v.iter().map(|&x| x.max(0.0).round() as u32).collect(),
        }
    }
    let mut parent: Vec<u32> = (0..m.points() as u32).collect();
    fn find(p: &mut [u32], mut i: u32) -> u32 {
        while p[i as usize] != i {
            p[i as usize] = p[p[i as usize] as usize];
            i = p[i as usize];
        }
        i
    }
    for f in 0..m.prims() {
        let c = m.prim(f);
        for w in c.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if a != b {
                parent[a.max(b) as usize] = a.min(b);
            }
        }
    }
    let mut id: HashMap<u32, u32> = HashMap::new();
    let mut out = Vec::with_capacity(m.points());
    // Numbered by first appearance in primitive order, then loose points.
    let mut order: Vec<u32> = m.corners.clone();
    order.extend(0..m.points() as u32);
    for &i in &order {
        let r = find(&mut parent, i);
        let n = id.len() as u32;
        id.entry(r).or_insert(n);
    }
    for i in 0..m.points() as u32 {
        out.push(id[&find(&mut parent, i)]);
    }
    out
}

/// Texel (column, row in frame 0) of each point.
fn texels(m: &Mesh, mode: &str, w: usize, h: usize) -> Vec<(usize, usize)> {
    let uv2 = m
        .point_attr("uv2")
        .filter(|c| c.comps >= 2)
        .and_then(|c| c.floats().map(|v| (v, usize::from(c.comps))));
    if let Some((v, comps)) = uv2 {
        return (0..m.points())
            .map(|i| {
                let (u, t) = (f64::from(v[i * comps]), f64::from(v[i * comps + 1]));
                let x = (u * w as f64).floor().clamp(0.0, (w - 1) as f64) as usize;
                let y = ((1.0 - t) * h as f64).floor().clamp(0.0, (h - 1) as f64) as usize;
                (x, y)
            })
            .collect();
    }
    let index: Vec<usize> = match mode {
        "rigid" | "sprite" => pieces(m).into_iter().map(|p| p as usize).collect(),
        _ => (0..m.points()).collect(),
    };
    index.into_iter().map(|i| (i % w, i / w)).collect()
}

/// Rotates `v` by unit quaternion `q` (x, y, z, w).
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let t = v3::scale(v3::cross(u, v), 2.0);
    v3::add(v3::add(v, v3::scale(t, q[3])), v3::cross(u, t))
}

pub fn run(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let path = s.str("mesh");
    if path.is_empty() {
        return Ok(Mesh::new());
    }
    let rest = mesh(Path::new(path))?;
    let Some(pos) = optional(s, "position")? else {
        return Ok((*rest).clone());
    };
    job.check()?;
    let data = match s.str("data") {
        "" => Data::default(),
        p => read_data(&std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?)?,
    };
    let mode = s.str("mode");
    let lookup = if mode == "fluid" {
        optional(s, "lookup")?
    } else {
        None
    };
    let rot = if mode == "rigid" {
        optional(s, "rotation")?
    } else {
        None
    };
    let col = optional(s, "colour")?;
    // The texture the points address: the lookup in fluid mode with one.
    let (aw, ah) = lookup
        .as_ref()
        .map_or((pos.width, pos.height), |l| (l.width, l.height));
    let tex = texels(&rest, mode, aw, ah);
    let rows = tex.iter().map(|t| t.1).max().map_or(1, |r| r + 1);
    let frames = match s.i64("frames") {
        n if n > 0 => n as usize,
        _ => data.frames.unwrap_or(ah / rows).max(1),
    };
    if (frames - 1) * rows + rows > ah {
        return Err(format!(
            "{frames} frames of {rows} rows need {} texture rows; the texture has {ah}",
            frames * rows
        ));
    }
    // Decoding of stored positions.
    let bounds = match (data.min, data.max) {
        (Some(lo), Some(hi)) => Some((lo, hi)),
        _ => None,
    };
    if !pos.hdr && bounds.is_none() {
        return Err("a .png position texture needs the bounds from the data file".into());
    }
    let decode = |c: [f32; 4]| -> [f64; 3] {
        let v = [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])];
        match (pos.hdr, bounds) {
            (false, Some((lo, hi))) => [0, 1, 2].map(|k| lo[k] + v[k] * (hi[k] - lo[k])),
            _ => v,
        }
    };
    // Frames to blend.
    let t = s.f64("frame");
    let last = (frames - 1) as f64;
    let t = if s.bool("loop") {
        t.rem_euclid(frames as f64)
    } else {
        t.clamp(0.0, last)
    };
    let f0 = t.floor() as usize % frames;
    let f1 = if s.bool("loop") {
        (f0 + 1) % frames
    } else {
        (f0 + 1).min(frames - 1)
    };
    let wgt = if s.bool("interpolate") && mode != "fluid" {
        t - t.floor()
    } else {
        0.0
    };
    let row = |r0: usize, f: usize| r0 + f * rows;
    let mut out = (*rest).clone();
    let n = out.points();
    // The position texel of each point at frame f (through the lookup).
    let pos_texel = |i: usize, f: usize| -> Option<(usize, usize)> {
        let (x, r0) = tex[i];
        match &lookup {
            None => Some((x, row(r0, f))),
            Some(l) => {
                let c = l.at(x, row(r0, f));
                if c[3] < 0.5 {
                    return None;
                }
                let px = (f64::from(c[0]) * pos.width as f64).floor();
                let py = ((1.0 - f64::from(c[1])) * pos.height as f64).floor();
                Some((
                    px.clamp(0.0, (pos.width - 1) as f64) as usize,
                    py.clamp(0.0, (pos.height - 1) as f64) as usize,
                ))
            }
        }
    };
    let sample = |i: usize| -> Option<[f64; 3]> {
        let (a, b) = (pos_texel(i, f0)?, pos_texel(i, f1)?);
        let pa = decode(pos.at(a.0, a.1));
        let pb = decode(pos.at(b.0, b.1));
        Some(v3::lerp(pa, pb, wgt))
    };
    let mut used = vec![true; n];
    match mode {
        "rigid" | "sprite" => {
            // Rest pivots: `pivot`, else each piece's centre.
            let key: Vec<(usize, usize)> = tex.clone();
            let pivot: Vec<[f64; 3]> = match out.vec3("pivot") {
                Some(p) if mode == "rigid" => p.iter().map(|&q| v3::of(q)).collect(),
                _ => {
                    let mut acc: HashMap<(usize, usize), ([f64; 3], f64)> = HashMap::new();
                    for i in 0..n {
                        let e = acc.entry(key[i]).or_insert(([0.0; 3], 0.0));
                        e.0 = v3::add(e.0, v3::of(rest.p[i]));
                        e.1 += 1.0;
                    }
                    (0..n)
                        .map(|i| {
                            let (s, c) = acc[&key[i]];
                            v3::scale(s, 1.0 / c)
                        })
                        .collect()
                }
            };
            let quat = |i: usize| -> Option<[f64; 4]> {
                let r = rot.as_ref()?;
                let (x, r0) = tex[i];
                let q = |f: usize| {
                    let c = r.at(x, row(r0, f));
                    let mut q = c.map(f64::from);
                    if !r.hdr {
                        q = q.map(|v| v * 2.0 - 1.0);
                    }
                    q
                };
                let (a, mut b) = (q(f0), q(f1));
                if a.iter().zip(&b).map(|(x, y)| x * y).sum::<f64>() < 0.0 {
                    b = b.map(|v| -v);
                }
                let m: [f64; 4] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * wgt);
                let l = m.iter().map(|v| v * v).sum::<f64>().sqrt();
                (l > 1e-12).then(|| m.map(|v| v / l))
            };
            let normals = out.vec3(names::N);
            let mut nn = normals.clone();
            for i in 0..n {
                job.every(i)?;
                let Some(c) = sample(i) else { continue };
                let local = v3::sub(v3::of(rest.p[i]), pivot[i]);
                let (p, q) = match quat(i) {
                    Some(q) => (rotate(q, local), Some(q)),
                    None => (local, None),
                };
                out.p[i] = v3::f32(v3::add(c, p));
                if let (Some(q), Some(src), Some(dst)) = (q, &normals, &mut nn) {
                    dst[i] = v3::f32(rotate(q, v3::of(src[i])));
                }
            }
            if let Some(nn) = nn {
                out.set_point_attr(Column::vec3(names::N, &nn));
            }
        }
        _ => {
            let offset = s.str("positions") == "offset";
            for i in 0..n {
                job.every(i)?;
                match sample(i) {
                    Some(p) => {
                        out.p[i] = v3::f32(if offset {
                            v3::add(v3::of(rest.p[i]), p)
                        } else {
                            p
                        });
                    }
                    None => used[i] = false,
                }
            }
        }
    }
    if let Some(c) = &col {
        let mut cd = Vec::with_capacity(n * 4);
        for i in 0..n {
            let (a, b) = match (pos_texel(i, f0), pos_texel(i, f1)) {
                (Some(a), Some(b)) if lookup.is_some() => (a, b),
                _ => {
                    let (x, r0) = tex[i];
                    ((x, row(r0, f0)), (x, row(r0, f1)))
                }
            };
            let (ca, cb) = (c.at(a.0, a.1), c.at(b.0, b.1));
            let v: [f32; 4] = std::array::from_fn(|k| ca[k] + (cb[k] - ca[k]) * wgt as f32);
            cd.extend_from_slice(&v);
        }
        out.set_point_attr(Column::f32(names::CD, 4, cd));
    }
    if mode == "fluid" {
        // Unused points and collapsed triangles leave the frame.
        let area_eps = {
            let (lo, hi) = out.bounds().unwrap_or(([0.0; 3], [1.0; 3]));
            let d = v3::len(v3::sub(v3::of(hi), v3::of(lo)));
            1e-12 * d * d
        };
        let keep: Vec<bool> = (0..out.prims())
            .map(|f| {
                let c = out.prim(f);
                c.iter().all(|&i| used[i as usize])
                    && (!out.closed[f] || v3::len(out.prim_normal(f)) > area_eps)
            })
            .collect();
        crate::edit::filter_prims(&mut out, &keep);
        out.remove_unused_points();
    }
    if matches!(mode, "soft" | "fluid") && out.prims() > 0 {
        let nn = point_normals(&out);
        out.set_point_attr(Column::vec3(names::N, &nn));
    }
    Ok(out)
}
