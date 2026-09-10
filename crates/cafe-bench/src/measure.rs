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
//! Uses `zstd::encode_all` with the same default level, 19, matching
//! `cafe_codec::encoder::EncoderOptions::default().level`.

use cafe_codec::palette::build_palette;
use cafe_codec::{encode_bytes, EncoderOptions};
use cafe_format::constants::{COLOR_TYPE_RGBA, SAMPLE_FORMAT_FLOAT, SAMPLE_FORMAT_UINT};
use image::RgbaImage;
use std::io;
use std::path::Path;

/// ZSTD compression level used for the raw-buffer placeholder measurement,
/// matching `cafe_codec::encoder::EncoderOptions::default().level` so
/// numbers stay comparable.
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
    /// Whether `cafe_bytes` came from an indexed-color (`PLTE`) encode
    /// rather than a direct one — `cafe_codec::palette::build_palette`
    /// found at most 256 distinct exact colors *and* the resulting
    /// indexed encode was smaller than the direct one, mirroring the same
    /// race `cafe-encode` runs (see `AGENTS.md`'s Palette (0.3) phase).
    pub used_palette: bool,
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

/// Compressed-size measurement for a single HDR (`.exr`) source image.
///
/// Unlike [`Measurement`], there's no PNG baseline here — PNG can't
/// represent float32 samples without lossy tonemapping, which would be
/// comparing CAFE against a different (lossy) representation of the image
/// rather than an equivalent one. Instead, the baseline is the original
/// `.exr` file's size on disk: OpenEXR's own compression (PIZ/ZIP-style
/// wavelet+Huffman coding, tuned for HDR float data) is a meaningful
/// real-world comparison even though it's a different container format,
/// per this project's decision to report that number rather than only a
/// same-format one.
#[derive(Debug, Clone, Copy)]
pub struct HdrMeasurement {
    pub width: u32,
    pub height: u32,
    /// Uncompressed float32 RGBA size in bytes (`width * height * 4 * 4`).
    pub raw_bytes: usize,
    /// Size of the original `.exr` file on disk.
    pub exr_bytes: usize,
    /// Size of a real `.cafe` file produced by `cafe_codec::encode_bytes`
    /// over the decoded float32 RGBA samples.
    pub cafe_bytes: usize,
}

impl HdrMeasurement {
    /// CAFE size divided by raw size: smaller is better.
    pub fn cafe_ratio(&self) -> f64 {
        self.cafe_bytes as f64 / self.raw_bytes as f64
    }

    /// How CAFE compares to the original `.exr` file: `< 1.0` means CAFE
    /// produces a smaller file than the source `.exr`; `> 1.0` means the
    /// `.exr`'s own compression still wins. Unlike [`Measurement::
    /// cafe_vs_png`], this is not expected to always favor CAFE — OpenEXR's
    /// codecs are purpose-built for HDR float data, whereas CAFE's v0.1
    /// predictors were designed against uint8/16 neighbor deltas; see
    /// `AGENTS.md`'s HDR benchmark wiring note for measured results.
    pub fn cafe_vs_exr(&self) -> f64 {
        self.cafe_bytes as f64 / self.exr_bytes as f64
    }
}

/// Decodes the `.exr` file at `path` to float32 RGBA, encodes it through
/// `cafe_codec::encode_bytes`, and returns sizes for both against the
/// original file on disk.
pub fn measure_hdr(path: &Path) -> Result<HdrMeasurement, MeasureError> {
    let exr_bytes = std::fs::read(path)?;

    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    let (width, height) = (img.width(), img.height());
    let rgba32f = img.to_rgba32f();

    // Big-endian bytes per spec section 4.1's endianness rule for
    // bit_depth > 8 (mirrors what `cafe_codec::encoder` expects for any
    // multi-byte sample, uint16 or float32 alike).
    let mut raw_be = Vec::with_capacity(rgba32f.as_raw().len() * 4);
    for &sample in rgba32f.as_raw() {
        raw_be.extend_from_slice(&sample.to_be_bytes());
    }

    let cafe_bytes = encode_bytes(
        width,
        height,
        32,
        SAMPLE_FORMAT_FLOAT,
        COLOR_TYPE_RGBA,
        &raw_be,
        EncoderOptions::default(),
    )?;

    Ok(HdrMeasurement {
        width,
        height,
        raw_bytes: raw_be.len(),
        exr_bytes: exr_bytes.len(),
        cafe_bytes: cafe_bytes.len(),
    })
}

