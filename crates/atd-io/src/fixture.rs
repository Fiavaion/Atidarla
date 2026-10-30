//! DMX fixtures and LED pixel mapping (FR-S7): fixture types, a patch that
//! places fixtures on universes and on regions of an image, and the
//! mapping from sampled colours to DMX slot values.
//!
//! Both files are TOML (`docs/dmx.md`). A **fixture type** lists the DMX
//! channels of one pixel by role:
//!
//! ```toml
//! # fixtures/my-par.toml
//! name = "My RGBW PAR"
//! channels = ["dimmer", "red", "green", "blue", "white"]
//! ```
//!
//! An LED strip is a fixture type whose `channels` are one pixel's
//! (`["green", "red", "blue"]` for WS2812); the patch gives the pixel
//! count. A **patch** places fixtures:
//!
//! ```toml
//! [[fixture]]
//! id = "strip1"
//! type = "pixel-grb"
//! universe = 1
//! address = 1
//! count = 60
//! line = [[0.0, 0.9], [1.0, 0.9]]
//! ```
//!
//! Reading files is the caller's business ([`load_patch`] reads; call it off
//! the render thread). [`Mapping`] turns a patch into sample cells for the
//! GPU and a flat list of slot writes, so filling universes each frame is
//! one loop with no allocation (rule 2).

use std::path::Path;

use toml_edit::{DocumentMut, Item, TableLike, Value};

/// Highest DMX512 slot number.
pub const SLOTS: usize = 512;

/// What one DMX channel of a fixture carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Intensity: full for a fixture with colour channels (the colour
    /// channels carry the image), the sample's luminance for a dimmer-only
    /// fixture.
    Dimmer,
    Red,
    Green,
    Blue,
    /// `min(r, g, b)`, taken out of the colour channels (white extraction).
    White,
    /// Always 0 (amber is not derived from RGB).
    Amber,
    /// The patch's `pan` (0–1, default 0.5).
    Pan,
    /// The patch's `tilt` (0–1, default 0.5).
    Tilt,
    /// Always full: shutter open, strobe off on most fixtures.
    Open,
    /// Always 0.
    None,
}

impl Role {
    pub const NAMES: [(&'static str, Role); 10] = [
        ("dimmer", Role::Dimmer),
        ("red", Role::Red),
        ("green", Role::Green),
        ("blue", Role::Blue),
        ("white", Role::White),
        ("amber", Role::Amber),
        ("pan", Role::Pan),
        ("tilt", Role::Tilt),
        ("open", Role::Open),
        ("none", Role::None),
    ];

    pub fn parse(name: &str) -> Option<Role> {
        Self::NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
    }

    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, r)| *r == self)
            .map_or("none", |(n, _)| n)
    }

    fn is_colour(self) -> bool {
        matches!(self, Role::Red | Role::Green | Role::Blue | Role::White)
    }
}

/// A fixture type: the channels of one pixel, in DMX order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureType {
    pub name: String,
    pub channels: Vec<Role>,
}

/// The built-in generic fixture types, by ID.
pub const BUILTIN: &[(&str, &str, &[Role])] = {
    use Role::*;
    &[
        ("dimmer", "Generic dimmer", &[Dimmer]),
        ("rgb", "Generic RGB", &[Red, Green, Blue]),
        ("drgb", "Generic dimmer + RGB", &[Dimmer, Red, Green, Blue]),
        ("rgbw", "Generic RGBW", &[Red, Green, Blue, White]),
        (
            "drgbw",
            "Generic dimmer + RGBW",
            &[Dimmer, Red, Green, Blue, White],
        ),
        (
            "rgbwa",
            "Generic RGBW + amber",
            &[Red, Green, Blue, White, Amber],
        ),
        ("pixel-rgb", "LED pixel, RGB order", &[Red, Green, Blue]),
        (
            "pixel-grb",
            "LED pixel, GRB order (WS2812)",
            &[Green, Red, Blue],
        ),
        (
            "pixel-rgbw",
            "LED pixel, RGBW order (SK6812)",
            &[Red, Green, Blue, White],
        ),
        (
            "moving-head",
            "Generic moving head: pan, tilt, dimmer, RGB, shutter",
            &[Pan, Tilt, Dimmer, Red, Green, Blue, Open],
        ),
    ]
};

