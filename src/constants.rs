//! CAFE constants

/// File signature: \x89CAFE\r\n\x1a\n (section 2 of the spec).
pub const SIGNATURE: [u8; 9] = [0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];

// --- Format version (docs/CAFE-spec.md "Versioning" section) ---
//
// This is deliberately **not** a byte written anywhere in a `.cafe` file —
// there is no `IHDR` version field, by design (see the spec's "Versioning"
// section for the full rationale: it would be a breaking change to the very
// field meant to describe compatibility, and PNG's own decades of stability
// without one is the closest prior art). `FORMAT_VERSION_MAJOR`/`_MINOR`
// exist purely so the crate's `--version` output and any future
// programmatic capability check have a single source of truth to read from,
// instead of a string hardcoded independently in the CLI, the spec, and
// `AGENTS.md`.
//
// This is a *documentation and tooling* version, independent of
// `CARGO_PKG_VERSION` (the crate/implementation's own SemVer, which changes
// on every release regardless of whether the on-disk format changed at
// all). Bump `FORMAT_VERSION_MINOR` only for a backward-compatible normative
// extension (e.g. a new ancillary chunk type, a new enum value an old
// decoder can safely reject); bump `FORMAT_VERSION_MAJOR` (and reset minor
// to 0) only for a change that makes previously-valid files unreadable by a
// conformant decoder of the prior major version, or vice versa. Routine
// implementation work (SIMD, new heuristics, streaming API additions whose
// on-disk bytes are byte-for-byte identical to the existing whole-file
// encoder, CI, CLI flags, documentation) never bumps either number.
pub const FORMAT_VERSION_MAJOR: u32 = 1;
pub const FORMAT_VERSION_MINOR: u32 = 0;

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
/// Per-row predictive filter (v1.5): unlike `FILTER_METHOD_PREDICTIVE` (one
/// filter code for the whole tile), each row within a tile carries its own
/// 1-byte filter code, chosen independently. F_WEIGHTED (15) is excluded
/// from the per-row candidate set (see `NUM_FILTERS_PER_ROW`) because its
/// adaptive state is only well-defined when the same filter runs across
/// consecutive rows of a block.
pub const FILTER_METHOD_PREDICTIVE_PER_ROW: u8 = 3;
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
/// Number of filter candidates considered for per-row selection (v1.5,
/// `FILTER_METHOD_PREDICTIVE_PER_ROW`). Excludes F_WEIGHTED (15), whose
/// adaptive state is only meaningful across multiple consecutive rows of the
/// same filter — see module docs in `filter.rs`.
pub const NUM_FILTERS_PER_ROW: u8 = 15;

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

/// Maximum number of tiles (`tiles_x * tiles_y`) accepted from an `iDIM`
/// chunk. Protection against a memory-allocation DoS (CWE-789/CWE-409-class):
/// `iDim::tile_order()` allocates one `(u16, u16)` tuple per tile (12 bytes
/// per entry in the Z-order path, which additionally sorts the buffer) up
/// front, from a 9-byte ancillary chunk, *before* any `IDAT` is read or
/// validated. Without this cap, `tiles_x = tiles_y = 65535` (both
/// individually valid `u16` values, and satisfiable against a consistent
/// `IHDR` via `tile_width = tile_height = 1`) makes the decoder attempt a
/// ~17 GiB (row-major) or ~51 GiB (Z-order, pre-sort) allocation from a
/// ~71-byte crafted file, aborting the process instead of returning a
/// handleable `Err`. 1,048,576 (1024 × 1024) comfortably covers every
/// legitimate streaming/tiling use case described in section 4.2 of the
/// spec (the reference encoder's default `DEFAULT_TILE_ROWS = 64` implies
/// far fewer tiles even for very large images) while keeping the resulting
/// `tile_order()` allocation on the order of a few dozen MiB at most.
pub const MAX_TILE_COUNT: u64 = 1024 * 1024; // 1,048,576 tiles

/// Maximum number of entries accepted from a `PLTE` chunk. Protection
/// against a disproportionate memory-allocation amplification: a legitimate
/// indexed-color `PLTE` never needs more than 256 entries (bit depths 1, 2,
/// 4, 8 all top out at 256 distinct indices, section 4.1.2), but without
/// this cap the only limit on `read_plte_chunk`'s `Vec<PaletteEntry>` size
/// is the generic 1 GiB `MAX_DECOMPRESSED_CHUNK_SIZE` chunk-decompression
/// ceiling — allowing a single crafted `PLTE` chunk to balloon into
/// gigabytes of `PaletteEntry` structs (4 bytes on disk vs. 4 bytes in
/// memory per entry, but up to ~357M entries from a 1 GiB payload) for data
/// that can never be addressed by any valid pixel index. The encoder
/// already enforces this same 256 limit (see `encode_indexed`).
pub const MAX_PALETTE_ENTRIES: usize = 256;
