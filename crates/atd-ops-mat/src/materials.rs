//! The MAT operators: each fills a [`Material`] from its parameters
//! (contract: `atd_gpu::mat`). They run on the CPU in microseconds and
//! allocate nothing.

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{MatCtx, MatOp, Params};
use atd_gpu::mat::{Material, Model};

/// A MAT operator schema: optional IMG texture inputs, one MAT output.
pub(crate) fn mat(
    id: &str,
    label: &str,
    summary: &str,
    texture: Option<&str>,
    params: Vec<ParamSchema>,
) -> OperatorSchema {
    let socket = |name: &str, wire| SocketSchema {
        name: name.into(),
        wire,
        optional: true,
        feedback: false,
    };
    OperatorSchema {
        type_id: format!("mat.{id}"),
        family: OpFamily::Mat,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs: texture
            .map(|t| vec![socket(t, WireType::Img)])
            .unwrap_or_default(),
        outputs: vec![socket("out", WireType::Mat)],
        capabilities: vec![],
        // Filling a struct from parameters: a few microseconds.
        cook_budget_us: 20,
        time_dependent: false,
        sink: false,
    }
}

fn use_cd(default: bool) -> ParamSchema {
    ParamSchema::toggle("use_cd", "Use Cd", default)
        .help("Multiply the colour by the geometry's `Cd` attribute (per point, else per primitive) when it has one.")
}

pub fn schemas() -> Vec<OperatorSchema> {
    vec![
        mat(
            "unlit",
            "Unlit",
            "A flat colour with no lighting: Colour, times `Cd` and times the `map` image (sampled at the `uv` attribute) when they are there. Use it for constant colours, vertex colours and textures shown as they are.",
            Some("map"),
            vec![
                ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0])
                    .help("Surface colour; alpha below 1 is screen-door transparency when the renderer antialiases."),
                use_cd(true),
            ],
        ),
        mat(
            "pbr",
            "PBR",
            "A lit surface: Lambert diffuse plus GGX specular from the renderer's light, ambient light and emission. Albedo is multiplied by `Cd` and by the `map` image (at `uv`) when they are there; emission adds a glow that lighting does not darken.",
            Some("map"),
            vec![
                ParamSchema::colour("albedo", "Albedo", [0.8, 0.8, 0.8, 1.0])
                    .help("Base colour of the surface."),
                ParamSchema::float("roughness", "Roughness", 0.5, Some(0.02), Some(1.0))
                    .help("0.02: a sharp mirror-like highlight; 1: a broad, dull one."),
                ParamSchema::float("metallic", "Metallic", 0.0, Some(0.0), Some(1.0))
                    .help("0: plastic-like (white highlights); 1: metal (highlights take the albedo, no diffuse)."),
                ParamSchema::colour("emission", "Emission", [0.0, 0.0, 0.0, 1.0])
                    .help("Light the surface gives off, added after lighting."),
                ParamSchema::float("emission_strength", "Emission Strength", 1.0, Some(0.0), None)
                    .help("Multiplies Emission (above 1 for a glow that a later Bloom picks up)."),
                use_cd(true),
            ],
        ),
        mat(
            "matcap",
            "Matcap",
            "A lit-sphere image (`matcap` input) looked up by the surface normal as the camera sees it, so a painted or photographed sphere gives the whole shape its look with no lights. Without an image it shows a soft top light.",
            Some("matcap"),
            vec![
                ParamSchema::colour("tint", "Tint", [1.0, 1.0, 1.0, 1.0])
                    .help("Multiplies the matcap colour."),
                use_cd(false),
            ],
        ),
        mat(
            "toon",
            "Toon",
            "Cel shading: the light's diffuse term in a few hard bands between Shadow and Colour, a hard highlight and a rim light along silhouettes.",
            None,
            vec![
                ParamSchema::colour("colour", "Colour", [0.95, 0.55, 0.3, 1.0])
                    .help("Colour where the light falls fully."),
                ParamSchema::colour("shadow", "Shadow", [0.25, 0.1, 0.2, 1.0])
                    .help("Colour where the light does not fall."),
                ParamSchema::int("bands", "Bands", 3, Some(1), Some(16))
                    .help("Number of steps from shadow to light."),
                ParamSchema::float("highlight", "Highlight", 0.3, Some(0.0), Some(1.0))
                    .help("Size of the hard specular spot; 0 turns it off."),
                ParamSchema::float("rim", "Rim", 0.5, Some(0.0), Some(2.0))
                    .help("Strength of the rim light along silhouettes."),
                ParamSchema::float("rim_width", "Rim Width", 0.3, Some(0.0), Some(1.0))
                    .help("How far the rim reaches in from the silhouette."),
                use_cd(false),
            ],
        ),
        mat(
            "line",
            "Line",
            "Wireframe: the edges of every polygon (not the diagonals the renderer adds to draw it) and every polyline, antialiased, a set number of pixels wide; inside the polygons a fill colour, or nothing so the back edges show.",
            None,
            vec![
                ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0])
                    .help("Edge colour."),
                ParamSchema::float("width", "Width", 1.5, Some(0.25), Some(32.0))
                    .help("Edge width in pixels."),
                ParamSchema::toggle("fill", "Fill", false)
                    .help("Fill the polygons with Fill Colour (hidden-line look); off: only edges are drawn."),
                ParamSchema::colour("fill_colour", "Fill Colour", [0.0, 0.0, 0.0, 1.0])
                    .help("Colour inside the polygons when Fill is on."),
                use_cd(false),
            ],
        ),
        mat(
            "point_sprite",
            "Point Sprite",
            "Draws the geometry's points (not its polygons) as discs or squares facing the camera, sized in world units (scaled by `pscale` when present, and by perspective) or in pixels; optionally shaded like small spheres by the renderer's light.",
            None,
            vec![
                ParamSchema::colour("colour", "Colour", [1.0, 1.0, 1.0, 1.0])
                    .help("Sprite colour."),
                ParamSchema::menu("size_mode", "Size Mode", SIZE_MODES, "world")
                    .help("world: Size is in world units, so far sprites look smaller; pixels: Size is in pixels."),
                ParamSchema::float("size", "Size", 0.05, Some(0.0), None)
                    .help("Diameter in world units or pixels (see Size Mode)."),
                ParamSchema::menu("shape", "Shape", SHAPES, "disc")
                    .help("disc (antialiased edge) or square."),
                ParamSchema::toggle("sphere", "Sphere Shading", false)
                    .help("Shade each disc as a small sphere lit by the renderer's light."),
                use_cd(true),
            ],
        ),
    ]
}

