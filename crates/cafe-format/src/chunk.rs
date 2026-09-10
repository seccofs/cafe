//! CAFE chunk structure (spec section 3): Length + Type + Flag + Data +
//! CRC32.
//!
//! Adapted from the frozen reference lineage's `old/src/chunk.rs`. The
//! streaming (`Read`-based) primitive is deferred until `cafe-codec`'s
//! `Decoder<R>` needs it (Phase 5+) — this module only provides the
//! slice-based path needed to parse a whole in-memory `.cafe` file, which
//! is enough for Phase 4's golden-file tests.

use crate::constants::MAX_DECOMPRESSED_CHUNK_SIZE;
use crate::error::{CafeError, Result};
use crc32fast::Hasher;

/// Computes the CRC32 of a chunk body (Type + Flag + Data), per spec
/// section 3: "CRC32 over Type + Flag + Data". Shared by `write_chunk` and
/// `read_chunk` so the two can never drift out of sync on what bytes are
/// hashed.
fn compute_chunk_crc(chunk_type: &[u8; 4], flag: u8, data: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(chunk_type);
    hasher.update(&[flag]);
    hasher.update(data);
    hasher.finalize()
}

/// Validates that `chunk_type` contains only alphabetic ASCII (A-Z, a-z),
/// per spec section 3.1: "The Type field must contain exactly 4 alphabetic
/// ASCII characters (A-Z, a-z)."
fn validate_chunk_type(chunk_type: &[u8; 4]) -> Result<()> {
    for &b in chunk_type {
        if !(b.is_ascii_uppercase() || b.is_ascii_lowercase()) {
            return Err(CafeError::TruncatedFile(format!(
                "Chunk type contains non-alphabetic bytes: {chunk_type:?}. Spec requires A-Z, a-z."
            )));
        }
    }
    Ok(())
}

/// Whether a chunk type's first letter marks it critical (uppercase, spec
/// section 3.1) — a decoder must understand or reject it — or ancillary
/// (lowercase) — a decoder may safely ignore an unrecognized one.
pub fn is_critical(chunk_type: &[u8; 4]) -> bool {
    chunk_type[0].is_ascii_uppercase()
}

/// Assembles a complete chunk: Length + Type + Flag + Data + CRC32.
pub fn write_chunk(chunk_type: &[u8; 4], flag: u8, data: &[u8]) -> Vec<u8> {
    let crc = compute_chunk_crc(chunk_type, flag, data);

    let mut out = Vec::with_capacity(4 + 4 + 1 + data.len() + 4);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.push(flag);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// A chunk already decoded, along with the offset where reading stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadChunk {
    pub chunk_type: [u8; 4],
    pub flag: u8,
    pub data: Vec<u8>,
    pub next_offset: usize,
}

