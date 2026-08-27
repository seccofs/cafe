//! SIMD (AVX2) nearest-palette-entry search for indexed-color quantization.
//!
//! `quantize_nearest_neighbor` (in `cafe.rs`) builds the palette
//! *incrementally*: each pixel is matched against the palette as it exists
//! so far, and may extend it before the next pixel is processed. This rules
//! out vectorizing across multiple *pixels* at once (a later pixel in the
//! same batch could depend on a palette entry an earlier pixel in that same
//! batch just added). Instead, this module vectorizes across *palette
//! entries* for a single pixel: comparing 8 entries per AVX2 iteration and
//! reducing to the minimum distance, which is always correct regardless of
//! how the palette grows between pixels.
//!
//! `quantize_median_cut_wrapper`'s second pass (mapping pixels to a
//! *already-built, fixed* palette) also benefits: the [`PaletteSoa`] is
//! built once, then reused across all pixels.
//!
//! # Packed-key reduction
//! Instead of tracking `(best_dist, best_idx)` as two parallel lanes and
//! reconciling them after a horizontal reduction (fiddly to get tie-breaking
//! right across SIMD lanes), each candidate is packed into a single `i32`
//! key: `key = (dist << 8) | idx`. Because `idx` fits in 8 bits (palette
//! index limit) and is strictly less than 256, `dist * 256` never overlaps
//! with the `idx` contribution — comparing packed keys is *exactly*
//! equivalent to comparing `(dist, idx)` lexicographically (smallest
//! distance wins; ties broken by smallest index), matching the scalar
//! reference's first-index-wins semantics. This is not just empirically
//! verified (see tests) but follows directly from the packing bounds: max
//! RGBA squared distance is `4 * 255^2 = 260100 < 2^19`, so `dist << 8`
//! stays well within `i32`'s positive range even after OR-ing in `idx`.
//!
//! # Dispatch
//! AVX2 support is detected **at runtime** via `is_x86_feature_detected!`;
//! on CPUs without AVX2 (or non-x86_64 targets), the scalar fallback is used
//! automatically. No special build flags are required.

#![allow(dead_code)]

use crate::types::PaletteEntry;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Structure-of-arrays view of a palette's color channels, enabling
/// AVX2-vectorized nearest-neighbor search across entries. Built once and
/// reused (or incrementally extended via [`PaletteSoa::push`]) so the O(n)
/// AoS→SoA transposition cost is paid at most once per palette entry, not
/// once per pixel comparison.
#[derive(Default)]
pub struct PaletteSoa {
    r: Vec<u8>,
    g: Vec<u8>,
    b: Vec<u8>,
    a: Vec<u8>,
}

impl PaletteSoa {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: &[PaletteEntry]) -> Self {
        let mut soa = Self {
            r: Vec::with_capacity(entries.len()),
            g: Vec::with_capacity(entries.len()),
            b: Vec::with_capacity(entries.len()),
            a: Vec::with_capacity(entries.len()),
        };
        for e in entries {
            soa.push(e);
        }
        soa
    }

    #[inline]
    pub fn push(&mut self, entry: &PaletteEntry) {
        self.r.push(entry.r);
        self.g.push(entry.g);
        self.b.push(entry.b);
        self.a.push(entry.a);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.r.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.r.is_empty()
    }

    /// Finds the closest palette entry to `(r, g, b, a)` by squared RGBA
    /// Euclidean distance, matching `PaletteEntry::distance_squared` /
    /// `quantize_nearest_neighbor`'s inline formula bit-for-bit (first
    /// index wins on exact ties). Returns `(index, squared_distance)`.
    ///
    /// Returns `(0, u32::MAX)` if the palette is empty (mirrors the scalar
    /// loop's initial "not found" state; callers must handle empty
    /// palettes themselves, same as before this module existed).
    pub fn find_closest_rgba(&self, r: u8, g: u8, b: u8, a: u8) -> (u8, u32) {
        if self.is_empty() {
            return (0, u32::MAX);
        }

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && self.len() >= 8 {
                return unsafe {
                    find_closest_rgba_avx2(r, g, b, a, &self.r, &self.g, &self.b, &self.a)
                };
            }
        }

        find_closest_rgba_scalar(r, g, b, a, &self.r, &self.g, &self.b, &self.a)
    }

    /// Finds the closest palette entry to `(r, g, b)` by squared RGB-only
    /// Euclidean distance (alpha ignored), matching
    /// `quantize_median_cut_wrapper`'s inline formula bit-for-bit
    /// (first index wins on exact ties). Returns `(index, squared_distance)`.
    pub fn find_closest_rgb(&self, r: u8, g: u8, b: u8) -> (u8, u32) {
        if self.is_empty() {
            return (0, u32::MAX);
        }

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && self.len() >= 8 {
                return unsafe { find_closest_rgb_avx2(r, g, b, &self.r, &self.g, &self.b) };
            }
        }

        find_closest_rgb_scalar(r, g, b, &self.r, &self.g, &self.b)
    }
}

