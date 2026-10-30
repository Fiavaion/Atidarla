//! The assistant (FR-G4): prompt to network with a language model over the
//! typed tools — generate, stage, accept (or hand to a person), read the
//! errors, repair — grounded in the operator catalogue the registry
//! generates (the same corpus as the operator docs).
//!
//! Models: Ollama on this machine by default (A8); Anthropic only when a
//! project opts in, and then through the egress gate, so Sovereign mode
//! refuses it before any packet leaves.

use std::fmt::Write as _;
use std::time::Duration;

use serde_json::{Value, json};

use crate::tools::{Agent, Output, output_text};
use crate::workspace::Workspace;

/// A chat model that can call tools.
pub trait Llm {
    /// The next assistant message (`{"content": …, "tool_calls": [{"name", "arguments"}]}`)
    /// for `messages` (OpenAI-style roles: system, user, assistant, tool).
    fn chat(&mut self, messages: &[Value], tools: &[Value]) -> Result<Value, String>;
    fn name(&self) -> String;
}

/// Ollama's `/api/chat` with tools.
#[derive(Debug, Clone)]
pub struct Ollama {
    pub base: String,
    pub model: String,
}

impl Ollama {
    pub fn new(model: &str) -> Self {
        Self {
            base: std::env::var("OLLAMA_HOST")
                .map(|h| {
                    if h.starts_with("http") {
                        h
                    } else {
                        format!("http://{h}")
                    }
                })
                .unwrap_or_else(|_| "http://127.0.0.1:11434".into()),
            model: model.to_owned(),
        }
    }
}

fn post_json(url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value, String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let hostport = rest.split('/').next().unwrap_or_default();
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(80)),
        None => (hostport, if url.starts_with("https") { 443 } else { 80 }),
    };
    // The egress gate decides (Sovereign mode refuses the internet).
    atd_ai::egress::resolve(host, port).map_err(|e| e.to_string())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into();
    let mut req = agent.post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req
        .send(body.to_string())
        .map_err(|e| format!("{url}: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("{url}: {e}: {}", &text[..text.len().min(300)]))
}

impl Llm for Ollama {
    fn chat(&mut self, messages: &[Value], tools: &[Value]) -> Result<Value, String> {
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": false,
            "options": {"temperature": 0.2, "num_ctx": 32768},
        });
        let r = post_json(&format!("{}/api/chat", self.base), &[], &body)?;
        if let Some(e) = r.get("error") {
            return Err(format!("Ollama: {e}"));
        }
        let m = &r["message"];
        let calls: Vec<Value> = m["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(
                |c| json!({"name": c["function"]["name"], "arguments": c["function"]["arguments"]}),
            )
            .collect();
        Ok(json!({"content": m["content"], "tool_calls": calls}))
    }

    fn name(&self) -> String {
        format!("ollama:{}", self.model)
    }
}

/// Anthropic's Messages API (opt-in per project; `ANTHROPIC_API_KEY`).
#[derive(Debug, Clone)]
pub struct Anthropic {
    pub key: String,
    pub model: String,
}