/// Encodes `img` as PNG, as a real `.cafe` file, and as raw-ZSTD, returning
/// all three sizes alongside the dimensions and uncompressed size.
pub fn measure(img: &RgbaImage) -> Result<Measurement, MeasureError> {
    let (width, height) = img.dimensions();
    let raw = img.as_raw();

    let mut png_bytes = Vec::new();
    img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut png_bytes))?;

    let direct_bytes = encode_bytes(
        width,
        height,
        8,
        SAMPLE_FORMAT_UINT,
        COLOR_TYPE_RGBA,
        raw,
        EncoderOptions::default(),
    )?;

    // Race an indexed (PLTE) encode against the direct one whenever the
    // image has few enough distinct exact colors, keeping whichever is
    // smaller — the same race `cafe-encode` runs (see `AGENTS.md`'s
    // Palette (0.3) phase); `build_palette` returning `None` (too many
    // distinct colors) is an ordinary outcome, not an error.
    let mut cafe_bytes = direct_bytes;
    let mut used_palette = false;
    if let Some(built) = build_palette(raw, 4) {
        let palette_bytes = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &built.indices,
            EncoderOptions {
                palette: Some(built.entries),
                ..Default::default()
            },
        )?;
        if palette_bytes.len() < cafe_bytes.len() {
            cafe_bytes = palette_bytes;
            used_palette = true;
        }
    }

    let zstd_raw = zstd::encode_all(raw.as_slice(), ZSTD_LEVEL)?;

    Ok(Measurement {
        width,
        height,
        raw_bytes: raw.len(),
        png_bytes: png_bytes.len(),
        cafe_bytes: cafe_bytes.len(),
        zstd_raw_bytes: zstd_raw.len(),
        used_palette,
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

    #[test]
    fn measure_palette_friendly_corpus_image_uses_palette() {
        // Real line-art content (256-color, irregular byte patterns) is
        // where PLTE's benefit was originally validated (see AGENTS.md's
        // Palette (0.3) phase) -- skipped gracefully if the corpus
        // checkout is incomplete, mirroring measure_hdr's test above.
        let path = crate::manifest::default_corpus_dir()
            .join("lineart")
            .join("abacus-psf.png");
        if !path.is_file() {
            return;
        }
        let img = image::open(&path).unwrap().to_rgba8();
        let m = measure(&img).expect("measurement should succeed");
        assert!(m.used_palette);
    }

    #[test]
    fn measure_hdr_produces_sane_sizes_for_corpus_files() {
        let hdr_dir = crate::manifest::default_corpus_dir().join("hdr");
        for name in ["Blobbies.exr", "Cannon.exr"] {
            let path = hdr_dir.join(name);
            if !path.is_file() {
                // Keep this test from failing in environments where the
                // corpus checkout is incomplete (e.g. a shallow clone);
                // the real assertions below only run against files that
                // are actually present.
                continue;
            }
            let m = measure_hdr(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(m.width > 0 && m.height > 0);
            assert_eq!(m.raw_bytes, (m.width * m.height * 4 * 4) as usize);
            assert!(m.exr_bytes > 0);
            assert!(m.cafe_bytes > 0);
            // Always smaller than the uncompressed float32 buffer, at
            // least -- this is not always true vs the original .exr (see
            // HdrMeasurement::cafe_vs_exr's docs).
            assert!(m.cafe_ratio() < 1.0);
        }
    }
}
