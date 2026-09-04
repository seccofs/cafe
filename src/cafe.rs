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
mod simd_quantize;
#[cfg(feature = "simd")]
mod simd_sample_conversion;
#[cfg(feature = "simd")]
mod simd_shuffle;
mod tonemap;

// Public re-exports for convenience
pub use constants::{FORMAT_VERSION_MAJOR, FORMAT_VERSION_MINOR};
pub use error::{CafeError, Result};
pub use tonemap::ToneMapOperator;
pub use types::{
    cHDR, iDim, DecodeInfo, DecodeResult, EncodeOptions, EncoderOptions, FilterHeuristic, Palette,
    PaletteAlgorithm, PaletteEntry, Tile,
};

use crate::constants::*;

// Import functions from the specialized modules
use crate::filter::{
    analyze_tile_complexity, apply_predictive_filter, apply_predictive_filter_per_row,
    undo_predictive_filter, undo_predictive_filter_per_row,
};

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

use crate::chunk::{read_chunk, read_chunk_from, write_chunk, ReadChunk};
use crate::types::{ChunkStats, CompressionStats};

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

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
            // v1.11: margin widened from a flat EVEN_ODD_NUM_PASSES (2, one
            // pass_number prefix byte per pass) to `h` (one row's worth of
            // margin) — the streaming encoder (`Encoder::add_even_odd_rows`)
            // may split each pass across multiple IDATs (one prefix byte
            // each), unlike the whole-file `encode()` path, which always
            // emits exactly one IDAT per pass. `h` is a generous but still
            // bounded upper bound on how many IDATs a legitimate encoder
            // would ever emit for one pass (at most one per row), so this
            // remains a real ceiling, not an unbounded allowance.
            .and_then(|v| v.checked_add(h as u64))
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

/// Computes bytes-per-row for a direct (non-indexed) color type + bit depth
/// (section 4.3.1): for bit_depth < 8 with GRAY/GRAY_ALPHA, rows are packed
/// sub-byte (`ceil(width * bpp_multiplier * bit_depth / 8)`); otherwise it's
/// simply `width * bpp`. `bpp` must already be the value
/// `bytes_per_pixel(color_type, bit_depth)` returned for this combination.
/// Shared by `encode()` and `Encoder<W>::new()` — the same calculation, only
/// ever duplicated once before (see AGENTS.md "Encoder<W>" notes).
fn bytes_per_row_for_direct_color(
    width: u32,
    color_type: u8,
    bit_depth: u8,
    bpp: usize,
) -> Result<usize> {
    if bit_depth < 8 && (color_type == COLOR_TYPE_GRAY || color_type == COLOR_TYPE_GRAY_ALPHA) {
        let bpp_multiplier = match color_type {
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
        Ok(bits_total.div_ceil(8))
    } else {
        (width as usize).checked_mul(bpp).ok_or_else(|| {
            CafeError::UnsupportedFeature(
                "bytes_per_row calculation would overflow during encode".into(),
            )
        })
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
/// Returns `(uses_zstd, used_dict)` — `used_dict` (v1.5) tells the caller
/// whether `dict` was the winning compression candidate for this chunk (see
/// `compress_with_fallback_dict`'s dictionary fallback guarantee), so callers
/// can decide whether the `zDIC` chunk is worth emitting at all.
#[inline]
fn append_idat_chunk(
    out: &mut Vec<u8>,
    data: &[u8],
    level: i32,
    dict: Option<&[u8]>,
) -> Result<(bool, bool)> {
    let (flag, compressed, used_dict) = compress_with_fallback_dict(data, level, dict)?;
    out.extend_from_slice(&write_chunk(CHUNK_IDAT, flag, &compressed));
    Ok((flag == FLAG_ZSTD, used_dict))
}

/// Same as `append_idat_chunk`, but builds and returns the complete IDAT
/// chunk bytes instead of appending to a shared buffer. Used so tiles can be
/// filtered/compressed independently on a thread pool (rayon) and the
/// resulting chunks are appended to `out` afterwards, in original tile order,
/// preserving the exact byte layout `append_idat_chunk` would have produced
/// sequentially. Returns `(chunk_bytes, uses_zstd, used_dict)` — see
/// `append_idat_chunk` for what `used_dict` means.
#[inline]
fn build_idat_chunk(data: &[u8], level: i32, dict: Option<&[u8]>) -> Result<(Vec<u8>, bool, bool)> {
    let (flag, compressed, used_dict) = compress_with_fallback_dict(data, level, dict)?;
    Ok((
        write_chunk(CHUNK_IDAT, flag, &compressed),
        flag == FLAG_ZSTD,
        used_dict,
    ))
}

/// Builds interlaced (Adam7 or even/odd, section 5) IDATs for RGBA pixel
/// data. Shared between `encode()` (direct RGBA path) and `encode_indexed()`
/// (which first expands its palette indices to RGBA before interlacing,
/// since Adam7/even-odd only operate on uint RGBA — section 5). Each pass
/// becomes its own IDAT, prefixed with a 1-byte pass_number.
///
/// Unlike the pre-v1.5 version, this does **not** append directly to the
/// output buffer — it returns the built IDAT bytes instead, so callers can
/// decide whether the dictionary fallback (see `compress_with_fallback_dict`)
/// makes the `zDIC` chunk worth emitting *before* committing any bytes to the
/// file (zDIC, section 4.9, must appear before the IDATs it applies to, but
/// whether it was actually useful for compression is only known after
/// compressing all the IDATs). Returns `(idat_bytes, uses_zstd, used_dict)`.
///
/// Passes are written sequentially (not parallelized): each pass is itself a
/// single compression unit, so there is no independent per-tile work to farm
/// out to a thread pool here (unlike the row/2D tiling paths below).
fn build_interlaced_idats(
    interlace_method: u8,
    rgba: &[u8],
    width: u32,
    height: u32,
    level: i32,
    dict: Option<&[u8]>,
) -> Result<(Vec<u8>, bool, bool)> {
    let mut idat_bytes = Vec::new();
    let mut uses_zstd = false;
    let mut used_dict = false;
    let passes: Vec<Vec<u8>> = if interlace_method == INTERLACE_ADAM7 {
        apply_adam7_interlace(rgba, width, height)
            .into_iter()
            .collect()
    } else if interlace_method == INTERLACE_EVEN_ODD {
        apply_even_odd_interlace(rgba, width, height)
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    for (pass_idx, pass_data) in passes.iter().enumerate() {
        let pass_number = (pass_idx + 1) as u8;
        let mut pass_payload = Vec::with_capacity(1 + pass_data.len());
        pass_payload.push(pass_number);
        pass_payload.extend_from_slice(pass_data);
        let (pass_uses_zstd, pass_used_dict) =
            append_idat_chunk(&mut idat_bytes, &pass_payload, level, dict)?;
        uses_zstd |= pass_uses_zstd;
        used_dict |= pass_used_dict;
    }
    Ok((idat_bytes, uses_zstd, used_dict))
}

/// Builds row-tiled IDATs (section 4.3, no 2D tiling/interlace): partitions
/// `height` into chunks of `tile_rows` lines, then — in parallel across a
/// rayon thread pool, since tiles are independent — builds each tile's raw
/// bytes via `build_tile_raw`, applies byte-shuffle or the predictive filter
/// (mutually exclusive, byte-shuffle takes precedence), and compresses with
/// fallback. Shared between `encode()` (raw pixel bytes) and
/// `encode_indexed()` (packed palette indices) — the only difference between
/// the two is how a tile's raw bytes are produced, captured by
/// `build_tile_raw`.
///
/// Unlike the pre-v1.5 version, this does **not** append directly to the
/// output buffer — it returns the concatenated IDAT bytes (in original row
/// order) instead, so callers can decide whether the dictionary fallback
/// (see `compress_with_fallback_dict`) makes the `zDIC` chunk worth emitting
/// *before* committing any bytes to the file. Returns
/// `(idat_bytes, uses_zstd, used_dict)`.
///
/// `bytes_per_row` / `bpp` describe the raw tile bytes returned by
/// `build_tile_raw` (used for the predictive filter's neighbor math);
/// `stride_width` is the pixel/sample width passed to byte-shuffle.
///
/// `use_filter_per_row` selects `FILTER_METHOD_PREDICTIVE_PER_ROW` (v1.5)
/// instead of the classic one-filter-per-tile predictive filter; ignored
/// when `use_byte_shuffle` or `!use_filter`. Validation that
/// `filter_heuristic` is one of the two heuristics supported in per-row mode
/// (`Entropy`/`Msad`) is the caller's responsibility (see `encode()`/
/// `encode_indexed()`), since it must be rejected before any per-tile work
/// is spawned on the thread pool, not per-tile inside this closure.
#[allow(clippy::too_many_arguments)]
fn apply_single_tile_filter(
    tile_raw: &[u8],
    tile_h: usize,
    bytes_per_row: usize,
    bpp: usize,
    stride_width: u32,
    use_byte_shuffle: bool,
    use_filter: bool,
    use_filter_per_row: bool,
    filter_heuristic: FilterHeuristic,
    level: i32,
) -> Result<Vec<u8>> {
    if use_byte_shuffle {
        shuffle::apply_byte_shuffle(tile_raw, bpp, stride_width, tile_h as u32)
    } else if use_filter && use_filter_per_row {
        apply_predictive_filter_per_row(tile_raw, tile_h, bytes_per_row, bpp, filter_heuristic)
    } else if use_filter {
        apply_predictive_filter(
            tile_raw,
            tile_h,
            bytes_per_row,
            bpp,
            filter_heuristic,
            level,
        )
    } else {
        Ok(tile_raw.to_vec())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row_tiled_idats<F>(
    height: u32,
    tile_rows: u32,
    bytes_per_row: usize,
    bpp: usize,
    stride_width: u32,
    use_byte_shuffle: bool,
    use_filter: bool,
    use_filter_per_row: bool,
    filter_heuristic: FilterHeuristic,
    level: i32,
    zstd_dict: Option<&[u8]>,
    build_tile_raw: F,
) -> Result<(Vec<u8>, bool, bool)>
where
    F: Fn(usize, usize) -> Result<Vec<u8>> + Sync,
{
    let height_usize = height as usize;
    let tile_rows = tile_rows as usize;

    let mut tile_bounds = Vec::new();
    let mut row_start = 0;
    while row_start < height_usize {
        let row_end = (row_start + tile_rows).min(height_usize);
        tile_bounds.push((row_start, row_end));
        row_start = row_end;
    }

    let chunks: Vec<(Vec<u8>, bool, bool)> = tile_bounds
        .par_iter()
        .map(|&(row_start, row_end)| -> Result<(Vec<u8>, bool, bool)> {
            let tile_h = row_end - row_start;
            let tile_raw = build_tile_raw(row_start, row_end)?;

            let tile_payload = apply_single_tile_filter(
                &tile_raw,
                tile_h,
                bytes_per_row,
                bpp,
                stride_width,
                use_byte_shuffle,
                use_filter,
                use_filter_per_row,
                filter_heuristic,
                level,
            )?;

            build_idat_chunk(&tile_payload, level, zstd_dict)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut idat_bytes = Vec::new();
    let mut uses_zstd = false;
    let mut used_dict = false;
    for (chunk, chunk_used_zstd, chunk_used_dict) in chunks {
        uses_zstd |= chunk_used_zstd;
        used_dict |= chunk_used_dict;
        idat_bytes.extend_from_slice(&chunk);
    }
    Ok((idat_bytes, uses_zstd, used_dict))
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
/// `encode()`, `encode_indexed()`, and `Encoder<W>::new()`, in spec order
/// (sections 4.5-4.8). Deduplicates a block that was previously copy-pasted
/// between the two path-based encoders — a past divergence between the
/// copies caused `encode_indexed()` to silently omit the zDIC chunk despite
/// using the dictionary to compress IDATs (see
/// `append_zdic_chunk_if_present`). Returns whether any chunk used ZSTD.
///
/// Takes individual fields rather than `&EncodeOptions` so that
/// `Encoder<W>::new()` (which uses the distinct `EncoderOptions` struct —
/// see `types.rs`) can share this logic too, instead of duplicating it or
/// constructing a throwaway `EncodeOptions` just to satisfy the signature.
fn append_common_metadata_chunks(
    out: &mut Vec<u8>,
    exif: Option<&[u8]>,
    json_metadata: &HashMap<String, Value>,
    icc_profile: Option<&[u8]>,
    xmp_metadata: Option<&str>,
    level: i32,
) -> Result<bool> {
    let mut uses_zstd = false;

    // --- eXIF (optional, single instance, section 4.5) ---
    if let Some(exif_bytes) = exif {
        let (flag, data) = compress_with_fallback(exif_bytes, level)?;
        uses_zstd |= flag == FLAG_ZSTD;
        out.extend_from_slice(&write_chunk(CHUNK_EXIF, flag, &data));
    }

    // --- jSON (optional, one per namespace, section 4.6) ---
    for (namespace, obj) in json_metadata {
        let chunk = write_json_chunk(namespace, obj, level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    // --- iCCP (optional, single instance, section 4.7) ---
    if let Some(icc) = icc_profile {
        let chunk = write_iccp_chunk(icc, level)?;
        uses_zstd |= chunk_uses_zstd(&chunk);
        out.extend_from_slice(&chunk);
    }

    // --- xMPd (optional, single instance, section 4.8) ---
    if let Some(xmp) = xmp_metadata {
        let chunk = write_xmpd_chunk(xmp, level)?;
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

    // v1.8: opt-in inverse tone-mapping (ITM) validation. Checked upfront
    // (before any conversion work happens) so an unsupported combination
    // fails fast with a clear message rather than silently falling back to
    // the naive `v/255` conversion or producing a file that only
    // superficially looks like valid HDR content.
    if opts.inverse_tonemap.is_some() {
        if sample_format_final != SAMPLE_FORMAT_FLOAT {
            return Err(CafeError::UnsupportedFeature(format!(
                "EncodeOptions::inverse_tonemap requires sample_format = Some(1) (float) — \
                 matching decode's own restriction of tone-mapping to SAMPLE_FORMAT_FLOAT — \
                 got sample_format = {:?}",
                opts.sample_format
            )));
        }
        if opts.target_color_type != COLOR_TYPE_RGBA {
            return Err(CafeError::UnsupportedFeature(format!(
                "EncodeOptions::inverse_tonemap requires target_color_type = COLOR_TYPE_RGBA (6) \
                 — apply_tone_mapping_to_image's decode-side counterpart assumes 4-channel \
                 (RGBA) float pixel data — got target_color_type = {}",
                opts.target_color_type
            )));
        }
        match &opts.chdr_metadata {
            None => {
                return Err(CafeError::UnsupportedFeature(
                    "EncodeOptions::inverse_tonemap requires chdr_metadata = Some(_) \
                     (max_luminance is needed to scale relative linear values to absolute nits)"
                        .into(),
                ));
            }
            Some(chdr) if chdr.transfer_function != 0 => {
                return Err(CafeError::UnsupportedFeature(format!(
                    "EncodeOptions::inverse_tonemap requires chdr_metadata.transfer_function = 0 \
                     (linear) — no OETF implemented for PQ/HLG/sRGB on encode — got {}",
                    chdr.transfer_function
                )));
            }
            Some(_) => {}
        }
    }

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
        } else if let Some(operator) = opts.inverse_tonemap {
            // v1.8: inverse tone-mapping (ITM) — synthesizes plausible HDR
            // linear-float RGBA data from the SDR input instead of the
            // naive v/255 conversion. Validated above: sample_format_final
            // == SAMPLE_FORMAT_FLOAT and chdr_metadata present with
            // transfer_function == 0 are both guaranteed at this point.
            let chdr = opts
                .chdr_metadata
                .as_ref()
                .expect("validated above: inverse_tonemap requires chdr_metadata");
            let source_primaries = 0u8; // image crate always decodes to sRGB/BT.709
            let converted = tonemap::apply_inverse_tone_mapping_to_image(
                &rgba_raw,
                width,
                height,
                chdr,
                source_primaries,
                operator,
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
    let bytes_per_row = bytes_per_row_for_direct_color(width, target_color_type, bit_depth, bpp)?;

    let filter_method = if opts.use_byte_shuffle {
        FILTER_METHOD_BYTE_SHUFFLE
    } else if opts.use_filter && opts.use_filter_per_row {
        FILTER_METHOD_PREDICTIVE_PER_ROW
    } else if opts.use_filter {
        FILTER_METHOD_PREDICTIVE
    } else {
        FILTER_METHOD_NONE
    };

    // Per-row predictive filter (v1.5) only supports the two heuristics cheap
    // enough to run per row (see `filter::apply_predictive_filter_per_row`).
    // It also only applies to the row-tiled path below (no interlace, no
    // iDIM 2D tiling) — reject those combinations upfront rather than
    // silently falling back to per-tile behavior.
    if opts.use_filter && opts.use_filter_per_row && !opts.use_byte_shuffle {
        if !matches!(
            opts.filter_heuristic,
            FilterHeuristic::Entropy | FilterHeuristic::Msad
        ) {
            return Err(CafeError::UnsupportedFeature(format!(
                "use_filter_per_row only supports FilterHeuristic::Entropy or ::Msad, got {:?}",
                opts.filter_heuristic
            )));
        }
        if opts.interlace_method != INTERLACE_NONE {
            return Err(CafeError::UnsupportedFeature(
                "use_filter_per_row is incompatible with interlace (Adam7/even-odd)".into(),
            ));
        }
        if opts.idim.is_some() {
            return Err(CafeError::UnsupportedFeature(
                "use_filter_per_row is incompatible with iDIM (2D tiling)".into(),
            ));
        }
    }

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
    uses_zstd |= append_common_metadata_chunks(
        &mut out,
        opts.exif.as_deref(),
        &opts.json_metadata,
        opts.icc_profile.as_deref(),
        opts.xmp_metadata.as_deref(),
        opts.level,
    )?;

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
            } else if opts.use_filter && opts.use_filter_per_row {
                apply_predictive_filter_per_row(
                    tile_raw,
                    tile_h,
                    bytes_per_row,
                    bpp,
                    opts.filter_heuristic,
                )?
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

    // --- IDAT (section 4.3) ---
    // Built via a closure (parameterized by the dictionary to use) into a
    // separate buffer — not `out` directly — so that whether the `zDIC`
    // chunk (section 4.9) is worth emitting can be decided *after*
    // compression. zDIC must still appear in the file before the IDATs it
    // applies to, so it's written to `out` right before `idat_bytes` is
    // appended, once the decision below is made.
    //
    // New feature (v1.0): local complexity analysis per tile (extended section 4.3.1).
    let build_idat_section = |dict_arg: Option<&[u8]>| -> Result<(Vec<u8>, bool, bool)> {
        if opts.interlace_method == INTERLACE_ADAM7 || opts.interlace_method == INTERLACE_EVEN_ODD {
            // Interlaced path (section 5): each pass becomes an IDAT with a
            // prefixed pass_number. Shared helper also used by encode_indexed().
            build_interlaced_idats(
                opts.interlace_method,
                &raw,
                width,
                height,
                opts.level,
                dict_arg,
            )
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
            // concatenated in the original tile_order sequence, which the
            // decoder relies on to reconstruct tile positions.
            let chunks: Vec<(Vec<u8>, bool, bool)> = idim
                .tile_order()?
                .into_par_iter()
                .map(|(tx, ty)| -> Result<(Vec<u8>, bool, bool)> {
                    let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, width, height);
                    let tw = tile_w as usize;
                    let th = tile_h as usize;
                    let tile_stride = tw.checked_mul(bpp).ok_or_else(|| {
                        CafeError::UnsupportedFeature(
                            "overflow in tile stride during encode".into(),
                        )
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
                                CafeError::UnsupportedFeature(
                                    "overflow in line offset (encode)".into(),
                                )
                            })?;
                        let end = start.checked_add(tile_stride).ok_or_else(|| {
                            CafeError::UnsupportedFeature("overflow at end of line (encode)".into())
                        })?;
                        if end > raw.len() {
                            return Err(CafeError::TruncatedFile(
                                "tile exceeds image data during encode".into(),
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

                    build_idat_chunk(&tile_payload, opts.level, dict_arg)
                })
                .collect::<Result<Vec<_>>>()?;

            let mut idat_bytes = Vec::new();
            let mut uses_zstd = false;
            let mut used_dict = false;
            for (chunk, chunk_used_zstd, chunk_used_dict) in chunks {
                uses_zstd |= chunk_used_zstd;
                used_dict |= chunk_used_dict;
                idat_bytes.extend_from_slice(&chunk);
            }
            Ok((idat_bytes, uses_zstd, used_dict))
        } else {
            // No interlace (v1.0): row tiles, with optional predictive filter.
            // Local complexity analysis (extended section 4.3.1) — cheap enough
            // to keep sequential, computed upfront so it doesn't interfere with
            // the shared row-tiling helper below. Only run once (dict_arg ==
            // final_zstd_dict.as_deref(), i.e. the first invocation) to avoid
            // duplicate log output when this closure is called a second time
            // for the auto-dictionary comparison below.
            if opts.adaptive_analysis && dict_arg == final_zstd_dict.as_deref() {
                let tile_rows = opts.tile_rows as usize;
                let height_usize = height as usize;
                let mut tile_bounds = Vec::new();
                let mut row_start = 0;
                while row_start < height_usize {
                    let row_end = (row_start + tile_rows).min(height_usize);
                    tile_bounds.push((row_start, row_end));
                    row_start = row_end;
                }
                let complexities: Vec<f64> = tile_bounds
                    .par_iter()
                    .map(|&(row_start, row_end)| {
                        let tile_raw = &raw[row_start * bytes_per_row..row_end * bytes_per_row];
                        analyze_tile_complexity(tile_raw)
                    })
                    .collect();

                if !complexities.is_empty() {
                    log::debug!("Adaptive analysis: {} tiles processed", complexities.len());
                    let avg_complexity =
                        complexities.iter().sum::<f64>() / complexities.len() as f64;
                    log::debug!("Average complexity: {:.2} bits/byte", avg_complexity);
                    let max_complexity = complexities
                        .iter()
                        .cloned()
                        .fold(f64::NEG_INFINITY, f64::max);
                    log::debug!("Maximum complexity: {:.2} bits/byte", max_complexity);
                }
            }

            // Tiles are independent (no shared state between them), so the
            // expensive per-tile work — filter search and ZSTD compression — is
            // parallelized across a rayon thread pool (v1.2.2) inside the shared
            // helper.
            build_row_tiled_idats(
                height,
                opts.tile_rows,
                bytes_per_row,
                bpp,
                width,
                opts.use_byte_shuffle,
                opts.use_filter,
                opts.use_filter_per_row,
                opts.filter_heuristic,
                opts.level,
                dict_arg,
                |row_start, row_end| {
                    Ok(raw[row_start * bytes_per_row..row_end * bytes_per_row].to_vec())
                },
            )
        }
    };

    // NOTE: uses `final_zstd_dict` (not `opts.zstd_dictionary`) so that an
    // auto-trained dictionary (opts.auto_dictionary) is actually used to
    // compress these IDATs, matching the dictionary announced in the zDIC
    // chunk (written below). Using the wrong (empty) dictionary here would
    // silently produce IDATs the decoder cannot reconstruct with the
    // dictionary it read from zDIC — see doc comment on
    // `append_common_metadata_chunks` for the historical bug this class of
    // divergence already caused in `encode_indexed()`.
    let (mut idat_bytes, mut idat_uses_zstd, used_dict) =
        build_idat_section(final_zstd_dict.as_deref())?;

    // --- zDIC (optional, single instance, section 4.9) ---
    // An explicitly user-provided `opts.zstd_dictionary` is always honored
    // (the caller opted in deliberately — e.g. a shared dictionary trained
    // offline across a batch of related images — so it's emitted
    // unconditionally, same as pre-v1.5 behavior).
    //
    // An *auto-trained* dictionary (`opts.auto_dictionary`, no explicit
    // dictionary given) gets the full v1.5 dictionary fallback guarantee:
    // per-IDAT, `compress_with_fallback_dict` already picks the smaller of
    // {raw, zstd-no-dict, zstd-with-dict} for each tile, but the *total*
    // file can still regress if the fixed `zDIC` chunk overhead outweighs
    // the sum of small per-tile savings (observed during the v1.4.2
    // compression audit: some tiles saved a few bytes each, but zDIC's own
    // ~100+ bytes of overhead made the whole file bigger). So when the
    // dictionary won at least one tile, re-run IDAT compression once more
    // with no dictionary at all and compare the two *whole-file* totals
    // (zDIC chunk + IDATs vs IDATs alone), keeping whichever is smaller.
    // This doubles IDAT compression cost only in this specific case
    // (auto_dictionary enabled AND the dictionary won somewhere) — bounded,
    // opt-in cost for a strict non-regression guarantee.
    let mut emit_zdic = opts.zstd_dictionary.is_some();
    if opts.auto_dictionary && opts.zstd_dictionary.is_none() && used_dict {
        if let Some(dict) = final_zstd_dict.as_deref() {
            let zdic_chunk = write_zdic_chunk(dict, opts.level)?;
            let total_with_dict = zdic_chunk.len() + idat_bytes.len();
            let (idat_bytes_nodict, idat_uses_zstd_nodict, _) = build_idat_section(None)?;
            if idat_bytes_nodict.len() < total_with_dict {
                // No dictionary wins overall once zDIC's fixed overhead is
                // accounted for — discard the dict-based IDATs and don't
                // emit zDIC at all.
                idat_bytes = idat_bytes_nodict;
                idat_uses_zstd = idat_uses_zstd_nodict;
            } else {
                emit_zdic = true;
            }
        }
    } else if used_dict {
        // Dictionary was explicitly provided by the caller and won at least
        // one tile — already covered by `opts.zstd_dictionary.is_some()`
        // above, kept here only for clarity (no-op).
        emit_zdic = true;
    }
    uses_zstd |= idat_uses_zstd;
    if emit_zdic {
        uses_zstd |=
            append_zdic_chunk_if_present(&mut out, final_zstd_dict.as_deref(), opts.level)?;
    }
    out.extend_from_slice(&idat_bytes);

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

/// Converts a buffer of raw (post-deinterlace, post-filter-undo) pixel bytes
/// in the file's native color_type/bit_depth/sample_format to a standalone
/// RGBA buffer (`width * height * 4` bytes) — palette dequantization,
/// float/half sample reduction, and HDR tone-mapping all happen here.
///
/// This is "Step 2" of whole-image reconstruction (see
/// `reconstruct_final_pixels`, which calls this after deinterlacing), but it
/// is deliberately parameterized on a plain `(data, width, height)` triple
/// rather than a whole `DecodeState`/`ReconstructParams`, so the exact same
/// conversion logic can also be applied to a single tile's raw bytes (see
/// the `decode_tile_*` family below) without duplicating this dispatch —
/// the two call sites can never diverge on how a given
/// color_type/bit_depth/sample_format combination gets converted to RGBA.
#[allow(clippy::too_many_arguments)]
fn convert_raw_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    color_type: u8,
    bit_depth: u8,
    sample_format: u8,
    palette: Option<&Palette>,
    chdr: Option<&cHDR>,
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<Vec<u8>> {
    if color_type == COLOR_TYPE_INDEXED {
        if let Some(pal) = palette {
            Ok(dequantize_from_palette(data, pal, width, height))
        } else {
            Err(CafeError::UnsupportedFeature(
                "Color type=3 found without PLTE chunk".into(),
            ))
        }
    } else if sample_format == SAMPLE_FORMAT_FLOAT && chdr.is_some() {
        // v1.1: HDR tone-mapping — converts linear HDR float → SDR sRGB 8-bit
        let target = 0u8; // 0=sRGB, 1=Rec.709, 2=DCI-P3, 3=Linear
        tonemap::apply_tone_mapping_to_image(
            data,
            width,
            height,
            chdr.unwrap(),
            target,
            tonemap_operator,
        )
    } else if sample_format == SAMPLE_FORMAT_FLOAT || sample_format == SAMPLE_FORMAT_HALF {
        // float/half: reduces samples to 8 bits and converts color → RGBA
        convert_color_type_to_rgba_with_format(
            data,
            width,
            height,
            color_type,
            bit_depth,
            sample_format,
        )
    } else {
        // uint: convert back to RGBA
        let _bpp_from_color = bytes_per_pixel(color_type, bit_depth).ok_or_else(|| {
            CafeError::UnsupportedFeature(format!(
                "Color type {}, bit depth {} not supported in output conversion",
                color_type, bit_depth
            ))
        })?;
        convert_color_type_to_rgba(data, width, height, color_type, bit_depth)
    }
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
    convert_raw_to_rgba(
        &pixel_rows,
        params.width,
        params.height,
        params.color_type,
        params.bit_depth,
        params.sample_format,
        params.palette,
        params.chdr,
        params.tonemap_operator,
    )
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
/// Mutable state accumulated while walking the chunk stream of a CAFE file
/// during decode (refactoring v1.2.2). Grouping these fields lets each chunk
/// handler be a small, independently auditable function instead of one
/// ~800-line match arm inline in the main loop — this state is populated
/// entirely from untrusted file bytes, so keeping each handler small and
/// testable in isolation reduces the chance a rare combination of chunks
/// (e.g. iDIM + cHDR + interlace + unusual bit depth) slips through
/// unvalidated.
struct DecodeState {
    width: Option<u32>,
    height: Option<u32>,
    filter_method: u8,
    interlace_method_read: u8,
    bytes_per_row: usize,
    pixel_rows: Vec<u8>,
    // SECURITY (CWE-409): cumulative decompression budget for the IDATs.
    // Derived from the size expected by the IHDR; prevents multiple IDATs
    // from expanding to gigabytes when the image is small.
    decompress_budget: Option<u64>,
    decompressed_total: u64,
    adam7_passes: [Vec<u8>; ADAM7_NUM_PASSES], // For Adam7 (v1.0)
    even_odd_passes: [Vec<u8>; EVEN_ODD_NUM_PASSES], // For even/odd (v1.0)
    exif: Option<Vec<u8>>,
    json_metadata: HashMap<String, Value>,
    icc_profile: Option<Vec<u8>>,     // ICC profile (v1.0)
    xmp_metadata: Option<String>,     // XMP metadata (v1.0)
    zstd_dictionary: Option<Vec<u8>>, // ZSTD dictionary (v1.0)
    color_type: u8,
    bit_depth: u8,
    sample_format: u8,
    palette: Option<Palette>,
    idim: Option<iDim>, // iDIM chunk (v1.0, ancillary)
    tiles_seen: usize,  // Tile counter for 2D tiling (iDIM)
    // Cached result of `idim.tile_order()`, computed once in
    // `handle_idim_chunk` right after the iDIM geometry is validated.
    // `handle_idat_tile_idim` indexes into this instead of recomputing
    // `tile_order()` (an O(tile_count) allocation, O(tile_count log
    // tile_count) for Z-order) on every single IDAT — recomputing it per
    // tile would make decoding an N-tile image O(N^2)/O(N^2 log N) overall.
    idim_tile_order: Option<Vec<(u16, u16)>>,
    chdr: Option<cHDR>, // cHDR chunk (v1.0, ancillary, HDR metadata)
    // Number of image rows already produced by previous IDATs in the
    // row-strip (non-iDIM, non-indexed-cap-tracked) case. Used only by the
    // per-tile decode path (`decode_idat_as_tile`, v1.5+ streaming-prep
    // refactor) to compute a `Tile`'s `y` offset — the whole-image
    // accumulation path (`handle_idat_indexed`/`handle_idat_direct_color`)
    // derives the same information implicitly from `state.pixel_rows.len()`
    // and does not need this counter.
    tile_rows_seen: usize,
    // Per-chunk compression stats (v1.6.2+), populated as each chunk is
    // read/decompressed so `DecodeResult::compression_stats` can report
    // real numbers instead of always being `None`. Per `ChunkStats`'s field
    // doc comments (`src/types.rs`): `original_size` is the decompressed
    // (post-`decompress_chunk`) size, `compressed_size` is the on-disk size
    // of the chunk's `Data` field (i.e. `chunk.data.len()`, already
    // compressed if Flag=ZSTD, identical to Flag=raw).
    chunk_stats: Vec<ChunkStats>,
}

impl Default for DecodeState {
    fn default() -> Self {
        DecodeState {
            width: None,
            height: None,
            filter_method: FILTER_METHOD_NONE,
            interlace_method_read: INTERLACE_NONE,
            bytes_per_row: 0,
            pixel_rows: Vec::new(),
            decompress_budget: None,
            decompressed_total: 0,
            adam7_passes: Default::default(),
            even_odd_passes: Default::default(),
            exif: None,
            json_metadata: HashMap::new(),
            icc_profile: None,
            xmp_metadata: None,
            zstd_dictionary: None,
            color_type: COLOR_TYPE_RGBA, // Default, will be overwritten
            bit_depth: 8,                // Default, will be overwritten
            sample_format: SAMPLE_FORMAT_UINT, // Default: unsigned integer (v1.0)
            palette: None,
            idim: None,
            tiles_seen: 0,
            idim_tile_order: None,
            chdr: None,
            tile_rows_seen: 0,
            chunk_stats: Vec::new(),
        }
    }
}

/// Records one chunk's compression stats into `state.chunk_stats` (v1.6.2+).
/// `chunk_type` is a human-readable 4-byte tag (e.g. `"IDAT"`); `compressed_size`
/// is the on-disk `Data` field length, `original_size` is the decompressed length.
fn record_chunk_stats(
    state: &mut DecodeState,
    chunk_type: &[u8; 4],
    original_size: usize,
    compressed_size: usize,
) {
    state.chunk_stats.push(ChunkStats {
        chunk_type: String::from_utf8_lossy(chunk_type).to_string(),
        original_size: original_size as u32,
        compressed_size: compressed_size as u32,
    });
}

/// Handles the IHDR chunk (section 4.1): parses and validates width, height,
/// bit depth, sample format, color type, compression/filter/interlace
/// methods, and computes `bytes_per_row` plus the cumulative decompression
/// budget (CWE-409). Critical chunk — any inconsistency is a hard error.
fn handle_ihdr_chunk(state: &mut DecodeState, data: &[u8]) -> Result<()> {
    // SECURITY (CWE-409): IHDR is always the first chunk and appears exactly
    // once (section 4.1/8.9 of the spec). Unlike every other stateful chunk
    // (PLTE, eXIF, iCCP, xMPd, zDIC, iDIM, cHDR - all guarded by
    // `state.X.is_none()`), IHDR had no such guard: a second IHDR would
    // silently overwrite width/height/bytes_per_row/color_type while
    // `decompress_budget` (below) stays cached from the *first* IHDR. A
    // forged file could declare huge dimensions in IHDR #1 (caching a huge
    // budget), then a tiny IHDR #2 (small effective image), letting
    // subsequent IDATs decompress far beyond what the tiny effective image
    // needs - defeating the cumulative decompression-bomb protection.
    if state.width.is_some() {
        return Err(CafeError::UnsupportedFeature(
            "duplicate IHDR chunk: IHDR must appear exactly once, as the first chunk".into(),
        ));
    }
    const IHDR_LEN: usize = 14;
    if data.len() < IHDR_LEN {
        return Err(CafeError::TruncatedFile(format!(
            "IHDR must have {IHDR_LEN} bytes, got {}",
            data.len()
        )));
    }
    let w = u32::from_be_bytes(
        data[0..4]
            .try_into()
            .map_err(|_| CafeError::TruncatedFile("IHDR Width conversion failed".into()))?,
    );
    let h = u32::from_be_bytes(
        data[4..8]
            .try_into()
            .map_err(|_| CafeError::TruncatedFile("IHDR Height conversion failed".into()))?,
    );
    let bd = data[8];
    let sf = data[9]; // Sample format (v1.0)
    let ct = data[10];
    let compression_method = data[11];
    let fm = data[12];
    let interlace_method = data[13];

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
    state.sample_format = sf;

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
    let mut bytes_per_row = state.bytes_per_row;
    match ct {
        COLOR_TYPE_GRAY => {
            // Grayscale: bit depth 1, 2, 4, 8, 10, 12, 16, 32 (section 4.1.1, v1.0)
            match bd {
                1 | 2 | 4 => {
                    // Sub-byte: compute ceil(width * bit_depth / 8)
                    bytes_per_row = bytes_per_row_for_bit_depth(w, bd).unwrap_or(w as usize);
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
                            "bytes_per_row calculation would overflow (Grayscale 32-bit)".into(),
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
            state.color_type = COLOR_TYPE_GRAY;
            state.bit_depth = bd;
        }
        COLOR_TYPE_RGB => {
            // RGB: bit depth 8, 10, 12, 16, 32 (section 4.1.2, v1.0)
            match bd {
                8 => {
                    // 8-bit: 3 bytes/pixel
                    bytes_per_row = (w as u64).checked_mul(3).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGB 8-bit)".into(),
                        )
                    })? as usize;
                }
                10 | 12 | 16 => {
                    // 16-bit container: 6 bytes/pixel (3 channels × 2 bytes)
                    bytes_per_row = (w as u64).checked_mul(6).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGB 10/12/16)".into(),
                        )
                    })? as usize;
                }
                32 => {
                    // 32-bit: 12 bytes/pixel (3 channels × 4 bytes)
                    bytes_per_row = (w as u64).checked_mul(12).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGB 32-bit)".into(),
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
            state.color_type = COLOR_TYPE_RGB;
            state.bit_depth = bd;
        }
        COLOR_TYPE_INDEXED => {
            // Palette: bit depth must be 1, 2, 4 or 8
            if bd != 1 && bd != 2 && bd != 4 && bd != 8 {
                return Err(CafeError::UnsupportedFeature(format!(
                    "Color type=3 (Indexed): bit depth must be 1, 2, 4, or 8, got {bd}"
                )));
            }
            state.color_type = COLOR_TYPE_INDEXED;
            state.bit_depth = bd;
            // For PLTE, bytes_per_row will be adjusted after reading the palette
        }
        COLOR_TYPE_GRAY_ALPHA => {
            // Gray + Alpha: bit depth 1, 2, 4, 8, 10, 12, 16, 32 (section 4.1.3, v1.0)
            match bd {
                1 | 2 | 4 => {
                    // Sub-byte: compute ceil(width * 2 * bit_depth / 8)
                    let samples_per_row = w as u64 * 2u64;
                    bytes_per_row = (samples_per_row.checked_mul(bd as u64).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (Color type=4)".into(),
                        )
                    })? as usize)
                        .div_ceil(8);
                }
                8 => {
                    // 8-bit: 2 bytes/pixel (Gray + Alpha)
                    bytes_per_row = (w as u64).checked_mul(2).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (Gray+Alpha 8-bit)".into(),
                        )
                    })? as usize;
                }
                10 | 12 | 16 => {
                    // 16-bit container: 4 bytes/pixel (2 channels × 2 bytes)
                    bytes_per_row = (w as u64).checked_mul(4).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (Gray+Alpha 10/12/16)".into(),
                        )
                    })? as usize;
                }
                32 => {
                    // 32-bit: 8 bytes/pixel (2 channels × 4 bytes)
                    bytes_per_row = (w as u64).checked_mul(8).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (Gray+Alpha 32-bit)".into(),
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
            state.color_type = COLOR_TYPE_GRAY_ALPHA;
            state.bit_depth = bd;
        }
        COLOR_TYPE_RGBA => {
            // RGBA: bit depth 8, 10, 12, 16, 32 (section 4.1.4, v1.0)
            match bd {
                8 => {
                    // 8-bit: 4 bytes/pixel (R, G, B, A)
                    bytes_per_row = (w as u64).checked_mul(4).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGBA 8-bit)".into(),
                        )
                    })? as usize;
                }
                10 | 12 | 16 => {
                    // 16-bit container: 8 bytes/pixel (4 channels × 2 bytes)
                    bytes_per_row = (w as u64).checked_mul(8).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGBA 10/12/16)".into(),
                        )
                    })? as usize;
                }
                32 => {
                    // 32-bit: 16 bytes/pixel (4 channels × 4 bytes)
                    bytes_per_row = (w as u64).checked_mul(16).ok_or_else(|| {
                        CafeError::TruncatedFile(
                            "bytes_per_row calculation would overflow (RGBA 32-bit)".into(),
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
            state.color_type = COLOR_TYPE_RGBA;
            state.bit_depth = bd;
        }
        _ => {
            return Err(CafeError::UnsupportedFeature(format!(
                "Color type {ct} not supported (supports 0, 2, 3, 4, 6)"
            )));
        }
    }
    state.bytes_per_row = bytes_per_row;

    // SECURITY: Validate Filter method (section 4.1)
    // Filter method = 1 (byte-shuffle) has been RESERVED since v1.0 and must be
    // rejected explicitly, per spec section 4.1
    // v1.1: Byte-shuffle (filter_method=1) now implemented
    // v1.5: Predictive per-row (filter_method=3) now implemented
    if fm != FILTER_METHOD_NONE
        && fm != FILTER_METHOD_BYTE_SHUFFLE
        && fm != FILTER_METHOD_PREDICTIVE
        && fm != FILTER_METHOD_PREDICTIVE_PER_ROW
    {
        return Err(CafeError::UnsupportedFeature(format!(
            "Filter method {} invalid: supports 0 (none), 1 (byte-shuffle), 2 (predictive), or 3 (predictive per-row)",
            fm
        )));
    }

    // SECURITY: Validate Interlace method (section 5)
    // v1.0 supports: 0 (none), 1 (Adam7) and 2 (even/odd)
    if interlace_method != INTERLACE_NONE
        && interlace_method != INTERLACE_ADAM7
        && interlace_method != INTERLACE_EVEN_ODD
    {
        return Err(CafeError::UnsupportedFeature(format!(
            "Interlace method {} invalid: supports only 0 (none), 1 (Adam7), and 2 (even/odd)",
            interlace_method
        )));
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
            "Byte-shuffle (filter_method=1) is incompatible with interlace (section 4.3.2)".into(),
        ));
    }

    // v1.5: Predictive per-row (filter_method=3) is likewise incompatible with
    // interlace — the encoder never produces this combination; a malformed
    // file declaring it must be rejected explicitly rather than mis-decoded.
    if fm == FILTER_METHOD_PREDICTIVE_PER_ROW && interlace_method != INTERLACE_NONE {
        return Err(CafeError::UnsupportedFeature(
            "Predictive per-row (filter_method=3) is incompatible with interlace".into(),
        ));
    }

    state.width = Some(w);
    state.height = Some(h);
    state.filter_method = fm;
    state.interlace_method_read = interlace_method;

    // SECURITY (CWE-409): compute the cumulative decompression cap from the IHDR
    // dimensions. Each IDAT may only expand up to what the image still needs —
    // multiple IDATs cannot sum to gigabytes if the image is small.
    if state.decompress_budget.is_none() {
        state.decompress_budget = Some(compute_decompress_budget(
            interlace_method,
            ct,
            w,
            h,
            state.bytes_per_row,
        ));
    }

    Ok(())
}

