//! In-show cues (FR-G5): a performer's words ("darker, slower on the next
//! drop") become parameter moves within Show Lock bounds, quantised to the
//! clock.
//!
//! A cue only ever moves numeric parameters on the show's whitelist
//! (`[[show_lock.allow]]` in `agent.toml`, engaged or not), clamped to the
//! narrower of the parameter's declared range and the whitelist's. A cue is
//! never a graph edit: a [`Move`] is a node, a parameter and a number, and
//! nothing else can be expressed.
//!
//! Planning is deterministic first: a small lexicon (darker/brighter,
//! slower/faster, calmer/wilder, warmer/cooler, bigger/smaller, more/less
//! *x*, "set *x* to *n*") matched against the whitelisted parameters by
//! name. When the lexicon finds nothing, a language model may propose moves
//! through the same typed schema ([`CueContext::plan_with`]); every
//! proposal is validated the same way, and anything outside the whitelist
//! is dropped. A [`CueContext`] owns its data, so planning (with a model,
//! which can take seconds) runs on any thread, never the render thread.
//!
//! Timing: "now" fires on the next beat, the default on the next bar (4
//! beats), "on the next drop" on the next 16-beat boundary and "next
//! phrase" on the next 32-beat one, in `atd_stage::beat_position` beats.
//! A host keeps a [`CueQueue`] and applies [`CueQueue::due`] moves as the
//! clock passes them, each one undo step ([`apply_move`]).

use atd_core::Registry;
use atd_core::graph::{Edit, Network, Param};
use atd_core::registry::{ParamKind, ParamSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assistant::Llm;
use crate::lock::{Allow, ShowLock};
use crate::workspace::Workspace;

/// When a cue fires: the next boundary of this many beats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantum {
    /// The next beat ("now").
    Beat,
    /// The next bar, 4 beats (the default).
    Bar,
    /// The next drop, a 16-beat boundary.
    Drop,
    /// The next phrase, a 32-beat boundary.
    Phrase,
}

impl Quantum {
    pub fn beats(self) -> f64 {
        match self {
            Self::Beat => 1.0,
            Self::Bar => 4.0,
            Self::Drop => 16.0,
            Self::Phrase => 32.0,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "beat" | "now" => Self::Beat,
            "bar" => Self::Bar,
            "drop" => Self::Drop,
            "phrase" => Self::Phrase,
            _ => return None,
        })
    }

    /// The timing a cue's words ask for (default: the next bar).
    pub fn of_text(text: &str) -> Self {
        let t = format!(" {} ", tokens(text).join(" "));
        let has = |w: &str| t.contains(&format!(" {w} "));
        if has("drop") || has("drops") {
            Self::Drop
        } else if has("phrase") {
            Self::Phrase
        } else if has("now") || has("immediately") || has("beat") {
            Self::Beat
        } else {
            Self::Bar
        }
    }

    /// The first boundary strictly after `now` (a cue never fires in the
    /// past or on the beat it was given).
    pub fn next_boundary(self, now: f64) -> f64 {
        let q = self.beats();
        let now = if now.is_finite() { now } else { 0.0 };
        ((now / q).floor() + 1.0) * q
    }
}

/// One planned parameter move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Move {
    pub node: String,
    pub param: String,
    pub target: f64,
    pub fire_at_beat: f64,
}

/// What a cue turned into.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub quantum: Quantum,
    pub fire_at_beat: f64,
    /// Why parts of the cue did nothing (for the performer).
    pub notes: Vec<String>,
}

/// A parameter a cue may move: whitelisted, numeric, not driven by an
/// expression or a binding.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Control {
    pub node: String,
    pub param: String,
    pub current: f64,
    /// The narrower of the declared and the whitelisted bounds.
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// An integer parameter (targets are rounded).
    pub int: bool,
}

