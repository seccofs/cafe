# CAFE — Compression Adaptive Filtering Experiment
## Image Format Specification

**CAFE Format Version: 0.1** (in development — see section 10 for versioning
policy)

**Author:** Daniel Secco<br/>
**Copyright** © 2026 Daniel Secco. Licensed under
[CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/) — see section 9.

---

## 1. Overview

CAFE is a chunk-based image format (inspired by PNG), using **ZSTD** as the
block compression algorithm. The core insight: don't invent another general
compressor — adaptively transform pixels (via a small set of predictors) so
that ZSTD compresses them better.

Format 0.1 is deliberately minimal. It supports uint8/uint16 and float32
samples, gray/RGB/gray+alpha/RGBA color types, an optional encoder-side
indexed-palette transform (`PLTE`) for 8-bit RGB/RGBA content, a single
unified tiling mechanism (`iDIM`), row-major or Z-order (Morton) scan,
per-row predictive filtering with 6 predictors, automatic raw-vs-compressed
fallback per chunk, and ancillary application metadata (EXIF, JSON, ICC,
XMP). Interlacing (Adam7, even/odd), ZSTD dictionaries, byte-shuffle, and
advanced HDR tone-mapping are either removed permanently or deferred to a
later, evidence-driven version (see section 11).

**Guiding principle:** the decoder defines what the format *is*, and stays
fixed and boringly simple. Every encoder capability (predictor choice, tile
size, scan order) is optional and encoder-side; the decoder needs to know
only a small, fixed set of primitives.

---

## 2. File Signature

Every `.cafe` file starts with 9 fixed bytes:

```
0x89 0x43 0x41 0x46 0x45 0x0D 0x0A 0x1A 0x0A
```

This corresponds to the sequence `\x89CAFE\r\n\x1a\n` (pure ASCII, no
accent). Same rationale as PNG's own signature:

| Bytes | Function |
|---|---|
| `0x89` | High byte — detects transmission that truncates bit 7 (7-bit text mode) |
| `0x43 0x41 0x46 0x45` (`CAFE`) | Readable mark of the format |
| `0x0D 0x0A` (`\r\n`) | Detects corruption from CRLF↔LF conversion |
| `0x1A` | Ctrl-Z — interrupts file type display on Windows/DOS |
| `0x0A` (`\n`) | Detects corruption from LF↔CRLF conversion (inverse of `\r\n`) |

---

## 3. Chunk Structure

Every chunk follows this layout:

| Field | Size | Description |
|---|---|---|
| Length | 4 bytes (uint32 BE) | Size of `Data` field |
| Type | 4 bytes (ASCII) | Chunk identifier (e.g., `IHDR`) |
| Flag | 1 byte | Codec used in this chunk — see enum in section 3.2 |
| Data | N bytes | Chunk content (raw or compressed according to Flag) |
| CRC32 | 4 bytes | CRC32 over `Type + Flag + Data` |

### 3.1 Type Naming Convention (critical vs. ancillary)

The `Type` field must contain exactly 4 alphabetic ASCII characters
(`A`–`Z`, `a`–`z`). Following the PNG convention:

- **1st letter uppercase** → critical chunk (decoder must understand or
  reject the file)
- **1st letter lowercase** → ancillary chunk (decoder can safely ignore if
  not recognized, or silently discard if malformed — see section 8.4)

### 3.2 `Flag` Field Enum and Compression Fallback Rule

| Value | Meaning |
|---|---|
| `0x00` | Raw data (no compression) |
| `0x01` | Compressed with ZSTD |
| `0x02`–`0xFF` | Reserved for future compression algorithms |

**Encoding logic (applicable to any compressible chunk):**

```
candidates = [
    (0x00, original_chunk_data),
    (0x01, zstd.compress(original_chunk_data, level)),
]

Flag, Data = smallest(candidates, key=size_in_bytes)
```

The `0x00` (raw) candidate always competes — if compression does not
produce a `Data` smaller than the original size, the chunk is written
uncompressed.

**Interoperability note:** the `Data` field of a chunk with `Flag = 0x01` is
a valid ZSTD frame, but the spec does not require that frame to declare its
decompressed size in the header. A decoder should use a streaming
decompression API rather than one that requires the size up front.

