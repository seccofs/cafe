//! Size measurements: PNG baseline vs the real `cafe-codec` encoder vs a
//! naive raw-ZSTD placeholder.
//!
//! `cafe_raw_bytes` (via `cafe_codec::encode_bytes`, since Phase 6, see
//! `AGENTS.md`) is now the real number this crate's whole purpose is to
//! compare against PNG — `zstd_raw_bytes` (plain ZSTD over the raw RGBA8
//! buffer, no predictor/tiling at all) is kept alongside it only as the
//! naive floor CAFE's predictors + tiling must beat, not as a CAFE stand-in
//! anymore.
//!
//! Uses `zstd::encode_all` (the same API `old/src/codec.rs` wraps as
//! `compress_with_fallback`) with the same default level, 19, matching
//! `cafe_codec::encoder::EncoderOptions::default().level`.

use cafe_codec::{encode_bytes, EncoderOptions};
use cafe_format::constants::{COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT};
use image::RgbaImage;
use std::io;

/// ZSTD compression level used for the raw-buffer placeholder measurement.
///
/// Matches `old/src/constants.rs::ZSTD_LEVEL`, so numbers stay comparable
/// once `cafe-codec` grows a real encoder at the same level.
pub const ZSTD_LEVEL: i32 = 19;

/// Compressed-size measurements for a single generated image.
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Uncompressed RGBA8 size in bytes (`width * height * 4`).
    pub raw_bytes: usize,
    /// PNG-encoded size in bytes, via the `image` crate's default encoder.
    pub png_bytes: usize,
    /// Size of a real `.cafe` file produced by `cafe_codec::encode_bytes`
    /// (per-row predictor selection + ZSTD-with-fallback, single tile).
    pub cafe_bytes: usize,
    /// Size of `zstd::encode_all` applied directly to the raw RGBA8 buffer,
    /// with no predictor or tiling — the naive floor CAFE's predictors +
    /// tiling must beat.
    pub zstd_raw_bytes: usize,
}

impl Measurement {
    /// PNG size divided by raw size: smaller is better.
    pub fn png_ratio(&self) -> f64 {
        self.png_bytes as f64 / self.raw_bytes as f64
    }

    /// Placeholder-ZSTD size divided by raw size: smaller is better.
    pub fn zstd_raw_ratio(&self) -> f64 {
        self.zstd_raw_bytes as f64 / self.raw_bytes as f64
    }

    /// CAFE size divided by raw size: smaller is better.
    pub fn cafe_ratio(&self) -> f64 {
        self.cafe_bytes as f64 / self.raw_bytes as f64
    }

    /// How the ZSTD-raw placeholder compares to PNG: `< 1.0` means the
    /// placeholder already beats PNG (a low bar, since PNG uses filtering +
    /// DEFLATE while this placeholder filters nothing); `> 1.0` means PNG
    /// still wins, which is the expected starting point.
    pub fn zstd_raw_vs_png(&self) -> f64 {
        self.zstd_raw_bytes as f64 / self.png_bytes as f64
    }

    /// How real CAFE compares to PNG: `< 1.0` means CAFE already produces a
    /// smaller file than PNG for this image; `> 1.0` means PNG still wins.
    /// This is the number `AGENTS.md`'s Phase 10 ("Benchmark vs PNG") cares
    /// about.
    pub fn cafe_vs_png(&self) -> f64 {
        self.cafe_bytes as f64 / self.png_bytes as f64
    }
}

/// Measurement failure: wraps the independent failure sources (PNG encoding
/// via `image`, compression via `zstd`, encoding via `cafe-codec`) without
/// pulling any of those crates' error types into the public API surface
/// unnecessarily.
#[derive(Debug)]
pub enum MeasureError {
    Png(image::ImageError),
    Io(io::Error),
    Cafe(cafe_codec::CodecError),
}

impl std::fmt::Display for MeasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeasureError::Png(e) => write!(f, "PNG encoding failed: {e}"),
            MeasureError::Io(e) => write!(f, "ZSTD compression failed: {e}"),
            MeasureError::Cafe(e) => write!(f, "CAFE encoding failed: {e}"),
        }
    }
}

impl std::error::Error for MeasureError {}

impl From<image::ImageError> for MeasureError {
    fn from(e: image::ImageError) -> Self {
        MeasureError::Png(e)
    }
}

impl From<io::Error> for MeasureError {
    fn from(e: io::Error) -> Self {
        MeasureError::Io(e)
    }
}

impl From<cafe_codec::CodecError> for MeasureError {
    fn from(e: cafe_codec::CodecError) -> Self {
        MeasureError::Cafe(e)
    }
}

/// Encodes `img` as PNG, as a real `.cafe` file, and as raw-ZSTD, returning
/// all three sizes alongside the dimensions and uncompressed size.
pub fn measure(img: &RgbaImage) -> Result<Measurement, MeasureError> {
    let (width, height) = img.dimensions();
    let raw = img.as_raw();

    let mut png_bytes = Vec::new();
    img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut png_bytes))?;

    let cafe_bytes = encode_bytes(
        width,
        height,
        8,
        SAMPLE_FORMAT_UINT,
        COLOR_TYPE_RGBA,
        raw,
        EncoderOptions::default(),
    )?;

    let zstd_raw = zstd::encode_all(raw.as_slice(), ZSTD_LEVEL)?;

    Ok(Measurement {
        width,
        height,
        raw_bytes: raw.len(),
        png_bytes: png_bytes.len(),
        cafe_bytes: cafe_bytes.len(),
        zstd_raw_bytes: zstd_raw.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{generate, Pattern};

    #[test]
    fn measure_gradient_produces_sane_sizes() {
        let img = generate(Pattern::Gradient, 64, 64);
        let m = measure(&img).expect("measurement should succeed");
        assert_eq!(m.raw_bytes, 64 * 64 * 4);
        assert!(m.png_bytes > 0);
        assert!(m.cafe_bytes > 0);
        assert!(m.zstd_raw_bytes > 0);
        // A smooth gradient should compress well under every codec.
        assert!(m.png_ratio() < 1.0);
        assert!(m.cafe_ratio() < 1.0);
        assert!(m.zstd_raw_ratio() < 1.0);
    }

    #[test]
    fn measure_noise_compresses_poorly_for_png_and_raw_zstd() {
        let img = generate(Pattern::Noise { seed: 1 }, 64, 64);
        let m = measure(&img).expect("measurement should succeed");
        // Mostly-incompressible noise: the alpha channel is constant 255
        // (1/4 of bytes), so a small amount of shrinkage is expected even
        // here, but nowhere near what a smooth gradient achieves.
        assert!(m.png_ratio() > 0.8);
        assert!(m.zstd_raw_ratio() > 0.8);
        // CAFE is a deliberate exception here, not a bug: this generator's
        // per-pixel low bytes come from a 64-bit LCG (corpus.rs's
        // `lcg_next`), whose low-order output bits are far more correlated
        // sample-to-sample than a true RNG's would be. Per-row predictor
        // selection (`choose_best_row_predictor`) finds and exploits that
        // structure -- unlike PNG's/raw-ZSTD's fixed, un-adapted approach --
        // so CAFE compresses this specific "noise" pattern to under a third
        // of its raw size (~30%) instead of the ~80% PNG/raw-ZSTD land on.
        // A cryptographically-random corpus entry would not show this gap;
        // treat this pattern's "noise" label as "LCG output", not "entropy
        // upper bound".
        assert!(m.cafe_ratio() < 0.5);
    }
}
