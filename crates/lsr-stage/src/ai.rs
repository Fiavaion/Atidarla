//! AI clips (FR-A5): Prompt Clip nodes (`ai.prompt`) played on a deck feed
//! the Prompt Blend of the AI nodes that follow the deck (`prompts_from` =
//! the mixer's name), with the crossfader as the blend.
//!
//! A Prompt Clip carries a prompt, negative prompt, seed, LoRA and how its
//! seed changes (on launch, every beat or every bar). The clip playing on
//! the deck's crossfader side A becomes prompt 1, side B prompt 2, weighted
//! `1 - crossfader` and `crossfader`; without crossfader groups, the lowest
//! and next playing layers stand in for A and B. The values go to the AI
//! node as live values (not edits, not saved), so a crossfade is as smooth
//! as the crossfader. The feed is worked out once per network revision
//! ([`Feeds::read`]); a frame ([`Feeds::frame`]) only picks values that are
//! already built, so it allocates nothing.

use lsr_core::Registry;
use lsr_core::graph::{Network, NodeId};
use serde_json::Value;

use crate::{Deck, Stage};

/// The Prompt Clip operator.
pub const PROMPT_CLIP: &str = "ai.prompt";
/// AI operators whose Prompt Blend a deck can drive.
pub const FOLLOWERS: [&str; 2] = ["ai.diffusion", "ai.video_model"];

/// When a Prompt Clip's seed moves on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reseed {
    /// Its seed as set.
    Off,
    /// One more each time it is launched.
    Launch,
    Beat,
    Bar,
}

/// A Prompt Clip's values, built once per revision.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptClip {
    pub node: NodeId,
    pub prompt: Value,
    pub negative: Value,
    pub seed: i64,
    pub reseed: Reseed,
    pub lora: Value,
    pub lora_weight: Value,
}

/// One AI node following one deck.
#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub follower: NodeId,
    pub mixer: NodeId,
    /// (layer, mixer input, side): side 0 = A, 1 = B, 2 = no group.
    layers: Vec<(NodeId, usize, u8)>,
    /// Prompt Clips by (layer, slot).
    clips: Vec<(NodeId, usize, PromptClip)>,
    crossfader: f64,
}

/// A live value: borrowed from the feed or a number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiveValue<'a> {
    Value(&'a Value),
    Float(f64),
    Int(i64),
}

/// Every feed of a network.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Feeds {
    pub feeds: Vec<Feed>,
    /// Launch counts by layer (for `Reseed::Launch`), with the target last seen.
    launches: Vec<(NodeId, Option<usize>, i64)>,
}

fn reseed(s: &str) -> Reseed {
    match s {
        "launch" => Reseed::Launch,
        "beat" => Reseed::Beat,
        "bar" => Reseed::Bar,
        _ => Reseed::Off,
    }
}

impl Feeds {
    /// Finds the AI nodes that follow a deck and the Prompt Clips on it.
    pub fn read(net: &Network, reg: &Registry, stage: &Stage) -> Self {
        let mut feeds = Vec::new();
        for n in net
            .nodes()
            .filter(|n| FOLLOWERS.contains(&n.type_id.as_str()))
        {
            let follows = super::text(net, reg, n.id, "prompts_from");
            if follows.is_empty() {
                continue;
            }
            let Some(deck) = stage.decks.iter().find(|d| d.name == follows) else {
                continue;
            };
            feeds.push(feed(net, reg, n.id, deck));
        }
        Self {
            feeds,
            launches: Vec::new(),
        }
    }

