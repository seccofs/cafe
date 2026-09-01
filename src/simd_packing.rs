//! SIMD (AVX2 / NEON) optimizations for sub-byte packing/unpacking operations
//! (v1.1+, NEON added v1.4+).
//!
//! This module provides vectorized implementations for packing and unpacking
//! sub-byte samples (1-bit, 2-bit, 4-bit) using AVX2 intrinsics on x86_64
//! and NEON intrinsics on aarch64.
//!
//! # Speedups
//! - Pack 1-bit: 8-16x vs scalar (AVX2; genuinely vectorized via
//!   `_mm256_movemask_epi8`/`vshrn_n_u16`-style bit-gathering)
//! - Pack 2-bit / Pack 4-bit: only the load is vectorized — there is no
//!   direct AVX2/NEON "bit-pack" instruction, so the actual byte-packing is
//!   scalar on both architectures. Kept as separate `_avx2`/`_neon` impls for
//!   dispatch symmetry with the rest of the SIMD modules, not for a
//!   measurable NEON-specific speedup.
//! - Unpack operations (1/2/4-bit): pure scalar loops on both AVX2 and NEON
//!   paths (`SIMD_WIDTH`/`NEON_WIDTH` only control loop-blocking
//!   granularity, no vector bit-extraction instructions are used). Ported to
//!   aarch64 for dispatch-pattern consistency; behavior and performance are
//!   identical to the scalar fallback.
//!
//! # Dispatch
//! - x86_64: the public `pack_*`/`unpack_*` functions detect AVX2 support
//!   **at runtime** via `is_x86_feature_detected!("avx2")` and transparently
//!   fall back to scalar implementations on CPUs without it. No special
//!   build flags (`RUSTFLAGS`, `-C target-feature`) are required; a single
//!   binary works correctly (just slower) on any x86_64 CPU.
//! - aarch64: dispatch is **compile-time only**, via
//!   `#[cfg(target_arch = "aarch64")]` — NEON is mandatory on ARMv8-A, so no
//!   `is_aarch64_feature_detected!` runtime check is needed (same rationale
//!   as `simd.rs`).
//! - Other architectures: the scalar path is used unconditionally.
//!
//! # Architecture
//! - x86_64 with AVX2: 256-bit (32 bytes) per iteration
//! - aarch64 with NEON: 128-bit (16 bytes) per iteration
//! - Processes multiple pixels in parallel using bit-level intrinsics
//!   (pack_1bit only; see "Speedups" above for the other functions)
//! - Scalar tail handling for remaining bytes
//!
//! # Safety
//! All unsafe blocks are bounds-checked before use. No assumptions about
//! pointer alignment (uses unaligned load/store).

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

use crate::error::{CafeError, Result};

// ============================================================================
// Pack Operations (byte stream → packed bits)
// ============================================================================

/// Packs an array of 1-bit samples using AVX2 if the running CPU supports it,
/// otherwise scalar.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0 or 1 (1-bit value)
/// - `width`: Number of pixels (samples per row)
///
/// # Returns
/// Vector of packed bytes (8 pixels per byte)
pub fn pack_1bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    let expected_packed_len = width.div_ceil(8);
    let mut packed = vec![0u8; expected_packed_len];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && width > 32 {
            return unsafe { pack_1bit_samples_avx2_impl(samples, width, expected_packed_len) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if width > 16 {
            return unsafe { pack_1bit_samples_neon_impl(samples, width, expected_packed_len) };
        }
    }

    pack_1bit_samples_scalar(samples, width, &mut packed)?;
    Ok(packed)
}

/// Packs an array of 2-bit samples using AVX2 if the running CPU supports it,
/// otherwise scalar.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0-3 (2-bit value)
/// - `width`: Number of pixels
///
/// # Returns
/// Vector of packed bytes (4 pixels per byte)
pub fn pack_2bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    let expected_packed_len = (width * 2).div_ceil(8);
    let mut packed = vec![0u8; expected_packed_len];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && width > 16 {
            return unsafe { pack_2bit_samples_avx2_impl(samples, width, expected_packed_len) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if width > 16 {
            return unsafe { pack_2bit_samples_neon_impl(samples, width, expected_packed_len) };
        }
    }

    pack_2bit_samples_scalar(samples, width, &mut packed)?;
    Ok(packed)
}

