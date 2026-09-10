//! Parity tests between the SIMD fast paths (`crate::simd`, dispatched
//! transparently by `predictor::filter_row`/`unfilter_row`) and the
//! scalar reference (`predictor::filter_row_scalar`/
//! `unfilter_row_scalar`) — the "scalar-is-reference,
//! SIMD-is-optimization" architecture (`AGENTS.md`) is only as safe as
//! these tests are thorough, since `filter_row`/`unfilter_row`
//! transparently pick whichever path is available on the running CPU
//! and callers (the encoder/decoder) never know which one ran.
//!
//! Every test below exercises every predictor code across every `bpp`
//! value the format actually uses (1/2/3/4/6/8/12/16, from
//! gray/RGB/RGBA x 8/16-bit x direct/palette-index) and a spread of row
//! lengths straddling the SIMD lane width (32 bytes for AVX2, 16 for
//! NEON, so anything from "shorter than one lane" through "several
//! lanes plus an uneven tail" needs covering) — including exact
//! multiples and non-multiples of both 16 and 32, since the scalar tail
//! handling in `crate::simd::x86`/`neon` is exactly the kind of
//! off-by-one-prone code parity tests exist to catch.

use cafe_codec::predictor::{
    filter_row, filter_row_scalar, unfilter_row, unfilter_row_scalar, NUM_PREDICTORS,
};
use proptest::prelude::*;

/// Deterministic pseudo-random byte generator (xorshift-style, no `rand`
/// dependency, matching this project's existing no-`rand` convention in
/// `cafe-bench`'s corpus generators).
fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2685821657736338717).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect()
}

/// Row lengths chosen to straddle both SIMD lane widths (16 for NEON, 32
/// for AVX2) and this crate's `MIN_SIMD_LEN` (64) threshold: too-short
/// (below MIN_SIMD_LEN, always scalar), right at/near the threshold,
/// non-multiples of 16/32 (uneven tails), and larger multi-lane rows.
const ROW_LENGTHS: &[usize] = &[
    0, 1, 3, 8, 15, 16, 17, 31, 32, 33, 48, 63, 64, 65, 96, 97, 100, 128, 129, 200, 257, 1000,
];

/// Every `bpp` value the spec's `color_type`/`bit_depth` combinations
/// actually produce: 1 (gray-8 or any palette index), 2 (gray-16), 3
/// (RGB-8), 4 (RGBA-8 or RGB-float32... no, RGB-16 is 6; kept here as
/// the literal byte-stride values `bytes_per_pixel()` can return), 6
/// (RGB-16), 8 (RGBA-16), 12 (RGB-32F), 16 (RGBA-32F).
const BPP_VALUES: &[usize] = &[1, 2, 3, 4, 6, 8, 12, 16];

fn all_codes() -> impl Iterator<Item = u8> {
    0..NUM_PREDICTORS
}

#[test]
fn test_filter_row_simd_matches_scalar_first_row_no_prev() {
    for &bpp in BPP_VALUES {
        for &len in ROW_LENGTHS {
            let row = pseudo_random_bytes(len, (bpp * 1000 + len) as u64);
            for code in all_codes() {
                let scalar = filter_row_scalar(&row, None, code, bpp).unwrap();
                let dispatched = filter_row(&row, None, code, bpp).unwrap();
                assert_eq!(
                    dispatched, scalar,
                    "mismatch: code={code} bpp={bpp} len={len} (no prev_row)"
                );
            }
        }
    }
}

#[test]
fn test_filter_row_simd_matches_scalar_with_prev_row() {
    for &bpp in BPP_VALUES {
        for &len in ROW_LENGTHS {
            let row = pseudo_random_bytes(len, (bpp * 2000 + len) as u64);
            let prev_row = pseudo_random_bytes(len, (bpp * 3000 + len) as u64);
            for code in all_codes() {
                let scalar = filter_row_scalar(&row, Some(&prev_row), code, bpp).unwrap();
                let dispatched = filter_row(&row, Some(&prev_row), code, bpp).unwrap();
                assert_eq!(
                    dispatched, scalar,
                    "mismatch: code={code} bpp={bpp} len={len} (with prev_row)"
                );
            }
        }
    }
}

#[test]
fn test_unfilter_row_simd_matches_scalar_first_row_no_prev() {
    for &bpp in BPP_VALUES {
        for &len in ROW_LENGTHS {
            let filtered = pseudo_random_bytes(len, (bpp * 4000 + len) as u64);
            for code in all_codes() {
                let scalar = unfilter_row_scalar(&filtered, None, code, bpp).unwrap();
                let dispatched = unfilter_row(&filtered, None, code, bpp).unwrap();
                assert_eq!(
                    dispatched, scalar,
                    "mismatch: code={code} bpp={bpp} len={len} (no prev_row)"
                );
            }
        }
    }
}

