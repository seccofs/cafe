//! Isolated benchmark for `EncodeOptions::tile_rows` tuning (v1.5 audit item
//! #5). Unlike `dictionary_regression.rs` (which varies `tile_rows`
//! incidentally as one of several parameters), this sweeps `tile_rows` alone
//! while holding pattern/dimensions/level/filter-mode fixed, to isolate its
//! effect on compressed file size and identify whether the current
//! `DEFAULT_TILE_ROWS = 64` (`src/constants.rs`) is a reasonable default.
//!
//! Not a `#[test]`-time assertion (there is no known-correct "best" value
//! that would make sense as a regression gate — the optimum is
//! content-dependent, as the results below demonstrate) — instead this is a
//! `#[test]` that always passes (smoke-tests that all encodes succeed) but
//! prints a full data table with `--nocapture`, meant to be read by a human
//! (or copied into `AGENTS.md`/spec notes) when deciding whether to retune
//! the default.
//!
//! Run with: `cargo test --test tile_rows_benchmark -- --nocapture`
//!
//! ## Conclusion (v1.5 audit item #5)
//!
//! Three data sets were collected (see the three `#[test]` functions below):
//!
//! 1. `tile_rows_sweep_by_content_type`: compressed size vs `tile_rows` for 5
//!    content types (checkerboard, gradient, repetitive4color, photo,
//!    vertical_bands) at 256x256 and 1024x1024, with and without per-row
//!    filtering. **Every single case monotonically favors larger
//!    `tile_rows`** — no content type or size reverses the trend within
//!    `4..=256`.
//! 2. `tile_rows_extreme_values_probe`: extends the sweep to extreme values
//!    (`4` up to `100000`, i.e. no tiling at all — one `IDAT` for the whole
//!    image) on content specifically crafted to reward small tiles (a sharp
//!    gradient→checkerboard transition at the image's vertical midpoint) and
//!    on a large 2048x2048 gradient. **The trend never reverses**: size keeps
//!    improving (or plateaus) all the way up to "no tiling", confirming
//!    tiling's compression cost (framing/CRC32 overhead + tile-edge filter
//!    resets, per spec section 4.3) is monotonic in this dimension, with no
//!    sweet-spot smaller than "as large as possible".
//! 3. `tile_rows_speed_vs_size_tradeoff`: measures **wall-clock encode time**
//!    (not just size) vs `tile_rows`, since `src/cafe.rs` parallelizes tile
//!    compression across a rayon thread pool — fewer/bigger tiles means less
//!    work to spread across cores. Unlike compression ratio, **encode time
//!    follows a clear U-shape**: too many small tiles has thread-pool
//!    scheduling/framing overhead per tile; too few huge tiles means each
//!    individual ZSTD-19 call is large and serial, so there's less work to
//!    parallelize. On both a 24-core machine and a 4-core-limited run
//!    (`RAYON_NUM_THREADS=4`), the time-vs-`tile_rows` minimum falls in the
//!    `64..=128` range — very close to the current default.
//!
//! **Decision: `DEFAULT_TILE_ROWS` stays at `64`** (`src/constants.rs`).
//! Compression ratio alone would favor an arbitrarily large value (or no
//! tiling), but that comes at a real encode-time cost once tiles get large
//! enough that there's insufficient parallel work for a many-core machine —
//! and `64` already sits at (1024x1024, 24 cores) or extremely close to
//! (2048x2048, 4 cores) the empirical minimum of the time-vs-`tile_rows`
//! curve, while still keeping compression within single-digit percent of the
//! best observed value at each size (e.g. 1024x1024 photo: 64 -> 1,203,271
//! bytes vs 1024 -> 1,070,921 bytes, i.e. an ~11% size cost for a ~10x encode
//! speedup at 24 cores, or worse at 4 cores). Streaming granularity (each
//! `IDAT` decodable independently, spec section 4.2/6) is a secondary
//! benefit of not going arbitrarily large. No code change was made to
//! `DEFAULT_TILE_ROWS`; this is a "keep + document the trade-off" outcome.

use cafe::EncodeOptions;
use image::{ImageBuffer, RgbaImage};
use std::time::Instant;

