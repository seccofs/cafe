//! `PLTE` chunk (spec section 4.3): critical, optional — an indexed-color
//! lookup table that changes `IDAT`'s effective `bpp` to 1 (one palette
//! index per pixel) without ever introducing a new `IHDR.color_type`
//! value (unlike PNG's `PLTE`).
//!
//! `Plte` (standard Rust casing) covers what `cafe-format` needs: framing
//! and structural validation of the chunk itself (`entry_count` bounds,
//! `IHDR` color_type/bit_depth compatibility). Reconstructing full pixels
//! from decoded indices — and rejecting out-of-range indices found inside
//! an `IDAT` — is `cafe-codec`'s concern (mirrors `idim.rs`'s split with
//! `cafe_codec::tiling`).

use crate::chunk::write_chunk;
use crate::constants::{plte_bytes_per_entry, MAX_PALETTE_ENTRIES, PLTE_ENTRY_COUNT_LEN};
use crate::error::{CafeError, Result};

/// A parsed `PLTE` payload (spec section 4.3): a flat list of color
/// entries, each `bytes_per_entry` bytes (3 for RGB, 4 for RGBA, per the
/// owning `IHDR.color_type`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plte {
    /// Bytes per entry this palette was built for (3 or 4) — needed to
    /// interpret `entries` and to serialize/validate against `IHDR`.
    pub bytes_per_entry: u8,
    /// Flat entry bytes, `entries.len() / bytes_per_entry` colors, in
    /// index order (`entries[0]` is index `0`, etc.).
    pub entries: Vec<u8>,
}

impl Plte {
    /// Number of palette entries (`entries.len() / bytes_per_entry`).
    pub fn entry_count(&self) -> usize {
        if self.bytes_per_entry == 0 {
            return 0;
        }
        self.entries.len() / self.bytes_per_entry as usize
    }

    /// Builds a `Plte` from `entry_count` flat color entries, inferring
    /// `bytes_per_entry` from `color_type` (spec section 4.3). Returns
    /// `InvalidPlte` if `color_type` isn't RGB/RGBA or `colors.len()` isn't
    /// an exact multiple of that color type's entry size.
    pub fn from_colors(color_type: u8, colors: &[u8]) -> Result<Self> {
        let bytes_per_entry = plte_bytes_per_entry(color_type).ok_or_else(|| {
            CafeError::InvalidPlte(format!(
                "PLTE is undefined for color_type={color_type} (only RGB=2/RGBA=6 are valid)"
            ))
        })?;
        if !colors.len().is_multiple_of(bytes_per_entry as usize) {
            return Err(CafeError::InvalidPlte(format!(
                "colors length {} is not a multiple of {bytes_per_entry} bytes/entry \
                 for color_type={color_type}",
                colors.len()
            )));
        }
        Ok(Plte {
            bytes_per_entry,
            entries: colors.to_vec(),
        })
    }

    /// Validates this `PLTE` against the owning `IHDR`'s `color_type`/
    /// `bit_depth` (spec section 4.3): `entry_count` in `1..=
    /// MAX_PALETTE_ENTRIES`, `color_type` must be RGB or RGBA (`PLTE` is
    /// undefined for gray/gray+alpha), `bit_depth` must be exactly `8`,
    /// and this palette's own `bytes_per_entry` must match what
    /// `color_type` requires.
    pub fn validate(&self, ihdr_color_type: u8, ihdr_bit_depth: u8) -> Result<()> {
        let expected_bpe = plte_bytes_per_entry(ihdr_color_type).ok_or_else(|| {
            CafeError::InvalidPlte(format!(
                "PLTE is undefined for IHDR.color_type={ihdr_color_type} \
                 (only RGB=2/RGBA=6 are valid)"
            ))
        })?;
        if ihdr_bit_depth != 8 {
            return Err(CafeError::InvalidPlte(format!(
                "PLTE requires IHDR.bit_depth=8, found {ihdr_bit_depth}"
            )));
        }
        if self.bytes_per_entry != expected_bpe {
            return Err(CafeError::InvalidPlte(format!(
                "PLTE entry size {} bytes doesn't match IHDR.color_type={ihdr_color_type}'s \
                 expected {expected_bpe} bytes/entry",
                self.bytes_per_entry
            )));
        }
        let count = self.entry_count();
        if count == 0 {
            return Err(CafeError::InvalidPlte(
                "PLTE must declare at least 1 entry".into(),
            ));
        }
        if count as u32 > MAX_PALETTE_ENTRIES {
            return Err(CafeError::InvalidPlte(format!(
                "PLTE entry_count={count} exceeds maximum allowed ({MAX_PALETTE_ENTRIES})"
            )));
        }
        Ok(())
    }

