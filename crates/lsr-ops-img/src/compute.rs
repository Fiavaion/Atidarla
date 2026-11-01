//! Compute (PRD FR-K5, §6 IMG render & code): a WGSL compute shader written
//! by the user, with `#bind` declarations for its images, parameters and
//! scratch buffers, a tile (workgroup) size and an iteration count.
//!
//! The user writes `fn kernel(id: vec2u)`, run once per output pixel; the
//! operator generates the declarations and the entry point around it.
//! Directives (one per line, `#` first on the line):
//!
//! ```text
//! #bind input  in0                          the input socket in0..in3, as `in0: texture_2d<f32>`
//! #bind output dst [format]                 a write-only storage image the size of the output
//! #bind state  heat [format] [from=in0]     a read-write image pair, ping-ponged per iteration:
//!                                           read `heat` (texture_2d<f32>), write `heat_out`
//! #bind param  p1 speed [f32|vec2f|vec3f|vec4f]   names parameter P1 in the shader: `speed()`
//! #bind buffer hist 256 [u32|i32|f32|vec2f|vec4f|atomic_u32|atomic_i32]
//!                                           a read-write storage buffer; `pixels` as the
//!                                           count sizes it to the output (one per pixel)
//! ```
//!
//! The node's image is the `output` if one is declared, else the first
//! `state` after the last iteration. States without `from=` and buffers
//! persist from frame to frame (zero at the start and after Reset); a state
//! with `from=inN` is loaded from that input at the start of every cook.
//! Formats are the storage formats wgpu guarantees on every backend
//! (WebGPU core; DX12, Metal and Vulkan): `rgba8unorm`, `rgba16float`
//! (default), `rgba32float` and `r32float`.
//!
//! Compilation (directive parsing, naga validation, then the driver) runs on
//! a worker thread, and a file is watched by its own thread, as WGSL does
//! (rule 1). The last program that compiled keeps running, with its state,
//! while a new one compiles or when it fails.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lsr_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use lsr_gpu::engine::{CookCtx, GpuOp};

use crate::stateful::{clear, clear_view};
use crate::watch::{Watcher, watch};

/// Input sockets `in0`..`in3`.
pub const MAX_INPUTS: usize = 4;
/// Parameters `p1`..`p8`.
pub const MAX_PARAMS: usize = 8;
/// Most dispatches per cook.
pub const MAX_ITERATIONS: u32 = 256;
/// Writable images (outputs and states) per shader: WebGPU's (and wgpu's
/// downlevel) guaranteed `max_storage_textures_per_shader_stage`.
pub const MAX_WRITABLE: usize = 4;
/// Scratch buffers per shader: wgpu's downlevel guaranteed
/// `max_storage_buffers_per_shader_stage`.
pub const MAX_BUFFERS: usize = 4;
/// Bytes of the per-dispatch uniform block (`LsrParams`).
pub const UNIFORM_BYTES: u64 = 160;
/// Stride between per-dispatch uniform blocks: the guaranteed
/// `min_uniform_buffer_offset_alignment`.
pub const UNIFORM_STRIDE: u64 = 256;

/// Tile (workgroup) sizes offered; each is at most 256 invocations and
/// 256 wide, within every adapter's guaranteed compute limits.
pub const TILES: &[&str] = &["8x8", "16x16", "32x8", "8x32", "64x4", "256x1"];

/// Storage image formats a `#bind` may name: those with guaranteed
/// write-only storage on every wgpu backend.
pub const FORMATS: &[(&str, wgpu::TextureFormat)] = &[
    ("rgba8unorm", wgpu::TextureFormat::Rgba8Unorm),
    ("rgba16float", wgpu::TextureFormat::Rgba16Float),
    ("rgba32float", wgpu::TextureFormat::Rgba32Float),
    ("r32float", wgpu::TextureFormat::R32Float),
];

const DEFAULT: &str = "#bind input in0
#bind output dst

fn kernel(id: vec2u) {
    textureStore(dst, id, textureLoad(in0, id, 0));
}
";

pub fn schema() -> OperatorSchema {
    let input = |name: &str| SocketSchema {
        name: name.into(),
        wire: WireType::Img,
        optional: true,
        feedback: false,
    };
    let mut params = vec![
        ParamSchema::string("code", "Code", DEFAULT)
            .help("The compute shader with its `#bind` lines, used when File is empty.")
            .multiline(),
        ParamSchema::path("file", "File")
            .help("Path to a file holding the shader; reloads when it changes."),
    ];
    for i in 1..=MAX_PARAMS {
        params.push(
            ParamSchema::vec(&format!("p{i}"), &format!("P{i}"), &[0.0; 4]).help(&format!(
                "Returned by `p{i}()`, or by the name a `#bind param p{i}` line gives it."
            )),
        );
    }
    params.extend([
        ParamSchema::menu("tile", "Tile", TILES, "8x8")
            .help("Workgroup size (`TILE_X` × `TILE_Y`): pixels each GPU work group covers."),
        ParamSchema::int(
            "iterations",
            "Iterations",
            1,
            Some(1),
            Some(MAX_ITERATIONS.into()),
        )
        .help("Dispatches per cook; `state` images swap between them."),
        ParamSchema::pulse("reset", "Reset")
            .help("Clears the persistent `state` images and buffers."),
    ]);
    OperatorSchema {
        type_id: "img.compute".into(),
        family: OpFamily::Img,
        label: "Compute".into(),
        summary: "A compute shader you write in WGSL: `fn kernel(id: vec2u)` runs once per output pixel. `#bind` lines declare the inputs (`in0`…`in3`), a storage `output` image, read-write `state` images that swap between iterations and persist, named parameters (`p1`…`p8`) and scratch buffers; Tile sets the workgroup size and Iterations how many dispatches run per cook. `time()`, `frame()`, `size()`, `iteration()` and sampler `samp` are available. A file reloads whenever it is saved; the last working shader keeps running while you edit. Reference: docs/compute.md.".into(),
        version: 1,
        params,
        inputs: (0..MAX_INPUTS).map(|i| input(&format!("in{i}"))).collect(),
        outputs: vec![SocketSchema {
            name: "out".into(),
            wire: WireType::Img,
            optional: true,
            feedback: false,
        }],
        capabilities: vec![],
        // One dispatch over a 1080p image is far below this on a discrete
        // GPU; iterations multiply it.
        cook_budget_us: 500,
        // State persists, and time() is common.
        time_dependent: true,
        sink: false,
    }
}

