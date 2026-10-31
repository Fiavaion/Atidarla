//! Point files on the CPU: readers for PLY (ASCII and binary), CSV and
//! XYZ/PTS text, and Atidarla's own point cache (`.atdpc`), plus writers
//! for `.atdpc`, PLY and CSV. Used by Point File In and Sensor In
//! recordings (on worker threads) and by `atidarla points convert`.
//!
//! **`.atdpc`, version 1** (little-endian; `docs/point-caches.md`):
//!
//! | Bytes | Field |
//! |---|---|
//! | 6 | magic `ATDPC\0` |
//! | 2 | version (`u16`, 1) |
//! | 4 | point count (`u32`) |
//! | 4 | attribute count (`u32`) |
//! | per attribute | name length (`u8`), name (UTF-8), scalar (`u8`: 0 `f32`, 1 `i32`, 2 `u32`), components (`u8`, 1-4) |
//! | per attribute, in order | count × components values, tightly packed |
//!
//! Houdini `.bgeo` and other caches with no open specification are not read
//! here: convert them to PLY or CSV with the tool that wrote them (Houdini's
//! `gconvert`), then to `.atdpc` with `atidarla points convert`.

use std::path::Path;

use atd_core::geo::{AttrType, Scalar, names};
use atd_gpu::geo::{Attribute, BufferPool, Geometry};

/// Most points a file may hold (the GPU's 1-D dispatch reach).
pub const MAX_POINTS: usize = 16_000_000;
/// Most attributes read from one file (the rest are skipped).
pub const MAX_ATTRS: usize = 16;

const MAGIC: &[u8; 6] = b"ATDPC\0";
const VERSION: u16 = 1;

/// One attribute's values, `comps` per point, tightly packed (as 32-bit
/// words of the attribute's scalar type).
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: AttrType,
    pub words: Vec<u32>,
}

impl Column {
    pub fn floats(name: &str, comps: u8, values: &[f32]) -> Self {
        Self {
            name: name.into(),
            ty: AttrType::new(Scalar::F32, comps),
            words: values.iter().map(|v| v.to_bits()).collect(),
        }
    }

    /// Point `i`'s components as `f32`.
    pub fn get(&self, i: usize) -> [f32; 4] {
        let n = usize::from(self.ty.comps());
        let mut out = [0.0; 4];
        for (c, o) in out.iter_mut().enumerate().take(n) {
            let w = self.words[i * n + c];
            *o = match self.ty.scalar {
                Scalar::F32 => f32::from_bits(w),
                Scalar::I32 => w as i32 as f32,
                Scalar::U32 => w as f32,
            };
        }
        out
    }

    /// The values in the GPU layout (each element padded to its stride).
    fn gpu_bytes(&self, count: usize) -> Vec<u8> {
        let n = usize::from(self.ty.comps());
        let stride = self.ty.stride() as usize;
        let mut out = vec![0u8; count * stride];
        for i in 0..count {
            for c in 0..n {
                let at = i * stride + c * 4;
                out[at..at + 4].copy_from_slice(&self.words[i * n + c].to_le_bytes());
            }
        }
        out
    }
}

/// A point cloud on the CPU: a count and columns of that many points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cloud {
    pub count: usize,
    pub columns: Vec<Column>,
}

impl Cloud {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    fn push(&mut self, c: Column) -> Result<(), String> {
        let n = usize::from(c.ty.comps());
        if c.words.len() != self.count * n {
            return Err(format!("`{}` has the wrong number of values", c.name));
        }
        if !atd_core::geo::valid_name(&c.name) {
            return Err(format!("`{}` is not a valid attribute name", c.name));
        }
        if self.column(&c.name).is_some() {
            return Err(format!("`{}` appears twice", c.name));
        }
        if self.columns.len() < MAX_ATTRS {
            self.columns.push(c);
        }
        Ok(())
    }

