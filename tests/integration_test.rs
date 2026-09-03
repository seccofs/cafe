//! Integration tests that encode/decode images.
//!
//! Unlike the previous version (which "skips" when an external test image
//! does not exist), these tests generate the input image in memory via the
//! `image` crate, ensuring they always run without depending on external files.

use cafe::{cHDR, decode, decode_with_opts, encode, EncodeOptions, ToneMapOperator};

/// Creates a small deterministic RGBA image in a temporary directory.
fn make_test_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(32, 24);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            ((x * 11) % 256) as u8,
            ((y * 13) % 256) as u8,
            ((x + y * 5) % 256) as u8,
            255,
        ]);
    }
    img
}

struct Paths {
    input: std::path::PathBuf,
    cafe: std::path::PathBuf,
    out: std::path::PathBuf,
}

fn roundtrip_paths(label: &str) -> Paths {
    let dir = std::env::temp_dir().join("cafe_integration");
    std::fs::create_dir_all(&dir).unwrap();
    Paths {
        input: dir.join(format!("{label}_in.png")),
        cafe: dir.join(format!("{label}.cafe")),
        out: dir.join(format!("{label}_out.png")),
    }
}

#[test]
fn test_encode_with_chdr_metadata() {
    let p = roundtrip_paths("chdr");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        // Set HDR metadata (cHDR chunk, v1.0)
        chdr_metadata: Some(cHDR {
            transfer_function: 1, // PQ transfer function
            color_primaries: 1,   // BT.2020 color primaries
            max_luminance: 10000.0,
            min_luminance: 0.0005,
            max_cll: Some(1000),
            max_fall: Some(500),
        }),
        ..EncodeOptions::default()
    };

    encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts).expect("encode failed");

    let result =
        decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode failed");

    let chdr = result
        .chdr_metadata
        .expect("cHDR metadata should be present");
    assert_eq!(chdr.transfer_function, 1);
    assert_eq!(chdr.color_primaries, 1);
    assert_eq!(chdr.max_luminance, 10000.0);
    assert_eq!(chdr.min_luminance, 0.0005);
    assert_eq!(chdr.max_cll, Some(1000));
    assert_eq!(chdr.max_fall, Some(500));
}

#[test]
fn test_encode_with_sample_format_float() {
    let p = roundtrip_paths("float");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(1), // SAMPLE_FORMAT_FLOAT
        ..EncodeOptions::default()
    };

    encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect("encode with float failed");
    decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode with float failed");
    assert!(p.out.exists(), "Decoded PNG should exist");
}

/// Creates a small deterministic RGBA image whose channel values stay
/// comfortably below the ~187/255 threshold where Reinhard's inverse
/// (`ToneMapOperator::apply_inverse`, domain-restricted to compressed
/// values in `[0, 0.5]` — see `src/tonemap.rs`'s doc comments) can no
/// longer round-trip exactly through the full `tonemap_hdr` decode path.
fn make_dim_test_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::new(16, 12);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            ((x * 7) % 150) as u8,
            ((y * 9) % 150) as u8,
            ((x + y * 3) % 150) as u8,
            255,
        ]);
    }
    img
}

#[test]
fn test_encode_with_inverse_tonemap_reinhard_roundtrips_end_to_end() {
    let p = roundtrip_paths("inverse_tonemap_reinhard");
    make_dim_test_image().save(&p.input).unwrap();

    let chdr = cHDR {
        transfer_function: 0, // linear — the only value inverse_tonemap supports
        color_primaries: 0,
        max_luminance: 1000.0,
        min_luminance: 0.001,
        max_cll: None,
        max_fall: None,
    };

    let opts = EncodeOptions {
        sample_format: Some(1), // SAMPLE_FORMAT_FLOAT (required by inverse_tonemap)
        chdr_metadata: Some(chdr),
        inverse_tonemap: Some(ToneMapOperator::Reinhard),
        ..EncodeOptions::default()
    };

    encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect("encode with inverse_tonemap failed");

    // Decode with the matching operator so tonemap_hdr's forward path is
    // the exact inverse of what apply_inverse_tone_mapping_to_image did.
    let decode_opts = EncodeOptions {
        tonemap_operator: ToneMapOperator::Reinhard,
        ..EncodeOptions::default()
    };
    decode_with_opts(
        &p.cafe.to_string_lossy(),
        &p.out.to_string_lossy(),
        &decode_opts,
    )
    .expect("decode with inverse_tonemap-produced file failed");

    let original = make_dim_test_image();
    let decoded = image::open(&p.out).unwrap().to_rgba8();
    assert_eq!(original.dimensions(), decoded.dimensions());

    // Not bit-identical (inverse tone-mapping is a lossy round-trip through
    // linear-light space and float32 quantization), but should be close.
    let mut max_diff = 0i32;
    for (orig_px, dec_px) in original.pixels().zip(decoded.pixels()) {
        for c in 0..3 {
            let diff = (orig_px[c] as i32 - dec_px[c] as i32).abs();
            max_diff = max_diff.max(diff);
        }
    }
    assert!(
        max_diff <= 3,
        "inverse-tonemap round-trip diverged too much: max_diff={max_diff}"
    );
}

