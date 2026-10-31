//! Speech to Text (`ai.stt`, FR-A13): transcribes speech from an audio-rate
//! SIG input on an AI Host (adapter `stt.whisper`, faster-whisper).
//!
//! Each cook (render thread) mixes the new block to mono, resamples it to
//! 16 kHz and gates it by level: speech goes into an utterance buffer (with
//! a quarter second of pre-roll so the first syllable is kept), and once
//! the speaker pauses for `Hang` the utterance is handed to the
//! [`SttLink`] worker, which sends it to the host. Transcripts come back
//! asynchronously; the node puts each one on its DAT `text` output, also
//! publishes it under its `Publish As` name ([`crate::text`]) and pulses
//! `new_text`. Every buffer is sized when the
//! node is created for the longest utterance, so the cook allocates nothing
//! after that (rule 2) and never waits for the host (rule 1).

use atd_ai::link::LinkState;
use atd_ai::router::Route;
use atd_ai::speech::{SttConfig, SttLink};
use atd_ai::supervisor::Supervisor;
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{Dat, Params, SigCtx, SigOp};

use crate::text;

/// Whisper's input rate.
pub const RATE: u32 = 16_000;
/// Longest utterance in seconds (the `max_seconds` range).
pub const MAX_SECONDS: i64 = 30;
/// Samples kept before the level crosses the threshold.
const PREROLL: usize = RATE as usize / 4;
const CAPACITY: usize = RATE as usize * MAX_SECONDS as usize;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "ai.stt".into(),
        family: OpFamily::Ai,
        label: "Speech to Text".into(),
        summary: "Transcribes speech from an audio input with Whisper (faster-whisper) on an AI Host. The input is gated by level: what is said is sent once the speaker pauses, and the transcript arrives asynchronously on the DAT `text` output (wire it into LLM Chat's `prompt` input or a Prompt Template); it is also published under `Publish As` for `Prompt From`. Channels: `active` while speech is heard, `busy` while transcribing, `new_text` for one frame when a transcript arrives, `count`, `level` and `ready` (a host has the model loaded). Needs a Whisper model from the model manager and the host's `stt` extra; `builtin.stt_echo` runs anywhere for testing.".into(),
        version: 1,
        params: vec![
            ParamSchema::string("route", "Route", "local").help(
                "Where the node runs, in order of preference: `local`, `lan:<name>` and `cloud:<name>` separated by `→`, `->` or commas. The first ready host with the `stt.whisper` adapter wins. Cloud hosts are skipped in Sovereign mode.",
            ),
            ParamSchema::string("model", "Model", "whisper-base").help(
                "Model id from the model manager (`whisper-base`, `whisper-small`), a path to a faster-whisper model folder, or `builtin.stt_echo` (describes the audio instead of transcribing it; for tests).",
            ),
            ParamSchema::string("language", "Language", "").help(
                "Spoken language as an ISO code (`en`, `de`, `fr`, …); empty detects it for each utterance.",
            ),
            ParamSchema::menu("task", "Task", &["transcribe", "translate"], "transcribe")
                .help("`transcribe` writes what was said; `translate` writes it in English."),
            ParamSchema::string("prompt", "Vocabulary", "").help(
                "Names and words to expect (Whisper's initial prompt), for example the performers' names.",
            ),
            ParamSchema::float("threshold", "Threshold", 0.02, Some(0.0), Some(1.0)).help(
                "Input level (RMS) above which the input counts as speech. Watch the `level` channel while speaking and set it between the room and the voice.",
            ),
            ParamSchema::float("hang_ms", "Hang", 600.0, Some(50.0), Some(5000.0)).help(
                "Milliseconds below the threshold that end an utterance and send it.",
            ),
            ParamSchema::float("min_ms", "Minimum", 250.0, Some(0.0), Some(5000.0)).help(
                "Utterances shorter than this many milliseconds (clicks, coughs) are dropped.",
            ),
            ParamSchema::int("max_seconds", "Maximum", 15, Some(1), Some(MAX_SECONDS)).help(
                "Longest utterance in seconds: longer speech is sent in pieces of this length.",
            ),
            ParamSchema::string("publish", "Publish As", "speech").help(
                "The name each transcript is also published under, for LLM Chat's `Prompt From`. Empty: not published (the `text` output still has it).",
            ),
        ],
        inputs: vec![SocketSchema {
            name: "audio".into(),
            wire: WireType::Sig,
            optional: false,
            feedback: false,
        }],
        outputs: vec![
            SocketSchema {
                name: "out".into(),
                wire: WireType::Sig,
                optional: false,
                feedback: false,
            },
            SocketSchema {
                name: "text".into(),
                wire: WireType::Dat,
                optional: true,
                feedback: false,
            },
        ],
        capabilities: vec![],
        cook_budget_us: 150,
        time_dependent: true,
        sink: true,
    }
}

