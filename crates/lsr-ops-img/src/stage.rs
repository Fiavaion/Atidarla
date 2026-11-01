//! Stage operators (M3, decision A54): `img.layer` plays one of its clip
//! slots and switches between them, `img.mixer` composites layers into a
//! deck. Launching a clip is an edit of the layer's `target` parameter; the
//! layer switches inside its own cook, at once or at the next beat or bar
//! of its `clock` input, so launches are undoable, saved and deterministic
//! offline. Launch modes, column launch and autopilot are app behaviour
//! over `target` (`lsr-stage`).

use std::fmt::Write as _;

use lsr_core::registry::{OperatorSchema, ParamSchema, SocketSchema, WireType};
use lsr_gpu::engine::{CookCtx, GpuOp, Params};
use lsr_gpu::fullscreen::FullscreenPass;

use crate::ops::{COMPOSITE_OPS, COMPOSITE_WGSL, img};

/// Clip slots per layer and layers per mixer.
pub const SLOTS: usize = 16;

/// Tempo assumed for beat lengths when a layer has no clock.
const DEFAULT_BPM: f64 = 120.0;

pub(crate) const TRANSITIONS: &[&str] = &["cut", "fade", "wipe"];
const UNITS: &[&str] = &["beats", "seconds"];
pub(crate) const QUANTISE: &[&str] = &["off", "beat", "bar"];
const GROUPS: &[&str] = &["none", "a", "b"];
pub(crate) const AUTOPILOT: &[&str] = &["off", "next", "random", "column"];
const AUTOPILOT_AFTER: &[&str] = &["beats", "clip_end"];

fn index(p: &Params<'_>, name: &str, options: &[&str]) -> usize {
    let v = p.str(name);
    options.iter().position(|o| *o == v).unwrap_or(0)
}

pub fn layer_schema() -> OperatorSchema {
    let names: Vec<String> = (0..SLOTS).map(|i| format!("clip{i}")).collect();
    let inputs: Vec<(&str, bool)> = names.iter().map(|n| (n.as_str(), false)).collect();
    let mut s = img(
        "layer",
        "Layer",
        "A Stage layer: plays the clip wired into slot `target` and switches to a new target with a cut, fade or wipe, at once or on the next beat or bar of its clock. Clips are any IMG nodes wired into `clip0`…`clip15`. Clips not shown do not cook, so a stateful clip (feedback, a simulation, a movie) pauses while hidden.",
        &inputs,
        vec![
            ParamSchema::int("target", "Target", -1, Some(-1), Some(SLOTS as i64 - 1))
                .help("The clip slot to play; −1 plays nothing. Launching a clip sets this (an undoable edit); the layer switches when quantise allows."),
            ParamSchema::menu("transition", "Transition", TRANSITIONS, "cut")
                .help("How the layer changes clip: `cut` at once, `fade` through the blend, `wipe` from left to right."),
            ParamSchema::float("transition_length", "Transition Length", 1.0, Some(0.0), None)
                .help("Length of a fade or wipe, in Transition Unit; 0 cuts."),
            ParamSchema::menu("transition_unit", "Transition Unit", UNITS, "beats")
                .help("Beats follow the clock input's tempo (120 BPM without a clock); seconds are wall-clock (frame time offline)."),
            ParamSchema::menu("quantise", "Quantise", QUANTISE, "off")
                .help("`beat` or `bar` waits for the next boundary of the clock input's `beat` or `bar` channel before switching; `off`, or no clock, switches in the same frame."),
            ParamSchema::float("opacity", "Opacity", 1.0, Some(0.0), Some(1.0))
                .help("Multiplies the layer's alpha."),
            ParamSchema::menu("blend", "Transition Blend", COMPOSITE_OPS, "over")
                .help("How the incoming clip combines with the outgoing one half-way through a fade (`over` is a plain crossfade for opaque clips). Blending onto the layers below is set on the Mixer."),
            ParamSchema::toggle("bypass", "Bypass", false)
                .help("Outputs nothing (transparent) while on; launches still queue and switch."),
            ParamSchema::string("slots", "Slots", "")
                .multiline()
                .help("Clip settings for the Stage, one line per slot: `slot,mode,beats,label`, e.g. `2,one_shot,4,Strobe hit`. Modes: trigger, toggle, flash, one_shot; beats is the clip length (one-shot return, autopilot clip end). Slots without a line are trigger clips named after their source node."),
        ],
        80,
    );
    s.inputs.push(SocketSchema {
        name: "clock".into(),
        wire: WireType::Sig,
        optional: true,
        feedback: false,
    });
    s
}

