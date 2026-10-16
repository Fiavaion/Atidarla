//! With the `ffmpeg` feature, compiles the C shim over FFmpeg
//! (`src/video/ffmpeg_shim.c`) and links FFmpeg's shared libraries.

use std::path::PathBuf;

const LIBS: [&str; 4] = ["avformat", "avcodec", "avutil", "swscale"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_FFMPEG").is_none() {
        return;
    }
    println!("cargo:rerun-if-changed=src/video/ffmpeg_shim.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");

    let mut includes = Vec::new();
    let mut search = Vec::new();
    if let Some(dir) = std::env::var_os("FFMPEG_DIR").map(PathBuf::from) {
        includes.push(dir.join("include"));
        search.push(dir.join("lib"));
    } else {
        for lib in LIBS {
            // Probe without emitting link flags: the shim must come first on
            // the linker line, before the libraries it uses.
            let found = pkg_config::Config::new()
                .cargo_metadata(false)
                .probe(&format!("lib{lib}"))
                .unwrap_or_else(|e| {
                    panic!(
                        "the `ffmpeg` feature needs FFmpeg's development files \
                         (lib{lib} not found by pkg-config: {e}); install them or set FFMPEG_DIR"
                    )
                });
            includes.extend(found.include_paths);
            search.extend(found.link_paths);
        }
    }
    let mut build = cc::Build::new();
    build.file("src/video/ffmpeg_shim.c").warnings(true);
    for dir in &includes {
        build.include(dir);
    }
    build.compile("atd_ffmpeg_shim");
    search.sort();
    search.dedup();
    for dir in search {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    for lib in LIBS {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
