//! `atidarla bench`: the standard performance suite (PRD §8, FR-H4).
//!
//! M0 measures start-up costs only. Each milestone adds the PT-n cases it is
//! accountable for; results are compared across runs by case `id`.

use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use atd_core::PlatformInfo;
use atd_gpu::{AdapterReport, GpuContext};
use serde::Serialize;

/// Bumped whenever the JSON layout changes incompatibly.
const SCHEMA: &str = "atidarla-bench/1";

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    atidarla: &'static str,
    unix_time: u64,
    platform: PlatformInfo,
    adapter: Option<AdapterReport>,
    cases: Vec<Case>,
}

#[derive(Debug, Serialize)]
struct Case {
    id: &'static str,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ok,
    Skipped,
    Error,
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed().as_secs_f64() * 1e3)
}

pub fn run(out: Option<&Path>, use_gpu: bool) -> anyhow::Result<()> {
    let mut platform = PlatformInfo::host();
    let mut adapter = None;
    let mut cases = Vec::new();

    let (registry, ms) = timed(crate::builtin_registry);
    cases.push(Case {
        id: "startup.registry",
        status: Status::Ok,
        ms: Some(ms),
        note: Some(format!("{} operators registered", registry.len())),
    });

    if use_gpu {
        let (gpu, ms) = timed(GpuContext::headless);
        match gpu {
            Ok(gpu) => {
                platform = gpu.platform_info();
                adapter = Some(gpu.report());
                cases.push(Case {
                    id: "startup.gpu_device",
                    status: Status::Ok,
                    ms: Some(ms),
                    note: None,
                });
            }
            Err(err) => cases.push(Case {
                id: "startup.gpu_device",
                status: Status::Error,
                ms: None,
                note: Some(err.to_string()),
            }),
        }
    } else {
        cases.push(Case {
            id: "startup.gpu_device",
            status: Status::Skipped,
            ms: None,
            note: Some("--no-gpu".into()),
        });
    }

    let report = Report {
        schema: SCHEMA,
        atidarla: atd_core::VERSION,
        unix_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        platform,
        adapter,
        cases,
    };
    let json = serde_json::to_string_pretty(&report)?;
    match out {
        Some(path) => std::fs::write(path, json + "\n")
            .with_context(|| format!("writing {}", path.display()))?,
        None => println!("{json}"),
    }
    Ok(())
}
