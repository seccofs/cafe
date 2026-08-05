//! Basic example of encoding with CAFE
//!
//! Demonstrates how to use the CAFE library to encode a PNG image to CAFE.
//!
//! Usage:
//! ```bash
//! cargo run --example basic_encode -- input.png output.cafe
//! ```

use cafe::{encode, EncodeOptions};
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Uso: {} <input.png> <output.cafe>", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("📦 Encodando {} → {}", input_path, output_path);

    // Configure encoding options
    let opts = EncodeOptions {
        use_filter: true,        // Use predictive filters
        level: 19,               // ZSTD compression level (1-22)
        adaptive_analysis: true, // Adaptive complexity analysis
        target_color_type: 6,    // 6 = RGBA (default)
        ..Default::default()
    };

    // Encode
    match encode(input_path, output_path, &opts) {
        Ok(()) => {
            println!("✅ Sucesso! Arquivo salvo em: {}", output_path);

            // Show size
            let metadata = std::fs::metadata(output_path)?;
            println!("   Tamanho: {} bytes", metadata.len());
        }
        Err(e) => {
            eprintln!("❌ Erro ao encodar: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
