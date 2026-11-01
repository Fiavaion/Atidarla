//! Mesh files: OBJ, PLY and glTF 2.0 readers and writers, written here so
//! every byte of a hostile file goes through code we can bound and fuzz
//! (design M9): sizes and counts are checked against the bytes that remain
//! before anything is allocated, and every index is validated.

use std::collections::HashMap;
use std::path::Path;

use lsr_core::geo::{AttrClass, names};
use serde_json::{Value, json};

use crate::edit::triangulate_polygon;
use crate::mesh::{Column, Mesh, limits};

/// Largest mesh file read.
pub const MAX_FILE: u64 = 512 << 20;

/// How OBJ corners become points (OBJ indexes positions, texture
/// coordinates and normals separately; GEO has one index per point).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjPoints {
    /// One point per distinct (position, uv, normal) triple: seams split.
    Split,
    /// One point per position; uv and normal from its first corner.
    Position,
}

/// A file format, from an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Obj,
    Ply,
    Gltf,
    Glb,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "obj" => Format::Obj,
            "ply" => Format::Ply,
            "gltf" => Format::Gltf,
            "glb" => Format::Glb,
            _ => return None,
        })
    }

    pub fn from_name(name: &str) -> Option<Format> {
        Some(match name {
            "obj" => Format::Obj,
            "ply" => Format::Ply,
            "gltf" => Format::Gltf,
            "glb" => Format::Glb,
            _ => return None,
        })
    }
}

