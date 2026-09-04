//! Main CAFE types: data structures for chunks, metadata and options.

/// A single decoded tile, already converted to RGBA — the unit of
/// incremental output a future streaming `Decoder<R: Read>` will yield one
/// at a time via `next_tile()` (see AGENTS.md "Streaming" discussion),
/// instead of requiring the whole image to be assembled in memory before
/// any pixels are available to the caller.
///
/// `(x, y)` is the pixel offset of the tile's top-left corner within the
/// full image; `pixels` holds exactly `width * height * 4` bytes of RGBA
/// data (8 bits/channel), regardless of the file's original color type,
/// bit depth, or sample format — the same normalization
/// `DecodeResult`/`decode_bytes` already apply to the whole image.
///
/// Two tile layouts currently produce a `Tile`:
/// - **Row-strip (default, no `iDIM`)**: `x` is always `0`, `width` is
///   always the full image width, and `height` is the number of rows
///   carried by that particular `IDAT` (matches the encoder's
///   `tile_rows`-based horizontal-strip tiling, section 4.3 of the spec).
/// - **2D tiling (`iDIM` present)**: `x`/`y`/`width`/`height` describe the
///   tile's actual rectangular region, which may be smaller than
///   `tile_width`/`tile_height` at the right/bottom edges of the image
///   (section 4.2 of the spec).
///
/// Interlaced images (Adam7/even-odd) do not currently produce `Tile`s —
/// an interlace pass is not a spatial rectangle and cannot be converted to
/// a standalone RGBA sub-image without the other passes.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// RGBA pixel data, `width * height * 4` bytes, row-major, top-to-bottom.
    pub pixels: Vec<u8>,
}

/// Geometry and format metadata returned by `Decoder::read_info()` —
/// everything a streaming caller can learn *before* any pixel data (`IDAT`)
/// has been read off the stream, e.g. to size buffers or pick a code path
/// ahead of calling `next_tile()` in a loop.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodeInfo {
    pub width: u32,
    pub height: u32,
    pub color_type: u8,
    pub bit_depth: u8,
    pub sample_format: u8,
    /// `false` if the file uses 2D tiling (`iDIM`) or interlacing (Adam7 /
    /// even-odd) — `Decoder::next_tile()` does not support either case (see
    /// `Tile`'s doc comment above: an iDIM tile has different geometry than
    /// a row-strip, and an interlace pass is not a spatial rectangle at
    /// all) and will return `Err(CafeError::UnsupportedFeature(..))` on the
    /// first call for such a file. Callers that need those cases must fall
    /// back to `decode_bytes`/`decode` (the whole-image API) instead.
    pub supports_streaming_tiles: bool,
}

/// Palette entry (RGB or RGBA)
#[derive(Clone, Debug, PartialEq)]
pub struct PaletteEntry {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl PaletteEntry {
    pub fn from_rgba(color: &[u8; 4]) -> Self {
        PaletteEntry {
            r: color[0],
            g: color[1],
            b: color[2],
            a: color[3],
        }
    }

    pub fn to_rgba(&self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Euclidean distance in RGBA space (for quantization)
    pub fn distance_squared(&self, other: &PaletteEntry) -> u32 {
        let dr = (self.r as i32) - (other.r as i32);
        let dg = (self.g as i32) - (other.g as i32);
        let db = (self.b as i32) - (other.b as i32);
        let da = (self.a as i32) - (other.a as i32);
        (dr * dr + dg * dg + db * db + da * da) as u32
    }

    /// Perceptually-weighted color distance ("redmean" approximation, see
    /// <https://www.compuphase.com/cmetric.htm>), used by
    /// `PaletteAlgorithm::NearestNeighborWeighted` (v1.5). Unlike
    /// `distance_squared` (plain unweighted Euclidean distance, used by
    /// `NearestNeighbor`/`MedianCut`), this weights each channel's squared
    /// difference by a factor that depends on the mean red level of the two
    /// colors being compared — an inexpensive integer approximation of human
    /// color perception (red and blue contributions are weighted more or
    /// less depending on where the pair sits in the red range; green is
    /// always weighted most heavily, matching its dominant contribution to
    /// perceived luminance). Formula (integer, no floating point, no
    /// `sqrt` — monotonic with the textbook redmean distance, which is all
    /// that's needed for nearest-neighbor comparisons):
    ///
    /// ```text
    /// rmean = (r1 + r2) / 2
    /// dist  = (512 + rmean) * dr^2  +  1024 * dg^2  +  (767 - rmean) * db^2  +  1024 * da^2
    /// ```
    ///
    /// This is the classic redmean formula scaled by 256 (dropping its
    /// `>> 8` step) to stay integer-only. The alpha term is not part of the
    /// original redmean formula — which predates alpha-compositing use
    /// cases — and is added here with the same weight as green, since there
    /// is no equivalent perceptual-weighting literature for alpha and
    /// green's weight is a reasonable, unbiased default.
    ///
    /// Maximum possible value: `767 * 255² + 1024 * 255² + 767 * 255² + 1024
    /// * 255²` ≈ 232,919,550, comfortably within `u32::MAX` (~4.29 billion)
    /// despite the larger weighted magnitudes compared to
    /// `distance_squared`'s unweighted maximum (~260,100) — no overflow risk.
    pub fn redmean_distance(&self, other: &PaletteEntry) -> u32 {
        let r1 = self.r as i64;
        let r2 = other.r as i64;
        let rmean = (r1 + r2) / 2;
        let dr = r1 - r2;
        let dg = self.g as i64 - other.g as i64;
        let db = self.b as i64 - other.b as i64;
        let da = self.a as i64 - other.a as i64;
        let dist =
            (512 + rmean) * dr * dr + 1024 * dg * dg + (767 - rmean) * db * db + 1024 * da * da;
        dist as u32
    }
}

/// Palette (list of indexed colors, section 4.1.2)
#[derive(Clone, Debug)]
pub struct Palette {
    pub entries: Vec<PaletteEntry>,
    pub has_alpha: bool, // true if entry_format = 1 (RGBA), false if = 0 (RGB)
}

impl Palette {
    pub fn new(has_alpha: bool) -> Self {
        Palette {
            entries: Vec::new(),
            has_alpha,
        }
    }