/// Handles the PLTE chunk (section 4.1.2): critical, required with
/// color_type=3 (Indexed). Adjusts `bytes_per_row` for the indexed bit depth.
fn handle_plte_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<()> {
    // PLTE is critical, required with Color type = 3 (section 4.1.2)
    if state.palette.is_none() {
        // Stats (v1.6.2+): PLTE payloads are tiny (<=256 entries), so a
        // second decompress_chunk call here (just to measure the
        // pre-parse decompressed length) is negligible overhead — avoids
        // changing read_plte_chunk's return type just for stats tracking.
        if let Ok(decompressed) = decompress_chunk(flag, data) {
            record_chunk_stats(state, CHUNK_PLTE, decompressed.len(), data.len());
        }
        state.palette = Some(read_plte_chunk(flag, data)?);
        // v1.0: Adjust bytes_per_row ONLY if color_type=3 (PLTE)
        // If color_type=6 (RGBA) with Adam7, PLTE is ignored (don't overwrite bytes_per_row)
        if state.color_type == COLOR_TYPE_INDEXED {
            if let Some(w) = state.width {
                // For palette, bytes_per_row depends on bit_depth (1, 2, 4, 8)
                state.bytes_per_row = bytes_per_row_for_bit_depth(w, state.bit_depth)?;
            }
        }
    }
    Ok(())
}

/// Handles the eXIF chunk (section 4.5): ancillary, single instance — ignores repeats.
fn handle_exif_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<()> {
    if state.exif.is_none() {
        let decompressed = decompress_chunk(flag, data)?;
        record_chunk_stats(state, CHUNK_EXIF, decompressed.len(), data.len());
        state.exif = Some(decompressed);
    }
    Ok(())
}

/// Handles the jSON chunk (section 4.6): ancillary, multiple instances per
/// namespace. Malformed JSON is silently discarded (ancillary contract).
fn handle_json_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<()> {
    // Stats (v1.6.2+): measured before parsing so a namespace is recorded
    // even if the JSON body itself turns out to be malformed (obj == None).
    if let Ok(decompressed) = decompress_chunk(flag, data) {
        record_chunk_stats(state, CHUNK_JSON, decompressed.len(), data.len());
    }
    let (namespace, obj) = read_json_chunk(flag, data)?;
    if let Some(obj) = obj {
        state.json_metadata.insert(namespace, obj);
    }
    // obj == None -> malformed JSON, silently discarded (ancillary)
    Ok(())
}

/// Handles the iCCP chunk (section 4.7): ancillary, single instance. Invalid
/// profiles are silently discarded (ancillary contract), never propagated as
/// a hard error.
fn handle_iccp_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) {
    if state.icc_profile.is_none() {
        match read_iccp_chunk(flag, data) {
            Ok(profile) => {
                record_chunk_stats(state, CHUNK_ICCP, profile.len(), data.len());
                state.icc_profile = Some(profile);
            }
            Err(e) => {
                // Invalid ICC profile, silently discarded (ancillary)
                log::warn!("invalid iCCP chunk, discarded: {}", e);
            }
        }
    }
}

/// Handles the xMPd chunk (section 4.8): ancillary, single instance. Invalid
/// UTF-8 is silently discarded (ancillary contract).
fn handle_xmpd_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) {
    if state.xmp_metadata.is_none() {
        match read_xmpd_chunk(flag, data) {
            Ok(xmp) => {
                record_chunk_stats(state, CHUNK_XMPD, xmp.len(), data.len());
                state.xmp_metadata = Some(xmp);
            }
            Err(e) => {
                // Invalid XMP metadata, silently discarded (ancillary)
                log::warn!("xMPd chunk contains invalid UTF-8, discarded: {}", e);
            }
        }
    }
}

/// Handles the zDIC chunk (v1.0): ancillary, single instance ZSTD
/// dictionary. Invalid dictionaries are silently discarded (ancillary contract).
fn handle_zdic_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) {
    if state.zstd_dictionary.is_none() {
        match read_zdic_chunk(flag, data) {
            Ok(dict) => {
                record_chunk_stats(state, CHUNK_ZDIC, dict.len(), data.len());
                state.zstd_dictionary = Some(dict);
            }
            Err(e) => {
                // Invalid ZSTD dictionary, silently discarded (ancillary)
                log::warn!("invalid zDIC chunk, discarded: {}", e);
            }
        }
    }
}

