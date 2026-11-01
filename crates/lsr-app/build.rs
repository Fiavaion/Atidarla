//! On Windows, embeds the app icon (`packaging/icons/lasair.ico`, PRD A69) as
//! the executable's icon resource. Other platforms build nothing here.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("packaging")
            .join("icons")
            .join("lasair.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        winresource::WindowsResource::new()
            .set_icon(&icon.to_string_lossy())
            .compile()
            .expect("embedding the icon resource (needs the Windows SDK's rc.exe)");
    }
}