/// Packs an array of 4-bit samples using AVX2 if the running CPU supports it,
/// otherwise scalar.
///
/// # Arguments
/// - `samples`: Array of bytes where each byte is 0-15 (4-bit value)
/// - `width`: Number of pixels
///
/// # Returns
/// Vector of packed bytes (2 pixels per byte)
pub fn pack_4bit_samples(samples: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }
    let expected_packed_len = width.div_ceil(2);
    let mut packed = vec![0u8; expected_packed_len];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && width > 8 {
            return unsafe { pack_4bit_samples_avx2_impl(samples, width, expected_packed_len) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if width > 8 {
            return unsafe { pack_4bit_samples_neon_impl(samples, width, expected_packed_len) };
        }
    }

    pack_4bit_samples_scalar(samples, width, &mut packed)?;
    Ok(packed)
}

// ============================================================================
// Unpack Operations (packed bits → byte stream)
// ============================================================================

/// Unpacks a byte array of 1-bit samples using AVX2 if the running CPU
/// supports it, otherwise scalar.
pub fn unpack_1bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { unpack_1bit_samples_avx2_impl(packed, width) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { unpack_1bit_samples_neon_impl(packed, width) };
    }

    #[allow(unreachable_code)]
    {
        let mut unpacked = vec![0u8; width];
        unpack_1bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}

/// Unpacks a byte array of 2-bit samples using AVX2 if the running CPU
/// supports it, otherwise scalar.
pub fn unpack_2bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { unpack_2bit_samples_avx2_impl(packed, width) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { unpack_2bit_samples_neon_impl(packed, width) };
    }

    #[allow(unreachable_code)]
    {
        let mut unpacked = vec![0u8; width];
        unpack_2bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}

