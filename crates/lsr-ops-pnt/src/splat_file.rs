//! Gaussian-splat files for Splat In (FR-C1): the standard 3D Gaussian
//! Splatting PLY layout, Niantic's SPZ and PlayCanvas's SOG, read into one
//! [`SplatCloud`] and turned into a GEO value.
//!
//! Written from the published method and the formats' specifications only:
//! Kerbl et al., "3D Gaussian Splatting for Real-Time Radiance Field
//! Rendering" (SIGGRAPH 2023) for the PLY property layout and its
//! activations; the SPZ format as documented by github.com/nianticlabs/spz
//! (MIT); SOG v2 as documented at developer.playcanvas.com
//! ("SOG format", MIT-licensed splat-transform). No code is taken from the
//! INRIA reference implementation (non-commercial licence).
//!
//! **Conventions** of a [`SplatCloud`] (and of the GEO value):
//! - `scales` are linear standard deviations (the PLY stores their log);
//! - `rotations` are unit quaternions `(x, y, z, w)` (the PLY and SOG store
//!   `w` first);
//! - `colours` RGB is the base colour, `0.5 + SH_C0 · dc` (the band-0
//!   coefficient), alpha the opacity after the sigmoid;
//! - `sh` holds the higher bands per splat, coefficient-major with RGB
//!   inner (`sh[i * n + k]` is coefficient `k + 1` of splat `i`), up to
//!   degree 3 (a degree-4 SPZ keeps its first three bands).
//!
//! **Coordinates:** PLY and SOG files carry the trainer's camera frame
//! (COLMAP: Y down, Z forward); SPZ stores Y up, Z back (`RUB`), like
//! Lasair. [`SplatCloud::flip_y_down`] turns the former into the latter
//! (a half turn about X, applied to positions, rotations and the odd
//! spherical-harmonic coefficients).

use std::io::{Cursor, Read, Seek};
use std::path::Path;

use lsr_core::geo::names;
use lsr_gpu::geo::{Attribute, BufferPool, DetailValue, Geometry};

/// The band-0 spherical-harmonic basis constant, `1 / (2 √π)`.
pub const SH_C0: f32 = 0.282_094_8;

/// The most splats one file may hold (the renderer's per-dispatch limit).
pub const MAX_SPLATS: usize = 65_535 * 256;

/// Linear scale given to splats of a file without scales (a plain point
/// cloud).
pub const DEFAULT_SCALE: f32 = 0.01;

/// Spherical-harmonic coefficients beyond band 0 for `degree` (0–3).
pub const fn coefs(degree: u8) -> usize {
    match degree {
        0 => 0,
        1 => 3,
        2 => 8,
        _ => 15,
    }
}

/// The degree whose higher bands have `n` coefficients per channel, if any.
fn degree_of(n: usize) -> Option<u8> {
    match n {
        0 => Some(0),
        3 => Some(1),
        8 => Some(2),
        15 => Some(3),
        _ => None,
    }
}

/// Decoded Gaussian splats (conventions in the module docs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplatCloud {
    pub positions: Vec<[f32; 3]>,
    pub scales: Vec<[f32; 3]>,
    pub rotations: Vec<[f32; 4]>,
    pub colours: Vec<[f32; 4]>,
    pub sh_degree: u8,
    pub sh: Vec<[f32; 3]>,
}

/// File formats Splat In reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ply,
    Spz,
    Sog,
}

impl Format {
    /// The format of `path` by its extension (`meta.json` is a SOG folder).
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name == "meta.json" {
            return Some(Format::Sog);
        }
        match path
            .extension()?
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str()
        {
            "ply" => Some(Format::Ply),
            "spz" => Some(Format::Spz),
            "sog" => Some(Format::Sog),
            _ => None,
        }
    }
}

impl SplatCloud {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Checks the arrays agree and every value is finite.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.len();
        if n > MAX_SPLATS {
            return Err(format!("{n} splats; at most {MAX_SPLATS} are supported"));
        }
        if self.sh_degree > 3 {
            return Err(format!("SH degree {} (at most 3)", self.sh_degree));
        }
        if self.scales.len() != n
            || self.rotations.len() != n
            || self.colours.len() != n
            || self.sh.len() != n * coefs(self.sh_degree)
        {
            return Err("the splat arrays have different lengths".into());
        }
        let finite = self
            .positions
            .iter()
            .chain(&self.scales)
            .chain(&self.sh)
            .flatten()
            .chain(self.rotations.iter().chain(&self.colours).flatten())
            .all(|v| v.is_finite());
        if !finite {
            return Err("the file has non-finite values".into());
        }
        Ok(())
    }

    /// A half turn about X: (x, y, z) → (x, −y, −z), from the Y-down
    /// training frame of PLY and SOG files to Lasair's Y up.
    pub fn flip_y_down(&mut self) {
        for p in &mut self.positions {
            p[1] = -p[1];
            p[2] = -p[2];
        }
        // The new rotation is the half turn times the old: q' = i ⊗ q.
        for q in &mut self.rotations {
            let [x, y, z, w] = *q;
            *q = [w, -z, y, -x];
        }
        // Basis functions odd in y + z change sign (3DGS order, k = 1…15).
        const FLIP: [bool; 15] = [
            true, true, false, true, false, false, true, false, true, false, true, true, false,
            true, false,
        ];
        let n = coefs(self.sh_degree);
        if n > 0 {
            for splat in self.sh.chunks_exact_mut(n) {
                for (c, flip) in splat.iter_mut().zip(FLIP) {
                    if flip {
                        *c = c.map(|v| -v);
                    }
                }
            }
        }
    }

    /// The GEO value: point attributes `P`, `Cd`, `scale`, `orient` and the
    /// [`names::SH`] attributes the degree needs, detail
    /// [`names::SH_DEGREE`]. With `upload`, the values are also put on the
    /// GPU now (call it on a worker thread, so the render thread does not).
    pub fn to_geometry(&self, upload: Option<(&wgpu::Queue, &BufferPool)>) -> Geometry {
        let n = self.len() as u32;
        let mut g = Geometry::new();
        let header = match upload {
            Some((queue, pool)) if n > 0 => Some(lsr_gpu::geo::cpu_header(queue, pool, n)),
            _ => None,
        };
        g.set_points(n, header);
        if n == 0 {
            return g;
        }
        g.set_point(Attribute::from_vec3(names::P, &self.positions));
        g.set_point(Attribute::from_vec4(names::CD, &self.colours));
        g.set_point(Attribute::from_vec3(names::SCALE, &self.scales));
        g.set_point(Attribute::from_vec4(names::ORIENT, &self.rotations));
        let per = coefs(self.sh_degree);
        let floats = per * 3;
        let attrs = floats.div_ceil(4);
        if attrs > 0 {
            let mut columns = vec![vec![[0.0f32; 4]; self.len()]; attrs];
            for (i, splat) in self.sh.chunks_exact(per).enumerate() {
                for (f, v) in splat.iter().flatten().enumerate() {
                    columns[f / 4][i][f % 4] = *v;
                }
            }
            for (name, column) in names::SH.iter().zip(&columns) {
                g.set_point(Attribute::from_vec4(*name, column));
            }
        }
        g.set_detail(
            names::SH_DEGREE,
            DetailValue::I32(i64::from(self.sh_degree)),
        );
        if let Some((queue, pool)) = upload {
            for a in g.attrs(lsr_core::geo::AttrClass::Point) {
                a.ensure_gpu(queue, pool);
            }
        }
        g
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn logit(y: f32) -> f32 {
    let y = y.clamp(1e-6, 1.0 - 1e-6);
    (y / (1.0 - y)).ln()
}

/// A unit quaternion, or the identity for a degenerate one.
fn normalised(q: [f32; 4]) -> [f32; 4] {
    let l = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if l > 1e-12 && l.is_finite() {
        q.map(|v| v / l)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Reads a splat file (format by extension; a SOG may be the `.sog` archive
/// or the folder's `meta.json`).
pub fn read(path: &Path) -> Result<SplatCloud, String> {
    let format = Format::from_path(path).ok_or_else(|| {
        format!(
            "{}: use a .ply, .spz or .sog file (or a SOG folder's meta.json)",
            path.display()
        )
    })?;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let cloud = match format {
        Format::Ply => read_ply(&std::fs::read(path).map_err(err)?),
        Format::Spz => read_spz(&std::fs::read(path).map_err(err)?),
        Format::Sog
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json")) =>
        {
            let dir = path.parent().unwrap_or(Path::new("")).to_owned();
            let meta = std::fs::read(path).map_err(err)?;
            read_sog(&meta, &mut |name| {
                let file = dir.join(name);
                if Path::new(name).components().count() != 1 {
                    return Err(format!("{name}: SOG files must sit beside meta.json"));
                }
                std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))
            })
        }
        Format::Sog => read_sog_archive(&std::fs::read(path).map_err(err)?),
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    cloud.validate()?;
    Ok(cloud)
}

// ------------------------------------------------------------------ PLY --

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "char" | "int8" => Scalar::I8,
            "uchar" | "uint8" => Scalar::U8,
            "short" | "int16" => Scalar::I16,
            "ushort" | "uint16" => Scalar::U16,
            "int" | "int32" => Scalar::I32,
            "uint" | "uint32" => Scalar::U32,
            "float" | "float32" => Scalar::F32,
            "double" | "float64" => Scalar::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::F64 => 8,
        }
    }

    fn read(self, b: &[u8], big: bool) -> f64 {
        macro_rules! get {
            ($t:ty, $n:expr) => {{
                let a: [u8; $n] = b[..$n].try_into().expect("sized by the row");
                if big {
                    <$t>::from_be_bytes(a) as f64
                } else {
                    <$t>::from_le_bytes(a) as f64
                }
            }};
        }
        match self {
            Scalar::I8 => f64::from(b[0] as i8),
            Scalar::U8 => f64::from(b[0]),
            Scalar::I16 => get!(i16, 2),
            Scalar::U16 => get!(u16, 2),
            Scalar::I32 => get!(i32, 4),
            Scalar::U32 => get!(u32, 4),
            Scalar::F32 => get!(f32, 4),
            Scalar::F64 => get!(f64, 8),
        }
    }

    fn integer(self) -> bool {
        !matches!(self, Scalar::F32 | Scalar::F64)
    }
}