#[test]
fn test_inverse_tonemap_rejects_uint_sample_format() {
    let p = roundtrip_paths("inverse_tonemap_rejects_uint");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        // sample_format left as None (uint, default) — inverse_tonemap requires float
        chdr_metadata: Some(cHDR {
            transfer_function: 0,
            ..cHDR::new()
        }),
        inverse_tonemap: Some(ToneMapOperator::Reinhard),
        ..EncodeOptions::default()
    };

    let err = encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect_err("encode should reject inverse_tonemap with non-float sample_format");
    assert!(matches!(err, cafe::CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_inverse_tonemap_rejects_missing_chdr() {
    let p = roundtrip_paths("inverse_tonemap_rejects_no_chdr");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(1),
        chdr_metadata: None, // missing — required by inverse_tonemap
        inverse_tonemap: Some(ToneMapOperator::Reinhard),
        ..EncodeOptions::default()
    };

    let err = encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect_err("encode should reject inverse_tonemap without chdr_metadata");
    assert!(matches!(err, cafe::CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_inverse_tonemap_rejects_non_linear_transfer_function() {
    let p = roundtrip_paths("inverse_tonemap_rejects_non_linear");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(1),
        chdr_metadata: Some(cHDR {
            transfer_function: 3, // sRGB — not supported by inverse_tonemap (linear only)
            ..cHDR::new()
        }),
        inverse_tonemap: Some(ToneMapOperator::Reinhard),
        ..EncodeOptions::default()
    };

    let err = encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect_err("encode should reject inverse_tonemap with non-linear transfer_function");
    assert!(matches!(err, cafe::CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_inverse_tonemap_rejects_filmic_operator() {
    let p = roundtrip_paths("inverse_tonemap_rejects_filmic");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(1),
        chdr_metadata: Some(cHDR {
            transfer_function: 0,
            ..cHDR::new()
        }),
        inverse_tonemap: Some(ToneMapOperator::Filmic), // no closed-form inverse
        ..EncodeOptions::default()
    };

    let err = encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect_err("encode should reject inverse_tonemap with Filmic operator");
    assert!(matches!(err, cafe::CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_inverse_tonemap_rejects_non_rgba_color_type() {
    let p = roundtrip_paths("inverse_tonemap_rejects_non_rgba");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(1),
        target_color_type: cafe::constants::COLOR_TYPE_RGB,
        chdr_metadata: Some(cHDR {
            transfer_function: 0,
            ..cHDR::new()
        }),
        inverse_tonemap: Some(ToneMapOperator::Reinhard),
        ..EncodeOptions::default()
    };

    let err = encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect_err("encode should reject inverse_tonemap with non-RGBA target_color_type");
    assert!(matches!(err, cafe::CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_encode_with_sample_format_half() {
    let p = roundtrip_paths("half");
    make_test_image().save(&p.input).unwrap();

    let opts = EncodeOptions {
        sample_format: Some(2), // SAMPLE_FORMAT_HALF
        ..EncodeOptions::default()
    };

    encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts)
        .expect("encode with half failed");
    decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode with half failed");
    assert!(p.out.exists(), "Decoded PNG should exist");
}
