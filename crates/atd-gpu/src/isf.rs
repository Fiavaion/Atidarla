//! ISF import (FR-K6): Interactive Shader Format shaders → WGSL.
//!
//! An ISF shader is GLSL with a JSON header in its first comment. Import
//! has three steps:
//!
//! 1. [`parse_header`] reads the header into [`IsfHeader`]: inputs become
//!    operator parameters and image inputs, and passes become render targets.
//! 2. [`translate`] wraps the GLSL in a generated prelude. The prelude
//!    declares the uniform block, textures and varyings and maps the ISF
//!    built-ins (`IMG_NORM_PIXEL`, `gl_FragColor`, `isf_FragNormCoord`, …).
//!    naga then converts both stages to WGSL.
//! 3. The caller runs the result with [`IsfShader::uniforms`], which gives
//!    the byte offset of every uniform, and [`IsfShader::textures`], which
//!    gives the texture binding order.
//!
//! **Coordinates.** ISF uses OpenGL's bottom-left origin. The vertex stage
//! keeps that convention for `isf_FragNormCoord`. The prelude flips the y
//! coordinate when it samples images (stored top row first) and when it
//! reads `gl_FragCoord`.
//!
//! Failures are reported per shader and by stage, with a line number in the
//! shader's own source.

use std::fmt;

use serde_json::Value;

/// Bind group layout of a translated shader: uniforms, sampler, textures.
pub const UNIFORM_BINDING: u32 = 0;
pub const SAMPLER_BINDING: u32 = 1;
pub const FIRST_TEXTURE_BINDING: u32 = 2;

/// Kind and range of one ISF input.
#[derive(Debug, Clone, PartialEq)]
pub enum IsfInputKind {
    Float {
        min: f32,
        max: f32,
        default: f32,
    },
    /// ISF `long`: an integer, usually a menu (`values` with `labels`).
    Long {
        min: i32,
        max: i32,
        default: i32,
        values: Vec<i32>,
        labels: Vec<String>,
    },
    Bool {
        default: bool,
    },
    /// A momentary trigger, true for one frame.
    Event,
    Color {
        default: [f32; 4],
    },
    Point2D {
        min: [f32; 2],
        max: [f32; 2],
        default: [f32; 2],
    },
    Image,
    /// Audio waveform as an image (one row per channel).
    Audio {
        max: Option<u32>,
    },
    /// Audio spectrum as an image.
    AudioFft {
        max: Option<u32>,
    },
}

impl IsfInputKind {
    /// True for inputs that are textures rather than uniforms.
    pub fn is_texture(&self) -> bool {
        matches!(
            self,
            Self::Image | Self::Audio { .. } | Self::AudioFft { .. }
        )
    }
}

/// One entry of the header's `INPUTS`.
#[derive(Debug, Clone, PartialEq)]
pub struct IsfInput {
    pub name: String,
    pub label: Option<String>,
    pub kind: IsfInputKind,
}

/// One entry of the header's `PASSES`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IsfPass {
    /// Name of the target buffer; `None` renders to the output.
    pub target: Option<String>,
    /// Keep the buffer between frames (feedback).
    pub persistent: bool,
    /// Use a float buffer.
    pub float: bool,
    /// Size expressions such as `floor($WIDTH/4.0)`; `None` is full size.
    pub width: Option<String>,
    pub height: Option<String>,
}

/// The parsed JSON header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IsfHeader {
    pub description: Option<String>,
    pub credit: Option<String>,
    pub categories: Vec<String>,
    pub inputs: Vec<IsfInput>,
    /// Always at least one pass.
    pub passes: Vec<IsfPass>,
    /// Images loaded from files next to the shader (`IMPORTED`), by name.
    pub imported: Vec<(String, String)>,
}

impl IsfHeader {
    /// An ISF filter has an image input named `inputImage`; otherwise it is
    /// a generator.
    pub fn is_filter(&self) -> bool {
        self.inputs
            .iter()
            .any(|i| i.name == "inputImage" && i.kind.is_texture())
    }
}

/// Where in the pipeline a failure happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsfStage {
    Header,
    Vertex,
    Fragment,
    Validate,
}

impl fmt::Display for IsfStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Header => "header",
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
            Self::Validate => "validate",
        })
    }
}

/// An import failure: stage, line in the user's source (if known), message.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{stage}{}: {message}", line.map(|l| format!(" line {l}")).unwrap_or_default())]
pub struct IsfError {
    pub stage: IsfStage,
    pub line: Option<u32>,
    pub message: String,
}

impl IsfError {
    fn header(message: impl Into<String>) -> Self {
        Self {
            stage: IsfStage::Header,
            line: None,
            message: message.into(),
        }
    }
}

/// Scalar type of a uniform member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniformType {
    Float,
    Int,
    Vec2,
    Vec4,
}

impl UniformType {
    fn glsl(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Int => "int",
            Self::Vec2 => "vec2",
            Self::Vec4 => "vec4",
        }
    }

    /// Size in bytes.
    pub fn size(self) -> u32 {
        match self {
            Self::Float | Self::Int => 4,
            Self::Vec2 => 8,
            Self::Vec4 => 16,
        }
    }
}

/// One member of the uniform block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniformField {
    /// The ISF name: a built-in (`TIME`, `RENDERSIZE`, …) or an input name.
    pub name: String,
    pub ty: UniformType,
    pub offset: u32,
}

/// Layout of the uniform block, read back from the translated module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniformLayout {
    pub size: u32,
    pub fields: Vec<UniformField>,
}

impl UniformLayout {
    pub fn field(&self, name: &str) -> Option<&UniformField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// What a texture binding holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureSource {
    /// Input `index` of `IsfHeader::inputs`.
    Input(usize),
    /// Imported image `index` of `IsfHeader::imported`.
    Imported(usize),
    /// Pass target `name`.
    Target(String),
}

