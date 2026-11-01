//! Every relative link in the user manual (`docs/manual`) points at a file
//! that exists, so the manual cannot rot silently as docs move.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The targets of `[text](target)` links in `text`.
fn links(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        if let Some(end) = rest.find(')') {
            out.push(&rest[..end]);
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn manual_links_resolve() {
    let dir = root().join("docs/manual");
    let mut checked = 0;
    let mut broken = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("docs/manual exists") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for target in links(&text) {
            let target = target.split_whitespace().next().unwrap_or_default();
            if target.is_empty()
                || target.starts_with('#')
                || target.contains("://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let file = target.split('#').next().unwrap_or_default();
            checked += 1;
            if !dir.join(file).exists() {
                broken.push(format!(
                    "{}: {target}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        checked > 50,
        "only {checked} links found; is the manual there?"
    );
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}