/// Reads a mesh file (format from its extension, or the `glTF` magic).
pub fn read(path: &Path, obj: ObjPoints) -> Result<Mesh, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE {
        return Err(format!(
            "{} is {} MiB; mesh files are read up to {} MiB",
            path.display(),
            meta.len() >> 20,
            MAX_FILE >> 20
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let m = match Format::from_path(path) {
        _ if bytes.starts_with(b"glTF") => read_gltf(&bytes, Some(dir))?,
        Some(Format::Obj) => read_obj(&bytes, obj)?,
        Some(Format::Ply) => read_ply(&bytes)?,
        Some(Format::Gltf | Format::Glb) => read_gltf(&bytes, Some(dir))?,
        None => {
            return Err(format!(
                "{}: unknown mesh format (use .obj, .ply, .gltf or .glb)",
                path.display()
            ));
        }
    };
    m.validate()?;
    Ok(m)
}

/// Writes `m` as `format`.
pub fn write(m: &Mesh, format: Format) -> Result<Vec<u8>, String> {
    Ok(match format {
        Format::Obj => write_obj(m).into_bytes(),
        Format::Ply => write_ply(m),
        Format::Gltf => write_gltf(m, false)?,
        Format::Glb => write_gltf(m, true)?,
    })
}

fn too_big(points: usize, prims: usize, corners: usize) -> Result<(), String> {
    limits::check(points as u64, prims as u64, corners as u64)
}

// -------------------------------------------------------------------- OBJ --

/// Reads Wavefront OBJ: `v` (with optional `r g b` colours), `vt`, `vn`,
/// `f` (with negative indices), `l`, `p`, and `g`/`o` names as primitive
/// groups (at most 32).
pub fn read_obj(bytes: &[u8], mode: ObjPoints) -> Result<Mesh, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut col: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut nrm: Vec<[f32; 3]> = Vec::new();
    // Corners as (v, vt, vn) indices (vt/vn: u32::MAX when absent).
    let mut prims: Vec<(Vec<[u32; 3]>, bool, u32)> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut current: u32 = 0;
    let mut corners = 0usize;
    for (ln, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut it = line.split_ascii_whitespace();
        let Some(tag) = it.next() else { continue };
        let err = |what: &str| format!("OBJ line {}: {what}", ln + 1);
        let floats = |it: std::str::SplitAsciiWhitespace<'_>| -> Vec<f32> {
            it.map_while(|t| t.parse::<f32>().ok()).collect()
        };
        match tag {
            "v" => {
                let f = floats(it);
                if f.len() < 3 {
                    return Err(err("a vertex needs x y z"));
                }
                pos.push([f[0], f[1], f[2]]);
                if f.len() >= 6 {
                    col.resize(pos.len() - 1, [1.0; 3]);
                    col.push([f[3], f[4], f[5]]);
                }
                too_big(pos.len(), 0, 0)?;
            }
            "vt" => {
                let f = floats(it);
                if f.is_empty() {
                    return Err(err("a texture coordinate needs u"));
                }
                uvs.push([f[0], f.get(1).copied().unwrap_or(0.0)]);
                too_big(uvs.len(), 0, 0)?;
            }
            "vn" => {
                let f = floats(it);
                if f.len() < 3 {
                    return Err(err("a normal needs x y z"));
                }
                nrm.push([f[0], f[1], f[2]]);
                too_big(nrm.len(), 0, 0)?;
            }
            "f" | "l" | "p" => {
                let mut ids = Vec::new();
                for t in it {
                    let mut parts = t.split('/');
                    let resolve = |s: Option<&str>, n: usize| -> Result<u32, String> {
                        match s.filter(|s| !s.is_empty()) {
                            None => Ok(u32::MAX),
                            Some(s) => {
                                let i: i64 = s.parse().map_err(|_| err("a bad index"))?;
                                let k = if i < 0 { n as i64 + i } else { i - 1 };
                                if i == 0 || k < 0 || k >= n as i64 {
                                    return Err(err(&format!("index {i} is out of range")));
                                }
                                Ok(k as u32)
                            }
                        }
                    };
                    let v = resolve(parts.next(), pos.len())?;
                    if v == u32::MAX {
                        return Err(err("a corner needs a vertex index"));
                    }
                    let vt = resolve(parts.next(), uvs.len())?;
                    let vn = resolve(parts.next(), nrm.len())?;
                    ids.push([v, vt, vn]);
                }
                corners += ids.len();
                too_big(0, prims.len() + 1, corners)?;
                match tag {
                    "f" if ids.len() >= 3 => prims.push((ids, true, current)),
                    "l" if ids.len() >= 2 => prims.push((ids, false, current)),
                    // Points: kept as loose points (no primitive).
                    "p" => prims.push((ids, false, u32::MAX)),
                    _ => {}
                }
            }
            "g" | "o" => {
                current = 0;
                for name in it {
                    let name = sanitize(name);
                    let bit = match groups.iter().position(|g| *g == name) {
                        Some(i) => i,
                        None if groups.len() < 32 => {
                            groups.push(name);
                            groups.len() - 1
                        }
                        None => continue,
                    };
                    current |= 1 << bit;
                }
            }
            _ => {}
        }
    }
    let mut m = Mesh::new();
    let mut index: HashMap<[u32; 3], u32> = HashMap::new();
    let (mut out_uv, mut out_n, mut out_c) = (Vec::new(), Vec::new(), Vec::new());
    let has_uv = !uvs.is_empty();
    let has_n = !nrm.is_empty();
    let has_c = !col.is_empty();
    col.resize(pos.len(), [1.0; 3]);
    let mut push_point = |m: &mut Mesh, c: [u32; 3]| -> u32 {
        let id = m.add_point(pos[c[0] as usize]);
        if has_uv {
            out_uv.push(if c[1] == u32::MAX {
                [0.0; 2]
            } else {
                uvs[c[1] as usize]
            });
        }
        if has_n {
            out_n.push(if c[2] == u32::MAX {
                [0.0; 3]
            } else {
                nrm[c[2] as usize]
            });
        }
        if has_c {
            out_c.push(col[c[0] as usize]);
        }
        id
    };
    if mode == ObjPoints::Position {
        // Every position becomes a point, in file order; attributes from
        // the first corner that uses it.
        let mut first = vec![[u32::MAX; 3]; pos.len()];
        for (ids, _, _) in &prims {
            for c in ids {
                if first[c[0] as usize][0] == u32::MAX {
                    first[c[0] as usize] = *c;
                }
            }
        }
        for (i, f) in first.iter().enumerate() {
            let c = if f[0] == u32::MAX {
                [i as u32, u32::MAX, u32::MAX]
            } else {
                *f
            };
            push_point(&mut m, c);
        }
    }
    let mut group_mask = Vec::new();
    for (ids, closed, mask) in &prims {
        let pts: Vec<u32> = ids
            .iter()
            .map(|c| match mode {
                ObjPoints::Position => c[0],
                ObjPoints::Split => *index.entry(*c).or_insert_with(|| push_point(&mut m, *c)),
            })
            .collect();
        if *mask == u32::MAX {
            continue;
        }
        m.add_prim(&pts, *closed);
        group_mask.push(*mask);
    }
    if has_uv {
        m.set_point_attr(Column::f32(
            names::UV,
            2,
            out_uv.into_iter().flatten().collect(),
        ));
    }
    if has_n {
        m.set_point_attr(Column::vec3(names::N, &out_n));
    }
    if has_c {
        let rgba: Vec<f32> = out_c.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
        m.set_point_attr(Column::f32(names::CD, 4, rgba));
    }
    if !groups.is_empty() {
        m.prim_groups = groups;
        m.prim_attrs.push(Column::u32(names::GROUP, group_mask));
    }
    Ok(m)
}

/// A group name made of letters, digits and `_`.
fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, 'g');
    }
    s
}

/// `N`, `uv` and `Cd` point values, when present with the standard types.
type Standard<'a> = (Option<&'a [f32]>, Option<&'a [f32]>, Option<&'a [f32]>);

fn point_cols(m: &Mesh) -> Standard<'_> {
    let get = |name: &str, comps: u8| {
        m.point_attr(name)
            .filter(|c| c.comps == comps)
            .and_then(Column::floats)
    };
    (get(names::N, 3), get(names::UV, 2), get(names::CD, 4))
}

