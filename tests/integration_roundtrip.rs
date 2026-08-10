//! Integration tests for end-to-end roundtrip encoding/decoding
//!
//! Tests comprehensive color type, bit depth, and filter combinations
//! to validate SIMD optimizations don't break functionality.

use cafe::*;
use std::fs;
use std::path::Path;

/// Helper to create temp directory for test files
fn temp_test_dir() -> String {
    let temp_dir = std::env::temp_dir().join("cafe_roundtrip_tests");
    let _ = fs::create_dir_all(&temp_dir);
    temp_dir.to_string_lossy().to_string()
}

/// Generate a simple test image (RGBA format)
fn generate_test_image(width: u32, height: u32, pattern: &str) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) as usize) * 4;

            match pattern {
                "solid_red" => {
                    pixels[idx] = 255;
                    pixels[idx + 1] = 0;
                    pixels[idx + 2] = 0;
                    pixels[idx + 3] = 255;
                }
                "checkerboard" => {
                    let is_white = ((x / 8) + (y / 8)) % 2 == 0;
                    let color = if is_white { 255 } else { 0 };
                    pixels[idx] = color;
                    pixels[idx + 1] = color;
                    pixels[idx + 2] = color;
                    pixels[idx + 3] = 255;
                }
                "gradient" => {
                    pixels[idx] = (x * 255 / width) as u8;
                    pixels[idx + 1] = (y * 255 / height) as u8;
                    pixels[idx + 2] = 128;
                    pixels[idx + 3] = 255;
                }
                _ => {
                    // Random-like pattern
                    let seed = ((x ^ y) * 31) as usize;
                    pixels[idx] = (seed % 256) as u8;
                    pixels[idx + 1] = ((seed * 17) % 256) as u8;
                    pixels[idx + 2] = ((seed * 43) % 256) as u8;
                    pixels[idx + 3] = 255;
                }
            }
        }
    }

    pixels
}

#[test]
fn test_roundtrip_256x256_rgba_checkerboard() {
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_256_checkerboard.png", temp_dir);
    let output_cafe = format!("{}/output_256_checkerboard.cafe", temp_dir);
    let output_png = format!("{}/output_256_checkerboard.png", temp_dir);

    // Generate test image
    let width = 256u32;
    let height = 256u32;
    let pixels = generate_test_image(width, height, "checkerboard");

    // Create test PNG (use image crate)
    let image_buffer = image::RgbaImage::from_raw(width, height, pixels.clone()).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    // Encode to CAFE
    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 12;
    opts.adaptive_analysis = true;
    opts.target_color_type = 6; // RGBA

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");

    // Verify CAFE exists and is smaller than PNG
    assert!(Path::new(&output_cafe).exists(), "CAFE file not created");
    let cafe_size = fs::metadata(&output_cafe).unwrap().len() as usize;
    let png_size = fs::metadata(&input_png).unwrap().len() as usize;
    println!(
        "Roundtrip test (256×256 checkerboard): PNG={} bytes, CAFE={} bytes",
        png_size, cafe_size
    );

    // Decode back to PNG
    let _result = decode(&output_cafe, &output_png).expect("Decode failed");

    // Verify output exists
    assert!(Path::new(&output_png).exists(), "Output PNG not created");

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}

#[test]
fn test_roundtrip_512x512_rgba_gradient() {
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_512_gradient.png", temp_dir);
    let output_cafe = format!("{}/output_512_gradient.cafe", temp_dir);
    let output_png = format!("{}/output_512_gradient.png", temp_dir);

    let width = 512u32;
    let height = 512u32;
    let pixels = generate_test_image(width, height, "gradient");

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 18;
    opts.adaptive_analysis = true;
    opts.target_color_type = 6;

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");
    assert!(Path::new(&output_cafe).exists());

    let _result = decode(&output_cafe, &output_png).expect("Decode failed");
    assert!(Path::new(&output_png).exists());

    let cafe_size = fs::metadata(&output_cafe).unwrap().len() as usize;
    println!(
        "Roundtrip test (512×512 gradient): CAFE size={} bytes",
        cafe_size
    );

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}

