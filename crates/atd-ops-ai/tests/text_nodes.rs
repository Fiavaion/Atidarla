//! Speech to Text and LLM Chat (FR-A13), cooked as the engine cooks SIG
//! nodes, against the reference host in this process (`builtin.stt_echo`)
//! and a fake Ollama on loopback: no network, no models. The cook never
//! waits, steady frames allocate nothing (rule 2), speech is sent after a
//! pause, transcripts chain into a chat prompt, and Sovereign mode refuses
//! the Anthropic provider with a node error.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use atd_ai::refhost::{RefHostOptions, serve};
use atd_ai::supervisor::{HostKind, HostSpec, Supervisor};
use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Channels, CookMode, Params, SigCtx, SigInputs, SigOp};
use atd_ops_ai::{chat, stt, text};
use serde_json::{Value, json};

struct Counting;
thread_local! {
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
// SAFETY: forwards to the system allocator; only counts calls on threads
// that turned counting on.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f` on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCS.with(std::cell::Cell::get);
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCS.with(std::cell::Cell::get) - before
}

/// A supervisor whose `local` host is a reference host in this process.
fn supervisor() -> &'static Supervisor {
    static SUP: OnceLock<&'static Supervisor> = OnceLock::new();
    SUP.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || serve(listener, RefHostOptions::default()));
        let sup: &'static Supervisor = Box::leak(Box::new(Supervisor::new()));
        sup.set(HostSpec {
            name: "local".into(),
            kind: HostKind::Remote { addr, token: None },
        });
        sup
    })
}

/// One node of type `ty` with `params`.
struct Rig {
    reg: Registry,
    net: Network,
    id: NodeId,
    op: Box<dyn SigOp>,
    out: Channels,
    sig: HashMap<NodeId, Channels>,
    /// The node wired into input 0 (a stand-in source holding `sig`).
    src: NodeId,
}

impl Rig {
    fn new(ty: &str, params: &[(&str, Value)]) -> Self {
        let mut reg = Registry::default();
        atd_ops_ai::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, ty, "n1"),
            },
        )
        .unwrap();
        let op = match ty {
            "ai.stt" => stt::create(supervisor()),
            _ => chat::create(),
        };
        let mut rig = Self {
            reg,
            net,
            id,
            op,
            out: Channels::default(),
            sig: HashMap::new(),
            src: NodeId(999),
        };
        for (p, v) in params {
            rig.set(p, v.clone());
        }
        rig
    }

    fn set(&mut self, param: &str, value: Value) {
        self.net
            .apply(
                &self.reg,
                Edit::SetParam {
                    node: self.id,
                    param: param.into(),
                    value: Some(Param::constant(value)),
                },
            )
            .unwrap();
    }

    /// Cooks once with `input` (if any) on input 0.
    fn cook(&mut self, input: Option<&Channels>) -> Result<(), String> {
        self.cook_in(CookMode::Live, input)
    }

    fn cook_in(&mut self, mode: CookMode, input: Option<&Channels>) -> Result<(), String> {
        match input {
            Some(c) => {
                self.sig.entry(self.src).or_default().copy_from(c);
            }
            None => {
                self.sig.remove(&self.src);
            }
        }
        let sources = [input.map(|_| self.src)];
        let mut ctx = SigCtx {
            dat: atd_gpu::engine::DatIo::none(),
            inputs: SigInputs::new(&sources, &self.sig),
            output: &mut self.out,
            params: Params::new(&self.net, &self.reg, &self.sig, self.id).with_mode(mode),
            time: 0.0,
            dt: 1.0 / 60.0,
            frame: 0,
        };
        self.op.cook(&mut ctx)
    }

    fn value(&self, name: &str) -> f32 {
        self.out.value(name).unwrap_or(f32::NAN)
    }
}

/// An audio block: 800 samples at 48 kHz (a 60 fps frame), `amp` sine.
fn block(buf: &mut Channels, amp: f32) {
    buf.set_block(&["left", "right"], 48_000.0, 800);
    for c in 0..2 {
        for (i, s) in buf.channel_mut(c).iter_mut().enumerate() {
            *s = amp * (i as f32 * 0.05).sin();
        }
    }
}

