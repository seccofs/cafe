//! CAFE chunk structure (section 3 of the spec)
//!
//! Reading and writing chunks: Length + Type + Flag + Data + CRC32

use crate::constants::MAX_DECOMPRESSED_CHUNK_SIZE;
use crate::error::{CafeError, Result};
use crc32fast::Hasher;
use std::io::Read;

/// Computes the CRC32 of a chunk body (Type + Flag + Data), per section 3.1
/// of the spec. Shared by `write_chunk`, `read_chunk`, and `read_chunk_from`
/// so the three can never drift out of sync on what bytes are hashed.
fn compute_chunk_crc(chunk_type: &[u8; 4], flag: u8, data: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(chunk_type);
    hasher.update(&[flag]);
    hasher.update(data);
    hasher.finalize()
}

/// Validates that `chunk_type` contains only alphabetic ASCII (A-Z, a-z),
/// per spec section 3.1: "The Type field must contain exactly 4 alphabetic
/// ASCII characters (A-Z, a-z). No other byte is allowed." Shared by
/// `read_chunk` and `read_chunk_from`.
fn validate_chunk_type(chunk_type: &[u8; 4]) -> Result<()> {
    for &b in chunk_type {
        if !(b.is_ascii_uppercase() || b.is_ascii_lowercase()) {
            return Err(CafeError::TruncatedFile(format!(
                "Chunk type contains non-alphabetic bytes: {chunk_type:?}. A spec requires A-Z, a-z."
            )));
        }
    }
    Ok(())
}