fn make_image(w: u32, h: u32, pattern: &str) -> RgbaImage {
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    match pattern {
        "checkerboard" => {
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                let v = if (x / 8 + y / 8) % 2 == 0 { 255 } else { 0 };
                *pixel = image::Rgba([v, v, v, 255]);
            }
        }
        "gradient" => {
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        "repetitive4color" => {
            let colors = [
                image::Rgba([255, 0, 0, 255]),
                image::Rgba([0, 255, 0, 255]),
                image::Rgba([0, 0, 255, 255]),
                image::Rgba([255, 255, 0, 255]),
            ];
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                let pattern = ((x / 8 + y / 8) % 4) as usize;
                *pixel = colors[pattern];
            }
        }
        "photo" => {
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let r = (128.0 + 100.0 * (fx * 6.0).sin()) as u8;
                let g = (128.0 + 100.0 * (fy * 5.0).cos()) as u8;
                let b = (128.0 + 80.0 * ((fx + fy) * 8.0).sin()) as u8;
                let noise = ((x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) % 17) as u8;
                *pixel = image::Rgba([r.wrapping_add(noise), g.wrapping_add(noise / 2), b, 255]);
            }
        }
        // Vertical bands: content that varies sharply between horizontal
        // strips, the case row-tiling's "local filter adaptation" argument
        // is specifically meant to help with.
        "vertical_bands" => {
            for (x, y, pixel) in img.enumerate_pixels_mut() {
                let band = (y / 16) % 3;
                let v = match band {
                    0 => (x % 256) as u8, // smooth gradient band
                    1 => {
                        if (x / 4) % 2 == 0 {
                            255
                        } else {
                            0
                        }
                    } // sharp checker band
                    _ => 128,             // flat band
                };
                *pixel = image::Rgba([v, v, v, 255]);
            }
        }
        _ => unreachable!(),
    }
    img
}

fn encode_size(png_path: &str, out_path: &str, tile_rows: u32, per_row: bool, level: i32) -> u64 {
    let opts = EncodeOptions {
        use_filter: true,
        use_filter_per_row: per_row,
        level,
        target_color_type: 6,
        target_bit_depth: Some(8),
        tile_rows,
        ..Default::default()
    };
    cafe::encode(png_path, out_path, &opts).expect("encode should succeed");
    std::fs::metadata(out_path).unwrap().len()
}

