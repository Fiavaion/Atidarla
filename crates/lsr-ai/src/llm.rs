//! LLM chat for the LLM Chat node (FR-A13): Ollama by default (local),
//! an OpenAI-compatible server (llama.cpp, LM Studio, vLLM) or Anthropic,
//! opt in.
//!
//! Every request goes through the egress gate ([`crate::egress`]): the HTTP
//! agent resolves each connection with it, so in Sovereign mode an internet
//! endpoint is refused before any packet is sent, and the Anthropic
//! provider (always on the internet) is refused up front with a clear error.
//! Requests run on a [`ChatLink`] worker thread; the render thread only
//! hands over prompts and polls for replies (rule 1).
//!
//! API keys are read from the environment (`ANTHROPIC_API_KEY`,
//! `OPENAI_API_KEY`), never from the project file, which is shared text.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::egress;
use crate::models::DownloadOptions;

/// Ollama's default address.
pub const OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";
/// The default model on Ollama.
pub const OLLAMA_MODEL: &str = "qwen2.5-coder:7b";
/// llama.cpp's `llama-server` default address.
pub const OPENAI_COMPAT_ENDPOINT: &str = "http://127.0.0.1:8080";
pub const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com";
/// The default Anthropic model: fast and inexpensive for live use.
pub const ANTHROPIC_MODEL: &str = "claude-haiku-5-5";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Who answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Ollama,
    /// `/v1/chat/completions` on a local or LAN server.
    OpenAiCompat,
    /// Anthropic's Messages API (internet: refused in Sovereign mode).
    Anthropic,
}

impl Provider {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ollama" => Ok(Self::Ollama),
            "openai_compat" => Ok(Self::OpenAiCompat),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(format!(
                "unknown provider {s:?}: use ollama, openai_compat or anthropic"
            )),
        }
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Ollama => OLLAMA_ENDPOINT,
            Self::OpenAiCompat => OPENAI_COMPAT_ENDPOINT,
            Self::Anthropic => ANTHROPIC_ENDPOINT,
        }
    }

    /// The model used when the node names none (an OpenAI-compatible
    /// server serves whichever model it loaded).
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => OLLAMA_MODEL,
            Self::OpenAiCompat => "default",
            Self::Anthropic => ANTHROPIC_MODEL,
        }
    }

    /// The environment variable holding the API key, if the provider uses one.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::Ollama => None,
            Self::OpenAiCompat => Some("OPENAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
        }
    }
}

/// One request's settings; changing them clears the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatConfig {
    pub provider: Provider,
    /// Base URL; empty: the provider's default.
    pub endpoint: String,
    /// Empty: the provider's default.
    pub model: String,
    pub system: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Earlier exchanges sent with each prompt (0: none).
    pub history: u32,
    pub timeout: Duration,
}

impl ChatConfig {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            endpoint: String::new(),
            model: String::new(),
            system: String::new(),
            temperature: 0.7,
            max_tokens: 512,
            history: 0,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn endpoint(&self) -> &str {
        let e = self.endpoint.trim().trim_end_matches('/');
        if e.is_empty() {
            self.provider.default_endpoint()
        } else {
            e
        }
    }

    pub fn model(&self) -> &str {
        let m = self.model.trim();
        if m.is_empty() {
            self.provider.default_model()
        } else {
            m
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ChatError {
    #[error(
        "Sovereign mode blocks {host}: it is on the internet. Use a local provider, or turn Sovereign mode off for this project"
    )]
    Sovereign { host: String },
    #[error("{provider} needs an API key: set {env} in the environment")]
    NoKey {
        provider: &'static str,
        env: &'static str,
    },
    #[error("{0}")]
    Http(String),
    #[error("the reply could not be read: {0}")]
    Reply(String),
}

/// A message of the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// `user` or `assistant`.
    pub role: &'static str,
    pub content: String,
}

