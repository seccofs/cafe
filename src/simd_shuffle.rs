//! SIMD Byte-Shuffle Optimization (AVX2 / PSHUFB)
//!
//! Implements vectorized byte-shuffling using the AVX2 `PSHUFB`
//! (`_mm256_shuffle_epi8`) instruction for fast byte reordering in Filter
//! Method 1 (byte-shuffle).
//!
//! **Background:**
//! Filter Method 1 reorders bytes within multi-byte samples (floats, 16/32-bit
//! integers, etc.) so that same-position bytes across all pixels become
//! contiguous ("array of structs" -> "struct of arrays"), improving
//! compressibility.
//!
//! - Input (AoS):  `[P0_b0, P0_b1, ..., P0_b{bpp-1}, P1_b0, P1_b1, ...]`
//! - Output (SoA): `[P0_b0, P1_b0, P2_b0, ..., P0_b1, P1_b1, P2_b1, ...]`
//!
//! # Algorithm
//!
//! For a 128-bit lane holding `P = 16 / bpp` whole pixels, reordering bytes
//! from AoS to "grouped by byte-position" order within the lane is a fixed
//! permutation, computable once per `bpp` and applied via `PSHUFB`
//! (`_mm256_shuffle_epi8`), which permutes each 128-bit half of a 256-bit
//! register independently using a per-lane index table.
//!
//! After the in-lane permutation, each `bpp`-sized group of `P` bytes must be
//! scattered to (encode) or gathered from (decode) its corresponding
//! byte-plane in the (globally non-contiguous) output/input buffer — this
//! part is done with plain slice copies, which is the correct approach since
//! the destination offsets are not contiguous in general (this is inherent to
//! the transform, not a shortcut).
//!
//! # Dispatch
//! - x86_64: callers (`shuffle.rs`) check `is_x86_feature_detected!("avx2")`
//!   **at runtime** before calling into this module; the AVX2 functions here
//!   assume AVX2 is available (enforced via `#[target_feature(enable =
//!   "avx2")]` on the `unsafe` implementation functions).
//! - aarch64: dispatch is **compile-time only**, via
//!   `#[cfg(target_arch = "aarch64")]` — NEON is mandatory on ARMv8-A, so no
//!   runtime feature check is needed (same rationale as `simd.rs`).
//!
//! # NEON Implementation
//! NEON's `vqtbl1q_u8` (single-register table lookup) is a direct
//! equivalent of AVX2's `PSHUFB` but operates on one 128-bit register at a
//! time (there is no 256-bit-wide table-lookup instruction on NEON, unlike
//! AVX2's per-128-bit-lane `_mm256_shuffle_epi8`), so the NEON kernels
//! process `pixels_per_lane` pixels per iteration (one `vld1q_u8` +
//! `vqtbl1q_u8` + `vst1q_u8`) instead of AVX2's `2 * pixels_per_lane`
//! (which needs two independent 128-bit shuffles glued into a 256-bit op).
//! The permutation index tables ([`build_encode_mask`]/[`build_decode_mask`])
//! are architecture-agnostic and reused as-is (no duplication needed, unlike
//! [`duplicate_mask`] which only exists for AVX2's 256-bit-wide load).

use crate::error::CafeError;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

/// Applies byte-shuffle using AVX2 PSHUFB for fast vectorized reordering.
///
/// # Arguments
/// * `data` - Input buffer in AoS (natural pixel) layout
/// * `bpp` - Bytes per pixel (2, 4, 8, or 16)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
///
/// # Safety
/// Caller must have verified `is_x86_feature_detected!("avx2")` returns true.
#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn apply_byte_shuffle_simd(
    data: &[u8],
    bpp: usize,
    width: u32,
    height: u32,
) -> crate::Result<Vec<u8>> {
    let pixels = (width as usize) * (height as usize);
    match bpp {
        2 | 4 | 8 | 16 => {
            #[cfg(target_arch = "x86_64")]
            {
                if is_x86_feature_detected!("avx2") {
                    return Ok(unsafe { apply_byte_shuffle_avx2_impl(data, bpp, pixels) });
                }
            }
            #[cfg(target_arch = "aarch64")]
            {
                return Ok(unsafe { apply_byte_shuffle_neon_impl(data, bpp, pixels) });
            }
            #[allow(unreachable_code)]
            Ok(apply_byte_shuffle_generic_scalar(data, bpp, pixels))
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "byte-shuffle SIMD not supported for bpp={}",
            bpp
        ))),
    }
}

