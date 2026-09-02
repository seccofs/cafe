//! Regression guard for the v1.5 auto-dictionary fallback guarantee
//! (section: `compress_with_fallback_dict` / `encode()`'s IDAT+zDIC
//! decision in `src/cafe.rs`).
//!
//! `EncodeOptions::auto_dictionary` trains and uses a ZSTD dictionary
//! automatically. Because the `zDIC` chunk carries a fixed overhead (its own
//! chunk framing plus the trained dictionary bytes), and because ZSTD's
//! dictionary-compression frame format has its own fixed overhead per IDAT,
//! naively always emitting `zDIC` + always compressing IDATs with the
//! dictionary can make small/highly-repetitive images *larger* than not
//! using a dictionary at all (observed up to +77.7% during the v1.4.2
//! compression audit).
//!
//! The fix has two layers:
//! 1. Per-IDAT: `compress_with_fallback_dict` always compares
//!    {raw, zstd-no-dict, zstd-with-dict} and keeps the smallest.
//! 2. Whole-file: if the auto-trained dictionary won at least one IDAT,
//!    `encode()` compares the *total* file size (zDIC chunk + IDATs) against
//!    re-encoding all IDATs with no dictionary at all, and keeps whichever
//!    total is smaller.
//!
//! This test asserts that, across a range of synthetic patterns (repetitive,
//! checkerboard, gradient, photo-like) and encode parameters (tile size,
//! ZSTD level), enabling `auto_dictionary` never produces a `.cafe` file
//! larger than the equivalent encode with `auto_dictionary: false`.

use cafe::EncodeOptions;
use image::{ImageBuffer, RgbaImage};

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
        _ => unreachable!(),
    }
    img
}

fn encode_size(png_path: &str, out_path: &str, auto_dict: bool, tile_rows: u32, level: i32) -> u64 {
    let opts = EncodeOptions {
        use_filter: true,
        level,
        target_color_type: 6,
        target_bit_depth: Some(8),
        auto_dictionary: auto_dict,
        tile_rows,
        ..Default::default()
    };
    cafe::encode(png_path, out_path, &opts).expect("encode should succeed");
    std::fs::metadata(out_path).unwrap().len()
}

#[test]
fn auto_dictionary_never_regresses_file_size() {
    let cases: Vec<(&str, u32, u32, u32, i32)> = vec![
        ("repetitive4color", 64, 64, 64, 9),
        ("repetitive4color", 64, 64, 8, 9),
        ("checkerboard", 256, 256, 16, 19),
        ("checkerboard", 256, 256, 64, 19),
        ("gradient", 256, 256, 16, 19),
        ("gradient", 256, 256, 64, 19),
        ("checkerboard", 512, 512, 16, 19),
        ("checkerboard", 512, 512, 64, 19),
        ("repetitive4color", 512, 512, 16, 9),
        ("repetitive4color", 512, 512, 64, 9),
        ("photo", 256, 256, 16, 19),
        ("photo", 512, 512, 16, 19),
        ("photo", 512, 512, 64, 19),
    ];

    println!(
        "{:<18} {:>6} {:>6} {:>10} {:>6} | {:>10} {:>10} {:>8}",
        "pattern", "w", "h", "tile_rows", "level", "no_dict", "auto_dict", "delta%"
    );

    let mut any_regression = false;
    let mut regressions = Vec::new();
    for (pattern, w, h, tile_rows, level) in cases {
        let img = make_image(w, h, pattern);
        let png_path = format!("target/dictregress_{}_{}x{}.png", pattern, w, h);
        img.save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();

        let out_no = format!(
            "target/dictregress_{}_{}x{}_{}_nodict.cafe",
            pattern, w, h, tile_rows
        );
        let out_auto = format!(
            "target/dictregress_{}_{}x{}_{}_autodict.cafe",
            pattern, w, h, tile_rows
        );

        let size_no = encode_size(&png_path, &out_no, false, tile_rows, level);
        let size_auto = encode_size(&png_path, &out_auto, true, tile_rows, level);

        let delta_pct = 100.0 * (size_auto as f64 - size_no as f64) / size_no as f64;
        if size_auto > size_no {
            any_regression = true;
            regressions.push(format!(
                "{pattern} {w}x{h} tile_rows={tile_rows} level={level}: {size_no} -> {size_auto} ({delta_pct:+.2}%)"
            ));
        }

        println!(
            "{:<18} {:>6} {:>6} {:>10} {:>6} | {:>10} {:>10} {:>7.2}%",
            pattern, w, h, tile_rows, level, size_no, size_auto, delta_pct
        );

        let _ = std::fs::remove_file(&png_path);
        let _ = std::fs::remove_file(&out_no);
        let _ = std::fs::remove_file(&out_auto);
    }

    assert!(
        !any_regression,
        "auto_dictionary should never produce a larger file than not using it, but found regressions:\n{}",
        regressions.join("\n")
    );
}