/// The host part of a base URL (`http://host:port/...`).
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Sends one chat request and waits for the reply (worker threads only).
pub fn complete(cfg: &ChatConfig, messages: &[Message]) -> Result<String, ChatError> {
    let base = cfg.endpoint();
    if cfg.provider == Provider::Anthropic && egress::sovereign() {
        // Always on the internet: refused before any lookup is made.
        return Err(ChatError::Sovereign {
            host: url_host(base).to_owned(),
        });
    }
    let key = match cfg.provider.key_env() {
        Some(env) => {
            let key = std::env::var(env).ok().filter(|k| !k.trim().is_empty());
            if key.is_none() && cfg.provider == Provider::Anthropic {
                return Err(ChatError::NoKey {
                    provider: "Anthropic",
                    env,
                });
            }
            key
        }
        None => None,
    };
    let (path, body) = request_body(cfg, messages);
    let refused = Arc::new(Mutex::new(None));
    let agent = crate::models::gated_agent(&DownloadOptions::default(), refused.clone());
    let mut req = agent
        .post(format!("{base}{path}"))
        .config()
        .timeout_global(Some(cfg.timeout))
        .build()
        .header("content-type", "application/json");
    req = match (cfg.provider, &key) {
        (Provider::Anthropic, Some(k)) => req
            .header("x-api-key", k.as_str())
            .header("anthropic-version", ANTHROPIC_VERSION),
        (Provider::OpenAiCompat, Some(k)) => req.header("authorization", format!("Bearer {k}")),
        _ => req,
    };
    let bytes = serde_json::to_vec(&body).map_err(|e| ChatError::Http(e.to_string()))?;
    let resp =
        req.send(&bytes[..])
            .map_err(|e| match refused.lock().ok().and_then(|mut r| r.take()) {
                Some(host) => ChatError::Sovereign { host },
                None => ChatError::Http(format!("{base}: {e}")),
            })?;
    let status = resp.status().as_u16();
    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| ChatError::Reply(e.to_string()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| ChatError::Reply(e.to_string()))?;
    if !(200..300).contains(&status) {
        let why = v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .unwrap_or("no details");
        return Err(ChatError::Http(format!("{base} answered {status}: {why}")));
    }
    parse_reply(cfg.provider, &v)
}

/// The path and JSON body of a chat request.
fn request_body(cfg: &ChatConfig, messages: &[Message]) -> (&'static str, Value) {
    let msgs = |with_system: bool| {
        let mut v: Vec<Value> = Vec::with_capacity(messages.len() + 1);
        if with_system && !cfg.system.trim().is_empty() {
            v.push(json!({"role": "system", "content": cfg.system}));
        }
        v.extend(
            messages
                .iter()
                .map(|m| json!({"role": m.role, "content": m.content})),
        );
        v
    };
    match cfg.provider {
        Provider::Ollama => (
            "/api/chat",
            json!({
                "model": cfg.model(),
                "messages": msgs(true),
                "stream": false,
                "options": {"temperature": cfg.temperature, "num_predict": cfg.max_tokens},
            }),
        ),
        Provider::OpenAiCompat => (
            "/v1/chat/completions",
            json!({
                "model": cfg.model(),
                "messages": msgs(true),
                "temperature": cfg.temperature,
                "max_tokens": cfg.max_tokens,
                "stream": false,
            }),
        ),
        Provider::Anthropic => {
            let mut body = json!({
                "model": cfg.model(),
                "max_tokens": cfg.max_tokens,
                "temperature": cfg.temperature,
                "messages": msgs(false),
            });
            if !cfg.system.trim().is_empty() {
                body["system"] = json!(cfg.system);
            }
            ("/v1/messages", body)
        }
    }
}

