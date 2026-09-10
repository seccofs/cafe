//! `cafe-bench` library: synthetic corpus generation and size measurement,
//! shared by the `cafe-bench` binary and the criterion benches.
//!
//! Since `AGENTS.md` phase 6, `measure()` compares PNG against the real
//! `cafe-codec` encoder, alongside a naive raw-ZSTD floor kept only as a
//! lower bound CAFE's predictors + tiling must beat.

pub mod corpus;
pub mod import;
pub mod manifest;
pub mod measure;

pub use corpus::{generate, Pattern};
pub use import::{import_sources, merge_and_write, SourceImage};
pub use manifest::{generate_corpus, Manifest};
pub use measure::{measure, Measurement};
