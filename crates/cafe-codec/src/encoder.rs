//! Streaming encoder (Phase 6-7, see `AGENTS.md`): pixels -> predict ->
//! ZSTD compress (with raw fallback) -> chunks -> `.cafe` bytes.
//!
//! Supports both the implicit single-tile case (`tile_size: None` — one
//! `IDAT` covering the whole image, no `iDIM` emitted) and full 2D tiling
//! (`tile_size: Some((tw, th))` — an `iDIM` chunk plus one `IDAT` per tile,
//! in `scan_order`), mirroring [`crate::decoder`]'s scope via the same
//! [`crate::tiling::TileLayout`] abstraction. `Encoder<W>` is the single
//! streaming-first API `AGENTS.md` calls for; [`encode_bytes`] is sugar
//! over `Encoder::new(...).add_tile(...)*.finish()` for the common
//! whole-buffer-in, whole-buffer-out case.
//!
//! Unlike Phase 6, `IHDR` (and `iDIM`, if any) are written eagerly in
//! [`Encoder::new`]: both are fully determined by `width`/`height`/tiling
//! options alone, with no pixel data needed, so there is no more reason to
//! defer them to `finish` now that more than one `IDAT` is possible.
//! [`Encoder::add_tile`] writes that tile's `IDAT` immediately, in true
//! streaming fashion — the encoder never buffers more than one tile's
//! pixels at a time.

use crate::error::{CodecError, Result};
use crate::tile::encode_tile_rows_auto;
use crate::tiling::TileLayout;
use crate::zstd_codec::{compress_with_fallback, FLAG_RAW};
use cafe_format::chunk::write_chunk;
use cafe_format::constants::{COMPRESSION_METHOD_ZSTD_BIT, SCAN_ORDER_ROW_MAJOR, SIGNATURE};
use cafe_format::ihdr::Ihdr;
use cafe_format::{Idim, JsonChunk, Plte, Xmpd};
use std::io::Write;

/// Encoder-side knobs (spec section 3.2's fallback rule, section 4.1's
/// `compression_method` capability bit, and section 4.2's tiling/scan
/// order are the things an encoder gets to decide — predictor choice is
/// already fixed to the per-row entropy heuristic in
/// [`crate::predictor::choose_best_row_predictor`]).
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderOptions {
    /// ZSTD compression level passed to [`compress_with_fallback`]. Higher
    /// is slower but usually smaller; `19` matches the level `cafe-bench`
    /// and this crate's other ZSTD tests already use as a reasonable
    /// default.
    pub level: i32,
    /// Whether this encoder may ever choose ZSTD for its `IDAT`. When
    /// `true`, `IHDR`'s `compression_method` declares the ZSTD capability
    /// bit up front (spec section 4.1: "declaring bit0=1 when no chunk
    /// ends up needing ZSTD is always a safe overestimate") and the actual
    /// per-tile fallback race decides `Flag` normally. When `false`, the
    /// fallback race is skipped entirely and every `IDAT` is written raw
    /// (`Flag = 0x00`) — required, since the spec forbids emitting
    /// `compression_method` bit0=0 while any chunk has `Flag = 0x01`.
    pub allow_zstd: bool,
    /// Tile size for `iDIM` (spec section 4.2). `None` (default) means no
    /// `iDIM` chunk is emitted and the whole image is a single implicit
    /// tile. `Some((tile_width, tile_height))` splits the image into a
    /// grid via [`Idim::for_image`] and emits an `iDIM` chunk — both
    /// components must be nonzero.
    pub tile_size: Option<(u16, u16)>,
    /// Scan order used to sequence `IDAT`s when `tile_size` is `Some`
    /// (spec section 4.2: `0`=row-major, `1`=Z-order). Ignored when
    /// `tile_size` is `None`.
    pub scan_order: u8,
    /// Indexed-color palette entries (spec section 4.3), flat bytes
    /// (`bytes_per_entry` per entry, matching `color_type`: `3` for RGB,
    /// `4` for RGBA). `None` (default) means no `PLTE` chunk is emitted
    /// and `add_tile`/`encode_bytes` expect direct pixel bytes as usual.
    ///
    /// `Some(entries)` is the caller's declaration "the pixels I'm about
    /// to hand this encoder are already palette *indices*, one byte per
    /// pixel, not direct channel bytes" (per `AGENTS.md`'s API decision:
    /// this encoder never quantizes colors itself — see
    /// [`crate::palette::build_palette`] for the helper that computes both
    /// `entries` and the index buffer to pass as `add_tile`/`encode_bytes`'
    /// pixel data from a real direct-color image).
    pub palette: Option<Vec<u8>>,
    /// Raw EXIF TIFF blob to embed as an `eXIF` chunk (spec section 4.5).
    /// `None` (default) omits the chunk entirely. This encoder never
    /// inspects or validates EXIF content — it's an opaque blob as far as
    /// `cafe-format`/`cafe-codec` are concerned.
    pub exif: Option<Vec<u8>>,
    /// `jSON` chunks to embed (spec section 4.6), in the order they should
    /// appear in the file. Empty (default) omits the chunk type entirely.
    /// Unlike every other metadata field here, more than one is allowed —
    /// `jSON` is the only repeatable metadata chunk type. Each
    /// [`JsonChunk`] is already validated (ASCII namespace, well-formed
    /// JSON payload) by its own constructor before it ever reaches this
    /// struct.
    pub json_chunks: Vec<JsonChunk>,
    /// Raw ICC color profile bytes to embed as an `iCCP` chunk (spec
    /// section 4.7). `None` (default) omits the chunk, meaning the
    /// default color space applies: sRGB (IEC 61966-2-1). This encoder
    /// never inspects or validates ICC content.
    pub icc_profile: Option<Vec<u8>>,
    /// XMP metadata (raw XML text) to embed as an `xMPd` chunk (spec
    /// section 4.8). `None` (default) omits the chunk.
    pub xmp: Option<String>,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            level: 19,
            allow_zstd: true,
            tile_size: None,
            scan_order: SCAN_ORDER_ROW_MAJOR,
            palette: None,
            exif: None,
            json_chunks: Vec::new(),
            icc_profile: None,
            xmp: None,
        }
    }
}