**Security note:** every decoder must enforce a configurable upper limit on
the output size of any decompression (section 8.2). This is part of the
safe decoding contract, not optional.

---

## 4. Defined Chunks

### 4.1 `IHDR` (critical, always first, always uncompressed)

| Field | Size | Description |
|---|---|---|
| Width | 4 bytes | uint32 BE, must be `> 0` |
| Height | 4 bytes | uint32 BE, must be `> 0` |
| Bit depth | 1 byte | `8` or `16` (uint), `32` (float) |
| Sample format | 1 byte | `0`=uint, `1`=float32 |
| Color type | 1 byte | `0`=gray, `2`=RGB, `4`=gray+alpha, `6`=RGBA (**default**) |
| Compression method | 1 byte | Bitmask of codecs used in the file — `bit0`=ZSTD, remaining bits reserved (0) |

**Total: 12 bytes of payload in IHDR.**

**Valid `sample_format`/`bit_depth` combinations:**

| Sample format | Allowed bit depth |
|---|---|
| `0` (uint) | `8`, `16` |
| `1` (float32) | `32` only |

`sample_format = 1` with any `bit_depth` other than `32` is invalid — the
decoder must reject the file. No half-float (`fp16`) support in 0.1 (see
section 11).

**Channels per color type:** `0`=1 (gray), `2`=3 (RGB), `4`=2
(gray+alpha), `6`=4 (RGBA). Channel order within a pixel: RGBA stores
R,G,B,A; RGB stores R,G,B; gray+alpha stores Gray,Alpha; gray stores only
the gray channel. Value `3` is reserved and unused in 0.1 — there is no
separate *indexed color type*; indexed color is instead an optional
encoder-side transform of `IDAT`'s payload shape, described in the `PLTE`
chunk (section 4.3) below, that leaves `color_type` itself always set to
the image's real, final color model (`2`=RGB or `6`=RGBA).

**Design note:** `IHDR` has **no `Filter method` field** and **no
`Interlace method` field**. Filtering is always per-row and always active
structurally (section 4.4.1 — a row can still opt out individually via
predictor code `0`, "None"), so there is nothing left for a separate
"filter method" enum to select between. Interlacing (Adam7, even/odd) is
removed entirely (not deferred — see `AGENTS.md`'s rationale: not
streamable / high complexity / low benefit vs. tiles). This keeps `IHDR`
at 12 bytes.

**`Compression method` semantics (normative):** this bitmask is a
**capability declaration** — a required lower bound on which codecs a
decoder must support to have any chance of decoding the file — not a
per-chunk record of what codec was actually used. That role belongs
exclusively to each chunk's own `Flag` byte (section 3.2), which a decoder
must always dispatch decompression from. An encoder must never emit
`bit0 = 0` while any chunk in the file has `Flag = 0x01`; declaring
`bit0 = 1` when no chunk ends up needing ZSTD is always a safe
overestimate.

**Endianness:** any sample with more than 8 bits (`bit_depth = 16, 32`) is
stored **big-endian**, consistent with all other multi-byte fields in the
format (`Width`, `Height`, `Length`, `CRC32`).

**Line order:** line `0` is the top line of the image/tile; lines proceed
top-to-bottom.

### 4.2 `iDIM` (ancillary, optional — tiling and scan order)

| Field | Size | Description |
|---|---|---|
| Tile width | 2 bytes | Tile width in pixels |
| Tile height | 2 bytes | Tile height in pixels |
| Tiles X | 2 bytes | Number of tiles horizontally |
| Tiles Y | 2 bytes | Number of tiles vertically |
| Scan order | 1 byte | `0`=row-major, `1`=Z-order (Morton) |

If absent, the decoder assumes a single `IDAT` covering the entire image
(one tile, no partitioning). `iDIM` is the **only** tiling mechanism in
0.1 — there is no separate row-strip vs. 2D-tile vs. even/odd API
distinction; a single tile that happens to be one row tall is simply a
degenerate case of the same grid.

**Recommended tile sizes:** 32×32, 64×64 (default), or 128×128. Encoders
may choose other sizes; these three are recommendations, not a decoder
requirement.

