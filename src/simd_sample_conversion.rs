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
//! `expand_8to16`/`reduce_16to8` are wired into `color.rs`'s bit_depth=16
//! GRAY/RGB/GRAY_ALPHA/RGBA conversion paths (uint sample_format only).
//! `expand_8to32float`/`reduce_32float_to8` remain unused for now: they are
//! plain scalar code (no AVX2 kernel), and their rounding does not exactly
//! match `u8_to_float`/`float_to_u8` (division vs. reciprocal
//! multiplication can differ by 1 ULP), so wiring them up would risk a
//! silent output change for zero performance benefit. Kept public/tested as
//! a building block for a future proper AVX2 float implementation.

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
/// - For each 8-bit value `v`: expand to 16-bit by byte replication,
///   `(v << 8) | v`, which is exactly `v * 65535 / 255` (full-range
///   scaling, matching `expand_sample_8_to_n_bits(v, 16)` bit-for-bit) —
///   e.g. 0xFF → 0xFFFF, not 0xFF00. Since both bytes of the big-endian
///   pair equal `v`, this is written as `[v, v]`.
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
        expanded[out_idx + 1] = sample;
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

        // Big-endian 16-bit expansion via byte replication: [sample, sample]
        // per output pair (full-range scaling, matches `(v << 8) | v`).
        // Since both bytes of each pair are identical, interleaving a lane
        // with itself (instead of with zeros) produces the correct result
        // regardless of byte order.
        let expanded_low = _mm_unpacklo_epi8(low_128, low_128); // samples 0..7
        let expanded_mid = _mm_unpackhi_epi8(low_128, low_128); // samples 8..15
        let expanded_high_low = _mm_unpacklo_epi8(high_128, high_128); // samples 16..23
        let expanded_high_high = _mm_unpackhi_epi8(high_128, high_128); // samples 24..31

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
        expanded[out_idx + 1] = sample;
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

/// Converts interleaved 8-bit RGBA pixels to 8-bit grayscale luma samples,
/// using the ITU-R BT.601-ish integer weights already used throughout
/// `color.rs`: `Y = (299*R + 587*G + 114*B) / 1000` (alpha ignored, matching
/// `convert_rgba_to_color_type`'s scalar formula bit-for-bit).
///
/// # Parameters
/// - `rgba`: Interleaved `[R, G, B, A, R, G, B, A, ...]` bytes, length must be
///   a multiple of 4.
///
/// # Returns
/// One grayscale byte per pixel (length = `rgba.len() / 4`).
///
/// # Strategy
/// AVX2 processes 8 pixels (32 bytes) per iteration: `_mm256_shuffle_epi8`
/// deinterleaves each channel into 32-bit lanes (zero-extended), the
/// weighted sum is computed in exact 32-bit integer arithmetic (max
/// `299*255+587*255+114*255 = 255000`, far below `i32::MAX`), then divided
/// by 1000 via a float round-trip (`_mm256_cvtepi32_ps` /
/// `_mm256_cvttps_epi32`, truncating toward zero like integer division for
/// non-negative values) before narrowing back to `u8`. Verified bit-exact
/// against the scalar integer formula for all 16,777,216 `(R, G, B)`
/// combinations (see tests).
pub fn rgba_to_luma8(rgba: &[u8]) -> Result<Vec<u8>> {
    if !rgba.len().is_multiple_of(4) {
        return Err(CafeError::TruncatedFile(
            "rgba_to_luma8: input length must be a multiple of 4".into(),
        ));
    }
    let n_pixels = rgba.len() / 4;
    if n_pixels == 0 {
        return Ok(Vec::new());
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && n_pixels >= 8 {
            return Ok(unsafe { rgba_to_luma8_avx2_impl(rgba, n_pixels) });
        }
    }

    Ok(rgba_to_luma8_scalar(rgba, n_pixels))
}

