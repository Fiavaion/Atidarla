//! Mesh File In and Mesh File Out (formats in [`crate::io`]).

use std::path::Path;

use atd_core::geo::names;
use atd_core::registry::ParamSchema;

use crate::cook::{Job, Snap};
use crate::io::{self, Format, ObjPoints};
use crate::mesh::{Column, Mesh};
use crate::normals::point_normals;

pub fn in_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::path("file", "File")
            .help("An .obj, .ply, .gltf or .glb file (relative paths are relative to the project folder)."),
        ParamSchema::menu("obj_points", "OBJ Points", &["split", "position"], "split").help(
            "OBJ only: split makes one point per distinct position, texture coordinate and normal (seams and hard edges stay exact); position makes one point per position (welded, attributes from its first use).",
        ),
        ParamSchema::toggle("normals", "Add Normals", true)
            .help("Computes smooth normals `N` when the file has none."),
    ]
}

pub fn file_in(s: &Snap, _: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let file = s.str("file");
    if file.is_empty() {
        return Ok(Mesh::new());
    }
    let mode = if s.str("obj_points") == "position" {
        ObjPoints::Position
    } else {
        ObjPoints::Split
    };
    let mut m = io::read(Path::new(file), mode)?;
    job.check()?;
    if s.bool("normals") && m.point_attr(names::N).is_none() && m.prims() > 0 {
        let n = point_normals(&m);
        m.set_point_attr(Column::vec3(names::N, &n));
    }
    Ok(m)
}

pub fn out_params() -> Vec<ParamSchema> {
    vec![
        ParamSchema::path("file", "File")
            .help("Where to write (relative paths are relative to the project folder); empty writes nothing."),
        ParamSchema::menu("format", "Format", &["auto", "obj", "ply", "gltf", "glb"], "auto")
            .help("auto: from the file's extension."),
    ]
}

/// Writes its input to a file whenever the input or the parameters change,
/// and passes the input through.
pub fn file_out(s: &Snap, inputs: &[Option<Mesh>], job: &Job) -> Result<Mesh, String> {
    let m = inputs
        .first()
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(Mesh::new);
    let file = s.str("file");
    if file.is_empty() {
        return Ok(m);
    }
    let path = Path::new(file);
    let format = match s.str("format") {
        "auto" => Format::from_path(path)
            .ok_or("use a file ending in .obj, .ply, .gltf or .glb, or choose a format")?,
        f => Format::from_name(f).ok_or("unknown format")?,
    };
    let bytes = io::write(&m, format)?;
    job.check()?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Write beside it and rename, so a reader never sees half a file.
    let tmp = path.with_extension("atd-partial");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(m)
}