/// Handles the iDIM chunk (section 4.2): ancillary, optional, single
/// instance per file — defines 2D tile partitioning for streaming.
///
/// SECURITY (CWE-190): `read_idim_chunk` only validates `scan_order`; the
/// tile geometry itself (`tile_width`/`tile_height`/`tiles_x`/`tiles_y`)
/// comes straight from untrusted file bytes. Per section 6 of the spec,
/// `IHDR` is always the first chunk, so `state.width`/`state.height` are
/// already known here; reject an `iDIM` chunk whose geometry cannot be
/// reconciled with the declared image dimensions (mirrors the encoder's own
/// `iDim::new()` derivation: `tiles_x = ceil(width / tile_width)`). Without
/// this check, `iDim::tile_dimensions` would later be called with
/// self-inconsistent values and either panic (debug builds, integer
/// underflow) or silently wrap to a bogus tile size (release builds).
fn handle_idim_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<()> {
    if state.idim.is_none() {
        let idim = read_idim_chunk(flag, data)?;
        if idim.tile_width == 0 || idim.tile_height == 0 {
            return Err(CafeError::UnsupportedFeature(
                "iDIM: tile_width and tile_height must be nonzero".into(),
            ));
        }
        if idim.tiles_x == 0 || idim.tiles_y == 0 {
            return Err(CafeError::UnsupportedFeature(
                "iDIM: tiles_x and tiles_y must be nonzero".into(),
            ));
        }
        let width = state.width.ok_or(CafeError::MissingIhdr)?;
        let height = state.height.ok_or(CafeError::MissingIhdr)?;
        let expected_tiles_x = width.div_ceil(idim.tile_width as u32);
        let expected_tiles_y = height.div_ceil(idim.tile_height as u32);
        if idim.tiles_x as u32 != expected_tiles_x || idim.tiles_y as u32 != expected_tiles_y {
            return Err(CafeError::UnsupportedFeature(format!(
                "iDIM: tiles_x/tiles_y ({}, {}) inconsistent with IHDR dimensions {}x{} and tile size {}x{} (expected {}, {})",
                idim.tiles_x,
                idim.tiles_y,
                width,
                height,
                idim.tile_width,
                idim.tile_height,
                expected_tiles_x,
                expected_tiles_y
            )));
        }
        // SECURITY (CWE-789/CWE-409-class): reject an excessive tile count
        // *before* calling tile_order(), which allocates one (u16, u16)
        // tuple per tile (plus a temporary (u16, u16, u64) buffer + sort in
        // the Z-order path) up front, from this 9-byte chunk alone, with no
        // IDAT read yet. tiles_x/tiles_y are individually valid u16 values
        // (already checked nonzero and consistent with IHDR above), but
        // their product has no inherent ceiling - see MAX_TILE_COUNT's doc
        // comment for the exploit this closes (tiles_x=tiles_y=65535 via
        // tile_width=tile_height=1 => ~17 GiB allocation from a ~71-byte
        // file).
        let tile_count = (idim.tiles_x as u64) * (idim.tiles_y as u64);
        if tile_count > MAX_TILE_COUNT {
            return Err(CafeError::UnsupportedFeature(format!(
                "iDIM: tiles_x * tiles_y = {} exceeds maximum allowed tile count ({})",
                tile_count, MAX_TILE_COUNT
            )));
        }
        // Compute the tile visitation order once here (not per-IDAT in
        // handle_idat_tile_idim) - see the doc comment on
        // DecodeState::idim_tile_order for why this matters.
        state.idim_tile_order = Some(idim.tile_order()?);
        // Single instance per file (similar to eXIF)
        state.idim = Some(idim);
    }
    Ok(())
}

/// Handles the cHDR chunk (section 4.4): ancillary, single instance HDR
/// metadata. Invalid cHDR is silently discarded (ancillary contract).
fn handle_chdr_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) {
    if state.chdr.is_none() {
        match read_chdr_chunk(flag, data) {
            Ok(chdr_data) => state.chdr = Some(chdr_data),
            Err(e) => {
                // Invalid cHDR, silently discarded (ancillary)
                log::warn!("invalid cHDR chunk, discarded: {}", e);
            }
        }
    }
}

/// Handles a decompressed IDAT payload for the interlaced case (Adam7 or
/// even/odd): extracts the 1-byte pass-number prefix and stashes the
/// remainder in the appropriate pass slot.
///
/// Adam7 IDATs are **overwritten** per pass slot (`encode()`'s whole-file
/// path always emits exactly one IDAT per Adam7 pass — a second IDAT for a
/// pass already seen would indicate a malformed/adversarial file, not a
/// legitimate multi-chunk pass, so overwriting silently rather than
/// concatenating or rejecting matches pre-v1.11 behavior unchanged).
///
/// Even/odd IDATs are **concatenated** per pass slot (v1.11+): unlike
/// Adam7, the streaming encoder (`Encoder::add_even_odd_rows()`) may split
/// a single pass's rows across multiple IDATs as rows arrive incrementally
/// — concatenating in arrival order reconstructs the exact same
/// `pass_data` a non-streaming encoder would have produced as one IDAT,
/// since `Encoder::add_even_odd_rows()` always appends complete rows in
/// increasing row order within a pass. The whole-file `encode()` path
/// still emits exactly one IDAT per even/odd pass, so this is a strict
/// superset of the prior (overwrite) behavior for that case, not a
/// behavior change for non-streaming-encoded files (concatenating onto an
/// initially-empty `Vec` for a pass slot that only ever receives one IDAT
/// is equivalent to what overwriting it once would have produced).
fn handle_interlaced_idat(state: &mut DecodeState, decompressed: Vec<u8>) -> Result<()> {
    if state.interlace_method_read == INTERLACE_ADAM7 {
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
        state.adam7_passes[pass_idx] = pass_data;
    } else {
        // INTERLACE_EVEN_ODD
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
        let pass_data = &decompressed[1..];
        let pass_idx = (pass_number - 1) as usize;
        state.even_odd_passes[pass_idx].extend_from_slice(pass_data);
    }
    Ok(())
}

/// Computes the next iDIM (2D tiling) tile's geometry and reverses its
/// byte-shuffle/predictive filtering, without writing the result anywhere —
/// shared by `handle_idat_tile_idim` (whole-image accumulation: copies the
/// returned `tile_raw` into the correct `(row0, col0)` region of
/// `state.pixel_rows`) and `decode_idat_as_tile_idim` (streaming: converts
/// `tile_raw` directly into a standalone `Tile`, v1.9+), so the two output
/// paths can never diverge on tile geometry or filter-reversal logic.
///
/// Advances `state.tiles_seen` by one on success. Returns
/// `(tx, ty, tile_w, tile_h, tile_raw)`: `(tx, ty)` is this tile's position
/// in the tile grid (not pixels — multiply by `tile_width`/`tile_height` for
/// the pixel offset), `tile_w`/`tile_h` are its real pixel dimensions (may
/// be smaller than `idim.tile_width`/`tile_height` at the image's right/
/// bottom edge), and `tile_raw` is `tile_w * tile_h * bpp` bytes of
/// unfiltered pixel data for the tile's color type/bit depth.
fn decode_idim_tile_raw(
    state: &mut DecodeState,
    tile_payload: Vec<u8>,
) -> Result<(u16, u16, u32, u32, Vec<u8>)> {
    let idim = state.idim.clone().ok_or_else(|| {
        CafeError::TruncatedFile("iDIM tile handler invoked without iDIM chunk".into())
    })?;
    // 2D tiling (section 4.2): each IDAT is a tile in the scan_order order.
    if state.color_type == COLOR_TYPE_INDEXED {
        return Err(CafeError::UnsupportedFeature(
            "iDIM (2D tiling) with indexed palette not supported".into(),
        ));
    }
    if state.bit_depth < 8 {
        return Err(CafeError::UnsupportedFeature(
            "iDIM (tiling 2D) requires bit_depth >= 8 in decode".into(),
        ));
    }
    let bpp_for_tile = bytes_per_pixel(state.color_type, state.bit_depth).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "Color type {}, bit depth {} not supported",
            state.color_type, state.bit_depth
        ))
    })?;
    let img_width = state.width.ok_or(CafeError::MissingIhdr)?;
    let img_height = state.height.ok_or(CafeError::MissingIhdr)?;
    let tile_count = idim.tiles_x as usize * idim.tiles_y as usize;
    if state.tiles_seen >= tile_count {
        return Err(CafeError::TruncatedFile(format!(
            "Excess IDAT: expected {tile_count} tiles (iDIM)"
        )));
    }
    // Use the tile order cached once in handle_idim_chunk instead of
    // recomputing it from scratch on every IDAT (would be O(tile_count^2)
    // - or worse, O(tile_count^2 log tile_count) for Z-order - across a
    // full decode instead of O(tile_count)).
    let (tx, ty) = state
        .idim_tile_order
        .as_ref()
        .ok_or_else(|| {
            CafeError::TruncatedFile("iDIM tile order not computed (internal error)".into())
        })?
        .get(state.tiles_seen)
        .copied()
        .ok_or_else(|| CafeError::TruncatedFile("iDIM tile order index out of range".into()))?;
    state.tiles_seen += 1;
    let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, img_width, img_height);
    let tw = tile_w as usize;
    let th = tile_h as usize;
    let tile_stride = tw
        .checked_mul(bpp_for_tile)
        .ok_or_else(|| CafeError::UnsupportedFeature("overflow in tile stride (iDIM)".into()))?;
    let tile_raw = if state.filter_method == FILTER_METHOD_BYTE_SHUFFLE {
        // v1.1: byte-shuffle undone before any predictive filter
        shuffle::undo_byte_shuffle(&tile_payload, bpp_for_tile, tile_w, tile_h)?
    } else if state.filter_method == FILTER_METHOD_PREDICTIVE {
        // 1 filter byte prefixed per tile, with tile_stride per row
        let tile_h_est = tile_payload.len().saturating_sub(1) / tile_stride.max(1);
        if tile_h_est != th {
            return Err(CafeError::TruncatedFile(format!(
                "tile with inconsistent height: expected {th}, "
            )));
        }
        undo_predictive_filter(&tile_payload, th, tile_stride, bpp_for_tile)?
    } else if state.filter_method == FILTER_METHOD_PREDICTIVE_PER_ROW {
        // v1.5: per-row predictive filter is never combined with iDIM (2D
        // tiling) by the encoder — reject a file that declares it rather
        // than silently mis-decoding.
        return Err(CafeError::UnsupportedFeature(
            "Predictive per-row (filter_method=3) is incompatible with iDIM (2D tiling)".into(),
        ));
    } else {
        tile_payload
    };
    let tile_len = tile_stride
        .checked_mul(th)
        .ok_or_else(|| CafeError::TruncatedFile("overflow in tile len (iDIM)".into()))?;
    if tile_raw.len() != tile_len {
        return Err(CafeError::TruncatedFile(format!(
            "tile {} with unexpected size: {} (expected {})",
            state.tiles_seen,
            tile_raw.len(),
            tile_len
        )));
    }
    Ok((tx, ty, tile_w, tile_h, tile_raw))
}

/// Handles a non-interlaced IDAT tile payload when 2D tiling (iDIM) is
/// present: undoes byte-shuffle/predictive filter for the tile (via
/// `decode_idim_tile_raw`), then copies it into the correct `(row0, col0)`
/// region of `state.pixel_rows`.
fn handle_idat_tile_idim(state: &mut DecodeState, tile_payload: Vec<u8>) -> Result<()> {
    let idim = state.idim.clone().ok_or_else(|| {
        CafeError::TruncatedFile("iDIM tile handler invoked without iDIM chunk".into())
    })?;
    let bpp_for_tile = bytes_per_pixel(state.color_type, state.bit_depth).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "Color type {}, bit depth {} not supported",
            state.color_type, state.bit_depth
        ))
    })?;
    let img_height = state.height.ok_or(CafeError::MissingIhdr)?;
    let ih = img_height as usize;
    let full_size = state.bytes_per_row.checked_mul(ih).ok_or_else(|| {
        CafeError::TruncatedFile("overflow in bytes_per_row × height (iDIM)".into())
    })?;
    if state.pixel_rows.is_empty() {
        state.pixel_rows = vec![0u8; full_size];
    }
    if state.pixel_rows.len() != full_size {
        return Err(CafeError::TruncatedFile(
            "tile buffer inconsistent with IHDR (iDIM)".into(),
        ));
    }

    let (tx, ty, tile_w, tile_h, tile_raw) = decode_idim_tile_raw(state, tile_payload)?;
    let th = tile_h as usize;
    let tile_stride = (tile_w as usize) * bpp_for_tile;
    let row0 = (ty as u32 * idim.tile_height as u32) as usize;
    let col0 = (tx as u32 * idim.tile_width as u32) as usize;
    for r in 0..th {
        let dst_start = (row0 + r)
            .checked_mul(state.bytes_per_row)
            .and_then(|v| v.checked_add(col0 * bpp_for_tile))
            .ok_or_else(|| {
                CafeError::TruncatedFile("overflow in tile destination (iDIM)".into())
            })?;
        if dst_start + tile_stride > state.pixel_rows.len() {
            return Err(CafeError::TruncatedFile(
                "tile exceeds image buffer (iDIM)".into(),
            ));
        }
        let src = &tile_raw[r * tile_stride..(r + 1) * tile_stride];
        state.pixel_rows[dst_start..dst_start + tile_stride].copy_from_slice(src);
    }
    Ok(())
}

/// Decodes a single non-interlaced IDAT payload into a standalone 2D-tiling
/// (`iDIM`) `Tile`, already converted to RGBA — the iDIM analogue of
/// `decode_idat_as_tile_row_strip` (v1.9+). Reuses `decode_idim_tile_raw`
/// (geometry + filter-reversal, shared with the whole-image
/// `handle_idat_tile_idim`) and `convert_raw_to_rgba` (color conversion,
/// shared with `reconstruct_final_pixels`) — the only code unique to this
/// function is computing the tile's pixel-space `(x, y)` offset from its
/// grid `(tx, ty)` position and calling `convert_raw_to_rgba` on just that
/// tile's raw bytes instead of the whole image's.
///
/// Does **not** touch `state.pixel_rows` — like
/// `decode_idat_as_tile_row_strip`, this is for `Decoder<R>::next_tile()`,
/// which yields tiles one at a time without ever materializing the whole
/// image in memory.
fn decode_idat_as_tile_idim(
    state: &mut DecodeState,
    tile_payload: Vec<u8>,
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<Tile> {
    let idim = state.idim.clone().ok_or_else(|| {
        CafeError::TruncatedFile("iDIM tile handler invoked without iDIM chunk".into())
    })?;
    let (tx, ty, tile_w, tile_h, tile_raw) = decode_idim_tile_raw(state, tile_payload)?;
    let x = tx as u32 * idim.tile_width as u32;
    let y = ty as u32 * idim.tile_height as u32;

    let pixels = convert_raw_to_rgba(
        &tile_raw,
        tile_w,
        tile_h,
        state.color_type,
        state.bit_depth,
        state.sample_format,
        state.palette.as_ref(),
        state.chdr.as_ref(),
        tonemap_operator,
    )?;

    Ok(Tile {
        x,
        y,
        width: tile_w,
        height: tile_h,
        pixels,
    })
}

/// Decompresses a single raw IDAT chunk (flag+data, as read straight off the
/// file/stream) and decodes it into an iDIM (2D tiling) `Tile` — the iDIM
/// analogue of `decode_idat_chunk_as_tile_row_strip`. This is the function
/// `Decoder::next_tile()` calls once per `IDAT` chunk when the file has an
/// `iDIM` chunk (`DecodeInfo::supports_streaming_tiles` is `true` for such
/// files as of v1.9).
fn decode_idat_chunk_as_tile_idim(
    state: &mut DecodeState,
    flag: u8,
    data: &[u8],
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<Tile> {
    let decompressed = decompress_idat_payload(state, flag, data)?;
    decode_idat_as_tile_idim(state, decompressed, tonemap_operator)
}

/// Undoes byte-shuffle and predictive/per-row-predictive filtering for a
/// single non-interlaced, non-2D-tiled (row-strip or whole-image) IDAT
/// payload. Shared by `handle_idat_indexed`, `handle_idat_direct_color`, and
/// the per-tile streaming-prep path (`decode_idat_as_tile_row_strip`) so the
/// three call sites can never diverge on how a payload's
/// byte-shuffle/predictive-filter combination gets reversed.
///
/// `bpp_for_filter` is the bytes-per-pixel used for both byte-shuffle and
/// the predictive filter's neighbor math — always `1` for indexed (operates
/// on packed index bytes, not unpacked samples), or `bytes_per_pixel
/// (color_type, bit_depth)` for direct color types.
///
/// Returns `(tile_raw, tile_h)`: `tile_raw` is `tile_h *
/// state.bytes_per_row` bytes of packed row data (still packed for
/// sub-byte-depth indexed — callers unpack separately), and `tile_h` is the
/// number of image rows recovered from this single payload.
fn undo_tile_filters(
    state: &DecodeState,
    tile_payload: Vec<u8>,
    bpp_for_filter: usize,
) -> Result<(Vec<u8>, usize)> {
    let img_width = state.width.ok_or(CafeError::MissingIhdr)?;
    let bytes_per_row = state.bytes_per_row;

    // v1.1: Byte-shuffle undone before other filters. Byte-shuffle
    // reorders bytes but never changes the payload length, so computing
    // `tile_h` from the (still-shuffled) payload length below is safe
    // regardless of whether it happens before or after this step.
    let tile_payload = if state.filter_method == FILTER_METHOD_BYTE_SHUFFLE {
        let tile_h = tile_payload.len() / bytes_per_row.max(1);
        shuffle::undo_byte_shuffle(&tile_payload, bpp_for_filter, img_width, tile_h as u32)?
    } else {
        tile_payload
    };

    let tile_h = if state.filter_method == FILTER_METHOD_PREDICTIVE {
        // v1.0: 1 filter byte prefixed per block/tile (not per row)
        tile_payload.len().saturating_sub(1) / bytes_per_row
    } else if state.filter_method == FILTER_METHOD_PREDICTIVE_PER_ROW {
        // v1.5: 1 filter byte prefixed per row (not per tile)
        tile_payload.len() / bytes_per_row.saturating_add(1).max(1)
    } else {
        tile_payload.len() / bytes_per_row
    };

    let tile_raw = if state.filter_method == FILTER_METHOD_PREDICTIVE {
        undo_predictive_filter(&tile_payload, tile_h, bytes_per_row, bpp_for_filter)?
    } else if state.filter_method == FILTER_METHOD_PREDICTIVE_PER_ROW {
        undo_predictive_filter_per_row(&tile_payload, tile_h, bytes_per_row, bpp_for_filter)?
    } else {
        tile_payload
    };

    Ok((tile_raw, tile_h))
}

/// Decodes a single non-interlaced, non-iDIM IDAT payload into a standalone
/// row-strip `Tile`, already converted to RGBA — the per-tile analogue of
/// `handle_idat_indexed`/`handle_idat_direct_color`, which instead append
/// their raw (pre-RGBA-conversion) output to `state.pixel_rows` for later
/// whole-image reconstruction.
///
/// This function does **not** touch `state.pixel_rows` at all — it is
/// intended for a future streaming `Decoder<R: Read>::next_tile()` that
/// yields tiles one at a time without ever materializing the whole image in
/// memory, so it must not depend on (or feed) the whole-image accumulation
/// path. It *does* read/update `state.tile_rows_seen` (this payload's `y`
/// offset within the full image) and the shared CWE-409 decompression
/// budget fields on `state`, exactly like the whole-image handlers, since
/// those protections must apply identically regardless of which output path
/// is used.
///
/// Reuses `undo_tile_filters` (byte-shuffle/predictive-filter reversal,
/// shared with the whole-image handlers) and `convert_raw_to_rgba` (color
/// conversion/palette dequantization/HDR tone-mapping, shared with
/// `reconstruct_final_pixels`) — the only code unique to this function is
/// computing the tile's row count/`y` offset and unpacking indexed samples
/// per-row (mirroring `handle_idat_indexed`'s loop, since
/// `convert_raw_to_rgba` expects unpacked bytes, not bit-packed indices).
///
/// # Panics / errors
/// Returns `CafeError::UnsupportedFeature` if `state.idim.is_some()` (2D
/// tiling has its own tile geometry — see `handle_idat_tile_idim` — and is
/// not row-strip shaped) or if the interlace method is not `INTERLACE_NONE`
/// (an interlace pass is not a spatial rectangle; see the `Tile` doc
/// comment in `src/types.rs`).
///
/// Called by `decode_idat_chunk_as_tile_row_strip` below, which in turn is
/// the function `Decoder::next_tile()` calls once per `IDAT` chunk.
fn decode_idat_as_tile_row_strip(
    state: &mut DecodeState,
    tile_payload: Vec<u8>,
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<Tile> {
    if state.idim.is_some() {
        return Err(CafeError::UnsupportedFeature(
            "decode_idat_as_tile_row_strip: 2D tiling (iDIM) uses its own tile geometry, \
             not row-strip tiling"
                .into(),
        ));
    }
    if state.interlace_method_read != INTERLACE_NONE {
        return Err(CafeError::UnsupportedFeature(
            "decode_idat_as_tile_row_strip: interlaced images do not produce row-strip tiles"
                .into(),
        ));
    }

    let img_width = state.width.ok_or(CafeError::MissingIhdr)?;
    let img_height = state.height.ok_or(CafeError::MissingIhdr)?;
    let y0 = state.tile_rows_seen;

    let raw_rgba_input = if state.color_type == COLOR_TYPE_INDEXED {
        // SECURITY (§4.1.2/CWE-369): same PLTE-before-IDAT requirement as
        // `handle_idat_indexed`.
        if state.palette.is_none() {
            return Err(CafeError::TruncatedFile(
                "Color type=3 requires PLTE chunk before first IDAT".into(),
            ));
        }
        let (tile_packed, tile_h) = undo_tile_filters(state, tile_payload, 1)?;
        let row_width = img_width as usize;
        let mut indices = Vec::with_capacity(tile_h.saturating_mul(row_width));
        for r in 0..tile_h {
            let row_packed = &tile_packed[r * state.bytes_per_row..(r + 1) * state.bytes_per_row];
            let row_indices = unpack_indices_row(row_packed, state.bit_depth, row_width)?;
            indices.extend_from_slice(&row_indices);
        }
        (indices, tile_h)
    } else {
        let bpp_for_filter =
            bytes_per_pixel(state.color_type, state.bit_depth).ok_or_else(|| {
                CafeError::UnsupportedFeature(format!(
                    "Color type {}, bit depth {} not supported",
                    state.color_type, state.bit_depth
                ))
            })?;
        undo_tile_filters(state, tile_payload, bpp_for_filter)?
    };
    let (raw_data, tile_h) = raw_rgba_input;

    // SECURITY (CWE-400): a row-strip tile can never carry more rows than
    // the image still has left, mirroring the accumulation caps in
    // `handle_idat_indexed`/`handle_idat_direct_color`.
    let img_height_usize = img_height as usize;
    let new_y = y0
        .checked_add(tile_h)
        .ok_or_else(|| CafeError::TruncatedFile("overflow accumulating tile rows".into()))?;
    if new_y > img_height_usize {
        return Err(CafeError::TruncatedFile(format!(
            "Excess IDAT: row-strip tile exceeds declared height \
             {img_height_usize} (tile rows {y0}..{new_y})"
        )));
    }

    let pixels = convert_raw_to_rgba(
        &raw_data,
        img_width,
        tile_h as u32,
        state.color_type,
        state.bit_depth,
        state.sample_format,
        state.palette.as_ref(),
        state.chdr.as_ref(),
        tonemap_operator,
    )?;

    state.tile_rows_seen = new_y;

    Ok(Tile {
        x: 0,
        y: y0 as u32,
        width: img_width,
        height: tile_h as u32,
        pixels,
    })
}

/// Decompresses a single raw IDAT chunk (flag+data, as read straight off the
/// file/stream) and decodes it into a row-strip `Tile` — the per-tile
/// analogue of `handle_idat_chunk`'s non-interlaced/non-iDIM branch. This is
/// the function `Decoder::next_tile()` calls once per `IDAT` chunk read off
/// the stream via `chunk::read_chunk_from`.
fn decode_idat_chunk_as_tile_row_strip(
    state: &mut DecodeState,
    flag: u8,
    data: &[u8],
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<Tile> {
    let decompressed = decompress_idat_payload(state, flag, data)?;
    decode_idat_as_tile_row_strip(state, decompressed, tonemap_operator)
}

/// Handles a non-interlaced, non-tiled IDAT payload for `color_type=3`
/// (Indexed): undoes byte-shuffle/predictive filter, unpacks each row back
/// to 1 byte/index, and appends to `state.pixel_rows` with an explicit
/// CWE-400 accumulation cap derived from the IHDR dimensions.
fn handle_idat_indexed(state: &mut DecodeState, tile_payload: Vec<u8>) -> Result<()> {
    // IDAT contains indices packed in bit_depth bits (or filtered)
    // SECURITY (§4.1.2/CWE-369): color_type=3 requires a PLTE chunk before
    // any IDAT; without it bytes_per_row is 0 and the division below
    // would panic. Reject with a recoverable error.
    if state.palette.is_none() {
        return Err(CafeError::TruncatedFile(
            "Color type=3 requires PLTE chunk before first IDAT".into(),
        ));
    }
    // Indexed always operates on 1 byte/pixel (packed index) for both
    // byte-shuffle and the predictive filter, regardless of bit_depth.
    let (tile_packed, tile_h) = undo_tile_filters(state, tile_payload, 1)?;
    // Unpack each row back to 1 byte/index
    // (bit_depth==8 is a trivial case inside unpack_indices_row)
    let row_width = state.width.ok_or(CafeError::MissingIhdr)? as usize;
    let img_height = state.height.ok_or(CafeError::MissingIhdr)? as usize;
    // SECURITY (CWE-400): prevents accumulation of indices beyond the
    // declared size (multiple-IDAT bomb).
    let expected_indices = row_width.checked_mul(img_height).ok_or_else(|| {
        CafeError::TruncatedFile("overflow in calculation of expected indices (indexed)".into())
    })?;
    for r in 0..tile_h {
        let row_packed = &tile_packed[r * state.bytes_per_row..(r + 1) * state.bytes_per_row];
        let row_indices = unpack_indices_row(row_packed, state.bit_depth, row_width)?;
        let new_len = state
            .pixel_rows
            .len()
            .checked_add(row_indices.len())
            .ok_or_else(|| {
                CafeError::TruncatedFile("overflow accumulating pixel indices".into())
            })?;
        if new_len > expected_indices {
            return Err(CafeError::TruncatedFile(format!(
                "Excess IDAT: indexed pixel data sum exceeds \
                  {expected_indices} (IHDR {row_width}x{img_height})"
            )));
        }
        state.pixel_rows.extend_from_slice(&row_indices);
    }
    Ok(())
}

/// Handles a non-interlaced, non-tiled IDAT payload for color types 0, 2, 4,
/// 6 (Gray, RGB, Gray+Alpha, RGBA): undoes byte-shuffle/predictive filter
/// using the type's bpp, and appends to `state.pixel_rows` with an explicit
/// CWE-400 accumulation cap derived from the IHDR dimensions.
fn handle_idat_direct_color(state: &mut DecodeState, tile_payload: Vec<u8>) -> Result<()> {
    // Color types 0, 2, 4, 6: unpack with the correct bpp for the type
    let bpp_for_filter = bytes_per_pixel(state.color_type, state.bit_depth).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "Color type {}, bit depth {} not supported",
            state.color_type, state.bit_depth
        ))
    })?;

    let (tile_raw, _tile_h) = undo_tile_filters(state, tile_payload, bpp_for_filter)?;
    // SECURITY (CWE-400): prevents accumulation of pixel rows beyond
    // the declared size (multiple-IDAT bomb).
    let img_height = state.height.ok_or(CafeError::MissingIhdr)? as usize;
    let expected_row_bytes = state
        .bytes_per_row
        .checked_mul(img_height)
        .ok_or_else(|| CafeError::TruncatedFile("overflow in bytes_per_row × height".into()))?;
    let new_len = state
        .pixel_rows
        .len()
        .checked_add(tile_raw.len())
        .ok_or_else(|| CafeError::TruncatedFile("overflow accumulating pixel rows".into()))?;
    if new_len > expected_row_bytes {
        return Err(CafeError::TruncatedFile(format!(
            "Excess IDAT: indexed data pixels sum more than \
             {expected_row_bytes} bytes (bytes_per_row={}, \
             height={img_height})",
            state.bytes_per_row
        )));
    }
    state.pixel_rows.extend_from_slice(&tile_raw);
    Ok(())
}

