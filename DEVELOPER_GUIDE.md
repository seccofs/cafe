# CAFE Developer Guide

This guide covers setup, testing, benchmarking, and fuzzing for CAFE development.

## Fuzzing with cargo-fuzz

### Why Fuzz?

Fuzzing ensures that `decode_bytes()` never panics on arbitrary input—it should only return `Err(CafeError::...)`. This is critical for handling malicious or malformed files.

### Available Harnesses

- **`decode_fuzz`**: Fuzzes the core `decode_bytes()` function with arbitrary byte sequences.
- **`chunk_roundtrip_fuzz`**: Fuzzes chunk parsing (read/write) indirectly via `decode_bytes()`.

### Setup (Linux/macOS only)

Fuzzing requires Rust nightly and a Unix-like OS (libFuzzer is not supported on Windows MSVC).

```bash
# Switch to nightly
rustup default nightly

# Initialize fuzzing infrastructure (already done; just for reference)
cargo fuzz init
```

### Running Fuzzing

```bash
# Run decode_fuzz for ~10 minutes locally to find basic crashes
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10

# Run overnight on CI for 1+ hours
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10 -max_total_time=3600

# Run chunk roundtrip fuzz
cargo fuzz run chunk_roundtrip_fuzz -- -max_len=16384 -timeout=10
```

### Interpreting Results

- **Success**: Fuzzer runs for the specified time without finding any panics → code is robust.
- **Crash Found**: libFuzzer will save a minimal crash input to `fuzz/artifacts/decode_fuzz/` (or `chunk_roundtrip_fuzz/`).
  - Examine the crash input with: `xxd fuzz/artifacts/decode_fuzz/<crash_file>`
  - Run the crash again: `cargo fuzz run decode_fuzz fuzz/artifacts/decode_fuzz/<crash_file>`
  - Minimize the crash: `cargo fuzz cmin decode_fuzz` (corpus minimization)

### Local Development Workflow

```bash
# Quick sanity check (before fuzzing for hours)
cargo test --lib
cargo clippy -- -D warnings

# Then run fuzzing locally for 10-15 minutes
cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10 -runs=1000000

# If no crashes, you're good!
```

### CI Integration (Recommended)

In `.github/workflows/fuzz.yml`:

```yaml
name: Fuzz

on:
  schedule:
    - cron: '0 2 * * *'  # Run nightly at 2 AM UTC
  workflow_dispatch:

jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo fuzz run decode_fuzz -- -max_len=16384 -timeout=10 -max_total_time=3600
      - run: cargo fuzz run chunk_roundtrip_fuzz -- -max_len=16384 -timeout=10 -max_total_time=3600
      - uses: actions/upload-artifact@v3
        if: failure()
        with:
          name: crash-artifacts
          path: fuzz/artifacts/
```

---

## Testing with proptest

### Property-Based Testing

Property tests use randomized inputs to find edge cases. CAFE uses `proptest` for:

- Random width/height (1..=32)
- Random color types and bit depths
- Random sample formats (uint/float/half)
- Random interlace methods and filter heuristics
- Verifying pixel round-trip accuracy

### Running Property Tests

```bash
# Run with default case count (usually 64-256)
cargo test --test '*' -- --nocapture

# Run with more cases (slower, more thorough)
PROPTEST_CASES=1000 cargo test --test '*'

# Run a specific property test
cargo test prop_roundtrip_arbitrary_config -- --nocapture

# Show shrunk failure case
RUST_BACKTRACE=1 cargo test prop_roundtrip_arbitrary_config
```

### Understanding Failures

When proptest finds a failure, it:
1. **Shrinks** the failing input to a minimal reproducer
2. **Saves** the seed to `proptest-regressions/` for deterministic re-runs
3. **Shows** the shrunk input in error output

Example:
```
Error: test failed as expected, and shrinking discovered smaller failing inputs.
The smallest failing input after 45 iterations was:
seed: [12345, 67890]
config: ColorType::GRAY, bit_depth=1, interlace=ADAM7, filter=Entropy
```

---

## Benchmarking with criterion

### What Gets Benchmarked

- **`encode()`** with levels 1, 9, 19, 22 and use_filter on/off
- **`decode()`** on various image types
- **`encode_indexed()`** on palette images
- **Comparison**: PNG vs CAFE encoding time for the same image

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run a specific benchmark
cargo bench encode_level

# Run with verbose output
cargo bench -- --verbose

# Generate HTML report (usually in target/criterion/report/index.html)
cargo bench -- --verbose
# Open the report
open target/criterion/report/index.html
```

### Performance Baseline

After running benchmarks, update `README.md` with real numbers:

```markdown
### Benchmarks (v1.1, 512x512 RGB image, measured locally)

| Operation | Time | Notes |
|-----------|------|-------|
| encode (level 19, with filter) | 45 ms | -X% vs PNG |
| encode (level 1, no filter) | 12 ms | fastest |
| decode (RGBA) | 8 ms | single-threaded |
| encode_indexed (256 colors) | 22 ms | palette mode |
```

---

## Build & Test Workflow

### Full Test Suite

```bash
# Run all library tests
cargo test --lib