impl Llm for Anthropic {
    fn chat(&mut self, messages: &[Value], tools: &[Value]) -> Result<Value, String> {
        let system = messages
            .iter()
            .filter(|m| m["role"] == "system")
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        // OpenAI-style history → Anthropic content blocks.
        let mut msgs: Vec<Value> = Vec::new();
        for m in messages.iter().filter(|m| m["role"] != "system") {
            match m["role"].as_str() {
                Some("tool") => msgs.push(json!({"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": m["tool_call_id"], "content": m["content"]}]})),
                Some("assistant") => {
                    let mut blocks = Vec::new();
                    if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) {
                        blocks.push(json!({"type": "text", "text": t}));
                    }
                    for c in m["tool_calls"].as_array().into_iter().flatten() {
                        blocks.push(json!({"type": "tool_use", "id": c["id"], "name": c["name"], "input": c["arguments"]}));
                    }
                    msgs.push(json!({"role": "assistant", "content": blocks}));
                }
                _ => msgs.push(json!({"role": "user", "content": m["content"]})),
            }
        }
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| json!({"name": t["function"]["name"], "description": t["function"]["description"], "input_schema": t["function"]["parameters"]}))
            .collect();
        let body = json!({"model": self.model, "max_tokens": 4096, "system": system, "messages": msgs, "tools": tools});
        let r = post_json(
            "https://api.anthropic.com/v1/messages",
            &[
                ("x-api-key", &self.key),
                ("anthropic-version", "2023-06-01"),
            ],
            &body,
        )?;
        if let Some(e) = r.get("error") {
            return Err(format!("Anthropic: {e}"));
        }
        let mut text = String::new();
        let mut calls = Vec::new();
        for b in r["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => text.push_str(b["text"].as_str().unwrap_or_default()),
                Some("tool_use") => {
                    calls.push(json!({"id": b["id"], "name": b["name"], "arguments": b["input"]}))
                }
                _ => {}
            }
        }
        Ok(json!({"content": text, "tool_calls": calls}))
    }

    fn name(&self) -> String {
        format!("anthropic:{}", self.model)
    }
}

/// The system prompt: how to work, and the operator catalogue.
pub fn system_prompt(ws: &dyn Workspace) -> String {
    let mut s = String::from(
        "You build node networks (patches) in Atidarla, a real-time visual instrument, by calling tools. \
Rules:\n\
- Call get_schema for every operator type before creating a node of it: use only parameter names and input names it lists.\n\
- Wires connect an output to an input of the same wire type (IMG to IMG, SIG to SIG, GEO to GEO). Most image nodes take their image on input `in`; check the schema.\n\
- Changes are staged. When the network is built, call accept_changes, then read the errors it returns and fix them (set_param, connect, delete_node) and accept again, until there are no errors.\n\
- Animate with expressions such as `0.5 + 0.5 * sin(time)` in set_param, or bind a parameter to a SIG node's channel.\n\
- Do not explain; act with tools. Stop when the network is accepted with no errors.\n\nOperators (type — summary):\n",
    );
    for op in ws.registry().iter() {
        if op.type_id.starts_with(atd_core::pack::TYPE_PREFIX) {
            continue;
        }
        let first = op.summary.split(". ").next().unwrap_or_default();
        let _ = writeln!(s, "{} — {}", op.type_id, first.trim_end_matches('.'));
    }
    s
}

/// How a request went.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub steps: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub accepted: bool,
    /// Node errors after the last accept.
    pub errors: Vec<String>,
    pub final_text: String,
    pub llm_error: Option<String>,
}

