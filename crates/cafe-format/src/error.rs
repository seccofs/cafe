//! CAFE format error types.
//!
//! Scoped to the format-level concerns owned by this crate (chunk framing,
//! signature, IHDR). Codec-level error variants (unsupported predictor,
//! palette limits, etc.) belong in `cafe-codec::error` instead.

use std::error::Error;
use std::fmt;

/// Errors that can occur while reading or writing the CAFE chunk container.
#[derive(Debug)]
pub enum CafeError {
    /// The file signature does not match the expected CAFE magic bytes.
    InvalidSignature,
    /// A chunk's CRC32 footer did not match the computed CRC32 of its
    /// Type + Flag + Data bytes.
    CrcMismatch {
        chunk_type: String,
        expected: u32,
        actual: u32,
    },
    /// A critical chunk type this decoder does not recognize.
    UnsupportedFeature(String),
    /// The file is missing a mandatory `IHDR` chunk.
    MissingIhdr,
    /// Underlying I/O error while reading/writing a chunk stream.
    Io(std::io::Error),
    /// Chunk framing inconsistent with the real file size (truncated or
    /// corrupted file, or a forged Length field).
    TruncatedFile(String),
    /// The decompression of a chunk exceeded the maximum allowed output
    /// limit (protection against "decompression bomb", CWE-409).
    DecompressionLimitExceeded { limit: u64 },
    /// An `IHDR` field (or the payload length itself) violates spec
    /// section 4.1 — e.g. `Width`/`Height = 0`, an invalid
    /// `sample_format`/`bit_depth` combination, an unsupported
    /// `color_type`, or reserved `compression_method` bits set.
    InvalidIhdr(String),
    /// An `iDIM` field (or the payload length itself) violates spec
    /// section 4.2 — e.g. zero `tile_width`/`tile_height`, `tiles_x`/
    /// `tiles_y` inconsistent with `IHDR`'s dimensions, an unknown
    /// `scan_order`, or `tiles_x * tiles_y` exceeding `MAX_TILE_COUNT`
    /// (spec section 8.2).
    InvalidIdim(String),
    /// A `PLTE` field (or the payload length itself) violates spec
    /// section 4.3 — e.g. `entry_count = 0`, `entry_count >
    /// MAX_PALETTE_ENTRIES`, `IHDR.color_type` not RGB/RGBA, or
    /// `IHDR.bit_depth != 8`. Out-of-range palette *indices* inside an
    /// `IDAT` are a separate, `cafe-codec`-level concern (this crate
    /// never inspects `IDAT` payloads).
    InvalidPlte(String),
    /// A chunk was expected to be a specific type (e.g. `IHDR` must be
    /// first) but a different type was found.
    UnexpectedChunkType { expected: String, found: String },
    /// A `jSON` chunk's content violates spec section 4.6 — e.g. a
    /// declared namespace length exceeding the remaining payload, a
    /// non-ASCII namespace, a payload that isn't valid UTF-8, or a
    /// payload that isn't syntactically valid JSON. Per spec section 8.4,
    /// a decoder must discard only the offending `jSON` chunk, not the
    /// whole file — callers that want that behavior should catch this
    /// variant specifically rather than propagating it.
    InvalidJsonChunk(String),
    /// An `xMPd` chunk's payload is not valid UTF-8 (spec section 4.8
    /// requires "Valid UTF-8 XML"). Same discard-only-this-chunk handling
    /// as [`CafeError::InvalidJsonChunk`] applies (spec section 8.4).
    InvalidXmpd(String),
}

impl fmt::Display for CafeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSignature => {
                write!(f, "Invalid CAFE signature - file corrupted or not a .cafe file")
            }
            Self::CrcMismatch {
                chunk_type,
                expected,
                actual,
            } => write!(
                f,
                "Invalid CRC in chunk {chunk_type:?}: expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::UnsupportedFeature(msg) => write!(f, "Unsupported CAFE feature: {msg}"),
            Self::MissingIhdr => write!(f, "File does not contain IHDR"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::TruncatedFile(msg) => write!(f, "File truncated or corrupted: {msg}"),
            Self::DecompressionLimitExceeded { limit } => write!(
                f,
                "Decompression exceeded maximum limit of {limit} bytes (possible decompression bomb)"
            ),
            Self::InvalidIhdr(msg) => write!(f, "Invalid IHDR: {msg}"),
            Self::InvalidIdim(msg) => write!(f, "Invalid iDIM: {msg}"),
            Self::InvalidPlte(msg) => write!(f, "Invalid PLTE: {msg}"),
            Self::UnexpectedChunkType { expected, found } => write!(
                f,
                "Expected chunk type {expected:?}, found {found:?}"
            ),
            Self::InvalidJsonChunk(msg) => write!(f, "Invalid jSON chunk: {msg}"),
            Self::InvalidXmpd(msg) => write!(f, "Invalid xMPd chunk: {msg}"),
        }
    }
}

impl Error for CafeError {}

impl From<std::io::Error> for CafeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// CAFE format result type.
pub type Result<T> = std::result::Result<T, CafeError>;
