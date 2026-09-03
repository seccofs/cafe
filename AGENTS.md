# CAFE — Developer Technical Guide

## Overview

CAFE (Compression Adaptive Filtering Experiment) is a modern chunk-based image format inspired by PNG, with support for ZSTD compression, advanced predictive filters, indexed palette, and structured metadata (EXIF, JSON, ICC, XMP).

**Specification:** `docs/CAFE-spec.md` (v1.1)
**Implementation:** Rust 2021 with BSD-3-Clause license

---

## Project Architecture

### File Structure

```
Cafe/
├── AGENTS.md             # This guide (you are here)
├── Cargo.toml            # Dependencies and configuration (with simd feature)
├── deny.toml             # Cargo-deny security and license configuration
├── README.md             # English README
├── README.pt.md          # Portuguese README
├── LICENSE               # BSD-3-Clause license text
├── src/
│   ├── cafe.rs                      # Core: encode/decode, chunks (re-exports)
│   ├── constants.rs                 # Signature, flags, color types, filters
│   ├── chunk.rs                     # Chunk framing (Length/Type/Flag/Data/CRC32)
│   ├── codec.rs                     # ZSTD compression with fallback (section 3.2)
│   ├── color.rs                     # Color conversions, pack/unpack, float/half
│   ├── filter.rs                    # 16 predictive filters + heuristics (with SIMD integration)
│   ├── simd.rs                      # AVX2 vectorized filters 1-3 (v1.1+, optional feature)
│   ├── simd_packing.rs              # AVX2 pack/unpack for 1/2/4-bit samples (v1.2+)
│   ├── simd_sample_conversion.rs    # AVX2 8→16/32 expansion, 16/32→8 reduction (v1.2+)
│   ├── shuffle.rs                   # Byte-shuffle (Filter Method=1, v1.1)
│   ├── tonemap.rs                   # HDR tone-mapping (EOTF, primaries, operators, v1.1)
│   ├── interlace.rs                 # Adam7 and even/odd
│   ├── types.rs                     # EncodeOptions, Palette, iDim, cHDR, etc.
│   └── error.rs                     # CafeError
├── tools/
│   ├── cafe-encode.rs    # Encoder CLI
│   └── cafe-decode.rs    # Decoder CLI
├── tests/                # Integration and round-trip tests
├── examples/             # Usage examples
├── docs/                 # Spec, security audit, dev guide
│   ├── CAFE-spec.md      # Complete format specification (v1.1)
│   └── CAFE-spec.pt.md   # Portuguese version of the spec
└── .github/workflows/    # CI (build, clippy -D warnings, fmt, doc, security audit)
```

### Main Dependencies

```toml
image = "0.25"            # Image read/write (PNG, JPEG, WebP, AVIF, etc.)
zstd = "0.13"             # ZSTD compression/decompression
serde_json = "1.0"        # JSON metadata parsing
half = "2.7"              # Half-float (fp16) for sample_format HALF (HDR, v1.0)
crc32fast = "1.3"         # Chunk validation via CRC32
log = "0.4"               # Diagnostic facade (debug/info/warn); consumers choose their own logger
```

---

## Chunk Logical Structure

All chunks follow this layout:

| Field    | Size      | Description |
|----------|-----------|-------------|
| Length   | 4 bytes BE  | Size of Data field |
| Type     | 4 bytes ASCII | Chunk type (e.g., `IHDR`, `IDAT`) |
| Flag     | 1 byte      | Codec: `0x00`=raw, `0x01`=ZSTD |
| Data     | N bytes     | Content (raw or compressed) |
| CRC32    | 4 bytes BE  | Hash of (Type + Flag + Data) |

### Defined Chunks

**Critical (1st letter uppercase):** decoder must understand or reject

| Type | Description | Notes |
|------|-----------|-------|
| `IHDR` | Header (always first, never compressed) | 14 bytes: W, H, bit_depth, sample_fmt, color_type, compression_method, filter_method, interlace_method |
| `PLTE` | Indexed palette (mandatory if color_type=3) | Format: entry_format (1 byte) + colors (N×3 or N×4 bytes). Follows compression fallback (§3.2) |
| `IDAT` | Pixel data (1+ per file) | Self-contained, can be compressed independently |
| `IEND` | End marker (always last, Length=0) | No Data |

**Ancillary (1st letter lowercase):** decoder can safely ignore

| Type | Description | Version |
|------|-----------|--------|
| `eXIF` | EXIF metadata (CIPA standard, opaque blob) | v1.0 ✅ |
| `jSON` | JSON metadata per namespace | v1.0 ✅ |
| `iDIM` | Tiling and scan order for streaming | v1.0 ✅ |
| `cHDR` | HDR metadata (transfer func, luminance) | v1.0 ✅ |
| `iCCP` | ICC profile for color management | v1.0 ✅ |
| `xMPd` | XMP metadata (Extensible Metadata Platform) | v1.0 ✅ |
| `zDIC` | ZSTD dictionary for better IDAT compression | v1.0 ✅ |

---

## Predictive Filters (Section 4.3.1)

Each block/tile (set of lines in an `IDAT`) chooses a single filter, prefixed by 1 byte at the start of the block. Prediction occurs **before** ZSTD compression.

| Code | Name | Prediction | Cost | SIMD (v1.1+) |
|--------|------|----------|-------|------|
| `0` | None | None | O(1) | ✅ (memcpy) |
| `1` | Sub | Byte to the left (L) | O(n) | ✅ AVX2 (4-8x) / NEON |
| `2` | Up | Byte above (U) | O(n) | ✅ AVX2 (4-8x) / NEON |
| `3` | Average | (L + U) / 2 | O(n) | ✅ AVX2 (scalar opt) / NEON (via `vhaddq_u8`) |
| `4` | Paeth | Left, above, or diagonal (UL) | O(n) | ✅ AVX2 / NEON (16-bit widen + blend) |
| `5` | MED | Median Edge Detector (JPEG-LS) | O(n) | ✅ AVX2 / NEON (unsigned min/max) |
| `6` | Gradient | (L + U - UL) mod 256 (JPEG Lossless) | O(n) | ✅ AVX2 / NEON |
| `7` | Simple Median | Simple median of L, U, UL | O(n) | ✅ AVX2 / NEON (unsigned min/max) |
| `8` | 2nd Order | (2L - LL + 2U - UU) / 2 with clamp | O(n), uses LL, UU | ✅ AVX2 / NEON (16-bit widen) |
| `9`-`12` | 4-way Directional | Combinations of L, U, UL with weights | O(n) | ✅ AVX2 / NEON (16-bit widen) |
| `13` | Context-Based | Detects local edge, chooses dynamically | O(n) | ✅ AVX2 / NEON (16-bit widen + blend) |
| `14` | TR-Directional | Bilinear average (L, UL, U, TR) — WebP Predictor 10 | O(n), uses TR | ✅ AVX2 / NEON (via `vhaddq_u8`) |
| `15` | Weighted (adaptive) | Adaptive weighted average of L, U, UL, TR — inspired by JPEG-XL | O(n), state per block | — Scalar (sequential state) |

**SIMD Optimization (v1.1+):**
- Filters 1-14 use **AVX2 intrinsics** on x86_64 CPUs and **ARM NEON intrinsics** on aarch64 CPUs (Filter 15 is scalar-only on every architecture — sequential adaptive-state dependency)
- Processes 32 bytes per SIMD iteration on AVX2, 16 bytes per iteration on NEON (4-8x speedup expected)
- **Feature gate:** `simd` (default: enabled, can be disabled with `--no-default-features`)
- **CPU detection:** AVX2 is detected automatically at runtime (falls back to scalar on non-AVX2 x86_64 CPUs); NEON is dispatched at **compile-time** via `#[cfg(target_arch = "aarch64")]` (NEON is mandatory on ARMv8-A, no runtime feature check needed)
- **Building:** `cargo build --release` (SIMD on), or `cargo build --release --no-default-features` (SIMD off)
- **NEON coverage (v1.3+, aarch64):** All 14 vectorized filters have NEON kernels. All other SIMD modules (`simd_packing.rs`, `simd_sample_conversion.rs`, `simd_quantize.rs`, `simd_shuffle.rs`) also have NEON kernels as of v1.4 — no module is AVX2-only anymore.

**Selection heuristics (encoder decides, not part of spec):**
- Shannon Entropy: Measures redundancy of patterns in residuals (default, `FilterHeuristic::Entropy`)
- MSAD: Sum of absolute values of residuals (fast, classic — `FilterHeuristic::Msad`, configurable via `--filter-heuristic msad`)
- Real compression test: Compresses each candidate (ZSTD) and uses the smallest (costly, `FilterHeuristic::CompressionTest` — configurable via `--filter-heuristic test`)
- QuickPrune (v1.1): Fast MSAD followed by Shannon Entropy on top 8 candidates (~1-2% gain, `FilterHeuristic::QuickPrune`)
- AdaptiveEntropy (v1.1): Block type analysis (Smooth/Natural/HighFreq/Mixed) + content-aware filter selection (~2-3% gain on photos, `FilterHeuristic::AdaptiveEntropy`)

---

## Color Types and Bit Depths

### Supported Mapping

```rust
COLOR_TYPE_GRAY (0)        → bit_depth: 1, 2, 4, 8, 10, 12, 16, 32
COLOR_TYPE_RGB (2)         → bit_depth: 8, 10, 12, 16, 32 (NOT 1,2,4)
COLOR_TYPE_INDEXED (3)     → bit_depth: 1, 2, 4, 8 + PLTE mandatory
COLOR_TYPE_GRAY_ALPHA (4)  → bit_depth: 1, 2, 4, 8, 10, 12, 16, 32
COLOR_TYPE_RGBA (6)        → bit_depth: 8, 10, 12, 16, 32 (NOT 1,2,4) [DEFAULT]
```

### Sample Format (HDR Extension)

| Value | Type | Bytes/Channel | Range |
|-------|------|-------------|-----------|
| `0` | uint | 1, 2, or 4 | [0, 2^bits - 1] |
| `1` | float | 4 (IEEE 754) | [0.0, 1.0] or [0, 65504] |
| `2` | half-float | 2 (fp16) | [0.0, 1.0] or [0, 65504] |

### Bytes Per Pixel (bytes_per_pixel)

Used to calculate stride and locate neighbors in filters:

```rust
fn bytes_per_pixel(color_type, bit_depth) -> Option<usize>
// RGBA, 8-bit → 4 bytes
// GRAY, 1-bit (packed) → 1 byte, but stride calculated specially
// RGB, 16-bit → 6 bytes (3 channels × 2 bytes big-endian)
// ...
```

---

## Data Format in Memory

### Byte Order (Endianness)

- **All multi-byte fields:** big-endian (BE)
- Including: Width, Height, Length, CRC32, floats (IEEE 754 BE)

### Channel Order

- **RGBA (type=6):** R, G, B, A (section 4.1.3)
- **RGB (type=2):** R, G, B
- **GRAY_ALPHA (type=4):** Gray, Alpha
- **GRAY (type=0):** Gray (no separate channel)
- **INDEXED (type=3):** Pixel index (color comes from PLTE)

### Line Order

- Line 0 = top of image
- Lines progress top-to-bottom (PNG convention)

### Sub-byte Packing (bit_depth < 8)

For bit_depth 1, 2, or 4, multiple pixels are packed per byte:

```
MSB [pixel 0 bits][pixel 1 bits]...[padding] LSB
```

**Bytes per line:** `ceil(width × bit_depth / 8)`

Example (bit_depth=1, width=10):
```
10 pixels → 10 bits → ceil(10/8) = 2 bytes/line
Byte 0: [p0][p1]...[p7] (MSB=p0)
Byte 1: [p8][p9][0...0] (padding with zeros)
```

---

## Encoding Flow

