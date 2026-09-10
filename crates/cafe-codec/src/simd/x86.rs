//! AVX2 (x86_64) fast paths for the predictor filters. Every function
//! here is `unsafe` and `#[target_feature(enable = "avx2")]` — callers
//! (see `super::dispatch_*`) must confirm `is_x86_feature_detected!
//! ("avx2")` before calling any of these, since AVX2 isn't guaranteed
//! present on every x86_64 CPU (unlike the x86_64 baseline SSE2).
//!
//! **Math identities used to avoid widening where possible** (all proven
//! correct against the scalar reference by this crate's parity tests):
//!
//! - `Sub`/`Up`: no math trick needed — the residual is a single 8-bit
//!   wrapping subtraction (`row[x] - row[x-bpp]` / `row[x] - prev[x]`).
//! - `Gradient`: `pred = (a + b - c) mod 256` is *already* a pure
//!   wrapping-arithmetic formula with no clamping (unlike Paeth) — so
//!   `residual = row - a - b + c (mod 256)` needs only 8-bit wrapping
//!   add/sub, reordered freely since wrapping arithmetic mod 2^8 is an
//!   abelian group.
//! - `Average`: `floor((a+b)/2) = (a & b) + ((a ^ b) >> 1)` (a classic
//!   bit-trick for the floor-average of two unsigned integers, avoiding
//!   the u16 widening the naive `(a+b)/2` formula would need). The
//!   per-byte `>> 1` itself is done via a 16-bit-lane shift plus a mask
//!   (`0x7F`), the standard trick for a byte-wise right-shift-by-1 using
//!   only 16/32/64-bit SIMD shift instructions (which don't have a
//!   per-byte lane variant): `_mm256_srli_epi16(x, 1)` shifts entire
//!   16-bit lanes, letting the low bit of each high byte leak into the
//!   top bit of the byte below it, but masking with `0x7F` both removes
//!   that leaked bit *and* matches the correct result, since a valid
//!   byte-wise `>> 1` output is always `<= 0x7F` anyway.
//! - `Paeth`: no integer trick avoids the actual `|p - a|`/`|p - b|`/
//!   `|p - c|` comparisons Paeth is defined by, so this one genuinely
//!   widens each 16-byte half-lane to `i16` (`p = a + b - c` ranges from
//!   -255 to 510, overflowing `u8`/`i8`), computes the three distances
//!   and a branchless nested `min`-style select in 16-bit lanes, then
//!   narrows back down to `u8` before the final 8-bit wrapping subtract.

use std::arch::x86_64::*;

const LANES: usize = 32;

/// Byte-wise arithmetic right shift by 1 bit, using the standard
/// 16-bit-lane-shift-plus-mask trick (see this module's doc comment).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn shr1_epi8(x: __m256i) -> __m256i {
    let shifted = _mm256_srli_epi16(x, 1);
    _mm256_and_si256(shifted, _mm256_set1_epi8(0x7F))
}

/// `floor((a + b) / 2)` per byte lane, both unsigned. See this module's
/// doc comment for the bit-trick used.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn avg_floor_epu8(a: __m256i, b: __m256i) -> __m256i {
    let and_ab = _mm256_and_si256(a, b);
    let xor_ab = _mm256_xor_si256(a, b);
    _mm256_add_epi8(and_ab, shr1_epi8(xor_ab))
}

/// `PREDICTOR_SUB` (encoder): `residual[x] = row[x] - row[x - bpp]` for
/// `x >= bpp`; the first `bpp` bytes (no left neighbor) are handled
/// scalar (`residual = row[x] - 0 = row[x]`, matching `predictor::
/// neighbors`' zero-neighbor convention).
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter_sub_avx2(row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];
    out[..bpp.min(len)].copy_from_slice(&row[..bpp.min(len)]);

    let mut x = bpp;
    while x + LANES <= len {
        let cur = _mm256_loadu_si256(row.as_ptr().add(x) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(x - bpp) as *const __m256i);
        let res = _mm256_sub_epi8(cur, left);
        _mm256_storeu_si256(out.as_mut_ptr().add(x) as *mut __m256i, res);
        x += LANES;
    }
    while x < len {
        out[x] = row[x].wrapping_sub(row[x - bpp]);
        x += 1;
    }
    out
}

