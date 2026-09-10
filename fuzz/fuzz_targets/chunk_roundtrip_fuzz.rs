#![no_main]

use cafe_format::chunk::read_chunk;
use libfuzzer_sys::fuzz_target;

// Fuzz test for chunk read/write roundtrip.
//
// Unlike the frozen `old/` lineage (where chunk framing functions were
// private and this harness could only exercise them indirectly through
// `decode_bytes`), `cafe_format::chunk::read_chunk` is public — so this
// harness calls it directly on arbitrary byte sequences at every offset
// the fuzzer chooses, ensuring it never panics on truncated, forged, or
// malformed input (spec section 8.1). It also drives `decode_bytes` for
// end-to-end chunk-sequencing coverage that a single `read_chunk` call
// can't reach on its own.
fuzz_target!(|data: &[u8]| {
    let _ = read_chunk(data, 0);
    let _ = cafe_codec::decode_bytes(data);
});
