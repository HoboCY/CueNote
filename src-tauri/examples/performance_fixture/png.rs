//! Deterministic patterned PNGs padded to an exact byte size with one valid
//! ancillary chunk (`caPa`) in front of `IEND`.
//!
//! The method mirrors `src-tauri/tests/format_v1.rs::png_with_ancillary_capacity`
//! but keeps the pixels nontrivial and seeded from the asset id, and it discloses
//! the ancillary capacity in the corpus manifest. Padding lives inside the PNG
//! container, never as ZIP or file-level filler.

use std::io::Cursor;

use image::{DynamicImage, ImageBuffer, ImageFormat, Luma};
use sha2::{Digest, Sha256};

/// Seed derived from the asset id, so verifiers can recompute pixels without
/// trusting any manifest field.
pub fn pattern_seed(asset_id: &str) -> u64 {
    let digest = Sha256::digest(asset_id.as_bytes());
    u64::from_be_bytes(digest[..8].try_into().expect("sha256 has 8 leading bytes"))
}

/// Smooth 16-by-16 block pattern: nontrivial, deterministic and still very
/// compressible, which keeps the padded container as the only large part.
pub fn expected_pixel(side: u32, seed: u64, x: u32, y: u32) -> u8 {
    let seed = seed as u32;
    let block = (x >> 4)
        .wrapping_mul(3)
        .wrapping_add((y >> 4).wrapping_mul(5))
        .wrapping_add(seed & 0xffff)
        .wrapping_add((seed >> 16) & 0x0f)
        & 0x0f;
    let local = ((x.wrapping_add(seed)) & 0x0f) ^ ((y.wrapping_add(seed >> 8)) & 0x0f);
    debug_assert!(x < side && y < side);
    ((block << 4) | local) as u8
}

pub fn patterned_png(side: u32, seed: u64) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut pixels = Vec::with_capacity((side as usize) * (side as usize));
    for y in 0..side {
        for x in 0..side {
            pixels.push(expected_pixel(side, seed, x, y));
        }
    }
    let image = ImageBuffer::<Luma<u8>, Vec<u8>>::from_raw(side, side, pixels)
        .ok_or("invalid image buffer dimensions")?;
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image).write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

/// Inserts one zero-payload `caPa` chunk before `IEND` so the PNG is exactly
/// `target` bytes. Returns the padded bytes and the ancillary capacity added
/// (chunk framing included), which the caller discloses in the manifest.
pub fn pad_png_exact(
    base: Vec<u8>,
    target: usize,
) -> Result<(Vec<u8>, u64), Box<dyn std::error::Error>> {
    if base.len() + 12 > target {
        return Err(format!(
            "png base of {} bytes leaves no room for a {} byte target",
            base.len(),
            target
        )
        .into());
    }
    let iend = base
        .windows(4)
        .rposition(|window| window == b"IEND")
        .ok_or("encoded PNG contains IEND")?;
    let offset = iend - 4;
    let payload = target - base.len() - 12;
    let mut chunk = Vec::with_capacity(payload + 12);
    chunk.extend_from_slice(&(payload as u32).to_be_bytes());
    chunk.extend_from_slice(b"caPa");
    chunk.resize(8 + payload, 0);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());

    let mut padded = Vec::with_capacity(target);
    padded.extend_from_slice(&base[..offset]);
    padded.extend_from_slice(&chunk);
    padded.extend_from_slice(&base[offset..]);
    if padded.len() != target {
        return Err("padded PNG does not match its target size".into());
    }
    Ok((padded, (target - base.len()) as u64))
}

/// Walks the PNG chunk stream. Returns `(ancillary_bytes, caPa_count)` where
/// ancillary bytes sum every non-critical chunk including its framing.
pub fn ancillary_profile(png: &[u8]) -> Result<(u64, usize), String> {
    if png.len() < 8 || png[..8] != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        return Err("PNG signature is missing".to_owned());
    }
    let mut offset = 8usize;
    let mut ancillary = 0u64;
    let mut capa = 0usize;
    loop {
        if offset + 8 > png.len() {
            return Err("PNG chunk stream is truncated".to_owned());
        }
        let length = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = &png[offset + 4..offset + 8];
        let total = length
            .checked_add(12)
            .ok_or_else(|| "PNG chunk length overflows".to_owned())?;
        if offset + total > png.len() {
            return Err("PNG chunk exceeds the file".to_owned());
        }
        if kind[0].is_ascii_lowercase() {
            ancillary += total as u64;
        }
        if kind == b"caPa" {
            capa += 1;
        }
        if kind == b"IEND" {
            if offset + total != png.len() {
                return Err("PNG has trailing data after IEND".to_owned());
            }
            break;
        }
        offset += total;
    }
    Ok((ancillary, capa))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// CRC-32 (IEEE) over the chunk type and payload, as the PNG spec requires.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_hits_the_exact_target_and_reports_capacity() {
        let base = patterned_png(64, 7).expect("encode");
        let target = base.len() + 12 + 1000;
        let (padded, ancillary) = pad_png_exact(base.clone(), target).expect("pad");
        assert_eq!(padded.len(), target);
        assert_eq!(ancillary, (target - base.len()) as u64);
        let (reported, capa) = ancillary_profile(&padded).expect("profile");
        assert_eq!(reported, ancillary);
        assert_eq!(capa, 1);
    }

    #[test]
    fn pattern_is_not_constant_and_is_recomputable() {
        let seed = pattern_seed("aaaaaaa-0000");
        let values: std::collections::BTreeSet<u8> = (0..32)
            .map(|index| expected_pixel(32, seed, index, index))
            .collect();
        assert!(values.len() > 4, "pattern must be nontrivial");
    }
}
