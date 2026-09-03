use std::collections::HashMap;
use std::env;
use std::process::ExitCode;

use cafe::{cHDR, encode, encode_indexed, EncodeOptions, FilterHeuristic, ToneMapOperator};
use image::ImageReader;

/// Minimal stderr logger so the `cafe` library's `log::warn!`/`info!`/`debug!`
/// diagnostics (ancillary-chunk warnings, encode stats, etc.) remain visible
/// when running this CLI, matching the previous `eprintln!`-based behavior.
/// Kept dependency-free on purpose: pulling in `env_logger` would add ~19
/// transitive crates just to format CLI diagnostics. Verbosity is controlled
/// via the `RUST_LOG` env var (error|warn|info|debug|trace), default `info`.
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
    eprintln!("Usage: cafe-encode <input> <output.cafe> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --no-filter              Disable predictive filter (faster, less compression)");
    eprintln!("  --byte-shuffle           Use byte-shuffle (Filter Method=1): reorders bytes of");
    eprintln!("                           multi-byte samples (bpp 2/4/8) for better compression");
    eprintln!("                           of float/HDR data (v1.1)");
    eprintln!("  --filter-heuristic <h>  Filter selection heuristic per block:");
    eprintln!("                           entropy (default, Shannon entropy),");
    eprintln!("                           msad (PNG classic, very fast),");
    eprintln!("                           test (real ZSTD compression, slow),");
    eprintln!("                           quick-prune (MSAD+entropy, balanced, v1.1),");
    eprintln!("                           adaptive (content-aware, better photos, v1.1)");
    eprintln!("  --level <1-22>           ZSTD compression level (default: 19, range: 1-22)");
    eprintln!("                           1=fast/large, 22=slow/small");
    eprintln!("  --auto-dict              Auto-train ZSTD dictionary from image data (v1.1)");
    eprintln!(
        "                           Useful for small/repetitive images, improves compression"
    );
    eprintln!("  --palette-algorithm <a>  Palette quantization algorithm (v1.1, indexed only):");
    eprintln!("                           nearest (default, fast), median-cut (better quality),");
    eprintln!(
        "                           weighted (v1.5, perceptual/redmean distance, scalar-only),"
    );
    eprintln!(
        "                           kmeans (v1.7, iterative refinement, best quality/slowest)"
    );
    eprintln!("  --color-type <type>      Color type (default: auto-detect):");
    eprintln!("                           0=GRAY (1 byte/px, -75%), 2=RGB (3 bytes/px, -25%)");
    eprintln!("                           4=GRAY_ALPHA (2 bytes/px), 6=RGBA (4 bytes/px)");
    eprintln!("  --bit-depth <d>          Target bit depth for uint (default: 8):");
    eprintln!(
        "                           GRAY/GRAY_ALPHA: 1,2,4,8,10,12,16,32; RGB/RGBA: 8,10,12,16,32"
    );
    eprintln!("  --adaptive               Enable local complexity analysis per tile");
    eprintln!("  --indexed                Encode with indexed palette (few colors: -70-90%)");
    eprintln!("  --json-file <file>       JSON file with metadata");
    eprintln!("  --exif-file <file>       Raw EXIF binary blob");
    eprintln!("  --icc-profile-file <f>   ICC color profile binary blob");
    eprintln!("  --xmp-file <file>        XMP metadata (UTF-8 text/XML)");
    eprintln!();
    eprintln!("  [v1.0 HDR and interlace]");
    eprintln!("  --sample-format <fmt>    Sample format (0=uint, 1=float, 2=half-float)");
    eprintln!("  --chdr-transfer <func>   Transfer function (0=linear, 1=PQ, 2=HLG, 3=sRGB)");
    eprintln!("  --chdr-primaries <prim>  Color primaries (0=sRGB, 1=BT.2020, 2=DCI-P3)");
    eprintln!("  --chdr-max-lum <float>   Max luminance (nits)");
    eprintln!("  --chdr-min-lum <float>   Min luminance (nits)");
    eprintln!("  --chdr-dict-file <path>  ZSTD dictionary file for better compression");
    eprintln!("  --interlace <method>     Interlace method (0=none, 1=Adam7, 2=Even/Odd)");
    eprintln!();
    eprintln!("  [v1.8 inverse tone-mapping]");
    eprintln!("  --inverse-tonemap <op>   Synthesize HDR float data from SDR input (opt-in ITM).");
    eprintln!("                           Only 'reinhard' is supported (no closed-form inverse");
    eprintln!("                           for filmic/ACES). Requires --sample-format 1 and");
    eprintln!("                           --chdr-transfer 0 (linear); --color-type must be RGBA");
    eprintln!("                           (or left unspecified, which defaults to RGBA).");
}

