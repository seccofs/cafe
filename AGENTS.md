# CAFE Reboot — Developer Guide & Roadmap

## Why a reboot

The original implementation (`cafe-rs` v1.12.0, preserved untouched in
[`old/`](old/)) grew feature-by-feature over ~12 releases shipped within
days of each other: 16 predictors, 5 filter heuristics, Adam7 + even/odd
interlacing, indexed palette as a structural format mode, auto-trained
ZSTD dictionaries, a monolithic 7.8k-line `src/cafe.rs`, and two parallel
`EncodeOptions`/`EncoderOptions` structs. It works and is well-tested, but
it grew via `problem -> feature -> another problem -> another feature`
rather than `hypothesis -> benchmark -> minimal feature -> spec -> golden
vectors -> implementation`.

This reboot keeps the core insight — *don't invent another general
compressor; adaptively transform pixels so Zstandard compresses them
better* — and rebuilds around a much smaller, boringly-simple format with
benchmarking and a golden corpus from commit zero, decoder-first, with a
single streaming-first API.

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

## What v0.1 keeps, defers, or removes

| Area | v0.1 decision |
|---|---|
| Chunk container (Length/Type/Flag/Data/CRC32, critical/ancillary naming) | **Keep**, adapted from `old/src/chunk.rs` |
| ZSTD with raw-vs-compressed fallback | **Keep**, adapted from `old/src/codec.rs` |
| Security ceilings (decompression budget, tile count, palette count) | **Keep**, adapted from `old/src/constants.rs` |
| Predictors | **Reduce** to 6: None, Sub, Up, Average, Paeth, Gradient — behind a new `trait Predictor` (didn't exist in v1) |
| Per-row predictor selection | **Keep**, structural from day one (not bolted on later) |
| Tiling | **Unify** into a single `iDIM`-based mechanism (32/64/128, default 64×64) — v1 had three parallel APIs (`add_tile`/`add_idim_tile`/`add_even_odd_rows`) |
| Scan order | **Keep** row-major + Morton/Z-order only |
| SIMD (AVX2/NEON) | **Defer to 0.2.** Scalar-is-reference/SIMD-is-optimization architecture planned from day one, but not implemented until the scalar format is validated |
| Adam7 interlace | **Remove.** Not streamable, high complexity, low benefit vs. tiles |
| Even/Odd interlace | **Remove** from core. Tiles already give progressive display |
| Indexed/Palette (PLTE) | **Defer to 0.3**, as an *encoder-side transform* (`direct` vs `palette transform`), not a decoder structural mode |
| K-means / MedianCut quantization | **Defer to 0.3**, encoder-only, decoder never needs to know how the encoder chose colors |
| ZSTD dictionary (zDIC/auto_dictionary) | **Defer to 0.4** (external dictionary first, embedded/trained later) — conflicts with single-pass streaming |
| HDR (FP16/PQ/HLG/tonemap) | **Defer.** Core v0.1 supports uint8/16 + float32 only. `old/src/tonemap.rs` kept as reference, not ported |
| Metadata (EXIF/ICC/XMP/JSON) | **Keep**, ancillary, decoder must decode pixels without understanding metadata |
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
│   ├── CAFE-spec.md          # normative, simplified vs old/docs/CAFE-spec.md
│   ├── invariants/           # spec rules expressed as data/tests
│   └── test-vectors/         # golden test cases derived from invariants
├── corpus/                   # photo/screenshot/illustration/pixelart/lineart/
│                              # gradient/texture/synthetic/hdr + manifest.json
├── golden/                    # golden .cafe files + golden/malformed/
├── fuzz/fuzz_targets/         # decode_fuzz, chunk_roundtrip_fuzz (ported from old/)
├── tests/                     # workspace-level integration tests
├── .github/workflows/         # ci.yml, fuzz.yml (adapted from old/)
├── old/                       # frozen v1.12.0 reference implementation — do not modify
└── Cargo.toml                 # workspace root
```

No umbrella `cafe` library crate — consumers depend on `cafe-format`
and/or `cafe-codec` directly. The CLI binary is still named `cafe` (inside
`cafe-cli`), plus the legacy-compatible `cafe-encode`/`cafe-decode`
binaries.

## Phased roadmap

- [x] **Phase 0 — Skeleton & CI.** Workspace with 4 crates, placeholder
      binaries, basic CI (fmt/clippy/test).
- [x] **Phase 1 — `cafe-bench` minimal.** Harness comparing compressed size
      against PNG (via the `image` crate) on a tiny synthetic corpus.
      Implemented as `cafe_bench::corpus` (deterministic gradient/
      checkerboard/noise generators, no `rand` dependency, formulas
      cross-checked against `old/benches/benchmark_image.rs`) and
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
      sinusoidal+hash-noise pattern ported from `old/tests/
      dictionary_regression.rs`'s `"photo"` case) and the `gen-corpus`
      binary (`cargo run -p cafe-bench --bin gen-corpus`), which writes PNGs
      under `corpus/{gradient,pixelart,texture,synthetic}/` and
      `corpus/manifest.json`. `photo/screenshot/illustration/lineart/hdr`
      remain `.gitkeep`-only and `.gitignore`d except for their `.gitkeep`,
      pending real content.
- [x] **Phase 3 — `spec/`.** `CAFE-spec.md` simplified from `old/docs/
      CAFE-spec.md`; invariants formalized in `spec/invariants/` and turned
      into automatic tests. Implemented as `spec/CAFE-spec.md` (11
      sections: overview, signature, chunk structure, defined chunks —
      `IHDR`/`iDIM`/`IDAT`/`eXIF`/`jSON`/`iCCP`/`xMPd`/`IEND` —, mandatory
      chunk order, streaming, design considerations, security, licensing,
      versioning, future extensions) plus six `spec/invariants/*.toml`
      files (`signature`, `chunks`, `ihdr`, `idim`, `predictors`,
      `security`) mirroring the normative text as machine-readable data,
      validated for internal/cross-file consistency by 9 tests in
      `crates/cafe-format/tests/spec_invariants.rs` (new `toml`
      dev-dependency). Key simplifications vs. `old/docs/CAFE-spec.md`:
      `IHDR` shrinks from 14 to 12 bytes (no `filter_method` byte — per-row
      predictor selection is the only, structural mode; no
      `interlace_method` byte — interlacing removed entirely); predictors
      reduced from 16 to 6 (None/Sub/Up/Average/Paeth/Gradient), always
      chosen per-row (never per-block, unlike the v1 lineage's dual
      per-block/per-row modes); a single `iDIM`-based tiling mechanism
      (row-major or Z-order) replaces the v1 lineage's row-strip/2D-tile/
      even-odd trichotomy; `PLTE` (indexed palette), `cHDR` (HDR metadata),
      and `zDIC` (ZSTD dictionary) chunks are dropped from the chunk set
      (deferred to 0.3/0.3/0.4 or unscheduled, per the "What v0.1 keeps,
      defers, or removes" table above — none removed permanently except
      Adam7/even-odd interlace and byte-shuffle, which this spec's section
      11 marks as permanently gone, not deferred). Security ceilings
      (`MAX_DECOMPRESSED_CHUNK_SIZE` = 1 GiB, `MAX_TILE_COUNT` = 1,048,576)
      carried forward unchanged from `old/src/constants.rs`, since their
      underlying DoS reasoning (CWE-409, CWE-789) is unaffected by the
      simplification. All workspace tests (17 total across `cafe-bench` and
      `cafe-format`), `cargo fmt --check`, and
      `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 4 — `cafe-format`.** Chunk framing, `IHDR`, critical/
      ancillary validation, hand-written golden files for the minimal
      decoder to target. Implemented as five modules: `constants` (signature
      bytes, security ceilings, `IHDR`/`iDIM` enums — mirrors
      `spec/invariants/*.toml`), `chunk` (Length/Type/Flag/Data/CRC32
      framing, adapted from `old/src/chunk.rs`'s slice-based `read_chunk`/
      `write_chunk`; the `Read`-based streaming primitive
      (`read_chunk_from`) is deferred to `cafe-codec`'s `Decoder<R>` in
      Phase 5+, since Phase 4 only needs to parse a whole in-memory file for
      golden-file tests), `signature` (9-byte magic validation), and `ihdr`
      (`Ihdr` struct with `to_payload`/`from_payload`/`to_chunk_bytes`/
      `validate`/`read_ihdr`, enforcing spec section 4.1's `width/height >
      0`, valid `sample_format`×`bit_depth` combinations, known
      `color_type`, and no reserved `compression_method` bits). Two new
      `CafeError` variants added (`InvalidIhdr`, `UnexpectedChunkType`) not
      present in the frozen v1 lineage's `old/src/error.rs`, since this
      crate validates `IHDR` content itself rather than deferring to a
      monolithic `cafe.rs`. `read_chunk` also gained an early
      `DecompressionLimitExceeded` check against a chunk's raw (still
      compressed, if `Flag=0x01`) `Length` field — stricter than
      `old/src/chunk.rs`'s slice-based path, which only bounded the
      `Read`-based streaming variant this way, not the slice-based one (the
      slice path's forged-`Length`-overruns-buffer check already prevented
      OOM in the old lineage, but bounding `Length` itself earlier is
      simpler to reason about and costs nothing). Golden fixtures
      (`golden/minimal_1x1_gray.cafe`, `golden/minimal_2x2_rgba.cafe`,
      `golden/malformed/{bad_signature,truncated_header,crc_mismatch,
      invalid_ihdr_zero_width,forged_length,wrong_first_chunk}.cafe`) are
      hand-built by an `#[ignore]`d fixture-generator test
      (`crates/cafe-format/tests/golden_files.rs`) rather than a real
      encoder (which doesn't exist until Phase 6) — 8 golden tests (2 valid
      + 6 malformed) confirm this crate's parser accepts the valid fixtures
      and rejects each malformed one with the correct `CafeError` variant.
      37 new unit tests added across the four modules (9 chunk, 4 signature,
      15 ihdr framing/validation, plus the golden-file tests), for 45 total
      `cafe-format` tests (28 lib + 8 golden + 9 spec-invariants, the latter
      unchanged from Phase 3) plus the pre-existing 8 `cafe-bench` tests —
      53 across the workspace. This crate still has zero predictor/pixel
      logic by design (per `AGENTS.md`'s "decoder before encoder" and
      "boringly simple" principles) — `IDAT` payloads are opaque bytes from
      its point of view; reversing predictors and reconstructing pixels is
      `cafe-codec`'s job (Phase 5). All workspace tests, `cargo fmt --check`,
      and `cargo clippy --all-targets -- -D warnings` pass cleanly.
- [x] **Phase 5 — `cafe-codec` decoder (scalar).** header -> chunks ->
      ZSTD decompress -> unpredict -> pixels. Validated against Phase 4's
      golden files. Implemented as five modules: `error` (new
      `CodecError` enum — `Format(cafe_format::CafeError)`,
      `InvalidPredictorCode(u8)`, `UnsupportedTiling(String)` — with
      `From<cafe_format::CafeError>`/`From<std::io::Error>`),
      `predictor` (the 6 spec predictors behind `filter_row`/
      `unfilter_row`; Paeth and Gradient formulas ported from
      `old/src/filter.rs`, Sub/Up/Average/None are direct arithmetic;
      absent neighbors at row/column 0 treated as zero per spec section
      4.3), `zstd_codec` (`compress_with_fallback`/`decompress_with_limit`/
      `decompress_chunk`, adapted from `old/src/codec.rs`'s
      `read_to_end_limited`; decompression is bounded by the caller-given
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
      zero-order byte-histogram entropy, adapted from `old/src/
      filter.rs::shannon_entropy`) plus public `predictor::
      choose_best_row_predictor(row, prev_row, bpp) -> (u8, Vec<u8>)`
      (tries all `NUM_PREDICTORS` candidates via `filter_row`, keeps the
      lowest-entropy result, ties favor the smaller code — so `PREDICTOR_NONE`
      wins whenever every candidate scores equally, e.g. a tile's first
      byte), and `tile::encode_tile_rows_auto` (the `encode_tile_rows`
      counterpart that calls `choose_best_row_predictor` independently per
      row instead of applying one fixed code to the whole tile). Only
      `Entropy`-style scoring is implemented — real compression-test/MSAD
      heuristics from `old/src/filter.rs` are deferred until `cafe-bench`
      shows they justify the extra encode cost, per `AGENTS.md`'s "every
      feature proves itself with a benchmark first". `encoder::Encoder<W>`
      mirrors the decoder's scope exactly (single whole-image tile, no
      `iDIM` emitted — multi-tile is Phase 7): `Encoder::new` validates
      `IHDR` fields via `Ihdr::validate` and records them without writing
      anything yet; `add_tile` accepts exactly one raw-pixel buffer of the
      expected `height * bytes_per_row` size; `finish` runs
      `encode_tile_rows_auto`, then `zstd_codec::compress_with_fallback`
      (or forces `FLAG_RAW` if `EncoderOptions::allow_zstd` is `false`),
      then writes `IHDR`/`IDAT`/`IEND` via `cafe_format::chunk::write_chunk`
      in spec section 5's mandatory order, and returns the writer.
      Deferring all writes to `finish` (rather than writing `IHDR` eagerly
      in `new`, as the frozen `old/src/cafe.rs::Encoder` did) is a
      deliberate Phase-6-only simplification: with only one tile ever
      accepted, every chunk can be emitted exactly once without a
      placeholder-then-patch step; true incremental writing (`IHDR` before
      pixel data is fully known) becomes relevant once Phase 7 allows more
      than one `IDAT`. A new `CodecError::EncoderMisuse(String)` variant
      covers caller misuse specifically (`add_tile` called twice, wrong
      buffer length, `finish` called before any `add_tile`) — unlike every
      other `CodecError` variant, this always indicates a programming error
      in the caller, never untrusted file input, so it's kept distinct from
      `CodecError::Format`. Confirmed against the Phase 4 golden fixtures:
      `encode_bytes` reproduces `golden/minimal_1x1_gray.cafe`
      **byte-for-byte** (single sample, no left neighbor to predict from —
      `choose_best_row_predictor` independently arrives at the same
      `PREDICTOR_NONE` the golden was hand-built with); for
      `golden/minimal_2x2_rgba.cafe`, the encoder's output differs at the
      byte level — the per-row heuristic finds `Sub` beats that golden's
      hand-picked `None` for its constant-step row 0 — so that case is
      instead asserted on decoded-pixel equality, documented in the test
      itself as a genuine improvement rather than a discrepancy to paper
      over. 24 new tests across `predictor` (10: `shannon_entropy` on
      empty/constant/uniform/varied input, `choose_best_row_predictor`
      picking None/Sub/Up correctly depending on row shape, always
      producing decoder-reversible output, and never scoring worse than
      `PREDICTOR_NONE`), `tile` (3: `encode_tile_rows_auto` round-trip,
      wrong-length rejection, and confirming per-row selection is
      independent row-to-row), and `encoder` (19: encode-then-decode
      round-trips across gray/RGBA, 8/16-bit, 1x1/2x2/16x16/64x64,
      uniform/varied content; raw-vs-ZSTD `Flag` selection in both
      directions including the `allow_zstd = false` override; the two
      golden-file comparisons above; and `EncoderMisuse`/`InvalidIhdr`
      rejection paths), for 77 total `cafe-codec` tests (up from 49) and
      122 across the workspace's format/codec/bench crates combined.
      `cafe-bench`'s `measure()` also stopped being a "PNG vs ZSTD-raw
      placeholder" and now calls the real `cafe_codec::encode_bytes`
      (new `Measurement::cafe_bytes`/`cafe_ratio`/`cafe_vs_png`, alongside
      the pre-existing PNG and raw-ZSTD-floor numbers, which are kept as a
      lower bound predictors+tiling must beat rather than removed) — this
      surfaced that CAFE already beats PNG by a wide margin on every
      synthetic pattern tried (e.g. ~0.6% of raw size vs PNG's ~2.9% on a
      64x64 gradient; ~30% vs PNG's ~101% on the `Noise` pattern, whose
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
      morton` (new — `morton_code`/`morton_decode`, naive bit-interleaving
      ported from `old/src/types.rs`, 4 tests including exhaustive 32x32
      round-trip and a Z-order block-locality property);
      `cafe_format::idim` (new — `Idim` struct, renamed from the old
      lineage's `iDim` for standard casing, with `for_image`
      (ceiling-division derivation), `validate` (nonzero fields, known
      `scan_order`, `MAX_TILE_COUNT` ceiling checked *before* the
      tiles_x/tiles_y-vs-IHDR consistency check — mirrors
      `old/src/cafe.rs::handle_idim_chunk`'s check ordering, CWE-789/
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
- [x] **Phase 8 — Fuzzing & robustness.** Port `decode_fuzz`/
      `chunk_roundtrip_fuzz`, adversarial/truncated-input tests, nightly
      fuzz CI. Implemented as a standalone `fuzz/` directory containing its
      own Cargo workspace (`fuzz/Cargo.toml`, crate `cafe-fuzz`, deliberately
      **not** a member of the root workspace — `cargo fuzz` requires
      nightly + libFuzzer's `#[no_main]` entry point, which doesn't mix
      with the root workspace's stable-toolchain `build`/`test`/`clippy`
      commands; mirrors the implicit separation `old/fuzz/` had for a
      different reason, since the old repo root wasn't a workspace at all)
      with two bins ported from `old/fuzz/fuzz_targets/`: `decode_fuzz.rs`
      (calls `cafe_codec::decode_bytes`, ignoring the result — the only
      forbidden outcome is a panic) and `chunk_roundtrip_fuzz.rs` (calls
      `cafe_format::chunk::read_chunk` directly — an improvement over the
      old lineage, where `read_chunk` was private and only reachable
      indirectly via `decode_bytes` — plus `cafe_codec::decode_bytes` for
      the same whole-file coverage `decode_fuzz` provides). Real libFuzzer
      execution requires Linux/nightly (confirmed via `cargo +nightly fuzz
      build --sanitizer none`, which compiles cleanly on Windows but fails
      to *link* with `LNK2001: unresolved external symbol
      __stop___sancov_cntrs`/`__start___sancov_pcs` — libFuzzer's coverage
      instrumentation is a known Unix-only limitation on MSVC, not a bug in
      this crate); local Windows development instead relies on two new
      proptest-based integration-test files that exercise the same
      "never panic on adversarial input" contract without needing
      libFuzzer at all: `crates/cafe-codec/tests/decode_robustness.rs` (12
      hand-written adversarial cases — empty buffer, truncated/invalid
      signature, garbage after a valid signature, forged/huge chunk
      lengths, zero-width `IHDR`, and two exhaustive sweeps against the
      Phase 4 golden fixture `golden/minimal_2x2_rgba.cafe`: every
      truncation length and every single-bit flip, confirming
      `decode_bytes` never panics across either) and
      `crates/cafe-codec/tests/roundtrip_proptest.rs` (3 proptest
      properties, new `proptest` dev-dependency added to `cafe-format` in
      addition to its pre-existing `cafe-codec` dev-dependency: arbitrary
      byte sequences up to 4 KiB never panic `decode_bytes`, the same with
      a genuine CAFE signature prefix, and a full
      `encode_bytes`/`decode_bytes` round-trip across random small
      width/height/color-type/seed/tiling combinations always reproduces
      the exact input pixels). A third new file,
      `crates/cafe-format/tests/chunk_proptest.rs` (3 properties), applies
      the same treatment one layer down, directly at `read_chunk` rather
      than the whole-file decoder: arbitrary bytes at arbitrary offsets,
      single-bit-flipped well-formed chunks, and truncated well-formed
      chunks at every length, all confirmed panic-free. CI gained two
      pieces: a `fuzz` job in `.github/workflows/ci.yml` (adapted from
      `old/.github/workflows/ci.yml`'s own `fuzz` job) running each of the
      two harnesses for a 60-second smoke test on every push/PR, and a new
      `.github/workflows/fuzz.yml` (adapted from
      `old/.github/workflows/fuzz.yml`) running each harness for a full
      hour nightly at 2 AM UTC (configurable via `workflow_dispatch`'s
      `duration_seconds` input), both uploading crash artifacts (and, for
      the nightly job, the accumulated corpus) on failure. Both workflow
      files were validated for YAML/schema correctness with `js-yaml`
      (Docker wasn't available to run `actionlint` directly in this
      environment) confirming both jobs and their step lists parse as
      intended. 18 new tests across the three new files (12
      `decode_robustness` + 3 `roundtrip_proptest` in `cafe-codec`, 3
      `chunk_proptest` in `cafe-format`), for 190 total workspace tests (up
      from 172): 105 + 12 + 3 `cafe-codec` (lib + the two new integration
      files) + 42 `cafe-format` lib + 3 `chunk_proptest` + 8 golden + 9
      spec-invariants + 8 `cafe-bench`. All workspace tests, `cargo fmt
      --all --check`, and `cargo clippy --all-targets -- -D warnings` pass
      cleanly; the standalone `fuzz/` crate was confirmed to still compile
      cleanly via `cargo +nightly check --manifest-path fuzz/Cargo.toml`
      (its own workspace isn't covered by the root's `fmt`/`clippy`
      commands, so this is checked separately, as documented in
      `fuzz/Cargo.toml`'s own comments).
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

## Reuse map (what comes from `old/`)

| New component | Source in `old/` | Treatment |
|---|---|---|
| `cafe-format::chunk` | `src/chunk.rs` | Copy framing + CRC32, adapt to the reduced chunk set |
| `cafe-format` security constants | `src/constants.rs` | Copy `MAX_DECOMPRESSED_CHUNK_SIZE`, `MAX_TILE_COUNT`, decompression budget |
| `cafe-codec::zstd` | `src/codec.rs` | Copy `compress_with_fallback`, drop the dictionary variant (returns in 0.4) |
| `cafe-codec::predictor` | `src/filter.rs` (5-6 core formulas) | Copy formulas, wrap in a new `trait Predictor` |
| Morton/scan order | `src/types.rs` (`morton_code`/`morton_decode`, `iDim::tile_order`) | Copy/adapt |
| Fuzz targets | `fuzz/fuzz_targets/*.rs` | Port, adapt to new API |
| CI workflows | `.github/workflows/{ci,fuzz}.yml` | Adapt to multi-crate workspace |
| Spec | `docs/CAFE-spec.md` | Rewrite: drop Adam7/palette/dictionary/HDR from the core, move to "future extensions" |

Everything else in `old/` (the monolithic `cafe.rs`, 16 predictors,
tonemap, k-means/median-cut, dual options structs) is reference-only —
read for context, not copied.

## Commands

```bash
cargo build                # build the whole workspace
cargo test                 # run all tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Rule about `old/`

`old/` is frozen. Do not edit files under it; it exists solely as a
reference implementation for the reboot.
