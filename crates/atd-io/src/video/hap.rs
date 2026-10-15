//! HAP frame decoding (the open Vidvox codec): section headers, snappy and
//! chunked ("complex") compression, down to block-compressed texture data
//! ready for the GPU (BC1/BC3/BC4/BC7). No pixel work happens on the CPU
//! unless the device lacks BC textures ([`super::bc`]).

/// Texture format of a HAP frame's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapFormat {
    /// HAP: RGB in BC1 (DXT1).
    Bc1,
    /// HAP Alpha: RGBA in BC3 (DXT5).
    Bc3,
    /// HAP Q: scaled YCoCg in BC3; convert to RGB when sampling.
    YCoCgBc3,
    /// HAP R: RGBA in BC7.
    Bc7,
    /// HAP Alpha-only: one channel in BC4.
    Bc4,
    /// Uncompressed straight RGBA8 (still images).
    Rgba8,
}

impl HapFormat {
    /// Bytes per 4×4 block.
    pub fn block_bytes(self) -> usize {
        match self {
            Self::Bc1 | Self::Bc4 => 8,
            Self::Bc3 | Self::YCoCgBc3 | Self::Bc7 => 16,
            Self::Rgba8 => 64,
        }
    }

    fn from_type(t: u8) -> Option<Self> {
        Some(match t & 0x0F {
            0xB => Self::Bc1,
            0xE => Self::Bc3,
            0xF => Self::YCoCgBc3,
            0xC => Self::Bc7,
            0x1 => Self::Bc4,
            _ => return None,
        })
    }
}

/// A decoded frame: block-compressed texture data for a texture whose size
/// is the movie size rounded up to whole 4×4 blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapFrame {
    pub format: HapFormat,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HapError {
    #[error("truncated HAP frame")]
    Truncated,
    #[error("unsupported HAP section type 0x{0:02x}")]
    Unsupported(u8),
    #[error("snappy: {0}")]
    Snappy(String),
    #[error("frame holds {got} bytes of texture data, {want} expected for {w}×{h}")]
    Size {
        got: usize,
        want: usize,
        w: u32,
        h: u32,
    },
}

/// Reads one section header: `(type, body, rest)`.
fn section(data: &[u8]) -> Result<(u8, &[u8], &[u8]), HapError> {
    let head = data.get(..4).ok_or(HapError::Truncated)?;
    let mut size = u32::from_le_bytes([head[0], head[1], head[2], 0]) as usize;
    let kind = head[3];
    let mut at = 4;
    if size == 0 {
        let s = data.get(4..8).ok_or(HapError::Truncated)?;
        size = u32::from_le_bytes(s.try_into().expect("4 bytes")) as usize;
        at = 8;
    }
    let body = data.get(at..at + size).ok_or(HapError::Truncated)?;
    Ok((kind, body, &data[at + size..]))
}

fn snappy(src: &[u8], out: &mut Vec<u8>) -> Result<(), HapError> {
    let len = snap::raw::decompress_len(src).map_err(|e| HapError::Snappy(e.to_string()))?;
    let start = out.len();
    out.resize(start + len, 0);
    snap::raw::Decoder::new()
        .decompress(src, &mut out[start..])
        .map_err(|e| HapError::Snappy(e.to_string()))?;
    Ok(())
}

/// Decodes a HAP frame for a `width`×`height` movie. `out` is reused.
pub fn decode(frame: &[u8], width: u32, height: u32, out: &mut HapFrame) -> Result<(), HapError> {
    let (kind, body, _) = section(frame)?;
    if kind & 0x0F == 0x0D {
        // Multiple images (HAP Q Alpha): use the first (the colour image).
        return decode(body, width, height, out);
    }
    let format = HapFormat::from_type(kind).ok_or(HapError::Unsupported(kind))?;
    out.format = format;
    out.data.clear();
    match kind >> 4 {
        0xA => out.data.extend_from_slice(body),
        0xB => snappy(body, &mut out.data)?,
        0xC => decode_chunks(body, &mut out.data)?,
        _ => return Err(HapError::Unsupported(kind)),
    }
    let want = width.div_ceil(4) as usize * height.div_ceil(4) as usize * format.block_bytes();
    if out.data.len() != want {
        return Err(HapError::Size {
            got: out.data.len(),
            want,
            w: width,
            h: height,
        });
    }
    Ok(())
}

/// "Complex" compression: decode instructions, then chunks.
fn decode_chunks(body: &[u8], out: &mut Vec<u8>) -> Result<(), HapError> {
    let (kind, instructions, data) = section(body)?;
    if kind != 0x01 {
        return Err(HapError::Unsupported(kind));
    }
    let (mut compressors, mut sizes, mut offsets): (&[u8], Vec<usize>, Option<Vec<usize>>) =
        (&[], Vec::new(), None);
    let mut rest = instructions;
    while !rest.is_empty() {
        let (t, b, r) = section(rest)?;
        let u32s = || -> Vec<usize> {
            b.chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().expect("4 bytes")) as usize)
                .collect()
        };
        match t {
            0x02 => compressors = b,
            0x03 => sizes = u32s(),
            0x04 => offsets = Some(u32s()),
            _ => {}
        }
        rest = r;
    }
    if compressors.len() != sizes.len() {
        return Err(HapError::Truncated);
    }
    let mut at = 0usize;
    for (i, (&c, &size)) in compressors.iter().zip(&sizes).enumerate() {
        let start = offsets
            .as_ref()
            .map_or(at, |o| o.get(i).copied().unwrap_or(at));
        let chunk = data.get(start..start + size).ok_or(HapError::Truncated)?;
        match c {
            0x0A => out.extend_from_slice(chunk),
            0x0B => snappy(chunk, out)?,
            other => return Err(HapError::Unsupported(other)),
        }
        at = start + size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_headers_short_and_long() {
        let mut short = vec![3, 0, 0, 0xAB, 1, 2, 3, 9];
        let (t, b, r) = section(&short).unwrap();
        assert_eq!((t, b, r), (0xAB, &[1u8, 2, 3][..], &[9u8][..]));
        short.truncate(5);
        assert_eq!(section(&short), Err(HapError::Truncated));
        let long = [0, 0, 0, 0xBE, 2, 0, 0, 0, 7, 8];
        assert_eq!(section(&long).unwrap(), (0xBE, &[7u8, 8][..], &[][..]));
    }

    #[test]
    fn uncompressed_frame_size_is_checked() {
        let w = 8;
        let h = 4;
        let mut frame = vec![16, 0, 0, 0xAB];
        frame.extend([0u8; 16]);
        let mut out = HapFrame {
            format: HapFormat::Bc1,
            data: Vec::new(),
        };
        decode(&frame, w, h, &mut out).unwrap();
        assert_eq!(out.data.len(), 16);
        assert!(matches!(
            decode(&frame, 16, 4, &mut out),
            Err(HapError::Size { want: 32, .. })
        ));
        assert_eq!(
            decode(&[1, 0, 0, 0x55, 0], 4, 4, &mut out),
            Err(HapError::Unsupported(0x55))
        );
    }
}