const SIZE_MODES: &[&str] = &["world", "pixels"];
const SHAPES: &[&str] = &["disc", "square"];

/// One MAT operator: a function from parameters to a material.
struct Fill(fn(&Params<'_>, &mut Material));

impl MatOp for Fill {
    fn cook(&mut self, ctx: &mut MatCtx<'_>) -> Result<(), String> {
        let mut m = Material {
            use_cd: ctx.params.bool("use_cd"),
            ..Material::default()
        };
        (self.0)(&ctx.params, &mut m);
        *ctx.output = m;
        Ok(())
    }
}

fn unlit(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Unlit;
    m.colour = p.vec4("colour");
}

fn pbr(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Pbr;
    m.colour = p.vec4("albedo");
    m.roughness = p.f32("roughness");
    m.metallic = p.f32("metallic");
    let s = p.f32("emission_strength").max(0.0);
    let e = p.vec4("emission");
    m.emission = [e[0] * s, e[1] * s, e[2] * s, 0.0];
}

fn matcap(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Matcap;
    m.colour = p.vec4("tint");
}

fn toon(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Toon;
    m.colour = p.vec4("colour");
    m.colour2 = p.vec4("shadow");
    m.params = [
        p.i64("bands").clamp(1, 16) as f32,
        p.f32("rim").clamp(0.0, 2.0),
        p.f32("highlight").clamp(0.0, 1.0),
        p.f32("rim_width").clamp(0.0, 1.0),
    ];
}

fn line(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Line;
    m.colour = p.vec4("colour");
    m.colour2 = if p.bool("fill") {
        p.vec4("fill_colour")
    } else {
        [0.0; 4]
    };
    m.params = [p.f32("width").clamp(0.25, 32.0), 0.0, 0.0, 0.0];
}

fn point_sprite(p: &Params<'_>, m: &mut Material) {
    m.model = Model::Sprite;
    m.colour = p.vec4("colour");
    m.params = [
        p.f32("size").max(0.0),
        f32::from(u8::from(p.str("size_mode") == "pixels")),
        f32::from(u8::from(p.str("shape") == "square")),
        f32::from(u8::from(p.bool("sphere"))),
    ];
}

/// The implementation of MAT operator `type_id`.
pub fn create(type_id: &str) -> Option<fn() -> Box<dyn MatOp>> {
    Some(match type_id {
        "mat.unlit" => || Box::new(Fill(unlit)),
        "mat.pbr" => || Box::new(Fill(pbr)),
        "mat.matcap" => || Box::new(Fill(matcap)),
        "mat.toon" => || Box::new(Fill(toon)),
        "mat.line" => || Box::new(Fill(line)),
        "mat.point_sprite" => || Box::new(Fill(point_sprite)),
        _ => return None,
    })
}
