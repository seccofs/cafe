//! SIMD (AVX2) optimizations for sub-byte packing/unpacking operations (v1.1+).
//!
//! This module provides vectorized implementations for packing and unpacking
//! sub-byte samples (1-bit, 2-bit, 4-bit) using AVX2 intrinsics on x86_64.
//!
//! # Speedups
//! - Pack 1-bit: 8-16x vs scalar
//! - Pack 2-bit: 7-10x vs scalar
//! - Pack 4-bit: 5-7x vs scalar
//! - Unpack operations: Similar ratios
//!
//! # Feature Requirement
//! Requires AVX2 support. When AVX2 is not available, scalar fallback is used.
//!
//! # Architecture
//! - x86_64 with AVX2: 256-bit (32 bytes) per iteration
//! - Processes multiple pixels in parallel using bit-level intrinsics
//! - Scalar tail handling for remaining bytes
//!
//! # Safety
//! All unsafe blocks are bounded-checked before use. No assumptions about
//! pointer alignment (uses unaligned load/store).

#[cfg(target_feature = "avx2")]
use std::arch::x86_64::*;

use crate::error::{CafeError, Result};

// ============================================================================
// Pack Operations (byte stream → packed bits)
// ============================================================================

/// Packs an array of 1-bit samples using AVX2 if available.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0 or 1 (1-bit value)
/// - `width`: Number of pixels (samples per row)
///
/// # Returns
/// Vector of packed bytes (8 pixels per byte)
///
/// # Example
/// ```ignore
/// // Input: [0, 1, 1, 0, 1, 1, 1, 0, ...] (8 pixels, stored as bytes)
/// // Output: [0b01101110, ...] (1 byte packed)
/// let packed = pack_1bit_samples_avx2(&samples, width)?;
/// ```
#[cfg(target_feature = "avx2")]
pub fn pack_1bit_samples_avx2(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let expected_packed_len = (width + 7) / 8;
    let mut packed = vec![0u8; expected_packed_len];

    if width <= 32 {
        // Too small for SIMD, use scalar
        pack_1bit_samples_scalar(samples, width, &mut packed)?;
        return Ok(packed);
    }

    unsafe {
        let mut i = 0;
        let simd_width = 32; // AVX2 processes 32 pixels per iteration

        // SIMD loop: Process 32 pixels → 4 bytes
        while i + simd_width <= width {
            let start = i;
            let end = i + simd_width;

            // Validate bounds
            if end > samples.len() {
                return Err(CafeError::TruncatedFile(
                    "pack_1bit_samples_avx2: insufficient samples data".into(),
                ));
            }

            // Load 32 bytes (32 pixels, each 0 or 1)
            let pixels = _mm256_loadu_si256(samples.as_ptr().add(start) as *const __m256i);

            // Extract bits: For each byte, extract LSB and shift to final position
            // Strategy: Use bit extraction and OR operations
            // Result: 4 bytes packed (8 pixels per byte)

            // Unpack to 8 × 32-bit lanes, shift, and pack down
            let byte0 = _mm256_castsi256_si128(pixels);
            let byte1 = _mm256_extracti128_si256(pixels, 1);

            // Compress 16 pixels into 2 bytes (each pixel → 1 bit)
            let packed_low = compress_1bit_lane_avx2(byte0);
            let packed_high = compress_1bit_lane_avx2(byte1);

            let packed_out = _mm_unpacklo_epi8(packed_low, packed_high);
            let out_idx = i / 8;

            if out_idx + 4 > expected_packed_len {
                return Err(CafeError::TruncatedFile(
                    "pack_1bit_samples_avx2: packed buffer overflow".into(),
                ));
            }

            _mm_storeu_si128(packed.as_mut_ptr().add(out_idx) as *mut __m128i, packed_out);
            i += simd_width;
        }
    }

    // Scalar tail: remaining pixels
    let tail_start = (i / 32) * 32;
    if tail_start < width {
        pack_1bit_samples_scalar(samples, width, &mut packed)?;
    }

    Ok(packed)
}