fn rgba_to_luma8_scalar(rgba: &[u8], n_pixels: usize) -> Vec<u8> {
    let mut out = vec![0u8; n_pixels];
    for (p, out_byte) in out.iter_mut().enumerate() {
        let base = p * 4;
        let r = rgba[base] as u32;
        let g = rgba[base + 1] as u32;
        let b = rgba[base + 2] as u32;
        *out_byte = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;
    }
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba_to_luma8_avx2_impl(rgba: &[u8], n_pixels: usize) -> Vec<u8> {
    let mut out = vec![0u8; n_pixels];
    let mut i = 0; // byte offset into rgba
    const SIMD_WIDTH_BYTES: usize = 32; // 8 pixels x 4 bytes

    // Per 128-bit lane (4 pixels), pshufb masks that pick one channel byte
    // per pixel into the low byte of each 32-bit slot and zero the rest
    // (top bit set = zero in `_mm256_shuffle_epi8`). Broadcasting a single
    // 128-bit mask across both lanes applies it identically to pixels 0-3
    // and 4-7 within one 256-bit chunk.
    let r_mask = _mm256_broadcastsi128_si256(_mm_setr_epi8(
        0, -128, -128, -128, 4, -128, -128, -128, 8, -128, -128, -128, 12, -128, -128, -128,
    ));
    let g_mask = _mm256_broadcastsi128_si256(_mm_setr_epi8(
        1, -128, -128, -128, 5, -128, -128, -128, 9, -128, -128, -128, 13, -128, -128, -128,
    ));
    let b_mask = _mm256_broadcastsi128_si256(_mm_setr_epi8(
        2, -128, -128, -128, 6, -128, -128, -128, 10, -128, -128, -128, 14, -128, -128, -128,
    ));

    let w_r = _mm256_set1_epi32(299);
    let w_g = _mm256_set1_epi32(587);
    let w_b = _mm256_set1_epi32(114);
    let recip_1000 = _mm256_set1_ps(1.0 / 1000.0);

    while i + SIMD_WIDTH_BYTES <= rgba.len() {
        let chunk = _mm256_loadu_si256(rgba.as_ptr().add(i) as *const __m256i);
        let r = _mm256_shuffle_epi8(chunk, r_mask);
        let g = _mm256_shuffle_epi8(chunk, g_mask);
        let b = _mm256_shuffle_epi8(chunk, b_mask);

        let sum = _mm256_add_epi32(
            _mm256_add_epi32(_mm256_mullo_epi32(r, w_r), _mm256_mullo_epi32(g, w_g)),
            _mm256_mullo_epi32(b, w_b),
        );

        // Exact integer sum (fits comfortably in f32's 24-bit mantissa, max
        // 255000 << 2^24) converted to float, divided by 1000, truncated
        // back to integer — bit-exact with `sum / 1000` for all reachable
        // sums (verified exhaustively in tests).
        let gray_f = _mm256_mul_ps(_mm256_cvtepi32_ps(sum), recip_1000);
        let gray_i = _mm256_cvttps_epi32(gray_f);

        let lo = _mm256_castsi256_si128(gray_i);
        let hi = _mm256_extracti128_si256(gray_i, 1);
        let packed16 = _mm_packus_epi32(lo, hi); // 8 x u16, all fit in u8 range
        let packed8 = _mm_packus_epi16(packed16, packed16); // low 8 bytes valid

        let mut tmp = [0u8; 16];
        _mm_storeu_si128(tmp.as_mut_ptr() as *mut __m128i, packed8);
        let out_idx = i / 4;
        out[out_idx..out_idx + 8].copy_from_slice(&tmp[0..8]);

        i += SIMD_WIDTH_BYTES;
    }

    let mut px = i / 4;
    while i < rgba.len() {
        let r = rgba[i] as u32;
        let g = rgba[i + 1] as u32;
        let b = rgba[i + 2] as u32;
        out[px] = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;
        i += 4;
        px += 1;
    }

    out
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

    /// `expand_8to16` must be bit-for-bit identical to
    /// `color::expand_sample_8_to_n_bits(v, 16)` (byte replication / full-range
    /// scaling, `v * 65535 / 255`), not a plain `v << 8` shift — otherwise
    /// wiring it into `color.rs` would silently change encoded output
    /// (e.g. 0xFF would map to 0xFF00 instead of 0xFFFF).
    #[test]
    fn test_expand_8to16_matches_full_range_scaling() {
        let original: Vec<u8> = (0..=255u8).collect();
        let expanded = expand_8to16(&original, original.len()).unwrap();
        for (i, &v) in original.iter().enumerate() {
            let expected = crate::color::expand_sample_8_to_n_bits(v, 16).unwrap();
            let actual = u16::from_be_bytes([expanded[i * 2], expanded[i * 2 + 1]]);
            assert_eq!(
                actual, expected,
                "mismatch for v={v}: got {actual:#06x}, expected {expected:#06x}"
            );
        }
    }

    /// AVX2 path (width >= 32) must also match, exercising the vectorized
    /// kernel rather than just the scalar fallback.
    #[test]
    fn test_expand_8to16_avx2_matches_full_range_scaling_large() {
        let original: Vec<u8> = (0..1024usize).map(|i| (i % 256) as u8).collect();
        let expanded = expand_8to16(&original, original.len()).unwrap();
        for (i, &v) in original.iter().enumerate() {
            let expected = crate::color::expand_sample_8_to_n_bits(v, 16).unwrap();
            let actual = u16::from_be_bytes([expanded[i * 2], expanded[i * 2 + 1]]);
            assert_eq!(actual, expected, "mismatch for index {i}, v={v}");
        }
    }

    /// `reduce_16to8` must match `color::compress_sample_n_to_8bits(v, 16)`
    /// (take the high byte), which is what's actually used on the decode
    /// path after wiring.
    #[test]
    fn test_reduce_16to8_matches_compress_sample() {
        let width = 300; // exercises both AVX2 (>=16) and scalar tail
        let samples_16: Vec<u16> = (0..width).map(|i| ((i * 37) % 65536) as u16).collect();
        let mut bytes = Vec::with_capacity(width * 2);
        for &s in &samples_16 {
            bytes.extend_from_slice(&s.to_be_bytes());
        }
        let reduced = reduce_16to8(&bytes, width).unwrap();
        for (i, &s) in samples_16.iter().enumerate() {
            let expected = crate::color::compress_sample_n_to_8bits(s, 16).unwrap();
            assert_eq!(
                reduced[i], expected,
                "mismatch for index {i}, sample={s:#06x}"
            );
        }
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

    // ========================================================================
    // rgba_to_luma8: scalar reference used by all tests below, matching
    // `convert_rgba_to_color_type`'s `Y = (299R + 587G + 114B) / 1000` formula
    // in color.rs bit-for-bit (alpha ignored by both).
    // ========================================================================
    fn scalar_luma_reference(r: u8, g: u8, b: u8) -> u8 {
        let (r, g, b) = (r as u32, g as u32, b as u32);
        ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8
    }

    #[test]
    fn test_rgba_to_luma8_rejects_non_multiple_of_4() {
        let bad = vec![0u8; 7];
        assert!(rgba_to_luma8(&bad).is_err());
    }

    #[test]
    fn test_rgba_to_luma8_empty() {
        assert_eq!(rgba_to_luma8(&[]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_rgba_to_luma8_matches_scalar_reference_various_sizes() {
        // Sizes spanning below/at/above the AVX2 threshold (8 pixels) and
        // exercising the scalar tail after full SIMD chunks.
        for &n_pixels in &[1usize, 7, 8, 9, 15, 16, 17, 31, 32, 33, 100, 257, 1000] {
            let mut rgba = vec![0u8; n_pixels * 4];
            for (i, byte) in rgba.iter_mut().enumerate() {
                *byte = ((i * 37 + 11) % 256) as u8;
            }
            let result = rgba_to_luma8(&rgba).unwrap();
            assert_eq!(result.len(), n_pixels);
            for (p, &actual) in result.iter().enumerate() {
                let base = p * 4;
                let expected = scalar_luma_reference(rgba[base], rgba[base + 1], rgba[base + 2]);
                assert_eq!(
                    actual, expected,
                    "mismatch at pixel {p} for n_pixels={n_pixels}"
                );
            }
        }
    }

    #[test]
    fn test_rgba_to_luma8_extreme_channel_values() {
        // Values known to be adjacent to rounding boundaries between the
        // integer formula and any float-based approximation.
        let extremes = [0u8, 1, 127, 128, 254, 255];
        for &r in &extremes {
            for &g in &extremes {
                for &b in &extremes {
                    let rgba = [r, g, b, 255];
                    let result = rgba_to_luma8(&rgba).unwrap();
                    let expected = scalar_luma_reference(r, g, b);
                    assert_eq!(result[0], expected, "mismatch for r={r} g={g} b={b}");
                }
            }
        }
    }

    /// Exhaustive check over all 16,777,216 `(R, G, B)` combinations, run
    /// once through the real AVX2 pipeline (8-pixels-per-iteration `_mm256`
    /// load/shuffle/convert/pack) rather than pixel-by-pixel, to guard
    /// against any float-rounding edge case the smaller tests might miss.
    /// This is the same validation performed standalone before wiring the
    /// kernel into `color.rs`.
    #[test]
    fn test_rgba_to_luma8_exhaustive_all_rgb_combinations() {
        let total = 256usize * 256 * 256;
        let mut rgba = vec![0u8; total * 4];
        let mut idx = 0;
        for r in 0u32..=255 {
            for g in 0u32..=255 {
                for b in 0u32..=255 {
                    rgba[idx * 4] = r as u8;
                    rgba[idx * 4 + 1] = g as u8;
                    rgba[idx * 4 + 2] = b as u8;
                    rgba[idx * 4 + 3] = 255;
                    idx += 1;
                }
            }
        }
        let result = rgba_to_luma8(&rgba).unwrap();
        idx = 0;
        for r in 0u32..=255 {
            for g in 0u32..=255 {
                for b in 0u32..=255 {
                    let expected = scalar_luma_reference(r as u8, g as u8, b as u8);
                    assert_eq!(
                        result[idx], expected,
                        "mismatch at r={r} g={g} b={b} (idx={idx})"
                    );
                    idx += 1;
                }
            }
        }
    }
}
