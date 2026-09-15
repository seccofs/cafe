//! Scalar, whole-file decoder (Phase 5-7, see `AGENTS.md`): header ->
//! chunks -> ZSTD decompress -> unpredict -> tile assembly -> pixels.
//!
//! Supports both the implicit single-tile case (`iDIM` absent — spec
//! section 4.2: "the decoder assumes a single `IDAT` covering the entire
//! image") and full 2D tiling (`iDIM` present, row-major or Z-order scan,
//! any number of tiles up to `MAX_TILE_COUNT`) via [`crate::tiling::TileLayout`].

use crate::error::{CodecError, Result};
use crate::palette::expand_indices;
use crate::tile::decode_tile_rows;
use crate::tiling::TileLayout;
use crate::zstd_codec::decompress_with_limit;
use cafe_format::chunk::{is_critical, read_chunk};
use cafe_format::ihdr::{read_ihdr, Ihdr};
use cafe_format::validate_signature;
use cafe_format::{Idim, JsonChunk, Plte, Xmpd};

/// A fully decoded CAFE image: the validated header plus raw, unpredicted
/// pixel bytes in row-major order (top-to-bottom), each sample stored per
/// `Ihdr::bit_depth`'s endianness (spec section 4.1: big-endian for
/// `bit_depth > 8`), channels interleaved per `Ihdr::color_type`'s order.
///
/// Also carries any of the four ancillary metadata chunks (spec sections
/// 4.5-4.8) found before the first `IDAT`, if present. All four are
/// `Option`/`Vec`-shaped so a file with none of them still decodes
/// normally with everything empty/`None` — metadata is purely additive,
/// never required (spec section 8.4: pixel decoding never depends on
/// metadata content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub ihdr: Ihdr,
    pub pixels: Vec<u8>,
    /// Raw EXIF TIFF blob (spec section 4.5), if an `eXIF` chunk was
    /// present. Only the *first* instance is kept (spec section 4.5: "a
    /// decoder finding more than one must consider only the first").
    pub exif: Option<Vec<u8>>,
    /// Every successfully parsed `jSON` chunk (spec section 4.6), in file
    /// order. A chunk whose *content* is malformed (bad namespace length,
    /// non-ASCII namespace, invalid JSON syntax) is silently skipped
    /// here rather than surfaced as a decode error, per spec section
    /// 8.4 — it never appears in this list, but does not abort decoding
    /// either. Multiple entries may share the same namespace if the file
    /// deliberately repeats one; this type does not deduplicate.
    pub json_chunks: Vec<JsonChunk>,
    /// Raw ICC profile bytes (spec section 4.7), if an `iCCP` chunk was
    /// present. Only the first instance is kept (mirrors `eXIF`'s
    /// single-instance handling, since spec section 4.7 doesn't specify
    /// duplicate behavior explicitly). `None` means the default color
    /// space applies: sRGB (IEC 61966-2-1).
    pub icc_profile: Option<Vec<u8>>,
    /// Parsed XMP metadata (spec section 4.8), if an `xMPd` chunk was
    /// present and its payload was valid UTF-8. A non-UTF-8 `xMPd`
    /// payload is silently discarded here (spec section 8.4), leaving
    /// this `None`, rather than surfaced as a decode error. Only the
    /// first instance is kept if more than one is present.
    pub xmp: Option<Xmpd>,
}

/// The result of the sequential, spec-mandated chunk-framing/ordering pass
/// (spec section 5's mandatory chunk order; `iDIM`/`PLTE` single-instance
/// and before-first-`IDAT` rules; CRC validation via
/// [`cafe_format::chunk::read_chunk`]) — everything a `.cafe` file's byte
/// layout requires to be checked strictly in file order, before any tile's
/// pixel content is touched.
///
/// `idat_chunks` holds each `IDAT` [`cafe_format::ReadChunk`] in scan
/// order (the *i*-th entry is tile *i*, spec section 4.2), still
/// compressed/unfiltered — actually decompressing and reversing the
/// predictor for each one is deliberately deferred to the caller
/// ([`decode_bytes`] or [`decode_bytes_parallel`]), since spec section 4.4
/// ("Each `IDAT` is independent ... decoded as soon as it arrives") makes
/// that part of the work embarrassingly parallel, unlike the framing pass
/// itself (each chunk's offset depends on the previous chunk having been
/// fully parsed, so this part must stay sequential).
struct ParsedFile {
    ihdr: Ihdr,
    layout: TileLayout,
    effective_bpp: u32,
    bytes_per_row: u32,
    plte: Option<Plte>,
    exif: Option<Vec<u8>>,
    json_chunks: Vec<JsonChunk>,
    icc_profile: Option<Vec<u8>>,
    xmp: Option<Xmpd>,
    idat_chunks: Vec<cafe_format::ReadChunk>,
}

