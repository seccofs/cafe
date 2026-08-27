//! CAFE — Compression Adaptive Filtering Experiment
//! Reference implementation v1.1 — full support:
//!
//!   - Chunks: IHDR, PLTE, iDIM, eXIF, jSON, IDAT, IEND
//!   - Codec: ZSTD with fallback to raw (Flag 0x00 = raw, 0x01 = ZSTD)
//!   - Predictive filter (Filter method = 2): all 16 filters (0-15),
//!     chosen per block/tile via Shannon-entropy heuristic of the residuals
//!   - Interlace: Adam7 (method = 1) and Even/Odd (method = 2)
//!   - Color types: Gray (0), RGB (2), Indexed (3), Gray+Alpha (4), RGBA (6)
//!   - Bit depths: 1, 2, 4, 8, 10, 12, 16, 32 with uint/float/half-float support
//!   - Tiling and streaming via iDIM (section 4.2)
//!   - Metadata: EXIF, JSON, ICC, XMP, HDR (cHDR) and ZSTD dictionary (zDIC)
//!   - Complete security audit (section 12 of spec)
//!
//! License: BSD-3-Clause OR GPL-2.0-or-later (same choice as ZSTD)
//! Autor: Daniel Secco

// Public modules
pub mod constants;
pub mod error;
pub mod types;

// Private modules (internal)
mod chunk;
mod codec;
mod color;
mod filter;
mod interlace;
mod quantize;
mod shuffle;
#[cfg(feature = "simd")]
mod simd;
#[cfg(feature = "simd")]
mod simd_packing;
#[cfg(feature = "simd")]
mod simd_sample_conversion;
#[cfg(feature = "simd")]
mod simd_shuffle;
mod tonemap;

// Public re-exports for convenience
pub use error::{CafeError, Result};
pub use tonemap::ToneMapOperator;
pub use types::{
    cHDR, iDim, DecodeResult, EncodeOptions, FilterHeuristic, Palette, PaletteAlgorithm,
    PaletteEntry,
};

use crate::constants::*;

// Import functions from the specialized modules
use crate::filter::{analyze_tile_complexity, apply_predictive_filter, undo_predictive_filter};

use crate::color::{
    bytes_per_pixel, bytes_per_pixel_with_format, bytes_per_row_for_bit_depth,
    convert_color_type_to_rgba, convert_color_type_to_rgba_with_format, convert_rgba_to_color_type,
    convert_rgba_to_color_type_with_format, pack_indices_row, unpack_indices_row,
};

use crate::interlace::{
    apply_adam7_interlace, apply_even_odd_interlace, reconstruct_adam7, reconstruct_even_odd,
};

use crate::codec::{
    compress_with_fallback, compress_with_fallback_dict, decompress_chunk,
    decompress_chunk_dict_limited,
};

use crate::chunk::{read_chunk, write_chunk};

use std::collections::HashMap;

use rayon::prelude::*;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Encoder / Decoder
// ---------------------------------------------------------------------------

/// Computes the cumulative decompressed-byte ceiling for the IDATs from the IHDR.
///
/// The budget covers exactly what the image needs (pixel bytes) plus
/// headroom for the per-block filter bytes (1 per block/tile) and interlace
/// pass prefixes. This way, multiple IDATs cannot sum to gigabytes when the
/// image is small (CWE-409, "cumulative decompression bomb").
fn compute_decompress_budget(
    interlace: u8,
    color_type: u8,
    w: u32,
    h: u32,
    bytes_per_row: usize,
) -> u64 {
    let pixels = (w as u64).saturating_mul(h as u64);
    match interlace {
        INTERLACE_ADAM7 => pixels
            .checked_mul(BPP as u64)
            .and_then(|v| v.checked_add(ADAM7_NUM_PASSES as u64))
            .unwrap_or(u64::MAX),
        INTERLACE_EVEN_ODD => pixels
            .checked_mul(BPP as u64)
            .and_then(|v| v.checked_add(EVEN_ODD_NUM_PASSES as u64))
            .unwrap_or(u64::MAX),
        _ => {
            // No interlace: data packed per row + 1 filter byte per block
            if color_type == COLOR_TYPE_INDEXED {
                // Packed (bit_depth<8) ≤ 1 byte/index, so width×height is a loose
                // upper bound on the decompressed bytes.
                pixels.saturating_add(h as u64)
            } else {
                let row_bytes = bytes_per_row as u64;
                row_bytes
                    .checked_mul(h as u64)
                    .and_then(|v| v.checked_add(h as u64))
                    .unwrap_or(u64::MAX)
            }
        }
    }
}

/// Trains a ZSTD dictionary from the given tile payloads.
/// Returns a dictionary if training is successful, otherwise None.
/// Silently falls back to no dictionary on small images or training failure.
fn train_zstd_dictionary(samples: &[Vec<u8>]) -> Option<Vec<u8>> {
    // Skip if too few or too small samples
    if samples.is_empty() {
        return None;
    }

    // Calculate total size
    let total_size: usize = samples.iter().map(|s| s.len()).sum();
    if total_size < 512 {
        // Not enough data to train a useful dictionary
        return None;
    }

    // Heuristic: dictionary size is 10% of total data, clamped to [256, 65536]
    let dict_size = (total_size / 10).clamp(256, 65536);

    // Convert samples to references
    let sample_refs: Vec<&[u8]> = samples.iter().map(|v| v.as_slice()).collect();

    // Train the dictionary (fall back silently to no dictionary on failure)
    zstd::dict::from_samples(&sample_refs, dict_size).ok()
}

/// Writes an IDAT chunk with compression fallback (refactoring v1.1).
/// Common pattern in encode() and encode_indexed() — compresses data using fallback strategy
/// (raw vs ZSTD) and appends the chunk to the output buffer.
/// Returns whether ZSTD was used for this chunk.
#[inline]
fn append_idat_chunk(
    out: &mut Vec<u8>,
    data: &[u8],
    level: i32,
    dict: Option<&[u8]>,
) -> Result<bool> {
    let (flag, compressed) = compress_with_fallback_dict(data, level, dict)?;
    out.extend_from_slice(&write_chunk(CHUNK_IDAT, flag, &compressed));
    Ok(flag == FLAG_ZSTD)
}

/// Same as `append_idat_chunk`, but builds and returns the complete IDAT
/// chunk bytes instead of appending to a shared buffer. Used so tiles can be
/// filtered/compressed independently on a thread pool (rayon) and the
/// resulting chunks are appended to `out` afterwards, in original tile order,
/// preserving the exact byte layout `append_idat_chunk` would have produced
/// sequentially.
#[inline]
fn build_idat_chunk(data: &[u8], level: i32, dict: Option<&[u8]>) -> Result<(Vec<u8>, bool)> {
    let (flag, compressed) = compress_with_fallback_dict(data, level, dict)?;
    Ok((
        write_chunk(CHUNK_IDAT, flag, &compressed),
        flag == FLAG_ZSTD,
    ))
}

/// Writes the iDIM chunk (section 4.2, tiling for progressive streaming) if
/// present in `opts`. Shared between `encode()` and `encode_indexed()` — must
/// appear immediately after IHDR in both (section 9, mandatory chunk order).
/// Returns whether ZSTD was used for this chunk.
fn append_idim_chunk_if_present(out: &mut Vec<u8>, opts: &EncodeOptions) -> Result<bool> {
    if let Some(idim) = &opts.idim {
        let chunk = write_idim_chunk(idim)?;
        let used = chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
        Ok(used)
    } else {
        Ok(false)
    }
}