    /// Finds the index of the closest color in the palette (for quantization)
    pub fn find_closest(&self, color: &PaletteEntry) -> u8 {
        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;
        for (idx, entry) in self.entries.iter().enumerate() {
            let dist = color.distance_squared(entry);
            if dist < best_dist {
                best_dist = dist;
                best_idx = idx as u8;
            }
        }
        best_idx
    }

    /// Computes the number of bits needed for indexing
    pub fn bit_depth(&self) -> u8 {
        let len = self.entries.len();
        if len <= 2 {
            1
        } else if len <= 4 {
            2
        } else if len <= 16 {
            4
        } else {
            8
        }
    }
}

/// iDIM structure (section 4.2 v1.0): defines tile partitioning for streaming.
/// Ancillary, optional. If absent, assumes 1 IDAT covering the whole image.
#[derive(Clone, Debug)]
#[allow(non_camel_case_types)]
pub struct iDim {
    pub tile_width: u16,  // Tile width in pixels
    pub tile_height: u16, // Tile height in pixels
    pub tiles_x: u16,     // Number of tiles horizontally
    pub tiles_y: u16,     // Number of tiles vertically
    pub scan_order: u8,   // 0=row-major, 1=Z-order (Morton)
}

impl iDim {
    pub fn new(
        tile_width: u16,
        tile_height: u16,
        img_width: u32,
        img_height: u32,
        scan_order: u8,
    ) -> Self {
        let tiles_x = img_width.div_ceil(tile_width as u32) as u16;
        let tiles_y = img_height.div_ceil(tile_height as u32) as u16;

        iDim {
            tile_width,
            tile_height,
            tiles_x,
            tiles_y,
            scan_order,
        }
    }

    /// Computes the real dimensions of a tile (may be smaller at the edges).
    ///
    /// Defense in depth (CWE-190): `iDim` fields are all `pub` and can be
    /// populated from an untrusted `iDIM` chunk without going through
    /// `iDim::new()` (see `read_idim_chunk` in `src/cafe.rs`), so `tile_x`/
    /// `tile_y`/`tiles_x`/`tiles_y`/`tile_width`/`tile_height`/`img_width`/
    /// `img_height` here must all be treated as adversarial and cannot be
    /// assumed internally consistent. Uses saturating arithmetic throughout
    /// so a malformed/inconsistent combination degrades to `0` instead of
    /// panicking (debug builds) or wrapping to a huge value (release
    /// builds) on subtraction underflow. Callers still validate the result
    /// against the actual decompressed payload size before use, but this
    /// function itself must never panic on any input.
    pub fn tile_dimensions(
        &self,
        tile_x: u16,
        tile_y: u16,
        img_width: u32,
        img_height: u32,
    ) -> (u32, u32) {
        let width = if tile_x == self.tiles_x.saturating_sub(1) {
            img_width.saturating_sub((tile_x as u32).saturating_mul(self.tile_width as u32))
        } else {
            self.tile_width as u32
        };

        let height = if tile_y == self.tiles_y.saturating_sub(1) {
            img_height.saturating_sub((tile_y as u32).saturating_mul(self.tile_height as u32))
        } else {
            self.tile_height as u32
        };

        (width, height)
    }

    /// Alias for `tile_dimensions` (test compatibility)
    pub fn tile_size(
        &self,
        tile_x: u16,
        tile_y: u16,
        img_width: u32,
        img_height: u32,
    ) -> (u32, u32) {
        self.tile_dimensions(tile_x, tile_y, img_width, img_height)
    }

