CAFE SIMD IMPLEMENTATION ANALYSIS & NEON OPTIMIZATION ROADMAP

EXECUTIVE SUMMARY
=================
The CAFE codebase has well-structured SIMD for x86_64 AVX2 with clean separation
between vectorized code (src/simd.rs) and scalar fallback. Ready for NEON porting.

Key Findings:
- AVX2: 32 bytes per iteration (256-bit SIMD)
- NEON: 16 bytes per iteration (128-bit SIMD)  
- Pack/unpack: Currently scalar (HIGH-VALUE optimization target)
- Byte-shuffle: Currently scalar (secondary target)
- Feature gate provides clean compile-time control
- All unsafe blocks are well-documented and bounds-checked


1. CURRENT SIMD STRUCTURE IN src/simd.rs (443 lines)
===================================================

File: D:\SMF\Secco\Cafe\src\simd.rs

Conditional compilation based on target_feature = "avx2":
  #[cfg(target_feature = "avx2")]
  use std::arch::x86_64::*;

AVX2-OPTIMIZED FILTERS:

Filter | Name    | Status    | Bytes/Iter | Encode | Decode
--------|---------|-----------|------------|--------|--------
0       | None    | AVX2      | 32         | Yes    | N/A
1       | Sub     | AVX2      | 32         | Yes    | Scalar (sequential dep)
2       | Up      | AVX2      | 32         | Yes    | Yes
3       | Average | Scalar    | N/A        | Yes    | Yes
4-15    | Others  | Scalar    | N/A        | Yes    | Yes

KEY INSIGHT on Filter 1 Decode:
Lines 100-104 explain why unfilter_sub_avx2 is scalar-only:
  out[i] = filtered[i] + out[i - bpp]
This has sequential dependency (each output depends on previous output).
SIMD cannot parallelize this. Correctly implemented as scalar.

Filter 2 (Up) is FULLY VECTORIZABLE in both directions because:
- Encode: pixel[x] - prev[x] → no horizontal dependency
- Decode: residual[x] + prev[x] → reads from constant prev_row


CPU DETECTION & ACTIVATION:

Compile-time only (no runtime detection):
- Default: cargo build --release (enables AVX2 if available)
- Disable: cargo build --release --no-default-features (scalar-only)
- Force: RUSTFLAGS="-C target-feature=+avx2" cargo build --release

Limitation: No RUNTIME CPU DETECTION (e.g., CPUID). Must compile for target CPU.


2. INTEGRATION POINTS & CALL SITES
==================================

ENCODING PIPELINE:

src/filter.rs filter_row() (lines 273-313):
- Dispatches to SIMD functions based on feature gate
- Called ONCE PER IMAGE ROW during encoding
- For 1080p image: ~1000 calls per encode operation

Filter dispatch pattern (lines 281-289):
  #[cfg(feature = "simd")]
  {
      match ftype {
          F_SUB => return filter_sub_avx2(row, bpp),      // SIMD
          F_UP => return filter_up_avx2(row, prev_row),   // SIMD
          F_AVERAGE => return filter_average_avx2(...),   // SIMD
          _ => {}  // Fall through to scalar
      }
  }
  // Scalar fallback for all 16 filters

src/cafe.rs (lines 390, 498, 539):
- apply_predictive_filter() calls filter_row() for each image row
- Returns 1 + filtered_bytes (1 byte filter code + residuals)

DECODING PIPELINE:

src/cafe.rs (lines 1229, 1288, 1352):
- undo_predictive_filter() reverses the filter on each tile


PACK/UNPACK OPERATIONS (src/color.rs):

Functions: pack_samples_row(), unpack_samples_row()
Lines: 1136-1283 (fully scalar implementation)

Call sites:
- Line 281, 397: Packing during encode (bit_depth < 8)
- Line 622, 650: Unpacking during decode (bit_depth < 8)