/// Reverses byte-shuffle (for decode): converts SoA layout back to AoS.
///
/// # Safety
/// Caller must have verified `is_x86_feature_detected!("avx2")` returns true.
#[cfg(all(feature = "simd", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn undo_byte_shuffle_simd(
    data: &[u8],
    bpp: usize,
    width: u32,
    height: u32,
) -> crate::Result<Vec<u8>> {
    let pixels = (width as usize) * (height as usize);
    match bpp {
        2 | 4 | 8 | 16 => {
            #[cfg(target_arch = "x86_64")]
            {
                if is_x86_feature_detected!("avx2") {
                    return Ok(unsafe { undo_byte_shuffle_avx2_impl(data, bpp, pixels) });
                }
            }
            #[cfg(target_arch = "aarch64")]
            {
                return Ok(unsafe { undo_byte_shuffle_neon_impl(data, bpp, pixels) });
            }
            #[allow(unreachable_code)]
            Ok(undo_byte_shuffle_generic_scalar(data, bpp, pixels))
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "byte-shuffle SIMD reverse not supported for bpp={}",
            bpp
        ))),
    }
}

/// Builds the in-lane PSHUFB/TBL index table for the encode direction:
/// `mask[b * pixels_per_lane + p] = p * bpp + b`, i.e. output position
/// `b*P+p` (grouped by byte-position `b`, pixel `p` within the lane) reads
/// from AoS input position `p*bpp+b`. Shared by both the AVX2 (`PSHUFB`)
/// and NEON (`vqtbl1q_u8`) kernels.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn build_encode_mask(bpp: usize) -> [i8; 16] {
    let pixels_per_lane = 16 / bpp;
    let mut mask = [0i8; 16];
    for p in 0..pixels_per_lane {
        for b in 0..bpp {
            let old_pos = p * bpp + b;
            let new_pos = b * pixels_per_lane + p;
            mask[new_pos] = old_pos as i8;
        }
    }
    mask
}

/// Builds the in-lane PSHUFB/TBL index table for the decode direction (the
/// inverse permutation of [`build_encode_mask`]):
/// `mask[p * bpp + b] = b * pixels_per_lane + p`, i.e. AoS output position
/// `p*bpp+b` reads from the "grouped by byte-position" input at `b*P+p`.
/// Shared by both the AVX2 (`PSHUFB`) and NEON (`vqtbl1q_u8`) kernels.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn build_decode_mask(bpp: usize) -> [i8; 16] {
    let pixels_per_lane = 16 / bpp;
    let mut mask = [0i8; 16];
    for p in 0..pixels_per_lane {
        for b in 0..bpp {
            let old_pos = b * pixels_per_lane + p;
            let new_pos = p * bpp + b;
            mask[new_pos] = old_pos as i8;
        }
    }
    mask
}

#[cfg(target_arch = "x86_64")]
fn duplicate_mask(mask16: [i8; 16]) -> [i8; 32] {
    let mut mask32 = [0i8; 32];
    mask32[0..16].copy_from_slice(&mask16);
    mask32[16..32].copy_from_slice(&mask16);
    mask32
}

