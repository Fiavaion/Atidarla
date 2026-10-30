//! Stage performance layer (PRD §7.3, FR-S1, FR-S8; decision A54).
//!
//! The Stage owns no media pipeline and no second data model: it is a view
//! and a controller over the graph the Patch edits.
//!
//! - [`Stage::read`] builds the model from a network: decks are `img.mixer`
//!   nodes, layers are `img.layer` nodes wired into them (directly or
//!   through effects), clips are the nodes wired into a layer's slots, and
//!   columns are slot indices.
//! - [`Launcher`] turns performer actions into graph [`Edit`]s on the
//!   layers' `target` parameter: trigger, toggle, flash (press and
//!   release), one-shot (back after the clip's length), column launch (one
//!   undo step for every layer) and stop. [`Launcher::tick`], called by the
//!   app each frame with the clock's beat position, returns one-shots and
//!   runs the autopilot (FR-S8).
//!
//! Quantised switching and transitions happen inside the layer's cook
//! (`atd-ops-img`), so launches are undoable, saved, recordable and
//! deterministic offline.

use std::collections::BTreeMap;

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use serde_json::{Value, json};

/// Clip slots per layer and layers per deck.
pub const SLOTS: usize = 16;

/// Operator types the Stage reads.
pub const LAYER: &str = "img.layer";
pub const MIXER: &str = "img.mixer";

/// Beats a one-shot clip plays when its slot gives no length.
pub const DEFAULT_ONE_SHOT_BEATS: f64 = 4.0;

/// Upstream hops followed from a mixer input to find its layer through
/// effects.
const MAX_EFFECT_HOPS: usize = 8;

/// What a click on a clip does (FR-S1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchMode {
    /// Plays the clip.
    #[default]
    Trigger,
    /// Plays the clip; launching it again stops the layer.
    Toggle,
    /// Plays the clip while held, then returns to what played before.
    Flash,
    /// Plays the clip for its length, then returns to what played before.
    OneShot,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [Self::Trigger, Self::Toggle, Self::Flash, Self::OneShot];

    pub fn name(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Toggle => "toggle",
            Self::Flash => "flash",
            Self::OneShot => "one_shot",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }
}

/// Per-slot clip settings, from a layer's `slots` parameter.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlotSettings {
    pub mode: LaunchMode,
    /// Clip length in beats; 0 when not given.
    pub beats: f64,
    /// Empty: the source node's name.
    pub label: String,
}

/// Parses a `slots` parameter: one `slot,mode,beats,label` line per slot
/// (the label may contain commas; blank lines and `#` comments are
/// skipped). Returns the settings by slot and a message per line that
/// could not be read.
pub fn parse_slots(text: &str) -> (BTreeMap<usize, SlotSettings>, Vec<String>) {
    let mut slots = BTreeMap::new();
    let mut errors = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(4, ',').map(str::trim);
        let slot = parts.next().and_then(|s| s.parse::<usize>().ok());
        let mode = parts.next().unwrap_or("trigger");
        let beats = parts.next().unwrap_or("0");
        let label = parts.next().unwrap_or("");
        let parsed = (
            slot.filter(|s| *s < SLOTS),
            LaunchMode::parse(if mode.is_empty() { "trigger" } else { mode }),
            if beats.is_empty() {
                Some(0.0)
            } else {
                beats.parse::<f64>().ok().filter(|b| *b >= 0.0)
            },
        );
        match parsed {
            (Some(slot), Some(mode), Some(beats)) => {
                slots.insert(
                    slot,
                    SlotSettings {
                        mode,
                        beats,
                        label: label.to_owned(),
                    },
                );
            }
            _ => errors.push(format!(
                "slots line {}: expected `slot,mode,beats,label` with slot 0–{} and mode trigger, toggle, flash or one_shot: `{line}`",
                n + 1,
                SLOTS - 1
            )),
        }
    }
    (slots, errors)
}

