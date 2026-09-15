//! Predictors (spec section 4.4.1): 6 codes, chosen per row, reducing
//! entropy *before* ZSTD compression by predicting each sample byte from
//! already-known causal neighbors and storing only the residual.
//!
//! Scalar-only for now (SIMD fast paths deferred to 0.2, per `AGENTS.md`)
//! — this module is the scalar reference the SIMD paths will eventually
//! need to match.

use crate::error::{CodecError, Result};

/// Predictor code `0` (spec section 4.4.1): original byte kept, no
/// prediction.
pub const PREDICTOR_NONE: u8 = 0;
/// Predictor code `1`: predicts from the left neighbor (`L`), same row.
pub const PREDICTOR_SUB: u8 = 1;
/// Predictor code `2`: predicts from the neighbor above (`U`), same
/// column, previous row.
pub const PREDICTOR_UP: u8 = 2;
/// Predictor code `3`: predicts `floor((L + U) / 2)`.
pub const PREDICTOR_AVERAGE: u8 = 3;
/// Predictor code `4`: Paeth predictor (left, above, or top-left
/// diagonal), identical to PNG's.
pub const PREDICTOR_PAETH: u8 = 4;
/// Predictor code `5`: `(L + U - UL) mod 256`, no clamping.
pub const PREDICTOR_GRADIENT: u8 = 5;

/// Number of predictor codes defined by spec section 4.4.1 — the highest
/// valid code is `NUM_PREDICTORS - 1`.
pub const NUM_PREDICTORS: u8 = 6;

/// Paeth predictor (spec section 4.4.1), identical to PNG's: predicts
/// whichever of `left`/`up`/`up_left` is numerically closest to
/// `left + up - up_left`.
fn paeth_predictor(left: u8, up: u8, up_left: u8) -> u8 {
    let (a, b, c) = (left as i32, up as i32, up_left as i32);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        up_left
    }
}

/// Gradient predictor (spec section 4.4.1): `(L + U - UL) mod 256`. Uses
/// wrapping arithmetic throughout — no clamping, unlike Paeth.
fn gradient_predictor(left: u8, up: u8, up_left: u8) -> u8 {
    left.wrapping_add(up).wrapping_sub(up_left)
}

/// Dispatches to the predictor named by `code`, given the three causal
/// neighbors (`left`, `up`, `up_left` — spec section 4.4.1's `L`/`U`/`UL`,
/// each `0` at a tile edge per the zero-neighbor convention). Returns
/// `Err(InvalidPredictorCode)` for any `code >= NUM_PREDICTORS` — this is
/// the single validation point every caller (both `filter_row` and
/// `unfilter_row`) goes through, so a malformed/hostile predictor byte is
/// never silently reinterpreted.
pub(crate) fn predict(code: u8, left: u8, up: u8, up_left: u8) -> Result<u8> {
    match code {
        PREDICTOR_NONE => Ok(0),
        PREDICTOR_SUB => Ok(left),
        PREDICTOR_UP => Ok(up),
        PREDICTOR_AVERAGE => Ok(((left as u16 + up as u16) / 2) as u8),
        PREDICTOR_PAETH => Ok(paeth_predictor(left, up, up_left)),
        PREDICTOR_GRADIENT => Ok(gradient_predictor(left, up, up_left)),
        other => Err(CodecError::InvalidPredictorCode(other)),
    }
}

/// Looks up the left/up/up-left neighbor bytes for sample byte `x` within
/// `row`, applying the tile-edge zero-neighbor convention (spec section
/// 4.3.1): missing neighbors (first `bpp` bytes of a row for `left`/
/// `up_left`, or an absent `prev_row` for `up`/`up_left`) are treated as
/// zero.
fn neighbors(row: &[u8], prev_row: Option<&[u8]>, x: usize, bpp: usize) -> (u8, u8, u8) {
    let left = if x >= bpp { row[x - bpp] } else { 0 };
    let up = prev_row.map(|p| p[x]).unwrap_or(0);
    let up_left = if x >= bpp {
        prev_row.map(|p| p[x - bpp]).unwrap_or(0)
    } else {
        0
    };
    (left, up, up_left)
}

/// Applies predictor `code` to `row` (encoder direction): for each byte,
/// computes `residual = original_byte - prediction` (`u8` wrapping,
/// spec section 4.4.1). `prev_row` is the already-reconstructed previous
/// row of the same tile, or `None` for the tile's first row.
///
/// Tries a SIMD fast path first (`crate::simd::filter_row_simd`, 0.2,
/// covering all 6 codes) when this row is long enough to be worth it;
/// [`filter_row_scalar`] is always the format-defining reference and is
/// what every SIMD path is checked against by this crate's parity tests
/// (`AGENTS.md`'s "scalar-is-reference, SIMD-is-optimization"
/// architecture).
pub fn filter_row(row: &[u8], prev_row: Option<&[u8]>, code: u8, bpp: usize) -> Result<Vec<u8>> {
    if let Some(simd_result) = crate::simd::filter_row_simd(row, prev_row, code, bpp) {
        return Ok(simd_result);
    }
    filter_row_scalar(row, prev_row, code, bpp)
}