/// The parameters that decide the link's configuration, as last applied.
#[derive(Debug, Default, PartialEq)]
struct Applied {
    route: String,
    model: String,
    language: String,
    task: String,
    prompt: String,
}

pub struct SttOp {
    supervisor: &'static Supervisor,
    link: Option<SttLink>,
    applied: Applied,
    error: Option<String>,
    /// The utterance being recorded, at 16 kHz.
    utt: Vec<f32>,
    /// Pre-roll ring, at 16 kHz.
    pre: Vec<f32>,
    pre_at: usize,
    pre_len: usize,
    speaking: bool,
    silence_s: f64,
    /// `utt` holds a finished utterance waiting to be handed over.
    closed: bool,
    // Resampler state: fractional output position and the input samples
    // averaged into the next output sample.
    phase: f64,
    acc: f32,
    acc_n: u32,
    seen: u64,
    count: u32,
    level: f32,
}

impl std::fmt::Debug for SttOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttOp").finish_non_exhaustive()
    }
}

pub fn create(supervisor: &'static Supervisor) -> Box<dyn SigOp> {
    Box::new(SttOp {
        supervisor,
        link: None,
        applied: Applied::default(),
        error: None,
        utt: Vec::with_capacity(CAPACITY),
        pre: vec![0.0; PREROLL],
        pre_at: 0,
        pre_len: 0,
        speaking: false,
        silence_s: 0.0,
        closed: false,
        phase: 0.0,
        acc: 0.0,
        acc_n: 0,
        seen: 0,
        count: 0,
        level: 0.0,
    })
}

/// A model given as an id resolves to its folder in the model cache; a
/// path or a built-in is used as it is.
fn model_path(model: &str) -> Option<String> {
    if model.starts_with("builtin.") || model.is_empty() {
        return None;
    }
    let p = std::path::Path::new(model);
    if p.is_absolute() || model.contains('/') || model.contains('\\') {
        return Some(model.to_owned());
    }
    Some(
        atd_ai::models::model_dir(model)
            .to_string_lossy()
            .into_owned(),
    )
}

impl SttOp {
    /// Re-configures the link when a parameter that decides it changed.
    /// Compares in place, so a steady frame allocates nothing.
    fn configure(&mut self, p: &Params<'_>) -> Result<(), String> {
        let a = &self.applied;
        if self.link.is_some()
            && a.route == p.str("route")
            && a.model == p.str("model")
            && a.language == p.str("language")
            && a.task == p.str("task")
            && a.prompt == p.str("prompt")
        {
            return Ok(());
        }
        let route = Route::parse(p.str("route"))?;
        let model = p.str("model").trim();
        if model.is_empty() {
            return Err("name a model: `whisper-base`, `whisper-small` or a folder".into());
        }
        let config = SttConfig {
            route,
            model_id: model.to_owned(),
            adapter: if model.starts_with("builtin.") {
                "builtin"
            } else {
                "stt.whisper"
            }
            .to_owned(),
            path: model_path(model),
            sample_rate: RATE,
            params: serde_json::json!({
                "language": p.str("language").trim(),
                "task": p.str("task"),
                "prompt": p.str("prompt"),
            }),
        };
        match &self.link {
            Some(link) => link.set_config(config),
            None => self.link = Some(SttLink::new(self.supervisor, config, CAPACITY)),
        }
        self.applied = Applied {
            route: p.str("route").to_owned(),
            model: p.str("model").to_owned(),
            language: p.str("language").to_owned(),
            task: p.str("task").to_owned(),
            prompt: p.str("prompt").to_owned(),
        };
        Ok(())
    }

    /// One 16 kHz sample: into the utterance while speaking, else the
    /// pre-roll.
    fn emit(&mut self, y: f32, room: usize) {
        if self.speaking {
            if self.utt.len() < room {
                self.utt.push(y);
            }
        } else {
            self.pre[self.pre_at] = y;
            self.pre_at = (self.pre_at + 1) % PREROLL;
            self.pre_len = (self.pre_len + 1).min(PREROLL);
        }
    }

    /// Speech starts: the pre-roll goes first, oldest sample first.
    fn onset(&mut self, room: usize) {
        if !self.closed {
            self.utt.clear();
        }
        let start = (self.pre_at + PREROLL - self.pre_len) % PREROLL;
        for i in 0..self.pre_len {
            if self.utt.len() < room {
                self.utt.push(self.pre[(start + i) % PREROLL]);
            }
        }
        self.pre_len = 0;
    }

