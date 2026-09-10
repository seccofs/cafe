//! `cafe-encode` — encoder CLI, legacy-compatible binary name.
//!
//! v0.1 scope (`AGENTS.md` Phase 9): reads an 8-bit PNG via the `image`
//! crate, writes a `.cafe` file via `cafe_codec::encode_bytes`. Tiling and
//! ZSTD level are exposed as flags; predictor selection is not (it's
//! always the per-row entropy heuristic, an encoder-internal decision the
//! spec never surfaces to callers).

use cafe_cli::dynamic_image_to_cafe_pixels;
use cafe_codec::{encode_bytes, EncoderOptions};
use cafe_format::constants::{SCAN_ORDER_ROW_MAJOR, SCAN_ORDER_Z_ORDER};
use std::env;
use std::process::ExitCode;

fn usage() {
    eprintln!("Usage: cafe-encode <input.png> <output.cafe> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!(
        "  --tile-size <WxH>   Split into WxH tiles with an iDIM chunk (default: single tile)"
    );
    eprintln!("  --scan-order <row|z>  Tile scan order when --tile-size is set (default: row)");
    eprintln!("  --level <N>         ZSTD level, 1-22 (default: 19)");
    eprintln!("  --no-zstd           Never use ZSTD; every IDAT is written raw");
    eprintln!();
    eprintln!("v0.1 supports 8-bit PNG input only (gray/gray+alpha/RGB/RGBA).");
}

struct Args {
    input: String,
    output: String,
    options: EncoderOptions,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    if args.len() < 2 {
        return Err("missing <input.png> and/or <output.cafe>".to_string());
    }
    let input = args[0].clone();
    let output = args[1].clone();
    let mut options = EncoderOptions::default();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--tile-size" => {
                let value = args.get(i + 1).ok_or("--tile-size requires a WxH value")?;
                let (w, h) = value.split_once('x').ok_or_else(|| {
                    format!("--tile-size expects WxH (e.g. 64x64), got {value:?}")
                })?;
                let tw: u16 = w.parse().map_err(|_| format!("invalid tile width {w:?}"))?;
                let th: u16 = h
                    .parse()
                    .map_err(|_| format!("invalid tile height {h:?}"))?;
                options.tile_size = Some((tw, th));
                i += 2;
            }
            "--scan-order" => {
                let value = args.get(i + 1).ok_or("--scan-order requires row or z")?;
                options.scan_order = match value.as_str() {
                    "row" => SCAN_ORDER_ROW_MAJOR,
                    "z" => SCAN_ORDER_Z_ORDER,
                    other => {
                        return Err(format!("--scan-order expects 'row' or 'z', got {other:?}"))
                    }
                };
                i += 2;
            }
            "--level" => {
                let value = args.get(i + 1).ok_or("--level requires a number")?;
                options.level = value
                    .parse()
                    .map_err(|_| format!("invalid ZSTD level {value:?}"))?;
                i += 2;
            }
            "--no-zstd" => {
                options.allow_zstd = false;
                i += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(Args {
        input,
        output,
        options,
    })
}

fn run(args: &Args) -> Result<(), String> {
    let img = image::ImageReader::open(&args.input)
        .map_err(|e| format!("failed to open {:?}: {e}", args.input))?
        .decode()
        .map_err(|e| format!("failed to decode {:?}: {e}", args.input))?;

    let pixels = dynamic_image_to_cafe_pixels(&img).map_err(|e| e.to_string())?;

    let buf = encode_bytes(
        pixels.width,
        pixels.height,
        pixels.bit_depth,
        pixels.sample_format,
        pixels.color_type,
        &pixels.raw_pixels,
        args.options,
    )
    .map_err(|e| format!("encode failed: {e}"))?;

    std::fs::write(&args.output, &buf)
        .map_err(|e| format!("failed to write {:?}: {e}", args.output))?;

    eprintln!(
        "wrote {} ({} bytes) from {}x{} input ({} bytes raw)",
        args.output,
        buf.len(),
        pixels.width,
        pixels.height,
        pixels.raw_pixels.len()
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