impl Control {
    fn of(node: &str, schema: &ParamSchema, value: Option<&Param>, rule: &Allow) -> Option<Self> {
        let (smin, smax, int) = match &schema.kind {
            ParamKind::Float { min, max } => (*min, *max, false),
            ParamKind::Int { min, max } => (min.map(|v| v as f64), max.map(|v| v as f64), true),
            _ => return None,
        };
        if value.is_some_and(|p| p.expr.is_some() || p.bind.is_some()) {
            return None;
        }
        let current = value
            .map_or(&schema.default, |p| &p.value)
            .as_f64()
            .filter(|v| v.is_finite())?;
        let pick = |a: Option<f64>, b: Option<f64>, f: fn(f64, f64) -> f64| match (a, b) {
            (Some(a), Some(b)) => Some(f(a, b)),
            (a, b) => a.or(b),
        };
        let min = pick(smin, rule.min, f64::max);
        let max = pick(smax, rule.max, f64::min);
        if let (Some(lo), Some(hi)) = (min, max)
            && lo > hi
        {
            return None;
        }
        Some(Self {
            node: node.to_owned(),
            param: schema.name.clone(),
            current,
            min,
            max,
            int,
        })
    }

    /// `v` clamped to the bounds (rounded for integers); `None` if not a number.
    pub fn clamp(&self, v: f64) -> Option<f64> {
        if !v.is_finite() {
            return None;
        }
        let v = v
            .max(self.min.unwrap_or(f64::MIN))
            .min(self.max.unwrap_or(f64::MAX));
        Some(if self.int { v.round() } else { v })
    }

    /// One relative step from `v` (`dir` ±1, `size` a fraction of the range,
    /// or of the value when the range is open).
    fn step(&self, v: f64, dir: f64, size: f64) -> f64 {
        let span = match (self.min, self.max) {
            (Some(lo), Some(hi)) => hi - lo,
            _ => v.abs().max(1.0),
        };
        let d = dir * size * span;
        // An integer moves at least one step.
        let d = if self.int && d.abs() < 1.0 { dir } else { d };
        v + d
    }
}

/// The controls a cue may move, captured from the show (owned, so a plan
/// can be made off the thread that owns the network).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CueContext {
    pub controls: Vec<Control>,
}

/// The control `node.param` of `net`, if a cue may move it.
pub fn control(
    net: &Network,
    registry: &Registry,
    lock: &ShowLock,
    node: &str,
    param: &str,
) -> Option<Control> {
    let n = net.node_by_name(node)?;
    let op = registry.get(&n.type_id)?;
    let pack = atd_core::pack::pack_name(&n.type_id);
    let schema = op.params.iter().find(|p| p.name == param)?;
    let rule = lock.rule(&n.name, pack, param)?;
    Control::of(&n.name, schema, n.params.get(param), rule)
}

/// Words that move parameters, and the parameter names they move.
const BRIGHT: &[&str] = &[
    "brightness",
    "exposure",
    "gain",
    "intensity",
    "level",
    "opacity",
];
const SPEED: &[&str] = &["speed", "rate", "tempo", "frequency", "freq"];
const ENERGY: &[&str] = &[
    "amount",
    "strength",
    "intensity",
    "energy",
    "chaos",
    "turbulence",
    "displacement",
    "distortion",
    "speed",
];
const WARM: &[&str] = &["warmth", "temperature"];
const SIZE: &[&str] = &["scale", "size", "radius", "zoom"];
const GENERIC: &[&str] = &["amount", "intensity", "mix", "strength"];

const LEXICON: &[(&[&str], f64, &[&str])] = &[
    (&["darker", "dimmer", "darken", "dim"], -1.0, BRIGHT),
    (&["brighter", "lighter", "brighten"], 1.0, BRIGHT),
    (&["slower", "slow down"], -1.0, SPEED),
    (&["faster", "quicker", "speed up"], 1.0, SPEED),
    (&["calmer", "calm", "softer", "gentler"], -1.0, ENERGY),
    (&["wilder", "crazier", "harder", "intense"], 1.0, ENERGY),
    (&["warmer"], 1.0, WARM),
    (&["cooler", "colder"], -1.0, WARM),
    (&["bigger", "larger", "zoom in"], 1.0, SIZE),
    (&["smaller", "zoom out"], -1.0, SIZE),
];