/// Runs the sequential chunk-framing/validation pass shared by
/// [`decode_bytes`] and [`decode_bytes_parallel`]. See [`ParsedFile`]'s
/// doc comment for why `IDAT` decompression/unfiltering itself is not
/// done here.
fn parse_chunks(buf: &[u8]) -> Result<ParsedFile> {
    let mut offset = validate_signature(buf)?;
    let (ihdr, next) = read_ihdr(buf, offset)?;
    offset = next;

    let bpp = ihdr.bytes_per_pixel().ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::InvalidIhdr(format!(
            "no channel count for color_type={}",
            ihdr.color_type
        )))
    })?;

    let mut idim: Option<Idim> = None;
    let mut plte: Option<Plte> = None;
    let mut layout: Option<(TileLayout, u32)> = None;
    let mut idat_chunks: Vec<cafe_format::ReadChunk> = Vec::new();
    let mut saw_iend = false;

    // Ancillary metadata (spec sections 4.5-4.8), collected as chunks are
    // encountered. Per spec section 8.4, malformed *content* in any of
    // these never aborts decoding — only a genuine framing error (caught
    // by `read_chunk`/`decompress_with_limit` before we even get here)
    // does. Single-instance types (eXIF/iCCP/xMPd) keep only the first
    // occurrence (spec section 4.5's explicit rule, applied consistently
    // to iCCP/xMPd too since the spec doesn't call out different
    // behavior for them).
    let mut exif: Option<Vec<u8>> = None;
    let mut json_chunks: Vec<JsonChunk> = Vec::new();
    let mut icc_profile: Option<Vec<u8>> = None;
    let mut xmp: Option<Xmpd> = None;

    while offset < buf.len() {
        let chunk = read_chunk(buf, offset)?;
        offset = chunk.next_offset;

        match &chunk.chunk_type {
            b"iDIM" => {
                if idim.is_some() {
                    return Err(CodecError::TilingMismatch(
                        "duplicate iDIM chunk (spec section 4.2: single instance per file)".into(),
                    ));
                }
                if !idat_chunks.is_empty() {
                    return Err(CodecError::TilingMismatch(
                        "iDIM chunk must appear before the first IDAT (spec section 5's \
                         mandatory chunk order)"
                            .into(),
                    ));
                }
                let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                let parsed = Idim::from_payload(&payload)?;
                parsed.validate(ihdr.width, ihdr.height)?;
                idim = Some(parsed);
            }
            b"PLTE" => {
                if plte.is_some() {
                    return Err(CodecError::TilingMismatch(
                        "duplicate PLTE chunk (spec section 4.3: single instance per file)".into(),
                    ));
                }
                if !idat_chunks.is_empty() {
                    return Err(CodecError::TilingMismatch(
                        "PLTE chunk must appear before the first IDAT (spec section 5's \
                         mandatory chunk order)"
                            .into(),
                    ));
                }
                let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                let parsed = Plte::from_payload(&payload, ihdr.color_type)?;
                parsed.validate(ihdr.color_type, ihdr.bit_depth)?;
                plte = Some(parsed);
            }
            b"eXIF" => {
                // Spec section 4.5: "a decoder finding more than one must
                // consider only the first" — never an error, later
                // instances are simply ignored, not rejected.
                if exif.is_none() {
                    let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                    exif = Some(payload);
                }
            }
            b"jSON" => {
                // Spec section 8.4: a malformed jSON chunk's *content*
                // must not invalidate the file — decompression failures
                // here are still framing-level (bounded by the chunk's
                // own declared Length, same as every other chunk type),
                // but JsonChunk::from_payload's InvalidJsonChunk errors
                // are deliberately swallowed rather than propagated.
                let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                if let Ok(parsed) = JsonChunk::from_payload(&payload) {
                    json_chunks.push(parsed);
                }
            }
            b"iCCP" => {
                // Single instance (spec section 4.7); mirrors eXIF's
                // first-instance-wins handling.
                if icc_profile.is_none() {
                    let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                    icc_profile = Some(payload);
                }
            }
            b"xMPd" => {
                // Single instance (spec section 4.8). A non-UTF-8 payload
                // is a content-level problem (spec section 8.4), silently
                // discarded rather than propagated as a decode error.
                if xmp.is_none() {
                    let payload = crate::zstd_codec::decompress_chunk(chunk.flag, &chunk.data)?;
                    if let Ok(parsed) = Xmpd::from_payload(&payload) {
                        xmp = Some(parsed);
                    }
                }
            }
            b"IDAT" => {
                if layout.is_none() {
                    let this_bpp = if plte.is_some() { 1 } else { bpp };
                    let built = TileLayout::new(idim, ihdr.width, ihdr.height)?;
                    layout = Some((built, this_bpp));
                }
                let (layout_ref, _) = layout.as_ref().unwrap();
                if idat_chunks.len() >= layout_ref.tile_count() {
                    return Err(CodecError::TilingMismatch(format!(
                        "file contains more IDAT chunks than iDIM declares tiles ({})",
                        layout_ref.tile_count()
                    )));
                }
                idat_chunks.push(chunk);
            }
            b"IEND" => {
                saw_iend = true;
                break;
            }
            other if is_critical(other) => {
                return Err(CodecError::Format(
                    cafe_format::CafeError::UnsupportedFeature(format!(
                        "unrecognized critical chunk type {:?}",
                        String::from_utf8_lossy(other)
                    )),
                ));
            }
            _ => {
                // Unrecognized ancillary chunk (not one of eXIF/jSON/
                // iCCP/xMPd/iDIM/PLTE, all handled above) — spec section
                // 8.4/3.1: a decoder may always safely skip an ancillary
                // chunk type it doesn't understand.
            }
        }
    }

    if !saw_iend {
        return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "file ended without an IEND chunk".into(),
        )));
    }

    let (layout, effective_bpp) = layout.ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "file contains no IDAT chunk".into(),
        ))
    })?;
    if idat_chunks.len() != layout.tile_count() {
        return Err(CodecError::TilingMismatch(format!(
            "iDIM declares {} tile(s), but only {} IDAT chunk(s) were present",
            layout.tile_count(),
            idat_chunks.len()
        )));
    }
    let bytes_per_row = ihdr.width.checked_mul(effective_bpp).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "overflow computing bytes_per_row (width * bpp)".into(),
        ))
    })?;

    Ok(ParsedFile {
        ihdr,
        layout,
        effective_bpp,
        bytes_per_row,
        plte,
        exif,
        json_chunks,
        icc_profile,
        xmp,
        idat_chunks,
    })
}

/// Decodes a whole in-memory `.cafe` file into pixel bytes.
///
/// Streaming (chunk-at-a-time, without requiring the whole file in memory)
/// is deferred to a `Decoder<R>` type in a later phase (mirrors
/// `cafe-format`'s Phase 4 decision to defer the `Read`-based chunk
/// primitive) — this is the reference, whole-buffer path golden files are
/// validated against.
///
/// This decodes tiles one at a time, sequentially, in scan order — see
/// [`decode_bytes_parallel`] for a version that decompresses/unfilters
/// tiles across multiple threads once framing has been validated (both
/// produce byte-for-byte identical [`DecodedImage::pixels`]).
pub fn decode_bytes(buf: &[u8]) -> Result<DecodedImage> {
    let parsed = parse_chunks(buf)?;
    let total_pixel_bytes = (parsed.ihdr.height as u64)
        .checked_mul(parsed.bytes_per_row as u64)
        .ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing total pixel buffer size".into(),
            ))
        })?;
    let mut pixels = vec![0u8; total_pixel_bytes as usize];

    for (index, chunk) in parsed.idat_chunks.iter().enumerate() {
        let tile = decode_tile(
            &parsed.layout,
            index,
            chunk.flag,
            &chunk.data,
            parsed.effective_bpp,
        )?;
        copy_tile_into_slice(&tile, parsed.bytes_per_row, &mut pixels);
    }

    finish_decoded_image(
        parsed.ihdr,
        pixels,
        parsed.plte,
        parsed.exif,
        parsed.json_chunks,
        parsed.icc_profile,
        parsed.xmp,
    )
}

/// Parallel counterpart to [`decode_bytes`]: identical chunk-framing,
/// validation, and error behavior (built on the exact same
/// [`parse_chunks`] pass — a malformed file is rejected the same way,
/// with the same [`CodecError`] variant, regardless of which function
/// decodes it), but once every `IDAT` chunk's bytes have been collected
/// in scan order, their decompression + predictor-reversal + copy into
/// the final pixel buffer runs across multiple threads instead of one
/// (spec section 4.4: "Each `IDAT` is independent ... decoded as soon as
/// it arrives" is exactly the independence property this relies on — see
/// `AGENTS.md`'s parallel-tiling benchmark for the measurements that
/// motivated adding this).
///
/// Produces byte-for-byte identical [`DecodedImage`] output to
/// [`decode_bytes`] for the same input — confirmed by this module's
/// `test_decode_bytes_parallel_matches_sequential_*` tests. For small
/// tile counts (below an internal threshold, see
/// `crate::parallel::MIN_TILES_FOR_PARALLEL`) or on a single-core host,
/// this transparently falls back to the same sequential loop
/// [`decode_bytes`] uses, so there's no reason to prefer [`decode_bytes`]
/// over this function purely to avoid thread overhead on small files —
/// the only reason to still call [`decode_bytes`] directly is to force
/// single-threaded, deterministic-scheduling behavior explicitly.
pub fn decode_bytes_parallel(buf: &[u8]) -> Result<DecodedImage> {
    let parsed = parse_chunks(buf)?;
    let total_pixel_bytes = (parsed.ihdr.height as u64)
        .checked_mul(parsed.bytes_per_row as u64)
        .ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing total pixel buffer size".into(),
            ))
        })?;
    let mut pixels = vec![0u8; total_pixel_bytes as usize];

    // Each worker thread decodes its assigned tile(s) fully into its own
    // freshly-allocated `Vec<u8>` first (no shared-buffer access at all
    // during decompression/unfiltering), then copies only that tile's
    // rows into `pixels` via `RawSliceMut::write_at` — a raw-pointer
    // `copy_nonoverlapping`, never a `&mut [u8]` over the shared buffer
    // (see `RawSliceMut`'s doc comment for why the latter is UB under
    // Rust's aliasing model even when the actual byte ranges written
    // never overlap, a real bug this design was changed to fix after
    // `cargo miri test` caught it in an earlier version of this
    // function). `TileLayout::tile_rect` partitioning the image into
    // non-overlapping rectangles (spec section 4.2) is what guarantees
    // distinct tile indices always target disjoint byte ranges of
    // `pixels`.
    let raw_pixels = crate::parallel::RawSliceMut::new(&mut pixels);
    let layout = &parsed.layout;
    let idat_chunks = &parsed.idat_chunks;
    let bpp = parsed.effective_bpp;
    let bytes_per_row = parsed.bytes_per_row;
    crate::parallel::for_each_parallel(idat_chunks.len(), move |index| {
        let chunk = &idat_chunks[index];
        let tile = decode_tile(layout, index, chunk.flag, &chunk.data, bpp)?;
        copy_tile_into_raw_slice(&tile, bytes_per_row, &raw_pixels);
        Ok(())
    })?;

    finish_decoded_image(
        parsed.ihdr,
        pixels,
        parsed.plte,
        parsed.exif,
        parsed.json_chunks,
        parsed.icc_profile,
        parsed.xmp,
    )
}

/// One tile's fully decompressed+unfiltered pixel rows, plus the
/// rectangular region of the whole image they belong in — an
/// intermediate result held entirely in its own freshly-allocated
/// buffer, with no reference to (or access of) any other tile's data or
/// the final shared pixel buffer. This separation (decode fully in
/// isolation, *then* copy into place) is what lets
/// [`decode_bytes_parallel`] avoid ever constructing a `&mut [u8]` over
/// the shared output buffer from multiple threads (see `RawSliceMut`'s
/// doc comment).
struct DecodedTile {
    origin_x: u32,
    origin_y: u32,
    tile_w: u32,
    tile_h: u32,
    bpp: u32,
    tile_bytes_per_row: u32,
    pixels: Vec<u8>,
}

/// Decompresses+unfilters tile `index`'s `IDAT` payload into its own
/// buffer — the single per-tile work unit shared by [`decode_bytes`]'s
/// sequential loop and [`decode_bytes_parallel`]'s per-thread work, so
/// the two can never drift in how a tile's bytes turn into pixels. Does
/// not touch the final image-sized pixel buffer at all; see
/// [`copy_tile_into_slice`]/[`copy_tile_into_raw_slice`] for that step.
fn decode_tile(
    layout: &TileLayout,
    index: usize,
    flag: u8,
    data: &[u8],
    bpp: u32,
) -> Result<DecodedTile> {
    let (origin_x, origin_y, tile_w, tile_h) = layout.tile_rect(index);
    let tile_bytes_per_row = tile_w.checked_mul(bpp).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "overflow computing tile bytes_per_row".into(),
        ))
    })?;
    let expected_tile_payload = (tile_h as u64)
        .checked_mul(tile_bytes_per_row as u64 + 1)
        .ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing expected tile IDAT payload size".into(),
            ))
        })?;

    let raw = decompress_with_limit(flag, data, expected_tile_payload)?;
    let pixels = decode_tile_rows(&raw, tile_h, tile_bytes_per_row, bpp)?;

    Ok(DecodedTile {
        origin_x,
        origin_y,
        tile_w,
        tile_h,
        bpp,
        tile_bytes_per_row,
        pixels,
    })
}