// ------------------------------------------------------------ directives

/// The type a named parameter is read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    F32,
    Vec2,
    Vec3,
    Vec4,
}

impl ParamType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "f32" => Self::F32,
            "vec2f" => Self::Vec2,
            "vec3f" => Self::Vec3,
            "vec4f" => Self::Vec4,
            _ => return None,
        })
    }

    fn wgsl(self) -> (&'static str, &'static str) {
        match self {
            Self::F32 => ("f32", ".x"),
            Self::Vec2 => ("vec2f", ".xy"),
            Self::Vec3 => ("vec3f", ".xyz"),
            Self::Vec4 => ("vec4f", ""),
        }
    }
}

/// A scratch buffer's element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemType {
    F32,
    U32,
    I32,
    Vec2,
    Vec4,
    AtomicU32,
    AtomicI32,
}

impl ElemType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "f32" => Self::F32,
            "u32" => Self::U32,
            "i32" => Self::I32,
            "vec2f" => Self::Vec2,
            "vec4f" => Self::Vec4,
            "atomic_u32" => Self::AtomicU32,
            "atomic_i32" => Self::AtomicI32,
            _ => return None,
        })
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::Vec2 => "vec2f",
            Self::Vec4 => "vec4f",
            Self::AtomicU32 => "atomic<u32>",
            Self::AtomicI32 => "atomic<i32>",
        }
    }

    pub fn bytes(self) -> u64 {
        match self {
            Self::Vec2 => 8,
            Self::Vec4 => 16,
            _ => 4,
        }
    }
}

/// How many elements a buffer holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    Fixed(u32),
    /// One per output pixel.
    Pixels,
}

/// One `#bind` line.
#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Input {
        socket: usize,
    },
    Output {
        name: String,
        format: wgpu::TextureFormat,
    },
    State {
        name: String,
        format: wgpu::TextureFormat,
        from: Option<usize>,
    },
    Param {
        slot: usize,
        name: String,
        ty: ParamType,
    },
    Buffer {
        name: String,
        count: Count,
        ty: ElemType,
    },
}

/// The declarations of one shader, in binding order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub decls: Vec<Decl>,
}

/// What a plan allocates; two plans with the same resources share them, so
/// editing the code keeps the state.
#[derive(Debug, Clone, PartialEq)]
enum Resource {
    Image(String, wgpu::TextureFormat, bool),
    Buffer(String, Count, ElemType),
}

impl Plan {
    fn resources(&self) -> Vec<Resource> {
        self.decls
            .iter()
            .filter_map(|d| match d {
                Decl::Output { name, format } => {
                    Some(Resource::Image(name.clone(), *format, false))
                }
                Decl::State { name, format, .. } => {
                    Some(Resource::Image(name.clone(), *format, true))
                }
                Decl::Buffer { name, count, ty } => {
                    Some(Resource::Buffer(name.clone(), *count, *ty))
                }
                _ => None,
            })
            .collect()
    }

    fn outputs(&self) -> usize {
        self.decls
            .iter()
            .filter(|d| matches!(d, Decl::Output { .. }))
            .count()
    }

    fn states(&self) -> usize {
        self.decls
            .iter()
            .filter(|d| matches!(d, Decl::State { .. }))
            .count()
    }

    /// A one-line summary of the named parameters, for the node's status.
    fn status(&self) -> String {
        let named: Vec<String> = self
            .decls
            .iter()
            .filter_map(|d| match d {
                Decl::Param { slot, name, ty } => {
                    Some(format!("P{} = {name}: {}", slot + 1, ty.wgsl().0))
                }
                _ => None,
            })
            .collect();
        named.join(", ")
    }
}

/// Names the generated code defines.
const RESERVED: &[&str] = &[
    "lsr",
    "samp",
    "kernel",
    "time",
    "frame",
    "size",
    "iteration",
    "iterations",
    "TILE_X",
    "TILE_Y",
    "LsrParams",
];

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s != "_"
        && !s.starts_with("__")
}

fn socket(s: &str) -> Option<usize> {
    let n: usize = s.strip_prefix("in")?.parse().ok()?;
    (n < MAX_INPUTS && s.len() == 3).then_some(n)
}

fn format_of(s: &str) -> Option<wgpu::TextureFormat> {
    FORMATS.iter().find(|(n, _)| *n == s).map(|(_, f)| *f)
}

/// Tokens of a line with their 1-based byte columns.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((s + 1, &line[s..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s + 1, &line[s..]));
    }
    out
}

