use std::env;
use std::process::ExitCode;
use std::str::FromStr;

use cafe::{decode_with_opts, EncodeOptions, ToneMapOperator};

/// Minimal stderr logger so the `cafe` library's `log::warn!` diagnostics
/// (e.g. "invalid iCCP chunk, discarded") remain visible when running this
/// CLI. See the identical logger in `cafe-encode.rs` for rationale on why
/// this is hand-rolled instead of pulling in `env_logger`.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn init_logger() {
    static LOGGER: StderrLogger = StderrLogger;
    let level = env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    log::set_max_level(level);
    let _ = log::set_logger(&LOGGER);
}

fn usage() {
    eprintln!("Usage: cafe-decode <input.cafe> <output> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --extract-metadata           Extract and display all metadata including cHDR");
    eprintln!("  --tonemap-operator <op>      Tone-map operator for HDR images (reinhard|filmic)");
    eprintln!("                               Default: filmic (ACES curve, recommended)");
    eprintln!("  --save-exif <path>           Save raw EXIF blob to a separate file, if present");
    eprintln!("  --save-icc-profile <path>    Save raw ICC profile to a separate file, if present");
    eprintln!(
        "  --save-xmp <path>            Save XMP metadata (UTF-8 text) to a file, if present"
    );
    eprintln!(
        "  --save-zstd-dict <path>      Save the embedded ZSTD dictionary to a file, if present"
    );
    eprintln!(
        "  --show-stats                 Print per-chunk compression statistics, if available"
    );
    eprintln!();
    eprintln!("  -h, --help                   Show this help message");
    eprintln!("  -V, --version                Show implementation and CAFE format version");
}

/// See the identical helper in `cafe-encode.rs` for rationale on why the
/// implementation (crate) version and the CAFE *format* version are two
/// separate, independently-changing numbers.
fn print_version() {
    println!("cafe-decode {}", env!("CARGO_PKG_VERSION"));
    println!(
        "CAFE format {}.{}",
        cafe::FORMAT_VERSION_MAJOR,
        cafe::FORMAT_VERSION_MINOR
    );
}

fn main() -> ExitCode {
    init_logger();
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        print_version();
        return ExitCode::SUCCESS;
    }

    if args.len() < 3 {
        usage();
        return ExitCode::FAILURE;
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return ExitCode::SUCCESS;
    }

    let src = &args[1];
    let dst = &args[2];

    match run_decode(&args, src, dst) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Returns the value following a flag at `pos` (i.e. `args[pos + 1]`), or an
/// error if the flag was the last argument. See the identical helper in
/// `cafe-encode.rs` for rationale (avoids a raw-index panic).
fn require_arg_value<'a>(args: &'a [String], pos: usize, flag: &str) -> Result<&'a str, String> {
    args.get(pos + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires an argument"))
}

