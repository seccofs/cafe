use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fs::File;
use std::io::Write;

// Create a simple test PNG in memory (for benchmarking)
fn create_simple_png_bytes() -> Vec<u8> {
    // A minimal 4x4 valid PNG file (created with imagemagick: convert -size 4x4 xc:red minimal.png)
    // This is a real PNG for testing without depending on image creation during benchmark
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // IHDR chunk
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
        0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x44, 0x44, 0x4f, 0x65, 0x41, 0x49, 0x44, 0x41, 0x54,
        0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0xe5, 0x21, 0xbc,
        0x33, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

fn benchmark_decode_bytes(c: &mut Criterion) {
    let png_bytes = create_simple_png_bytes();

    c.bench_function("decode_bytes_minimal_4x4_png", |b| {
        b.iter(|| {
            // This will fail (not a CAFE file), but the important thing is
            // that decode_bytes doesn't panic on the input
            let _ = cafe::decode_bytes(black_box(&png_bytes));
        });
    });
}

fn benchmark_encode_small_image(c: &mut Criterion) {
    // For encoding, we need to create a temporary PNG file first
    // This is done in the setup phase, outside the timed benchmark
    c.bench_function("encode_small_image_512x512", |b| {
        b.iter_batched(
            || {
                // Setup: create a simple test PNG
                let test_png = "target/bench_test_input.png";
                if !std::path::Path::new(test_png).exists() {
                    // Create a minimal PNG (or use pre-created one)
                    let png_bytes = create_simple_png_bytes();
                    let mut f = File::create(test_png).unwrap();
                    f.write_all(&png_bytes).unwrap();
                }
                test_png.to_string()
            },
            |test_png| {
                // Benchmark: encode the image
                let output_path = "target/bench_test_output.cafe";
                let opts = cafe::EncodeOptions {
                    use_filter: true,
                    level: 9,
                    adaptive_analysis: false,
                    target_color_type: 6, // RGBA
                    target_bit_depth: Some(8),
                    json_metadata: Default::default(),
                    exif: None,
                    sample_format: None,
                    chdr_metadata: None,
                    idim: None,
                    interlace_method: 0,
                    filter_heuristic: cafe::FilterHeuristic::Entropy,
                    use_byte_shuffle: false,
                    zstd_dictionary: None,
                    auto_dictionary: false,
                    icc_profile: None,
                    palette_algorithm: cafe::PaletteAlgorithm::NearestNeighbor,
                    tile_rows: 8,
                    xmp_metadata: None,
                    tonemap_operator: cafe::ToneMapOperator::Filmic,
                    use_filter_per_row: false,
                    inverse_tonemap: None,
                };
                let _ = cafe::encode(
                    black_box(&test_png),
                    black_box(output_path),
                    black_box(&opts),
                );
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    benchmark_decode_bytes,
    benchmark_encode_small_image,
);

criterion_main!(benches);
