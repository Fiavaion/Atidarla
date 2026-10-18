//! Golden WGSL: each `tests/golden/<name>.<target>.atk` must compile to exactly
//! `<name>.<target>.wgsl`. Run with `ATD_BLESS=1` to rewrite the expected files after an
//! intended change, then review the diff.
//!
//! The kernels may have CRLF line endings in a Windows checkout; the WGSL never contains
//! kernel text, so the expected output is the same on every OS.

use std::path::{Path, PathBuf};

use atd_kernel::{CompileOptions, Target, compile};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn kernels() -> Vec<(PathBuf, Target)> {
    let mut out: Vec<(PathBuf, Target)> = std::fs::read_dir(golden_dir())
        .expect("golden dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "atk"))
        .map(|p| {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let target = stem
                .rsplit('.')
                .next()
                .and_then(Target::parse)
                .unwrap_or_else(|| panic!("{}: name must end in .points or .pixels", p.display()));
            (p, target)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn generated_wgsl_matches_the_golden_files() {
    let bless = std::env::var("ATD_BLESS").is_ok_and(|v| v == "1");
    let kernels = kernels();
    assert!(kernels.len() >= 8, "found {} golden kernels", kernels.len());
    let mut failures = Vec::new();
    for (path, target) in kernels {
        let src = std::fs::read_to_string(&path).expect("read kernel");
        let compiled = compile(&src, &CompileOptions::new(target)).unwrap_or_else(|d| {
            panic!(
                "{} does not compile:\n{}",
                path.display(),
                d.iter()
                    .map(|d| d.render("kernel", &src))
                    .collect::<String>()
            )
        });
        let expected_path = path.with_extension("wgsl");
        if bless {
            std::fs::write(&expected_path, &compiled.wgsl).expect("write golden");
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        if expected != compiled.wgsl {
            failures.push(expected_path.display().to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "generated WGSL differs from: {failures:?} (rerun with ATD_BLESS=1 if intended)"
    );
}

/// Same source, same text: compiling twice, or with CRLF line endings, or with
/// different parameter values (they are uniforms), gives byte-identical WGSL.
#[test]
fn output_is_stable_text() {
    for (path, target) in kernels() {
        let src = std::fs::read_to_string(&path).expect("read kernel");
        let lf = src.replace("\r\n", "\n");
        let crlf = lf.replace('\n', "\r\n");
        let opts = CompileOptions::new(target);
        let a = compile(&lf, &opts).expect("compiles").wgsl;
        let b = compile(&lf, &opts).expect("compiles").wgsl;
        let c = compile(&crlf, &opts).expect("compiles").wgsl;
        assert_eq!(a, b, "{}", path.display());
        assert_eq!(a, c, "{}: CRLF changed the output", path.display());
    }
    // Changing a ch() default changes the parameter, not the shader.
    let a = compile(
        "@P.y = ch(\"h\", 1.0);",
        &CompileOptions::new(Target::Points),
    )
    .unwrap();
    let b = compile(
        "@P.y = ch(\"h\", 7.5);",
        &CompileOptions::new(Target::Points),
    )
    .unwrap();
    assert_eq!(a.wgsl, b.wgsl);
    assert_ne!(a.params, b.params);
}
