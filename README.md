# CAFE

CAFE (Compression Adaptive Filtering Experiment) is a chunk-based image
format that transforms pixels adaptively so that a general-purpose
compressor (Zstandard) compresses them exceptionally well — not an attempt
to reinvent a general compressor.

This is a ground-up reboot of the original `cafe-rs` v1.x implementation.
The previous implementation is preserved, untouched, in [`old/`](old/) for
reference. The reboot's design rationale and phased roadmap live in
[`AGENTS.md`](AGENTS.md).

## Status

**Phase 10 - Benchmark vs PNG.** The format/framing layer (Phase 4), the
full scalar decode path (Phase 5), a streaming scalar encode path (Phase
6), full 2D tiling (Phase 7 - `iDIM`, row-major or Z-order/Morton scan,
edge-truncated tiles, any tile count up to `MAX_TILE_COUNT`),
fuzzing/robustness coverage (Phase 8), a real CLI (Phase 9 -
`cafe inspect/verify/explain/benchmark` plus `cafe-encode`/`cafe-decode`),
and now a full-corpus benchmark run (Phase 10) are implemented and tested.
`.cafe` files can be produced single-tile or multi-tile by
`cafe_codec::encode_bytes`/`Encoder`, and round-tripped to/from PNG
entirely via the `cafe-cli` binaries, not just by test fixture generators.

Running `cargo run -p cafe-cli --bin cafe -- benchmark` against the full
`corpus/manifest.json` — 8 synthetic images (gradient/pixelart/texture/
synthetic x 64x64/256x256) plus, as of the corpus-population follow-up to
Phase 2, 14 real-content images (4 Kodak photos, 4 public-domain line-art
illustrations, 2 public-domain colour illustrations, 4 locally-captured
Windows screenshots — see `corpus/ATTRIBUTION.md` for full sourcing/
licensing) — shows CAFE beating PNG on every single entry, synthetic or
real:

```
TOTAL: raw=3760128 png=1324113 (35.2%) cafe=795462 (21.2%)
```

Per `AGENTS.md`'s "every feature proves itself with a benchmark first"
principle, Phase 10's go/no-go verdict for 0.2+ features was **no-go for
now** on SIMD, palette, and dictionary, based on the synthetic-only corpus
available at the time (see `AGENTS.md`'s Phase 10 entry for the full
reasoning). Real photo/line-art/illustration/screenshot content narrows
CAFE's margin over PNG somewhat (e.g. photos land around 45-62% of raw
size, vs. near-0% on smooth synthetic gradients) but doesn't overturn that
verdict — CAFE still beats PNG by a comfortable margin on every real image
tried. `corpus/hdr/` now has two staged OpenEXR fixtures
(`Blobbies.exr`/`Cannon.exr`), but they're intentionally not wired into
`manifest.json` yet, since `cafe-cli`'s PNG-only 8-bit I/O bridge
(`png_io`) doesn't support float32/EXR — HDR benchmarking remains a
follow-up CLI pass, not a Phase 10 change.

- `spec/CAFE-spec.md` - normative Format 0.1 spec, simplified from
  `old/docs/CAFE-spec.md` (12-byte `IHDR`, no interlace, no palette, 6
  predictors chosen per-row, single `iDIM` tiling mechanism). See its
  section 11 for what's deferred to 0.2+.
- `spec/invariants/*.toml` - the same rules as machine-readable data
  (signature bytes, chunk critical/ancillary flags and mandatory order,
  `IHDR`/`iDIM` field layouts, predictor codes, security ceilings),
  cross-checked for internal consistency by
  `crates/cafe-format/tests/spec_invariants.rs`.