    /// A GEO value of the cloud's points, with a point number `id` when
    /// the file has none. With `upload`, attributes go to the GPU now (call
    /// it on a worker thread) and a header is written.
    pub fn to_geometry(&self, upload: Option<(&wgpu::Queue, &BufferPool)>) -> Geometry {
        let mut g = Geometry::new();
        let n = self.count as u32;
        let header = match upload {
            Some((queue, pool)) if n > 0 => Some(atd_gpu::geo::cpu_header(queue, pool, n)),
            _ => None,
        };
        g.set_points(n, header);
        if n == 0 {
            return g;
        }
        for c in &self.columns {
            g.set_point(Attribute::cpu(
                std::sync::Arc::<str>::from(c.name.as_str()),
                c.ty,
                c.gpu_bytes(self.count),
            ));
        }
        if self.column(names::ID).is_none() {
            let ids: Vec<u32> = (0..n).collect();
            g.set_point(Attribute::from_u32(names::ID, &ids));
        }
        if let Some((queue, pool)) = upload {
            for a in g.attrs(atd_core::geo::AttrClass::Point) {
                a.ensure_gpu(queue, pool);
            }
        }
        g
    }
}

/// The file formats this module knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Atdpc,
    Ply,
    Csv,
    /// Whitespace-separated `x y z [r g b]` rows (`.xyz`, `.pts`, `.txt`;
    /// a first line with a single number, as PTS has, is a count).
    Xyz,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "atdpc" => Format::Atdpc,
            "ply" => Format::Ply,
            "csv" => Format::Csv,
            "xyz" | "pts" | "txt" => Format::Xyz,
            _ => return None,
        })
    }
}

/// `pattern` with `$F` replaced by the frame number, and `$F2`-`$F9` by it
/// zero-padded to that many digits (as Houdini names cache sequences).
pub fn frame_path(pattern: &str, frame: i64) -> String {
    let Some(at) = pattern.find("$F") else {
        return pattern.to_owned();
    };
    let rest = &pattern[at + 2..];
    let (width, tail) = match rest.as_bytes().first() {
        Some(d @ b'1'..=b'9') => (usize::from(d - b'0'), &rest[1..]),
        _ => (0, rest),
    };
    let number = if frame < 0 {
        format!("-{:0width$}", -frame)
    } else {
        format!("{frame:0width$}")
    };
    format!("{}{number}{}", &pattern[..at], frame_path(tail, frame))
}

/// Reads a point file, choosing the reader by extension.
pub fn read(path: &Path) -> Result<Cloud, String> {
    let format = Format::from_path(path).ok_or_else(|| {
        format!(
            "{}: unknown point file (use .atdpc, .ply, .csv, .xyz or .pts; convert other caches with `atidarla points convert`)",
            path.display()
        )
    })?;
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_bytes(&bytes, format).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_bytes(bytes: &[u8], format: Format) -> Result<Cloud, String> {
    match format {
        Format::Atdpc => read_atdpc(bytes),
        Format::Ply => read_ply(bytes),
        Format::Csv | Format::Xyz => {
            let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text")?;
            if format == Format::Csv {
                read_csv(text)
            } else {
                read_xyz(text)
            }
        }
    }
}

/// Writes `cloud` in `format` (not XYZ).
pub fn write(cloud: &Cloud, format: Format) -> Result<Vec<u8>, String> {
    match format {
        Format::Atdpc => Ok(write_atdpc(cloud)),
        Format::Ply => Ok(write_ply(cloud)),
        Format::Csv => Ok(write_csv(cloud).into_bytes()),
        Format::Xyz => Err("XYZ is read only; write .atdpc, .ply or .csv".into()),
    }
}

fn check_count(count: usize) -> Result<(), String> {
    if count > MAX_POINTS {
        return Err(format!("{count} points; at most {MAX_POINTS} are read"));
    }
    Ok(())
}

// ------------------------------------------------------------------ atdpc

fn scalar_code(s: Scalar) -> u8 {
    match s {
        Scalar::F32 => 0,
        Scalar::I32 => 1,
        Scalar::U32 => 2,
    }
}

pub fn write_atdpc(cloud: &Cloud) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(cloud.count as u32).to_le_bytes());
    out.extend_from_slice(&(cloud.columns.len() as u32).to_le_bytes());
    for c in &cloud.columns {
        let name = &c.name.as_bytes()[..c.name.len().min(255)];
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        out.push(scalar_code(c.ty.scalar));
        out.push(c.ty.comps());
    }
    for c in &cloud.columns {
        for w in &c.words {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    out
}

pub fn read_atdpc(bytes: &[u8]) -> Result<Cloud, String> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Result<&[u8], String> {
        let s = bytes.get(at..at + n).ok_or("the point cache ends early")?;
        at += n;
        Ok(s)
    };
    if take(6)? != MAGIC {
        return Err("not an Atidarla point cache (no `ATDPC` magic)".into());
    }
    let version = u16::from_le_bytes(take(2)?.try_into().expect("2 bytes"));
    if version != VERSION {
        return Err(format!(
            "point cache version {version}; this version reads {VERSION}"
        ));
    }
    let count = u32::from_le_bytes(take(4)?.try_into().expect("4 bytes")) as usize;
    let attrs = u32::from_le_bytes(take(4)?.try_into().expect("4 bytes")) as usize;
    check_count(count)?;
    if attrs > 64 {
        return Err(format!(
            "{attrs} attributes is more than a point cache holds"
        ));
    }
    let mut specs = Vec::with_capacity(attrs);
    for _ in 0..attrs {
        let len = usize::from(take(1)?[0]);
        let name = std::str::from_utf8(take(len)?)
            .map_err(|_| "an attribute name is not UTF-8")?
            .to_owned();
        let kind = take(2)?;
        let scalar = match kind[0] {
            0 => Scalar::F32,
            1 => Scalar::I32,
            2 => Scalar::U32,
            k => return Err(format!("`{name}`: unknown scalar type {k}")),
        };
        if !(1..=4).contains(&kind[1]) {
            return Err(format!("`{name}`: {} components", kind[1]));
        }
        specs.push((name, AttrType::new(scalar, kind[1])));
    }
    let mut cloud = Cloud {
        count,
        columns: Vec::new(),
    };
    for (name, ty) in specs {
        let n = count * usize::from(ty.comps());
        let raw = take(n * 4)?;
        let words = raw
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().expect("4 bytes")))
            .collect();
        cloud.push(Column { name, ty, words })?;
    }
    Ok(cloud)
}