#[derive(Debug)]
struct Property {
    name: String,
    ty: Scalar,
    /// A list property: the type of its length.
    list: Option<Scalar>,
}

#[derive(Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

/// The header's elements, encoding and the byte offset of the body.
fn ply_header(bytes: &[u8]) -> Result<(Vec<Element>, Encoding, usize), String> {
    const END: &[u8] = b"end_header";
    let at = bytes
        .windows(END.len())
        .take(1 << 20)
        .position(|w| w == END)
        .ok_or("not a PLY file (no end_header)")?;
    let mut body = at + END.len();
    if bytes.get(body) == Some(&b'\r') {
        body += 1;
    }
    if bytes.get(body) != Some(&b'\n') {
        return Err("a malformed PLY header".into());
    }
    body += 1;
    let text = std::str::from_utf8(&bytes[..at]).map_err(|_| "a PLY header that is not text")?;
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file".into());
    }
    let mut encoding = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["format", f, _] => {
                encoding = Some(match *f {
                    "ascii" => Encoding::Ascii,
                    "binary_little_endian" => Encoding::Little,
                    "binary_big_endian" => Encoding::Big,
                    _ => return Err(format!("unknown PLY format {f}")),
                })
            }
            ["element", name, count] => elements.push(Element {
                name: (*name).to_owned(),
                count: count
                    .parse()
                    .map_err(|_| format!("a bad element count {count}"))?,
                props: Vec::new(),
            }),
            ["property", "list", len, item, name] => {
                let e = elements.last_mut().ok_or("a property before any element")?;
                e.props.push(Property {
                    name: (*name).to_owned(),
                    ty: Scalar::parse(item).ok_or_else(|| format!("unknown type {item}"))?,
                    list: Some(Scalar::parse(len).ok_or_else(|| format!("unknown type {len}"))?),
                });
            }
            ["property", ty, name] => {
                let e = elements.last_mut().ok_or("a property before any element")?;
                e.props.push(Property {
                    name: (*name).to_owned(),
                    ty: Scalar::parse(ty).ok_or_else(|| format!("unknown type {ty}"))?,
                    list: None,
                });
            }
            [] | ["comment", ..] | ["obj_info", ..] => {}
            _ => return Err(format!("an unknown PLY header line: {line}")),
        }
    }
    Ok((
        elements,
        encoding.ok_or("a PLY header without a format")?,
        body,
    ))
}

/// Indices of the vertex properties a splat uses.
struct Columns {
    pos: [usize; 3],
    dc: Option<[usize; 3]>,
    rgb: Option<[usize; 3]>,
    opacity: Option<usize>,
    scale: Option<[usize; 3]>,
    rot: Option<[usize; 4]>,
    rest: Vec<usize>,
}

impl Columns {
    fn find(props: &[Property]) -> Result<Self, String> {
        let idx = |name: &str| props.iter().position(|p| p.name == name);
        let all = |names: &[&str]| -> Option<Vec<usize>> { names.iter().map(|n| idx(n)).collect() };
        let three = |names: [&str; 3]| all(&names).map(|v| [v[0], v[1], v[2]]);
        let pos = three(["x", "y", "z"]).ok_or("the vertices have no x, y, z")?;
        let mut rest = Vec::new();
        while let Some(i) = idx(&format!("f_rest_{}", rest.len())) {
            rest.push(i);
        }
        Ok(Self {
            pos,
            dc: three(["f_dc_0", "f_dc_1", "f_dc_2"]),
            rgb: three(["red", "green", "blue"]),
            opacity: idx("opacity"),
            scale: three(["scale_0", "scale_1", "scale_2"]),
            rot: all(&["rot_0", "rot_1", "rot_2", "rot_3"]).map(|v| [v[0], v[1], v[2], v[3]]),
            rest,
        })
    }
}

