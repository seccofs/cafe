//! `cafe-decode` — decoder CLI, legacy-compatible binary name.
//!
//! v0.1 scope (`AGENTS.md` Phase 9): reads a `.cafe` file via
//! `cafe_codec::decode_bytes`, writes an 8-bit PNG via the `image` crate.

use cafe_cli::cafe_pixels_to_dynamic_image;
use cafe_codec::decode_bytes;
use std::env;
use std::process::ExitCode;

fn usage() {
    eprintln!("Usage: cafe-decode <input.cafe> <output.png>");
    eprintln!();
    eprintln!("v0.1 supports 8-bit CAFE images only (gray/gray+alpha/RGB/RGBA); writes PNG.");
}

fn run(input: &str, output: &str) -> Result<(), String> {
    let buf = std::fs::read(input).map_err(|e| format!("failed to read {input:?}: {e}"))?;
    let img = decode_bytes(&buf).map_err(|e| format!("decode failed: {e}"))?;

    let dynimg = cafe_pixels_to_dynamic_image(
        img.ihdr.width,
        img.ihdr.height,
        img.ihdr.bit_depth,
        img.ihdr.sample_format,
        img.ihdr.color_type,
        &img.pixels,
    )
    .map_err(|e| e.to_string())?;

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