/// Streaming encoder: [`Encoder::new`] validates the header (and tiling)
/// fields and immediately writes `IHDR` (and `iDIM`, if tiling is
/// configured); [`Encoder::add_tile`] accepts one tile's raw pixel bytes
/// at a time, in the layout's scan order, writing its `IDAT` immediately;
/// [`Encoder::finish`] writes `IEND` and returns the underlying writer.
pub struct Encoder<W: Write> {
    writer: W,
    bpp: u32,
    options: EncoderOptions,
    layout: TileLayout,
    next_tile_index: usize,
}

impl<W: Write> Encoder<W> {
    /// Validates `width`/`height`/`bit_depth`/`sample_format`/`color_type`
    /// (via [`Ihdr::validate`]) and, if `options.tile_size` is `Some`,
    /// the derived `iDIM` geometry (via [`Idim::validate`], including its
    /// `MAX_TILE_COUNT` ceiling) — then writes the CAFE signature, `IHDR`,
    /// and (if configured) `iDIM` to `writer` before returning.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut writer: W,
        width: u32,
        height: u32,
        bit_depth: u8,
        sample_format: u8,
        color_type: u8,
        options: EncoderOptions,
    ) -> Result<Self> {
        let compression_method = if options.allow_zstd {
            COMPRESSION_METHOD_ZSTD_BIT
        } else {
            0
        };
        let ihdr = Ihdr {
            width,
            height,
            bit_depth,
            sample_format,
            color_type,
            compression_method,
        };
        ihdr.validate()?;

        let direct_bpp = ihdr.bytes_per_pixel().ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::InvalidIhdr(format!(
                "no channel count for color_type={}",
                ihdr.color_type
            )))
        })?;

        // When a palette is configured, IDAT holds one index byte per
        // pixel (spec section 4.3), regardless of color_type's real
        // channel count — `direct_bpp` is still needed above/below for
        // PLTE's own entry-size validation.
        let plte = match &options.palette {
            None => None,
            Some(entries) => {
                let parsed = Plte::from_colors(color_type, entries)?;
                parsed.validate(color_type, bit_depth)?;
                Some(parsed)
            }
        };
        let bpp = if plte.is_some() { 1 } else { direct_bpp };

        let idim = match options.tile_size {
            None => None,
            Some((tile_width, tile_height)) => {
                if tile_width == 0 || tile_height == 0 {
                    return Err(CodecError::EncoderMisuse(
                        "EncoderOptions::tile_size components must be nonzero".into(),
                    ));
                }
                let idim =
                    Idim::for_image(tile_width, tile_height, width, height, options.scan_order);
                idim.validate(width, height)?;
                Some(idim)
            }
        };
        let layout = TileLayout::new(idim, width, height)?;

        writer.write_all(&SIGNATURE)?;
        writer.write_all(&ihdr.to_chunk_bytes())?;
        if let Some(idim) = &idim {
            writer.write_all(&idim.to_chunk_bytes())?;
        }
        // Metadata chunks, in spec section 5's mandatory order: eXIF ->
        // jSON -> iCCP -> xMPd, all before PLTE/IDAT. All four are fully
        // determined by `options` alone (no pixel data needed), so — like
        // IHDR/iDIM above — they're written eagerly here rather than
        // deferred to `add_tile`/`finish`. Each payload races raw vs. ZSTD
        // exactly like IDAT does (spec section 3.2: the raw-vs-compressed
        // fallback applies to any compressible chunk, not just IDAT) —
        // `JsonChunk`/`Xmpd::to_chunk_bytes()` always emit `Flag = 0x00`
        // by design (`cafe-format` doesn't make compression decisions), so
        // this encoder calls `to_payload()` and does the race itself,
        // respecting `options.allow_zstd` the same way `add_tile` does
        // (compression_method's bit0 must stay 0 if nothing ever sets
        // `Flag = 0x01`, spec section 4.1).
        let write_metadata_chunk =
            |writer: &mut W, chunk_type: &[u8; 4], payload: &[u8]| -> Result<()> {
                let (flag, data) = if options.allow_zstd {
                    compress_with_fallback(payload, options.level)?
                } else {
                    (FLAG_RAW, payload.to_vec())
                };
                writer.write_all(&write_chunk(chunk_type, flag, &data))?;
                Ok(())
            };
        if let Some(exif) = &options.exif {
            write_metadata_chunk(&mut writer, b"eXIF", exif)?;
        }
        for json_chunk in &options.json_chunks {
            write_metadata_chunk(&mut writer, b"jSON", &json_chunk.to_payload())?;
        }
        if let Some(icc_profile) = &options.icc_profile {
            write_metadata_chunk(&mut writer, b"iCCP", icc_profile)?;
        }
        if let Some(xmp) = &options.xmp {
            write_metadata_chunk(&mut writer, b"xMPd", &Xmpd::new(xmp).to_payload())?;
        }
        if let Some(plte) = &plte {
            writer.write_all(&plte.to_chunk_bytes())?;
        }

        Ok(Self {
            writer,
            bpp,
            options,
            layout,
            next_tile_index: 0,
        })
    }

    /// Accepts the next tile's raw pixel bytes (row-major, top-to-bottom,
    /// `tile_height * tile_width * bpp` bytes — using *this* tile's real
    /// dimensions, which may be smaller than the nominal tile size at the
    /// right/bottom edges, spec section 4.2) in the layout's scan order,
    /// and writes its `IDAT` chunk immediately.
    ///
    /// Returns [`CodecError::EncoderMisuse`] if called more times than the
    /// layout has tiles, or if `raw.len()` doesn't match the expected size
    /// for the next tile.
    pub fn add_tile(&mut self, raw: &[u8]) -> Result<()> {
        if self.next_tile_index >= self.layout.tile_count() {
            return Err(CodecError::EncoderMisuse(format!(
                "add_tile called more times ({}) than the layout has tiles ({})",
                self.next_tile_index + 1,
                self.layout.tile_count()
            )));
        }

        let (_, _, tile_w, tile_h) = self.layout.tile_rect(self.next_tile_index);
        let tile_bytes_per_row = tile_w.checked_mul(self.bpp).ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing tile bytes_per_row (tile_width * bpp)".into(),
            ))
        })?;
        let expected_len = (tile_h as usize)
            .checked_mul(tile_bytes_per_row as usize)
            .ok_or_else(|| {
                CodecError::Format(cafe_format::CafeError::TruncatedFile(
                    "overflow computing expected tile length (tile_height * bytes_per_row)".into(),
                ))
            })?;
        if raw.len() != expected_len {
            return Err(CodecError::EncoderMisuse(format!(
                "add_tile: tile {} expected {expected_len} bytes (tile_height={tile_h} * \
                 bytes_per_row={tile_bytes_per_row}), got {}",
                self.next_tile_index,
                raw.len()
            )));
        }

        let chunk_bytes = encode_tile_chunk(
            raw,
            tile_h,
            tile_bytes_per_row,
            self.bpp,
            self.options.level,
            self.options.allow_zstd,
        )?;
        self.writer.write_all(&chunk_bytes)?;
        self.next_tile_index += 1;
        Ok(())
    }

    /// Writes `IEND` (spec section 5's mandatory last chunk) and returns
    /// the underlying writer.
    ///
    /// Returns [`CodecError::EncoderMisuse`] if fewer tiles were added than
    /// the layout requires — every CAFE file needs at least one `IDAT`
    /// (spec section 4.4), and a partially-tiled file would silently
    /// decode as truncated.
    pub fn finish(mut self) -> Result<W> {
        if self.next_tile_index != self.layout.tile_count() {
            return Err(CodecError::EncoderMisuse(format!(
                "finish called after only {} of {} tile(s) were added via add_tile",
                self.next_tile_index,
                self.layout.tile_count()
            )));
        }
        self.writer.write_all(&write_chunk(b"IEND", 0x00, b""))?;
        Ok(self.writer)
    }
}