/// A built-in fixture type by ID.
pub fn builtin(id: &str) -> Option<FixtureType> {
    BUILTIN
        .iter()
        .find(|(i, _, _)| *i == id)
        .map(|(_, name, ch)| FixtureType {
            name: (*name).to_owned(),
            channels: ch.to_vec(),
        })
}

fn parse_doc(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>()
        .map_err(|e| e.to_string().trim_end().to_owned())
}

fn only_keys(t: &dyn TableLike, keys: &[&str], at: &str) -> Result<(), String> {
    for (k, _) in t.iter() {
        if !keys.contains(&k) {
            return Err(format!(
                "{at}: unknown key `{k}` (expected {})",
                keys.join(", ")
            ));
        }
    }
    Ok(())
}

/// Parses a fixture type file.
pub fn parse_fixture(text: &str) -> Result<FixtureType, String> {
    let doc = parse_doc(text)?;
    let root = doc.as_table();
    only_keys(root, &["name", "channels"], "the file")?;
    let name = root
        .get("name")
        .and_then(Item::as_str)
        .ok_or("`name` must be a string")?
        .to_owned();
    let list = root
        .get("channels")
        .and_then(Item::as_array)
        .ok_or("`channels` must be a list of roles")?;
    let mut channels = Vec::with_capacity(list.len());
    for v in list {
        let s = v.as_str().ok_or("`channels` must be a list of roles")?;
        let role = Role::parse(s).ok_or_else(|| {
            let names: Vec<&str> = Role::NAMES.iter().map(|(n, _)| *n).collect();
            format!("`{s}` is not a channel role ({})", names.join(", "))
        })?;
        channels.push(role);
    }
    if channels.is_empty() || channels.len() > SLOTS {
        return Err(format!("`channels` must list 1 to {SLOTS} roles"));
    }
    Ok(FixtureType { name, channels })
}

/// Where a patched fixture's pixels sample the image, in normalised image
/// coordinates (0–1, origin top-left).
#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    /// Every pixel samples one point.
    Point([f32; 2]),
    /// Pixels evenly spaced along a line, each averaging its share of it.
    Line([f32; 2], [f32; 2]),
    /// A grid of `cols × rows` cells over the rectangle, each averaging its
    /// cell, row by row from the top-left; `snake` reverses every other row
    /// (serpentine wiring).
    Rect {
        rect: [f32; 4],
        cols: u32,
        rows: u32,
        snake: bool,
    },
}

/// One fixture in the patch.
#[derive(Debug, Clone, PartialEq)]
pub struct Patched {
    /// Stable ID, unique in the patch.
    pub id: String,
    /// Fixture type ID (`fixtures/<id>.toml` or a built-in).
    pub type_id: String,
    pub fixture: FixtureType,
    /// Art-Net port address or sACN universe of the first pixel.
    pub universe: u16,
    /// DMX slot (1–512) of the first pixel's first channel.
    pub address: u16,
    /// Pixels.
    pub count: u32,
    pub region: Region,
    pub pan: f32,
    pub tilt: f32,
}

/// A patch: fixtures in file order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    pub fixtures: Vec<Patched>,
}

fn number(v: &Value) -> Option<f32> {
    v.as_float()
        .or_else(|| v.as_integer().map(|i| i as f64))
        .map(|f| f as f32)
}

fn floats<const N: usize>(item: &Item, what: &str) -> Result<[f32; N], String> {
    let a = item
        .as_array()
        .ok_or_else(|| format!("`{what}` must be a list of {N} numbers"))?;
    let v: Vec<f32> = a.iter().filter_map(number).collect();
    if v.len() != N || a.len() != N {
        return Err(format!("`{what}` must be a list of {N} numbers"));
    }
    let mut out = [0.0; N];
    out.copy_from_slice(&v);
    Ok(out)
}

fn int(
    t: &dyn TableLike,
    key: &str,
    default: i64,
    range: (i64, i64),
    at: &str,
) -> Result<i64, String> {
    match t.get(key) {
        None => Ok(default),
        Some(item) => item
            .as_integer()
            .filter(|v| (range.0..=range.1).contains(v))
            .ok_or_else(|| {
                format!(
                    "{at}: `{key}` must be an integer from {} to {}",
                    range.0, range.1
                )
            }),
    }
}

