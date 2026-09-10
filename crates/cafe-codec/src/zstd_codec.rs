//! ZSTD compression/decompression with raw fallback (spec section 3.2).
//!
//! A single `compress_with_fallback`/`decompress_chunk` pair — no
//! dictionary variant (`zDIC` is deferred to 0.4, per `AGENTS.md`).

use crate::error::{CodecError, Result};
use cafe_format::constants::MAX_DECOMPRESSED_CHUNK_SIZE;
use std::io::Read;

/// `Flag` value (spec section 3.2): raw, uncompressed data.
pub const FLAG_RAW: u8 = 0x00;
/// `Flag` value (spec section 3.2): ZSTD-compressed data.
pub const FLAG_ZSTD: u8 = 0x01;

/// Reads from `reader` to the end, but never more than `limit` bytes. If
/// the stream has more data beyond the limit, returns an error instead of
/// continuing to allocate memory indefinitely (CWE-409, spec section 8.2).
fn read_to_end_limited<R: Read>(reader: R, limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // Asks for 1 byte more than the limit: if we receive exactly limit+1,
    // we know there was more data than allowed.
    reader
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(CodecError::from)?;
    if out.len() as u64 > limit {
        return Err(CodecError::Format(
            cafe_format::CafeError::DecompressionLimitExceeded { limit },
        ));
    }
    Ok(out)
}

/// Fallback rule from spec section 3.2: compresses `raw` at `level`, and
/// only keeps the compressed result if it's smaller than the original;
/// otherwise returns the raw bytes with `Flag = FLAG_RAW`.
pub fn compress_with_fallback(raw: &[u8], level: i32) -> Result<(u8, Vec<u8>)> {
    let compressed = zstd::encode_all(raw, level)
        .map_err(|e| CodecError::from(cafe_format::CafeError::Io(e)))?;
    if compressed.len() < raw.len() {
        Ok((FLAG_ZSTD, compressed))
    } else {
        Ok((FLAG_RAW, raw.to_vec()))
    }
}

/// Decompresses a chunk's `data` according to `flag`, bounded by `limit`
/// bytes of output (itself capped at `MAX_DECOMPRESSED_CHUNK_SIZE`, spec
/// section 8.2). Does not rely on ZSTD frame headers declaring their own
/// decompressed size (spec section 3.2's interoperability note) — uses the
/// streaming decoder API instead.
pub fn decompress_with_limit(flag: u8, data: &[u8], limit: u64) -> Result<Vec<u8>> {
    let limit = limit.min(MAX_DECOMPRESSED_CHUNK_SIZE);
    match flag {
        FLAG_RAW => {
            if data.len() as u64 > limit {
                return Err(CodecError::Format(
                    cafe_format::CafeError::DecompressionLimitExceeded { limit },
                ));
            }
            Ok(data.to_vec())
        }
        FLAG_ZSTD => {
            let decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(data))
                .map_err(|e| CodecError::from(cafe_format::CafeError::Io(e)))?;
            read_to_end_limited(decoder, limit)
        }
        other => Err(CodecError::Format(
            cafe_format::CafeError::UnsupportedFeature(format!("Codec flag {other:#04x}")),
        )),
    }
}

/// Decompresses a chunk using the default per-chunk limit
/// (`MAX_DECOMPRESSED_CHUNK_SIZE`, spec section 8.2). Prefer
/// [`decompress_with_limit`] with a tighter, IHDR-derived budget when the
/// expected output size is known (e.g. one `IDAT`'s worth of pixels) —
/// this is the coarse per-chunk ceiling alone, without accumulated
/// per-image budget tracking across multiple `IDAT`s (deferred until this
/// crate actually needs it).
pub fn decompress_chunk(flag: u8, data: &[u8]) -> Result<Vec<u8>> {
    decompress_with_limit(flag, data, MAX_DECOMPRESSED_CHUNK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_with_fallback_uses_raw_for_incompressible_tiny_data() {
        let raw = b"ab";
        let (flag, data) = compress_with_fallback(raw, 19).unwrap();
        assert_eq!(flag, FLAG_RAW);
        assert_eq!(data, raw);
    }

    #[test]
    fn test_compress_with_fallback_uses_zstd_for_compressible_data() {
        let raw = vec![0x42u8; 4096];
        let (flag, data) = compress_with_fallback(&raw, 19).unwrap();
        assert_eq!(flag, FLAG_ZSTD);
        assert!(data.len() < raw.len());
    }

    #[test]
    fn test_decompress_raw_roundtrip() {
        let raw = b"raw payload bytes";
        let result = decompress_with_limit(FLAG_RAW, raw, 1024).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn test_decompress_zstd_roundtrip() {
        let raw = vec![0x99u8; 2048];
        let (flag, compressed) = compress_with_fallback(&raw, 19).unwrap();
        assert_eq!(flag, FLAG_ZSTD);
        let result = decompress_with_limit(flag, &compressed, 4096).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn test_compress_then_decompress_roundtrip_incompressible() {
        let raw = b"xy";
        let (flag, compressed) = compress_with_fallback(raw, 19).unwrap();
        let result = decompress_with_limit(flag, &compressed, 1024).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn test_decompress_raw_over_limit_is_rejected() {
        let raw = vec![0u8; 100];
        let result = decompress_with_limit(FLAG_RAW, &raw, 10);
        assert!(matches!(
            result,
            Err(CodecError::Format(
                cafe_format::CafeError::DecompressionLimitExceeded { limit: 10 }
            ))
        ));
    }

    #[test]
    fn test_decompress_zstd_over_limit_is_rejected() {
        let raw = vec![0x77u8; 8192];
        let (flag, compressed) = compress_with_fallback(&raw, 19).unwrap();
        assert_eq!(flag, FLAG_ZSTD);
        // Decompressed size (8192) exceeds this artificially tiny limit.
        let result = decompress_with_limit(flag, &compressed, 100);
        assert!(matches!(
            result,
            Err(CodecError::Format(
                cafe_format::CafeError::DecompressionLimitExceeded { limit: 100 }
            ))
        ));
    }

    #[test]
    fn test_decompress_unknown_flag_is_rejected() {
        let result = decompress_with_limit(0x02, b"whatever", 1024);
        assert!(matches!(
            result,
            Err(CodecError::Format(
                cafe_format::CafeError::UnsupportedFeature(_)
            ))
        ));
    }

    #[test]
    fn test_decompress_chunk_uses_default_limit() {
        let raw = b"small";
        let result = decompress_chunk(FLAG_RAW, raw).unwrap();
        assert_eq!(result, raw);
    }
}