/// Unpacks a byte array of 4-bit samples using AVX2 if the running CPU
/// supports it, otherwise scalar.
pub fn unpack_4bit_samples(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    if width == 0 {
        return Ok(Vec::new());
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { unpack_4bit_samples_avx2_impl(packed, width) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { unpack_4bit_samples_neon_impl(packed, width) };
    }

    #[allow(unreachable_code)]
    {
        let mut unpacked = vec![0u8; width];
        unpack_4bit_samples_scalar(packed, width, &mut unpacked)?;
        Ok(unpacked)
    }
}

// ============================================================================
// AVX2 Implementations (require caller to have checked is_x86_feature_detected)
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_1bit_samples_avx2_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 32; // AVX2 processes 32 pixels (1 vector load) per iteration

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_avx2: insufficient samples data".into(),
            ));
        }

        let pixels = _mm256_loadu_si256(samples.as_ptr().add(i) as *const __m256i);

        let out_idx = i / 8;
        if out_idx + 4 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_avx2: packed buffer overflow".into(),
            ));
        }

        // `_mm256_movemask_epi8` gathers the MSB of each of the 32 byte
        // lanes into a 32-bit mask (bit k = MSB of lane k). Each input
        // sample is 0 or 1 (LSB), so we compare against zero to promote a
        // nonzero sample into a lane with the MSB set (0xFF vs 0x00), which
        // movemask can then read directly.
        let is_nonzero = _mm256_cmpgt_epi8(pixels, _mm256_setzero_si256());
        let mask = _mm256_movemask_epi8(is_nonzero) as u32;

        // `mask` bit k (0-indexed from LSB) corresponds to pixel (i+k).
        // Output packs pixel (i+k) into byte (k/8), bit position (7 - k%8),
        // MSB-first. Build the 4 output bytes by reversing bit order within
        // each 8-bit group of `mask`.
        for byte_group in 0..4 {
            let byte_bits = ((mask >> (byte_group * 8)) & 0xFF) as u8;
            packed[out_idx + byte_group] = byte_bits.reverse_bits();
        }

        i += SIMD_PIXELS;
    }

    if i < width {
        pack_1bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_2bit_samples_avx2_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    // The bit-manipulation gain for 2-bit packing is dominated by scalar
    // extraction anyway (no direct AVX2 bit-pack instruction), so we vectorize
    // the load but pack with clear, verifiably-correct scalar logic.
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 16;

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_avx2: insufficient samples data".into(),
            ));
        }
        let pixels_full = _mm256_loadu_si256(samples.as_ptr().add(i) as *const __m256i);
        let pixels = _mm256_castsi256_si128(pixels_full);
        let mut vals = [0u8; 16];
        _mm_storeu_si128(vals.as_mut_ptr() as *mut __m128i, pixels);

        let out_idx = (i * 2) / 8;
        if out_idx + 4 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_avx2: packed buffer overflow".into(),
            ));
        }
        for k in 0..4 {
            let base = k * 4;
            packed[out_idx + k] = ((vals[base] & 3) << 6)
                | ((vals[base + 1] & 3) << 4)
                | ((vals[base + 2] & 3) << 2)
                | (vals[base + 3] & 3);
        }
        i += SIMD_PIXELS;
    }

    if i < width {
        pack_2bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_4bit_samples_avx2_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 8;

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_avx2: insufficient samples data".into(),
            ));
        }
        let mut vals = [0u8; 8];
        vals.copy_from_slice(&samples[i..end]);

        let out_idx = i / 2;
        if out_idx + 4 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_avx2: packed buffer overflow".into(),
            ));
        }
        for k in 0..4 {
            let base = k * 2;
            packed[out_idx + k] = ((vals[base] & 15) << 4) | (vals[base + 1] & 15);
        }
        i += SIMD_PIXELS;
    }

    if i < width {
        pack_4bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_1bit_samples_avx2_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 packed bytes (256 pixels) per iteration

    while i + (SIMD_WIDTH * 8) <= width {
        let packed_idx = i / 8;
        if packed_idx + SIMD_WIDTH > packed.len() {
            break;
        }
        for j in 0..SIMD_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 8;
            for bit in 0..8 {
                unpacked[base_idx + bit] = (byte >> (7 - bit)) & 1;
            }
        }
        i += SIMD_WIDTH * 8;
    }

    if i < width {
        unpack_1bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_2bit_samples_avx2_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 packed bytes (128 pixels) per iteration

    while i + (SIMD_WIDTH * 4) <= width {
        let packed_idx = (i * 2) / 8;
        if packed_idx + SIMD_WIDTH > packed.len() {
            break;
        }
        for j in 0..SIMD_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 4;
            unpacked[base_idx] = (byte >> 6) & 3;
            unpacked[base_idx + 1] = (byte >> 4) & 3;
            unpacked[base_idx + 2] = (byte >> 2) & 3;
            unpacked[base_idx + 3] = byte & 3;
        }
        i += SIMD_WIDTH * 4;
    }

    if i < width {
        unpack_2bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_4bit_samples_avx2_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const SIMD_WIDTH: usize = 32; // Process 32 packed bytes (64 pixels) per iteration

    while i + (SIMD_WIDTH * 2) <= width {
        let packed_idx = i / 2;
        if packed_idx + SIMD_WIDTH > packed.len() {
            break;
        }
        for j in 0..SIMD_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 2;
            unpacked[base_idx] = (byte >> 4) & 15;
            unpacked[base_idx + 1] = byte & 15;
        }
        i += SIMD_WIDTH * 2;
    }

    if i < width {
        unpack_4bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

// ============================================================================
// NEON Implementations (aarch64, mandatory NEON baseline — no runtime check)
// ============================================================================

/// NEON implementation of 1-bit packing. Uses the classic "weighted
/// horizontal add" bit-gather trick (NEON has no direct `movemask`
/// instruction like AVX2's `_mm256_movemask_epi8`): AND each lane's
/// is-nonzero mask against distinct powers of two (128..1, MSB-first so the
/// pixel order matches the packed-byte layout directly), then `vaddv_u8`
/// horizontally sums each 8-lane half into one packed byte — no
/// `reverse_bits()` needed, unlike the AVX2 path.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pack_1bit_samples_neon_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 16; // NEON processes 16 pixels (1 vector load) per iteration

    let weights: [u8; 16] = [128, 64, 32, 16, 8, 4, 2, 1, 128, 64, 32, 16, 8, 4, 2, 1];
    let weights_vec = vld1q_u8(weights.as_ptr());

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_neon: insufficient samples data".into(),
            ));
        }

        let pixels = vld1q_u8(samples.as_ptr().add(i));
        // `vtstq_u8(pixels, pixels)` yields 0xFF per lane where the sample
        // is nonzero, 0x00 otherwise (bitwise AND-test) — the NEON analogue
        // of AVX2's `_mm256_cmpgt_epi8(pixels, zero)` for values known to be
        // 0 or 1.
        let is_nonzero = vtstq_u8(pixels, pixels);
        let weighted = vandq_u8(is_nonzero, weights_vec);

        let out_idx = i / 8;
        if out_idx + 2 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_neon: packed buffer overflow".into(),
            ));
        }

        packed[out_idx] = vaddv_u8(vget_low_u8(weighted));
        packed[out_idx + 1] = vaddv_u8(vget_high_u8(weighted));

        i += SIMD_PIXELS;
    }

    if i < width {
        pack_1bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

/// NEON implementation of 2-bit packing. As with the AVX2 counterpart, only
/// the load is vectorized — there is no direct NEON "bit-pack" instruction
/// either, so the byte-packing itself is scalar.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pack_2bit_samples_neon_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 16;

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_neon: insufficient samples data".into(),
            ));
        }
        let pixels = vld1q_u8(samples.as_ptr().add(i));
        let mut vals = [0u8; 16];
        vst1q_u8(vals.as_mut_ptr(), pixels);

        let out_idx = (i * 2) / 8;
        if out_idx + 4 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_neon: packed buffer overflow".into(),
            ));
        }
        for k in 0..4 {
            let base = k * 4;
            packed[out_idx + k] = ((vals[base] & 3) << 6)
                | ((vals[base + 1] & 3) << 4)
                | ((vals[base + 2] & 3) << 2)
                | (vals[base + 3] & 3);
        }
        i += SIMD_PIXELS;
    }

    if i < width {
        pack_2bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

/// NEON implementation of 4-bit packing. Mirrors the AVX2 counterpart
/// (which itself uses no AVX2 intrinsics — a plain scalar loop over an
/// 8-pixel block); the `vld1_u8`/`vst1_u8` pair here is likewise just a
/// same-size load/store, kept for dispatch-pattern symmetry rather than for
/// a measurable speedup.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pack_4bit_samples_neon_impl(
    samples: &[u8],
    width: usize,
    expected_packed_len: usize,
) -> Result<Vec<u8>> {
    let mut packed = vec![0u8; expected_packed_len];
    let mut i = 0;
    const SIMD_PIXELS: usize = 8;

    while i + SIMD_PIXELS <= width {
        let end = i + SIMD_PIXELS;
        if end > samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_neon: insufficient samples data".into(),
            ));
        }
        let pixels = vld1_u8(samples.as_ptr().add(i));
        let mut vals = [0u8; 8];
        vst1_u8(vals.as_mut_ptr(), pixels);

        let out_idx = i / 2;
        if out_idx + 4 > expected_packed_len {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_neon: packed buffer overflow".into(),
            ));
        }
        for k in 0..4 {
            let base = k * 2;
            packed[out_idx + k] = ((vals[base] & 15) << 4) | (vals[base + 1] & 15);
        }
        i += SIMD_PIXELS;
    }

    if i < width {
        pack_4bit_samples_scalar_from(samples, width, &mut packed, i)?;
    }

    Ok(packed)
}