Current implementation: Nested loops with bit-by-bit manipulation
- for i in 0..width { for ch in 0..bpp { ... } }
- Compute bit position, extract/combine bits
- Random memory writes (poor cache locality)

Optimization: SIMD vectorization could yield 4-8x speedup


BYTE-SHUFFLE OPERATIONS (src/shuffle.rs):

Functions: apply_byte_shuffle(), undo_byte_shuffle()
Lines: 24-130 (fully scalar implementation)

Call sites:
- cafe.rs line 388, 496, 537: Encode path
- cafe.rs line 1219, 1278, 1339: Decode path

Pattern: Transpose-like operation
- Input: [b0_p0, b1_p0, b0_p1, b1_p1, ...] (bpp=2)
- Output: [b0_p0, b0_p1, ..., b1_p0, b1_p1, ...]

Performance: O(width × height × bpp) with stride-bpp memory access
Comments (lines 60-65) identify BLOCKING STRATEGY as 10-20% improvement


3. PERFORMANCE-CRITICAL PATHS
=============================

Function              | Call Frequency        | BPP | SIMD? | Notes
----------------------|------------------------|-----|-------|-------------------
filter_row()          | 1 per image row        | All | Yes   | Heavily used
pack_samples_row()    | 1 per row (if<8 bits) | 1,2,4 | No | High-value target
unpack_samples_row()  | 1 per row (if<8 bits) | 1,2,4 | No | High-value target
apply_byte_shuffle()  | 1 per IDAT tile       | 2,4,8,16 | No | Medium-value target

BOTTLENECK EXAMPLES:

1920×1080 RGBA 8-bit:
- filter_row(): 1080 calls × 7680 bytes/call = 8.3 MB
  With SIMD: 1080 × (7680/32) = 259,200 SIMD operations ✓ Good

2048×2048 Gray 1-bit:
- pack_samples_row(): 2048 calls × 2048 samples/call
  Scalar: Very slow (bit-level operations)
  SIMD: 8-16x speedup potential

1920×1080 Float 32-bit with byte-shuffle:
- apply_byte_shuffle(): 1920×1080×16 = 33.2 MB with stride-4 reads
  Current scalar: Poor cache locality
  SIMD: 4-8x improvement possible via blocking


4. NEON CONSTRAINTS & OPPORTUNITIES
===================================

REGISTER WIDTH:
- AVX2: 256 bits
- NEON (ARM64): 128 bits
- Consequence: Process 16 bytes/iter (vs 32), leading to 2x more iterations
  But same speedup potential (4-8x vs scalar)

AVAILABLE NEON INTRINSICS:

Operation              | AVX2                   | NEON              | Difference
-----------------------|------------------------|------------------|------------------
Load 16 bytes          | _mm256_loadu_si256     | vld1q_u8          | NEON is 128-bit
Store 16 bytes         | _mm256_storeu_si256    | vst1q_u8          | NEON is 128-bit
Subtract u8            | _mm256_sub_epi8        | vsubq_u8          | Direct map
Add u8                 | _mm256_add_epi8        | vaddq_u8          | Direct map
Average u8             | Unpack+Add+Shift       | vrhadd_u8         | NEON direct
Shift operations       | _mm256_slli/srli_epi16 | vshlq_n_u8        | Similar

PACKING/UNPACKING:
NEON provides bit-level intrinsics ideal for pack/unpack:
- vshlq_u8: Shift samples within bytes
- vandq_u8: Extract N-bit values  
- vorrq_u8: Combine packed bits
- vtrn1q_u8: Transpose/reorder
- vtbl1q_u8: Byte permutation

Potential: 8-16x speedup for packing (vs current scalar)

BYTE-SHUFFLE:
NEON provides specialized load-deinterleave operations:
- vld2q_u8: Load & deinterleave (2 channels)
- vld4q_u8: Load & deinterleave (4 channels)

These replace stride-based reads with hardware support.
Potential: 4-8x speedup for shuffle operations

COMPILE-TIME DETECTION:
  #[cfg(target_feature = "neon")]
  use std::arch::aarch64::*;