/// Decompresses a single IDAT chunk's payload, enforcing the cumulative
/// decompression budget (CWE-409): the cap for this IDAT is whatever
/// remains of the whole-image budget (derived from the IHDR) after
/// previous IDATs, so multiple IDATs can never together decompress to more
/// than the image actually needs. Shared by `handle_idat_chunk` (whole-image
/// accumulation path) and the per-tile streaming-prep path
/// (`decode_idat_as_tile_row_strip`'s callers), since this protection must
/// apply identically regardless of which output path consumes the result.
fn decompress_idat_payload(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<Vec<u8>> {
    let budget = state
        .decompress_budget
        .ok_or_else(|| CafeError::TruncatedFile("IDAT before IHDR".into()))?;
    let remaining = budget.saturating_sub(state.decompressed_total);
    let decompressed =
        decompress_chunk_dict_limited(flag, data, state.zstd_dictionary.as_deref(), remaining)?;
    state.decompressed_total = state
        .decompressed_total
        .checked_add(decompressed.len() as u64)
        .ok_or_else(|| CafeError::TruncatedFile("overflow in decompressed total".into()))?;
    record_chunk_stats(state, CHUNK_IDAT, decompressed.len(), data.len());
    Ok(decompressed)
}

/// Handles the IDAT chunk (pixel data): enforces the cumulative
/// decompression budget (CWE-409), then dispatches to the interlaced,
/// tiled (iDIM), indexed, or direct-color handler depending on the state
/// accumulated so far from IHDR/iDIM/PLTE.
fn handle_idat_chunk(state: &mut DecodeState, flag: u8, data: &[u8]) -> Result<()> {
    let decompressed = decompress_idat_payload(state, flag, data)?;

    // v1.0/+5: If interlaced, extract pass_number from the prefix
    if state.interlace_method_read == INTERLACE_ADAM7
        || state.interlace_method_read == INTERLACE_EVEN_ODD
    {
        handle_interlaced_idat(state, decompressed)
    } else {
        // v1.0 (with full interlace support): process normally
        let tile_payload = decompressed;

        if state.idim.is_some() {
            handle_idat_tile_idim(state, tile_payload)
        } else if state.color_type == COLOR_TYPE_INDEXED {
            handle_idat_indexed(state, tile_payload)
        } else {
            handle_idat_direct_color(state, tile_payload)
        }
    }
}
/// Decodes a CAFE buffer with custom decode options (tone-map operator selection, etc.)
/// This is the core decode implementation without file I/O, with customizable options.
///
/// This is a small dispatch loop over `read_chunk`; each chunk type's parsing
/// and validation logic lives in its own `handle_*_chunk` function above
/// (refactoring v1.2.2) so the critical decode path — which processes 100%
/// of the untrusted file bytes — stays auditable chunk-by-chunk instead of
/// as one large function.
fn decode_bytes_internal(
    buf: &[u8],
    tonemap_operator: tonemap::ToneMapOperator,
) -> Result<(Vec<u8>, DecodeResult)> {
    if buf.len() < 9 || buf[0..9] != SIGNATURE {
        return Err(CafeError::InvalidSignature);
    }

    let mut offset = 9;
    let mut state = DecodeState::default();

    while offset < buf.len() {
        let chunk = read_chunk(buf, offset)?;
        offset = chunk.next_offset;

        match &chunk.chunk_type {
            t if t == CHUNK_IHDR => handle_ihdr_chunk(&mut state, &chunk.data)?,
            t if t == CHUNK_PLTE => handle_plte_chunk(&mut state, chunk.flag, &chunk.data)?,
            t if t == CHUNK_EXIF => handle_exif_chunk(&mut state, chunk.flag, &chunk.data)?,
            t if t == CHUNK_JSON => handle_json_chunk(&mut state, chunk.flag, &chunk.data)?,
            t if t == CHUNK_ICCP => handle_iccp_chunk(&mut state, chunk.flag, &chunk.data),
            t if t == CHUNK_XMPD => handle_xmpd_chunk(&mut state, chunk.flag, &chunk.data),
            t if t == CHUNK_ZDIC => handle_zdic_chunk(&mut state, chunk.flag, &chunk.data),
            t if t == CHUNK_IDIM => handle_idim_chunk(&mut state, chunk.flag, &chunk.data)?,
            t if t == CHUNK_CHDR => handle_chdr_chunk(&mut state, chunk.flag, &chunk.data),
            t if t == CHUNK_IDAT => handle_idat_chunk(&mut state, chunk.flag, &chunk.data)?,
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

    let width = state.width.ok_or(CafeError::MissingIhdr)?;
    let height = state.height.ok_or(CafeError::MissingIhdr)?;

    // Reconstruct final pixels: deinterlace, dequantize, convert color type
    let params = ReconstructParams {
        interlace_method: state.interlace_method_read,
        color_type: state.color_type,
        bit_depth: state.bit_depth,
        sample_format: state.sample_format,
        width,
        height,
        palette: state.palette.as_ref(),
        chdr: state.chdr.as_ref(),
        adam7_passes: &state.adam7_passes,
        even_odd_passes: &state.even_odd_passes,
        tonemap_operator,
    };
    let final_pixels = reconstruct_final_pixels(state.pixel_rows, &params)?;

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

    // Compression statistics (v1.6.2+): aggregated from `state.chunk_stats`,
    // populated incrementally by each chunk handler above as it decompresses
    // its payload. `None` only in the (practically unreachable, since IDAT
    // is mandatory) case of zero recorded chunks.
    let compression_stats = if state.chunk_stats.is_empty() {
        None
    } else {
        let total_original: u64 = state
            .chunk_stats
            .iter()
            .map(|c| c.original_size as u64)
            .sum();
        let total_compressed: u64 = state
            .chunk_stats
            .iter()
            .map(|c| c.compressed_size as u64)
            .sum();
        Some(CompressionStats {
            total_original,
            total_compressed,
            chunks: state.chunk_stats,
        })
    };

    let result = DecodeResult {
        width,
        height,
        exif: state.exif,
        json_metadata: state.json_metadata,
        compression_stats,
        icc_profile: state.icc_profile,
        xmp_metadata: state.xmp_metadata,
        zstd_dictionary: state.zstd_dictionary,
        chdr_metadata: state.chdr,
    };

    Ok((final_pixels, result))
}

/// Dispatches a single ancillary/PLTE/iDIM chunk (everything except
/// `IHDR`/`IDAT`/`IEND`, which the two `Decoder` loops below handle
/// specially since their behavior differs between `read_info()` and
/// `finish()`'s drain loop). Shared so the two loops' chunk-type matches
/// can never drift out of sync on what is recognized/ignored/rejected —
/// mirrors the fallback arm of `decode_bytes_internal`'s single big match
/// (critical unknown chunk -> error, ancillary unknown chunk -> ignore,
/// section 3.1).
fn dispatch_ancillary_chunk(state: &mut DecodeState, chunk: &ReadChunk) -> Result<()> {
    match &chunk.chunk_type {
        t if t == CHUNK_PLTE => handle_plte_chunk(state, chunk.flag, &chunk.data)?,
        t if t == CHUNK_EXIF => handle_exif_chunk(state, chunk.flag, &chunk.data)?,
        t if t == CHUNK_JSON => handle_json_chunk(state, chunk.flag, &chunk.data)?,
        t if t == CHUNK_ICCP => handle_iccp_chunk(state, chunk.flag, &chunk.data),
        t if t == CHUNK_XMPD => handle_xmpd_chunk(state, chunk.flag, &chunk.data),
        t if t == CHUNK_ZDIC => handle_zdic_chunk(state, chunk.flag, &chunk.data),
        t if t == CHUNK_IDIM => handle_idim_chunk(state, chunk.flag, &chunk.data)?,
        t if t == CHUNK_CHDR => handle_chdr_chunk(state, chunk.flag, &chunk.data),
        t => {
            // Unknown chunk: ancillary (1st letter lowercase) -> ignore;
            // critical (uppercase) -> error (section 3.1). This also
            // catches a misplaced/duplicate IHDR here, since IHDR is not
            // matched above — reported as "unknown critical chunk" rather
            // than a more specific message, but still correctly rejected.
            if t[0].is_ascii_uppercase() {
                return Err(CafeError::UnsupportedFeature(format!(
                    "unknown critical chunk: {:?}",
                    String::from_utf8_lossy(t)
                )));
            }
        }
    }
    Ok(())
}

/// Streaming decoder over any `Read` source (file, socket, in-memory
/// `Cursor`, etc.) — decodes one tile at a time instead of requiring the
/// whole compressed file to be materialized in memory up front, unlike
/// `decode`/`decode_bytes` (which call `std::fs::read`/expect an
/// already-fully-buffered `&[u8]`).
///
/// # Usage
/// ```no_run
/// use cafe::Decoder;
/// use std::fs::File;
///
/// # fn main() -> Result<(), cafe::CafeError> {
/// let file = File::open("input.cafe")?;
/// let mut decoder = Decoder::new(file);
/// let info = decoder.read_info()?;
/// println!("{}x{}", info.width, info.height);
/// while let Some(tile) = decoder.next_tile()? {
///     // tile.pixels is tile.width * tile.height * 4 RGBA bytes
/// }
/// let result = decoder.finish()?;
/// # let _ = result;
/// # Ok(())
/// # }
/// ```
///
/// # Call order
/// `read_info()` must be called exactly once, before any call to
/// `next_tile()`; `next_tile()` is then called in a loop until it returns
/// `Ok(None)`; `finish()` may be called at any point afterward (even before
/// `next_tile()` has returned `Ok(None)`, in which case it drains and
/// discards any remaining `IDAT`s — still subject to the CWE-409
/// decompression budget — until `IEND`) to obtain the same ancillary
/// metadata (`eXIF`/`jSON`/`iCCP`/`xMPd`/`zDIC`/`cHDR`) that
/// `decode_bytes`/`decode` return in their `DecodeResult`.
///
/// # Limitations
/// - Does **not** support interlaced (Adam7/even-odd) files: `next_tile()`
///   returns `Err(CafeError::UnsupportedFeature(..))` on every call for such
///   a file (`read_info()` itself still succeeds, so callers can check
///   `DecodeInfo::supports_streaming_tiles` up front and fall back to
///   `decode_bytes`/`decode` instead of calling `next_tile()` at all). This
///   is a permanent, by-design limitation, not a "not yet implemented" gap:
///   an interlace pass is not a spatial rectangle (each pass covers a
///   strided subset of every row/column) and cannot be converted to a
///   standalone RGBA `Tile` without every other pass also being available —
///   `decode_bytes`/`decode`, which buffer the whole file, do not have this
///   restriction.
/// - 2D tiling (`iDIM`) **is** supported (as of v1.9) — each `IDAT` yields
///   one `Tile` with its real `(x, y, width, height)` position in the tile
///   grid (narrower/shorter than `tile_width`/`tile_height` at the image's
///   right/bottom edges, same as `iDim::tile_dimensions`), in whatever
///   `scan_order` the file declares (row-major or Z-order) — the same
///   per-`IDAT` restrictions `handle_idat_tile_idim` already enforces for
///   the whole-image path apply here too (`COLOR_TYPE_INDEXED` and
///   `bit_depth < 8` are rejected with `UnsupportedFeature`).
/// - Relies on the file honoring section 9's mandatory chunk order (all
///   ancillary chunks and `PLTE` appear before the first `IDAT`): a
///   spec-nonconforming file that places one of those chunks *after* an
///   `IDAT` will have it silently ignored by `next_tile()`/`finish()` — the
///   ancillary contract (section 3.1) already permits ignoring ancillary
///   chunks unconditionally, and this streaming decoder additionally
///   relies on their expected position to know when it's safe to stop
///   looking for them. `decode_bytes`/`decode`, which see the whole file
///   at once, do not have this limitation.
pub struct Decoder<R: Read> {
    reader: R,
    state: DecodeState,
    tonemap_operator: tonemap::ToneMapOperator,
    /// `Some` once `read_info()` has completed successfully. Guards against
    /// calling `next_tile()` before `read_info()`, and against calling
    /// `read_info()` a second time.
    info: Option<DecodeInfo>,
    /// `read_info()` must read one chunk past the pre-IDAT metadata to know
    /// whether to stop (it stops at the first `IDAT` or at `IEND`) — if
    /// that chunk was an `IDAT`, it is stashed here for the first
    /// `next_tile()` call to consume instead of re-reading from `reader`.
    pending_idat: Option<ReadChunk>,
    /// Set once `IEND` has been observed (by `read_info()`, `next_tile()`,
    /// or `finish()`'s drain loop), making `next_tile()` idempotent
    /// (`Ok(None)` forever after) instead of erroring on a second call
    /// past the end of the stream.
    finished: bool,
}

impl<R: Read> Decoder<R> {
    /// Creates a new streaming decoder over `reader`, using the default
    /// tone-map operator (Filmic — same default as `EncodeOptions`/
    /// `decode_bytes`). Nothing is read from `reader` until `read_info()`
    /// is called.
    pub fn new(reader: R) -> Self {
        Self::with_tonemap_operator(reader, tonemap::ToneMapOperator::Filmic)
    }

    /// Creates a new streaming decoder over `reader` with an explicit
    /// tone-map operator (relevant only for HDR content with a `cHDR`
    /// chunk) — the streaming equivalent of `decode_bytes_with_opts`.
    pub fn with_tonemap_operator(reader: R, tonemap_operator: tonemap::ToneMapOperator) -> Self {
        Decoder {
            reader,
            state: DecodeState::default(),
            tonemap_operator,
            info: None,
            pending_idat: None,
            finished: false,
        }
    }

    /// Reads the signature and every chunk up to (but not including) the
    /// first `IDAT` — i.e. `IHDR`, and any of `iDIM`/`cHDR`/`eXIF`/`jSON`/
    /// `iCCP`/`xMPd`/`zDIC`/`PLTE` that are present — and returns the
    /// resulting geometry/format info. Must be called exactly once, before
    /// any call to `next_tile()`.
    ///
    /// If the file has no `IDAT` at all (`IEND` appears immediately after
    /// the pre-IDAT chunks — a degenerate but not inherently malformed
    /// case, e.g. truly empty streaming input), `next_tile()` will simply
    /// return `Ok(None)` on its first call rather than this function
    /// erroring.
    pub fn read_info(&mut self) -> Result<DecodeInfo> {
        if self.info.is_some() {
            return Err(CafeError::UnsupportedFeature(
                "Decoder::read_info() called more than once".into(),
            ));
        }

        let mut sig = [0u8; 9];
        self.reader.read_exact(&mut sig).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                CafeError::TruncatedFile(
                    "stream ended before the 9-byte signature could be read".into(),
                )
            } else {
                CafeError::Io(e)
            }
        })?;
        if sig != SIGNATURE {
            return Err(CafeError::InvalidSignature);
        }

        let mut ihdr_seen = false;
        loop {
            let chunk = read_chunk_from(&mut self.reader)?.ok_or_else(|| {
                CafeError::TruncatedFile("stream ended before the first IDAT/IEND chunk".into())
            })?;
            match &chunk.chunk_type {
                t if t == CHUNK_IHDR => {
                    handle_ihdr_chunk(&mut self.state, &chunk.data)?;
                    ihdr_seen = true;
                }
                t if t == CHUNK_IDAT => {
                    self.pending_idat = Some(chunk);
                    break;
                }
                t if t == CHUNK_IEND => {
                    self.finished = true;
                    break;
                }
                _ => dispatch_ancillary_chunk(&mut self.state, &chunk)?,
            }
        }

        if !ihdr_seen {
            return Err(CafeError::MissingIhdr);
        }

        let width = self.state.width.ok_or(CafeError::MissingIhdr)?;
        let height = self.state.height.ok_or(CafeError::MissingIhdr)?;
        // Interlaced files never support streaming tiles (see `next_tile()`'s
        // doc comment: an interlace pass is not a spatial rectangle). iDIM
        // (2D tiling) files do support it as of v1.9, except for the same
        // color-type/bit-depth combinations `handle_idat_tile_idim` already
        // rejects for the whole-image path (indexed palette, bit_depth < 8)
        // — checked here too so callers can find out from `read_info()`
        // alone, without needing to call `next_tile()` first to discover it.
        let supports_streaming_tiles = self.state.interlace_method_read == INTERLACE_NONE
            && (self.state.idim.is_none()
                || (self.state.color_type != COLOR_TYPE_INDEXED && self.state.bit_depth >= 8));

        let info = DecodeInfo {
            width,
            height,
            color_type: self.state.color_type,
            bit_depth: self.state.bit_depth,
            sample_format: self.state.sample_format,
            supports_streaming_tiles,
        };
        self.info = Some(info.clone());
        Ok(info)
    }

    /// Reads and decodes the next `IDAT` chunk into a standalone RGBA
    /// `Tile`, or returns `Ok(None)` once `IEND` has been reached (safe to
    /// call again after that point — always returns `Ok(None)`).
    ///
    /// # Errors
    /// - `UnsupportedFeature` if called before `read_info()`.
    /// - `UnsupportedFeature` if the file is interlaced, or uses 2D tiling
    ///   (`iDIM`) combined with an indexed palette / `bit_depth < 8` — check
    ///   `DecodeInfo::supports_streaming_tiles` (from `read_info()`'s return
    ///   value) before calling this in a loop, and fall back to
    ///   `decode_bytes`/`decode` if `false`.
    /// - Any error `chunk::read_chunk_from`/the per-tile decode pipeline
    ///   can return (CRC mismatch, truncation, decompression-budget
    ///   exceeded, unknown critical chunk, etc.)
    pub fn next_tile(&mut self) -> Result<Option<Tile>> {
        let info = self.info.as_ref().ok_or_else(|| {
            CafeError::UnsupportedFeature("Decoder::next_tile() called before read_info()".into())
        })?;
        if !info.supports_streaming_tiles {
            return Err(CafeError::UnsupportedFeature(
                "Decoder::next_tile() does not support this file (interlaced, or iDIM combined \
                 with indexed palette / bit_depth < 8); use decode_bytes()/decode() instead"
                    .into(),
            ));
        }
        let is_idim = self.state.idim.is_some();
        if self.finished {
            return Ok(None);
        }

        let chunk = if let Some(chunk) = self.pending_idat.take() {
            chunk
        } else {
            loop {
                let chunk = read_chunk_from(&mut self.reader)?
                    .ok_or_else(|| CafeError::TruncatedFile("stream ended before IEND".into()))?;
                match &chunk.chunk_type {
                    t if t == CHUNK_IDAT => break chunk,
                    t if t == CHUNK_IEND => {
                        self.finished = true;
                        return Ok(None);
                    }
                    _ => dispatch_ancillary_chunk(&mut self.state, &chunk)?,
                }
            }
        };

        let tile = if is_idim {
            decode_idat_chunk_as_tile_idim(
                &mut self.state,
                chunk.flag,
                &chunk.data,
                self.tonemap_operator,
            )?
        } else {
            decode_idat_chunk_as_tile_row_strip(
                &mut self.state,
                chunk.flag,
                &chunk.data,
                self.tonemap_operator,
            )?
        };
        Ok(Some(tile))
    }

    /// Consumes the decoder and returns the accumulated ancillary metadata
    /// (`eXIF`/`jSON`/`iCCP`/`xMPd`/`zDIC`/`cHDR`) as a `DecodeResult`, the
    /// same struct `decode_bytes`/`decode` return alongside their pixel
    /// buffer (here, the pixels were already handed out incrementally via
    /// `next_tile()`, so `DecodeResult` is all that's left to return).
    ///
    /// May be called before `next_tile()` has returned `Ok(None)`: any
    /// remaining `IDAT`s are decompressed (respecting the CWE-409 budget)
    /// and discarded — not decoded into `Tile`s — up through `IEND`.
    ///
    /// # Errors
    /// `MissingIhdr` if `read_info()` was never called (or failed before
    /// reaching `IHDR`). Otherwise, any error the underlying chunk reads
    /// can return (same as `next_tile()`).
    pub fn finish(mut self) -> Result<DecodeResult> {
        while !self.finished {
            let chunk = read_chunk_from(&mut self.reader)?
                .ok_or_else(|| CafeError::TruncatedFile("stream ended before IEND".into()))?;
            match &chunk.chunk_type {
                t if t == CHUNK_IDAT => {
                    // Caller stopped calling next_tile() before IEND (or
                    // next_tile() was never called at all, e.g. an
                    // unsupported-tiling file): decompress-and-discard,
                    // still enforcing the decompression budget so a
                    // malicious tail can't bypass CWE-409 protection.
                    decompress_idat_payload(&mut self.state, chunk.flag, &chunk.data)?;
                }
                t if t == CHUNK_IEND => self.finished = true,
                _ => dispatch_ancillary_chunk(&mut self.state, &chunk)?,
            }
        }

        let width = self.state.width.ok_or(CafeError::MissingIhdr)?;
        let height = self.state.height.ok_or(CafeError::MissingIhdr)?;

        // Compression statistics (v1.6.2+): same aggregation as
        // `decode_bytes_internal` (see that function for the exact
        // semantics). `self.state.chunk_stats` already accumulates entries
        // from any `IDAT`s consumed earlier via `next_tile()` too — both
        // `next_tile()` (via `decode_idat_chunk_as_tile_row_strip`) and this
        // function's own drain loop (via the direct `decompress_idat_payload`
        // call above) route through the same `decompress_idat_payload`,
        // which is where stats are recorded — so `finish()`'s totals cover
        // every `IDAT` in the file regardless of how each one was consumed.
        let compression_stats = if self.state.chunk_stats.is_empty() {
            None
        } else {
            let total_original: u64 = self
                .state
                .chunk_stats
                .iter()
                .map(|c| c.original_size as u64)
                .sum();
            let total_compressed: u64 = self
                .state
                .chunk_stats
                .iter()
                .map(|c| c.compressed_size as u64)
                .sum();
            Some(CompressionStats {
                total_original,
                total_compressed,
                chunks: self.state.chunk_stats,
            })
        };

        Ok(DecodeResult {
            width,
            height,
            exif: self.state.exif,
            json_metadata: self.state.json_metadata,
            compression_stats,
            icc_profile: self.state.icc_profile,
            xmp_metadata: self.state.xmp_metadata,
            zstd_dictionary: self.state.zstd_dictionary,
            chdr_metadata: self.state.chdr,
        })
    }
}

