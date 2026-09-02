//! Round-trip and quality tests for `PaletteAlgorithm` variants (v1.1
//! `NearestNeighbor`/`MedianCut`, v1.5 `NearestNeighborWeighted`).

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
fn all_three_palette_algorithms_produce_valid_roundtrips() {
    let img = make_multicolor_image(24, 24);
    let png_path = "target/palette_all_algorithms_input.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();
    let expected_len = img.into_raw().len();

    for (algo, label) in [
        (PaletteAlgorithm::NearestNeighbor, "nn"),
        (PaletteAlgorithm::MedianCut, "mediancut"),
        (PaletteAlgorithm::NearestNeighborWeighted, "weighted"),
    ] {
        let decoded = encode_decode_indexed(png_path, algo, &format!("palette_all_{label}"));
        assert_eq!(decoded.len(), expected_len, "algorithm {label:?} failed");
    }

    let _ = std::fs::remove_file(png_path);
}
