//! Color quantization algorithms for indexed palette encoding (v1.1)
//!
//! Provides median-cut and nearest-neighbor quantization strategies
//! for converting arbitrary RGB(A) images to indexed palettes.

use crate::error::{CafeError, Result};
use crate::types::{Palette, PaletteEntry};

/// Collects unique opaque RGB colors (alpha > 0) with their pixel-count
/// weights. Shared between `quantize_median_cut` and `quantize_kmeans`,
/// which both need the same histogram as their starting point before
/// diverging into recursive bisection vs. iterative centroid refinement.
/// Fully transparent pixels are excluded, matching both algorithms'
/// existing RGB-only (alpha-agnostic) quantization strategy.
fn collect_opaque_color_counts(rgba: &[u8]) -> Result<std::collections::HashMap<[u8; 3], usize>> {
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

    Ok(color_counts)
}

/// Builds a `Palette` directly from a set of unique colors (one entry per
/// color, alpha forced to 255) -- the lossless short-circuit both
/// `quantize_median_cut` and `quantize_kmeans` take when the image already
/// has `<= max_colors` unique opaque colors, since no quantization is
/// actually needed in that case.
fn palette_from_unique_colors(color_counts: std::collections::HashMap<[u8; 3], usize>) -> Palette {
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
    palette
}

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

    let color_counts = collect_opaque_color_counts(rgba)?;

    // If we already have ≤ max_colors unique colors, just use them
    if color_counts.len() <= max_colors {
        return Ok(palette_from_unique_colors(color_counts));
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
            let (r_sum, g_sum, b_sum): (u32, u32, u32) =
                bucket.iter().fold((0, 0, 0), |(r, g, b), color| {
                    (
                        r + color[0] as u32,
                        g + color[1] as u32,
                        b + color[2] as u32,
                    )
                });
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
    let (r_sum, g_sum, b_sum): (f64, f64, f64) =
        colors.iter().fold((0.0, 0.0, 0.0), |(r, g, b), c| {
            (r + c[0] as f64, g + c[1] as f64, b + c[2] as f64)
        });
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

/// K-means (Lloyd's algorithm) color quantization (v1.7).
///
/// Unlike `quantize_median_cut` (which partitions color space by recursive
/// bisection) or the greedy incremental `NearestNeighbor`/
/// `NearestNeighborWeighted` strategies (which grow the palette one pixel at
/// a time, order-dependent), k-means directly minimizes total squared
/// distance from each pixel to its assigned palette entry via iterative
/// centroid refinement -- typically the best-quality (lowest mean-squared-
/// error) of the four algorithms, at the cost of more computation.
///
/// # Algorithm
/// 1. Collect unique opaque RGB colors with pixel-count weights (same
///    histogram `quantize_median_cut` uses -- clustering the histogram
///    instead of every raw pixel is the standard practice for color
///    quantization, since duplicate colors are extremely common in real
///    images and clustering on frequency-weighted uniques is mathematically
///    identical to clustering every pixel, just far cheaper).
/// 2. **Initialize centroids via median-cut** (`quantize_median_cut`'s own
///    bucket-averaging output) rather than random seeding. This is a
///    deterministic choice -- CAFE has no RNG dependency anywhere else in
///    the codebase, and introducing one purely for k-means++-style
///    stochastic seeding would be a new category of non-determinism for a
///    format whose encoder is otherwise fully reproducible given the same
///    input and options. Median-cut is also a well-established good
///    initializer for k-means in color quantization specifically (it
///    already roughly partitions color space by density), so determinism
///    is not traded against quality here.
/// 3. **Lloyd's algorithm**: repeatedly (a) assign every unique color to its
///    nearest centroid by squared Euclidean RGB distance, (b) recompute each
///    centroid as the pixel-count-weighted mean of its assigned colors,
///    until assignments stop changing or `MAX_KMEANS_ITERATIONS` is reached
///    (guards against theoretical non-termination -- Lloyd's algorithm is
///    only guaranteed to converge, not to converge *quickly*, though in
///    practice it typically stabilizes within a handful of iterations on
///    real images).
/// 4. **Empty clusters** (a centroid to which zero colors get assigned,
///    which can happen with adversarial/synthetic inputs) are dropped
///    rather than re-seeded -- this can yield a final palette with fewer
///    than `max_colors` entries, which is already an accepted behavior of
///    `quantize_median_cut` (see its bucket-splitting loop) and every other
///    `PaletteAlgorithm` variant, none of which guarantee hitting the
///    requested count exactly.
pub fn quantize_kmeans(rgba: &[u8], max_colors: usize) -> Result<Palette> {
    if !rgba.len().is_multiple_of(4) {
        return Err(CafeError::UnsupportedFeature(
            "quantize_kmeans expects RGBA data (len % 4 == 0)".into(),
        ));
    }

    if max_colors == 0 || max_colors > 256 {
        return Err(CafeError::UnsupportedFeature(format!(
            "max_colors must be 1..=256, got {}",
            max_colors
        )));
    }

    let color_counts = collect_opaque_color_counts(rgba)?;

    // If we already have ≤ max_colors unique colors, no clustering needed.
    if color_counts.len() <= max_colors {
        return Ok(palette_from_unique_colors(color_counts));
    }

    // Weighted histogram: (color, pixel_count) pairs, fixed order for the
    // rest of this function (HashMap iteration order is otherwise
    // nondeterministic across runs, which would make k-means' assignment
    // step -- and thus its output -- nondeterministic too).
    let mut histogram: Vec<([u8; 3], usize)> = color_counts.into_iter().collect();
    histogram.sort_unstable_by_key(|(rgb, _)| *rgb);

    // Step 2: deterministic initialization via median-cut.
    let init_palette = quantize_median_cut(rgba, max_colors)?;
    let mut centroids: Vec<[f64; 3]> = init_palette
        .entries
        .iter()
        .map(|e| [e.r as f64, e.g as f64, e.b as f64])
        .collect();

    const MAX_KMEANS_ITERATIONS: usize = 20;
    let mut assignments = vec![0usize; histogram.len()];

    for _ in 0..MAX_KMEANS_ITERATIONS {
        // Assignment step: nearest centroid (by squared RGB distance) for
        // every unique color in the histogram.
        let mut changed = false;
        for (i, (rgb, _count)) in histogram.iter().enumerate() {
            let mut best_idx = 0usize;
            let mut best_dist = f64::MAX;
            for (c_idx, centroid) in centroids.iter().enumerate() {
                let dr = rgb[0] as f64 - centroid[0];
                let dg = rgb[1] as f64 - centroid[1];
                let db = rgb[2] as f64 - centroid[2];
                let dist = dr * dr + dg * dg + db * db;
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = c_idx;
                }
            }
            if assignments[i] != best_idx {
                assignments[i] = best_idx;
                changed = true;
            }
        }

        // Update step: recompute each centroid as the weighted mean of its
        // assigned colors. Empty clusters keep their previous centroid
        // value here (harmless, since they're dropped entirely below if
        // still empty after the loop finishes) rather than being reseeded
        // mid-loop, keeping this step a straightforward weighted average.
        let mut sums = vec![[0.0f64; 3]; centroids.len()];
        let mut weights = vec![0.0f64; centroids.len()];
        for (i, (rgb, count)) in histogram.iter().enumerate() {
            let c_idx = assignments[i];
            let w = *count as f64;
            sums[c_idx][0] += rgb[0] as f64 * w;
            sums[c_idx][1] += rgb[1] as f64 * w;
            sums[c_idx][2] += rgb[2] as f64 * w;
            weights[c_idx] += w;
        }
        for (c_idx, centroid) in centroids.iter_mut().enumerate() {
            if weights[c_idx] > 0.0 {
                centroid[0] = sums[c_idx][0] / weights[c_idx];
                centroid[1] = sums[c_idx][1] / weights[c_idx];
                centroid[2] = sums[c_idx][2] / weights[c_idx];
            }
        }

        if !changed {
            break;
        }
    }

    // Step 4: drop empty clusters, round surviving centroids to u8.
    let mut cluster_has_members = vec![false; centroids.len()];
    for &c_idx in &assignments {
        cluster_has_members[c_idx] = true;
    }

    let mut entries = Vec::new();
    for (c_idx, centroid) in centroids.iter().enumerate() {
        if cluster_has_members[c_idx] {
            entries.push(PaletteEntry {
                r: centroid[0].round().clamp(0.0, 255.0) as u8,
                g: centroid[1].round().clamp(0.0, 255.0) as u8,
                b: centroid[2].round().clamp(0.0, 255.0) as u8,
                a: 255,
            });
        }
    }

    let mut palette = Palette::new(false);
    palette.entries = entries;
    Ok(palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_median_cut_simple() {
        // Simple test: 8 colors, should return all 8
        let rgba = vec![
            255, 0, 0, 255, // Red
            0, 255, 0, 255, // Green
            0, 0, 255, 255, // Blue
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
            assert!(
                entry.r >= 50,
                "Entry should have red >= 50, got r={}",
                entry.r
            );
        }
    }

    #[test]
    fn test_kmeans_already_under_max_colors() {
        // With <= max_colors unique colors, quantize_kmeans should take the
        // same lossless short-circuit as quantize_median_cut: one entry per
        // unique color, no clustering iteration needed.
        let rgba = vec![
            255, 0, 0, 255, // Red
            0, 255, 0, 255, // Green
            0, 0, 255, 255, // Blue
        ];

        let palette = quantize_kmeans(&rgba, 8).unwrap();
        assert_eq!(
            palette.entries.len(),
            3,
            "Should have exactly 3 palette entries (one per unique color)"
        );
    }

    #[test]
    fn test_kmeans_reduction() {
        // Many shades of red, quantize to 2 clusters. Unlike median-cut's
        // deterministic bisection, kmeans should converge to two clusters
        // roughly separating low vs. high red intensity.
        let mut rgba = Vec::new();
        for i in 0..100 {
            let shade = ((i * 255 / 100) as u8).saturating_add(50); // Range [50..255]
            rgba.extend_from_slice(&[shade, 0, 0, 255]);
        }

        let palette = quantize_kmeans(&rgba, 2).unwrap();
        assert!(
            palette.entries.len() <= 2,
            "Should quantize to at most 2 palette entries, got {}",
            palette.entries.len()
        );

        for entry in &palette.entries {
            assert!(
                entry.r >= 50,
                "Entry should have red >= 50, got r={}",
                entry.r
            );
            assert_eq!(entry.g, 0);
            assert_eq!(entry.b, 0);
            assert_eq!(entry.a, 255);
        }
    }

    #[test]
    fn test_kmeans_deterministic_across_runs() {
        // No RNG is involved (initialization is via median-cut, a pure
        // function of the input) -- running twice on identical input must
        // produce byte-identical palettes.
        let mut rgba = Vec::new();
        for i in 0..300u32 {
            let r = ((i * 37) % 256) as u8;
            let g = ((i * 71) % 256) as u8;
            let b = ((i * 113) % 256) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }

        let palette1 = quantize_kmeans(&rgba, 16).unwrap();
        let palette2 = quantize_kmeans(&rgba, 16).unwrap();

        assert_eq!(palette1.entries.len(), palette2.entries.len());
        for (e1, e2) in palette1.entries.iter().zip(palette2.entries.iter()) {
            assert_eq!(e1.r, e2.r);
            assert_eq!(e1.g, e2.g);
            assert_eq!(e1.b, e2.b);
            assert_eq!(e1.a, e2.a);
        }
    }

    #[test]
    fn test_kmeans_rejects_invalid_max_colors() {
        let rgba = vec![255, 0, 0, 255];
        assert!(quantize_kmeans(&rgba, 0).is_err());
        assert!(quantize_kmeans(&rgba, 257).is_err());
    }

    #[test]
    fn test_kmeans_rejects_non_rgba_length() {
        let rgba = vec![255, 0, 0]; // len % 4 != 0
        assert!(quantize_kmeans(&rgba, 8).is_err());
    }
}
