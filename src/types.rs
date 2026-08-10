//! Main CAFE types: data structures for chunks, metadata and options.

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

    /// Computes the real dimensions of a tile (may be smaller at the edges)
    pub fn tile_dimensions(
        &self,
        tile_x: u16,
        tile_y: u16,
        img_width: u32,
        img_height: u32,
    ) -> (u32, u32) {
        let width = if tile_x == self.tiles_x - 1 {
            img_width - (tile_x as u32) * (self.tile_width as u32)
        } else {
            self.tile_width as u32
        };

        let height = if tile_y == self.tiles_y - 1 {
            img_height - (tile_y as u32) * (self.tile_height as u32)
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
    pub auto_dictionary: bool,
    /// Palette quantization algorithm for indexed mode (v1.1).
    /// Default: NearestNeighbor (existing behavior).
    pub palette_algorithm: PaletteAlgorithm,
}

/// Palette quantization algorithm selector (v1.1)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAlgorithm {
    /// Simple greedy nearest-neighbor (existing behavior, fastest)
    NearestNeighbor,
    /// Median-cut algorithm: recursively splits color space for better quality
    MedianCut,
}

impl std::str::FromStr for PaletteAlgorithm {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "nearest" | "nearest-neighbor" | "nn" => Ok(PaletteAlgorithm::NearestNeighbor),
            "median-cut" | "mediancut" | "median" => Ok(PaletteAlgorithm::MedianCut),
            other => Err(format!(
                "unknown palette algorithm '{}': use 'nearest' or 'median-cut'",
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
            adaptive_analysis: false,
            target_color_type: crate::constants::COLOR_TYPE_RGBA,
            target_bit_depth: None,
            exif: None,
            json_metadata: std::collections::HashMap::new(),
            icc_profile: None,
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
        }
    }
}

/// Chunk statistics
#[derive(Clone, Debug)]
pub struct ChunkStats {
    pub chunk_type: String,
    pub original_size: u32,
    pub compressed_size: u32,
}

/// Compression statistics
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
    pub compression_stats: Option<CompressionStats>,
    pub icc_profile: Option<Vec<u8>>,
    pub xmp_metadata: Option<String>,
    pub zstd_dictionary: Option<Vec<u8>>,
    pub chdr_metadata: Option<cHDR>, // HDR metadata (v1.0)
}