// -------------------------------------------------------------------- PLY

#[derive(Debug, Clone, Copy, PartialEq)]
enum PlyType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyType {
    fn parse(s: &str) -> Option<PlyType> {
        Some(match s {
            "char" | "int8" => PlyType::I8,
            "uchar" | "uint8" => PlyType::U8,
            "short" | "int16" => PlyType::I16,
            "ushort" | "uint16" => PlyType::U16,
            "int" | "int32" => PlyType::I32,
            "uint" | "uint32" => PlyType::U32,
            "float" | "float32" => PlyType::F32,
            "double" | "float64" => PlyType::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            PlyType::I8 | PlyType::U8 => 1,
            PlyType::I16 | PlyType::U16 => 2,
            PlyType::I32 | PlyType::U32 | PlyType::F32 => 4,
            PlyType::F64 => 8,
        }
    }

    /// Divisor that brings an integer colour channel to 0-1.
    fn colour_max(self) -> f64 {
        match self {
            PlyType::U8 | PlyType::I8 => 255.0,
            PlyType::U16 | PlyType::I16 => 65535.0,
            _ => 1.0,
        }
    }
}

struct PlyProp {
    name: String,
    ty: PlyType,
    list: Option<PlyType>,
}

struct PlyElement {
    name: String,
    count: usize,
    props: Vec<PlyProp>,
}

struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
    ascii: bool,
    big: bool,
}

impl Body<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn value(&mut self, ty: PlyType) -> Result<f64, String> {
        if self.ascii {
            while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let start = self.at;
            while self.at < self.bytes.len() && !self.bytes[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let t = std::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| "bad text")?;
            if t.is_empty() {
                return Err("PLY data ends early".into());
            }
            return t
                .parse::<f64>()
                .map_err(|_| format!("PLY: `{t}` is not a number"));
        }
        let n = ty.size();
        if self.remaining() < n {
            return Err("PLY data ends early".into());
        }
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        if self.big {
            b[..n].reverse();
        }
        Ok(match ty {
            PlyType::I8 => f64::from(b[0] as i8),
            PlyType::U8 => f64::from(b[0]),
            PlyType::I16 => f64::from(i16::from_le_bytes([b[0], b[1]])),
            PlyType::U16 => f64::from(u16::from_le_bytes([b[0], b[1]])),
            PlyType::I32 => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            PlyType::U32 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            PlyType::F32 => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            PlyType::F64 => f64::from_le_bytes(b),
        })
    }
}

/// An attribute name from a file's column or property name (invalid
/// characters become `_`).
fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        s.insert(0, '_');
    }
    if s == "_" {
        s.push('v');
    }
    s
}