    /// The live values for each follower at deck position `beat` (beats),
    /// with `crossfader` overriding a deck's stored one when given (a live
    /// value). Calls `apply(follower, param, value)`.
    pub fn frame(
        &mut self,
        beat: f64,
        crossfader: impl Fn(NodeId) -> Option<f64>,
        targets: impl Fn(NodeId) -> Option<usize>,
        mut apply: impl FnMut(NodeId, &'static str, LiveValue<'_>),
    ) {
        for f in &self.feeds {
            for &(layer, ..) in &f.layers {
                let t = targets(layer);
                match self.launches.iter_mut().find(|l| l.0 == layer) {
                    Some(l) if l.1 != t => {
                        l.1 = t;
                        l.2 += 1;
                    }
                    Some(_) => {}
                    None => self.launches.push((layer, t, 0)),
                }
            }
        }
        for f in &self.feeds {
            let x = crossfader(f.mixer).unwrap_or(f.crossfader).clamp(0.0, 1.0);
            // The clip each side plays: grouped layers first, else the
            // lowest playing layers in order.
            let playing = |side: u8| {
                f.layers
                    .iter()
                    .filter(|l| l.2 == side)
                    .find_map(|&(layer, ..)| {
                        let slot = targets(layer)?;
                        f.clips
                            .iter()
                            .find(|c| c.0 == layer && c.1 == slot)
                            .map(|c| (layer, &c.2))
                    })
            };
            let (mut a, mut b) = (playing(0), playing(1));
            if a.is_none() && b.is_none() {
                let mut it = f
                    .layers
                    .iter()
                    .filter(|l| l.2 == 2)
                    .filter_map(|&(layer, ..)| {
                        let slot = targets(layer)?;
                        f.clips
                            .iter()
                            .find(|c| c.0 == layer && c.1 == slot)
                            .map(|c| (layer, &c.2))
                    });
                a = it.next();
                b = it.next();
            }
            let (w1, w2) = match (a, b) {
                (Some(_), Some(_)) => (1.0 - x, x),
                (Some(_), None) => (1.0, 0.0),
                (None, Some(_)) => (0.0, 1.0),
                (None, None) => continue,
            };
            let seed = |(layer, c): (NodeId, &PromptClip)| {
                let launches = self
                    .launches
                    .iter()
                    .find(|l| l.0 == layer)
                    .map_or(0, |l| l.2);
                c.seed
                    + match c.reseed {
                        Reseed::Off => 0,
                        Reseed::Launch => launches,
                        Reseed::Beat => beat.floor() as i64,
                        Reseed::Bar => (beat / 4.0).floor() as i64,
                    }
            };
            // The side with more weight sets the seed, negative and LoRA.
            let main = if w2 > w1 { b.or(a) } else { a.or(b) }.expect("a playing clip");
            let empty = &EMPTY;
            let (p1, p2) = (
                a.map_or(empty, |c| &c.1.prompt),
                b.map_or(empty, |c| &c.1.prompt),
            );
            let id = f.follower;
            apply(id, "prompt1", LiveValue::Value(p1));
            apply(id, "weight1", LiveValue::Float(w1));
            apply(id, "prompt2", LiveValue::Value(p2));
            apply(id, "weight2", LiveValue::Float(w2));
            apply(id, "seed", LiveValue::Int(seed(main)));
            apply(id, "negative", LiveValue::Value(&main.1.negative));
            apply(id, "lora", LiveValue::Value(&main.1.lora));
            apply(id, "lora_weight", LiveValue::Value(&main.1.lora_weight));
        }
    }
}

static EMPTY: Value = Value::String(String::new());

fn feed(net: &Network, reg: &Registry, follower: NodeId, deck: &Deck) -> Feed {
    let mut layers = Vec::new();
    let mut clips = Vec::new();
    for l in &deck.layers {
        let side = match super::text(net, reg, deck.mixer, &format!("group{}", l.input)) {
            "a" => 0,
            "b" => 1,
            _ => 2,
        };
        layers.push((l.node, l.input, side));
        for c in &l.clips {
            let Some(n) = net.node(c.source) else {
                continue;
            };
            if n.type_id != PROMPT_CLIP {
                continue;
            }
            let p = |name: &str| {
                super::param(net, reg, c.source, name)
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            clips.push((
                l.node,
                c.slot,
                PromptClip {
                    node: c.source,
                    prompt: p("prompt"),
                    negative: p("negative"),
                    seed: super::int(net, reg, c.source, "seed"),
                    reseed: reseed(super::text(net, reg, c.source, "reseed")),
                    lora: p("lora"),
                    lora_weight: p("lora_weight"),
                },
            ));
        }
    }
    // Lowest mixer input first.
    layers.sort_by_key(|l| l.1);
    Feed {
        follower,
        mixer: deck.mixer,
        layers,
        clips,
        crossfader: deck.crossfader,
    }
}