    /// Mixes, gates and records one block of the input.
    fn record(&mut self, input: &atd_gpu::engine::Channels, p: &Params<'_>) {
        let (n, chans, rate) = (input.samples(), input.len(), input.rate());
        if n == 0 || chans == 0 || rate <= 0.0 {
            self.level = 0.0;
            return;
        }
        let mono = |i: usize| -> f32 {
            (0..chans).map(|c| input.channel(c)[i]).sum::<f32>() / chans as f32
        };
        let sumsq: f32 = (0..n)
            .map(|i| {
                let m = mono(i);
                m * m
            })
            .sum();
        self.level = (sumsq / n as f32).sqrt();
        let room = (p.i64("max_seconds").clamp(1, MAX_SECONDS) as usize) * RATE as usize;
        let dur = n as f64 / f64::from(rate);
        if self.level >= p.f32("threshold") {
            if !self.speaking {
                self.speaking = true;
                self.onset(room);
            }
            self.silence_s = 0.0;
        } else if self.speaking {
            self.silence_s += dur;
        }
        let step = f64::from(RATE) / f64::from(rate);
        for i in 0..n {
            self.acc += mono(i);
            self.acc_n += 1;
            self.phase += step;
            if self.phase >= 1.0 {
                let y = self.acc / self.acc_n as f32;
                while self.phase >= 1.0 {
                    self.phase -= 1.0;
                    self.emit(y, room);
                }
                self.acc = 0.0;
                self.acc_n = 0;
            }
        }
        if self.speaking && self.silence_s * 1e3 >= f64::from(p.f32("hang_ms")) {
            self.speaking = false;
            let min = (f64::from(p.f32("min_ms")) / 1e3 * f64::from(RATE)) as usize;
            if self.utt.len() >= min.max(1) {
                self.closed = true;
            } else if !self.closed {
                self.utt.clear();
            }
        }
        if self.speaking && self.utt.len() >= room {
            self.closed = true;
        }
    }
}

impl SigOp for SttOp {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): nothing is sent or published.
            let mut w = ctx.output.writer();
            for name in ["active", "busy", "new_text", "count", "level", "ready"] {
                w.push(name, 0.0);
            }
            return Ok(());
        }
        if let Err(e) = self.configure(&p) {
            self.error = Some(e.clone());
            ctx.output.writer().keep_previous();
            return Err(e);
        }
        if let Some(input) = ctx.inputs.get(0) {
            self.record(input, &p);
        } else {
            self.level = 0.0;
        }
        let link = self.link.as_ref().expect("configured");
        let room = (p.i64("max_seconds").clamp(1, MAX_SECONDS) as usize) * RATE as usize;
        // Hand over a finished utterance once the speaker paused (or the
        // buffer is full); a busy worker leaves it for the next frame.
        if self.closed
            && (!self.speaking || self.utt.len() >= room)
            && link.offer(&self.utt).is_some()
        {
            self.utt.clear();
            self.closed = false;
        }
        let mut new_text = false;
        let publish = p.str("publish");
        let out = ctx.dat.output.as_deref_mut();
        link.take(&mut self.seen, |heard| {
            let said = heard.text.trim();
            if !said.is_empty() {
                text::publish(publish, said);
                if let Some(out) = out {
                    // A new value per transcript (allocates only then):
                    // the same words said twice still reach the chat.
                    *out = Dat::text(said);
                }
                new_text = true;
            }
        });
        if new_text {
            self.count += 1;
        }
        let (ready, blocked) = link.with_status(|s| (s.state == LinkState::Running, s.blocked));
        let busy = link.busy();
        let mut w = ctx.output.writer();
        w.push("active", f32::from(u8::from(self.speaking)));
        w.push("busy", f32::from(u8::from(busy)));
        w.push("new_text", f32::from(u8::from(new_text)));
        w.push("count", self.count as f32);
        w.push("level", self.level);
        w.push("ready", f32::from(u8::from(ready)));
        drop(w);
        self.error = None;
        if blocked {
            return Err(
                "Sovereign mode or the spend cap blocks every host on this node's route: add `local` or a LAN host".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_resolve_to_the_cache_and_paths_stay() {
        assert_eq!(model_path("builtin.stt_echo"), None);
        assert_eq!(model_path("/models/w").as_deref(), Some("/models/w"));
        let dir = model_path("whisper-base").unwrap();
        assert!(dir.ends_with("whisper-base"), "{dir}");
    }

    #[test]
    fn the_schema_declares_sig_first_and_dat_text() {
        let s = schema();
        assert_eq!(s.outputs[0].wire, WireType::Sig);
        assert_eq!(s.outputs[1].wire, WireType::Dat);
        assert!(s.sink && s.time_dependent && s.cook_budget_us > 0);
    }
}
