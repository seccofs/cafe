//! `cafe-decode` — decoder CLI, legacy-compatible binary name.
//!
//! Scope (`AGENTS.md` Phase 9, extended by the HDR-CLI-support and
//! 16-bit-CLI-support follow-ups): reads a `.cafe` file via
//! `cafe_codec::decode_bytes`, writes a uint8/uint16 PNG (via `png_io`) or
//! a float32 HDR image such as `.exr` (via `hdr_io`) through the `image`
//! crate. Which bridge applies is decided by the decoded `Ihdr`'s
//! `sample_format` (`SAMPLE_FORMAT_FLOAT` -> `hdr_io`, `SAMPLE_FORMAT_UINT`
//! -> `png_io`) — `png_io` itself further dispatches on `bit_depth` (8 vs
//! 16). The output *file* format is still whatever `image` infers from
//! `<output>`'s extension (`.exr` for `Rgb32F`/`Rgba32F`, per `image`'s
//! `OpenExrEncoder`).

use cafe_cli::{cafe_pixels_to_dynamic_image, cafe_pixels_to_dynamic_image_hdr};
use cafe_codec::decode_bytes;
use cafe_format::constants::SAMPLE_FORMAT_FLOAT;
use std::env;
use std::process::ExitCode;

fn usage() {
    eprintln!("Usage: cafe-decode <input.cafe> <output>");
    eprintln!();
    eprintln!(
        "Supports 8-bit and 16-bit uint CAFE images (gray/gray+alpha/RGB/RGBA, writes PNG) \
         and float32 HDR CAFE images (RGB/RGBA, writes .exr)."
    );
}

fn run(input: &str, output: &str) -> Result<(), String> {
    let buf = std::fs::read(input).map_err(|e| format!("failed to read {input:?}: {e}"))?;
    let img = decode_bytes(&buf).map_err(|e| format!("decode failed: {e}"))?;

    let dynimg = if img.ihdr.sample_format == SAMPLE_FORMAT_FLOAT {
        cafe_pixels_to_dynamic_image_hdr(
            img.ihdr.width,
            img.ihdr.height,
            img.ihdr.bit_depth,
            img.ihdr.sample_format,
            img.ihdr.color_type,
            &img.pixels,
        )
        .map_err(|e| e.to_string())?
    } else {
        cafe_pixels_to_dynamic_image(
            img.ihdr.width,
            img.ihdr.height,
            img.ihdr.bit_depth,
            img.ihdr.sample_format,
            img.ihdr.color_type,
            &img.pixels,
        )
        .map_err(|e| e.to_string())?
    };

    dynimg
        .save(output)
        .map_err(|e| format!("failed to write {output:?}: {e}"))?;

    eprintln!(
        "wrote {output} ({}x{} pixels) from {input}",
        img.ihdr.width, img.ihdr.height
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        usage();
        return ExitCode::FAILURE;
    }
    match run(&args[1], &args[2]) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