/// Lower-case words, numbers (with an optional `%`) and `node.param` names.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '%' | '-')))
        .filter_map(|raw| {
            let t = raw.trim_end_matches('.');
            let t = if number(t).is_some() {
                t
            } else {
                t.trim_matches(|c| c == '.' || c == '-')
            };
            (!t.is_empty()).then(|| t.to_owned())
        })
        .collect()
}

/// A number token: the value and whether it was a percentage.
fn number(t: &str) -> Option<(f64, bool)> {
    let (n, pct) = match t.strip_suffix('%') {
        Some(n) => (n, true),
        None => (t, false),
    };
    n.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| (v, pct))
}

/// Whether parameter `param` is what `word` names.
fn names(param: &str, word: &str) -> bool {
    let p = param.to_ascii_lowercase();
    p == word
        || p.split('_').any(|part| part == word)
        || word.strip_suffix('s').is_some_and(|w| p == w)
}

enum Hit {
    /// A relative step up (+1) or down (−1).
    Step(f64),
    /// An absolute value (a percentage of the range when `.1`).
    To(f64, bool),
}

impl CueContext {
    /// Every control in `net` a cue may move.
    pub fn capture(net: &Network, registry: &Registry, lock: &ShowLock) -> Self {
        let mut controls: Vec<Control> = net
            .nodes()
            .filter_map(|n| registry.get(&n.type_id).map(|op| (n, op)))
            .flat_map(|(n, op)| {
                op.params
                    .iter()
                    .filter_map(|p| control(net, registry, lock, &n.name, &p.name))
            })
            .collect();
        controls.sort_by(|a, b| (&a.node, &a.param).cmp(&(&b.node, &b.param)));
        Self { controls }
    }

    fn find(&self, node: &str, param: &str) -> Option<&Control> {
        self.controls
            .iter()
            .find(|c| c.node == node && c.param == param)
    }

    /// `target` for `node.param`, validated: the control must exist (be
    /// whitelisted and numeric) and the value is clamped to its bounds.
    pub fn admit(&self, node: &str, param: &str, target: f64) -> Result<f64, String> {
        let c = self
            .find(node, param)
            .ok_or_else(|| format!("{node}.{param} is not a whitelisted parameter"))?;
        c.clamp(target)
            .ok_or_else(|| format!("{node}.{param}: {target} is not a number"))
    }

    /// Plans a cue with the lexicon alone. `quantum` overrides the timing
    /// the words ask for.
    pub fn plan(&self, text: &str, now_beat: f64, quantum: Option<Quantum>) -> Plan {
        let quantum = quantum.unwrap_or_else(|| Quantum::of_text(text));
        let fire = quantum.next_boundary(now_beat);
        if self.controls.is_empty() {
            return Plan {
                moves: Vec::new(),
                quantum,
                fire_at_beat: fire,
                notes: vec![
                    "nothing is whitelisted for cues: add [[show_lock.allow]] entries to agent.toml"
                        .to_owned(),
                ],
            };
        }
        let mut notes = Vec::new();
        let hits = self.hits(text, &mut notes);
        let mut values: Vec<f64> = self.controls.iter().map(|c| c.current).collect();
        let size = step_size(text);
        for (idx, hit) in &hits {
            for &i in idx {
                let c = &self.controls[i];
                let v = match hit {
                    Hit::Step(dir) => c.step(values[i], *dir, size),
                    Hit::To(v, true) => match (c.min, c.max) {
                        (Some(lo), Some(hi)) => lo + v / 100.0 * (hi - lo),
                        _ => {
                            notes.push(format!(
                                "{}.{} has no range for a percentage",
                                c.node, c.param
                            ));
                            continue;
                        }
                    },
                    Hit::To(v, false) => *v,
                };
                if let Some(v) = c.clamp(v) {
                    values[i] = v;
                }
            }
        }
        let mut moves = Vec::new();
        for (i, c) in self.controls.iter().enumerate() {
            if !hits.iter().any(|(idx, _)| idx.contains(&i)) {
                continue;
            }
            if (values[i] - c.current).abs() < 1e-9 {
                notes.push(format!("{}.{} is already at its limit", c.node, c.param));
                continue;
            }
            moves.push(Move {
                node: c.node.clone(),
                param: c.param.clone(),
                target: values[i],
                fire_at_beat: fire,
            });
        }
        if hits.is_empty() {
            notes.push("nothing in the cue matches a whitelisted parameter".to_owned());
        }
        Plan {
            moves,
            quantum,
            fire_at_beat: fire,
            notes,
        }
    }