# Run all integration tests
cargo test --test '*'

# Run with all features
cargo test --all-features

# Run release build (faster, optimized)
cargo test --release
```

### Linting & Formatting

```bash
# Check for warnings (same as CI)
cargo clippy -- -D warnings

# Auto-format code
cargo fmt

# Check formatting without changes
cargo fmt -- --check
```

### Security Audit

```bash
# Requires: cargo install cargo-deny
cargo deny check
```

### Documentation

```bash
# Generate and open docs
cargo doc --no-deps --open

# Check for broken doc links
cargo test --doc
```

---

## Quick Reference

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

---

## Common Issues

### Fuzzing fails to link on Windows

**Root cause**: libFuzzer requires Unix-like systems. Windows MSVC linking doesn't support the `#[no_main]` entry point required by libFuzzer.

**Solution**: Run fuzzing on a Linux CI machine or in WSL2. Local development on Windows should use property tests instead.

### Property test takes too long

**Solution**: Reduce `PROPTEST_CASES` or run specific tests:

```bash
PROPTEST_CASES=64 cargo test prop_roundtrip
```

### Criterion benchmarks timeout

**Solution**: Increase timeout or reduce sample count:

```bash
cargo bench -- --sample-size=100
```

---

## CLI Parity: EncodeOptions ↔ cafe-encode Flags (v1.1+)

This table tracks completeness of CLI flag coverage for `EncodeOptions` fields.
**Goal**: all public library features must be accessible via CLI.

| Field | CLI Flag | Status | Notes |
|-------|----------|--------|-------|
| `use_filter` | `--no-filter` | ✅ | Inverse logic; default=true (filter on) |
| `use_byte_shuffle` | `--byte-shuffle` | ✅ | v1.1, for HDR/float data |
| `level` | `--level <1-22>` | ✅ | ZSTD compression (default: 19) |
| `adaptive_analysis` | `--adaptive` | ✅ | Local complexity per tile |
| `target_color_type` | `--color-type <0\|2\|4\|6>` | ✅ | 0=GRAY, 2=RGB, 4=GRAY_ALPHA, 6=RGBA |
| `target_bit_depth` | `--bit-depth <d>` | ✅ | 1,2,4,8,10,12,16,32 (uint only) |
| `json_metadata` | `--json-file <path>` | ✅ | Reads JSON from file |
| `exif` | `--exif-file <path>` | ✅ | Raw EXIF binary blob |
| `icc_profile` | ❌ | ❌ **MISSING** | Could add `--icc-profile <path>` |
| `xmp_metadata` | ❌ | ❌ **MISSING** | Could add `--xmp-file <path>` |
| `idim` | ❌ | ❌ **NOT IMPL** | 2D tiling (internal feature, rare) |
| `interlace_method` | `--interlace <0\|1\|2>` | ✅ | 0=none, 1=Adam7, 2=even/odd |
| `zstd_dictionary` | `--chdr-dict-file <path>` | ✅ | Pre-trained ZSTD dict |
| `sample_format` | `--sample-format <0\|1\|2>` | ✅ | 0=uint, 1=float, 2=half-float |
| `chdr_metadata` | `--chdr-transfer`, `--chdr-primaries`, `--chdr-max-lum`, `--chdr-min-lum` | ✅ | HDR tone-mapping metadata |
| `filter_heuristic` | `--filter-heuristic <h>` | ✅ | entropy, msad, test, quick-prune, adaptive |
| `auto_dictionary` | `--auto-dict` | ✅ | v1.1, auto-train ZSTD dict |
| `palette_algorithm` | `--palette-algorithm <a>` | ✅ | v1.1, nearest or median-cut |

### DecodeResult Fields Accessibility

| Field | CLI Export | Status | Notes |
|-------|-----------|--------|-------|
| `width` | Implicit (file only) | ✅ | Encoded in output image dimensions |
| `height` | Implicit (file only) | ✅ | Encoded in output image dimensions |
| `exif` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |
| `json_metadata` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |
| `compression_stats` | ❓ | ❌ **MISSING** | Could add `--show-stats` flag |
| `icc_profile` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |
| `xmp_metadata` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |
| `zstd_dictionary` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |
| `chdr_metadata` | ❓ | ⚠️ **PARTIAL** | Present in library, not exported to CLI |

### Notes

- **✅ Complete**: Field has CLI flag; default behavior is correct
- **⚠️ Partial**: Field exists in library but not exposed in CLI (could improve)
- **❌ Missing**: Field is missing both in library and CLI (needs implementation)
- **IDM (2D tiling)**: Rarely used; low priority for CLI exposure
- **Metadata export**: Decode results (EXIF, JSON, etc.) exist in library but CLI doesn't save them separately

---

## Contribution Checklist

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

**Last updated**: August 11, 2026  
**CAFE version**: v1.2.1
