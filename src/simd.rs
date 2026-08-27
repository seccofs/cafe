//! SIMD (AVX2) optimizations for predictive filters (v1.1+).
//!
//! Vectorized implementations of the most common and compute-intensive filters:
//! - Filter 0 (None): Direct copy
//! - Filter 1 (Sub): pixel - left
//! - Filter 2 (Up): pixel - above
//! - Filter 3 (Average): pixel - (left + above) / 2
//! - Filter 6 (Gradient): pixel - (left + above - diagonal), pure mod-256 arithmetic
//! - Filters 9-12 (4-way Directional): weighted averages of left/above/diagonal
//! - Filter 14 (TR-Directional): bilinear average of left/above/diagonal/top-right
//!
//! Filters 4, 5, 7, 8, 13, 15 (Paeth, MED, Simple Median, 2nd Order, Context,
//! Weighted) are less vectorizable due to conditional/branchy logic or
//! in-flight adaptive state; they use scalar fallback only.
//!
//! # Encode-only Vectorization for Left-Dependent Filters
//!
//! Filters 1, 3, 6, 9-12 and 14 all use the *same-row* left neighbor (`a`,
//! and for 14 also the diagonal `c`) as part of their prediction. Applying
//! the filter (encode) reads only from the original row, so it is always
//! safe to vectorize (see "Encode vs. Decode Vectorization" below). Reversing
//! it (decode) reconstructs `out[x]` from `out[x - bpp]`, the *just
//! reconstructed* neighbor — unsafe to vectorize for the same reason as Sub
//! and Average, so their `unfilter_*` counterparts remain scalar-only
//! (handled directly in `filter.rs`, not duplicated here).
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

/// Widens one 128-bit half (16 x `u8`) of a 32-byte SIMD chunk to 16 x `u16`
/// lanes (zero-extended), so arithmetic that can overflow 8 bits (sums,
/// weighted sums) keeps full precision before narrowing back down.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn widen_epu8_16_to_epu16(half: __m128i) -> __m256i {
    _mm256_cvtepu8_epi16(half)
}

/// Narrows a widened 16 x `u16` lane group (values must fit in `[0, 255]`,
/// which every predictor below guarantees) back down to 16 x `u8` via
/// `_mm_packus_epi16`, splitting the 256-bit input into its two 128-bit
/// halves first (see `widen_epu8_16_to_epu16`).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn narrow_epu16_to_epu8_16(wide: __m256i) -> __m128i {
    _mm_packus_epi16(
        _mm256_castsi256_si128(wide),
        _mm256_extracti128_si256(wide, 1),
    )
}

/// Splits a full 32-byte SIMD chunk into its two 128-bit halves and widens
/// each to 16 x `u16` lanes, returning `(lo, hi)` covering bytes 0-15 and
/// 16-31 respectively.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn widen_epu8_32_to_epu16_pair(v: __m256i) -> (__m256i, __m256i) {
    let lo = _mm256_castsi256_si128(v);
    let hi = _mm256_extracti128_si256(v, 1);
    (widen_epu8_16_to_epu16(lo), widen_epu8_16_to_epu16(hi))
}

/// Inverse of `widen_epu8_32_to_epu16_pair`: narrows a `(lo, hi)` pair of
/// widened 16 x `u16` lane groups back into a single 32-byte `u8` chunk.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn narrow_epu16_pair_to_epu8_32(lo: __m256i, hi: __m256i) -> __m256i {
    _mm256_set_m128i(narrow_epu16_to_epu8_16(hi), narrow_epu16_to_epu8_16(lo))
}

/// Computes `(a[i] + b[i]) >> 1` for 32 packed `u8` lanes without losing the
/// carry bit, by widening each 128-bit half to 16-bit lanes, adding, shifting,
/// and narrowing back down (safe because all intermediate values fit in
/// `[0, 255]`).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn average_epu8_32(a: __m256i, b: __m256i) -> __m256i {
    let (a_lo, a_hi) = widen_epu8_32_to_epu16_pair(a);
    let (b_lo, b_hi) = widen_epu8_32_to_epu16_pair(b);

    let pred_lo16 = _mm256_srli_epi16(_mm256_add_epi16(a_lo, b_lo), 1);
    let pred_hi16 = _mm256_srli_epi16(_mm256_add_epi16(a_hi, b_hi), 1);

    narrow_epu16_pair_to_epu8_32(pred_lo16, pred_hi16)
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

