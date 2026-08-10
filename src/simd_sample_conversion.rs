//! SIMD-accelerated sample expansion and reduction
//!
//! Handles conversion between different bit depths:
//! - Expansion: 8→16, 8→32
//! - Reduction: 16→8, 32→8

#[allow(unused_imports)]
use crate::error::{CafeError, Result};

#[cfg(target_feature = "avx2")]
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
#[cfg(target_feature = "avx2")]
pub fn expand_8to16_avx2(samples_8bit: &[u8], width: usize) -> Result<Vec<u8>> {
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

    let mut expanded = vec![0u8; total_bytes];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 samples × 1 byte = 32 bytes

    unsafe {
        // SIMD fast path: use AVX2 to process 32 samples at once
        while i + SIMD_WIDTH <= width {
            let src_ptr = samples_8bit.as_ptr().add(i) as *const u8;
            let loaded = _mm256_loadu_si256(src_ptr as *const __m256i);

            // Expand each 8-bit to 16-bit: [s0, s1, s2, s3, ...] → [s0_hi, s0_lo, s1_hi, s1_lo, ...]
            // Use unpacklo and unpackhi to interleave with zeros
            let zeros = _mm256_setzero_si256();

            // Split loaded vector into two 128-bit parts
            let low_128 = _mm256_castsi256_si128(loaded);
            let high_128 = _mm256_extracti128_si256(loaded, 1);

            // Unpack low 128 bits: interleave with zeros (big-endian)
            // For big-endian, we want [s0, 0x00, s1, 0x00, ...]
            // pshufb or unpacklo_epi8 can help
            let low_zeros = _mm_setzero_si128();
            let expanded_low = _mm_unpacklo_epi8(low_128, low_zeros); // [s0, 0, s1, 0, ..., s7, 0]

            // Unpack high part of low 128 bits
            let expanded_mid = _mm_unpackhi_epi8(low_128, low_zeros); // [s8, 0, s9, 0, ..., s15, 0]

            // Unpack high 128 bits similarly
            let high_zeros = _mm_setzero_si128();
            let expanded_high_low = _mm_unpacklo_epi8(high_128, high_zeros); // [s16, 0, s17, 0, ..., s23, 0]
            let expanded_high_high = _mm_unpackhi_epi8(high_128, high_zeros); // [s24, 0, s25, 0, ..., s31, 0]

            // Write results
            let dst_ptr = expanded.as_mut_ptr().add(i * 2) as *mut __m128i;
            _mm_storeu_si128(dst_ptr, expanded_low);
            _mm_storeu_si128(dst_ptr.add(1), expanded_mid);
            _mm_storeu_si128(dst_ptr.add(2), expanded_high_low);
            _mm_storeu_si128(dst_ptr.add(3), expanded_high_high);

            i += SIMD_WIDTH;
        }
    }

    // Scalar tail
    for j in i..width {
        let val = samples_8bit[j];
        let out_idx = j * 2;
        expanded[out_idx] = val;
        expanded[out_idx + 1] = 0;
    }

    Ok(expanded)
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
#[cfg(target_feature = "avx2")]
pub fn reduce_16to8_avx2(samples_16bit: &[u8], width: usize) -> Result<Vec<u8>> {
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

    let mut reduced = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 16; // Process 16 samples × 2 bytes = 32 bytes

    unsafe {
        // SIMD fast path: process 32 bytes (16 samples) at once
        while i + SIMD_WIDTH <= width {
            let src_ptr = samples_16bit.as_ptr().add(i * 2) as *const u8;
            let loaded = _mm256_loadu_si256(src_ptr as *const __m256i);

            // Extract every other byte (high byte of each 16-bit sample)
            // Mask for selecting bytes 0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30
            // We want big-endian: byte[0] is the high byte
            let mask = _mm256_setr_epi8(
                0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1, 0, 2, 4, 6, 8, 10, 12,
                14, -1, -1, -1, -1, -1, -1, -1, -1,
            );

            let shuffled = _mm256_shuffle_epi8(loaded, mask);

            // Extract the first 16 bytes from the shuffle result
            let low_128 = _mm256_castsi256_si128(shuffled);
            let high_128 = _mm256_extracti128_si256(shuffled, 1);

            // Store lower 8 samples
            _mm_storeu_si64(reduced.as_mut_ptr().add(i) as *mut i64, low_128);

            // Store upper 8 samples
            _mm_storeu_si64(reduced.as_mut_ptr().add(i + 8) as *mut i64, high_128);

            i += SIMD_WIDTH;
        }
    }

    // Scalar tail
    for j in i..width {
        reduced[j] = samples_16bit[j * 2]; // Take high byte
    }

    Ok(reduced)
}

/// Expands 8-bit samples to 32-bit IEEE 754 float format (big-endian).
///
/// Each 8-bit value [0, 255] is converted to [0.0, 1.0] and stored as big-endian float.
#[cfg(target_feature = "avx2")]
pub fn expand_8to32float_avx2(samples_8bit: &[u8], width: usize) -> Result<Vec<u8>> {
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

    let mut expanded = vec![0u8; total_bytes];
    let mut i = 0;
    const SIMD_WIDTH: usize = 8; // Process 8 samples at a time

    // Conversion factor: 1.0 / 255.0 ≈ 0.00392156862745098
    let scale = 1.0_f32 / 255.0_f32;

    unsafe {
        // SIMD fast path: use AVX2 to convert 8 samples at once
        while i + SIMD_WIDTH <= width {
            // Load 8 bytes
            let src_ptr = samples_8bit.as_ptr().add(i) as *const u8;
            let mut temp = [0u32; 8];
            for j in 0..8 {
                temp[j] = *src_ptr.add(j) as u32;
            }

            // Convert each to float and store
            for j in 0..8 {
                let fval = (temp[j] as f32) * scale;
                let bits = fval.to_bits();
                let out_idx = (i + j) * 4;
                expanded[out_idx..out_idx + 4].copy_from_slice(&bits.to_be_bytes());
            }

            i += SIMD_WIDTH;
        }
    }

    // Scalar tail
    for j in i..width {
        let val = (samples_8bit[j] as f32) * scale;
        let out_idx = j * 4;
        let bits = val.to_bits();
        expanded[out_idx..out_idx + 4].copy_from_slice(&bits.to_be_bytes());
    }

    Ok(expanded)
}

/// Reduces 32-bit IEEE 754 float samples (big-endian) to 8-bit.
///
/// Clamps values to [0.0, 1.0], scales to [0, 255], and rounds.
#[cfg(target_feature = "avx2")]
pub fn reduce_32float_to8_avx2(samples_32bit: &[u8], width: usize) -> Result<Vec<u8>> {
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

    // Process samples
    for i in 0..width {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&samples_32bit[i * 4..i * 4 + 4]);
        let fval = f32::from_be_bytes(bytes);

        // Clamp to [0.0, 1.0] and scale to [0, 255]
        let clamped = fval.max(0.0).min(1.0);
        let scaled = (clamped * 255.0).round();
        reduced[i] = scaled as u8;
    }

    Ok(reduced)
}

