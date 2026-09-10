//! One-tile pixel <-> `IDAT` payload conversion (spec section 4.3): reverses
//! the per-row predictor prefix to reconstruct raw pixel bytes, or applies
//! it to produce an `IDAT` payload (encoder direction, Phase 6+).
//!
//! A "tile" here is exactly `iDIM`'s unit (spec section 4.2) — for Phase 5,
//! `cafe-codec` only decodes the single-tile case (`iDIM` absent, or
//! declaring exactly one tile the size of the whole image), so every
//! decoded image today is one tile. Multi-tile assembly is Phase 7.

use crate::error::{CodecError, Result};
use crate::predictor::{choose_best_row_predictor, filter_row, unfilter_row};

/// Reverses the per-row predictor over one `IDAT` payload (spec section
/// 4.3: `for each row: [predictor code: 1 byte][filtered row: bytes_per_row
/// bytes]`), reconstructing `tile_height * bytes_per_row` raw pixel bytes.
///
/// `bpp` is spec section 4.3.1's bytes-per-pixel (`bytes_per_sample *
/// channels`), used only to locate each byte's left/up-left neighbors —
/// prediction always operates on raw bytes, regardless of sample width.
pub fn decode_tile_rows(
    payload: &[u8],
    tile_height: u32,
    bytes_per_row: u32,
    bpp: u32,
) -> Result<Vec<u8>> {
    let tile_height = tile_height as usize;
    let bytes_per_row = bytes_per_row as usize;
    let bpp = bpp as usize;

    let mut out = Vec::with_capacity(tile_height * bytes_per_row);
    let mut offset = 0usize;

    for row_idx in 0..tile_height {
        if offset >= payload.len() {
            return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
                format!(
                    "IDAT payload truncated: expected predictor byte for row {row_idx} \
                     of {tile_height}, but only {} bytes remain",
                    payload.len() - offset
                ),
            )));
        }
        let code = payload[offset];
        offset += 1;

        let row_end = offset.checked_add(bytes_per_row).ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "IDAT payload: overflow computing row end offset".into(),
            ))
        })?;
        if row_end > payload.len() {
            return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
                format!(
                    "IDAT payload truncated: row {row_idx} needs {bytes_per_row} bytes, \
                     only {} available",
                    payload.len() - offset
                ),
            )));
        }
        let filtered = &payload[offset..row_end];
        offset = row_end;

        let prev_row: Option<&[u8]> = if row_idx == 0 {
            None
        } else {
            Some(&out[out.len() - bytes_per_row..])
        };

        let restored = unfilter_row(filtered, prev_row, code, bpp)?;
        out.extend_from_slice(&restored);
    }

    Ok(out)
}

/// Encoder-direction counterpart to [`decode_tile_rows`] (Phase 6+): applies
/// predictor `code` to every row of `raw` uniformly, producing one `IDAT`
/// payload. Exposed now so `predictor`/`tile` round-trip tests don't need
/// to duplicate row-splitting logic; per-row filter *selection* heuristics
/// (choosing a different code per row) are an encoder concern, not
/// implemented until Phase 6.
pub fn encode_tile_rows(
    raw: &[u8],
    tile_height: u32,
    bytes_per_row: u32,
    bpp: u32,
    code: u8,
) -> Result<Vec<u8>> {
    let tile_height = tile_height as usize;
    let bytes_per_row = bytes_per_row as usize;
    let bpp = bpp as usize;

    let expected_len = tile_height.checked_mul(bytes_per_row).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "encode_tile_rows: overflow in tile_height * bytes_per_row".into(),
        ))
    })?;
    if raw.len() != expected_len {
        return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
            format!(
                "encode_tile_rows: expected {expected_len} bytes (tile_height={tile_height} * \
                 bytes_per_row={bytes_per_row}), got {}",
                raw.len()
            ),
        )));
    }

    let mut out = Vec::with_capacity(tile_height * (bytes_per_row + 1));
    let mut prev_row: Option<&[u8]> = None;
    for row_idx in 0..tile_height {
        let row = &raw[row_idx * bytes_per_row..(row_idx + 1) * bytes_per_row];
        let filtered = filter_row(row, prev_row, code, bpp)?;
        out.push(code);
        out.extend_from_slice(&filtered);
        prev_row = Some(row);
    }
    Ok(out)
}

