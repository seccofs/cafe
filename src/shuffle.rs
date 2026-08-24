//! Byte-Shuffle (Filter Method = 1)
//!
//! Reorders bytes in multi-byte samples for better compression of float/HDR
//! data. Especially effective for floating-point data where contiguous
//! bytes have high correlation.
//!
//! Example: [b0_p0, b1_p0, b0_p1, b1_p1, ...]
//!       →  [b0_p0, b0_p1, b0_p2, ..., b1_p0, b1_p1, ...]
//!
//! # SIMD Optimization (v1.2.1+)
//! On x86_64, byte-shuffle automatically dispatches to a vectorized
//! implementation in `simd_shuffle.rs` at **runtime** when the running CPU
//! supports AVX2 (`is_x86_feature_detected!("avx2")`), for 2-3x speedup.
//! Falls back gracefully to scalar+blocking on CPUs without AVX2 and on
//! non-x86_64 architectures. No special build flags are required.

use crate::error::{CafeError, Result};

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
use crate::simd_shuffle;

/// Applies byte-shuffle for better compression of multi-byte samples.
///
/// Dispatches to the SIMD implementation on AVX2-capable x86_64 CPUs
/// (detected at runtime) for 2-3x speedup. Falls back to scalar+blocking
/// otherwise.
///
/// # Parameters
/// - `raw`: original pixel data in natural layout (R, G, B, A alternating bytes)
/// - `bpp`: bytes per pixel — must be 2, 4, 8 or 16
/// - `width`: width in pixels
/// - `height`: height in pixels
///
/// # Safety
/// - Validates `bpp ∈ {2, 4, 8, 16}` (rejects 1, 3, 5, 6, 7, etc.)
/// - Overflow-protected: `width × height × bpp`
/// - Bounds-checked on copy
pub(crate) fn apply_byte_shuffle(
    raw: &[u8],
    bpp: usize,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    // Validation: valid bpp (2, 4, 8 or 16 bytes/pixel)
    if bpp != 2 && bpp != 4 && bpp != 8 && bpp != 16 {
        return Err(CafeError::UnsupportedFeature(format!(
            "Byte-shuffle requires bpp ∈ {{2,4,8,16}}, got {}",
            bpp
        )));
    }

    // Overflow check: width × height
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width × height".into()))?
        as usize;

    // Overflow check: pixels × bpp
    let total_bytes = pixels
        .checked_mul(bpp)
        .ok_or_else(|| CafeError::TruncatedFile("Overflow on pixels × bpp".into()))?;

    // Validate buffer size
    if raw.len() != total_bytes {
        return Err(CafeError::TruncatedFile(format!(
            "Byte-shuffle: expected {} bytes, got {}",
            total_bytes,
            raw.len()
        )));
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return simd_shuffle::apply_byte_shuffle_simd(raw, bpp, width, height);
        }
    }

    apply_byte_shuffle_scalar(raw, bpp, pixels)
}

/// Scalar byte-shuffle implementation (fallback or when SIMD not available).
#[inline]
fn apply_byte_shuffle_scalar(raw: &[u8], bpp: usize, pixels: usize) -> Result<Vec<u8>> {
    let total_bytes = pixels * bpp;
    let mut shuffled = vec![0u8; total_bytes];

    // Reorder: groups bytes by position
    // [b0_p0, b1_p0, b0_p1, b1_p1, ...] → [b0_p0, b0_p1, ..., b1_p0, b1_p1, ...]
    for byte_pos in 0..bpp {
        for (pixel_idx, write_offset) in (0..pixels).enumerate() {
            let read_idx = pixel_idx * bpp + byte_pos;
            let write_idx = byte_pos * pixels + write_offset;
            shuffled[write_idx] = raw[read_idx];
        }
    }
    Ok(shuffled)
}

/// Undoes byte-shuffle: converts the reordered layout back to the original.
///
/// Dispatches to the SIMD implementation on AVX2-capable x86_64 CPUs
/// (detected at runtime) for 2-3x speedup. Falls back to scalar otherwise.
///
/// # Parameters
/// - `shuffled`: reordered data (output of `apply_byte_shuffle`)
/// - `bpp`: bytes per pixel (must match the encode)
/// - `width`, `height`: image dimensions
///
/// # Safety
/// - Validations identical to `apply_byte_shuffle`
/// - Returns an error if the buffer is truncated or bpp is invalid
pub(crate) fn undo_byte_shuffle(
    shuffled: &[u8],
    bpp: usize,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    // Validations identical to apply_byte_shuffle
    if bpp != 2 && bpp != 4 && bpp != 8 && bpp != 16 {
        return Err(CafeError::UnsupportedFeature(format!(
            "Byte-shuffle requires bpp ∈ {{2,4,8,16}}, got {}",
            bpp
        )));
    }

    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width × height".into()))?
        as usize;

    let total_bytes = pixels
        .checked_mul(bpp)
        .ok_or_else(|| CafeError::TruncatedFile("Overflow on pixels × bpp".into()))?;

    if shuffled.len() != total_bytes {
        return Err(CafeError::TruncatedFile(format!(
            "Byte-unshuffle: expected {} bytes, got {}",
            total_bytes,
            shuffled.len()
        )));
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return simd_shuffle::undo_byte_shuffle_simd(shuffled, bpp, width, height);
        }
    }

    undo_byte_shuffle_scalar(shuffled, bpp, pixels)
}

