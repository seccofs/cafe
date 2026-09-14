//! Criterion benchmark comparing sequential vs. parallel tile
//! encode/decode (`AGENTS.md`'s parallel-tiling phase): this is the
//! permanent record of the throughput numbers a throwaway PoC first
//! measured before any of `crate::parallel`/`encode_bytes_parallel`/
//! `decode_bytes_parallel` were written, per this project's "every
//! feature proves itself with a benchmark first" principle.
//!
//! Mirrors that PoC's own finding: ZSTD dominates encode time so heavily
//! at the encoder's default level (19) that parallelizing tile work only
//! saves a modest fraction end-to-end, while at fast levels (1/3, the
//! interactive/preview use case) predictor selection dominates instead
//! and parallel tiling wins by a much wider margin — so this benchmark
//! deliberately covers both regimes rather than only the default level,
//! to keep that scope decision honest and re-checkable over time.
//! Decode is fast at every level (no per-level split needed there).

use cafe_codec::{decode_bytes, decode_bytes_parallel, encode_bytes, encode_bytes_parallel};
use cafe_codec::{CodecError, EncoderOptions};
use cafe_format::constants::{COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

const TILE_SIZE: (u16, u16) = (64, 64);

fn pseudo_random_rgba(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2685821657736338717).wrapping_add(1);
    (0..(width as usize * height as usize * 4))
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect()
}

fn encode_options(level: i32) -> EncoderOptions {
    EncoderOptions {
        level,
        tile_size: Some(TILE_SIZE),
        ..Default::default()
    }
}

fn bench_encode_at_level(
    c: &mut Criterion,
    level: i32,
    width: u32,
    height: u32,
) -> Result<(), CodecError> {
    let raw = pseudo_random_rgba(width, height, level as u64);
    let options = encode_options(level);
    let group_label = format!("encode/level{level}/{width}x{height}");

    c.bench_function(&format!("{group_label}/sequential"), |b| {
        b.iter(|| {
            encode_bytes(
                black_box(width),
                black_box(height),
                8,
                SAMPLE_FORMAT_UINT,
                COLOR_TYPE_RGBA,
                black_box(&raw),
                options.clone(),
            )
            .unwrap()
        })
    });
    c.bench_function(&format!("{group_label}/parallel"), |b| {
        b.iter(|| {
            encode_bytes_parallel(
                black_box(width),
                black_box(height),
                8,
                SAMPLE_FORMAT_UINT,
                COLOR_TYPE_RGBA,
                black_box(&raw),
                options.clone(),
            )
            .unwrap()
        })
    });
    Ok(())
}

// Fast levels (1, 3): predictor selection dominates ZSTD, so this is the
// regime where parallel tiling's win is largest — sized generously
// (1024x1024, 256 tiles) since each iteration is cheap even sequentially.
fn bench_encode_level1(c: &mut Criterion) {
    bench_encode_at_level(c, 1, 1024, 1024).unwrap();
}

fn bench_encode_level3(c: &mut Criterion) {
    bench_encode_at_level(c, 3, 1024, 1024).unwrap();
}

// Default level (19): ZSTD dominates, so this is the regime where
// parallel tiling's win is smallest — sized down (256x256, 16 tiles) so
// the sequential baseline still finishes in a reasonable time per sample.
fn bench_encode_level19_default(c: &mut Criterion) {
    bench_encode_at_level(c, 19, 256, 256).unwrap();
}

fn bench_decode(c: &mut Criterion) {
    let width = 512u32;
    let height = 512u32;
    let raw = pseudo_random_rgba(width, height, 99);
    let buf = encode_bytes(
        width,
        height,
        8,
        SAMPLE_FORMAT_UINT,
        COLOR_TYPE_RGBA,
        &raw,
        encode_options(19),
    )
    .unwrap();

    c.bench_function("decode/512x512/sequential", |b| {
        b.iter(|| decode_bytes(black_box(&buf)).unwrap())
    });
    c.bench_function("decode/512x512/parallel", |b| {
        b.iter(|| decode_bytes_parallel(black_box(&buf)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_encode_level1,
    bench_encode_level3,
    bench_encode_level19_default,
    bench_decode
);
criterion_main!(benches);