/// Whether a line is a directive (its first non-blank character is `#`).
fn is_directive(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Parses the `#bind` lines of `source`. Errors read
/// `line L, col C: message`. `max_buffer` is the device's largest storage
/// buffer binding, in bytes.
pub fn parse(source: &str, max_buffer: u64) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut names: Vec<String> = Vec::new();
    for (n, line) in source.lines().enumerate() {
        if !is_directive(line) {
            continue;
        }
        let toks = tokens(line);
        let at = |col: usize, msg: String| format!("line {}, col {col}: {msg}", n + 1);
        let (col0, head) = toks[0];
        if head != "#bind" {
            return Err(at(
                col0,
                format!("unknown directive `{head}`; the only directive is `#bind`"),
            ));
        }
        let Some(&(kcol, kind)) = toks.get(1) else {
            return Err(at(
                col0,
                "`#bind` needs a kind: input, output, state, param or buffer".into(),
            ));
        };
        let args = &toks[2..];
        let need = |i: usize, what: &str| -> Result<(usize, &str), String> {
            args.get(i)
                .copied()
                .ok_or_else(|| at(line.len() + 1, format!("`#bind {kind}` needs {what}")))
        };
        let name = |col: usize, s: &str, names: &mut Vec<String>| -> Result<String, String> {
            if !is_ident(s) {
                return Err(at(col, format!("`{s}` is not a WGSL name")));
            }
            if RESERVED.contains(&s)
                || s.starts_with("lsr_")
                || socket(s).is_some()
                || (s.len() == 2 && s.starts_with('p') && s[1..].parse::<u8>().is_ok())
            {
                return Err(at(col, format!("`{s}` is reserved")));
            }
            if names.iter().any(|n| n == s) {
                return Err(at(col, format!("`{s}` is declared twice")));
            }
            names.push(s.to_owned());
            Ok(s.to_owned())
        };
        let extra = |i: usize| -> Result<(), String> {
            match args.get(i) {
                Some(&(c, t)) => Err(at(c, format!("unexpected `{t}`"))),
                None => Ok(()),
            }
        };
        let decl = match kind {
            "input" => {
                let (c, s) = need(0, "a socket (in0 to in3)")?;
                let socket = socket(s)
                    .ok_or_else(|| at(c, format!("`{s}` is not an input socket (in0 to in3)")))?;
                if plan
                    .decls
                    .iter()
                    .any(|d| matches!(d, Decl::Input { socket: s2 } if *s2 == socket))
                {
                    return Err(at(c, format!("`{s}` is declared twice")));
                }
                extra(1)?;
                Decl::Input { socket }
            }
            "output" | "state" => {
                let (c, s) = need(0, "a name")?;
                let image = name(c, s, &mut names)?;
                let mut format = wgpu::TextureFormat::Rgba16Float;
                let mut from = None;
                let mut seen_format = false;
                for &(c, t) in &args[1..] {
                    if let Some(src) = t.strip_prefix("from=") {
                        if kind != "state" || from.is_some() {
                            return Err(at(c, format!("unexpected `{t}`")));
                        }
                        from = Some(socket(src).ok_or_else(|| {
                            at(c, format!("`{src}` is not an input socket (in0 to in3)"))
                        })?);
                    } else if let Some(f) = format_of(t) {
                        if seen_format {
                            return Err(at(c, format!("unexpected `{t}`")));
                        }
                        format = f;
                        seen_format = true;
                    } else {
                        let list: Vec<&str> = FORMATS.iter().map(|(n, _)| *n).collect();
                        return Err(at(
                            c,
                            format!(
                                "`{t}` is not a storage format; use one of {}",
                                list.join(", ")
                            ),
                        ));
                    }
                }
                if kind == "output" {
                    if plan.outputs() > 0 {
                        return Err(at(c, "only one `#bind output` is allowed".into()));
                    }
                    Decl::Output {
                        name: image,
                        format,
                    }
                } else {
                    let state_out = format!("{image}_out");
                    if names.contains(&state_out) {
                        return Err(at(c, format!("`{state_out}` is declared twice")));
                    }
                    names.push(state_out);
                    Decl::State {
                        name: image,
                        format,
                        from,
                    }
                }
            }
            "param" => {
                let (c, s) = need(0, "a parameter (p1 to p8)")?;
                let slot = s
                    .strip_prefix('p')
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| (1..=MAX_PARAMS).contains(n) && s.len() == 2)
                    .ok_or_else(|| at(c, format!("`{s}` is not a parameter (p1 to p8)")))?
                    - 1;
                if plan
                    .decls
                    .iter()
                    .any(|d| matches!(d, Decl::Param { slot: s2, .. } if *s2 == slot))
                {
                    return Err(at(c, format!("`{s}` is named twice")));
                }
                let (nc, ns) = need(1, "a name")?;
                let pname = name(nc, ns, &mut names)?;
                let ty = match args.get(2) {
                    Some(&(tc, t)) => ParamType::parse(t).ok_or_else(|| {
                        at(tc, format!("`{t}` is not f32, vec2f, vec3f or vec4f"))
                    })?,
                    None => ParamType::Vec4,
                };
                extra(3)?;
                Decl::Param {
                    slot,
                    name: pname,
                    ty,
                }
            }
            "buffer" => {
                let (c, s) = need(0, "a name")?;
                let bname = name(c, s, &mut names)?;
                let (cc, cs) = need(1, "a count (a number, or `pixels`)")?;
                let count = if cs == "pixels" {
                    Count::Pixels
                } else {
                    match cs.parse::<u32>() {
                        Ok(n) if n > 0 => Count::Fixed(n),
                        _ => {
                            return Err(at(
                                cc,
                                format!("`{cs}` is not a count (a number above 0, or `pixels`)"),
                            ));
                        }
                    }
                };
                let ty = match args.get(2) {
                    Some(&(tc, t)) => ElemType::parse(t).ok_or_else(|| {
                        at(
                            tc,
                            format!(
                                "`{t}` is not f32, u32, i32, vec2f, vec4f, atomic_u32 or atomic_i32"
                            ),
                        )
                    })?,
                    None => ElemType::F32,
                };
                if let Count::Fixed(n) = count {
                    let bytes = u64::from(n) * ty.bytes();
                    if bytes > max_buffer {
                        return Err(at(
                            cc,
                            format!(
                                "{bytes} bytes is over this GPU's largest storage buffer ({max_buffer} bytes)"
                            ),
                        ));
                    }
                }
                if plan
                    .decls
                    .iter()
                    .filter(|d| matches!(d, Decl::Buffer { .. }))
                    .count()
                    == MAX_BUFFERS
                {
                    return Err(at(c, format!("at most {MAX_BUFFERS} buffers")));
                }
                extra(3)?;
                Decl::Buffer {
                    name: bname,
                    count,
                    ty,
                }
            }
            other => {
                return Err(at(
                    kcol,
                    format!("unknown kind `{other}`; use input, output, state, param or buffer"),
                ));
            }
        };
        if matches!(decl, Decl::Output { .. } | Decl::State { .. })
            && plan.outputs() + plan.states() == MAX_WRITABLE
        {
            return Err(at(
                kcol,
                format!("at most {MAX_WRITABLE} output and state images in all"),
            ));
        }
        plan.decls.push(decl);
    }
    // A state loaded from an input binds that input too.
    let froms: Vec<usize> = plan
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::State { from: Some(s), .. } => Some(*s),
            _ => None,
        })
        .collect();
    for s in froms {
        if !plan
            .decls
            .iter()
            .any(|d| matches!(d, Decl::Input { socket } if *socket == s))
        {
            plan.decls.push(Decl::Input { socket: s });
        }
    }
    if plan.outputs() + plan.states() == 0 {
        return Err(
            "nothing to show: declare `#bind output <name>` or `#bind state <name>`".into(),
        );
    }
    Ok(plan)
}

