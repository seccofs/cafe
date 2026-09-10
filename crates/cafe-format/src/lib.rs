//! CAFE image format: chunk container, `IHDR`, validation, and the
//! normative spec-as-code invariants (see `spec/` in the workspace root).
//!
//! This crate deliberately contains **no codec logic** — no predictors, no
//! ZSTD, no tiling heuristics. It only knows how to frame and validate
//! chunks per the CAFE format specification. See `cafe-codec` for the
//! encoder/decoder pipeline built on top of this crate.
//!
//! Status: skeleton (workspace reboot, phase 0). See `AGENTS.md` at the
//! workspace root for the implementation roadmap.

pub mod chunk;
pub mod constants;
pub mod error;
pub mod idim;
pub mod ihdr;
pub mod signature;

pub use chunk::{read_chunk, write_chunk, ReadChunk};
pub use error::{CafeError, Result};
pub use idim::Idim;
pub use ihdr::{read_ihdr, Ihdr};
pub use signature::validate_signature;
