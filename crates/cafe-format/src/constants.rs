//! Format-wide constants: signature, security ceilings, `IHDR` enums.
//!
//! Mirrors `spec/invariants/*.toml` — every constant here has a
//! corresponding normative value in the spec-as-code invariants, checked
//! for consistency by `tests/spec_invariants.rs`. If you change a value
//! here, update the matching `.toml` (and vice versa).

/// File signature (spec section 2): `\x89CAFE\r\n\x1a\n`.
pub const SIGNATURE: [u8; 9] = [0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A];

/// Maximum bytes a single chunk's decompression may produce before the
/// decoder must reject it (spec section 8.2, CWE-409). 1 GiB.
pub const MAX_DECOMPRESSED_CHUNK_SIZE: u64 = 1024 * 1024 * 1024;

/// Maximum `tiles_x * tiles_y` an `iDIM` chunk may declare before the
/// decoder must reject the file, checked *before* computing tile order or
/// allocating anything proportional to it (spec section 8.2).
pub const MAX_TILE_COUNT: u64 = 1024 * 1024;

/// `IHDR` payload size in bytes (spec section 4.1): Width(4) + Height(4) +
/// BitDepth(1) + SampleFormat(1) + ColorType(1) + CompressionMethod(1).
pub const IHDR_PAYLOAD_LEN: usize = 12;

/// `iDIM` payload size in bytes (spec section 4.2).
pub const IDIM_PAYLOAD_LEN: usize = 9;

/// `PLTE` entry-count field size in bytes (spec section 4.3): a uint16 BE
/// preceding the entries themselves.
pub const PLTE_ENTRY_COUNT_LEN: usize = 2;

/// Maximum number of `PLTE` entries (spec section 4.3 / section 8.2): a
/// one-byte-per-pixel index can only ever select among 256 distinct
/// colors, so `entry_count` above this is rejected outright.
pub const MAX_PALETTE_ENTRIES: u32 = 256;

// --- Color types (spec section 4.1) ---
pub const COLOR_TYPE_GRAY: u8 = 0;
pub const COLOR_TYPE_RGB: u8 = 2;
pub const COLOR_TYPE_GRAY_ALPHA: u8 = 4;
pub const COLOR_TYPE_RGBA: u8 = 6;

// --- Sample formats (spec section 4.1) ---
pub const SAMPLE_FORMAT_UINT: u8 = 0;
pub const SAMPLE_FORMAT_FLOAT: u8 = 1;

// --- Compression method bitmask (spec section 4.1) ---
pub const COMPRESSION_METHOD_ZSTD_BIT: u8 = 0b0000_0001;
pub const COMPRESSION_METHOD_RESERVED_MASK: u8 = 0b1111_1110;

// --- Scan order (spec section 4.2) ---
pub const SCAN_ORDER_ROW_MAJOR: u8 = 0;
pub const SCAN_ORDER_Z_ORDER: u8 = 1;

/// Returns the channel count for a given `color_type`, or `None` if the
/// value is not one of the four valid 0.1 color types (spec section 4.1).
pub fn channels_for_color_type(color_type: u8) -> Option<u8> {
    match color_type {
        COLOR_TYPE_GRAY => Some(1),
        COLOR_TYPE_RGB => Some(3),
        COLOR_TYPE_GRAY_ALPHA => Some(2),
        COLOR_TYPE_RGBA => Some(4),
        _ => None,
    }
}

/// Returns `true` if `sample_format`/`bit_depth` is a valid combination
/// per spec section 4.1's table (uint: 8 or 16; float32: 32 only).
pub fn is_valid_sample_format_bit_depth(sample_format: u8, bit_depth: u8) -> bool {
    match sample_format {
        SAMPLE_FORMAT_UINT => matches!(bit_depth, 8 | 16),
        SAMPLE_FORMAT_FLOAT => bit_depth == 32,
        _ => false,
    }
}

/// `jSON` chunk's namespace-length field size in bytes (spec section
/// 4.6): a single byte preceding the namespace string itself.
pub const JSON_NAMESPACE_LEN_FIELD_LEN: usize = 1;

/// Returns the size in bytes of one `PLTE` entry for a given
/// `color_type` (spec section 4.3): `3` (R,G,B) for `COLOR_TYPE_RGB`, `4`
/// (R,G,B,A) for `COLOR_TYPE_RGBA`, or `None` for any other `color_type`
/// — `PLTE` is undefined for gray/gray+alpha (spec section 4.3's
/// validation rules).
pub fn plte_bytes_per_entry(color_type: u8) -> Option<u8> {
    match color_type {
        COLOR_TYPE_RGB => Some(3),
        COLOR_TYPE_RGBA => Some(4),
        _ => None,
    }
}