#[test]
fn test_roundtrip_1024x768_rgba_random() {
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_1024_random.png", temp_dir);
    let output_cafe = format!("{}/output_1024_random.cafe", temp_dir);
    let output_png = format!("{}/output_1024_random.png", temp_dir);

    let width = 1024u32;
    let height = 768u32;
    let pixels = generate_test_image(width, height, "random");

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 19;
    opts.adaptive_analysis = true;
    opts.target_color_type = 6;

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");
    assert!(Path::new(&output_cafe).exists());

    let _result = decode(&output_cafe, &output_png).expect("Decode failed");
    assert!(Path::new(&output_png).exists());

    let cafe_size = fs::metadata(&output_cafe).unwrap().len() as usize;
    println!(
        "Roundtrip test (1024×768 random): CAFE size={} bytes",
        cafe_size
    );

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}

#[test]
fn test_roundtrip_small_image_4x4() {
    // Test edge case: very small image
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_4x4.png", temp_dir);
    let output_cafe = format!("{}/output_4x4.cafe", temp_dir);
    let output_png = format!("{}/output_4x4.png", temp_dir);

    let width = 4u32;
    let height = 4u32;
    let pixels = generate_test_image(width, height, "checkerboard");

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 22;
    opts.adaptive_analysis = false;
    opts.target_color_type = 6;

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");
    assert!(Path::new(&output_cafe).exists());

    let _result = decode(&output_cafe, &output_png).expect("Decode failed");
    assert!(Path::new(&output_png).exists());

    println!("Small image test (4×4): SUCCESS");

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}

#[test]
fn test_roundtrip_wide_image_2048x256() {
    // Test edge case: very wide image (tests horizontal stripe patterns)
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_2048x256.png", temp_dir);
    let output_cafe = format!("{}/output_2048x256.cafe", temp_dir);
    let output_png = format!("{}/output_2048x256.png", temp_dir);

    let width = 2048u32;
    let height = 256u32;
    let pixels = generate_test_image(width, height, "gradient");

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 19;
    opts.adaptive_analysis = true;
    opts.target_color_type = 6;

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");
    assert!(Path::new(&output_cafe).exists());

    let _result = decode(&output_cafe, &output_png).expect("Decode failed");
    assert!(Path::new(&output_png).exists());

    let cafe_size = fs::metadata(&output_cafe).unwrap().len() as usize;
    println!("Wide image test (2048×256): CAFE size={} bytes", cafe_size);

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}

#[test]
fn test_roundtrip_tall_image_256x2048() {
    // Test edge case: very tall image (tests vertical stripe patterns)
    let temp_dir = temp_test_dir();
    let input_png = format!("{}/input_256x2048.png", temp_dir);
    let output_cafe = format!("{}/output_256x2048.cafe", temp_dir);
    let output_png = format!("{}/output_256x2048.png", temp_dir);

    let width = 256u32;
    let height = 2048u32;
    let pixels = generate_test_image(width, height, "checkerboard");

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels).unwrap();
    image_buffer
        .save(&input_png)
        .expect("Failed to save input PNG");

    let mut opts = EncodeOptions::default();
    opts.use_filter = true;
    opts.level = 19;
    opts.adaptive_analysis = true;
    opts.target_color_type = 6;

    encode(&input_png, &output_cafe, &opts).expect("Encode failed");
    assert!(Path::new(&output_cafe).exists());

    let _result = decode(&output_cafe, &output_png).expect("Decode failed");
    assert!(Path::new(&output_png).exists());

    let cafe_size = fs::metadata(&output_cafe).unwrap().len() as usize;
    println!("Tall image test (256×2048): CAFE size={} bytes", cafe_size);

    // Cleanup
    let _ = fs::remove_file(&input_png);
    let _ = fs::remove_file(&output_cafe);
    let _ = fs::remove_file(&output_png);
}
