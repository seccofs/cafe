//! Shared helpers for the `cafe-cli` binaries (`cafe`, `cafe-encode`,
//! `cafe-decode`): PNG/HDR <-> CAFE pixel-buffer conversion, plus small
//! chunk-walking utilities for `cafe inspect`/`verify`/`explain` that don't
//! belong in `cafe-format`/`cafe-codec`'s own public APIs (those crates
//! stay decoder/encoder-focused; presentation is this crate's job).
//!
//! v0.1 CLI scope (see `AGENTS.md`'s Phase 9 entry): PNG (8-bit uint,
//! gray/gray+alpha/RGB/RGBA, via `png_io`) plus HDR (32-bit float,
//! RGB/RGBA, via `hdr_io`, added in the HDR-CLI-support follow-up) —
//! matches the current `corpus/`'s content and `cafe-codec`'s most-tested
//! paths. 16-bit uint image I/O is still deferred to a later CLI pass;
//! `cafe-codec` itself already supports it (see its `encoder`/`decoder`
//! tests), only this crate's bridges are narrower for now.

pub mod chunks;
pub mod hdr_io;
pub mod png_io;

pub use chunks::{read_predictor_codes, walk_chunks, ChunkInfo};
pub use hdr_io::{cafe_pixels_to_dynamic_image_hdr, dynamic_image_to_cafe_pixels_hdr, HdrIoError};
pub use png_io::{
    cafe_pixels_to_dynamic_image, dynamic_image_to_cafe_pixels, CafePixels, PngIoError,
};
