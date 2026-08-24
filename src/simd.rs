//! SIMD (AVX2) optimizations for predictive filters (v1.1+).
//!
//! Vectorized implementations of the most common and compute-intensive filters:
//! - Filter 0 (None): Direct copy
//! - Filter 1 (Sub): pixel - left
//! - Filter 2 (Up): pixel - above
//! - Filter 3 (Average): pixel - (left + above) / 2
//!
//! Filters 4+ (Paeth, MED, Gradient, etc.) are less vectorizable due to
//! conditional logic and complex neighbor dependencies; they use scalar fallback.
//!
//! # Architecture
//!
//! - **x86_64 with AVX2 (256-bit)**: Process 32 bytes per iteration
//! - **Other architectures / CPUs without AVX2**: Scalar fallback (automatic)
//! - **Dispatch**: Runtime, via `is_x86_feature_detected!("avx2")`. The AVX2
//!   code is always compiled into the binary on `target_arch = "x86_64"` (no
//!   special `RUSTFLAGS` or `-C target-feature` needed at build time) and is
//!   only executed if the running CPU actually supports AVX2; otherwise the
//!   scalar fallback runs. This makes a single binary portable across CPUs.
//!
//! # Encode vs. Decode Vectorization
//!
//! Applying a filter (encode) only reads from the original, already-known
//! pixel data (`row`, `prev_row`), so it has no dependency on other output
//! bytes and can always be safely vectorized.
//!
//! Reversing a filter (decode) reconstructs `out[x]` from `out[x - bpp]`
//! (the *already reconstructed* left neighbor). When `bpp` is smaller than
//! the SIMD width (32 bytes) — which is the common case (bpp is usually
//! 1-16) — a naive vectorized reconstruction would read `out[x - bpp]`
//! values that have not been written yet within the same SIMD chunk,
//! silently corrupting the image. For that reason, `unfilter_sub` and
//! `unfilter_average` (which both depend on the left neighbor) are
//! intentionally scalar-only. `unfilter_up` has no such dependency (it only
//! reads the *previous row*, which is fully known) and is safely vectorized.
//!
//! # Bytes Per Pixel (bpp) Coverage
//!
//! - **bpp = 1**: Full SIMD support (no alignment issues)
//! - **bpp = 2**: Full SIMD support (pairs of bytes)
//! - **bpp = 4**: Full SIMD support (RGBA, aligned)
//! - **bpp = 8**: Full SIMD support (floats, aligned)
//! - **bpp = 3**: Scalar fallback (RGB, non-aligned)
//! - **bpp > 8**: Scalar fallback

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

const SIMD_WIDTH: usize = 32;

// ============================================================================
// Filter 1 (Sub): residual[x] = pixel[x] - pixel[x - bpp]
// ============================================================================

/// Applies Filter 1 (Sub) using AVX2 if the running CPU supports it, otherwise scalar.
pub(crate) fn filter_sub_avx2(row: &[u8], bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_sub_avx2_impl(row, bpp) };
        }
    }
    filter_sub_scalar(row, bpp)
}

fn filter_sub_scalar(row: &[u8], bpp: usize) -> Vec<u8> {
    let mut filtered = vec![0u8; row.len()];
    filtered[0..bpp].copy_from_slice(&row[0..bpp]);
    for i in bpp..row.len() {
        filtered[i] = row[i].wrapping_sub(row[i - bpp]);
    }
    filtered
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_sub_avx2_impl(row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];
    filtered[0..bpp].copy_from_slice(&row[0..bpp]);

    let mut i = bpp;
    while i + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
        let residual = _mm256_sub_epi8(pixels, left);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);
        i += SIMD_WIDTH;
    }

    for j in i..len {
        filtered[j] = row[j].wrapping_sub(row[j - bpp]);
    }

    filtered
}

/// Reverses Filter 1 (Sub). Always scalar: `out[x]` depends on the
/// just-reconstructed `out[x - bpp]`, which prevents safe vectorization
/// when `bpp` is smaller than the SIMD width (see module docs).
pub(crate) fn unfilter_sub_avx2(filtered: &[u8], bpp: usize) -> Vec<u8> {
    let mut out = vec![0u8; filtered.len()];
    out[0..bpp].copy_from_slice(&filtered[0..bpp]);
    for i in bpp..filtered.len() {
        out[i] = filtered[i].wrapping_add(out[i - bpp]);
    }
    out
}