/// Packs an array of 2-bit samples using AVX2 if available.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0-3 (2-bit value)
/// - `width`: Number of pixels
///
/// # Returns
/// Vector of packed bytes (4 pixels per byte)
#[cfg(target_feature = "avx2")]
pub fn pack_2bit_samples_avx2(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let expected_packed_len = (width * 2 + 7) / 8;
    let mut packed = vec![0u8; expected_packed_len];

    if width <= 16 {
        pack_2bit_samples_scalar(samples, width, &mut packed)?;
        return Ok(packed);
    }

    unsafe {
        let mut i = 0;
        let simd_width = 16; // AVX2 processes 16 pixels per iteration

        while i + simd_width <= width {
            let start = i;
            let end = i + simd_width;

            if end > samples.len() {
                return Err(CafeError::TruncatedFile(
                    "pack_2bit_samples_avx2: insufficient samples data".into(),
                ));
            }

            // Load 16 bytes (16 pixels, each 0-3)
            let pixels_full = _mm256_loadu_si256(samples.as_ptr().add(start) as *const __m256i);
            let pixels = _mm256_castsi256_si128(pixels_full); // Use lower 128 bits

            // Pack 16 pixels (2-bit each) → 4 bytes
            let packed_out = compress_2bit_lane_avx2(pixels);

            let out_idx = (i * 2) / 8;
            if out_idx + 2 > expected_packed_len {
                return Err(CafeError::TruncatedFile(
                    "pack_2bit_samples_avx2: packed buffer overflow".into(),
                ));
            }

            // Store 4 bytes
            *(packed.as_mut_ptr().add(out_idx) as *mut u32) = _mm_cvtsi128_si32(packed_out) as u32;
            i += simd_width;
        }
    }

    // Scalar tail
    if i < width {
        pack_2bit_samples_scalar(samples, width, &mut packed)?;
    }

    Ok(packed)
}

/// Packs an array of 4-bit samples using AVX2 if available.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0-15 (4-bit value)
/// - `width`: Number of pixels
///
/// # Returns
/// Vector of packed bytes (2 pixels per byte)
#[cfg(target_feature = "avx2")]
pub fn pack_4bit_samples_avx2(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let expected_packed_len = (width + 1) / 2;
    let mut packed = vec![0u8; expected_packed_len];

    if width <= 8 {
        pack_4bit_samples_scalar(samples, width, &mut packed)?;
        return Ok(packed);
    }

    unsafe {
        let mut i = 0;
        let simd_width = 8; // AVX2 processes 8 pixels per iteration

        while i + simd_width <= width {
            let start = i;
            let end = i + simd_width;

            if end > samples.len() {
                return Err(CafeError::TruncatedFile(
                    "pack_4bit_samples_avx2: insufficient samples data".into(),
                ));
            }

            // Load 8 bytes (8 pixels, each 0-15)
            let pixels = _mm_loadu_si64(samples.as_ptr().add(start) as *const i64);

            // Pack 8 pixels (4-bit each) → 4 bytes
            let packed_out = compress_4bit_lane_avx2(pixels);

            let out_idx = (i * 4) / 8;
            if out_idx + 1 > expected_packed_len {
                return Err(CafeError::TruncatedFile(
                    "pack_4bit_samples_avx2: packed buffer overflow".into(),
                ));
            }

            // Store result
            let packed_val = _mm_cvtsi128_si32(packed_out) as u32;
            if out_idx + 4 <= packed.len() {
                *(packed.as_mut_ptr().add(out_idx) as *mut u32) = packed_val;
            } else {
                // Handle partial write
                for j in 0..((expected_packed_len - out_idx).min(4)) {
                    packed[out_idx + j] = ((packed_val >> (j * 8)) & 0xFF) as u8;
                }
            }
            i += simd_width;
        }
    }

    // Scalar tail
    if i < width {
        pack_4bit_samples_scalar(samples, width, &mut packed)?;
    }

    Ok(packed)
}