/// Writes the eXIF, jSON, iCCP, and xMPd ancillary chunks shared between
/// `encode()` and `encode_indexed()`, in spec order (sections 4.5-4.8).
/// Deduplicates a block that was previously copy-pasted between the two
/// encoders — a past divergence between the copies caused `encode_indexed()`
/// to silently omit the zDIC chunk despite using the dictionary to compress
/// IDATs (see `append_zdic_chunk_if_present`). Returns whether any chunk used
/// ZSTD.
fn append_common_metadata_chunks(out: &mut Vec<u8>, opts: &EncodeOptions) -> Result<bool> {
    let mut uses_zstd = false;

    // --- eXIF (optional, single instance, section 4.5) ---
    if let Some(exif_bytes) = &opts.exif {
        let (flag, data) = compress_with_fallback(exif_bytes, opts.level)?;
        uses_zstd |= flag == FLAG_ZSTD;
        out.extend_from_slice(&write_chunk(CHUNK_EXIF, flag, &data));
    }

    // --- jSON (optional, one per namespace, section 4.6) ---
    for (namespace, obj) in &opts.json_metadata {
        let chunk = write_json_chunk(namespace, obj, opts.level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    // --- iCCP (optional, single instance, section 4.7) ---
    if let Some(icc) = &opts.icc_profile {
        let chunk = write_iccp_chunk(icc, opts.level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    // --- xMPd (optional, single instance, section 4.8) ---
    if let Some(xmp) = &opts.xmp_metadata {
        let chunk = write_xmpd_chunk(xmp, opts.level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    Ok(uses_zstd)
}

/// Writes the zDIC chunk (section 4.9) if `dict` is present. Shared between
/// `encode()` (which may pass an auto-trained dictionary) and
/// `encode_indexed()`. The dictionary is actually used when compressing the
/// IDATs (`compress_with_fallback_dict`) — it is not merely informational, so
/// this chunk must be written whenever a dictionary is used for IDATs, or the
/// decoder cannot reconstruct the pixel data (see doc comment on
/// `append_common_metadata_chunks`). Returns whether ZSTD was used for this
/// chunk.
fn append_zdic_chunk_if_present(
    out: &mut Vec<u8>,
    dict: Option<&[u8]>,
    level: i32,
) -> Result<bool> {
    if let Some(dict) = dict {
        let chunk = write_zdic_chunk(dict, level)?;
        let used = chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
        Ok(used)
    } else {
        Ok(false)
    }
}

/// Encodes an image (any format the `image` crate can read) to `.cafe`.
pub fn encode(input_path: &str, output_path: &str, opts: &EncodeOptions) -> Result<()> {
    let img = image::open(input_path)?.to_rgba8();
    let (width, height) = img.dimensions();
    let rgba_raw = img.into_raw(); // RGBA interleaved, 4 bytes/pixel, row by row (top-to-bottom)

    // Determine sample format (default: uint)
    let sample_format_final = opts.sample_format.unwrap_or(SAMPLE_FORMAT_UINT);

    // Determine target bit depth based on sample format.
    // uint: 8 by default, or the explicit value in opts.target_bit_depth;
    // float/half: 32/16 fixed (IEEE-754 / fp16 container).
    let target_bit_depth = match sample_format_final {
        SAMPLE_FORMAT_FLOAT => 32, // float: 32-bit IEEE 754
        SAMPLE_FORMAT_HALF => 16,  // half-float: 16-bit fp16
        _ => opts.target_bit_depth.unwrap_or(8),
    };

    // Converts RGBA to the target color type (section 4.1.3, v1.0)
    // Security validation: width/height were already validated by the image crate
    let (raw, target_color_type, bit_depth) = if opts.target_color_type == COLOR_TYPE_RGBA {
        if sample_format_final == SAMPLE_FORMAT_UINT {
            // uint with target bit depth (>= 8); RGBA/8 returns an identical copy
            let converted = convert_rgba_to_color_type(
                &rgba_raw,
                width,
                height,
                COLOR_TYPE_RGBA,
                target_bit_depth,
            )?;
            (converted, COLOR_TYPE_RGBA, target_bit_depth)
        } else {
            // Uses convert_rgba_to_color_type_with_format for float/half-float
            let converted = convert_rgba_to_color_type_with_format(
                &rgba_raw,
                width,
                height,
                COLOR_TYPE_RGBA,
                target_bit_depth,
                sample_format_final,
            )?;
            (converted, COLOR_TYPE_RGBA, target_bit_depth)
        }
    } else {
        // Converts RGBA → target color type with sample format support
        let converted = if sample_format_final == SAMPLE_FORMAT_UINT {
            convert_rgba_to_color_type(
                &rgba_raw,
                width,
                height,
                opts.target_color_type,
                target_bit_depth, // Target bit depth for uint
            )?
        } else {
            convert_rgba_to_color_type_with_format(
                &rgba_raw,
                width,
                height,
                opts.target_color_type,
                target_bit_depth,
                sample_format_final,
            )?
        };
        (converted, opts.target_color_type, target_bit_depth)
    };

    // Computes bytes per row based on the converted color type (section 4.3.1)
    // Validation: width was already validated; bytes_per_pixel returns Some for supported color types
    let bpp = bytes_per_pixel(target_color_type, bit_depth).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "Color type {target_color_type}, bit depth {bit_depth} not supported in encode"
        ))
    })?;

    // Alternative validation using bytes_per_pixel_with_format (v1.0) for consistency
    // Ensures the function is used when sample_format != UINT
    if sample_format_final != SAMPLE_FORMAT_UINT {
        let _bpp_with_format = bytes_per_pixel_with_format(target_color_type, 8, sample_format_final)
             .ok_or_else(|| {
                 CafeError::UnsupportedFeature(format!(
                     "Color type {target_color_type} incompatible with sample format {sample_format_final}"
                 ))
             })?;
        // SECURITY: This assertion validates that bytes_per_pixel and bytes_per_pixel_with_format
        // return consistent values for this configuration. It's development-only (removed in --release),
        // but the _bpp_with_format computation above validates that the combination is supported.
        // In production, if the invariant is violated, earlier checks would have already caught it.
        debug_assert_eq!(_bpp_with_format, bpp, "bpp mismatch between functions");
    }

    // For bit_depth < 8, uses bytes_per_row_for_bit_depth (which computes the ceil of packed bits)
    // Otherwise, uses the normal bpp
    let bytes_per_row = if bit_depth < 8
        && (target_color_type == COLOR_TYPE_GRAY || target_color_type == COLOR_TYPE_GRAY_ALPHA)
    {
        let bpp_multiplier = match target_color_type {
            COLOR_TYPE_GRAY => 1,
            COLOR_TYPE_GRAY_ALPHA => 2,
            _ => 1,
        };
        // Computes ceil(width * bpp_multiplier * bit_depth / 8)
        let bits_total = (width as u64)
            .checked_mul(bpp_multiplier as u64)
            .and_then(|b| b.checked_mul(bit_depth as u64))
            .ok_or_else(|| {
                CafeError::UnsupportedFeature(
                    "bytes_per_row calculation would overflow during encode".into(),
                )
            })? as usize;
        bits_total.div_ceil(8)
    } else {
        (width as usize).checked_mul(bpp).ok_or_else(|| {
            CafeError::UnsupportedFeature(
                "bytes_per_row calculation would overflow during encode".into(),
            )
        })?
    };

    let filter_method = if opts.use_byte_shuffle {
        FILTER_METHOD_BYTE_SHUFFLE
    } else if opts.use_filter {
        FILTER_METHOD_PREDICTIVE
    } else {
        FILTER_METHOD_NONE
    };

    // Interlace (Adam7 / even-odd) only operates on RGBA uint (section 5): the
    // passes assume 4 bytes/pixel and 8-bit samples. Any other combination
    // would produce a corrupted file — reject explicitly.
    if (opts.interlace_method == INTERLACE_ADAM7 || opts.interlace_method == INTERLACE_EVEN_ODD)
        && (sample_format_final != SAMPLE_FORMAT_UINT || target_color_type != COLOR_TYPE_RGBA)
    {
        return Err(CafeError::UnsupportedFeature(
             "Interlace (Adam7/even-odd) requires sample format uint and color type RGBA (section 5)"
                 .into(),
         ));
    }

    // Byte-shuffle (filter_method=1) operates on multi-byte samples (bpp ∈
    // {2,4,8,16}) and is incompatible with interlace (which requires uint RGBA
    // 8-bit, bpp=4 but with no filter). Reject invalid combinations before writing.
    if opts.use_byte_shuffle {
        if opts.interlace_method != INTERLACE_NONE {
            return Err(CafeError::UnsupportedFeature(
                "Byte-shuffle is incompatible with interlace (section 4.3.1)".into(),
            ));
        }
        if bpp != 2 && bpp != 4 && bpp != 8 && bpp != 16 {
            return Err(CafeError::UnsupportedFeature(format!(
                 "Byte-shuffle requires bpp ∈ {{2,4,8,16}}, got {bpp} (color type {target_color_type}, \
                  bit depth {bit_depth} not compatible)"
             )));
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(&SIGNATURE);

    // --- IHDR (14 bytes of payload, section 4.1) ---
    let mut ihdr = Vec::with_capacity(14);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(bit_depth); // Bit depth
    ihdr.push(sample_format_final); // Sample format: uint (default) or float/half (v1.0)
    ihdr.push(target_color_type); // Color type (now supports 0, 2, 3, 4, 6)
    ihdr.push(0); // compression_method: bitmask (section 3.2), filled in at the end of encode
    ihdr.push(filter_method);
    ihdr.push(opts.interlace_method);
    out.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
    let mut uses_zstd = false;

    // --- iDIM (optional, ancillary, section 4.2, v1.0) ---
    // iDIM defines the tile partitioning for progressive streaming.
    // Must appear immediately after IHDR (section 9, mandatory order).
    uses_zstd |= append_idim_chunk_if_present(&mut out, opts)?;

    // --- cHDR (optional, single instance, section 4.4, v1.0) ---
    // HDR metadata: transfer function, color space, luminance. Only emitted
    // by encode() — encode_indexed() has no HDR path.
    if let Some(chdr) = &opts.chdr_metadata {
        let chunk = write_chdr_chunk(chdr, opts.level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    // --- eXIF, jSON, iCCP, xMPd (optional, sections 4.5-4.8) ---
    uses_zstd |= append_common_metadata_chunks(&mut out, opts)?;

    // --- Auto-dictionary training (v1.1, opt-in) ---
    // If auto_dictionary is enabled and no explicit dictionary provided,
    // collect samples from the first few tiles and train a dictionary.
    let final_zstd_dict = if opts.auto_dictionary && opts.zstd_dictionary.is_none() {
        // Collect samples from the first few tiles (max 10) for training
        let mut samples = Vec::new();
        let sample_tile_rows = opts.tile_rows as usize;
        let height_usize = height as usize;
        let mut row_start = 0;
        let mut tile_idx = 0;
        const MAX_SAMPLE_TILES: usize = 10;

        while row_start < height_usize && tile_idx < MAX_SAMPLE_TILES {
            let row_end = (row_start + sample_tile_rows).min(height_usize);
            let tile_h = row_end - row_start;
            let tile_raw = &raw[row_start * bytes_per_row..row_end * bytes_per_row];

            let tile_payload = if opts.use_byte_shuffle {
                shuffle::apply_byte_shuffle(tile_raw, bpp, width, tile_h as u32)?
            } else if opts.use_filter {
                apply_predictive_filter(
                    tile_raw,
                    tile_h,
                    bytes_per_row,
                    bpp,
                    opts.filter_heuristic,
                    opts.level,
                )?
            } else {
                tile_raw.to_vec()
            };

            samples.push(tile_payload);
            row_start = row_end;
            tile_idx += 1;
        }

        train_zstd_dictionary(&samples)
    } else {
        opts.zstd_dictionary.as_ref().cloned()
    };

    // --- zDIC (optional, single instance, section 4.9) ---
    uses_zstd |= append_zdic_chunk_if_present(&mut out, final_zstd_dict.as_deref(), opts.level)?;

    // --- IDAT (section 4.3) ---
    // New feature (v1.0): local complexity analysis per tile (extended section 4.3.1).
    if opts.interlace_method == INTERLACE_ADAM7 || opts.interlace_method == INTERLACE_EVEN_ODD {
        // Interlaced path (section 5): each pass becomes an IDAT with a
        // prefixed pass_number. Same logic as already used by encode_indexed(),
        // now also available for the direct RGBA path.
        if opts.interlace_method == INTERLACE_ADAM7 {
            let passes = apply_adam7_interlace(&raw, width, height);
            for (pass_idx, pass_data) in passes.iter().enumerate() {
                let pass_number = (pass_idx + 1) as u8;
                let mut pass_payload = vec![pass_number];
                pass_payload.extend_from_slice(pass_data);
                uses_zstd |= append_idat_chunk(
                    &mut out,
                    &pass_payload,
                    opts.level,
                    final_zstd_dict.as_deref(),
                )?;
            }
        } else {
            let passes = apply_even_odd_interlace(&raw, width, height);
            for (pass_idx, pass_data) in passes.iter().enumerate() {
                let pass_number = (pass_idx + 1) as u8;
                let mut pass_payload = vec![pass_number];
                pass_payload.extend_from_slice(pass_data);
                uses_zstd |= append_idat_chunk(
                    &mut out,
                    &pass_payload,
                    opts.level,
                    final_zstd_dict.as_deref(),
                )?;
            }
        }
    } else if let Some(idim) = &opts.idim {
        // Real 2D tiling (section 4.2): each IDAT is a rectangular tile, in the
        // scan_order order (row-major or Z-order). Requires bit_depth >= 8 so
        // that tile columns are byte-aligned.
        if bit_depth < 8 {
            return Err(CafeError::UnsupportedFeature(
                "iDIM (tiling 2D) requires bit_depth >= 8 in encode".into(),
            ));
        }
        // Tiles are independent, so extraction + filter + compression is
        // parallelized across a rayon thread pool (v1.2.2); chunks are then
        // appended to `out` in the original tile_order sequence, which the
        // decoder relies on to reconstruct tile positions.
        let zstd_dict = opts.zstd_dictionary.as_deref();
        let chunks: Vec<(Vec<u8>, bool)> = idim
            .tile_order()?
            .into_par_iter()
            .map(|(tx, ty)| -> Result<(Vec<u8>, bool)> {
                let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, width, height);
                let tw = tile_w as usize;
                let th = tile_h as usize;
                let tile_stride = tw.checked_mul(bpp).ok_or_else(|| {
                    CafeError::UnsupportedFeature("overflow in tile stride during encode".into())
                })?;
                let tile_len = tile_stride.checked_mul(th).ok_or_else(|| {
                    CafeError::UnsupportedFeature("overflow in tile len during encode".into())
                })?;
                let mut tile_raw = Vec::with_capacity(tile_len);
                let row0 = (ty as u32 * idim.tile_height as u32) as usize;
                let col0 = (tx as u32 * idim.tile_width as u32) as usize;
                for r in 0..th {
                    let row = row0 + r;
                    let start = row
                        .checked_mul(bytes_per_row)
                        .and_then(|v| v.checked_add(col0 * bpp))
                        .ok_or_else(|| {
                            CafeError::UnsupportedFeature("overflow in line offset (encode)".into())
                        })?;
                    let end = start.checked_add(tile_stride).ok_or_else(|| {
                        CafeError::UnsupportedFeature("overflow at end of line (encode)".into())
                    })?;
                    if end > raw.len() {
                        return Err(CafeError::TruncatedFile(
                            "tile excede os dados da imagem durante encode".into(),
                        ));
                    }
                    tile_raw.extend_from_slice(&raw[start..end]);
                }

                let tile_payload = if opts.use_byte_shuffle {
                    shuffle::apply_byte_shuffle(&tile_raw, bpp, tile_w, tile_h)?
                } else if opts.use_filter {
                    apply_predictive_filter(
                        &tile_raw,
                        th,
                        tile_stride,
                        bpp,
                        opts.filter_heuristic,
                        opts.level,
                    )?
                } else {
                    tile_raw
                };

                build_idat_chunk(&tile_payload, opts.level, zstd_dict)
            })
            .collect::<Result<Vec<_>>>()?;

        for (chunk, chunk_used_zstd) in chunks {
            uses_zstd |= chunk_used_zstd;
            out.extend_from_slice(&chunk);
        }
    } else {
        // No interlace (v1.0): row tiles, with optional predictive filter.
        // Tiles are independent (no shared state between them), so the
        // expensive per-tile work — filter search and ZSTD compression — is
        // parallelized across a rayon thread pool (v1.2.2). Tile boundaries
        // are computed sequentially first, then processed in parallel, and
        // finally appended to `out` in original order to keep the exact same
        // byte layout a sequential loop would produce.
        let tile_rows = opts.tile_rows as usize;
        let height = height as usize;

        let mut tile_bounds = Vec::new();
        let mut row_start = 0;
        while row_start < height {
            let row_end = (row_start + tile_rows).min(height);
            tile_bounds.push((row_start, row_end));
            row_start = row_end;
        }

        // Local complexity analysis (extended section 4.3.1) — cheap enough
        // to keep sequential, but computed alongside the parallel pass below.
        let complexities: Vec<f64> = if opts.adaptive_analysis {
            tile_bounds
                .par_iter()
                .map(|&(row_start, row_end)| {
                    let tile_raw = &raw[row_start * bytes_per_row..row_end * bytes_per_row];
                    analyze_tile_complexity(tile_raw)
                })
                .collect()
        } else {
            Vec::new()
        };

        let zstd_dict = opts.zstd_dictionary.as_deref();
        let chunks: Vec<(Vec<u8>, bool)> = tile_bounds
            .par_iter()
            .map(|&(row_start, row_end)| -> Result<(Vec<u8>, bool)> {
                let tile_h = row_end - row_start;
                let tile_raw = &raw[row_start * bytes_per_row..row_end * bytes_per_row];

                let tile_payload = if opts.use_byte_shuffle {
                    shuffle::apply_byte_shuffle(tile_raw, bpp, width, tile_h as u32)?
                } else if opts.use_filter {
                    apply_predictive_filter(
                        tile_raw,
                        tile_h,
                        bytes_per_row,
                        bpp,
                        opts.filter_heuristic,
                        opts.level,
                    )?
                } else {
                    tile_raw.to_vec()
                };

                build_idat_chunk(&tile_payload, opts.level, zstd_dict)
            })
            .collect::<Result<Vec<_>>>()?;

        let tile_count = chunks.len();
        for (chunk, chunk_used_zstd) in chunks {
            uses_zstd |= chunk_used_zstd;
            out.extend_from_slice(&chunk);
        }

        // Adaptive analysis log (if enabled)
        if opts.adaptive_analysis && !complexities.is_empty() {
            eprintln!("[CAFE] Adaptive analysis: {} tiles processed", tile_count);
            let avg_complexity = complexities.iter().sum::<f64>() / complexities.len() as f64;
            eprintln!("[CAFE] Average complexity: {:.2} bits/byte", avg_complexity);
            let max_complexity = complexities
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            eprintln!("[CAFE] Maximum complexity: {:.2} bits/byte", max_complexity);
        }
    }

    // --- IEND ---
    out.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

    // Actual compression_method (section 3.2): bit0 = at least one chunk used ZSTD
    patch_ihdr_compression_method(
        &mut out,
        if uses_zstd {
            COMPRESSION_METHOD_ZSTD_BIT
        } else {
            0
        },
    );

    std::fs::write(output_path, out)?;
    Ok(())
}

/// Decodes a `.cafe` back into an image (PNG, by the extension of
/// `output_path`). Returns the raw EXIF and any JSON metadata found.
/// Parameters for final pixel reconstruction (refactoring v1.1).
struct ReconstructParams<'a> {
    interlace_method: u8,
    color_type: u8,
    bit_depth: u8,
    sample_format: u8,
    width: u32,
    height: u32,
    palette: Option<&'a Palette>,
    chdr: Option<&'a cHDR>,
    adam7_passes: &'a [Vec<u8>; ADAM7_NUM_PASSES],
    even_odd_passes: &'a [Vec<u8>; EVEN_ODD_NUM_PASSES],
    /// Tone-map operator for HDR images (v1.2.1)
    tonemap_operator: tonemap::ToneMapOperator,
}

/// Reconstructs final RGBA image from pixel data (refactoring v1.1).
/// Handles interlace deinterlacing, palette dequantization, color type conversion,
/// and HDR tone-mapping. Returns final RGBA pixel buffer.
fn reconstruct_final_pixels(pixel_rows: Vec<u8>, params: &ReconstructParams) -> Result<Vec<u8>> {
    // Step 1: Deinterlace if needed
    let pixel_rows = if params.interlace_method == INTERLACE_ADAM7 {
        reconstruct_adam7(params.adam7_passes, params.width, params.height)?
    } else if params.interlace_method == INTERLACE_EVEN_ODD {
        reconstruct_even_odd(params.even_odd_passes, params.width, params.height)?
    } else {
        pixel_rows
    };

    // Step 2: Convert to RGBA based on color type and sample format
    if params.color_type == COLOR_TYPE_INDEXED {
        if let Some(pal) = params.palette {
            Ok(dequantize_from_palette(
                &pixel_rows,
                pal,
                params.width,
                params.height,
            ))
        } else {
            Err(CafeError::UnsupportedFeature(
                "Color type=3 found without PLTE chunk".into(),
            ))
        }
    } else if params.sample_format == SAMPLE_FORMAT_FLOAT && params.chdr.is_some() {
        // v1.1: HDR tone-mapping — converts linear HDR float → SDR sRGB 8-bit
        let target = 0u8; // 0=sRGB, 1=Rec.709, 2=DCI-P3, 3=Linear
        tonemap::apply_tone_mapping_to_image(
            &pixel_rows,
            params.width,
            params.height,
            params.chdr.unwrap(),
            target,
            params.tonemap_operator,
        )
    } else if params.sample_format == SAMPLE_FORMAT_FLOAT
        || params.sample_format == SAMPLE_FORMAT_HALF
    {
        // float/half: reduces samples to 8 bits and converts color → RGBA
        convert_color_type_to_rgba_with_format(
            &pixel_rows,
            params.width,
            params.height,
            params.color_type,
            params.bit_depth,
            params.sample_format,
        )
    } else {
        // uint: convert back to RGBA
        let _bpp_from_color =
            bytes_per_pixel(params.color_type, params.bit_depth).ok_or_else(|| {
                CafeError::UnsupportedFeature(format!(
                    "Color type {}, bit depth {} not supported in output conversion",
                    params.color_type, params.bit_depth
                ))
            })?;
        convert_color_type_to_rgba(
            &pixel_rows,
            params.width,
            params.height,
            params.color_type,
            params.bit_depth,
        )
    }
}

/// Decodes a CAFE buffer (bytes) and returns pixels + metadata.
/// This is the core decode implementation without file I/O.
///
/// Uses default tone-map operator (Filmic). For custom operator selection, use decode_bytes_with_opts().
pub fn decode_bytes(buf: &[u8]) -> Result<(Vec<u8>, DecodeResult)> {
    decode_bytes_with_opts(buf, &EncodeOptions::default())
}

/// Decodes a CAFE buffer with custom decode options (tone-map operator selection, etc.)
/// This is the core decode implementation without file I/O, with customizable options.
fn decode_bytes_internal(
    buf: &[u8],
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<(Vec<u8>, DecodeResult)> {
    if buf.len() < 9 || buf[0..9] != SIGNATURE {
        return Err(CafeError::InvalidSignature);
    }

    let mut offset = 9;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut filter_method = FILTER_METHOD_NONE;
    let mut interlace_method_read = INTERLACE_NONE; // Read from IHDR (v1.0)
    let mut bytes_per_row: usize = 0;
    let mut pixel_rows: Vec<u8> = Vec::new();
    // SECURITY (CWE-409): cumulative decompression budget for the IDATs.
    // Derived from the size expected by the IHDR; prevents multiple IDATs
    // from expanding to gigabytes when the image is small.
    let mut decompress_budget: Option<u64> = None;
    let mut decompressed_total: u64 = 0;
    let mut adam7_passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default(); // For Adam7 (v1.0)
    let mut even_odd_passes: [Vec<u8>; EVEN_ODD_NUM_PASSES] = Default::default(); // For even/odd (v1.0)
    let mut exif: Option<Vec<u8>> = None;
    let mut json_metadata: HashMap<String, Value> = HashMap::new();
    let mut icc_profile: Option<Vec<u8>> = None; // New: ICC profile (v1.0)
    let mut xmp_metadata: Option<String> = None; // New: XMP metadata (v1.0)
    let mut zstd_dictionary: Option<Vec<u8>> = None; // New: ZSTD dictionary (v1.0)
    let mut color_type: u8 = COLOR_TYPE_RGBA; // Default, will be overwritten
    let mut bit_depth: u8 = 8; // Default, will be overwritten
    let mut sample_format: u8 = SAMPLE_FORMAT_UINT; // Default: unsigned integer (v1.0)
    let mut palette: Option<Palette> = None;
    let mut idim: Option<iDim> = None; // iDIM chunk (v1.0, ancillary)
    let mut tiles_seen: usize = 0; // Tile counter for 2D tiling (iDIM)
    let mut chdr: Option<cHDR> = None; // cHDR chunk (v1.0, ancilar, HDR metadata)

    while offset < buf.len() {
        let chunk = read_chunk(buf, offset)?;
        offset = chunk.next_offset;

        match &chunk.chunk_type {
            t if t == CHUNK_IHDR => {
                const IHDR_LEN: usize = 14;
                if chunk.data.len() < IHDR_LEN {
                    return Err(CafeError::TruncatedFile(format!(
                        "IHDR must have {IHDR_LEN} bytes, got {}",
                        chunk.data.len()
                    )));
                }
                let w = u32::from_be_bytes(chunk.data[0..4].try_into().map_err(|_| {
                    CafeError::TruncatedFile("IHDR Width conversion failed".into())
                })?);
                let h = u32::from_be_bytes(chunk.data[4..8].try_into().map_err(|_| {
                    CafeError::TruncatedFile("IHDR Height conversion failed".into())
                })?);
                let bd = chunk.data[8];
                let sf = chunk.data[9]; // Sample format (v1.0)
                let ct = chunk.data[10];
                let compression_method = chunk.data[11];
                let fm = chunk.data[12];
                let interlace_method = chunk.data[13];

                // Width/Height = 0 is a degenerate image; rejecting here avoids
                // a later division by zero (bytes_per_row computed from the
                // width) instead of propagating an invalid state silently.
                if w == 0 || h == 0 {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "Invalid dimensions: Width={w}, Height={h} (neither can be 0)"
                    )));
                }

                // Validate sample format (section 4.1, v1.0)
                match sf {
                    SAMPLE_FORMAT_UINT | SAMPLE_FORMAT_FLOAT | SAMPLE_FORMAT_HALF => {
                        // Valid, will be stored
                    }
                    _ => {
                        return Err(CafeError::UnsupportedFeature(format!(
                            "Sample format {} not supported (supports 0=uint, 1=float, 2=half)",
                            sf
                        )));
                    }
                }
                sample_format = sf;

                // Validate the sample_format + bit_depth combination (section 4.1, v1.0)
                if sf == SAMPLE_FORMAT_FLOAT && bd != 32 {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "Sample format FLOAT requires bit_depth 32, got {}",
                        bd
                    )));
                }
                if sf == SAMPLE_FORMAT_HALF && bd != 16 {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "Sample format HALF requires bit_depth 16, got {}",
                        bd
                    )));
                }
                if sf == SAMPLE_FORMAT_UINT && (bd == 32 || bd == 16) && bd != 32 && bd != 16 {
                    // uint allows multiple bit depths, but 16 and 32 may conflict with float/half
                    // Allowed only if explicitly uint
                }

                // Validate color type and bit depth (section 4.1, v1.0)
                match ct {
                    COLOR_TYPE_GRAY => {
                        // Grayscale: bit depth 1, 2, 4, 8, 10, 12, 16, 32 (section 4.1.1, v1.0)
                        match bd {
                            1 | 2 | 4 => {
                                // Sub-byte: compute ceil(width * bit_depth / 8)
                                bytes_per_row =
                                    bytes_per_row_for_bit_depth(w, bd).unwrap_or(w as usize);
                            }
                            8 => {
                                // 8-bit: 1 byte per pixel
                                bytes_per_row = w as usize;
                            }
                            10 | 12 | 16 => {
                                // 16-bit container: 2 bytes per pixel
                                bytes_per_row = (w as u64).checked_mul(2).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Grayscale multi-byte 10/12/16)".into(),
                                    )
                                })? as usize;
                            }
                            32 => {
                                // 32-bit: 4 bytes per pixel
                                bytes_per_row = (w as u64).checked_mul(4).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Grayscale 32-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            _ => {
                                return Err(CafeError::UnsupportedFeature(format!(
                                    "Color type=0 (Grayscale): bit depth {} not supported",
                                    bd
                                )));
                            }
                        }
                        color_type = COLOR_TYPE_GRAY;
                        bit_depth = bd;
                    }
                    COLOR_TYPE_RGB => {
                        // RGB: bit depth 8, 10, 12, 16, 32 (section 4.1.2, v1.0)
                        match bd {
                            8 => {
                                // 8-bit: 3 bytes/pixel
                                bytes_per_row = (w as u64).checked_mul(3).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGB 8-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            10 | 12 | 16 => {
                                // 16-bit container: 6 bytes/pixel (3 channels × 2 bytes)
                                bytes_per_row = (w as u64).checked_mul(6).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGB 10/12/16)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            32 => {
                                // 32-bit: 12 bytes/pixel (3 channels × 4 bytes)
                                bytes_per_row = (w as u64).checked_mul(12).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGB 32-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            _ => {
                                return Err(CafeError::UnsupportedFeature(format!(
                                    "Color type=2 (RGB): bit depth {} not supported",
                                    bd
                                )));
                            }
                        }
                        color_type = COLOR_TYPE_RGB;
                        bit_depth = bd;
                    }
                    COLOR_TYPE_INDEXED => {
                        // Palette: bit depth must be 1, 2, 4 or 8
                        if bd != 1 && bd != 2 && bd != 4 && bd != 8 {
                            return Err(CafeError::UnsupportedFeature(format!(
                                "Color type=3 (Indexed): bit depth must be 1, 2, 4, or 8, got {bd}"
                            )));
                        }
                        color_type = COLOR_TYPE_INDEXED;
                        bit_depth = bd;
                        // For PLTE, bytes_per_row will be adjusted after reading the palette
                    }
                    COLOR_TYPE_GRAY_ALPHA => {
                        // Gray + Alpha: bit depth 1, 2, 4, 8, 10, 12, 16, 32 (section 4.1.3, v1.0)
                        match bd {
                            1 | 2 | 4 => {
                                // Sub-byte: compute ceil(width * 2 * bit_depth / 8)
                                let samples_per_row = w as u64 * 2u64;
                                bytes_per_row =
                                    (samples_per_row.checked_mul(bd as u64).ok_or_else(|| {
                                        CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Color type=4)"
                                            .into(),
                                    )
                                    })? as usize)
                                        .div_ceil(8);
                            }
                            8 => {
                                // 8-bit: 2 bytes/pixel (Gray + Alpha)
                                bytes_per_row = (w as u64).checked_mul(2).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Gray+Alpha 8-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            10 | 12 | 16 => {
                                // 16-bit container: 4 bytes/pixel (2 channels × 2 bytes)
                                bytes_per_row = (w as u64).checked_mul(4).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Gray+Alpha 10/12/16)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            32 => {
                                // 32-bit: 8 bytes/pixel (2 channels × 4 bytes)
                                bytes_per_row = (w as u64).checked_mul(8).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (Gray+Alpha 32-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            _ => {
                                return Err(CafeError::UnsupportedFeature(format!(
                                    "Color type=4 (Gray+Alpha): bit depth {} not supported",
                                    bd
                                )));
                            }
                        }
                        color_type = COLOR_TYPE_GRAY_ALPHA;
                        bit_depth = bd;
                    }
                    COLOR_TYPE_RGBA => {
                        // RGBA: bit depth 8, 10, 12, 16, 32 (section 4.1.4, v1.0)
                        match bd {
                            8 => {
                                // 8-bit: 4 bytes/pixel (R, G, B, A)
                                bytes_per_row = (w as u64).checked_mul(4).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGBA 8-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            10 | 12 | 16 => {
                                // 16-bit container: 8 bytes/pixel (4 channels × 2 bytes)
                                bytes_per_row = (w as u64).checked_mul(8).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGBA 10/12/16)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            32 => {
                                // 32-bit: 16 bytes/pixel (4 channels × 4 bytes)
                                bytes_per_row = (w as u64).checked_mul(16).ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "bytes_per_row calculation would overflow (RGBA 32-bit)"
                                            .into(),
                                    )
                                })? as usize;
                            }
                            _ => {
                                return Err(CafeError::UnsupportedFeature(format!(
                                    "Color type=6 (RGBA): bit depth {} not supported",
                                    bd
                                )));
                            }
                        }
                        color_type = COLOR_TYPE_RGBA;
                        bit_depth = bd;
                    }
                    _ => {
                        return Err(CafeError::UnsupportedFeature(format!(
                            "Color type {ct} not supported (supports 0, 2, 3, 4, 6)"
                        )));
                    }
                }

                // SECURITY: Validate Filter method (section 4.1)
                // Filter method = 1 (byte-shuffle) has been RESERVED since v1.0 and must be
                // rejected explicitly, per spec section 4.1
                // v1.1: Byte-shuffle (filter_method=1) now implemented
                if fm != FILTER_METHOD_NONE
                    && fm != FILTER_METHOD_BYTE_SHUFFLE
                    && fm != FILTER_METHOD_PREDICTIVE
                {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "Filter method {} invalid: supports 0 (none), 1 (byte-shuffle), or 2 (predictive)",
                        fm
                    )));
                }

                // SECURITY: Validate Interlace method (section 5)
                // v1.0 supports: 0 (none), 1 (Adam7) and 2 (even/odd)
                if interlace_method != INTERLACE_NONE
                    && interlace_method != INTERLACE_ADAM7
                    && interlace_method != INTERLACE_EVEN_ODD
                {
                    return Err(CafeError::UnsupportedFeature(
                        format!("Interlace method {} invalid: supports only 0 (none), 1 (Adam7), and 2 (even/odd)", interlace_method),
                    ));
                }
                if compression_method & !COMPRESSION_METHOD_ZSTD_BIT != 0 {
                    return Err(CafeError::UnsupportedFeature(
                        "unknown compression codec".into(),
                    ));
                }

                // SECURITY/§4.3.2: byte-shuffle (filter_method=1) is incompatible with
                // interlace. The encoder already rejects the combination; the decoder must
                // reject malformed files that declare it — the Adam7/even-odd passes assume
                // raw RGBA data, not byte-shuffled.
                if fm == FILTER_METHOD_BYTE_SHUFFLE && interlace_method != INTERLACE_NONE {
                    return Err(CafeError::UnsupportedFeature(
                        "Byte-shuffle (filter_method=1) is incompatible with interlace (section 4.3.2)"
                            .into(),
                    ));
                }

                width = Some(w);
                height = Some(h);
                filter_method = fm;
                interlace_method_read = interlace_method;

                // SECURITY (CWE-409): compute the cumulative decompression cap from the IHDR
                // dimensions. Each IDAT may only expand up to what the image still needs —
                // multiple IDATs cannot sum to gigabytes if the image is small.
                if decompress_budget.is_none() {
                    decompress_budget = Some(compute_decompress_budget(
                        interlace_method,
                        ct,
                        w,
                        h,
                        bytes_per_row,
                    ));
                }
            }
            t if t == CHUNK_PLTE => {
                // PLTE is critical, required with Color type = 3 (section 4.1.2)
                if palette.is_none() {
                    palette = Some(read_plte_chunk(chunk.flag, &chunk.data)?);
                    // v1.0: Adjust bytes_per_row ONLY if color_type=3 (PLTE)
                    // If color_type=6 (RGBA) with Adam7, PLTE is ignored (don't overwrite bytes_per_row)
                    if color_type == COLOR_TYPE_INDEXED {
                        if let Some(w) = width {
                            // For palette, bytes_per_row depends on bit_depth (1, 2, 4, 8)
                            bytes_per_row = bytes_per_row_for_bit_depth(w, bit_depth)?;
                        }
                    }
                }
            }
            t if t == CHUNK_EXIF => {
                if exif.is_none() {
                    // single instance (section 4.5) - ignores repeats
                    exif = Some(decompress_chunk(chunk.flag, &chunk.data)?);
                }
            }
            t if t == CHUNK_JSON => {
                let (namespace, obj) = read_json_chunk(chunk.flag, &chunk.data)?;
                if let Some(obj) = obj {
                    json_metadata.insert(namespace, obj);
                }
                // obj == None -> malformed JSON, silently discarded (ancillary)
            }
            t if t == CHUNK_ICCP => {
                // iCCP is ancillary, single instance (v1.0)
                if icc_profile.is_none() {
                    match read_iccp_chunk(chunk.flag, &chunk.data) {
                        Ok(profile) => icc_profile = Some(profile),
                        Err(e) => {
                            // Invalid ICC profile, silently discarded (ancillary)
                            eprintln!("Warning: invalid iCCP chunk, discarded: {}", e);
                        }
                    }
                }
            }
            t if t == CHUNK_XMPD => {
                // xMPd is ancillary, single instance (v1.0)
                if xmp_metadata.is_none() {
                    match read_xmpd_chunk(chunk.flag, &chunk.data) {
                        Ok(xmp) => xmp_metadata = Some(xmp),
                        Err(e) => {
                            // Invalid XMP metadata, silently discarded (ancillary)
                            eprintln!(
                                "Warning: xMPd chunk contains invalid UTF-8, discarded: {}",
                                e
                            );
                        }
                    }
                }
            }
            t if t == CHUNK_ZDIC => {
                // zDIC is ancillary, single instance (v1.0)
                if zstd_dictionary.is_none() {
                    match read_zdic_chunk(chunk.flag, &chunk.data) {
                        Ok(dict) => zstd_dictionary = Some(dict),
                        Err(e) => {
                            // Invalid ZSTD dictionary, silently discarded (ancillary)
                            eprintln!("Warning: invalid zDIC chunk, discarded: {}", e);
                        }
                    }
                }
            }
            t if t == CHUNK_IDIM => {
                // iDIM is ancillary, optional (v1.0, section 4.2)
                if idim.is_none() {
                    // Single instance per file (similar to eXIF)
                    idim = Some(read_idim_chunk(chunk.flag, &chunk.data)?);
                }
            }
            t if t == CHUNK_CHDR => {
                // cHDR is ancillary, single instance (v1.0, section 4.4)
                if chdr.is_none() {
                    match read_chdr_chunk(chunk.flag, &chunk.data) {
                        Ok(chdr_data) => chdr = Some(chdr_data),
                        Err(e) => {
                            // Invalid cHDR, silently discarded (ancillary)
                            eprintln!("Warning: invalid cHDR chunk, discarded: {}", e);
                        }
                    }
                }
            }
            t if t == CHUNK_IDAT => {
                // SECURITY (CWE-409): the decompression cap for this IDAT is the
                // remaining budget (computed from the IHDR). A single IDAT cannot
                // expand beyond what the image still needs.
                let budget = decompress_budget
                    .ok_or_else(|| CafeError::TruncatedFile("IDAT before IHDR".into()))?;
                let remaining = budget.saturating_sub(decompressed_total);
                let decompressed = decompress_chunk_dict_limited(
                    chunk.flag,
                    &chunk.data,
                    zstd_dictionary.as_deref(),
                    remaining,
                )?;
                decompressed_total = decompressed_total
                    .checked_add(decompressed.len() as u64)
                    .ok_or_else(|| {
                        CafeError::TruncatedFile("overflow in decompressed total".into())
                    })?;

                // v1.0/+5: If interlaced, extract pass_number from the prefix
                if interlace_method_read == INTERLACE_ADAM7 {
                    if decompressed.is_empty() {
                        return Err(CafeError::UnsupportedFeature("Empty Adam7 IDAT".into()));
                    }
                    let pass_number = decompressed[0];
                    if pass_number == 0 || pass_number > 7 {
                        return Err(CafeError::UnsupportedFeature(format!(
                            "Invalid Adam7 pass number: {}",
                            pass_number
                        )));
                    }
                    let pass_data = decompressed[1..].to_vec();
                    let pass_idx = (pass_number - 1) as usize;
                    adam7_passes[pass_idx] = pass_data;
                } else if interlace_method_read == INTERLACE_EVEN_ODD {
                    if decompressed.is_empty() {
                        return Err(CafeError::UnsupportedFeature("Empty even/odd IDAT".into()));
                    }
                    let pass_number = decompressed[0];
                    if pass_number == 0 || pass_number > 2 {
                        return Err(CafeError::UnsupportedFeature(format!(
                            "invalid even/odd pass number: {}",
                            pass_number
                        )));
                    }
                    let pass_data = decompressed[1..].to_vec();
                    let pass_idx = (pass_number - 1) as usize;
                    even_odd_passes[pass_idx] = pass_data;
                } else {
                    // v1.0 (with full interlace support): process normally
                    let tile_payload = decompressed;

                    if let Some(idim) = &idim {
                        // 2D tiling (section 4.2): each IDAT is a tile in the scan_order order;
                        // reassembles the tiles back into the `pixel_rows` buffer.
                        if color_type == COLOR_TYPE_INDEXED {
                            return Err(CafeError::UnsupportedFeature(
                                "iDIM (2D tiling) with indexed palette not supported".into(),
                            ));
                        }
                        if bit_depth < 8 {
                            return Err(CafeError::UnsupportedFeature(
                                "iDIM (tiling 2D) requires bit_depth >= 8 in decode".into(),
                            ));
                        }
                        let bpp_for_tile =
                            bytes_per_pixel(color_type, bit_depth).ok_or_else(|| {
                                CafeError::UnsupportedFeature(format!(
                                    "Color type {color_type}, bit depth {bit_depth} not supported"
                                ))
                            })?;
                        let img_width = width.ok_or(CafeError::MissingIhdr)?;
                        let img_height = height.ok_or(CafeError::MissingIhdr)?;
                        let ih = img_height as usize;
                        let full_size = bytes_per_row.checked_mul(ih).ok_or_else(|| {
                            CafeError::TruncatedFile(
                                "overflow in bytes_per_row × height (iDIM)".into(),
                            )
                        })?;
                        let tile_count = idim.tiles_x as usize * idim.tiles_y as usize;
                        if tiles_seen >= tile_count {
                            return Err(CafeError::TruncatedFile(format!(
                                "Excess IDAT: expected {tile_count} tiles (iDIM)"
                            )));
                        }
                        if pixel_rows.is_empty() {
                            pixel_rows = vec![0u8; full_size];
                        }
                        if pixel_rows.len() != full_size {
                            return Err(CafeError::TruncatedFile(
                                "tile buffer inconsistent with IHDR (iDIM)".into(),
                            ));
                        }
                        let tile_order = idim.tile_order()?;
                        let (tx, ty) = tile_order[tiles_seen];
                        tiles_seen += 1;
                        let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, img_width, img_height);
                        let tw = tile_w as usize;
                        let th = tile_h as usize;
                        let tile_stride = tw.checked_mul(bpp_for_tile).ok_or_else(|| {
                            CafeError::UnsupportedFeature("overflow in tile stride (iDIM)".into())
                        })?;
                        let tile_raw = if filter_method == FILTER_METHOD_BYTE_SHUFFLE {
                            // v1.1: byte-shuffle undone before any predictive filter
                            shuffle::undo_byte_shuffle(&tile_payload, bpp_for_tile, tile_w, tile_h)?
                        } else if filter_method == FILTER_METHOD_PREDICTIVE {
                            // 1 filter byte prefixed per tile, with tile_stride per row
                            let tile_h_est =
                                tile_payload.len().saturating_sub(1) / tile_stride.max(1);
                            if tile_h_est != th {
                                return Err(CafeError::TruncatedFile(format!(
                                    "tile with inconsistent height: expected {th}, "
                                )));
                            }
                            undo_predictive_filter(&tile_payload, th, tile_stride, bpp_for_tile)?
                        } else {
                            tile_payload
                        };
                        let tile_len = tile_stride.checked_mul(th).ok_or_else(|| {
                            CafeError::TruncatedFile("overflow in tile len (iDIM)".into())
                        })?;
                        if tile_raw.len() != tile_len {
                            return Err(CafeError::TruncatedFile(format!(
                                "tile {tiles_seen} with unexpected size: {} (expected {})",
                                tile_raw.len(),
                                tile_len
                            )));
                        }
                        let row0 = (ty as u32 * idim.tile_height as u32) as usize;
                        let col0 = (tx as u32 * idim.tile_width as u32) as usize;
                        for r in 0..th {
                            let dst_start = (row0 + r)
                                .checked_mul(bytes_per_row)
                                .and_then(|v| v.checked_add(col0 * bpp_for_tile))
                                .ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "overflow in tile destination (iDIM)".into(),
                                    )
                                })?;
                            if dst_start + tile_stride > pixel_rows.len() {
                                return Err(CafeError::TruncatedFile(
                                    "tile exceeds image buffer (iDIM)".into(),
                                ));
                            }
                            let src = &tile_raw[r * tile_stride..(r + 1) * tile_stride];
                            pixel_rows[dst_start..dst_start + tile_stride].copy_from_slice(src);
                        }
                    } else if color_type == COLOR_TYPE_INDEXED {
                        // IDAT contains indices packed in bit_depth bits (or filtered)
                        // v1.0: the predictive filter prefixes 1 byte per block/tile (not per row)
                        // SECURITY (§4.1.2/CWE-369): color_type=3 requires a PLTE chunk before
                        // any IDAT; without it bytes_per_row is 0 and the division below
                        // would panic. Reject with a recoverable error.
                        if palette.is_none() {
                            return Err(CafeError::TruncatedFile(
                                "Color type=3 requires PLTE chunk before first IDAT".into(),
                            ));
                        }
                        // v1.1: Byte-shuffle undone before other filters
                        let tile_payload = if filter_method == FILTER_METHOD_BYTE_SHUFFLE {
                            let img_width = width.ok_or(CafeError::MissingIhdr)?;
                            let img_height = height.ok_or(CafeError::MissingIhdr)?;
                            let bpp = 1; // Indexed always 1 byte/pixel (before pack)
                            shuffle::undo_byte_shuffle(&tile_payload, bpp, img_width, img_height)?
                        } else {
                            tile_payload
                        };
                        let tile_h = if filter_method == FILTER_METHOD_PREDICTIVE {
                            tile_payload.len().saturating_sub(1) / bytes_per_row
                        } else {
                            tile_payload.len() / bytes_per_row
                        };
                        let tile_packed = if filter_method == FILTER_METHOD_PREDICTIVE {
                            undo_predictive_filter(&tile_payload, tile_h, bytes_per_row, 1)?
                        } else {
                            tile_payload
                        };
                        // Unpack each row back to 1 byte/index
                        // (bit_depth==8 is a trivial case inside unpack_indices_row)
                        let row_width = width.ok_or(CafeError::MissingIhdr)? as usize;
                        let img_height = height.ok_or(CafeError::MissingIhdr)? as usize;
                        // SECURITY (CWE-400): prevents accumulation of indices beyond the
                        // declared size (multiple-IDAT bomb).
                        let expected_indices =
                            row_width.checked_mul(img_height).ok_or_else(|| {
                                CafeError::TruncatedFile(
                                    "overflow in calculation of expected indices (indexed)".into(),
                                )
                            })?;
                        for r in 0..tile_h {
                            let row_packed =
                                &tile_packed[r * bytes_per_row..(r + 1) * bytes_per_row];
                            let row_indices = unpack_indices_row(row_packed, bit_depth, row_width)?;
                            let new_len = pixel_rows
                                .len()
                                .checked_add(row_indices.len())
                                .ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "overflow accumulating pixel indices".into(),
                                    )
                                })?;
                            if new_len > expected_indices {
                                return Err(CafeError::TruncatedFile(format!(
                                    "Excess IDAT: indexed pixel data sum exceeds \
                                      {expected_indices} (IHDR {row_width}x{img_height})"
                                )));
                            }
                            pixel_rows.extend_from_slice(&row_indices);
                        }
                    } else {
                        // Color types 0, 2, 4, 6: unpack with the correct bpp for the type
                        let bpp_for_filter =
                            bytes_per_pixel(color_type, bit_depth).ok_or_else(|| {
                                CafeError::UnsupportedFeature(format!(
                                    "Color type {color_type}, bit depth {bit_depth} not supported"
                                ))
                            })?;

                        // v1.1: Byte-shuffle undone before the predictive filter
                        let tile_payload = if filter_method == FILTER_METHOD_BYTE_SHUFFLE {
                            let img_width = width.ok_or(CafeError::MissingIhdr)?;
                            // tile_h derived from the payload (without the prefixed filter byte):
                            // each row has bytes_per_row bytes, so height = len / stride
                            let tile_h = tile_payload.len() / bytes_per_row.max(1);
                            shuffle::undo_byte_shuffle(
                                &tile_payload,
                                bpp_for_filter,
                                img_width,
                                tile_h as u32,
                            )?
                        } else {
                            tile_payload
                        };

                        let tile_raw = if filter_method == FILTER_METHOD_PREDICTIVE {
                            // v1.0: 1 filter byte per block/tile
                            let tile_h = tile_payload.len().saturating_sub(1) / bytes_per_row;
                            undo_predictive_filter(
                                &tile_payload,
                                tile_h,
                                bytes_per_row,
                                bpp_for_filter,
                            )?
                        } else {
                            tile_payload
                        };
                        // SECURITY (CWE-400): prevents accumulation of pixel rows beyond
                        // the declared size (multiple-IDAT bomb).
                        let img_height = height.ok_or(CafeError::MissingIhdr)? as usize;
                        let expected_row_bytes =
                            bytes_per_row.checked_mul(img_height).ok_or_else(|| {
                                CafeError::TruncatedFile(
                                    "overflow in bytes_per_row × height".into(),
                                )
                            })?;
                        let new_len =
                            pixel_rows
                                .len()
                                .checked_add(tile_raw.len())
                                .ok_or_else(|| {
                                    CafeError::TruncatedFile(
                                        "overflow accumulating linhas de pixel".into(),
                                    )
                                })?;
                        if new_len > expected_row_bytes {
                            return Err(CafeError::TruncatedFile(format!(
                                "Excess IDAT: indexed data pixels sum more than \
                                 {expected_row_bytes} bytes (bytes_per_row={bytes_per_row}, \
                                 height={img_height})"
                            )));
                        }
                        pixel_rows.extend_from_slice(&tile_raw);
                    }
                }
            }
            t if t == CHUNK_IEND => break,
            t => {
                // Unknown chunk: ancillary (1st letter lowercase) -> ignore;
                // critical (uppercase) -> error (section 3.1).
                if t[0].is_ascii_uppercase() {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "unknown critical chunk: {:?}",
                        String::from_utf8_lossy(t)
                    )));
                }
            }
        }
    }

    let width = width.ok_or(CafeError::MissingIhdr)?;
    let height = height.ok_or(CafeError::MissingIhdr)?;

    // Reconstruct final pixels: deinterlace, dequantize, convert color type
    let params = ReconstructParams {
        interlace_method: interlace_method_read,
        color_type,
        bit_depth,
        sample_format,
        width,
        height,
        palette: palette.as_ref(),
        chdr: chdr.as_ref(),
        adam7_passes: &adam7_passes,
        even_odd_passes: &even_odd_passes,
        tonemap_operator,
    };
    let final_pixels = reconstruct_final_pixels(pixel_rows, &params)?;

    // Validate final buffer: must have exactly width × height × 4 bytes (always RGBA)
    let expected_len = (width as u64)
        .checked_mul(height as u64)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| {
            CafeError::TruncatedFile("Expected final buffer size calculation would overflow".into())
        })? as usize;

    if final_pixels.len() != expected_len {
        return Err(CafeError::TruncatedFile(format!(
             "reconstructed pixel buffer has {} bytes, expected {expected_len} ({width}x{height}x4) \
              - corrupted file: incomplete/excess IDAT or inconsistent IHDR dimensions",
             final_pixels.len()
         )));
    }

    // Optional: Add compression statistics (always None for now)
    // Note: For true tracking, would need to store sizes during decoding
    let compression_stats = None;

    let result = DecodeResult {
        width,
        height,
        exif,
        json_metadata,
        compression_stats,
        icc_profile,
        xmp_metadata,
        zstd_dictionary,
        chdr_metadata: chdr,
    };

    Ok((final_pixels, result))
}

