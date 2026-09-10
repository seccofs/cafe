//! SIMD fast paths for the predictor filters (spec section 4.4.1),
//! deferred from v0.1 to 0.2 per `AGENTS.md`'s "scalar-is-reference,
//! SIMD-is-optimization" architecture: [`crate::predictor`]'s scalar
//! functions remain the format-defining reference implementation and are
//! never bypassed for correctness, only for throughput, and every SIMD
//! path here must produce bit-identical output to its scalar counterpart
//! (enforced by parity tests in `crates/cafe-codec/tests/`).
//!
//! **Scope decision (0.2, benchmark-first per `AGENTS.md`):** a
//! throwaway benchmark PoC (deleted after use) measured that ZSTD level
//! 19 (the encoder's default) dominates encode time so overwhelmingly
//! (~97% of total time on a 4096x4096 image) that vectorizing the
//! predictors alone would save at most ~3% end-to-end at that level.
//! At fast ZSTD levels (1-3, relevant for interactive/preview use), the
//! balance flips: predictor selection dominates (~80% of encode time),
//! since [`crate::predictor::choose_best_row_predictor`] runs all 6
//! candidate filters per row. This module targets that scenario — SIMD
//! is a real win specifically when the caller chooses a fast
//! [`crate::encoder::EncoderOptions::level`], not a general win at the
//! default level.
//!
//! **What's vectorized, and why not everything:** [`filter_row_simd`]
//! (encoder direction) covers all 6 predictor codes, since every one
//! reads only from already-known input bytes (`row`/`prev_row`), with no
//! byte-to-byte dependency within the output — genuinely
//! embarrassingly parallel. [`unfilter_row_simd`] (decoder direction)
//! only covers `None`/`Up`, the two codes whose reconstruction doesn't
//! depend on the just-reconstructed left neighbor within the *output*
//! buffer (see `predictor::unfilter_row`'s doc comment); `Sub`/
//! `Average`/`Paeth`/`Gradient` decoding is an inherently serial prefix
//! dependency (`out[x]` needs `out[x-bpp]`) that isn't a good SIMD
//! target without a parallel-prefix-scan rewrite, which nothing in this
//! phase's benchmark data showed a need for — decode is already fast
//! (244ms for a 67MB image, per the same PoC), so this is a deliberate,
//! documented scope boundary rather than an oversight.
//!
//! **Row-shift trick (bpp-generic, no per-bpp specialization):** every
//! predictor's "left"/"up-left" neighbor is just `row`/`prev_row` read
//! at a `bpp`-byte earlier offset. Instead of specializing SIMD code per
//! concrete `bpp` value (1/2/3/4/6/8/12/16, depending on
//! `color_type`/`bit_depth`), every implementation here uses unaligned
//! vector loads at both `row[i]` and `row[i - bpp]` — this works
//! correctly for *any* `bpp`, at the cost of the first `bpp` bytes of
//! each row (where `x - bpp` would read out of bounds) always being
//! handled by the scalar reference path.
//!
//! **Architecture coverage:** AVX2 (x86_64, runtime-detected via
//! `is_x86_feature_detected!`, since not every x86_64 CPU has it) and
//! NEON (aarch64, unconditional — every Tier-1 aarch64 target Rust
//! supports mandates NEON as a baseline, no runtime check needed). Any
//! other architecture (wasm32, non-AVX2 x86_64 at runtime, etc.) always
//! falls back to the scalar reference path via [`filter_row_simd`]/
//! [`unfilter_row_simd`] returning `None`.

#[cfg(target_arch = "x86_64")]
mod x86;

#[cfg(target_arch = "aarch64")]
mod neon;

use crate::predictor::{
    PREDICTOR_AVERAGE, PREDICTOR_GRADIENT, PREDICTOR_PAETH, PREDICTOR_SUB, PREDICTOR_UP,
};

/// Minimum row length below which SIMD overhead (feature detection,
/// setup, scalar-tail handling) isn't worth it over just calling the
/// scalar reference directly — an ordinary tuning constant, not a
/// correctness boundary (every SIMD implementation still produces
/// correct output for any length; this purely decides when to bother).
const MIN_SIMD_LEN: usize = 64;