pub fn mixer_schema() -> OperatorSchema {
    let names: Vec<String> = (0..SLOTS).map(|i| format!("layer{i}")).collect();
    let inputs: Vec<(&str, bool)> = names.iter().map(|n| (n.as_str(), false)).collect();
    let mut params = vec![
        ParamSchema::toggle("stage", "Stage Deck", false)
            .help("The deck the Stage tab shows (the first Mixer when none is marked)."),
        ParamSchema::float("crossfader", "Crossfader", 0.5, Some(0.0), Some(1.0))
            .help("0 shows group A, 1 group B; both at full in the middle. Layers in no group ignore it."),
        ParamSchema::float("master", "Master", 1.0, Some(0.0), Some(1.0))
            .help("Multiplies the composite's colour (fade to black)."),
        ParamSchema::menu("autopilot", "Autopilot", AUTOPILOT, "off")
            .help("The Stage launches columns by itself: `next` in order, `random`, or always `column` Autopilot Column (FR-S8). App behaviour, like one-shot returns and flash releases: none of them run in `lasair render`."),
        ParamSchema::menu("autopilot_after", "Autopilot After", AUTOPILOT_AFTER, "beats")
            .help("When the autopilot moves on: after Autopilot Beats, or when the longest clip of the column ends (its length in the layer's slots)."),
        ParamSchema::int("autopilot_beats", "Autopilot Beats", 16, Some(1), Some(1024))
            .help("Beats between autopilot launches."),
        ParamSchema::int("autopilot_column", "Autopilot Column", 0, Some(0), Some(SLOTS as i64 - 1))
            .help("The column a `column` autopilot launches."),
    ];
    for i in 0..SLOTS {
        params.push(
            ParamSchema::float(
                &format!("opacity{i}"),
                &format!("Opacity {i}"),
                1.0,
                Some(0.0),
                Some(1.0),
            )
            .help(&format!("Opacity of layer {i}.")),
        );
        params.push(
            ParamSchema::menu(
                &format!("blend{i}"),
                &format!("Blend {i}"),
                COMPOSITE_OPS,
                "over",
            )
            .help(&format!("How layer {i} combines with the layers below it.")),
        );
        params.push(
            ParamSchema::menu(&format!("group{i}"), &format!("Group {i}"), GROUPS, "none").help(
                &format!("Crossfader group of layer {i}: `a`, `b` or `none`."),
            ),
        );
    }
    img(
        "mixer",
        "Mixer",
        "A Stage deck: composites up to 16 layers bottom (`layer0`) to top, each with an opacity, a blend and a crossfader group, then a master level.",
        &inputs,
        params,
        200,
    )
}

/// The parameter names of mixer input `i`, built once.
struct LayerNames {
    opacity: String,
    blend: String,
    group: String,
}

/// A clock reading from the `clock` input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Clock {
    /// Whole beats counted.
    pub beat: f64,
    /// Whole bars counted.
    pub bar: f64,
    /// Beat position: `beat + beat_phase`.
    pub pos: f64,
}

/// What a layer is set to this cook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Settings {
    pub target: Option<u8>,
    /// Index into [`TRANSITIONS`].
    pub transition: usize,
    pub length: f64,
    pub seconds: bool,
    /// Index into [`QUANTISE`].
    pub quantise: usize,
}