/// AVX2 encode implementation. Processes `32 / bpp` pixels per iteration
/// (one 256-bit register = two 128-bit lanes of `16 / bpp` pixels each),
/// falling back to the scalar reference for any remaining tail pixels.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn apply_byte_shuffle_avx2_impl(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    use std::arch::x86_64::*;

    let pixels_per_lane = 16 / bpp;
    let pixels_per_vec = 32 / bpp;
    let mask = duplicate_mask(build_encode_mask(bpp));
    let mask_vec = _mm256_loadu_si256(mask.as_ptr() as *const __m256i);

    let mut output = vec![0u8; pixels * bpp];
    let mut i = 0;

    while i + pixels_per_vec <= pixels {
        let loaded = _mm256_loadu_si256(data.as_ptr().add(i * bpp) as *const __m256i);
        let shuffled = _mm256_shuffle_epi8(loaded, mask_vec);

        let mut temp = [0u8; 32];
        _mm256_storeu_si256(temp.as_mut_ptr() as *mut __m256i, shuffled);

        for h in 0..2 {
            let pixel_start = i + h * pixels_per_lane;
            for b in 0..bpp {
                let src_off = h * 16 + b * pixels_per_lane;
                let dst_off = b * pixels + pixel_start;
                output[dst_off..dst_off + pixels_per_lane]
                    .copy_from_slice(&temp[src_off..src_off + pixels_per_lane]);
            }
        }

        i += pixels_per_vec;
    }

    // Scalar tail
    for pix in i..pixels {
        for b in 0..bpp {
            output[b * pixels + pix] = data[pix * bpp + b];
        }
    }

    output
}

/// AVX2 decode implementation, symmetric to [`apply_byte_shuffle_avx2_impl`].
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn undo_byte_shuffle_avx2_impl(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    use std::arch::x86_64::*;

    let pixels_per_lane = 16 / bpp;
    let pixels_per_vec = 32 / bpp;
    let mask = duplicate_mask(build_decode_mask(bpp));
    let mask_vec = _mm256_loadu_si256(mask.as_ptr() as *const __m256i);

    let mut output = vec![0u8; pixels * bpp];
    let mut i = 0;

    while i + pixels_per_vec <= pixels {
        let mut temp = [0u8; 32];
        for h in 0..2 {
            let pixel_start = i + h * pixels_per_lane;
            for b in 0..bpp {
                let src_off = b * pixels + pixel_start;
                let dst_off = h * 16 + b * pixels_per_lane;
                temp[dst_off..dst_off + pixels_per_lane]
                    .copy_from_slice(&data[src_off..src_off + pixels_per_lane]);
            }
        }

        let loaded = _mm256_loadu_si256(temp.as_ptr() as *const __m256i);
        let shuffled = _mm256_shuffle_epi8(loaded, mask_vec);
        _mm256_storeu_si256(output.as_mut_ptr().add(i * bpp) as *mut __m256i, shuffled);

        i += pixels_per_vec;
    }

    // Scalar tail
    for pix in i..pixels {
        for b in 0..bpp {
            output[pix * bpp + b] = data[b * pixels + pix];
        }
    }

    output
}

/// NEON encode implementation. `vqtbl1q_u8` is a single-register 128-bit
/// table lookup (direct NEON analogue of `PSHUFB`), so each iteration
/// processes exactly `pixels_per_lane` pixels (one 128-bit lane) — half of
/// AVX2's per-iteration throughput (which glues two 128-bit shuffles into
/// one 256-bit op), since NEON has no wider table-lookup instruction.
/// Falls back to the scalar reference for any remaining tail pixels.
#[cfg(all(feature = "simd", target_arch = "aarch64"))]
#[target_feature(enable = "neon")]
unsafe fn apply_byte_shuffle_neon_impl(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    let pixels_per_lane = 16 / bpp;
    let mask = build_encode_mask(bpp);
    let mask_vec = vld1q_u8(mask.as_ptr() as *const u8);

    let mut output = vec![0u8; pixels * bpp];
    let mut i = 0;

    while i + pixels_per_lane <= pixels {
        let loaded = vld1q_u8(data.as_ptr().add(i * bpp));
        let shuffled = vqtbl1q_u8(loaded, mask_vec);

        let mut temp = [0u8; 16];
        vst1q_u8(temp.as_mut_ptr(), shuffled);

        for b in 0..bpp {
            let src_off = b * pixels_per_lane;
            let dst_off = b * pixels + i;
            output[dst_off..dst_off + pixels_per_lane]
                .copy_from_slice(&temp[src_off..src_off + pixels_per_lane]);
        }

        i += pixels_per_lane;
    }

    // Scalar tail
    for pix in i..pixels {
        for b in 0..bpp {
            output[b * pixels + pix] = data[pix * bpp + b];
        }
    }

    output
}