/// Writes settings in the form [`parse_slots`] reads, one line per slot in
/// slot order.
pub fn format_slots(slots: &BTreeMap<usize, SlotSettings>) -> String {
    let mut out = String::new();
    for (slot, s) in slots {
        let beats = if s.beats.fract() == 0.0 {
            format!("{}", s.beats as i64)
        } else {
            format!("{}", s.beats)
        };
        out.push_str(&format!("{slot},{},{beats},{}\n", s.mode.name(), s.label));
    }
    out
}

/// A clip: the node wired into a layer slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub slot: usize,
    pub source: NodeId,
    pub label: String,
    pub mode: LaunchMode,
    /// Length in beats; 0 when not given.
    pub beats: f64,
}

/// A layer of a deck.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub node: NodeId,
    pub name: String,
    /// The mixer input it reaches (`layer{input}`).
    pub input: usize,
    /// The stored `target` (its fallback value when bound to a channel).
    pub target: Option<usize>,
    /// Whether `target` follows a channel or an expression rather than
    /// launches.
    pub target_driven: bool,
    /// The SIG node wired into `clock`, if any.
    pub clock: Option<NodeId>,
    /// Clips by slot, in slot order.
    pub clips: Vec<Clip>,
    /// Problems reading the layer's `slots` parameter.
    pub warnings: Vec<String>,
}

impl Layer {
    pub fn clip(&self, slot: usize) -> Option<&Clip> {
        self.clips.iter().find(|c| c.slot == slot)
    }
}

/// Which column the autopilot launches next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutopilotMode {
    #[default]
    Off,
    Next,
    Random,
    Column(usize),
}

/// When the autopilot moves on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutopilotAfter {
    Beats(u32),
    /// When the longest clip of the playing column ends (its `beats`); the
    /// beat count when no clip there has a length.
    ClipEnd {
        fallback_beats: u32,
    },
}

/// The autopilot's settings, from the mixer (FR-S8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Autopilot {
    pub mode: AutopilotMode,
    pub after: AutopilotAfter,
}

/// A deck: an `img.mixer` and the layers wired into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Deck {
    pub mixer: NodeId,
    pub name: String,
    /// Marked as the deck the Stage shows.
    pub stage: bool,
    /// Layers by mixer input, bottom first.
    pub layers: Vec<Layer>,
    pub crossfader: f64,
    pub master: f64,
    pub autopilot: Autopilot,
}

impl Deck {
    /// Columns the grid shows: one past the highest slot with a clip.
    pub fn columns(&self) -> usize {
        self.layers
            .iter()
            .flat_map(|l| l.clips.iter().map(|c| c.slot + 1))
            .max()
            .unwrap_or(0)
    }

    /// The clock the deck follows: the first layer's (bottom up) that has
    /// one.
    pub fn clock(&self) -> Option<NodeId> {
        self.layers.iter().find_map(|l| l.clock)
    }

    /// The column most layers target (the lowest on a tie), if any layer
    /// plays.
    pub fn current_column(&self) -> Option<usize> {
        let mut counts = [0usize; SLOTS];
        for t in self.layers.iter().filter_map(|l| l.target) {
            counts[t] += 1;
        }
        let best = counts.iter().copied().max().filter(|n| *n > 0)?;
        counts.iter().position(|n| *n == best)
    }

    pub fn layer(&self, node: NodeId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.node == node)
    }
}

/// The Stage model of a network.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stage {
    /// Every deck, in node ID order.
    pub decks: Vec<Deck>,
}

fn param<'a>(net: &'a Network, reg: &'a Registry, node: NodeId, name: &str) -> Option<&'a Value> {
    net.param_value(reg, node, name)
}

fn int(net: &Network, reg: &Registry, node: NodeId, name: &str) -> i64 {
    param(net, reg, node, name)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
        .unwrap_or(0)
}

