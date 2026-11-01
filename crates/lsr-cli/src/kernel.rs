//! `lasair kernel`: compile a Kernel DSL file and print its diagnostics, bindings and
//! parameters, or the generated WGSL (`docs/kernel-dsl.md`). Experimental: the Kernel
//! DSL (PRD A12) is still Proposed and no operator uses it yet.

use std::path::Path;

use anyhow::{Context, Result, bail};
use lsr_kernel::{Access, CompileOptions, Target, compile};

/// `target`: `points` or `pixels`; by default from the file name (`*.pixels.atk` is a
/// pixel kernel), otherwise points.
pub fn run(file: &Path, target: Option<&str>, emit_wgsl: bool) -> Result<()> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let target = match target {
        Some(t) => Target::parse(t)
            .with_context(|| format!("unknown target `{t}` (use points or pixels)"))?,
        None => {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.contains(".pixels.") {
                Target::Pixels
            } else {
                Target::Points
            }
        }
    };
    let name = file.display().to_string();
    let compiled = match compile(&source, &CompileOptions::new(target)) {
        Ok(c) => c,
        Err(diagnostics) => {
            for d in &diagnostics {
                eprint!("{}", d.render(&name, &source));
            }
            bail!("{} error(s) in {name}", diagnostics.len());
        }
    };
    if emit_wgsl {
        print!("{}", compiled.wgsl);
        return Ok(());
    }
    let b = &compiled.bindings;
    println!(
        "ok    {name}  ({target:?} kernel, entry point `{}`)",
        b.entry_point
    );
    for a in &b.attributes {
        let access = match a.access {
            Access::Read => "read",
            Access::ReadWrite => "read_write",
        };
        let what = if a.snapshot {
            "  (input values for point())"
        } else {
            ""
        };
        println!(
            "  @group({}) @binding({})  @{:<10} {:<6} {:<7} stride {:>2}  {access}{what}",
            a.group, a.binding, a.name, a.kernel_type, a.ty, a.stride
        );
    }
    for i in &b.images {
        println!(
            "  @group({}) @binding({})  image input {}",
            i.group, i.binding, i.input
        );
    }
    for p in &compiled.params {
        let n = p.ty.components();
        let range = match (p.min, p.max) {
            (Some(lo), Some(hi)) => format!("  [{lo}, {hi}]"),
            (Some(lo), None) => format!("  [{lo}, ...]"),
            (None, Some(hi)) => format!("  [..., {hi}]"),
            (None, None) => String::new(),
        };
        println!(
            "  ch(\"{}\")  {}  default {:?}{range}",
            p.name,
            p.ty.kernel_type(),
            &p.default[..n]
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_good_kernels_and_reports_bad_ones() {
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lsr-kernel/tests/golden");
        run(&golden.join("curl_particles.points.atk"), None, false).expect("points kernel");
        run(&golden.join("grade.pixels.atk"), None, true).expect("pixel kernel");
        assert!(run(&golden.join("grade.pixels.atk"), Some("voxels"), false).is_err());
        let bad = std::env::temp_dir().join(format!("lsr-cli-kernel-{}.atk", std::process::id()));
        std::fs::write(&bad, "int n = @P.x;\n").expect("write");
        let err = run(&bad, None, false).expect_err("type error");
        let _ = std::fs::remove_file(&bad);
        assert!(err.to_string().contains("1 error(s)"), "{err}");
    }
}