/// Parses a patch; `lookup` finds a fixture type by ID.
pub fn parse_patch(
    text: &str,
    mut lookup: impl FnMut(&str) -> Result<FixtureType, String>,
) -> Result<Patch, String> {
    let doc = parse_doc(text)?;
    let root = doc.as_table();
    only_keys(root, &["fixture"], "the file")?;
    let mut patch = Patch::default();
    let Some(list) = root.get("fixture") else {
        return Ok(patch);
    };
    let list = list
        .as_array_of_tables()
        .ok_or("`fixture` must be an array of tables ([[fixture]])")?;
    for (i, t) in list.iter().enumerate() {
        let at = format!("fixture {}", i + 1);
        only_keys(
            t,
            &[
                "id", "type", "universe", "address", "count", "point", "line", "rect", "grid",
                "snake", "pan", "tilt",
            ],
            &at,
        )?;
        let id = t
            .get("id")
            .and_then(Item::as_str)
            .ok_or_else(|| format!("{at}: `id` must be a string"))?
            .to_owned();
        let at = format!("fixture `{id}`");
        if patch.fixtures.iter().any(|f| f.id == id) {
            return Err(format!("{at}: the ID is used twice"));
        }
        let type_id = t
            .get("type")
            .and_then(Item::as_str)
            .ok_or_else(|| format!("{at}: `type` must be a fixture type ID"))?
            .to_owned();
        let fixture = lookup(&type_id).map_err(|e| format!("{at}: {e}"))?;
        let universe = int(t, "universe", 1, (0, 63_999), &at)? as u16;
        let address = int(t, "address", 1, (1, SLOTS as i64), &at)? as u16;
        let unit = |key: &str| -> Result<f32, String> {
            match t.get(key) {
                None => Ok(0.5),
                Some(v) => v
                    .as_value()
                    .and_then(number)
                    .filter(|v| (0.0..=1.0).contains(v))
                    .ok_or_else(|| format!("{at}: `{key}` must be a number from 0 to 1")),
            }
        };
        let (pan, tilt) = (unit("pan")?, unit("tilt")?);
        let shapes = ["point", "line", "rect"]
            .iter()
            .filter(|k| t.contains_key(k))
            .count();
        if shapes != 1 {
            return Err(format!("{at}: give exactly one of `point`, `line`, `rect`"));
        }
        let (region, count) = if let Some(p) = t.get("point") {
            let count = int(t, "count", 1, (1, 65_536), &at)? as u32;
            (
                Region::Point(floats::<2>(p, "point").map_err(|e| format!("{at}: {e}"))?),
                count,
            )
        } else if let Some(l) = t.get("line") {
            let a = l
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or_else(|| format!("{at}: `line` must be [[x0, y0], [x1, y1]]"))?;
            let end = |v: Option<&Value>| -> Result<[f32; 2], String> {
                let p = v
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(number).collect::<Vec<_>>())
                    .filter(|p| p.len() == 2)
                    .ok_or_else(|| format!("{at}: `line` must be [[x0, y0], [x1, y1]]"))?;
                Ok([p[0], p[1]])
            };
            let count = int(t, "count", 1, (1, 65_536), &at)? as u32;
            (Region::Line(end(a.get(0))?, end(a.get(1))?), count)
        } else {
            let rect = floats::<4>(&t["rect"], "rect").map_err(|e| format!("{at}: {e}"))?;
            let (cols, rows) = match t.get("grid") {
                Some(g) => {
                    let g = floats::<2>(g, "grid").map_err(|e| format!("{at}: {e}"))?;
                    if g.iter().any(|v| *v < 1.0 || v.fract() != 0.0) {
                        return Err(format!(
                            "{at}: `grid` must be [columns, rows], each at least 1"
                        ));
                    }
                    (g[0] as u32, g[1] as u32)
                }
                None => (int(t, "count", 1, (1, 65_536), &at)? as u32, 1),
            };
            if t.contains_key("grid") && t.contains_key("count") {
                return Err(format!(
                    "{at}: `grid` sets the pixel count; leave out `count`"
                ));
            }
            let snake = match t.get("snake") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| format!("{at}: `snake` must be true or false"))?,
            };
            let count = cols
                .checked_mul(rows)
                .filter(|c| *c <= 65_536)
                .ok_or_else(|| format!("{at}: at most 65536 pixels"))?;
            (
                Region::Rect {
                    rect,
                    cols,
                    rows,
                    snake,
                },
                count,
            )
        };
        if t.contains_key("snake") && !matches!(region, Region::Rect { .. }) {
            return Err(format!("{at}: `snake` applies to `rect` only"));
        }
        patch.fixtures.push(Patched {
            id,
            type_id,
            fixture,
            universe,
            address,
            count,
            region,
            pan,
            tilt,
        });
    }
    Ok(patch)
}