#[test]
fn speech_is_sent_after_a_pause_and_published() {
    let mut rig = Rig::new(
        "ai.stt",
        &[
            ("model", json!("builtin.stt_echo")),
            ("threshold", json!(0.1)),
            ("hang_ms", json!(100.0)),
            ("min_ms", json!(50.0)),
            ("publish", json!("test-speech")),
        ],
    );
    let mut audio = Channels::default();
    // Room tone, then 20 frames of speech, then silence.
    block(&mut audio, 0.01);
    for _ in 0..5 {
        rig.cook(Some(&audio)).unwrap();
    }
    assert_eq!(rig.value("active"), 0.0);
    block(&mut audio, 0.5);
    let mut slowest = Duration::ZERO;
    for _ in 0..20 {
        let t = Instant::now();
        rig.cook(Some(&audio)).unwrap();
        slowest = slowest.max(t.elapsed());
    }
    assert_eq!(rig.value("active"), 1.0);
    assert!(rig.value("level") > 0.3, "{}", rig.value("level"));
    block(&mut audio, 0.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let t = Instant::now();
        rig.cook(Some(&audio)).unwrap();
        slowest = slowest.max(t.elapsed());
        if rig.value("new_text") == 1.0 {
            break;
        }
        assert!(Instant::now() < deadline, "no transcript");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        slowest < Duration::from_millis(50),
        "a cook waited {slowest:?}"
    );
    assert_eq!((rig.value("count"), rig.value("active")), (1.0, 0.0));
    assert_eq!(rig.value("ready"), 1.0);
    let (_, heard) = text::get("test-speech").expect("published");
    // 5 frames of pre-roll room tone (0.25 s cap: all of it), 20 frames of
    // speech and the 100 ms hang in whole frames, at 16 kHz: 1333 + 5333 +
    // 1600 to 1867.
    let n: usize = heard.split(' ').next().unwrap().parse().unwrap();
    assert!((8_260..=8_540).contains(&n), "{heard}");
    assert!(
        heard.ends_with("samples at 16000 Hz, peak 0.500"),
        "{heard}"
    );
    rig.cook(Some(&audio)).unwrap();
    assert_eq!(rig.value("new_text"), 0.0, "the pulse lasts one frame");
}

#[test]
fn steady_frames_allocate_nothing_and_bursts_are_dropped() {
    let mut rig = Rig::new(
        "ai.stt",
        &[
            ("model", json!("builtin.stt_echo")),
            ("threshold", json!(0.1)),
            ("hang_ms", json!(50.0)),
            ("min_ms", json!(1000.0)),
            ("publish", json!("test-burst")),
        ],
    );
    let (mut loud, mut quiet) = (Channels::default(), Channels::default());
    block(&mut loud, 0.5);
    block(&mut quiet, 0.0);
    // Warm-up: the link starts, the output channels get their names.
    for _ in 0..10 {
        rig.cook(Some(&quiet)).unwrap();
    }
    let n = allocations(|| {
        for i in 0..120 {
            // Bursts of 3 frames (50 ms): shorter than the minimum.
            let input = if i % 20 < 3 { &loud } else { &quiet };
            rig.cook(Some(input)).unwrap();
        }
    });
    assert_eq!(n, 0, "steady frames allocated");
    std::thread::sleep(Duration::from_millis(200));
    rig.cook(Some(&quiet)).unwrap();
    assert_eq!((rig.value("count"), rig.value("busy")), (0.0, 0.0));
    assert!(text::get("test-burst").is_none());
}

/// A loopback server answering like Ollama's `/api/chat`; returns its URL
/// and the user message of each request.
fn fake_ollama() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let msgs = body["messages"].as_array().unwrap();
            let prompt = msgs.last().unwrap()["content"].as_str().unwrap().to_owned();
            let answer =
                json!({"message": {"role": "assistant", "content": format!("echo: {prompt}")}})
                    .to_string();
            log.lock().unwrap().push(prompt);
            let mut conn = conn;
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (url, seen)
}