// ============================================================================
// Filter 6 (Gradient): residual[x] = pixel[x] - (left + above - diagonal)
// ============================================================================

/// Applies Filter 6 (Gradient) using AVX2 if the running CPU supports it,
/// otherwise scalar. Pure mod-256 arithmetic (`wrapping_add`/`wrapping_sub`),
/// so it needs no 16-bit widening, unlike Average.
pub(crate) fn filter_gradient_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_gradient_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_gradient_scalar(row, prev_row, bpp)
}

fn filter_gradient_scalar(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    let mut filtered = vec![0u8; row.len()];
    for i in 0..row.len() {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev_row.map(|p| p[i]).unwrap_or(0);
        let c = if i >= bpp {
            prev_row.map(|p| p[i - bpp]).unwrap_or(0)
        } else {
            0
        };
        let pred = a.wrapping_add(b).wrapping_sub(c);
        filtered[i] = row[i].wrapping_sub(pred);
    }
    filtered
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_gradient_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];

    let Some(prev) = prev_row else {
        // No previous row: b = c = 0, so pred = a (first `bpp` bytes pred = 0).
        return filter_sub_avx2_impl(row, bpp);
    };

    // First `bpp` bytes have no left/diagonal neighbor: pred = above (b).
    for i in 0..bpp.min(len) {
        filtered[i] = row[i].wrapping_sub(prev[i]);
    }

    let mut i = bpp;
    while i + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);
        let diag = _mm256_loadu_si256(prev.as_ptr().add(i - bpp) as *const __m256i);

        let pred = _mm256_sub_epi8(_mm256_add_epi8(left, above), diag);
        let residual = _mm256_sub_epi8(pixels, pred);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);

        i += SIMD_WIDTH;
    }

    for j in i..len {
        let a = row[j - bpp];
        let b = prev[j];
        let c = prev[j - bpp];
        let pred = a.wrapping_add(b).wrapping_sub(c);
        filtered[j] = row[j].wrapping_sub(pred);
    }

    filtered
}

// ============================================================================
// Filters 9-12 (4-way Directional): weighted averages of left/above/diagonal
// ============================================================================
//
// All four predictors are exact-integer weighted averages with power-of-two
// (H, V, D1) or non-power-of-two (D2, divisor 5) denominators. Each is
// widened to 16-bit lanes to avoid overflow in the weighted sum, computed,
// then narrowed back with `narrow_epu16_pair_to_epu8_32` (values are
// guaranteed to fit in `[0, 255]` by construction, same guarantee the
// scalar formula relies on).
//
// D2's `/5` uses the fixed-point reciprocal multiply `(sum * 13108) >> 16`
// (`13108 == ceil(2^16 / 5)`), verified exhaustively against the scalar
// `/5` for all 16,777,216 combinations of `a, b, c` in `simd.rs` tests.

/// Applies Filter 9 (4-way Horizontal): `pred = (3*left + above) / 4`.
pub(crate) fn filter_4way_h_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_4way_h_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_4way_scalar(row, prev_row, bpp, |a, b, _c| {
        ((a as u16 * 3 + b as u16) / 4) as u8
    })
}

/// Applies Filter 10 (4-way Vertical): `pred = (left + 3*above) / 4`.
pub(crate) fn filter_4way_v_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_4way_v_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_4way_scalar(row, prev_row, bpp, |a, b, _c| {
        ((a as u16 + b as u16 * 3) / 4) as u8
    })
}

/// Applies Filter 11 (4-way Diagonal \\): `pred = (left + above + 2*diag) / 4`.
pub(crate) fn filter_4way_d1_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_4way_d1_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_4way_scalar(row, prev_row, bpp, |a, b, c| {
        ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
    })
}