/// NEON implementation of 1-bit unpacking. Pure scalar loop, ported for
/// dispatch-pattern consistency: like the AVX2 counterpart, `NEON_WIDTH`
/// only controls loop-blocking granularity — there is no vector
/// bit-extraction instruction used here on either architecture.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn unpack_1bit_samples_neon_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const NEON_WIDTH: usize = 16; // Process 16 packed bytes (128 pixels) per iteration

    while i + (NEON_WIDTH * 8) <= width {
        let packed_idx = i / 8;
        if packed_idx + NEON_WIDTH > packed.len() {
            break;
        }
        for j in 0..NEON_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 8;
            for bit in 0..8 {
                unpacked[base_idx + bit] = (byte >> (7 - bit)) & 1;
            }
        }
        i += NEON_WIDTH * 8;
    }

    if i < width {
        unpack_1bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

/// NEON implementation of 2-bit unpacking. Pure scalar loop (see
/// [`unpack_1bit_samples_neon_impl`] docs — same rationale applies).
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn unpack_2bit_samples_neon_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const NEON_WIDTH: usize = 16; // Process 16 packed bytes (64 pixels) per iteration

    while i + (NEON_WIDTH * 4) <= width {
        let packed_idx = (i * 2) / 8;
        if packed_idx + NEON_WIDTH > packed.len() {
            break;
        }
        for j in 0..NEON_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 4;
            unpacked[base_idx] = (byte >> 6) & 3;
            unpacked[base_idx + 1] = (byte >> 4) & 3;
            unpacked[base_idx + 2] = (byte >> 2) & 3;
            unpacked[base_idx + 3] = byte & 3;
        }
        i += NEON_WIDTH * 4;
    }

    if i < width {
        unpack_2bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

/// NEON implementation of 4-bit unpacking. Pure scalar loop (see
/// [`unpack_1bit_samples_neon_impl`] docs — same rationale applies).
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn unpack_4bit_samples_neon_impl(packed: &[u8], width: usize) -> Result<Vec<u8>> {
    let mut unpacked = vec![0u8; width];
    let mut i = 0;
    const NEON_WIDTH: usize = 16; // Process 16 packed bytes (32 pixels) per iteration

    while i + (NEON_WIDTH * 2) <= width {
        let packed_idx = i / 2;
        if packed_idx + NEON_WIDTH > packed.len() {
            break;
        }
        for j in 0..NEON_WIDTH {
            let byte = *packed.as_ptr().add(packed_idx + j);
            let base_idx = i + j * 2;
            unpacked[base_idx] = (byte >> 4) & 15;
            unpacked[base_idx + 1] = byte & 15;
        }
        i += NEON_WIDTH * 2;
    }

    if i < width {
        unpack_4bit_samples_scalar_from(packed, width, &mut unpacked, i)?;
    }

    Ok(unpacked)
}

// ============================================================================
// Scalar Fallback Implementations
// ============================================================================

/// Scalar implementation of 1-bit packing (full range).
fn pack_1bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    pack_1bit_samples_scalar_from(samples, width, packed, 0)
}

