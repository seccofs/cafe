//! SIMD-accelerated sample expansion and reduction
//!
//! Handles conversion between different bit depths:
//! - Expansion: 8→16, 8→32
//! - Reduction: 16→8, 32→8
//!
//! # Dispatch
//! AVX2 support is detected **at runtime** via `is_x86_feature_detected!`;
//! on CPUs without AVX2 (or non-x86_64 targets), the scalar fallback is used
//! automatically. No special build flags are required.
//!
//! # Status
//! These functions are not yet wired into the encode/decode pipeline in
//! `color.rs` (bit-depth conversion there uses `expand_sample_8_to_n_bits`
//! and friends instead). They are kept public and tested as a
//! ready-to-use building block for a future integration; hence the
//! module-wide `allow(dead_code)`.

#![allow(dead_code)]

use crate::error::{CafeError, Result};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Expands 8-bit samples to 16-bit big-endian format.
///
/// # Parameters
/// - `samples_8bit`: Array of 8-bit samples
/// - `width`: Number of samples
///
/// # Returns
/// Vector of 16-bit big-endian samples (each occupies 2 bytes)
///
/// # Strategy
/// - For each 8-bit value: expand to 16-bit by shifting left 8 bits
/// - Example: 0x12 → 0x1200 (big-endian: [0x12, 0x00])
pub fn expand_8to16(samples_8bit: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    if samples_8bit.len() < width {
        return Err(CafeError::TruncatedFile(
            "expand_8to16: insufficient sample data".into(),
        ));
    }
    let total_bytes = width
        .checked_mul(2)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width * 2".into()))?;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && width >= 32 {
            return Ok(unsafe { expand_8to16_avx2_impl(samples_8bit, width, total_bytes) });
        }
    }

    Ok(expand_8to16_scalar(samples_8bit, width, total_bytes))
}

