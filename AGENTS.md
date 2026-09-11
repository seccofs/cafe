# CAFE — Developer Guide & Roadmap

## Design philosophy

CAFE's core insight is simple: don't invent another general-purpose
compressor — adaptively transform pixels so that Zstandard compresses them
exceptionally well. Around that insight, this project is built as a small,
boringly-simple format with benchmarking and a golden corpus from commit
zero, decoder-first, with a single streaming-first API. The process is
`hypothesis -> benchmark -> minimal feature -> spec -> golden vectors ->
implementation` for every feature, not `problem -> feature -> another
problem -> another feature`.

## Guiding principles

1. **Decoder before encoder.** `spec -> minimal decoder -> golden files ->
   encoder -> optimization`. The decoder defines what the format *is*; the
   encoder can change heuristics forever without touching the format.
2. **Boringly simple.** The ideal decoder reads: header -> chunks ->
   decompress ZSTD -> undo predictor -> reconstruct pixels. No nested
   `if mode == X { if mode == Y { unless palette { ... } } }`.
3. **Every feature proves itself with a benchmark first.** `cafe-bench`
   exists before `cafe-codec` has anything to encode.
4. **One streaming-first API.** `Encoder<W>`/`Decoder<R>` from day one;
   `encode(image)` is sugar over `Encoder::new(...).add_tile(...).finish()`.
   No parallel "batch" vs "streaming" option structs.
5. **Encoder is smart, decoder is fixed.** Every encoder capability
   (predictor choice, palette algorithm, dictionary, tile size, scan
   order) is optional and encoder-side. The decoder needs to know a small,
   fixed set of primitives.

## Core v0.1 design decisions

| Area | v0.1 decision |
|---|---|
| Chunk container (Length/Type/Flag/Data/CRC32, critical/ancillary naming) | PNG-style framing, kept intentionally simple |
| ZSTD with raw-vs-compressed fallback | Per-chunk race between raw and ZSTD; whichever is smaller wins |
| Security ceilings (decompression budget, tile count) | Hard ceilings checked before any proportional allocation (CWE-409, CWE-789) |
| Predictors | 6 total: None, Sub, Up, Average, Paeth, Gradient — behind a `trait Predictor` |
| Per-row predictor selection | Structural from day one, the only filtering granularity — no per-block mode |
| Tiling | A single `iDIM`-based mechanism (32/64/128, default 64×64) — one unified API, not several parallel ones |
| Scan order | Row-major + Morton/Z-order only |
| SIMD (AVX2/NEON) | **Deferred to 0.2.** Scalar-is-reference/SIMD-is-optimization architecture planned from day one, but not implemented until the scalar format is validated |
| Adam7 interlace | **Removed from scope.** Not streamable, high complexity, low benefit vs. tiles |
| Even/Odd interlace | **Removed from scope.** Tiles already give progressive display |
| Indexed/Palette (PLTE) | **Deferred to 0.3**, as an *encoder-side transform* (`direct` vs `palette transform`), not a decoder structural mode |
| K-means / MedianCut quantization | **Deferred to 0.3**, encoder-only, decoder never needs to know how the encoder chose colors |
| ZSTD dictionary (zDIC/auto_dictionary) | **Deferred to 0.4** (external dictionary first, embedded/trained later) — conflicts with single-pass streaming |
| HDR (FP16/PQ/HLG/tonemap) | **Deferred.** Core v0.1 supports uint8/16 + float32 only |
| Metadata (EXIF/ICC/XMP/JSON) | Ancillary; decoder must decode pixels without understanding metadata |
| API | Single `EncoderOptions`/`DecoderOptions`-style struct with capability gating, not two divergent structs |

## Workspace layout

```
Cafe/
├── crates/
│   ├── cafe-format/     # chunks, IHDR, validation, spec-as-code
│   ├── cafe-codec/      # predictors, ZSTD, tiling, encoder/decoder streaming
│   ├── cafe-cli/        # bin "cafe" (inspect/benchmark/verify/explain)
│   │                    #  + legacy-named cafe-encode/cafe-decode binaries
│   └── cafe-bench/      # harness comparing CAFE vs PNG (WebP/JPEG XL/AVIF later)
├── spec/
│   ├── CAFE-spec.md          # normative format specification
│   └── invariants/           # spec rules expressed as data/tests
├── corpus/                   # photo/screenshot/illustration/pixelart/lineart/
│                              # gradient/texture/synthetic/hdr + manifest.json
├── golden/                    # golden .cafe files + golden/malformed/
│                              # (these are the project's actual golden test
│                              #  vectors, referenced from crate-local tests)
├── fuzz/fuzz_targets/         # decode_fuzz, chunk_roundtrip_fuzz
├── .github/workflows/         # ci.yml, fuzz.yml
└── Cargo.toml                 # workspace root
```

Integration tests and benchmarks live inside each crate's own `tests/`/
`benches/` directory (e.g. `crates/cafe-format/tests/`,
`crates/cafe-codec/tests/`, `crates/cafe-codec/benches/`,
`crates/cafe-bench/benches/`) rather than a root-level `tests/`/`benches/`
— there is no workspace-level integration-test crate.

No umbrella `cafe` library crate — consumers depend on `cafe-format`
and/or `cafe-codec` directly. The CLI binary is named `cafe` (inside
`cafe-cli`), plus the legacy-compatible `cafe-encode`/`cafe-decode`
binaries.

## Phased roadmap

- [x] **Phase 0 — Skeleton & CI.** Workspace with 4 crates, placeholder
      binaries, basic CI (fmt/clippy/test).
- [x] **Phase 1 — `cafe-bench` minimal.** Harness comparing compressed size
      against PNG (via the `image` crate) on a tiny synthetic corpus.
      Implemented as `cafe_bench::corpus` (deterministic gradient/
      checkerboard/noise generators, no `rand` dependency) and
      `cafe_bench::measure` (PNG size vs a naive raw-ZSTD-level-19
      placeholder — `cafe-codec` doesn't exist yet, so this is the number
      it needs to beat). `cafe-bench` binary prints a comparison table;
      criterion benches time both measurements on a 512x512 image.
- [x] **Phase 2 — Synthetic `corpus/`.** Deterministic generator (gradient,
      noise, checkerboard/pixelart, texture) + `manifest.json` (SHA-256,
      dimensions, color type, bit depth). `photo/screenshot/illustration/
      lineart/hdr` start as placeholders for real content added later.
      Implemented as `cafe_bench::manifest` (`generate_corpus`, reusing
      `cafe_bench::corpus`'s generators plus a new `Pattern::Texture`
      sinusoidal+hash-noise pattern) and the `gen-corpus` binary
      (`cargo run -p cafe-bench --bin gen-corpus`), which writes PNGs
      under `corpus/{gradient,pixelart,texture,synthetic}/` and
      `corpus/manifest.json`. `photo/screenshot/illustration/lineart/hdr`
      remain `.gitkeep`-only and `.gitignore`d except for their `.gitkeep`,
      pending real content.