```
Input image (PNG, JPEG, etc.)
    ↓
1. Convert to RGBA (via `image` crate)
    ↓
2. Automatic analysis:
   - Detect if grayscale (R=G=B in sampling)
   - Detect if has alpha (via image.color().has_alpha())
   - Detect if few colors (< 256 → INDEXED candidate)
    ↓
3. Convert to target color_type (default or CLI)
    ↓
4. Partition into tiles (or line-by-line, section 4.2)
    ↓
5. For each tile/IDAT:
   a. If Filter method = 2 (predictive):
      - For each block/tile:
        * Test all 16 filters on all lines of the block
        * Choose a single filter via heuristic (MSAD, entropy, or real test)
        * Store 1 byte at start of block indicating the filter used
        * Calculate residuals
   b. If interlace ≠ 0: prefix pass_number (1 byte)
   c. Apply compression fallback (section 3.2):
      - Candidate 1: raw (Flag=0x00)
      - Candidate 2: ZSTD (Flag=0x01), level 19 (default)
      - Use the shorter version
    ↓
6. Write CAFE structure:
   - Signature (9 bytes)
   - IHDR (14 bytes payload, not compressed)
   - iDIM (9 bytes, if 2D tiling)
   - eXIF (if EXIF metadata provided)
   - jSON (multiple instances per namespace)
   - PLTE (if color_type=3)
   - zDIC (if ZSTD dictionary available)
   - IDAT (1 or more, as tiles)
   - IEND (0 bytes payload)
    ↓
7. Validate CRC32 for each chunk
```

## Decoding Flow

```
File .cafe
    ↓
1. Validate signature (9 bytes)
    ↓
2. Read and validate IHDR
   - Extract: width, height, color_type, bit_depth, sample_format
   - Check Compression method (which codecs are in use)
   - Accept filter_method ∈ {0, 1, 2}
   - Reject filter_method=3+ (unknown/critical)
    ↓
3. Read pre-IDAT ancillary chunks (optional)
   - iDIM (if present): set tiling scheme
   - eXIF, jSON, iCCP, xMPd, zDIC
    ↓
4. If color_type=3, read PLTE (mandatory)
    ↓
5. For each IDAT:
   a. Decompress according to Flag
      - Flag=0x00: use as-is (raw)
      - Flag=0x01: ZSTD.decompress() with zDIC if present
      - Enforce: 1 GiB limit per chunk (CWE-409 protection)
   b. If interlace ≠ 0: read 1 byte of pass_number
   c. If filter_method=2: for each block/tile:
      - Read 1 byte indicating the block's filter
      - Reverse prediction on all lines (only the indicated type, don't test all)
   d. Unpack if bit_depth < 8
   e. Uncompact if bit_depth > 8 (big-endian)
   f. Convert to RGBA (via convert_color_type_to_rgba)
    ↓
6. Reconstruct final image in RGBA
    ↓
7. Return image + metadata
```

---

## Reference Implementation

Public entry points and chunk I/O live in `src/cafe.rs`; internal helpers are split across the modules listed above (`color.rs`, `filter.rs`, `codec.rs`, `chunk.rs`, etc.).

### Main Structures

`CafeError` lives in `src/error.rs`; `Palette`, `iDim`, `cHDR`, `EncodeOptions`, `DecodeResult`, `FilterHeuristic`, `PaletteEntry` live in `src/types.rs`. All are re-exported from `src/cafe.rs`.

```rust
pub enum CafeError {
    InvalidSignature,
    CrcMismatch { chunk_type, expected, actual },
    UnsupportedFeature(String),
    TruncatedFile(String),          // Truncated or forged file
    DecompressionLimitExceeded { limit }, // Protection against decompression bomb
    // ... others
}

pub struct Palette {
    entries: Vec<PaletteEntry>,
    has_alpha: bool, // entry_format 0=RGB, 1=RGBA
}

pub struct iDim {
    tile_width: u16, tile_height: u16,
    tiles_x: u16, tiles_y: u16,
    scan_order: u8, // 0=row-major, 1=Z-order (Morton)
}

pub struct cHDR {
    transfer_function: u8,  // 0=linear, 1=PQ, 2=HLG, 3=sRGB
    color_primaries: u8,    // 0=sRGB/BT.709, 1=BT.2020, 2=DCI-P3
    max_luminance: f32,
    min_luminance: f32,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
}
```

### Key Functions

#### Encoder

```rust
pub fn encode(input_path: &str, output_path: &str, opts: &EncodeOptions) -> Result<()>
pub fn encode_indexed(input_path: &str, output_path: &str, opts: &EncodeOptions) -> Result<()>

pub struct EncodeOptions {
    pub use_filter: bool,
    pub level: i32, // 1-22, default 19
    pub adaptive_analysis: bool,
    pub target_color_type: u8,
    pub json_metadata: HashMap<String, Value>,
    pub exif: Option<Vec<u8>>,
}
```

#### Decoder

```rust
pub fn decode(input_path: &str, output_path: &str) -> Result<DecodeResult>
pub fn decode_bytes(buf: &[u8]) -> Result<(Vec<u8>, DecodeResult)> // pixels + metadata

pub struct DecodeResult {
    pub width: u32,
    pub height: u32,
    pub exif: Option<Vec<u8>>,
    pub json_metadata: HashMap<String, Value>,
    pub compression_stats: Option<CompressionStats>,
    pub icc_profile: Option<Vec<u8>>,
    pub xmp_metadata: Option<String>,
    pub zstd_dictionary: Option<Vec<u8>>,
    pub chdr_metadata: Option<cHDR>,
}
```

Note: `DecodeResult` does not carry the pixel buffer itself — `decode_bytes`
returns pixels as the first element of a tuple, and `decode` writes them
directly to `output_path` instead.

#### Streaming Decoder (`Decoder<R: Read>`, v1.5+)

For large images or memory-constrained environments, `Decoder<R: Read>`
decodes tile-by-tile directly off any `Read` source (file, socket, in-memory
`Cursor`) instead of requiring the whole compressed file (`decode_bytes`'s
`&[u8]`) or the whole decoded image to be materialized in memory up front.

```rust
pub struct Decoder<R: Read> { /* ... */ }

impl<R: Read> Decoder<R> {
    pub fn new(reader: R) -> Self
    pub fn with_tonemap_operator(reader: R, tonemap_operator: ToneMapOperator) -> Self
    pub fn read_info(&mut self) -> Result<DecodeInfo>       // reads signature + all pre-IDAT chunks
    pub fn next_tile(&mut self) -> Result<Option<Tile>>     // one IDAT -> one Tile, None at IEND
    pub fn finish(self) -> Result<DecodeResult>             // drains remaining IDATs, returns metadata
}

pub struct DecodeInfo {
    pub width: u32,
    pub height: u32,
    pub color_type: u8,
    pub bit_depth: u8,
    pub sample_format: u8,
    pub supports_streaming_tiles: bool, // false for iDIM (2D tiling) or interlaced files
}

pub struct Tile {
    pub x: u32, pub y: u32, pub width: u32, pub height: u32,
    pub pixels: Vec<u8>, // width * height * 4 bytes RGBA, already color-converted
}
```

**Call order**: `read_info()` exactly once, then `next_tile()` in a loop
until `Ok(None)`, then optionally `finish()` for ancillary metadata.
`next_tile()` returns `UnsupportedFeature` for files with `iDIM` (2D tiling)
or Adam7/even-odd interlacing — check `DecodeInfo::supports_streaming_tiles`
first and fall back to `decode`/`decode_bytes` for those. See
`examples/streaming_decode.rs` for a complete runnable example.

**Implementation note**: built entirely on top of the same private
`DecodeState`/`handle_*_chunk` functions and the CWE-409 decompression
budget used by `decode_bytes_internal` — `chunk::read_chunk_from` (a
`Read`-based counterpart to the slice-based `read_chunk`) is the only new
low-level primitive; everything else (per-tile color conversion, filter
reversal, budget enforcement) is shared, not duplicated. `decode_bytes`/
`decode`/`decode_bytes_internal` themselves are unchanged and still operate
on an in-memory `&[u8]` — `Decoder<R>` is an additional, independent API,
not a replacement.

#### Streaming Encoder (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`, v1.6+)

Symmetric counterpart to `Decoder<R>`: writes `IHDR` and each row-strip
`IDAT` immediately as tiles arrive, instead of requiring the whole image in
memory before `encode()` can produce any output.

```rust
pub struct Encoder<W: Write> { /* ... */ }

impl<W: Write> Encoder<W> {
    pub fn new(writer: W, width: u32, height: u32, opts: &EncoderOptions) -> Result<Self>
    pub fn tile_rows(&self) -> u32                    // suggested, not enforced
    pub fn add_tile(&mut self, rgba_tile: &[u8]) -> Result<()> // width*tile_height*4 bytes RGBA
    pub fn finish(self) -> Result<W>                  // conservative compression_method
}

impl<W: Write + Seek> Encoder<W> {
    pub fn finish_exact(self) -> Result<W>            // patched, exact compression_method
}

pub struct EncoderOptions { /* tile_rows, level, use_filter(_per_row), target_color_type,
                               target_bit_depth, exif, json_metadata, icc_profile,
                               xmp_metadata, zstd_dictionary, sample_format, chdr_metadata,
                               filter_heuristic, use_byte_shuffle */ }
```

**Call order**: `new()` once (writes signature + `IHDR` + pre-IDAT
ancillary chunks immediately), then `add_tile()` any number of times with
row-strip buffers (`width * tile_height * 4` bytes RGBA — `tile_height` is
inferred from the buffer, not required to equal `EncoderOptions::tile_rows`
or be constant across calls), then `finish()`/`finish_exact()` once all
`height` rows have been submitted. See `examples/streaming_encode.rs` for a
complete runnable example.

**Limitations (v1 of this API, see `EncoderOptions`'s doc comment for the
full rationale)**: no indexed palette (`COLOR_TYPE_INDEXED` — palette
quantization needs the whole image upfront; use `encode_indexed()`
instead), no `iDIM` (2D tiling), no interlace (Adam7/even-odd), no
`auto_dictionary` (training needs to sample several tiles before
compressing any). Only row-strip tiling and direct color types
(Gray/RGB/GrayAlpha/RGBA), mirroring `Decoder<R>`'s existing limitation for
symmetry between the two streaming APIs. An explicit, caller-supplied
`zstd_dictionary` remains supported.