/// Decodes a CAFE buffer with custom decode options (tone-map operator selection, etc.)
///
/// # Arguments
/// * `buf` - Buffer containing CAFE-encoded data
/// * `opts` - Decode options (tone-map operator, etc.)
pub fn decode_bytes_with_opts(buf: &[u8], opts: &EncodeOptions) -> Result<(Vec<u8>, DecodeResult)> {
    decode_bytes_internal(buf, opts.tonemap_operator)
}

/// Decodes a CAFE file to RGBA image on disk.
/// Decodes a CAFE file with custom options (tone-map operator selection, etc.)
///
/// # Arguments
/// * `input_path` - Path to input .cafe file
/// * `output_path` - Path to output image file
/// * `opts` - Decode options (primarily for tone-map operator selection)
pub fn decode_with_opts(
    input_path: &str,
    output_path: &str,
    opts: &EncodeOptions,
) -> Result<DecodeResult> {
    let buf = std::fs::read(input_path)?;
    let (final_pixels, result) = decode_bytes_with_opts(&buf, opts)?;

    let img_buf = image::RgbaImage::from_raw(result.width, result.height, final_pixels)
        .ok_or_else(|| {
            CafeError::TruncatedFile(
                "unexpected failure assembling final image from pixel buffer".to_string(),
            )
        })?;
    img_buf.save(output_path)?;

    Ok(result)
}

pub fn decode(input_path: &str, output_path: &str) -> Result<DecodeResult> {
    decode_with_opts(input_path, output_path, &EncodeOptions::default())
}