/// Parses patch text, looking fixture types up in `fixtures_dir`
/// (`<type>.toml`) and then among the built-ins. Reads files: call it off
/// the render thread.
pub fn load_patch(text: &str, fixtures_dir: &Path) -> Result<Patch, String> {
    parse_patch(text, |id| {
        let valid = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(format!(
                "`{id}`: fixture type IDs use letters, digits, `-` and `_`"
            ));
        }
        let file = fixtures_dir.join(format!("{id}.toml"));
        match std::fs::read_to_string(&file) {
            Ok(text) => parse_fixture(&text).map_err(|e| format!("fixtures/{id}.toml: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => builtin(id).ok_or_else(|| {
                let names: Vec<&str> = BUILTIN.iter().map(|(i, _, _)| *i).collect();
                format!(
                    "no fixture type `{id}` (not in fixtures/, not built in: {})",
                    names.join(", ")
                )
            }),
            Err(e) => Err(format!("fixtures/{id}.toml: {e}")),
        }
    })
}

/// One DMX slot written from one sample.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Write {
    /// Index into [`Mapping::universes`].
    universe: u16,
    /// Slot index, 0-based.
    slot: u16,
    sample: u32,
    role: Role,
    /// The fixture has colour channels (dimmer at full) and a white channel
    /// (white extraction).
    colour: bool,
    white: bool,
    /// Pan or tilt value.
    fixed: f32,
}

/// A patch compiled for sampling: the sample cells and the slot writes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapping {
    /// Sample cells (centre x, centre y, half width, half height) in
    /// normalised image coordinates, one per pixel, in patch order.
    pub cells: Vec<[f32; 4]>,
    /// The patched universes, ascending, and the number of slots used in
    /// each (from slot 1 to the highest written).
    pub universes: Vec<(u16, usize)>,
    writes: Vec<Write>,
}