/// The columns of a table of per-point `values` (`count` rows of
/// `names.len()`): `x y z` → `P`, `nx ny nz` → `N`, colour channels → `Cd`
/// (scaled by each channel's divisor), `u v` → `uv`, every other column a
/// float attribute of its own.
fn columns_from_table(
    names_in: &[String],
    divisors: &[f64],
    count: usize,
    values: &[f64],
) -> Result<Cloud, String> {
    let w = names_in.len();
    let find = |cands: &[&str]| {
        names_in
            .iter()
            .position(|n| cands.iter().any(|c| n.eq_ignore_ascii_case(c)))
    };
    let xyz = [
        find(&["x", "px", "P.x"]),
        find(&["y", "py", "P.y"]),
        find(&["z", "pz", "P.z"]),
    ];
    let nxyz = [
        find(&["nx", "N.x"]),
        find(&["ny", "N.y"]),
        find(&["nz", "N.z"]),
    ];
    let rgba = [
        find(&["red", "r", "Cd.r", "Cd.x"]),
        find(&["green", "g", "Cd.g", "Cd.y"]),
        find(&["blue", "b", "Cd.b", "Cd.z"]),
        find(&["alpha", "a", "Cd.a", "Cd.w"]),
    ];
    let tuv = [
        find(&["u", "s", "texture_u"]),
        find(&["v", "t", "texture_v"]),
    ];
    if xyz.iter().any(Option::is_none) {
        return Err("no x, y and z columns".into());
    }
    let mut used = vec![false; w];
    let mut cloud = Cloud {
        count,
        columns: Vec::new(),
    };
    let group = |name: &str, idx: &[Option<usize>], fill: f32, used: &mut [bool]| {
        let n = idx.len();
        let mut out = Vec::with_capacity(count * n);
        for r in 0..count {
            for i in idx {
                out.push(match i {
                    Some(i) => (values[r * w + i] / divisors[*i]) as f32,
                    None => fill,
                });
            }
        }
        for i in idx.iter().flatten() {
            used[*i] = true;
        }
        Column::floats(name, n as u8, &out)
    };
    let p = group(names::P, &xyz, 0.0, &mut used);
    cloud.push(p)?;
    if nxyz.iter().all(Option::is_some) {
        let n = group(names::N, &nxyz, 0.0, &mut used);
        cloud.push(n)?;
    }
    if rgba[..3].iter().all(Option::is_some) {
        let cd = group(names::CD, &rgba, 1.0, &mut used);
        cloud.push(cd)?;
    }
    if tuv.iter().all(Option::is_some) {
        let uv = group(names::UV, &tuv, 0.0, &mut used);
        cloud.push(uv)?;
    }
    for (i, name) in names_in.iter().enumerate() {
        if used[i] {
            continue;
        }
        let mut name = sanitize(name);
        if cloud.column(&name).is_some() {
            name.push_str("_1");
        }
        let c = group(&name, &[Some(i)], 0.0, &mut used);
        // `id` is an unsigned point identity, as everywhere in PNT.
        let c = if c.name == names::ID {
            Column {
                ty: AttrType::U32,
                words: c
                    .words
                    .iter()
                    .map(|w| f32::from_bits(*w).max(0.0) as u32)
                    .collect(),
                ..c
            }
        } else {
            c
        };
        cloud.push(c)?;
    }
    Ok(cloud)
}