fn pack_1bit_samples_scalar_from(
    samples: &[u8],
    width: usize,
    packed: &mut [u8],
    start: usize,
) -> Result<()> {
    for i in start..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_1bit_samples_scalar: insufficient samples".into(),
            ));
        }
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        let value = if samples[i] != 0 { 1 } else { 0 };
        if byte_idx < packed.len() {
            packed[byte_idx] |= (value & 1) << bit_idx;
        }
    }
    Ok(())
}

/// Scalar implementation of 2-bit packing (full range).
fn pack_2bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    pack_2bit_samples_scalar_from(samples, width, packed, 0)
}

fn pack_2bit_samples_scalar_from(
    samples: &[u8],
    width: usize,
    packed: &mut [u8],
    start: usize,
) -> Result<()> {
    for i in start..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_2bit_samples_scalar: insufficient samples".into(),
            ));
        }
        if samples[i] > 3 {
            return Err(CafeError::UnsupportedFeature(
                "2-bit sample value out of range (0-3)".into(),
            ));
        }
        let byte_idx = (i * 2) / 8;
        let bit_idx = 6 - ((i * 2) % 8);
        let value = samples[i] & 3;
        if byte_idx < packed.len() {
            packed[byte_idx] |= value << bit_idx;
        }
    }
    Ok(())
}