/// `PREDICTOR_UP` (encoder): `residual[x] = row[x] - prev_row[x]` for
/// every `x` — no `bpp` dependency at all, since `Up` only ever reads
/// the row directly above, never a same-row neighbor.
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")` and
/// that `row.len() == prev_row.len()`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter_up_avx2(row: &[u8], prev_row: &[u8]) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    let mut x = 0;
    while x + LANES <= len {
        let cur = _mm256_loadu_si256(row.as_ptr().add(x) as *const __m256i);
        let up = _mm256_loadu_si256(prev_row.as_ptr().add(x) as *const __m256i);
        let res = _mm256_sub_epi8(cur, up);
        _mm256_storeu_si256(out.as_mut_ptr().add(x) as *mut __m256i, res);
        x += LANES;
    }
    while x < len {
        out[x] = row[x].wrapping_sub(prev_row[x]);
        x += 1;
    }
    out
}

/// `PREDICTOR_AVERAGE` (encoder): `residual[x] = row[x] -
/// floor((left + up) / 2)`, `left = 0` for `x < bpp`.
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")` and
/// that `row.len() == prev_row.len()`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter_average_avx2(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    // First bpp bytes: left = 0, so pred = floor(up / 2) = up >> 1
    // (unsigned, no leaked-bit concern for a lone value, but reuse the
    // scalar formula directly for simplicity and to match the reference
    // exactly).
    for x in 0..bpp.min(len) {
        let up = prev_row[x];
        let pred = (up as u16 / 2) as u8;
        out[x] = row[x].wrapping_sub(pred);
    }

    let mut x = bpp;
    while x + LANES <= len {
        let cur = _mm256_loadu_si256(row.as_ptr().add(x) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(x - bpp) as *const __m256i);
        let up = _mm256_loadu_si256(prev_row.as_ptr().add(x) as *const __m256i);
        let pred = avg_floor_epu8(left, up);
        let res = _mm256_sub_epi8(cur, pred);
        _mm256_storeu_si256(out.as_mut_ptr().add(x) as *mut __m256i, res);
        x += LANES;
    }
    while x < len {
        let left = row[x - bpp];
        let up = prev_row[x];
        let pred = ((left as u16 + up as u16) / 2) as u8;
        out[x] = row[x].wrapping_sub(pred);
        x += 1;
    }
    out
}

/// `PREDICTOR_GRADIENT` (encoder): `residual[x] = row[x] - ((left + up -
/// up_left) mod 256)`, pure wrapping 8-bit arithmetic throughout (see
/// this module's doc comment) — `left`/`up_left` are `0` for `x < bpp`.
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")` and
/// that `row.len() == prev_row.len()`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter_gradient_avx2(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    for x in 0..bpp.min(len) {
        // left = up_left = 0, pred = up.
        out[x] = row[x].wrapping_sub(prev_row[x]);
    }

    let mut x = bpp;
    while x + LANES <= len {
        let cur = _mm256_loadu_si256(row.as_ptr().add(x) as *const __m256i);
        let left = _mm256_loadu_si256(row.as_ptr().add(x - bpp) as *const __m256i);
        let up = _mm256_loadu_si256(prev_row.as_ptr().add(x) as *const __m256i);
        let up_left = _mm256_loadu_si256(prev_row.as_ptr().add(x - bpp) as *const __m256i);
        // residual = cur - left - up + up_left (mod 256).
        let res = _mm256_add_epi8(_mm256_sub_epi8(_mm256_sub_epi8(cur, left), up), up_left);
        _mm256_storeu_si256(out.as_mut_ptr().add(x) as *mut __m256i, res);
        x += LANES;
    }
    while x < len {
        let left = row[x - bpp];
        let up = prev_row[x];
        let up_left = prev_row[x - bpp];
        let pred = left.wrapping_add(up).wrapping_sub(up_left);
        out[x] = row[x].wrapping_sub(pred);
        x += 1;
    }
    out
}

/// Widens the low 16 bytes of `v` to sixteen `i16` lanes (zero-extended,
/// since inputs are `u8`).
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn widen_lo_epu8_epi16(v: __m128i) -> __m256i {
    _mm256_cvtepu8_epi16(v)
}

