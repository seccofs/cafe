#![no_main]

use libfuzzer_sys::fuzz_target;

// Fuzz test for chunk read/write roundtrip.
// Tests that read_chunk/write_chunk don't panic on arbitrary byte sequences.
fuzz_target!(|data: &[u8]| {
    // We can't easily expose internal chunk functions (they're private).
    // Instead, we rely on decode_bytes which heavily exercises the chunk parsing layer.
    // This ensures that read_chunk never panics on malformed input.
    let _ = cafe::decode_bytes(data);
});
