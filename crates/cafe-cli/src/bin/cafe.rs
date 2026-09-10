//! `cafe` — unified CLI with subcommands: inspect, benchmark, verify, explain.

use cafe_cli::{read_predictor_codes, walk_chunks};
use cafe_codec::predictor::{
    NUM_PREDICTORS, PREDICTOR_AVERAGE, PREDICTOR_GRADIENT, PREDICTOR_NONE, PREDICTOR_PAETH,
    PREDICTOR_SUB, PREDICTOR_UP,
};
use cafe_codec::zstd_codec::{decompress_chunk, FLAG_RAW, FLAG_ZSTD};
use cafe_format::ihdr::Ihdr;
use cafe_format::{Idim, Plte};
use std::env;
use std::process::ExitCode;

fn usage() {
    eprintln!("Usage: cafe <subcommand> [args]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  inspect <file.cafe>      Print chunk layout and header info");
    eprintln!("  benchmark <corpus-dir>   Compare CAFE vs PNG on a corpus");
    eprintln!("  verify <file.cafe>       Validate a file against the spec invariants");
    eprintln!("  explain <file.cafe>      Human-readable breakdown of encoding decisions");
}

fn predictor_name(code: u8) -> &'static str {
    match code {
        PREDICTOR_NONE => "None",
        PREDICTOR_SUB => "Sub",
        PREDICTOR_UP => "Up",
        PREDICTOR_AVERAGE => "Average",
        PREDICTOR_PAETH => "Paeth",
        PREDICTOR_GRADIENT => "Gradient",
        _ => "?",
    }
}

fn color_type_name(color_type: u8) -> &'static str {
    match color_type {
        cafe_format::constants::COLOR_TYPE_GRAY => "Gray",
        cafe_format::constants::COLOR_TYPE_RGB => "RGB",
        cafe_format::constants::COLOR_TYPE_GRAY_ALPHA => "GrayAlpha",
        cafe_format::constants::COLOR_TYPE_RGBA => "RGBA",
        _ => "Unknown",
    }
}

fn scan_order_name(scan_order: u8) -> &'static str {
    match scan_order {
        cafe_format::constants::SCAN_ORDER_ROW_MAJOR => "row-major",
        cafe_format::constants::SCAN_ORDER_Z_ORDER => "Z-order",
        _ => "unknown",
    }
}

// --- inspect ---------------------------------------------------------