/// Extracts one tile's raw pixel bytes (row-major) out of a full image's
/// raw pixel buffer, given the tile's pixel-space origin/size and the full
/// image's `bytes_per_row`/`bpp`. Used by [`encode_bytes`] to split a
/// whole-image buffer into per-tile calls to [`Encoder::add_tile`].
fn extract_tile_pixels(
    full: &[u8],
    origin_x: u32,
    origin_y: u32,
    tile_w: u32,
    tile_h: u32,
    full_bytes_per_row: u32,
    bpp: u32,
) -> Vec<u8> {
    let row_bytes = (tile_w * bpp) as usize;
    let mut out = Vec::with_capacity(row_bytes * tile_h as usize);
    for r in 0..tile_h {
        let src_start = ((origin_y + r) * full_bytes_per_row + origin_x * bpp) as usize;
        out.extend_from_slice(&full[src_start..src_start + row_bytes]);
    }
    out
}

/// Encodes one tile's raw pixel bytes into a complete, ready-to-write
/// `IDAT` chunk (predictor selection + raw-vs-ZSTD fallback race + chunk
/// framing) — the exact per-tile work [`Encoder::add_tile`] does, factored
/// out so [`encode_bytes_parallel`] can run it concurrently across tiles
/// without duplicating [`Encoder::add_tile`]'s logic (or the two drifting
/// apart over time).
fn encode_tile_chunk(
    raw: &[u8],
    tile_h: u32,
    tile_bytes_per_row: u32,
    bpp: u32,
    level: i32,
    allow_zstd: bool,
) -> Result<Vec<u8>> {
    let payload = encode_tile_rows_auto(raw, tile_h, tile_bytes_per_row, bpp)?;
    let (flag, data) = if allow_zstd {
        compress_with_fallback(&payload, level)?
    } else {
        (FLAG_RAW, payload)
    };
    Ok(write_chunk(b"IDAT", flag, &data))
}