**`compression_method` (`IHDR` field, section 4.1) semantics**: `Encoder<W:
Write>` cannot know in advance whether any tile will end up using ZSTD, and
(being `Write`-only) cannot seek back to patch `IHDR` after the fact — so
`finish()` leaves the ZSTD bit set unconditionally, an overestimate that is
always safe (a decoder may reject the file as needing a codec it doesn't
actually need, but never accepts a file it can't actually decompress).
`Encoder<W: Write + Seek>::finish_exact()` instead patches the byte (and
recomputes `IHDR`'s CRC32) to the exact value once every chunk is known,
identical to what `encode()`'s `patch_ihdr_compression_method` produces for
the same pixels/options — verified byte-for-byte in
`tests/streaming_encode.rs::test_streaming_encoder_matches_whole_file_encode_byte_for_byte`.
The CRC recomputation avoids requiring `W: Read` (which `Write + Seek`
alone does not guarantee) by keeping an in-memory copy of the 19
already-written `IHDR` chunk bytes (Type + Flag + Data) from `new()`,
rather than seeking back and re-reading them from `writer`.

**Implementation note**: tiles are compressed sequentially as `add_tile()`
receives them — no rayon parallelism across tiles, unlike `encode()`'s
whole-image path, since there's no independent future work to farm out to
a thread pool when the caller controls the pace of tile submission. Two
helpers were factored out of `encode()`'s tile pipeline to be shared with
`add_tile()` without duplication: `apply_single_tile_filter` (byte-shuffle/
predictive/predictive-per-row/none dispatch for one tile) and
`bytes_per_row_for_direct_color` (stride calculation for direct color
types). `append_common_metadata_chunks`'s signature was also generalized
(primitive parameters instead of `&EncodeOptions`) so both `encode()` and
`Encoder::new()` can call it for `eXIF`/`jSON`/`iCCP`/`xMPd`.

### Color Conversion Functions

Implemented in `src/color.rs`:

```rust
// RGBA → color_type (with bit_depth reduction if necessary)
fn convert_rgba_to_color_type(
    rgba: &[u8],
    width: u32, height: u32,
    target_color_type: u8,
    target_bit_depth: u8,
) -> Result<Vec<u8>>

// Version with sample_format (float/half)
fn convert_rgba_to_color_type_with_format(
    rgba: &[u8],
    width: u32, height: u32,
    target_color_type: u8,
    target_bit_depth: u8,
    sample_format: u8,
) -> Result<Vec<u8>>

// Color_type → RGBA (unpacks, uncompacts, expands)
fn convert_color_type_to_rgba(
    data: &[u8],
    width: u32, height: u32,
    color_type: u8,
    bit_depth: u8,
) -> Result<Vec<u8>>
```

### Predictive Filter Functions

Implemented in `src/filter.rs`:

```rust
// High level (per block/tile): chooses a single filter and prefixes 1 byte
fn apply_predictive_filter(tile_raw, tile_height, bytes_per_row, bpp, heuristic, level) -> Result<Vec<u8>>
fn undo_predictive_filter(tile_data, tile_height, bytes_per_row, bpp) -> Result<Vec<u8>>

// Application/reversal per line (residuals); prev_prev_row only for F_2NDORDER (UU)
fn filter_row(row, prev_row, prev_prev_row, ftype, bpp) -> Vec<u8>
fn unfilter_row(filtered, prev_row, prev_prev_row, ftype, bpp) -> Vec<u8>
fn filter_block(tile_raw, tile_height, bytes_per_row, bpp, ftype) -> Vec<u8>   // F_WEIGHTED with state in flow

// Dispatcher (F_WEIGHTED doesn't go here — requires state in flow)
fn predict(ftype, a, b, c, d, ll, uu) -> u8

// Specific predictors (a=left, b=above, c=diagonal top-left; ll/uu for 2nd order; d=TR)
fn paeth_predictor(a, b, c) -> u8
fn med_predictor(a, b, c) -> u8
fn gradient_predictor(a, b, c) -> u8
fn simple_median_predictor(a, b, c) -> u8
fn four_way_horizontal_predictor(a, b, c) -> u8
fn four_way_vertical_predictor(a, b, c) -> u8
fn four_way_diagonal1_predictor(a, b, c) -> u8
fn four_way_diagonal2_predictor(a, b, c) -> u8
fn context_based_predictor(a, b, c) -> u8
fn second_order_predictor(a, b, ll, uu) -> u8
fn average2(p, q) -> u8
fn tr_directional_predictor(a, b, c, d) -> u8               // F_TR_DIRECTIONAL (14), d=TR
fn weighted_predict(state: &WeightedState, a, b, c, d) -> u8 // F_WEIGHTED (15)
fn weighted_update(state: &mut WeightedState, a, b, c, d, actual) -> ()

// Selection heuristics (not part of decoding contract)
fn shannon_entropy(data) -> f64
fn analyze_tile_complexity(tile_raw) -> f64
fn choose_best_block_filter(tile_raw, tile_height, bytes_per_row, bpp, heuristic, level) -> (u8, Vec<u8>)
```

### Sub-byte Packing/Unpacking

Implemented in `src/color.rs`:

```rust
fn pack_samples_row(
    samples: &[u8],
    bit_depth: u8,
    width: usize,
    bpp: usize,
) -> Result<Vec<u8>>

fn unpack_samples_row(
    packed: &[u8],
    bit_depth: u8,
    width: usize,
    bpp: usize,
) -> Result<Vec<u8>>

fn bytes_per_row_for_bit_depth(width: u32, bit_depth: u8) -> Result<usize>
```

---

## CLI (tools/)

### Encode

```bash
cargo run --bin cafe-encode -- <input> <output.cafe> [options]
```

**Options:**
- `--no-filter` — Disable predictive filter (faster)
- `--byte-shuffle` — Use byte-shuffle (Filter Method=1) for multi-byte samples (bpp ∈ {2,4,8,16}), ideal for float/HDR (v1.1)
- `--filter-heuristic <entropy|msad|test>` — Filter selection heuristic per block (default: entropy)
- `--level <1-22>` — ZSTD compression level (default: 19)
- `--color-type <0|2|4|6>` — Force color type (default: auto-detected)
- `--bit-depth <1-32>` — Target bit depth for uint (default: 8)
- `--adaptive` — Local complexity analysis per tile
- `--indexed` — Force indexed palette
- `--json-file <file>` — JSON metadata
- `--exif-file <file>` — Raw EXIF blob
- `--sample-format <0|1|2>` — Sample format (uint/float/half-float)
- `--chdr-transfer <func>` / `--chdr-primaries <prim>` / `--chdr-max-lum` / `--chdr-min-lum` — HDR metadata (cHDR)
- `--chdr-dict-file <path>` — ZSTD dictionary
- `--interlace <0|1|2>` — Interlace (none/Adam7/Even-Odd)
- `-h`, `--help` — Help

**Example:**
```bash
cargo run --bin cafe-encode -- photo.png photo.cafe --level 22 --color-type 2
```

### Decode

```bash
cargo run --bin cafe-decode -- <input.cafe> <output> [--extract-metadata]
```

**Example:**
```bash
cargo run --bin cafe-decode -- photo.cafe photo.png
```

---

## Security Considerations (Section 12 of Spec)

### 12.1 Input Validation

**Never** panic on untrusted input:
- ❌ Truncated files
- ❌ Forged Length fields (cause overflow)
- ❌ Critical chunks with invalid minimum size
- ❌ Degenerate dimensions (Width=0, Height=0)
- ❌ Inconsistency between IHDR and actual pixel data

**Always** return a handleable error (CafeError).

### 12.2 Protection against Decompression Bomb (CWE-409)

**Mandatory requirement:**

```rust
const MAX_DECOMPRESSED_CHUNK_SIZE: u64 = 1_073_741_824; // 1 GiB

// On decompression:
if decompressed_size > MAX_DECOMPRESSED_CHUNK_SIZE {
    return Err(CafeError::DecompressionLimitExceeded { limit });
}
```

This **is not** optional — it's part of the safe decoding contract.

**Accumulated ceiling per image (IDATs):** in addition to the per-chunk ceiling, the decoder calculates a total decompression budget derived from IHDR (`compute_decompress_budget` in `src/cafe.rs`): `bytes_per_row × height` (+margin), `width × height` for indexed, and `width × height × 4 + passes` for interlace. Each IDAT is decompressed via `decompress_chunk_dict_limited` limited to the remaining budget — multiple IDATs cannot sum to gigabytes when the image is small.

**Tile-count ceiling (`iDIM`, v1.5 round-10 fix):** `MAX_DECOMPRESSED_CHUNK_SIZE` bounds ZSTD *decompression* output; it does **not** bound allocations sized directly from small chunk-header fields with no decompression involved. `iDim::tile_order()` (`src/types.rs`) allocates one `(u16, u16)` tuple per tile (`tiles_x × tiles_y`) up front, from a 9-byte `iDIM` chunk, before any `IDAT` is read — `tiles_x = tiles_y = 65535` (individually valid, and reconcilable against `IHDR` via `tile_width = tile_height = 1`) previously caused a ~17 GiB allocation attempt from a ~71-byte file, aborting the process. `handle_idim_chunk` now rejects `tiles_x as u64 * tiles_y as u64 > MAX_TILE_COUNT` (1,048,576) before calling `tile_order()`. See `docs/SECURITY_AUDIT.md` round 10.

**Palette entry-count ceiling (`PLTE`, v1.5 round-10 fix):** similarly, `read_plte_chunk` (`src/cafe.rs`) now rejects a `PLTE` chunk declaring more than `MAX_PALETTE_ENTRIES` (256 — the maximum any bit depth ∈ {1,2,4,8} can ever address) entries, before allocating the `Vec<PaletteEntry>` — previously bounded only by the generic 1 GiB `MAX_DECOMPRESSED_CHUNK_SIZE`, allowing disproportionate amplification for data no valid pixel index could reference.

### 12.3 Absence of Upper Limit for Width/Height

Intentionally there is no maximum. Decoder should reconstruct **incrementally** from the actual data received in IDAT, not pre-allocate `width × height × bytes_per_pixel` before validation.

### 12.4 Malformed Ancillary Chunks

Never cause panic:
```rust
// ✅ Correct
match read_json_chunk(data) {
    Ok(metadata) => { store_metadata(metadata); }
    Err(_) => { /* Silently ignore, continue */ }
}

// ❌ Incorrect
let metadata = read_json_chunk(data).unwrap(); // Can panic!
```

---

## Testing and Validation

### Current Coverage (section 12.5 of spec)

- ✅ Chunk reading (framing)
- ✅ IHDR header
- ✅ Decompression with/without zDIC dictionary
- ✅ Indexed palette decoding
- ✅ jSON chunk
- ✅ Final image assembly
- ✅ Adversarial validation: truncated files, forged fields, real decompression bomb

### How to Run

```bash
cargo test                           # All tests
cargo test --lib                     # Library tests only
cargo test --release                 # Release mode (faster)
cargo test -- --nocapture           # With output
cargo clippy                         # Linting and warnings
cargo fmt --check                   # Verify formatting
cargo deny check                     # Security and license audit (requires: cargo install cargo-deny)
```

### Fuzzing with cargo-fuzz

Fuzzing ensures that `decode_bytes()` never panics on arbitrary input — it should only ever return `Err(CafeError::...)`. This is critical for handling malicious or malformed files (see section 12, Security Considerations).

**Available harnesses:**

- **`decode_fuzz`**: fuzzes the core `decode_bytes()` function with arbitrary byte sequences.
- **`chunk_roundtrip_fuzz`**: fuzzes chunk parsing (read/write) indirectly via `decode_bytes()`.

**Setup (Linux/macOS only)** — fuzzing requires Rust nightly and a Unix-like OS (libFuzzer is not supported on Windows MSVC; run it on a Linux CI machine or in WSL2, and use property tests instead for local Windows development):

```bash
rustup default nightly
cargo fuzz init   # already done in this repo; for reference only
```

**Running fuzzing:**

```bash
# Local run, ~10 minutes, basic crash search
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10

# Longer run (e.g. overnight/CI), 1+ hours
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10 -max_total_time=3600

# Chunk roundtrip harness
cargo fuzz run chunk_roundtrip_fuzz -- -max_len=16384 -timeout=10
```

**Interpreting results:** a clean run for the specified time with no panics means the code is robust. If libFuzzer finds a crash, it saves a minimal reproducer to `fuzz/artifacts/decode_fuzz/` (or `chunk_roundtrip_fuzz/`):

```bash
xxd fuzz/artifacts/decode_fuzz/<crash_file>                       # inspect the input
cargo fuzz run decode_fuzz fuzz/artifacts/decode_fuzz/<crash_file> # reproduce
cargo fuzz cmin decode_fuzz                                        # minimize the corpus
```

**Local workflow before a long fuzzing run:**

```bash
cargo test --lib
cargo clippy -- -D warnings
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10 -runs=1000000
```

**CI integration (v1.6.3+, implemented)** — `.github/workflows/fuzz.yml` runs a full-hour (configurable via `workflow_dispatch`'s `duration_seconds` input) fuzz run per harness (`decode_fuzz`, `chunk_roundtrip_fuzz`) nightly at 2 AM UTC, plus on-demand. This is separate from `ci.yml`'s own `fuzz` job, which only runs each target for 60s on every push/PR as a fast smoke test — the nightly job trades that speed for depth. On failure, both crash artifacts (`fuzz/artifacts/<target>/`) and the corpus (`fuzz/corpus/<target>/`) are uploaded as workflow artifacts for local reproduction. See "v1.6.3" notes below.

### Property-Based Testing with proptest

Property tests use randomized inputs to find edge cases that hand-written unit tests might miss. CAFE uses `proptest` (`tests/roundtrip_proptest.rs`) to randomize width/height (1..=32), color types and bit depths, sample formats (uint/float/half), interlace methods, and filter heuristics, then verifies pixel round-trip accuracy for each generated configuration.

```bash
# Default case count (usually 64-256)
cargo test --test '*' -- --nocapture

# More cases (slower, more thorough)
PROPTEST_CASES=1000 cargo test --test '*'

# A specific property test, with backtrace on failure
RUST_BACKTRACE=1 cargo test prop_roundtrip_arbitrary_config -- --nocapture
```

When proptest finds a failure, it **shrinks** the failing input to a minimal reproducer, **saves** the seed to `proptest-regressions/` for deterministic re-runs, and **shows** the shrunk input in the error output, e.g.:

```
Error: test failed as expected, and shrinking discovered smaller failing inputs.
The smallest failing input after 45 iterations was:
seed: [12345, 67890]
config: ColorType::GRAY, bit_depth=1, interlace=ADAM7, filter=Entropy
```

If a property test run is too slow locally, reduce the case count: `PROPTEST_CASES=64 cargo test prop_roundtrip`.

### Benchmarking with criterion

`benches/encode_decode.rs` benchmarks `encode()` (levels 1/9/19/22, filter on/off), `decode()` on various image types, `encode_indexed()` on palette images, and PNG-vs-CAFE encoding time for the same image.

```bash
cargo bench                    # run all benchmarks
cargo bench encode_level       # run a specific benchmark
cargo bench -- --sample-size=100  # reduce sample count if a run times out
```

Reports are written to `target/criterion/report/index.html`. After running, consider updating `README.md`'s performance numbers if they've drifted meaningfully.

### Quick Reference

| Goal | Command |
|------|---------|
| Run all tests | `cargo test --lib` |
| Quick lint check | `cargo clippy -- -D warnings` |
| Run fuzzing (local) | `cargo fuzz run decode_fuzz -- -max_len=16384 -runs=100000` |
| Run property tests | `PROPTEST_CASES=256 cargo test prop_` |
| Benchmark | `cargo bench` |
| Build release | `cargo build --release` |
| Encode an image | `cargo run --release --bin cafe-encode -- input.png output.cafe` |
| Decode a CAFE | `cargo run --release --bin cafe-decode -- input.cafe output.png` |

### CLI Parity: `EncodeOptions`/`EncoderOptions` ↔ CLI Flags (updated for v1.6)

This table tracks completeness of CLI flag coverage for `EncodeOptions` fields (batch `encode()`/`encode_indexed()`, via `tools/cafe-encode.rs`). **Goal**: all public library features should be accessible via CLI where practical.

| Field | CLI Flag | Status | Notes |
|-------|----------|--------|-------|
| `use_filter` | `--no-filter` | ✅ | Inverse logic; default=true (filter on) |
| `use_filter_per_row` | — | ❌ **MISSING** | v1.5, `FILTER_METHOD_PREDICTIVE_PER_ROW`; library-only today, no `--filter-per-row`-style flag |
| `use_byte_shuffle` | `--byte-shuffle` | ✅ | v1.1, for HDR/float data |
| `level` | `--level <1-22>` | ✅ | ZSTD compression (default: 19) |
| `adaptive_analysis` | `--adaptive` | ✅ | Local complexity per tile |
| `target_color_type` | `--color-type <0\|2\|4\|6>` | ✅ | 0=GRAY, 2=RGB, 4=GRAY_ALPHA, 6=RGBA |
| `target_bit_depth` | `--bit-depth <d>` | ✅ | 1,2,4,8,10,12,16,32 (uint only) |
| `json_metadata` | `--json-file <path>` | ✅ | Reads JSON from file |
| `exif` | `--exif-file <path>` | ✅ | Raw EXIF binary blob |
| `icc_profile` | `--icc-profile-file <path>` | ✅ | v1.6.1, ICC binary blob |
| `xmp_metadata` | `--xmp-file <path>` | ✅ | v1.6.1, UTF-8 XML/text |
| `idim` | — | ❌ **NOT IMPL** | 2D tiling (internal feature, rare) |
| `interlace_method` | `--interlace <0\|1\|2>` | ✅ | 0=none, 1=Adam7, 2=even/odd |
| `zstd_dictionary` | `--chdr-dict-file <path>` | ✅ | Pre-trained ZSTD dict |
| `sample_format` | `--sample-format <0\|1\|2>` | ✅ | 0=uint, 1=float, 2=half-float |
| `chdr_metadata` | `--chdr-transfer`, `--chdr-primaries`, `--chdr-max-lum`, `--chdr-min-lum` | ✅ | HDR tone-mapping metadata |
| `filter_heuristic` | `--filter-heuristic <h>` | ✅ | entropy, msad, test, quick-prune, adaptive |
| `auto_dictionary` | `--auto-dict` | ✅ | v1.1, auto-train ZSTD dict |
| `palette_algorithm` | `--palette-algorithm <a>` | ✅ | v1.1, nearest (default); median-cut; weighted (v1.5, redmean, scalar-only) |
| `tonemap_operator` | — | ❌ **MISSING** | Encode-side field exists (v1.2.1) but only `cafe-decode`'s `--tonemap-operator` is wired up; no encode-side flag |

`EncoderOptions` (the streaming `Encoder<W>` API, v1.6) is a deliberately smaller struct with no CLI binary of its own — it's a library-only API (`tile_rows`, `level`, `use_filter`/`use_filter_per_row`, `target_color_type`, `target_bit_depth`, `exif`, `json_metadata`, `icc_profile`, `xmp_metadata`, `zstd_dictionary`, `sample_format`, `chdr_metadata`, `filter_heuristic`, `use_byte_shuffle`), so CLI parity doesn't apply to it the same way; see its limitations list in the "Streaming Encoder" section above.

**`DecodeResult` fields accessibility** (`tools/cafe-decode.rs`):

| Field | CLI Export | Status | Notes |
|-------|-----------|--------|-------|
| `width` / `height` | Implicit (output file dimensions) | ✅ | Encoded in the decoded output image |
| `exif` | `--save-exif <path>` (v1.6.2) | ✅ | Byte count always printed; raw bytes saved to a file when the flag is given |
| `json_metadata` | `--extract-metadata` | ✅ | Prints namespace keys always, full contents with `--extract-metadata` |
| `compression_stats` | `--show-stats` (v1.6.2) | ✅ | Real per-chunk original/compressed sizes (see "v1.6.2" notes below), printed as a table with totals + ratio |
| `icc_profile` | `--save-icc-profile <path>` (v1.6.2) | ✅ | Byte count always printed; raw bytes saved to a file when the flag is given |
| `xmp_metadata` | `--save-xmp <path>` (v1.6.2) | ✅ | Byte count always printed; text saved to a file when the flag is given |
| `zstd_dictionary` | `--save-zstd-dict <path>` (v1.6.2) | ✅ | Byte count always printed; raw bytes saved to a file when the flag is given |
| `chdr_metadata` | Printed unconditionally (full detail) | ✅ | Transfer function, primaries, luminance, MaxCLL/MaxFALL all printed |

**Legend**: ✅ complete (CLI flag exists, correct default behavior) · ⚠️ partial (library has it, CLI only surfaces a summary) · ❌ missing (no CLI flag; 2D tiling is intentionally low-priority for CLI exposure since it's a rarely-used internal feature).

### Common Issues

**Fuzzing fails to link on Windows** — libFuzzer requires Unix-like systems; Windows MSVC linking doesn't support the `#[no_main]` entry point libFuzzer needs. Run fuzzing on Linux CI or WSL2; use property tests for local Windows development instead.

**Property test takes too long** — reduce `PROPTEST_CASES` or target a specific test: `PROPTEST_CASES=64 cargo test prop_roundtrip`.

**Criterion benchmarks time out** — reduce the sample count: `cargo bench -- --sample-size=100`.

### Contribution Checklist

Before submitting a PR:

- [ ] `cargo test --lib` passes
- [ ] `cargo clippy -- -D warnings` passes (no warnings)
- [ ] `cargo fmt --check` passes (code is formatted)
- [ ] New functionality has a test
- [ ] Fuzzing harnesses still compile (Linux/nightly: `cargo fuzz build`)
- [ ] No breaking changes to `.cafe` binary format
- [ ] `README.md` updated if adding public APIs
- [ ] Performance-sensitive changes run `cargo bench`

---

## Version Roadmap

| Version | Functionality | Status |
|--------|---|---|
| v1.0 | IHDR, IDAT, IEND, ZSTD, Filters 0-13 (Shannon Entropy or real compression test), iDIM (tiling), Adam7, even/odd, indexed PLTE, eXIF, jSON, iCCP, xMPd, cHDR, zDIC, sample_format (uint/float/half), bit depths 1-32, security audit | ✅ |
| v1.1 | Filters 14-15 (TR-Directional WebP Predictor 10 and adaptive Weighted inspired by JPEG-XL), 16 total predictors, MSAD heuristic, real 2D tiling (iDIM) with end-to-end round-trip, **byte-shuffle (Filter method=1) complete encode+decode (bpp ∈ {2,4,8,16})**, **HDR tone-mapping on decode** (EOTF PQ/HLG/sRGB, color primaries conversion via XYZ, Reinhard/Filmic operators), **AVX2 SIMD for Filters 1-3 (4-8x speedup)** | ✅ |
| **v1.2** | **Aggressive SIMD Acceleration (AVX2 x86_64)**: Pack/Unpack 1/2/4-bit samples (8-16x), Sample expansion/reduction 8→16/32 (4-6x), **Byte-shuffle blocking** (10-20% cache improvement), **Improved Filter 3 Average** (4-6x), **203 tests** (197 unit + 6 integration roundtrip), **Zero TODOs/FIXMEs**, **Comprehensive benchmarks** (Criterion-ready), Feature-gated SIMD with CPU detection | ✅ |
| **v1.3** | **ARM NEON SIMD (aarch64)**: all 14 vectorized filters (Sub, Up, Average, Gradient, 4-way Directional, Paeth, MED, Simple Median, 2nd Order, Context-Based, TR-Directional) ported to NEON intrinsics, compile-time dispatch via `#[cfg(target_arch = "aarch64")]` (no runtime check needed, NEON is ARMv8-A baseline), 273 unit tests + 7 integration tests still passing on x86_64, cross-compile validated via `cargo check`/`cargo clippy --target aarch64-unknown-linux-gnu` | ✅ (Filters 1-14) |
| **v1.4** | **ARM NEON SIMD extended to all remaining modules**: `simd_packing.rs` (1/2/4-bit pack/unpack), `simd_sample_conversion.rs` (8↔16-bit expand/reduce, RGBA→luma8), `simd_shuffle.rs` (byte-shuffle via `vqtbl1q_u8`), `simd_quantize.rs` (nearest-palette search via widened `i32` distance + `vminvq_s32` reduction) — no SIMD module is AVX2-only anymore, 273 unit tests + 7 integration tests still passing on x86_64, cross-compile validated via `cargo check`/`cargo clippy --target aarch64-unknown-linux-gnu` | ✅ |
| **v1.4.1** | **Real ARM execution validation (QEMU emulation via Docker)**: ran the full test suite natively on aarch64 for the first time (not just `cargo check`/`clippy` cross-compile) — found and fixed a real index-calculation bug in `simd_quantize.rs`'s NEON path that cross-compilation could never have caught (see "v1.4.1" notes below) | ✅ |
| **v1.4.2** | **CI: ARM64 Cross-Compile Check job** — new `aarch64-cross-compile` job in `.github/workflows/ci.yml` runs `cargo check`/`cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` on every push/PR (Ubuntu runner + `gcc-aarch64-linux-gnu` cross-compiler, no `zig cc` needed since `apt` provides a native GNU cross-toolchain in CI), preventing future aarch64 regressions from merging unnoticed | ✅ |
| **v1.5** | **Compression-focused audit (5 items)**: per-row predictive filter (`Filter method=3`, finer-grained adaptation than per-tile), real compression benchmarks + CI regression gate (`tests/compression_regression.rs`, `benches/encode_decode.rs`), `auto_dictionary` non-regression guarantee (never emits a `zDIC`-using encode larger than the no-dictionary equivalent), perceptually-weighted palette quantization (`PaletteAlgorithm::NearestNeighborWeighted`, redmean distance), `DEFAULT_TILE_ROWS` retuning investigation (benchmarked, kept at 64 — see "v1.5" notes below) | ✅ |
| **v1.6** | **Streaming Encoder** (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`, symmetric counterpart to v1.5's `Decoder<R: Read>`): writes `IHDR` + ancillary chunks + row-strip `IDAT`s incrementally as tiles arrive instead of requiring the whole image in memory first; `finish()` leaves `compression_method`'s ZSTD bit conservatively set (safe overestimate) for `Write`-only destinations, `finish_exact()` patches it to the exact value (byte-for-byte identical to `encode()`) when `W` also supports `Seek` — see "v1.6" notes below | ✅ |
| **v1.6.1** | **CLI: `--icc-profile-file`/`--xmp-file` flags for `cafe-encode`** — closes a CLI-parity gap: `EncodeOptions::icc_profile`/`xmp_metadata` already existed in the library and were written correctly by `encode()`/`encode_indexed()`, but had no CLI flag to populate them — see "v1.6.1" notes below | ✅ |
| **v1.6.2** | **Real `compression_stats` + `cafe-decode` metadata-export flags**: `DecodeResult::compression_stats` now populated with real per-chunk original/compressed sizes (previously always `None`); new `--show-stats`, `--save-exif`, `--save-icc-profile`, `--save-xmp`, `--save-zstd-dict` flags on `cafe-decode` — see "v1.6.2" notes below | ✅ |
| **v1.6.3** | **CI: nightly fuzz workflow** — new `.github/workflows/fuzz.yml` runs `decode_fuzz`/`chunk_roundtrip_fuzz` for a full hour nightly (plus on-demand via `workflow_dispatch`), separate from `ci.yml`'s existing 60s-per-push smoke test job — see "v1.6.3" notes below | ✅ |
| Future | Real hardware validation on physical ARM devices, additional compressors, k-means palette, tone-mapping on encode (SDR→HDR), operator selection via CLI | ⏳ |

---

## Performance and Optimizations

### SIMD Acceleration (v1.1+ → v1.2+)

**What's Vectorized (v1.2):**
- **Filter 1 (Sub)**: `pixel - left`, 32 bytes/iteration with AVX2 (v1.1)
- **Filter 2 (Up)**: `pixel - above`, 32 bytes/iteration with AVX2 (v1.1)
- **Filter 3 (Average)**: `pixel - (left + above) / 2`, 4-6x AVX2 via unpacklo/unpackhi (v1.2)
- **Pack 1-bit**: 256 pixels/iteration, 8-16x speedup (v1.2)
- **Pack 2-bit**: 128 pixels/iteration, 7-10x speedup (v1.2)
- **Pack 4-bit**: 64 pixels/iteration, 5-7x speedup (v1.2)
- **Unpack 1/2/4-bit**: Symmetric speedups with AVX2 bit extraction (v1.2)
- **Sample Expansion**: 8→16, 8→32 float with AVX2 unpack operations (v1.2)
- **Sample Reduction**: 16→8, 32→8 with AVX2 shuffle/extract (v1.2)
- **Byte-Shuffle**: Cache-friendly 1024-pixel blocking (10-20% improvement on large images) (v1.2)

**Building with SIMD:**
```bash
# Default (SIMD enabled on x86_64, AVX2 detected at runtime)
cargo build --release

# Disable SIMD for portability
cargo build --release --no-default-features
```

**Runtime dispatch:** AVX2 detection happens via `is_x86_feature_detected!("avx2")` at runtime, so the same binary automatically uses AVX2 on capable CPUs and falls back to scalar code otherwise. No `RUSTFLAGS` or special build flags are needed to enable AVX2 — it works out of the box with the default `cargo build --release`.

**How to Verify SIMD is Working:**
- On AVX2 systems: 2.8-3.5x overall speedup on typical mixed workloads
- Filter processing: 4-8x faster (Filters 1-3)
- Pack/unpack: 5-16x faster (1/2/4-bit samples)
- Falls back gracefully on non-AVX2 CPUs (no runtime penalty, just slower)
- Run `cargo test --lib` (197 tests) + `cargo test --test integration_roundtrip` (6 integration tests) to verify

### Known Bottlenecks

1. **Filter heuristic:** Testing all 16 filters is O(16n) per block/tile
   - Solution: Shannon entropy (cheaper than real compression)
   - **SIMD helps**: Filters 1-3 are now 4-8x faster
   - Future: Smart heuristic that skips unlikely filters

2. **Decompression without dictionary:** ZSTD is slow without context
   - Solution: zDIC chunk for small images

3. **Sub-byte packing:** Lots of bit-by-bit arithmetic
   - Solution: SIMD/vectorization (partially done for filters; can extend to pack/unpack)

### Performance Tips

```rust
// ✅ Use level=1 for fast encode (still good compression)
// ✅ Use --no-filter for noisy images (doesn't compress well)
// ✅ Use --indexed for images with < 256 colors (70-90% smaller)
// ❌ Avoid level=22 (maximum compression) for large images
```

---

## Welcome Contributions

### High-Potential Areas

1. **SIMD for sub-byte packing** — Extend AVX2 to `pack/unpack_samples_row` (currently scalar)
2. **Real ARM hardware validation on physical devices** — QEMU emulation (v1.4.1) already caught and fixed one real NEON bug (see "v1.4.1" notes below); running the suite on actual ARM64 hardware (Raspberry Pi, mobile, Apple Silicon) would add confidence beyond emulation (e.g. timing-sensitive or alignment-sensitive behavior QEMU might not reproduce exactly)
3. **Advanced 2D tiling** — iDIM with per-tile IDAT already implemented (row-major and Z-order); evolve with preview/progressive streaming
4. **Optimized interlace** — Adam7 and even/odd already supported; optimize progressiveness and SIMD of passes
5. **Optimized indexed palette** — `NearestNeighbor`, `MedianCut`, and a perceptually-weighted (`NearestNeighborWeighted`, redmean distance, v1.5) variant already exist; could still use k-means clustering for the palette-building step itself (all three current algorithms use either greedy incremental collection or median-cut bucket splitting, not iterative clustering)
6. **Automatic ZSTD dictionary** — Train dictionary for small images
7. **Tone-mapping on encode (SDR→HDR)** — Inverse of decode; also operator selection (Reinhard/Filmic) via CLI
8. **In-depth tone-mapping** — Validate color conversions in real HDR scenarios; look-up tables
9. **More robust tests** — Adversarial files, fuzzing
10. **Benchmarking** — Performance vs PNG, JPEG, WebP

---

## Quick References

### Useful Commands

```bash
# Build
cargo build --release

# Test an image
cargo run --release --bin cafe-encode -- encode input.png output.cafe
cargo run --release --bin cafe-decode -- decode output.cafe output.png

# Compare sizes
ls -lh input.png output.cafe

# Lint and format
cargo clippy && cargo fmt

# Generate documentation
cargo doc --open
```

### Specification Links

- **Section 2** — File signature
- **Section 3** — Chunk structure
- **Section 3.2** — Compression fallback (flag)
- **Section 4** — Defined chunks (IHDR, IDAT, eXIF, jSON, etc.)
- **Section 4.3.1** — Predictive filters (all 16 types)
- **Section 5** — Interlacing (Adam7, even/odd)
- **Section 12** — Security considerations (CWE-409, validation)

---

**Last updated:** September 3, 2026 | **Project version:** v1.6.3 | **ARM NEON SIMD Phase (Sep 1/2026) + Compression-Focused Audit (Sep 2/2026) + Streaming Encoder (Sep 3/2026) + CLI ICC/XMP flags (Sep 3/2026, v1.6.1) + Real compression_stats + cafe-decode export flags (Sep 3/2026, v1.6.2) + Nightly fuzz CI workflow (Sep 3/2026, v1.6.3):**

### v1.6.3 - CI: nightly fuzz workflow

Closes the last remaining item from the "CI integration (recommended)" note under "Fuzzing with cargo-fuzz" above: `.github/workflows/fuzz.yml` now exists and runs automatically, instead of being a documented-but-unimplemented YAML snippet.

- **New workflow**: `.github/workflows/fuzz.yml`, separate from `ci.yml`'s existing `fuzz` job (which runs each target for only 60s on every push/PR as a fast smoke test — unchanged by this addition). The new job runs the **same two harnesses** (`decode_fuzz`, `chunk_roundtrip_fuzz`) for a full hour each (`-max_total_time=3600`, configurable per-run via `workflow_dispatch`'s `duration_seconds` input), on a `matrix.target` strategy with `fail-fast: false` so one harness crashing doesn't cancel the other's run.
- **Triggers**: `schedule: cron: '0 2 * * *'` (nightly at 2 AM UTC) plus `workflow_dispatch` for on-demand manual runs (e.g. after a suspicious change to decode-path code, without waiting for the next scheduled run).
- **Toolchain/setup**: mirrors `ci.yml`'s existing `fuzz` job exactly — `dtolnay/rust-toolchain@nightly` (cargo-fuzz requires nightly), `Swatinem/rust-cache@v2` scoped to `fuzz -> target` (the fuzz harnesses are a separate Cargo workspace member with their own `Cargo.toml`/lockfile-adjacent target dir), `taiki-e/install-action@v2` to install `cargo-fuzz` itself, and `cargo fuzz run --target x86_64-unknown-linux-gnu <target> -- ...` run from the `fuzz` working directory.
- **Artifact uploads on failure/always**: `actions/upload-artifact@v4` uploads `fuzz/artifacts/<target>/` (crash reproducers, `if: failure()`, `if-no-files-found: ignore` since a clean run produces none) and, unconditionally (`if: always()`), `fuzz/corpus/<target>/` (the accumulated corpus of interesting inputs libFuzzer discovered, useful for seeding future local `cargo fuzz cmin`/reproduction even on a successful run — `if-no-files-found: ignore` since first-ever runs may not have generated a corpus dir yet either).
- **`timeout-minutes: 90`** (vs. `ci.yml`'s `fuzz` job's default): gives each hour-long fuzz run headroom for toolchain setup/dependency compilation before the job itself would be forcibly killed by GitHub Actions' own default, without being so generous that a genuinely hung run wastes CI minutes indefinitely.
- **Docs updated**: `AGENTS.md`'s and `docs/DEVELOPER_GUIDE.md`'s "Fuzzing with cargo-fuzz" sections — the `**CI integration (recommended, not yet implemented)**` paragraph with its inline YAML snippet was replaced with a short `**CI integration (v1.6.3+, implemented)**` paragraph pointing at the real file instead of duplicating its contents (avoiding two sources of truth for the same workflow).

**Validation:** workflow YAML syntax checked with `rhysd/actionlint` (via Docker, `docker run --rm -v <repo>:/repo -w /repo rhysd/actionlint`) against both `ci.yml` and the new `fuzz.yml` together — zero errors/warnings for either file. `cargo build --lib`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib` (312 tests, unchanged — this item touches no Rust source) all still pass, confirming the version bump and doc-only changes introduced no regressions. The workflow's actual scheduled/triggered execution was not exercised end-to-end (that requires pushing to trigger a real GitHub Actions run, or waiting for the nightly cron) — `actionlint`'s syntax/schema validation plus manual review against `ci.yml`'s already-proven-working `fuzz` job (identical toolchain/action versions, only the trigger and duration differ) is the validation ceiling achievable without pushing.

### v1.6.2 - Real `compression_stats` + `cafe-decode` metadata-export flags

Closes two related gaps: `DecodeResult::compression_stats` (`Option<CompressionStats>`) existed in the library since v1.0 but was **always `None`** — nothing ever populated it — and `cafe-decode` had no way to write extracted `exif`/`icc_profile`/`xmp_metadata`/`zstd_dictionary` blobs to separate files (only byte counts were printed).

- **`DecodeState` (`src/cafe.rs`) gained a `chunk_stats: Vec<ChunkStats>` field** (empty by default), and a new `record_chunk_stats(state, chunk_type: &[u8; 4], original_size, compressed_size)` helper appends one entry per chunk actually decoded.
- **Instrumented handlers**: `handle_plte_chunk` (an extra `decompress_chunk` call purely to measure the decompressed size — payloads are small, overhead is negligible; avoids changing `read_plte_chunk`'s return signature), `handle_exif_chunk`, `handle_json_chunk`, `handle_iccp_chunk`, `handle_xmpd_chunk`, `handle_zdic_chunk`, and `decompress_idat_payload` (the single shared function behind **every** `IDAT` consumption path — whole-image `decode_bytes_internal`, `Decoder<R>::next_tile()` via `decode_idat_chunk_as_tile_row_strip`, and `Decoder<R>::finish()`'s drain loop — so instrumenting it once covers all three call sites without duplication). `handle_chdr_chunk` is deliberately **not** instrumented: `cHDR` isn't a simple decompressed byte blob like the others, so "original/compressed size" doesn't map cleanly onto it.
- **`decode_bytes_internal`**: replaced the permanent `let compression_stats = None;` with real aggregation — sums `chunk_stats` into `CompressionStats { total_original, total_compressed, chunks }`, still `None` only in the (currently unreachable in practice) case of an empty `chunk_stats` vec, since every valid CAFE file has at least one `IDAT`, which is always recorded.
- **`Decoder<R>::finish()`**: the same aggregation logic applied to `self.state.chunk_stats`, correctly covering `IDAT`s regardless of whether they were consumed via `next_tile()` calls before `finish()` or drained by `finish()` itself — both paths go through the same instrumented `decompress_idat_payload`.
- **`tools/cafe-decode.rs`** gained five new flags: `--show-stats` (prints `compression_stats` as a table — one line per chunk `[TYPE] orig -> comp bytes`, plus totals and overall ratio; prints a "no chunk statistics were recorded" message if somehow absent) and `--save-exif <path>` / `--save-icc-profile <path>` / `--save-xmp <path>` / `--save-zstd-dict <path>` (each writes the corresponding `DecodeResult` field's raw bytes to a file when present, using a shared `require_arg_value` argument-parsing helper mirroring `cafe-encode.rs`'s pattern; a "not found" message is printed if the field is absent instead of silently no-op'ing). Default behavior (no new flags passed) is byte-for-byte unchanged from v1.6.1.
- **New test**: `test_decode_bytes_populates_compression_stats` (`src/cafe.rs`) encodes a 32×20 PNG with `tile_rows: 8` plus an EXIF blob, decodes it, and asserts `compression_stats` is `Some`, contains `"IDAT"` and `"eXIF"` chunk-type entries, and that the summed per-chunk sizes match the aggregated totals exactly.

**Validation:** `cargo build --lib`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib` (312 tests, +1 from the new test) all pass with zero regressions; all pre-existing integration suites re-run manually and still pass (`integration_roundtrip`, `integration_test`, `decode_robustness`, `streaming_encode`, `dictionary_regression`, `auto_dictionary_test`, `palette_algorithm_test`, `roundtrip_formats`, `simd_integration`, `compression_regression`). Manually verified end-to-end via release binaries: encoded a 24×24 PNG with `--exif-file`/`--icc-profile-file`/`--xmp-file`, decoded with `--extract-metadata --show-stats --save-exif --save-icc-profile --save-xmp`, confirmed saved bytes are identical to the originals and printed stats are internally consistent (e.g. `[PLTE] 1025 -> 524 bytes`, `[IDAT] 577 -> 55 bytes`); also verified the no-metadata case (correct "not found" messages) and the no-new-flags case (output identical to pre-v1.6.2 behavior).

### v1.6.1 - CLI: `--icc-profile-file`/`--xmp-file` flags for `cafe-encode`

Closes a CLI-parity gap flagged in the "CLI Parity" table below: `EncodeOptions::icc_profile` (`Option<Vec<u8>>`, raw ICC binary blob) and `EncodeOptions::xmp_metadata` (`Option<String>`, UTF-8 XML/text) already existed in the library and were written correctly to the `iCCP`/`xMPd` chunks by `encode()`/`encode_indexed()`, but `tools/cafe-encode.rs` had no flag to populate them from the CLI — the only way to set them was via the library API directly.

- **`--icc-profile-file <path>`**: reads the file as raw bytes (`std::fs::read`), identical pattern to the existing `--exif-file`/`--chdr-dict-file` flags.
- **`--xmp-file <path>`**: reads the file as UTF-8 text (`std::fs::read_to_string`), since `xmp_metadata` is `Option<String>` (XMP is XML/text, unlike the binary EXIF/ICC/dictionary blobs).
- Both print a byte-count confirmation line (`ICC profile: N bytes` / `XMP metadata: N bytes`) in `cafe-encode`'s summary output, matching the existing pattern for `--exif-file`/`--chdr-dict-file`.
- No library changes — purely a CLI wiring gap closed in `tools/cafe-encode.rs`. `cafe-decode`'s existing `--extract-metadata` flag already printed ICC/XMP byte counts on the read side; this only closes the write side.

**Validation:** `cargo build --lib`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (full suite) all pass with zero regressions. Manually verified end-to-end: encoded a generated 16×16 test PNG with both new flags pointing at a 20-byte dummy ICC blob and a 15-byte XMP string, decoded the result with `cafe-decode --extract-metadata`, and confirmed both byte counts matched the input files exactly.

### v1.6 - Streaming Encoder (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`)

Symmetric counterpart to v1.5's `Decoder<R: Read>` (see "Streaming Decoder" under "Reference Implementation" above): writes `IHDR` and each row-strip `IDAT` immediately as tiles arrive via `add_tile()`, instead of requiring `encode()`'s whole-image-in-memory path before any output can be produced.

- **`EncoderOptions`** (`src/types.rs`): a deliberately smaller sibling of `EncodeOptions`, omitting `auto_dictionary` (needs to sample several tiles before compressing any — incompatible with incremental submission), `idim`/2D tiling (needs the full tile grid upfront), `interlace_method` (Adam7/even-odd need the whole image's pixels to interleave), and indexed-palette support (`target_color_type` restricted to direct color types — quantization needs to see every pixel before a single index can be emitted; `encode_indexed()` remains the only path for `COLOR_TYPE_INDEXED`).
- **`Encoder::new(writer, width, height, opts)`**: validates dimensions/color-type/filter combinations (same rules as `encode()`), writes the signature + `IHDR` + all pre-`IDAT` ancillary chunks (`cHDR`/`eXIF`/`jSON`/`iCCP`/`xMPd`/`zDIC`) immediately.
- **`Encoder::add_tile(rgba_tile)`**: infers tile height from the buffer's length (`len / (width*4)`), so callers may submit irregular tile sizes rather than being locked to `EncoderOptions::tile_rows` (a suggestion only, exposed via the `tile_rows()` getter). Compresses and writes one `IDAT` per call, sequentially (no rayon parallelism across tiles, unlike `encode()`'s whole-image path — there's no independent future work to farm out when the caller controls submission pace).
- **`Encoder::finish()` / `Encoder::finish_exact()`**: both require every declared row to have been submitted first (`UnsupportedFeature` otherwise, preventing a silently-truncated image from being accepted as valid). `finish()` (`W: Write`) leaves `IHDR`'s `compression_method` ZSTD bit conservatively set from `new()` — an intentional overestimate, since a `Write`-only destination cannot seek back to correct it, and overestimating is the only safe direction for that field (see spec section 4.1's new note). `finish_exact()` (`W: Write + Seek`) instead patches the byte to its exact, non-conservative value and recomputes `IHDR`'s CRC32, matching `encode()`'s own `patch_ihdr_compression_method` byte-for-byte.
- **Avoiding a `Read` bound on `finish_exact()`**: the first implementation attempt re-read the already-written stream (via a `read_chunk_from`-based scan) to determine whether any chunk used ZSTD — but `W: Write + Seek` does not guarantee `Read` (e.g. some socket/pipe wrappers), so that approach didn't compile in the general case. Fixed by tracking `uses_zstd: bool` incrementally as a struct field (updated in both `new()`, for ancillary chunks, and `add_tile()`, for each `IDAT`'s actual compression outcome) and keeping an in-memory copy of the 19 already-written `IHDR` chunk bytes (`ihdr_type_flag_data: [u8; 19]`, Type+Flag+Data) from `new()` — `finish_exact()` patches the copy and recomputes the CRC32 purely from memory, then seeks only to *write* (never read) the two patched spans.
- **Code reuse with `encode()`**: two helpers were extracted from `encode()`'s existing tile pipeline specifically so `add_tile()` could reuse them without duplicating logic — `apply_single_tile_filter` (byte-shuffle/predictive/predictive-per-row/none dispatch for a single tile) and `bytes_per_row_for_direct_color` (stride calculation for direct color types). `append_common_metadata_chunks`'s signature was also generalized to take primitive parameters instead of `&EncodeOptions`, so both `encode()`/`encode_indexed()` and `Encoder::new()` can call it.
- **Testing**: `tests/streaming_encode.rs` (17 tests) covers pixel-exact round-trips through both `finish()` and `finish_exact()`, the conservative-vs-exact `compression_method` byte in each case, irregular/variable tile heights, every documented error path (non-multiple-of-row-width tile, single-call and cumulative height overflow, incomplete `finish()`/`finish_exact()`, `COLOR_TYPE_INDEXED` rejection, zero dimensions, unsupported per-row heuristic), non-default color types (Gray) and byte-shuffle/per-row-filter through the streaming path, and — most importantly — a byte-for-byte comparison (`test_streaming_encoder_matches_whole_file_encode_byte_for_byte`) confirming `Encoder<W>::finish_exact()`'s output is bit-identical to `encode()`'s whole-file output for the same pixels/options, not just pixel-equivalent after decoding.
- **Example**: `examples/streaming_encode.rs`, symmetric to `examples/streaming_decode.rs`, demonstrates feeding row-strip tiles read from a whole-image `image::open()` call (a real streaming producer would instead hand tiles to `add_tile()` as they become available, e.g. from a renderer or another format's `Decoder<R>`).
- **Docs**: `docs/CAFE-spec.md`/`docs/CAFE-spec.pt.md` gained a new subsection 6.1 (streaming encode) and a note under section 4.1's `Compression method` field explaining the conservative-overestimation semantics for non-seekable destinations.

**Validation:** `cargo build --lib`, `cargo test` (full suite — 311 lib tests + all integration suites including the new 17-test `streaming_encode.rs`, plus doc-tests), `cargo clippy --tests -- -D warnings`, `cargo fmt --check` all pass with zero regressions. `cargo run --example streaming_encode` manually verified end-to-end against a generated PNG, decoded back via `cargo run --example basic_decode` to confirm output byte size matches the source.

### v1.5 - Compression-focused audit (5 items)

A comparative audit of the CAFE algorithm was done to separate genuine compression gains from mere engineering/performance gains, yielding 5 prioritized improvements. Items #1-#4 are documented in detail inline (doc comments in `src/types.rs`, `src/cafe.rs`, `src/codec.rs`, `src/filter.rs`, `src/constants.rs`) and in `docs/CAFE-spec.md`/`docs/CAFE-spec.pt.md` (sections 4.1.2, 4.3.1.1, 4.9, 10); a summary of each, plus item #5's investigation in full, follows:

1. **Per-row predictive filter** (`FILTER_METHOD_PREDICTIVE_PER_ROW = 3`, section 4.3.1.1 of the spec) — finer-grained filter adaptation than the existing per-tile filter (one filter byte per row instead of one per whole tile), at the cost of one extra byte per row before compression.
2. **Real compression benchmarks + CI regression gate** — `tests/compression_regression.rs` asserts compressed size stays within tolerance across representative content types on every CI run; `benches/encode_decode.rs` (Criterion) gives detailed timing/ratio profiles for manual analysis.
3. **`auto_dictionary` non-regression guarantee** — an auto-trained ZSTD dictionary is only emitted (and only used) when doing so produces a strictly smaller file than the no-dictionary equivalent, checked both per-`IDAT` (`src/codec.rs::compress_with_fallback_dict`) and whole-file (`src/cafe.rs::encode`, comparing `zDIC` chunk + IDATs vs. no-dict IDATs). `tests/dictionary_regression.rs` guards this across 13 pattern/size/tile_rows/level combinations. A caller-supplied dictionary (not auto-trained) is always honored unconditionally, since that's a deliberate choice by the caller.
4. **Perceptually-weighted palette quantization** — `PaletteEntry::redmean_distance` (`src/types.rs`) implements the "redmean" approximation of human color perception (<https://www.compuphase.com/cmetric.htm>) as an integer-only, sqrt-free formula; wired into a new opt-in `PaletteAlgorithm::NearestNeighborWeighted` variant (`src/cafe.rs::quantize_nearest_neighbor_weighted`), deliberately scalar-only (the redmean weight depends on `(r1+r2)/2` per comparison, unlike the fixed-weight Euclidean distance the existing SIMD-accelerated `NearestNeighbor` path uses via `PaletteSoa`). Existing `NearestNeighbor`/`MedianCut` behavior is unchanged — this is purely additive.
5. **`DEFAULT_TILE_ROWS` retuning investigation** — see below.

**Item #5 investigation and conclusion (`DEFAULT_TILE_ROWS`, `src/constants.rs`):**

The audit's premise was "benchmark first, decide later" — no production code was changed until data was collected. Three data sets were gathered, all in `tests/tile_rows_benchmark.rs`:

- **Compression-size sweep** (`tile_rows_sweep_by_content_type`): 5 content types (checkerboard, gradient, repetitive4color, photo, vertical_bands) at 256×256 and 1024×1024, with and without per-row filtering, across `tile_rows ∈ {4,8,16,32,64,128,256}`. **Every case monotonically favored larger `tile_rows`** — no content type or size reversed the trend.
- **Extreme-value probe** (`tile_rows_extreme_values_probe`): extended to `tile_rows` up to `100000` (i.e., no tiling — one `IDAT` for the whole image), on content deliberately crafted to reward small tiles (an abrupt gradient→checkerboard transition at the vertical midpoint of a 256×256 image) and on a large 2048×2048 gradient. **The trend never reversed** even at this extreme — compressed size kept improving (or plateaued) all the way to "no tiling at all," meaning there is no compression-only sweet spot smaller than "as large as possible."
- **Speed-vs-size probe** (`tile_rows_speed_vs_size_tradeoff`): since `src/cafe.rs` parallelizes tile compression across a rayon thread pool, fewer/bigger tiles means less independent work to spread across cores. Measuring **wall-clock encode time** (not just size) revealed a clear U-shaped curve: too many small tiles adds per-tile scheduling/framing overhead; too few huge tiles leaves each individual ZSTD-19 call large and largely serial, starving a multi-core machine of parallel work. Tested on both a 24-core machine and a 4-core-limited run (`RAYON_NUM_THREADS=4`): in both cases the time-vs-`tile_rows` minimum fell in the `64..=128` range, very close to the existing default. Concretely, on a 1024×1024 photo-like image (24 cores): `tile_rows=64` → 1,203,271 bytes / ~250ms, vs. `tile_rows=1024` → 1,070,921 bytes / ~2,820ms — an ~11% size improvement costs an ~11x slowdown once tiles get large enough to run out of parallel work.

**Decision: `DEFAULT_TILE_ROWS` stays at `64`.** Compression ratio alone would favor an arbitrarily large tile size (or no tiling), but that ignores encode-time cost: `64` already sits at or very near the empirical minimum of the time-vs-`tile_rows` curve on both many-core and few-core machines, while staying within single-digit-to-low-double-digit percent of the best observed compressed size at each tested dimension. Streaming granularity (spec sections 4.2/6 — each `IDAT` independently decodable) is a secondary benefit of not going arbitrarily large. This is a "keep + document the trade-off" outcome, not a code change; the trade-off itself is now documented quantitatively in `docs/CAFE-spec.md`/`docs/CAFE-spec.pt.md` section 10 and in `tests/tile_rows_benchmark.rs`'s module doc comment.

**Validation:** `cargo build --release`, `cargo test` (full suite, including all three `tile_rows_benchmark.rs` tests and all pre-existing tests), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` all pass with zero regressions.

### v1.3.0 - ARM NEON SIMD (aarch64)

**NEON Vectorization (Filters 1-3, `src/simd.rs`):**
- `filter_sub_avx2` / `filter_up_avx2` / `unfilter_up_avx2` / `filter_average_avx2` now dispatch to NEON kernels on aarch64, compile-time gated via `#[cfg(target_arch = "aarch64")]` (no runtime feature check needed — NEON is mandatory on ARMv8-A, unlike AVX2 which is optional on x86_64)
- Public function names/signatures unchanged (`_avx2` suffix kept) to avoid touching call sites in `filter.rs`
- Filter 3 (Average) NEON kernel uses `vhaddq_u8` (halving add), simpler than the AVX2 path's widen-to-16-bit/narrow-back-to-8-bit workaround
- `unfilter_sub_avx2` and `unfilter_average_avx2` remain scalar-only on all architectures (sequential dependency on the just-reconstructed previous byte prevents safe vectorization)
- At this point in the NEON phase, Filters 4-15 and the other SIMD modules (`simd_packing.rs`, `simd_sample_conversion.rs`, `simd_quantize.rs`, `simd_shuffle.rs`) were still AVX2-only; see "v1.3.0 (cont.)" and "v1.4.0" sections below for their subsequent NEON ports

**Validation:**
- Native x86_64: `cargo build --lib`, `cargo test --lib` (273 tests), `cargo test --test integration_roundtrip` (7 tests), `cargo clippy -- -D warnings` all pass with zero regressions
- Cross-compile: `cargo check --target aarch64-unknown-linux-gnu --lib` and `cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` pass cleanly (toolchain: `zig cc -target aarch64-linux-gnu` as the C cross-compiler for `zstd-sys`)

### v1.3.0 (cont.) - ARM NEON SIMD extended to Filters 4-14

Ported incrementally, one filter at a time, each verified (native build + cross-compile check/clippy) before moving to the next:

- **Filters 9-12 (4-way Directional)**: new shared `directional_chunk_neon`/`filter_directional_neon_body` helpers (NEON analogue of `directional_chunk_avx2`/`filter_directional_avx2_body`), plus reusable widen/narrow helpers `widen_u8x16_to_u16x8_pair`/`narrow_u16x8_pair_to_u8x16`. Filter 12's exact `/5` uses a widening multiply (`vmull_u16`) + narrowing shift (`vshrn_n_u32`) instead of AVX2's `_mm256_mulhi_epu16` (NEON has no direct 16x16→high-16 multiply).
- **Filter 6 (Gradient)**: pure 8-bit wrapping arithmetic (`vaddq_u8`/`vsubq_u8`), no widening needed — same as the scalar formula.
- **Filters 4 (Paeth) and 13 (Context-Based)**: 16-bit-widened branchless blends via `vbslq_u16` (bitwise select), replacing AVX2's `blendv_epi8`; `<=` comparisons built the same way as AVX2 (`!(a > b)`, no native NEON `<=` opcode either).
- **Filters 5 (MED) and 7 (Simple Median)**: pure unsigned-byte `vminq_u8`/`vmaxq_u8`, no widening — new `filter_byte_chunk_neon_body` helper added since these don't fit the widened `filter_directional_neon_body` signature (NEON's distinct `uint8x16_t`/`uint16x8_t` types, unlike AVX2's uniform `__m256i`, required a separate non-widening body). MED's `c >= max(a,b)`/`c <= min(a,b)` use NEON's native `vcgeq_u8`/`vcleq_u8` directly (simpler than AVX2's `max_epu8(x,y)==x` trick, since NEON has real unsigned comparison intrinsics).
- **Filter 8 (2nd Order)**: 16-bit widening of `a`/`b`/`ll`/`uu` (4 neighbors), 8-lane (not 16-lane) NEON chunks since the AVX2 half-chunk width matches; `vshrq_n_s16` (arithmetic shift) has the same truncate-toward-`-infinity` semantics as AVX2's `_mm256_srai_epi16`.
- **Filter 14 (TR-Directional)**: three nested `vhaddq_u8` (halving add) calls, simpler than AVX2's widen-based `average_epu8_32` composition — same simplification already used for Filter 3.
- Filter 15 (Weighted) confirmed to remain scalar-only on every architecture (sequential adaptive-state dependency, unchanged from v1.1).
- Public function names/signatures unchanged (`_avx2` suffix kept everywhere) to avoid touching call sites in `filter.rs`.

**Validation (repeated after each filter, full suite re-run at the end):**
- Native x86_64: `cargo build --lib`, `cargo test --lib` (273 tests), `cargo test --test integration_roundtrip` (7 tests), `cargo clippy --lib -- -D warnings`, `cargo fmt --check` all pass with zero regressions
- Cross-compile: `cargo clean -p cafe --target aarch64-unknown-linux-gnu` followed by `cargo check --target aarch64-unknown-linux-gnu --lib` and `cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` (clean, not incremental, to force full re-analysis) pass with zero warnings

### v1.4.0 - ARM NEON SIMD extended to all remaining modules

Ported module by module (`simd_packing.rs` → `simd_sample_conversion.rs` → `simd_shuffle.rs` → `simd_quantize.rs`), each fully validated (native build/test + cross-compile clean check/clippy) before moving to the next:

- **`simd_packing.rs` (1/2/4-bit pack/unpack)** — "Full symmetry" approach: dedicated `_neon_impl` functions added even for the two functions that already had no real vectorization gain in the AVX2 version (2/4-bit pack: load-vectorized but pack scalar; 1/2/4-bit unpack: fully scalar), to keep the dispatch pattern consistent across the module. `pack_1bit_samples_neon_impl` is the one genuinely vectorized kernel: bit-gather via `vtstq_u8` + `vandq_u8` against MSB-first bit weights `[128,64,...,1]`, then `vaddv_u8` horizontal sum — no `reverse_bits()` workaround needed (unlike the AVX2 path, which has to reverse bit order to match `movemask` semantics). `pack_2bit`/`pack_4bit` NEON kernels load via NEON but pack scalar; `unpack_1bit/2bit/4bit` NEON kernels are scalar loops kept only for dispatch symmetry.
- **`simd_sample_conversion.rs` (8↔16-bit expand/reduce, RGBA→luma8)** — `expand_8to16_neon_impl` uses `vzip1q_u8`/`vzip2q_u8` (zipping a vector with itself duplicates each byte into an adjacent pair in one instruction per half, vs. 4 unpack ops on AVX2). `reduce_16to8_neon_impl` uses `vld2q_u8` (native 2-way deinterleave extracts the high byte of big-endian pairs directly, no mask/shuffle needed). `rgba_to_luma8_neon_impl` uses `vld4_u8` (native 4-way deinterleave splits R/G/B/A across 8 pixels per iteration), widening `vmovl_u8`+`vmull_u16` for the weighted sum in `u32`, `vcvtq_f32_u32`/`vcvtq_u32_f32` for the ÷1000 with truncation matching AVX2's `_mm256_cvttps_epi32`, and `vmovn_u32`+`vmovn_u16` to narrow back down. `expand_8to32float`/`reduce_32float_to8` are intentionally left scalar-only on every architecture (already unused/dead code in production; not wired into any call site, no behavior change needed).
- **`simd_shuffle.rs` (byte-shuffle, Filter Method=1)** — `apply_byte_shuffle_neon_impl`/`undo_byte_shuffle_neon_impl` use `vqtbl1q_u8` (128-bit table lookup, direct NEON analogue of `PSHUFB`, but processes half as many pixels per iteration as AVX2's 256-bit lanes since there's no 256-bit table-lookup instruction in NEON). `build_encode_mask`/`build_decode_mask` broadened from `x86_64`-only to `any(target_arch = "x86_64", target_arch = "aarch64")` since they're arch-agnostic and shared between both AVX2 and NEON paths — only `duplicate_mask` (specific to AVX2's 256-bit lane duplication) stays x86_64-only. **Bug fix**: `src/shuffle.rs` only imported/called into `simd_shuffle` under `#[cfg(target_arch = "x86_64")]`, which would have left the new NEON implementation dead code, never actually called; fixed to include `aarch64` in both the import and the dispatch call sites.
- **`simd_quantize.rs` (nearest-palette search)** — `find_closest_rgba_neon`/`find_closest_rgb_neon` reuse the same packed-key reduction trick as AVX2 (`key = (dist << 8) | idx`, so a single integer min also yields the winning index), widening `u8`→`u16`→`i32` via `vmovl_u8`+`vmovl_u16`+`vreinterpretq_s32_u32`, computing the squared Euclidean distance in `i32` (`vsubq_s32`+`vmulq_s32`+`vaddq_s32`, no overflow risk: max is `4×255² = 260100`), packing via `vshlq_n_s32`+`vorrq_s32`, and reducing 8 lanes (as two 4-lane halves combined with `vminq_s32`) down to a scalar via `vminvq_s32` — NEON's native horizontal-minimum reduction, simpler than AVX2's shuffle-based equivalent.
- No SIMD module is AVX2-only anymore: `simd.rs` (Filters 1-14), `simd_packing.rs`, `simd_sample_conversion.rs`, `simd_shuffle.rs`, and `simd_quantize.rs` all now dispatch to NEON on aarch64 at compile-time, mirroring the AVX2 runtime-detected paths on x86_64.

**Validation (repeated after each module, full suite re-run at the end):**
- Native x86_64: `cargo build --lib`, `cargo test --lib` (273 tests), `cargo test --test integration_roundtrip` (7 tests), `cargo clippy --lib -- -D warnings`, `cargo fmt --check` all pass with zero regressions
- Cross-compile: `cargo clean -p cafe --target aarch64-unknown-linux-gnu` followed by `cargo check --target aarch64-unknown-linux-gnu --lib` and `cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` (clean, not incremental) pass with zero warnings

### v1.4.1 - Real ARM execution validation (QEMU emulation via Docker)

Up through v1.4, all aarch64 validation was `cargo check`/`clippy --target aarch64-unknown-linux-gnu` — the code type-checked and lint-passed, but the NEON intrinsics had **never actually executed**. This phase ran the real test suite on native aarch64 via QEMU user-mode emulation (Docker Desktop's `--platform linux/arm64`, `rust:1-bookworm` image), closing that gap:

- **Setup**: `docker run --platform linux/arm64 rust:1-bookworm` transparently runs aarch64 binaries via QEMU on an x86_64 host; the container's own `gcc` (aarch64-native) builds `zstd-sys`'s C code directly — no cross-compiler needed inside the container (unlike the host-side `cargo check` cross-compile, which does need the `zig cc` wrapper). Docker Desktop's corporate-proxy env vars (`HTTP_PROXY`/`HTTPS_PROXY` baked into the image/daemon config) had to be overridden to empty strings per `docker run`/`docker exec` invocation, otherwise `cargo` couldn't reach crates.io from inside the container.
- **Bug found**: `cargo test --lib` under emulation failed 4 tests, all originating from `simd_quantize.rs`'s NEON path (`find_closest_rgba_neon`/`find_closest_rgb_neon`): the reported *distance* was always correct, but the reported *index* was off by exactly `+4` for matches found in the high half of an 8-entry chunk. Root cause: the per-half index computation `vaddq_s32(vdupq_n_s32((i + half * 4) as i32), lane_offsets)` used `lane_offsets_hi = [4,5,6,7]` for the high half (`half=1`) — but the base `i + half * 4` *already* added that `+4` shift, so the `+4..+7` offset was applied twice, double-counting it into `+8..+11`. Fix: always use `lane_offsets_lo = [0,1,2,3]` for both halves, since the within-chunk base already accounts for which half is being processed (`lane_offsets_hi` and its backing array removed as now-unused).
- **Why cross-compilation couldn't catch this**: `cargo check`/`clippy` only type-check the code — they don't execute NEON intrinsics, so a logic bug in index arithmetic (as opposed to a type/lint error) is invisible without actually running the instructions. This is precisely the risk flagged as an open item in the v1.3/v1.4 "Welcome Contributions" section, and why this validation phase existed.
- **Test coverage note**: the 4 failing tests (`test_find_closest_rgba_matches_scalar_reference_various_sizes`, `test_find_closest_rgb_matches_scalar_reference_various_sizes`, `test_max_palette_size_256_exhaustive_index_coverage`, `tests::test_roundtrip_adam7_indexed`) already existed prior to this phase (written during the original NEON port) and correctly compare NEON output against a scalar reference — they simply couldn't run on x86_64 CI, only on real/emulated aarch64.

**Validation:**
- Native aarch64 (QEMU emulation): `cargo build --lib` (~3m47s cold), `cargo test --lib` — **268/268 passed** (5 fewer than x86_64's 273: architecture-specific AVX2-vs-scalar comparison tests don't apply to aarch64), `cargo test --test integration_roundtrip` — **7/7 passed**, `cargo clippy --lib -- -D warnings` — zero warnings, all run from a cold `/usr/local/cargo/registry` volume (first run compiles the full dependency tree, ~4-7 min per command; a named Docker volume persists the registry cache across runs)
- Native x86_64 (re-verified after the fix): `cargo test --lib` (273 tests), `cargo test --test integration_roundtrip` (7 tests), `cargo clippy --lib -- -D warnings`, `cargo fmt --check` all still pass with zero regressions
- Cross-compile (re-verified after the fix): `cargo clean -p cafe --target aarch64-unknown-linux-gnu` followed by `cargo check`/`cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` pass with zero warnings

### v1.4.2 - CI: ARM64 Cross-Compile Check job

Closes the last open item from the 5-step ARM NEON plan: automating the aarch64 cross-compile check in CI so future regressions can't merge unnoticed (previously it was a manual, ad-hoc step run locally before each NEON-related commit).

- **New job**: `aarch64-cross-compile` added to `.github/workflows/ci.yml`, running independently alongside the existing `build`, `clippy`, `fmt`, and `security-audit` jobs on every push/PR.
- **Toolchain setup**: `dtolnay/rust-toolchain@stable` with `targets: aarch64-unknown-linux-gnu` and `components: clippy` installs the Rust side; `apt-get install -y gcc-aarch64-linux-gnu` installs the C cross-compiler needed by `zstd-sys`'s build script. Unlike local development (which uses a `zig cc` wrapper batch script, since Windows has no native GNU cross-toolchain readily available), Ubuntu CI runners can install a real `gcc-aarch64-linux-gnu` package directly via `apt`, which is simpler and more standard for CI.
- **Env vars**: `CC_aarch64_unknown_linux_gnu` and `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` both set to `aarch64-linux-gnu-gcc`, matching the pattern already used for local cross-compilation (just with a different underlying compiler binary).
- **Commands**: `cargo check --target aarch64-unknown-linux-gnu --lib` and `cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` — deliberately `--lib` only (not `--all-targets`/tests), since executing real aarch64 tests requires QEMU emulation (already done manually in v1.4.1) and would slow down every CI run for a check whose main goal is catching type/lint errors, not runtime logic bugs.
- **Caching**: `Swatinem/rust-cache@v2` keyed on `aarch64-unknown-linux-gnu` so the aarch64 dependency tree doesn't need a full rebuild on every run.
- **Scope note**: this CI job catches the same class of bugs as the local `cargo check`/`clippy --target aarch64-unknown-linux-gnu` commands used throughout v1.3/v1.4 (type errors, lint warnings) — it does **not** catch runtime logic bugs like the index-calculation bug found in v1.4.1, which required actually executing the NEON intrinsics. Real hardware/QEMU validation remains a manual, periodic step (see "Welcome Contributions" #2).

**Validation:**
- Workflow YAML syntax checked with `rhysd/actionlint` (via Docker, `docker run --rm -v <repo>:/repo rhysd/actionlint`) — zero errors/warnings.
- CI logic reproduced locally end-to-end inside a `rust:1-bookworm` Docker container (same `apt-get install gcc-aarch64-linux-gnu`, same env vars, same two `cargo` commands) to confirm the exact commands the CI job will run actually succeed: both `cargo check --target aarch64-unknown-linux-gnu --lib` and `cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` complete cleanly with zero warnings against the current `cafe v1.2.1` crate.
- This validation only exercises the job's shell commands in isolation (not a full GitHub Actions run, which requires pushing to trigger); the workflow triggers (`on: push`/`pull_request`) and job dependencies were reviewed manually against the rest of `ci.yml` to confirm no interference with existing jobs.

### v1.2.0 - SIMD Optimization Release

**Comprehensive AVX2 Vectorization (New Modules):**
- `src/simd_packing.rs` (523 lines): Pack/unpack 1/2/4-bit samples, 8-16x speedup on indexed/grayscale
- `src/simd_sample_conversion.rs` (349 lines): 8→16, 8→32 float expansion, 16→8 reduction
- `src/simd.rs`: Enhanced Filters 1-3 with optimized tail handling

**Performance Improvements (Validated):**
- Filter 3 (Average): 4-6x speedup via AVX2 unpacklo/unpackhi
- Pack 1-bit: 8-16x speedup (256 pixels/iteration)
- Pack 2-bit: 7-10x speedup (128 pixels/iteration)  
- Pack 4-bit: 5-7x speedup (64 pixels/iteration)
- Overall typical blend: 2.8-3.5x on mixed workloads (indexed, grayscale, float samples)
- Compression ratio validation: Checkerboard 11.4x smaller, gradient 9.3x, random 5.5x vs PNG

**Testing & Validation:**
- 203 total tests (197 unit + 6 integration roundtrip tests)
- Edge cases: 4×4 tiny, 2048×256 wide, 256×2048 tall images
- Roundtrip accuracy: PNG→CAFE→PNG verified for multiple dimensions & patterns
- Benchmark suite: Criterion framework ready for detailed profiling (benches/simd_performance.rs)

**Code Quality:**
- Zero TODOs/FIXMEs remaining
- Full Clippy compliance in library code
- 100% test passing rate (zero regressions)
- Feature-gated SIMD (--features simd, enabled by default)
- Automatic CPU detection with scalar fallback on non-AVX2

**Previous audits (v1.1.0):**
Round 5-10 covered CWE-369, PLTE compression, HDR tone-mapping, byte-shuffle, Filter 1-3 SIMD, image crate upgrade, supply-chain security via cargo-deny.
