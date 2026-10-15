//! Still images (PNG, JPEG) decoded to RGBA8. Callers run this off the
//! render thread: Movie In on its player thread, ISF imports on the
//! compile thread (rule 1).

use std::io::Cursor;
use std::path::Path;

/// An image as straight RGBA8, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// True for file names this module can decode.
pub fn is_still(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// Loads a PNG or JPEG file (detected by content, not extension).
pub fn load(path: &Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Decodes PNG or JPEG bytes.
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg(bytes)
    } else {
        Err("not a PNG or JPEG image".into())
    }
}

fn png(bytes: &[u8]) -> Result<Image, String> {
    let mut decoder = ::png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(::png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG too large to decode")?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in buf.chunks(info.line_size).take(h) {
        match info.color_type {
            ::png::ColorType::Rgba => rgba.extend_from_slice(&row[..w * 4]),
            ::png::ColorType::Rgb => {
                for p in row[..w * 3].chunks_exact(3) {
                    rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            ::png::ColorType::GrayscaleAlpha => {
                for p in row[..w * 2].chunks_exact(2) {
                    rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
                }
            }
            ::png::ColorType::Grayscale => {
                for &g in &row[..w] {
                    rgba.extend_from_slice(&[g, g, g, 255]);
                }
            }
            ::png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
        }
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

fn jpeg(bytes: &[u8]) -> Result<Image, String> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    let rgba = decoder.decode().map_err(|e| format!("{e:?}"))?;
    let (w, h) = decoder.dimensions().ok_or("JPEG without dimensions")?;
    Ok(Image {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_png(w: u32, h: u32, colour: ::png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = ::png::Encoder::new(&mut out, w, h);
        enc.set_color(colour);
        enc.set_depth(::png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn png_colour_types_become_rgba() {
        let rgb = encode_png(2, 1, ::png::ColorType::Rgb, &[255, 0, 0, 0, 0, 255]);
        assert_eq!(decode(&rgb).unwrap().rgba, [255, 0, 0, 255, 0, 0, 255, 255]);
        let grey = encode_png(1, 2, ::png::ColorType::GrayscaleAlpha, &[10, 20, 30, 40]);
        let img = decode(&grey).unwrap();
        assert_eq!((img.width, img.height), (1, 2));
        assert_eq!(img.rgba, [10, 10, 10, 20, 30, 30, 30, 40]);
        assert!(decode(b"GIF89a").is_err());
        assert!(is_still(Path::new("a/b.PNG")) && !is_still(Path::new("x.mov")));
    }

    #[test]
    fn jpeg_test_clip_frame_decodes() {
        // A JPEG made by FFmpeg from testsrc2 (see tests/data/README.md).
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/still.jpg");
        let img = load(&path).unwrap();
        assert_eq!((img.width, img.height), (64, 32));
        assert_eq!(img.rgba.len(), 64 * 32 * 4);
        let reference = std::fs::read(path.with_file_name("hap1.f2.rgba")).unwrap();
        // Lossy, but recognisably the same picture.
        let mean = img
            .rgba
            .iter()
            .zip(&reference)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / reference.len() as f64;
        assert!(mean < 12.0, "mean difference {mean}");
    }
}
