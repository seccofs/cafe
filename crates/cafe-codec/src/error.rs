//! Codec-level errors (predictors, ZSTD, tiling).
//!
//! `cafe-format`'s own `error` module doc comment explains the split: that
//! crate owns format-framing/validation errors (bad signature, CRC
//! mismatch, invalid `IHDR`, ...); this crate adds the concerns it doesn't
//! know about — predictor codes, ZSTD decompression, tiling support.
//! [`CodecError::Format`] wraps the former so callers of this crate only
//! ever handle one error type.

use std::fmt;

/// Errors that can occur while decoding (and, from Phase 6 on, encoding)
/// CAFE pixel data.
#[derive(Debug)]
pub enum CodecError {
    /// A chunk-framing/validation error surfaced by `cafe-format` (bad
    /// signature, CRC mismatch, invalid `IHDR`, truncated file, ...).
    Format(cafe_format::CafeError),
    /// A row's predictor code byte is outside `0..NUM_PREDICTORS` (spec
    /// section 4.4.1 defines exactly 6 predictor codes, `0`-`5`). Since
    /// this byte comes straight from an untrusted file, it must be
    /// validated before ever being used to select a predictor.
    InvalidPredictorCode(u8),
    /// A decoded `IDAT` (or reconstructed tile) contains a palette index
    /// byte with no corresponding `PLTE` entry (spec section 4.3's
    /// validation rule: "decoders must reject any index >= entry_count").
    /// Distinct from [`CodecError::Format`]'s `InvalidPlte` (which covers
    /// `PLTE`'s own fields being self-inconsistent, checked independently
    /// of what indices any `IDAT` actually contains).
    InvalidPaletteIndex(u8),
    /// The number (or order) of `IDAT` chunks in the file doesn't match
    /// what `iDIM` (or its absence) requires — e.g. fewer/more `IDAT`s
    /// than `iDIM`'s tile count, more than one `IDAT` when no `iDIM` is
    /// present (spec section 4.2: "the decoder assumes a single `IDAT`
    /// covering the entire image"), or an `iDIM` chunk appearing after the
    /// first `IDAT` (violates spec section 5's mandatory chunk order).
    /// This is always a malformed-file condition, distinct from
    /// [`CodecError::Format`]'s `InvalidIdim` (which covers `iDIM`'s own
    /// fields being self-inconsistent or exceeding `MAX_TILE_COUNT`,
    /// checked independently of how many `IDAT`s actually follow).
    TilingMismatch(String),
    /// The caller misused [`crate::encoder::Encoder`]'s API contract —
    /// e.g. `add_tile`'s buffer length didn't match `height * bytes_per_row`,
    /// `add_tile` was called more than once, `finish` was called before any
    /// tile was added, or `EncoderOptions::tile_size` contains a zero
    /// component. Unlike every other `CodecError` variant, this always
    /// indicates a programming error in the caller, never untrusted input
    /// — there is no file to be malformed yet.
    EncoderMisuse(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(e) => write!(f, "{e}"),
            Self::InvalidPredictorCode(code) => write!(
                f,
                "invalid predictor code: {code} (spec section 4.4.1 defines codes 0-5)"
            ),
            Self::TilingMismatch(msg) => write!(f, "tiling mismatch: {msg}"),
            Self::InvalidPaletteIndex(idx) => write!(
                f,
                "palette index {idx} has no corresponding PLTE entry (spec section 4.3)"
            ),
            Self::EncoderMisuse(msg) => write!(f, "encoder misuse: {msg}"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<cafe_format::CafeError> for CodecError {
    fn from(e: cafe_format::CafeError) -> Self {
        Self::Format(e)
    }
}

impl From<std::io::Error> for CodecError {
    fn from(e: std::io::Error) -> Self {
        Self::Format(cafe_format::CafeError::Io(e))
    }
}

/// CAFE codec result type.
pub type Result<T> = std::result::Result<T, CodecError>;