/// Reads a PLY file in the 3DGS layout (`x y z`, `f_dc_*`, `f_rest_*`,
/// `opacity`, `scale_*`, `rot_*`; ASCII or binary of either byte order,
/// any scalar types). A plain coloured point cloud (`red green blue`) is
/// read as opaque round splats of [`DEFAULT_SCALE`].
pub fn read_ply(bytes: &[u8]) -> Result<SplatCloud, String> {
    let (elements, encoding, body) = ply_header(bytes)?;
    if elements.iter().any(|e| e.name == "chunk") {
        return Err("a compressed PLY (with a `chunk` element) is not supported: convert it to SOG, SPZ or a standard 3DGS PLY".into());
    }
    let vi = elements
        .iter()
        .position(|e| e.name == "vertex")
        .ok_or("the PLY has no vertex element")?;
    let vertex = &elements[vi];
    if vertex.count > MAX_SPLATS {
        return Err(format!(
            "{} splats; at most {MAX_SPLATS} are supported",
            vertex.count
        ));
    }
    if vertex.props.iter().any(|p| p.list.is_some()) {
        return Err("the vertices have list properties".into());
    }
    let cols = Columns::find(&vertex.props)?;
    if cols.rest.len() % 3 != 0 {
        return Err(format!(
            "{} f_rest properties (not a multiple of 3)",
            cols.rest.len()
        ));
    }
    let per_channel = cols.rest.len() / 3;
    // Degree 4 keeps its first three bands.
    let (degree, keep) = match per_channel {
        24 => (3, 15),
        n => (
            degree_of(n).ok_or_else(|| format!("{n} SH coefficients per channel"))?,
            n,
        ),
    };
    let n = vertex.count;
    let mut cloud = SplatCloud {
        positions: Vec::with_capacity(n),
        scales: Vec::with_capacity(n),
        rotations: Vec::with_capacity(n),
        colours: Vec::with_capacity(n),
        sh_degree: degree,
        sh: Vec::with_capacity(n * keep),
    };
    let rgb_scale = cols.rgb.map(|c| {
        if vertex.props[c[0]].ty.integer() {
            255.0
        } else {
            1.0
        }
    });
    let mut row = vec![0.0f64; vertex.props.len()];
    let mut push = |row: &[f64]| {
        let f = |i: usize| row[i] as f32;
        cloud.positions.push(cols.pos.map(f));
        let colour = match (cols.dc, cols.rgb) {
            (Some(dc), _) => dc.map(|i| 0.5 + SH_C0 * f(i)),
            (None, Some(rgb)) => rgb.map(|i| f(i) / rgb_scale.unwrap_or(1.0)),
            (None, None) => [1.0; 3],
        };
        let alpha = cols.opacity.map_or(1.0, |i| sigmoid(f(i)));
        cloud.colours.push([colour[0], colour[1], colour[2], alpha]);
        cloud.scales.push(
            cols.scale
                .map_or([DEFAULT_SCALE; 3], |s| s.map(|i| f(i).exp())),
        );
        cloud.rotations.push(match cols.rot {
            Some([w, x, y, z]) => normalised([f(x), f(y), f(z), f(w)]),
            None => [0.0, 0.0, 0.0, 1.0],
        });
        for k in 0..keep {
            cloud
                .sh
                .push([0, 1, 2].map(|c| f(cols.rest[c * per_channel + k])));
        }
    };
    match encoding {
        Encoding::Ascii => {
            let text = std::str::from_utf8(&bytes[body..])
                .map_err(|_| "an ASCII PLY body that is not text")?;
            let mut words = text.split_ascii_whitespace();
            let mut next = || -> Result<f64, String> {
                let w = words.next().ok_or("the PLY ends early")?;
                w.parse().map_err(|_| format!("a bad PLY value {w}"))
            };
            for e in &elements[..vi] {
                for _ in 0..e.count {
                    for p in &e.props {
                        let len = if p.list.is_some() {
                            next()? as usize
                        } else {
                            1
                        };
                        for _ in 0..len {
                            next()?;
                        }
                    }
                }
            }
            for _ in 0..n {
                for v in &mut row {
                    *v = next()?;
                }
                push(&row);
            }
        }
        Encoding::Little | Encoding::Big => {
            let big = encoding == Encoding::Big;
            let mut at = body;
            for e in &elements[..vi] {
                if e.props.iter().any(|p| p.list.is_some()) {
                    return Err(format!(
                        "element `{}` before the vertices has list properties",
                        e.name
                    ));
                }
                let size: usize = e.props.iter().map(|p| p.ty.size()).sum();
                at = size
                    .checked_mul(e.count)
                    .and_then(|s| s.checked_add(at))
                    .ok_or("the PLY is too large")?;
            }
            let stride: usize = vertex.props.iter().map(|p| p.ty.size()).sum();
            let need = stride.checked_mul(n).ok_or("the PLY is too large")?;
            let data = bytes
                .get(at..)
                .filter(|d| d.len() >= need)
                .ok_or("the PLY ends early")?;
            for chunk in data[..need].chunks_exact(stride) {
                let mut o = 0;
                for (v, p) in row.iter_mut().zip(&vertex.props) {
                    *v = p.ty.read(&chunk[o..], big);
                    o += p.ty.size();
                }
                push(&row);
            }
        }
    }
    Ok(cloud)
}

/// Writes `cloud` as a binary little-endian PLY in the 3DGS layout.
pub fn write_ply(cloud: &SplatCloud) -> Vec<u8> {
    let per = coefs(cloud.sh_degree);
    let mut props: Vec<String> = [
        "x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2",
    ]
    .map(String::from)
    .to_vec();
    props.extend((0..per * 3).map(|i| format!("f_rest_{i}")));
    props.extend(
        [
            "opacity", "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
        ]
        .map(String::from),
    );
    let mut out = format!(
        "ply\nformat binary_little_endian 1.0\ncomment written by Lasair\nelement vertex {}\n",
        cloud.len()
    );
    for p in &props {
        out.push_str(&format!("property float {p}\n"));
    }
    out.push_str("end_header\n");
    let mut out = out.into_bytes();
    out.reserve(cloud.len() * props.len() * 4);
    let mut put = |v: f32| out.extend_from_slice(&v.to_le_bytes());
    for i in 0..cloud.len() {
        cloud.positions[i].into_iter().for_each(&mut put);
        [0.0; 3].into_iter().for_each(&mut put);
        let c = cloud.colours[i];
        c[..3].iter().for_each(|v| put((v - 0.5) / SH_C0));
        let sh = &cloud.sh[i * per..(i + 1) * per];
        for ch in 0..3 {
            for coef in sh {
                put(coef[ch]);
            }
        }
        put(logit(c[3]));
        cloud.scales[i].iter().for_each(|s| put(s.max(1e-30).ln()));
        let [x, y, z, w] = cloud.rotations[i];
        [w, x, y, z].into_iter().for_each(&mut put);
    }
    out
}

// ------------------------------------------------------------------ SPZ --

const SPZ_MAGIC: u32 = 0x5053_474e;
/// SPZ base-colour scale (`colorScale`): bytes store `dc · 0.15 + 0.5`.
const SPZ_COLOUR_SCALE: f32 = 0.15;

/// SPZ coefficients per channel beyond band 0 (degree up to 4).
fn spz_dim(degree: u8) -> Option<usize> {
    [0, 3, 8, 15, 24].get(usize::from(degree)).copied()
}

/// The decoded SPZ header.
struct SpzHeader {
    version: u32,
    n: usize,
    degree: u8,
    fractional_bits: u8,
}

impl SpzHeader {
    /// Byte sizes of the six sections in file order: positions, alphas,
    /// colours, scales, rotations, SH.
    fn sections(&self) -> Result<[usize; 6], String> {
        let n = self.n;
        let dim = spz_dim(self.degree).ok_or_else(|| format!("SH degree {}", self.degree))?;
        Ok([
            n * if self.version == 1 { 6 } else { 9 },
            n,
            n * 3,
            n * 3,
            n * if self.version >= 3 { 4 } else { 3 },
            n * dim * 3,
        ])
    }

