//! Tile grid geometry and visitation order (spec section 4.2): turns a
//! validated [`cafe_format::Idim`] into the concrete sequence of
//! `(tile_x, tile_y)` positions `IDAT` chunks appear in, and back
//! (position -> pixel-space offset/size).
//!
//! This is `cafe-codec`'s layer on top of `cafe_format::idim` — that crate
//! only knows chunk framing and structural validation (nonzero fields,
//! `tiles_x`/`tiles_y` consistent with `IHDR`, `MAX_TILE_COUNT`); this
//! module adds the decode/encode-time concern of enumerating tiles in scan
//! order and mapping between grid position and raw pixel-buffer offsets.

use crate::error::{CodecError, Result};
use crate::morton::morton_code;
use cafe_format::constants::{SCAN_ORDER_ROW_MAJOR, SCAN_ORDER_Z_ORDER};
use cafe_format::Idim;

/// Enumerates every `(tile_x, tile_y)` position in `idim`'s grid, ordered
/// per `idim.scan_order` (spec section 4.2): row-major (left->right, then
/// top->bottom) or Z-order (Morton code of `(tile_x, tile_y)`, ascending).
///
/// The N-th entry of the returned `Vec` corresponds to the N-th `IDAT` in
/// the file (spec section 4.2: "The N-th IDAT in the file... corresponds
/// to the N-th position in this enumeration order").
///
/// Callers must validate `idim` (in particular `Idim::validate`'s
/// `MAX_TILE_COUNT` check) *before* calling this — it allocates one
/// `(u16, u16)` tuple per tile (plus a temporary sort buffer for Z-order),
/// proportional to `idim.tile_count()`, with no further bound of its own.
pub fn tile_order(idim: &Idim) -> Result<Vec<(u16, u16)>> {
    match idim.scan_order {
        SCAN_ORDER_ROW_MAJOR => {
            let mut order = Vec::with_capacity(idim.tile_count() as usize);
            for ty in 0..idim.tiles_y {
                for tx in 0..idim.tiles_x {
                    order.push((tx, ty));
                }
            }
            Ok(order)
        }
        SCAN_ORDER_Z_ORDER => {
            let mut tiles: Vec<(u16, u16, u64)> = Vec::with_capacity(idim.tile_count() as usize);
            for ty in 0..idim.tiles_y {
                for tx in 0..idim.tiles_x {
                    let code = morton_code(tx as u32, ty as u32);
                    tiles.push((tx, ty, code));
                }
            }
            tiles.sort_by_key(|&(_, _, code)| code);
            Ok(tiles.into_iter().map(|(tx, ty, _)| (tx, ty)).collect())
        }
        other => Err(CodecError::Format(cafe_format::CafeError::InvalidIdim(
            format!("unknown scan_order {other} (spec section 4.2 defines only 0, 1)"),
        ))),
    }
}

/// Pixel-space top-left offset (in samples, not bytes) of tile
/// `(tile_x, tile_y)` within the whole image, given `idim`'s nominal tile
/// size (spec section 4.2: tiles are laid out on a uniform grid — only the
/// last row/column may be smaller, never offset).
pub fn tile_origin(idim: &Idim, tile_x: u16, tile_y: u16) -> (u32, u32) {
    (
        tile_x as u32 * idim.tile_width as u32,
        tile_y as u32 * idim.tile_height as u32,
    )
}

/// The concrete tile grid for one image: either the implicit single
/// whole-image tile (no `iDIM`, spec section 4.2: "If absent, the decoder
/// assumes a single `IDAT` covering the entire image") or an explicit
/// `iDIM`-declared grid with its scan order already resolved via
/// [`tile_order`]. Shared by [`crate::decoder`] and [`crate::encoder`] so
/// the two can never diverge on how a tile index maps to a pixel-space
/// rectangle.
#[derive(Debug, Clone)]
pub struct TileLayout {
    idim: Option<Idim>,
    order: Vec<(u16, u16)>,
    image_width: u32,
    image_height: u32,
}

impl TileLayout {
    /// Builds the tile layout for an image of `image_width`x`image_height`.
    ///
    /// `idim = None` means the implicit single whole-image tile.
    /// `idim = Some(idim)` must already be validated by the caller via
    /// [`Idim::validate`] (in particular its `MAX_TILE_COUNT` check) —
    /// this calls [`tile_order`], which relies on that having already
    /// happened before allocating anything proportional to tile count.
    pub fn new(idim: Option<Idim>, image_width: u32, image_height: u32) -> Result<Self> {
        let order = match &idim {
            None => vec![(0u16, 0u16)],
            Some(idim) => tile_order(idim)?,
        };
        Ok(Self {
            idim,
            order,
            image_width,
            image_height,
        })
    }