/// Writes OBJ (point-indexed: corner `i` uses `v`, `vt` and `vn` number
/// `i`); primitive groups become `g` lines.
pub fn write_obj(m: &Mesh) -> String {
    use std::fmt::Write as _;
    let (n, uv, cd) = point_cols(m);
    let mut s = String::from("# Lasair mesh\n");
    for (i, p) in m.p.iter().enumerate() {
        let _ = write!(s, "v {} {} {}", p[0], p[1], p[2]);
        if let Some(c) = cd {
            let _ = write!(s, " {} {} {}", c[i * 4], c[i * 4 + 1], c[i * 4 + 2]);
        }
        s.push('\n');
    }
    if let Some(uv) = uv {
        for t in uv.chunks_exact(2) {
            let _ = writeln!(s, "vt {} {}", t[0], t[1]);
        }
    }
    if let Some(n) = n {
        for v in n.chunks_exact(3) {
            let _ = writeln!(s, "vn {} {} {}", v[0], v[1], v[2]);
        }
    }
    let masks: Vec<u32> = match m.prim_attr(names::GROUP).map(|c| &c.values) {
        Some(crate::mesh::Values::U32(v)) if !m.prim_groups.is_empty() => v.clone(),
        _ => vec![0; m.prims()],
    };
    let mut last = u32::MAX;
    for f in 0..m.prims() {
        if !m.prim_groups.is_empty() && masks[f] != last {
            last = masks[f];
            let names: Vec<&str> = m
                .prim_groups
                .iter()
                .enumerate()
                .filter(|(b, _)| masks[f] & (1 << b) != 0)
                .map(|(_, g)| g.as_str())
                .collect();
            let _ = writeln!(
                s,
                "g {}",
                if names.is_empty() {
                    "default".into()
                } else {
                    names.join(" ")
                }
            );
        }
        s.push_str(if m.closed[f] { "f" } else { "l" });
        for &c in m.prim(f) {
            let k = c + 1;
            match (uv.is_some(), n.is_some()) {
                (true, true) => {
                    let _ = write!(s, " {k}/{k}/{k}");
                }
                (true, false) => {
                    let _ = write!(s, " {k}/{k}");
                }
                (false, true) => {
                    let _ = write!(s, " {k}//{k}");
                }
                _ => {
                    let _ = write!(s, " {k}");
                }
            }
        }
        s.push('\n');
    }
    s
}

// -------------------------------------------------------------------- PLY --

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

    /// Normalisation divisor for colour channels.
    fn max(self) -> f64 {
        match self {
            PlyType::U8 | PlyType::I8 => 255.0,
            PlyType::U16 | PlyType::I16 => 65535.0,
            _ => 1.0,
        }
    }
}

#[derive(Debug)]
struct PlyProp {
    name: String,
    ty: PlyType,
    /// The count type of a list property.
    list: Option<PlyType>,
}

#[derive(Debug)]
struct PlyElement {
    name: String,
    count: usize,
    props: Vec<PlyProp>,
}

/// A cursor over PLY body values (ASCII tokens or binary).
struct PlyBody<'a> {
    bytes: &'a [u8],
    at: usize,
    ascii: bool,
    big: bool,
}