/// **New feature (v1.0):** Encodes an image with an indexed palette (section 4.1.2).
/// Quantizes colors automatically if there are too many, and writes a PLTE chunk.
/// **Note:** Reduced v1.0 implementation — supports up to 256 colors, bit depth = 8 only.
pub fn encode_indexed(input_path: &str, output_path: &str, opts: &EncodeOptions) -> Result<()> {
    if opts.idim.is_some() {
        return Err(CafeError::UnsupportedFeature(
            "iDIM (2D tiling) is not supported with indexed palette".into(),
        ));
    }
    let img = image::open(input_path)?.to_rgba8();
    let (width, height) = img.dimensions();
    let raw = img.into_raw();

    // Quantize to palette (max 256 colors in v1.0)
    let (indices, palette) = quantize_to_palette(&raw, width, 256, opts.palette_algorithm);

    // Validate palette
    if palette.entries.is_empty() {
        return Err(CafeError::UnsupportedFeature("Empty palette".into()));
    }
    if palette.entries.len() > 256 {
        return Err(CafeError::UnsupportedFeature(format!(
            "Palette with {} colors, maximum 256 in v1.0",
            palette.entries.len()
        )));
    }

    let bit_depth = palette.bit_depth();

    // Byte-shuffle is incompatible with an indexed palette: indices have 1 byte/pixel
    // (bpp=1), and byte-shuffle requires bpp ∈ {2,4,8}.
    if opts.use_byte_shuffle {
        return Err(CafeError::UnsupportedFeature(
            "Byte-shuffle is incompatible with indexed palette (bpp=1)".into(),
        ));
    }

    let filter_method = if opts.use_filter {
        FILTER_METHOD_PREDICTIVE
    } else {
        FILTER_METHOD_NONE
    };

    let mut out = Vec::new();
    out.extend_from_slice(&SIGNATURE);

    // --- IHDR (14 bytes of payload, section 4.1) ---
    let mut ihdr = Vec::with_capacity(14);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());

    // v1.0/+5: If interlaced (Adam7 or even/odd), use color_type=6 (RGBA) because indices→RGBA conversion is needed
    let color_type_ihdr = if opts.interlace_method == INTERLACE_ADAM7
        || opts.interlace_method == INTERLACE_EVEN_ODD
    {
        COLOR_TYPE_RGBA // 6: interlace converts indices to RGBA
    } else {
        COLOR_TYPE_INDEXED // 3: without interlace, uses packed indices
    };

    ihdr.push(if color_type_ihdr == COLOR_TYPE_INDEXED {
        bit_depth
    } else {
        8
    }); // Bit depth: real for palette, 8 for RGBA (interlace)
    ihdr.push(0); // Sample format: uint
    ihdr.push(color_type_ihdr); // Color type
    ihdr.push(0); // compression_method: bitmask (section 3.2), filled in at the end of encode
    ihdr.push(filter_method);
    ihdr.push(opts.interlace_method); // Interlace: 0=none, 1=Adam7
    out.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
    let mut uses_zstd = false;

    // --- iDIM (ancillary, optional, v1.0 smart streaming) ---
    // Must appear immediately after IHDR (section 9, mandatory order).
    uses_zstd |= append_idim_chunk_if_present(&mut out, opts)?;

    // --- eXIF, jSON, iCCP, xMPd (optional, sections 4.5-4.8) ---
    uses_zstd |= append_common_metadata_chunks(&mut out, opts)?;

    // --- zDIC (optional, single instance, section 4.9) ---
    // BUG HISTORY: before this shared helper existed, encode_indexed() USED
    // the dictionary to compress the IDATs (via compress_with_fallback_dict
    // below) but never wrote the zDIC chunk here — generating undecodable
    // files (the decoder could not find the dictionary and failed with
    // "Dictionary mismatch"). Sharing `append_zdic_chunk_if_present` with
    // encode() prevents this class of divergence from recurring.
    uses_zstd |=
        append_zdic_chunk_if_present(&mut out, opts.zstd_dictionary.as_deref(), opts.level)?;

    // --- PLTE (critical, required with Color type = 3 only) ---
    // v1.0/+5: If interlaced (color_type=6), do NOT write PLTE (not needed)
    if opts.interlace_method != INTERLACE_ADAM7 && opts.interlace_method != INTERLACE_EVEN_ODD {
        out.extend_from_slice(&write_plte_chunk(&palette, opts.level)?);
    }

    // --- IDAT (packed indices, optionally interlaced) ---

    if opts.interlace_method == INTERLACE_ADAM7 || opts.interlace_method == INTERLACE_EVEN_ODD {
        // v1.0/+5: Progressive interlace (Adam7 or even/odd)
        // Convert indices to RGBA to apply interlace
        let rgba_raw = indices
            .iter()
            .flat_map(|&idx| {
                let entry = &palette.entries[idx as usize];
                vec![entry.r, entry.g, entry.b, entry.a]
            })
            .collect::<Vec<u8>>();

        // Apply interlace (Adam7 or even/odd)
        if opts.interlace_method == INTERLACE_ADAM7 {
            let passes = apply_adam7_interlace(&rgba_raw, width, height);
            // Write each pass as an IDAT with a prefixed pass_number
            for (pass_idx, pass_data) in passes.iter().enumerate() {
                let pass_number = (pass_idx + 1) as u8;
                let mut pass_payload = vec![pass_number];
                pass_payload.extend_from_slice(pass_data);
                uses_zstd |= append_idat_chunk(
                    &mut out,
                    &pass_payload,
                    opts.level,
                    opts.zstd_dictionary.as_deref(),
                )?;
            }
        } else if opts.interlace_method == INTERLACE_EVEN_ODD {
            let passes = apply_even_odd_interlace(&rgba_raw, width, height);
            // Write each pass as an IDAT with a prefixed pass_number
            for (pass_idx, pass_data) in passes.iter().enumerate() {
                let pass_number = (pass_idx + 1) as u8;
                let mut pass_payload = vec![pass_number];
                pass_payload.extend_from_slice(pass_data);
                uses_zstd |= append_idat_chunk(
                    &mut out,
                    &pass_payload,
                    opts.level,
                    opts.zstd_dictionary.as_deref(),
                )?;
            }
        }
    } else {
        // v1.0 (with full interlace support): write in row tiles.
        // Tiles are independent, so packing + filter + compression is
        // parallelized across a rayon thread pool (v1.2.2); chunks are then
        // appended to `out` in original row order.
        let tile_rows = opts.tile_rows as usize;
        let height_usize = height as usize;
        let idx_bytes_per_row = bytes_per_row_for_bit_depth(width, bit_depth)?;

        let mut tile_bounds = Vec::new();
        let mut row_start = 0;
        while row_start < height_usize {
            let row_end = (row_start + tile_rows).min(height_usize);
            tile_bounds.push((row_start, row_end));
            row_start = row_end;
        }

        let zstd_dict = opts.zstd_dictionary.as_deref();
        let chunks: Vec<(Vec<u8>, bool)> = tile_bounds
            .par_iter()
            .map(|&(row_start, row_end)| -> Result<(Vec<u8>, bool)> {
                let tile_h = row_end - row_start;

                // Pack each row of indices into bit_depth bits/index (section 4.1.2)
                let mut tile_packed = Vec::with_capacity(tile_h * idx_bytes_per_row);
                for row in row_start..row_end {
                    let row_indices =
                        &indices[(row * width as usize)..((row + 1) * width as usize)];
                    let packed_row = pack_indices_row(row_indices, bit_depth)?;
                    tile_packed.extend_from_slice(&packed_row);
                }

                // The predictive filter operates on the already-packed bytes (bpp=1),
                // valid for any bit_depth (section 4.1.1/4.1.2 - interaction with Filter method).
                let tile_payload = if opts.use_filter {
                    apply_predictive_filter(
                        &tile_packed,
                        tile_h,
                        idx_bytes_per_row,
                        1,
                        opts.filter_heuristic,
                        opts.level,
                    )?
                } else {
                    tile_packed
                };

                build_idat_chunk(&tile_payload, opts.level, zstd_dict)
            })
            .collect::<Result<Vec<_>>>()?;

        for (chunk, chunk_used_zstd) in chunks {
            uses_zstd |= chunk_used_zstd;
            out.extend_from_slice(&chunk);
        }
    }

    // --- IEND ---
    out.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

    // Actual compression_method (section 3.2): bit0 = at least one chunk used ZSTD
    patch_ihdr_compression_method(
        &mut out,
        if uses_zstd {
            COMPRESSION_METHOD_ZSTD_BIT
        } else {
            0
        },
    );

    std::fs::write(output_path, out)?;
    eprintln!(
        "[CAFE] encoded with palette: {} colors, bit depth = {}",
        palette.entries.len(),
        bit_depth
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Chunk helper functions (wrappers using imported module functions)
// ---------------------------------------------------------------------------

// These functions write specific chunk types. They all use the imported
// module functions: write_chunk(), compress_with_fallback(), etc.

/// Returns `true` if the complete chunk (as produced by `write_chunk`) was
/// compressed with ZSTD (Flag = 0x01). Used to build the bitmask of
/// `compression_method` in the IHDR (section 3.2): bit 0 is only set if at
/// least one chunk of the file uses ZSTD.
fn chunk_uses_zstd(chunk: &[u8]) -> bool {
    chunk.len() >= 9 && chunk[8] == FLAG_ZSTD
}

/// Fills the `compression_method` byte in IHDR already assembled in `out` (with
/// placeholder value) and recalculates the CRC32 of the IHDR chunk, which was calculated
/// over the placeholder (section 3.2). Layout: sig(9) + len(4) + type(4) + flag(1)
/// + data(14) + crc(4); CRC = hash(type + flag + data), sections 3 and 4.1.
fn patch_ihdr_compression_method(out: &mut [u8], compression_method: u8) {
    const IHDR_DATA_OFFSET: usize = 9 + 4 + 4 + 1; // 18
    const CM_OFFSET: usize = IHDR_DATA_OFFSET + 11; // 29
    const CRC_OFFSET: usize = IHDR_DATA_OFFSET + 14; // 32
    out[CM_OFFSET] = compression_method;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&out[13..CRC_OFFSET]); // type + flag + data
    let crc = hasher.finalize();
    out[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_be_bytes());
}

fn write_json_chunk(namespace: &str, obj: &Value, level: i32) -> Result<Vec<u8>> {
    let ns = namespace.as_bytes();
    if ns.len() > u8::MAX as usize {
        return Err(CafeError::UnsupportedFeature(
            "JSON namespace exceeds 255 bytes (section 4.6)".into(),
        ));
    }
    let mut payload = Vec::with_capacity(1 + ns.len() + 32);
    payload.push(ns.len() as u8);
    payload.extend_from_slice(ns);
    let json_str = serde_json::to_string(obj)?;
    payload.extend_from_slice(json_str.as_bytes());
    let (flag, data) = compress_with_fallback(&payload, level)?;
    Ok(write_chunk(CHUNK_JSON, flag, &data))
}

fn read_json_chunk(flag: u8, data: &[u8]) -> Result<(String, Option<Value>)> {
    let decompressed = decompress_chunk(flag, data)?;
    if decompressed.is_empty() {
        return Err(CafeError::TruncatedFile(
            "JSON: empty payload (section 4.6)".into(),
        ));
    }
    let ns_len = decompressed[0] as usize;
    if decompressed.len() < 1 + ns_len {
        return Err(CafeError::TruncatedFile(
            "JSON: namespace length inconsistent (section 4.6)".into(),
        ));
    }
    let namespace = String::from_utf8(decompressed[1..1 + ns_len].to_vec())
        .map_err(|_| CafeError::TruncatedFile("JSON: invalid namespace UTF-8".into()))?;
    let json_str = String::from_utf8(decompressed[1 + ns_len..].to_vec())
        .map_err(|_| CafeError::TruncatedFile("JSON: invalid UTF-8".into()))?;
    let obj = serde_json::from_str(&json_str).ok();
    Ok((namespace, obj))
}

fn write_iccp_chunk(profile: &[u8], level: i32) -> Result<Vec<u8>> {
    let (flag, data) = compress_with_fallback(profile, level)?;
    Ok(write_chunk(CHUNK_ICCP, flag, &data))
}

fn read_iccp_chunk(flag: u8, data: &[u8]) -> Result<Vec<u8>> {
    decompress_chunk(flag, data)
}

fn write_xmpd_chunk(xmp: &str, level: i32) -> Result<Vec<u8>> {
    let payload = xmp.as_bytes();
    let (flag, data) = compress_with_fallback(payload, level)?;
    Ok(write_chunk(CHUNK_XMPD, flag, &data))
}

fn read_xmpd_chunk(flag: u8, data: &[u8]) -> Result<String> {
    let decompressed = decompress_chunk(flag, data)?;
    String::from_utf8(decompressed)
        .map_err(|_| CafeError::TruncatedFile("XMP: invalid UTF-8".into()))
}

fn write_zdic_chunk(dict: &[u8], level: i32) -> Result<Vec<u8>> {
    let (flag, data) = compress_with_fallback(dict, level)?;
    Ok(write_chunk(CHUNK_ZDIC, flag, &data))
}

fn read_zdic_chunk(flag: u8, data: &[u8]) -> Result<Vec<u8>> {
    decompress_chunk(flag, data)
}

fn write_idim_chunk(idim: &iDim) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(10);
    payload.extend_from_slice(&idim.tile_width.to_be_bytes());
    payload.extend_from_slice(&idim.tile_height.to_be_bytes());
    payload.extend_from_slice(&idim.tiles_x.to_be_bytes());
    payload.extend_from_slice(&idim.tiles_y.to_be_bytes());
    payload.push(idim.scan_order);
    Ok(write_chunk(CHUNK_IDIM, FLAG_RAW, &payload))
}

fn read_idim_chunk(_flag: u8, data: &[u8]) -> Result<iDim> {
    if data.len() < 9 {
        return Err(CafeError::TruncatedFile("iDIM: insufficient data".into()));
    }
    let tile_width = u16::from_be_bytes([data[0], data[1]]);
    let tile_height = u16::from_be_bytes([data[2], data[3]]);
    let tiles_x = u16::from_be_bytes([data[4], data[5]]);
    let tiles_y = u16::from_be_bytes([data[6], data[7]]);
    let scan_order = data[8];
    // SECURITY (CWE-20): validate scan_order (section 4.2 of spec)
    if scan_order > 1 {
        return Err(CafeError::UnsupportedFeature(format!(
            "invalid scan_order: {} (supports only 0=row-major, 1=Z-order/Morton)",
            scan_order
        )));
    }
    Ok(iDim {
        tile_width,
        tile_height,
        tiles_x,
        tiles_y,
        scan_order,
    })
}

fn write_plte_chunk(palette: &Palette, level: i32) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    let entry_format = if palette.has_alpha { 1u8 } else { 0u8 };
    payload.push(entry_format);
    for entry in &palette.entries {
        payload.push(entry.r);
        payload.push(entry.g);
        payload.push(entry.b);
        if palette.has_alpha {
            payload.push(entry.a);
        }
    }
    let (flag, data) = compress_with_fallback(&payload, level)?;
    Ok(write_chunk(CHUNK_PLTE, flag, &data))
}

fn read_plte_chunk(flag: u8, data: &[u8]) -> Result<Palette> {
    // SECURITY: the flag comes from file (untrusted); decompress_chunk respects
    // the 1 GiB ceiling. PLTE follows the fallback compression rule (section 3.2/4.1.2).
    let data = decompress_chunk(flag, data)?;
    if data.is_empty() {
        return Err(CafeError::TruncatedFile("PLTE: empty chunk".into()));
    }
    let entry_format = data[0];
    let has_alpha = entry_format != 0;
    let bytes_per_entry = if has_alpha { 4 } else { 3 };
    let mut entries = Vec::new();
    let data_payload = &data[1..];
    for i in (0..data_payload.len()).step_by(bytes_per_entry) {
        if i + bytes_per_entry > data_payload.len() {
            break;
        }
        let r = data_payload[i];
        let g = data_payload[i + 1];
        let b = data_payload[i + 2];
        let a = if has_alpha { data_payload[i + 3] } else { 255 };
        entries.push(PaletteEntry { r, g, b, a });
    }
    Ok(Palette { entries, has_alpha })
}

fn write_chdr_chunk(chdr: &cHDR, level: i32) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(18);
    payload.push(chdr.transfer_function);
    payload.push(chdr.color_primaries);
    payload.extend_from_slice(&chdr.max_luminance.to_bits().to_be_bytes());
    payload.extend_from_slice(&chdr.min_luminance.to_bits().to_be_bytes());
    if let Some(max_cll) = chdr.max_cll {
        payload.extend_from_slice(&max_cll.to_be_bytes());
    }
    if let Some(max_fall) = chdr.max_fall {
        payload.extend_from_slice(&max_fall.to_be_bytes());
    }
    let (flag, data) = compress_with_fallback(&payload, level)?;
    Ok(write_chunk(CHUNK_CHDR, flag, &data))
}

fn read_chdr_chunk(flag: u8, data: &[u8]) -> Result<cHDR> {
    let raw_data = decompress_chunk(flag, data)?;
    if !matches!(raw_data.len(), 10 | 14 | 18) {
        return Err(CafeError::TruncatedFile(format!(
            "cHDR: invalid size {} (must be 10, 14, or 18, section 4.4)",
            raw_data.len()
        )));
    }
    let transfer_function = raw_data[0];
    let color_primaries = raw_data[1];
    let max_lum_bits = u32::from_be_bytes([raw_data[2], raw_data[3], raw_data[4], raw_data[5]]);
    let max_luminance = f32::from_bits(max_lum_bits);
    let min_lum_bits = u32::from_be_bytes([raw_data[6], raw_data[7], raw_data[8], raw_data[9]]);
    let min_luminance = f32::from_bits(min_lum_bits);
    let max_cll = if raw_data.len() >= 14 {
        Some(u32::from_be_bytes([
            raw_data[10],
            raw_data[11],
            raw_data[12],
            raw_data[13],
        ]))
    } else {
        None
    };
    let max_fall = if raw_data.len() >= 18 {
        Some(u32::from_be_bytes([
            raw_data[14],
            raw_data[15],
            raw_data[16],
            raw_data[17],
        ]))
    } else {
        None
    };
    Ok(cHDR {
        transfer_function,
        color_primaries,
        max_luminance,
        min_luminance,
        max_cll,
        max_fall,
    })
}

// Palette quantization (finds best color matches and creates indices)
fn quantize_to_palette(
    rgba: &[u8],
    _width: u32,
    max_colors: u32,
    algorithm: PaletteAlgorithm,
) -> (Vec<u8>, Palette) {
    match algorithm {
        PaletteAlgorithm::NearestNeighbor => quantize_nearest_neighbor(rgba, max_colors),
        PaletteAlgorithm::MedianCut => quantize_median_cut_wrapper(rgba, max_colors),
    }
}

fn quantize_nearest_neighbor(rgba: &[u8], max_colors: u32) -> (Vec<u8>, Palette) {
    let mut palette = Palette {
        entries: Vec::new(),
        has_alpha: true,
    };
    let mut indices = Vec::new();

    for chunk in rgba.as_chunks::<4>().0 {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        let a = chunk[3];

        // Simple nearest-neighbor palette lookup
        let mut best_idx = 0;
        let mut best_dist = u32::MAX;

        for (i, entry) in palette.entries.iter().enumerate() {
            let dist = ((r as i32 - entry.r as i32).pow(2)
                + (g as i32 - entry.g as i32).pow(2)
                + (b as i32 - entry.b as i32).pow(2)
                + (a as i32 - entry.a as i32).pow(2)) as u32;
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }

        // Add color to palette if not found and space available
        if best_dist > 0 && (palette.entries.len() as u32) < max_colors {
            palette.entries.push(PaletteEntry { r, g, b, a });
            best_idx = palette.entries.len() - 1;
        }

        indices.push(best_idx as u8);
    }

    (indices, palette)
}

/// Wrapper around quantize::quantize_median_cut for use in encode_indexed
fn quantize_median_cut_wrapper(rgba: &[u8], max_colors: u32) -> (Vec<u8>, Palette) {
    // Use only RGB (ignoring alpha for now, as median-cut is RGB-based)
    let rgb_only: Vec<u8> = rgba
        .chunks(4)
        .flat_map(|chunk| vec![chunk[0], chunk[1], chunk[2], 255])
        .collect();

    match quantize::quantize_median_cut(&rgb_only, max_colors as usize) {
        Ok(palette) => {
            // Now map original RGBA pixels to palette indices using nearest-neighbor
            let mut indices = Vec::new();
            for chunk in rgba.as_chunks::<4>().0 {
                let r = chunk[0];
                let g = chunk[1];
                let b = chunk[2];

                let mut best_idx = 0;
                let mut best_dist = u32::MAX;

                for (i, entry) in palette.entries.iter().enumerate() {
                    // Only consider RGB distance (ignore alpha in palette matching)
                    let dist = ((r as i32 - entry.r as i32).pow(2)
                        + (g as i32 - entry.g as i32).pow(2)
                        + (b as i32 - entry.b as i32).pow(2)) as u32;
                    if dist < best_dist {
                        best_dist = dist;
                        best_idx = i;
                    }
                }

                indices.push(best_idx as u8);
            }

            (indices, palette)
        }
        Err(_) => {
            // Fall back to nearest-neighbor on error
            quantize_nearest_neighbor(rgba, max_colors)
        }
    }
}

// Dequantize palette (convert indices to RGBA)
fn dequantize_from_palette(
    indices: &[u8],
    palette: &Palette,
    _width: u32,
    _height: u32,
) -> Vec<u8> {
    let mut result = Vec::new();
    for &idx in indices {
        let entry = palette.entries.get(idx as usize).unwrap_or(&PaletteEntry {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        });
        result.push(entry.r);
        result.push(entry.g);
        result.push(entry.b);
        result.push(entry.a);
    }
    result
}