fn expand_8to16_scalar(samples_8bit: &[u8], width: usize, total_bytes: usize) -> Vec<u8> {
    let mut expanded = vec![0u8; total_bytes];
    for (j, &sample) in samples_8bit.iter().enumerate().take(width) {
        let out_idx = j * 2;
        expanded[out_idx] = sample;
        expanded[out_idx + 1] = 0;
    }
    expanded
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn expand_8to16_avx2_impl(samples_8bit: &[u8], width: usize, total_bytes: usize) -> Vec<u8> {
    let mut expanded = vec![0u8; total_bytes];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 samples per iteration

    while i + SIMD_WIDTH <= width {
        let loaded = _mm256_loadu_si256(samples_8bit.as_ptr().add(i) as *const __m256i);

        let low_128 = _mm256_castsi256_si128(loaded);
        let high_128 = _mm256_extracti128_si256(loaded, 1);
        let zeros = _mm_setzero_si128();

        // Big-endian 16-bit expansion: [sample, 0x00] per output pair.
        let expanded_low = _mm_unpacklo_epi8(low_128, zeros); // samples 0..7
        let expanded_mid = _mm_unpackhi_epi8(low_128, zeros); // samples 8..15
        let expanded_high_low = _mm_unpacklo_epi8(high_128, zeros); // samples 16..23
        let expanded_high_high = _mm_unpackhi_epi8(high_128, zeros); // samples 24..31

        let dst_ptr = expanded.as_mut_ptr().add(i * 2);
        _mm_storeu_si128(dst_ptr as *mut __m128i, expanded_low);
        _mm_storeu_si128(dst_ptr.add(16) as *mut __m128i, expanded_mid);
        _mm_storeu_si128(dst_ptr.add(32) as *mut __m128i, expanded_high_low);
        _mm_storeu_si128(dst_ptr.add(48) as *mut __m128i, expanded_high_high);

        i += SIMD_WIDTH;
    }

    for (j, &sample) in samples_8bit.iter().enumerate().take(width).skip(i) {
        let out_idx = j * 2;
        expanded[out_idx] = sample;
        expanded[out_idx + 1] = 0;
    }

    expanded
}

/// Reduces 16-bit big-endian samples to 8-bit.
///
/// # Parameters
/// - `samples_16bit`: Array of 16-bit big-endian samples (2 bytes per sample)
/// - `width`: Number of samples to produce (not the byte count)
///
/// # Returns
/// Vector of 8-bit samples
///
/// # Strategy
/// - For each 16-bit sample [high, low]: take the high byte
/// - Example: [0x12, 0x34] → 0x12
pub fn reduce_16to8(samples_16bit: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    let expected_len = width
        .checked_mul(2)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width * 2".into()))?;
    if samples_16bit.len() != expected_len {
        return Err(CafeError::TruncatedFile(format!(
            "reduce_16to8: expected {} bytes, got {}",
            expected_len,
            samples_16bit.len()
        )));
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && width >= 16 {
            return Ok(unsafe { reduce_16to8_avx2_impl(samples_16bit, width) });
        }
    }

    Ok(reduce_16to8_scalar(samples_16bit, width))
}

fn reduce_16to8_scalar(samples_16bit: &[u8], width: usize) -> Vec<u8> {
    let mut reduced = vec![0u8; width];
    for j in 0..width {
        reduced[j] = samples_16bit[j * 2]; // Take high byte (big-endian)
    }
    reduced
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn reduce_16to8_avx2_impl(samples_16bit: &[u8], width: usize) -> Vec<u8> {
    let mut reduced = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 16; // Process 16 samples (32 bytes) per iteration

    // Mask selects byte 0, 2, 4, ... 14 (high byte of each big-endian pair)
    // within each 128-bit lane; the other bytes are zeroed (-1 in _mm256_setr_epi8).
    let mask = _mm256_setr_epi8(
        0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1, 0, 2, 4, 6, 8, 10, 12, 14, -1,
        -1, -1, -1, -1, -1, -1, -1,
    );

    while i + SIMD_WIDTH <= width {
        let loaded = _mm256_loadu_si256(samples_16bit.as_ptr().add(i * 2) as *const __m256i);
        let shuffled = _mm256_shuffle_epi8(loaded, mask);

        let low_128 = _mm256_castsi256_si128(shuffled);
        let high_128 = _mm256_extracti128_si256(shuffled, 1);

        // Low 8 bytes of each 128-bit lane hold the 8 extracted high-bytes.
        let mut buf = [0u8; 16];
        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, low_128);
        reduced[i..i + 8].copy_from_slice(&buf[0..8]);

        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, high_128);
        reduced[i + 8..i + 16].copy_from_slice(&buf[0..8]);

        i += SIMD_WIDTH;
    }

    for j in i..width {
        reduced[j] = samples_16bit[j * 2];
    }

    reduced
}

/// Expands 8-bit samples to 32-bit IEEE 754 float format (big-endian).
///
/// Each 8-bit value [0, 255] is converted to [0.0, 1.0] and stored as big-endian float.
pub fn expand_8to32float(samples_8bit: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    if samples_8bit.len() < width {
        return Err(CafeError::TruncatedFile(
            "expand_8to32float: insufficient sample data".into(),
        ));
    }
    let total_bytes = width
        .checked_mul(4)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width * 4".into()))?;

    // Scalar float conversion (division) doesn't have a meaningful AVX2 win
    // without a much larger rewrite (needs `_mm256_cvtepi32_ps` + shuffle back
    // to big-endian byte order); kept scalar for now but structured for a
    // future AVX2 implementation if profiling shows it's a hot path.
    let mut expanded = vec![0u8; total_bytes];
    let scale = 1.0_f32 / 255.0_f32;
    for (j, &sample) in samples_8bit.iter().enumerate().take(width) {
        let val = (sample as f32) * scale;
        let out_idx = j * 4;
        let bits = val.to_bits();
        expanded[out_idx..out_idx + 4].copy_from_slice(&bits.to_be_bytes());
    }

    Ok(expanded)
}