/// Scalar reference implementation of [`filter_row`] — the format
/// definition itself, never bypassed for correctness. See
/// `crate::simd`'s module doc for the scalar-is-reference architecture
/// this split exists for.
pub fn filter_row_scalar(
    row: &[u8],
    prev_row: Option<&[u8]>,
    code: u8,
    bpp: usize,
) -> Result<Vec<u8>> {
    if code == PREDICTOR_NONE {
        return Ok(row.to_vec());
    }
    let mut out = vec![0u8; row.len()];
    for x in 0..row.len() {
        let (left, up, up_left) = neighbors(row, prev_row, x, bpp);
        let pred = predict(code, left, up, up_left)?;
        out[x] = row[x].wrapping_sub(pred);
    }
    Ok(out)
}

/// Reverses predictor `code` on `filtered` (decoder direction): for each
/// byte, computes `original_byte = residual + prediction`. Bytes must be
/// reconstructed strictly left-to-right within the row, since `Sub`/
/// `Average`/`Paeth`/`Gradient` all depend on the just-reconstructed left
/// neighbor within the *output* buffer, not the still-filtered input.
///
/// Tries a SIMD fast path first (`crate::simd::unfilter_row_simd`, 0.2,
/// covering only `PREDICTOR_UP` — see that module's doc comment for why
/// the other serially-dependent codes aren't vectorized here);
/// [`unfilter_row_scalar`] is always the format-defining reference.
pub fn unfilter_row(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    code: u8,
    bpp: usize,
) -> Result<Vec<u8>> {
    if let Some(simd_result) = crate::simd::unfilter_row_simd(filtered, prev_row, code, bpp) {
        return Ok(simd_result);
    }
    unfilter_row_scalar(filtered, prev_row, code, bpp)
}

/// Scalar reference implementation of [`unfilter_row`] — the format
/// definition itself, never bypassed for correctness.
pub fn unfilter_row_scalar(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    code: u8,
    bpp: usize,
) -> Result<Vec<u8>> {
    if code == PREDICTOR_NONE {
        return Ok(filtered.to_vec());
    }
    let mut out = vec![0u8; filtered.len()];
    for x in 0..filtered.len() {
        let (left, up, up_left) = neighbors(&out, prev_row, x, bpp);
        let pred = predict(code, left, up, up_left)?;
        out[x] = filtered[x].wrapping_add(pred);
    }
    Ok(out)
}