#[allow(clippy::too_many_arguments)]
fn find_closest_rgba_scalar(
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    r_arr: &[u8],
    g_arr: &[u8],
    b_arr: &[u8],
    a_arr: &[u8],
) -> (u8, u32) {
    let (r, g, b, a) = (r as i32, g as i32, b as i32, a as i32);
    let mut best_idx = 0u8;
    let mut best_dist = u32::MAX;
    for i in 0..r_arr.len() {
        let dr = r - r_arr[i] as i32;
        let dg = g - g_arr[i] as i32;
        let db = b - b_arr[i] as i32;
        let da = a - a_arr[i] as i32;
        let dist = (dr * dr + dg * dg + db * db + da * da) as u32;
        if dist < best_dist {
            best_dist = dist;
            best_idx = i as u8;
        }
    }
    (best_idx, best_dist)
}

fn find_closest_rgb_scalar(
    r: u8,
    g: u8,
    b: u8,
    r_arr: &[u8],
    g_arr: &[u8],
    b_arr: &[u8],
) -> (u8, u32) {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let mut best_idx = 0u8;
    let mut best_dist = u32::MAX;
    for i in 0..r_arr.len() {
        let dr = r - r_arr[i] as i32;
        let dg = g - g_arr[i] as i32;
        let db = b - b_arr[i] as i32;
        let dist = (dr * dr + dg * dg + db * db) as u32;
        if dist < best_dist {
            best_dist = dist;
            best_idx = i as u8;
        }
    }
    (best_idx, best_dist)
}