    /// Generates the tile order according to scan_order (section 4.2, v1.0 Phase 2).
    /// - scan_order = 0: row-major (left→right, top→bottom)
    /// - scan_order = 1: Z-order (Morton code, per-region preview)
    ///
    /// # Errors
    /// Returns `CafeError::UnsupportedFeature` if `scan_order` is not 0 or 1.
    pub fn tile_order(&self) -> crate::error::Result<Vec<(u16, u16)>> {
        if self.scan_order == 0 {
            // Row-major: left→right, then top→bottom
            let mut order = Vec::new();
            for ty in 0..self.tiles_y {
                for tx in 0..self.tiles_x {
                    order.push((tx, ty));
                }
            }
            Ok(order)
        } else if self.scan_order == 1 {
            // Z-order (Morton): interleaves tile_x and tile_y bits
            // Enables per-region preview during streaming
            let mut tiles = Vec::new();
            for ty in 0..self.tiles_y {
                for tx in 0..self.tiles_x {
                    let code = morton_code(tx as u32, ty as u32);
                    tiles.push((tx, ty, code));
                }
            }
            // Sort by Morton code
            tiles.sort_by_key(|&(_, _, code)| code);
            // Remove the code, keeping only (tx, ty)
            Ok(tiles.into_iter().map(|(tx, ty, _)| (tx, ty)).collect())
        } else {
            Err(crate::error::CafeError::UnsupportedFeature(format!(
                "Unknown scan order: {}",
                self.scan_order
            )))
        }
    }
}

/// Computes the Morton code of a point (x, y).
/// Interleaves the bits of x and y to create a space-filling Z-order curve.
pub fn morton_code(x: u32, y: u32) -> u64 {
    let mut code = 0u64;
    for i in 0..32 {
        // Extracts the i-th bit of x and y, placing them in alternating positions
        let x_bit = (x >> i) & 1;
        let y_bit = (y >> i) & 1;
        code |= (x_bit as u64) << (2 * i); // x in even position
        code |= (y_bit as u64) << (2 * i + 1); // y in odd position
    }
    code
}

/// Decomposes a Morton code into (x, y) coordinates.
/// Inverse of morton_code().
pub fn morton_decode(code: u64) -> (u32, u32) {
    let mut x = 0u32;
    let mut y = 0u32;
    for i in 0..32 {
        x |= (((code >> (2 * i)) & 1) as u32) << i;
        y |= (((code >> (2 * i + 1)) & 1) as u32) << i;
    }
    (x, y)
}

/// cHDR structure (section 4.4 v1.0): HDR metadata.
/// Ancillary, optional. Defines the transfer function, color space and luminance.
#[derive(Clone, Debug)]
#[allow(non_camel_case_types)]
pub struct cHDR {
    pub transfer_function: u8, // 0=linear, 1=PQ, 2=HLG, 3=sRGB/gamma
    pub color_primaries: u8,   // 0=sRGB/BT.709, 1=BT.2020, 2=DCI-P3
    pub max_luminance: f32,    // In nits (float 32-bit big-endian)
    pub min_luminance: f32,    // In nits (float 32-bit big-endian)
    pub max_cll: Option<u32>,  // Content Light Level (optional, in nits)
    pub max_fall: Option<u32>, // Frame Average Light Level (optional, in nits)
}

impl cHDR {
    /// Creates a new cHDR with default values (linear sRGB, no HDR)
    pub fn new() -> Self {
        cHDR {
            transfer_function: 3, // sRGB/gamma (default)
            color_primaries: 0,   // sRGB/BT.709 (default)
            max_luminance: 1.0,
            min_luminance: 0.0,
            max_cll: None,
            max_fall: None,
        }
    }

