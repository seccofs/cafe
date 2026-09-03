# CAFE — Compression Adaptive Filtering Experiment

[![License](https://img.shields.io/badge/license-BSD--3--Clause-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Security](https://img.shields.io/badge/security-audited-green)](docs/SECURITY_AUDIT.md)

A modern chunk-based image format inspired by PNG, with support for ZSTD compression, advanced predictive filters (16 types), indexed palette, structured metadata (EXIF, JSON, ICC, XMP), and progressive interlacing.

**Version**: 1.6.1  
**Status**: ✅ Complete, audited, and SIMD-accelerated  
**Compatibility**: Rust 2021+

---

## 🚀 Key Features

### Intelligent Compression
- **ZSTD** with fallback to raw data (section 3.2)
- Adjustable compression level (1-22)
- Optional ZSTD dictionary (`zDIC` chunk)

### Advanced Predictive Filters
- **16 filter types**: None, Sub, Up, Average, Paeth, MED, Gradient, Simple Median, 2nd Order, 4-way Directional (4 variants), Context-Based, TR-Directional (WebP Predictor 10), and adaptive Weighted (inspired by JPEG-XL)
- Applied per block/tile (Filter method=2) or **per row** (Filter method=3, v1.5, finer-grained adaptation)
- **AVX2 SIMD acceleration** (v1.1+): Filters 1-14 vectorized for 4-8x faster processing; automatic CPU detection with scalar fallback
- **ARM NEON SIMD acceleration** (v1.3-v1.4): all 14 vectorized filters plus pack/unpack, sample conversion, byte-shuffle, and palette quantization ported to NEON — no SIMD module is AVX2-only anymore
- **v1.2 Aggressive SIMD**: Pack/Unpack 1/2/4-bit (8-16x), Sample expansion/reduction 8→16/32 float (4-6x), Byte-shuffle with blocking (10-20% cache improvement), Filter 3 enhanced (4-6x)
- Automatic selection via heuristic: **Shannon Entropy** (default), **MSAD** (`--filter-heuristic msad`), **real compression test** (`--filter-heuristic test`), **QuickPrune** (v1.1, fast MSAD + Entropy on top 8), or **AdaptiveEntropy** (v1.1, content-aware analysis)

### Color Flexibility
- **Color types**: Grayscale, RGB, Indexed (palette), Grayscale+Alpha, RGBA
- **Bit depths**: 1, 2, 4, 8, 10, 12, 16, 32 bits
- **Sample formats**: uint, float (IEEE 754), half-float (fp16)

### Structured Metadata
- **EXIF**: Camera, geolocation, date (TIFF blob)
- **JSON**: Proprietary data per namespace
- **ICC**: Color profile for professional color management
- **XMP**: Editorial workflow metadata

### Intelligent Streaming
- **iDIM**: Real 2D tiling with IDAT per tile, row-major or Z-order (Morton) scan order
- **Interlacing**: Adam7 (7 passes) or Even/Odd (2 passes)
- Incremental decoding (chunk-by-chunk)
- **`Decoder<R: Read>` streaming API**: decode tile-by-tile directly off any `Read` source (file, socket, `Cursor`) without buffering the whole compressed file or the whole decoded image in memory — see `examples/streaming_decode.rs` and the "Library API" section below (row-strip tiling only; falls back to `decode`/`decode_bytes` for 2D-tiled or interlaced files)
- **`Encoder<W: Write>` streaming API** (v1.6): symmetric counterpart — write `IHDR` and each row-strip `IDAT` immediately as tiles arrive via `add_tile()`, instead of requiring the whole image in memory before `encode()` can produce output; `Encoder<W: Write + Seek>::finish_exact()` patches `compression_method` to its exact value (byte-for-byte identical to `encode()`), while plain `Write` destinations get a conservative (always-safe) overestimate — see `examples/streaming_encode.rs` and the "Library API" section below (no indexed palette, 2D tiling, or interlace in this v1)

### Compression Audit (v1.5)
- **Per-row predictive filter** (`Filter method=3`): finer-grained adaptation than per-tile filtering
- **`auto_dictionary` non-regression guarantee**: an auto-trained ZSTD dictionary is only used when it strictly shrinks the file, checked per-`IDAT` and whole-file
- **Perceptually-weighted palette quantization**: `PaletteAlgorithm::NearestNeighborWeighted` using the redmean distance formula
- **Real compression benchmarks + CI regression gate**: `tests/compression_regression.rs` and `benches/encode_decode.rs`

### Security
- ✅ Decompression bomb protection (CWE-409)
- ✅ Untrusted input validation
- ✅ No panic on malformed/truncated files
- ✅ [Complete audit](docs/SECURITY_AUDIT.md)

---

## 📦 Project Structure

```
cafe/
├── AGENTS.md                      # Developer technical guide
├── CLAUDE.md                      # Symbolic link to AGENTS.md
├── Cargo.toml                     # Dependencies and configuration (with simd feature)
├── deny.toml                      # Cargo-deny security and license configuration
├── README.md                      # English README
├── README.pt.md                   # Portuguese README
├── LICENSE                        # BSD-3-Clause license
├── src/                           # Main library
│   ├── cafe.rs                    # Core: encode/decode, chunks (re-exports)
│   ├── constants.rs               # Signature, flags, color types, filters
│   ├── chunk.rs                   # Chunk framing (Length/Type/Flag/Data/CRC32)
│   ├── codec.rs                   # ZSTD compression with fallback (section 3.2)
│   ├── color.rs                   # Color conversions, pack/unpack, float/half
│   ├── filter.rs                  # 16 predictive filters + heuristics (with SIMD integration)
│   ├── simd.rs                    # AVX2/NEON vectorized filters 1-14 (v1.1+, optional feature)
│   ├── simd_packing.rs            # AVX2/NEON pack/unpack for 1/2/4-bit samples (v1.2+)
│   ├── simd_sample_conversion.rs  # AVX2/NEON 8→16/32 expansion, 16/32→8 reduction (v1.2+)
│   ├── simd_quantize.rs           # AVX2/NEON nearest-palette search (v1.2+)
│   ├── simd_shuffle.rs            # AVX2/NEON byte-shuffle table lookup (v1.2+)
│   ├── shuffle.rs                 # Byte-shuffle (Filter Method=1, v1.1)
│   ├── tonemap.rs                 # HDR tone-mapping (EOTF, primaries, operators, v1.1)
│   ├── interlace.rs               # Adam7 and even/odd
│   ├── types.rs                   # EncodeOptions, Palette, iDim, cHDR, etc.
│   └── error.rs                   # CafeError
├── tools/                         # CLI tools
│   ├── cafe-encode.rs            # Encoder binary
│   └── cafe-decode.rs            # Decoder binary
├── docs/                          # Documentation
│   ├── CAFE-spec.md              # Complete specification (v1.1, updated through v1.6)
│   ├── CAFE-spec.pt.md           # Portuguese version of the spec
│   ├── SECURITY_AUDIT.md         # Security audit
│   └── DEVELOPER_GUIDE.md        # Developer guide
├── tests/                         # Integration and round-trip tests
├── examples/                      # Usage examples
│   ├── basic_encode.rs           # Basic encoding example
│   └── basic_decode.rs           # Basic decoding example
└── .github/
    └── workflows/                 # CI (build, clippy -D warnings, fmt, doc, security audit)
```

---

## 🏗️ Architecture

### Chunk Layout
```
[Length: 4 bytes BE]
[Type: 4 bytes ASCII]
[Flag: 1 byte] — 0x00=raw, 0x01=ZSTD
[Data: N bytes] — content (compressed or not)
[CRC32: 4 bytes BE]
```

### Defined Chunks

**Critical** (1st letter uppercase):
| Type | Description |
|------|-----------|
| `IHDR` | Header (always first, never compressed) |
| `PLTE` | Indexed palette (mandatory if color_type=3) |
| `IDAT` | Pixel data (1+ per file) |
| `IEND` | End marker (always last) |

**Ancillary** (1st letter lowercase, optional):
| Type | Description |
|------|-----------|
| `eXIF` | EXIF metadata (TIFF blob) |
| `jSON` | JSON metadata (multiple instances per namespace) |
| `iDIM` | Tiling and scan order for streaming |
| `iCCP` | ICC profile for color management |
| `xMPd` | XMP metadata |
| `zDIC` | ZSTD dictionary for IDAT |
| `cHDR` | HDR metadata (transfer func, luminance) |

---

## 📖 Usage

### Compilation

```bash
# Release build with SIMD (optimized, recommended)
cargo build --release

# Release build without SIMD (if AVX2 support is not available)
cargo build --release --no-default-features

# Executables
./target/release/cafe-encode input.png output.cafe
./target/release/cafe-decode output.cafe decoded.png
```

**Note on SIMD:** The `simd` feature is enabled by default. AVX2 support is detected at runtime via `is_x86_feature_detected!("avx2")`, so the same binary automatically uses AVX2 intrinsics for Filters 1, 2, and 3 on capable CPUs and falls back to scalar code otherwise. No special `RUSTFLAGS` or build flags are needed — it works out of the box with `cargo build --release`.

### Library API

```rust
use cafe::{encode, decode, EncodeOptions};

// Encode
let opts = EncodeOptions {
    use_filter: true,
    level: 19,
    adaptive_analysis: true,
    target_color_type: 6, // RGBA
    ..EncodeOptions::default()
};
encode("input.png", "output.cafe", &opts)?;

// Decode
let result = decode("output.cafe", "output.png")?;
println!("EXIF: {:?}", result.exif);
println!("JSON: {:?}", result.json_metadata);
```

#### Streaming decode (large images / low memory)

```rust
use cafe::Decoder;
use std::fs::File;

let file = File::open("output.cafe")?;
let mut decoder = Decoder::new(file);

let info = decoder.read_info()?; // reads IHDR + all pre-IDAT chunks
if info.supports_streaming_tiles {
    while let Some(tile) = decoder.next_tile()? {
        // tile.pixels: tile.width * tile.height * 4 bytes of RGBA
    }
}
let result = decoder.finish()?; // EXIF/JSON/ICC/XMP/HDR metadata
```

See `examples/streaming_decode.rs` for a complete runnable example, including
the fallback path for files using 2D tiling (`iDIM`) or interlacing, which
`next_tile()` does not support (check `info.supports_streaming_tiles`).

#### Streaming encode (large images / incremental producers)

```rust
use cafe::{Encoder, EncoderOptions};
use std::fs::File;

let file = File::create("output.cafe")?;
let opts = EncoderOptions::default();
let mut encoder = Encoder::new(file, width, height, &opts)?; // writes IHDR immediately

for row_strip in tiles {
    encoder.add_tile(&row_strip)?; // width * tile_height * 4 bytes RGBA per call
}

let _file = encoder.finish_exact()?; // exact compression_method (requires Seek)
// or encoder.finish()? for Write-only destinations (conservative compression_method)
```

See `examples/streaming_encode.rs` for a complete runnable example. `Encoder<W>`
v1 supports row-strip tiling and direct color types only (no indexed palette,
2D tiling, or interlace — see `EncoderOptions`'s doc comment).

### CLI

```bash
# Default encode
cafe-encode input.png output.cafe

# Encode with options
cafe-encode input.png output.cafe --level 22 --color-type 2 --no-filter

# Decode
cafe-decode output.cafe decoded.png

# Help
cafe-encode --help
cafe-decode --help
```

---

## 📊 Performance

### Compression Ratio
- **Typical PNG**: 100 KB → 60-80 KB (CAFE, 20-40% gain)
- **Colorful image**: Better on patterned data (gradients, lines)
- **Noisy image**: Similar to PNG (minimal filter gain)

### Encoding Speed (Benchmarked v1.1)
| Configuration | Time (512×512 RGB) | Notes |
|---|---|---|
| **Level 1** (fastest) | ~12 ms | No filters, single-pass compression |
| **Level 9** (balanced) | ~25 ms | Recommended for most use cases |
| **Level 19** (default) | ~45 ms | High compression, slight quality improvement |
| **Level 22** (maximum) | ~120 ms | Not recommended for real-time applications |

### Decoding Speed
- **RGBA decode** (512×512): ~8 ms
- **Indexed decode** (512×512): ~5 ms
- **With AVX2 SIMD** (v1.1+): 4-8x faster filter processing on Filters 1, 2, 3

### Comparison with PNG
- Encoding: ~2-5% slower than PNG (offset by better compression)
- Decoding: ~1-2x faster than PNG (simpler filter set, SIMD-accelerated)
- File size: ~15-25% smaller on average

**Benchmark note**: Run `cargo bench` to generate a detailed criterion report in `target/criterion/report/index.html`

---

## 🔒 Security

- ✅ **Audited**: [Complete report](docs/SECURITY_AUDIT.md)
- ✅ **Standardized**: Follows best practices for image formats
- ✅ **No panics**: All failures return `Result`, never crash on untrusted input
- ✅ **Memory limit**: Decompression bomb protection (1 GiB/chunk)

---

## 📋 Dependencies

```toml
image = "0.25"          # PNG, JPEG, etc. read/write
zstd = "0.13"           # ZSTD compression
serde_json = "1.0"      # JSON parsing
half = "2.7"            # Half-float (fp16)
crc32fast = "1.3"       # CRC32 for chunks
```

---

## 📚 Documentation

- **[CAFE Specification](docs/CAFE-spec.md)** — Complete specification (v1.2)
- **[Security Audit](docs/SECURITY_AUDIT.md)** — Detailed security audit
- **[Developer Guide](docs/DEVELOPER_GUIDE.md)** — Technical guide for contributors
- **[API Docs](https://docs.rs/cafe)** — Rust documentation (generated by `cargo doc`)

---

## 📝 License

Licensed under **BSD-3-Clause** — permissive, commercial-friendly, no copyleft requirements.

---

## 🤝 Contributing

Contributions welcome! High-potential areas:

- [x] SIMD in filters (Filter method 1, 2, 3) — *complete in v1.1* (AVX2, 4-8x speedup)
- [x] Byte-shuffle (Filter method=1) — *complete in v1.1*
- [x] Fuzzing tests — *complete in v1.1* (cargo-fuzz + robustness tests)
- [x] Property-based testing — *complete in v1.1* (proptest)
- [x] Benchmarking — *complete in v1.1* (criterion with PNG comparison)
- [x] Automatic ZSTD dictionary training — *complete in v1.1* (`--auto-dict`)
- [x] Indexed palette with median-cut — *complete in v1.1* (`--palette-algorithm`)
- [x] **SIMD in sub-byte packing** — *complete in v1.2.1* (pack/unpack 1/2/4-bit, 8-16x speedup, integrated into encode/decode pipeline)
- [x] **NEON support (ARM SIMD)** — *complete in v1.3-v1.4*: Filters 1-14 (v1.3) plus pack/unpack, sample conversion, byte-shuffle, and palette quantization (v1.4) — no SIMD module is AVX2-only anymore
- [x] **Per-row predictive filter** — *complete in v1.5* (`Filter method=3`, finer-grained adaptation than per-tile)
- [x] **Automatic ZSTD dictionary non-regression guarantee** — *complete in v1.5* (`auto_dictionary` only used when it strictly shrinks the file)
- [x] **Perceptually-weighted palette quantization** — *complete in v1.5* (`PaletteAlgorithm::NearestNeighborWeighted`, redmean distance)
- [x] **Real compression benchmarks + CI regression gate** — *complete in v1.5* (`tests/compression_regression.rs`, `benches/encode_decode.rs`)
- [x] **Streaming encoder** — *complete in v1.6* (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`, symmetric counterpart to the v1.5 `Decoder<R: Read>`)

---

## 📈 Roadmap

| Version | Features | Status |
|---------|----------|--------|
| **v1.0** | Critical chunks, ZSTD, 14 filters, metadata (EXIF/JSON/ICC/XMP/HDR), zDIC, sample_format float/half, security | ✅ Complete |
| **v1.1** | Filters 14-15: TR-Directional (WebP Predictor 10) and adaptive Weighted (inspired by JPEG-XL) — 16 total predictors; MSAD heuristic; real 2D tiling (iDIM) with end-to-end round-trip; byte-shuffle encode/decode; **AVX2 SIMD optimization (Filters 1-3)** | ✅ Complete |
| **v1.2** | **Aggressive SIMD Acceleration (AVX2 x86_64)**: Pack/Unpack 1/2/4-bit (8-16x), Sample expansion/reduction (4-6x), Byte-shuffle with blocking (10-20%), Filter 3 enhanced (4-6x); **252 comprehensive tests** (197 unit + 6 integration roundtrip + 49 SIMD), **Zero TODOs/FIXMEs**, Feature-gated SIMD with CPU detection | ✅ Complete |
| **v1.2.1** | Refinements and operator dispatcher for tone-mapping selection | ✅ Complete |
| **v1.3** | **ARM NEON SIMD (aarch64)**: all 14 vectorized filters ported to NEON, compile-time dispatch, no runtime feature check needed (NEON is ARMv8-A baseline) | ✅ Complete (Filters 1-14) |
| **v1.4** | **ARM NEON SIMD extended to all remaining modules**: pack/unpack, sample conversion, byte-shuffle, palette quantization — no SIMD module is AVX2-only anymore | ✅ Complete |
| **v1.4.1** | **Real ARM execution validation (QEMU emulation)**: full test suite run natively on aarch64 for the first time — found and fixed a real NEON index-calculation bug that cross-compile checks alone couldn't catch | ✅ Complete |
| **v1.4.2** | **CI: ARM64 cross-compile check** — new `aarch64-cross-compile` job runs `cargo check`/`clippy --target aarch64-unknown-linux-gnu` on every push/PR, preventing future aarch64 regressions from merging unnoticed | ✅ Complete |
| **v1.5** | **Compression-focused audit (5 items)**: per-row predictive filter (`Filter method=3`), real compression benchmarks + CI regression gate, `auto_dictionary` non-regression guarantee, perceptually-weighted palette quantization (redmean distance), `DEFAULT_TILE_ROWS` retuning investigation (kept at 64, documented trade-off) | ✅ Complete |
| **v1.6** | **Streaming Encoder** (`Encoder<W: Write>` / `Encoder<W: Write + Seek>`): writes `IHDR` + ancillary chunks + row-strip `IDAT`s incrementally as tiles arrive, symmetric counterpart to v1.5's `Decoder<R: Read>`; `finish()` sets a conservative `compression_method` for `Write`-only destinations, `finish_exact()` patches it to the exact value (byte-for-byte identical to `encode()`) when `W` also supports `Seek` | ✅ Complete |
| **Future** | Real hardware validation on physical ARM devices, additional compressors, k-means palette, tone-mapping on encode (SDR→HDR) | 🔮 Planned |

---

## 🐛 Report Issues

1. Check [existing issues](../../issues)
2. If new, provide:
   - CAFE version
   - Test file (if possible)
   - Full stack trace
   - Operating system / Rust version

For security vulnerabilities: see [SECURITY_AUDIT.md](docs/SECURITY_AUDIT.md)

---

## 👨‍💻 Author

**Daniel Secco** — Creator and maintainer  
Architecture, specification, Rust reference implementation (v1.1)

---

## 🙏 Acknowledgments

- **ZSTD** (Yann Collet) — Compression algorithm
- **PNG** (W3C working group) — Design inspiration
- **Rust community** — Excellent language and tools

---

**Last updated**: 2026-09-03 (v1.6.1: `cafe-encode` gains `--icc-profile-file`/`--xmp-file` CLI flags; v1.6: streaming encoder — `Encoder<W: Write>` / `Encoder<W: Write + Seek>`, symmetric counterpart to the v1.5 `Decoder<R: Read>`)  
**Test Coverage**: 311 lib tests + 13 integration test suites (roundtrip, streaming encode, SIMD, compression regression, dictionary regression, palette algorithm, tile_rows benchmarks, etc.)  
**Next security audit**: 2027-08-04
