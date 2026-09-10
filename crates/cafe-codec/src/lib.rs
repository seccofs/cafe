//! CAFE codec: predictors, ZSTD compression, tiling, and the
//! streaming-first `Encoder<W>`/`Decoder<R>` API.
//!
//! This crate builds on top of `cafe-format` (chunk container, IHDR,
//! validation) and adds everything needed to turn pixels into CAFE bytes
//! and back: predictors (behind a `Predictor` trait), the ZSTD
//! compress-with-fallback rule, tiling (`iDIM`), and scan order
//! (row-major / Morton).
//!
//! Status: Phase 7 (tiling + Morton) — see `AGENTS.md` at the workspace
//! root for the implementation roadmap. Images encode/decode as a single
//! implicit whole-image tile (no `iDIM`) or as an explicit `iDIM`-declared
//! grid of any size (row-major or Z-order scan, edge-truncated tiles).

pub mod decoder;
pub mod encoder;
pub mod error;
pub mod morton;
pub mod predictor;
pub mod tile;
pub mod tiling;
pub mod zstd_codec;

pub use decoder::{decode_bytes, DecodedImage};
pub use encoder::{encode_bytes, Encoder, EncoderOptions};
pub use error::{CodecError, Result};