/// NEON decode implementation, symmetric to [`apply_byte_shuffle_neon_impl`].
#[cfg(all(feature = "simd", target_arch = "aarch64"))]
#[target_feature(enable = "neon")]
unsafe fn undo_byte_shuffle_neon_impl(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    let pixels_per_lane = 16 / bpp;
    let mask = build_decode_mask(bpp);
    let mask_vec = vld1q_u8(mask.as_ptr() as *const u8);

    let mut output = vec![0u8; pixels * bpp];
    let mut i = 0;

    while i + pixels_per_lane <= pixels {
        let mut temp = [0u8; 16];
        for b in 0..bpp {
            let src_off = b * pixels + i;
            let dst_off = b * pixels_per_lane;
            temp[dst_off..dst_off + pixels_per_lane]
                .copy_from_slice(&data[src_off..src_off + pixels_per_lane]);
        }

        let loaded = vld1q_u8(temp.as_ptr());
        let shuffled = vqtbl1q_u8(loaded, mask_vec);
        vst1q_u8(output.as_mut_ptr().add(i * bpp), shuffled);

        i += pixels_per_lane;
    }

    // Scalar tail
    for pix in i..pixels {
        for b in 0..bpp {
            output[pix * bpp + b] = data[b * pixels + pix];
        }
    }

    output
}

/// Reference scalar implementation (encode direction), used both as the
/// non-AVX2/NEON fallback and as the correctness oracle in tests.
#[allow(dead_code)]
fn apply_byte_shuffle_generic_scalar(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    let mut output = vec![0u8; pixels * bpp];
    for b in 0..bpp {
        for p in 0..pixels {
            output[b * pixels + p] = data[p * bpp + b];
        }
    }
    output
}