    /// Computes the serialized size based on the optional fields present
    pub fn serialized_size(&self) -> usize {
        let mut size = 10; // transfer_function + color_primaries + max_lum + min_lum
        if self.max_cll.is_some() {
            size += 4;
        }
        if self.max_fall.is_some() {
            size += 4;
        }
        size
    }
}

impl Default for cHDR {
    fn default() -> Self {
        Self::new()
    }
}

/// Heuristic for choosing the predictive filter per block (section 4.3.1).
///
/// The encoder always tests the `NUM_FILTERS` predictors over all rows of the
/// block and chooses a single code; the heuristic defines the "best" criterion.
/// Not part of the decoding contract (the decoder only reverts the
/// recorded code), so it can vary between implementations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterHeuristic {
    /// Shannon entropy (zero order) of the residuals, summed row by row.
    /// Cheap and the default.
    #[default]
    Entropy,
    /// Sum of the absolute values of the residuals (MSAD) — the classic PNG
    /// heuristic. Very cheap and good for images with large flat regions;
    /// assumes that small residuals compress well, which is not always true.
    Msad,
    /// Real test compression: compresses each candidate (ZSTD at the
    /// configured level) and picks the smallest final size. Best result,
    /// though costly — ~14 compressions per block.
    CompressionTest,
    /// Quick Filter Pruning (v1.1): tests all 16 filters with MSAD (very fast),
    /// then applies Shannon entropy only to the top 8 candidates.
    /// Balance between speed and quality (~1-2% compression improvement).
    /// Cost: O(24n), Quality: ~90%.
    QuickPrune,
    /// Adaptive Entropy (v1.1): analyzes block type (smooth/natural/high-freq/mixed)
    /// and applies the heuristic most suitable for that content.
    /// Better compression on natural photos (~2-3% improvement).
    /// Cost: O(n) analysis + adaptive heuristic, Quality: ~95%.
    AdaptiveEntropy,
}

/// Encoding options
#[derive(Clone, Debug)]
pub struct EncodeOptions {
    pub tile_rows: u32,
    pub level: i32,
    pub use_filter: bool,
    /// Selects the predictive filter independently per row instead of once
    /// per whole tile (v1.5, `FILTER_METHOD_PREDICTIVE_PER_ROW`). Only takes
    /// effect when `use_filter` is also `true`; mutually exclusive with
    /// `use_byte_shuffle` (byte-shuffle takes precedence, same as
    /// `use_filter`). Only `FilterHeuristic::Entropy` and
    /// `FilterHeuristic::Msad` are supported in this mode — any other value
    /// of `filter_heuristic` causes `encode()`/`encode_indexed()` to return
    /// `CafeError::UnsupportedFeature`. Trades 1 extra filter-code byte per
    /// row for finer-grained adaptation to local content changes within a
    /// tile. Default: `false` (existing per-tile behavior).
    pub use_filter_per_row: bool,
    pub adaptive_analysis: bool,
    pub target_color_type: u8,
    /// Target bit depth for the uint sample format (section 4.1). Valid: GRAY and
    /// GRAY_ALPHA → 1, 2, 4, 8, 10, 12, 16, 32; RGB and RGBA → 8, 10, 12, 16, 32.
    /// `None` = 8 (default). Ignored by float/half sample formats (which fix
    /// 32/16) and by the indexed path (derived from the palette).
    pub target_bit_depth: Option<u8>,
    pub exif: Option<Vec<u8>>,
    pub json_metadata: std::collections::HashMap<String, serde_json::Value>,
    pub icc_profile: Option<Vec<u8>>,
    pub xmp_metadata: Option<String>,
    pub idim: Option<iDim>,
    pub interlace_method: u8,
    pub zstd_dictionary: Option<Vec<u8>>,
    pub sample_format: Option<u8>, // 0=uint, 1=float, 2=half-float (v1.0)
    pub chdr_metadata: Option<cHDR>, // HDR metadata (v1.0)
    pub filter_heuristic: FilterHeuristic, // filter selection criterion (v1.0)
    /// Uses byte-shuffle (Filter Method = 1) instead of the predictive filter.
    /// Reorders bytes of multi-byte samples (bpp ∈ {2,4,8}) to improve the
    /// ZSTD compression of float/HDR data. Mutually exclusive with
    /// `use_filter` (byte-shuffle takes precedence). v1.1.
    pub use_byte_shuffle: bool,
    /// Automatically train a ZSTD dictionary from the image data when
    /// `zstd_dictionary` is None. Useful for improving compression of
    /// small or repetitive images. Default: false (for backward compatibility).
    ///
    /// **Non-regression guarantee (v1.5):** enabling this option never
    /// produces a larger output file than leaving it disabled. The encoder
    /// compresses every IDAT both with and without the trained dictionary
    /// and keeps the smaller result per tile (`compress_with_fallback_dict`
    /// in `codec.rs`); if the dictionary wins at least one tile, it then
    /// compares the *whole-file* total (the `zDIC` chunk's own overhead plus
    /// all IDATs) against re-encoding every IDAT with no dictionary at all,
    /// and keeps whichever total is smaller (see the IDAT/zDIC section of
    /// `encode()` in `cafe.rs`). This guarantee only applies to the
    /// auto-trained dictionary; an explicitly supplied `zstd_dictionary` is
    /// always honored and always emitted, since that is a deliberate
    /// caller decision (e.g. a shared dictionary trained offline across a
    /// batch of related images), not a heuristic.
    pub auto_dictionary: bool,
    /// Palette quantization algorithm for indexed mode (v1.1).
    /// Default: NearestNeighbor (existing behavior).
    pub palette_algorithm: PaletteAlgorithm,
    /// Tone-map operator for HDR decode (v1.2.1)
    /// Default: ToneMapOperator::Filmic (recommended for most images)
    pub tonemap_operator: crate::tonemap::ToneMapOperator,
    /// Opt-in inverse tone-mapping (ITM) for the encode side (v1.8):
    /// synthesizes plausible HDR linear-float pixel data from ordinary SDR
    /// 8-bit input instead of the naive `v/255` used when this is `None`.
    /// Requires `sample_format` to be exactly `Some(1)` (float — matching
    /// `convert_raw_to_rgba`'s own decode-side restriction of tone-mapping
    /// to `SAMPLE_FORMAT_FLOAT`, never `HALF`, so an image produced with
    /// this option round-trips through `decode()`'s existing tone-mapping
    /// path) and `chdr_metadata` to be `Some(_)` with `transfer_function ==
    /// 0` (linear) — `encode()` returns `CafeError::UnsupportedFeature`
    /// otherwise. Only `ToneMapOperator::Reinhard` is supported (`Filmic`
    /// has no closed-form inverse — see `tonemap::ToneMapOperator::apply_inverse`).
    ///
    /// This is inverse tone-mapping (ITM), an approximation that expands
    /// SDR content into a plausible HDR-shaped range — never a lossless
    /// recovery of highlight/shadow detail the SDR source never had.
    /// Default: `None` (existing naive `v/255` behavior, unchanged).
    pub inverse_tonemap: Option<crate::tonemap::ToneMapOperator>,
}

/// Palette quantization algorithm selector (v1.1, extended v1.5)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAlgorithm {
    /// Simple greedy nearest-neighbor (existing behavior, fastest)
    NearestNeighbor,
    /// Median-cut algorithm: recursively splits color space for better quality
    MedianCut,
    /// Same greedy strategy as `NearestNeighbor`, but matching uses a
    /// perceptually-weighted ("redmean") distance instead of plain
    /// unweighted Euclidean distance (v1.5). See
    /// `PaletteEntry::redmean_distance` for the formula. Typically produces
    /// palette assignments that better match human color perception (fewer
    /// visually-jarring mismatches, especially around red/blue extremes) at
    /// the cost of always running the scalar (non-SIMD) matching path —
    /// see `quantize_nearest_neighbor_weighted` in `cafe.rs`.
    NearestNeighborWeighted,
    /// K-means (Lloyd's algorithm) clustering, initialized deterministically
    /// from `MedianCut`'s bucket-averaged output (v1.7, `quantize_kmeans` in
    /// `quantize.rs`). Directly minimizes total squared distance from each
    /// pixel to its assigned palette entry via iterative centroid
    /// refinement, typically producing the lowest mean-squared-error
    /// palette of the four algorithms (at the highest computational cost --
    /// several full passes over the color histogram instead of one).
    KMeans,
}

