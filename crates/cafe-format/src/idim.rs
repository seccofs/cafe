//! `iDIM` chunk (spec section 4.2): ancillary, optional — declares tile
//! partitioning and scan order for streaming.
//!
//! Ported from `old/src/types.rs`'s `iDim` struct (renamed `Idim` for
//! standard Rust casing — the old lineage's `#[allow(non_camel_case_types)]`
//! is not carried forward), trimmed to what `cafe-format` needs: framing
//! and structural validation. Tile-order enumeration (row-major vs.
//! Morton) is `cafe-codec`'s concern (it needs `MAX_TILE_COUNT`-checked
//! geometry before allocating anything proportional to tile count) — see
//! `cafe_codec::tiling`.

use crate::chunk::write_chunk;
use crate::constants::{
    IDIM_PAYLOAD_LEN, MAX_TILE_COUNT, SCAN_ORDER_ROW_MAJOR, SCAN_ORDER_Z_ORDER,
};
use crate::error::{CafeError, Result};

/// A parsed `iDIM` payload (spec section 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Idim {
    pub tile_width: u16,
    pub tile_height: u16,
    pub tiles_x: u16,
    pub tiles_y: u16,
    pub scan_order: u8,
}

impl Idim {
    /// Derives an `Idim` from an image's dimensions and a chosen tile size
    /// (encoder-side convenience): `tiles_x = ceil(width / tile_width)`,
    /// `tiles_y = ceil(height / tile_height)` (spec section 4.2: "no
    /// padding... the last column/row of tiles has reduced actual
    /// dimensions").
    pub fn for_image(
        tile_width: u16,
        tile_height: u16,
        width: u32,
        height: u32,
        scan_order: u8,
    ) -> Self {
        let tiles_x = width.div_ceil(tile_width as u32) as u16;
        let tiles_y = height.div_ceil(tile_height as u32) as u16;
        Idim {
            tile_width,
            tile_height,
            tiles_x,
            tiles_y,
            scan_order,
        }
    }

    /// `tiles_x as u64 * tiles_y as u64`, the quantity spec section 8.2's
    /// `MAX_TILE_COUNT` ceiling bounds. Widened to `u64` so the multiply
    /// itself can never overflow (`tiles_x`/`tiles_y` are `u16`).
    pub fn tile_count(&self) -> u64 {
        self.tiles_x as u64 * self.tiles_y as u64
    }

    /// Validates this `iDIM` against the image dimensions declared in
    /// `IHDR` (spec section 4.2 + 8.2): nonzero `tile_width`/`tile_height`/
    /// `tiles_x`/`tiles_y`, `tiles_x`/`tiles_y` exactly matching the
    /// ceiling-division derivation from `width`/`height`, a known
    /// `scan_order`, and `tile_count() <= MAX_TILE_COUNT` — checked
    /// *before* any caller computes tile order or allocates anything
    /// proportional to tile count (CWE-789/CWE-409-class, mirrors
    /// `old/src/cafe.rs::handle_idim_chunk`).
    pub fn validate(&self, width: u32, height: u32) -> Result<()> {
        if self.tile_width == 0 || self.tile_height == 0 {
            return Err(CafeError::InvalidIdim(
                "tile_width and tile_height must be nonzero".into(),
            ));
        }
        if self.tiles_x == 0 || self.tiles_y == 0 {
            return Err(CafeError::InvalidIdim(
                "tiles_x and tiles_y must be nonzero".into(),
            ));
        }
        if self.scan_order != SCAN_ORDER_ROW_MAJOR && self.scan_order != SCAN_ORDER_Z_ORDER {
            return Err(CafeError::InvalidIdim(format!(
                "unknown scan_order {} (spec section 4.2 defines only 0=row-major, 1=Z-order)",
                self.scan_order
            )));
        }
        // SECURITY (CWE-789/CWE-409-class): check the tile count ceiling
        // before the tiles_x/tiles_y-vs-IHDR consistency check below would
        // otherwise still let an attacker probe with tiny tile sizes; both
        // checks are cheap, but this one guards every caller that might
        // allocate a Vec sized by tile_count() right after validate()
        // returns Ok, so it must never be reachable to skip.
        if self.tile_count() > MAX_TILE_COUNT {
            return Err(CafeError::InvalidIdim(format!(
                "tiles_x * tiles_y = {} exceeds maximum allowed tile count ({MAX_TILE_COUNT})",
                self.tile_count()
            )));
        }
        let expected_tiles_x = width.div_ceil(self.tile_width as u32);
        let expected_tiles_y = height.div_ceil(self.tile_height as u32);
        if self.tiles_x as u32 != expected_tiles_x || self.tiles_y as u32 != expected_tiles_y {
            return Err(CafeError::InvalidIdim(format!(
                "tiles_x/tiles_y ({}, {}) inconsistent with IHDR dimensions {width}x{height} \
                 and tile size {}x{} (expected {expected_tiles_x}, {expected_tiles_y})",
                self.tiles_x, self.tiles_y, self.tile_width, self.tile_height
            )));
        }
        Ok(())
    }