fn main() -> ExitCode {
    init_logger();
    let args: Vec<String> = env::args().collect();

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

    match run_encode(&args, src, dst) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Returns the value following a flag at `pos` (i.e. `args[pos + 1]`), or an
/// error if the flag was the last argument. Used instead of raw `args[pos +
/// 1]` indexing so a trailing flag without its value (e.g. `... --level`)
/// produces a normal `Error: ...` message and exit code 1, instead of a
/// panic (index out of bounds).
fn require_arg_value<'a>(args: &'a [String], pos: usize, flag: &str) -> Result<&'a str, String> {
    args.get(pos + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires an argument"))
}

fn run_encode(args: &[String], src: &str, dst: &str) -> Result<(), Box<dyn std::error::Error>> {
    let use_filter = !args.iter().any(|a| a == "--no-filter");
    let use_byte_shuffle = args.iter().any(|a| a == "--byte-shuffle");
    let adaptive_analysis = args.iter().any(|a| a == "--adaptive");
    let user_specified_indexed = args.iter().any(|a| a == "--indexed");
    let auto_dictionary = args.iter().any(|a| a == "--auto-dict");

    // Parse --palette-algorithm <nearest|median-cut|weighted|kmeans>
    let palette_algorithm = if let Some(pos) = args.iter().position(|a| a == "--palette-algorithm")
    {
        let algo = require_arg_value(args, pos, "--palette-algorithm")?;
        use std::str::FromStr;
        match cafe::PaletteAlgorithm::from_str(algo) {
            Ok(alg) => alg,
            Err(e) => {
                eprintln!("Error: {e}");
                return Err(e.into());
            }
        }
    } else {
        cafe::PaletteAlgorithm::NearestNeighbor
    };

    // Parse --filter-heuristic <entropy|msad|test|quick-prune|adaptive>
    let filter_heuristic = if let Some(pos) = args.iter().position(|a| a == "--filter-heuristic") {
        let h = require_arg_value(args, pos, "--filter-heuristic")?;
        match h {
            "entropy" => FilterHeuristic::Entropy,
            "msad" => FilterHeuristic::Msad,
            "test" => FilterHeuristic::CompressionTest,
            "quick-prune" => FilterHeuristic::QuickPrune,
            "adaptive" => FilterHeuristic::AdaptiveEntropy,
            _ => {
                return Err(format!(
                    "--filter-heuristic: only 'entropy', 'msad', 'test', 'quick-prune', \
                     or 'adaptive' accepted, got: {h}"
                )
                .into())
            }
        }
    } else {
        FilterHeuristic::Entropy
    };

    // Parse --level <1-22>
    let level = if let Some(pos) = args.iter().position(|a| a == "--level") {
        let level_str = require_arg_value(args, pos, "--level")?;
        let level: i32 = level_str.parse().map_err(|_| {
            format!("Error: --level must be an integer between 1 and 22, got: {level_str}")
        })?;
        if !(1..=22).contains(&level) {
            return Err(format!("Error: --level must be between 1 and 22, got: {level}").into());
        }
        level
    } else {
        19 // ZSTD_LEVEL from constants
    };

    // Auto-detect color-type and whether to use indexed
    let (detected_color_type, is_indexed_candidate) = analyze_image(src)?;

    // Parse --color-type <0|2|4|6> (overrides the detected default)
    let target_color_type = if let Some(pos) = args.iter().position(|a| a == "--color-type") {
        let ct_str = require_arg_value(args, pos, "--color-type")?;
        let ct: u8 = ct_str
            .parse()
            .map_err(|_| format!("Error: --color-type must be 0, 2, 4, or 6, got: {ct_str}"))?;
        if ![0, 2, 4, 6].contains(&ct) {
            return Err(format!("Error: --color-type must be 0, 2, 4, or 6, got: {ct}").into());
        }
        ct
    } else {
        detected_color_type // Use the auto-detected type
    };

    // Parse --bit-depth <d> (uint only; float/half fix at 32/16)
    let target_bit_depth = if let Some(pos) = args.iter().position(|a| a == "--bit-depth") {
        let bd_str = require_arg_value(args, pos, "--bit-depth")?;
        let bd: u8 = bd_str
            .parse()
            .map_err(|_| format!("Error: --bit-depth must be numeric, got: {bd_str}"))?;
        if ![1, 2, 4, 8, 10, 12, 16, 32].contains(&bd) {
            return Err(format!(
                "Error: --bit-depth must be 1, 2, 4, 8, 10, 12, 16, or 32, got: {bd}"
            )
            .into());
        }
        Some(bd)
    } else {
        None
    };

    let json_metadata: HashMap<String, serde_json::Value> =
        if let Some(pos) = args.iter().position(|a| a == "--json-file") {
            let path = require_arg_value(args, pos, "--json-file")?;
            let text = std::fs::read_to_string(path)?;
            serde_json::from_str(&text)?
        } else {
            HashMap::new()
        };

    let exif = if let Some(pos) = args.iter().position(|a| a == "--exif-file") {
        let path = require_arg_value(args, pos, "--exif-file")?;
        Some(std::fs::read(path)?)
    } else {
        None
    };

    let icc_profile = if let Some(pos) = args.iter().position(|a| a == "--icc-profile-file") {
        let path = require_arg_value(args, pos, "--icc-profile-file")?;
        Some(std::fs::read(path).map_err(|e| format!("Error reading ICC profile file: {e}"))?)
    } else {
        None
    };

    let xmp_metadata = if let Some(pos) = args.iter().position(|a| a == "--xmp-file") {
        let path = require_arg_value(args, pos, "--xmp-file")?;
        Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("Error reading XMP file as UTF-8 text: {e}"))?,
        )
    } else {
        None
    };

    // --- Parse v1.0 features ---

    // Sample format (uint/float/half)
    let sample_format = if let Some(pos) = args.iter().position(|a| a == "--sample-format") {
        let fmt_str = require_arg_value(args, pos, "--sample-format")?;
        let fmt: u8 = fmt_str.parse().map_err(|_| {
            format!("--sample-format must be 0(uint), 1(float) or 2(half), got: {fmt_str}")
        })?;
        if ![0, 1, 2].contains(&fmt) {
            return Err("--sample-format: only 0, 1 or 2 supported".into());
        }
        Some(fmt)
    } else {
        None
    };

    // cHDR metadata
    let chdr = if args.iter().any(|a| a.starts_with("--chdr-")) {
        let transfer_function = if let Some(pos) = args.iter().position(|a| a == "--chdr-transfer")
        {
            require_arg_value(args, pos, "--chdr-transfer")?
                .parse()
                .map_err(|_| "Error parsing --chdr-transfer as a number between 0-3".to_string())?
        } else {
            3 // sRGB default
        };

        let color_primaries = if let Some(pos) = args.iter().position(|a| a == "--chdr-primaries") {
            require_arg_value(args, pos, "--chdr-primaries")?
                .parse()
                .map_err(|_| "Error parsing --chdr-primaries as a number between 0-2".to_string())?
        } else {
            0 // sRGB default
        };

        let max_luminance = if let Some(pos) = args.iter().position(|a| a == "--chdr-max-lum") {
            require_arg_value(args, pos, "--chdr-max-lum")?
                .parse()
                .map_err(|_| "Error parsing --chdr-max-lum as a float".to_string())?
        } else {
            1.0
        };

        let min_luminance = if let Some(pos) = args.iter().position(|a| a == "--chdr-min-lum") {
            require_arg_value(args, pos, "--chdr-min-lum")?
                .parse()
                .map_err(|_| "Error parsing --chdr-min-lum as a float".to_string())?
        } else {
            0.0
        };

        Some(cHDR {
            transfer_function,
            color_primaries,
            max_luminance,
            min_luminance,
            max_cll: None,
            max_fall: None,
        })
    } else {
        None
    };

    // ZSTD dictionary
    let zstd_dictionary = if let Some(pos) = args.iter().position(|a| a == "--chdr-dict-file") {
        let path = require_arg_value(args, pos, "--chdr-dict-file")?;
        Some(std::fs::read(path).map_err(|e| format!("Error reading ZSTD dictionary file: {e}"))?)
    } else {
        None
    };

    // Inverse tone-mapping (v1.8, opt-in ITM: synthesize HDR float data from
    // SDR input). Only 'reinhard' has a closed-form inverse; validated more
    // thoroughly (sample_format/chdr_metadata/color_type combinations) by
    // `encode()` itself, so this parsing step only needs to parse the
    // operator name.
    let inverse_tonemap = if let Some(pos) = args.iter().position(|a| a == "--inverse-tonemap") {
        let op_str = require_arg_value(args, pos, "--inverse-tonemap")?;
        use std::str::FromStr;
        match ToneMapOperator::from_str(op_str) {
            Ok(ToneMapOperator::Filmic) => {
                return Err(
                    "--inverse-tonemap: 'filmic' has no closed-form inverse tone-map operator; \
                     use 'reinhard'"
                        .into(),
                );
            }
            Ok(op) => Some(op),
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };

    // `encode_indexed()` has no HDR path at all (see AGENTS.md): it never
    // reads `sample_format`/`chdr_metadata`/`inverse_tonemap`, always
    // producing uint8 indexed output. Silently routing to it whenever the
    // few-colors auto-detection fires would drop `--sample-format`,
    // `--chdr-*`, and `--inverse-tonemap` on the floor with no warning —
    // dangerous in particular for `--inverse-tonemap`, whose whole point is
    // producing HDR float output. `--indexed` explicitly combined with any
    // of those is a contradiction in terms and rejected outright; the
    // *auto-detected* few-colors path is instead skipped (falls through to
    // the normal `encode()` call below) so HDR/float flags stay honored.
    let hdr_incompatible_with_indexed =
        sample_format.is_some() || chdr.is_some() || inverse_tonemap.is_some();
    if user_specified_indexed && hdr_incompatible_with_indexed {
        return Err(
            "--indexed is incompatible with --sample-format/--chdr-*/--inverse-tonemap \
             (encode_indexed() has no HDR/float path — see AGENTS.md)"
                .into(),
        );
    }
    let use_indexed =
        user_specified_indexed || (is_indexed_candidate && !hdr_incompatible_with_indexed);

    // Interlace method
    let interlace_method = if let Some(pos) = args.iter().position(|a| a == "--interlace") {
        let method_str = require_arg_value(args, pos, "--interlace")?;
        let method: u8 = method_str.parse().map_err(|_| {
            format!("--interlace must be 0(none), 1(Adam7) or 2(Even/Odd), got: {method_str}")
        })?;
        if ![0, 1, 2].contains(&method) {
            return Err("--interlace: only 0, 1 or 2 supported".into());
        }
        method
    } else {
        0 // INTERLACE_NONE
    };

    let mut opts = EncodeOptions {
        use_filter,
        use_byte_shuffle,
        level,
        adaptive_analysis,
        json_metadata,
        exif,
        icc_profile: icc_profile.clone(),
        xmp_metadata: xmp_metadata.clone(),
        sample_format,
        chdr_metadata: chdr.clone(),
        zstd_dictionary: zstd_dictionary.clone(),
        interlace_method,
        filter_heuristic,
        auto_dictionary,
        palette_algorithm,
        inverse_tonemap,
        ..EncodeOptions::default()
    };
    opts.target_color_type = target_color_type;
    opts.target_bit_depth = target_bit_depth;

    let color_type_name = match target_color_type {
        0 => "GRAY",
        2 => "RGB",
        4 => "GRAY_ALPHA",
        6 => "RGBA",
        _ => "DESCONHECIDO",
    };
    let auto_detected = detected_color_type == target_color_type;
    let color_type_info = if auto_detected {
        format!("{color_type_name} (detectado automaticamente)")
    } else {
        color_type_name.to_string()
    };

    if use_indexed {
        encode_indexed(src, dst, &opts)?;
        let filter_status = if use_filter { "sim" } else { "não" };
        let indexed_reason = if is_indexed_candidate && !user_specified_indexed {
            " (detectado: poucas cores)"
        } else {
            ""
        };
        println!("Codificado (INDEXED{indexed_reason}): {src} -> {dst}");
        println!("  Filtro preditivo: {filter_status}");
        println!(
            "  Heurística de filtro: {}",
            heuristic_name(filter_heuristic)
        );
        println!("  Nível ZSTD: {level}/22");
        println!("  Color type: {color_type_info}");
    } else {
        encode(src, dst, &opts)?;
        let filter_status = if use_filter { "sim" } else { "não" };
        let shuffle_status = if use_byte_shuffle { "sim" } else { "não" };
        let adaptive_status = if adaptive_analysis { "sim" } else { "não" };
        println!("Codificado: {src} -> {dst}");
        println!("  Filtro preditivo: {filter_status}");
        println!("  Byte-shuffle: {shuffle_status}");
        println!(
            "  Heurística de filtro: {}",
            heuristic_name(filter_heuristic)
        );
        println!("  Nível ZSTD: {level}/22");
        println!("  Color type: {color_type_info}");
        println!("  Análise adaptativa: {adaptive_status}");
    }
    if !opts.json_metadata.is_empty() {
        let keys: Vec<&str> = opts.json_metadata.keys().map(String::as_str).collect();
        println!("  Metadados jSON gravados: {keys:?}");
    }

    // Print v1.0 options if provided
    if let Some(fmt) = sample_format {
        let fmt_name = match fmt {
            0 => "uint",
            1 => "float",
            2 => "half-float",
            _ => "unknown",
        };
        println!("  Sample format: {fmt_name}");
    }

    if let Some(ref chdr) = chdr {
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
    }

    if zstd_dictionary.is_some() {
        println!("  ZSTD dictionary: included");
    }

    if let Some(ref icc) = icc_profile {
        println!("  ICC profile: {} bytes", icc.len());
    }

    if let Some(ref xmp) = xmp_metadata {
        println!("  XMP metadata: {} bytes", xmp.len());
    }

    if let Some(op) = inverse_tonemap {
        let op_name = match op {
            ToneMapOperator::Reinhard => "reinhard",
            ToneMapOperator::Filmic => "filmic", // unreachable: rejected during parsing above
        };
        println!("  Inverse tone-mapping (ITM): {op_name}");
    }

    if interlace_method > 0 {
        let method_name = match interlace_method {
            1 => "Adam7",
            2 => "Even/Odd",
            _ => "unknown",
        };
        println!("  Interlace method: {method_name}");
    }

    Ok(())
}

