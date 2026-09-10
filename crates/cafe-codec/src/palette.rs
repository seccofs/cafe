//! `PLTE` indexed-color transform (spec section 4.3): encoder-side color
//! quantization is deliberately **not** implemented here — [`build_palette`]
//! only ever maps *exact* pixel values to indices (first-seen order),
//! returning `None` once more than [`cafe_format::constants::
//! MAX_PALETTE_ENTRIES`] distinct colors are found. Real quantization
//! (K-means/MedianCut for images with more distinct colors than the
//! 256-entry ceiling allows) remains deferred per `AGENTS.md`'s Core v0.1
//! design decisions table — this only helps images that already have few
//! enough exact colors.
//!
//! [`Encoder`](crate::encoder::Encoder) never calls [`build_palette`]
//! itself: per `AGENTS.md`'s API decision, the caller pre-computes the
//! palette and indices and passes them in via `EncoderOptions::palette`,
//! keeping `Encoder<W>` a pure streaming sink.

use crate::error::{CodecError, Result};
use cafe_format::constants::MAX_PALETTE_ENTRIES;
use std::collections::HashMap;

/// A built palette: `entries` is the flat list of distinct colors
/// (`channels` bytes each, first-seen order) and `indices` is one byte per
/// input pixel selecting into `entries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Flat entry bytes, `entries.len() / channels as usize` colors.
    pub entries: Vec<u8>,
    /// One index byte per pixel, in the same row-major order as `pixels`.
    pub indices: Vec<u8>,
}

/// Scans `pixels` (flat, `channels` bytes per pixel, row-major) for its
/// distinct exact colors in first-seen order, returning the resulting
/// [`Palette`] if there are at most `MAX_PALETTE_ENTRIES` of them, or
/// `None` if the image has too many distinct colors for a 1-byte index to
/// address (spec section 4.3: `entry_count` is capped at `256`).
///
/// Returns `None` rather than an error: "too many colors for a palette"
/// is an ordinary, expected outcome for photographic content, not a
/// malformed-input condition — callers decide whether to fall back to a
/// direct (non-palette) encode.
pub fn build_palette(pixels: &[u8], channels: u8) -> Option<Palette> {
    let channels = channels as usize;
    if channels == 0 || !pixels.len().is_multiple_of(channels) {
        return None;
    }
    let pixel_count = pixels.len() / channels;

    let mut entries: Vec<u8> = Vec::new();
    let mut seen: HashMap<&[u8], u8> = HashMap::new();
    let mut indices = Vec::with_capacity(pixel_count);

    for i in 0..pixel_count {
        let color = &pixels[i * channels..(i + 1) * channels];
        if let Some(&idx) = seen.get(color) {
            indices.push(idx);
            continue;
        }
        let next_idx = seen.len();
        if next_idx as u32 >= MAX_PALETTE_ENTRIES {
            return None;
        }
        let idx = next_idx as u8;
        entries.extend_from_slice(color);
        // Safety of the borrow: `entries` only ever grows (never shrinks
        // or reallocates-and-drops earlier elements out from under a live
        // slice a HashMap key points into) would be unsound if `entries`
        // reallocated — so key on the *input* `pixels` slice instead,
        // which never moves.
        seen.insert(color, idx);
        indices.push(idx);
    }

    Some(Palette { entries, indices })
}

/// Expands `indices` (one byte per pixel) back into full pixel bytes using
/// `entries` (flat, `channels` bytes each) — the decoder-side inverse of
/// [`build_palette`]. Returns [`CodecError::InvalidPaletteIndex`] if any
/// index has no corresponding entry (spec section 4.3: "decoders must
/// reject any index >= entry_count").
pub fn expand_indices(indices: &[u8], entries: &[u8], channels: u8) -> Result<Vec<u8>> {
    let channels = channels as usize;
    let entry_count = entries.len().checked_div(channels).unwrap_or(0);

    let mut out = Vec::with_capacity(indices.len() * channels);
    for &idx in indices {
        if idx as usize >= entry_count {
            return Err(CodecError::InvalidPaletteIndex(idx));
        }
        let start = idx as usize * channels;
        out.extend_from_slice(&entries[start..start + channels]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_palette_two_colors() {
        // 4 gray pixels: 0, 255, 0, 255
        let pixels = vec![0u8, 255, 0, 255];
        let palette = build_palette(&pixels, 1).unwrap();
        assert_eq!(palette.entries, vec![0, 255]);
        assert_eq!(palette.indices, vec![0, 1, 0, 1]);
    }

    #[test]
    fn test_build_palette_rgb_first_seen_order() {
        // 3 RGB pixels: red, green, red
        let pixels = vec![255u8, 0, 0, 0, 255, 0, 255, 0, 0];
        let palette = build_palette(&pixels, 3).unwrap();
        assert_eq!(palette.entries, vec![255, 0, 0, 0, 255, 0]);
        assert_eq!(palette.indices, vec![0, 1, 0]);
    }

    #[test]
    fn test_build_palette_returns_none_when_too_many_colors() {
        // 257 distinct RGB colors - one more than MAX_PALETTE_ENTRIES (a
        // single byte can only ever hold 256 distinct index values, so RGB
        // channels are needed to exceed 256 distinct colors at all).
        let pixels: Vec<u8> = (0..257u32)
            .flat_map(|i| [(i & 0xFF) as u8, ((i >> 8) & 0xFF) as u8, 0u8])
            .collect();
        let result = build_palette(&pixels, 3);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_palette_accepts_exactly_max_entries() {
        // 256 distinct gray values (0..=255) fits exactly.
        let pixels: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let palette = build_palette(&pixels, 1).unwrap();
        assert_eq!(palette.entries.len(), 256);
        assert_eq!(palette.indices.len(), 256);
    }

    #[test]
    fn test_build_palette_rejects_length_not_multiple_of_channels() {
        let pixels = vec![1u8, 2, 3, 4]; // not a multiple of 3
        assert!(build_palette(&pixels, 3).is_none());
    }

    #[test]
    fn test_build_palette_zero_channels_returns_none() {
        assert!(build_palette(&[1, 2, 3], 0).is_none());
    }

    #[test]
    fn test_build_palette_empty_input() {
        let palette = build_palette(&[], 3).unwrap();
        assert!(palette.entries.is_empty());
        assert!(palette.indices.is_empty());
    }

    #[test]
    fn test_expand_indices_roundtrip() {
        let pixels = vec![255u8, 0, 0, 0, 255, 0, 255, 0, 0];
        let palette = build_palette(&pixels, 3).unwrap();
        let expanded = expand_indices(&palette.indices, &palette.entries, 3).unwrap();
        assert_eq!(expanded, pixels);
    }

    #[test]
    fn test_expand_indices_rejects_out_of_range_index() {
        let entries = vec![10u8, 20, 30]; // 1 entry
        let indices = vec![0u8, 1u8]; // index 1 doesn't exist
        let result = expand_indices(&indices, &entries, 3);
        assert!(matches!(result, Err(CodecError::InvalidPaletteIndex(1))));
    }

    #[test]
    fn test_expand_indices_empty() {
        let result = expand_indices(&[], &[], 3).unwrap();
        assert!(result.is_empty());
    }
}