#[test]
fn test_unfilter_row_simd_matches_scalar_with_prev_row() {
    for &bpp in BPP_VALUES {
        for &len in ROW_LENGTHS {
            let filtered = pseudo_random_bytes(len, (bpp * 5000 + len) as u64);
            let prev_row = pseudo_random_bytes(len, (bpp * 6000 + len) as u64);
            for code in all_codes() {
                let scalar = unfilter_row_scalar(&filtered, Some(&prev_row), code, bpp).unwrap();
                let dispatched = unfilter_row(&filtered, Some(&prev_row), code, bpp).unwrap();
                assert_eq!(
                    dispatched, scalar,
                    "mismatch: code={code} bpp={bpp} len={len} (with prev_row)"
                );
            }
        }
    }
}

/// Edge-value-heavy input (0x00/0xFF alternating and runs), since the
/// Paeth/Average/Gradient math (wrapping arithmetic, widen/narrow,
/// branchless select) is most likely to hide a bug at saturation
/// boundaries rather than in "typical" random mid-range bytes.
#[test]
fn test_filter_row_simd_matches_scalar_extreme_values() {
    for &bpp in BPP_VALUES {
        for &len in &[64usize, 65, 96, 128, 129] {
            let row: Vec<u8> = (0..len)
                .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
                .collect();
            let prev_row: Vec<u8> = (0..len)
                .map(|i| if i % 3 == 0 { 0xFF } else { 0x00 })
                .collect();
            for code in all_codes() {
                let scalar = filter_row_scalar(&row, Some(&prev_row), code, bpp).unwrap();
                let dispatched = filter_row(&row, Some(&prev_row), code, bpp).unwrap();
                assert_eq!(
                    dispatched, scalar,
                    "mismatch: code={code} bpp={bpp} len={len} (extreme values)"
                );
            }
        }
    }
}

/// A full filter-then-unfilter round trip through whichever path
/// `filter_row`/`unfilter_row` actually dispatch to on this machine
/// (SIMD if available, scalar otherwise) must always reconstruct the
/// original row exactly — the property the encoder/decoder actually
/// depend on, independent of the scalar-vs-SIMD parity tests above.
#[test]
fn test_filter_then_unfilter_roundtrip_dispatched_all_codes_bpp_len() {
    for &bpp in BPP_VALUES {
        for &len in ROW_LENGTHS {
            if len == 0 {
                continue;
            }
            let row = pseudo_random_bytes(len, (bpp * 7000 + len) as u64);
            let prev_row = pseudo_random_bytes(len, (bpp * 8000 + len) as u64);
            for code in all_codes() {
                let filtered = filter_row(&row, Some(&prev_row), code, bpp).unwrap();
                let restored = unfilter_row(&filtered, Some(&prev_row), code, bpp).unwrap();
                assert_eq!(
                    restored, row,
                    "roundtrip failed: code={code} bpp={bpp} len={len}"
                );
            }
        }
    }
}

/// Proptest complement to the exhaustive `ROW_LENGTHS`/`BPP_VALUES`
/// sweeps above: arbitrary row content, length, and `bpp`, checked
/// against the same dispatched-vs-scalar parity property. Deliberately
/// narrower per-run (proptest already runs hundreds of cases) but with
/// no hand-picked-input bias at all, unlike the deterministic sweeps.
#[test]
fn prop_filter_row_simd_matches_scalar() {
    proptest!(|(
        len in 0usize..300,
        bpp in 1usize..=16,
        code in 0u8..NUM_PREDICTORS,
        seed in 0u64..=u64::MAX,
        has_prev in any::<bool>(),
    )| {
        let row = pseudo_random_bytes(len, seed);
        let prev_row = pseudo_random_bytes(len, seed.wrapping_add(1));
        let prev = if has_prev { Some(prev_row.as_slice()) } else { None };

        let scalar = filter_row_scalar(&row, prev, code, bpp).unwrap();
        let dispatched = filter_row(&row, prev, code, bpp).unwrap();
        prop_assert_eq!(dispatched, scalar);
    });
}

/// Proptest complement for `unfilter_row`, mirroring
/// `prop_filter_row_simd_matches_scalar` above.
#[test]
fn prop_unfilter_row_simd_matches_scalar() {
    proptest!(|(
        len in 0usize..300,
        bpp in 1usize..=16,
        code in 0u8..NUM_PREDICTORS,
        seed in 0u64..=u64::MAX,
        has_prev in any::<bool>(),
    )| {
        let filtered = pseudo_random_bytes(len, seed);
        let prev_row = pseudo_random_bytes(len, seed.wrapping_add(1));
        let prev = if has_prev { Some(prev_row.as_slice()) } else { None };

        let scalar = unfilter_row_scalar(&filtered, prev, code, bpp).unwrap();
        let dispatched = unfilter_row(&filtered, prev, code, bpp).unwrap();
        prop_assert_eq!(dispatched, scalar);
    });
}