    /// Total number of tiles in this layout (`1` when `idim` is `None`).
    pub fn tile_count(&self) -> usize {
        self.order.len()
    }

    /// Pixel-space rectangle `(origin_x, origin_y, tile_width, tile_height)`
    /// of the `index`-th tile in scan order (0-indexed, matching the N-th
    /// `IDAT`'s position — spec section 4.2). Panics if `index >=
    /// self.tile_count()`, same contract as slice indexing.
    pub fn tile_rect(&self, index: usize) -> (u32, u32, u32, u32) {
        match &self.idim {
            None => (0, 0, self.image_width, self.image_height),
            Some(idim) => {
                let (tx, ty) = self.order[index];
                let (ox, oy) = tile_origin(idim, tx, ty);
                let (tw, th) = idim.tile_dimensions(tx, ty, self.image_width, self.image_height);
                (ox, oy, tw, th)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idim_grid(tiles_x: u16, tiles_y: u16, scan_order: u8) -> Idim {
        Idim {
            tile_width: 8,
            tile_height: 8,
            tiles_x,
            tiles_y,
            scan_order,
        }
    }

    #[test]
    fn test_row_major_order_2x2() {
        let idim = idim_grid(2, 2, SCAN_ORDER_ROW_MAJOR);
        let order = tile_order(&idim).unwrap();
        assert_eq!(order, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn test_row_major_order_3x2() {
        let idim = idim_grid(3, 2, SCAN_ORDER_ROW_MAJOR);
        let order = tile_order(&idim).unwrap();
        assert_eq!(order, vec![(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]);
    }

    #[test]
    fn test_z_order_2x2_matches_known_morton_sequence() {
        let idim = idim_grid(2, 2, SCAN_ORDER_Z_ORDER);
        let order = tile_order(&idim).unwrap();
        // morton_code: (0,0)=0, (1,0)=1, (0,1)=2, (1,1)=3
        assert_eq!(order, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn test_z_order_4x4_is_valid_permutation_of_row_major() {
        let idim_z = idim_grid(4, 4, SCAN_ORDER_Z_ORDER);
        let idim_rm = idim_grid(4, 4, SCAN_ORDER_ROW_MAJOR);
        let mut z_order = tile_order(&idim_z).unwrap();
        let mut rm_order = tile_order(&idim_rm).unwrap();
        assert_eq!(z_order.len(), 16);
        z_order.sort();
        rm_order.sort();
        assert_eq!(z_order, rm_order); // same set of tiles, different order
    }

    #[test]
    fn test_z_order_groups_2x2_blocks_contiguously() {
        // Defining Z-order property: within an aligned 2x2 block, all four
        // tiles are visited consecutively.
        let idim = idim_grid(4, 4, SCAN_ORDER_Z_ORDER);
        let order = tile_order(&idim).unwrap();
        let pos_of = |t: (u16, u16)| order.iter().position(|&x| x == t).unwrap();
        for by in [0u16, 2] {
            for bx in [0u16, 2] {
                let mut positions = [
                    pos_of((bx, by)),
                    pos_of((bx + 1, by)),
                    pos_of((bx, by + 1)),
                    pos_of((bx + 1, by + 1)),
                ];
                positions.sort_unstable();
                assert_eq!(
                    positions,
                    [
                        positions[0],
                        positions[0] + 1,
                        positions[0] + 2,
                        positions[0] + 3
                    ]
                );
            }
        }
    }

    #[test]
    fn test_tile_order_rejects_unknown_scan_order() {
        let idim = idim_grid(2, 2, 2); // 2 is not a defined scan_order
        let result = tile_order(&idim);
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidIdim(_)))
        ));
    }

    #[test]
    fn test_tile_origin_computation() {
        let idim = idim_grid(3, 2, SCAN_ORDER_ROW_MAJOR);
        assert_eq!(tile_origin(&idim, 0, 0), (0, 0));
        assert_eq!(tile_origin(&idim, 1, 0), (8, 0));
        assert_eq!(tile_origin(&idim, 2, 1), (16, 8));
    }

    #[test]
    fn test_single_tile_grid_order_is_trivial() {
        let idim = idim_grid(1, 1, SCAN_ORDER_ROW_MAJOR);
        assert_eq!(tile_order(&idim).unwrap(), vec![(0, 0)]);
        let idim_z = idim_grid(1, 1, SCAN_ORDER_Z_ORDER);
        assert_eq!(tile_order(&idim_z).unwrap(), vec![(0, 0)]);
    }
}
