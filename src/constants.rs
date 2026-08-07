//! CAFE constants

/// File signature: \x89CAFE\r\n\x1a\n (section 2 of the spec).
pub const SIGNATURE: [u8; 9] = [0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];

// --- Enum constants (sections 3.2 and 4.1 of the spec) ---
pub const FLAG_RAW: u8 = 0x00;
pub const FLAG_ZSTD: u8 = 0x01;

pub const COLOR_TYPE_GRAY: u8 = 0;
pub const COLOR_TYPE_RGB: u8 = 2;
pub const COLOR_TYPE_INDEXED: u8 = 3;
pub const COLOR_TYPE_GRAY_ALPHA: u8 = 4;
pub const COLOR_TYPE_RGBA: u8 = 6;

// --- Sample Format (section 4.1, v1.0) ---
pub const SAMPLE_FORMAT_UINT: u8 = 0; // Unsigned integer (default)
pub const SAMPLE_FORMAT_FLOAT: u8 = 1; // IEEE 754 float (32-bit)
pub const SAMPLE_FORMAT_HALF: u8 = 2; // Half-float (fp16, 16-bit)

pub const COMPRESSION_METHOD_ZSTD_BIT: u8 = 0b0000_0001;
pub const FILTER_METHOD_NONE: u8 = 0;
pub const FILTER_METHOD_BYTE_SHUFFLE: u8 = 1;
pub const FILTER_METHOD_PREDICTIVE: u8 = 2;
pub const INTERLACE_NONE: u8 = 0;

pub const CHUNK_IHDR: &[u8; 4] = b"IHDR";
pub const CHUNK_IDIM: &[u8; 4] = b"iDIM"; // ancillary, optional, defines partitioning for streaming (section 4.2, v1.0)
pub const CHUNK_PLTE: &[u8; 4] = b"PLTE"; // critical, required with Color type = 3 (section 4.1.2)
pub const CHUNK_IDAT: &[u8; 4] = b"IDAT";
pub const CHUNK_IEND: &[u8; 4] = b"IEND";
pub const CHUNK_EXIF: &[u8; 4] = b"eXIF"; // ancillary (1st letter lowercase), single instance (section 4.5)
pub const CHUNK_JSON: &[u8; 4] = b"jSON"; // ancillary, multiple instances per namespace (section 4.6)
pub const CHUNK_ICCP: &[u8; 4] = b"iCCP"; // ancillary, optional, ICC profile (section 4.7, v1.0)
pub const CHUNK_XMPD: &[u8; 4] = b"xMPd"; // ancillary, optional, XMP metadata (section 4.8, v1.0)
pub const CHUNK_ZDIC: &[u8; 4] = b"zDIC"; // ancillary, optional, ZSTD dictionary (v1.0)
pub const CHUNK_CHDR: &[u8; 4] = b"cHDR"; // ancillary, optional, HDR metadata (section 4.4, v1.0)

// --- Predictive filter codes (section 4.3.1) ---
pub const F_NONE: u8 = 0;
pub const F_SUB: u8 = 1;
pub const F_UP: u8 = 2;
pub const F_AVERAGE: u8 = 3;
pub const F_PAETH: u8 = 4;
pub const F_MED: u8 = 5;
pub const F_GRADIENT: u8 = 6;
pub const F_SMEDIAN: u8 = 7; // Simple Median (v1.0)
pub const F_2NDORDER: u8 = 8; // 2nd Order Difference (v1.0)
pub const F_4WAY_H: u8 = 9; // 4-way Directional: Horizontal emphasis (v1.0)
pub const F_4WAY_V: u8 = 10; // 4-way Directional: Vertical emphasis
pub const F_4WAY_D1: u8 = 11; // 4-way Directional: Diagonal \ emphasis
pub const F_4WAY_D2: u8 = 12; // 4-way Directional: Diagonal / emphasis
pub const F_CONTEXT: u8 = 13; // Context-Based: Detects edges and chooses dynamically (v1.0)
pub const F_TR_DIRECTIONAL: u8 = 14; // TR-aware Directional: bilinear average with top-right neighbor (WebP Predictor 10, v1.1)
pub const F_WEIGHTED: u8 = 15; // Adaptive Weighted: adaptive weighted predictor (inspired by JPEG-XL, v1.1)
pub const NUM_FILTERS: u8 = 16;

// --- Adam7 Interlace (section 5, v1.0 Phase 3) ---
pub const INTERLACE_ADAM7: u8 = 1;
pub const INTERLACE_EVEN_ODD: u8 = 2; // Even/odd (v1.0)
pub const ADAM7_NUM_PASSES: usize = 7;
pub const EVEN_ODD_NUM_PASSES: usize = 2; // Even/odd: 2 passes

// Adam7 pass parameters: (x_step, y_step, x_offset, y_offset)
// Standard PNG Adam7 according to section 5 of the CAFE spec.
// Each pass covers a different fraction WITHOUT progressive overlap.
// Pass 1: 1/64 pixels → Pass 7: all pixels (100% coverage)
pub const ADAM7_PASSES: &[(u32, u32, u32, u32)] = &[
    (8, 8, 0, 0), // Pass 1: x_step=8, y_step=8, x_offset=0, y_offset=0 (1/64 pixels)
    (8, 8, 4, 0), // Pass 2: x_step=8, y_step=8, x_offset=4, y_offset=0 (1/32 pixels)
    (4, 8, 2, 0), // Pass 3: x_step=4, y_step=8, x_offset=2, y_offset=0 (1/32 pixels)
    (4, 4, 2, 2), // Pass 4: x_step=4, y_step=4, x_offset=2, y_offset=2 (1/16 pixels)
    (2, 4, 1, 2), // Pass 5: x_step=2, y_step=4, x_offset=1, y_offset=2 (1/16 pixels)
    (2, 2, 1, 1), // Pass 6: x_step=2, y_step=2, x_offset=1, y_offset=1 (1/4 pixels)
    (1, 1, 0, 0), // Pass 7: x_step=1, y_step=1, x_offset=0, y_offset=0 (all remaining pixels)
];

pub const DEFAULT_TILE_ROWS: u32 = 64;
pub const ZSTD_LEVEL: i32 = 19;
pub const BPP: usize = 4; // bytes per pixel for RGBA (8 bits/channel)

/// Maximum number of bytes that the decompression of a single chunk can
/// produce. Protection against "decompression bomb" (CWE-409): without this
/// limit, a chunk of a few KB could expand to gigabytes and
/// exhaust all the memory available to the process. 1 GiB is generous for
/// realistic images (even at very high resolution), but finite.
pub const MAX_DECOMPRESSED_CHUNK_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB
