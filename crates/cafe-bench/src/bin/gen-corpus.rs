//! `gen-corpus` — (re)generates the synthetic categories of `corpus/`
//! (`gradient`/`pixelart`/`texture`/`synthetic`) and `corpus/manifest.json`.
//!
//! Per `AGENTS.md` phase 2: real-content categories (`photo`/`screenshot`/
//! `illustration`/`lineart`/`hdr`) are out of scope here and stay as
//! `.gitkeep`-only placeholders.
//!
//! Usage:
//! ```text
//! cargo run -p cafe-bench --bin gen-corpus [output-dir]
//! ```
//! Defaults to the workspace root's `corpus/` directory when no argument is
//! given.

use cafe_bench::manifest::{default_corpus_dir, generate_corpus};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let dir = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_corpus_dir);

    println!("Generating synthetic corpus into {}...", dir.display());
    match generate_corpus(&dir) {
        Ok(manifest) => {
            println!(
                "Wrote {} images and {}/manifest.json",
                manifest.images.len(),
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gen-corpus failed: {e}");
            ExitCode::FAILURE
        }
    }
}
