//! Values the parameter panel offers for string parameters with a
//! `suggest` key (device names). Enumerating devices can block, so lists
//! refresh on a background thread and the UI shows the last known list.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a list stays fresh.
const FRESH: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Cache {
    lists: HashMap<String, (Instant, Vec<String>)>,
    refreshing: Vec<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn enumerate(key: &str) -> Vec<String> {
    match key {
        "audio.inputs" => atd_io::audio_in::input_devices(),
        "midi.inputs" => {
            let mut v = vec!["virtual".to_owned()];
            v.extend(atd_io::midi::ports().0);
            v
        }
        "midi.outputs" => {
            let mut v = vec!["virtual".to_owned()];
            v.extend(atd_io::midi::ports().1);
            v
        }
        _ => Vec::new(),
    }
}

/// The last known list for `key` (possibly empty), starting a refresh in
/// the background when it is stale. Never blocks.
pub fn list(key: &str) -> Vec<String> {
    let Ok(mut c) = cache().try_lock() else {
        return Vec::new();
    };
    let stale = c.lists.get(key).is_none_or(|(at, _)| at.elapsed() > FRESH);
    if stale && !c.refreshing.iter().any(|k| k == key) {
        c.refreshing.push(key.to_owned());
        let key = key.to_owned();
        let _ = std::thread::Builder::new()
            .name("atd-enumerate".into())
            .spawn(move || {
                let values = enumerate(&key);
                if let Ok(mut c) = cache().lock() {
                    c.refreshing.retain(|k| *k != key);
                    c.lists.insert(key, (Instant::now(), values));
                }
            });
    }
    c.lists.get(key).map(|(_, v)| v.clone()).unwrap_or_default()
}