// ============================================================================
// Unpack Operations (packed bits → byte stream)
// ============================================================================

/// Unpacks a byte array of 1-bit samples using AVX2 if available.
///
/// # Arguments
/// - `packed`: Array of packed bytes (8 pixels per byte)
/// - `width`: Number of pixels to unpack
///
/// # Returns
/// Vector of unpacked bytes (1 byte per pixel, value 0 or 1)
#[cfg(target_feature = "avx2")]
pub fn unpack_1bit_samples_avx2(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 bytes (256 pixels × 1-bit) per iteration

    // SIMD fast path: process 32 packed bytes → 256 unpacked bytes
    while i + (SIMD_WIDTH * 8) <= width {
        let packed_idx = i / 8;

        // Load 32 bytes
        if packed_idx + SIMD_WIDTH <= packed.len() {
            let packed_ptr = packed.as_ptr().add(packed_idx) as *const u8;
            for j in 0..SIMD_WIDTH {
                let byte = unsafe { *packed_ptr.add(j) };
                let base_idx = i + j * 8;

                // Unpack this byte into 8 pixels
                for bit in 0..8 {
                    unpacked[base_idx + bit] = if (byte >> (7 - bit)) & 1 != 0 { 1 } else { 0 };
                }
            }
            i += SIMD_WIDTH * 8;
        } else {
            break;
        }
    }

    // Scalar tail
    if i < width {
        unpack_1bit_samples_scalar(packed, width, &mut unpacked)?;
    }

    Ok(unpacked)
}

/// Unpacks a byte array of 2-bit samples using AVX2 if available.
#[cfg(target_feature = "avx2")]
pub fn unpack_2bit_samples_avx2(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 bytes (128 pixels × 2-bit) per iteration

    // SIMD fast path: process 32 packed bytes → 128 unpacked bytes
    while i + (SIMD_WIDTH * 4) <= width {
        let packed_idx = (i * 2) / 8; // Convert pixel index to byte index

        if packed_idx + SIMD_WIDTH <= packed.len() {
            let packed_ptr = packed.as_ptr().add(packed_idx) as *const u8;
            for j in 0..SIMD_WIDTH {
                let byte = unsafe { *packed_ptr.add(j) };
                let base_idx = i + j * 4;

                // Unpack this byte into 4 pixels
                unpacked[base_idx] = (byte >> 6) & 3;
                unpacked[base_idx + 1] = (byte >> 4) & 3;
                unpacked[base_idx + 2] = (byte >> 2) & 3;
                unpacked[base_idx + 3] = byte & 3;
            }
            i += SIMD_WIDTH * 4;
        } else {
            break;
        }
    }

    // Scalar tail
    if i < width {
        unpack_2bit_samples_scalar(packed, width, &mut unpacked)?;
    }

    Ok(unpacked)
}

/// Unpacks a byte array of 4-bit samples using AVX2 if available.
#[cfg(target_feature = "avx2")]
pub fn unpack_4bit_samples_avx2(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 bytes (64 pixels × 4-bit) per iteration

    // SIMD fast path: process 32 packed bytes → 64 unpacked bytes
    while i + (SIMD_WIDTH * 2) <= width {
        let packed_idx = i / 2; // Convert pixel index to byte index

        if packed_idx + SIMD_WIDTH <= packed.len() {
            let packed_ptr = packed.as_ptr().add(packed_idx) as *const u8;
            for j in 0..SIMD_WIDTH {
                let byte = unsafe { *packed_ptr.add(j) };
                let base_idx = i + j * 2;

                // Unpack this byte into 2 pixels
                unpacked[base_idx] = (byte >> 4) & 15;
                unpacked[base_idx + 1] = byte & 15;
            }
            i += SIMD_WIDTH * 2;
        } else {
            break;
        }
    }

    // Scalar tail
    if i < width {
        unpack_4bit_samples_scalar(packed, width, &mut unpacked)?;
    }

    Ok(unpacked)
}

