//! Predictive filter (section 4.3.1)
//!
//! Implementation of the 16 predictors (0-15), automatic selection via Shannon
//! entropy or real compression testing, application and reversal of filters.
//! Filters 14 (TR-aware Directional, WebP Predictor 10) and 15 (Adaptive
//! Weighted, inspired by JPEG-XL) were added in v1.1.

use crate::codec::compress_with_fallback;
use crate::constants::*;
use crate::error::{CafeError, Result};
use crate::types::FilterHeuristic;

#[cfg(feature = "simd")]
use crate::simd::{
    filter_average_avx2, filter_sub_avx2, filter_up_avx2, unfilter_average_avx2, unfilter_sub_avx2,
    unfilter_up_avx2,
};

// ============================================================================
// Adaptive Entropy Support Structures (v1.1)
// ============================================================================

/// Detected block type based on local variance characteristics.
/// Used by AdaptiveEntropy heuristic to select appropriate filters.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockType {
    /// Very smooth content: constant values, large flat regions, gradients (variance < 10)
    Smooth,
    /// Natural photo content: typical photographs (variance 10-50)
    Natural,
    /// High frequency content: noise, fine textures, details (variance > 50 with many high-variance regions)
    HighFreq,
    /// Mixed content: doesn't clearly fit one category
    Mixed,
}

/// Block statistics computed during content analysis.
/// Used to determine the BlockType and guide filter selection.
#[derive(Debug, Clone, Copy)]
struct BlockStats {
    avg_variance: f64,
    high_variance_regions: usize,
    total_regions: usize,
}

impl BlockStats {
    /// Detects the block type based on variance distribution.
    fn detect_block_type(&self) -> BlockType {
        match (self.avg_variance, self.high_variance_regions) {
            (v, _) if v < 10.0 => BlockType::Smooth,
            (v, _) if v < 50.0 => BlockType::Natural,
            (v, h) if v >= 50.0 && h > self.total_regions / 3 => BlockType::HighFreq,
            _ => BlockType::Mixed,
        }
    }
}

// Predictors (all receive a=left, b=up, c=upper-left diagonal)

/// Paeth predictor
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// MED (Median Edge Detector) — same predictor as JPEG-LS/FFV1.
fn med_predictor(a: u8, b: u8, c: u8) -> u8 {
    if c >= a.max(b) {
        a.min(b)
    } else if c <= a.min(b) {
        a.max(b)
    } else {
        // Wrapping: same modulo-256 arithmetic used throughout the rest of the filter.
        a.wrapping_add(b).wrapping_sub(c)
    }
}

/// Gradient/Plane — one of the 7 classic JPEG Lossless modes.
/// No clamping (unlike Paeth/MED); Rust's wrapping arithmetic already
/// safely absorbs any overshoot, without risk of overflow in debug
/// (that's why we use `wrapping_*` explicitly).
fn gradient_predictor(a: u8, b: u8, c: u8) -> u8 {
    a.wrapping_add(b).wrapping_sub(c)
}

/// Simple Median Filter — simple median of the 3 neighbors (left, up, upper-left diagonal).
/// A fast alternative to MED, without a predicated branch.
fn simple_median_predictor(a: u8, b: u8, c: u8) -> u8 {
    // Sort 3 elements
    let mut vals = [a, b, c];
    vals.sort_unstable();
    vals[1] // Median is the middle value
}

/// F_4WAY_H: Horizontal emphasis — good for horizontal lines/edges
fn four_way_horizontal_predictor(a: u8, b: u8, _c: u8) -> u8 {
    // More weight on left, less on up

    ((a as u16 * 3 + b as u16) / 4) as u8
}

/// F_4WAY_V: Vertical emphasis — good for vertical lines/edges
fn four_way_vertical_predictor(a: u8, b: u8, _c: u8) -> u8 {
    // More weight on up, less on left

    ((a as u16 + b as u16 * 3) / 4) as u8
}

/// F_4WAY_D1: Diagonal \ emphasis — good for diagonal patterns
fn four_way_diagonal1_predictor(a: u8, b: u8, c: u8) -> u8 {
    // More weight on the upper-left diagonal

    ((a as u16 + b as u16 + c as u16 * 2) / 4) as u8
}

/// F_4WAY_D2: Diagonal / emphasis — good for inverse diagonal patterns
fn four_way_diagonal2_predictor(a: u8, b: u8, c: u8) -> u8 {
    // Symmetric distribution with emphasis on a and b

    ((a as u16 * 2 + b as u16 * 2 + c as u16) / 5) as u8
}

/// F_CONTEXT: Context-Based Predictor (v1.0)
/// Detects local orientation via gradient analysis and picks the appropriate filter.
/// Very good for graphics, screenshots, icons with sharp edges.
fn context_based_predictor(a: u8, b: u8, c: u8) -> u8 {
    let dh = (a as i16 - c as i16).abs(); // Horizontal difference
    let dv = (b as i16 - c as i16).abs(); // Vertical difference

    if dh > dv {
        // Vertical edge more pronounced → use left (Sub)
        a
    } else if dv > dh {
        // Horizontal edge more pronounced → use up (Up)
        b
    } else {
        // Homogeneous → use Average
        ((a as u16 + b as u16) / 2) as u8
    }
}

/// 2nd Order — linear extrapolation using second-order differences
fn second_order_predictor(a: u8, b: u8, ll: u8, uu: u8) -> u8 {
    let pred_h = 2i16 * a as i16 - ll as i16;
    let pred_v = 2i16 * b as i16 - uu as i16;
    let pred = (pred_h + pred_v) / 2;
    pred.clamp(0, 255) as u8
}

