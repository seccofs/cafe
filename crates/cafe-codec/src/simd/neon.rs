//! NEON (aarch64) fast paths for the predictor filters — the same
//! algorithms as `super::x86`'s AVX2 versions (see that module's doc
//! comment for the math identities shared by both), just expressed with
//! NEON intrinsics and 16-byte (128-bit) vectors instead of AVX2's
//! 32-byte ones. Unlike AVX2, NEON needs no runtime feature-detection
//! call: every aarch64 target Rust supports mandates NEON as a baseline
//! ISA feature, so these functions are safe to call unconditionally on
//! `target_arch = "aarch64"` (still marked `unsafe` because they call
//! `unsafe` intrinsics, not because of any runtime feature check).
//!
//! **Validation note (per `AGENTS.md`):** this module is cross-compiled
//! and `cargo check`-verified for `aarch64-unknown-linux-gnu` locally
//! (no ARM hardware available in this environment), with real
//! byte-for-byte parity execution deferred to CI on an actual aarch64
//! runner — see `.github/workflows/ci.yml`'s `simd-parity-aarch64` job.

use std::arch::aarch64::*;

const LANES: usize = 16;

/// `floor((a + b) / 2)` per byte lane, both unsigned — NEON has a direct
/// instruction for this (`vrhadd` is *rounding* half-add, not what we
/// want; `vhadd_u8`/`vhaddq_u8` is the plain floor-average we need).
#[inline]
#[target_feature(enable = "neon")]
unsafe fn avg_floor_epu8(a: uint8x16_t, b: uint8x16_t) -> uint8x16_t {
    vhaddq_u8(a, b)
}

/// `PREDICTOR_SUB` (encoder). See `x86::filter_sub_avx2` for the shared
/// algorithm description.
///
/// # Safety
/// Only requires `target_arch = "aarch64"` (NEON is a baseline feature
/// there).
#[target_feature(enable = "neon")]
pub(super) unsafe fn filter_sub_neon(row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];
    out[..bpp.min(len)].copy_from_slice(&row[..bpp.min(len)]);

    let mut x = bpp;
    while x + LANES <= len {
        let cur = vld1q_u8(row.as_ptr().add(x));
        let left = vld1q_u8(row.as_ptr().add(x - bpp));
        let res = vsubq_u8(cur, left);
        vst1q_u8(out.as_mut_ptr().add(x), res);
        x += LANES;
    }
    while x < len {
        out[x] = row[x].wrapping_sub(row[x - bpp]);
        x += 1;
    }
    out
}

/// `PREDICTOR_UP` (encoder). See `x86::filter_up_avx2`.
///
/// # Safety
/// Only requires `target_arch = "aarch64"`; caller must ensure
/// `row.len() == prev_row.len()`.
#[target_feature(enable = "neon")]
pub(super) unsafe fn filter_up_neon(row: &[u8], prev_row: &[u8]) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    let mut x = 0;
    while x + LANES <= len {
        let cur = vld1q_u8(row.as_ptr().add(x));
        let up = vld1q_u8(prev_row.as_ptr().add(x));
        let res = vsubq_u8(cur, up);
        vst1q_u8(out.as_mut_ptr().add(x), res);
        x += LANES;
    }
    while x < len {
        out[x] = row[x].wrapping_sub(prev_row[x]);
        x += 1;
    }
    out
}