impl std::str::FromStr for PaletteAlgorithm {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "nearest" | "nearest-neighbor" | "nn" => Ok(PaletteAlgorithm::NearestNeighbor),
            "median-cut" | "mediancut" | "median" => Ok(PaletteAlgorithm::MedianCut),
            "weighted" | "perceptual" | "redmean" => Ok(PaletteAlgorithm::NearestNeighborWeighted),
            "kmeans" | "k-means" => Ok(PaletteAlgorithm::KMeans),
            other => Err(format!(
                "unknown palette algorithm '{}': use 'nearest', 'median-cut', 'weighted', or 'kmeans'",
                other
            )),
        }
    }
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            tile_rows: crate::constants::DEFAULT_TILE_ROWS,
            level: crate::constants::ZSTD_LEVEL,
            use_filter: true,
            use_filter_per_row: false,
            adaptive_analysis: false,
            target_color_type: crate::constants::COLOR_TYPE_RGBA,
            target_bit_depth: None,
            exif: None,
            json_metadata: std::collections::HashMap::new(),
            icc_profile: None,
            tonemap_operator: crate::tonemap::ToneMapOperator::Filmic,
            xmp_metadata: None,
            idim: None,
            interlace_method: crate::constants::INTERLACE_NONE,
            zstd_dictionary: None,
            sample_format: None, // Default: uint (handled in encode)
            chdr_metadata: None, // Default: no HDR metadata
            filter_heuristic: FilterHeuristic::Entropy,
            use_byte_shuffle: false,
            auto_dictionary: false,
            palette_algorithm: PaletteAlgorithm::NearestNeighbor,
            inverse_tonemap: None,
        }
    }
}