/// Truncated average of two bytes (WebP `Average2`): `(a + b) >> 1`, without any risk of
/// overflow (arithmetic in u16).
fn average2(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16) >> 1) as u8
}

/// F_TR_DIRECTIONAL (14): TR directional predictor — bilinear average of the 4 neighbors
/// (W, N, NW, TR), equivalent to WebP's lossless "Predictor 10". It is the only
/// predictor in the format that consumes the top-right neighbor (TR), capturing
/// diagonal `/` gradients the others cannot see.
///
/// `d` is the top-right neighbor (TR), absent on the right edge (treated as 0).
fn tr_directional_predictor(a: u8, b: u8, c: u8, d: u8) -> u8 {
    average2(average2(a, c), average2(b, d))
}

// --- F_WEIGHTED (15): Adaptive weighted predictor (inspired by JPEG-XL) ---
//
// Combines W, N, NW and NE with fixed-point weights that reward the neighbor
// closest to the observed actual value (±1 adaptation per byte). The state
// persists across the whole block and evolves in a scan order identical in the
// encoder and decoder — therefore deterministic, at a cost of 0 extra bits.
//
// Weight: integer in [0, 64] (WEIGHTED_WEIGHT_MAX), initially 8,
// resulting in a simple average of the 4 neighbors (a good starting point).

/// Cap for each weight.
const WEIGHTED_WEIGHT_MAX: u32 = 64;
/// Initial weight (simple average of the 4 neighbors).
const WEIGHTED_INIT_WEIGHT: u32 = 8;

/// Adaptive state of F_WEIGHTED: weight per neighbor (order: W, N, NW, NE).
/// Reset per block, persistent across rows.
#[derive(Clone, Copy)]
struct WeightedState {
    weight: [u32; 4],
}

impl Default for WeightedState {
    fn default() -> Self {
        WeightedState {
            weight: [WEIGHTED_INIT_WEIGHT; 4],
        }
    }
}

/// Predicts the current byte via the weighted average (normalized by the sum of
/// the weights) of W, N, NW and NE (F_WEIGHTED). With uniform weights it becomes
/// the simple average of the 4 neighbors; as one neighbor dominates, the prediction tends toward it.
fn weighted_predict(state: &WeightedState, a: u8, b: u8, c: u8, d: u8) -> u8 {
    let w = &state.weight;
    let sum_w = w[0] + w[1] + w[2] + w[3];
    if sum_w == 0 {
        return 0;
    }
    let acc = w[0] * a as u32 + w[1] * b as u32 + w[2] * c as u32 + w[3] * d as u32;
    let pred = (acc + sum_w / 2) / sum_w;
    pred.min(255) as u8
}

/// Updates the state after learning the actual value `actual` — run identically
/// in the encoder and the decoder, guaranteeing determinism. Rewards the
/// neighbor with error ≤ average and penalizes the others (the weights converge
/// to the locally dominant predictor).
fn weighted_update(state: &mut WeightedState, a: u8, b: u8, c: u8, d: u8, actual: u8) {
    let errs = [
        (actual as i32 - a as i32).unsigned_abs(),
        (actual as i32 - b as i32).unsigned_abs(),
        (actual as i32 - c as i32).unsigned_abs(),
        (actual as i32 - d as i32).unsigned_abs(),
    ];
    let avg = errs.iter().sum::<u32>() / 4;
    for (w, e) in state.weight.iter_mut().zip(errs) {
        if e <= avg {
            *w = (*w + 1).min(WEIGHTED_WEIGHT_MAX);
        } else if *w > 0 {
            *w -= 1;
        }
    }
}

/// Dispatcher that chooses the predictor based on the filter type.
/// F_WEIGHTED does not go through here (requires in-flight state, handled separately).
fn predict(ftype: u8, a: u8, b: u8, c: u8, d: u8, ll: u8, uu: u8) -> u8 {
    match ftype {
        F_NONE => 0,
        F_SUB => a,
        F_UP => b,
        F_AVERAGE => ((a as u16 + b as u16) / 2) as u8,
        F_PAETH => paeth_predictor(a, b, c),
        F_MED => med_predictor(a, b, c),
        F_GRADIENT => gradient_predictor(a, b, c),
        F_SMEDIAN => simple_median_predictor(a, b, c),
        F_2NDORDER => second_order_predictor(a, b, ll, uu),
        F_4WAY_H => four_way_horizontal_predictor(a, b, c),
        F_4WAY_V => four_way_vertical_predictor(a, b, c),
        F_4WAY_D1 => four_way_diagonal1_predictor(a, b, c),
        F_4WAY_D2 => four_way_diagonal2_predictor(a, b, c),
        F_CONTEXT => context_based_predictor(a, b, c),
        F_TR_DIRECTIONAL => tr_directional_predictor(a, b, c, d),
        // F_WEIGHTED never reaches here (requires in-flight state, handled separately).
        _ => 0,
    }
}

