//! Real-content corpus import: takes arbitrary source images (photos,
//! screenshots, line art, illustrations) staged on disk, normalizes them to
//! the same shape the synthetic generators in [`crate::corpus`] already
//! produce (RGBA8, capped at 256x256, written as PNG under `corpus/<category>/`),
//! and extends `corpus/manifest.json` with one [`crate::manifest::ImageEntry`]
//! per source image.
//!
//! This exists because `AGENTS.md` phase 2 deliberately left `photo`,
//! `screenshot`, `illustration`, and `lineart` as `.gitkeep`-only
//! placeholders — procedurally generating believable photographic or UI
//! content isn't practical, so those categories need real source images
//! instead.
//!
//! `hdr` is handled separately by [`register_hdr_sources`], not
//! [`import_sources`]: `.exr` files are float32 and aren't representable as
//! an 8-bit PNG at all (`cafe-cli`'s `png_io` bridge is 8-bit-only, see its
//! module docs), so there's no resize-and-re-encode-as-PNG step for this
//! category — the original `.exr` is the corpus asset, referenced by path
//! rather than transcoded.
//!
//! Unlike [`crate::manifest::generate_corpus`], this module is not
//! idempotent in the "byte-identical on every run" sense — it resizes
//! whatever source image it's given, and a different source image (or a
//! different `image` crate version's resize filter) would change the
//! output bytes. It *is* idempotent in the "re-running with the same
//! sources reproduces the same manifest" sense, since [`resize_to_fit`] is
//! a pure function of its inputs.

use crate::manifest::{ImageEntry, Manifest, ManifestError};
use image::imageops::FilterType;
use image::DynamicImage;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// Maximum edge length real-content images are downscaled to, matching the
/// largest synthetic size (256x256, `crate::manifest`'s internal `SIZES`)
/// so the real categories don't dominate repo size.
pub const MAX_DIMENSION: u32 = 256;

/// One source image staged for import: where it lives on disk, which
/// `corpus/` category it belongs to, and a short slug used both in the
/// output filename and the manifest's `pattern` field (standing in for the
/// synthetic generators' pattern name, e.g. `"gradient"` — here it's
/// instead a stable identifier for *which* real image this is, e.g.
/// `"kodim01"` or `"notepad"`).
#[derive(Debug, Clone)]
pub struct SourceImage {
    /// Absolute or relative path to the already-downloaded/captured source
    /// file (any format `image::ImageReader` can guess-and-decode).
    pub source_path: std::path::PathBuf,
    /// `corpus/` subdirectory this belongs to, e.g. `"photo"`.
    pub category: String,
    /// Short filesystem-safe slug identifying this specific image,
    /// e.g. `"kodim01"`.
    pub slug: String,
}

/// Import-specific failures, layered on top of [`ManifestError`] for the
/// I/O/serialization failure modes the two modules share.
#[derive(Debug)]
pub enum ImportError {
    Manifest(ManifestError),
    Decode {
        path: std::path::PathBuf,
        source: image::ImageError,
    },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Manifest(e) => write!(f, "{e}"),
            ImportError::Decode { path, source } => {
                write!(f, "failed to decode {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ImportError {}

impl From<ManifestError> for ImportError {
    fn from(e: ManifestError) -> Self {
        ImportError::Manifest(e)
    }
}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        ImportError::Manifest(ManifestError::Io(e))
    }
}

impl From<serde_json::Error> for ImportError {
    fn from(e: serde_json::Error) -> Self {
        ImportError::Manifest(ManifestError::Json(e))
    }
}

/// Downscales `img` so neither dimension exceeds `max_dim`, preserving
/// aspect ratio; images already within bounds are returned unchanged
/// (never upscaled — a small source image stays small rather than gaining
/// fake detail). Uses Lanczos3 for quality, since this runs once at import
/// time, not in any hot path.
pub fn resize_to_fit(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (width, height) = (img.width(), img.height());
    if width <= max_dim && height <= max_dim {
        return img.clone();
    }
    // `image::imageops::resize` (via `DynamicImage::resize`) already
    // preserves aspect ratio and fits within a bounding box when given
    // `FilterType` + the `resize` (not `resize_exact`) entry point.
    img.resize(max_dim, max_dim, FilterType::Lanczos3)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Imports every [`SourceImage`] into `corpus_root`, writing a resized
/// (capped at [`MAX_DIMENSION`]) RGBA8 PNG under `corpus_root/<category>/`
/// and returning one [`ImageEntry`] per source, in input order.
///
/// This does not touch `manifest.json` itself — callers combine the
/// returned entries with any pre-existing manifest (e.g. the synthetic one
/// from [`crate::manifest::generate_corpus`]) via [`merge_and_write`].
pub fn import_sources(
    corpus_root: &Path,
    sources: &[SourceImage],
) -> Result<Vec<ImageEntry>, ImportError> {
    let mut entries = Vec::with_capacity(sources.len());

    for source in sources {
        let img = image::ImageReader::open(&source.source_path)?
            .with_guessed_format()?
            .decode()
            .map_err(|e| ImportError::Decode {
                path: source.source_path.clone(),
                source: e,
            })?;

        let resized = resize_to_fit(&img, MAX_DIMENSION);
        let rgba = resized.to_rgba8();
        let (width, height) = (rgba.width(), rgba.height());

        let category_dir = corpus_root.join(&source.category);
        fs::create_dir_all(&category_dir)?;

        let file_name = format!("{}.png", source.slug);
        let file_path = category_dir.join(&file_name);

        let mut png_bytes = Vec::new();
        DynamicImage::ImageRgba8(rgba)
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut png_bytes))
            .map_err(|e| ImportError::Decode {
                path: source.source_path.clone(),
                source: e,
            })?;
        fs::write(&file_path, &png_bytes)?;

        let sha256 = hex_sha256(&png_bytes);
        let rel_path = format!("{}/{file_name}", source.category);

        entries.push(ImageEntry {
            path: rel_path,
            category: source.category.clone(),
            pattern: source.slug.clone(),
            width,
            height,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256,
            file_size: png_bytes.len() as u64,
            format: "png".to_string(),
        });
    }

