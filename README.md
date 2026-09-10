# CAFE — Compression Adaptive Filtering Experiment

[![License](https://img.shields.io/badge/license-BSD--3--Clause-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)

A chunk-based image format inspired by PNG: instead of reinventing a
general-purpose compressor, CAFE adaptively transforms pixels (per-row
predictors) so that Zstandard compresses them exceptionally well.

**Format Version**: 0.1 — see [`spec/CAFE-spec.md`](spec/CAFE-spec.md)
**Status**: initial implementation, built decoder-first from a small,
boringly-simple spec. See [Status](#status) and [`AGENTS.md`](AGENTS.md)
for the full phased roadmap, design rationale, and per-phase implementation
notes.
**Compatibility**: Rust 2021+

---

## Status

All phases through **Phase 10 (benchmark vs. PNG) plus a corpus-population
follow-up** are complete: chunk framing and `IHDR`/`iDIM` validation
(`cafe-format`), a full scalar decode/encode path with 2D tiling
(`cafe-codec`), fuzzing/proptest robustness coverage, a real CLI
(`cafe-cli`: `inspect`/`verify`/`explain`/`benchmark` plus
`cafe-encode`/`cafe-decode`), and a benchmark harness (`cafe-bench`) are all
implemented and tested (206 tests across the workspace).

Running `cargo run -p cafe-cli --bin cafe -- benchmark` against the full
22-entry `corpus/manifest.json` (8 synthetic gradient/pixelart/texture/
synthetic images plus 14 real photo/lineart/illustration/screenshot images
— see [`corpus/ATTRIBUTION.md`](corpus/ATTRIBUTION.md) for sourcing/
licensing) shows CAFE beating PNG on **every single entry**:

```
TOTAL: raw=3760128 png=1324113 (35.2%) cafe=795462 (21.2%)
```

Per `AGENTS.md`'s "every feature proves itself with a benchmark first"
principle, SIMD, indexed palette, and ZSTD dictionary support (all planned
for 0.2+) are currently a **no-go**: nothing in this corpus shows scalar
predictor/ZSTD throughput as a bottleneck, no category has a
palette-favorable small-distinct-color profile that isn't already
near-optimal, and this corpus's images are large/varied singles, not the
small/similar/many-files scenario a shared dictionary would pay off on. See
`AGENTS.md`'s Phase 10 entry for the full reasoning.

## Key Features

### Chunk container
- PNG-style framing: `Length` / `Type` / `Flag` / `Data` / `CRC32`
- Critical vs. ancillary chunk types (uppercase/lowercase first letter)
- ZSTD compression with a raw-data fallback per chunk (section 3.2 of the
  spec) — whichever is smaller wins

### Predictors
- **6 predictors**: None, Sub, Up, Average, Paeth, Gradient — chosen
  **per row** (not per block/tile), the only filtering granularity 0.1
  supports
- Encoder heuristic: Shannon-entropy scoring picks the lowest-entropy
  candidate per row, ties favor the smaller predictor code

### Tiling
- Single `iDIM`-based mechanism: default single implicit tile (no `iDIM`
  chunk), or an explicit 2D tile grid (any size up to `MAX_TILE_COUNT`)
- Row-major or Z-order (Morton) scan order
- Edge-truncated tiles when width/height aren't exact multiples of the
  tile size

### Color & sample formats
- **Color types**: Grayscale, RGB, Grayscale+Alpha, RGBA (no indexed
  palette in 0.1 — deferred to 0.3 as an encoder-side transform)
- **Bit depths**: uint8, uint16, float32
- Big-endian multi-byte fields and samples throughout

### Metadata
- `eXIF` (TIFF blob), `jSON` (multiple instances per namespace), `iCCP`
  (ICC profile), `xMPd` (XMP) — all ancillary; a decoder must be able to
  decode pixels without understanding any of them

### Security
- Decompression-bomb protection (CWE-409): decompression bounded by a
  caller-given limit, capped at `MAX_DECOMPRESSED_CHUNK_SIZE` (1 GiB),
  never trusting a ZSTD frame header's declared size
- Tile-count ceiling (CWE-789/CWE-409): `iDIM`'s `tiles_x * tiles_y`
  checked against `MAX_TILE_COUNT` before any tile-proportional allocation
- No panics on malformed/truncated/adversarial input — exercised by 12
  hand-written adversarial cases plus proptest properties (arbitrary bytes,
  bit-flips, truncations) at both the chunk-parsing and whole-file-decode
  layers, and (locally, Linux/nightly) two libFuzzer harnesses

### What's deferred (not in 0.1)
Indexed palette (0.3), ZSTD dictionary (0.4), SIMD (0.2), Adam7/even-odd
interlace (removed permanently), full HDR tone-mapping (unscheduled) — see
`spec/CAFE-spec.md` section 11 and `AGENTS.md`'s "What v0.1 keeps, defers,
or removes" table for the full list and rationale.

---

## Project Structure

```
Cafe/
├── AGENTS.md                  # Design rationale + full phased roadmap
├── Cargo.toml                 # Workspace manifest
├── LICENSE                    # BSD-3-Clause
├── crates/
│   ├── cafe-format/           # Chunk framing, IHDR/iDIM, validation
│   ├── cafe-codec/            # Predictors, ZSTD, tiling, Encoder/Decoder
│   ├── cafe-cli/              # bin "cafe" + cafe-encode/cafe-decode
│   └── cafe-bench/            # Benchmark harness vs. PNG, corpus tooling
├── spec/
│   ├── CAFE-spec.md           # Normative Format 0.1 specification
│   ├── invariants/*.toml      # Spec rules as machine-readable data
│   └── test-vectors/
├── corpus/                    # Synthetic + real benchmark images + manifest.json
│   └── ATTRIBUTION.md         # Source/author/license for every real image
├── golden/                    # Hand-built valid + malformed .cafe fixtures
│   └── malformed/
├── fuzz/                      # Standalone cargo-fuzz workspace (Linux/nightly)
└── .github/workflows/         # CI (build/clippy/fmt/test) + nightly fuzz
```

No umbrella `cafe` library crate — consumers depend on `cafe-format` and/or
`cafe-codec` directly. The CLI binary is `cafe` (in `cafe-cli`), alongside
the legacy-named `cafe-encode`/`cafe-decode` binaries.

---

## Architecture

### Chunk layout
```
[Length: 4 bytes BE]
[Type: 4 bytes ASCII]
[Flag: 1 byte] — 0x00=raw, 0x01=ZSTD
[Data: N bytes] — content (compressed or not)
[CRC32: 4 bytes BE] — covers Type+Flag+Data
```

### Defined chunks

**Critical** (1st letter uppercase — decoder must understand or reject):

| Type | Description |
|------|-----------|
| `IHDR` | Header, 12-byte payload (always first, never compressed) |
| `IDAT` | Pixel data (1 or more per file, one per tile) |
| `IEND` | End marker (always last, zero-length payload) |

**Ancillary** (1st letter lowercase — decoder may safely ignore):

| Type | Description |
|------|-----------|
| `iDIM` | Tiling and scan order (9-byte payload; absent = single implicit tile) |
| `eXIF` | EXIF metadata (TIFF blob) |
| `jSON` | JSON metadata (multiple instances per namespace) |
| `iCCP` | ICC color profile |
| `xMPd` | XMP metadata |

Mandatory chunk order: `IHDR` → `iDIM` → `eXIF` → `jSON` → `iCCP` →
`xMPd` → `IDAT`(s) → `IEND`. See `spec/CAFE-spec.md` section 5.

---

## Usage

### Building

```bash
cargo build --release

# Executables
./target/release/cafe-encode input.png output.cafe
./target/release/cafe-decode output.cafe decoded.png
./target/release/cafe inspect output.cafe
```

v0.1's CLI scope is deliberately narrow: PNG-only, 8-bit-only I/O
(`cafe-codec` itself already supports uint16/float32 — CLI support for
those is a later pass).

### CLI

```bash
# Encode (single implicit tile, default ZSTD level 19)
cafe-encode input.png output.cafe

# Encode with 2D tiling and Z-order scan
cafe-encode input.png output.cafe --tile-size 64x64 --scan-order z

# Encode without ZSTD (every IDAT written raw)
cafe-encode input.png output.cafe --no-zstd

# Decode
cafe-decode output.cafe decoded.png

# Inspect chunk layout / validate against the spec / explain predictor choices
cafe inspect output.cafe
cafe verify output.cafe
cafe explain output.cafe

# Benchmark a corpus against PNG
cafe benchmark corpus/
```

### Library API

```rust
use cafe_codec::{encode_bytes, decode_bytes, EncoderOptions};
use cafe_format::constants::{COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT};

// Encode a whole in-memory RGBA8 buffer to CAFE bytes
let cafe_bytes = encode_bytes(
    width,
    height,
    8,                  // bit_depth
    SAMPLE_FORMAT_UINT,
    COLOR_TYPE_RGBA,
    &raw_rgba_pixels,
    EncoderOptions::default(),
)?;

// Decode back
let decoded = decode_bytes(&cafe_bytes)?;
assert_eq!(decoded.pixels, raw_rgba_pixels);
```

For true streaming (one tile at a time, writing each `IDAT` immediately),
use `cafe_codec::Encoder<W>` directly:

```rust
use cafe_codec::{Encoder, EncoderOptions};
use cafe_format::constants::{COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT};
use std::fs::File;

let file = File::create("output.cafe")?;
let options = EncoderOptions { tile_size: Some((64, 64)), ..Default::default() };
let mut encoder = Encoder::new(file, width, height, 8, SAMPLE_FORMAT_UINT, COLOR_TYPE_RGBA, options)?; // writes IHDR + iDIM immediately

for tile_pixels in tiles_in_scan_order {
    encoder.add_tile(&tile_pixels)?; // writes that tile's IDAT immediately
}

let _file = encoder.finish()?; // writes IEND
```

`cafe-format` is also usable on its own for low-level chunk work
(`cafe_format::chunk::{read_chunk, write_chunk}`, `cafe_format::Ihdr`,
`cafe_format::Idim`).

---

## Performance

See [Status](#status) above for the current full-corpus benchmark. In
short: CAFE beats PNG on every entry tried so far, from a ~2x margin on
mid-frequency synthetic texture content up to two orders of magnitude on
smooth gradients/flat pixel art, and a comfortable margin (roughly half the
bytes of PNG) on real photos/screenshots/line art. Run it yourself with:

```bash
cargo run -p cafe-cli --bin cafe -- benchmark
cargo run -p cafe-bench --bin cafe-bench   # in-memory synthetic matrix + criterion benches
```

SIMD is deferred to 0.2 and has not been implemented or benchmarked yet —
current numbers are all scalar.

---

## Security

- No panics on untrusted input — return `Result`/`CafeError`/`CodecError`
  instead (spec section 8.1)
- Decompression-bomb protection: 1 GiB per-chunk ceiling, plus an
  expected-payload-size limit derived from `IHDR`/`iDIM` rather than the
  generic ceiling wherever the exact size is knowable
- Tile-count ceiling checked before any tile-proportional allocation
- Adversarial-input test coverage: hand-written truncation/bit-flip sweeps
  against golden fixtures, proptest properties at the chunk and whole-file
  layers, and standalone libFuzzer harnesses (`fuzz/`, Linux/nightly only)

---

## Dependencies

```toml
crc32fast = "1.3"   # CRC32 for chunk framing
zstd = "0.13"       # Compression
image = "0.25"      # PNG read/write (cafe-cli, cafe-bench only)
serde_json = "1.0"  # JSON metadata / corpus manifest
```

---

## Documentation

- [`spec/CAFE-spec.md`](spec/CAFE-spec.md) — normative Format 0.1
  specification (signature, chunk structure, `IHDR`/`iDIM`/`IDAT`/metadata
  chunks, mandatory order, streaming, security, versioning, future
  extensions)
- [`spec/invariants/*.toml`](spec/invariants/) — the same rules as
  machine-readable data, cross-checked for consistency by
  `crates/cafe-format/tests/spec_invariants.rs`
- [`AGENTS.md`](AGENTS.md) — design rationale, phased roadmap, and a
  detailed changelog-style entry per completed phase

---

## License

Licensed under **BSD-3-Clause** — see [LICENSE](LICENSE).

---

## Contributing

The phased roadmap in [`AGENTS.md`](AGENTS.md) is the source of truth for
what's done, what's deferred, and why. Before proposing a 0.2+ feature
(SIMD, palette, dictionary), check `AGENTS.md`'s Phase 10 go/no-go
verdicts — per this project's guiding principle, "every feature proves
itself with a benchmark first" (`cafe-bench`/`cafe benchmark` exist
specifically to make that case).

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