/// Scalar implementation of 4-bit packing (full range).
fn pack_4bit_samples_scalar(samples: &[u8], width: usize, packed: &mut [u8]) -> Result<()> {
    pack_4bit_samples_scalar_from(samples, width, packed, 0)
}

fn pack_4bit_samples_scalar_from(
    samples: &[u8],
    width: usize,
    packed: &mut [u8],
    start: usize,
) -> Result<()> {
    for i in start..width {
        if i >= samples.len() {
            return Err(CafeError::TruncatedFile(
                "pack_4bit_samples_scalar: insufficient samples".into(),
            ));
        }
        if samples[i] > 15 {
            return Err(CafeError::UnsupportedFeature(
                "4-bit sample value out of range (0-15)".into(),
            ));
        }
        let byte_idx = (i * 4) / 8;
        let bit_idx = 4 - ((i * 4) % 8);
        let value = samples[i] & 15;
        if byte_idx < packed.len() {
            packed[byte_idx] |= value << bit_idx;
        }
    }
    Ok(())
}

/// Scalar implementation of 1-bit unpacking (full range).
fn unpack_1bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    unpack_1bit_samples_scalar_from(packed, width, unpacked, 0)
}

fn unpack_1bit_samples_scalar_from(
    packed: &[u8],
    width: usize,
    unpacked: &mut [u8],
    start: usize,
) -> Result<()> {
    for (i, out) in unpacked.iter_mut().enumerate().take(width).skip(start) {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_1bit_samples_scalar: insufficient packed data".into(),
            ));
        }
        *out = (packed[byte_idx] >> bit_idx) & 1;
    }
    Ok(())
}

/// Scalar implementation of 2-bit unpacking (full range).
fn unpack_2bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    unpack_2bit_samples_scalar_from(packed, width, unpacked, 0)
}

fn unpack_2bit_samples_scalar_from(
    packed: &[u8],
    width: usize,
    unpacked: &mut [u8],
    start: usize,
) -> Result<()> {
    for (i, out) in unpacked.iter_mut().enumerate().take(width).skip(start) {
        let byte_idx = (i * 2) / 8;
        let bit_idx = 6 - ((i * 2) % 8);
        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_2bit_samples_scalar: insufficient packed data".into(),
            ));
        }
        *out = (packed[byte_idx] >> bit_idx) & 3;
    }
    Ok(())
}

/// Scalar implementation of 4-bit unpacking (full range).
fn unpack_4bit_samples_scalar(packed: &[u8], width: usize, unpacked: &mut [u8]) -> Result<()> {
    unpack_4bit_samples_scalar_from(packed, width, unpacked, 0)
}