    /// Computes the real dimensions of tile `(tile_x, tile_y)` (may be
    /// smaller than `tile_width`/`tile_height` at the right/bottom edges
    /// when `width`/`height` are not exact multiples of the tile size —
    /// spec section 4.2: "no padding"). Saturating arithmetic throughout
    /// so a caller that skipped [`Idim::validate`] gets `0` instead of a
    /// panic on an inconsistent combination (defense in depth, mirrors
    /// `old/src/types.rs::iDim::tile_dimensions`'s doc comment).
    pub fn tile_dimensions(&self, tile_x: u16, tile_y: u16, width: u32, height: u32) -> (u32, u32) {
        let tile_width = if tile_x == self.tiles_x.saturating_sub(1) {
            width.saturating_sub((tile_x as u32).saturating_mul(self.tile_width as u32))
        } else {
            self.tile_width as u32
        };
        let tile_height = if tile_y == self.tiles_y.saturating_sub(1) {
            height.saturating_sub((tile_y as u32).saturating_mul(self.tile_height as u32))
        } else {
            self.tile_height as u32
        };
        (tile_width, tile_height)
    }

    /// Serializes to the 9-byte `iDIM` payload (spec section 4.2).
    pub fn to_payload(&self) -> [u8; IDIM_PAYLOAD_LEN] {
        let mut buf = [0u8; IDIM_PAYLOAD_LEN];
        buf[0..2].copy_from_slice(&self.tile_width.to_be_bytes());
        buf[2..4].copy_from_slice(&self.tile_height.to_be_bytes());
        buf[4..6].copy_from_slice(&self.tiles_x.to_be_bytes());
        buf[6..8].copy_from_slice(&self.tiles_y.to_be_bytes());
        buf[8] = self.scan_order;
        buf
    }

