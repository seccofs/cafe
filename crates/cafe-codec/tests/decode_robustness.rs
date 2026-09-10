//! Robustness tests for `decode_bytes`.
//!
//! These tests exercise `decode_bytes()` with various malformed inputs to
//! confirm it never panics, only ever returns `Err` — the same contract
//! `fuzz/fuzz_targets/decode_fuzz.rs` checks continuously against
//! arbitrary bytes (spec section 8.1). Adapted from the frozen `old/`
//! lineage's `tests/decode_robustness.rs`, updated for `cafe-codec`'s
//! `Result<DecodedImage>` return type.

use cafe_codec::decode_bytes;

/// Completely empty buffer.
#[test]
fn test_decode_empty_buffer() {
    let result = decode_bytes(&[]);
    assert!(result.is_err(), "Empty buffer should return Err");
}

/// Buffer with just a few bytes (incomplete signature).
#[test]
fn test_decode_truncated_signature() {
    let buf = [0x89, 0x43, 0x41, 0x46, 0x45]; // only 5 bytes of the 9-byte signature
    let result = decode_bytes(&buf);
    assert!(result.is_err(), "Truncated signature should return Err");
}

/// Invalid signature entirely.
#[test]
fn test_decode_invalid_signature() {
    let buf = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let result = decode_bytes(&buf);
    assert!(result.is_err(), "Invalid signature should return Err");
}

/// Valid signature but nothing follows it (no IHDR).
#[test]
fn test_decode_valid_signature_truncated_ihdr() {
    let buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];
    let result = decode_bytes(&buf);
    assert!(result.is_err(), "Signature valid but no IHDR should error");
}

/// Random garbage after a valid signature — must not panic.
#[test]
fn test_decode_garbage_after_signature() {
    let buf = [
        0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, // random bytes
    ];
    let _ = decode_bytes(&buf);
}

/// Chunk with a forged huge length but no matching data.
#[test]
fn test_decode_forged_chunk_length() {
    let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];
    buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Length ~ 4 GiB
    buf.extend_from_slice(b"IHDR");
    buf.extend_from_slice(&[0x00]); // Flag
                                    // No data/CRC follows -> truncated file.
    let result = decode_bytes(&buf);
    assert!(
        result.is_err(),
        "Forged length should cause a truncation/limit error, not a panic"
    );
}

/// 1000 random-looking bytes after the signature — must not panic.
#[test]
fn test_decode_random_bytes_1000() {
    let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];
    buf.extend_from_slice(&[0xFF; 1000]);
    let _ = decode_bytes(&buf);
}

/// Zero width in IHDR must be rejected, not panic.
#[test]
fn test_decode_zero_width_ihdr() {
    let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];

    // IHDR payload is 12 bytes in the 0.1 format: Width(4) Height(4)
    // BitDepth(1) SampleFormat(1) ColorType(1) CompressionMethod(1).
    buf.extend_from_slice(&(12u32).to_be_bytes());
    buf.extend_from_slice(b"IHDR");
    buf.push(0x00); // Flag
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Width = 0 (invalid)
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // Height = 1
    buf.push(0x08); // bit_depth
    buf.push(0x00); // sample_format = UINT
    buf.push(0x06); // color_type = RGBA
    buf.push(0x00); // compression_method
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // dummy CRC (fails CRC before reaching validation, both are Err)

    let result = decode_bytes(&buf);
    assert!(result.is_err(), "Zero width should be rejected");
}

/// Very large width/height must not overflow/panic on the way to an error.
#[test]
fn test_decode_huge_dimensions() {
    let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];
    buf.extend_from_slice(&(12u32).to_be_bytes());
    buf.extend_from_slice(b"IHDR");
    buf.push(0x00);
    buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Width ~ 4 GiB
    buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Height ~ 4 GiB
    buf.push(0x08);
    buf.push(0x00);
    buf.push(0x06);
    buf.push(0x00);
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // dummy CRC
    let _ = decode_bytes(&buf);
}

/// A grab-bag of malformed inputs run in rapid succession, to catch any
/// state leaking between calls (there shouldn't be any — `decode_bytes`
/// takes no persistent state).
#[test]
fn test_decode_rapid_malformed_inputs() {
    let malformed_inputs: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x00],
        vec![0x89, 0x43, 0x41],
        vec![0xFF; 100],
        vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A, 0x00],
    ];

    for input in malformed_inputs {
        let _ = decode_bytes(&input);
    }
}

/// Truncating a known-valid golden file at every possible byte offset must
/// never panic — only ever return `Err` (or, at the exact full length,
/// `Ok`). This is a cheap, deterministic stand-in for what a fuzzer would
/// eventually discover by random mutation.
#[test]
fn test_decode_truncated_golden_at_every_offset_never_panics() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("golden")
        .join("minimal_2x2_rgba.cafe");
    let full = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path:?}: {e}"));

    for len in 0..=full.len() {
        let _ = decode_bytes(&full[..len]);
    }
}

/// Flipping every single byte (one at a time) of a known-valid golden file
/// must never panic. Most flips should be rejected (CRC mismatch or
/// structural violation); a few may coincidentally still decode, which is
/// fine — the only forbidden outcome is a panic.
#[test]
fn test_decode_bitflipped_golden_never_panics() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("golden")
        .join("minimal_2x2_rgba.cafe");
    let full = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path:?}: {e}"));

    for i in 0..full.len() {
        for bit in 0..8u8 {
            let mut mutated = full.clone();
            mutated[i] ^= 1 << bit;
            let _ = decode_bytes(&mutated);
        }
    }
}