/// Applies a filter to a row, producing the residuals (section 4.3.1).
/// `prev_row` is the immediately preceding row (for `U`/`UU`); `prev_prev_row`
/// is the row two positions back (needed only for `F_2NDORDER`, `UU`).
/// F_WEIGHTED does not go through here (requires in-flight state, see `filter_block`).
///
/// # SIMD Optimizations (v1.1+)
/// Uses AVX2 for Filters 1 (Sub), 2 (Up), and 3 (Average) when available.
/// Other filters use scalar fallback.
fn filter_row(
    row: &[u8],
    prev_row: Option<&[u8]>,
    prev_prev_row: Option<&[u8]>,
    ftype: u8,
    bpp: usize,
) -> Vec<u8> {
    // SIMD fast paths for the most common and vectorizable filters
    #[cfg(feature = "simd")]
    {
        match ftype {
            F_NONE => return row.to_vec(),
            F_SUB => return filter_sub_avx2(row, bpp),
            F_UP => return filter_up_avx2(row, prev_row),
            F_AVERAGE => return filter_average_avx2(row, prev_row, bpp),
            _ => {} // Fall through to scalar for other filters
        }
    }

    // Scalar fallback for all filters or when SIMD is disabled
    let mut out = vec![0u8; row.len()];
    for x in 0..row.len() {
        let a = if x >= bpp { row[x - bpp] } else { 0 };
        let b = prev_row.map(|p| p[x]).unwrap_or(0);
        let c = if x >= bpp {
            prev_row.map(|p| p[x - bpp]).unwrap_or(0)
        } else {
            0
        };
        let d = if x + bpp < row.len() {
            prev_row.map(|p| p[x + bpp]).unwrap_or(0)
        } else {
            0
        };
        let ll = if x >= 2 * bpp { row[x - 2 * bpp] } else { 0 };
        let uu = prev_prev_row.map(|p| p[x]).unwrap_or(0);
        let pred = predict(ftype, a, b, c, d, ll, uu);
        out[x] = row[x].wrapping_sub(pred);
    }
    out
}

/// Reverses a filter, reconstructing the original row from the residuals.
/// F_WEIGHTED does not go through here (see `undo_predictive_filter`).
///
/// # SIMD Optimizations (v1.1+)
/// Uses AVX2 for Filters 1 (Sub), 2 (Up), and 3 (Average) when available.
/// Other filters use scalar fallback.
fn unfilter_row(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    prev_prev_row: Option<&[u8]>,
    ftype: u8,
    bpp: usize,
) -> Vec<u8> {
    // SIMD fast paths for the most common and vectorizable filters
    #[cfg(feature = "simd")]
    {
        match ftype {
            F_NONE => return filtered.to_vec(),
            F_SUB => return unfilter_sub_avx2(filtered, bpp),
            F_UP => return unfilter_up_avx2(filtered, prev_row),
            F_AVERAGE => return unfilter_average_avx2(filtered, prev_row, bpp),
            _ => {} // Fall through to scalar for other filters
        }
    }

    // Scalar fallback for all filters or when SIMD is disabled
    let mut out = vec![0u8; filtered.len()];
    for x in 0..filtered.len() {
        let a = if x >= bpp { out[x - bpp] } else { 0 };
        let b = prev_row.map(|p| p[x]).unwrap_or(0);
        let c = if x >= bpp {
            prev_row.map(|p| p[x - bpp]).unwrap_or(0)
        } else {
            0
        };
        let d = if x + bpp < filtered.len() {
            prev_row.map(|p| p[x + bpp]).unwrap_or(0)
        } else {
            0
        };
        let ll = if x >= 2 * bpp { out[x - 2 * bpp] } else { 0 };
        let uu = prev_prev_row.map(|p| p[x]).unwrap_or(0);
        let pred = predict(ftype, a, b, c, d, ll, uu);
        out[x] = filtered[x].wrapping_add(pred);
    }
    out
}

/// Applies F_WEIGHTED to a whole row with adaptive state shared
/// across the rows of the block (encoder). Each byte predicts with the current
/// weights and then updates the state with the original value.
fn filter_row_weighted(
    row: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
    state: &mut WeightedState,
) -> Vec<u8> {
    let mut out = vec![0u8; row.len()];
    for x in 0..row.len() {
        let a = if x >= bpp { row[x - bpp] } else { 0 };
        let b = prev_row.map(|p| p[x]).unwrap_or(0);
        let c = if x >= bpp {
            prev_row.map(|p| p[x - bpp]).unwrap_or(0)
        } else {
            0
        };
        let d = if x + bpp < row.len() {
            prev_row.map(|p| p[x + bpp]).unwrap_or(0)
        } else {
            0
        };
        let pred = weighted_predict(state, a, b, c, d);
        out[x] = row[x].wrapping_sub(pred);
        weighted_update(state, a, b, c, d, row[x]);
    }
    out
}

/// Reverses F_WEIGHTED row by row (decoder): reconstructs the actual value and
/// updates the state with it, in the same order as the encoder.
fn unfilter_row_weighted(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    bpp: usize,
    state: &mut WeightedState,
) -> Vec<u8> {
    let mut out = vec![0u8; filtered.len()];
    for x in 0..filtered.len() {
        let a = if x >= bpp { out[x - bpp] } else { 0 };
        let b = prev_row.map(|p| p[x]).unwrap_or(0);
        let c = if x >= bpp {
            prev_row.map(|p| p[x - bpp]).unwrap_or(0)
        } else {
            0
        };
        let d = if x + bpp < filtered.len() {
            prev_row.map(|p| p[x + bpp]).unwrap_or(0)
        } else {
            0
        };
        let pred = weighted_predict(state, a, b, c, d);
        let actual = filtered[x].wrapping_add(pred);
        out[x] = actual;
        weighted_update(state, a, b, c, d, actual);
    }
    out
}