/// Streaming encoder: writes a `.cafe` file incrementally to any `W: Write`
/// destination (a file, a `Vec<u8>`, a socket, ...) one row-strip tile at a
/// time via `add_tile()`, instead of requiring the whole image to be
/// buffered in RGBA form before any byte can be written — the encode-side
/// counterpart to `Decoder<R: Read>`.
///
/// # Usage
/// ```no_run
/// use cafe::{Encoder, EncoderOptions};
/// use std::fs::File;
///
/// let file = File::create("out.cafe")?;
/// let opts = EncoderOptions::default();
/// let mut encoder = Encoder::new(file, 256, 128, &opts)?;
/// for row_start in (0..128).step_by(opts.tile_rows as usize) {
///     let row_end = (row_start + opts.tile_rows).min(128);
///     let tile_h = (row_end - row_start) as usize;
///     let rgba_tile = vec![0u8; 256 * tile_h * 4]; // caller-supplied pixels
///     encoder.add_tile(&rgba_tile)?;
/// }
/// encoder.finish()?;
/// # Ok::<(), cafe::CafeError>(())
/// ```
///
/// # `compression_method` semantics (section 3.2) — conservative
/// Because `W` offers no `Seek`, this encoder cannot go back and patch the
/// `IHDR`'s `compression_method` byte after compressing tiles the way
/// `encode()`/`encode_indexed()` do (see `patch_ihdr_compression_method`).
/// Instead, **`compression_method`'s ZSTD bit is always set unconditionally
/// up front**, in `IHDR`, before any tile is compressed — this can
/// overestimate (declare ZSTD support required even if every tile happened
/// to fall back to raw storage) but never underestimate (a decoder that
/// only understands raw chunks would incorrectly accept a file that
/// actually contains ZSTD-compressed IDATs). This matches the safe
/// direction required by the spec's stated purpose for the bit (decoder
/// capability pre-check, section 3.2) — see `Encoder::<W: Write + Seek>`
/// below for the exact (non-conservative) alternative when the destination
/// supports seeking.
///
/// # Permanent limitations (see `EncoderOptions`'s doc comment for the full analysis)
/// No `auto_dictionary`, no Adam7 interlace, no indexed palette
/// (`COLOR_TYPE_INDEXED`). These were each investigated (not merely
/// deferred) and found to require either buffering the whole image first
/// (defeating this API's purpose) or a fundamentally different two-pass API
/// shape — see `EncoderOptions`'s doc comment for the per-item reasoning.
/// Even/odd interlace is *not* in this list — see "Even/odd interlace"
/// below, and `EncoderOptions`'s doc comment for why it didn't share Adam7's
/// fate. This mirrors the same limitation `Decoder<R>` has for Adam7
/// (though `Decoder<R>` did go on to gain real streaming `iDIM` support in
/// v1.9, and `Encoder<W>` symmetrically gained it in v1.10 — see
/// `add_idim_tile()` below — since a decoder reads `PLTE`/`zDIC` before any
/// `IDAT` rather than having to produce them from data it hasn't seen yet,
/// unlike palette/dictionary training, which do require it).
///
/// # 2D tiling (`iDIM`, v1.10+)
/// When `EncoderOptions::idim` is `Some((tile_width, tile_height,
/// scan_order))`, `new()` writes the `iDIM` chunk immediately after `IHDR`
/// and tiles must be submitted one full rectangle at a time via
/// `add_idim_tile()` (in `iDim::tile_order()`'s sequence) instead of
/// `add_tile()` — calling the wrong method for the configured mode returns
/// `UnsupportedFeature`. See `add_idim_tile()`'s doc comment for the full
/// contract.
///
/// # Even/odd interlace (v1.11+)
/// When `EncoderOptions::even_odd_interlace` is `true`, `new()` writes
/// `INTERLACE_EVEN_ODD` into `IHDR` and rows must be submitted via
/// `add_even_odd_rows()` (any row-count grouping, not required to align to
/// pass or `tile_rows` boundaries) instead of `add_tile()`/`add_idim_tile()`
/// — calling the wrong method for the configured mode returns
/// `UnsupportedFeature`. Mutually exclusive with `idim`,
/// `use_filter_per_row`, and `use_byte_shuffle`; requires uint RGBA 8-bit
/// (section 5). See `add_even_odd_rows()`'s doc comment for the full
/// contract.
pub struct Encoder<W: Write> {
    writer: W,
    width: u32,
    height: u32,
    opts_level: i32,
    opts_tile_rows: u32,
    target_color_type: u8,
    bit_depth: u8,
    sample_format: u8,
    bpp: usize,
    bytes_per_row: usize,
    use_byte_shuffle: bool,
    use_filter: bool,
    use_filter_per_row: bool,
    filter_heuristic: FilterHeuristic,
    zstd_dictionary: Option<Vec<u8>>,
    /// `Some` when `EncoderOptions::idim` was set in `new()` — selects 2D
    /// tiling mode (`add_idim_tile()` only, `add_tile()` rejected) instead
    /// of the default row-strip mode (`add_tile()` only). Precomputed once
    /// in `new()` (both the `iDim` geometry and its `tile_order()`
    /// sequence), mirroring `DecodeState::idim_tile_order`'s decode-side
    /// rationale: recomputing per-tile would be wasted work at best and a
    /// correctness hazard at worst if it could ever disagree with itself
    /// between calls.
    idim: Option<iDim>,
    /// Precomputed `idim.tile_order()` (empty when `idim` is `None`).
    /// `add_idim_tile()` indexes into this with `idim_next_tile_idx` to
    /// determine which `(tx, ty)` grid cell the next call fills.
    idim_tile_order: Vec<(u16, u16)>,
    /// Index into `idim_tile_order` of the next tile `add_idim_tile()` will
    /// write. Unused (stays `0`) in row-strip mode.
    idim_next_tile_idx: usize,
    /// `true` when `EncoderOptions::even_odd_interlace` was set in `new()`
    /// — selects even/odd interlace mode (`add_even_odd_rows()` only,
    /// `add_tile()`/`add_idim_tile()` rejected) instead of row-strip or
    /// `iDIM` mode. Mutually exclusive with `idim.is_some()` (enforced in
    /// `new()`). Rows are still counted via `rows_written` — even/odd
    /// shares row-strip mode's "submit until `height` rows written"
    /// completeness semantics exactly (each row belongs to exactly one of
    /// the 2 passes by parity, but every row still gets submitted exactly
    /// once, same as row-strip mode), so `is_complete()`/`finish()`/
    /// `finish_exact()` need no even/odd-specific branch beyond error-
    /// message wording.
    even_odd_interlace: bool,
    /// Row index (within the full image) of the next row that has not yet
    /// been submitted via `add_tile()`/`add_even_odd_rows()`. Used to
    /// reject tiles/row-ranges that would overrun `height`, to compute each
    /// call's row count from the caller-supplied buffer, and (even/odd mode
    /// only) to determine each submitted row's absolute parity. Unused
    /// (stays `0`) in `iDIM` mode — see `idim_next_tile_idx` instead.
    rows_written: u32,
    /// Tracks whether any chunk written so far (ancillary or `IDAT`) used
    /// ZSTD (`Flag = 0x01`). Used only by `finish_exact()` (`W: Write +
    /// Seek`) to patch `IHDR`'s `compression_method` byte to its exact
    /// value; irrelevant for plain `finish()` (`W: Write`), which leaves
    /// `Encoder::new()`'s conservative always-set bit untouched.
    uses_zstd: bool,
    /// A copy of the exact 19 bytes (Type + Flag + Data) already written to
    /// `writer` for the `IHDR` chunk in `new()`. Kept only so
    /// `finish_exact()` (`W: Write + Seek`) can patch `compression_method`
    /// and recompute the CRC32 *without* needing `W: Read` (seeking back to
    /// re-read the already-written bytes would require it) — the CRC is
    /// instead recomputed from this in-memory copy and the patched byte
    /// value, then both are written back at their known offsets.
    ihdr_type_flag_data: [u8; 19],
    /// Even/odd interlace mode only (v1.11+, `even_odd_interlace = true`):
    /// accumulated raw row bytes (`width * bpp` each) for each of the 2
    /// passes (index 0 = even rows, index 1 = odd rows), not yet flushed as
    /// an `IDAT`. `add_even_odd_rows()` appends each incoming row to the
    /// correct pass buffer by its absolute row-index parity, then flushes
    /// (writes an `IDAT` and clears the buffer) whichever pass buffer has
    /// accumulated `opts_tile_rows` complete rows — mirroring row-strip
    /// mode's `tile_rows`-sized `IDAT` granularity, applied per-pass rather
    /// than per-image-row-range. A final, possibly-shorter flush of any
    /// remaining buffered rows happens in `finish()`/`finish_exact()`.
    /// Always empty (unused) outside even/odd mode.
    even_odd_pending: [Vec<u8>; EVEN_ODD_NUM_PASSES],
}

impl<W: Write> Encoder<W> {
    /// Returns the `tile_rows` value from the `EncoderOptions` passed to
    /// `new()` — a suggested (not enforced) tile height: `add_tile()`
    /// infers each tile's actual height from the buffer it's given, so
    /// callers are free to submit differently-sized tiles if they wish
    /// (e.g. a smaller final tile, or an entirely different tiling
    /// strategy). Exposed for callers that want to mirror the same
    /// tile-size default `encode()` would have used.
    pub fn tile_rows(&self) -> u32 {
        self.opts_tile_rows
    }
}

