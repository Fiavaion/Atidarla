//! A reference AI Host in Rust: protocol v1 with the built-in models only
//! (`builtin.invert`, `builtin.echo`). Engine tests and CI use it to drive
//! the client, supervisor and AI nodes without Python or model weights,
//! including killing it mid-run (PT-9). The real host is `ai-host/`.
//!
//! Run as `atd-ai-refhost --listen 127.0.0.1:0 [--token T] [--parent-pid P]
//! [--fake-vendor V] [--delay-ms N]`; it prints
//! `atd-ai-host listening on <addr>` like the Python host.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::protocol::{PROTOCOL_VERSION, codes};
use crate::ring::FrameRing;

/// Options of a reference host.
#[derive(Debug, Clone, Default)]
pub struct RefHostOptions {
    pub token: Option<String>,
    pub vendor: Option<String>,
    /// Added to every frame, to model a slow GPU.
    pub delay: Duration,
}

/// Serves sessions on `listener` until the process ends.
pub fn serve(listener: TcpListener, opts: RefHostOptions) {
    let opts = Arc::new(opts);
    let started = Instant::now();
    let frames = Arc::new(AtomicU64::new(0));
    for conn in listener.incoming().flatten() {
        let opts = opts.clone();
        let frames = frames.clone();
        std::thread::spawn(move || session(conn, &opts, started, &frames));
    }
}

fn reply(id: &Value, result: Result<Value, (i64, String)>) -> Value {
    match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    }
}

fn write(w: &Mutex<TcpStream>, msg: &Value, payload: Option<&[u8]>) {
    let mut msg = msg.clone();
    if let Some(p) = payload {
        msg["payload"] = json!(p.len());
    }
    let mut line = serde_json::to_vec(&msg).expect("json");
    line.push(b'\n');
    let mut w = w.lock().expect("writer");
    let _ = w.write_all(&line);
    if let Some(p) = payload {
        let _ = w.write_all(p);
    }
}

/// A frame's result and output pixels (inline), or an error code and message.
type FrameOutcome = Result<(Value, Option<Vec<u8>>), (i64, String)>;

struct Job {
    id: Value,
    params: Value,
    pixels: Option<Vec<u8>>,
}

fn session(conn: TcpStream, opts: &RefHostOptions, started: Instant, frames: &AtomicU64) {
    let _ = conn.set_nodelay(true);
    let Ok(w) = conn.try_clone() else { return };
    let writer = Arc::new(Mutex::new(w));
    let ring: Arc<Mutex<Option<FrameRing>>> = Arc::default();
    let models: Arc<Mutex<Vec<String>>> = Arc::default();
    let (jobs, rx) = channel::<Job>();
    // Frames run in order on their own thread so health stays answered.
    let worker = {
        let writer = writer.clone();
        let ring = ring.clone();
        let models = models.clone();
        let delay = opts.delay;
        std::thread::spawn(move || {
            for job in rx {
                let t0 = Instant::now();
                std::thread::sleep(delay);
                let mut ring = ring.lock().expect("ring");
                let r = frame(&job, ring.as_mut(), &models.lock().expect("models"));
                let host_ms = t0.elapsed().as_secs_f64() * 1e3;
                match r {
                    Ok((mut result, out)) => {
                        result["host_ms"] = json!(host_ms);
                        write(&writer, &reply(&job.id, Ok(result)), out.as_deref());
                    }
                    Err(e) => write(&writer, &reply(&job.id, Err(e)), None),
                }
            }
        })
    };
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    let mut open = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            break;
        };
        let id = msg["id"].clone();
        let p = msg["params"].clone();
        let pixels = match msg["payload"].as_u64() {
            Some(n) => {
                let mut buf = vec![0; n as usize];
                if reader.read_exact(&mut buf).is_err() {
                    break;
                }
                Some(buf)
            }
            None => None,
        };
        let method = msg["method"].as_str().unwrap_or_default();
        if !open && method != "session.open" {
            write(
                &writer,
                &reply(
                    &id,
                    Err((codes::UNAUTHORIZED, "open a session first".into())),
                ),
                None,
            );
            continue;
        }
        let r: Result<Value, (i64, String)> = match method {
            "session.open" => {
                if p["protocol_version"] != PROTOCOL_VERSION {
                    Err((
                        codes::VERSION_MISMATCH,
                        format!("host speaks {PROTOCOL_VERSION}"),
                    ))
                } else if opts.token.is_some() && p["token"].as_str() != opts.token.as_deref() {
                    Err((codes::UNAUTHORIZED, "wrong or missing token".into()))
                } else {
                    let kind = p["transport"]["kind"].as_str().unwrap_or_default();
                    let mut r = Ok(());
                    if kind == "shm" {
                        match FrameRing::open(
                            p["transport"]["ring_path"].as_str().unwrap_or_default(),
                        ) {
                            Ok(opened) => *ring.lock().expect("ring") = Some(opened),
                            Err(e) => r = Err((codes::UNSUPPORTED, e.to_string())),
                        }
                    }
                    r.map(|()| {
                        open = true;
                        json!({
                            "session_id": format!("ref-{}", std::process::id()),
                            "protocol_version": PROTOCOL_VERSION,
                            "host_name": "atd-ai-refhost",
                            "host_version": env!("CARGO_PKG_VERSION"),
                            "transport": kind,
                            "codec": if kind == "inline" { json!("rgba8") } else { Value::Null },
                        })
                    })
                }
            }
            "host.capabilities" => {
                let vendor = opts.vendor.clone().unwrap_or_else(|| "cpu".into());
                Ok(json!({
                    "vendor": vendor,
                    "devices": [{"name": "reference", "vendor": vendor, "vram_mb": null}],
                    "vram_mb": null,
                    "runtimes": ["cpu-reference"],
                    "transports": ["shm", "inline"],
                    "adapters": ["builtin", "diffusion.sd"],
                    "models": ["builtin.echo", "builtin.invert", "builtin.prompts"],
                }))
            }
            "host.health" => Ok(json!({
                "uptime_s": started.elapsed().as_secs_f64(),
                "frames_processed": frames.load(Ordering::Acquire),
                "loaded_models": *models.lock().expect("models"),
                "queue_depth": 0,
                "gpu_mem_mb": null,
            })),
            "model.load" => {
                let m = p["model_id"].as_str().unwrap_or_default().to_owned();
                if m == "builtin.invert" || m == "builtin.echo" || m == "builtin.prompts" {
                    let mut models = models.lock().expect("models");
                    if !models.contains(&m) {
                        models.push(m.clone());
                    }
                    Ok(
                        json!({"model_id": m, "load_ms": 0.0, "adapter": "builtin", "device": "cpu"}),
                    )
                } else {
                    Err((codes::UNKNOWN_MODEL, format!("unknown model {m:?}")))
                }
            }
            "model.unload" => {
                let m = p["model_id"].as_str().unwrap_or_default();
                models.lock().expect("models").retain(|x| x != m);
                Ok(Value::Null)
            }
            "frame.submit" => {
                frames.fetch_add(1, Ordering::AcqRel);
                let _ = jobs.send(Job {
                    id,
                    params: p,
                    pixels,
                });
                continue;
            }
            "frame.cancel" | "session.close" => Ok(Value::Null),
            other => Err((-32601, format!("unknown method {other:?}"))),
        };
        write(&writer, &reply(&id, r), None);
        if method == "session.close" {
            break;
        }
    }
    drop(jobs);
    let _ = worker.join();
}

