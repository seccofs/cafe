#![no_main]

use libfuzzer_sys::fuzz_target;
use cafe::decode_bytes;

fuzz_target!(|data: &[u8]| {
    // Attempt to decode arbitrary bytes. Success or Err is expected.
    // The only bad outcome is panic or hang.
    let _ = decode_bytes(data);
});