**Order of `IDAT` appearance:** tiles appear in the file in the order
defined by `Scan order`, with no explicit index needed per chunk:

- `Scan order = 0` (row-major): left→right within each tile row, then
  top→bottom.
- `Scan order = 1` (Z-order/Morton): tiles ordered by the Morton code
  (bits of `tile_x`/`tile_y` interleaved) of their `(tile_x, tile_y)` grid
  position.

The N-th `IDAT` in the file (0-indexed, counting only `IDAT` chunks)
corresponds to the N-th position in this enumeration order.

**Edge tiles:** no padding. When `Width`/`Height` are not exact multiples
of `Tile width`/`Tile height`, the last column/row of tiles has reduced
actual dimensions, computed by the decoder from `IHDR` + `iDIM` — no
additional per-tile field is needed.

**Security note (tile count ceiling):** `Tiles X × Tiles Y` has no
inherent ceiling from IHDR/iDIM consistency checks alone. A decoder must
enforce a finite upper bound on this product (`MAX_TILE_COUNT`, see
section 8.2 and `spec/invariants/idim.toml`) and reject the file before
computing tile order or allocating anything proportional to tile count.

### 4.3 `PLTE` (critical, optional, single instance — indexed-color transform)

An optional lookup table of colors, only meaningful together with a
matching change to how `IDAT` payloads are shaped (section 4.4 below).
Unlike PNG's `PLTE`, this chunk never introduces a new `color_type` value:
`IHDR.color_type` always stays the image's real, final color model (`2`
RGB or `6` RGBA — `PLTE` is undefined for `color_type` `0`/`4`, and a
decoder must reject a file combining them). `PLTE` is critical (uppercase)
because, when present, it is not optional context a decoder can safely
ignore: without it, the sample bytes in every `IDAT` cannot be correctly
interpreted at all (they are palette indices, not color channels) — this
differs from PNG, where `PLTE` is only critical for `color_type = 3`
specifically.

| Field | Size | Description |
|---|---|---|
| Entry count | 2 bytes | uint16 BE, number of palette entries, `1..=256` |
| Entries | `entry_count × bytes_per_entry` | One entry per palette index, `0`-based |

Each entry is `channels_for_color_type(IHDR.color_type)` bytes: 3 bytes
(R,G,B) when `IHDR.color_type = 2`, or 4 bytes (R,G,B,A) when
`IHDR.color_type = 6` — always `bit_depth = 8` per entry regardless of
`IHDR.bit_depth` (section 8.2's rationale: an index only ever needs to
select among at most 256 final colors, so paletted content is restricted
to `IHDR.bit_depth = 8`; 16-bit/float32 paletted images are not supported
in 0.1). There is no separate transparency (`tRNS`-equivalent) chunk —
per-entry alpha is already covered by using `color_type = 6` entries
directly, keeping this a single chunk rather than PNG's two-chunk
`PLTE`+`tRNS` split.

**Effect on `IDAT` (normative, see section 4.4):** when `PLTE` is present,
every `IDAT`'s per-pixel payload is exactly one byte (a palette index,
`0..entry_count`) instead of `bpp` bytes of direct color channels —
`bpp` for predictor/tiling purposes becomes `1`, regardless of
`IHDR.color_type`'s real channel count. A decoder reconstructs the final
`channels_for_color_type(IHDR.color_type)`-channel pixel buffer by
looking up each decoded index in the table. This is the *only* structural
effect `PLTE` has: the predictor, tiling, and chunk-framing machinery
never change shape or gain a palette-specific branch — they operate on
whatever `bpp` currently is (`1`, when `PLTE` is present; the direct
per-color-type value otherwise), the same way they already adapt `bpp`
per `bit_depth`/`color_type` combination.

**Order:** `PLTE` must appear after `IHDR`/`iDIM` (if present) and before
the first `IDAT` (section 5). A file may contain at most one `PLTE`; a
decoder finding a second instance must reject the file.