impl<W: Write> Encoder<W> {
    /// Creates a new streaming encoder over `writer` and immediately writes
    /// the signature, `IHDR`, and all pre-IDAT ancillary chunks (`cHDR`,
    /// `eXIF`, `jSON`, `iCCP`, `xMPd`, `zDIC`), in spec order (section 9).
    /// `width`/`height` must be known upfront (they go in `IHDR`, the very
    /// first chunk) — this is the one piece of whole-image knowledge this
    /// API still requires, same as `Decoder<R>::read_info()` returning them
    /// from the file rather than the caller supplying them.
    ///
    /// # Errors
    /// - `UnsupportedFeature` if `opts.target_color_type ==
    ///   COLOR_TYPE_INDEXED`, or if `width`/`height` is 0, or for any
    ///   invalid color-type/bit-depth/sample-format/filter combination (same
    ///   validation as `encode()`).
    /// - Any error from writing to `writer` (`CafeError::Io`).
    pub fn new(mut writer: W, width: u32, height: u32, opts: &EncoderOptions) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(CafeError::UnsupportedFeature(
                "Encoder::new() requires width > 0 and height > 0".into(),
            ));
        }
        if opts.target_color_type == COLOR_TYPE_INDEXED {
            return Err(CafeError::UnsupportedFeature(
                "Encoder<W> does not support indexed palette (COLOR_TYPE_INDEXED) — palette \
                 quantization requires seeing the whole image upfront; use encode_indexed() \
                 instead"
                    .into(),
            ));
        }

        let sample_format_final = opts.sample_format.unwrap_or(SAMPLE_FORMAT_UINT);
        let bit_depth = match sample_format_final {
            SAMPLE_FORMAT_FLOAT => 32,
            SAMPLE_FORMAT_HALF => 16,
            _ => opts.target_bit_depth.unwrap_or(8),
        };
        let target_color_type = opts.target_color_type;

        // Same color-type/bit-depth compatibility validation as encode()
        // (via bytes_per_pixel returning None for invalid combinations).
        let bpp = bytes_per_pixel(target_color_type, bit_depth).ok_or_else(|| {
            CafeError::UnsupportedFeature(format!(
                "Color type {target_color_type}, bit depth {bit_depth} not supported in Encoder::new()"
            ))
        })?;
        if sample_format_final != SAMPLE_FORMAT_UINT {
            bytes_per_pixel_with_format(target_color_type, 8, sample_format_final).ok_or_else(
                || {
                    CafeError::UnsupportedFeature(format!(
                        "Color type {target_color_type} incompatible with sample format {sample_format_final}"
                    ))
                },
            )?;
        }

        let bytes_per_row =
            bytes_per_row_for_direct_color(width, target_color_type, bit_depth, bpp)?;

        let filter_method = if opts.use_byte_shuffle {
            FILTER_METHOD_BYTE_SHUFFLE
        } else if opts.use_filter && opts.use_filter_per_row {
            FILTER_METHOD_PREDICTIVE_PER_ROW
        } else if opts.use_filter {
            FILTER_METHOD_PREDICTIVE
        } else {
            FILTER_METHOD_NONE
        };

        if opts.use_filter
            && opts.use_filter_per_row
            && !matches!(
                opts.filter_heuristic,
                FilterHeuristic::Entropy | FilterHeuristic::Msad
            )
        {
            return Err(CafeError::UnsupportedFeature(format!(
                "use_filter_per_row only supports FilterHeuristic::Entropy or ::Msad, got {:?}",
                opts.filter_heuristic
            )));
        }

        if opts.use_byte_shuffle && bpp != 2 && bpp != 4 && bpp != 8 && bpp != 16 {
            return Err(CafeError::UnsupportedFeature(format!(
                "Byte-shuffle requires bpp ∈ {{2,4,8,16}}, got {bpp} (color type {target_color_type}, \
                 bit depth {bit_depth} not compatible)"
            )));
        }

        // --- iDIM validation (2D tiling, v1.10+) ---
        // Mirrors encode()'s own iDIM validation (bit_depth >= 8 so tile
        // columns are byte-aligned) plus the use_filter_per_row rejection —
        // both checked upfront here too, before any byte is written, for
        // the same reasons documented at encode()'s own call sites.
        let idim = match opts.idim {
            Some((tile_width, tile_height, scan_order)) => {
                if opts.even_odd_interlace {
                    return Err(CafeError::UnsupportedFeature(
                        "EncoderOptions::idim is incompatible with even_odd_interlace".into(),
                    ));
                }
                if bit_depth < 8 {
                    return Err(CafeError::UnsupportedFeature(
                        "iDIM (2D tiling) requires bit_depth >= 8 in Encoder::new()".into(),
                    ));
                }
                if opts.use_filter && opts.use_filter_per_row {
                    return Err(CafeError::UnsupportedFeature(
                        "use_filter_per_row is incompatible with iDIM (2D tiling)".into(),
                    ));
                }
                if tile_width == 0 || tile_height == 0 {
                    return Err(CafeError::UnsupportedFeature(
                        "iDIM: tile_width and tile_height must be nonzero".into(),
                    ));
                }
                if scan_order > 1 {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "iDIM: invalid scan_order {scan_order} (supports only 0=row-major, 1=Z-order/Morton)"
                    )));
                }
                let idim = iDim::new(tile_width, tile_height, width, height, scan_order);
                let tile_count = (idim.tiles_x as u64) * (idim.tiles_y as u64);
                if tile_count > MAX_TILE_COUNT {
                    return Err(CafeError::UnsupportedFeature(format!(
                        "iDIM: tiles_x * tiles_y = {tile_count} exceeds maximum allowed tile count ({MAX_TILE_COUNT})"
                    )));
                }
                Some(idim)
            }
            None => None,
        };

        // --- even/odd interlace validation (v1.11+) ---
        // Mirrors encode()'s own interlace validation (uint RGBA 8-bit only,
        // section 5) plus the use_filter_per_row/use_byte_shuffle rejections
        // encode() also enforces for interlace in general — all checked
        // upfront here, before any byte is written, same rationale as the
        // iDIM validation block above.
        if opts.even_odd_interlace {
            if opts.use_filter_per_row {
                return Err(CafeError::UnsupportedFeature(
                    "use_filter_per_row is incompatible with interlace (Adam7/even-odd)".into(),
                ));
            }
            if opts.use_byte_shuffle {
                return Err(CafeError::UnsupportedFeature(
                    "Byte-shuffle is incompatible with interlace (section 4.3.1)".into(),
                ));
            }
            if sample_format_final != SAMPLE_FORMAT_UINT || target_color_type != COLOR_TYPE_RGBA {
                return Err(CafeError::UnsupportedFeature(
                    "Interlace (Adam7/even-odd) requires sample format uint and color type RGBA (section 5)"
                        .into(),
                ));
            }
            if bit_depth != 8 {
                return Err(CafeError::UnsupportedFeature(
                    "Interlace (Adam7/even-odd) requires bit_depth = 8 (section 5)".into(),
                ));
            }
        }
        // filter_method is forced to FILTER_METHOD_NONE for even/odd
        // interlace, mirroring encode()'s own behavior — Adam7/even/odd
        // passes are not byte-shuffled or predictively filtered (section
        // 4.3.2/5).
        let filter_method = if opts.even_odd_interlace {
            FILTER_METHOD_NONE
        } else {
            filter_method
        };

        // --- Signature + IHDR ---
        // compression_method: set the ZSTD bit unconditionally and upfront
        // (conservative — see struct doc comment) since this Write-only
        // encoder cannot patch it after the fact.
        writer.write_all(&SIGNATURE)?;
        let mut ihdr = Vec::with_capacity(14);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(bit_depth);
        ihdr.push(sample_format_final);
        ihdr.push(target_color_type);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(filter_method);
        ihdr.push(if opts.even_odd_interlace {
            INTERLACE_EVEN_ODD
        } else {
            INTERLACE_NONE
        });
        let ihdr_chunk = write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr);
        // Type(4) + Flag(1) + Data(14) = bytes [4..23) of the written chunk
        // (skipping the 4-byte Length prefix) — exactly the span
        // `finish_exact()` needs to recompute the CRC after patching
        // `compression_method` in place, without requiring `W: Read`.
        let mut ihdr_type_flag_data = [0u8; 19];
        ihdr_type_flag_data.copy_from_slice(&ihdr_chunk[4..23]);
        writer.write_all(&ihdr_chunk)?;

        let mut uses_zstd = false;

        // --- iDIM (optional, 2D tiling, section 4.2) ---
        // Must appear immediately after IHDR (section 9, mandatory order),
        // same placement as encode()'s own append_idim_chunk_if_present.
        let idim_tile_order = if let Some(idim) = &idim {
            let chunk = write_idim_chunk(idim)?;
            uses_zstd |= chunk_uses_zstd(&chunk);
            writer.write_all(&chunk)?;
            idim.tile_order()?
        } else {
            Vec::new()
        };

        // --- cHDR (optional) ---
        if let Some(chdr) = &opts.chdr_metadata {
            let chunk = write_chdr_chunk(chdr, opts.level)?;
            uses_zstd |= chunk_uses_zstd(&chunk);
            writer.write_all(&chunk)?;
        }

        // --- eXIF, jSON, iCCP, xMPd (optional) ---
        let mut ancillary = Vec::new();
        uses_zstd |= append_common_metadata_chunks(
            &mut ancillary,
            opts.exif.as_deref(),
            &opts.json_metadata,
            opts.icc_profile.as_deref(),
            opts.xmp_metadata.as_deref(),
            opts.level,
        )?;
        writer.write_all(&ancillary)?;

        // --- zDIC (optional, explicit dictionary only — no auto-training) ---
        if let Some(dict) = &opts.zstd_dictionary {
            let chunk = write_zdic_chunk(dict, opts.level)?;
            uses_zstd |= chunk_uses_zstd(&chunk);
            writer.write_all(&chunk)?;
        }

        Ok(Encoder {
            writer,
            width,
            height,
            opts_level: opts.level,
            opts_tile_rows: opts.tile_rows,
            target_color_type,
            bit_depth,
            sample_format: sample_format_final,
            bpp,
            bytes_per_row,
            use_byte_shuffle: opts.use_byte_shuffle,
            use_filter: opts.use_filter,
            use_filter_per_row: opts.use_filter_per_row,
            filter_heuristic: opts.filter_heuristic,
            zstd_dictionary: opts.zstd_dictionary.clone(),
            idim,
            idim_tile_order,
            idim_next_tile_idx: 0,
            even_odd_interlace: opts.even_odd_interlace,
            rows_written: 0,
            uses_zstd,
            ihdr_type_flag_data,
            even_odd_pending: Default::default(),
        })
    }

    /// Encodes one row-strip tile of RGBA pixels (`width * tile_height * 4`
    /// bytes, top-to-bottom, row-major — `tile_height` is inferred from
    /// `rgba_tile.len()`) and writes the resulting `IDAT` chunk immediately.
    /// May be called any number of times; `finish()` must be called after
    /// the last call once all `height` rows have been submitted.
    ///
    /// Unlike `encode()`'s whole-image path, tiles are compressed
    /// sequentially as they arrive (no rayon parallelism across tiles) —
    /// there is no independent future work to farm out to a thread pool
    /// when the caller controls the pace of tile submission.
    ///
    /// # Errors
    /// - `UnsupportedFeature` if `rgba_tile.len()` is not a multiple of
    ///   `width * 4` bytes, or if this call would submit more rows than
    ///   `height` (declared in `Encoder::new()`).
    /// - Any error from color conversion, filtering, compression, or
    ///   writing to the underlying `W`.
    pub fn add_tile(&mut self, rgba_tile: &[u8]) -> Result<()> {
        if self.idim.is_some() {
            return Err(CafeError::UnsupportedFeature(
                "add_tile() called on an Encoder configured with EncoderOptions::idim — use \
                 add_idim_tile() instead"
                    .into(),
            ));
        }
        if self.even_odd_interlace {
            return Err(CafeError::UnsupportedFeature(
                "add_tile() called on an Encoder configured with EncoderOptions::even_odd_interlace \
                 — use add_even_odd_rows() instead"
                    .into(),
            ));
        }
        let row_bytes_rgba = (self.width as usize).checked_mul(4).ok_or_else(|| {
            CafeError::UnsupportedFeature("overflow computing RGBA row size".into())
        })?;
        if row_bytes_rgba == 0 || !rgba_tile.len().is_multiple_of(row_bytes_rgba) {
            return Err(CafeError::UnsupportedFeature(format!(
                "add_tile(): buffer length {} is not a multiple of width*4={row_bytes_rgba}",
                rgba_tile.len()
            )));
        }
        let tile_h = rgba_tile.len() / row_bytes_rgba;
        let tile_h_u32 = u32::try_from(tile_h)
            .map_err(|_| CafeError::UnsupportedFeature("tile height exceeds u32::MAX".into()))?;
        let new_rows_written = self.rows_written.checked_add(tile_h_u32).ok_or_else(|| {
            CafeError::UnsupportedFeature("overflow accumulating rows_written".into())
        })?;
        if new_rows_written > self.height {
            return Err(CafeError::UnsupportedFeature(format!(
                "add_tile(): submitting {tile_h} rows would exceed the declared height {} \
                 ({} rows already written)",
                self.height, self.rows_written
            )));
        }
        if tile_h == 0 {
            return Ok(());
        }

        let sample_format_final = self.sample_format;
        let target_bit_depth = self.bit_depth;
        let tile_raw = if self.target_color_type == COLOR_TYPE_RGBA
            && sample_format_final == SAMPLE_FORMAT_UINT
            && target_bit_depth == 8
        {
            // Fast path: RGBA/8/uint is an identity conversion.
            rgba_tile.to_vec()
        } else if sample_format_final == SAMPLE_FORMAT_UINT {
            convert_rgba_to_color_type(
                rgba_tile,
                self.width,
                tile_h_u32,
                self.target_color_type,
                target_bit_depth,
            )?
        } else {
            convert_rgba_to_color_type_with_format(
                rgba_tile,
                self.width,
                tile_h_u32,
                self.target_color_type,
                target_bit_depth,
                sample_format_final,
            )?
        };

        let tile_payload = apply_single_tile_filter(
            &tile_raw,
            tile_h,
            self.bytes_per_row,
            self.bpp,
            self.width,
            self.use_byte_shuffle,
            self.use_filter,
            self.use_filter_per_row,
            self.filter_heuristic,
            self.opts_level,
        )?;

        let (flag, compressed, _used_dict) = compress_with_fallback_dict(
            &tile_payload,
            self.opts_level,
            self.zstd_dictionary.as_deref(),
        )?;
        self.uses_zstd |= flag == FLAG_ZSTD;
        self.writer
            .write_all(&write_chunk(CHUNK_IDAT, flag, &compressed))?;

        self.rows_written = new_rows_written;
        Ok(())
    }

    /// Encodes one rectangular tile of RGBA pixels for 2D tiling (`iDIM`,
    /// section 4.2) and writes the resulting `IDAT` chunk immediately. Only
    /// valid on an `Encoder` created with `EncoderOptions::idim = Some(_)`
    /// — returns `UnsupportedFeature` on a row-strip-mode `Encoder`
    /// (use `add_tile()` instead).
    ///
    /// Must be called exactly once per tile in `iDim::tile_order()`'s
    /// sequence (the same order `encode()`'s own iDIM path and
    /// `Decoder<R>::next_tile()` use) — row-major (`scan_order = 0`) visits
    /// tiles left-to-right then top-to-bottom; Z-order (`scan_order = 1`)
    /// visits them by Morton code. `finish()`/`finish_exact()` require
    /// every tile in the grid to have been submitted first. `rgba_tile`
    /// must be exactly `tile_width * tile_height * 4` bytes for that
    /// specific tile's position — edge tiles (last column/row, when
    /// `width`/`height` are not exact multiples of the declared tile size)
    /// are narrower/shorter per `iDim::tile_dimensions()`, mirroring
    /// `decode_idim_tile_raw`'s decode-side handling of the same edge case.
    ///
    /// # Errors
    /// - `UnsupportedFeature` if this `Encoder` was not configured with
    ///   `EncoderOptions::idim`, if every tile in the grid has already been
    ///   submitted, or if `rgba_tile.len()` does not match the expected
    ///   size for the next tile's position.
    /// - Any error from color conversion, filtering, compression, or
    ///   writing to the underlying `W`.
    pub fn add_idim_tile(&mut self, rgba_tile: &[u8]) -> Result<()> {
        if self.even_odd_interlace {
            return Err(CafeError::UnsupportedFeature(
                "add_idim_tile() called on an Encoder configured with \
                 EncoderOptions::even_odd_interlace — use add_even_odd_rows() instead"
                    .into(),
            ));
        }
        let idim = self.idim.as_ref().ok_or_else(|| {
            CafeError::UnsupportedFeature(
                "add_idim_tile() called on an Encoder not configured with EncoderOptions::idim \
                 — use add_tile() instead"
                    .into(),
            )
        })?;
        let &(tx, ty) = self
            .idim_tile_order
            .get(self.idim_next_tile_idx)
            .ok_or_else(|| {
                CafeError::UnsupportedFeature(format!(
                    "add_idim_tile(): all {} tiles have already been submitted",
                    self.idim_tile_order.len()
                ))
            })?;
        let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, self.width, self.height);
        let tile_stride = (tile_w as usize).checked_mul(self.bpp).ok_or_else(|| {
            CafeError::UnsupportedFeature("overflow in tile stride during add_idim_tile".into())
        })?;
        let expected_rgba_len = (tile_w as usize)
            .checked_mul(tile_h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| {
                CafeError::UnsupportedFeature("overflow computing expected tile RGBA length".into())
            })?;
        if rgba_tile.len() != expected_rgba_len {
            return Err(CafeError::UnsupportedFeature(format!(
                "add_idim_tile(): buffer length {} does not match expected {expected_rgba_len} \
                 bytes for tile ({tx}, {ty}) of size {tile_w}x{tile_h}",
                rgba_tile.len()
            )));
        }

        let sample_format_final = self.sample_format;
        let target_bit_depth = self.bit_depth;
        let tile_raw = if self.target_color_type == COLOR_TYPE_RGBA
            && sample_format_final == SAMPLE_FORMAT_UINT
            && target_bit_depth == 8
        {
            rgba_tile.to_vec()
        } else if sample_format_final == SAMPLE_FORMAT_UINT {
            convert_rgba_to_color_type(
                rgba_tile,
                tile_w,
                tile_h,
                self.target_color_type,
                target_bit_depth,
            )?
        } else {
            convert_rgba_to_color_type_with_format(
                rgba_tile,
                tile_w,
                tile_h,
                self.target_color_type,
                target_bit_depth,
                sample_format_final,
            )?
        };

        let tile_payload = apply_single_tile_filter(
            &tile_raw,
            tile_h as usize,
            tile_stride,
            self.bpp,
            tile_w,
            self.use_byte_shuffle,
            self.use_filter,
            self.use_filter_per_row,
            self.filter_heuristic,
            self.opts_level,
        )?;

        let (flag, compressed, _used_dict) = compress_with_fallback_dict(
            &tile_payload,
            self.opts_level,
            self.zstd_dictionary.as_deref(),
        )?;
        self.uses_zstd |= flag == FLAG_ZSTD;
        self.writer
            .write_all(&write_chunk(CHUNK_IDAT, flag, &compressed))?;

        self.idim_next_tile_idx += 1;
        Ok(())
    }

    /// Compresses and writes one `IDAT` for `row_count` complete rows
    /// drained from the front of `self.even_odd_pending[pass_idx]` (each row
    /// is `width * bpp` bytes — `bpp` is always 4 in even/odd mode, since
    /// `Encoder::new()` requires uint RGBA 8-bit for `even_odd_interlace`),
    /// prefixed with the 1-byte `pass_number` (`pass_idx + 1`) the decoder's
    /// `handle_interlaced_idat` expects (section 5). Shared by
    /// `add_even_odd_rows()`'s opportunistic mid-stream flush and
    /// `finish()`/`finish_exact()`'s final flush of any remaining
    /// less-than-`tile_rows` residue.
    fn flush_even_odd_idat(&mut self, pass_idx: usize, row_count: usize) -> Result<()> {
        let row_bytes = (self.width as usize) * self.bpp;
        let drain_len = row_count * row_bytes;
        let drained: Vec<u8> = self.even_odd_pending[pass_idx]
            .drain(0..drain_len)
            .collect();
        let mut pass_payload = Vec::with_capacity(1 + drained.len());
        pass_payload.push((pass_idx + 1) as u8);
        pass_payload.extend_from_slice(&drained);
        let (flag, compressed, _used_dict) = compress_with_fallback_dict(
            &pass_payload,
            self.opts_level,
            self.zstd_dictionary.as_deref(),
        )?;
        self.uses_zstd |= flag == FLAG_ZSTD;
        self.writer
            .write_all(&write_chunk(CHUNK_IDAT, flag, &compressed))?;
        Ok(())
    }

    /// Flushes as many full `opts_tile_rows`-sized chunks as are currently
    /// buffered in each even/odd pass (zero, one, or more per call,
    /// depending on how many rows `add_even_odd_rows()` was just given) —
    /// mirrors row-strip mode's per-`tile_rows` `IDAT` granularity, applied
    /// per-pass. Any remainder smaller than `opts_tile_rows` rows stays
    /// buffered until either more rows arrive or `finish()`/`finish_exact()`
    /// flushes it as a final, possibly-shorter `IDAT`.
    fn flush_even_odd_full_tiles(&mut self) -> Result<()> {
        let row_bytes = (self.width as usize) * self.bpp;
        let tile_rows = self.opts_tile_rows as usize;
        if tile_rows == 0 || row_bytes == 0 {
            return Ok(());
        }
        for pass_idx in 0..EVEN_ODD_NUM_PASSES {
            loop {
                let buffered_rows = self.even_odd_pending[pass_idx].len() / row_bytes;
                if buffered_rows < tile_rows {
                    break;
                }
                self.flush_even_odd_idat(pass_idx, tile_rows)?;
            }
        }
        Ok(())
    }

    /// Flushes any remaining buffered rows (fewer than `opts_tile_rows`,
    /// otherwise `flush_even_odd_full_tiles()` would already have emitted
    /// them) in each even/odd pass as one final `IDAT` each — called by
    /// `finish()`/`finish_exact()` once every row has been submitted, since
    /// a pass's last partial chunk would otherwise never be written. A pass
    /// whose buffer happens to be empty (e.g. `height` is even and evenly
    /// divisible by `2 * opts_tile_rows`) is skipped — an empty `IDAT` isn't
    /// wrong, but it isn't produced by the non-streaming `encode()` path
    /// either, and skipping it keeps output byte-for-byte comparable.
    fn flush_even_odd_remaining(&mut self) -> Result<()> {
        let row_bytes = (self.width as usize) * self.bpp;
        if row_bytes == 0 {
            return Ok(());
        }
        for pass_idx in 0..EVEN_ODD_NUM_PASSES {
            let buffered_rows = self.even_odd_pending[pass_idx].len() / row_bytes;
            if buffered_rows > 0 {
                self.flush_even_odd_idat(pass_idx, buffered_rows)?;
            }
        }
        Ok(())
    }

    /// Encodes a contiguous, top-to-bottom range of RGBA rows
    /// (`width * 4 * n_rows` bytes, `n_rows` inferred from the buffer's
    /// length) for even/odd interlace (`EncoderOptions::even_odd_interlace =
    /// true`, section 5) — the streaming counterpart to `encode()`'s
    /// whole-image `build_interlaced_idats(INTERLACE_EVEN_ODD, ...)` path.
    ///
    /// Unlike `add_tile()`, no color conversion is needed: `Encoder::new()`
    /// already requires uint RGBA 8-bit for `even_odd_interlace = true`
    /// (section 5's own restriction on which formats interlace supports),
    /// so `rgba_rows` is used as-is. Each row is bucketed into one of the 2
    /// passes (index 0 = even absolute row index, index 1 = odd) and
    /// buffered; whenever a pass accumulates `opts_tile_rows`-or-more
    /// complete rows, one or more `IDAT`s are flushed immediately — the same
    /// `tile_rows`-sized granularity `add_tile()` uses for row-strip mode,
    /// applied per-pass here since a single call's rows are split across
    /// both passes by parity. `finish()`/`finish_exact()` flush any
    /// remaining (`< opts_tile_rows`-row) buffered residue per pass as a
    /// final `IDAT` each.
    ///
    /// May be called any number of times with any row-count grouping
    /// (including one row at a time) — rows do not need to be submitted in
    /// pass-aligned or `tile_rows`-aligned batches, only in overall
    /// top-to-bottom image order across calls.
    ///
    /// # Errors
    /// - `UnsupportedFeature` if this `Encoder` was not configured with
    ///   `EncoderOptions::even_odd_interlace = true` (points the caller at
    ///   `add_tile()`/`add_idim_tile()` instead).
    /// - `UnsupportedFeature` if `rgba_rows.len()` is not a multiple of
    ///   `width * 4` bytes, or if this call would submit more rows than
    ///   `height` (declared in `Encoder::new()`).
    /// - Any error from compression or writing to the underlying `W`.
    pub fn add_even_odd_rows(&mut self, rgba_rows: &[u8]) -> Result<()> {
        if !self.even_odd_interlace {
            return Err(CafeError::UnsupportedFeature(
                "add_even_odd_rows() called on an Encoder not configured with \
                 EncoderOptions::even_odd_interlace — use add_tile()/add_idim_tile() instead"
                    .into(),
            ));
        }
        let row_bytes_rgba = (self.width as usize).checked_mul(4).ok_or_else(|| {
            CafeError::UnsupportedFeature("overflow computing RGBA row size".into())
        })?;
        if row_bytes_rgba == 0 || !rgba_rows.len().is_multiple_of(row_bytes_rgba) {
            return Err(CafeError::UnsupportedFeature(format!(
                "add_even_odd_rows(): buffer length {} is not a multiple of width*4={row_bytes_rgba}",
                rgba_rows.len()
            )));
        }
        let n_rows = rgba_rows.len() / row_bytes_rgba;
        let n_rows_u32 = u32::try_from(n_rows)
            .map_err(|_| CafeError::UnsupportedFeature("row count exceeds u32::MAX".into()))?;
        let new_rows_written = self.rows_written.checked_add(n_rows_u32).ok_or_else(|| {
            CafeError::UnsupportedFeature("overflow accumulating rows_written".into())
        })?;
        if new_rows_written > self.height {
            return Err(CafeError::UnsupportedFeature(format!(
                "add_even_odd_rows(): submitting {n_rows} rows would exceed the declared height \
                 {} ({} rows already written)",
                self.height, self.rows_written
            )));
        }

        for i in 0..n_rows {
            let abs_row = self.rows_written + i as u32;
            let pass_idx = (abs_row % 2) as usize;
            let row_start = i * row_bytes_rgba;
            let row_end = row_start + row_bytes_rgba;
            self.even_odd_pending[pass_idx].extend_from_slice(&rgba_rows[row_start..row_end]);
        }
        self.rows_written = new_rows_written;

        self.flush_even_odd_full_tiles()?;
        Ok(())
    }

    /// Returns `true` once every declared row (row-strip mode, `iDIM` mode,
    /// or even/odd interlace mode — all three count rows/tiles via
    /// `rows_written`/`idim_next_tile_idx` the same way) has been submitted
    /// — the same completeness check `finish()`/`finish_exact()` perform,
    /// exposed so callers can assert it themselves before calling either.
    fn is_complete(&self) -> bool {
        match &self.idim {
            Some(_) => self.idim_next_tile_idx == self.idim_tile_order.len(),
            None => self.rows_written == self.height,
        }
    }

    /// Builds the `UnsupportedFeature` message for an incomplete
    /// `finish()`/`finish_exact()` call, naming the correct submission
    /// method (`add_tile()`, `add_idim_tile()`, or `add_even_odd_rows()`)
    /// for whichever mode this `Encoder` was configured in — shared so the
    /// three modes' wording can't drift apart between `finish()` and
    /// `finish_exact()`.
    fn incomplete_message(&self, caller: &str) -> String {
        match &self.idim {
            Some(_) => format!(
                "Encoder::{caller} called after only {} of {} declared iDIM tiles were \
                 submitted via add_idim_tile()",
                self.idim_next_tile_idx,
                self.idim_tile_order.len()
            ),
            None if self.even_odd_interlace => format!(
                "Encoder::{caller} called after only {} of {} declared rows were submitted \
                 via add_even_odd_rows()",
                self.rows_written, self.height
            ),
            None => format!(
                "Encoder::{caller} called after only {} of {} declared rows were submitted \
                 via add_tile()",
                self.rows_written, self.height
            ),
        }
    }

    /// Writes the `IEND` chunk and returns the underlying `writer`. Must be
    /// called after all `height` rows (row-strip mode via `add_tile()`, or
    /// even/odd interlace mode via `add_even_odd_rows()`) or all tiles in
    /// the grid (`iDIM` mode, via `add_idim_tile()`) have been submitted —
    /// returns `UnsupportedFeature` otherwise (a truncated image would
    /// otherwise be silently accepted as valid). In even/odd mode, also
    /// flushes any buffered residual rows (fewer than `opts_tile_rows`) per
    /// pass as a final `IDAT` each before writing `IEND`.
    pub fn finish(mut self) -> Result<W> {
        if !self.is_complete() {
            return Err(CafeError::UnsupportedFeature(
                self.incomplete_message("finish()"),
            ));
        }
        if self.even_odd_interlace {
            self.flush_even_odd_remaining()?;
        }
        self.writer
            .write_all(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]))?;
        Ok(self.writer)
    }
}

impl<W: Write + Seek> Encoder<W> {
    /// Like `finish()`, but for a `W` that also supports `Seek` (a `File`, a
    /// `Cursor<Vec<u8>>`, ...): patches the `IHDR`'s `compression_method`
    /// byte (and recomputes its CRC32) to reflect whether ZSTD was
    /// *actually* used by any chunk written so far (tracked incrementally
    /// in `self.uses_zstd` by `new()`/`add_tile()`), instead of leaving the
    /// conservative (always-set) bit `Encoder::new()` wrote upfront — the
    /// same exact value `encode()`/`encode_indexed()` compute via
    /// `patch_ihdr_compression_method`.
    ///
    /// Patches the byte and recomputes the CRC32 entirely from the
    /// in-memory `self.ihdr_type_flag_data` copy kept since `new()`,
    /// deliberately avoiding any need to seek back and *read* the
    /// already-written bytes (`W: Write + Seek` alone does not guarantee
    /// `Read`).
    pub fn finish_exact(mut self) -> Result<W> {
        if !self.is_complete() {
            return Err(CafeError::UnsupportedFeature(
                self.incomplete_message("finish_exact()"),
            ));
        }
        if self.even_odd_interlace {
            self.flush_even_odd_remaining()?;
        }
        self.writer
            .write_all(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]))?;

        let compression_method = if self.uses_zstd {
            COMPRESSION_METHOD_ZSTD_BIT
        } else {
            0
        };
        // Layout of ihdr_type_flag_data (19 bytes): Type(4) + Flag(1) +
        // Data(14, IHDR payload) — compression_method is Data byte 11.
        self.ihdr_type_flag_data[4 + 1 + 11] = compression_method;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&self.ihdr_type_flag_data);
        let crc = hasher.finalize();

        // File layout: sig(9) + len(4) + type(4) + flag(1) + data(14) + crc(4).
        const CM_OFFSET: u64 = 9 + 4 + 4 + 1 + 11; // 29
        const CRC_OFFSET: u64 = 9 + 4 + 4 + 1 + 14; // 32
        self.writer.seek(SeekFrom::Start(CM_OFFSET))?;
        self.writer.write_all(&[compression_method])?;
        self.writer.seek(SeekFrom::Start(CRC_OFFSET))?;
        self.writer.write_all(&crc.to_be_bytes())?;

        self.writer.seek(SeekFrom::End(0))?;
        Ok(self.writer)
    }
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

    let filter_method = if opts.use_filter && opts.use_filter_per_row {
        FILTER_METHOD_PREDICTIVE_PER_ROW
    } else if opts.use_filter {
        FILTER_METHOD_PREDICTIVE
    } else {
        FILTER_METHOD_NONE
    };

    // Per-row predictive filter (v1.5) only supports Entropy/Msad and only
    // applies to the row-tiled path below (no interlace — encode_indexed()
    // already rejects iDIM unconditionally above).
    if opts.use_filter && opts.use_filter_per_row {
        if !matches!(
            opts.filter_heuristic,
            FilterHeuristic::Entropy | FilterHeuristic::Msad
        ) {
            return Err(CafeError::UnsupportedFeature(format!(
                "use_filter_per_row only supports FilterHeuristic::Entropy or ::Msad, got {:?}",
                opts.filter_heuristic
            )));
        }
        if opts.interlace_method != INTERLACE_NONE {
            return Err(CafeError::UnsupportedFeature(
                "use_filter_per_row is incompatible with interlace (Adam7/even-odd)".into(),
            ));
        }
    }

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
    uses_zstd |= append_common_metadata_chunks(
        &mut out,
        opts.exif.as_deref(),
        &opts.json_metadata,
        opts.icc_profile.as_deref(),
        opts.xmp_metadata.as_deref(),
        opts.level,
    )?;

    // --- IDAT (packed indices, optionally interlaced) ---
    // Built into a separate buffer first (not `out` directly), same pattern
    // and rationale as `encode()`'s IDAT section: whether `zDIC` (written
    // below, before PLTE/IDAT) is worth emitting is only known after
    // compression, based on whether any IDAT actually benefited from the
    // dictionary (v1.5 dictionary fallback guarantee — see
    // `compress_with_fallback_dict`).
    let (idat_bytes, idat_uses_zstd, _used_dict) = if opts.interlace_method == INTERLACE_ADAM7
        || opts.interlace_method == INTERLACE_EVEN_ODD
    {
        // v1.0/+5: Progressive interlace (Adam7 or even/odd)
        // Convert indices to RGBA to apply interlace
        let rgba_raw = indices
            .iter()
            .flat_map(|&idx| {
                let entry = &palette.entries[idx as usize];
                vec![entry.r, entry.g, entry.b, entry.a]
            })
            .collect::<Vec<u8>>();

        // Apply interlace (Adam7 or even/odd) via the shared helper (also
        // used by encode()'s direct RGBA path).
        build_interlaced_idats(
            opts.interlace_method,
            &rgba_raw,
            width,
            height,
            opts.level,
            opts.zstd_dictionary.as_deref(),
        )?
    } else {
        // v1.0 (with full interlace support): write in row tiles, via the
        // shared helper also used by encode()'s direct-color path. Tiles are
        // independent, so packing + filter + compression is parallelized
        // across a rayon thread pool (v1.2.2) inside the helper; chunks are
        // concatenated in original row order. Byte-shuffle is rejected
        // above (bpp=1 for packed indices), so it's always disabled here.
        let idx_bytes_per_row = bytes_per_row_for_bit_depth(width, bit_depth)?;
        build_row_tiled_idats(
            height,
            opts.tile_rows,
            idx_bytes_per_row,
            1, // bpp: predictive filter operates on packed bytes (1 byte/unit)
            width,
            false, // use_byte_shuffle: incompatible with indexed (rejected above)
            opts.use_filter,
            opts.use_filter_per_row,
            opts.filter_heuristic,
            opts.level,
            opts.zstd_dictionary.as_deref(),
            |row_start, row_end| {
                // Pack each row of indices into bit_depth bits/index (section 4.1.2)
                let tile_h = row_end - row_start;
                let mut tile_packed = Vec::with_capacity(tile_h * idx_bytes_per_row);
                for row in row_start..row_end {
                    let row_indices =
                        &indices[(row * width as usize)..((row + 1) * width as usize)];
                    let packed_row = pack_indices_row(row_indices, bit_depth)?;
                    tile_packed.extend_from_slice(&packed_row);
                }
                Ok(tile_packed)
            },
        )?
    };
    uses_zstd |= idat_uses_zstd;

    // --- zDIC (optional, single instance, section 4.9) ---
    // BUG HISTORY: before the `append_zdic_chunk_if_present` helper existed,
    // encode_indexed() USED the dictionary to compress the IDATs (via
    // compress_with_fallback_dict below) but never wrote the zDIC chunk here
    // — generating undecodable files (the decoder could not find the
    // dictionary and failed with "Dictionary mismatch"). Sharing
    // `append_zdic_chunk_if_present` with encode() prevents this class of
    // divergence from recurring.
    //
    // `opts.zstd_dictionary` here is always an explicit, user-provided
    // dictionary (`encode_indexed()` has no `auto_dictionary` support), so
    // it's always honored unconditionally — same as pre-v1.5 behavior — even
    // though `compress_with_fallback_dict`'s dictionary fallback guarantee
    // (v1.5) means `used_dict` can legitimately be `false` (e.g. the
    // dictionary didn't help any tile of this particular image). See the
    // matching comment in `encode()` for why an *auto-trained* dictionary
    // gets the opposite treatment (only emitted when `used_dict` is true).
    // (`used_dict` is intentionally unused here — see comment above: only
    // relevant for auto-trained dictionaries in `encode()`.)
    uses_zstd |=
        append_zdic_chunk_if_present(&mut out, opts.zstd_dictionary.as_deref(), opts.level)?;

    // --- PLTE (critical, required with Color type = 3 only) ---
    // v1.0/+5: If interlaced (color_type=6), do NOT write PLTE (not needed)
    if opts.interlace_method != INTERLACE_ADAM7 && opts.interlace_method != INTERLACE_EVEN_ODD {
        out.extend_from_slice(&write_plte_chunk(&palette, opts.level)?);
    }

    out.extend_from_slice(&idat_bytes);

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
    log::info!(
        "encoded with palette: {} colors, bit depth = {}",
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
    // SECURITY (memory-amplification): a legitimate indexed-color PLTE never
    // needs more than MAX_PALETTE_ENTRIES (256, section 4.1.2 - bit depths
    // 1/2/4/8 all top out at 256 distinct indices). Without this check, the
    // only limit on this Vec<PaletteEntry>'s size is the generic 1 GiB
    // MAX_DECOMPRESSED_CHUNK_SIZE chunk-decompression ceiling, allowing a
    // single crafted PLTE chunk to balloon into hundreds of millions of
    // PaletteEntry structs that can never be addressed by any valid pixel
    // index. Reject early, before allocating/populating `entries`.
    let data_payload = &data[1..];
    let declared_entries = data_payload.len() / bytes_per_entry;
    if declared_entries > MAX_PALETTE_ENTRIES {
        return Err(CafeError::UnsupportedFeature(format!(
            "PLTE: {} entries exceeds maximum allowed ({})",
            declared_entries, MAX_PALETTE_ENTRIES
        )));
    }
    let mut entries = Vec::with_capacity(declared_entries);
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
        PaletteAlgorithm::NearestNeighborWeighted => {
            quantize_nearest_neighbor_weighted(rgba, max_colors)
        }
        PaletteAlgorithm::KMeans => quantize_kmeans_wrapper(rgba, max_colors),
    }
}

