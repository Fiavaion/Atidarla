//! LLM Chat (`ai.llm_chat`, FR-A13): sends a prompt to a language model and
//! publishes the reply. Ollama on this machine by default; an
//! OpenAI-compatible server or Anthropic opt in, every request through the
//! egress gate (Sovereign mode refuses Anthropic with a node error).
//!
//! The request runs on a [`ChatLink`] worker thread; the cook only hands
//! over a prompt when asked to send (the `Send` pulse, a rising `trigger`
//! input, a new text under `Prompt From`, or an edited prompt with `On
//! Change`) and polls for the reply (rule 1). Prompt and reply buffers keep
//! their capacity, so a steady frame allocates nothing (rule 2).

use std::time::Duration;

use atd_ai::llm::{ChatConfig, ChatLink, Provider};
use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, SocketSchema, WireType};
use atd_gpu::engine::{Params, SigCtx, SigOp};

use crate::text;

/// Prompts up to this many bytes are handed over without allocating.
const PROMPT_CAPACITY: usize = 16 * 1024;

pub fn schema() -> OperatorSchema {
    OperatorSchema {
        type_id: "ai.llm_chat".into(),
        family: OpFamily::Ai,
        label: "LLM Chat".into(),
        summary: "Sends a prompt to a language model and publishes the reply under `Publish As` (DAT text out when the engine routes DAT). Ollama on this machine by default (`qwen2.5-coder:7b`); an OpenAI-compatible server (llama.cpp, LM Studio, vLLM) or Anthropic opt in. Sends on the `Send` pulse, a rising `trigger` input, each new text under `Prompt From` (a Speech to Text node's transcripts, say) or, with `On Change`, an edited prompt. Asynchronous: channels `busy`, `new_reply` (one frame), `replies` and `error`. Every request goes through Sovereign mode's egress gate: Anthropic and internet endpoints are refused while it is on.".into(),
        version: 1,
        params: vec![
            ParamSchema::menu("provider", "Provider", &["ollama", "openai_compat", "anthropic"], "ollama").help(
                "Who answers: `ollama` (local, the default), `openai_compat` (any server with `/v1/chat/completions`; `OPENAI_API_KEY` from the environment if set) or `anthropic` (internet; needs `ANTHROPIC_API_KEY` in the environment and Sovereign mode off).",
            ),
            ParamSchema::string("endpoint", "Endpoint", "").help(
                "Base URL of the server; empty: the provider's default (`http://127.0.0.1:11434` for Ollama, `http://127.0.0.1:8080` for OpenAI-compatible, `https://api.anthropic.com`). A LAN address is allowed in Sovereign mode; an internet address is not.",
            ),
            ParamSchema::string("model", "Model", "").help(
                "Model name; empty: the provider's default (`qwen2.5-coder:7b` on Ollama, `claude-haiku-4-5` on Anthropic, whatever the OpenAI-compatible server loaded).",
            ),
            ParamSchema::string("system", "System Prompt", "You are part of a live visual performance. Answer in one short sentence.")
                .multiline()
                .help("Instructions the model follows for every prompt."),
            ParamSchema::string("prompt", "Prompt", "")
                .multiline()
                .help("The prompt. With `Prompt From` set, it is put before the incoming text (an instruction such as \"Turn this into an image prompt:\")."),
            ParamSchema::string("prompt_from", "Prompt From", "").help(
                "The `Publish As` name of another node (Speech to Text, LLM Chat): each new text it publishes is sent. Empty: only the prompt above.",
            ),
            ParamSchema::pulse("send", "Send").help("Sends the prompt now."),
            ParamSchema::toggle("on_change", "On Change", false).help(
                "Sends whenever the prompt changes (for a prompt driven by an expression or a mapping).",
            ),
            ParamSchema::float("temperature", "Temperature", 0.7, Some(0.0), Some(2.0))
                .help("Randomness of the reply: 0 is the most predictable."),
            ParamSchema::int("max_tokens", "Max Tokens", 256, Some(1), Some(8192))
                .help("Longest reply, in tokens."),
            ParamSchema::int("history", "History", 0, Some(0), Some(20)).help(
                "Earlier exchanges sent with each prompt, so the model remembers the conversation; 0 sends each prompt on its own. Changing a setting above starts a new conversation.",
            ),
            ParamSchema::float("timeout_s", "Timeout", 60.0, Some(1.0), Some(600.0))
                .help("Seconds a request may take before it fails."),
            ParamSchema::string("publish", "Publish As", "reply").help(
                "The name each reply is published under, for another node's `Prompt From` and the app. Empty: not published.",
            ),
        ],
        inputs: vec![SocketSchema {
            name: "trigger".into(),
            wire: WireType::Sig,
            optional: true,
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
                name: "reply".into(),
                wire: WireType::Dat,
                optional: true,
                feedback: false,
            },
        ],
        capabilities: vec![],
        cook_budget_us: 50,
        time_dependent: true,
        sink: true,
    }
}

/// The parameters that decide the link's configuration, as last applied.
#[derive(Debug, Default, PartialEq)]
struct Applied {
    provider: String,
    endpoint: String,
    model: String,
    system: String,
    temperature: f32,
    max_tokens: i64,
    history: i64,
    timeout_s: f32,
}

impl Applied {
    fn matches(&self, p: &Params<'_>) -> bool {
        self.provider == p.str("provider")
            && self.endpoint == p.str("endpoint")
            && self.model == p.str("model")
            && self.system == p.str("system")
            && self.temperature == p.f32("temperature")
            && self.max_tokens == p.i64("max_tokens")
            && self.history == p.i64("history")
            && self.timeout_s == p.f32("timeout_s")
    }
}

