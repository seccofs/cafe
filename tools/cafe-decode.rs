use std::env;
use std::process::ExitCode;
use std::str::FromStr;

use cafe::{decode_with_opts, EncodeOptions, ToneMapOperator};

fn usage() {
    eprintln!("Usage: cafe-decode <input.cafe> <output> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --extract-metadata           Extract and display all metadata including cHDR");
    eprintln!("  --tonemap-operator <op>      Tone-map operator for HDR images (reinhard|filmic)");
    eprintln!("                               Default: filmic (ACES curve, recommended)");
}

fn main() -> ExitCode {
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

    match run_decode(&args, src, dst) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_decode(args: &[String], src: &str, dst: &str) -> Result<(), Box<dyn std::error::Error>> {
    let extract_metadata = args.iter().any(|a| a == "--extract-metadata");

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
    }

    // XMP metadata info
    if let Some(xmp) = &result.xmp_metadata {
        println!("  XMP metadata found: {} bytes", xmp.len());
    }

    // ZSTD dictionary info
    if let Some(dict) = &result.zstd_dictionary {
        println!("  ZSTD dictionary found: {} bytes", dict.len());
    }

    Ok(())
}