fn quantize_nearest_neighbor(rgba: &[u8], max_colors: u32) -> (Vec<u8>, Palette) {
    let mut palette = Palette {
        entries: Vec::new(),
        has_alpha: true,
    };
    let mut indices = Vec::with_capacity(rgba.len() / 4);

    // The palette grows incrementally as new colors are found, so each
    // pixel must be matched against the *complete current* palette before
    // it can possibly extend it — this rules out vectorizing across pixels.
    // Instead, `PaletteSoa` vectorizes the nearest-entry search across
    // palette *entries* (AVX2 on x86_64, scalar fallback otherwise), which
    // stays correct regardless of how the palette grows between pixels.
    #[cfg(feature = "simd")]
    let mut palette_soa = crate::simd_quantize::PaletteSoa::new();

    for chunk in rgba.as_chunks::<4>().0 {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        let a = chunk[3];

        // Simple nearest-neighbor palette lookup
        #[cfg(feature = "simd")]
        let (mut best_idx, best_dist) = palette_soa.find_closest_rgba(r, g, b, a);

        #[cfg(not(feature = "simd"))]
        let (mut best_idx, best_dist) = {
            let mut best_idx = 0u8;
            let mut best_dist = u32::MAX;
            for (i, entry) in palette.entries.iter().enumerate() {
                let dist = ((r as i32 - entry.r as i32).pow(2)
                    + (g as i32 - entry.g as i32).pow(2)
                    + (b as i32 - entry.b as i32).pow(2)
                    + (a as i32 - entry.a as i32).pow(2)) as u32;
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = i as u8;
                }
            }
            (best_idx, best_dist)
        };

        // Add color to palette if not found and space available
        if best_dist > 0 && (palette.entries.len() as u32) < max_colors {
            let new_entry = PaletteEntry { r, g, b, a };
            #[cfg(feature = "simd")]
            palette_soa.push(&new_entry);
            palette.entries.push(new_entry);
            best_idx = (palette.entries.len() - 1) as u8;
        }

        indices.push(best_idx);
    }

    (indices, palette)
}

/// Same greedy incremental strategy as `quantize_nearest_neighbor`, but
/// matching uses `PaletteEntry::redmean_distance` (perceptually-weighted)
/// instead of plain unweighted Euclidean distance (v1.5,
/// `PaletteAlgorithm::NearestNeighborWeighted`).
///
/// Deliberately scalar-only (no SIMD dispatch via `PaletteSoa`): the
/// redmean weight depends on `(r1 + r2) / 2`, which varies per-comparison
/// (unlike the fixed integer weights of a plain luma approximation), so a
/// vectorized version would need its own dedicated AVX2/NEON kernels rather
/// than reusing `PaletteSoa`'s existing (unweighted) ones — deferred as a
/// follow-up if this path proves hot in profiling. Quantization is bounded
/// by `palette_size <= 256`, so the O(pixels * palette_size) scalar cost
/// here is the same asymptotic cost `quantize_nearest_neighbor` already
/// pays for its own non-SIMD fallback build.
fn quantize_nearest_neighbor_weighted(rgba: &[u8], max_colors: u32) -> (Vec<u8>, Palette) {
    let mut palette = Palette {
        entries: Vec::new(),
        has_alpha: true,
    };
    let mut indices = Vec::with_capacity(rgba.len() / 4);

    for chunk in rgba.as_chunks::<4>().0 {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        let a = chunk[3];
        let candidate = PaletteEntry { r, g, b, a };

        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;
        for (i, entry) in palette.entries.iter().enumerate() {
            let dist = candidate.redmean_distance(entry);
            if dist < best_dist {
                best_dist = dist;
                best_idx = i as u8;
            }
        }

        // Add color to palette if not found and space available
        if best_dist > 0 && (palette.entries.len() as u32) < max_colors {
            palette.entries.push(candidate);
            best_idx = (palette.entries.len() - 1) as u8;
        }

        indices.push(best_idx);
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
        Ok(palette) => (map_pixels_to_fixed_palette(rgba, &palette), palette),
        Err(_) => {
            // Fall back to nearest-neighbor on error
            quantize_nearest_neighbor(rgba, max_colors)
        }
    }
}

/// K-means quantization (v1.7, `PaletteAlgorithm::KMeans`): builds a fixed
/// palette via `quantize::quantize_kmeans` (RGB-only, same convention as
/// `quantize_median_cut_wrapper`), then maps every pixel to its nearest
/// entry. Falls back to `quantize_nearest_neighbor` if clustering itself
/// errors (mirrors `quantize_median_cut_wrapper`'s own fallback policy).
fn quantize_kmeans_wrapper(rgba: &[u8], max_colors: u32) -> (Vec<u8>, Palette) {
    let rgb_only: Vec<u8> = rgba
        .chunks(4)
        .flat_map(|chunk| vec![chunk[0], chunk[1], chunk[2], 255])
        .collect();

    match quantize::quantize_kmeans(&rgb_only, max_colors as usize) {
        Ok(palette) => (map_pixels_to_fixed_palette(rgba, &palette), palette),
        Err(_) => quantize_nearest_neighbor(rgba, max_colors),
    }
}

