//! `cafe-encode` — encoder CLI, legacy-compatible binary name.
//!
//! Scope (`AGENTS.md` Phase 9, extended by the HDR-CLI-support and
//! 16-bit-CLI-support follow-ups): reads a uint8/uint16 PNG (via
//! `png_io`) or a float32 HDR image such as `.exr` (via `hdr_io`) through
//! the `image` crate, writes a `.cafe` file via `cafe_codec::
//! encode_bytes`. Which bridge applies is decided by the *decoded* color
//! type (`Rgb32F`/`Rgba32F` -> `hdr_io`, anything else -> `png_io`), not
//! by file extension - this matches how `image::ImageReader::decode()`
//! already dispatches internally; `png_io` itself further dispatches
//! uint8 vs uint16 the same way. Tiling and ZSTD level are exposed as
//! flags; predictor selection is not (it's always the per-row entropy
//! heuristic, an encoder-internal decision the spec never surfaces to
//! callers).

use cafe_cli::{dynamic_image_to_cafe_pixels, dynamic_image_to_cafe_pixels_hdr, CafePixels};
use cafe_codec::palette::build_palette;
use cafe_codec::{encode_bytes, EncoderOptions};
use cafe_format::constants::{
    COLOR_TYPE_RGB, COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT, SCAN_ORDER_ROW_MAJOR, SCAN_ORDER_Z_ORDER,
};
use image::ColorType;
use std::env;
use std::process::ExitCode;

fn usage() {
    eprintln!("Usage: cafe-encode <input> <output.cafe> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!(
        "  --tile-size <WxH>   Split into WxH tiles with an iDIM chunk (default: single tile)"
    );
    eprintln!("  --scan-order <row|z>  Tile scan order when --tile-size is set (default: row)");
    eprintln!("  --level <N>         ZSTD level, 1-22 (default: 19)");
    eprintln!("  --no-zstd           Never use ZSTD; every IDAT is written raw");
    eprintln!("  --no-palette        Never emit a PLTE chunk; always encode direct pixels");
    eprintln!();
    eprintln!(
        "Supports 8-bit and 16-bit PNG input (gray/gray+alpha/RGB/RGBA) and float32 HDR \
         input such as .exr (RGB/RGBA)."
    );
    eprintln!(
        "For 8-bit RGB/RGBA input, an indexed-color (PLTE) encode is tried automatically \
         alongside the direct encode whenever the image has at most 256 distinct exact \
         colors, and whichever produces the smaller file is kept (see AGENTS.md's Palette \
         (0.3) phase). PLTE is undefined for 16-bit/float input (spec section 4.3), so this \
         race is skipped for those."
    );
}

/// Picks the PNG or HDR bridge based on the *decoded* color type, not the
/// input file's extension — mirrors how `image::ImageReader::decode()`
/// already dispatches to its OpenEXR codec internally for `.exr` sources.
fn image_to_cafe_pixels(img: &image::DynamicImage) -> Result<CafePixels, String> {
    match img.color() {
        ColorType::Rgb32F | ColorType::Rgba32F => {
            dynamic_image_to_cafe_pixels_hdr(img).map_err(|e| e.to_string())
        }
        _ => dynamic_image_to_cafe_pixels(img).map_err(|e| e.to_string()),
    }
}

struct Args {
    input: String,
    output: String,
    options: EncoderOptions,
    try_palette: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    if args.len() < 2 {
        return Err("missing <input.png> and/or <output.cafe>".to_string());
    }
    let input = args[0].clone();
    let output = args[1].clone();
    let mut options = EncoderOptions::default();
    let mut try_palette = true;

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
            "--no-palette" => {
                try_palette = false;
                i += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(Args {
        input,
        output,
        options,
        try_palette,
    })
}

/// Number of direct color channels PLTE (spec section 4.3) can index for
/// this `color_type`, or `None` if palette encoding doesn't apply
/// (anything other than 8-bit uint RGB/RGBA — PLTE is undefined for
/// gray/gray+alpha and non-8-bit depths, per `Plte::validate`).
fn palette_channels(pixels: &CafePixels) -> Option<u8> {
    if pixels.bit_depth != 8 || pixels.sample_format != SAMPLE_FORMAT_UINT {
        return None;
    }
    match pixels.color_type {
        COLOR_TYPE_RGB => Some(3),
        COLOR_TYPE_RGBA => Some(4),
        _ => None,
    }
}

fn run(args: &Args) -> Result<(), String> {
    let img = image::ImageReader::open(&args.input)
        .map_err(|e| format!("failed to open {:?}: {e}", args.input))?
        .decode()
        .map_err(|e| format!("failed to decode {:?}: {e}", args.input))?;

    let pixels = image_to_cafe_pixels(&img)?;

    let direct_buf = encode_bytes(
        pixels.width,
        pixels.height,
        pixels.bit_depth,
        pixels.sample_format,
        pixels.color_type,
        &pixels.raw_pixels,
        args.options.clone(),
    )
    .map_err(|e| format!("encode failed: {e}"))?;

    // Race a palette (indexed-color) encode against the direct encode
    // whenever it applies (spec section 4.3), keeping whichever is
    // smaller — mirrors the existing raw-vs-ZSTD-per-tile fallback race,
    // just one layer up. `build_palette` returning `None` (too many
    // distinct colors) is an ordinary outcome, not an error: the direct
    // encode above is always a valid result on its own.
    let mut buf = direct_buf;
    let mut used_palette = false;
    if args.try_palette {
        if let Some(channels) = palette_channels(&pixels) {
            if let Some(built) = build_palette(&pixels.raw_pixels, channels) {
                let mut palette_options = args.options.clone();
                palette_options.palette = Some(built.entries);
                if let Ok(palette_buf) = encode_bytes(
                    pixels.width,
                    pixels.height,
                    pixels.bit_depth,
                    pixels.sample_format,
                    pixels.color_type,
                    &built.indices,
                    palette_options,
                ) {
                    if palette_buf.len() < buf.len() {
                        buf = palette_buf;
                        used_palette = true;
                    }
                }
            }
        }
    }

    std::fs::write(&args.output, &buf)
        .map_err(|e| format!("failed to write {:?}: {e}", args.output))?;

    eprintln!(
        "wrote {} ({} bytes{}) from {}x{} input ({} bytes raw)",
        args.output,
        buf.len(),
        if used_palette {
            ", indexed via PLTE"
        } else {
            ""
        },
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