/// A translated ISF shader, ready to build a pipeline from.
#[derive(Debug, Clone)]
pub struct IsfShader {
    pub header: IsfHeader,
    /// WGSL with entry point `main`, vertex stage. It draws a 6-vertex quad.
    pub vertex_wgsl: String,
    /// WGSL with entry point `main`, fragment stage.
    pub fragment_wgsl: String,
    pub uniforms: UniformLayout,
    /// Texture bindings from [`FIRST_TEXTURE_BINDING`], in order.
    pub textures: Vec<TextureSource>,
}

/// Number of vertices to draw.
pub const VERTEX_COUNT: u32 = 6;

/// Splits an ISF source into its JSON header and the GLSL after it.
pub fn split_source(src: &str) -> Result<(&str, &str), IsfError> {
    let start = src
        .find("/*")
        .ok_or_else(|| IsfError::header("no /* JSON header */ comment"))?;
    let end = src[start..]
        .find("*/")
        .ok_or_else(|| IsfError::header("unterminated header comment"))?
        + start;
    Ok((&src[start + 2..end], &src[end + 2..]))
}

/// Parses the JSON header of an ISF source.
pub fn parse_header(src: &str) -> Result<IsfHeader, IsfError> {
    let (json, _) = split_source(src)?;
    let value: Value = serde_json::from_str(json.trim())
        .or_else(|_| serde_json::from_str(&strip_trailing_commas(json)))
        .map_err(|e| IsfError::header(format!("JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| IsfError::header("header is not an object"))?;
    let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);

    let mut header = IsfHeader {
        description: text("DESCRIPTION"),
        credit: text("CREDIT"),
        categories: obj
            .get("CATEGORIES")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };

    for input in obj
        .get("INPUTS")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = input
            .get("NAME")
            .and_then(Value::as_str)
            .ok_or_else(|| IsfError::header("input without NAME"))?;
        if !is_identifier(name) {
            return Err(IsfError::header(format!(
                "input name {name:?} is not an identifier"
            )));
        }
        let ty = input.get("TYPE").and_then(Value::as_str).unwrap_or("");
        let num = |k: &str, d: f32| input.get(k).and_then(as_f32).unwrap_or(d);
        let vec = |k: &str, d: &[f32]| -> Vec<f32> {
            match input.get(k).and_then(Value::as_array) {
                Some(a) => d
                    .iter()
                    .enumerate()
                    .map(|(i, d)| a.get(i).and_then(as_f32).unwrap_or(*d))
                    .collect(),
                None => d.to_vec(),
            }
        };
        let kind = match ty {
            "float" => IsfInputKind::Float {
                min: num("MIN", 0.0),
                max: num("MAX", 1.0),
                default: num("DEFAULT", 0.0),
            },
            "long" => {
                let ints = |k: &str| -> Vec<i32> {
                    input
                        .get(k)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(as_f32).map(|v| v as i32).collect())
                        .unwrap_or_default()
                };
                let values = ints("VALUES");
                let labels: Vec<String> = input
                    .get("LABELS")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_str().unwrap_or("").to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let (lo, hi) = match (values.iter().min(), values.iter().max()) {
                    (Some(lo), Some(hi)) => (*lo, *hi),
                    _ => (num("MIN", 0.0) as i32, num("MAX", 10.0) as i32),
                };
                IsfInputKind::Long {
                    min: lo,
                    max: hi,
                    default: num("DEFAULT", lo as f32) as i32,
                    values,
                    labels,
                }
            }
            "bool" => IsfInputKind::Bool {
                default: input.get("DEFAULT").map(truthy).unwrap_or(false),
            },
            "event" => IsfInputKind::Event,
            "color" => {
                let v = vec("DEFAULT", &[0.0, 0.0, 0.0, 1.0]);
                IsfInputKind::Color {
                    default: [v[0], v[1], v[2], v[3]],
                }
            }
            "point2D" => {
                let d = vec("DEFAULT", &[0.0, 0.0]);
                let lo = vec("MIN", &[0.0, 0.0]);
                let hi = vec("MAX", &[1.0, 1.0]);
                IsfInputKind::Point2D {
                    min: [lo[0], lo[1]],
                    max: [hi[0], hi[1]],
                    default: [d[0], d[1]],
                }
            }
            "image" => IsfInputKind::Image,
            "audio" => IsfInputKind::Audio {
                max: input.get("MAX").and_then(as_f32).map(|v| v as u32),
            },
            "audioFFT" => IsfInputKind::AudioFft {
                max: input.get("MAX").and_then(as_f32).map(|v| v as u32),
            },
            other => {
                return Err(IsfError::header(format!(
                    "input {name}: unknown TYPE {other:?}"
                )));
            }
        };
        header.inputs.push(IsfInput {
            name: name.to_owned(),
            label: input
                .get("LABEL")
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind,
        });
    }

    for pass in obj
        .get("PASSES")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let size = |k: &str| match pass.get(k) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let target = pass
            .get("TARGET")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if let Some(t) = &target
            && !is_identifier(t)
        {
            return Err(IsfError::header(format!(
                "pass target {t:?} is not an identifier"
            )));
        }
        header.passes.push(IsfPass {
            target,
            persistent: pass.get("PERSISTENT").map(truthy).unwrap_or(false),
            float: pass.get("FLOAT").map(truthy).unwrap_or(false),
            width: size("WIDTH"),
            height: size("HEIGHT"),
        });
    }
    if header.passes.is_empty() {
        header.passes.push(IsfPass::default());
    }

    match obj.get("IMPORTED") {
        Some(Value::Object(m)) => {
            for (name, v) in m {
                let path = v.get("PATH").and_then(Value::as_str).unwrap_or("");
                header.imported.push((name.clone(), path.to_owned()));
            }
        }
        Some(Value::Array(a)) => {
            for v in a {
                let name = v.get("NAME").and_then(Value::as_str).unwrap_or("");
                let path = v.get("PATH").and_then(Value::as_str).unwrap_or("");
                header.imported.push((name.to_owned(), path.to_owned()));
            }
        }
        _ => {}
    }
    Ok(header)
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::Bool(b) => Some(f32::from(u8::from(*b))),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(s.trim(), "true" | "1" | "TRUE" | "YES" | "yes"),
        _ => false,
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Removes `,` before `}` or `]` (common in hand-written headers).
fn strip_trailing_commas(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    let bytes: Vec<char> = json.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if in_string {
            escaped = c == '\\' && !escaped;
            if c == '"' && !escaped {
                in_string = false;
            }
            out.push(c);
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Built-in uniforms every shader gets, in block order.
const BUILTINS: [(&str, UniformType); 6] = [
    ("DATE", UniformType::Vec4),
    ("RENDERSIZE", UniformType::Vec2),
    ("TIME", UniformType::Float),
    ("TIMEDELTA", UniformType::Float),
    ("PASSINDEX", UniformType::Int),
    ("FRAMEINDEX", UniformType::Int),
];

/// GLSL member name of a uniform: bools and events are stored as ints and
/// exposed to the shader as a `bool` global of the ISF name.
fn member_name(input: &IsfInput) -> String {
    match input.kind {
        IsfInputKind::Bool { .. } | IsfInputKind::Event => format!("atd_b_{}", input.name),
        _ => input.name.clone(),
    }
}

fn uniform_type(kind: &IsfInputKind) -> Option<UniformType> {
    Some(match kind {
        IsfInputKind::Float { .. } => UniformType::Float,
        IsfInputKind::Long { .. } | IsfInputKind::Bool { .. } | IsfInputKind::Event => {
            UniformType::Int
        }
        IsfInputKind::Color { .. } => UniformType::Vec4,
        IsfInputKind::Point2D { .. } => UniformType::Vec2,
        _ => return None,
    })
}

/// A `varying` declaration found in the user's source.
#[derive(Debug, Clone)]
struct Varying {
    ty: String,
    name: String,
    /// Array length, if declared as an array.
    len: Option<u32>,
}

/// Removes `varying` declarations (they become located in/out globals) and
/// statements naga does not accept (`precision`, `#version`, `#extension`).
///
/// ISF sources often branch on `__VERSION__ <= 120` between `varying` and
/// `in`/`out`; `__VERSION__` reads as 120 so the `varying` branch is taken.
///
/// Stray `uniform` declarations (ISF v1 habits) are dropped when they name an
/// input or built-in, and otherwise become plain zero-initialised globals.
fn scrub(src: &str, varyings: &mut Vec<Varying>, known: &[&str]) -> String {
    let mut out = String::with_capacity(src.len());
    let src = src.replace("__VERSION__", "120");
    for line in src.lines() {
        let t = line.trim_start();
        let directive = t.starts_with("#version") || t.starts_with("#extension");
        let precision = t.starts_with("precision ") && t.trim_end().ends_with(';');
        if directive || precision {
            out.push('\n');
            continue;
        }
        if let Some(rest) = t.strip_prefix("uniform ")
            && rest.trim_end().ends_with(';')
            && !rest.contains('{')
            && !rest.contains("sampler")
        {
            let decl = rest.trim_end().trim_end_matches(';');
            let name = decl.split_whitespace().last().unwrap_or("");
            if !known.contains(&name) {
                out.push_str(rest);
            }
            out.push('\n');
            continue;
        }
        if let Some(rest) = t.strip_prefix("varying")
            && rest.starts_with(char::is_whitespace)
            && let Some(decls) = parse_varying(rest)
        {
            for v in decls {
                if !varyings.iter().any(|x| x.name == v.name) {
                    varyings.push(v);
                }
            }
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Replaces comments with spaces, keeping newlines (and so line numbers).
fn blank_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('/')) => {
                while let Some(&n) = chars.peek() {
                    if n == '\n' {
                        break;
                    }
                    chars.next();
                }
                out.push(' ');
            }
            ('/', Some('*')) => {
                chars.next();
                out.push_str("  ");
                let mut prev = ' ';
                for n in chars.by_ref() {
                    out.push(if n == '\n' { '\n' } else { ' ' });
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A `return` for the end of a function with header `header` (text before
/// its `{`) when it returns a built-in type. GLSL lets control reach the end
/// of a non-void function; naga rejects it.
fn fallback_return(header: &str) -> Option<String> {
    let before = header.trim().strip_suffix(')')?;
    let open = before.find('(')?;
    let mut words = before[..open].split_whitespace().rev();
    let name = words.next()?;
    let ty = words.next()?;
    if !is_identifier(name) || name == "main" {
        return None;
    }
    let value = match ty {
        "float" | "int" | "uint" => format!("{ty}(0)"),
        "bool" => "false".to_owned(),
        "vec2" | "vec3" | "vec4" | "ivec2" | "ivec3" | "ivec4" | "mat2" | "mat3" | "mat4" => {
            format!("{ty}(0)")
        }
        "bvec2" | "bvec3" | "bvec4" => format!("{ty}(false)"),
        _ => return None,
    };
    Some(format!(" return {value}; "))
}

/// Moves initializers of non-`const` globals into statements for an init
/// function run at the start of `main`. Desktop GLSL lets globals be
/// initialised from uniforms (`float ratio = RENDERSIZE.x / RENDERSIZE.y;`),
/// but naga keeps only constant initializers. Returns the rewritten source
/// (same line count) and the init statements in source order.
fn hoist_initializers(src: &str) -> (String, String) {
    let src = blank_comments(src);
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut init = String::new();
    let (mut braces, mut parens) = (0i32, 0i32);
    let mut start = 0;
    let mut i = 0;
    let mut line_start = true;
    let mut fallback: Option<String> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if line_start && c == b'#' {
            // Preprocessor line (with continuations): copy as is.
            while i < bytes.len() && !(bytes[i] == b'\n' && (i == 0 || bytes[i - 1] != b'\\')) {
                i += 1;
            }
            if braces == 0 && parens == 0 {
                out.push_str(&src[start..i]);
                start = i;
            }
            continue;
        }
        if c == b'\n' {
            line_start = true;
        } else if !c.is_ascii_whitespace() {
            line_start = false;
        }
        match c {
            b'{' => {
                if braces == 0 {
                    fallback = fallback_return(&src[start..i]);
                }
                braces += 1;
            }
            b'}' => {
                braces -= 1;
                if braces == 0 {
                    out.push_str(&src[start..i]);
                    if let Some(f) = fallback.take() {
                        out.push_str(&f);
                    }
                    out.push('}');
                    start = i + 1;
                }
            }
            b'(' | b'[' => parens += 1,
            b')' | b']' => parens -= 1,
            b';' if braces == 0 && parens == 0 => {
                let stmt = &src[start..i];
                match split_global_init(stmt) {
                    Some((decl, inits)) => {
                        let body = stmt.trim_start();
                        out.push_str(&stmt[..stmt.len() - body.len()]);
                        out.push_str(&decl);
                        out.push(';');
                        out.extend(std::iter::repeat_n('\n', body.matches('\n').count()));
                        init.push_str(&inits);
                    }
                    None => out.push_str(&src[start..=i]),
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&src[start..]);
    (out, init)
}

/// For a top-level `type a = x, b[2] = y, c;` returns the declaration
/// without initializers and the assignments `a = x; b = y;`.
fn split_global_init(stmt: &str) -> Option<(String, String)> {
    let trimmed = stmt.trim_start();
    let first = trimmed
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()?;
    if matches!(
        first,
        "const" | "uniform" | "in" | "out" | "layout" | "struct" | "precision" | "return"
    ) || !trimmed.contains('=')
    {
        return None;
    }
    // Split declarators on top-level commas.
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut last = 0;
    for (i, c) in trimmed.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&trimmed[last..i]);
                last = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&trimmed[last..]);

    let mut decl = String::new();
    let mut inits = String::new();
    for (k, part) in parts.iter().enumerate() {
        let (lhs, rhs) = match top_level_assign(part) {
            Some(at) => (part[..at].trim(), Some(part[at + 1..].trim())),
            None => (part.trim(), None),
        };
        // The declared name: the last identifier, before an optional `[N]`.
        let base = lhs.split('[').next()?.trim_end();
        let name_start = base
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map_or(0, |p| p + 1);
        let name = &base[name_start..];
        if !is_identifier(name) || lhs.contains("[]") {
            return None;
        }
        if k == 0 {
            // The type must come before the name, and contain no call.
            let ty = base[..name_start].trim();
            if ty.is_empty() || ty.contains('(') || !is_identifier(ty.rsplit(' ').next()?) {
                return None;
            }
            decl.push_str(lhs);
        } else {
            decl.push_str(", ");
            decl.push_str(lhs);
        }
        if let Some(rhs) = rhs {
            inits.push_str(&format!("    {name} = {rhs};\n"));
        }
    }
    Some((decl, inits))
}

/// Index of a top-level `=` that is an assignment (not `==`, `<=`, …).
fn top_level_assign(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = i.checked_sub(1).map(|p| b[p]);
                let next = b.get(i + 1);
                if !matches!(prev, Some(b'=' | b'<' | b'>' | b'!')) && next != Some(&b'=') {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses `[qualifiers] type name[, name2][N];` after `varying`.
fn parse_varying(rest: &str) -> Option<Vec<Varying>> {
    let body = rest.trim().strip_suffix(';')?;
    let body = body.split("//").next()?.trim();
    let mut words = body.split_whitespace().peekable();
    let mut ty = words.next()?;
    while matches!(ty, "highp" | "mediump" | "lowp" | "flat" | "smooth") {
        ty = words.next()?;
    }
    let rest: String = words.collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    for decl in rest.split(',') {
        let decl = decl.trim();
        let (name, len) = match decl.split_once('[') {
            Some((n, l)) => (n.trim(), Some(l.trim_end_matches(']').trim().parse().ok()?)),
            None => (decl, None),
        };
        if !is_identifier(name) {
            return None;
        }
        out.push(Varying {
            ty: ty.to_owned(),
            name: name.to_owned(),
            len,
        });
    }
    Some(out)
}

/// Locations used by one value of a GLSL type.
fn locations(ty: &str) -> u32 {
    match ty {
        "mat2" => 2,
        "mat3" => 3,
        "mat4" => 4,
        _ => 1,
    }
}

/// Generated GLSL for both stages.
struct Generated {
    vertex: String,
    vertex_prelude_lines: u32,
    fragment: String,
    fragment_prelude_lines: u32,
    textures: Vec<TextureSource>,
}

fn generate(
    header: &IsfHeader,
    fs_body: &str,
    vs_src: Option<&str>,
) -> Result<Generated, IsfError> {
    let mut varyings = Vec::new();
    let known: Vec<&str> = BUILTINS
        .iter()
        .map(|(n, _)| *n)
        .chain(header.inputs.iter().map(|i| i.name.as_str()))
        .collect();
    let (vs_body, vs_init) = match vs_src {
        Some(vs) => {
            let (body, init) = hoist_initializers(&scrub(vs, &mut varyings, &known));
            (Some(body), init)
        }
        None => (None, String::new()),
    };
    let (fs_body, fs_init) = hoist_initializers(&scrub(fs_body, &mut varyings, &known));

    // Uniform block: vec4s, then vec2s, then scalars (tight std140 packing).
    let mut members: Vec<(String, UniformType)> = BUILTINS
        .iter()
        .map(|(n, t)| ((*n).to_owned(), *t))
        .collect();
    for input in &header.inputs {
        if let Some(t) = uniform_type(&input.kind) {
            members.push((member_name(input), t));
        }
    }
    members.sort_by_key(|(_, t)| std::cmp::Reverse(t.size()));

    let mut textures = Vec::new();
    let mut names = Vec::new();
    for (i, input) in header.inputs.iter().enumerate() {
        if input.kind.is_texture() {
            textures.push(TextureSource::Input(i));
            names.push(input.name.clone());
        }
    }
    for (i, (name, _)) in header.imported.iter().enumerate() {
        if !names.contains(name) {
            textures.push(TextureSource::Imported(i));
            names.push(name.clone());
        }
    }
    for pass in &header.passes {
        if let Some(t) = &pass.target
            && !names.contains(t)
        {
            textures.push(TextureSource::Target(t.clone()));
            names.push(t.clone());
        }
    }

    let mut common = String::from("#version 450\n");
    common.push_str("layout(set = 0, binding = 0) uniform AtdIsf {\n");
    for (name, ty) in &members {
        common.push_str(&format!("    {} {name};\n", ty.glsl()));
    }
    common.push_str("};\nlayout(set = 0, binding = 1) uniform sampler atd_samp;\n");
    for (i, name) in names.iter().enumerate() {
        common.push_str(&format!(
            "layout(set = 0, binding = {}) uniform texture2D {name};\n",
            FIRST_TEXTURE_BINDING as usize + i
        ));
    }
    let bools: Vec<&IsfInput> = header
        .inputs
        .iter()
        .filter(|i| matches!(i.kind, IsfInputKind::Bool { .. } | IsfInputKind::Event))
        .collect();
    for b in &bools {
        common.push_str(&format!("bool {};\n", b.name));
    }
    let set_bools: String = bools
        .iter()
        .map(|b| format!("    {} = {} != 0;\n", b.name, member_name(b)))
        .collect();
    common.push_str("vec2 atd_flip(vec2 c) { return vec2(c.x, 1.0 - c.y); }\n");
    common.push_str("#define IMG_SIZE(i) vec2(textureSize(sampler2D(i, atd_samp), 0))\n");
    // ISF v1 names and per-image built-ins still used by older shaders.
    common.push_str("#define vv_FragNormCoord isf_FragNormCoord\n");
    for name in &names {
        common.push_str(&format!(
            "#define _{name}_imgRect vec4(0.0, 0.0, 1.0, 1.0)\n#define _{name}_imgSize IMG_SIZE({name})\n#define _{name}_flip false\n"
        ));
    }

    let varying_decls = |dir: &str| -> String {
        let mut s = String::new();
        let mut loc = 1;
        for v in &varyings {
            let len = v.len.map(|n| format!("[{n}]")).unwrap_or_default();
            s.push_str(&format!(
                "layout(location = {loc}) {dir} {} {}{len};\n",
                v.ty, v.name
            ));
            loc += locations(&v.ty) * v.len.unwrap_or(1);
        }
        s
    };

    // Fragment stage.
    let mut fs = common.clone();
    fs.push_str("layout(location = 0) in vec2 isf_FragNormCoord;\n");
    fs.push_str(&varying_decls("in"));
    fs.push_str("layout(location = 0) out vec4 atd_out;\nvec4 atd_fc;\n");
    fs.push_str(concat!(
        "#define IMG_NORM_PIXEL(i, c) texture(sampler2D(i, atd_samp), atd_flip(c))\n",
        "#define IMG_PIXEL(i, c) texture(sampler2D(i, atd_samp), atd_flip((c) / IMG_SIZE(i)))\n",
        "#define IMG_THIS_NORM_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n",
        "#define IMG_THIS_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n",
        "#define texture2D(i, c) texture(sampler2D(i, atd_samp), atd_flip(c))\n",
        "#define gl_FragColor atd_out\n",
        "#define gl_FragCoord atd_fc\n",
        "#define main isf_main\n",
    ));
    let fragment_prelude_lines = fs.lines().count() as u32;
    fs.push_str(&fs_body);
    fs.push_str(&format!("\nvoid atd_init() {{\n{fs_init}}}\n"));
    fs.push_str("#undef main\n#undef gl_FragCoord\nvoid main() {\n");
    fs.push_str(
        "    atd_fc = vec4(gl_FragCoord.x, RENDERSIZE.y - gl_FragCoord.y, gl_FragCoord.z, gl_FragCoord.w);\n",
    );
    fs.push_str(&set_bools);
    fs.push_str("    atd_init();\n    isf_main();\n}\n");

    // Vertex stage: a quad over the target, or the user's vertex shader.
    let mut vs = common;
    vs.push_str("layout(location = 0) out vec2 isf_FragNormCoord;\n");
    vs.push_str(&varying_decls("out"));
    vs.push_str(concat!(
        "vec2 atd_pos;\n",
        "void isf_vertShaderInit() {\n",
        "    gl_Position = vec4(atd_pos, 0.0, 1.0);\n",
        "    isf_FragNormCoord = atd_pos * 0.5 + 0.5;\n",
        "}\n",
        "#define IMG_NORM_PIXEL(i, c) textureLod(sampler2D(i, atd_samp), atd_flip(c), 0.0)\n",
        "#define IMG_PIXEL(i, c) textureLod(sampler2D(i, atd_samp), atd_flip((c) / IMG_SIZE(i)), 0.0)\n",
        "#define IMG_THIS_NORM_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n",
        "#define IMG_THIS_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n",
        "#define texture2D(i, c) textureLod(sampler2D(i, atd_samp), atd_flip(c), 0.0)\n",
        "#define vv_vertShaderInit isf_vertShaderInit\n",
        "#define main isf_main\n",
    ));
    let vertex_prelude_lines = vs.lines().count() as u32;
    vs.push_str(
        vs_body
            .as_deref()
            .unwrap_or("void main() { isf_vertShaderInit(); }\n"),
    );
    vs.push_str(&format!("\nvoid atd_init() {{\n{vs_init}}}\n"));
    vs.push_str(concat!(
        "#undef main\n",
        "const vec2 ATD_QUAD[6] = vec2[6](vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),\n",
        "                                 vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0));\n",
        "void main() {\n",
        "    atd_pos = ATD_QUAD[gl_VertexIndex];\n",
    ));
    vs.push_str(&set_bools);
    vs.push_str("    atd_init();\n    isf_main();\n}\n");

    Ok(Generated {
        vertex: vs,
        vertex_prelude_lines,
        fragment: fs,
        fragment_prelude_lines,
        textures,
    })
}

/// Translates an ISF shader (fragment source, optional `.vs` source) to WGSL.
pub fn translate(fs_src: &str, vs_src: Option<&str>) -> Result<IsfShader, IsfError> {
    let header = parse_header(fs_src)?;
    let (_, body) = split_source(fs_src)?;
    // Keep the user's line numbers: count the header's lines.
    let header_lines = fs_src[..fs_src.len() - body.len()].lines().count() as u32 - 1;
    let vs_body = vs_src.map(|vs| match split_source(vs) {
        Ok((_, b)) if vs.trim_start().starts_with("/*") => b,
        _ => vs,
    });
    let generated = generate(&header, body, vs_body)?;

    let fragment = compile(
        &generated.fragment,
        naga::ShaderStage::Fragment,
        IsfStage::Fragment,
        generated.fragment_prelude_lines,
        header_lines,
    )?;
    let vertex = compile(
        &generated.vertex,
        naga::ShaderStage::Vertex,
        IsfStage::Vertex,
        generated.vertex_prelude_lines,
        0,
    )?;
    let uniforms = uniform_layout(&fragment.1, &header)?;
    Ok(IsfShader {
        header,
        vertex_wgsl: vertex.0,
        fragment_wgsl: fragment.0,
        uniforms,
        textures: generated.textures,
    })
}

/// GLSL → validated naga module → WGSL. Returns the WGSL and the module
/// re-parsed from it (so layouts are those the GPU will see).
fn compile(
    glsl: &str,
    shader_stage: naga::ShaderStage,
    stage: IsfStage,
    prelude_lines: u32,
    line_offset: u32,
) -> Result<(String, naga::Module), IsfError> {
    let user_line = |line: u32| (line > prelude_lines).then(|| line - prelude_lines + line_offset);
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(shader_stage), glsl)
        .map_err(|e| {
            let first = e.errors.first();
            IsfError {
                stage,
                line: first
                    .and_then(|e| e.location(glsl))
                    .and_then(|l| user_line(l.line_number)),
                message: first.map(|e| e.kind.to_string()).unwrap_or_default(),
            }
        })?;
    let info = validate(&module).map_err(|message| IsfError {
        stage: IsfStage::Validate,
        line: None,
        message: format!("{stage}: {message}"),
    })?;
    let wgsl =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| IsfError {
                stage: IsfStage::Validate,
                line: None,
                message: format!("{stage}: WGSL output: {e}"),
            })?;
    let reparsed = naga::front::wgsl::parse_str(&wgsl).map_err(|e| IsfError {
        stage: IsfStage::Validate,
        line: None,
        message: format!("{stage}: WGSL re-parse: {e}"),
    })?;
    validate(&reparsed).map_err(|message| IsfError {
        stage: IsfStage::Validate,
        line: None,
        message: format!("{stage}: WGSL re-validate: {message}"),
    })?;
    Ok((wgsl, reparsed))
}

fn validate(module: &naga::Module) -> Result<naga::valid::ModuleInfo, String> {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(module)
    .map_err(|e| {
        let mut msg = e.as_inner().to_string();
        let mut source: Option<&dyn std::error::Error> = std::error::Error::source(e.as_inner());
        while let Some(s) = source {
            msg.push_str(": ");
            msg.push_str(&s.to_string());
            source = s.source();
        }
        msg
    })
}

/// Reads the uniform block's member offsets from the translated module.
fn uniform_layout(module: &naga::Module, header: &IsfHeader) -> Result<UniformLayout, IsfError> {
    let err = |m: &str| IsfError {
        stage: IsfStage::Validate,
        line: None,
        message: m.to_owned(),
    };
    let global = module.global_variables.iter().map(|(_, g)| g).find(|g| {
        g.space == naga::AddressSpace::Uniform
            && g.binding
                .as_ref()
                .is_some_and(|b| b.group == 0 && b.binding == UNIFORM_BINDING)
    });
    let mut layout = UniformLayout::default();
    let Some(global) = global else {
        // Every uniform was optimised away; nothing to upload.
        return Ok(layout);
    };
    let naga::TypeInner::Struct { members, span } = &module.types[global.ty].inner else {
        return Err(err("uniform block is not a struct"));
    };
    layout.size = *span;
    let mut lookup: Vec<(String, String, UniformType)> = BUILTINS
        .iter()
        .map(|(n, t)| ((*n).to_owned(), (*n).to_owned(), *t))
        .collect();
    for input in &header.inputs {
        if let Some(t) = uniform_type(&input.kind) {
            lookup.push((member_name(input), input.name.clone(), t));
        }
    }
    // The WGSL writer may rename members that clash with WGSL keywords, but
    // it keeps their order, so match by position.
    let mut order: Vec<(String, UniformType)> =
        lookup.iter().map(|(m, _, t)| (m.clone(), *t)).collect();
    order.sort_by_key(|(_, t)| std::cmp::Reverse(t.size()));
    if order.len() != members.len() {
        return Err(err("uniform block member count changed in translation"));
    }
    for ((member_name, ty), m) in order.iter().zip(members) {
        let isf = lookup
            .iter()
            .find(|(n, _, _)| n == member_name)
            .map(|(_, isf, _)| isf.clone())
            .unwrap_or_default();
        layout.fields.push(UniformField {
            name: isf,
            ty: *ty,
            offset: m.offset,
        });
    }
    Ok(layout)
}

/// Evaluates a pass size expression such as `floor($WIDTH/4.0)`.
///
/// Supports numbers, `$WIDTH`, `$HEIGHT`, `$<input>` from `vars`, `+ - * /`,
/// parentheses, and `floor ceil round abs min max pow sqrt`.
pub fn eval_size(expr: &str, vars: &dyn Fn(&str) -> Option<f64>) -> Result<f64, String> {
    let tokens = tokenize(expr)?;
    let mut p = ExprParser {
        tokens,
        pos: 0,
        vars,
    };
    let v = p.expr()?;
    if p.pos != p.tokens.len() {
        return Err(format!("unexpected trailing input in {expr:?}"));
    }
    Ok(v)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Var(String),
    Op(char),
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Tok::Num(
                text.parse().map_err(|_| format!("bad number {text}"))?,
            ));
        } else if c == '$' || c.is_ascii_alphabetic() || c == '_' {
            let start = if c == '$' { i + 1 } else { i };
            i = start;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            out.push(if c == '$' {
                Tok::Var(text)
            } else {
                Tok::Ident(text)
            });
        } else if "+-*/(),".contains(c) {
            out.push(Tok::Op(c));
            i += 1;
        } else {
            return Err(format!("unexpected {c:?}"));
        }
    }
    Ok(out)
}

struct ExprParser<'a> {
    tokens: Vec<Tok>,
    pos: usize,
    vars: &'a dyn Fn(&str) -> Option<f64>,
}

impl ExprParser<'_> {
    fn peek_op(&self) -> Option<char> {
        match self.tokens.get(self.pos) {
            Some(Tok::Op(c)) => Some(*c),
            _ => None,
        }
    }

    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek_op() {
            self.pos += 1;
            let r = self.term()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        while let Some(op @ ('*' | '/')) = self.peek_op() {
            self.pos += 1;
            let r = self.unary()?;
            v = if op == '*' { v * r } else { v / r };
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, String> {
        if self.peek_op() == Some('-') {
            self.pos += 1;
            return Ok(-self.unary()?);
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<f64, String> {
        let tok = self.tokens.get(self.pos).cloned().ok_or("unexpected end")?;
        self.pos += 1;
        match tok {
            Tok::Num(n) => Ok(n),
            Tok::Var(name) => (self.vars)(&name).ok_or_else(|| format!("unknown ${name}")),
            Tok::Op('(') => {
                let v = self.expr()?;
                self.expect(')')?;
                Ok(v)
            }
            Tok::Ident(f) => {
                self.expect('(')?;
                let mut args = vec![self.expr()?];
                while self.peek_op() == Some(',') {
                    self.pos += 1;
                    args.push(self.expr()?);
                }
                self.expect(')')?;
                let a = args[0];
                let b = args.get(1).copied();
                Ok(match (f.as_str(), b) {
                    ("floor", None) => a.floor(),
                    ("ceil", None) => a.ceil(),
                    ("round", None) => a.round(),
                    ("abs", None) => a.abs(),
                    ("sqrt", None) => a.sqrt(),
                    ("min", Some(b)) => a.min(b),
                    ("max", Some(b)) => a.max(b),
                    ("pow", Some(b)) => a.powf(b),
                    _ => return Err(format!("unknown function {f}/{}", args.len())),
                })
            }
            Tok::Op(c) => Err(format!("unexpected {c:?}")),
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.peek_op() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {c:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATOR: &str = r#"/*{
        "DESCRIPTION": "test generator",
        "CATEGORIES": ["Generator"],
        "INPUTS": [
            {"NAME": "level", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 1.0},
            {"NAME": "tint", "TYPE": "color", "DEFAULT": [1.0, 0.5, 0.25, 1.0]},
            {"NAME": "invert", "TYPE": "bool", "DEFAULT": true},
            {"NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["a", "b", "c"], "DEFAULT": 1},
            {"NAME": "centre", "TYPE": "point2D", "DEFAULT": [0.5, 0.5]},
        ]
    }*/
    precision highp float;
    void main() {
        vec2 uv = isf_FragNormCoord;
        float d = distance(uv, centre) * level;
        vec4 c = tint * d;
        if (invert) c.rgb = 1.0 - c.rgb;
        if (mode == 2) c = vec4(gl_FragCoord.xy / RENDERSIZE, 0.0, 1.0);
        gl_FragColor = c + 0.0 * TIME;
    }
    "#;

    #[test]
    fn header_is_parsed_leniently() {
        let h = parse_header(GENERATOR).unwrap();
        assert_eq!(h.inputs.len(), 5);
        assert_eq!(
            h.inputs[0].kind,
            IsfInputKind::Float {
                min: 0.0,
                max: 2.0,
                default: 1.0
            }
        );
        assert_eq!(h.inputs[2].kind, IsfInputKind::Bool { default: true });
        assert!(matches!(
            &h.inputs[3].kind,
            IsfInputKind::Long { min: 0, max: 2, default: 1, labels, .. } if labels.len() == 3
        ));
        assert_eq!(h.passes.len(), 1);
        assert!(!h.is_filter());
    }

    #[test]
    fn generator_translates_with_a_uniform_layout() {
        let s = translate(GENERATOR, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.fragment_wgsl.contains("@fragment"));
        assert!(s.vertex_wgsl.contains("@vertex"));
        let f = |n: &str| s.uniforms.field(n).unwrap_or_else(|| panic!("{n}"));
        assert_eq!(f("DATE").offset, 0);
        assert_eq!(f("tint").ty, UniformType::Vec4);
        assert_eq!(f("tint").offset % 16, 0);
        assert_eq!(f("centre").offset % 8, 0);
        assert_eq!(f("invert").ty, UniformType::Int);
        assert!(s.uniforms.size >= 16 * 2 + 8 * 2 + 4 * 7);
        assert!(s.textures.is_empty());
    }

    #[test]
    fn multipass_filter_binds_inputs_then_targets() {
        let src = r#"/*{
            "INPUTS": [{"NAME": "inputImage", "TYPE": "image"}],
            "PASSES": [{"TARGET": "half", "WIDTH": "floor($WIDTH/2.0)", "HEIGHT": "$HEIGHT/2", "PERSISTENT": true}, {}]
        }*/
        void main() {
            if (PASSINDEX == 0) gl_FragColor = IMG_THIS_PIXEL(inputImage);
            else gl_FragColor = IMG_NORM_PIXEL(half, isf_FragNormCoord) + IMG_PIXEL(inputImage, gl_FragCoord.xy) * IMG_SIZE(half).x;
        }"#;
        let s = translate(src, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.header.is_filter());
        assert_eq!(
            s.textures,
            vec![
                TextureSource::Input(0),
                TextureSource::Target("half".into())
            ]
        );
        assert!(s.header.passes[0].persistent);
        let w = eval_size(s.header.passes[0].width.as_deref().unwrap(), &|v| {
            (v == "WIDTH").then_some(641.0)
        })
        .unwrap();
        assert_eq!(w, 320.0);
    }

    #[test]
    fn vertex_shader_varyings_are_linked() {
        let fs = r#"/*{"INPUTS": [{"NAME": "inputImage", "TYPE": "image"}]}*/
        varying vec2 left;
        varying vec2 right;
        void main() { gl_FragColor = IMG_NORM_PIXEL(inputImage, left) + IMG_NORM_PIXEL(inputImage, right); }"#;
        let vs = r#"varying vec2 left;
        varying vec2 right;
        void main() {
            isf_vertShaderInit();
            left = isf_FragNormCoord - vec2(0.01, 0.0);
            right = isf_FragNormCoord + vec2(0.01, 0.0);
        }"#;
        let s = translate(fs, Some(vs)).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.vertex_wgsl.contains("@location(2)"));
        assert!(s.fragment_wgsl.contains("@location(2)"));
    }

    #[test]
    fn errors_point_at_the_users_line() {
        let src =
            "/*{\"INPUTS\": []}*/\nvoid main() {\n    gl_FragColor = vec4(undefined_thing);\n}\n";
        let e = translate(src, None).unwrap_err();
        assert_eq!(e.stage, IsfStage::Fragment);
        assert_eq!(e.line, Some(3), "{e}");
    }

    #[test]
    fn global_initializers_are_hoisted() {
        let src = "float ratio = RENDERSIZE.x / RENDERSIZE.y; // c\nconst float K = 2.0;\nvec2 a = vec2(1.0), b, c[2] = vec2[2](a, a);\nfloat f(float x) { float y = x; return y; }\n#define Q 1\nbool ok = TIME >= 0.0;\n";
        let (body, init) = hoist_initializers(src);
        assert_eq!(body.lines().count(), src.lines().count());
        assert!(body.contains("float ratio;"));
        assert!(body.contains("const float K = 2.0;"));
        assert!(body.contains("vec2 a, b, c[2];"));
        assert!(body.contains("float y = x;"));
        assert!(body.contains("bool ok;"));
        assert_eq!(
            init,
            "    ratio = RENDERSIZE.x / RENDERSIZE.y;\n    a = vec2(1.0);\n    c = vec2[2](a, a);\n    ok = TIME >= 0.0;\n"
        );
        let shader = format!(
            "/*{{\"INPUTS\": []}}*/\n{src}void main() {{ gl_FragColor = vec4(ratio, a, float(ok)); }}\n"
        );
        let s = translate(&shader, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.fragment_wgsl.contains("fn atd_init"));
    }

    #[test]
    fn size_expressions() {
        let vars = |v: &str| match v {
            "WIDTH" => Some(1920.0),
            "HEIGHT" => Some(1080.0),
            "amount" => Some(0.5),
            _ => None,
        };
        assert_eq!(eval_size("$WIDTH/3.0", &vars).unwrap(), 640.0);
        assert_eq!(
            eval_size("floor($HEIGHT*$amount) + 1", &vars).unwrap(),
            541.0
        );
        assert_eq!(eval_size("max(1.0, -2)", &vars).unwrap(), 1.0);
        assert_eq!(eval_size("16", &vars).unwrap(), 16.0);
        assert!(eval_size("$nope", &vars).is_err());
    }
}
