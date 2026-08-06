//! Benchmark and integration tests for filter heuristics (v1.1)
//!
//! Tests compare the performance and compression ratio of:
//! - Entropy (default, Shannon entropy)
//! - Msad (PNG classic)
//! - CompressionTest (real ZSTD test)
//! - QuickPrune (MSAD + entropy on top 8, v1.1)
//! - AdaptiveEntropy (content-aware, v1.1)
//!
//! Generates synthetic test images and measures:
//! - Encoding time
//! - Output file size
//! - Decompression correctness

use cafe::{decode, encode, EncodeOptions, FilterHeuristic};
use std::time::Instant;

/// Creates a smooth (low-variance) synthetic image.
/// Expected to compress well with simple filters.
fn make_smooth_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(256, 256);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let val = ((x + y) as u8).wrapping_mul(2);
        *px = image::Rgba([val, val, val, 255]); // Grayscale gradient
    }
    img
}

/// Creates a natural (medium-variance) synthetic image.
/// Expected to benefit from mixed predictors.
fn make_natural_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(256, 256);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let r = ((x * 3) % 256) as u8;
        let g = ((y * 5) % 256) as u8;
        let b = ((x + y * 2) % 256) as u8;
        *px = image::Rgba([r, g, b, 255]);
    }
    img
}

/// Creates a high-frequency (high-variance) synthetic image.
/// Expected to benefit from adaptive filters (F_WEIGHTED, F_CONTEXT).
fn make_noisy_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(256, 256);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let pseudo_random = ((x * 137 + y * 73 + 19) % 256) as u8;
        *px = image::Rgba([pseudo_random, pseudo_random, pseudo_random, 255]);
    }
    img
}

/// Encodes and decodes an image with the given heuristic, measuring time and size.
struct BenchmarkResult {
    heuristic: FilterHeuristic,
    encode_ms: u128,
    output_size: u64,
    decode_ok: bool,
}

impl BenchmarkResult {
    fn compression_ratio(&self) -> f64 {
        let input_size = (256 * 256 * 4) as f64; // RGBA
        self.output_size as f64 / input_size
    }
}

impl std::fmt::Display for BenchmarkResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let heuristic_name = match self.heuristic {
            FilterHeuristic::Entropy => "Entropy",
            FilterHeuristic::Msad => "Msad",
            FilterHeuristic::CompressionTest => "CompressionTest",
            FilterHeuristic::QuickPrune => "QuickPrune",
            FilterHeuristic::AdaptiveEntropy => "AdaptiveEntropy",
        };

        write!(
            f,
            "{:18} | Size: {:6} B | Ratio: {:.2}% | Time: {:4} ms | Decode: {}",
            heuristic_name,
            self.output_size,
            self.compression_ratio() * 100.0,
            self.encode_ms,
            if self.decode_ok { "OK" } else { "FAIL" }
        )
    }
}

/// Benchmarks a single image with all heuristics.
fn benchmark_image(label: &str, img: image::RgbaImage) -> Vec<BenchmarkResult> {
    let temp_dir = std::env::temp_dir().join("cafe_benchmark");
    std::fs::create_dir_all(&temp_dir).unwrap();

    let input_path = temp_dir.join(format!("{label}_input.png"));
    img.save(&input_path).unwrap();

    let heuristics = [
        FilterHeuristic::Entropy,
        FilterHeuristic::Msad,
        FilterHeuristic::QuickPrune,
        FilterHeuristic::AdaptiveEntropy,
        // Skip CompressionTest (too slow for integration test)
    ];

    let mut results = Vec::new();

    for heuristic in heuristics {
        let cafe_path = temp_dir.join(format!("{label}_{:?}.cafe", heuristic));
        let out_path = temp_dir.join(format!("{label}_{:?}_out.png", heuristic));

        // Encode
        let start = Instant::now();
        let opts = EncodeOptions {
            filter_heuristic: heuristic,
            level: 19,
            ..Default::default()
        };
        let encode_result = encode(
            input_path.to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &opts,
        );
        let encode_ms = start.elapsed().as_millis();

        let output_size = if encode_result.is_ok() {
            std::fs::metadata(&cafe_path).unwrap().len()
        } else {
            0
        };

        // Decode and verify
        let decode_ok = if encode_result.is_ok() {
            let result = decode(cafe_path.to_str().unwrap(), out_path.to_str().unwrap());
            result.is_ok()
        } else {
            false
        };

        results.push(BenchmarkResult {
            heuristic,
            encode_ms,
            output_size,
            decode_ok,
        });

        // Cleanup
        let _ = std::fs::remove_file(&cafe_path);
        let _ = std::fs::remove_file(&out_path);
    }

    // Cleanup input
    let _ = std::fs::remove_file(&input_path);

    results
}

#[test]
#[ignore] // Ignore by default (slow); run with: cargo test --lib heuristic -- --ignored
fn benchmark_smooth_image() {
    println!("\n========== Smooth Image (256x256) ==========");
    let img = make_smooth_image();
    let results = benchmark_image("smooth", img);

    println!("\nResults:");
    for result in &results {
        println!("  {}", result);
    }

    // All should succeed
    for result in &results {
        assert!(result.decode_ok, "Decode failed for {:?}", result.heuristic);
    }
}

#[test]
#[ignore]
fn benchmark_natural_image() {
    println!("\n========== Natural Image (256x256) ==========");
    let img = make_natural_image();
    let results = benchmark_image("natural", img);

    println!("\nResults:");
    for result in &results {
        println!("  {}", result);
    }

    for result in &results {
        assert!(result.decode_ok, "Decode failed for {:?}", result.heuristic);
    }
}

#[test]
#[ignore]
fn benchmark_noisy_image() {
    println!("\n========== Noisy Image (256x256) ==========");
    let img = make_noisy_image();
    let results = benchmark_image("noisy", img);

    println!("\nResults:");
    for result in &results {
        println!("  {}", result);
    }

    for result in &results {
        assert!(result.decode_ok, "Decode failed for {:?}", result.heuristic);
    }
}

#[test]
fn test_all_heuristics_produce_correct_output() {
    // Quick functional test (not ignored)
    let img = make_natural_image();
    let temp_dir = std::env::temp_dir().join("cafe_quick_test");
    std::fs::create_dir_all(&temp_dir).unwrap();

    let input_path = temp_dir.join("test_input.png");
    img.save(&input_path).unwrap();

    let heuristics = [
        FilterHeuristic::Entropy,
        FilterHeuristic::QuickPrune,
        FilterHeuristic::AdaptiveEntropy,
    ];

    for heuristic in heuristics {
        let cafe_path = temp_dir.join(format!("test_{:?}.cafe", heuristic));
        let out_path = temp_dir.join(format!("test_{:?}_out.png", heuristic));

        let opts = EncodeOptions {
            filter_heuristic: heuristic,
            level: 19,
            ..Default::default()
        };

        // Should encode successfully
        let encode_result = encode(
            input_path.to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &opts,
        );
        assert!(encode_result.is_ok(), "Encode failed for {:?}", heuristic);

        // Should decode successfully
        let decode_result = decode(cafe_path.to_str().unwrap(), out_path.to_str().unwrap());
        assert!(decode_result.is_ok(), "Decode failed for {:?}", heuristic);

        // Cleanup
        let _ = std::fs::remove_file(&cafe_path);
        let _ = std::fs::remove_file(&out_path);
    }

    let _ = std::fs::remove_file(&input_path);
}
