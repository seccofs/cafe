//! Property-based ("light fuzz") tests for `chunk::read_chunk`.
//!
//! Complements `fuzz/fuzz_targets/chunk_roundtrip_fuzz.rs` (real
//! libFuzzer coverage, Linux/nightly-only) with proptest properties
//! runnable anywhere, including local Windows development. Mirrors
//! `crates/cafe-codec/tests/roundtrip_proptest.rs`'s scope one layer
//! down, at the raw chunk-framing level rather than the whole-file
//! decoder level.

use cafe_format::chunk::{read_chunk, write_chunk};
use proptest::prelude::*;

/// `read_chunk` must never panic on arbitrary bytes at any offset — only
/// ever return `Err` or (rarely, coincidentally) `Ok`.
#[test]
fn prop_read_chunk_never_panics_on_arbitrary_input() {
    proptest!(|(data in prop::collection::vec(0u8..=u8::MAX, 0..2048), offset in 0usize..64)| {
        let _ = read_chunk(&data, offset);
    });
}

/// A well-formed chunk (produced by `write_chunk`) with a single bit
/// flipped anywhere must never panic when re-parsed — it should either
/// still parse (extremely rare: only if the flip lands somewhere that
/// doesn't affect Length/Type/CRC validity) or return `Err`.
#[test]
fn prop_read_chunk_bitflipped_wellformed_chunk_never_panics() {
    proptest!(|(
        chunk_type in prop::array::uniform4(prop::sample::select(vec![
            b'A', b'B', b'I', b'D', b'H', b'R', b'N', b'a', b'z',
        ])),
        flag in 0u8..=1,
        payload in prop::collection::vec(0u8..=u8::MAX, 0..256),
        flip_index in 0usize..300,
        flip_bit in 0u8..8,
    )| {
        let bytes = write_chunk(&chunk_type, flag, &payload);
        if flip_index < bytes.len() {
            let mut mutated = bytes.clone();
            mutated[flip_index] ^= 1 << flip_bit;
            let _ = read_chunk(&mutated, 0);
        }
    });
}

/// Truncating a well-formed chunk at every possible length must never
/// panic.
#[test]
fn prop_read_chunk_truncated_wellformed_chunk_never_panics() {
    proptest!(|(
        payload in prop::collection::vec(0u8..=u8::MAX, 0..128),
    )| {
        let bytes = write_chunk(b"IDAT", 0x00, &payload);
        for len in 0..=bytes.len() {
            let _ = read_chunk(&bytes[..len], 0);
        }
    });
}
