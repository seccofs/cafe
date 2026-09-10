//! Property-based ("light fuzz") tests for `cafe-codec`.
//!
//! Complements `fuzz/fuzz_targets/decode_fuzz.rs` (real libFuzzer coverage
//! guided by code-coverage feedback, Linux/nightly-only) with proptest
//! properties runnable anywhere, including local Windows development,
//! where libFuzzer's coverage instrumentation doesn't link under MSVC.

use cafe_codec::{decode_bytes, encode_bytes, EncoderOptions};
use cafe_format::constants::{
    COLOR_TYPE_GRAY, COLOR_TYPE_GRAY_ALPHA, COLOR_TYPE_RGB, COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT,
};
use proptest::prelude::*;

/// `decode_bytes` must never panic on arbitrary byte sequences of any
/// length up to a few KiB — only ever return `Err` or (rarely,
/// coincidentally) `Ok`.
#[test]
fn prop_decode_bytes_never_panics_on_arbitrary_input() {
    proptest!(|(data in prop::collection::vec(0u8..=u8::MAX, 0..4096))| {
        let _ = decode_bytes(&data);
    });
}

/// `decode_bytes` must never panic even when the input starts with a
/// genuine CAFE signature (increasing the odds the decoder proceeds
/// further into chunk parsing before hitting malformed data).
#[test]
fn prop_decode_bytes_never_panics_with_valid_signature_prefix() {
    proptest!(|(tail in prop::collection::vec(0u8..=u8::MAX, 0..2048))| {
        let mut buf = cafe_format::constants::SIGNATURE.to_vec();
        buf.extend(tail);
        let _ = decode_bytes(&buf);
    });
}

fn arbitrary_color_type() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(COLOR_TYPE_GRAY),
        Just(COLOR_TYPE_RGB),
        Just(COLOR_TYPE_GRAY_ALPHA),
        Just(COLOR_TYPE_RGBA),
    ]
}

fn channels_for(color_type: u8) -> usize {
    cafe_format::constants::channels_for_color_type(color_type).unwrap() as usize
}

/// `encode_bytes` followed by `decode_bytes` must reproduce the original
/// pixel buffer exactly, for small random images across every direct
/// color type and both single-tile and multi-tile layouts.
#[test]
fn prop_encode_decode_roundtrip_arbitrary_small_image() {
    proptest!(|(
        width in 1u32..=12,
        height in 1u32..=12,
        color_type in arbitrary_color_type(),
        seed in 0u64..=u64::MAX,
        use_tiling in any::<bool>(),
    )| {
        let channels = channels_for(color_type);
        let num_samples = width as usize * height as usize * channels;
        let pixels: Vec<u8> = (0..num_samples)
            .map(|i| ((seed.wrapping_add(i as u64).wrapping_mul(2654435761)) % 256) as u8)
            .collect();

        let opts = EncoderOptions {
            tile_size: if use_tiling { Some((3, 3)) } else { None },
            ..Default::default()
        };

        let encoded = encode_bytes(width, height, 8, SAMPLE_FORMAT_UINT, color_type, &pixels, opts)
            .expect("encode_bytes should succeed for a valid small image");
        let decoded = decode_bytes(&encoded).expect("decode_bytes should succeed on our own output");

        prop_assert_eq!(decoded.pixels, pixels);
        prop_assert_eq!(decoded.ihdr.width, width);
        prop_assert_eq!(decoded.ihdr.height, height);
        prop_assert_eq!(decoded.ihdr.color_type, color_type);
    });
}