- [x] **Phase 3 — `spec/`.** `CAFE-spec.md`, formalized normatively;
      invariants formalized in `spec/invariants/` and turned into
      automatic tests. Implemented as `spec/CAFE-spec.md` (11 sections:
      overview, signature, chunk structure, defined chunks —
      `IHDR`/`iDIM`/`IDAT`/`eXIF`/`jSON`/`iCCP`/`xMPd`/`IEND` —, mandatory
      chunk order, streaming, design considerations, security, licensing,
      versioning, future extensions) plus six `spec/invariants/*.toml`
      files (`signature`, `chunks`, `ihdr`, `idim`, `predictors`,
      `security`) mirroring the normative text as machine-readable data,
      validated for internal/cross-file consistency by 9 tests in
      `crates/cafe-format/tests/spec_invariants.rs` (new `toml`
      dev-dependency). Core simplicity choices baked into the spec from
      the start: `IHDR` is 12 bytes (no `filter_method` byte — per-row
      predictor selection is the only, structural mode; no
      `interlace_method` byte — interlacing is out of scope entirely);
      6 predictors (None/Sub/Up/Average/Paeth/Gradient), always chosen
      per-row (never per-block); a single `iDIM`-based tiling mechanism
      (row-major or Z-order) instead of several parallel tiling
      mechanisms; `PLTE` (indexed palette), `cHDR` (HDR metadata), and
      `zDIC` (ZSTD dictionary) chunks are absent from the v0.1 chunk set
      (deferred to 0.3/0.3/0.4 or unscheduled, per the "Core v0.1 design
      decisions" table above — Adam7/even-odd interlace and byte-shuffle
      are permanently out of scope, per the spec's section 11, not
      deferred). Security ceilings (`MAX_DECOMPRESSED_CHUNK_SIZE` = 1 GiB,
      `MAX_TILE_COUNT` = 1,048,576) are grounded in well-understood DoS
      classes (CWE-409, CWE-789). All workspace tests (17 total across
      `cafe-bench` and `cafe-format`), `cargo fmt --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 4 — `cafe-format`.** Chunk framing, `IHDR`, critical/
      ancillary validation, hand-written golden files for the minimal
      decoder to target. Implemented as five modules: `constants` (signature
      bytes, security ceilings, `IHDR`/`iDIM` enums — mirrors
      `spec/invariants/*.toml`), `chunk` (Length/Type/Flag/Data/CRC32
      framing, slice-based `read_chunk`/`write_chunk`; the `Read`-based
      streaming primitive (`read_chunk_from`) is deferred to
      `cafe-codec`'s `Decoder<R>` in Phase 5+, since Phase 4 only needs to
      parse a whole in-memory file for golden-file tests), `signature`
      (9-byte magic validation), and `ihdr` (`Ihdr` struct with
      `to_payload`/`from_payload`/`to_chunk_bytes`/`validate`/`read_ihdr`,
      enforcing spec section 4.1's `width/height > 0`, valid
      `sample_format`×`bit_depth` combinations, known `color_type`, and no
      reserved `compression_method` bits). `CafeError` includes
      `InvalidIhdr` and `UnexpectedChunkType` variants, since this crate
      validates `IHDR` content itself rather than deferring that to a
      larger, monolithic module. `read_chunk` also enforces an early
      `DecompressionLimitExceeded` check against a chunk's raw (still
      compressed, if `Flag=0x01`) `Length` field, before any decompression
      is attempted — bounding `Length` itself upfront is simpler to reason
      about than checking only the decompressed output size, and costs
      nothing. Golden fixtures (`golden/minimal_1x1_gray.cafe`,
      `golden/minimal_2x2_rgba.cafe`, `golden/malformed/{bad_signature,
      truncated_header,crc_mismatch,invalid_ihdr_zero_width,forged_length,
      wrong_first_chunk}.cafe`) are hand-built by an `#[ignore]`d
      fixture-generator test (`crates/cafe-format/tests/golden_files.rs`)
      rather than a real encoder (which doesn't exist until Phase 6) — 8
      golden tests (2 valid + 6 malformed) confirm this crate's parser
      accepts the valid fixtures and rejects each malformed one with the
      correct `CafeError` variant. 37 new unit tests added across the four
      modules (9 chunk, 4 signature, 15 ihdr framing/validation, plus the
      golden-file tests), for 45 total `cafe-format` tests (28 lib + 8
      golden + 9 spec-invariants, the latter unchanged from Phase 3) plus
      the pre-existing 8 `cafe-bench` tests — 53 across the workspace.
      This crate still has zero predictor/pixel logic by design (per
      `AGENTS.md`'s "decoder before encoder" and "boringly simple"
      principles) — `IDAT` payloads are opaque bytes from its point of
      view; reversing predictors and reconstructing pixels is
      `cafe-codec`'s job (Phase 5). All workspace tests, `cargo fmt --check`,
      and `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 5 — `cafe-codec` decoder (scalar).** header -> chunks ->
      ZSTD decompress -> unpredict -> pixels. Validated against Phase 4's
      golden files. Implemented as five modules: `error` (`CodecError`
      enum — `Format(cafe_format::CafeError)`, `InvalidPredictorCode(u8)`,
      `UnsupportedTiling(String)` — with `From<cafe_format::CafeError>`/
      `From<std::io::Error>`), `predictor` (the 6 spec predictors behind
      `filter_row`/`unfilter_row`; absent neighbors at row/column 0 are
      treated as zero per spec section 4.3), `zstd_codec`
      (`compress_with_fallback`/`decompress_with_limit`/
      `decompress_chunk`; decompression is bounded by the caller-given
      limit capped at `MAX_DECOMPRESSED_CHUNK_SIZE`, never trusting a
      ZSTD frame header's declared size), `tile` (`encode_tile_rows`/
      `decode_tile_rows`, applying/reversing one predictor code across
      all rows of a tile — per-row heuristic selection is Phase 6's
      encoder concern, not decoder concern), and `decoder`
      (`decode_bytes(&[u8]) -> Result<DecodedImage>`: validates
      signature, reads `IHDR`, accepts either no `iDIM` or an explicit
      single-tile `iDIM` covering the whole image — multi-tile is
      deferred to Phase 7 as `CodecError::UnsupportedTiling` — decompresses
      exactly one mandatory `IDAT` with the decompression limit set to the
      exact expected payload size rather than the generic 1 GiB ceiling,
      reverses predictors, and requires `IEND`; unknown critical chunks
      are rejected, unknown ancillary chunks are skipped, multiple
      `IDAT`s are rejected). 49 new tests across the four new modules
      (20 predictor, 10 zstd_codec, 10 tile, plus `decoder`'s suite:
      the 2 valid + 6 malformed golden fixtures from Phase 4 re-validated
      through the decoder, plus round-trip and adversarial tests built via
      an in-file `build_file()` helper — explicit 1x1 `iDIM` accepted,
      multi-tile and undersized-tile `iDIM` rejected, missing/duplicate
      `IDAT` rejected, missing `IEND` rejected, unknown ancillary chunk
      skipped, unknown critical chunk rejected, ZSTD-compressed `IDAT`
      round-trip, RGBA-with-Paeth round-trip). This decoder is
      whole-buffer/slice-based, not `Read`-based streaming — same
      deliberate deferral `cafe-format` made in Phase 4, revisited when
      `Decoder<R>` streaming is designed. All workspace tests (102 total:
      49 `cafe-codec` + 28 `cafe-format` lib + 8 golden + 9 spec-invariants
      + 8 `cafe-bench`), `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 6 — `cafe-codec` encoder.** Streaming `Encoder<W>` (plus
      `encode_bytes` sugar) closing the round-trip with Phase 5's decoder.
      Implemented as one new module, `encoder`, and two small additions to
      existing Phase 5 modules: `predictor::shannon_entropy` (private,
      zero-order byte-histogram entropy) plus public `predictor::
      choose_best_row_predictor(row, prev_row, bpp) -> (u8, Vec<u8>)`
      (tries all `NUM_PREDICTORS` candidates via `filter_row`, keeps the
      lowest-entropy result, ties favor the smaller code — so `PREDICTOR_NONE`
      wins whenever every candidate scores equally, e.g. a tile's first
      byte), and `tile::encode_tile_rows_auto` (the `encode_tile_rows`
      counterpart that calls `choose_best_row_predictor` independently per
      row instead of applying one fixed code to the whole tile). Only
      `Entropy`-style scoring is implemented — real compression-test/MSAD
      heuristics are deferred until `cafe-bench` shows they justify the
      extra encode cost, per `AGENTS.md`'s "every feature proves itself
      with a benchmark first". `encoder::Encoder<W>` mirrors the decoder's
      scope exactly (single whole-image tile, no `iDIM` emitted —
      multi-tile is Phase 7): `Encoder::new` validates `IHDR` fields via
      `Ihdr::validate` and records them without writing anything yet;
      `add_tile` accepts exactly one raw-pixel buffer of the expected
      `height * bytes_per_row` size; `finish` runs
      `encode_tile_rows_auto`, then `zstd_codec::compress_with_fallback`
      (or forces `FLAG_RAW` if `EncoderOptions::allow_zstd` is `false`),
      then writes `IHDR`/`IDAT`/`IEND` via `cafe_format::chunk::write_chunk`
      in spec section 5's mandatory order, and returns the writer.
      Deferring all writes to `finish` (rather than writing `IHDR`
      eagerly in `new`) is a deliberate Phase-6-only simplification: with
      only one tile ever accepted, every chunk can be emitted exactly once
      without a placeholder-then-patch step; true incremental writing
      (`IHDR` before pixel data is fully known) becomes relevant once
      Phase 7 allows more than one `IDAT`. A new `CodecError::
      EncoderMisuse(String)` variant covers caller misuse specifically
      (`add_tile` called twice, wrong buffer length, `finish` called
      before any `add_tile`) — unlike every other `CodecError` variant,
      this always indicates a programming error in the caller, never
      untrusted file input, so it's kept distinct from `CodecError::
      Format`. Confirmed against the Phase 4 golden fixtures: `encode_bytes`
      reproduces `golden/minimal_1x1_gray.cafe` **byte-for-byte** (single
      sample, no left neighbor to predict from — `choose_best_row_predictor`
      independently arrives at the same `PREDICTOR_NONE` the golden was
      hand-built with); for `golden/minimal_2x2_rgba.cafe`, the encoder's
      output differs at the byte level — the per-row heuristic finds `Sub`
      beats that golden's hand-picked `None` for its constant-step row 0
      — so that case is instead asserted on decoded-pixel equality,
      documented in the test itself as a genuine improvement rather than
      a discrepancy to paper over. 24 new tests across `predictor` (10:
      `shannon_entropy` on empty/constant/uniform/varied input,
      `choose_best_row_predictor` picking None/Sub/Up correctly depending
      on row shape, always producing decoder-reversible output, and never
      scoring worse than `PREDICTOR_NONE`), `tile` (3:
      `encode_tile_rows_auto` round-trip, wrong-length rejection, and
      confirming per-row selection is independent row-to-row), and
      `encoder` (19: encode-then-decode round-trips across gray/RGBA,
      8/16-bit, 1x1/2x2/16x16/64x64, uniform/varied content; raw-vs-ZSTD
      `Flag` selection in both directions including the
      `allow_zstd = false` override; the two golden-file comparisons
      above; and `EncoderMisuse`/`InvalidIhdr` rejection paths), for 77
      total `cafe-codec` tests (up from 49) and 122 across the workspace's
      format/codec/bench crates combined. `cafe-bench`'s `measure()` also
      stopped being a "PNG vs ZSTD-raw placeholder" and now calls the real
      `cafe_codec::encode_bytes` (new `Measurement::cafe_bytes`/
      `cafe_ratio`/`cafe_vs_png`, alongside the pre-existing PNG and
      raw-ZSTD-floor numbers, which are kept as a lower bound
      predictors+tiling must beat rather than removed) — this surfaced
      that CAFE already beats PNG by a wide margin on every synthetic
      pattern tried (e.g. ~0.6% of raw size vs PNG's ~2.9% on a 64x64
      gradient; ~30% vs PNG's ~101% on the `Noise` pattern, whose
      LCG-based low-order bytes turn out to be far more predictor-exploitable
      than true entropy — the affected `cafe-bench` test and doc comments
      were updated to explain this rather than silently loosening an
      assertion). All workspace tests (130 total: 77 `cafe-codec` + 28
      `cafe-format` lib + 8 golden + 9 spec-invariants + 8 `cafe-bench`),
      `cargo fmt --all --check`, and `cargo clippy --all-targets --
      -D warnings` pass cleanly.
- [x] **Phase 7 — Tiling + Morton.** `iDIM`, 64×64 default, row-major +
      Z-order. Implemented as three new/extended modules plus a
      generalization of the Phase 5/6 decoder and encoder: `cafe-codec::
      morton` (new — `morton_code`/`morton_decode`, naive bit-interleaving,
      4 tests including exhaustive 32x32 round-trip and a Z-order
      block-locality property); `cafe_format::idim` (new — `Idim` struct
      with `for_image` (ceiling-division derivation), `validate` (nonzero
      fields, known `scan_order`, `MAX_TILE_COUNT` ceiling checked
      *before* the tiles_x/tiles_y-vs-IHDR consistency check, CWE-789/
      CWE-409-class), `tile_dimensions` (saturating arithmetic for partial
      edge tiles), `to_payload`/`from_payload`/`to_chunk_bytes`; 16 tests
      including the `tiles_x=tiles_y=65535`-via-`tile_width=tile_height=1`
      exploit case from the spec's security note); `cafe-codec::tiling`
      (new — `tile_order` enumerates `(tile_x, tile_y)` positions in
      row-major or Morton-sorted order, `tile_origin` maps a grid position
      to pixel-space, and `TileLayout` unifies the "no `iDIM`" and
      "explicit `iDIM`" cases behind one `tile_rect(index) -> (origin_x,
      origin_y, tile_w, tile_h)` API shared by the decoder and encoder; 9
      + tests). `cafe-codec::decoder::decode_bytes` now assembles any
      number of tiles (previously restricted to the single-tile case via
      `validate_single_tile_idim`): it reads `iDIM` (if present, requiring
      it before the first `IDAT` per spec section 5's mandatory order and
      rejecting a duplicate instance), builds a `TileLayout`, and for each
      `IDAT` decodes exactly that tile's rows (bounded by that tile's own
      expected payload size, not a whole-image budget) into the correct
      rectangular region of one pre-allocated, exactly-sized pixel buffer
      — new `CodecError::TilingMismatch` (replacing the narrower
      `UnsupportedTiling`) covers `IDAT` count/order not matching what
      `iDIM` (or its absence) requires, distinct from `InvalidIdim`'s
      "iDIM's own fields are self-inconsistent" case. `cafe-codec::
      encoder::Encoder<W>` gained `EncoderOptions::tile_size: Option<(u16,
      u16)>` and `scan_order: u8`: when `tile_size` is `Some`, `new` now
      derives and validates an `Idim` and writes it right after `IHDR`;
      `add_tile` accepts one tile's pixels at a time (in scan order,
      using that tile's real — possibly edge-truncated — dimensions) and
      writes its `IDAT` immediately, true streaming rather than Phase 6's
      buffer-until-`finish` design (now unnecessary: `IHDR`/`iDIM` need
      only `width`/`height`/tiling options, not pixel data, so eager
      writing in `new` is strictly simpler once more than one `IDAT` is
      possible); `encode_bytes` splits a whole-image buffer into
      per-tile calls via a new private `extract_tile_pixels` helper. 24
      new tests across `encoder` (multi-tile row-major/Z-order/partial-
      edge/RGBA round-trips, `iDIM` chunk presence/absence, misuse paths
      for too-few/too-many `add_tile` calls, zero tile-size-component
      rejection, and the `MAX_TILE_COUNT` exploit rejected at `Encoder::
      new` time) and `decoder` (multi-tile row-major/Z-order/partial-edge
      round-trips, duplicate `iDIM`, `iDIM` after the first `IDAT`, fewer/
      more `IDAT`s than `iDIM` declares). All workspace tests (172 total:
      105 `cafe-codec` + 42 `cafe-format` lib + 8 golden + 9
      spec-invariants + 8 `cafe-bench`), `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 8 — Fuzzing & robustness.** `decode_fuzz`/
      `chunk_roundtrip_fuzz` harnesses, adversarial/truncated-input tests,
      nightly fuzz CI. Implemented as a standalone `fuzz/` directory
      containing its own Cargo workspace (`fuzz/Cargo.toml`, crate
      `cafe-fuzz`, deliberately **not** a member of the root workspace —
      `cargo fuzz` requires nightly + libFuzzer's `#[no_main]` entry
      point, which doesn't mix with the root workspace's stable-toolchain
      `build`/`test`/`clippy` commands) with two bins: `decode_fuzz.rs`
      (calls `cafe_codec::decode_bytes`, ignoring the result — the only
      forbidden outcome is a panic) and `chunk_roundtrip_fuzz.rs` (calls
      `cafe_format::chunk::read_chunk` directly, plus
      `cafe_codec::decode_bytes` for the same whole-file coverage
      `decode_fuzz` provides). Real libFuzzer execution requires
      Linux/nightly (confirmed via `cargo +nightly fuzz build --sanitizer
      none`, which compiles cleanly on Windows but fails to *link* with
      `LNK2001: unresolved external symbol __stop___sancov_cntrs`/
      `__start___sancov_pcs` — libFuzzer's coverage instrumentation is a
      known Unix-only limitation on MSVC, not a bug in this crate); local
      Windows development instead relies on two new proptest-based
      integration-test files that exercise the same "never panic on
      adversarial input" contract without needing libFuzzer at all:
      `crates/cafe-codec/tests/decode_robustness.rs` (12 hand-written
      adversarial cases — empty buffer, truncated/invalid signature,
      garbage after a valid signature, forged/huge chunk lengths,
      zero-width `IHDR`, and two exhaustive sweeps against the Phase 4
      golden fixture `golden/minimal_2x2_rgba.cafe`: every truncation
      length and every single-bit flip, confirming `decode_bytes` never
      panics across either) and `crates/cafe-codec/tests/
      roundtrip_proptest.rs` (3 proptest properties, new `proptest`
      dev-dependency added to `cafe-format` in addition to its
      pre-existing `cafe-codec` dev-dependency: arbitrary byte sequences
      up to 4 KiB never panic `decode_bytes`, the same with a genuine
      CAFE signature prefix, and a full `encode_bytes`/`decode_bytes`
      round-trip across random small width/height/color-type/seed/tiling
      combinations always reproduces the exact input pixels). A third new
      file, `crates/cafe-format/tests/chunk_proptest.rs` (3 properties),
      applies the same treatment one layer down, directly at `read_chunk`
      rather than the whole-file decoder: arbitrary bytes at arbitrary
      offsets, single-bit-flipped well-formed chunks, and truncated
      well-formed chunks at every length, all confirmed panic-free. CI
      gained two pieces: a `fuzz` job in `.github/workflows/ci.yml`
      running each of the two harnesses for a 60-second smoke test on
      every push/PR, and a new `.github/workflows/fuzz.yml` running each
      harness for a full hour nightly at 2 AM UTC (configurable via
      `workflow_dispatch`'s `duration_seconds` input), both uploading
      crash artifacts (and, for the nightly job, the accumulated corpus)
      on failure. Both workflow files were validated for YAML/schema
      correctness with `js-yaml` (Docker wasn't available to run
      `actionlint` directly in this environment) confirming both jobs and
      their step lists parse as intended. 18 new tests across the three
      new files (12 `decode_robustness` + 3 `roundtrip_proptest` in
      `cafe-codec`, 3 `chunk_proptest` in `cafe-format`), for 190 total
      workspace tests (up from 172): 105 + 12 + 3 `cafe-codec` (lib + the
      two new integration files) + 42 `cafe-format` lib + 3
      `chunk_proptest` + 8 golden + 9 spec-invariants + 8 `cafe-bench`.
      All workspace tests, `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly; the
      standalone `fuzz/` crate was confirmed to still compile cleanly via
      `cargo +nightly check --manifest-path fuzz/Cargo.toml` (its own
      workspace isn't covered by the root's `fmt`/`clippy` commands, so
      this is checked separately, as documented in `fuzz/Cargo.toml`'s
      own comments).
- [x] **Phase 9 — `cafe-cli`.** `cafe inspect/verify/explain/benchmark` +
      `cafe-encode`/`cafe-decode`. Scope decided up front (v0.1): PNG-only,
      8-bit-only I/O (16-bit/float32 deferred to a later CLI pass even
      though `cafe-codec` itself already supports them); hand-rolled
      `std::env::args()` parsing, no `clap`; `cafe explain` gets its own
      chunk-walking/predictor-byte-reading code in `cafe-cli` rather than
      new public API on `cafe-codec`; `cafe benchmark` is a thin wrapper
      over `cafe-bench` (added as a new `cafe-cli` dependency, alongside a
      new `cafe-bench = { path = ... }` workspace-dependencies entry).
      Implemented as a `cafe-cli` library (`cafe_cli`, new — the crate
      previously had only bin targets) plus two modules: `png_io`
      (`CafePixels` struct + `dynamic_image_to_cafe_pixels`/
      `cafe_pixels_to_dynamic_image`, bridging `image::DynamicImage` and
      CAFE's raw-pixel-plus-header shape by picking the narrowest of the
      four 0.1 `color_type`s from `image::ColorType::has_alpha()`/
      `has_color()`; a source PNG with a different bit depth/model is
      silently downconverted to 8-bit uint by `image`'s own
      `to_rgba8()`-family conversions, documented as a known v0.1
      narrowing rather than an error) and `chunks` (`walk_chunks` — a thin
      wrapper looping `cafe_format::chunk::read_chunk` until `IEND`, used
      by all three inspection subcommands — plus `read_predictor_codes`,
      which reads each row's leading predictor-code byte out of an
      already-decompressed `IDAT` payload without reversing prediction,
      since `explain` only needs a histogram, not reconstructed pixels;
      unlike `cafe_codec::tile::decode_tile_rows` this never rejects an
      out-of-range code, since describing a malformed file is `explain`'s
      job, not refusing it). `cafe-encode`/`cafe-decode` are now real
      binaries: `cafe-encode` reads a PNG via `image::ImageReader`,
      converts via `png_io`, and calls `cafe_codec::encode_bytes` (hand-
      rolled `--tile-size WxH`/`--scan-order row|z`/`--level N`/`--no-zstd`
      flags map onto `EncoderOptions`); `cafe-decode` reverses that via
      `cafe_codec::decode_bytes` + `png_io` + `DynamicImage::save`. The
      `cafe` binary's four subcommands are real: `inspect` prints every
      chunk's type/offset/flag/length/criticality plus parsed `IHDR`/`iDIM`
      fields; `verify` re-checks signature, first-chunk-is-IHDR,
      `Ihdr::validate`/`Idim::validate`, IDAT/IEND presence, and a full
      `cafe_codec::decode_bytes` pass, printing every problem found (exit
      code reflects overall validity) rather than stopping at the first
      one; `explain` decompresses every `IDAT` (via
      `cafe_codec::zstd_codec::decompress_chunk`) and prints a predictor-
      code histogram plus compressed-vs-raw byte totals per file — first
      implemented using row-major arithmetic to locate each tile's
      dimensions directly from `iDIM`, a bug caught and fixed during manual
      end-to-end testing (a Z-order-tiled file decoded fine but `explain`
      read the wrong tile dimensions for tiles past the first, since it
      never consulted `iDIM.scan_order`); now built on
      `cafe_codec::tiling::TileLayout::new(...).tile_rect(i)`, the same
      shared abstraction the decoder/encoder already use, so it can never
      drift from their notion of tile order again; `benchmark` reads
      `<corpus-dir>/manifest.json` (defaulting to
      `cafe_bench::manifest::default_corpus_dir()`), calls
      `cafe_bench::measure()` per entry, and prints a table plus a raw/png/
      cafe total row. All four subcommands and both encode/decode binaries
      were exercised manually end-to-end (not just unit-tested) against
      real corpus PNGs, including a multi-tile Z-order round-trip with
      partial edge tiles, confirming `verify` accepts what `cafe-encode`
      produces and `explain`'s tile accounting matches after the fix
      above. 9 new tests (4 `png_io`: RGBA/gray round-trip, 16-bit-source
      downconversion, unsupported-bit-depth rejection; 5 `chunks`: chunk
      listing/ordering, stopping at `IEND`, CRC-error propagation,
      predictor-code extraction including a truncated-payload case), for
      199 total workspace tests (up from 190): 105 `cafe-codec` (lib) + 12
      + 3 (`cafe-codec` integration files) + 42 `cafe-format` lib + 3
      `chunk_proptest` + 9 `cafe-cli` (new) + 8 golden + 9 spec-invariants
      + 8 `cafe-bench`. All workspace tests, `cargo fmt --all --check`,
      and `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 10 — Benchmark vs PNG.** Run `cafe-bench` on the full corpus;
      only add SIMD/palette/dictionary (0.2+) if the numbers justify it. "The
      full corpus" is `corpus/manifest.json`'s 8 synthetic entries
      (gradient/pixelart/texture/synthetic x 64x64/256x256) — Phase 2's
      photo/screenshot/illustration/lineart/hdr placeholders still have no
      real content to benchmark against (unchanged from Phase 2/9; adding
      real assets is unscheduled, not part of this phase), so this run
      exercises `cafe benchmark`'s corpus-walking path end-to-end (Phase 9's
      `<corpus-dir>/manifest.json` + `cafe_bench::measure()` per entry) on
      everything that currently exists on disk, rather than the in-memory
      matrix `cafe-bench`'s own binary (`main.rs`) generates ad hoc. Results
      (`cargo run -p cafe-cli --bin cafe -- benchmark`):

      ```
      image                                           raw        png       cafe      png %     cafe %
      gradient/gradient_64x64.png                   16384        474        102       2.9%       0.6%
      gradient/gradient_256x256.png                262144       2382        119       0.9%       0.0%
      pixelart/checkerboard4_64x64.png              16384       1021        119       6.2%       0.7%
      pixelart/checkerboard4_256x256.png           262144      13285        142       5.1%       0.1%
      texture/texture_64x64.png                     16384       9605       6869      58.6%      41.9%
      texture/texture_256x256.png                  262144     126931      92596      48.4%      35.3%
      synthetic/noisec0ffee_64x64.png               16384      16516       5024     100.8%      30.7%
      synthetic/noisec0ffee_256x256.png            262144     212515      35769      81.1%      13.6%

      TOTAL: raw=1114112 png=382729 (34.4%) cafe=140740 (12.6%)
      ```

      CAFE beats PNG on every single entry, by margins ranging from ~2x
      (`texture`, mid-frequency sinusoidal+hash-noise content, the hardest
      category in this corpus) to >100x (`gradient`/`pixelart`, smooth or
      flat content where per-row predictors plus ZSTD leave almost nothing
      for PNG's filter+DEFLATE to beat). `cafe explain` was run against each
      of the 8 encoded files to break down *why*, via the per-row predictor
      histogram: `gradient` is Paeth on 98-99% of rows (as expected for a
      smooth 2D ramp); `pixelart`/checkerboard is Up-dominated (75%) with a
      Sub/Paeth split on the remaining rows (each row differs from the one
      above by a clean vertical block shift, exactly what Up predicts);
      `texture` splits mostly between Average (76% at 256x256) and Gradient
      (14-99% depending on size) with no None/Paeth at the larger size — the
      sinusoidal-plus-hash-noise formula doesn't collapse onto one predictor
      the way the other three patterns do, which is also reflected in it
      having by far the worst compression ratio of the four; `synthetic`
      (LCG-based "noise") is 98-99% Gradient, the same LCG-low-byte-
      correlation effect already documented in `measure.rs`'s
      `measure_noise_compresses_poorly_for_png_and_raw_zstd` test, just
      surfaced here at the per-row level instead of the aggregate-ratio
      level.

      **SIMD/palette/dictionary (0.2+) go/no-go, per `AGENTS.md`'s "every
      feature proves itself with a benchmark first" principle: no-go for
      now, on all three.** None of the three defers have justified
      themselves against this corpus:
      - **SIMD (0.2):** every measured file above encodes in well under a
        second on scalar code (`cargo run --release` timing not separately
        instrumented here, but `cafe-bench`'s own criterion benches already
        clock the full `measure()` — PNG + CAFE + raw-ZSTD together — at
        sub-millisecond-to-low-millisecond scale on a 512x512 image; see
        `crates/cafe-bench/benches/encode_decode.rs`). Nothing in this
        corpus shows scalar predictor/ZSTD throughput as a bottleneck worth
        trading away "scalar-is-reference, SIMD-is-optimization" simplicity
        for.
      - **Palette (0.3):** every corpus category here already reaches
        0.0-41.9% of raw size with CAFE's direct (non-indexed) transform;
        `pixelart`/`checkerboard4`, the category most likely to have a
        small distinct-color count and thus benefit from an indexed
        transform, is already at 0.1-0.7% — there's no headroom visible for
        palette to meaningfully improve on here. A palette-favorable case
        (large flat-color illustration/pixelart with many repeated exact
        colors and a small palette) is exactly the category still missing
        real content (`corpus/illustration/`, `corpus/pixelart/` currently
        only has synthetic checkerboards) — re-evaluate once that content
        exists, rather than adding palette speculatively.
      - **Dictionary (0.4):** this corpus's entries are large, single
        varied images, not the small/similar/many-small-files scenario
        (e.g. sprite sheets, icon sets, thumbnail batches) where a shared
        ZSTD dictionary earns back its embedding/training cost. No entry
        here is dictionary-shaped.

      This isn't a permanent verdict — it's specifically "not justified by
      *this* corpus", and the biggest open gap remains the missing
      photo/screenshot/illustration/lineart/hdr content (real photos in
      particular are the category most likely to stress predictors
      differently than any synthetic pattern here, and are what would most
      plausibly move the SIMD needle). No code changes in this phase beyond
      the two `cafe-bench` doc-comment corrections above (`main.rs`'s
      module doc and startup banner still described the corpus-walking
      `cafe benchmark` path as unimplemented future work after Phase 9 had
      already shipped it) — Phase 10 is a benchmark-and-decide phase, not
      an implementation phase, per its own description. All workspace
      tests, `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` continue to pass cleanly
      (199 tests, unchanged from Phase 9, since no test-relevant code
      changed).
- [x] **Corpus population (post-Phase-10 follow-up).** Phase 10's own text
      flagged "the biggest open gap remains the missing photo/screenshot/
      illustration/lineart/hdr content" as the thing most likely to move
      its SIMD/palette/dictionary no-go verdicts. This follow-up populates
      those categories with real (not synthetic) content and re-runs the
      benchmark, without revisiting the no-go verdicts themselves (see
      below). Implemented as a new `cafe_bench::import` module
      (`resize_to_fit` — Lanczos3 downscale capped at 256x256, matching the
      synthetic categories' largest size, never upscales a smaller source;
      `import_sources` — decodes each staged source image, resizes,
      re-encodes as RGBA8 PNG under `corpus/<category>/<slug>.png`, and
      returns one `ImageEntry` per image; `merge_and_write` — loads any
      existing `manifest.json`, replaces entries sharing a `path` or
      appends new ones, and writes the result back, so re-running import
      updates in place rather than duplicating) plus a new
      `import-corpus` binary (`crates/cafe-bench/src/bin/import-corpus.rs`)
      with a hard-coded `EXPECTED_SOURCES` table mapping a staging
      directory's expected file layout to `(category, slug)` pairs — a
      curated, license-reviewed list rather than an open-ended CLI, since
      every source image's provenance needed to be checked before
      inclusion (see `corpus/ATTRIBUTION.md`). Sources: the
      [Kodak Lossless True Color Image Suite](https://r0k.us/graphics/kodak/)
      (4 photos, public domain/unrestricted research use) for `photo/`;
      Wikimedia Commons' `Category:PD-ScottForesman` (Pearson Scott
      Foresman line art, donated to the public domain) for `lineart/` (4
      images) and `illustration/` (2 images, chosen for being more
      colour/tone-heavy than the line-art-proper picks); 4 Windows
      screenshots (Calculator, Character Map, Notepad with placeholder
      Lorem-ipsum text, Paint's blank canvas) captured locally for
      `screenshot/`, deliberately restricted to built-in utilities with no
      personal data, file paths, or network-share names visible (two
      earlier capture attempts — an Explorer window showing real corporate
      network drive names, and a Settings "About" page showing the
      machine's real owner name and device ID — were caught during manual
      review and discarded before import, never written to `corpus/`).
      `corpus/hdr/` also gained two OpenEXR sample fixtures
      (`Blobbies.exr`/`Cannon.exr` from the Academy Software Foundation's
      `openexr-images` repo) but — unlike the other four categories — these
      are placed directly on disk without going through `import_sources`
      or appearing in `manifest.json`, since `cafe-cli`'s `png_io` bridge
      is 8-bit-PNG-only; wiring HDR into the benchmark path remains a
      distinct, unscheduled follow-up. `corpus/manifest.json` now has 22
      entries (the original 8 synthetic plus 14 real); `.gitignore`'s
      per-category `/corpus/<cat>/*` exclusions were removed now that every
      category has real, license-checked content worth tracking (`hdr/`
      too, despite not being in the manifest, since its two files are
      small and license-checked the same way). A new `corpus/
      ATTRIBUTION.md` documents source/author/license for every real image
      across all five categories. Re-running `cafe benchmark` against the
      now-22-entry manifest confirms CAFE still beats PNG on every single
      entry, real or synthetic — real photos land around 45-62% of raw
      size (vs. PNG's 48-73%), public-domain line art/illustrations around
      8-36% (vs. PNG's 17-51%), and screenshots around 5-24% (vs. PNG's
      6-32%) — a smaller margin than the near-0% synthetic gradients but
      still a clear win on every entry:

      ```
      TOTAL: raw=3760128 png=1324113 (35.2%) cafe=795462 (21.2%)
      ```

      This **does not overturn** Phase 10's SIMD/palette/dictionary no-go
      verdicts: nothing in the real-content numbers shows scalar
      predictor/ZSTD throughput as a bottleneck (SIMD), none of the
      real categories have a palette-favorable small-distinct-color-count
      profile the way pure pixel-art would (palette — `pixelart/` itself
      is still purely synthetic checkerboards, unchanged), and this
      corpus's entries remain large/varied single images, not the small/
      similar/many-files scenario dictionary compression targets. 7 new
      tests in `cafe_bench::import` (`resize_to_fit`'s downscale/no-op/
      never-upscale behavior, `import_sources`' PNG-writing and
      entry-generation round-trip, `merge_and_write`'s create/replace/
      preserve-unrelated-entries behavior against a temp manifest), for
      206 total workspace tests (up from 199). All workspace tests,
      `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **HDR benchmark wiring (post-corpus-population follow-up).** The
      corpus-population follow-up above left "wiring HDR into the
      benchmark path" as a distinct, unscheduled item, since `cafe-cli`'s
      `png_io` bridge is 8-bit-PNG-only and PNG can't represent float32
      HDR samples at all. This follow-up wires `corpus/hdr/`'s two `.exr`
      fixtures into `cafe-bench`/`cafe benchmark` without touching
      `png_io` or the CLI's encode/decode binaries — the `image` crate's
      `to_rgba32f()` decodes `.exr` directly into `cafe_codec::
      encode_bytes`'s expected float32 buffer shape, so no new PNG
      bridge code was needed, only a new comparison path.
      A throwaway `crates/cafe-bench/examples/hdr_poc.rs` (since deleted)
      proved the hypothesis first, per `AGENTS.md`'s "every feature
      proves itself with a benchmark first" principle, before any
      permanent code was written — see "PoC result and comparison
      baseline decision" below.

      **PoC result and comparison baseline decision.** Unlike every prior
      benchmark in this project, PNG isn't a meaningful baseline for HDR
      at all (tonemapping float32 to 8-bit is lossy, so it isn't
      comparing equivalent representations). The alternative considered
      and chosen instead: compare against the original `.exr` file's size
      on disk — a different container format, but the most meaningful
      real-world number, since that's the file CAFE would actually be
      replacing. The PoC's result was mixed, not a clean win: encoding
      `Blobbies.exr`'s decoded float32 RGBA through
      `cafe_codec::encode_bytes` (default `EncoderOptions`, no tiling)
      produced a `.cafe` file at 61.5% of the original `.exr`'s size, but
      `Cannon.exr` produced one at 131.9% — *larger* than the source
      file. This is the first case in this project where CAFE's v0.1
      predictor set does not consistently beat the comparison baseline,
      and is reported here rather than hidden, per this project's
      practice (e.g. the `texture`/`synthetic` categories' weaker ratios
      in the "Benchmark vs PNG" phase above): OpenEXR's own compression
      (wavelet+Huffman coding tuned for HDR float data) evidently
      outperforms generic per-row-predictor-plus-ZSTD on at least one of
      the two fixtures on hand — the six spec predictors were designed
      around uint8/16 neighbor-delta locality, and nothing in `AGENTS.md`
      ever claimed that transfers cleanly to float32 bit patterns.

      Implemented as: a new `format` field on `cafe_bench::manifest::
      ImageEntry` (`"png"`/`"exr"`, defaulting to `"png"` via
      `#[serde(default)]` so every pre-existing manifest entry keeps
      parsing without a migration) distinguishing HDR entries from every
      other category; `cafe_bench::import::{HdrSource,
      register_hdr_sources}` (new, alongside the pre-existing
      `SourceImage`/`import_sources` pair), which records `.exr` files
      already sitting under `corpus/hdr/` into the manifest by reference
      (reading just their header for dimensions via `image::ImageReader::
      into_dimensions`, no resize/re-encode step — deliberately not
      reusing `import_sources`, since there's no PNG output to write for
      this category at all, unlike photo/screenshot/illustration/
      lineart); `cafe_bench::measure::{HdrMeasurement, measure_hdr}` (new,
      alongside the pre-existing `Measurement`/`measure` pair, kept
      separate rather than shoehorned into one struct since the two have
      genuinely different baselines — `cafe_vs_png` doesn't apply to HDR,
      `cafe_vs_exr` doesn't apply to the synthetic/real PNG corpus); and
      `import-corpus`'s `EXPECTED_HDR_SOURCES` table plus a `-` sentinel
      for its previously-mandatory `staging-dir` argument (HDR sources,
      unlike every other category, are never staged anywhere — they're
      registered directly from their final `corpus/hdr/` location, so
      running `import-corpus -` re-registers HDR entries without
      requiring an unrelated staging directory to exist). `cafe
      benchmark` (`cmd_benchmark` in `cafe-cli`'s `cafe.rs`) now splits
      manifest entries by `format`: PNG entries print in the pre-existing
      table exactly as before (confirmed byte-identical `TOTAL` line to
      the corpus-population phase above — `raw=3760128 png=1324113
      (35.2%) cafe=795462 (21.2%)` — since no PNG-path code changed), and
      HDR entries print afterward in a second table explicitly labeled
      as comparing against the original `.exr`, not PNG:

      ```
      HDR (compared against the original .exr file, not PNG — see AGENTS.md):
      image                                           raw        exr       cafe cafe vs exr %
      hdr/Blobbies.exr                           16000000    6109568    3754970        61.5%
      hdr/Cannon.exr                              7063680    1163637    1535160       131.9%
      ```

      `corpus/manifest.json` now has 24 entries (22 from the
      corpus-population phase plus the 2 HDR entries) — every PNG entry's
      bytes are unchanged, confirmed by re-running `import-corpus -`
      being a pure addition (`merge_and_write`'s existing replace-by-path
      behavior left all 22 prior entries untouched). This result is a
      narrower, evidence-based version of a SIMD/palette/dictionary-style
      no-go: it does **not** justify adding an HDR-specific predictor or
      transform speculatively — a two-fixture sample is too small to
      generalize from, and per `AGENTS.md`'s guiding principles, that
      would need its own hypothesis/benchmark/golden-vector treatment,
      not a reactive patch. The honest, current state is recorded as-is:
      CAFE's existing v0.1 format handles HDR float32 correctly and
      sometimes beats OpenEXR's own compression, but not reliably enough
      yet to claim a general win for this content type. 1 new test
      (`measure::tests::measure_hdr_produces_sane_sizes_for_corpus_files`,
      skipping its real assertions gracefully if the corpus checkout
      lacks the `.exr` fixtures rather than failing environments without
      them), for 207 total workspace tests (up from 206). All workspace
      tests, `cargo fmt --all --check`, `cargo clippy --all-targets --
      -D warnings`, and `cargo +nightly check --manifest-path
      fuzz/Cargo.toml` pass cleanly.
- [x] **HDR regression investigation (post-HDR-benchmark-wiring follow-up).**
      The HDR benchmark wiring phase above reported `Cannon.exr` losing to
      its own source file (131.9%) without explaining why, while
      `Blobbies.exr` won (61.5%). This follow-up root-causes that gap
      using a throwaway `crates/cafe-bench/examples/hdr_investigate.rs`
      (since deleted, along with the `exr`/`half` dev-dependencies it
      needed — pure research code, per `AGENTS.md`'s "every feature
      proves itself with a benchmark first" principle, never intended to
      become a permanent tool). Two independent, additive causes were
      found, both stemming from the same root: `image::to_rgba32f()`
      forces every `.exr` into a 4-channel float32 buffer regardless of
      what the source file actually contains, which is not what either
      fixture actually stores.

      Inspecting both files' real channel layout directly via the `exr`
      crate (`exr::meta::MetaData::read_from_file`) showed neither is
      genuinely float32 RGBA data: `Blobbies.exr` stores HALF (16-bit
      float) R/G/B/A channels (plus an unrelated F32 depth channel, `Z`,
      which the RGBA-only decode path never touches) under `ZIP16`
      (lossless) compression; `Cannon.exr` stores HALF R/G/B **with no
      alpha channel at all** under `B44` compression. `to_rgba32f()`
      quietly (a) widens every 16-bit sample to 32 bits — doubling
      `raw_bytes` with zero-information padding the source file never
      had — and (b) synthesizes a constant alpha=1.0 channel for Cannon
      that doesn't exist in the source, inflating `raw_bytes` by another
      25% on top of that. Re-encoding at the fixtures' native 16-bit
      width (measuring the half-precision bit patterns as opaque uint16
      samples — a research measurement only, since CAFE's v0.1 spec's
      `is_valid_sample_format_bit_depth` only accepts
      `SAMPLE_FORMAT_FLOAT` at `bit_depth == 32`, no float16 support
      exists) improved both: `Blobbies.exr` 61.5% -> 55.3%,
      `Cannon.exr` 131.9% -> 124.2%. Additionally dropping Cannon's
      synthesized alpha channel (RGB-only, matching the source exactly)
      improved it further to 113.4%.

      Even after removing both artifacts, `Cannon.exr` still loses to
      its `.exr` file. The reason is unrelated to CAFE's predictors at
      all: `B44` is a **lossy**, fixed-ratio compression scheme — per
      OpenEXR's own technical documentation, "the size of a B44-compressed
      file depends on the number of pixels in the image, but not on the
      data in the pixels", packing every 4x4 block of HALF samples into a
      constant 14 bytes (~44% of uncompressed size) regardless of
      content. Measuring `Cannon.exr`'s real RGB-HALF raw size
      (780×566×3×2 = 2,648,880 bytes) against its on-disk size
      (1,163,637 bytes) confirms this exactly: 43.9%, matching B44's
      documented ratio to within rounding. `Blobbies.exr`'s `ZIP16` is
      lossless, so it was never in this category — the phase above's
      framing of "CAFE loses to `Cannon.exr`" was, in hindsight, an
      apples-to-oranges comparison: a lossless CAFE encode being judged
      against a lossy fixed-ratio baseline that discards image data CAFE
      is not permitted to discard. **This does not change the
      SIMD/palette/dictionary-style no-go verdict** from the HDR
      benchmark wiring phase — it sharpens it: there is no evidence here
      that CAFE's v0.1 predictor set underperforms on HDR content
      generally, only that one specific fixture's baseline was never a
      fair lossless-vs-lossless comparison to begin with, and that
      `image::to_rgba32f()`'s channel-widening/alpha-synthesis behavior
      (a decode-path detail, not a CAFE format limitation) was inflating
      `raw_bytes` for both fixtures. No permanent code changed as a
      result of this investigation — it is diagnostic, not corrective;
      adding native float16 support to `cafe-format`/`cafe-codec` remains
      unscheduled speculative work per this project's "benchmark before
      feature" principle, and would need its own hypothesis/golden-vector
      treatment against a larger HDR corpus than today's two fixtures,
      not a reactive patch driven by a single lossy-vs-lossless
      mismeasurement. All workspace tests (207, unchanged — no
      test-relevant code changed), `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` continue to pass
      cleanly.
- [x] **HDR CLI support (post-HDR-regression-investigation follow-up).**
      `cafe-codec`/`cafe-format` have supported float32 (HDR) data
      end-to-end since Phase 6/7 — the only gap was `cafe-cli`'s
      `png_io.rs` bridge, hardcoded to `bit_depth = 8`/
      `SAMPLE_FORMAT_UINT`, and the `cafe-encode`/`cafe-decode` binaries
      built on it. This follow-up closes that gap without touching
      `png_io.rs`'s existing scope: a new sibling module, `hdr_io.rs`
      (`dynamic_image_to_cafe_pixels_hdr`/`cafe_pixels_to_dynamic_image_hdr`,
      the same `CafePixels` shape `png_io` already returns), handles
      `bit_depth = 32`/`SAMPLE_FORMAT_FLOAT` RGB/RGBA — the only two
      channel layouts `image` 0.25's own OpenEXR codec round-trips (no
      gray/gray+alpha EXR layout exists to support). Byte order is
      big-endian, per spec section 4.1's rule for any sample wider than 8
      bits, matching the convention `cafe_bench::measure::measure_hdr`
      already established.

      Directly applying the HDR regression investigation's finding:
      `hdr_io` dispatches on the *decoded* `image::ColorType`
      (`Rgb32F` -> `COLOR_TYPE_RGB`, `Rgba32F` -> `COLOR_TYPE_RGBA`) rather
      than unconditionally calling `.to_rgba32f()` the way the deleted PoC
      did — `image::ImageReader::decode()` already inspects the source
      file's real channel list and reports `Rgb32F` for an alpha-less EXR
      (confirmed by reading `image`'s `codecs/openexr.rs` directly), so
      this avoids synthesizing a fake alpha channel for `Cannon.exr`-shaped
      sources without needing any `.exr`-specific code in `cafe-cli` at
      all — the same `img.color()`-driven dispatch pattern `png_io`
      already uses for PNG. `png_io.rs` itself gained one new guard rather
      than staying silently wrong: `dynamic_image_to_cafe_pixels` now
      rejects `Rgb32F`/`Rgba32F` input (`PngIoError::
      Float32NotSupportedHere`) instead of silently truncating HDR content
      to 8-bit uint via `to_rgb8()`/`to_rgba8()`, which is what it would
      have done before this phase — a real behavior change, not just a
      new code path alongside the old one; the module doc was reworded to
      state this exclusion as a hard boundary rather than a narrowing.

      `cafe-encode`/`cafe-decode` gained a small dispatcher instead of a
      new flag: `cafe-encode` picks `hdr_io` vs `png_io` from the decoded
      `image::ColorType` (`Rgb32F`/`Rgba32F` -> `hdr_io`, everything else
      -> `png_io`) — the same rule `image::ImageReader::decode()` already
      uses internally to pick its OpenEXR vs. other codecs, so no
      `--format` flag or file-extension sniffing was needed; `cafe-decode`
      picks the same way from the decoded `.cafe` file's own `Ihdr.
      sample_format` (`SAMPLE_FORMAT_FLOAT` -> `hdr_io`, `SAMPLE_FORMAT_
      UINT` -> `png_io`) — the output *container* format is still
      whatever `image` infers from the output path's extension (`.exr`
      for `Rgb32F`/`Rgba32F`, via `image`'s existing `OpenExrEncoder`, no
      new write-path code needed in this crate at all). Both binaries'
      usage strings/module docs were updated to describe both paths
      instead of asserting "PNG only, 8-bit only", since that description
      is no longer accurate for the whole crate (only for `png_io`'s own
      narrower scope, whose doc comment was reworded to match).

      Manually verified end-to-end (not just unit-tested) against both
      `corpus/hdr/` fixtures via a throwaway
      `crates/cafe-bench/examples/verify_hdr_cli_roundtrip.rs` (since
      deleted): `cafe-encode corpus/hdr/Blobbies.exr blobbies.cafe` then
      `cafe-decode blobbies.cafe blobbies_out.exr` reproduces the exact
      original RGBA32F sample values (4,000,000 samples, byte-for-byte via
      `image`'s own re-decode); the same round-trip for `Cannon.exr`
      reproduces the exact original RGB32F sample values (1,324,440
      samples) — confirming both the pixel round-trip *and* that Cannon's
      alpha-less shape survives the CLI path unchanged (5,297,760 raw
      bytes at encode time, RGB not RGBA — down from the 7,063,680 bytes
      the old `.to_rgba32f()`-based PoC would have produced for the same
      file, per the regression investigation above). The pre-existing PNG
      path (`gradient_64x64.png` round-tripped through both binaries) was
      re-verified unaffected by the same manual pass.

      9 new tests (6 in `hdr_io`'s own module: RGB32F round-trip, RGBA32F
      round-trip, big-endian byte order assertion, unsupported-image-
      color-type rejection, unsupported-CAFE-bit-depth rejection,
      unsupported-CAFE-color-type rejection; 1 new in `png_io`:
      float32-source-is-rejected-not-downconverted, replacing the silent
      truncation the old code would have done), for 214 total workspace
      tests (up from 207): 105 `cafe-codec` (lib) + 12 + 3 (`cafe-codec`
      integration files) + 42 `cafe-format` lib + 3 `chunk_proptest` + 16
      `cafe-cli` (up from 9) + 8 golden + 9 spec-invariants + 8
      `cafe-bench`. This does not change any of the HDR benchmark
      numbers reported in the two phases above (`cafe_bench::measure::
      measure_hdr` is untouched — it already used `.to_rgba32f()`
      deliberately for a different reason, matching the synthetic/real
      PNG corpus's uniform RGBA shape for its own comparison table, not
      because it was unaware of the alpha-synthesis issue); this phase's
      alpha-avoidance improvement is specific to the new interactive CLI
      path, not the benchmark path. All workspace tests, `cargo fmt --all
      --check`, `cargo clippy --all-targets -- -D warnings`, and
      `cargo +nightly check --manifest-path fuzz/Cargo.toml` pass cleanly.
- [x] **Palette (0.3) implementation (post-HDR-CLI-support follow-up).**
      Phase 10's no-go verdict on palette explicitly left the door open:
      "a palette-favorable case ... is exactly the category still missing
      real content ... re-evaluate once that content exists". The
      corpus-population follow-up later added real lineart/illustration
      content; this phase re-runs that evaluation and reopens palette
      based on it. A throwaway proof-of-concept (since deleted) encoding
      `corpus/lineart/abacus-psf.png` (256 exact colors) both directly and
      via a hand-rolled indexed transform confirmed a 1.4-1.5x size
      reduction before any permanent code was written, per `AGENTS.md`'s
      "every feature proves itself with a benchmark first" principle.

      **Design, decided up front:** `PLTE` is an *encoder-side transform*
      of `IDAT`'s content, not a new structural `IHDR.color_type` (unlike
      PNG's indexed-color mode) — `IHDR.color_type` always names the
      palette *entries'* real format (RGB=2 or RGBA=6; gray/gray+alpha are
      not palette-eligible), and `PLTE`'s presence alone is what tells the
      decoder `IDAT` holds one index byte per pixel instead of direct
      channel bytes. This keeps `IHDR` untouched and avoids ever inventing
      a fifth `color_type` value. `PLTE` is critical (per spec section
      3.3's naming convention: an unrecognized-but-mandatory chunk must
      abort decoding, and a decoder that doesn't understand `PLTE` cannot
      possibly reconstruct correct pixels), single-instance, and ordered
      after `xMPd` and before the first `IDAT` (spec section 5). Palette
      entries mirror `color_type`'s real channel count (3 bytes for RGB,
      4 for RGBA), always at `bit_depth=8` — `PLTE` is undefined for any
      other bit depth or for gray/gray+alpha, and rejected there. No
      separate `tRNS`-style alpha-only chunk: RGBA entries carry their own
      alpha byte directly. `entry_count` is capped at 256 (new
      `MAX_PALETTE_ENTRIES` security ceiling, CWE-409-class, mirroring
      `MAX_TILE_COUNT`'s treatment) — chosen specifically so indices always
      fit in one byte; no bit-packed 1/2/4-bit index encoding was added,
      since it would only matter for palettes small enough that the 1
      extra bit/pixel is already negligible after ZSTD. No spec version
      bump (stays 0.1) — `AGENTS.md`'s existing "no umbrella library
      crate" and "single API struct" principles already assumed
      backward-compatible ancillary/optional-chunk growth like this.

      **API decision:** mirroring `EncoderOptions::tile_size`, a new
      `EncoderOptions::palette: Option<Vec<u8>>` field declares "the pixel
      bytes I'm about to hand `add_tile`/`encode_bytes` are already
      palette *indices*, one byte per pixel" — `Encoder<W>` itself never
      quantizes colors or scans for exact-color repeats; that's the
      caller's job via a new, independent helper,
      `cafe_codec::palette::build_palette(pixels, channels) ->
      Option<Palette>` (`Palette { entries, indices }`), keeping
      `Encoder<W>` a pure streaming sink exactly as it already was for
      tiling. `build_palette` returns `None` (not an error) once a real
      direct-color image has more than 256 distinct exact colors — an
      ordinary, expected outcome for photographic content, not a
      malformed-input condition. Real color quantization (K-means/
      MedianCut for images with too many distinct colors to index
      directly) remains deferred exactly as `AGENTS.md`'s Core v0.1
      design-decisions table already said: `build_palette` only ever maps
      *exact* pixel values to indices, first-seen order — it is not a
      quantizer.

      Implemented following this project's mandated order for a new
      normative feature: **spec -> invariants -> `cafe-format` ->
      `cafe-codec` -> golden files -> `cafe-cli` -> benchmark -> this
      changelog entry.** `spec/CAFE-spec.md` gained a new normative
      section 4.3 (`PLTE`), with every subsequent chunk section
      renumbered (4.3->4.4 `IDAT`/predictors, 4.4->4.5 `eXIF`, ... 4.8->4.9
      `IEND`), section 5's mandatory chunk order updated, and section 11's
      roadmap entry updated to record this as done rather than deferred.
      Three invariant files changed/were added:
      `spec/invariants/chunks.toml` (`PLTE` added to `mandatory_order`
      plus its own `[[chunk]]` entry), `spec/invariants/security.toml`
      (`max_palette_entries = 256`), and a new
      `spec/invariants/plte.toml` (entry-size-per-color-type table,
      `bit_depth` restriction, `max_entries`) — 4 new tests in
      `crates/cafe-format/tests/spec_invariants.rs` cross-check `plte.toml`
      against `ihdr.toml`'s channel-count table and `security.toml`'s
      ceiling, the same self-consistency pattern every prior invariant
      file already follows.

      `cafe-format` gained: `constants::plte_bytes_per_entry(color_type)
      -> Option<u8>` (`3` for RGB, `4` for RGBA, `None` otherwise),
      `constants::{PLTE_ENTRY_COUNT_LEN, MAX_PALETTE_ENTRIES}`, a new
      `CafeError::InvalidPlte(String)` variant, and a new `plte` module
      (`Plte` struct: `from_colors`/`validate`/`to_payload`/
      `from_payload`/`to_chunk_bytes`, mirroring `idim.rs`'s shape almost
      exactly) — 19 new unit tests. `cafe-codec` gained: a new
      `CodecError::InvalidPaletteIndex(u8)` variant, a new `palette`
      module (`build_palette`/`expand_indices`, 10 new unit tests),
      `decoder::decode_bytes` support (parses an optional `PLTE` before
      the first `IDAT`, exactly like `iDIM`'s existing "before first IDAT,
      single instance" enforcement; computes `IDAT`'s effective `bpp` as
      `1` instead of the direct channel count whenever `PLTE` is present;
      expands indices back to full pixel bytes via `expand_indices` at the
      end of decoding, so `DecodedImage::pixels` is always direct pixels
      regardless of whether the source file was palette-encoded — 6 new
      tests), and `encoder::Encoder::new` support (parses+validates
      `EncoderOptions::palette` via `Plte::from_colors`/`validate`, uses
      effective `bpp=1` for the whole encode when set, writes the `PLTE`
      chunk immediately after `iDIM` and before any `IDAT` — 6 new tests,
      including a combined palette+tiling round-trip). `EncoderOptions`
      lost its `derive(Copy)` (now `Debug, Clone, PartialEq` only) due to
      the new `Vec<u8>` field; the one caller that relied on `Copy`
      (`cafe-encode`) was updated to `.clone()`. Two new hand-built golden
      fixtures were generated via the existing `#[ignore]`d
      `generate_golden_fixtures` test (never a real encoder, per Phase 4's
      precedent): `golden/minimal_2x2_indexed_rgb.cafe` (a valid 2x2 RGB
      image, 2-color palette, confirmed via a new decoder-level test to
      expand back to the exact expected direct pixels) and
      `golden/malformed/plte_entry_count_zero.cafe` (a `PLTE` chunk
      declaring zero entries, confirmed rejected with
      `CafeError::InvalidPlte`) — 3 new golden tests across
      `cafe-format`'s `golden_files.rs` and `cafe-codec`'s `decoder.rs`.

      `cafe-cli` gained palette support across all the places a direct
      pixel buffer already flows: `cafe inspect` now parses and prints any
      `PLTE` chunk's entry count/bytes-per-entry (or "absent" when there
      is none); `cafe explain` now accounts for `PLTE`'s effective `bpp=1`
      when extracting predictor codes per tile (previously always used
      `Ihdr::bytes_per_pixel()` directly, which would have read the wrong
      byte offsets for a palette-encoded file) and prints whether a
      palette is present; `cafe-encode` now races a palette encode against
      the direct encode automatically whenever the *decoded* image is
      8-bit uint RGB/RGBA (`palette_channels` helper) and `build_palette`
      succeeds (at most 256 distinct exact colors), keeping whichever
      output is smaller — mirroring the existing raw-vs-ZSTD-per-tile
      fallback race one layer up — with a new `--no-palette` flag to skip
      the attempt entirely; `cafe-decode` needed no changes at all, since
      `decode_bytes` already always returns expanded direct pixels
      regardless of how the file was encoded. `cafe_bench::measure` gained
      the identical direct-vs-palette race (new `Measurement::
      used_palette: bool` field), so `cafe benchmark`'s existing
      corpus-wide table now reports real palette wins inline (a `plte`
      column) rather than requiring a separate benchmark pass.

      Re-running `cafe benchmark` against the full corpus (unchanged
      manifest from the corpus-population phase) with this palette race
      enabled:

      ```
      TOTAL: raw=3760128 png=1324113 (35.2%) cafe=751155 (20.0%)
      ```

      down from the HDR-CLI-support phase's `cafe=795462 (21.2%)` — a
      workspace-wide improvement even though only 6 of the corpus's 22
      PNG entries actually have few enough exact colors to benefit
      (`pixelart/checkerboard4_256x256`, all four `lineart/` entries
      except `aardvark2-psf-colourised` which has too many distinct
      colors, and both `illustration/` entries). The gains on those 6
      entries are substantial where they apply — e.g.
      `lineart/abdomen-psf.png` drops from 23,395 to 15,212 bytes (a
      further ~35% on top of CAFE's already-direct-encode win over PNG),
      `illustration/abstract-art-psf.png` from 38,394 to 26,561 (~31%
      further) — confirming the original PoC's 1.4-1.5x hypothesis held
      up in the real, fully-implemented, benchmark-verified path, not
      just the throwaway estimate. Every other corpus entry (photos,
      screenshots, gradients, textures, synthetic noise) is unaffected,
      as expected: `build_palette` either returns `None` for them (too
      many distinct colors) or the race correctly keeps the direct
      encode when indexing doesn't help (e.g. `pixelart/
      checkerboard4_64x64` and `gradient/*`, whose already-near-zero
      direct-encode sizes leave no room for an indexed encode to beat,
      confirmed by a dedicated `cafe-bench` test measuring a real
      256-entry corpus file rather than asserting palette always wins).
      HDR's `cafe vs exr` numbers are unchanged (palette is irrelevant to
      float32 content; `measure_hdr` was not touched).

      44 new tests in this phase (4 spec-invariants + 18 `cafe-format`
      `plte` unit tests + 10 `cafe-codec` `palette` unit tests + 6
      `decoder` PLTE tests + 6 `encoder` PLTE tests + 2 golden-file
      [`cafe-format`'s 2 new fixture tests, `cafe-codec`'s 1 new golden
      decode test already counted in `decoder`'s 6 above] + 1 `cafe-bench`
      measure test — see each module's own test list above for exact
      breakdowns), for 261 total workspace tests (up from 214 at the end
      of the HDR-CLI-support phase): 127 `cafe-codec` (lib, up from 105)
      + 12 + 3 (`cafe-codec` integration files, unchanged) + 60
      `cafe-format` lib (up from 42) + 3 `chunk_proptest` (unchanged) +
      16 `cafe-cli` (unchanged) + 11 golden (up from 8: 2 new
      `cafe-format`-side fixture tests plus the pre-existing ignored
      generator) + 13 spec-invariants (up from 9) + 17 `cafe-bench` (up
      from 8 — includes tests added by the corpus-population and
      HDR-benchmark-wiring follow-ups in between, not just this phase's 1
      new test). All workspace tests, `cargo fmt --all --check`, `cargo
      clippy --all-targets -- -D warnings`, and `cargo +nightly check
      --manifest-path fuzz/Cargo.toml` pass cleanly.
- [x] **SIMD (0.2) for predictors (post-Palette-implementation
      follow-up).** Phase 10's SIMD no-go verdict was benchmark-driven and
      always left the door open to revisit; this phase reopens it with a
      fresh benchmark, per `AGENTS.md`'s "every feature proves itself with
      a benchmark first" principle. A throwaway benchmark PoC (three
      successive throwaway `crates/cafe-bench/examples/simd_poc*.rs`
      files, all deleted after use) measured, on synthetic "photo-like"
      images up to 4096x4096 at the encoder's default ZSTD level (19):
      predictor selection alone takes ~1.2s, ZSTD alone (on the raw
      buffer) ~18.3s, and the full `encode_bytes` pipeline ~46.4s — ZSTD
      dominates encode time so overwhelmingly (~97%) that vectorizing the
      predictors alone would save at most ~3% end-to-end at this level. A
      follow-up PoC sweeping ZSTD levels 1/3/6 on the same 4096x4096 image
      inverted that finding: at level 1, predictor selection (566ms)
      dominates ZSTD (145ms) by 4:1, and level 3 is similar — the balance
      only flips back to ZSTD-dominated around level 6.

      **Scope decision:** SIMD is a real win specifically for callers who
      choose a fast `EncoderOptions::level` (interactive/preview use), not
      a general win at the encoder's own default level 19 — this is
      recorded here as the honest scope, not oversold as an
      across-the-board speedup. Within that scope, two further boundaries
      were set from a close reading of `predictor.rs`'s existing
      structure: `filter_row` (encoder direction) is vectorized for all 6
      predictor codes, since every one reads only already-known input
      bytes (`row`/`prev_row`) with zero byte-to-byte output dependency —
      genuinely embarrassingly parallel; `unfilter_row` (decoder
      direction) is vectorized only for `None`/`Up`, the two codes whose
      reconstruction doesn't depend on the just-reconstructed left
      neighbor within the *output* buffer (`Sub`/`Average`/`Paeth`/
      `Gradient` decoding is an inherently serial prefix dependency,
      `out[x]` needs `out[x-bpp]`, not a good SIMD target without a
      parallel-prefix-scan rewrite nothing in this phase's benchmark data
      showed a need for — decode is already fast, 244ms for a 67MB image
      in the same PoC). Both x86_64 (AVX2) and aarch64 (NEON) are
      implemented together in this phase, per the "scalar-is-reference,
      SIMD-is-optimization" architecture `AGENTS.md` already committed to
      from day one — AVX2 is runtime-detected (`is_x86_feature_detected!`,
      since not every x86_64 CPU has it); NEON needs no runtime check,
      since every aarch64 target Rust supports mandates it as a baseline
      ISA feature.

      **Implementation:** a new `cafe-codec::simd` module (private,
      `mod simd` not `pub mod`, since it's purely an internal optimization
      with no public API surface of its own) with two architecture-gated
      submodules, `x86` (`#[cfg(target_arch = "x86_64")]`) and `neon`
      (`#[cfg(target_arch = "aarch64")]`), plus a shared
      `filter_row_simd`/`unfilter_row_simd` dispatcher pair that returns
      `Option<Vec<u8>>` (`None` whenever no SIMD path applies: unsupported
      code, a row shorter than a new `MIN_SIMD_LEN = 64` tuning threshold
      below which SIMD setup overhead isn't worth it, or no matching CPU
      feature/architecture at all). `predictor::filter_row`/
      `unfilter_row` were split into a thin SIMD-attempting wrapper plus a
      newly-public `filter_row_scalar`/`unfilter_row_scalar` pair — the
      former keeps its exact prior name and signature (so
      `crate::tile`/`crate::encoder`/`crate::decoder` needed zero changes
      to opt into SIMD transparently), the latter is the
      format-defining scalar reference this crate's parity tests check
      every SIMD path against, per `AGENTS.md`'s scalar-is-reference
      architecture. Every predictor's "left"/"up-left" neighbor is just
      `row`/`prev_row` read at a `bpp`-byte-earlier offset, so a single
      **row-shift trick** (unaligned vector loads at both `row[i]` and
      `row[i - bpp]`) makes every implementation `bpp`-generic — no
      per-`bpp`-value (1/2/3/4/6/8/12/16) specialization anywhere; the
      first `bpp` bytes of each row (where `x - bpp` would read
      out-of-bounds) always fall through to the scalar reference path via
      each SIMD function's own scalar prologue.

      **Math identities** (see `x86.rs`'s module doc for the full
      derivations, shared verbatim by `neon.rs`): `Sub`/`Up` need only a
      single wrapping 8-bit subtract per lane; `Gradient`'s `(a + b - c)
      mod 256` is already pure wrapping arithmetic (unlike Paeth), so it
      needs no widening either; `Average`'s `floor((a+b)/2)` uses the
      classic `(a & b) + ((a ^ b) >> 1)` bit-trick to avoid u16 widening,
      with the per-byte `>> 1` itself done via a 16-bit-lane shift plus an
      `0x7F` mask (AVX2) or NEON's direct `vhaddq_u8` half-add instruction
      (no trick needed — NEON has a native floor-average op AVX2 lacks);
      `Paeth` is the one predictor with no shortcut around its actual
      `|p-a|`/`|p-b|`/`|p-c|` comparisons, so it genuinely widens each
      16-byte (AVX2) or 8-byte (NEON) half-lane to 16-bit, computes the
      three distances and a branchless select matching
      `paeth_predictor`'s exact tie-breaking rule (`pa <= pb && pa <= pc`
      -> left, expressed as negated-greater-than compares so ties resolve
      identically to the scalar reference), then narrows back to `u8`
      before the final wrapping subtract.

      **Testing:** a new `crates/cafe-codec/tests/simd_parity.rs` (8
      tests: 6 exhaustive deterministic sweeps — first-row/with-prev-row
      x filter/unfilter, an extreme-values 0x00/0xFF sweep specifically
      targeting Paeth/Average/Gradient's widen-narrow/branchless-select
      math at saturation boundaries, and a full filter-then-unfilter
      round-trip through whichever path is actually dispatched — each
      run across every spec-relevant `bpp` value (1/2/3/4/6/8/12/16) and
      21 row lengths straddling both SIMD lane widths (16 for NEON, 32
      for AVX2) and the `MIN_SIMD_LEN` threshold, from empty/short-scalar-
      only through several-lanes-plus-uneven-tail; plus 2 proptest
      properties as an unbiased complement to the hand-picked deterministic
      lengths). All 8 passed against this development machine's real
      AVX2 hardware. **NEON validation** followed the plan decided for
      this phase: `neon.rs` was confirmed to compile and type-check
      correctly for `aarch64-unknown-linux-gnu` via a standalone
      throwaway `rustc --crate-type lib --target aarch64-unknown-linux-gnu`
      harness (full `cargo check --target aarch64-unknown-linux-gnu`
      wasn't possible in this environment — the workspace's `zstd-sys` C
      dependency needs an `aarch64-linux-gnu-gcc` cross-compiler this
      Windows machine doesn't have installed — so this narrower
      rustc-direct check covers exactly the new code's own syntax/types,
      independent of that unrelated cross-compilation gap); real
      byte-for-byte NEON execution is deferred to a new CI job (below),
      never asserted as locally-verified when it wasn't.

      A new Criterion benchmark, `crates/cafe-codec/benches/
      predictor_simd.rs` (`cafe-codec` gained `criterion` as a
      dev-dependency and its own `[[bench]]` target, mirroring
      `cafe-bench`'s existing benchmark setup one crate over), measures
      scalar vs SIMD-dispatched throughput per predictor on a
      representative 1024px RGBA row (`bpp=4`, 4096 bytes) — confirmed
      speedups on this development machine's AVX2 hardware: Sub 29.3x,
      Up 26.1x, Average 21.2x, Gradient 19.2x, Paeth 8.0x (lower, as
      expected, since it's the one predictor that couldn't avoid
      widen/narrow overhead), and decoder-side Up 11.7x.

      **CI:** a new `simd-parity-aarch64` job in
      `.github/workflows/ci.yml` runs `cargo test -p cafe-codec --test
      simd_parity --release` natively on GitHub's `ubuntu-24.04-arm`
      hosted runner (free for public repos) — this is the real NEON
      hardware validation this phase's local environment couldn't
      provide, closing the loop the module doc comment above promises
      rather than leaving it as an unchecked claim; the pre-existing
      `ci.yml` bottom-of-file comment listing "not yet added" future CI
      work had its now-obsolete
      "aarch64-cross-compile / arm64-native-test (deferred to 0.2)" line
      removed, since this phase is that deferred item, done.

      8 new tests in `crates/cafe-codec/tests/simd_parity.rs` (see the
      Testing paragraph above for the breakdown), for 269 total workspace
      tests (up from 261 at the end of the Palette-implementation phase):
      127 `cafe-codec` (lib, unchanged) + 12 + 3 + 8 (`cafe-codec`
      integration files, up from 12 + 3 — the new `simd_parity.rs`) + 60
      `cafe-format` lib (unchanged) + 3 `chunk_proptest` (unchanged) + 16
      `cafe-cli` (unchanged) + 11 golden (unchanged) + 13 spec-invariants
      (unchanged) + 17 `cafe-bench` (unchanged) + 2 doc-comment updates
      (`x86.rs`/`neon.rs`, not counted as tests). All workspace tests,
      `cargo fmt --all --check`, `cargo clippy --all-targets --
      -D warnings`, and `cargo +nightly check --manifest-path
      fuzz/Cargo.toml` pass cleanly.
- [x] **HDR corpus expansion (post-SIMD follow-up).** The HDR benchmark
      wiring and regression investigation phases above drew conclusions
      from only two `.exr` fixtures (`Blobbies.exr`/`Cannon.exr`) — this
      follow-up grows `corpus/hdr/` to 12 files, all sourced from the same
      already-documented, already-license-checked
      [`openexr-images`](https://github.com/AcademySoftwareFoundation/openexr-images)
      repository (BSD-3-Clause, per every source subdirectory's own
      `README.rst`), to get a broader read on how consistently CAFE's
      predictor set performs against OpenEXR's own compression.

      Ten new files were added: seven more from `ScanLines/`
      (`CandleGlass`, `Carrots`, `Desk`, `MtTamWest`, `PrismsLenses`,
      `StillLife`, `Tree` — the same directory `Blobbies`/`Cannon` already
      came from) plus three from different subdirectories for content
      diversity beyond still-life photography: `TestImages/SquaresSwirls.exr`
      (synthetic squares/swirls pattern), `TestImages/RgbRampsDiagonal.exr`
      (a smooth diagonal gradient ramp), and `Chromaticities/Rec709.exr`
      (a real-world outdoor photo, included specifically for its
      Rec.709-chromaticities RGB channel layout). Two other candidates —
      `TestImages/GrayRampsDiagonal.exr` and `LuminanceChroma/Garden.exr`
      — were tried first and rejected: both store luminance/chroma or
      single-channel data rather than real RGB, which `image` 0.25's
      OpenEXR decoder (`image does not contain non-deep rgb channels`)
      cannot decode at all — a hard constraint discovered empirically
      during this phase, not a preference, so every file actually chosen
      was verified importable before being kept. `EXPECTED_HDR_SOURCES` in
      `crates/cafe-bench/src/bin/import-corpus.rs` grew to list all 12
      files; re-running `cafe-bench --bin import-corpus -` (the `-`
      sentinel added in the HDR benchmark wiring phase, since HDR sources
      are registered directly from `corpus/hdr/`, never staged) merged all
      10 new entries into `corpus/manifest.json` via the pre-existing
      `register_hdr_sources`/`merge_and_write` path with no code changes
      needed there. `corpus/ATTRIBUTION.md`'s HDR section was rewritten to
      list all 12 files (with their source subdirectory) and to record the
      two-rejected-candidates constraint above, so a future contributor
      doesn't repeat the same failed attempt; `.gitignore`'s `corpus/hdr/`
      comment was reworded from "staged for a future CLI pass (not yet
      referenced by manifest.json)" to reflect that it's been wired into
      the benchmark path since the HDR benchmark wiring phase, not still
      pending.

      Re-running `cafe benchmark`: the PNG-entries table's `TOTAL` line is
      byte-for-byte unchanged (`raw=3760128 png=1324113 (35.2%)
      cafe=751155 (20.0%)`), confirming this phase touched nothing on that
      path. The expanded HDR table **reinforces, rather than overturns**,
      the regression investigation's finding that `Cannon.exr`-style
      lossy-fixed-ratio-compressed sources aren't a fair lossless-vs-lossy
      baseline: 9 of the 10 new files land between 123-160% of their
      source `.exr`'s size (worse than the original two-fixture sample's
      worst case), with only `RgbRampsDiagonal.exr` (51.2%, a smooth
      gradient — exactly the content type CAFE's predictors already excel
      at throughout every other benchmark in this project) and the
      pre-existing `Blobbies.exr` (61.5%) beating their source file.
      Manually cross-checking a few of the worst performers' compression
      methods against their file headers (via the same `exr` crate
      inspection technique the regression investigation phase used)
      confirms the same root cause generalizes: OpenEXR's own PIZ/B44/ZIP
      wavelet- and Huffman-based schemes are simply well-tuned for HDR
      float/half data in ways CAFE's six spec predictors — designed around
      uint8/16 integer neighbor-delta locality — don't replicate. **This
      does not change the SIMD/palette/dictionary no-go verdicts, nor does
      it retroactively invalidate CAFE's v0.1 scope decisions** — HDR
      support has always been framed in this project as "the format can
      correctly represent float32 HDR data end-to-end" (true, and
      unaffected by this phase), never as "CAFE's generic predictors beat
      specialized HDR codecs" (evidently often false, now confirmed on a
      6x larger sample than before). Adding an HDR-tuned predictor or
      transform remains unscheduled speculative work, per this project's
      "benchmark before feature" principle — this phase's contribution is
      a materially larger, honestly-reported evidence base for that future
      decision, not a fix. No new tests were added (no test-relevant code
      changed — `register_hdr_sources`/`merge_and_write`/the benchmark
      path are all pre-existing and already covered); all 269 workspace
      tests, `cargo fmt --all --check`, and
      `cargo clippy --all-targets -- -D warnings` continue to pass
      cleanly.
- [x] **16-bit uint CLI support (post-HDR-corpus-expansion follow-up).**
      `cafe-codec`/`cafe-format` have supported `bit_depth = 16` end-to-end
      since Phase 5/6 — the only gap, called out explicitly in `png_io.rs`'s
      own module doc since Phase 9, was that `cafe-cli`'s `png_io` bridge
      always coerced any input down to 8-bit uint via `image`'s
      `to_luma8()`/`to_rgb8()`-family conversions, discarding a 16-bit
      source's real bit depth. This follow-up closes that gap, mirroring
      the HDR-CLI-support follow-up's approach one bit depth down: dispatch
      on the *decoded* `image::ColorType` rather than adding a CLI flag.

      **Design decision:** unlike HDR (a distinct `sample_format`, split
      into its own `hdr_io` module), 8-bit and 16-bit uint share
      `sample_format = SAMPLE_FORMAT_UINT` and the same four `color_type`
      values (spec section 4.1) — so 16-bit support was added directly
      inside the existing `png_io.rs` rather than a new sibling module,
      keeping one bridge per `sample_format` rather than one per
      `bit_depth`. `dynamic_image_to_cafe_pixels` now inspects the decoded
      `image::ColorType` (`L16`/`La16`/`Rgb16`/`Rgba16` -> `bit_depth = 16`,
      everything else -> `bit_depth = 8`, `Rgb32F`/`Rgba32F` still rejected
      exactly as before) instead of unconditionally calling the 8-bit
      `to_*8()` conversions — a real behavior change: a 16-bit source PNG
      now round-trips at its original bit depth rather than being silently
      downconverted, the same kind of hard-boundary correction the
      HDR-CLI-support follow-up made for float32 input. 16-bit samples are
      big-endian on the wire, per spec section 4.1's rule for any sample
      wider than 8 bits — the same convention `hdr_io` already established
      for float32 — via two small new private helpers,
      `u16_samples_to_be_bytes`/`be_bytes_to_u16_samples`, shared by every
      16-bit `color_type` branch in both conversion directions.
      `cafe_pixels_to_dynamic_image`'s guard changed from "reject anything
      but `bit_depth = 8`" to "reject anything but `bit_depth = 8` or `16`
      (still only for `SAMPLE_FORMAT_UINT`)", dispatching to
      `image::ImageBuffer::<Luma/LumaA/Rgb/Rgba<u16>, _>::from_raw` for the
      16-bit case.

      No changes were needed in `hdr_io.rs`, `cafe.rs` (`inspect`/`verify`/
      `explain` already work in terms of `Ihdr.bit_depth` generically, never
      assuming 8), or `cafe_bench::measure` (still deliberately RGBA8-only
      for its own PNG-corpus comparison table, unrelated to this CLI-only
      gap). `cafe-encode`/`cafe-decode` needed no dispatcher changes either
      — both already call `png_io` for any non-float32 decoded color type,
      so 16-bit PNGs simply flow through the same call path with no new
      branching; `cafe-encode`'s existing palette race
      (`palette_channels`) already correctly excludes 16-bit input via its
      pre-existing `bit_depth != 8` check (PLTE is undefined above 8-bit
      per spec section 4.3), so no changes were needed there either.

      Manually verified end-to-end (not just unit-tested), per this
      project's established practice, via two throwaway
      `crates/cafe-bench/examples/{gen_16bit_png,verify_16bit_roundtrip}.rs`
      files (since deleted): a synthetic 8x8 16-bit RGBA PNG encoded via
      `cafe-encode` (512 raw bytes, 147-byte `.cafe` output — confirmed via
      `cafe inspect` reporting `bit_depth: 16`, `color_type: 6 (RGBA)`, no
      `PLTE`/`iDIM`), decoded back via `cafe-decode`, and the two PNGs'
      `to_rgba16()` sample buffers confirmed byte-for-byte identical (256
      samples).

      2 net new tests in `png_io.rs` (the pre-existing
      `test_16bit_source_is_downconverted_to_8bit` was renamed to
      `test_16bit_gray_roundtrip_preserves_bit_depth` and rewritten to
      assert big-endian round-trip preservation instead of downconversion,
      since that's a real behavior change, not merely an addition, so it
      isn't counted as a new test; `test_unsupported_cafe_bit_depth_is_
      rejected` was updated to use `bit_depth = 32` with
      `SAMPLE_FORMAT_UINT` as its rejected case, since `bit_depth = 16` is
      no longer unsupported; two genuinely new round-trip tests,
      `test_16bit_rgba_roundtrip_through_dynamic_image` and
      `test_16bit_byte_order_is_big_endian`, added following the same
      pattern `hdr_io`'s own tests already use), for 18 total `cafe-cli`
      lib tests (up from 16) and 272 total workspace tests (up from 270 at
      the end of the HDR-corpus-expansion phase). Manually verified
      end-to-end (not just unit-tested), per this project's established
      practice, via two throwaway `crates/cafe-bench/examples/
      {gen_16bit_png,verify_16bit_roundtrip}.rs` files (since deleted): a
      synthetic 8x8 16-bit RGBA PNG encoded via `cafe-encode` (512 raw
      bytes, 147-byte `.cafe` output — confirmed via `cafe inspect`
      reporting `bit_depth: 16`, `color_type: 6 (RGBA)`, no `PLTE`/`iDIM`),
      decoded back via `cafe-decode`, and the two PNGs' `to_rgba16()`
      sample buffers confirmed byte-for-byte identical (256       samples). All
      workspace tests, `cargo fmt --all --check`, and `cargo clippy
      --all-targets -- -D warnings` pass cleanly.
- [x] **Metadata chunks: eXIF/jSON/iCCP/xMPd (post-16-bit-uint-CLI-support
      follow-up).** Spec section 5 has always listed `eXIF`/`jSON`/`iCCP`/
      `xMPd` as defined ancillary chunk types (their framing/ordering rules
      already existed in `spec/invariants/chunks.toml`), but no crate
      actually read or wrote their content — `cafe-format`'s `read_chunk`
      only ever returned them as opaque, unparsed bytes, and neither the
      decoder nor the encoder had any awareness of them at all. This phase
      closes that spec-vs-implementation gap for all four types.

      **Design decisions, per this project's decoder-before-encoder,
      spec-first process:** `eXIF`/`iCCP` stay opaque `Vec<u8>` blobs — spec
      section 4.5's only rule for `eXIF` is "raw EXIF bytes, first instance
      wins", and `iCCP`'s content (an ICC color profile) is likewise
      meaningless for this project to parse — so neither gets a dedicated
      `cafe-format` struct, only pass-through handling via
      `chunk::write_chunk`/`read_chunk` directly, kept consistent with the
      "decoder needs to know a small, fixed set of primitives" principle:
      CAFE doesn't need to understand EXIF/ICC structure to pass it through
      losslessly. `jSON` and `xMPd`, in contrast, have real internal framing
      (`jSON`'s 1-byte namespace-length prefix ahead of its JSON payload)
      or a validity constraint worth enforcing at the format layer (`xMPd`'s
      UTF-8 requirement, since XML is inherently text) — both get dedicated
      `cafe-format` modules mirroring `idim.rs`/`plte.rs`'s existing shape
      (`new`/`to_payload`/`from_payload`/`to_chunk_bytes`). Per spec section
      8.4, a decoder must never fail a whole file over malformed *ancillary*
      chunk content — so `jSON`/`xMPd` parse failures are silently
      discarded by the decoder (`if let Ok(...) = ...`, no error
      propagated), while structural/framing failures (truncation,
      decompression) still propagate exactly like any other chunk, and the
      *encoder* (given content the caller controls directly) still rejects
      malformed content up front as a caller error, never silently drops
      it. `eXIF`/`iCCP`/`xMPd` are single-instance (first occurrence wins,
      spec section 4.5's explicit rule for `eXIF`, applied consistently to
      the other two since the spec doesn't specify otherwise); `jSON` is
      the only repeatable type, and all instances are kept in file order.

      Implemented as: `cafe-format` gained `CafeError::
      InvalidJsonChunk(String)`/`InvalidXmpd(String)`, a new
      `JSON_NAMESPACE_LEN_FIELD_LEN` constant, a `serde_json` workspace
      dependency, and two new modules — `json` (`JsonChunk { namespace,
      payload }`, 14 tests) and `xmpd` (`Xmpd { xml }`, 5 tests) — both
      re-exported from `lib.rs`; a new `spec_invariants.rs` test,
      `metadata_chunks_are_ancillary_and_json_is_the_only_repeatable_one`,
      cross-checks `chunks.toml`'s existing rules for all four types. Two
      new golden fixtures were hand-built via the existing `#[ignore]`d
      `generate_golden_fixtures` test (same precedent as every prior golden
      fixture in this project — never a real encoder):
      `golden/minimal_1x1_gray_with_metadata.cafe` (all four chunk types
      present, validated by a new `golden_files.rs` test and a new
      `cafe-codec` decoder test). `cafe-codec`'s `decoder::DecodedImage`
      gained `exif: Option<Vec<u8>>`, `json_chunks: Vec<JsonChunk>`,
      `icc_profile: Option<Vec<u8>>`, `xmp: Option<Xmpd>`; `decode_bytes`
      now recognizes all four chunk types at their spec-mandated position
      (before `PLTE`/`IDAT`), enforcing first-instance-wins for the three
      single-instance types and file-order collection for `jSON` (7 new
      tests). `cafe-codec`'s `encoder::EncoderOptions` gained the mirror
      fields (`exif`, `json_chunks`, `icc_profile`, `xmp: Option<String>`);
      `Encoder::new` writes any present metadata chunks eagerly, right
      after `iDIM` and before `PLTE`/`IDAT` per spec section 5's mandatory
      order, racing raw-vs-ZSTD via the same `compress_with_fallback`
      policy `IDAT` already uses (respecting `EncoderOptions::allow_zstd`,
      5 new tests); `EncoderOptions` lost its `derive(Copy)` (now `Debug,
      Clone, PartialEq` only) due to the new `Vec<u8>`/`Vec<JsonChunk>`
      fields, with `cafe-encode` updated to `.clone()` at its one call
      site that relied on `Copy`.

      `cafe-cli` gained: `cafe inspect` prints a metadata section (presence
      and byte counts for `eXIF`/`iCCP`/`xMPd`, namespace+payload size per
      `jSON` chunk), decompressing each chunk directly via
      `zstd_codec::decompress_chunk` rather than adding new `cafe-codec`
      API surface (matching `chunks.rs`'s existing "presentation-layer
      parsing belongs in `cafe-cli`" precedent); `cafe explain` gained one
      summary line reporting each type's presence/count; `cafe-encode`
      gained `--exif <file>`, `--icc <file>`, `--xmp <file>`, and
      repeatable `--json <ns>:<file>` flags, each populating the
      corresponding new `EncoderOptions` field and erroring out cleanly on
      invalid `--json` namespace/JSON syntax before ever calling
      `Encoder::new`; `cafe-decode` gained the mirror `--extract-exif`,
      `--extract-icc`, `--extract-xmp` flags, writing a decoded file's
      recovered metadata bytes back out to disk (this required
      restructuring `cafe-decode`'s previously-inline `main`/`run` into a
      small `parse_args`/`Args` pair, since it now needs to parse optional
      flags beyond the original fixed two positional arguments).

      Manually verified end-to-end (not just unit-tested), per this
      project's established practice: encoding
      `corpus/gradient/gradient_64x64.png` via `cafe-encode` with all four
      new flags produced a file `cafe inspect` confirmed contains
      `eXIF`/`jSON`/`iCCP`/`xMPd` in the correct spec order ahead of
      `IDAT`, which `cafe verify` accepted and `cafe explain` summarized
      correctly; `cafe-decode --extract-exif/--extract-icc/--extract-xmp`
      recovered each embedded file's bytes exactly; decoding that file
      back to PNG and re-encoding the result *without* any metadata flags
      produced a `.cafe` file with an identical SHA-256 hash to encoding
      the original PNG directly — confirming metadata embedding has zero
      effect on pixel data or predictor/compression decisions, as
      required. `cafe-encode` was also confirmed to reject a
      deliberately-malformed `--json` file's syntax error before writing
      any output, distinct from the decoder's silent-discard behavior for
      untrusted file input.

      33 new tests across four areas (14 `json` + 5 `xmpd` in
      `cafe-format`'s lib, 1 `spec_invariants`, 1 `golden_files`, 7
      `decoder` + 5 `encoder` in `cafe-codec`'s lib), for 305 total
      workspace tests (up from 272 at the end of the 16-bit-uint-CLI-support
      phase): 139 `cafe-codec` (lib, up from 127) + 12 + 3 + 8
      (`cafe-codec` integration files, unchanged) + 79 `cafe-format` lib
      (up from 60) + 3 `chunk_proptest` (unchanged) + 18 `cafe-cli`
      (unchanged — no unit tests live in `cafe-cli`'s binaries themselves,
      per this project's presentation-layer convention) + 12 golden (11
      passed + 1 ignored generator, up from 11 total) + 14 spec-invariants
      (up from 13) + 17 `cafe-bench` (unchanged). All workspace tests,
      `cargo fmt --all --check`, and `cargo clippy --all-targets --
      -D warnings` pass cleanly.

## Commands

```bash
cargo build                # build the whole workspace
cargo test                 # run all tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