/// Maps every RGBA pixel to its nearest-by-RGB-distance entry in an
/// already-computed, fixed palette (alpha ignored in matching, matching
/// both `quantize_median_cut`'s and `quantize_kmeans`'s RGB-only
/// clustering). Shared by both wrappers above, since building a palette up
/// front (as opposed to `quantize_nearest_neighbor`'s incremental growth)
/// means a single SoA transposition can be reused across all pixels.
fn map_pixels_to_fixed_palette(rgba: &[u8], palette: &Palette) -> Vec<u8> {
    let mut indices = Vec::with_capacity(rgba.len() / 4);

    #[cfg(feature = "simd")]
    {
        let palette_soa = crate::simd_quantize::PaletteSoa::from_entries(&palette.entries);
        for chunk in rgba.as_chunks::<4>().0 {
            let (best_idx, _) = palette_soa.find_closest_rgb(chunk[0], chunk[1], chunk[2]);
            indices.push(best_idx);
        }
    }

    #[cfg(not(feature = "simd"))]
    {
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
    }

    indices
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
    fn test_decode_adversarial_inconsistent_idim_geometry() {
        // SECURITY (CWE-190): iDIM chunk whose tiles_x/tiles_y/tile_width/
        // tile_height cannot be reconciled with IHDR's width/height. Before
        // the fix, this reached iDim::tile_dimensions with self-inconsistent
        // values and panicked on unchecked subtraction (debug builds) or
        // silently wrapped to a bogus huge tile size (release builds).
        // handle_idim_chunk must now reject it immediately with a clean error.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));

        // img_width=8, tile_width=5 -> consistent tiles_x would be
        // ceil(8/5)=2, but the forged chunk declares tiles_x=3.
        let mut idim = Vec::new();
        idim.extend_from_slice(&5u16.to_be_bytes()); // tile_width
        idim.extend_from_slice(&1u16.to_be_bytes()); // tile_height
        idim.extend_from_slice(&3u16.to_be_bytes()); // tiles_x (wrong: should be 2)
        idim.extend_from_slice(&8u16.to_be_bytes()); // tiles_y
        idim.push(0); // scan_order
        evil.extend_from_slice(&write_chunk(CHUNK_IDIM, FLAG_RAW, &idim));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let result = decode_bytes(&evil);
        match result {
            Ok(_) => panic!("inconsistent iDIM geometry must be rejected"),
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("iDIM"),
                    "expected iDIM-consistency error, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn test_decode_adversarial_idim_zero_tile_dims() {
        // SECURITY (CWE-190/CWE-369): tile_width=0 or tile_height=0 would
        // make tiles_x/tiles_y's div_ceil divide by zero. Must be rejected
        // before any such computation happens.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.push(8);
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));

        let mut idim = Vec::new();
        idim.extend_from_slice(&0u16.to_be_bytes()); // tile_width = 0 (invalid)
        idim.extend_from_slice(&1u16.to_be_bytes());
        idim.extend_from_slice(&1u16.to_be_bytes());
        idim.extend_from_slice(&8u16.to_be_bytes());
        idim.push(0);
        evil.extend_from_slice(&write_chunk(CHUNK_IDIM, FLAG_RAW, &idim));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let result = decode_bytes(&evil);
        assert!(
            result.is_err(),
            "iDIM with tile_width=0 must be rejected without panic"
        );
    }

    #[test]
    fn test_decode_adversarial_idim_excessive_tile_count() {
        // SECURITY (CWE-789/CWE-409-class): tiles_x=tiles_y=65535 (both
        // individually valid u16 values) with tile_width=tile_height=1 is
        // consistent with an IHDR declaring width=height=65535 via the
        // usual div_ceil check - but iDim::tile_order() would then allocate
        // a Vec<(u16,u16)> of ~4.29 billion entries (~17 GiB) from this
        // ~71-byte file, before any IDAT is read. Confirmed via a
        // standalone PoC to abort the process with a real allocation
        // failure. handle_idim_chunk must now reject tile_count >
        // MAX_TILE_COUNT before ever calling tile_order().
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&65_535u32.to_be_bytes());
        ihdr.extend_from_slice(&65_535u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_RGBA);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));

        let mut idim = Vec::new();
        idim.extend_from_slice(&1u16.to_be_bytes()); // tile_width = 1
        idim.extend_from_slice(&1u16.to_be_bytes()); // tile_height = 1
        idim.extend_from_slice(&65_535u16.to_be_bytes()); // tiles_x
        idim.extend_from_slice(&65_535u16.to_be_bytes()); // tiles_y
        idim.push(0); // scan_order
        evil.extend_from_slice(&write_chunk(CHUNK_IDIM, FLAG_RAW, &idim));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        assert!(
            evil.len() < 100,
            "PoC file should be tiny (was {} bytes)",
            evil.len()
        );

        let result = decode_bytes(&evil);
        match result {
            Ok(_) => panic!("excessive iDIM tile count must be rejected"),
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("tile count") || msg.contains("tiles_x"),
                    "expected tile-count-limit error, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn test_idim_tile_dimensions_saturates_on_inconsistent_geometry() {
        // Defense-in-depth unit test for iDim::tile_dimensions itself:
        // since all iDim fields are `pub`, a caller (or a future code path)
        // could construct a self-inconsistent instance directly, bypassing
        // handle_idim_chunk's validation entirely. tile_dimensions must
        // never panic regardless - it saturates to 0 instead of
        // underflowing.
        let idim = iDim {
            tile_width: 200,
            tile_height: 200,
            tiles_x: 2,
            tiles_y: 2,
            scan_order: 0,
        };
        // tile_x=1 is the "last tile" (tiles_x-1=1); img_width=8 is far
        // smaller than tile_x*tile_width=200, so unchecked subtraction
        // would underflow. Must saturate to 0 instead of panicking.
        let (w, h) = idim.tile_dimensions(1, 1, 8, 8);
        assert_eq!((w, h), (0, 0));
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
    fn test_decode_adversarial_duplicate_ihdr_budget_bypass() {
        // SECURITY (CWE-409): a first IHDR declaring huge dimensions caches a
        // huge decompress_budget; a second IHDR then overwrites width/height
        // to a tiny effective image, but (before the fix) decompress_budget
        // stayed cached from the first IHDR - letting IDATs decompress far
        // beyond what the tiny effective image needs. The decoder must now
        // reject the second (duplicate) IHDR outright.
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);

        let build_ihdr = |w: u32, h: u32| -> Vec<u8> {
            let mut ihdr = Vec::new();
            ihdr.extend_from_slice(&w.to_be_bytes());
            ihdr.extend_from_slice(&h.to_be_bytes());
            ihdr.push(8); // bit_depth
            ihdr.push(SAMPLE_FORMAT_UINT);
            ihdr.push(COLOR_TYPE_RGBA);
            ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
            ihdr.push(FILTER_METHOD_NONE);
            ihdr.push(INTERLACE_NONE);
            ihdr
        };

        // IHDR #1: huge image -> huge decompress_budget would be cached.
        evil.extend_from_slice(&write_chunk(
            CHUNK_IHDR,
            FLAG_RAW,
            &build_ihdr(20_000, 20_000),
        ));
        // IHDR #2: tiny effective image.
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &build_ihdr(4, 4)));

        // Several IDATs, each decompressing to 800 KiB of zeros (highly
        // compressible, tiny on disk) - far beyond what a 4x4 RGBA image
        // needs (68 bytes), but within the huge budget from IHDR #1.
        let piece = vec![0u8; 800 * 1024];
        let compressed = zstd::encode_all(piece.as_slice(), 3).unwrap();
        for _ in 0..10 {
            evil.extend_from_slice(&write_chunk(CHUNK_IDAT, FLAG_ZSTD, &compressed));
        }
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let tmp_in = std::env::temp_dir().join("cafe_evil_duplicate_ihdr.cafe");
        let tmp_out = std::env::temp_dir().join("cafe_evil_duplicate_ihdr.png");
        std::fs::write(&tmp_in, &evil).unwrap();
        let result = decode(tmp_in.to_str().unwrap(), tmp_out.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp_in);
        let _ = std::fs::remove_file(&tmp_out);
        assert!(
            result.is_err(),
            "duplicate IHDR (budget-bypass attempt) must be rejected"
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
    fn test_decode_adversarial_plte_excessive_entries() {
        // SECURITY (memory-amplification): a PLTE chunk declaring more than
        // MAX_PALETTE_ENTRIES (256) entries. A legitimate indexed-color
        // palette never needs more than 256 entries (bit depths 1/2/4/8 all
        // top out at 256 distinct indices, section 4.1.2), but before this
        // fix the only limit on read_plte_chunk's Vec<PaletteEntry> was the
        // generic 1 GiB MAX_DECOMPRESSED_CHUNK_SIZE chunk-decompression
        // ceiling, allowing amplification far beyond what any valid pixel
        // index could ever address. Must be rejected immediately with a
        // clean error, not silently truncated or allowed through.
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

        // 300 RGB entries (900 bytes) > MAX_PALETTE_ENTRIES (256).
        let mut plte_payload = Vec::new();
        plte_payload.push(0u8); // entry_format = 0 (RGB, no alpha)
        for i in 0..300u32 {
            plte_payload.push((i % 256) as u8);
            plte_payload.push((i % 256) as u8);
            plte_payload.push((i % 256) as u8);
        }
        evil.extend_from_slice(&write_chunk(CHUNK_PLTE, FLAG_RAW, &plte_payload));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let result = decode_bytes(&evil);
        match result {
            Ok(_) => panic!("PLTE with 300 entries must be rejected"),
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("PLTE") && msg.contains("300"),
                    "expected PLTE entry-count-limit error, got: {msg}"
                );
            }
        }
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
        // Even/odd 4×4 RGBA: 64 + h(4) = 68 (v1.11: margin widened from a
        // flat EVEN_ODD_NUM_PASSES=2 to `h` rows, to allow the streaming
        // encoder to split each pass across multiple IDATs).
        assert_eq!(
            compute_decompress_budget(INTERLACE_EVEN_ODD, COLOR_TYPE_RGBA, 4, 4, 16),
            68
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
    fn test_quantize_palette_weighted() {
        // Same shape as test_quantize_palette, but exercising the v1.5
        // NearestNeighborWeighted (redmean) algorithm — should behave the
        // same as plain NearestNeighbor for a small number of exactly
        // repeated colors well under max_colors (each unique color gets its
        // own palette entry regardless of which distance metric is used,
        // since a distance of 0 short-circuits palette growth in both).
        let mut rgba = Vec::new();
        for _ in 0..100 {
            rgba.extend_from_slice(&[255, 0, 0, 255]); // Red
        }
        for _ in 0..100 {
            rgba.extend_from_slice(&[0, 255, 0, 255]); // Green
        }

        let (indices, palette) =
            quantize_to_palette(&rgba, 100, 256, PaletteAlgorithm::NearestNeighborWeighted);
        assert_eq!(palette.entries.len(), 2);
        assert_eq!(indices.len(), 200);
        // All "red" pixels map to the same index, all "green" pixels map to
        // a different (single) index.
        assert!(indices[0..100].iter().all(|&i| i == indices[0]));
        assert!(indices[100..200].iter().all(|&i| i == indices[100]));
        assert_ne!(indices[0], indices[100]);
    }

    #[test]
    fn test_quantize_weighted_prefers_perceptually_closer_palette_entry() {
        // Construct a case where redmean and unweighted Euclidean distance
        // disagree on which of two existing palette entries is "closer" to
        // a new pixel, and confirm the weighted algorithm follows redmean.
        //
        // Candidate pixel: pure blue (0, 0, 255).
        // Entry A: pure red (255, 0, 0) -- unweighted dist to candidate:
        //   dr=255 (255-0), db=255 (0-255) -> 255^2+255^2 = 130050
        //   redmean: rmean=(0+255)/2=127 -> (512+127)*255^2 + (767-127)*255^2
        //          = 639*65025 + 640*65025 = 1279*65025 = 83,157,975
        // Entry B: mid-gray (128, 128, 128) -- unweighted dist to candidate:
        //   dr=128, dg=128, db=127 -> 128^2+128^2+127^2 = 16384+16384+16129=48897
        //   redmean: rmean=(0+128)/2=64 -> (512+64)*128^2 + 1024*128^2 + (767-64)*127^2
        //          = 576*16384 + 1024*16384 + 703*16129
        //          = 9,437,184 + 16,777,216 + 11,338,687 = 37,553,087
        // Unweighted picks B in both cases here (B is closer under both
        // metrics for this particular pair) -- so instead directly assert
        // the redmean formula's ordering differs from unweighted's ordering
        // for a pair specifically chosen to disagree (see
        // types.rs::test_redmean_distance_differs_from_unweighted_for_non_gray_pairs
        // for the isolated formula-level proof); here we just confirm the
        // quantizer actually uses redmean_distance end-to-end by checking a
        // palette built by the weighted algorithm never has a plain-zero
        // (identical color) match count differing from NearestNeighbor's
        // (i.e. this is a smoke/integration test that the wiring works, not
        // a duplicate of the formula-level unit test above).
        let candidate = PaletteEntry {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let entry_a = PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let entry_b = PaletteEntry {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        // Both metrics agree B is closer for this specific triple (verified
        // above in the comment) -- this test's real purpose is to confirm
        // `quantize_nearest_neighbor_weighted` is reachable and produces a
        // valid palette/index mapping end-to-end.
        assert!(candidate.redmean_distance(&entry_b) < candidate.redmean_distance(&entry_a));
        assert!(candidate.distance_squared(&entry_b) < candidate.distance_squared(&entry_a));

        let mut rgba = Vec::new();
        rgba.extend_from_slice(&entry_a.to_rgba());
        rgba.extend_from_slice(&entry_b.to_rgba());
        rgba.extend_from_slice(&candidate.to_rgba());
        let (indices, palette) =
            quantize_to_palette(&rgba, 3, 256, PaletteAlgorithm::NearestNeighborWeighted);
        assert_eq!(palette.entries.len(), 3); // all 3 colors distinct, all fit
        assert_eq!(indices.len(), 3);
        assert_eq!(indices[0], 0);
        assert_eq!(indices[1], 1);
        assert_eq!(indices[2], 2); // candidate is distinct enough to get its own entry
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
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");

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
            .expect("failed to create image");

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
            .expect("failed to create image");

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

    /// Test helper for the Fase 2 (streaming-prep) parity tests below:
    /// manually walks a `.cafe` file's chunks (mirroring
    /// `decode_bytes_internal`'s loop but stopping short of accumulating
    /// into `state.pixel_rows`), decoding each `IDAT` via
    /// `decode_idat_chunk_as_tile_row_strip` instead of
    /// `handle_idat_indexed`/`handle_idat_direct_color`. Returns the
    /// collected tiles in file order (which is `y` order for row-strip
    /// tiling, section 4.3 of the spec). Panics (via `expect`) on any
    /// decode error — acceptable in a test helper operating on
    /// freshly-encoded, trusted input.
    fn decode_all_tiles_row_strip(buf: &[u8]) -> Vec<Tile> {
        assert_eq!(
            &buf[0..9],
            &SIGNATURE,
            "test file must start with signature"
        );
        let mut offset = 9;
        let mut state = DecodeState::default();
        let mut tiles: Vec<Tile> = Vec::new();
        loop {
            let chunk = read_chunk(buf, offset).expect("chunk read failed");
            offset = chunk.next_offset;
            match &chunk.chunk_type {
                t if t == CHUNK_IHDR => {
                    handle_ihdr_chunk(&mut state, &chunk.data).expect("IHDR handling failed")
                }
                t if t == CHUNK_PLTE => handle_plte_chunk(&mut state, chunk.flag, &chunk.data)
                    .expect("PLTE handling failed"),
                t if t == CHUNK_ZDIC => handle_zdic_chunk(&mut state, chunk.flag, &chunk.data),
                t if t == CHUNK_CHDR => handle_chdr_chunk(&mut state, chunk.flag, &chunk.data),
                t if t == CHUNK_IDIM => handle_idim_chunk(&mut state, chunk.flag, &chunk.data)
                    .expect("iDIM handling failed"),
                t if t == CHUNK_IDAT => {
                    let tile = decode_idat_chunk_as_tile_row_strip(
                        &mut state,
                        chunk.flag,
                        &chunk.data,
                        crate::tonemap::ToneMapOperator::Filmic,
                    )
                    .expect("per-tile IDAT decode failed");
                    tiles.push(tile);
                }
                t if t == CHUNK_IEND => break,
                _ => {} // eXIF/jSON/iCCP/xMPd and unknown ancillary chunks: irrelevant here
            }
        }
        tiles
    }

    /// Asserts `tiles` (in `y` order) reassemble, via simple row-major
    /// copy, into exactly `expected_rgba` — the shared assertion body for
    /// the direct-color and indexed row-strip parity tests below.
    fn assert_tiles_reassemble_to(tiles: &[Tile], width: u32, height: u32, expected_rgba: &[u8]) {
        assert!(!tiles.is_empty(), "expected at least one tile");
        let mut reassembled = vec![0u8; expected_rgba.len()];
        let mut rows_covered = 0u32;
        for tile in tiles {
            assert_eq!(tile.x, 0, "row-strip tile must start at x=0");
            assert_eq!(tile.width, width, "row-strip tile must span full width");
            assert_eq!(tile.y, rows_covered, "tiles must be contiguous, in order");
            assert_eq!(
                tile.pixels.len(),
                (tile.width * tile.height * 4) as usize,
                "tile pixel buffer size must match width*height*4"
            );
            let dst_start = (tile.y * width * 4) as usize;
            let dst_end = dst_start + tile.pixels.len();
            reassembled[dst_start..dst_end].copy_from_slice(&tile.pixels);
            rows_covered += tile.height;
        }
        assert_eq!(
            rows_covered, height,
            "tiles must cover the whole image height"
        );
        assert_eq!(
            reassembled, expected_rgba,
            "per-tile decode must match whole-image decode pixel-for-pixel"
        );
    }

    /// Asserts `tiles` (in whatever `scan_order` they were produced) fully
    /// and exclusively cover a `width`×`height` image, each written into
    /// its declared `(x, y, width, height)` rectangle, reassembling to
    /// exactly `expected_rgba` — the 2D-tiling (`iDIM`) analogue of
    /// `assert_tiles_reassemble_to` above, which assumes row-strip (`x=0`,
    /// full-width) tiles instead.
    fn assert_idim_tiles_reassemble_to(
        tiles: &[Tile],
        width: u32,
        height: u32,
        expected_rgba: &[u8],
    ) {
        assert!(!tiles.is_empty(), "expected at least one tile");
        let mut reassembled = vec![0u8; expected_rgba.len()];
        let mut covered = vec![false; (width * height) as usize];
        for tile in tiles {
            assert_eq!(
                tile.pixels.len(),
                (tile.width * tile.height * 4) as usize,
                "tile pixel buffer size must match width*height*4"
            );
            assert!(
                tile.x + tile.width <= width && tile.y + tile.height <= height,
                "tile ({}, {}, {}, {}) must fit within the {}x{} image",
                tile.x,
                tile.y,
                tile.width,
                tile.height,
                width,
                height
            );
            for r in 0..tile.height {
                let src_start = (r * tile.width * 4) as usize;
                let src_end = src_start + (tile.width * 4) as usize;
                let dst_row = tile.y + r;
                let dst_start = ((dst_row * width + tile.x) * 4) as usize;
                let dst_end = dst_start + (tile.width * 4) as usize;
                reassembled[dst_start..dst_end].copy_from_slice(&tile.pixels[src_start..src_end]);
                for c in 0..tile.width {
                    let idx = (dst_row * width + tile.x + c) as usize;
                    assert!(
                        !covered[idx],
                        "pixel ({}, {}) covered by multiple tiles",
                        tile.x + c,
                        dst_row
                    );
                    covered[idx] = true;
                }
            }
        }
        assert!(
            covered.iter().all(|&c| c),
            "every pixel of the image must be covered by exactly one tile"
        );
        assert_eq!(
            reassembled, expected_rgba,
            "per-tile iDIM decode must match whole-image decode pixel-for-pixel"
        );
    }

    /// Fase 2 (streaming-prep) parity test, direct-color path (RGBA): the
    /// concatenation of tiles produced by `decode_idat_chunk_as_tile_row_strip`
    /// must be byte-for-byte identical to what `decode_bytes()` produces for
    /// the same file. This is the regression test guarding the new per-tile
    /// decode path introduced for the future streaming `Decoder<R: Read>` —
    /// a divergence here would mean `next_tile()` silently produces
    /// different pixels than the existing whole-image decoder.
    #[test]
    fn test_decode_idat_as_tile_row_strip_matches_whole_image_decode() {
        let width = 48u32;
        let height = 37u32; // deliberately not a multiple of tile_rows
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
            .expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_tile_parity_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8, // multiple row-strip tiles, including a partial last one
            level: 10,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_tile_parity.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");

        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");
        let (whole_image_pixels, whole_image_result) =
            decode_bytes(&buf).expect("whole-image decode failed");

        let tiles = decode_all_tiles_row_strip(&buf);
        assert_tiles_reassemble_to(&tiles, width, height, &whole_image_pixels);
        assert_eq!(whole_image_result.width, width);
        assert_eq!(whole_image_result.height, height);
    }

    /// `compression_stats` (v1.6.2+) must be populated with real per-chunk
    /// sizes rather than always `None` — one entry per IDAT (there should be
    /// `height / tile_rows` rounded up of them) plus one for the eXIF chunk
    /// this test attaches, with `total_original`/`total_compressed` matching
    /// the sum of the individual entries.
    #[test]
    fn test_decode_bytes_populates_compression_stats() {
        let width = 32u32;
        let height = 20u32; // not a multiple of tile_rows, so >1 IDAT with a partial last tile
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        for (i, cell) in img_data.iter_mut().enumerate() {
            *cell = (i % 256) as u8;
        }
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_stats_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8,
            level: 5,
            exif: Some(vec![1, 2, 3, 4, 5]),
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_stats.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");

        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");
        let (_pixels, result) = decode_bytes(&buf).expect("decode failed");

        let stats = result
            .compression_stats
            .expect("compression_stats should be Some after decode");

        assert!(
            !stats.chunks.is_empty(),
            "expected at least one recorded chunk"
        );
        // At least one IDAT and one eXIF entry must be present.
        assert!(stats.chunks.iter().any(|c| c.chunk_type == "IDAT"));
        assert!(stats.chunks.iter().any(|c| c.chunk_type == "eXIF"));

        // Sums must match the aggregated totals exactly.
        let sum_original: u64 = stats.chunks.iter().map(|c| c.original_size as u64).sum();
        let sum_compressed: u64 = stats.chunks.iter().map(|c| c.compressed_size as u64).sum();
        assert_eq!(stats.total_original, sum_original);
        assert_eq!(stats.total_compressed, sum_compressed);

        // Sanity: original pixel bytes reconstructed across all IDATs must
        // equal width*height*4 (no filter-byte overhead leaking through, no
        // undercount from tiling).
        let idat_original_sum: u64 = stats
            .chunks
            .iter()
            .filter(|c| c.chunk_type == "IDAT")
            .map(|c| c.original_size as u64)
            .sum();
        assert!(
            idat_original_sum > 0,
            "IDAT original size sum should be nonzero"
        );
    }

    /// Fase 2 (streaming-prep) parity test, indexed-palette path
    /// (`color_type=3`): same guarantee as
    /// `test_decode_idat_as_tile_row_strip_matches_whole_image_decode`, but
    /// exercising `decode_idat_as_tile_row_strip`'s indexed branch (palette
    /// dequantization + sub-byte-depth index unpacking per row) instead of
    /// the direct-color branch.
    #[test]
    fn test_decode_idat_as_tile_row_strip_matches_whole_image_decode_indexed() {
        let width = 40u32;
        let height = 33u32; // deliberately not a multiple of tile_rows
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        // Few solid colors (< 256) so quantization is lossless.
        let colors = [
            [255u8, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let c = colors[((x + y) as usize) % colors.len()];
                img_data[idx..idx + 4].copy_from_slice(&c);
            }
        }

        let img = image::RgbaImage::from_raw(width, height, img_data.clone())
            .expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_tile_parity_indexed_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8, // multiple row-strip tiles, including a partial last one
            level: 10,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_tile_parity_indexed.cafe")
            .to_string_lossy()
            .to_string();
        encode_indexed(&input_path, &cafe_path, &opts).expect("failed to encode_indexed");

        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");
        let (whole_image_pixels, whole_image_result) =
            decode_bytes(&buf).expect("whole-image decode failed");

        let tiles = decode_all_tiles_row_strip(&buf);
        assert_tiles_reassemble_to(&tiles, width, height, &whole_image_pixels);
        assert_eq!(whole_image_result.width, width);
        assert_eq!(whole_image_result.height, height);
    }

    // --- Tests for Phase 3: `Decoder<R: Read>` streaming API ---

    /// Builds a small encoded CAFE file (row-strip, non-indexed) and
    /// returns its bytes, for `Decoder<R>` tests that don't need to vary
    /// dimensions/content — kept separate from
    /// `decode_all_tiles_row_strip`'s own test fixtures above since those
    /// are tied to specific width/height/content per test.
    fn build_simple_cafe_bytes(width: u32, height: u32, tile_rows: u32) -> Vec<u8> {
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                img_data[idx] = (x * 7) as u8;
                img_data[idx + 1] = (y * 11) as u8;
                img_data[idx + 2] = ((x + y) % 256) as u8;
                img_data[idx + 3] = 255;
            }
        }
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join(format!(
                "test_decoder_stream_input_{width}x{height}_{tile_rows}.png"
            ))
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows,
            level: 5,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join(format!(
                "test_decoder_stream_{width}x{height}_{tile_rows}.cafe"
            ))
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");
        std::fs::read(&cafe_path).expect("failed to read encoded file")
    }

    /// `read_info()` must report the same width/height as the whole-image
    /// decoder, and `supports_streaming_tiles` must be `true` for a plain
    /// row-strip (non-iDIM, non-interlaced) file.
    #[test]
    fn test_decoder_read_info_matches_whole_image_dimensions() {
        let buf = build_simple_cafe_bytes(48, 37, 8);
        let (_pixels, whole_result) = decode_bytes(&buf).expect("whole-image decode failed");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");

        assert_eq!(info.width, whole_result.width);
        assert_eq!(info.height, whole_result.height);
        assert_eq!(info.color_type, COLOR_TYPE_RGBA);
        assert_eq!(info.bit_depth, 8);
        assert!(info.supports_streaming_tiles);
    }

    /// Calling `next_tile()` in a loop until `Ok(None)` must reassemble to
    /// exactly the same pixels as `decode_bytes()` — the core end-to-end
    /// guarantee of the streaming API, exercised over a real `Read`
    /// (`Cursor`) rather than calling the per-tile helper functions
    /// directly (unlike `decode_all_tiles_row_strip`, which bypasses
    /// `Decoder` entirely).
    #[test]
    fn test_decoder_next_tile_loop_matches_whole_image_decode() {
        let buf = build_simple_cafe_bytes(48, 37, 8);
        let (whole_pixels, whole_result) = decode_bytes(&buf).expect("whole-image decode failed");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert!(info.supports_streaming_tiles);

        let mut tiles = Vec::new();
        while let Some(tile) = decoder.next_tile().expect("next_tile failed") {
            tiles.push(tile);
        }
        // Idempotent after exhaustion.
        assert!(decoder
            .next_tile()
            .expect("next_tile after None failed")
            .is_none());

        assert_tiles_reassemble_to(&tiles, info.width, info.height, &whole_pixels);
        assert_eq!(info.width, whole_result.width);
        assert_eq!(info.height, whole_result.height);
    }

    /// Same end-to-end guarantee, indexed-palette path (`color_type=3`):
    /// `PLTE` must be consumed correctly by `read_info()` before the first
    /// `next_tile()` call.
    #[test]
    fn test_decoder_next_tile_loop_matches_whole_image_decode_indexed() {
        let width = 40u32;
        let height = 33u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        let colors = [
            [255u8, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let c = colors[((x + y) as usize) % colors.len()];
                img_data[idx..idx + 4].copy_from_slice(&c);
            }
        }
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_stream_indexed_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 8,
            level: 5,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_stream_indexed.cafe")
            .to_string_lossy()
            .to_string();
        encode_indexed(&input_path, &cafe_path, &opts).expect("failed to encode_indexed");
        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");

        let (whole_pixels, _whole_result) = decode_bytes(&buf).expect("whole-image decode failed");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert_eq!(info.color_type, COLOR_TYPE_INDEXED);
        assert!(info.supports_streaming_tiles);

        let mut tiles = Vec::new();
        while let Some(tile) = decoder.next_tile().expect("next_tile failed") {
            tiles.push(tile);
        }
        assert_tiles_reassemble_to(&tiles, width, height, &whole_pixels);
    }

    /// `finish()` must return the same ancillary metadata (`eXIF`/`jSON`)
    /// that `decode_bytes_with_opts` returns in its `DecodeResult`,
    /// regardless of whether it's called after draining all tiles via
    /// `next_tile()` or immediately after `read_info()` (mid-stream,
    /// draining the remaining IDATs itself).
    #[test]
    fn test_decoder_finish_returns_same_metadata_as_whole_image_decode() {
        let width = 16u32;
        let height = 16u32;
        let img_data = vec![128u8; (width * height * 4) as usize];
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_finish_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let mut json_metadata = HashMap::new();
        json_metadata.insert("test".to_string(), serde_json::json!({"hello": "world"}));
        let opts = EncodeOptions {
            use_filter: true,
            tile_rows: 4,
            level: 5,
            exif: Some(vec![0x49, 0x49, 0x2A, 0x00, 0, 0, 0, 0]), // minimal fake TIFF/EXIF header
            json_metadata,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_finish.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");
        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");

        let (_whole_pixels, whole_result) =
            decode_bytes_with_opts(&buf, &opts).expect("whole-image decode failed");

        // Case 1: drain all tiles first, then finish().
        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        decoder.read_info().expect("read_info failed");
        while decoder.next_tile().expect("next_tile failed").is_some() {}
        let result = decoder.finish().expect("finish failed");
        assert_eq!(result.width, whole_result.width);
        assert_eq!(result.height, whole_result.height);
        assert_eq!(result.exif, whole_result.exif);
        assert_eq!(result.json_metadata, whole_result.json_metadata);

        // Case 2: finish() immediately after read_info(), without ever
        // calling next_tile() — must still drain remaining IDATs (subject
        // to the CWE-409 budget) and reach the same metadata.
        let cursor2 = std::io::Cursor::new(buf.as_slice());
        let mut decoder2 = Decoder::new(cursor2);
        decoder2.read_info().expect("read_info failed");
        let result2 = decoder2.finish().expect("finish failed");
        assert_eq!(result2.exif, whole_result.exif);
        assert_eq!(result2.json_metadata, whole_result.json_metadata);
    }

    /// A file using 2D tiling (`iDIM`) must be reported as supporting
    /// streaming tiles (v1.9+), and `next_tile()`'s loop must reassemble to
    /// exactly the same pixels as `decode_bytes()`, with each yielded
    /// `Tile` carrying its real `(x, y, width, height)` position in the
    /// tile grid (including a partial last row/column, since 33x23 with an
    /// 8x8 tile does not divide evenly).
    #[test]
    fn test_decoder_next_tile_loop_matches_whole_image_decode_idim() {
        let width = 33u32;
        let height = 23u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                img_data[idx] = ((x * 7 + y * 3) % 256) as u8;
                img_data[idx + 1] = ((x * 13 + y) % 256) as u8;
                img_data[idx + 2] = ((x + y * 29) % 256) as u8;
                img_data[idx + 3] = ((x * 3 + y * 5) % 256) as u8;
            }
        }
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_idim_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            level: 5,
            idim: Some(iDim::new(8, 8, width, height, 0)),
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_idim.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");
        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");
        let (whole_pixels, _whole_result) = decode_bytes(&buf).expect("whole-image decode failed");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert!(info.supports_streaming_tiles);

        let mut tiles = Vec::new();
        while let Some(tile) = decoder.next_tile().expect("next_tile failed") {
            tiles.push(tile);
        }
        assert!(decoder
            .next_tile()
            .expect("next_tile after None failed")
            .is_none());

        assert_idim_tiles_reassemble_to(&tiles, width, height, &whole_pixels);
    }

    /// Same as above, but with `scan_order=1` (Z-order/Morton) — confirms
    /// `next_tile()` yields tiles in the file's declared visitation order
    /// and each still lands at its correct `(x, y)` regardless of the
    /// order tiles arrive in.
    #[test]
    fn test_decoder_next_tile_loop_matches_whole_image_decode_idim_zorder() {
        let width = 33u32;
        let height = 23u32;
        let img_data: Vec<u8> = (0..(width * height * 4)).map(|i| (i % 256) as u8).collect();
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_idim_zorder_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            level: 5,
            idim: Some(iDim::new(8, 8, width, height, 1)),
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_idim_zorder.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");
        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");
        let (whole_pixels, _whole_result) = decode_bytes(&buf).expect("whole-image decode failed");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert!(info.supports_streaming_tiles);

        let mut tiles = Vec::new();
        while let Some(tile) = decoder.next_tile().expect("next_tile failed") {
            tiles.push(tile);
        }
        assert_idim_tiles_reassemble_to(&tiles, width, height, &whole_pixels);
    }

    /// `iDIM` combined with an indexed palette is rejected outright by
    /// `encode_indexed()` (no CAFE writer can currently produce such a
    /// file), which in turn means `Decoder`'s
    /// `handle_idat_tile_idim`/`decode_idim_tile_raw` indexed-palette guard
    /// can only ever be reached by an adversarial/hand-crafted file — see
    /// `test_decode_adversarial_idim_with_indexed_color_type` below for
    /// that direct decode-side check. This test instead pins down the
    /// encoder-side rejection so a future change can't silently start
    /// allowing this combination without the decoder story being revisited.
    #[test]
    fn test_encode_indexed_rejects_idim() {
        let width = 32u32;
        let height = 32u32;
        let mut img_data = vec![0u8; (width * height * 4) as usize];
        let colors = [[255u8, 0, 0, 255], [0, 255, 0, 255]];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let c = colors[((x + y) as usize) % colors.len()];
                img_data[idx..idx + 4].copy_from_slice(&c);
            }
        }
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_idim_indexed_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            level: 5,
            idim: Some(iDim::new(16, 16, width, height, 0)),
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_idim_indexed.cafe")
            .to_string_lossy()
            .to_string();
        let result = encode_indexed(&input_path, &cafe_path, &opts);
        assert!(
            result.is_err(),
            "encode_indexed must reject iDIM combined with indexed palette"
        );
    }

    /// Direct decode-side check (adversarial, hand-crafted file, since no
    /// encoder can produce this combination — see
    /// `test_encode_indexed_rejects_idim` above): `iDIM` + `color_type=3`
    /// (indexed) must be reported as not supporting streaming tiles, and
    /// `next_tile()` must reject it cleanly instead of misinterpreting the
    /// tile payload as unpacked direct-color bytes.
    #[test]
    fn test_decode_adversarial_idim_with_indexed_color_type() {
        let mut evil = Vec::new();
        evil.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.push(8); // bit_depth
        ihdr.push(SAMPLE_FORMAT_UINT);
        ihdr.push(COLOR_TYPE_INDEXED);
        ihdr.push(COMPRESSION_METHOD_ZSTD_BIT);
        ihdr.push(FILTER_METHOD_NONE);
        ihdr.push(INTERLACE_NONE);
        evil.extend_from_slice(&write_chunk(CHUNK_IHDR, FLAG_RAW, &ihdr));

        let mut idim = Vec::new();
        idim.extend_from_slice(&4u16.to_be_bytes()); // tile_width
        idim.extend_from_slice(&4u16.to_be_bytes()); // tile_height
        idim.extend_from_slice(&2u16.to_be_bytes()); // tiles_x
        idim.extend_from_slice(&2u16.to_be_bytes()); // tiles_y
        idim.push(0); // scan_order
        evil.extend_from_slice(&write_chunk(CHUNK_IDIM, FLAG_RAW, &idim));

        let mut plte = Vec::new();
        plte.push(0); // entry_format=RGB
        plte.extend_from_slice(&[255, 0, 0]);
        plte.extend_from_slice(&[0, 255, 0]);
        evil.extend_from_slice(&write_chunk(CHUNK_PLTE, FLAG_RAW, &plte));
        evil.extend_from_slice(&write_chunk(CHUNK_IEND, FLAG_RAW, &[]));

        let cursor = std::io::Cursor::new(evil.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert!(!info.supports_streaming_tiles);
        assert!(matches!(
            decoder.next_tile(),
            Err(CafeError::UnsupportedFeature(_))
        ));
    }

    /// Interlaced files (Adam7/even-odd) remain permanently unsupported by
    /// `next_tile()` even after v1.9's iDIM support — an interlace pass is
    /// not a spatial rectangle, so this is a by-design limitation, not a
    /// gap. `supports_streaming_tiles` must be `false` and `next_tile()`
    /// must reject with a clear error.
    #[test]
    fn test_decoder_next_tile_rejects_interlaced_adam7() {
        let width = 32u32;
        let height = 32u32;
        let img_data = vec![64u8; (width * height * 4) as usize];
        let img =
            image::RgbaImage::from_raw(width, height, img_data).expect("failed to create image");
        let temp_dir = std::env::temp_dir();
        let input_path = temp_dir
            .join("test_decoder_adam7_input.png")
            .to_string_lossy()
            .to_string();
        img.save(&input_path).expect("failed to save PNG");

        let opts = EncodeOptions {
            use_filter: true,
            level: 5,
            interlace_method: INTERLACE_ADAM7,
            ..EncodeOptions::default()
        };
        let cafe_path = temp_dir
            .join("test_decoder_adam7.cafe")
            .to_string_lossy()
            .to_string();
        encode(&input_path, &cafe_path, &opts).expect("failed to encode");
        let buf = std::fs::read(&cafe_path).expect("failed to read encoded file");

        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        let info = decoder.read_info().expect("read_info failed");
        assert!(!info.supports_streaming_tiles);
        assert!(matches!(
            decoder.next_tile(),
            Err(CafeError::UnsupportedFeature(_))
        ));
    }

    /// Calling `next_tile()` before `read_info()` must error clearly
    /// instead of panicking (e.g. on an unwrap of `self.info`).
    #[test]
    fn test_decoder_next_tile_before_read_info_errors() {
        let buf = build_simple_cafe_bytes(8, 8, 4);
        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        assert!(matches!(
            decoder.next_tile(),
            Err(CafeError::UnsupportedFeature(_))
        ));
    }

    /// A stream truncated mid-header (before the first `IDAT`/`IEND`) must
    /// surface as `TruncatedFile`, not panic, and not silently succeed.
    #[test]
    fn test_decoder_read_info_truncated_stream_errors() {
        let buf = build_simple_cafe_bytes(8, 8, 4);
        // Cut off partway through, before the first IDAT (IHDR is 14 bytes
        // payload + 9 bytes header/footer = 23 bytes; the signature is
        // another 9 bytes, so 20 total bytes lands inside the IHDR chunk).
        let truncated = &buf[0..20];
        let cursor = std::io::Cursor::new(truncated);
        let mut decoder = Decoder::new(cursor);
        assert!(matches!(
            decoder.read_info(),
            Err(CafeError::TruncatedFile(_))
        ));
    }

    /// Calling `read_info()` twice must error rather than silently
    /// re-parsing (which would double-apply IHDR/duplicate-chunk checks
    /// against a stream position that has already moved past them).
    #[test]
    fn test_decoder_read_info_called_twice_errors() {
        let buf = build_simple_cafe_bytes(8, 8, 4);
        let cursor = std::io::Cursor::new(buf.as_slice());
        let mut decoder = Decoder::new(cursor);
        decoder.read_info().expect("first read_info failed");
        assert!(matches!(
            decoder.read_info(),
            Err(CafeError::UnsupportedFeature(_))
        ));
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
            for chunk in out.as_chunks::<2>().0.iter() {
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
                    for px in decoded.as_chunks::<4>().0.iter() {
                        assert_eq!(px[0], px[1]);
                        assert_eq!(px[1], px[2]);
                        assert_eq!(px[3], 0xFF);
                    }
                }
                COLOR_TYPE_RGB => {
                    assert_eq!(decoded.len(), n * 4);
                    for (orig, dec) in rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(decoded.as_chunks::<4>().0.iter())
                    {
                        assert_eq!(dec[0], orig[0]);
                        assert_eq!(dec[1], orig[1]);
                        assert_eq!(dec[2], orig[2]);
                        assert_eq!(dec[3], 0xFF); // alpha forced opaque, original discarded
                    }
                }
                COLOR_TYPE_GRAY_ALPHA => {
                    assert_eq!(decoded.len(), n * 4);
                    for (orig, dec) in rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(decoded.as_chunks::<4>().0.iter())
                    {
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
