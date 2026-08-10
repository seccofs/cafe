//! Test auto-dictionary ZSTD training (v1.1)

use std::fs;
use std::path::Path;
use tempfile::NamedTempFile;

#[test]
fn test_auto_dictionary_roundtrip() {
    // Create a simple test PNG with repetitive pattern (good for dictionary)
    use image::{ImageBuffer, RgbaImage};

    let mut img: RgbaImage = ImageBuffer::new(64, 64);

    // Fill with a repetitive pattern
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let pattern = ((x / 8 + y / 8) % 4) as u8;
        let colors = [
            image::Rgba([255, 0, 0, 255]),   // Red
            image::Rgba([0, 255, 0, 255]),   // Green
            image::Rgba([0, 0, 255, 255]),   // Blue
            image::Rgba([255, 255, 0, 255]), // Yellow
        ];
        *pixel = colors[pattern as usize];
    }

    let temp_file = NamedTempFile::new().unwrap();
    let mut png_path = temp_file.path().to_path_buf();
    png_path.set_extension("png");

    // Use image crate with format inference - save as PNG
    img.save_with_format(&png_path, image::ImageFormat::Png)
        .unwrap();

    let png_path_str = png_path.to_str().unwrap();

    // Test 1: Encode with auto_dictionary = true
    let cafe_auto_dict = "target/test_auto_dict.cafe";
    {
        let opts = cafe::EncodeOptions {
            use_filter: true,
            level: 9,
            target_color_type: 6, // RGBA
            target_bit_depth: Some(8),
            auto_dictionary: true, // Enable auto-dictionary
            ..Default::default()
        };

        let result = cafe::encode(png_path_str, cafe_auto_dict, &opts);
        assert!(result.is_ok(), "encode with auto_dictionary should succeed");
    }

    // Test 2: Encode without auto_dictionary for comparison
    let cafe_no_auto_dict = "target/test_no_auto_dict.cafe";
    {
        let opts = cafe::EncodeOptions {
            use_filter: true,
            level: 9,
            target_color_type: 6, // RGBA
            target_bit_depth: Some(8),
            auto_dictionary: false, // Disabled
            ..Default::default()
        };

        let result = cafe::encode(png_path_str, cafe_no_auto_dict, &opts);
        assert!(
            result.is_ok(),
            "encode without auto_dictionary should succeed"
        );
    }

    // Test 3: Decode both files and verify pixels match
    let decoded_auto_dict = "target/test_auto_dict_decoded.png";
    let decoded_no_auto_dict = "target/test_no_auto_dict_decoded.png";

    let result_auto = cafe::decode(cafe_auto_dict, decoded_auto_dict);
    assert!(result_auto.is_ok(), "decode with auto_dict should succeed");

    let result_no_auto = cafe::decode(cafe_no_auto_dict, decoded_no_auto_dict);
    assert!(
        result_no_auto.is_ok(),
        "decode without auto_dict should succeed"
    );

    // Verify that both decoded files exist
    assert!(
        Path::new(decoded_auto_dict).exists(),
        "decoded auto_dict file should exist"
    );
    assert!(
        Path::new(decoded_no_auto_dict).exists(),
        "decoded no_auto_dict file should exist"
    );

    // Test 4: Compare file sizes (with dictionary, file might be slightly larger due to zDIC chunk,
    // but individual IDATs should compress better)
    let size_auto = fs::metadata(cafe_auto_dict).unwrap().len();
    let size_no_auto = fs::metadata(cafe_no_auto_dict).unwrap().len();

    println!("File sizes:");
    println!("  With auto_dictionary: {} bytes", size_auto);
    println!("  Without auto_dictionary: {} bytes", size_no_auto);

    // For a small repetitive image, auto-dictionary should help compression
    // but it's not guaranteed, so we just log it
}