/// Sugar over `Encoder::new(...).add_tile(...)*.finish()` for the common
/// case of encoding one whole in-memory image into one in-memory `.cafe`
/// buffer: `raw_pixels` is the whole image's raw pixel bytes (row-major,
/// top-to-bottom, `height * width * bpp` bytes total — the same shape
/// [`crate::decoder::DecodedImage::pixels`] produces), split into tiles
/// per `options.tile_size` and fed to the encoder in the layout's scan
/// order.
#[allow(clippy::too_many_arguments)]
pub fn encode_bytes(
    width: u32,
    height: u32,
    bit_depth: u8,
    sample_format: u8,
    color_type: u8,
    raw_pixels: &[u8],
    options: EncoderOptions,
) -> Result<Vec<u8>> {
    let mut encoder = Encoder::new(
        Vec::new(),
        width,
        height,
        bit_depth,
        sample_format,
        color_type,
        options,
    )?;

    let full_bytes_per_row = width.checked_mul(encoder.bpp).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "overflow computing bytes_per_row (width * bpp)".into(),
        ))
    })?;
    let expected_total = (height as usize)
        .checked_mul(full_bytes_per_row as usize)
        .ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing expected total pixel buffer size".into(),
            ))
        })?;
    if raw_pixels.len() != expected_total {
        return Err(CodecError::EncoderMisuse(format!(
            "encode_bytes: expected {expected_total} bytes (height={height} * \
             bytes_per_row={full_bytes_per_row}), got {}",
            raw_pixels.len()
        )));
    }

    let tile_count = encoder.layout.tile_count();
    for i in 0..tile_count {
        let (origin_x, origin_y, tile_w, tile_h) = encoder.layout.tile_rect(i);
        let tile_raw = extract_tile_pixels(
            raw_pixels,
            origin_x,
            origin_y,
            tile_w,
            tile_h,
            full_bytes_per_row,
            encoder.bpp,
        );
        encoder.add_tile(&tile_raw)?;
    }
    encoder.finish()
}

