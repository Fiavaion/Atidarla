use super::*;

fn temp_store(name: &str) -> Store {
    let dir = std::env::temp_dir().join(format!("lsr-models-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::new(dir.join("models"))
}

#[test]
fn the_catalogue_parses_and_every_entry_is_pinned_and_classed() {
    let cat = catalogue();
    for id in [
        "sd-turbo",
        "sdxl-turbo",
        "taesd",
        "taesdxl",
        "depth-anything-v2-small",
        "birefnet-lite",
        "dwpose",
        "raft",
        "swin2sr-x4",
        "whisper-base",
        "whisper-small",
    ] {
        assert!(cat.get(id).is_some(), "{id} missing");
    }
    for m in cat.iter() {
        assert_eq!(m.source.revision.len(), 40, "{}: pinned commit", m.id);
        assert!(!m.files.is_empty(), "{}", m.id);
        assert!(m.files.iter().all(|f| f.sha256.len() == 64 && f.size > 0));
        assert!(
            m.licence.url.contains(&m.source.revision) || m.licence.url.starts_with("https://")
        );
        assert!(!m.licence.id.is_empty());
    }
    // Vision models: at least one per task, commercial-safe preferred.
    for task in ["Depth", "Segment", "Pose", "Optical flow", "Upscale"] {
        assert!(
            cat.iter()
                .any(|m| m.task.starts_with(task) && m.adapter == "onnx"),
            "{task}"
        );
    }
    assert!(
        cat.iter()
            .filter(|m| m.licence.class == LicenceClass::NonCommercial)
            .count()
            <= 1
    );
    let sd = cat.get("sd-turbo").unwrap();
    assert_eq!(sd.licence.class, LicenceClass::Restricted);
    assert_eq!(sd.files.len(), 12);
    assert_eq!(
        sd.main_file().path,
        "unet/diffusion_pytorch_model.fp16.safetensors"
    );
    assert_eq!(
        sd.file_url(HUGGING_FACE, sd.main_file()),
        "https://huggingface.co/stabilityai/sd-turbo/resolve/b261bac6fd2cf515557d5d0707481eafa0485ec2/unet/diffusion_pytorch_model.fp16.safetensors"
    );
    assert_eq!(licence_class("taesd"), Some(LicenceClass::CommercialSafe));
    assert_eq!(licence_class("nope"), None);
    // Speech-to-text (FR-A13): faster-whisper conversions, MIT.
    for id in ["whisper-base", "whisper-small"] {
        let m = cat.get(id).unwrap();
        assert_eq!(m.adapter, "stt.whisper", "{id}");
        assert_eq!(m.licence.class, LicenceClass::CommercialSafe, "{id}");
        assert_eq!(m.main_file().path, "model.bin", "{id}");
    }
}

#[test]
fn bad_catalogues_are_refused() {
    let good = r#"format = 1
[[model]]
id = "m"
name = "M"
adapter = "onnx"
task = "t"
vram_mb = 1
platforms = "any"
main = "a.onnx"
licence = { id = "MIT", class = "commercial-safe", url = "https://x" }
source = { repo = "o/r", revision = "0123456789abcdef0123456789abcdef01234567" }
files = [{ path = "a.onnx", size = 1, sha256 = "0000000000000000000000000000000000000000000000000000000000000000" }]
"#;
    assert_eq!(parse_catalogue(good).unwrap().len(), 1);
    for (from, to, want) in [
        (
            "revision = \"0123456789abcdef0123456789abcdef01234567\"",
            "revision = \"main\"",
            "40-digit",
        ),
        (
            "class = \"commercial-safe\"",
            "class = \"free\"",
            "unknown licence class",
        ),
        (
            "path = \"a.onnx\"",
            "path = \"../a.onnx\"",
            "unsafe file path",
        ),
        (
            "main = \"a.onnx\"",
            "main = \"b.onnx\"",
            "is not in `files`",
        ),
        ("sha256 = \"0000", "sha256 = \"X000", "SHA-256"),
        ("url = \"https://x\"", "url = \"http://x\"", "https"),
    ] {
        let e = parse_catalogue(&good.replace(from, to)).unwrap_err();
        assert!(e.contains(want), "{to}: {e}");
    }
    let twice = format!("{good}{}", &good["format = 1\n".len()..]);
    assert!(
        parse_catalogue(&twice)
            .unwrap_err()
            .contains("listed twice")
    );
}

#[test]
fn paths_follow_a61() {
    let s = Store::new("/c/Lasair/models");
    assert_eq!(s.accepted_path(), Path::new("/c/Lasair/accepted.toml"));
    assert_eq!(
        s.file("sd-turbo", "unet/x.safetensors"),
        Path::new("/c/Lasair/models/sd-turbo/unet/x.safetensors")
    );
    assert_eq!(
        s.part("m", "onnx/model.onnx"),
        Path::new("/c/Lasair/models/m/onnx/model.onnx.part")
    );
    let root = models_root();
    if std::env::var_os("LSR_MODELS_DIR").is_none() {
        let tail = if cfg!(target_os = "macos") {
            "Library/Caches/Lasair/models"
        } else if cfg!(windows) {
            "Lasair\\models"
        } else {
            "lasair/models"
        };
        assert!(root.ends_with(tail), "{}", root.display());
    }
    assert_eq!(model_dir("taesd"), root.join("taesd"));
}

use std::path::Path;

#[test]
fn acceptances_round_trip_and_follow_the_licence() {
    let store = temp_store("accept");
    let cat = catalogue();
    let sd = cat.get("sd-turbo").unwrap();
    let taesd = cat.get("taesd").unwrap();
    let mut a = Acceptances::load(store.accepted_path()).unwrap();
    assert!(a.by_id.is_empty() && !a.is_accepted(sd));
    a.accept(taesd, "2026-10-30");
    a.accept(sd, "2026-10-30");
    a.save(store.accepted_path()).unwrap();
    let text = std::fs::read_to_string(store.accepted_path()).unwrap();
    let b = Acceptances::load(store.accepted_path()).unwrap();
    assert_eq!(a, b);
    assert_eq!(b.to_string(), text, "byte-stable");
    assert!(text.find("sd-turbo").unwrap() < text.find("taesd").unwrap());
    assert!(b.is_accepted(sd) && b.is_accepted(taesd));
    // A different licence URL (a new revision) needs a new acceptance.
    let mut moved = sd.clone();
    moved.licence.url.push_str("?v2");
    assert!(!b.is_accepted(&moved));
    let mut c = b.clone();
    assert!(c.revoke("sd-turbo") && !c.revoke("sd-turbo"));
    assert!(!c.is_accepted(sd));
    assert!(Acceptances::from_str("format = 1\nbogus = 2\n").is_err());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

fn write(store: &Store, id: &str, rel: &str, bytes: &[u8]) {
    let p = store.file(id, rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// A two-file model whose files are `b"hello"` and `b"world!"`.
fn tiny() -> Model {
    let text = r#"format = 1
[[model]]
id = "tiny"
name = "Tiny"
adapter = "onnx"
task = "t"
vram_mb = 1
platforms = "any"
main = "w/b.bin"
licence = { id = "MIT", class = "commercial-safe", url = "https://x" }
source = { repo = "o/r", revision = "0123456789abcdef0123456789abcdef01234567" }
files = [
  { path = "a.txt", size = 5, sha256 = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" },
  { path = "w/b.bin", size = 6, sha256 = "711e9609339e92b03ddc0a211827dba421f38f9ed8b9d806e1ffdd8c15ffa03d" },
]
"#;
    parse_catalogue(text).unwrap().get("tiny").unwrap().clone()
}

#[test]
fn status_and_verify_tell_ready_from_partial_and_corrupt() {
    let store = temp_store("status");
    let m = tiny();
    assert_eq!(status(&store, &m), Status::NotDownloaded);
    std::fs::create_dir_all(store.model_dir("tiny")).unwrap();
    std::fs::write(store.part("tiny", "a.txt"), b"he").unwrap();
    assert_eq!(status(&store, &m), Status::Partial);
    std::fs::remove_file(store.part("tiny", "a.txt")).unwrap();
    write(&store, "tiny", "a.txt", b"hello");
    assert_eq!(status(&store, &m), Status::Partial);
    let r = verify(&store, &m, &Progress::default());
    assert_eq!(r.status(), Status::Partial);
    assert_eq!(r.files[1].state, FileState::Missing);
    write(&store, "tiny", "w/b.bin", b"world!");
    assert_eq!(status(&store, &m), Status::Ready);
    let job = start_verify(store.clone(), m.clone());
    let r = job.wait().unwrap();
    assert_eq!(r.status(), Status::Ready, "{r:?}");
    // Same size, other bytes: only the full check sees it.
    write(&store, "tiny", "w/b.bin", b"WORLD!");
    assert_eq!(status(&store, &m), Status::Ready);
    let r = verify(&store, &m, &Progress::default());
    assert_eq!(r.status(), Status::Corrupt);
    assert!(matches!(r.files[1].state, FileState::WrongHash { .. }));
    write(&store, "tiny", "w/b.bin", b"world");
    assert_eq!(status(&store, &m), Status::Corrupt);
    // A cancelled check is not "ready".
    let p = Progress::default();
    p.cancel();
    write(&store, "tiny", "w/b.bin", b"world!");
    assert!(verify(&store, &m, &p).cancelled);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn missing_models_and_the_commercial_safe_filter() {
    let store = temp_store("missing");
    let cat = catalogue();
    let mut project = BTreeMap::new();
    for id in ["sd-turbo", "taesd"] {
        project.insert(id.to_owned(), pin(cat.get(id).unwrap()));
    }
    project.insert(
        "gone".to_owned(),
        ModelPin {
            licence_class: LicenceClass::CommercialSafe,
            sha256: "0".repeat(64),
        },
    );
    let mut old = pin(cat.get("depth-anything-v2-small").unwrap());
    old.sha256 = "1".repeat(64);
    project.insert("depth-anything-v2-small".to_owned(), old);
    // taesd present with the right sizes.
    let taesd = cat.get("taesd").unwrap();
    for f in &taesd.files {
        let p = store.file("taesd", &f.path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::File::create(&p).unwrap().set_len(f.size).unwrap();
    }
    let m = missing(&store, cat, &project);
    assert_eq!(m.get("gone"), Some(&Missing::Unknown));
    assert_eq!(
        m.get("sd-turbo"),
        Some(&Missing::NotReady(Status::NotDownloaded))
    );
    assert!(matches!(
        m.get("depth-anything-v2-small"),
        Some(Missing::OtherVersion { .. })
    ));
    assert!(!m.contains_key("taesd"));

    // The filter.
    assert!(allowed(true, LicenceClass::CommercialSafe));
    assert!(!allowed(true, LicenceClass::Restricted));
    assert!(!allowed(true, LicenceClass::NonCommercial));
    assert!(allowed(false, LicenceClass::NonCommercial));
    let shown: Vec<&str> = visible(cat, true).map(|m| m.id.as_str()).collect();
    assert!(
        shown.contains(&"taesd") && !shown.contains(&"sd-turbo") && !shown.contains(&"sdxl-turbo")
    );
    assert_eq!(visible(cat, false).count(), cat.len());
    assert_eq!(blocked(cat, true, &project), vec!["sd-turbo"]);
    assert!(blocked(cat, false, &project).is_empty());
    // A project file that claims sd-turbo is commercial-safe does not fool it.
    let mut lying = project.clone();
    lying.get_mut("sd-turbo").unwrap().licence_class = LicenceClass::CommercialSafe;
    assert_eq!(blocked(cat, true, &lying), vec!["sd-turbo"]);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}