/// `PREDICTOR_AVERAGE` (encoder). See `x86::filter_average_avx2`.
///
/// # Safety
/// Only requires `target_arch = "aarch64"`; caller must ensure
/// `row.len() == prev_row.len()`.
#[target_feature(enable = "neon")]
pub(super) unsafe fn filter_average_neon(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    for x in 0..bpp.min(len) {
        let up = prev_row[x];
        let pred = (up as u16 / 2) as u8;
        out[x] = row[x].wrapping_sub(pred);
    }

    let mut x = bpp;
    while x + LANES <= len {
        let cur = vld1q_u8(row.as_ptr().add(x));
        let left = vld1q_u8(row.as_ptr().add(x - bpp));
        let up = vld1q_u8(prev_row.as_ptr().add(x));
        let pred = avg_floor_epu8(left, up);
        let res = vsubq_u8(cur, pred);
        vst1q_u8(out.as_mut_ptr().add(x), res);
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

/// `PREDICTOR_GRADIENT` (encoder). See `x86::filter_gradient_avx2`.
///
/// # Safety
/// Only requires `target_arch = "aarch64"`; caller must ensure
/// `row.len() == prev_row.len()`.
#[target_feature(enable = "neon")]
pub(super) unsafe fn filter_gradient_neon(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    let len = row.len();
    let mut out = vec![0u8; len];

    for x in 0..bpp.min(len) {
        out[x] = row[x].wrapping_sub(prev_row[x]);
    }

    let mut x = bpp;
    while x + LANES <= len {
        let cur = vld1q_u8(row.as_ptr().add(x));
        let left = vld1q_u8(row.as_ptr().add(x - bpp));
        let up = vld1q_u8(prev_row.as_ptr().add(x));
        let up_left = vld1q_u8(prev_row.as_ptr().add(x - bpp));
        let res = vaddq_u8(vsubq_u8(vsubq_u8(cur, left), up), up_left);
        vst1q_u8(out.as_mut_ptr().add(x), res);
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

/// `PREDICTOR_PAETH` (encoder), 8 bytes per iteration (NEON's widening
/// instructions operate on 64-bit halves, `uint8x8_t` -> `int16x8_t`).
/// See `x86::filter_paeth_avx2` for the shared algorithm description
/// (widen to 16-bit, compute `p`/distances, branchless select, narrow
/// back to `u8`).
///
/// # Safety
/// Only requires `target_arch = "aarch64"`; caller must ensure
/// `row.len() == prev_row.len()`.
#[target_feature(enable = "neon")]
pub(super) unsafe fn filter_paeth_neon(row: &[u8], prev_row: &[u8], bpp: usize) -> Vec<u8> {
    const PAETH_LANES: usize = 8;
    let len = row.len();
    let mut out = vec![0u8; len];

    for x in 0..bpp.min(len) {
        out[x] = row[x].wrapping_sub(prev_row[x]);
    }

    let mut x = bpp;
    while x + PAETH_LANES <= len {
        let left8 = vld1_u8(row.as_ptr().add(x - bpp));
        let up8 = vld1_u8(prev_row.as_ptr().add(x));
        let up_left8 = vld1_u8(prev_row.as_ptr().add(x - bpp));
        let cur8 = vld1_u8(row.as_ptr().add(x));

        let a = vreinterpretq_s16_u16(vmovl_u8(left8));
        let b = vreinterpretq_s16_u16(vmovl_u8(up8));
        let c = vreinterpretq_s16_u16(vmovl_u8(up_left8));

        let p = vsubq_s16(vaddq_s16(a, b), c);
        let pa = vabsq_s16(vsubq_s16(p, a));
        let pb = vabsq_s16(vsubq_s16(p, b));
        let pc = vabsq_s16(vsubq_s16(p, c));

        // cond1 = pa <= pb && pa <= pc  (select `a`), expressed via
        // negated greater-than to preserve ties in favor of `a`, exactly
        // matching the scalar reference (see x86.rs's identical note).
        let pa_gt_pb = vcgtq_s16(pa, pb);
        let pa_gt_pc = vcgtq_s16(pa, pc);
        let cond1 = vmvnq_u16(vorrq_u16(pa_gt_pb, pa_gt_pc));

        let pb_gt_pc = vcgtq_s16(pb, pc);
        let cond2 = vmvnq_u16(pb_gt_pc);

        let b_or_c = vbslq_s16(cond2, b, c);
        let pred16 = vbslq_s16(cond1, a, b_or_c);

        // Narrow 8 i16 predictions (each in 0..=255) back to 8 u8 bytes.
        let pred8 = vqmovun_s16(pred16);

        let res = vsub_u8(cur8, pred8);
        vst1_u8(out.as_mut_ptr().add(x), res);

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

/// `PREDICTOR_UP` (decoder direction). See `x86::unfilter_up_avx2`.
///
/// # Safety
/// Only requires `target_arch = "aarch64"`; caller must ensure
/// `filtered.len() == prev_row.len()`.
#[target_feature(enable = "neon")]
pub(super) unsafe fn unfilter_up_neon(filtered: &[u8], prev_row: &[u8]) -> Vec<u8> {
    let len = filtered.len();
    let mut out = vec![0u8; len];

    let mut x = 0;
    while x + LANES <= len {
        let res = vld1q_u8(filtered.as_ptr().add(x));
        let up = vld1q_u8(prev_row.as_ptr().add(x));
        let orig = vaddq_u8(res, up);
        vst1q_u8(out.as_mut_ptr().add(x), orig);
        x += LANES;
    }
    while x < len {
        out[x] = filtered[x].wrapping_add(prev_row[x]);
        x += 1;
    }
    out
}