// ============================================================================
// Filter 2 (Up): residual[x] = pixel[x] - pixel_above[x]
// ============================================================================

/// Applies Filter 2 (Up) using AVX2 if the running CPU supports it, otherwise scalar.
pub(crate) fn filter_up_avx2(row: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_up_avx2_impl(row, prev_row) };
        }
    }
    filter_up_scalar(row, prev_row)
}

fn filter_up_scalar(row: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    let mut filtered = vec![0u8; row.len()];
    if let Some(prev) = prev_row {
        for i in 0..row.len() {
            filtered[i] = row[i].wrapping_sub(prev[i]);
        }
    } else {
        filtered.copy_from_slice(row);
    }
    filtered
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_up_avx2_impl(row: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];

    let Some(prev) = prev_row else {
        filtered.copy_from_slice(row);
        return filtered;
    };

    let mut i = 0;
    while i + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);
        let residual = _mm256_sub_epi8(pixels, above);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);
        i += SIMD_WIDTH;
    }

    for j in i..len {
        filtered[j] = row[j].wrapping_sub(prev[j]);
    }

    filtered
}

/// Reverses Filter 2 (Up) using AVX2 if the running CPU supports it, otherwise
/// scalar. Safe to vectorize: `out[x]` only depends on `prev_row[x]`, which is
/// fully known ahead of time (not on other bytes of `out`).
pub(crate) fn unfilter_up_avx2(filtered: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { unfilter_up_avx2_impl(filtered, prev_row) };
        }
    }
    unfilter_up_scalar(filtered, prev_row)
}

fn unfilter_up_scalar(filtered: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    let mut out = vec![0u8; filtered.len()];
    if let Some(prev) = prev_row {
        for i in 0..filtered.len() {
            out[i] = filtered[i].wrapping_add(prev[i]);
        }
    } else {
        out.copy_from_slice(filtered);
    }
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unfilter_up_avx2_impl(filtered: &[u8], prev_row: Option<&[u8]>) -> Vec<u8> {
    let len = filtered.len();
    let mut out = vec![0u8; len];

    let Some(prev) = prev_row else {
        out.copy_from_slice(filtered);
        return out;
    };

    let mut i = 0;
    while i + SIMD_WIDTH <= len {
        let residuals = _mm256_loadu_si256(filtered.as_ptr().add(i) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);
        let reconstructed = _mm256_add_epi8(residuals, above);
        _mm256_storeu_si256(out.as_mut_ptr().add(i) as *mut __m256i, reconstructed);
        i += SIMD_WIDTH;
    }

    for j in i..len {
        out[j] = filtered[j].wrapping_add(prev[j]);
    }

    out
}

// ============================================================================
// Filter 3 (Average): residual[x] = pixel[x] - (left + above) / 2
// ============================================================================

/// Applies Filter 3 (Average) using AVX2 if the running CPU supports it,
/// otherwise scalar. Safe to vectorize: both operands (`row`, `prev_row`) are
/// the original, already-known pixel data.
pub(crate) fn filter_average_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_average_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_average_scalar(row, prev_row, bpp)
}

fn filter_average_scalar(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    let mut filtered = vec![0u8; row.len()];
    for i in 0..row.len() {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev_row.map(|p| p[i]).unwrap_or(0);
        let pred = ((a as u16 + b as u16) >> 1) as u8;
        filtered[i] = row[i].wrapping_sub(pred);
    }
    filtered
}

