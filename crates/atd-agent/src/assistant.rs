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

use atd_core::registry::ParamKind;

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
            "options": {"temperature": 0.2, "num_ctx": 32768, "num_predict": 2048},
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
- Build with ONE build_network call holding every node, its parameters and every wire; use only the operator types, parameter names and input names listed (call get_schema for any other type first).\n\
- Wires connect an output to an input of the same wire type (IMG to IMG, SIG to SIG, GEO to GEO). Most image nodes take their image on input `in`; check the schema.\n\
- Changes are staged. When the network is built, call accept_changes, then read the errors it returns and fix them (set_param, connect, delete_node) and accept again, until there are no errors.\n\
- Animate with expressions such as `0.5 + 0.5 * sin(time)` in set_param, or bind a parameter to a SIG node's channel.\n\
- Do not explain; act with tools. Stop when the network is accepted with no errors.\n\nOperators (type — summary):\n",
    );
    // Image and channel operators first: a small model copies the prefix
    // of whatever it read first (an alphabetical list opens with `ai.`).
    let order = ["img.", "sig.", "pnt.", "msh.", "mat.", "cmp.", "dat."];
    let rank = |t: &str| {
        order
            .iter()
            .position(|p| t.starts_with(p))
            .unwrap_or(order.len())
    };
    let mut ops: Vec<_> = ws.registry().iter().collect();
    ops.sort_by_key(|op| rank(&op.type_id));
    for op in ops {
        if op.type_id.starts_with(atd_core::pack::TYPE_PREFIX) {
            continue;
        }
        let first = op.summary.split(". ").next().unwrap_or_default();
        let _ = writeln!(s, "{} — {}", op.type_id, first.trim_end_matches('.'));
    }
    s
}

/// Tool calls a model wrote into its text instead of the tool-call field
/// (qwen2.5-coder 7B through Ollama does this): every JSON object in the
/// text, fenced or not, whose `name` is one of the offered tools.
fn calls_in_text(text: &str, tools: &[Value], is_op: impl Fn(&str) -> bool) -> Vec<Value> {
    let known = |n: &str| tools.iter().any(|t| t["function"]["name"] == n) || is_op(n);
    let mut calls = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('{') {
        let start = i + off;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(v)) if v["name"].as_str().is_some_and(known) => {
                let args = v.get("arguments").or_else(|| v.get("parameters"));
                calls.push(
                    json!({"name": v["name"], "arguments": args.cloned().unwrap_or(json!({}))}),
                );
                i = start + values.byte_offset();
            }
            _ => i = start + 1,
        }
    }
    calls
}

/// Words of a text, lower case, with a plural or verb ending taken off
/// (`blurred` → `blur`, `gradients` → `gradient`).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| {
            let w = w.to_ascii_lowercase();
            for end in ["ing", "red", "ed", "es", "s"] {
                if let Some(stem) = w.strip_suffix(end)
                    && stem.len() > 3
                {
                    return stem.to_owned();
                }
            }
            w
        })
        .collect()
}

/// Operators every patch tends to need, always described in full.
const CORE_OPS: &[&str] = &[
    "img.noise",
    "img.ramp",
    "img.constant",
    "img.composite",
    "img.blend",
    "img.transform",
    "img.level",
    "sig.lfo",
    "sig.math",
];