impl Mapping {
    /// Lays the patch out. A pixel whose channels would run past slot 512
    /// starts the next universe at slot 1 (pixels are never split across
    /// universes); two fixtures writing the same slot are an error.
    pub fn new(patch: &Patch) -> Result<Self, String> {
        let mut cells = Vec::new();
        // (universe, slot) pairs, checked for overlaps.
        let mut raw: Vec<(u16, u16, u32, Role, bool, bool, f32)> = Vec::new();
        for f in &patch.fixtures {
            let n = f.fixture.channels.len();
            if n > SLOTS {
                return Err(format!("fixture `{}`: more than {SLOTS} channels", f.id));
            }
            let colour = f.fixture.channels.iter().any(|r| r.is_colour());
            let white = f.fixture.channels.contains(&Role::White);
            let (mut universe, mut slot) = (f.universe, usize::from(f.address) - 1);
            for i in 0..f.count {
                if slot + n > SLOTS {
                    universe = universe
                        .checked_add(1)
                        .filter(|u| *u <= 63_999)
                        .ok_or_else(|| {
                            format!("fixture `{}`: runs past the last universe", f.id)
                        })?;
                    slot = 0;
                }
                let sample = cells.len() as u32;
                cells.push(cell(&f.region, i, f.count));
                for (c, role) in f.fixture.channels.iter().enumerate() {
                    let fixed = match role {
                        Role::Pan => f.pan,
                        Role::Tilt => f.tilt,
                        _ => 0.0,
                    };
                    raw.push((
                        universe,
                        (slot + c) as u16,
                        sample,
                        *role,
                        colour,
                        white,
                        fixed,
                    ));
                }
                slot += n;
            }
        }
        let mut seen: Vec<(u16, u16)> = raw.iter().map(|w| (w.0, w.1)).collect();
        seen.sort_unstable();
        if let Some(w) = seen.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!(
                "universe {} slot {} is patched twice",
                w[0].0,
                w[0].1 + 1
            ));
        }
        let mut universes: Vec<(u16, usize)> = Vec::new();
        for &(u, s) in &seen {
            match universes.iter_mut().find(|(x, _)| *x == u) {
                Some((_, len)) => *len = (*len).max(usize::from(s) + 1),
                None => universes.push((u, usize::from(s) + 1)),
            }
        }
        universes.sort_unstable();
        let writes = raw
            .into_iter()
            .map(|(u, slot, sample, role, colour, white, fixed)| Write {
                universe: universes.iter().position(|(x, _)| *x == u).unwrap_or(0) as u16,
                slot,
                sample,
                role,
                colour,
                white,
                fixed,
            })
            .collect();
        Ok(Self {
            cells,
            universes,
            writes,
        })
    }

    /// Writes every slot from the sampled colours (RGBA 0–1, one per cell)
    /// into `out`, one buffer per universe of [`Self::universes`] (each at
    /// least that universe's length; other slots are left alone). No
    /// allocation.
    pub fn fill(&self, colours: &[[f32; 4]], out: &mut [Vec<u8>]) {
        for w in &self.writes {
            let c = colours.get(w.sample as usize).copied().unwrap_or_default();
            let value = role_value(w, c);
            if let Some(slot) = out
                .get_mut(usize::from(w.universe))
                .and_then(|u| u.get_mut(usize::from(w.slot)))
            {
                *slot = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
}

fn role_value(w: &Write, c: [f32; 4]) -> f32 {
    let [r, g, b, _] = c.map(|v| v.clamp(0.0, 1.0));
    let white = if w.white { r.min(g).min(b) } else { 0.0 };
    match w.role {
        Role::Dimmer if w.colour => 1.0,
        Role::Dimmer => 0.2126 * r + 0.7152 * g + 0.0722 * b,
        Role::Red => r - white,
        Role::Green => g - white,
        Role::Blue => b - white,
        Role::White => white,
        Role::Amber | Role::None => 0.0,
        Role::Pan | Role::Tilt => w.fixed,
        Role::Open => 1.0,
    }
}

/// The sample cell of pixel `i` of `count` in `region`.
fn cell(region: &Region, i: u32, count: u32) -> [f32; 4] {
    match *region {
        Region::Point([x, y]) => [x, y, 0.0, 0.0],
        Region::Line([x0, y0], [x1, y1]) => {
            let n = count.max(1) as f32;
            let t = (i as f32 + 0.5) / n;
            [
                x0 + (x1 - x0) * t,
                y0 + (y1 - y0) * t,
                (x1 - x0).abs() / (2.0 * n),
                (y1 - y0).abs() / (2.0 * n),
            ]
        }
        Region::Rect {
            rect: [x, y, w, h],
            cols,
            rows,
            snake,
        } => {
            let (cols, rows) = (cols.max(1), rows.max(1));
            let row = i / cols;
            let mut col = i % cols;
            if snake && row % 2 == 1 {
                col = cols - 1 - col;
            }
            let (cw, ch) = (w / cols as f32, h / rows as f32);
            [
                x + cw * (col as f32 + 0.5),
                y + ch * (row.min(rows - 1) as f32 + 0.5),
                cw / 2.0,
                ch / 2.0,
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(id: &str) -> Result<FixtureType, String> {
        builtin(id).ok_or_else(|| format!("no `{id}`"))
    }

    #[test]
    fn fixture_files_parse_and_reject_unknown_roles() {
        let f = parse_fixture(
            "name = \"Par\"\nchannels = [\"dimmer\", \"red\", \"green\", \"blue\"]\n",
        )
        .unwrap();
        assert_eq!(
            f.channels,
            [Role::Dimmer, Role::Red, Role::Green, Role::Blue]
        );
        let e = parse_fixture("name = \"X\"\nchannels = [\"purple\"]\n").unwrap_err();
        assert!(e.contains("`purple` is not a channel role"), "{e}");
        assert!(parse_fixture("name = \"X\"\nchannels = []\n").is_err());
        assert!(parse_fixture("name = \"X\"\nchannels = [\"red\"]\nextra = 1\n").is_err());
        for (id, _, ch) in BUILTIN {
            assert!(!ch.is_empty(), "{id}");
            assert_eq!(builtin(id).unwrap().channels, *ch);
        }
    }

    #[test]
    fn patch_regions_lay_out_cells_and_slots() {
        let text = r#"
[[fixture]]
id = "par"
type = "drgbw"
universe = 2
address = 10
point = [0.25, 0.5]

[[fixture]]
id = "strip"
type = "pixel-grb"
universe = 1
address = 1
count = 4
line = [[0.0, 0.5], [1.0, 0.5]]

[[fixture]]
id = "wall"
type = "pixel-rgb"
universe = 3
rect = [0.0, 0.0, 1.0, 1.0]
grid = [2, 2]
snake = true
"#;
        let patch = parse_patch(text, lookup).unwrap();
        assert_eq!(patch.fixtures.len(), 3);
        let m = Mapping::new(&patch).unwrap();
        assert_eq!(m.cells.len(), 1 + 4 + 4);
        assert_eq!(m.cells[0], [0.25, 0.5, 0.0, 0.0]);
        assert_eq!(m.cells[1], [0.125, 0.5, 0.125, 0.0]);
        assert_eq!(m.cells[4], [0.875, 0.5, 0.125, 0.0]);
        // Serpentine: the second row runs right to left.
        assert_eq!(m.cells[5], [0.25, 0.25, 0.25, 0.25]);
        assert_eq!(m.cells[7], [0.75, 0.75, 0.25, 0.25]);
        assert_eq!(m.cells[8], [0.25, 0.75, 0.25, 0.25]);
        assert_eq!(m.universes, [(1, 12), (2, 14), (3, 12)]);
    }

    #[test]
    fn colours_map_to_roles_with_white_extraction() {
        let patch = parse_patch(
            "[[fixture]]\nid = \"a\"\ntype = \"drgbw\"\npoint = [0, 0]\n\
             [[fixture]]\nid = \"b\"\ntype = \"dimmer\"\naddress = 6\npoint = [0, 0]\n\
             [[fixture]]\nid = \"c\"\ntype = \"moving-head\"\naddress = 7\npoint = [0, 0]\npan = 0.25\n",
            lookup,
        )
        .unwrap();
        let m = Mapping::new(&patch).unwrap();
        let mut out = vec![vec![0u8; m.universes[0].1]];
        m.fill(&[[1.0, 0.5, 0.25, 1.0]; 3], &mut out);
        // dimmer full, RGB minus white (0.25), white 0.25.
        assert_eq!(&out[0][..5], &[255, 191, 64, 0, 64]);
        // Dimmer-only: Rec. 709 luminance of (1, .5, .25).
        assert_eq!(out[0][5], 150);
        // pan .25, tilt .5, dimmer, r, g, b, open.
        assert_eq!(&out[0][6..13], &[64, 128, 255, 255, 128, 64, 255]);
    }

    #[test]
    fn pixels_wrap_to_the_next_universe_and_overlaps_fail() {
        // 171 RGB pixels: 170 fill universe 1 (510 slots), one goes to 2.
        let patch = parse_patch(
            "[[fixture]]\nid = \"s\"\ntype = \"pixel-rgb\"\ncount = 171\nline = [[0, 0], [1, 0]]\n",
            lookup,
        )
        .unwrap();
        let m = Mapping::new(&patch).unwrap();
        assert_eq!(m.universes, [(1, 510), (2, 3)]);
        let clash = parse_patch(
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\npoint = [0, 0]\n\
             [[fixture]]\nid = \"b\"\ntype = \"rgb\"\naddress = 3\npoint = [0, 0]\n",
            lookup,
        )
        .unwrap();
        let e = Mapping::new(&clash).unwrap_err();
        assert_eq!(e, "universe 1 slot 3 is patched twice");
        for bad in [
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\n",
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\npoint = [0, 0]\nline = [[0, 0], [1, 1]]\n",
            "[[fixture]]\nid = \"a\"\ntype = \"nope\"\npoint = [0, 0]\n",
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\naddress = 513\npoint = [0, 0]\n",
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\npoint = [0, 0]\n[[fixture]]\nid = \"a\"\ntype = \"rgb\"\naddress = 4\npoint = [0, 0]\n",
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\npoint = [0, 0]\nsnake = true\n",
        ] {
            assert!(parse_patch(bad, lookup).is_err(), "{bad}");
        }
    }

    #[test]
    fn project_fixtures_override_builtins() {
        let dir = std::env::temp_dir().join(format!("atd-fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("rgb.toml"),
            "name = \"Mine\"\nchannels = [\"blue\", \"red\"]\n",
        )
        .unwrap();
        let p = load_patch(
            "[[fixture]]\nid = \"a\"\ntype = \"rgb\"\npoint = [0, 0]\n\
             [[fixture]]\nid = \"b\"\ntype = \"rgbw\"\naddress = 10\npoint = [0, 0]\n",
            &dir,
        )
        .unwrap();
        assert_eq!(p.fixtures[0].fixture.name, "Mine");
        assert_eq!(p.fixtures[1].fixture.channels.len(), 4);
        assert!(
            load_patch(
                "[[fixture]]\nid = \"a\"\ntype = \"../x\"\npoint = [0, 0]\n",
                &dir
            )
            .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