    /// The lexicon's matches: which controls move, and how.
    fn hits(&self, text: &str, notes: &mut Vec<String>) -> Vec<(Vec<usize>, Hit)> {
        let toks = tokens(text);
        let joined = format!(" {} ", toks.join(" "));
        // Naming a node limits the relative words to it.
        let named: Vec<&str> = self
            .controls
            .iter()
            .map(|c| c.node.as_str())
            .filter(|n| {
                let n = n.to_lowercase();
                toks.iter()
                    .any(|t| *t == n || t.split_once('.').is_some_and(|(a, _)| a == n))
            })
            .collect();
        let scoped = |c: &Control| named.is_empty() || named.contains(&c.node.as_str());
        let by_names = |words: &[&str]| -> Vec<usize> {
            (0..self.controls.len())
                .filter(|&i| {
                    let c = &self.controls[i];
                    scoped(c) && words.iter().any(|w| names(&c.param, w))
                })
                .collect()
        };
        let mut hits = Vec::new();

        // "set x to n", "node.x to n", "x at 50%".
        for (i, t) in toks.iter().enumerate() {
            if !matches!(t.as_str(), "to" | "at") || i == 0 {
                continue;
            }
            let Some((v, pct)) = toks.get(i + 1).and_then(|n| number(n)) else {
                continue;
            };
            let what = &toks[i - 1];
            let idx: Vec<usize> = match what.split_once('.') {
                Some((node, param)) => (0..self.controls.len())
                    .filter(|&j| {
                        let c = &self.controls[j];
                        c.node.to_lowercase() == node && names(&c.param, param)
                    })
                    .collect(),
                None => by_names(&[what.as_str()]),
            };
            if idx.is_empty() {
                notes.push(format!("`{what}`: no whitelisted parameter by that name"));
            } else {
                hits.push((idx, Hit::To(v, pct)));
            }
        }

        // "more x", "less x" (bare: the generic amount-like parameters).
        for (i, t) in toks.iter().enumerate() {
            let dir = match t.as_str() {
                "more" => 1.0,
                "less" => -1.0,
                _ => continue,
            };
            let next = toks.get(i + 1).map(String::as_str);
            let lexical = next.is_some_and(|n| LEXICON.iter().any(|(w, _, _)| w.contains(&n)));
            let idx = match next {
                Some(n) if !lexical && number(n).is_none() => {
                    let idx = by_names(&[n]);
                    if idx.is_empty() {
                        notes.push(format!("`{n}`: no whitelisted parameter by that name"));
                        continue;
                    }
                    idx
                }
                _ => by_names(GENERIC),
            };
            if !idx.is_empty() {
                hits.push((idx, Hit::Step(dir)));
            }
        }

        for (words, dir, params) in LEXICON {
            let said = words.iter().any(|w| joined.contains(&format!(" {w} ")));
            if !said {
                continue;
            }
            let idx = by_names(params);
            if idx.is_empty() {
                notes.push(format!(
                    "`{}`: no whitelisted parameter it moves ({})",
                    words[0],
                    params.join(", ")
                ));
            } else {
                hits.push((idx, Hit::Step(*dir)));
            }
        }
        hits
    }

