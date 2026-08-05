//! CAFE error types

use std::error::Error;
use std::fmt;

/// Generic CAFE error. A dedicated error enum, instead of exceptions —
/// each variant represents a different way
/// the encode/decode can fail.
#[derive(Debug)]
pub enum CafeError {
    InvalidSignature,
    CrcMismatch {
        chunk_type: String,
        expected: u32,
        actual: u32,
    },
    UnsupportedFeature(String),
    MissingIhdr,
    Io(std::io::Error),
    Image(image::ImageError),
    Zstd(std::io::Error),
    Json(serde_json::Error),
    /// Chunk framing inconsistent with the real file size (truncated or
    /// corrupted file, or a forged Length field). See read_chunk().
    TruncatedFile(String),
    /// The decompression of a chunk exceeded the maximum allowed output limit
    /// (protection against "decompression bomb", CWE-409). See MAX_DECOMPRESSED_CHUNK_SIZE.
    DecompressionLimitExceeded {
        limit: u64,
    },
}

impl fmt::Display for CafeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CafeError::InvalidSignature => write!(f, "Invalid CAFE signature - file corrupted or not a .cafe file"),
            CafeError::CrcMismatch { chunk_type, expected, actual } => write!(
                f,
                "Invalid CRC in chunk {chunk_type:?}: expected {expected:#010x}, got {actual:#010x}"
            ),
            CafeError::UnsupportedFeature(msg) => write!(f, "Feature not supported by this v1.0+ decoder: {msg}"),
            CafeError::MissingIhdr => write!(f, "File does not contain IHDR"),
            CafeError::Io(e) => write!(f, "I/O error: {e}"),
            CafeError::Image(e) => write!(f, "Image error: {e}"),
            CafeError::Zstd(e) => write!(f, "ZSTD error: {e}"),
            CafeError::Json(e) => write!(f, "JSON error: {e}"),
            CafeError::TruncatedFile(msg) => write!(f, "File truncated or corrupted: {msg}"),
            CafeError::DecompressionLimitExceeded { limit } => write!(
                f,
                "Decompression exceeded maximum limit of {limit} bytes (possible decompression bomb)"
            ),
        }
    }
}

impl Error for CafeError {}

impl From<std::io::Error> for CafeError {
    fn from(e: std::io::Error) -> Self {
        CafeError::Io(e)
    }
}

impl From<image::ImageError> for CafeError {
    fn from(e: image::ImageError) -> Self {
        CafeError::Image(e)
    }
}

impl From<serde_json::Error> for CafeError {
    fn from(e: serde_json::Error) -> Self {
        CafeError::Json(e)
    }
}

/// CAFE result type
pub type Result<T> = std::result::Result<T, CafeError>;