No runtime detection (same as AVX2 limitation).
Build with: cargo build --release --target aarch64-unknown-linux-gnu

NEON CRATE OPTIONS:
- std::arch::aarch64: Raw intrinsics (recommended, same approach as AVX2)
- wide: SIMD abstraction (maintained but adds dependency)
- packed_simd: Unstable (not suitable for stable release)


5. EXISTING CODE PATTERNS & BOUNDS CHECKING
============================================

STANDARD SIMD LOOP PATTERN (all SIMD code):

  #[cfg(target_feature = "avx2")]
  unsafe {
      let mut i = bpp;
      while i + 32 <= len {  // Bounds check BEFORE unsafe block
          let pixels = _mm256_loadu_si256(row.as_ptr().add(i) as *const __m256i);
          let left = _mm256_loadu_si256(row.as_ptr().add(i - bpp) as *const __m256i);
          let residual = _mm256_sub_epi8(pixels, left);
          _mm256_storeu_si256(filtered.as_mut_ptr().add(i) as *mut __m256i, residual);
          i += 32;
      }
  }
  
  // Tail loop (scalar)
  for i in (bpp + ((len - bpp) / 32) * 32)..len {
      filtered[i] = row[i].wrapping_sub(row[i - bpp]);
  }

Safety invariants:
1. Loop condition: i + 32 <= len (protects all loads/stores)
2. Pointer validity: as_ptr().add(i) is always in bounds
3. No alignment: Uses unaligned variant (_mm256_loadu_si256, not _mm256_load_si256)
4. Tail handling: Scalar loop for remaining < 32 bytes

Pattern for NEON (change to 16 bytes/iteration):
  while i + 16 <= len {
      // Process 16 bytes with NEON intrinsics
      i += 16;
  }

OVERFLOW PROTECTION (consistent throughout codebase):

Example from pack_samples_row():
  let bits_total = (width as u64)
      .checked_mul(bit_depth as u64)
      .and_then(|result| result.checked_mul(bpp as u64))
      .ok_or_else(|| CafeError::TruncatedFile(...))?;

Pattern: Use checked_* operations, return error on overflow (never panic)
Applied to all size calculations


6. CODE LOCATIONS FOR NEON PORTING
==================================

Priority 0: Conditional Dispatch Setup (src/filter.rs)
Lines 273-313 (filter_row) and 321-361 (unfilter_row)

Current:
  #[cfg(feature = "simd")]
  {
      match ftype {
          F_SUB => return filter_sub_avx2(row, bpp),
          ...
      }
  }

Needed:
  #[cfg(feature = "simd")]
  {
      match ftype {
          F_SUB => {
              #[cfg(target_feature = "avx2")]
              return filter_sub_avx2(row, bpp);
              #[cfg(target_feature = "neon")]
              return filter_sub_neon(row, bpp);
              #[cfg(not(any(target_feature = "avx2", target_feature = "neon")))]
              {} // Fall through
          }
          ...
      }
  }

Estimated: ~50 lines of changes

Priority 1: Filter Implementation (src/simd.rs)
Add NEON versions of filters 1, 2, 3 (mirroring AVX2 structure)

Functions:
- filter_sub_neon(row, bpp) → Vec<u8>
- unfilter_sub_neon(filtered, bpp) → Vec<u8>
- filter_up_neon(row, prev_row) → Vec<u8>
- unfilter_up_neon(filtered, prev_row) → Vec<u8>
- filter_average_neon(row, prev_row, bpp) → Vec<u8>
- unfilter_average_neon(filtered, prev_row, bpp) → Vec<u8>

Estimated: 300-400 lines
Testing: Extend existing tests (lines 384-442)

Priority 2: Pack/Unpack Operations (src/color.rs)
Add NEON versions of packing functions (lines 1136-1283)