    /// Plans a cue with the lexicon and, when it finds nothing, asks `llm`
    /// for moves through the `propose_moves` schema. Every proposal is
    /// validated ([`Self::admit`]); the rest are dropped with a note. Call
    /// it off the render thread: the model may take seconds.
    pub fn plan_with(
        &self,
        text: &str,
        now_beat: f64,
        quantum: Option<Quantum>,
        llm: &mut dyn Llm,
    ) -> Plan {
        let mut plan = self.plan(text, now_beat, quantum);
        if !plan.moves.is_empty() || self.controls.is_empty() {
            return plan;
        }
        let messages = [
            json!({"role": "system", "content": self.prompt()}),
            json!({"role": "user", "content": text}),
        ];
        let tools = [propose_moves_tool()];
        let answer = match llm.chat(&messages, &tools) {
            Ok(a) => a,
            Err(e) => {
                plan.notes.push(format!("the model failed: {e}"));
                return plan;
            }
        };
        let mut moves: Vec<Move> = Vec::new();
        for call in answer["tool_calls"].as_array().into_iter().flatten() {
            if call["name"] != "propose_moves" {
                plan.notes.push(format!(
                    "the model called {}, which is not offered",
                    call["name"]
                ));
                continue;
            }
            let args = match &call["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                v => v.clone(),
            };
            for m in args["moves"].as_array().into_iter().flatten() {
                let (Some(node), Some(param), Some(target)) = (
                    m["node"].as_str(),
                    m["param"].as_str(),
                    m["target"].as_f64(),
                ) else {
                    plan.notes.push(format!("dropped a malformed move: {m}"));
                    continue;
                };
                match self.admit(node, param, target) {
                    Ok(v) => {
                        moves.retain(|o| !(o.node == node && o.param == param));
                        moves.push(Move {
                            node: node.to_owned(),
                            param: param.to_owned(),
                            target: v,
                            fire_at_beat: plan.fire_at_beat,
                        });
                    }
                    Err(e) => plan.notes.push(format!("dropped: {e}")),
                }
            }
        }
        moves.sort_by(|a, b| (&a.node, &a.param).cmp(&(&b.node, &b.param)));
        plan.moves = moves;
        plan
    }

    fn prompt(&self) -> String {
        let list = serde_json::to_string(&self.controls).unwrap_or_default();
        format!(
            "You turn a live visual performer's cue into parameter moves. You may only move \
             these parameters, within min and max (JSON: node, param, current, min, max): \
             {list}. Call propose_moves once with the moves (absolute targets). Never invent \
             nodes or parameters; if the cue asks for anything else, propose no moves."
        )
    }
}

/// The one tool a model planning a cue is offered.
fn propose_moves_tool() -> Value {
    json!({"type": "function", "function": {
        "name": "propose_moves",
        "description": "Proposes parameter moves for the cue.",
        "parameters": {
            "type": "object",
            "properties": {"moves": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "node": {"type": "string"},
                    "param": {"type": "string"},
                    "target": {"type": "number"}
                },
                "required": ["node", "param", "target"],
                "additionalProperties": false
            }}},
            "required": ["moves"],
            "additionalProperties": false
        }
    }})
}

/// How big a relative step the words ask for (a fraction of the range).
fn step_size(text: &str) -> f64 {
    let t = format!(" {} ", tokens(text).join(" "));
    let has = |w: &str| t.contains(&format!(" {w} "));
    if ["a bit", "slightly", "a little", "a touch", "bit"]
        .iter()
        .any(|w| has(w))
    {
        0.1
    } else if ["much", "lot", "way", "very", "really"]
        .iter()
        .any(|w| has(w))
    {
        0.5
    } else {
        0.25
    }
}

/// Moves waiting for their beat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CueQueue {
    pending: Vec<Move>,
}

impl CueQueue {
    /// Queues moves; a newer move for a parameter replaces a pending one.
    pub fn push(&mut self, moves: impl IntoIterator<Item = Move>) {
        for m in moves {
            self.pending
                .retain(|o| !(o.node == m.node && o.param == m.param));
            self.pending.push(m);
        }
    }

    /// Takes the moves due at `beat` (fire beat at or before it), in firing
    /// order.
    pub fn due(&mut self, beat: f64) -> Vec<Move> {
        let (mut due, rest): (Vec<Move>, Vec<Move>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|m| m.fire_at_beat <= beat + 1e-9);
        self.pending = rest;
        due.sort_by(|a, b| {
            a.fire_at_beat
                .total_cmp(&b.fire_at_beat)
                .then_with(|| (&a.node, &a.param).cmp(&(&b.node, &b.param)))
        });
        due
    }

