//! Streaming decode example with CAFE
//!
//! Demonstrates the `Decoder<R: Read>` API (v1.5+): decodes a CAFE file
//! one tile at a time instead of buffering the whole compressed file and
//! the whole decoded image in memory up front, the way `decode`/
//! `decode_bytes` do.
//!
//! This is useful for large images or memory-constrained environments,
//! or when pixels should be consumed/streamed out (e.g. re-encoded,
//! displayed progressively) as soon as each tile is available, rather
//! than waiting for the entire image to finish decoding.
//!
//! # Limitations
//! `Decoder::next_tile()` only supports the default row-strip tiling
//! (no `iDIM` 2D tiling, no Adam7/even-odd interlacing) — check
//! `DecodeInfo::supports_streaming_tiles` before looping, and fall back
//! to `decode`/`decode_bytes` if it's `false`.
//!
//! Usage:
//! ```bash
//! cargo run --example streaming_decode -- input.cafe
//! ```

use cafe::Decoder;
use std::env;
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <input.cafe>", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    println!("📖 Streaming decode of {}", input_path);

    let file = BufReader::new(File::open(input_path)?);
    let mut decoder = Decoder::new(file);

    // read_info() reads the signature plus every chunk up to (but not
    // including) the first IDAT: IHDR, and any of iDIM/cHDR/eXIF/jSON/
    // iCCP/xMPd/zDIC/PLTE that are present.
    let info = decoder.read_info()?;
    println!(
        "   {}x{}, color_type={}, bit_depth={}, sample_format={}",
        info.width, info.height, info.color_type, info.bit_depth, info.sample_format
    );

    if !info.supports_streaming_tiles {
        eprintln!(
            "⚠️  This file uses 2D tiling (iDIM) or interlacing, which next_tile() does not \
             support yet — falling back to decode_bytes() instead."
        );
        let buf = std::fs::read(input_path)?;
        let (pixels, result) = cafe::decode_bytes(&buf)?;
        println!(
            "✅ Whole-image decode: {} bytes of RGBA pixels ({}x{})",
            pixels.len(),
            result.width,
            result.height
        );
        return Ok(());
    }

    // Stream tiles one at a time. Each `tile.pixels` is already-converted
    // RGBA data (tile.width * tile.height * 4 bytes) — no further
    // processing needed regardless of the file's original color type,
    // bit depth, or sample format.
    let mut tile_count = 0usize;
    let mut rows_decoded = 0u32;
    while let Some(tile) = decoder.next_tile()? {
        tile_count += 1;
        rows_decoded += tile.height;
        println!(
            "   tile {}: y={}, {}x{} ({} bytes RGBA)",
            tile_count,
            tile.y,
            tile.width,
            tile.height,
            tile.pixels.len()
        );
        // A real application would consume `tile.pixels` here (write to a
        // framebuffer, re-encode incrementally, display progressively,
        // etc.) instead of just printing its size.
    }

    // finish() returns the same ancillary metadata (EXIF, JSON, ICC, XMP,
    // HDR) that decode()/decode_bytes() return in their DecodeResult.
    let result = decoder.finish()?;
    println!(
        "✅ Done: {} tiles, {} rows decoded (expected {})",
        tile_count, rows_decoded, result.height
    );
    if let Some(exif) = &result.exif {
        println!("   EXIF: {} bytes", exif.len());
    }
    if !result.json_metadata.is_empty() {
        println!("   JSON namespaces: {}", result.json_metadata.len());
    }

    Ok(())
}