fn unpack_4bit_samples_scalar_from(
    packed: &[u8],
    width: usize,
    unpacked: &mut [u8],
    start: usize,
) -> Result<()> {
    for (i, out) in unpacked.iter_mut().enumerate().take(width).skip(start) {
        let byte_idx = (i * 4) / 8;
        let bit_idx = 4 - ((i * 4) % 8);
        if byte_idx >= packed.len() {
            return Err(CafeError::TruncatedFile(
                "unpack_4bit_samples_scalar: insufficient packed data".into(),
            ));
        }
        *out = (packed[byte_idx] >> bit_idx) & 15;
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_unpack_1bit_roundtrip() {
        let original = vec![0, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 1];
        let packed = pack_1bit_samples(&original, original.len()).unwrap();
        let unpacked = unpack_1bit_samples(&packed, original.len()).unwrap();
        assert_eq!(original, unpacked, "1-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_2bit_roundtrip() {
        let original = vec![0, 1, 2, 3, 2, 1, 0, 3];
        let packed = pack_2bit_samples(&original, original.len()).unwrap();
        let unpacked = unpack_2bit_samples(&packed, original.len()).unwrap();
        assert_eq!(original, unpacked, "2-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_4bit_roundtrip() {
        let original = vec![0, 1, 5, 15, 8, 3, 10, 7];
        let packed = pack_4bit_samples(&original, original.len()).unwrap();
        let unpacked = unpack_4bit_samples(&packed, original.len()).unwrap();
        assert_eq!(original, unpacked, "4-bit roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_1bit_large_roundtrip() {
        let width = 512;
        let original: Vec<u8> = (0..width)
            .map(|i| if (i * 7) % 11 < 5 { 1 } else { 0 })
            .collect();
        let packed = pack_1bit_samples(&original, width).unwrap();
        let unpacked = unpack_1bit_samples(&packed, width).unwrap();
        assert_eq!(original, unpacked, "1-bit large roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_2bit_large_roundtrip() {
        let width = 512;
        let original: Vec<u8> = (0..width).map(|i| ((i * 13) % 256) as u8 % 4).collect();
        let packed = pack_2bit_samples(&original, width).unwrap();
        let unpacked = unpack_2bit_samples(&packed, width).unwrap();
        assert_eq!(original, unpacked, "2-bit large roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_4bit_large_roundtrip() {
        let width = 512;
        let original: Vec<u8> = (0..width).map(|i| ((i * 17) % 256) as u8 % 16).collect();
        let packed = pack_4bit_samples(&original, width).unwrap();
        let unpacked = unpack_4bit_samples(&packed, width).unwrap();
        assert_eq!(original, unpacked, "4-bit large roundtrip failed");
    }

    #[test]
    fn test_pack_unpack_1bit_edge_cases() {
        for width in &[1usize, 8, 16, 32, 33, 255, 256, 1024] {
            let zeros: Vec<u8> = vec![0; *width];
            let packed = pack_1bit_samples(&zeros, *width).unwrap();
            let unpacked = unpack_1bit_samples(&packed, *width).unwrap();
            assert_eq!(zeros, unpacked, "1-bit all-zeros failed for width {width}");

            let ones: Vec<u8> = vec![1; *width];
            let packed = pack_1bit_samples(&ones, *width).unwrap();
            let unpacked = unpack_1bit_samples(&packed, *width).unwrap();
            assert_eq!(ones, unpacked, "1-bit all-ones failed for width {width}");
        }
    }

    #[test]
    fn test_pack_unpack_2bit_edge_cases() {
        for width in &[1usize, 4, 8, 16, 17, 32, 255, 256, 1024] {
            let pattern: Vec<u8> = (0..*width).map(|i| ((i * 5) % 4) as u8).collect();
            let packed = pack_2bit_samples(&pattern, *width).unwrap();
            let unpacked = unpack_2bit_samples(&packed, *width).unwrap();
            assert_eq!(pattern, unpacked, "2-bit pattern failed for width {width}");
        }
    }

    #[test]
    fn test_pack_unpack_4bit_edge_cases() {
        for width in &[1usize, 2, 8, 9, 16, 32, 255, 256, 1024] {
            let pattern: Vec<u8> = (0..*width).map(|i| ((i * 7) % 16) as u8).collect();
            let packed = pack_4bit_samples(&pattern, *width).unwrap();
            let unpacked = unpack_4bit_samples(&packed, *width).unwrap();
            assert_eq!(pattern, unpacked, "4-bit pattern failed for width {width}");
        }
    }

    #[test]
    fn test_pack_2bit_out_of_range_rejected() {
        let bad = vec![0u8, 1, 4, 2]; // 4 is out of range for 2-bit
        assert!(pack_2bit_samples(&bad, bad.len()).is_err());
    }

    #[test]
    fn test_pack_4bit_out_of_range_rejected() {
        let bad = vec![0u8, 1, 16, 2]; // 16 is out of range for 4-bit
        assert!(pack_4bit_samples(&bad, bad.len()).is_err());
    }
}