/// Reads the `vertex` element of a PLY file (ASCII, binary little- and
/// big-endian); faces and other elements are skipped.
pub fn read_ply(bytes: &[u8]) -> Result<Cloud, String> {
    let end = bytes
        .windows(10)
        .position(|w| w == b"end_header")
        .ok_or("PLY: no `end_header`")?;
    let mut body_at = end + 10;
    while body_at < bytes.len() && bytes[body_at] != b'\n' {
        body_at += 1;
    }
    body_at = (body_at + 1).min(bytes.len());
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| "PLY: the header is not text")?;
    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file (no `ply` line)".into());
    }
    let (mut ascii, mut big, mut format) = (false, false, false);
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in lines {
        let t: Vec<&str> = line.split_ascii_whitespace().collect();
        match t.as_slice() {
            ["format", f, _] => {
                format = true;
                match *f {
                    "ascii" => ascii = true,
                    "binary_little_endian" => {}
                    "binary_big_endian" => big = true,
                    _ => return Err(format!("PLY: unknown format `{f}`")),
                }
            }
            ["element", name, count] => elements.push(PlyElement {
                name: (*name).into(),
                count: count.parse().map_err(|_| "PLY: a bad element count")?,
                props: Vec::new(),
            }),
            ["property", "list", ct, it, name] => {
                let e = elements
                    .last_mut()
                    .ok_or("PLY: a property before any element")?;
                let (Some(ct), Some(it)) = (PlyType::parse(ct), PlyType::parse(it)) else {
                    return Err("PLY: unknown list type".into());
                };
                e.props.push(PlyProp {
                    name: (*name).into(),
                    ty: it,
                    list: Some(ct),
                });
            }
            ["property", ty, name] => {
                let e = elements
                    .last_mut()
                    .ok_or("PLY: a property before any element")?;
                e.props.push(PlyProp {
                    name: (*name).into(),
                    ty: PlyType::parse(ty).ok_or_else(|| format!("PLY: unknown type `{ty}`"))?,
                    list: None,
                });
            }
            _ => {}
        }
    }
    if !format {
        return Err("PLY: no `format` line".into());
    }
    let mut body = Body {
        bytes,
        at: body_at,
        ascii,
        big,
    };
    for e in &elements {
        let min_row: usize = e
            .props
            .iter()
            .map(|p| {
                if ascii {
                    2
                } else {
                    p.list.map_or(p.ty.size(), PlyType::size)
                }
            })
            .sum::<usize>()
            .max(1);
        if e.count > body.remaining() / min_row {
            return Err(format!(
                "PLY: element `{}` claims {} rows but the file is too short",
                e.name, e.count
            ));
        }
        if e.name != "vertex" {
            // Skip the element (faces of a mesh: Mesh File In reads them).
            for _ in 0..e.count {
                for p in &e.props {
                    if let Some(ct) = p.list {
                        let k = body.value(ct)?.max(0.0) as usize;
                        if k > body.remaining() {
                            return Err("PLY: a list is longer than the file".into());
                        }
                        for _ in 0..k {
                            body.value(p.ty)?;
                        }
                    } else {
                        body.value(p.ty)?;
                    }
                }
            }
            continue;
        }
        check_count(e.count)?;
        let scalars: Vec<&PlyProp> = e.props.iter().filter(|p| p.list.is_none()).collect();
        let w = scalars.len();
        let mut values = Vec::with_capacity(e.count * w);
        for _ in 0..e.count {
            for p in &e.props {
                if let Some(ct) = p.list {
                    let k = body.value(ct)?.max(0.0) as usize;
                    for _ in 0..k.min(body.remaining()) {
                        body.value(p.ty)?;
                    }
                } else {
                    values.push(body.value(p.ty)?);
                }
            }
        }
        let names_in: Vec<String> = scalars.iter().map(|p| p.name.clone()).collect();
        let colour = ["red", "green", "blue", "alpha", "r", "g", "b", "a"];
        let divisors: Vec<f64> = scalars
            .iter()
            .map(|p| {
                if colour.contains(&p.name.as_str()) {
                    p.ty.colour_max()
                } else {
                    1.0
                }
            })
            .collect();
        return columns_from_table(&names_in, &divisors, e.count, &values);
    }
    Err("PLY: no `vertex` element".into())
}

