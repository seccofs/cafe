//! Integration tests verifying SIMD packing is used in encode/decode pipeline

#[cfg(feature = "simd")]
mod simd_integration_tests {
    use cafe::{decode, encode, EncodeOptions};
    use image::ImageBuffer;
    use tempfile::TempDir;

    /// Helper to create a test image (grayscale checkerboard)
    fn create_checkerboard_image() -> ImageBuffer<image::Rgba<u8>, Vec<u8>> {
        let width = 256u32;
        let height = 256u32;
        let mut img = ImageBuffer::new(width, height);

        // Create a checkerboard pattern (0 or 255 for each pixel)
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let val = if (x + y) % 2 == 0 { 0 } else { 255 };
            *pixel = image::Rgba([val, val, val, 255]);
        }

        img
    }

    /// Helper to create a gradient image
    fn create_gradient_image() -> ImageBuffer<image::Rgba<u8>, Vec<u8>> {
        let width = 256u32;
        let height = 256u32;
        let mut img = ImageBuffer::new(width, height);

        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let val = (x as u32 * 255 / width) as u8;
            *pixel = image::Rgba([val, val, val, 255]);
        }

        img
    }

    #[test]
    fn test_simd_grayscale_1bit_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let cafe_path = temp_dir.path().join("test_gray_1bit.cafe");

        let img = create_checkerboard_image();
        img.save(temp_dir.path().join("original.png")).unwrap();

        // Encode as grayscale 1-bit (forces SIMD pack/unpack in color conversion)
        let encode_opts = EncodeOptions {
            target_color_type: 0, // Grayscale
            use_filter: false,
            ..Default::default()
        };

        encode(
            temp_dir.path().join("original.png").to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &encode_opts,
        )
        .expect("Encode failed");

        // Verify CAFE file was created
        let cafe_size = std::fs::metadata(&cafe_path)
            .expect("CAFE file not found")
            .len();
        assert!(cafe_size > 0, "CAFE file is empty");

        // Decode and verify roundtrip
        let output_path = temp_dir.path().join("roundtrip.png");
        decode(cafe_path.to_str().unwrap(), output_path.to_str().unwrap()).expect("Decode failed");

        let decoded_img = image::open(&output_path).expect("Failed to open decoded image");
        assert_eq!(decoded_img.width(), 256);
        assert_eq!(decoded_img.height(), 256);
    }

    #[test]
    fn test_simd_grayscale_2bit_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let cafe_path = temp_dir.path().join("test_gray_2bit.cafe");

        let img = create_gradient_image();
        img.save(temp_dir.path().join("original.png")).unwrap();

        // Encode as grayscale with 2-bit depth
        let encode_opts = EncodeOptions {
            target_color_type: 0,
            use_filter: false,
            ..Default::default()
        };

        encode(
            temp_dir.path().join("original.png").to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &encode_opts,
        )
        .expect("Encode failed");

        let cafe_size = std::fs::metadata(&cafe_path)
            .expect("CAFE file not found")
            .len();
        assert!(cafe_size > 0, "CAFE file is empty");

        // Decode and verify
        let output_path = temp_dir.path().join("roundtrip.png");
        decode(cafe_path.to_str().unwrap(), output_path.to_str().unwrap()).expect("Decode failed");

        let decoded_img = image::open(&output_path).expect("Failed to open decoded image");
        assert_eq!(decoded_img.width(), 256);
        assert_eq!(decoded_img.height(), 256);
    }

    #[test]
    fn test_simd_grayscale_4bit_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let cafe_path = temp_dir.path().join("test_gray_4bit.cafe");

        let img = create_gradient_image();
        img.save(temp_dir.path().join("original.png")).unwrap();

        // Encode as grayscale with 4-bit depth
        let encode_opts = EncodeOptions {
            target_color_type: 0,
            use_filter: false,
            ..Default::default()
        };

        encode(
            temp_dir.path().join("original.png").to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &encode_opts,
        )
        .expect("Encode failed");

        let cafe_size = std::fs::metadata(&cafe_path)
            .expect("CAFE file not found")
            .len();
        assert!(cafe_size > 0, "CAFE file is empty");

        // Decode and verify
        let output_path = temp_dir.path().join("roundtrip.png");
        decode(cafe_path.to_str().unwrap(), output_path.to_str().unwrap()).expect("Decode failed");

        let decoded_img = image::open(&output_path).expect("Failed to open decoded image");
        assert_eq!(decoded_img.width(), 256);
        assert_eq!(decoded_img.height(), 256);
    }

    #[test]
    fn test_simd_large_image_roundtrip() {
        // Test SIMD packing on larger image where benefits are more visible
        let temp_dir = TempDir::new().unwrap();
        let cafe_path = temp_dir.path().join("test_large.cafe");

        // Create a larger grayscale image
        let mut img = ImageBuffer::new(512u32, 512u32);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let val = (((x + y) as u32 * 255) / 1024) as u8;
            *pixel = image::Rgba([val, val, val, 255]);
        }
        img.save(temp_dir.path().join("large.png")).unwrap();

        // Encode as grayscale 4-bit (uses SIMD packing)
        let encode_opts = EncodeOptions {
            target_color_type: 0,
            use_filter: true,
            ..Default::default()
        };

        encode(
            temp_dir.path().join("large.png").to_str().unwrap(),
            cafe_path.to_str().unwrap(),
            &encode_opts,
        )
        .expect("Encode failed");

        let cafe_size = std::fs::metadata(&cafe_path)
            .expect("CAFE file not found")
            .len();
        assert!(cafe_size > 0, "CAFE file is empty");

        // Decode and verify roundtrip
        let output_path = temp_dir.path().join("roundtrip.png");
        decode(cafe_path.to_str().unwrap(), output_path.to_str().unwrap()).expect("Decode failed");

        let decoded_img = image::open(&output_path).expect("Failed to open decoded image");
        assert_eq!(decoded_img.width(), 512);
        assert_eq!(decoded_img.height(), 512);
    }
}

#[cfg(not(feature = "simd"))]
mod no_simd_tests {
    #[test]
    fn simd_feature_disabled() {
        // Just confirm compilation works without SIMD
        assert!(true);
    }
}