/// Copies a [`DecodedTile`]'s rows into the correct rectangular region of
/// `pixels_buf` via ordinary slice indexing — used by [`decode_bytes`]'s
/// sequential loop, which owns `pixels_buf` exclusively and has no
/// concurrent-access concern at all.
fn copy_tile_into_slice(tile: &DecodedTile, full_bytes_per_row: u32, pixels_buf: &mut [u8]) {
    let row_bytes = (tile.tile_w * tile.bpp) as usize;
    for row in 0..tile.tile_h {
        let dst_start = (tile.origin_y + row) as usize * full_bytes_per_row as usize
            + tile.origin_x as usize * tile.bpp as usize;
        let src_start = row as usize * tile.tile_bytes_per_row as usize;
        pixels_buf[dst_start..dst_start + row_bytes]
            .copy_from_slice(&tile.pixels[src_start..src_start + row_bytes]);
    }
}

/// Copies a [`DecodedTile`]'s rows into the correct rectangular region of
/// a shared [`crate::parallel::RawSliceMut`] via
/// [`crate::parallel::RawSliceMut::write_at`] — used by
/// [`decode_bytes_parallel`]'s per-thread work, where `pixels_buf` is
/// shared across threads and must never be reconstructed as a `&mut
/// [u8]` (see `RawSliceMut`'s doc comment). Each `write_at` call targets
/// exactly one row's `row_bytes`-length range, which
/// [`TileLayout::tile_rect`]'s non-overlapping-rectangles guarantee (spec
/// section 4.2) ensures no other thread ever touches concurrently.
fn copy_tile_into_raw_slice(
    tile: &DecodedTile,
    full_bytes_per_row: u32,
    pixels_buf: &crate::parallel::RawSliceMut,
) {
    let row_bytes = (tile.tile_w * tile.bpp) as usize;
    for row in 0..tile.tile_h {
        let dst_start = (tile.origin_y + row) as usize * full_bytes_per_row as usize
            + tile.origin_x as usize * tile.bpp as usize;
        let src_start = row as usize * tile.tile_bytes_per_row as usize;
        // SAFETY: distinct tile indices' rectangles never overlap (spec
        // section 4.2 / `TileLayout::tile_rect`'s grid-partitioning
        // geometry), so this row's `[dst_start, dst_start + row_bytes)`
        // range is never targeted by any other concurrent `write_at`
        // call on this same `RawSliceMut`.
        unsafe {
            pixels_buf.write_at(dst_start, &tile.pixels[src_start..src_start + row_bytes]);
        }
    }
}