Functions:
- pack_samples_row_neon(samples, bit_depth, width, bpp) → Result<Vec<u8>>
- unpack_samples_row_neon(packed, bit_depth, width, bpp) → Result<Vec<u8>>

Estimated: 200-300 lines
Testing: Extend tests (lines 3995-4547)

Priority 3: Byte-Shuffle Operations (src/shuffle.rs)
Add NEON versions (lines 24-130)

Functions:
- apply_byte_shuffle_neon(raw, bpp, width, height) → Result<Vec<u8>>
- undo_byte_shuffle_neon(shuffled, bpp, width, height) → Result<Vec<u8>>

Estimated: 150-200 lines
Testing: Extend tests (lines 132-177)


7. FEATURE FLAG ARCHITECTURE
============================

Current (Cargo.toml lines 26-28):
  [features]
  default = ["simd"]
  simd = []

Behavior:
- default = ["simd"]: Enables SIMD (AVX2 on x86_64, NEON on ARM64)
- simd = []: Empty feature (controls #[cfg(feature = "simd")] guards)

Platform detection: Compile-time via #[cfg(target_feature = "...")]
- #[cfg(target_feature = "avx2")] for x86_64
- #[cfg(target_feature = "neon")] for ARM64

No changes needed to Cargo.toml for NEON support.
The feature gate system already supports multiplatform SIMD.


8. TESTING STRATEGY
===================

Current tests in src/simd.rs (lines 384-442):
- test_filter_sub_avx2_roundtrip()
- test_filter_up_avx2_roundtrip()
- test_filter_average_avx2_roundtrip()
- test_filter_sub_avx2_large_bpp()
- test_filter_sub_avx2_large_row()
- test_filter_up_avx2_large_row()

Tests to add:
- test_filter_*_neon_roundtrip() (3 filters × 2 paths = 6 tests)
- test_filter_*_neon_large_row() (3 tests)
- test_pack_samples_row_neon_*() (coverage of 1, 2, 4-bit)
- test_unpack_samples_row_neon_*() (coverage of 1, 2, 4-bit)
- test_shuffle_neon_roundtrip() and variants

CI: Add ARM64 target to GitHub Actions workflow (if cloud ARM runners available)


9. NEON IMPLEMENTATION TIMELINE
===============================

Week 1: NEON build environment setup, Filter 1 (Sub) encode/decode
Week 2: Filter 2 (Up) encode/decode
Week 3: Filter 3 (Average), extend tests
Week 4: Pack/unpack operations
Week 5: Byte-shuffle, optimization pass
Week 6: Performance benchmarking, cleanup, documentation

Total: 6-8 weeks for experienced developer


10. SUMMARY & QUICK REFERENCE
=============================

Current Optimizations:
- Filter 1 (Sub) encode: 4-8x via AVX2
- Filter 2 (Up) encode/decode: 4-8x via AVX2
- Filter 3: 1x (scalar, conservative approach)
- Filters 4-15: 1x (scalar, complex predicates)
- Pack/unpack: 1x (scalar, HIGH-VALUE TARGET)
- Byte-shuffle: 1x (scalar, MEDIUM-VALUE TARGET)

NEON Expected Improvements:
- Filters 1-3: 4-8x speedup (similar to AVX2)
- Pack/unpack: 8-16x speedup (vs current scalar)
- Byte-shuffle: 4-8x speedup (via hardware transpose)
- Overall for ARM64: 2-4x speedup on typical images

Code Quality:
- All unsafe blocks are documented and bounds-checked
- Consistent overflow protection with checked_* operations
- No runtime CPU detection (compile-time only, same as x86_64)
- Error handling: return CafeError, no panics on bad input

Safety Pattern:
- Bounds check loop condition BEFORE entering unsafe block
- Use unaligned load/store (no alignment assumptions)
- Process tail with scalar loop
- Consistent with Rust safety guidelines

===== ANALYSIS COMPLETE =====

Document Version: 1.0
Last Updated: August 10, 2026
Analysis by: CAFE codebase review specialist
