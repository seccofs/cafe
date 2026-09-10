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
use cafe_format::{Idim, Plte};

/// A fully decoded CAFE image: the validated header plus raw, unpredicted
/// pixel bytes in row-major order (top-to-bottom), each sample stored per
/// `Ihdr::bit_depth`'s endianness (spec section 4.1: big-endian for
/// `bit_depth > 8`), channels interleaved per `Ihdr::color_type`'s order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub ihdr: Ihdr,
    pub pixels: Vec<u8>,
}

/// Decodes a whole in-memory `.cafe` file into pixel bytes.
///
/// Streaming (chunk-at-a-time, without requiring the whole file in memory)
/// is deferred to a `Decoder<R>` type in a later phase (mirrors
/// `cafe-format`'s Phase 4 decision to defer the `Read`-based chunk
/// primitive) — this is the reference, whole-buffer path golden files are
/// validated against.
pub fn decode_bytes(buf: &[u8]) -> Result<DecodedImage> {
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
    let mut layout: Option<TileLayout> = None;
    let mut pixels: Option<Vec<u8>> = None;
    let mut tiles_seen = 0usize;
    let mut saw_iend = false;

    // `PLTE` (spec section 4.3) changes IDAT's effective bpp to 1 (one
    // palette index per pixel) — this must be resolved before any IDAT is
    // decoded, but PLTE itself can only be parsed once we've seen its
    // bytes, so `effective_bpp`/`bytes_per_row` are computed lazily, right
    // before the first IDAT (mirroring `layout`'s own lazy construction
    // below), once we know whether a PLTE chunk preceded it.
    let mut effective_bpp: Option<u32> = None;
    let mut bytes_per_row: Option<u32> = None;

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
                if tiles_seen > 0 {
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
                if tiles_seen > 0 {
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
            b"IDAT" => {
                if layout.is_none() {
                    let this_bpp = if plte.is_some() { 1 } else { bpp };
                    let this_bytes_per_row = ihdr.width.checked_mul(this_bpp).ok_or_else(|| {
                        CodecError::Format(cafe_format::CafeError::TruncatedFile(
                            "overflow computing bytes_per_row (width * bpp)".into(),
                        ))
                    })?;
                    effective_bpp = Some(this_bpp);
                    bytes_per_row = Some(this_bytes_per_row);
                    let built = TileLayout::new(idim, ihdr.width, ihdr.height)?;
                    // Allocate the full assembled pixel buffer once, sized
                    // exactly to IHDR's declared dimensions — each tile's
                    // own decompression remains individually bounded below
                    // by that tile's own expected size, so this allocation
                    // is never larger than the sum of legitimately-bounded
                    // per-tile work the file's chunks actually justify.
                    let total_pixel_bytes = (ihdr.height as u64)
                        .checked_mul(this_bytes_per_row as u64)
                        .ok_or_else(|| {
                            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                                "overflow computing total pixel buffer size".into(),
                            ))
                        })?;
                    pixels = Some(vec![0u8; total_pixel_bytes as usize]);
                    layout = Some(built);
                }
                let this_bpp = effective_bpp.unwrap();
                let this_bytes_per_row = bytes_per_row.unwrap();
                let layout_ref = layout.as_ref().unwrap();
                if tiles_seen >= layout_ref.tile_count() {
                    return Err(CodecError::TilingMismatch(format!(
                        "file contains more IDAT chunks than iDIM declares tiles ({})",
                        layout_ref.tile_count()
                    )));
                }
                let (origin_x, origin_y, tile_w, tile_h) = layout_ref.tile_rect(tiles_seen);
                let tile_bytes_per_row = tile_w.checked_mul(this_bpp).ok_or_else(|| {
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

                let raw = decompress_with_limit(chunk.flag, &chunk.data, expected_tile_payload)?;
                let tile_pixels = decode_tile_rows(&raw, tile_h, tile_bytes_per_row, this_bpp)?;

                let pixels_buf = pixels.as_mut().unwrap();
                let row_bytes = (tile_w * this_bpp) as usize;
                for row in 0..tile_h {
                    let dst_start = (origin_y + row) as usize * this_bytes_per_row as usize
                        + origin_x as usize * this_bpp as usize;
                    let src_start = row as usize * tile_bytes_per_row as usize;
                    pixels_buf[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&tile_pixels[src_start..src_start + row_bytes]);
                }
                tiles_seen += 1;
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
                // Ancillary chunk this decoder doesn't yet interpret
                // (eXIF/jSON/iCCP/xMPd — deferred to a later phase, spec
                // section 8.4: safe to skip).
            }
        }
    }

    if !saw_iend {
        return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "file ended without an IEND chunk".into(),
        )));
    }

    let layout = layout.ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "file contains no IDAT chunk".into(),
        ))
    })?;
    if tiles_seen != layout.tile_count() {
        return Err(CodecError::TilingMismatch(format!(
            "iDIM declares {} tile(s), but only {tiles_seen} IDAT chunk(s) were present",
            layout.tile_count()
        )));
    }

    let pixels = pixels.ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "file contains no IDAT chunk".into(),
        ))
    })?;

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

    Ok(DecodedImage { ihdr, pixels })
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
}