    pub fn pending(&self) -> &[Move] {
        &self.pending
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Applies one due move as one undo step, checked again against the show
/// as it is now (the network or the lock may have changed since the cue was
/// planned). Returns the value set.
pub fn apply_move(ws: &mut dyn Workspace, lock: &ShowLock, m: &Move) -> Result<f64, String> {
    let c = control(ws.network(), ws.registry(), lock, &m.node, &m.param)
        .ok_or_else(|| format!("{}.{} is not a whitelisted parameter", m.node, m.param))?;
    let v = c
        .clamp(m.target)
        .ok_or_else(|| format!("{}.{}: not a number", m.node, m.param))?;
    let net = ws.network();
    let node = net.node_by_name(&m.node).expect("control found it");
    let pack = atd_core::pack::pack_name(&node.type_id);
    lock.check_param(&node.name, pack, &m.param, Some(v))
        .map_err(|e| e.to_string())?;
    let value = if c.int { json!(v as i64) } else { json!(v) };
    let edit = Edit::SetParam {
        node: node.id,
        param: m.param.clone(),
        value: Some(Param::constant(value)),
    };
    ws.apply(&format!("Cue: {}.{} = {v}", m.node, m.param), vec![edit])?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantisation_fires_on_the_next_boundary() {
        assert_eq!(Quantum::Bar.next_boundary(5.3), 8.0);
        assert_eq!(Quantum::Bar.next_boundary(8.0), 12.0);
        assert_eq!(Quantum::Beat.next_boundary(5.3), 6.0);
        assert_eq!(Quantum::Drop.next_boundary(5.3), 16.0);
        assert_eq!(Quantum::Drop.next_boundary(16.0), 32.0);
        assert_eq!(Quantum::Phrase.next_boundary(33.0), 64.0);
        assert_eq!(Quantum::Bar.next_boundary(f64::NAN), 4.0);
        // Beats as atd_stage reads them from a clock.
        let now = atd_stage::beat_position(Some(5.0), Some(0.3), 0.0);
        assert_eq!(Quantum::Bar.next_boundary(now), 8.0);
    }

    #[test]
    fn the_words_choose_the_timing() {
        assert_eq!(Quantum::of_text("darker"), Quantum::Bar);
        assert_eq!(Quantum::of_text("darker now!"), Quantum::Beat);
        assert_eq!(
            Quantum::of_text("darker, slower on the next drop"),
            Quantum::Drop
        );
        assert_eq!(Quantum::of_text("bigger next phrase"), Quantum::Phrase);
        assert_eq!(Quantum::of_text("Wilder on the next bar."), Quantum::Bar);
    }

    #[test]
    fn tokens_keep_numbers_and_dotted_names() {
        assert_eq!(
            tokens("Set level1.brightness to 0.5."),
            ["set", "level1.brightness", "to", "0.5"]
        );
        assert_eq!(tokens("speed at 50%!"), ["speed", "at", "50%"]);
        assert_eq!(tokens("to -1"), ["to", "-1"]);
    }

    #[test]
    fn the_queue_releases_moves_in_order() {
        let m = |node: &str, beat: f64| Move {
            node: node.into(),
            param: "p".into(),
            target: 1.0,
            fire_at_beat: beat,
        };
        let mut q = CueQueue::default();
        q.push([m("b", 8.0), m("a", 8.0), m("c", 16.0)]);
        assert!(q.due(7.99).is_empty());
        let due = q.due(8.0);
        assert_eq!(
            due.iter().map(|m| m.node.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        // A newer cue for the same parameter replaces the pending one.
        q.push([m("c", 12.0)]);
        assert_eq!(q.pending().len(), 1);
        assert_eq!(q.due(100.0)[0].fire_at_beat, 12.0);
        assert!(q.is_empty());
    }
}
