//! `atidarla isf-import`: translate a directory of ISF shaders and report
//! per-shader results (FR-K6, M1 AC2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use atd_core::graph::{Edit, Endpoint, Node, Wire};
use atd_core::registry::Registry;
use atd_gpu::GpuContext;
use atd_gpu::engine::{Engine, EngineConfig, OpLibrary};
use atd_interop::readback::read_rgba8;
use atd_ops_img::isf::{IsfDef, register};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Entry {
    shader: String,
    type_id: Option<String>,
    /// Translated to WGSL and validated by naga.
    ok: bool,
    /// Built and cooked on a GPU (only with `--gpu`).
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu_ok: Option<bool>,
    passes: usize,
    has_vertex: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Entry {
    fn passed(&self) -> bool {
        self.ok && self.gpu_ok != Some(false)
    }
}

/// Options for [`run`].
#[derive(Debug)]
pub struct Options<'a> {
    /// List every failure and group failures by kind.
    pub report: bool,
    /// Also build every shader on a GPU and cook it for two frames.
    pub gpu: bool,
    pub json: Option<&'a Path>,
    /// Write the generated WGSL here.
    pub out: Option<&'a Path>,
    /// With `gpu`, write a contact sheet of every shader's output (PNG).
    pub thumbs: Option<&'a Path>,
    /// Fail unless at least this percentage pass.
    pub min_pass: Option<f64>,
    /// With `gpu`, how long one shader may take to compile.
    pub compile_timeout: Duration,
}

/// Translates every `.fs` under `dir` and prints a summary.
pub fn run(dir: &Path, opts: &Options<'_>) -> Result<()> {
    let mut shaders = Vec::new();
    collect(dir, &mut shaders)?;
    shaders.sort();
    anyhow::ensure!(
        !shaders.is_empty(),
        "no .fs shaders under {}",
        dir.display()
    );

    let mut entries = Vec::new();
    let mut defs = Vec::new();
    for path in &shaders {
        let name = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fs = std::fs::read_to_string(path).with_context(|| format!("reading {name}"))?;
        let vs = std::fs::read_to_string(path.with_extension("vs")).ok();
        let result = IsfDef::new(&stem, &fs, vs.as_deref());
        let passes = atd_gpu::isf::parse_header(&fs)
            .map(|h| h.passes.len())
            .unwrap_or(0);
        if let (Ok(def), Some(out)) = (&result, opts.out) {
            let base = out.join(&name).with_extension("");
            if let Some(parent) = base.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(base.with_extension("frag.wgsl"), &def.shader.fragment_wgsl)?;
            std::fs::write(base.with_extension("vert.wgsl"), &def.shader.vertex_wgsl)?;
        }
        let (ok, type_id, error) = match result {
            Ok(def) => {
                let id = def.schema.type_id.clone();
                defs.push(def);
                (true, Some(id), None)
            }
            Err(e) => (false, None, Some(e.to_string())),
        };
        entries.push(Entry {
            shader: name,
            type_id,
            ok,
            gpu_ok: None,
            passes,
            has_vertex: vs.is_some(),
            error,
        });
    }

    if opts.gpu {
        gpu_check(defs, &mut entries, opts.thumbs, opts.compile_timeout)?;
    }

    let passed = entries.iter().filter(|e| e.passed()).count();
    let total = entries.len();
    if opts.report {
        for e in entries.iter().filter(|e| !e.passed()) {
            println!("FAIL {}: {}", e.shader, e.error.as_deref().unwrap_or(""));
        }
        let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
        for e in entries.iter().filter_map(|e| e.error.as_deref()) {
            *by_kind.entry(error_kind(e)).or_default() += 1;
        }
        if !by_kind.is_empty() {
            println!("\nfailures by kind:");
            let mut kinds: Vec<_> = by_kind.into_iter().collect();
            kinds.sort_by_key(|k| std::cmp::Reverse(k.1));
            for (kind, n) in kinds {
                println!("  {n:>4}  {kind}");
            }
        }
        let count = |f: &dyn Fn(&Entry) -> bool| {
            let sel = entries.iter().filter(|e| f(e));
            sel.fold((0, 0), |(o, t), e| (o + usize::from(e.passed()), t + 1))
        };
        let (multi_ok, multi) = count(&|e| e.passes > 1);
        let (vert_ok, vert) = count(&|e| e.has_vertex);
        println!("\nmulti-pass: {multi_ok}/{multi}  with vertex shader: {vert_ok}/{vert}");
    }
    let translated = entries.iter().filter(|e| e.ok).count();
    println!(
        "isf-import: {translated}/{total} translated{}; {passed}/{total} passed ({:.1}%)",
        if opts.gpu {
            ", built and cooked on GPU"
        } else {
            ""
        },
        passed as f64 * 100.0 / total as f64
    );
    if let Some(json) = opts.json {
        std::fs::write(json, serde_json::to_string_pretty(&entries)?)
            .with_context(|| format!("writing {}", json.display()))?;
    }
    let rate = passed as f64 * 100.0 / total as f64;
    if let Some(min) = opts.min_pass {
        anyhow::ensure!(rate >= min, "pass rate {rate:.1}% is below {min}%");
    }
    Ok(())
}

