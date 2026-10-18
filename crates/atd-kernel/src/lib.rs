//! Kernel DSL compiler: a VEX-flavoured per-element language lowered through a typed IR to
//! WGSL (PRD A12, FR-K1..K3). Sub-crate of the `atd-gpu` domain.
//!
//! **Status: experimental. Decision A12 is still Proposed (PRD §0).** This crate is
//! self-contained: nothing in the engine or the registry uses it, so it can be removed
//! without touching anything else. Language reference: `docs/kernel-dsl.md`; integration
//! plan and open questions: `docs/milestones/m2/kernel-dsl.md`.
//!
//! ```
//! use atd_kernel::{CompileOptions, Target, compile};
//!
//! let kernel = "v@P += v@v * @TimeInc * ch(\"speed\", 1.0);";
//! let out = compile(kernel, &CompileOptions::new(Target::Points)).unwrap();
//! assert!(out.wgsl.contains("@compute"));
//! assert_eq!(out.params[0].name, "speed");
//! assert_eq!(out.bindings.attributes.len(), 2); // P (read-write), v (read)
//! ```
//!
//! [`compile`] is a pure function: it allocates and may take milliseconds, so the engine
//! calls it on a worker thread (rule 1), never on the render thread.

pub mod bindings;
pub mod diag;
pub mod interp;
pub mod ir;
pub mod noise;
pub mod types;

mod ast;
mod check;
mod lexer;
mod lower;
mod parser;
mod stdlib;
mod validate;

pub use bindings::{
    Access, AttrBinding, Bindings, FrameGlobals, ImageBinding, ParamDecl, ParamType, Slot,
};
pub use diag::{Diagnostic, Origin, Span};
pub use lower::SourceMap;
pub use stdlib::noise_library;
pub use types::{AttrType, Scalar, Type};

use diag::{Error, LineIndex};

/// What a kernel runs over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Once per point: a compute shader over attribute storage buffers.
    Points,
    /// Once per output pixel: a full-screen fragment shader.
    Pixels,
}

impl Target {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "points" | "point" => Some(Target::Points),
            "pixels" | "pixel" => Some(Target::Pixels),
            _ => None,
        }
    }
}

/// One attribute of the kernel's input geometry, as the engine knows it before cooking
/// (design §5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrSchema {
    pub name: String,
    pub ty: AttrType,
}

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub target: Target,
    /// The input's point attributes, when known; decides the type of `@name` without a
    /// sigil and checks sigils against it.
    pub input_schema: Option<Vec<AttrSchema>>,
    /// Validate the WGSL with naga (on by default).
    pub validate: bool,
    /// Storage buffers the device allows per stage (8 with `wgpu::Limits::default()`).
    pub max_storage_buffers: u32,
}

impl CompileOptions {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            input_schema: None,
            validate: true,
            max_storage_buffers: 8,
        }
    }
}

/// A compiled kernel.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub target: Target,
    /// The complete WGSL module; also the "Generated WGSL" view (FR-K3).
    pub wgsl: String,
    pub bindings: Bindings,
    /// Parameters created by `ch()`, in order of first use.
    pub params: Vec<ParamDecl>,
    pub source_map: SourceMap,
    /// The typed IR (used by the reference interpreter).
    pub program: ir::Program,
}

impl Compiled {
    /// Writes the uniform buffer (`bindings.uniform_size` bytes) without allocating.
    /// `values[slot]` holds each parameter's value (missing entries use the default).
    pub fn write_uniforms(&self, globals: &FrameGlobals, values: &[[f32; 4]], out: &mut [u8]) {
        bindings::write_uniforms(&self.params, globals, values, out);
    }

    /// The attributes whose values survive a hot reload (FR-K4): every attribute the
    /// kernel writes, as (name, storage type), sorted by name. A new version of the kernel
    /// may keep a buffer when the same name has the same type.
    pub fn state_keys(&self) -> Vec<(String, AttrType)> {
        self.bindings
            .attributes
            .iter()
            .filter(|a| a.access == Access::ReadWrite)
            .map(|a| (a.name.clone(), a.ty))
            .collect()
    }
}

/// Compiles `source` for `options.target`. Never panics: any failure, including an
/// internal error, comes back as diagnostics (at most [`diag::MAX_ERRORS`]).
pub fn compile(source: &str, options: &CompileOptions) -> Result<Compiled, Vec<Diagnostic>> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_inner(source, options)
    }));
    match result {
        Ok(r) => r,
        Err(_) => Err(vec![Diagnostic {
            line: 1,
            col: 1,
            len: 1,
            message: "internal compiler error: the compiler panicked".into(),
            hint: Some("this is a bug in the kernel compiler; please report the kernel".into()),
            span: Span::default(),
            origin: Origin::Kernel,
        }]),
    }
}

fn compile_inner(source: &str, options: &CompileOptions) -> Result<Compiled, Vec<Diagnostic>> {
    let index = LineIndex::new(source);
    let report = |errors: &[Error], origin: Origin| -> Vec<Diagnostic> {
        let mut d: Vec<Diagnostic> = errors
            .iter()
            .take(diag::MAX_ERRORS)
            .map(|e| index.diagnostic(e, origin))
            .collect();
        d.sort_by_key(|d| (d.span.start, d.span.end));
        d.dedup_by(|a, b| a.span == b.span && a.message == b.message);
        d
    };
    let mut errors = Vec::new();
    let tokens = lexer::lex(source, &mut errors);
    let lexed = errors.len();
    let ast = parser::parse(&tokens, &mut errors);
    if !errors.is_empty() {
        // A bad character or number usually makes the parser complain on the same line
        // too; keep only the lexer's error there.
        let lines: Vec<u32> = errors[..lexed]
            .iter()
            .map(|e| index.position(e.span.start).0)
            .collect();
        let mut kept: Vec<Error> = errors[..lexed].to_vec();
        kept.extend(
            errors[lexed..]
                .iter()
                .filter(|e| !lines.contains(&index.position(e.span.start).0))
                .cloned(),
        );
        return Err(report(&kept, Origin::Kernel));
    }
    let Some(program) = check::check(
        &ast,
        options.target,
        options.input_schema.as_deref(),
        &mut errors,
    ) else {
        return Err(report(&errors, Origin::Kernel));
    };
    let lowered = lower::lower(&program, options.max_storage_buffers)
        .map_err(|e| report(&[e], Origin::Kernel))?;
    if options.validate {
        validate::validate(&lowered.wgsl, &lowered.source_map)
            .map_err(|e| report(&[e], Origin::Wgsl))?;
    }
    Ok(Compiled {
        target: options.target,
        wgsl: lowered.wgsl,
        bindings: lowered.bindings,
        params: program.params.clone(),
        source_map: lowered.source_map,
        program,
    })
}