fn frame(job: &Job, ring: Option<&mut FrameRing>, models: &[String]) -> FrameOutcome {
    let p = &job.params;
    let model = p["model_id"].as_str().unwrap_or_default();
    if !models.iter().any(|m| m == model) {
        return Err((codes::UNKNOWN_MODEL, format!("model {model:?} not loaded")));
    }
    let seq = p["seq"].as_u64().unwrap_or(0);
    let (w, h) = (
        p["width"].as_u64().unwrap_or(0),
        p["height"].as_u64().unwrap_or(0),
    );
    let len = (w * h * 4) as usize;
    let invert = model == "builtin.invert";
    // `builtin.prompts` stands in for a diffusion model in tests: it paints
    // the weighted mix of a colour per prompt, so a prompt blend shows.
    let prompted = (model == "builtin.prompts").then(|| prompt_colour(&p["params"]["prompts"]));
    let apply = |src: &[u8], dst: &mut [u8]| {
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            if let Some(c) = prompted {
                d.copy_from_slice(&c);
            } else if invert {
                d.copy_from_slice(&[255 - s[0], 255 - s[1], 255 - s[2], s[3]]);
            } else {
                d.copy_from_slice(s);
            }
        }
    };
    let mut result = json!({"seq": seq, "width": w, "height": h, "skipped": false});
    match (ring, &job.pixels) {
        (Some(ring), None) => {
            let (Some(src), Some(dst)) = (p["input_slot"].as_u64(), p["output_slot"].as_u64())
            else {
                return Err((
                    codes::BAD_SLOT,
                    "shm frames need input and output slots".into(),
                ));
            };
            let count = u64::from(ring.layout().slot_count);
            if src >= count || dst >= count || len > ring.layout().frame_bytes() {
                return Err((codes::BAD_SLOT, "slot or size out of range".into()));
            }
            if ring.seq(src as u32) != seq {
                return Err((codes::BAD_SLOT, format!("slot {src} holds another frame")));
            }
            let input = ring.frame(src as u32)[..len].to_vec();
            apply(&input, &mut ring.frame_mut(dst as u32)[..len]);
            ring.set_seq(dst as u32, seq);
            result["output_slot"] = json!(dst);
            Ok((result, None))
        }
        (_, Some(pixels)) if pixels.len() == len => {
            let mut out = vec![0; len];
            apply(pixels, &mut out);
            Ok((result, Some(out)))
        }
        _ => Err((codes::BAD_SLOT, "frame size does not match".into())),
    }
}

/// The weighted mix of each prompt's colour (an FNV-1a hash of its text).
fn prompt_colour(prompts: &Value) -> [u8; 4] {
    let mut sum = [0.0f64; 3];
    let mut total = 0.0;
    for p in prompts.as_array().into_iter().flatten() {
        let w = p["weight"].as_f64().unwrap_or(0.0);
        let mut h: u32 = 0x811c_9dc5;
        for b in p["text"].as_str().unwrap_or_default().bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        for (i, s) in sum.iter_mut().enumerate() {
            *s += w * f64::from((h >> (8 * i)) as u8);
        }
        total += w;
    }
    if total <= 0.0 {
        return [0, 0, 0, 255];
    }
    let c = |i: usize| (sum[i] / total).round() as u8;
    [c(0), c(1), c(2), 255]
}

/// Exits the process once `pid` is gone (checked every second), so a host
/// never outlives a crashed engine.
pub fn watch_parent(pid: u32) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if !process_alive(pid) {
                std::process::exit(0);
            }
        }
    });
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // `kill -0` semantics without libc: /proc is not on macOS, so ask ps.
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}