fn cmd_inspect(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("usage: cafe inspect <file.cafe>")?;
    let buf = std::fs::read(path).map_err(|e| format!("failed to read {path:?}: {e}"))?;

    let chunks = walk_chunks(&buf).map_err(|e| format!("failed to parse chunks: {e}"))?;

    println!("{path}: {} bytes, {} chunk(s)", buf.len(), chunks.len());
    println!();
    println!(
        "{:<8} {:>10} {:>6} {:>10}  critical",
        "type", "offset", "flag", "length"
    );
    for chunk in &chunks {
        let critical = if cafe_format::chunk::is_critical(&chunk.chunk_type) {
            "yes"
        } else {
            "no"
        };
        println!(
            "{:<8} {:>10} {:>6} {:>10}  {}",
            chunk.type_str(),
            chunk.offset,
            format!("{:#04x}", chunk.flag),
            chunk.data_len,
            critical
        );
    }

    if let Some(ihdr_chunk) = chunks.iter().find(|c| &c.chunk_type == b"IHDR") {
        match Ihdr::from_payload(&ihdr_chunk.data) {
            Ok(ihdr) => {
                println!();
                println!("IHDR:");
                println!("  width:              {}", ihdr.width);
                println!("  height:             {}", ihdr.height);
                println!("  bit_depth:          {}", ihdr.bit_depth);
                println!(
                    "  sample_format:      {}",
                    if ihdr.sample_format == cafe_format::constants::SAMPLE_FORMAT_FLOAT {
                        "float"
                    } else {
                        "uint"
                    }
                );
                println!(
                    "  color_type:         {} ({})",
                    ihdr.color_type,
                    color_type_name(ihdr.color_type)
                );
                println!(
                    "  compression_method: {:#010b} (zstd capable: {})",
                    ihdr.compression_method,
                    ihdr.compression_method & cafe_format::constants::COMPRESSION_METHOD_ZSTD_BIT
                        != 0
                );
            }
            Err(e) => println!("\nIHDR: failed to parse payload: {e}"),
        }
    }

    if let Some(idim_chunk) = chunks.iter().find(|c| &c.chunk_type == b"iDIM") {
        match Idim::from_payload(&idim_chunk.data) {
            Ok(idim) => {
                println!();
                println!("iDIM:");
                println!("  tile_size:  {}x{}", idim.tile_width, idim.tile_height);
                println!(
                    "  tiles:      {}x{} ({} total)",
                    idim.tiles_x,
                    idim.tiles_y,
                    idim.tile_count()
                );
                println!(
                    "  scan_order: {} ({})",
                    idim.scan_order,
                    scan_order_name(idim.scan_order)
                );
            }
            Err(e) => println!("\niDIM: failed to parse payload: {e}"),
        }
    } else {
        println!();
        println!("iDIM: absent (single implicit whole-image tile)");
    }

    if let Some(plte_chunk) = chunks.iter().find(|c| &c.chunk_type == b"PLTE") {
        let ihdr_for_plte = chunks
            .iter()
            .find(|c| &c.chunk_type == b"IHDR")
            .and_then(|c| Ihdr::from_payload(&c.data).ok());
        println!();
        println!("PLTE:");
        match ihdr_for_plte.and_then(|h| Plte::from_payload(&plte_chunk.data, h.color_type).ok()) {
            Some(plte) => {
                println!("  entries: {}", plte.entry_count());
                println!("  bytes_per_entry: {}", plte.bytes_per_entry);
            }
            None => println!("  failed to parse payload against this file's IHDR"),
        }
    } else {
        println!();
        println!("PLTE: absent (direct, non-indexed pixels)");
    }

    let idat_count = chunks.iter().filter(|c| &c.chunk_type == b"IDAT").count();
    println!();
    println!("IDAT chunk count: {idat_count}");

    Ok(())
}

// --- verify ------------------------------------------------------------

fn cmd_verify(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("usage: cafe verify <file.cafe>")?;
    let buf = std::fs::read(path).map_err(|e| format!("failed to read {path:?}: {e}"))?;

    let mut errors: Vec<String> = Vec::new();

    if let Err(e) = cafe_format::validate_signature(&buf) {
        errors.push(format!("signature: {e}"));
    }

    let chunks = match walk_chunks(&buf) {
        Ok(c) => c,
        Err(e) => {
            errors.push(format!("chunk framing: {e}"));
            Vec::new()
        }
    };

    let mut ihdr: Option<Ihdr> = None;
    if let Some(first) = chunks.first() {
        if &first.chunk_type != b"IHDR" {
            errors.push(format!(
                "first chunk must be IHDR, found {}",
                first.type_str()
            ));
        } else {
            match Ihdr::from_payload(&first.data) {
                Ok(h) => match h.validate() {
                    Ok(()) => ihdr = Some(h),
                    Err(e) => errors.push(format!("IHDR validation: {e}")),
                },
                Err(e) => errors.push(format!("IHDR payload: {e}")),
            }
        }
    } else if errors.is_empty() {
        errors.push("file has no chunks".to_string());
    }

    if let Some(ihdr) = &ihdr {
        if let Some(idim_chunk) = chunks.iter().find(|c| &c.chunk_type == b"iDIM") {
            match Idim::from_payload(&idim_chunk.data) {
                Ok(idim) => {
                    if let Err(e) = idim.validate(ihdr.width, ihdr.height) {
                        errors.push(format!("iDIM validation: {e}"));
                    }
                }
                Err(e) => errors.push(format!("iDIM payload: {e}")),
            }
        }
    }

    let idat_count = chunks.iter().filter(|c| &c.chunk_type == b"IDAT").count();
    if idat_count == 0 && errors.is_empty() {
        errors.push("file has no IDAT chunk".to_string());
    }

    if !chunks.iter().any(|c| &c.chunk_type == b"IEND") {
        errors.push("file has no IEND chunk".to_string());
    }

    // Full decode is the strongest check: exercises everything above plus
    // tile assembly and predictor validity.
    if let Err(e) = cafe_codec::decode_bytes(&buf) {
        errors.push(format!("full decode: {e}"));
    }

    if errors.is_empty() {
        println!("{path}: OK");
        Ok(())
    } else {
        println!("{path}: INVALID");
        for e in &errors {
            println!("  - {e}");
        }
        Err(format!("{} problem(s) found", errors.len()))
    }
}