fn parse_reply(provider: Provider, v: &Value) -> Result<String, ChatError> {
    let text = match provider {
        Provider::Ollama => v["message"]["content"].as_str().map(str::to_owned),
        Provider::OpenAiCompat => v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_owned),
        Provider::Anthropic => v["content"].as_array().map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<String>()
        }),
    };
    text.map(|t| t.trim().to_owned())
        .ok_or_else(|| ChatError::Reply("no text in the answer".into()))
}

/// What the node shows about its requests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatStatus {
    /// A request is on its way.
    pub busy: bool,
    pub replies: u64,
    pub latency_ms: f64,
    pub error: Option<ChatError>,
}

/// The newest reply.
#[derive(Debug, Default)]
pub struct Reply {
    /// The prompt it answers.
    pub seq: u64,
    pub text: String,
    pub at: Option<Instant>,
}

#[derive(Debug, Default)]
struct Ask {
    seq: u64,
    full: bool,
    prompt: String,
}

struct Shared {
    input: Mutex<Ask>,
    wake: Condvar,
    output: Mutex<Reply>,
    out_seq: AtomicU64,
    status: Mutex<ChatStatus>,
    config: Mutex<(u64, ChatConfig)>,
    stop: AtomicBool,
}

/// One LLM Chat node's request worker.
pub struct ChatLink {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ChatLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatLink").finish_non_exhaustive()
    }
}

impl ChatLink {
    /// Starts the worker; prompts up to `capacity` bytes are handed over
    /// without allocating.
    pub fn new(config: ChatConfig, capacity: usize) -> Self {
        let shared = Arc::new(Shared {
            input: Mutex::new(Ask {
                seq: 0,
                full: false,
                prompt: String::with_capacity(capacity),
            }),
            wake: Condvar::new(),
            output: Mutex::default(),
            out_seq: AtomicU64::new(0),
            status: Mutex::default(),
            config: Mutex::new((1, config)),
            stop: AtomicBool::new(false),
        });
        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("lsr-ai-chat".into())
                .spawn(move || chat_worker(&shared))
                .expect("spawn LLM chat worker")
        };
        Self {
            shared,
            worker: Some(worker),
        }
    }

    pub fn set_config(&self, config: ChatConfig) {
        let mut c = self.shared.config.lock().expect("config");
        if c.1 != config {
            c.0 += 1;
            c.1 = config;
        }
    }

    /// Hands over a prompt without waiting. A prompt not yet taken by the
    /// worker is replaced (the newest prompt wins); `None` only when the
    /// worker holds the mailbox this instant.
    pub fn ask(&self, prompt: &str) -> Option<u64> {
        let mut a = self.shared.input.try_lock().ok()?;
        a.prompt.clear();
        a.prompt.push_str(prompt);
        a.full = true;
        a.seq += 1;
        let seq = a.seq;
        drop(a);
        self.shared.wake.notify_one();
        Some(seq)
    }

    pub fn result_seq(&self) -> u64 {
        self.shared.out_seq.load(Ordering::Acquire)
    }

    /// Calls `f` with the newest reply if it is newer than `*seen`.
    pub fn take(&self, seen: &mut u64, f: impl FnOnce(&Reply)) -> bool {
        let n = self.result_seq();
        if n <= *seen {
            return false;
        }
        let Ok(out) = self.shared.output.try_lock() else {
            return false;
        };
        f(&out);
        *seen = n;
        true
    }

    pub fn with_status<R>(&self, f: impl FnOnce(&ChatStatus) -> R) -> R {
        f(&self.shared.status.lock().expect("status"))
    }
}

impl Drop for ChatLink {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        // A request in flight finishes within its timeout; do not wait for it.
        if let Some(w) = self.worker.take()
            && w.is_finished()
        {
            let _ = w.join();
        }
    }
}