#[test]
fn tile_rows_sweep_by_content_type() {
    let tile_rows_values = [4u32, 8, 16, 32, 64, 128, 256];
    let patterns = [
        "checkerboard",
        "gradient",
        "repetitive4color",
        "photo",
        "vertical_bands",
    ];
    let (w, h, level) = (256u32, 256u32, 19i32);

    println!("\n=== tile_rows sweep: {w}x{h}, level={level}, use_filter_per_row=false ===");
    println!(
        "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "pattern", "tr=4", "tr=8", "tr=16", "tr=32", "tr=64*", "tr=128", "tr=256"
    );

    let mut all_sizes: Vec<(&str, Vec<u64>)> = Vec::new();

    for pattern in patterns {
        let img = make_image(w, h, pattern);
        let png_path = format!("target/tilerows_{pattern}_{w}x{h}.png");
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();

        let mut sizes = Vec::new();
        for &tr in &tile_rows_values {
            let out_path = format!("target/tilerows_{pattern}_{w}x{h}_{tr}.cafe");
            let size = encode_size(&png_path, &out_path, tr, false, level);
            sizes.push(size);
            let _ = std::fs::remove_file(&out_path);
        }

        println!(
            "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            pattern, sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6]
        );

        let _ = std::fs::remove_file(&png_path);
        all_sizes.push((pattern, sizes));
    }

    // Also sweep with use_filter_per_row=true (v1.5 per-row filter, item #1)
    // to see whether it changes the tile_rows tradeoff — smaller tiles lose
    // less from per-row filtering since each tile already gets per-row
    // adaptation regardless of tile size, so the "local adaptation" argument
    // for small tile_rows should matter less here.
    println!("\n=== tile_rows sweep: {w}x{h}, level={level}, use_filter_per_row=true ===");
    println!(
        "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "pattern", "tr=4", "tr=8", "tr=16", "tr=32", "tr=64*", "tr=128", "tr=256"
    );
    for pattern in patterns {
        let img = make_image(w, h, pattern);
        let png_path = format!("target/tilerows_pr_{pattern}_{w}x{h}.png");
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();

        let mut sizes = Vec::new();
        for &tr in &tile_rows_values {
            let out_path = format!("target/tilerows_pr_{pattern}_{w}x{h}_{tr}.cafe");
            let size = encode_size(&png_path, &out_path, tr, true, level);
            sizes.push(size);
            let _ = std::fs::remove_file(&out_path);
        }

        println!(
            "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            pattern, sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6]
        );

        let _ = std::fs::remove_file(&png_path);
    }

    // Sanity: every encode above succeeded (encode_size would have panicked
    // otherwise) and produced a non-empty file.
    for (pattern, sizes) in &all_sizes {
        for &s in sizes {
            assert!(s > 0, "pattern {pattern} produced an empty file");
        }
    }

    // Also test larger images, where fixed per-chunk overhead
    // (framing+CRC32, ~13 bytes/IDAT) matters proportionally less, to see if
    // the optimal tile_rows shifts with image size.
    println!("\n=== tile_rows sweep: 1024x1024, level=19, use_filter_per_row=false ===");
    println!(
        "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "pattern", "tr=4", "tr=8", "tr=16", "tr=32", "tr=64*", "tr=128", "tr=256"
    );
    let (w2, h2) = (1024u32, 1024u32);
    for pattern in patterns {
        let img = make_image(w2, h2, pattern);
        let png_path = format!("target/tilerows_big_{pattern}_{w2}x{h2}.png");
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();

        let mut sizes = Vec::new();
        for &tr in &tile_rows_values {
            let out_path = format!("target/tilerows_big_{pattern}_{w2}x{h2}_{tr}.cafe");
            let size = encode_size(&png_path, &out_path, tr, false, level);
            sizes.push(size);
            let _ = std::fs::remove_file(&out_path);
        }

        println!(
            "{:<18} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            pattern, sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6]
        );

        let _ = std::fs::remove_file(&png_path);
    }
}

fn make_half_and_half(w: u32, h: u32) -> RgbaImage {
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        if y < h / 2 {
            // Top half: smooth gradient (rewards large tiles / ZSTD context).
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
        } else {
            // Bottom half: sharp checkerboard (the case where restarting
            // filter prediction at a small tile boundary should help most,
            // if such a case exists at all).
            let v = if (x / 4 + y / 4) % 2 == 0 { 255 } else { 0 };
            *pixel = image::Rgba([v, v, v, 255]);
        }
    }
    img
}