/// Parallel counterpart to [`encode_bytes`]: identical header/`iDIM`/
/// metadata/`PLTE` handling (via the same [`Encoder::new`], which still
/// runs once, sequentially, up front — none of that depends on pixel
/// data), but every tile's predictor selection + ZSTD fallback race +
/// chunk framing (the work [`encode_tile_chunk`] does) runs concurrently
/// across tiles instead of one at a time, via
/// [`crate::parallel::map_parallel`]. The resulting `IDAT` chunks are
/// then written to the output buffer sequentially, in scan order — spec
/// section 4.2's "the N-th `IDAT` in the file corresponds to the N-th
/// position in this enumeration order" is a file-structure requirement,
/// not a scheduling one, so which thread computed a chunk's bytes never
/// affects where they land in the output.
///
/// Produces byte-for-byte identical output to [`encode_bytes`] for the
/// same input and options — confirmed by this module's
/// `test_encode_bytes_parallel_matches_sequential_*` tests. Like
/// [`crate::decoder::decode_bytes_parallel`], this transparently falls
/// back to sequential execution for small tile counts or single-core
/// hosts (see `crate::parallel::MIN_TILES_FOR_PARALLEL`), so there is no
/// throughput reason to prefer [`encode_bytes`] on small images — only
/// use it to force deterministic single-threaded scheduling explicitly.
///
/// Note: this is *not* built on the streaming [`Encoder<W>`] API at all
/// (unlike [`encode_bytes`], which is sugar over
/// `Encoder::new(...).add_tile(...)*.finish()`) — [`Encoder::add_tile`]'s
/// contract writes its `IDAT` to the underlying writer immediately, which
/// is fundamentally incompatible with computing tiles out of order across
/// threads. This function instead calls [`Encoder::new`] only for its
/// header/metadata-writing side effect, extracting the tile geometry it
/// needs (`bpp`, `layout`) before computing every tile's chunk bytes via
/// [`encode_tile_chunk`] directly.
#[allow(clippy::too_many_arguments)]
pub fn encode_bytes_parallel(
    width: u32,
    height: u32,
    bit_depth: u8,
    sample_format: u8,
    color_type: u8,
    raw_pixels: &[u8],
    options: EncoderOptions,
) -> Result<Vec<u8>> {
    let level = options.level;
    let allow_zstd = options.allow_zstd;
    let encoder = Encoder::new(
        Vec::new(),
        width,
        height,
        bit_depth,
        sample_format,
        color_type,
        options,
    )?;

    let bpp = encoder.bpp;
    let full_bytes_per_row = width.checked_mul(bpp).ok_or_else(|| {
        CodecError::Format(cafe_format::CafeError::TruncatedFile(
            "overflow computing bytes_per_row (width * bpp)".into(),
        ))
    })?;
    let expected_total = (height as usize)
        .checked_mul(full_bytes_per_row as usize)
        .ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing expected total pixel buffer size".into(),
            ))
        })?;
    if raw_pixels.len() != expected_total {
        return Err(CodecError::EncoderMisuse(format!(
            "encode_bytes_parallel: expected {expected_total} bytes (height={height} * \
             bytes_per_row={full_bytes_per_row}), got {}",
            raw_pixels.len()
        )));
    }

    let tile_count = encoder.layout.tile_count();
    let layout = &encoder.layout;
    let idat_chunks = crate::parallel::map_parallel(tile_count, move |i| {
        let (origin_x, origin_y, tile_w, tile_h) = layout.tile_rect(i);
        let tile_raw = extract_tile_pixels(
            raw_pixels,
            origin_x,
            origin_y,
            tile_w,
            tile_h,
            full_bytes_per_row,
            bpp,
        );
        let tile_bytes_per_row = tile_w.checked_mul(bpp).ok_or_else(|| {
            CodecError::Format(cafe_format::CafeError::TruncatedFile(
                "overflow computing tile bytes_per_row (tile_width * bpp)".into(),
            ))
        })?;
        encode_tile_chunk(
            &tile_raw,
            tile_h,
            tile_bytes_per_row,
            bpp,
            level,
            allow_zstd,
        )
    })?;

    // `encoder.writer` already holds every byte up through
    // signature/IHDR/iDIM/metadata/PLTE (all written eagerly by
    // `Encoder::new`, none of which depends on pixel data) — append every
    // tile's precomputed `IDAT` bytes in scan order, then `IEND`,
    // completing the file exactly as `Encoder::finish` would after a
    // sequential `add_tile` loop.
    let mut writer = encoder.writer;
    for chunk_bytes in idat_chunks {
        writer.write_all(&chunk_bytes)?;
    }
    writer.write_all(&write_chunk(b"IEND", 0x00, b""))?;
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::decode_bytes;
    use cafe_format::constants::{COLOR_TYPE_GRAY, COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT};

    #[test]
    fn test_encode_then_decode_roundtrip_1x1_gray() {
        let raw = vec![0x7F];
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
        assert_eq!(img.ihdr.width, 1);
        assert_eq!(img.ihdr.height, 1);
        assert_eq!(img.ihdr.color_type, COLOR_TYPE_GRAY);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_2x2_rgba() {
        let raw = vec![
            0x10, 0x20, 0x30, 0xFF, 0x11, 0x21, 0x31, 0xFF, // row 0
            0x12, 0x22, 0x32, 0xFF, 0x13, 0x23, 0x33, 0xFF, // row 1
        ];
        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_large_compressible_image() {
        // Large + uniform, so the fallback race actually picks ZSTD, and
        // per-row predictor selection has real work to do.
        let width = 64u32;
        let height = 64u32;
        let raw = vec![42u8; (width * height) as usize];
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_varied_content_all_channels() {
        let width = 16u32;
        let height = 16u32;
        let raw: Vec<u8> = (0..(width * height * 4))
            .map(|i| ((i * 37 + 11) % 251) as u8)
            .collect();
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_uses_raw_flag_for_incompressible_tiny_image() {
        let raw = vec![0x7F];
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        // Locate the IDAT chunk's Flag byte directly: signature(9) +
        // IHDR chunk (4+4+1+12+4=25) = 34, then IDAT's Length(4)+Type(4)
        // puts Flag at offset 34+8=42.
        assert_eq!(buf[9 + 25 + 8], crate::zstd_codec::FLAG_RAW);
    }

    #[test]
    fn test_encode_picks_zstd_flag_for_large_compressible_image() {
        let width = 64u32;
        let height = 64u32;
        let raw = vec![7u8; (width * height) as usize];
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        assert_eq!(buf[9 + 25 + 8], crate::zstd_codec::FLAG_ZSTD);
    }

    #[test]
    fn test_encode_declares_zstd_capability_bit_when_allowed() {
        let raw = vec![7u8; 16];
        let buf = encode_bytes(
            4,
            4,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                level: 19,
                allow_zstd: true,
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.ihdr.compression_method, COMPRESSION_METHOD_ZSTD_BIT);
    }

    #[test]
    fn test_encode_forces_raw_flag_when_zstd_disallowed() {
        let width = 64u32;
        let height = 64u32;
        let raw = vec![7u8; (width * height) as usize]; // very compressible
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                level: 19,
                allow_zstd: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(buf[9 + 25 + 8], crate::zstd_codec::FLAG_RAW);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.ihdr.compression_method, 0);
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_add_tile_rejects_wrong_buffer_length() {
        let mut encoder = Encoder::new(
            Vec::new(),
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions::default(),
        )
        .unwrap();
        let result = encoder.add_tile(&[1, 2, 3]); // needs 4 bytes
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_add_tile_rejects_being_called_twice() {
        let mut encoder = Encoder::new(
            Vec::new(),
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions::default(),
        )
        .unwrap();
        encoder.add_tile(&[5]).unwrap();
        let result = encoder.add_tile(&[6]);
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_finish_rejects_being_called_without_add_tile() {
        let encoder = Encoder::new(
            Vec::new(),
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions::default(),
        )
        .unwrap();
        let result = encoder.finish();
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_new_rejects_invalid_ihdr_fields() {
        let result = Encoder::new(
            Vec::new(),
            0, // width = 0 is invalid
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions::default(),
        );
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidIhdr(_)))
        ));
    }

    #[test]
    fn test_new_rejects_unsupported_color_type() {
        let result = Encoder::new(
            Vec::new(),
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            3, // indexed - not supported in 0.1
            EncoderOptions::default(),
        );
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidIhdr(_)))
        ));
    }

    fn golden_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("golden")
    }

    /// Confirms this encoder produces byte-identical output to the
    /// hand-built golden fixtures from Phase 4 (`AGENTS.md`'s Phase 6 goal:
    /// "producing byte-identical golden files, closes the round-trip").
    /// Both goldens use `PREDICTOR_NONE` for every row and a raw (`Flag =
    /// 0x00`) `IDAT` — exactly what `choose_best_row_predictor` and the
    /// ZSTD fallback race independently arrive at for such tiny, flat
    /// inputs, so no special-casing is needed for this test to hold.
    #[test]
    fn test_encode_matches_golden_minimal_1x1_gray_byte_for_byte() {
        let expected = std::fs::read(golden_dir().join("minimal_1x1_gray.cafe")).unwrap();
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &[0x7F],
            EncoderOptions {
                level: 19,
                allow_zstd: false, // golden's IHDR declares compression_method = 0
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(buf, expected);
    }

    /// Unlike the 1x1 gray golden, this fixture's rows are *not*
    /// byte-identical to what a real encoder chooses: the per-row entropy
    /// heuristic (`choose_best_row_predictor`) finds that `Sub` beats the
    /// golden's hand-picked `None` for row 0 (each pixel is the previous
    /// pixel plus a constant [1,1,1,0] step, so `Sub`'s residual stream is
    /// far lower-entropy) — a genuine improvement, not a discrepancy to
    /// paper over. So this test only asserts the *pixel* round-trip
    /// matches the golden's decoded pixels, not byte-for-byte file
    /// equality; see `test_encode_matches_golden_minimal_1x1_gray_byte_for_byte`
    /// for a case simple enough (one byte, no left neighbor) that `None`
    /// really is optimal and byte-identity does hold.
    #[test]
    fn test_encode_matches_golden_minimal_2x2_rgba_pixels() {
        let golden_buf = std::fs::read(golden_dir().join("minimal_2x2_rgba.cafe")).unwrap();
        let golden_img = decode_bytes(&golden_buf).unwrap();

        let raw = vec![
            0x10, 0x20, 0x30, 0xFF, 0x11, 0x21, 0x31, 0xFF, // row 0
            0x12, 0x22, 0x32, 0xFF, 0x13, 0x23, 0x33, 0xFF, // row 1
        ];
        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            EncoderOptions {
                level: 19,
                allow_zstd: true,
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, golden_img.pixels);
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_roundtrip_bit_depth_16() {
        let width = 2u32;
        let height = 2u32;
        // 2 bytes/sample * 1 channel (gray) * 4 pixels = 8 bytes.
        let raw: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let buf = encode_bytes(
            width,
            height,
            16,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_multi_tile_row_major_roundtrip() {
        let width = 6u32;
        let height = 4u32;
        let raw: Vec<u8> = (0..(width * height)).map(|i| (i % 251) as u8).collect();
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                tile_size: Some((3, 2)),
                scan_order: cafe_format::constants::SCAN_ORDER_ROW_MAJOR,
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
        assert_eq!(img.ihdr.width, width);
        assert_eq!(img.ihdr.height, height);
    }

    #[test]
    fn test_encode_multi_tile_z_order_roundtrip() {
        let width = 8u32;
        let height = 8u32;
        let raw: Vec<u8> = (0..(width * height))
            .map(|i| ((i * 7 + 3) % 251) as u8)
            .collect();
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                tile_size: Some((4, 4)),
                scan_order: cafe_format::constants::SCAN_ORDER_Z_ORDER,
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_multi_tile_with_partial_edge_tiles_roundtrip() {
        // 5x5 image with 2x2 tiles: tiles_x=tiles_y=3 (ceil(5/2)), so the
        // rightmost/bottommost tiles are only 1 pixel wide/tall.
        let width = 5u32;
        let height = 5u32;
        let raw: Vec<u8> = (0..(width * height)).map(|i| i as u8).collect();
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                tile_size: Some((2, 2)),
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_multi_tile_rgba_roundtrip() {
        let width = 6u32;
        let height = 6u32;
        let raw: Vec<u8> = (0..(width * height * 4))
            .map(|i| ((i * 17 + 5) % 251) as u8)
            .collect();
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            EncoderOptions {
                tile_size: Some((3, 3)),
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_emits_idim_chunk_when_tile_size_configured() {
        let width = 4u32;
        let height = 4u32;
        let raw = vec![1u8; (width * height) as usize];
        let buf = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                tile_size: Some((2, 2)),
                ..Default::default()
            },
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) puts the iDIM chunk's Type field at
        // offset 9+25+4 = 38.
        assert_eq!(&buf[9 + 25 + 4..9 + 25 + 8], b"iDIM");
    }

    #[test]
    fn test_encode_omits_idim_chunk_when_tile_size_is_none() {
        let raw = vec![1u8; 4];
        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) puts the next chunk's Type field at
        // offset 9+25+4 = 38; with no iDIM, that's IDAT directly.
        assert_eq!(&buf[9 + 25 + 4..9 + 25 + 8], b"IDAT");
    }

    #[test]
    fn test_add_tile_rejects_being_called_more_times_than_layout_has_tiles() {
        let mut encoder = Encoder::new(
            Vec::new(),
            4,
            4,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions {
                tile_size: Some((2, 2)),
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..4 {
            encoder.add_tile(&[0u8; 4]).unwrap();
        }
        let result = encoder.add_tile(&[0u8; 4]);
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_finish_rejects_being_called_with_too_few_tiles_added() {
        let mut encoder = Encoder::new(
            Vec::new(),
            4,
            4,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions {
                tile_size: Some((2, 2)),
                ..Default::default()
            },
        )
        .unwrap();
        encoder.add_tile(&[0u8; 4]).unwrap();
        let result = encoder.finish();
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_new_rejects_zero_tile_size_component() {
        let result = Encoder::new(
            Vec::new(),
            4,
            4,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions {
                tile_size: Some((0, 2)),
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_encode_with_palette_roundtrip() {
        use crate::palette::{build_palette, expand_indices};
        use cafe_format::constants::COLOR_TYPE_RGB;

        // 2x2 RGB image, 2 distinct colors.
        let direct_pixels = vec![
            255u8, 0, 0, 0, 255, 0, // row 0: red, green
            255, 0, 0, 0, 255, 0, // row 1: red, green
        ];
        let built = build_palette(&direct_pixels, 3).unwrap();

        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGB,
            &built.indices,
            EncoderOptions {
                palette: Some(built.entries.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let img = decode_bytes(&buf).unwrap();
        // decode_bytes already expands indices back to direct pixels.
        assert_eq!(img.pixels, direct_pixels);

        // Sanity: expand_indices independently agrees.
        let expanded = expand_indices(&built.indices, &built.entries, 3).unwrap();
        assert_eq!(expanded, direct_pixels);
    }

    #[test]
    fn test_encode_with_palette_emits_plte_chunk() {
        use cafe_format::constants::COLOR_TYPE_RGB;

        let indices = vec![0u8, 1, 0, 1];
        let entries = vec![255u8, 0, 0, 0, 255, 0];
        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGB,
            &indices,
            EncoderOptions {
                palette: Some(entries),
                ..Default::default()
            },
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) puts the next chunk's Type field at
        // offset 9+25+4 = 38; with a palette configured, that's PLTE.
        assert_eq!(&buf[9 + 25 + 4..9 + 25 + 8], b"PLTE");
    }

    #[test]
    fn test_encode_without_palette_omits_plte_chunk() {
        use cafe_format::constants::COLOR_TYPE_RGB;

        let raw = vec![1u8; 4 * 3];
        let buf = encode_bytes(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGB,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        assert_eq!(&buf[9 + 25 + 4..9 + 25 + 8], b"IDAT");
    }

    #[test]
    fn test_encode_rejects_invalid_palette_color_type() {
        use cafe_format::constants::COLOR_TYPE_GRAY;

        let result = Encoder::new(
            Vec::new(),
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY, // PLTE is undefined for gray
            EncoderOptions {
                palette: Some(vec![255, 0, 0]),
                ..Default::default()
            },
        );
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidPlte(_)))
        ));
    }

    #[test]
    fn test_encode_rejects_palette_at_wrong_bit_depth() {
        use cafe_format::constants::COLOR_TYPE_RGB;

        let result = Encoder::new(
            Vec::new(),
            1,
            1,
            16, // PLTE requires bit_depth=8
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGB,
            EncoderOptions {
                palette: Some(vec![255, 0, 0]),
                ..Default::default()
            },
        );
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidPlte(_)))
        ));
    }

    #[test]
    fn test_encode_with_palette_and_tiling_roundtrip() {
        use crate::palette::build_palette;
        use cafe_format::constants::COLOR_TYPE_RGBA;

        // 4x4 RGBA image, few distinct colors, split into 2x2 tiles.
        let mut direct_pixels = Vec::new();
        for r in 0..4u32 {
            for c in 0..4u32 {
                let color = if (r + c) % 2 == 0 {
                    [255u8, 0, 0, 255]
                } else {
                    [0u8, 255, 0, 255]
                };
                direct_pixels.extend_from_slice(&color);
            }
        }
        let built = build_palette(&direct_pixels, 4).unwrap();

        let buf = encode_bytes(
            4,
            4,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &built.indices,
            EncoderOptions {
                palette: Some(built.entries),
                tile_size: Some((2, 2)),
                ..Default::default()
            },
        )
        .unwrap();

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, direct_pixels);
    }

    #[test]
    fn test_encode_embeds_and_roundtrips_all_metadata_chunk_types() {
        let raw = vec![0x7Fu8];
        let json_chunk = JsonChunk::new("com.example", "{\"a\":1}").unwrap();
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                exif: Some(b"fake exif bytes".to_vec()),
                json_chunks: vec![json_chunk.clone()],
                icc_profile: Some(b"fake icc profile bytes".to_vec()),
                xmp: Some("<x:xmpmeta></x:xmpmeta>".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.pixels, raw);
        assert_eq!(img.exif.as_deref(), Some(b"fake exif bytes".as_slice()));
        assert_eq!(img.json_chunks, vec![json_chunk]);
        assert_eq!(
            img.icc_profile.as_deref(),
            Some(b"fake icc profile bytes".as_slice())
        );
        assert_eq!(
            img.xmp,
            Some(cafe_format::Xmpd::new("<x:xmpmeta></x:xmpmeta>"))
        );
    }

    #[test]
    fn test_encode_without_metadata_options_omits_all_metadata_chunks() {
        let raw = vec![0x7Fu8];
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) puts the next chunk's Type field
        // at offset 9+25+4 = 38; with no metadata/tiling/palette
        // configured, that's IDAT directly.
        assert_eq!(&buf[9 + 25 + 4..9 + 25 + 8], b"IDAT");
    }

    #[test]
    fn test_encode_embeds_multiple_json_chunks_in_order() {
        let raw = vec![0x7Fu8];
        let json_a = JsonChunk::new("a.namespace", "{\"n\":1}").unwrap();
        let json_b = JsonChunk::new("b.namespace", "{\"n\":2}").unwrap();
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                json_chunks: vec![json_a.clone(), json_b.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.json_chunks, vec![json_a, json_b]);
    }

    #[test]
    fn test_encode_metadata_chunks_precede_plte_and_idat() {
        use cafe_format::constants::COLOR_TYPE_RGB;

        let indices = vec![0u8];
        let entries = vec![255u8, 0, 0];
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGB,
            &indices,
            EncoderOptions {
                exif: Some(b"exif".to_vec()),
                palette: Some(entries),
                ..Default::default()
            },
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) = 34; the eXIF chunk's Type field
        // is at offset 34+4 = 38, confirming eXIF precedes PLTE (spec
        // section 5's mandatory order).
        assert_eq!(&buf[38..42], b"eXIF");
    }

    #[test]
    fn test_encode_forces_raw_flag_for_metadata_when_zstd_disallowed() {
        // A large, highly compressible EXIF blob: if allow_zstd were
        // honored, this would normally pick FLAG_ZSTD, so forcing raw
        // here directly exercises the allow_zstd=false branch of
        // write_metadata_chunk.
        let raw = vec![0x7Fu8];
        let exif_blob = vec![7u8; 4096];
        let buf = encode_bytes(
            1,
            1,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions {
                allow_zstd: false,
                exif: Some(exif_blob.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        // Signature(9) + IHDR chunk(25) = 34; eXIF's Flag byte is at
        // offset 34 + Length(4) + Type(4) = 42.
        assert_eq!(buf[42], crate::zstd_codec::FLAG_RAW);
        let img = decode_bytes(&buf).unwrap();
        assert_eq!(img.exif.as_deref(), Some(exif_blob.as_slice()));
    }

    // --- encode_bytes_parallel parity tests -------------------------------
    //
    // `encode_bytes_parallel` shares `Encoder::new`/`encode_tile_chunk`
    // with `encode_bytes`, so these tests focus on confirming the two
    // produce byte-for-byte identical output across a range of tile
    // counts, not on re-testing header/tiling validation already covered
    // above.

    #[test]
    fn test_encode_bytes_parallel_matches_sequential_single_tile() {
        let width = 4u32;
        let height = 4u32;
        let raw: Vec<u8> = (0..(width * height))
            .map(|i| ((i * 37 + 11) % 251) as u8)
            .collect();
        let seq = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        let par = encode_bytes_parallel(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            EncoderOptions::default(),
        )
        .unwrap();
        assert_eq!(seq, par);
    }

    #[test]
    fn test_encode_bytes_parallel_matches_sequential_below_threshold_tile_count() {
        let width = 6u32;
        let height = 4u32;
        let raw: Vec<u8> = (0..(width * height)).map(|i| (i % 251) as u8).collect();
        let options = EncoderOptions {
            tile_size: Some((3, 2)),
            ..Default::default()
        };
        let seq = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options.clone(),
        )
        .unwrap();
        let par = encode_bytes_parallel(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options,
        )
        .unwrap();
        assert_eq!(seq, par);
    }

    #[test]
    fn test_encode_bytes_parallel_matches_sequential_many_tiles() {
        let width = 32u32;
        let height = 32u32;
        let raw: Vec<u8> = (0..(width * height * 4))
            .map(|i| ((i * 17 + 5) % 251) as u8)
            .collect();
        let options = EncoderOptions {
            tile_size: Some((4, 4)),
            ..Default::default()
        };
        let seq = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            options.clone(),
        )
        .unwrap();
        let par = encode_bytes_parallel(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_RGBA,
            &raw,
            options,
        )
        .unwrap();
        assert_eq!(seq, par);
        let img = decode_bytes(&par).unwrap();
        assert_eq!(img.pixels, raw);
    }

    #[test]
    fn test_encode_bytes_parallel_matches_sequential_with_partial_edge_tiles() {
        let width = 5u32;
        let height = 5u32;
        let raw: Vec<u8> = (0..(width * height)).map(|i| i as u8).collect();
        let options = EncoderOptions {
            tile_size: Some((2, 2)),
            ..Default::default()
        };
        let seq = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options.clone(),
        )
        .unwrap();
        let par = encode_bytes_parallel(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options,
        )
        .unwrap();
        assert_eq!(seq, par);
    }

    #[test]
    fn test_encode_bytes_parallel_matches_sequential_z_order() {
        let width = 8u32;
        let height = 8u32;
        let raw: Vec<u8> = (0..(width * height))
            .map(|i| ((i * 7 + 3) % 251) as u8)
            .collect();
        let options = EncoderOptions {
            tile_size: Some((4, 4)),
            scan_order: cafe_format::constants::SCAN_ORDER_Z_ORDER,
            ..Default::default()
        };
        let seq = encode_bytes(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options.clone(),
        )
        .unwrap();
        let par = encode_bytes_parallel(
            width,
            height,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &raw,
            options,
        )
        .unwrap();
        assert_eq!(seq, par);
    }

    #[test]
    fn test_encode_bytes_parallel_rejects_wrong_buffer_length() {
        let result = encode_bytes_parallel(
            2,
            2,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            &[1, 2, 3], // needs 4 bytes
            EncoderOptions::default(),
        );
        assert!(matches!(result, Err(CodecError::EncoderMisuse(_))));
    }

    #[test]
    fn test_new_rejects_excessive_tile_count() {
        // tile_width=tile_height=1 on a 65535x65535 image: tiles_x=tiles_y=65535,
        // product exceeds MAX_TILE_COUNT (mirrors the exploit documented in
        // cafe_format::Idim::validate).
        let result = Encoder::new(
            Vec::new(),
            65535,
            65535,
            8,
            SAMPLE_FORMAT_UINT,
            COLOR_TYPE_GRAY,
            EncoderOptions {
                tile_size: Some((1, 1)),
                ..Default::default()
            },
        );
        assert!(matches!(
            result,
            Err(CodecError::Format(cafe_format::CafeError::InvalidIdim(_)))
        ));
    }
}