impl PlyBody<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn token(&mut self) -> Result<&str, String> {
        while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
        let start = self.at;
        while self.at < self.bytes.len() && !self.bytes[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
        if start == self.at {
            return Err("PLY data ends early".into());
        }
        std::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| "PLY: bad text".into())
    }

    fn value(&mut self, ty: PlyType) -> Result<f64, String> {
        if self.ascii {
            let t = self.token()?;
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

/// Reads PLY (ASCII, binary little- and big-endian): the `vertex` element
/// (`x y z`, `nx ny nz`, `red green blue alpha`, `u v` / `s t` /
/// `texture_u texture_v`) and the `face` element (`vertex_indices`);
/// other elements are skipped.
pub fn read_ply(bytes: &[u8]) -> Result<Mesh, String> {
    let end = find(bytes, b"end_header").ok_or("PLY: no `end_header`")?;
    let mut body_at = end + b"end_header".len();
    // The header ends at the end of its line.
    while body_at < bytes.len() && bytes[body_at] != b'\n' {
        body_at += 1;
    }
    body_at = (body_at + 1).min(bytes.len());
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| "PLY: the header is not text")?;
    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file (no `ply` line)".into());
    }
    let (mut ascii, mut big) = (false, false);
    let mut format_seen = false;
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in lines {
        let t: Vec<&str> = line.split_ascii_whitespace().collect();
        match t.as_slice() {
            ["format", f, _] => {
                format_seen = true;
                match *f {
                    "ascii" => ascii = true,
                    "binary_little_endian" => {}
                    "binary_big_endian" => big = true,
                    _ => return Err(format!("PLY: unknown format `{f}`")),
                }
            }
            ["element", name, count] => {
                let count: usize = count.parse().map_err(|_| "PLY: a bad element count")?;
                elements.push(PlyElement {
                    name: (*name).into(),
                    count,
                    props: Vec::new(),
                });
            }
            ["property", "list", ct, it, name] => {
                let e = elements
                    .last_mut()
                    .ok_or("PLY: a property before any element")?;
                let (ct, it) = (PlyType::parse(ct), PlyType::parse(it));
                let (Some(ct), Some(it)) = (ct, it) else {
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
                let ty = PlyType::parse(ty).ok_or_else(|| format!("PLY: unknown type `{ty}`"))?;
                e.props.push(PlyProp {
                    name: (*name).into(),
                    ty,
                    list: None,
                });
            }
            _ => {}
        }
    }
    if !format_seen {
        return Err("PLY: no `format` line".into());
    }
    let mut body = PlyBody {
        bytes,
        at: body_at,
        ascii,
        big,
    };
    let mut m = Mesh::new();
    let (mut n, mut uv, mut cd) = (Vec::new(), Vec::new(), Vec::new());
    let (mut has_n, mut has_uv, mut has_cd) = (false, false, false);
    let mut row = Vec::new();
    let mut ids = Vec::new();
    for e in &elements {
        // Each row needs at least this many bytes (ASCII: a digit and a
        // separator per value), so a count larger than the file is refused
        // before anything is allocated.
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
        let vertex = e.name == "vertex";
        let face = e.name == "face";
        if vertex {
            too_big(e.count, 0, 0)?;
            m.p.reserve(e.count);
            let find = |names: &[&str]| {
                e.props
                    .iter()
                    .position(|p| names.contains(&p.name.as_str()))
            };
            let xyz = [find(&["x"]), find(&["y"]), find(&["z"])];
            let nxyz = [find(&["nx"]), find(&["ny"]), find(&["nz"])];
            let rgba = [
                find(&["red", "r"]),
                find(&["green", "g"]),
                find(&["blue", "b"]),
                find(&["alpha", "a"]),
            ];
            let tuv = [
                find(&["u", "s", "texture_u"]),
                find(&["v", "t", "texture_v"]),
            ];
            has_n = nxyz.iter().all(Option::is_some);
            has_cd = rgba[..3].iter().all(Option::is_some);
            has_uv = tuv.iter().all(Option::is_some);
            for _ in 0..e.count {
                row.clear();
                for p in &e.props {
                    if let Some(ct) = p.list {
                        let k = body.value(ct)?;
                        for _ in 0..(k.max(0.0) as usize).min(body.remaining()) {
                            body.value(p.ty)?;
                        }
                        row.push(0.0);
                    } else {
                        row.push(body.value(p.ty)?);
                    }
                }
                let g = |i: Option<usize>| i.map_or(0.0, |i| row[i]) as f32;
                m.p.push(xyz.map(g));
                if has_n {
                    n.push(nxyz.map(g));
                }
                if has_cd {
                    let c = rgba.map(|i| i.map_or(1.0, |i| (row[i] / e.props[i].ty.max()) as f32));
                    cd.extend(c);
                }
                if has_uv {
                    uv.extend(tuv.map(g));
                }
            }
        } else {
            let list = e.props.iter().position(|p| {
                p.list.is_some() && (p.name == "vertex_indices" || p.name == "vertex_index")
            });
            for _ in 0..e.count {
                ids.clear();
                for (k, p) in e.props.iter().enumerate() {
                    match p.list {
                        Some(ct) => {
                            let count = body.value(ct)?;
                            let count = if count.is_finite() && count >= 0.0 {
                                count as usize
                            } else {
                                0
                            };
                            if count > body.remaining() / if ascii { 2 } else { p.ty.size() } {
                                return Err("PLY: a list is longer than the file".into());
                            }
                            for _ in 0..count {
                                let v = body.value(p.ty)?;
                                if face && Some(k) == list {
                                    if !(v >= 0.0 && (v as usize) < m.p.len()) {
                                        return Err(format!("PLY: face index {v} is out of range"));
                                    }
                                    ids.push(v as u32);
                                }
                            }
                        }
                        None => {
                            body.value(p.ty)?;
                        }
                    }
                }
                if face && ids.len() >= 3 {
                    too_big(0, m.prims() + 1, m.corners.len() + ids.len())?;
                    m.add_polygon(&ids);
                }
            }
        }
    }
    if has_n {
        m.set_point_attr(Column::vec3(names::N, &n));
    }
    if has_uv {
        m.set_point_attr(Column::f32(names::UV, 2, uv));
    }
    if has_cd {
        m.set_point_attr(Column::f32(names::CD, 4, cd));
    }
    Ok(m)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Writes binary little-endian PLY: positions, normals, uv and colours
/// (8-bit) when present, polygons as `vertex_indices` (polylines are not
/// written: PLY has no standard element for them).
pub fn write_ply(m: &Mesh) -> Vec<u8> {
    let (n, uv, cd) = point_cols(m);
    let polys: Vec<usize> = (0..m.prims()).filter(|&f| m.closed[f]).collect();
    let mut h = String::from("ply\nformat binary_little_endian 1.0\ncomment Lasair mesh\n");
    h += &format!(
        "element vertex {}\nproperty float x\nproperty float y\nproperty float z\n",
        m.points()
    );
    if n.is_some() {
        h += "property float nx\nproperty float ny\nproperty float nz\n";
    }
    if uv.is_some() {
        h += "property float u\nproperty float v\n";
    }
    if cd.is_some() {
        h +=
            "property uchar red\nproperty uchar green\nproperty uchar blue\nproperty uchar alpha\n";
    }
    h += &format!(
        "element face {}\nproperty list uchar int vertex_indices\nend_header\n",
        polys.len()
    );
    let mut b = h.into_bytes();
    for i in 0..m.points() {
        for c in m.p[i] {
            b.extend(c.to_le_bytes());
        }
        if let Some(n) = n {
            for c in &n[i * 3..i * 3 + 3] {
                b.extend(c.to_le_bytes());
            }
        }
        if let Some(uv) = uv {
            for c in &uv[i * 2..i * 2 + 2] {
                b.extend(c.to_le_bytes());
            }
        }
        if let Some(cd) = cd {
            for c in &cd[i * 4..i * 4 + 4] {
                b.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
    }
    let mut tris = Vec::new();
    for f in polys {
        let p = m.prim(f);
        if p.len() <= 255 {
            b.push(p.len() as u8);
            for &c in p {
                b.extend((c as i32).to_le_bytes());
            }
        } else {
            // Too many corners for a uchar count: written as triangles.
            tris.clear();
            triangulate_polygon(&m.p, p, &mut tris);
            for t in &tris {
                b.push(3);
                for &c in t {
                    b.extend((c as i32).to_le_bytes());
                }
            }
        }
    }
    b
}

// ------------------------------------------------------------------- glTF --

const MAX_DEPTH: usize = 64;

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c).ok_or("glTF: bad base64 data")?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn base64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                s.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn uint(v: &Value, key: &str) -> Option<usize> {
    v.get(key)?.as_u64().and_then(|x| usize::try_from(x).ok())
}

/// One glTF file's buffers and accessors.
struct Gltf<'a> {
    doc: &'a Value,
    buffers: Vec<Vec<u8>>,
}

impl Gltf<'_> {
    /// Accessor `i` as floats (`comps` per element) with normalisation.
    fn floats(&self, i: usize) -> Result<(Vec<f32>, usize), String> {
        let a = self
            .doc
            .get("accessors")
            .and_then(|v| v.get(i))
            .ok_or_else(|| format!("glTF: no accessor {i}"))?;
        if a.get("sparse").is_some() {
            return Err("glTF: sparse accessors are not supported".into());
        }
        let count = uint(a, "count").ok_or("glTF: an accessor without a count")?;
        let comps = match a.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            t => return Err(format!("glTF: accessor type {t:?} is not supported")),
        };
        let ct = uint(a, "componentType").ok_or("glTF: an accessor without a component type")?;
        let (size, norm): (usize, f64) = match ct {
            5120 => (1, 127.0),
            5121 => (1, 255.0),
            5122 => (2, 32767.0),
            5123 => (2, 65535.0),
            5125 => (4, 0.0),
            5126 => (4, 0.0),
            _ => return Err(format!("glTF: component type {ct} is not supported")),
        };
        let normalized = a
            .get("normalized")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let Some(view_i) = uint(a, "bufferView") else {
            // No view: zeros (bounded: a tiny file must not claim millions).
            if count > 1 << 16 {
                return Err("glTF: an accessor without data is too long".into());
            }
            return Ok((vec![0.0; count * comps], comps));
        };
        let view = self
            .doc
            .get("bufferViews")
            .and_then(|v| v.get(view_i))
            .ok_or("glTF: a missing buffer view")?;
        let buf = self
            .buffers
            .get(uint(view, "buffer").ok_or("glTF: a view without a buffer")?)
            .ok_or("glTF: a missing buffer")?;
        let v_off = uint(view, "byteOffset").unwrap_or(0);
        let v_len = uint(view, "byteLength").ok_or("glTF: a view without a length")?;
        let a_off = uint(a, "byteOffset").unwrap_or(0);
        let elem = size * comps;
        let stride = uint(view, "byteStride").unwrap_or(elem).max(elem);
        let need = if count == 0 {
            0
        } else {
            (count - 1)
                .checked_mul(stride)
                .and_then(|x| x.checked_add(elem + a_off))
                .ok_or("glTF: an accessor is too large")?
        };
        if v_off.checked_add(v_len).is_none_or(|e| e > buf.len()) || need > v_len {
            return Err("glTF: an accessor reaches past its buffer".into());
        }
        too_big(count, 0, 0)?;
        let data = &buf[v_off + a_off..v_off + v_len];
        let mut out = Vec::with_capacity(count * comps);
        for e in 0..count {
            for k in 0..comps {
                let at = e * stride + k * size;
                let b = &data[at..at + size];
                let raw = match ct {
                    5120 => f64::from(b[0] as i8),
                    5121 => f64::from(b[0]),
                    5122 => f64::from(i16::from_le_bytes([b[0], b[1]])),
                    5123 => f64::from(u16::from_le_bytes([b[0], b[1]])),
                    5125 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    _ => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                };
                let v = if normalized && norm > 0.0 {
                    (raw / norm).max(-1.0)
                } else {
                    raw
                };
                out.push(v as f32);
            }
        }
        Ok((out, comps))
    }

    fn indices(&self, i: usize) -> Result<Vec<u32>, String> {
        let (v, comps) = self.floats(i)?;
        if comps != 1 {
            return Err("glTF: indices must be scalars".into());
        }
        Ok(v.into_iter().map(|x| x as u32).collect())
    }
}