/// Scalar byte-unshuffle implementation (fallback or when SIMD not available).
#[inline]
fn undo_byte_shuffle_scalar(shuffled: &[u8], bpp: usize, pixels: usize) -> Result<Vec<u8>> {
    let total_bytes = pixels * bpp;
    let mut unshuffled = vec![0u8; total_bytes];

    // Invert: [b0, b0, ..., b1, b1, ...] → [b0, b1, b0, b1, ...]
    for byte_pos in 0..bpp {
        let read_offset = byte_pos * pixels;
        for pixel_idx in 0..pixels {
            let write_idx = pixel_idx * bpp + byte_pos;
            unshuffled[write_idx] = shuffled[read_offset + pixel_idx];
        }
    }
    Ok(unshuffled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shuffle_apply_undo_roundtrip_2byte() {
        let original = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
        let shuffled = apply_byte_shuffle(&original, 2, 3, 1).unwrap();
        assert_eq!(shuffled, vec![0x12, 0x56, 0x9A, 0x34, 0x78, 0xBC]);

        let unshuffled = undo_byte_shuffle(&shuffled, 2, 3, 1).unwrap();
        assert_eq!(unshuffled, original);
    }

    #[test]
    fn test_shuffle_invalid_bpp_rejected() {
        let data = vec![0u8; 8];
        assert!(apply_byte_shuffle(&data, 1, 4, 1).is_err());
        assert!(apply_byte_shuffle(&data, 3, 4, 1).is_err());
        assert!(apply_byte_shuffle(&data, 5, 4, 1).is_err());
    }

    #[test]
    fn test_shuffle_overflow_protection() {
        let data = vec![0u8; 100];
        assert!(apply_byte_shuffle(&data, 2, u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn test_shuffle_truncated_buffer() {
        let data = vec![0u8; 10];
        assert!(undo_byte_shuffle(&data, 2, 10, 1).is_err());
    }

    #[test]
    fn test_shuffle_4byte_pixel() {
        let original = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let shuffled = apply_byte_shuffle(&original, 4, 2, 1).unwrap();
        assert_eq!(
            shuffled,
            vec![0x11, 0x55, 0x22, 0x66, 0x33, 0x77, 0x44, 0x88]
        );

        let unshuffled = undo_byte_shuffle(&shuffled, 4, 2, 1).unwrap();
        assert_eq!(unshuffled, original);
    }

    #[test]
    fn test_shuffle_dispatcher_2byte_roundtrip() {
        // Test dispatcher (routes to SIMD on AVX2, scalar otherwise)
        let original: Vec<u8> = (0..64).map(|i| (i % 256) as u8).collect();
        let shuffled = apply_byte_shuffle(&original, 2, 32, 1).unwrap();
        let unshuffled = undo_byte_shuffle(&shuffled, 2, 32, 1).unwrap();
        assert_eq!(original, unshuffled, "2-byte dispatcher roundtrip failed");
    }

    #[test]
    fn test_shuffle_dispatcher_4byte_roundtrip() {
        // Test dispatcher (routes to SIMD on AVX2, scalar otherwise)
        let original: Vec<u8> = (0..128).map(|i| (i % 256) as u8).collect();
        let shuffled = apply_byte_shuffle(&original, 4, 32, 1).unwrap();
        let unshuffled = undo_byte_shuffle(&shuffled, 4, 32, 1).unwrap();
        assert_eq!(original, unshuffled, "4-byte dispatcher roundtrip failed");
    }

    #[test]
    fn test_shuffle_dispatcher_8byte_roundtrip() {
        // Test dispatcher (routes to SIMD on AVX2, scalar otherwise)
        let original: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let shuffled = apply_byte_shuffle(&original, 8, 32, 1).unwrap();
        let unshuffled = undo_byte_shuffle(&shuffled, 8, 32, 1).unwrap();
        assert_eq!(original, unshuffled, "8-byte dispatcher roundtrip failed");
    }

    #[test]
    fn test_shuffle_dispatcher_large_dataset() {
        // Test dispatcher with larger dataset to exercise multiple iterations
        let width = 1024u32;
        let height = 512u32;
        let total_pixels = (width as usize) * (height as usize);
        let original: Vec<u8> = (0..(total_pixels * 4))
            .map(|i| ((i as u64 * 13) % 256) as u8)
            .collect();

        let shuffled = apply_byte_shuffle(&original, 4, width, height).unwrap();
        let unshuffled = undo_byte_shuffle(&shuffled, 4, width, height).unwrap();
        assert_eq!(
            original, unshuffled,
            "Large 4-byte dispatcher roundtrip failed"
        );
    }

    #[test]
    fn test_shuffle_scalar_matches_dispatcher() {
        // Verify the scalar path always agrees with whatever the dispatcher
        // picks (SIMD or scalar), for every supported bpp.
        let width = 128u32;
        let height = 64u32;
        let total_pixels = (width as usize) * (height as usize);

        for bpp in [2usize, 4, 8, 16] {
            let original: Vec<u8> = (0..(total_pixels * bpp))
                .map(|i| ((i as u64 * 17) % 256) as u8)
                .collect();

            let dispatched = apply_byte_shuffle(&original, bpp, width, height).unwrap();
            let scalar = apply_byte_shuffle_scalar(&original, bpp, total_pixels).unwrap();
            assert_eq!(
                dispatched, scalar,
                "SIMD and scalar byte-shuffle diverge for bpp={bpp}"
            );

            let unshuffled = undo_byte_shuffle(&dispatched, bpp, width, height).unwrap();
            assert_eq!(original, unshuffled, "roundtrip failed for bpp={bpp}");
        }
    }
}
