//! Robustness tests for decode_bytes.
//! These tests exercise decode_bytes() with various malformed inputs
//! to ensure it never panics, only returns Err.

#[cfg(test)]
mod decode_robustness_tests {
    use cafe::decode_bytes;

    /// Test: completely empty buffer
    #[test]
    fn test_decode_empty_buffer() {
        let result = decode_bytes(&[]);
        assert!(result.is_err(), "Empty buffer should return Err");
    }

    /// Test: buffer with just a few bytes (incomplete signature)
    #[test]
    fn test_decode_truncated_signature() {
        let buf = [0x89, 0x43, 0x41, 0x46, 0x45]; // Only 5 bytes of CAFE sig
        let result = decode_bytes(&buf);
        assert!(result.is_err(), "Truncated signature should return Err");
    }

    /// Test: invalid signature
    #[test]
    fn test_decode_invalid_signature() {
        let buf = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let result = decode_bytes(&buf);
        assert!(result.is_err(), "Invalid signature should return Err");
    }

    /// Test: valid signature but truncated IHDR
    #[test]
    fn test_decode_valid_signature_truncated_ihdr() {
        let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]; // CAFE signature
        // No IHDR chunk follows
        let result = decode_bytes(&buf);
        assert!(result.is_err(), "Signature valid but no IHDR should error");
    }

    /// Test: random garbage after signature
    #[test]
    fn test_decode_garbage_after_signature() {
        let buf = [
            0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A, // CAFE sig
            0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, // Random bytes
        ];
        let result = decode_bytes(&buf);
        // May error on parsing, but should NOT panic
        let _ = result;
    }

    /// Test: chunk with forged length (would cause allocation failure)
    #[test]
    fn test_decode_forged_chunk_length() {
        let mut buf = vec![
            0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A, // CAFE sig
        ];
        // Add a chunk with HUGE length field (without actual data)
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Length = 4GB
        buf.extend_from_slice(b"IHDR"); // Chunk type
        buf.extend_from_slice(&[0x00]); // Flag
        // CRC follows, but no data => truncated file error
        let result = decode_bytes(&buf);
        assert!(result.is_err(), "Forged length should cause truncation error");
    }

    /// Test: 1000 random bytes
    #[test]
    fn test_decode_random_bytes_1000() {
        let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]; // CAFE sig
        buf.extend_from_slice(&[0xFF; 1000]);
        let result = decode_bytes(&buf);
        // Should error gracefully, not panic
        let _ = result;
    }

    /// Test: zero width in IHDR
    #[test]
    fn test_decode_zero_width_ihdr() {
        let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]; // CAFE sig
        
        // Minimal IHDR chunk: length=14, type=IHDR, flag=0x00, data=14 bytes, crc32=4 bytes
        buf.extend_from_slice(&(14u32).to_be_bytes()); // Length = 14
        buf.extend_from_slice(b"IHDR"); // Type
        buf.push(0x00); // Flag (not compressed)
        
        // IHDR data (14 bytes):
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Width = 0 (invalid!)
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // Height = 1
        buf.push(0x08); // bit_depth = 8
        buf.push(0x00); // sample_format = UINT
        buf.push(0x06); // color_type = RGBA
        buf.push(0x00); // compression_method
        buf.push(0x00); // filter_method
        buf.push(0x00); // interlace_method
        
        // CRC32 (dummy, will fail CRC check but that's fine)
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        
        // IEND chunk
        buf.extend_from_slice(&(0u32).to_be_bytes()); // Length = 0
        buf.extend_from_slice(b"IEND");
        buf.push(0x00); // Flag
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Dummy CRC
        
        let result = decode_bytes(&buf);
        // Should error on width=0, not panic
        assert!(result.is_err(), "Zero width should be rejected");
    }

    /// Test: zero height in IHDR
    #[test]
    fn test_decode_zero_height_ihdr() {
        let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]; // CAFE sig
        
        // Minimal IHDR chunk
        buf.extend_from_slice(&(14u32).to_be_bytes()); // Length = 14
        buf.extend_from_slice(b"IHDR"); // Type
        buf.push(0x00); // Flag
        
        // IHDR data (14 bytes):
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // Width = 1
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Height = 0 (invalid!)
        buf.push(0x08); // bit_depth = 8
        buf.push(0x00); // sample_format = UINT
        buf.push(0x06); // color_type = RGBA
        buf.push(0x00); // compression_method
        buf.push(0x00); // filter_method
        buf.push(0x00); // interlace_method
        
        // CRC32 (dummy)
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        
        // IEND chunk
        buf.extend_from_slice(&(0u32).to_be_bytes()); // Length = 0
        buf.extend_from_slice(b"IEND");
        buf.push(0x00); // Flag
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Dummy CRC
        
        let result = decode_bytes(&buf);
        assert!(result.is_err(), "Zero height should be rejected");
    }

    /// Test: very large width/height (would overflow)
    #[test]
    fn test_decode_huge_dimensions() {
        let mut buf = vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]; // CAFE sig
        
        buf.extend_from_slice(&(14u32).to_be_bytes()); // Length = 14
        buf.extend_from_slice(b"IHDR");
        buf.push(0x00); // Flag
        
        // IHDR data:
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Width = 4GB
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Height = 4GB
        buf.push(0x08); // bit_depth = 8
        buf.push(0x00); // sample_format = UINT
        buf.push(0x06); // color_type = RGBA
        buf.push(0x00);
        buf.push(0x00);
        buf.push(0x00);
        
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Dummy CRC
        
        // IEND
        buf.extend_from_slice(&(0u32).to_be_bytes());
        buf.extend_from_slice(b"IEND");
        buf.push(0x00);
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        
        let result = decode_bytes(&buf);
        // May error on IDAT processing or budget, but should NOT panic
        let _ = result;
    }

    /// Test: multiple test cases in rapid succession (no panic memory corruption)
    #[test]
    fn test_decode_rapid_malformed_inputs() {
        let malformed_inputs = vec![
            vec![],
            vec![0x00],
            vec![0x89, 0x43, 0x41],
            vec![0xFF; 100],
            vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A, 0x00],
        ];
        
        for input in malformed_inputs {
            let result = decode_bytes(&input);
            // Should never panic, result doesn't matter
            let _ = result;
        }
    }
}
