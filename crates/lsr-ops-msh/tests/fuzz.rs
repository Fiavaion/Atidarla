//! Deterministic fuzzing of the mesh readers with damaged copies of valid
//! files: mesh files are untrusted, so damage must give errors, never
//! panics, and never one allocation over 64 MiB (a counting allocator
//! watches every allocation in this test binary).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use lsr_ops_msh::io::{self, Format, ObjPoints};
use lsr_ops_msh::{Column, Mesh};
use serde_json::json;

struct Watch;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Watch {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        LARGEST.fetch_max(new, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static ALLOC: Watch = Watch;

const CAP: usize = 64 << 20;

/// The two tests share `LARGEST`: one at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

fn corrupt(bytes: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = bytes.to_vec();
    for _ in 0..1 + rng.next(8) {
        if b.is_empty() {
            break;
        }
        let i = rng.next(b.len());
        match rng.next(6) {
            0 => b[i] = rng.next(256) as u8,
            // Size and index fields: huge or zero.
            1 => {
                for k in i..(i + 4).min(b.len()) {
                    b[k] = if rng.next(2) == 0 { 0xFF } else { 0 };
                }
            }
            2 => b.truncate(i),
            3 => {
                let n = rng.next(64);
                b.drain(i..(i + n).min(b.len()));
            }
            // Text formats: a digit becomes a large number.
            4 => {
                let digits = b"999999999";
                b.splice(i..i, digits.iter().copied());
            }
            _ => {
                let piece: Vec<u8> = b[i..(i + rng.next(32)).min(b.len())].to_vec();
                let at = rng.next(b.len());
                b.splice(at..at, piece);
            }
        }
    }
    b
}

fn sample() -> Mesh {
    let s = lsr_ops_msh::run(
        "msh.sphere",
        &[("rows", json!(6)), ("columns", json!(8))],
        &[],
    )
    .unwrap();
    let mut s = lsr_ops_msh::run("msh.uv_project", &[], &[Some(s)]).unwrap();
    let n = s.points();
    s.set_point_attr(Column::f32("Cd", 4, vec![0.5; n * 4]));
    let line = lsr_ops_msh::run("msh.line", &[], &[]).unwrap();
    lsr_ops_msh::edit::merged(&[&s, &line], &lsr_ops_msh::Job::new()).unwrap()
}

#[test]
fn damaged_mesh_files_never_panic_or_allocate_hugely() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let rounds = std::env::var("LSR_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    let m = sample();
    let originals: Vec<(Format, Vec<u8>)> = [Format::Obj, Format::Ply, Format::Gltf, Format::Glb]
        .into_iter()
        .map(|f| (f, io::write(&m, f).unwrap()))
        .chain([(
            Format::Ply,
            b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n".to_vec(),
        )])
        .collect();
    let mut rng = Rng(0x5EED_0FFA_CECA_FE01);
    let (mut ok, mut failed) = (0, 0);
    for _ in 0..rounds {
        let (format, original) = &originals[rng.next(originals.len())];
        let bytes = corrupt(original, &mut rng);
        LARGEST.store(0, Ordering::Relaxed);
        let result = match format {
            Format::Obj => io::read_obj(
                &bytes,
                if rng.next(2) == 0 {
                    ObjPoints::Split
                } else {
                    ObjPoints::Position
                },
            ),
            Format::Ply => io::read_ply(&bytes),
            Format::Gltf | Format::Glb => io::read_gltf(&bytes, None),
        };
        let largest = LARGEST.load(Ordering::Relaxed);
        assert!(
            largest <= CAP,
            "{format:?}: one allocation of {largest} bytes"
        );
        match result {
            Ok(mesh) => {
                // Whatever is accepted is a valid mesh later operators can
                // index freely.
                mesh.validate().unwrap();
                ok += 1;
            }
            Err(_) => failed += 1,
        }
    }
    eprintln!("{rounds} damaged files: {ok} read, {failed} refused");
    assert!(failed > rounds / 10, "the damage is real");
}

#[test]
fn hostile_counts_are_refused_before_allocating() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    LARGEST.store(0, Ordering::Relaxed);
    let ply = "ply\nformat binary_little_endian 1.0\nelement vertex 100000000\nproperty float x\nend_header\n";
    assert!(io::read_ply(ply.as_bytes()).is_err());
    let glb_json = json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{"componentType": 5126, "count": 4000000000u64, "type": "VEC3"}],
    });
    assert!(io::read_gltf(&serde_json::to_vec(&glb_json).unwrap(), None).is_err());
    let mut view = glb_json.clone();
    view["accessors"][0]["bufferView"] = json!(0);
    view["bufferViews"] = json!([{"buffer": 0, "byteLength": 12}]);
    view["buffers"] =
        json!([{"byteLength": 12, "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAA"}]);
    assert!(io::read_gltf(&serde_json::to_vec(&view).unwrap(), None).is_err());
    assert!(
        LARGEST.load(Ordering::Relaxed) <= 1 << 20,
        "{}",
        LARGEST.load(Ordering::Relaxed)
    );
}
