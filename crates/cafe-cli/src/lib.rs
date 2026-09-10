//! Shared helpers for the `cafe-cli` binaries (`cafe`, `cafe-encode`,
//! `cafe-decode`): PNG/HDR <-> CAFE pixel-buffer conversion, plus small
//! chunk-walking utilities for `cafe inspect`/`verify`/`explain` that don't
//! belong in `cafe-format`/`cafe-codec`'s own public APIs (those crates
//! stay decoder/encoder-focused; presentation is this crate's job).
//!
//! CLI scope (see `AGENTS.md`'s Phase 9 entry, extended by the
//! HDR-CLI-support and 16-bit-CLI-support follow-ups): PNG (uint8/uint16,
//! gray/gray+alpha/RGB/RGBA, via `png_io`) plus HDR (32-bit float,
//! RGB/RGBA, via `hdr_io`) — every `sample_format`/`bit_depth`/
//! `color_type` combination spec section 4.1 allows now round-trips
//! through this crate's bridges, matching `cafe-codec`'s own scope
//! exactly.

pub mod chunks;
pub mod hdr_io;
pub mod png_io;

pub use chunks::{read_predictor_codes, walk_chunks, ChunkInfo};
pub use hdr_io::{cafe_pixels_to_dynamic_image_hdr, dynamic_image_to_cafe_pixels_hdr, HdrIoError};
pub use png_io::{
    cafe_pixels_to_dynamic_image, dynamic_image_to_cafe_pixels, CafePixels, PngIoError,
};