/// Options for the streaming `Encoder<W: Write>` / `Encoder<W: Write +
/// Seek>` API (v1, see `cafe.rs`), analogous to `EncodeOptions` for the
/// whole-file `encode()`/`encode_indexed()` path, but deliberately smaller:
/// a handful of `EncodeOptions` features fundamentally require seeing the
/// whole image up front (auto-trained dictionary sampling, indexed-palette
/// quantization) or the whole image's pixels at once (Adam7/even-odd
/// interlacing) — none of these fit a caller that wants to push tiles
/// through as they become available, so they are not offered here at all
/// rather than silently ignored or erroring at runtime.
///
/// **2D tiling (`idim`, v1.10+)** is the one item from this list that
/// turned out *not* to require whole-image buffering after all: unlike
/// `auto_dictionary`/indexed/interlace below, `iDim::tile_order()` only
/// needs `tile_width`/`tile_height`/`scan_order` plus the `width`/`height`
/// already required by `Encoder::new()` — no pixel data. See the `idim`
/// field below and `Encoder::add_idim_tile()` (`cafe.rs`) for the resolved
/// design (row-major and Z-order both supported, one full rectangular tile
/// submitted per call, in `tile_order()`'s sequence).
///
/// **Even/odd interlace (`even_odd_interlace`, v1.11+)** is the second item
/// from this list that turned out not to require whole-image buffering:
/// unlike Adam7 (permanently out of scope, see below), even/odd's 2 passes
/// (even rows, odd rows) are each already fully present within any
/// contiguous row range — a streaming caller submitting rows top-to-bottom
/// via `add_even_odd_rows()` can bucket each row into its pass by row
/// parity and flush a pass's accumulated rows as an `IDAT` once enough have
/// built up, with no need to see rows it hasn't been given yet. See the
/// `even_odd_interlace` field below and `Encoder::add_even_odd_rows()`
/// (`cafe.rs`) for the resolved design. Adam7 remains permanently
/// unsupported by `Encoder<W>` (see below) — the asymmetry this creates
/// (even/odd streamable, Adam7 not) is intentional and mirrors
/// `Decoder<R>::next_tile()`'s own v1.9 resolution (streaming `iDIM`
/// support without streaming interlace support at all); it no longer
/// mirrors `Decoder<R>`'s interlace support *itself*, since
/// `Decoder<R>::next_tile()` continues to reject even/odd files exactly
/// like Adam7 ones (an even/odd pass is still not a spatial rectangle, so
/// it cannot become a standalone `Tile` — this asymmetry, encode can
/// stream row-parity passes but decode cannot yield row-parity passes as
/// tiles, is inherent to the shapes of the two problems, not an oversight).
///
/// # Permanently out of scope (investigated, decided against — not a "v1" gap)
/// Each of the following was investigated for a possible incremental
/// `Encoder<W>`-compatible implementation; each turned out to require
/// either buffering the whole image in memory (defeating the entire point
/// of this API) or a fundamentally different, two-pass API shape — neither
/// of which is an incremental extension of today's single-pass, zero-buffer
/// `add_tile()` contract. See AGENTS.md's "`Encoder<W>` auto_dictionary/
/// indexed/interlace investigation" section for the full per-item analysis;
/// summary:
/// - **`auto_dictionary`**: `train_zstd_dictionary` needs several already-
///   *compressed* tile samples before it can train a useful dictionary, but
///   using that dictionary on the earliest tiles (where it helps compression
///   the most, since it "warms up" ZSTD's context) would require having
///   *not yet written* those tiles — and unlike `finish_exact()`'s single
///   fixed-position `compression_method` byte patch, recompressing an
///   already-written tile changes its length, which would shift every
///   subsequent byte in the file (there is no seek-and-patch fix for a
///   variable-length field). The only fix is buffering the first N tiles
///   before writing anything, which is not a small extension — it is a
///   different contract for `add_tile()`. An explicit, caller-supplied
///   `zstd_dictionary` (trained offline, e.g. across a batch of related
///   images) remains fully supported and requires no such buffering.
/// - **Indexed palette (`COLOR_TYPE_INDEXED`)**: palette quantization needs
///   a color histogram of the *entire* image, and — because `PLTE` must
///   appear before any `IDAT` (section 9's mandatory chunk order) — the
///   final palette must be complete before the first pixel is written.
///   There is no incremental middle ground: either the whole image is
///   buffered (making this just `encode_indexed()` in disguise, not a
///   streaming API), or callers submit tiles twice (once to collect
///   per-tile color statistics, once to actually encode against the now-
///   known palette) — a fundamentally different, two-pass API shape that
///   would deserve its own type (e.g. a hypothetical `Encoder2Pass<W>`)
///   rather than a mode of today's single-pass `Encoder<W>`.
///   `target_color_type` here is therefore restricted to the direct color
///   types (Gray/RGB/GrayAlpha/RGBA); `Encoder<W>::new()` rejects
///   `COLOR_TYPE_INDEXED` explicitly.
/// - **Adam7 interlace**: `extract_adam7_pass` reads directly from the
///   *entire* image buffer, because each of its 7 passes picks pixels
///   scattered non-contiguously across the whole image (e.g. pass 0 =
///   every 8th pixel in both X and Y) — a contiguous row-strip tile does
///   not correspond to "one pass," it is a horizontal band spanning parts
///   of all 7 passes, so generating any pass requires the whole image up
///   front. This is a structural property of Adam7 itself, not a
///   pixel-data-volume problem `add_tile()`-style incremental buffering
///   could solve. (Even/odd interlace does *not* share this problem — see
///   `even_odd_interlace` above and `Encoder::add_even_odd_rows()` for why
///   it's supported instead of being included in this permanently-out-of-
///   scope list.)
///
/// `auto_dictionary` and indexed-palette/interlace support all remain
/// available through the existing `encode()`/`encode_indexed()` (`&str`
/// path-based) or `encode_bytes()` (whole-buffer) APIs, which still require
/// the whole image in memory first.
#[derive(Clone, Debug)]
pub struct EncoderOptions {
    /// Number of rows per row-strip tile (one `IDAT` per tile, section 4.3).
    /// Default: `DEFAULT_TILE_ROWS` (64) — same default and same empirically
    /// measured size/speed trade-off as `EncodeOptions::tile_rows` (see
    /// AGENTS.md "v1.5" notes on `DEFAULT_TILE_ROWS` retuning).
    pub tile_rows: u32,
    /// ZSTD compression level (1-22). Default: `ZSTD_LEVEL` (19).
    pub level: i32,
    pub use_filter: bool,
    /// Selects the predictive filter independently per row instead of once
    /// per whole tile (`FILTER_METHOD_PREDICTIVE_PER_ROW`). Only takes
    /// effect when `use_filter` is also `true`; mutually exclusive with
    /// `use_byte_shuffle` (byte-shuffle takes precedence). Only
    /// `FilterHeuristic::Entropy` and `FilterHeuristic::Msad` are supported
    /// in this mode. Default: `false`.
    pub use_filter_per_row: bool,
    /// Color type to encode pixels as (section 4.1.3). Must be one of
    /// `COLOR_TYPE_GRAY`, `COLOR_TYPE_RGB`, `COLOR_TYPE_GRAY_ALPHA`, or
    /// `COLOR_TYPE_RGBA` — `COLOR_TYPE_INDEXED` is rejected (see struct doc
    /// comment). Default: `COLOR_TYPE_RGBA`.
    pub target_color_type: u8,
    /// Target bit depth for the uint sample format (section 4.1). `None` =
    /// 8 (default). Ignored by float/half sample formats (which fix 32/16).
    pub target_bit_depth: Option<u8>,
    pub exif: Option<Vec<u8>>,
    pub json_metadata: std::collections::HashMap<String, serde_json::Value>,
    pub icc_profile: Option<Vec<u8>>,
    pub xmp_metadata: Option<String>,
    /// Explicit, caller-supplied ZSTD dictionary (section 4.9, `zDIC`).
    /// Unlike `EncodeOptions::auto_dictionary`, there is no automatic
    /// training mode here (see struct doc comment) — this is always honored
    /// unconditionally when present, same as an explicit
    /// `EncodeOptions::zstd_dictionary`.
    pub zstd_dictionary: Option<Vec<u8>>,
    pub sample_format: Option<u8>, // 0=uint, 1=float, 2=half-float
    pub chdr_metadata: Option<cHDR>,
    pub filter_heuristic: FilterHeuristic,
    /// Uses byte-shuffle (Filter Method = 1) instead of the predictive
    /// filter. Mutually exclusive with `use_filter` (byte-shuffle takes
    /// precedence).
    pub use_byte_shuffle: bool,
    /// Enables real 2D tiling (`iDIM`, section 4.2) instead of row-strip
    /// tiling, as `(tile_width, tile_height, scan_order)`. `scan_order`
    /// must be `0` (row-major) or `1` (Z-order/Morton) — any other value is
    /// rejected by `Encoder::new()`. When `Some`, tiles must be submitted
    /// via `Encoder::add_idim_tile()` instead of `add_tile()` (calling the
    /// wrong one for the configured mode is an error) — see
    /// `add_idim_tile()`'s doc comment (`cafe.rs`) for the exact submission
    /// contract (one full rectangular tile per call, in `iDim::tile_order()`
    /// sequence). Default: `None` (row-strip tiling via `add_tile()`,
    /// unchanged from pre-v1.10 behavior).
    pub idim: Option<(u16, u16, u8)>,
    /// Enables even/odd interlacing (`INTERLACE_EVEN_ODD`, section 5)
    /// instead of the default non-interlaced layout. When `true`, rows must
    /// be submitted via `Encoder::add_even_odd_rows()` instead of
    /// `add_tile()`/`add_idim_tile()` (calling the wrong one for the
    /// configured mode is an error) — see `add_even_odd_rows()`'s doc
    /// comment (`cafe.rs`) for the exact submission contract (a contiguous,
    /// top-to-bottom range of rows per call, not required to align to any
    /// particular boundary).
    ///
    /// Mutually exclusive with `idim`, `use_filter_per_row`, and
    /// `use_byte_shuffle` (`Encoder::new()` rejects combining any of them
    /// with `even_odd_interlace = true`, mirroring `encode()`'s own
    /// interlace-incompatibility checks). Also requires `sample_format =
    /// None` (uint), `target_color_type = COLOR_TYPE_RGBA`, and
    /// `target_bit_depth = None` (8) — the same restriction interlace
    /// already has in `encode()` (section 5: Adam7/even-odd only operate on
    /// uint RGBA 8-bit data). Adam7 is *not* offered here — see this
    /// struct's doc comment above for why only even/odd could be made to
    /// fit `Encoder<W>`'s incremental, zero-whole-image-buffer contract.
    /// Default: `false`.
    pub even_odd_interlace: bool,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        EncoderOptions {
            tile_rows: crate::constants::DEFAULT_TILE_ROWS,
            level: crate::constants::ZSTD_LEVEL,
            use_filter: true,
            use_filter_per_row: false,
            target_color_type: crate::constants::COLOR_TYPE_RGBA,
            target_bit_depth: None,
            exif: None,
            json_metadata: std::collections::HashMap::new(),
            icc_profile: None,
            xmp_metadata: None,
            zstd_dictionary: None,
            sample_format: None,
            chdr_metadata: None,
            filter_heuristic: FilterHeuristic::Entropy,
            use_byte_shuffle: false,
            idim: None,
            even_odd_interlace: false,
        }
    }
}

