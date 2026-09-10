//! Chunk-walking helpers shared by `cafe inspect`/`verify`/`explain`.
//!
//! These are presentation-layer conveniences built entirely on
//! `cafe-format`'s public API (`validate_signature`/`read_chunk`) — no new
//! API surface is added to `cafe-format`/`cafe-codec` for this (per the
//! Phase 9 scope decision recorded in `AGENTS.md`: `cafe explain` gets its
//! own parsing here rather than exposing per-row predictor codes from
//! `cafe-codec::tile`).

use cafe_format::chunk::read_chunk;
use cafe_format::validate_signature;

/// One chunk's framing info, as seen while walking a `.cafe` file
/// front-to-back — enough for `inspect`'s listing without needing the
/// chunk's (possibly still-compressed) payload.
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub chunk_type: [u8; 4],
    pub flag: u8,
    pub offset: usize,
    pub data_len: usize,
    pub data: Vec<u8>,
}

impl ChunkInfo {
    pub fn type_str(&self) -> String {
        String::from_utf8_lossy(&self.chunk_type).to_string()
    }
}

/// Walks every chunk in `buf` starting right after the signature, stopping
/// after `IEND` (inclusive) or at end-of-buffer. Returns `Err` on the first
/// framing/CRC problem `cafe_format::read_chunk` finds — the caller
/// decides how to present that (this module has no opinion on error
/// formatting).
pub fn walk_chunks(buf: &[u8]) -> cafe_format::Result<Vec<ChunkInfo>> {
    let mut offset = validate_signature(buf)?;
    let mut chunks = Vec::new();

    while offset < buf.len() {
        let chunk_offset = offset;
        let chunk = read_chunk(buf, offset)?;
        offset = chunk.next_offset;
        let is_iend = &chunk.chunk_type == b"IEND";
        chunks.push(ChunkInfo {
            chunk_type: chunk.chunk_type,
            flag: chunk.flag,
            offset: chunk_offset,
            data_len: chunk.data.len(),
            data: chunk.data,
        });
        if is_iend {
            break;
        }
    }

    Ok(chunks)
}

/// Extracts just the predictor code byte prefixing each row of a
/// (already decompressed) `IDAT` payload (spec section 4.4: `for each
/// row: [predictor code: 1 byte][filtered row: bytes_per_row bytes]`),
/// without reversing the prediction — `cafe explain` only needs the codes
/// themselves (for a histogram), not reconstructed pixels.
///
/// Unlike `cafe_codec::tile::decode_tile_rows`, this never fails on an
/// invalid predictor code (`>= NUM_PREDICTORS`) — it reports whatever byte
/// is there, since `explain`'s job is to describe the file, including
/// malformed ones, not to reject them.
pub fn read_predictor_codes(payload: &[u8], tile_height: u32, bytes_per_row: u32) -> Vec<u8> {
    let bytes_per_row = bytes_per_row as usize;
    let mut codes = Vec::with_capacity(tile_height as usize);
    let mut offset = 0usize;
    for _ in 0..tile_height {
        if offset >= payload.len() {
            break;
        }
        codes.push(payload[offset]);
        offset += 1 + bytes_per_row;
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;
    use cafe_format::chunk::write_chunk;
    use cafe_format::constants::SIGNATURE;

    #[test]
    fn test_walk_chunks_lists_every_chunk_in_order() {
        let mut buf = SIGNATURE.to_vec();
        buf.extend(write_chunk(b"IHDR", 0x00, &[0u8; 12]));
        buf.extend(write_chunk(b"IDAT", 0x01, b"payload"));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        let chunks = walk_chunks(&buf).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].type_str(), "IHDR");
        assert_eq!(chunks[1].type_str(), "IDAT");
        assert_eq!(chunks[1].flag, 0x01);
        assert_eq!(chunks[2].type_str(), "IEND");
    }

    #[test]
    fn test_walk_chunks_stops_after_iend() {
        let mut buf = SIGNATURE.to_vec();
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        buf.extend(write_chunk(b"IHDR", 0x00, &[0u8; 12])); // trailing garbage
        let chunks = walk_chunks(&buf).unwrap();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_walk_chunks_propagates_crc_error() {
        let mut buf = SIGNATURE.to_vec();
        let mut chunk = write_chunk(b"IDAT", 0x00, b"payload");
        let len = chunk.len();
        chunk[len - 1] ^= 0xFF;
        buf.extend(chunk);
        let result = walk_chunks(&buf);
        assert!(matches!(
            result,
            Err(cafe_format::CafeError::CrcMismatch { .. })
        ));
    }

    #[test]
    fn test_read_predictor_codes_extracts_first_byte_per_row() {
        // 2 rows, bytes_per_row=3: [code0, a,b,c, code1, d,e,f]
        let payload = vec![5u8, 1, 2, 3, 7u8, 4, 5, 6];
        let codes = read_predictor_codes(&payload, 2, 3);
        assert_eq!(codes, vec![5, 7]);
    }

    #[test]
    fn test_read_predictor_codes_stops_on_truncated_payload() {
        let payload = vec![5u8, 1, 2, 3]; // only one full row present
        let codes = read_predictor_codes(&payload, 2, 3);
        assert_eq!(codes, vec![5]);
    }
}