/// Builds every translated shader on the GPU and cooks it twice (t = 0
/// and one frame later), with the test card on every image input.
fn gpu_check(
    defs: Vec<IsfDef>,
    entries: &mut [Entry],
    thumbs: Option<&Path>,
    compile_timeout: Duration,
) -> Result<()> {
    let gpu = GpuContext::headless().context("no GPU adapter for --gpu")?;
    eprintln!("isf-import: GPU {}", gpu.adapter.get_info().name);
    let mut registry = Registry::default();
    atd_ops_img::register(&mut registry);
    let mut library = OpLibrary::default();
    atd_ops_img::library(&mut library);
    let card = IsfDef::new("atd test card", TEST_CARD, None).expect("the test card translates");
    let card_id = card.schema.type_id.clone();
    let mut defs = defs;
    defs.push(card);
    let registered = register(defs, &mut registry, &mut library);
    let config = EngineConfig {
        width: 640,
        height: 360,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };
    let device = gpu.device.clone();
    let mut engine = Engine::new(gpu.device, gpu.queue, registry, library, config);
    let card = engine.alloc_id();
    let card_name = engine.network().unique_name("card");
    engine.edit(
        "isf-import",
        [Edit::AddNode {
            node: Node::new(card, card_id, card_name),
        }],
    )?;
    let mut sheet = thumbs.map(|_| Sheet::new(entries.iter().filter(|e| e.ok).count()));

    for entry in entries.iter_mut().filter(|e| e.ok) {
        let id = entry
            .type_id
            .clone()
            .expect("translated entries have an ID");
        if !registered.contains(&id) {
            entry.gpu_ok = Some(false);
            entry.error = Some(format!("type ID {id} clashes with another shader"));
            continue;
        }
        let node = engine.alloc_id();
        let name = engine.network().unique_name("isf");
        engine.edit(
            "isf-import",
            [Edit::AddNode {
                node: Node::new(node, id, name),
            }],
        )?;
        let inputs: Vec<String> = engine
            .registry()
            .get(entry.type_id.as_deref().unwrap_or(""))
            .map(|s| s.inputs.iter().map(|i| i.name.clone()).collect())
            .unwrap_or_default();
        for input in inputs {
            engine.edit(
                "isf-import",
                [Edit::Connect {
                    wire: Wire::new(Endpoint::new(card, "out"), Endpoint::new(node, input)),
                }],
            )?;
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut error = None;
        // Pipelines compile on a worker thread; wait for them (bounded).
        let start = Instant::now();
        let mut compiled = true;
        loop {
            if let Err(e) = engine.cook(&[node], 0.0) {
                error = Some(e.to_string());
            }
            if !engine.pending() {
                break;
            }
            if start.elapsed() > compile_timeout {
                compiled = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let compile = start.elapsed();
        if !compiled {
            error = Some(format!(
                "compile did not finish within {}s",
                compile_timeout.as_secs()
            ));
        } else {
            if compile > Duration::from_secs(2) {
                eprintln!(
                    "isf-import: slow compile {:.1}s: {}",
                    compile.as_secs_f64(),
                    entry.shader
                );
            }
            if let Err(e) = engine.cook(&[node], 1.0 / 60.0) {
                error = Some(e.to_string());
            }
            if let Some(e) = engine.error(node) {
                error = Some(e.to_owned());
            }
        }
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        if let Some(e) = pollster::block_on(scope.pop()) {
            error.get_or_insert(format!("wgpu: {e}"));
        }
        if let Some(sheet) = &mut sheet {
            let pixels = engine
                .result(node)
                .and_then(|t| read_rgba8(engine.device(), engine.queue(), t).ok());
            sheet.push(pixels.as_deref(), config.width, config.height);
        }
        entry.gpu_ok = Some(error.is_none());
        entry.error = error.map(|e| format!("gpu: {}", condense(&e)));
        engine.edit("isf-import", [Edit::RemoveNode { id: node }])?;
    }
    if let (Some(sheet), Some(path)) = (sheet, thumbs) {
        sheet.write(path)?;
        println!(
            "contact sheet: {} (row-major, translated shaders in order)",
            path.display()
        );
    }
    Ok(())
}

/// A generator with orientation marks: a coordinate gradient, a checker,
/// white in the top-left corner and a blue bar along the bottom.
const TEST_CARD: &str = r#"/*{"DESCRIPTION": "Atidarla ISF test card.", "INPUTS": []}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    float checker = mod(floor(uv.x * 8.0) + floor(uv.y * 4.5), 2.0);
    vec3 c = vec3(uv.x, uv.y, 0.25) * (0.6 + 0.4 * checker);
    if (uv.x < 0.12 && uv.y > 0.8) c = vec3(1.0);
    if (uv.y < 0.06) c = vec3(0.1, 0.2, 1.0);
    gl_FragColor = vec4(c, 1.0);
}
"#;

/// Tiles of every output, 8 per row.
struct Sheet {
    pixels: Vec<u8>,
    n: usize,
}

const TILE_W: u32 = 160;
const TILE_H: u32 = 90;
const COLS: u32 = 8;

impl Sheet {
    fn new(count: usize) -> Self {
        let rows = (count as u32).div_ceil(COLS).max(1);
        Self {
            pixels: vec![0; (COLS * TILE_W * rows * TILE_H * 4) as usize],
            n: 0,
        }
    }

    fn width(&self) -> u32 {
        COLS * TILE_W
    }

    /// Adds a tile, nearest-sampled from a `w`×`h` RGBA8 image (red if missing).
    fn push(&mut self, image: Option<&[u8]>, w: u32, h: u32) {
        let (tx, ty) = (self.n as u32 % COLS, self.n as u32 / COLS);
        let stride = self.width() * 4;
        for y in 0..TILE_H {
            for x in 0..TILE_W {
                let px = match image {
                    Some(img) => {
                        let (sx, sy) = (x * w / TILE_W, y * h / TILE_H);
                        let i = ((sy * w + sx) * 4) as usize;
                        [img[i], img[i + 1], img[i + 2], 255]
                    }
                    None => [255, 0, 0, 255],
                };
                // One-pixel gutter.
                let px = if x == 0 || y == 0 {
                    [40, 40, 40, 255]
                } else {
                    px
                };
                let o = ((ty * TILE_H + y) * stride + (tx * TILE_W + x) * 4) as usize;
                self.pixels[o..o + 4].copy_from_slice(&px);
            }
        }
        self.n += 1;
    }

    fn write(&self, path: &Path) -> Result<()> {
        let w = self.width();
        let h = (self.pixels.len() as u32 / 4) / w;
        let file = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?,
        );
        let mut encoder = png::Encoder::new(file, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&self.pixels)?;
        Ok(())
    }
}

/// One line with the whole cause chain (wgpu errors span several lines),
/// capped for the report.
fn condense(s: &str) -> String {
    let joined = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    let mut out: String = joined.chars().take(600).collect();
    if out.len() < joined.len() {
        out.push('…');
    }
    out
}

/// The error message without identifiers and numbers, for grouping.
fn error_kind(e: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    for c in e.chars() {
        if c == '"' || c == '`' {
            quoted = !quoted;
            if quoted {
                out.push('…');
            }
            continue;
        }
        if !quoted && !c.is_ascii_digit() {
            out.push(c);
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(110)
        .collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "fs") {
            out.push(path);
        }
    }
    Ok(())
}