    /// Parses (but does not [`validate`](Idim::validate)) a 9-byte `iDIM`
    /// payload. Returns `TruncatedFile` if `payload.len() !=
    /// IDIM_PAYLOAD_LEN`.
    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        if payload.len() != IDIM_PAYLOAD_LEN {
            return Err(CafeError::TruncatedFile(format!(
                "iDIM payload must be {IDIM_PAYLOAD_LEN} bytes, got {}",
                payload.len()
            )));
        }
        Ok(Idim {
            tile_width: u16::from_be_bytes(payload[0..2].try_into().unwrap()),
            tile_height: u16::from_be_bytes(payload[2..4].try_into().unwrap()),
            tiles_x: u16::from_be_bytes(payload[4..6].try_into().unwrap()),
            tiles_y: u16::from_be_bytes(payload[6..8].try_into().unwrap()),
            scan_order: payload[8],
        })
    }

    /// Assembles the complete `iDIM` chunk (Length + Type + Flag + Data +
    /// CRC32). `Flag` is always `0x00` — 9 bytes is never worth the ZSTD
    /// frame overhead (mirrors why `IHDR` is also always raw).
    pub fn to_chunk_bytes(&self) -> Vec<u8> {
        write_chunk(b"iDIM", 0x00, &self.to_payload())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_for_image_exact_multiple() {
        let idim = Idim::for_image(64, 64, 128, 128, 0);
        assert_eq!(idim.tiles_x, 2);
        assert_eq!(idim.tiles_y, 2);
    }

    #[test]
    fn test_for_image_ceiling_division_for_partial_tiles() {
        let idim = Idim::for_image(64, 64, 130, 65, 0);
        assert_eq!(idim.tiles_x, 3); // ceil(130/64) = 3
        assert_eq!(idim.tiles_y, 2); // ceil(65/64) = 2
    }

    #[test]
    fn test_idim_roundtrip_payload() {
        let idim = Idim::for_image(32, 32, 100, 90, 1);
        let payload = idim.to_payload();
        assert_eq!(payload.len(), IDIM_PAYLOAD_LEN);
        let parsed = Idim::from_payload(&payload).unwrap();
        assert_eq!(parsed, idim);
    }

    #[test]
    fn test_from_payload_rejects_wrong_length() {
        let result = Idim::from_payload(&[0u8; 5]);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_validate_accepts_consistent_idim() {
        let idim = Idim::for_image(64, 64, 130, 65, 0);
        assert!(idim.validate(130, 65).is_ok());
    }

    #[test]
    fn test_validate_rejects_zero_tile_width() {
        let mut idim = Idim::for_image(64, 64, 128, 128, 0);
        idim.tile_width = 0;
        assert!(matches!(
            idim.validate(128, 128),
            Err(CafeError::InvalidIdim(_))
        ));
    }

    #[test]
    fn test_validate_rejects_zero_tiles_x() {
        let mut idim = Idim::for_image(64, 64, 128, 128, 0);
        idim.tiles_x = 0;
        assert!(matches!(
            idim.validate(128, 128),
            Err(CafeError::InvalidIdim(_))
        ));
    }

    #[test]
    fn test_validate_rejects_unknown_scan_order() {
        let mut idim = Idim::for_image(64, 64, 128, 128, 0);
        idim.scan_order = 2;
        assert!(matches!(
            idim.validate(128, 128),
            Err(CafeError::InvalidIdim(_))
        ));
    }

    #[test]
    fn test_validate_rejects_tiles_inconsistent_with_ihdr_dimensions() {
        let idim = Idim {
            tile_width: 64,
            tile_height: 64,
            tiles_x: 1,
            tiles_y: 1,
            scan_order: 0,
        };
        // 1x1 tiles of 64x64 cannot cover a 128x128 image.
        assert!(matches!(
            idim.validate(128, 128),
            Err(CafeError::InvalidIdim(_))
        ));
    }

    #[test]
    fn test_validate_rejects_excessive_tile_count() {
        // tile_width=tile_height=1, tiles_x=tiles_y=65535: individually
        // valid u16s, consistent with a 65535x65535 image, but their
        // product (~4.29 billion) is the classic CWE-789 exploit from the
        // old lineage's MAX_TILE_COUNT doc comment.
        let idim = Idim {
            tile_width: 1,
            tile_height: 1,
            tiles_x: 65535,
            tiles_y: 65535,
            scan_order: 0,
        };
        assert!(matches!(
            idim.validate(65535, 65535),
            Err(CafeError::InvalidIdim(_))
        ));
    }

    #[test]
    fn test_tile_dimensions_full_tiles_everywhere_for_exact_multiple() {
        let idim = Idim::for_image(64, 64, 128, 128, 0);
        assert_eq!(idim.tile_dimensions(0, 0, 128, 128), (64, 64));
        assert_eq!(idim.tile_dimensions(1, 1, 128, 128), (64, 64));
    }

    #[test]
    fn test_tile_dimensions_partial_edge_tiles() {
        let idim = Idim::for_image(64, 64, 130, 65, 0);
        // tiles_x=3 (64,64,2), tiles_y=2 (64,1)
        assert_eq!(idim.tile_dimensions(0, 0, 130, 65), (64, 64));
        assert_eq!(idim.tile_dimensions(1, 0, 130, 65), (64, 64));
        assert_eq!(idim.tile_dimensions(2, 0, 130, 65), (2, 64)); // last column: 130 - 2*64 = 2
        assert_eq!(idim.tile_dimensions(0, 1, 130, 65), (64, 1)); // last row: 65 - 1*64 = 1
        assert_eq!(idim.tile_dimensions(2, 1, 130, 65), (2, 1)); // corner tile
    }

    #[test]
    fn test_tile_count_computation() {
        let idim = Idim {
            tile_width: 1,
            tile_height: 1,
            tiles_x: 65535,
            tiles_y: 65535,
            scan_order: 0,
        };
        assert_eq!(idim.tile_count(), 65535u64 * 65535u64);
    }

    #[test]
    fn test_to_chunk_bytes_uses_raw_flag() {
        let bytes = Idim::for_image(64, 64, 128, 128, 0).to_chunk_bytes();
        // Length(4) + Type(4) => Flag byte at offset 8, per spec section 3.
        assert_eq!(bytes[8], 0x00);
    }
}