/// `PREDICTOR_PAETH` (encoder), 16 bytes (one 128-bit half) per
/// iteration: widens `left`/`up`/`up_left` to `i16` (since `p = a + b -
/// c` can be negative or exceed 255), computes `p`, the three absolute
/// distances, and a branchless select matching `predictor::
/// paeth_predictor`'s tie-breaking rule (`pa <= pb && pa <= pc` -> left;
/// else `pb <= pc` -> up; else up_left), then narrows the selected
/// prediction back to `u8` before the final wrapping subtract.
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")` and
/// that `row.len() == prev_row.len()`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter_paeth_avx2(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    const PAETH_LANES: usize = 16;
    let len = row.len();
    let mut out = vec![0u8; len];

    for x in 0..bpp.min(len) {
        // left = up_left = 0; Paeth of (0, up, 0) always selects `up`
        // (p=up, pa=up, pb=0, pc=up; pb<=pc so `up` wins unless up==0,
        // in which case pa==pb==0 and `left`=0 ties first, same value).
        out[x] = row[x].wrapping_sub(prev_row[x]);
    }

    let mut x = bpp;
    while x + PAETH_LANES <= len {
        let left8 = _mm_loadu_si128(row.as_ptr().add(x - bpp) as *const __m128i);
        let up8 = _mm_loadu_si128(prev_row.as_ptr().add(x) as *const __m128i);
        let up_left8 = _mm_loadu_si128(prev_row.as_ptr().add(x - bpp) as *const __m128i);
        let cur8 = _mm_loadu_si128(row.as_ptr().add(x) as *const __m128i);

        let a = widen_lo_epu8_epi16(left8);
        let b = widen_lo_epu8_epi16(up8);
        let c = widen_lo_epu8_epi16(up_left8);

        let p = _mm256_sub_epi16(_mm256_add_epi16(a, b), c);
        let pa = _mm256_abs_epi16(_mm256_sub_epi16(p, a));
        let pb = _mm256_abs_epi16(_mm256_sub_epi16(p, b));
        let pc = _mm256_abs_epi16(_mm256_sub_epi16(p, c));

        // cond1 = pa <= pb && pa <= pc  (select `a`). Expressed as
        // !(pa > pb) && !(pa > pc), since SIMD only has a
        // greater-than compare, and negating it (rather than swapping
        // operands to fake `<`) correctly keeps the `pa == pb` tie
        // resolved in favor of `a`, matching the scalar reference.
        let pa_gt_pb = _mm256_cmpgt_epi16(pa, pb);
        let pa_gt_pc = _mm256_cmpgt_epi16(pa, pc);
        let cond1 = _mm256_andnot_si256(_mm256_or_si256(pa_gt_pb, pa_gt_pc), _mm256_set1_epi16(-1));

        // cond2 = pb <= pc  (select `b`, only consulted when cond1 is
        // false), same negated-greater-than trick.
        let pb_gt_pc = _mm256_cmpgt_epi16(pb, pc);
        let cond2 = _mm256_andnot_si256(pb_gt_pc, _mm256_set1_epi16(-1));

        // Select: cond1 ? a : (cond2 ? b : c)
        let b_or_c = _mm256_blendv_epi8(c, b, cond2);
        let pred16 = _mm256_blendv_epi8(b_or_c, a, cond1);

        // Narrow the 16 i16 predictions back to 16 u8 bytes.
        let pred16_lo = _mm256_castsi256_si128(pred16);
        let pred16_hi = _mm256_extracti128_si256(pred16, 1);
        let pred8 = _mm_packus_epi16(pred16_lo, pred16_hi);

        let res = _mm_sub_epi8(cur8, pred8);
        _mm_storeu_si128(out.as_mut_ptr().add(x) as *mut __m128i, res);

        x += PAETH_LANES;
    }
    while x < len {
        let left = row[x - bpp];
        let up = prev_row[x];
        let up_left = prev_row[x - bpp];
        let pred = crate::predictor::predict(4, left, up, up_left)
            .expect("code 4 (Paeth) is always valid");
        out[x] = row[x].wrapping_sub(pred);
        x += 1;
    }
    out
}

/// `PREDICTOR_UP` (decoder direction): `out[x] = filtered[x] +
/// prev_row[x]` for every `x` — no serial dependency, since `Up` never
/// reads the just-reconstructed left neighbor.
///
/// # Safety
/// Caller must have confirmed `is_x86_feature_detected!("avx2")` and
/// that `filtered.len() == prev_row.len()`.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn unfilter_up_avx2(filtered: &[u8], prev_row: &[u8]) -> Vec<u8> {
    let len = filtered.len();
    let mut out = vec![0u8; len];

    let mut x = 0;
    while x + LANES <= len {
        let res = _mm256_loadu_si256(filtered.as_ptr().add(x) as *const __m256i);
        let up = _mm256_loadu_si256(prev_row.as_ptr().add(x) as *const __m256i);
        let orig = _mm256_add_epi8(res, up);
        _mm256_storeu_si256(out.as_mut_ptr().add(x) as *mut __m256i, orig);
        x += LANES;
    }
    while x < len {
        out[x] = filtered[x].wrapping_add(prev_row[x]);
        x += 1;
    }
    out
}