- `cafe-format` - chunk container (Length/Type/Flag/Data/CRC32 framing,
  adapted from `old/src/chunk.rs`), signature validation, `IHDR`
  parsing/serialization/validation, all with adversarial-input tests (never
  panics on truncated/forged/corrupted input — spec section 8.1). Golden
  `.cafe` fixtures (hand-built via a fixture generator, not by an encoder
  that doesn't exist yet) live in `golden/` (valid) and `golden/malformed/`
  (adversarial), tested via `crates/cafe-format/tests/golden_files.rs`. This
  crate deliberately has no predictor/pixel logic — `IDAT` payloads are
  opaque bytes from its point of view.
- `cafe-codec` — predictors, ZSTD, tiling, streaming `Encoder`/`Decoder`.
  Phase 5 implemented the full scalar **decode** path, Phase 6 added the
  scalar **encode** path, and Phase 7 adds full 2D **tiling**:
  - `predictor` — the 6 spec predictors (None/Sub/Up/Average/Paeth/
    Gradient) behind `filter_row`/`unfilter_row`, byte-exact against
    known values and Paeth's PNG reference cases; plus (Phase 6)
    `choose_best_row_predictor`, a per-row Shannon-entropy heuristic that
    tries all 6 codes and keeps the lowest-entropy result (ties favor the
    smaller code).
  - `zstd_codec` — `compress_with_fallback` (raw-vs-ZSTD, adapted from
    `old/src/codec.rs`) and `decompress_with_limit`/`decompress_chunk`,
    which bound decompressed size to guard against decompression bombs.
  - `tile` — `encode_tile_rows`/`decode_tile_rows` (fixed predictor code
    for the whole tile) and (Phase 6) `encode_tile_rows_auto`, which calls
    `choose_best_row_predictor` independently for every row.
  - `morton` — (Phase 7) `morton_code`/`morton_decode`, naive bit
    interleaving used to derive Z-order scan sequences.
  - `tiling` — (Phase 7) `tile_order` (enumerates `(tile_x, tile_y)`
    positions in row-major or Morton order), `tile_origin` (grid position
    -> pixel-space offset), and `TileLayout` (unifies the "no `iDIM`" and
    "explicit `iDIM`" cases behind one `tile_rect(index)` API shared by the
    decoder and encoder).
  - `decoder` — `decode_bytes` ties it together: validates the signature,
    reads `IHDR`, accepts an optional `iDIM` describing any tile grid up
    to `MAX_TILE_COUNT` (row-major or Z-order, edge-truncated tiles),
    assembles each `IDAT` into the correct rectangular region of one
    pre-allocated pixel buffer, reverses predictors per tile, and requires
    `IEND`. Rejects unknown critical chunks, duplicate/misplaced `iDIM`,
    `IDAT` count mismatches against `iDIM`, and missing `IDAT`/`IEND`;
    skips unknown ancillary chunks. Validated against every `golden/`
    fixture from Phase 4 (2 valid, 6 malformed) plus targeted single- and
    multi-tile round-trip and adversarial tests.
  - `encoder` — `Encoder<W>`/`EncoderOptions` and the `encode_bytes` sugar
    function: validates `IHDR` (and, since Phase 7, `iDIM`) fields, writes
    `IHDR`/`iDIM` eagerly in `new()`, accepts one tile at a time via
    `add_tile` (writing that tile's `IDAT` immediately — true streaming),
    and `finish()` writes `IEND`. `EncoderOptions::tile_size` (`None` by
    default) opts into `iDIM` emission and multi-tile splitting; `None`
    reproduces the single-implicit-tile Phase 6 behavior byte-identically
    for the `minimal_1x1_gray` golden (the `minimal_2x2_rgba` golden's
    *pixels* match but not its bytes, since the per-row heuristic
    legitimately picks a better predictor than that golden's hand-picked
    `None`). Round-tripped against the decoder across gray/RGBA, 8/16-bit,
    tiny/64x64, uniform/varied content, both ZSTD/raw fallback outcomes,
    and (Phase 7) multi-tile row-major/Z-order/partial-edge-tile layouts.
- `fuzz/` — standalone Cargo workspace (not a member of the root
  workspace — `cargo fuzz` needs nightly + libFuzzer's `#[no_main]`,
  incompatible with the root's stable `build`/`test`/`clippy`), crate
  `cafe-fuzz` with two harnesses ported from `old/fuzz/fuzz_targets/`:
  `decode_fuzz` (`cafe_codec::decode_bytes`) and `chunk_roundtrip_fuzz`
  (`cafe_format::chunk::read_chunk` directly, plus `decode_bytes`). Real
  libFuzzer execution needs Linux/nightly (fails to *link* on Windows
  MSVC — a known libFuzzer coverage-instrumentation limitation, not a bug
  here); CI runs both harnesses as a 60s smoke test on every push/PR
  (`.github/workflows/ci.yml`'s `fuzz` job) and for a full hour nightly
  (`.github/workflows/fuzz.yml`). Local Windows development instead relies
  on proptest-based "never panic on adversarial input" tests:
  `crates/cafe-codec/tests/decode_robustness.rs` (12 hand-written
  adversarial cases plus exhaustive truncation/bit-flip sweeps of a golden
  fixture), `crates/cafe-codec/tests/roundtrip_proptest.rs` (3 properties:
  arbitrary bytes never panic `decode_bytes`, with or without a genuine
  signature prefix, and `encode_bytes`/`decode_bytes` round-trips exactly
  across random small configs), and `crates/cafe-format/tests/
  chunk_proptest.rs` (3 properties, same treatment one layer down at
  `read_chunk`).
- `cafe-cli` — binaries `cafe` (`inspect`/`verify`/`explain`/`benchmark`
  subcommands), `cafe-encode`, `cafe-decode`. v0.1 scope: PNG only, 8-bit
  only (`png_io` bridges `image::DynamicImage` <-> CAFE's raw-pixel shape,
  picking the narrowest of the four color types via
  `ColorType::has_alpha()`/`has_color()`; 16-bit/float32 CLI I/O is
  deferred even though `cafe-codec` itself supports them). `cafe-encode`
  exposes `--tile-size WxH`/`--scan-order row|z`/`--level N`/`--no-zstd`
  hand-rolled flags over `EncoderOptions`; `cafe-decode` reverses via
  `decode_bytes` + `DynamicImage::save`. `cafe inspect` lists chunk framing
  plus parsed `IHDR`/`iDIM`; `cafe verify` re-validates signature/`IHDR`/
  `iDIM`/chunk presence and does a full `decode_bytes` pass, reporting
  every problem found; `cafe explain` decompresses every `IDAT` and prints
  a predictor-code histogram plus compressed-vs-raw totals, built on
  `cafe_codec::tiling::TileLayout` so its tile accounting can never drift
  from the decoder's; `cafe benchmark` is a thin wrapper reading a
  corpus's `manifest.json` and calling `cafe_bench::measure()` per image.
- `cafe-bench` — comparative benchmark harness. Generates a small
  deterministic synthetic matrix (gradient/checkerboard/noise, no external
  `rand` dependency) and (since Phase 6) compares PNG against the real
  `cafe-codec` encoder, alongside a naive raw-ZSTD floor:

  ```bash
  cargo run -p cafe-bench --bin cafe-bench
  ```

  Real CAFE already beats PNG by a wide margin on every synthetic pattern
  tried so far (e.g. ~0.6% of raw size vs PNG's ~2.9% on a 64x64 gradient,
  ~30% vs PNG's ~101% on LCG-based "noise" at the same size) — encouraging
  for a scalar, unoptimized encoder, but this binary's matrix is generated
  in-memory, not walked from disk. For the on-disk `corpus/manifest.json`
  full-corpus comparison (Phase 10's actual benchmark run), use
  `cafe benchmark` instead — see [Status](#status) above; these are still
  small deterministic synthetic images, not the real photo/screenshot
  content still missing from `corpus/`.

- `corpus/` — deterministic synthetic images (`gradient`/`pixelart`/
  `texture`/`synthetic`, regenerate with
  `cargo run -p cafe-bench --bin gen-corpus`) plus curated real-content
  images (`photo`/`screenshot`/`illustration`/`lineart`), all described by
  `corpus/manifest.json` (path, category, dimensions, color type, bit
  depth, SHA-256). Real content is imported/resized (capped at 256x256,
  never upscaled) via `cafe_bench::import` and:

  ```bash
  cargo run -p cafe-bench --bin import-corpus -- <staging-dir> [corpus-dir]
  ```

  Source, author, and license for every real image (Kodak photo suite,
  Wikimedia public-domain line art/illustrations, locally-captured Windows
  screenshots) is documented in `corpus/ATTRIBUTION.md`. `corpus/hdr/`
  holds two staged OpenEXR fixtures for a future HDR CLI pass; they're not
  yet referenced by `manifest.json` (see [Status](#status) above).

See `AGENTS.md` for the full phased implementation plan.

## Building

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## License

BSD-3-Clause, see [LICENSE](LICENSE).
