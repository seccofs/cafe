//! Criterion benchmarks for CAFE encode/decode vs PNG.
//!
//! Status: phase 6 (`AGENTS.md`). `cafe_bench::measure` now calls the real
//! `cafe_codec::encode_bytes` alongside PNG encode and the raw-ZSTD floor,
//! so timing `measure()` times all three together on a fixed synthetic
//! image. Splitting out isolated per-codec timings (e.g. CAFE decode alone)
//! is left for when that granularity is actually needed.

use cafe_bench::{generate, measure, Pattern};
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_measure_gradient_512(c: &mut Criterion) {
    let img = generate(Pattern::Gradient, 512, 512);
    c.bench_function("measure/gradient_512x512", |b| {
        b.iter(|| measure(&img).expect("measurement should succeed"))
    });
}

fn bench_measure_noise_512(c: &mut Criterion) {
    let img = generate(Pattern::Noise { seed: 0xDEAD_BEEF }, 512, 512);
    c.bench_function("measure/noise_512x512", |b| {
        b.iter(|| measure(&img).expect("measurement should succeed"))
    });
}

criterion_group!(benches, bench_measure_gradient_512, bench_measure_noise_512);
criterion_main!(benches);