fn wait_reply(rig: &mut Rig, trigger: Option<&Channels>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let r = rig.cook(trigger);
        if rig.value("new_reply") == 1.0 || r.is_err() {
            return r;
        }
        assert!(Instant::now() < deadline, "no reply");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn transcripts_chain_into_chat_and_replies_are_published() {
    let (url, seen) = fake_ollama();
    let mut rig = Rig::new(
        "ai.llm_chat",
        &[
            ("endpoint", json!(url)),
            ("prompt", json!("Make it an image prompt:")),
            ("prompt_from", json!("test-chain")),
            ("publish", json!("test-reply")),
        ],
    );
    text::publish("test-chain", "said before the node existed");
    for _ in 0..5 {
        rig.cook(None).unwrap();
    }
    assert!(seen.lock().unwrap().is_empty(), "sent an old text");
    text::publish("test-chain", "a red fox in the snow");
    wait_reply(&mut rig, None).unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["Make it an image prompt:\n\na red fox in the snow"]
    );
    assert_eq!(
        text::get("test-reply").unwrap().1,
        "echo: Make it an image prompt:\n\na red fox in the snow"
    );
    assert_eq!((rig.value("replies"), rig.value("error")), (1.0, 0.0));
    rig.cook(None).unwrap();
    assert_eq!(rig.value("new_reply"), 0.0);
    // A rising trigger input sends again; holding it high does not.
    rig.set("prompt_from", json!(""));
    rig.set("prompt", json!("again"));
    let high = Channels::single(&["go"], &[1.0]);
    wait_reply(&mut rig, Some(&high)).unwrap();
    for _ in 0..20 {
        rig.cook(Some(&high)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(seen.lock().unwrap().len(), 2);
    // The Send pulse.
    rig.set("send", json!(true));
    wait_reply(&mut rig, None).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(rig.value("replies"), 3.0);
    // Waiting frames allocate nothing.
    rig.set("send", json!(false));
    rig.cook(None).unwrap();
    let n = allocations(|| {
        for _ in 0..60 {
            rig.cook(None).unwrap();
        }
    });
    assert_eq!(n, 0, "waiting frames allocated");
}

#[test]
fn sovereign_mode_refuses_anthropic_with_a_node_error() {
    // Sovereign mode is on by default and no test here turns it off.
    assert!(atd_ai::egress::sovereign());
    let mut rig = Rig::new(
        "ai.llm_chat",
        &[
            ("provider", json!("anthropic")),
            ("prompt", json!("hello")),
            ("publish", json!("test-sovereign")),
        ],
    );
    rig.cook(None).unwrap();
    rig.set("send", json!(true));
    let err = wait_reply(&mut rig, None).unwrap_err();
    assert!(
        err.contains("Sovereign mode blocks api.anthropic.com"),
        "{err}"
    );
    assert_eq!(rig.value("error"), 1.0);
    assert!(text::get("test-sovereign").is_none());
}

#[test]
fn offline_renders_send_and_publish_nothing() {
    let (url, seen) = fake_ollama();
    let offline = CookMode::Offline { fps: 60.0 };
    let mut chat = Rig::new(
        "ai.llm_chat",
        &[
            ("endpoint", json!(url)),
            ("prompt", json!("hi")),
            ("send", json!(true)),
            ("publish", json!("test-offline-reply")),
        ],
    );
    let mut stt = Rig::new(
        "ai.stt",
        &[
            ("model", json!("builtin.stt_echo")),
            ("hang_ms", json!(50.0)),
            ("min_ms", json!(0.0)),
            ("publish", json!("test-offline-speech")),
        ],
    );
    let (mut loud, mut quiet) = (Channels::default(), Channels::default());
    block(&mut loud, 0.5);
    block(&mut quiet, 0.0);
    for i in 0..60 {
        chat.cook_in(offline, None).unwrap();
        let input = if i < 20 { &loud } else { &quiet };
        stt.cook_in(offline, Some(input)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!((chat.value("busy"), chat.value("replies")), (0.0, 0.0));
    assert_eq!((stt.value("active"), stt.value("count")), (0.0, 0.0));
    assert!(text::get("test-offline-speech").is_none());
    assert!(text::get("test-offline-reply").is_none());
}