/// Reads a chunk starting at `offset`. Verifies the CRC32 before returning.
///
/// All reads validate bounds against `buf.len()` before indexing — a
/// truncated file or a forged `Length` field returns `Err`
/// (`TruncatedFile`), never panics (spec section 8.1).
pub fn read_chunk(buf: &[u8], offset: usize) -> Result<ReadChunk> {
    const HEADER_LEN: usize = 4 + 4 + 1; // Length + Type + Flag
    const FOOTER_LEN: usize = 4; // CRC32

    if offset
        .checked_add(HEADER_LEN)
        .is_none_or(|end| end > buf.len())
    {
        return Err(CafeError::TruncatedFile(format!(
            "incomplete chunk header at offset {offset} ({} bytes remaining, {HEADER_LEN} needed)",
            buf.len().saturating_sub(offset)
        )));
    }

    let length = u32::from_be_bytes(
        buf[offset..offset + 4]
            .try_into()
            .map_err(|_| CafeError::TruncatedFile("Slice conversion to array failed".into()))?,
    ) as usize;
    let mut o = offset + 4;

    let mut chunk_type = [0u8; 4];
    chunk_type.copy_from_slice(&buf[o..o + 4]);
    o += 4;

    // SECURITY: see validate_chunk_type doc comment (spec section 3.1).
    validate_chunk_type(&chunk_type)?;

    let flag = buf[o];
    o += 1;

    // SECURITY (CWE-409): reject a declared Length exceeding the
    // decompression ceiling before even attempting to slice/allocate that
    // much data — mirrors read_chunk_from's streaming-path check in the
    // frozen reference lineage, applied here to the slice-based path too
    // since a chunk's raw Data length is itself attacker-controlled even
    // before any decompression happens.
    if length as u64 > MAX_DECOMPRESSED_CHUNK_SIZE {
        return Err(CafeError::DecompressionLimitExceeded {
            limit: MAX_DECOMPRESSED_CHUNK_SIZE,
        });
    }

    // Validates that `length` (file-controlled) does not overrun the
    // buffer, and that the sum does not overflow usize before comparing.
    let data_end = o
        .checked_add(length)
        .ok_or_else(|| CafeError::TruncatedFile(format!("Length field overflow: {length}")))?;
    let footer_end = data_end
        .checked_add(FOOTER_LEN)
        .ok_or_else(|| CafeError::TruncatedFile(format!("Length field overflow: {length}")))?;
    if footer_end > buf.len() {
        return Err(CafeError::TruncatedFile(format!(
            "chunk {:?} declares Length={length}, but only {} bytes remaining in file",
            String::from_utf8_lossy(&chunk_type),
            buf.len().saturating_sub(o)
        )));
    }

    let data = buf[o..data_end].to_vec();
    o = data_end;

    let crc_expected = u32::from_be_bytes(
        buf[o..o + 4]
            .try_into()
            .map_err(|_| CafeError::TruncatedFile("Slice conversion to array failed".into()))?,
    );
    o += 4;

    let crc_actual = compute_chunk_crc(&chunk_type, flag, &data);

    if crc_actual != crc_expected {
        return Err(CafeError::CrcMismatch {
            chunk_type: String::from_utf8_lossy(&chunk_type).to_string(),
            expected: crc_expected,
            actual: crc_actual,
        });
    }

    Ok(ReadChunk {
        chunk_type,
        flag,
        data,
        next_offset: o,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_then_read_chunk_roundtrip() {
        let bytes = write_chunk(b"IDAT", 0x01, b"some pixel payload bytes");
        let chunk = read_chunk(&bytes, 0).expect("read_chunk should succeed");
        assert_eq!(&chunk.chunk_type, b"IDAT");
        assert_eq!(chunk.flag, 0x01);
        assert_eq!(chunk.data, b"some pixel payload bytes");
        assert_eq!(chunk.next_offset, bytes.len());
    }

    #[test]
    fn test_read_chunk_empty_data_payload() {
        let bytes = write_chunk(b"IEND", 0x00, b"");
        let chunk = read_chunk(&bytes, 0).expect("read_chunk should succeed");
        assert_eq!(&chunk.chunk_type, b"IEND");
        assert!(chunk.data.is_empty());
    }

    #[test]
    fn test_read_chunk_truncated_header_is_error() {
        let bytes = write_chunk(b"IDAT", 0x00, b"data");
        let result = read_chunk(&bytes[..5], 0);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_read_chunk_truncated_data_is_error() {
        let bytes = write_chunk(b"IDAT", 0x00, b"0123456789");
        let result = read_chunk(&bytes[..9 + 4], 0);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_read_chunk_forged_length_overruns_buffer_is_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(1000u32).to_be_bytes()); // Length = 1000
        buf.extend_from_slice(b"IDAT");
        buf.push(0x00);
        buf.extend_from_slice(b"short"); // far fewer than 1000 bytes
        let result = read_chunk(&buf, 0);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_read_chunk_crc_mismatch_is_error() {
        let mut bytes = write_chunk(b"IDAT", 0x00, b"payload");
        let len = bytes.len();
        bytes[len - 1] ^= 0xFF;
        let result = read_chunk(&bytes, 0);
        assert!(matches!(result, Err(CafeError::CrcMismatch { .. })));
    }

    #[test]
    fn test_read_chunk_invalid_chunk_type_is_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(0u32).to_be_bytes());
        buf.extend_from_slice(&[0x31, 0x32, 0x33, 0x34]); // "1234"
        buf.push(0x00);
        let crc = compute_chunk_crc(&[0x31, 0x32, 0x33, 0x34], 0x00, b"");
        buf.extend_from_slice(&crc.to_be_bytes());
        let result = read_chunk(&buf, 0);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_read_chunk_forged_huge_length_is_rejected_before_allocating() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(u32::MAX).to_be_bytes());
        buf.extend_from_slice(b"IDAT");
        buf.push(0x00);
        let result = read_chunk(&buf, 0);
        assert!(matches!(
            result,
            Err(CafeError::DecompressionLimitExceeded { .. })
        ));
    }

    #[test]
    fn test_read_chunk_sequential_chunks_advance_offset() {
        let mut bytes = write_chunk(b"IHDR", 0x00, &[0u8; 12]);
        let idat = write_chunk(b"IDAT", 0x01, b"tile-one-bytes");
        let iend = write_chunk(b"IEND", 0x00, b"");
        let idat_offset = bytes.len();
        bytes.extend(&idat);
        let iend_offset = bytes.len();
        bytes.extend(&iend);

        let c1 = read_chunk(&bytes, 0).unwrap();
        assert_eq!(&c1.chunk_type, b"IHDR");
        assert_eq!(c1.next_offset, idat_offset);

        let c2 = read_chunk(&bytes, c1.next_offset).unwrap();
        assert_eq!(&c2.chunk_type, b"IDAT");
        assert_eq!(c2.next_offset, iend_offset);

        let c3 = read_chunk(&bytes, c2.next_offset).unwrap();
        assert_eq!(&c3.chunk_type, b"IEND");
        assert_eq!(c3.next_offset, bytes.len());
    }

    #[test]
    fn test_is_critical_matches_naming_convention() {
        assert!(is_critical(b"IHDR"));
        assert!(is_critical(b"IDAT"));
        assert!(is_critical(b"IEND"));
        assert!(!is_critical(b"iDIM"));
        assert!(!is_critical(b"eXIF"));
        assert!(!is_critical(b"jSON"));
        assert!(!is_critical(b"iCCP"));
        assert!(!is_critical(b"xMPd"));
    }
}