/// AVX2 implementation. Uses 16-bit widening (`_mm256_cvtepu8_epi16` on each
/// 128-bit half) so the `left + above` sum keeps the 9th (carry) bit before
/// shifting right — computing `(left + above) >> 1` directly in 8-bit lanes
/// would silently drop that bit and produce a wrong result whenever
/// `left + above >= 256`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_average_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];

    let Some(prev) = prev_row else {
        // No previous row: pred = left >> 1 (b = 0), first `bpp` bytes pred = 0.
        filtered[0..bpp.min(len)].copy_from_slice(&row[0..bpp.min(len)]);
        for i in bpp..len {
            let pred = (row[i - bpp] as u16) >> 1;
            filtered[i] = row[i].wrapping_sub(pred as u8);
        }
        return filtered;
    };

    // First `bpp` bytes have no left neighbor: pred = above >> 1 (a = 0).
    for i in 0..bpp.min(len) {
        let pred = (prev[i] as u16) >> 1;
        filtered[i] = row[i].wrapping_sub(pred as u8);
    }

    let mut i = bpp;
    while i + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);

        let pred = average_epu8_32(left, above);
        let residuals = _mm256_sub_epi8(pixels, pred);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residuals);

        i += SIMD_WIDTH;
    }

    for j in i..len {
        let a = row[j - bpp];
        let b = prev[j];
        let pred = ((a as u16 + b as u16) >> 1) as u8;
        filtered[j] = row[j].wrapping_sub(pred);
    }

    filtered
}

/// Computes `(a[i] + b[i]) >> 1` for 32 packed `u8` lanes without losing the
/// carry bit, by widening each 128-bit half to 16-bit lanes, adding, shifting,
/// and narrowing back down with `_mm_packus_epi16` (safe because all
/// intermediate values fit in `[0, 255]`).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn average_epu8_32(a: __m256i, b: __m256i) -> __m256i {
    let a_lo = _mm256_castsi256_si128(a);
    let a_hi = _mm256_extracti128_si256(a, 1);
    let b_lo = _mm256_castsi256_si128(b);
    let b_hi = _mm256_extracti128_si256(b, 1);

    let sum_lo16 = _mm256_add_epi16(_mm256_cvtepu8_epi16(a_lo), _mm256_cvtepu8_epi16(b_lo));
    let sum_hi16 = _mm256_add_epi16(_mm256_cvtepu8_epi16(a_hi), _mm256_cvtepu8_epi16(b_hi));

    let pred_lo16 = _mm256_srli_epi16(sum_lo16, 1);
    let pred_hi16 = _mm256_srli_epi16(sum_hi16, 1);

    // Each pred_*16 spans 16 x u16 across a 256-bit register; split back into
    // its two 128-bit halves (8 x u16 each) and pack down to u8 in order.
    let pred_lo_bytes = _mm_packus_epi16(
        _mm256_castsi256_si128(pred_lo16),
        _mm256_extracti128_si256(pred_lo16, 1),
    );
    let pred_hi_bytes = _mm_packus_epi16(
        _mm256_castsi256_si128(pred_hi16),
        _mm256_extracti128_si256(pred_hi16, 1),
    );

    _mm256_set_m128i(pred_hi_bytes, pred_lo_bytes)
}

