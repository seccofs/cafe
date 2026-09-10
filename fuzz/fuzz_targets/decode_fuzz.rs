#![no_main]

use cafe_codec::decode_bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Attempt to decode arbitrary bytes. Success or Err is expected.
    // The only bad outcome is panic or hang.
    let _ = decode_bytes(data);
});
