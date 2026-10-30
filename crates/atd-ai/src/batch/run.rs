//! The batch runner (FR-B1–B3): cached items are skipped, the rest go
//! from one queue to a worker per host slot, and each result lands in the
//! media bin as it finishes.
//!
//! An item whose image (`media/bin/<key>.png`) exists is not cooked again;
//! if this wedge does not list it yet (another wedge made it, or the index
//! was lost) it gets an entry. Items with the same key as an earlier item
//! of the run are cooked once. A failed item goes back on the queue for
//! any worker, up to `attempts` times; a worker whose host fails three
//! items in a row stops, and when every worker has stopped the items left
//! fail. The index is written after every item, so an interrupted run
//! keeps what it made and the next run picks up the rest.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use atd_core::graph::NodeId;
use atd_format::media_bin::{self, BinEntry, MediaBin};
use serde_json::Value as Json;

/// A worker stops after this many failures in a row.
const STRIKES: u32 = 3;

/// One parameter value a work item sets.
#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    pub node: NodeId,
    pub param: String,
    pub value: Json,
}

/// A work item: its overrides, its attributes for the bin and its key.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Position in the wedge.
    pub index: usize,
    /// Cache key ([`super::cache_key`]).
    pub key: String,
    pub attributes: BTreeMap<String, Json>,
    pub overrides: Vec<Override>,
}

/// A cooked image: tightly packed RGBA8, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Cooks items on one host slot. Made on the worker's own thread.
pub trait Cooker {
    fn cook(&mut self, item: &Item) -> Result<Image, String>;
}

/// How to run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The wedge's name (entries are filed under it).
    pub wedge: String,
    /// Hosts in order; each gets `per_host` workers.
    pub hosts: Vec<String>,
    pub per_host: u32,
    /// Cook every item, cached or not.
    pub force: bool,
    /// Tries per item.
    pub attempts: u32,
}

/// Progress, reported from the workers' threads.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    Cached {
        index: usize,
    },
    Cooked {
        index: usize,
        host: &'a str,
        ms: f64,
    },
    Retry {
        index: usize,
        host: &'a str,
        error: &'a str,
    },
    Failed {
        index: usize,
        error: &'a str,
    },
    /// A worker stopped: its cooker could not be made or its host kept failing.
    WorkerStopped {
        host: &'a str,
        error: &'a str,
    },
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub total: usize,
    /// Items not cooked: in the bin already, or the same as an earlier item.
    pub cached: usize,
    pub cooked: usize,
    pub failed: Vec<(usize, String)>,
    /// Items cooked per host (every host listed).
    pub per_host: BTreeMap<String, usize>,
    pub wall: Duration,
}

struct Shared {
    queue: VecDeque<(Item, u32)>,
    in_flight: usize,
    alive: usize,
    bin: MediaBin,
    report: Report,
    /// The first error writing the bin; ends the run.
    fatal: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The image file of `key` in the project at `dir`.
pub fn image_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(media_bin::image_rel(key))
}