/// Applies Filter 12 (4-way Diagonal /): `pred = (2*left + 2*above + diag) / 5`.
pub(crate) fn filter_4way_d2_avx2(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_4way_d2_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_4way_scalar(row, prev_row, bpp, |a, b, c| {
        ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
    })
}

/// Shared scalar fallback for all four 4-way directional filters: applies
/// `pred_fn(left, above, diag)` at every position, with the same tile-edge
/// zero-fill convention (`predict()`/`filter_row` in filter.rs) used
/// everywhere else in the codebase.
fn filter_4way_scalar(
    row: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
    pred_fn: impl Fn(u8, u8, u8) -> u8,
) -> Vec<u8> {
    let mut filtered = vec![0u8; row.len()];
    for i in 0..row.len() {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev_row.map(|p| p[i]).unwrap_or(0);
        let c = if i >= bpp {
            prev_row.map(|p| p[i - bpp]).unwrap_or(0)
        } else {
            0
        };
        filtered[i] = row[i].wrapping_sub(pred_fn(a, b, c));
    }
    filtered
}

/// Computes `pred_fn` for a full 32-byte SIMD chunk given pre-loaded
/// `left`/`above`/`diag` vectors, via 16-bit widening (to avoid overflow in
/// the weighted sum) followed by narrowing back to `u8` — shared by all four
/// 4-way directional AVX2 kernels below, each supplying its own weighted-sum
/// closure operating on the widened 16-bit lane pairs.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn directional_chunk_avx2(
    left: __m256i,
    above: __m256i,
    diag: __m256i,
    weighted_sum_shift: impl Fn(__m256i, __m256i, __m256i) -> __m256i,
) -> __m256i {
    let (a_lo, a_hi) = widen_epu8_32_to_epu16_pair(left);
    let (b_lo, b_hi) = widen_epu8_32_to_epu16_pair(above);
    let (c_lo, c_hi) = widen_epu8_32_to_epu16_pair(diag);

    let pred_lo = weighted_sum_shift(a_lo, b_lo, c_lo);
    let pred_hi = weighted_sum_shift(a_hi, b_hi, c_hi);

    narrow_epu16_pair_to_epu8_32(pred_lo, pred_hi)
}