/// Encoder-direction counterpart to [`decode_tile_rows`], choosing a
/// predictor code independently for each row via
/// [`choose_best_row_predictor`] (spec section 4.3.1: predictor selection
/// is a per-row, encoder-only decision) instead of applying one fixed code
/// to the whole tile. This is what [`crate::encoder`] uses; the
/// fixed-code [`encode_tile_rows`] remains for tests and callers that want
/// to force a specific predictor.
pub fn encode_tile_rows_auto(
    raw: &[u8],
    tile_height: u32,
    bytes_per_row: u32,
    bpp: u32,
) -> Result<Vec<u8>> {
    let tile_height = tile_height as usize;
    let bytes_per_row = bytes_per_row as usize;
    let bpp = bpp as usize;

    let expected_len = tile_height.checked_mul(bytes_per_row).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "encode_tile_rows_auto: overflow in tile_height * bytes_per_row".into(),
        ))
    })?;
    if raw.len() != expected_len {
        return Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(
            format!(
                "encode_tile_rows_auto: expected {expected_len} bytes (tile_height={tile_height} \
                 * bytes_per_row={bytes_per_row}), got {}",
                raw.len()
            ),
        )));
    }

    let mut out = Vec::with_capacity(tile_height * (bytes_per_row + 1));
    let mut prev_row: Option<&[u8]> = None;
    for row_idx in 0..tile_height {
        let row = &raw[row_idx * bytes_per_row..(row_idx + 1) * bytes_per_row];
        let (code, filtered) = choose_best_row_predictor(row, prev_row, bpp);
        out.push(code);
        out.extend_from_slice(&filtered);
        prev_row = Some(row);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predictor::{PREDICTOR_NONE, PREDICTOR_PAETH, PREDICTOR_SUB, PREDICTOR_UP};

    #[test]
    fn test_encode_then_decode_roundtrip_none() {
        let raw: Vec<u8> = (0u8..24).collect(); // 4 rows x 6 bytes
        let payload = encode_tile_rows(&raw, 4, 6, 3, PREDICTOR_NONE).unwrap();
        let restored = decode_tile_rows(&payload, 4, 6, 3).unwrap();
        assert_eq!(restored, raw);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_sub() {
        let raw: Vec<u8> = vec![
            10, 20, 30, 40, 50, 60, // row 0
            15, 25, 35, 45, 55, 65, // row 1
            5, 250, 3, 253, 1, 255, // row 2
        ];
        let payload = encode_tile_rows(&raw, 3, 6, 2, PREDICTOR_SUB).unwrap();
        let restored = decode_tile_rows(&payload, 3, 6, 2).unwrap();
        assert_eq!(restored, raw);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_up() {
        let raw: Vec<u8> = (0..20).map(|i| (i * 7) as u8).collect();
        let payload = encode_tile_rows(&raw, 5, 4, 4, PREDICTOR_UP).unwrap();
        let restored = decode_tile_rows(&payload, 5, 4, 4).unwrap();
        assert_eq!(restored, raw);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_paeth_multirow() {
        let raw: Vec<u8> = (0..64).map(|i| ((i * 37) % 251) as u8).collect(); // 8 rows x 8 bytes
        let payload = encode_tile_rows(&raw, 8, 8, 4, PREDICTOR_PAETH).unwrap();
        let restored = decode_tile_rows(&payload, 8, 8, 4).unwrap();
        assert_eq!(restored, raw);
    }

    #[test]
    fn test_decode_single_row_no_prev() {
        // predictor=None, one row of 3 bytes.
        let payload = vec![PREDICTOR_NONE, 1, 2, 3];
        let restored = decode_tile_rows(&payload, 1, 3, 1).unwrap();
        assert_eq!(restored, vec![1, 2, 3]);
    }

    #[test]
    fn test_decode_truncated_missing_predictor_byte() {
        let payload: Vec<u8> = vec![]; // no predictor byte at all
        let result = decode_tile_rows(&payload, 1, 3, 1);
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_truncated_row_data() {
        let payload = vec![PREDICTOR_NONE, 1, 2]; // needs 3 bytes, only 2 present
        let result = decode_tile_rows(&payload, 1, 3, 1);
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_decode_rejects_invalid_predictor_code() {
        let payload = vec![9, 1, 2, 3]; // code 9 is not one of the 6 defined
        let result = decode_tile_rows(&payload, 1, 3, 1);
        assert!(matches!(result, Err(CodecError::InvalidPredictorCode(9))));
    }

    #[test]
    fn test_encode_rejects_wrong_length_input() {
        let raw = vec![1, 2, 3]; // 3 bytes, but 2 rows x 2 bytes = 4 expected
        let result = encode_tile_rows(&raw, 2, 2, 1, PREDICTOR_NONE);
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_encode_auto_then_decode_roundtrip() {
        let raw: Vec<u8> = (0..64).map(|i| ((i * 37) % 251) as u8).collect(); // 8 rows x 8 bytes
        let payload = encode_tile_rows_auto(&raw, 8, 8, 4).unwrap();
        let restored = decode_tile_rows(&payload, 8, 8, 4).unwrap();
        assert_eq!(restored, raw);
    }

    #[test]
    fn test_encode_auto_rejects_wrong_length_input() {
        let raw = vec![1, 2, 3];
        let result = encode_tile_rows_auto(&raw, 2, 2, 1);
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::TruncatedFile(_)))
        ));
    }

    #[test]
    fn test_encode_auto_picks_per_row_predictors_independently() {
        // Row 0 is a ramp (Sub wins); row 1 exactly matches row 0 (Up wins
        // with zero residual, beating everything else). Confirms
        // choose_best_row_predictor is invoked fresh per row rather than
        // reusing row 0's winner for every subsequent row.
        let row0: Vec<u8> = (0u8..8).map(|i| i * 5).collect();
        let row1 = row0.clone();
        let mut raw = row0.clone();
        raw.extend_from_slice(&row1);
        let payload = encode_tile_rows_auto(&raw, 2, 8, 1).unwrap();
        assert_eq!(payload[0], PREDICTOR_SUB);
        assert_eq!(payload[9], crate::predictor::PREDICTOR_UP);
        let restored = decode_tile_rows(&payload, 2, 8, 1).unwrap();
        assert_eq!(restored, raw);
    }
}
