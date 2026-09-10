//! Deterministic synthetic image generators.
//!
//! Per `AGENTS.md` phase 2, the real `corpus/` directory (photo, screenshot,
//! illustration, ...) is populated later; the generators here exist first so
//! `cafe-bench` has something to measure from commit zero, without depending
//! on any external asset or a `rand`-style crate. Every pattern is a pure
//! function of `(x, y)` (plus, for [`Pattern::Noise`], a `seed`), so the same
//! call always produces byte-identical output — this matters for reproducible
//! benchmarks and, later, for golden files.
//!
//! Formulas are adapted from `old/benches/benchmark_image.rs` and
//! `old/tests/*_test.rs`, which independently arrived at the same
//! hand-rolled-hash / LCG approach for the same reason: no `rand` dependency.

use image::{Rgba, RgbaImage};

/// A synthetic test pattern, generated deterministically from its
/// parameters alone (no external state, no I/O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Smooth linear ramp: low-frequency content, the easiest case for any
    /// predictor-based compressor.
    Gradient,
    /// Hard-edged alternating blocks of `square_size` pixels: worst case for
    /// smoothing predictors (Average/Paeth), best case for None/Sub across
    /// block boundaries.
    Checkerboard { square_size: u32 },
    /// Deterministic pseudo-random noise (64-bit LCG, MMIX/PCG constants),
    /// seeded so the same `seed` always reproduces the same image. Worst
    /// case for every predictor and a lower bound on achievable ratio.
    Noise { seed: u64 },
    /// Smooth sinusoidal bands plus a small multiplicative-hash noise term:
    /// mid-frequency, "textured" content that is neither as trivial as
    /// `Gradient` nor as adversarial as `Noise`. Formula ported verbatim
    /// from `old/tests/dictionary_regression.rs`'s `"photo"` pattern (also
    /// duplicated in `old/tests/tile_rows_benchmark.rs`), renamed here
    /// since it stands in for procedural texture, not photographic content
    /// (deferred to the real `corpus/photo/` category, see `AGENTS.md`).
    Texture,
}

impl Pattern {
    /// Short, filesystem- and report-friendly name.
    pub fn name(&self) -> String {
        match self {
            Pattern::Gradient => "gradient".to_string(),
            Pattern::Checkerboard { square_size } => format!("checkerboard{square_size}"),
            Pattern::Noise { seed } => format!("noise{seed:x}"),
            Pattern::Texture => "texture".to_string(),
        }
    }
}

/// Generates an RGBA8 image of the given pattern and dimensions.
///
/// `width` and `height` must be non-zero; this is a synthetic-data helper,
/// not a validated public API, so it panics rather than returning a
/// `Result` on invalid input.
pub fn generate(pattern: Pattern, width: u32, height: u32) -> RgbaImage {
    assert!(width > 0 && height > 0, "corpus images must be non-empty");
    match pattern {
        Pattern::Gradient => generate_gradient(width, height),
        Pattern::Checkerboard { square_size } => {
            generate_checkerboard(width, height, square_size.max(1))
        }
        Pattern::Noise { seed } => generate_noise(width, height, seed),
        Pattern::Texture => generate_texture(width, height),
    }
}

fn generate_gradient(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let r = (x * 255 / width.max(1)) as u8;
        let g = (y * 255 / height.max(1)) as u8;
        Rgba([r, g, 128, 255])
    })
}

fn generate_checkerboard(width: u32, height: u32, square_size: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let on = ((x / square_size) + (y / square_size)).is_multiple_of(2);
        let v = if on { 255 } else { 0 };
        Rgba([v, v, v, 255])
    })
}

/// 64-bit LCG step using the MMIX/Knuth constants, matching
/// `old/benches/benchmark_image.rs`'s hand-rolled generator so both
/// implementations are cross-checkable against the same formula.
fn lcg_next(state: u64) -> u64 {
    state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

fn generate_noise(width: u32, height: u32, seed: u64) -> RgbaImage {
    let mut buf = RgbaImage::new(width, height);
    let mut state = seed;
    for pixel in buf.pixels_mut() {
        state = lcg_next(state);
        let bytes = state.to_le_bytes();
        *pixel = Rgba([bytes[0], bytes[1], bytes[2], 255]);
    }
    buf
}

fn generate_texture(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let fx = x as f32 / width as f32;
        let fy = y as f32 / height as f32;
        let r = (128.0 + 100.0 * (fx * 6.0).sin()) as u8;
        let g = (128.0 + 100.0 * (fy * 5.0).cos()) as u8;
        let b = (128.0 + 80.0 * ((fx + fy) * 8.0).sin()) as u8;
        // Knuth multiplicative-hash constants, matching the noise term in
        // old/tests/dictionary_regression.rs's "photo" pattern.
        let noise = ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40503)) % 17) as u8;
        Rgba([r.wrapping_add(noise), g.wrapping_add(noise / 2), b, 255])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_deterministic() {
        for pattern in [
            Pattern::Gradient,
            Pattern::Checkerboard { square_size: 8 },
            Pattern::Noise { seed: 0xDEAD_BEEF },
            Pattern::Texture,
        ] {
            let a = generate(pattern, 32, 24);
            let b = generate(pattern, 32, 24);
            assert_eq!(
                a.as_raw(),
                b.as_raw(),
                "{} must be deterministic",
                pattern.name()
            );
        }
    }

    #[test]
    fn generate_respects_dimensions() {
        let img = generate(Pattern::Gradient, 17, 5);
        assert_eq!(img.width(), 17);
        assert_eq!(img.height(), 5);
    }

    #[test]
    #[should_panic(expected = "non-empty")]
    fn generate_rejects_zero_dimensions() {
        generate(Pattern::Gradient, 0, 10);
    }
}