/// The layer's playback state: which slot plays, the transition in
/// progress and a launch waiting for its quantum. Pure, so it is unit
/// tested without a GPU and deterministic given the same cooks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Switcher {
    started: bool,
    pub playing: Option<u8>,
    /// The outgoing slot while a transition runs.
    pub from: Option<u8>,
    /// Transition progress, 1 when none runs.
    pub progress: f64,
    /// A launch waiting for the clock: (target, quantum count when queued).
    pub queued: Option<(Option<u8>, f64)>,
    last_time: Option<f64>,
    last_pos: Option<f64>,
}

impl Default for Switcher {
    fn default() -> Self {
        Self {
            started: false,
            playing: None,
            from: None,
            progress: 1.0,
            queued: None,
            last_time: None,
            last_pos: None,
        }
    }
}

impl Switcher {
    /// Advances one cook at `time`; true while it must cook again next
    /// frame (a transition runs or a launch waits).
    pub fn step(&mut self, s: &Settings, clock: Option<Clock>, time: f64) -> bool {
        let dt = self.last_time.map_or(0.0, |t| (time - t).max(0.0));
        self.last_time = Some(time);
        let beats = match (clock, self.last_pos) {
            // A clock that jumps back (reset) does not rewind a transition.
            (Some(c), Some(p)) => (c.pos - p).max(0.0),
            (Some(_), None) => 0.0,
            (None, _) => dt * DEFAULT_BPM / 60.0,
        };
        self.last_pos = clock.map(|c| c.pos);
        if !self.started {
            // A project opens on its targets, without waiting or fading.
            self.started = true;
            self.playing = s.target;
            return false;
        }
        if self.progress < 1.0 {
            let d = if s.seconds { dt } else { beats };
            self.progress = if s.length > 0.0 {
                (self.progress + d / s.length).min(1.0)
            } else {
                1.0
            };
            if self.progress >= 1.0 {
                self.from = None;
            }
        }
        let count = clock.and_then(|c| match s.quantise {
            1 => Some(c.beat),
            2 => Some(c.bar),
            _ => None,
        });
        let heading = self.queued.map_or(self.playing, |q| q.0);
        if s.target != heading {
            self.queued = None;
            match count {
                Some(n) if s.target != self.playing => self.queued = Some((s.target, n)),
                Some(_) => {}
                None => self.launch(s.target, s),
            }
        }
        if let Some((target, at)) = self.queued {
            // Any change of the count is a boundary (a reset counts too);
            // quantise turned off launches at once.
            if count.is_none_or(|n| n != at) {
                self.queued = None;
                self.launch(target, s);
            }
        }
        self.progress < 1.0 || self.queued.is_some()
    }

    fn launch(&mut self, target: Option<u8>, s: &Settings) {
        if target == self.playing {
            return;
        }
        if s.transition == 0 || s.length <= 0.0 {
            self.from = None;
            self.progress = 1.0;
        } else {
            // A launch during a transition starts from the clip fading in.
            self.from = self.playing;
            self.progress = 0.0;
        }
        self.playing = target;
    }
}

const LAYER_BODY: &str = r#"
    let a = textureSample(in0, samp, uv);
    let b = textureSample(in1, samp, uv);
    let p = params.v[0].x;
    let kind = i32(params.v[0].y);
    var c = b;
    if (kind == 1) {
        // Outgoing → incoming blended onto outgoing → incoming.
        let r = composite(b, a, i32(params.v[0].w));
        c = mix(mix(a, r, p), b, p);
    } else if (kind == 2) {
        c = select(a, b, uv.x < p);
    }
    if (params.v[1].x > 0.5) { return vec4f(0.0); }
    return vec4f(c.rgb, c.a * params.v[0].z);"#;

pub(crate) fn create_layer(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Layer {
        pass: FullscreenPass::new(device, "img.layer", format, 2, COMPOSITE_WGSL, LAYER_BODY),
        state: Switcher::default(),
        status: String::with_capacity(48),
        shown: None,
    })
}