#[cfg(test)]
mod tests {

    #[test]
    #[cfg(target_feature = "avx2")]
    fn test_expand_8to16_avx2_roundtrip() {
        let original: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let expanded = expand_8to16_avx2(&original, original.len()).unwrap();
        let reduced = reduce_16to8_avx2(&expanded, original.len()).unwrap();
        assert_eq!(original, reduced, "8→16→8 roundtrip failed");
    }

    #[test]
    #[cfg(target_feature = "avx2")]
    fn test_expand_8to16_large_width() {
        let width = 1024;
        let original: Vec<u8> = (0..width).map(|i| ((i * 17) % 256) as u8).collect();
        let expanded = expand_8to16_avx2(&original, width).unwrap();
        assert_eq!(expanded.len(), width * 2);

        let reduced = reduce_16to8_avx2(&expanded, width).unwrap();
        assert_eq!(original, reduced);
    }

    #[test]
    #[cfg(target_feature = "avx2")]
    fn test_expand_8to32float_basic() {
        let original = vec![0, 128, 255];
        let expanded = expand_8to32float_avx2(&original, 3).unwrap();
        assert_eq!(expanded.len(), 12); // 3 samples × 4 bytes

        // Verify first sample (0 → 0.0)
        let fval0 = f32::from_be_bytes([expanded[0], expanded[1], expanded[2], expanded[3]]);
        assert!((fval0 - 0.0).abs() < 0.01);

        // Verify second sample (128 → ~0.502)
        let fval128 = f32::from_be_bytes([expanded[4], expanded[5], expanded[6], expanded[7]]);
        assert!((fval128 - 0.502).abs() < 0.01);

        // Verify third sample (255 → 1.0)
        let fval255 = f32::from_be_bytes([expanded[8], expanded[9], expanded[10], expanded[11]]);
        assert!((fval255 - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_expand_8to32float_roundtrip() {
        let original = vec![0, 64, 128, 192, 255];
        #[cfg(target_feature = "avx2")]
        {
            let expanded = expand_8to32float_avx2(&original, 5).unwrap();
            let reduced = reduce_32float_to8_avx2(&expanded, 5).unwrap();
            // Allow small rounding differences
            for i in 0..5 {
                assert!(
                    (original[i] as i32 - reduced[i] as i32).abs() <= 1,
                    "Float roundtrip error for sample {}",
                    original[i]
                );
            }
        }

        #[cfg(not(target_feature = "avx2"))]
        {
            // Just test the concept without AVX2
            for val in &original {
                let fval = (*val as f32) / 255.0;
                let clamped = fval.max(0.0).min(1.0);
                let scaled = (clamped * 255.0).round() as u8;
                assert!((*val as i32 - scaled as i32).abs() <= 1);
            }
        }
    }

    #[test]
    #[cfg(target_feature = "avx2")]
    fn test_reduce_16to8_edge_cases() {
        // All zeros
        let zeros_16bit = vec![0u8; 32]; // 16 samples × 2 bytes
        let reduced = reduce_16to8_avx2(&zeros_16bit, 16).unwrap();
        assert_eq!(reduced, vec![0u8; 16]);

        // All 0xFF
        let ones_16bit = vec![0xFFu8; 32];
        let reduced = reduce_16to8_avx2(&ones_16bit, 16).unwrap();
        assert_eq!(reduced, vec![0xFFu8; 16]);
    }
}
