# CAFE — Developer Technical Guide

## Overview

CAFE (Compression Adaptive Filtering Experiment) is a modern chunk-based image format, inspired by PNG, with support for ZSTD compression, advanced predictive filters, indexed palette, and structured metadata (EXIF, JSON, ICC, XMP).

**Specification:** `docs/CAFE-spec.md` (v1.1, updated through v1.6)
**Implementation:** Rust 2021 with BSD-3-Clause license

---

## Project Architecture

### File Structure

```
Cafe/
├── AGENTS.md             # Developer technical guide (you are here)
├── CLAUDE.md             # Symbolic link to AGENTS.md
├── Cargo.toml            # Dependencies and configuration (with simd feature)
├── deny.toml             # Cargo-deny security and license configuration
├── README.md             # English README
├── README.pt.md          # Portuguese README
├── LICENSE               # BSD-3-Clause license text
├── src/
│   ├── cafe.rs           # Core: encode/decode, chunks (re-exports)
│   ├── constants.rs      # Signature, flags, color types, filters
│   ├── chunk.rs          # Chunk framing (Length/Type/Flag/Data/CRC32)
│   ├── codec.rs          # ZSTD compression with fallback (section 3.2)
│   ├── color.rs          # Color conversions, pack/unpack, float/half
│   ├── filter.rs         # 16 predictive filters + heuristics (with SIMD integration)
│   ├── simd.rs           # AVX2/NEON vectorized filters 1-14 (v1.1+, optional feature)
│   ├── simd_packing.rs   # AVX2/NEON pack/unpack for 1/2/4-bit samples (v1.2+)
│   ├── simd_sample_conversion.rs # AVX2/NEON 8→16/32 expansion, 16/32→8 reduction (v1.2+)
│   ├── simd_quantize.rs  # AVX2/NEON nearest-palette search (v1.2+)
│   ├── simd_shuffle.rs   # AVX2/NEON byte-shuffle table lookup (v1.2+)
│   ├── shuffle.rs        # Byte-shuffle (Filter Method=1, v1.1)
│   ├── tonemap.rs        # HDR tone-mapping (EOTF, primaries, operators, v1.1)
│   ├── interlace.rs      # Adam7 and even/odd
│   ├── types.rs          # EncodeOptions, Palette, iDim, cHDR, etc.
│   └── error.rs          # CafeError
├── tools/
│   ├── cafe-encode.rs    # Encode binary (CLI)
│   └── cafe-decode.rs    # Decode binary (CLI)
├── tests/                # Integration and round-trip tests
├── examples/             # Usage examples
│   ├── basic_encode.rs   # Basic encoding example
│   └── basic_decode.rs   # Basic decoding example
├── docs/                 # Spec, security audit, dev guide
│   ├── CAFE-spec.md      # Complete format specification (v1.1, updated through v1.6)
│   ├── CAFE-spec.pt.md   # Portuguese version of the spec
│   ├── SECURITY_AUDIT.md # Security audit report
│   └── DEVELOPER_GUIDE.md # Developer guide
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
| Length   | 4 bytes BE  | Size of `Data` field |
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

Each block/tile (set of lines in an `IDAT`) chooses a single filter, prefixed by 1 byte at start of block (`Filter method=2`). As of v1.5, `Filter method=3` selects a filter **per row** instead, at the cost of one extra byte per row before compression (finer-grained adaptation than per-tile). Prediction occurs **before** ZSTD compression.

| Code | Name | Prediction | Cost | SIMD (v1.1+) |
|--------|------|----------|-------|------|
| `0` | None | None | O(1) | ✅ (memcpy) |
| `1` | Sub | Left byte (L) | O(n) | ✅ AVX2 (4-8x) / NEON |
| `2` | Up | Above byte (U) | O(n) | ✅ AVX2 (4-8x) / NEON |
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
- Filters 1-14 use **AVX2 intrinsics** on x86_64 CPUs and **ARM NEON intrinsics** on aarch64 CPUs when available (Filter 15 is scalar-only on every architecture)
- Processes 32 bytes per SIMD iteration on AVX2, 16 bytes per iteration on NEON
- **Automatic CPU feature detection**: If AVX2 not available, falls back to scalar (NEON is dispatched at compile-time instead, since it's mandatory on ARMv8-A)
- **Configurable via feature gate**: `cargo build --no-default-features` disables SIMD
- Expected speedup: **4-8x** on these filters
- **NEON coverage (v1.3+, aarch64):** All 14 vectorized filters have NEON kernels, dispatched at **compile-time** via `#[cfg(target_arch = "aarch64")]` (no runtime feature check needed — NEON is baseline on ARMv8-A). All other SIMD modules (`simd_packing.rs`, `simd_sample_conversion.rs`, `simd_quantize.rs`, `simd_shuffle.rs`) also have NEON kernels as of v1.4 — no module is AVX2-only anymore.