fn write_ply(cloud: &Cloud) -> Vec<u8> {
    let mut header = format!(
        "ply\nformat binary_little_endian 1.0\ncomment written by Atidarla\nelement vertex {}\n",
        cloud.count
    );
    let props = ply_props(cloud);
    for (name, _, ty) in &props {
        header.push_str(&format!("property {ty} {name}\n"));
    }
    header.push_str("end_header\n");
    let mut out = header.into_bytes();
    for i in 0..cloud.count {
        for (_, (col, comp), _) in &props {
            let c = &cloud.columns[*col];
            let w = c.words[i * usize::from(c.ty.comps()) + comp];
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    out
}

/// PLY property names for every component: (name, (column, component),
/// type).
fn ply_props(cloud: &Cloud) -> Vec<(String, (usize, usize), &'static str)> {
    let mut props = Vec::new();
    for (k, c) in cloud.columns.iter().enumerate() {
        let ty = match c.ty.scalar {
            Scalar::F32 => "float",
            Scalar::I32 => "int",
            Scalar::U32 => "uint",
        };
        let n = usize::from(c.ty.comps());
        for comp in 0..n {
            let name = match (c.name.as_str(), comp) {
                (names::P, _) => ["x", "y", "z"][comp.min(2)].to_owned(),
                (names::N, _) => ["nx", "ny", "nz"][comp.min(2)].to_owned(),
                (names::CD, _) => ["red", "green", "blue", "alpha"][comp].to_owned(),
                (names::UV, _) => ["u", "v"][comp.min(1)].to_owned(),
                (_, _) if n == 1 => c.name.clone(),
                (_, _) => format!("{}_{comp}", c.name),
            };
            props.push((name, (k, comp), ty));
        }
    }
    props
}

// ------------------------------------------------------------------- text

/// The separator of a CSV line: the first of `,`, `;` or tab it contains,
/// else whitespace.
fn split(line: &str) -> Vec<&str> {
    for sep in [',', ';', '\t'] {
        if line.contains(sep) {
            return line.split(sep).map(str::trim).collect();
        }
    }
    line.split_ascii_whitespace().collect()
}

/// CSV with a header row naming the columns (`x,y,z`, optional `nx,ny,nz`,
/// `r,g,b[,a]` or `red,green,blue` 0-1, `u,v`, and any other numeric
/// columns as float attributes).
pub fn read_csv(text: &str) -> Result<Cloud, String> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let header = lines.next().ok_or("the CSV file is empty")?;
    let names_in: Vec<String> = split(header).iter().map(|s| (*s).to_owned()).collect();
    let w = names_in.len();
    let mut values = Vec::new();
    let mut count = 0usize;
    for (row, line) in lines.enumerate() {
        let cells = split(line);
        if cells.len() != w {
            return Err(format!(
                "CSV row {}: {} values for {w} columns",
                row + 2,
                cells.len()
            ));
        }
        for c in cells {
            values.push(
                c.parse::<f64>()
                    .map_err(|_| format!("CSV row {}: `{c}` is not a number", row + 2))?,
            );
        }
        count += 1;
        check_count(count)?;
    }
    columns_from_table(&names_in, &vec![1.0; w], count, &values)
}

/// XYZ and PTS text: rows of `x y z`, optionally followed by an intensity
/// and/or `r g b`. Colours above 1 are taken as 0-255.
pub fn read_xyz(text: &str) -> Result<Cloud, String> {
    let mut rows: Vec<Vec<f64>> = Vec::new();
    for (k, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let cells = split(line);
        let v: Result<Vec<f64>, _> = cells.iter().map(|c| c.parse::<f64>()).collect();
        let v = v.map_err(|_| format!("line {}: not numbers", k + 1))?;
        if rows.is_empty() && v.len() == 1 {
            // A PTS point count.
            continue;
        }
        if v.len() < 3 {
            return Err(format!("line {}: fewer than three values", k + 1));
        }
        rows.push(v);
        check_count(rows.len())?;
    }
    let w = rows.first().map_or(3, Vec::len);
    if rows.iter().any(|r| r.len() != w) {
        return Err("rows have different numbers of values".into());
    }
    let mut names_in: Vec<String> = ["x", "y", "z"].map(String::from).to_vec();
    match w {
        4 => names_in.push("intensity".into()),
        6 => names_in.extend(["r", "g", "b"].map(String::from)),
        n if n >= 7 => {
            names_in.push("intensity".into());
            names_in.extend(["r", "g", "b"].map(String::from));
            for k in 7..n {
                names_in.push(format!("c{k}"));
            }
        }
        n => {
            for k in 3..n {
                names_in.push(format!("c{k}"));
            }
        }
    }
    let colour_scale = if rows.iter().any(|r| {
        names_in
            .iter()
            .zip(r)
            .any(|(n, v)| ["r", "g", "b"].contains(&n.as_str()) && *v > 1.0)
    }) {
        255.0
    } else {
        1.0
    };
    let divisors: Vec<f64> = names_in
        .iter()
        .map(|n| {
            if ["r", "g", "b"].contains(&n.as_str()) {
                colour_scale
            } else {
                1.0
            }
        })
        .collect();
    let count = rows.len();
    let values: Vec<f64> = rows.into_iter().flatten().collect();
    columns_from_table(&names_in, &divisors, count, &values)
}

fn write_csv(cloud: &Cloud) -> String {
    let props = ply_props(cloud);
    let mut s = props
        .iter()
        .map(|(n, _, _)| n.clone())
        .collect::<Vec<_>>()
        .join(",");
    s.push('\n');
    for i in 0..cloud.count {
        let row: Vec<String> = props
            .iter()
            .map(|(_, (col, comp), _)| {
                let c = &cloud.columns[*col];
                c.get(i)[*comp].to_string()
            })
            .collect();
        s.push_str(&row.join(","));
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Cloud {
        let mut c = Cloud {
            count: 2,
            columns: Vec::new(),
        };
        c.push(Column::floats("P", 3, &[0.0, 1.0, 2.0, -1.0, 0.5, 3.0]))
            .unwrap();
        c.push(Column::floats(
            "Cd",
            4,
            &[1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.5],
        ))
        .unwrap();
        c.push(Column::floats("pscale", 1, &[0.1, 0.2])).unwrap();
        c.push(Column {
            name: "id".into(),
            ty: AttrType::U32,
            words: vec![7, 9],
        })
        .unwrap();
        c
    }

    #[test]
    fn atdpc_round_trips_exactly() {
        let c = sample();
        let bytes = write_atdpc(&c);
        assert_eq!(read_atdpc(&bytes).unwrap(), c);
        assert!(read_atdpc(&bytes[..bytes.len() - 1]).is_err(), "short");
        assert!(read_atdpc(b"ATDPX\0").is_err(), "magic");
    }

    #[test]
    fn ply_and_csv_round_trip_through_their_writers() {
        let c = sample();
        let ply = read_ply(&write_ply(&c)).unwrap();
        assert_eq!(ply.count, 2);
        assert_eq!(ply.column("P"), c.column("P"));
        assert_eq!(ply.column("Cd"), c.column("Cd"), "float colours stay 0-1");
        assert_eq!(ply.column("pscale"), c.column("pscale"));
        let csv = read_csv(&write_csv(&c)).unwrap();
        assert_eq!(csv.column("P").unwrap().get(1), [-1.0, 0.5, 3.0, 0.0]);
        assert_eq!(csv.column("Cd").unwrap().get(1)[3], 0.5);
        assert_eq!(csv.column("id").unwrap().words, vec![7, 9]);
    }

    #[test]
    fn reads_ascii_ply_with_byte_colours_and_skips_faces() {
        let text = "ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nproperty float intensity\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255 0 0 0.5\n1 0 0 0 255 0 0.25\n0 1 0 0 0 255 1\n3 0 1 2\n";
        let c = read_ply(text.as_bytes()).unwrap();
        assert_eq!(c.count, 3);
        assert_eq!(c.column("Cd").unwrap().get(1), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(c.column("intensity").unwrap().get(0)[0], 0.5);
        assert!(
            read_ply(b"ply\nformat ascii 1.0\nelement vertex 9\nproperty float x\nend_header\n1\n")
                .is_err()
        );
    }

    #[test]
    fn reads_xyz_and_pts_text() {
        let pts = "2\n0 0 0 10 255 128 0\n1 2 3 20 0 0 255\n";
        let c = read_xyz(pts).unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(c.column("P").unwrap().get(1), [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(c.column("intensity").unwrap().get(1)[0], 20.0);
        let cd = c.column("Cd").unwrap().get(0);
        assert!((cd[1] - 128.0 / 255.0).abs() < 1e-6 && cd[3] == 1.0);
        assert_eq!(read_xyz("1 2 3\n4 5 6\n").unwrap().count, 2);
        assert!(read_xyz("1 2\n").is_err());
    }

    #[test]
    fn frame_patterns_expand() {
        assert_eq!(frame_path("cache.$F4.atdpc", 12), "cache.0012.atdpc");
        assert_eq!(frame_path("c_$F.ply", 3), "c_3.ply");
        assert_eq!(frame_path("still.ply", 3), "still.ply");
        assert_eq!(frame_path("$F2/$F3.csv", 7), "07/007.csv");
    }

    #[test]
    fn geometry_has_gpu_layout_and_ids() {
        let mut c = sample();
        c.columns.retain(|c| c.name != "id");
        let g = c.to_geometry(None);
        assert_eq!(g.points(), 2);
        let p = g.point("P").unwrap();
        assert_eq!(p.bytes(), 32, "vec3 elements padded to 16 bytes");
        let id = g.point("id").unwrap().cpu_bytes().unwrap();
        assert_eq!(&id[4..8], &1u32.to_le_bytes());
    }
}
