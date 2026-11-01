//! Deterministic fuzzing of the kernel compiler: mutated kernels (cut, duplicated and
//! moved ranges, stray tokens, truncation) must give diagnostics, never a panic, and
//! anything that compiles must be WGSL naga accepts (a naga rejection is a compiler bug,
//! reported with `Origin::Wgsl`). `LSR_FUZZ_ROUNDS` sets the number of mutants.

use std::path::Path;

use lsr_kernel::{CompileOptions, Origin, Target, compile};

const NOISE: &[&str] = &[
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ";",
    ",",
    ".",
    "@",
    "v@",
    "i@",
    "3@",
    "\"",
    "/*",
    "*/",
    "//",
    "\n",
    "\0",
    "\u{e9}",
    "1e400",
    "-",
    "--",
    "++",
    "+=",
    "?",
    ":",
    "&&",
    "<<",
    "0x",
    "for",
    "foreach",
    "if",
    "else",
    "return",
    "break",
    "let",
    "int",
    "float",
    "vec3",
    "mat4",
    "void",
    "function",
    "ch(\"a\"",
    "point(0, \"P\", ",
    "nearpoints(",
    "texture(",
    "@P",
    "@ptnum",
    "M_PI",
    "2147483648",
    "-2147483648",
    ".xyzw",
    "[9]",
    "1.",
    ".5",
    "{1,2,3,4,5}",
    "set(",
    "ident()",
];

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

fn at(s: &str, rng: &mut Rng) -> usize {
    let mut i = rng.next(s.len() + 1);
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn mutate(src: &str, rng: &mut Rng) -> String {
    let mut s = src.to_owned();
    for _ in 0..1 + rng.next(4) {
        let (a, b) = {
            let (x, y) = (at(&s, rng), at(&s, rng));
            (x.min(y), x.max(y))
        };
        match rng.next(6) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_owned();
                s.insert_str(a, &piece);
            }
            2 | 5 => s.insert_str(a, NOISE[rng.next(NOISE.len())]),
            3 => s.truncate(a),
            _ => {
                let piece = s[a..b].to_owned();
                let c = at(&s, rng);
                s.replace_range(a..b, "");
                let mut c = c.min(s.len());
                while !s.is_char_boundary(c) {
                    c -= 1;
                }
                s.insert_str(c, &piece);
            }
        }
    }
    s
}

fn seeds() -> Vec<(String, Target)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut out: Vec<(String, Target)> = std::fs::read_dir(dir)
        .expect("golden dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "atk"))
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            let target = if name.contains(".pixels.") {
                Target::Pixels
            } else {
                Target::Points
            };
            (std::fs::read_to_string(&p).expect("read"), target)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn mutated_kernels_never_panic_or_emit_invalid_wgsl() {
    let rounds = std::env::var("LSR_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let seeds = seeds();
    for (src, target) in &seeds {
        compile(src, &CompileOptions::new(*target)).expect("seeds compile");
    }
    let mut rng = Rng(0x4B45_524E_454C_0001);
    let (mut ok, mut err) = (0, 0);
    for i in 0..rounds {
        let (seed, target) = &seeds[rng.next(seeds.len())];
        let src = mutate(seed, &mut rng);
        // Half the mutants also go to the other target.
        let target = if rng.next(2) == 0 {
            *target
        } else if *target == Target::Points {
            Target::Pixels
        } else {
            Target::Points
        };
        match compile(&src, &CompileOptions::new(target)) {
            Ok(c) => {
                ok += 1;
                assert!(!c.wgsl.is_empty());
            }
            Err(d) => {
                err += 1;
                assert!(
                    !d.is_empty() && d.len() <= 25,
                    "round {i}: {} diagnostics",
                    d.len()
                );
                for x in &d {
                    assert!(
                        !x.message.contains("panicked"),
                        "round {i}: compiler panicked on:\n{src}"
                    );
                    assert!(
                        x.origin != Origin::Wgsl,
                        "round {i}: naga rejected the generated WGSL ({}) for:\n{src}",
                        x.message
                    );
                    assert!(x.line >= 1 && x.col >= 1 && x.len >= 1, "round {i}: {x:?}");
                    let lines = src.split('\n').count() as u32;
                    assert!(
                        x.line <= lines.max(1),
                        "round {i}: line {} of {lines}",
                        x.line
                    );
                }
            }
        }
    }
    assert!(ok > 0 && err > 0, "{ok} compiled, {err} rejected");
}

/// Random byte soup, including invalid UTF-8 replaced by U+FFFD.
#[test]
fn random_text_never_panics() {
    let mut rng = Rng(0x5EED_0000_0000_0003);
    let alphabet: Vec<char> =
        "@$#vf ipu234{}()[];,.=+-*/%<>!&|^~?:\"\n\r\t0123456789.eExyzw_aA\u{e9}\u{2603}"
            .chars()
            .collect();
    for _ in 0..3000 {
        let n = rng.next(80);
        let s: String = (0..n).map(|_| alphabet[rng.next(alphabet.len())]).collect();
        for target in [Target::Points, Target::Pixels] {
            if let Err(d) = compile(&s, &CompileOptions::new(target)) {
                assert!(d.iter().all(|x| !x.message.contains("panicked")), "{s:?}");
                assert!(d.iter().all(|x| x.origin != Origin::Wgsl), "{s:?}: {d:?}");
            }
        }
    }
}

/// The nesting limit keeps every pass (parser, checker, lowering, naga) within a 1 MiB
/// stack, the size of the main thread on Windows, even in a debug build.
#[test]
fn deep_kernels_fit_a_small_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(|| {
            let depth = 40;
            let parens = format!("@P.y = {}1.0{};", "(".repeat(depth), ")".repeat(depth));
            let chain = format!("@P.y = 1.0{};", " + @P.x".repeat(depth));
            let unary = format!("@P.y = {}1.0;", "- ".repeat(depth));
            let ternary = format!("@P.y = {}1.0;", "@P.x > 0 ? 2.0 : ".repeat(depth / 2));
            let blocks = format!(
                "{}@P.y = 1;{}",
                "if (@P.x > 0) {".repeat(depth),
                "}".repeat(depth)
            );
            let mut results = Vec::new();
            for src in [parens, chain, unary, ternary, blocks] {
                results.push(compile(&src, &CompileOptions::new(Target::Points)).is_ok());
            }
            // Past the limit: an error, not a crash.
            let too_deep = format!("@P.y = {}1{};", "(".repeat(5000), ")".repeat(5000));
            let rejected = compile(&too_deep, &CompileOptions::new(Target::Points)).is_err();
            (results, rejected)
        })
        .expect("spawn");
    let (results, rejected) = handle.join().expect("no stack overflow");
    assert_eq!(results, [true; 5]);
    assert!(rejected);
}
