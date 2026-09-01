# CAFE — Developer Technical Guide

## Overview

CAFE (Compression Adaptive Filtering Experiment) is a modern chunk-based image format, inspired by PNG, with support for ZSTD compression, advanced predictive filters, indexed palette, and structured metadata (EXIF, JSON, ICC, XMP).

**Specification:** `docs/CAFE-spec.md` (698 lines, v1.2.1)
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
│   ├── simd.rs           # AVX2 vectorized filters 1-3 (v1.1+, optional feature)
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
│   ├── CAFE-spec.md      # Complete format specification (v1.2.1)
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

Each block/tile (set of lines in an `IDAT`) chooses a single filter, prefixed by 1 byte at start of block. Prediction occurs **before** ZSTD compression.

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
cargo run --bin cafe-encode -- encode <input> <output.cafe> [options]
```

**Options:**
- `--no-filter` — Disable predictive filter (faster)
- `--level <1-22>` — ZSTD compression level (default: 19)
- `--color-type <0|2|4|6>` — Force color type (default: auto-detected)
- `--adaptive` — Local complexity analysis per tile
- `--indexed` — Force indexed palette
- `--json-file <file>` — JSON metadata
- `--exif-file <file>` — Raw EXIF blob

**Example:**
```bash
cargo run --bin cafe-encode -- encode photo.png photo.cafe --level 22 --color-type 2
```

### Decode

```bash
cargo run --bin cafe-decode -- decode <input.cafe> <output>
```

**Example:**
```bash
cargo run --bin cafe-decode -- decode photo.cafe photo.png
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
```

---

## Version Roadmap

| Version | Functionality | Status |
|--------|---|---|
| v1.0 | IHDR, IDAT, IEND, ZSTD, Filters 0-13 (Shannon Entropy or real compression test), iDIM (tiling), Adam7, even/odd, indexed PLTE, eXIF, jSON, iCCP, xMPd, cHDR, zDIC, sample_format (uint/float/half), bit depths 1-32, security audit | ✅ |
| v1.1 | Filters 14-15 (TR-Directional WebP Predictor 10 and adaptive Weighted inspired by JPEG-XL), 16 total predictors, MSAD heuristic, real 2D tiling (iDIM) with end-to-end round-trip, byte-shuffle (Filter method=1, encode+decode), HDR tone-mapping on decode, **AVX2 SIMD for Filters 1-3** (4-8x speedup) | ✅ |
| **v1.3** | **ARM NEON SIMD (aarch64)**: all 14 vectorized filters ported to NEON, compile-time dispatch via `#[cfg(target_arch = "aarch64")]` (no runtime check needed, NEON is ARMv8-A baseline) | ✅ (Filters 1-14) |
| **v1.4** | **ARM NEON SIMD extended to all remaining modules**: `simd_packing.rs`, `simd_sample_conversion.rs`, `simd_shuffle.rs`, `simd_quantize.rs` — no SIMD module is AVX2-only anymore | ✅ |
| Future | Real hardware/emulated ARM validation (QEMU/Docker), CI step for aarch64 cross-compile check, additional compressors, k-means palette, tone-mapping on encode (SDR→HDR) | ⏳ |

---

## Performance and Optimizations

### SIMD Acceleration (v1.1+)

**What's Vectorized:**
- **Filter 1 (Sub)**: `pixel - left`, 32 bytes/iteration with AVX2
- **Filter 2 (Up)**: `pixel - above`, 32 bytes/iteration with AVX2
- **Filter 3 (Average)**: `pixel - (left + above) / 2`, scalar-optimized

**Building with SIMD:**
```bash
# Default (SIMD enabled on x86_64)
cargo build --release

# Disable SIMD for portability
cargo build --release --no-default-features

# Force SIMD on compatible CPU
RUSTFLAGS="-C target-feature=+avx2" cargo build --release
```

**How to Check SIMD is Working:**
- On AVX2 systems: filter processing is **4-8x faster** than scalar
- Falls back gracefully on non-AVX2 CPUs (no runtime penalty, just slower)
- Run `cargo test --lib` to verify roundtrips pass with SIMD

### Known Bottlenecks

1. **Filter heuristic:** Testing all 16 filters is O(16n) per block/tile
   - Solution: Shannon entropy (cheaper than real compression)
   - Future: Smart heuristic that skips unlikely filters
   - **SIMD helps**: Filters 1-3 are now 4-8x faster

2. **Decompression without dictionary:** ZSTD slow without context
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
2. **Real ARM hardware/emulated validation** — NEON kernels for all SIMD modules are complete (v1.3-v1.4) and validated via `cargo check`/`clippy --target aarch64-unknown-linux-gnu`, but never actually executed; run the test suite under QEMU user-mode emulation or real ARM64 hardware (Raspberry Pi, mobile, Apple Silicon) to catch any intrinsic-semantics mismatch cross-compilation can't detect
3. **Real 2D tiling (iDIM)** — Implemented; evolve with preview/progressive streaming
4. **Optimized interlace** — Adam7 and even/odd already supported; optimize progressiveness and SIMD of passes
5. **Optimized indexed palette** — Currently uses nearest-neighbor; could use k-means
6. **Automatic ZSTD dictionary** — Train dictionary for small images
7. **Tone-mapping on encode (SDR→HDR)** — Inverse of decode; operator selection via CLI
8. **More robust tests** — Adversarial files, fuzzing
9. **Benchmarking** — Performance vs PNG, JPEG, WebP

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

**Last updated:** August 11, 2026 (v1.2.1: SIMD fully integrated, tone-mapping operator dispatcher, 252 comprehensive tests) | **Project version:** v1.2.1
