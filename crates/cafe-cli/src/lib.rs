//! Shared helpers for the `cafe-cli` binaries (`cafe`, `cafe-encode`,
//! `cafe-decode`): PNG <-> CAFE pixel-buffer conversion, plus small
//! chunk-walking utilities for `cafe inspect`/`verify`/`explain` that don't
//! belong in `cafe-format`/`cafe-codec`'s own public APIs (those crates
//! stay decoder/encoder-focused; presentation is this crate's job).
//!
//! v0.1 CLI scope (see `AGENTS.md`'s Phase 9 entry): PNG only, 8-bit only
//! (gray/gray+alpha/RGB/RGBA) - matches the current `corpus/`'s content and
//! `cafe-codec`'s most-tested path. 16-bit/float32 image I/O is deferred to
//! a later CLI pass; `cafe-codec` itself already supports those bit depths
//! (see its `encoder`/`decoder` tests), only this crate's PNG bridge is
//! narrower for now.

pub mod chunks;
pub mod png_io;

pub use chunks::{read_predictor_codes, walk_chunks, ChunkInfo};
pub use png_io::{cafe_pixels_to_dynamic_image, dynamic_image_to_cafe_pixels, PngIoError};