    /// Serializes to the `PLTE` payload (spec section 4.3): a 2-byte
    /// big-endian `entry_count` followed by the flat entry bytes.
    pub fn to_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(PLTE_ENTRY_COUNT_LEN + self.entries.len());
        buf.extend_from_slice(&(self.entry_count() as u16).to_be_bytes());
        buf.extend_from_slice(&self.entries);
        buf
    }

    /// Parses (but does not [`validate`](Plte::validate)) a `PLTE`
    /// payload, given the owning `IHDR.color_type` to determine
    /// `bytes_per_entry`. Returns `TruncatedFile` if the payload is
    /// shorter than its own declared `entry_count` requires, or
    /// `InvalidPlte` if `color_type` isn't RGB/RGBA.
    pub fn from_payload(payload: &[u8], ihdr_color_type: u8) -> Result<Self> {
        let bytes_per_entry = plte_bytes_per_entry(ihdr_color_type).ok_or_else(|| {
            CafeError::InvalidPlte(format!(
                "PLTE is undefined for IHDR.color_type={ihdr_color_type} \
                 (only RGB=2/RGBA=6 are valid)"
            ))
        })?;
        if payload.len() < PLTE_ENTRY_COUNT_LEN {
            return Err(CafeError::TruncatedFile(format!(
                "PLTE payload must be at least {PLTE_ENTRY_COUNT_LEN} bytes (entry_count), got {}",
                payload.len()
            )));
        }
        let entry_count = u16::from_be_bytes(payload[0..2].try_into().unwrap()) as usize;
        let expected_len = PLTE_ENTRY_COUNT_LEN + entry_count * bytes_per_entry as usize;
        if payload.len() != expected_len {
            return Err(CafeError::TruncatedFile(format!(
                "PLTE payload length {} doesn't match entry_count={entry_count} \
                 * {bytes_per_entry} bytes/entry + {PLTE_ENTRY_COUNT_LEN} (expected {expected_len})",
                payload.len()
            )));
        }
        Ok(Plte {
            bytes_per_entry,
            entries: payload[PLTE_ENTRY_COUNT_LEN..].to_vec(),
        })
    }

    /// Assembles the complete `PLTE` chunk (Length + Type + Flag + Data +
    /// CRC32). `Flag` is always `0x00` — palettes are tiny (at most
    /// `256 * 4 = 1024` bytes), never worth the ZSTD frame overhead
    /// (mirrors why `IHDR`/`iDIM` are also always raw).
    pub fn to_chunk_bytes(&self) -> Vec<u8> {
        write_chunk(b"PLTE", 0x00, &self.to_payload())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{COLOR_TYPE_GRAY, COLOR_TYPE_RGB, COLOR_TYPE_RGBA};

    fn rgb_colors(n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| [i as u8, (i * 2) as u8, (i * 3) as u8])
            .collect()
    }

    #[test]
    fn test_from_colors_rgb() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(4)).unwrap();
        assert_eq!(plte.bytes_per_entry, 3);
        assert_eq!(plte.entry_count(), 4);
    }

    #[test]
    fn test_from_colors_rgba() {
        let colors: Vec<u8> = (0..4 * 4).collect();
        let plte = Plte::from_colors(COLOR_TYPE_RGBA, &colors).unwrap();
        assert_eq!(plte.bytes_per_entry, 4);
        assert_eq!(plte.entry_count(), 4);
    }

    #[test]
    fn test_from_colors_rejects_invalid_color_type() {
        let result = Plte::from_colors(COLOR_TYPE_GRAY, &[1, 2, 3]);
        assert!(matches!(result, Err(CafeError::InvalidPlte(_))));
    }

    #[test]
    fn test_from_colors_rejects_non_multiple_length() {
        let result = Plte::from_colors(COLOR_TYPE_RGB, &[1, 2]); // not a multiple of 3
        assert!(matches!(result, Err(CafeError::InvalidPlte(_))));
    }

    #[test]
    fn test_roundtrip_payload_rgb() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(3)).unwrap();
        let payload = plte.to_payload();
        let parsed = Plte::from_payload(&payload, COLOR_TYPE_RGB).unwrap();
        assert_eq!(parsed, plte);
    }

    #[test]
    fn test_roundtrip_payload_rgba() {
        let colors: Vec<u8> = (0..(10 * 4)).map(|i| i as u8).collect();
        let plte = Plte::from_colors(COLOR_TYPE_RGBA, &colors).unwrap();
        let payload = plte.to_payload();
        let parsed = Plte::from_payload(&payload, COLOR_TYPE_RGBA).unwrap();
        assert_eq!(parsed, plte);
    }

    #[test]
    fn test_from_payload_rejects_too_short_for_entry_count_field() {
        let result = Plte::from_payload(&[0u8], COLOR_TYPE_RGB);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_from_payload_rejects_length_mismatch() {
        // Declares 2 entries (6 bytes needed) but only supplies 3.
        let mut payload = vec![0u8, 2];
        payload.extend_from_slice(&[1, 2, 3]);
        let result = Plte::from_payload(&payload, COLOR_TYPE_RGB);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_from_payload_rejects_invalid_color_type() {
        let result = Plte::from_payload(&[0, 1, 1, 2, 3], COLOR_TYPE_GRAY);
        assert!(matches!(result, Err(CafeError::InvalidPlte(_))));
    }

    #[test]
    fn test_validate_accepts_consistent_plte() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(10)).unwrap();
        assert!(plte.validate(COLOR_TYPE_RGB, 8).is_ok());
    }

    #[test]
    fn test_validate_rejects_zero_entries() {
        let plte = Plte {
            bytes_per_entry: 3,
            entries: vec![],
        };
        assert!(matches!(
            plte.validate(COLOR_TYPE_RGB, 8),
            Err(CafeError::InvalidPlte(_))
        ));
    }

    #[test]
    fn test_validate_rejects_excessive_entry_count() {
        let plte = Plte {
            bytes_per_entry: 3,
            entries: vec![0u8; 257 * 3], // 257 entries, exceeds MAX_PALETTE_ENTRIES
        };
        assert!(matches!(
            plte.validate(COLOR_TYPE_RGB, 8),
            Err(CafeError::InvalidPlte(_))
        ));
    }

    #[test]
    fn test_validate_accepts_max_entry_count() {
        let plte = Plte {
            bytes_per_entry: 3,
            entries: vec![0u8; 256 * 3], // exactly MAX_PALETTE_ENTRIES
        };
        assert!(plte.validate(COLOR_TYPE_RGB, 8).is_ok());
    }

    #[test]
    fn test_validate_rejects_gray_color_type() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(2)).unwrap();
        assert!(matches!(
            plte.validate(COLOR_TYPE_GRAY, 8),
            Err(CafeError::InvalidPlte(_))
        ));
    }

    #[test]
    fn test_validate_rejects_non_8_bit_depth() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(2)).unwrap();
        assert!(matches!(
            plte.validate(COLOR_TYPE_RGB, 16),
            Err(CafeError::InvalidPlte(_))
        ));
    }

    #[test]
    fn test_validate_rejects_entry_size_mismatch() {
        // Built for RGB (3 bytes/entry) but validated against RGBA IHDR
        // (expects 4 bytes/entry).
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(2)).unwrap();
        assert!(matches!(
            plte.validate(COLOR_TYPE_RGBA, 8),
            Err(CafeError::InvalidPlte(_))
        ));
    }

    #[test]
    fn test_to_chunk_bytes_uses_raw_flag() {
        let plte = Plte::from_colors(COLOR_TYPE_RGB, &rgb_colors(2)).unwrap();
        let bytes = plte.to_chunk_bytes();
        // Length(4) + Type(4) => Flag byte at offset 8, per spec section 3.
        assert_eq!(bytes[8], 0x00);
        assert_eq!(&bytes[4..8], b"PLTE");
    }

    #[test]
    fn test_entry_count_zero_bytes_per_entry_is_zero() {
        let plte = Plte {
            bytes_per_entry: 0,
            entries: vec![1, 2, 3],
        };
        assert_eq!(plte.entry_count(), 0);
    }
}
