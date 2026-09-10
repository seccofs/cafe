//! Synthetic corpus generation: writes deterministic PNGs to `corpus/` and a
//! `manifest.json` describing them (SHA-256, dimensions, color type, bit
//! depth), per `AGENTS.md` phase 2.
//!
//! Only the categories that can be produced procedurally
//! (`gradient`/`pixelart`/`texture`/`synthetic`) are populated here.
//! `photo`/`screenshot`/`illustration`/`lineart`/`hdr` need real content and
//! stay as `.gitkeep`-only placeholders until that's added (see
//! `AGENTS.md`'s corpus table).

use crate::corpus::{generate, Pattern};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Pixel dimensions generated for every synthetic category. Kept small
/// (<=256x256) so the corpus stays lightweight enough to commit as PNGs.
const SIZES: &[(u32, u32)] = &[(64, 64), (256, 256)];

/// Maps each synthetic corpus category to the pattern used to fill it.
/// Order here is the order entries appear in `manifest.json`.
fn synthetic_categories() -> Vec<(&'static str, Pattern)> {
    vec![
        ("gradient", Pattern::Gradient),
        ("pixelart", Pattern::Checkerboard { square_size: 4 }),
        ("texture", Pattern::Texture),
        ("synthetic", Pattern::Noise { seed: 0xC0FFEE }),
    ]
}

/// One entry in `manifest.json`: everything needed to identify and validate
/// a corpus image without decoding it first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntry {
    /// Path relative to the `corpus/` directory, using forward slashes.
    pub path: String,
    /// Corpus category (subdirectory name), e.g. `"gradient"`.
    pub category: String,
    /// Generator pattern name, e.g. `"checkerboard4"`.
    pub pattern: String,
    pub width: u32,
    pub height: u32,
    /// Always `"rgba"` for the current generator (see `corpus` module doc).
    pub color_type: String,
    /// Bits per channel. 8 for every PNG entry; HDR entries (see
    /// `crate::import::register_hdr_sources`) are `32` (float32).
    pub bit_depth: u8,
    /// SHA-256 of the file's bytes on disk (PNG or, for HDR entries, the
    /// original `.exr`), hex-encoded.
    pub sha256: String,
    /// Size of the file on disk, in bytes (PNG or original `.exr`).
    pub file_size: u64,
    /// Container format of the file at `path`: `"png"` for every entry
    /// produced by [`generate_corpus`]/[`crate::import::import_sources`],
    /// `"exr"` for HDR entries from
    /// [`crate::import::register_hdr_sources`]. Defaults to `"png"` when
    /// absent so manifests written before this field existed still parse.
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "png".to_string()
}

/// Top-level `manifest.json` document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version, bumped if the entry shape changes incompatibly.
    pub version: u32,
    pub images: Vec<ImageEntry>,
}

/// Generation/IO failures, kept separate from [`crate::measure::MeasureError`]
/// since this module's failure modes (file IO, JSON serialization) differ.
#[derive(Debug)]
pub enum ManifestError {
    Io(io::Error),
    Png(image::ImageError),
    Json(serde_json::Error),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "I/O error: {e}"),
            ManifestError::Png(e) => write!(f, "PNG encoding failed: {e}"),
            ManifestError::Json(e) => write!(f, "manifest serialization failed: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<io::Error> for ManifestError {
    fn from(e: io::Error) -> Self {
        ManifestError::Io(e)
    }
}

impl From<image::ImageError> for ManifestError {
    fn from(e: image::ImageError) -> Self {
        ManifestError::Png(e)
    }
}

impl From<serde_json::Error> for ManifestError {
    fn from(e: serde_json::Error) -> Self {
        ManifestError::Json(e)
    }
}

/// Generates every synthetic category into `corpus_root` (creating
/// subdirectories as needed), writes `corpus_root/manifest.json`, and
/// returns the manifest that was written.
///
/// Re-running this is idempotent: the generators are deterministic, so
/// regenerated files are byte-identical and the manifest is stable modulo
/// entry order (which is fixed by this module's private `synthetic_categories`
/// and `SIZES`).
pub fn generate_corpus(corpus_root: &Path) -> Result<Manifest, ManifestError> {
    let mut images = Vec::new();

    for (category, pattern) in synthetic_categories() {
        let category_dir = corpus_root.join(category);
        fs::create_dir_all(&category_dir)?;

        for &(width, height) in SIZES {
            let img = generate(pattern, width, height);
            let file_name = format!("{}_{width}x{height}.png", pattern.name());
            let file_path = category_dir.join(&file_name);

            let mut png_bytes = Vec::new();
            img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut png_bytes))?;
            fs::write(&file_path, &png_bytes)?;

            let sha256 = hex_sha256(&png_bytes);
            let rel_path = format!("{category}/{file_name}");

            images.push(ImageEntry {
                path: rel_path,
                category: category.to_string(),
                pattern: pattern.name(),
                width,
                height,
                color_type: "rgba".to_string(),
                bit_depth: 8,
                sha256,
                file_size: png_bytes.len() as u64,
                format: default_format(),
            });
        }
    }

    let manifest = Manifest { version: 1, images };
    let manifest_path = corpus_root.join("manifest.json");
    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(&manifest_path, json)?;

    Ok(manifest)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Default location of the `corpus/` directory: `<crate>/../../corpus`,
/// i.e. the workspace root's `corpus/`, resolved relative to this crate's
/// manifest so it works regardless of the caller's current directory.
pub fn default_corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("corpus")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_corpus_is_deterministic() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");

        let manifest_a = generate_corpus(dir_a.path()).expect("generate a");
        let manifest_b = generate_corpus(dir_b.path()).expect("generate b");

        assert_eq!(manifest_a.images.len(), manifest_b.images.len());
        for (a, b) in manifest_a.images.iter().zip(manifest_b.images.iter()) {
            assert_eq!(a.path, b.path);
            assert_eq!(a.sha256, b.sha256, "{} sha256 must be reproducible", a.path);
            assert_eq!(a.file_size, b.file_size);
        }
    }

    #[test]
    fn generate_corpus_writes_expected_categories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = generate_corpus(dir.path()).expect("generate");

        let categories: std::collections::HashSet<_> = manifest
            .images
            .iter()
            .map(|e| e.category.as_str())
            .collect();
        assert!(categories.contains("gradient"));
        assert!(categories.contains("pixelart"));
        assert!(categories.contains("texture"));
        assert!(categories.contains("synthetic"));

        // Every referenced file must actually exist on disk with a matching
        // size, and the manifest.json itself must be present.
        assert!(dir.path().join("manifest.json").is_file());
        for entry in &manifest.images {
            let path = dir.path().join(&entry.path);
            assert!(path.is_file(), "{} must exist", entry.path);
            let metadata = fs::metadata(&path).expect("metadata");
            assert_eq!(metadata.len(), entry.file_size);
        }
    }

    #[test]
    fn hex_sha256_matches_known_vector() {
        // SHA-256("") — a standard test vector, sanity-checks the hex
        // encoding independent of image/PNG code.
        assert_eq!(
            hex_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