fn chat_worker(shared: &Shared) {
    let mut config_gen = 0;
    let mut history: Vec<Message> = Vec::new();
    while !shared.stop.load(Ordering::Acquire) {
        let (seq, prompt) = {
            let mut a = shared.input.lock().expect("input");
            if !a.full {
                let _ = shared.wake.wait_timeout(a, Duration::from_millis(100));
                continue;
            }
            a.full = false;
            (a.seq, a.prompt.clone())
        };
        let cfg = {
            let c = shared.config.lock().expect("config");
            if c.0 != config_gen {
                config_gen = c.0;
                history.clear();
            }
            c.1.clone()
        };
        let keep = cfg.history as usize * 2;
        if history.len() > keep {
            history.drain(..history.len() - keep);
        }
        history.push(Message {
            role: "user",
            content: prompt,
        });
        shared.status.lock().expect("status").busy = true;
        let sent = Instant::now();
        let result = complete(&cfg, &history);
        let latency = sent.elapsed().as_secs_f64() * 1e3;
        match result {
            Ok(text) => {
                {
                    let mut out = shared.output.lock().expect("output");
                    out.seq = seq;
                    out.text.clone_from(&text);
                    out.at = Some(Instant::now());
                }
                shared.out_seq.fetch_add(1, Ordering::AcqRel);
                history.push(Message {
                    role: "assistant",
                    content: text,
                });
                let mut s = shared.status.lock().expect("status");
                s.busy = false;
                s.replies += 1;
                s.latency_ms = latency;
                s.error = None;
            }
            Err(e) => {
                // The unanswered prompt leaves the conversation.
                history.pop();
                log::warn!("LLM Chat: {e}");
                let mut s = shared.status.lock().expect("status");
                s.busy = false;
                s.error = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// Requests a fake server received: path, lower-cased headers, body.
    type Seen = Arc<Mutex<Vec<(String, Vec<String>, Value)>>>;

    /// A loopback HTTP server answering every request with `reply(body)`;
    /// returns its base URL and the requests it received.
    fn fake_server(reply: impl Fn(&Value) -> (u16, Value) + Send + 'static) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut headers = Vec::new();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end().to_owned();
                    if h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    headers.push(h.to_ascii_lowercase());
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let (status, answer) = reply(&body);
                log.lock().unwrap().push((path, headers, body));
                let answer = answer.to_string();
                let mut conn = conn;
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn user(text: &str) -> Message {
        Message {
            role: "user",
            content: text.into(),
        }
    }

    #[test]
    fn ollama_requests_carry_the_system_prompt_and_model() {
        let (url, seen) = fake_server(|_| {
            (
                200,
                json!({"message": {"role": "assistant", "content": " Hi there. "}}),
            )
        });
        let mut cfg = ChatConfig::new(Provider::Ollama);
        cfg.endpoint = url;
        cfg.system = "Answer in one line.".into();
        cfg.temperature = 0.2;
        cfg.max_tokens = 64;
        assert_eq!(complete(&cfg, &[user("hello")]).unwrap(), "Hi there.");
        let (path, headers, body) = seen.lock().unwrap().pop().unwrap();
        assert_eq!(path, "/api/chat");
        assert!(
            headers
                .iter()
                .any(|h| h == "content-type: application/json")
        );
        // Ollama takes no key: none is sent.
        assert!(!headers.iter().any(|h| h.starts_with("authorization")));
        assert_eq!(body["model"], OLLAMA_MODEL);
        assert_eq!(body["stream"], false);
        assert_eq!(body["options"]["num_predict"], 64);
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "Answer in one line."},
                {"role": "user", "content": "hello"}
            ])
        );
    }

    #[test]
    fn openai_compatible_servers_and_http_errors() {
        let (url, seen) = fake_server(|b| {
            if b["model"] == "broken" {
                (500, json!({"error": {"message": "model not loaded"}}))
            } else {
                (200, json!({"choices": [{"message": {"content": "ok"}}]}))
            }
        });
        let mut cfg = ChatConfig::new(Provider::OpenAiCompat);
        cfg.endpoint = format!("{url}/");
        assert_eq!(complete(&cfg, &[user("x")]).unwrap(), "ok");
        assert_eq!(seen.lock().unwrap()[0].0, "/v1/chat/completions");
        cfg.model = "broken".into();
        let err = complete(&cfg, &[user("x")]).unwrap_err();
        assert!(
            matches!(&err, ChatError::Http(m) if m.contains("500") && m.contains("model not loaded")),
            "{err}"
        );
    }

    #[test]
    fn anthropic_replies_are_parsed_and_its_body_is_shaped() {
        let v = json!({"content": [{"type": "text", "text": "Hello"}, {"type": "text", "text": " world"}]});
        assert_eq!(parse_reply(Provider::Anthropic, &v).unwrap(), "Hello world");
        let mut cfg = ChatConfig::new(Provider::Anthropic);
        cfg.system = "Be brief.".into();
        let (path, body) = request_body(&cfg, &[user("hi")]);
        assert_eq!(path, "/v1/messages");
        assert_eq!(body["model"], ANTHROPIC_MODEL);
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"], json!([{"role": "user", "content": "hi"}]));
        assert!(parse_reply(Provider::Ollama, &json!({})).is_err());
    }

    // Touches the process-wide Sovereign flag, so it holds the flags lock.
    #[test]
    fn sovereign_mode_refuses_anthropic_and_internet_endpoints() {
        let _flags = crate::egress::TEST_FLAGS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        egress::set_sovereign(true);
        let before = egress::refused();
        let err = complete(&ChatConfig::new(Provider::Anthropic), &[user("hi")]).unwrap_err();
        assert_eq!(
            err,
            ChatError::Sovereign {
                host: "api.anthropic.com".into()
            }
        );
        assert!(
            err.to_string()
                .contains("Sovereign mode blocks api.anthropic.com")
        );
        // An internet address for a local provider is refused by the gate,
        // before connecting (TEST-NET-3: nothing listens there).
        let mut cfg = ChatConfig::new(Provider::Ollama);
        cfg.endpoint = "http://203.0.113.9:11434".into();
        let err = complete(&cfg, &[user("hi")]).unwrap_err();
        assert!(
            matches!(&err, ChatError::Sovereign { host } if host == "203.0.113.9"),
            "{err}"
        );
        assert!(egress::refused() > before);
        egress::set_sovereign(true);
    }

    #[test]
    fn the_link_answers_prompts_and_keeps_history() {
        let (url, seen) = fake_server(|b| {
            let n = b["messages"].as_array().map_or(0, Vec::len);
            (
                200,
                json!({"message": {"content": format!("{n} messages")}}),
            )
        });
        let mut cfg = ChatConfig::new(Provider::Ollama);
        cfg.endpoint = url;
        cfg.history = 1;
        let link = ChatLink::new(cfg, 256);
        let mut seen_seq = 0;
        let mut replies = Vec::new();
        for prompt in ["one", "two", "three"] {
            let seq = link.ask(prompt).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let mut got = None;
                link.take(&mut seen_seq, |r| got = Some((r.seq, r.text.clone())));
                if let Some((s, text)) = got {
                    assert_eq!(s, seq);
                    replies.push(text);
                    break;
                }
                assert!(Instant::now() < deadline, "no reply");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // One earlier exchange is kept: 1, then 3, then 3 messages.
        assert_eq!(replies, ["1 messages", "3 messages", "3 messages"]);
        let last = &seen.lock().unwrap()[2].2;
        assert_eq!(last["messages"][0]["content"], "two");
        assert!(link.with_status(|s| s.replies == 3 && !s.busy && s.error.is_none()));
    }

    #[test]
    fn hosts_are_taken_from_urls() {
        assert_eq!(url_host("https://api.anthropic.com"), "api.anthropic.com");
        assert_eq!(url_host("http://127.0.0.1:11434/x"), "127.0.0.1");
        assert_eq!(url_host("http://[::1]:8080"), "::1");
        assert_eq!(url_host("studio.local"), "studio.local");
    }
}
