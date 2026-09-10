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
    eprintln!("Usage: cafe-decode <input.cafe> <output> [options]");
    eprintln!();
    eprintln!(
        "Supports 8-bit and 16-bit uint CAFE images (gray/gray+alpha/RGB/RGBA, writes PNG) \
         and float32 HDR CAFE images (RGB/RGBA, writes .exr)."
    );
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --extract-exif <file>  Write the file's eXIF chunk bytes to <file>, if present");
    eprintln!("  --extract-icc <file>   Write the file's iCCP chunk bytes to <file>, if present");
    eprintln!("  --extract-xmp <file>   Write the file's xMPd XML text to <file>, if present");
}

struct Args {
    input: String,
    output: String,
    extract_exif: Option<String>,
    extract_icc: Option<String>,
    extract_xmp: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    if args.len() < 2 {
        return Err("missing <input.cafe> and/or <output>".to_string());
    }
    let input = args[0].clone();
    let output = args[1].clone();
    let mut extract_exif = None;
    let mut extract_icc = None;
    let mut extract_xmp = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--extract-exif" => {
                extract_exif = Some(
                    args.get(i + 1)
                        .ok_or("--extract-exif requires a file path")?
                        .clone(),
                );
                i += 2;
            }
            "--extract-icc" => {
                extract_icc = Some(
                    args.get(i + 1)
                        .ok_or("--extract-icc requires a file path")?
                        .clone(),
                );
                i += 2;
            }
            "--extract-xmp" => {
                extract_xmp = Some(
                    args.get(i + 1)
                        .ok_or("--extract-xmp requires a file path")?
                        .clone(),
                );
                i += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(Args {
        input,
        output,
        extract_exif,
        extract_icc,
        extract_xmp,
    })
}

fn run(args: &Args) -> Result<(), String> {
    let input = &args.input;
    let output = &args.output;
    let buf = std::fs::read(input).map_err(|e| format!("failed to read {input:?}: {e}"))?;
    let img = decode_bytes(&buf).map_err(|e| format!("decode failed: {e}"))?;

    if let Some(path) = &args.extract_exif {
        match &img.exif {
            Some(bytes) => {
                std::fs::write(path, bytes).map_err(|e| format!("failed to write {path:?}: {e}"))?
            }
            None => eprintln!("warning: {input} has no eXIF chunk, nothing written to {path}"),
        }
    }
    if let Some(path) = &args.extract_icc {
        match &img.icc_profile {
            Some(bytes) => {
                std::fs::write(path, bytes).map_err(|e| format!("failed to write {path:?}: {e}"))?
            }
            None => eprintln!("warning: {input} has no iCCP chunk, nothing written to {path}"),
        }
    }
    if let Some(path) = &args.extract_xmp {
        match &img.xmp {
            Some(xmpd) => std::fs::write(path, &xmpd.xml)
                .map_err(|e| format!("failed to write {path:?}: {e}"))?,
            None => eprintln!("warning: {input} has no xMPd chunk, nothing written to {path}"),
        }
    }

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
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let parsed = match parse_args(&raw_args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            usage();
            return ExitCode::FAILURE;
        }
    };

    match run(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
