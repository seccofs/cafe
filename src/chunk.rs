//! CAFE chunk structure (section 3 of the spec)
//!
//! Reading and writing chunks: Length + Type + Flag + Data + CRC32

use crate::error::{CafeError, Result};
use crc32fast::Hasher;

/// Assembles a complete chunk: Length + Type + Flag + Data + CRC32.
pub(crate) fn write_chunk(chunk_type: &[u8; 4], flag: u8, data: &[u8]) -> Vec<u8> {
    // Compute CRC32 incrementally without allocating intermediate body vector
    let mut hasher = Hasher::new();
    hasher.update(chunk_type);
    hasher.update(&[flag]);
    hasher.update(data);
    let crc = hasher.finalize();

    // Assemble output: Length (4) + Type (4) + Flag (1) + Data (N) + CRC32 (4)
    let mut out = Vec::with_capacity(4 + 4 + 1 + data.len() + 4);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.push(flag);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// A chunk already decoded, along with the offset where reading stopped.
pub(crate) struct ReadChunk {
    pub(crate) chunk_type: [u8; 4],
    pub(crate) flag: u8,
    pub(crate) data: Vec<u8>,
    pub(crate) next_offset: usize,
}

/// Reads a chunk starting at `offset`. Verifies the CRC32 before returning.
///
/// All reads validate bounds against `buf.len()` before indexing —
/// a truncated file or a forged `Length` field returns `Err`
/// (`TruncatedFile`), never panics.
pub(crate) fn read_chunk(buf: &[u8], offset: usize) -> Result<ReadChunk> {
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

    // SECURITY: Validates that chunk_type contains only alphabetic ASCII (A-Z, a-z)
    // per spec section 3.1: "The Type field must contain exactly 4 alphabetic
    // ASCII characters (A–Z, a–z). No other byte is allowed."
    for &b in &chunk_type {
        if !(b.is_ascii_uppercase() || b.is_ascii_lowercase()) {
            return Err(CafeError::TruncatedFile(format!(
                "Chunk type contains non-alphabetic bytes: {:?}. A spec requires A-Z, a-z.",
                chunk_type
            )));
        }
    }

    let flag = buf[o];
    o += 1;

    // Validates that `length` (file-controlled) does not overrun the buffer,
    // and that the sum does not overflow usize before comparing.
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

    // Compute CRC32 incrementally without allocating intermediate body vector
    let mut hasher = Hasher::new();
    hasher.update(&chunk_type);
    hasher.update(&[flag]);
    hasher.update(&data);
    let crc_actual = hasher.finalize();

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