/// Generic AVX2 body shared by the four 4-way directional filters: loads
/// left/above/diag, computes the predictor via `directional_chunk_avx2`, and
/// subtracts from the pixel — differing only in `weighted_sum_shift` and the
/// no-`prev_row` edge case (H/V need `above=0`, D1/D2 additionally need
/// `diag=0`, both already implied by `prev_row = None` short-circuiting to
/// scalar edge handling for the first `bpp` bytes and the whole row when
/// there is no previous row at all).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_directional_avx2_body(
    row: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
    pred_fn: impl Fn(u8, u8, u8) -> u8,
    weighted_sum_shift: impl Fn(__m256i, __m256i, __m256i) -> __m256i,
) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];

    let Some(prev) = prev_row else {
        for i in 0..len {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            filtered[i] = row[i].wrapping_sub(pred_fn(a, 0, 0));
        }
        return filtered;
    };

    for i in 0..bpp.min(len) {
        filtered[i] = row[i].wrapping_sub(pred_fn(0, prev[i], 0));
    }

    let mut i = bpp;
    while i + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);
        let diag = _mm256_loadu_si256(prev.as_ptr().add(i - bpp) as *const __m256i);

        let pred = directional_chunk_avx2(left, above, diag, &weighted_sum_shift);
        let residual = _mm256_sub_epi8(pixels, pred);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);

        i += SIMD_WIDTH;
    }

    for j in i..len {
        let a = row[j - bpp];
        let b = prev[j];
        let c = prev[j - bpp];
        filtered[j] = row[j].wrapping_sub(pred_fn(a, b, c));
    }

    filtered
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_4way_h_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    filter_directional_avx2_body(
        row,
        prev_row,
        bpp,
        |a, b, _c| ((a as u16 * 3 + b as u16) / 4) as u8,
        |a, b, _c| {
            _mm256_srli_epi16(
                _mm256_add_epi16(_mm256_slli_epi16(a, 1), _mm256_add_epi16(a, b)),
                2,
            )
        },
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_4way_v_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    filter_directional_avx2_body(
        row,
        prev_row,
        bpp,
        |a, b, _c| ((a as u16 + b as u16 * 3) / 4) as u8,
        |a, b, _c| {
            _mm256_srli_epi16(
                _mm256_add_epi16(a, _mm256_add_epi16(_mm256_slli_epi16(b, 1), b)),
                2,
            )
        },
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_4way_d1_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    filter_directional_avx2_body(
        row,
        prev_row,
        bpp,
        |a, b, c| ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8,
        |a, b, c| {
            _mm256_srli_epi16(
                _mm256_add_epi16(_mm256_add_epi16(a, b), _mm256_slli_epi16(c, 1)),
                2,
            )
        },
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_4way_d2_avx2_impl(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    // sum = 2a + 2b + c (max 5*255 = 1275, fits in u16). Exact `/5` via
    // fixed-point reciprocal multiply: floor(sum/5) == (sum * 13108) >> 16
    // for every sum in 0..=1275 (verified exhaustively over all a,b,c in
    // simd.rs tests). `_mm256_mulhi_epu16` directly yields the high 16 bits
    // of the 32-bit product, i.e. `(sum * 13108) >> 16`, without needing a
    // separate widen-to-32-bit step.
    const RECIPROCAL_DIV5: i16 = 13108u16 as i16;
    filter_directional_avx2_body(
        row,
        prev_row,
        bpp,
        |a, b, c| ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8,
        |a, b, c| {
            let sum = _mm256_add_epi16(
                _mm256_add_epi16(_mm256_slli_epi16(a, 1), _mm256_slli_epi16(b, 1)),
                c,
            );
            _mm256_mulhi_epu16(sum, _mm256_set1_epi16(RECIPROCAL_DIV5))
        },
    )
}

// ============================================================================
// Filter 14 (TR-Directional): bilinear average of left/above/diag/top-right
// ============================================================================

/// Applies Filter 14 (TR-Directional): `pred = avg2(avg2(left, diag), avg2(above, tr))`,
/// where `tr` is the top-right neighbor (0 past the right edge). Built out of
/// three nested calls to `average_epu8_32` (already carry-safe via 16-bit
/// widening), matching `tr_directional_predictor` in filter.rs exactly.
pub(crate) fn filter_tr_directional_avx2(
    row: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { filter_tr_directional_avx2_impl(row, prev_row, bpp) };
        }
    }
    filter_tr_directional_scalar(row, prev_row, bpp)
}

fn filter_tr_directional_scalar(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> Vec<u8> {
    let avg2 = |x: u8, y: u8| ((x as u16 + y as u16) >> 1) as u8;
    let mut filtered = vec![0u8; row.len()];
    for i in 0..row.len() {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev_row.map(|p| p[i]).unwrap_or(0);
        let c = if i >= bpp {
            prev_row.map(|p| p[i - bpp]).unwrap_or(0)
        } else {
            0
        };
        let d = if i + bpp < row.len() {
            prev_row.map(|p| p[i + bpp]).unwrap_or(0)
        } else {
            0
        };
        let pred = avg2(avg2(a, c), avg2(b, d));
        filtered[i] = row[i].wrapping_sub(pred);
    }
    filtered
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_tr_directional_avx2_impl(
    row: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
) -> Vec<u8> {
    let len = row.len();
    let mut filtered = vec![0u8; len];

    let Some(prev) = prev_row else {
        // No previous row: b = d = 0 everywhere (c is also 0 for the first
        // `bpp` bytes, same as the general case). pred = avg2(avg2(a, 0), 0),
        // matching the scalar reference exactly.
        let avg2 = |x: u8, y: u8| ((x as u16 + y as u16) >> 1) as u8;
        for i in 0..len {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            let pred = avg2(avg2(a, 0), 0);
            filtered[i] = row[i].wrapping_sub(pred);
        }
        return filtered;
    };

    // First bpp bytes: a = c = 0 (no left/diagonal neighbor yet).
    let avg2 = |x: u8, y: u8| ((x as u16 + y as u16) >> 1) as u8;
    for i in 0..bpp.min(len) {
        let b = prev[i];
        let d = if i + bpp < len { prev[i + bpp] } else { 0 };
        let pred = avg2(avg2(0, 0), avg2(b, d));
        filtered[i] = row[i].wrapping_sub(pred);
    }

    let mut i = bpp;
    while i + bpp + SIMD_WIDTH <= len {
        let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
        let above = _mm256_loadu_si256(prev.as_ptr().add(i) as *const __m256i);
        let diag = _mm256_loadu_si256(prev.as_ptr().add(i - bpp) as *const __m256i);
        let tr = _mm256_loadu_si256(prev.as_ptr().add(i + bpp) as *const __m256i);

        let avg_left_diag = average_epu8_32(left, diag);
        let avg_above_tr = average_epu8_32(above, tr);
        let pred = average_epu8_32(avg_left_diag, avg_above_tr);

        let residual = _mm256_sub_epi8(pixels, pred);
        _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);

        i += SIMD_WIDTH;
    }

    for j in i..len {
        let a = row[j - bpp];
        let b = prev[j];
        let c = prev[j - bpp];
        let d = if j + bpp < len { prev[j + bpp] } else { 0 };
        let pred = avg2(avg2(a, c), avg2(b, d));
        filtered[j] = row[j].wrapping_sub(pred);
    }

    filtered
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

    // ========================================================================
    // Filter 6 (Gradient), 9-12 (4-way Directional), 14 (TR-Directional):
    // AVX2 vs. scalar cross-checks, roundtrip via matching `unfilter_row` in
    // filter.rs (imported directly here to avoid duplicating the reversal
    // logic), and no-prev-row edge cases. `width = 257` (odd, > 1 SIMD chunk)
    // exercises the AVX2 tail path for every filter and bpp combination.
    // ========================================================================

    /// Scalar reversal shared by all the new left-dependent filters, mirroring
    /// `unfilter_row`'s generic scalar loop in filter.rs (kept local to avoid
    /// a `pub(crate)` widening of filter.rs internals just for tests).
    fn unfilter_generic(
        filtered: &[u8],
        prev_row: Option<&[u8]>,
        bpp: usize,
        pred_fn: impl Fn(u8, u8, u8, u8) -> u8,
    ) -> Vec<u8> {
        let mut out = vec![0u8; filtered.len()];
        for i in 0..filtered.len() {
            let a = if i >= bpp { out[i - bpp] } else { 0 };
            let b = prev_row.map(|p| p[i]).unwrap_or(0);
            let c = if i >= bpp {
                prev_row.map(|p| p[i - bpp]).unwrap_or(0)
            } else {
                0
            };
            let d = if i + bpp < filtered.len() {
                prev_row.map(|p| p[i + bpp]).unwrap_or(0)
            } else {
                0
            };
            out[i] = filtered[i].wrapping_add(pred_fn(a, b, c, d));
        }
        out
    }

    fn gradient_pred(a: u8, b: u8, c: u8, _d: u8) -> u8 {
        a.wrapping_add(b).wrapping_sub(c)
    }
    fn h_pred(a: u8, b: u8, _c: u8, _d: u8) -> u8 {
        ((a as u16 * 3 + b as u16) / 4) as u8
    }
    fn v_pred(a: u8, b: u8, _c: u8, _d: u8) -> u8 {
        ((a as u16 + b as u16 * 3) / 4) as u8
    }
    fn d1_pred(a: u8, b: u8, c: u8, _d: u8) -> u8 {
        ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
    }
    fn d2_pred(a: u8, b: u8, c: u8, _d: u8) -> u8 {
        ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
    }
    fn tr_pred(a: u8, b: u8, c: u8, d: u8) -> u8 {
        let avg2 = |x: u8, y: u8| ((x as u16 + y as u16) >> 1) as u8;
        avg2(avg2(a, c), avg2(b, d))
    }

    #[test]
    fn test_filter_gradient_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_gradient_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_gradient_scalar(&row, Some(&prev), bpp);
            assert_eq!(avx2_filtered, scalar_filtered, "Gradient bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, gradient_pred);
            assert_eq!(unfiltered, row, "Gradient roundtrip bpp={bpp}");

            // No-prev-row edge case
            let no_prev = filter_gradient_avx2(&row, None, bpp);
            let no_prev_scalar = filter_gradient_scalar(&row, None, bpp);
            assert_eq!(no_prev, no_prev_scalar, "Gradient no-prev bpp={bpp}");
        }
    }

    /// Regression test mirroring `test_filter_average_avx2_carry_bit_large_values`:
    /// with `a=200, b=210+`, `a+b` can reach 400+, which must not silently wrap
    /// mod 256 in the weighted-sum widening used by the 4-way directional and
    /// TR-directional kernels.
    #[test]
    fn test_directional_filters_carry_bit_large_values() {
        let bpp = 1;
        let width = 100;
        let row: Vec<u8> = (0..width).map(|i| 200u8.wrapping_add(i as u8)).collect();
        let prev: Vec<u8> = (0..width).map(|i| 210u8.wrapping_add(i as u8)).collect();

        macro_rules! check {
            ($avx2:ident, $scalar_pred:expr) => {
                let avx2_filtered = $avx2(&row, Some(&prev), bpp);
                let scalar_filtered = filter_4way_scalar(&row, Some(&prev), bpp, $scalar_pred);
                assert_eq!(
                    avx2_filtered,
                    scalar_filtered,
                    "{} carry-bit mismatch",
                    stringify!($avx2)
                );
            };
        }
        check!(
            filter_4way_h_avx2,
            |a, b, _c| ((a as u16 * 3 + b as u16) / 4) as u8
        );
        check!(
            filter_4way_v_avx2,
            |a, b, _c| ((a as u16 + b as u16 * 3) / 4) as u8
        );
        check!(filter_4way_d1_avx2, |a, b, c| {
            ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
        });
        check!(filter_4way_d2_avx2, |a, b, c| {
            ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
        });
    }

    #[test]
    fn test_filter_4way_h_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_4way_h_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_4way_scalar(&row, Some(&prev), bpp, |a, b, _c| {
                ((a as u16 * 3 + b as u16) / 4) as u8
            });
            assert_eq!(avx2_filtered, scalar_filtered, "4wayH bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, h_pred);
            assert_eq!(unfiltered, row, "4wayH roundtrip bpp={bpp}");

            let no_prev = filter_4way_h_avx2(&row, None, bpp);
            let no_prev_scalar = filter_4way_scalar(&row, None, bpp, |a, b, _c| {
                ((a as u16 * 3 + b as u16) / 4) as u8
            });
            assert_eq!(no_prev, no_prev_scalar, "4wayH no-prev bpp={bpp}");
        }
    }

    #[test]
    fn test_filter_4way_v_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_4way_v_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_4way_scalar(&row, Some(&prev), bpp, |a, b, _c| {
                ((a as u16 + b as u16 * 3) / 4) as u8
            });
            assert_eq!(avx2_filtered, scalar_filtered, "4wayV bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, v_pred);
            assert_eq!(unfiltered, row, "4wayV roundtrip bpp={bpp}");

            let no_prev = filter_4way_v_avx2(&row, None, bpp);
            let no_prev_scalar = filter_4way_scalar(&row, None, bpp, |a, b, _c| {
                ((a as u16 + b as u16 * 3) / 4) as u8
            });
            assert_eq!(no_prev, no_prev_scalar, "4wayV no-prev bpp={bpp}");
        }
    }

    #[test]
    fn test_filter_4way_d1_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_4way_d1_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_4way_scalar(&row, Some(&prev), bpp, |a, b, c| {
                ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
            });
            assert_eq!(avx2_filtered, scalar_filtered, "4wayD1 bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, d1_pred);
            assert_eq!(unfiltered, row, "4wayD1 roundtrip bpp={bpp}");

            let no_prev = filter_4way_d1_avx2(&row, None, bpp);
            let no_prev_scalar = filter_4way_scalar(&row, None, bpp, |a, b, c| {
                ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
            });
            assert_eq!(no_prev, no_prev_scalar, "4wayD1 no-prev bpp={bpp}");
        }
    }

    #[test]
    fn test_filter_4way_d2_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_4way_d2_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_4way_scalar(&row, Some(&prev), bpp, |a, b, c| {
                ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
            });
            assert_eq!(avx2_filtered, scalar_filtered, "4wayD2 bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, d2_pred);
            assert_eq!(unfiltered, row, "4wayD2 roundtrip bpp={bpp}");

            let no_prev = filter_4way_d2_avx2(&row, None, bpp);
            let no_prev_scalar = filter_4way_scalar(&row, None, bpp, |a, b, c| {
                ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
            });
            assert_eq!(no_prev, no_prev_scalar, "4wayD2 no-prev bpp={bpp}");
        }
    }

    /// Exhaustive check that the `_mm256_mulhi_epu16` fixed-point reciprocal
    /// (`(sum * 13108) >> 16`) used by Filter 12's AVX2 kernel is bit-exact
    /// with true integer `/5` for every reachable `sum = 2a + 2b + c` value
    /// (`0..=1275`), guarding against a subtle rounding regression if the
    /// magic constant or shift amount is ever changed.
    #[test]
    fn test_4way_d2_reciprocal_division_by_5_exact_for_all_sums() {
        for sum in 0u32..=1275 {
            let expected = sum / 5;
            let approx = (sum * 13108) >> 16;
            assert_eq!(
                expected, approx,
                "reciprocal /5 approximation diverges at sum={sum}"
            );
        }
    }

    #[test]
    fn test_filter_tr_directional_avx2_matches_scalar_and_roundtrips() {
        for &bpp in &[1usize, 3, 4] {
            let width = 257;
            let row: Vec<u8> = (0..width).map(|i| ((i * 197) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 131 + 7) % 256) as u8).collect();

            let avx2_filtered = filter_tr_directional_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_tr_directional_scalar(&row, Some(&prev), bpp);
            assert_eq!(avx2_filtered, scalar_filtered, "TR-directional bpp={bpp}");

            let unfiltered = unfilter_generic(&avx2_filtered, Some(&prev), bpp, tr_pred);
            assert_eq!(unfiltered, row, "TR-directional roundtrip bpp={bpp}");

            let no_prev = filter_tr_directional_avx2(&row, None, bpp);
            let no_prev_scalar = filter_tr_directional_scalar(&row, None, bpp);
            assert_eq!(no_prev, no_prev_scalar, "TR-directional no-prev bpp={bpp}");
        }
    }

    /// `width` smaller than `bpp + SIMD_WIDTH` (the TR kernel's inner-loop
    /// guard is `i + bpp + SIMD_WIDTH <= len`, stricter than the other
    /// filters' `i + SIMD_WIDTH <= len`) must still fall back correctly to
    /// the scalar tail without ever reading past the row (which would happen
    /// if the TR neighbor lookahead `i + bpp` overran the buffer).
    #[test]
    fn test_filter_tr_directional_avx2_small_row_no_overread() {
        for &bpp in &[1usize, 4] {
            let width = bpp + 10; // smaller than one SIMD chunk
            let row: Vec<u8> = (0..width).map(|i| ((i * 53) % 256) as u8).collect();
            let prev: Vec<u8> = (0..width).map(|i| ((i * 29 + 3) % 256) as u8).collect();

            let avx2_filtered = filter_tr_directional_avx2(&row, Some(&prev), bpp);
            let scalar_filtered = filter_tr_directional_scalar(&row, Some(&prev), bpp);
            assert_eq!(avx2_filtered, scalar_filtered, "TR small row bpp={bpp}");
        }
    }
}
