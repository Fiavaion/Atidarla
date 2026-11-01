//! Deterministic fuzzing of the ISF importer: mutated shaders (cut,
//! duplicated and swapped ranges, stray punctuation, truncation) must give
//! errors, never panics, because users import arbitrary files.

use lsr_gpu::isf::{eval_size, parse_header, translate};

const SEEDS: &[&str] = &[
    r#"/*{
    "DESCRIPTION": "seed",
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "amount", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["a", "b", "c"]},
        {"NAME": "tint", "TYPE": "color", "DEFAULT": [1, 0.5, 0, 1]},
        {"NAME": "flip", "TYPE": "bool"},
        {"NAME": "centre", "TYPE": "point2D", "DEFAULT": [0.5, 0.5]}
    ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    if (flip) c.rgb = 1.0 - c.rgb;
    gl_FragColor = mix(c, tint, amount * float(mode) / 2.0);
}
"#,
    r#"/*{
    "INPUTS": [{"NAME": "inputImage", "TYPE": "image"}],
    "PASSES": [
        {"TARGET": "half", "WIDTH": "$WIDTH/2", "HEIGHT": "floor($HEIGHT / 2.0)", "PERSISTENT": true},
        {}
    ]
}*/
void main() {
    if (PASSINDEX == 0) {
        gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord) * 0.5 + IMG_NORM_PIXEL(half, isf_FragNormCoord) * 0.5;
    } else {
        gl_FragColor = IMG_NORM_PIXEL(half, isf_FragNormCoord);
    }
}
"#,
    r#"/*{ "INPUTS": [], "IMPORTED": {"tex": {"PATH": "a.png"}} }*/
varying vec2 offset;
float wave(float x) { return sin(x * 6.2831) * 0.5 + 0.5; }
void main() { gl_FragColor = vec4(wave(TIME + offset.x), wave(isf_FragNormCoord.y), 0.0, 1.0); }
"#,
];

const NOISE: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    "\"",
    ",",
    ":",
    "/*",
    "*/",
    "(",
    ")",
    ";",
    "$",
    "\\",
    "\n",
    "\0",
    "é",
    "1e400",
    "-",
    "PASSES",
    "TARGET",
    "INPUTS",
    "#define",
    "#if",
    "vec4(",
    "void main(",
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

/// A char-boundary-safe index into `s`.
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
        match rng.next(5) {
            0 => s.replace_range(a..b, ""),
            1 => {
                let piece = s[a..b].to_owned();
                s.insert_str(a, &piece);
            }
            2 => s.insert_str(a, NOISE[rng.next(NOISE.len())]),
            3 => s.truncate(a),
            _ => {
                let piece = s[a..b].to_owned();
                let c = at(&s, rng);
                s.replace_range(a..b, "");
                let c = c.min(s.len());
                let mut c = c;
                while !s.is_char_boundary(c) {
                    c -= 1;
                }
                s.insert_str(c, &piece);
            }
        }
    }
    s
}

#[test]
fn mutated_shaders_never_panic() {
    let iterations = std::env::var("LSR_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let mut rng = Rng(0xC0FF_EE15_F00D_0001);
    for seed in SEEDS {
        translate(seed, None).expect("seeds translate");
    }
    let (mut ok, mut err) = (0, 0);
    for i in 0..iterations {
        let src = mutate(SEEDS[rng.next(SEEDS.len())], &mut rng);
        let header_ok = std::panic::catch_unwind(|| parse_header(&src).is_ok());
        assert!(
            header_ok.is_ok(),
            "iteration {i}: parse_header panicked on:\n{src}"
        );
        // translate() turns panics into errors; any such error is a bug.
        match translate(&src, None) {
            Ok(_) => ok += 1,
            Err(e) if e.message.contains("translator panicked") => {
                panic!("iteration {i}: {}\non:\n{src}", e.message)
            }
            Err(_) => err += 1,
        }
    }
    assert!(err > 0 && ok > 0, "{ok} translated, {err} rejected");
}

#[test]
fn size_expressions_never_panic() {
    let mut rng = Rng(0x5EED_0000_0000_0002);
    let atoms = [
        "$WIDTH", "$HEIGHT", "2", "0", "1e9", "-", "+", "*", "/", "(", ")", "floor(", "max(", ",",
        "pow(", "x", " ", ".", "1.5",
    ];
    let vars = |name: &str| match name {
        "WIDTH" => Some(640.0),
        "HEIGHT" => Some(360.0),
        _ => None,
    };
    for _ in 0..5000 {
        let n = 1 + rng.next(10);
        let expr: String = (0..n).map(|_| atoms[rng.next(atoms.len())]).collect();
        let r = std::panic::catch_unwind(|| eval_size(&expr, &vars));
        assert!(r.is_ok(), "eval_size panicked on {expr:?}");
    }
}
