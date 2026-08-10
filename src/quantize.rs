//! Color quantization algorithms for indexed palette encoding (v1.1)
//!
//! Provides median-cut and nearest-neighbor quantization strategies
//! for converting arbitrary RGB(A) images to indexed palettes.

use crate::error::{CafeError, Result};
use crate::types::{Palette, PaletteEntry};

/// Median-cut quantization: recursively bisects color space
///
/// Algorithm (Heckbert 1982):
/// 1. Take all unique colors in the image
/// 2. Find the color axis (R, G, B) with the largest range
/// 3. Sort by that axis and split at the median
/// 4. Recursively apply to each half until we have ≤ max_colors
/// 5. Average colors in each bucket to form palette entries
pub fn quantize_median_cut(rgba: &[u8], max_colors: usize) -> Result<Palette> {
    if !rgba.len().is_multiple_of(4) {
        return Err(CafeError::UnsupportedFeature(
            "quantize_median_cut expects RGBA data (len % 4 == 0)".into(),
        ));
    }

    if max_colors == 0 || max_colors > 256 {
        return Err(CafeError::UnsupportedFeature(format!(
            "max_colors must be 1..=256, got {}",
            max_colors
        )));
    }

    // Collect unique colors with their frequencies
    let mut color_counts: std::collections::HashMap<[u8; 3], usize> =
        std::collections::HashMap::new();

    for chunk in rgba.chunks(4) {
        if chunk.len() == 4 {
            let has_alpha = chunk[3] > 0; // Treat transparent as separate
            if has_alpha {
                let rgb = [chunk[0], chunk[1], chunk[2]];
                *color_counts.entry(rgb).or_insert(0) += 1;
            }
        }
    }

    if color_counts.is_empty() {
        return Err(CafeError::UnsupportedFeature(
            "Image contains no opaque pixels".into(),
        ));
    }

    // If we already have ≤ max_colors unique colors, just use them
    if color_counts.len() <= max_colors {
        let mut entries = Vec::new();
        for (rgb, _count) in color_counts {
            entries.push(PaletteEntry {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 255,
            });
        }
        let mut palette = Palette::new(false);
        palette.entries = entries;
        return Ok(palette);
    }

    // Median-cut: recursively partition
    let mut buckets: Vec<Vec<[u8; 3]>> = vec![color_counts.keys().copied().collect()];

    while buckets.len() < max_colors && buckets.iter().any(|b| b.len() > 1) {
        // Find the bucket with the largest range
        let (bucket_idx, _) = buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1) // Only consider splittable buckets
            .map(|(idx, b)| (idx, bucket_variance(b)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, 0.0));

        // Split the chosen bucket
        let bucket = buckets.remove(bucket_idx);
        let (b1, b2) = split_bucket_median(&bucket);
        if !b1.is_empty() {
            buckets.push(b1);
        }
        if !b2.is_empty() {
            buckets.push(b2);
        }
    }

    // Average colors in each bucket to form palette
    let mut entries = Vec::new();
    for bucket in &buckets {
        if !bucket.is_empty() {
            let (r_sum, g_sum, b_sum): (u32, u32, u32) = bucket.iter().fold(
                (0, 0, 0),
                |(r, g, b), color| (r + color[0] as u32, g + color[1] as u32, b + color[2] as u32),
            );
            let count = bucket.len() as u32;
            entries.push(PaletteEntry {
                r: (r_sum / count) as u8,
                g: (g_sum / count) as u8,
                b: (b_sum / count) as u8,
                a: 255,
            });
        }
    }

    let mut palette = Palette::new(false);
    palette.entries = entries;
    Ok(palette)
}

/// Calculates the variance (sum of squared distances from mean) for a bucket
fn bucket_variance(colors: &[[u8; 3]]) -> f64 {
    if colors.is_empty() {
        return 0.0;
    }

    // Mean of each channel
    let (r_sum, g_sum, b_sum): (f64, f64, f64) = colors.iter().fold(
        (0.0, 0.0, 0.0),
        |(r, g, b), c| (r + c[0] as f64, g + c[1] as f64, b + c[2] as f64),
    );
    let count = colors.len() as f64;
    let (r_mean, g_mean, b_mean) = (r_sum / count, g_sum / count, b_sum / count);

    // Variance: average squared distance from mean
    colors
        .iter()
        .map(|c| {
            let dr = c[0] as f64 - r_mean;
            let dg = c[1] as f64 - g_mean;
            let db = c[2] as f64 - b_mean;
            dr * dr + dg * dg + db * db
        })
        .sum::<f64>()
        / count
}

/// Finds the longest axis and splits the bucket at the median
fn split_bucket_median(colors: &[[u8; 3]]) -> (Vec<[u8; 3]>, Vec<[u8; 3]>) {
    if colors.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Find longest axis
    let (r_min, r_max) = colors.iter().fold((u8::MAX, u8::MIN), |(min, max), c| {
        (min.min(c[0]), max.max(c[0]))
    });
    let (g_min, g_max) = colors.iter().fold((u8::MAX, u8::MIN), |(min, max), c| {
        (min.min(c[1]), max.max(c[1]))
    });
    let (b_min, b_max) = colors.iter().fold((u8::MAX, u8::MIN), |(min, max), c| {
        (min.min(c[2]), max.max(c[2]))
    });

    let r_range = r_max as u16 - r_min as u16;
    let g_range = g_max as u16 - g_min as u16;
    let b_range = b_max as u16 - b_min as u16;

    // Choose axis with largest range
    let axis = if r_range >= g_range && r_range >= b_range {
        0 // R
    } else if g_range >= b_range {
        1 // G
    } else {
        2 // B
    };

    // Sort by chosen axis
    let mut sorted = colors.to_vec();
    sorted.sort_by_key(|c| c[axis]);

    // Split at median
    let mid = sorted.len() / 2;
    let (b1, b2) = sorted.split_at(mid);
    (b1.to_vec(), b2.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_median_cut_simple() {
        // Simple test: 8 colors, should return all 8
        let rgba = vec![
            255, 0, 0, 255,   // Red
            0, 255, 0, 255,   // Green
            0, 0, 255, 255,   // Blue
            255, 255, 0, 255, // Yellow
            255, 0, 255, 255, // Magenta
            0, 255, 255, 255, // Cyan
            128, 128, 128, 255, // Gray
            255, 255, 255, 255, // White
        ];

        let palette = quantize_median_cut(&rgba, 8).unwrap();
        assert_eq!(palette.entries.len(), 8, "Should have 8 palette entries");
    }

    #[test]
    fn test_median_cut_reduction() {
        // Gradient test: many shades of red, quantize to 2 colors
        let mut rgba = Vec::new();
        for i in 0..100 {
            let shade = ((i * 255 / 100) as u8).saturating_add(50); // Range [50..255]
            rgba.extend_from_slice(&[shade, 0, 0, 255]);
        }

        let palette = quantize_median_cut(&rgba, 2).unwrap();
        assert!(
            palette.entries.len() <= 2,
            "Should quantize to at most 2 palette entries"
        );

        // All entries should have some red (since input is red gradient)
        for entry in &palette.entries {
            assert!(entry.r >= 50, "Entry should have red >= 50, got r={}", entry.r);
        }
    }
}
