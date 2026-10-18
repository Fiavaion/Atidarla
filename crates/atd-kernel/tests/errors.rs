//! FR-K3: errors carry the right line, column and length, a useful message and, where
//! there is an obvious fix, a hint.

use atd_kernel::{CompileOptions, Diagnostic, Origin, Target, compile};

fn errors(src: &str, target: Target) -> Vec<Diagnostic> {
    match compile(src, &CompileOptions::new(target)) {
        Ok(_) => panic!("expected an error for:\n{src}"),
        Err(d) => d,
    }
}

/// (source, line, col, len, message fragment, hint fragment)
const POINTS: &[(&str, u32, u32, u32, &str, &str)] = &[
    // Lexer and parser.
    (
        "float x = 1\nfloat y = 2;",
        1,
        12,
        1,
        "expected `;`",
        "end the statement",
    ),
    ("@P.y = 2.0f;", 1, 8, 4, "invalid number", "no suffix"),
    ("@P.y = 1 $ 2;", 1, 10, 1, "unexpected character", ""),
    ("x = \"open", 1, 5, 5, "unterminated string", ""),
    (
        "while (true) { }",
        1,
        1,
        5,
        "`while` loops are not supported",
        "counted loop",
    ),
    ("@P.y = (1 + 2;", 1, 14, 1, "expected `)`", ""),
    (
        "if (@P.x > 0 {\n  @P.y = 1;\n}",
        1,
        14,
        1,
        "expected `)`",
        "",
    ),
    (
        "float f(float a) {\n  return a;\n",
        3,
        1,
        1,
        "expected `}`",
        "not closed",
    ),
    (
        "@P.y = x++ ;",
        1,
        9,
        2,
        "`++` and `--` are statements",
        "own line",
    ),
    (
        "{\n  float g(float a) { return a; }\n}",
        2,
        3,
        5,
        "only be defined at the top level",
        "",
    ),
    // Names.
    ("@P.y = speed;", 1, 8, 5, "unknown variable `speed`", ""),
    ("@P.y = P.x;", 1, 8, 1, "unknown variable `P`", "@P"),
    (
        "@P = nromalize(@P);",
        1,
        6,
        9,
        "unknown function `nromalize`",
        "did you mean `normalize`",
    ),
    (
        "float a = 1;\nfloat a = 2;",
        2,
        7,
        1,
        "already declared",
        "",
    ),
    (
        "float sin(float x) { return x; }",
        1,
        7,
        3,
        "built-in function",
        "",
    ),
    (
        "float f(float a) { return f(a); }",
        1,
        7,
        1,
        "calls itself",
        "recursion",
    ),
    // Types.
    (
        "int n = @P.x * 4;",
        1,
        9,
        8,
        "expected `int`, found `float`",
        "int(...)",
    ),
    (
        "vec3 v = {1, 2, 3, 4};",
        1,
        10,
        12,
        "expected `vec3`, found `vec4`",
        ".xyz",
    ),
    (
        "@P = @P + {1, 2};",
        1,
        6,
        11,
        "cannot combine `vec3` and `vec2`",
        "",
    ),
    ("if (@P) { }", 1, 5, 2, "a condition must be", ""),
    (
        "@P.y = @P < 1;",
        1,
        8,
        6,
        "cannot combine",
        "compare components",
    ),
    ("@P.q = 1;", 1, 4, 1, "`.q` is not a component", ""),
    (
        "@P.xy = {1, 2};",
        1,
        4,
        2,
        "only one component",
        "whole vector",
    ),
    ("vec3 v; v[3] = 1;", 1, 11, 1, "out of range", ""),
    (
        "float f(float a) { if (a > 0) return a; }",
        1,
        7,
        1,
        "can end without returning",
        "",
    ),
    (
        "return 1;",
        1,
        8,
        1,
        "the kernel body returns nothing",
        "return;",
    ),
    ("break;", 1, 1, 6, "only work inside a loop", ""),
    ("@P.y = 3000000000;", 1, 8, 10, "too large for an int", ""),
    ("sin(1.0);", 1, 1, 8, "has no effect", ""),
    // Attributes.
    (
        "v@x = 1; f@x = 2;",
        1,
        10,
        3,
        "used as `vec3` elsewhere and `float` here",
        "",
    ),
    ("s@name = 1;", 1, 1, 6, "string attributes", ""),
    ("3@m = 1;", 1, 1, 3, "matrix attributes", ""),
    ("@ptnum = 1;", 1, 1, 6, "read-only", ""),
    ("f@ptnum = 1;", 1, 1, 7, "is a `int`, not a `float`", ""),
    (
        "float f(float a) { return a + @P.x; }",
        1,
        31,
        2,
        "inside a function",
        "argument",
    ),
    // Loops.
    (
        "for (int i = 0; i < 4; i += 0.5) { }",
        1,
        29,
        3,
        "positive integer literal",
        "",
    ),
    (
        "for (int i = 0; i < 4; i--) { }",
        1,
        24,
        3,
        "counts away",
        "count up",
    ),
    (
        "for (int i = 0; 4 > i; i++) { }",
        1,
        17,
        1,
        "must start with the counter",
        "",
    ),
    (
        "for (int i = 0; i < 4; i++) { i = 2; }",
        1,
        31,
        1,
        "cannot be assigned in the loop",
        "",
    ),
    (
        "for (float i = 0; i < 4; i++) { }",
        1,
        6,
        11,
        "`int` counter",
        "",
    ),
    // Helpers.
    (
        "@P.y = ch(\"a b\");",
        1,
        11,
        5,
        "not a valid parameter name",
        "",
    ),
    (
        "@P.y = ch(\"a\", @P.x);",
        1,
        16,
        4,
        "must be a constant",
        "",
    ),
    (
        "@P = chv(\"d\", {1, 2});",
        1,
        15,
        6,
        "default with 3 components",
        "",
    ),
    (
        "@P.y = ch(\"a\", 1); @P.x = ch(\"a\", 2);",
        1,
        30,
        3,
        "declared twice",
        "",
    ),
    (
        "@P.y = clamp(1, 2);",
        1,
        8,
        11,
        "takes 3 arguments, found 2",
        "",
    ),
    (
        "@P.y = perlin(1.0);",
        1,
        15,
        3,
        "takes a `vec2` or a `vec3`",
        "set(x, 0)",
    ),
    ("@P.y = point(1, \"P\", 0).x;", 1, 14, 1, "only input 0", ""),
    (
        "@P.y = point(0, name, 0).x;",
        1,
        17,
        4,
        "string literal",
        "",
    ),
    (
        "int n = nearpoints(0, @P, 1, 4);",
        1,
        9,
        23,
        "only be the source of a foreach",
        "foreach",
    ),
    (
        "foreach (int j; nearpoint(0, @P)) { }",
        1,
        17,
        16,
        "only loop over nearpoints",
        "",
    ),
    (
        "@Cd = texture(5, {0, 0});",
        1,
        15,
        1,
        "must be 0, 1, 2 or 3",
        "",
    ),
];

