//! Text that AI nodes publish by name (Speech to Text transcripts, LLM Chat
//! replies), until the engine carries DAT wires.
//!
//! The engine routes IMG, SIG, GEO and MAT results today; DAT, the wire
//! these nodes declare for their text, is not routed yet. Meanwhile a node
//! publishes its newest text under its `Publish As` name, another node reads
//! it with `Prompt From`, and the app can show it. Each name holds one text
//! with a sequence number that grows on every publish. Buffers keep their
//! capacity, so publishing and reading a text no longer than the last one
//! allocate nothing; both happen only when a new text arrives.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Default)]
struct Entry {
    seq: u64,
    text: String,
}

fn board() -> &'static Mutex<HashMap<String, Entry>> {
    static BOARD: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    BOARD.get_or_init(Mutex::default)
}

/// Publishes `text` under `name` (nothing for an empty name); returns its
/// sequence number.
pub fn publish(name: &str, text: &str) -> u64 {
    if name.is_empty() {
        return 0;
    }
    let mut b = board().lock().expect("text board");
    let e = match b.get_mut(name) {
        Some(e) => e,
        None => b.entry(name.to_owned()).or_insert_with(|| Entry {
            seq: 0,
            text: String::with_capacity(4096),
        }),
    };
    e.text.clear();
    e.text.push_str(text);
    e.seq += 1;
    e.seq
}

/// Sequence number of the newest text under `name` (0: none yet).
pub fn seq(name: &str) -> u64 {
    board()
        .lock()
        .expect("text board")
        .get(name)
        .map_or(0, |e| e.seq)
}

/// Calls `f` with the newest text under `name` and its sequence number.
pub fn read<R>(name: &str, f: impl FnOnce(u64, &str) -> R) -> Option<R> {
    let b = board().lock().expect("text board");
    b.get(name).map(|e| f(e.seq, &e.text))
}

/// The newest text under `name`, copied (for the app and tools).
pub fn get(name: &str) -> Option<(u64, String)> {
    read(name, |seq, text| (seq, text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_published_and_read_by_name() {
        assert_eq!(seq("text-test-a"), 0);
        assert_eq!(publish("text-test-a", "hello"), 1);
        assert_eq!(publish("text-test-a", "again"), 2);
        assert_eq!(get("text-test-a"), Some((2, "again".into())));
        assert_eq!(read("text-test-a", |_, t| t.len()), Some(5));
        assert_eq!(get("text-test-missing"), None);
        // An empty name publishes nothing.
        assert_eq!(publish("", "x"), 0);
        assert_eq!(get(""), None);
    }
}