struct Layer {
    pass: FullscreenPass,
    state: Switcher,
    status: String,
    /// (playing, queued) the status line was written for.
    shown: Option<(Option<u8>, Option<Option<u8>>)>,
}

/// `playing 2`, `playing none · queued 3`: the Stage reads this format
/// (`lsr_stage::LayerStatus::parse`).
fn write_status(out: &mut String, playing: Option<u8>, queued: Option<Option<u8>>) {
    out.clear();
    let slot = |out: &mut String, s: Option<u8>| match s {
        Some(i) => {
            let _ = write!(out, "{i}");
        }
        None => out.push_str("none"),
    };
    out.push_str("playing ");
    slot(out, playing);
    if let Some(q) = queued {
        out.push_str(" · queued ");
        slot(out, q);
    }
}

fn read_clock(ctx: &CookCtx<'_>) -> Option<Clock> {
    let ch = ctx.sig_inputs.get(SLOTS)?;
    let beat = f64::from(ch.value("beat")?);
    Some(Clock {
        beat,
        bar: f64::from(ch.value("bar").unwrap_or(0.0)),
        pos: beat + f64::from(ch.value("beat_phase").unwrap_or(0.0)),
    })
}

impl GpuOp for Layer {
    /// Only the clips it shows: the playing one, the outgoing one during a
    /// transition and the target (so a launch shows its first frame in the
    /// same cook, and a quantised launch prerolls), plus the clock. The
    /// other clips do not cook. A `target` bound to a channel or driven by
    /// an expression is not known until its sources cook, so then every
    /// input is read (the value here would be last frame's).
    fn needed_inputs(&self, params: &Params<'_>) -> Option<u64> {
        if params.is_driven("target") {
            return None;
        }
        let bit = |s: Option<u8>| s.map_or(0, |i| 1u64 << i);
        let target = params.i64("target");
        let target = (0..SLOTS as i64).contains(&target).then_some(target as u8);
        let s = &self.state;
        Some(bit(s.playing) | bit(s.from) | bit(target) | 1u64 << SLOTS)
    }

    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        project: (u32, u32),
    ) -> Option<(u32, u32)> {
        Some(project)
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = &ctx.params;
        let target = p.i64("target");
        let settings = Settings {
            target: (0..SLOTS as i64).contains(&target).then_some(target as u8),
            transition: index(p, "transition", TRANSITIONS),
            length: f64::from(p.f32("transition_length")),
            seconds: index(p, "transition_unit", UNITS) == 1,
            quantise: index(p, "quantise", QUANTISE),
        };
        let clock = read_clock(ctx);
        ctx.cook_again |= self.state.step(&settings, clock, ctx.time);
        let s = &self.state;
        let shown = (s.playing, s.queued.map(|q| q.0));
        if self.shown != Some(shown) {
            write_status(&mut self.status, shown.0, shown.1);
            self.shown = Some(shown);
        }
        // Copies of the shared references, so the encoder can be borrowed.
        let (inputs, black) = (ctx.inputs, ctx.black);
        let slot = |i: Option<u8>| {
            i.and_then(|i| inputs.get(usize::from(i))?.as_ref())
                .unwrap_or(black)
        };
        let (from, to) = (slot(s.from), slot(s.playing));
        let kind = if s.progress >= 1.0 {
            0.0
        } else {
            settings.transition as f32
        };
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[from, to],
            &[
                [
                    s.progress as f32,
                    kind,
                    p.f32("opacity"),
                    index(p, "blend", COMPOSITE_OPS) as f32,
                ],
                [if p.bool("bypass") { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            ],
            ctx.output,
        );
        Ok(())
    }

    fn status(&self) -> Option<&str> {
        Some(&self.status)
    }
}