/// Per-chunk compression statistics for a single chunk read during decode
/// (v1.6.2+, `src/cafe.rs`'s `record_chunk_stats`). `chunk_type` is the
/// human-readable 4-byte tag (e.g. `"IDAT"`, `"eXIF"`); `original_size` is
/// the decompressed payload length, `compressed_size` is the on-disk `Data`
/// field length (identical to `original_size` when Flag=raw).
#[derive(Clone, Debug)]
pub struct ChunkStats {
    pub chunk_type: String,
    pub original_size: u32,
    pub compressed_size: u32,
}

/// Aggregated compression statistics across every chunk read during decode
/// (v1.6.2+). `total_original`/`total_compressed` are simply the sum of
/// `chunks`' respective fields — provided pre-summed for convenience since
/// that's almost always what a caller wants first.
#[derive(Clone, Debug)]
pub struct CompressionStats {
    pub total_original: u64,
    pub total_compressed: u64,
    pub chunks: Vec<ChunkStats>,
}

/// Decoding result
pub struct DecodeResult {
    pub width: u32,
    pub height: u32,
    pub exif: Option<Vec<u8>>,
    pub json_metadata: std::collections::HashMap<String, serde_json::Value>,
    /// Real per-chunk compression statistics (v1.6.2+, populated by both
    /// `decode_bytes`/`decode` and `Decoder<R>::finish()`). `None` only in
    /// the practically unreachable case of zero chunks having been recorded
    /// (every valid CAFE file has at least one `IDAT`, which is always
    /// recorded — see `record_chunk_stats` call sites in `src/cafe.rs`).
    pub compression_stats: Option<CompressionStats>,
    pub icc_profile: Option<Vec<u8>>,
    pub xmp_metadata: Option<String>,
    pub zstd_dictionary: Option<Vec<u8>>,
    pub chdr_metadata: Option<cHDR>, // HDR metadata (v1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_redmean_distance_zero_for_identical_colors() {
        let c = PaletteEntry {
            r: 120,
            g: 200,
            b: 30,
            a: 255,
        };
        assert_eq!(c.redmean_distance(&c), 0);
        assert_eq!(c.distance_squared(&c), 0);
    }

    #[test]
    fn test_redmean_distance_symmetric() {
        let a = PaletteEntry {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        };
        let b = PaletteEntry {
            r: 200,
            g: 50,
            b: 5,
            a: 128,
        };
        assert_eq!(a.redmean_distance(&b), b.redmean_distance(&a));
    }

    #[test]
    fn test_redmean_distance_differs_from_unweighted_for_non_gray_pairs() {
        // Redmean weights channels asymmetrically based on mean red level,
        // so for a pair with distinct per-channel deltas the two metrics
        // should generally disagree (this is the whole point of the
        // perceptual weighting) — pick a pair where they provably differ.
        let a = PaletteEntry {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let b = PaletteEntry {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        // Unweighted: dr=255, db=255 -> 255^2 + 255^2 = 130050
        assert_eq!(a.distance_squared(&b), 130_050);
        // Redmean: rmean=(255+0)/2=127, weight_r=512+127=639, weight_b=767-127=640
        // dist = 639*255^2 + 640*255^2 = (639+640)*65025 = 1279*65025
        assert_eq!(a.redmean_distance(&b), 1279 * 65_025);
        assert_ne!(a.distance_squared(&b), a.redmean_distance(&b));
    }

    #[test]
    fn test_redmean_distance_no_overflow_at_extremes() {
        let black = PaletteEntry {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        let white = PaletteEntry {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        // Should not panic (debug overflow check) and should stay within u32.
        let dist = black.redmean_distance(&white);
        assert!(dist > 0);
        assert!(dist < u32::MAX);
    }

    #[test]
    fn test_palette_algorithm_from_str_weighted_variants() {
        assert_eq!(
            PaletteAlgorithm::from_str("weighted").unwrap(),
            PaletteAlgorithm::NearestNeighborWeighted
        );
        assert_eq!(
            PaletteAlgorithm::from_str("perceptual").unwrap(),
            PaletteAlgorithm::NearestNeighborWeighted
        );
        assert_eq!(
            PaletteAlgorithm::from_str("redmean").unwrap(),
            PaletteAlgorithm::NearestNeighborWeighted
        );
        assert_eq!(
            PaletteAlgorithm::from_str("WEIGHTED").unwrap(),
            PaletteAlgorithm::NearestNeighborWeighted
        );
    }

    #[test]
    fn test_palette_algorithm_from_str_still_accepts_existing_variants() {
        assert_eq!(
            PaletteAlgorithm::from_str("nearest").unwrap(),
            PaletteAlgorithm::NearestNeighbor
        );
        assert_eq!(
            PaletteAlgorithm::from_str("median-cut").unwrap(),
            PaletteAlgorithm::MedianCut
        );
    }

    #[test]
    fn test_palette_algorithm_from_str_rejects_unknown() {
        assert!(PaletteAlgorithm::from_str("bogus").is_err());
    }
}