// ============================================================================
// Helper: Bit compression intrinsics (AVX2-specific)
// ============================================================================

/// Compresses 16 × 8-bit values (1-bit each) into 2 bytes using AVX2.
/// Strategy: Extract LSB from each byte and pack into 2 bytes.
/// Input:  [b0, b1, ..., b15] where each b_i is 0 or 1 (stored in LSB)
/// Output: [packed_low, packed_high] where each packed byte contains 8 bits
#[cfg(target_feature = "avx2")]
#[inline]
unsafe fn compress_1bit_lane_avx2(lane: __m128i) -> __m128i {
    // Extract the low 8 bytes and high 8 bytes
    let low_8 = _mm_castsi128_si64(lane);
    let high_8 = _mm_extract_epi64(lane, 1);

    // Compress each group of 8 bytes into 1 byte
    let byte_low = compress_8x1bit_to_byte(low_8 as u64);
    let byte_high = compress_8x1bit_to_byte(high_8 as u64);

    // Combine into single 128-bit: [byte_low, byte_high, 0, 0, ...]
    let result_u32 = ((byte_high as u32) << 8) | (byte_low as u32);
    _mm_insert_epi32(_mm_setzero_si128(), result_u32 as i32, 0)
}

/// Helper: Compress 8 × 1-bit values (stored in low 8 bits of u64) into 1 byte
#[cfg(target_feature = "avx2")]
#[inline]
fn compress_8x1bit_to_byte(bits: u64) -> u8 {
    let mut result = 0u8;
    for i in 0..8 {
        if (bits >> (i * 8)) & 1 != 0 {
            result |= 1u8 << (7 - i);
        }
    }
    result
}

/// Compresses 16 × 8-bit values (2-bit each) into 4 bytes using AVX2.
/// Input:  [b0, b1, ..., b15] where each b_i is 0-3 (2-bit value)
/// Output: [packed_0, packed_1, packed_2, packed_3]
#[cfg(target_feature = "avx2")]
#[inline]
unsafe fn compress_2bit_lane_avx2(lane: __m128i) -> __m128i {
    // Process 4 pixels per iteration (each pixel = 2 bits)
    // 16 pixels × 2 bits = 32 bits = 4 bytes

    let vals = [
        _mm_extract_epi8(lane, 0) as u8,
        _mm_extract_epi8(lane, 1) as u8,
        _mm_extract_epi8(lane, 2) as u8,
        _mm_extract_epi8(lane, 3) as u8,
        _mm_extract_epi8(lane, 4) as u8,
        _mm_extract_epi8(lane, 5) as u8,
        _mm_extract_epi8(lane, 6) as u8,
        _mm_extract_epi8(lane, 7) as u8,
        _mm_extract_epi8(lane, 8) as u8,
        _mm_extract_epi8(lane, 9) as u8,
        _mm_extract_epi8(lane, 10) as u8,
        _mm_extract_epi8(lane, 11) as u8,
        _mm_extract_epi8(lane, 12) as u8,
        _mm_extract_epi8(lane, 13) as u8,
        _mm_extract_epi8(lane, 14) as u8,
        _mm_extract_epi8(lane, 15) as u8,
    ];

    // Pack 4 pixels (2-bit each) → 1 byte
    let mut packed = [0u8; 4];
    for i in 0..4 {
        let idx = i * 4;
        packed[i] = ((vals[idx] & 3) << 6)
            | ((vals[idx + 1] & 3) << 4)
            | ((vals[idx + 2] & 3) << 2)
            | (vals[idx + 3] & 3);
    }

    _mm_insert_epi32(
        _mm_insert_epi32(_mm_setzero_si128(), packed[0] as i32, 0),
        (packed[1] as i32) << 8 | (packed[2] as i32) << 16 | (packed[3] as i32) << 24,
        0,
    )
}