fn run_decode(args: &[String], src: &str, dst: &str) -> Result<(), Box<dyn std::error::Error>> {
    let extract_metadata = args.iter().any(|a| a == "--extract-metadata");
    let show_stats = args.iter().any(|a| a == "--show-stats");

    let save_exif_path = if let Some(pos) = args.iter().position(|a| a == "--save-exif") {
        Some(require_arg_value(args, pos, "--save-exif")?.to_string())
    } else {
        None
    };
    let save_icc_path = if let Some(pos) = args.iter().position(|a| a == "--save-icc-profile") {
        Some(require_arg_value(args, pos, "--save-icc-profile")?.to_string())
    } else {
        None
    };
    let save_xmp_path = if let Some(pos) = args.iter().position(|a| a == "--save-xmp") {
        Some(require_arg_value(args, pos, "--save-xmp")?.to_string())
    } else {
        None
    };
    let save_dict_path = if let Some(pos) = args.iter().position(|a| a == "--save-zstd-dict") {
        Some(require_arg_value(args, pos, "--save-zstd-dict")?.to_string())
    } else {
        None
    };

    // Parse tone-map operator option
    let tonemap_operator = if let Some(idx) = args.iter().position(|a| a == "--tonemap-operator") {
        if idx + 1 < args.len() {
            match ToneMapOperator::from_str(&args[idx + 1]) {
                Ok(op) => op,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return Err(e.into());
                }
            }
        } else {
            eprintln!("Error: --tonemap-operator requires an argument (reinhard|filmic)");
            return Err("missing tone-map operator argument".into());
        }
    } else {
        ToneMapOperator::Filmic
    };

    // Decode with custom options
    let opts = EncodeOptions {
        tonemap_operator,
        ..Default::default()
    };
    let result = decode_with_opts(src, dst, &opts)?;
    println!("Decoded: {src} -> {dst}");

    if let Some(exif) = &result.exif {
        println!("  EXIF found: {} bytes", exif.len());
        if let Some(path) = &save_exif_path {
            std::fs::write(path, exif)?;
            println!("    Saved to: {path}");
        }
    } else if save_exif_path.is_some() {
        println!("  --save-exif requested, but no EXIF chunk was found");
    }

    if !result.json_metadata.is_empty() {
        let keys: Vec<&str> = result.json_metadata.keys().map(String::as_str).collect();
        println!("  jSON metadata found: {keys:?}");
        if extract_metadata {
            for (ns, obj) in &result.json_metadata {
                println!("    [{ns}] {obj}");
            }
        }
    }

    // Extract cHDR metadata if present
    if let Some(chdr) = &result.chdr_metadata {
        println!("  cHDR found:");
        let tf_name = match chdr.transfer_function {
            0 => "linear",
            1 => "PQ (Perceptual Quantizer)",
            2 => "HLG (Hybrid Log-Gamma)",
            3 => "sRGB/gamma",
            _ => "unknown",
        };
        let prim_name = match chdr.color_primaries {
            0 => "sRGB/BT.709",
            1 => "BT.2020",
            2 => "DCI-P3",
            _ => "unknown",
        };
        println!("    Transfer function: {tf_name}");
        println!("    Color primaries: {prim_name}");
        println!("    Max luminance: {} nits", chdr.max_luminance);
        println!("    Min luminance: {} nits", chdr.min_luminance);
        if let Some(max_cll) = chdr.max_cll {
            println!("    Max CLL: {} nits", max_cll);
        }
        if let Some(max_fall) = chdr.max_fall {
            println!("    Max FALL: {} nits", max_fall);
        }
    }

    // Sample format info if present
    if extract_metadata {
        // This would be extracted if available in DecodeResult
        // Currently not in the struct, but we're keeping the logic here for completeness
    }

    // ICC Profile info
    if let Some(icc) = &result.icc_profile {
        println!("  ICC Profile found: {} bytes", icc.len());
        if let Some(path) = &save_icc_path {
            std::fs::write(path, icc)?;
            println!("    Saved to: {path}");
        }
    } else if save_icc_path.is_some() {
        println!("  --save-icc-profile requested, but no iCCP chunk was found");
    }

    // XMP metadata info
    if let Some(xmp) = &result.xmp_metadata {
        println!("  XMP metadata found: {} bytes", xmp.len());
        if let Some(path) = &save_xmp_path {
            std::fs::write(path, xmp)?;
            println!("    Saved to: {path}");
        }
    } else if save_xmp_path.is_some() {
        println!("  --save-xmp requested, but no xMPd chunk was found");
    }

    // ZSTD dictionary info
    if let Some(dict) = &result.zstd_dictionary {
        println!("  ZSTD dictionary found: {} bytes", dict.len());
        if let Some(path) = &save_dict_path {
            std::fs::write(path, dict)?;
            println!("    Saved to: {path}");
        }
    } else if save_dict_path.is_some() {
        println!("  --save-zstd-dict requested, but no zDIC chunk was found");
    }

    // Compression statistics, if the library computed them
    if show_stats {
        match &result.compression_stats {
            Some(stats) => {
                println!("  Compression stats:");
                println!("    Total original:   {} bytes", stats.total_original);
                println!("    Total compressed: {} bytes", stats.total_compressed);
                if stats.total_original > 0 {
                    let ratio = stats.total_compressed as f64 / stats.total_original as f64;
                    println!(
                        "    Ratio:            {:.4} ({:.1}% of original)",
                        ratio,
                        ratio * 100.0
                    );
                }
                for chunk in &stats.chunks {
                    println!(
                        "    [{}] {} -> {} bytes",
                        chunk.chunk_type, chunk.original_size, chunk.compressed_size
                    );
                }
            }
            None => {
                // Practically unreachable: every valid CAFE file has at
                // least one IDAT, and compression_stats is only None when
                // zero chunks were recorded (see DecodeResult::compression_stats
                // doc comment). Kept as a graceful message instead of an
                // unwrap() in case a future minimal/empty-body test file
                // ever reaches this path.
                println!("  --show-stats requested, but no chunk statistics were recorded");
            }
        }
    }

    Ok(())
}
