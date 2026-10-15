//! CPU decoding of BC1/BC3/BC4 blocks and HAP Q's scaled YCoCg: the
//! fallback for devices without BC texture support (CLAUDE.md rule 6),
//! and the reference the GPU path is tested against.

use super::hap::{HapFormat, HapFrame};

fn rgb565(c: u16) -> [u8; 3] {
    let r = (c >> 11) & 31;
    let g = (c >> 5) & 63;
    let b = c & 31;
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

/// Colour part of a BC1/BC3 block. BC3 colour blocks always use the
/// four-colour mode.
fn colour_block(b: &[u8], four_colour_only: bool, out: &mut [[u8; 4]; 16]) {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mix = |a: u8, b: u8, wa: u16, wb: u16, d: u16| {
        ((u16::from(a) * wa + u16::from(b) * wb) / d) as u8
    };
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [p0[0], p0[1], p0[2], 255];
    palette[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || four_colour_only {
        for k in 0..3 {
            palette[2][k] = mix(p0[k], p1[k], 2, 1, 3);
            palette[3][k] = mix(p0[k], p1[k], 1, 2, 3);
        }
        palette[2][3] = 255;
        palette[3][3] = 255;
    } else {
        for k in 0..3 {
            palette[2][k] = mix(p0[k], p1[k], 1, 1, 2);
        }
        palette[2][3] = 255;
        palette[3] = [0, 0, 0, 0];
    }
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    for (i, px) in out.iter_mut().enumerate() {
        *px = palette[((bits >> (2 * i)) & 3) as usize];
    }
}

/// A BC4 (or BC3 alpha) block: 16 single-channel values.
fn alpha_block(b: &[u8], out: &mut [u8; 16]) {
    let (a0, a1) = (u16::from(b[0]), u16::from(b[1]));
    let mut p = [0u8; 8];
    p[0] = a0 as u8;
    p[1] = a1 as u8;
    if a0 > a1 {
        for i in 1..7u16 {
            p[i as usize + 1] = (((7 - i) * a0 + i * a1) / 7) as u8;
        }
    } else {
        for i in 1..5u16 {
            p[i as usize + 1] = (((5 - i) * a0 + i * a1) / 5) as u8;
        }
        p[6] = 0;
        p[7] = 255;
    }
    let bits = b[2..8]
        .iter()
        .rev()
        .fold(0u64, |acc, &x| (acc << 8) | u64::from(x));
    for (i, v) in out.iter_mut().enumerate() {
        *v = p[((bits >> (3 * i)) & 7) as usize];
    }
}

/// HAP Q: scaled YCoCg (Co, Cg, scale, Y in R, G, B, A) to RGB.
pub fn ycocg_to_rgb(px: [u8; 4]) -> [u8; 4] {
    let co = f32::from(px[0]) / 255.0 - 0.501_960_8;
    let cg = f32::from(px[1]) / 255.0 - 0.501_960_8;
    let scale = (f32::from(px[2]) / 255.0) * (255.0 / 8.0) + 1.0;
    let (co, cg) = (co / scale, cg / scale);
    let y = f32::from(px[3]) / 255.0;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(y + co - cg), q(y + cg), q(y - co - cg), 255]
}

/// Decodes a frame to `width`×`height` RGBA8 (rows top to bottom), or
/// `None` for formats without a CPU decoder (BC7).
pub fn to_rgba(frame: &HapFrame, width: u32, height: u32, out: &mut Vec<u8>) -> Option<()> {
    let (w, h) = (width as usize, height as usize);
    if frame.format == HapFormat::Rgba8 {
        out.clear();
        out.extend_from_slice(&frame.data);
        return Some(());
    }
    let bw = w.div_ceil(4);
    let bytes = frame.format.block_bytes();
    out.clear();
    out.resize(w * h * 4, 0);
    let mut px = [[0u8; 4]; 16];
    let mut alpha = [0u8; 16];
    for (i, block) in frame.data.chunks_exact(bytes).enumerate() {
        let (bx, by) = (i % bw, i / bw);
        match frame.format {
            HapFormat::Bc1 => colour_block(block, false, &mut px),
            HapFormat::Bc3 | HapFormat::YCoCgBc3 => {
                alpha_block(&block[..8], &mut alpha);
                colour_block(&block[8..], true, &mut px);
                for (p, a) in px.iter_mut().zip(alpha) {
                    p[3] = a;
                }
            }
            HapFormat::Bc4 => {
                alpha_block(block, &mut alpha);
                for (p, a) in px.iter_mut().zip(alpha) {
                    *p = [a, a, a, 255];
                }
            }
            HapFormat::Bc7 => return None,
            HapFormat::Rgba8 => unreachable!("handled above"),
        }
        for (j, p) in px.iter().enumerate() {
            let (x, y) = (bx * 4 + j % 4, by * 4 + j / 4);
            if x < w && y < h {
                let p = if frame.format == HapFormat::YCoCgBc3 {
                    ycocg_to_rgb(*p)
                } else {
                    *p
                };
                out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&p);
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_four_and_three_colour_modes() {
        let mut px = [[0u8; 4]; 16];
        // c0 = white > c1 = black: four colours; indices 0,1,2,3 in a row.
        colour_block(&[0xFF, 0xFF, 0, 0, 0b1110_0100, 0, 0, 0], false, &mut px);
        assert_eq!(
            &px[..4],
            &[
                [255; 4],
                [0, 0, 0, 255],
                [170, 170, 170, 255],
                [85, 85, 85, 255]
            ]
        );
        // c0 <= c1: three colours plus transparent black.
        colour_block(&[0, 0, 0xFF, 0xFF, 0b1110_0100, 0, 0, 0], false, &mut px);
        assert_eq!(px[2], [127, 127, 127, 255]);
        assert_eq!(px[3], [0, 0, 0, 0]);
    }

    #[test]
    fn bc4_eight_and_six_value_modes() {
        let mut a = [0u8; 16];
        // a0 = 255 > a1 = 0; index 1 then index 2 (= (6*255)/7).
        alpha_block(&[255, 0, 0b0001_0001, 0, 0, 0, 0, 0], &mut a);
        assert_eq!((a[0], a[1]), (0, 218));
        alpha_block(&[0, 255, 0b0011_1110, 0, 0, 0, 0, 0], &mut a);
        assert_eq!((a[0], a[1]), (0, 255));
    }

    #[test]
    fn ycocg_grey_is_grey() {
        // Co = Cg = 128 (zero), scale 0, Y = 200 → grey 200.
        let rgb = ycocg_to_rgb([128, 128, 0, 200]);
        assert!(rgb[..3].iter().all(|v| v.abs_diff(200) <= 1), "{rgb:?}");
    }
}
