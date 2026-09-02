//! Compression regression guard (v1.5, audit item #2).
//!
//! `tests/heuristic_benchmark.rs` compares heuristics against each other but
//! has no absolute baseline and its interesting cases are `#[ignore]`d (not
//! run in CI). This file closes that gap: it encodes a small set of fixed,
//! deterministic synthetic images with fixed `EncodeOptions`, and asserts
//! the resulting `.cafe` file size does not regress beyond a fixed tolerance
//! relative to a checked-in baseline. A silent size increase (e.g. from an
//! accidentally-disabled filter, a broken heuristic, or a downgraded ZSTD
//! level) will fail CI instead of going unnoticed.
//!
//! This is deliberately **not** a proxy for "compression got better/worse
//! for real-world photos" — it only guards against **regressions** in this
//! specific, deterministic, synthetic test suite. Real-world validation
//! still relies on the `#[ignore]`d benchmarks in `heuristic_benchmark.rs`
//! (run manually) and manual testing with representative images.
//!
//! ## Regenerating the baseline
//!
//! If a change is expected to alter compressed size (e.g. a new/tuned
//! heuristic, a new filter, a tiling change), regenerate the baseline by
//! running this test with the `CAFE_PRINT_COMPRESSION_SIZES` environment
//! variable set, copying the printed sizes into the `BASELINE` table below:
//!
//! ```text
//! CAFE_PRINT_COMPRESSION_SIZES=1 cargo test --test compression_regression -- --nocapture
//! ```

use cafe::{encode, encode_indexed, EncodeOptions, FilterHeuristic};
use std::fs;
use std::path::PathBuf;

/// Allowed growth over baseline before the test fails. Generous enough to
/// absorb minor cross-platform ZSTD variance (if any) while still catching
/// real regressions (a disabled filter or heuristic typically costs far more
/// than 5%).
const TOLERANCE: f64 = 0.05;

/// Baseline `.cafe` file sizes (bytes) for each case below, measured with
/// the current default `EncodeOptions` + ZSTD level fixed at 19. See the
/// module doc for how to regenerate after an intentional change.
const BASELINE: &[(&str, u64)] = &[
    ("gradient_256x256_default", 245),
    ("natural_256x256_default", 249),
    ("checkerboard_256x256_default", 325),
    ("checkerboard_256x256_indexed", 275),
];

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("cafe_compression_regression");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Smooth gradient — low-variance, favors simple predictors (Sub/Up/Average).
fn make_gradient_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(256, 256);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
    }
    img
}

/// Medium-variance "natural photo-like" synthetic pattern — benefits from
/// mixed predictors and real entropy coding, same generator as
/// `heuristic_benchmark.rs::make_natural_image` for consistency across the
/// two test suites.
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

/// High-redundancy checkerboard — few distinct colors, favors both the
/// predictive filter and the indexed palette path.
fn make_checkerboard_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(256, 256);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let is_white = ((x / 8) + (y / 8)) % 2 == 0;
        let v = if is_white { 255 } else { 0 };
        *px = image::Rgba([v, v, v, 255]);
    }
    img
}

fn baseline_for(name: &str) -> u64 {
    BASELINE
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no baseline entry for case '{name}'"))
        .1
}

fn check_size(name: &str, actual_size: u64) {
    if std::env::var("CAFE_PRINT_COMPRESSION_SIZES").is_ok() {
        println!("{name}: {actual_size} bytes");
    }
    let baseline = baseline_for(name);
    let max_allowed = (baseline as f64 * (1.0 + TOLERANCE)).ceil() as u64;
    assert!(
        actual_size <= max_allowed,
        "compression regression in '{name}': {actual_size} bytes exceeds baseline {baseline} \
         bytes + {:.0}% tolerance ({max_allowed} bytes). If this growth is expected (e.g. an \
         intentional format/heuristic change), regenerate the baseline — see the module doc in \
         tests/compression_regression.rs.",
        TOLERANCE * 100.0
    );
}

#[test]
fn test_compression_regression_gradient_default() {
    let dir = temp_dir();
    let input = dir.join("gradient_default.png");
    let output = dir.join("gradient_default.cafe");
    make_gradient_image().save(&input).unwrap();

    let opts = EncodeOptions {
        level: 19,
        ..Default::default()
    };
    encode(input.to_str().unwrap(), output.to_str().unwrap(), &opts).expect("encode failed");
    let size = fs::metadata(&output).unwrap().len();
    check_size("gradient_256x256_default", size);

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
}

#[test]
fn test_compression_regression_natural_default() {
    let dir = temp_dir();
    let input = dir.join("natural_default.png");
    let output = dir.join("natural_default.cafe");
    make_natural_image().save(&input).unwrap();

    let opts = EncodeOptions {
        level: 19,
        filter_heuristic: FilterHeuristic::Entropy,
        ..Default::default()
    };
    encode(input.to_str().unwrap(), output.to_str().unwrap(), &opts).expect("encode failed");
    let size = fs::metadata(&output).unwrap().len();
    check_size("natural_256x256_default", size);

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
}

#[test]
fn test_compression_regression_checkerboard_default() {
    let dir = temp_dir();
    let input = dir.join("checkerboard_default.png");
    let output = dir.join("checkerboard_default.cafe");
    make_checkerboard_image().save(&input).unwrap();

    let opts = EncodeOptions {
        level: 19,
        ..Default::default()
    };
    encode(input.to_str().unwrap(), output.to_str().unwrap(), &opts).expect("encode failed");
    let size = fs::metadata(&output).unwrap().len();
    check_size("checkerboard_256x256_default", size);

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
}

#[test]
fn test_compression_regression_checkerboard_indexed() {
    let dir = temp_dir();
    let input = dir.join("checkerboard_indexed.png");
    let output = dir.join("checkerboard_indexed.cafe");
    make_checkerboard_image().save(&input).unwrap();

    let opts = EncodeOptions {
        level: 19,
        ..Default::default()
    };
    encode_indexed(input.to_str().unwrap(), output.to_str().unwrap(), &opts)
        .expect("encode_indexed failed");
    let size = fs::metadata(&output).unwrap().len();
    check_size("checkerboard_256x256_indexed", size);

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
}