/// Attempts a SIMD fast path for `filter_row` (encoder direction, spec
/// section 4.4.1). Returns `None` whenever no SIMD implementation
/// applies (unsupported code, row too short to be worth it, or no SIMD
/// feature available on this build/CPU) — callers must fall back to the
/// scalar reference ([`crate::predictor::filter_row`]) in that case.
///
/// `PREDICTOR_SUB` is the only code that never needs `prev_row` (it only
/// reads `row` itself); every other vectorized code requires
/// `prev_row.is_some()` — without it, `Up`/`Average`/`Paeth`/`Gradient`
/// degenerate to different, cheaper formulas (e.g. `Paeth` degenerates
/// exactly to `Sub`) that aren't worth a separate SIMD path, since a
/// tile's first row is a small fraction of any real image.
pub(crate) fn filter_row_simd(
    row: &[u8],
    prev_row: Option<&[u8]>,
    code: u8,
    bpp: usize,
) -> Option<Vec<u8>> {
    if row.len() < MIN_SIMD_LEN || bpp == 0 {
        return None;
    }

    match code {
        PREDICTOR_SUB => dispatch_sub(row, bpp),
        PREDICTOR_UP => dispatch_up(row, prev_row?),
        PREDICTOR_AVERAGE => dispatch_average(row, prev_row?, bpp),
        PREDICTOR_PAETH => dispatch_paeth(row, prev_row?, bpp),
        PREDICTOR_GRADIENT => dispatch_gradient(row, prev_row?, bpp),
        _ => None,
    }
}

/// Attempts a SIMD fast path for `unfilter_row` (decoder direction).
/// Only `PREDICTOR_UP`'s reconstruction has no serial dependency on the
/// just-reconstructed left neighbor (see this module's doc comment for
/// why `Sub`/`Average`/`Paeth`/`Gradient` are out of scope here).
pub(crate) fn unfilter_row_simd(
    filtered: &[u8],
    prev_row: Option<&[u8]>,
    code: u8,
    _bpp: usize,
) -> Option<Vec<u8>> {
    if filtered.len() < MIN_SIMD_LEN {
        return None;
    }
    match code {
        PREDICTOR_UP => dispatch_unfilter_up(filtered, prev_row?),
        _ => None,
    }
}

// --- Per-architecture dispatch ---------------------------------------
//
// Each `dispatch_*` function below picks the fastest available
// implementation at runtime (x86_64: AVX2 if detected) or unconditionally
// (aarch64: NEON, always present), returning `None` when this build has
// no SIMD implementation for the current architecture at all.

fn dispatch_sub(row: &[u8], bpp: usize) -> Option<Vec<u8>> {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::filter_sub_avx2(row, bpp) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::filter_sub_neon(row, bpp) });
    }
    #[allow(unreachable_code)]
    None
}

fn dispatch_up(row: &[u8], prev_row: &[u8]) -> Option<Vec<u8>> {
    if row.len() != prev_row.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::filter_up_avx2(row, prev_row) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::filter_up_neon(row, prev_row) });
    }
    #[allow(unreachable_code)]
    None
}

fn dispatch_average(row: &[u8], prev_row: &[u8], bpp: usize) -> Option<Vec<u8>> {
    if row.len() != prev_row.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::filter_average_avx2(row, prev_row, bpp) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::filter_average_neon(row, prev_row, bpp) });
    }
    #[allow(unreachable_code)]
    None
}

fn dispatch_paeth(row: &[u8], prev_row: &[u8], bpp: usize) -> Option<Vec<u8>> {
    if row.len() != prev_row.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::filter_paeth_avx2(row, prev_row, bpp) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::filter_paeth_neon(row, prev_row, bpp) });
    }
    #[allow(unreachable_code)]
    None
}

fn dispatch_gradient(row: &[u8], prev_row: &[u8], bpp: usize) -> Option<Vec<u8>> {
    if row.len() != prev_row.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::filter_gradient_avx2(row, prev_row, bpp) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::filter_gradient_neon(row, prev_row, bpp) });
    }
    #[allow(unreachable_code)]
    None
}

fn dispatch_unfilter_up(filtered: &[u8], prev_row: &[u8]) -> Option<Vec<u8>> {
    if filtered.len() != prev_row.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return Some(unsafe { x86::unfilter_up_avx2(filtered, prev_row) });
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return Some(unsafe { neon::unfilter_up_neon(filtered, prev_row) });
    }
    #[allow(unreachable_code)]
    None
}
