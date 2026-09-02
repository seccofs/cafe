//! Real filter/SIMD performance benchmarks.
//!
//! Unlike a synthetic microbenchmark against unrelated code, these
//! benchmarks exercise the actual encode pipeline (`cafe::encode`) with the
//! predictive filter enabled, which is the real caller of `filter.rs`'s
//! `filter_block`/`filter_row` — and, through them, of the AVX2-accelerated
//! kernels in `simd.rs` (Filters 1-3, `simd` feature, default: enabled).
//! Internal filter functions are `pub(crate)`, not `pub`, so they cannot be
//! called directly from an external bench crate; going through the public
//! `encode()` API is the only way to reach them without changing visibility
//! just for benchmarking purposes.
//!
//! To measure the actual AVX2-vs-scalar delta, run this bench twice and
//! compare wall-clock time:
//!
//! ```text
//! cargo bench --bench simd_performance                      # SIMD enabled (default)
//! cargo bench --bench simd_performance --no-default-features # scalar fallback only
//! ```
//!
//! Each image pattern below mirrors the ones used in
//! `tests/heuristic_benchmark.rs` and `tests/integration_roundtrip.rs`
//! (checkerboard, gradient, random), so results are comparable with the
//! compression-ratio numbers already tracked elsewhere in the test suite.

use cafe::{encode, EncodeOptions};
use criterion::{criterion_group, criterion_main, Criterion};
use std::fs;
use std::path::PathBuf;

/// Generates a synthetic RGBA image, saves it as a PNG under `target/`, and
/// returns the path. Called once per benchmark group during setup (not
/// timed) — the benchmark itself only exercises `cafe::encode`.
fn write_bench_png(name: &str, width: u32, height: u32, pattern: &str) -> PathBuf {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) as usize) * 4;
            match pattern {
                "checkerboard" => {
                    let is_white = ((x / 8) + (y / 8)) % 2 == 0;
                    let v = if is_white { 255 } else { 0 };
                    pixels[idx] = v;
                    pixels[idx + 1] = v;
                    pixels[idx + 2] = v;
                    pixels[idx + 3] = 255;
                }
                "gradient" => {
                    pixels[idx] = (x * 255 / width.max(1)) as u8;
                    pixels[idx + 1] = (y * 255 / height.max(1)) as u8;
                    pixels[idx + 2] = 128;
                    pixels[idx + 3] = 255;
                }
                _ => {
                    // Pseudo-random pattern, deterministic across runs.
                    let seed = ((x ^ y).wrapping_mul(31)) as usize;
                    pixels[idx] = (seed % 256) as u8;
                    pixels[idx + 1] = ((seed * 17) % 256) as u8;
                    pixels[idx + 2] = ((seed * 43) % 256) as u8;
                    pixels[idx + 3] = 255;
                }
            }
        }
    }

    let dir = PathBuf::from("target/bench_simd_performance");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.png"));
    image::RgbaImage::from_raw(width, height, pixels)
        .unwrap()
        .save(&path)
        .expect("failed to save benchmark PNG");
    path
}

/// Benchmarks `encode()` with the predictive filter enabled (default
/// heuristic: Entropy) on representative image patterns/sizes — this is the
/// real code path that dispatches to `simd.rs`'s AVX2 kernels for Filters
/// 1-3 when the `simd` feature is enabled (default).
fn filter_benchmark(c: &mut Criterion) {
    let output_path = "target/bench_simd_performance/output.cafe";
    let cases: &[(&str, u32, u32, &str)] = &[
        ("checkerboard_256x256", 256, 256, "checkerboard"),
        ("gradient_512x512", 512, 512, "gradient"),
        ("random_256x256", 256, 256, "random"),
    ];

    for &(name, width, height, pattern) in cases {
        let input_path = write_bench_png(name, width, height, pattern);
        let input_path_str = input_path.to_str().unwrap().to_string();

        c.bench_function(&format!("encode_predictive_filter_{name}"), |b| {
            let opts = EncodeOptions {
                use_filter: true,
                level: 3, // low level: keeps ZSTD cost small relative to filtering
                target_color_type: 6,
                ..Default::default()
            };
            b.iter(|| {
                encode(&input_path_str, output_path, &opts).expect("encode failed in benchmark");
            });
        });
    }
}

/// Benchmarks sub-byte sample packing (`pack_samples_row`, `simd_packing.rs`)
/// indirectly through `encode_indexed()` with a low-color-count image, the
/// real caller of the AVX2-accelerated 1/2/4-bit packing kernels.
fn packing_benchmark(c: &mut Criterion) {
    let output_path = "target/bench_simd_performance/output_indexed.cafe";
    // Few distinct colors so the palette stays small and bit_depth is packed
    // tightly (exercises pack_1bit/2bit/4bit depending on color count).
    let input_path = write_bench_png("packing_checkerboard_256x256", 256, 256, "checkerboard");
    let input_path_str = input_path.to_str().unwrap().to_string();

    c.bench_function("encode_indexed_packing_checkerboard_256x256", |b| {
        let opts = EncodeOptions {
            use_filter: true,
            level: 3,
            ..Default::default()
        };
        b.iter(|| {
            cafe::encode_indexed(&input_path_str, output_path, &opts)
                .expect("encode_indexed failed in benchmark");
        });
    });
}

criterion_group!(benches, filter_benchmark, packing_benchmark);
criterion_main!(benches);