/// Zero-order entropy (bits/byte) of the byte histogram — filter selection
/// heuristic, capturing pattern repetition, not just magnitude.
pub(crate) fn shannon_entropy(data: &[u8]) -> f64 {
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

/// Computes the local complexity of a tile (section 4.3.1 extended).
/// Complexity analysis: the higher the entropy, the more complex the tile.
pub(crate) fn analyze_tile_complexity(tile_raw: &[u8]) -> f64 {
    shannon_entropy(tile_raw)
}

/// Quick Filter Pruning (v1.1): tests all 16 filters with MSAD (very fast),
/// collects scores and sorted rankings, then applies Shannon entropy only to
/// the top 8 candidates to make a final decision.
///
/// Rationale: MSAD filtering removes obvious bad candidates quickly, while
/// entropy ensures we don't miss subtle patterns in the top tier.
///
/// Cost: O(24n) = 16 MSAD + 8 entropy operations
/// Quality: ~90% (slightly better than pure Entropy, much faster than CompressionTest)
fn choose_best_block_filter_quick_prune(
    tile_raw: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
) -> (u8, Vec<u8>) {
    // Phase 1: Quick MSAD test on all 16 filters
    let mut candidates = Vec::new();
    for ftype in 0..NUM_FILTERS {
        let filtered = filter_block(tile_raw, tile_height, bytes_per_row, bpp, ftype);
        let msad: u64 = filtered.iter().map(|&b| u64::from(b)).sum();
        candidates.push((ftype, msad, filtered));
    }

    // Phase 2: Sort by MSAD and keep top 8
    candidates.sort_by_key(|x| x.1);
    let top_8_count = 8.min(candidates.len());
    let top_candidates = &candidates[0..top_8_count];

    // Phase 3: Apply Shannon entropy to top 8
    let mut best_ftype = top_candidates[0].0;
    let mut best_entropy = f64::INFINITY;

    for (ftype, _, filtered) in top_candidates {
        let entropy = filtered
            .chunks_exact(bytes_per_row)
            .map(shannon_entropy)
            .sum::<f64>();

        if entropy < best_entropy {
            best_entropy = entropy;
            best_ftype = *ftype;
        }
    }

    // Return the winning filter and its filtered data
    let (_, _, filtered) = candidates
        .iter()
        .find(|(ft, _, _)| *ft == best_ftype)
        .unwrap();

    (best_ftype, filtered.clone())
}

/// Analyzes block variance distribution to detect content type.
/// Divides the block into regions and computes local variance to classify
/// the block as Smooth, Natural, HighFreq, or Mixed.
///
/// Used by AdaptiveEntropy to select appropriate filter candidates.
fn analyze_block_type(tile_raw: &[u8], tile_height: usize, bytes_per_row: usize) -> BlockStats {
    // Divide block into 4x4 regions (or as many as fit)
    let region_height = (tile_height / 4).max(1);
    let region_width = (bytes_per_row / 4).max(1);
    let mut total_variance = 0.0;
    let mut high_variance_count = 0;
    let mut region_count = 0;

    for ry in (0..tile_height).step_by(region_height) {
        for rx in (0..bytes_per_row).step_by(region_width) {
            let y_end = (ry + region_height).min(tile_height);
            let x_end = (rx + region_width).min(bytes_per_row);

            // Calculate variance for this region
            let mut region_sum = 0.0;
            let mut region_sum_sq = 0.0;
            let mut region_count_local = 0;

            for y in ry..y_end {
                for x in rx..x_end {
                    let idx = y * bytes_per_row + x;
                    if idx < tile_raw.len() {
                        let val = tile_raw[idx] as f64;
                        region_sum += val;
                        region_sum_sq += val * val;
                        region_count_local += 1;
                    }
                }
            }

            if region_count_local > 0 {
                let mean = region_sum / region_count_local as f64;
                let variance = (region_sum_sq / region_count_local as f64) - (mean * mean);
                total_variance += variance;
                if variance > 100.0 {
                    high_variance_count += 1;
                }
                region_count += 1;
            }
        }
    }

    let avg_variance = if region_count > 0 {
        total_variance / region_count as f64
    } else {
        0.0
    };

    BlockStats {
        avg_variance,
        high_variance_regions: high_variance_count,
        total_regions: region_count,
    }
}

/// Adaptive Entropy (v1.1): analyzes block type and applies heuristic tailored
/// to the detected content. Smooth blocks test fewer filters, natural photos
/// test all, high-frequency content prefers adaptive filters.
///
/// Cost: O(n) analysis + adaptive heuristic
/// Quality: ~95% (better on natural photos, +2-3% improvement)
fn choose_best_block_filter_adaptive(
    tile_raw: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
) -> (u8, Vec<u8>) {
    // Analyze block type via variance distribution
    let stats = analyze_block_type(tile_raw, tile_height, bytes_per_row);
    let block_type = stats.detect_block_type();

    // Select subset of filters to test based on block type
    let candidates_to_test: Vec<u8> = match block_type {
        BlockType::Smooth => {
            // For smooth blocks, simple filters work well and are fast.
            // Avoid expensive filters like WEIGHTED and CONTEXT.
            vec![F_NONE, F_SUB, F_UP, F_AVERAGE, F_GRADIENT, F_MED]
        }
        BlockType::Natural => {
            // For natural photos, test all filters — good balance between
            // speed and quality. All 16 predictors may be useful.
            (0..NUM_FILTERS).collect()
        }
        BlockType::HighFreq => {
            // For high-frequency content (noise, textures), adaptive and median
            // predictors are excellent. Skip simple filters that won't capture
            // fine details.
            vec![
                F_WEIGHTED,
                F_MED,
                F_PAETH,
                F_CONTEXT,
                F_TR_DIRECTIONAL,
                F_SMEDIAN,
                F_2NDORDER,
                F_4WAY_H,
                F_4WAY_V,
                F_4WAY_D1,
                F_4WAY_D2,
            ]
        }
        BlockType::Mixed => {
            // For mixed content, test most filters to find the best match.
            (0..NUM_FILTERS).collect()
        }
    };

    // Test selected candidates with Shannon entropy
    let mut best_ftype = candidates_to_test[0];
    let mut best_entropy = f64::INFINITY;
    let mut best_filtered = Vec::new();

    for &ftype in &candidates_to_test {
        let filtered = filter_block(tile_raw, tile_height, bytes_per_row, bpp, ftype);
        let entropy = filtered
            .chunks_exact(bytes_per_row)
            .map(shannon_entropy)
            .sum::<f64>();

        if entropy < best_entropy {
            best_entropy = entropy;
            best_ftype = ftype;
            best_filtered = filtered;
        }
    }

    (best_ftype, best_filtered)
}

/// Applies a single filter to all rows of the block, returning the concatenated
/// residuals (without the filter code byte). The block's first row treats the
/// neighbor above as zero; the second treats `UU` as zero ("Tile edges",
/// section 4.3.1).
fn filter_block(
    tile_raw: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
    ftype: u8,
) -> Vec<u8> {
    // F_WEIGHTED requires adaptive state shared across the rows of the block.
    if ftype == F_WEIGHTED {
        let mut acc = Vec::with_capacity(tile_height * bytes_per_row);
        let mut prev_row: Option<&[u8]> = None;
        let mut state = WeightedState::default();
        for r in 0..tile_height {
            let row = &tile_raw[r * bytes_per_row..(r + 1) * bytes_per_row];
            let filtered = filter_row_weighted(row, prev_row, bpp, &mut state);
            acc.extend_from_slice(&filtered);
            prev_row = Some(row); // the predictor uses the ORIGINAL row, not the filtered one
        }
        return acc;
    }

    let mut acc = Vec::with_capacity(tile_height * bytes_per_row);
    let mut prev_row: Option<&[u8]> = None;
    let mut prev_prev_row: Option<&[u8]> = None;
    for r in 0..tile_height {
        let row = &tile_raw[r * bytes_per_row..(r + 1) * bytes_per_row];
        let filtered = filter_row(row, prev_row, prev_prev_row, ftype, bpp);
        acc.extend_from_slice(&filtered);
        prev_prev_row = prev_row;
        prev_row = Some(row); // the predictor uses the ORIGINAL row, not the filtered one
    }
    acc
}

/// Chooses the best filter for a whole block/tile (v1.0, section 4.3.1):
/// tests each of the `NUM_FILTERS` predictors over all the rows of the block and
/// picks the best-scoring one, according to the selected heuristic
/// (`FilterHeuristic::Entropy`, default, or `FilterHeuristic::CompressionTest`).
/// Returns the filter code and the whole block already filtered with it.
fn choose_best_block_filter(
    tile_raw: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
    heuristic: FilterHeuristic,
    level: i32,
) -> (u8, Vec<u8>) {
    // Fast path for QuickPrune: uses specialized implementation
    if matches!(heuristic, FilterHeuristic::QuickPrune) {
        return choose_best_block_filter_quick_prune(tile_raw, tile_height, bytes_per_row, bpp);
    }

    // Fast path for AdaptiveEntropy: uses specialized implementation
    if matches!(heuristic, FilterHeuristic::AdaptiveEntropy) {
        return choose_best_block_filter_adaptive(tile_raw, tile_height, bytes_per_row, bpp);
    }

    // Standard path for Entropy, Msad, CompressionTest
    let mut best_ftype = F_NONE;
    let mut best_score = f64::INFINITY;
    let mut best_filtered = Vec::new();

    for ftype in 0..NUM_FILTERS {
        let filtered = filter_block(tile_raw, tile_height, bytes_per_row, bpp, ftype);
        let score = match heuristic {
            // Shannon entropy summed row by row (bits) — the lower, the better.
            FilterHeuristic::Entropy => filtered
                .chunks_exact(bytes_per_row)
                .map(shannon_entropy)
                .sum(),
            // MSAD: sum of the absolute values of the residuals (the PNG classic).
            // Each filtered byte is a residual in [0, 255]; summing the bytes is the
            // unsigned version of SAD — the lower, the better.
            FilterHeuristic::Msad => filtered.iter().map(|&b| u64::from(b)).sum::<u64>() as f64,
            // Real compression test: smallest final size (raw or ZSTD, whichever is
            // smaller, via compress_with_fallback) of the compressed block. A compression
            // error discards the candidate (infinite score).
            FilterHeuristic::CompressionTest => match compress_with_fallback(&filtered, level) {
                Ok((_, compressed)) => compressed.len() as f64,
                Err(_) => f64::INFINITY,
            },
            // Unreachable: QuickPrune and AdaptiveEntropy are handled by fast paths above
            FilterHeuristic::QuickPrune | FilterHeuristic::AdaptiveEntropy => unreachable!(),
        };
        if score < best_score {
            best_score = score;
            best_ftype = ftype;
            best_filtered = filtered;
        }
    }
    (best_ftype, best_filtered)
}

/// Filters a whole tile with a single filter chosen for the entire block
/// (v1.0, section 4.3.1). The filter code is prefixed as 1 byte at the start
/// of the block; all the tile's rows share the same predictor.
///
/// # Security
/// - Validates that tile_raw contains exactly tile_height × bytes_per_row bytes
/// - Returns Err on insufficient data (untrusted input)
pub(crate) fn apply_predictive_filter(
    tile_raw: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
    heuristic: FilterHeuristic,
    level: i32,
) -> Result<Vec<u8>> {
    // SECURITY: Validates that we have enough space before any indexing
    let expected_size = tile_height.checked_mul(bytes_per_row).ok_or_else(|| {
        CafeError::TruncatedFile(
            "apply_predictive_filter: overflow in tile_height × bytes_per_row".into(),
        )
    })?;

    if tile_raw.len() < expected_size {
        return Err(CafeError::TruncatedFile(format!(
             "apply_predictive_filter: insufficient data. expected {} bytes (tile_height={} × bytes_per_row={}), got {}",
            expected_size, tile_height, bytes_per_row, tile_raw.len()
        )));
    }

    let (ftype, filtered) =
        choose_best_block_filter(tile_raw, tile_height, bytes_per_row, bpp, heuristic, level);

    let mut out = Vec::with_capacity(1 + filtered.len());
    out.push(ftype);
    out.extend_from_slice(&filtered);
    Ok(out)
}

/// Reverses the predictive filter of a whole tile (v1.0, section 4.3.1): reads a
/// single filter code byte at the start of the block and reverses the same
/// operation on all the tile's rows.
pub(crate) fn undo_predictive_filter(
    tile_data: &[u8],
    tile_height: usize,
    bytes_per_row: usize,
    bpp: usize,
) -> Result<Vec<u8>> {
    // SECURITY: Validates that we have at least 1 byte (filter code) before reading
    if tile_data.is_empty() {
        return Err(CafeError::TruncatedFile(
            "Insufficient filter data: file truncated when reading filter code".into(),
        ));
    }

    let ftype = tile_data[0];
    // The filter byte comes straight from the file (untrusted): validate before
    // using it, so that the `unreachable!()` in `predict()` is never reached
    // with a malicious/corrupted file (see the security audit).
    if ftype >= NUM_FILTERS {
        return Err(CafeError::UnsupportedFeature(format!(
            "invalid filter code: {ftype} (maximum allowed: {})",
            NUM_FILTERS - 1
        )));
    }

    // SECURITY: Validates that there are bytes_per_row × tile_height bytes after the code
    let data_bytes = tile_height.checked_mul(bytes_per_row).ok_or_else(|| {
        CafeError::TruncatedFile(
            "undo_predictive_filter: overflow in tile_height × bytes_per_row".into(),
        )
    })?;
    let needed = 1usize.checked_add(data_bytes).ok_or_else(|| {
        CafeError::TruncatedFile("undo_predictive_filter: overflow no Calculation of bytes".into())
    })?;
    if tile_data.len() < needed {
        return Err(CafeError::TruncatedFile(format!(
            "Insufficient filter data: expected {} bytes (1 de filtro + {} de dados), mas apenas {} available",
            needed, data_bytes, tile_data.len()
        )));
    }

    let mut out: Vec<u8> = Vec::with_capacity(tile_height * bytes_per_row);
    let body = &tile_data[1..];

    if ftype == F_WEIGHTED {
        let mut state = WeightedState::default();
        for r in 0..tile_height {
            let filtered = &body[r * bytes_per_row..(r + 1) * bytes_per_row];
            let prev_row = if out.len() >= bytes_per_row {
                Some(&out[out.len() - bytes_per_row..])
            } else {
                None
            };
            let row = unfilter_row_weighted(filtered, prev_row, bpp, &mut state);
            out.extend_from_slice(&row);
        }
        return Ok(out);
    }

    for r in 0..tile_height {
        let filtered = &body[r * bytes_per_row..(r + 1) * bytes_per_row];

        // prev_row/prev_prev_row must point to the last rows already
        // written in `out` (one and two positions back, respectively).
        let prev_row = if out.len() >= bytes_per_row {
            Some(&out[out.len() - bytes_per_row..])
        } else {
            None
        };
        let prev_prev_row = if out.len() >= 2 * bytes_per_row {
            Some(&out[out.len() - 2 * bytes_per_row..out.len() - bytes_per_row])
        } else {
            None
        };
        let row = unfilter_row(filtered, prev_row, prev_prev_row, ftype, bpp);
        out.extend_from_slice(&row);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_num_filters_includes_new_predictors() {
        assert_eq!(NUM_FILTERS, 16);
        assert_eq!(F_TR_DIRECTIONAL, 14);
        assert_eq!(F_WEIGHTED, 15);
    }

    #[test]
    fn test_tr_directional_predictor_formula() {
        // Average2(Average2(L, TL), Average2(T, TR)):
        // avg2(avg2(40,20), avg2(30,10)) = avg2(30, 20) = 25
        assert_eq!(tr_directional_predictor(40, 30, 20, 10), 25);
        // avg2(avg2(40,20), avg2(30,200)) = avg2(30, 115) = 72
        assert_eq!(tr_directional_predictor(40, 30, 20, 200), 72);
    }

    #[test]
    fn test_tr_directional_uses_tr_neighbor() {
        // The TR neighbor influences the prediction (right edge vs. high value):
        assert_eq!(tr_directional_predictor(100, 100, 100, 0), 75);
        assert_eq!(tr_directional_predictor(100, 100, 100, 200), 125);
    }

    #[test]
    fn test_weighted_predictor_adapts() {
        let mut state = WeightedState::default();
        // Uniform neighbors: pred = (8 * 4 * 100 + 16) >> 5 = 100
        let pred = weighted_predict(&state, 100, 100, 100, 100);
        assert_eq!(pred, ((8 * 4 * 100 + 16) >> 5) as u8);

        // Updates with an actual value close to W (a): W's weight rises, the others fall.
        weighted_update(&mut state, 100, 200, 200, 200, 100);
        assert!(
            state.weight[0] > state.weight[1],
            "W weight should increase (error 0), N weight decrease"
        );
        assert_eq!(state.weight[0], WEIGHTED_INIT_WEIGHT + 1);
        assert_eq!(state.weight[1], WEIGHTED_INIT_WEIGHT - 1);
    }

    #[test]
    fn test_tr_directional_roundtrip() {
        let data: Vec<u8> = (0..160).map(|i| ((i * 37) % 256) as u8).collect();
        for bpp in [1usize, 4usize] {
            let filtered = filter_block(&data, 4, 40, bpp, F_TR_DIRECTIONAL);
            let mut out = Vec::with_capacity(1 + filtered.len());
            out.push(F_TR_DIRECTIONAL);
            out.extend_from_slice(&filtered);
            let undone = undo_predictive_filter(&out, 4, 40, bpp).unwrap();
            assert_eq!(undone, data, "roundtrip F_TR_DIRECTIONAL bpp={bpp}");
        }
    }

    #[test]
    fn test_weighted_roundtrip() {
        let data: Vec<u8> = (0..160)
            .map(|i| ((i * 37 + (i / 3) * 7) % 256) as u8)
            .collect();
        for bpp in [1usize, 4usize] {
            let filtered = filter_block(&data, 4, 40, bpp, F_WEIGHTED);
            let mut out = Vec::with_capacity(1 + filtered.len());
            out.push(F_WEIGHTED);
            out.extend_from_slice(&filtered);
            let undone = undo_predictive_filter(&out, 4, 40, bpp).unwrap();
            assert_eq!(undone, data, "roundtrip F_WEIGHTED bpp={bpp}");
        }
    }

    #[test]
    fn test_apply_predictive_filter_roundtrips_with_new_filters() {
        // The heuristic now tests 16 filters; any choice must reverse correctly.
        let data: Vec<u8> = (0..240).map(|i| ((i * 13) % 256) as u8).collect();
        for &(h, lvl) in &[
            (FilterHeuristic::Entropy, 19),
            (FilterHeuristic::CompressionTest, 19),
        ] {
            let out = apply_predictive_filter(&data, 4, 60, 4, h, lvl).unwrap();
            let undone = undo_predictive_filter(&out, 4, 60, 4).unwrap();
            assert_eq!(undone, data, "roundtrip with heuristic {h:?}");
        }
    }

    #[test]
    fn test_undo_rejects_invalid_filter_code() {
        // ftype 16 ≥ NUM_FILTERS (16) → must be rejected
        let mut data = vec![NUM_FILTERS, 0, 1, 2, 3];
        data.extend_from_slice(&[0u8; 36]);
        match undo_predictive_filter(&data, 1, 36, 4) {
            Err(CafeError::UnsupportedFeature(msg)) => {
                assert!(msg.contains("invalid filter code"));
            }
            other => panic!("esperava UnsupportedFeature, obtive {other:?}"),
        }
    }

    #[test]
    fn test_tr_directional_selected_on_recurrence_field() {
        // Synthetic field that EXACTLY satisfies the TR predictor recurrence:
        // interior filled with tr_directional_predictor(...) → residual 0.
        // Shows that filter 14 is selectable by the heuristic and beats the others.
        let w = 64;
        let h = 32;
        let mut trfield = vec![0u8; w * h];
        for (x, cell) in trfield.iter_mut().take(w).enumerate() {
            *cell = ((x * 3) % 256) as u8;
        }
        for y in 0..h {
            trfield[y * w] = ((y * 5) % 256) as u8;
        }
        for y in 1..h {
            for x in 1..w - 1 {
                let l = trfield[y * w + (x - 1)];
                let tl = trfield[(y - 1) * w + (x - 1)];
                let t = trfield[(y - 1) * w + x];
                let tr = trfield[(y - 1) * w + (x + 1)];
                trfield[y * w + x] = tr_directional_predictor(l, t, tl, tr);
            }
        }
        let (ftype, _) = choose_best_block_filter(&trfield, h, w, 1, FilterHeuristic::Entropy, 19);
        assert_eq!(ftype, F_TR_DIRECTIONAL);
    }

    #[test]
    fn test_weighted_beats_average_on_mixed_direction() {
        // Half horizontal (perfect Sub), half vertical (perfect Up):
        // no fixed filter wins in both halves. The F_WEIGHTED weight adaptation
        // reduces the total absolute residual compared to Average.
        let w = 64;
        let h = 32;
        let mut mixed = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                mixed[y * w + x] = if y < 16 { x as u8 } else { (y % 256) as u8 };
            }
        }
        let absres = |f: u8| -> u64 {
            filter_block(&mixed, h, w, 1, f)
                .iter()
                .map(|&r| (r as i8).unsigned_abs() as u64)
                .sum::<u64>()
        };
        assert!(
            absres(F_WEIGHTED) < absres(F_AVERAGE),
            "F_WEIGHTED should adapt better than F_AVERAGE on mixed direction"
        );
    }

    // ========================================================================
    // Tests for QuickPrune (v1.1)
    // ========================================================================

    #[test]
    fn test_quick_prune_selects_valid_filter() {
        // Synthetic horizontal gradient: QuickPrune should select a valid filter
        let w = 64;
        let h = 16;
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = (x % 256) as u8; // Horizontal gradient
            }
        }

        let (ftype, filtered) = choose_best_block_filter_quick_prune(&data, h, w, 1);

        // QuickPrune should always select a valid filter (0-15)
        assert!(
            ftype < NUM_FILTERS,
            "QuickPrune selected invalid filter: {}",
            ftype
        );

        // Result should be filtered data (non-empty)
        assert!(
            !filtered.is_empty(),
            "QuickPrune should return filtered data"
        );
    }

    #[test]
    fn test_quick_prune_roundtrip() {
        // Ensure QuickPrune's chosen filter roundtrips correctly
        let data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let (ftype, filtered) = choose_best_block_filter_quick_prune(&data, 8, 32, 1);

        let mut with_code = vec![ftype];
        with_code.extend_from_slice(&filtered);

        let undone = undo_predictive_filter(&with_code, 8, 32, 1).unwrap();
        assert_eq!(undone, data, "QuickPrune roundtrip failed");
    }

    #[test]
    fn test_quick_prune_vs_entropy_selection() {
        // QuickPrune should sometimes select differently than pure Entropy
        // Test on a block where MSAD and Entropy disagree
        let w = 32;
        let h = 8;
        let data: Vec<u8> = (0..w * h)
            .map(|i| ((i * 17 + (i / 8) * 19) % 256) as u8)
            .collect();

        let (ftype_qp, _) = choose_best_block_filter_quick_prune(&data, h, w, 1);
        let (ftype_ent, _) = choose_best_block_filter(&data, h, w, 1, FilterHeuristic::Entropy, 19);

        // They may be the same or different, but both should be valid filters
        assert!(
            ftype_qp < NUM_FILTERS,
            "QuickPrune selected invalid filter: {}",
            ftype_qp
        );
        assert!(
            ftype_ent < NUM_FILTERS,
            "Entropy selected invalid filter: {}",
            ftype_ent
        );
    }

    // ========================================================================
    // Tests for AdaptiveEntropy (v1.1)
    // ========================================================================

    #[test]
    fn test_analyze_block_type_smooth() {
        // Uniform block should be detected as Smooth
        let w = 32;
        let h = 8;
        let data = vec![128u8; w * h]; // All pixels the same

        let stats = analyze_block_type(&data, h, w);
        assert!(
            stats.avg_variance < 1.0,
            "Uniform block should have very low variance"
        );
        assert_eq!(stats.detect_block_type(), BlockType::Smooth);
    }

    #[test]
    fn test_analyze_block_type_natural() {
        // Gentle gradient should be detected as Natural
        let w = 32;
        let h = 8;
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = ((x + y) % 256) as u8; // Gentle gradient
            }
        }

        let stats = analyze_block_type(&data, h, w);
        let block_type = stats.detect_block_type();
        assert!(
            block_type == BlockType::Natural || block_type == BlockType::Smooth,
            "Gentle gradient should be Natural or Smooth, got {:?}",
            block_type
        );
    }

    #[test]
    fn test_analyze_block_type_high_freq() {
        // Random-like data should be detected as HighFreq
        let w = 32;
        let h = 8;
        let data: Vec<u8> = (0..w * h).map(|i| ((i * 137 + 19) % 256) as u8).collect();

        let stats = analyze_block_type(&data, h, w);
        let block_type = stats.detect_block_type();
        assert!(
            block_type == BlockType::HighFreq || block_type == BlockType::Mixed,
            "Random-like data should be HighFreq or Mixed, got {:?}",
            block_type
        );
    }

    #[test]
    fn test_adaptive_entropy_selects_appropriate_filters() {
        // Test that AdaptiveEntropy selects filters appropriate to content
        let w = 32;
        let h = 8;

        // Smooth block
        let smooth_data = vec![100u8; w * h];
        let (ftype_smooth, _) = choose_best_block_filter_adaptive(&smooth_data, h, w, 1);
        assert!(
            ftype_smooth <= F_MED,
            "Smooth block should prefer simple filters (0-5), got {}",
            ftype_smooth
        );

        // HighFreq block
        let high_freq_data: Vec<u8> = (0..w * h).map(|i| ((i * 97 + 37) % 256) as u8).collect();
        let (ftype_hf, _) = choose_best_block_filter_adaptive(&high_freq_data, h, w, 1);
        // Should be a valid filter
        assert!(
            ftype_hf < NUM_FILTERS,
            "Invalid filter selected for HighFreq: {}",
            ftype_hf
        );
    }

    #[test]
    fn test_adaptive_entropy_roundtrip() {
        // Ensure AdaptiveEntropy's chosen filter roundtrips correctly
        let data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let (ftype, filtered) = choose_best_block_filter_adaptive(&data, 8, 32, 1);

        let mut with_code = vec![ftype];
        with_code.extend_from_slice(&filtered);

        let undone = undo_predictive_filter(&with_code, 8, 32, 1).unwrap();
        assert_eq!(undone, data, "AdaptiveEntropy roundtrip failed");
    }

    #[test]
    fn test_block_stats_deterministic() {
        // Block analysis should be deterministic
        let data: Vec<u8> = (0..128).map(|i| ((i * 73) % 256) as u8).collect();

        let stats1 = analyze_block_type(&data, 8, 16);
        let stats2 = analyze_block_type(&data, 8, 16);

        assert_eq!(stats1.avg_variance, stats2.avg_variance);
        assert_eq!(stats1.high_variance_regions, stats2.high_variance_regions);
        assert_eq!(stats1.total_regions, stats2.total_regions);
        assert_eq!(stats1.detect_block_type(), stats2.detect_block_type());
    }

    #[test]
    fn test_choose_best_block_filter_with_quick_prune() {
        // Test dispatch through choose_best_block_filter with QuickPrune
        let data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let (ftype, _) = choose_best_block_filter(&data, 8, 32, 1, FilterHeuristic::QuickPrune, 19);

        assert!(
            ftype < NUM_FILTERS,
            "QuickPrune dispatch selected invalid filter: {}",
            ftype
        );
    }

    #[test]
    fn test_choose_best_block_filter_with_adaptive_entropy() {
        // Test dispatch through choose_best_block_filter with AdaptiveEntropy
        let data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
        let (ftype, _) =
            choose_best_block_filter(&data, 8, 32, 1, FilterHeuristic::AdaptiveEntropy, 19);

        assert!(
            ftype < NUM_FILTERS,
            "AdaptiveEntropy dispatch selected invalid filter: {}",
            ftype
        );
    }
}