/// Parses a tile menu value (`"16x8"`); unknown values give 8×8.
pub fn tile(s: &str) -> (u32, u32) {
    s.split_once('x')
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .filter(|&(x, y): &(u32, u32)| x > 0 && y > 0 && x * y <= 256)
        .unwrap_or((8, 8))
}

/// Whether a format can be read through a filtering sampler.
fn filterable(format: wgpu::TextureFormat) -> bool {
    !matches!(
        format,
        wgpu::TextureFormat::Rgba32Float | wgpu::TextureFormat::R32Float
    )
}

fn format_name(format: wgpu::TextureFormat) -> &'static str {
    FORMATS
        .iter()
        .find(|(_, f)| *f == format)
        .map_or("rgba16float", |(n, _)| n)
}

// -------------------------------------------------------------- codegen

/// The complete module, and the 1-based line of the first user line.
pub fn generate(plan: &Plan, source: &str, tile: (u32, u32)) -> (String, usize) {
    let mut s = String::from(
        "struct LsrParams { p: array<vec4f, 8>, time: f32, frame: u32, iteration: u32, iterations: u32, size: vec2u, pad: vec2u };\n\
         @group(0) @binding(0) var<uniform> lsr: LsrParams;\n\
         @group(0) @binding(1) var samp: sampler;\n",
    );
    s.push_str(&format!(
        "const TILE_X: u32 = {}u;\nconst TILE_Y: u32 = {}u;\n",
        tile.0, tile.1
    ));
    s.push_str(
        "fn time() -> f32 { return lsr.time; }\n\
         fn frame() -> u32 { return lsr.frame; }\n\
         fn iteration() -> u32 { return lsr.iteration; }\n\
         fn iterations() -> u32 { return lsr.iterations; }\n\
         fn size() -> vec2u { return lsr.size; }\n",
    );
    for i in 0..MAX_PARAMS {
        s.push_str(&format!(
            "fn p{}() -> vec4f {{ return lsr.p[{i}]; }}\n",
            i + 1
        ));
    }
    let mut binding = 2;
    for d in &plan.decls {
        match d {
            Decl::Input { socket } => {
                s.push_str(&format!(
                    "@group(0) @binding({binding}) var in{socket}: texture_2d<f32>;\n"
                ));
                binding += 1;
            }
            Decl::Output { name, format } => {
                s.push_str(&format!(
                    "@group(0) @binding({binding}) var {name}: texture_storage_2d<{}, write>;\n",
                    format_name(*format)
                ));
                binding += 1;
            }
            Decl::State { name, format, .. } => {
                s.push_str(&format!(
                    "@group(0) @binding({binding}) var {name}: texture_2d<f32>;\n\
                     @group(0) @binding({}) var {name}_out: texture_storage_2d<{}, write>;\n",
                    binding + 1,
                    format_name(*format)
                ));
                binding += 2;
            }
            Decl::Param { slot, name, ty } => {
                let (t, swizzle) = ty.wgsl();
                s.push_str(&format!(
                    "fn {name}() -> {t} {{ return lsr.p[{slot}]{swizzle}; }}\n"
                ));
            }
            Decl::Buffer { name, count, ty } => {
                let array = match count {
                    Count::Fixed(n) => format!("array<{}, {n}>", ty.wgsl()),
                    Count::Pixels => format!("array<{}>", ty.wgsl()),
                };
                s.push_str(&format!(
                    "@group(0) @binding({binding}) var<storage, read_write> {name}: {array};\n"
                ));
                binding += 1;
            }
        }
    }
    let first_user_line = s.lines().count() + 1;
    for line in source.lines() {
        // Directives become blank lines, so line numbers stay the user's.
        if !is_directive(line) {
            s.push_str(line);
        }
        s.push('\n');
    }
    s.push_str(
        "\n@compute @workgroup_size(TILE_X, TILE_Y, 1)\n\
         fn lsr_main(@builtin(global_invocation_id) lsr_id: vec3u) {\n\
         \x20   if (lsr_id.x >= lsr.size.x || lsr_id.y >= lsr.size.y) { return; }\n\
         \x20   kernel(lsr_id.xy);\n\
         }\n",
    );
    for d in &plan.decls {
        if let Decl::State {
            name,
            from: Some(socket),
            ..
        } = d
        {
            s.push_str(&format!(
                "\n@compute @workgroup_size(TILE_X, TILE_Y, 1)\n\
                 fn lsr_init_{name}(@builtin(global_invocation_id) lsr_id: vec3u) {{\n\
                 \x20   if (lsr_id.x >= lsr.size.x || lsr_id.y >= lsr.size.y) {{ return; }}\n\
                 \x20   let uv = (vec2f(lsr_id.xy) + 0.5) / vec2f(lsr.size);\n\
                 \x20   textureStore({name}_out, lsr_id.xy, textureSampleLevel(in{socket}, samp, uv, 0.0));\n\
                 }}\n"
            ));
        }
    }
    (s, first_user_line)
}

/// Validates the module with naga, naming the user's line and column.
fn validate(src: &str, first_user_line: usize, user_lines: usize) -> Result<(), String> {
    let place = |loc: Option<naga::SourceLocation>| match loc {
        Some(l)
            if (first_user_line..first_user_line + user_lines)
                .contains(&(l.line_number as usize)) =>
        {
            format!(
                "line {}, col {}: ",
                l.line_number as usize - first_user_line + 1,
                l.line_position
            )
        }
        _ => String::new(),
    };
    let module = naga::front::wgsl::parse_str(src).map_err(|e| {
        let loc = e
            .labels()
            .find(|(span, _)| span.to_range().is_some_and(|r| !r.is_empty()))
            .map(|(span, _)| span.location(src))
            .or_else(|| e.location(src));
        format!("{}{}", place(loc), e.message())
    })?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .map_err(|e| {
        // The innermost span (the expression, not the whole function) and
        // the whole chain of causes ("Function 'kernel' is invalid: ...").
        let loc = e
            .spans()
            .filter(|(span, _)| span.to_range().is_some_and(|r| !r.is_empty()))
            .last()
            .map(|(span, _)| span.location(src));
        let mut msg = e.as_inner().to_string();
        let mut cause = std::error::Error::source(e.as_inner());
        while let Some(c) = cause {
            msg.push_str(": ");
            msg.push_str(&c.to_string());
            cause = c.source();
        }
        format!("{}{msg}", place(loc))
    })?;
    Ok(())
}

