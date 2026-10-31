//! CLAUDE.md rule 2: SIG cooks allocate nothing after warm-up.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use atd_core::graph::{Edit, Network, Node, NodeId, Param};
use atd_core::registry::Registry;
use atd_gpu::engine::{Channels, Params, SigCtx, SigInputs, SigOp};

type Case = (
    String,
    NodeId,
    Vec<Option<NodeId>>,
    Box<dyn SigOp>,
    Channels,
);
use serde_json::json;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Only the cook thread is measured: I/O threads (OSC encoding, audio
    /// decoding) may allocate by design.
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn counted() -> bool {
    COUNTING.try_with(std::cell::Cell::get).unwrap_or(false)
}

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counted() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A temporary file, deleted when dropped (also when a test fails).
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// A one-second stereo WAV in the temp directory.
fn wav_file() -> TempFile {
    let path = std::env::temp_dir().join(format!("atd-alloc-{}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..48_000 {
        let v = ((i as f32 * 0.05).sin() * 10_000.0) as i16;
        w.write_sample(v).unwrap();
        w.write_sample(v).unwrap();
    }
    w.finalize().unwrap();
    TempFile(path)
}

#[test]
fn every_sig_operator_cooks_without_allocating() {
    let wav = wav_file();
    let mut reg = Registry::default();
    atd_ops_sig::register(&mut reg);
    let mut net = Network::default();
    let mut sig: HashMap<NodeId, Channels> = HashMap::new();

    // Two upstream sources with fixed channels.
    let source = |net: &mut Network, sig: &mut HashMap<NodeId, Channels>, chans: Channels| {
        let id = net.alloc_id();
        let name = net.unique_name("src");
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.constant", name),
            },
        )
        .unwrap();
        sig.insert(id, chans);
        id
    };
    let a = source(
        &mut net,
        &mut sig,
        Channels::single(&["x", "y"], &[0.25, 0.5]),
    );
    let b = source(&mut net, &mut sig, Channels::single(&["k"], &[2.0]));
    // An audio-rate block (44.1 kHz stereo, one 60 Hz frame).
    let mut block = Channels::default();
    block.set_block(&["left", "right"], 44_100.0, 735);
    for c in 0..2 {
        for (i, v) in block.channel_mut(c).iter_mut().enumerate() {
            *v = (i as f32 * 0.06).sin() * 0.5;
        }
    }
    let audio = source(&mut net, &mut sig, block);

    let mut cases: Vec<Case> = Vec::new();
    for def in atd_ops_sig::defs() {
        let schema = (def.schema)();
        let id = net.alloc_id();
        let name = net.unique_name("op");
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, schema.type_id.clone(), name),
            },
        )
        .unwrap();
        if schema.type_id == "sig.audio_file_in" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "file".into(),
                    value: Some(Param::constant(json!(wav.0.to_string_lossy()))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.audio_in" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "device".into(),
                    value: Some(Param::constant(json!(atd_io::audio_in::TEST_TONE))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.dmx_out" {
            // Loopback, so the test sends nowhere real.
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "host".into(),
                    value: Some(Param::constant(json!("127.0.0.1"))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.audio_out" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "device".into(),
                    value: Some(Param::constant(json!(atd_io::audio_out::NULL_OUTPUT))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.timecode_in" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "midi_device".into(),
                    value: Some(Param::constant(json!("virtual"))),
                },
            )
            .unwrap();
        }
        if schema.type_id.starts_with("sig.midi_") {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "device".into(),
                    value: Some(Param::constant(json!("virtual"))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.osc_in" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "port".into(),
                    value: Some(Param::constant(json!(0))),
                },
            )
            .unwrap();
        }
        if schema.type_id == "sig.select" {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: "channels".into(),
                    value: Some(Param::constant(json!("y x"))),
                },
            )
            .unwrap();
        }
        let inputs: Vec<Option<NodeId>> = if schema.type_id == "sig.audio_out" {
            vec![Some(audio)]
        } else {
            (0..schema.inputs.len())
                .map(|i| [Some(a), Some(b)].get(i).copied().flatten())
                .collect()
        };
        cases.push((
            schema.type_id,
            id,
            inputs,
            (def.create)(),
            Channels::default(),
        ));
    }

    let run = |cases: &mut Vec<Case>, frames: usize, t0: f64| {
        for f in 0..frames {
            for (_, id, inputs, op, out) in cases.iter_mut() {
                let mut ctx = SigCtx {
                    dat: atd_gpu::engine::DatIo::none(),
                    inputs: SigInputs::new(inputs, &sig),
                    output: out,
                    params: Params::new(&net, &reg, &sig, *id),
                    time: t0 + f as f64 * 0.016,
                    dt: 0.016,
                    frame: f as u64,
                };
                op.cook(&mut ctx).unwrap();
            }
        }
    };
    COUNTING.with(|c| c.set(true));
    let at_start = ALLOCS.load(Ordering::Relaxed);
    // Warm up until the audio file has decoded and the test tone streams.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        run(&mut cases, 3, 0.0);
        let loaded = ["sig.audio_file_in", "sig.audio_in"].iter().all(|want| {
            cases
                .iter()
                .any(|(ty, _, _, _, out)| ty == want && !out.is_empty())
        });
        // Let the null output open, so playback (resampling, the ring) is
        // what gets measured.
        std::thread::sleep(std::time::Duration::from_millis(20));
        if loaded {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audio file decoded and test tone streaming"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    run(&mut cases, 3, 0.0);
    assert!(
        ALLOCS.load(Ordering::Relaxed) > at_start,
        "the counter sees warm-up allocations"
    );
    let before = ALLOCS.load(Ordering::Relaxed);
    run(&mut cases, 100, 1.0);
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.with(|c| c.set(false));
    assert_eq!(
        allocs, 0,
        "SIG cooks allocated {allocs} times over 100 frames"
    );
    // Inputs from the outside world have no channels until messages arrive.
    let silent = ["sig.osc_in", "sig.midi_in"];
    for (ty, _, _, _, out) in cases.iter().filter(|c| !silent.contains(&c.0.as_str())) {
        assert!(!out.is_empty(), "{ty} produced channels");
    }
}
