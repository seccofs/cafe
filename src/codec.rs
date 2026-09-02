//! Compression and decompression (ZSTD with fallback to raw)
//!
//! Section 3.2 of the spec: automatic fallback between ZSTD and raw,
//! with support for ZSTD dictionaries.

use crate::constants::*;
use crate::error::{CafeError, Result};
use std::io::Read;

/// Maximum number of bytes that the decompression of a single chunk can
/// produce. Protection against "decompression bomb" (CWE-409): without this
/// limit, a chunk of a few KB could expand to gigabytes and
/// exhaust all the memory available to the process. 1 GiB is generous for
/// realistic images (even at very high resolution), but finite.
const MAX_DECOMPRESSED_CHUNK_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB

/// Reads from `reader` to the end, but never more than `limit` bytes. If the
/// stream has more data beyond the limit, returns an error instead of
/// continuing to allocate memory indefinitely.
fn read_to_end_limited<R: std::io::Read>(reader: R, limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // Asks for 1 byte more than the limit: if we receive exactly limit+1,
    // we know there was more data than allowed.
    reader
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(CafeError::Zstd)?;
    if out.len() as u64 > limit {
        return Err(CafeError::DecompressionLimitExceeded { limit });
    }
    Ok(out)
}

/// Fallback rule from section 3.2: compresses and only keeps the result if
/// it is smaller than the original; otherwise it writes raw.
pub(crate) fn compress_with_fallback(raw: &[u8], level: i32) -> Result<(u8, Vec<u8>)> {
    let compressed = zstd::encode_all(raw, level).map_err(CafeError::Zstd)?;
    if compressed.len() < raw.len() {
        Ok((FLAG_ZSTD, compressed))
    } else {
        Ok((FLAG_RAW, raw.to_vec()))
    }
}

/// Decompresses a chunk using the streaming API, respecting
/// `MAX_DECOMPRESSED_CHUNK_SIZE` (protection against decompression bomb).
/// Does not rely on the content size in the frame header — for the same
/// reason as the interoperability fix documented in section 3.2 of the spec.
pub(crate) fn decompress_chunk(flag: u8, data: &[u8]) -> Result<Vec<u8>> {
    decompress_with_limit(flag, data, None, MAX_DECOMPRESSED_CHUNK_SIZE)
}

/// Same as `compress_with_fallback`, but using a ZSTD dictionary when
/// provided (section 4.9, zDIC). Scope: used only for IDAT chunks — the
/// dictionary does not apply to other chunks (eXIF, jSON, iCCP, xMPd).
///
/// **Dictionary fallback guarantee (v1.5):** a ZSTD dictionary trained from a
/// handful of sample tiles (see `train_zstd_dictionary` in `cafe.rs`) can
/// backfire badly on small/highly-redundant payloads — the dictionary-mode
/// ZSTD frame carries extra framing overhead that a plain (non-dictionary)
/// frame doesn't, and this overhead can outweigh any actual gain from
/// dictionary matches (measured up to ~78% *larger* output on synthetic
/// checkerboard/repetitive content during the v1.4.2 compression audit).
/// To make `dict` strictly non-regressive, this function always also tries
/// compressing without the dictionary and keeps whichever of
/// `{raw, zstd-no-dict, zstd-with-dict}` is smallest. Decoding a
/// no-dictionary frame with a dictionary-configured decoder (or vice versa)
/// is safe — ZSTD frame headers self-describe whether a dictionary was used,
/// so `decompress_chunk_dict_limited` does not need to know in advance
/// whether a given IDAT actually used the dictionary.
///
/// Returns `(flag, compressed_bytes, used_dict)` — `used_dict` tells the
/// caller whether the dictionary was the winning candidate for this chunk,
/// so callers (see `append_zdic_chunk_if_present` call sites in `cafe.rs`)
/// can skip emitting the `zDIC` chunk entirely when no IDAT ends up using it.
pub(crate) fn compress_with_fallback_dict(
    raw: &[u8],
    level: i32,
    dict: Option<&[u8]>,
) -> Result<(u8, Vec<u8>, bool)> {
    let no_dict_compressed = zstd::encode_all(raw, level).map_err(CafeError::Zstd)?;

    let (best_compressed, used_dict) = match dict {
        Some(d) => {
            let mut compressor =
                zstd::bulk::Compressor::with_dictionary(level, d).map_err(CafeError::Zstd)?;
            let dict_compressed = compressor.compress(raw).map_err(CafeError::Zstd)?;
            if dict_compressed.len() < no_dict_compressed.len() {
                (dict_compressed, true)
            } else {
                (no_dict_compressed, false)
            }
        }
        None => (no_dict_compressed, false),
    };

    if best_compressed.len() < raw.len() {
        Ok((FLAG_ZSTD, best_compressed, used_dict))
    } else {
        Ok((FLAG_RAW, raw.to_vec(), false))
    }
}

/// Decompresses a chunk respecting a maximum output limit `limit`
/// (still bounded by `MAX_DECOMPRESSED_CHUNK_SIZE`). Used for IDATs,
/// where the limit is derived from the size expected by the IHDR: each IDAT
/// can only expand to what the image still needs, preventing multiple
/// IDATs from accumulating gigabytes even when the image is small (CWE-409,
/// "cumulative decompression bomb").
pub(crate) fn decompress_chunk_dict_limited(
    flag: u8,
    data: &[u8],
    dict: Option<&[u8]>,
    limit: u64,
) -> Result<Vec<u8>> {
    decompress_with_limit(flag, data, dict, limit)
}

/// Decompression core: `limit` is the cap on allowed output bytes,
/// but it never exceeds `MAX_DECOMPRESSED_CHUNK_SIZE`.
fn decompress_with_limit(
    flag: u8,
    data: &[u8],
    dict: Option<&[u8]>,
    limit: u64,
) -> Result<Vec<u8>> {
    let limit = limit.min(MAX_DECOMPRESSED_CHUNK_SIZE);
    match flag {
        FLAG_RAW => {
            if data.len() as u64 > limit {
                return Err(CafeError::DecompressionLimitExceeded { limit });
            }
            Ok(data.to_vec())
        }
        FLAG_ZSTD => match dict {
            Some(d) => {
                let decoder =
                    zstd::stream::read::Decoder::with_dictionary(std::io::Cursor::new(data), d)
                        .map_err(CafeError::Zstd)?;
                read_to_end_limited(decoder, limit)
            }
            None => {
                let decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(data))
                    .map_err(CafeError::Zstd)?;
                read_to_end_limited(decoder, limit)
            }
        },
        other => Err(CafeError::UnsupportedFeature(format!(
            "Codec flag {other:#04x}"
        ))),
    }
}