fn float(net: &Network, reg: &Registry, node: NodeId, name: &str) -> f64 {
    param(net, reg, node, name)
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn text<'a>(net: &'a Network, reg: &'a Registry, node: NodeId, name: &str) -> &'a str {
    param(net, reg, node, name)
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The source of `node`'s input `socket`.
fn source(net: &Network, node: NodeId, socket: &str) -> Option<NodeId> {
    net.inputs_of(node)
        .find(|(name, _)| *name == socket)
        .map(|(_, src)| src.node)
}

/// The layer feeding a mixer input: the source itself, or found upstream
/// through effects with a single wired input.
fn find_layer(net: &Network, mut node: NodeId) -> Option<&Node> {
    for _ in 0..=MAX_EFFECT_HOPS {
        let n = net.node(node)?;
        if n.type_id == LAYER {
            return Some(n);
        }
        let mut wired = net.inputs_of(node);
        let first = wired.next()?;
        if wired.next().is_some() {
            return None;
        }
        node = first.1.node;
    }
    None
}

impl Stage {
    pub fn read(net: &Network, reg: &Registry) -> Self {
        let decks = net
            .nodes()
            .filter(|n| n.type_id == MIXER)
            .map(|m| read_deck(net, reg, m))
            .collect();
        Self { decks }
    }

    /// The deck the Stage shows: the one marked `stage`, else the first.
    pub fn deck(&self) -> Option<&Deck> {
        self.decks
            .iter()
            .find(|d| d.stage)
            .or_else(|| self.decks.first())
    }
}

fn read_deck(net: &Network, reg: &Registry, mixer: &Node) -> Deck {
    let id = mixer.id;
    let mut layers = Vec::new();
    for input in 0..SLOTS {
        let Some(src) = source(net, id, &format!("layer{input}")) else {
            continue;
        };
        if let Some(layer) = find_layer(net, src) {
            layers.push(read_layer(net, reg, layer, input));
        }
    }
    let beats = int(net, reg, id, "autopilot_beats").clamp(1, i64::from(u32::MAX)) as u32;
    let mode = match text(net, reg, id, "autopilot") {
        "next" => AutopilotMode::Next,
        "random" => AutopilotMode::Random,
        "column" => {
            AutopilotMode::Column(int(net, reg, id, "autopilot_column").clamp(0, 15) as usize)
        }
        _ => AutopilotMode::Off,
    };
    let after = match text(net, reg, id, "autopilot_after") {
        "clip_end" => AutopilotAfter::ClipEnd {
            fallback_beats: beats,
        },
        _ => AutopilotAfter::Beats(beats),
    };
    Deck {
        mixer: id,
        name: mixer.name.clone(),
        stage: param(net, reg, id, "stage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        layers,
        crossfader: float(net, reg, id, "crossfader"),
        master: float(net, reg, id, "master"),
        autopilot: Autopilot { mode, after },
    }
}

fn read_layer(net: &Network, reg: &Registry, layer: &Node, input: usize) -> Layer {
    let id = layer.id;
    let (settings, warnings) = parse_slots(text(net, reg, id, "slots"));
    let mut clips = Vec::new();
    for slot in 0..SLOTS {
        let Some(src) = source(net, id, &format!("clip{slot}")) else {
            continue;
        };
        let s = settings.get(&slot).cloned().unwrap_or_default();
        let label = if s.label.is_empty() {
            net.node(src).map(|n| n.name.clone()).unwrap_or_default()
        } else {
            s.label
        };
        clips.push(Clip {
            slot,
            source: src,
            label,
            mode: s.mode,
            beats: s.beats,
        });
    }
    let target = int(net, reg, id, "target");
    let driven = layer
        .params
        .get("target")
        .is_some_and(|p| p.bind.is_some() || p.expr.is_some());
    Layer {
        node: id,
        name: layer.name.clone(),
        input,
        target: usize::try_from(target).ok().filter(|t| *t < SLOTS),
        target_driven: driven,
        clock: source(net, id, "clock"),
        clips,
        warnings,
    }
}

/// A layer's state as its status line reports it (`img.layer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerStatus {
    pub playing: Option<usize>,
    /// A launch waiting for its beat or bar: `Some(None)` is a queued stop.
    pub queued: Option<Option<usize>>,
}

impl LayerStatus {
    /// Reads `playing 2` or `playing none · queued 3`.
    pub fn parse(status: &str) -> Option<Self> {
        let slot = |s: &str| match s.trim() {
            "none" => Some(None),
            n => n.parse::<usize>().ok().map(Some),
        };
        let mut parts = status.split(" · ");
        let playing = slot(parts.next()?.strip_prefix("playing ")?)?;
        let queued = match parts.next() {
            Some(q) => Some(slot(q.strip_prefix("queued ")?)?),
            None => None,
        };
        Some(Self { playing, queued })
    }
}

/// The edit that launches `target` on `layer` (`None` stops it).
pub fn set_target(layer: NodeId, target: Option<usize>) -> Edit {
    let value = target.map_or(json!(-1), |t| json!(t));
    Edit::SetParam {
        node: layer,
        param: "target".into(),
        value: Some(Param::constant(value)),
    }
}

/// A clip played for a while, then the layer returns to `back_to`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Return {
    layer: NodeId,
    slot: usize,
    back_to: Option<usize>,
    /// Beat position to return at; `None` while a flash is held.
    at: Option<f64>,
}

/// Launch logic over a deck: performer actions and the clock in, edits
/// out. Holds the state that is app behaviour rather than graph state
/// (held flashes, pending one-shot returns, the autopilot's timing).
#[derive(Debug, Clone, Default)]
pub struct Launcher {
    returns: Vec<Return>,
    /// The column the autopilot launched last and the beat it did.
    autopilot: Option<(Option<usize>, f64)>,
    rng: u64,
}

impl Launcher {
    /// A clip pressed at beat position `now`: what its launch mode does.
    pub fn press(&mut self, layer: &Layer, slot: usize, now: f64) -> Vec<Edit> {
        let Some(clip) = layer.clip(slot) else {
            return Vec::new();
        };
        match clip.mode {
            LaunchMode::Trigger => {
                self.forget(layer.node);
                vec![set_target(layer.node, Some(slot))]
            }
            LaunchMode::Toggle => {
                self.forget(layer.node);
                let next = (layer.target != Some(slot)).then_some(slot);
                vec![set_target(layer.node, next)]
            }
            LaunchMode::Flash | LaunchMode::OneShot => {
                // Pressed again while it plays: return to the original clip.
                let back_to = self
                    .returns
                    .iter()
                    .find(|r| r.layer == layer.node)
                    .map_or(layer.target, |r| r.back_to);
                self.forget(layer.node);
                let at = (clip.mode == LaunchMode::OneShot).then(|| {
                    let beats = if clip.beats > 0.0 {
                        clip.beats
                    } else {
                        DEFAULT_ONE_SHOT_BEATS
                    };
                    now + beats
                });
                self.returns.push(Return {
                    layer: layer.node,
                    slot,
                    back_to,
                    at,
                });
                vec![set_target(layer.node, Some(slot))]
            }
        }
    }

    /// A clip released: a held flash returns to what played before.
    pub fn release(&mut self, layer: &Layer, slot: usize) -> Vec<Edit> {
        let Some(i) = self
            .returns
            .iter()
            .position(|r| r.layer == layer.node && r.slot == slot && r.at.is_none())
        else {
            return Vec::new();
        };
        let r = self.returns.remove(i);
        if layer.target == Some(slot) {
            vec![set_target(layer.node, r.back_to)]
        } else {
            Vec::new()
        }
    }

    /// Stops one layer.
    pub fn stop(&mut self, layer: &Layer) -> Vec<Edit> {
        self.forget(layer.node);
        vec![set_target(layer.node, None)]
    }

    /// Stops every layer of the deck (one undo step).
    pub fn stop_all(&mut self, deck: &Deck) -> Vec<Edit> {
        self.returns.clear();
        deck.layers
            .iter()
            .map(|l| set_target(l.node, None))
            .collect()
    }

    /// Launches column `column` on every layer of the deck in one undo
    /// step: layers with a clip there play it, the others stop. Layers
    /// already there are left alone; layers whose target follows a channel
    /// are skipped.
    pub fn column(&mut self, deck: &Deck, column: usize, now: f64) -> Vec<Edit> {
        self.autopilot = Some((Some(column), now));
        let mut edits = Vec::new();
        for l in deck.layers.iter().filter(|l| !l.target_driven) {
            let next = l.clip(column).map(|c| c.slot);
            self.forget(l.node);
            if l.target != next {
                edits.push(set_target(l.node, next));
            }
        }
        edits
    }

    /// Whether the launcher acts on the clock without input: a one-shot
    /// or flash waiting to return, or the autopilot running. The app keeps
    /// frames coming while it is.
    pub fn busy(&self) -> bool {
        !self.returns.is_empty() || self.autopilot.is_some()
    }

    fn forget(&mut self, layer: NodeId) {
        self.returns.retain(|r| r.layer != layer);
    }

    /// One frame at beat position `now`: one-shots whose length has passed
    /// return, and the autopilot launches its next column when due.
    pub fn tick(&mut self, deck: &Deck, now: f64) -> Vec<Edit> {
        let mut edits = Vec::new();
        self.returns.retain(|r| {
            let Some(at) = r.at else { return true };
            if now < at {
                return true;
            }
            // Only if nothing else was launched on the layer meanwhile.
            if deck
                .layer(r.layer)
                .is_some_and(|l| l.target == Some(r.slot))
            {
                edits.push(set_target(r.layer, r.back_to));
            }
            false
        });
        edits.extend(self.autopilot_tick(deck, now));
        edits
    }

    fn autopilot_tick(&mut self, deck: &Deck, now: f64) -> Vec<Edit> {
        let columns = deck.columns();
        if deck.autopilot.mode == AutopilotMode::Off || columns == 0 {
            self.autopilot = None;
            return Vec::new();
        }
        let current = deck.current_column();
        let Some((last, since)) = self.autopilot else {
            // Switched on: start counting from now.
            self.autopilot = Some((current, now));
            return Vec::new();
        };
        let playing = last.or(current);
        let wait = match deck.autopilot.after {
            AutopilotAfter::Beats(n) => f64::from(n),
            AutopilotAfter::ClipEnd { fallback_beats } => playing
                .map(|c| {
                    deck.layers
                        .iter()
                        .filter_map(|l| l.clip(c))
                        .map(|c| c.beats)
                        .fold(0.0, f64::max)
                })
                .filter(|b| *b > 0.0)
                .unwrap_or(f64::from(fallback_beats)),
        };
        if now - since < wait {
            return Vec::new();
        }
        let next = match deck.autopilot.mode {
            AutopilotMode::Next => playing.map_or(0, |c| (c + 1) % columns),
            AutopilotMode::Random => self.random_column(columns, playing),
            AutopilotMode::Column(c) => c,
            AutopilotMode::Off => unreachable!("handled above"),
        };
        self.column(deck, next, now)
    }

    /// A column other than `playing` (when there is another), from a fixed
    /// pseudo-random sequence so a set replays the same way.
    fn random_column(&mut self, columns: usize, playing: Option<usize>) -> usize {
        // SplitMix64.
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        match playing {
            Some(p) if columns > 1 => (p + 1 + (z % (columns as u64 - 1)) as usize) % columns,
            _ => (z % columns as u64) as usize,
        }
    }
}

/// The beat position of a clock's channels (`beat` + `beat_phase`), or
/// seconds at 120 BPM when there is no clock.
pub fn beat_position(beat: Option<f32>, beat_phase: Option<f32>, seconds: f64) -> f64 {
    match beat {
        Some(b) => f64::from(b) + f64::from(beat_phase.unwrap_or(0.0)),
        None => seconds * 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atd_core::graph::{Endpoint, Wire};

    struct Fixture {
        reg: Registry,
        net: Network,
    }

    impl Fixture {
        fn new() -> Self {
            let mut reg = Registry::default();
            atd_ops_img::register(&mut reg);
            atd_ops_sig::register(&mut reg);
            Self {
                reg,
                net: Network::default(),
            }
        }

        fn node(&mut self, ty: &str, name: &str, params: &[(&str, Value)]) -> NodeId {
            let id = self.net.alloc_id();
            self.net
                .apply(
                    &self.reg,
                    Edit::AddNode {
                        node: Node::new(id, ty, name),
                    },
                )
                .unwrap();
            for (p, v) in params {
                self.set(id, p, v.clone());
            }
            id
        }

        fn set(&mut self, node: NodeId, p: &str, v: Value) {
            self.net
                .apply(
                    &self.reg,
                    Edit::SetParam {
                        node,
                        param: p.into(),
                        value: Some(Param::constant(v)),
                    },
                )
                .unwrap();
        }

        fn wire(&mut self, from: NodeId, to: NodeId, input: &str) {
            self.net
                .apply(
                    &self.reg,
                    Edit::Connect {
                        wire: Wire::new(Endpoint::new(from, "out"), Endpoint::new(to, input)),
                    },
                )
                .unwrap();
        }

        fn apply(&mut self, edits: Vec<Edit>) {
            for e in edits {
                self.net.apply(&self.reg, e).unwrap();
            }
        }

        fn deck(&self) -> Deck {
            Stage::read(&self.net, &self.reg).deck().unwrap().clone()
        }
    }

    /// Two layers × three columns; layer 1 reaches the mixer through a
    /// Level; slot settings on layer 0; a clock on layer 1.
    fn two_layers() -> (Fixture, [NodeId; 3]) {
        let mut f = Fixture::new();
        let clock = f.node("sig.beat", "clock", &[]);
        let mixer = f.node("img.mixer", "deck", &[]);
        let l0 = f.node(
            "img.layer",
            "backs",
            &[(
                "slots",
                json!("0,trigger,0,Tunnel\n1,one_shot,2,Hit\n# comment\n2,flash,,Strobe, white"),
            )],
        );
        let l1 = f.node("img.layer", "fronts", &[("target", json!(1))]);
        let level = f.node("img.level", "fx", &[]);
        f.wire(l0, mixer, "layer0");
        f.wire(l1, level, "in");
        f.wire(level, mixer, "layer1");
        f.wire(clock, l1, "clock");
        for (layer, slots) in [(l0, [0, 1, 2]), (l1, [0, 1, 4])] {
            for s in slots {
                let c = f.node("img.noise", &format!("n{}_{s}", layer.0), &[]);
                f.wire(c, layer, &format!("clip{s}"));
            }
        }
        (f, [mixer, l0, l1])
    }

    #[test]
    fn reads_decks_layers_clips_and_columns_from_the_graph() {
        let (f, [mixer, l0, l1]) = two_layers();
        let stage = Stage::read(&f.net, &f.reg);
        assert_eq!(stage.decks.len(), 1);
        let deck = stage.deck().unwrap();
        assert_eq!(deck.mixer, mixer);
        assert_eq!(
            deck.layers
                .iter()
                .map(|l| (l.node, l.input))
                .collect::<Vec<_>>(),
            [(l0, 0), (l1, 1)],
            "the second layer is found through its effect"
        );
        assert_eq!(deck.columns(), 5);
        assert_eq!(deck.clock(), deck.layers[1].clock);
        assert!(deck.clock().is_some());
        let backs = &deck.layers[0];
        assert_eq!(backs.target, None);
        assert_eq!(
            backs
                .clips
                .iter()
                .map(|c| (c.slot, c.label.as_str(), c.mode, c.beats))
                .collect::<Vec<_>>(),
            [
                (0, "Tunnel", LaunchMode::Trigger, 0.0),
                (1, "Hit", LaunchMode::OneShot, 2.0),
                (2, "Strobe, white", LaunchMode::Flash, 0.0),
            ]
        );
        assert!(backs.warnings.is_empty(), "{:?}", backs.warnings);
        // Without settings a clip is a trigger named after its node.
        assert_eq!(
            deck.layers[1].clip(4).unwrap().label,
            format!("n{}_4", l1.0)
        );
        assert_eq!(deck.layers[1].target, Some(1));
        assert_eq!(deck.current_column(), Some(1));
        assert_eq!(deck.autopilot.mode, AutopilotMode::Off);
    }

    #[test]
    fn the_marked_deck_is_shown() {
        let (mut f, [mixer, ..]) = two_layers();
        let other = f.node("img.mixer", "deck2", &[("stage", json!(true))]);
        let stage = Stage::read(&f.net, &f.reg);
        assert_eq!(stage.decks.len(), 2);
        assert_eq!(stage.deck().unwrap().mixer, other);
        f.set(other, "stage", json!(false));
        assert_eq!(Stage::read(&f.net, &f.reg).deck().unwrap().mixer, mixer);
    }

    #[test]
    fn slots_parse_report_and_round_trip() {
        let (slots, errors) = parse_slots("3,toggle,8,Drop\n17,trigger,0,x\n1,spin,0,y\n2\n");
        assert_eq!(slots.len(), 2, "{slots:?}");
        assert_eq!(slots[&2], SlotSettings::default());
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("line 2"), "{errors:?}");
        let text = format_slots(&slots);
        assert_eq!(text, "2,trigger,0,\n3,toggle,8,Drop\n");
        assert_eq!(parse_slots(&text).0, slots);
    }

    #[test]
    fn status_lines_parse() {
        assert_eq!(
            LayerStatus::parse("playing 2"),
            Some(LayerStatus {
                playing: Some(2),
                queued: None
            })
        );
        assert_eq!(
            LayerStatus::parse("playing none · queued 3"),
            Some(LayerStatus {
                playing: None,
                queued: Some(Some(3))
            })
        );
        assert_eq!(
            LayerStatus::parse("playing 1 · queued none")
                .unwrap()
                .queued,
            Some(None)
        );
        assert_eq!(LayerStatus::parse("compiling"), None);
    }

    #[test]
    fn trigger_toggle_and_stop() {
        let (mut f, [_, l0, l1]) = two_layers();
        let mut launch = Launcher::default();
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 0, 0.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(0));
        // Toggle: on, then off.
        f.set(l0, "slots", json!("0,toggle,0,"));
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 0, 0.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, None);
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 0, 0.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(0));
        // An empty slot does nothing.
        assert!(launch.press(&deck.layers[0], 9, 0.0).is_empty());
        let deck = f.deck();
        f.apply(launch.stop(deck.layer(l1).unwrap()));
        assert_eq!(f.deck().layer(l1).unwrap().target, None);
        let deck = f.deck();
        let edits = launch.stop_all(&deck);
        assert_eq!(edits.len(), 2);
    }

    #[test]
    fn flash_returns_on_release_and_one_shot_after_its_length() {
        let (mut f, [_, l0, _]) = two_layers();
        let mut launch = Launcher::default();
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 0, 0.0));
        // Flash (slot 2) while held.
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 2, 1.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(2));
        assert!(launch.tick(&f.deck(), 50.0).is_empty(), "held: no return");
        let deck = f.deck();
        f.apply(launch.release(&deck.layers[0], 2));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(0));

        // One-shot (slot 1, 2 beats) launched at beat 10.
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 1, 10.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(1));
        assert!(
            launch.busy(),
            "a return is pending: frames must keep coming"
        );
        assert!(launch.tick(&f.deck(), 11.9).is_empty());
        let edits = launch.tick(&f.deck(), 12.0);
        f.apply(edits);
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(0));
        assert!(!launch.busy());
        assert!(launch.tick(&f.deck(), 13.0).is_empty(), "returned once");

        // A launch meanwhile wins over the return.
        let deck = f.deck();
        f.apply(launch.press(&deck.layers[0], 1, 20.0));
        let deck = f.deck();
        let edits = launch.press(&deck.layers[0], 0, 21.0);
        f.apply(edits);
        assert!(launch.tick(&f.deck(), 30.0).is_empty());
    }

    #[test]
    fn column_launch_is_one_step_for_every_layer() {
        let (mut f, [_, l0, l1]) = two_layers();
        let mut launch = Launcher::default();
        let deck = f.deck();
        let edits = launch.column(&deck, 0, 0.0);
        assert_eq!(edits.len(), 2);
        f.apply(edits);
        let deck = f.deck();
        assert_eq!(deck.layer(l0).unwrap().target, Some(0));
        assert_eq!(deck.layer(l1).unwrap().target, Some(0));
        // Column 4: only the second layer has a clip there; the first stops.
        f.apply(launch.column(&deck, 4, 1.0));
        let deck = f.deck();
        assert_eq!(deck.layer(l0).unwrap().target, None);
        assert_eq!(deck.layer(l1).unwrap().target, Some(4));
        // Nothing to do when already there.
        assert!(launch.column(&deck, 4, 2.0).is_empty());
    }

    #[test]
    fn autopilot_next_random_column_and_clip_end() {
        let (mut f, [mixer, l0, l1]) = two_layers();
        let mut launch = Launcher::default();
        f.set(mixer, "autopilot", json!("next"));
        f.set(mixer, "autopilot_beats", json!(4));
        let deck = f.deck();
        assert_eq!(deck.current_column(), Some(1));
        assert!(launch.tick(&deck, 0.0).is_empty(), "starts counting");
        assert!(launch.busy(), "the autopilot runs");
        assert!(launch.tick(&deck, 3.9).is_empty());
        let edits = launch.tick(&deck, 4.0);
        f.apply(edits);
        let deck = f.deck();
        assert_eq!(deck.layer(l0).unwrap().target, Some(2));
        assert_eq!(deck.layer(l1).unwrap().target, None);
        // Column 3 is empty (every layer stops); wraps after the last.
        for (beat, col) in [(8.0, None), (12.0, Some(4)), (16.0, Some(0))] {
            f.apply(launch.tick(&f.deck(), beat));
            assert_eq!(f.deck().current_column(), col, "beat {beat}");
        }

        // Specific column.
        f.set(mixer, "autopilot", json!("column"));
        f.set(mixer, "autopilot_column", json!(1));
        f.apply(launch.tick(&f.deck(), 20.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(1));

        // Random never repeats the playing column and replays the same way.
        f.set(mixer, "autopilot", json!("random"));
        let mut seq = Vec::new();
        for i in 0..8 {
            let before = f.deck().current_column();
            f.apply(launch.tick(&f.deck(), 24.0 + 4.0 * f64::from(i)));
            let after = f.deck().current_column();
            assert_ne!(before, after, "step {i}");
            seq.push(after);
        }
        let mut again = Launcher::default();
        let mut g = two_layers().0;
        g.set(mixer, "autopilot", json!("column"));
        g.set(mixer, "autopilot_column", json!(1));
        g.set(mixer, "autopilot_beats", json!(4));
        g.apply(again.tick(&g.deck(), 0.0));
        g.apply(again.tick(&g.deck(), 20.0));
        g.set(mixer, "autopilot", json!("random"));
        let replay: Vec<_> = (0..8)
            .map(|i| {
                g.apply(again.tick(&g.deck(), 24.0 + 4.0 * f64::from(i)));
                g.deck().current_column()
            })
            .collect();
        assert_eq!(seq, replay);

        // Clip end: column 1 holds a 2-beat clip on layer 0.
        f.set(mixer, "autopilot", json!("next"));
        f.set(mixer, "autopilot_after", json!("clip_end"));
        let deck = f.deck();
        f.apply(launch.column(&deck, 1, 100.0));
        assert!(launch.tick(&f.deck(), 101.9).is_empty());
        f.apply(launch.tick(&f.deck(), 102.0));
        assert_eq!(f.deck().layer(l0).unwrap().target, Some(2));
        // Column 2 has no length: the beat count (4) applies.
        assert!(launch.tick(&f.deck(), 105.9).is_empty());
        assert!(!launch.tick(&f.deck(), 106.0).is_empty());

        f.set(mixer, "autopilot", json!("off"));
        assert!(launch.tick(&f.deck(), 500.0).is_empty());
    }

    #[test]
    fn driven_targets_are_left_to_their_channel() {
        let (mut f, [_, _, l1]) = two_layers();
        let clock = f.deck().clock().unwrap();
        f.net
            .apply(
                &f.reg,
                Edit::SetParam {
                    node: l1,
                    param: "target".into(),
                    value: Some(Param::bound(json!(0), clock, "beat")),
                },
            )
            .unwrap();
        let deck = f.deck();
        assert!(deck.layer(l1).unwrap().target_driven);
        let mut launch = Launcher::default();
        assert_eq!(launch.column(&deck, 0, 0.0).len(), 1, "only the free layer");
    }

    #[test]
    fn beat_positions() {
        assert_eq!(beat_position(Some(3.0), Some(0.25), 99.0), 3.25);
        assert_eq!(beat_position(None, None, 1.5), 3.0);
    }
}