    Ok(entries)
}

/// One `.exr` file already staged under `corpus/hdr/`, to be recorded in
/// `manifest.json` without any resizing/re-encoding (see module docs for
/// why this differs from [`import_sources`]).
#[derive(Debug, Clone)]
pub struct HdrSource {
    /// Path to the `.exr` file, already in its final `corpus/hdr/`
    /// location (unlike [`SourceImage::source_path`], nothing is copied or
    /// transcoded — this *is* the corpus asset).
    pub path: std::path::PathBuf,
    /// Short filesystem-safe slug identifying this image, used as the
    /// manifest's `pattern` field, e.g. `"blobbies"`.
    pub slug: String,
}

/// Records HDR `.exr` files already present under `corpus_root/hdr/` as
/// [`ImageEntry`] values, without resizing or transcoding them (see module
/// docs). Dimensions come from decoding the file just far enough to read
/// its header via `image::ImageReader`; `bit_depth` is always `32`
/// (float32) and `format` is `"exr"`, distinguishing these entries from
/// every PNG entry [`crate::manifest::generate_corpus`]/[`import_sources`]
/// produce.
pub fn register_hdr_sources(
    corpus_root: &Path,
    sources: &[HdrSource],
) -> Result<Vec<ImageEntry>, ImportError> {
    let mut entries = Vec::with_capacity(sources.len());

    for source in sources {
        let bytes = fs::read(&source.path)?;
        let (width, height) = image::ImageReader::open(&source.path)?
            .with_guessed_format()?
            .into_dimensions()
            .map_err(|e| ImportError::Decode {
                path: source.path.clone(),
                source: e,
            })?;

        let sha256 = hex_sha256(&bytes);
        let rel_path = source
            .path
            .strip_prefix(corpus_root)
            .unwrap_or(&source.path)
            .to_string_lossy()
            .replace('\\', "/");

        entries.push(ImageEntry {
            path: rel_path,
            category: "hdr".to_string(),
            pattern: source.slug.clone(),
            width,
            height,
            color_type: "rgba".to_string(),
            bit_depth: 32,
            sha256,
            file_size: bytes.len() as u64,
            format: "exr".to_string(),
        });
    }

    Ok(entries)
}

/// Merges `new_entries` into `corpus_root/manifest.json` (loading the
/// existing manifest if present, otherwise starting from `version: 1` with
/// no images) and writes the result back, replacing any prior entry that
/// shares a `path` with a new one (so re-running import for the same
/// sources updates in place instead of duplicating).
pub fn merge_and_write(
    corpus_root: &Path,
    new_entries: Vec<ImageEntry>,
) -> Result<Manifest, ImportError> {
    let manifest_path = corpus_root.join("manifest.json");
    let mut manifest = if manifest_path.is_file() {
        let json = fs::read_to_string(&manifest_path)?;
        serde_json::from_str(&json)?
    } else {
        Manifest {
            version: 1,
            images: Vec::new(),
        }
    };

    for entry in new_entries {
        if let Some(existing) = manifest.images.iter_mut().find(|e| e.path == entry.path) {
            *existing = entry;
        } else {
            manifest.images.push(entry);
        }
    }

    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(&manifest_path, json)?;

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn make_test_png(dir: &Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let img = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let path = dir.join(name);
        img.save(&path).expect("save test source PNG");
        path
    }

    #[test]
    fn resize_to_fit_downscales_large_images() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(1000, 500));
        let resized = resize_to_fit(&img, 256);
        assert!(resized.width() <= 256);
        assert!(resized.height() <= 256);
        // Aspect ratio preserved: original is 2:1.
        assert_eq!(resized.width(), 256);
        assert_eq!(resized.height(), 128);
    }

    #[test]
    fn resize_to_fit_leaves_small_images_unchanged() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(64, 32));
        let resized = resize_to_fit(&img, 256);
        assert_eq!(resized.width(), 64);
        assert_eq!(resized.height(), 32);
    }

    #[test]
    fn resize_to_fit_never_upscales() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(10, 10));
        let resized = resize_to_fit(&img, 256);
        assert_eq!((resized.width(), resized.height()), (10, 10));
    }

    #[test]
    fn import_sources_writes_resized_pngs_and_entries() {
        let src_dir = tempfile::tempdir().expect("src tempdir");
        let corpus_dir = tempfile::tempdir().expect("corpus tempdir");

        let path_a = make_test_png(src_dir.path(), "a.png", 500, 300);
        let path_b = make_test_png(src_dir.path(), "b.png", 100, 100);

        let sources = vec![
            SourceImage {
                source_path: path_a,
                category: "photo".to_string(),
                slug: "sample-a".to_string(),
            },
            SourceImage {
                source_path: path_b,
                category: "screenshot".to_string(),
                slug: "sample-b".to_string(),
            },
        ];

        let entries = import_sources(corpus_dir.path(), &sources).expect("import");
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].category, "photo");
        assert_eq!(entries[0].pattern, "sample-a");
        assert!(entries[0].width <= MAX_DIMENSION);
        assert!(entries[0].height <= MAX_DIMENSION);

        assert_eq!(entries[1].category, "screenshot");
        assert_eq!(entries[1].width, 100);
        assert_eq!(entries[1].height, 100);

        for entry in &entries {
            let full_path = corpus_dir.path().join(&entry.path);
            assert!(full_path.is_file(), "{} must exist", entry.path);
            let metadata = fs::metadata(&full_path).expect("metadata");
            assert_eq!(metadata.len(), entry.file_size);
        }
    }

    #[test]
    fn merge_and_write_creates_manifest_if_absent() {
        let corpus_dir = tempfile::tempdir().expect("tempdir");
        let entries = vec![ImageEntry {
            path: "photo/x.png".to_string(),
            category: "photo".to_string(),
            pattern: "x".to_string(),
            width: 10,
            height: 10,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256: "deadbeef".to_string(),
            file_size: 123,
            format: "png".to_string(),
        }];

        let manifest = merge_and_write(corpus_dir.path(), entries).expect("merge");
        assert_eq!(manifest.images.len(), 1);
        assert!(corpus_dir.path().join("manifest.json").is_file());
    }

    #[test]
    fn merge_and_write_replaces_entries_with_same_path() {
        let corpus_dir = tempfile::tempdir().expect("tempdir");

        let first = vec![ImageEntry {
            path: "photo/x.png".to_string(),
            category: "photo".to_string(),
            pattern: "x".to_string(),
            width: 10,
            height: 10,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256: "aaaa".to_string(),
            file_size: 100,
            format: "png".to_string(),
        }];
        merge_and_write(corpus_dir.path(), first).expect("first merge");

        let second = vec![ImageEntry {
            path: "photo/x.png".to_string(),
            category: "photo".to_string(),
            pattern: "x".to_string(),
            width: 20,
            height: 20,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256: "bbbb".to_string(),
            file_size: 200,
            format: "png".to_string(),
        }];
        let manifest = merge_and_write(corpus_dir.path(), second).expect("second merge");

        assert_eq!(manifest.images.len(), 1, "must replace, not duplicate");
        assert_eq!(manifest.images[0].sha256, "bbbb");
        assert_eq!(manifest.images[0].width, 20);
    }

    #[test]
    fn merge_and_write_preserves_existing_unrelated_entries() {
        let corpus_dir = tempfile::tempdir().expect("tempdir");

        let synthetic = vec![ImageEntry {
            path: "gradient/gradient_64x64.png".to_string(),
            category: "gradient".to_string(),
            pattern: "gradient".to_string(),
            width: 64,
            height: 64,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256: "cccc".to_string(),
            file_size: 300,
            format: "png".to_string(),
        }];
        merge_and_write(corpus_dir.path(), synthetic).expect("seed manifest");

        let real = vec![ImageEntry {
            path: "photo/kodim01.png".to_string(),
            category: "photo".to_string(),
            pattern: "kodim01".to_string(),
            width: 256,
            height: 171,
            color_type: "rgba".to_string(),
            bit_depth: 8,
            sha256: "dddd".to_string(),
            file_size: 400,
            format: "png".to_string(),
        }];
        let manifest = merge_and_write(corpus_dir.path(), real).expect("merge real");

        assert_eq!(manifest.images.len(), 2);
        assert!(manifest
            .images
            .iter()
            .any(|e| e.path == "gradient/gradient_64x64.png"));
        assert!(manifest
            .images
            .iter()
            .any(|e| e.path == "photo/kodim01.png"));
    }
}
