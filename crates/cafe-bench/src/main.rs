//! `cafe-bench` — comparative benchmark harness: CAFE vs PNG over the
//! `corpus/` dataset.
//!
//! Per `AGENTS.md`'s "every feature proves itself with a benchmark first"
//! philosophy, this binary exists from the first commit, even before the
//! codec had anything meaningful to measure. Scope is intentionally
//! limited to PNG (via the `image` crate) vs CAFE — WebP/JPEG XL/AVIF are
//! deferred to avoid pulling in C-library bindings before there is a real
//! need to compare against them (see AGENTS.md).
//!
//! Status: phase 6 (`AGENTS.md`) — `cafe_codec::encode_bytes` now exists,
//! so `measure()` reports real CAFE sizes alongside PNG and the naive
//! raw-ZSTD floor. This binary still runs a small in-memory synthetic
//! matrix (gradient/checkerboard/noise x a few sizes) rather than walking
//! the real `corpus/` directory on disk; walking `corpus/manifest.json` and
//! reporting against real files is Phase 9/10's `cafe benchmark` subcommand
//! (`crates/cafe-cli/src/bin/cafe.rs`), which reuses this crate's
//! `measure()` directly rather than duplicating it here — see Phase 10's
//! `AGENTS.md` entry for the full-corpus results and the SIMD/palette/
//! dictionary go/no-go decision.

use cafe_bench::{generate, measure, Measurement, Pattern};

/// Dimensions exercised by the synthetic matrix. Kept small so the harness
/// runs in well under a second even at ZSTD level 19.
const SIZES: &[(u32, u32)] = &[(64, 64), (256, 256), (512, 512)];

fn patterns() -> Vec<Pattern> {
    vec![
        Pattern::Gradient,
        Pattern::Checkerboard { square_size: 8 },
        Pattern::Noise { seed: 0xDEAD_BEEF },
        Pattern::Texture,
    ]
}

fn print_header() {
    println!(
        "{:<16} {:>10} {:>12} {:>12} {:>12} {:>12} {:>9} {:>9} {:>9} {:>10}",
        "pattern", "dims", "raw", "png", "cafe", "zstd-raw", "png%", "cafe%", "zstd%", "cafe/png"
    );
}

fn print_row(name: &str, m: &Measurement) {
    println!(
        "{:<16} {:>4}x{:<5} {:>12} {:>12} {:>12} {:>12} {:>8.1}% {:>8.1}% {:>8.1}% {:>10.2}",
        name,
        m.width,
        m.height,
        m.raw_bytes,
        m.png_bytes,
        m.cafe_bytes,
        m.zstd_raw_bytes,
        m.png_ratio() * 100.0,
        m.cafe_ratio() * 100.0,
        m.zstd_raw_ratio() * 100.0,
        m.cafe_vs_png(),
    );
}

fn main() {
    println!(
        "cafe-bench: in-memory synthetic matrix (for the on-disk corpus/manifest.json, run \
         `cafe benchmark` instead — see AGENTS.md phase 10)\n"
    );
    print_header();

    for pattern in patterns() {
        for &(width, height) in SIZES {
            let img = generate(pattern, width, height);
            match measure(&img) {
                Ok(m) => print_row(&pattern.name(), &m),
                Err(e) => eprintln!(
                    "error measuring {} at {width}x{height}: {e}",
                    pattern.name()
                ),
            }
        }
    }
}