fn encode_png(img: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&img.rgba))
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes the image beside its final name and renames it, so readers
/// (the app's bin) never see half a file.
fn write_image(dir: &Path, key: &str, img: &Image) -> Result<(), String> {
    let path = image_path(dir, key);
    let bytes = encode_png(img)?;
    let tmp = path
        .with_extension(format!("{:?}.tmp~", std::thread::current().id()).replace(['(', ')'], ""));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Width and height from a PNG file's header.
fn png_size(path: &Path) -> Option<[u32; 2]> {
    let f = std::fs::File::open(path).ok()?;
    let reader = png::Decoder::new(std::io::BufReader::new(f))
        .read_info()
        .ok()?;
    let info = reader.info();
    Some([info.width, info.height])
}

fn entry(wedge: &str, item: &Item, host: &str, size: [u32; 2], ms: f64) -> BinEntry {
    BinEntry {
        hash: item.key.clone(),
        wedge: wedge.to_owned(),
        index: item.index as u64,
        host: host.to_owned(),
        size,
        created: now_secs(),
        cook_ms: (ms * 10.0).round() / 10.0,
        attributes: item.attributes.clone(),
    }
}

/// Runs `items` of wedge `opts.wedge` for the project at `dir`. `make`
/// builds a worker's cooker for a host, on that worker's thread. Returns
/// once every item is cooked, cached or failed.
pub fn run<C, F>(
    dir: &Path,
    items: Vec<Item>,
    opts: &RunOptions,
    make: F,
    on: &(dyn Fn(Event<'_>) + Sync),
) -> Result<Report, String>
where
    C: Cooker,
    F: Fn(&str) -> Result<C, String> + Sync,
{
    let start = Instant::now();
    std::fs::create_dir_all(media_bin::bin_dir(dir))
        .map_err(|e| format!("{}: {e}", media_bin::bin_dir(dir).display()))?;
    let mut bin = media_bin::load_bin(dir).map_err(|e| e.to_string())?;
    let mut report = Report {
        total: items.len(),
        per_host: opts.hosts.iter().map(|h| (h.clone(), 0)).collect(),
        ..Report::default()
    };
    let mut queue = VecDeque::new();
    let mut seen = BTreeSet::new();
    let mut changed = false;
    for item in items {
        if !seen.insert(item.key.clone()) {
            report.cached += 1;
            on(Event::Cached { index: item.index });
            continue;
        }
        let path = image_path(dir, &item.key);
        if !opts.force && path.is_file() {
            report.cached += 1;
            if !bin.contains(&opts.wedge, &item.key) {
                let mut e = match bin.find(&item.key) {
                    Some(prev) => entry(&opts.wedge, &item, &prev.host, prev.size, prev.cook_ms),
                    None => entry(
                        &opts.wedge,
                        &item,
                        "unknown",
                        png_size(&path).unwrap_or([0, 0]),
                        0.0,
                    ),
                };
                if let Some(prev) = bin.find(&item.key) {
                    e.created = prev.created;
                }
                bin.insert(e);
                changed = true;
            }
            on(Event::Cached { index: item.index });
            continue;
        }
        queue.push_back((item, 0));
    }
    if changed {
        media_bin::save_bin(dir, &bin).map_err(|e| e.to_string())?;
    }
    let slots: Vec<&str> = opts
        .hosts
        .iter()
        .flat_map(|h| std::iter::repeat_n(h.as_str(), opts.per_host.max(1) as usize))
        .collect();
    if !queue.is_empty() && slots.is_empty() {
        return Err("no hosts to run the items on".into());
    }
    let shared = Mutex::new(Shared {
        queue,
        in_flight: 0,
        alive: slots.len(),
        bin,
        report,
        fatal: None,
    });
    let wake = Condvar::new();
    let attempts = opts.attempts.max(1);
    if !shared.lock().expect("batch").queue.is_empty() {
        std::thread::scope(|scope| {
            for host in &slots {
                let (shared, wake, make) = (&shared, &wake, &make);
                scope.spawn(move || worker(dir, host, opts, attempts, shared, wake, make, on));
            }
        });
    }
    let shared = shared.into_inner().expect("batch");
    if let Some(e) = shared.fatal {
        return Err(e);
    }
    let mut report = shared.report;
    report.failed.sort_by_key(|f| f.0);
    report.wall = start.elapsed();
    Ok(report)
}

/// Marks a worker gone; the last one fails what is left.
fn stop(s: &mut Shared, host: &str, error: &str, on: &(dyn Fn(Event<'_>) + Sync)) {
    on(Event::WorkerStopped { host, error });
    s.alive -= 1;
    if s.alive == 0 {
        while let Some((item, _)) = s.queue.pop_front() {
            let why = format!("no host left to run it (last: {host}: {error})");
            on(Event::Failed {
                index: item.index,
                error: &why,
            });
            s.report.failed.push((item.index, why));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn worker<C, F>(
    dir: &Path,
    host: &str,
    opts: &RunOptions,
    attempts: u32,
    shared: &Mutex<Shared>,
    wake: &Condvar,
    make: &F,
    on: &(dyn Fn(Event<'_>) + Sync),
) where
    C: Cooker,
    F: Fn(&str) -> Result<C, String> + Sync,
{
    let mut cooker = match make(host) {
        Ok(c) => c,
        Err(e) => {
            stop(&mut shared.lock().expect("batch"), host, &e, on);
            wake.notify_all();
            return;
        }
    };
    let mut strikes = 0;
    loop {
        let (item, tries) = {
            let mut s = shared.lock().expect("batch");
            loop {
                if s.fatal.is_some() {
                    return;
                }
                if let Some(next) = s.queue.pop_front() {
                    s.in_flight += 1;
                    break next;
                }
                if s.in_flight == 0 {
                    return;
                }
                s = wake.wait(s).expect("batch");
            }
        };
        let t0 = Instant::now();
        let result = cooker
            .cook(&item)
            .and_then(|img| write_image(dir, &item.key, &img).map(|()| img));
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        let mut s = shared.lock().expect("batch");
        s.in_flight -= 1;
        match result {
            Ok(img) => {
                strikes = 0;
                s.bin
                    .insert(entry(&opts.wedge, &item, host, [img.width, img.height], ms));
                if let Err(e) = media_bin::save_bin(dir, &s.bin) {
                    s.fatal = Some(e.to_string());
                }
                s.report.cooked += 1;
                *s.report.per_host.entry(host.to_owned()).or_default() += 1;
                on(Event::Cooked {
                    index: item.index,
                    host,
                    ms,
                });
            }
            Err(e) => {
                strikes += 1;
                let again = tries + 1 < attempts && (s.alive > 1 || strikes < STRIKES);
                if again {
                    on(Event::Retry {
                        index: item.index,
                        host,
                        error: &e,
                    });
                    s.queue.push_back((item, tries + 1));
                } else {
                    let why = format!("{host}: {e}");
                    on(Event::Failed {
                        index: item.index,
                        error: &why,
                    });
                    s.report.failed.push((item.index, why));
                }
                if strikes >= STRIKES {
                    stop(&mut s, host, &e, on);
                    drop(s);
                    wake.notify_all();
                    return;
                }
            }
        }
        drop(s);
        wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn items(n: usize, salt: &str) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                index: i,
                key: format!("{:064x}", i + salt.len() * 1000),
                attributes: BTreeMap::from([("seed".to_owned(), json!(i))]),
                overrides: vec![],
            })
            .collect()
    }

    struct Flat {
        fail: bool,
    }

    impl Cooker for Flat {
        fn cook(&mut self, item: &Item) -> Result<Image, String> {
            if self.fail {
                return Err("host down".into());
            }
            Ok(Image {
                width: 2,
                height: 1,
                rgba: vec![item.index as u8; 8],
            })
        }
    }

    fn opts(hosts: &[&str]) -> RunOptions {
        RunOptions {
            wedge: "w".into(),
            hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            per_host: 2,
            force: false,
            attempts: 3,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atd-batch-run-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn items_land_once_and_reruns_cook_nothing() {
        let dir = temp("rerun");
        let made = AtomicUsize::new(0);
        let make = |_: &str| {
            made.fetch_add(1, Ordering::Relaxed);
            Ok(Flat { fail: false })
        };
        let mut list = items(20, "");
        list.push(Item {
            index: 20,
            ..list[3].clone()
        });
        let r = run(
            &dir,
            list.clone(),
            &opts(&["lan:a", "lan:b"]),
            make,
            &|_| {},
        )
        .unwrap();
        assert_eq!((r.total, r.cooked, r.cached), (21, 20, 1));
        assert_eq!(r.per_host.values().sum::<usize>(), 20);
        let bin = media_bin::load_bin(&dir).unwrap();
        assert_eq!(bin.len(), 20);
        let img = std::fs::read(image_path(&dir, &list[5].key)).unwrap();
        assert!(img.starts_with(b"\x89PNG"));
        assert_eq!(png_size(&image_path(&dir, &list[5].key)), Some([2, 1]));
        // Again: nothing cooks, no worker is even made.
        made.store(0, Ordering::Relaxed);
        let r = run(&dir, list.clone(), &opts(&["lan:a"]), make, &|_| {}).unwrap();
        assert_eq!((r.cooked, r.cached), (0, 21));
        assert_eq!(made.load(Ordering::Relaxed), 0);
        // Another wedge with the same items gets entries without cooking.
        let mut o = opts(&["lan:a"]);
        o.wedge = "other".into();
        let r = run(&dir, list, &o, make, &|_| {}).unwrap();
        assert_eq!(r.cooked, 0);
        let bin = media_bin::load_bin(&dir).unwrap();
        assert_eq!(bin.len(), 40);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_host_hands_its_items_to_the_others() {
        let dir = temp("failover");
        let make = |h: &str| {
            Ok(Flat {
                fail: h == "lan:bad",
            })
        };
        let r = run(
            &dir,
            items(12, "x"),
            &opts(&["lan:bad", "lan:ok"]),
            make,
            &|_| {},
        )
        .unwrap();
        assert_eq!(r.cooked, 12, "{r:?}");
        assert!(r.failed.is_empty(), "{r:?}");
        assert_eq!(r.per_host["lan:bad"], 0);
        assert_eq!(r.per_host["lan:ok"], 12);
        // With no good host, everything fails with a reason.
        let dir2 = temp("allbad");
        let r = run(&dir2, items(5, "y"), &opts(&["lan:bad"]), make, &|_| {}).unwrap();
        assert_eq!(r.cooked, 0);
        assert_eq!(r.failed.len(), 5);
        assert!(r.failed.iter().all(|f| f.1.contains("host down")), "{r:?}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn a_worker_that_cannot_start_leaves_the_items_to_others() {
        let dir = temp("nostart");
        let make = |h: &str| {
            if h == "lan:x" {
                Err("cannot connect".to_owned())
            } else {
                Ok(Flat { fail: false })
            }
        };
        let r = run(
            &dir,
            items(6, "zz"),
            &opts(&["lan:x", "lan:y"]),
            make,
            &|_| {},
        )
        .unwrap();
        assert_eq!(r.cooked, 6);
        assert_eq!(r.per_host["lan:y"], 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
