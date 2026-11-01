//! The app icon (PRD A69): `packaging/icons/lasair.svg` is what
//! `make_icon.py` draws, `lasair.ico` holds the committed renders at every
//! Windows size, and the installers use it. Runs on every platform (WiX and
//! the renderer, sips, are not needed).

use std::path::{Path, PathBuf};
use std::process::Command;

fn icons() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/icons")
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `LSR_PYTHON` if set, else `python` on Windows and `python3` elsewhere.
fn python() -> String {
    std::env::var("LSR_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    })
}

#[test]
fn the_committed_svg_is_what_the_generator_draws() {
    let out = std::env::temp_dir().join(format!("lsr-app-icon-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let status = Command::new(python())
        .arg(icons().join("make_icon.py"))
        .arg(&out)
        .status()
        .expect("runs Python (set LSR_PYTHON)");
    assert!(status.success(), "make_icon.py failed: {status}");
    let drawn = read(&out.join("lasair.svg"));
    std::fs::remove_dir_all(&out).ok();
    assert!(
        drawn == read(&icons().join("lasair.svg")),
        "packaging/icons/lasair.svg is stale: run python3 packaging/icons/make_icon.py \
         and packaging/icons/render_icons.sh"
    );
}

fn u16_at(b: &[u8], i: usize) -> usize {
    u16::from_le_bytes([b[i], b[i + 1]]) as usize
}

fn u32_at(b: &[u8], i: usize) -> usize {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
}

/// RGBA pixels of an 8-bit RGBA PNG.
fn png_rgba(bytes: &[u8]) -> (usize, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(info.width, info.height);
    buf.truncate(info.buffer_size());
    (info.width as usize, buf)
}

#[test]
fn the_ico_holds_the_committed_renders_at_every_windows_size() {
    let ico = read(&icons().join("lasair.ico"));
    assert_eq!((u16_at(&ico, 0), u16_at(&ico, 2)), (0, 1), "an icon file");
    let count = u16_at(&ico, 4);
    let mut sizes = vec![];
    for n in 0..count {
        let entry = &ico[6 + 16 * n..6 + 16 * (n + 1)];
        let size = if entry[0] == 0 {
            256
        } else {
            entry[0] as usize
        };
        assert_eq!(u16_at(entry, 6), 32, "{size} px: 32 bits per pixel");
        let (len, offset) = (u32_at(entry, 8), u32_at(entry, 12));
        let body = &ico[offset..offset + len];
        let png = read(&icons().join(format!("png/lasair-{size}.png")));
        if size == 256 {
            assert!(body == png.as_slice(), "256 px is the PNG render itself");
        } else {
            // A BITMAPINFOHEADER image: BGRA rows bottom up, then the mask.
            assert_eq!(u32_at(body, 0), 40, "{size} px: a BMP image");
            assert_eq!((u32_at(body, 4), u32_at(body, 8)), (size, 2 * size));
            let (w, rgba) = png_rgba(&png);
            assert_eq!(w, size);
            let row = size * 4;
            for y in 0..size {
                let bmp = &body[40 + (size - 1 - y) * row..40 + (size - y) * row];
                for x in 0..size {
                    let p = &rgba[y * row + x * 4..y * row + x * 4 + 4];
                    let q = &bmp[x * 4..x * 4 + 4];
                    assert_eq!([q[2], q[1], q[0], q[3]], p, "{size} px, pixel ({x}, {y})");
                }
            }
        }
        sizes.push(size);
    }
    assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 256]);
}

fn wxs(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/windows")
        .join(name);
    String::from_utf8(read(&path)).unwrap()
}

#[test]
fn the_installers_show_the_icon() {
    let msi = wxs("lasair.wxs");
    assert!(msi.contains(
        r#"<Icon Id="LasairIcon.ico" SourceFile="$(Root)\packaging\icons\lasair.ico" />"#
    ));
    assert!(msi.contains(r#"<Property Id="ARPPRODUCTICON" Value="LasairIcon.ico" />"#));
    let shortcuts = msi.matches("<Shortcut ").count();
    assert_eq!(shortcuts, 2, "Start menu and desktop");
    assert_eq!(msi.matches(r#"Icon="LasairIcon.ico""#).count(), shortcuts);

    let bundle = wxs("bundle.wxs");
    assert!(bundle.contains(r#"IconSourceFile="$(Root)\packaging\icons\lasair.ico""#));
    assert!(bundle.contains(r#"LogoFile="$(Root)\packaging\icons\png\lasair-64.png""#));
    for file in ["lasair.ico", "png/lasair-64.png"] {
        assert!(icons().join(file).is_file(), "{file}");
    }
}