    fn check(&self) -> Result<(), String> {
        if !(1..=4).contains(&self.version) {
            return Err(format!("SPZ version {} is not supported", self.version));
        }
        if self.n > MAX_SPLATS {
            return Err(format!(
                "{} splats; at most {MAX_SPLATS} are supported",
                self.n
            ));
        }
        if self.fractional_bits > 23 {
            return Err(format!("{} fractional bits", self.fractional_bits));
        }
        spz_dim(self.degree).ok_or_else(|| format!("SH degree {}", self.degree))?;
        Ok(())
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("in bounds"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("in bounds"))
}

/// Reads an SPZ file: versions 1–3 (one gzip stream) and 4 (a plain
/// header, a table of contents and one zstd stream per attribute).
pub fn read_spz(bytes: &[u8]) -> Result<SplatCloud, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut gz = flate2::read::GzDecoder::new(bytes);
        let mut head = [0u8; 16];
        gz.read_exact(&mut head)
            .map_err(|e| format!("not an SPZ file ({e})"))?;
        if u32_at(&head, 0) != SPZ_MAGIC {
            return Err("not an SPZ file (bad magic)".into());
        }
        let h = SpzHeader {
            version: u32_at(&head, 4),
            n: u32_at(&head, 8) as usize,
            degree: head[12],
            fractional_bits: head[13],
        };
        h.check()?;
        if h.version >= 4 {
            return Err("a gzip SPZ claiming version 4".into());
        }
        let mut sections = Vec::new();
        for size in h.sections()? {
            let mut s = vec![0u8; size];
            gz.read_exact(&mut s).map_err(|_| "the SPZ ends early")?;
            sections.push(s);
        }
        return spz_decode(&h, &sections);
    }
    if bytes.len() < 32 || u32_at(bytes, 0) != SPZ_MAGIC {
        return Err("not an SPZ file".into());
    }
    let h = SpzHeader {
        version: u32_at(bytes, 4),
        n: u32_at(bytes, 8) as usize,
        degree: bytes[12],
        fractional_bits: bytes[13],
    };
    h.check()?;
    if h.version < 4 {
        return Err(format!("an uncompressed SPZ of version {}", h.version));
    }
    let streams = usize::from(bytes[15]);
    let toc = u32_at(bytes, 16) as usize;
    let sizes = h.sections()?;
    let expected: Vec<usize> = sizes.iter().copied().filter(|&s| s > 0).collect();
    if streams != expected.len() {
        return Err(format!(
            "{streams} SPZ streams (expected {})",
            expected.len()
        ));
    }
    let toc_end = toc
        .checked_add(streams * 16)
        .filter(|&e| toc >= 32 && e <= bytes.len())
        .ok_or("a bad SPZ table of contents")?;
    let mut at = toc_end;
    let mut decoded = Vec::with_capacity(streams);
    for (k, &size) in expected.iter().enumerate() {
        let compressed = u64_at(bytes, toc + k * 16) as usize;
        let uncompressed = u64_at(bytes, toc + k * 16 + 8) as usize;
        if uncompressed != size {
            return Err("an SPZ stream of the wrong size".into());
        }
        let end = at
            .checked_add(compressed)
            .filter(|&e| e <= bytes.len())
            .ok_or("the SPZ ends early")?;
        let mut src = &bytes[at..end];
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut src)
            .map_err(|e| format!("an SPZ stream: {e}"))?;
        let mut s = vec![0u8; size];
        dec.read_exact(&mut s)
            .map_err(|e| format!("an SPZ stream: {e}"))?;
        decoded.push(s);
        at = end;
    }
    let mut it = decoded.into_iter();
    let sections: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            if s > 0 {
                it.next().unwrap_or_default()
            } else {
                Vec::new()
            }
        })
        .collect();
    spz_decode(&h, &sections)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => f32::NAN,
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Smallest-three quaternion (SPZ v3+): the largest component's index in
/// the top two bits, then the other three, last first, 9-bit magnitudes
/// over `1/√2` and a sign bit each.
fn spz_quat3(r: &[u8]) -> [f32; 4] {
    let mut comp = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
    let largest = (comp >> 30) as usize;
    let mut q = [0.0f32; 4];
    let mut sum = 0.0;
    for i in (0..4).rev() {
        if i != largest {
            let mag = comp & 0x1ff;
            let neg = (comp >> 9) & 1;
            comp >>= 10;
            let v = std::f32::consts::FRAC_1_SQRT_2 * mag as f32 / 511.0;
            q[i] = if neg == 1 { -v } else { v };
            sum += v * v;
        }
    }
    q[largest] = (1.0 - sum).max(0.0).sqrt();
    normalised(q)
}

fn spz_decode(h: &SpzHeader, s: &[Vec<u8>]) -> Result<SplatCloud, String> {
    let n = h.n;
    let dim = spz_dim(h.degree).expect("checked");
    let keep = dim.min(15);
    let degree = h.degree.min(3);
    let mut cloud = SplatCloud {
        sh_degree: degree,
        ..SplatCloud::default()
    };
    let scale = 1.0 / (1u32 << h.fractional_bits) as f32;
    for i in 0..n {
        let p = if h.version == 1 {
            let b = &s[0][i * 6..];
            [0, 1, 2].map(|c| half_to_f32(u16::from_le_bytes([b[2 * c], b[2 * c + 1]])))
        } else {
            let b = &s[0][i * 9..];
            [0, 1, 2].map(|c| {
                let raw = u32::from(b[3 * c])
                    | u32::from(b[3 * c + 1]) << 8
                    | u32::from(b[3 * c + 2]) << 16;
                // Sign-extend 24 bits.
                ((raw << 8) as i32 >> 8) as f32 * scale
            })
        };
        cloud.positions.push(p);
        let colour = [0, 1, 2].map(|c| {
            let dc = (f32::from(s[2][i * 3 + c]) / 255.0 - 0.5) / SPZ_COLOUR_SCALE;
            0.5 + SH_C0 * dc
        });
        cloud
            .colours
            .push([colour[0], colour[1], colour[2], f32::from(s[1][i]) / 255.0]);
        cloud
            .scales
            .push([0, 1, 2].map(|c| (f32::from(s[3][i * 3 + c]) / 16.0 - 10.0).exp()));
        cloud.rotations.push(if h.version >= 3 {
            spz_quat3(&s[4][i * 4..])
        } else {
            let b = &s[4][i * 3..];
            let xyz = [0, 1, 2].map(|c| f32::from(b[c]) / 127.5 - 1.0);
            let w = (1.0 - xyz.iter().map(|v| v * v).sum::<f32>())
                .max(0.0)
                .sqrt();
            normalised([xyz[0], xyz[1], xyz[2], w])
        });
        let sh = &s[5][i * dim * 3..];
        for k in 0..keep {
            cloud
                .sh
                .push([0, 1, 2].map(|c| (f32::from(sh[k * 3 + c]) - 128.0) / 128.0));
        }
    }
    Ok(cloud)
}