/// Zero-order entropy (bits/byte) of `data`'s byte histogram — the
/// per-row predictor selection heuristic (spec section 4.4.1: "how an
/// encoder chooses which of the 6 predictor codes to use for a given row
/// is entirely an encoder-side decision").
fn shannon_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Chooses the lowest-Shannon-entropy predictor for one row, independent
/// of every other row (spec section 4.4.1: predictor selection is chosen
/// per row, not part of the decoding contract). Tests all
/// [`NUM_PREDICTORS`] candidates and returns the winning code plus its
/// already-filtered row; ties favor the numerically smaller code, since
/// codes are tried in ascending order and only a *strictly* better score
/// replaces the current best — in particular this means `PREDICTOR_NONE`
/// (code `0`) wins whenever every candidate scores equally (e.g. a tile's
/// very first byte, where every predictor's neighbors are zero).
///
/// Only `Entropy`-style scoring is implemented in Phase 6 — real
/// compression-test or MSAD-based heuristics are optimization work
/// deferred until `cafe-bench` shows they justify the extra encode cost
/// (`AGENTS.md`'s "every feature proves itself with a benchmark first").
pub fn choose_best_row_predictor(row: &[u8], prev_row: Option<&[u8]>, bpp: usize) -> (u8, Vec<u8>) {
    let mut best_code = PREDICTOR_NONE;
    let mut best_score = f64::INFINITY;
    let mut best_filtered = Vec::new();

    for code in 0..NUM_PREDICTORS {
        // Every code in 0..NUM_PREDICTORS is valid by construction, so
        // filter_row can never fail here.
        let filtered = filter_row(row, prev_row, code, bpp)
            .expect("code in 0..NUM_PREDICTORS is always a valid predictor");
        let score = shannon_entropy(&filtered);
        if score < best_score {
            best_score = score;
            best_code = code;
            best_filtered = filtered;
        }
    }

    (best_code, best_filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CODES: [u8; 6] = [
        PREDICTOR_NONE,
        PREDICTOR_SUB,
        PREDICTOR_UP,
        PREDICTOR_AVERAGE,
        PREDICTOR_PAETH,
        PREDICTOR_GRADIENT,
    ];

    #[test]
    fn test_paeth_predictor_matches_png_reference_cases() {
        // p = a+b-c; picks whichever of a/b/c is closest to p, ties favor a then b.
        assert_eq!(paeth_predictor(10, 20, 5), 20); // p=25, |25-20|=5 < |25-10|=15, < |25-5|=20 -> b
        assert_eq!(paeth_predictor(10, 10, 10), 10); // flat region -> a (tie)
        assert_eq!(paeth_predictor(0, 0, 0), 0);
        assert_eq!(paeth_predictor(255, 0, 0), 255); // p=255, exact match on a
    }

    #[test]
    fn test_gradient_predictor_wraps() {
        assert_eq!(gradient_predictor(0, 0, 0), 0);
        assert_eq!(gradient_predictor(255, 255, 0), 254); // wraps past 255
        assert_eq!(gradient_predictor(10, 20, 5), 25);
    }

    #[test]
    fn test_predict_rejects_invalid_code() {
        assert!(matches!(
            predict(6, 1, 2, 3),
            Err(CodecError::InvalidPredictorCode(6))
        ));
        assert!(matches!(
            predict(255, 1, 2, 3),
            Err(CodecError::InvalidPredictorCode(255))
        ));
    }

    #[test]
    fn test_filter_none_is_identity() {
        let row = vec![1, 2, 3, 4];
        assert_eq!(filter_row(&row, None, PREDICTOR_NONE, 1).unwrap(), row);
    }

    #[test]
    fn test_unfilter_none_is_identity() {
        let row = vec![1, 2, 3, 4];
        assert_eq!(unfilter_row(&row, None, PREDICTOR_NONE, 1).unwrap(), row);
    }

    #[test]
    fn test_filter_then_unfilter_roundtrip_first_row_all_codes() {
        let row: Vec<u8> = vec![10, 200, 5, 250, 128, 0, 255, 1];
        let bpp = 4;
        for &code in &ALL_CODES {
            let filtered = filter_row(&row, None, code, bpp).unwrap();
            let restored = unfilter_row(&filtered, None, code, bpp).unwrap();
            assert_eq!(restored, row, "roundtrip failed for code {code}");
        }
    }

    #[test]
    fn test_filter_then_unfilter_roundtrip_with_prev_row_all_codes() {
        let prev_row: Vec<u8> = vec![100, 150, 200, 250, 10, 20, 30, 40];
        let row: Vec<u8> = vec![10, 200, 5, 250, 128, 0, 255, 1];
        let bpp = 4;
        for &code in &ALL_CODES {
            let filtered = filter_row(&row, Some(&prev_row), code, bpp).unwrap();
            let restored = unfilter_row(&filtered, Some(&prev_row), code, bpp).unwrap();
            assert_eq!(restored, row, "roundtrip failed for code {code}");
        }
    }

    #[test]
    fn test_filter_then_unfilter_roundtrip_bpp_one_gray() {
        let prev_row: Vec<u8> = vec![5, 6, 7, 8, 9];
        let row: Vec<u8> = vec![1, 2, 3, 4, 5];
        for &code in &ALL_CODES {
            let filtered = filter_row(&row, Some(&prev_row), code, 1).unwrap();
            let restored = unfilter_row(&filtered, Some(&prev_row), code, 1).unwrap();
            assert_eq!(restored, row, "roundtrip failed for code {code}");
        }
    }

    #[test]
    fn test_sub_predictor_known_values() {
        // Sub predicts the left neighbor; residual = byte - left.
        let row = vec![10, 15, 25, 20];
        let filtered = filter_row(&row, None, PREDICTOR_SUB, 1).unwrap();
        assert_eq!(filtered, vec![10, 5, 10, 251]); // 20 - 25 wraps to 251
    }

    #[test]
    fn test_up_predictor_known_values() {
        let prev_row = vec![10, 20, 30, 40];
        let row = vec![15, 15, 35, 30];
        let filtered = filter_row(&row, Some(&prev_row), PREDICTOR_UP, 1).unwrap();
        assert_eq!(filtered, vec![5, 251, 5, 246]);
    }

    #[test]
    fn test_average_predictor_known_values() {
        // Average predicts floor((L+U)/2).
        let row = vec![10, 20];
        let prev_row = vec![0, 30];
        let filtered = filter_row(&row, Some(&prev_row), PREDICTOR_AVERAGE, 1).unwrap();
        // x=0: L=0, U=0 (prev_row[0]=0), pred=0, residual=10
        // x=1: L=10 (row[0]), U=30, pred=floor(40/2)=20, residual=0
        assert_eq!(filtered, vec![10, 0]);
    }

    #[test]
    fn test_filter_row_propagates_invalid_code_error() {
        let row = vec![1, 2, 3];
        assert!(matches!(
            filter_row(&row, None, 99, 1),
            Err(CodecError::InvalidPredictorCode(99))
        ));
    }

    #[test]
    fn test_unfilter_row_propagates_invalid_code_error() {
        let row = vec![1, 2, 3];
        assert!(matches!(
            unfilter_row(&row, None, 99, 1),
            Err(CodecError::InvalidPredictorCode(99))
        ));
    }

    #[test]
    fn test_shannon_entropy_of_empty_is_zero() {
        assert_eq!(shannon_entropy(&[]), 0.0);
    }

    #[test]
    fn test_shannon_entropy_of_constant_data_is_zero() {
        // A single repeated byte value has zero uncertainty.
        assert_eq!(shannon_entropy(&[7u8; 100]), 0.0);
    }

    #[test]
    fn test_shannon_entropy_of_uniform_bytes_is_maximal() {
        // All 256 byte values equally represented once each -> exactly 8
        // bits/byte (log2(256)).
        let data: Vec<u8> = (0u8..=255).collect();
        let entropy = shannon_entropy(&data);
        assert!((entropy - 8.0).abs() < 1e-9, "entropy was {entropy}");
    }

    #[test]
    fn test_shannon_entropy_higher_for_more_diverse_data() {
        let low = shannon_entropy(&[1u8; 8]);
        let high = shannon_entropy(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(high > low);
    }

    #[test]
    fn test_choose_best_row_predictor_first_row_ties_favor_none() {
        // First row, no prev_row: every predictor's neighbors at x=0 are
        // zero, and for a flat row every code's residual stream has the
        // same entropy as the row itself once left=0 everywhere (row is
        // constant) -- ties should favor the smallest code, PREDICTOR_NONE.
        let row = vec![5u8; 6];
        let (code, filtered) = choose_best_row_predictor(&row, None, 1);
        assert_eq!(code, PREDICTOR_NONE);
        assert_eq!(filtered, row);
    }

    #[test]
    fn test_choose_best_row_predictor_picks_sub_for_ramp() {
        // A linear ramp is predicted perfectly by Sub once bpp=1 (each byte
        // minus its immediate left neighbor is a constant step), which
        // collapses to a lower-entropy constant residual stream than any
        // other candidate -- Sub should win.
        let row: Vec<u8> = (0u8..16).map(|i| i * 3).collect();
        let (code, filtered) = choose_best_row_predictor(&row, None, 1);
        assert_eq!(code, PREDICTOR_SUB);
        let expected = filter_row(&row, None, PREDICTOR_SUB, 1).unwrap();
        assert_eq!(filtered, expected);
    }

    #[test]
    fn test_choose_best_row_predictor_picks_up_for_row_matching_prev() {
        // A row identical to prev_row is predicted perfectly by Up (every
        // residual is zero), which is unbeatable entropy (0 bits/byte).
        // Non-arithmetic values (not a ramp) so Sub can't also tie at zero
        // entropy -- Up must win outright, and Up (code 2) sorts before
        // Average/Paeth/Gradient regardless.
        let prev_row = vec![10, 200, 7, 40, 250];
        let row = prev_row.clone();
        let (code, filtered) = choose_best_row_predictor(&row, Some(&prev_row), 1);
        assert_eq!(code, PREDICTOR_UP);
        assert_eq!(filtered, vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_choose_best_row_predictor_output_is_always_reversible() {
        // Whatever code choose_best_row_predictor selects, unfilter_row
        // must reconstruct the original row exactly -- this is the
        // property the encoder actually depends on.
        let prev_row = vec![100, 150, 200, 250, 10, 20, 30, 40];
        let row = vec![10, 200, 5, 250, 128, 0, 255, 1];
        let bpp = 4;
        let (code, filtered) = choose_best_row_predictor(&row, Some(&prev_row), bpp);
        let restored = unfilter_row(&filtered, Some(&prev_row), code, bpp).unwrap();
        assert_eq!(restored, row);
    }

    #[test]
    fn test_choose_best_row_predictor_never_scores_worse_than_none() {
        // By construction (PREDICTOR_NONE is one of the candidates tried),
        // the chosen code's entropy must be <= None's entropy for varied,
        // non-adversarial data.
        let row: Vec<u8> = (0..32).map(|i| ((i * 17 + 3) % 251) as u8).collect();
        let (_, filtered) = choose_best_row_predictor(&row, None, 2);
        let none_entropy = shannon_entropy(&row);
        let chosen_entropy = shannon_entropy(&filtered);
        assert!(chosen_entropy <= none_entropy + 1e-9);
    }
}