/// # Safety
/// Caller must ensure the AVX2 target feature is available at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn find_closest_rgba_avx2(
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    r_arr: &[u8],
    g_arr: &[u8],
    b_arr: &[u8],
    a_arr: &[u8],
) -> (u8, u32) {
    let n = r_arr.len();
    let vr = _mm256_set1_epi32(r as i32);
    let vg = _mm256_set1_epi32(g as i32);
    let vb = _mm256_set1_epi32(b as i32);
    let va = _mm256_set1_epi32(a as i32);
    let lane_offsets = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);

    let mut best_key = _mm256_set1_epi32(i32::MAX);

    let mut i = 0usize;
    while i + 8 <= n {
        let ver = _mm256_cvtepu8_epi32(_mm_loadl_epi64(r_arr.as_ptr().add(i) as *const __m128i));
        let veg = _mm256_cvtepu8_epi32(_mm_loadl_epi64(g_arr.as_ptr().add(i) as *const __m128i));
        let veb = _mm256_cvtepu8_epi32(_mm_loadl_epi64(b_arr.as_ptr().add(i) as *const __m128i));
        let vea = _mm256_cvtepu8_epi32(_mm_loadl_epi64(a_arr.as_ptr().add(i) as *const __m128i));

        let dr = _mm256_sub_epi32(vr, ver);
        let dg = _mm256_sub_epi32(vg, veg);
        let db = _mm256_sub_epi32(vb, veb);
        let da = _mm256_sub_epi32(va, vea);

        // Max: 4 * 255^2 = 260100, well within i32 range, no overflow.
        let dist = _mm256_add_epi32(
            _mm256_add_epi32(_mm256_mullo_epi32(dr, dr), _mm256_mullo_epi32(dg, dg)),
            _mm256_add_epi32(_mm256_mullo_epi32(db, db), _mm256_mullo_epi32(da, da)),
        );

        let idx_vec = _mm256_add_epi32(_mm256_set1_epi32(i as i32), lane_offsets);
        let key = _mm256_or_si256(_mm256_slli_epi32(dist, 8), idx_vec);
        best_key = _mm256_min_epi32(best_key, key);

        i += 8;
    }

    let mut best_key_scalar = horizontal_min_epi32(best_key) as u32;

    // Scalar tail (< 8 remaining entries).
    while i < n {
        let dr = r as i32 - r_arr[i] as i32;
        let dg = g as i32 - g_arr[i] as i32;
        let db = b as i32 - b_arr[i] as i32;
        let da = a as i32 - a_arr[i] as i32;
        let dist = (dr * dr + dg * dg + db * db + da * da) as u32;
        let key = (dist << 8) | (i as u32);
        if key < best_key_scalar {
            best_key_scalar = key;
        }
        i += 1;
    }

    unpack_key(best_key_scalar)
}

/// # Safety
/// Caller must ensure the AVX2 target feature is available at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_closest_rgb_avx2(
    r: u8,
    g: u8,
    b: u8,
    r_arr: &[u8],
    g_arr: &[u8],
    b_arr: &[u8],
) -> (u8, u32) {
    let n = r_arr.len();
    let vr = _mm256_set1_epi32(r as i32);
    let vg = _mm256_set1_epi32(g as i32);
    let vb = _mm256_set1_epi32(b as i32);
    let lane_offsets = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);

    let mut best_key = _mm256_set1_epi32(i32::MAX);

    let mut i = 0usize;
    while i + 8 <= n {
        let ver = _mm256_cvtepu8_epi32(_mm_loadl_epi64(r_arr.as_ptr().add(i) as *const __m128i));
        let veg = _mm256_cvtepu8_epi32(_mm_loadl_epi64(g_arr.as_ptr().add(i) as *const __m128i));
        let veb = _mm256_cvtepu8_epi32(_mm_loadl_epi64(b_arr.as_ptr().add(i) as *const __m128i));

        let dr = _mm256_sub_epi32(vr, ver);
        let dg = _mm256_sub_epi32(vg, veg);
        let db = _mm256_sub_epi32(vb, veb);

        // Max: 3 * 255^2 = 195075, well within i32 range, no overflow.
        let dist = _mm256_add_epi32(
            _mm256_mullo_epi32(dr, dr),
            _mm256_add_epi32(_mm256_mullo_epi32(dg, dg), _mm256_mullo_epi32(db, db)),
        );

        let idx_vec = _mm256_add_epi32(_mm256_set1_epi32(i as i32), lane_offsets);
        let key = _mm256_or_si256(_mm256_slli_epi32(dist, 8), idx_vec);
        best_key = _mm256_min_epi32(best_key, key);

        i += 8;
    }

    let mut best_key_scalar = horizontal_min_epi32(best_key) as u32;

    // Scalar tail (< 8 remaining entries).
    while i < n {
        let dr = r as i32 - r_arr[i] as i32;
        let dg = g as i32 - g_arr[i] as i32;
        let db = b as i32 - b_arr[i] as i32;
        let dist = (dr * dr + dg * dg + db * db) as u32;
        let key = (dist << 8) | (i as u32);
        if key < best_key_scalar {
            best_key_scalar = key;
        }
        i += 1;
    }

    unpack_key(best_key_scalar)
}

