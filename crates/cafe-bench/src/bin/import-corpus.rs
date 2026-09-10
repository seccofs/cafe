//! `import-corpus` — imports staged real-content source images into
//! `corpus/{photo,screenshot,illustration,lineart}/` and merges them into
//! `corpus/manifest.json`, per `AGENTS.md`'s corpus-population follow-up to
//! phase 2 (real content for the categories that stayed `.gitkeep`-only).
//!
//! This binary hard-codes the mapping from a staging directory's file
//! layout to `(category, slug)` pairs, since the source set is a curated,
//! one-time-reviewed list (licensing matters — see `corpus/ATTRIBUTION.md`),
//! not an open-ended user-supplied list. `hdr` is intentionally absent:
//! `.exr` source files are placed directly under `corpus/hdr/` without
//! import (see `cafe_bench::import`'s module docs).
//!
//! Usage:
//! ```text
//! cargo run -p cafe-bench --bin import-corpus <staging-dir> [corpus-dir]
//! ```
//! `staging-dir` must contain `photo/`, `screenshot/`, `illustration/`,
//! `lineart/` subdirectories with the expected file names (see
//! `EXPECTED_SOURCES` below). `corpus-dir` defaults to the workspace's
//! `corpus/` directory.

use cafe_bench::import::{import_sources, merge_and_write};
use cafe_bench::manifest::default_corpus_dir;
use cafe_bench::SourceImage;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

/// `(category, source file name relative to staging-dir/<category>/, slug)`.
/// The slug becomes both the output file's stem (`corpus/<category>/<slug>.png`)
/// and the manifest's `pattern` field.
const EXPECTED_SOURCES: &[(&str, &str, &str)] = &[
    ("photo", "kodim01.png", "kodim01"),
    ("photo", "kodim05.png", "kodim05"),
    ("photo", "kodim15.png", "kodim15"),
    ("photo", "kodim23.png", "kodim23"),
    ("lineart", "Blip_(PSF).png", "blip-psf"),
    ("lineart", "Abacus_(PSF).png", "abacus-psf"),
    ("lineart", "ABDOMEN_(PSF).png", "abdomen-psf"),
    (
        "lineart",
        "Aardvark2_(PSF)_colourised.png",
        "aardvark2-psf-colourised",
    ),
    ("illustration", "Abstract_art_(PSF).png", "abstract-art-psf"),
    (
        "illustration",
        "Hertzsprung-Russell_chart_(PSF).png",
        "hr-chart-psf",
    ),
    ("screenshot", "calculator.png", "calculator"),
    ("screenshot", "charmap.png", "charmap"),
    ("screenshot", "notepad.png", "notepad"),
    ("screenshot", "paint.png", "paint"),
];

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let staging_dir = match args.next() {
        Some(s) => PathBuf::from(s),
        None => {
            eprintln!("usage: import-corpus <staging-dir> [corpus-dir]");
            return ExitCode::FAILURE;
        }
    };
    let corpus_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_corpus_dir);

    let mut sources = Vec::new();
    let mut missing = Vec::new();
    for &(category, file_name, slug) in EXPECTED_SOURCES {
        let source_path = staging_dir.join(category).join(file_name);
        if source_path.is_file() {
            sources.push(SourceImage {
                source_path,
                category: category.to_string(),
                slug: slug.to_string(),
            });
        } else {
            missing.push(source_path);
        }
    }

    if !missing.is_empty() {
        eprintln!(
            "import-corpus: missing {} expected source file(s):",
            missing.len()
        );
        for path in &missing {
            eprintln!("  {}", path.display());
        }
        return ExitCode::FAILURE;
    }

    println!(
        "Importing {} real-content images into {}...",
        sources.len(),
        corpus_dir.display()
    );

    let entries = match import_sources(&corpus_dir, &sources) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("import-corpus failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    match merge_and_write(&corpus_dir, entries) {
        Ok(manifest) => {
            println!(
                "Wrote {} total images to {}/manifest.json",
                manifest.images.len(),
                corpus_dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import-corpus failed: {e}");
            ExitCode::FAILURE
        }
    }
}