/// Whether the source defines `fn kernel`.
fn defines_kernel(source: &str) -> bool {
    source.lines().filter(|l| !is_directive(l)).any(|l| {
        let mut t = l.split_whitespace();
        while let Some(w) = t.next() {
            if w == "fn" {
                return t
                    .next()
                    .is_some_and(|n| n == "kernel" || n.starts_with("kernel("));
            }
        }
        false
    })
}

// ------------------------------------------------------------- programs

/// A compiled shader: its plan, layout and pipelines.
pub struct Program {
    plan: Plan,
    layout: wgpu::BindGroupLayout,
    main: wgpu::ComputePipeline,
    /// Loads `from=` states: (state index among the states, pipeline).
    inits: Vec<(usize, wgpu::ComputePipeline)>,
    tile: (u32, u32),
    status: String,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("plan", &self.plan)
            .field("tile", &self.tile)
            .finish_non_exhaustive()
    }
}

type Built = Result<Program, String>;

/// Parses, validates and builds a program. Blocks while the driver
/// compiles: call it off the render thread.
pub fn compile(device: &wgpu::Device, source: &str, tile: (u32, u32)) -> Built {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(device, source, tile)))
        .unwrap_or_else(|_| Err("shader compilation panicked".into()))
}

fn build(device: &wgpu::Device, source: &str, tile: (u32, u32)) -> Built {
    let limits = device.limits();
    let plan = parse(source, limits.max_storage_buffer_binding_size)?;
    if !defines_kernel(source) {
        return Err("define `fn kernel(id: vec2u)`: it runs once per output pixel".into());
    }
    if tile.0 * tile.1 > limits.max_compute_invocations_per_workgroup
        || tile.0 > limits.max_compute_workgroup_size_x
        || tile.1 > limits.max_compute_workgroup_size_y
    {
        return Err(format!(
            "tile {}x{} is over this GPU's workgroup limits",
            tile.0, tile.1
        ));
    }
    let (src, first) = generate(&plan, source, tile);
    validate(&src, first, source.lines().count())?;

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let compute = wgpu::ShaderStages::COMPUTE;
    let mut entries = vec![
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: compute,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: compute,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
    ];
    let texture = |filterable| wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    };
    let storage = |format| wgpu::BindingType::StorageTexture {
        access: wgpu::StorageTextureAccess::WriteOnly,
        format,
        view_dimension: wgpu::TextureViewDimension::D2,
    };
    let mut push = |ty| {
        let binding = entries.len() as u32;
        entries.push(wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty,
            count: None,
        });
    };
    for d in &plan.decls {
        match d {
            Decl::Input { .. } => push(texture(true)),
            Decl::Output { format, .. } => push(storage(*format)),
            Decl::State { format, .. } => {
                push(texture(filterable(*format)));
                push(storage(*format));
            }
            Decl::Buffer { .. } => push(wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            }),
            Decl::Param { .. } => {}
        }
    }
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("img.compute"),
        entries: &entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("img.compute"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("img.compute"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let pipeline = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("img.compute"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let main = pipeline("lsr_main");
    let mut inits = Vec::new();
    let mut state = 0;
    for d in &plan.decls {
        if let Decl::State { name, from, .. } = d {
            if from.is_some() {
                inits.push((state, pipeline(&format!("lsr_init_{name}"))));
            }
            state += 1;
        }
    }
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(e.to_string());
    }
    let status = plan.status();
    Ok(Program {
        plan,
        layout,
        main,
        inits,
        tile,
        status,
    })
}

// ------------------------------------------------------------ resources

fn image(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("img.compute"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            // For clearing on Reset.
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Bind groups kept per set of input textures (pooled inputs cycle among a
/// few), so steady-state frames create none (rule 2).
const CACHE: usize = 8;

/// The images and buffers of a plan at one output size.
struct Resources {
    size: (u32, u32),
    resources: Vec<Resource>,
    outputs: Vec<(wgpu::Texture, wgpu::TextureView)>,
    /// Each state's pair; `parity` selects the one read next.
    states: Vec<[(wgpu::Texture, wgpu::TextureView); 2]>,
    buffers: Vec<wgpu::Buffer>,
    parity: usize,
    /// (input textures, bind groups reading pair 0 and pair 1, last use).
    binds: Vec<(Vec<wgpu::Texture>, [wgpu::BindGroup; 2], u64)>,
    uses: u64,
}

impl Resources {
    fn new(device: &wgpu::Device, plan: &Plan, w: u32, h: u32) -> Result<Self, String> {
        let limit = device.limits().max_storage_buffer_binding_size;
        let mut outputs = Vec::new();
        let mut states = Vec::new();
        let mut buffers = Vec::new();
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        for d in &plan.decls {
            match d {
                Decl::Output { format, .. } => {
                    let t = image(device, w, h, *format);
                    let v = view(&t);
                    outputs.push((t, v));
                }
                Decl::State { format, .. } => {
                    let pair = [(); 2].map(|()| {
                        let t = image(device, w, h, *format);
                        let v = view(&t);
                        (t, v)
                    });
                    states.push(pair);
                }
                Decl::Buffer { name, count, ty } => {
                    let n = match count {
                        Count::Fixed(n) => u64::from(*n),
                        Count::Pixels => u64::from(w) * u64::from(h),
                    };
                    let bytes = (n * ty.bytes()).max(4);
                    if bytes > limit {
                        return Err(format!(
                            "buffer `{name}` needs {bytes} bytes at {w}×{h}, over this GPU's largest storage buffer ({limit} bytes)"
                        ));
                    }
                    buffers.push(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("img.compute"),
                        size: bytes,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
                }
                _ => {}
            }
        }
        Ok(Self {
            size: (w, h),
            resources: plan.resources(),
            outputs,
            states,
            buffers,
            parity: 0,
            binds: Vec::new(),
            uses: 0,
        })
    }

    /// Zeroes the persistent state.
    fn clear(&mut self, encoder: &mut wgpu::CommandEncoder) {
        for pair in &self.states {
            for (_, v) in pair {
                clear_view(encoder, v);
            }
        }
        for b in &self.buffers {
            encoder.clear_buffer(b, 0, None);
        }
        self.parity = 0;
    }

    /// The bind groups for these inputs (built on first use).
    fn binds(
        &mut self,
        device: &wgpu::Device,
        program: &Program,
        uniforms: &wgpu::Buffer,
        sampler: &wgpu::Sampler,
        inputs: &[&wgpu::Texture; MAX_INPUTS],
    ) -> &[wgpu::BindGroup; 2] {
        self.uses += 1;
        let now = self.uses;
        self.binds.retain(|e| now - e.2 <= 2 * CACHE as u64);
        if let Some(i) = self
            .binds
            .iter()
            .position(|(key, _, _)| key.iter().eq(inputs.iter().copied()))
        {
            let mut hit = self.binds.remove(i);
            hit.2 = now;
            self.binds.push(hit);
            return &self.binds.last().expect("pushed").1;
        }
        let views: Vec<wgpu::TextureView> = inputs
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let group = |q: usize| {
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: uniforms,
                        offset: 0,
                        size: wgpu::BufferSize::new(UNIFORM_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ];
            let (mut out, mut state, mut buffer) = (0, 0, 0);
            for d in &program.plan.decls {
                let next = entries.len() as u32;
                match d {
                    Decl::Input { socket } => entries.push(wgpu::BindGroupEntry {
                        binding: next,
                        resource: wgpu::BindingResource::TextureView(&views[*socket]),
                    }),
                    Decl::Output { .. } => {
                        entries.push(wgpu::BindGroupEntry {
                            binding: next,
                            resource: wgpu::BindingResource::TextureView(&self.outputs[out].1),
                        });
                        out += 1;
                    }
                    Decl::State { .. } => {
                        let pair = &self.states[state];
                        entries.push(wgpu::BindGroupEntry {
                            binding: next,
                            resource: wgpu::BindingResource::TextureView(&pair[q].1),
                        });
                        entries.push(wgpu::BindGroupEntry {
                            binding: next + 1,
                            resource: wgpu::BindingResource::TextureView(&pair[1 - q].1),
                        });
                        state += 1;
                    }
                    Decl::Buffer { .. } => {
                        entries.push(wgpu::BindGroupEntry {
                            binding: next,
                            resource: self.buffers[buffer].as_entire_binding(),
                        });
                        buffer += 1;
                    }
                    Decl::Param { .. } => {}
                }
            }
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.compute"),
                layout: &program.layout,
                entries: &entries,
            })
        };
        let groups = [group(0), group(1)];
        if self.binds.len() == CACHE {
            self.binds.remove(0);
        }
        let key = inputs.iter().map(|t| (*t).clone()).collect();
        self.binds.push((key, groups, now));
        &self.binds.last().expect("pushed").1
    }

    /// The image the node shows.
    fn shown(&self) -> &wgpu::Texture {
        match self.outputs.first() {
            Some((t, _)) => t,
            None => &self.states[0][self.parity].0,
        }
    }
}

// ----------------------------------------------------------------- blit

const BLIT: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    return textureLoad(src, vec2i(pos.xy), 0);
}
"#;

/// Copies a storage image of any guaranteed format, texel for texel, into
/// the engine's output (whose format has no storage use).
struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// (source, bind group); sources are the operator's own images.
    sources: Vec<(wgpu::Texture, wgpu::BindGroup)>,
    /// (target, view, last use); pooled outputs cycle among a few.
    targets: Vec<(wgpu::Texture, wgpu::TextureView, u64)>,
    uses: u64,
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img.compute.blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img.compute.blit"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img.compute.blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("img.compute.blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            sources: Vec::new(),
            targets: Vec::new(),
            uses: 0,
        }
    }

    fn draw(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::Texture,
        dst: &wgpu::Texture,
    ) {
        self.uses += 1;
        let now = self.uses;
        if !self.sources.iter().any(|(t, _)| t == src) {
            let view = src.create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("img.compute.blit"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.sources.push((src.clone(), bind));
        }
        self.targets.retain(|e| now - e.2 <= 2 * CACHE as u64);
        let target = match self.targets.iter().position(|(t, _, _)| t == dst) {
            Some(i) => {
                self.targets[i].2 = now;
                i
            }
            None => {
                if self.targets.len() == CACHE {
                    self.targets.remove(0);
                }
                let view = dst.create_view(&Default::default());
                self.targets.push((dst.clone(), view, now));
                self.targets.len() - 1
            }
        };
        let bind = &self
            .sources
            .iter()
            .find(|(t, _)| t == src)
            .expect("added above")
            .1;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img.compute.blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets[target].1,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

// ------------------------------------------------------------- operator

pub struct Compute {
    device: wgpu::Device,
    uniforms: wgpu::Buffer,
    /// Uniform bytes for every dispatch of a cook, allocated once.
    bytes: Vec<u8>,
    sampler: wgpu::Sampler,
    blit: Blit,
    /// The last program that compiled.
    program: Option<Program>,
    error: Option<String>,
    /// Inline code (and tile) compiled or being compiled, and the result.
    code: Option<(String, (u32, u32))>,
    building: Option<Receiver<Built>>,
    watcher: Option<(Watcher<Program>, (u32, u32))>,
    resources: Option<Resources>,
    last_reset: Option<bool>,
}

impl std::fmt::Debug for Compute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compute")
            .field("program", &self.program)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

pub fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Compute {
        device: device.clone(),
        uniforms: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img.compute"),
            size: u64::from(MAX_ITERATIONS) * UNIFORM_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        bytes: vec![0; (u64::from(MAX_ITERATIONS) * UNIFORM_STRIDE) as usize],
        sampler: device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img.compute"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
        blit: Blit::new(device, format),
        program: None,
        error: None,
        code: None,
        building: None,
        watcher: None,
        resources: None,
        last_reset: None,
    })
}