fn byte(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Writes `cloud` as SPZ `version` 2 or 3 (gzip) or 4 (zstd streams), with
/// 12 fractional position bits.
pub fn write_spz(cloud: &SplatCloud, version: u32) -> Result<Vec<u8>, String> {
    if !(2..=4).contains(&version) {
        return Err(format!("SPZ version {version} cannot be written"));
    }
    let n = cloud.len();
    let frac = 12u8;
    let per = coefs(cloud.sh_degree);
    let mut sec: [Vec<u8>; 6] = Default::default();
    for i in 0..n {
        for v in cloud.positions[i] {
            let q = (v * f32::from(1u16 << frac))
                .round()
                .clamp(-8_388_608.0, 8_388_607.0) as i32;
            sec[0].extend_from_slice(&q.to_le_bytes()[..3]);
        }
        let c = cloud.colours[i];
        sec[1].push(byte(c[3] * 255.0));
        for v in &c[..3] {
            let dc = (v - 0.5) / SH_C0;
            sec[2].push(byte(dc * SPZ_COLOUR_SCALE * 255.0 + 127.5));
        }
        for s in cloud.scales[i] {
            sec[3].push(byte((s.max(1e-30).ln() + 10.0) * 16.0));
        }
        let q = normalised(cloud.rotations[i]);
        if version >= 3 {
            let largest = (0..4)
                .max_by(|&a, &b| q[a].abs().total_cmp(&q[b].abs()))
                .expect("four components");
            let negate = q[largest] < 0.0;
            let mut comp = largest as u32;
            for (k, v) in q.iter().enumerate() {
                if k != largest {
                    let neg = u32::from((*v < 0.0) ^ negate);
                    let mag = (511.0 * (v.abs() / std::f32::consts::FRAC_1_SQRT_2) + 0.5) as u32;
                    comp = (comp << 10) | (neg << 9) | mag.min(511);
                }
            }
            sec[4].extend_from_slice(&comp.to_le_bytes());
        } else {
            let sign = if q[3] < 0.0 { -1.0 } else { 1.0 };
            for v in &q[..3] {
                sec[4].push(byte(v * sign * 127.5 + 127.5));
            }
        }
        for coef in &cloud.sh[i * per..(i + 1) * per] {
            for v in coef {
                sec[5].push(byte(v * 128.0 + 128.0));
            }
        }
    }
    let mut head = Vec::new();
    head.extend_from_slice(&SPZ_MAGIC.to_le_bytes());
    head.extend_from_slice(&version.to_le_bytes());
    head.extend_from_slice(&(n as u32).to_le_bytes());
    head.extend_from_slice(&[cloud.sh_degree, frac, 0]);
    if version < 4 {
        head.push(0);
        for s in &sec {
            head.extend_from_slice(s);
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &head).map_err(|e| e.to_string())?;
        return gz.finish().map_err(|e| e.to_string());
    }
    let streams: Vec<(usize, Vec<u8>)> = sec
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| {
            (
                s.len(),
                ruzstd::encoding::compress_to_vec(
                    s.as_slice(),
                    ruzstd::encoding::CompressionLevel::Fastest,
                ),
            )
        })
        .collect();
    head.push(streams.len() as u8);
    head.extend_from_slice(&32u32.to_le_bytes());
    head.extend_from_slice(&[0u8; 12]);
    for (raw, z) in &streams {
        head.extend_from_slice(&(z.len() as u64).to_le_bytes());
        head.extend_from_slice(&(*raw as u64).to_le_bytes());
    }
    for (_, z) in &streams {
        head.extend_from_slice(z);
    }
    Ok(head)
}

// ------------------------------------------------------------------ SOG --

/// Largest file inside a SOG archive Splat In reads.
const SOG_FILE_LIMIT: u64 = 1 << 30;

/// Reads a `.sog` archive (a zip with `meta.json` and the images at its
/// root).
pub fn read_sog_archive(bytes: &[u8]) -> Result<SplatCloud, String> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a SOG archive ({e})"))?;
    let mut load = |name: &str| -> Result<Vec<u8>, String> { zip_file(&mut zip, name) };
    let meta = load("meta.json")?;
    read_sog(&meta, &mut load)
}

fn zip_file<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, String> {
    let mut f = zip
        .by_name(name)
        .map_err(|e| format!("{name} in the archive: {e}"))?;
    if f.size() > SOG_FILE_LIMIT {
        return Err(format!("{name} is too large"));
    }
    let mut out = Vec::with_capacity(f.size() as usize);
    (&mut f)
        .take(SOG_FILE_LIMIT)
        .read_to_end(&mut out)
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}