fn heuristic_name(h: FilterHeuristic) -> &'static str {
    match h {
        FilterHeuristic::Entropy => "entropy (Shannon Entropy)",
        FilterHeuristic::Msad => "msad (sum of absolute residuals)",
        FilterHeuristic::CompressionTest => "test (real compression test)",
        FilterHeuristic::QuickPrune => "quick-prune (fast MSAD + Entropy on top 8, v1.1)",
        FilterHeuristic::AdaptiveEntropy => {
            "adaptive (block type analysis + adaptive Entropy, v1.1)"
        }
    }
}

/// Analyzes the input image and detects:
/// - Whether it has an alpha channel (to decide GRAY vs GRAY_ALPHA or RGB vs RGBA)
/// - Whether it has few unique colors (to decide whether to use indexed)
/// - The ideal color type to minimize size
fn analyze_image(src: &str) -> Result<(u8, bool), Box<dyn std::error::Error>> {
    let img = ImageReader::open(src)?
        .decode()
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("Erro ao decodificar imagem: {e}").into()
        })?;

    let has_alpha = img.color().has_alpha();
    let _width = img.width();
    let _height = img.height();

    // Detect whether there are few colors (candidate for indexed)
    // Uses a heuristic: if fewer than 256 unique colors, use indexed
    let is_indexed_candidate = {
        // Fast sampling: checks every 10th pixel
        let mut unique_colors = std::collections::HashSet::new();
        let rgba = img.to_rgba8();

        for pixel in rgba.chunks(4).step_by(10) {
            if pixel.len() >= 4 {
                let color = (pixel[0], pixel[1], pixel[2], pixel[3]);
                unique_colors.insert(color);
                if unique_colors.len() > 256 {
                    break; // More than 256 colors, not indexed
                }
            }
        }
        unique_colors.len() <= 256
    };

    // Detect whether it is grayscale (all pixels have R=G=B)
    let is_grayscale = {
        let rgba = img.to_rgba8();
        rgba.chunks(4)
            .step_by(100)
            .all(|pixel| pixel.len() >= 3 && pixel[0] == pixel[1] && pixel[1] == pixel[2])
    };

    // Determine the ideal color type
    let color_type = if is_grayscale {
        if has_alpha {
            4 // GRAY_ALPHA (-50% vs RGBA)
        } else {
            0 // GRAY (-75% vs RGBA)
        }
    } else {
        if has_alpha {
            6 // RGBA (default)
        } else {
            2 // RGB (-25% vs RGBA)
        }
    };

    Ok((color_type, is_indexed_candidate))
}