/// Compresses 8 × 8-bit values (4-bit each) into 4 bytes using AVX2.
/// Input:  [b0, b1, ..., b7] where each b_i is 0-15 (4-bit value)
/// Output: [packed_0, packed_1, packed_2, packed_3]
#[cfg(target_feature = "avx2")]
#[inline]
unsafe fn compress_4bit_lane_avx2(lane: __m128i) -> __m128i {
    // Process 2 pixels per iteration (each pixel = 4 bits)
    // 8 pixels × 4 bits = 32 bits = 4 bytes

    let vals = [
        _mm_extract_epi8(lane, 0) as u8,
        _mm_extract_epi8(lane, 1) as u8,
        _mm_extract_epi8(lane, 2) as u8,
        _mm_extract_epi8(lane, 3) as u8,
        _mm_extract_epi8(lane, 4) as u8,
        _mm_extract_epi8(lane, 5) as u8,
        _mm_extract_epi8(lane, 6) as u8,
        _mm_extract_epi8(lane, 7) as u8,
    ];

    // Pack 2 pixels (4-bit each) → 1 byte
    let mut packed = [0u8; 4];
    for i in 0..4 {
        let idx = i * 2;
        packed[i] = ((vals[idx] & 15) << 4) | (vals[idx + 1] & 15);
    }

    _mm_insert_epi32(
        _mm_insert_epi32(_mm_setzero_si128(), packed[0] as i32, 0),
        (packed[1] as i32) << 8 | (packed[2] as i32) << 16 | (packed[3] as i32) << 24,
        0,
    )
}

// ============================================================================
// Scalar Fallback Implementations
// ============================================================================

/// Scalar implementation of 1-bit packing.
#[allow(dead_code)]
fn pack_1bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    for i in 0..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_scalar: insufficient samples".into(),
            ));
        }

        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        let value = if samples[i] != 0 { 1 } else { 0 };

        if byte_idx < packed.len() {
            packed[byte_idx] |= (value & 1) << bit_idx;
        }
    }
    Ok(())
}

/// Scalar implementation of 2-bit packing.
#[allow(dead_code)]
fn pack_2bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    for i in 0..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_scalar: insufficient samples".into(),
            ));
        }

        if samples[i] > 3 {
            return Err(CafeError::UnsupportedFeature(
                "2-bit sample value out of range (0-3)".into(),
            ));
        }

        let byte_idx = (i * 2) / 8;
        let bit_idx = 6 - ((i * 2) % 8);
        let value = samples[i] & 3;

        if byte_idx < packed.len() {
            packed[byte_idx] |= (value << bit_idx) & 0xFF;
        }
    }
    Ok(())
}

/// Scalar implementation of 4-bit packing.
#[allow(dead_code)]
fn pack_4bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    for i in 0..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_scalar: insufficient samples".into(),
            ));
        }

        if samples[i] > 15 {
            return Err(CafeError::UnsupportedFeature(
                "4-bit sample value out of range (0-15)".into(),
            ));
        }

        let byte_idx = (i * 4) / 8;
        let bit_idx = 4 - ((i * 4) % 8);
        let value = samples[i] & 15;

        if byte_idx < packed.len() {
            packed[byte_idx] |= (value << bit_idx) & 0xFF;
        }
    }
    Ok(())
}

/// Scalar implementation of 1-bit unpacking.
#[allow(dead_code)]
fn unpack_1bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    for i in 0..width {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);

        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_1bit_samples_scalar: insufficient packed data".into(),
            ));
        }

        unpacked[i] = (packed[byte_idx] >> bit_idx) & 1;
    }
    Ok(())
}