// --- explain -------------------------------------------------------------

fn cmd_explain(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("usage: cafe explain <file.cafe>")?;
    let buf = std::fs::read(path).map_err(|e| format!("failed to read {path:?}: {e}"))?;

    let chunks = walk_chunks(&buf).map_err(|e| format!("failed to parse chunks: {e}"))?;

    let ihdr_chunk = chunks
        .iter()
        .find(|c| &c.chunk_type == b"IHDR")
        .ok_or("file has no IHDR chunk")?;
    let ihdr = Ihdr::from_payload(&ihdr_chunk.data).map_err(|e| format!("IHDR payload: {e}"))?;
    let direct_bpp = ihdr
        .bytes_per_pixel()
        .ok_or_else(|| format!("unsupported color_type={}", ihdr.color_type))?;

    let idim = chunks
        .iter()
        .find(|c| &c.chunk_type == b"iDIM")
        .map(|c| Idim::from_payload(&c.data))
        .transpose()
        .map_err(|e| format!("iDIM payload: {e}"))?;

    let plte = chunks
        .iter()
        .find(|c| &c.chunk_type == b"PLTE")
        .map(|c| Plte::from_payload(&c.data, ihdr.color_type))
        .transpose()
        .map_err(|e| format!("PLTE payload: {e}"))?;
    // PLTE (spec section 4.3) changes IDAT's effective bpp to 1 (one
    // palette index per pixel), same rule `cafe_codec::decode_bytes` uses.
    let bpp = if plte.is_some() { 1 } else { direct_bpp };

    println!("{path}");
    println!(
        "  {}x{} {} bit_depth={} color_type={}",
        ihdr.width,
        ihdr.height,
        color_type_name(ihdr.color_type),
        ihdr.bit_depth,
        ihdr.color_type
    );
    match &idim {
        Some(idim) => println!(
            "  tiling: {}x{} tiles of {}x{} pixels, {} scan order",
            idim.tiles_x,
            idim.tiles_y,
            idim.tile_width,
            idim.tile_height,
            scan_order_name(idim.scan_order)
        ),
        None => println!("  tiling: single whole-image tile (no iDIM)"),
    }
    match &plte {
        Some(plte) => println!(
            "  palette: {} entries ({} bytes/entry) — indexed pixels",
            plte.entry_count(),
            plte.bytes_per_entry
        ),
        None => println!("  palette: none (direct pixels)"),
    }
    println!();

    let layout = cafe_codec::tiling::TileLayout::new(idim, ihdr.width, ihdr.height)
        .map_err(|e| format!("failed to build tile layout: {e}"))?;

    let mut histogram = [0u64; NUM_PREDICTORS as usize];
    let mut raw_total = 0u64;
    let mut compressed_total = 0u64;
    let mut zstd_tiles = 0u64;
    let mut raw_tiles = 0u64;

    let idat_chunks: Vec<_> = chunks.iter().filter(|c| &c.chunk_type == b"IDAT").collect();
    for (i, chunk) in idat_chunks.iter().enumerate() {
        let (_, _, tile_w, tile_h) = layout.tile_rect(i);
        let bytes_per_row = tile_w * bpp;

        compressed_total += chunk.data_len as u64;
        match chunk.flag {
            FLAG_ZSTD => zstd_tiles += 1,
            FLAG_RAW => raw_tiles += 1,
            _ => {}
        }

        let payload = decompress_chunk(chunk.flag, &chunk.data)
            .map_err(|e| format!("tile {i}: decompression failed: {e}"))?;
        raw_total += payload.len() as u64;

        let codes = read_predictor_codes(&payload, tile_h, bytes_per_row);
        for code in codes {
            if (code as usize) < histogram.len() {
                histogram[code as usize] += 1;
            }
        }
    }

    println!(
        "IDAT chunks: {} ({} raw, {} zstd)",
        idat_chunks.len(),
        raw_tiles,
        zstd_tiles
    );
    println!("  compressed bytes total: {compressed_total}");
    println!("  raw (predicted) bytes total: {raw_total}");
    println!();
    println!("Predictor usage (per row, across all tiles):");
    let total_rows: u64 = histogram.iter().sum();
    for code in 0..NUM_PREDICTORS {
        let count = histogram[code as usize];
        let pct = if total_rows > 0 {
            100.0 * count as f64 / total_rows as f64
        } else {
            0.0
        };
        println!(
            "  {:<10} {:>8} rows ({:>5.1}%)",
            predictor_name(code),
            count,
            pct
        );
    }

    Ok(())
}