/// The operators that match the request best (by the words of their type,
/// label and summary), plus [`CORE_OPS`], with their parameters and
/// sockets written compactly, so a small model needs no schema lookups.
pub fn relevant_ops(ws: &dyn Workspace, request: &str) -> String {
    let asked = words(request);
    let mut scored: Vec<(usize, &atd_core::registry::OperatorSchema)> = ws
        .registry()
        .iter()
        .filter(|op| !op.type_id.starts_with(atd_core::pack::TYPE_PREFIX))
        .map(|op| {
            let name = words(&format!(
                "{} {}",
                op.type_id.replace(['.', '_'], " "),
                op.label
            ));
            let summary = words(&op.summary);
            // Same word, or one a prefix of the other (`displac` / `displace`).
            let like = |a: &str, b: &str| {
                a == b || a.len().min(b.len()) >= 4 && (a.starts_with(b) || b.starts_with(a))
            };
            let hit = |ws: &[String], w: &str| ws.iter().any(|x| like(x, w));
            let score = asked
                .iter()
                .map(|w| 3 * usize::from(hit(&name, w)) + usize::from(hit(&summary, w)))
                .sum();
            (score, op)
        })
        .filter(|(score, op)| *score > 0 || CORE_OPS.contains(&op.type_id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.type_id.cmp(&b.1.type_id)));
    let mut out = String::from(
        "\nOperators for this request (type: inputs -> outputs; parameters as name kind=default):\n",
    );
    let mut shown = 0;
    for (score, op) in scored {
        if score == 0 && !CORE_OPS.contains(&op.type_id.as_str()) || shown >= 24 {
            continue;
        }
        shown += 1;
        let sockets = |s: &[atd_core::registry::SocketSchema]| {
            s.iter()
                .map(|s| {
                    format!(
                        "{}:{:?}{}",
                        s.name,
                        s.wire,
                        if s.optional { "?" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let params: Vec<String> = op
            .params
            .iter()
            .map(|p| {
                let kind = match &p.kind {
                    ParamKind::Float { min, max, .. } => {
                        format!("float[{}..{}]", opt(*min), opt(*max))
                    }
                    ParamKind::Int { min, max, .. } => format!(
                        "int[{}..{}]",
                        opt(min.map(|v| v as f64)),
                        opt(max.map(|v| v as f64))
                    ),
                    ParamKind::Menu { options, .. } => format!("menu({})", options.join("|")),
                    k => format!("{k:?}").to_ascii_lowercase(),
                };
                format!("{} {kind}={}", p.name, p.default)
            })
            .collect();
        let _ = writeln!(
            out,
            "{}: {} -> {}; {}",
            op.type_id,
            sockets(&op.inputs),
            sockets(&op.outputs),
            params.join(", ")
        );
    }
    out
}

fn opt(v: Option<f64>) -> String {
    v.map_or(String::new(), |v| format!("{v}"))
}

/// How a request went./// How a request went.
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
    let system = format!("{}{}", system_prompt(ws), relevant_ops(ws, request));
    let mut messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": request}),
    ];
    let mut run = Run::default();
    let mut nudged = false;
    let (mut last_failed, mut repeats, mut repairs) = (Vec::<String>::new(), 0, 0);
    // ATD_AGENT_TRACE=1 prints every tool call and the start of its result.
    let trace = std::env::var("ATD_AGENT_TRACE").is_ok_and(|v| v == "1");
    while run.steps < max_steps {
        run.steps += 1;
        let reply = match llm.chat(&messages, &tools) {
            Ok(r) => r,
            Err(e) => {
                run.llm_error = Some(e);
                break;
            }
        };
        let mut calls = reply["tool_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            calls = calls_in_text(reply["content"].as_str().unwrap_or_default(), &tools, |n| {
                ws.registry().get(n).is_some()
            });
        }
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
            if trace {
                eprintln!("[{}] text: {}", run.steps, run.final_text);
            }
            // Built but never accepted: ask once to accept and check.
            if !agent.staged().is_empty() && !nudged {
                nudged = true;
                messages.push(json!({"role": "user", "content": "Call accept_changes now, then fix any errors it reports."}));
                continue;
            }
            // Stopped with failed calls or node errors: ask for a repair
            // a few times before giving up.
            let mut problems = last_failed.clone();
            problems.extend(run.errors.iter().map(|e| format!("node error {e}")));
            if !problems.is_empty() && repairs < 3 {
                repairs += 1;
                messages.push(json!({"role": "user", "content": format!(
                    "The request is not done; these failed:\n{}\nFix them with tool calls (use the operator types and node names the errors suggest), then call accept_changes.",
                    problems.join("\n"))}));
                continue;
            }
            break;
        }
        let mut failed = Vec::new();
        for c in &calls {
            let mut name = c["name"].as_str().unwrap_or_default();
            let mut args = c["arguments"].clone();
            if let Some(s) = args.as_str() {
                args = serde_json::from_str(s).unwrap_or(json!({}));
            }
            // An operator type used as the tool name means "make one".
            if ws.registry().get(name).is_some() {
                let params = args.as_object().cloned().unwrap_or_default();
                args = json!({"type": name, "params": params});
                name = "create_node";
            }
            let out = agent.call(ws, name, &args);
            if trace {
                let mut o = output_text(&out);
                o.truncate(160);
                eprintln!("[{}] {name} {args} -> {o}", run.steps);
            }
            run.tool_calls += 1;
            if let Output::Error(e) = &out {
                run.tool_errors += 1;
                failed.push(format!("{name} {args}: {e}"));
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
        // A small model can repeat the same failing calls turn after turn:
        // say so plainly, then give up on the third repeat.
        if !failed.is_empty() && failed == last_failed {
            repeats += 1;
            if repeats >= 3 {
                break;
            }
            messages.push(json!({"role": "user", "content": format!(
                "These calls failed again with the same errors. Do not repeat them; read each error and change the call (use the operator types and node names the errors suggest):\n{}",
                failed.join("\n"))}));
        } else {
            repeats = 0;
        }
        last_failed = failed;
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
    fn tool_calls_written_as_text_are_found() {
        let tools = [
            json!({"function": {"name": "create_node"}}),
            json!({"function": {"name": "connect"}}),
        ];
        let text = "{\"name\": \"create_node\", \"arguments\": {\"type\": \"img.noise\"}}\n\
```json\n{\"name\": \"connect\", \"parameters\": {\"from\": \"a\", \"to\": \"b\"}}\n```\n\
{\"name\": \"rm_rf\", \"arguments\": {}} and {not json";
        let calls = calls_in_text(text, &tools, |n| n == "img.noise");
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0]["arguments"]["type"], "img.noise");
        assert_eq!(calls[1]["name"], "connect");
        assert_eq!(calls[1]["arguments"]["to"], "b");
        assert!(calls_in_text("done, the network is built", &tools, |_| false).is_empty());
    }

    #[test]
    fn build_network_stages_everything_and_updates_by_name() {
        let mut reg = atd_core::Registry::default();
        atd_ops_img::register(&mut reg);
        let mut ws = NetworkWorkspace::new(reg);
        let mut agent = Agent::new(Scope::defaults().into_iter().collect(), ShowLock::default());
        let net = json!({
            "nodes": [
                {"name": "noise1", "type": "img.noise", "params": {"scale": "2 + sin(time)", "octaves": "5"}},
                {"name": "blur1", "type": "img.blur", "params": {"size": 4, "radius": 1}},
                {"name": "bad", "type": "ai.noise"}
            ],
            "wires": [
                {"from": "noise1", "to": "blur1", "input": "in"},
                {"from": "bad", "to": "blur1", "input": "in"}
            ]
        });
        let Output::Json(out) = agent.call(&mut ws, "build_network", &net) else {
            panic!("build_network failed");
        };
        assert_eq!(out["nodes"], json!(["noise1", "blur1"]), "{out}");
        assert_eq!(out["wires"], 1);
        let errors = out["errors"].to_string();
        assert!(errors.contains("did you mean `img.noise`"), "{errors}");
        let view = agent.view(&ws).clone();
        let noise = view.node_by_name("noise1").unwrap();
        assert!(noise.params.contains_key("scale"), "{errors} {:?}", noise.params);
        assert_eq!(noise.params["scale"].expr.as_deref(), Some("2 + sin(time)"));
        assert_eq!(noise.params["octaves"].value, json!(5));
        // Again with the same names: updated, not duplicated.
        let again =
            json!({"nodes": [{"name": "blur1", "type": "img.blur", "params": {"size": 8}}]});
        agent.call(&mut ws, "build_network", &again);
        let view = agent.view(&ws).clone();
        assert_eq!(view.len(), 2);
        assert_eq!(
            view.node_by_name("blur1").unwrap().params["size"].value,
            json!(8)
        );
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