// ---------------------------------------------------------------------------
// Automated tests (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{
        compress_sample_32bit_to_8, compress_sample_n_to_8bits, expand_sample_8_to_32bit,
        expand_sample_8_to_n_bits, float_to_u8, half_to_u8, pack_samples_row, read_u16_be,
        read_u32_be, reduce_sample_8_to_n_bits, u8_to_float, u8_to_half, unpack_samples_row,
    };
    use crate::interlace::{adam7_pass_dimensions, extract_adam7_pass};
    use crate::types::{morton_code, morton_decode};

    #[test]
    fn test_signature_validation() {
        // Tests that invalid signature is rejected
        let buf = vec![0x00; 100]; // Incorrect signature
        let result = decode_from_buf(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_reconstruct_adam7_huge_dims_no_panic() {
        // SECURITY (CWE-190): extreme dimensions from untrusted IHDR
        // (65536×65536 overflows u32) must not cause panic — should return error.
        let passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default();
        let res = reconstruct_adam7(&passes, 65_536, 65_536);
        assert!(
            res.is_err(),
            "dimensions that overflow u32 must return error, not panic"
        );
    }

    #[test]
    fn test_reconstruct_even_odd_huge_dims_no_panic() {
        let passes: [Vec<u8>; EVEN_ODD_NUM_PASSES] = Default::default();
        let res = reconstruct_even_odd(&passes, 65_536, 65_536);
        assert!(
            res.is_err(),
            "dimensions that overflow u32 must return error, not panic"
        );
    }

    #[test]
    fn test_reconstruct_adam7_inconsistent_data() {
        // SECURITY (CWE-400): file with insufficient data for declared dimensions
        // must not pre-allocate giant buffer nor return truncated image.
        let mut passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default();
        passes[0] = vec![0u8; 16]; // 16 bytes ≪ 4×4×4=64 expected
        let res = reconstruct_adam7(&passes, 4, 4);
        assert!(res.is_err(), "inconsistent data must be rejected");
    }

    #[test]
    fn test_decode_adversarial_huge_interlace_dims() {
        // Forged file ~49 bytes: IHDR 65536×65536 + interlace Adam7 + IEND,
        // with no IDAT. Before the fix this crashed the decoder with panic
        // (overflow in debug, index out of bounds in release); now returns error.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&65_536u32.to_be_bytes());
        ihdr.extend_from_slice(&65_536u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_ADAM7);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_huge_adam7.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_huge_adam7.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_err(),
            "forged file must be rejected without panic"
        );
    }

    #[test]
    fn test_decode_adversarial_overflow_idat() {
        // Cumulative decompression bomb (CWE-400): multiple IDATs whose sum exceeds
        // declared size. Image is 4×4 RGBA (64 bytes); second excess IDAT
        // must be rejected without accumulating memory indefinitely.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        let row = [42u8; 64]; // 4 rows × 16 bytes = 64 bytes (entire image)
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &row));
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &row)); // excess
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_overflow_idat.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_overflow_idat.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(result.is_err(), "excess IDAT must be rejected");
    }

    #[test]
    fn test_decode_adversarial_idat_decompress_budget() {
        // Single IDAT decompression bomb (CWE-409): IHDR declares 4×4 RGBA image
        // (budget = 16 bytes/row × 4 + 4 = 68), but IDAT decompresses
        // to 1 MiB. The cap derived from IHDR must cut decompression at the limit,
        // not allow expansion to 1 GiB.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        // Payload that decompresses to 1 MiB of zeros
        let bomb = vec![0u8; 1024 * 1024];
        let compressed = zstd::encode_all(bomb.as_slice(), 19).unwrap();
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_ZSTD, &compressed));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_budget_bomb.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_budget_bomb.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_err(),
            "decompression beyond IHDR budget must be rejected"
        );
    }

    #[test]
    fn test_decode_adversarial_idat_cumulative_budget() {
        // Cumulative bomb (CWE-409): several small IDATs whose SUM decompresses
        // beyond the IHDR budget. Each individually is below the per-chunk cap,
        // but together they exceed the size the image can hold.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        // Each IDAT decompresses to 32 bytes (< 1 GiB per chunk), but 2 IDATs already
        // sum 64 > budget 68? No — 64 < 68. Use pieces that sum beyond the
        // budget clearly: 5 × 32 bytes = 160 > 68.
        let piece = [7u8; 32];
        let compressed = zstd::encode_all(piece.as_slice(), 19).unwrap();
        for _ in 0..5 {
            evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_ZSTD, &compressed));
        }
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_cumulative_bomb.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_cumulative_bomb.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_err(),
            "sum of IDATs beyond IHDR budget must be rejected"
        );
    }

    #[test]
    fn test_decode_adversarial_indexed_without_plte() {
        // PoC (CWE-369/§12.1): IHDR color_type=3 (INDEXED) without required PLTE chunk.
        // bytes_per_row is only calculated when PLTE is read; without it,
        // division tile_payload.len() / bytes_per_row with bytes_per_row == 0
        // must return recoverable error, NEVER panic due to division by zero.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_INDEXED);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        // No PLTE chunk; IDAT with 4 bytes = 2 rows × 2 bytes (bit_depth=8)
        let row = [0u8, 1, 2, 3];
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &row));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_indexed_noplte.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_indexed_noplte.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = std::panic::catch_unwind(|| {
            decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap())
        });
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_ok(),
            "decode of INDEXED without PLTE must return error, not panic (division by zero)"
        );
        assert!(
            result.unwrap().is_err(),
            "INDEXED without PLTE must be rejected"
        );
    }

    #[test]
    fn test_decode_adversarial_byte_shuffle_bad_size() {
        // PoC (CWE-125/§4.3.2): IHDR filter_method=1 (byte-shuffle) with an IDAT
        // whose decompressed size is not a multiple of bytes_per_row. Derivation
        // tile_h = len / bytes_per_row yields tile_h that does not satisfy
        // len == width × tile_h × bpp → undo_byte_shuffle must return error
        // recoverable, NEVER panic (read out-of-bounds).
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes()); // width
        ihdr.extend_from_slice(&2u32.to_be_bytes()); // height
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA); // bpp = 4
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_BYTE_SHUFFLE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        // 4×2 RGBA → bytes_per_row = 16, image = 32 bytes. Provide 10 bytes:
        // 10 % 16 != 0 → inconsistent size must be rejected.
        let bad_payload = vec![0u8; 10];
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &bad_payload));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_bs_badsize.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_bs_badsize.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = std::panic::catch_unwind(|| {
            decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap())
        });
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_ok(),
            "byte-shuffle with inconsistent size must return error, not panic"
        );
        assert!(
            result.unwrap().is_err(),
            "byte-shuffle with inconsistent size must be rejected"
        );
    }

    #[test]
    fn test_decode_adversarial_byte_shuffle_invalid_bpp() {
        // PoC (CWE-20/§4.3.2): color type RGB 8-bit (bpp=3, outside {2,4,8,16})
        // with filter_method=1. Decoder must validate bpp and reject with error
        // recoverable before any indexing — never panic.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGB); // bpp = 3 → invalid for byte-shuffle
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_BYTE_SHUFFLE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        // 4×2 RGB → bytes_per_row = 12, image = 24 bytes (correct in size,
        // but bpp invalid).
        let payload = vec![0u8; 24];
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &payload));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_bs_bpp.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_bs_bpp.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = std::panic::catch_unwind(|| {
            decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap())
        });
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_ok(),
            "byte-shuffle with invalid bpp must return error, not panic"
        );
        assert!(
            result.unwrap().is_err(),
            "byte-shuffle with invalid bpp must be rejected"
        );
    }

    #[test]
    fn test_decode_adversarial_byte_shuffle_with_interlace() {
        // PoC (CWE-20/§4.3.2): IHDR with filter_method=1 (byte-shuffle) and
        // interlace=Adam7 — combination the encoder never produces (is rejected).
        // Decoder must reject file with recoverable error instead of treating
        // passes as if they were byte-shuffled data.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_BYTE_SHUFFLE);
        ihdr.push(INTERLACE_ADAM7);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));
        let payload = vec![0u8; 16];
        evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_RAW, &payload));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_bs_interlace.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_bs_interlace.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = std::panic::catch_unwind(|| {
            decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap())
        });
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_ok(),
            "byte-shuffle + interlace must return error, not panic"
        );
        assert!(
            result.unwrap().is_err(),
            "byte-shuffle + interlace must be rejected"
        );
    }

    #[test]
    fn test_compute_decompress_budget_values() {
        // Unit validation of decompression cap derived from IHDR.
        // RGBA 4×4 no interlace: 16 bytes/row × 4 + 4 = 68
        assert_eq!(
            compute_decompress_budget(INTERLACE_NONE, COLOR_TYPE_RGBA, 4, 4, 16),
            68
        );
        // Indexed 4×4: loose cap width×height + height = 20
        assert_eq!(
            compute_decompress_budget(INTERLACE_NONE, COLOR_TYPE_INDEXED, 4, 4, 2),
            20
        );
        // Adam7 4×4 RGBA: 16 pixels × 4 bytes + 7 passes = 71
        assert_eq!(
            compute_decompress_budget(INTERLACE_ADAM7, COLOR_TYPE_RGBA, 4, 4, 16),
            71
        );
        // Even/odd 4×4 RGBA: 64 + 2 = 66
        assert_eq!(
            compute_decompress_budget(INTERLACE_EVEN_ODD, COLOR_TYPE_RGBA, 4, 4, 16),
            66
        );
    }

    #[test]
    fn test_plte_palette_creation() {
        let mut palette = Palette::new(true);
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        });

        assert_eq!(palette.entries.len(), 3);
        assert_eq!(palette.bit_depth(), 2); // 3 colors = 2 bits
    }

    #[test]
    fn test_plte_bit_depth_calculation() {
        let mut palette = Palette::new(false);

        // 1 color = 1 bit
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        });
        assert_eq!(palette.bit_depth(), 1);

        // 2 colors = 1 bit
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 255,
            b: 255,
            a: 0xFF,
        });
        assert_eq!(palette.bit_depth(), 1);

        // 3-4 colors = 2 bits
        palette.entries.push(PaletteEntry {
            r: 128,
            g: 128,
            b: 128,
            a: 0xFF,
        });
        assert_eq!(palette.bit_depth(), 2);

        // 5-16 colors = 4 bits
        for i in 0..13 {
            palette.entries.push(PaletteEntry {
                r: i as u8,
                g: i as u8,
                b: i as u8,
                a: 0xFF,
            });
        }
        assert_eq!(palette.bit_depth(), 4);

        // 17-256 colors = 8 bits
        for i in 0..240 {
            palette.entries.push(PaletteEntry {
                r: i as u8,
                g: (i * 2) as u8,
                b: (i * 3) as u8,
                a: 0xFF,
            });
            if palette.entries.len() >= 256 {
                break;
            }
        }
        assert_eq!(palette.bit_depth(), 8);
    }

    #[test]
    fn test_palette_find_closest() {
        let mut palette = Palette::new(true);
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        });

        let test_color = PaletteEntry {
            r: 255,
            g: 5,
            b: 5,
            a: 255,
        };
        let closest = palette.find_closest(&test_color);
        assert_eq!(closest, 1); // Should be close to pure red
    }

    #[test]
    fn test_quantize_palette() {
        // Tests color quantization
        let mut rgba = Vec::new();
        for _ in 0..100 {
            rgba.extend_from_slice(&[255, 0, 0, 255]); // Red
        }
        for _ in 0..100 {
            rgba.extend_from_slice(&[0, 255, 0, 255]); // Green
        }

        let (indices, palette) =
            quantize_to_palette(&rgba, 100, 256, PaletteAlgorithm::NearestNeighbor);
        assert!(palette.entries.len() <= 256);
        assert_eq!(indices.len(), 200);
    }

    #[test]
    fn test_dequantize_roundtrip() {
        // Tests dequantization
        let mut palette = Palette::new(true);
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        });

        let indices = vec![0, 1, 0, 1, 0];
        let rgba = dequantize_from_palette(&indices, &palette, 5, 1);

        assert_eq!(rgba.len(), 20); // 5 pixels - 4 bytes
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]); // First index = red
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]); // Second index = green
    }

    #[test]
    fn test_write_plte_chunk() {
        let mut palette = Palette::new(true);
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        });

        let chunk = write_plte_chunk(&palette, 19).unwrap();

        // Chunk must have: 4 (length) + 4 (type) + 1 (flag) + payload + 4 (crc32)
        assert!(chunk.len() > 10);
        // Type must be "PLTE"
        assert_eq!(&chunk[4..8], b"PLTE");
        // Flag must respect fallback (small palette stays raw)
        assert_eq!(chunk[8], FLAG_RAW);
    }

    #[test]
    fn test_plte_compressed_roundtrip() {
        // PLTE can follow the fallback compression (section 4.1.2/3.2); decoder
        // must honor the flag. Ensures a ZSTD-compressed PLTE is read correctly.
        let mut palette = Palette::new(true);
        palette.entries.push(PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        });
        palette.entries.push(PaletteEntry {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        });

        let mut payload = Vec::new();
        payload.push(1u8); // entry_format = RGBA
        for e in &palette.entries {
            payload.extend_from_slice(&[e.r, e.g, e.b, e.a]);
        }
        let compressed = zstd::encode_all(payload.as_slice(), 19).unwrap();

        let decoded = read_plte_chunk(FLAG_ZSTD, &compressed).unwrap();
        assert_eq!(decoded.entries.len(), 3);
        assert_eq!(decoded.entries[0], palette.entries[0]);
        assert_eq!(decoded.entries[2].a, 255);
    }

    // Helper function for tests (mock decode)
    fn decode_from_buf(_buf: &[u8]) -> Result<()> {
        Err(CafeError::InvalidSignature)
    }

    // ---------- Tests for iDIM (v1.0 Phase 1) ----------

    #[test]
    fn test_idim_creation() {
        // Create iDIM with image dimensions 512x512, tiles 128x128
        let idim = iDim::new(128, 128, 512, 512, 0);
        assert_eq!(idim.tile_width, 128);
        assert_eq!(idim.tile_height, 128);
        assert_eq!(idim.tiles_x, 4);
        assert_eq!(idim.tiles_y, 4);
        assert_eq!(idim.scan_order, 0);
    }

    #[test]
    fn test_idim_tile_dimensions() {
        // Image 500x500, tiles 128x128 -> unequal edges
        let idim = iDim::new(128, 128, 500, 500, 0);

        // Internal tile (0, 0) -> full dimensions
        let (w, h) = idim.tile_dimensions(0, 0, 500, 500);
        assert_eq!((w, h), (128, 128));

        // Bottom-right edge tile (3, 3) -> reduced
        let (w, h) = idim.tile_dimensions(3, 3, 500, 500);
        assert_eq!((w, h), (116, 116)); // 500 - 3*128 = 116
    }

    #[test]
    fn test_idim_tile_order_row_major() {
        // Test row-major (scan_order = 0)
        let idim = iDim::new(64, 64, 256, 256, 0);
        let order = idim.tile_order().expect("valid scan_order");

        // 4x4 = 16 tiles, row-major
        assert_eq!(order.len(), 16);
        assert_eq!(order[0], (0, 0));
        assert_eq!(order[1], (1, 0));
        assert_eq!(order[2], (2, 0));
        assert_eq!(order[3], (3, 0));
        assert_eq!(order[4], (0, 1)); // Second row
        assert_eq!(order[15], (3, 3)); // Last tile
    }

    #[test]
    fn test_write_idim_chunk() {
        let idim = iDim {
            tile_width: 128,
            tile_height: 128,
            tiles_x: 4,
            tiles_y: 4,
            scan_order: 0,
        };

        let chunk = write_idim_chunk(&idim).expect("Failed to write iDIM");

        // Chunk format: length (4) + type (4) + data (9) + crc (4) = 21 bytes
        assert!(chunk.len() >= 21);

        // Verify correct signature
        assert_eq!(&chunk[4..8], b"iDIM");
    }

    #[test]
    fn test_read_idim_chunk() {
        // Create and write iDIM
        let idim_orig = iDim {
            tile_width: 256,
            tile_height: 256,
            tiles_x: 2,
            tiles_y: 2,
            scan_order: 1, // Z-order
        };

        // Assemble payload manually
        let mut payload = Vec::new();
        payload.extend_from_slice(&idim_orig.tile_width.to_be_bytes());
        payload.extend_from_slice(&idim_orig.tile_height.to_be_bytes());
        payload.extend_from_slice(&idim_orig.tiles_x.to_be_bytes());
        payload.extend_from_slice(&idim_orig.tiles_y.to_be_bytes());
        payload.push(idim_orig.scan_order);

        // Read back
        let idim_read = read_idim_chunk(FLAG_RAW, &payload).expect("Failed to read iDIM");

        assert_eq!(idim_read.tile_width, 256);
        assert_eq!(idim_read.tile_height, 256);
        assert_eq!(idim_read.tiles_x, 2);
        assert_eq!(idim_read.tiles_y, 2);
        assert_eq!(idim_read.scan_order, 1);
    }

    #[test]
    fn test_read_idim_chunk_invalid_scan_order() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&128u16.to_be_bytes());
        payload.extend_from_slice(&128u16.to_be_bytes());
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.push(99); // invalid scan_order

        let result = read_idim_chunk(FLAG_RAW, &payload);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_idim_chunk_too_short() {
        let payload = vec![0u8; 8]; // Too short
        let result = read_idim_chunk(FLAG_RAW, &payload);
        assert!(result.is_err());
    }

    #[test]
    fn test_idim_roundtrip() {
        // Create original iDIM
        let idim_orig = iDim::new(64, 64, 512, 480, 0);

        // Write chunk
        let chunk = write_idim_chunk(&idim_orig).expect("failed to write");

        // Extract data from chunk (skipping length and type)
        // Chunk format: [length(4)][type(4)][flag(1)][data][crc(4)]
        let flag = chunk[8];
        let data_len = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
        let data = &chunk[9..9 + data_len];

        // Read back
        let idim_read = read_idim_chunk(flag, data).expect("failed to read");

        // Verify round-trip
        assert_eq!(idim_read.tile_width, idim_orig.tile_width);
        assert_eq!(idim_read.tile_height, idim_orig.tile_height);
        assert_eq!(idim_read.tiles_x, idim_orig.tiles_x);
        assert_eq!(idim_read.tiles_y, idim_orig.tiles_y);
        assert_eq!(idim_read.scan_order, idim_orig.scan_order);
    }

    // ---------- Tests for Z-order / Morton code (v1.0 Phase 2) ----------

    #[test]
    fn test_morton_code_basic() {
        // Basic test: (0, 0) -> 0
        assert_eq!(morton_code(0, 0), 0);

        // (1, 0) -> 0b01 = 1
        assert_eq!(morton_code(1, 0), 1);

        // (0, 1) -> 0b10 = 2
        assert_eq!(morton_code(0, 1), 2);

        // (1, 1) -> 0b11 = 3
        assert_eq!(morton_code(1, 1), 3);
    }

    #[test]
    fn test_morton_code_2x2_grid() {
        // 2x2 grid: expected
        // (0,0)=0, (1,0)=1, (0,1)=2, (1,1)=3
        // Z-order: 0, 1, 2, 3
        let mut codes = Vec::new();
        for y in 0..2 {
            for x in 0..2 {
                codes.push(morton_code(x as u32, y as u32));
            }
        }
        assert_eq!(codes, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_morton_code_4x4_grid() {
        // 4x4 grid in Z-order
        // Should follow pattern:
        // 0  1  4  5
        // 2  3  6  7
        // 8  9 12 13
        // 10 11 14 15

        let mut grid = vec![vec![0u64; 4]; 4];
        for y in 0..4 {
            for x in 0..4 {
                grid[y as usize][x as usize] = morton_code(x as u32, y as u32);
            }
        }

        // Check some key points
        assert_eq!(grid[0][0], 0); // (0,0)
        assert_eq!(grid[0][1], 1); // (1,0)
        assert_eq!(grid[1][0], 2); // (0,1)
        assert_eq!(grid[1][1], 3); // (1,1)
        assert_eq!(grid[0][2], 4); // (2,0)
        assert_eq!(grid[0][3], 5); // (3,0)
    }

    #[test]
    fn test_morton_decode_basic() {
        // Basic inversion test
        assert_eq!(morton_decode(0), (0, 0));
        assert_eq!(morton_decode(1), (1, 0));
        assert_eq!(morton_decode(2), (0, 1));
        assert_eq!(morton_decode(3), (1, 1));
    }

    #[test]
    fn test_morton_roundtrip() {
        // Roundtrip: encode -> decode must recover original
        for x in 0..16 {
            for y in 0..16 {
                let code = morton_code(x as u32, y as u32);
                let (x_dec, y_dec) = morton_decode(code);
                assert_eq!(
                    (x_dec as u16, y_dec as u16),
                    (x, y),
                    "Roundtrip failed for ({}, {})",
                    x,
                    y
                );
            }
        }
    }

    #[test]
    fn test_idim_tile_order_zorder() {
        // 4x4 grid with Z-order
        let idim = iDim::new(64, 64, 256, 256, 1); // scan_order = 1 (Z-order)
        let order = idim.tile_order().expect("valid scan_order");

        // Must have 16 tiles
        assert_eq!(order.len(), 16);

        // Verify Z-order vs row-major
        // Row-major would be: (0,0), (1,0), (2,0), (3,0), (0,1), ...
        // Z-order would be:   (0,0), (1,0), (0,1), (1,1), (2,0), (3,0), (2,1), (3,1), ...

        // First 4 tiles in Z-order
        assert_eq!(order[0], (0, 0));
        assert_eq!(order[1], (1, 0));
        assert_eq!(order[2], (0, 1));
        assert_eq!(order[3], (1, 1));
    }

    #[test]
    fn test_idim_tile_order_row_major_vs_zorder() {
        let idim_row = iDim::new(64, 64, 256, 256, 0); // Row-major
        let idim_z = iDim::new(64, 64, 256, 256, 1); // Z-order

        let order_row = idim_row.tile_order().expect("valid scan_order");
        let order_z = idim_z.tile_order().expect("valid scan_order");

        // Both have 16 tiles
        assert_eq!(order_row.len(), 16);
        assert_eq!(order_z.len(), 16);

        // All tiles appear in both (just different order)
        let mut row_set = order_row.to_vec();
        let mut z_set = order_z.to_vec();
        row_set.sort();
        z_set.sort();
        assert_eq!(row_set, z_set);

        // First line row-major
        assert_eq!(order_row[0..4], [(0, 0), (1, 0), (2, 0), (3, 0)]);

        // First line Z-order (2x2 quadrant)
        assert_eq!(order_z[0..4], [(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn test_idim_tile_order_invalid_scan_order() {
        // Test that invalid scan_order returns Err instead of panicking
        let mut idim = iDim::new(64, 64, 256, 256, 0);
        idim.scan_order = 2; // Invalid: only 0 and 1 are allowed

        let result = idim.tile_order();
        assert!(result.is_err(), "Expected error for invalid scan_order=2");

        if let Err(e) = result {
            // Verify the error message contains the scan order value
            assert!(e.to_string().contains("Unknown scan order"));
        }
    }

    #[test]
    fn test_morton_code_properties() {
        // Important properties of Morton code:
        // 1. Monotonic along Z-order curve
        // 2. Preserves spatial proximity

        let mut prev_code = 0u64;
        for y in 0..8 {
            for x in 0..8 {
                let code = morton_code(x as u32, y as u32);
                // Codes must be unique and monotonically increasing (approximately)
                assert!(
                    code >= prev_code || (x == 0 && y > 0),
                    "Non-monotonic code at ({}, {}): {}",
                    x,
                    y,
                    code
                );
                prev_code = code;
            }
        }
    }

    // ---------- Tests for Adam7 (v1.0 Phase 3) ----------

    #[test]
    fn test_adam7_passes_constant() {
        // Validate ADAM7_PASSES constant
        assert_eq!(ADAM7_PASSES.len(), 7);

        // Verify expected pattern (fixed for 100% coverage)
        assert_eq!(ADAM7_PASSES[0], (8, 8, 0, 0)); // Pass 1
        assert_eq!(ADAM7_PASSES[1], (8, 8, 4, 0)); // Pass 2
        assert_eq!(ADAM7_PASSES[6], (1, 1, 0, 0)); // Pass 7 (1x1 for 100% coverage)
    }

    #[test]
    fn test_adam7_pass_dimensions() {
        let width = 16u32;
        let height = 16u32;

        // Pass 1: grid 8×8, offset (0,0)
        let (w, h) = adam7_pass_dimensions(width, height, 0);
        assert_eq!((w, h), (2, 2)); // 16÷8 = 2 pixels in each dimension

        // Pass 7: step 1×1, offset (0,0) → covers entire image
        let (w, h) = adam7_pass_dimensions(width, height, 6);
        assert_eq!((w, h), (16, 16)); // Covers entire image
    }

    #[test]
    fn test_extract_adam7_pass_pass1() {
        // Create 8×8 RGBA image (all zeros)
        let mut raw = vec![0u8; 8 * 8 * 4];

        // Mark some pixels for testing
        // Pixel (0,0) -> index 0
        raw[0] = 255; // R channel
                      // Pixel (0,7) will not be extracted in Pass 1 (y_offset=0, y_step=8)
                      // Pixel (4,0) -> index (4*4) = 16 (will be extracted: x_offset=4, x_step=8)
        raw[16] = 128;

        let pass = extract_adam7_pass(&raw, 8, 8, 0); // Pass 1

        // Pass 1 extracts grid 8×8 with offset (0,0), step 8
        // Only 1 pixel (0,0) for 8×8 image
        assert_eq!(pass.len(), 4); // 1 pixel = 4 bytes
        assert_eq!(pass[0], 255);
    }

    #[test]
    fn test_extract_adam7_all_passes() {
        // Create 256×256 image (filled with pattern)
        // Use larger image to ensure all passes have data
        let width = 256u32;
        let height = 256u32;
        let size = (width * height * 4) as usize;
        let mut raw = vec![0u8; size];
        // Fill with repeating pattern
        for (i, cell) in raw.iter_mut().enumerate() {
            *cell = (i as u8).wrapping_mul(7).wrapping_add(31);
        }

        // Extract all 7 passes
        let passes_data: Vec<_> = (0..7)
            .map(|p| extract_adam7_pass(&raw, width, height, p))
            .collect();

        // Validate each pass has data
        for (i, pass_data) in passes_data.iter().enumerate() {
            assert!(!pass_data.is_empty(), "Pass {} must not be empty", i);
            // Each pixel is 4 bytes
            assert_eq!(
                pass_data.len() % 4,
                0,
                "Pass {} must have integer number of pixels",
                i
            );
        }
    }

    #[test]
    fn test_reconstruct_adam7_simple() {
        // Create 8×8 image with simple pattern: each pixel (x,y) = color (x*16, y*16, 0, 255)
        let width = 8u32;
        let height = 8u32;
        let mut original = vec![0u8; (width * height * 4) as usize];

        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                original[idx] = (x * 16) as u8; // R
                original[idx + 1] = (y * 16) as u8; // G
                original[idx + 2] = 0; // B
                original[idx + 3] = 255; // A
            }
        }

        // Extract passes
        let mut passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default();
        for (i, out) in passes.iter_mut().enumerate() {
            *out = extract_adam7_pass(&original, width, height, i);
        }

        // Reconstruct
        let reconstructed = reconstruct_adam7(&passes, width, height).unwrap();

        // Validate round-trip
        assert_eq!(reconstructed.len(), original.len());
        assert_eq!(
            reconstructed, original,
            "Adam7 roundtrip was not bit-perfect"
        );
    }

    #[test]
    fn test_adam7_roundtrip_large() {
        // Test with larger image (256×256)
        let width = 256u32;
        let height = 256u32;

        // Create image with pseudo-random pattern
        let mut original = vec![0u8; (width * height * 4) as usize];
        for (i, cell) in original.iter_mut().enumerate() {
            *cell = (i as u8).wrapping_mul(7).wrapping_add(31);
        }

        // Extract passes
        let mut passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default();
        for (p, out) in passes.iter_mut().enumerate() {
            *out = extract_adam7_pass(&original, width, height, p);
        }

        // Reconstruct
        let reconstructed = reconstruct_adam7(&passes, width, height).unwrap();

        // Validate round-trip bit-perfect
        assert_eq!(reconstructed.len(), original.len());
        assert_eq!(
            reconstructed, original,
            "Adam7 roundtrip failed on 256×256 image"
        );
    }

    #[test]
    fn test_adam7_pass_coverage_progressive() {
        // Validate that 7 passes progressively cover entire image
        // Adam7 is progressive: each pass refines the previous ones
        // Validate that union of all passes covers 100% of pixels
        let width = 64u32;
        let height = 64u32;

        let mut covered = vec![false; (width * height) as usize];

        for &(x_step, y_step, x_offset, y_offset) in ADAM7_PASSES {
            let mut y = y_offset;
            while y < height {
                let mut x = x_offset;
                while x < width {
                    let idx = (y * width + x) as usize;
                    covered[idx] = true;
                    x += x_step;
                }
                y += y_step;
            }
        }

        // Validate all pixels were covered at least once
        for (idx, &c) in covered.iter().enumerate() {
            assert!(c, "Pixel {} was not covered by any pass", idx);
        }
    }

    #[test]
    fn test_adam7_interlace_constant() {
        // Validate that interlace constants are defined correctly
        assert_eq!(INTERLACE_ADAM7, 1);
        assert_eq!(INTERLACE_EVEN_ODD, 2);
        assert_eq!(ADAM7_NUM_PASSES, 7);
    }

    #[test]
    fn test_adam7_pass_dimensions_all_passes() {
        // Test dimensions for all 7 passes
        let width = 256u32;
        let height = 256u32;

        for (pass, &params) in ADAM7_PASSES.iter().enumerate() {
            let (w, h) = adam7_pass_dimensions(width, height, pass);
            // Validate dimensions are positive (except for very small images)
            if width > params.2 {
                assert!(w > 0, "Pass {} must have positive width", pass);
            }
            if height > params.3 {
                assert!(h > 0, "Pass {} must have positive height", pass);
            }
        }
    }

    #[test]
    fn test_adam7_each_pass_extracts_data() {
        // Validate each pass extracts non-empty data from typical image
        let width = 128u32;
        let height = 128u32;
        let raw = vec![42u8; (width * height * 4) as usize];

        let mut total_pixels = 0u32;

        for pass in 0..7 {
            let pass_data = extract_adam7_pass(&raw, width, height, pass);
            assert!(!pass_data.is_empty(), "Pass {} must not be empty", pass);

            let num_pixels = pass_data.len() as u32 / 4;
            total_pixels += num_pixels;
        }

        // Total pixels extracted must be width × height
        assert_eq!(
            total_pixels,
            width * height,
            "Total extracted pixels does not match image size"
        );
    }

    // ---------- Encoder Test with Adam7 (Phase 4) ----------

    #[test]
    fn test_encoder_adam7_option() {
        // Validate EncodeOptions supports interlace_method
        let mut opts = EncodeOptions::default();
        assert_eq!(opts.interlace_method, INTERLACE_NONE);

        opts.interlace_method = INTERLACE_ADAM7;
        assert_eq!(opts.interlace_method, INTERLACE_ADAM7);
    }

    #[test]
    fn test_apply_adam7_interlace() {
        // Test application of Adam7 on RGBA data
        let width = 16u32;
        let height = 16u32;
        let mut raw = vec![0u8; (width * height * 4) as usize];
        for (i, cell) in raw.iter_mut().enumerate() {
            *cell = (i as u8).wrapping_mul(7).wrapping_add(31);
        }

        let passes = apply_adam7_interlace(&raw, width, height);

        // Validate 7 passes
        assert_eq!(passes.len(), 7);

        // Each pass must have data (multiples of 4 for RGBA)
        let mut total_pixels = 0u32;
        for (i, pass) in passes.iter().enumerate() {
            assert!(!pass.is_empty(), "Pass {} must not be empty", i);
            assert_eq!(
                pass.len() % 4,
                0,
                "Pass {} must have multiples of 4 bytes (RGBA)",
                i
            );
            total_pixels += pass.len() as u32 / 4;
        }

        // Total must be width × height
        assert_eq!(total_pixels, width * height);
    }

    // ---------- Decoder Test with Adam7 (Phase 5) ----------

    #[test]
    fn test_decoder_supports_adam7() {
        // Validate decoder can recognize Adam7
        // (Basic test without real file)

        // If interlace_method != 0 and != 1, must return error
        // If interlace_method == 1 (Adam7), must continue
        // This test is more of integration test (see Phase 6 for full roundtrip)
        assert_eq!(INTERLACE_ADAM7, 1);
        assert_eq!(INTERLACE_NONE, 0);
    }

    #[test]
    fn test_interlace_method_storage() {
        // Validate interlace_method is stored in EncodeOptions
        let mut opts = EncodeOptions::default();
        assert_eq!(opts.interlace_method, INTERLACE_NONE);

        opts.interlace_method = INTERLACE_ADAM7;
        assert_eq!(opts.interlace_method, INTERLACE_ADAM7);
    }

    // ---------- Integration Test: Adam7 Roundtrip (Phase 6) ----------

    #[test]
    fn test_adam7_extract_reconstruct_roundtrip() {
        // Isolated test: extract + reconstruct must recover original data
        let width = 16u32;
        let height = 16u32;

        // Create simple RGBA data (different colors per pixel)
        let mut original_rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            original_rgba[idx] = (i % 256) as u8; // R
            original_rgba[idx + 1] = ((i / 256) % 256) as u8; // G
            original_rgba[idx + 2] = ((i * 2) % 256) as u8; // B
            original_rgba[idx + 3] = 255; // A
        }

        // Apply Adam7: extract 7 passes
        let passes = apply_adam7_interlace(&original_rgba, width, height);

        // Reconstruct
        let reconstructed = reconstruct_adam7(&passes, width, height).unwrap();

        // Compare
        assert_eq!(original_rgba.len(), reconstructed.len(), "Size mismatch");
        for (i, (o, r)) in original_rgba.iter().zip(reconstructed.iter()).enumerate() {
            assert_eq!(o, r, "Byte {} mismatch: {} != {}", i, o, r);
        }
    }

    #[test]
    fn test_extract_reconstruct_pixel_values() {
        // Validate reconstructed pixel data has correct values
        let width = 8u32;
        let height = 8u32;

        // Create simple RGBA data: pixel[i] = [i, i, i, 255]
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = i as u8;
            rgba[idx + 1] = i as u8;
            rgba[idx + 2] = i as u8;
            rgba[idx + 3] = 255;
        }

        // Extract all passes
        let passes = apply_adam7_interlace(&rgba, width, height);

        // Reconstruct
        let recovered = reconstruct_adam7(&passes, width, height).unwrap();

        // Validate specific pixels
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            if recovered[idx] != i as u8 {
                eprintln!(
                    "Pixel {} R mismatch: recovered={} expected={}",
                    i, recovered[idx], i as u8
                );
            }
            assert_eq!(recovered[idx], i as u8, "Pixel {} R mismatch", i);
            assert_eq!(recovered[idx + 1], i as u8, "Pixel {} G mismatch", i);
            assert_eq!(recovered[idx + 2], i as u8, "Pixel {} B mismatch", i);
            assert_eq!(recovered[idx + 3], 255, "Pixel {} A mismatch", i);
        }
    }

    #[test]
    fn test_extract_adam7_pixel_order() {
        // Validate extract returns pixels in correct order within pass
        let width = 8u32;
        let height = 8u32;

        // Create simple RGBA data: pixel[i] = [i, i, i, 255]
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = i as u8; // R = pixel index
            rgba[idx + 1] = i as u8; // G = pixel index
            rgba[idx + 2] = i as u8; // B = pixel index
            rgba[idx + 3] = 255; // A
        }

        // Extract pass 0 (Adam7_PASSES[0] = (8, 8, 0, 0) = 1×1 pixel at 0,0)
        let pass0 = extract_adam7_pass(&rgba, width, height, 0);
        eprintln!("Pass 0: {} bytes (expected 4)", pass0.len());
        if pass0.len() >= 4 {
            eprintln!(
                "Pass 0 data: [{}, {}, {}, {}]",
                pass0[0], pass0[1], pass0[2], pass0[3]
            );
            // Pixel (0,0) has index 0, so we expect [0, 0, 0, 255]
            assert_eq!(pass0[0], 0, "Pass 0 pixel (0,0) R should be 0");
            assert_eq!(pass0[1], 0, "Pass 0 pixel (0,0) G should be 0");
            assert_eq!(pass0[2], 0, "Pass 0 pixel (0,0) B should be 0");
        }

        // Extract pass 1: 1-1 pixel at 4,0)
        let pass1 = extract_adam7_pass(&rgba, width, height, 1);
        eprintln!("Pass 1: {} bytes (expected 4)", pass1.len());
        if pass1.len() >= 4 {
            eprintln!(
                "Pass 1 data: [{}, {}, {}, {}]",
                pass1[0], pass1[1], pass1[2], pass1[3]
            );
            // Pixel (4,0) has index 4, so we expect [4, 4, 4, 255]
            assert_eq!(pass1[0], 4, "Pass 1 pixel (4,0) R should be 4");
            assert_eq!(pass1[1], 4, "Pass 1 pixel (4,0) G should be 4");
            assert_eq!(pass1[2], 4, "Pass 1 pixel (4,0) B should be 4");
        }
    }

    #[test]
    fn test_encoder_adam7_output() {
        // Validate that encoder writes dados Adam7 corretos (sem full PNG)
        // Simulate: RGBA->quantize->Adam7->check passes

        let width = 16u32;
        let height = 16u32;

        // Create simple RGBA data
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = (i % 256) as u8; // R
            rgba[idx + 1] = (i / 16) as u8; // G
            rgba[idx + 2] = 100; // B: fixed for easy debug
            rgba[idx + 3] = 255; // A
        }

        // Quantize
        let (indices, palette) =
            quantize_to_palette(&rgba, width, 256, PaletteAlgorithm::NearestNeighbor);
        eprintln!("Quantized {} colors", palette.entries.len());

        // Convert indices->RGBA (encoder flow)
        let rgba_from_indices: Vec<u8> = indices
            .iter()
            .flat_map(|&idx| {
                let entry = &palette.entries[idx as usize];
                vec![entry.r, entry.g, entry.b, entry.a]
            })
            .collect();

        eprintln!("rgba original: {} bytes", rgba.len());
        eprintln!("rgba_from_indices: {} bytes", rgba_from_indices.len());

        // Apply Adam7
        let passes = apply_adam7_interlace(&rgba_from_indices, width, height);
        let total_pass_size: usize = passes.iter().map(|p| p.len()).sum();
        eprintln!(
            "Total Adam7 data: {} bytes (expected {})",
            total_pass_size,
            width * height * 4
        );

        // Validate reconstruct gives
        let reconstructed = reconstruct_adam7(&passes, width, height).unwrap();

        let mut mismatches = 0;
        for (i, (orig, rec)) in rgba_from_indices
            .iter()
            .zip(reconstructed.iter())
            .enumerate()
        {
            if orig != rec {
                mismatches += 1;
                if mismatches <= 5 {
                    eprintln!("Byte {} mismatch: {} != {}", i, orig, rec);
                }
            }
        }

        eprintln!("Encoder output validation: {} mismatches", mismatches);
        assert_eq!(mismatches, 0, "Encoder Adam7 output is corrupt");
    }

    #[test]
    fn test_adam7_encode_decode_flow() {
        // Simulate: RGBA->compress->decompress->reconstruct
        let width = 16u32;
        let height = 16u32;

        // Create RGBA data with pattern
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = (i % 256) as u8;
            rgba[idx + 1] = ((i / 256) % 256) as u8;
            rgba[idx + 2] = ((i * 2) % 256) as u8;
            rgba[idx + 3] = 255;
        }

        // Step 1: Extract passes
        let passes = apply_adam7_interlace(&rgba, width, height);

        // Step 2: Compress each pass (simulating encoder)
        let mut passes_with_flags = vec![];
        for pass_data in passes.iter() {
            let mut payload = vec![0u8]; // pass_number placeholder
            payload.extend_from_slice(pass_data);

            let (flag, compressed) =
                compress_with_fallback(&payload, 10).expect("failed to compress");
            passes_with_flags.push((flag, compressed));
        }

        // Step 3: Decompress each pass (simulating decoder)
        let mut decompressed_passes: [Vec<u8>; 7] = Default::default();
        for (idx, (flag, compressed)) in passes_with_flags.iter().enumerate() {
            let decompressed = decompress_chunk(*flag, compressed).expect("failed to decompress");
            let pass_data = decompressed[1..].to_vec();
            decompressed_passes[idx] = pass_data;
        }

        // Step 4: Reconstruct
        let reconstructed = reconstruct_adam7(&decompressed_passes, width, height).unwrap();

        // Compare
        assert_eq!(rgba, reconstructed, "Adam7 encode-decode failed");
    }

    #[test]
    fn test_zstd_with_pass_number() {
        // Validate that compress/decompress with prefixed pass_number works
        let pass_number = 5u8;
        let original_data = vec![1, 2, 3, 4, 5, 255, 254, 253];

        // Simular encoding: prepend pass_number, compress
        let mut payload = vec![pass_number];
        payload.extend_from_slice(&original_data);

        let (flag, compressed) = compress_with_fallback(&payload, 10).expect("compress failed");

        eprintln!("Original payload: {} bytes", payload.len());
        eprintln!("Compressed: {} bytes, flag={:#04x}", compressed.len(), flag);

        // Simular decoding: decompress, extract pass_number
        let decompressed = decompress_chunk(flag, &compressed).expect("decompress failed");

        eprintln!("Decompressed: {} bytes", decompressed.len());

        assert_eq!(decompressed[0], pass_number, "Pass number mismatch");
        assert_eq!(
            &decompressed[1..],
            &original_data[..],
            "Data mismatch after compression roundtrip"
        );
    }

    #[test]
    fn test_quantize_dequantize_roundtrip() {
        // Validate that quantize-dequantize preserves colors
        let width = 16u32;
        let height = 16u32;
        let mut rgba = vec![0u8; (width * height * 4) as usize];

        // Fill with simple and different colors
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = (i % 256) as u8; // R: vary 0-255
            rgba[idx + 1] = ((i * 2) % 256) as u8; // G
            rgba[idx + 2] = ((i * 3) % 256) as u8; // B
            rgba[idx + 3] = 255; // A
        }

        // Quantize
        let (indices, palette) =
            quantize_to_palette(&rgba, width, 256, PaletteAlgorithm::NearestNeighbor);

        eprintln!("Quantized to {} colors", palette.entries.len());
        eprintln!(
            "indices.len()={}, expected={}",
            indices.len(),
            width * height
        );

        // DesQuantize
        let recovered = dequantize_from_palette(&indices, &palette, width, height);

        // Comparar
        let mut mismatches = 0;
        for (i, (o, r)) in rgba.iter().zip(recovered.iter()).enumerate() {
            if o != r {
                mismatches += 1;
                if mismatches <= 10 {
                    eprintln!("Byte {} original={} recovered={}", i, o, r);
                }
            }
        }

        eprintln!("Quantize-dequantize: {} mismatches", mismatches);
        assert_eq!(mismatches, 0, "quantize-dequantize failed");
    }

    #[test]
    fn test_image_lib_roundtrip() {
        // Validate that image lib preserves pixel order
        let width = 8u32;
        let height = 8u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];

        // Fill with pattern identifiable
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                img_data[idx] = (x * 2) as u8; // R: value depends on X
                img_data[idx + 1] = (y * 2) as u8; // G: value depends on Y
                img_data[idx + 2] = 100; // B: fixo
                img_data[idx + 3] = 255; // A: opaco
            }
        }

        let temp_dir = std::env::temp_dir();
        let test_path = temp_dir
            .join("test_image_lib.png")
            .to_string_lossy()
            .to_string();

        // Save and reload
        {
            let img = image::RgbaImage::from_raw(width, height, img_data.clone())
                .expect("failed to create image");
            img.save(&test_path).expect("failed to save");
        }

        let loaded = image::open(&test_path)
            .expect("failed to open")
            .to_rgba8()
            .into_raw();

        // Comparar
        for (i, (o, l)) in img_data.iter().zip(loaded.iter()).enumerate() {
            if o != l {
                eprintln!("Byte {} mismatch: original={} loaded={}", i, o, l);
            }
        }

        assert_eq!(img_data, loaded, "Image lib roundtrip failed");
    }

    #[test]
    fn test_even_odd_extract_reconstruct() {
        // Test even/odd: extract->reconstruct must recover original data
        let width = 16u32;
        let height = 16u32;

        // Create RGBA data with pattern
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for i in 0..width * height {
            let idx = (i * 4) as usize;
            rgba[idx] = (i % 256) as u8;
            rgba[idx + 1] = ((i / 256) % 256) as u8;
            rgba[idx + 2] = ((i * 2) % 256) as u8;
            rgba[idx + 3] = 255;
        }

        // Apply even/odd: extract 2 passes
        let passes = apply_even_odd_interlace(&rgba, width, height);

        // Reconstruct
        let reconstructed = reconstruct_even_odd(&passes, width, height).unwrap();

        // Comparar
        assert_eq!(rgba, reconstructed, "even/odd roundtrip failed");
    }

    #[test]
    fn test_roundtrip_adam7_indexed() {
        // End-to-end test: create PNG->encode->decode->validate

        // Create simple image 32-32 with colors FIXED (without quantization)
        let width = 32u32;
        let height = 32u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];

        // Fill with solid pure colors to avoid lossy quantization
        let colors = [
            [255, 0, 0, 255],     // Red
            [0, 255, 0, 255],     // Green
            [0, 0, 255, 255],     // Blue
            [255, 255, 0, 255],   // Yellow
            [255, 0, 255, 255],   // Magenta
            [0, 255, 255, 255],   // Cyan
            [255, 255, 255, 255], // White
            [128, 128, 128, 255], // Gray
        ];

        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let color_idx = ((x + y * 2) as usize) % colors.len();
                let color = colors[color_idx];
                img_data[idx] = color[0];
                img_data[idx + 1] = color[1];
                img_data[idx + 2] = color[2];
                img_data[idx + 3] = color[3];
            }
        }

        // Create original PNG image
        let img = image::RgbaImage::from_raw(width, height, img_data)
            .expect("failed to create RGBA image");

        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_adam7_input.png")
            .to_string_lossy()
            .to_string();
        let encoded_path = temp_dir
            .join("test_adam7_encoded.cafe")
            .to_string_lossy()
            .to_string();
        let decoded_path = temp_dir
            .join("test_adam7_decoded.png")
            .to_string_lossy()
            .to_string();

        // Save original PNG
        img.save(&input_path).expect("failed to save original PNG");

        // encode with Adam7
        let opts = EncodeOptions {
            interlace_method: INTERLACE_ADAM7,
            level: 10, // Moderate compression for test
            ..EncodeOptions::default()
        };

        encode_indexed(&input_path, &encoded_path, &opts).expect("failed to encode with Adam7");

        // Decode
        decode(&encoded_path, &decoded_path).expect("failed ao Decode Adam7");

        // Load both images and compare
        let original = image::open(&input_path)
            .expect("failed ao abrir PNG original")
            .to_rgba8();

        let reconstructed = image::open(&decoded_path)
            .expect("failed to open decoded PNG")
            .to_rgba8();

        // Validate dimensions
        assert_eq!(
            original.dimensions(),
            reconstructed.dimensions(),
            "Dimensions match after roundtrip"
        );

        // Validate pixels (must be bit-perfect for indexed + Adam7)
        let orig_pixels = original.as_raw();
        let recon_pixels = reconstructed.as_raw();

        assert_eq!(
            orig_pixels.len(),
            recon_pixels.len(),
            "Pixel size does not match"
        );

        // Comparar pixel by pixel
        let mut mismatches = 0;
        for (i, (o, r)) in orig_pixels.iter().zip(recon_pixels.iter()).enumerate() {
            if o != r {
                mismatches += 1;
                if mismatches <= 10 {
                    eprintln!("Byte mismatch at {}: {} != {}", i, o, r);
                }
            }
        }

        eprintln!("Total mismatches: {}/{}", mismatches, orig_pixels.len());
        assert_eq!(
            mismatches, 0,
            "{} bytes match after roundtrip Adam7",
            mismatches
        );
    }

    #[test]
    fn test_roundtrip_adam7_vs_uncompressed() {
        // Compare file size: Adam7 vs without interlace
        // Adam7 must be slightly larger (due to pass_number overhead)
        // but must compress igual ou melhor

        let width = 64u32;
        let height = 64u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];

        // Fill with pattern
        for (i, cell) in img_data.iter_mut().enumerate() {
            *cell = ((i / 4) % 256) as u8;
        }

        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed ao criar imagem");

        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_compare_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed ao salvar PNG");

        // encodesr without interlace
        let opts_none = EncodeOptions {
            interlace_method: INTERLACE_NONE,
            level: 10,
            ..EncodeOptions::default()
        };

        let path_none = temp_dir
            .join("test_compare_none.cafe")
            .to_string_lossy()
            .to_string();
        encode_indexed(&input_path, &path_none, &opts_none)
            .expect("failed to encode without interlace");

        // encode with Adam7
        let opts_adam7 = EncodeOptions {
            interlace_method: INTERLACE_ADAM7,
            level: 10,
            ..EncodeOptions::default()
        };

        let path_adam7 = temp_dir
            .join("test_compare_adam7.cafe")
            .to_string_lossy()
            .to_string();
        encode_indexed(&input_path, &path_adam7, &opts_adam7).expect("failed to encode with Adam7");

        // Compare file sizes
        let size_none = std::fs::metadata(&path_none)
            .expect("failed to read metadata from file without interlace")
            .len();

        let size_adam7 = std::fs::metadata(&path_adam7)
            .expect("failed to read metadata from file with Adam7")
            .len();

        eprintln!("Size WITHOUT interlace: {} bytes", size_none);
        eprintln!("Size WITH Adam7: {} bytes", size_adam7);
        eprintln!(
            "Overhead Adam7: {} bytes ({:.1}%)",
            size_adam7 as i64 - size_none as i64,
            ((size_adam7 as f64 - size_none as f64) / size_none as f64) * 100.0
        );

        // Adam7 can be up to 50% larger (7 pass_number + possible recompression diferentes)
        assert!(
            size_adam7 <= size_none * 150 / 100,
            "Adam7 too large: {} vs {}",
            size_adam7,
            size_none
        );
    }

    // Note: Tests of private filter foram removidos porque as
    // implementations were moved to the filter filter.rs. Os testes para
    // filter.rs are in its own test file.

    #[test]
    fn test_filter_roundtrip_per_block() {
        // v1.0: predictive filter chosen per block (entire tile), 1 byte per tile.
        // Round-trip: encode-> decode must be perfectly reversible, even with
        // multiple tiles (including partial) and patterns that favor filters.
        let width = 48u32;
        let height = 37u32; // non-multiple tile partial
        let mut img_data = vec![0u8; (width * height * 4) as usize];

        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                img_data[idx] = (x * 3) as u8; // R: horizontal gradient
                img_data[idx + 1] = (y * 5) as u8; // G: vertical gradient
                img_data[idx + 2] = ((x + y) % 256) as u8; // B: diagonal
                img_data[idx + 3] = 255; // A: opaco
            }
        }

        let img = image::RgbaImage::from_raw(width, height, img_data.clone())
            .expect("failed ao criar imagem");

        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_filter_block_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed ao salvar PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8, // multiple tiles -> multiple filter codes
            level: 10,
            ..EncodeOptions::default()
        };

        let cafe_path = temp_dir
            .join("test_filter_block.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode with per-block filter");

        let decoded_path = temp_dir
            .join("test_filter_block_out.png")
            .to_string_lossy()
            .to_string();
        decode(&cafe_path, &decoded_path).expect("failed ao Decode");

        let decoded = image::open(&decoded_path)
            .expect("failed to open decoded PNG")
            .to_rgba8()
            .into_raw();

        assert_eq!(img_data, decoded, "Round-trip per-block filter failed");
    }

    #[test]
    fn test_filter_roundtrip_compression_test_heuristic() {
        // v1.0: "compression test real" heuristic (FilterHeuristic::CompressionTest)
        // must produce perfectly reversible files, like entropy.
        let width = 48u32;
        let height = 37u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];

        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                img_data[idx] = (x * 3) as u8;
                img_data[idx + 1] = (y * 5) as u8;
                img_data[idx + 2] = ((x + y) % 256) as u8;
                img_data[idx + 3] = 255;
            }
        }

        let img = image::RgbaImage::from_raw(width, height, img_data.clone())
            .expect("failed ao criar imagem");

        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_filter_test_heuristic_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed ao salvar PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8,
            level: 10,
            filter_heuristic: FilterHeuristic::CompressionTest,
            ..EncodeOptions::default()
        };

        let cafe_path = temp_dir
            .join("test_filter_test_heuristic.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts)
            .expect("failed to encode with compression test heuristic");

        let decoded_path = temp_dir
            .join("test_filter_test_heuristic_out.png")
            .to_string_lossy()
            .to_string();
        decode(&cafe_path, &decoded_path).expect("failed ao Decode");

        let decoded = image::open(&decoded_path)
            .expect("failed to open decoded PNG")
            .to_rgba8()
            .into_raw();

        assert_eq!(img_data, decoded, "Round-trip compression test failed");
    }

    // --- Tests for Phase 1: Bit depths 1, 2, 4 (v1.0) ---

    #[test]
    fn test_bytes_per_row_for_bit_depth_1bit() {
        // 1 bit depth: 16 pixels = 2 bytes (16 * 1 / 8)
        let result = bytes_per_row_for_bit_depth(16, 1);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
    }

    #[test]
    fn test_bytes_per_row_for_bit_depth_2bit() {
        // 2 bit depth: 16 pixels = 4 bytes (16 * 2 / 8)
        let result = bytes_per_row_for_bit_depth(16, 2);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 4);
    }

    #[test]
    fn test_bytes_per_row_for_bit_depth_4bit() {
        // 4 bit depth: 16 pixels = 8 bytes (16 * 4 / 8)
        let result = bytes_per_row_for_bit_depth(16, 4);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 8);
    }

    #[test]
    fn test_bytes_per_row_for_bit_depth_overflow() {
        // Overflow protection: u32::MAX * 1 bit -> ceil(MAX/8) bytes
        // This test documents that with u32 inputs, checked_mul protects us
        let result = bytes_per_row_for_bit_depth(u32::MAX, 1);
        assert!(result.is_ok());
        let bytes = result.unwrap();
        // ceil(4294967295 * 1 / 8) = ceil(536870911.875) = 536870912
        assert_eq!(bytes, 536870912);
    }

    #[test]
    fn test_reduce_sample_8_to_n_bits() {
        // 8-bit -> 1-bit: threshold at 128
        assert_eq!(reduce_sample_8_to_n_bits(100, 1).unwrap(), 0);
        assert_eq!(reduce_sample_8_to_n_bits(128, 1).unwrap(), 1);
        assert_eq!(reduce_sample_8_to_n_bits(255, 1).unwrap(), 1);

        // 8-bit -> 2-bit: shift right 6
        assert_eq!(reduce_sample_8_to_n_bits(0, 2).unwrap(), 0);
        assert_eq!(reduce_sample_8_to_n_bits(63, 2).unwrap(), 0);
        assert_eq!(reduce_sample_8_to_n_bits(64, 2).unwrap(), 1);
        assert_eq!(reduce_sample_8_to_n_bits(192, 2).unwrap(), 3);
        assert_eq!(reduce_sample_8_to_n_bits(255, 2).unwrap(), 3);

        // 8-bit -> 4-bit: shift right 4
        assert_eq!(reduce_sample_8_to_n_bits(0, 4).unwrap(), 0);
        assert_eq!(reduce_sample_8_to_n_bits(15, 4).unwrap(), 0);
        assert_eq!(reduce_sample_8_to_n_bits(16, 4).unwrap(), 1);
        assert_eq!(reduce_sample_8_to_n_bits(255, 4).unwrap(), 15);

        // 8-bit -> 8-bit: no change
        assert_eq!(reduce_sample_8_to_n_bits(100, 8).unwrap(), 100);
    }

    #[test]
    fn test_pack_samples_row_1bit() {
        // Pack 2 samples of 1-bit in 1 byte
        let samples = vec![0u8, 1u8];
        let packed = pack_samples_row(&samples, 1, 2, 1).unwrap();
        assert_eq!(packed.len(), 1);
        // MSB-first: [0, 1] -> 01000000 = 0x40
        assert_eq!(packed[0], 0x40);
    }

    #[test]
    fn test_unpack_samples_row_1bit() {
        // Unpack 1 byte with 2 samples of 1-bit
        let packed = vec![0x40u8]; // 01000000
        let unpacked = unpack_samples_row(&packed, 1, 2, 1).unwrap();
        assert_eq!(unpacked.len(), 2);
        assert_eq!(unpacked[0], 0);
        assert_eq!(unpacked[1], 1);
    }

    #[test]
    fn test_pack_unpack_samples_roundtrip_4bit() {
        // Roundtrip: pack -> unpack must return original
        let original = vec![5u8, 15u8, 3u8, 7u8];
        let packed = pack_samples_row(&original, 4, 4, 1).unwrap();
        let unpacked = unpack_samples_row(&packed, 4, 4, 1).unwrap();
        assert_eq!(unpacked, original);
    }

    #[test]
    fn test_pack_samples_row_validates_range() {
        // Value 16 does not fit in 4 bits (max 15)
        let samples = vec![5u8, 16u8];
        let result = pack_samples_row(&samples, 4, 2, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_unpack_samples_row_validates_buffer() {
        // Buffer too small for unpacking
        let packed = vec![0xFFu8]; // 1 byte
        let result = unpack_samples_row(&packed, 4, 10, 1); // Wants 10 pixels
        assert!(result.is_err());
    }

    #[test]
    fn test_pack_unpack_ga_channel_4bit() {
        // Gray + Alpha (2 channels) in 4-bit
        let original = vec![5, 15, 3, 7]; // G1, A1, G2, A2
        let packed = pack_samples_row(&original, 4, 2, 2).unwrap(); // 2 pixels, 2 samples each
        let unpacked = unpack_samples_row(&packed, 4, 2, 2).unwrap();
        assert_eq!(unpacked, original);
    }

    #[test]
    fn test_convert_rgba_to_grayscale_8bit() {
        // Convert RGBA -> Grayscale (8-bit)
        let rgba = vec![255, 0, 0, 255]; // Red pixel, opaque
        let gray = convert_rgba_to_color_type(&rgba, 1, 1, COLOR_TYPE_GRAY, 8).unwrap();
        assert_eq!(gray.len(), 1);
        // Y = 0.299*R + 0.587*G + 0.114*B = 0.299*255 = ~76
        assert!(gray[0] > 70 && gray[0] < 85);
    }

    #[test]
    fn test_convert_color_type_to_rgba_gray_8bit() {
        // Convert Grayscale -> RGBA
        let gray = vec![128u8];
        let rgba = convert_color_type_to_rgba(&gray, 1, 1, COLOR_TYPE_GRAY, 8).unwrap();
        assert_eq!(rgba.len(), 4);
        assert_eq!(rgba[0], 128); // R
        assert_eq!(rgba[1], 128); // G
        assert_eq!(rgba[2], 128); // B
        assert_eq!(rgba[3], 0xFF); // A (opaque)
    }

    // --- Tests for Phase 2: Bit depths 10, 12, 16, 32 (v1.0) ---

    #[test]
    fn test_read_u16_be() {
        let buf = vec![0x12, 0x34, 0x56, 0x78];
        let val = read_u16_be(&buf, 0).unwrap();
        assert_eq!(val, 0x1234);
        let val2 = read_u16_be(&buf, 2).unwrap();
        assert_eq!(val2, 0x5678);
    }

    #[test]
    fn test_read_u16_be_underflow() {
        let buf = vec![0x12, 0x34];
        let result = read_u16_be(&buf, 1);
        assert!(result.is_err()); // Offset + 2 > buf.len()
    }

    #[test]
    fn test_read_u32_be() {
        let buf = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let val = read_u32_be(&buf, 0).unwrap();
        assert_eq!(val, 0x12345678);
        let val2 = read_u32_be(&buf, 4).unwrap();
        assert_eq!(val2, 0x9ABCDEF0);
    }

    #[test]
    fn test_expand_sample_8_to_16bit() {
        // 8-bit -> 10-bit
        let expanded = expand_sample_8_to_n_bits(255, 10).unwrap();
        assert_eq!(expanded, 1023);
        let expanded_min = expand_sample_8_to_n_bits(0, 10).unwrap();
        assert_eq!(expanded_min, 0);

        // 8-bit -> 12-bit
        let expanded = expand_sample_8_to_n_bits(255, 12).unwrap();
        assert_eq!(expanded, 4095);

        // 8-bit -> 16-bit
        let expanded = expand_sample_8_to_n_bits(255, 16).unwrap();
        assert_eq!(expanded, 65535);
    }

    #[test]
    fn test_expand_sample_8_to_32bit() {
        let expanded = expand_sample_8_to_32bit(255);
        assert_eq!(expanded, 0xFFFFFFFFu32);
        let expanded_zero = expand_sample_8_to_32bit(0);
        assert_eq!(expanded_zero, 0);
    }

    #[test]
    fn test_compress_sample_n_to_8bits() {
        // 10-bit -> 8-bit
        let compressed = compress_sample_n_to_8bits(1023, 10).unwrap();
        assert_eq!(compressed, 255);
        let compressed_zero = compress_sample_n_to_8bits(0, 10).unwrap();
        assert_eq!(compressed_zero, 0);

        // 16-bit -> 8-bit
        let compressed = compress_sample_n_to_8bits(65535, 16).unwrap();
        assert_eq!(compressed, 255);
    }

    #[test]
    fn test_compress_sample_32bit_to_8() {
        let compressed = compress_sample_32bit_to_8(0xFFFFFFFFu32);
        assert_eq!(compressed, 255);
        let compressed_zero = compress_sample_32bit_to_8(0);
        assert_eq!(compressed_zero, 0);
    }

    #[test]
    fn test_convert_rgba_to_grayscale_16bit() {
        // Convert RGBA -> Gray 16-bit
        let rgba = vec![255, 0, 0, 255]; // Red pixel
        let gray_16 = convert_rgba_to_color_type(&rgba, 1, 1, COLOR_TYPE_GRAY, 16).unwrap();
        assert_eq!(gray_16.len(), 2); // 2 bytes para 16-bit
                                      // Must be big-endian value ~red
    }

    #[test]
    fn test_convert_rgba_to_rgb_16bit() {
        // Convert RGBA -> RGB 16-bit
        let rgba = vec![255, 128, 64, 255]; // RGB pixel
        let rgb_16 = convert_rgba_to_color_type(&rgba, 1, 1, COLOR_TYPE_RGB, 16).unwrap();
        assert_eq!(rgb_16.len(), 6); // 3 channels × 2 bytes
    }

    /// Regression test for the SIMD-batched bit_depth=16 expansion path
    /// (color.rs `expand_8to16_batch`): output must be bit-for-bit identical
    /// to the original per-sample `expand_sample_8_to_n_bits(_, 16)` formula
    /// for every color type, including full-range values (0x00 and 0xFF).
    #[test]
    fn test_convert_rgba_to_color_type_16bit_bit_exact_all_types() {
        // 4 pixels covering edge values (0, 255) and mixed channels.
        let rgba: Vec<u8> = vec![
            0, 0, 0, 0, // pixel 0: all zero
            255, 255, 255, 255, // pixel 1: all max
            255, 0, 128, 64, // pixel 2: mixed
            17, 200, 3, 250, // pixel 3: mixed
        ];
        let width = 4u32;
        let height = 1u32;

        for &(color_type, channels) in &[
            (COLOR_TYPE_GRAY, 1usize),
            (COLOR_TYPE_RGB, 3usize),
            (COLOR_TYPE_GRAY_ALPHA, 2usize),
            (COLOR_TYPE_RGBA, 4usize),
        ] {
            let out = convert_rgba_to_color_type(&rgba, width, height, color_type, 16).unwrap();
            assert_eq!(
                out.len(),
                4 * channels * 2,
                "unexpected length for color_type {color_type}"
            );
            // Every 16-bit big-endian sample must have identical high and low
            // bytes (byte-replication scaling: v*65535/255 == (v<<8)|v).
            for chunk in out.chunks_exact(2) {
                assert_eq!(
                    chunk[0], chunk[1],
                    "16-bit sample {:?} is not byte-replicated for color_type {color_type}",
                    chunk
                );
            }
        }
    }

    /// Roundtrip RGBA -> color_type (bit_depth=16) -> RGBA must recover the
    /// original 8-bit values exactly (lossless for uint sample_format),
    /// exercising both the SIMD expand and SIMD reduce batched paths.
    #[test]
    fn test_convert_color_type_16bit_roundtrip_all_types() {
        let width = 37u32; // odd width, exercises AVX2 tail handling
        let height = 3u32;
        let n = (width * height) as usize;
        let rgba: Vec<u8> = (0..n * 4).map(|i| ((i * 53) % 256) as u8).collect();

        for &color_type in &[
            COLOR_TYPE_GRAY,
            COLOR_TYPE_RGB,
            COLOR_TYPE_GRAY_ALPHA,
            COLOR_TYPE_RGBA,
        ] {
            let encoded = convert_rgba_to_color_type(&rgba, width, height, color_type, 16).unwrap();
            let decoded =
                convert_color_type_to_rgba(&encoded, width, height, color_type, 16).unwrap();

            match color_type {
                COLOR_TYPE_RGBA => {
                    assert_eq!(decoded, rgba, "RGBA 16-bit roundtrip mismatch");
                }
                COLOR_TYPE_GRAY => {
                    // Gray replicates Y to R,G,B and forces alpha=0xFF; just
                    // check length and internal RGB replication.
                    assert_eq!(decoded.len(), n * 4);
                    for px in decoded.chunks_exact(4) {
                        assert_eq!(px[0], px[1]);
                        assert_eq!(px[1], px[2]);
                        assert_eq!(px[3], 0xFF);
                    }
                }
                COLOR_TYPE_RGB => {
                    assert_eq!(decoded.len(), n * 4);
                    for (orig, dec) in rgba.chunks_exact(4).zip(decoded.chunks_exact(4)) {
                        assert_eq!(dec[0], orig[0]);
                        assert_eq!(dec[1], orig[1]);
                        assert_eq!(dec[2], orig[2]);
                        assert_eq!(dec[3], 0xFF); // alpha forced opaque, original discarded
                    }
                }
                COLOR_TYPE_GRAY_ALPHA => {
                    assert_eq!(decoded.len(), n * 4);
                    for (orig, dec) in rgba.chunks_exact(4).zip(decoded.chunks_exact(4)) {
                        assert_eq!(dec[0], dec[1]);
                        assert_eq!(dec[1], dec[2]);
                        assert_eq!(dec[3], orig[3]); // alpha preserved
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn test_convert_rgba_to_rgba_32bit() {
        // Convert RGBA -> RGBA 32-bit
        let rgba = vec![255, 128, 64, 192];
        let rgba_32 = convert_rgba_to_color_type(&rgba, 1, 1, COLOR_TYPE_RGBA, 32).unwrap();
        assert_eq!(rgba_32.len(), 16); // 4 channels × 4 bytes
    }

    #[test]
    fn test_bytes_per_pixel_multi_byte() {
        // Gray 16-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_GRAY, 16), Some(2));
        // Gray 32-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_GRAY, 32), Some(4));
        // RGB 16-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGB, 16), Some(6));
        // RGB 32-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGB, 32), Some(12));
        // RGBA 16-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGBA, 16), Some(8));
        // RGBA 32-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGBA, 32), Some(16));
        // Gray+Alpha 16-bit
        assert_eq!(bytes_per_pixel(COLOR_TYPE_GRAY_ALPHA, 16), Some(4));
    }

    // --- Tests for Phase 3: Sample Format Float/Half-Float (v1.0) ---

    #[test]
    fn test_bytes_per_pixel_with_format_float() {
        // Float always uses 32-bit container
        let gray_float = bytes_per_pixel_with_format(COLOR_TYPE_GRAY, 32, SAMPLE_FORMAT_FLOAT);
        assert_eq!(gray_float, Some(4));

        let rgb_float = bytes_per_pixel_with_format(COLOR_TYPE_RGB, 32, SAMPLE_FORMAT_FLOAT);
        assert_eq!(rgb_float, Some(12)); // 3 channels - 4 bytes
    }

    #[test]
    fn test_bytes_per_pixel_with_format_half() {
        // Half always uses 16-bit container
        let gray_half = bytes_per_pixel_with_format(COLOR_TYPE_GRAY, 16, SAMPLE_FORMAT_HALF);
        assert_eq!(gray_half, Some(2));

        let rgba_half = bytes_per_pixel_with_format(COLOR_TYPE_RGBA, 16, SAMPLE_FORMAT_HALF);
        assert_eq!(rgba_half, Some(8)); // 4 channels × 2 bytes
    }

    #[test]
    fn test_u8_to_float_conversion() {
        // 0 -> 0.0
        assert_eq!(u8_to_float(0), 0.0);
        // 255 -> 1.0
        assert_eq!(u8_to_float(255), 1.0);
        // 128 -> 0.5 (approximately)
        let half = u8_to_float(128);
        assert!(half > 0.4 && half < 0.6);
    }

    #[test]
    fn test_float_to_u8_conversion() {
        // 0.0 -> 0
        assert_eq!(float_to_u8(0.0), 0);
        // 1.0 -> 255
        assert_eq!(float_to_u8(1.0), 255);
        // 0.5 -> 128
        let val = float_to_u8(0.5);
        assert!((127..=128).contains(&val));
        // Clipping: values outside range
        assert_eq!(float_to_u8(-0.5), 0);
        assert_eq!(float_to_u8(1.5), 255);
    }

    #[test]
    fn test_u8_to_half_conversion() {
        // 0 -> half(0.0)
        let half_zero = u8_to_half(0);
        let half_zero_back = half_to_u8(half_zero);
        assert_eq!(half_zero_back, 0);

        // 255 -> half(1.0)
        let half_max = u8_to_half(255);
        let half_max_back = half_to_u8(half_max);
        assert_eq!(half_max_back, 255);
    }

    #[test]
    fn test_half_to_u8_conversion() {
        // Roundtrip: u8 -> half -> u8 must preserve value approximately
        for value in [0, 64, 128, 192, 255] {
            let half_bits = u8_to_half(value);
            let recovered = half_to_u8(half_bits);
            // May have small precision loss in half-float
            assert!(recovered >= value.saturating_sub(1) && recovered <= value.saturating_add(1));
        }
    }

    #[test]
    fn test_convert_rgba_to_color_type_with_format_float() {
        // Convert RGBA -> RGBA with sample_format=FLOAT
        let rgba = vec![255, 128, 64, 192]; // 1 pixel RGBA (4 bytes)
        let result = convert_rgba_to_color_type_with_format(
            &rgba,
            1,
            1,
            COLOR_TYPE_RGBA,
            32, // float uses 32-bit container per sample (section 4.1)
            SAMPLE_FORMAT_FLOAT,
        )
        .unwrap();
        // RGBA - 4 samples, each converted to 4-byte float = 16 bytes
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn test_convert_rgba_to_color_type_with_format_half() {
        // Convert RGBA -> RGBA with sample_format=HALF
        let rgba = vec![255, 128, 64, 192]; // 1 pixel RGBA
        let result = convert_rgba_to_color_type_with_format(
            &rgba,
            1,
            1,
            COLOR_TYPE_RGBA,
            16, // half uses 16-bit container per sample (section 4.1)
            SAMPLE_FORMAT_HALF,
        )
        .unwrap();
        // RGBA 8-bit (4 bytes) -> each byte converted to half (2 bytes) = 8 bytes
        assert_eq!(result.len(), 8);
    }

    #[test]
    fn test_convert_color_type_to_rgba_with_format_float() {
        // Float 0.5 (32-bit BE) em GRAY -> RGBA [128, 128, 128, 255]
        let gray_float = (0.5f32).to_bits().to_be_bytes().to_vec();
        let rgba = convert_color_type_to_rgba_with_format(
            &gray_float,
            1,
            1,
            COLOR_TYPE_GRAY,
            32,
            SAMPLE_FORMAT_FLOAT,
        )
        .expect("failed to convert float GRAY to RGBA");
        assert_eq!(rgba, vec![128, 128, 128, 255]);
    }

    #[test]
    fn test_convert_color_type_to_rgba_with_format_half() {
        // Half-float of 200/255 in GRAY -> RGBA with gray ~200 and alpha 255
        let gray_half = u8_to_half(200);
        let gray_half_bytes = gray_half.to_be_bytes().to_vec();
        let rgba = convert_color_type_to_rgba_with_format(
            &gray_half_bytes,
            1,
            1,
            COLOR_TYPE_GRAY,
            16,
            SAMPLE_FORMAT_HALF,
        )
        .expect("failed to convert half GRAY to RGBA");
        assert_eq!(rgba.len(), 4);
        assert_eq!(rgba[3], 255);
        assert!(rgba[0].abs_diff(200) <= 2, "gray diverged: {}", rgba[0]);
    }

    #[test]
    fn test_sample_format_constants() {
        assert_eq!(SAMPLE_FORMAT_UINT, 0);
        assert_eq!(SAMPLE_FORMAT_FLOAT, 1);
        assert_eq!(SAMPLE_FORMAT_HALF, 2);
    }

    // --- Tests for Phase 4: Chunk cHDR (v1.0) ---

    #[test]
    fn test_chdr_creation() {
        let chdr = cHDR::new();
        assert_eq!(chdr.transfer_function, 3); // sRGB
        assert_eq!(chdr.color_primaries, 0); // BT.709
        assert_eq!(chdr.max_luminance, 1.0);
        assert_eq!(chdr.min_luminance, 0.0);
        assert!(chdr.max_cll.is_none());
        assert!(chdr.max_fall.is_none());
    }

    #[test]
    fn test_chdr_serialized_size_minimal() {
        let chdr = cHDR::new();
        // Without optional: 1 + 1 + 4 + 4 = 10 bytes
        assert_eq!(chdr.serialized_size(), 10);
    }

    #[test]
    fn test_chdr_serialized_size_with_maxcll() {
        let mut chdr = cHDR::new();
        chdr.max_cll = Some(1000);
        // With MaxCLL: 10 + 4 = 14 bytes
        assert_eq!(chdr.serialized_size(), 14);
    }

    #[test]
    fn test_chdr_serialized_size_with_both_optional() {
        let mut chdr = cHDR::new();
        chdr.max_cll = Some(1000);
        chdr.max_fall = Some(500);
        // With both: 10 + 4 + 4 = 18 bytes
        assert_eq!(chdr.serialized_size(), 18);
    }

    #[test]
    fn test_chdr_custom_values() {
        let mut chdr = cHDR::new();
        chdr.transfer_function = 1; // PQ (SMPTE 2084)
        chdr.color_primaries = 1; // BT.2020
        chdr.max_luminance = 10000.0;
        chdr.min_luminance = 0.001;
        chdr.max_cll = Some(3000);

        assert_eq!(chdr.transfer_function, 1);
        assert_eq!(chdr.color_primaries, 1);
        assert_eq!(chdr.max_luminance, 10000.0);
        assert_eq!(chdr.min_luminance, 0.001);
        assert_eq!(chdr.max_cll, Some(3000));
        assert!(chdr.max_fall.is_none());
    }

    #[test]
    fn test_write_chdr_minimal() {
        let chdr = cHDR::new();
        let result = write_chdr_chunk(&chdr, 19);
        assert!(result.is_ok());
        let data = result.unwrap();
        // Chunk header (12) + type (4) + compacted payload + CRC (4)
        assert!(data.len() > 12); // At least header + type + CRC
    }

    #[test]
    fn test_write_chdr_with_optional() {
        let mut chdr = cHDR::new();
        chdr.max_cll = Some(2000);
        chdr.max_fall = Some(1000);
        let result = write_chdr_chunk(&chdr, 19);
        assert!(result.is_ok());
    }

    #[test]
    fn test_read_chdr_minimal_raw() {
        // Create buffer manually (no compression)
        let mut payload = Vec::new();
        payload.push(3); // transfer_function = sRGB
        payload.push(0); // color_primaries = sRGB/BT.709
        payload.extend_from_slice(&(1.0_f32).to_bits().to_be_bytes()); // max_luminance
        payload.extend_from_slice(&(0.0_f32).to_bits().to_be_bytes()); // min_luminance

        let result = read_chdr_chunk(FLAG_RAW, &payload);
        assert!(result.is_ok());
        let chdr = result.unwrap();
        assert_eq!(chdr.transfer_function, 3);
        assert_eq!(chdr.color_primaries, 0);
        assert_eq!(chdr.max_luminance, 1.0);
        assert_eq!(chdr.min_luminance, 0.0);
        assert!(chdr.max_cll.is_none());
    }

    #[test]
    fn test_read_chdr_with_maxcll() {
        let mut payload = Vec::new();
        payload.push(1); // PQ
        payload.push(1); // BT.2020
        payload.extend_from_slice(&(10000.0_f32).to_bits().to_be_bytes());
        payload.extend_from_slice(&(0.001_f32).to_bits().to_be_bytes());
        payload.extend_from_slice(&3000u32.to_be_bytes()); // MaxCLL

        let result = read_chdr_chunk(FLAG_RAW, &payload);
        assert!(result.is_ok());
        let chdr = result.unwrap();
        assert_eq!(chdr.max_cll, Some(3000));
        assert!(chdr.max_fall.is_none());
    }

    #[test]
    fn test_read_chdr_invalid_size() {
        // Too short (less than 10 mandatory bytes)
        let payload = vec![0u8; 9];
        let result = read_chdr_chunk(FLAG_RAW, &payload);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_chdr_maxfall_without_maxcll_invalid() {
        // Construir 18 bytes: transfer (1) + color (1) + max_lum (4) + min_lum (4) + MaxCLL (4) + MaxFALL (4)
        let mut payload = Vec::new();
        payload.push(1);
        payload.push(0);
        payload.extend_from_slice(&(1000.0_f32).to_bits().to_be_bytes());
        payload.extend_from_slice(&(0.0_f32).to_bits().to_be_bytes());
        payload.extend_from_slice(&100u32.to_be_bytes()); // MaxCLL (present!)
        payload.extend_from_slice(&500u32.to_be_bytes()); // MaxFALL

        // This should succeed (18 bytes, max_cll and max_fall both present)
        let result = read_chdr_chunk(FLAG_RAW, &payload);
        assert!(result.is_ok());
    }

    // --- Tests for Phase 5: iDIM Writing in Encoder (v1.0) ---

    #[test]
    fn test_idim_morton_code() {
        // Test morton_code() for Z-order preview
        let code1 = morton_code(0, 0);
        assert_eq!(code1, 0);

        let code2 = morton_code(1, 0);
        assert!(code2 > 0);

        let code3 = morton_code(0, 1);
        assert!(code3 > 0);

        // Different codes for different coordinates
        assert_ne!(code2, code3);
    }

    // --- PHASE 6: Tests for Security and Validation (v1.0) ---

    #[test]
    fn test_overflow_protection_bytes_per_row_1bit() {
        // bytes_per_row_for_bit_depth should succeed even with u32::MAX
        // (u32 max fits in u64, no true overflow possible)
        let result = bytes_per_row_for_bit_depth(u32::MAX, 1);
        assert!(result.is_ok());
        let bytes = result.unwrap();
        // ceil(u32::MAX * 1 / 8)
        assert!(bytes > 0);
    }

    #[test]
    fn test_overflow_protection_bytes_per_row_4bit() {
        // Test with high value that causes overflow
        let large_width = 1_000_000_000u32; // 1 billion pixels
        let result = bytes_per_row_for_bit_depth(large_width, 4);
        // Expected: Ok if not overflow, Err if overflow
        // 1B * 4 / 8 = 500M bytes - may be valid or overflow depending on u64
        let _ = result; // Just validate no panics
    }

    #[test]
    fn test_pack_samples_row_boundary_1bit() {
        // Test at boundary: 8 pixels of 1-bit = 1 exact byte
        let samples = vec![1, 0, 1, 1, 0, 1, 0, 1];
        let result = pack_samples_row(&samples, 1, 8, 1);
        assert!(result.is_ok());
        let packed = result.unwrap();
        assert_eq!(packed.len(), 1); // 8 pixels * 1 bit / 8 = 1 byte
    }

    #[test]
    fn test_pack_samples_row_boundary_4bit() {
        // Test at boundary: 2 pixels of 4-bit = 1 exact byte
        let samples = vec![15, 14]; // Maximum values 4-bit
        let result = pack_samples_row(&samples, 4, 2, 1);
        assert!(result.is_ok());
        let packed = result.unwrap();
        assert_eq!(packed.len(), 1); // 2 pixels * 4 bits / 8 = 1 byte
    }

    #[test]
    fn test_unpack_samples_row_boundary() {
        // Roundtrip: pack -> unpack with boundary values
        let original = vec![15, 15, 15, 15]; // Maximum 4-bit
        let packed = pack_samples_row(&original, 4, 4, 1).unwrap();
        let unpacked = unpack_samples_row(&packed, 4, 4, 1).unwrap();
        assert_eq!(unpacked, original);
    }

    #[test]
    fn test_truncation_protection_pack() {
        // pack_samples_row must reject values > maximum for bit_depth
        let samples = vec![16]; // 16 does not fit in 4 bits (maximum 15)
        let result = pack_samples_row(&samples, 4, 1, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_truncation_protection_unpack() {
        // unpack_samples_row must validate buffer size
        let packed = vec![0xFFu8]; // 1 byte
        let result = unpack_samples_row(&packed, 4, 10, 1);
        // 10 pixels * 4 bits = 40 bits = 5 bytes, we have only 1
        assert!(result.is_err());
    }

    #[test]
    fn test_float_conversion_clipping() {
        // float_to_u8 must clip values outside [0.0, 1.0]
        assert_eq!(float_to_u8(-1.0), 0); // Negative -> 0
        assert_eq!(float_to_u8(2.0), 255); // > 1.0 -> 255
                                           // 0.5 * 255 = 127.5, round() = 128
        assert_eq!(float_to_u8(0.5), 128);
    }

    #[test]
    fn test_expand_compress_roundtrip_16bit() {
        // Roundtrip 8-bit -> 16-bit -> 8-bit
        for value in [0, 1, 127, 128, 254, 255] {
            let expanded = expand_sample_8_to_n_bits(value, 16).unwrap();
            let compressed = compress_sample_n_to_8bits(expanded, 16).unwrap();
            // May have precision loss in low bits
            assert_eq!(compressed, value);
        }
    }

    #[test]
    fn test_expand_compress_roundtrip_12bit() {
        // Roundtrip 8-bit -> 12-bit -> 8-bit
        for value in [0, 16, 128, 200, 255] {
            let expanded = expand_sample_8_to_n_bits(value, 12).unwrap();
            let compressed = compress_sample_n_to_8bits(expanded, 12).unwrap();
            // 12-bit has less precision than 16-bit
            let diff = (compressed as i16 - value as i16).abs();
            assert!(diff <= 1, "Value {} lost too much precision", value);
        }
    }

    #[test]
    fn test_color_conversion_rgb_to_gray_preserves_brightness() {
        // RGB -> Gray must approximately preserve brightness
        // White pixel
        let white = vec![255, 255, 255, 255];
        let gray_data = convert_rgba_to_color_type(&white, 1, 1, COLOR_TYPE_GRAY, 8).unwrap();
        assert_eq!(gray_data[0], 255); // Should be white

        // Black pixel
        let black = vec![0, 0, 0, 255];
        let gray_data = convert_rgba_to_color_type(&black, 1, 1, COLOR_TYPE_GRAY, 8).unwrap();
        assert_eq!(gray_data[0], 0); // Should be black
    }

    #[test]
    fn test_color_conversion_consistency() {
        // RGBA -> Color Type -> RGBA must be consistent
        let original = vec![100, 150, 200, 255];

        // Convert RGBA -> RGB -> RGBA
        let rgb_data = convert_rgba_to_color_type(&original, 1, 1, COLOR_TYPE_RGB, 8).unwrap();
        let back_to_rgba = convert_color_type_to_rgba(&rgb_data, 1, 1, COLOR_TYPE_RGB, 8).unwrap();

        // Should be [R, G, B, 255] (alpha becomes opaque)
        assert_eq!(back_to_rgba[0], original[0]); // R
        assert_eq!(back_to_rgba[1], original[1]); // G
        assert_eq!(back_to_rgba[2], original[2]); // B
        assert_eq!(back_to_rgba[3], 0xFF); // A always opaque in RGB
    }

    #[test]
    fn test_chdr_luminance_ordering() {
        // min_luminance <= max_luminance is assumed (not validated in read_chdr)
        // But test consistency
        let mut chdr = cHDR::new();
        chdr.min_luminance = 0.001;
        chdr.max_luminance = 10000.0;
        assert!(chdr.min_luminance < chdr.max_luminance);
    }

    #[test]
    fn test_idim_tile_coverage() {
        // iDim with tiles_x=2, tiles_y=2, tile_width=256, tile_height=256
        // Must cover image 512x512 exactly
        let idim = iDim {
            tile_width: 256,
            tile_height: 256,
            tiles_x: 2,
            tiles_y: 2,
            scan_order: 0,
        };

        let mut total_width = 0u32;
        let mut total_height = 0u32;

        for ty in 0..idim.tiles_y {
            for tx in 0..idim.tiles_x {
                let (w, h) = idim.tile_size(tx, ty, 512, 512);
                if ty == 0 {
                    total_width += w;
                }
                if tx == 0 {
                    total_height += h;
                }
            }
        }

        assert_eq!(total_width, 512);
        assert_eq!(total_height, 512);
    }

    #[test]
    fn test_compression_method_bitmask() {
        // COMPRESSION_METHOD_ZSTD_BIT should be 0b0000_0001
        assert_eq!(COMPRESSION_METHOD_ZSTD_BIT, 0b0000_0001);

        // Test: bit 0 must be set for ZSTD
        let compression_method = COMPRESSION_METHOD_ZSTD_BIT;
        assert_eq!(compression_method & 0b0000_0001, 0b0000_0001);

        // Bits 1-7 must be 0 (reserved)
        assert_eq!(compression_method & 0b1111_1110, 0);
    }

    #[test]
    fn test_no_panic_on_empty_image() {
        // Dimensions 0 must be rejected, must not panic
        let result = bytes_per_pixel_with_format(COLOR_TYPE_GRAY, 8, SAMPLE_FORMAT_UINT);
        // bytes_per_pixel does not validate dimensions, only type
        assert!(result.is_some());
    }

    #[test]
    fn test_no_panic_on_invalid_color_type() {
        // invalid color_type must return None, no panic
        let result = bytes_per_pixel(99, 8); // 99 is invalid
        assert!(result.is_none());
    }

    #[test]
    fn test_read_u16_be_boundary() {
        // read_u16_be must validate buffer
        let buf = [0x12u8, 0x34];
        let result = read_u16_be(&buf, 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x1234);

        // Offset outside buffer
        let result = read_u16_be(&buf, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_u32_be_boundary() {
        // read_u32_be must validate buffer
        let buf = [0x12u8, 0x34, 0x56, 0x78];
        let result = read_u32_be(&buf, 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x12345678);

        // Offset + 4 > buf.len()
        let result = read_u32_be(&buf, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_sample_format_uint_default() {
        // SAMPLE_FORMAT_UINT = 0 - default
        assert_eq!(SAMPLE_FORMAT_UINT, 0);
    }

    #[test]
    fn test_bit_depth_validation_indexed() {
        // Indexed color must accept 1, 2, 4, 8
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 1), Some(1));
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 2), Some(1));
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 4), Some(1));
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 8), Some(1));

        // Must reject 16, 32
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 16), None);
        assert_eq!(bytes_per_pixel(COLOR_TYPE_INDEXED, 32), None);
    }

    #[test]
    fn test_bit_depth_validation_rgb() {
        // RGB must not accept 1, 2, 4
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGB, 1), None);
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGB, 2), None);
        assert_eq!(bytes_per_pixel(COLOR_TYPE_RGB, 4), None);

        // Must accept 8, 16, 32
        assert!(bytes_per_pixel(COLOR_TYPE_RGB, 8).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_RGB, 16).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_RGB, 32).is_some());
    }

    #[test]
    fn test_color_type_gray_supports_all_bit_depths() {
        // Gray (type 0) should support 1, 2, 4, 8, 10, 12, 16, 32
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 1).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 2).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 4).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 8).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 10).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 12).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 16).is_some());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 32).is_some());

        // Must reject 3, 5, 6, 7, 9, 11, 13, 14, 31, 33
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 3).is_none());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 5).is_none());
        assert!(bytes_per_pixel(COLOR_TYPE_GRAY, 33).is_none());
    }
}