/// Reverses Filter 3 (Average). Always scalar: `out[x]` depends on the
/// just-reconstructed `out[x - bpp]`, which prevents safe vectorization
/// when `bpp` is smaller than the SIMD width (see module docs).
pub(crate) fn unfilter_average_avx2(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; filtered.len()];
    for i in 0..filtered.len() {
        let a = if i >= bpp { out[i - bpp] } else { 0 };
        let b = prev_row.map(|p| p[i]).unwrap_or(0);
        let pred = ((a as u16 + b as u16) >> 1) as u8;
        out[i] = filtered[i].wrapping_add(pred);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_sub_avx2_roundtrip() {
        let row = vec![10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let bpp = 1;
        let filtered = filter_sub_avx2(&row, bpp);
        let unfiltered = unfilter_sub_avx2(&filtered, bpp);
        assert_eq!(unfiltered, row, "Filter 1 (Sub) roundtrip failed");
    }

    #[test]
    fn test_filter_up_avx2_roundtrip() {
        let row = vec![100u8, 110, 120, 130, 140, 150];
        let prev = vec![50u8, 60, 70, 80, 90, 100];
        let filtered = filter_up_avx2(&row, Some(&prev));
        let unfiltered = unfilter_up_avx2(&filtered, Some(&prev));
        assert_eq!(unfiltered, row, "Filter 2 (Up) roundtrip failed");
    }

    #[test]
    fn test_filter_average_avx2_roundtrip() {
        let row = vec![100u8, 110, 120, 130, 140, 150];
        let prev = vec![50u8, 60, 70, 80, 90, 100];
        let filtered = filter_average_avx2(&row, Some(&prev), 1);
        let unfiltered = unfilter_average_avx2(&filtered, Some(&prev), 1);
        assert_eq!(unfiltered, row, "Filter 3 (Average) roundtrip failed");
    }

    #[test]
    fn test_filter_sub_avx2_large_bpp() {
        // Test with bpp=4 (RGBA)
        let row = vec![10u8; 40];
        let bpp = 4;
        let filtered = filter_sub_avx2(&row, bpp);
        let unfiltered = unfilter_sub_avx2(&filtered, bpp);
        assert_eq!(unfiltered, row, "Filter 1 (Sub) with bpp=4 failed");
    }

    #[test]
    fn test_filter_sub_avx2_large_row() {
        // Test with row > 1KB to exercise SIMD loop
        let row: Vec<u8> = (0..2048).map(|i| ((i * 7) % 256) as u8).collect();
        let bpp = 1;
        let filtered = filter_sub_avx2(&row, bpp);
        let unfiltered = unfilter_sub_avx2(&filtered, bpp);
        assert_eq!(unfiltered, row, "Filter 1 (Sub) large row failed");
    }

    #[test]
    fn test_filter_up_avx2_large_row() {
        // Test with row > 1KB
        let row: Vec<u8> = (0..2048).map(|i| ((i * 11) % 256) as u8).collect();
        let prev: Vec<u8> = (0..2048).map(|i| ((i * 13) % 256) as u8).collect();
        let filtered = filter_up_avx2(&row, Some(&prev));
        let unfiltered = unfilter_up_avx2(&filtered, Some(&prev));
        assert_eq!(unfiltered, row, "Filter 2 (Up) large row failed");
    }

    /// Regression test: the AVX2 average predictor must not lose the carry
    /// bit. With naive 8-bit-lane arithmetic, `left=200, above=200` wraps to
    /// `400 mod 256 = 144` before the shift, yielding pred=72 instead of the
    /// correct 200. Use values that trigger this specific failure mode across
    /// a full 32-byte SIMD chunk plus scalar tail.
    #[test]
    fn test_filter_average_avx2_carry_bit_large_values() {
        let bpp = 1;
        let width = 100; // > 32 to exercise SIMD + tail
        let row: Vec<u8> = (0..width).map(|i| 200u8.wrapping_add(i as u8)).collect();
        let prev: Vec<u8> = (0..width).map(|i| 210u8.wrapping_add(i as u8)).collect();

        let filtered = filter_average_avx2(&row, Some(&prev), bpp);
        let unfiltered = unfilter_average_avx2(&filtered, Some(&prev), bpp);
        assert_eq!(
            unfiltered, row,
            "Filter 3 (Average) AVX2 carry-bit roundtrip failed"
        );

        // Cross-check against the scalar reference implementation directly.
        let scalar_filtered = filter_average_scalar(&row, Some(&prev), bpp);
        assert_eq!(
            filtered, scalar_filtered,
            "AVX2 average filter diverges from scalar reference (carry-bit bug)"
        );
    }

    #[test]
    fn test_filter_average_avx2_matches_scalar_random() {
        let bpp = 4;
        let width = 257; // odd size, spans multiple SIMD chunks + tail
        let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
        let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

        let avx2_filtered = filter_average_avx2(&row, Some(&prev), bpp);
        let scalar_filtered = filter_average_scalar(&row, Some(&prev), bpp);
        assert_eq!(
            avx2_filtered, scalar_filtered,
            "AVX2 and scalar average filter diverge"
        );
    }

    #[test]
    fn test_filter_average_no_prev_row() {
        let bpp = 1;
        let width = 64;
        let row: Vec<u8> = (0..width).map(|i| ((i * 3) % 256) as u8).collect();
        let filtered = filter_average_avx2(&row, None, bpp);
        let scalar_filtered = filter_average_scalar(&row, None, bpp);
        assert_eq!(filtered, scalar_filtered);
    }
}