/// Shared tail end of [`decode_bytes`]/[`decode_bytes_parallel`]: expands
/// palette indices back to direct pixels (if `plte` is present) and
/// assembles the final [`DecodedImage`].
fn finish_decoded_image(
    ihdr: Ihdr,
    pixels: Vec<u8>,
    plte: Option<Plte>,
    exif: Option<Vec<u8>>,
    json_chunks: Vec<JsonChunk>,
    icc_profile: Option<Vec<u8>>,
    xmp: Option<Xmpd>,
) -> Result<DecodedImage> {
    // When PLTE is present, everything decoded above is palette indices
    // (one byte per pixel), not final channel bytes — expand them into
    // real pixels here so DecodedImage::pixels always holds direct
    // channel data, matching every non-palette decode's shape (spec
    // section 4.3: expansion is purely a decoder-side concern).
    let pixels = if let Some(plte) = &plte {
        expand_indices(&pixels, &plte.entries, plte.bytes_per_entry)?
    } else {
        pixels
    };

    Ok(DecodedImage {
        ihdr,
        pixels,
        exif,
        json_chunks,
        icc_profile,
        xmp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cafe_format::chunk::write_chunk;
    use cafe_format::constants::{
        COLOR_TYPE_GRAY, COLOR_TYPE_RGBA, COMPRESSION_METHOD_ZSTD_BIT, SAMPLE_FORMAT_UINT,
        SIGNATURE,
    };
    use std::path::{Path, PathBuf};

    fn golden_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("golden")
    }

    fn read_golden(name: &str) -> Vec<u8> {
        let path = golden_dir().join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read golden {path:?}: {e}"))
    }

    fn read_malformed(name: &str) -> Vec<u8> {
        let path = golden_dir().join("malformed").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read golden {path:?}: {e}"))
    }

    #[test]
    fn test_decode_golden_minimal_1x1_gray() {
        let buf = read_golden("minimal_1x1_gray.cafe");
        let img = decode_bytes(&buf).expect("golden file should decode");
        assert_eq!(img.ihdr.width, 1);
        assert_eq!(img.ihdr.height, 1);
        assert_eq!(img.ihdr.color_type, COLOR_TYPE_GRAY);
        // predictor=None row, one gray sample = 0x7F.
        assert_eq!(img.pixels, vec![0x7F]);
    }

    #[test]
    fn test_decode_golden_minimal_2x2_rgba() {
        let buf = read_golden("minimal_2x2_rgba.cafe");
        let img = decode_bytes(&buf).expect("golden file should decode");
        assert_eq!(img.ihdr.width, 2);
        assert_eq!(img.ihdr.height, 2);
        assert_eq!(img.ihdr.color_type, COLOR_TYPE_RGBA);
        assert_eq!(
            img.pixels,
            vec![
                0x10, 0x20, 0x30, 0xFF, 0x11, 0x21, 0x31, 0xFF, // row 0
                0x12, 0x22, 0x32, 0xFF, 0x13, 0x23, 0x33, 0xFF, // row 1
            ]
        );
    }

    #[test]
    fn test_decode_golden_minimal_2x2_indexed_rgb() {
        use cafe_format::constants::COLOR_TYPE_RGB;
        let buf = read_golden("minimal_2x2_indexed_rgb.cafe");
        let img = decode_bytes(&buf).expect("golden file should decode");
        assert_eq!(img.ihdr.width, 2);
        assert_eq!(img.ihdr.height, 2);
        assert_eq!(img.ihdr.color_type, COLOR_TYPE_RGB);
        // row0=[red,green], row1=[green,red], expanded from indices.
        assert_eq!(
            img.pixels,
            vec![0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00]
        );
    }

    #[test]
    fn test_decode_golden_minimal_1x1_gray_with_metadata() {
        let buf = read_golden("minimal_1x1_gray_with_metadata.cafe");
        let img = decode_bytes(&buf).expect("golden file should decode");
        assert_eq!(img.ihdr.width, 1);
        assert_eq!(img.ihdr.height, 1);
        assert_eq!(img.ihdr.color_type, COLOR_TYPE_GRAY);
        assert_eq!(img.pixels, vec![0x7F]);
        assert_eq!(img.exif.as_deref(), Some(b"fake exif bytes".as_slice()));
        assert_eq!(img.json_chunks.len(), 1);
        assert_eq!(img.json_chunks[0].namespace, "com.example");
        assert_eq!(img.json_chunks[0].payload, "{\"a\":1}");
        assert_eq!(
            img.icc_profile.as_deref(),
            Some(b"fake icc profile bytes".as_slice())
        );
        assert_eq!(img.xmp.as_ref().unwrap().xml, "<x:xmpmeta></x:xmpmeta>");
    }

    #[test]
    fn test_decode_malformed_bad_signature_is_rejected() {
        let buf = read_malformed("bad_signature.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::InvalidSignature))
        ));
    }

    #[test]
    fn test_decode_malformed_truncated_header_is_rejected() {
        let buf = read_malformed("truncated_header.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_malformed_crc_mismatch_is_rejected() {
        let buf = read_malformed("crc_mismatch.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(
                cafe_format::CafeError::CrcMismatch { .. }
            ))
        ));
    }

    #[test]
    fn test_decode_malformed_invalid_ihdr_zero_width_is_rejected() {
        let buf = read_malformed("invalid_ihdr_zero_width.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::InvalidIhdr(_)))
        ));
    }

    #[test]
    fn test_decode_malformed_forged_length_is_rejected() {
        let buf = read_malformed("forged_length.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_malformed_wrong_first_chunk_is_rejected() {
        let buf = read_malformed("wrong_first_chunk.cafe");
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(
                cafe_format::CafeError::UnexpectedChunkType { .. }
            ))
        ));
    }

    /// Builds a minimal valid file (signature + IHDR + one IDAT + IEND)
    /// with a caller-supplied predictor code and raw pixel rows, letting
    /// tests exercise the full decode path with different predictors,
    /// ZSTD-compressed IDATs, and adversarial trailers.
    fn build_file(
        ihdr: &Ihdr,
        idat_flag: u8,
        idat_data: &[u8],
        include_idim: Option<(u16, u16, u16, u16, u8)>,
        include_iend: bool,
    ) -> Vec<u8> {
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        if let Some((tw, th, tx, ty, scan)) = include_idim {
            let mut payload = Vec::with_capacity(9);
            payload.extend_from_slice(&tw.to_be_bytes());
            payload.extend_from_slice(&th.to_be_bytes());
            payload.extend_from_slice(&tx.to_be_bytes());
            payload.extend_from_slice(&ty.to_be_bytes());
            payload.push(scan);
            buf.extend(write_chunk(b"iDIM", 0x00, &payload));
        }
        buf.extend(write_chunk(b"IDAT", idat_flag, idat_data));
        if include_iend {
            buf.extend(write_chunk(b"IEND", 0x00, b""));
        }
        buf
    }

    fn gray_2x2_ihdr() -> Ihdr {
        Ihdr {
            width: 2,
            height: 2,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        }
    }

    #[test]
    fn test_decode_with_explicit_1x1_idim_succeeds() {
        let ihdr = gray_2x2_ihdr();
        // bytes_per_row = 2, bpp = 1: rows are [pred, s0, s1].
        let idat_payload = vec![0u8, 10, 20, 0u8, 30, 40];
        let buf = build_file(&ihdr, 0x00, &idat_payload, Some((2, 2, 1, 1, 0)), true);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![10, 20, 30, 40]);
    }

    #[test]
    fn test_decode_rejects_idim_declaring_multiple_tiles_with_only_one_idat() {
        let ihdr = gray_2x2_ihdr();
        // Consistent iDIM (2x2 tiles of 1x1 pixels covers a 2x2 image), but
        // the file only supplies one IDAT (correctly sized for the first
        // 1x1 tile: one row of [predictor, sample]) — a genuine
        // tile-count/IDAT-count mismatch, not a malformed iDIM or an
        // oversized/undersized payload.
        let idat_payload = vec![0u8, 10];
        let buf = build_file(&ihdr, 0x00, &idat_payload, Some((1, 1, 2, 2, 0)), true);
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_idim_single_tile_smaller_than_image() {
        let ihdr = gray_2x2_ihdr();
        let idat_payload = vec![0u8, 10, 20, 0u8, 30, 40];
        // Declares 1x1 tiles but a 1x1-pixel tile size, inconsistent with a 2x2 image.
        let buf = build_file(&ihdr, 0x00, &idat_payload, Some((1, 1, 1, 1, 0)), true);
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::InvalidIdim(_)))
        ));
    }

    #[test]
    fn test_decode_rejects_multiple_idat_without_idim() {
        let ihdr = Ihdr {
            width: 1,
            height: 1,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        };
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 5]));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 6]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_fewer_idat_than_idim_declares() {
        let ihdr = Ihdr {
            width: 4,
            height: 4,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        };
        // 2x2 tiles of 2x2 pixels = 4 tiles total, but only one correctly-
        // sized IDAT (one 2x2 tile: 2 rows of [predictor, 2 samples]) is
        // given for the first tile.
        let idat_payload = vec![0u8, 1, 2, 0u8, 3, 4];
        let buf = build_file(&ihdr, 0x00, &idat_payload, Some((2, 2, 2, 2, 0)), true);
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_duplicate_idim_chunk() {
        let ihdr = gray_2x2_ihdr();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        let mut payload = Vec::with_capacity(9);
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.push(0u8);
        buf.extend(write_chunk(b"iDIM", 0x00, &payload));
        buf.extend(write_chunk(b"iDIM", 0x00, &payload));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_idim_appearing_after_first_idat() {
        let ihdr = gray_2x2_ihdr();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        let mut payload = Vec::with_capacity(9);
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.push(0u8);
        buf.extend(write_chunk(b"iDIM", 0x00, &payload));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_multi_tile_row_major_roundtrip() {
        use crate::predictor::PREDICTOR_NONE;
        use crate::tile::encode_tile_rows;

        // 4x4 gray image split into 2x2 tiles of 2x2 pixels each, row-major
        // scan order (spec section 4.2).
        let width = 4u32;
        let height = 4u32;
        let ihdr = Ihdr {
            width,
            height,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        };
        // Full image, row-major, values = row*10 + col for easy checking.
        let full: Vec<u8> = (0..height)
            .flat_map(|r| (0..width).map(move |c| (r * 10 + c) as u8))
            .collect();

        // Tiles in row-major order: (0,0),(1,0),(0,1),(1,1), each 2x2.
        let tile_at = |tx: u32, ty: u32| -> Vec<u8> {
            let mut out = Vec::new();
            for r in 0..2u32 {
                for c in 0..2u32 {
                    let row = ty * 2 + r;
                    let col = tx * 2 + c;
                    out.push(full[(row * width + col) as usize]);
                }
            }
            out
        };

        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        let mut idim_payload = Vec::with_capacity(9);
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.push(0u8); // row-major
        buf.extend(write_chunk(b"iDIM", 0x00, &idim_payload));
        for (tx, ty) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
            let raw = tile_at(tx, ty);
            let payload = encode_tile_rows(&raw, 2, 2, 1, PREDICTOR_NONE).unwrap();
            buf.extend(write_chunk(b"IDAT", 0x00, &payload));
        }
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, full);
    }

    #[test]
    fn test_decode_multi_tile_z_order_roundtrip() {
        use crate::predictor::PREDICTOR_NONE;
        use crate::tile::encode_tile_rows;

        // Same 4x4 image and tiling as the row-major test, but scan_order=1
        // (Z-order): IDATs must appear in Morton order (0,0),(1,0),(0,1),(1,1)
        // for a 2x2 tile grid (matches tiling::tests's known sequence).
        let width = 4u32;
        let height = 4u32;
        let ihdr = Ihdr {
            width,
            height,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        };
        let full: Vec<u8> = (0..height)
            .flat_map(|r| (0..width).map(move |c| (r * 10 + c) as u8))
            .collect();
        let tile_at = |tx: u32, ty: u32| -> Vec<u8> {
            let mut out = Vec::new();
            for r in 0..2u32 {
                for c in 0..2u32 {
                    let row = ty * 2 + r;
                    let col = tx * 2 + c;
                    out.push(full[(row * width + col) as usize]);
                }
            }
            out
        };

        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        let mut idim_payload = Vec::with_capacity(9);
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.push(1u8); // Z-order
        buf.extend(write_chunk(b"iDIM", 0x00, &idim_payload));
        // Morton order for a 2x2 grid is the same as row-major: (0,0),(1,0),(0,1),(1,1).
        for (tx, ty) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
            let raw = tile_at(tx, ty);
            let payload = encode_tile_rows(&raw, 2, 2, 1, PREDICTOR_NONE).unwrap();
            buf.extend(write_chunk(b"IDAT", 0x00, &payload));
        }
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, full);
    }

    #[test]
    fn test_decode_multi_tile_with_partial_edge_tiles() {
        use crate::predictor::PREDICTOR_NONE;
        use crate::tile::encode_tile_rows;

        // 3x3 image, tile size 2x2 -> tiles_x=tiles_y=2 (ceil(3/2)), so the
        // right column and bottom row of tiles are only 1 pixel wide/tall.
        let width = 3u32;
        let height = 3u32;
        let ihdr = Ihdr {
            width,
            height,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: 0,
        };
        let full: Vec<u8> = (0..(width * height) as u8).collect();

        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        let mut idim_payload = Vec::with_capacity(9);
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.extend_from_slice(&2u16.to_be_bytes());
        idim_payload.push(0u8);
        buf.extend(write_chunk(b"iDIM", 0x00, &idim_payload));

        // Tile rects (row-major order): (0,0)=2x2, (1,0)=1x2, (0,1)=2x1, (1,1)=1x1.
        let rects = [
            (0u32, 0u32, 2u32, 2u32),
            (2, 0, 1, 2),
            (0, 2, 2, 1),
            (2, 2, 1, 1),
        ];
        for (ox, oy, tw, th) in rects {
            let mut raw = Vec::new();
            for r in 0..th {
                for c in 0..tw {
                    raw.push(full[((oy + r) * width + (ox + c)) as usize]);
                }
            }
            let payload = encode_tile_rows(&raw, th, tw, 1, PREDICTOR_NONE).unwrap();
            buf.extend(write_chunk(b"IDAT", 0x00, &payload));
        }
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, full);
    }

    #[test]
    fn test_decode_rejects_missing_idat() {
        let ihdr = gray_2x2_ihdr();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_rejects_missing_iend() {
        let ihdr = gray_2x2_ihdr();
        let idat_payload = vec![0u8, 10, 20, 0u8, 30, 40];
        let buf = build_file(&ihdr, 0x00, &idat_payload, None, false);
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_skips_unrecognized_ancillary_chunk() {
        let ihdr = gray_2x2_ihdr();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"zzzz", 0x00, b"unknown ancillary data"));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![10, 20, 30, 40]);
    }

    #[test]
    fn test_decode_rejects_unrecognized_critical_chunk() {
        let ihdr = gray_2x2_ihdr();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"ZZZZ", 0x00, b"unknown critical data"));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(
                cafe_format::CafeError::UnsupportedFeature(_)
            ))
        ));
    }

    #[test]
    fn test_decode_zstd_compressed_idat_roundtrip() {
        // Large enough (32x32, constant gray value) that the ZSTD
        // candidate reliably beats raw despite per-frame overhead (spec
        // section 3.2's fallback only picks ZSTD when it actually wins) —
        // a tiny tile's ZSTD frame overhead can exceed its raw size.
        let width = 32u32;
        let height = 32u32;
        let ihdr = Ihdr {
            width,
            height,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_GRAY,
            compression_method: COMPRESSION_METHOD_ZSTD_BIT,
        };
        let mut raw_payload = Vec::new();
        for _ in 0..height {
            raw_payload.push(0u8); // predictor: None
            raw_payload.extend(std::iter::repeat_n(42u8, width as usize));
        }
        let (flag, compressed) =
            crate::zstd_codec::compress_with_fallback(&raw_payload, 19).unwrap();
        assert_eq!(flag, crate::zstd_codec::FLAG_ZSTD);
        let buf = build_file(&ihdr, flag, &compressed, None, true);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![42u8; (width * height) as usize]);
    }

    #[test]
    fn test_decode_rgba_with_paeth_predictor_roundtrip() {
        use crate::predictor::PREDICTOR_PAETH;
        use crate::tile::encode_tile_rows;

        let ihdr = Ihdr {
            width: 3,
            height: 3,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGBA,
            compression_method: 0,
        };
        let raw: Vec<u8> = (0..(3 * 3 * 4)).map(|i| ((i * 13) % 251) as u8).collect();
        let bytes_per_row = 3 * 4;
        let idat_payload = encode_tile_rows(&raw, 3, bytes_per_row, 4, PREDICTOR_PAETH).unwrap();
        let buf = build_file(&ihdr, 0x00, &idat_payload, None, true);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_decode_plte_roundtrip() {
        use crate::predictor::PREDICTOR_NONE;
        use crate::tile::encode_tile_rows;
        use cafe_format::constants::COLOR_TYPE_RGB;
        use cafe_format::Plte;

        // 2x2 RGB image, 2 distinct colors: red, green, red, green.
        let ihdr = Ihdr {
            width: 2,
            height: 2,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGB,
            compression_method: 0,
        };
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &[255, 0, 0, 0, 255, 0]).unwrap();
        // Indices: row0=[0,1], row1=[0,1], bpp=1 (one index byte/pixel).
        let indices = vec![0u8, 1, 0, 1];
        let idat_payload = encode_tile_rows(&indices, 2, 2, 1, PREDICTOR_NONE).unwrap();

        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(plte.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &idat_payload));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![255, 0, 0, 0, 255, 0, 255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn test_decode_rejects_duplicate_plte_chunk() {
        use cafe_format::constants::COLOR_TYPE_RGB;
        use cafe_format::Plte;

        let ihdr = Ihdr {
            width: 1,
            height: 1,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGB,
            compression_method: 0,
        };
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &[255, 0, 0]).unwrap();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(plte.to_chunk_bytes());
        buf.extend(plte.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 0]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_plte_appearing_after_first_idat() {
        use cafe_format::constants::COLOR_TYPE_RGB;
        use cafe_format::Plte;

        let ihdr = Ihdr {
            width: 1,
            height: 1,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGB,
            compression_method: 0,
        };
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &[255, 0, 0]).unwrap();
        // Without PLTE parsed yet, the decoder treats bpp as 3 (RGB direct)
        // for this first IDAT: one predictor byte + 3 sample bytes.
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 0, 0, 0]));
        buf.extend(plte.to_chunk_bytes());
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::TilingMismatch(_))
        ));
    }

    #[test]
    fn test_decode_rejects_out_of_range_palette_index() {
        use crate::predictor::PREDICTOR_NONE;
        use crate::tile::encode_tile_rows;
        use cafe_format::constants::COLOR_TYPE_RGB;
        use cafe_format::Plte;

        let ihdr = Ihdr {
            width: 1,
            height: 1,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGB,
            compression_method: 0,
        };
        // Only 1 entry (index 0 valid), but the IDAT references index 5.
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &[255, 0, 0]).unwrap();
        let idat_payload = encode_tile_rows(&[5u8], 1, 1, 1, PREDICTOR_NONE).unwrap();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(plte.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &idat_payload));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::InvalidPaletteIndex(5))
        ));
    }

    #[test]
    fn test_decode_rejects_plte_with_inconsistent_bit_depth() {
        use cafe_format::constants::COLOR_TYPE_RGB;
        use cafe_format::Plte;

        let ihdr = Ihdr {
            width: 1,
            height: 1,
            bit_depth: 16, // PLTE requires bit_depth=8
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGB,
            compression_method: 0,
        };
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &[255, 0, 0]).unwrap();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(plte.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 0]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));
        assert!(matches!(
            decode_bytes(&buf),
            Err(CodecError::Format(cafe_format::CafeError::InvalidPlte(_)))
        ));
    }

    #[test]
    fn test_decode_parses_exif_json_iccp_xmpd_chunks() {
        let ihdr = gray_2x2_ihdr();
        let json = JsonChunk::new("com.example", "{\"a\":1}").unwrap();
        let xmpd = Xmpd::new("<x:xmpmeta></x:xmpmeta>");
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"eXIF", 0x00, b"fake exif bytes"));
        buf.extend(json.to_chunk_bytes());
        buf.extend(write_chunk(b"iCCP", 0x00, b"fake icc profile bytes"));
        buf.extend(xmpd.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![10, 20, 30, 40]);
        assert_eq!(img.exif.as_deref(), Some(b"fake exif bytes".as_slice()));
        assert_eq!(img.json_chunks, vec![json]);
        assert_eq!(
            img.icc_profile.as_deref(),
            Some(b"fake icc profile bytes".as_slice())
        );
        assert_eq!(img.xmp, Some(xmpd));
    }

    #[test]
    fn test_decode_keeps_only_first_instance_of_single_instance_metadata_chunks() {
        // Spec section 4.5: "a decoder finding more than one must consider
        // only the first" — applied consistently here to eXIF/iCCP/xMPd.
        let ihdr = gray_2x2_ihdr();
        let xmpd_first = Xmpd::new("first");
        let xmpd_second = Xmpd::new("second");
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"eXIF", 0x00, b"first exif"));
        buf.extend(write_chunk(b"eXIF", 0x00, b"second exif"));
        buf.extend(write_chunk(b"iCCP", 0x00, b"first icc"));
        buf.extend(write_chunk(b"iCCP", 0x00, b"second icc"));
        buf.extend(xmpd_first.to_chunk_bytes());
        buf.extend(xmpd_second.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.exif.as_deref(), Some(b"first exif".as_slice()));
        assert_eq!(img.icc_profile.as_deref(), Some(b"first icc".as_slice()));
        assert_eq!(img.xmp, Some(xmpd_first));
    }

    #[test]
    fn test_decode_keeps_multiple_json_chunks_in_file_order() {
        // Spec section 4.6: jSON is the only metadata chunk type allowed
        // to repeat.
        let ihdr = gray_2x2_ihdr();
        let json_a = JsonChunk::new("a.namespace", "{\"n\":1}").unwrap();
        let json_b = JsonChunk::new("b.namespace", "{\"n\":2}").unwrap();
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(json_a.to_chunk_bytes());
        buf.extend(json_b.to_chunk_bytes());
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.json_chunks, vec![json_a, json_b]);
    }

    #[test]
    fn test_decode_silently_discards_malformed_json_content() {
        // Spec section 8.4: malformed ancillary *content* must not
        // invalidate the file — this jSON payload has invalid JSON
        // syntax (unterminated object) but valid chunk framing, so
        // decoding as a whole must still succeed with the chunk simply
        // absent from json_chunks.
        let ihdr = gray_2x2_ihdr();
        let mut json_payload = vec![b"ns".len() as u8];
        json_payload.extend_from_slice(b"ns");
        json_payload.extend_from_slice(b"{not valid json");
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"jSON", 0x00, &json_payload));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![10, 20, 30, 40]);
        assert!(img.json_chunks.is_empty());
    }

    #[test]
    fn test_decode_silently_discards_non_utf8_xmpd_content() {
        // Spec section 8.4: a non-UTF-8 xMPd payload is a content-level
        // problem, discarded, not a decode-aborting error.
        let ihdr = gray_2x2_ihdr();
        let invalid_utf8: &[u8] = &[0xFF, 0xFE, 0xFD];
        let mut buf = SIGNATURE.to_vec();
        buf.extend(ihdr.to_chunk_bytes());
        buf.extend(write_chunk(b"xMPd", 0x00, invalid_utf8));
        buf.extend(write_chunk(b"IDAT", 0x00, &[0u8, 10, 20, 0u8, 30, 40]));
        buf.extend(write_chunk(b"IEND", 0x00, b""));

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, vec![10, 20, 30, 40]);
        assert_eq!(img.xmp, None);
    }

    #[test]
    fn test_decode_with_no_metadata_chunks_leaves_metadata_fields_empty() {
        let ihdr = gray_2x2_ihdr();
        let buf = build_file(&ihdr, 0x00, &[0u8, 10, 20, 0u8, 30, 40], None, true);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.exif, None);
        assert!(img.json_chunks.is_empty());
        assert_eq!(img.icc_profile, None);
        assert_eq!(img.xmp, None);
    }

    // --- decode_bytes_parallel parity tests -------------------------------
    //
    // `decode_bytes_parallel` shares `parse_chunks`/`decode_tile_into`/
    // `finish_decoded_image` with `decode_bytes`, so these tests focus
    // specifically on confirming the two produce identical output across a
    // range of tile counts (including counts below/above
    // `crate::parallel::MIN_TILES_FOR_PARALLEL`), not on re-testing framing
    // rules already covered above.

    fn encode_test_image(
        width: u32,
        height: u32,
        color_type: u8,
        tile_size: Option<(u16, u16)>,
    ) -> (Vec<u8>, Vec<u8>) {
        let channels = match color_type {
            COLOR_TYPE_GRAY => 1u32,
            COLOR_TYPE_RGBA => 4,
            other => panic!("unsupported color_type {other} in test helper"),
        };
        let raw: Vec<u8> = (0..(width * height * channels))
            .map(|i| ((i * 37 + 11) % 251) as u8)
            .collect();
        let buf = crate::encoder::encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            color_type,
            &raw,
            crate::encoder::EncoderOptions {
                tile_size,
                ..Default::default()
            },
        )
        .unwrap();
        (buf, raw)
    }

    #[test]
    fn test_decode_bytes_parallel_matches_sequential_single_tile() {
        let (buf, raw) = encode_test_image(4, 4, COLOR_TYPE_GRAY, None);
        let seq = decode_bytes(&buf).unwrap();
        let par = decode_bytes_parallel(&buf).unwrap();
        assert_eq!(seq, par);
        assert_eq!(seq.pixels, raw);
    }

    #[test]
    fn test_decode_bytes_parallel_matches_sequential_below_threshold_tile_count() {
        // 2x2 tiles of 3x2 pixels each = 4 tiles total, below
        // MIN_TILES_FOR_PARALLEL (falls back to sequential internally, but
        // must still match decode_bytes exactly).
        let (buf, raw) = encode_test_image(6, 4, COLOR_TYPE_GRAY, Some((3, 2)));
        let seq = decode_bytes(&buf).unwrap();
        let par = decode_bytes_parallel(&buf).unwrap();
        assert_eq!(seq, par);
        assert_eq!(seq.pixels, raw);
    }

    #[test]
    fn test_decode_bytes_parallel_matches_sequential_many_tiles() {
        // 8x8 tiles of 4x4 pixels each = 16 tiles, well above the
        // parallel threshold, RGBA to also exercise bpp > 1.
        let (buf, raw) = encode_test_image(32, 32, COLOR_TYPE_RGBA, Some((4, 4)));
        let seq = decode_bytes(&buf).unwrap();
        let par = decode_bytes_parallel(&buf).unwrap();
        assert_eq!(seq, par);
        assert_eq!(seq.pixels, raw);
    }

    #[test]
    fn test_decode_bytes_parallel_matches_sequential_with_partial_edge_tiles() {
        // 5x5 image with 2x2 tiles: tiles_x=tiles_y=3, right/bottom tiles
        // are partial.
        let (buf, raw) = encode_test_image(5, 5, COLOR_TYPE_GRAY, Some((2, 2)));
        let seq = decode_bytes(&buf).unwrap();
        let par = decode_bytes_parallel(&buf).unwrap();
        assert_eq!(seq, par);
        assert_eq!(seq.pixels, raw);
    }

    #[test]
    fn test_decode_bytes_parallel_propagates_same_errors_as_sequential() {
        let malformed = read_malformed("bad_signature.cafe");
        let seq_err = decode_bytes(&malformed).unwrap_err();
        let par_err = decode_bytes_parallel(&malformed).unwrap_err();
        assert_eq!(format!("{seq_err:?}"), format!("{par_err:?}"));
    }

    #[test]
    fn test_decode_bytes_parallel_matches_sequential_against_golden_fixture() {
        let buf = read_golden("minimal_2x2_rgba.cafe");
        let seq = decode_bytes(&buf).unwrap();
        let par = decode_bytes_parallel(&buf).unwrap();
        assert_eq!(seq, par);
    }
}