pub struct ChatOp {
    link: Option<ChatLink>,
    applied: Applied,
    /// The prompt being sent.
    buf: String,
    /// The prompt as last seen (`On Change`).
    last_prompt: String,
    /// `Prompt From`: the name and sequence number last seen.
    from: String,
    from_seen: Option<u64>,
    last_send: bool,
    last_trigger: bool,
    /// A send is due but the mailbox was busy this instant.
    due: bool,
    seen: u64,
    replies: u32,
}

impl std::fmt::Debug for ChatOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatOp").finish_non_exhaustive()
    }
}

pub fn create() -> Box<dyn SigOp> {
    Box::new(ChatOp {
        link: None,
        applied: Applied::default(),
        buf: String::with_capacity(PROMPT_CAPACITY),
        last_prompt: String::with_capacity(1024),
        from: String::with_capacity(64),
        from_seen: None,
        last_send: false,
        last_trigger: false,
        due: false,
        seen: 0,
        replies: 0,
    })
}

impl ChatOp {
    fn configure(&mut self, p: &Params<'_>) -> Result<(), String> {
        if self.link.is_some() && self.applied.matches(p) {
            return Ok(());
        }
        let provider = Provider::parse(p.str("provider"))?;
        let config = ChatConfig {
            provider,
            endpoint: p.str("endpoint").to_owned(),
            model: p.str("model").to_owned(),
            system: p.str("system").to_owned(),
            temperature: p.f32("temperature").clamp(0.0, 2.0),
            max_tokens: p.i64("max_tokens").clamp(1, 8192) as u32,
            history: p.i64("history").clamp(0, 20) as u32,
            timeout: Duration::from_secs_f32(p.f32("timeout_s").clamp(1.0, 600.0)),
        };
        match &self.link {
            Some(link) => link.set_config(config),
            None => self.link = Some(ChatLink::new(config, PROMPT_CAPACITY)),
        }
        self.applied = Applied {
            provider: p.str("provider").to_owned(),
            endpoint: p.str("endpoint").to_owned(),
            model: p.str("model").to_owned(),
            system: p.str("system").to_owned(),
            temperature: p.f32("temperature"),
            max_tokens: p.i64("max_tokens"),
            history: p.i64("history"),
            timeout_s: p.f32("timeout_s"),
        };
        Ok(())
    }

    /// Whether this frame asks for a send; fills `buf` with the prompt.
    fn wants_send(&mut self, p: &Params<'_>, trigger: f32) -> bool {
        let prompt = p.str("prompt");
        let send = p.bool("send");
        let mut go = send && !self.last_send;
        self.last_send = send;
        let trig = trigger > 0.5;
        go |= trig && !self.last_trigger;
        self.last_trigger = trig;
        if prompt != self.last_prompt {
            go |= p.bool("on_change") && !prompt.trim().is_empty();
            self.last_prompt.clear();
            self.last_prompt.push_str(prompt);
        }
        let from = p.str("prompt_from").trim();
        if from != self.from {
            // Following another name: only texts published from now on.
            self.from.clear();
            self.from.push_str(from);
            self.from_seen = None;
        }
        self.buf.clear();
        self.buf.push_str(prompt.trim());
        if from.is_empty() {
            return go;
        }
        let seen = *self.from_seen.get_or_insert_with(|| text::seq(from));
        let buf = &mut self.buf;
        let fresh = text::read(from, |seq, incoming| {
            if !buf.is_empty() {
                buf.push_str("\n\n");
            }
            buf.push_str(incoming);
            (seq > seen).then_some(seq)
        })
        .flatten();
        if let Some(seq) = fresh {
            self.from_seen = Some(seq);
            go = true;
        }
        go
    }
}

impl SigOp for ChatOp {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // A sink offline (docs/render.md): nothing is sent or published.
            let mut w = ctx.output.writer();
            for name in ["busy", "new_reply", "replies", "error"] {
                w.push(name, 0.0);
            }
            return Ok(());
        }
        if let Err(e) = self.configure(&p) {
            ctx.output.writer().keep_previous();
            return Err(e);
        }
        let trigger = ctx
            .inputs
            .get(0)
            .filter(|c| !c.is_empty())
            .map_or(0.0, |c| c.last(0));
        self.due |= self.wants_send(&p, trigger);
        let link = self.link.as_ref().expect("configured");
        if self.due && !self.buf.trim().is_empty() {
            if link.ask(&self.buf).is_some() {
                self.due = false;
            }
        } else {
            self.due = false;
        }
        let mut new_reply = false;
        let publish = p.str("publish");
        link.take(&mut self.seen, |reply| {
            text::publish(publish, &reply.text);
            new_reply = true;
        });
        if new_reply {
            self.replies += 1;
        }
        let (busy, failed) = link.with_status(|s| (s.busy, s.error.is_some()));
        let mut w = ctx.output.writer();
        w.push("busy", f32::from(u8::from(busy)));
        w.push("new_reply", f32::from(u8::from(new_reply)));
        w.push("replies", self.replies as f32);
        w.push("error", f32::from(u8::from(failed)));
        drop(w);
        if failed {
            // The last request failed: say why until one succeeds.
            if let Some(e) = link.with_status(|s| s.error.as_ref().map(ToString::to_string)) {
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_declares_sig_first_and_dat_reply() {
        let s = schema();
        assert_eq!(s.outputs[0].wire, WireType::Sig);
        assert_eq!(s.outputs[1].wire, WireType::Dat);
        assert_eq!(s.inputs[0].wire, WireType::Sig);
        assert!(s.sink && s.time_dependent && s.cook_budget_us > 0);
    }
}