impl Compute {
    fn take(&mut self, built: Built) {
        match built {
            Ok(program) => {
                // Same images and buffers: keep them (and their state);
                // the bind groups belong to the old layout.
                match &mut self.resources {
                    Some(r) if r.resources == program.plan.resources() => r.binds.clear(),
                    _ => self.resources = None,
                }
                self.blit.sources.clear();
                self.program = Some(program);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl GpuOp for Compute {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        let tile = tile(p.str("tile"));
        let file = p.str("file");
        if file.is_empty() {
            self.watcher = None;
            let code = p.str("code");
            if self
                .code
                .as_ref()
                .is_none_or(|(c, t)| c != code || *t != tile)
            {
                let (tx, rx) = channel();
                let (device, body) = (self.device.clone(), code.to_owned());
                std::thread::Builder::new()
                    .name("lsr-compute-compile".into())
                    .spawn(move || {
                        let _ = tx.send(compile(&device, &body, tile));
                    })
                    .map_err(|e| format!("cannot start compiler thread: {e}"))?;
                self.code = Some((code.to_owned(), tile));
                self.building = Some(rx);
            }
        } else {
            self.code = None;
            self.building = None;
            if self
                .watcher
                .as_ref()
                .is_none_or(|(w, t)| w.path != file || *t != tile)
            {
                let device = self.device.clone();
                self.watcher = Some((
                    watch(
                        file.to_owned(),
                        p.resolve(file),
                        "lsr-compute-watch",
                        move |text| compile(&device, text, tile),
                    ),
                    tile,
                ));
            }
        }

        // Offline the cook waits for the compiler (FR-H1).
        let mode = p.mode();
        if let Some(rx) = &self.building {
            match mode.receive(rx) {
                Ok(built) => {
                    self.building = None;
                    self.take(built);
                }
                Err(TryRecvError::Empty) => ctx.cook_again = true,
                Err(TryRecvError::Disconnected) => {
                    self.building = None;
                    self.error = Some("shader compiler stopped".into());
                }
            }
        }
        if let Some((w, _)) = &self.watcher {
            if mode.is_offline() {
                // Offline the file is compiled once, as it was when the
                // render started; later saves are not picked up.
                if self.program.is_none() && self.error.is_none() {
                    let built = mode
                        .receive(&w.rx)
                        .unwrap_or_else(|_| Err("shader file watcher stopped".into()));
                    self.take(built);
                }
            } else {
                let mut latest = None;
                while let Ok(built) = w.rx.try_recv() {
                    latest = Some(built);
                }
                if let Some(built) = latest {
                    self.take(built);
                }
                // Keep looking for saves.
                ctx.cook_again = true;
            }
        }

        let Some(program) = &self.program else {
            clear(ctx.encoder, ctx.output);
            return self.error.clone().map_or(Ok(()), Err);
        };
        let out = ctx.output;
        let (w, h) = (out.width(), out.height());
        let reset = p.bool("reset");
        let pulsed = self.last_reset.is_some_and(|r| r != reset);
        self.last_reset = Some(reset);
        let fits = self.resources.as_ref().is_some_and(|r| r.size == (w, h));
        if !fits {
            match Resources::new(ctx.device, &program.plan, w, h) {
                Ok(r) => self.resources = Some(r),
                Err(e) => {
                    self.resources = None;
                    clear(ctx.encoder, out);
                    return Err(e);
                }
            }
            self.blit.sources.clear();
        }
        let res = self.resources.as_mut().expect("created above");
        if pulsed {
            res.clear(ctx.encoder);
        }

        // One uniform block per dispatch.
        let n = p.i64("iterations").clamp(1, MAX_ITERATIONS.into()) as u32;
        let mut block = [0u8; UNIFORM_BYTES as usize];
        for (i, name) in PARAM_NAMES.iter().enumerate() {
            for (k, f) in p.vec4(name).iter().enumerate() {
                let at = (i * 4 + k) * 4;
                block[at..at + 4].copy_from_slice(&f.to_le_bytes());
            }
        }
        let tail: [u32; 8] = [
            (ctx.time as f32).to_bits(),
            ctx.frame as u32,
            0,
            n,
            w,
            h,
            0,
            0,
        ];
        for (k, v) in tail.iter().enumerate() {
            let at = 128 + k * 4;
            block[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        for i in 0..n as usize {
            block[136..140].copy_from_slice(&(i as u32).to_le_bytes());
            let at = i * UNIFORM_STRIDE as usize;
            self.bytes[at..at + UNIFORM_BYTES as usize].copy_from_slice(&block);
        }
        ctx.queue.write_buffer(
            &self.uniforms,
            0,
            &self.bytes[..n as usize * UNIFORM_STRIDE as usize],
        );

        let mut inputs = [ctx.black; MAX_INPUTS];
        for (r, input) in inputs.iter_mut().zip(ctx.inputs) {
            if let Some(t) = input {
                *r = t;
            }
        }
        let has_states = program.plan.states() > 0;
        let mut parity = res.parity;
        {
            let binds = res.binds(ctx.device, program, &self.uniforms, &self.sampler, &inputs);
            let groups = (
                w.div_ceil(program.tile.0).min(65535),
                h.div_ceil(program.tile.1).min(65535),
            );
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("img.compute"),
                    timestamp_writes: None,
                });
            // `from=` states: write the image read next.
            for (_, pipeline) in &program.inits {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &binds[1 - parity], &[0]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            pass.set_pipeline(&program.main);
            for i in 0..n {
                pass.set_bind_group(0, &binds[parity], &[i * UNIFORM_STRIDE as u32]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
                if has_states {
                    parity = 1 - parity;
                }
            }
        }
        res.parity = parity;
        self.blit.draw(ctx.device, ctx.encoder, res.shown(), out);
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn status(&self) -> Option<&str> {
        self.program
            .as_ref()
            .map(|p| p.status.as_str())
            .filter(|s| !s.is_empty())
    }
}

const PARAM_NAMES: [&str; MAX_PARAMS] = ["p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8"];

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: u64 = 128 << 20;

    #[test]
    fn directives_parse_into_a_plan() {
        let src = "#bind input in1\n#bind state heat r32float from=in0\n  #bind param p2 speed f32\n#bind buffer hist 256 atomic_u32\n#bind output dst rgba8unorm\nfn kernel(id: vec2u) {}\n";
        let plan = parse(src, BIG).unwrap();
        assert_eq!(
            plan.decls,
            vec![
                Decl::Input { socket: 1 },
                Decl::State {
                    name: "heat".into(),
                    format: wgpu::TextureFormat::R32Float,
                    from: Some(0)
                },
                Decl::Param {
                    slot: 1,
                    name: "speed".into(),
                    ty: ParamType::F32
                },
                Decl::Buffer {
                    name: "hist".into(),
                    count: Count::Fixed(256),
                    ty: ElemType::AtomicU32
                },
                Decl::Output {
                    name: "dst".into(),
                    format: wgpu::TextureFormat::Rgba8Unorm
                },
                // Added for `from=in0`.
                Decl::Input { socket: 0 },
            ]
        );
        assert_eq!(plan.status(), "P2 = speed: f32");
        // Generated code keeps the user's line numbers.
        let (wgsl, first) = generate(&plan, src, (16, 16));
        let lines: Vec<&str> = wgsl.lines().collect();
        assert_eq!(lines[first - 1 + 5], "fn kernel(id: vec2u) {}");
        assert!(lines[first - 1].is_empty(), "directives become blank lines");
        assert!(wgsl.contains("const TILE_X: u32 = 16u;"));
        assert!(wgsl.contains("fn speed() -> f32 { return lsr.p[1].x; }"));
        assert!(wgsl.contains("var heat_out: texture_storage_2d<r32float, write>;"));
        assert!(wgsl.contains("fn lsr_init_heat("));
        validate(&wgsl, first, src.lines().count()).unwrap();
    }

    #[test]
    fn directive_errors_name_line_and_column() {
        let err = |src: &str| parse(src, BIG).unwrap_err();
        assert_eq!(
            err("fn kernel(id: vec2u) {}\n#bind outptu dst"),
            "line 2, col 7: unknown kind `outptu`; use input, output, state, param or buffer"
        );
        assert_eq!(
            err("#bind input in7"),
            "line 1, col 13: `in7` is not an input socket (in0 to in3)"
        );
        assert_eq!(
            err("#bind output dst bgra8unorm"),
            "line 1, col 18: `bgra8unorm` is not a storage format; use one of rgba8unorm, rgba16float, rgba32float, r32float"
        );
        assert_eq!(
            err("#bind output time"),
            "line 1, col 14: `time` is reserved"
        );
        assert_eq!(
            err("#bind output a\n#bind state a"),
            "line 2, col 13: `a` is declared twice"
        );
        assert_eq!(
            err("#tile 8"),
            "line 1, col 1: unknown directive `#tile`; the only directive is `#bind`"
        );
        assert_eq!(
            err("#bind output dst\n#bind buffer b 99999999999"),
            "line 2, col 16: `99999999999` is not a count (a number above 0, or `pixels`)"
        );
        assert_eq!(
            err("#bind output dst\n#bind buffer b 40000000 vec4f"),
            "line 2, col 16: 640000000 bytes is over this GPU's largest storage buffer (134217728 bytes)"
        );
        assert!(err("#bind input in0").starts_with("nothing to show"));
        let five = "#bind output a\n#bind state b\n#bind state c\n#bind state d\n#bind state e";
        assert_eq!(
            err(five),
            "line 5, col 7: at most 4 output and state images in all"
        );
    }

    #[test]
    fn wgsl_errors_name_the_users_line_and_column() {
        let src = "#bind output dst\n\nfn kernel(id: vec2u) {\n    textureStore(dst, id, vec4f(1.0)) oops;\n}\n";
        let plan = parse(src, BIG).unwrap();
        let (wgsl, first) = generate(&plan, src, (8, 8));
        let e = validate(&wgsl, first, src.lines().count()).unwrap_err();
        assert!(e.starts_with("line 4, col 39:"), "{e}");
        // A type error found by validation.
        let src = "#bind output dst\nfn kernel(id: vec2u) {\n    let x: f32 = 1u;\n}\n";
        let plan = parse(src, BIG).unwrap();
        let (wgsl, first) = generate(&plan, src, (8, 8));
        let e = validate(&wgsl, first, src.lines().count()).unwrap_err();
        assert!(e.starts_with("line 3, col "), "{e}");
        // One found by the validator names the expression, not the function.
        let src =
            "#bind output dst\nfn kernel(id: vec2u) {\n\n    textureStore(dst, id, 1.0);\n}\n";
        let plan = parse(src, BIG).unwrap();
        let (wgsl, first) = generate(&plan, src, (8, 8));
        let e = validate(&wgsl, first, src.lines().count()).unwrap_err();
        assert!(e.starts_with("line 4, col "), "{e}");
        assert!(!defines_kernel("#bind output dst\nfn main() {}"));
        assert!(defines_kernel("fn kernel(id: vec2u) {}"));
        assert!(defines_kernel("fn  kernel (id: vec2u) {}"));
    }

    /// The limits the operator relies on hold on every adapter wgpu
    /// supports (its downlevel defaults), so DX12 and Metal both have them.
    #[test]
    fn limits_are_within_wgpus_guarantees() {
        let low = wgpu::Limits::downlevel_defaults();
        for t in TILES {
            let (x, y) = tile(t);
            assert_eq!(format!("{x}x{y}"), *t);
            assert!(x * y <= low.max_compute_invocations_per_workgroup, "{t}");
            assert!(x <= low.max_compute_workgroup_size_x, "{t}");
            assert!(y <= low.max_compute_workgroup_size_y, "{t}");
        }
        assert!(MAX_WRITABLE as u32 <= low.max_storage_textures_per_shader_stage);
        assert!(MAX_BUFFERS as u32 <= low.max_storage_buffers_per_shader_stage);
        assert!(UNIFORM_BYTES <= low.max_uniform_buffer_binding_size);
        assert_eq!(
            UNIFORM_STRIDE % u64::from(low.min_uniform_buffer_offset_alignment),
            0
        );
        assert!(
            MAX_INPUTS as u32 + 2 * MAX_WRITABLE as u32
                <= low.max_sampled_textures_per_shader_stage
        );
        assert!(1 <= low.max_dynamic_uniform_buffers_per_pipeline_layout);
        // Every format a #bind may name has guaranteed write-only storage
        // and can be cleared (rendered to) and read back, without features.
        for (name, f) in FORMATS {
            let g = f.guaranteed_format_features(wgpu::Features::empty());
            assert!(
                g.allowed_usages.contains(
                    wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                ),
                "{name}: {:?}",
                g.allowed_usages
            );
            assert!(
                g.flags
                    .contains(wgpu::TextureFormatFeatureFlags::STORAGE_WRITE_ONLY),
                "{name}"
            );
            assert_eq!(
                filterable(*f),
                g.flags
                    .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE),
                "{name}"
            );
        }
    }
}