#[inline]
fn unpack_key(key: u32) -> (u8, u32) {
    ((key & 0xFF) as u8, key >> 8)
}

/// Horizontal minimum of 8 packed `i32` keys (all non-negative in practice,
/// so signed `_mm256_min_epi32` behaves identically to an unsigned min).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn horizontal_min_epi32(v: __m256i) -> i32 {
    let hi = _mm256_extracti128_si256(v, 1);
    let lo = _mm256_castsi256_si128(v);
    let min128 = _mm_min_epi32(lo, hi);
    let shuf1 = _mm_shuffle_epi32(min128, 0b01_00_11_10); // swap 64-bit halves
    let min64 = _mm_min_epi32(min128, shuf1);
    let shuf2 = _mm_shuffle_epi32(min64, 0b00_00_00_01); // swap adjacent 32-bit
    let min32 = _mm_min_epi32(min64, shuf2);
    _mm_cvtsi128_si32(min32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(r: u8, g: u8, b: u8, a: u8) -> PaletteEntry {
        PaletteEntry { r, g, b, a }
    }

    fn scalar_rgba_reference(r: u8, g: u8, b: u8, a: u8, palette: &[PaletteEntry]) -> (u8, u32) {
        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;
        for (i, e) in palette.iter().enumerate() {
            let dist = ((r as i32 - e.r as i32).pow(2)
                + (g as i32 - e.g as i32).pow(2)
                + (b as i32 - e.b as i32).pow(2)
                + (a as i32 - e.a as i32).pow(2)) as u32;
            if dist < best_dist {
                best_dist = dist;
                best_idx = i as u8;
            }
        }
        (best_idx, best_dist)
    }

    fn scalar_rgb_reference(r: u8, g: u8, b: u8, palette: &[PaletteEntry]) -> (u8, u32) {
        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;
        for (i, e) in palette.iter().enumerate() {
            let dist = ((r as i32 - e.r as i32).pow(2)
                + (g as i32 - e.g as i32).pow(2)
                + (b as i32 - e.b as i32).pow(2)) as u32;
            if dist < best_dist {
                best_dist = dist;
                best_idx = i as u8;
            }
        }
        (best_idx, best_dist)
    }

    #[test]
    fn test_empty_palette() {
        let soa = PaletteSoa::new();
        assert_eq!(soa.find_closest_rgba(1, 2, 3, 4), (0, u32::MAX));
        assert_eq!(soa.find_closest_rgb(1, 2, 3), (0, u32::MAX));
    }

    #[test]
    fn test_single_exact_match() {
        let entries = vec![
            entry(0, 0, 0, 255),
            entry(255, 0, 0, 255),
            entry(0, 255, 0, 255),
        ];
        let soa = PaletteSoa::from_entries(&entries);
        assert_eq!(soa.find_closest_rgba(255, 0, 0, 255), (1, 0));
    }

    #[test]
    fn test_find_closest_rgba_matches_scalar_reference_various_sizes() {
        for &palette_len in &[1usize, 2, 7, 8, 9, 15, 16, 17, 100, 200, 255, 256] {
            let entries: Vec<PaletteEntry> = (0..palette_len)
                .map(|i| {
                    let v = (i * 97) as u32;
                    entry(
                        (v & 0xFF) as u8,
                        ((v >> 3) & 0xFF) as u8,
                        ((v >> 5) & 0xFF) as u8,
                        ((v >> 1) & 0xFF) as u8,
                    )
                })
                .collect();
            let soa = PaletteSoa::from_entries(&entries);

            for trial in 0..50u32 {
                let r = ((trial * 37) % 256) as u8;
                let g = ((trial * 53 + 11) % 256) as u8;
                let b = ((trial * 71 + 23) % 256) as u8;
                let a = ((trial * 13 + 5) % 256) as u8;

                let result = soa.find_closest_rgba(r, g, b, a);
                let expected = scalar_rgba_reference(r, g, b, a, &entries);
                assert_eq!(
                    result, expected,
                    "mismatch for palette_len={palette_len}, pixel=({r},{g},{b},{a})"
                );
            }
        }
    }

    #[test]
    fn test_find_closest_rgb_matches_scalar_reference_various_sizes() {
        for &palette_len in &[1usize, 2, 7, 8, 9, 15, 16, 17, 100, 200, 255, 256] {
            let entries: Vec<PaletteEntry> = (0..palette_len)
                .map(|i| {
                    let v = (i * 61) as u32;
                    entry(
                        (v & 0xFF) as u8,
                        ((v >> 2) & 0xFF) as u8,
                        ((v >> 4) & 0xFF) as u8,
                        255,
                    )
                })
                .collect();
            let soa = PaletteSoa::from_entries(&entries);

            for trial in 0..50u32 {
                let r = ((trial * 41) % 256) as u8;
                let g = ((trial * 59 + 7) % 256) as u8;
                let b = ((trial * 83 + 17) % 256) as u8;

                let result = soa.find_closest_rgb(r, g, b);
                let expected = scalar_rgb_reference(r, g, b, &entries);
                assert_eq!(
                    result, expected,
                    "mismatch for palette_len={palette_len}, pixel=({r},{g},{b})"
                );
            }
        }
    }

    #[test]
    fn test_tie_break_first_index_wins_with_duplicates() {
        let entries = vec![
            entry(10, 10, 10, 255),
            entry(200, 200, 200, 255),
            entry(10, 10, 10, 255), // exact duplicate of index 0
        ];
        let soa = PaletteSoa::from_entries(&entries);
        assert_eq!(soa.find_closest_rgba(10, 10, 10, 255), (0, 0));
    }

    #[test]
    fn test_tie_break_across_simd_boundary() {
        // Duplicate placed right at a SIMD-chunk boundary (index 8, first
        // entry of the second chunk) and again in the scalar tail, to
        // exercise cross-chunk and chunk/tail tie-breaking.
        let mut entries = vec![entry(1, 1, 1, 1); 20];
        entries[8] = entry(50, 50, 50, 50);
        entries[17] = entry(50, 50, 50, 50); // duplicate in scalar tail (16..20)
        let soa = PaletteSoa::from_entries(&entries);
        assert_eq!(soa.find_closest_rgba(50, 50, 50, 50), (8, 0));
    }

    #[test]
    fn test_incremental_push_matches_from_entries() {
        let entries: Vec<PaletteEntry> = (0..30)
            .map(|i| entry((i * 7) as u8, (i * 3) as u8, (i * 11) as u8, 200))
            .collect();

        let mut soa = PaletteSoa::new();
        for e in &entries {
            soa.push(e);
        }
        let soa_from_entries = PaletteSoa::from_entries(&entries);

        for trial in 0..20u32 {
            let r = ((trial * 29) % 256) as u8;
            let g = ((trial * 31) % 256) as u8;
            let b = ((trial * 37) % 256) as u8;
            let a = ((trial * 43) % 256) as u8;
            assert_eq!(
                soa.find_closest_rgba(r, g, b, a),
                soa_from_entries.find_closest_rgba(r, g, b, a)
            );
        }
    }

    #[test]
    fn test_max_palette_size_256_exhaustive_index_coverage() {
        // A palette where distance to pixel (0,0,0,0) increases strictly
        // with index guarantees the correct answer is always index 0,
        // stressing the full 256-entry sweep (32 SIMD iterations) without
        // any tie-break ambiguity.
        let entries: Vec<PaletteEntry> = (0..256).map(|i| entry(i as u8, 0, 0, 0)).collect();
        let soa = PaletteSoa::from_entries(&entries);
        assert_eq!(soa.find_closest_rgba(0, 0, 0, 0), (0, 0));
        assert_eq!(soa.find_closest_rgba(255, 0, 0, 0), (255, 0));
        assert_eq!(soa.find_closest_rgba(128, 0, 0, 0), (128, 0));
    }
}
