# CAFE — Compression Adaptive Filtering Experiment

[![License](https://img.shields.io/badge/license-BSD--3--Clause-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Security](https://img.shields.io/badge/security-audited-green)](docs/SECURITY_AUDIT.md)

A modern chunk-based image format inspired by PNG, with support for ZSTD compression, advanced predictive filters (16 types), indexed palette, structured metadata (EXIF, JSON, ICC, XMP), and progressive interlacing.

**Version**: 1.2.1  
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
- Applied per block (tile) for maximum efficiency
- **AVX2 SIMD acceleration** (v1.1+): Filters 1 (Sub), 2 (Up), 3 (Average) vectorized for 4-8x faster processing; automatic CPU detection with scalar fallback
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
│   ├── simd.rs                    # AVX2 vectorized filters 1-3 (v1.1+, optional feature)
│   ├── shuffle.rs                 # Byte-shuffle (Filter Method=1, v1.1)
│   ├── tonemap.rs                 # HDR tone-mapping (EOTF, primaries, operators, v1.1)
│   ├── interlace.rs               # Adam7 and even/odd
│   ├── types.rs                   # EncodeOptions, Palette, iDim, cHDR, etc.
│   └── error.rs                   # CafeError
├── tools/                         # CLI tools
│   ├── cafe-encode.rs            # Encoder binary
│   └── cafe-decode.rs            # Decoder binary
├── docs/                          # Documentation
│   ├── CAFE-spec.md              # Complete specification (v1.2)
│   ├── CAFE-spec.pt.md           # Portuguese version of the spec (v1.2)
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

**Note on SIMD:** The `simd` feature is enabled by default. It uses AVX2 intrinsics for Filters 1, 2, and 3, automatically falling back to scalar code on CPUs without AVX2 or when SIMD is disabled.

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
- [ ] NEON support (ARM SIMD)

---

## 📈 Roadmap

| Version | Features | Status |
|---------|----------|--------|
| **v1.0** | Critical chunks, ZSTD, 14 filters, metadata (EXIF/JSON/ICC/XMP/HDR), zDIC, sample_format float/half, security | ✅ Complete |
| **v1.1** | Filters 14-15: TR-Directional (WebP Predictor 10) and adaptive Weighted (inspired by JPEG-XL) — 16 total predictors; MSAD heuristic; real 2D tiling (iDIM) with end-to-end round-trip; byte-shuffle encode/decode; **AVX2 SIMD optimization (Filters 1-3)** | ✅ Complete |
| **v1.2** | **Aggressive SIMD Acceleration (AVX2 x86_64)**: Pack/Unpack 1/2/4-bit (8-16x), Sample expansion/reduction (4-6x), Byte-shuffle with blocking (10-20%), Filter 3 enhanced (4-6x); **252 comprehensive tests** (197 unit + 6 integration roundtrip + 49 SIMD), **Zero TODOs/FIXMEs**, Feature-gated SIMD with CPU detection | ✅ Complete |
| **v1.2.1** | Refinements and operator dispatcher for tone-mapping selection | ✅ Complete |
| **Future** | Additional compressors, NEON SIMD (ARM), enhanced progressive streaming | 🔮 Planned |

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

**Last updated**: 2026-08-11 (v1.2.1: SIMD fully integrated, tone-mapping operator dispatcher, 252 comprehensive tests)  
**Test Coverage**: 197 unit tests + 6 roundtrip integration tests + 49 SIMD-specific tests = 252 total  
**Next security audit**: 2027-08-04