/// Reduces 32-bit IEEE 754 float samples (big-endian) to 8-bit.
///
/// Clamps values to [0.0, 1.0], scales to [0, 255], and rounds.
pub fn reduce_32float_to8(samples_32bit: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    let expected_len = width
        .checked_mul(4)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width * 4".into()))?;
    if samples_32bit.len() != expected_len {
        return Err(CafeError::TruncatedFile(format!(
            "reduce_32float_to8: expected {} bytes, got {}",
            expected_len,
            samples_32bit.len()
        )));
    }

    let mut reduced = vec![0u8; width];
    for i in 0..width {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&samples_32bit[i * 4..i * 4 + 4]);
        let fval = f32::from_be_bytes(bytes);
        let clamped = fval.clamp(0.0, 1.0);
        let scaled = (clamped * 255.0).round();
        reduced[i] = scaled as u8;
    }

    Ok(reduced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_8to16_roundtrip() {
        let original: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let expanded = expand_8to16(&original, original.len()).unwrap();
        let reduced = reduce_16to8(&expanded, original.len()).unwrap();
        assert_eq!(original, reduced, "8→16→8 roundtrip failed");
    }

    #[test]
    fn test_expand_8to16_large_width() {
        let width = 1024;
        let original: Vec<u8> = (0..width).map(|i| ((i * 17) % 256) as u8).collect();
        let expanded = expand_8to16(&original, width).unwrap();
        assert_eq!(expanded.len(), width * 2);

        let reduced = reduce_16to8(&expanded, width).unwrap();
        assert_eq!(original, reduced);
    }

    #[test]
    fn test_expand_8to16_odd_width_tail() {
        // Widths that don't divide evenly into the SIMD chunk size (32/16)
        // exercise the scalar tail path.
        for width in [1usize, 15, 16, 17, 31, 32, 33, 47, 100, 257] {
            let original: Vec<u8> = (0..width).map(|i| ((i * 29) % 256) as u8).collect();
            let expanded = expand_8to16(&original, width).unwrap();
            let reduced = reduce_16to8(&expanded, width).unwrap();
            assert_eq!(original, reduced, "roundtrip failed for width {width}");
        }
    }

    #[test]
    fn test_expand_8to32float_basic() {
        let original = vec![0, 128, 255];
        let expanded = expand_8to32float(&original, 3).unwrap();
        assert_eq!(expanded.len(), 12); // 3 samples × 4 bytes

        let fval0 = f32::from_be_bytes([expanded[0], expanded[1], expanded[2], expanded[3]]);
        assert!((fval0 - 0.0).abs() < 0.01);

        let fval128 = f32::from_be_bytes([expanded[4], expanded[5], expanded[6], expanded[7]]);
        assert!((fval128 - 0.502).abs() < 0.01);

        let fval255 = f32::from_be_bytes([expanded[8], expanded[9], expanded[10], expanded[11]]);
        assert!((fval255 - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_expand_8to32float_roundtrip() {
        let original = vec![0, 64, 128, 192, 255];
        let expanded = expand_8to32float(&original, 5).unwrap();
        let reduced = reduce_32float_to8(&expanded, 5).unwrap();
        for i in 0..5 {
            assert!(
                (original[i] as i32 - reduced[i] as i32).abs() <= 1,
                "Float roundtrip error for sample {}",
                original[i]
            );
        }
    }

    #[test]
    fn test_reduce_16to8_edge_cases() {
        let zeros_16bit = vec![0u8; 32]; // 16 samples × 2 bytes
        let reduced = reduce_16to8(&zeros_16bit, 16).unwrap();
        assert_eq!(reduced, vec![0u8; 16]);

        let ones_16bit = vec![0xFFu8; 32];
        let reduced = reduce_16to8(&ones_16bit, 16).unwrap();
        assert_eq!(reduced, vec![0xFFu8; 16]);
    }

    #[test]
    fn test_reduce_16to8_length_mismatch_rejected() {
        let bad = vec![0u8; 5]; // odd length, not a multiple of 2
        assert!(reduce_16to8(&bad, 3).is_err());
    }
}