/// Reads glTF 2.0 (`.glb`, or `.gltf` with data URIs or files next to it):
/// static meshes of the default scene with node transforms (triangles,
/// strips, fans, lines and points), `POSITION`, `NORMAL`, `TEXCOORD_0`,
/// `TEXCOORD_1` (as `uv2`), `COLOR_0`. Skins, morph targets and animations are ignored; compressed
/// meshes are refused.
pub fn read_gltf(bytes: &[u8], dir: Option<&Path>) -> Result<Mesh, String> {
    let (json_bytes, bin): (&[u8], Option<&[u8]>) = if bytes.starts_with(b"glTF") {
        if bytes.len() < 20 {
            return Err("GLB: too short".into());
        }
        let word =
            |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        if word(4) != 2 {
            return Err(format!("GLB version {} (only 2 is read)", word(4)));
        }
        let total = word(8).min(bytes.len());
        let mut at = 12;
        let (mut json, mut bin) = (None, None);
        while at + 8 <= total {
            let (len, kind) = (word(at), word(at + 4));
            let start = at + 8;
            let end = start
                .checked_add(len)
                .filter(|&e| e <= total)
                .ok_or("GLB: a chunk reaches past the file")?;
            match kind {
                0x4E4F534A => json = Some(&bytes[start..end]),
                0x004E4942 => bin = Some(&bytes[start..end]),
                _ => {}
            }
            at = end;
        }
        (json.ok_or("GLB: no JSON chunk")?, bin)
    } else {
        (bytes, None)
    };
    let doc: Value = serde_json::from_slice(json_bytes).map_err(|e| format!("glTF JSON: {e}"))?;
    if let Some(req) = doc.get("extensionsRequired").and_then(Value::as_array)
        && !req.is_empty()
    {
        let names: Vec<&str> = req.iter().filter_map(Value::as_str).collect();
        return Err(format!(
            "glTF: required extensions are not supported: {}",
            names.join(", ")
        ));
    }
    let mut buffers = Vec::new();
    for (i, b) in doc
        .get("buffers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let len = uint(b, "byteLength").ok_or("glTF: a buffer without a length")?;
        let data = match b.get("uri").and_then(Value::as_str) {
            None if i == 0 => bin.ok_or("glTF: buffer 0 has no data")?.to_vec(),
            None => return Err("glTF: a buffer without data".into()),
            Some(uri) if uri.starts_with("data:") => {
                let comma = uri.find(',').ok_or("glTF: a bad data URI")?;
                if !uri[..comma].ends_with(";base64") {
                    return Err("glTF: data URIs must be base64".into());
                }
                base64_decode(&uri[comma + 1..])?
            }
            Some(uri) => {
                let dir = dir.ok_or("glTF: external buffers need a file location")?;
                let rel = Path::new(uri);
                if rel.is_absolute() || uri.contains(':') {
                    return Err(format!("glTF: buffer `{uri}` must be a relative file"));
                }
                let path = dir.join(rel);
                let size = std::fs::metadata(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .len();
                if size > MAX_FILE {
                    return Err(format!("{}: too large", path.display()));
                }
                std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?
            }
        };
        if data.len() < len {
            return Err(format!("glTF: buffer {i} is shorter than its byteLength"));
        }
        buffers.push(data);
    }
    let g = Gltf { doc: &doc, buffers };
    let mut out = Mesh::new();
    let mut cols = Cols::default();
    // Nodes of the default scene (or every mesh, untransformed).
    let nodes = doc.get("nodes").and_then(Value::as_array);
    let scene = uint(&doc, "scene").unwrap_or(0);
    let roots: Vec<usize> = doc
        .get("scenes")
        .and_then(|s| s.get(scene))
        .and_then(|s| s.get("nodes"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_u64().map(|x| x as usize))
                .collect()
        })
        .unwrap_or_default();
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let nodes = nodes.filter(|_| !roots.is_empty());
    if nodes.is_none() {
        for i in 0..doc
            .get("meshes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
        {
            add_mesh(&g, i, identity, &mut out, &mut cols)?;
        }
    } else if let Some(nodes) = nodes {
        let mut stack: Vec<(usize, [[f64; 4]; 4], usize)> =
            roots.iter().rev().map(|&r| (r, identity, 0)).collect();
        let mut visits = 0usize;
        while let Some((i, parent, depth)) = stack.pop() {
            visits += 1;
            if depth > MAX_DEPTH || visits > nodes.len() * 4 + 64 {
                return Err("glTF: the node hierarchy is too deep or has cycles".into());
            }
            let node = nodes.get(i).ok_or_else(|| format!("glTF: no node {i}"))?;
            let m = mat_mul(parent, node_matrix(node));
            if let Some(mesh) = uint(node, "mesh") {
                add_mesh(&g, mesh, m, &mut out, &mut cols)?;
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                for c in children.iter().rev().filter_map(Value::as_u64) {
                    stack.push((c as usize, m, depth + 1));
                }
            }
        }
    }
    cols.finish(&mut out);
    Ok(out)
}

#[derive(Default)]
struct Cols {
    n: Option<Vec<[f32; 3]>>,
    uv: Option<Vec<f32>>,
    /// `TEXCOORD_1` (VAT In finds texels by it).
    uv2: Option<Vec<f32>>,
    cd: Option<Vec<f32>>,
}

impl Cols {
    fn finish(self, m: &mut Mesh) {
        if let Some(n) = self.n {
            m.set_point_attr(Column::vec3(names::N, &n));
        }
        if let Some(uv) = self.uv {
            m.set_point_attr(Column::f32(names::UV, 2, uv));
        }
        if let Some(uv2) = self.uv2 {
            m.set_point_attr(Column::f32("uv2", 2, uv2));
        }
        if let Some(cd) = self.cd {
            m.set_point_attr(Column::f32(names::CD, 4, cd));
        }
    }
}

fn node_matrix(node: &Value) -> [[f64; 4]; 4] {
    let nums = |key: &str| -> Option<Vec<f64>> {
        Some(
            node.get(key)?
                .as_array()?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0))
                .collect(),
        )
    };
    if let Some(m) = nums("matrix").filter(|m| m.len() == 16) {
        // Column-major.
        return std::array::from_fn(|r| std::array::from_fn(|c| m[c * 4 + r]));
    }
    let t = nums("translation")
        .filter(|v| v.len() == 3)
        .unwrap_or(vec![0.0; 3]);
    let q = nums("rotation")
        .filter(|v| v.len() == 4)
        .unwrap_or(vec![0.0, 0.0, 0.0, 1.0]);
    let s = nums("scale")
        .filter(|v| v.len() == 3)
        .unwrap_or(vec![1.0; 3]);
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    std::array::from_fn(|i| {
        if i == 3 {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            [r[i][0] * s[0], r[i][1] * s[1], r[i][2] * s[2], t[i]]
        }
    })
}