/// Assembles a complete chunk: Length + Type + Flag + Data + CRC32.
pub(crate) fn write_chunk(chunk_type: &[u8; 4], flag: u8, data: &[u8]) -> Vec<u8> {
    let crc = compute_chunk_crc(chunk_type, flag, data);

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
    /// Only meaningful when produced by [`read_chunk`] (slice-based path).
    /// [`read_chunk_from`] (the `Read`-based path) always sets this to `0`,
    /// since a `Read` source has no addressable "offset" for the caller to
    /// resume from — the stream itself already advanced past the chunk.
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

    // SECURITY: see validate_chunk_type doc comment (spec section 3.1).
    validate_chunk_type(&chunk_type)?;

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

/// Reads a single chunk from any `Read` source: Length(4) + Type(4) +
/// Flag(1) + Data(Length) + CRC32(4). Streaming counterpart to
/// [`read_chunk`] — this is the core new primitive needed by a future
/// `Decoder<R: Read>` (see AGENTS.md "Streaming" discussion): it lets a
/// caller read one chunk at a time directly off a `Read`/`BufRead` source
/// (e.g. a `File` or a network socket) without first buffering the *entire*
/// file into memory, the way `decode_bytes_internal` does today via
/// `std::fs::read`.
///
/// Returns `Ok(None)` on a clean EOF *before* any byte of a new chunk header
/// has been read (i.e. the stream ended exactly at a chunk boundary — the
/// only valid place for a well-formed file to end, though a well-formed CAFE
/// file should always end with an `IEND` chunk before EOF, not rely on this).
/// Any other truncation (partial header, partial data, partial CRC) returns
/// `Err(CafeError::TruncatedFile(..))`, matching `read_chunk`'s behavior for
/// the equivalent slice-truncation cases.
///
/// SECURITY (CWE-409): unlike `read_chunk`, which operates on a `&[u8]` that
/// the caller already had to fully materialize in memory (so a forged
/// `Length` can never exceed the bytes already paid for), a `Read` source
/// has no such natural ceiling — a hostile stream could declare an
/// arbitrarily large `Length` and then actually provide that many bytes.
/// This function bounds the `Data` read to `MAX_DECOMPRESSED_CHUNK_SIZE` (1
/// GiB) regardless of the declared `Length`, returning
/// `DecompressionLimitExceeded` if the declared length exceeds it — the same
/// ceiling already applied to decompression output elsewhere in the crate.
///
/// Used by `Decoder<R: Read>` (`src/cafe.rs`) — the streaming decode API —
/// for both `read_info()` (pre-`IDAT` chunks) and `next_tile()` (`IDAT`
/// chunks one at a time). `decode_bytes_internal` still uses the
/// slice-based `read_chunk` exclusively, since it already has the whole
/// buffer in memory.
pub(crate) fn read_chunk_from<R: Read>(reader: &mut R) -> Result<Option<ReadChunk>> {
    const HEADER_LEN: usize = 4 + 4 + 1; // Length + Type + Flag
    const FOOTER_LEN: usize = 4; // CRC32

    let mut header = [0u8; HEADER_LEN];
    // Distinguish "clean EOF at a chunk boundary" (Ok(None)) from "EOF in
    // the middle of a header" (Err) by reading byte-by-byte until either
    // the first byte fails with EOF (clean boundary) or a later byte fails
    // (truncated). `read_exact` alone can't make this distinction, since it
    // reports UnexpectedEof for both cases identically.
    let mut filled = 0usize;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None); // clean EOF, no chunk started
                }
                return Err(CafeError::TruncatedFile(format!(
                    "incomplete chunk header ({filled} of {HEADER_LEN} bytes read before EOF)"
                )));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(CafeError::Io(e)),
        }
    }

    let length = u32::from_be_bytes(header[0..4].try_into().unwrap()) as u64;

    let mut chunk_type = [0u8; 4];
    chunk_type.copy_from_slice(&header[4..8]);
    validate_chunk_type(&chunk_type)?;

    let flag = header[8];

    // SECURITY: see this function's doc comment. Bound Data to
    // MAX_DECOMPRESSED_CHUNK_SIZE regardless of the declared Length, before
    // attempting to allocate/read that many bytes from the stream.
    if length > MAX_DECOMPRESSED_CHUNK_SIZE {
        return Err(CafeError::DecompressionLimitExceeded {
            limit: MAX_DECOMPRESSED_CHUNK_SIZE,
        });
    }
    let length = length as usize;

    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            CafeError::TruncatedFile(format!(
                "chunk {:?} declares Length={length}, but stream ended before all data was read",
                String::from_utf8_lossy(&chunk_type)
            ))
        } else {
            CafeError::Io(e)
        }
    })?;

    let mut crc_buf = [0u8; FOOTER_LEN];
    reader.read_exact(&mut crc_buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            CafeError::TruncatedFile(format!(
                "chunk {:?}: stream ended before CRC32 footer was read",
                String::from_utf8_lossy(&chunk_type)
            ))
        } else {
            CafeError::Io(e)
        }
    })?;
    let crc_expected = u32::from_be_bytes(crc_buf);

    let crc_actual = compute_chunk_crc(&chunk_type, flag, &data);
    if crc_actual != crc_expected {
        return Err(CafeError::CrcMismatch {
            chunk_type: String::from_utf8_lossy(&chunk_type).to_string(),
            expected: crc_expected,
            actual: crc_actual,
        });
    }

    Ok(Some(ReadChunk {
        chunk_type,
        flag,
        data,
        next_offset: 0, // meaningless for the Read-based path; caller tracks its own stream position
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A `Read` impl that only ever yields at most `chunk_size` bytes per
    /// `read()` call, regardless of the caller's buffer size — exercises
    /// `read_chunk_from`'s header-reading loop (which must not assume a
    /// single `read()` fills the whole 9-byte header) and its use of
    /// `read_exact` for Data/CRC (which already handles short reads
    /// internally, but worth covering explicitly since real sockets/pipes
    /// behave exactly like this).
    struct StutteringReader<'a> {
        data: &'a [u8],
        pos: usize,
        chunk_size: usize,
    }

    impl<'a> StutteringReader<'a> {
        fn new(data: &'a [u8], chunk_size: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk_size,
            }
        }
    }

    impl Read for StutteringReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(buf.len()).min(self.chunk_size.max(1));
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            Ok(n)
        }
    }

    /// Round-trip a chunk through `write_chunk`, then confirm `read_chunk`
    /// (slice-based) and `read_chunk_from` (Read-based) agree on every
    /// field — the core parity requirement for the new primitive.
    #[test]
    fn test_read_chunk_from_matches_read_chunk() {
        let bytes = write_chunk(b"IDAT", 0x01, b"some pixel payload bytes");

        let slice_result = read_chunk(&bytes, 0).expect("read_chunk should succeed");

        let mut cursor = Cursor::new(bytes.as_slice());
        let stream_result = read_chunk_from(&mut cursor)
            .expect("read_chunk_from should succeed")
            .expect("should yield Some, not EOF");

        assert_eq!(slice_result.chunk_type, stream_result.chunk_type);
        assert_eq!(slice_result.flag, stream_result.flag);
        assert_eq!(slice_result.data, stream_result.data);
        // next_offset is intentionally not compared: meaningless for the
        // Read-based path (see ReadChunk's doc comment).
    }

    /// Same parity check, but with a reader that only yields a few bytes at
    /// a time — ensures the header-read loop correctly accumulates partial
    /// reads instead of assuming one `read()` call is enough.
    #[test]
    fn test_read_chunk_from_matches_read_chunk_with_stuttering_reader() {
        let bytes = write_chunk(b"jSON", 0x00, b"{\"key\":\"value\"}");

        let slice_result = read_chunk(&bytes, 0).expect("read_chunk should succeed");

        let mut reader = StutteringReader::new(&bytes, 3);
        let stream_result = read_chunk_from(&mut reader)
            .expect("read_chunk_from should succeed")
            .expect("should yield Some, not EOF");

        assert_eq!(slice_result.chunk_type, stream_result.chunk_type);
        assert_eq!(slice_result.flag, stream_result.flag);
        assert_eq!(slice_result.data, stream_result.data);
    }

    /// Reading multiple chunks sequentially off the same stream (as a
    /// future `Decoder<R>` loop would) must advance correctly chunk by
    /// chunk and report clean EOF (`Ok(None)`) only once every chunk has
    /// been consumed.
    #[test]
    fn test_read_chunk_from_sequential_chunks_then_clean_eof() {
        let mut bytes = write_chunk(b"IHDR", 0x00, &[0u8; 14]);
        bytes.extend(write_chunk(b"IDAT", 0x01, b"tile-one-bytes"));
        bytes.extend(write_chunk(b"IEND", 0x00, b""));

        let mut cursor = Cursor::new(bytes.as_slice());

        let c1 = read_chunk_from(&mut cursor).unwrap().unwrap();
        assert_eq!(&c1.chunk_type, b"IHDR");

        let c2 = read_chunk_from(&mut cursor).unwrap().unwrap();
        assert_eq!(&c2.chunk_type, b"IDAT");
        assert_eq!(c2.data, b"tile-one-bytes");

        let c3 = read_chunk_from(&mut cursor).unwrap().unwrap();
        assert_eq!(&c3.chunk_type, b"IEND");
        assert!(c3.data.is_empty());

        // Clean EOF exactly at a chunk boundary must yield Ok(None), not Err.
        let eof = read_chunk_from(&mut cursor).unwrap();
        assert!(eof.is_none());
    }

    /// Empty stream (0 bytes available) must be treated as a clean EOF, not
    /// a truncation error — this is what lets a Decoder loop use
    /// `while let Some(chunk) = read_chunk_from(&mut reader)?` naturally.
    #[test]
    fn test_read_chunk_from_empty_stream_is_clean_eof() {
        let mut cursor = Cursor::new(&[][..]);
        let result = read_chunk_from(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    /// EOF partway through the 9-byte header must be a hard error, not
    /// `Ok(None)` — mirrors `read_chunk`'s `TruncatedFile` behavior for a
    /// header that doesn't fully fit in the remaining slice.
    #[test]
    fn test_read_chunk_from_truncated_header_is_error() {
        let bytes = write_chunk(b"IDAT", 0x00, b"data");
        // Keep only 5 of the 9 header bytes.
        let mut cursor = Cursor::new(&bytes[..5]);
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    /// EOF partway through Data must be a hard error (parity with
    /// `read_chunk`'s bounds check against Length + FOOTER_LEN).
    #[test]
    fn test_read_chunk_from_truncated_data_is_error() {
        let bytes = write_chunk(b"IDAT", 0x00, b"0123456789");
        // Header (9 bytes) + only part of the 10-byte Data.
        let mut cursor = Cursor::new(&bytes[..9 + 4]);
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    /// EOF partway through the CRC32 footer must be a hard error.
    #[test]
    fn test_read_chunk_from_truncated_crc_is_error() {
        let bytes = write_chunk(b"IDAT", 0x00, b"0123456789");
        // Everything except the last 2 of 4 CRC bytes.
        let cut = bytes.len() - 2;
        let mut cursor = Cursor::new(&bytes[..cut]);
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    /// A forged/corrupted CRC32 must be rejected identically to the
    /// slice-based path.
    #[test]
    fn test_read_chunk_from_crc_mismatch_is_error() {
        let mut bytes = write_chunk(b"IDAT", 0x00, b"payload");
        // Flip a bit in the CRC32 footer (last 4 bytes).
        let len = bytes.len();
        bytes[len - 1] ^= 0xFF;
        let mut cursor = Cursor::new(bytes.as_slice());
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(result, Err(CafeError::CrcMismatch { .. })));
    }

    /// A chunk type containing non-alphabetic bytes must be rejected,
    /// matching `read_chunk`'s validation (spec section 3.1).
    #[test]
    fn test_read_chunk_from_invalid_chunk_type_is_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(0u32).to_be_bytes()); // Length = 0
        buf.extend_from_slice(&[0x31, 0x32, 0x33, 0x34]); // "1234" - not alphabetic
        buf.push(0x00); // Flag
                        // CRC over ("1234" + flag + empty data)
        let crc = compute_chunk_crc(&[0x31, 0x32, 0x33, 0x34], 0x00, b"");
        buf.extend_from_slice(&crc.to_be_bytes());

        let mut cursor = Cursor::new(buf.as_slice());
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    /// A forged `Length` field declaring more than `MAX_DECOMPRESSED_CHUNK_SIZE`
    /// must be rejected immediately, *before* attempting to allocate or read
    /// that many bytes from the stream (CWE-409: a `Read` source, unlike a
    /// `&[u8]` slice, has no natural ceiling on how much data it can
    /// eventually provide behind a forged Length).
    #[test]
    fn test_read_chunk_from_forged_huge_length_is_rejected_before_reading() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(u32::MAX).to_be_bytes()); // Length = ~4 GiB
        buf.extend_from_slice(b"IDAT");
        buf.push(0x00);
        // No Data/CRC follows - if the function tried to actually allocate
        // and read u32::MAX bytes, this test would hang/OOM instead of
        // returning quickly with an error.
        let mut cursor = Cursor::new(buf.as_slice());
        let result = read_chunk_from(&mut cursor);
        assert!(matches!(
            result,
            Err(CafeError::DecompressionLimitExceeded { .. })
        ));
    }

    /// Sanity check that a chunk with an empty Data payload (e.g. `IEND`)
    /// round-trips correctly - Length=0 is a valid, common case.
    #[test]
    fn test_read_chunk_from_empty_data_payload() {
        let bytes = write_chunk(b"IEND", 0x00, b"");
        let mut cursor = Cursor::new(bytes.as_slice());
        let result = read_chunk_from(&mut cursor).unwrap().unwrap();
        assert_eq!(&result.chunk_type, b"IEND");
        assert_eq!(result.flag, 0x00);
        assert!(result.data.is_empty());
    }
}
