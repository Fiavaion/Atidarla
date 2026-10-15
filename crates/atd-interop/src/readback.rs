//! Texture → CPU copy: the portable fallback for sharing and for tests.

use crate::InteropError;

/// Bytes per row for an RGBA8-class texture, padded to wgpu's copy alignment.
pub fn padded_bytes_per_row(width: u32) -> u32 {
    (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Uploads tightly packed 4-byte pixels to the whole of `texture`.
pub fn write_rgba8(queue: &wgpu::Queue, texture: &wgpu::Texture, pixels: &[u8]) {
    let size = texture.size();
    assert_eq!(
        pixels.len(),
        (size.width * size.height * 4) as usize,
        "pixel buffer size"
    );
    queue.write_texture(
        texture.as_image_copy(),
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * 4),
            rows_per_image: None,
        },
        size,
    );
}

/// Copies a 4-byte-per-pixel texture to CPU memory, tightly packed. Blocks
/// until the GPU is done: not for the render thread.
pub fn read_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, InteropError> {
    let size = texture.size();
    let padded = padded_bytes_per_row(size.width);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atd-readback"),
        size: u64::from(padded) * u64::from(size.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("atd-readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|e| InteropError::Gpu(e.to_string()))?;
    rx.recv()
        .map_err(|e| InteropError::Gpu(e.to_string()))?
        .map_err(|e| InteropError::Gpu(e.to_string()))?;

    let mapped = slice
        .get_mapped_range()
        .map_err(|e| InteropError::Gpu(e.to_string()))?;
    let row = (size.width * 4) as usize;
    let mut out = Vec::with_capacity(row * size.height as usize);
    for chunk in mapped.chunks(padded as usize).take(size.height as usize) {
        out.extend_from_slice(&chunk[..row]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}

/// A deterministic test pattern (each pixel unique for small images).
pub fn test_pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
    (0..width * height)
        .flat_map(|i| {
            let (x, y) = ((i % width) as u8, (i / width) as u8);
            [
                x.wrapping_mul(7).wrapping_add(seed),
                y.wrapping_mul(13),
                x ^ y,
                255,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_padding() {
        assert_eq!(padded_bytes_per_row(1), 256);
        assert_eq!(padded_bytes_per_row(64), 256);
        assert_eq!(padded_bytes_per_row(65), 512);
    }

    #[test]
    fn pattern_size() {
        assert_eq!(test_pattern(3, 2, 0).len(), 24);
    }
}