fn mat_mul(a: [[f64; 4]; 4], b: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}

fn add_mesh(
    g: &Gltf<'_>,
    mesh: usize,
    mat: [[f64; 4]; 4],
    out: &mut Mesh,
    cols: &mut Cols,
) -> Result<(), String> {
    let prims = g
        .doc
        .get("meshes")
        .and_then(|m| m.get(mesh))
        .and_then(|m| m.get("primitives"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("glTF: no mesh {mesh}"))?;
    // Normal matrix: the inverse transpose of the upper 3×3 (by cofactors).
    let a = |i: usize, j: usize| mat[i][j];
    let cof: [[f64; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            let (i1, i2) = ((i + 1) % 3, (i + 2) % 3);
            let (j1, j2) = ((j + 1) % 3, (j + 2) % 3);
            a(i1, j1) * a(i2, j2) - a(i1, j2) * a(i2, j1)
        })
    });
    let det: f64 = (0..3).map(|j| a(0, j) * cof[0][j]).sum();
    for p in prims {
        let attrs = p
            .get("attributes")
            .ok_or("glTF: a primitive without attributes")?;
        let Some(pos_i) = uint(attrs, "POSITION") else {
            continue;
        };
        let (pos, pc) = g.floats(pos_i)?;
        if pc != 3 {
            return Err("glTF: POSITION must be VEC3".into());
        }
        let count = pos.len() / 3;
        let base = out.points();
        too_big(base + count, 0, 0)?;
        for v in pos.chunks_exact(3) {
            let v = v.iter().map(|&x| f64::from(x)).collect::<Vec<_>>();
            let q: [f64; 3] = std::array::from_fn(|i| {
                mat[i][0] * v[0] + mat[i][1] * v[1] + mat[i][2] * v[2] + mat[i][3]
            });
            out.add_point(q.map(|x| x as f32));
        }
        // Optional attributes; earlier points without them get defaults.
        let read = |key: &str, want: usize| -> Result<Option<Vec<f32>>, String> {
            match uint(attrs, key) {
                None => Ok(None),
                Some(i) => {
                    let (v, c) = g.floats(i)?;
                    if v.len() / c.max(1) != count {
                        return Err(format!("glTF: {key} has a different count from POSITION"));
                    }
                    // COLOR_0 may be RGB: pad alpha.
                    let v = if c == 3 && want == 4 {
                        v.chunks_exact(3)
                            .flat_map(|c| [c[0], c[1], c[2], 1.0])
                            .collect()
                    } else if c == want {
                        v
                    } else {
                        return Err(format!("glTF: {key} has {c} components"));
                    };
                    Ok(Some(v))
                }
            }
        };
        if let Some(n) = read("NORMAL", 3)? {
            let list = cols.n.get_or_insert_with(|| vec![[0.0; 3]; base]);
            for v in n.chunks_exact(3) {
                let q: [f64; 3] = std::array::from_fn(|i| {
                    (0..3).map(|j| cof[i][j] * f64::from(v[j])).sum::<f64>() * det.signum()
                });
                list.push(crate::mesh::v3::f32(crate::mesh::v3::norm(q)));
            }
        }
        if let Some(n) = cols.n.as_mut() {
            n.resize(base + count, [0.0; 3]);
        }
        if let Some(uv) = read("TEXCOORD_0", 2)? {
            cols.uv
                .get_or_insert_with(|| vec![0.0; base * 2])
                .extend(uv);
        }
        if let Some(uv) = cols.uv.as_mut() {
            uv.resize((base + count) * 2, 0.0);
        }
        if let Some(uv) = read("TEXCOORD_1", 2)? {
            cols.uv2
                .get_or_insert_with(|| vec![0.0; base * 2])
                .extend(uv);
        }
        if let Some(uv) = cols.uv2.as_mut() {
            uv.resize((base + count) * 2, 0.0);
        }
        if let Some(cd) = read("COLOR_0", 4)? {
            cols.cd
                .get_or_insert_with(|| vec![1.0; base * 4])
                .extend(cd);
        }
        if let Some(cd) = cols.cd.as_mut() {
            cd.resize((base + count) * 4, 1.0);
        }
        let idx: Vec<u32> = match uint(p, "indices") {
            Some(i) => g.indices(i)?,
            None => (0..count as u32).collect(),
        };
        if let Some(bad) = idx.iter().find(|&&i| i as usize >= count) {
            return Err(format!(
                "glTF: index {bad} is out of range ({count} vertices)"
            ));
        }
        let b = base as u32;
        let flip = det < 0.0;
        let tri = |out: &mut Mesh, x: u32, y: u32, z: u32| {
            if x != y && y != z && x != z {
                let t = if flip {
                    [b + x, b + z, b + y]
                } else {
                    [b + x, b + y, b + z]
                };
                out.add_polygon(&t);
            }
        };
        let mode = uint(p, "mode").unwrap_or(4);
        too_big(
            out.points(),
            out.prims() + idx.len(),
            out.corners.len() + idx.len() * 3,
        )?;
        match mode {
            4 => idx.chunks_exact(3).for_each(|t| tri(out, t[0], t[1], t[2])),
            5 => {
                for k in 2..idx.len() {
                    if k % 2 == 0 {
                        tri(out, idx[k - 2], idx[k - 1], idx[k]);
                    } else {
                        tri(out, idx[k - 1], idx[k - 2], idx[k]);
                    }
                }
            }
            6 => {
                for k in 2..idx.len() {
                    tri(out, idx[0], idx[k - 1], idx[k]);
                }
            }
            1 => idx.chunks_exact(2).for_each(|l| {
                out.add_prim(&[b + l[0], b + l[1]], false);
            }),
            2 | 3 => {
                let mut l: Vec<u32> = idx.iter().map(|i| b + i).collect();
                if mode == 2 && !l.is_empty() {
                    l.push(l[0]);
                }
                if l.len() >= 2 {
                    out.add_prim(&l, false);
                }
            }
            _ => {} // points: the points alone
        }
    }
    Ok(())
}