// --- benchmark -----------------------------------------------------------

fn cmd_benchmark(args: &[String]) -> Result<(), String> {
    let corpus_dir = match args.first() {
        Some(dir) => std::path::PathBuf::from(dir),
        None => cafe_bench::manifest::default_corpus_dir(),
    };

    let manifest_path = corpus_dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("failed to read {manifest_path:?}: {e}"))?;
    let manifest: cafe_bench::Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid manifest.json: {e}"))?;

    println!(
        "{:<40} {:>10} {:>10} {:>10} {:>10} {:>10}  plte",
        "image", "raw", "png", "cafe", "png %", "cafe %"
    );

    let mut total_raw = 0u64;
    let mut total_png = 0u64;
    let mut total_cafe = 0u64;

    let mut hdr_entries: Vec<&cafe_bench::manifest::ImageEntry> = Vec::new();

    for entry in &manifest.images {
        if entry.format == "exr" {
            // HDR entries have no PNG baseline (see
            // `cafe_bench::measure::HdrMeasurement`'s docs) and print in a
            // separate table below, once every PNG entry has been listed.
            hdr_entries.push(entry);
            continue;
        }

        let image_path = corpus_dir.join(&entry.path);
        let img = image::ImageReader::open(&image_path)
            .map_err(|e| format!("failed to open {image_path:?}: {e}"))?
            .decode()
            .map_err(|e| format!("failed to decode {image_path:?}: {e}"))?
            .to_rgba8();

        let m = cafe_bench::measure(&img).map_err(|e| format!("{}: {e}", entry.path))?;

        println!(
            "{:<40} {:>10} {:>10} {:>10} {:>9.1}% {:>9.1}%  {}",
            entry.path,
            m.raw_bytes,
            m.png_bytes,
            m.cafe_bytes,
            m.png_ratio() * 100.0,
            m.cafe_ratio() * 100.0,
            if m.used_palette { "yes" } else { "" }
        );

        total_raw += m.raw_bytes as u64;
        total_png += m.png_bytes as u64;
        total_cafe += m.cafe_bytes as u64;
    }

    println!();
    println!(
        "TOTAL: raw={total_raw} png={total_png} ({:.1}%) cafe={total_cafe} ({:.1}%)",
        100.0 * total_png as f64 / total_raw as f64,
        100.0 * total_cafe as f64 / total_raw as f64
    );

    if !hdr_entries.is_empty() {
        println!();
        println!("HDR (compared against the original .exr file, not PNG — see AGENTS.md):");
        println!(
            "{:<40} {:>10} {:>10} {:>10} {:>12}",
            "image", "raw", "exr", "cafe", "cafe vs exr %"
        );
        for entry in hdr_entries {
            let image_path = corpus_dir.join(&entry.path);
            let m =
                cafe_bench::measure_hdr(&image_path).map_err(|e| format!("{}: {e}", entry.path))?;
            println!(
                "{:<40} {:>10} {:>10} {:>10} {:>11.1}%",
                entry.path,
                m.raw_bytes,
                m.exr_bytes,
                m.cafe_bytes,
                m.cafe_vs_exr() * 100.0
            );
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(subcommand) = args.get(1) else {
        usage();
        return ExitCode::FAILURE;
    };
    let rest = &args[2..];

    let result = match subcommand.as_str() {
        "inspect" => cmd_inspect(rest),
        "verify" => cmd_verify(rest),
        "explain" => cmd_explain(rest),
        "benchmark" => cmd_benchmark(rest),
        "-h" | "--help" => {
            usage();
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("Unknown subcommand: {other}");
            usage();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