#[test]
fn point_kernel_errors_are_positioned() {
    let mut bad = Vec::new();
    for &(src, line, col, len, msg, hint) in POINTS {
        let d = errors(src, Target::Points);
        let first = &d[0];
        let ok = first.line == line
            && first.col == col
            && first.len == len
            && first.message.contains(msg)
            && (hint.is_empty() || first.hint.as_deref().is_some_and(|h| h.contains(hint)));
        if !ok {
            bad.push(format!(
                "{src:?}\n  want {line}:{col} len {len} {msg:?} hint {hint:?}\n  got  {}:{} len {} {:?} hint {:?}",
                first.line, first.col, first.len, first.message, first.hint
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn pixel_kernel_errors() {
    let d = errors("@P = {0, 0};", Target::Pixels);
    assert!(d[0].message.contains("read-only"), "{d:?}");
    let d = errors("@v = {0, 0, 0};", Target::Pixels);
    assert!(d[0].message.contains("no attribute `@v`"), "{d:?}");
    let d = errors("v@Cd = {0, 0, 0};", Target::Pixels);
    assert!(d[0].message.contains("vec4"), "{d:?}");
    let d = errors("@Cd.x = @ptnum;", Target::Pixels);
    assert!(d[0].message.contains("no attribute"), "{d:?}");
    let d = errors("@Cd.x = npoints(0);", Target::Pixels);
    assert!(
        d[0].message.contains("only works in point kernels"),
        "{d:?}"
    );
}

#[test]
fn several_errors_are_reported_in_order() {
    let src = "float a = 1\nint b = 1.5;\n@P.y = c;\n@P = nope(1);\n";
    let d = errors(src, Target::Points);
    // A syntax error stops before type checking.
    assert_eq!(d.len(), 1, "{d:?}");
    let src = "int b = 1.5;\n@P.y = c;\n@P = nope(1);\nfloat ok = 1;\n";
    let d = errors(src, Target::Points);
    let lines: Vec<u32> = d.iter().map(|d| d.line).collect();
    assert_eq!(lines, [1, 2, 3], "{d:?}");
    // At most 25.
    let many = "@P.y = q;\n".repeat(100);
    assert_eq!(errors(&many, Target::Points).len(), 25);
}

/// Constant expressions wrap like run-time ones (naga does not fold them in function
/// scope), so a kernel never trips naga; the interpreter agrees.
#[test]
fn integer_overflow_wraps_and_never_reaches_naga() {
    let c = compile(
        "int x = 2147483647 + 1;\n@P.y = x;",
        &CompileOptions::new(Target::Points),
    )
    .expect("compiles");
    let mut g = atd_kernel::interp::Geometry {
        count: 1,
        attributes: Vec::new(),
    };
    atd_kernel::interp::run_points(&c, &mut g, &[], &Default::default()).expect("runs");
    assert_eq!(g.get("P").expect("P").vec3(0)[1], -2147483648.0);
    // Errors from the checker are kernel errors.
    let d = errors("@P.y = q;", Target::Points);
    assert_eq!(d[0].origin, Origin::Kernel);
}

#[test]
fn rendering_shows_the_line_and_a_caret() {
    let src = "float a = 1;\nint n = @P.x * 4;\n";
    let d = errors(src, Target::Points);
    let text = d[0].render("k.atk", src);
    assert_eq!(
        text,
        "k.atk:2:9: error: expected `int`, found `float`\n  int n = @P.x * 4;\n          ^^^^^^^^\n  hint: convert explicitly with int(...)\n"
    );
}

#[test]
fn too_many_storage_buffers() {
    let src = "f@a = 1; f@b = 1; f@c = 1; f@d = 1; f@e = 1; f@f = 1; f@g = 1; f@h = 1;";
    let d = errors(src, Target::Points);
    assert!(d[0].message.contains("9 storage buffers"), "{d:?}");
    let mut opts = CompileOptions::new(Target::Points);
    opts.max_storage_buffers = 16;
    assert!(compile(src, &opts).is_ok());
}
