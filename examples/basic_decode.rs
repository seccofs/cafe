//! Basic example of decoding with CAFE
//!
//! Demonstrates how to use the CAFE library to decode a CAFE file to PNG.
//!
//! Usage:
//! ```bash
//! cargo run --example basic_decode -- input.cafe output.png
//! ```

use cafe::decode;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <input.cafe> <output.png>", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("📖 Decoding {} → {}", input_path, output_path);

    // Decode
    match decode(input_path, output_path) {
        Ok(result) => {
            println!("✅ Success!");

            // Show metadata if present
            if let Some(exif) = &result.exif {
                println!("   EXIF: {} bytes", exif.len());
            }

            if !result.json_metadata.is_empty() {
                println!("   JSON namespaces: {}", result.json_metadata.len());
                for ns in result.json_metadata.keys() {
                    println!("     - {}", ns);
                }
            }

            if let Some(stats) = &result.compression_stats {
                println!("   Total original: {} bytes", stats.total_original);
                println!("   Total compressed: {} bytes", stats.total_compressed);
            }
        }
        Err(e) => {
            eprintln!("❌ Error decoding: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