/// The mixer's shader: every input composited in one pass (WGSL cannot
/// index texture bindings, so the loop is unrolled). Layer `i`'s blend and
/// effective opacity arrive packed as `2 × blend + opacity` in
/// `params.v[i / 4][i % 4]`; `params.v[4].x` is the master level.
fn mixer_body() -> String {
    let mut body = String::from("    var acc = vec4f(0.0);\n");
    for i in 0..SLOTS {
        let lane = ["x", "y", "z", "w"][i % 4];
        let _ = writeln!(
            body,
            "    {{ let l = textureSample(in{i}, samp, uv); let x = params.v[{}].{lane}; let k = floor(x * 0.5); let o = x - 2.0 * k; if (o > 0.0) {{ acc = mix(acc, composite(l, acc, i32(k)), o); }} }}",
            i / 4
        );
    }
    body.push_str("    return vec4f(acc.rgb * params.v[4].x, acc.a);");
    body
}

pub(crate) fn create_mixer(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    Box::new(Mixer {
        pass: FullscreenPass::new(
            device,
            "img.mixer",
            format,
            SLOTS,
            COMPOSITE_WGSL,
            &mixer_body(),
        ),
        names: (0..SLOTS)
            .map(|i| LayerNames {
                opacity: format!("opacity{i}"),
                blend: format!("blend{i}"),
                group: format!("group{i}"),
            })
            .collect(),
    })
}

struct Mixer {
    pass: FullscreenPass,
    names: Vec<LayerNames>,
}

/// Crossfader weight of a layer in `group` (index into [`GROUPS`]) at
/// crossfader position `x`: both groups at full in the middle.
pub(crate) fn crossfade(group: usize, x: f32) -> f32 {
    match group {
        1 => (2.0 * (1.0 - x)).clamp(0.0, 1.0),
        2 => (2.0 * x).clamp(0.0, 1.0),
        _ => 1.0,
    }
}

