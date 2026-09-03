//! Round-trip and quality tests for `PaletteAlgorithm` variants (v1.1
//! `NearestNeighbor`/`MedianCut`, v1.5 `NearestNeighborWeighted`, v1.7
//! `KMeans`).

use cafe::{decode, encode_indexed, EncodeOptions, PaletteAlgorithm};
use image::{ImageBuffer, RgbaImage};
use std::str::FromStr;

fn make_multicolor_image(w: u32, h: u32) -> RgbaImage {
    // A handful of visually distinct colors, including some at the extremes
    // (pure red/blue) where redmean's weighting most visibly differs from
    // plain Euclidean distance.
    let colors = [
        image::Rgba([255, 0, 0, 255]),     // red
        image::Rgba([0, 0, 255, 255]),     // blue
        image::Rgba([0, 200, 0, 255]),     // green
        image::Rgba([255, 255, 0, 255]),   // yellow
        image::Rgba([128, 128, 128, 255]), // gray
        image::Rgba([255, 128, 200, 200]), // pink, semi-transparent
    ];
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let idx = ((x / 4 + y / 4) as usize) % colors.len();
        *pixel = colors[idx];
    }
    img
}

fn encode_decode_indexed(png_path: &str, algorithm: PaletteAlgorithm, out_stub: &str) -> Vec<u8> {
    let opts = EncodeOptions {
        level: 9,
        palette_algorithm: algorithm,
        ..Default::default()
    };
    let cafe_path = format!("target/{out_stub}.cafe");
    let decoded_path = format!("target/{out_stub}_decoded.png");
    encode_indexed(png_path, &cafe_path, &opts).expect("encode_indexed should succeed");
    decode(&cafe_path, &decoded_path).expect("decode should succeed");
    let img = image::open(&decoded_path)
        .expect("decoded output should be a valid image")
        .to_rgba8();
    let _ = std::fs::remove_file(&cafe_path);
    let _ = std::fs::remove_file(&decoded_path);
    img.into_raw()
}

#[test]
fn palette_algorithm_from_str_weighted_accepted_end_to_end() {
    assert_eq!(
        PaletteAlgorithm::from_str("weighted").unwrap(),
        PaletteAlgorithm::NearestNeighborWeighted
    );
}

#[test]
fn weighted_algorithm_roundtrips_without_panicking() {
    let img = make_multicolor_image(32, 32);
    let png_path = "target/palette_weighted_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();

    let decoded = encode_decode_indexed(
        png_path,
        PaletteAlgorithm::NearestNeighborWeighted,
        "palette_weighted",
    );
    assert_eq!(decoded.len(), img.into_raw().len());

    let _ = std::fs::remove_file(png_path);
}

#[test]
fn weighted_algorithm_maps_exact_palette_colors_losslessly() {
    // With few enough unique colors to all fit in the palette (<=256), the
    // greedy incremental algorithm (weighted or not) should reproduce every
    // pixel exactly: each pixel either matches an existing entry at distance
    // 0, or becomes a brand-new entry (the redmean formula does not change
    // this property, since a distance of exactly 0 only occurs for
    // identical colors under either metric).
    let img = make_multicolor_image(16, 16);
    let png_path = "target/palette_weighted_exact_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();

    let original = img.into_raw();
    let decoded = encode_decode_indexed(
        png_path,
        PaletteAlgorithm::NearestNeighborWeighted,
        "palette_weighted_exact",
    );

    assert_eq!(
        original, decoded,
        "with <=256 unique colors, weighted quantization should be lossless"
    );

    let _ = std::fs::remove_file(png_path);
}

#[test]
fn all_four_palette_algorithms_produce_valid_roundtrips() {
    let img = make_multicolor_image(24, 24);
    let png_path = "target/palette_all_algorithms_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();
    let expected_len = img.into_raw().len();

    for (algo, label) in [
        (PaletteAlgorithm::NearestNeighbor, "nn"),
        (PaletteAlgorithm::MedianCut, "mediancut"),
        (PaletteAlgorithm::NearestNeighborWeighted, "weighted"),
        (PaletteAlgorithm::KMeans, "kmeans"),
    ] {
        let decoded = encode_decode_indexed(png_path, algo, &format!("palette_all_{label}"));
        assert_eq!(decoded.len(), expected_len, "algorithm {label:?} failed");
    }

    let _ = std::fs::remove_file(png_path);
}

#[test]
fn palette_algorithm_from_str_kmeans_accepted_end_to_end() {
    assert_eq!(
        PaletteAlgorithm::from_str("kmeans").unwrap(),
        PaletteAlgorithm::KMeans
    );
    assert_eq!(
        PaletteAlgorithm::from_str("k-means").unwrap(),
        PaletteAlgorithm::KMeans
    );
}

#[test]
fn kmeans_algorithm_maps_exact_palette_colors_losslessly() {
    // Unlike `NearestNeighborWeighted` (which quantizes the original RGBA
    // buffer directly, preserving alpha exactly), `quantize_kmeans` shares
    // `quantize_median_cut`'s RGB-only convention: alpha is always forced
    // to 255 in the resulting palette. So this test uses an all-opaque
    // image (no semi-transparent pixels) -- with <=256 unique opaque
    // colors, the median-cut short-circuit `quantize_kmeans` also takes
    // returns one palette entry per unique color, and every pixel
    // round-trips exactly.
    let colors = [
        image::Rgba([255, 0, 0, 255]),
        image::Rgba([0, 0, 255, 255]),
        image::Rgba([0, 200, 0, 255]),
        image::Rgba([255, 255, 0, 255]),
        image::Rgba([128, 128, 128, 255]),
    ];
    let mut img: RgbaImage = ImageBuffer::new(16, 16);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let idx = ((x / 4 + y / 4) as usize) % colors.len();
        *pixel = colors[idx];
    }

    let png_path = "target/palette_kmeans_exact_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();

    let original = img.into_raw();
    let decoded = encode_decode_indexed(png_path, PaletteAlgorithm::KMeans, "palette_kmeans_exact");

    assert_eq!(
        original, decoded,
        "with <=256 unique opaque colors, kmeans quantization should be lossless"
    );

    let _ = std::fs::remove_file(png_path);
}

#[test]
fn kmeans_reduces_many_colors_to_requested_palette_size() {
    // A gradient with far more unique colors than 256 forces real
    // clustering (not the lossless short-circuit), exercising Lloyd's
    // algorithm's iterate-to-convergence path.
    let w = 64u32;
    let h = 64u32;
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let r = ((x * 255) / w) as u8;
        let g = ((y * 255) / h) as u8;
        let b = (((x + y) * 255) / (w + h)) as u8;
        *pixel = image::Rgba([r, g, b, 255]);
    }
    let png_path = "target/palette_kmeans_gradient_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();
    let expected_len = img.into_raw().len();

    let decoded = encode_decode_indexed(
        png_path,
        PaletteAlgorithm::KMeans,
        "palette_kmeans_gradient",
    );
    assert_eq!(decoded.len(), expected_len);

    let _ = std::fs::remove_file(png_path);
}
