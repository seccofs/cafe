//! Criterion benchmark isolating predictor filter throughput: scalar
//! reference vs the transparently-dispatched (SIMD-if-available) path,
//! for every predictor code at a representative RGBA row (`bpp = 4`,
//! 4096 bytes = a 1024px-wide RGBA row). This is the throughput number
//! that justified SIMD's scope decision in `AGENTS.md` (SIMD matters at
//! fast ZSTD levels where predictor selection dominates encode time, not
//! at the default level 19 where ZSTD itself dominates).

use cafe_codec::predictor::{
    filter_row, filter_row_scalar, unfilter_row, unfilter_row_scalar, PREDICTOR_AVERAGE,
    PREDICTOR_GRADIENT, PREDICTOR_PAETH, PREDICTOR_SUB, PREDICTOR_UP,
};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

const ROW_WIDTH_PX: usize = 1024;
const BPP: usize = 4;
const ROW_LEN: usize = ROW_WIDTH_PX * BPP;

fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2685821657736338717).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect()
}

fn bench_filter_row(c: &mut Criterion) {
    let row = pseudo_random_bytes(ROW_LEN, 1);
    let prev_row = pseudo_random_bytes(ROW_LEN, 2);

    for &(name, code) in &[
        ("sub", PREDICTOR_SUB),
        ("up", PREDICTOR_UP),
        ("average", PREDICTOR_AVERAGE),
        ("paeth", PREDICTOR_PAETH),
        ("gradient", PREDICTOR_GRADIENT),
    ] {
        c.bench_function(&format!("filter_row/scalar/{name}"), |b| {
            b.iter(|| {
                filter_row_scalar(black_box(&row), Some(black_box(&prev_row)), code, BPP).unwrap()
            })
        });
        c.bench_function(&format!("filter_row/dispatched/{name}"), |b| {
            b.iter(|| filter_row(black_box(&row), Some(black_box(&prev_row)), code, BPP).unwrap())
        });
    }
}

fn bench_unfilter_row(c: &mut Criterion) {
    let filtered = pseudo_random_bytes(ROW_LEN, 3);
    let prev_row = pseudo_random_bytes(ROW_LEN, 4);

    c.bench_function("unfilter_row/scalar/up", |b| {
        b.iter(|| {
            unfilter_row_scalar(
                black_box(&filtered),
                Some(black_box(&prev_row)),
                PREDICTOR_UP,
                BPP,
            )
            .unwrap()
        })
    });
    c.bench_function("unfilter_row/dispatched/up", |b| {
        b.iter(|| {
            unfilter_row(
                black_box(&filtered),
                Some(black_box(&prev_row)),
                PREDICTOR_UP,
                BPP,
            )
            .unwrap()
        })
    });
}

criterion_group!(benches, bench_filter_row, bench_unfilter_row);
criterion_main!(benches);