impl GpuOp for Mixer {
    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        project: (u32, u32),
    ) -> Option<(u32, u32)> {
        Some(project)
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        let p = &ctx.params;
        let x = p.f32("crossfader");
        let mut v = [[0f32; 4]; 5];
        let (inputs, black) = (ctx.inputs, ctx.black);
        let mut refs: [&wgpu::Texture; SLOTS] = [black; SLOTS];
        for (i, names) in self.names.iter().enumerate() {
            let Some(t) = inputs.get(i).and_then(Option::as_ref) else {
                continue;
            };
            refs[i] = t;
            let opacity = (p.f32(&names.opacity) * crossfade(index(p, &names.group, GROUPS), x))
                .clamp(0.0, 1.0);
            let blend = index(p, &names.blend, COMPOSITE_OPS) as f32;
            v[i / 4][i % 4] = 2.0 * blend + opacity;
        }
        v[4][0] = p.f32("master");
        self.pass
            .draw(ctx.device, ctx.queue, ctx.encoder, &refs, &v, ctx.output);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(target: i8, transition: usize, length: f64, quantise: usize) -> Settings {
        Settings {
            target: u8::try_from(target).ok(),
            transition,
            length,
            seconds: false,
            quantise,
        }
    }

    fn clock(pos: f64) -> Option<Clock> {
        Some(Clock {
            beat: pos.floor(),
            bar: (pos / 4.0).floor(),
            pos,
        })
    }

    #[test]
    fn opens_on_the_target_then_cuts_at_once() {
        let mut s = Switcher::default();
        assert!(!s.step(&settings(2, 0, 1.0, 0), None, 0.0));
        assert_eq!(s.playing, Some(2));
        assert!(!s.step(&settings(5, 0, 1.0, 0), None, 0.1));
        assert_eq!((s.playing, s.from, s.progress), (Some(5), None, 1.0));
        s.step(&settings(-1, 0, 1.0, 0), None, 0.2);
        assert_eq!(s.playing, None);
    }

    #[test]
    fn quantised_launch_waits_for_the_next_beat_or_bar() {
        let mut s = Switcher::default();
        s.step(&settings(0, 0, 0.0, 1), clock(0.2), 0.0);
        assert!(s.step(&settings(1, 0, 0.0, 1), clock(0.4), 0.1));
        assert_eq!((s.playing, s.queued), (Some(0), Some((Some(1), 0.0))));
        s.step(&settings(1, 0, 0.0, 1), clock(0.9), 0.2);
        assert_eq!(s.playing, Some(0), "same beat: still waiting");
        assert!(!s.step(&settings(1, 0, 0.0, 1), clock(1.05), 0.3));
        assert_eq!((s.playing, s.queued), (Some(1), None), "beat 1 started");

        // Bar quantise waits through beats 1..3 of the bar.
        s.step(&settings(2, 0, 0.0, 2), clock(1.5), 0.4);
        for pos in [2.0, 3.0, 3.99] {
            s.step(&settings(2, 0, 0.0, 2), clock(pos), 0.5);
            assert_eq!(s.playing, Some(1), "beat {pos}");
        }
        s.step(&settings(2, 0, 0.0, 2), clock(4.0), 0.6);
        assert_eq!(s.playing, Some(2));
    }

    #[test]
    fn relaunching_the_playing_clip_cancels_a_queued_one() {
        let mut s = Switcher::default();
        s.step(&settings(0, 0, 0.0, 2), clock(0.0), 0.0);
        s.step(&settings(3, 0, 0.0, 2), clock(0.5), 0.1);
        assert!(s.queued.is_some());
        assert!(!s.step(&settings(0, 0, 0.0, 2), clock(1.0), 0.2));
        assert_eq!((s.playing, s.queued), (Some(0), None));
        // Without a clock, quantise does not wait.
        s.step(&settings(3, 0, 0.0, 2), None, 0.3);
        assert_eq!(s.playing, Some(3));
    }

    #[test]
    fn fades_advance_in_beats_or_seconds() {
        let mut s = Switcher::default();
        s.step(&settings(0, 1, 2.0, 0), clock(0.0), 0.0);
        assert!(s.step(&settings(1, 1, 2.0, 0), clock(0.0), 0.0));
        assert_eq!((s.from, s.playing, s.progress), (Some(0), Some(1), 0.0));
        s.step(&settings(1, 1, 2.0, 0), clock(1.0), 0.5);
        assert!((s.progress - 0.5).abs() < 1e-9, "{}", s.progress);
        assert!(!s.step(&settings(1, 1, 2.0, 0), clock(2.5), 1.0));
        assert_eq!((s.from, s.progress), (None, 1.0));

        // Seconds ignore the clock.
        let mut set = settings(2, 2, 0.5, 0);
        set.seconds = true;
        s.step(&set, clock(2.5), 1.0);
        s.step(&set, clock(10.0), 1.25);
        assert!((s.progress - 0.5).abs() < 1e-9, "{}", s.progress);
        // A launch mid-transition starts from the clip fading in.
        set.target = Some(3);
        s.step(&set, clock(10.0), 1.25);
        assert_eq!((s.from, s.playing, s.progress), (Some(2), Some(3), 0.0));
    }

    #[test]
    fn crossfader_groups() {
        assert_eq!(crossfade(0, 0.0), 1.0);
        assert_eq!((crossfade(1, 0.0), crossfade(2, 0.0)), (1.0, 0.0));
        assert_eq!((crossfade(1, 0.5), crossfade(2, 0.5)), (1.0, 1.0));
        assert_eq!((crossfade(1, 1.0), crossfade(2, 1.0)), (0.0, 1.0));
    }

    #[test]
    fn status_lines() {
        let mut s = String::new();
        write_status(&mut s, Some(2), None);
        assert_eq!(s, "playing 2");
        write_status(&mut s, None, Some(Some(3)));
        assert_eq!(s, "playing none · queued 3");
        write_status(&mut s, Some(1), Some(None));
        assert_eq!(s, "playing 1 · queued none");
    }
}