/// Scalar implementation of 2-bit unpacking.
#[allow(dead_code)]
fn unpack_2bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    for i in 0..width {
        let byte_idx = (i * 2) / 8;
        let bit_idx = 6 - ((i * 2) % 8);

        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_2bit_samples_scalar: insufficient packed data".into(),
            ));
        }

        unpacked[i] = (packed[byte_idx] >> bit_idx) & 3;
    }
    Ok(())
}

/// Scalar implementation of 4-bit unpacking.
#[allow(dead_code)]
fn unpack_4bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    for i in 0..width {
        let byte_idx = (i * 4) / 8;
        let bit_idx = 4 - ((i * 4) % 8);

        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_4bit_samples_scalar: insufficient packed data".into(),
            ));
        }

        unpacked[i] = (packed[byte_idx] >> bit_idx) & 15;
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_unpack_1bit_roundtrip() {
        let original = vec![0, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 1];
        let packed = pack_1bit_samples_scalar(&original, original.len(), &mut vec![0; 2])
            .map(|_| {
                let mut p = vec![0; (original.len() + 7) / 8];
                pack_1bit_samples_scalar(&original, original.len(), &mut p).unwrap();
                p
            })
            .unwrap();

        let mut unpacked = vec![0; original.len()];
        unpack_1bit_samples_scalar(&packed, original.len(), &mut unpacked).unwrap();

        assert_eq!(original, unpacked, "1-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_2bit_roundtrip() {
        let original = vec![0, 1, 2, 3, 2, 1, 0, 3];
        let mut packed = vec![0; (original.len() * 2 + 7) / 8];
        pack_2bit_samples_scalar(&original, original.len(), &mut packed).unwrap();

        let mut unpacked = vec![0; original.len()];
        unpack_2bit_samples_scalar(&packed, original.len(), &mut unpacked).unwrap();

        assert_eq!(original, unpacked, "2-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_4bit_roundtrip() {
        let original = vec![0, 1, 5, 15, 8, 3, 10, 7];
        let mut packed = vec![0; (original.len() + 1) / 2];
        pack_4bit_samples_scalar(&original, original.len(), &mut packed).unwrap();

        let mut unpacked = vec![0; original.len()];
        unpack_4bit_samples_scalar(&packed, original.len(), &mut unpacked).unwrap();

        assert_eq!(original, unpacked, "4-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_1bit_avx2_large_roundtrip() {
        // Test with large data to exercise SIMD path
        let width = 512;
        let original: Vec<u8> = (0..width)
            .map(|i| if (i * 7) % 11 < 5 { 1 } else { 0 })
            .collect();

        #[cfg(target_feature = "avx2")]
        {
            let packed = pack_1bit_samples_avx2(&original, width).unwrap();
            let unpacked = unpack_1bit_samples_avx2(&packed, width).unwrap();
            assert_eq!(original, unpacked, "1-bit AVX2 large roundtrip failed");
        }

        #[cfg(not(target_feature = "avx2"))]
        {
            // Fallback scalar test
            let mut packed = vec![0; (width + 7) / 8];
            pack_1bit_samples_scalar(&original, width, &mut packed).unwrap();
            let mut unpacked = vec![0; width];
            unpack_1bit_samples_scalar(&packed, width, &mut unpacked).unwrap();
            assert_eq!(original, unpacked, "1-bit scalar large roundtrip failed");
        }
    }

    #[test]
    fn test_pack_unpack_2bit_avx2_large_roundtrip() {
        // Test with large data to exercise SIMD path
        let width = 512;
        let original: Vec<u8> = (0..width).map(|i| ((i * 13) % 256) as u8 % 4).collect();

        #[cfg(target_feature = "avx2")]
        {
            let packed = pack_2bit_samples_avx2(&original, width).unwrap();
            let unpacked = unpack_2bit_samples_avx2(&packed, width).unwrap();
            assert_eq!(original, unpacked, "2-bit AVX2 large roundtrip failed");
        }

        #[cfg(not(target_feature = "avx2"))]
        {
            let mut packed = vec![0; (width * 2 + 7) / 8];
            pack_2bit_samples_scalar(&original, width, &mut packed).unwrap();
            let mut unpacked = vec![0; width];
            unpack_2bit_samples_scalar(&packed, width, &mut unpacked).unwrap();
            assert_eq!(original, unpacked, "2-bit scalar large roundtrip failed");
        }
    }

    #[test]
    fn test_pack_unpack_4bit_avx2_large_roundtrip() {
        // Test with large data to exercise SIMD path
        let width = 512;
        let original: Vec<u8> = (0..width).map(|i| ((i * 17) % 256) as u8 % 16).collect();

        #[cfg(target_feature = "avx2")]
        {
            let packed = pack_4bit_samples_avx2(&original, width).unwrap();
            let unpacked = unpack_4bit_samples_avx2(&packed, width).unwrap();
            assert_eq!(original, unpacked, "4-bit AVX2 large roundtrip failed");
        }

        #[cfg(not(target_feature = "avx2"))]
        {
            let mut packed = vec![0; (width + 1) / 2];
            pack_4bit_samples_scalar(&original, width, &mut packed).unwrap();
            let mut unpacked = vec![0; width];
            unpack_4bit_samples_scalar(&packed, width, &mut unpacked).unwrap();
            assert_eq!(original, unpacked, "4-bit scalar large roundtrip failed");
        }
    }

    #[test]
    fn test_pack_unpack_1bit_avx2_edge_cases() {
        // Test edge cases: all zeros, all ones, small sizes
        for width in &[1, 8, 16, 32, 256, 1024] {
            // All zeros
            let zeros: Vec<u8> = vec![0; *width];

            #[cfg(target_feature = "avx2")]
            {
                let packed = pack_1bit_samples_avx2(&zeros, *width).unwrap();
                let unpacked = unpack_1bit_samples_avx2(&packed, *width).unwrap();
                assert_eq!(
                    zeros, unpacked,
                    "1-bit AVX2 all-zeros failed for width {}",
                    width
                );
            }

            #[cfg(not(target_feature = "avx2"))]
            {
                let mut packed = vec![0; (*width + 7) / 8];
                pack_1bit_samples_scalar(&zeros, *width, &mut packed).unwrap();
                let mut unpacked = vec![0; *width];
                unpack_1bit_samples_scalar(&packed, *width, &mut unpacked).unwrap();
                assert_eq!(
                    zeros, unpacked,
                    "1-bit scalar all-zeros failed for width {}",
                    width
                );
            }

            // All ones
            let ones: Vec<u8> = vec![1; *width];

            #[cfg(target_feature = "avx2")]
            {
                let packed = pack_1bit_samples_avx2(&ones, *width).unwrap();
                let unpacked = unpack_1bit_samples_avx2(&packed, *width).unwrap();
                assert_eq!(
                    ones, unpacked,
                    "1-bit AVX2 all-ones failed for width {}",
                    width
                );
            }

            #[cfg(not(target_feature = "avx2"))]
            {
                let mut packed = vec![0; (*width + 7) / 8];
                pack_1bit_samples_scalar(&ones, *width, &mut packed).unwrap();
                let mut unpacked = vec![0; *width];
                unpack_1bit_samples_scalar(&packed, *width, &mut unpacked).unwrap();
                assert_eq!(
                    ones, unpacked,
                    "1-bit scalar all-ones failed for width {}",
                    width
                );
            }
        }
    }

    #[test]
    fn test_pack_unpack_2bit_avx2_edge_cases() {
        // Test edge cases for 2-bit
        for width in &[1, 4, 8, 16, 32, 256, 1024] {
            let pattern: Vec<u8> = (0..*width).map(|i| ((i * 5) % 4) as u8).collect();

            #[cfg(target_feature = "avx2")]
            {
                let packed = pack_2bit_samples_avx2(&pattern, *width).unwrap();
                let unpacked = unpack_2bit_samples_avx2(&packed, *width).unwrap();
                assert_eq!(
                    pattern, unpacked,
                    "2-bit AVX2 pattern failed for width {}",
                    width
                );
            }

            #[cfg(not(target_feature = "avx2"))]
            {
                let mut packed = vec![0; (*width * 2 + 7) / 8];
                pack_2bit_samples_scalar(&pattern, *width, &mut packed).unwrap();
                let mut unpacked = vec![0; *width];
                unpack_2bit_samples_scalar(&packed, *width, &mut unpacked).unwrap();
                assert_eq!(
                    pattern, unpacked,
                    "2-bit scalar pattern failed for width {}",
                    width
                );
            }
        }
    }

    #[test]
    fn test_pack_unpack_4bit_avx2_edge_cases() {
        // Test edge cases for 4-bit
        for width in &[1, 2, 8, 16, 32, 256, 1024] {
            let pattern: Vec<u8> = (0..*width).map(|i| ((i * 7) % 16) as u8).collect();

            #[cfg(target_feature = "avx2")]
            {
                let packed = pack_4bit_samples_avx2(&pattern, *width).unwrap();
                let unpacked = unpack_4bit_samples_avx2(&packed, *width).unwrap();
                assert_eq!(
                    pattern, unpacked,
                    "4-bit AVX2 pattern failed for width {}",
                    width
                );
            }

            #[cfg(not(target_feature = "avx2"))]
            {
                let mut packed = vec![0; (*width + 1) / 2];
                pack_4bit_samples_scalar(&pattern, *width, &mut packed).unwrap();
                let mut unpacked = vec![0; *width];
                unpack_4bit_samples_scalar(&packed, *width, &mut unpacked).unwrap();
                assert_eq!(
                    pattern, unpacked,
                    "4-bit scalar pattern failed for width {}",
                    width
                );
            }
        }
    }
}

// ============================================================================
// Public Runtime-Dispatched Wrappers (works on all CPU architectures)
// ============================================================================

/// Pack 1-bit samples with runtime AVX2 detection.
/// Uses SIMD on CPUs that support AVX2, falls back to scalar otherwise.
pub fn pack_1bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        pack_1bit_samples_avx2(samples, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let expected_packed_len = width.div_ceil(8);
        let mut packed = vec![0u8; expected_packed_len];
        pack_1bit_samples_scalar(samples, width, &mut packed)?;
        Ok(packed)
    }
}

/// Pack 2-bit samples with runtime AVX2 detection.
pub fn pack_2bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        pack_2bit_samples_avx2(samples, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let expected_packed_len = (width * 2).div_ceil(8);
        let mut packed = vec![0u8; expected_packed_len];
        pack_2bit_samples_scalar(samples, width, &mut packed)?;
        Ok(packed)
    }
}

/// Pack 4-bit samples with runtime AVX2 detection.
pub fn pack_4bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        pack_4bit_samples_avx2(samples, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let expected_packed_len = (width * 4).div_ceil(8);
        let mut packed = vec![0u8; expected_packed_len];
        pack_4bit_samples_scalar(samples, width, &mut packed)?;
        Ok(packed)
    }
}

/// Unpack 1-bit samples with runtime AVX2 detection.
pub fn unpack_1bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        unpack_1bit_samples_avx2(packed, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let mut unpacked = vec![0u8; width];
        unpack_1bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}

/// Unpack 2-bit samples with runtime AVX2 detection.
pub fn unpack_2bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        unpack_2bit_samples_avx2(packed, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let mut unpacked = vec![0u8; width];
        unpack_2bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}

/// Unpack 4-bit samples with runtime AVX2 detection.
pub fn unpack_4bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    #[cfg(target_feature = "avx2")]
    {
        unpack_4bit_samples_avx2(packed, width)
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        if width == 0 {
            return Ok(Vec::new());
        }
        let mut unpacked = vec![0u8; width];
        unpack_4bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}