/// Runs one request to completion (or `max_steps` model turns).
pub fn build(
    llm: &mut dyn Llm,
    agent: &mut Agent,
    ws: &mut dyn Workspace,
    request: &str,
    max_steps: u32,
) -> Run {
    let tools: Vec<Value> = agent
        .tools()
        .iter()
        .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
        .collect();
    let mut messages = vec![
        json!({"role": "system", "content": system_prompt(ws)}),
        json!({"role": "user", "content": request}),
    ];
    let mut run = Run::default();
    let mut nudged = false;
    while run.steps < max_steps {
        run.steps += 1;
        let reply = match llm.chat(&messages, &tools) {
            Ok(r) => r,
            Err(e) => {
                run.llm_error = Some(e);
                break;
            }
        };
        let calls = reply["tool_calls"].as_array().cloned().unwrap_or_default();
        let mut assistant =
            json!({"role": "assistant", "content": reply["content"].as_str().unwrap_or_default()});
        let calls: Vec<Value> = calls
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                if c["id"].is_null() {
                    c["id"] = json!(format!("call_{}_{i}", run.steps));
                }
                c
            })
            .collect();
        if !calls.is_empty() {
            assistant["tool_calls"] = json!(
                calls
                    .iter()
                    .map(|c| json!({
                "id": c["id"], "name": c["name"], "arguments": c["arguments"],
                "function": {"name": c["name"], "arguments": c["arguments"]}}))
                    .collect::<Vec<_>>()
            );
        }
        messages.push(assistant);
        if calls.is_empty() {
            run.final_text = reply["content"].as_str().unwrap_or_default().to_owned();
            // Built but never accepted: ask once to accept and check.
            if !agent.staged().is_empty() && !nudged {
                nudged = true;
                messages.push(json!({"role": "user", "content": "Call accept_changes now, then fix any errors it reports."}));
                continue;
            }
            break;
        }
        for c in &calls {
            let name = c["name"].as_str().unwrap_or_default();
            let mut args = c["arguments"].clone();
            if let Some(s) = args.as_str() {
                args = serde_json::from_str(s).unwrap_or(json!({}));
            }
            let out = agent.call(ws, name, &args);
            run.tool_calls += 1;
            if matches!(out, Output::Error(_)) {
                run.tool_errors += 1;
            }
            if name == "accept_changes"
                && let Output::Json(v) = &out
            {
                run.accepted = true;
                run.errors = v["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        format!(
                            "{}: {}",
                            e["node"].as_str().unwrap_or("?"),
                            e["error"].as_str().unwrap_or("?")
                        )
                    })
                    .collect();
            }
            let mut text = output_text(&out);
            if text.len() > 6000 {
                text.truncate(6000);
                text.push_str("\n… (truncated)");
            }
            messages.push(
                json!({"role": "tool", "tool_call_id": c["id"], "name": name, "content": text}),
            );
        }
    }
    // Whatever is still staged is applied for the record (the eval judges
    // the network the model left).
    if !agent.staged().is_empty() && agent.scopes.contains(&crate::lock::Scope::Accept) {
        match agent.accept(ws, "Assistant") {
            Ok(errors) => {
                run.accepted = true;
                let net = ws.network();
                run.errors = errors
                    .iter()
                    .map(|(id, e)| {
                        format!("{}: {e}", net.node(*id).map_or("?", |n| n.name.as_str()))
                    })
                    .collect();
            }
            Err(e) => run.errors.push(e),
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{Scope, ShowLock};
    use crate::workspace::NetworkWorkspace;

    /// A scripted model: replays tool calls, then stops.
    struct Script(Vec<Value>);

    impl Llm for Script {
        fn chat(&mut self, _: &[Value], _: &[Value]) -> Result<Value, String> {
            Ok(if self.0.is_empty() {
                json!({"content": "done", "tool_calls": []})
            } else {
                self.0.remove(0)
            })
        }
        fn name(&self) -> String {
            "script".into()
        }
    }

    #[test]
    fn the_loop_applies_tool_calls_and_accepts() {
        let mut reg = atd_core::Registry::default();
        atd_ops_img::register(&mut reg);
        let mut ws = NetworkWorkspace::new(reg);
        let scopes = Scope::defaults()
            .into_iter()
            .chain([Scope::Accept])
            .collect();
        let mut agent = Agent::new(scopes, ShowLock::default());
        let call =
            |n: &str, a: Value| json!({"content": "", "tool_calls": [{"name": n, "arguments": a}]});
        let mut llm = Script(vec![
            call(
                "create_node",
                json!({"type": "img.noise", "name": "noise1"}),
            ),
            // Arguments as a JSON string, as some models send them.
            call(
                "create_node",
                json!("{\"type\": \"img.blur\", \"name\": \"blur1\"}"),
            ),
            call(
                "connect",
                json!({"from": "noise1", "to": "blur1", "input": "in"}),
            ),
            call("create_node", json!({"type": "img.nope"})),
        ]);
        let run = build(&mut llm, &mut agent, &mut ws, "noise, blurred", 20);
        assert_eq!(run.tool_calls, 4);
        assert_eq!(run.tool_errors, 1);
        assert!(run.accepted, "nudged to accept");
        assert_eq!(ws.network().len(), 2);
        assert!(system_prompt(&ws).contains("img.noise — "));
    }
}
