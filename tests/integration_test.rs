//! Integration tests that encode/decode images.
//!
//! Unlike the previous version (which "skips" when an external test image
//! does not exist), these tests generate the input image in memory via the
//! `image` crate, ensuring they always run without depending on external files.

use cafe::{cHDR, decode, encode, EncodeOptions};

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

    encode(&p.input.to_string_lossy(), &p.cafe.to_string_lossy(), &opts).expect("encode falhou");

    let result =
        decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode falhou");

    let chdr = result
        .chdr_metadata
        .expect("cHDR metadata deveria estar presente");
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
        .expect("encode com float falhou");
    decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode de float falhou");
    assert!(p.out.exists(), "PNG decodificado deveria existir");
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
        .expect("encode com half falhou");
    decode(&p.cafe.to_string_lossy(), &p.out.to_string_lossy()).expect("decode com half falhou");
    assert!(p.out.exists(), "PNG decodificado deveria existir");
}