**Selection heuristics (encoder decides, not part of spec):**
- Shannon Entropy: Measures pattern redundancy in residuals (default, `FilterHeuristic::Entropy`)
- MSAD: Sum of absolute residual values (fast, classic — available as alternative)
- Real compression test: Compresses each candidate (ZSTD) and uses smallest (costly, `FilterHeuristic::CompressionTest`)
- QuickPrune (v1.1): Fast MSAD followed by Shannon Entropy on top 8 candidates (~1-2% gain, `FilterHeuristic::QuickPrune`)
- AdaptiveEntropy (v1.1): Block type analysis + content-aware selection (~2-3% gain on photos, `FilterHeuristic::AdaptiveEntropy`)

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
- **INDEXED (type=3):** Pixel index (color from PLTE)

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
        * Test all 16 filters on all lines of block
        * Choose single filter via heuristic (MSAD, entropy, or real test)
        * Store 1 byte at start of block indicating filter used
        * Calculate residuals
   b. If interlace ≠ 0: prefix pass_number (1 byte)
   c. Apply compression fallback (section 3.2):
      - Candidate 1: raw (Flag=0x00)
      - Candidate 2: ZSTD (Flag=0x01), level 19 (default)
      - Use shorter version
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
   - Accept filter_method ∈ {0, 1, 2}; reject 3+ (critical/unknown)
    ↓
3. Read pre-IDAT ancillary chunks (optional)
   - iDIM (if present): set tiling scheme
   - eXIF, jSON, iCCP, xMPd, zDIC
    ↓
4. If color_type=3, read PLTE (mandatory)
    ↓
5. For each IDAT:
   a. Decompress per Flag
      - Flag=0x00: use as-is (raw)
      - Flag=0x01: ZSTD.decompress() with zDIC if present
      - Enforce: 1 GiB limit per chunk (CWE-409 protection)
   b. If interlace ≠ 0: read 1 byte of pass_number
   c. If filter_method=1 (byte-shuffle): undo reordering per block/tile
      (validate bpp ∈ {2,4,8,16}; tile dimensions, not entire image)
   d. If filter_method=2: for each block/tile:
      - Read 1 byte indicating block's filter
      - Reverse prediction on all lines (only indicated type, not test all)
   e. Unpack if bit_depth < 8
   f. Uncompact if bit_depth > 8 (big-endian)
   g. Convert to RGBA (via convert_color_type_to_rgba)
    ↓
6. Reconstruct final image in RGBA
    ↓