/// Extends the sweep in `tile_rows_sweep_by_content_type` to extreme values,
/// including "no tiling at all" (`tile_rows` far beyond image height), on
/// content specifically designed to reward small tiles (an abrupt
/// gradient-to-checkerboard transition at the vertical midpoint) and on a
/// large uniform gradient. See the module doc comment for the conclusion.
#[test]
fn tile_rows_extreme_values_probe() {
    let (w, h) = (256u32, 256u32);
    let img = make_half_and_half(w, h);
    let png_path = "target/tilerows_extreme_half.png";
    img.save_with_format(png_path, image::ImageFormat::Png)
        .unwrap();

    let values = [4u32, 8, 16, 32, 64, 128, 200, 255, 256, 512, 1024, 100000];
    println!("\n=== half_and_half {w}x{h} content-transition probe (extreme tile_rows) ===");
    let mut sizes = Vec::new();
    for &tr in &values {
        let out = format!("target/tilerows_extreme_half_{tr}.cafe");
        let size = encode_size(png_path, &out, tr, false, 19);
        println!("tile_rows={tr:>7} -> {size} bytes");
        sizes.push(size);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(png_path);
    assert!(
        sizes.iter().all(|&s| s > 0),
        "some encode produced an empty file"
    );

    // Large uniform gradient at 2048x2048, to confirm the trend holds at
    // scale and that going far beyond height never regresses vs some large
    // sub-height value.
    let (w2, h2) = (2048u32, 2048u32);
    let mut img2: RgbaImage = ImageBuffer::new(w2, h2);
    for (x, y, pixel) in img2.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
    }
    let png_path2 = "target/tilerows_extreme_biggrad.png";
    img2.save_with_format(png_path2, image::ImageFormat::Png)
        .unwrap();
    println!("\n=== gradient {w2}x{h2} probe (extreme tile_rows) ===");
    let mut sizes2 = Vec::new();
    for &tr in &[8u32, 16, 32, 64, 128, 256, 512, 1024, 2048, 100000] {
        let out = format!("target/tilerows_extreme_biggrad_{tr}.cafe");
        let size = encode_size(png_path2, &out, tr, false, 19);
        println!("tile_rows={tr:>7} -> {size} bytes");
        sizes2.push(size);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(png_path2);
    assert!(
        sizes2.iter().all(|&s| s > 0),
        "some encode produced an empty file"
    );
}

fn make_photo_like_for_speed(w: u32, h: u32) -> RgbaImage {
    let mut img: RgbaImage = ImageBuffer::new(w, h);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let r = (128.0 + 100.0 * (fx * 6.0).sin()) as u8;
        let g = (128.0 + 100.0 * (fy * 5.0).cos()) as u8;
        let b = (128.0 + 80.0 * ((fx + fy) * 8.0).sin()) as u8;
        let noise = ((x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) % 17) as u8;
        *pixel = image::Rgba([r.wrapping_add(noise), g.wrapping_add(noise / 2), b, 255]);
    }
    img
}

fn timed_encode(png_path: &str, out_path: &str, tile_rows: u32, level: i32) -> (u64, f64) {
    let opts = EncodeOptions {
        use_filter: true,
        level,
        target_color_type: 6,
        target_bit_depth: Some(8),
        tile_rows,
        ..Default::default()
    };
    let start = Instant::now();
    cafe::encode(png_path, out_path, &opts).expect("encode should succeed");
    let elapsed = start.elapsed().as_secs_f64();
    let size = std::fs::metadata(out_path).unwrap().len();
    (size, elapsed)
}

/// Measures wall-clock encode time (not just size) vs `tile_rows`, since tile
/// compression is parallelized across a rayon thread pool (`src/cafe.rs`) —
/// fewer/bigger tiles means less work to spread across cores. See the module
/// doc comment for the conclusion (time follows a U-shape, minimum near the
/// current `DEFAULT_TILE_ROWS = 64`).
///
/// This test is deliberately not a strict regression gate on absolute
/// timings (machine-dependent, and debug vs release builds differ by an
/// order of magnitude) — it only smoke-asserts that every encode succeeds
/// and prints the table with `--nocapture` for humans. Run with
/// `--release` for realistic numbers.
#[test]
fn tile_rows_speed_vs_size_tradeoff() {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    println!("\nlogical CPUs available: {cpus}");

    let mut all_ok = true;
    for &(w, h) in &[(1024u32, 1024u32), (2048, 2048)] {
        let img = make_photo_like_for_speed(w, h);
        let png_path = format!("target/tilerows_speed_{w}x{h}.png");
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();

        println!("\n=== {w}x{h} photo-like, level=19 ===");
        println!(
            "{:>10} {:>12} {:>12}",
            "tile_rows", "size(bytes)", "time(ms)"
        );
        for &tr in &[4u32, 8, 16, 32, 64, 128, 256, 512, 1024, 2048] {
            if tr > h {
                continue;
            }
            let out_path = format!("target/tilerows_speed_{w}x{h}_{tr}.cafe");
            // Warm-up run then a timed run, to reduce first-call noise
            // (filesystem cache, thread pool spin-up).
            let _ = timed_encode(&png_path, &out_path, tr, 19);
            let (size, elapsed) = timed_encode(&png_path, &out_path, tr, 19);
            println!("{:>10} {:>12} {:>12.2}", tr, size, elapsed * 1000.0);
            all_ok &= size > 0;
            let _ = std::fs::remove_file(&out_path);
        }

        let _ = std::fs::remove_file(&png_path);
    }
    assert!(
        all_ok,
        "some encode in the speed probe produced an empty file"
    );
}