/// Reference scalar implementation (decode direction).
#[allow(dead_code)]
fn undo_byte_shuffle_generic_scalar(data: &[u8], bpp: usize, pixels: usize) -> Vec<u8> {
    let mut output = vec![0u8; pixels * bpp];
    for b in 0..bpp {
        for p in 0..pixels {
            output[p * bpp + b] = data[b * pixels + p];
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_check(bpp: usize, pixels: usize) {
        let data: Vec<u8> = (0..(pixels * bpp))
            .map(|i| ((i as u64 * 197 + 13) % 256) as u8)
            .collect();

        let scalar_shuffled = apply_byte_shuffle_generic_scalar(&data, bpp, pixels);

        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            let avx2_shuffled = unsafe { apply_byte_shuffle_avx2_impl(&data, bpp, pixels) };
            assert_eq!(
                scalar_shuffled, avx2_shuffled,
                "AVX2 vs scalar shuffle mismatch for bpp={bpp}, pixels={pixels}"
            );

            let avx2_unshuffled =
                unsafe { undo_byte_shuffle_avx2_impl(&avx2_shuffled, bpp, pixels) };
            assert_eq!(
                data, avx2_unshuffled,
                "AVX2 roundtrip failed for bpp={bpp}, pixels={pixels}"
            );
        }

        #[cfg(target_arch = "aarch64")]
        {
            let neon_shuffled = unsafe { apply_byte_shuffle_neon_impl(&data, bpp, pixels) };
            assert_eq!(
                scalar_shuffled, neon_shuffled,
                "NEON vs scalar shuffle mismatch for bpp={bpp}, pixels={pixels}"
            );

            let neon_unshuffled =
                unsafe { undo_byte_shuffle_neon_impl(&neon_shuffled, bpp, pixels) };
            assert_eq!(
                data, neon_unshuffled,
                "NEON roundtrip failed for bpp={bpp}, pixels={pixels}"
            );
        }

        let scalar_unshuffled = undo_byte_shuffle_generic_scalar(&scalar_shuffled, bpp, pixels);
        assert_eq!(
            data, scalar_unshuffled,
            "scalar roundtrip failed for bpp={bpp}, pixels={pixels}"
        );
    }

    #[test]
    fn test_roundtrip_all_bpp_small() {
        for bpp in [2usize, 4, 8, 16] {
            for pixels in [0usize, 1, 2, 3, 7, 8, 9, 15, 16, 17] {
                roundtrip_check(bpp, pixels);
            }
        }
    }

    #[test]
    fn test_roundtrip_all_bpp_large() {
        for bpp in [2usize, 4, 8, 16] {
            for pixels in [32usize, 33, 64, 100, 1000, 4096] {
                roundtrip_check(bpp, pixels);
            }
        }
    }

    #[test]
    fn test_byte_shuffle_bpp2_known_values() {
        // 4 pixels, bpp=2: AoS [h0,l0,h1,l1,h2,l2,h3,l3] -> SoA [h0,h1,h2,h3,l0,l1,l2,l3]
        let data = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let shuffled = apply_byte_shuffle_generic_scalar(&data, 2, 4);
        assert_eq!(
            shuffled,
            vec![0x11, 0x33, 0x55, 0x77, 0x22, 0x44, 0x66, 0x88]
        );
        let unshuffled = undo_byte_shuffle_generic_scalar(&shuffled, 2, 4);
        assert_eq!(unshuffled, data);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_byte_shuffle_simd_matches_scalar_bpp4_odd_size() {
        if !is_x86_feature_detected!("avx2") {
            return;
        }
        // width*height not a multiple of 8 (pixels_per_vec for bpp=4),
        // exercises the scalar tail path.
        let width = 13u32;
        let height = 7u32;
        let pixels = (width * height) as usize;
        let data: Vec<u8> = (0..(pixels * 4)).map(|i| (i % 256) as u8).collect();

        let shuffled = apply_byte_shuffle_simd(&data, 4, width, height).unwrap();
        let scalar_shuffled = apply_byte_shuffle_generic_scalar(&data, 4, pixels);
        assert_eq!(shuffled, scalar_shuffled);

        let unshuffled = undo_byte_shuffle_simd(&shuffled, 4, width, height).unwrap();
        assert_eq!(unshuffled, data);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_byte_shuffle_simd_matches_scalar_bpp4_odd_size() {
        // width*height not a multiple of 4 (pixels_per_lane for bpp=4 on
        // NEON), exercises the scalar tail path.
        let width = 13u32;
        let height = 7u32;
        let pixels = (width * height) as usize;
        let data: Vec<u8> = (0..(pixels * 4)).map(|i| (i % 256) as u8).collect();

        let shuffled = apply_byte_shuffle_simd(&data, 4, width, height).unwrap();
        let scalar_shuffled = apply_byte_shuffle_generic_scalar(&data, 4, pixels);
        assert_eq!(shuffled, scalar_shuffled);

        let unshuffled = undo_byte_shuffle_simd(&shuffled, 4, width, height).unwrap();
        assert_eq!(unshuffled, data);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn test_byte_shuffle_invalid_bpp() {
        let result = apply_byte_shuffle_simd(&[0u8; 9], 3, 3, 1);
        assert!(result.is_err(), "unsupported BPP should error");
    }
}