/// Writes glTF 2.0: one mesh with a triangle primitive (polygons
/// triangulated) and a line primitive for polylines, sharing `POSITION`
/// (with the `min`/`max` the specification requires), `NORMAL`,
/// `TEXCOORD_0` and `COLOR_0`. `glb`: binary; else JSON with the data
/// embedded as a base64 URI. Deterministic: no dates, sorted keys.
pub fn write_gltf(m: &Mesh, glb: bool) -> Result<Vec<u8>, String> {
    let (n, uv, cd) = point_cols(m);
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut push_view = |bin: &mut Vec<u8>, data: &[u8], target: u32| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        views.push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": data.len(), "target": target}));
        bin.extend_from_slice(data);
        views.len() - 1
    };
    let floats = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
    let count = m.points();
    let mut attributes = serde_json::Map::new();
    if count > 0 {
        let flat: Vec<f32> = m.p.iter().flatten().copied().collect();
        let (lo, hi) = m.bounds().expect("has points");
        let view = push_view(&mut bin, &floats(&flat), 34962);
        accessors.push(json!({"bufferView": view, "componentType": 5126, "count": count, "type": "VEC3", "min": lo, "max": hi}));
        attributes.insert("POSITION".into(), json!(accessors.len() - 1));
        for (key, data, ty) in [
            ("NORMAL", n, "VEC3"),
            ("TEXCOORD_0", uv, "VEC2"),
            ("COLOR_0", cd, "VEC4"),
        ] {
            if let Some(d) = data {
                let view = push_view(&mut bin, &floats(d), 34962);
                accessors.push(
                    json!({"bufferView": view, "componentType": 5126, "count": count, "type": ty}),
                );
                attributes.insert(key.into(), json!(accessors.len() - 1));
            }
        }
    }
    let mut tris = Vec::new();
    let mut lines: Vec<u32> = Vec::new();
    for f in 0..m.prims() {
        if m.closed[f] {
            triangulate_polygon(&m.p, m.prim(f), &mut tris);
        } else {
            for w in m.prim(f).windows(2) {
                lines.extend_from_slice(w);
            }
        }
    }
    let mut primitives = Vec::new();
    for (mode, idx) in [
        (4u32, tris.iter().flatten().copied().collect::<Vec<u32>>()),
        (1, lines),
    ] {
        if idx.is_empty() || count == 0 {
            continue;
        }
        let data: Vec<u8> = idx.iter().flat_map(|x| x.to_le_bytes()).collect();
        let view = push_view(&mut bin, &data, 34963);
        accessors.push(json!({"bufferView": view, "componentType": 5125, "count": idx.len(), "type": "SCALAR"}));
        primitives
            .push(json!({"attributes": attributes, "indices": accessors.len() - 1, "mode": mode}));
    }
    if primitives.is_empty() && count > 0 {
        primitives.push(json!({"attributes": attributes, "mode": 0}));
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let mut buffer = json!({"byteLength": bin.len()});
    if !glb {
        buffer["uri"] = json!(format!(
            "data:application/octet-stream;base64,{}",
            base64_encode(&bin)
        ));
    }
    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "Lasair"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": primitives}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [buffer],
    });
    if bin.is_empty() {
        doc["buffers"] = json!([]);
        doc["bufferViews"] = json!([]);
        doc["meshes"] = json!([]);
        doc["nodes"] = json!([{}]);
    }
    let text = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
    if !glb {
        return Ok(text.into_bytes());
    }
    let mut j = text.into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let total = 12 + 8 + j.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((j.len() as u32).to_le_bytes());
    out.extend(0x4E4F534Au32.to_le_bytes());
    out.extend(j);
    if !bin.is_empty() {
        out.extend((bin.len() as u32).to_le_bytes());
        out.extend(0x004E4942u32.to_le_bytes());
        out.extend(bin);
    }
    Ok(out)
}

/// Members of prim group `name` (for tests).
pub fn prim_group(m: &Mesh, name: &str) -> Vec<bool> {
    m.group_members(AttrClass::Prim, name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..10 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 1) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }
}