/// An RGBA8 image.
struct Image {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

fn webp(bytes: &[u8], name: &str) -> Result<Image, String> {
    let err = |e: image_webp::DecodingError| format!("{name}: {e}");
    let mut dec = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
    if dec.is_lossy() {
        return Err(format!("{name}: SOG images must be lossless WebP"));
    }
    let (w, h) = dec.dimensions();
    let size = dec
        .output_buffer_size()
        .ok_or_else(|| format!("{name} is too large"))?;
    let mut buf = vec![0u8; size];
    dec.read_image(&mut buf).map_err(err)?;
    let rgba = if dec.has_alpha() {
        buf
    } else {
        buf.chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect()
    };
    Ok(Image {
        width: w as usize,
        height: h as usize,
        rgba,
    })
}

fn json_floats(v: &serde_json::Value, what: &str, len: usize) -> Result<Vec<f32>, String> {
    let a = v
        .as_array()
        .ok_or_else(|| format!("meta.json: {what} is missing"))?;
    let out: Vec<f32> = a
        .iter()
        .filter_map(|x| x.as_f64())
        .map(|x| x as f32)
        .collect();
    if out.len() != a.len() || (len > 0 && out.len() != len) {
        return Err(format!("meta.json: {what} must be {len} numbers"));
    }
    Ok(out)
}

/// Reads SOG version 2 from its `meta.json` and `load` (file name → bytes).
pub fn read_sog(
    meta: &[u8],
    load: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<SplatCloud, String> {
    let m: serde_json::Value =
        serde_json::from_slice(meta).map_err(|e| format!("meta.json: {e}"))?;
    match m.get("version").and_then(|v| v.as_u64()) {
        Some(2) => {}
        Some(v) => return Err(format!("SOG version {v} is not supported")),
        None => {
            return Err(
                "SOG version 1 (no `version`) is not supported: convert it with a current tool"
                    .into(),
            );
        }
    }
    let n = m
        .get("count")
        .and_then(|v| v.as_u64())
        .ok_or("meta.json: count is missing")? as usize;
    if n > MAX_SPLATS {
        return Err(format!("{n} splats; at most {MAX_SPLATS} are supported"));
    }
    let files = |key: &str, k: usize| -> Result<String, String> {
        m.get(key)
            .and_then(|p| p.get("files"))
            .and_then(|f| f.get(k))
            .and_then(|f| f.as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("meta.json: {key}.files is missing"))
    };
    let mut image = |key: &str, k: usize| -> Result<Image, String> {
        let name = files(key, k)?;
        let img = webp(&load(&name)?, &name)?;
        Ok(img)
    };
    let per_splat = |img: &Image, name: &str| -> Result<(), String> {
        if img.width * img.height < n {
            Err(format!("the {name} image is too small for {n} splats"))
        } else {
            Ok(())
        }
    };
    let lo = image("means", 0)?;
    let hi = image("means", 1)?;
    let quats = image("quats", 0)?;
    let scales = image("scales", 0)?;
    let sh0 = image("sh0", 0)?;
    for (img, name) in [
        (&lo, "means"),
        (&hi, "means"),
        (&quats, "quats"),
        (&scales, "scales"),
        (&sh0, "sh0"),
    ] {
        per_splat(img, name)?;
    }
    let mins = json_floats(&m["means"]["mins"], "means.mins", 3)?;
    let maxs = json_floats(&m["means"]["maxs"], "means.maxs", 3)?;
    let scale_book = json_floats(&m["scales"]["codebook"], "scales.codebook", 256)?;
    let colour_book = json_floats(&m["sh0"]["codebook"], "sh0.codebook", 256)?;
    let bands = m
        .get("shN")
        .and_then(|s| s.get("bands"))
        .and_then(|b| b.as_u64())
        .unwrap_or(0);
    let degree = u8::try_from(bands)
        .ok()
        .filter(|b| *b <= 3)
        .ok_or_else(|| format!("{bands} SH bands"))?;
    let per = coefs(degree);
    let sh_parts = if per > 0 {
        let book = json_floats(&m["shN"]["codebook"], "shN.codebook", 256)?;
        let palette = m["shN"]["count"].as_u64().unwrap_or(0) as usize;
        let centroids = image("shN", 0)?;
        let labels = image("shN", 1)?;
        per_splat(&labels, "shN labels")?;
        if centroids.width != 64 * per {
            return Err(format!(
                "the shN centroids image is {} wide (expected {})",
                centroids.width,
                64 * per
            ));
        }
        Some((book, palette, centroids, labels))
    } else {
        None
    };
    let unlog = |v: f32| v.signum() * (v.abs().exp() - 1.0);
    let mut cloud = SplatCloud {
        sh_degree: degree,
        positions: Vec::with_capacity(n),
        scales: Vec::with_capacity(n),
        rotations: Vec::with_capacity(n),
        colours: Vec::with_capacity(n),
        sh: Vec::with_capacity(n * per),
    };
    const KEPT: [[usize; 3]; 4] = [[1, 2, 3], [0, 2, 3], [0, 1, 3], [0, 1, 2]];
    for i in 0..n {
        let o = i * 4;
        cloud.positions.push([0, 1, 2].map(|c| {
            let q = u16::from(lo.rgba[o + c]) | u16::from(hi.rgba[o + c]) << 8;
            unlog(mins[c] + (maxs[c] - mins[c]) * (f32::from(q) / 65535.0))
        }));
        let tag = quats.rgba[o + 3];
        // (w, x, y, z), the largest dropped.
        let wxyz = if (252..=255).contains(&tag) {
            let mode = usize::from(tag - 252);
            let mut q = [0.0f32; 4];
            let mut sum = 0.0;
            for (k, &slot) in KEPT[mode].iter().enumerate() {
                let v = (f32::from(quats.rgba[o + k]) / 255.0 - 0.5) * std::f32::consts::SQRT_2;
                q[slot] = v;
                sum += v * v;
            }
            q[mode] = (1.0 - sum).max(0.0).sqrt();
            q
        } else {
            [1.0, 0.0, 0.0, 0.0]
        };
        cloud
            .rotations
            .push(normalised([wxyz[1], wxyz[2], wxyz[3], wxyz[0]]));
        cloud
            .scales
            .push([0, 1, 2].map(|c| scale_book[usize::from(scales.rgba[o + c])].exp()));
        let rgb = [0, 1, 2].map(|c| 0.5 + colour_book[usize::from(sh0.rgba[o + c])] * SH_C0);
        cloud
            .colours
            .push([rgb[0], rgb[1], rgb[2], f32::from(sh0.rgba[o + 3]) / 255.0]);
        if let Some((book, palette, centroids, labels)) = &sh_parts {
            let label = usize::from(labels.rgba[o]) | usize::from(labels.rgba[o + 1]) << 8;
            let (row, col) = (label / 64, (label % 64) * per);
            for k in 0..per {
                let px = (row * centroids.width + col + k) * 4;
                let coef = if label < *palette && row < centroids.height {
                    [0, 1, 2].map(|c| book[usize::from(centroids.rgba[px + c])])
                } else {
                    [0.0; 3]
                };
                cloud.sh.push(coef);
            }
        }
    }
    Ok(cloud)
}

/// 256 evenly spaced values over `values`' range, and each value's nearest
/// index (a simple codebook; enough for tests and examples).
fn codebook(values: &[f32]) -> (Vec<f32>, Vec<u8>) {
    let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (lo, hi) = if lo.is_finite() && hi > lo {
        (lo, hi)
    } else {
        (lo.min(0.0), lo.max(0.0) + 1.0)
    };
    let step = (hi - lo) / 255.0;
    let book: Vec<f32> = (0..256).map(|k| lo + step * k as f32).collect();
    let idx = values.iter().map(|v| byte((v - lo) / step)).collect();
    (book, idx)
}

fn encode_webp(rgba: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(rgba, w as u32, h as u32, image_webp::ColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes `cloud` as a SOG v2 archive (`.sog`): lossless WebP images and
/// even codebooks; higher SH bands as a palette of one entry per splat
/// (so at most 65 536 splats with SH).
pub fn write_sog(cloud: &SplatCloud) -> Result<Vec<u8>, String> {
    let n = cloud.len();
    let per = coefs(cloud.sh_degree);
    if per > 0 && n > 65_536 {
        return Err("SOG SH palettes hold at most 65 536 entries".into());
    }
    let w = (n as f64).sqrt().ceil().max(1.0) as usize;
    let h = n.div_ceil(w).max(1);
    let img = || vec![0u8; w * h * 4];
    let log = |v: f32| v.signum() * (v.abs() + 1.0).ln();
    let mut mins = [f32::INFINITY; 3];
    let mut maxs = [f32::NEG_INFINITY; 3];
    for p in &cloud.positions {
        for c in 0..3 {
            mins[c] = mins[c].min(log(p[c]));
            maxs[c] = maxs[c].max(log(p[c]));
        }
    }
    if n == 0 {
        mins = [0.0; 3];
        maxs = [1.0; 3];
    }
    let (mut lo, mut hi, mut quats, mut scales, mut sh0) = (img(), img(), img(), img(), img());
    let log_scales: Vec<f32> = cloud
        .scales
        .iter()
        .flatten()
        .map(|s| s.max(1e-30).ln())
        .collect();
    let (scale_book, scale_idx) = codebook(&log_scales);
    let dcs: Vec<f32> = cloud
        .colours
        .iter()
        .flat_map(|c| [0, 1, 2].map(|k| (c[k] - 0.5) / SH_C0))
        .collect();
    let (colour_book, colour_idx) = codebook(&dcs);
    for i in 0..n {
        let o = i * 4;
        for c in 0..3 {
            let range = maxs[c] - mins[c];
            let t = if range > 0.0 {
                (log(cloud.positions[i][c]) - mins[c]) / range
            } else {
                0.0
            };
            let q = (t * 65535.0).round().clamp(0.0, 65535.0) as u16;
            lo[o + c] = q as u8;
            hi[o + c] = (q >> 8) as u8;
            scales[o + c] = scale_idx[i * 3 + c];
            sh0[o + c] = colour_idx[i * 3 + c];
        }
        scales[o + 3] = 255;
        lo[o + 3] = 255;
        hi[o + 3] = 255;
        sh0[o + 3] = byte(cloud.colours[i][3] * 255.0);
        let [x, y, z, w_] = normalised(cloud.rotations[i]);
        let mut q = [w_, x, y, z];
        let mode = (0..4)
            .max_by(|&a, &b| q[a].abs().total_cmp(&q[b].abs()))
            .expect("four components");
        if q[mode] < 0.0 {
            q = q.map(|v| -v);
        }
        let mut k = 0;
        for (slot, v) in q.iter().enumerate() {
            if slot != mode {
                quats[o + k] = byte((v / std::f32::consts::SQRT_2 + 0.5) * 255.0);
                k += 1;
            }
        }
        quats[o + 3] = 252 + mode as u8;
    }
    let mut meta = serde_json::json!({
        "version": 2,
        "asset": { "generator": "Lasair" },
        "count": n,
        "means": { "mins": mins, "maxs": maxs, "files": ["means_l.webp", "means_u.webp"] },
        "scales": { "codebook": scale_book, "files": ["scales.webp"] },
        "quats": { "files": ["quats.webp"] },
        "sh0": { "codebook": colour_book, "files": ["sh0.webp"] },
    });
    let mut files = vec![
        ("means_l.webp", encode_webp(&lo, w, h)?),
        ("means_u.webp", encode_webp(&hi, w, h)?),
        ("quats.webp", encode_webp(&quats, w, h)?),
        ("scales.webp", encode_webp(&scales, w, h)?),
        ("sh0.webp", encode_webp(&sh0, w, h)?),
    ];
    if per > 0 {
        let flat: Vec<f32> = cloud.sh.iter().flatten().copied().collect();
        let (book, idx) = codebook(&flat);
        let (cw, ch) = (64 * per, n.div_ceil(64).max(1));
        let mut centroids = vec![255u8; cw * ch * 4];
        let mut labels = img();
        for i in 0..n {
            labels[i * 4] = i as u8;
            labels[i * 4 + 1] = (i >> 8) as u8;
            labels[i * 4 + 3] = 255;
            for k in 0..per {
                let px = ((i / 64) * cw + (i % 64) * per + k) * 4;
                for c in 0..3 {
                    centroids[px + c] = idx[(i * per + k) * 3 + c];
                }
            }
        }
        meta["shN"] = serde_json::json!({
            "count": n,
            "bands": cloud.sh_degree,
            "codebook": book,
            "files": ["shN_centroids.webp", "shN_labels.webp"],
        });
        files.push(("shN_centroids.webp", encode_webp(&centroids, cw, ch)?));
        files.push(("shN_labels.webp", encode_webp(&labels, w, h)?));
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    let meta_text = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    for (name, data) in std::iter::once(("meta.json", meta_text)).chain(files) {
        zip.start_file(name, stored).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut zip, &data).map_err(|e| e.to_string())?;
    }
    Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
}

/// A deterministic synthetic splat cloud for tests, examples and the bench:
/// `n` splats on a noisy sphere shell of radius 1, coloured by direction,
/// with varied anisotropic scales, rotations and opacities, and SH of
/// `degree` (small, view-dependent tints).
pub fn synthetic(n: usize, degree: u8) -> SplatCloud {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let per = coefs(degree);
    let mut cloud = SplatCloud {
        sh_degree: degree,
        ..SplatCloud::default()
    };
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    for i in 0..n {
        let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
        let r = (1.0 - y * y).max(0.0).sqrt();
        let a = golden * i as f32;
        let d = [r * a.cos(), y, r * a.sin()];
        let radius = 0.9 + 0.2 * rand();
        cloud.positions.push(d.map(|v| v * radius));
        let size = 0.6 / (n as f32).sqrt().max(1.0);
        cloud.scales.push([
            size * (0.5 + rand()),
            size * (0.5 + rand()),
            size * 0.2 * (0.5 + rand()),
        ]);
        cloud.rotations.push(normalised([
            rand() - 0.5,
            rand() - 0.5,
            rand() - 0.5,
            rand() - 0.5,
        ]));
        cloud.colours.push([
            0.5 + 0.45 * d[0],
            0.5 + 0.45 * d[1],
            0.5 + 0.45 * d[2],
            0.6 + 0.4 * rand(),
        ]);
        for k in 0..per {
            let amp = 0.25 / (1 + k / 3) as f32;
            cloud
                .sh
                .push([rand(), rand(), rand()].map(|v| (v - 0.5) * amp));
        }
    }
    cloud
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f32], b: &[f32], tol: f32, what: &str) {
        assert_eq!(a.len(), b.len(), "{what}: lengths");
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert!((x - y).abs() <= tol, "{what}[{i}]: {x} vs {y} (±{tol})");
        }
    }

    /// The same rotation (q and −q are one rotation).
    fn same_rotation(a: &[[f32; 4]], b: &[[f32; 4]], tol: f32) {
        for (i, (p, q)) in a.iter().zip(b).enumerate() {
            let d: f32 = p.iter().zip(q).map(|(x, y)| x * y).sum();
            assert!(d.abs() >= 1.0 - tol, "rotation {i}: {p:?} vs {q:?}");
        }
    }

    fn compare(
        a: &SplatCloud,
        b: &SplatCloud,
        pos: f32,
        colour: f32,
        scale: f32,
        rot: f32,
        sh: f32,
    ) {
        assert_eq!(a.len(), b.len());
        assert_eq!(a.sh_degree, b.sh_degree);
        let flat3 = |v: &[[f32; 3]]| v.iter().flatten().copied().collect::<Vec<_>>();
        let flat4 = |v: &[[f32; 4]]| v.iter().flatten().copied().collect::<Vec<_>>();
        close(&flat3(&a.positions), &flat3(&b.positions), pos, "positions");
        close(&flat4(&a.colours), &flat4(&b.colours), colour, "colours");
        // Scales compared relatively (the formats store logs).
        for (i, (x, y)) in a
            .scales
            .iter()
            .flatten()
            .zip(b.scales.iter().flatten())
            .enumerate()
        {
            assert!((x / y - 1.0).abs() <= scale, "scale[{i}]: {x} vs {y}");
        }
        same_rotation(&a.rotations, &b.rotations, rot);
        close(&flat3(&a.sh), &flat3(&b.sh), sh, "sh");
    }

    #[test]
    fn ply_round_trips_exactly_at_every_degree() {
        for degree in 0..=3 {
            let c = synthetic(200, degree);
            let back = read_ply(&write_ply(&c)).unwrap();
            compare(&c, &back, 1e-6, 1e-5, 1e-5, 1e-6, 1e-6);
            back.validate().unwrap();
        }
    }

    #[test]
    fn ply_reads_ascii_big_endian_other_elements_and_plain_point_clouds() {
        let ascii = b"ply\nformat ascii 1.0\ncomment hi\nelement camera 1\nproperty list uchar float m\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n3 1 2 3\n1 2 3 255 0 51\n-1 0 0.5 0 255 0\n";
        let c = read_ply(ascii).unwrap();
        assert_eq!(c.positions, vec![[1.0, 2.0, 3.0], [-1.0, 0.0, 0.5]]);
        close(&c.colours[0], &[1.0, 0.0, 0.2, 1.0], 1e-6, "colour");
        assert_eq!(c.scales[1], [DEFAULT_SCALE; 3]);
        assert_eq!(c.rotations[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(c.sh_degree, 0);

        let mut big = b"ply\nformat binary_big_endian 1.0\nelement face 1\nproperty int a\nelement vertex 1\nproperty double x\nproperty float y\nproperty float z\nproperty float f_dc_0\nproperty float f_dc_1\nproperty float f_dc_2\nproperty float opacity\nproperty float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\nend_header\n".to_vec();
        big.extend_from_slice(&7i32.to_be_bytes());
        big.extend_from_slice(&1.5f64.to_be_bytes());
        for v in [2.0f32, 3.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 2.0] {
            big.extend_from_slice(&v.to_be_bytes());
        }
        let c = read_ply(&big).unwrap();
        assert_eq!(c.positions, vec![[1.5, 2.0, 3.0]]);
        close(
            &c.colours[0],
            &[0.5, 0.5 + SH_C0, 0.5 - SH_C0, 0.5],
            1e-6,
            "colour",
        );
        assert_eq!(c.rotations[0], [0.0, 0.0, 1.0, 0.0], "w x y z → x y z w");
    }

    #[test]
    fn ply_rejects_broken_files() {
        assert!(read_ply(b"not a ply").is_err());
        assert!(read_ply(b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nend_header\n\0\0\0\0").is_err(), "short body");
        assert!(read_ply(b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nproperty float f_rest_0\nend_header\n").is_err(), "one f_rest");
        assert!(read_ply(b"ply\nformat binary_little_endian 1.0\nelement chunk 1\nproperty float min_x\nelement vertex 0\nproperty float x\nproperty float y\nproperty float z\nend_header\n").is_err(), "compressed PLY");
        assert!(read_ply(b"ply\nformat binary_little_endian 1.0\nelement vertex 99999999999\nproperty float x\nproperty float y\nproperty float z\nend_header\n").is_err(), "huge count");
    }

    #[test]
    fn spz_round_trips_within_its_quantisation_for_every_version() {
        for version in [2, 3, 4] {
            for degree in [0, 1, 3] {
                let c = synthetic(300, degree);
                let bytes = write_spz(&c, version).unwrap();
                let back = read_spz(&bytes).unwrap();
                back.validate().unwrap();
                // 12 fractional bits; colour and SH bytes; log scale in 1/16.
                let rot = if version >= 3 { 2e-5 } else { 1e-2 };
                compare(&c, &back, 2.0 / 4096.0, 0.006, 0.035, rot, 1.0 / 128.0);
            }
        }
    }

    #[test]
    fn spz_degree_four_keeps_three_bands_and_bad_files_fail() {
        // A hand-made v3 file of one splat at degree 4: SH bytes 0..72.
        let mut raw = Vec::new();
        raw.extend_from_slice(&SPZ_MAGIC.to_le_bytes());
        raw.extend_from_slice(&3u32.to_le_bytes());
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&[4, 12, 0, 0]);
        raw.extend_from_slice(&[0, 16, 0, 0, 0, 0, 0xff, 0xff, 0xff]); // x 1, z -1/4096
        raw.push(255);
        raw.extend_from_slice(&[128; 3]);
        raw.extend_from_slice(&[160; 3]);
        raw.extend_from_slice(&(3u32 << 30).to_le_bytes()); // identity (w largest)
        raw.extend((0..72).map(|k| 128 + k as u8));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        let c = read_spz(&gz.finish().unwrap()).unwrap();
        assert_eq!(c.sh_degree, 3);
        assert_eq!(c.sh.len(), 15);
        close(&c.positions[0], &[1.0, 0.0, -1.0 / 4096.0], 1e-7, "pos");
        close(&c.rotations[0], &[0.0, 0.0, 0.0, 1.0], 1e-6, "rot");
        close(&c.scales[0], &[(0.0f32).exp(); 3], 1e-6, "scale");
        close(
            &c.sh[14],
            &[42.0 / 128.0, 43.0 / 128.0, 44.0 / 128.0],
            1e-6,
            "last kept",
        );

        assert!(read_spz(b"").is_err());
        assert!(read_spz(&[0x1f, 0x8b, 0, 0]).is_err());
        let good = write_spz(&synthetic(10, 1), 4).unwrap();
        assert!(read_spz(&good[..good.len() - 3]).is_err(), "truncated v4");
        let mut huge = good.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_spz(&huge).is_err(), "absurd count");
    }

    #[test]
    fn sog_round_trips_within_its_quantisation() {
        for degree in [0, 2, 3] {
            let c = synthetic(500, degree);
            let bytes = write_sog(&c).unwrap();
            let back = read_sog_archive(&bytes).unwrap();
            back.validate().unwrap();
            // 16-bit log positions (range ~ln 2.1); 8-bit codebooks.
            let sh_range = 0.25f32;
            compare(&c, &back, 3e-5, 0.01, 0.02, 2e-4, sh_range / 255.0);
        }
    }

    #[test]
    fn sog_reads_a_folder_and_rejects_bad_meta() {
        let bytes = write_sog(&synthetic(40, 1)).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        let dir = std::env::temp_dir().join(format!("lsr-sog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).unwrap();
            let name = f.name().unwrap().to_string();
            let mut data = Vec::new();
            f.read_to_end(&mut data).unwrap();
            std::fs::write(dir.join(name), data).unwrap();
        }
        let a = read(&dir.join("meta.json")).unwrap();
        let b = read_sog_archive(&bytes).unwrap();
        assert_eq!(a, b);
        std::fs::remove_dir_all(&dir).unwrap();

        let mut none = |_: &str| -> Result<Vec<u8>, String> { Err("no".into()) };
        assert!(read_sog(br#"{"count": 1}"#, &mut none).is_err(), "v1");
        assert!(read_sog(br#"{"version": 3, "count": 1}"#, &mut none).is_err());
        assert!(read_sog(b"{", &mut none).is_err());
    }

    #[test]
    fn the_flip_is_a_half_turn_that_keeps_view_dependent_colour() {
        // SH basis (3DGS order) at a direction.
        fn basis(d: [f32; 3]) -> [f32; 15] {
            let [x, y, z] = d;
            let (xx, yy, zz) = (x * x, y * y, z * z);
            [
                -y,
                z,
                -x,
                x * y,
                y * z,
                2.0 * zz - xx - yy,
                x * z,
                xx - yy,
                y * (3.0 * xx - yy),
                x * y * z,
                y * (4.0 * zz - xx - yy),
                z * (2.0 * zz - 3.0 * xx - 3.0 * yy),
                x * (4.0 * zz - xx - yy),
                z * (xx - yy),
                x * (xx - 3.0 * yy),
            ]
        }
        let c = synthetic(20, 3);
        let mut f = c.clone();
        f.flip_y_down();
        let d = [0.48f32, -0.6, 0.64];
        let fd = [d[0], -d[1], -d[2]];
        for i in 0..c.len() {
            let p = c.positions[i];
            assert_eq!(f.positions[i], [p[0], -p[1], -p[2]]);
            let eval = |cloud: &SplatCloud, d| -> f32 {
                let b = basis(d);
                cloud.sh[i * 15..(i + 1) * 15]
                    .iter()
                    .zip(b)
                    .map(|(c, b)| c[0] * b)
                    .sum()
            };
            assert!(
                (eval(&c, d) - eval(&f, fd)).abs() < 1e-5,
                "colour seen from the same side"
            );
            // R' = Rx · R: rotate a vector by both and compare.
            let rot = |q: [f32; 4], v: [f32; 3]| -> [f32; 3] {
                let [x, y, z, w] = q;
                let u = [x, y, z];
                let cross = |a: [f32; 3], b: [f32; 3]| {
                    [
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ]
                };
                let t = cross(u, v).map(|c| 2.0 * c);
                let ut = cross(u, t);
                [0, 1, 2].map(|k| v[k] + w * t[k] + ut[k])
            };
            let v = [0.3, 0.5, -0.8];
            let a = rot(c.rotations[i], v);
            let b = rot(f.rotations[i], v);
            close(&b, &[a[0], -a[1], -a[2]], 1e-5, "rotation");
        }
    }

    #[test]
    fn geometry_carries_the_attributes_the_renderer_reads() {
        let c = synthetic(5, 2);
        let g = c.to_geometry(None);
        assert_eq!(g.points(), 5);
        for name in [names::P, names::CD, names::SCALE, names::ORIENT] {
            assert!(g.point(name).is_some(), "{name}");
        }
        // Degree 2: 24 floats → sh0..sh5.
        assert!(g.point("sh5").is_some() && g.point("sh6").is_none());
        assert!(matches!(
            g.detail(names::SH_DEGREE),
            Some(DetailValue::I32(2))
        ));
        let sh1 = g.point("sh1").unwrap().cpu_bytes().unwrap();
        let first = f32::from_le_bytes(sh1[..4].try_into().unwrap());
        assert_eq!(first, c.sh[1][1], "float 4 is coefficient 2, green");
    }

    /// The example's media (`examples/splats.lasair/media`): synthetic
    /// clouds, so no third-party capture is in the repository.
    fn example_media() -> [(&'static str, SplatCloud); 3] {
        // PLY and SOG stored as a trainer writes them, Y down (Splat In
        // turns them upright); SPZ stores Y up.
        let mut ply = synthetic(800, 1);
        ply.flip_y_down();
        let mut sog = synthetic(2000, 2);
        sog.flip_y_down();
        [
            ("capture.ply", ply),
            ("capture.spz", synthetic(2000, 3)),
            ("capture.sog", sog),
        ]
    }

    fn media_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/splats.lasair/media")
    }

    /// Regenerates the example media: `cargo test -p lsr-ops-pnt --lib
    /// write_example_media -- --ignored`.
    #[test]
    #[ignore = "writes examples/splats.lasair/media"]
    fn write_example_media() {
        std::fs::create_dir_all(media_dir()).unwrap();
        for (name, cloud) in example_media() {
            let bytes = match Format::from_path(Path::new(name)).unwrap() {
                Format::Ply => write_ply(&cloud),
                Format::Spz => write_spz(&cloud, 4).unwrap(),
                Format::Sog => write_sog(&cloud).unwrap(),
            };
            std::fs::write(media_dir().join(name), bytes).unwrap();
        }
    }

    #[test]
    fn the_example_media_is_the_generator_output() {
        for (name, cloud) in example_media() {
            let got = read(&media_dir().join(name)).unwrap();
            compare(&cloud, &got, 1e-3, 0.01, 0.035, 2e-4, 0.01);
        }
    }
}