7. Return image + metadata
```

---

## Reference Implementation (src/cafe.rs)

Public entry points and chunk I/O live in `src/cafe.rs`; internal helpers are split across the modules listed above (`color.rs`, `filter.rs`, `codec.rs`, `chunk.rs`, etc.).

### Main Structures

`CafeError` lives in `src/error.rs`; `Palette`, `iDim`, `cHDR`, `EncodeOptions`, `DecodeResult`, `FilterHeuristic`, `PaletteEntry` live in `src/types.rs`. All are re-exported from `src/cafe.rs`.

```rust
pub enum CafeError {
    InvalidSignature,
    CrcMismatch { chunk_type, expected, actual },
    UnsupportedFeature(String),
    TruncatedFile(String),          // Truncated file or forged field
    DecompressionLimitExceeded { limit }, // Decompression bomb protection
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

pub struct DecodeResult {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA
    pub exif: Option<Vec<u8>>,
    pub json_metadata: HashMap<String, Value>,
}
```

#### Streaming Decoder (`Decoder<R: Read>`, v1.5+)

For large images or memory-constrained environments, `Decoder<R: Read>`
decodes tile-by-tile directly off any `Read` source (file, socket, in-memory
`Cursor`) instead of requiring the whole compressed file or the whole
decoded image to be materialized in memory up front.

```rust
pub struct Decoder<R: Read> { /* ... */ }

impl<R: Read> Decoder<R> {
    pub fn new(reader: R) -> Self
    pub fn read_info(&mut self) -> Result<DecodeInfo>       // reads signature + all pre-IDAT chunks
    pub fn next_tile(&mut self) -> Result<Option<Tile>>     // one IDAT -> one Tile, None at IEND
    pub fn finish(self) -> Result<DecodeResult>             // drains remaining IDATs, returns metadata
}
```

**Call order**: `read_info()` exactly once, then `next_tile()` in a loop
until `Ok(None)`, then optionally `finish()` for ancillary metadata.
`next_tile()` supports `iDIM` (2D tiling, since v1.9 — yields tiles at their
real `(x, y)` grid position) as well as plain row-strip files, but still
returns `UnsupportedFeature` for Adam7/even-odd interlacing (a permanent
design limitation — an interlace pass is not a spatial rectangle) and for
`iDIM` combined with `COLOR_TYPE_INDEXED` or `bit_depth < 8` — check
`DecodeInfo::supports_streaming_tiles` first and fall back to
`decode`/`decode_bytes` for those. See `examples/streaming_decode.rs` for a
complete runnable example, and `AGENTS.md`'s "Streaming Decoder" section for
full field-by-field detail.

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
```

**Call order**: `new()` once (writes signature + `IHDR` + pre-IDAT
ancillary chunks immediately), then `add_tile()` any number of times with
row-strip buffers (`width * tile_height * 4` bytes RGBA — `tile_height` is
inferred from the buffer, not required to be constant across calls), then
`finish()`/`finish_exact()` once all `height` rows have been submitted.

**Limitations (v1)**: no indexed palette (`COLOR_TYPE_INDEXED` —
`encode_indexed()` remains the only path for that), no `iDIM` (2D tiling),
no interlace (Adam7/even-odd), no `auto_dictionary`. Only row-strip tiling
and direct color types (Gray/RGB/GrayAlpha/RGBA), mirroring `Decoder<R>`'s
existing limitation. An explicit, caller-supplied `zstd_dictionary` remains
supported.

**`compression_method` semantics**: `finish()` (`W: Write`, cannot seek
back) leaves the ZSTD bit set unconditionally — a safe overestimate, never
an underestimate. `finish_exact()` (`W: Write + Seek`) instead patches the
byte (and recomputes `IHDR`'s CRC32) to its exact value once every chunk is
known, byte-for-byte identical to `encode()`'s own output for the same
pixels/options. See `examples/streaming_encode.rs` for a complete runnable
example, and `AGENTS.md`'s "Streaming Encoder" section for full
implementation detail.

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
// High level (per block/tile): chooses single filter and prefixes 1 byte
fn apply_predictive_filter(tile_raw, tile_height, bytes_per_row, bpp, heuristic, level) -> Result<Vec<u8>>
fn undo_predictive_filter(tile_data, tile_height, bytes_per_row, bpp) -> Result<Vec<u8>>

// Application/reversal per line (residuals); prev_prev_row only for F_2NDORDER (UU)
fn filter_row(row, prev_row, prev_prev_row, ftype, bpp) -> Vec<u8>
fn unfilter_row(filtered, prev_row, prev_prev_row, ftype, bpp) -> Vec<u8>
fn filter_block(tile_raw, tile_height, bytes_per_row, bpp, ftype) -> Vec<u8>   // F_WEIGHTED with state in flow

// Dispatcher (F_WEIGHTED doesn't go here — requires state in flow)
fn predict(ftype, a, b, c, d, ll, uu) -> u8

// Specific predictors (a=left, b=above, c=top-left diagonal; ll/uu for 2nd order; d=TR)
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
- `--byte-shuffle` — Use byte-shuffle (Filter Method=1) for multi-byte samples (bpp ∈ {2,4,8,16})
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

**Always** return handleable error (CafeError).

### 12.2 Protection against Decompression Bomb (CWE-409)

**Mandatory requirement:**

```rust
const MAX_DECOMPRESSED_CHUNK_SIZE: u64 = 1_073_741_824; // 1 GiB

// On decompression:
if decompressed_size > MAX_DECOMPRESSED_CHUNK_SIZE {
    return Err(CafeError::DecompressionLimitExceeded { limit });
}
```

This **is not** optional — it is part of safe decoding contract.

**Cumulative ceiling per image (IDATs):** in addition to per-chunk ceiling, decoder calculates total decompression budget derived from IHDR (`compute_decompress_budget` in `src/cafe.rs`): `bytes_per_row × height` (+margin), `width × height` for indexed, and `width × height × 4 + passes` for interlace. Each IDAT is decompressed via `decompress_chunk_dict_limited` limited to remaining budget — multiple IDATs cannot sum to gigabytes when image is small.

### 12.3 Absence of Upper Limit for Width/Height

Intentionally no maximum. Decoder must reconstruct **incrementally** from actual data received in IDAT, not pre-allocate `width × height × bytes_per_pixel` before validation.

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

**CI integration (v1.6.3+, implemented)** — `.github/workflows/fuzz.yml` runs a full-hour (configurable via `workflow_dispatch`'s `duration_seconds` input) fuzz run per harness (`decode_fuzz`, `chunk_roundtrip_fuzz`) nightly at 2 AM UTC, plus on-demand. This is separate from `ci.yml`'s own `fuzz` job, which only runs each target for 60s on every push/PR as a fast smoke test — the nightly job trades that speed for depth. On failure, both crash artifacts (`fuzz/artifacts/<target>/`) and the corpus (`fuzz/corpus/<target>/`) are uploaded as workflow artifacts for local reproduction.

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
| `palette_algorithm` | `--palette-algorithm <a>` | ✅ | v1.1, nearest (default); median-cut; weighted (v1.5, redmean, scalar-only); kmeans (v1.7, Lloyd's algorithm) |
| `tonemap_operator` | — | ⚠️ **partial** | Decode-side only (`cafe-decode`'s `--tonemap-operator`); not consulted by `encode()`. See `inverse_tonemap` below for the actual encode-side option. |
| `inverse_tonemap` | `--inverse-tonemap <reinhard>` | ✅ | v1.8, opt-in SDR→HDR synthesis on encode. Only `reinhard` supported; requires `--sample-format 1` + `--chdr-transfer 0` + RGBA color type. |

`EncoderOptions` (the streaming `Encoder<W>` API, v1.6) is a deliberately smaller struct with no CLI binary of its own — it's a library-only API (`tile_rows`, `level`, `use_filter`/`use_filter_per_row`, `target_color_type`, `target_bit_depth`, `exif`, `json_metadata`, `icc_profile`, `xmp_metadata`, `zstd_dictionary`, `sample_format`, `chdr_metadata`, `filter_heuristic`, `use_byte_shuffle`), so CLI parity doesn't apply to it the same way; see its limitations list in the "Streaming Encoder" section above.

**`DecodeResult` fields accessibility** (`tools/cafe-decode.rs`):

| Field | CLI Export | Status | Notes |
|-------|-----------|--------|-------|
| `width` / `height` | Implicit (output file dimensions) | ✅ | Encoded in the decoded output image |
| `exif` | `--save-exif <path>` (v1.6.2) | ✅ | Byte count always printed; raw bytes saved to a file when the flag is given |
| `json_metadata` | `--extract-metadata` | ✅ | Prints namespace keys always, full contents with `--extract-metadata` |
| `compression_stats` | `--show-stats` (v1.6.2) | ✅ | Real per-chunk original/compressed sizes, printed as a table with totals + ratio |
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
| v1.1 | Filters 14-15 (TR-Directional WebP Predictor 10 and adaptive Weighted inspired by JPEG-XL), 16 total predictors, MSAD heuristic, real 2D tiling (iDIM) with end-to-end round-trip, byte-shuffle (Filter method=1, encode+decode), HDR tone-mapping on decode, **AVX2 SIMD for Filters 1-3** (4-8x speedup) | ✅ |
| **v1.3** | **ARM NEON SIMD (aarch64)**: all 14 vectorized filters ported to NEON, compile-time dispatch via `#[cfg(target_arch = "aarch64")]` (no runtime check needed, NEON is ARMv8-A baseline) | ✅ (Filters 1-14) |
| **v1.4** | **ARM NEON SIMD extended to all remaining modules**: `simd_packing.rs`, `simd_sample_conversion.rs`, `simd_shuffle.rs`, `simd_quantize.rs` — no SIMD module is AVX2-only anymore | ✅ |
| **v1.4.1** | **Real ARM execution validation (QEMU emulation)**: ran the full test suite natively on aarch64 for the first time — found and fixed a real NEON index-calculation bug in `simd_quantize.rs` that cross-compilation alone could never have caught | ✅ |
| **v1.4.2** | **CI: ARM64 Cross-Compile Check job** — new `aarch64-cross-compile` job in `.github/workflows/ci.yml` runs `cargo check`/`cargo clippy --target aarch64-unknown-linux-gnu --lib -- -D warnings` on every push/PR (Ubuntu runner + `gcc-aarch64-linux-gnu`), preventing future aarch64 regressions from merging unnoticed | ✅ |
| **v1.5** | **Compression-focused audit (5 items)**: per-row predictive filter (`Filter method=3`), real compression benchmarks + CI regression gate (`tests/compression_regression.rs`, `benches/encode_decode.rs`), `auto_dictionary` non-regression guarantee, perceptually-weighted palette quantization (`PaletteAlgorithm::NearestNeighborWeighted`, redmean distance), `DEFAULT_TILE_ROWS` retuning investigation (benchmarked, kept at 64) | ✅ |
| **v1.6** | **Streaming Encoder** (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`, symmetric counterpart to v1.5's `Decoder<R: Read>`): writes `IHDR` + ancillary chunks + row-strip `IDAT`s incrementally as tiles arrive; `finish()` sets a conservative `compression_method` for `Write`-only destinations, `finish_exact()` patches it to the exact value (byte-for-byte identical to `encode()`) when `W` also supports `Seek` | ✅ |
| **v1.6.1** | **CLI: `--icc-profile-file`/`--xmp-file` flags for `cafe-encode`** — closes a CLI-parity gap: `EncodeOptions::icc_profile`/`xmp_metadata` already existed in the library but had no CLI flag to populate them | ✅ |
| **v1.6.2** | **Real `compression_stats` + `cafe-decode` metadata-export flags**: `DecodeResult::compression_stats` now populated with real per-chunk original/compressed sizes (previously always `None`); new `--show-stats`, `--save-exif`, `--save-icc-profile`, `--save-xmp`, `--save-zstd-dict` flags on `cafe-decode` | ✅ |
| **v1.6.3** | **CI: nightly fuzz workflow** — new `.github/workflows/fuzz.yml` runs `decode_fuzz`/`chunk_roundtrip_fuzz` for a full hour nightly (plus on-demand via `workflow_dispatch`), separate from `ci.yml`'s existing 60s-per-push smoke test job | ✅ |
| **v1.7** | **`PaletteAlgorithm::KMeans`**: new indexed-palette quantization algorithm implementing Lloyd's algorithm, deterministically initialized from `MedianCut`'s output (no RNG dependency), typically the lowest mean-squared-error palette of the four algorithms at the highest computational cost — `--palette-algorithm kmeans` | ✅ |
| **v1.8** | **Inverse tone-mapping on encode (SDR→HDR synthesis)**: `ToneMapOperator::apply_inverse` (Reinhard only), `apply_inverse_tone_mapping_to_image`, opt-in `EncodeOptions::inverse_tonemap: Option<ToneMapOperator>` field (default `None`, non-breaking), `--inverse-tonemap reinhard` CLI flag; requires `sample_format=1` + `chdr_transfer=0` (linear) + RGBA color type | ✅ |
| **v1.9** | **`Decoder<R>::next_tile()` support for 2D tiling (`iDIM`)**: yields one `Tile` per `IDAT` with its real `(x, y, width, height)` grid position (row-major or Z-order, partial edge tiles included) instead of unconditionally erroring for `iDIM` files; `decode_idim_tile_raw` shared between the whole-image and streaming paths; interlace (Adam7/even-odd) remains a permanent, documented design limitation of `next_tile()` | ✅ |
| **v1.9.1** | **Documentation-only: `Encoder<W>`'s missing `auto_dictionary`/indexed/interlace support reclassified from "v1 gap" to permanent, investigated design limitation** — no code or behavior change; doc comments rewritten with per-item technical reasoning for why no incremental, buffer-free implementation exists for any of the three | ✅ |
| **v1.9.2** | **CI: ARM64 native test job** — new `arm64-native-test` job runs `cargo test --lib --release` + full integration suite on `ubuntu-24.04-arm` (real Arm server CPUs, not x86_64-with-emulation), closing the gap between `aarch64-cross-compile`'s type-check-only coverage and v1.4.1's one-off manual QEMU validation | ✅ |
| Future | Real hardware validation on physical *end-user* ARM devices (Raspberry Pi, mobile, Apple Silicon), additional compressors, tone-mapping operator selection via CLI for PQ/HLG/sRGB transfer functions and Filmic on encode | ⏳ |

---

## Performance and Optimizations

### SIMD Acceleration (v1.1+ → v1.4+)

**What's Vectorized:**
- **Filters 1-14**: all vectorized on AVX2 (x86_64) and NEON (aarch64); Filter 15 (Weighted) remains scalar-only everywhere (sequential adaptive-state dependency)
- **Pack/Unpack 1/2/4-bit, sample expansion/reduction, byte-shuffle, palette quantization**: all vectorized on AVX2 and NEON as of v1.4 (see `AGENTS.md` for per-module details)

**Building with SIMD:**
```bash
# Default (SIMD enabled on x86_64, AVX2 detected at runtime; NEON on aarch64 at compile-time)
cargo build --release

# Disable SIMD for portability
cargo build --release --no-default-features
```

**How to Check SIMD is Working:**
- On AVX2 systems: filter processing is **4-8x faster** than scalar
- Falls back gracefully on non-AVX2 CPUs (no runtime penalty, just slower)
- Run `cargo test --lib` to verify roundtrips pass with SIMD

### Known Bottlenecks

1. **Filter heuristic:** Testing all 16 filters is O(16n) per block/tile
   - Solution: Shannon entropy (cheaper than real compression)
   - Future: Smart heuristic that skips unlikely filters
   - **SIMD helps**: Filters 1-14 are now 4-8x faster

2. **Decompression without dictionary:** ZSTD slow without context
   - Solution: zDIC chunk for small images; `auto_dictionary` non-regression guarantee (v1.5) ensures it's only used when it actually helps

3. **Encode time vs. tile size:** larger tiles compress slightly better but reduce parallelism (rayon splits work per tile) — `DEFAULT_TILE_ROWS=64` is a deliberately tuned balance, not a compression-only optimum (see `tests/tile_rows_benchmark.rs`, v1.5)

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

1. **SIMD for sub-byte packing** — Extend AVX2/NEON to the scalar-only parts of `pack/unpack_samples_row` (partially vectorized as of v1.4)
2. **Real ARM hardware validation on physical devices** — QEMU emulation (v1.4.1) already caught and fixed one real NEON bug; running the suite on actual ARM64 hardware (Raspberry Pi, mobile, Apple Silicon) would add confidence beyond emulation (e.g. timing-sensitive or alignment-sensitive behavior QEMU might not reproduce exactly)
3. **Advanced 2D tiling** — iDIM with per-tile IDAT already implemented, including streaming decode via `Decoder<R>::next_tile()` (v1.9); evolve with preview/progressive streaming, and `Encoder<W>` write-side support (currently row-strip only, no iDIM)
4. **Optimized interlace** — Adam7 and even/odd already supported; optimize progressiveness and SIMD of passes
5. ~~**Optimized indexed palette**~~ — `NearestNeighbor`, `MedianCut`, `NearestNeighborWeighted` (redmean distance, v1.5), and `KMeans` (Lloyd's algorithm, v1.7) now cover greedy incremental, median-cut bucket-splitting, and iterative-clustering strategies — closed as of v1.7
6. **Automatic ZSTD dictionary** — Train dictionary for small images (non-regression guarantee already added in v1.5)
7. **Tone-mapping on encode (SDR→HDR)** — Inverse of decode; operator selection via CLI
8. **More robust tests** — Adversarial files, fuzzing
9. **Benchmarking** — Performance vs PNG, JPEG, WebP (real compression regression tests + Criterion benches already added in v1.5)

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

# Lint and fmt
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

**Last updated:** September 4, 2026 (v1.9.2: CI gains an `arm64-native-test` job running the full test suite natively on `ubuntu-24.04-arm` (real ARM64 silicon, not emulated); v1.9.1: `Encoder<W>`'s `auto_dictionary`/indexed/interlace limitations reclassified from "v1 gap" to permanent, investigated design limitation — documentation only, no code change; v1.9: `Decoder<R>::next_tile()` streaming support for 2D tiling — `iDIM` files now stream real `(x, y, width, height)` tiles instead of erroring, interlace remains a permanent documented limitation; v1.8: inverse tone-mapping on encode — `EncodeOptions::inverse_tonemap`, `--inverse-tonemap reinhard`, SDR→HDR synthesis; v1.7: `PaletteAlgorithm::KMeans` — deterministic k-means palette quantization, `--palette-algorithm kmeans`; v1.6.3: nightly fuzz CI workflow — `.github/workflows/fuzz.yml`; v1.6.2: real `compression_stats` tracking + `cafe-decode` gains `--show-stats`/`--save-exif`/`--save-icc-profile`/`--save-xmp`/`--save-zstd-dict` flags; v1.6.1: `cafe-encode` gains `--icc-profile-file`/`--xmp-file` CLI flags; v1.6: streaming encoder — `Encoder<W: Write>` / `Encoder<W: Write + Seek>`, symmetric counterpart to the v1.5 `Decoder<R: Read>`) | **Project version:** v1.9.2