**Validation (normative):** a decoder must reject the file if any of the
following hold: `entry_count = 0`; `IHDR.color_type` is `0` or `4`
(`PLTE` is undefined for gray/gray+alpha); `IHDR.bit_depth != 8`; any
`IDAT` byte, once split into indices, is `>= entry_count` (an
out-of-range index, section 8.1's "decoders must never panic on untrusted
input" — indexing the palette table with it must be bounds-checked, not
assumed valid).

**Choosing to use `PLTE` is entirely an encoder-side decision** (spec
section 4.4.1's predictor-selection precedent applies equally here): an
encoder may inspect an image's distinct-color count and only emit `PLTE`
when it helps (`cafe-bench` empirically found 256-or-fewer-distinct-color
RGB/RGBA content compresses 1.4-1.5x smaller this way — see `AGENTS.md`).
The decoder's only obligation is correctly reversing whichever choice the
encoder made, per section 1's "decoder is fixed" principle.

### 4.4 `IDAT` (critical, one or more per file)

Contains the pixels of one tile. Each `IDAT` is independent — it can be
compressed or not (fallback rule, section 3.2), and decoded as soon as it
arrives (streaming).

**Payload before compression (always this shape — there is no alternate
"whole-block, single filter byte" mode):**

```
for each row of the tile, in order:
    [predictor code: 1 byte][filtered row: bytes_per_row bytes]
```

`bytes_per_row = tile_width × bpp`, where `bpp = bytes_per_sample ×
channels` (`bytes_per_sample` is `1` for `bit_depth = 8`, `2` for
`bit_depth = 16`, `4` for `bit_depth = 32`) — except when a `PLTE` chunk
(section 4.3) is present, in which case `bpp = 1` (one index byte per
pixel) regardless of `IHDR.color_type`'s real channel count.

#### 4.4.1 Predictors

Reduces data entropy **before** compression by predicting each sample byte
from already-known causal neighbors and storing only the residual. This is
a preprocessing step, not compression — it works together with ZSTD
(section 3.2), not instead of it.

**Chosen per row** (not per tile/block): each row of an `IDAT` carries its
own 1-byte predictor code, immediately before that row's filtered data.
This is structural from day one — there is no "whole-block, one predictor
for every row" mode to reconcile it against.

| Code | Predictor | Prediction used (per sample byte) |
|---|---|---|
| `0` | None | Original byte kept |
| `1` | Sub | Byte of left pixel (`L`), same row |
| `2` | Up | Byte of pixel above (`U`), same column, previous row |
| `3` | Average | `floor((L + U) / 2)` |
| `4` | Paeth | Left, above, or top-left diagonal (`UL`) — Paeth predictor |
| `5` | Gradient | `(L + U − UL) mod 256`, no clamping |

**Paeth predictor** (identical to PNG's):

```
p = L + U - UL
pL = |p - L|; pU = |p - U|; pUL = |p - UL|
if pL <= pU and pL <= pUL: prediction = L
else if pU <= pUL:         prediction = U
else:                      prediction = UL
```

All predictors compute the final residual (`original_byte − prediction`)
and reverse it (`residual + prediction`) using integer arithmetic with
modulo-256 wraparound (`u8` wrapping), identical between encoding and
decoding.

**Tile edges:** since each `IDAT` is independent, the "above" neighbor
only exists if the row is not the first of the tile — in the first row of
each tile, the predictor treats the above neighbor as zero. Likewise, the
"left" neighbor is zero for the first column, and the diagonal (`UL`)
neighbor is zero whenever either its row or column falls outside the tile.
This is the same zero-neighbor convention PNG uses for the first line of
the whole image, applied per tile here for streaming independence.

**Bytes per pixel (`bpp`)**, used to locate the left neighbor:
`bpp = bytes_per_sample × channels` (section 4.4), or `bpp = 1` when a
`PLTE` chunk is present (section 4.3). Minimum `bpp = 1`.

**Selection heuristic is not part of the decoding contract:** how an
encoder chooses which of the 6 predictor codes to use for a given row is
entirely an encoder-side decision (e.g. sum of absolute residuals, Shannon
entropy, or a real compression test) — the decoder only ever reverses
whichever code is actually written.

### 4.5 `eXIF` (ancillary, optional, single instance)

Stores EXIF metadata (camera, capture date, geolocation, orientation,
etc.) in complete TIFF format, exactly as the Exif specification defines.

| Field | Size | Description |
|---|---|---|
| Payload | rest of `Data` | Raw EXIF blob, complete TIFF format including its own byte-order header |

CAFE does not interpret or transform this content — it is stored as an
opaque blob. Single instance per file (a decoder finding more than one
must consider only the first). Recommended position: before the first
`IDAT`.

### 4.6 `jSON` (ancillary, optional, multiple instances allowed)

Stores arbitrary application/user metadata in JSON format, namespaced to
avoid collisions between sources.

| Field | Size | Description |
|---|---|---|
| Namespace length | 1 byte | Size of Namespace field |
| Namespace | N bytes | ASCII string (e.g., `"app.editor"`, `"user"`) |
| JSON payload | rest of `Data` | Valid UTF-8 text, free structure |

Multiple `jSON` instances are allowed, each with its own namespace.
A malformed `jSON` chunk (inconsistent namespace length, or invalid JSON)
must not invalidate the file — the decoder discards only that chunk
(section 8.4).

### 4.7 `iCCP` (ancillary, optional, single instance)

Stores an ICC color management profile.

| Field | Size | Description |
|---|---|---|
| Payload | rest of `Data` | Raw ICC profile (binary), opaque to CAFE |

**Default color space in the absence of `iCCP`:** all CAFE RGB values must
be interpreted as **sRGB (IEC 61966-2-1)**. A decoder not implementing
`iCCP` is always correct treating colors as sRGB.

### 4.8 `xMPd` (ancillary, optional, single instance)

Stores metadata in XMP format (Adobe/ISO 16684-1).

| Field | Size | Description |
|---|---|---|
| Payload | rest of `Data` | Valid UTF-8 XML, per XMP specification |

Applications are expected to choose one metadata mechanism per data type
(EXIF for capture data, XMP for editorial flow, JSON for
application-proprietary data) — CAFE does not mandate which.

### 4.9 `IEND` (critical, marks end of file)

`Length = 0`. No `Data`.

---

## 5. Mandatory Chunk Order

```
Signature (9 bytes)
IHDR                  (mandatory, first)
iDIM                  (optional)
eXIF                  (optional, single instance)
jSON (zero or more)   (optional, one per namespace)
iCCP                  (optional, single instance)
xMPd                  (optional, single instance)
PLTE                  (optional, single instance — section 4.3)
IDAT (one or more)    (mandatory, in scan order — section 4.2)
IEND                  (mandatory, last)
```

---

## 6. Streaming

Requirements for incremental decoding:

1. `IHDR` is always first, small, and uncompressed — dimensions and format
   are available immediately.
2. `iDIM`, if present, informs the tile scheme before any `IDAT`.
3. Each `IDAT` is self-contained and can be decoded as soon as it arrives,
   without waiting for others.
4. Combining `scan_order = 1` (Z-order) with small tiles gives a
   progressive-by-region loading experience without needing interlacing.

---

## 7. Design Considerations

- **Fallback per chunk** (not per file) avoids compression overhead on
  high-entropy blocks.
- **CRC per chunk** detects corruption without decompressing the entire
  file.
- **Critical/ancillary convention** allows adding new chunks without
  breaking old decoders.
- **Per-row predictor selection** trades 1 extra header byte per row for
  finer-grained adaptation to local content changes within a tile than a
  single per-tile predictor choice would allow — chosen as the *only* mode
  from day one, rather than a per-block mode needing reconciliation later.
- Tile size is a trade-off between streaming granularity, per-chunk
  framing/CRC overhead, and predictor efficiency (each tile restarts
  prediction on its first row). See `cafe-bench` for empirical
  measurements once `cafe-codec` exists (Phase 6+).

---

## 8. Security Considerations

A CAFE decoder processes **untrusted** data by definition.

### 8.1 General principle: decoders must never panic on untrusted input

Every size, count, or offset field read from a `.cafe` file is
attacker-controlled. A correct decoder validates these fields before using
them to index memory, allocate buffers, or divide values, returning a
handleable error for any malformed input — including truncated files,
forged `Length` fields, undersized critical chunks, degenerate dimensions
(`Width = 0` or `Height = 0`), and inconsistency between `IHDR` and actual
pixel data.

### 8.2 Protection against "decompression bomb" (CWE-409)

Every decoder must impose a configurable upper limit on the output size of
any single decompression operation, rejecting decompression as soon as the
limit is exceeded — without ever attempting to allocate or materialize
content beyond the limit before checking. Default:
`MAX_DECOMPRESSED_CHUNK_SIZE = 1 GiB` (see `spec/invariants/security.toml`).

**Tile-count ceiling:** `iDIM`'s `Tiles X × Tiles Y` must be bounded
(`MAX_TILE_COUNT = 1,048,576`) and checked *before* computing tile order or
allocating anything proportional to it — a ~15-byte crafted `iDIM` chunk
alone (no `IDAT` needed) can otherwise trigger a multi-gigabyte allocation
attempt purely from `Tiles X = Tiles Y = 65535`.

### 8.3 Absence of upper limit for `Width`/`Height`

Intentionally, there is no maximum. Decoders must reconstruct
incrementally from actual `IDAT` data, not pre-allocate
`Width × Height × bytes_per_pixel` before validation.

### 8.4 Malformed ancillary chunks never invalidate the file or panic

An ancillary chunk (`eXIF`, `jSON`, `iCCP`, `xMPd`) with malformed content
must be silently discarded by the decoder (or reported as a non-fatal
warning) and must never interrupt image decoding or cause a panic. The
only exception is when the chunk is structurally impossible to delimit
(e.g. `Length` exceeding the file) — that is a framing error (section 8.1),
handled at the chunk-parser level, not the content-parser level.

---

## 9. Licensing

- **Text of this specification:** © 2026 Daniel Secco. Licensed under
  [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- **Reference implementation (code):** BSD-3-Clause.

---

## 10. Versioning

Format version is tracked as `MAJOR.MINOR`, independent of any crate's own
SemVer. During the `0.x` line, **breaking changes may occur between minor
versions** — the format is not yet stabilized, and `golden/` (plus
`spec/invariants/`) exist precisely to catch unintentional breakage as the
format solidifies. Once declared `1.0`, the discipline becomes: `MAJOR` bumps
only for breaking changes, `MINOR` bumps only for backward-compatible
normative extensions.

There is no version field inside `IHDR`, for the same reason PNG has
none: decades of stability without one, and the chicken-and-egg problem
of a version field describing compatibility for a decoder that doesn't
yet know to look for it. The critical/ancillary naming convention
(section 3.1) and per-field enum validation already carry format
evolution, as they do in PNG.

---

## 11. Future Extensions (deferred, not part of 0.1)

These are documented here only as pointers — none are normative for
Format 0.1, and none should be implemented until a benchmark on
`cafe-bench` justifies them (per `AGENTS.md`'s guiding principles). See
`AGENTS.md`'s "Core v0.1 design decisions" table for the full rationale
per item, including two implementation-only items (SIMD, and `PLTE`
itself) that have since shipped and are therefore no longer listed below
— neither changed the on-disk format shape: SIMD is byte-identical to the
scalar reference, and `PLTE` was already normatively defined in this
section's chunk set from Format 0.1's first draft.

| Feature | Target version | Notes |
|---|---|---|
| Palette quantization algorithms | 0.3+ | Median-cut / k-means / redmean, encoder-only — `PLTE` itself (section 4.3) is implemented; only picking a palette for images with *more* than 256 distinct colors (quantization) remains deferred |
| ZSTD dictionary (external, then embedded) | 0.4 | Conflicts with single-pass streaming until designed carefully |
| HDR (fp16, PQ/HLG/tonemap) | Unscheduled | No design work started |

**Permanently removed** (not deferred — see `AGENTS.md`): Adam7 interlace,
even/odd interlace, byte-shuffle, per-block (as opposed to per-row) filter
selection, dual `EncodeOptions`/`EncoderOptions` structs, three parallel
tiling APIs.

---

*This document is normative for CAFE Format 0.1. Machine-readable
invariants derived from it live in `spec/invariants/`; golden test vectors
live in `golden/`.*
