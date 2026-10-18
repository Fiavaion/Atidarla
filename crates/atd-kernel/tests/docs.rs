//! Every ```` ```kernel points ```` / ```` ```kernel pixels ```` block in
//! `docs/kernel-dsl.md` compiles and its WGSL validates; ```` ```kernel error ```` blocks
//! must fail. Keeps the reference page honest.

use atd_kernel::{CompileOptions, Target, compile};

fn blocks() -> Vec<(String, String, usize)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/kernel-dsl.md");
    let text = std::fs::read_to_string(path).expect("read docs/kernel-dsl.md");
    let mut out = Vec::new();
    let mut current: Option<(String, String, usize)> = None;
    for (n, line) in text.lines().enumerate() {
        match &mut current {
            None => {
                if let Some(kind) = line.strip_prefix("```kernel") {
                    current = Some((kind.trim().to_owned(), String::new(), n + 1));
                }
            }
            Some((_, body, _)) => {
                if line.starts_with("```") {
                    out.push(current.take().expect("open block"));
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
    }
    out
}

#[test]
fn reference_examples_compile() {
    let blocks = blocks();
    assert!(blocks.len() >= 5, "found {} kernel blocks", blocks.len());
    for (kind, src, line) in blocks {
        let (target, ok) = match kind.as_str() {
            "points" => (Target::Points, true),
            "pixels" => (Target::Pixels, true),
            "error" => (Target::Points, false),
            other => panic!("docs line {line}: unknown block kind `{other}`"),
        };
        let r = compile(&src, &CompileOptions::new(target));
        match (ok, r) {
            (true, Ok(_)) | (false, Err(_)) => {}
            (true, Err(d)) => panic!(
                "docs line {line}: example does not compile:\n{}",
                d.iter()
                    .map(|d| d.render("example", &src))
                    .collect::<String>()
            ),
            (false, Ok(_)) => panic!("docs line {line}: error example compiled"),
        }
    }
}

/// The ten-line curl-noise particle kernel of M2 AC2: exactly ten lines, compiles,
/// validates, and binds what it should.
#[test]
fn ten_line_curl_noise_kernel() {
    let (_, src, _) = blocks()
        .into_iter()
        .find(|(_, s, _)| s.contains("curlnoise") && s.contains("attract"))
        .expect("the curl-noise example is in the docs");
    assert_eq!(src.lines().count(), 10, "the AC2 kernel is ten lines");
    // The GPU tests run the golden copy; keep them the same kernel.
    let golden = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/curl_particles.points.atk"
    ))
    .expect("read golden");
    assert_eq!(golden.replace("\r\n", "\n"), src);
    let c = compile(&src, &CompileOptions::new(Target::Points)).expect("compiles");
    let names: Vec<&str> = c
        .bindings
        .attributes
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(names, ["P", "age", "v"]);
    let params: Vec<&str> = c.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(params, ["freq", "amp", "attract", "target"]);
    assert!(c.wgsl.contains("fn atd_curlnoise"));
}
